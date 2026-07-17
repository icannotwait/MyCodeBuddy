//! Pull-driven git commit metadata cursor for reference search.
//!
//! Streams only lightweight `git log` metadata (no diffs), pinned to a captured
//! full HEAD so pages stay consistent if the branch advances after open.

use std::path::{Path, PathBuf};
use std::process::Stdio;

use async_trait::async_trait;
use sea_orm::DatabaseConnection;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
use tokio::process::{Child, ChildStdout};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::app_error::{AppCommandError, AppErrorCode};
use crate::commands::folders::{resolve_git_head, CommitSourceEpoch};
use crate::reference_search::matcher::{
    build_commit_uri, match_reference_candidate, SearchPattern,
};
use crate::reference_search::sources::file::resolve_open_workspace_root;
use crate::reference_search::sources::{ReferenceSourceCursor, SourcePage};
use crate::reference_search::types::{
    ReferenceCandidate, ReferenceCandidateMetadata, ReferenceDoneReason, ReferenceSearchSource,
};

/// Maximum retained bytes for one six-field commit metadata record.
pub const MAX_REFERENCE_COMMIT_RECORD_BYTES: usize = 64 * 1024;

/// Bound on stderr diagnostic prefix retained after draining to EOF.
const MAX_STDERR_DIAGNOSTIC_BYTES: usize = 4 * 1024;

/// Pull-driven commit cursor over one open workspace's git history.
pub struct CommitCursor {
    pattern: SearchPattern,
    limit: usize,
    epoch: CommitSourceEpoch,
    epoch_opaque: String,
    /// Filesystem path used as `git` cwd (canonical open workspace).
    repo_cwd: PathBuf,
    child: Option<Child>,
    stdout: Option<BufReader<ChildStdout>>,
    stderr_task: Option<JoinHandle<String>>,
    source_ordinal: u64,
    published: usize,
    finished: bool,
    unborn: bool,
    #[cfg(test)]
    spawned_args: Vec<String>,
    #[cfg(test)]
    max_retained_bytes: usize,
}

/// Pure argument list for the production commit log process.
pub(crate) fn commit_log_args(captured_head: &str) -> Vec<String> {
    vec![
        "log".to_string(),
        "-z".to_string(),
        "--format=%H%x00%h%x00%an%x00%aI%x00%s%x00%B".to_string(),
        captured_head.to_string(),
    ]
}

/// Arguments for `git show -s` metadata reads used by validation.
pub(crate) fn commit_show_args(full_hash: &str) -> Vec<String> {
    vec![
        "show".to_string(),
        "-s".to_string(),
        "-z".to_string(),
        "--format=%H%x00%h%x00%an%x00%aI%x00%s%x00%B".to_string(),
        full_hash.to_string(),
    ]
}

impl CommitCursor {
    pub async fn open(
        db: &DatabaseConnection,
        workspace_path: &str,
        pattern: SearchPattern,
        limit: usize,
    ) -> Result<Self, AppCommandError> {
        let open_root = resolve_open_workspace_root(db, workspace_path).await?;
        let open_root_str = open_root.to_string_lossy().to_string();
        let head_info = resolve_git_head(&open_root_str).await?;
        if !head_info.is_repo {
            return Err(AppCommandError::new(
                AppErrorCode::NotAGitRepository,
                "workspace path is not a git repository",
            ));
        }
        let canonical_repo = head_info.canonical_repo.clone().ok_or_else(|| {
            AppCommandError::new(
                AppErrorCode::SourceFailed,
                "git repository root could not be resolved",
            )
        })?;
        let epoch = match head_info.head_sha.clone() {
            Some(head) => CommitSourceEpoch {
                canonical_repo,
                branch: head_info.branch.clone(),
                detached: head_info.detached,
                head,
            },
            None => CommitSourceEpoch {
                canonical_repo,
                branch: head_info.branch.clone(),
                detached: false,
                head: CommitSourceEpoch::UNBORN_HEAD.to_string(),
            },
        };
        let epoch_opaque = head_info
            .reference_source_epoch
            .clone()
            .unwrap_or_else(|| epoch.opaque());
        let unborn = epoch.is_unborn();

        Ok(Self {
            pattern,
            limit,
            epoch,
            epoch_opaque,
            repo_cwd: open_root,
            child: None,
            stdout: None,
            stderr_task: None,
            source_ordinal: 0,
            published: 0,
            finished: unborn,
            unborn,
            #[cfg(test)]
            spawned_args: Vec::new(),
            #[cfg(test)]
            max_retained_bytes: 0,
        })
    }

