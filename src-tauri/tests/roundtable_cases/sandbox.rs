//! P06a isolation gate. Plan data is checked on every host. Live OS escape
//! attempts stay in `sandbox_blocks_host_escape_live`, which is ignored until
//! an authorized Linux candidate exists. An ignore is not a sandbox pass.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use codeg_lib::roundtable::{
    build_sandbox_plan, evaluate_certificate, scopes_convert, AdmissionFacts, CertifiedBinary,
    DbIdentity, ExecutionGate, ExecutionPolicy, ExecutionScope, IsolationProvider,
    JournalLaunchIntentStore, LaunchIntent, LaunchIntentStore, LinuxOciIsolator, OsIdentity,
    QualificationKey, QualificationReport, SandboxInput,
};
use roundtable_protocol::{Epoch, ErrorCode, Hash256, IncarnationId, MonoMs, QualificationStatus};
use tempfile::tempdir;

fn digest(byte: u8) -> Hash256 {
    Hash256::from_bytes([byte; 32])
}

fn image_digest(byte: u8) -> String {
    format!("sha256:{}", digest(byte).to_hex())
}

fn crun_path() -> PathBuf {
    if cfg!(windows) {
        PathBuf::from(r"C:\crun\pinned\crun.exe")
    } else {
        PathBuf::from("/usr/local/libexec/codeg/crun-pinned")
    }
}

fn cli_path() -> PathBuf {
    if cfg!(windows) {
        PathBuf::from(r"C:\crun\pinned\codex.exe")
    } else {
        PathBuf::from("/usr/local/libexec/codeg/codex-pinned")
    }
}

fn base_key() -> QualificationKey {
    QualificationKey {
        os: OsIdentity {
            name: "linux".to_string(),
            version: "debian-12".to_string(),
        },
        binaries: vec![
            CertifiedBinary {
                role: "crun".to_string(),
                absolute_path: crun_path().display().to_string(),
                version: "crun-1.15".to_string(),
                sha256: digest(1),
            },
            CertifiedBinary {
                role: "cli".to_string(),
                absolute_path: cli_path().display().to_string(),
                version: "codex-acp-2.1.1".to_string(),
                sha256: digest(2),
            },
        ],
        image_digest: image_digest(3),
        policy_hash: digest(4),
        tool_contract_hash: digest(5),
        core_hash: digest(6),
        adapter_version: "codex-acp@2.1.1".to_string(),
        isolator_version: "linux-oci-1".to_string(),
        plan_hash: digest(7),
    }
}

fn passed_report(key: &QualificationKey) -> QualificationReport {
    QualificationReport {
        key: key.clone(),
        status: QualificationStatus::Passed,
        evidence_ref: "qual-report-1".to_string(),
    }
}

fn facts_for(key: &QualificationKey) -> AdmissionFacts {
    AdmissionFacts {
        certificate: QualificationStatus::Passed,
        presented_key: key.clone(),
        qualification_attempts_used: 0,
        qualification_spend_used: 0,
        fixture_hash: digest(11),
        recipient: "fixture-model".to_string(),
    }
}

fn qualification_scope() -> ExecutionScope {
    ExecutionScope::Qualification {
        approval_id: "approval-1".to_string(),
        expires_at: MonoMs(10_000),
        attempt_limit: 2,
        spend_limit: 50,
        recipient: "fixture-model".to_string(),
        fixture_hash: digest(11),
    }
}

fn product_scope() -> ExecutionScope {
    ExecutionScope::Product {
        rollout_generation: 0,
    }
}

