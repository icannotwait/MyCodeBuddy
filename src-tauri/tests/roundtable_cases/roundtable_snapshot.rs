use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use codeg_lib::db::{open_configured_sqlite, DbOpenOptions};
use codeg_lib::roundtable::{
    build_delivery, canonical_path_bytes, capture_snapshot, commit_captured_manifest,
    confirmation_echo, ensure_within_root, freeze_phase, freeze_preflight, fresh_binding_context,
    lexical_within, line_start_offsets, migrate_roundtable, offer_text_tool, open_roundtable_store,
    validate_relative_path, ConfirmationEcho, ObjectFault, ObjectStore, PhaseInput, PreflightLog,
    ReservationLedger, ResolvedRecipients, RoomDraft, SelectedFile, SnapshotEncoding,
    SnapshotLimits, SourceClass, SourceEntryV1, SourceManifestV1, SourceSelection,
    FRESH_CONTEXT_STATE, MAX_SNAPSHOT_READS,
};
use codeg_lib::roundtable::{NewBinding, NewRoom, NewSpeaker};
use roundtable_protocol::{
    canonical_hash, ActorContext, BindingId, ClientIdentity, ClientKind, ContextFreshness,
    DeliveryEncoder, ErrorCode, Hash256, IncarnationId, InternalReason, ManifestId, OperatorScope,
    PhaseId, PhaseKind, PrincipalId, QualifiedContextProfile, ResolvedRecipientV1, Revision,
    RoleSnapshot, RoomId, RtError, RtResult, SafeInt, SpeakerId, TokenBound, ToolQuotaV1,
};

fn ids<T: FromStr>(n: u8) -> T
where
    T::Err: std::fmt::Debug,
{
    format!("00000000-0000-4000-8000-{n:012x}").parse().unwrap()
}

fn reason(err: &RtError) -> &str {
    err.details.reason.as_deref().unwrap_or("")
}

fn limits(estimated: u64) -> SnapshotLimits {
    SnapshotLimits {
        estimated_bytes: estimated,
        max_file_bytes: 1_048_576,
        max_total_bytes: 4_194_304,
        max_files: 32,
    }
}

fn open_objects(root: &Path, quota: u64) -> (Arc<ReservationLedger>, ObjectStore) {
    let ledger = Arc::new(ReservationLedger::new(quota));
    let objects = ObjectStore::open(
        root.join("objects"),
        Arc::clone(&ledger),
        ids::<PrincipalId>(1),
    )
    .unwrap();
    (ledger, objects)
}

#[tokio::test]
async fn storage_fix_reopened_object_survives_capture_rollback() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("src");
    write_tree(&root);
    let (_, first) = open_objects(dir.path(), 1_000_000);
    let original = capture_snapshot(
        selection(root.clone(), ids::<RoomId>(6), 1, None, sample_files()),
        limits(10_000),
        &first,
    )
    .await
    .unwrap();
    drop(first);
    let (_, reopened) = open_objects(dir.path(), 1_000_000);
    let reused = capture_snapshot(
        selection(root, ids::<RoomId>(6), 2, None, sample_files()),
        limits(10_000),
        &reopened,
    )
    .await
    .unwrap();
    let (_pool_dir, conn) = super::roundtable_support::open_pool(1).await;
    migrate_roundtable(&conn).await.unwrap();
    let store = open_roundtable_store(conn).await.unwrap();
    reopened.set_fault(ObjectFault::DbCommit);
    assert!(commit_captured_manifest(&store, &reopened, &reused)
        .await
        .is_err());
    for entry in original.entries {
        reopened
            .get_verified(&entry.object)
            .await
            .expect("pre-existing immutable object must survive rollback");
    }
}

fn selection(
    root: PathBuf,
    room: RoomId,
    version: i64,
    base: Option<&str>,
    files: Vec<(&str, SourceClass)>,
) -> SourceSelection {
    SourceSelection {
        root,
        room_id: room,
        version,
        base_commit: base.map(str::to_string),
        files: files
            .into_iter()
            .map(|(relative_path, class)| SelectedFile {
                relative_path: relative_path.to_string(),
                class,
            })
            .collect(),
        mutate_while_open: None,
    }
}

