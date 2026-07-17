//! Authoritative field matching, regex ranking, and resource URI codecs.
//!
//! File / conversation / commit search and candidate validation all call
//! [`match_reference_candidate`] so primary/secondary field order stays in
//! one place.

use std::path::Path;

use regex::RegexBuilder;

use super::types::{
    MatchReferenceRegexRequest, ReferenceCandidate, ReferenceCandidateMetadata,
    ReferenceDescriptor, ReferenceFieldMatch, ReferenceRegexMatch, ReferenceRegexRank,
    ReferenceSearchError,
};

/// Maximum UTF-8 length of a literal query (bytes).
const MAX_LITERAL_QUERY_BYTES: usize = 512;
/// Maximum UTF-8 length of the body after the `re:` prefix.
const MAX_REGEX_BODY_BYTES: usize = 256;

/// Descriptor batch / row limits for the bulk regex helper.
const MAX_DESCRIPTORS: usize = 1_024;
const MAX_DESCRIPTOR_ID_BYTES: usize = 1_024;
const MAX_FIELD_SLOTS: usize = 1_024;
const MAX_SEARCHABLE_BYTES: usize = 4_096;

/// Parsed search pattern: literal (Unicode-lowercase comparison, raw identity
/// preserved) or compiled regex (`re:` prefix).
#[derive(Debug, Clone)]
pub enum SearchPattern {
    Literal { raw: String, lowered: String },
    Regex { raw: String, regex: regex::Regex },
}

impl SearchPattern {
    pub fn parse(query: &str) -> Result<Self, ReferenceSearchError> {
        if query.is_empty() {
            return Err(ReferenceSearchError::invalid_request(
                "reference search query must not be empty",
            ));
        }

        if let Some(body) = query.strip_prefix("re:") {
            if body.is_empty() || body.len() > MAX_REGEX_BODY_BYTES {
                return Err(ReferenceSearchError::invalid_pattern(format!(
                    "regex body must be 1..={MAX_REGEX_BODY_BYTES} UTF-8 bytes"
                )));
            }
            let regex = RegexBuilder::new(body)
                .size_limit(1024 * 1024)
                .build()
                .map_err(|error| ReferenceSearchError::invalid_pattern(error.to_string()))?;
            return Ok(SearchPattern::Regex {
                raw: query.to_string(),
                regex,
            });
        }

        if query.len() > MAX_LITERAL_QUERY_BYTES {
            return Err(ReferenceSearchError::invalid_request(format!(
                "literal query must be at most {MAX_LITERAL_QUERY_BYTES} UTF-8 bytes"
            )));
        }

        Ok(SearchPattern::Literal {
            raw: query.to_string(),
            lowered: query.to_lowercase(),
        })
    }

    pub fn raw(&self) -> &str {
        match self {
            SearchPattern::Literal { raw, .. } | SearchPattern::Regex { raw, .. } => raw,
        }
    }

    pub fn is_regex(&self) -> bool {
        matches!(self, SearchPattern::Regex { .. })
    }
}

/// Match `pattern` against declared primary then secondary fields.
///
/// Literal tiers: exact primary=0, prefix=1, word-boundary=2, substring=3,
/// any secondary=4. Regex `field_tier` is the flattened field ordinal
/// (primary first, then secondary offset by `primary.len()`).
pub fn match_fields(
    pattern: &SearchPattern,
    primary: &[&str],
    secondary: &[&str],
) -> Option<ReferenceFieldMatch> {
    match pattern {
        SearchPattern::Literal { lowered, .. } => {
            if lowered.is_empty() {
                return None;
            }
            let mut best: Option<(u32, usize)> = None;
            for (index, field) in primary.iter().enumerate() {
                if let Some(tier) = literal_primary_tier(lowered, field) {
                    consider_literal(&mut best, tier, index);
                }
            }
            for (index, field) in secondary.iter().enumerate() {
                if field.to_lowercase().contains(lowered.as_str()) {
                    consider_literal(&mut best, 4, index);
                }
            }
            best.map(|(field_tier, _)| ReferenceFieldMatch {
                field_tier,
                regex_rank: None,
            })
        }
        SearchPattern::Regex { regex, .. } => {
            let mut best: Option<ReferenceRegexRank> = None;
            for (index, field) in primary.iter().enumerate() {
                consider_regex_field(&mut best, regex, field, index as u32);
            }
            let primary_len = primary.len() as u32;
            for (index, field) in secondary.iter().enumerate() {
                let field_tier = primary_len.saturating_add(index as u32);
                consider_regex_field(&mut best, regex, field, field_tier);
            }
            best.map(|rank| ReferenceFieldMatch {
                field_tier: rank.field_tier,
                regex_rank: Some(rank),
            })
        }
    }
}