#[test]
fn certificate_invalidates_every_component() {
    let key = base_key();
    let report = passed_report(&key);
    assert_eq!(
        evaluate_certificate(&key, Some(&report)),
        QualificationStatus::Passed
    );
    assert_eq!(
        evaluate_certificate(&key, None),
        QualificationStatus::NotTested
    );

    let cases: Vec<(&str, QualificationKey)> = vec![
        ("os-name", {
            let mut next = key.clone();
            next.os.name = "windows".to_string();
            next
        }),
        ("os-version", {
            let mut next = key.clone();
            next.os.version = "debian-13".to_string();
            next
        }),
        ("binary-hash", {
            let mut next = key.clone();
            next.binaries[0].sha256 = digest(9);
            next
        }),
        ("binary-path", {
            let mut next = key.clone();
            next.binaries[1].absolute_path = "/usr/local/bin/other-cli".to_string();
            next
        }),
        ("binary-version", {
            let mut next = key.clone();
            next.binaries[1].version = "codex-acp-9.9.9".to_string();
            next
        }),
        ("image", {
            let mut next = key.clone();
            next.image_digest = image_digest(8);
            next
        }),
        ("policy", {
            let mut next = key.clone();
            next.policy_hash = digest(12);
            next
        }),
        ("tool", {
            let mut next = key.clone();
            next.tool_contract_hash = digest(13);
            next
        }),
        ("core", {
            let mut next = key.clone();
            next.core_hash = digest(14);
            next
        }),
        ("adapter", {
            let mut next = key.clone();
            next.adapter_version = "codex-acp@9".to_string();
            next
        }),
        ("isolator", {
            let mut next = key.clone();
            next.isolator_version = "linux-oci-2".to_string();
            next
        }),
        ("plan", {
            let mut next = key.clone();
            next.plan_hash = digest(15);
            next
        }),
    ];

    for (label, changed) in cases {
        assert_eq!(
            evaluate_certificate(&changed, Some(&report)),
            QualificationStatus::Expired,
            "{label} must invalidate the old pass"
        );
        assert_ne!(changed, key, "{label}");
    }

    let mut incomplete = key.clone();
    incomplete.core_hash = Hash256::from_bytes([0; 32]);
    let incomplete_report = passed_report(&incomplete);
    assert_eq!(
        evaluate_certificate(&incomplete, Some(&incomplete_report)),
        QualificationStatus::Failed,
        "a pass without a real core hash is not a certificate"
    );
}

#[test]
fn default_persistent_gate_denies_product() {
    let dir = tempdir().expect("temp dir");
    let data = dir.path();
    let key = base_key();
    let facts = facts_for(&key);
    let now = MonoMs(1_000);
    let product = product_scope();

    let gate = ExecutionGate::open(data);
    assert!(!gate.enabled());
    assert!(!gate.is_read_only());
    let denied = gate.check(&product, &facts, now).expect_err("product");
    assert_eq!(denied.code, ErrorCode::CapabilityUnqualified);
    assert_eq!(denied.details.reason.as_deref(), Some("rollout_disabled"));
    assert!(!data
        .join("roundtable")
        .join("execution-policy.json")
        .exists());

    drop(gate);
    let restarted = ExecutionGate::open(data);
    assert!(!restarted.enabled());
    let denied_again = restarted.check(&product, &facts, now).expect_err("restart");
    assert_eq!(denied_again.code, ErrorCode::CapabilityUnqualified);

    restarted
        .store_policy(&ExecutionPolicy::default())
        .expect("store default false");
    let policy_path = data.join("roundtable").join("execution-policy.json");
    let stored = fs::read_to_string(&policy_path).expect("policy file");
    assert!(
        stored.contains("\"enabled\":false"),
        "persisted default must stay false: {stored}"
    );
    assert!(
        !data
            .join("roundtable")
            .read_dir()
            .expect("dir")
            .any(|entry| entry
                .ok()
                .and_then(|entry| entry.file_name().into_string().ok())
                .is_some_and(|name| name.ends_with(".tmp"))),
        "atomic replace must not leave a temp file"
    );
    drop(restarted);
    let reloaded = ExecutionGate::open(data);
    assert!(!reloaded.enabled());
    assert!(reloaded.check(&product, &facts, now).is_err());

    let bad = dir.path().join("corrupt-data");
    fs::create_dir_all(bad.join("roundtable")).expect("corrupt dir");
    let bad_path = bad.join("roundtable").join("execution-policy.json");
    let garbage = b"{not-json";
    fs::write(&bad_path, garbage).expect("write garbage");
    let read_only = ExecutionGate::open(&bad);
    assert!(read_only.is_read_only());
    assert!(!read_only.enabled());
    for scope in [product.clone(), ExecutionScope::Fake, qualification_scope()] {
        let err = read_only.check(&scope, &facts, now).expect_err("read-only");
        assert_eq!(err.code, ErrorCode::CapabilityUnqualified);
        assert_eq!(err.details.reason.as_deref(), Some("policy_unreadable"));
    }
    assert!(read_only.store_policy(&ExecutionPolicy::default()).is_err());
    assert_eq!(fs::read(&bad_path).expect("unchanged"), garbage);
}

