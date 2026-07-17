use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

pub const MAX_TOUCHED_FILES: usize = 200;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DelegationTouchedFile {
    pub path: String,
    pub outside_workspace: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub additions: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deletions: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DelegationRuntimeStats {
    pub started_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<DateTime<Utc>>,
    pub tool_call_count: u64,
    pub edit_tool_call_count: u64,
    pub touched_files: Vec<DelegationTouchedFile>,
    pub touched_files_truncated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub additions: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deletions: Option<u64>,
    pub line_counts_complete: bool,
}

impl DelegationRuntimeStats {
    pub fn empty(started_at: DateTime<Utc>) -> Self {
        Self {
            started_at,
            finished_at: None,
            tool_call_count: 0,
            edit_tool_call_count: 0,
            touched_files: Vec::new(),
            touched_files_truncated: false,
            additions: None,
            deletions: None,
            line_counts_complete: false,
        }
    }
}

pub struct PersistedRuntimeStatsColumns<'a> {
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
    pub tool_call_count: Option<i64>,
    pub edit_tool_call_count: Option<i64>,
    pub touched_files_json: Option<&'a str>,
    pub touched_files_truncated: Option<bool>,
    pub additions: Option<i64>,
    pub deletions: Option<i64>,
    pub line_counts_complete: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum RuntimeStatsDecodeError {
    #[error("runtime statistics contain an invalid count")]
    InvalidCount,
    #[error("runtime statistics contain invalid touched-file JSON")]
    InvalidTouchedFiles,
    #[error("runtime statistics violate timestamp or line-count invariants")]
    InvalidInvariant,
}

pub fn decode_persisted_runtime_stats(
    fields: PersistedRuntimeStatsColumns<'_>,
) -> Result<Option<DelegationRuntimeStats>, RuntimeStatsDecodeError> {
    let PersistedRuntimeStatsColumns {
        started_at,
        finished_at,
        tool_call_count,
        edit_tool_call_count,
        touched_files_json,
        touched_files_truncated,
        additions,
        deletions,
        line_counts_complete,
    } = fields;
    let (
        Some(started_at),
        Some(tool_call_count),
        Some(edit_tool_call_count),
        Some(touched_files_json),
        Some(touched_files_truncated),
        Some(line_counts_complete),
    ) = (
        started_at,
        tool_call_count,
        edit_tool_call_count,
        touched_files_json,
        touched_files_truncated,
        line_counts_complete,
    )
    else {
        return Ok(None);
    };
    let tool_call_count =
        u64::try_from(tool_call_count).map_err(|_| RuntimeStatsDecodeError::InvalidCount)?;
    let edit_tool_call_count = u64::try_from(edit_tool_call_count)
        .map_err(|_| RuntimeStatsDecodeError::InvalidCount)?;
    let additions = additions
        .map(u64::try_from)
        .transpose()
        .map_err(|_| RuntimeStatsDecodeError::InvalidCount)?;
    let deletions = deletions
        .map(u64::try_from)
        .transpose()
        .map_err(|_| RuntimeStatsDecodeError::InvalidCount)?;
    let touched_files = serde_json::from_str::<Vec<DelegationTouchedFile>>(touched_files_json)
        .map_err(|_| RuntimeStatsDecodeError::InvalidTouchedFiles)?;
    let invalid_file = touched_files.iter().any(|file| {
        file.path.trim().is_empty() || file.additions.is_some() != file.deletions.is_some()
    });
    let invalid_invariant = edit_tool_call_count > tool_call_count
        || touched_files.len() > MAX_TOUCHED_FILES
        || invalid_file
        || finished_at
            .as_ref()
            .is_some_and(|finished| finished < &started_at)
        || additions.is_some() != deletions.is_some()
        || line_counts_complete != additions.is_some()
        || (line_counts_complete && edit_tool_call_count == 0);
    if invalid_invariant {
        return Err(RuntimeStatsDecodeError::InvalidInvariant);
    }
    Ok(Some(DelegationRuntimeStats {
        started_at,
        finished_at,
        tool_call_count,
        edit_tool_call_count,
        touched_files,
        touched_files_truncated,
        additions,
        deletions,
        line_counts_complete,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn base_columns() -> PersistedRuntimeStatsColumns<'static> {
        PersistedRuntimeStatsColumns {
            started_at: Some(Utc.with_ymd_and_hms(2026, 7, 17, 0, 0, 0).unwrap()),
            finished_at: None,
            tool_call_count: Some(2),
            edit_tool_call_count: Some(1),
            touched_files_json: Some(r#"[{"path":"src/a.rs","outside_workspace":false}]"#),
            touched_files_truncated: Some(false),
            additions: None,
            deletions: None,
            line_counts_complete: Some(false),
        }
    }

    #[test]
    fn decode_persisted_runtime_stats_accepts_complete_required_fields() {
        let stats = decode_persisted_runtime_stats(base_columns())
            .expect("decode")
            .expect("Some");
        assert_eq!(stats.tool_call_count, 2);
        assert_eq!(stats.edit_tool_call_count, 1);
        assert_eq!(stats.touched_files.len(), 1);
        assert!(!stats.line_counts_complete);
    }

    #[test]
    fn decode_persisted_runtime_stats_returns_none_when_any_required_field_null() {
        let cases: Vec<fn(&mut PersistedRuntimeStatsColumns<'_>)> = vec![
            |c| c.started_at = None,
            |c| c.tool_call_count = None,
            |c| c.edit_tool_call_count = None,
            |c| c.touched_files_json = None,
            |c| c.touched_files_truncated = None,
            |c| c.line_counts_complete = None,
        ];
        for mutate in cases {
            let mut cols = base_columns();
            mutate(&mut cols);
            assert_eq!(
                decode_persisted_runtime_stats(cols).expect("ok"),
                None,
                "any null required field must yield None"
            );
        }
    }

    #[test]
    fn decode_persisted_runtime_stats_rejects_negative_counts() {
        for mutate in [
            |c: &mut PersistedRuntimeStatsColumns<'_>| c.tool_call_count = Some(-1),
            |c: &mut PersistedRuntimeStatsColumns<'_>| c.edit_tool_call_count = Some(-1),
            |c: &mut PersistedRuntimeStatsColumns<'_>| c.additions = Some(-1),
            |c: &mut PersistedRuntimeStatsColumns<'_>| {
                c.deletions = Some(-1);
                c.additions = Some(1);
                c.line_counts_complete = Some(true);
                c.edit_tool_call_count = Some(1);
            },
        ] {
            let mut cols = base_columns();
            mutate(&mut cols);
            assert_eq!(
                decode_persisted_runtime_stats(cols),
                Err(RuntimeStatsDecodeError::InvalidCount)
            );
        }
    }

    #[test]
    fn decode_persisted_runtime_stats_rejects_invariants() {
        // edit count greater than tool count
        let mut cols = base_columns();
        cols.edit_tool_call_count = Some(5);
        cols.tool_call_count = Some(2);
        assert_eq!(
            decode_persisted_runtime_stats(cols),
            Err(RuntimeStatsDecodeError::InvalidInvariant)
        );

        // more than 200 paths
        let many: Vec<_> = (0..201)
            .map(|i| {
                format!(r#"{{"path":"f{i}.rs","outside_workspace":false}}"#)
            })
            .collect();
        let json = format!("[{}]", many.join(","));
        let mut cols = base_columns();
        let owned = json;
        cols.touched_files_json = Some(&owned);
        assert_eq!(
            decode_persisted_runtime_stats(cols),
            Err(RuntimeStatsDecodeError::InvalidInvariant)
        );

        // blank path
        let mut cols = base_columns();
        cols.touched_files_json =
            Some(r#"[{"path":"  ","outside_workspace":false}]"#);
        assert_eq!(
            decode_persisted_runtime_stats(cols),
            Err(RuntimeStatsDecodeError::InvalidInvariant)
        );

        // one-sided line pairs on a file
        let mut cols = base_columns();
        cols.touched_files_json =
            Some(r#"[{"path":"a.rs","outside_workspace":false,"additions":1}]"#);
        assert_eq!(
            decode_persisted_runtime_stats(cols),
            Err(RuntimeStatsDecodeError::InvalidInvariant)
        );

        // finished_at < started_at
        let mut cols = base_columns();
        cols.finished_at = Some(Utc.with_ymd_and_hms(2026, 7, 16, 0, 0, 0).unwrap());
        assert_eq!(
            decode_persisted_runtime_stats(cols),
            Err(RuntimeStatsDecodeError::InvalidInvariant)
        );

        // mismatch between line_counts_complete and aggregate totals
        let mut cols = base_columns();
        cols.line_counts_complete = Some(true);
        cols.additions = None;
        cols.deletions = None;
        assert_eq!(
            decode_persisted_runtime_stats(cols),
            Err(RuntimeStatsDecodeError::InvalidInvariant)
        );

        // line_counts_complete with zero edits
        let mut cols = base_columns();
        cols.edit_tool_call_count = Some(0);
        cols.additions = Some(1);
        cols.deletions = Some(1);
        cols.line_counts_complete = Some(true);
        assert_eq!(
            decode_persisted_runtime_stats(cols),
            Err(RuntimeStatsDecodeError::InvalidInvariant)
        );

        // aggregate one-sided additions/deletions
        let mut cols = base_columns();
        cols.additions = Some(1);
        cols.deletions = None;
        cols.line_counts_complete = Some(false);
        assert_eq!(
            decode_persisted_runtime_stats(cols),
            Err(RuntimeStatsDecodeError::InvalidInvariant)
        );
    }

    #[test]
    fn decode_persisted_runtime_stats_rejects_invalid_json_with_fixed_variant() {
        let mut cols = base_columns();
        cols.touched_files_json = Some("not-json");
        assert_eq!(
            decode_persisted_runtime_stats(cols),
            Err(RuntimeStatsDecodeError::InvalidTouchedFiles)
        );
    }
}
