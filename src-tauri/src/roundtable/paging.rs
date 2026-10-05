//! Manifest pages freeze the body they were read from.

use roundtable_protocol::{Hash256, RtResult};

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
    let start = cursor.and_then(|value| value.parse::<usize>().ok()).unwrap_or(0);
    let end = (start + limit as usize).min(entries.len());
    Ok(ManifestPage {
        cursor: end.to_string(),
        limit,
        body_hash,
        entries: entries[start..end].to_vec(),
    })
}