/// One resource-field mapping used by file / conversation / commit search and
/// validation. Declared field order is authoritative.
pub fn match_reference_candidate(
    pattern: &SearchPattern,
    candidate: &ReferenceCandidate,
) -> Option<ReferenceFieldMatch> {
    let (primary, secondary) = candidate_field_strings(candidate);
    let primary_refs: Vec<&str> = primary.iter().map(String::as_str).collect();
    let secondary_refs: Vec<&str> = secondary.iter().map(String::as_str).collect();
    match_fields(pattern, &primary_refs, &secondary_refs)
}

/// Declared primary/secondary strings for a candidate. Absent conversation
/// branch contributes `""` so project field tiers stay stable.
pub fn candidate_field_strings(candidate: &ReferenceCandidate) -> (Vec<String>, Vec<String>) {
    match &candidate.metadata {
        ReferenceCandidateMetadata::File { relative_path, .. } => {
            (vec![candidate.label.clone()], vec![relative_path.clone()])
        }
        ReferenceCandidateMetadata::Conversation {
            agent_type,
            status,
            branch,
            project_name,
            project_path,
            ..
        } => {
            let agent_wire = agent_type_snake_case(*agent_type);
            (
                vec![candidate.label.clone()],
                vec![
                    candidate.id.clone(),
                    agent_wire,
                    status.clone(),
                    branch.clone().unwrap_or_default(),
                    project_name.clone(),
                    project_path.clone(),
                ],
            )
        }
        ReferenceCandidateMetadata::Commit {
            short_hash,
            full_hash,
            subject,
            message,
            author,
            ..
        } => (
            vec![short_hash.clone(), full_hash.clone(), subject.clone()],
            vec![message.clone(), author.clone()],
        ),
    }
}

fn agent_type_snake_case(agent_type: crate::models::agent::AgentType) -> String {
    // AgentType is `rename_all = "snake_case"` on the wire.
    serde_json::to_value(agent_type)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_else(|| format!("{agent_type:?}").to_lowercase())
}

fn consider_literal(best: &mut Option<(u32, usize)>, tier: u32, declared_index: usize) {
    let key = (tier, declared_index);
    match best {
        Some(current) if key >= *current => {}
        _ => *best = Some(key),
    }
}

fn literal_primary_tier(pattern_lowered: &str, field: &str) -> Option<u32> {
    let field_lowered = field.to_lowercase();
    if field_lowered == pattern_lowered {
        return Some(0);
    }
    if field_lowered.starts_with(pattern_lowered) {
        return Some(1);
    }
    let mut search_from = 0;
    let mut found_any = false;
    while search_from <= field_lowered.len() {
        let rest = &field_lowered[search_from..];
        let Some(rel) = rest.find(pattern_lowered) else {
            break;
        };
        let abs = search_from + rel;
        found_any = true;
        if is_word_boundary_at(&field_lowered, abs) {
            return Some(2);
        }
        // Advance by one scalar so overlapping candidates are still found.
        let advance = field_lowered[abs..]
            .chars()
            .next()
            .map(|c| c.len_utf8())
            .unwrap_or(1);
        search_from = abs + advance;
    }
    if found_any {
        Some(3)
    } else {
        None
    }
}

fn is_word_boundary_at(field: &str, byte_index: usize) -> bool {
    if byte_index == 0 {
        return true;
    }
    let Some(prev) = field[..byte_index].chars().next_back() else {
        return true;
    };
    !prev.is_alphanumeric() && prev != '_'
}

