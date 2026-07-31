//! Parse and validate the optional `<!-- codeg-card-summary-v1 ... -->` block
//! that a child agent may append to its final assistant text.
//!
//! Validated summaries are frontend display data only — they are persisted on
//! the run row (`card_summary_json`) and may ride completion events, but must
//! **never** appear in parent-facing MCP tool results.
//!
//! **Report-file harvest (defense in depth):** agents sometimes put the card
//! only inside a written report `.md` and link it from chat. Settlement may
//! harvest a validated card from linked/touched report files when chat text
//! has no well-formed block. Prefer chat emission; harvest is a fallback.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Marker that opens a v1 card-summary HTML comment block.
pub const CARD_SUMMARY_MARKER: &str = "<!-- codeg-card-summary-v1";

const SUMMARY_MAX_CHARS: usize = 240;
const TEST_STATUS_MAX_CHARS: usize = 64;
const COMMITS_MAX: usize = 20;
const SHA_MAX: usize = 64;
const SUBJECT_MAX: usize = 200;
const CONCERNS_MAX: usize = 20;
const CONCERN_MAX_CHARS: usize = 240;
const COUNT_MAX: u32 = 1_000_000;
const PLAN_DIGEST_MAX: usize = 128;
const REPORT_FILE_MAX: usize = 512;

