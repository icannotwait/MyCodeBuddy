//! Source-specific live validation for selected reference candidates.
//!
//! Rebuilds authoritative candidates from filesystem / DB / git and applies
//! the same [`match_reference_candidate`] field mapping used by search.

use std::path::{Component, Path, PathBuf};

use sea_orm::{ColumnTrait, EntityTrait, JoinType, QueryFilter, QuerySelect, RelationTrait};

use crate::app_error::{AppCommandError, AppErrorCode};
use crate::commands::folders::{resolve_git_head, CommitSourceEpoch};
use crate::db::entities::conversation::ConversationKind;
use crate::db::entities::{conversation, folder};
use crate::db::AppDatabase;
use crate::models::agent::AgentType;
use crate::reference_search::matcher::{
    build_file_uri, match_reference_candidate, normalize_path_for_uri, SearchPattern,
};
use crate::reference_search::sources::commit::{build_commit_candidate, read_commit_show_fields};
use crate::reference_search::sources::conversation::build_conversation_candidate;
use crate::reference_search::sources::file::{path_is_under_root, resolve_open_workspace_root};
use crate::reference_search::types::{
    parse_canonical_uuid_v4, validate_source_epoch_scope, validate_source_scope,
    ReferenceCandidate, ReferenceCandidateMetadata, ReferenceCandidateValidation,
    ReferenceFileKind, ReferenceSearchSource, ValidateReferenceCandidateRequest,
};

/// Validate a selected candidate against live workspace / DB / git state.
pub async fn validate_reference_candidate_core(
    db: &AppDatabase,
    request: ValidateReferenceCandidateRequest,
) -> Result<ReferenceCandidateValidation, AppCommandError> {
    let validation_request_id = parse_canonical_uuid_v4(&request.validation_request_id)?
        .hyphenated()
        .to_string();
    validate_source_scope(request.source, request.workspace_path.as_deref())?;
    validate_source_epoch_scope(request.source, request.source_epoch.as_deref())?;
    let pattern = SearchPattern::parse(&request.query)?;
    let candidate = match request.source {
        ReferenceSearchSource::File => validate_file_candidate(db, &request).await?,
        ReferenceSearchSource::Conversation => {
            validate_conversation_candidate(db, &request).await?
        }
        ReferenceSearchSource::Commit => validate_commit_candidate(db, &request).await?,
    };
    let Some(mut candidate) = candidate else {
        return Ok(ReferenceCandidateValidation::NotFound {
            validation_request_id,
        });
    };
    let field_match = match_reference_candidate(&pattern, &candidate);
    let regex_rank = field_match
        .as_ref()
        .and_then(|matched| matched.regex_rank.clone());
    candidate.regex_rank = regex_rank.clone();
    Ok(if field_match.is_some() {
        ReferenceCandidateValidation::Match {
            validation_request_id,
            candidate,
            regex_rank,
        }
    } else {
        ReferenceCandidateValidation::NotMatch {
            validation_request_id,
            candidate,
            regex_rank,
        }
    })
}

