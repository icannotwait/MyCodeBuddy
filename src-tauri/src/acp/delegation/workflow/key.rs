//! Canonical A1/B1 `work_unit_key` derivation and recognition.

use unicode_normalization::UnicodeNormalization;

use crate::models::agent::AgentType;

use super::types::{
    ParsedWorkUnitKey, ReviewerSlot, WorkUnitKeyParts, WorkflowError, MAX_WORK_UNIT_KEY_LEN,
};

/// Normalize a workspace-relative path to the B1 stored form:
/// UTF-8 NFC, separators → `/`, reject `|` / absolute / empty / `..` / controls,
/// and lowercase the path field on Windows before key construction.
pub fn normalize_rel_path(path: &str) -> Result<String, WorkflowError> {
    if path.is_empty() {
        return Err(WorkflowError::InvalidPath("empty path".into()));
    }
    if path.contains('|') {
        return Err(WorkflowError::InvalidPath(
            "path must not contain '|'".into(),
        ));
    }
    if path.chars().any(|c| c.is_control()) {
        return Err(WorkflowError::InvalidPath(
            "path must not contain control characters".into(),
        ));
    }

    let nfc: String = path.nfc().collect();
    if is_absolute_path(&nfc) {
        return Err(WorkflowError::InvalidPath(
            "absolute paths are not allowed".into(),
        ));
    }

    let mut normalized = String::with_capacity(nfc.len());
    for ch in nfc.chars() {
        if ch == '\\' || ch == '/' {
            if normalized.ends_with('/') {
                continue;
            }
            normalized.push('/');
        } else {
            normalized.push(ch);
        }
    }

    while normalized.starts_with("./") {
        normalized = normalized[2..].to_string();
    }
    if normalized.ends_with('/') && normalized.len() > 1 {
        normalized.pop();
    }
    if normalized.starts_with('/') {
        return Err(WorkflowError::InvalidPath(
            "absolute paths are not allowed".into(),
        ));
    }
    if normalized.is_empty() || normalized == "." {
        return Err(WorkflowError::InvalidPath("empty path".into()));
    }

    for component in normalized.split('/') {
        if component.is_empty() {
            return Err(WorkflowError::InvalidPath("empty path component".into()));
        }
        if component == ".." {
            return Err(WorkflowError::InvalidPath(
                "parent traversal '..' is not allowed".into(),
            ));
        }
        if component == "." {
            return Err(WorkflowError::InvalidPath(
                "current-dir component '.' is not allowed".into(),
            ));
        }
    }

    if cfg!(windows) {
        normalized = normalized.to_lowercase();
    }

    Ok(normalized)
}

/// Build a canonical A1 work unit key (≤ 200 Unicode scalar values).
pub fn build_work_unit_key(parts: &WorkUnitKeyParts<'_>) -> Result<String, WorkflowError> {
    let key = match parts {
        WorkUnitKeyParts::Design {
            rel_doc_path,
            agent_type,
            profile_id,
        } => {
            let path = normalize_rel_path(rel_doc_path)?;
            let agent = validate_agent_type(agent_type)?;
            let profile = profile_token(profile_id)?;
            format!("design|{path}|reviewer|{agent}|{profile}")
        }
        WorkUnitKeyParts::DesignFixer {
            rel_doc_path,
            agent_type,
            profile_id,
        } => {
            let path = normalize_rel_path(rel_doc_path)?;
            let agent = validate_agent_type(agent_type)?;
            let profile = profile_token(profile_id)?;
            format!("design|{path}|fixer|{agent}|{profile}")
        }
        WorkUnitKeyParts::PlanAuthor {
            rel_plan_path,
            agent_type,
            profile_id,
        } => {
            let path = normalize_rel_path(rel_plan_path)?;
            let agent = validate_agent_type(agent_type)?;
            let profile = profile_token(profile_id)?;
            format!("plan|{path}|author|{agent}|{profile}")
        }
        WorkUnitKeyParts::PlanReviewer {
            rel_plan_path,
            agent_type,
            profile_id,
        } => {
            let path = normalize_rel_path(rel_plan_path)?;
            let agent = validate_agent_type(agent_type)?;
            let profile = profile_token(profile_id)?;
            format!("plan|{path}|reviewer|{agent}|{profile}")
        }
        WorkUnitKeyParts::TaskImplementer {
            task_index,
            agent_type,
            profile_id,
        } => {
            validate_task_index(*task_index)?;
            let agent = validate_agent_type(agent_type)?;
            let profile = profile_token(profile_id)?;
            format!("task|{task_index}|implementer|{agent}|{profile}")
        }
        WorkUnitKeyParts::TaskReviewer {
            task_index,
            agent_type,
            profile_id,
        } => {
            validate_task_index(*task_index)?;
            let agent = validate_agent_type(agent_type)?;
            let profile = profile_token(profile_id)?;
            format!("task|{task_index}|reviewer|{agent}|{profile}")
        }
        WorkUnitKeyParts::TaskReviewerSlotted {
            task_index,
            slot,
            agent_type,
            profile_id,
        } => {
            validate_task_index(*task_index)?;
            let agent = validate_agent_type(agent_type)?;
            let profile = profile_token(profile_id)?;
            let slot = slot.as_str();
            format!("task|{task_index}|reviewer|{slot}|{agent}|{profile}")
        }
        WorkUnitKeyParts::FinalReviewer {
            agent_type,
            profile_id,
        } => {
            let agent = validate_agent_type(agent_type)?;
            let profile = profile_token(profile_id)?;
            format!("final_review|reviewer|{agent}|{profile}")
        }
        WorkUnitKeyParts::FinalFixer {
            agent_type,
            profile_id,
        } => {
            let agent = validate_agent_type(agent_type)?;
            let profile = profile_token(profile_id)?;
            format!("final_review|fixer|{agent}|{profile}")
        }
    };

    if key_len(&key) > MAX_WORK_UNIT_KEY_LEN {
        return Err(WorkflowError::KeyTooLong);
    }
    Ok(key)
}