fn consider_regex_field(
    best: &mut Option<ReferenceRegexRank>,
    regex: &regex::Regex,
    field: &str,
    field_tier: u32,
) {
    let Some(m) = regex.find(field) else {
        return;
    };
    let Ok(start) = u32::try_from(m.start()) else {
        return;
    };
    let Ok(length) = u32::try_from(m.len()) else {
        return;
    };
    let rank = ReferenceRegexRank {
        field_tier,
        start,
        length,
    };
    let better = match best {
        None => true,
        Some(current) => {
            (rank.field_tier, rank.start, rank.length)
                < (current.field_tier, current.start, current.length)
        }
    };
    if better {
        *best = Some(rank);
    }
}

/// Bulk regex ranking over caller-provided descriptors (cache / UI helper).
///
/// Accepts at most 1,024 rows; each row requires a unique non-empty ID of at
/// most 1,024 UTF-8 bytes, 1..=1,024 combined field slots, and at most 4,096
/// searchable UTF-8 bytes. Never truncates matches within an accepted batch.
pub fn match_reference_regex(
    request: MatchReferenceRegexRequest,
) -> Result<Vec<ReferenceRegexMatch>, ReferenceSearchError> {
    validate_descriptor_batch(&request.descriptors)?;
    let pattern = SearchPattern::parse(&request.query)?;
    if !pattern.is_regex() {
        return Err(ReferenceSearchError::invalid_request(
            "match_reference_regex requires a re: query",
        ));
    }

    let mut out = Vec::new();
    for descriptor in &request.descriptors {
        let primary_refs: Vec<&str> = descriptor.primary.iter().map(String::as_str).collect();
        let secondary_refs: Vec<&str> = descriptor.secondary.iter().map(String::as_str).collect();
        if let Some(m) = match_fields(&pattern, &primary_refs, &secondary_refs) {
            if let Some(rank) = m.regex_rank {
                out.push(ReferenceRegexMatch {
                    id: descriptor.id.clone(),
                    source_ordinal: descriptor.source_ordinal,
                    rank,
                });
            }
        }
    }
    Ok(out)
}

fn validate_descriptor_batch(
    descriptors: &[ReferenceDescriptor],
) -> Result<(), ReferenceSearchError> {
    if descriptors.len() > MAX_DESCRIPTORS {
        return Err(ReferenceSearchError::invalid_request(format!(
            "at most {MAX_DESCRIPTORS} descriptors allowed"
        )));
    }

    let mut seen_ids = std::collections::HashSet::with_capacity(descriptors.len());
    for descriptor in descriptors {
        if descriptor.id.is_empty() || descriptor.id.len() > MAX_DESCRIPTOR_ID_BYTES {
            return Err(ReferenceSearchError::invalid_request(
                "descriptor id must be non-empty and at most 1024 UTF-8 bytes",
            ));
        }
        if !seen_ids.insert(descriptor.id.as_str()) {
            return Err(ReferenceSearchError::invalid_request(format!(
                "duplicate descriptor id: {}",
                descriptor.id
            )));
        }
        if u32::try_from(descriptor.source_ordinal).is_err() {
            return Err(ReferenceSearchError::invalid_request(format!(
                "source_ordinal does not fit u32: {}",
                descriptor.source_ordinal
            )));
        }

        let slot_count = descriptor.primary.len() + descriptor.secondary.len();
        if slot_count == 0 || slot_count > MAX_FIELD_SLOTS {
            return Err(ReferenceSearchError::invalid_request(format!(
                "descriptor requires 1..={MAX_FIELD_SLOTS} combined field slots"
            )));
        }

        let searchable: usize = descriptor
            .primary
            .iter()
            .chain(descriptor.secondary.iter())
            .map(|s| s.len())
            .sum();
        if searchable > MAX_SEARCHABLE_BYTES {
            return Err(ReferenceSearchError::invalid_request(format!(
                "descriptor searchable bytes must be at most {MAX_SEARCHABLE_BYTES}"
            )));
        }
    }
    Ok(())
}