async fn validate_file_candidate(
    db: &AppDatabase,
    request: &ValidateReferenceCandidateRequest,
) -> Result<Option<ReferenceCandidate>, AppCommandError> {
    let workspace_path = request
        .workspace_path
        .as_deref()
        .expect("validated workspace scope");
    let open_root = resolve_open_workspace_root(&db.conn, workspace_path).await?;
    let root_display = normalize_path_for_uri(&open_root);

    let relative = parse_file_uri_relative(&request.uri, &root_display, &open_root)?;
    let Some(relative_path) = relative else {
        return Ok(None);
    };

    let mut platform_path = open_root.clone();
    for segment in relative_path.split('/') {
        platform_path.push(segment);
    }

    let resolved = match std::fs::canonicalize(&platform_path) {
        Ok(path) => path,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => {
            // Missing intermediate path components also surface as not-found.
            if !platform_path.exists() {
                return Ok(None);
            }
            return Err(AppCommandError::io(err));
        }
    };

    if !path_is_under_root(&resolved, &open_root) {
        return Err(AppCommandError::new(
            AppErrorCode::InvalidRequest,
            "file uri escapes open workspace root",
        ));
    }

    let is_dir = resolved.is_dir();
    let label = Path::new(&relative_path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| relative_path.clone());

    let uri_path = {
        let mut out = PathBuf::from(&root_display);
        for part in relative_path.split('/') {
            if !part.is_empty() {
                out.push(part);
            }
        }
        out
    };

    Ok(Some(ReferenceCandidate {
        source: ReferenceSearchSource::File,
        uri: build_file_uri(&uri_path),
        id: relative_path.clone(),
        label,
        detail: Some(relative_path.clone()),
        keywords: relative_path.clone(),
        metadata: ReferenceCandidateMetadata::File {
            canonical_workspace_root: root_display,
            relative_path,
            entry_kind: if is_dir {
                ReferenceFileKind::Directory
            } else {
                ReferenceFileKind::File
            },
        },
        source_ordinal: 0,
        regex_rank: None,
    }))
}

async fn validate_conversation_candidate(
    db: &AppDatabase,
    request: &ValidateReferenceCandidateRequest,
) -> Result<Option<ReferenceCandidate>, AppCommandError> {
    let conversation_id = parse_session_uri(&request.uri)?;
    let joined = conversation::Entity::find_by_id(conversation_id)
        .join(JoinType::InnerJoin, conversation::Relation::Folder.def())
        .filter(conversation::Column::DeletedAt.is_null())
        .filter(conversation::Column::ParentId.is_null())
        .filter(
            conversation::Column::Kind.is_in([ConversationKind::Regular, ConversationKind::Chat]),
        )
        .filter(folder::Column::DeletedAt.is_null())
        .select_also(folder::Entity)
        .one(&db.conn)
        .await
        .map_err(|err| AppCommandError::from(crate::db::error::DbError::from(err)))?;

    let Some((conv, Some(folder_row))) = joined else {
        return Ok(None);
    };

    let Some(agent_type) = parse_agent_type(&conv.agent_type) else {
        return Ok(None);
    };

    Ok(Some(build_conversation_candidate(
        conv.id,
        conv.title.as_deref(),
        agent_type,
        conv.status,
        conv.git_branch,
        folder_row.name,
        folder_row.path,
        0,
    )))
}

