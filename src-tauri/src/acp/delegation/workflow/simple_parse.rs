//! Bounded, non-authoritative parsing for Simple Plan and progress documents.

use std::collections::BTreeSet;
use std::path::Path;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::artifact_resolver::{read_bounded_workspace_file, ArtifactError, ArtifactFailure};
use super::key::{normalize_rel_path, parse_recognized_work_unit_key};
use super::plan_material::{
    collect_headings_and_front_matter, normalize_source_for_parsing, MAX_PLAN_MATERIAL_BYTES,
    MAX_PLAN_SECTION_BYTES,
};
use super::types::ReviewerSlot;

pub const MAX_SIMPLE_PROGRESS_BYTES: usize = 512 * 1024;
pub const MAX_SIMPLE_PROGRESS_BLOCK_BYTES: usize = 64 * 1024;
pub const MAX_SIMPLE_ROUTING_BLOCK_BYTES: usize = 256 * 1024;
pub const MAX_SIMPLE_PROJECTION_WARNINGS: usize = 64;
const PROGRESS_MARKER: &str = "<!-- codeg-simple-progress-v1";
const ROUTING_MARKER: &str = "<!-- codeg-b2d-routing-v1";
const COMMENT_END: &str = "-->";

pub const WARNING_PLAN_DUPLICATE_TASK: &str = "simple_plan_duplicate_task_index";
pub const WARNING_PLAN_MALFORMED_TASK: &str = "simple_plan_malformed_task_heading";
pub const WARNING_PLAN_NON_CONTIGUOUS: &str = "simple_plan_non_contiguous_tasks";
pub const WARNING_PLAN_SECTION_TRUNCATED: &str = "simple_plan_section_truncated";
pub const WARNING_ROUTING_MULTIPLE: &str = "simple_routing_multiple_blocks";
pub const WARNING_ROUTING_TRUNCATED: &str = "simple_routing_block_truncated";
pub const WARNING_ROUTING_TOO_LARGE: &str = "simple_routing_block_too_large";
pub const WARNING_ROUTING_INVALID_JSON: &str = "simple_routing_invalid_json";
pub const WARNING_ROUTING_SCHEMA: &str = "simple_routing_schema_unsupported";
pub const WARNING_ROUTING_POLICY: &str = "simple_routing_policy_unsupported";
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SimpleAgentSelection {
    pub agent_type: String,
    pub profile_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SimpleTaskAgentGeneration {
    pub generation: u32,
    pub agent_type: String,
    pub profile_id: Option<String>,
    pub effective_from_task_index: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SimpleRiskEvidence {
    pub kind: String,
    pub score: Option<u32>,
    pub evidence: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SimpleTaskRisk {
    pub level: String,
    pub hard_triggers: Vec<SimpleRiskEvidence>,
    pub soft_signals: Vec<SimpleRiskEvidence>,
    pub score: u32,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SimpleTaskReviewerRoute {
    pub slot: ReviewerSlot,
    pub agent_type: String,
    pub profile_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SimpleTaskRoute {
    pub implementer: SimpleAgentSelection,
    pub reviewers: Vec<SimpleTaskReviewerRoute>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SimpleRoutingTask {
    pub index: u32,
    pub task_agent_generation: u32,
    pub risk: SimpleTaskRisk,
    pub route: SimpleTaskRoute,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SimpleRoutingSnapshot {
    pub schema_version: u32,
    pub risk_policy_version: String,
    pub task_agent_generations: Vec<SimpleTaskAgentGeneration>,
    pub tasks: Vec<SimpleRoutingTask>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct SimplePlanDocument {
    pub tasks: Vec<SimplePlanTask>,
    pub routing: Option<SimpleRoutingSnapshot>,
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
pub struct SimpleExpectedReviewerKeys {
    pub primary: String,
    pub auxiliary: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SimpleExpectedWorkUnitKeys {
    pub implementer: String,
    pub reviewers: SimpleExpectedReviewerKeys,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SimpleProgressTask {
    pub index: u32,
    pub status: SimpleDeclaredStatus,
    pub commit: Option<String>,
    pub risk_level: Option<String>,
    pub task_agent_generation: Option<u32>,
    pub expected_work_unit_keys: Option<SimpleExpectedWorkUnitKeys>,
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
    risk_level: Option<String>,
    #[serde(default)]
    task_agent_generation: Option<u32>,
    #[serde(default)]
    expected_work_unit_keys: Option<SimpleExpectedWorkUnitKeys>,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SimpleCommentProblem {
    Truncated,
    TooLarge,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SimpleCommentBlock<'a> {
    body: Option<&'a str>,
    marker_count: usize,
    problem: Option<SimpleCommentProblem>,
}

#[derive(Debug, Clone, Copy)]
struct MarkdownFence {
    character: u8,
    length: usize,
}

fn markdown_fence_start(line: &str) -> Option<MarkdownFence> {
    let bytes = line.as_bytes();
    let indentation = bytes.iter().take_while(|byte| **byte == b' ').count();
    if indentation > 3 {
        return None;
    }
    let character = *bytes.get(indentation)?;
    if !matches!(character, b'`' | b'~') {
        return None;
    }
    let length = bytes[indentation..]
        .iter()
        .take_while(|byte| **byte == character)
        .count();
    (length >= 3).then_some(MarkdownFence { character, length })
}

fn markdown_fence_end(line: &str, fence: MarkdownFence) -> bool {
    let bytes = line.as_bytes();
    let indentation = bytes.iter().take_while(|byte| **byte == b' ').count();
    if indentation > 3 {
        return false;
    }
    let length = bytes[indentation..]
        .iter()
        .take_while(|byte| **byte == fence.character)
        .count();
    length >= fence.length
        && bytes[indentation + length..]
            .iter()
            .all(|byte| matches!(byte, b' ' | b'\t' | b'\r'))
}

fn extract_unfenced_comment<'a>(
    source: &'a str,
    marker: &str,
    max_block_bytes: usize,
) -> SimpleCommentBlock<'a> {
    let mut marker_offsets = Vec::new();
    let mut fence = None;
    let mut line_start = 0;

    for line_with_ending in source.split_inclusive('\n') {
        let line = line_with_ending
            .strip_suffix('\n')
            .unwrap_or(line_with_ending);
        if let Some(active_fence) = fence {
            if markdown_fence_end(line, active_fence) {
                fence = None;
            }
            line_start += line_with_ending.len();
            continue;
        }
        if let Some(opening_fence) = markdown_fence_start(line) {
            fence = Some(opening_fence);
            line_start += line_with_ending.len();
            continue;
        }

        let indentation = line
            .as_bytes()
            .iter()
            .take_while(|byte| **byte == b' ')
            .count();
        let marker_is_exact = line[indentation..]
            .strip_prefix(marker)
            // A marker ends with the line or ASCII whitespace before its body.
            .is_some_and(|rest| rest.is_empty() || rest.as_bytes()[0].is_ascii_whitespace());
        if indentation <= 3 && marker_is_exact {
            marker_offsets.push(line_start + indentation);
        }
        line_start += line_with_ending.len();
    }

    let marker_count = marker_offsets.len();
    let Some(marker_start) = marker_offsets.first().copied() else {
        return SimpleCommentBlock {
            body: None,
            marker_count,
            problem: None,
        };
    };
    let body_start = marker_start + marker.len();
    let Some(relative_end) = source[body_start..].find(COMMENT_END) else {
        return SimpleCommentBlock {
            body: None,
            marker_count,
            problem: Some(SimpleCommentProblem::Truncated),
        };
    };
    let body = source[body_start..body_start + relative_end].trim();
    if body.len() > max_block_bytes {
        return SimpleCommentBlock {
            body: None,
            marker_count,
            problem: Some(SimpleCommentProblem::TooLarge),
        };
    }
    SimpleCommentBlock {
        body: Some(body),
        marker_count,
        problem: None,
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
    let routing_block =
        extract_unfenced_comment(decoded, ROUTING_MARKER, MAX_SIMPLE_ROUTING_BLOCK_BYTES);
    if routing_block.marker_count > 1 {
        push_warning(&mut warnings, WARNING_ROUTING_MULTIPLE);
    }
    let routing = match (routing_block.body, routing_block.problem) {
        (_, Some(SimpleCommentProblem::Truncated)) => {
            push_warning(&mut warnings, WARNING_ROUTING_TRUNCATED);
            None
        }
        (_, Some(SimpleCommentProblem::TooLarge)) => {
            push_warning(&mut warnings, WARNING_ROUTING_TOO_LARGE);
            None
        }
        (Some(json), None) => match serde_json::from_str::<SimpleRoutingSnapshot>(json) {
            Ok(snapshot) if snapshot.schema_version != 1 => {
                push_warning(&mut warnings, WARNING_ROUTING_SCHEMA);
                None
            }
            Ok(snapshot) if snapshot.risk_policy_version != "b2d_task_risk_v1" => {
                push_warning(&mut warnings, WARNING_ROUTING_POLICY);
                None
            }
            Ok(snapshot) => Some(snapshot),
            Err(_) => {
                push_warning(&mut warnings, WARNING_ROUTING_INVALID_JSON);
                None
            }
        },
        (None, None) => None,
    };
    Ok(SimplePlanDocument {
        tasks,
        routing,
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

fn recognized_expected_work_unit_keys(keys: &SimpleExpectedWorkUnitKeys) -> bool {
    parse_recognized_work_unit_key(&keys.implementer).is_some()
        && parse_recognized_work_unit_key(&keys.reviewers.primary).is_some()
        && keys
            .reviewers
            .auxiliary
            .as_deref()
            .is_none_or(|key| parse_recognized_work_unit_key(key).is_some())
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
    let progress_block =
        extract_unfenced_comment(source, PROGRESS_MARKER, MAX_SIMPLE_PROGRESS_BLOCK_BYTES);
    if progress_block.marker_count == 0 {
        push_warning(&mut warnings, WARNING_PROGRESS_MISSING);
        return Ok(SimpleProgressDocument {
            snapshot: None,
            warning_codes: warnings,
        });
    }
    if progress_block.marker_count > 1 {
        push_warning(&mut warnings, WARNING_PROGRESS_MULTIPLE);
    }
    let json = match (progress_block.body, progress_block.problem) {
        (_, Some(SimpleCommentProblem::Truncated)) => {
            push_warning(&mut warnings, WARNING_PROGRESS_TRUNCATED);
            return Ok(SimpleProgressDocument {
                snapshot: None,
                warning_codes: warnings,
            });
        }
        (_, Some(SimpleCommentProblem::TooLarge)) => {
            push_warning(&mut warnings, WARNING_PROGRESS_TOO_LARGE);
            return Ok(SimpleProgressDocument {
                snapshot: None,
                warning_codes: warnings,
            });
        }
        (Some(json), None) => json,
        (None, None) => {
            push_warning(&mut warnings, WARNING_PROGRESS_TRUNCATED);
            return Ok(SimpleProgressDocument {
                snapshot: None,
                warning_codes: warnings,
            });
        }
    };
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
    if raw
        .tasks
        .iter()
        .filter_map(|task| task.expected_work_unit_keys.as_ref())
        .any(|keys| !recognized_expected_work_unit_keys(keys))
    {
        push_warning(&mut warnings, WARNING_PROGRESS_INVALID_JSON);
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
            risk_level: task.risk_level,
            task_agent_generation: task.task_agent_generation,
            expected_work_unit_keys: task.expected_work_unit_keys,
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
    use super::super::types::ReviewerSlot;
    use super::*;

    const VALID_ROUTING_JSON: &str = r#"{
  "schema_version": 1,
  "risk_policy_version": "b2d_task_risk_v1",
  "task_agent_generations": [{
    "generation": 1,
    "agent_type": "grok",
    "profile_id": null,
    "effective_from_task_index": 1
  }],
  "tasks": [{
    "index": 1,
    "task_agent_generation": 1,
    "risk": {
      "level": "high",
      "hard_triggers": [{
        "kind": "public_compatibility",
        "evidence": ["public parser model"]
      }],
      "soft_signals": [{
        "kind": "shared_interface",
        "score": 1,
        "evidence": ["Simple Plan projection"]
      }],
      "score": 1,
      "reason": "Adds routing metadata."
    },
    "route": {
      "implementer": {"agent_type": "codex", "profile_id": null},
      "reviewers": [
        {"slot": "primary", "agent_type": "codex", "profile_id": null},
        {"slot": "auxiliary", "agent_type": "grok", "profile_id": null}
      ]
    }
  }]
}"#;

    fn routed_plan(routing_json: &str) -> String {
        format!(
            "# Plan\n\n<!-- codeg-b2d-routing-v1\n{routing_json}\n-->\n\n## Task 1: Parse routing\n\nBody.\n\n### Task 2: Preserve tasks\n\nRun parser tests.\n"
        )
    }

    #[test]
    fn simple_parse_routing_parses_one_bounded_block_with_real_task_headings() {
        let parsed = parse_simple_plan(routed_plan(VALID_ROUTING_JSON).as_bytes()).expect("parse");
        assert_eq!(
            parsed
                .tasks
                .iter()
                .map(|task| (task.index, task.title.as_str()))
                .collect::<Vec<_>>(),
            vec![(1, "Parse routing"), (2, "Preserve tasks")]
        );

        let routing = parsed.routing.expect("routing");
        assert_eq!(routing.schema_version, 1);
        assert_eq!(routing.risk_policy_version, "b2d_task_risk_v1");
        assert_eq!(routing.task_agent_generations[0].agent_type, "grok");
        assert_eq!(routing.tasks[0].risk.hard_triggers[0].score, None);
        assert_eq!(
            routing.tasks[0].route.reviewers[1].slot,
            ReviewerSlot::Auxiliary
        );
        assert!(parsed.warning_codes.is_empty());
    }

    #[test]
    fn simple_parse_routing_keeps_legacy_plan_without_routing_warning() {
        let parsed = parse_simple_plan(b"## Task 1: Legacy\n\nBody.\n").expect("parse legacy");

        assert!(parsed.routing.is_none());
        assert!(parsed.warning_codes.is_empty());
    }

    #[test]
    fn simple_parse_routing_returns_tasks_and_exact_warning_for_bad_blocks() {
        let valid = routed_plan(VALID_ROUTING_JSON);
        let multiple = format!("{valid}\n<!-- codeg-b2d-routing-v1\n{VALID_ROUTING_JSON}\n-->\n");
        let invalid_json = routed_plan("{not-json}");
        let unsupported_schema = routed_plan(&VALID_ROUTING_JSON.replacen(
            "\"schema_version\": 1",
            "\"schema_version\": 2",
            1,
        ));
        let unsupported_policy =
            routed_plan(&VALID_ROUTING_JSON.replacen("b2d_task_risk_v1", "future_policy", 1));
        let truncated = format!(
            "## Task 1: Parse routing\n\nBody.\n<!-- codeg-b2d-routing-v1\n{VALID_ROUTING_JSON}\n"
        );
        let too_large = routed_plan(&"x".repeat(MAX_SIMPLE_ROUTING_BLOCK_BYTES + 1));

        for (name, source, warning, routing_expected) in [
            ("multiple", multiple, WARNING_ROUTING_MULTIPLE, true),
            ("truncated", truncated, WARNING_ROUTING_TRUNCATED, false),
            (
                "invalid JSON",
                invalid_json,
                WARNING_ROUTING_INVALID_JSON,
                false,
            ),
            (
                "unsupported schema",
                unsupported_schema,
                WARNING_ROUTING_SCHEMA,
                false,
            ),
            (
                "unsupported policy",
                unsupported_policy,
                WARNING_ROUTING_POLICY,
                false,
            ),
            ("too large", too_large, WARNING_ROUTING_TOO_LARGE, false),
        ] {
            let parsed = parse_simple_plan(source.as_bytes()).unwrap_or_else(|error| {
                panic!("{name} routing problem must remain recoverable: {error}")
            });
            assert_eq!(parsed.tasks[0].index, 1, "{name}");
            assert_eq!(parsed.tasks[0].title, "Parse routing", "{name}");
            assert_eq!(parsed.routing.is_some(), routing_expected, "{name}");
            assert_eq!(parsed.warning_codes, vec![warning], "{name}");
        }
    }

    #[test]
    fn simple_parse_routing_ignores_fenced_marker_examples() {
        let plan = format!(
            "# Plan\n\n```markdown\n<!-- codeg-b2d-routing-v1\n{{not-json}}\n-->\n```\n\n~~~markdown\n<!-- codeg-b2d-routing-v1\n{{also-not-json}}\n-->\n~~~\n\n<!-- codeg-b2d-routing-v1\n{VALID_ROUTING_JSON}\n-->\n\n## Task 1: Live routing\n"
        );
        let parsed = parse_simple_plan(plan.as_bytes()).expect("parse");

        assert!(parsed.routing.is_some());
        assert!(parsed.warning_codes.is_empty());
    }

    #[test]
    fn simple_parse_routing_ignores_prefix_lookalikes_before_live_marker() {
        let plan = format!(
            "# Plan\n\n<!-- codeg-b2d-routing-v10\n{{not-v1}}\n-->\n\n<!-- codeg-b2d-routing-v1-extra\n{{also-not-v1}}\n-->\n\n<!-- codeg-b2d-routing-v1\n{VALID_ROUTING_JSON}\n-->\n\n## Task 1: Live routing\n"
        );
        let parsed = parse_simple_plan(plan.as_bytes()).expect("parse");

        assert_eq!(
            parsed.routing.expect("live routing").risk_policy_version,
            "b2d_task_risk_v1"
        );
        assert!(parsed.warning_codes.is_empty());
    }

    #[test]
    fn simple_parse_routing_retains_plan_file_utf8_and_size_hard_bounds() {
        assert_eq!(
            parse_simple_plan(&[0xff]).expect_err("invalid UTF-8"),
            SimpleParseError::InvalidUtf8
        );
        assert_eq!(
            parse_simple_plan(&vec![b'x'; MAX_PLAN_MATERIAL_BYTES + 1])
                .expect_err("Plan file limit"),
            SimpleParseError::SizeLimitExceeded
        );
    }

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
        assert!(!matches!(
            snapshot.tasks[0].status,
            SimpleDeclaredStatus::Completed
        ));
        assert_eq!(snapshot.tasks[0].risk_level, None);
        assert_eq!(snapshot.tasks[0].task_agent_generation, None);
        assert_eq!(snapshot.tasks[0].expected_work_unit_keys, None);
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
    fn simple_parse_progress_preserves_additive_route_metadata_and_canonical_keys() {
        let progress = br#"<!-- codeg-simple-progress-v1
{
  "schema_version": 1,
  "plan_rel_path": "docs/plan.md",
  "active_task_index": null,
  "tasks": [{
    "index": 2,
    "status": "pending",
    "risk_level": "high",
    "task_agent_generation": 1,
    "expected_work_unit_keys": {
      "implementer": "task|2|implementer|codex|none",
      "reviewers": {
        "primary": "task|2|reviewer|primary|codex|none",
        "auxiliary": "task|2|reviewer|auxiliary|grok|none"
      }
    },
    "runs": []
  }],
  "final_review_status": "pending"
}
-->"#;
        let parsed = parse_simple_progress(progress, "docs/plan.md").expect("parse");
        let task = &parsed.snapshot.expect("snapshot").tasks[0];

        assert_eq!(task.risk_level.as_deref(), Some("high"));
        assert_eq!(task.task_agent_generation, Some(1));
        let keys = task
            .expected_work_unit_keys
            .as_ref()
            .expect("expected keys");
        assert_eq!(keys.implementer, "task|2|implementer|codex|none");
        assert_eq!(keys.reviewers.primary, "task|2|reviewer|primary|codex|none");
        assert_eq!(
            keys.reviewers.auxiliary.as_deref(),
            Some("task|2|reviewer|auxiliary|grok|none")
        );
        for key in [
            Some(keys.implementer.as_str()),
            Some(keys.reviewers.primary.as_str()),
            keys.reviewers.auxiliary.as_deref(),
        ] {
            assert!(
                key.and_then(super::super::key::parse_recognized_work_unit_key)
                    .is_some(),
                "expected route key must use the canonical grammar"
            );
        }
        assert!(parsed.warning_codes.is_empty());
    }

    #[test]
    fn simple_parse_progress_ignores_prefix_lookalikes_before_live_marker() {
        let progress = br#"<!-- codeg-simple-progress-v10
{"schema_version":10}
-->
<!-- codeg-simple-progress-v1-extra
{"schema_version":1,"plan_rel_path":"docs/wrong.md","tasks":[]}
-->
<!-- codeg-simple-progress-v1
{
  "schema_version": 1,
  "plan_rel_path": "docs/plan.md",
  "tasks": [{"index": 1, "status": "pending", "runs": []}],
  "final_review_status": "pending"
}
-->"#;
        let parsed = parse_simple_progress(progress, "docs/plan.md").expect("parse");

        let snapshot = parsed.snapshot.expect("live progress");
        assert_eq!(snapshot.plan_rel_path, "docs/plan.md");
        assert_eq!(snapshot.tasks.len(), 1);
        assert!(parsed.warning_codes.is_empty());
    }

    #[test]
    fn simple_parse_progress_rejects_malformed_nested_route_metadata_without_panicking() {
        let wrong_type = br#"<!-- codeg-simple-progress-v1
{
  "schema_version": 1,
  "plan_rel_path": "docs/plan.md",
  "tasks": [{
    "index": 1,
    "expected_work_unit_keys": {
      "implementer": "task|1|implementer|grok|none",
      "reviewers": {"primary": 7, "auxiliary": null}
    },
    "runs": []
  }],
  "final_review_status": "pending"
}
-->"#;
        let invalid_key = br#"<!-- codeg-simple-progress-v1
{
  "schema_version": 1,
  "plan_rel_path": "docs/plan.md",
  "tasks": [{
    "index": 1,
    "expected_work_unit_keys": {
      "implementer": "not-a-canonical-key",
      "reviewers": {
        "primary": "task|1|reviewer|primary|codex|none",
        "auxiliary": null
      }
    },
    "runs": []
  }],
  "final_review_status": "pending"
}
-->"#;

        for (name, malformed) in [
            ("wrong nested type", wrong_type.as_slice()),
            ("unrecognized key", invalid_key.as_slice()),
        ] {
            let parsed = parse_simple_progress(malformed, "docs/plan.md")
                .unwrap_or_else(|error| panic!("{name} must be recoverable: {error}"));
            assert!(parsed.snapshot.is_none(), "{name}");
            assert_eq!(
                parsed.warning_codes,
                vec![WARNING_PROGRESS_INVALID_JSON],
                "{name}"
            );
        }
    }

    #[test]
    fn simple_parse_progress_keeps_slotted_reviewer_runs_and_profiles_separate() {
        let progress = br#"<!-- codeg-simple-progress-v1
{
  "schema_version": 1,
  "plan_rel_path": "docs/plan.md",
  "tasks": [{
    "index": 2,
    "status": "in_progress",
    "runs": [
      {
        "role": "implementer",
        "agent_type": "codex",
        "profile_id": null,
        "state": "completed",
        "work_unit_key": "task|2|implementer|codex|none"
      },
      {
        "role": "reviewer",
        "agent_type": "codex",
        "profile_id": "review-profile",
        "state": "running",
        "work_unit_key": "task|2|reviewer|primary|codex|review-profile"
      },
      {
        "role": "reviewer",
        "agent_type": "grok",
        "profile_id": "task-profile",
        "state": "running",
        "work_unit_key": "task|2|reviewer|auxiliary|grok|task-profile"
      }
    ]
  }],
  "final_review_status": "pending"
}
-->"#;
        let parsed = parse_simple_progress(progress, "docs/plan.md").expect("parse");
        let snapshot = parsed.snapshot.expect("snapshot");
        let runs = &snapshot.tasks[0].runs;

        assert_eq!(runs.len(), 3);
        assert_eq!(runs[1].profile_id.as_deref(), Some("review-profile"));
        assert_eq!(runs[2].profile_id.as_deref(), Some("task-profile"));
        assert_eq!(
            runs[1].work_unit_key.as_deref(),
            Some("task|2|reviewer|primary|codex|review-profile")
        );
        assert_eq!(
            runs[2].work_unit_key.as_deref(),
            Some("task|2|reviewer|auxiliary|grok|task-profile")
        );
        assert_ne!(runs[1].work_unit_key, runs[2].work_unit_key);
        assert!(parsed.warning_codes.is_empty());
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
