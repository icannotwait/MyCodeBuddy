use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use similar::{ChangeTag, TextDiff};

use crate::acp::types::AcpEvent;

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

// ---------------------------------------------------------------------------
// Bounded pure projector
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default)]
struct ToolProjectionState {
    kind: Option<String>,
    title: Option<String>,
    raw_input: Option<String>,
    /// Parsed only from a complete, replacement-style JSON `raw_output`.
    /// Opaque text and append chunks are discarded rather than accumulated.
    structured_result: Option<serde_json::Value>,
    locations: Option<serde_json::Value>,
    meta: Option<serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PathContribution {
    key: String,
    display: DelegationTouchedFile,
    textual_edit: bool,
    additions: Option<u64>,
    deletions: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct ToolContribution {
    edit: bool,
    textual_edit: bool,
    retained_paths: Vec<PathContribution>,
    additions: Option<u64>,
    deletions: Option<u64>,
}

pub struct RuntimeStatsProjector {
    stats: DelegationRuntimeStats,
    workspace: PathBuf,
    calls: HashMap<String, ToolProjectionState>,
    contributions: HashMap<String, ToolContribution>,
    retained_path_order: Vec<String>,
    retained_path_first_display: HashMap<String, DelegationTouchedFile>,
    retained_path_ref_counts: HashMap<String, usize>,
    retained_path_additions: HashMap<String, u128>,
    retained_path_deletions: HashMap<String, u128>,
    retained_path_textual_with_counts: HashMap<String, usize>,
    retained_path_textual_without_counts: HashMap<String, usize>,
    known_additions: u128,
    known_deletions: u128,
    textual_edits_with_counts: usize,
    textual_edits_without_counts: usize,
    overflow_seen: bool,
    case_insensitive_paths: bool,
}

impl RuntimeStatsProjector {
    pub fn new(started_at: DateTime<Utc>, workspace: PathBuf) -> Self {
        Self::new_with_path_case(started_at, workspace, cfg!(windows))
    }

    fn new_with_path_case(
        started_at: DateTime<Utc>,
        workspace: PathBuf,
        case_insensitive_paths: bool,
    ) -> Self {
        Self {
            stats: DelegationRuntimeStats::empty(started_at),
            workspace: lexical_normalize(&workspace),
            calls: HashMap::new(),
            contributions: HashMap::new(),
            retained_path_order: Vec::new(),
            retained_path_first_display: HashMap::new(),
            retained_path_ref_counts: HashMap::new(),
            retained_path_additions: HashMap::new(),
            retained_path_deletions: HashMap::new(),
            retained_path_textual_with_counts: HashMap::new(),
            retained_path_textual_without_counts: HashMap::new(),
            known_additions: 0,
            known_deletions: 0,
            textual_edits_with_counts: 0,
            textual_edits_without_counts: 0,
            overflow_seen: false,
            case_insensitive_paths,
        }
    }

    #[cfg(test)]
    fn new_for_test(
        started_at: DateTime<Utc>,
        workspace: PathBuf,
        case_insensitive_paths: bool,
    ) -> Self {
        Self::new_with_path_case(started_at, workspace, case_insensitive_paths)
    }

    pub fn finish(&mut self, finished_at: DateTime<Utc>) -> bool {
        if self.stats.finished_at == Some(finished_at) {
            return false;
        }
        self.stats.finished_at = Some(finished_at);
        true
    }

    pub fn snapshot(&self) -> DelegationRuntimeStats {
        self.stats.clone()
    }

    pub fn apply(&mut self, event: &AcpEvent) -> bool {
        let before = self.stats.clone();
        let tool_call_id = match event {
            AcpEvent::ToolCall { tool_call_id, .. }
            | AcpEvent::ToolCallUpdate { tool_call_id, .. } => tool_call_id.as_str(),
            _ => return false,
        };
        if tool_call_id.is_empty() {
            return false;
        }
        let id = tool_call_id.to_string();
        let is_new = !self.calls.contains_key(&id);
        if is_new {
            self.calls.insert(id.clone(), ToolProjectionState::default());
            self.stats.tool_call_count = self.stats.tool_call_count.saturating_add(1);
        }
        {
            let state = self.calls.get_mut(&id).expect("just inserted or existing");
            match event {
                AcpEvent::ToolCall {
                    kind,
                    title,
                    raw_input,
                    raw_output,
                    locations,
                    meta,
                    ..
                } => {
                    state.kind = Some(kind.clone());
                    state.title = Some(title.clone());
                    if let Some(raw) = raw_input {
                        state.raw_input = Some(raw.clone());
                    }
                    if let Some(raw) = raw_output {
                        merge_structured_result(state, raw, false);
                    }
                    if let Some(loc) = locations {
                        state.locations = Some(loc.clone());
                    }
                    if let Some(m) = meta {
                        state.meta = Some(m.clone());
                    }
                }
                AcpEvent::ToolCallUpdate {
                    title,
                    raw_input,
                    raw_output,
                    raw_output_append,
                    locations,
                    meta,
                    ..
                } => {
                    if let Some(t) = title {
                        state.title = Some(t.clone());
                    }
                    if let Some(raw) = raw_input {
                        state.raw_input = Some(raw.clone());
                    }
                    if let Some(raw) = raw_output {
                        let is_append = raw_output_append == &Some(true);
                        merge_structured_result(state, raw, is_append);
                    }
                    if let Some(loc) = locations {
                        state.locations = Some(loc.clone());
                    }
                    if let Some(m) = meta {
                        state.meta = Some(m.clone());
                    }
                }
                _ => {}
            }
        }

        let previous = self
            .contributions
            .remove(&id)
            .unwrap_or_default();
        self.subtract_contribution(&previous);

        let state = self.calls.get(&id).cloned().unwrap_or_default();
        let next = self.compute_contribution(&state);
        self.add_contribution(&next);
        self.contributions.insert(id, next);
        self.rebuild_public_fields();
        self.stats != before
    }

    fn subtract_contribution(&mut self, contrib: &ToolContribution) {
        if contrib.edit {
            self.stats.edit_tool_call_count = self.stats.edit_tool_call_count.saturating_sub(1);
        }
        if contrib.textual_edit {
            match (contrib.additions, contrib.deletions) {
                (Some(add), Some(del)) => {
                    self.textual_edits_with_counts =
                        self.textual_edits_with_counts.saturating_sub(1);
                    self.known_additions = self.known_additions.saturating_sub(u128::from(add));
                    self.known_deletions = self.known_deletions.saturating_sub(u128::from(del));
                }
                _ => {
                    self.textual_edits_without_counts =
                        self.textual_edits_without_counts.saturating_sub(1);
                }
            }
        }
        for path in &contrib.retained_paths {
            if path.textual_edit {
                match (path.additions, path.deletions) {
                    (Some(add), Some(del)) => {
                        if let Some(c) =
                            self.retained_path_textual_with_counts.get_mut(&path.key)
                        {
                            *c = c.saturating_sub(1);
                            if *c == 0 {
                                self.retained_path_textual_with_counts.remove(&path.key);
                            }
                        }
                        if let Some(v) = self.retained_path_additions.get_mut(&path.key) {
                            *v = v.saturating_sub(u128::from(add));
                        }
                        if let Some(v) = self.retained_path_deletions.get_mut(&path.key) {
                            *v = v.saturating_sub(u128::from(del));
                        }
                    }
                    _ => {
                        if let Some(c) =
                            self.retained_path_textual_without_counts.get_mut(&path.key)
                        {
                            *c = c.saturating_sub(1);
                            if *c == 0 {
                                self.retained_path_textual_without_counts
                                    .remove(&path.key);
                            }
                        }
                    }
                }
            }
            if let Some(count) = self.retained_path_ref_counts.get_mut(&path.key) {
                *count = count.saturating_sub(1);
                if *count == 0 {
                    self.retained_path_ref_counts.remove(&path.key);
                    self.retained_path_additions.remove(&path.key);
                    self.retained_path_deletions.remove(&path.key);
                    self.retained_path_textual_with_counts.remove(&path.key);
                    self.retained_path_textual_without_counts.remove(&path.key);
                    // Keep first_display + order as bounded history.
                }
            }
        }
    }

    fn add_contribution(&mut self, contrib: &ToolContribution) {
        if contrib.edit {
            self.stats.edit_tool_call_count = self.stats.edit_tool_call_count.saturating_add(1);
        }
        if contrib.textual_edit {
            match (contrib.additions, contrib.deletions) {
                (Some(add), Some(del)) => {
                    self.textual_edits_with_counts =
                        self.textual_edits_with_counts.saturating_add(1);
                    self.known_additions =
                        self.known_additions.saturating_add(u128::from(add));
                    self.known_deletions =
                        self.known_deletions.saturating_add(u128::from(del));
                }
                _ => {
                    self.textual_edits_without_counts =
                        self.textual_edits_without_counts.saturating_add(1);
                }
            }
        }
        for path in &contrib.retained_paths {
            let entry = self
                .retained_path_ref_counts
                .entry(path.key.clone())
                .or_insert(0);
            *entry = entry.saturating_add(1);
            if !self.retained_path_first_display.contains_key(&path.key) {
                if self.retained_path_order.len() < MAX_TOUCHED_FILES {
                    self.retained_path_order.push(path.key.clone());
                    self.retained_path_first_display
                        .insert(path.key.clone(), path.display.clone());
                } else {
                    self.overflow_seen = true;
                }
            }
            if path.textual_edit {
                match (path.additions, path.deletions) {
                    (Some(add), Some(del)) => {
                        *self
                            .retained_path_textual_with_counts
                            .entry(path.key.clone())
                            .or_insert(0) += 1;
                        *self
                            .retained_path_additions
                            .entry(path.key.clone())
                            .or_insert(0) += u128::from(add);
                        *self
                            .retained_path_deletions
                            .entry(path.key.clone())
                            .or_insert(0) += u128::from(del);
                    }
                    _ => {
                        *self
                            .retained_path_textual_without_counts
                            .entry(path.key.clone())
                            .or_insert(0) += 1;
                    }
                }
            }
        }
    }

    fn rebuild_public_fields(&mut self) {
        self.stats.touched_files_truncated = self.overflow_seen;
        self.stats.touched_files = self
            .retained_path_order
            .iter()
            .filter(|key| {
                self.retained_path_ref_counts
                    .get(*key)
                    .is_some_and(|c| *c > 0)
            })
            .filter_map(|key| {
                let mut display = self.retained_path_first_display.get(key)?.clone();
                let with = self
                    .retained_path_textual_with_counts
                    .get(key)
                    .copied()
                    .unwrap_or(0);
                let without = self
                    .retained_path_textual_without_counts
                    .get(key)
                    .copied()
                    .unwrap_or(0);
                if with > 0 && without == 0 {
                    let add = self.retained_path_additions.get(key).copied().unwrap_or(0);
                    let del = self.retained_path_deletions.get(key).copied().unwrap_or(0);
                    display.additions = Some(add.min(u128::from(u64::MAX)) as u64);
                    display.deletions = Some(del.min(u128::from(u64::MAX)) as u64);
                } else {
                    display.additions = None;
                    display.deletions = None;
                }
                Some(display)
            })
            .collect();

        if self.textual_edits_with_counts > 0 && self.textual_edits_without_counts == 0 {
            self.stats.additions =
                Some(self.known_additions.min(u128::from(u64::MAX)) as u64);
            self.stats.deletions =
                Some(self.known_deletions.min(u128::from(u64::MAX)) as u64);
            self.stats.line_counts_complete = true;
        } else {
            self.stats.additions = None;
            self.stats.deletions = None;
            self.stats.line_counts_complete = false;
        }
    }

    fn compute_contribution(&self, state: &ToolProjectionState) -> ToolContribution {
        if !is_structured_edit(state) {
            return ToolContribution::default();
        }
        let pure_move = is_pure_move_or_rename(state);
        let textual_edit = !pure_move;

        let mut path_map: HashMap<String, PathContribution> = HashMap::new();
        let mut path_order: Vec<String> = Vec::new();

        let mut insert_path = |raw: &str, path_add: Option<(u64, u64)>| {
            let Some((key, display)) = self.normalize_path(raw) else {
                return;
            };
            if !path_map.contains_key(&key) {
                path_order.push(key.clone());
                path_map.insert(
                    key.clone(),
                    PathContribution {
                        key: key.clone(),
                        display,
                        textual_edit,
                        additions: None,
                        deletions: None,
                    },
                );
            }
            // Per-path counts come only from the single selected evidence;
            // set rather than sum so equivalent representations never stack.
            if let (Some(entry), Some((a, d))) = (path_map.get_mut(&key), path_add) {
                entry.additions = Some(a);
                entry.deletions = Some(d);
            }
        };

        // Path discovery unions every allowed source; line counts do not.
        for path in extract_paths_from_locations(state.locations.as_ref()) {
            insert_path(&path, None);
        }
        for value in [
            state
                .raw_input
                .as_deref()
                .and_then(|raw| serde_json::from_str::<serde_json::Value>(raw).ok()),
            state.meta.clone(),
            state.structured_result.clone(),
        ]
        .into_iter()
        .flatten()
        {
            for path in extract_paths_from_object(&value) {
                insert_path(&path, None);
            }
        }

        // At most one line-count evidence representation per stable call.
        let mut call_additions: Option<u64> = None;
        let mut call_deletions: Option<u64> = None;
        match select_line_evidence(state) {
            Some(SelectedLineEvidence::Changes(per_path)) => {
                let mut sum_a = 0u64;
                let mut sum_d = 0u64;
                for (path, (a, d)) in &per_path {
                    insert_path(path, Some((*a, *d)));
                    sum_a = sum_a.saturating_add(*a);
                    sum_d = sum_d.saturating_add(*d);
                }
                call_additions = Some(sum_a);
                call_deletions = Some(sum_d);
            }
            Some(SelectedLineEvidence::Aggregate(add, del)) => {
                call_additions = Some(add);
                call_deletions = Some(del);
                // Attribute call-level counts to a single unattributed path.
                let unattributed: Vec<_> = path_order
                    .iter()
                    .filter(|k| {
                        path_map
                            .get(*k)
                            .is_some_and(|p| p.additions.is_none() && p.deletions.is_none())
                    })
                    .cloned()
                    .collect();
                if unattributed.len() == 1 {
                    if let Some(entry) = path_map.get_mut(&unattributed[0]) {
                        entry.additions = Some(add);
                        entry.deletions = Some(del);
                    }
                }
            }
            None => {}
        }

        // Paths without per-path counts: for textual edits leave None so
        // per-file public values stay absent unless attributed.
        let retained_paths: Vec<PathContribution> = path_order
            .into_iter()
            .filter_map(|k| path_map.remove(&k))
            .map(|mut p| {
                if !textual_edit {
                    p.textual_edit = false;
                    p.additions = None;
                    p.deletions = None;
                } else {
                    p.textual_edit = true;
                    // Keep per-path counts only when both sides present.
                    if p.additions.is_some() != p.deletions.is_some() {
                        p.additions = None;
                        p.deletions = None;
                    }
                }
                p
            })
            .collect();

        let (additions, deletions) = if textual_edit {
            match (call_additions, call_deletions) {
                (Some(a), Some(d)) => (Some(a), Some(d)),
                _ => {
                    // Incomplete textual edit when no usable counts.
                    (None, None)
                }
            }
        } else {
            (None, None)
        };

        // For path contributions on textual edits without any call-level or
        // per-path counts: mark as textual without counts (additions None).
        let retained_paths: Vec<PathContribution> = retained_paths
            .into_iter()
            .map(|mut p| {
                if textual_edit && p.additions.is_none() {
                    // uncounted textual path contribution
                    p.textual_edit = true;
                }
                p
            })
            .collect();

        ToolContribution {
            edit: true,
            textual_edit,
            retained_paths,
            additions,
            deletions,
        }
    }

    fn normalize_path(&self, raw: &str) -> Option<(String, DelegationTouchedFile)> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return None;
        }
        let path = PathBuf::from(trimmed);
        let absolute = if path.is_absolute() {
            lexical_normalize(&path)
        } else {
            lexical_normalize(&self.workspace.join(&path))
        };
        let relative =
            lexical_strip_prefix(&absolute, &self.workspace, self.case_insensitive_paths);
        let outside = relative.is_none();
        let display_path = if let Some(rel) = relative {
            if rel.as_os_str().is_empty() {
                ".".to_string()
            } else {
                rel.to_string_lossy().replace('\\', "/")
            }
        } else {
            absolute.to_string_lossy().replace('\\', "/")
        };
        let key = if self.case_insensitive_paths {
            display_path.to_lowercase()
        } else {
            display_path.clone()
        };
        let display = DelegationTouchedFile {
            path: display_path,
            outside_workspace: outside,
            additions: None,
            deletions: None,
        };
        Some((key, display))
    }
}

