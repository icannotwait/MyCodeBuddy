//! Controlled execution uses the same scheduler, scoped tools, SQLite accept,
//! close and publication as an installed provider. No model process is run.
use async_trait::async_trait;
use codeg_lib::acp::manager::ConnectionManager;
use codeg_lib::roundtable::*;
use roundtable_protocol::*;
use sea_orm::ConnectionTrait;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use crate::support;

struct ControlledExecutor {
    root: PathBuf,
    prompts: Mutex<Vec<Value>>,
    requests: Mutex<Vec<RoundtableTurnRequest>>,
}
fn profile() -> QualifiedContextProfile {
    QualifiedContextProfile::proposed(
        "test-bytes",
        Hash256::from_bytes([1; 32]),
        2_000_000,
        0,
        "controlled-fixture",
    )
}
impl TokenBound for ControlledExecutor {
    fn upper_bound(&self, bytes: &[u8]) -> RtResult<u64> {
        Ok(bytes.len() as u64)
    }
    fn capacity_tokens(&self) -> Option<u64> {
        Some(2_000_000)
    }
}
fn proof(incarnation: IncarnationId) -> CleanupProof {
    CleanupProof {
        process: ProcessTreeProof {
            instance_id: "controlled-no-process".into(),
            incarnation,
            process_tree_empty: true,
        },
        mailbox_empty: true,
        tools_drained: true,
        ingress_drained: true,
    }
}
#[async_trait]
impl RoundtableTurnExecutor for ControlledExecutor {
    async fn capability(&self, _: &RoundtableConfigV1) -> RtResult<RuntimeCapability> {
        Ok(RuntimeCapability {
            recipients: json!(["controlled:test"]),
            qualification_keys: json!([]),
            policy_hash: Hash256::from_bytes([2; 32]),
            profile: profile(),
        })
    }
    fn token_bound(&self) -> &(dyn TokenBound + Send + Sync) {
        self
    }
    async fn cancel_and_reap(&self, identity: RuntimeIdentity) -> RtResult<CleanupProof> {
        Ok(proof(identity.incarnation))
    }
    async fn execute_turn(
        &self,
        request: RoundtableTurnRequest,
    ) -> RtResult<RoundtableTurnOutcome> {
        self.requests.lock().unwrap().push(request.clone());
        let prompt: Value = serde_json::from_slice(&request.prompt).unwrap();
        self.prompts.lock().unwrap().push(prompt.clone());
        assert_eq!(prompt["context"]["topic"], "Review the implementation");
        let result = if request.phase.kind == PhaseKind::Synthesis {
            assert_eq!(
                prompt["context"]["published_messages"]
                    .as_array()
                    .unwrap()
                    .len(),
                2
            );
            json!({"kind":"synthesis","recommendation":{"text":"Choose the reviewed option","aliases":[{"kind":"claim","alias":"c0"}],"inference":false},"alternatives":[],"consensus_items":[],"disagreements":[],"risks":[],"decision_requests":[]})
        } else {
            json!({"kind":"proposal","summary":"A grounded proposal","claims":[{"local_key":"a","text":"Use the shared scheduler","evidence_aliases":[],"confidence":"low"}],"responses":[],"open_questions":[],"position_changes":[]})
        };
        let binding = TokenBinding {
            attempt_id: request.fence.attempt_id,
            room_id: request.room_id,
            fence: request.fence.clone(),
            speaker_id: request.speaker_id,
            tool_version: SERVICE_TOOL_VERSION.into(),
            aliases: request.scope.aliases.clone(),
            result_scope: request.scope.clone(),
            evidence: Default::default(),
            profile: profile(),
        };
        let zero = Hash256::from_bytes([0; 32]);
        let facts = AdmissionFacts {
            certificate: QualificationStatus::NotTested,
            presented_key: QualificationKey {
                os: OsIdentity {
                    name: "controlled".into(),
                    version: "0".into(),
                },
                binaries: vec![],
                image_digest: String::new(),
                policy_hash: zero,
                tool_contract_hash: zero,
                core_hash: zero,
                adapter_version: String::new(),
                isolator_version: String::new(),
                plan_hash: zero,
            },
            qualification_attempts_used: 0,
            qualification_spend_used: 0,
            fixture_hash: zero,
            recipient: String::new(),
        };
        let authority = GateToolAuthority::open(
            &self.root,
            ExecutionScope::Fake,
            facts,
            MonoMs(0),
            MonoMs(u64::MAX),
            binding,
        );
        let token = authority.issue();
        let objects = ObjectStore::open(
            self.root.join("objects"),
            Arc::new(ReservationLedger::new(16_000_000)),
            "00000000-0000-4000-8000-000000000001".parse().unwrap(),
        )
        .unwrap();
        let store = DurableToolStore::open(
            request.store,
            objects,
            ToolSession {
                room_id: request.room_id.to_string(),
                attempt_id: request.fence.attempt_id.to_string(),
                speaker_id: request.speaker_id.to_string(),
                phase_id: request.phase.phase_id.to_string(),
                phase_revision: request.phase.revision.0 as i64,
                manifest_id: request.phase.source_manifest_id.to_string(),
                workspace_snapshot_id: request.phase.source_manifest_id.to_string(),
                manifest_hash: request.phase.source_manifest_hash.to_hex(),
            },
            request.scope,
        );
        let response=invoke_scoped_tool(&token,RoundtableToolCall{name:"submit_result".into(),arguments:json!({"submission_id":uuid::Uuid::new_v4().to_string(),"result":result})},&authority,&store).await?;
        let response: Value = serde_json::from_slice(&response.body).unwrap();
        assert_eq!(response["kind"], "staged", "{response}");
        Ok(RoundtableTurnOutcome {
            completion: RuntimeTurnCompleted {
                fence: request.fence.clone(),
                finish_reason: "completed".into(),
                ingress_watermark: Seq(1),
                tool_barrier: ToolBarrierV1 { drained: true },
                candidate_id: Some(canonical_hash(&result)?.to_hex()),
            },
            cleanup: proof(request.fence.incarnation),
        })
    }
}