#[test]
fn scope_cannot_convert_qualification_to_product() {
    let dir = tempdir().expect("temp dir");
    let gate = ExecutionGate::open(dir.path());
    let key = base_key();
    let facts = facts_for(&key);
    let now = MonoMs(1_000);
    let qualification = qualification_scope();
    let product = product_scope();
    let fake = ExecutionScope::Fake;

    assert!(!scopes_convert(&qualification, &product));
    assert!(!scopes_convert(&fake, &product));
    assert!(!scopes_convert(&fake, &qualification));
    assert!(!scopes_convert(&product, &qualification));
    assert!(!scopes_convert(&qualification, &qualification));

    let permit = gate
        .check(&qualification, &facts, now)
        .expect("qualification scope is not product enablement");
    assert!(permit.authorizes(&qualification));
    assert!(!permit.authorizes(&product));
    assert!(!permit.authorizes(&fake));
    assert!(
        !gate.enabled(),
        "qualification must not flip the product gate"
    );

    let product_err = gate.check(&product, &facts, now).expect_err("product");
    assert_eq!(product_err.code, ErrorCode::CapabilityUnqualified);
    assert_eq!(
        product_err.details.reason.as_deref(),
        Some("rollout_disabled")
    );

    let fake_permit = gate.check(&fake, &facts, now).expect("fake");
    assert!(fake_permit.authorizes(&fake));
    assert!(!fake_permit.authorizes(&product));
    assert!(!fake_permit.authorizes(&qualification));
}

fn must_err<T, E>(result: Result<T, E>, what: &str) -> E {
    match result {
        Ok(_) => panic!("{what} unexpectedly succeeded"),
        Err(err) => err,
    }
}

fn abs_dir(root: &Path, name: &str) -> PathBuf {
    let path = root.join(name);
    fs::create_dir_all(&path).expect("mkdir");
    fs::canonicalize(&path).unwrap_or(path)
}