async fn validate_commit_candidate(
    db: &AppDatabase,
    request: &ValidateReferenceCandidateRequest,
) -> Result<Option<ReferenceCandidate>, AppCommandError> {
    let workspace_path = request
        .workspace_path
        .as_deref()
        .expect("validated workspace scope");
    let open_root = resolve_open_workspace_root(&db.conn, workspace_path).await?;
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
    let current_epoch = match head_info.head_sha.clone() {
        Some(head) => CommitSourceEpoch {
            canonical_repo: canonical_repo.clone(),
            branch: head_info.branch.clone(),
            detached: head_info.detached,
            head,
        },
        None => CommitSourceEpoch {
            canonical_repo: canonical_repo.clone(),
            branch: head_info.branch.clone(),
            detached: false,
            head: CommitSourceEpoch::UNBORN_HEAD.to_string(),
        },
    };
    let current_opaque = head_info
        .reference_source_epoch
        .clone()
        .unwrap_or_else(|| current_epoch.opaque());

    let provided_epoch = request
        .source_epoch
        .as_deref()
        .expect("validated commit epoch scope");
    if provided_epoch != current_opaque {
        return Err(AppCommandError::new(
            AppErrorCode::SourceEpochChanged,
            "commit source epoch changed",
        ));
    }

    let (uri_repo, uri_hash) = parse_commit_uri(&request.uri)?;
    if uri_repo != current_epoch.canonical_repo {
        return Err(AppCommandError::new(
            AppErrorCode::InvalidRequest,
            "commit uri repository does not match workspace git root",
        ));
    }

    if current_epoch.is_unborn() {
        return Ok(None);
    }

    let current_head = current_epoch.head.as_str();
    if !is_valid_full_hash(uri_hash, current_head.len()) {
        return Err(AppCommandError::new(
            AppErrorCode::InvalidRequest,
            "commit uri hash is not a full object id for this repository",
        ));
    }

    // Existence: git cat-file -e <hash>^{commit}
    let object_spec = format!("{uri_hash}^{{commit}}");
    let cat = crate::process::tokio_command("git")
        .args(["cat-file", "-e", &object_spec])
        .current_dir(&open_root)
        .output()
        .await
        .map_err(|err| {
            AppCommandError::new(AppErrorCode::SourceFailed, "failed to spawn git cat-file")
                .with_detail(err.to_string())
        })?;
    if !cat.status.success() {
        // Missing / wrong type → not found (not a spawn failure).
        return Ok(None);
    }

    // Reachability from current HEAD.
    let merge = crate::process::tokio_command("git")
        .args(["merge-base", "--is-ancestor", uri_hash, current_head])
        .current_dir(&open_root)
        .output()
        .await
        .map_err(|err| {
            AppCommandError::new(AppErrorCode::SourceFailed, "failed to spawn git merge-base")
                .with_detail(err.to_string())
        })?;
    match merge.status.code() {
        Some(0) => {}
        Some(1) => return Ok(None),
        _ => {
            return Err(
                AppCommandError::new(AppErrorCode::SourceFailed, "git merge-base failed")
                    .with_detail(String::from_utf8_lossy(&merge.stderr).into_owned()),
            );
        }
    }

    let Some(fields) = read_commit_show_fields(&open_root, uri_hash).await? else {
        // Oversized or empty metadata cannot be published by search.
        return Ok(None);
    };

    Ok(Some(build_commit_candidate(
        &current_epoch.canonical_repo,
        fields,
        0,
    )))
}

fn is_valid_full_hash(hash: &str, expected_len: usize) -> bool {
    hash.len() == expected_len && hash.bytes().all(|b| b.is_ascii_hexdigit())
}

fn parse_session_uri(uri: &str) -> Result<i32, AppCommandError> {
    let rest = uri
        .strip_prefix("codeg://session/")
        .ok_or_else(|| invalid_request("conversation uri must use codeg://session/<id>"))?;
    if rest.is_empty() || !rest.bytes().all(|b| b.is_ascii_digit()) {
        return Err(invalid_request(
            "conversation uri id must be a positive numeric database id",
        ));
    }
    let id: i32 = rest
        .parse()
        .map_err(|_| invalid_request("conversation uri id is out of range"))?;
    if id <= 0 {
        return Err(invalid_request(
            "conversation uri id must be a positive numeric database id",
        ));
    }
    Ok(id)
}

fn parse_commit_uri(uri: &str) -> Result<(String, &str), AppCommandError> {
    let rest = uri
        .strip_prefix("codeg://commit/")
        .ok_or_else(|| invalid_request("commit uri must use codeg://commit/<repo>@<hash>"))?;
    let at = rest
        .rfind('@')
        .ok_or_else(|| invalid_request("commit uri missing @hash separator"))?;
    let (encoded_repo, hash) = rest.split_at(at);
    let hash = &hash[1..];
    if hash.is_empty() {
        return Err(invalid_request("commit uri hash is empty"));
    }
    let repo = percent_decode_component(encoded_repo)
        .map_err(|msg| invalid_request(format!("commit uri repository: {msg}")))?;
    Ok((repo, hash))
}