/// Parse a recognized A1-grammar work unit key (A11). Pre-A1 keys return `None`.
///
/// Recognition uses the same field validators as `build_work_unit_key` so builder
/// and recognizer agree on agent types, control characters, and length.
pub fn parse_recognized_work_unit_key(key: &str) -> Option<ParsedWorkUnitKey> {
    if key.is_empty() || key_len(key) > MAX_WORK_UNIT_KEY_LEN {
        return None;
    }
    if key.chars().any(|c| c.is_control()) {
        return None;
    }

    let parts: Vec<&str> = key.split('|').collect();
    match parts.as_slice() {
        ["design", path, "reviewer", agent, profile] => {
            let rel = normalize_rel_path(path).ok()?;
            if rel != *path {
                return None;
            }
            let agent_type = validate_agent_type(agent).ok()?.to_string();
            let profile_id = parse_profile(profile)?;
            Some(ParsedWorkUnitKey::Design {
                rel_doc_path: rel,
                agent_type,
                profile_id,
            })
        }
        ["design", path, "fixer", agent, profile] => {
            let rel = normalize_rel_path(path).ok()?;
            if rel != *path {
                return None;
            }
            let agent_type = validate_agent_type(agent).ok()?.to_string();
            let profile_id = parse_profile(profile)?;
            Some(ParsedWorkUnitKey::DesignFixer {
                rel_doc_path: rel,
                agent_type,
                profile_id,
            })
        }
        ["plan", path, "author", agent, profile] => {
            let rel = normalize_rel_path(path).ok()?;
            if rel != *path {
                return None;
            }
            let profile_id = parse_profile(profile)?;
            let agent_type = validate_agent_type(agent).ok()?.to_string();
            Some(ParsedWorkUnitKey::PlanAuthor {
                rel_plan_path: rel,
                agent_type,
                profile_id,
            })
        }
        ["plan", path, "reviewer", agent, profile] => {
            let rel = normalize_rel_path(path).ok()?;
            if rel != *path {
                return None;
            }
            let agent_type = validate_agent_type(agent).ok()?.to_string();
            let profile_id = parse_profile(profile)?;
            Some(ParsedWorkUnitKey::PlanReviewer {
                rel_plan_path: rel,
                agent_type,
                profile_id,
            })
        }
        ["task", index, "implementer", agent, profile] => {
            let task_index = parse_task_index_str(index)?;
            let agent_type = validate_agent_type(agent).ok()?.to_string();
            let profile_id = parse_profile(profile)?;
            Some(ParsedWorkUnitKey::TaskImplementer {
                task_index,
                agent_type,
                profile_id,
            })
        }
        ["task", index, "reviewer", slot, agent, profile] => {
            let task_index = parse_task_index_str(index)?;
            let slot = match *slot {
                "primary" => ReviewerSlot::Primary,
                "auxiliary" => ReviewerSlot::Auxiliary,
                _ => return None,
            };
            let agent_type = validate_agent_type(agent).ok()?.to_string();
            let profile_id = parse_profile(profile)?;
            Some(ParsedWorkUnitKey::TaskReviewer {
                task_index,
                slot,
                agent_type,
                profile_id,
            })
        }
        ["task", index, "reviewer", agent, profile] => {
            let task_index = parse_task_index_str(index)?;
            let agent_type = validate_agent_type(agent).ok()?.to_string();
            let profile_id = parse_profile(profile)?;
            Some(ParsedWorkUnitKey::TaskReviewer {
                task_index,
                slot: ReviewerSlot::Primary,
                agent_type,
                profile_id,
            })
        }
        ["final_review", "reviewer", agent, profile] => {
            let agent_type = validate_agent_type(agent).ok()?.to_string();
            let profile_id = parse_profile(profile)?;
            Some(ParsedWorkUnitKey::FinalReviewer {
                agent_type,
                profile_id,
            })
        }
        ["final_review", "fixer", agent, profile] => {
            let agent_type = validate_agent_type(agent).ok()?.to_string();
            let profile_id = parse_profile(profile)?;
            Some(ParsedWorkUnitKey::FinalFixer {
                agent_type,
                profile_id,
            })
        }
        _ => None,
    }
}

