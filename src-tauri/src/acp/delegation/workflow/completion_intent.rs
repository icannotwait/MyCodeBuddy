use pulldown_cmark::{Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::ops::Range;
use unicode_normalization::UnicodeNormalization;

pub const MAX_COMPLETION_SUMMARY_BYTES: usize = 4 * 1024;
pub const MAX_COMPLETION_EXCERPT_BYTES: usize = 512;
pub const MAX_COMPLETION_DIAGNOSTICS: usize = 16;
pub const MAX_COMPLETION_CANDIDATES: usize = 8;
pub const MAX_REPORT_CANDIDATES: usize = 8;
pub const MAX_REPORT_BYTES: usize = 512 * 1024;
pub const MAX_REPORT_PATH_BYTES: usize = 1024;

const REVIEWER_SUFFIX: &str = "Finish with one plain-language conclusion line:\nConclusion: approve | approve with minor issues | request changes | blocked";
const PRODUCER_SUFFIX: &str = "Finish with one plain-language conclusion line:\nConclusion: done | done with concerns | blocked";

const LABELS: &[&str] = &[
    "conclusion",
    "final conclusion",
    "verdict",
    "结论",
    "最终结论",
    "审核结论",
];
const REPORT_SECTION_LABELS: &[&str] = &["conclusion", "verdict", "结论"];

const OUTCOME_ALIASES: &[(CompletionOutcome, &[&str])] = &[
    (
        CompletionOutcome::Approve,
        &["approve", "approved", "pass", "通过", "认可"],
    ),
    (
        CompletionOutcome::ApproveWithMinors,
        &[
            "approve with minors",
            "approve with minor issues",
            "pass with minor issues",
            "有小问题通过",
            "有轻微问题通过",
        ],
    ),
    (
        CompletionOutcome::RequestChanges,
        &[
            "request changes",
            "changes requested",
            "needs changes",
            "需修改",
            "需要修改",
        ],
    ),
    (
        CompletionOutcome::Block,
        &["block", "blocked", "阻塞", "无法通过"],
    ),
    (
        CompletionOutcome::Done,
        &["done", "complete", "completed", "完成", "已完成"],
    ),
    (
        CompletionOutcome::DoneWithConcerns,
        &[
            "done with concerns",
            "completed with concerns",
            "有顾虑完成",
            "完成但有顾虑",
        ],
    ),
    (CompletionOutcome::Blocked, &["blocked", "无法完成", "阻塞"]),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompletionOutcome {
    Approve,
    ApproveWithMinors,
    RequestChanges,
    Block,
    Done,
    DoneWithConcerns,
    Blocked,
}

impl CompletionOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Approve => "approve",
            Self::ApproveWithMinors => "approve_with_minors",
            Self::RequestChanges => "request_changes",
            Self::Block => "block",
            Self::Done => "done",
            Self::DoneWithConcerns => "done_with_concerns",
            Self::Blocked => "blocked",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompletionRole {
    Reviewer,
    Author,
    Implementer,
    Fixer,
}

impl CompletionRole {
    pub fn accepts(self, outcome: CompletionOutcome) -> bool {
        match self {
            Self::Reviewer => matches!(
                outcome,
                CompletionOutcome::Approve
                    | CompletionOutcome::ApproveWithMinors
                    | CompletionOutcome::RequestChanges
                    | CompletionOutcome::Block
            ),
            Self::Author | Self::Implementer | Self::Fixer => matches!(
                outcome,
                CompletionOutcome::Done
                    | CompletionOutcome::DoneWithConcerns
                    | CompletionOutcome::Blocked
            ),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompletionIntentSource {
    CompleteWork,
    AssistantConclusion,
    Report,
    UserAdjudication,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompletionIntent {
    pub outcome: CompletionOutcome,
    pub summary: Option<String>,
    pub report_file: Option<String>,
    pub source: CompletionIntentSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CompletionIntentReason {
    #[serde(rename = "completion_intent_missing")]
    Missing,
    #[serde(rename = "completion_intent_conflict")]
    Conflict,
    #[serde(rename = "completion_outcome_role_mismatch")]
    RoleMismatch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompletionDiagnosticCode {
    Missing,
    EarlierConclusion,
    RoleMismatch,
    NoEligibleConclusion,
    CandidateLimitExceeded,
    ReportTooLarge,
    UnsafeReportPath,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompletionCandidate {
    pub outcome: CompletionOutcome,
    pub source: CompletionIntentSource,
    pub report_file: Option<String>,
    pub excerpt: String,
    pub role_compatible: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompletionDiagnostic {
    pub channel: CompletionIntentSource,
    pub code: CompletionDiagnosticCode,
    pub report_file: Option<String>,
    pub excerpt: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CompletionResolution {
    Resolved(CompletionIntent),
    NeedsDecision {
        reason_code: CompletionIntentReason,
        bounded_candidates: Vec<CompletionCandidate>,
        diagnostics: Vec<CompletionDiagnostic>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletionToolIntent {
    pub accepted_ordinal: i64,
    pub outcome: CompletionOutcome,
    pub summary: Option<String>,
    pub report_file: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletionReportCandidate {
    pub path: String,
    pub contents: String,
    pub summary: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletionResolverInput {
    pub role: CompletionRole,
    pub tool_intents: Vec<CompletionToolIntent>,
    pub final_assistant_text: String,
    pub report_candidates: Vec<CompletionReportCandidate>,
    pub touched_report_candidates: Vec<CompletionReportCandidate>,
}

pub fn build_conclusion_suffix(role: CompletionRole) -> &'static str {
    match role {
        CompletionRole::Reviewer => REVIEWER_SUFFIX,
        CompletionRole::Author | CompletionRole::Implementer | CompletionRole::Fixer => {
            PRODUCER_SUFFIX
        }
    }
}

pub fn resolve_completion_intent(input: &CompletionResolverInput) -> CompletionResolution {
    if let Some(selected) = input
        .tool_intents
        .iter()
        .max_by_key(|intent| intent.accepted_ordinal)
    {
        let candidate = CompletionCandidate {
            outcome: selected.outcome,
            source: CompletionIntentSource::CompleteWork,
            report_file: selected
                .report_file
                .as_deref()
                .and_then(|path| normalize_report_path(path, false)),
            excerpt: selected.outcome.as_str().to_string(),
            role_compatible: input.role.accepts(selected.outcome),
        };
        if !candidate.role_compatible {
            return needs_decision(
                CompletionIntentReason::RoleMismatch,
                vec![candidate.clone()],
                vec![diagnostic_for_candidate(
                    &candidate,
                    CompletionDiagnosticCode::RoleMismatch,
                )],
            );
        }

        let report_file = candidate.report_file;
        let summary = selected
            .summary
            .as_deref()
            .and_then(bound_nonblank)
            .or_else(|| {
                parse_terminal_conclusions(input.role, &input.final_assistant_text)
                    .authoritative
                    .and_then(|conclusion| conclusion.summary)
            })
            .or_else(|| report_summary_for_hint(input, report_file.as_deref()))
            .or_else(|| Some(selected.outcome.as_str().to_string()));
        return CompletionResolution::Resolved(CompletionIntent {
            outcome: selected.outcome,
            summary,
            report_file,
            source: CompletionIntentSource::CompleteWork,
        });
    }

    let terminal = parse_terminal_conclusions(input.role, &input.final_assistant_text);
    if let Some(authoritative) = terminal.authoritative.as_ref() {
        let candidates =
            parsed_candidates(&terminal, CompletionIntentSource::AssistantConclusion, None);
        if !authoritative.role_compatible {
            return needs_decision(
                CompletionIntentReason::RoleMismatch,
                candidates,
                vec![diagnostic_from_parsed(
                    authoritative,
                    CompletionIntentSource::AssistantConclusion,
                    None,
                    CompletionDiagnosticCode::RoleMismatch,
                )],
            );
        }

        let report_file = terminal_report_hint(&terminal.lines, authoritative.line_index);
        let summary = authoritative
            .summary
            .clone()
            .or_else(|| report_summary_for_hint(input, report_file.as_deref()))
            .or_else(|| Some(authoritative.outcome.as_str().to_string()));
        return CompletionResolution::Resolved(CompletionIntent {
            outcome: authoritative.outcome,
            summary,
            report_file,
            source: CompletionIntentSource::AssistantConclusion,
        });
    }

    resolve_reports(input)
}

fn resolve_reports(input: &CompletionResolverInput) -> CompletionResolution {
    let supplied_count = input.report_candidates.len() + input.touched_report_candidates.len();
    if supplied_count > MAX_REPORT_CANDIDATES {
        return needs_decision(
            CompletionIntentReason::Missing,
            Vec::new(),
            vec![CompletionDiagnostic {
                channel: CompletionIntentSource::Report,
                code: CompletionDiagnosticCode::CandidateLimitExceeded,
                report_file: None,
                excerpt: None,
            }],
        );
    }

    let mut seen = HashSet::new();
    let mut matches = Vec::new();
    let mut diagnostics = Vec::new();
    for report in input
        .report_candidates
        .iter()
        .chain(input.touched_report_candidates.iter())
    {
        let Some(path) = normalize_report_path(&report.path, true) else {
            push_diagnostic(
                &mut diagnostics,
                CompletionDiagnostic {
                    channel: CompletionIntentSource::Report,
                    code: CompletionDiagnosticCode::UnsafeReportPath,
                    report_file: None,
                    excerpt: None,
                },
            );
            continue;
        };
        if !seen.insert(path.clone()) {
            continue;
        }
        if report.contents.len() > MAX_REPORT_BYTES {
            push_diagnostic(
                &mut diagnostics,
                CompletionDiagnostic {
                    channel: CompletionIntentSource::Report,
                    code: CompletionDiagnosticCode::ReportTooLarge,
                    report_file: Some(path),
                    excerpt: None,
                },
            );
            continue;
        }

        let parsed = parse_report_conclusions(input.role, &report.contents);
        let Some(authoritative) = parsed.authoritative.clone() else {
            push_diagnostic(
                &mut diagnostics,
                CompletionDiagnostic {
                    channel: CompletionIntentSource::Report,
                    code: CompletionDiagnosticCode::NoEligibleConclusion,
                    report_file: Some(path),
                    excerpt: None,
                },
            );
            continue;
        };
        let summary = authoritative
            .summary
            .clone()
            .or_else(|| report.summary.as_deref().and_then(bound_nonblank));
        matches.push(ReportMatch {
            path,
            authoritative,
            summary,
        });
    }

    if matches.is_empty() {
        if diagnostics.is_empty() {
            diagnostics.push(CompletionDiagnostic {
                channel: CompletionIntentSource::Report,
                code: CompletionDiagnosticCode::Missing,
                report_file: None,
                excerpt: None,
            });
        }
        return needs_decision(CompletionIntentReason::Missing, Vec::new(), diagnostics);
    }

    let candidates: Vec<_> = matches
        .iter()
        .map(|report| {
            completion_candidate(
                &report.authoritative,
                CompletionIntentSource::Report,
                Some(report.path.clone()),
            )
        })
        .collect();
    if matches
        .iter()
        .any(|report| !report.authoritative.role_compatible)
    {
        for report in &matches {
            if !report.authoritative.role_compatible {
                push_diagnostic(
                    &mut diagnostics,
                    diagnostic_from_parsed(
                        &report.authoritative,
                        CompletionIntentSource::Report,
                        Some(report.path.clone()),
                        CompletionDiagnosticCode::RoleMismatch,
                    ),
                );
            }
        }
        return needs_decision(
            CompletionIntentReason::RoleMismatch,
            candidates,
            diagnostics,
        );
    }

    let outcomes: HashSet<_> = matches
        .iter()
        .map(|report| report.authoritative.outcome)
        .collect();
    if outcomes.len() > 1 {
        return needs_decision(CompletionIntentReason::Conflict, candidates, diagnostics);
    }

    let selected = &matches[0];
    CompletionResolution::Resolved(CompletionIntent {
        outcome: selected.authoritative.outcome,
        summary: selected
            .summary
            .clone()
            .or_else(|| Some(selected.authoritative.outcome.as_str().to_string())),
        report_file: Some(selected.path.clone()),
        source: CompletionIntentSource::Report,
    })
}

struct ReportMatch {
    path: String,
    authoritative: ParsedConclusion,
    summary: Option<String>,
}

fn needs_decision(
    reason_code: CompletionIntentReason,
    mut bounded_candidates: Vec<CompletionCandidate>,
    mut diagnostics: Vec<CompletionDiagnostic>,
) -> CompletionResolution {
    bounded_candidates.truncate(MAX_COMPLETION_CANDIDATES);
    diagnostics.truncate(MAX_COMPLETION_DIAGNOSTICS);
    CompletionResolution::NeedsDecision {
        reason_code,
        bounded_candidates,
        diagnostics,
    }
}

fn push_diagnostic(diagnostics: &mut Vec<CompletionDiagnostic>, diagnostic: CompletionDiagnostic) {
    if diagnostics.len() < MAX_COMPLETION_DIAGNOSTICS {
        diagnostics.push(diagnostic);
    }
}

fn diagnostic_for_candidate(
    candidate: &CompletionCandidate,
    code: CompletionDiagnosticCode,
) -> CompletionDiagnostic {
    CompletionDiagnostic {
        channel: candidate.source,
        code,
        report_file: candidate.report_file.clone(),
        excerpt: Some(candidate.excerpt.clone()),
    }
}

fn diagnostic_from_parsed(
    parsed: &ParsedConclusion,
    channel: CompletionIntentSource,
    report_file: Option<String>,
    code: CompletionDiagnosticCode,
) -> CompletionDiagnostic {
    CompletionDiagnostic {
        channel,
        code,
        report_file,
        excerpt: Some(bound_utf8(&parsed.excerpt, MAX_COMPLETION_EXCERPT_BYTES)),
    }
}

fn parsed_candidates(
    parsed: &ParsedConclusions,
    source: CompletionIntentSource,
    report_file: Option<String>,
) -> Vec<CompletionCandidate> {
    let skip = parsed
        .eligible
        .len()
        .saturating_sub(MAX_COMPLETION_CANDIDATES);
    parsed
        .eligible
        .iter()
        .skip(skip)
        .map(|candidate| completion_candidate(candidate, source, report_file.clone()))
        .collect()
}

fn completion_candidate(
    candidate: &ParsedConclusion,
    source: CompletionIntentSource,
    report_file: Option<String>,
) -> CompletionCandidate {
    CompletionCandidate {
        outcome: candidate.outcome,
        source,
        report_file,
        excerpt: bound_utf8(&candidate.excerpt, MAX_COMPLETION_EXCERPT_BYTES),
        role_compatible: candidate.role_compatible,
    }
}

#[derive(Debug, Clone)]
struct ParsedConclusion {
    outcome: CompletionOutcome,
    role_compatible: bool,
    excerpt: String,
    line_index: usize,
    offset: usize,
    summary: Option<String>,
}

#[derive(Debug, Clone)]
struct ParsedConclusions {
    eligible: Vec<ParsedConclusion>,
    authoritative: Option<ParsedConclusion>,
    lines: Vec<SourceLine>,
}

impl ParsedConclusions {
    #[cfg(test)]
    fn classification(&self) -> Option<&'static str> {
        self.authoritative.as_ref().map(|candidate| {
            if candidate.role_compatible {
                candidate.outcome.as_str()
            } else {
                "role_mismatch"
            }
        })
    }
}

fn parse_terminal_conclusions(role: CompletionRole, source: &str) -> ParsedConclusions {
    let document = MarkdownDocument::parse(source);
    let mut eligible = Vec::new();
    for line in &document.lines {
        if !line.conclusion_eligible {
            continue;
        }
        if let Some((outcome, role_compatible)) = parse_conclusion_line(role, &line.text) {
            eligible.push(ParsedConclusion {
                outcome,
                role_compatible,
                excerpt: bound_utf8(&line.text, MAX_COMPLETION_EXCERPT_BYTES),
                line_index: line.index,
                offset: line.offset,
                summary: adjacent_summary(&document.lines, line.index),
            });
        }
    }
    eligible.sort_by_key(|candidate| candidate.offset);
    let authoritative = eligible.last().cloned();
    ParsedConclusions {
        eligible,
        authoritative,
        lines: document.lines,
    }
}

fn parse_report_conclusions(role: CompletionRole, source: &str) -> ParsedConclusions {
    let document = MarkdownDocument::parse(source);
    let mut eligible = Vec::new();

    for line in &document.lines {
        if !line.conclusion_eligible {
            continue;
        }
        if let Some((outcome, role_compatible)) = parse_conclusion_line(role, &line.text) {
            eligible.push(ParsedConclusion {
                outcome,
                role_compatible,
                excerpt: bound_utf8(&line.text, MAX_COMPLETION_EXCERPT_BYTES),
                line_index: line.index,
                offset: line.offset,
                summary: adjacent_summary(&document.lines, line.index),
            });
        }
    }

    for (index, heading) in document.blocks.iter().enumerate() {
        let MarkdownBlockKind::Heading(level) = heading.kind else {
            continue;
        };
        if heading.list_depth != 0
            || !matches!(level, HeadingLevel::H1 | HeadingLevel::H2)
            || !heading.inline_plain
            || !REPORT_SECTION_LABELS.contains(&normalize_match(&heading.text).as_str())
        {
            continue;
        }

        let mut first_plain_paragraph = None;
        for block in document.blocks.iter().skip(index + 1) {
            if matches!(
                block.kind,
                MarkdownBlockKind::Heading(HeadingLevel::H1 | HeadingLevel::H2)
            ) {
                break;
            }
            if block.kind == MarkdownBlockKind::Paragraph
                && block.list_depth == 0
                && block.inline_plain
            {
                first_plain_paragraph = Some(block);
                break;
            }
        }
        let Some(paragraph) = first_plain_paragraph else {
            continue;
        };
        let normalized = normalize_match(&paragraph.text);
        let Some((outcome, role_compatible)) = outcome_from_alias(role, &normalized) else {
            continue;
        };
        let line_index = document
            .lines
            .iter()
            .find(|line| ranges_overlap(&paragraph.range, line.offset, line.end_offset))
            .map_or(0, |line| line.index);
        eligible.push(ParsedConclusion {
            outcome,
            role_compatible,
            excerpt: bound_utf8(
                source
                    .get(paragraph.range.clone())
                    .unwrap_or(&paragraph.text),
                MAX_COMPLETION_EXCERPT_BYTES,
            ),
            line_index,
            offset: paragraph.range.start,
            summary: adjacent_summary(&document.lines, line_index),
        });
    }

    eligible.sort_by_key(|candidate| candidate.offset);
    eligible.dedup_by(|left, right| left.offset == right.offset && left.outcome == right.outcome);
    let authoritative = eligible.last().cloned();
    ParsedConclusions {
        eligible,
        authoritative,
        lines: document.lines,
    }
}

fn parse_conclusion_line(
    role: CompletionRole,
    original: &str,
) -> Option<(CompletionOutcome, bool)> {
    if original.is_empty() || original.chars().next().is_some_and(char::is_whitespace) {
        return None;
    }
    let normalized = normalize_match(original.trim_end());
    let (without_prefix, had_prefix) = strip_one_block_prefix(&normalized)?;
    if had_prefix && starts_block_prefix(without_prefix) {
        return None;
    }
    let without_bold = strip_one_bold_pair(without_prefix)?;
    if starts_block_prefix(without_bold)
        || without_bold.starts_with("**")
        || without_bold.starts_with("__")
        || without_bold.ends_with("**")
        || without_bold.ends_with("__")
    {
        return None;
    }

    let mut rest = None;
    for label in LABELS {
        if let Some(after_label) = without_bold.strip_prefix(label) {
            if after_label.is_empty()
                || after_label.starts_with(char::is_whitespace)
                || after_label.starts_with(':')
                || after_label.starts_with('-')
            {
                rest = Some(after_label);
                break;
            }
        }
    }
    let mut rest = rest?.trim_start();
    let separator = rest.chars().next()?;
    if !matches!(separator, ':' | '-') {
        return None;
    }
    rest = rest[separator.len_utf8()..].trim_start();
    if rest.is_empty() {
        return None;
    }

    let mut outcome_text = rest.trim_end();
    if let Some(last) = outcome_text.chars().last() {
        if matches!(last, '.' | '!' | '。' | '！') {
            outcome_text = outcome_text[..outcome_text.len() - last.len_utf8()].trim_end();
        }
    }
    outcome_from_alias(role, outcome_text)
}

fn strip_one_block_prefix(value: &str) -> Option<(&str, bool)> {
    if value.starts_with('#') {
        let count = value
            .chars()
            .take_while(|character| *character == '#')
            .count();
        if !(1..=6).contains(&count) || value.as_bytes().get(count) != Some(&b' ') {
            return None;
        }
        return Some((&value[count + 1..], true));
    }
    if value.starts_with("- ") || value.starts_with("* ") || value.starts_with("+ ") {
        return Some((&value[2..], true));
    }

    let digit_count = value.bytes().take_while(u8::is_ascii_digit).count();
    if digit_count > 0 {
        if digit_count > 3
            || !matches!(value.as_bytes().get(digit_count), Some(b'.' | b')'))
            || value.as_bytes().get(digit_count + 1) != Some(&b' ')
        {
            return None;
        }
        return Some((&value[digit_count + 2..], true));
    }
    Some((value, false))
}

fn starts_block_prefix(value: &str) -> bool {
    if value.starts_with('#')
        || value.starts_with("- ")
        || value.starts_with("* ")
        || value.starts_with("+ ")
    {
        return true;
    }
    let digit_count = value.bytes().take_while(u8::is_ascii_digit).count();
    digit_count > 0
        && matches!(value.as_bytes().get(digit_count), Some(b'.' | b')'))
        && value.as_bytes().get(digit_count + 1) == Some(&b' ')
}

fn strip_one_bold_pair(value: &str) -> Option<&str> {
    for marker in ["**", "__"] {
        if value.starts_with(marker) || value.ends_with(marker) {
            if !(value.starts_with(marker) && value.ends_with(marker))
                || value.len() <= marker.len() * 2
            {
                return None;
            }
            let inner = &value[marker.len()..value.len() - marker.len()];
            if inner.chars().next().is_some_and(char::is_whitespace)
                || inner.chars().last().is_some_and(char::is_whitespace)
            {
                return None;
            }
            return Some(inner);
        }
    }
    Some(value)
}

fn outcome_from_alias(role: CompletionRole, normalized: &str) -> Option<(CompletionOutcome, bool)> {
    let matches: Vec<_> = OUTCOME_ALIASES
        .iter()
        .filter_map(|(outcome, aliases)| aliases.contains(&normalized).then_some(*outcome))
        .collect();
    if matches.is_empty() {
        return None;
    }
    if let Some(outcome) = matches
        .iter()
        .copied()
        .find(|outcome| role.accepts(*outcome))
    {
        return Some((outcome, true));
    }
    Some((matches[0], false))
}

fn normalize_match(value: &str) -> String {
    value
        .nfkc()
        .flat_map(char::to_lowercase)
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MarkdownBlockKind {
    Paragraph,
    Heading(HeadingLevel),
}

#[derive(Debug, Clone)]
struct MarkdownBlock {
    kind: MarkdownBlockKind,
    range: Range<usize>,
    list_depth: usize,
    inline_plain: bool,
    text: String,
}

#[derive(Debug, Clone)]
struct SourceLine {
    index: usize,
    offset: usize,
    end_offset: usize,
    text: String,
    conclusion_eligible: bool,
    plain_paragraph: bool,
}

struct MarkdownDocument {
    blocks: Vec<MarkdownBlock>,
    lines: Vec<SourceLine>,
}

impl MarkdownDocument {
    fn parse(source: &str) -> Self {
        let mut options = Options::empty();
        options.insert(Options::ENABLE_TABLES);
        let mut blocks = Vec::<MarkdownBlock>::new();
        let mut active_block = None;
        let mut quote_depth = 0usize;
        let mut code_depth = 0usize;
        let mut table_depth = 0usize;
        let mut list_depth = 0usize;
        let mut tight_item_candidate = false;
        let mut tight_item_plain = true;

        for (event, range) in Parser::new_ext(source, options).into_offset_iter() {
            match event {
                Event::Start(Tag::BlockQuote(_)) => quote_depth += 1,
                Event::Start(Tag::CodeBlock(_)) => code_depth += 1,
                Event::Start(Tag::Table(_)) => table_depth += 1,
                Event::Start(Tag::List(_)) => {
                    list_depth += 1;
                    if list_depth > 1 {
                        active_block = None;
                        tight_item_candidate = false;
                    }
                }
                Event::Start(Tag::Item) => {
                    tight_item_candidate =
                        quote_depth == 0 && code_depth == 0 && table_depth == 0 && list_depth == 1;
                    tight_item_plain = true;
                }
                Event::Start(Tag::Paragraph) => {
                    tight_item_candidate = false;
                    if quote_depth == 0 && code_depth == 0 && table_depth == 0 && list_depth <= 1 {
                        blocks.push(MarkdownBlock {
                            kind: MarkdownBlockKind::Paragraph,
                            range,
                            list_depth,
                            inline_plain: true,
                            text: String::new(),
                        });
                        active_block = Some(blocks.len() - 1);
                    }
                }
                Event::Start(Tag::Heading { level, .. }) => {
                    tight_item_candidate = false;
                    if quote_depth == 0 && code_depth == 0 && table_depth == 0 && list_depth <= 1 {
                        blocks.push(MarkdownBlock {
                            kind: MarkdownBlockKind::Heading(level),
                            range,
                            list_depth,
                            inline_plain: true,
                            text: String::new(),
                        });
                        active_block = Some(blocks.len() - 1);
                    }
                }
                Event::Start(_) => {
                    if let Some(index) = active_block {
                        blocks[index].inline_plain = false;
                    } else if tight_item_candidate {
                        tight_item_plain = false;
                    }
                }
                Event::End(TagEnd::Paragraph | TagEnd::Heading(_)) => active_block = None,
                Event::End(TagEnd::BlockQuote(_)) => quote_depth = quote_depth.saturating_sub(1),
                Event::End(TagEnd::CodeBlock) => code_depth = code_depth.saturating_sub(1),
                Event::End(TagEnd::Table) => table_depth = table_depth.saturating_sub(1),
                Event::End(TagEnd::List(_)) => list_depth = list_depth.saturating_sub(1),
                Event::End(TagEnd::Item) => {
                    active_block = None;
                    tight_item_candidate = false;
                }
                Event::Text(text) => {
                    if let Some(index) = active_block {
                        blocks[index].text.push_str(&text);
                        blocks[index].range.end = blocks[index].range.end.max(range.end);
                    } else if tight_item_candidate {
                        blocks.push(MarkdownBlock {
                            kind: MarkdownBlockKind::Paragraph,
                            range,
                            list_depth,
                            inline_plain: tight_item_plain,
                            text: text.into_string(),
                        });
                        active_block = Some(blocks.len() - 1);
                    }
                }
                Event::SoftBreak | Event::HardBreak => {
                    if let Some(index) = active_block {
                        blocks[index].text.push(' ');
                    }
                }
                Event::Code(text) => {
                    if let Some(index) = active_block {
                        blocks[index].inline_plain = false;
                        blocks[index].text.push_str(&text);
                    }
                }
                Event::InlineHtml(_) | Event::Html(_) | Event::FootnoteReference(_) => {
                    if let Some(index) = active_block {
                        blocks[index].inline_plain = false;
                    }
                }
                Event::TaskListMarker(_) => {
                    if let Some(index) = active_block {
                        blocks[index].inline_plain = false;
                    }
                }
                _ => {}
            }
        }

        let lines = source_lines(source, &blocks);
        Self { blocks, lines }
    }
}

fn source_lines(source: &str, blocks: &[MarkdownBlock]) -> Vec<SourceLine> {
    let mut lines = Vec::new();
    let mut offset = 0usize;
    for (index, chunk) in source.split_inclusive('\n').enumerate() {
        let without_newline = chunk.strip_suffix('\n').unwrap_or(chunk);
        let text = without_newline
            .strip_suffix('\r')
            .unwrap_or(without_newline)
            .to_string();
        let end_offset = offset + without_newline.len();
        let block = blocks
            .iter()
            .find(|block| ranges_overlap(&block.range, offset, end_offset));
        lines.push(SourceLine {
            index,
            offset,
            end_offset,
            text,
            conclusion_eligible: block.is_some(),
            plain_paragraph: block.is_some_and(|block| {
                block.kind == MarkdownBlockKind::Paragraph
                    && block.list_depth == 0
                    && block.inline_plain
            }),
        });
        offset += chunk.len();
    }
    lines
}

fn ranges_overlap(range: &Range<usize>, line_start: usize, line_end: usize) -> bool {
    line_start < range.end && line_end > range.start
}

fn adjacent_summary(lines: &[SourceLine], line_index: usize) -> Option<String> {
    let current_position = lines.iter().position(|line| line.index == line_index)?;
    let before = lines[..current_position]
        .iter()
        .rev()
        .find(|line| !line.text.trim().is_empty());
    let after = lines[current_position + 1..]
        .iter()
        .find(|line| !line.text.trim().is_empty());
    before.into_iter().chain(after).find_map(summary_from_line)
}

fn summary_from_line(line: &SourceLine) -> Option<String> {
    if !line.plain_paragraph
        || parse_conclusion_line(CompletionRole::Reviewer, &line.text).is_some()
        || parse_conclusion_line(CompletionRole::Implementer, &line.text).is_some()
        || extract_first_markdown_path(&line.text).is_some()
    {
        return None;
    }
    bound_nonblank(&line.text)
}

fn terminal_report_hint(lines: &[SourceLine], conclusion_line_index: usize) -> Option<String> {
    let current_position = lines
        .iter()
        .position(|line| line.index == conclusion_line_index)?;
    let mut adjacent = Vec::with_capacity(2);
    if current_position > 0 {
        let before = &lines[current_position - 1];
        if before.index + 1 == conclusion_line_index {
            adjacent.push(before);
        }
    }
    if let Some(after) = lines.get(current_position + 1) {
        if conclusion_line_index + 1 == after.index {
            adjacent.push(after);
        }
    }
    adjacent
        .into_iter()
        .filter(|line| line.plain_paragraph)
        .find_map(|line| extract_first_markdown_path(&line.text))
}

fn extract_first_markdown_path(line: &str) -> Option<String> {
    line.split_whitespace().find_map(|token| {
        let candidate = token.trim_matches(|character: char| {
            matches!(
                character,
                ',' | ';' | ':' | '"' | '\'' | '(' | ')' | '[' | ']' | '{' | '}'
            )
        });
        normalize_report_path(candidate, false)
    })
}

fn report_summary_for_hint(
    input: &CompletionResolverInput,
    report_file: Option<&str>,
) -> Option<String> {
    let report_file = report_file?;
    input
        .report_candidates
        .iter()
        .chain(input.touched_report_candidates.iter())
        .find(|candidate| {
            candidate.contents.len() <= MAX_REPORT_BYTES
                && normalize_report_path(&candidate.path, true).as_deref() == Some(report_file)
        })
        .and_then(|candidate| candidate.summary.as_deref())
        .and_then(bound_nonblank)
}

fn normalize_report_path(path: &str, allow_markdown: bool) -> Option<String> {
    let path = path.trim();
    if path.is_empty()
        || path.len() > MAX_REPORT_PATH_BYTES
        || path.starts_with('/')
        || path.starts_with('\\')
        || path.contains("://")
        || path
            .as_bytes()
            .get(1)
            .is_some_and(|separator| *separator == b':')
    {
        return None;
    }

    let mut normalized = Vec::new();
    for component in path.split(['/', '\\']) {
        match component {
            "" | "." => continue,
            ".." => return None,
            value if value.contains(':') || value.contains('\0') => return None,
            value => normalized.push(value),
        }
    }
    if normalized.is_empty() {
        return None;
    }
    let path = normalized.join("/");
    let lowercase = path.to_lowercase();
    if !lowercase.ends_with(".md") && !(allow_markdown && lowercase.ends_with(".markdown")) {
        return None;
    }
    Some(path)
}

fn bound_nonblank(value: &str) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| bound_utf8(trimmed, MAX_COMPLETION_SUMMARY_BYTES))
}

fn bound_utf8(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_string();
    }
    let mut boundary = max_bytes;
    while !value.is_char_boundary(boundary) {
        boundary -= 1;
    }
    value[..boundary].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;
    use std::collections::BTreeMap;

    #[derive(Debug, Deserialize)]
    struct CompletionConclusionVectors {
        schema: String,
        terminal: Vec<ConclusionVector>,
        report: Vec<ConclusionVector>,
        grammar_matrix: GrammarMatrix,
    }

    #[derive(Debug, Deserialize)]
    struct ConclusionVector {
        role: String,
        input: String,
        expected: Option<String>,
    }

    #[derive(Debug, Deserialize)]
    struct GrammarMatrix {
        labels: Vec<String>,
        separators: Vec<String>,
        terminal_punctuation: Vec<String>,
        heading_prefixes: Vec<String>,
        list_prefixes: Vec<String>,
        bold_pairs: Vec<(String, String)>,
        reviewer_aliases: BTreeMap<String, Vec<String>>,
        producer_aliases: BTreeMap<String, Vec<String>>,
        report_section_headings: Vec<String>,
        rejected_lines: Vec<String>,
        report_hint_adjacency: ReportHintAdjacency,
    }

    #[derive(Debug, Deserialize)]
    struct ReportHintAdjacency {
        accepted_before: String,
        accepted_after: String,
        rejected_non_adjacent: String,
        rejected_non_markdown: String,
    }

    fn vectors() -> CompletionConclusionVectors {
        serde_json::from_str(include_str!("fixtures/completion_conclusion_vectors.json")).unwrap()
    }

    fn role(value: &str) -> CompletionRole {
        match value {
            "reviewer" => CompletionRole::Reviewer,
            "author" => CompletionRole::Author,
            "implementer" => CompletionRole::Implementer,
            "fixer" => CompletionRole::Fixer,
            other => panic!("unknown fixture role {other}"),
        }
    }

    fn tool(accepted_ordinal: i64, outcome: CompletionOutcome) -> CompletionToolIntent {
        CompletionToolIntent {
            accepted_ordinal,
            outcome,
            summary: None,
            report_file: None,
        }
    }

    fn report(path: &str, contents: &str) -> CompletionReportCandidate {
        CompletionReportCandidate {
            path: path.into(),
            contents: contents.into(),
            summary: None,
        }
    }

    fn terminal_input(role: CompletionRole, text: &str) -> CompletionResolverInput {
        CompletionResolverInput {
            role,
            tool_intents: Vec::new(),
            final_assistant_text: text.into(),
            report_candidates: Vec::new(),
            touched_report_candidates: Vec::new(),
        }
    }

    fn resolved(input: &CompletionResolverInput) -> CompletionIntent {
        let CompletionResolution::Resolved(intent) = resolve_completion_intent(input) else {
            panic!("expected resolved completion intent");
        };
        intent
    }

    #[test]
    fn completion_intent_vectors_match_terminal_and_report_grammars() {
        let vectors = vectors();
        assert_eq!(vectors.schema, "CompletionConclusionVectorsV1");

        for case in vectors.terminal {
            assert_eq!(
                parse_terminal_conclusions(role(&case.role), &case.input).classification(),
                case.expected.as_deref(),
                "terminal vector: {:?}",
                case.input
            );
        }
        for case in vectors.report {
            assert_eq!(
                parse_report_conclusions(role(&case.role), &case.input).classification(),
                case.expected.as_deref(),
                "report vector: {:?}",
                case.input
            );
        }
    }

    #[test]
    fn grammar_matrix_is_normative_for_all_tokens_and_aliases() {
        let matrix = vectors().grammar_matrix;
        let wrapper_sets = [&matrix.heading_prefixes, &matrix.list_prefixes];
        let role_aliases = [
            (CompletionRole::Reviewer, &matrix.reviewer_aliases),
            (CompletionRole::Implementer, &matrix.producer_aliases),
        ];

        for (role, aliases) in role_aliases {
            for (expected, values) in aliases {
                for alias in values {
                    for label in &matrix.labels {
                        for separator in &matrix.separators {
                            for punctuation in &matrix.terminal_punctuation {
                                for wrappers in wrapper_sets {
                                    for prefix in wrappers {
                                        for (bold_open, bold_close) in &matrix.bold_pairs {
                                            let line = format!(
                                                "{prefix}{bold_open}{label} {separator} {alias}{punctuation}{bold_close}"
                                            );
                                            assert_eq!(
                                                parse_terminal_conclusions(role, &line)
                                                    .classification(),
                                                Some(expected.as_str()),
                                                "matrix line: {line:?}"
                                            );
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        for line in matrix.rejected_lines {
            assert_eq!(
                parse_terminal_conclusions(CompletionRole::Reviewer, &line).classification(),
                None,
                "rejected matrix line: {line:?}"
            );
        }

        for heading in matrix.report_section_headings {
            let report = format!("{heading}\n\napprove\n");
            assert_eq!(
                parse_report_conclusions(CompletionRole::Reviewer, &report).classification(),
                Some("approve"),
                "report heading: {heading:?}"
            );
        }
    }

    #[test]
    fn terminal_parser_uses_nfkc_case_and_whitespace_without_changing_excerpts() {
        let parsed = parse_terminal_conclusions(
            CompletionRole::Reviewer,
            "ＣＯＮＣＬＵＳＩＯＮ：  APPROVE   WITH   MINORS！",
        );
        assert_eq!(parsed.classification(), Some("approve_with_minors"));
        assert_eq!(
            parsed.authoritative.unwrap().excerpt,
            "ＣＯＮＣＬＵＳＩＯＮ：  APPROVE   WITH   MINORS！"
        );
    }

    #[test]
    fn terminal_parser_ignores_non_top_level_markdown_contexts() {
        for text in [
            "```text\nConclusion: approve\n```",
            "> Conclusion: approve",
            "<!-- Conclusion: approve -->",
            "| Conclusion | approve |\n| --- | --- |",
            "- parent\n  - Conclusion: approve",
            "    Conclusion: approve",
        ] {
            assert_eq!(
                parse_terminal_conclusions(CompletionRole::Reviewer, text).classification(),
                None,
                "context: {text:?}"
            );
        }
    }

    #[test]
    fn last_eligible_terminal_line_is_authoritative_even_when_role_incompatible() {
        let input = terminal_input(
            CompletionRole::Reviewer,
            "Conclusion: approve\nConclusion: done",
        );
        let CompletionResolution::NeedsDecision {
            reason_code,
            bounded_candidates,
            diagnostics,
        } = resolve_completion_intent(&input)
        else {
            panic!("role mismatch must need one user decision");
        };
        assert_eq!(reason_code, CompletionIntentReason::RoleMismatch);
        assert_eq!(bounded_candidates.len(), 2);
        assert!(diagnostics.iter().all(|diagnostic| diagnostic
            .excerpt
            .as_ref()
            .is_none_or(|value| value.len() <= MAX_COMPLETION_EXCERPT_BYTES)));
    }

    #[test]
    fn bounded_terminal_candidates_keep_the_authoritative_last_line() {
        let mut lines = vec!["Conclusion: approve"; MAX_COMPLETION_CANDIDATES + 1];
        lines.push("Conclusion: done");
        let input = terminal_input(CompletionRole::Reviewer, &lines.join("\n"));
        let CompletionResolution::NeedsDecision {
            reason_code,
            bounded_candidates,
            ..
        } = resolve_completion_intent(&input)
        else {
            panic!("role mismatch must need a decision");
        };
        assert_eq!(reason_code, CompletionIntentReason::RoleMismatch);
        assert_eq!(bounded_candidates.len(), MAX_COMPLETION_CANDIDATES);
        assert_eq!(
            bounded_candidates
                .last()
                .map(|candidate| candidate.excerpt.as_str()),
            Some("Conclusion: done")
        );
    }

    #[test]
    fn resolver_uses_strict_channel_precedence_and_last_in_source() {
        let input = CompletionResolverInput {
            role: CompletionRole::Reviewer,
            tool_intents: vec![
                tool(1, CompletionOutcome::RequestChanges),
                tool(2, CompletionOutcome::Approve),
            ],
            final_assistant_text: "Conclusion: block\nConclusion: request changes".into(),
            report_candidates: vec![report("a.md", "Conclusion: block")],
            touched_report_candidates: Vec::new(),
        };
        let intent = resolved(&input);
        assert_eq!(intent.outcome, CompletionOutcome::Approve);
        assert_eq!(intent.source, CompletionIntentSource::CompleteWork);
    }

    #[test]
    fn incompatible_latest_tool_intent_does_not_fall_through() {
        let input = CompletionResolverInput {
            role: CompletionRole::Author,
            tool_intents: vec![tool(1, CompletionOutcome::Approve)],
            final_assistant_text: "Conclusion: done".into(),
            report_candidates: Vec::new(),
            touched_report_candidates: Vec::new(),
        };
        let CompletionResolution::NeedsDecision { reason_code, .. } =
            resolve_completion_intent(&input)
        else {
            panic!("tool role mismatch must not fall through");
        };
        assert_eq!(reason_code, CompletionIntentReason::RoleMismatch);
    }

    #[test]
    fn conflicting_reports_need_one_user_decision_and_equivalent_reports_coalesce() {
        let conflicting = CompletionResolverInput {
            role: CompletionRole::Reviewer,
            tool_intents: Vec::new(),
            final_assistant_text: "Reports: [A](a.md), [B](b.md)".into(),
            report_candidates: vec![
                report("a.md", "Conclusion: block"),
                report("b.md", "Conclusion: request changes"),
            ],
            touched_report_candidates: Vec::new(),
        };
        let CompletionResolution::NeedsDecision {
            reason_code,
            bounded_candidates,
            ..
        } = resolve_completion_intent(&conflicting)
        else {
            panic!("cross-file conflict must not resolve");
        };
        assert_eq!(reason_code, CompletionIntentReason::Conflict);
        assert_eq!(bounded_candidates.len(), 2);

        let equivalent = CompletionResolverInput {
            report_candidates: vec![
                report("a.md", "Conclusion: approve"),
                report("b.md", "# Conclusion\n\napproved"),
            ],
            ..conflicting
        };
        let intent = resolved(&equivalent);
        assert_eq!(intent.outcome, CompletionOutcome::Approve);
        assert_eq!(intent.source, CompletionIntentSource::Report);
        assert_eq!(intent.report_file.as_deref(), Some("a.md"));
    }

    #[test]
    fn conflicting_reports_expose_one_authoritative_candidate_per_file() {
        let input = CompletionResolverInput {
            role: CompletionRole::Reviewer,
            tool_intents: Vec::new(),
            final_assistant_text: String::new(),
            report_candidates: vec![
                report("a.md", "Conclusion: request changes\nConclusion: approve"),
                report("b.md", "Conclusion: block"),
            ],
            touched_report_candidates: Vec::new(),
        };
        let CompletionResolution::NeedsDecision {
            reason_code,
            bounded_candidates,
            ..
        } = resolve_completion_intent(&input)
        else {
            panic!("cross-file conflict must need a decision");
        };
        assert_eq!(reason_code, CompletionIntentReason::Conflict);
        assert_eq!(bounded_candidates.len(), 2);
        assert_eq!(bounded_candidates[0].outcome, CompletionOutcome::Approve);
        assert_eq!(bounded_candidates[1].outcome, CompletionOutcome::Block);
    }

    #[test]
    fn report_uses_last_eligible_conclusion_and_first_plain_section_paragraph() {
        let parsed = parse_report_conclusions(
            CompletionRole::Reviewer,
            "# Conclusion\n\napprove\n\nDetails.\n\nConclusion: request changes\n",
        );
        assert_eq!(parsed.classification(), Some("request_changes"));

        for rejected in [
            "# Conclusion\n\nDetails first.\n\napprove\n",
            "# Conclusion\n\n> approve\n",
            "# Conclusion\n\n- approve\n",
            "# Conclusion\n\n`approve`\n",
        ] {
            assert_eq!(
                parse_report_conclusions(CompletionRole::Reviewer, rejected).classification(),
                None,
                "report section: {rejected:?}"
            );
        }
    }

    #[test]
    fn text_report_hint_requires_plain_adjacent_workspace_markdown_line() {
        let adjacency = vectors().grammar_matrix.report_hint_adjacency;
        for accepted in [adjacency.accepted_before, adjacency.accepted_after] {
            assert_eq!(
                resolved(&terminal_input(CompletionRole::Reviewer, &accepted))
                    .report_file
                    .as_deref(),
                Some("reports/review.md")
            );
        }
        for rejected in [
            adjacency.rejected_non_adjacent,
            adjacency.rejected_non_markdown,
            "Conclusion: approve\nReport: ../review.md".into(),
            "Conclusion: approve\n> Report: reports/review.md".into(),
            "Conclusion: approve\n`reports/review.md`".into(),
        ] {
            assert_eq!(
                resolved(&terminal_input(CompletionRole::Reviewer, &rejected)).report_file,
                None,
                "report hint: {rejected:?}"
            );
        }
    }

    #[test]
    fn summary_fallback_order_is_tool_adjacent_report_then_outcome_label() {
        let mut tool_intent = tool(1, CompletionOutcome::Done);
        tool_intent.summary = Some("tool summary".into());
        tool_intent.report_file = Some("reports/tool.md".into());
        let tool_input = CompletionResolverInput {
            role: CompletionRole::Implementer,
            tool_intents: vec![tool_intent],
            final_assistant_text: "terminal summary\nConclusion: blocked".into(),
            report_candidates: vec![report("reports/tool.md", "Conclusion: blocked")],
            touched_report_candidates: Vec::new(),
        };
        let tool_result = resolved(&tool_input);
        assert_eq!(tool_result.summary.as_deref(), Some("tool summary"));
        assert_eq!(tool_result.report_file.as_deref(), Some("reports/tool.md"));

        let terminal_result = resolved(&terminal_input(
            CompletionRole::Implementer,
            "Implementation and tests are complete.\nConclusion: done",
        ));
        assert_eq!(
            terminal_result.summary.as_deref(),
            Some("Implementation and tests are complete.")
        );

        let mut report_candidate = report("reports/task.md", "# Conclusion\n\ndone\n");
        report_candidate.summary = Some("report summary".into());
        let report_result = resolved(&CompletionResolverInput {
            role: CompletionRole::Implementer,
            tool_intents: Vec::new(),
            final_assistant_text: String::new(),
            report_candidates: vec![report_candidate],
            touched_report_candidates: Vec::new(),
        });
        assert_eq!(report_result.summary.as_deref(), Some("report summary"));

        let label_result = resolved(&terminal_input(
            CompletionRole::Reviewer,
            "Conclusion: approve with minors",
        ));
        assert_eq!(label_result.summary.as_deref(), Some("approve_with_minors"));
    }

    #[test]
    fn tool_summary_falls_back_to_the_conclusion_adjacent_paragraph() {
        let mut tool_intent = tool(1, CompletionOutcome::Done);
        tool_intent.report_file = Some("reports/tool.md".into());
        let mut report_candidate = report("reports/tool.md", "Conclusion: blocked");
        report_candidate.summary = Some("report summary".into());
        let result = resolved(&CompletionResolverInput {
            role: CompletionRole::Implementer,
            tool_intents: vec![tool_intent],
            final_assistant_text: "terminal summary\nConclusion: blocked".into(),
            report_candidates: vec![report_candidate],
            touched_report_candidates: Vec::new(),
        });

        assert_eq!(result.outcome, CompletionOutcome::Done);
        assert_eq!(result.summary.as_deref(), Some("terminal summary"));
    }

    #[test]
    fn adjacent_summary_does_not_cross_a_non_plain_markdown_block() {
        let result = resolved(&terminal_input(
            CompletionRole::Implementer,
            "stale summary\n\n> quoted interruption\n\nConclusion: done",
        ));

        assert_eq!(result.summary.as_deref(), Some("done"));
    }

    #[test]
    fn invalid_report_bounds_and_paths_never_produce_a_pass() {
        let base = CompletionResolverInput {
            role: CompletionRole::Reviewer,
            tool_intents: Vec::new(),
            final_assistant_text: String::new(),
            report_candidates: Vec::new(),
            touched_report_candidates: Vec::new(),
        };
        for candidate in [
            report("../outside.md", "Conclusion: approve"),
            report("C:/outside.md", "Conclusion: approve"),
            report("report.txt", "Conclusion: approve"),
            report(
                "oversized.md",
                &format!("Conclusion: approve\n{}", "x".repeat(MAX_REPORT_BYTES)),
            ),
        ] {
            let CompletionResolution::NeedsDecision { reason_code, .. } =
                resolve_completion_intent(&CompletionResolverInput {
                    report_candidates: vec![candidate],
                    ..base.clone()
                })
            else {
                panic!("invalid report input must not pass");
            };
            assert_eq!(reason_code, CompletionIntentReason::Missing);
        }

        let CompletionResolution::NeedsDecision { diagnostics, .. } =
            resolve_completion_intent(&CompletionResolverInput {
                report_candidates: (0..=MAX_REPORT_CANDIDATES)
                    .map(|index| report(&format!("{index}.md"), "Conclusion: approve"))
                    .collect(),
                ..base
            })
        else {
            panic!("too many report candidates must fail closed");
        };
        assert!(diagnostics.len() <= MAX_COMPLETION_DIAGNOSTICS);
    }

    #[test]
    fn missing_intent_needs_decision_with_bounded_diagnostics() {
        let input = terminal_input(
            CompletionRole::Reviewer,
            &format!(
                "ordinary prose {}",
                "x".repeat(MAX_COMPLETION_EXCERPT_BYTES * 4)
            ),
        );
        let CompletionResolution::NeedsDecision {
            reason_code,
            bounded_candidates,
            diagnostics,
        } = resolve_completion_intent(&input)
        else {
            panic!("missing intent must need a decision");
        };
        assert_eq!(reason_code, CompletionIntentReason::Missing);
        assert!(bounded_candidates.is_empty());
        assert!(diagnostics.len() <= MAX_COMPLETION_DIAGNOSTICS);
        assert!(diagnostics.iter().all(|diagnostic| diagnostic
            .excerpt
            .as_ref()
            .is_none_or(|value| value.len() <= MAX_COMPLETION_EXCERPT_BYTES)));
    }

    #[test]
    fn conclusion_suffix_is_role_specific_and_uses_parser_aliases() {
        let reviewer = build_conclusion_suffix(CompletionRole::Reviewer);
        assert!(reviewer.contains(
            "Conclusion: approve | approve with minor issues | request changes | blocked"
        ));
        assert!(!reviewer.contains("done with concerns"));

        for role in [
            CompletionRole::Author,
            CompletionRole::Implementer,
            CompletionRole::Fixer,
        ] {
            let producer = build_conclusion_suffix(role);
            assert!(producer.contains("Conclusion: done | done with concerns | blocked"));
            assert!(!producer.contains("request changes"));
        }

        assert_eq!(
            parse_terminal_conclusions(CompletionRole::Reviewer, "Conclusion: 阻塞")
                .classification(),
            Some("block")
        );
        assert_eq!(
            parse_terminal_conclusions(CompletionRole::Implementer, "Conclusion: 阻塞")
                .classification(),
            Some("blocked")
        );
    }
}