#[tokio::test]
async fn runtime_fix_scheduler_publishes_members_then_moderator() {
    let (dir, conn) = support::open_pool(1).await;
    migrate_roundtable(&conn).await.unwrap();
    let store = open_roundtable_store(conn.clone()).await.unwrap();
    let executor = Arc::new(ControlledExecutor {
        root: dir.path().into(),
        prompts: Mutex::new(Vec::new()),
        requests: Mutex::new(Vec::new()),
    });
    let runtime = Arc::new(OwnedParticipantRuntime::with_executor(
        dir.path().into(),
        Arc::new(ConnectionManager::new()),
        executor.clone(),
    ));
    let service = RoundtableService::open(
        ServiceConfig {
            data_dir: dir.path().into(),
            db_path: dir.path().join("roundtable.db"),
            db_identity: DbIdentity::new("runtime-scheduler").unwrap(),
            discover: None,
        },
        store.clone(),
        runtime.clone(),
    )
    .await
    .unwrap();
    let actor = ActorContext::from_trusted_entry(
        "00000000-0000-4000-8000-000000000001".parse().unwrap(),
        OperatorScope::SingleOperator,
        ClientIdentity {
            kind: ClientKind::Web,
            session_ref: "scheduler".into(),
        },
    );
    let config = config();
    let created = service
        .execute_fake_command(
            &actor,
            "roundtable_create",
            json!({"request_id":uuid::Uuid::new_v4().to_string(),"config":config}),
        )
        .await
        .unwrap();
    let room: RoomId = created["room_id"].as_str().unwrap().parse().unwrap();
    conn.execute_unprepared("UPDATE rt_rooms SET status='running'")
        .await
        .unwrap();
    runtime
        .run_room(store, room, serde_json::from_value(config).unwrap())
        .await
        .unwrap();
    assert_eq!(
        support::scalar_i64(
            &conn,
            "SELECT COUNT(*) FROM rt_phases WHERE status='published'"
        )
        .await,
        2
    );
    assert_eq!(
        support::scalar_i64(&conn, "SELECT COUNT(*) FROM rt_messages").await,
        3
    );
    assert_eq!(
        support::scalar_i64(&conn, "SELECT COUNT(*) FROM rt_deliveries").await,
        3
    );
    assert_eq!(
        support::scalar_text(&conn, "SELECT status FROM rt_rooms").await,
        "completed"
    );
    assert_eq!(executor.prompts.lock().unwrap().len(), 3);
    assert_eq!(support::scalar_i64(&conn,"SELECT COUNT(*) FROM rt_events WHERE cause NOT IN ('create','config','start','attempt','accept','close','publish','input','control','recovery','terminal')").await,0);
    assert!(!dir.path().join("roundtable/execution-policy.json").exists());
    let request = executor.requests.lock().unwrap()[0].clone();
    verify_pre_spawn_and_diagnostic_failure(&dir, &conn, request).await;
}

