//! Stable ignore-aware pull walker for workspace file reference search.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use sea_orm::DatabaseConnection;
use tokio_util::sync::CancellationToken;

use crate::app_error::{AppCommandError, AppErrorCode};
use crate::commands::folders::workspace_walk_builder;
use crate::db::service::folder_service;
use crate::parsers::normalize_path_for_matching;
use crate::reference_search::matcher::{
    build_file_uri, match_reference_candidate, normalize_path_for_uri, SearchPattern,
};
use crate::reference_search::sources::{ReferenceSourceCursor, SourcePage};
use crate::reference_search::types::{
    ReferenceCandidate, ReferenceCandidateMetadata, ReferenceDoneReason, ReferenceFileKind,
    ReferenceSearchSource,
};

/// Hard-skip ignore rule file names (same set as workspace file search).
const IGNORE_FILE_NAMES: &[&str] = &[".gitignore", ".ignore", ".rgignore"];

/// Pull-driven file/directory cursor over one open workspace root.
pub struct FileCursor {
    pattern: SearchPattern,
    limit: usize,
    /// Canonical root for containment checks (may retain Windows `\\?\` form).
    canonical_root: PathBuf,
    /// Verbatim-prefix-stripped display form for metadata and URI construction.
    root_display: String,
    walker: Option<ignore::Walk>,
    source_ordinal: u64,
    published: usize,
    finished: bool,
}

impl FileCursor {
    pub async fn open(
        db: &DatabaseConnection,
        workspace_path: &str,
        pattern: SearchPattern,
        limit: usize,
    ) -> Result<Self, AppCommandError> {
        let canonical_root = resolve_open_workspace_root(db, workspace_path).await?;
        let root_display = normalize_path_for_uri(&canonical_root);
        let mut builder = workspace_walk_builder(&canonical_root, None, true);
        builder.sort_by_file_path(|a, b| a.cmp(b));
        let walker = builder.build();
        Ok(Self {
            pattern,
            limit,
            canonical_root,
            root_display,
            walker: Some(walker),
            source_ordinal: 0,
            published: 0,
            finished: false,
        })
    }
}

/// Return the canonical open-workspace root when `requested_path` matches one
/// live open-folder row under platform path-equivalence rules.
pub(crate) async fn resolve_open_workspace_root(
    conn: &DatabaseConnection,
    requested_path: &str,
) -> Result<PathBuf, AppCommandError> {
    if requested_path.is_empty() {
        return Err(AppCommandError::new(
            AppErrorCode::InvalidRequest,
            "workspace path must not be empty",
        ));
    }
    let requested = PathBuf::from(requested_path);
    if !requested.is_absolute() {
        return Err(AppCommandError::new(
            AppErrorCode::InvalidRequest,
            "workspace path must be absolute",
        ));
    }

    let requested_canonical = std::fs::canonicalize(&requested).map_err(|err| {
        // Missing / non-directory / unreadable requested path → InvalidRequest.
        // Other I/O failures use typed conversion.
        match err.kind() {
            std::io::ErrorKind::NotFound => AppCommandError::new(
                AppErrorCode::InvalidRequest,
                "workspace path does not exist",
            )
            .with_detail(err.to_string()),
            _ if !requested.exists() => AppCommandError::new(
                AppErrorCode::InvalidRequest,
                "workspace path does not exist",
            )
            .with_detail(err.to_string()),
            _ => AppCommandError::io(err),
        }
    })?;

    if !requested_canonical.is_dir() {
        return Err(AppCommandError::new(
            AppErrorCode::InvalidRequest,
            "workspace path is not a directory",
        ));
    }

    let open_folders = folder_service::list_open_folders(conn)
        .await
        .map_err(AppCommandError::from)?;

    for folder in open_folders {
        let folder_path = PathBuf::from(&folder.path);
        let folder_canonical = match std::fs::canonicalize(&folder_path) {
            Ok(path) => path,
            Err(err) => {
                tracing::debug!(
                    path = %folder.path,
                    error = %err,
                    "skipping stale open-folder path during workspace resolution"
                );
                continue;
            }
        };
        if open_folder_paths_match(&folder_canonical, &requested_canonical) {
            return Ok(requested_canonical);
        }
    }

    Err(AppCommandError::new(
        AppErrorCode::InvalidRequest,
        "workspace path is not an open folder",
    ))
}

