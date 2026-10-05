//! P07a service-owned admission. The lane is in-memory. Nothing here execs a
//! CLI, calls a model, or resolves a program through PATH or npx.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use codeg_lib::acp::agent_process::roundtable_process_exec_count;
use codeg_lib::acp::connection::{connection_channel_for_test, ConnectionCommand};
use codeg_lib::acp::manager::ConnectionManager;
use codeg_lib::acp::types::PromptInputBlock;
use codeg_lib::auto_title::ConnectionPurpose;
use codeg_lib::roundtable::{
    build_sandbox_plan, prepare_roundtable_connection, try_enqueue, AdmittedPrompt,
    CertifiedBinary, ConnectionOwner, DbIdentity, InteractivePermission, IsolationProvider,
    JournalLaunchIntentStore, LinuxOciIsolator, OsIdentity, PreparedSandbox, PrivateRuntimeSink,
    QualificationKey, QueueReject, RoundtableLaunch, SandboxInput, TurnGeneration,
    ROUNDTABLE_SERVICE_LABEL,
};
use roundtable_protocol::{AttemptId, Epoch, ErrorCode, Hash256, IncarnationId, RoomId};
use tempfile::tempdir;

#[test]
fn runtime_fix_roundtable_host_policy_is_noninteractive_and_unhosted() {
    use codeg_lib::acp::host_tools_policy::HostToolsPolicy;
    assert!(!HostToolsPolicy::Default
        .for_purpose(ConnectionPurpose::Roundtable)
        .hosts_channels());
    assert!(HostToolsPolicy::Default
        .for_purpose(ConnectionPurpose::User)
        .hosts_channels());
    assert!(!HostToolsPolicy::Agent
        .for_purpose(ConnectionPurpose::User)
        .hosts_channels());
    assert!(
        codeg_lib::acp::host_tools_policy::denies_interactive_permission(
            ConnectionPurpose::Roundtable
        )
    );
    assert!(
        !codeg_lib::acp::host_tools_policy::denies_interactive_permission(ConnectionPurpose::User)
    );
}