/// Parse a `file://` URI into a relative path under `root_display`, or `Ok(None)`
/// when the target path is outside / missing as not-found is handled by caller.
///
/// Returns `Ok(None)` only when the URI decodes cleanly to a path that is not
/// under the open root (treated as not found only after existence checks —
/// escapes are InvalidRequest). Actually: escapes after canonicalize are
/// InvalidRequest; URI that doesn't map under root is InvalidRequest.
fn parse_file_uri_relative(
    uri: &str,
    root_display: &str,
    open_root: &Path,
) -> Result<Option<String>, AppCommandError> {
    let path_part = decode_file_uri_path(uri)?;
    let root_norm = root_display.trim_end_matches('/');
    let path_norm = path_part.trim_end_matches('/');

    // Case-fold comparison on Windows for drive/root equality.
    let path_key = crate::parsers::normalize_path_for_matching(path_norm);
    let root_key = crate::parsers::normalize_path_for_matching(root_norm);

    if path_key == root_key {
        // Workspace root itself is not a selectable file candidate.
        return Ok(None);
    }
    let root_prefix = format!("{root_key}/");
    if !path_key.starts_with(&root_prefix) {
        return Err(invalid_request(
            "file uri is outside the open workspace root",
        ));
    }

    // Preserve original decoded relative segments from path_norm.
    let relative = path_norm
        .strip_prefix(root_norm)
        .or_else(|| {
            // Windows drive letter case differences.
            if path_norm.len() >= root_norm.len() {
                Some(&path_norm[root_norm.len()..])
            } else {
                None
            }
        })
        .ok_or_else(|| invalid_request("file uri is outside the open workspace root"))?
        .trim_start_matches('/');

    if relative.is_empty() {
        return Ok(None);
    }

    // Reject `.` / `..` and empty segments in the relative path.
    for segment in relative.split('/') {
        if segment.is_empty() || segment == "." || segment == ".." {
            return Err(invalid_request(
                "file uri path contains empty or parent segments",
            ));
        }
        if segment.contains('\\') {
            return Err(invalid_request(
                "file uri path segment contains a backslash",
            ));
        }
    }

    // Ensure open_root is used so unused param stays meaningful for callers.
    let _ = open_root;
    Ok(Some(relative.to_string()))
}

fn decode_file_uri_path(uri: &str) -> Result<String, AppCommandError> {
    let rest = uri
        .strip_prefix("file://")
        .ok_or_else(|| invalid_request("file uri must use the file:// scheme"))?;

    // Authority forms:
    // - `file:///C:/...` or `file:///home/...` → path starts with `/`
    // - `file://server/share/...` UNC → unsupported for validation
    if !rest.starts_with('/') {
        return Err(invalid_request(
            "file uri network authority is not supported",
        ));
    }

    // `file:///C%3A/repo` → path part after empty authority is `/C%3A/repo`
    // `file:///home/repo` → `/home/repo`
    let path_encoded = rest; // includes leading `/`

    let segments: Vec<&str> = path_encoded.split('/').collect();
    // First segment is empty because path starts with `/`.
    let mut decoded_segments = Vec::with_capacity(segments.len());
    for (i, segment) in segments.iter().enumerate() {
        if i == 0 {
            if !segment.is_empty() {
                return Err(invalid_request("malformed file uri path"));
            }
            continue;
        }
        if segment.is_empty() {
            // Allow only the leading empty from absolute path; internal empties reject.
            return Err(invalid_request("file uri contains an empty path segment"));
        }
        let decoded = percent_decode_component(segment)
            .map_err(|msg| invalid_request(format!("file uri segment: {msg}")))?;
        if decoded.contains('/') || decoded.contains('\\') || decoded.contains('\0') {
            return Err(invalid_request(
                "file uri segment decodes to a path separator or NUL",
            ));
        }
        if decoded == "." || decoded == ".." {
            return Err(invalid_request(
                "file uri path contains empty or parent segments",
            ));
        }
        // Reject Component equivalents just in case.
        let as_path = Path::new(&decoded);
        if as_path
            .components()
            .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
        {
            return Err(invalid_request(
                "file uri path contains empty or parent segments",
            ));
        }
        decoded_segments.push(decoded);
    }

    if decoded_segments.is_empty() {
        return Err(invalid_request("file uri path is empty"));
    }

    // Windows drive: first segment `C:` → `C:/rest`
    let mut path = String::new();
    if decoded_segments[0].len() == 2 {
        let b = decoded_segments[0].as_bytes();
        if b[0].is_ascii_alphabetic() && b[1] == b':' {
            path.push_str(&decoded_segments[0]);
            for seg in &decoded_segments[1..] {
                path.push('/');
                path.push_str(seg);
            }
            return Ok(path);
        }
    }

    // POSIX absolute
    path.push('/');
    path.push_str(&decoded_segments.join("/"));
    Ok(path)
}