fn entry<'a>(manifest: &'a SourceManifestV1, path: &str) -> &'a SourceEntryV1 {
    manifest
        .entries
        .iter()
        .find(|item| item.path == path)
        .unwrap_or_else(|| panic!("missing {path}"))
}

fn write_tree(root: &Path) {
    std::fs::create_dir_all(root).unwrap();
    std::fs::write(root.join("tracked.txt"), b"tracked\n").unwrap();
    std::fs::write(root.join("dirty.txt"), b"selected dirty contents\n").unwrap();
    std::fs::write(root.join("untracked.txt"), b"untracked\n").unwrap();
}

fn sample_files() -> Vec<(&'static str, SourceClass)> {
    vec![
        ("tracked.txt", SourceClass::Tracked),
        ("dirty.txt", SourceClass::Dirty),
        ("untracked.txt", SourceClass::Untracked),
    ]
}

#[cfg(unix)]
fn try_symlink(target: &Path, link: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

#[cfg(windows)]
fn try_symlink(target: &Path, link: &Path) -> std::io::Result<()> {
    std::os::windows::fs::symlink_file(target, link)
}

struct ByteTokens(u64);

impl TokenBound for ByteTokens {
    fn upper_bound(&self, utf8: &[u8]) -> RtResult<u64> {
        u64::try_from(utf8.len()).map_err(|_| RtError::from_reason(InternalReason::SafeInteger))
    }

    fn capacity_tokens(&self) -> Option<u64> {
        Some(self.0)
    }
}

#[tokio::test]
async fn dirty_and_untracked_change_manifest() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("src");
    write_tree(&root);
    let room = ids::<RoomId>(4);
    let (ledger, objects) = open_objects(dir.path(), 1_000_000);
    let first = capture_snapshot(
        selection(root.clone(), room, 1, Some("abc123"), sample_files()),
        limits(10_000),
        &objects,
    )
    .await
    .unwrap();
    let same_bytes = capture_snapshot(
        selection(root.clone(), room, 1, Some("def456"), sample_files()),
        limits(10_000),
        &objects,
    )
    .await
    .unwrap();
    assert_eq!(first.manifest_hash, same_bytes.manifest_hash);
    assert_eq!(first.base_commit.as_deref(), Some("abc123"));
    assert_eq!(same_bytes.base_commit.as_deref(), Some("def456"));
    assert_eq!(entry(&first, "dirty.txt").class, SourceClass::Dirty);
    assert_eq!(entry(&first, "untracked.txt").class, SourceClass::Untracked);
    assert_eq!(entry(&first, "tracked.txt").class, SourceClass::Tracked);

    std::fs::write(root.join("dirty.txt"), b"selected dirty contents!\n").unwrap();
    std::fs::write(root.join("untracked.txt"), b"untracked!\n").unwrap();
    let after = capture_snapshot(
        selection(root.clone(), room, 1, Some("abc123"), sample_files()),
        limits(10_000),
        &objects,
    )
    .await
    .unwrap();
    assert_ne!(first.manifest_hash, after.manifest_hash);
    assert_eq!(after.base_commit, first.base_commit);

    let frozen = objects
        .get_verified(&entry(&first, "dirty.txt").object)
        .await
        .unwrap();
    assert_eq!(frozen, b"selected dirty contents\n");
    std::fs::write(root.join("dirty.txt"), b"changed after freeze\n").unwrap();
    let still = objects
        .get_verified(&entry(&first, "dirty.txt").object)
        .await
        .unwrap();
    assert_eq!(still, b"selected dirty contents\n");
    assert_eq!(ledger.reserved_bytes(), 0);
    assert!(ledger.used_bytes() > 0);
}

