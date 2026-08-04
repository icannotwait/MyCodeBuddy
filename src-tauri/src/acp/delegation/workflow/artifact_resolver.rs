//! Platform-owned document and clean Git artifact resolution.

use std::io::Read;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use super::completion_intent::CompletionOutcome;
use super::key::normalize_rel_path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactKind {
    DocumentSha256,
    GitHeadV1,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocumentSha256Artifact {
    rel_path: String,
    digest: String,
}

impl DocumentSha256Artifact {
    pub fn rel_path(&self) -> &str {
        &self.rel_path
    }

    pub fn digest(&self) -> &str {
        &self.digest
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitHeadV1Artifact {
    pub head: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "artifact", rename_all = "snake_case")]
pub enum ResolvedArtifact {
    DocumentSha256(DocumentSha256Artifact),
    GitHeadV1(GitHeadV1Artifact),
}

impl ResolvedArtifact {
    pub const fn kind(&self) -> ArtifactKind {
        match self {
            Self::DocumentSha256(_) => ArtifactKind::DocumentSha256,
            Self::GitHeadV1(_) => ArtifactKind::GitHeadV1,
        }
    }

    pub fn digest(&self) -> &str {
        match self {
            Self::DocumentSha256(artifact) => artifact.digest(),
            Self::GitHeadV1(artifact) => &artifact.head,
        }
    }
}

impl From<DocumentSha256Artifact> for ResolvedArtifact {
    fn from(value: DocumentSha256Artifact) -> Self {
        Self::DocumentSha256(value)
    }
}

impl From<GitHeadV1Artifact> for ResolvedArtifact {
    fn from(value: GitHeadV1Artifact) -> Self {
        Self::GitHeadV1(value)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactFailure {
    InvalidPath,
    WorkspaceUnavailable,
    WorkspaceEscape,
    NotFile,
    SizeLimitExceeded,
    ReadFailed,
    GitCommandFailed,
    MissingHead,
    MalformedHead,
    DirtyWorktree,
    CommitRequired,
    ExpectedArtifactInvalid,
}

#[derive(Debug, Clone, PartialEq, Eq, Error, Serialize, Deserialize)]
pub enum ArtifactError {
    #[error("completion artifact unavailable: {0:?}")]
    Unavailable(ArtifactFailure),
    #[error("completion scope changed")]
    ScopeChanged { expected: String, actual: String },
    #[error("final artifact drift")]
    FinalArtifactDrift { expected: String, actual: String },
}

impl ArtifactError {
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Unavailable(_) => "completion_artifact_unavailable",
            Self::ScopeChanged { .. } => "completion_scope_changed",
            Self::FinalArtifactDrift { .. } => "final_artifact_drift",
        }
    }

    pub const fn failure(&self) -> Option<ArtifactFailure> {
        match self {
            Self::Unavailable(failure) => Some(*failure),
            Self::ScopeChanged { .. } | Self::FinalArtifactDrift { .. } => None,
        }
    }
}

pub async fn resolve_document(
    workspace: &Path,
    rel_path: &str,
    max_bytes: usize,
) -> Result<DocumentSha256Artifact, ArtifactError> {
    if has_uri_scheme(rel_path) {
        return Err(ArtifactError::Unavailable(ArtifactFailure::InvalidPath));
    }
    let rel_path = normalize_rel_path(rel_path)
        .map_err(|_| ArtifactError::Unavailable(ArtifactFailure::InvalidPath))?;
    let workspace = workspace.to_path_buf();
    let read_path = rel_path.clone();
    let bytes = tokio::task::spawn_blocking(move || {
        read_bounded_workspace_file(&workspace, &read_path, max_bytes)
    })
    .await
    .map_err(|_| ArtifactError::Unavailable(ArtifactFailure::ReadFailed))??;
    let digest = format!("sha256:{:x}", Sha256::digest(&bytes));
    Ok(DocumentSha256Artifact { rel_path, digest })
}

fn has_uri_scheme(path: &str) -> bool {
    let Some((scheme, _)) = path.split_once(':') else {
        return false;
    };
    let mut chars = scheme.chars();
    matches!(chars.next(), Some(first) if first.is_ascii_alphabetic())
        && chars.all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '+' | '-' | '.'))
}

