//! Canonical A1/B1 `work_unit_key` derivation and recognition.

use unicode_normalization::UnicodeNormalization;

use super::types::{
    ParsedWorkUnitKey, WorkUnitKeyParts, WorkflowError, MAX_WORK_UNIT_KEY_LEN,
};

/// Normalize a workspace-relative path to the B1 stored form:
/// UTF-8 NFC, separators → `/`, reject `|` / absolute / empty / `..`,
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

    let nfc: String = path.nfc().collect();
    if is_absolute_path(&nfc) {
        return Err(WorkflowError::InvalidPath(
            "absolute paths are not allowed".into(),
        ));
    }

    let mut normalized = String::with_capacity(nfc.len());
    for ch in nfc.chars() {
        if ch == '\\' || ch == '/' {
            // Collapse path separators to '/'
            if normalized.ends_with('/') {
                continue;
            }
            normalized.push('/');
        } else {
            normalized.push(ch);
        }
    }

    // Strip leading `./` segments and trailing slash (except keep empty → err)
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
            return Err(WorkflowError::InvalidPath(
                "empty path component".into(),
            ));
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

/// Build a canonical A1 work unit key (≤ 200 chars after normalization).
pub fn build_work_unit_key(parts: &WorkUnitKeyParts<'_>) -> Result<String, WorkflowError> {
    let key = match parts {
        WorkUnitKeyParts::Design {
            rel_doc_path,
            agent_type,
            profile_id,
        } => {
            let path = normalize_rel_path(rel_doc_path)?;
            let agent = validate_key_field(agent_type, "agent_type")?;
            let profile = profile_token(profile_id)?;
            format!("design|{path}|reviewer|{agent}|{profile}")
        }
        WorkUnitKeyParts::Plan {
            rel_plan_path,
            agent_type,
            profile_id,
        } => {
            let path = normalize_rel_path(rel_plan_path)?;
            let agent = validate_key_field(agent_type, "agent_type")?;
            let profile = profile_token(profile_id)?;
            format!("plan|{path}|reviewer|{agent}|{profile}")
        }
        WorkUnitKeyParts::TaskImplementer {
            task_index,
            agent_type,
            profile_id,
        } => {
            validate_task_index(*task_index)?;
            let agent = validate_key_field(agent_type, "agent_type")?;
            let profile = profile_token(profile_id)?;
            format!("task|{task_index}|implementer|{agent}|{profile}")
        }
        WorkUnitKeyParts::TaskReviewer {
            task_index,
            agent_type,
            profile_id,
        } => {
            validate_task_index(*task_index)?;
            let agent = validate_key_field(agent_type, "agent_type")?;
            let profile = profile_token(profile_id)?;
            format!("task|{task_index}|reviewer|{agent}|{profile}")
        }
        WorkUnitKeyParts::FinalReviewer {
            agent_type,
            profile_id,
        } => {
            let agent = validate_key_field(agent_type, "agent_type")?;
            let profile = profile_token(profile_id)?;
            format!("final_review|reviewer|{agent}|{profile}")
        }
        WorkUnitKeyParts::FinalFixer {
            agent_type,
            profile_id,
        } => {
            let agent = validate_key_field(agent_type, "agent_type")?;
            let profile = profile_token(profile_id)?;
            format!("final_review|fixer|{agent}|{profile}")
        }
    };

    if key.len() > MAX_WORK_UNIT_KEY_LEN {
        return Err(WorkflowError::KeyTooLong);
    }
    Ok(key)
}