    #[cfg(test)]
    pub fn spawned_args_for_test(&self) -> &[String] {
        &self.spawned_args
    }

    #[cfg(test)]
    pub fn has_live_child_for_test(&self) -> bool {
        self.child.is_some()
    }

    #[cfg(test)]
    pub fn max_retained_bytes_for_test(&self) -> usize {
        self.max_retained_bytes
    }

    #[cfg(test)]
    pub fn captured_head_for_test(&self) -> &str {
        &self.epoch.head
    }

    #[cfg(test)]
    pub fn epoch_opaque_for_test(&self) -> &str {
        &self.epoch_opaque
    }

    async fn ensure_spawned(&mut self) -> Result<(), AppCommandError> {
        if self.child.is_some() || self.unborn || self.finished {
            return Ok(());
        }
        let args = commit_log_args(&self.epoch.head);
        #[cfg(test)]
        {
            self.spawned_args = args.clone();
        }
        let mut child = crate::process::tokio_command("git")
            .args(&args)
            .current_dir(&self.repo_cwd)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|err| {
                AppCommandError::new(AppErrorCode::SourceFailed, "failed to spawn git log")
                    .with_detail(err.to_string())
            })?;

        let stdout = child.stdout.take().ok_or_else(|| {
            AppCommandError::new(AppErrorCode::SourceFailed, "git log stdout missing")
        })?;
        let stderr = child.stderr.take().ok_or_else(|| {
            AppCommandError::new(AppErrorCode::SourceFailed, "git log stderr missing")
        })?;

        let stderr_task = tokio::spawn(async move { drain_stderr_prefix(stderr).await });

        self.stdout = Some(BufReader::new(stdout));
        self.stderr_task = Some(stderr_task);
        self.child = Some(child);
        Ok(())
    }

    async fn terminate_child(&mut self) {
        self.stdout = None;
        if let Some(mut child) = self.child.take() {
            match child.try_wait() {
                Ok(Some(_)) => {}
                Ok(None) | Err(_) => {
                    let _ = child.start_kill();
                }
            }
            let _ = child.wait().await;
        }
        if let Some(handle) = self.stderr_task.take() {
            let _ = handle.await;
        }
    }

    fn build_candidate(&self, fields: ParsedCommitFields, source_ordinal: u64) -> ReferenceCandidate {
        let keywords = format!(
            "{} {} {} {}",
            fields.short_hash, fields.full_hash, fields.subject, fields.author
        );
        ReferenceCandidate {
            source: ReferenceSearchSource::Commit,
            uri: build_commit_uri(&self.epoch.canonical_repo, &fields.full_hash),
            id: fields.full_hash.clone(),
            label: fields.short_hash.clone(),
            detail: Some(fields.subject.clone()),
            keywords,
            metadata: ReferenceCandidateMetadata::Commit {
                canonical_repo: self.epoch.canonical_repo.clone(),
                full_hash: fields.full_hash,
                short_hash: fields.short_hash,
                subject: fields.subject,
                message: fields.message,
                author: fields.author,
                authored_at: fields.authored_at,
            },
            source_ordinal,
            regex_rank: None,
        }
    }

    fn empty_done_page(&self, reason: ReferenceDoneReason) -> SourcePage {
        SourcePage {
            items: Vec::new(),
            source_epoch: Some(self.epoch_opaque.clone()),
            done: true,
            done_reason: Some(reason),
        }
    }
}