async fn verify_pre_spawn_and_diagnostic_failure(
    dir: &tempfile::TempDir,
    conn: &sea_orm::DatabaseConnection,
    request: RoundtableTurnRequest,
) {
    let rejected = rejected_live_executor_fixture(dir.path().into()).unwrap();
    let incarnation = request.fence.incarnation;
    let error = match rejected.execute_turn(request.clone()).await {
        Ok(_) => panic!("missing report cannot execute"),
        Err(error) => error,
    };
    assert_eq!(
        error.details.reason.as_deref(),
        Some("qualification_artifact_missing")
    );
    let cleanup = rejected
        .cancel_and_reap(RuntimeIdentity {
            incarnation,
            pid: 0,
        })
        .await
        .unwrap();
    assert_eq!(cleanup.process.incarnation, incarnation);
    assert!(cleanup.process.process_tree_empty && cleanup.tools_drained);
    assert!(rejected
        .cancel_and_reap(RuntimeIdentity {
            incarnation: uuid::Uuid::new_v4().to_string().parse().unwrap(),
            pid: 0
        })
        .await
        .is_err());
    let mut diagnostic = DiagnosticCapture::new(vec!["fixture-secret".into()]);
    diagnostic.push("prefix fixture-");
    diagnostic.push("secret suffix.");
    let capture = Mutex::new(Some(diagnostic));
    conn.execute_unprepared("CREATE TRIGGER diagnostic_fail BEFORE UPDATE OF diagnostic_ref ON rt_attempts BEGIN SELECT RAISE(ABORT,'fixture_diagnostic_write'); END").await.unwrap();
    assert!(persist_runtime_diagnostic_fixture(
        &request.store,
        request.room_id,
        request.fence.attempt_id,
        &capture
    )
    .await
    .is_err());
    assert!(capture.lock().unwrap().is_some());
    assert_eq!(
        support::scalar_i64(conn, "SELECT COUNT(*) FROM rt_diagnostics").await,
        0
    );
    conn.execute_unprepared("DROP TRIGGER diagnostic_fail")
        .await
        .unwrap();
    persist_runtime_diagnostic_fixture(
        &request.store,
        request.room_id,
        request.fence.attempt_id,
        &capture,
    )
    .await
    .unwrap();
    assert!(capture.lock().unwrap().is_none());
    assert_eq!(
        support::scalar_i64(conn, "SELECT COUNT(*) FROM rt_diagnostics").await,
        1
    );
    assert_eq!(
        support::scalar_text(conn, "SELECT assistant_prefix FROM rt_diagnostics").await,
        "prefix [redacted] suffix."
    );
}

fn config() -> Value {
    json!({"schema_version":1,"topic":"Review the implementation","workspace_id":"test-workspace","source_refs":[],"participants":[{"ordinal":0,"role":"reviewer","provider_ref":"provider:a"},{"ordinal":1,"role":"critic","provider_ref":"provider:b"}],"moderator_ordinal":0,"strategy":{"type":"phased_rounds","version":1,"critique_rounds":0},"concurrency":1,"strict_snapshot_v1":true,"budgets":{"room_budget":"900000","phase_budget":"450000"},"timeouts":{"attempt_timeout":"225000"},"quotas":{"output_byte_limit":8192,"input_byte_limit":16384,"interjection_byte_limit":16384}})
}