#[tokio::test(flavor = "current_thread")]
async fn sandbox_blocks_host_escape() {
    let dir = tempdir().expect("temp dir");
    let scratch = abs_dir(dir.path(), "scratch-a");
    let other = abs_dir(dir.path(), "scratch-b");
    let project = abs_dir(dir.path(), "project");
    let home = abs_dir(dir.path(), "home");
    let decoy = abs_dir(dir.path(), "decoy-abs");
    let key = base_key();
    let db = DbIdentity::new("sqlite:canonical-db-1").expect("db");
    let boot = Epoch(7);
    let incarnation: IncarnationId = "00000000-0000-4000-8000-00000000000a"
        .parse()
        .expect("incarnation");

    let mut inherited = BTreeMap::new();
    inherited.insert("HOME".to_string(), home.display().to_string());
    inherited.insert("PATH".to_string(), "/usr/bin:/bin".to_string());
    inherited.insert("MCP_CONFIG".to_string(), "/global/mcp.json".to_string());
    let mut allow = BTreeMap::new();
    allow.insert("LANG".to_string(), "C".to_string());

    let input = SandboxInput {
        certificate: key.clone(),
        db: db.clone(),
        boot_epoch: boot,
        incarnation,
        scratch: scratch.clone(),
        project: project.clone(),
        home: home.clone(),
        other_scratches: vec![other.clone()],
        decoy_paths: vec![decoy.clone()],
        inherited_env: inherited,
        env_allowlist: allow,
        global_mcp: false,
    };
    let plan = build_sandbox_plan(&input).expect("plan is data");
    assert!(plan.runtime_path.is_absolute());
    assert_eq!(plan.runtime_path, crun_path());
    assert_eq!(plan.runtime_sha256, digest(1));
    assert_eq!(
        plan.argv.first().map(String::as_str),
        Some(crun_path().to_str().unwrap())
    );
    assert!(plan.namespaces.user);
    assert!(plan.namespaces.mount);
    assert!(plan.namespaces.pid);
    assert!(plan.namespaces.network);
    assert!(plan.image_readonly);
    assert_eq!(plan.image_digest, key.image_digest);
    assert!(plan.drop_all_caps);
    assert!(plan.no_new_privileges);
    assert!(!plan.seccomp_default.is_empty());
    assert!(plan.cgroup.memory_max_bytes > 0);
    assert!(plan.cgroup.pids_max > 0);
    assert!(plan.cgroup.cpu_quota_us > 0);
    assert!(!plan.host_network);
    assert!(!plan.docker_socket_mounted);
    assert!(!plan.home_mounted);
    assert!(!plan.project_mounted);
    assert!(!plan.host_proc_mounted);
    assert!(!plan.inherited_fds);
    assert!(!plan.devices_allowed);
    assert!(!plan.host_sockets_mounted);
    assert!(!plan.symlinks_followed);
    assert!(!plan.global_mcp);
    assert!(plan.env_cleared_before_allowlist);
    assert_eq!(plan.env.get("LANG").map(String::as_str), Some("C"));
    assert!(!plan.env.contains_key("HOME"));
    assert!(!plan.env.contains_key("PATH"));
    assert!(!plan.env.contains_key("MCP_CONFIG"));
    assert!(plan.network_destinations.is_empty());
    assert!(plan.owner_label.contains(db.canonical()));
    assert!(plan.owner_label.contains(&boot.0.to_string()));
    assert!(plan.owner_label.contains(&incarnation.to_string()));
    assert!(
        !plan.owner_label.chars().all(|ch| ch.is_ascii_digit()),
        "label must not be a bare pid"
    );

    let namespaces = plan.oci["linux"]["namespaces"]
        .as_array()
        .expect("namespaces");
    let types: Vec<&str> = namespaces
        .iter()
        .filter_map(|entry| entry["type"].as_str())
        .collect();
    for required in ["user", "mount", "pid", "network"] {
        assert!(types.contains(&required), "{required} namespace");
    }
    assert_eq!(plan.oci["root"]["readonly"], serde_json::json!(true));
    assert_eq!(
        plan.oci["process"]["noNewPrivileges"],
        serde_json::json!(true)
    );
    assert!(plan.oci["process"]["capabilities"]["bounding"]
        .as_array()
        .expect("bounding")
        .is_empty());
    assert!(plan.oci["linux"]["seccomp"]["defaultAction"]
        .as_str()
        .expect("seccomp")
        .starts_with("SCMP_ACT_"));
    assert!(
        plan.oci["linux"]["resources"]["memory"]["limit"]
            .as_u64()
            .expect("memory")
            > 0
    );
    assert!(
        plan.oci["linux"]["resources"]["pids"]["limit"]
            .as_u64()
            .expect("pids")
            > 0
    );
    let env = plan.oci["process"]["env"].as_array().expect("env");
    assert!(env.iter().any(|item| item.as_str() == Some("LANG=C")));
    assert!(env.iter().all(|item| {
        let text = item.as_str().unwrap_or("");
        !text.starts_with("HOME=") && !text.starts_with("PATH=") && !text.starts_with("MCP_")
    }));
    for forbidden in [&home, &project, &other, &decoy] {
        assert!(
            plan.mounts.iter().all(|mount| mount.source != *forbidden),
            "mounted {}",
            forbidden.display()
        );
    }
    assert!(plan.mounts.iter().all(|mount| {
        let source = mount.source.to_string_lossy();
        !source.contains("docker.sock") && mount.source != Path::new("/proc")
    }));
    assert!(!plan
        .oci
        .to_string()
        .to_ascii_lowercase()
        .contains("docker.sock"));

    let mut relative = input.clone();
    relative.certificate.binaries[0].absolute_path = "crun".to_string();
    assert!(
        build_sandbox_plan(&relative).is_err(),
        "PATH name is not a pin"
    );

    let journal_dir = dir.path().join("roundtable").join("launch-intents");
    let journal = JournalLaunchIntentStore::open(&journal_dir).expect("journal");
    let intent = LaunchIntent::from_plan(&plan);
    journal.record(&intent).expect("record before spawn");
    let listed = journal.list_unreaped().expect("list");
    assert_eq!(listed.len(), 1);
    assert!(
        listed[0].spawned.is_none(),
        "intent is durable before spawn"
    );

    let isolator = LinuxOciIsolator::new(journal);
    let prepared = isolator.prepare(&plan).await.expect("prepare data");
    if !cfg!(target_os = "linux") {
        let err = isolator
            .spawn(&prepared, &intent)
            .await
            .expect_err("windows spawn");
        assert_eq!(err.code, ErrorCode::PolicyUnenforceable);
        assert_eq!(err.details.reason.as_deref(), Some("policy_unenforceable"));
    }
    let still = isolator.list_unreaped().expect("unacked");
    assert_eq!(still.len(), 1);
    assert!(still[0].spawned.is_none());

    let recorded = isolator.recorded_instances(&db).expect("lost ack");
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0].owner_label, plan.owner_label);
    assert_eq!(recorded[0].incarnation, incarnation);
    assert_eq!(recorded[0].boot_epoch, boot);
    assert_eq!(recorded[0].image_digest, plan.image_digest);
    assert!(recorded[0].runtime_id.parse::<u32>().is_err());
    assert!(recorded[0].runtime_id.contains(&incarnation.to_string()));
    assert!(recorded[0].cgroup_handle.contains(db.canonical()));

    let discovered = isolator.discover_owned(&db).await.expect_err("unproven");
    assert_eq!(discovered.code, ErrorCode::PolicyUnenforceable);
    assert_eq!(
        discovered.details.reason.as_deref(),
        Some("enumeration_unproven")
    );
    let reaped = isolator.reap(&recorded[0]).await.expect_err("no release");
    assert_eq!(reaped.code, ErrorCode::PolicyUnenforceable);
    assert_eq!(
        reaped.details.reason.as_deref(),
        Some("enumeration_unproven")
    );
    assert!(isolator.mailbox_drained().is_none());
    assert!(isolator.tools_drained().is_none());
    assert_eq!(isolator.list_unreaped().expect("held").len(), 1);

    let blocked = must_err(JournalLaunchIntentStore::open(&journal_dir), "exclusive");
    assert_eq!(blocked.code, ErrorCode::StorageUnavailable);
    assert_eq!(blocked.details.reason.as_deref(), Some("journal_locked"));
    drop(isolator);

    let replayed = JournalLaunchIntentStore::open(&journal_dir).expect("replay");
    let replayed_list = replayed.list_unreaped().expect("replay list");
    assert_eq!(replayed_list.len(), 1);
    assert_eq!(replayed_list[0].owner_label, plan.owner_label);
    assert!(replayed_list[0].spawned.is_none());
    drop(replayed);

    let truncated = dir.path().join("truncated-journal");
    fs::create_dir_all(&truncated).expect("truncated dir");
    fs::write(
        truncated.join("journal.jsonl"),
        b"{\"v\":1,\"op\":\"record\"",
    )
    .expect("partial");
    let truncated_err = must_err(JournalLaunchIntentStore::open(&truncated), "truncated");
    assert_eq!(truncated_err.code, ErrorCode::InvalidArgument);
    assert_eq!(
        truncated_err.details.reason.as_deref(),
        Some("journal_truncated")
    );

    let corrupt = dir.path().join("corrupt-journal");
    fs::create_dir_all(&corrupt).expect("corrupt dir");
    fs::write(
        corrupt.join("journal.jsonl"),
        b"{\"v\":1,\"op\":\"bogus\"}\n",
    )
    .expect("bogus");
    let corrupt_err = must_err(JournalLaunchIntentStore::open(&corrupt), "corrupt");
    assert_eq!(corrupt_err.code, ErrorCode::InvalidArgument);
    assert_eq!(
        corrupt_err.details.reason.as_deref(),
        Some("journal_corrupt")
    );
}