#[async_trait]
impl ReferenceSourceCursor for FileCursor {
    async fn next_page(
        &mut self,
        page_size: usize,
        token: CancellationToken,
    ) -> Result<SourcePage, AppCommandError> {
        if token.is_cancelled() {
            return Err(cancelled());
        }
        if self.finished || self.published >= self.limit {
            self.finished = true;
            self.walker = None;
            return Ok(SourcePage {
                items: Vec::new(),
                source_epoch: None,
                done: true,
                done_reason: Some(if self.published >= self.limit {
                    ReferenceDoneReason::Limit
                } else {
                    ReferenceDoneReason::Exhausted
                }),
            });
        }

        let remaining = self.limit - self.published;
        let want = page_size.min(remaining);
        if want == 0 {
            self.finished = true;
            self.walker = None;
            return Ok(SourcePage {
                items: Vec::new(),
                source_epoch: None,
                done: true,
                done_reason: Some(ReferenceDoneReason::Limit),
            });
        }

        let Some(walker) = self.walker.take() else {
            self.finished = true;
            return Ok(SourcePage {
                items: Vec::new(),
                source_epoch: None,
                done: true,
                done_reason: Some(ReferenceDoneReason::Exhausted),
            });
        };

        let pattern = self.pattern.clone();
        let canonical_root = self.canonical_root.clone();
        let root_display = self.root_display.clone();
        let mut source_ordinal = self.source_ordinal;
        let published_before = self.published;

        let scan = tokio::task::spawn_blocking(move || {
            scan_file_page(
                walker,
                pattern,
                canonical_root,
                root_display,
                &mut source_ordinal,
                want,
                token,
            )
        })
        .await
        .map_err(|err| {
            AppCommandError::io_error("File reference search task failed")
                .with_detail(err.to_string())
        })?;

        let (walker, items, walk_done_reason, cancelled_mid) = scan?;
        self.source_ordinal = source_ordinal;

        if cancelled_mid {
            // Do not publish a partial page; restore walker for a possible retry.
            self.walker = Some(walker);
            return Err(cancelled());
        }

        self.published = published_before + items.len();
        let hit_limit = self.published >= self.limit;
        // A short page means the walk produced no more matches (or hit the
        // snapshotted limit mid-page via `want`).
        let done = walk_done_reason.is_some() || hit_limit || items.len() < want;
        let done_reason = if !done {
            None
        } else if hit_limit {
            Some(ReferenceDoneReason::Limit)
        } else {
            Some(walk_done_reason.unwrap_or(ReferenceDoneReason::Exhausted))
        };

        if done {
            self.finished = true;
            self.walker = None;
        } else {
            self.walker = Some(walker);
        }

        Ok(SourcePage {
            items,
            source_epoch: None,
            done,
            done_reason,
        })
    }

    async fn close(&mut self) {
        self.walker = None;
        self.finished = true;
    }
}

fn cancelled() -> AppCommandError {
    AppCommandError::new(AppErrorCode::Cancelled, "reference search cancelled")
}

fn scan_file_page(
    mut walker: ignore::Walk,
    pattern: SearchPattern,
    canonical_root: PathBuf,
    root_display: String,
    source_ordinal: &mut u64,
    want: usize,
    token: CancellationToken,
) -> Result<
    (
        ignore::Walk,
        Vec<ReferenceCandidate>,
        Option<ReferenceDoneReason>,
        bool,
    ),
    AppCommandError,