fn key_len(s: &str) -> usize {
    s.chars().count()
}

fn is_absolute_path(path: &str) -> bool {
    if path.starts_with('/') {
        return true;
    }
    if path.starts_with("\\\\") || path.starts_with("//") {
        return true;
    }
    let bytes = path.as_bytes();
    if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        return true;
    }
    false
}

/// Validate `agent_type` as a Codeg [`AgentType`] wire string (snake_case).
pub fn validate_agent_type(value: &str) -> Result<&str, WorkflowError> {
    if value.is_empty() {
        return Err(WorkflowError::InvalidAgentType("empty".into()));
    }
    if value.contains('|') {
        return Err(WorkflowError::InvalidAgentType(
            "must not contain '|'".into(),
        ));
    }
    if value.chars().any(|c| c.is_control()) {
        return Err(WorkflowError::InvalidAgentType(
            "must not contain control characters".into(),
        ));
    }
    // Wire form is serde snake_case unit variant (e.g. `code_buddy`, `grok`).
    let parsed: Result<AgentType, _> =
        serde_json::from_value(serde_json::Value::String(value.to_string()));
    match parsed {
        Ok(_) => Ok(value),
        Err(_) => Err(WorkflowError::InvalidAgentType(value.to_string())),
    }
}

fn profile_token(profile_id: &Option<&str>) -> Result<String, WorkflowError> {
    match profile_id {
        None => Ok("none".to_string()),
        Some(id) => {
            if id.is_empty() {
                return Err(WorkflowError::InvalidField("profile_id is empty".into()));
            }
            if *id == "none" {
                return Ok("none".to_string());
            }
            if id.contains('|') {
                return Err(WorkflowError::InvalidField(
                    "profile_id must not contain '|'".into(),
                ));
            }
            if id.chars().any(|c| c.is_control()) {
                return Err(WorkflowError::InvalidField(
                    "profile_id must not contain control characters".into(),
                ));
            }
            Ok((*id).to_string())
        }
    }
}

fn validate_task_index(task_index: u32) -> Result<(), WorkflowError> {
    if task_index == 0 {
        return Err(WorkflowError::InvalidTaskIndex(
            "task_index must be a positive integer".into(),
        ));
    }
    Ok(())
}

fn parse_profile(value: &str) -> Option<Option<String>> {
    if value.is_empty() || value.contains('|') || value.chars().any(|c| c.is_control()) {
        return None;
    }
    if value == "none" {
        Some(None)
    } else {
        Some(Some(value.to_string()))
    }
}