/// Live rootless-crun escape attempts. Ignored on every host: Windows must not
/// fail the suite because namespaces are absent, and a skipped probe is not a
/// pass. Running it with `--ignored` on a non-Linux host fails closed.
#[tokio::test(flavor = "current_thread")]
#[ignore = "linux os escape probe; ignored is not a sandbox pass"]
async fn sandbox_blocks_host_escape_live() {
    if !cfg!(target_os = "linux") {
        panic!("refusing to treat a non-linux run as a sandbox pass");
    }
    let dir = tempdir().expect("temp dir");
    let plan = build_sandbox_plan(&SandboxInput {
        certificate: base_key(),
        db: DbIdentity::new("sqlite:canonical-db-live").expect("db"),
        boot_epoch: Epoch(1),
        incarnation: "00000000-0000-4000-8000-00000000000b".parse().expect("id"),
        scratch: abs_dir(dir.path(), "scratch-live"),
        project: abs_dir(dir.path(), "project-live"),
        home: abs_dir(dir.path(), "home-live"),
        other_scratches: vec![abs_dir(dir.path(), "other-live")],
        decoy_paths: vec![abs_dir(dir.path(), "decoy-live")],
        inherited_env: BTreeMap::new(),
        env_allowlist: BTreeMap::from([("LANG".to_string(), "C".to_string())]),
        global_mcp: false,
    })
    .expect("plan");
    let report = codeg_lib::roundtable::attempt_live_escapes(&plan)
        .await
        .expect("live probe must actually run");
    assert!(report.denied_decoy_absolute_path);
    assert!(report.denied_home);
    assert!(report.denied_project);
    assert!(report.denied_other_scratch);
    assert!(report.denied_proc);
    assert!(report.denied_inherited_fd);
    assert!(report.denied_symlink);
    assert!(report.denied_device);
    assert!(report.denied_socket);
    assert!(report.denied_global_mcp);
    assert!(report.denied_arbitrary_network);
}

