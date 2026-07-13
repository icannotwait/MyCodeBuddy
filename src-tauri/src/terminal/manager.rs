use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize};

use super::error::TerminalError;
use super::shell::{configure_interactive_command, ResolvedShellSpec};
use super::types::{TerminalEvent, TerminalInfo};
use crate::web::event_bridge::EventEmitter;

struct TerminalInstance {
    write_tx: mpsc::Sender<Vec<u8>>,
    master: Box<dyn MasterPty + Send>,
    _child: Box<dyn portable_pty::Child + Send>,
    title: String,
    owner_window_label: String,
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
        configure_interactive_command(
            &opts.shell,
            &mut cmd,
            opts.initial_command.as_deref(),
        );
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

        let instance = TerminalInstance {
            write_tx,
            master: pair.master,
            _child: child,
            title: "Terminal".to_string(),
            owner_window_label: opts.owner_window_label,
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
        std::thread::Builder::new()
            .name(format!("pty-reader-{short_id}"))
            .spawn(move || {
                read_loop(reader, id_for_reader, &emitter, &terminals_ref);
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

    pub fn list_with_exit_check(&self, emitter: Option<&EventEmitter>) -> Vec<TerminalInfo> {
        let mut terminals = self.terminals.lock().unwrap();
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
                        id, err
                    );
                    exited_terminal_ids.push(id.clone());
                }
            }
        }

        for terminal_id in &exited_terminal_ids {
            terminals.remove(terminal_id);
        }

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

    pub fn kill_by_owner_window(&self, owner_window_label: &str) -> usize {
        let mut instances = {
            let mut terminals = self.terminals.lock().unwrap();
            let ids: Vec<String> = terminals
                .iter()
                .filter_map(|(id, instance)| {
                    if instance.owner_window_label == owner_window_label {
                        Some(id.clone())
                    } else {
                        None
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
) {
    let output_event = format!("terminal://output/{}", terminal_id);
    let mut buf = [0u8; 8192];

    loop {
        match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                let data = String::from_utf8_lossy(&buf[..n]).to_string();
                let event = TerminalEvent {
                    terminal_id: terminal_id.clone(),
                    data,
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
}
