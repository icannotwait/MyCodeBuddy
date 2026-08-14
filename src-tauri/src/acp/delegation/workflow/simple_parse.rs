//! Bounded, non-authoritative parsing for Simple Plan and progress documents.

use std::collections::BTreeSet;
use std::path::Path;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::artifact_resolver::{read_bounded_workspace_file, ArtifactError, ArtifactFailure};
use super::key::normalize_rel_path;
use super::plan_material::{
    collect_headings_and_front_matter, normalize_source_for_parsing, MAX_PLAN_MATERIAL_BYTES,
    MAX_PLAN_SECTION_BYTES,
};

pub const MAX_SIMPLE_PROGRESS_BYTES: usize = 512 * 1024;
pub const MAX_SIMPLE_PROGRESS_BLOCK_BYTES: usize = 64 * 1024;
pub const MAX_SIMPLE_PROJECTION_WARNINGS: usize = 64;
const PROGRESS_MARKER: &str = "<!-- codeg-simple-progress-v1";
const COMMENT_END: &str = "-->";

pub const WARNING_PLAN_DUPLICATE_TASK: &str = "simple_plan_duplicate_task_index";
pub const WARNING_PLAN_MALFORMED_TASK: &str = "simple_plan_malformed_task_heading";
pub const WARNING_PLAN_NON_CONTIGUOUS: &str = "simple_plan_non_contiguous_tasks";
pub const WARNING_PLAN_SECTION_TRUNCATED: &str = "simple_plan_section_truncated";
pub const WARNING_PROGRESS_MISSING: &str = "simple_progress_block_missing";
pub const WARNING_PROGRESS_MULTIPLE: &str = "simple_progress_multiple_blocks";
pub const WARNING_PROGRESS_TRUNCATED: &str = "simple_progress_block_truncated";
pub const WARNING_PROGRESS_TOO_LARGE: &str = "simple_progress_block_too_large";
pub const WARNING_PROGRESS_INVALID_JSON: &str = "simple_progress_invalid_json";
pub const WARNING_PROGRESS_SCHEMA: &str = "simple_progress_schema_unsupported";
pub const WARNING_PROGRESS_DUPLICATE_TASK: &str = "simple_progress_duplicate_task_index";
pub const WARNING_PROGRESS_UNKNOWN_STATUS: &str = "simple_progress_unknown_task_status";
pub const WARNING_PROGRESS_UNKNOWN_RUN_STATE: &str = "simple_progress_unknown_run_state";
pub const WARNING_PROGRESS_PLAN_PATH: &str = "simple_progress_plan_path_mismatch";

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum SimpleParseError {
    #[error("Simple document path is invalid")]
    InvalidPath,
    #[error("Simple document is not valid UTF-8")]
    InvalidUtf8,
    #[error("Simple document exceeds its byte limit")]
    SizeLimitExceeded,
    #[error("Simple document is unavailable: {0:?}")]
    Unavailable(ArtifactFailure),
}