struct HangingExecutor {
    started: tokio::sync::Notify,
    active: Mutex<Vec<IncarnationId>>,
    cancelled: std::sync::atomic::AtomicUsize,
    dropped: Arc<std::sync::atomic::AtomicUsize>,
}
struct DroppedTurn(Arc<std::sync::atomic::AtomicUsize>);
impl Drop for DroppedTurn {
    fn drop(&mut self) {
        self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
}
impl TokenBound for HangingExecutor {
    fn upper_bound(&self, bytes: &[u8]) -> RtResult<u64> {
        Ok(bytes.len() as u64)
    }
    fn capacity_tokens(&self) -> Option<u64> {
        Some(2_000_000)
    }
}
#[async_trait]
impl RoundtableTurnExecutor for HangingExecutor {
    async fn capability(&self, _: &RoundtableConfigV1) -> RtResult<RuntimeCapability> {
        Ok(RuntimeCapability {
            recipients: json!([]),
            qualification_keys: json!([]),
            policy_hash: Hash256::from_bytes([2; 32]),
            profile: profile(),
        })
    }
    fn token_bound(&self) -> &(dyn TokenBound + Send + Sync) {
        self
    }
    async fn execute_turn(
        &self,
        request: RoundtableTurnRequest,
    ) -> RtResult<RoundtableTurnOutcome> {
        self.active.lock().unwrap().push(request.fence.incarnation);
        let _drop = DroppedTurn(self.dropped.clone());
        self.started.notify_one();
        std::future::pending().await
    }
    async fn cancel_and_reap(&self, identity: RuntimeIdentity) -> RtResult<CleanupProof> {
        let mut active = self.active.lock().unwrap();
        let index = active
            .iter()
            .position(|id| *id == identity.incarnation)
            .expect("known controlled incarnation");
        active.remove(index);
        self.cancelled
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Ok(proof(identity.incarnation))
    }
}
#[tokio::test]
async fn runtime_fix_budget_exhaustion_cleans_cancelled_future() {
    let (dir, conn) = support::open_pool(1).await;
    migrate_roundtable(&conn).await.unwrap();
    let store = open_roundtable_store(conn.clone()).await.unwrap();
    let executor = Arc::new(HangingExecutor {
        started: tokio::sync::Notify::new(),
        active: Mutex::new(Vec::new()),
        cancelled: std::sync::atomic::AtomicUsize::new(0),
        dropped: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
    });
    let runtime = Arc::new(OwnedParticipantRuntime::with_executor(
        dir.path().into(),
        Arc::new(ConnectionManager::new()),
        executor.clone(),
    ));
    let service = RoundtableService::open(
        ServiceConfig {
            data_dir: dir.path().into(),
            db_path: dir.path().join("roundtable.db"),
            db_identity: DbIdentity::new("budget-cancel-runtime").unwrap(),
            discover: None,
        },
        store,
        runtime,
    )
    .await
    .unwrap();
    let actor = ActorContext::from_trusted_entry(
        "00000000-0000-4000-8000-000000000001".parse().unwrap(),
        OperatorScope::SingleOperator,
        ClientIdentity {
            kind: ClientKind::Web,
            session_ref: "budget-cancel".into(),
        },
    );
    let created = service
        .execute_fake_command(
            &actor,
            "roundtable_create",
            json!({"request_id":uuid::Uuid::new_v4().to_string(),"config":config()}),
        )
        .await
        .unwrap();
    let room = created["room_id"].clone();
    conn.execute_unprepared("UPDATE rt_rooms SET status='paused'")
        .await
        .unwrap();
    let resumed=service.execute_fake_command(&actor,"roundtable_resume",json!({"room_id":room,"request_id":uuid::Uuid::new_v4().to_string(),"expected_revision":created["revision"],"recovery_consent":true})).await.unwrap();
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        executor.started.notified(),
    )
    .await
    .unwrap();
    conn.execute_unprepared("UPDATE rt_rooms SET remaining_active_ms=0")
        .await
        .unwrap();
    conn.execute_unprepared("UPDATE rt_active_time_leases SET prepaid_ms=0")
        .await
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if executor
                .cancelled
                .load(std::sync::atomic::Ordering::Relaxed)
                == 1
                && support::scalar_i64(
                    &conn,
                    "SELECT COUNT(*) FROM rt_attempts WHERE cleanup_state='confirmed'",
                )
                .await
                    == 1
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();
    assert!(executor.active.lock().unwrap().is_empty());
    assert_eq!(
        executor.dropped.load(std::sync::atomic::Ordering::Relaxed),
        1
    );
    assert_eq!(
        support::scalar_text(&conn, "SELECT status FROM rt_rooms").await,
        "paused"
    );
    assert!(
        support::scalar_i64(&conn, "SELECT run_epoch FROM rt_rooms").await
            > resumed["run_epoch"]
                .as_str()
                .unwrap()
                .parse::<i64>()
                .unwrap()
    );
    assert_eq!(
        support::scalar_i64(&conn, "SELECT COUNT(*) FROM rt_active_time_leases").await,
        0
    );
    assert!(!dir.path().join("roundtable/execution-policy.json").exists());
}