#[tokio::test]
async fn sandbox_prepare_rejects_oci_namespace_tampering() {
    let dir = tempdir().unwrap();
    let plan = build_sandbox_plan(&SandboxInput {
        certificate: base_key(),
        db: DbIdentity::new("sqlite:tamper-review").unwrap(),
        boot_epoch: Epoch(1),
        incarnation: "00000000-0000-4000-8000-00000000000c".parse().unwrap(),
        scratch: abs_dir(dir.path(), "scratch"),
        project: abs_dir(dir.path(), "project"),
        home: abs_dir(dir.path(), "home"),
        other_scratches: vec![],
        decoy_paths: vec![],
        inherited_env: BTreeMap::new(),
        env_allowlist: BTreeMap::new(),
        global_mcp: false,
    })
    .unwrap();
    let isolator =
        LinuxOciIsolator::new(JournalLaunchIntentStore::open(&dir.path().join("journal")).unwrap());
    let mut edited = plan;
    edited.oci["linux"]["namespaces"] = serde_json::json!([]);
    assert!(
        isolator.prepare(&edited).await.is_err(),
        "OCI edits must invalidate a prepared plan"
    );
    edited.plan_hash = Hash256::sha256(&serde_json::to_vec(&edited.oci).unwrap());
    assert!(
        isolator.prepare(&edited).await.is_err(),
        "a recomputed hash cannot replace required namespaces"
    );
}

