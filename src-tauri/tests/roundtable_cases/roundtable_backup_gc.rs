//! Referenced objects stay pinned. A bad restore does not become runnable.

use std::collections::BTreeSet;

use codeg_lib::roundtable::{
    backup_roundtable, gc_unreferenced, migrate_roundtable, open_roundtable_store,
    restore_roundtable, TrackedObject,
};
use roundtable_protocol::ErrorCode;

use crate::roundtable_support::open_pool;

fn object(id: &str, hash: &str, age: u64, referenced: bool, kind: &str) -> TrackedObject {
    TrackedObject {
        id: id.to_string(),
        hash: hash.to_string(),
        age_hours: age,
        referenced,
        kind: kind.to_string(),
        present: true,
    }
}

#[tokio::test]
async fn backup_gc_and_disk_failures() {
    let (_dir, conn) = open_pool(1).await;
    migrate_roundtable(&conn).await.expect("migrate");
    let store = open_roundtable_store(conn).await.expect("store");
    let catalog_ref = store.reference_catalog();
    {
        let mut catalog = catalog_ref.lock().expect("catalog");
        catalog.watermark = "room-1".to_string();
        catalog.objects = vec![
            object("manifest", "h1", 48, true, "manifest"),
            object("diagnostic", "h2", 48, true, "diagnostic"),
            object("source", "h3", 48, true, "source"),
            object("projection", "h4", 48, true, "projection"),
            object("backup", "h5", 48, true, "backup"),
            object("pending", "h6", 48, true, "pending"),
            object("fresh-temp", "h7", 1, false, "temp"),
            object("old-temp", "h8", 48, false, "temp"),
            object("capture-temp", "h9", 48, false, "temp"),
        ];
        catalog.capture_ids.insert("capture-temp".to_string());
    }
    let manifest = backup_roundtable(&store).await.expect("backup");
    assert_eq!(manifest.section, "roundtable");
    assert!(manifest.referenced.contains_key("manifest"));
    assert!(!manifest.referenced.contains_key("old-temp"));

    let mut missing = manifest.clone();
    missing.referenced.insert("gone".to_string(), "missing".to_string());
    let error = restore_roundtable(&store, &missing).await.expect_err("missing");
    assert_eq!(error.code, ErrorCode::StorageUnavailable);
    {
        let catalog = catalog_ref.lock().expect("catalog");
        assert!(catalog.read_only);
        assert!(!catalog.runnable);
    }

    {
        let mut catalog = catalog_ref.lock().expect("catalog");
        catalog.objects[0].hash = "tampered".to_string();
        catalog.read_only = false;
        catalog.runnable = true;
    }
    let error = restore_roundtable(&store, &manifest).await.expect_err("hash");
    assert_eq!(error.code, ErrorCode::StorageUnavailable);
    {
        let catalog = catalog_ref.lock().expect("catalog");
        assert!(catalog.read_only);
        assert!(!catalog.runnable);
        assert!(catalog.objects.iter().all(|object| object.id != "decoy"));
    }

    let report = gc_unreferenced(&store).await.expect("gc");
    assert_eq!(report.deleted, BTreeSet::from(["old-temp".to_string()]));
    let left: Vec<String> = catalog_ref
        .lock()
        .expect("catalog")
        .objects
        .iter()
        .map(|object| object.id.clone())
        .collect();
    for pinned in ["manifest", "diagnostic", "source", "projection", "backup", "pending", "fresh-temp", "capture-temp"] {
        assert!(left.iter().any(|id| id == pinned), "{pinned}");
    }
}