> {
    let mut items = Vec::with_capacity(want);
    let mut done_reason = None;

    while items.len() < want {
        if token.is_cancelled() {
            return Ok((walker, Vec::new(), None, true));
        }

        let Some(result) = walker.next() else {
            done_reason = Some(ReferenceDoneReason::Exhausted);
            break;
        };

        let entry = result.map_err(|err| {
            AppCommandError::new(AppErrorCode::SourceFailed, "Failed to walk workspace files")
                .with_detail(err.to_string())
        })?;

        let entry_path = entry.path();
        if entry_path == canonical_root.as_path() {
            continue;
        }

        let name = entry.file_name().to_string_lossy();
        if IGNORE_FILE_NAMES.iter().any(|n| *n == name.as_ref()) {
            continue;
        }
        // `.git` / `__pycache__` / `.DS_Store` are already filtered by the
        // shared walk builder; keep the name checks as a safety net for
        // files that might still surface.

        let Ok(relative) = entry_path.strip_prefix(&canonical_root) else {
            tracing::debug!(
                path = %entry_path.display(),
                "skipping walk entry outside canonical workspace root"
            );
            continue;
        };
        let relative_path = relative.to_string_lossy().replace('\\', "/");
        if relative_path.is_empty() {
            continue;
        }

        // Eligible entry: assign ordinal before matching.
        *source_ordinal = source_ordinal.saturating_add(1);
        let ordinal = *source_ordinal;

        // Containment: resolved target must stay under the canonical root.
        let resolved = match std::fs::canonicalize(entry_path) {
            Ok(path) => path,
            Err(err) => {
                tracing::debug!(
                    path = %entry_path.display(),
                    error = %err,
                    "skipping file candidate with broken link or unreadable path"
                );
                continue;
            }
        };
        if !path_is_under_root(&resolved, &canonical_root) {
            tracing::debug!(
                path = %entry_path.display(),
                resolved = %resolved.display(),
                "skipping file candidate that escapes workspace root"
            );
            continue;
        }

        let is_dir = entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false);
        let label = name.into_owned();
        let uri_path = join_display_root(&root_display, &relative_path);
        let candidate = ReferenceCandidate {
            source: ReferenceSearchSource::File,
            uri: build_file_uri(&uri_path),
            id: relative_path.clone(),
            label,
            detail: Some(relative_path.clone()),
            keywords: relative_path.clone(),
            metadata: ReferenceCandidateMetadata::File {
                canonical_workspace_root: root_display.clone(),
                relative_path,
                entry_kind: if is_dir {
                    ReferenceFileKind::Directory
                } else {
                    ReferenceFileKind::File
                },
            },
            source_ordinal: ordinal,
            regex_rank: None,
        };

        let Some(field_match) = match_reference_candidate(&pattern, &candidate) else {
            continue;
        };
        let mut published = candidate;
        published.regex_rank = field_match.regex_rank;
        items.push(published);
    }

    Ok((walker, items, done_reason, false))
}

fn join_display_root(root_display: &str, relative_path: &str) -> PathBuf {
    let mut out = PathBuf::from(root_display);
    for part in relative_path.split('/') {
        if !part.is_empty() {
            out.push(part);
        }
    }
    out
}

/// Open-folder membership: exact canonical equality on Unix; Windows folds
/// case, separators, and verbatim prefixes via `normalize_path_for_matching`.
fn open_folder_paths_match(folder: &Path, requested: &Path) -> bool {
    #[cfg(windows)]
    {
        normalize_path_for_matching(&folder.to_string_lossy())
            == normalize_path_for_matching(&requested.to_string_lossy())
    }
    #[cfg(not(windows))]
    {
        folder == requested
    }
}