// ─── URI codecs ───────────────────────────────────────────────────────────

/// JavaScript `encodeURIComponent` over UTF-8 bytes: unescaped ASCII set is
/// `A-Z a-z 0-9 - _ . ! ~ * ' ( )`, percent encodings uppercase `%HH`.
pub fn encode_uri_component(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for &byte in value.as_bytes() {
        match byte {
            b'A'..=b'Z'
            | b'a'..=b'z'
            | b'0'..=b'9'
            | b'-'
            | b'_'
            | b'.'
            | b'!'
            | b'~'
            | b'*'
            | b'\''
            | b'('
            | b')' => out.push(byte as char),
            _ => {
                use std::fmt::Write as _;
                let _ = write!(out, "%{byte:02X}");
            }
        }
    }
    out
}

/// Normalize path separators and strip Windows verbatim (`\\?\`) prefixes
/// before URI classification.
fn normalize_path_for_uri(path: &Path) -> String {
    let mut normalized = path.to_string_lossy().replace('\\', "/");
    // `//?/C:/...` → `C:/...`
    if let Some(rest) = normalized.strip_prefix("//?/") {
        if rest.len() >= 2 {
            let bytes = rest.as_bytes();
            // Drive form: `C:/...`
            if bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
                normalized = rest.to_string();
            } else if let Some(unc) = strip_verbatim_unc_prefix(rest) {
                // `//?/UNC/server/share/...` → `//server/share/...`
                normalized = format!("//{unc}");
            }
        }
    }
    normalized
}

fn strip_verbatim_unc_prefix(rest: &str) -> Option<&str> {
    // Accept `UNC/` case-insensitively.
    let prefix_len = 4; // "UNC/"
    if rest.len() < prefix_len {
        return None;
    }
    let (head, tail) = rest.split_at(prefix_len);
    if head.eq_ignore_ascii_case("UNC/") {
        Some(tail)
    } else {
        None
    }
}

/// Segment-encode an absolute path into a `file://` URI matching the frontend
/// `buildFileUri` codec (POSIX triple-slash, Windows drive triple-slash with
/// encoded colon, UNC authority form).
pub fn build_file_uri(path: &Path) -> String {
    let normalized = normalize_path_for_uri(path);
    if normalized.starts_with("//") {
        let encoded = normalized[2..]
            .split('/')
            .map(encode_uri_component)
            .collect::<Vec<_>>()
            .join("/");
        return format!("file://{encoded}");
    }
    let encoded = normalized
        .split('/')
        .map(encode_uri_component)
        .collect::<Vec<_>>()
        .join("/");
    if normalized.starts_with('/') {
        format!("file://{encoded}")
    } else {
        format!("file:///{encoded}")
    }
}

/// `codeg://session/<positive database id>`.
pub fn build_session_uri(conversation_id: i32) -> String {
    assert!(
        conversation_id > 0,
        "build_session_uri requires a positive database id"
    );
    format!("codeg://session/{conversation_id}")
}