#[tokio::test]
async fn capture_rejects_escape_and_mutation() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("src");
    write_tree(&root);
    let room = ids::<RoomId>(5);
    let (_ledger, objects) = open_objects(dir.path(), 1_000_000);

    for path in [
        r"C:\Windows\System32\cmd.exe",
        r"\\server\share\secret",
        r"D:relative",
        "/etc/passwd",
        r"foo\..\secret",
        "foo/../../etc/passwd",
        "../escape",
    ] {
        assert!(validate_relative_path(path).is_err(), "{path}");
    }

    let absolute = capture_snapshot(
        selection(
            root.clone(),
            room,
            1,
            None,
            vec![(r"C:\Windows\System32\cmd.exe", SourceClass::Tracked)],
        ),
        limits(100),
        &objects,
    )
    .await
    .unwrap_err();
    assert_eq!(reason(&absolute), "absolute_path");

    let parent = capture_snapshot(
        selection(
            root.clone(),
            room,
            1,
            None,
            vec![(r"foo\..\secret", SourceClass::Tracked)],
        ),
        limits(100),
        &objects,
    )
    .await
    .unwrap_err();
    assert_eq!(reason(&parent), "parent_escape");

    let outside = dir.path().join("outside.txt");
    std::fs::write(&outside, b"nope").unwrap();
    assert_eq!(
        reason(&ensure_within_root(&root, &outside).unwrap_err()),
        "cross_root"
    );
    assert!(ensure_within_root(&root, &root.join("tracked.txt")).is_ok());

    std::fs::create_dir(root.join("nested")).unwrap();
    let directory = capture_snapshot(
        selection(
            root.clone(),
            room,
            1,
            None,
            vec![("nested", SourceClass::Tracked)],
        ),
        limits(100),
        &objects,
    )
    .await
    .unwrap_err();
    assert_eq!(reason(&directory), "not_regular");

    let link = root.join("link.txt");
    match try_symlink(&root.join("tracked.txt"), &link) {
        Ok(())
            if std::fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink() =>
        {
            let err = capture_snapshot(
                selection(
                    root.clone(),
                    room,
                    1,
                    None,
                    vec![("link.txt", SourceClass::Tracked)],
                ),
                limits(100),
                &objects,
            )
            .await
            .unwrap_err();
            assert_eq!(reason(&err), "symlink");
        }
        Ok(()) => {
            eprintln!("skip symlink physical fixture: path is not a symlink");
        }
        Err(err) => {
            eprintln!("skip symlink physical fixture: {err}");
        }
    }

    let linked = root.join("hard.txt");
    match std::fs::hard_link(root.join("tracked.txt"), &linked) {
        Ok(()) => {
            let err = capture_snapshot(
                selection(
                    root.clone(),
                    room,
                    1,
                    None,
                    vec![("hard.txt", SourceClass::Tracked)],
                ),
                limits(100),
                &objects,
            )
            .await
            .unwrap_err();
            assert_eq!(reason(&err), "hard_link");
        }
        Err(err) => eprintln!("skip hard-link physical fixture: {err}"),
    }

    #[cfg(unix)]
    {
        let fifo = root.join("probe.fifo");
        let made = std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .map(|status| status.success())
            .unwrap_or(false);
        if made {
            let err = capture_snapshot(
                selection(
                    root.clone(),
                    room,
                    1,
                    None,
                    vec![("probe.fifo", SourceClass::Tracked)],
                ),
                limits(100),
                &objects,
            )
            .await
            .unwrap_err();
            assert_eq!(reason(&err), "not_regular");
        } else {
            eprintln!("skip fifo physical fixture: mkfifo unavailable");
        }
        let sock = root.join("probe.sock");
        if std::os::unix::net::UnixListener::bind(&sock).is_ok() {
            let err = capture_snapshot(
                selection(
                    root.clone(),
                    room,
                    1,
                    None,
                    vec![("probe.sock", SourceClass::Tracked)],
                ),
                limits(100),
                &objects,
            )
            .await
            .unwrap_err();
            assert_eq!(reason(&err), "not_regular");
        } else {
            eprintln!("skip socket physical fixture: bind failed");
        }
    }
    #[cfg(windows)]
    {
        eprintln!(
            "skip device/socket physical fixture: Windows capture root cannot create device nodes or AF_UNIX sockets"
        );
    }

    let hits = Arc::new(AtomicUsize::new(0));
    let hook_hits = Arc::clone(&hits);
    std::fs::write(root.join("moving.txt"), b"v1").unwrap();
    let mut moving = selection(
        root.clone(),
        room,
        1,
        None,
        vec![("moving.txt", SourceClass::Untracked)],
    );
    moving.mutate_while_open = Some(Arc::new(move |path| {
        hook_hits.fetch_add(1, Ordering::SeqCst);
        let mut bytes = std::fs::read(path).unwrap_or_default();
        bytes.push(b'x');
        std::fs::write(path, bytes).unwrap();
    }));
    let unstable = capture_snapshot(moving, limits(100), &objects).await;
    let err = unstable.unwrap_err();
    assert_eq!(err.code, ErrorCode::InvalidArgument);
    assert_eq!(reason(&err), InternalReason::SnapshotUnstable.as_str());
    assert_eq!(
        hits.load(Ordering::SeqCst),
        usize::try_from(MAX_SNAPSHOT_READS).unwrap()
    );
}

