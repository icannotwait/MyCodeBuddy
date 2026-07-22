//! Parse and validate the optional `<!-- codeg-card-summary-v1 ... -->` block
//! that a child agent may append to its final assistant text.
//!
//! Validated summaries are frontend display data only — they are persisted on
//! the run row (`card_summary_json`) and may ride completion events, but must
//! **never** appear in parent-facing MCP tool results.

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

/// Extract the **last** well-formed `codeg-card-summary-v1` comment from raw
/// assistant text. Earlier echoed examples are ignored. Returns `None` when
/// missing or invalid (never fails the delegation).
pub fn extract_card_summary(raw_final_text: &str) -> Option<CardSummary> {
    let json = last_well_formed_summary_json(raw_final_text)?;
    parse_and_validate_summary_json(&json)
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
            Some(CardSummary::Review {
                verdict,
                critical,
                important,
                minor,
                summary,
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

fn validate_report_file(path: &str) -> Option<String> {
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
}
