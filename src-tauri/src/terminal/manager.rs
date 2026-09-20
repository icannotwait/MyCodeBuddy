use std::collections::{HashMap, VecDeque};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize};

use super::error::TerminalError;
use super::shell::{configure_interactive_command, ResolvedShellSpec};
use super::types::{TerminalEvent, TerminalInfo, TerminalSnapshot};
use crate::browser::service_url::ServiceScanner;
use crate::browser::services::ServiceWatch;
use crate::browser::types::ServiceSource;
use crate::web::event_bridge::EventEmitter;

/// How much recent PTY output a terminal keeps for re-attaching viewers. Sized
/// to cover a screenful of `ls -R` or a compile log, not a whole session: this
/// is a "pick the pane back up where you left it" buffer, not a transcript.
const SCROLLBACK_MAX_CHARS: usize = 128 * 1024;

/// Recent output of one terminal, kept so a viewer that mounts after the spawn
/// (or re-mounts after its host view was unmounted — a canvas terminal card
/// crossing a route switch) can redraw instead of showing a blank pane while
/// the shell sits there waiting at a prompt it already printed.
///
/// Whole chunks are evicted from the front rather than characters: the buffer
/// holds raw terminal bytes, and slicing one at an arbitrary offset would cut
/// an escape sequence in half and paint the replay with whatever the truncated
/// tail happens to mean.
#[derive(Default)]
struct Scrollback {
    chunks: VecDeque<String>,
    chars: usize,
    /// Chunks appended since the terminal spawned — the monotonic cursor the
    /// output events carry. Never reset, so it stays comparable across a
    /// re-attach.
    seq: u64,
}

impl Scrollback {
    /// Record a chunk and return the seq it was assigned (= the seq an event
    /// carrying this chunk must report).
    fn append(&mut self, data: &str) -> u64 {
        self.seq += 1;
        self.chars += data.chars().count();
        self.chunks.push_back(data.to_string());
        while self.chars > SCROLLBACK_MAX_CHARS && self.chunks.len() > 1 {
            if let Some(old) = self.chunks.pop_front() {
                self.chars = self.chars.saturating_sub(old.chars().count());
            }
        }
        self.seq
    }

    fn read(&self) -> (String, u64) {
        (self.chunks.iter().cloned().collect(), self.seq)
    }
}

struct TerminalInstance {
    write_tx: mpsc::Sender<Vec<u8>>,
    master: Box<dyn MasterPty + Send>,
    _child: Box<dyn portable_pty::Child + Send>,
    title: String,
    owner_window_label: String,
    owner_operation_id: Option<String>,
    /// Shared with this terminal's reader thread — the thread appends, viewers
    /// read. Held behind its own lock rather than the map's so a chunk of
    /// output never waits on a write / resize / list call.
    scrollback: Arc<Mutex<Scrollback>>,
    /// Temp files (credential store + helper script) to clean up on exit.
    temp_files: Vec<std::path::PathBuf>,
}

pub struct TerminalManager {
    terminals: Arc<Mutex<HashMap<String, TerminalInstance>>>,
}

/// Options for spawning a new terminal session.
pub struct SpawnOptions {
    pub terminal_id: String,
    pub working_dir: String,
    pub owner_window_label: String,
    pub owner_operation_id: Option<String>,
    pub shell: ResolvedShellSpec,
    pub initial_command: Option<String>,
    pub extra_env: Option<HashMap<String, String>>,
    pub temp_files: Vec<PathBuf>,
}