#[async_trait]
impl ReferenceSourceCursor for CommitCursor {
    async fn next_page(
        &mut self,
        page_size: usize,
        token: CancellationToken,
    ) -> Result<SourcePage, AppCommandError> {
        if token.is_cancelled() {
            self.terminate_child().await;
            return Err(cancelled());
        }

        if self.unborn {
            self.finished = true;
            return Ok(self.empty_done_page(ReferenceDoneReason::Exhausted));
        }

        if self.finished || self.published >= self.limit {
            self.finished = true;
            self.terminate_child().await;
            return Ok(self.empty_done_page(if self.published >= self.limit {
                ReferenceDoneReason::Limit
            } else {
                ReferenceDoneReason::Exhausted
            }));
        }

        let remaining = self.limit - self.published;
        let want = page_size.min(remaining);
        if want == 0 {
            self.finished = true;
            self.terminate_child().await;
            return Ok(self.empty_done_page(ReferenceDoneReason::Limit));
        }

        self.ensure_spawned().await?;

        let mut items = Vec::with_capacity(want);
        let mut exhausted = false;

        while items.len() < want {
            if token.is_cancelled() {
                self.terminate_child().await;
                return Err(cancelled());
            }

            let Some(reader) = self.stdout.as_mut() else {
                exhausted = true;
                break;
            };

            #[cfg(test)]
            let mut page_max_retained = self.max_retained_bytes;

            let read = read_six_nul_fields(
                reader,
                MAX_REFERENCE_COMMIT_RECORD_BYTES,
                #[cfg(test)]
                &mut page_max_retained,
            )
            .await?;

            #[cfg(test)]
            {
                self.max_retained_bytes = page_max_retained;
            }

            match read {
                SixFieldRead::Eof => {
                    exhausted = true;
                    break;
                }
                SixFieldRead::Oversized => {
                    self.source_ordinal = self.source_ordinal.saturating_add(1);
                    tracing::warn!(
                        ordinal = self.source_ordinal,
                        "skipping oversized commit metadata record (>{} bytes)",
                        MAX_REFERENCE_COMMIT_RECORD_BYTES
                    );
                    continue;
                }
                SixFieldRead::Fields(raw) => {
                    self.source_ordinal = self.source_ordinal.saturating_add(1);
                    let ordinal = self.source_ordinal;
                    let fields = ParsedCommitFields::from_raw(raw);
                    let candidate = self.build_candidate(fields, ordinal);
                    let Some(field_match) = match_reference_candidate(&self.pattern, &candidate)
                    else {
                        continue;
                    };
                    let mut published = candidate;
                    published.regex_rank = field_match.regex_rank;
                    items.push(published);
                }
            }
        }

        self.published += items.len();
        let hit_limit = self.published >= self.limit;
        let done = exhausted || hit_limit || items.len() < want;
        if done {
            self.finished = true;
            self.terminate_child().await;
        }

        let done_reason = if !done {
            None
        } else if hit_limit {
            Some(ReferenceDoneReason::Limit)
        } else {
            Some(ReferenceDoneReason::Exhausted)
        };

        Ok(SourcePage {
            items,
            source_epoch: Some(self.epoch_opaque.clone()),
            done,
            done_reason,
        })
    }

    async fn close(&mut self) {
        self.finished = true;
        self.terminate_child().await;
    }
}

#[derive(Debug)]
struct ParsedCommitFields {
    full_hash: String,
    short_hash: String,
    author: String,
    authored_at: String,
    subject: String,
    message: String,
}

impl ParsedCommitFields {
    fn from_raw(raw: [String; 6]) -> Self {
        let [full_hash, short_hash, author, authored_at, subject, message] = raw;
        Self {
            full_hash,
            short_hash,
            author,
            authored_at,
            subject,
            message,
        }
    }
}

#[derive(Debug)]
pub(crate) enum SixFieldRead {
    Fields([String; 6]),
    Oversized,
    Eof,
}