fn parse_task_index_str(value: &str) -> Option<u32> {
    if value.is_empty() || !value.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    if value.starts_with('0') {
        return None;
    }
    value.parse().ok().filter(|&n| n > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn design_key_uses_relative_path_and_agent() {
        let k = build_work_unit_key(&WorkUnitKeyParts::Design {
            rel_doc_path: r"docs\superpowers\specs\x.md",
            agent_type: "code_buddy",
            profile_id: Some("a1c14cde-f9c0-4fce-9d7f-66c3f8e85039"),
        })
        .unwrap();
        assert_eq!(
            k,
            "design|docs/superpowers/specs/x.md|reviewer|code_buddy|a1c14cde-f9c0-4fce-9d7f-66c3f8e85039"
        );
    }

    #[test]
    fn absolute_path_materials_not_recognized() {
        assert!(
            parse_recognized_work_unit_key(r"design|D:\repo\docs\a.md|reviewer|none").is_none()
        );
    }

    #[test]
    fn rejects_pipe_in_path_field() {
        assert!(normalize_rel_path("a|b.md").is_err());
    }

    #[test]
    fn plan_key_and_task_keys_match_a1_table() {
        let plan = build_work_unit_key(&WorkUnitKeyParts::PlanReviewer {
            rel_plan_path: "docs/superpowers/plans/p.md",
            agent_type: "codex",
            profile_id: None,
        })
        .unwrap();
        assert_eq!(plan, "plan|docs/superpowers/plans/p.md|reviewer|codex|none");

        let impl_key = build_work_unit_key(&WorkUnitKeyParts::TaskImplementer {
            task_index: 2,
            agent_type: "grok",
            profile_id: None,
        })
        .unwrap();
        assert_eq!(impl_key, "task|2|implementer|grok|none");

        let rev = build_work_unit_key(&WorkUnitKeyParts::TaskReviewer {
            task_index: 2,
            agent_type: "codex",
            profile_id: Some("prof-1"),
        })
        .unwrap();
        assert_eq!(rev, "task|2|reviewer|codex|prof-1");

        let final_rev = build_work_unit_key(&WorkUnitKeyParts::FinalReviewer {
            agent_type: "codex",
            profile_id: None,
        })
        .unwrap();
        assert_eq!(final_rev, "final_review|reviewer|codex|none");

        let final_fix = build_work_unit_key(&WorkUnitKeyParts::FinalFixer {
            agent_type: "grok",
            profile_id: None,
        })
        .unwrap();
        assert_eq!(final_fix, "final_review|fixer|grok|none");
    }

    #[test]
    fn design_fixer_and_slotted_reviewers_round_trip() {
        let fixer = build_work_unit_key(&WorkUnitKeyParts::DesignFixer {
            rel_doc_path: "docs/design.md",
            agent_type: "codex",
            profile_id: None,
        })
        .unwrap();
        assert_eq!(fixer, "design|docs/design.md|fixer|codex|none");
        assert!(matches!(
            parse_recognized_work_unit_key(&fixer),
            Some(ParsedWorkUnitKey::DesignFixer { .. })
        ));

        for (slot, expected) in [
            (ReviewerSlot::Primary, "task|7|reviewer|primary|codex|none"),
            (
                ReviewerSlot::Auxiliary,
                "task|7|reviewer|auxiliary|codex|none",
            ),
        ] {
            let key = build_work_unit_key(&WorkUnitKeyParts::TaskReviewerSlotted {
                task_index: 7,
                slot,
                agent_type: "codex",
                profile_id: None,
            })
            .unwrap();
            assert_eq!(key, expected);
            assert!(matches!(
                parse_recognized_work_unit_key(&key),
                Some(ParsedWorkUnitKey::TaskReviewer {
                    task_index: 7,
                    slot: parsed,
                    ..
                }) if parsed == slot
            ));
        }
    }

    #[test]
    fn legacy_task_reviewer_is_primary_and_invalid_slots_fail() {
        assert!(matches!(
            parse_recognized_work_unit_key("task|7|reviewer|codex|none"),
            Some(ParsedWorkUnitKey::TaskReviewer {
                task_index: 7,
                slot: ReviewerSlot::Primary,
                ..
            })
        ));
        for key in [
            "task|7|reviewer|secondary|codex|none",
            "task|0|reviewer|primary|codex|none",
            "task|7|reviewer|primary|unknown-agent|none",
            "design|../design.md|fixer|codex|none",
            "design|docs/design.md|fixer|codex|bad|profile",
        ] {
            assert_eq!(parse_recognized_work_unit_key(key), None, "{key}");
        }
    }

    #[test]
    fn new_work_unit_branches_reuse_all_identity_validators_and_scalar_bounds() {
        for parts in [
            WorkUnitKeyParts::DesignFixer {
                rel_doc_path: "../design.md",
                agent_type: "codex",
                profile_id: None,
            },
            WorkUnitKeyParts::DesignFixer {
                rel_doc_path: "docs/design.md",
                agent_type: "not_an_agent",
                profile_id: None,
            },
            WorkUnitKeyParts::DesignFixer {
                rel_doc_path: "docs/design.md",
                agent_type: "codex",
                profile_id: Some("bad\u{0007}profile"),
            },
        ] {
            assert!(build_work_unit_key(&parts).is_err());
        }

        for parts in [
            WorkUnitKeyParts::TaskReviewerSlotted {
                task_index: 0,
                slot: ReviewerSlot::Primary,
                agent_type: "codex",
                profile_id: None,
            },
            WorkUnitKeyParts::TaskReviewerSlotted {
                task_index: 1,
                slot: ReviewerSlot::Auxiliary,
                agent_type: "not_an_agent",
                profile_id: None,
            },
            WorkUnitKeyParts::TaskReviewerSlotted {
                task_index: 1,
                slot: ReviewerSlot::Auxiliary,
                agent_type: "codex",
                profile_id: Some("bad\u{0007}profile"),
            },
        ] {
            assert!(build_work_unit_key(&parts).is_err());
        }

        for key in [
            "design|docs/design.md|fixer|not_an_agent|none",
            "design|docs/design.md|fixer|codex|bad\u{0007}profile",
            "task|1|reviewer|auxiliary|not_an_agent|none",
            "task|1|reviewer|auxiliary|codex|bad\u{0007}profile",
            "task|01|reviewer|primary|codex|none",
        ] {
            assert_eq!(parse_recognized_work_unit_key(key), None, "{key:?}");
        }

        let design_profile_at_limit = "x".repeat(171);
        let design_at_limit = build_work_unit_key(&WorkUnitKeyParts::DesignFixer {
            rel_doc_path: "docs/d.md",
            agent_type: "codex",
            profile_id: Some(&design_profile_at_limit),
        })
        .expect("200-scalar Design Fixer key");
        assert_eq!(design_at_limit.chars().count(), 200);
        assert!(parse_recognized_work_unit_key(&design_at_limit).is_some());
        assert!(build_work_unit_key(&WorkUnitKeyParts::DesignFixer {
            rel_doc_path: "docs/d.md",
            agent_type: "codex",
            profile_id: Some(&"x".repeat(172)),
        })
        .is_err());

        let reviewer_profile_at_limit = "x".repeat(170);
        let reviewer_at_limit = build_work_unit_key(&WorkUnitKeyParts::TaskReviewerSlotted {
            task_index: 1,
            slot: ReviewerSlot::Primary,
            agent_type: "codex",
            profile_id: Some(&reviewer_profile_at_limit),
        })
        .expect("200-scalar slotted reviewer key");
        assert_eq!(reviewer_at_limit.chars().count(), 200);
        assert!(parse_recognized_work_unit_key(&reviewer_at_limit).is_some());
        assert!(build_work_unit_key(&WorkUnitKeyParts::TaskReviewerSlotted {
            task_index: 1,
            slot: ReviewerSlot::Primary,
            agent_type: "codex",
            profile_id: Some(&"x".repeat(171)),
        })
        .is_err());
    }

    #[test]
    fn parse_round_trips_recognized_keys() {
        let key = "design|docs/superpowers/specs/x.md|reviewer|code_buddy|a1c14cde-f9c0-4fce-9d7f-66c3f8e85039";
        let parsed = parse_recognized_work_unit_key(key).expect("recognized");
        match parsed {
            ParsedWorkUnitKey::Design {
                rel_doc_path,
                agent_type,
                profile_id,
            } => {
                assert_eq!(rel_doc_path, "docs/superpowers/specs/x.md");
                assert_eq!(agent_type, "code_buddy");
                assert_eq!(
                    profile_id.as_deref(),
                    Some("a1c14cde-f9c0-4fce-9d7f-66c3f8e85039")
                );
            }
            other => panic!("unexpected parse: {other:?}"),
        }

        assert!(parse_recognized_work_unit_key("unit-preboot").is_none());
        assert!(parse_recognized_work_unit_key("task|02|implementer|grok|none").is_none());
        assert!(parse_recognized_work_unit_key("task|0|implementer|grok|none").is_none());
    }

    #[test]
    fn plan_author_key_round_trips() {
        let author_key = build_work_unit_key(&WorkUnitKeyParts::PlanAuthor {
            rel_plan_path: "docs/superpowers/plans/p.md",
            agent_type: "codex",
            profile_id: None,
        })
        .expect("Plan Author key builds");
        assert_eq!(
            author_key,
            "plan|docs/superpowers/plans/p.md|author|codex|none"
        );
        assert_eq!(
            parse_recognized_work_unit_key(&author_key),
            Some(ParsedWorkUnitKey::PlanAuthor {
                rel_plan_path: "docs/superpowers/plans/p.md".into(),
                agent_type: "codex".into(),
                profile_id: None,
            })
        );

        let reviewer_key = build_work_unit_key(&WorkUnitKeyParts::PlanReviewer {
            rel_plan_path: "docs/superpowers/plans/p.md",
            agent_type: "grok",
            profile_id: Some("profile-1"),
        })
        .expect("Plan reviewer key builds");
        assert_eq!(
            parse_recognized_work_unit_key(&reviewer_key),
            Some(ParsedWorkUnitKey::PlanReviewer {
                rel_plan_path: "docs/superpowers/plans/p.md".into(),
                agent_type: "grok".into(),
                profile_id: Some("profile-1".into()),
            })
        );

        assert!(
            parse_recognized_work_unit_key("plan|docs/superpowers/plans/p.md|codex|none").is_none()
        );
    }

    #[test]
    fn rejects_key_longer_than_200() {
        let long_path = format!("docs/{}/x.md", "a".repeat(220));
        let err = build_work_unit_key(&WorkUnitKeyParts::Design {
            rel_doc_path: &long_path,
            agent_type: "grok",
            profile_id: None,
        })
        .unwrap_err();
        assert_eq!(err, WorkflowError::KeyTooLong);
    }

    #[test]
    fn rejects_absolute_unix_and_drive_paths() {
        assert!(normalize_rel_path("/etc/passwd").is_err());
        assert!(normalize_rel_path(r"C:\Windows\system32").is_err());
        assert!(normalize_rel_path("C:/Windows/system32").is_err());
        assert!(normalize_rel_path(r"\\server\share\a.md").is_err());
    }

    #[test]
    fn pre_a1_absolute_five_field_key_not_recognized() {
        assert!(parse_recognized_work_unit_key(
            r"design|D:/repo/docs/a.md|reviewer|code_buddy|none"
        )
        .is_none());
        assert!(
            parse_recognized_work_unit_key("design|/abs/docs/a.md|reviewer|code_buddy|none")
                .is_none()
        );
    }

    #[test]
    fn agent_type_must_be_codeg_enum() {
        let err = build_work_unit_key(&WorkUnitKeyParts::FinalReviewer {
            agent_type: "not_an_agent",
            profile_id: None,
        })
        .unwrap_err();
        assert!(matches!(err, WorkflowError::InvalidAgentType(_)));
        assert!(
            parse_recognized_work_unit_key("final_review|reviewer|not_an_agent|none").is_none()
        );
        // Display name is not the wire form.
        assert!(build_work_unit_key(&WorkUnitKeyParts::FinalReviewer {
            agent_type: "Codex CLI",
            profile_id: None,
        })
        .is_err());
    }

    #[test]
    fn builder_and_recognizer_reject_control_chars() {
        let bad_agent = "grok\u{0001}";
        assert!(build_work_unit_key(&WorkUnitKeyParts::FinalFixer {
            agent_type: bad_agent,
            profile_id: None,
        })
        .is_err());
        assert!(parse_recognized_work_unit_key("final_review|fixer|grok\u{0001}|none").is_none());

        let bad_profile = "prof\u{0007}";
        assert!(build_work_unit_key(&WorkUnitKeyParts::TaskReviewer {
            task_index: 1,
            agent_type: "codex",
            profile_id: Some(bad_profile),
        })
        .is_err());
    }

    #[test]
    fn key_length_uses_unicode_scalar_count() {
        // 180 ASCII + multi-byte scalars still counted by chars(), not bytes.
        let path = format!("docs/{}/x.md", "文".repeat(60));
        let result = build_work_unit_key(&WorkUnitKeyParts::Design {
            rel_doc_path: &path,
            agent_type: "grok",
            profile_id: None,
        });
        // Path alone is large; either KeyTooLong or Ok — length gate is chars.
        match result {
            Ok(k) => assert!(k.chars().count() <= MAX_WORK_UNIT_KEY_LEN),
            Err(WorkflowError::KeyTooLong) => {}
            Err(other) => panic!("unexpected: {other:?}"),
        }
    }
}
