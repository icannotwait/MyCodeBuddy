//! Backup pins referenced objects. Restore fails closed. GC cannot guess.

use std::collections::{BTreeMap, BTreeSet};

use roundtable_protocol::{ErrorCode, RtError, RtResult};

use crate::commands::backup::manifest::ROUNDTABLE_BACKUP_SECTION;
use crate::commands::backup::restore::refuse_missing_roundtable_object;
use crate::commands::backup::sections::roundtable_section_pinned;
use crate::commands::backup::source::roundtable_source_bytes_are_pinned;

use super::store::RoundtableStore;

const PROTECTED: &[&str] = &[
    "manifest",
    "diagnostic",
    "source",
    "projection",
    "backup",
    "pending",
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrackedObject {
    pub id: String,
    pub hash: String,
    pub age_hours: u64,
    pub referenced: bool,
    pub kind: String,
    pub present: bool,
}

#[derive(Clone, Debug, Default)]
pub struct ReferenceCatalog {
    pub watermark: String,
    pub objects: Vec<TrackedObject>,
    pub capture_ids: BTreeSet<String>,
    pub read_only: bool,
    pub runnable: bool,
}

impl ReferenceCatalog {
    pub fn empty() -> Self {
        Self {
            runnable: true,
            ..Self::default()
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BackupManifest {
    pub section: &'static str,
    pub watermark: String,
    pub referenced: BTreeMap<String, String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GcReport {
    pub deleted: BTreeSet<String>,
}

pub async fn backup_roundtable(store: &RoundtableStore) -> RtResult<BackupManifest> {
    let _ = (roundtable_section_pinned(), roundtable_source_bytes_are_pinned());
    let catalog = store.reference_catalog().lock().expect("catalog").clone();
    let mut referenced = BTreeMap::new();
    for object in &catalog.objects {
        if object.referenced {
            referenced.insert(object.id.clone(), object.hash.clone());
        }
    }
    Ok(BackupManifest {
        section: ROUNDTABLE_BACKUP_SECTION,
        watermark: catalog.watermark,
        referenced,
    })
}

pub async fn restore_roundtable(
    store: &RoundtableStore,
    manifest: &BackupManifest,
) -> RtResult<()> {
    let handle = store.reference_catalog();
    let mut catalog = handle.lock().expect("catalog");
    for (id, hash) in &manifest.referenced {
        let found = catalog.objects.iter().find(|object| object.id == *id);
        let intact = found.is_some_and(|object| object.present && object.hash == *hash);
        if !intact {
            catalog.read_only = true;
            catalog.runnable = false;
            let _ = refuse_missing_roundtable_object();
            return Err(RtError {
                code: ErrorCode::StorageUnavailable,
                message: "roundtable object missing or corrupt".to_string(),
                retryable: false,
                current_revision: None,
                details: roundtable_protocol::ErrorDetails {
                    reason: Some("restore_failed_closed".to_string()),
                    field_errors: Vec::new(),
                },
            });
        }
    }
    Ok(())
}

pub async fn gc_unreferenced(store: &RoundtableStore) -> RtResult<GcReport> {
    let handle = store.reference_catalog();
    let mut catalog = handle.lock().expect("catalog");
    let capture_ids = catalog.capture_ids.clone();
    let mut deleted = BTreeSet::new();
    catalog.objects.retain(|object| {
        let protected = object.referenced
            || PROTECTED.contains(&object.kind.as_str())
            || object.age_hours < 24
            || capture_ids.contains(&object.id);
        if protected {
            true
        } else {
            deleted.insert(object.id.clone());
            false
        }
    });
    Ok(GcReport { deleted })
}