fn merge_structured_result(state: &mut ToolProjectionState, raw: &str, is_append: bool) {
    if is_append {
        // Opaque/append chunks never clear a previously parsed result.
        return;
    }
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(raw) {
        if value.is_object() {
            state.structured_result = Some(value);
        }
        // Opaque non-object JSON replacement: do not clear prior evidence.
    }
    // Opaque text: do not clear prior evidence.
}

fn is_structured_edit(state: &ToolProjectionState) -> bool {
    if structured_diff_object(state.structured_result.as_ref()).is_some() {
        // An explicit structured result may prove that even an execute-kind
        // wrapper applied a mutation; no command text is inspected.
        return true;
    }
    if matches!(
        state.kind.as_deref(),
        Some("command" | "shell" | "execute")
    ) {
        return false;
    }
    matches!(
        state.kind.as_deref(),
        Some("edit" | "write" | "create" | "delete" | "move" | "rename" | "patch")
    ) || matches!(
        canonical_tool_name(state).as_deref(),
        Some(
            "edit"
                | "write"
                | "write_file"
                | "create_file"
                | "write_to_file"
                | "delete_file"
                | "move_file"
                | "rename_file"
                | "apply_patch"
                | "str_replace"
                | "replace_in_file"
        )
    ) || structured_mutation_input(state.raw_input.as_deref())
        || structured_diff_object(state.meta.as_ref()).is_some()
}