#[tokio::test]
async fn blob_failure_never_commits_reference() {
    for fault in [
        ObjectFault::Write,
        ObjectFault::FileFsync,
        ObjectFault::Rename,
        ObjectFault::DirFsync,
        ObjectFault::Hash,
    ] {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("src");
        write_tree(&root);
        let (ledger, objects) = open_objects(dir.path(), 1_000_000);
        objects.set_fault(fault);
        let err = capture_snapshot(
            selection(root, ids::<RoomId>(6), 1, Some("abc123"), sample_files()),
            limits(10_000),
            &objects,
        )
        .await
        .unwrap_err();
        assert_eq!(err.code, ErrorCode::StorageUnavailable, "{fault:?}");
        assert!(objects.committed_ids().is_empty(), "{fault:?}");
        assert!(objects
            .remaining_files()
            .iter()
            .all(|name| !name.starts_with(".partial-")));
        assert_eq!(ledger.reserved_bytes(), 0, "{fault:?}");
        if fault == ObjectFault::DirFsync {
            assert!(ledger.used_bytes() > 0);
        } else {
            assert_eq!(ledger.used_bytes(), 0, "{fault:?}");
        }
    }

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("src");
    write_tree(&root);
    let (ledger, objects) = open_objects(dir.path(), 1_000_000);
    let manifest = capture_snapshot(
        selection(root, ids::<RoomId>(6), 1, Some("abc123"), sample_files()),
        limits(10_000),
        &objects,
    )
    .await
    .unwrap();
    let (pool_dir, conn) = super::roundtable_support::open_pool(1).await;
    migrate_roundtable(&conn).await.unwrap();
    let store = open_roundtable_store(conn).await.unwrap();
    objects.set_fault(ObjectFault::DbCommit);
    let err = commit_captured_manifest(&store, &objects, &manifest)
        .await
        .unwrap_err();
    assert_eq!(reason(&err), "manifest_commit");
    assert!(objects.committed_ids().is_empty());
    assert!(objects
        .remaining_files()
        .iter()
        .all(|name| !name.starts_with(".partial-")));
    for entry in &manifest.entries {
        objects.get_verified(&entry.object).await.unwrap();
    }
    assert_eq!(ledger.reserved_bytes(), 0);
    assert!(ledger.used_bytes() > 0);

    let room = ids::<RoomId>(7);
    let principal = ids::<PrincipalId>(7);
    store
        .insert_room(&NewRoom {
            room_id: room.to_string(),
            principal_id: principal.to_string(),
            status: "draft".into(),
            config_ref: "cfg".into(),
            revision: 1,
            run_epoch: 1,
            boot_epoch: 1,
            last_seq: 0,
            remaining_active_ms: 0,
            blocked_reason: None,
            result_quality: None,
        })
        .await
        .unwrap();
    let speaker = ids::<SpeakerId>(8);
    store
        .insert_speaker(&NewSpeaker {
            room_id: room.to_string(),
            speaker_id: speaker.to_string(),
            ordinal: 0,
            role: "moderator".into(),
            provider_ref: "provider".into(),
            model_id: "model".into(),
            model_snapshot_json: "{}".into(),
        })
        .await
        .unwrap();
    let binding = ids::<BindingId>(9);
    store
        .insert_binding(&NewBinding {
            room_id: room.to_string(),
            binding_id: binding.to_string(),
            speaker_id: speaker.to_string(),
            generation: 1,
            incarnation: ids::<IncarnationId>(10).to_string(),
            external_session_id: "ext".into(),
            policy_ref: "policy".into(),
            certificate_ref: "cert".into(),
            context_state: FRESH_CONTEXT_STATE.into(),
            state: "ready".into(),
        })
        .await
        .unwrap();

    let kept_root = dir.path().join("kept");
    write_tree(&kept_root);
    objects.set_fault(ObjectFault::None);
    let kept = capture_snapshot(
        selection(kept_root, room, 2, Some("abc123"), sample_files()),
        limits(10_000),
        &objects,
    )
    .await
    .unwrap();
    commit_captured_manifest(&store, &objects, &kept)
        .await
        .unwrap();
    assert!(!objects.committed_ids().is_empty());
    let db = pool_dir.path().join("roundtable.db");
    let hash = read_column(
        &db,
        &format!(
            "SELECT manifest_hash FROM rt_source_manifests WHERE manifest_id = '{}'",
            kept.manifest_id
        ),
    )
    .await;
    assert_eq!(hash, kept.manifest_hash.to_hex());
    let body = read_column(
        &db,
        &format!(
            "SELECT body_json FROM rt_source_manifests WHERE manifest_id = '{}'",
            kept.manifest_id
        ),
    )
    .await;
    assert!(body.contains(&kept.manifest_hash.to_hex()));
    assert!(body.contains("abc123"));
    let state = read_column(
        &db,
        &format!(
            "SELECT context_state FROM rt_bindings WHERE binding_id = '{}'",
            binding
        ),
    )
    .await;
    assert_eq!(state, FRESH_CONTEXT_STATE);
}