fn read_bounded_workspace_file(
    workspace: &Path,
    rel_path: &str,
    max_bytes: usize,
) -> Result<Vec<u8>, ArtifactError> {
    let canonical_workspace = workspace
        .canonicalize()
        .map_err(|_| ArtifactError::Unavailable(ArtifactFailure::WorkspaceUnavailable))?;
    if !canonical_workspace.is_dir() {
        return Err(ArtifactError::Unavailable(
            ArtifactFailure::WorkspaceUnavailable,
        ));
    }

    let joined = canonical_workspace.join(PathBuf::from(rel_path));
    let canonical_file = joined
        .canonicalize()
        .map_err(|_| ArtifactError::Unavailable(ArtifactFailure::ReadFailed))?;
    if !canonical_file.starts_with(&canonical_workspace) {
        return Err(ArtifactError::Unavailable(ArtifactFailure::WorkspaceEscape));
    }
    if !canonical_file
        .metadata()
        .map_err(|_| ArtifactError::Unavailable(ArtifactFailure::ReadFailed))?
        .is_file()
    {
        return Err(ArtifactError::Unavailable(ArtifactFailure::NotFile));
    }

    let read_limit = max_bytes.checked_add(1).ok_or(ArtifactError::Unavailable(
        ArtifactFailure::SizeLimitExceeded,
    ))?;
    let mut bytes = Vec::with_capacity(read_limit.min(64 * 1024));
    std::fs::File::open(&canonical_file)
        .map_err(|_| ArtifactError::Unavailable(ArtifactFailure::ReadFailed))?
        .take(read_limit as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| ArtifactError::Unavailable(ArtifactFailure::ReadFailed))?;
    if bytes.len() > max_bytes {
        return Err(ArtifactError::Unavailable(
            ArtifactFailure::SizeLimitExceeded,
        ));
    }
    Ok(bytes)
}

pub async fn resolve_git_head_clean(workspace: &Path) -> Result<GitHeadV1Artifact, ArtifactError> {
    let head_output = run_git(workspace, &["rev-parse", "HEAD"]).await?;
    let head = normalize_git_head(&head_output)?;
    let porcelain = run_git(
        workspace,
        &["status", "--porcelain=v1", "--untracked-files=all"],
    )
    .await?;
    if !porcelain.is_empty() {
        return Err(ArtifactError::Unavailable(ArtifactFailure::DirtyWorktree));
    }
    Ok(GitHeadV1Artifact { head })
}

async fn run_git(workspace: &Path, args: &[&str]) -> Result<Vec<u8>, ArtifactError> {
    if workspace.as_os_str().is_empty() {
        return Err(ArtifactError::Unavailable(
            ArtifactFailure::WorkspaceUnavailable,
        ));
    }
    let output = tokio::process::Command::new("git")
        .args(args)
        .current_dir(workspace)
        .kill_on_drop(true)
        .output()
        .await
        .map_err(|_| ArtifactError::Unavailable(ArtifactFailure::GitCommandFailed))?;
    if !output.status.success() {
        return Err(ArtifactError::Unavailable(
            ArtifactFailure::GitCommandFailed,
        ));
    }
    Ok(output.stdout)
}

fn normalize_git_head(stdout: &[u8]) -> Result<String, ArtifactError> {
    let without_newline = stdout
        .strip_suffix(b"\r\n")
        .or_else(|| stdout.strip_suffix(b"\n"))
        .unwrap_or(stdout);
    if without_newline.is_empty() {
        return Err(ArtifactError::Unavailable(ArtifactFailure::MissingHead));
    }
    if !matches!(without_newline.len(), 40 | 64)
        || !without_newline.iter().all(u8::is_ascii_hexdigit)
    {
        return Err(ArtifactError::Unavailable(ArtifactFailure::MalformedHead));
    }
    let head = std::str::from_utf8(without_newline)
        .map_err(|_| ArtifactError::Unavailable(ArtifactFailure::MalformedHead))?
        .to_ascii_lowercase();
    Ok(head)
}

fn normalize_expected_head(expected: &str) -> Result<String, ArtifactError> {
    let bytes = expected.as_bytes();
    if !matches!(bytes.len(), 40 | 64) || !bytes.iter().all(u8::is_ascii_hexdigit) {
        return Err(ArtifactError::Unavailable(
            ArtifactFailure::ExpectedArtifactInvalid,
        ));
    }
    Ok(expected.to_ascii_lowercase())
}

pub async fn resolve_producer_completion(
    workspace: &Path,
    outcome: CompletionOutcome,
    producer_baseline_head: &str,
    allow_noop_verification: bool,
) -> Result<Option<ResolvedArtifact>, ArtifactError> {
    if !matches!(
        outcome,
        CompletionOutcome::Done | CompletionOutcome::DoneWithConcerns
    ) {
        return Ok(None);
    }

    let baseline = normalize_expected_head(producer_baseline_head)?;
    let current = resolve_git_head_clean(workspace).await?;
    if current.head == baseline && !allow_noop_verification {
        return Err(ArtifactError::Unavailable(ArtifactFailure::CommitRequired));
    }
    Ok(Some(current.into()))
}