fn is_pure_move_or_rename(state: &ToolProjectionState) -> bool {
    let by_kind = matches!(state.kind.as_deref(), Some("move" | "rename"));
    let by_name = matches!(
        canonical_tool_name(state).as_deref(),
        Some("move_file" | "rename_file")
    );
    if !(by_kind || by_name) {
        return false;
    }
    // Pure when no usable textual payload is present.
    !has_usable_textual_payload(state)
}

fn has_usable_textual_payload(state: &ToolProjectionState) -> bool {
    select_line_evidence(state).is_some()
}

fn canonical_tool_name(state: &ToolProjectionState) -> Option<String> {
    if matches!(
        state.kind.as_deref(),
        Some("command" | "shell" | "execute")
    ) {
        return None;
    }
    let title_name = state
        .title
        .as_deref()
        .map(str::trim)
        .filter(|title| {
            !title.is_empty()
                && !matches!(
                    title.to_ascii_lowercase().as_str(),
                    "tool" | "use_tool" | "mcp"
                )
        });
    let meta_name = state.meta.as_ref().and_then(|meta| {
        meta.get("tool_name")
            .or_else(|| meta.get("toolName"))
            .and_then(serde_json::Value::as_str)
            .or_else(|| {
                meta.get("x.ai/tool")
                    .and_then(|tool| tool.get("name"))
                    .and_then(serde_json::Value::as_str)
            })
    });
    let raw = title_name.or(meta_name)?.trim();
    if raw.is_empty() {
        return None;
    }
    let normalized = raw
        .to_ascii_lowercase()
        .replace('-', "_")
        .replace(' ', "_");
    normalized
        .rsplit("__")
        .next()
        .filter(|name| !name.is_empty())
        .map(str::to_string)
}