async fn read_column(path: &Path, sql: &str) -> String {
    let url = format!(
        "sqlite:{}?mode=rwc",
        urlencoding::encode(path.to_string_lossy().as_ref())
    );
    let conn = open_configured_sqlite(&DbOpenOptions {
        url,
        max_connections: 1,
        min_connections: 1,
        connect_timeout: Duration::from_secs(10),
        idle_timeout: None,
    })
    .await
    .unwrap();
    super::roundtable_support::scalar_text(&conn, sql).await
}

#[tokio::test]
async fn confirmed_manifest_cannot_change_before_start() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("src");
    write_tree(&root);
    let room = ids::<RoomId>(11);
    let (_ledger, objects) = open_objects(dir.path(), 1_000_000);
    let manifest = capture_snapshot(
        selection(root.clone(), room, 1, Some("abc123"), sample_files()),
        limits(10_000),
        &objects,
    )
    .await
    .unwrap();
    let actor = ActorContext::from_trusted_entry(
        ids::<PrincipalId>(11),
        OperatorScope::SingleOperator,
        ClientIdentity {
            kind: ClientKind::Desktop,
            session_ref: "desktop".into(),
        },
    );
    let recipients = ResolvedRecipients {
        recipients: vec![ResolvedRecipientV1 {
            provider_ref: "provider".into(),
            provider_config_version: "1".into(),
            endpoint_origin: "https://example.test".into(),
            model: "model".into(),
            effort: "low".into(),
        }],
    };
    let draft = RoomDraft {
        room_id: Some(room),
        revision: Some(Revision(1)),
        config_hash: Hash256::sha256(b"config"),
        policy_hash: Hash256::sha256(b"policy"),
        qualification_keys: vec!["key".into()],
        limits_hash: Hash256::sha256(b"limits"),
        confirmable: true,
        exclusion_only: false,
        excluded_paths: vec![".env".into()],
        secret_names: vec!["API_KEY".into()],
        created_at: "2026-10-04T00:00:00Z".into(),
        expires_at: "2026-10-05T00:00:00Z".into(),
    };
    let mut exclusion = draft.clone();
    exclusion.exclusion_only = true;
    let echo: ConfirmationEcho = confirmation_echo(&exclusion, &recipients);
    assert_eq!(echo.secret_names, vec!["API_KEY".to_string()]);
    assert_eq!(echo.excluded_paths, vec![".env".to_string()]);
    assert!(!echo.confirmable);
    assert!(
        !freeze_preflight(&actor, &exclusion, &manifest, &recipients)
            .unwrap()
            .confirmable
    );

    let record = freeze_preflight(&actor, &draft, &manifest, &recipients).unwrap();
    assert!(record.confirmable);
    assert_eq!(record.recipients, recipients.recipients);
    assert_eq!(record.source_manifest_hash, manifest.manifest_hash);
    let log = PreflightLog::new();
    log.confirm(record.clone(), manifest.clone()).unwrap();

    std::fs::write(root.join("dirty.txt"), b"changed on disk\n").unwrap();
    let recaptured = capture_snapshot(
        selection(root, room, 1, Some("abc123"), sample_files()),
        limits(10_000),
        &objects,
    )
    .await
    .unwrap();
    assert_ne!(recaptured.manifest_hash, manifest.manifest_hash);
    let started = log
        .start_confirmed_manifest(&record.preflight_id, &recipients)
        .unwrap();
    assert_eq!(started.manifest_hash, manifest.manifest_hash);
    let frozen = objects
        .get_verified(&entry(&manifest, "dirty.txt").object)
        .await
        .unwrap();
    assert_eq!(frozen, b"selected dirty contents\n");

    let mut changed = recipients.clone();
    changed.recipients[0].model = "other".into();
    let err = log
        .start_confirmed_manifest(&record.preflight_id, &changed)
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::InvalidState);
    assert_eq!(
        reason(&err),
        InternalReason::ReconfirmationRequired.as_str()
    );
}