/// Parse a recognized A1-grammar work unit key (A11). Pre-A1 keys return `None`.
pub fn parse_recognized_work_unit_key(key: &str) -> Option<ParsedWorkUnitKey> {
    if key.is_empty() || key.len() > MAX_WORK_UNIT_KEY_LEN {
        return None;
    }
    if key.chars().any(|c| c.is_control()) {
        return None;
    }

    let parts: Vec<&str> = key.split('|').collect();
    match parts.as_slice() {
        ["design", path, "reviewer", agent, profile] => {
            let rel = normalize_rel_path(path).ok()?;
            // Require byte equality with the normalized stored form.
            if rel != *path {
                return None;
            }
            let agent_type = parse_field(agent)?;
            let profile_id = parse_profile(profile)?;
            Some(ParsedWorkUnitKey::Design {
                rel_doc_path: rel,
                agent_type,
                profile_id,
            })
        }
        ["plan", path, "reviewer", agent, profile] => {
            let rel = normalize_rel_path(path).ok()?;
            if rel != *path {
                return None;
            }
            let agent_type = parse_field(agent)?;
            let profile_id = parse_profile(profile)?;
            Some(ParsedWorkUnitKey::Plan {
                rel_plan_path: rel,
                agent_type,
                profile_id,
            })
        }
        ["task", index, "implementer", agent, profile] => {
            let task_index = parse_task_index_str(index)?;
            let agent_type = parse_field(agent)?;
            let profile_id = parse_profile(profile)?;
            Some(ParsedWorkUnitKey::TaskImplementer {
                task_index,
                agent_type,
                profile_id,
            })
        }
        ["task", index, "reviewer", agent, profile] => {
            let task_index = parse_task_index_str(index)?;
            let agent_type = parse_field(agent)?;
            let profile_id = parse_profile(profile)?;
            Some(ParsedWorkUnitKey::TaskReviewer {
                task_index,
                agent_type,
                profile_id,
            })
        }
        ["final_review", "reviewer", agent, profile] => {
            let agent_type = parse_field(agent)?;
            let profile_id = parse_profile(profile)?;
            Some(ParsedWorkUnitKey::FinalReviewer {
                agent_type,
                profile_id,
            })
        }
        ["final_review", "fixer", agent, profile] => {
            let agent_type = parse_field(agent)?;
            let profile_id = parse_profile(profile)?;
            Some(ParsedWorkUnitKey::FinalFixer {
                agent_type,
                profile_id,
            })
        }
        _ => None,
    }
}

fn is_absolute_path(path: &str) -> bool {
    if path.starts_with('/') {
        return true;
    }
    // UNC / root-like
    if path.starts_with("\\\\") || path.starts_with("//") {
        return true;
    }
    let bytes = path.as_bytes();
    // Windows drive: `C:` or `C:\` / `C:/`
    if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        return true;
    }
    false
}

fn validate_key_field<'a>(value: &'a str, name: &str) -> Result<&'a str, WorkflowError> {
    if value.is_empty() {
        return Err(WorkflowError::InvalidField(format!("{name} is empty")));
    }
    if value.contains('|') {
        return Err(WorkflowError::InvalidField(format!(
            "{name} must not contain '|'"
        )));
    }
    Ok(value)
}

fn profile_token(profile_id: &Option<&str>) -> Result<String, WorkflowError> {
    match profile_id {
        None => Ok("none".to_string()),
        Some(id) => {
            if id.is_empty() {
                return Err(WorkflowError::InvalidField(
                    "profile_id is empty".into(),
                ));
            }
            if *id == "none" {
                // Explicit `none` is the absent-profile literal; treat as absent.
                return Ok("none".to_string());
            }
            if id.contains('|') {
                return Err(WorkflowError::InvalidField(
                    "profile_id must not contain '|'".into(),
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

fn parse_field(value: &str) -> Option<String> {
    if value.is_empty() || value.contains('|') {
        return None;
    }
    Some(value.to_string())
}

fn parse_profile(value: &str) -> Option<Option<String>> {
    if value.is_empty() || value.contains('|') {
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
    // No leading zeros (A1.4); "0" alone is invalid (must be positive).
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
        assert!(parse_recognized_work_unit_key(
            r"design|D:\repo\docs\a.md|reviewer|none"
        )
        .is_none());
    }

    #[test]
    fn rejects_pipe_in_path_field() {
        assert!(normalize_rel_path("a|b.md").is_err());
    }

    #[test]
    fn plan_key_and_task_keys_match_a1_table() {
        let plan = build_work_unit_key(&WorkUnitKeyParts::Plan {
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
        // Full A1 arity but absolute path material — still rejected.
        assert!(parse_recognized_work_unit_key(
            r"design|D:/repo/docs/a.md|reviewer|code_buddy|none"
        )
        .is_none());
        assert!(
            parse_recognized_work_unit_key("design|/abs/docs/a.md|reviewer|code_buddy|none")
                .is_none()
        );
    }
}