fn structured_mutation_input(raw_input: Option<&str>) -> bool {
    raw_input
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(raw).ok())
        .as_ref()
        .is_some_and(has_structured_mutation)
}

fn structured_diff_object(
    value: Option<&serde_json::Value>,
) -> Option<&serde_json::Map<String, serde_json::Value>> {
    let value = value?;
    [
        Some(value),
        value.get("structuredContent"),
        value.get("result"),
        value
            .get("result")
            .and_then(|result| result.get("structuredContent")),
        value.get("output"),
        value
            .get("output")
            .and_then(|output| output.get("structuredContent")),
    ]
    .into_iter()
    .flatten()
    .find_map(|candidate| {
        has_structured_mutation(candidate)
            .then(|| candidate.as_object())
            .flatten()
    })
}

fn has_structured_mutation(value: &serde_json::Value) -> bool {
    let Some(object) = value.as_object() else {
        return false;
    };
    if object.contains_key("command") {
        return false;
    }
    let has_path = [
        "file_path",
        "filePath",
        "path",
        "notebook_path",
        "move_path",
        "movePath",
    ]
    .iter()
    .any(|key| object.get(*key).and_then(serde_json::Value::as_str).is_some());
    let has_payload = [
        "old_string",
        "new_string",
        "old_text",
        "new_text",
        "content",
        "new_source",
        "patch",
        "diff",
        "unified_diff",
        "unifiedDiff",
    ]
    .iter()
    .any(|key| object.get(*key).is_some_and(|item| !item.is_null()));
    let has_changes = object
        .get("changes")
        .and_then(serde_json::Value::as_object)
        .is_some_and(|changes| {
            !changes.is_empty()
                && changes.values().any(|entry| {
                    entry.is_string()
                        || entry.as_object().is_some_and(|change| {
                            [
                                "old_text",
                                "new_text",
                                "old_string",
                                "new_string",
                                "content",
                                "new_source",
                                "patch",
                                "diff",
                            ]
                            .iter()
                            .any(|key| change.get(*key).is_some_and(|item| !item.is_null()))
                        })
                })
        });
    let has_explicit_diff = ["patch", "diff", "unified_diff", "unifiedDiff"]
        .iter()
        .any(|key| object.get(*key).is_some_and(|item| !item.is_null()));
    (has_path && has_payload) || has_changes || has_explicit_diff
}