#[test]
fn reservation_preflights_cannot_both_exceed() {
    let ledger = Arc::new(ReservationLedger::new(100));
    let left = Arc::clone(&ledger);
    let right = Arc::clone(&ledger);
    let (first, second) = std::thread::scope(|scope| {
        let one = scope.spawn(move || left.reserve_capture(ids::<PrincipalId>(1), 60));
        let two = scope.spawn(move || right.reserve_capture(ids::<PrincipalId>(2), 60));
        (one.join().unwrap(), two.join().unwrap())
    });
    let winner = match (first, second) {
        (Ok(lease), Err(err)) | (Err(err), Ok(lease)) => {
            assert_eq!(err.code, ErrorCode::CapacityLimited);
            lease
        }
        other => panic!("one reservation must win, got {}", match_label(&other)),
    };
    assert_eq!(ledger.reserved_bytes(), 60);
    winner.release_after_partial_removed(true).unwrap();
    assert_eq!(ledger.reserved_bytes(), 0);
    assert_eq!(ledger.used_bytes(), 0);
}

fn match_label(
    result: &(
        RtResult<codeg_lib::roundtable::StorageLease>,
        RtResult<codeg_lib::roundtable::StorageLease>,
    ),
) -> &'static str {
    match result {
        (Ok(_), Ok(_)) => "both reserved",
        (Err(_), Err(_)) => "both rejected",
        _ => "mixed",
    }
}