/// Bounded six-field NUL reader shared by commit search and validation.
///
/// Appends only while the cumulative six-field payload is at most `max_bytes`,
/// then drains remaining NULs without storing oversized content.
pub(crate) async fn read_six_nul_fields<R>(
    reader: &mut BufReader<R>,
    max_bytes: usize,
    #[cfg(test)] max_retained: &mut usize,
) -> Result<SixFieldRead, AppCommandError>
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut fields: Vec<Vec<u8>> = Vec::with_capacity(6);
    let mut current = Vec::new();
    let mut total: usize = 0;
    let mut oversized = false;
    let mut nuls_seen: usize = 0;

    loop {
        let buf = reader.fill_buf().await.map_err(|err| {
            AppCommandError::new(AppErrorCode::SourceFailed, "failed reading git commit stream")
                .with_detail(err.to_string())
        })?;
        if buf.is_empty() {
            if nuls_seen == 0 && current.is_empty() && fields.is_empty() {
                return Ok(SixFieldRead::Eof);
            }
            return Err(AppCommandError::new(
                AppErrorCode::SourceFailed,
                "truncated git commit metadata record",
            ));
        }

        #[cfg(test)]
        {
            let retained = total.saturating_add(buf.len());
            if retained > *max_retained {
                *max_retained = retained;
            }
        }

        if let Some(pos) = buf.iter().position(|&b| b == 0) {
            if !oversized {
                if total.saturating_add(pos) > max_bytes {
                    oversized = true;
                    fields.clear();
                    current.clear();
                    total = 0;
                } else {
                    current.extend_from_slice(&buf[..pos]);
                    total += pos;
                    fields.push(std::mem::take(&mut current));
                }
            }
            reader.consume(pos + 1);
            nuls_seen += 1;
            if nuls_seen == 6 {
                if oversized {
                    return Ok(SixFieldRead::Oversized);
                }
                let mut out: [String; 6] = Default::default();
                for (i, field) in fields.into_iter().enumerate() {
                    out[i] = String::from_utf8_lossy(&field).into_owned();
                }
                return Ok(SixFieldRead::Fields(out));
            }
        } else {
            let n = buf.len();
            if !oversized {
                if total.saturating_add(n) > max_bytes {
                    oversized = true;
                    fields.clear();
                    current.clear();
                    total = 0;
                } else {
                    current.extend_from_slice(buf);
                    total += n;
                }
            }
            reader.consume(n);
        }
    }
}

/// Build a commit candidate from six parsed metadata fields (search/validation).
pub(crate) fn build_commit_candidate(
    canonical_repo: &str,
    fields: [String; 6],
    source_ordinal: u64,
) -> ReferenceCandidate {
    let parsed = ParsedCommitFields::from_raw(fields);
    let keywords = format!(
        "{} {} {} {}",
        parsed.short_hash, parsed.full_hash, parsed.subject, parsed.author
    );
    ReferenceCandidate {
        source: ReferenceSearchSource::Commit,
        uri: build_commit_uri(canonical_repo, &parsed.full_hash),
        id: parsed.full_hash.clone(),
        label: parsed.short_hash.clone(),
        detail: Some(parsed.subject.clone()),
        keywords,
        metadata: ReferenceCandidateMetadata::Commit {
            canonical_repo: canonical_repo.to_string(),
            full_hash: parsed.full_hash,
            short_hash: parsed.short_hash,
            subject: parsed.subject,
            message: parsed.message,
            author: parsed.author,
            authored_at: parsed.authored_at,
        },
        source_ordinal,
        regex_rank: None,
    }
}