fn lexical_normalize(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                if matches!(
                    normalized.components().next_back(),
                    Some(Component::Normal(_))
                ) {
                    normalized.pop();
                } else if !normalized.has_root() {
                    normalized.push("..");
                }
            }
            Component::Normal(value) => normalized.push(value),
        }
    }
    normalized
}

fn lexical_strip_prefix(
    path: &Path,
    workspace: &Path,
    case_insensitive: bool,
) -> Option<PathBuf> {
    if !case_insensitive {
        return path.strip_prefix(workspace).ok().map(Path::to_path_buf);
    }
    let path_components = path.components().collect::<Vec<_>>();
    let workspace_components = workspace.components().collect::<Vec<_>>();
    if workspace_components.len() > path_components.len()
        || !workspace_components
            .iter()
            .zip(&path_components)
            .all(|(left, right)| {
                left.as_os_str().to_string_lossy().to_lowercase()
                    == right.as_os_str().to_string_lossy().to_lowercase()
            })
    {
        return None;
    }
    Some(
        path_components
            .into_iter()
            .skip(workspace_components.len())
            .map(|component| component.as_os_str())
            .collect(),
    )
}

fn extract_paths_from_locations(locations: Option<&serde_json::Value>) -> Vec<String> {
    let Some(arr) = locations.and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|item| {
            item.get("path")
                .and_then(|p| p.as_str())
                .map(str::to_string)
        })
        .collect()
}

fn extract_paths_from_object(value: &serde_json::Value) -> Vec<String> {
    let mut paths = Vec::new();
    let candidates = [
        Some(value),
        value.get("structuredContent"),
        value.get("result"),
        value
            .get("result")
            .and_then(|result| result.get("structuredContent")),
        value.get("output"),
        value
            .get("output")
            .and_then(|output| output.get("structuredContent")),
    ];
    for candidate in candidates.into_iter().flatten() {
        if let Some(object) = candidate.as_object() {
            for key in [
                "file_path",
                "filePath",
                "path",
                "notebook_path",
                "move_path",
                "movePath",
            ] {
                if let Some(p) = object.get(key).and_then(|v| v.as_str()) {
                    paths.push(p.to_string());
                }
            }
            if let Some(changes) = object.get("changes").and_then(|c| c.as_object()) {
                for path in changes.keys() {
                    paths.push(path.clone());
                }
            }
        }
    }
    paths
}

/// One selected line-count evidence representation for a stable tool call.
/// Paths may still be discovered from every source; counts come only from here.
#[derive(Debug, Clone)]
enum SelectedLineEvidence {
    /// Fully countable `changes` map: each entry contributes per-path counts.
    Changes(Vec<(String, (u64, u64))>),
    /// Call-level counts from one patch/diff, old/new, or content write.
    Aggregate(u64, u64),
}

/// Source precedence: structured_result > meta > parsed raw_input.
/// Within one source: complete changes > first patch/diff key > old/new > content.
/// Never sums equivalent wrapper candidates, keys, or sources.
fn select_line_evidence(state: &ToolProjectionState) -> Option<SelectedLineEvidence> {
    let sources = [
        state.structured_result.clone(),
        state.meta.clone(),
        state
            .raw_input
            .as_deref()
            .and_then(|raw| serde_json::from_str::<serde_json::Value>(raw).ok()),
    ];
    for source in sources.into_iter().flatten() {
        if let Some(evidence) = select_line_evidence_from_source(&source) {
            return Some(evidence);
        }
    }
    None
}

fn wrapper_candidates(value: &serde_json::Value) -> [Option<&serde_json::Value>; 6] {
    [
        Some(value),
        value.get("structuredContent"),
        value.get("result"),
        value
            .get("result")
            .and_then(|result| result.get("structuredContent")),
        value.get("output"),
        value
            .get("output")
            .and_then(|output| output.get("structuredContent")),
    ]
}

fn select_line_evidence_from_source(value: &serde_json::Value) -> Option<SelectedLineEvidence> {
    // 1. Prefer one fully countable changes map (first wrapper that qualifies).
    for candidate in wrapper_candidates(value).into_iter().flatten() {
        if let Some(per_path) = fully_countable_changes(candidate) {
            return Some(SelectedLineEvidence::Changes(per_path));
        }
    }
    // 2. First usable patch / diff / unified_diff / unifiedDiff.
    for candidate in wrapper_candidates(value).into_iter().flatten() {
        let Some(object) = candidate.as_object() else {
            continue;
        };
        for key in ["patch", "diff", "unified_diff", "unifiedDiff"] {
            if let Some(text) = object.get(key).and_then(|v| v.as_str()) {
                let (a, d) = count_unified_diff_lines(text);
                return Some(SelectedLineEvidence::Aggregate(a, d));
            }
        }
    }
    // 3. One old/new pair.
    for candidate in wrapper_candidates(value).into_iter().flatten() {
        let Some(object) = candidate.as_object() else {
            continue;
        };
        if let Some((a, d)) = line_counts_from_old_new(object) {
            return Some(SelectedLineEvidence::Aggregate(a, d));
        }
    }
    // 4. One content / new_source write.
    for candidate in wrapper_candidates(value).into_iter().flatten() {
        let Some(object) = candidate.as_object() else {
            continue;
        };
        if let Some(content) = object
            .get("content")
            .or_else(|| object.get("new_source"))
            .and_then(|v| v.as_str())
        {
            let (a, d) = count_write_content_lines(content);
            return Some(SelectedLineEvidence::Aggregate(a, d));
        }
    }
    None
}