#[tokio::test]
async fn fixture_line_offsets_and_binary() {
    assert_eq!(line_start_offsets(b"alpha\r\nbeta").unwrap(), vec![0, 7]);
    assert_eq!(
        line_start_offsets(b"alpha\r\nbeta\r\n").unwrap(),
        vec![0, 7]
    );
    assert_eq!(line_start_offsets(b"abc").unwrap(), vec![0]);
    assert!(line_start_offsets(&[0xff, 0xfe, 0x00, 0x0a]).is_none());

    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/roundtable/snapshot");
    let dir = tempfile::tempdir().unwrap();
    let (_ledger, objects) = open_objects(dir.path(), 1_000_000);
    let manifest = capture_snapshot(
        selection(
            root,
            ids::<RoomId>(12),
            1,
            Some("fixture"),
            vec![
                ("tracked.txt", SourceClass::Tracked),
                ("untracked.txt", SourceClass::Untracked),
                ("dirty.txt", SourceClass::Dirty),
                ("crlf.txt", SourceClass::Tracked),
                ("no-final-newline.txt", SourceClass::Tracked),
                ("binary.bin", SourceClass::Untracked),
            ],
        ),
        limits(10_000),
        &objects,
    )
    .await
    .unwrap();
    assert_eq!(
        manifest
            .entries
            .iter()
            .map(|item| item.path.as_str())
            .collect::<Vec<_>>(),
        vec![
            "binary.bin",
            "crlf.txt",
            "dirty.txt",
            "no-final-newline.txt",
            "tracked.txt",
            "untracked.txt",
        ]
    );
    assert_eq!(manifest.read_limit, MAX_SNAPSHOT_READS);
    let crlf = entry(&manifest, "crlf.txt");
    assert_eq!(crlf.line_offsets, vec![0, 7]);
    assert_eq!(crlf.encoding, SnapshotEncoding::Utf8);
    let crlf_bytes = objects.get_verified(&crlf.object).await.unwrap();
    assert_eq!(crlf_bytes, b"alpha\r\nbeta");
    assert_eq!(offer_text_tool(crlf, &crlf_bytes).unwrap(), "alpha\r\nbeta");
    let bare = entry(&manifest, "no-final-newline.txt");
    assert_eq!(bare.line_offsets, vec![0]);
    assert_eq!(objects.get_verified(&bare.object).await.unwrap(), b"abc");
    let binary = entry(&manifest, "binary.bin");
    assert_eq!(binary.encoding, SnapshotEncoding::Binary);
    assert!(!binary.text_admissible);
    assert!(binary.line_offsets.is_empty());
    let binary_bytes = objects.get_verified(&binary.object).await.unwrap();
    assert_eq!(binary_bytes, [0xff, 0xfe, 0x00, 0x0a]);
    assert_eq!(
        reason(&offer_text_tool(binary, &binary_bytes).unwrap_err()),
        "binary_source"
    );
    assert_ne!(entry(&manifest, "dirty.txt").mode, 0);
}

#[test]
fn windows_path_and_case_fixtures() {
    assert_ne!(
        canonical_path_bytes("CaseFile").unwrap(),
        canonical_path_bytes("casefile").unwrap()
    );
    let mut paths = vec![
        validate_relative_path("b").unwrap(),
        validate_relative_path("a").unwrap(),
    ];
    paths.sort_by(|left, right| left.as_bytes().cmp(right.as_bytes()));
    assert_eq!(paths, vec!["a".to_string(), "b".to_string()]);
    assert!(lexical_within(r"C:\work\room", r"C:\work\room\file"));
    assert!(!lexical_within(r"C:\work\room", r"C:\work\other\file"));
    assert!(!lexical_within(r"C:\work\room", r"C:\work\room-extra\file"));
    assert!(!lexical_within(r"C:\work\room", r"C:\work\room\..\other"));
    assert!(!lexical_within(r"C:\Work\room", r"C:\work\room\file"));

    let dir = tempfile::tempdir().unwrap();
    let lower = dir.path().join("casefile");
    let upper = dir.path().join("CaseFile");
    std::fs::write(&lower, b"lower").unwrap();
    let before = std::fs::read(&lower).unwrap();
    std::fs::write(&upper, b"upper").unwrap();
    let lower_now = std::fs::read(&lower).unwrap();
    let upper_now = std::fs::read(&upper).unwrap_or_default();
    let case_sensitive = lower_now == before && upper_now != lower_now;
    if !case_sensitive {
        eprintln!("skip case-pair physical fixture: filesystem is case-insensitive");
        return;
    }
    assert_ne!(lower_now, upper_now);
}