#[tokio::test]
async fn runtime_fix_streaming_diagnostic_redacts_split_secret_before_clipping() {
    let secret = "attempt-secret-crosses-chunks";
    let mut capture = DiagnosticCapture::new(vec![secret.into()]);
    capture.push("prefix attempt-secret-");
    capture.push("crosses-chunks ");
    capture.push(&"字".repeat(30_000));
    capture.push(" suffix attempt-secret-crosses-");
    capture.push("chunks end");
    let (result, hash) = capture.finish().await.unwrap();
    assert!(result.truncated);
    assert!(result.text.len() <= 65_536);
    assert!(result.text.starts_with("prefix [redacted] "));
    assert!(result.text.ends_with(" suffix [redacted] end"));
    assert!(!result.text.contains("attempt-secret"));
    assert_ne!(hash, Hash256::from_bytes([0; 32]));
    assert!(result.total_bytes > 90_000);
}

#[tokio::test]
async fn runtime_fix_live_gateway_charges_incomplete_stream_and_preserves_uncertainty() {
    let dir = tempfile::tempdir().unwrap();
    let request = json!({"model":"fixture-model","store":false,"input":[],"tools":[],"reasoning":{"effort":"low"}});
    let incomplete = b"data: {\"type\":\"response.in_progress\"}\n\n".to_vec();
    let complete = br#"{"status":"completed","output":[]}"#.to_vec();
    let seen = exercise_live_gateway_fixture(
        dir.path(),
        profile(),
        vec![request.clone(), request],
        vec![
            (200, "text/event-stream".into(), incomplete.clone()),
            (200, "application/json".into(), complete.clone()),
        ],
    )
    .await
    .unwrap();
    assert_eq!(seen.requests_sent, 2);
    assert_eq!(
        seen.generated_bytes,
        (incomplete.len() + complete.len()) as u64
    );
    assert_eq!(seen.errors, vec![Some("upstream_incomplete".into()), None]);
    assert!(seen.uncertain);
}

#[tokio::test]
async fn runtime_fix_live_gateway_cumulative_overrun_blocks_next_send() {
    let dir = tempfile::tempdir().unwrap();
    let request = json!({"model":"fixture-model","store":false,"input":[],"tools":[],"reasoning":{"effort":"low"}});
    let body = vec![b'x'; 80];
    let mut bounded = profile();
    bounded.max_attempt_generated_utf8_bytes = 100;
    let seen = exercise_live_gateway_fixture(
        dir.path(),
        bounded,
        vec![request.clone(), request.clone(), request],
        vec![
            (200, "text/event-stream".into(), body.clone()),
            (200, "text/event-stream".into(), body),
        ],
    )
    .await
    .unwrap();
    assert_eq!(seen.requests_sent, 2);
    assert_eq!(seen.generated_bytes, 160);
    assert_eq!(seen.errors[1].as_deref(), Some("generated_utf8_limit"));
    assert!(seen.errors[2].is_some());
    assert!(seen.uncertain);
}