pub async fn resolve_reviewer_completion(
    workspace: &Path,
    expected_producer_head: &str,
) -> Result<ResolvedArtifact, ArtifactError> {
    let expected = normalize_expected_head(expected_producer_head)?;
    let current = resolve_git_head_clean(workspace).await?;
    if current.head != expected {
        return Err(ArtifactError::ScopeChanged {
            expected,
            actual: current.head,
        });
    }
    Ok(current.into())
}

pub async fn resolve_final_delivery(
    workspace: &Path,
    expected_final_head: &str,
) -> Result<ResolvedArtifact, ArtifactError> {
    let expected = normalize_expected_head(expected_final_head)?;
    let current = resolve_git_head_clean(workspace).await?;
    if current.head != expected {
        return Err(ArtifactError::FinalArtifactDrift {
            expected,
            actual: current.head,
        });
    }
    Ok(current.into())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;
    use std::process::Command;

    use sha2::{Digest, Sha256};
    use tempfile::TempDir;

    use super::{
        resolve_document, resolve_final_delivery, resolve_git_head_clean,
        resolve_producer_completion, resolve_reviewer_completion, ArtifactFailure,
        ResolvedArtifact,
    };
    use crate::acp::delegation::workflow::CompletionOutcome;

    fn sha256_token(bytes: &[u8]) -> String {
        format!("sha256:{:x}", Sha256::digest(bytes))
    }

    fn git(repo: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .args(args)
            .current_dir(repo)
            .output()
            .expect("run git fixture command");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout)
            .expect("git fixture output is UTF-8")
            .trim()
            .to_string()
    }

    struct GitFixture {
        dir: TempDir,
    }

    impl GitFixture {
        fn new() -> Self {
            let dir = tempfile::tempdir().expect("temp repo");
            git(dir.path(), &["init", "--quiet"]);
            fs::write(dir.path().join("owned.txt"), b"baseline\n").expect("write baseline");
            git(dir.path(), &["add", "owned.txt"]);
            git(
                dir.path(),
                &[
                    "-c",
                    "user.name=Codeg Test",
                    "-c",
                    "user.email=codeg@example.invalid",
                    "commit",
                    "--quiet",
                    "-m",
                    "baseline",
                ],
            );
            Self { dir }
        }

        fn path(&self) -> &Path {
            self.dir.path()
        }

        fn head(&self) -> String {
            git(self.path(), &["rev-parse", "HEAD"])
        }

        fn commit_owned_change(&self) {
            fs::write(self.path().join("owned.txt"), b"changed\n").expect("write change");
            git(self.path(), &["add", "owned.txt"]);
            git(
                self.path(),
                &[
                    "-c",
                    "user.name=Codeg Test",
                    "-c",
                    "user.email=codeg@example.invalid",
                    "commit",
                    "--quiet",
                    "-m",
                    "owned change",
                ],
            );
        }
    }

    #[tokio::test]
    async fn document_resolver_hashes_exact_bounded_workspace_bytes() {
        let workspace = tempfile::tempdir().expect("temp workspace");
        fs::create_dir_all(workspace.path().join("docs")).expect("create docs");
        fs::write(workspace.path().join("docs/plan.md"), b"# Plan\r\n").expect("write plan");

        let artifact = resolve_document(workspace.path(), "docs/plan.md", 2 * 1024 * 1024)
            .await
            .expect("resolve document");
        assert_eq!(artifact.rel_path(), "docs/plan.md");
        assert_eq!(artifact.digest(), sha256_token(b"# Plan\r\n"));

        for rejected in ["../plan.md", "/tmp/plan.md", "file:///tmp/plan.md"] {
            assert_eq!(
                resolve_document(workspace.path(), rejected, 1024)
                    .await
                    .expect_err("unsafe path must fail")
                    .code(),
                "completion_artifact_unavailable"
            );
        }
        assert_eq!(
            resolve_document(workspace.path(), "docs/plan.md", 7)
                .await
                .expect_err("oversized document must fail")
                .code(),
            "completion_artifact_unavailable"
        );
    }

    #[tokio::test]
    async fn document_resolver_rejects_uri_scheme_as_invalid_path() {
        let workspace = tempfile::tempdir().expect("temp workspace");
        #[cfg(unix)]
        {
            fs::create_dir_all(workspace.path().join("https:"))
                .expect("create scheme-shaped directory");
            fs::write(workspace.path().join("https:/artifact"), b"must not hash\n")
                .expect("write scheme-shaped artifact");
        }

        assert_eq!(
            resolve_document(workspace.path(), "https://artifact", 1024)
                .await
                .expect_err("URI scheme must fail before filesystem resolution")
                .failure(),
            Some(ArtifactFailure::InvalidPath)
        );
    }

    #[tokio::test]
    async fn git_resolver_requires_head_and_completely_empty_porcelain() {
        let clean = GitFixture::new();
        assert_eq!(
            resolve_git_head_clean(clean.path())
                .await
                .expect("clean repository")
                .head,
            clean.head()
        );

        let tracked = GitFixture::new();
        fs::write(tracked.path().join("owned.txt"), b"dirty\n").expect("tracked dirt");
        assert_eq!(
            resolve_git_head_clean(tracked.path())
                .await
                .expect_err("tracked dirt must fail")
                .code(),
            "completion_artifact_unavailable"
        );

        let staged = GitFixture::new();
        fs::write(staged.path().join("owned.txt"), b"staged\n").expect("staged dirt");
        git(staged.path(), &["add", "owned.txt"]);
        assert_eq!(
            resolve_git_head_clean(staged.path())
                .await
                .expect_err("staged dirt must fail")
                .code(),
            "completion_artifact_unavailable"
        );

        let untracked = GitFixture::new();
        fs::write(untracked.path().join("untracked.txt"), b"untracked\n").expect("untracked dirt");
        assert_eq!(
            resolve_git_head_clean(untracked.path())
                .await
                .expect_err("untracked dirt must fail")
                .code(),
            "completion_artifact_unavailable"
        );

        let non_repo = tempfile::tempdir().expect("non-repository workspace");
        assert_eq!(
            resolve_git_head_clean(non_repo.path())
                .await
                .expect_err("missing HEAD must fail")
                .code(),
            "completion_artifact_unavailable"
        );
    }

    #[tokio::test]
    async fn git_resolver_forces_untracked_visibility_over_repo_config() {
        let repo = GitFixture::new();
        git(
            repo.path(),
            &["config", "--local", "status.showUntrackedFiles", "no"],
        );
        fs::write(repo.path().join("hidden-untracked.txt"), b"still dirty\n")
            .expect("write hidden untracked dirt");

        assert_eq!(
            resolve_git_head_clean(repo.path())
                .await
                .expect_err("repository config must not hide untracked dirt")
                .failure(),
            Some(ArtifactFailure::DirtyWorktree)
        );
    }

    #[tokio::test]
    async fn completion_artifact_contract_producer_requires_commit_or_durable_noop() {
        let repo = GitFixture::new();
        let baseline = repo.head();

        assert_eq!(
            resolve_producer_completion(repo.path(), CompletionOutcome::Done, &baseline, false,)
                .await
                .expect_err("same HEAD without durable no-op must fail")
                .code(),
            "completion_artifact_unavailable"
        );
        assert!(
            resolve_producer_completion(repo.path(), CompletionOutcome::Done, &baseline, true,)
                .await
                .is_ok()
        );

        repo.commit_owned_change();
        let artifact = resolve_producer_completion(
            repo.path(),
            CompletionOutcome::DoneWithConcerns,
            &baseline,
            false,
        )
        .await
        .expect("new clean commit satisfies producer contract")
        .expect("passing producer records an artifact");
        assert_eq!(artifact.digest(), repo.head());

        fs::write(repo.path().join("untracked.txt"), b"still blocked\n")
            .expect("dirty non-pass workspace");
        assert_eq!(
            resolve_producer_completion(repo.path(), CompletionOutcome::Blocked, &baseline, false,)
                .await
                .expect("non-pass bypasses artifact requirements"),
            None
        );
    }

    #[tokio::test]
    async fn completion_artifact_contract_reviewer_and_delivery_freeze_exact_clean_commit() {
        let repo = GitFixture::new();
        let producer_head = repo.head();
        assert!(matches!(
            resolve_reviewer_completion(repo.path(), &producer_head)
                .await
                .expect("reviewer admits on producer HEAD"),
            ResolvedArtifact::GitHeadV1(_)
        ));

        repo.commit_owned_change();
        assert_eq!(
            resolve_reviewer_completion(repo.path(), &producer_head)
                .await
                .expect_err("reviewer commit drift must fail")
                .code(),
            "completion_scope_changed"
        );
        assert_eq!(
            resolve_final_delivery(repo.path(), &producer_head)
                .await
                .expect_err("post-Final commit drift must block delivery")
                .code(),
            "final_artifact_drift"
        );

        let dirty = GitFixture::new();
        let expected = dirty.head();
        fs::write(dirty.path().join("untracked.txt"), b"dirty\n").expect("untracked dirt");
        assert_eq!(
            resolve_reviewer_completion(dirty.path(), &expected)
                .await
                .expect_err("reviewer dirt must fail")
                .code(),
            "completion_artifact_unavailable"
        );
    }
}