/// Returns per-path counts only when every entry in `changes` is countable.
/// A partially countable map is not selected; callers fall through to other keys.
fn fully_countable_changes(value: &serde_json::Value) -> Option<Vec<(String, (u64, u64))>> {
    let object = value.as_object()?;
    let changes = object.get("changes")?.as_object()?;
    if changes.is_empty() {
        return None;
    }
    let mut out = Vec::with_capacity(changes.len());
    for (path, entry) in changes {
        let (a, d) = line_counts_from_change_entry(entry)?;
        out.push((path.clone(), (a, d)));
    }
    Some(out)
}

fn line_counts_from_change_entry(entry: &serde_json::Value) -> Option<(u64, u64)> {
    if let Some(text) = entry.as_str() {
        return Some(count_unified_diff_lines(text));
    }
    let object = entry.as_object()?;
    if let Some((a, d)) = line_counts_from_old_new(object) {
        return Some((a, d));
    }
    for key in ["patch", "diff", "unified_diff", "unifiedDiff"] {
        if let Some(text) = object.get(key).and_then(|v| v.as_str()) {
            return Some(count_unified_diff_lines(text));
        }
    }
    if let Some(content) = object
        .get("content")
        .or_else(|| object.get("new_source"))
        .and_then(|v| v.as_str())
    {
        return Some(count_write_content_lines(content));
    }
    None
}

fn line_counts_from_old_new(
    object: &serde_json::Map<String, serde_json::Value>,
) -> Option<(u64, u64)> {
    let old = object
        .get("old_text")
        .or_else(|| object.get("old_string"))
        .and_then(|v| v.as_str());
    let new = object
        .get("new_text")
        .or_else(|| object.get("new_string"))
        .and_then(|v| v.as_str());
    match (old, new) {
        (Some(o), Some(n)) => Some(count_text_diff_lines(o, n)),
        _ => None,
    }
}

fn count_text_diff_lines(old: &str, new: &str) -> (u64, u64) {
    // Split with `lines()` so trailing-newline differences do not turn an
    // equal line into a delete+insert pair under TextDiff::from_lines.
    let old_lines: Vec<&str> = old.lines().collect();
    let new_lines: Vec<&str> = new.lines().collect();
    let diff = TextDiff::from_slices(&old_lines, &new_lines);
    let mut add = 0u64;
    let mut del = 0u64;
    for change in diff.iter_all_changes() {
        match change.tag() {
            ChangeTag::Insert => add = add.saturating_add(1),
            ChangeTag::Delete => del = del.saturating_add(1),
            ChangeTag::Equal => {}
        }
    }
    (add, del)
}

fn count_write_content_lines(content: &str) -> (u64, u64) {
    let (add, _) = count_text_diff_lines("", content);
    (add, 0)
}

