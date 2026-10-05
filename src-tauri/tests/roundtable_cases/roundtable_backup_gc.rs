//! Actual SQLite references and content-addressed files govern backups.
use crate::roundtable_support::open_pool;
use codeg_lib::roundtable::{
    backup_roundtable, capture_snapshot, commit_captured_manifest, gc_unreferenced,
    migrate_roundtable, open_roundtable_store, restore_roundtable, NewRoom, ObjectStore,
    ReservationLedger, SelectedFile, SnapshotLimits, SourceClass, SourceSelection,
};
use roundtable_protocol::{PrincipalId, RoomId};
use std::sync::Arc;

#[tokio::test]
async fn backup_gc_and_disk_failures() {
    let (dir, conn) = open_pool(1).await;
    migrate_roundtable(&conn).await.unwrap();
    let store = open_roundtable_store(conn).await.unwrap();
    let room: RoomId = "00000000-0000-4000-8000-000000000001".parse().unwrap();
    let principal: PrincipalId = "00000000-0000-4000-8000-000000000002".parse().unwrap();
    store
        .insert_room(&NewRoom {
            room_id: room.to_string(),
            principal_id: principal.to_string(),
            status: "draft".into(),
            config_ref: "{}".into(),
            revision: 1,
            run_epoch: 1,
            boot_epoch: 1,
            last_seq: 0,
            remaining_active_ms: 1000,
            blocked_reason: None,
            result_quality: None,
        })
        .await
        .unwrap();
    let source = dir.path().join("source");
    std::fs::create_dir(&source).unwrap();
    std::fs::write(source.join("file.txt"), b"immutable source").unwrap();
    let path = dir.path().join("objects");
    let objects =
        ObjectStore::open(&path, Arc::new(ReservationLedger::new(10000)), principal).unwrap();
    let captured = capture_snapshot(
        SourceSelection {
            root: source,
            room_id: room,
            version: 1,
            base_commit: None,
            files: vec![SelectedFile {
                relative_path: "file.txt".into(),
                class: SourceClass::Untracked,
            }],
            mutate_while_open: None,
        },
        SnapshotLimits {
            estimated_bytes: 1000,
            max_file_bytes: 1000,
            max_total_bytes: 1000,
            max_files: 1,
        },
        &objects,
    )
    .await
    .unwrap();
    commit_captured_manifest(&store, &objects, &captured)
        .await
        .unwrap();
    let manifest = backup_roundtable(&store).await.unwrap();
    assert_eq!(manifest.section, "roundtable");
    assert_eq!(manifest.referenced.len(), 1);
    restore_roundtable(&store, &manifest, &path).await.unwrap();
    let blob = path.join(&captured.entries[0].object.object_id);
    std::fs::write(&blob, b"tampered").unwrap();
    assert!(restore_roundtable(&store, &manifest, &path).await.is_err());
    std::fs::remove_file(&blob).unwrap();
    assert!(restore_roundtable(&store, &manifest, &path).await.is_err());
    let error = gc_unreferenced(&store).await.unwrap_err();
    assert_eq!(
        error.details.reason.as_deref(),
        Some("gc_reference_proof_required")
    );
}