#[test]
fn runtime_fix_qualification_provider_and_host_drift_rejects() {
    let bindings = json!([{"provider_ref":"p","model":"m","origin":"https://provider.invalid","credential_env":"TEST_SECRET","supported_efforts":["low"]}]);
    // A contract fragment carries no verdict and cannot qualify a runtime.
    let report = json!({"provider_bindings_hash":canonical_hash(&bindings).unwrap(),"profile":{"os_name":"linux","os_version":"debian-12"},"host_kernel_release":"6.fixture","host_arch":std::env::consts::ARCH});
    verify_runtime_contract_fixture(&report, &bindings, "debian-12", "6.fixture").unwrap();
    let mut changed = bindings.clone();
    changed[0]["model"] = json!("other-model");
    assert_eq!(
        verify_runtime_contract_fixture(&report, &changed, "debian-12", "6.fixture")
            .unwrap_err()
            .details
            .reason
            .as_deref(),
        Some("provider_bindings_changed")
    );
    let mut changed = bindings.clone();
    changed[0]["origin"] = json!("https://other.invalid");
    assert!(verify_runtime_contract_fixture(&report, &changed, "debian-12", "6.fixture").is_err());
    assert!(verify_runtime_contract_fixture(&report, &bindings, "debian-13", "6.fixture").is_err());
    assert!(verify_runtime_contract_fixture(&report, &bindings, "debian-12", "6.other").is_err());
}

#[tokio::test]
async fn runtime_fix_gateway_enforces_output_reserve_and_accounts_outgoing_body() {
    let dir = tempfile::tempdir().unwrap();
    let request = json!({"model":"fixture-model","store":false,"input":[],"tools":[],"reasoning":{"effort":"low"}});
    let mut requests = Vec::new();
    for limit in [json!(0), json!(-1), json!(1.5), json!(8193), json!("8192")] {
        let mut invalid = request.clone();
        invalid["max_output_tokens"] = limit;
        requests.push(invalid);
    }
    requests.push(request.clone());
    let response = br#"{"status":"completed","output":[]}"#.to_vec();
    let result = exercise_live_gateway_fixture(
        dir.path(),
        profile(),
        requests,
        vec![(200, "application/json".into(), response)],
    )
    .await
    .unwrap();
    assert_eq!(result.requests_sent, 1);
    assert_eq!(
        result.errors[..5],
        [
            Some("generation_reserve_exceeded".into()),
            Some("generation_reserve_exceeded".into()),
            Some("float_rejected".into()),
            Some("generation_reserve_exceeded".into()),
            Some("generation_reserve_exceeded".into()),
        ]
    );
    assert!(result.errors[5].is_none());
    assert_eq!(result.forwarded_bodies[0]["max_output_tokens"], 8192);
    let mut bounded = profile();
    bounded.max_request_body_bytes = canonical_bytes(&request).unwrap().len() as u64;
    let result = exercise_live_gateway_fixture(dir.path(), bounded, vec![request], Vec::new())
        .await
        .unwrap();
    assert_eq!(result.requests_sent, 0);
    assert_eq!(result.errors[0].as_deref(), Some("request_body_limit"));
}

#[tokio::test]
async fn runtime_fix_gateway_rejects_selected_effort_drift() {
    let dir = tempfile::tempdir().unwrap();
    let base = json!({"model":"fixture-model","store":false,"input":[],"tools":[]});
    let mut high = base.clone();
    high["reasoning"] = json!({"effort":"high"});
    let result = exercise_live_gateway_fixture(dir.path(), profile(), vec![base, high], Vec::new())
        .await
        .unwrap();
    assert_eq!(result.requests_sent, 0);
    assert!(result
        .errors
        .iter()
        .all(|error| error.as_deref() == Some("reasoning_effort_changed")));
    let actual = json!({"configOptions":[{"id":"model","currentValue":"fixture-model"},{"id":"reasoning_effort","currentValue":"high"}]});
    verify_confirmed_option_fixture(&actual, "model", "fixture-model").unwrap();
    assert!(verify_confirmed_option_fixture(&actual, "reasoning_effort", "low").is_err());
    assert!(verify_confirmed_option_fixture(&json!({}), "model", "fixture-model").is_err());
}