/// `codeg://commit/<encodeURIComponent(canonical_repo)>@<full_hash>`.
pub fn build_commit_uri(canonical_repo: &str, full_hash: &str) -> String {
    format!(
        "codeg://commit/{}@{}",
        encode_uri_component(canonical_repo),
        full_hash
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::agent::AgentType;
    use crate::reference_search::types::{
        ReferenceCandidate, ReferenceCandidateMetadata, ReferenceFileKind, ReferenceSearchSource,
    };

    fn file_candidate(label: &str, relative_path: &str) -> ReferenceCandidate {
        ReferenceCandidate {
            source: ReferenceSearchSource::File,
            uri: build_file_uri(Path::new(&format!("/repo/{relative_path}"))),
            id: relative_path.to_string(),
            label: label.to_string(),
            detail: Some(relative_path.to_string()),
            keywords: String::new(),
            metadata: ReferenceCandidateMetadata::File {
                canonical_workspace_root: "/repo".to_string(),
                relative_path: relative_path.to_string(),
                entry_kind: ReferenceFileKind::File,
            },
            source_ordinal: 0,
            regex_rank: None,
        }
    }

    fn conversation_candidate() -> ReferenceCandidate {
        ReferenceCandidate {
            source: ReferenceSearchSource::Conversation,
            uri: build_session_uri(7),
            id: "7".to_string(),
            label: "Weekly report".to_string(),
            detail: None,
            keywords: String::new(),
            metadata: ReferenceCandidateMetadata::Conversation {
                conversation_id: 7,
                agent_type: AgentType::ClaudeCode,
                status: "idle".to_string(),
                branch: Some("main".to_string()),
                project_name: "codeg".to_string(),
                project_path: "/repo/codeg".to_string(),
            },
            source_ordinal: 1,
            regex_rank: None,
        }
    }

    fn commit_candidate() -> ReferenceCandidate {
        ReferenceCandidate {
            source: ReferenceSearchSource::Commit,
            uri: build_commit_uri("/repo", "abcdef0123456789"),
            id: "abcdef0123456789".to_string(),
            label: "abcdef0".to_string(),
            detail: Some("fix matcher".to_string()),
            keywords: String::new(),
            metadata: ReferenceCandidateMetadata::Commit {
                canonical_repo: "/repo".to_string(),
                full_hash: "abcdef0123456789".to_string(),
                short_hash: "abcdef0".to_string(),
                subject: "fix matcher".to_string(),
                message: "fix matcher\n\nbody mentions author-only-token never".to_string(),
                author: "Ada Lovelace".to_string(),
                authored_at: "2026-01-01T00:00:00Z".to_string(),
            },
            source_ordinal: 2,
            regex_rank: None,
        }
    }

    #[test]
    fn best_field_rank_cannot_be_replaced_by_secondary_match() {
        let pattern = SearchPattern::parse("read").unwrap();
        let rank = match_fields(&pattern, &["README.md"], &["project/read archive"]).expect("match");
        assert_eq!(rank.field_tier, 1);
    }

    #[test]
    fn resource_candidates_use_the_approved_fields_in_declared_order() {
        // File: relative_path is secondary → tier 4 when only it matches.
        let file = file_candidate("app.ts", "src/unique-file-path-token/app.ts");
        let pattern = SearchPattern::parse("unique-file-path-token").unwrap();
        let m = match_reference_candidate(&pattern, &file).expect("file path match");
        assert_eq!(m.field_tier, 4);

        // Conversation: project_name is secondary index 4 → tier 4.
        let conversation = conversation_candidate();
        let pattern = SearchPattern::parse("codeg").unwrap();
        // label is "Weekly report", id "7", agent "claude_code", status "idle",
        // branch "main", project_name "codeg", project_path "/repo/codeg".
        // "codeg" hits project_name (and project_path); both secondary → tier 4.
        let m = match_reference_candidate(&pattern, &conversation).expect("project match");
        assert_eq!(m.field_tier, 4);

        // Commit: author is secondary → tier 4; subject is primary index 2.
        let commit = commit_candidate();
        let pattern = SearchPattern::parse("Ada").unwrap();
        let m = match_reference_candidate(&pattern, &commit).expect("author match");
        assert_eq!(m.field_tier, 4);

        let pattern = SearchPattern::parse("matcher").unwrap();
        let m = match_reference_candidate(&pattern, &commit).expect("subject match");
        // subject is primary[2]; "matcher" is substring of "fix matcher" at a
        // word boundary after space → tier 2 (not secondary message).
        assert!(m.field_tier <= 3, "subject is primary, got tier {}", m.field_tier);

        // Regex flattened ordinals: File primary=0 secondary=1;
        // Conversation primary=0, secondary id=1..project_path=6;
        // Commit short=0 full=1 subject=2 message=3 author=4.
        let file_re = SearchPattern::parse("re:unique-file-path-token").unwrap();
        let m = match_reference_candidate(&file_re, &file).expect("file regex");
        assert_eq!(m.field_tier, 1);
        assert_eq!(m.regex_rank.as_ref().unwrap().field_tier, 1);

        let conv_re = SearchPattern::parse("re:codeg").unwrap();
        let m = match_reference_candidate(&conv_re, &conversation).expect("conv regex");
        // project_name is secondary index 4 → flattened 1+4=5; project_path is 6.
        // leftmost best is project_name at tier 5.
        assert_eq!(m.regex_rank.as_ref().unwrap().field_tier, 5);

        let commit_author_re = SearchPattern::parse("re:Ada").unwrap();
        let m = match_reference_candidate(&commit_author_re, &commit).expect("author regex");
        assert_eq!(m.regex_rank.as_ref().unwrap().field_tier, 4);

        let commit_subject_re = SearchPattern::parse("re:matcher").unwrap();
        let m = match_reference_candidate(&commit_subject_re, &commit).expect("subject regex");
        // subject (2) wins over message (3).
        assert_eq!(m.regex_rank.as_ref().unwrap().field_tier, 2);
    }

    #[test]
    fn descriptor_batch_accepts_exact_boundaries_and_rejects_one_over() {
        // Max descriptors (1024) with minimal valid rows.
        let mut descriptors = Vec::with_capacity(MAX_DESCRIPTORS);
        for i in 0..MAX_DESCRIPTORS {
            descriptors.push(ReferenceDescriptor {
                id: format!("id-{i}"),
                source_ordinal: i as u64,
                primary: vec!["a".to_string()],
                secondary: vec![],
            });
        }
        assert!(match_reference_regex(MatchReferenceRegexRequest {
            query: "re:a".to_string(),
            descriptors: descriptors.clone(),
        })
        .is_ok());

        descriptors.push(ReferenceDescriptor {
            id: "overflow".to_string(),
            source_ordinal: MAX_DESCRIPTORS as u64,
            primary: vec!["a".to_string()],
            secondary: vec![],
        });
        assert!(matches!(
            match_reference_regex(MatchReferenceRegexRequest {
                query: "re:a".to_string(),
                descriptors,
            })
            .unwrap_err()
            .code,
            crate::app_error::AppErrorCode::InvalidRequest
        ));

        // ID length boundary.
        let ok_id = "x".repeat(MAX_DESCRIPTOR_ID_BYTES);
        assert!(match_reference_regex(MatchReferenceRegexRequest {
            query: "re:a".to_string(),
            descriptors: vec![ReferenceDescriptor {
                id: ok_id,
                source_ordinal: 0,
                primary: vec!["a".to_string()],
                secondary: vec![],
            }],
        })
        .is_ok());
        assert!(matches!(
            match_reference_regex(MatchReferenceRegexRequest {
                query: "re:a".to_string(),
                descriptors: vec![ReferenceDescriptor {
                    id: "x".repeat(MAX_DESCRIPTOR_ID_BYTES + 1),
                    source_ordinal: 0,
                    primary: vec!["a".to_string()],
                    secondary: vec![],
                }],
            })
            .unwrap_err()
            .code,
            crate::app_error::AppErrorCode::InvalidRequest
        ));

        // Empty / duplicate id.
        assert!(matches!(
            match_reference_regex(MatchReferenceRegexRequest {
                query: "re:a".to_string(),
                descriptors: vec![ReferenceDescriptor {
                    id: String::new(),
                    source_ordinal: 0,
                    primary: vec!["a".to_string()],
                    secondary: vec![],
                }],
            })
            .unwrap_err()
            .code,
            crate::app_error::AppErrorCode::InvalidRequest
        ));
        assert!(matches!(
            match_reference_regex(MatchReferenceRegexRequest {
                query: "re:a".to_string(),
                descriptors: vec![
                    ReferenceDescriptor {
                        id: "dup".to_string(),
                        source_ordinal: 0,
                        primary: vec!["a".to_string()],
                        secondary: vec![],
                    },
                    ReferenceDescriptor {
                        id: "dup".to_string(),
                        source_ordinal: 1,
                        primary: vec!["b".to_string()],
                        secondary: vec![],
                    },
                ],
            })
            .unwrap_err()
            .code,
            crate::app_error::AppErrorCode::InvalidRequest
        ));

        // Field slot count: 0 rejects; 1024 accepts; 1025 rejects.
        assert!(matches!(
            match_reference_regex(MatchReferenceRegexRequest {
                query: "re:a".to_string(),
                descriptors: vec![ReferenceDescriptor {
                    id: "empty-fields".to_string(),
                    source_ordinal: 0,
                    primary: vec![],
                    secondary: vec![],
                }],
            })
            .unwrap_err()
            .code,
            crate::app_error::AppErrorCode::InvalidRequest
        ));

        let many_fields: Vec<String> = (0..MAX_FIELD_SLOTS).map(|i| format!("f{i}")).collect();
        assert!(match_reference_regex(MatchReferenceRegexRequest {
            query: "re:f0".to_string(),
            descriptors: vec![ReferenceDescriptor {
                id: "max-fields".to_string(),
                source_ordinal: 0,
                primary: many_fields.clone(),
                secondary: vec![],
            }],
        })
        .is_ok());

        let mut over_fields = many_fields;
        over_fields.push("extra".to_string());
        assert!(matches!(
            match_reference_regex(MatchReferenceRegexRequest {
                query: "re:f0".to_string(),
                descriptors: vec![ReferenceDescriptor {
                    id: "over-fields".to_string(),
                    source_ordinal: 0,
                    primary: over_fields,
                    secondary: vec![],
                }],
            })
            .unwrap_err()
            .code,
            crate::app_error::AppErrorCode::InvalidRequest
        ));

        // More than 255 field slots preserves u32 field_tier contract.
        let slots_300: Vec<String> = (0..300).map(|i| format!("slot{i}")).collect();
        let matched = match_reference_regex(MatchReferenceRegexRequest {
            query: "re:slot299".to_string(),
            descriptors: vec![ReferenceDescriptor {
                id: "wide".to_string(),
                source_ordinal: 0,
                primary: slots_300,
                secondary: vec![],
            }],
        })
        .expect("300 slots ok");
        assert_eq!(matched.len(), 1);
        assert_eq!(matched[0].rank.field_tier, 299);

        // Searchable bytes boundary.
        let exact_bytes = "y".repeat(MAX_SEARCHABLE_BYTES);
        assert!(match_reference_regex(MatchReferenceRegexRequest {
            query: "re:y".to_string(),
            descriptors: vec![ReferenceDescriptor {
                id: "bytes-ok".to_string(),
                source_ordinal: 0,
                primary: vec![exact_bytes],
                secondary: vec![],
            }],
        })
        .is_ok());
        assert!(matches!(
            match_reference_regex(MatchReferenceRegexRequest {
                query: "re:y".to_string(),
                descriptors: vec![ReferenceDescriptor {
                    id: "bytes-over".to_string(),
                    source_ordinal: 0,
                    primary: vec!["y".repeat(MAX_SEARCHABLE_BYTES + 1)],
                    secondary: vec![],
                }],
            })
            .unwrap_err()
            .code,
            crate::app_error::AppErrorCode::InvalidRequest
        ));

        // source_ordinal that cannot fit u32.
        assert!(matches!(
            match_reference_regex(MatchReferenceRegexRequest {
                query: "re:a".to_string(),
                descriptors: vec![ReferenceDescriptor {
                    id: "big-ord".to_string(),
                    source_ordinal: u64::from(u32::MAX) + 1,
                    primary: vec!["a".to_string()],
                    secondary: vec![],
                }],
            })
            .unwrap_err()
            .code,
            crate::app_error::AppErrorCode::InvalidRequest
        ));
    }

    #[test]
    fn encode_uri_component_matches_js_unescaped_set() {
        assert_eq!(encode_uri_component("AZaz09-_.!~*'()"), "AZaz09-_.!~*'()");
        assert_eq!(encode_uri_component("a b"), "a%20b");
        assert_eq!(encode_uri_component("#"), "%23");
        assert_eq!(encode_uri_component("文档"), "%E6%96%87%E6%A1%A3");
    }
}
