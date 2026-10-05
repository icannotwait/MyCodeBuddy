//! Backup pins referenced objects. Restore fails closed. GC cannot guess.

use std::collections::{BTreeMap, BTreeSet};

use roundtable_protocol::{ErrorCode, RtResult};

use crate::commands::backup::manifest::ROUNDTABLE_BACKUP_SECTION;

use super::store::RoundtableStore;

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
    use super::store::{column, rows};
    use sea_orm::TransactionTrait;
    let txn = store
        .connection()
        .begin()
        .await
        .map_err(super::store::storage_err)?;
    let mut referenced = BTreeMap::new();
    let mut watermarks = Vec::new();
    for row in rows(
        &txn,
        "SELECT room_id,last_seq FROM rt_rooms ORDER BY room_id",
        vec![],
    )
    .await?
    {
        watermarks.push((column::<String>(&row, 0)?, column::<i64>(&row, 1)?));
    }
    for row in rows(
        &txn,
        "SELECT body_json FROM rt_source_manifests ORDER BY room_id,version",
        vec![],
    )
    .await?
    {
        let raw: String = column(&row, 0)?;
        let manifest: super::snapshot::SourceManifestV1 = serde_json::from_str(&raw)
            .map_err(|_| super::rt_error(ErrorCode::StorageUnavailable, "manifest_corrupt"))?;
        for entry in manifest.entries {
            if entry.object.object_id != entry.object.content_hash.to_hex() {
                return Err(super::rt_error(ErrorCode::StorageUnavailable, "object_id"));
            }
            referenced.insert(entry.object.object_id, entry.object.content_hash.to_hex());
        }
    }
    txn.commit().await.map_err(super::store::storage_err)?;
    Ok(BackupManifest {
        section: ROUNDTABLE_BACKUP_SECTION,
        watermark: roundtable_protocol::canonical_hash(&watermarks)?.to_hex(),
        referenced,
    })
}

pub async fn restore_roundtable(
    store: &RoundtableStore,
    manifest: &BackupManifest,
    objects: &std::path::Path,
) -> RtResult<()> {
    let durable = backup_roundtable(store).await?;
    if durable != *manifest {
        return Err(super::rt_error(
            ErrorCode::StorageUnavailable,
            "restore_manifest_mismatch",
        ));
    }
    for (id, hash) in &manifest.referenced {
        if id != hash || id.len() != 64 || !id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(super::rt_error(ErrorCode::StorageUnavailable, "object_id"));
        }
        let bytes = std::fs::read(objects.join(id))
            .map_err(|_| super::rt_error(ErrorCode::StorageUnavailable, "restore_failed_closed"))?;
        if roundtable_protocol::Hash256::sha256(&bytes).to_hex() != *hash {
            return Err(super::rt_error(
                ErrorCode::StorageUnavailable,
                "restore_failed_closed",
            ));
        }
    }
    Ok(())
}

/// There is no durable capture/backup lease yet. Never infer deletion safety from age.
pub async fn gc_unreferenced(_store: &RoundtableStore) -> RtResult<GcReport> {
    Err(super::rt_error(
        ErrorCode::InvalidState,
        "gc_reference_proof_required",
    ))
}

impl RoundtableStore {
    /// Validate the exact immutable object set referenced by a SQLite snapshot.
    /// This also rejects archives which omitted the roundtable section entirely.
    pub async fn validate_backup_objects(
        database: &std::path::Path,
        objects: &std::path::Path,
    ) -> Result<(), crate::app_error::AppCommandError> {
        use sea_orm::Database;
        let url = format!(
            "sqlite:{}?mode=ro",
            urlencoding::encode(database.to_string_lossy().as_ref())
        );
        let conn = Database::connect(url).await.map_err(|err| {
            crate::app_error::AppCommandError::database_error("Open backup database")
                .with_detail(err.to_string())
        })?;
        let result=async {
            let exists=super::store::query_i64(&conn,"SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='rt_source_manifests'",vec![]).await?;
            if exists==0 {return Ok(());}
            for row in super::store::rows(&conn,"SELECT body_json FROM rt_source_manifests",vec![]).await? {
                let raw:String=super::store::column(&row,0)?;
                let manifest:super::snapshot::SourceManifestV1=serde_json::from_str(&raw).map_err(|_|super::rt_error(ErrorCode::StorageUnavailable,"manifest_corrupt"))?;
                for entry in manifest.entries {
                    if entry.object.object_id!=entry.object.content_hash.to_hex() {return Err(super::rt_error(ErrorCode::StorageUnavailable,"object_id"));}
                    let bytes=std::fs::read(objects.join(&entry.object.object_id)).map_err(|_|super::rt_error(ErrorCode::StorageUnavailable,"object_missing"))?;
                    if bytes.len() as u64!=entry.object.total_bytes || roundtable_protocol::Hash256::sha256(&bytes)!=entry.object.content_hash {return Err(super::rt_error(ErrorCode::StorageUnavailable,"object_corrupt"));}
                }
            }
            Ok::<(),roundtable_protocol::RtError>(())
        }.await;
        let _ = conn.close().await;
        result.map_err(|err| {
            crate::app_error::AppCommandError::database_error(
                "Roundtable backup objects are missing or corrupt",
            )
            .with_detail(err.to_string())
        })
    }
}