#[tokio::test]
async fn runtime_fix_retired_owner_reenters_real_reaper_without_passed_report() {
    let dir = tempfile::tempdir().unwrap();
    let incarnation = uuid::Uuid::new_v4().to_string().parse().unwrap();
    let runtime = retired_live_executor_fixture(dir.path().into(), incarnation).unwrap();
    // The marker permits an identity lookup, never a synthetic cleanup proof.
    let error = runtime
        .cancel_and_reap(RuntimeIdentity {
            incarnation,
            pid: 0,
        })
        .await
        .unwrap_err();
    #[cfg(not(target_os = "linux"))]
    assert_eq!(
        error.details.reason.as_deref(),
        Some("policy_unenforceable")
    );
    #[cfg(target_os = "linux")]
    assert_eq!(error.details.reason.as_deref(), Some("cgroup_unproven"));
    assert!(!dir.path().join("roundtable/execution-policy.json").exists());
}

#[tokio::test]
async fn runtime_fix_diagnostic_eof_partial_secret_stays_redacted() {
    let mut capture = DiagnosticCapture::new(vec!["fixture-secret".into()]);
    capture.push("ordinary suffix");
    let (result, _) = capture.finish().await.unwrap();
    // The final 'fix' is a secret prefix, deliberately redacted on interruption.
    assert_eq!(result.text, "ordinary suf[redacted]");
}

#[tokio::test]
async fn runtime_fix_queued_gateway_rechecks_lease_and_gate_before_send() {
    for (expire, expected) in [(true, "lease_expired"), (false, "policy_unreadable")] {
        let dir = tempfile::tempdir().unwrap();
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            exercise_queued_gateway_fixture(dir.path(), profile(), expire),
        )
        .await
        .expect("queued gateway must finish")
        .unwrap();
        assert_eq!(result.requests_sent, 1);
        assert_eq!(result.errors[1].as_deref(), Some(expected));
    }
}

/// Interrupt after the scheduler accepted one real scoped submission. The next
/// invocation blocks once so pause/resume runs through production service code.
struct PausableExecutor {
    inner: ControlledExecutor,
    second_started: tokio::sync::Notify,
    pause_once: std::sync::atomic::AtomicBool,
}
impl TokenBound for PausableExecutor {
    fn upper_bound(&self, bytes: &[u8]) -> RtResult<u64> {
        self.inner.upper_bound(bytes)
    }
    fn capacity_tokens(&self) -> Option<u64> {
        self.inner.capacity_tokens()
    }
}
#[async_trait]
impl RoundtableTurnExecutor for PausableExecutor {
    async fn capability(&self, config: &RoundtableConfigV1) -> RtResult<RuntimeCapability> {
        self.inner.capability(config).await
    }
    fn token_bound(&self) -> &(dyn TokenBound + Send + Sync) {
        self
    }
    async fn cancel_and_reap(&self, identity: RuntimeIdentity) -> RtResult<CleanupProof> {
        self.inner.cancel_and_reap(identity).await
    }
    async fn execute_turn(
        &self,
        request: RoundtableTurnRequest,
    ) -> RtResult<RoundtableTurnOutcome> {
        let is_second = self.inner.requests.lock().unwrap().len() == 1;
        if is_second
            && self
                .pause_once
                .swap(false, std::sync::atomic::Ordering::AcqRel)
        {
            self.second_started.notify_one();
            std::future::pending::<()>().await;
        }
        self.inner.execute_turn(request).await
    }
}