fn path_is_under_root(target: &Path, root: &Path) -> bool {
    if target.starts_with(root) {
        return true;
    }
    // Windows: case / separator / verbatim differences after canonicalize.
    let target_key = normalize_path_for_matching(&target.to_string_lossy());
    let root_key = normalize_path_for_matching(&root.to_string_lossy());
    if target_key == root_key {
        return true;
    }
    let root_prefix = if root_key.ends_with('/') {
        root_key
    } else {
        format!("{root_key}/")
    };
    target_key.starts_with(&root_prefix)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_helpers::{fresh_in_memory_db, seed_folder};
    use crate::db::AppDatabase;
    use crate::reference_search::sources::literal;
    use std::fs;
    use std::io::Write;
    use tempfile::TempDir;

    pub struct OpenWorkspaceFixture {
        pub db: AppDatabase,
        _temp: TempDir,
        path: String,
    }

    impl OpenWorkspaceFixture {
        pub fn path(&self) -> &str {
            &self.path
        }

        pub fn write(&self, relative: &str, contents: &str) {
            let target = Path::new(&self.path).join(relative);
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent).expect("create parent dirs");
            }
            let mut file = fs::File::create(&target).expect("create file");
            file.write_all(contents.as_bytes()).expect("write file");
        }
    }

    pub async fn open_workspace_fixture() -> OpenWorkspaceFixture {
        let temp = tempfile::tempdir().expect("tempdir");
        // Use the exact path string we seed so open-folder membership matches.
        let path = temp.path().to_string_lossy().to_string();
        let db = fresh_in_memory_db().await;
        seed_folder(&db, &path).await;
        OpenWorkspaceFixture {
            db,
            _temp: temp,
            path,
        }
    }

    #[tokio::test]
    async fn file_cursor_is_ignore_aware_stable_and_stops_at_limit() {
        let fixture = open_workspace_fixture().await;
        fixture.write(".gitignore", "ignored/\n");
        fixture.write("ignored/no.ts", "x");
        for name in ["b.ts", "a.ts", "dir/c.ts", "dir/d.ts", "dir/e.ts", "z.ts"] {
            fixture.write(name, "x");
        }
        let mut cursor = FileCursor::open(&fixture.db.conn, fixture.path(), literal(".ts"), 6)
            .await
            .expect("cursor");
        let first = cursor.next_page(5, CancellationToken::new()).await.unwrap();
        let second = cursor.next_page(5, CancellationToken::new()).await.unwrap();
        assert_eq!(first.items.len(), 5);
        assert_eq!(second.items.len(), 1);
        assert!(matches!(
            &first.items[0].metadata,
            ReferenceCandidateMetadata::File { relative_path, .. } if relative_path == "a.ts"
        ));
        assert!(first
            .items
            .iter()
            .chain(&second.items)
            .all(|item| !item.uri.contains("ignored")));
        assert!(second.done);
        assert_eq!(second.done_reason, Some(ReferenceDoneReason::Limit));
    }

    #[tokio::test]
    async fn file_cursor_cancel_returns_cancelled_without_partial_items() {
        let fixture = open_workspace_fixture().await;
        for i in 0..20 {
            fixture.write(&format!("f{i:02}.ts"), "x");
        }
        let mut cursor = FileCursor::open(&fixture.db.conn, fixture.path(), literal(".ts"), 50)
            .await
            .expect("cursor");
        let token = CancellationToken::new();
        token.cancel();
        let err = cursor
            .next_page(5, token)
            .await
            .expect_err("cancelled scan");
        assert_eq!(err.code, AppErrorCode::Cancelled);
    }

    #[tokio::test]
    async fn resolve_open_workspace_root_rejects_unopened_path() {
        let fixture = open_workspace_fixture().await;
        let other = tempfile::tempdir().expect("other");
        let err = resolve_open_workspace_root(&fixture.db.conn, &other.path().to_string_lossy())
            .await
            .expect_err("unopened");
        assert_eq!(err.code, AppErrorCode::InvalidRequest);
    }
}