fn percent_decode_component(input: &str) -> Result<String, String> {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' => {
                if i + 2 >= bytes.len() {
                    return Err("truncated percent escape".into());
                }
                let h1 =
                    from_hex(bytes[i + 1]).ok_or_else(|| "invalid percent escape".to_string())?;
                let h2 =
                    from_hex(bytes[i + 2]).ok_or_else(|| "invalid percent escape".to_string())?;
                out.push((h1 << 4) | h2);
                i += 3;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8(out).map_err(|_| "percent-decoded segment is not valid UTF-8".into())
}

fn from_hex(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

fn parse_agent_type(raw: &str) -> Option<AgentType> {
    serde_json::from_value(serde_json::Value::String(raw.to_string())).ok()
}

fn invalid_request(message: impl Into<String>) -> AppCommandError {
    AppCommandError::new(AppErrorCode::InvalidRequest, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::service::conversation_service;
    use crate::db::test_helpers::{fresh_in_memory_db, seed_folder};
    use crate::models::agent::AgentType;
    use crate::reference_search::matcher::build_commit_uri;
    use std::fs;
    use std::io::Write;
    use std::process::Command;
    use uuid::Uuid;

    pub struct ValidationFixture {
        pub db: AppDatabase,
        _temp: tempfile::TempDir,
        path: String,
        root_display: String,
        commit_uri: String,
        old_epoch: String,
        full_hash: String,
    }

    impl ValidationFixture {
        pub fn path(&self) -> &str {
            &self.path
        }

        pub async fn validate_file(
            &self,
            relative: &str,
            query: &str,
        ) -> Result<ReferenceCandidateValidation, AppCommandError> {
            let mut uri_path = PathBuf::from(&self.root_display);
            for part in relative.split('/') {
                if !part.is_empty() {
                    uri_path.push(part);
                }
            }
            let uri = build_file_uri(&uri_path);
            validate_reference_candidate_core(
                &self.db,
                ValidateReferenceCandidateRequest {
                    validation_request_id: Uuid::new_v4().hyphenated().to_string(),
                    source: ReferenceSearchSource::File,
                    uri,
                    query: query.to_string(),
                    workspace_path: Some(self.path.clone()),
                    source_epoch: None,
                },
            )
            .await
        }

        pub async fn validate_old_commit_epoch(
            &self,
        ) -> Result<ReferenceCandidateValidation, AppCommandError> {
            validate_reference_candidate_core(
                &self.db,
                ValidateReferenceCandidateRequest {
                    validation_request_id: Uuid::new_v4().hyphenated().to_string(),
                    source: ReferenceSearchSource::Commit,
                    uri: self.commit_uri.clone(),
                    query: "commit".to_string(),
                    workspace_path: Some(self.path.clone()),
                    source_epoch: Some(self.old_epoch.clone()),
                },
            )
            .await
        }

        pub fn commit(&self, message: &str) {
            git_run(
                Path::new(&self.path),
                &["commit", "-q", "--allow-empty", "-m", message],
            );
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

    pub async fn validation_fixture() -> ValidationFixture {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().to_string_lossy().to_string();
        let app_ts = temp.path().join("src").join("app.ts");
        fs::create_dir_all(app_ts.parent().unwrap()).expect("mkdir");
        let mut f = fs::File::create(&app_ts).expect("create");
        f.write_all(b"export const app = 1;\n").expect("write");

        git_run(temp.path(), &["init", "-q"]);
        git_run(
            temp.path(),
            &["commit", "-q", "--allow-empty", "-m", "commit initial"],
        );
        let hash_out = Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(temp.path())
            .output()
            .expect("rev-parse");
        let full_hash = String::from_utf8_lossy(&hash_out.stdout).trim().to_string();

        let db = fresh_in_memory_db().await;
        let folder_id = seed_folder(&db, &path).await;
        conversation_service::create(
            &db.conn,
            folder_id,
            AgentType::ClaudeCode,
            Some("root conversation".to_string()),
            Some("main".to_string()),
        )
        .await
        .expect("conversation");

        let open_root = resolve_open_workspace_root(&db.conn, &path)
            .await
            .expect("open root");
        let root_display = normalize_path_for_uri(&open_root);
        let head = resolve_git_head(&path).await.expect("head");
        let old_epoch = head.reference_source_epoch.expect("epoch");
        let canonical_repo = head.canonical_repo.expect("repo");
        let commit_uri = build_commit_uri(&canonical_repo, &full_hash);

        ValidationFixture {
            db,
            _temp: temp,
            path,
            root_display,
            commit_uri,
            old_epoch,
            full_hash,
        }
    }

    #[tokio::test]
    async fn validation_distinguishes_match_not_match_not_found_and_epoch_change() {
        let fixture = validation_fixture().await;
        assert!(matches!(
            fixture.validate_file("src/app.ts", "app").await.unwrap(),
            ReferenceCandidateValidation::Match { .. }
        ));
        assert!(matches!(
            fixture.validate_file("src/app.ts", "readme").await.unwrap(),
            ReferenceCandidateValidation::NotMatch { .. }
        ));
        assert!(matches!(
            fixture
                .validate_file("missing.ts", "missing")
                .await
                .unwrap(),
            ReferenceCandidateValidation::NotFound { .. }
        ));
        fixture.commit("new head");
        let error = fixture
            .validate_old_commit_epoch()
            .await
            .expect_err("epoch");
        assert!(matches!(error.code, AppErrorCode::SourceEpochChanged));
    }

    #[tokio::test]
    async fn file_validation_rejects_parent_segment_escape() {
        let fixture = validation_fixture().await;
        let mut uri_path = PathBuf::from(&fixture.root_display);
        uri_path.push("..");
        uri_path.push("secret.txt");
        // Build a file URI that path-encodes `..` as a segment.
        let uri = format!(
            "file://{}/../secret.txt",
            // Use the same encoding as build_file_uri for the root, then append.
            build_file_uri(Path::new(&fixture.root_display)).trim_start_matches("file://")
        );
        // The constructed URI may still be parsed; ensure parent segments error.
        let result = validate_reference_candidate_core(
            &fixture.db,
            ValidateReferenceCandidateRequest {
                validation_request_id: Uuid::new_v4().hyphenated().to_string(),
                source: ReferenceSearchSource::File,
                uri,
                query: "secret".to_string(),
                workspace_path: Some(fixture.path.clone()),
                source_epoch: None,
            },
        )
        .await;
        assert!(
            matches!(
                result.as_ref().map_err(|e| e.code),
                Err(AppErrorCode::InvalidRequest)
            ),
            "expected InvalidRequest, got {result:?}"
        );
        let _ = uri_path;
    }

    #[tokio::test]
    async fn commit_validation_matches_live_reachable_commit() {
        let fixture = validation_fixture().await;
        let head = resolve_git_head(fixture.path()).await.expect("head");
        let result = validate_reference_candidate_core(
            &fixture.db,
            ValidateReferenceCandidateRequest {
                validation_request_id: Uuid::new_v4().hyphenated().to_string(),
                source: ReferenceSearchSource::Commit,
                uri: fixture.commit_uri.clone(),
                query: "commit".to_string(),
                workspace_path: Some(fixture.path.clone()),
                source_epoch: head.reference_source_epoch,
            },
        )
        .await
        .expect("validate");
        assert!(matches!(
            result,
            ReferenceCandidateValidation::Match {
                candidate,
                ..
            } if candidate.id == fixture.full_hash
        ));
    }
}