impl TerminalManager {
    pub fn new() -> Self {
        Self {
            terminals: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Returns a shallow clone sharing the same underlying terminal map.
    pub fn clone_ref(&self) -> Self {
        Self {
            terminals: self.terminals.clone(),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn spawn_with_id(
        &self,
        opts: SpawnOptions,
        emitter: EventEmitter,
    ) -> Result<String, TerminalError> {
        // Reject duplicate IDs to prevent orphaning an existing PTY process.
        {
            let terminals = self.terminals.lock().unwrap();
            if terminals.contains_key(&opts.terminal_id) {
                return Err(TerminalError::SpawnFailed(format!(
                    "terminal id '{}' already exists",
                    opts.terminal_id
                )));
            }
        }

        let pty_system = native_pty_system();

        let pair = pty_system
            .openpty(PtySize {
                rows: 24,
                cols: 80,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| TerminalError::SpawnFailed(e.to_string()))?;

        let mut cmd = CommandBuilder::new(&opts.shell.executable);
        configure_interactive_command(&opts.shell, &mut cmd, opts.initial_command.as_deref());
        cmd.cwd(&opts.working_dir);

        // Inject extra environment variables (e.g. git credential helper config)
        if let Some(env) = &opts.extra_env {
            for (key, value) in env {
                cmd.env(key, value);
            }
        }

        let child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|e| TerminalError::SpawnFailed(e.to_string()))?;

        drop(pair.slave);

        let writer = pair
            .master
            .take_writer()
            .map_err(|e| TerminalError::SpawnFailed(e.to_string()))?;

        let reader = pair
            .master
            .try_clone_reader()
            .map_err(|e| TerminalError::SpawnFailed(e.to_string()))?;

        let terminal_id = opts.terminal_id;
        // Boundary-, length-, and NUL-safe prefix for the PTY thread names; see
        // `thread_name_prefix`. `terminal_id` is caller-supplied.
        let short_id = thread_name_prefix(&terminal_id);

        let (write_tx, write_rx) = mpsc::channel::<Vec<u8>>();
        let scrollback = Arc::new(Mutex::new(Scrollback::default()));

        let owner_window = opts.owner_window_label.clone();
        let instance = TerminalInstance {
            write_tx,
            master: pair.master,
            _child: child,
            title: "Terminal".to_string(),
            owner_window_label: opts.owner_window_label,
            owner_operation_id: opts.owner_operation_id,
            scrollback: scrollback.clone(),
            temp_files: opts.temp_files,
        };

        self.terminals
            .lock()
            .unwrap()
            .insert(terminal_id.clone(), instance);

        // Named writer thread
        std::thread::Builder::new()
            .name(format!("pty-writer-{short_id}"))
            .spawn(move || {
                write_loop(writer, write_rx);
            })
            .map_err(|e| TerminalError::SpawnFailed(e.to_string()))?;

        // Named reader thread — emits per-terminal events
        let id_for_reader = terminal_id.clone();
        let terminals_ref = self.terminals.clone();
        // The watch that notices a dev server announcing itself in this
        // terminal's output. Built here because this is where the owning
        // window is known; it probes and emits on threads of its own, so the
        // reader below never waits on a socket.
        let watch = ServiceWatch::new(emitter.clone(), owner_window, ServiceSource::Terminal);
        std::thread::Builder::new()
            .name(format!("pty-reader-{short_id}"))
            .spawn(move || {
                read_loop(
                    reader,
                    id_for_reader,
                    &emitter,
                    &terminals_ref,
                    &scrollback,
                    &watch,
                );
            })
            .map_err(|e| TerminalError::SpawnFailed(e.to_string()))?;

        Ok(terminal_id)
    }

    pub fn write(&self, terminal_id: &str, data: &[u8]) -> Result<(), TerminalError> {
        let terminals = self.terminals.lock().unwrap();
        let instance = terminals
            .get(terminal_id)
            .ok_or_else(|| TerminalError::NotFound(terminal_id.to_string()))?;
        instance
            .write_tx
            .send(data.to_vec())
            .map_err(|e| TerminalError::WriteFailed(e.to_string()))?;
        Ok(())
    }

    pub fn resize(&self, terminal_id: &str, cols: u16, rows: u16) -> Result<(), TerminalError> {
        let terminals = self.terminals.lock().unwrap();
        let instance = terminals
            .get(terminal_id)
            .ok_or_else(|| TerminalError::NotFound(terminal_id.to_string()))?;
        instance
            .master
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| TerminalError::ResizeFailed(e.to_string()))?;
        Ok(())
    }

    /// Recent output of a live terminal, for a viewer attaching to a PTY that
    /// was spawned before it mounted.
    ///
    /// Never an error: "no such terminal" is the answer that tells the caller
    /// to spawn one instead, and a caller that has to distinguish that from a
    /// transport failure would have to parse the error string to do it. Note
    /// the map lock is taken only to find the instance — the buffer has its own,
    /// so a large scrollback is never copied while output is blocked.
    pub fn snapshot(&self, terminal_id: &str) -> TerminalSnapshot {
        let buffer = {
            let terminals = self.terminals.lock().unwrap();
            match terminals.get(terminal_id) {
                Some(instance) => instance.scrollback.clone(),
                None => {
                    return TerminalSnapshot {
                        alive: false,
                        data: String::new(),
                        seq: 0,
                    }
                }
            }
        };
        let (data, seq) = buffer
            .lock()
            .map(|s| s.read())
            .unwrap_or_else(|_| (String::new(), 0));
        TerminalSnapshot {
            alive: true,
            data,
            seq,
        }
    }

    pub fn kill(&self, terminal_id: &str) -> Result<(), TerminalError> {
        let mut instance = self
            .terminals
            .lock()
            .unwrap()
            .remove(terminal_id)
            .ok_or_else(|| TerminalError::NotFound(terminal_id.to_string()))?;
        terminate_terminal(&mut instance);
        Ok(())
    }

    /// THE liveness gate. Drops terminals whose child has exited and returns
    /// their ids so the caller can announce them.
    ///
    /// Every "is this terminal still running" question routes through here.
    /// A second copy of the `try_wait` logic is how a close confirmation ends
    /// up claiming three terminals will die while the kill that follows
    /// reports two.
    ///
    /// Reaped instances get their temp files removed. Dropping a
    /// `TerminalInstance` releases the PTY but not the credential store and
    /// helper script on disk — only [`terminate_terminal`] did that, and it is
    /// not on this path.
    fn reap_exited(terminals: &mut HashMap<String, TerminalInstance>) -> Vec<String> {
        let mut exited_terminal_ids: Vec<String> = Vec::new();

        // Windows ConPTY may not always surface EOF promptly; reconcile exited
        // child processes here so frontend running-state can recover reliably.
        for (id, instance) in terminals.iter_mut() {
            match instance._child.try_wait() {
                Ok(Some(_)) => exited_terminal_ids.push(id.clone()),
                Ok(None) => {}
                Err(err) => {
                    tracing::error!(
                        "[TERM] failed to query child status for terminal {}: {}",
                        id,
                        err
                    );
                    exited_terminal_ids.push(id.clone());
                }
            }
        }

        for terminal_id in &exited_terminal_ids {
            if let Some(mut instance) = terminals.remove(terminal_id) {
                cleanup_temp_files(&mut instance.temp_files);
            }
        }

        exited_terminal_ids
    }

    pub fn list_with_exit_check(&self, emitter: Option<&EventEmitter>) -> Vec<TerminalInfo> {
        let mut terminals = self.terminals.lock().unwrap();
        let exited_terminal_ids = Self::reap_exited(&mut terminals);

        let infos = terminals
            .iter()
            .map(|(id, inst)| TerminalInfo {
                id: id.clone(),
                title: inst.title.clone(),
            })
            .collect();

        drop(terminals);

        if let Some(emitter) = emitter {
            for terminal_id in exited_terminal_ids {
                emit_terminal_exit_event(emitter, &terminal_id);
            }
        }

        infos
    }

    /// How many of `owner_window_label`'s terminals are still running.
    ///
    /// Shares [`Self::reap_exited`] with `list_with_exit_check` so a finished
    /// build is never counted as work in progress — the close confirmation
    /// this feeds is ignored the moment it cries wolf.
    pub fn count_live_by_owner_window(
        &self,
        owner_window_label: &str,
        emitter: Option<&EventEmitter>,
    ) -> usize {
        let mut terminals = self.terminals.lock().unwrap();
        let exited_terminal_ids = Self::reap_exited(&mut terminals);

        let live = terminals
            .values()
            .filter(|instance| instance.owner_window_label == owner_window_label)
            .count();

        drop(terminals);

        if let Some(emitter) = emitter {
            for terminal_id in exited_terminal_ids {
                emit_terminal_exit_event(emitter, &terminal_id);
            }
        }

        live
    }

    pub fn kill_by_owner_window(&self, owner_window_label: &str) -> usize {
        self.kill_by_owner_window_and_operation(owner_window_label, None)
    }

    /// Rebind every terminal matching `(from_label, operation_id)` to
    /// `to_label` without killing the PTY process. Used by pop-out close
    /// residual so child-window terminals survive reverse to `main`.
    pub fn rebind_owner_window_by_operation(
        &self,
        from_label: &str,
        operation_id: &str,
        to_label: &str,
    ) -> usize {
        let mut terminals = self.terminals.lock().unwrap();
        let mut n = 0usize;
        for instance in terminals.values_mut() {
            if instance.owner_window_label != from_label {
                continue;
            }
            if instance.owner_operation_id.as_deref() != Some(operation_id) {
                continue;
            }
            instance.owner_window_label = to_label.to_string();
            n += 1;
        }
        n
    }

    /// When `operation_id` is `Some`, only kill terminals stamped with that
    /// incarnation. When `None`, match label only (legacy / main window).
    pub fn kill_by_owner_window_and_operation(
        &self,
        owner_window_label: &str,
        operation_id: Option<&str>,
    ) -> usize {
        let mut instances = {
            let mut terminals = self.terminals.lock().unwrap();
            let ids: Vec<String> = terminals
                .iter()
                .filter_map(|(id, instance)| {
                    if instance.owner_window_label != owner_window_label {
                        return None;
                    }
                    match operation_id {
                        None => Some(id.clone()),
                        Some(op) => {
                            if instance.owner_operation_id.as_deref() == Some(op) {
                                Some(id.clone())
                            } else {
                                None
                            }
                        }
                    }
                })
                .collect();

            let mut removed = Vec::with_capacity(ids.len());
            for id in ids {
                if let Some(instance) = terminals.remove(&id) {
                    removed.push(instance);
                }
            }
            removed
        };

        let killed = instances.len();
        for instance in &mut instances {
            terminate_terminal(instance);
        }
        killed
    }

    pub fn kill_all(&self) -> usize {
        let mut instances: Vec<TerminalInstance> = {
            let mut terminals = self.terminals.lock().unwrap();
            terminals.drain().map(|(_, inst)| inst).collect()
        };
        let killed = instances.len();
        for instance in &mut instances {
            terminate_terminal(instance);
        }
        tracing::info!("[TERM] kill_all killed_terminals={}", killed);
        killed
    }

    /// Inject a stub terminal for unit tests (no real PTY process).
    #[cfg(test)]
    pub fn insert_test_terminal(
        &self,
        terminal_id: &str,
        owner_window_label: &str,
        owner_operation_id: Option<&str>,
    ) {
        use portable_pty::{Child, ChildKiller, ExitStatus, MasterPty, PtySize};
        use std::io::{Read, Result as IoResult, Write};

        struct StubMasterPty;
        impl MasterPty for StubMasterPty {
            fn resize(&self, _size: PtySize) -> Result<(), anyhow::Error> {
                Ok(())
            }
            fn get_size(&self) -> Result<PtySize, anyhow::Error> {
                Ok(PtySize::default())
            }
            fn try_clone_reader(&self) -> Result<Box<dyn Read + Send>, anyhow::Error> {
                Ok(Box::new(std::io::empty()))
            }
            fn take_writer(&self) -> Result<Box<dyn Write + Send>, anyhow::Error> {
                Ok(Box::new(std::io::sink()))
            }
            // portable-pty requires these on Unix; stubs have no real PTY.
            #[cfg(unix)]
            fn process_group_leader(&self) -> Option<libc::pid_t> {
                None
            }
            #[cfg(unix)]
            fn as_raw_fd(&self) -> Option<std::os::unix::io::RawFd> {
                None
            }
        }

        #[derive(Debug)]
        struct StubChild;
        #[derive(Debug)]
        struct StubChildKiller;
        impl ChildKiller for StubChildKiller {
            fn kill(&mut self) -> IoResult<()> {
                Ok(())
            }
            fn clone_killer(&self) -> Box<dyn ChildKiller + Send + Sync> {
                Box::new(StubChildKiller)
            }
        }
        impl ChildKiller for StubChild {
            fn kill(&mut self) -> IoResult<()> {
                Ok(())
            }
            fn clone_killer(&self) -> Box<dyn ChildKiller + Send + Sync> {
                Box::new(StubChildKiller)
            }
        }
        impl Child for StubChild {
            fn try_wait(&mut self) -> IoResult<Option<ExitStatus>> {
                Ok(None)
            }
            fn wait(&mut self) -> IoResult<ExitStatus> {
                Ok(ExitStatus::with_exit_code(0))
            }
            fn process_id(&self) -> Option<u32> {
                None
            }
            #[cfg(windows)]
            fn as_raw_handle(&self) -> Option<std::os::windows::io::RawHandle> {
                None
            }
        }

        let (write_tx, _write_rx) = mpsc::channel();
        let instance = TerminalInstance {
            write_tx,
            master: Box::new(StubMasterPty),
            _child: Box::new(StubChild),
            title: "Test".to_string(),
            owner_window_label: owner_window_label.to_string(),
            owner_operation_id: owner_operation_id.map(str::to_string),
            scrollback: Arc::new(Mutex::new(Scrollback::default())),
            temp_files: Vec::new(),
        };
        self.terminals
            .lock()
            .unwrap()
            .insert(terminal_id.to_string(), instance);
    }

    #[cfg(test)]
    pub fn owner_window_label_for_test(&self, terminal_id: &str) -> Option<String> {
        self.terminals
            .lock()
            .unwrap()
            .get(terminal_id)
            .map(|i| i.owner_window_label.clone())
    }

    #[cfg(test)]
    pub fn contains_for_test(&self, terminal_id: &str) -> bool {
        self.terminals.lock().unwrap().contains_key(terminal_id)
    }
}

fn terminate_terminal(instance: &mut TerminalInstance) {
    let _ = instance._child.kill();
    let _ = instance._child.wait();
    cleanup_temp_files(&mut instance.temp_files);
}

fn cleanup_temp_files(files: &mut Vec<std::path::PathBuf>) {
    for path in files.drain(..) {
        let _ = std::fs::remove_file(&path);
    }
}

fn write_loop(mut writer: Box<dyn Write + Send>, rx: mpsc::Receiver<Vec<u8>>) {
    while let Ok(data) = rx.recv() {
        if writer.write_all(&data).is_err() {
            break;
        }
        while let Ok(more) = rx.try_recv() {
            if writer.write_all(&more).is_err() {
                return;
            }
        }
        if writer.flush().is_err() {
            break;
        }
    }
}

fn read_loop(
    mut reader: Box<dyn Read + Send>,
    terminal_id: String,
    emitter: &EventEmitter,
    terminals: &Arc<Mutex<HashMap<String, TerminalInstance>>>,
    scrollback: &Arc<Mutex<Scrollback>>,
    watch: &ServiceWatch,
) {
    let output_event = format!("terminal://output/{}", terminal_id);
    let mut buf = [0u8; 8192];
    // Thread-confined, so the carry buffer and the rate limit need no lock.
    let mut services = ServiceScanner::new();

    loop {
        match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                let data = String::from_utf8_lossy(&buf[..n]).to_string();
                // Record BEFORE emitting, and take the seq from the same
                // critical section: a snapshot read concurrent with this chunk
                // either sees it (and reports a seq at least this high) or does
                // not (and reports one below it). Either way the receiving
                // client can tell overlap from new output — see `TerminalEvent`.
                // A poisoned lock is not fatal here: the buffer is an
                // optimisation, so fall back to an un-deduplicable seq of 0
                // rather than killing the reader thread and the terminal with it.
                let seq = scrollback
                    .lock()
                    .map(|mut s| s.append(&data))
                    .unwrap_or_default();
                // Before the emit, not after: this is the only place a
                // terminal's output passes through in one piece, and a viewer
                // that is not mounted (the mobile drawer, a canvas card on
                // another route) never sees it at all.
                watch.feed(&mut services, &data, &terminal_id);
                let event = TerminalEvent {
                    terminal_id: terminal_id.clone(),
                    data,
                    seq,
                };
                crate::web::event_bridge::emit_event(emitter, &output_event, event.clone());
            }
            Err(_) => break,
        }
    }

    // Terminal exited — remove from map and clean up temp files
    if let Some(mut instance) = terminals.lock().unwrap().remove(&terminal_id) {
        cleanup_temp_files(&mut instance.temp_files);
    }

    emit_terminal_exit_event(emitter, &terminal_id);
}

fn emit_terminal_exit_event(emitter: &EventEmitter, terminal_id: &str) {
    let exit_event = format!("terminal://exit/{}", terminal_id);
    let event = TerminalEvent {
        terminal_id: terminal_id.to_string(),
        data: String::new(),
        seq: 0,
    };
    crate::web::event_bridge::emit_event(emitter, &exit_event, event.clone());
}

/// Build a thread-name-safe short prefix from a caller-supplied `terminal_id`.
///
/// `terminal_id` arrives from the frontend (Tauri/web spawn paths) and is not
/// guaranteed to be ASCII, at least 8 bytes long, or free of NUL bytes. Naive
/// `&terminal_id[..8]` panics on a short id or a multibyte char straddling
/// byte 8, and `std::thread::Builder::spawn` panics if the resulting thread
/// name contains an interior NUL. Take the first 8 Unicode scalar values
/// (boundary- and length-safe) and replace NUL with `_`.
fn thread_name_prefix(terminal_id: &str) -> String {
    terminal_id
        .chars()
        .take(8)
        .map(|c| if c == '\0' { '_' } else { c })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::thread_name_prefix;
    use super::TerminalManager;
    #[cfg(not(target_os = "windows"))]
    use super::{Arc, EventEmitter, SpawnOptions};
    use super::{Scrollback, SCROLLBACK_MAX_CHARS};

    #[test]
    fn scrollback_seq_counts_every_chunk_and_never_rewinds() {
        // The seq is what a re-attaching client uses to tell "already in the
        // snapshot" from "arrived after it". A repeated or reset value would
        // make the replay either double-print or swallow output.
        let mut buffer = Scrollback::default();
        assert_eq!(buffer.append("a"), 1);
        assert_eq!(buffer.append("b"), 2);
        assert_eq!(buffer.read(), ("ab".to_string(), 2));
    }

    #[test]
    fn scrollback_evicts_whole_chunks_and_keeps_counting() {
        // Whole chunks, never a character slice: the buffer holds raw terminal
        // bytes, and cutting one mid-escape paints the replay with whatever the
        // truncated tail happens to mean.
        let mut buffer = Scrollback::default();
        let chunk = "x".repeat(SCROLLBACK_MAX_CHARS / 2 + 1);
        buffer.append(&chunk);
        buffer.append(&chunk);
        let seq = buffer.append("tail");
        let (data, read_seq) = buffer.read();
        assert_eq!(read_seq, seq, "eviction must not rewind the cursor");
        assert!(data.ends_with("tail"));
        assert!(
            data.chars().count() <= SCROLLBACK_MAX_CHARS,
            "kept {} chars",
            data.chars().count()
        );
    }

    #[test]
    fn scrollback_keeps_the_last_chunk_even_when_it_alone_is_too_big() {
        // A single chunk over the cap must not evict itself into an empty
        // buffer — the newest output is the part worth keeping.
        let mut buffer = Scrollback::default();
        let huge = "y".repeat(SCROLLBACK_MAX_CHARS * 2);
        buffer.append(&huge);
        let (data, seq) = buffer.read();
        assert_eq!(seq, 1);
        assert_eq!(data.chars().count(), huge.chars().count());
    }

    #[test]
    fn a_missing_terminal_reports_not_alive_rather_than_failing() {
        // "No such terminal" is the answer that tells a caller to spawn one;
        // an error would force it to parse a string to tell that apart from a
        // transport failure.
        let manager = TerminalManager::new();
        let snapshot = manager.snapshot("nope");
        assert!(!snapshot.alive);
        assert!(snapshot.data.is_empty());
        assert_eq!(snapshot.seq, 0);
    }

    #[test]
    fn keeps_short_ascii_id() {
        assert_eq!(thread_name_prefix("abc"), "abc");
        assert_eq!(thread_name_prefix(""), "");
    }

    #[test]
    fn truncates_to_first_eight_chars() {
        assert_eq!(thread_name_prefix("0123456789"), "01234567");
    }

    #[test]
    fn is_char_boundary_safe() {
        // '密' occupies bytes 7..10, so `&s[..8]` would slice inside it and
        // panic; taking 8 scalar values keeps the whole char.
        assert_eq!(thread_name_prefix("abcdefg密钥"), "abcdefg密");
    }

    #[test]
    fn sanitizes_interior_nul_so_thread_spawns() {
        assert_eq!(thread_name_prefix("ab\0cd"), "ab_cd");
        // The result must be usable as a real thread name without panicking.
        std::thread::Builder::new()
            .name(thread_name_prefix("ab\0cdefghij"))
            .spawn(|| {})
            .expect("spawn with sanitized name")
            .join()
            .expect("join");
    }

    #[test]
    fn rebind_owner_window_by_operation_moves_matching_terminals() {
        let tm = TerminalManager::new();
        // Matching (label, op) — must rebind to main.
        tm.insert_test_terminal("t-match", "conversation-1", Some("op-1"));
        // Wrong op — left alone.
        tm.insert_test_terminal("t-other-op", "conversation-1", Some("op-2"));
        // Wrong label — left alone.
        tm.insert_test_terminal("t-other-label", "conversation-9", Some("op-1"));
        // No op stamp — left alone.
        tm.insert_test_terminal("t-no-op", "conversation-1", None);

        let n = tm.rebind_owner_window_by_operation("conversation-1", "op-1", "main");
        assert_eq!(n, 1, "exactly one matching terminal should rebind");

        assert_eq!(
            tm.owner_window_label_for_test("t-match").as_deref(),
            Some("main")
        );
        assert_eq!(
            tm.owner_window_label_for_test("t-other-op").as_deref(),
            Some("conversation-1")
        );
        assert_eq!(
            tm.owner_window_label_for_test("t-other-label").as_deref(),
            Some("conversation-9")
        );
        assert_eq!(
            tm.owner_window_label_for_test("t-no-op").as_deref(),
            Some("conversation-1")
        );
        // All terminals still alive (rebind never kills).
        assert!(tm.contains_for_test("t-match"));
        assert!(tm.contains_for_test("t-other-op"));
        assert!(tm.contains_for_test("t-other-label"));
        assert!(tm.contains_for_test("t-no-op"));
    }

    /// The whole local-server path over a REAL pty: a real shell prints a real
    /// banner, the watch reads it out of the output stream, connects to the
    /// socket, and the frontend's event arrives naming the window that owns
    /// the terminal.
    ///
    /// The pieces have unit tests of their own; what only an end-to-end run
    /// can show is that the watch is wired into the reader at all, and that a
    /// banner survives a real PTY (its line endings, its echo, its shell).
    #[cfg(not(target_os = "windows"))]
    #[tokio::test]
    async fn a_server_announced_in_a_terminal_reaches_the_frontend() {
        use crate::browser::types::SERVICE_DETECTED_EVENT;
        use crate::web::event_bridge::WebEventBroadcaster;
        use std::time::Duration;

        // Something really listening, so the probe has something to find.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().unwrap().port();

        let broadcaster = Arc::new(WebEventBroadcaster::new());
        // Subscribed before the spawn: the broadcaster drops what it sends
        // with no receivers, and a shell prints fast.
        let mut events = broadcaster.subscribe();
        let emitter = EventEmitter::test_web_only(broadcaster);

        let manager = TerminalManager::new();
        manager
            .spawn_with_id(
                SpawnOptions {
                    terminal_id: "svc-e2e".to_string(),
                    working_dir: std::env::temp_dir().to_string_lossy().to_string(),
                    owner_window_label: "main".to_string(),
                    owner_operation_id: None,
                    shell: super::ResolvedShellSpec {
                        executable: std::path::PathBuf::from("/bin/sh"),
                        dialect: crate::terminal::shell::ShellDialect::Posix,
                        display_name: "sh".into(),
                        source: crate::terminal::shell::ShellSource::System,
                        command_strategy: crate::terminal::shell::ShellCommandStrategy::Posix,
                    },
                    initial_command: Some(format!(
                        "printf '  ➜  Local:   http://127.0.0.1:{port}/\\n'"
                    )),
                    extra_env: None,
                    temp_files: vec![],
                },
                emitter,
            )
            .expect("spawn");

        let detected = tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                let event = events.recv().await.expect("event bus");
                if event.channel == SERVICE_DETECTED_EVENT {
                    return event;
                }
            }
        })
        .await
        .expect("the service event");

        let payload = detected.payload;
        assert_eq!(payload["origin"], format!("http://127.0.0.1:{port}"));
        assert_eq!(payload["url"], format!("http://127.0.0.1:{port}/"));
        assert_eq!(payload["authority"], format!("127.0.0.1:{port}"));
        assert_eq!(payload["ownerWindow"], "main");
        assert_eq!(payload["source"], "terminal");
        assert_eq!(payload["terminalId"], "svc-e2e");

        let _ = manager.kill("svc-e2e");
    }
}