/// Read a single commit's metadata via `git show -s` with the same bounded
/// six-field framing. Returns `Ok(None)` for oversized records.
pub(crate) async fn read_commit_show_fields(
    repo_cwd: &Path,
    full_hash: &str,
) -> Result<Option<[String; 6]>, AppCommandError> {
    let args = commit_show_args(full_hash);
    let mut child = crate::process::tokio_command("git")
        .args(&args)
        .current_dir(repo_cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|err| {
            AppCommandError::new(AppErrorCode::SourceFailed, "failed to spawn git show")
                .with_detail(err.to_string())
        })?;

    let stdout = child.stdout.take().ok_or_else(|| {
        AppCommandError::new(AppErrorCode::SourceFailed, "git show stdout missing")
    })?;
    let stderr = child.stderr.take();
    let stderr_task = stderr.map(|s| tokio::spawn(async move { drain_stderr_prefix(s).await }));

    let mut reader = BufReader::new(stdout);
    #[cfg(test)]
    let mut max_retained = 0usize;
    let read = read_six_nul_fields(
        &mut reader,
        MAX_REFERENCE_COMMIT_RECORD_BYTES,
        #[cfg(test)]
        &mut max_retained,
    )
    .await;

    // Drain remaining stdout so the child can exit.
    let mut sink = Vec::new();
    let _ = reader.read_to_end(&mut sink).await;
    drop(reader);

    let status = child.wait().await.map_err(|err| {
        AppCommandError::new(AppErrorCode::SourceFailed, "git show wait failed")
            .with_detail(err.to_string())
    })?;
    if let Some(handle) = stderr_task {
        let _ = handle.await;
    }

    match read? {
        SixFieldRead::Fields(fields) => {
            if !status.success() {
                return Err(AppCommandError::new(
                    AppErrorCode::SourceFailed,
                    "git show failed after producing metadata",
                ));
            }
            Ok(Some(fields))
        }
        SixFieldRead::Oversized => Ok(None),
        SixFieldRead::Eof => {
            if status.success() {
                Ok(None)
            } else {
                Err(AppCommandError::new(
                    AppErrorCode::SourceFailed,
                    "git show failed",
                ))
            }
        }
    }
}

async fn drain_stderr_prefix(mut stderr: impl tokio::io::AsyncRead + Unpin) -> String {
    let mut prefix = Vec::with_capacity(MAX_STDERR_DIAGNOSTIC_BYTES.min(512));
    let mut buf = [0u8; 4096];
    loop {
        match stderr.read(&mut buf).await {
            Ok(0) => break,
            Ok(n) => {
                if prefix.len() < MAX_STDERR_DIAGNOSTIC_BYTES {
                    let room = MAX_STDERR_DIAGNOSTIC_BYTES - prefix.len();
                    prefix.extend_from_slice(&buf[..n.min(room)]);
                }
                // Keep reading to EOF so a full pipe cannot block git.
            }
            Err(_) => break,
        }
    }
    String::from_utf8_lossy(&prefix).into_owned()
}