/// Wire-stable validated card summary (serde for event + DB JSON).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CardSummary {
    Review {
        verdict: ReviewVerdict,
        critical: u32,
        important: u32,
        minor: u32,
        summary: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        report_file: Option<String>,
    },
    Author {
        status: WorkStatus,
        summary: String,
        plan_digest: String,
        report_file: String,
    },
    Implementation {
        phase: ImplementationPhase,
        status: WorkStatus,
        summary: String,
        #[serde(default)]
        commits: Vec<CommitEntry>,
        #[serde(default)]
        tests: Option<TestsSummary>,
        #[serde(default)]
        concerns: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        report_file: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewVerdict {
    Approve,
    ApproveWithMinors,
    RequestChanges,
    Block,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImplementationPhase {
    Implementation,
    Fix,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkStatus {
    Done,
    DoneWithConcerns,
    Blocked,
    NeedsContext,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommitEntry {
    pub sha: String,
    pub subject: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TestsSummary {
    pub status: String,
    #[serde(default)]
    pub passed: u32,
    #[serde(default)]
    pub failed: u32,
    #[serde(default)]
    pub summary: String,
}

/// Max report files to open when harvesting a missing chat card.
pub const MAX_REPORT_HARVEST_CANDIDATES: usize = 8;
/// Cap individual report reads (bytes) so settlement never slurps huge trees.
pub const MAX_REPORT_HARVEST_FILE_BYTES: u64 = 512 * 1024;

/// Extract the **last** well-formed `codeg-card-summary-v1` comment from raw
/// assistant text. Earlier echoed examples are ignored. Returns `None` when
/// missing or invalid (never fails the delegation).
pub fn extract_card_summary(raw_final_text: &str) -> Option<CardSummary> {
    let json = last_well_formed_summary_json(raw_final_text)?;
    parse_and_validate_summary_json(&json)
}

/// Prefer a card in `raw_final_text`; if missing, harvest from report files.
///
/// Candidate order (later wins when scanning reverse):
/// 1. Markdown link targets in chat (e.g. `](D:/…/final-review.md)`)
/// 2. `extra_paths` (typically runtime touched `.md` paths)
///
/// Relative candidates are resolved against `workspace_path` when provided.
/// Never fails the delegation: IO / oversized / invalid files are skipped.
pub fn extract_card_summary_with_report_fallback(
    raw_final_text: &str,
    extra_paths: &[PathBuf],
    workspace_path: Option<&Path>,
) -> Option<CardSummary> {
    if let Some(summary) = extract_card_summary(raw_final_text) {
        return Some(summary);
    }
    let candidates = collect_report_harvest_candidates(raw_final_text, extra_paths, workspace_path);
    for path in candidates.iter().rev().take(MAX_REPORT_HARVEST_CANDIDATES) {
        if let Some(summary) = extract_card_summary_from_report_file(path) {
            return Some(summary);
        }
    }
    None
}

/// Collect absolute-or-resolved paths that may contain a terminal card block.
pub fn collect_report_harvest_candidates(
    raw_final_text: &str,
    extra_paths: &[PathBuf],
    workspace_path: Option<&Path>,
) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    for raw in extract_markdown_link_targets(raw_final_text) {
        if let Some(path) = normalize_report_candidate(&raw, workspace_path) {
            push_unique_path(&mut out, path);
        }
    }
    for path in extra_paths {
        if is_markdown_report_path(path) {
            push_unique_path(&mut out, path.clone());
        } else if let Some(s) = path.to_str() {
            if let Some(norm) = normalize_report_candidate(s, workspace_path) {
                push_unique_path(&mut out, norm);
            }
        }
    }
    out
}

/// Read a single report file and extract a validated card (size-bounded).
pub fn extract_card_summary_from_report_file(path: &Path) -> Option<CardSummary> {
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() > MAX_REPORT_HARVEST_FILE_BYTES {
        return None;
    }
    let content = std::fs::read_to_string(path).ok()?;
    extract_card_summary(&content)
}

fn push_unique_path(out: &mut Vec<PathBuf>, path: PathBuf) {
    if !out.iter().any(|existing| existing == &path) {
        out.push(path);
    }
}

fn extract_markdown_link_targets(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(idx) = rest.find("](") {
        let after = &rest[idx + 2..];
        let Some(end) = after.find(')') else {
            break;
        };
        let target = after[..end].trim();
        if !target.is_empty() {
            out.push(target.to_string());
        }
        rest = &after[end + 1..];
    }
    out
}

fn normalize_report_candidate(raw: &str, workspace_path: Option<&Path>) -> Option<PathBuf> {
    let mut s = raw.trim();
    if s.is_empty() {
        return None;
    }
    // Angle-bracket autolinks: <path>
    if s.starts_with('<') && s.ends_with('>') && s.len() >= 2 {
        s = s[1..s.len() - 1].trim();
    }
    // Strip surrounding quotes.
    if (s.starts_with('"') && s.ends_with('"')) || (s.starts_with('\'') && s.ends_with('\'')) {
        s = s[1..s.len() - 1].trim();
    }
    // file:// / file:/// URLs (Windows file:///C:/... and file://localhost/...)
    if let Some(rest) = s.strip_prefix("file://") {
        s = rest.trim_start_matches('/');
        // On Windows, file:///C:/... becomes /C:/... after one slash strip;
        // trim leading slash before drive letter.
        if s.len() >= 3 && s.as_bytes()[0] == b'/' && s.as_bytes()[2] == b':' {
            s = &s[1..];
        }
    } else if s.contains("://") {
        // http(s) and other schemes are not local reports.
        return None;
    }

    if !looks_like_markdown_report(s) {
        return None;
    }

    let path = PathBuf::from(s);
    if path.is_absolute() {
        return Some(path);
    }
    // Workspace-relative only when a base is provided.
    let base = workspace_path?;
    // Reject parent traversal in relative report candidates.
    for component in path.components() {
        if matches!(component, std::path::Component::ParentDir) {
            return None;
        }
    }
    Some(base.join(path))
}

fn looks_like_markdown_report(s: &str) -> bool {
    let lower = s.to_ascii_lowercase();
    lower.ends_with(".md") || lower.ends_with(".markdown")
}

fn is_markdown_report_path(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| {
            let lower = ext.to_ascii_lowercase();
            lower == "md" || lower == "markdown"
        })
}

/// Strip all `codeg-card-summary-v1` comment blocks from text. Used when
/// building parent MCP result text so structured summaries are never exposed.
pub fn strip_card_summary_comments(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;
    while let Some(start) = rest.find(CARD_SUMMARY_MARKER) {
        out.push_str(&rest[..start]);
        let after_marker = &rest[start + CARD_SUMMARY_MARKER.len()..];
        match after_marker.find("-->") {
            Some(end) => {
                rest = &after_marker[end + 3..];
            }
            None => {
                // Unclosed comment: drop from marker to EOF.
                rest = "";
                break;
            }
        }
    }
    out.push_str(rest);
    out
}

fn last_well_formed_summary_json(raw: &str) -> Option<String> {
    let mut last: Option<String> = None;
    let mut search_from = 0usize;
    while search_from < raw.len() {
        let Some(rel) = raw[search_from..].find(CARD_SUMMARY_MARKER) else {
            break;
        };
        let start = search_from + rel;
        let body_start = start + CARD_SUMMARY_MARKER.len();
        let Some(end_rel) = raw[body_start..].find("-->") else {
            break;
        };
        let body_end = body_start + end_rel;
        let body = raw[body_start..body_end].trim();
        // Last *validated* well-formed block wins: a later malformed marker
        // must not suppress an earlier valid summary.
        if !body.is_empty()
            && (body.starts_with('{') || body.starts_with('['))
            && parse_and_validate_summary_json(body).is_some()
        {
            last = Some(body.to_string());
        }
        search_from = body_end + 3;
    }
    last
}

/// Parse persisted / event JSON through the same bounds validator used at
/// settlement. Shape-only serde is not enough for snapshot DTOs — corrupt
/// lengths / report paths must be omitted rather than round-tripped.
pub fn parse_and_validate_summary_json(json: &str) -> Option<CardSummary> {
    let value: serde_json::Value = serde_json::from_str(json).ok()?;
    let obj = value.as_object()?;
    let kind = obj.get("kind")?.as_str()?;
    match kind {
        "review" => {
            let verdict = parse_verdict(obj.get("verdict")?.as_str()?)?;
            let critical = parse_count(obj.get("critical")?)?;
            let important = parse_count(obj.get("important")?)?;
            let minor = parse_count(obj.get("minor")?)?;
            let summary = parse_bounded_string(obj.get("summary")?.as_str()?, SUMMARY_MAX_CHARS)?;
            let report_file = parse_optional_report_file(obj.get("report_file"))?;
            Some(CardSummary::Review {
                verdict,
                critical,
                important,
                minor,
                summary,
                report_file,
            })
        }
        "author" => {
            let status = parse_work_status(obj.get("status")?.as_str()?)?;
            let summary = parse_bounded_string(obj.get("summary")?.as_str()?, SUMMARY_MAX_CHARS)?;
            let plan_digest =
                parse_bounded_non_empty_string(obj.get("plan_digest")?.as_str()?, PLAN_DIGEST_MAX)?;
            let report_file = obj.get("report_file")?.as_str()?;
            if report_file.is_empty() {
                return None;
            }
            let report_file = validate_report_file(report_file)?;
            Some(CardSummary::Author {
                status,
                summary,
                plan_digest,
                report_file,
            })
        }
        "implementation" => {
            let phase = parse_phase(obj.get("phase")?.as_str()?)?;
            let status = parse_work_status(obj.get("status")?.as_str()?)?;
            let summary = parse_bounded_string(obj.get("summary")?.as_str()?, SUMMARY_MAX_CHARS)?;
            let commits = parse_commits(obj.get("commits"))?;
            let tests = parse_tests(obj.get("tests"))?;
            let concerns = parse_concerns(obj.get("concerns"))?;
            let report_file = match obj.get("report_file") {
                None | Some(serde_json::Value::Null) => None,
                Some(v) => {
                    let s = v.as_str()?;
                    Some(validate_report_file(s)?)
                }
            };
            Some(CardSummary::Implementation {
                phase,
                status,
                summary,
                commits,
                tests,
                concerns,
                report_file,
            })
        }
        _ => None,
    }
}

fn parse_verdict(s: &str) -> Option<ReviewVerdict> {
    match s {
        "approve" => Some(ReviewVerdict::Approve),
        "approve_with_minors" => Some(ReviewVerdict::ApproveWithMinors),
        "request_changes" => Some(ReviewVerdict::RequestChanges),
        "block" => Some(ReviewVerdict::Block),
        _ => None,
    }
}

fn parse_phase(s: &str) -> Option<ImplementationPhase> {
    match s {
        "implementation" => Some(ImplementationPhase::Implementation),
        "fix" => Some(ImplementationPhase::Fix),
        _ => None,
    }
}

fn parse_work_status(s: &str) -> Option<WorkStatus> {
    match s {
        "done" => Some(WorkStatus::Done),
        "done_with_concerns" => Some(WorkStatus::DoneWithConcerns),
        "blocked" => Some(WorkStatus::Blocked),
        "needs_context" => Some(WorkStatus::NeedsContext),
        _ => None,
    }
}

fn parse_count(v: &serde_json::Value) -> Option<u32> {
    let n = if let Some(u) = v.as_u64() {
        u
    } else {
        let i = v.as_i64()?;
        if i < 0 {
            return None;
        }
        i as u64
    };
    if n > COUNT_MAX as u64 {
        return None;
    }
    Some(n as u32)
}

fn parse_bounded_string(s: &str, max_chars: usize) -> Option<String> {
    if s.chars().count() > max_chars {
        return None;
    }
    Some(s.to_string())
}

fn parse_bounded_non_empty_string(s: &str, max_chars: usize) -> Option<String> {
    if s.trim().is_empty() {
        return None;
    }
    parse_bounded_string(s, max_chars)
}

fn parse_optional_report_file(v: Option<&serde_json::Value>) -> Option<Option<String>> {
    match v {
        None | Some(serde_json::Value::Null) => Some(None),
        Some(value) => Some(Some(validate_report_file(value.as_str()?)?)),
    }
}

fn parse_commits(v: Option<&serde_json::Value>) -> Option<Vec<CommitEntry>> {
    let Some(v) = v else {
        return Some(Vec::new());
    };
    if v.is_null() {
        return Some(Vec::new());
    }
    let arr = v.as_array()?;
    if arr.len() > COMMITS_MAX {
        return None;
    }
    let mut out = Vec::with_capacity(arr.len());
    for item in arr {
        let obj = item.as_object()?;
        let sha = obj.get("sha")?.as_str()?;
        let subject = obj.get("subject")?.as_str()?;
        if sha.chars().count() > SHA_MAX || subject.chars().count() > SUBJECT_MAX {
            return None;
        }
        out.push(CommitEntry {
            sha: sha.to_string(),
            subject: subject.to_string(),
        });
    }
    Some(out)
}

fn parse_tests(v: Option<&serde_json::Value>) -> Option<Option<TestsSummary>> {
    let Some(v) = v else {
        return Some(None);
    };
    if v.is_null() {
        return Some(None);
    }
    let obj = v.as_object()?;
    let status = parse_bounded_string(obj.get("status")?.as_str()?, TEST_STATUS_MAX_CHARS)?;
    let passed = match obj.get("passed") {
        None => 0,
        Some(x) => parse_count(x)?,
    };
    let failed = match obj.get("failed") {
        None => 0,
        Some(x) => parse_count(x)?,
    };
    let summary = match obj.get("summary") {
        None => String::new(),
        Some(x) => {
            let s = x.as_str()?;
            if s.chars().count() > SUMMARY_MAX_CHARS {
                return None;
            }
            s.to_string()
        }
    };
    Some(Some(TestsSummary {
        status,
        passed,
        failed,
        summary,
    }))
}

fn parse_concerns(v: Option<&serde_json::Value>) -> Option<Vec<String>> {
    let Some(v) = v else {
        return Some(Vec::new());
    };
    if v.is_null() {
        return Some(Vec::new());
    }
    let arr = v.as_array()?;
    if arr.len() > CONCERNS_MAX {
        return None;
    }
    let mut out = Vec::with_capacity(arr.len());
    for item in arr {
        let s = item.as_str()?;
        if s.chars().count() > CONCERN_MAX_CHARS {
            return None;
        }
        out.push(s.to_string());
    }
    Some(out)
}

pub(crate) fn validate_report_file(path: &str) -> Option<String> {
    if path.chars().count() > REPORT_FILE_MAX {
        return None;
    }
    // Workspace-relative only: no absolute paths, no `..` segments.
    if path.starts_with('/') || path.starts_with('\\') {
        return None;
    }
    if path.len() >= 2 && path.as_bytes()[1] == b':' {
        // Windows drive absolute
        return None;
    }
    for seg in path.split(['/', '\\']) {
        if seg == ".." {
            return None;
        }
    }
    Some(path.to_string())
}

/// Serialize a validated summary for durable storage / event payload.
pub fn card_summary_to_json(summary: &CardSummary) -> Result<String, serde_json::Error> {
    serde_json::to_string(summary)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn review_block(extra: &str) -> String {
        format!(
            r#"<!-- codeg-card-summary-v1
{{"kind":"review","verdict":"approve_with_minors","critical":0,
 "important":0,"minor":2,"summary":"Two Minor findings remain."{extra}}}
-->"#
        )
    }

    fn impl_block() -> String {
        r#"<!-- codeg-card-summary-v1
{"kind":"implementation","phase":"implementation","status":"done",
 "summary":"Implemented the cleaning component and automation tests.",
 "commits":[{"sha":"a1b2c3d","subject":"feat: add cleaning component"}],
 "tests":{"status":"passed","passed":14,"failed":0,
 "summary":"14/14 passing, output pristine"},"concerns":[],
 "report_file":".superpowers/sdd/task-3-report.md"}
-->"#
            .to_string()
    }

    fn author_block(digest: Option<&str>, report_file: Option<&str>) -> String {
        let digest = digest
            .map(|value| format!(r#","plan_digest":"{value}""#))
            .unwrap_or_default();
        let report_file = report_file
            .map(|value| format!(r#","report_file":"{value}""#))
            .unwrap_or_default();
        format!(
            r#"<!-- codeg-card-summary-v1
{{"kind":"author","status":"done","summary":"Plan is ready."{digest}{report_file}}}
-->"#
        )
    }

    #[test]
    fn last_well_formed_summary_wins() {
        let text = format!(
            "echo example\n{}\nreal work\n{}",
            review_block(""),
            impl_block()
        );
        let s = extract_card_summary(&text).expect("last block");
        match s {
            CardSummary::Implementation { status, .. } => {
                assert_eq!(status, WorkStatus::Done);
            }
            other => panic!("expected implementation, got {other:?}"),
        }
    }

    #[test]
    fn missing_summary_returns_none() {
        assert!(extract_card_summary("just plain text").is_none());
    }

    #[test]
    fn invalid_json_returns_none() {
        let text = "<!-- codeg-card-summary-v1 {not json} -->";
        assert!(extract_card_summary(text).is_none());
    }

    #[test]
    fn summary_too_long_rejected() {
        let long = "x".repeat(SUMMARY_MAX_CHARS + 1);
        let text = format!(
            r#"<!-- codeg-card-summary-v1
{{"kind":"review","verdict":"approve","critical":0,"important":0,"minor":0,"summary":"{long}"}}
-->"#
        );
        assert!(extract_card_summary(&text).is_none());
    }

    #[test]
    fn count_over_bound_rejected() {
        let text = r#"<!-- codeg-card-summary-v1
{"kind":"review","verdict":"approve","critical":1000001,"important":0,"minor":0,"summary":"ok"}
-->"#;
        assert!(extract_card_summary(text).is_none());
    }

    #[test]
    fn tests_status_too_long_rejected() {
        let status = "x".repeat(65);
        let text = format!(
            r#"<!-- codeg-card-summary-v1
{{"kind":"implementation","phase":"implementation","status":"done","summary":"ok",
 "tests":{{"status":"{status}"}}}}
-->"#
        );
        assert!(extract_card_summary(&text).is_none());
    }

    #[test]
    fn report_file_rejects_parent_segments() {
        let text = r#"<!-- codeg-card-summary-v1
{"kind":"implementation","phase":"fix","status":"blocked","summary":"blocked",
 "report_file":"../etc/passwd"}
-->"#;
        assert!(extract_card_summary(text).is_none());
    }

    #[test]
    fn strip_removes_all_summary_comments() {
        let text = format!("before\n{}\nafter\n{}", review_block(""), impl_block());
        let stripped = strip_card_summary_comments(&text);
        assert!(!stripped.contains(CARD_SUMMARY_MARKER));
        assert!(!stripped.contains("approve_with_minors"));
        assert!(stripped.contains("before"));
        assert!(stripped.contains("after"));
    }

    #[test]
    fn valid_review_parses() {
        let s = extract_card_summary(&review_block("")).unwrap();
        match s {
            CardSummary::Review {
                verdict,
                critical,
                minor,
                summary,
                ..
            } => {
                assert_eq!(verdict, ReviewVerdict::ApproveWithMinors);
                assert_eq!(critical, 0);
                assert_eq!(minor, 2);
                assert!(summary.contains("Minor"));
            }
            other => panic!("expected review, got {other:?}"),
        }
    }

    #[test]
    fn valid_author_evidence_parses() {
        let summary = extract_card_summary(&author_block(
            Some("sha256:plan-v2"),
            Some("docs/superpowers/plans/adaptive-routing.md"),
        ))
        .expect("valid author summary");
        assert_eq!(
            serde_json::to_value(summary).unwrap(),
            serde_json::json!({
                "kind": "author",
                "status": "done",
                "summary": "Plan is ready.",
                "plan_digest": "sha256:plan-v2",
                "report_file": "docs/superpowers/plans/adaptive-routing.md"
            })
        );
    }

    #[test]
    fn author_digest_is_required_and_non_empty() {
        assert!(extract_card_summary(&author_block(
            None,
            Some("docs/superpowers/plans/adaptive-routing.md")
        ))
        .is_none());
        assert!(extract_card_summary(&author_block(
            Some(""),
            Some("docs/superpowers/plans/adaptive-routing.md")
        ))
        .is_none());
    }

    #[test]
    fn author_digest_enforces_unicode_scalar_bound() {
        assert_eq!(PLAN_DIGEST_MAX, 128);

        let at_limit = "\u{1f9ed}".repeat(128);
        assert!(extract_card_summary(&author_block(
            Some(&at_limit),
            Some("docs/superpowers/plans/adaptive-routing.md")
        ))
        .is_some());

        let over_limit = "\u{1f9ed}".repeat(129);
        assert!(extract_card_summary(&author_block(
            Some(&over_limit),
            Some("docs/superpowers/plans/adaptive-routing.md")
        ))
        .is_none());
    }

    #[test]
    fn author_report_file_is_required_and_workspace_relative() {
        assert!(extract_card_summary(&author_block(Some("sha256:plan-v2"), None)).is_none());
        for report_file in ["C:/repo/plan.md", "/repo/plan.md", "../plan.md"] {
            assert!(
                extract_card_summary(&author_block(Some("sha256:plan-v2"), Some(report_file)))
                    .is_none(),
                "expected {report_file:?} to be rejected"
            );
        }
    }

    #[test]
    fn review_report_file_is_preserved_when_valid_and_rejected_when_unsafe() {
        let summary = extract_card_summary(&review_block(
            r#", "report_file":".superpowers/sdd/plan-review.md""#,
        ))
        .expect("valid review report path");
        assert_eq!(
            serde_json::to_value(summary).unwrap()["report_file"],
            ".superpowers/sdd/plan-review.md"
        );

        for report_file in ["C:/repo/review.md", "/repo/review.md", "../review.md"] {
            let extra = format!(r#", "report_file":"{report_file}""#);
            assert!(extract_card_summary(&review_block(&extra)).is_none());
        }
    }

    #[test]
    fn commits_over_bound_rejected() {
        let commits: Vec<String> = (0..21)
            .map(|i| format!(r#"{{"sha":"s{i}","subject":"c{i}"}}"#))
            .collect();
        let text = format!(
            r#"<!-- codeg-card-summary-v1
{{"kind":"implementation","phase":"implementation","status":"done","summary":"ok",
 "commits":[{}]}}
-->"#,
            commits.join(",")
        );
        assert!(extract_card_summary(&text).is_none());
    }

    #[test]
    fn malformed_final_marker_does_not_suppress_earlier_valid() {
        let text = format!(
            "work\n{}\n<!-- codeg-card-summary-v1 {{not valid json}} -->",
            review_block("")
        );
        let s = extract_card_summary(&text).expect("earlier valid block");
        match s {
            CardSummary::Review { minor, .. } => assert_eq!(minor, 2),
            other => panic!("expected review, got {other:?}"),
        }
    }

    #[test]
    fn report_fallback_reads_card_from_linked_markdown_file() {
        let dir = std::env::temp_dir().join(format!(
            "codeg-card-harvest-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let report = dir.join("final-review.md");
        // Session-2534 shape: card only in report file, not in chat.
        std::fs::write(
            &report,
            r#"# Final Whole-Branch Review

**Verdict:** request_changes

<!-- codeg-card-summary-v1
{"kind":"review","verdict":"request_changes","critical":0,"important":1,"minor":2,"summary":"HEAD fails TypeScript build; two deferred UI minors remain.","report_file":".superpowers/sdd/final-review.md"}
-->
"#,
        )
        .unwrap();

        let chat = format!(
            "Verdict: `request_changes`.\n\nReport: [final-review.md]({})",
            report.display()
        );
        assert!(
            extract_card_summary(&chat).is_none(),
            "chat must not contain the card — simulates Codex final reviewer"
        );

        let summary = extract_card_summary_with_report_fallback(&chat, &[], None)
            .expect("harvest from markdown link");
        match summary {
            CardSummary::Review {
                verdict,
                important,
                report_file,
                ..
            } => {
                assert_eq!(verdict, ReviewVerdict::RequestChanges);
                assert_eq!(important, 1);
                assert_eq!(
                    report_file.as_deref(),
                    Some(".superpowers/sdd/final-review.md")
                );
            }
            other => panic!("expected review, got {other:?}"),
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn report_fallback_uses_touched_paths_when_chat_has_no_link() {
        let dir = std::env::temp_dir().join(format!(
            "codeg-card-touched-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let report = dir.join("task-1-review.md");
        std::fs::write(
            &report,
            r#"<!-- codeg-card-summary-v1
{"kind":"review","verdict":"approve","critical":0,"important":0,"minor":0,"summary":"Task 1 rebind approve.","report_file":".superpowers/sdd/task-1-review.md"}
-->
"#,
        )
        .unwrap();

        let chat = "Review complete. See the report on disk.";
        let summary =
            extract_card_summary_with_report_fallback(chat, std::slice::from_ref(&report), None)
                .expect("harvest from touched path");
        match summary {
            CardSummary::Review { verdict, .. } => {
                assert_eq!(verdict, ReviewVerdict::Approve);
            }
            other => panic!("expected review, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn report_fallback_prefers_chat_card_over_file() {
        let dir = std::env::temp_dir().join(format!(
            "codeg-card-prefer-chat-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let report = dir.join("stale.md");
        std::fs::write(
            &report,
            r#"<!-- codeg-card-summary-v1
{"kind":"review","verdict":"block","critical":9,"important":0,"minor":0,"summary":"stale file card"}
-->
"#,
        )
        .unwrap();
        let chat = format!(
            "fresh chat card\n{}\nReport: [stale.md]({})",
            review_block(""),
            report.display()
        );
        let summary = extract_card_summary_with_report_fallback(&chat, &[report], None).unwrap();
        match summary {
            CardSummary::Review {
                verdict: ReviewVerdict::ApproveWithMinors,
                minor: 2,
                ..
            } => {}
            other => panic!("chat card must win, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn report_fallback_resolves_workspace_relative_links() {
        let dir = std::env::temp_dir().join(format!(
            "codeg-card-rel-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let sdd = dir.join(".superpowers").join("sdd");
        std::fs::create_dir_all(&sdd).unwrap();
        let report = sdd.join("final-review.md");
        std::fs::write(
            &report,
            r#"<!-- codeg-card-summary-v1
{"kind":"review","verdict":"request_changes","critical":0,"important":1,"minor":0,"summary":"build fails","report_file":".superpowers/sdd/final-review.md"}
-->
"#,
        )
        .unwrap();
        let chat = "Report: [final-review.md](.superpowers/sdd/final-review.md)";
        let summary = extract_card_summary_with_report_fallback(chat, &[], Some(dir.as_path()))
            .expect("relative link under workspace");
        match summary {
            CardSummary::Review {
                verdict: ReviewVerdict::RequestChanges,
                ..
            } => {}
            other => panic!("expected request_changes, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn report_fallback_skips_http_links_and_parent_relative() {
        let chat = "see [doc](https://example.com/a.md) and [bad](../secret.md)";
        assert!(
            extract_card_summary_with_report_fallback(chat, &[], Some(Path::new("/tmp/ws")))
                .is_none()
        );
    }
}