fn digest(byte: u8) -> Hash256 {
    Hash256::from_bytes([byte; 32])
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

fn room_id() -> RoomId {
    "00000000-0000-4000-8000-0000000000a1"
        .parse()
        .expect("room")
}

fn attempt_id() -> AttemptId {
    "00000000-0000-4000-8000-0000000000a2"
        .parse()
        .expect("attempt")
}

async fn prepared_sandbox() -> PreparedSandbox {
    let dir = tempdir().expect("temp");
    let scratch = dir.path().join("scratch");
    let project = dir.path().join("project");
    let home = dir.path().join("home");
    let other = dir.path().join("other");
    let decoy = dir.path().join("decoy");
    for path in [&scratch, &project, &home, &other, &decoy] {
        std::fs::create_dir_all(path).expect("dir");
    }
    let mut inherited = BTreeMap::new();
    inherited.insert("HOME".to_string(), home.display().to_string());
    inherited.insert("PATH".to_string(), "/usr/bin:/bin".to_string());
    inherited.insert("MCP_CONFIG".to_string(), "/global/mcp.json".to_string());
    inherited.insert(
        "CODEG_RT_HOST_SENTINEL".to_string(),
        "from-inherited".into(),
    );
    let mut allow = BTreeMap::new();
    allow.insert("LANG".to_string(), "C".to_string());
    let input = SandboxInput {
        certificate: QualificationKey {
            os: OsIdentity {
                name: "linux".into(),
                version: "debian-12".into(),
            },
            binaries: vec![
                CertifiedBinary {
                    role: "crun".into(),
                    absolute_path: crun_path().display().to_string(),
                    version: "crun-1.15".into(),
                    sha256: digest(1),
                },
                CertifiedBinary {
                    role: "cli".into(),
                    absolute_path: cli_path().display().to_string(),
                    version: "codex-acp-2.1.1".into(),
                    sha256: digest(2),
                },
            ],
            image_digest: format!("sha256:{}", digest(3).to_hex()),
            policy_hash: digest(4),
            tool_contract_hash: digest(5),
            core_hash: digest(6),
            adapter_version: "codex-acp@2.1.1".into(),
            isolator_version: "linux-oci-1".into(),
            plan_hash: digest(7),
        },
        db: DbIdentity::new("sqlite:roundtable-p07a").expect("db"),
        boot_epoch: Epoch(7),
        incarnation: "00000000-0000-4000-8000-00000000000a"
            .parse::<IncarnationId>()
            .expect("incarnation"),
        scratch,
        project,
        home,
        other_scratches: vec![other],
        decoy_paths: vec![decoy],
        inherited_env: inherited,
        env_allowlist: allow,
        global_mcp: false,
    };
    let plan = build_sandbox_plan(&input).expect("plan");
    let journal = JournalLaunchIntentStore::open(&dir.path().join("journal")).expect("journal");
    let isolator = LinuxOciIsolator::new(journal);
    let prepared = isolator.prepare(&plan).await.expect("prepared sandbox");
    drop(isolator);
    prepared
}

fn launch(sandbox: PreparedSandbox, spawn_failpoint: bool) -> RoundtableLaunch {
    RoundtableLaunch {
        room_id: room_id(),
        attempt_id: attempt_id(),
        boot_epoch: Epoch(7),
        sandbox,
        spawn_failpoint,
    }
}

fn prompt(text: &str) -> AdmittedPrompt {
    AdmittedPrompt {
        blocks: vec![PromptInputBlock::Text {
            text: text.to_string(),
        }],
        before_send: None,
    }
}

#[derive(Default)]
struct RecordingSink {
    attached: Mutex<Vec<(String, ConnectionOwner)>>,
}

impl PrivateRuntimeSink for RecordingSink {
    fn attach(&self, connection_id: &str, owner: &ConnectionOwner) {
        self.attached
            .lock()
            .expect("sink")
            .push((connection_id.to_string(), owner.clone()));
    }
}

#[tokio::test]
async fn roundtable_policy_is_not_hidden_generation() {
    assert!(
        !ConnectionPurpose::Roundtable.is_hidden_generation(),
        "roundtable is not hidden generation"
    );
    assert!(!ConnectionPurpose::User.is_hidden_generation());
    assert!(!ConnectionPurpose::Delegation.is_hidden_generation());
    assert!(!ConnectionPurpose::InternalProbe.is_hidden_generation());
    assert!(ConnectionPurpose::InternalTitle.is_hidden_generation());
    assert!(ConnectionPurpose::InternalTranslate.is_hidden_generation());

    temp_env::async_with_vars(
        [
            ("CODEG_RT_HOST_SENTINEL", Some("leak")),
            ("CODEG_ACP_HOST_TOOLS", Some("default")),
            ("PATH", Some("/host/bin")),
        ],
        async {
            let sandbox = prepared_sandbox().await;
            let manager = ConnectionManager::new();
            let sink = Arc::new(RecordingSink::default());
            let denied = prepare_roundtable_connection(
                &manager,
                launch(sandbox.clone(), false),
                sink.clone(),
            )
            .await;
            let Err(denied) = denied else {
                panic!("production spawn stays off");
            };
            assert_eq!(denied.code, ErrorCode::PolicyUnenforceable);
            assert_eq!(
                denied.details.reason.as_deref(),
                Some("policy_unenforceable")
            );
            assert!(manager.list_connections().await.is_empty());
            assert_eq!(roundtable_process_exec_count(), 0);

            let prepared =
                prepare_roundtable_connection(&manager, launch(sandbox, true), sink.clone())
                    .await
                    .expect("failpoint installs the lane without exec");
            assert_eq!(roundtable_process_exec_count(), 0);
            assert_eq!(prepared.compatibility_label, ROUNDTABLE_SERVICE_LABEL);
            assert_eq!(
                prepared.owner,
                ConnectionOwner::Service {
                    room_id: room_id(),
                    attempt_id: attempt_id(),
                    boot_epoch: Epoch(7),
                }
            );
            assert!(prepared.policy.service_owned);
            assert_eq!(
                prepared.policy.interactive_permissions,
                InteractivePermission::Deny
            );
            assert!(!prepared.policy.host_fs);
            assert!(!prepared.policy.host_terminal);
            assert_eq!(
                prepared.policy.companion_groups,
                vec!["roundtable".to_string()]
            );
            assert!(!prepared.policy.ordinary_conversation_import);
            assert!(!prepared.policy.automatic_title);
            assert!(!prepared.policy.hidden_generation);
            assert!(prepared.launch.program.is_absolute());
            assert_eq!(prepared.launch.program, cli_path());
            assert!(!prepared.launch.uses_path_lookup);
            assert!(!prepared.launch.uses_npx);
            assert!(prepared.launch.env_cleared);
            assert_eq!(
                prepared.launch.env.get("LANG").map(String::as_str),
                Some("C")
            );
            for forbidden in ["PATH", "HOME", "CODEG_RT_HOST_SENTINEL", "MCP_CONFIG"] {
                assert!(
                    !prepared.launch.env.contains_key(forbidden),
                    "{forbidden} must not be inherited"
                );
            }
            assert!(prepared
                .launch
                .args
                .iter()
                .all(|arg| !arg.to_ascii_lowercase().contains("npx")));
            let state = prepared.state.read().await;
            assert_eq!(state.purpose, ConnectionPurpose::Roundtable);
            assert!(!state.purpose.is_hidden_generation());
            assert_eq!(state.owner_window_label, ROUNDTABLE_SERVICE_LABEL);
            assert!(state.conversation_id.is_none());
            assert!(state.active_turn.is_none());
            drop(state);
            let attached = sink.attached.lock().expect("sink").clone();
            assert_eq!(attached.len(), 1);
            assert_eq!(attached[0].0, prepared.connection_id);
            assert!(matches!(attached[0].1, ConnectionOwner::Service { .. }));
            assert_eq!(
                manager.connection_owner(&prepared.connection_id).await,
                Some(prepared.owner.clone())
            );
        },
    )
    .await;
}

#[tokio::test]
async fn try_write_failure_does_not_mutate_turn() {
    let sandbox = prepared_sandbox().await;
    let manager = ConnectionManager::new();
    let sink = Arc::new(RecordingSink::default());
    let prepared = prepare_roundtable_connection(&manager, launch(sandbox, true), sink)
        .await
        .expect("lane");
    let state = prepared.state.clone();
    {
        let mut guard = state.write().await;
        guard.parent_turn_generation = 4;
        guard.active_provider_turn_id = Some("keep".into());
        let prepared_prompt = prepared.prepare_prompt(prompt("held")).expect("reserved");
        let rejected = try_enqueue(prepared_prompt);
        assert_eq!(rejected, Err(QueueReject::QueueRejected));
        assert_eq!(guard.parent_turn_generation, 4);
        assert!(guard.active_turn_generation.is_none());
        assert!(!guard.turn_in_flight);
        assert_eq!(guard.active_provider_turn_id.as_deref(), Some("keep"));
        assert!(guard.active_turn.is_none());
        assert!(guard.conversation_id.is_none());
    }

    let full = prepared
        .prepare_prompt(prompt("fills"))
        .expect("capacity after cancel");
    let blocked = prepared.prepare_prompt(prompt("overflow-slot"));
    assert!(matches!(blocked, Err(QueueReject::QueueRejected)));
    {
        let guard = state.read().await;
        assert_eq!(guard.parent_turn_generation, 4);
        assert!(!guard.turn_in_flight);
        assert_eq!(guard.active_provider_turn_id.as_deref(), Some("keep"));
    }
    drop(full);
}

#[tokio::test]
async fn generation_is_updated_before_send() {
    let sandbox = prepared_sandbox().await;
    let manager = ConnectionManager::new();
    let sink = Arc::new(RecordingSink::default());
    let prepared = prepare_roundtable_connection(&manager, launch(sandbox, true), sink)
        .await
        .expect("lane");
    {
        let mut state = prepared.state.write().await;
        state.parent_turn_generation = u64::MAX;
        state.active_provider_turn_id = Some("stale".into());
        state.active_turn_generation = None;
        state.turn_in_flight = false;
    }
    let overflow = prepared
        .prepare_prompt(prompt("overflow"))
        .expect("reserve");
    assert_eq!(try_enqueue(overflow), Err(QueueReject::Overflow));
    {
        let state = prepared.state.read().await;
        assert_eq!(state.parent_turn_generation, u64::MAX);
        assert!(state.active_turn_generation.is_none());
        assert!(!state.turn_in_flight);
        assert_eq!(state.active_provider_turn_id.as_deref(), Some("stale"));
        assert!(state.active_turn.is_none());
        assert!(state.conversation_id.is_none());
    }

    {
        let mut state = prepared.state.write().await;
        state.parent_turn_generation = 3;
        state.active_provider_turn_id = Some("old".into());
    }
    let inbox = prepared.command_inbox();
    let saw_before_send = Arc::new(AtomicBool::new(false));
    let flag = saw_before_send.clone();
    let hook_inbox = inbox.clone();
    let mut admitted = prompt("turn");
    admitted.before_send = Some(Arc::new(move |state| {
        assert_eq!(state.parent_turn_generation, 4);
        assert_eq!(state.active_turn_generation, Some(4));
        assert!(state.turn_in_flight);
        assert!(state.active_provider_turn_id.is_none());
        assert!(state.active_turn.is_none());
        assert!(
            hook_inbox.lock().expect("inbox").try_recv().is_err(),
            "generation must be visible before permit.send"
        );
        flag.store(true, Ordering::SeqCst);
    }));
    let reserved = prepared.prepare_prompt(admitted).expect("reserve");
    assert_eq!(try_enqueue(reserved), Ok(TurnGeneration(4)));
    assert!(saw_before_send.load(Ordering::SeqCst));
    match inbox.lock().expect("inbox").try_recv().expect("enqueued") {
        ConnectionCommand::Prompt {
            blocks,
            user_message,
            mark_awaiting_reply,
            bypass_autonomous_hold,
            turn_generation,
        } => {
            assert!(user_message.is_none());
            assert!(!mark_awaiting_reply);
            assert!(!bypass_autonomous_hold);
            assert_eq!(turn_generation, 4);
            assert!(matches!(
                blocks.as_slice(),
                [PromptInputBlock::Text { text }] if text == "turn"
            ));
        }
        _ => panic!("unexpected command"),
    }
    {
        let state = prepared.state.read().await;
        assert_eq!(state.parent_turn_generation, 4);
        assert_eq!(state.active_turn_generation, Some(4));
        assert!(state.turn_in_flight);
        assert!(state.active_provider_turn_id.is_none());
        assert!(state.conversation_id.is_none());
    }

    let second = prepared.prepare_prompt(prompt("busy")).expect("slot free");
    assert_eq!(try_enqueue(second), Err(QueueReject::QueueRejected));
    let state = prepared.state.read().await;
    assert_eq!(state.parent_turn_generation, 4);
    assert_eq!(state.active_turn_generation, Some(4));
    assert!(state.turn_in_flight);
}

#[tokio::test]
async fn observer_close_does_not_reap_service() {
    let sandbox = prepared_sandbox().await;
    let manager = ConnectionManager::new();
    let sink = Arc::new(RecordingSink::default());
    let prepared = prepare_roundtable_connection(&manager, launch(sandbox, true), sink)
        .await
        .expect("service lane");
    manager
        .insert_test_connection(
            "window-decoy",
            codeg_lib::models::AgentType::Codex,
            None,
            codeg_lib::web::event_bridge::EventEmitter::Noop,
        )
        .await;
    assert!(
        manager
            .retag_test_connection_window("window-decoy", ROUNDTABLE_SERVICE_LABEL, None)
            .await
    );
    manager
        .insert_test_connection(
            "other-window",
            codeg_lib::models::AgentType::Codex,
            None,
            codeg_lib::web::event_bridge::EventEmitter::Noop,
        )
        .await;
    assert!(
        manager
            .retag_test_connection_window("other-window", "main", Some("op-main".into()))
            .await
    );

    let removed = manager
        .disconnect_by_owner_window(ROUNDTABLE_SERVICE_LABEL)
        .await;
    assert_eq!(removed, 1, "only the window owner of the sentinel label");
    assert!(
        manager
            .has_connection_map_entry_for_test(&prepared.connection_id)
            .await
    );
    assert!(
        !manager
            .has_connection_map_entry_for_test("window-decoy")
            .await
    );
    assert!(
        manager
            .has_connection_map_entry_for_test("other-window")
            .await
    );
    assert!(matches!(
        manager.connection_owner(&prepared.connection_id).await,
        Some(ConnectionOwner::Service { .. })
    ));

    let removed_again = manager
        .disconnect_by_owner_window_and_operation(ROUNDTABLE_SERVICE_LABEL, "")
        .await;
    assert_eq!(removed_again, 0);
    assert!(
        manager
            .has_connection_map_entry_for_test(&prepared.connection_id)
            .await
    );
    assert_eq!(manager.disconnect_by_owner_window("main").await, 1);
    assert!(
        manager
            .has_connection_map_entry_for_test(&prepared.connection_id)
            .await
    );
    assert!(
        !manager
            .has_connection_map_entry_for_test("other-window")
            .await
    );
}

#[tokio::test]
async fn last_sender_drop_with_pending_permit() {
    let (tx, mut rx, closed) = connection_channel_for_test::<u8>(1);
    let permit = tx.reserve_owned().await.expect("reserve");
    drop(tx);
    assert!(
        !*closed.borrow(),
        "a pending owned permit must keep sender liveness above zero"
    );
    permit.send(9);
    assert_eq!(rx.recv().await, Some(9));
    assert!(
        *closed.borrow(),
        "sending the last owned permit drops its LaneSender"
    );
}

#[tokio::test]
async fn cancel_owned_reservation_releases_capacity() {
    let (tx, _rx, _closed) = connection_channel_for_test::<u8>(1);
    let permit = tx.reserve_owned().await.expect("reserve");
    assert!(
        tx.try_reserve_owned().is_err(),
        "the owned reservation holds the only slot"
    );
    drop(permit);
    let released = tokio::time::timeout(Duration::from_secs(2), tx.reserve_owned())
        .await
        .expect("drop must release capacity")
        .expect("reserve");
    drop(released);
}