#[test]
fn qualified_rootfs_digest_detects_frozen_file_changes() {
    let dir = tempdir().unwrap();
    fs::create_dir(dir.path().join("bin")).unwrap();
    let binary = dir.path().join("bin/member");
    fs::write(&binary, b"pinned member").unwrap();
    let original = codeg_lib::roundtable::qualified_rootfs_digest(dir.path()).unwrap();
    assert_eq!(
        original,
        codeg_lib::roundtable::qualified_rootfs_digest(dir.path()).unwrap()
    );
    fs::write(binary, b"replaced member").unwrap();
    assert_ne!(
        original,
        codeg_lib::roundtable::qualified_rootfs_digest(dir.path()).unwrap()
    );
}

#[cfg(unix)]
#[test]
fn qualified_rootfs_digest_hashes_symlink_text_and_special_files() {
    let dir = tempdir().unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::os::unix::fs::symlink("/usr/bin/mawk", dir.path().join("awk-link")).unwrap();
    std::os::unix::fs::symlink("missing-target", dir.path().join("dangling")).unwrap();
    let fifo = dir.path().join("pipe");
    assert!(std::process::Command::new("mkfifo")
        .arg(&fifo)
        .status()
        .expect("mkfifo")
        .success());
    let first = codeg_lib::roundtable::qualified_rootfs_digest_detail(dir.path()).expect("digest");
    std::fs::remove_file(dir.path().join("awk-link")).unwrap();
    std::os::unix::fs::symlink("/usr/bin/gawk", dir.path().join("awk-link")).unwrap();
    let changed =
        codeg_lib::roundtable::qualified_rootfs_digest_detail(dir.path()).expect("digest");
    assert_ne!(first, changed);
    let status = fs::read_to_string("/proc/self/status").unwrap_or_default();
    let root = status
        .lines()
        .any(|line| line.starts_with("Uid:") && line.split_whitespace().nth(1) == Some("0"));
    if !root {
        let hidden = dir.path().join("root-only");
        fs::write(&hidden, b"shadow").unwrap();
        fs::set_permissions(&hidden, fs::Permissions::from_mode(0o000)).unwrap();
        let error = codeg_lib::roundtable::qualified_rootfs_digest_detail(dir.path())
            .expect_err("unreadable file");
        assert!(error.contains("root-only"), "{error}");
        assert!(error.contains("chown"), "{error}");
        let _ = fs::set_permissions(&hidden, fs::Permissions::from_mode(0o644));
    }
}

#[test]
fn qualified_profile_hash_binds_execution_template_without_attempt_socket_names() {
    let dir = tempdir().unwrap();
    let key = base_key();
    let profile = codeg_lib::roundtable::QualifiedOciProfile {
        runtime: key.binaries[0].clone(),
        rootfs: dir.path().join("rootfs"),
        rootfs_sha256: digest(41),
        runtime_root: dir.path().join("runtime"),
        cgroup_root: PathBuf::from("/sys/fs/cgroup/codeg"),
        cli_args: vec!["--acp".into()],
        service_socket: Some(dir.path().join("one.sock")),
        gateway_socket: None,
        auth_mounts: Vec::new(),
        container_env: BTreeMap::new(),
    };
    let first = codeg_lib::roundtable::qualified_oci_profile_hash(&profile, &key).unwrap();
    let mut next = profile.clone();
    next.service_socket = Some(dir.path().join("two.sock"));
    assert_eq!(
        first,
        codeg_lib::roundtable::qualified_oci_profile_hash(&next, &key).unwrap()
    );
    next.service_socket = None;
    next.gateway_socket = Some(dir.path().join("gateway.sock"));
    assert_eq!(
        first,
        codeg_lib::roundtable::qualified_oci_profile_hash(&next, &key).unwrap()
    );
    next.rootfs_sha256 = digest(42);
    assert_ne!(
        first,
        codeg_lib::roundtable::qualified_oci_profile_hash(&next, &key).unwrap()
    );
    next = profile;
    next.cli_args.push("--other-contract".into());
    assert_ne!(
        first,
        codeg_lib::roundtable::qualified_oci_profile_hash(&next, &key).unwrap()
    );
}