#[tokio::test]
async fn delivery_prompt_bytes_and_fresh_binding() {
    let phase = freeze_phase(PhaseInput {
        phase_id: ids::<PhaseId>(2),
        phase_index: 0,
        revision: Revision(1),
        kind: PhaseKind::Proposal,
        critique_round: None,
        config_version: Revision(1),
        question_version: Revision(1),
        interjection_version: Revision(1),
        source_manifest_id: ids::<ManifestId>(3),
        source_manifest_hash: Hash256::sha256(b"manifest"),
        published_messages: Vec::new(),
        members: Vec::new(),
        mandatory_targets: Vec::new(),
        policy_hash: Hash256::sha256(b"policy"),
        output_byte_limit: SafeInt(32),
        tool_quota: ToolQuotaV1 {
            per_call_bytes: SafeInt(32),
            per_attempt_bytes: SafeInt(32),
        },
    })
    .unwrap();
    let role = RoleSnapshot {
        role: "member".into(),
        model: "model".into(),
        effort: "low".into(),
        provider_ref: "provider".into(),
        prompt_version: "p1".into(),
        template_version: "t1".into(),
        schema_id: "schema".into(),
        schema_text: "{}".into(),
        tool_version: "tool".into(),
        tool_text: "[]".into(),
    };
    let binding = ids::<BindingId>(9);
    let profile = QualifiedContextProfile::proposed(
        "tokenizer",
        Hash256::sha256(b"tokenizer"),
        50_000_000,
        0,
        "proof",
    );
    let delivery =
        build_delivery(&phase, &role, &binding, &ByteTokens(50_000_000), &profile).unwrap();
    let prompt = DeliveryEncoder::prompt_utf8(&phase, &role, &binding).unwrap();
    assert_eq!(delivery.prompt_bytes.0, prompt.len() as u64);
    assert!(delivery.prior_cursor.is_none());
    assert_eq!(delivery.binding_id, binding);
    assert_eq!(delivery.role_hash, canonical_hash(&role).unwrap());
    assert_eq!(delivery.public_view_hash, canonical_hash(&phase).unwrap());
    assert_eq!(delivery.template_version, role.template_version);
    let context = fresh_binding_context(delivery.prompt_bytes);
    assert_eq!(context.freshness, ContextFreshness::Fresh);
    assert_eq!(context.delivered_prompt_bytes, delivery.prompt_bytes);
    assert_eq!(FRESH_CONTEXT_STATE, "fresh");
}

#[tokio::test]
async fn storage_fix_concurrent_stores_retain_shared_blob_on_rollback() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("src");
    write_tree(&root);
    let (ledger_a, a) = open_objects(dir.path(), 1_000_000);
    let first = capture_snapshot(
        selection(root.clone(), ids::<RoomId>(6), 1, None, sample_files()),
        limits(10_000),
        &a,
    )
    .await
    .unwrap();
    let (_, b) = open_objects(dir.path(), 1_000_000);
    let second = capture_snapshot(
        selection(root, ids::<RoomId>(7), 1, None, sample_files()),
        limits(10_000),
        &b,
    )
    .await
    .unwrap();
    let (_pool, conn) = super::roundtable_support::open_pool(1).await;
    migrate_roundtable(&conn).await.unwrap();
    let store = open_roundtable_store(conn).await.unwrap();
    store
        .insert_room(&NewRoom {
            room_id: ids::<RoomId>(7).to_string(),
            principal_id: ids::<PrincipalId>(1).to_string(),
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
    commit_captured_manifest(&store, &b, &second).await.unwrap();
    a.set_fault(ObjectFault::DbCommit);
    assert!(commit_captured_manifest(&store, &a, &first).await.is_err());
    for entry in &second.entries {
        b.get_verified(&entry.object).await.unwrap();
    }
    assert!(ledger_a.used_bytes() >= first.accounted_bytes());
}
