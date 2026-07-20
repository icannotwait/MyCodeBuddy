//! Shared ownership-rebind types used by both desktop pop-out and
//! `ConnectionManager` (available without `tauri-runtime`).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RebindResult {
    pub rebound_count: usize,
    pub ownership_generation: u64,
    pub operation_id: String,
}