fn scratch_review_input(root: &Path) -> SandboxInput {
    SandboxInput {
        certificate: base_key(),
        db: DbIdentity::new("scratch-review").unwrap(),
        boot_epoch: Epoch(1),
        incarnation: "00000000-0000-4000-8000-00000000000d".parse().unwrap(),
        scratch: abs_dir(root, "scratch"),
        project: abs_dir(root, "project"),
        home: abs_dir(root, "home"),
        other_scratches: vec![],
        decoy_paths: vec![],
        inherited_env: BTreeMap::new(),
        env_allowlist: BTreeMap::new(),
        global_mcp: false,
    }
}

#[test]
fn sandbox_scratch_cannot_mount_a_forbidden_parent() {
    let dir = tempdir().unwrap();
    let mut input = scratch_review_input(dir.path());
    input.scratch = dir.path().canonicalize().unwrap();
    assert!(
        build_sandbox_plan(&input).is_err(),
        "a parent scratch exposes the entire forbidden tree"
    );
}

#[test]
fn sandbox_scratch_cannot_use_a_parent_traversal_alias() {
    let dir = tempdir().unwrap();
    let mut input = scratch_review_input(dir.path());
    fs::create_dir(input.project.join("child")).unwrap();
    // Verbatim Windows Path::join normalizes dot-dot itself; use the normal
    // spelling so the plan builder receives the unresolved alias.
    let project = input.project.to_string_lossy();
    input.scratch = PathBuf::from(project.strip_prefix(r"\\?\").unwrap_or(&project))
        .join("child")
        .join("..");
    assert!(
        build_sandbox_plan(&input).is_err(),
        "canonical aliases must not mount the project"
    );
}

#[cfg(unix)]
#[test]
fn sandbox_scratch_cannot_use_a_symlink_to_a_forbidden_directory() {
    let dir = tempdir().unwrap();
    let mut input = scratch_review_input(dir.path());
    let alias = dir.path().join("alias");
    std::os::unix::fs::symlink(&input.project, &alias).unwrap();
    input.scratch = alias;
    assert!(build_sandbox_plan(&input).is_err());
}

#[test]
fn qualified_default_home_scratch_reaches_the_certificate_check() {
    let dir = tempdir().unwrap();
    let mut input = scratch_review_input(dir.path());
    let runtime_root = input.home.join(".codeg/roundtable/oci");
    input.scratch = runtime_root
        .join("runs")
        .join(input.incarnation.to_string())
        .join("scratch");
    fs::create_dir_all(&input.scratch).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(
            input.scratch.parent().unwrap(),
            fs::Permissions::from_mode(0o700),
        )
        .unwrap();
    }
    let profile = codeg_lib::roundtable::QualifiedOciProfile {
        runtime: input.certificate.binaries[0].clone(),
        rootfs: dir.path().join("rootfs"),
        rootfs_sha256: digest(2),
        runtime_root,
        cgroup_root: dir.path().join("unqualified-cgroup"),
        cli_args: vec![],
        service_socket: Some(dir.path().join("service.sock")),
        gateway_socket: Some(dir.path().join("gateway.sock")),
        auth_mounts: vec![],
        container_env: BTreeMap::new(),
    };
    // Deliberately use a stale certificate: passing path validation does not
    // authorize any runtime, socket, auth-file, or cgroup operation.
    let error = codeg_lib::roundtable::build_qualified_sandbox_plan(&input, &profile).unwrap_err();
    assert_eq!(
        error.details.reason.as_deref(),
        Some("qualification_plan_drift")
    );
    for rejected in [
        input.home.clone(),
        profile.runtime_root.clone(),
        input.scratch.parent().unwrap().to_path_buf(),
        input.home.join(".ssh"),
    ] {
        fs::create_dir_all(&rejected).unwrap();
        input.scratch = rejected;
        let error =
            codeg_lib::roundtable::build_qualified_sandbox_plan(&input, &profile).unwrap_err();
        assert!(matches!(
            error.details.reason.as_deref(),
            Some("scratch_is_host_path" | "scratch_not_owned")
        ));
    }
}