#[tokio::test]
async fn runtime_fix_pause_resume_preserves_real_accepted_turn_without_relaunch() {
    let (dir, conn) = support::open_pool(1).await;
    migrate_roundtable(&conn).await.unwrap();
    let store = open_roundtable_store(conn.clone()).await.unwrap();
    let executor = Arc::new(PausableExecutor {
        inner: ControlledExecutor {
            root: dir.path().into(),
            prompts: Mutex::new(Vec::new()),
            requests: Mutex::new(Vec::new()),
        },
        second_started: tokio::sync::Notify::new(),
        pause_once: std::sync::atomic::AtomicBool::new(true),
    });
    let runtime = Arc::new(OwnedParticipantRuntime::with_executor(
        dir.path().into(),
        Arc::new(ConnectionManager::new()),
        executor.clone(),
    ));
    let service = RoundtableService::open(
        ServiceConfig {
            data_dir: dir.path().into(),
            db_path: dir.path().join("roundtable.db"),
            db_identity: DbIdentity::new("accepted-pause-resume").unwrap(),
            discover: None,
        },
        store,
        runtime,
    )
    .await
    .unwrap();
    let actor = ActorContext::from_trusted_entry(
        "00000000-0000-4000-8000-000000000001".parse().unwrap(),
        OperatorScope::SingleOperator,
        ClientIdentity {
            kind: ClientKind::Web,
            session_ref: "accepted-resume".into(),
        },
    );
    let created = service
        .execute_fake_command(
            &actor,
            "roundtable_create",
            json!({"request_id":uuid::Uuid::new_v4().to_string(),"config":config()}),
        )
        .await
        .unwrap();
    let room = created["room_id"].clone();
    conn.execute_unprepared("UPDATE rt_rooms SET status='paused'")
        .await
        .unwrap();
    service
        .execute_fake_command(
            &actor,
            "roundtable_resume",
            json!({
                "room_id":room,"request_id":uuid::Uuid::new_v4().to_string(),
                "expected_revision":created["revision"],"recovery_consent":true,
            }),
        )
        .await
        .unwrap();
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        executor.second_started.notified(),
    )
    .await
    .expect("second member starts after first acceptance");
    assert_eq!(
        support::scalar_i64(&conn, "SELECT COUNT(*) FROM rt_turns WHERE status='valid'").await,
        1
    );
    assert_eq!(
        support::scalar_i64(
            &conn,
            "SELECT COUNT(*) FROM rt_attempts WHERE state='accepted'"
        )
        .await,
        1
    );
    let accepted = executor.inner.requests.lock().unwrap()[0].clone();
    let revision = support::scalar_i64(&conn, "SELECT revision FROM rt_rooms")
        .await
        .to_string();
    service.execute_fake_command(&actor, "roundtable_pause", json!({
        "room_id":room,"request_id":uuid::Uuid::new_v4().to_string(),"expected_revision":revision,
        "reason":"retained acceptance regression",
    })).await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while support::scalar_text(&conn, "SELECT status FROM rt_rooms").await != "paused" {
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("pause cleanup completes");
    assert_eq!(
        support::scalar_i64(&conn, "SELECT COUNT(*) FROM rt_turns WHERE status='valid'").await,
        1
    );
    let revision = support::scalar_i64(&conn, "SELECT revision FROM rt_rooms")
        .await
        .to_string();
    service
        .execute_fake_command(
            &actor,
            "roundtable_resume",
            json!({
                "room_id":room,"request_id":uuid::Uuid::new_v4().to_string(),
                "expected_revision":revision,"recovery_consent":true,
            }),
        )
        .await
        .expect("accepted turns must not consume a new first-launch reservation");
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while support::scalar_text(&conn, "SELECT status FROM rt_rooms").await != "completed" {
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("remaining member and moderator complete");
    {
        let requests = executor.inner.requests.lock().unwrap();
        assert_eq!(requests.len(), 3);
        assert_eq!(
            requests
                .iter()
                .filter(|request| request.phase.phase_id == accepted.phase.phase_id
                    && request.speaker_id == accepted.speaker_id)
                .count(),
            1,
            "accepted member must never be relaunched after pause/resume"
        );
    }
    assert_eq!(
        support::scalar_i64(&conn, "SELECT COUNT(*) FROM rt_messages").await,
        3
    );
}

#[tokio::test]
async fn runtime_fix_gateway_shutdown_drains_idle_headers_and_slow_body_connections() {
    let dir = tempfile::tempdir().unwrap();
    let (drained, clients_closed) = exercise_gateway_shutdown_fixture(dir.path(), profile())
        .await
        .unwrap();
    assert!(
        drained,
        "shutdown must cancel accepted IO before waiting for graceful completion"
    );
    assert!(
        clients_closed,
        "all accepted sockets must close before cleanup is proven"
    );
}