fn count_unified_diff_lines(diff: &str) -> (u64, u64) {
    let mut add = 0u64;
    let mut del = 0u64;
    for line in diff.split('\n') {
        let trimmed_cr = line.strip_suffix('\r').unwrap_or(line);
        if trimmed_cr == "+++" || trimmed_cr == "---" {
            continue;
        }
        if trimmed_cr.starts_with("+++ ") || trimmed_cr.starts_with("--- ") {
            continue;
        }
        if trimmed_cr.starts_with('+') {
            add = add.saturating_add(1);
        } else if trimmed_cr.starts_with('-') {
            del = del.saturating_add(1);
        }
    }
    (add, del)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use serde_json::json;
    use std::path::PathBuf;

    use crate::acp::types::AcpEvent;

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

    fn tool_call(
        id: &str,
        kind: &str,
        title: &str,
        locations: Option<serde_json::Value>,
        meta: Option<serde_json::Value>,
    ) -> AcpEvent {
        AcpEvent::ToolCall {
            tool_call_id: id.to_string(),
            title: title.to_string(),
            kind: kind.to_string(),
            status: "in_progress".into(),
            content: None,
            raw_input: None,
            raw_output: None,
            locations,
            meta,
            images: None,
        }
    }

    fn tool_update(
        id: &str,
        title: Option<&str>,
        locations: Option<serde_json::Value>,
        meta: Option<serde_json::Value>,
    ) -> AcpEvent {
        AcpEvent::ToolCallUpdate {
            tool_call_id: id.to_string(),
            title: title.map(str::to_string),
            status: None,
            content: None,
            raw_input: None,
            raw_output: None,
            raw_output_append: None,
            locations,
            meta,
            images: None,
        }
    }

    fn tool_update_with_output(
        id: &str,
        raw_output: Option<&str>,
        raw_output_append: Option<bool>,
    ) -> AcpEvent {
        AcpEvent::ToolCallUpdate {
            tool_call_id: id.to_string(),
            title: None,
            status: Some("completed".into()),
            content: None,
            raw_input: None,
            raw_output: raw_output.map(str::to_string),
            raw_output_append,
            locations: None,
            meta: None,
            images: None,
        }
    }

    fn tool_call_with_input(id: &str, kind: &str, title: &str, raw_input: &str) -> AcpEvent {
        let mut event = tool_call(id, kind, title, None, None);
        if let AcpEvent::ToolCall { raw_input: slot, .. } = &mut event {
            *slot = Some(raw_input.to_string());
        }
        event
    }

    fn edit_with_paths(id: &str, paths: Vec<&str>) -> AcpEvent {
        tool_call(
            id,
            "edit",
            "edit",
            Some(json!(paths
                .into_iter()
                .map(|path| json!({"path": path}))
                .collect::<Vec<_>>())),
            None,
        )
    }

    fn structured_patch(id: &str, path: &str, diff: &str) -> AcpEvent {
        tool_call_with_input(
            id,
            "edit",
            "apply_patch",
            &json!({"file_path": path, "diff": diff}).to_string(),
        )
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

    #[test]
    fn stable_id_counts_once_and_sparse_updates_enrich_the_same_call() {
        let started = Utc::now();
        let workspace = if cfg!(windows) {
            PathBuf::from(r"C:\repo")
        } else {
            PathBuf::from("/repo")
        };
        let file = workspace.join("src").join("lib.rs");
        let file_text = file.to_string_lossy().to_string();
        let changes = serde_json::Map::from_iter([(
            file_text.clone(),
            json!({"old_text": "a", "new_text": "a\nb"}),
        )]);
        let mut projector = RuntimeStatsProjector::new(started, workspace);
        assert!(projector.apply(&tool_call(
            "tc-1",
            "read",
            "Read",
            Some(json!([{"path": file_text.clone()}])),
            None,
        )));
        assert!(!projector.apply(&tool_update("tc-1", None, None, None)));
        assert!(projector.apply(&tool_update(
            "tc-1",
            Some("edit"),
            Some(json!([{"path": file_text.clone()}])),
            Some(json!({"changes": changes})),
        )));
        assert!(!projector.apply(&tool_call(
            "tc-1",
            "read",
            "Read",
            Some(json!([{"path": file_text}])),
            None,
        )));
        let stats = projector.snapshot();
        assert_eq!(stats.tool_call_count, 1);
        assert_eq!(stats.edit_tool_call_count, 1);
        assert_eq!(stats.touched_files.len(), 1);
        assert_eq!(stats.touched_files[0].path.replace('\\', "/"), "src/lib.rs");
        assert_eq!((stats.additions, stats.deletions), (Some(1), Some(0)));
    }

    #[test]
    fn read_locations_and_shell_text_never_claim_edits() {
        let mut projector = RuntimeStatsProjector::new(Utc::now(), PathBuf::from(r"C:\repo"));
        projector.apply(&tool_call(
            "read-1",
            "read",
            "Read",
            Some(json!([{"path": r"C:\repo\a.rs"}])),
            None,
        ));
        projector.apply(&tool_call_with_input(
            "shell-1",
            "execute",
            "exec_command",
            r#"{"command":"Set-Content a.rs changed"}"#,
        ));
        let stats = projector.snapshot();
        assert_eq!(stats.tool_call_count, 2);
        assert_eq!(stats.edit_tool_call_count, 0);
        assert!(stats.touched_files.is_empty());
        assert!(!stats.line_counts_complete);
    }

    #[test]
    fn complete_structured_result_can_classify_an_execute_call_without_reading_command_text() {
        let mut projector = RuntimeStatsProjector::new(Utc::now(), PathBuf::from("/repo"));
        projector.apply(&tool_call_with_input(
            "shell-structured",
            "execute",
            "exec_command",
            r#"{"command":"opaque command text"}"#,
        ));
        projector.apply(&tool_update_with_output(
            "shell-structured",
            Some(r#"{"changes":{"/repo/a.rs":{"old_text":"a","new_text":"a\nb"}}}"#),
            Some(false),
        ));
        let stats = projector.snapshot();
        assert_eq!(stats.tool_call_count, 1);
        assert_eq!(stats.edit_tool_call_count, 1);
        assert_eq!(stats.touched_files[0].path, "a.rs");
        assert_eq!((stats.additions, stats.deletions), (Some(1), Some(0)));

        // Opaque append must not clear or mutate structured-result evidence.
        let before = projector.snapshot();
        assert!(!projector.apply(&tool_update_with_output(
            "shell-structured",
            Some("trailing opaque suffix"),
            Some(true),
        )));
        assert_eq!(projector.snapshot(), before);

        // Replayed start still counts once and keeps structured-result evidence.
        assert!(!projector.apply(&tool_call_with_input(
            "shell-structured",
            "execute",
            "exec_command",
            r#"{"command":"opaque command text"}"#,
        )));
        let after_replay = projector.snapshot();
        assert_eq!(after_replay.tool_call_count, 1);
        assert_eq!(after_replay.edit_tool_call_count, 1);
        assert_eq!(after_replay.touched_files[0].path, "a.rs");
        assert_eq!(
            (after_replay.additions, after_replay.deletions),
            (Some(1), Some(0))
        );
    }

    #[test]
    fn equivalent_line_evidence_is_counted_once_per_stable_call() {
        let mut projector = RuntimeStatsProjector::new(Utc::now(), PathBuf::from("/repo"));
        projector.apply(&tool_call_with_input(
            "duplicate-evidence",
            "edit",
            "apply_patch",
            r#"{"file_path":"/repo/a.rs","old_text":"a","new_text":"a\nb"}"#,
        ));
        projector.apply(&tool_update_with_output(
            "duplicate-evidence",
            Some(
                r#"{"changes":{"/repo/a.rs":{"old_text":"a","new_text":"a\nb"}},"diff":" a\n+b"}"#,
            ),
            Some(false),
        ));
        let stats = projector.snapshot();
        assert_eq!(stats.edit_tool_call_count, 1);
        assert_eq!((stats.additions, stats.deletions), (Some(1), Some(0)));
        assert_eq!(
            (stats.touched_files[0].additions, stats.touched_files[0].deletions),
            (Some(1), Some(0))
        );
    }

    #[test]
    fn line_counts_fall_back_to_meta_when_structured_result_has_none() {
        let mut projector = RuntimeStatsProjector::new(Utc::now(), PathBuf::from("/repo"));
        // Path-only structured result (classifies as edit via kind, no counts).
        projector.apply(&tool_call(
            "meta-fallback",
            "edit",
            "edit",
            Some(json!([{"path": "/repo/a.rs"}])),
            Some(json!({
                "file_path": "/repo/a.rs",
                "old_text": "a",
                "new_text": "a\nb"
            })),
        ));
        // Structured result without usable line evidence should not block meta.
        projector.apply(&tool_update_with_output(
            "meta-fallback",
            Some(r#"{"ok":true}"#),
            Some(false),
        ));
        let stats = projector.snapshot();
        assert_eq!(stats.edit_tool_call_count, 1);
        assert_eq!((stats.additions, stats.deletions), (Some(1), Some(0)));
        assert_eq!(
            (stats.touched_files[0].additions, stats.touched_files[0].deletions),
            (Some(1), Some(0))
        );
    }

    #[test]
    fn line_counts_fall_back_to_raw_input_when_higher_sources_have_none() {
        let mut projector = RuntimeStatsProjector::new(Utc::now(), PathBuf::from("/repo"));
        projector.apply(&tool_call_with_input(
            "raw-fallback",
            "edit",
            "apply_patch",
            r#"{"file_path":"/repo/a.rs","old_text":"a","new_text":"a\nb"}"#,
        ));
        // Opaque non-count structured_result must not clear raw_input counts.
        projector.apply(&tool_update_with_output(
            "raw-fallback",
            Some(r#"{"status":"done"}"#),
            Some(false),
        ));
        let stats = projector.snapshot();
        assert_eq!(stats.edit_tool_call_count, 1);
        assert_eq!((stats.additions, stats.deletions), (Some(1), Some(0)));
        assert_eq!(
            (stats.touched_files[0].additions, stats.touched_files[0].deletions),
            (Some(1), Some(0))
        );
    }

    #[test]
    fn partially_countable_changes_fall_through_to_sibling_diff_key() {
        let mut projector = RuntimeStatsProjector::new(Utc::now(), PathBuf::from("/repo"));
        projector.apply(&tool_call_with_input(
            "partial-changes",
            "edit",
            "apply_patch",
            &json!({
                "file_path": "/repo/a.rs",
                "changes": {
                    "/repo/a.rs": {"note": "no countable payload"}
                },
                "diff": " a\n+b"
            })
            .to_string(),
        ));
        let stats = projector.snapshot();
        assert_eq!(stats.edit_tool_call_count, 1);
        assert_eq!((stats.additions, stats.deletions), (Some(1), Some(0)));
        assert_eq!(
            (stats.touched_files[0].additions, stats.touched_files[0].deletions),
            (Some(1), Some(0))
        );
    }

    #[test]
    fn case_insensitive_paths_dedupe_and_cap_at_two_hundred() {
        let mut projector = RuntimeStatsProjector::new_for_test(
            Utc::now(),
            PathBuf::from("/repo"),
            true,
        );
        projector.apply(&edit_with_paths(
            "tc-case",
            vec!["A.rs", "a.rs"],
        ));
        for i in 0..MAX_TOUCHED_FILES + 5 {
            projector.apply(&edit_with_paths(
                &format!("tc-{i}"),
                vec![&format!("file-{i}.rs")],
            ));
        }
        let stats = projector.snapshot();
        assert_eq!(stats.touched_files.len(), MAX_TOUCHED_FILES);
        assert!(stats.touched_files_truncated);
        assert_eq!(stats.touched_files[0].path, "A.rs");
    }

    #[test]
    fn partial_textual_line_metadata_suppresses_aggregate_totals() {
        let mut projector = RuntimeStatsProjector::new(Utc::now(), PathBuf::from("/repo"));
        projector.apply(&structured_patch("known", "/repo/a.rs", "+one\n-two"));
        projector.apply(&edit_with_paths("unknown", vec!["/repo/b.rs"]));
        let stats = projector.snapshot();
        assert_eq!(stats.edit_tool_call_count, 2);
        assert_eq!(stats.additions, None);
        assert_eq!(stats.deletions, None);
        assert!(!stats.line_counts_complete);
    }

    #[test]
    fn lexical_parent_path_is_marked_outside_without_touching_the_filesystem() {
        let mut projector = RuntimeStatsProjector::new(Utc::now(), PathBuf::from("/repo/work"));
        projector.apply(&edit_with_paths("outside", vec!["../shared/a.rs"]));
        let file = &projector.snapshot().touched_files[0];
        assert!(file.outside_workspace);
        assert!(file.path.replace('\\', "/").ends_with("/repo/shared/a.rs"));
    }
}
