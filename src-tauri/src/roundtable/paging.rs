//! Manifest pages freeze the body they were read from.

use std::collections::HashMap;

use roundtable_protocol::{
    ErrorCode, ErrorDetails, Hash256, ManifestId, PrincipalId, ProjectionId, RoomId, RtError,
    RtResult,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CursorScope {
    pub principal_id: PrincipalId,
    pub room_id: RoomId,
    pub projection_id: ProjectionId,
    pub manifest_id: ManifestId,
}

#[derive(Default)]
pub struct ScopedCursors {
    entries: HashMap<String, (CursorScope, String, u64)>,
}

impl ScopedCursors {
    pub fn issue(&mut self, scope: CursorScope, cursor: String, now_ms: u64) -> RtResult<String> {
        self.entries.retain(|_, (_, _, expires)| *expires > now_ms);
        if self.entries.len() >= 10_000 {
            return Err(invalid_cursor("cursor_capacity"));
        }
        let expires = now_ms
            .checked_add(15 * 60 * 1000)
            .ok_or_else(|| invalid_cursor("cursor_time"))?;
        let token = uuid::Uuid::new_v4().to_string();
        self.entries.insert(token.clone(), (scope, cursor, expires));
        Ok(token)
    }

    pub fn resolve(&self, token: &str, scope: &CursorScope, now_ms: u64) -> RtResult<String> {
        let (stored_scope, cursor, expires) = self
            .entries
            .get(token)
            .ok_or_else(|| invalid_cursor("page_cursor"))?;
        if scope != stored_scope || now_ms >= *expires {
            return Err(invalid_cursor("cursor_scope"));
        }
        Ok(cursor.clone())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ManifestPage {
    pub cursor: String,
    pub limit: u32,
    pub body_hash: Hash256,
    pub entries: Vec<String>,
}

pub fn read_manifest_page(
    entries: &[String],
    body_hash: Hash256,
    cursor: Option<&str>,
    limit: u32,
) -> RtResult<ManifestPage> {
    if limit == 0 || limit > 100 {
        return Err(invalid_cursor("page_limit"));
    }
    let start = match cursor {
        None => 0,
        Some(value) => {
            let (hash, offset) = value
                .strip_prefix("v1:")
                .and_then(|value| value.split_once(':'))
                .ok_or_else(|| invalid_cursor("page_cursor"))?;
            if hash != body_hash.to_hex() {
                return Err(invalid_cursor("cursor_object"));
            }
            if offset.is_empty()
                || !offset.bytes().all(|byte| byte.is_ascii_digit())
                || (offset.len() > 1 && offset.starts_with('0'))
            {
                return Err(invalid_cursor("page_cursor"));
            }
            offset
                .parse::<usize>()
                .map_err(|_| invalid_cursor("page_cursor"))?
        }
    };
    if start > entries.len() {
        return Err(invalid_cursor("cursor_offset"));
    }
    // Reserve room for hashes, the opaque cursor and the response envelope.
    let max_bytes = roundtable_protocol::v1_1::MAX_PAGE_BYTES as usize - 4096;
    let upper = start.saturating_add(limit as usize).min(entries.len());
    let mut end = start;
    let mut bytes = 2_usize;
    while end < upper {
        let entry_bytes = entries[end].len().saturating_add(usize::from(end > start));
        if entry_bytes > max_bytes.saturating_sub(bytes) {
            break;
        }
        bytes += entry_bytes;
        end += 1;
    }
    if end == start && start < entries.len() {
        return Err(invalid_cursor("object_page_required"));
    }
    Ok(ManifestPage {
        cursor: format!("v1:{}:{end}", body_hash.to_hex()),
        limit,
        body_hash,
        entries: entries[start..end].to_vec(),
    })
}

fn invalid_cursor(reason: &str) -> RtError {
    RtError {
        code: ErrorCode::InvalidArgument,
        message: "The page cursor is invalid.".to_string(),
        retryable: false,
        current_revision: None,
        details: ErrorDetails {
            reason: Some(reason.to_string()),
            field_errors: Vec::new(),
        },
    }
}