impl From<ArtifactError> for SimpleParseError {
    fn from(value: ArtifactError) -> Self {
        match value {
            ArtifactError::Unavailable(ArtifactFailure::SizeLimitExceeded) => {
                Self::SizeLimitExceeded
            }
            ArtifactError::Unavailable(failure) => Self::Unavailable(failure),
            ArtifactError::ScopeChanged { .. } | ArtifactError::FinalArtifactDrift { .. } => {
                Self::Unavailable(ArtifactFailure::ReadFailed)
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SimplePlanTask {
    pub index: u32,
    pub title: String,
    pub body: String,
    pub declared_files: Vec<String>,
    pub verification_text: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct SimplePlanDocument {
    pub tasks: Vec<SimplePlanTask>,
    pub warning_codes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SimpleDeclaredStatus {
    Pending,
    InProgress,
    Completed,
    Blocked,
    Unknown(String),
}

impl SimpleDeclaredStatus {
    fn parse(value: &str) -> Self {
        match value {
            "pending" => Self::Pending,
            "in_progress" => Self::InProgress,
            "completed" => Self::Completed,
            "blocked" => Self::Blocked,
            other => Self::Unknown(other.to_string()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SimpleFinalReviewStatus {
    Pending,
    InProgress,
    Completed,
    Blocked,
    Unknown(String),
}

impl SimpleFinalReviewStatus {
    fn parse(value: &str) -> Self {
        match value {
            "pending" => Self::Pending,
            "in_progress" => Self::InProgress,
            "completed" => Self::Completed,
            "blocked" => Self::Blocked,
            other => Self::Unknown(other.to_string()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SimpleProgressRun {
    pub role: String,
    pub agent_type: String,
    pub profile_id: Option<String>,
    pub task_id: Option<String>,
    pub child_conversation_id: Option<i32>,
    pub state: String,
    pub work_unit_key: Option<String>,
    pub recovery_count: Option<u32>,
    pub replaced_task_id: Option<String>,
    pub replacement_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SimpleProgressTask {
    pub index: u32,
    pub status: SimpleDeclaredStatus,
    pub commit: Option<String>,
    pub runs: Vec<SimpleProgressRun>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SimpleProgressSnapshot {
    pub schema_version: u32,
    pub plan_rel_path: String,
    pub active_task_index: Option<u32>,
    pub tasks: Vec<SimpleProgressTask>,
    pub final_review_status: SimpleFinalReviewStatus,
    pub updated_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct SimpleProgressDocument {
    pub snapshot: Option<SimpleProgressSnapshot>,
    pub warning_codes: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct RawProgressRun {
    #[serde(default)]
    role: String,
    #[serde(default)]
    agent_type: String,
    #[serde(default)]
    profile_id: Option<String>,
    #[serde(default)]
    task_id: Option<String>,
    #[serde(default)]
    child_conversation_id: Option<i32>,
    #[serde(default)]
    state: String,
    #[serde(default)]
    work_unit_key: Option<String>,
    #[serde(default)]
    recovery_count: Option<u32>,
    #[serde(default)]
    replaced_task_id: Option<String>,
    #[serde(default)]
    replacement_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawProgressTask {
    index: u32,
    #[serde(default = "default_pending")]
    status: String,
    #[serde(default)]
    commit: Option<String>,
    #[serde(default)]
    runs: Vec<RawProgressRun>,
}

#[derive(Debug, Deserialize)]
struct RawProgressSnapshot {
    schema_version: u32,
    #[serde(default)]
    plan_rel_path: String,
    #[serde(default)]
    active_task_index: Option<u32>,
    #[serde(default)]
    tasks: Vec<RawProgressTask>,
    #[serde(default = "default_pending")]
    final_review_status: String,
    #[serde(default)]
    updated_at: Option<String>,
}

fn default_pending() -> String {
    "pending".into()
}

fn push_warning(warnings: &mut Vec<String>, code: &str) {
    if warnings.len() < MAX_SIMPLE_PROJECTION_WARNINGS
        && !warnings.iter().any(|existing| existing == code)
    {
        warnings.push(code.to_string());
    }
}

fn task_heading(text: &str) -> Result<Option<(u32, String)>, ()> {
    let Some(rest) = text.strip_prefix("Task ") else {
        return Ok(None);
    };
    let Some((index, title)) = rest.split_once(':') else {
        return Err(());
    };
    if index.is_empty()
        || index.starts_with('0')
        || !index.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(());
    }
    let index = index.parse::<u32>().map_err(|_| ())?;
    let title = title.trim();
    if index == 0 || title.is_empty() {
        return Err(());
    }
    Ok(Some((index, title.to_string())))
}

fn truncate_utf8(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_string();
    }
    let mut end = max_bytes;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_string()
}

fn declared_files(body: &str) -> Vec<String> {
    let mut files = Vec::new();
    let mut seen = BTreeSet::new();
    for line in body.lines() {
        let trimmed = line.trim_start();
        if !trimmed.starts_with('-') {
            continue;
        }
        let Some(start) = trimmed.find('`') else {
            continue;
        };
        let Some(relative_end) = trimmed[start + 1..].find('`') else {
            continue;
        };
        let candidate = &trimmed[start + 1..start + 1 + relative_end];
        if let Ok(path) = normalize_rel_path(candidate) {
            if seen.insert(path.clone()) {
                files.push(path);
            }
        }
    }
    files
}

fn verification_text(body: &str) -> Option<String> {
    let lines = body.lines().collect::<Vec<_>>();
    let start = lines.iter().position(|line| {
        let lower = line.to_ascii_lowercase();
        lower.contains("run") && (lower.contains("verify") || lower.contains("test"))
    })?;
    let text = lines[start..].join("\n").trim().to_string();
    (!text.is_empty()).then_some(truncate_utf8(&text, 64 * 1024))
}

pub fn parse_simple_plan(bytes: &[u8]) -> Result<SimplePlanDocument, SimpleParseError> {
    if bytes.len() > MAX_PLAN_MATERIAL_BYTES {
        return Err(SimpleParseError::SizeLimitExceeded);
    }
    let decoded = std::str::from_utf8(bytes).map_err(|_| SimpleParseError::InvalidUtf8)?;
    let source = normalize_source_for_parsing(decoded);
    let (headings, _) = collect_headings_and_front_matter(&source);
    let mut warnings = Vec::new();
    let mut candidates = Vec::new();
    for heading in &headings {
        if !matches!(heading.level, 2 | 3) || !heading.is_atx {
            continue;
        }
        match task_heading(&heading.text) {
            Ok(Some((index, title))) => candidates.push((index, title, heading)),
            Ok(None) => {}
            Err(()) => push_warning(&mut warnings, WARNING_PLAN_MALFORMED_TASK),
        }
    }

    let mut tasks = Vec::new();
    let mut seen = BTreeSet::new();
    for (candidate_offset, (index, title, heading)) in candidates.iter().enumerate() {
        if !seen.insert(*index) {
            push_warning(&mut warnings, WARNING_PLAN_DUPLICATE_TASK);
            continue;
        }
        // Task headings delimit one another even when a document mixes H2 and H3.
        // Non-task subheadings remain part of their owning task's bounded body.
        let end = candidates
            .get(candidate_offset + 1)
            .map(|(_, _, candidate)| candidate.line_start)
            .unwrap_or(source.len());
        let raw_body = &source[heading.body_start..end];
        let body = if raw_body.len() > MAX_PLAN_SECTION_BYTES {
            push_warning(&mut warnings, WARNING_PLAN_SECTION_TRUNCATED);
            truncate_utf8(raw_body, MAX_PLAN_SECTION_BYTES)
        } else {
            raw_body.to_string()
        };
        tasks.push(SimplePlanTask {
            index: *index,
            title: title.clone(),
            declared_files: declared_files(&body),
            verification_text: verification_text(&body),
            body,
        });
    }
    if tasks
        .iter()
        .enumerate()
        .any(|(offset, task)| task.index as usize != offset + 1)
    {
        push_warning(&mut warnings, WARNING_PLAN_NON_CONTIGUOUS);
    }
    Ok(SimplePlanDocument {
        tasks,
        warning_codes: warnings,
    })
}

fn known_run_state(state: &str) -> bool {
    matches!(
        state,
        "reserving"
            | "running"
            | "completed"
            | "failed"
            | "canceled"
            | "cancelled"
            | "stalled"
            | "unknown"
    )
}

pub fn parse_simple_progress(
    bytes: &[u8],
    expected_plan_rel_path: &str,
) -> Result<SimpleProgressDocument, SimpleParseError> {
    if bytes.len() > MAX_SIMPLE_PROGRESS_BYTES {
        return Err(SimpleParseError::SizeLimitExceeded);
    }
    let source = std::str::from_utf8(bytes).map_err(|_| SimpleParseError::InvalidUtf8)?;
    let mut warnings = Vec::new();
    let starts = source
        .match_indices(PROGRESS_MARKER)
        .map(|(offset, _)| offset)
        .collect::<Vec<_>>();
    let Some(start) = starts.first().copied() else {
        push_warning(&mut warnings, WARNING_PROGRESS_MISSING);
        return Ok(SimpleProgressDocument {
            snapshot: None,
            warning_codes: warnings,
        });
    };
    if starts.len() > 1 {
        push_warning(&mut warnings, WARNING_PROGRESS_MULTIPLE);
    }
    let json_start = start + PROGRESS_MARKER.len();
    let Some(relative_end) = source[json_start..].find(COMMENT_END) else {
        push_warning(&mut warnings, WARNING_PROGRESS_TRUNCATED);
        return Ok(SimpleProgressDocument {
            snapshot: None,
            warning_codes: warnings,
        });
    };
    let json = source[json_start..json_start + relative_end].trim();
    if json.len() > MAX_SIMPLE_PROGRESS_BLOCK_BYTES {
        push_warning(&mut warnings, WARNING_PROGRESS_TOO_LARGE);
        return Ok(SimpleProgressDocument {
            snapshot: None,
            warning_codes: warnings,
        });
    }
    let raw: RawProgressSnapshot = match serde_json::from_str(json) {
        Ok(raw) => raw,
        Err(_) => {
            push_warning(&mut warnings, WARNING_PROGRESS_INVALID_JSON);
            return Ok(SimpleProgressDocument {
                snapshot: None,
                warning_codes: warnings,
            });
        }
    };
    if raw.schema_version != 1 {
        push_warning(&mut warnings, WARNING_PROGRESS_SCHEMA);
        return Ok(SimpleProgressDocument {
            snapshot: None,
            warning_codes: warnings,
        });
    }
    let expected_plan_rel_path =
        normalize_rel_path(expected_plan_rel_path).map_err(|_| SimpleParseError::InvalidPath)?;
    let progress_plan_path = normalize_rel_path(&raw.plan_rel_path).ok();
    if progress_plan_path.as_deref() != Some(expected_plan_rel_path.as_str()) {
        push_warning(&mut warnings, WARNING_PROGRESS_PLAN_PATH);
    }

    let mut seen = BTreeSet::new();
    let mut tasks = Vec::new();
    for task in raw.tasks {
        if task.index == 0 || !seen.insert(task.index) {
            push_warning(&mut warnings, WARNING_PROGRESS_DUPLICATE_TASK);
            continue;
        }
        let status = SimpleDeclaredStatus::parse(&task.status);
        if matches!(status, SimpleDeclaredStatus::Unknown(_)) {
            push_warning(&mut warnings, WARNING_PROGRESS_UNKNOWN_STATUS);
        }
        let runs = task
            .runs
            .into_iter()
            .map(|run| {
                if !known_run_state(&run.state) {
                    push_warning(&mut warnings, WARNING_PROGRESS_UNKNOWN_RUN_STATE);
                }
                SimpleProgressRun {
                    role: run.role,
                    agent_type: run.agent_type,
                    profile_id: run.profile_id,
                    task_id: run.task_id,
                    child_conversation_id: run.child_conversation_id,
                    state: run.state,
                    work_unit_key: run.work_unit_key,
                    recovery_count: run.recovery_count,
                    replaced_task_id: run.replaced_task_id,
                    replacement_reason: run.replacement_reason,
                }
            })
            .collect();
        tasks.push(SimpleProgressTask {
            index: task.index,
            status,
            commit: task.commit,
            runs,
        });
    }
    let final_review_status = SimpleFinalReviewStatus::parse(&raw.final_review_status);
    if matches!(final_review_status, SimpleFinalReviewStatus::Unknown(_)) {
        push_warning(&mut warnings, WARNING_PROGRESS_UNKNOWN_STATUS);
    }
    Ok(SimpleProgressDocument {
        snapshot: Some(SimpleProgressSnapshot {
            schema_version: raw.schema_version,
            plan_rel_path: progress_plan_path.unwrap_or(raw.plan_rel_path),
            active_task_index: raw.active_task_index,
            tasks,
            final_review_status,
            updated_at: raw.updated_at,
        }),
        warning_codes: warnings,
    })
}

async fn read_bounded(
    workspace: &Path,
    rel_path: &str,
    max_bytes: usize,
) -> Result<Vec<u8>, SimpleParseError> {
    let rel_path = normalize_rel_path(rel_path).map_err(|_| SimpleParseError::InvalidPath)?;
    let workspace = workspace.to_path_buf();
    tokio::task::spawn_blocking(move || {
        read_bounded_workspace_file(&workspace, &rel_path, max_bytes)
    })
    .await
    .map_err(|_| SimpleParseError::Unavailable(ArtifactFailure::ReadFailed))?
    .map_err(SimpleParseError::from)
}

pub async fn read_simple_plan(
    workspace: &Path,
    plan_rel_path: &str,
) -> Result<SimplePlanDocument, SimpleParseError> {
    let bytes = read_bounded(workspace, plan_rel_path, MAX_PLAN_MATERIAL_BYTES).await?;
    parse_simple_plan(&bytes)
}

pub async fn read_simple_progress(
    workspace: &Path,
    progress_rel_path: &str,
    plan_rel_path: &str,
) -> Result<SimpleProgressDocument, SimpleParseError> {
    let bytes = read_bounded(workspace, progress_rel_path, MAX_SIMPLE_PROGRESS_BYTES).await?;
    parse_simple_progress(&bytes, plan_rel_path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simple_parse_plan_uses_real_markdown_headings_and_warns_without_blocking() {
        let plan = br#"# Plan

```markdown
### Task 99: Not real
```

### Task 1: Parser

**Files:**
- Create: `src/parser.rs`

- [ ] Run parser tests

### Task 3: Projection

Body.

### Task 3: Duplicate

Ignored duplicate.

### Task x: Malformed
"#;
        let parsed = parse_simple_plan(plan).expect("parse");
        assert_eq!(
            parsed
                .tasks
                .iter()
                .map(|task| task.index)
                .collect::<Vec<_>>(),
            vec![1, 3]
        );
        assert_eq!(parsed.tasks[0].declared_files, vec!["src/parser.rs"]);
        assert!(parsed.tasks[0].verification_text.is_some());
        assert_eq!(
            parsed.warning_codes,
            vec![
                WARNING_PLAN_MALFORMED_TASK,
                WARNING_PLAN_DUPLICATE_TASK,
                WARNING_PLAN_NON_CONTIGUOUS,
            ]
        );
    }

    #[test]
    fn simple_parse_progress_preserves_unknowns_as_warnings() {
        let progress = br#"# Notes
<!-- codeg-simple-progress-v1
{
  "schema_version": 1,
  "plan_rel_path": "docs/other.md",
  "active_task_index": 1,
  "tasks": [{
    "index": 1,
    "status": "mystery",
    "runs": [{"role":"implementer","agent_type":"codex","state":"future"}]
  }],
  "final_review_status": "pending"
}
-->
"#;
        let parsed = parse_simple_progress(progress, "docs/plan.md").expect("parse");
        let snapshot = parsed.snapshot.expect("snapshot");
        assert!(matches!(
            snapshot.tasks[0].status,
            SimpleDeclaredStatus::Unknown(ref value) if value == "mystery"
        ));
        assert_eq!(
            parsed.warning_codes,
            vec![
                WARNING_PROGRESS_PLAN_PATH,
                WARNING_PROGRESS_UNKNOWN_STATUS,
                WARNING_PROGRESS_UNKNOWN_RUN_STATE,
            ]
        );
    }

    #[test]
    fn simple_parse_plan_bounds_mixed_level_tasks_and_extracts_task_material() {
        let plan = br#"## Task 1: First

### Notes
Keep this body.

### Task 2: Second

**Files:**
- Modify: `src/second.rs`

Run verification: `cargo test second`
"#;
        let parsed = parse_simple_plan(plan).expect("parse mixed task headings");

        assert_eq!(parsed.tasks.len(), 2);
        assert!(parsed.tasks[0].body.contains("Keep this body."));
        assert!(!parsed.tasks[0].body.contains("src/second.rs"));
        assert_eq!(parsed.tasks[1].declared_files, vec!["src/second.rs"]);
        assert_eq!(
            parsed.tasks[1].verification_text.as_deref(),
            Some("Run verification: `cargo test second`")
        );

        let oversized_section = format!(
            "## Task 1: Large\n\n{}\n\n## Task 2: Next\n",
            "x".repeat(MAX_PLAN_SECTION_BYTES + 128)
        );
        let bounded = parse_simple_plan(oversized_section.as_bytes())
            .expect("oversized section remains recoverable");
        assert!(bounded.tasks[0].body.len() <= MAX_PLAN_SECTION_BYTES);
        assert!(bounded
            .warning_codes
            .iter()
            .any(|code| code == WARNING_PLAN_SECTION_TRUNCATED));
    }

    #[test]
    fn simple_parse_progress_returns_safe_partial_models_for_document_problems() {
        let no_marker = parse_simple_progress(b"# ordinary Markdown\n", "docs/plan.md")
            .expect("missing marker is recoverable");
        assert!(no_marker.snapshot.is_none());
        assert_eq!(no_marker.warning_codes, vec![WARNING_PROGRESS_MISSING]);

        let schema_mismatch = br#"<!-- codeg-simple-progress-v1
{"schema_version":2,"plan_rel_path":"docs/plan.md","tasks":[{"index":1,"status":"completed"}],"final_review_status":"completed"}
-->"#;
        let invalid = parse_simple_progress(schema_mismatch, "docs/plan.md")
            .expect("unsupported schema is recoverable");
        assert!(
            invalid.snapshot.is_none(),
            "unsupported state must not complete work"
        );
        assert_eq!(invalid.warning_codes, vec![WARNING_PROGRESS_SCHEMA]);

        let duplicate = br#"<!-- codeg-simple-progress-v1
{"schema_version":1,"plan_rel_path":"docs/plan.md","tasks":[{"index":1},{"index":1}],"final_review_status":"pending"}
-->"#;
        let parsed = parse_simple_progress(duplicate, "docs/plan.md").expect("parse duplicate");
        assert_eq!(parsed.snapshot.expect("safe snapshot").tasks.len(), 1);
        assert_eq!(parsed.warning_codes, vec![WARNING_PROGRESS_DUPLICATE_TASK]);

        let invalid_json = br#"<!-- codeg-simple-progress-v1
{not-json}
-->"#;
        let invalid = parse_simple_progress(invalid_json, "docs/plan.md")
            .expect("invalid JSON is recoverable");
        assert!(invalid.snapshot.is_none());
        assert_eq!(invalid.warning_codes, vec![WARNING_PROGRESS_INVALID_JSON]);

        let multiple = br#"<!-- codeg-simple-progress-v1
{"schema_version":1,"plan_rel_path":"docs/plan.md","tasks":[],"final_review_status":"pending"}
-->
<!-- codeg-simple-progress-v1
{"schema_version":1,"plan_rel_path":"docs/plan.md","tasks":[],"final_review_status":"completed"}
-->"#;
        let multiple = parse_simple_progress(multiple, "docs/plan.md")
            .expect("the first of multiple blocks remains readable");
        assert!(multiple.snapshot.is_some());
        assert_eq!(multiple.warning_codes, vec![WARNING_PROGRESS_MULTIPLE]);
    }

    #[test]
    fn simple_parse_progress_bounds_markers_and_document_size() {
        let truncated = parse_simple_progress(
            b"<!-- codeg-simple-progress-v1 {\"schema_version\":1}",
            "docs/plan.md",
        )
        .expect("truncated marker is recoverable");
        assert!(truncated.snapshot.is_none());
        assert_eq!(truncated.warning_codes, vec![WARNING_PROGRESS_TRUNCATED]);

        let too_large_block = format!(
            "<!-- codeg-simple-progress-v1 {} -->",
            "x".repeat(MAX_SIMPLE_PROGRESS_BLOCK_BYTES + 1)
        );
        let large = parse_simple_progress(too_large_block.as_bytes(), "docs/plan.md")
            .expect("oversized block is recoverable");
        assert!(large.snapshot.is_none());
        assert_eq!(large.warning_codes, vec![WARNING_PROGRESS_TOO_LARGE]);

        let over_limit = vec![b'x'; MAX_SIMPLE_PROGRESS_BYTES + 1];
        assert_eq!(
            parse_simple_progress(&over_limit, "docs/plan.md").expect_err("file limit"),
            SimpleParseError::SizeLimitExceeded
        );
        assert_eq!(
            parse_simple_plan(&[0xff]).expect_err("invalid UTF-8"),
            SimpleParseError::InvalidUtf8
        );
        assert_eq!(
            parse_simple_progress(&[0xff], "docs/plan.md").expect_err("progress invalid UTF-8"),
            SimpleParseError::InvalidUtf8
        );
        assert_eq!(
            parse_simple_plan(&vec![b'x'; MAX_PLAN_MATERIAL_BYTES + 1])
                .expect_err("Plan file limit"),
            SimpleParseError::SizeLimitExceeded
        );
    }

    #[tokio::test]
    async fn simple_parse_reads_reject_escaping_paths_and_report_missing_documents() {
        let workspace = tempfile::tempdir().expect("temporary workspace");
        assert_eq!(
            read_simple_plan(workspace.path(), "../outside.md")
                .await
                .expect_err("path escape"),
            SimpleParseError::InvalidPath
        );
        assert!(matches!(
            read_simple_progress(workspace.path(), "missing.md", "docs/plan.md").await,
            Err(SimpleParseError::Unavailable(_))
        ));
    }
}