fn cancelled() -> AppCommandError {
    AppCommandError::new(AppErrorCode::Cancelled, "reference search cancelled")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_helpers::{fresh_in_memory_db, seed_folder};
    use crate::db::AppDatabase;
    use crate::reference_search::sources::literal;
    use std::process::Command;

    pub struct GitHistoryFixture {
        pub db: AppDatabase,
        _temp: tempfile::TempDir,
        path: String,
    }

    impl GitHistoryFixture {
        pub fn path(&self) -> &str {
            &self.path
        }

        pub fn path_buf(&self) -> &Path {
            Path::new(&self.path)
        }

        fn git(&self, args: &[&str]) {
            git_run(self.path_buf(), args);
        }

        pub fn commit_message(&self, message: &str) {
            self.git(&["commit", "-q", "--allow-empty", "-m", message]);
        }

        pub fn advance_branch(&self, message: &str) {
            self.git(&["checkout", "-q", "-b", "moved-branch"]);
            self.commit_message(message);
        }
    }

    fn git_run(dir: &Path, args: &[&str]) {
        let out = Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_CONFIG_GLOBAL", "NUL")
            .env("GIT_CONFIG_SYSTEM", "NUL")
            .env("GIT_AUTHOR_NAME", "Commit Author")
            .env("GIT_AUTHOR_EMAIL", "commit@example.com")
            .env("GIT_COMMITTER_NAME", "Commit Author")
            .env("GIT_COMMITTER_EMAIL", "commit@example.com")
            .output()
            .expect("spawn git");
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    pub async fn git_history_fixture(commit_count: usize) -> GitHistoryFixture {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().to_string_lossy().to_string();
        git_run(temp.path(), &["init", "-q"]);
        // Deterministic default branch name across git versions.
        let _ = Command::new("git")
            .args(["checkout", "-q", "-b", "main"])
            .current_dir(temp.path())
            .env("GIT_CONFIG_GLOBAL", "NUL")
            .env("GIT_CONFIG_SYSTEM", "NUL")
            .output();
        for i in 0..commit_count {
            git_run(
                temp.path(),
                &[
                    "commit",
                    "-q",
                    "--allow-empty",
                    "-m",
                    &format!("commit message {i}"),
                ],
            );
        }
        let db = fresh_in_memory_db().await;
        seed_folder(&db, &path).await;
        GitHistoryFixture {
            db,
            _temp: temp,
            path,
        }
    }

    fn assert_cancelled(result: Result<SourcePage, AppCommandError>) {
        let err = result.expect_err("expected cancelled");
        assert_eq!(err.code, AppErrorCode::Cancelled);
    }

    #[tokio::test]
    async fn commit_cursor_streams_current_history_without_diff_payloads_and_kills_on_cancel() {
        let fixture = git_history_fixture(12).await;
        let mut cursor = CommitCursor::open(&fixture.db.conn, fixture.path(), literal("commit"), 12)
            .await
            .expect("cursor");
        let page = cursor
            .next_page(5, CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(page.items.len(), 5);
        assert!(page.items.iter().all(|item| matches!(
            &item.metadata,
            ReferenceCandidateMetadata::Commit { .. }
        )));
        assert!(!cursor.spawned_args_for_test().iter().any(|arg| {
            matches!(arg.as_str(), "--raw" | "--numstat" | "--name-only" | "--stat")
        }));
        assert_eq!(
            cursor.spawned_args_for_test().last().map(String::as_str),
            Some(cursor.captured_head_for_test())
        );
        let captured_head = cursor.captured_head_for_test().to_string();
        let first_page_ids: Vec<String> = page.items.iter().map(|i| i.id.clone()).collect();

        // Advance the checked-out branch; the cursor must stay pinned to the
        // captured object id and continue that history.
        fixture.advance_branch("post-open commit should not appear");
        let page2 = cursor
            .next_page(5, CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(page2.items.len(), 5);
        assert!(page2.items.iter().all(|item| item.id != captured_head
            || first_page_ids.contains(&item.id)));
        assert!(page2
            .items
            .iter()
            .all(|item| !item.detail.as_deref().unwrap_or("").contains("post-open")));

        let token = CancellationToken::new();
        token.cancel();
        assert_cancelled(cursor.next_page(5, token).await);
        assert!(!cursor.has_live_child_for_test());
    }

    #[tokio::test]
    async fn unborn_repository_returns_a_stable_empty_epoch_page_without_spawning_git_log() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().to_string_lossy().to_string();
        git_run(temp.path(), &["init", "-q"]);
        let db = fresh_in_memory_db().await;
        seed_folder(&db, &path).await;

        let info = resolve_git_head(&path).await.expect("head");
        assert!(info.is_repo);
        assert_eq!(info.head_sha, None);
        assert!(info.canonical_repo.is_some());
        let epoch = info.reference_source_epoch.clone().expect("epoch");

        let mut cursor = CommitCursor::open(&db.conn, &path, literal("commit"), 12)
            .await
            .expect("cursor");
        let page = cursor
            .next_page(5, CancellationToken::new())
            .await
            .unwrap();
        assert!(page.items.is_empty());
        assert!(page.done);
        assert_eq!(page.done_reason, Some(ReferenceDoneReason::Exhausted));
        assert_eq!(page.source_epoch.as_deref(), Some(epoch.as_str()));
        assert!(!cursor.has_live_child_for_test());
        assert!(cursor.spawned_args_for_test().is_empty());

        git_run(
            temp.path(),
            &["commit", "-q", "--allow-empty", "-m", "first"],
        );
        let after = resolve_git_head(&path).await.expect("head after");
        assert!(after.head_sha.is_some());
        assert_ne!(after.reference_source_epoch.as_deref(), Some(epoch.as_str()));
    }

    #[tokio::test]
    async fn oversized_commit_metadata_is_drained_and_the_next_record_stays_aligned() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().to_string_lossy().to_string();
        git_run(temp.path(), &["init", "-q"]);
        // Older normal matching commit.
        git_run(
            temp.path(),
            &[
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "commit normal subject\n\nnormal body",
            ],
        );
        let older = Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(temp.path())
            .output()
            .expect("rev-parse");
        let older_hash = String::from_utf8_lossy(&older.stdout).trim().to_string();

        // Newest commit with a body that pushes the six-field payload over 64 KiB.
        // Write the message via a file — Windows rejects multi-64KiB `-m` argv.
        let huge_body = "X".repeat(70 * 1024);
        let huge_message = format!("commit oversized subject\n\n{huge_body}");
        let msg_path = temp.path().join("huge-commit-msg.txt");
        std::fs::write(&msg_path, &huge_message).expect("write huge message");
        git_run(
            temp.path(),
            &[
                "commit",
                "-q",
                "--allow-empty",
                "-F",
                msg_path.to_str().expect("utf8 msg path"),
            ],
        );

        let db = fresh_in_memory_db().await;
        seed_folder(&db, &path).await;
        let mut cursor = CommitCursor::open(&db.conn, &path, literal("commit"), 12)
            .await
            .expect("cursor");
        let page = cursor
            .next_page(5, CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(page.items.len(), 1);
        assert_eq!(page.items[0].id, older_hash);
        assert!(matches!(
            &page.items[0].metadata,
            ReferenceCandidateMetadata::Commit {
                subject,
                message,
                full_hash,
                ..
            } if subject == "commit normal subject"
                && message.contains("normal body")
                && full_hash == &older_hash
        ));
        assert!(
            cursor.max_retained_bytes_for_test()
                <= MAX_REFERENCE_COMMIT_RECORD_BYTES + 64 * 1024,
            "retained {} bytes",
            cursor.max_retained_bytes_for_test()
        );
    }

    #[tokio::test]
    async fn multiline_and_control_text_in_messages_do_not_shift_field_alignment() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().to_string_lossy().to_string();
        git_run(temp.path(), &["init", "-q"]);
        git_run(
            temp.path(),
            &[
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "commit older\n\nbody with\n\nblank lines and \x1e RS-like text",
            ],
        );
        let older = Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(temp.path())
            .output()
            .expect("rev-parse");
        let older_hash = String::from_utf8_lossy(&older.stdout).trim().to_string();
        git_run(
            temp.path(),
            &[
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "commit newer\n\nsecond body\n\nwith blanks",
            ],
        );
        let newer = Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(temp.path())
            .output()
            .expect("rev-parse");
        let newer_hash = String::from_utf8_lossy(&newer.stdout).trim().to_string();

        let db = fresh_in_memory_db().await;
        seed_folder(&db, &path).await;
        let mut cursor = CommitCursor::open(&db.conn, &path, literal("commit"), 12)
            .await
            .expect("cursor");
        let page = cursor
            .next_page(5, CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(page.items.len(), 2);
        assert_eq!(page.items[0].id, newer_hash);
        assert_eq!(page.items[1].id, older_hash);
        assert!(matches!(
            &page.items[1].metadata,
            ReferenceCandidateMetadata::Commit { message, .. }
                if message.contains("blank lines") && message.contains("RS-like")
        ));
    }
}
