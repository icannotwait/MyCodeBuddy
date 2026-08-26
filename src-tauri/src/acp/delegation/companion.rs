//! Companion-side MCP protocol — the bits that live inside the `codeg-mcp`
//! binary but are factored out into the library so they can be unit-tested
//! without spawning the binary.
//!
//! The companion speaks newline-delimited JSON-RPC 2.0 on stdio:
//! one request → one response per line, with concurrent dispatch so
//! `notifications/cancelled` can race an in-flight `tools/call`. It exposes up
//! tools — `delegate_to_agent` (async; returns a `task_id` ack),
//! `get_delegation_status` (poll/long-poll for the result), `cancel_delegation`,
//! `check_user_feedback` (pull the user's mid-turn steering notes),
//! `ask_user_question` (block on a multiple-choice card), `get_session_info`
//! (resolve a referenced session by id), coordination-only
//! `request_parent_decision` / `reply_to_delegation`, plus Root-only
//! `register_simple_workflow` locator registration. Retired `workflow_v2` and
//! `completion_v2` schemas and dispatch handlers remain embedded for protocol
//! compatibility tests, but stale launch tokens cannot add them to a new
//! companion catalog.
//! Only `delegate_to_agent` registers a broker-side cancel handle; canceling a
//! status / cancel / feedback / session / decision round-trip merely suppresses
//! its response — and for `check_user_feedback` also skips the delivery commit,
//! so a cancelled note stays pending.
//!
//! Notifications (id = None) produce no response, matching MCP's expectation
//! that `notifications/initialized` etc. are fire-and-forget.
//!
//! Cancellation flow per the MCP 2024-11-05 / 2025-11-25 cancellation utility:
//!
//! 1. Companion receives `tools/call` with JSON-RPC `id = X`, mints an opaque
//!    `external_handle`, registers `X → (handle, cancel_tx)` in
//!    [`InflightCalls`], and kicks off the broker round-trip.
//! 2. If `notifications/cancelled` for `requestId = X` arrives, the
//!    notification handler pops the entry, fires `cancel_tx`, and sends a
//!    `BrokerMessage::Cancel { external_handle }` to the broker.
//! 3. The `tools/call` task observes `cancel_tx`, abandons its UDS read,
//!    and returns `None` — the binary suppresses the response per spec.
//! 4. If the round-trip completes before the cancel arrives, the entry is
//!    removed normally and the response goes out on stdout; a late cancel
//!    notification finds nothing and is silently ignored.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant, SystemTime};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::sync::{oneshot, Mutex};

use crate::acp::delegation::attention::ATTENTION_PAYLOAD_MAX_BYTES;
use crate::acp::delegation::metrics::{ArtifactExportOutcome, DelegationMetrics};
use crate::acp::delegation::transport::{
    client_ask_round_trip, client_cancel, client_cancel_task_round_trip, client_commit_feedback,
    client_complete_work_round_trip, client_feedback_round_trip,
    client_get_workflow_state_round_trip, client_orchestration_bindings_round_trip,
    client_parent_decision_round_trip, client_publish_workflow_round_trip,
    client_recover_workflow_round_trip, client_recovery_authorization_round_trip,
    client_register_simple_workflow_round_trip, client_reply_delegation_round_trip,
    client_round_trip, client_session_round_trip, client_settle_workflow_round_trip,
    client_status_round_trip, BrokerAskRequest, BrokerCancelRequest, BrokerCancelTaskRequest,
    BrokerCommitFeedbackRequest, BrokerCompleteWorkRequest, BrokerFeedbackRequest,
    BrokerGetWorkflowStateRequest, BrokerOrchestrationBindingsRequest, BrokerParentDecisionRequest,
    BrokerPublishWorkflowRequest, BrokerRecoverWorkflowRequest, BrokerRecoveryAuthorizationRequest,
    BrokerRegisterSimpleWorkflowRequest, BrokerReplyDelegationRequest, BrokerRequest,
    BrokerResponse, BrokerSessionRequest, BrokerSettleWorkflowRequest, BrokerStatusRequest,
    CancelDelegationReason, CompanionRole,
};
use crate::acp::delegation::types::{
    validate_correlation_id, BindingEvidenceV1, DelegationOrchestrationBindingPage,
    DelegationReturnWhen, OrchestrationBindingArtifactDescriptor, OrchestrationBindingQueryError,
    OrchestrationBindingQueryRequest,
};
use crate::acp::delegation::workflow::{
    CompleteWorkRequest, WorkflowIndexOmissionStep, WorkflowStateIndexDto,
    COMPLETE_WORK_REPORT_FILE_MAX_BYTES, COMPLETE_WORK_SUMMARY_MAX_BYTES,
    WORKFLOW_CAPABILITY_VERSION,
};
use crate::acp::question::parse_questions;
use crate::acp::recovery_authorization::RecoverySubjectKind;
use crate::acp::session_info::MAX_SESSION_MESSAGES;

/// Upper bound on one broker-side cancel round-trip. Bounds both
/// `handle_cancel_notification` (so stdin dispatch can't stall behind a
/// stuck UDS connect/read) and the shutdown-drain loop (so an
/// unresponsive listener can't keep the EOF / watchdog path hung). 500 ms
/// is generous for a same-host UDS exchange and short enough that a user
/// won't notice the bound being hit. Misses are absorbed by the codeg
/// main side's `cancel_by_parent` cascade when the parent ACP connection
/// eventually ends.
const BROKER_CANCEL_BUDGET: Duration = Duration::from_millis(500);

/// Wrap `client_cancel` in [`BROKER_CANCEL_BUDGET`] so callers can fire
/// a synchronous cancel without worrying about a hung listener freezing
/// them. Both success, transport error, and timeout collapse to `()` —
/// callers couldn't usefully react anyway, and the broker has independent
/// cancel backstops (parent / child disconnect cascades) if this one
/// misses.
async fn send_broker_cancel(socket_path: &str, req: &BrokerCancelRequest) {
    let _ = tokio::time::timeout(BROKER_CANCEL_BUDGET, client_cancel(socket_path, req)).await;
}

#[cfg(test)]
mod orchestration_binding_artifact_storage_tests {
    use std::collections::BTreeSet;
    use std::fs;
    use std::path::Path;
    use std::sync::Arc;
    use std::time::{Duration, SystemTime};

    use chrono::{TimeZone, Utc};
    use serde_json::json;
    use sha2::Digest;

    use super::*;
    use crate::acp::delegation::metrics::DelegationMetrics;
    use crate::acp::delegation::transport::CompanionRole;
    use crate::acp::delegation::types::{BindingEvidenceV1, DelegationOrchestrationBindingRun};

    const INCARNATION_A: &str = "00000000-0000-4000-8000-000000000001";
    #[cfg(unix)]
    const INCARNATION_B: &str = "00000000-0000-4000-8000-000000000002";

    fn run(task_id: impl Into<String>) -> DelegationOrchestrationBindingRun {
        let task_id = task_id.into();
        DelegationOrchestrationBindingRun {
            root_task_id: task_id.clone(),
            lineage_root_task_id: task_id.clone(),
            task_id,
            previous_task_id: None,
            replaced_task_id: None,
            replacement_reason: None,
            generic_generation: 1,
            work_unit_key: Some("task:1".into()),
            child_conversation_id: 1,
            agent_type: "grok".into(),
            profile_id: None,
            status: "running".into(),
            orchestration_binding: None,
        }
    }

    fn page(runs: Vec<DelegationOrchestrationBindingRun>) -> DelegationOrchestrationBindingPage {
        DelegationOrchestrationBindingPage {
            schema_version: 1,
            namespace: "brainstorm-to-delivery".into(),
            snapshot_id: "1a641e16-36f4-4ec5-aa4f-18d18e6ab107".into(),
            snapshot_revision: "42".into(),
            snapshot_created_at: Utc.with_ymd_and_hms(2026, 8, 26, 8, 0, 0).unwrap(),
            snapshot_expires_at: Utc.with_ymd_and_hms(2026, 8, 26, 8, 1, 0).unwrap(),
            total_rows: runs.len() as u64,
            page_start: 0,
            request_cursor: None,
            runs,
            next_cursor: None,
            complete: true,
        }
    }

    fn two_pages() -> Vec<DelegationOrchestrationBindingPage> {
        let mut first = page(vec![run("task-1")]);
        first.total_rows = 2;
        first.next_cursor = Some("cursor-a".into());
        first.complete = false;

        let mut second = page(vec![run("task-2")]);
        second.total_rows = 2;
        second.page_start = 1;
        second.request_cursor = Some("cursor-a".into());
        vec![first, second]
    }

    fn exact_size_page(target_bytes: usize) -> DelegationOrchestrationBindingPage {
        let mut page = page(vec![run("x")]);
        let current = serde_json::to_vec(&BindingEvidenceV1 {
            schema_version: 1,
            pages: vec![page.clone()],
        })
        .unwrap()
        .len();
        page.runs[0].task_id = "x".repeat(1 + target_bytes - current);
        assert_eq!(
            serde_json::to_vec(&BindingEvidenceV1 {
                schema_version: 1,
                pages: vec![page.clone()],
            })
            .unwrap()
            .len(),
            target_bytes
        );
        page
    }

    fn context() -> CompanionContext {
        CompanionContext {
            parent_connection_id: "parent".into(),
            socket_path: "unused".into(),
            token: "token".into(),
            features: CompanionFeatures {
                delegation: true,
                coordination_v1: true,
                feedback: false,
                ask: false,
                sessions: false,
                workflow_v2: false,
                completion_v2: false,
            },
            role: CompanionRole::Root,
            can_spawn_child: true,
            connection_incarnation_id: INCARNATION_A.into(),
            disabled_agents: Vec::new(),
        }
    }

    fn json_files(root: &Path) -> Vec<std::path::PathBuf> {
        let mut files = Vec::new();
        if !root.exists() {
            return files;
        }
        for directory in fs::read_dir(root).unwrap() {
            let directory = directory.unwrap().path();
            if !directory.is_dir() {
                continue;
            }
            files.extend(
                fs::read_dir(directory)
                    .unwrap()
                    .filter_map(Result::ok)
                    .map(|entry| entry.path())
                    .filter(|path| {
                        path.extension()
                            .is_some_and(|extension| extension == "json")
                    }),
            );
        }
        files
    }

    #[test]
    fn orchestration_binding_artifact_storage_accepts_empty_one_and_4096_rows() {
        for rows in [0, 1, ORCHESTRATION_BINDING_ARTIFACT_MAX_ROWS] {
            let runs = (0..rows)
                .map(|index| run(format!("task-{index}")))
                .collect();
            let prepared = prepare_binding_evidence(vec![page(runs)]).unwrap();
            assert_eq!(prepared.total_rows, rows);
            let parsed: BindingEvidenceV1 = serde_json::from_slice(&prepared.bytes).unwrap();
            assert_eq!(parsed.schema_version, 1);
            assert_eq!(parsed.pages.len(), 1);
            assert_eq!(parsed.pages[0].runs.len(), rows);
        }
    }

    #[test]
    fn orchestration_binding_artifact_storage_rejects_4097_rows() {
        let runs = (0..=ORCHESTRATION_BINDING_ARTIFACT_MAX_ROWS)
            .map(|index| run(format!("task-{index}")))
            .collect();
        assert_eq!(
            prepare_binding_evidence(vec![page(runs)]).unwrap_err(),
            OrchestrationBindingQueryError::TooLarge
        );
    }

    #[test]
    fn orchestration_binding_artifact_storage_accepts_exactly_4_mib_and_rejects_one_more_byte() {
        let exact = prepare_binding_evidence(vec![exact_size_page(
            ORCHESTRATION_BINDING_ARTIFACT_MAX_BYTES,
        )])
        .unwrap();
        assert_eq!(exact.bytes.len(), ORCHESTRATION_BINDING_ARTIFACT_MAX_BYTES);

        assert_eq!(
            prepare_binding_evidence(vec![exact_size_page(
                ORCHESTRATION_BINDING_ARTIFACT_MAX_BYTES + 1,
            )])
            .unwrap_err(),
            OrchestrationBindingQueryError::ArtifactTooLarge
        );
    }

    #[test]
    fn orchestration_binding_artifact_storage_rejects_invalid_page_chains() {
        let mut cases = Vec::new();

        let mut duplicate = two_pages();
        duplicate[1].runs[0].task_id = duplicate[0].runs[0].task_id.clone();
        cases.push(("duplicate task id", duplicate));

        let mut mixed_metadata = two_pages();
        mixed_metadata[1].snapshot_revision = "43".into();
        cases.push(("mixed snapshot metadata", mixed_metadata));

        let mut wrong_first_start = two_pages();
        wrong_first_start[0].page_start = 1;
        cases.push(("wrong first page start", wrong_first_start));

        let mut wrong_first_cursor = two_pages();
        wrong_first_cursor[0].request_cursor = Some("cursor-before-first".into());
        cases.push(("wrong first cursor", wrong_first_cursor));

        let mut wrong_cursor_echo = two_pages();
        wrong_cursor_echo[1].request_cursor = Some("cursor-b".into());
        cases.push(("wrong cursor echo", wrong_cursor_echo));

        let mut gap = two_pages();
        gap[1].page_start = 2;
        cases.push(("gap", gap));

        let mut reordered = two_pages();
        reordered.swap(0, 1);
        cases.push(("reordered pages", reordered));

        let mut trailing = two_pages();
        let mut extra = page(Vec::new());
        extra.total_rows = 2;
        extra.page_start = 2;
        trailing.push(extra);
        cases.push(("trailing page", trailing));

        let mut missing_completion = two_pages();
        missing_completion[1].complete = false;
        missing_completion[1].next_cursor = Some("cursor-c".into());
        cases.push(("missing completion", missing_completion));

        let mut changed_total = two_pages();
        changed_total[1].total_rows = 3;
        cases.push(("changed total count", changed_total));

        for (name, pages) in cases {
            assert_eq!(
                prepare_binding_evidence(pages).unwrap_err(),
                OrchestrationBindingQueryError::Invalid,
                "{name} must fail closed"
            );
        }
    }

    #[test]
    fn orchestration_binding_artifact_storage_uses_random_atomic_private_files() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("codeg-mcp/orchestration-bindings");
        let inflight = Arc::new(InflightCalls::new());

        let (first, first_pending) = store_binding_artifact_at(
            &root,
            INCARNATION_A,
            vec![page(vec![run("task-a")])],
            &inflight,
        )
        .unwrap();
        let (second, second_pending) = store_binding_artifact_at(
            &root,
            INCARNATION_A,
            vec![page(vec![run("task-b")])],
            &inflight,
        )
        .unwrap();

        assert_ne!(first.artifact_path, second.artifact_path);
        for descriptor in [&first, &second] {
            let path = Path::new(&descriptor.artifact_path);
            let stem = path.file_stem().unwrap().to_str().unwrap();
            assert_eq!(uuid::Uuid::parse_str(stem).unwrap().to_string(), stem);
            assert_eq!(path.extension().unwrap(), "json");
            assert!(!descriptor.artifact_path.contains("brainstorm-to-delivery"));
            let bytes = fs::read(path).unwrap();
            assert_eq!(bytes.len() as u64, descriptor.artifact_bytes);
            assert_eq!(
                descriptor.artifact_sha256,
                format!("sha256:{:x}", sha2::Sha256::digest(&bytes))
            );
            serde_json::from_slice::<BindingEvidenceV1>(&bytes).unwrap();
        }
        assert_eq!(
            json_files(&root).len(),
            2,
            "no sibling partial files remain"
        );

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            let owner = Path::new(&first.artifact_path).parent().unwrap();
            assert_eq!(
                fs::metadata(owner).unwrap().permissions().mode() & 0o777,
                0o700
            );
            assert_eq!(
                fs::metadata(&first.artifact_path)
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }

        first_pending.mark_delivered();
        second_pending.mark_delivered();
    }

    #[test]
    fn orchestration_binding_artifact_storage_sweeps_only_verified_stale_root_files() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("codeg-mcp/orchestration-bindings");
        let owner = root.join(INCARNATION_A);
        fs::create_dir_all(&owner).unwrap();
        let stale = owner.join("00000000-0000-4000-8000-000000000010.json");
        let fresh = owner.join("00000000-0000-4000-8000-000000000011.json");
        let partial = owner.join(".codeg-binding-crash-partial");
        let sibling = temp.path().join("must-survive.json");
        fs::write(&stale, b"stale").unwrap();
        fs::write(&fresh, b"fresh").unwrap();
        fs::write(&partial, b"partial").unwrap();
        fs::write(&sibling, b"outside").unwrap();

        let metrics = DelegationMetrics::default();
        sweep_stale_binding_artifacts_at(
            &root,
            SystemTime::now() + ORCHESTRATION_BINDING_ARTIFACT_STALE_AGE + Duration::from_secs(1),
            &metrics,
        )
        .unwrap();

        assert!(!stale.exists());
        assert!(!fresh.exists());
        assert!(!partial.exists());
        assert!(
            sibling.exists(),
            "sweep must remain under the verified root"
        );
        assert_eq!(metrics.snapshot().artifact_cleanup_success_count, 3);
    }

    #[cfg(unix)]
    #[test]
    fn orchestration_binding_artifact_storage_skips_symlinked_owner_outside_verified_root() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("codeg-mcp/orchestration-bindings");
        let outside = temp.path().join("outside");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(&outside).unwrap();
        let outside_file = outside.join("00000000-0000-4000-8000-000000000012.json");
        fs::write(&outside_file, b"outside").unwrap();
        symlink(&outside, root.join(INCARNATION_B)).unwrap();

        sweep_stale_binding_artifacts_at(
            &root,
            SystemTime::now() + ORCHESTRATION_BINDING_ARTIFACT_STALE_AGE + Duration::from_secs(1),
            &DelegationMetrics::default(),
        )
        .unwrap();

        assert!(outside_file.exists());
    }

    #[test]
    fn orchestration_binding_artifact_storage_cleans_unpublished_partial_on_persist_failure() {
        let temp = tempfile::tempdir().unwrap();
        let owner = temp.path().join(INCARNATION_A);
        fs::create_dir_all(&owner).unwrap();
        let final_path = owner.join("00000000-0000-4000-8000-000000000020.json");
        fs::write(&final_path, b"existing").unwrap();

        assert!(atomic_publish_binding_artifact(&owner, &final_path, b"replacement").is_err());
        let files = fs::read_dir(&owner)
            .unwrap()
            .filter_map(Result::ok)
            .collect::<Vec<_>>();
        assert_eq!(files.len(), 1);
        assert_eq!(fs::read(final_path).unwrap(), b"existing");
    }

    #[test]
    fn orchestration_binding_artifact_storage_cancellation_removes_published_pending_file() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("codeg-mcp/orchestration-bindings");
        let inflight = Arc::new(InflightCalls::new());
        let (descriptor, pending) = store_binding_artifact_at(
            &root,
            INCARNATION_A,
            vec![page(vec![run("task-a")])],
            &inflight,
        )
        .unwrap();

        assert!(Path::new(&descriptor.artifact_path).exists());
        assert_eq!(inflight.binding_artifact_count(), 1);
        drop(pending);
        assert!(!Path::new(&descriptor.artifact_path).exists());
        assert_eq!(inflight.binding_artifact_count(), 0);
    }

    #[tokio::test]
    async fn orchestration_binding_artifact_storage_shutdown_cleans_delivered_files() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("codeg-mcp/orchestration-bindings");
        let inflight = Arc::new(InflightCalls::new());
        let (descriptor, pending) = store_binding_artifact_at(
            &root,
            INCARNATION_A,
            vec![page(vec![run("task-a")])],
            &inflight,
        )
        .unwrap();
        pending.set_final_result_bytes(1_024).after_relay().await;

        drain_and_cancel_all(&context(), &inflight, "test shutdown").await;

        assert!(!Path::new(&descriptor.artifact_path).exists());
        assert_eq!(inflight.binding_artifact_count(), 0);
    }

    #[tokio::test]
    async fn orchestration_binding_artifact_storage_pre_relay_shutdown_drains_registered_file() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("codeg-mcp/orchestration-bindings");
        let inflight = Arc::new(InflightCalls::new());
        let (descriptor, pending) = store_binding_artifact_at(
            &root,
            INCARNATION_A,
            vec![page(vec![run("task-a")])],
            &inflight,
        )
        .unwrap();
        let relay_result = SpawnResult {
            response: Some(ok(json!(1), json!(descriptor.clone()))),
            after_relay: Some(pending.after_relay()),
        };

        drain_and_cancel_all(&context(), &inflight, "test hard exit").await;

        assert!(!Path::new(&descriptor.artifact_path).exists());
        assert_eq!(inflight.binding_artifact_count(), 0);
        assert!(
            relay_result.after_relay.is_some(),
            "drain must not depend on running or dropping the relay callback"
        );
    }

    #[test]
    fn orchestration_binding_artifact_storage_metrics_are_identifier_free() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("codeg-mcp/orchestration-bindings");
        let metrics = Arc::new(DelegationMetrics::default());
        let inflight = Arc::new(InflightCalls::with_artifact_metrics(metrics.clone()));
        let sensitive_task = "task-sensitive";
        let sensitive_incarnation = "00000000-0000-4000-8000-000000004123";

        let (descriptor, pending) = store_binding_artifact_at(
            &root,
            sensitive_incarnation,
            vec![page(vec![run(sensitive_task)])],
            &inflight,
        )
        .unwrap();
        drop(pending.set_final_result_bytes(1_536));
        let oversized = store_binding_artifact_at(
            &root,
            sensitive_incarnation,
            vec![exact_size_page(
                ORCHESTRATION_BINDING_ARTIFACT_MAX_BYTES + 1,
            )],
            &inflight,
        )
        .err()
        .expect("oversized artifact must fail");
        assert_eq!(oversized, OrchestrationBindingQueryError::ArtifactTooLarge);
        inflight.record_binding_artifact_stale_restart();

        let snapshot = metrics.snapshot();
        assert_eq!(snapshot.artifact_cleanup_success_count, 1);
        assert_eq!(snapshot.artifact_export_outcomes["cancelled"], 1);
        assert_eq!(snapshot.artifact_export_outcomes["too_large"], 1);
        assert_eq!(snapshot.artifact_transparent_stale_restarts, 1);
        assert_eq!(
            snapshot
                .artifact_internal_page_count_buckets
                .iter()
                .sum::<u64>(),
            1
        );
        assert_eq!(
            snapshot
                .artifact_selected_row_count_buckets
                .iter()
                .sum::<u64>(),
            1
        );
        assert_eq!(
            snapshot.artifact_evidence_bytes_buckets.iter().sum::<u64>(),
            1
        );
        assert_eq!(
            snapshot
                .artifact_export_duration_ms_buckets
                .iter()
                .sum::<u64>(),
            1
        );
        assert_eq!(
            snapshot
                .artifact_final_mcp_result_bytes_buckets
                .iter()
                .sum::<u64>(),
            1
        );
        let serialized = serde_json::to_string(&snapshot).unwrap();
        for sensitive in [
            sensitive_task,
            sensitive_incarnation,
            descriptor.artifact_path.as_str(),
            descriptor.artifact_sha256.as_str(),
        ] {
            assert!(!serialized.contains(sensitive));
        }
        assert_eq!(
            snapshot
                .artifact_export_outcomes
                .keys()
                .map(String::as_str)
                .collect::<BTreeSet<_>>(),
            BTreeSet::from([
                "broker_failed",
                "cancelled",
                "io_failed",
                "result_too_large",
                "serialization_failed",
                "stale",
                "success",
                "too_large",
            ])
        );
    }
}

/// Static MCP tool schema. Lives next to this module so codeg-mcp ships
/// a single embedded copy — no runtime file IO, no version skew with the
/// broker's [`super::types::DelegationRequest`].
pub const TOOL_SCHEMA_JSON: &str = include_str!("tool_schema.json");

pub const GET_WORKFLOW_STATE_MAX_RESULT_BYTES: usize = 7_680;
pub const GET_WORKFLOW_STATE_MAX_REQUEST_ID_BYTES: usize = 256;

pub const GET_ORCHESTRATION_BINDINGS_MAX_RESULT_BYTES: usize = 7_680;
pub const GET_ORCHESTRATION_BINDINGS_MAX_REQUEST_ID_BYTES: usize = 256;

pub const ORCHESTRATION_BINDING_ARTIFACT_MAX_ROWS: usize = 4_096;
pub const ORCHESTRATION_BINDING_ARTIFACT_MAX_BYTES: usize = 4 * 1024 * 1024;
const ORCHESTRATION_BINDING_ARTIFACT_FORMAT: &str = "codeg-binding-evidence-v1";
const ORCHESTRATION_BINDING_ARTIFACT_STALE_AGE: Duration = Duration::from_secs(10 * 60);

/// Grok stdio host splits JSONL at 8,192 bytes without reassembly. Keep the
/// same 512-byte headroom used by `tools/list` / `get_workflow_state`.
pub const GET_SESSION_INFO_MAX_RESULT_BYTES: usize = 7_680;
/// Same ceiling as workflow: oversized request ids are rejected pre-inflight
/// so they cannot consume the entire response budget.
pub const GET_SESSION_INFO_MAX_REQUEST_ID_BYTES: usize = 256;

const SESSION_INFO_TRANSPORT_NOTE: &str =
    "Session content was omitted to satisfy the 7680-byte stdio transport budget.";
const SESSION_INFO_METADATA_NOTE: &str =
    "Session metadata was omitted to satisfy the 7680-byte stdio transport budget.";

/// Pre-coordination `delegate_to_agent` description restored when
/// `coordination_v1` is off so old connections never see Join instructions.
pub const LEGACY_DELEGATE_DESCRIPTION: &str = "Start an independent local sub-agent for a self-contained task. ASYNCHRONOUS: returns task_id immediately; collect it later with get_delegation_status. The child starts cold and cannot see this conversation, open files, or earlier turns, so task must include all context. Fan out work before collecting results. For each distinct codeg://delegation-profile/<uuid>, call once with its UUID as profile_id. Recover admission_failed or admission_unknown via explicit replacement (replaces_task_id + replacement_reason) only — never continue_delegation.";

/// Pre-coordination `get_delegation_status` description restored when
/// `coordination_v1` is off (also strips `return_when` from the schema).
pub const LEGACY_STATUS_DESCRIPTION: &str = "Get status or results for one or more task_ids from delegate_to_agent. Omit wait_ms for an immediate snapshot. A positive wait (max 60000 ms) returns on terminal, stalled, waiting_input, or its deadline. wait_ms=0 waits only for a terminal result without a timeout. A running result at a bounded deadline is not a failure. After stalled/waiting_input, surface or handle the condition, or use terminal wait when the result remains required. A wait returns when ANY requested task meets the mode condition, so call again for unfinished tasks. Returns {\"tasks\":[...]} in input order with each task_id, status (running, completed, failed, canceled, or unknown), observation fields while running when available, and final text when available. Prefer blocking waits to repeated polls. While only waiting, call again silently; message the user only for a terminal result or needed input.";

/// Pre-coordination `wait_ms` parameter description restored when
/// `coordination_v1` is off so legacy tools/list does not advertise rejection.
pub const LEGACY_WAIT_MS_DESCRIPTION: &str = "Omit wait_ms for an immediate snapshot. A positive wait (max 60000 ms) returns on terminal, stalled, waiting_input, or its deadline. wait_ms=0 waits only for a terminal result without a timeout.";

pub const COORDINATION_POSITIVE_WAIT_ERROR: &str =
    "positive wait_ms is unavailable with coordination_v1; retry with \
     return_when=\"all_terminal_or_attention\" and wait_ms=0";

#[derive(Debug, Deserialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: String,
    /// MCP notifications carry no `id`. We dispatch a response only when this
    /// is `Some`.
    pub id: Option<Value>,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct JsonRpcError {
    pub code: i64,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: String,
    pub id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RegisterSimpleWorkflowArguments {
    plan_rel_path: String,
    #[serde(default)]
    progress_rel_path: Option<String>,
}

pub fn serialize_jsonrpc_line(response: &JsonRpcResponse) -> Result<Vec<u8>, serde_json::Error> {
    let mut line = serde_json::to_vec(response)?;
    line.push(b'\n');
    Ok(line)
}

pub fn ok(id: Value, result: Value) -> JsonRpcResponse {
    JsonRpcResponse {
        jsonrpc: "2.0".into(),
        id,
        result: Some(result),
        error: None,
    }
}

pub fn err(id: Value, code: i64, message: impl Into<String>) -> JsonRpcResponse {
    JsonRpcResponse {
        jsonrpc: "2.0".into(),
        id,
        result: None,
        error: Some(JsonRpcError {
            code,
            message: message.into(),
            data: None,
        }),
    }
}

/// Which tool groups this companion exposes. One `codeg-mcp` process can carry
/// the delegation tools, the feedback tool, or both — gated independently so
/// each feature can be toggled in settings without the other. Passed in via the
/// `--features` arg at launch; a tool whose group is off is hidden from
/// `tools/list` and rejected on `tools/call`.
#[derive(Debug, Clone, Copy)]
pub struct CompanionFeatures {
    pub delegation: bool,
    /// Connection-bound Join capability. Only the literal `coordination_v1`
    /// feature token enables this; omitted `--features` stays legacy.
    pub coordination_v1: bool,
    pub feedback: bool,
    pub ask: bool,
    pub sessions: bool,
    /// Retired workflow_manifest_v2 feature bit. Production launch parsing
    /// leaves this false; historical protocol tests construct it explicitly.
    pub workflow_v2: bool,
    /// Retired child completion feature bit used only by historical tests.
    pub completion_v2: bool,
}

/// Canonical root tool set for `workflow_manifest_v2`.
pub const WORKFLOW_V2_TOOLS: &[&str] = &[
    "get_workflow_capabilities",
    "get_workflow_state",
    "recover_workflow",
    "publish_workflow_manifest",
    "settle_workflow_gate",
];

/// Capability catalog classification (B9).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkflowCapabilityMode {
    /// None of the five workflow tools present; workflow is unavailable.
    Unavailable,
    /// All five present → v2 mode (capability must also report true).
    WorkflowManifestV2,
    /// Partial tool set → inconsistent hard-block.
    Inconsistent,
}

/// Classify a workflow catalog. Missing and partial catalogs both fail closed.
pub fn classify_workflow_tool_catalog<'a, I>(tool_names: I) -> WorkflowCapabilityMode
where
    I: IntoIterator<Item = &'a str>,
{
    let mut mask = 0u8;
    for name in tool_names {
        if let Some(i) = WORKFLOW_V2_TOOLS.iter().position(|t| *t == name) {
            mask |= 1 << i;
        }
    }
    let all = (1u8 << WORKFLOW_V2_TOOLS.len()) - 1;
    match mask {
        0 => WorkflowCapabilityMode::Unavailable,
        m if m == all => WorkflowCapabilityMode::WorkflowManifestV2,
        _ => WorkflowCapabilityMode::Inconsistent,
    }
}

/// Local `get_workflow_capabilities` payload from launch features/role (A15.1).
pub fn local_workflow_capabilities(features: &CompanionFeatures, role: CompanionRole) -> Value {
    let enabled = features.workflow_v2 && role == CompanionRole::Root;
    let operations: Vec<&str> = if enabled {
        WORKFLOW_V2_TOOLS.to_vec()
    } else {
        Vec::new()
    };
    json!({
        "versions": {
            WORKFLOW_CAPABILITY_VERSION: enabled,
        },
        "operations": operations,
        "workflow_manifest_v2": enabled,
    })
}

impl CompanionFeatures {
    /// Parse the comma-joined `--features` value (e.g.
    /// `delegation,coordination_v1,feedback,ask,sessions,workflow_v2`). Unknown
    /// tokens are ignored. An absent value (`None`) defaults to
    /// delegation-only without Join or workflow — backward compatible with a
    /// parent that predates feature gating.
    pub fn parse(raw: Option<&str>) -> Self {
        let Some(s) = raw else {
            return Self {
                delegation: true,
                coordination_v1: false,
                feedback: false,
                ask: false,
                sessions: false,
                workflow_v2: false,
                completion_v2: false,
            };
        };
        let mut f = Self {
            delegation: false,
            coordination_v1: false,
            feedback: false,
            ask: false,
            sessions: false,
            workflow_v2: false,
            completion_v2: false,
        };
        for tok in s.split(',').map(str::trim).filter(|t| !t.is_empty()) {
            match tok {
                "delegation" => f.delegation = true,
                "coordination_v1" => f.coordination_v1 = true,
                "feedback" => f.feedback = true,
                "ask" => f.ask = true,
                "sessions" => f.sessions = true,
                "workflow_v2" | "completion_v2" => {}
                _ => {}
            }
        }
        f
    }

    /// Whether a pre-coordination MCP tool is exposed under the enabled groups.
    fn allows_legacy_tool(&self, name: &str) -> bool {
        match name {
            "check_user_feedback" => self.feedback,
            "ask_user_question" => self.ask,
            "get_session_info" => self.sessions,
            "delegate_to_agent"
            | "continue_delegation"
            | "get_delegation_status"
            | "cancel_delegation" => self.delegation,
            _ => false,
        }
    }

    /// Whether workflow_manifest_v2 tools are structurally enabled (feature bit).
    /// Role gating is applied separately in [`CompanionContext::allows_tool`].
    pub fn workflow_tools_enabled(&self) -> bool {
        self.workflow_v2
    }
}

/// Process arguments threaded through every `tools/call` so the dispatcher
/// can build a [`BrokerRequest`] without re-parsing argv per call.
#[derive(Debug, Clone)]
pub struct CompanionContext {
    pub parent_connection_id: String,
    pub socket_path: String,
    pub token: String,
    /// Tool groups this launch exposes (see [`CompanionFeatures`]).
    pub features: CompanionFeatures,
    /// Immutable launch role (`--role root|delegation_child`).
    pub role: CompanionRole,
    /// Launch-time depth snapshot. The broker remains the hard authority when
    /// settings change after launch; this only suppresses doomed child spawns.
    pub can_spawn_child: bool,
    /// Immutable ACP connection incarnation supplied by the launcher.
    pub connection_incarnation_id: String,
    /// Built-in agent slugs disabled in settings. These may only narrow the
    /// embedded closed enum; launch inputs never add new delegate targets.
    pub disabled_agents: Vec<String>,
}

impl CompanionContext {
    /// Whether the named MCP tool is exposed under this launch's features and
    /// role. Used independently by `tools/list` and `tools/call` so a disabled
    /// tool is indistinguishable from an unknown one.
    pub fn allows_tool(&self, name: &str) -> bool {
        match name {
            "request_parent_decision" => {
                self.features.delegation
                    && self.features.coordination_v1
                    && self.role == CompanionRole::DelegationChild
            }
            "complete_work" => {
                self.features.completion_v2 && self.role == CompanionRole::DelegationChild
            }
            "reply_to_delegation" => self.features.delegation && self.features.coordination_v1,
            "get_delegation_orchestration_bindings" => {
                self.features.delegation
                    && self.features.coordination_v1
                    && self.role == CompanionRole::Root
            }
            "register_simple_workflow" => {
                self.features.delegation && self.role == CompanionRole::Root
            }
            "request_recovery_authorization" => {
                (self.features.delegation && self.features.coordination_v1)
                    || (self.features.workflow_v2 && self.role == CompanionRole::Root)
            }
            "get_workflow_capabilities"
            | "get_workflow_state"
            | "publish_workflow_manifest"
            | "settle_workflow_gate"
            | "recover_workflow" => self.features.workflow_v2 && self.role == CompanionRole::Root,
            "delegate_to_agent" => {
                self.features.delegation
                    && (self.role == CompanionRole::Root || self.can_spawn_child)
            }
            other => self.features.allows_legacy_tool(other),
        }
    }
}

/// Per-in-flight-call state. The companion stashes one of these per
/// `tools/call` so a subsequent `notifications/cancelled` for the same
/// JSON-RPC `id` can wake the round-trip task and trigger a broker-side
/// cancel.
pub struct InflightEntry {
    /// Companion-minted opaque handle threaded through the broker, for the
    /// `delegate_to_agent` tool ONLY — a `notifications/cancelled` during its
    /// setup must tear down the just-started child via the broker's
    /// `cancel_by_external_handle`. `None` for `get_delegation_status` /
    /// `cancel_delegation`: canceling those round-trips only suppresses the
    /// response (no broker-side cancel — the query/cancel itself must not touch
    /// the task).
    external_handle: Option<String>,
    /// Tripped by the cancel handler to wake the round-trip task.
    cancel_tx: oneshot::Sender<()>,
}

/// `request_id_key(id) → InflightEntry`. Keyed by a string form of the
/// JSON-RPC `id` so we can compare against the `requestId` payload of
/// `notifications/cancelled` which is itself a JSON value (numbers serialize
/// as their canonical string form here).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BindingArtifactState {
    PendingRelay,
    Delivered,
}

struct BindingArtifactRegistry {
    entries: StdMutex<HashMap<PathBuf, BindingArtifactState>>,
    metrics: Arc<DelegationMetrics>,
}

impl BindingArtifactRegistry {
    fn new(metrics: Arc<DelegationMetrics>) -> Self {
        Self {
            entries: StdMutex::new(HashMap::new()),
            metrics,
        }
    }

    fn register(
        self: &Arc<Self>,
        path: PathBuf,
        export_metrics: PendingBindingArtifactMetrics,
    ) -> PendingBindingArtifact {
        self.entries
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .insert(path.clone(), BindingArtifactState::PendingRelay);
        PendingBindingArtifact {
            path,
            registry: self.clone(),
            cleanup_on_drop: true,
            export_metrics: Some(export_metrics),
        }
    }

    fn mark_delivered(&self, path: &Path) {
        if let Some(state) = self
            .entries
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .get_mut(path)
        {
            *state = BindingArtifactState::Delivered;
        }
    }

    fn cleanup(&self, path: &Path) {
        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if !entries.contains_key(path) {
            return;
        }
        match fs::remove_file(path) {
            Ok(()) => {
                entries.remove(path);
                self.metrics.record_artifact_cleanup(true);
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                entries.remove(path);
                self.metrics.record_artifact_cleanup(true);
            }
            Err(_) => self.metrics.record_artifact_cleanup(false),
        }
    }

    fn drain(&self) {
        let paths = self
            .entries
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        for path in paths {
            self.cleanup(&path);
        }
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.entries
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .len()
    }
}

pub struct PendingBindingArtifact {
    path: PathBuf,
    registry: Arc<BindingArtifactRegistry>,
    cleanup_on_drop: bool,
    export_metrics: Option<PendingBindingArtifactMetrics>,
}

struct PendingBindingArtifactMetrics {
    internal_page_count: usize,
    selected_row_count: usize,
    evidence_bytes: usize,
    export_duration: Duration,
    final_result_bytes: usize,
}

impl PendingBindingArtifact {
    pub fn set_final_result_bytes(mut self, final_result_bytes: usize) -> Self {
        if let Some(metrics) = &mut self.export_metrics {
            metrics.final_result_bytes = final_result_bytes;
        }
        self
    }

    fn record_outcome(&mut self, outcome: ArtifactExportOutcome) {
        let Some(export) = self.export_metrics.take() else {
            return;
        };
        self.registry.metrics.record_artifact_export(outcome);
        self.registry.metrics.record_artifact_shape(
            export.internal_page_count,
            export.selected_row_count,
            export.evidence_bytes,
            export.export_duration,
            export.final_result_bytes,
        );
    }

    fn mark_delivered(mut self) {
        self.record_outcome(ArtifactExportOutcome::Success);
        self.registry.mark_delivered(&self.path);
        self.cleanup_on_drop = false;
    }

    pub fn after_relay(self) -> futures_util::future::BoxFuture<'static, ()> {
        Box::pin(async move { self.mark_delivered() })
    }
}

impl Drop for PendingBindingArtifact {
    fn drop(&mut self) {
        if self.cleanup_on_drop {
            self.record_outcome(ArtifactExportOutcome::Cancelled);
            self.registry.cleanup(&self.path);
        }
    }
}

pub struct InflightCalls {
    inner: Mutex<HashMap<String, InflightEntry>>,
    binding_artifacts: Arc<BindingArtifactRegistry>,
}

impl Default for InflightCalls {
    fn default() -> Self {
        Self::with_artifact_metrics(Arc::new(DelegationMetrics::default()))
    }
}

impl InflightCalls {
    pub fn new() -> Self {
        Self::default()
    }

    fn with_artifact_metrics(metrics: Arc<DelegationMetrics>) -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
            binding_artifacts: Arc::new(BindingArtifactRegistry::new(metrics)),
        }
    }

    #[cfg(test)]
    fn binding_artifact_count(&self) -> usize {
        self.binding_artifacts.len()
    }

    async fn register(&self, id_key: String, entry: InflightEntry) {
        self.inner.lock().await.insert(id_key, entry);
    }

    async fn take(&self, id_key: &str) -> Option<InflightEntry> {
        self.inner.lock().await.remove(id_key)
    }

    /// Drain every in-flight entry, clearing the registry. Called at
    /// companion shutdown so we can fire one broker cancel per pending
    /// delegation — without this the broker would park on `rx.await` for
    /// each entry until the parent ACP connection's `cancel_by_parent`
    /// fires (or never, if the agent CLI keeps running after only the
    /// MCP child died).
    pub async fn drain_all(&self) -> Vec<InflightEntry> {
        let mut map = self.inner.lock().await;
        map.drain().map(|(_k, v)| v).collect()
    }

    fn drain_binding_artifacts(&self) {
        self.binding_artifacts.drain();
    }

    pub fn record_binding_artifact_stale_restart(&self) {
        self.binding_artifacts
            .metrics
            .record_artifact_transparent_stale_restart();
    }
}

#[derive(Debug)]
struct PreparedBindingEvidence {
    evidence: BindingEvidenceV1,
    bytes: Vec<u8>,
    total_rows: usize,
}

fn valid_binding_artifact_cursor(cursor: &Option<String>) -> bool {
    cursor.as_ref().is_none_or(|cursor| {
        !cursor.is_empty()
            && cursor.len() <= 128
            && cursor
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
    })
}

fn prepare_binding_evidence(
    pages: Vec<DelegationOrchestrationBindingPage>,
) -> Result<PreparedBindingEvidence, OrchestrationBindingQueryError> {
    let Some(first) = pages.first() else {
        return Err(OrchestrationBindingQueryError::Invalid);
    };
    let namespace = first.namespace.as_bytes();
    let snapshot_id = uuid::Uuid::parse_str(&first.snapshot_id)
        .ok()
        .filter(|snapshot_id| snapshot_id.to_string() == first.snapshot_id);
    let snapshot_revision = first
        .snapshot_revision
        .parse::<u64>()
        .ok()
        .filter(|revision| revision.to_string() == first.snapshot_revision);
    if first.schema_version != 1
        || namespace.is_empty()
        || namespace.len() > 64
        || !namespace[0].is_ascii_lowercase()
        || !namespace[1..]
            .iter()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'-')
        || snapshot_id.is_none()
        || snapshot_revision.is_none()
        || first.snapshot_expires_at <= first.snapshot_created_at
        || first.snapshot_expires_at - first.snapshot_created_at > chrono::Duration::seconds(60)
        || first.page_start != 0
        || first.request_cursor.is_some()
        || !valid_binding_artifact_cursor(&first.next_cursor)
    {
        return Err(OrchestrationBindingQueryError::Invalid);
    }
    if first.total_rows > ORCHESTRATION_BINDING_ARTIFACT_MAX_ROWS as u64 {
        return Err(OrchestrationBindingQueryError::TooLarge);
    }

    let mut expected_start = 0u64;
    let mut previous_next_cursor: Option<&str> = None;
    let mut seen_complete = false;
    let mut seen_task_ids = HashSet::new();
    for (index, page) in pages.iter().enumerate() {
        if page.schema_version != first.schema_version
            || page.namespace != first.namespace
            || page.snapshot_id != first.snapshot_id
            || page.snapshot_revision != first.snapshot_revision
            || page.snapshot_created_at != first.snapshot_created_at
            || page.snapshot_expires_at != first.snapshot_expires_at
            || page.total_rows != first.total_rows
            || seen_complete
            || page.page_start != expected_start
            || !valid_binding_artifact_cursor(&page.request_cursor)
            || !valid_binding_artifact_cursor(&page.next_cursor)
            || (index > 0 && page.request_cursor.as_deref() != previous_next_cursor)
            || (page.complete && page.next_cursor.is_some())
            || (!page.complete && page.next_cursor.is_none())
        {
            return Err(OrchestrationBindingQueryError::Invalid);
        }
        expected_start = expected_start
            .checked_add(page.runs.len() as u64)
            .ok_or(OrchestrationBindingQueryError::TooLarge)?;
        if expected_start > ORCHESTRATION_BINDING_ARTIFACT_MAX_ROWS as u64 {
            return Err(OrchestrationBindingQueryError::TooLarge);
        }
        for run in &page.runs {
            if run.task_id.is_empty() || !seen_task_ids.insert(run.task_id.as_str()) {
                return Err(OrchestrationBindingQueryError::Invalid);
            }
        }
        seen_complete = page.complete;
        previous_next_cursor = page.next_cursor.as_deref();
    }
    if !seen_complete || expected_start != first.total_rows {
        return Err(OrchestrationBindingQueryError::Invalid);
    }

    let total_rows = expected_start as usize;
    let evidence = BindingEvidenceV1 {
        schema_version: 1,
        pages,
    };
    let bytes =
        serde_json::to_vec(&evidence).map_err(|_| OrchestrationBindingQueryError::Failed)?;
    if bytes.len() > ORCHESTRATION_BINDING_ARTIFACT_MAX_BYTES {
        return Err(OrchestrationBindingQueryError::ArtifactTooLarge);
    }
    Ok(PreparedBindingEvidence {
        evidence,
        bytes,
        total_rows,
    })
}

#[cfg(unix)]
fn set_private_directory_permissions(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
}

#[cfg(not(unix))]
fn set_private_directory_permissions(_path: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(unix)]
fn set_private_file_permissions(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
fn set_private_file_permissions(_path: &Path) -> io::Result<()> {
    Ok(())
}

fn canonical_binding_artifact_root(root: &Path) -> io::Result<PathBuf> {
    let parent = root.parent().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "artifact root has no parent")
    })?;
    fs::create_dir_all(parent)?;
    set_private_directory_permissions(parent)?;
    fs::create_dir_all(root)?;
    set_private_directory_permissions(root)?;
    let canonical_parent = parent.canonicalize()?;
    let canonical_root = root.canonicalize()?;
    if canonical_root.parent() != Some(canonical_parent.as_path()) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "artifact root escaped its fixed parent",
        ));
    }
    Ok(canonical_root)
}

fn canonical_binding_artifact_owner(root: &Path, incarnation_id: &str) -> io::Result<PathBuf> {
    let incarnation = uuid::Uuid::parse_str(incarnation_id)
        .ok()
        .filter(|incarnation| incarnation.to_string() == incarnation_id)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "connection incarnation is not a canonical UUID",
            )
        })?;
    let canonical_root = canonical_binding_artifact_root(root)?;
    let owner = canonical_root.join(incarnation.to_string());
    fs::create_dir_all(&owner)?;
    set_private_directory_permissions(&owner)?;
    let canonical_owner = owner.canonicalize()?;
    if canonical_owner.parent() != Some(canonical_root.as_path()) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "artifact owner escaped the fixed root",
        ));
    }
    Ok(canonical_owner)
}

fn sweep_stale_binding_artifacts_at(
    root: &Path,
    now: SystemTime,
    metrics: &DelegationMetrics,
) -> io::Result<()> {
    let canonical_root = canonical_binding_artifact_root(root)?;
    for owner in fs::read_dir(&canonical_root)? {
        let owner = owner?;
        if !owner.file_type()?.is_dir() {
            continue;
        }
        let owner_name = owner.file_name();
        let Some(owner_name) = owner_name.to_str() else {
            continue;
        };
        if uuid::Uuid::parse_str(owner_name)
            .ok()
            .is_none_or(|owner_id| owner_id.to_string() != owner_name)
        {
            continue;
        }
        let canonical_owner = owner.path().canonicalize()?;
        if canonical_owner.parent() != Some(canonical_root.as_path()) {
            continue;
        }
        for file in fs::read_dir(&canonical_owner)? {
            let file = file?;
            if !file.file_type()?.is_file() {
                continue;
            }
            let path = file.path();
            let canonical_file = path.canonicalize()?;
            if canonical_file.parent() != Some(canonical_owner.as_path()) {
                continue;
            }
            let modified = file.metadata()?.modified()?;
            if now
                .duration_since(modified)
                .is_ok_and(|age| age > ORCHESTRATION_BINDING_ARTIFACT_STALE_AGE)
            {
                match fs::remove_file(&canonical_file) {
                    Ok(()) => metrics.record_artifact_cleanup(true),
                    Err(error) => {
                        metrics.record_artifact_cleanup(false);
                        return Err(error);
                    }
                }
            }
        }
    }
    Ok(())
}

fn atomic_publish_binding_artifact(
    owner: &Path,
    final_path: &Path,
    bytes: &[u8],
) -> io::Result<()> {
    let mut partial = tempfile::NamedTempFile::new_in(owner)?;
    set_private_file_permissions(partial.path())?;
    partial.write_all(bytes)?;
    partial.as_file_mut().sync_all()?;
    partial
        .persist_noclobber(final_path)
        .map(|_| ())
        .map_err(|error| error.error)
}

fn artifact_storage_metric_outcome(error: OrchestrationBindingQueryError) -> ArtifactExportOutcome {
    match error {
        OrchestrationBindingQueryError::SnapshotStale => ArtifactExportOutcome::Stale,
        OrchestrationBindingQueryError::Failed => ArtifactExportOutcome::SerializationFailed,
        OrchestrationBindingQueryError::ArtifactIoFailed => ArtifactExportOutcome::IoFailed,
        OrchestrationBindingQueryError::TooLarge
        | OrchestrationBindingQueryError::ArtifactTooLarge => ArtifactExportOutcome::TooLarge,
        OrchestrationBindingQueryError::ArtifactResultTooLarge => {
            ArtifactExportOutcome::ResultTooLarge
        }
        OrchestrationBindingQueryError::Invalid => ArtifactExportOutcome::BrokerFailed,
    }
}

fn store_binding_artifact_at(
    root: &Path,
    incarnation_id: &str,
    pages: Vec<DelegationOrchestrationBindingPage>,
    inflight: &Arc<InflightCalls>,
) -> Result<
    (
        OrchestrationBindingArtifactDescriptor,
        PendingBindingArtifact,
    ),
    OrchestrationBindingQueryError,
> {
    let started = Instant::now();
    let prepared = match prepare_binding_evidence(pages) {
        Ok(prepared) => prepared,
        Err(error) => {
            inflight
                .binding_artifacts
                .metrics
                .record_artifact_export(artifact_storage_metric_outcome(error));
            return Err(error);
        }
    };
    if sweep_stale_binding_artifacts_at(
        root,
        SystemTime::now(),
        &inflight.binding_artifacts.metrics,
    )
    .is_err()
    {
        inflight
            .binding_artifacts
            .metrics
            .record_artifact_export(ArtifactExportOutcome::IoFailed);
        return Err(OrchestrationBindingQueryError::ArtifactIoFailed);
    }
    let owner = canonical_binding_artifact_owner(root, incarnation_id).map_err(|_| {
        inflight
            .binding_artifacts
            .metrics
            .record_artifact_export(ArtifactExportOutcome::IoFailed);
        OrchestrationBindingQueryError::ArtifactIoFailed
    })?;
    let final_path = owner.join(format!("{}.json", uuid::Uuid::new_v4()));
    let artifact_path = final_path.to_str().map(str::to_owned).ok_or_else(|| {
        inflight
            .binding_artifacts
            .metrics
            .record_artifact_export(ArtifactExportOutcome::IoFailed);
        OrchestrationBindingQueryError::ArtifactIoFailed
    })?;
    let digest = format!("sha256:{:x}", Sha256::digest(&prepared.bytes));
    if atomic_publish_binding_artifact(&owner, &final_path, &prepared.bytes).is_err() {
        inflight
            .binding_artifacts
            .metrics
            .record_artifact_export(ArtifactExportOutcome::IoFailed);
        return Err(OrchestrationBindingQueryError::ArtifactIoFailed);
    }
    let pending = inflight.binding_artifacts.register(
        final_path,
        PendingBindingArtifactMetrics {
            internal_page_count: prepared.evidence.pages.len(),
            selected_row_count: prepared.total_rows,
            evidence_bytes: prepared.bytes.len(),
            export_duration: started.elapsed(),
            final_result_bytes: 0,
        },
    );
    let first = &prepared.evidence.pages[0];
    let descriptor = OrchestrationBindingArtifactDescriptor {
        schema_version: 1,
        delivery: "artifact".into(),
        namespace: first.namespace.clone(),
        snapshot_id: first.snapshot_id.clone(),
        snapshot_revision: first.snapshot_revision.clone(),
        snapshot_created_at: first.snapshot_created_at,
        snapshot_expires_at: first.snapshot_expires_at,
        total_rows: prepared.total_rows as u64,
        artifact_path,
        artifact_format: ORCHESTRATION_BINDING_ARTIFACT_FORMAT.into(),
        artifact_bytes: prepared.bytes.len() as u64,
        artifact_sha256: digest,
    };
    Ok((descriptor, pending))
}

#[allow(dead_code)] // Activated by Task 4; Task 3 keeps artifact delivery private.
pub(crate) fn store_binding_artifact(
    incarnation_id: &str,
    pages: Vec<DelegationOrchestrationBindingPage>,
    inflight: &Arc<InflightCalls>,
) -> Result<
    (
        OrchestrationBindingArtifactDescriptor,
        PendingBindingArtifact,
    ),
    OrchestrationBindingQueryError,
> {
    let root = std::env::temp_dir().join("codeg-mcp/orchestration-bindings");
    store_binding_artifact_at(&root, incarnation_id, pages, inflight)
}

/// Canonicalize a JSON-RPC `id` to a string suitable as a `HashMap` key.
/// JSON-RPC permits string OR number ids; we collapse both via
/// `serde_json::to_string` so a numeric `42` and string `"42"` stay
/// distinct (which the spec also requires).
pub fn request_id_key(id: &Value) -> String {
    serde_json::to_string(id).unwrap_or_else(|_| String::from("null"))
}

/// Dispatch verdict for a single inbound stdin line.
pub enum LineAction {
    /// Synchronous response — write `resp` to stdout immediately.
    Respond(JsonRpcResponse),
    /// Asynchronous tools/call — the binary should spawn the round-trip
    /// task and only write a response if the future returns `Some`.
    Spawn(SpawnedCall),
    /// Notification or no-op (parse errors with `id = null`). Nothing to
    /// emit on stdout.
    Silent,
}

/// Resolution of a spawned `tools/call`: the response to relay to the agent
/// (`None` = cancellation won, so suppress per the MCP spec) plus an optional
/// action the binary runs ONLY after that response is successfully written to
/// the agent's stdout.
///
/// `after_relay` exists for `check_user_feedback`: marking the pulled notes
/// `Delivered` (the broker `CommitFeedback`) must happen strictly AFTER the
/// agent actually receives them. Committing any earlier — at listener read
/// time, or right after the round-trip but before the stdout relay — would mark
/// a note delivered that a failed/never-reached write (or a companion dying mid
/// teardown) never put in front of the agent, breaking at-least-once delivery.
/// Every other tool leaves this `None`.
pub struct SpawnResult {
    pub response: Option<JsonRpcResponse>,
    pub after_relay: Option<futures_util::future::BoxFuture<'static, ()>>,
}

/// Materialized async tools/call ready to drive in a tokio task. The binary
/// awaits `future` to obtain the [`SpawnResult`]: it writes `response` (when
/// `Some`) and, on a successful write, runs `after_relay` (when `Some`).
pub struct SpawnedCall {
    /// JSON-RPC `id` of the original `tools/call` so the binary can stamp
    /// the response.
    pub request_id: Value,
    /// String form of `request_id` for inflight bookkeeping.
    pub request_id_key: String,
    /// The future that performs the UDS round-trip racing the cancel channel
    /// and resolves to the [`SpawnResult`] to relay (and optionally commit).
    pub future: futures_util::future::BoxFuture<'static, SpawnResult>,
}

/// Parse a stdin line and produce a [`LineAction`]. The binary handles the
/// IO side; this function is pure aside from registering the inflight
/// entry on `tools/call` so unit tests can drive it without stdio.
pub async fn dispatch_line(
    ctx: &CompanionContext,
    inflight: Arc<InflightCalls>,
    line: &str,
) -> LineAction {
    let req: JsonRpcRequest = match serde_json::from_str(line) {
        Ok(r) => r,
        Err(e) => {
            return LineAction::Respond(err(Value::Null, -32700, format!("parse error: {e}")));
        }
    };

    // Notifications carry no id — no response goes out. Cancellation is
    // the only notification we act on.
    if req.id.is_none() {
        if req.method == "notifications/cancelled" {
            handle_cancel_notification(ctx, &inflight, &req.params).await;
        }
        return LineAction::Silent;
    }

    let id = req.id.expect("checked is_none");
    match req.method.as_str() {
        "initialize" => LineAction::Respond(ok(
            id,
            json!({
                "protocolVersion": "2024-11-05",
                "serverInfo": {
                    "name": "codeg-mcp",
                    "version": env!("CARGO_PKG_VERSION"),
                },
                "capabilities": { "tools": {} },
            }),
        )),
        "tools/list" => {
            // The embedded schema is a JSON array of every tool the companion
            // can carry; filter to the groups enabled for this launch so a
            // disabled feature's tools never surface to the LLM.
            let all: Value = match serde_json::from_str(TOOL_SCHEMA_JSON) {
                Ok(v) => v,
                Err(e) => {
                    return LineAction::Respond(err(
                        id,
                        -32603,
                        format!("embedded schema invalid: {e}"),
                    ));
                }
            };
            let mut tools = match all.as_array() {
                Some(arr) => {
                    let mut filtered: Vec<Value> = arr
                        .iter()
                        .filter(|t| {
                            t.get("name")
                                .and_then(|v| v.as_str())
                                .map(|n| ctx.allows_tool(n))
                                .unwrap_or(false)
                        })
                        .cloned()
                        .collect();
                    // Without coordination_v1, restore pre-Join descriptions
                    // and hide return_when so old connections cannot call Join.
                    if !ctx.features.coordination_v1 {
                        for tool in &mut filtered {
                            let name = tool.get("name").and_then(Value::as_str).unwrap_or("");
                            match name {
                                "delegate_to_agent" => {
                                    if let Some(obj) = tool.as_object_mut() {
                                        obj.insert(
                                            "description".into(),
                                            Value::String(LEGACY_DELEGATE_DESCRIPTION.into()),
                                        );
                                    }
                                }
                                "get_delegation_status" => {
                                    if let Some(obj) = tool.as_object_mut() {
                                        obj.insert(
                                            "description".into(),
                                            Value::String(LEGACY_STATUS_DESCRIPTION.into()),
                                        );
                                        if let Some(props) = obj
                                            .get_mut("inputSchema")
                                            .and_then(|s| s.get_mut("properties"))
                                            .and_then(Value::as_object_mut)
                                        {
                                            props.remove("return_when");
                                            if let Some(wait_ms) = props
                                                .get_mut("wait_ms")
                                                .and_then(Value::as_object_mut)
                                            {
                                                wait_ms.remove("maximum");
                                                wait_ms.insert(
                                                    "description".into(),
                                                    Value::String(
                                                        LEGACY_WAIT_MS_DESCRIPTION.into(),
                                                    ),
                                                );
                                            }
                                        }
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                    Value::Array(filtered)
                }
                None => all,
            };
            remove_disabled_agents_from_delegate_enum(&mut tools, &ctx.disabled_agents);
            LineAction::Respond(ok(id, json!({ "tools": tools })))
        }
        "tools/call" => build_tools_call_spawn(ctx.clone(), inflight, id, req.params).await,
        _ => LineAction::Respond(err(id, -32601, format!("method not found: {}", req.method))),
    }
}

fn remove_disabled_agents_from_delegate_enum(tools: &mut Value, disabled_agents: &[String]) {
    if disabled_agents.is_empty() {
        return;
    }
    let Some(variants) = tools
        .as_array_mut()
        .and_then(|tools| {
            tools
                .iter_mut()
                .find(|tool| tool.get("name").and_then(Value::as_str) == Some("delegate_to_agent"))
        })
        .and_then(|tool| tool.pointer_mut("/inputSchema/properties/agent_type/enum"))
        .and_then(Value::as_array_mut)
    else {
        return;
    };

    variants.retain(|variant| {
        variant
            .as_str()
            .is_none_or(|slug| !disabled_agents.iter().any(|disabled| disabled == slug))
    });
}

/// Build the spawned-call descriptor for a `tools/call` (or, when the
/// arguments are obviously bogus, a synchronous error response). Registers
/// the inflight entry and returns a future the binary should drive.
async fn build_tools_call_spawn(
    ctx: CompanionContext,
    inflight: Arc<InflightCalls>,
    id: Value,
    params: Value,
) -> LineAction {
    let name = params
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if matches!(
        name.as_str(),
        "get_workflow_state" | "get_session_info" | "get_delegation_orchestration_bindings"
    ) && serde_json::to_vec(&id)
        .map(|serialized| {
            serialized.len()
                > if name == "get_session_info" {
                    GET_SESSION_INFO_MAX_REQUEST_ID_BYTES
                } else if name == "get_delegation_orchestration_bindings" {
                    GET_ORCHESTRATION_BINDINGS_MAX_REQUEST_ID_BYTES
                } else {
                    GET_WORKFLOW_STATE_MAX_REQUEST_ID_BYTES
                }
        })
        .unwrap_or(true)
    {
        return LineAction::Respond(err(
            Value::Null,
            -32600,
            format!("Invalid Request: {name} request id exceeds 256 serialized bytes"),
        ));
    }
    let arguments = params.get("arguments").cloned().unwrap_or(Value::Null);
    let socket = ctx.socket_path.clone();
    // Defense in depth: tools/list already hides tools whose feature group is
    // off, but a misbehaving client could still call one by name. A disabled
    // tool is rejected uniformly as "unknown tool" — indistinguishable from a
    // genuinely nonexistent one (no leak that the feature exists but is off),
    // and matching the legacy unknown-tool rejection shape.
    if !ctx.allows_tool(&name) {
        return LineAction::Respond(err(id, -32602, format!("unknown tool: {name}")));
    }
    match name.as_str() {
        "delegate_to_agent" | "continue_delegation" => {
            // MCP clients (Codex / Claude Code) generally do NOT populate
            // `_meta.tool_use_id` when calling an MCP server. We still surface it
            // when present (the most precise binding). For `continue_delegation`,
            // missing id under concurrent cards is fail-closed in the broker.
            let tool_use_id = params
                .get("_meta")
                .and_then(|m| m.get("tool_use_id"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            // Mint an external_handle so a `notifications/cancelled` during setup
            // tears down the just-started child via `cancel_by_external_handle`.
            let external_handle = uuid::Uuid::new_v4().to_string();
            // Tag continue so the listener dispatches continue_delegation.
            let mut input = arguments;
            if name == "continue_delegation" {
                if let Some(obj) = input.as_object_mut() {
                    obj.insert(
                        "_codeg_tool".into(),
                        Value::String("continue_delegation".into()),
                    );
                }
            }
            let req = BrokerRequest {
                token: ctx.token.clone(),
                parent_connection_id: ctx.parent_connection_id.clone(),
                parent_tool_use_id: tool_use_id,
                external_handle: Some(external_handle.clone()),
                input,
            };
            let round_trip = Box::pin(async move { client_round_trip(&socket, &req).await });
            register_and_spawn(
                inflight,
                id,
                Some(external_handle),
                round_trip,
                render_task_report,
            )
            .await
        }
        "get_delegation_status" => {
            // Normalize the `task_ids` array: trim, drop empty/whitespace
            // entries, de-dup (order-preserving). A non-string entry violates the
            // schema's `items: string` contract and is rejected outright (rather
            // than silently polling a subset); an all-empty / missing array maps
            // to `Ok(empty)`, rejected below.
            let task_ids = match normalize_status_task_ids(&arguments) {
                Ok(ids) if !ids.is_empty() => ids,
                Ok(_) => {
                    return LineAction::Respond(err(
                        id,
                        -32602,
                        "get_delegation_status requires a non-empty task_ids array \
                         (one or more task ids)",
                    ));
                }
                Err(msg) => return LineAction::Respond(err(id, -32602, msg)),
            };
            let (wait_ms, return_when) =
                match parse_status_wait_arguments(&arguments, ctx.features.coordination_v1) {
                    Ok(values) => values,
                    Err(message) => return LineAction::Respond(err(id, -32602, message)),
                };
            // Same host `_meta.tool_use_id` surface as `delegate_to_agent`. This
            // is the request-associated wait tool id for later WaitStamp arming;
            // empty when the host omits it (never invent).
            let req = build_status_request(&ctx, task_ids, wait_ms, return_when, &params);
            // No external_handle: canceling a status query only suppresses its
            // response — it must not touch the task itself. The status round-trip
            // returns a `{tasks:[..]}` envelope, so it renders via
            // `render_status_result` — uniformly one `{tasks:[..]}` entry per id,
            // whether the poll asked for a single id or a whole fan-out.
            let round_trip = Box::pin(async move { client_status_round_trip(&socket, &req).await });
            register_and_spawn(inflight, id, None, round_trip, render_status_result).await
        }
        "get_delegation_orchestration_bindings" => {
            if arguments.as_object().is_some_and(|object| {
                object.get("snapshot_id").is_some_and(Value::is_null)
                    || object.get("cursor").is_some_and(Value::is_null)
            }) {
                return orchestration_binding_query_invalid_response(id);
            }
            let query: OrchestrationBindingQueryRequest = match serde_json::from_value(arguments) {
                Ok(query) => query,
                Err(_) => return orchestration_binding_query_invalid_response(id),
            };
            if query.validate().is_err() {
                return orchestration_binding_query_invalid_response(id);
            }
            let req = BrokerOrchestrationBindingsRequest {
                token: ctx.token.clone(),
                namespace: query.namespace,
                limit: query.limit,
                page_limit: None,
                snapshot_id: query.snapshot_id,
                cursor: query.cursor,
            };
            register_and_spawn_orchestration_bindings(inflight, id, socket, req).await
        }
        "cancel_delegation" => {
            let task_id = match arguments.get("task_id").and_then(|v| v.as_str()) {
                Some(s) if !s.is_empty() => s.to_string(),
                _ => {
                    return LineAction::Respond(err(
                        id,
                        -32602,
                        "cancel_delegation requires a non-empty string task_id",
                    ));
                }
            };
            let reason = match parse_cancel_reason(&arguments) {
                Ok(reason) => reason,
                Err(msg) => return LineAction::Respond(err(id, -32602, msg)),
            };
            if reason == CancelDelegationReason::Timeout {
                return LineAction::Respond(ok(
                    id,
                    render_task_report(&timeout_cancel_guidance_report(&task_id)),
                ));
            }
            let req = BrokerCancelTaskRequest {
                token: ctx.token.clone(),
                task_id,
                reason,
            };
            let round_trip =
                Box::pin(async move { client_cancel_task_round_trip(&socket, &req).await });
            register_and_spawn(inflight, id, None, round_trip, render_task_report).await
        }
        "complete_work" => {
            let request: CompleteWorkRequest = match serde_json::from_value(arguments) {
                Ok(request) => request,
                Err(error) => {
                    return LineAction::Respond(err(
                        id,
                        -32602,
                        format!("invalid complete_work arguments: {error}"),
                    ));
                }
            };
            if request
                .summary
                .as_ref()
                .is_some_and(|value| value.len() > COMPLETE_WORK_SUMMARY_MAX_BYTES)
                || request
                    .report_file
                    .as_ref()
                    .is_some_and(|value| value.len() > COMPLETE_WORK_REPORT_FILE_MAX_BYTES)
            {
                return LineAction::Respond(err(
                    id,
                    -32602,
                    "complete_work string exceeds its schema bound",
                ));
            }
            let tool_use_id = params
                .get("_meta")
                .and_then(|meta| meta.get("tool_use_id"))
                .and_then(Value::as_str);
            let child_tool_call_id =
                match derive_child_tool_call_id(tool_use_id, &ctx.connection_incarnation_id, &id) {
                    Ok(identity) => identity,
                    Err(message) => return LineAction::Respond(err(id, -32602, message)),
                };
            let req = BrokerCompleteWorkRequest {
                token: ctx.token.clone(),
                child_tool_call_id,
                request,
            };
            let round_trip =
                Box::pin(async move { client_complete_work_round_trip(&socket, &req).await });
            register_and_spawn(inflight, id, None, round_trip, render_workflow_result).await
        }
        "check_user_feedback" => {
            let req = BrokerFeedbackRequest {
                token: ctx.token.clone(),
            };
            // Feedback uses a dedicated spawn so it can COMMIT delivery only when
            // the round-trip wins the cancel race (i.e. the result actually goes
            // to the agent). A cancel that suppresses the response sends no
            // commit, leaving the notes pending for the next check.
            register_and_spawn_feedback(inflight, id, socket, ctx.token.clone(), req).await
        }
        "ask_user_question" => {
            // Validate + parse the schema HERE so a malformed call gets a
            // synchronous -32602 the LLM can fix, rather than round-tripping bad
            // data. Stable per-question ids are minted now and flow through to
            // the answer correlation.
            let questions = match parse_questions(&arguments) {
                Ok(qs) => qs,
                Err(msg) => return LineAction::Respond(err(id, -32602, msg)),
            };
            let req = BrokerAskRequest {
                token: ctx.token.clone(),
                questions,
            };
            // No external_handle: canceling a blocking ask only suppresses its
            // response. The companion dropping the round-trip future closes the
            // socket, which the listener observes (peer-close) to tear the
            // pending question down — no broker-side cancel to dispatch.
            let round_trip = Box::pin(async move { client_ask_round_trip(&socket, &req).await });
            register_and_spawn(inflight, id, None, round_trip, render_ask_result).await
        }
        "get_session_info" => {
            // `session_id` is the codeg conversation id the agent read out of a
            // `codeg://session/<id>` reference. Accept a JSON number or a numeric
            // string (some hosts stringify integer args); reject anything else
            // synchronously so the LLM can fix it.
            let session_id = match parse_session_id(&arguments) {
                Some(id) => id,
                None => {
                    return LineAction::Respond(err(
                        id,
                        -32602,
                        "get_session_info requires an integer `session_id` \
                         (the number in the codeg://session/<id> reference)",
                    ));
                }
            };
            // Default to a modest recent-message window; `0` means metadata-only.
            // Keep the catalog's integer contract at runtime; clamp oversized
            // numeric values in the existing helper.
            if arguments
                .get("max_messages")
                .is_some_and(|value| !is_nonnegative_json_integer(value))
            {
                return LineAction::Respond(err(
                    id,
                    -32602,
                    "get_session_info max_messages must be a non-negative integer",
                ));
            }
            let max_messages = parse_max_messages(&arguments);
            let req = BrokerSessionRequest {
                token: ctx.token.clone(),
                session_id,
                max_messages: Some(max_messages),
            };
            // No external_handle: a read-only lookup has nothing to cancel
            // broker-side — canceling only suppresses the response.
            let round_trip =
                Box::pin(async move { client_session_round_trip(&socket, &req).await });
            register_and_spawn_session_info(inflight, id, round_trip).await
        }
        "request_parent_decision" => {
            let args = match parse_parent_decision_args(&arguments) {
                Ok(args) => args,
                Err(msg) => return LineAction::Respond(err(id, -32602, msg)),
            };
            // Internal correlation only — never accepted from LLM arguments.
            // Prefer MCP `_meta.tool_use_id` when the host provides it; otherwise
            // the stable JSON-RPC request id for this tools/call lifetime/replay.
            let child_tool_call_id = params
                .get("_meta")
                .and_then(|meta| meta.get("tool_use_id"))
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| format!("mcp_request:{}", request_id_key(&id)));
            let req = BrokerParentDecisionRequest {
                token: ctx.token.clone(),
                child_tool_call_id,
                message: args.message,
            };
            // No external_handle: cancel suppresses the response and closes the
            // socket (listener drops only the waiter) without Broker task cancel.
            let round_trip =
                Box::pin(async move { client_parent_decision_round_trip(&socket, &req).await });
            register_and_spawn(
                inflight,
                id,
                None,
                round_trip,
                render_parent_decision_result,
            )
            .await
        }
        "reply_to_delegation" => {
            let args = match parse_reply_args(&arguments) {
                Ok(args) => args,
                Err(msg) => return LineAction::Respond(err(id, -32602, msg)),
            };
            let req = BrokerReplyDelegationRequest {
                token: ctx.token.clone(),
                request_id: args.request_id,
                reply: args.reply,
            };
            let round_trip =
                Box::pin(async move { client_reply_delegation_round_trip(&socket, &req).await });
            register_and_spawn(
                inflight,
                id,
                None,
                round_trip,
                render_reply_delegation_result,
            )
            .await
        }
        "request_recovery_authorization" => {
            let req = match parse_recovery_authorization_args(&arguments, &ctx.token) {
                Ok(request) => request,
                Err(message) => return LineAction::Respond(err(id, -32602, message)),
            };
            if ctx.role == CompanionRole::DelegationChild
                && req.subject_kind == RecoverySubjectKind::Workflow
            {
                return LineAction::Respond(err(
                    id,
                    -32602,
                    "delegation children cannot authorize workflow recovery",
                ));
            }
            let round_trip =
                Box::pin(
                    async move { client_recovery_authorization_round_trip(&socket, &req).await },
                );
            register_and_spawn(
                inflight,
                id,
                None,
                round_trip,
                render_recovery_authorization_result,
            )
            .await
        }
        // A15.1: answer locally from CompanionFeatures — no UDS / no store.
        "get_workflow_capabilities" => {
            if !arguments.is_object() {
                return LineAction::Respond(err(
                    id,
                    -32602,
                    "get_workflow_capabilities arguments must be an object",
                ));
            }
            let caps = local_workflow_capabilities(&ctx.features, ctx.role);
            LineAction::Respond(ok(id, render_workflow_local_result(&caps)))
        }
        "get_workflow_state" => {
            let req = match parse_get_workflow_state_args(&arguments, &ctx.token) {
                Ok(req) => req,
                Err(message) => {
                    return LineAction::Respond(err(id, -32602, message));
                }
            };
            let round_trip =
                Box::pin(async move { client_get_workflow_state_round_trip(&socket, &req).await });
            register_and_spawn_workflow_state(inflight, id, round_trip).await
        }
        "publish_workflow_manifest" => {
            if let Err(message) = validate_publish_workflow_compacted_fields(&arguments) {
                return LineAction::Respond(err(id, -32602, message));
            }
            let req = BrokerPublishWorkflowRequest {
                token: ctx.token.clone(),
                document: arguments,
            };
            let round_trip =
                Box::pin(async move { client_publish_workflow_round_trip(&socket, &req).await });
            register_and_spawn(inflight, id, None, round_trip, render_workflow_result).await
        }
        "register_simple_workflow" => {
            if arguments
                .get("progress_rel_path")
                .is_some_and(Value::is_null)
            {
                return LineAction::Respond(err(
                    id,
                    -32602,
                    "invalid register_simple_workflow arguments: progress_rel_path must be a string",
                ));
            }
            let arguments: RegisterSimpleWorkflowArguments = match serde_json::from_value(arguments)
            {
                Ok(arguments) => arguments,
                Err(error) => {
                    return LineAction::Respond(err(
                        id,
                        -32602,
                        format!("invalid register_simple_workflow arguments: {error}"),
                    ));
                }
            };
            if arguments.plan_rel_path.trim().is_empty()
                || arguments
                    .progress_rel_path
                    .as_deref()
                    .is_some_and(|path| path.trim().is_empty())
            {
                return LineAction::Respond(err(
                    id,
                    -32602,
                    "register_simple_workflow paths must be non-empty strings",
                ));
            }
            let req = BrokerRegisterSimpleWorkflowRequest {
                token: ctx.token.clone(),
                plan_rel_path: arguments.plan_rel_path,
                progress_rel_path: arguments.progress_rel_path,
            };
            let round_trip =
                Box::pin(
                    async move { client_register_simple_workflow_round_trip(&socket, &req).await },
                );
            register_and_spawn(inflight, id, None, round_trip, render_workflow_result).await
        }
        "settle_workflow_gate" => {
            let req = match parse_settle_workflow_args(&arguments, &ctx.token) {
                Ok(r) => r,
                Err(msg) => return LineAction::Respond(err(id, -32602, msg)),
            };
            let round_trip =
                Box::pin(async move { client_settle_workflow_round_trip(&socket, &req).await });
            register_and_spawn(inflight, id, None, round_trip, render_workflow_result).await
        }
        "recover_workflow" => {
            let req = match parse_recover_workflow_args(&arguments, &ctx.token) {
                Ok(request) => request,
                Err(message) => return LineAction::Respond(err(id, -32602, message)),
            };
            let round_trip =
                Box::pin(async move { client_recover_workflow_round_trip(&socket, &req).await });
            register_and_spawn(inflight, id, None, round_trip, render_workflow_result).await
        }
        other => LineAction::Respond(err(id, -32602, format!("unknown tool: {other}"))),
    }
}

fn derive_child_tool_call_id(
    tool_use_id: Option<&str>,
    connection_incarnation_id: &str,
    json_rpc_request_id: &Value,
) -> Result<String, String> {
    if let Some(tool_use_id) = tool_use_id.filter(|value| !value.is_empty()) {
        return Ok(tool_use_id.to_string());
    }
    if json_rpc_request_id.is_null() || connection_incarnation_id.is_empty() {
        return Err("complete_work requires a stable request identity".into());
    }
    Ok(format!(
        "rpc:{connection_incarnation_id}:{}",
        request_id_key(json_rpc_request_id)
    ))
}

fn parse_get_workflow_state_args(
    arguments: &Value,
    token: &str,
) -> Result<BrokerGetWorkflowStateRequest, String> {
    match arguments.get("detail") {
        None => {}
        Some(Value::String(detail)) if detail == "index" => {}
        Some(_) => {
            return Err("get_workflow_state detail must be \"index\" when provided".to_string());
        }
    }
    let workflow_id = match arguments.get("workflow_id") {
        None => None,
        Some(Value::String(workflow_id)) if !workflow_id.is_empty() => Some(workflow_id.clone()),
        Some(_) => {
            return Err(
                "get_workflow_state workflow_id must be a non-empty string when provided"
                    .to_string(),
            );
        }
    };
    Ok(BrokerGetWorkflowStateRequest {
        token: token.to_string(),
        workflow_id,
    })
}

fn is_nonnegative_json_integer(value: &Value) -> bool {
    value.as_u64().is_some()
}

fn json_u64(value: &Value) -> Option<u64> {
    value.as_u64()
}

fn validate_publish_workflow_compacted_fields(arguments: &Value) -> Result<(), String> {
    if arguments.get("schema_version").and_then(json_u64) != Some(2) {
        return Err("publish_workflow_manifest schema_version must be integer 2".into());
    }
    if arguments
        .get("workflow_id")
        .is_some_and(|value| !value.is_string())
    {
        return Err("publish_workflow_manifest workflow_id must be a string".into());
    }
    if arguments
        .get("expected_manifest_revision")
        .is_some_and(|value| json_u64(value).is_none())
    {
        return Err(
            "publish_workflow_manifest expected_manifest_revision must be a non-negative integer"
                .into(),
        );
    }
    if !arguments
        .get("plan_target_rel_path")
        .is_some_and(Value::is_string)
    {
        return Err("publish_workflow_manifest plan_target_rel_path must be a string".into());
    }
    if arguments.get("risk_policy_version").and_then(Value::as_str) != Some("b2d_task_risk_v1") {
        return Err(
            "publish_workflow_manifest risk_policy_version must be b2d_task_risk_v1".into(),
        );
    }
    if !arguments.get("task_policies").is_some_and(Value::is_array) {
        return Err("publish_workflow_manifest task_policies must be an array".into());
    }
    Ok(())
}

fn reject_unknown_arguments(arguments: &Value, tool: &str, allowed: &[&str]) -> Result<(), String> {
    let object = arguments
        .as_object()
        .ok_or_else(|| format!("{tool} arguments must be an object"))?;
    if let Some(key) = object.keys().find(|key| !allowed.contains(&key.as_str())) {
        return Err(format!("{tool} does not accept `{key}`"));
    }
    Ok(())
}

fn parse_recovery_authorization_args(
    arguments: &Value,
    token: &str,
) -> Result<BrokerRecoveryAuthorizationRequest, String> {
    reject_unknown_arguments(
        arguments,
        "request_recovery_authorization",
        &[
            "subject_kind",
            "subject_id",
            "correlation_id",
            "proposed_user_reason",
        ],
    )?;
    let subject_kind =
        match arguments.get("subject_kind").and_then(Value::as_str) {
            Some("delegation_task") => RecoverySubjectKind::DelegationTask,
            Some("workflow") => RecoverySubjectKind::Workflow,
            _ => return Err(
                "request_recovery_authorization subject_kind must be delegation_task or workflow"
                    .into(),
            ),
        };
    let subject_id = arguments
        .get("subject_id")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| "request_recovery_authorization requires non-empty subject_id".to_string())?
        .to_string();
    let correlation_id = arguments
        .get("correlation_id")
        .and_then(Value::as_str)
        .ok_or_else(|| "request_recovery_authorization requires correlation_id".to_string())?;
    validate_correlation_id(correlation_id).map_err(|message| {
        format!("request_recovery_authorization invalid correlation_id: {message}")
    })?;
    let proposed_user_reason = match arguments.get("proposed_user_reason") {
        None => None,
        Some(Value::String(reason)) if !reason.trim().is_empty() && reason.len() <= 4096 => {
            Some(reason.clone())
        }
        Some(Value::String(_)) => {
            return Err("proposed_user_reason must be nonblank and at most 4096 UTF-8 bytes".into())
        }
        Some(_) => return Err("proposed_user_reason must be a string".into()),
    };
    if subject_kind == RecoverySubjectKind::DelegationTask && proposed_user_reason.is_some() {
        return Err("proposed_user_reason is not accepted for delegation recovery".into());
    }
    Ok(BrokerRecoveryAuthorizationRequest {
        token: token.to_string(),
        subject_kind,
        subject_id,
        correlation_id: correlation_id.to_string(),
        proposed_user_reason,
    })
}

fn parse_recover_workflow_args(
    arguments: &Value,
    token: &str,
) -> Result<BrokerRecoverWorkflowRequest, String> {
    reject_unknown_arguments(
        arguments,
        "recover_workflow",
        &[
            "workflow_id",
            "recovery_authorization_id",
            "expected_manifest_revision",
            "correlation_id",
        ],
    )?;
    let required_string = |key: &str| {
        arguments
            .get(key)
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .map(str::to_string)
            .ok_or_else(|| format!("recover_workflow requires non-empty {key}"))
    };
    let correlation_id = required_string("correlation_id")?;
    validate_correlation_id(&correlation_id)
        .map_err(|message| format!("recover_workflow invalid correlation_id: {message}"))?;
    let expected_manifest_revision = arguments
        .get("expected_manifest_revision")
        .and_then(json_u64)
        .filter(|revision| *revision > 0)
        .ok_or_else(|| {
            "recover_workflow expected_manifest_revision must be a positive integer".to_string()
        })?;
    Ok(BrokerRecoverWorkflowRequest {
        token: token.to_string(),
        workflow_id: required_string("workflow_id")?,
        recovery_authorization_id: required_string("recovery_authorization_id")?,
        expected_manifest_revision,
        correlation_id,
    })
}

fn parse_settle_workflow_args(
    arguments: &Value,
    token: &str,
) -> Result<BrokerSettleWorkflowRequest, String> {
    reject_unknown_arguments(
        arguments,
        "settle_workflow_gate",
        &[
            "workflow_id",
            "gate_id",
            "expected_graph_revision",
            "expected_review_round",
            "expected_gate_cycle",
            "expected_outcome",
            "recovery_authorization_id",
            "summary",
        ],
    )?;
    let workflow_id = arguments
        .get("workflow_id")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "settle_workflow_gate requires non-empty workflow_id".to_string())?
        .to_string();
    let gate_id = arguments
        .get("gate_id")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "settle_workflow_gate requires non-empty gate_id".to_string())?
        .to_string();
    let summary = arguments
        .get("summary")
        .and_then(Value::as_str)
        .ok_or_else(|| "settle_workflow_gate requires summary string".to_string())?
        .to_string();
    let expected_graph_revision = parse_u64_arg(arguments, "expected_graph_revision")?;
    let expected_review_round = parse_optional_u64_arg(arguments, "expected_review_round")?;
    let expected_gate_cycle = parse_optional_u64_arg(arguments, "expected_gate_cycle")?;
    if expected_review_round == Some(0) {
        return Err("settle_workflow_gate expected_review_round must be at least 1".into());
    }
    if expected_gate_cycle == Some(0) {
        return Err("settle_workflow_gate expected_gate_cycle must be at least 1".into());
    }
    let parse_outcome = |key: &str| -> Result<Option<String>, String> {
        let Some(value) = arguments.get(key) else {
            return Ok(None);
        };
        let value = value.as_str().ok_or_else(|| {
            format!("settle_workflow_gate {key} must be approved, changes_requested, or blocked")
        })?;
        if !matches!(value, "approved" | "changes_requested" | "blocked") {
            return Err(format!(
                "settle_workflow_gate {key} must be approved, changes_requested, or blocked"
            ));
        }
        Ok(Some(value.to_string()))
    };
    let expected_outcome = parse_outcome("expected_outcome")?;
    let recovery_authorization_id = arguments
        .get("recovery_authorization_id")
        .map(|value| {
            value
                .as_str()
                .filter(|value| !value.is_empty())
                .map(str::to_string)
                .ok_or_else(|| {
                    "settle_workflow_gate recovery_authorization_id must be non-empty".to_string()
                })
        })
        .transpose()?;
    Ok(BrokerSettleWorkflowRequest {
        token: token.to_string(),
        workflow_id,
        gate_id,
        expected_graph_revision,
        expected_review_round,
        expected_gate_cycle,
        expected_outcome,
        recovery_authorization_id,
        summary,
    })
}

fn parse_u64_arg(arguments: &Value, key: &str) -> Result<u64, String> {
    arguments
        .get(key)
        .and_then(json_u64)
        .ok_or_else(|| format!("settle_workflow_gate {key} must be a non-negative integer"))
}

fn parse_optional_u64_arg(arguments: &Value, key: &str) -> Result<Option<u64>, String> {
    match arguments.get(key) {
        None => Ok(None),
        Some(value) => json_u64(value)
            .map(Some)
            .ok_or_else(|| format!("settle_workflow_gate {key} must be a non-negative integer")),
    }
}

/// Local capability result (no broker error shape).
fn render_workflow_local_result(caps: &Value) -> Value {
    json!({
        "content": [{
            "type": "text",
            "text": serde_json::to_string(caps).unwrap_or_else(|_| "{}".into()),
        }],
        "isError": false,
        "structuredContent": caps.clone(),
    })
}

/// Broker workflow outcome: success DTO or `{ "error": { code, message } }`.
fn render_workflow_result(outcome: &Value) -> Value {
    let is_error = outcome
        .get("error")
        .map(|e| e.is_object() || e.is_string())
        .unwrap_or(false);
    let text = if is_error {
        outcome
            .get("error")
            .and_then(|e| e.get("message"))
            .and_then(Value::as_str)
            .or_else(|| outcome.get("error").and_then(Value::as_str))
            .unwrap_or("workflow operation failed")
            .to_string()
    } else {
        serde_json::to_string(outcome).unwrap_or_else(|_| outcome.to_string())
    };
    json!({
        "content": [{ "type": "text", "text": text }],
        "isError": is_error,
        "structuredContent": outcome.clone(),
    })
}

fn render_orchestration_binding_page(outcome: &Value) -> Value {
    let is_error = outcome.get("error").is_some();
    let text = if is_error {
        outcome
            .pointer("/error/message")
            .and_then(Value::as_str)
            .unwrap_or("orchestration binding query failed")
            .to_string()
    } else {
        serde_json::to_string(outcome).unwrap_or_else(|_| outcome.to_string())
    };
    json!({
        "content": [{ "type": "text", "text": text }],
        "isError": is_error,
        "structuredContent": outcome.clone(),
    })
}

const ORCHESTRATION_BINDING_CURSOR_BUDGET_PLACEHOLDER: &str = "AAAAAAAAAAAAAAAAAAAAAA";

enum OrchestrationBindingBudgetDecision {
    Response(JsonRpcResponse),
    RetryWithPageLimit(u16),
}

fn bounded_orchestration_binding_error(id: Value, code: &str) -> JsonRpcResponse {
    let message = match code {
        "orchestration_binding_query_invalid" => "invalid orchestration binding query",
        "orchestration_binding_query_too_large" => {
            "orchestration binding query exceeds the row limit"
        }
        "orchestration_binding_snapshot_stale" => "orchestration binding snapshot is stale",
        "payload_too_large" => {
            "orchestration binding row exceeds the 7680-byte stdio transport budget"
        }
        _ => "orchestration binding query failed",
    };
    ok(
        id,
        render_orchestration_binding_page(&json!({
            "error": { "code": code, "message": message }
        })),
    )
}

fn bounded_orchestration_binding_jsonrpc_error(id: Value) -> JsonRpcResponse {
    err(
        id,
        -32603,
        "get_delegation_orchestration_bindings response failed",
    )
}

fn stable_orchestration_binding_error_code(outcome: &Value) -> &'static str {
    match outcome.pointer("/error/code").and_then(Value::as_str) {
        Some("orchestration_binding_query_invalid") => "orchestration_binding_query_invalid",
        Some("orchestration_binding_query_too_large") => "orchestration_binding_query_too_large",
        Some("orchestration_binding_snapshot_stale") => "orchestration_binding_snapshot_stale",
        _ => "orchestration_binding_query_failed",
    }
}

fn orchestration_binding_page_candidate(
    id: &Value,
    page: &DelegationOrchestrationBindingPage,
    included: usize,
) -> Result<JsonRpcResponse, serde_json::Error> {
    let mut candidate = page.clone();
    candidate.runs.truncate(included);
    candidate.next_cursor = Some(ORCHESTRATION_BINDING_CURSOR_BUDGET_PLACEHOLDER.into());
    candidate.complete = false;
    let outcome = serde_json::to_value(candidate)?;
    Ok(ok(id.clone(), render_orchestration_binding_page(&outcome)))
}

fn largest_fitting_orchestration_binding_page_limit(
    id: &Value,
    page: &DelegationOrchestrationBindingPage,
    max_bytes: usize,
) -> Result<Option<u16>, serde_json::Error> {
    if page.runs.is_empty() {
        return Ok(None);
    }

    let mut low = 1usize;
    let mut high = page.runs.len();
    let mut best = None;
    while low <= high {
        let mid = low + (high - low) / 2;
        let candidate = orchestration_binding_page_candidate(id, page, mid)?;
        if serialize_jsonrpc_line(&candidate)?.len() <= max_bytes {
            best = Some(mid);
            low = mid.saturating_add(1);
        } else if mid == 1 {
            break;
        } else {
            high = mid - 1;
        }
    }

    best.map(u16::try_from).transpose().map_err(|_| {
        serde_json::Error::io(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "orchestration binding page limit exceeds u16",
        ))
    })
}

fn orchestration_binding_budget_decision(
    id: &Value,
    outcome: Value,
    max_bytes: usize,
) -> Result<OrchestrationBindingBudgetDecision, serde_json::Error> {
    let preferred = ok(id.clone(), render_orchestration_binding_page(&outcome));
    if serialize_jsonrpc_line(&preferred)?.len() <= max_bytes {
        return Ok(OrchestrationBindingBudgetDecision::Response(preferred));
    }

    if outcome.get("error").is_some() {
        return Ok(OrchestrationBindingBudgetDecision::Response(
            bounded_orchestration_binding_error(
                id.clone(),
                stable_orchestration_binding_error_code(&outcome),
            ),
        ));
    }

    let page = serde_json::from_value::<DelegationOrchestrationBindingPage>(outcome)?;
    Ok(
        match largest_fitting_orchestration_binding_page_limit(id, &page, max_bytes)? {
            Some(page_limit) => OrchestrationBindingBudgetDecision::RetryWithPageLimit(page_limit),
            None => OrchestrationBindingBudgetDecision::Response(
                bounded_orchestration_binding_error(id.clone(), "payload_too_large"),
            ),
        },
    )
}

async fn orchestration_binding_response_with_budget(
    socket: &str,
    mut request: BrokerOrchestrationBindingsRequest,
    id: Value,
    max_bytes: usize,
) -> JsonRpcResponse {
    loop {
        let response = match client_orchestration_bindings_round_trip(socket, &request).await {
            Ok(response) => response,
            Err(_) => return bounded_orchestration_binding_jsonrpc_error(id),
        };
        let decision = match orchestration_binding_budget_decision(&id, response.outcome, max_bytes)
        {
            Ok(decision) => decision,
            Err(_) => return bounded_orchestration_binding_jsonrpc_error(id),
        };
        match decision {
            OrchestrationBindingBudgetDecision::Response(response) => {
                debug_assert!(
                    serialize_jsonrpc_line(&response).is_ok_and(|line| line.len() <= max_bytes),
                    "bounded orchestration response must fit the line budget"
                );
                return response;
            }
            OrchestrationBindingBudgetDecision::RetryWithPageLimit(page_limit) => {
                let current_limit = request.page_limit.unwrap_or(request.limit);
                if page_limit == 0 || page_limit >= current_limit {
                    return bounded_orchestration_binding_error(id, "payload_too_large");
                }
                request.page_limit = Some(page_limit);
            }
        }
    }
}

fn orchestration_binding_query_invalid_response(id: Value) -> LineAction {
    let error = OrchestrationBindingQueryError::Invalid;
    let outcome = json!({
        "error": {
            "code": error.code(),
            "message": error.to_string(),
        }
    });
    LineAction::Respond(ok(id, render_orchestration_binding_page(&outcome)))
}

fn render_recovery_authorization_result(outcome: &Value) -> Value {
    if outcome.get("error").is_some() {
        return render_workflow_result(outcome);
    }
    let status = outcome
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("abandoned");
    let text = match status {
        "approved" => "Recovery approved",
        "declined" => "Recovery declined",
        "abandoned" => "Recovery authorization abandoned",
        _ => "Recovery authorization did not resolve",
    };
    json!({
        "content": [{ "type": "text", "text": text }],
        "isError": false,
        "structuredContent": outcome.clone(),
    })
}

fn render_get_workflow_state_response(id: Value, index: WorkflowStateIndexDto) -> JsonRpcResponse {
    let public_index = index
        .public_value()
        .expect("workflow state index is JSON serializable");
    let text = serde_json::to_string(&public_index)
        .expect("workflow state public JSON value is serializable");
    ok(
        id,
        json!({
            "content": [{ "type": "text", "text": text }],
            "isError": false,
        }),
    )
}

fn render_get_workflow_state_response_with_budget(
    id: Value,
    mut index: WorkflowStateIndexDto,
    max_bytes: usize,
) -> Result<JsonRpcResponse, serde_json::Error> {
    bound_long_completion_attention_node_ids(&mut index);
    let workflow_id = index.workflow_id.clone();
    let preferred = render_get_workflow_state_response(id.clone(), index.clone());
    let preferred_bytes = serialize_jsonrpc_line(&preferred)?.len();
    let mut applied_tokens = Vec::new();

    for step in WorkflowIndexOmissionStep::ALL {
        let response = render_get_workflow_state_response(id.clone(), index.clone());
        let final_bytes = serialize_jsonrpc_line(&response)?.len();
        if final_bytes <= max_bytes {
            tracing::debug!(
                preferred_bytes,
                final_bytes,
                workflow_id,
                applied_omission_tokens = ?applied_tokens,
                "rendered get_workflow_state response within line budget"
            );
            return Ok(response);
        }
        if index.apply_omission_step(step) {
            applied_tokens.push(step.token());
        }
    }

    let response = render_get_workflow_state_response(id.clone(), index.clone());
    let final_bytes = serialize_jsonrpc_line(&response)?.len();
    if index.validate_protected_minimum().is_ok() && final_bytes <= max_bytes {
        tracing::debug!(
            preferred_bytes,
            final_bytes,
            workflow_id,
            applied_omission_tokens = ?applied_tokens,
            "rendered get_workflow_state response within line budget"
        );
        return Ok(response);
    }

    let fallback = render_payload_too_large(id);
    let fallback_bytes = serialize_jsonrpc_line(&fallback)?.len();
    tracing::debug!(
        preferred_bytes,
        final_bytes = fallback_bytes,
        workflow_id,
        applied_omission_tokens = ?applied_tokens,
        "get_workflow_state protected payload exceeded line budget"
    );
    Ok(fallback)
}

fn bound_long_completion_attention_node_ids(index: &mut WorkflowStateIndexDto) {
    fn bound(completion: &mut Option<crate::acp::delegation::workflow::CompletionProjectionV2>) {
        let Some(attention) = completion
            .as_mut()
            .and_then(|completion| completion.card.attention.as_mut())
        else {
            return;
        };
        if attention.node_id.chars().count()
            > crate::acp::delegation::workflow::completion_evidence::COMPLETION_ATTENTION_NODE_ID_MAX_CHARS
        {
            attention.node_id =
                crate::acp::delegation::workflow::completion_evidence::completion_attention_public_node_id(
                    &attention.node_id,
                );
        }
    }

    bound(&mut index.completion);
    for node in &mut index.nodes {
        bound(&mut node.completion);
    }
}

fn render_get_workflow_state_outcome_with_budget(
    id: Value,
    outcome: Value,
    max_bytes: usize,
) -> Result<JsonRpcResponse, serde_json::Error> {
    if outcome.get("error").is_some() {
        let stable_code = workflow_state_stable_error_code(&outcome);
        if stable_code != "internal_error" {
            let response = ok(id.clone(), render_workflow_result(&outcome));
            if serialize_jsonrpc_line(&response)?.len() <= max_bytes {
                return Ok(response);
            }
        }
        let fallback = render_bounded_workflow_error(id, stable_code);
        let _fallback_bytes = serialize_jsonrpc_line(&fallback)?.len();
        return Ok(fallback);
    }

    let index = serde_json::from_value::<WorkflowStateIndexDto>(outcome)?;
    render_get_workflow_state_response_with_budget(id, index, max_bytes)
}

fn render_bounded_workflow_error(id: Value, stable_code: &'static str) -> JsonRpcResponse {
    ok(
        id,
        json!({
            "content": [{
                "type": "text",
                "text": "get_workflow_state failed; inspect structuredContent.error.code"
            }],
            "isError": true,
            "structuredContent": {
                "error": {
                    "code": stable_code,
                    "message": "get_workflow_state failed"
                }
            }
        }),
    )
}

fn workflow_state_stable_error_code(outcome: &Value) -> &'static str {
    match outcome
        .pointer("/error/code")
        .and_then(Value::as_str)
        .unwrap_or_default()
    {
        "risk_assessment_invalid" => "risk_assessment_invalid",
        "task_route_mismatch" => "task_route_mismatch",
        "validation" => "validation",
        "reviewer_set_mismatch" => "reviewer_set_mismatch",
        "plan_review" => "plan_review",
        "not_found" => "not_found",
        "cross_parent" => "cross_parent",
        "stale_manifest_revision" => "stale_manifest_revision",
        "stale_graph_revision" => "stale_graph_revision",
        "publication_token_mismatch" => "publication_token_mismatch",
        "publication_token_conflict" => "publication_token_conflict",
        "admitted_node_identity_mutation" => "admitted_node_identity_mutation",
        "cohort_frozen" => "cohort_frozen",
        "reviewed_task_stale" => "reviewed_task_stale",
        "artifact_digest_mismatch" => "artifact_digest_mismatch",
        "gate_not_ready" => "gate_not_ready",
        "gate_cycle_conflict" => "gate_cycle_conflict",
        "execution_gate_settle_rejected" => "execution_gate_settle_rejected",
        "approval_with_open_findings" => "approval_with_open_findings",
        "approval_rejected_failed_reviewer" => "approval_rejected_failed_reviewer",
        "summary_too_large" => "summary_too_large",
        "negative_finding_counts" => "negative_finding_counts",
        "parent_not_found" => "parent_not_found",
        "busy" => "busy",
        "persistence" => "persistence",
        _ => "internal_error",
    }
}

fn render_payload_too_large(id: Value) -> JsonRpcResponse {
    ok(
        id,
        json!({
            "content": [{
                "type": "text",
                "text": "get_workflow_state payload exceeds the 7680-byte response budget"
            }],
            "isError": true,
            "structuredContent": {
                "error": {
                    "code": "payload_too_large",
                    "message": "get_workflow_state protected recovery index exceeds 7680 bytes"
                }
            }
        }),
    )
}

/// Register the inflight entry and build the [`SpawnedCall`] that races the
/// broker round-trip against the cancel signal. `external_handle` is `Some` only
/// for `delegate_to_agent` (so a cancel during setup tears the child down);
/// `None` for status/cancel queries (a cancel only suppresses the response).
///
/// `render` maps the broker's `BrokerResponse.outcome` into the MCP `tools/call`
/// result body: `delegate_to_agent` / `cancel_delegation` pass
/// [`render_task_report`] (a single report); `get_delegation_status` passes
/// [`render_status_result`] (always a `{tasks:[..]}` envelope, one entry per id).
async fn register_and_spawn(
    inflight: Arc<InflightCalls>,
    id: Value,
    external_handle: Option<String>,
    round_trip: futures_util::future::BoxFuture<'static, std::io::Result<BrokerResponse>>,
    render: fn(&Value) -> Value,
) -> LineAction {
    let (cancel_tx, cancel_rx) = oneshot::channel();
    let id_key = request_id_key(&id);
    inflight
        .register(
            id_key.clone(),
            InflightEntry {
                external_handle,
                cancel_tx,
            },
        )
        .await;

    let id_for_response = id.clone();
    let id_key_for_task = id_key.clone();
    let inflight_for_task = inflight.clone();
    let future = Box::pin(async move {
        // Race the UDS round-trip against the cancel signal. Cancel wins →
        // suppress the response per MCP spec; for `delegate_to_agent` the cancel
        // notification handler is responsible for dispatching the broker-side
        // `Cancel` (status/cancel queries carry no external_handle, so nothing
        // is dispatched).
        let response = tokio::select! {
            biased;
            _ = cancel_rx => {
                let _ = inflight_for_task.take(&id_key_for_task).await;
                None
            }
            rt = round_trip => {
                let _ = inflight_for_task.take(&id_key_for_task).await;
                match rt {
                    Ok(resp) => Some(ok(id_for_response, render(&resp.outcome))),
                    Err(e) => Some(err(
                        id_for_response,
                        -32603,
                        format!("broker round-trip failed: {e}"),
                    )),
                }
            }
        };
        // Delegation / status / cancel have no post-relay step.
        SpawnResult {
            response,
            after_relay: None,
        }
    });

    LineAction::Spawn(SpawnedCall {
        request_id: id,
        request_id_key: id_key,
        future,
    })
}

async fn register_and_spawn_workflow_state(
    inflight: Arc<InflightCalls>,
    id: Value,
    round_trip: futures_util::future::BoxFuture<'static, std::io::Result<BrokerResponse>>,
) -> LineAction {
    let (cancel_tx, cancel_rx) = oneshot::channel();
    let id_key = request_id_key(&id);
    inflight
        .register(
            id_key.clone(),
            InflightEntry {
                external_handle: None,
                cancel_tx,
            },
        )
        .await;

    let id_for_response = id.clone();
    let id_key_for_task = id_key.clone();
    let inflight_for_task = inflight.clone();
    let future = Box::pin(async move {
        let response = tokio::select! {
            biased;
            _ = cancel_rx => {
                let _ = inflight_for_task.take(&id_key_for_task).await;
                None
            }
            rt = round_trip => {
                let _ = inflight_for_task.take(&id_key_for_task).await;
                match rt {
                    Ok(response) => {
                        let rendered = render_get_workflow_state_outcome_with_budget(
                            id_for_response.clone(),
                            response.outcome,
                            GET_WORKFLOW_STATE_MAX_RESULT_BYTES,
                        )
                        .unwrap_or_else(|_| {
                            let internal_error = err(
                                id_for_response.clone(),
                                -32603,
                                "get_workflow_state response serialization failed",
                            );
                            let internal_error_bytes = serialize_jsonrpc_line(&internal_error)
                                .expect("fixed JSON-RPC error is serializable")
                                .len();
                            assert!(
                                internal_error_bytes <= GET_WORKFLOW_STATE_MAX_RESULT_BYTES,
                                "bounded request id keeps fixed workflow error within budget"
                            );
                            internal_error
                        });
                        let rendered_bytes = serialize_jsonrpc_line(&rendered)
                            .expect("workflow response was already serialized")
                            .len();
                        assert!(
                            rendered_bytes <= GET_WORKFLOW_STATE_MAX_RESULT_BYTES,
                            "accepted workflow response must fit the line budget"
                        );
                        Some(rendered)
                    }
                    Err(_) => Some(err(
                        id_for_response,
                        -32603,
                        "get_workflow_state broker round-trip failed",
                    )),
                }
            }
        };
        SpawnResult {
            response,
            after_relay: None,
        }
    });

    LineAction::Spawn(SpawnedCall {
        request_id: id,
        request_id_key: id_key,
        future,
    })
}

async fn register_and_spawn_orchestration_bindings(
    inflight: Arc<InflightCalls>,
    id: Value,
    socket: String,
    request: BrokerOrchestrationBindingsRequest,
) -> LineAction {
    let (cancel_tx, cancel_rx) = oneshot::channel();
    let id_key = request_id_key(&id);
    inflight
        .register(
            id_key.clone(),
            InflightEntry {
                external_handle: None,
                cancel_tx,
            },
        )
        .await;

    let id_for_response = id.clone();
    let id_key_for_task = id_key.clone();
    let inflight_for_task = inflight.clone();
    let future = Box::pin(async move {
        let response = tokio::select! {
            biased;
            _ = cancel_rx => {
                let _ = inflight_for_task.take(&id_key_for_task).await;
                None
            }
            response = orchestration_binding_response_with_budget(
                &socket,
                request,
                id_for_response,
                GET_ORCHESTRATION_BINDINGS_MAX_RESULT_BYTES,
            ) => {
                let _ = inflight_for_task.take(&id_key_for_task).await;
                Some(response)
            }
        };
        SpawnResult {
            response,
            after_relay: None,
        }
    });

    LineAction::Spawn(SpawnedCall {
        request_id: id,
        request_id_key: id_key,
        future,
    })
}

async fn register_and_spawn_session_info(
    inflight: Arc<InflightCalls>,
    id: Value,
    round_trip: futures_util::future::BoxFuture<'static, std::io::Result<BrokerResponse>>,
) -> LineAction {
    let (cancel_tx, cancel_rx) = oneshot::channel();
    let id_key = request_id_key(&id);
    inflight
        .register(
            id_key.clone(),
            InflightEntry {
                external_handle: None,
                cancel_tx,
            },
        )
        .await;

    let id_for_response = id.clone();
    let id_key_for_task = id_key.clone();
    let inflight_for_task = inflight.clone();
    let future = Box::pin(async move {
        let response = tokio::select! {
            biased;
            _ = cancel_rx => {
                let _ = inflight_for_task.take(&id_key_for_task).await;
                None
            }
            rt = round_trip => {
                let _ = inflight_for_task.take(&id_key_for_task).await;
                match rt {
                    Ok(response) => {
                        let rendered = render_session_outcome_with_budget(
                            id_for_response.clone(),
                            response.outcome,
                            GET_SESSION_INFO_MAX_RESULT_BYTES,
                        )
                        .unwrap_or_else(|_| {
                            let internal_error = err(
                                id_for_response.clone(),
                                -32603,
                                "get_session_info response serialization failed",
                            );
                            let internal_error_bytes = serialize_jsonrpc_line(&internal_error)
                                .expect("fixed JSON-RPC error is serializable")
                                .len();
                            assert!(
                                internal_error_bytes <= GET_SESSION_INFO_MAX_RESULT_BYTES,
                                "bounded request id keeps fixed session-info error within budget"
                            );
                            internal_error
                        });
                        let rendered_bytes = serialize_jsonrpc_line(&rendered)
                            .expect("session-info response was already serialized")
                            .len();
                        assert!(
                            rendered_bytes <= GET_SESSION_INFO_MAX_RESULT_BYTES,
                            "accepted session-info response must fit the line budget"
                        );
                        Some(rendered)
                    }
                    Err(e) => Some(err(
                        id_for_response,
                        -32603,
                        format!("broker round-trip failed: {e}"),
                    )),
                }
            }
        };
        SpawnResult {
            response,
            after_relay: None,
        }
    });

    LineAction::Spawn(SpawnedCall {
        request_id: id,
        request_id_key: id_key,
        future,
    })
}

/// `check_user_feedback`-specific spawn. Like [`register_and_spawn`], but it
/// carries an `after_relay` commit — a `CommitFeedback` round-trip marking the
/// pulled notes `Delivered` — that the binary runs ONLY after it successfully
/// writes this response to the agent's stdout (the listener does not commit at
/// read time). Two guards compose to make delivery at-least-once. First, if the
/// cancel branch wins the biased select the result is `response: None` with no
/// `after_relay`, so the check is suppressed and never committed (the notes stay
/// pending for the next check). Second, when the round-trip wins, `after_relay`
/// is built but only fires once the stdout relay succeeds; a failed or
/// never-reached write (a dying companion, a broken agent stdin) skips the
/// commit entirely. So a note flips to `Delivered` only after it was actually
/// put in front of the agent. The sole irreducible boundary is the agent
/// crashing after the bytes are flushed to its stdin but before it reads them —
/// at which point the note is moot (the agent will not act on it), the correct
/// semantics for a delivered best-effort steering side-channel.
async fn register_and_spawn_feedback(
    inflight: Arc<InflightCalls>,
    id: Value,
    socket: String,
    token: String,
    req: BrokerFeedbackRequest,
) -> LineAction {
    let (cancel_tx, cancel_rx) = oneshot::channel();
    let id_key = request_id_key(&id);
    inflight
        .register(
            id_key.clone(),
            InflightEntry {
                external_handle: None,
                cancel_tx,
            },
        )
        .await;

    let id_for_response = id.clone();
    let id_key_for_task = id_key.clone();
    let inflight_for_task = inflight.clone();
    let future = Box::pin(async move {
        tokio::select! {
            biased;
            _ = cancel_rx => {
                // Cancelled before delivery → suppress AND do not commit.
                let _ = inflight_for_task.take(&id_key_for_task).await;
                SpawnResult {
                    response: None,
                    after_relay: None,
                }
            }
            rt = client_feedback_round_trip(&socket, &req) => {
                let _ = inflight_for_task.take(&id_key_for_task).await;
                match rt {
                    Ok(resp) => {
                        // Relay-then-commit: render the agent-facing result now,
                        // but defer the `CommitFeedback` to `after_relay` so it
                        // fires ONLY after the binary writes this response to the
                        // agent's stdout. A dead/failed relay skips the commit,
                        // leaving the notes pending for the next check
                        // (at-least-once at the agent-facing boundary).
                        let outcome = resp.outcome;
                        let response = ok(id_for_response, render_feedback_result(&outcome));
                        let commit: futures_util::future::BoxFuture<'static, ()> =
                            Box::pin(async move {
                                commit_feedback_after_delivery(&socket, &token, &outcome).await;
                            });
                        SpawnResult {
                            response: Some(response),
                            after_relay: Some(commit),
                        }
                    }
                    Err(e) => SpawnResult {
                        response: Some(err(
                            id_for_response,
                            -32603,
                            format!("broker round-trip failed: {e}"),
                        )),
                        after_relay: None,
                    },
                }
            }
        }
    });

    LineAction::Spawn(SpawnedCall {
        request_id: id,
        request_id_key: id_key,
        future,
    })
}

/// Send a `CommitFeedback` for the note ids the listener embedded in the
/// response (`_commit_ids`). Fire-and-forget, bounded by [`BROKER_CANCEL_BUDGET`]:
/// a failed commit just leaves the notes pending for the next check.
async fn commit_feedback_after_delivery(socket: &str, token: &str, outcome: &Value) {
    let ids: Vec<String> = outcome
        .get("_commit_ids")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    if ids.is_empty() {
        return;
    }
    let req = BrokerCommitFeedbackRequest {
        token: token.to_string(),
        ids,
    };
    let _ = tokio::time::timeout(BROKER_CANCEL_BUDGET, client_commit_feedback(socket, &req)).await;
}

/// Handle a `notifications/cancelled` notification. Looks up the in-flight
/// call by `requestId` and fires its cancel channel. Unknown ids are
/// silently ignored per MCP spec.
async fn handle_cancel_notification(
    ctx: &CompanionContext,
    inflight: &Arc<InflightCalls>,
    params: &Value,
) {
    let request_id = match params.get("requestId") {
        Some(v) => v.clone(),
        None => return,
    };
    let id_key = request_id_key(&request_id);
    let Some(entry) = inflight.take(&id_key).await else {
        return;
    };
    let _ = entry.cancel_tx.send(());
    // Only `delegate_to_agent` carries an external_handle. For
    // `get_delegation_status` / `cancel_delegation` there is nothing to cancel
    // broker-side — suppressing the (possibly long-poll) response is the whole
    // effect, and dispatching a broker `Cancel` would wrongly target a task.
    let Some(external_handle) = entry.external_handle else {
        return;
    };
    // Single broker-side cancel per notification: the round-trip task
    // observes `cancel_rx` and only suppresses its response. If we ALSO
    // dispatched a cancel from the task we'd hit the broker twice — the
    // first call drains the pending entry, the second buffers the handle
    // in `pre_canceled_handles` with no consumer (silent leak).
    //
    // Synchronous, bounded by `BROKER_CANCEL_BUDGET`. Detaching via
    // `tokio::spawn` would race the runtime shutdown: if stdin closes
    // before the spawned task scheduled its UDS connect, the runtime
    // drops it and the broker never gets the cancel. The bounded await
    // here guarantees the cancel either lands or hits a known cap
    // before the next stdin line is read.
    let cancel_req = BrokerCancelRequest {
        token: ctx.token.clone(),
        external_handle,
        reason: params
            .get("reason")
            .and_then(|v| v.as_str())
            .map(str::to_string),
    };
    send_broker_cancel(&ctx.socket_path, &cancel_req).await;
}

/// Drain every in-flight `tools/call` entry and dispatch a broker cancel
/// for each. Called at companion shutdown (stdin EOF, parent-watchdog
/// fire) so the broker doesn't hold a `pending` row open forever waiting
/// for a `TurnComplete` whose response we couldn't deliver anyway. Each
/// cancel is bounded by [`BROKER_CANCEL_BUDGET`] so a hung listener
/// can't pin shutdown — the codeg main side's `cancel_by_parent` cascade
/// is the eventual backstop for any cancel that times out here.
pub async fn drain_and_cancel_all(
    ctx: &CompanionContext,
    inflight: &Arc<InflightCalls>,
    reason: &str,
) {
    inflight.drain_binding_artifacts();
    for entry in inflight.drain_all().await {
        // Wake the round-trip task if it's still scheduled, so it can
        // exit promptly when the runtime tears down.
        let _ = entry.cancel_tx.send(());
        // Only delegate_to_agent entries hold an external_handle worth a
        // broker-side cancel; status/cancel queries have nothing to tear down.
        let Some(external_handle) = entry.external_handle else {
            continue;
        };
        let cancel_req = BrokerCancelRequest {
            token: ctx.token.clone(),
            external_handle,
            reason: Some(reason.to_string()),
        };
        send_broker_cancel(&ctx.socket_path, &cancel_req).await;
    }
}

/// Build the UDS/pipe status request for `get_delegation_status`.
///
/// Copies host MCP `_meta.tool_use_id` onto [`BrokerStatusRequest::parent_tool_use_id`]
/// (same pattern as `delegate_to_agent`). Empty string when the host omits it
/// or supplies a non-string — never invent a wait tool id.
fn build_status_request(
    ctx: &CompanionContext,
    task_ids: Vec<String>,
    wait_ms: Option<u64>,
    return_when: Option<crate::acp::delegation::types::DelegationReturnWhen>,
    params: &Value,
) -> BrokerStatusRequest {
    let parent_tool_use_id = params
        .get("_meta")
        .and_then(|m| m.get("tool_use_id"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    BrokerStatusRequest {
        token: ctx.token.clone(),
        task_ids,
        wait_ms,
        return_when,
        parent_tool_use_id,
    }
}

/// Normalize the MCP `get_delegation_status` arguments into the wire `task_ids`
/// list. Reads the `task_ids` array, trims each entry, drops empty / whitespace
/// strings, and de-duplicates while preserving first-seen order. A non-string
/// entry violates the schema's `items: string` contract, so the whole call is
/// rejected (`Err`) instead of silently polling a subset — otherwise a malformed
/// `{"task_ids":[123,"abc"]}` would quietly resolve to just `abc`. `Ok(empty)`
/// means nothing usable was supplied (missing array, or all empty/whitespace);
/// the caller rejects both `Err` and `Ok(empty)` with `-32602`. Empty strings are
/// dropped (not rejected): `items` carries no `minLength`, so `""` satisfies the
/// schema and is treated as a formatting nicety. No upper bound on the count: a
/// fan-out can be arbitrarily wide.
fn normalize_status_task_ids(arguments: &Value) -> Result<Vec<String>, String> {
    let Some(arr) = arguments.get("task_ids").and_then(|v| v.as_array()) else {
        return Ok(Vec::new());
    };
    let mut out: Vec<String> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for v in arr {
        let Some(s) = v.as_str() else {
            return Err(
                "get_delegation_status task_ids must contain only string task ids".to_string(),
            );
        };
        let trimmed = s.trim();
        if !trimmed.is_empty() && seen.insert(trimmed.to_string()) {
            out.push(trimmed.to_string());
        }
    }
    Ok(out)
}

fn parse_cancel_reason(arguments: &Value) -> Result<CancelDelegationReason, String> {
    let Some(reason) = arguments.get("reason").and_then(|v| v.as_str()) else {
        return Err(cancel_reason_error());
    };
    match reason {
        "timeout" => Ok(CancelDelegationReason::Timeout),
        "taskfail" => Ok(CancelDelegationReason::TaskFail),
        "usercancel" => Ok(CancelDelegationReason::UserCancel),
        "others" => Ok(CancelDelegationReason::Others),
        _ => Err(cancel_reason_error()),
    }
}

fn cancel_reason_error() -> String {
    "cancel_delegation requires reason to be one of: timeout, taskfail, usercancel, others"
        .to_string()
}

fn timeout_cancel_guidance_report(task_id: &str) -> Value {
    json!({
        "task_id": task_id,
        "status": "running",
        "message": crate::acp::delegation::types::TIMEOUT_CANCEL_GUIDANCE
    })
}

/// Render the `get_delegation_status` round-trip outcome into an MCP
/// `tools/call` result. Preserves Join fields (`wake_reason`,
/// `attention_requests`) in both text content and `structuredContent`. Legacy
/// outcomes without those keys stay a tasks-only envelope.
pub fn render_status_result(outcome: &Value) -> Value {
    if let Some(error) = outcome.get("error") {
        let message = error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("Delegation continuation could not be armed");
        return json!({
            "content": [{"type": "text", "text": message}],
            "isError": true,
            "structuredContent": outcome,
        });
    }
    let tasks = outcome
        .get("tasks")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_else(|| vec![outcome.clone()]);
    let mut envelope = serde_json::Map::new();
    envelope.insert("tasks".into(), Value::Array(tasks.clone()));
    for key in ["wake_reason", "attention_requests"] {
        if let Some(value) = outcome.get(key) {
            envelope.insert(key.into(), value.clone());
        }
    }
    render_status_envelope(Value::Object(envelope), &tasks)
}

fn render_status_envelope(envelope: Value, tasks: &[Value]) -> Value {
    let all_failed = !tasks.is_empty()
        && tasks
            .iter()
            .all(|task| task.get("status").and_then(Value::as_str) == Some("failed"));
    let text = serde_json::to_string(&envelope).unwrap_or_else(|_| String::from("{\"tasks\":[]}"));
    json!({
        "content": [{"type": "text", "text": text}],
        "isError": all_failed,
        "structuredContent": envelope,
    })
}

/// Parse the optional Join `return_when` argument. Absent is legacy; present
/// requires `coordination_v1`, the literal enum value, and explicit `wait_ms=0`.
pub fn parse_return_when(
    arguments: &Value,
    coordination_v1: bool,
) -> Result<Option<DelegationReturnWhen>, String> {
    let Some(raw) = arguments.get("return_when") else {
        return Ok(None);
    };
    if !coordination_v1 {
        return Err("return_when is unavailable on this connection".into());
    }
    if raw.as_str() != Some("all_terminal_or_attention") {
        return Err("return_when must be all_terminal_or_attention".into());
    }
    if arguments.get("wait_ms").and_then(Value::as_u64) != Some(0) {
        return Err("return_when=all_terminal_or_attention requires explicit wait_ms=0".into());
    }
    Ok(Some(DelegationReturnWhen::AllTerminalOrAttention))
}

fn parse_status_wait_arguments(
    arguments: &Value,
    coordination_v1: bool,
) -> Result<(Option<u64>, Option<DelegationReturnWhen>), String> {
    let wait_ms = match arguments.get("wait_ms") {
        None => None,
        Some(value) => Some(
            json_u64(value).ok_or_else(|| "wait_ms must be a non-negative integer".to_string())?,
        ),
    };
    let return_when = parse_return_when(arguments, coordination_v1)?;
    if coordination_v1 && return_when.is_none() && wait_ms.is_some_and(|ms| ms > 0) {
        return Err(COORDINATION_POSITIVE_WAIT_ERROR.into());
    }
    Ok((wait_ms, return_when))
}

struct ParentDecisionArgs {
    message: String,
}

struct ReplyArgs {
    request_id: String,
    reply: String,
}

fn exact_object<'a>(
    value: &'a Value,
    allowed: &[&str],
) -> Result<&'a serde_json::Map<String, Value>, String> {
    let object = value
        .as_object()
        .ok_or_else(|| "arguments must be an object".to_string())?;
    if object.keys().any(|key| !allowed.contains(&key.as_str())) {
        return Err("arguments contain unexpected keys".into());
    }
    Ok(object)
}

fn bounded_nonblank(object: &serde_json::Map<String, Value>, key: &str) -> Result<String, String> {
    let value = object
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("{key} must be a string"))?;
    if value.trim().is_empty() {
        return Err(format!("{key} must not be blank"));
    }
    // `str::len` is UTF-8 byte length (same as `as_bytes().len()`).
    if value.len() > ATTENTION_PAYLOAD_MAX_BYTES {
        return Err(format!("{key} exceeds 16 KiB UTF-8"));
    }
    Ok(value.to_string())
}

fn parse_parent_decision_args(arguments: &Value) -> Result<ParentDecisionArgs, String> {
    let object = exact_object(arguments, &["message"])?;
    Ok(ParentDecisionArgs {
        message: bounded_nonblank(object, "message")?,
    })
}

fn parse_reply_args(arguments: &Value) -> Result<ReplyArgs, String> {
    let object = exact_object(arguments, &["request_id", "reply"])?;
    let request_id = object
        .get("request_id")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| "request_id must be a nonblank string".to_string())?
        .to_string();
    Ok(ReplyArgs {
        request_id,
        reply: bounded_nonblank(object, "reply")?,
    })
}

fn render_parent_decision_result(outcome: &Value) -> Value {
    let status = match outcome.get("status").and_then(Value::as_str) {
        Some("replied") => "replied",
        Some("closed") => "closed",
        _ => "rejected",
    };
    let text = match status {
        "replied" => outcome
            .get("reply")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        "closed" => format!(
            "Parent decision request closed: {}",
            outcome
                .get("resolution_code")
                .and_then(Value::as_str)
                .unwrap_or("task_terminal")
        ),
        _ => outcome
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("Parent decision request was rejected.")
            .to_string(),
    };
    json!({
        "content": [{"type":"text", "text":text}],
        "isError": status == "rejected",
        "structuredContent": outcome,
    })
}

fn render_reply_delegation_result(outcome: &Value) -> Value {
    let status = match outcome.get("status").and_then(Value::as_str) {
        Some("replied") => "replied",
        Some("idempotent") => "idempotent",
        Some("already_resolved") => "already_resolved",
        Some("missing") => "missing",
        Some("unauthorized") => "unauthorized",
        Some("rejected") => "rejected",
        _ => "rejected",
    };
    let text = match status {
        "replied" => "Reply delivered".to_string(),
        "idempotent" => "Reply already delivered".to_string(),
        "already_resolved" => format!(
            "Request already resolved: {}",
            outcome
                .get("resolution_code")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
        ),
        "missing" => "Decision request was not found.".to_string(),
        "unauthorized" => "Decision request is not owned by this parent.".to_string(),
        _ => outcome
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("Delegation reply was rejected.")
            .to_string(),
    };
    json!({
        "content": [{"type":"text", "text":text}],
        "isError": matches!(
            status,
            "missing" | "unauthorized" | "already_resolved" | "rejected"
        ),
        "structuredContent": outcome,
    })
}

/// Map a serialized [`super::types::DelegationTaskReport`] into MCP `tools/call`
/// result content. Shared by `delegate_to_agent` and `cancel_delegation`, which
/// each resolve to a single report; `get_delegation_status` no longer uses this
/// path — it always renders via [`render_status_result`] / [`render_batch_report`].
/// Kept separate so unit tests can assert the mapping without a real socket.
///
/// The human-readable `content` text is the result for a `completed` task and
/// the `message` (status note / failure reason) otherwise. `isError` is set
/// ONLY for `failed` — `running` (ack), `canceled` (a successful cancel or a
/// canceled task), and `unknown` are all valid tool results the LLM should read
/// rather than treat as errors. The full report rides along in
/// `structuredContent` so the frontend can read `status` + the child ids.
/// Map the `check_user_feedback` round-trip outcome (a `{ count, feedback:[..] }`
/// envelope from the listener) into an MCP `tools/call` result.
///
/// The human-readable `content` text is the steering the LLM acts on: when
/// notes are present it frames them as high-priority user corrections and asks
/// the agent to adjust and acknowledge; when empty it says so plainly. The raw
/// envelope rides along in `structuredContent`. `isError` is always `false` — a
/// successful check with no feedback is a valid result, not an error.
pub fn render_feedback_result(outcome: &Value) -> Value {
    let count = outcome.get("count").and_then(|v| v.as_u64()).unwrap_or(0);
    let text = if count == 0 {
        "No new feedback from the user. Continue with your current plan.".to_string()
    } else {
        let mut s = format!(
            "The user sent {count} message(s) while you were working. Treat this as \
             high-priority steering: adjust your current approach to honor it now, and \
             briefly acknowledge what you changed.\n"
        );
        if let Some(notes) = outcome.get("feedback").and_then(|v| v.as_array()) {
            for (i, note) in notes.iter().enumerate() {
                let body = note.get("text").and_then(|v| v.as_str()).unwrap_or("");
                s.push_str(&format!("{}. {}\n", i + 1, body));
            }
        }
        s
    };
    json!({
        "content": [{ "type": "text", "text": text }],
        "isError": false,
        // Rebuild the structured payload from count + feedback only — the
        // listener's internal `_commit_ids` must not leak to the agent's host.
        "structuredContent": {
            "count": count,
            "feedback": outcome.get("feedback").cloned().unwrap_or_else(|| json!([])),
        },
    })
}

/// Map the `ask_user_question` round-trip outcome (a `{ answers, declined }`
/// envelope from the listener) into an MCP `tools/call` result.
///
/// The human-readable `content` text reports the user's selections per question
/// so the agent can act on them; a declined / empty answer tells the agent to
/// proceed with its own judgment. The raw envelope rides along in
/// `structuredContent`. `isError` is always `false` — a declined question is a
/// valid result, not an error.
pub fn render_ask_result(outcome: &Value) -> Value {
    let declined = outcome
        .get("declined")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let answers = outcome
        .get("answers")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let text = if declined || answers.is_empty() {
        "The user dismissed the question(s) without choosing an answer. Proceed \
         using your best judgment and reasonable defaults."
            .to_string()
    } else {
        let mut s = String::from("The user answered your question(s):\n");
        for (i, a) in answers.iter().enumerate() {
            let header = a.get("header").and_then(|v| v.as_str()).unwrap_or("");
            let question = a.get("question").and_then(|v| v.as_str()).unwrap_or("");
            let selected: Vec<&str> = a
                .get("selected")
                .and_then(|v| v.as_array())
                .map(|arr| arr.iter().filter_map(|x| x.as_str()).collect())
                .unwrap_or_default();
            let joined = if selected.is_empty() {
                "(no selection)".to_string()
            } else {
                selected.join(", ")
            };
            s.push_str(&format!(
                "{}. [{header}] {question}\n   → {joined}\n",
                i + 1
            ));
        }
        s
    };
    json!({
        "content": [{ "type": "text", "text": text }],
        "isError": false,
        "structuredContent": { "answers": answers, "declined": declined },
    })
}

/// Extract the `session_id` integer from the `get_session_info` arguments,
/// tolerating a JSON number (int or whole float) or a numeric string — some MCP
/// hosts stringify integer args. `None` for missing / non-integer / out-of-range,
/// which the dispatcher maps to a synchronous `-32602` the LLM can fix.
fn parse_session_id(arguments: &Value) -> Option<i32> {
    let v = arguments.get("session_id")?;
    if let Some(n) = v.as_i64() {
        return i32::try_from(n).ok();
    }
    if let Some(f) = v.as_f64() {
        if f.fract() == 0.0 && f >= f64::from(i32::MIN) && f <= f64::from(i32::MAX) {
            return Some(f as i32);
        }
    }
    if let Some(s) = v.as_str() {
        return s.trim().parse::<i32>().ok();
    }
    None
}

/// Parse the optional `max_messages` tuning arg robustly: a JSON number (integer
/// or whole non-negative float) or a numeric string — consistent with how
/// `session_id` tolerates stringified ints. Clamps in `u64` space BEFORE narrowing
/// to `u32`, so a huge value (e.g. `4294967296`) saturates to the cap instead of
/// wrapping to a small number. An absent OR unparseable value falls back to the
/// default window — it is an optional knob, not a hard error — while an explicit
/// `0` (or `"0"`) is preserved to mean metadata-only.
fn parse_max_messages(arguments: &Value) -> u32 {
    const DEFAULT_MAX_MESSAGES: u32 = 20;
    let Some(v) = arguments.get("max_messages") else {
        return DEFAULT_MAX_MESSAGES;
    };
    let raw: Option<u64> = if let Some(n) = v.as_u64() {
        Some(n)
    } else if let Some(f) = v.as_f64() {
        // Reject negatives / fractions; `f as u64` saturates a huge float.
        (f.fract() == 0.0 && f >= 0.0).then_some(f as u64)
    } else if let Some(s) = v.as_str() {
        s.trim().parse::<u64>().ok()
    } else {
        None
    };
    match raw {
        Some(n) => n.min(u64::from(MAX_SESSION_MESSAGES)) as u32,
        None => DEFAULT_MAX_MESSAGES,
    }
}

/// Map the `get_session_info` round-trip outcome (a serialized
/// [`crate::acp::session_info::SessionInfo`]) into an MCP `tools/call` result. A
/// not-found result is surfaced as readable text with `isError: false` (the LLM
/// reads it and proceeds), never as a tool error. The full structured envelope
/// rides along in `structuredContent` for hosts that keep it.
pub fn render_session_result(outcome: &Value) -> Value {
    let found = outcome
        .get("found")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let text = if found {
        render_session_summary_text(outcome)
    } else {
        outcome
            .get("note")
            .and_then(|v| v.as_str())
            .unwrap_or("No matching session was found.")
            .to_string()
    };
    json!({
        "content": [{ "type": "text", "text": text }],
        "isError": false,
        "structuredContent": outcome.clone(),
    })
}

/// Build a complete JSON-RPC response for `get_session_info` that fits
/// `max_bytes` (including the trailing newline). Progressive omission:
/// drop oldest messages → shorten newest text on UTF-8 char boundaries →
/// drop tools → drop messages → strip metadata to a bounded envelope.
pub fn render_session_outcome_with_budget(
    id: Value,
    mut outcome: Value,
    max_bytes: usize,
) -> Result<JsonRpcResponse, serde_json::Error> {
    let preferred = ok(id.clone(), render_session_result(&outcome));
    let preferred_bytes = serialize_jsonrpc_line(&preferred)?.len();
    if preferred_bytes <= max_bytes {
        return Ok(preferred);
    }

    // 1) Drop oldest message items until only the newest remains.
    while session_message_items(&outcome)
        .map(|items| items.len() > 1)
        .unwrap_or(false)
    {
        drop_oldest_session_message(&mut outcome);
        let candidate = ok(id.clone(), render_session_result(&outcome));
        if serialize_jsonrpc_line(&candidate)?.len() <= max_bytes {
            tracing::debug!(
                preferred_bytes,
                final_bytes = serialize_jsonrpc_line(&candidate)?.len(),
                "rendered get_session_info by dropping oldest messages"
            );
            return Ok(candidate);
        }
    }

    // 2) UTF-8-safe progressive shortening of the remaining newest text.
    if let Some(best) = shrink_newest_session_text_to_fit(id.clone(), &mut outcome, max_bytes)? {
        return Ok(best);
    }

    // 3) Remove tool names from the remaining item (existing order).
    while session_message_items(&outcome)
        .and_then(|items| items.last())
        .and_then(|item| item.get("tools"))
        .and_then(Value::as_array)
        .map(|tools| !tools.is_empty())
        .unwrap_or(false)
    {
        if let Some(tools) = outcome
            .pointer_mut("/messages/items")
            .and_then(Value::as_array_mut)
            .and_then(|items| items.last_mut())
            .and_then(|item| item.get_mut("tools"))
            .and_then(Value::as_array_mut)
        {
            tools.remove(0);
        }
        mark_session_messages_truncated(&mut outcome);
        let candidate = ok(id.clone(), render_session_result(&outcome));
        if serialize_jsonrpc_line(&candidate)?.len() <= max_bytes {
            return Ok(candidate);
        }
    }

    // 4) Drop the messages envelope; keep full session metadata + transport note.
    if outcome.get("messages").is_some() {
        if let Some(obj) = outcome.as_object_mut() {
            obj.remove("messages");
            obj.insert("note".into(), json!(SESSION_INFO_TRANSPORT_NOTE));
        }
        let candidate = ok(id.clone(), render_session_result(&outcome));
        if serialize_jsonrpc_line(&candidate)?.len() <= max_bytes {
            return Ok(candidate);
        }
    }

    // 5) Bounded metadata-only fallback (found + session_id + counts + note).
    let fallback_outcome = bounded_session_info_fallback(&outcome);
    let fallback = ok(id, render_session_result(&fallback_outcome));
    let fallback_bytes = serialize_jsonrpc_line(&fallback)?.len();
    if fallback_bytes <= max_bytes {
        tracing::debug!(
            preferred_bytes,
            final_bytes = fallback_bytes,
            "rendered get_session_info with bounded metadata fallback"
        );
        return Ok(fallback);
    }

    // Pathological: even the fallback exceeds budget (should not happen with a
    // bounded request id). Return a fixed tiny soft result rather than hang.
    Ok(ok(
        fallback.id,
        json!({
            "content": [{
                "type": "text",
                "text": SESSION_INFO_METADATA_NOTE
            }],
            "isError": false,
            "structuredContent": {
                "found": false,
                "session_id": 0,
                "note": SESSION_INFO_METADATA_NOTE
            }
        }),
    ))
}

fn session_message_items(outcome: &Value) -> Option<&Vec<Value>> {
    outcome.pointer("/messages/items").and_then(Value::as_array)
}

fn mark_session_messages_truncated(outcome: &mut Value) {
    let Some(messages) = outcome.get_mut("messages") else {
        return;
    };
    let included = messages
        .get("items")
        .and_then(Value::as_array)
        .map(|items| items.len() as u64)
        .unwrap_or(0);
    if let Some(obj) = messages.as_object_mut() {
        obj.insert("included".into(), json!(included));
        obj.insert("truncated".into(), json!(true));
    }
}

fn drop_oldest_session_message(outcome: &mut Value) {
    if let Some(items) = outcome
        .pointer_mut("/messages/items")
        .and_then(Value::as_array_mut)
    {
        if !items.is_empty() {
            items.remove(0);
        }
    }
    mark_session_messages_truncated(outcome);
}

/// Binary-search the maximum character prefix of the newest message text that
/// still fits `max_bytes` when fully serialized. Returns `Some` when a fit is
/// found (including empty text with tools still present).
fn shrink_newest_session_text_to_fit(
    id: Value,
    outcome: &mut Value,
    max_bytes: usize,
) -> Result<Option<JsonRpcResponse>, serde_json::Error> {
    let original = outcome
        .pointer("/messages/items")
        .and_then(Value::as_array)
        .and_then(|items| items.last())
        .and_then(|item| item.get("text"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    if original.is_empty() {
        return Ok(None);
    }

    let char_len = original.chars().count();
    let mut lo = 0usize;
    let mut hi = char_len;
    let mut best: Option<(usize, JsonRpcResponse)> = None;

    while lo <= hi {
        let mid = (lo + hi) / 2;
        let candidate_text = if mid >= char_len {
            original.clone()
        } else if mid == 0 {
            String::new()
        } else {
            let mut s: String = original.chars().take(mid).collect();
            s.push('…');
            s
        };
        set_newest_session_text(outcome, &candidate_text);
        mark_session_messages_truncated(outcome);
        let response = ok(id.clone(), render_session_result(outcome));
        let bytes = serialize_jsonrpc_line(&response)?.len();
        if bytes <= max_bytes {
            best = Some((mid, response));
            lo = mid.saturating_add(1);
        } else if mid == 0 {
            break;
        } else {
            hi = mid - 1;
        }
    }

    if let Some((chars, _response)) = best {
        // Restore the winning text into outcome so structuredContent matches.
        let winning = if chars >= char_len {
            original
        } else if chars == 0 {
            String::new()
        } else {
            let mut s: String = original.chars().take(chars).collect();
            s.push('…');
            s
        };
        set_newest_session_text(outcome, &winning);
        mark_session_messages_truncated(outcome);
        return Ok(Some(ok(id, render_session_result(outcome))));
    }

    // Empty text still oversized (tools / metadata) — leave text empty for
    // subsequent tool-stripping steps.
    set_newest_session_text(outcome, "");
    mark_session_messages_truncated(outcome);
    Ok(None)
}

fn set_newest_session_text(outcome: &mut Value, text: &str) {
    if let Some(item) = outcome
        .pointer_mut("/messages/items")
        .and_then(Value::as_array_mut)
        .and_then(|items| items.last_mut())
    {
        if let Some(obj) = item.as_object_mut() {
            obj.insert("text".into(), json!(text));
        }
    }
}

fn bounded_session_info_fallback(outcome: &Value) -> Value {
    let mut out = json!({
        "found": outcome.get("found").cloned().unwrap_or(json!(false)),
        "session_id": outcome.get("session_id").cloned().unwrap_or(json!(0)),
        "note": SESSION_INFO_METADATA_NOTE,
    });
    if let Some(count) = outcome.get("message_count").cloned() {
        out["message_count"] = count;
    }
    // Keep a small stable agent_type when present so the soft text remains useful.
    if let Some(agent) = outcome.get("agent_type").and_then(Value::as_str) {
        if agent.len() <= 64 {
            out["agent_type"] = json!(agent);
        }
    }
    out
}

/// Build the human-readable summary block for a found session: a metadata header
/// plus, when present, a "Recent messages" section.
fn render_session_summary_text(o: &Value) -> String {
    let s = |k: &str| o.get(k).and_then(|v| v.as_str());
    let id = o.get("session_id").and_then(|v| v.as_i64()).unwrap_or(0);
    let agent = s("agent_type").unwrap_or("unknown");
    let mut out = format!("Session #{id} ({agent})\n");
    if let Some(t) = s("title") {
        out.push_str(&format!("Title: {t}\n"));
    }
    let mut meta: Vec<String> = Vec::new();
    if let Some(v) = s("status") {
        meta.push(format!("status: {v}"));
    }
    if let Some(v) = s("git_branch") {
        meta.push(format!("branch: {v}"));
    }
    if let Some(v) = s("model") {
        meta.push(format!("model: {v}"));
    }
    if !meta.is_empty() {
        out.push_str(&meta.join(" | "));
        out.push('\n');
    }
    if let Some(v) = s("workspace_path") {
        out.push_str(&format!("Workspace: {v}\n"));
    }
    if let Some(n) = o.get("message_count").and_then(|v| v.as_u64()) {
        out.push_str(&format!("Messages: {n}\n"));
    }
    if o.get("is_delegation_child")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        if let Some(p) = o.get("parent_id").and_then(|v| v.as_i64()) {
            out.push_str(&format!("Delegation child of session #{p}\n"));
        }
    }
    if let Some(tokens) = o
        .get("stats")
        .and_then(|st| st.get("total_tokens"))
        .and_then(|v| v.as_u64())
    {
        out.push_str(&format!("Total tokens: {tokens}\n"));
    }
    if let Some(note) = s("note") {
        out.push_str(&format!("Note: {note}\n"));
    }
    if let Some(messages) = o.get("messages") {
        let total = messages.get("total").and_then(|v| v.as_u64()).unwrap_or(0);
        let included = messages
            .get("included")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let truncated = messages
            .get("truncated")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let suffix = if truncated {
            ", older turns omitted"
        } else {
            ""
        };
        out.push_str(&format!(
            "\nRecent messages ({included}/{total}{suffix}):\n"
        ));
        if let Some(items) = messages.get("items").and_then(|v| v.as_array()) {
            for item in items {
                let role = item.get("role").and_then(|v| v.as_str()).unwrap_or("?");
                let body = item.get("text").and_then(|v| v.as_str()).unwrap_or("");
                let tools: Vec<&str> = item
                    .get("tools")
                    .and_then(|v| v.as_array())
                    .map(|a| a.iter().filter_map(|x| x.as_str()).collect())
                    .unwrap_or_default();
                out.push_str(&format!("- [{role}] {body}"));
                if !tools.is_empty() {
                    out.push_str(&format!(" (tools: {})", tools.join(", ")));
                }
                out.push('\n');
            }
        }
    }
    out
}

pub fn render_task_report(report: &Value) -> Value {
    let status = report.get("status").and_then(|v| v.as_str()).unwrap_or("");
    let is_error = status == "failed";
    let report_str = |key: &str| {
        report
            .get(key)
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
    };
    let text = if status == "completed" {
        // Prefer the result text; fall back to `message` so the DB-fallback note
        // ("Result no longer cached; open child session N…") for an evicted
        // result isn't rendered as empty content.
        report_str("text")
            .or_else(|| report_str("message"))
            .unwrap_or("")
            .to_string()
    } else {
        report_str("message")
            .or_else(|| report_str("text"))
            .unwrap_or("")
            .to_string()
    };
    json!({
        "content": [{ "type": "text", "text": text }],
        "isError": is_error,
        "structuredContent": report.clone(),
    })
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, HashSet};

    use super::*;
    use crate::acp::delegation::types::OrchestrationBindingV1;
    use crate::acp::delegation::workflow::{
        project_workflow_state_index, WorkflowIndexOmissionStep, WorkflowStateDto,
        WorkflowStateIndexDto,
    };

    fn ctx() -> CompanionContext {
        // Delegation-only by default so the existing delegation-focused tests
        // keep seeing exactly the three delegation tools.
        ctx_with(CompanionFeatures {
            delegation: true,
            coordination_v1: false,
            feedback: false,
            ask: false,
            sessions: false,
            workflow_v2: false,
            completion_v2: false,
        })
    }

    fn ctx_with(features: CompanionFeatures) -> CompanionContext {
        CompanionContext {
            parent_connection_id: "p1".into(),
            socket_path: "/tmp/codeg-mcp-companion-test-nope.sock".into(),
            token: "tok".into(),
            features,
            role: CompanionRole::Root,
            can_spawn_child: true,
            connection_incarnation_id: "test-incarnation".into(),
            disabled_agents: Vec::new(),
        }
    }

    async fn dispatch_for_test(line: &str) -> LineAction {
        dispatch_line(&ctx(), Arc::new(InflightCalls::new()), line).await
    }

    async fn dispatch_with_features(features: CompanionFeatures, line: &str) -> LineAction {
        dispatch_line(&ctx_with(features), Arc::new(InflightCalls::new()), line).await
    }

    fn unwrap_respond(action: LineAction) -> JsonRpcResponse {
        match action {
            LineAction::Respond(r) => r,
            LineAction::Spawn(_) => panic!("expected Respond, got Spawn"),
            LineAction::Silent => panic!("expected Respond, got Silent"),
        }
    }

    fn ascii_string_id_with_serialized_len(bytes: usize) -> Value {
        Value::String("x".repeat(bytes - 2))
    }

    fn representative_large_index() -> WorkflowStateIndexDto {
        let nodes = (0..20)
            .map(|index| {
                let node_id = match index {
                    0 => "plan-reviewer-quote\"slash\\界".to_string(),
                    1 => "plan-reviewer-grok-界".to_string(),
                    2 => "task-1-impl-界".to_string(),
                    3 => "task-1-review-quote\"".to_string(),
                    _ => format!("node-{index:02}-quote\"slash\\界"),
                };
                let required_for_gate = index < 2;
                let (task_index, status) = match index {
                    2 => (Some(1), "running"),
                    3 => (Some(1), "pending"),
                    4 => (None, "running"),
                    _ => (None, "completed"),
                };
                json!({
                    "node_id": node_id,
                    "work_unit_key": format!("task|{index}|reviewer|quote\\\"界|none"),
                    "role": if index == 2 { "implementer" } else { "reviewer" },
                    "agent_type": if index % 2 == 0 { "codex" } else { "grok" },
                    "profile_id": format!("private-profile-{index}"),
                    "phase_id": if task_index.is_some() { "tasks" } else { "plan" },
                    "task_index": task_index,
                    "is_observed": true,
                    "retained_observed": false,
                    "cohort_frozen": true,
                    "latest_task_id": format!("task-{index:02}-quote\"slash\\界"),
                    "latest_status": status,
                    "latest_generation": 7,
                    "summary_validated": true,
                    "artifact_digest": format!("sha256:{}", "abcdef0123456789".repeat(4)),
                    "child_conversation_id": 900 + index,
                    "reviewed_task_id": "private-reviewed-task",
                    "verdict": "done",
                    "report_file": format!("reports/界/quote\"slash\\node-{index:02}.md"),
                    "replaced_task_id": "private-replaced-task",
                    "required_for_gate": required_for_gate
                })
            })
            .collect::<Vec<_>>();
        let findings = (0..15)
            .map(|index| {
                json!({
                    "finding_id": format!("finding-{index:02}-quote\"slash\\界"),
                    "severity": match index % 3 { 0 => "critical", 1 => "important", _ => "minor" },
                    "status": match index % 4 { 0 => "open", 1 => "new", 2 => "reopened", _ => "resolved" },
                    "owner_reviewer_node_ids": ["plan-reviewer-quote\"slash\\界"],
                    "summary": "S".repeat(4 * 1024),
                    "evidence_ref": format!("docs/界/plan.md#finding-{index:02}-quote\""),
                    "report_file": format!("reports/界/finding-{index:02}-quote\"slash\\.md")
                })
            })
            .collect::<Vec<_>>();
        let state: WorkflowStateDto = serde_json::from_value(json!({
            "workflow_id": "wf-1",
            "parent_conversation_id": 42,
            "workflow_kind": "brainstorm_to_delivery",
            "capability_version": "workflow_manifest_v2",
            "workflow_state": "estimated",
            "manifest_revision": 7,
            "graph_revision": 11,
            "schema_version": 2,
            "publication_token": "publication-quote\"slash\\界",
            "plan_target_rel_path": "docs/界/plan-quote\"slash\\.md",
            "risk_policy_version": "b2d_task_risk_v1",
            "completion_protocol": {
                "version": 2,
                "mode": "v2_enforce",
                "creation_mode": "v2_enforce",
                "automatic_root_wake": false
            },
            "completion": {
                "protocol_version": 2,
                "graph_revision": 11,
                "card": {
                    "state": "needs_decision",
                    "role": "reviewer",
                    "summary": "C".repeat(1024),
                    "source": "assistant_conclusion",
                    "evidence_validated": false,
                    "attention": {
                        "attention_id": "attention-budget",
                        "task_id": "task-budget",
                        "kind": "completion_decision",
                        "captured_scope_digest": format!("sha256:{}", "a".repeat(64)),
                        "latest_run_id": "task-budget",
                        "node_id": "plan-reviewer-quote\"slash\\界"
                    }
                }
            },
            "task_policies": [{
                "task_index": 1,
                "risk": {
                    "level": "high",
                    "hard_triggers": [],
                    "soft_signals": [],
                    "score": 3,
                    "reason": "private risk prose"
                },
                "route": {
                    "implementer_node_id": "task-1-impl-界",
                    "reviewer_node_ids": ["task-1-review-quote\""]
                }
            }],
            "design": {
                "rel_path": "docs/界/design-quote\"slash\\.md",
                "digest": format!("sha256:{}", "0123456789abcdef".repeat(4))
            },
            "plan": {
                "rel_path": "docs/界/plan-quote\"slash\\.md",
                "digest": format!("sha256:{}", "fedcba9876543210".repeat(4))
            },
            "nodes": nodes,
            "gates": [{
                "gate_id": "plan-gate-quote\"slash\\界",
                "gate_kind": "plan",
                "resolution_mode": "parent_adjudication",
                "reviewer_cohort_node_ids": [
                    "plan-reviewer-quote\"slash\\界",
                    "plan-reviewer-grok-界"
                ],
                "required_reviewer_node_ids": [
                    "plan-reviewer-quote\"slash\\界",
                    "plan-reviewer-grok-界"
                ],
                "latest_gate_cycle": 2,
                "latest_outcome": "changes_requested",
                "next_gate_cycle": 3
            }],
            "latest_plan_review": {
                "scope": "full",
                "revision_kind": "material",
                "scope_reason": "private scope prose",
                "covered_author_task_id": "plan-author-task-quote\"slash\\界",
                "covered_plan_digest": format!("sha256:{}", "13579bdf02468ace".repeat(4)),
                "reviewed_reviewer_node_ids": [
                    "plan-reviewer-quote\"slash\\界",
                    "plan-reviewer-grok-界"
                ],
                "next_required_reviewer_node_ids": ["plan-reviewer-grok-界"],
                "findings": findings,
                "lineage_reset_reason": "private lineage prose",
                "critical_count": 5,
                "important_count": 8,
                "minor_count": 2,
                "net_improvement": true,
                "stagnation_count": 0,
                "rewrite_used": false,
                "next_action": "continue_review"
            },
            "evidence_truncated": false
        }))
        .expect("representative source state");
        assert_eq!(state.nodes.len(), 20);
        assert_eq!(
            state.latest_plan_review.as_ref().unwrap().findings.len(),
            15
        );
        project_workflow_state_index(
            state,
            &HashSet::from([
                "task-1-impl-界".to_string(),
                "task-1-review-quote\"".to_string(),
            ]),
            &BTreeMap::from([(1, false)]),
        )
    }

    fn response_index(response: &JsonRpcResponse) -> Value {
        let result = response.result.as_ref().expect("tool result");
        serde_json::from_str(result["content"][0]["text"].as_str().expect("index text"))
            .expect("valid projected index JSON")
    }

    fn omission_candidate(index: &WorkflowStateIndexDto, id: Value) -> JsonRpcResponse {
        render_get_workflow_state_response(id, index.clone())
    }

    #[cfg(windows)]
    fn workflow_broker_with_outcome(outcome: Value) -> (String, tokio::task::JoinHandle<()>) {
        use crate::acp::delegation::transport::{
            read_frame, write_frame, BrokerMessage, BrokerResponse,
        };
        use tokio::net::windows::named_pipe::ServerOptions;

        let pipe_name = format!(
            r"\\.\pipe\codeg-workflow-budget-test-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        );
        let mut server = ServerOptions::new()
            .first_pipe_instance(true)
            .create(&pipe_name)
            .unwrap();
        let task = tokio::spawn(async move {
            server.connect().await.unwrap();
            let message: BrokerMessage = read_frame(&mut server).await.unwrap();
            assert!(matches!(message, BrokerMessage::GetWorkflowState(_)));
            write_frame(&mut server, &BrokerResponse { outcome })
                .await
                .unwrap();
        });
        (pipe_name, task)
    }

    #[cfg(unix)]
    fn workflow_broker_with_outcome(outcome: Value) -> (String, tokio::task::JoinHandle<()>) {
        use crate::acp::delegation::transport::{
            read_frame, write_frame, BrokerMessage, BrokerResponse,
        };
        use tokio::net::UnixListener;

        let dir = tempfile::tempdir().unwrap();
        let socket_path = dir.path().join("workflow-budget.sock");
        let socket = socket_path.to_string_lossy().to_string();
        let listener = UnixListener::bind(&socket_path).unwrap();
        let task = tokio::spawn(async move {
            let _dir = dir;
            let (mut stream, _) = listener.accept().await.unwrap();
            let message: BrokerMessage = read_frame(&mut stream).await.unwrap();
            assert!(matches!(message, BrokerMessage::GetWorkflowState(_)));
            write_frame(&mut stream, &BrokerResponse { outcome })
                .await
                .unwrap();
        });
        (socket, task)
    }

    #[cfg(windows)]
    fn orchestration_broker_with_outcome(outcome: Value) -> (String, tokio::task::JoinHandle<()>) {
        use crate::acp::delegation::transport::{
            read_frame, write_frame, BrokerMessage, BrokerResponse,
        };
        use tokio::net::windows::named_pipe::ServerOptions;

        let pipe_name = format!(
            r"\\.\pipe\codeg-binding-query-test-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        );
        let mut server = ServerOptions::new()
            .first_pipe_instance(true)
            .create(&pipe_name)
            .unwrap();
        let task = tokio::spawn(async move {
            server.connect().await.unwrap();
            let message: BrokerMessage = read_frame(&mut server).await.unwrap();
            let BrokerMessage::OrchestrationBindings(request) = message else {
                panic!("expected orchestration binding query")
            };
            assert_eq!(request.token, "tok");
            assert_eq!(request.namespace, "brainstorm-to-delivery");
            write_frame(&mut server, &BrokerResponse { outcome })
                .await
                .unwrap();
        });
        (pipe_name, task)
    }

    #[cfg(unix)]
    fn orchestration_broker_with_outcome(outcome: Value) -> (String, tokio::task::JoinHandle<()>) {
        use crate::acp::delegation::transport::{
            read_frame, write_frame, BrokerMessage, BrokerResponse,
        };
        use tokio::net::UnixListener;

        let dir = tempfile::tempdir().unwrap();
        let socket_path = dir.path().join("binding-query.sock");
        let socket = socket_path.to_string_lossy().to_string();
        let listener = UnixListener::bind(&socket_path).unwrap();
        let task = tokio::spawn(async move {
            let _dir = dir;
            let (mut stream, _) = listener.accept().await.unwrap();
            let message: BrokerMessage = read_frame(&mut stream).await.unwrap();
            let BrokerMessage::OrchestrationBindings(request) = message else {
                panic!("expected orchestration binding query")
            };
            assert_eq!(request.token, "tok");
            assert_eq!(request.namespace, "brainstorm-to-delivery");
            write_frame(&mut stream, &BrokerResponse { outcome })
                .await
                .unwrap();
        });
        (socket, task)
    }

    fn large_orchestration_binding_page() -> Value {
        let runs = (0..20)
            .map(|index| {
                let task_id = format!("task-{index:03}-{}", "t".repeat(32));
                json!({
                    "task_id": task_id,
                    "root_task_id": format!("root-{index:03}-{}", "r".repeat(32)),
                    "previous_task_id": Value::Null,
                    "lineage_root_task_id": format!("lineage-{index:03}-{}", "l".repeat(32)),
                    "replaced_task_id": Value::Null,
                    "replacement_reason": Value::Null,
                    "generic_generation": index + 1,
                    "work_unit_key": format!("work-unit-{index:03}-{}", "w".repeat(80)),
                    "child_conversation_id": 1000 + index,
                    "agent_type": "grok",
                    "profile_id": format!("profile-{index:03}-{}", "p".repeat(32)),
                    "status": "completed",
                    "orchestration_binding": {
                        "schema_version": 1,
                        "namespace": "brainstorm-to-delivery",
                        "generation": index + 1,
                        "route_fingerprint": format!("sha256:{}", "a".repeat(64))
                    }
                })
            })
            .collect::<Vec<_>>();
        json!({
            "schema_version": 1,
            "namespace": "brainstorm-to-delivery",
            "snapshot_id": "1a641e16-36f4-4ec5-aa4f-18d18e6ab107",
            "snapshot_revision": "17",
            "snapshot_created_at": "2026-08-17T08:00:00Z",
            "snapshot_expires_at": "2026-08-17T08:01:00Z",
            "total_rows": 25,
            "page_start": 5,
            "request_cursor": "request-cursor-opaque",
            "runs": runs,
            "next_cursor": Value::Null,
            "complete": true
        })
    }

    #[test]
    fn delegation_admission_context_phase0_session_4123_frozen_baseline() {
        let fixture: Value = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/delegation_binding_session_4123.json"
        )))
        .unwrap();
        let fixture = fixture.as_object().unwrap();
        assert_eq!(fixture.len(), 5);
        assert!([
            "schema_version",
            "session_id",
            "baseline",
            "rows",
            "expected_sequence",
        ]
        .iter()
        .all(|key| fixture.contains_key(*key)));
        assert_eq!(fixture["schema_version"], 1);
        assert_eq!(fixture["session_id"], 4123);

        let baseline = fixture["baseline"].as_object().unwrap();
        assert_eq!(baseline.len(), 6);
        assert_eq!(baseline["first_page_calls"], 125);
        assert_eq!(baseline["continuation_page_calls"], 670);
        assert_eq!(baseline["total_binding_query_calls"], 795);
        assert_eq!(baseline["model_visible_result_bytes"], 3_040_870);
        assert_eq!(baseline["final_row_count"], 72);
        assert_eq!(baseline["work_unit_key_count"], 34);

        let rows = fixture["rows"].as_array().unwrap();
        assert_eq!(rows.len(), 72);
        let identity_fields = [
            "agent_type",
            "child_conversation_id",
            "generic_generation",
            "lineage_root_task_id",
            "orchestration_binding",
            "previous_task_id",
            "profile_id",
            "replaced_task_id",
            "replacement_reason",
            "root_task_id",
            "status",
            "task_id",
            "work_unit_key",
        ];
        let mut work_unit_keys = HashSet::new();
        for row in rows {
            let row = row.as_object().unwrap();
            assert_eq!(row.len(), identity_fields.len());
            assert!(identity_fields.iter().all(|field| row.contains_key(*field)));
            for field in ["task_id", "root_task_id", "lineage_root_task_id"] {
                uuid::Uuid::parse_str(row[field].as_str().unwrap()).unwrap();
            }
            work_unit_keys.insert(row["work_unit_key"].as_str().unwrap());
        }
        assert_eq!(work_unit_keys.len(), 34);

        let expected_sequence = json!([
            { "label": "selected_identity_rows", "value": 72 },
            { "label": "legacy_model_visible_first_page_calls", "value": 125 },
            { "label": "artifact_model_visible_calls", "value": 125 },
            { "label": "legacy_model_visible_continuation_page_calls", "value": 670 },
            { "label": "artifact_model_visible_continuation_page_calls", "value": 0 },
            { "label": "legacy_total_binding_query_calls", "value": 795 },
            { "label": "legacy_model_visible_binding_result_jsonl_utf8_bytes", "value": 3_040_870 },
            { "label": "artifact_aggregate_result_jsonl_utf8_bytes_max", "value": 262_144 },
            { "label": "artifact_reduction_percent_min", "value": 90 },
            { "label": "selected_identity_order", "value": "unchanged" },
            { "label": "validator_reconciliation_decisions", "value": "unchanged" },
            { "label": "dispatch_labels", "value": "unchanged" }
        ]);
        assert_eq!(fixture["expected_sequence"], expected_sequence);

        let page: DelegationOrchestrationBindingPage = serde_json::from_value(json!({
            "schema_version": 1,
            "namespace": "brainstorm-to-delivery",
            "snapshot_id": "00000000-0000-4000-8000-000000004123",
            "snapshot_revision": "0",
            "snapshot_created_at": "2026-08-26T08:00:00Z",
            "snapshot_expires_at": "2026-08-26T08:01:00Z",
            "total_rows": 72,
            "page_start": 0,
            "request_cursor": null,
            "runs": rows,
            "next_cursor": null,
            "complete": true
        }))
        .unwrap();
        let id = json!("session-4123-baseline");
        let response = ok(
            id.clone(),
            render_orchestration_binding_page(&serde_json::to_value(&page).unwrap()),
        );
        assert!(
            serialize_jsonrpc_line(&response).unwrap().len()
                > GET_ORCHESTRATION_BINDINGS_MAX_RESULT_BYTES
        );
        let fitting = largest_fitting_orchestration_binding_page_limit(
            &id,
            &page,
            GET_ORCHESTRATION_BINDINGS_MAX_RESULT_BYTES,
        )
        .unwrap()
        .unwrap();
        assert!((1..72).contains(&usize::from(fitting)));
    }

    fn transport_sized_orchestration_page(page: &Value, page_limit: usize) -> Value {
        let mut bounded = page.clone();
        let runs = bounded["runs"].as_array_mut().unwrap();
        assert!((1..runs.len()).contains(&page_limit));
        runs.truncate(page_limit);
        bounded["next_cursor"] = json!("transport-page-cursorx");
        bounded["complete"] = json!(false);
        bounded
    }

    #[cfg(windows)]
    fn orchestration_broker_with_adaptive_page(
        page: Value,
    ) -> (String, tokio::task::JoinHandle<()>) {
        use crate::acp::delegation::transport::{
            read_frame, write_frame, BrokerMessage, BrokerResponse,
        };
        use tokio::net::windows::named_pipe::ServerOptions;

        let pipe_name = format!(
            r"\\.\pipe\codeg-binding-budget-test-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        );
        let mut first_server = ServerOptions::new()
            .first_pipe_instance(true)
            .create(&pipe_name)
            .unwrap();
        let pipe_name_for_task = pipe_name.clone();
        let task = tokio::spawn(async move {
            first_server.connect().await.unwrap();
            let mut second_server = ServerOptions::new().create(&pipe_name_for_task).unwrap();
            let first: BrokerMessage = read_frame(&mut first_server).await.unwrap();
            let first_wire = serde_json::to_value(&first).unwrap();
            assert!(first_wire.get("page_limit").is_none());
            write_frame(
                &mut first_server,
                &BrokerResponse {
                    outcome: page.clone(),
                },
            )
            .await
            .unwrap();

            second_server.connect().await.unwrap();
            let second: BrokerMessage = read_frame(&mut second_server).await.unwrap();
            let second_wire = serde_json::to_value(&second).unwrap();
            let page_limit = second_wire["page_limit"]
                .as_u64()
                .expect("oversized page must be retried with a private page limit")
                as usize;
            assert_eq!(second_wire["limit"], 100);
            assert_eq!(second_wire["snapshot_id"], first_wire["snapshot_id"]);
            assert_eq!(second_wire["cursor"], first_wire["cursor"]);
            let outcome = transport_sized_orchestration_page(&page, page_limit);
            write_frame(&mut second_server, &BrokerResponse { outcome })
                .await
                .unwrap();
        });
        (pipe_name, task)
    }

    #[cfg(unix)]
    fn orchestration_broker_with_adaptive_page(
        page: Value,
    ) -> (String, tokio::task::JoinHandle<()>) {
        use crate::acp::delegation::transport::{
            read_frame, write_frame, BrokerMessage, BrokerResponse,
        };
        use tokio::net::UnixListener;

        let dir = tempfile::tempdir().unwrap();
        let socket_path = dir.path().join("binding-budget.sock");
        let socket = socket_path.to_string_lossy().to_string();
        let listener = UnixListener::bind(&socket_path).unwrap();
        let task = tokio::spawn(async move {
            let _dir = dir;
            let (mut first_stream, _) = listener.accept().await.unwrap();
            let first: BrokerMessage = read_frame(&mut first_stream).await.unwrap();
            let first_wire = serde_json::to_value(&first).unwrap();
            assert!(first_wire.get("page_limit").is_none());
            write_frame(
                &mut first_stream,
                &BrokerResponse {
                    outcome: page.clone(),
                },
            )
            .await
            .unwrap();

            let (mut second_stream, _) = listener.accept().await.unwrap();
            let second: BrokerMessage = read_frame(&mut second_stream).await.unwrap();
            let second_wire = serde_json::to_value(&second).unwrap();
            let page_limit = second_wire["page_limit"]
                .as_u64()
                .expect("oversized page must be retried with a private page limit")
                as usize;
            assert_eq!(second_wire["limit"], 100);
            assert_eq!(second_wire["snapshot_id"], first_wire["snapshot_id"]);
            assert_eq!(second_wire["cursor"], first_wire["cursor"]);
            let outcome = transport_sized_orchestration_page(&page, page_limit);
            write_frame(&mut second_stream, &BrokerResponse { outcome })
                .await
                .unwrap();
        });
        (socket, task)
    }

    #[tokio::test]
    async fn initialize_returns_protocol_version() {
        let line = r#"{"jsonrpc":"2.0","id":1,"method":"initialize"}"#;
        let resp = unwrap_respond(dispatch_for_test(line).await);
        let result = resp.result.unwrap();
        assert_eq!(result["protocolVersion"], "2024-11-05");
        assert_eq!(result["serverInfo"]["name"], "codeg-mcp");
    }

    #[tokio::test]
    async fn tools_list_exposes_continue_and_replacement_inputs() {
        let line = r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#;
        let resp = unwrap_respond(dispatch_for_test(line).await);
        let result = resp.result.unwrap();
        let tools = result["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 5);
        let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert!(names.contains(&"delegate_to_agent"));
        assert!(names.contains(&"register_simple_workflow"));
        assert!(names.contains(&"continue_delegation"));
        assert!(names.contains(&"get_delegation_status"));
        assert!(names.contains(&"cancel_delegation"));
        // delegate_to_agent schema still enumerates all supported agent types.

        let delegate = tools
            .iter()
            .find(|t| t["name"] == "delegate_to_agent")
            .unwrap();
        let agents = delegate["inputSchema"]["properties"]["agent_type"]["enum"]
            .as_array()
            .unwrap();
        let agent_slugs: Vec<&str> = agents.iter().filter_map(Value::as_str).collect();
        assert_eq!(
            agent_slugs,
            vec![
                "claude_code",
                "codex",
                "open_code",
                "gemini",
                "cline",
                "hermes",
                "code_buddy",
                "kimi_code",
                "pi",
                "grok",
                "cursor",
                "deepseek",
            ]
        );
        assert!(delegate["inputSchema"]["properties"]["profile_id"].is_object());
        assert!(!delegate["inputSchema"]["required"]
            .as_array()
            .unwrap()
            .iter()
            .any(|value| value == "profile_id"));
        // Optional orchestration key for concurrent first-dispatch fencing.
        let work_unit = &delegate["inputSchema"]["properties"]["work_unit_key"];
        assert!(work_unit.is_object());
        assert!(work_unit.get("type").is_none());
        assert_eq!(work_unit["maxLength"], 200);
        assert!(!delegate["inputSchema"]["required"]
            .as_array()
            .unwrap()
            .iter()
            .any(|value| value == "work_unit_key"));
        for name in ["replaces_task_id", "replacement_reason"] {
            assert!(delegate["inputSchema"]["properties"][name].is_object());
            assert!(!delegate["inputSchema"]["required"]
                .as_array()
                .unwrap()
                .iter()
                .any(|value| value == name));
        }
        let reason_enum = delegate["inputSchema"]["properties"]["replacement_reason"]["enum"]
            .as_array()
            .expect("replacement_reason enum");
        for expected in [
            "unresumable",
            "budget_exhausted_continue",
            "not_supported",
            "admission_failed",
            "admission_unknown",
        ] {
            assert!(
                reason_enum.iter().any(|v| v == expected),
                "replacement_reason enum missing {expected}: {reason_enum:?}"
            );
        }
        let reason_desc = delegate["inputSchema"]["properties"]["replacement_reason"]
            ["description"]
            .as_str()
            .unwrap();
        assert_eq!(reason_desc, "");
        let delegate_desc = delegate["description"]
            .as_str()
            .unwrap_or("")
            .to_ascii_lowercase();
        assert!(
            delegate_desc.contains("admission_failed")
                && delegate_desc.contains("admission_unknown")
                && (delegate_desc.contains("explicit replacement")
                    || delegate_desc.contains("replaces_task_id")),
            "delegate_to_agent description must document admission recovery via replacement: {delegate_desc}"
        );
        // correlation_id is required on both delegation entry points (fresh per
        // invocation; server still accepts legacy missing when host tool id present).
        let corr = &delegate["inputSchema"]["properties"]["correlation_id"];
        assert!(corr.is_object());
        assert!(corr.as_object().unwrap().is_empty());
        assert!(delegate["inputSchema"]["required"]
            .as_array()
            .unwrap()
            .iter()
            .any(|value| value == "correlation_id"));
        let continue_tool = tools
            .iter()
            .find(|t| t["name"] == "continue_delegation")
            .expect("continue_delegation must be exposed");
        let continue_props = &continue_tool["inputSchema"]["properties"];
        assert!(continue_props["task_id"].is_object());
        assert!(continue_props["task"].is_object());
        assert!(continue_props["agent_type"].is_null());
        assert!(continue_props["profile_id"].is_null());
        assert!(continue_props["working_dir"].is_null());
        let continue_corr = &continue_props["correlation_id"];
        assert!(continue_corr.is_object());
        assert!(continue_corr.as_object().unwrap().is_empty());
        let continue_required = continue_tool["inputSchema"]["required"].as_array().unwrap();
        assert!(continue_required.iter().any(|value| value == "task_id"));
        assert!(continue_required.iter().any(|value| value == "task"));
        assert!(continue_required
            .iter()
            .any(|value| value == "correlation_id"));
        // get_delegation_status takes a single id param — task_ids (required) —
        // plus wait_ms. The legacy single `task_id` param is gone.
        let status = tools
            .iter()
            .find(|t| t["name"] == "get_delegation_status")
            .unwrap();
        assert!(status["inputSchema"]["properties"]["task_id"].is_null());
        assert!(status["inputSchema"]["properties"]["task_ids"].is_object());
        assert!(status["inputSchema"]["properties"]["wait_ms"].is_object());
        let required = status["inputSchema"]["required"].as_array().unwrap();
        assert!(required.iter().any(|v| v == "task_ids"));
        let cancel = tools
            .iter()
            .find(|t| t["name"] == "cancel_delegation")
            .unwrap();
        let cancel_required = cancel["inputSchema"]["required"].as_array().unwrap();
        assert!(cancel_required.iter().any(|v| v == "task_id"));
        assert!(cancel_required.iter().any(|v| v == "reason"));
        assert_eq!(
            cancel["inputSchema"]["properties"]["reason"]["enum"],
            json!(["timeout", "taskfail", "usercancel", "others"])
        );
    }

    #[tokio::test]
    async fn disabled_builtins_narrow_the_closed_delegate_enum() {
        let mut context = ctx();
        context.disabled_agents = vec![
            "codex".into(),
            "grok".into(),
            "custom:disabled-agent".into(),
            "not-an-agent".into(),
        ];

        let response = unwrap_respond(
            dispatch_line(
                &context,
                Arc::new(InflightCalls::new()),
                r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
            )
            .await,
        );
        let tools = response.result.unwrap()["tools"].clone();
        let agents = tools
            .as_array()
            .unwrap()
            .iter()
            .find(|tool| tool["name"] == "delegate_to_agent")
            .unwrap()["inputSchema"]["properties"]["agent_type"]["enum"]
            .as_array()
            .unwrap();

        assert_eq!(agents.len(), 10);
        assert!(!agents.iter().any(|agent| agent == "codex"));
        assert!(!agents.iter().any(|agent| agent == "grok"));
        assert!(agents.iter().any(|agent| agent == "code_buddy"));
        assert!(agents.iter().any(|agent| agent == "deepseek"));
        assert!(!agents.iter().any(|agent| {
            agent
                .as_str()
                .is_some_and(|slug| slug.starts_with("custom:"))
        }));
    }

    #[tokio::test]
    async fn empty_disabled_list_serves_the_embedded_builtin_enum_unchanged() {
        let line = r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#;
        let resp = unwrap_respond(dispatch_for_test(line).await);
        let tools = resp.result.unwrap()["tools"].clone();
        let delegate = tools
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["name"] == "delegate_to_agent")
            .cloned()
            .unwrap();
        let agents = delegate["inputSchema"]["properties"]["agent_type"]["enum"]
            .as_array()
            .unwrap()
            .clone();
        assert_eq!(agents.len(), 12);
        assert_eq!(agents[0], "claude_code");
        assert_eq!(agents[11], "deepseek");
    }

    #[tokio::test]
    async fn get_delegation_status_without_task_ids_rejected() {
        let line = r#"{
            "jsonrpc":"2.0",
            "id":11,
            "method":"tools/call",
            "params": { "name": "get_delegation_status", "arguments": {} }
        }"#;
        let resp = unwrap_respond(dispatch_for_test(line).await);
        let e = resp.error.unwrap();
        assert_eq!(e.code, -32602);
        assert!(e.message.contains("task_ids"));
    }

    #[tokio::test]
    async fn cancel_delegation_without_reason_rejected() {
        let line = r#"{
            "jsonrpc":"2.0",
            "id":12,
            "method":"tools/call",
            "params": { "name": "cancel_delegation", "arguments": { "task_id": "abc" } }
        }"#;
        let resp = unwrap_respond(dispatch_for_test(line).await);
        let e = resp.error.unwrap();
        assert_eq!(e.code, -32602);
        assert!(e.message.contains("reason"));
    }

    #[tokio::test]
    async fn cancel_delegation_rejects_invalid_reason() {
        let line = r#"{
            "jsonrpc":"2.0",
            "id":13,
            "method":"tools/call",
            "params": {
                "name": "cancel_delegation",
                "arguments": { "task_id": "abc", "reason": "slow" }
            }
        }"#;
        let resp = unwrap_respond(dispatch_for_test(line).await);
        let e = resp.error.unwrap();
        assert_eq!(e.code, -32602);
        assert!(e.message.contains("reason"));
        assert!(e.message.contains("timeout"));
    }

    #[tokio::test]
    async fn cancel_delegation_timeout_reason_returns_guidance_without_spawning() {
        let inflight = Arc::new(InflightCalls::new());
        let line = r#"{
            "jsonrpc":"2.0",
            "id":14,
            "method":"tools/call",
            "params": {
                "name": "cancel_delegation",
                "arguments": { "task_id": "abc", "reason": "timeout" }
            }
        }"#;
        let resp = unwrap_respond(dispatch_line(&ctx(), inflight.clone(), line).await);
        assert!(resp.error.is_none());
        let result = resp.result.unwrap();
        assert_eq!(
            result["content"][0]["text"],
            crate::acp::delegation::types::TIMEOUT_CANCEL_GUIDANCE
        );
        assert_eq!(result["isError"], false);
        assert_eq!(result["structuredContent"]["status"], "running");
        assert_eq!(inflight.inner.lock().await.len(), 0);
    }

    #[tokio::test]
    async fn notifications_initialized_produces_no_response() {
        let line = r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#;
        let action = dispatch_for_test(line).await;
        assert!(matches!(action, LineAction::Silent));
    }

    #[tokio::test]
    async fn parse_error_returns_null_id_error() {
        let line = "not json";
        let resp = unwrap_respond(dispatch_for_test(line).await);
        let e = resp.error.unwrap();
        assert_eq!(e.code, -32700);
        assert!(e.message.contains("parse"));
        assert_eq!(resp.id, Value::Null);
    }

    #[tokio::test]
    async fn unknown_method_returns_32601() {
        let line = r#"{"jsonrpc":"2.0","id":9,"method":"resources/list"}"#;
        let resp = unwrap_respond(dispatch_for_test(line).await);
        let e = resp.error.unwrap();
        assert_eq!(e.code, -32601);
    }

    #[tokio::test]
    async fn tools_call_with_unknown_tool_rejected_synchronously() {
        let line = r#"{
            "jsonrpc":"2.0",
            "id":3,
            "method":"tools/call",
            "params": {
                "name": "other_tool",
                "arguments": {},
                "_meta": {"tool_use_id": "tu1"}
            }
        }"#;
        let resp = unwrap_respond(dispatch_for_test(line).await);
        let e = resp.error.unwrap();
        assert_eq!(e.code, -32602);
        assert!(e.message.contains("other_tool"));
    }

    #[tokio::test]
    async fn tools_call_registers_inflight_and_returns_spawn() {
        let inflight = Arc::new(InflightCalls::new());
        let line = r#"{
            "jsonrpc":"2.0",
            "id":4,
            "method":"tools/call",
            "params": {
                "name": "delegate_to_agent",
                "arguments": {"agent_type": "codex", "task": "x"}
            }
        }"#;
        let action = dispatch_line(&ctx(), inflight.clone(), line).await;
        match action {
            LineAction::Spawn(call) => {
                assert_eq!(call.request_id_key, request_id_key(&Value::from(4)));
            }
            _ => panic!("expected Spawn"),
        }
        // The inflight registry should now have an entry for id=4.
        let map = inflight.inner.lock().await;
        assert_eq!(map.len(), 1);
        assert!(map.contains_key(&request_id_key(&Value::from(4))));
    }

    #[tokio::test]
    async fn cancel_notification_fires_inflight_cancel_channel() {
        let inflight = Arc::new(InflightCalls::new());
        // Pre-seed an inflight entry with a known cancel_tx; verify the
        // notification handler trips it.
        let (cancel_tx, mut cancel_rx) = oneshot::channel();
        inflight
            .register(
                request_id_key(&Value::from(7)),
                InflightEntry {
                    external_handle: Some("h-7".into()),
                    cancel_tx,
                },
            )
            .await;

        let line = r#"{
            "jsonrpc":"2.0",
            "method":"notifications/cancelled",
            "params": {"requestId": 7, "reason": "user requested"}
        }"#;
        let action = dispatch_line(&ctx(), inflight.clone(), line).await;
        assert!(matches!(action, LineAction::Silent));
        // The cancel channel should now be tripped (best-effort
        // `client_cancel` to a bogus socket failed silently — that's fine).
        assert!(cancel_rx.try_recv().is_ok());
        // Entry has been pulled.
        let map = inflight.inner.lock().await;
        assert!(map.is_empty());
    }

    #[tokio::test]
    async fn cancel_for_unknown_request_id_is_silent_noop() {
        let inflight = Arc::new(InflightCalls::new());
        let line = r#"{
            "jsonrpc":"2.0",
            "method":"notifications/cancelled",
            "params": {"requestId": 999}
        }"#;
        let action = dispatch_line(&ctx(), inflight.clone(), line).await;
        assert!(matches!(action, LineAction::Silent));
        assert!(inflight.inner.lock().await.is_empty());
    }

    #[test]
    fn render_task_report_running_ack_is_not_error() {
        let report = json!({
            "task_id": "t1",
            "status": "running",
            "child_conversation_id": 42,
            "message": "running in background"
        });
        let rendered = render_task_report(&report);
        assert_eq!(rendered["isError"], false);
        assert_eq!(rendered["content"][0]["text"], "running in background");
        assert_eq!(rendered["structuredContent"]["status"], "running");
        assert_eq!(rendered["structuredContent"]["child_conversation_id"], 42);
    }

    #[test]
    fn render_task_report_completed_surfaces_text() {
        let report = json!({
            "task_id": "t1",
            "status": "completed",
            "child_conversation_id": 42,
            "text": "the result"
        });
        let rendered = render_task_report(&report);
        assert_eq!(rendered["isError"], false);
        assert_eq!(rendered["content"][0]["text"], "the result");
        assert_eq!(rendered["structuredContent"]["status"], "completed");
    }

    #[test]
    fn render_task_report_failed_is_error() {
        let report = json!({
            "status": "failed",
            "error_code": "spawn_failed",
            "message": "spawn failed: agent missing"
        });
        let rendered = render_task_report(&report);
        assert_eq!(rendered["isError"], true);
        assert_eq!(
            rendered["content"][0]["text"],
            "spawn failed: agent missing"
        );
        assert_eq!(rendered["structuredContent"]["error_code"], "spawn_failed");
    }

    #[test]
    fn render_task_report_canceled_is_not_error() {
        // A successful cancel (or a canceled task) is a valid result, not an
        // error the LLM should treat as a failure.
        let report = json!({
            "task_id": "t1",
            "status": "canceled",
            "error_code": "canceled",
            "message": "canceled: canceled by request"
        });
        let rendered = render_task_report(&report);
        assert_eq!(rendered["isError"], false);
        assert_eq!(rendered["structuredContent"]["status"], "canceled");
    }

    #[test]
    fn render_task_report_completed_without_text_falls_back_to_message() {
        // DB-fallback for an evicted completed result: status completed, no
        // text, only a message. The content must not be empty.
        let report = json!({
            "task_id": "t1",
            "status": "completed",
            "child_conversation_id": 7,
            "message": "Result no longer cached; open child session 7 for the full output."
        });
        let rendered = render_task_report(&report);
        assert_eq!(rendered["isError"], false);
        assert_eq!(
            rendered["content"][0]["text"],
            "Result no longer cached; open child session 7 for the full output."
        );
    }

    // -- Batch get_delegation_status normalization + rendering -------------

    #[tokio::test]
    async fn get_delegation_status_bare_task_id_now_rejected() {
        // The legacy single `task_id` param is gone: a bare `{task_id}` no longer
        // resolves to a poll — it's an empty task set and must be rejected,
        // steering the caller to `task_ids`.
        let line = json!({
            "jsonrpc": "2.0", "id": 20, "method": "tools/call",
            "params": { "name": "get_delegation_status", "arguments": { "task_id": "abc" } }
        })
        .to_string();
        let resp = unwrap_respond(dispatch_for_test(&line).await);
        let e = resp.error.unwrap();
        assert_eq!(e.code, -32602);
        assert!(e.message.contains("task_ids"));
    }

    #[tokio::test]
    async fn get_delegation_status_accepts_task_ids_array() {
        let line = json!({
            "jsonrpc": "2.0", "id": 21, "method": "tools/call",
            "params": { "name": "get_delegation_status", "arguments": { "task_ids": ["a", "b"] } }
        })
        .to_string();
        assert!(matches!(
            dispatch_for_test(&line).await,
            LineAction::Spawn(_)
        ));
    }

    #[tokio::test]
    async fn get_delegation_status_empty_task_ids_rejected() {
        // An absent, empty, or all-whitespace array yields no usable ids.
        for args in [json!({ "task_ids": [] }), json!({ "task_ids": ["  "] })] {
            let line = json!({
                "jsonrpc": "2.0", "id": 22, "method": "tools/call",
                "params": { "name": "get_delegation_status", "arguments": args }
            })
            .to_string();
            let resp = unwrap_respond(dispatch_for_test(&line).await);
            let e = resp.error.expect("empty task_ids must be rejected");
            assert_eq!(e.code, -32602);
            assert!(e.message.contains("task_ids"));
        }
    }

    #[tokio::test]
    async fn get_delegation_status_non_string_task_id_rejected() {
        // A non-string entry violates the schema's `items: string` contract — the
        // whole call is rejected, NOT silently narrowed to the valid ids. Both a
        // lone non-string and a mixed `[123, "abc"]` must fail.
        for args in [
            json!({ "task_ids": [123] }),
            json!({ "task_ids": [123, "abc"] }),
        ] {
            let line = json!({
                "jsonrpc": "2.0", "id": 23, "method": "tools/call",
                "params": { "name": "get_delegation_status", "arguments": args }
            })
            .to_string();
            let resp = unwrap_respond(dispatch_for_test(&line).await);
            let e = resp
                .error
                .expect("non-string task_ids entry must be rejected");
            assert_eq!(e.code, -32602);
            assert!(e.message.contains("task_ids"));
        }
    }

    #[tokio::test]
    async fn coordination_rejects_positive_legacy_status_wait_without_spawning() {
        let line = json!({
            "jsonrpc": "2.0",
            "id": 24,
            "method": "tools/call",
            "params": {
                "name": "get_delegation_status",
                "arguments": { "task_ids": ["task-a"], "wait_ms": 60_000 }
            }
        })
        .to_string();

        let response = unwrap_respond(dispatch_with_features(COORDINATION, &line).await);
        let error = response
            .error
            .expect("positive coordination wait must fail");
        assert_eq!(error.code, -32602);
        assert_eq!(
            error.message,
            "positive wait_ms is unavailable with coordination_v1; retry with \
             return_when=\"all_terminal_or_attention\" and wait_ms=0"
        );
    }

    #[tokio::test]
    async fn coordination_keeps_supported_status_wait_forms() {
        for arguments in [
            json!({ "task_ids": ["task-a"] }),
            json!({ "task_ids": ["task-a"], "wait_ms": 0 }),
            json!({
                "task_ids": ["task-a"],
                "wait_ms": 0,
                "return_when": "all_terminal_or_attention"
            }),
        ] {
            let line = json!({
                "jsonrpc": "2.0",
                "id": 25,
                "method": "tools/call",
                "params": { "name": "get_delegation_status", "arguments": arguments }
            })
            .to_string();
            assert!(matches!(
                dispatch_with_features(COORDINATION, &line).await,
                LineAction::Spawn(_)
            ));
        }
    }

    #[tokio::test]
    async fn legacy_connection_keeps_positive_status_wait() {
        let line = json!({
            "jsonrpc": "2.0",
            "id": 26,
            "method": "tools/call",
            "params": {
                "name": "get_delegation_status",
                "arguments": { "task_ids": ["task-a"], "wait_ms": 60_000 }
            }
        })
        .to_string();
        assert!(matches!(
            dispatch_for_test(&line).await,
            LineAction::Spawn(_)
        ));
    }

    /// Incident 1570 production field path (companion layer): host `_meta` on
    /// `get_delegation_status` becomes `BrokerStatusRequest.parent_tool_use_id`
    /// for the listener arm path. Never invents an id when meta is absent.
    /// See also `listener::tests::incident_1570_*` and attribution 1570 pack.
    #[test]
    fn incident_1570_companion_meta_becomes_status_parent_tool_use_id() {
        let with_meta = json!({
            "name": "get_delegation_status",
            "arguments": { "task_ids": ["task-1"], "wait_ms": 0 },
            "_meta": { "tool_use_id": "wait-B" }
        });
        let req = build_status_request(&ctx(), vec!["task-1".into()], Some(0), None, &with_meta);
        assert_eq!(
            req.parent_tool_use_id, "wait-B",
            "production wait tool id must ride the status request field"
        );
        assert_eq!(req.task_ids, vec!["task-1".to_string()]);
        assert_eq!(req.wait_ms, Some(0));

        let without_meta = json!({
            "name": "get_delegation_status",
            "arguments": { "task_ids": ["task-1"], "wait_ms": 0 }
        });
        let empty =
            build_status_request(&ctx(), vec!["task-1".into()], Some(0), None, &without_meta);
        assert_eq!(
            empty.parent_tool_use_id, "",
            "missing _meta must not invent a wait tool id"
        );
    }

    /// Empty / non-string tool_use_id is treated as missing.
    #[test]
    fn get_delegation_status_request_blank_or_non_string_meta_is_empty() {
        for meta in [
            json!({ "tool_use_id": "" }),
            json!({ "tool_use_id": 123 }),
            json!({}),
        ] {
            let params = json!({
                "name": "get_delegation_status",
                "arguments": { "task_ids": ["t1"] },
                "_meta": meta
            });
            let req = build_status_request(&ctx(), vec!["t1".into()], None, None, &params);
            assert_eq!(
                req.parent_tool_use_id, "",
                "meta={meta:?} must not invent a wait tool id"
            );
        }
    }

    #[test]
    fn normalize_status_task_ids_dedups_preserves_order() {
        // Trim each entry, drop "", collapse the duplicate "a", keep first-seen
        // order.
        let args = json!({ "task_ids": [" a ", "b", "a", "", "c"] });
        assert_eq!(
            normalize_status_task_ids(&args).unwrap(),
            vec!["a", "b", "c"]
        );
    }

    #[test]
    fn render_status_result_single_renders_as_one_element_batch() {
        // A single-id poll now renders through the SAME `{tasks:[..]}` envelope as
        // a fan-out (unified shape) — NOT the bare single-report path. The
        // structured batch carries the one task with its id + status, and the
        // content text is the `{tasks:[..]}` JSON (not the bare result text).
        let report = json!({
            "task_id": "t1", "status": "completed",
            "child_conversation_id": 42, "text": "the result"
        });
        let rendered = render_status_result(&json!({ "tasks": [report.clone()] }));
        let tasks = rendered["structuredContent"]["tasks"].as_array().unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0]["task_id"], "t1");
        assert_eq!(tasks[0]["status"], "completed");
        // Content text is the compact {tasks:[..]} JSON, recoverable by
        // content-only hosts — not the raw "the result" string.
        let text = rendered["content"][0]["text"].as_str().unwrap();
        let parsed: Value = serde_json::from_str(text).unwrap();
        assert_eq!(parsed["tasks"][0]["text"], "the result");
        assert_eq!(rendered["isError"], false);
    }

    #[test]
    fn render_status_result_bare_report_wrapped_as_one_element_batch() {
        // Defensive: an outcome with no `tasks` array (older / unexpected shape) is
        // wrapped into a one-element batch so the output stays uniformly
        // `{tasks:[..]}`. A lone failed task flags `isError` (all-failed).
        let report = json!({
            "task_id": "t1", "status": "failed",
            "error_code": "spawn_failed", "message": "spawn failed"
        });
        let rendered = render_status_result(&report);
        let tasks = rendered["structuredContent"]["tasks"].as_array().unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0]["task_id"], "t1");
        assert_eq!(tasks[0]["status"], "failed");
        assert_eq!(rendered["isError"], true);
    }

    #[test]
    fn continuation_arm_failure_returns_explicit_tool_error() {
        let outcome = json!({
            "error": {
                "code": "continuation_arm_failed",
                "message": "Delegation continuation could not be armed"
            }
        });

        let rendered = render_status_result(&outcome);

        assert_eq!(rendered["isError"], true);
        assert_eq!(
            rendered["content"][0]["text"],
            "Delegation continuation could not be armed"
        );
        assert_eq!(rendered["structuredContent"], outcome);
        assert!(rendered["structuredContent"].get("tasks").is_none());
    }

    #[test]
    fn render_batch_report_carries_tasks_and_parseable_text() {
        let envelope = json!({ "tasks": [
            { "task_id": "t1", "status": "completed", "text": "r1" },
            { "task_id": "t2", "status": "running", "message": "Running." },
        ] });
        let rendered = render_status_result(&envelope);
        // structuredContent carries the whole batch.
        assert_eq!(
            rendered["structuredContent"]["tasks"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        // The content text is the compact {tasks:[..]} JSON, recoverable by hosts
        // that persist only CallToolResult.content text (e.g. Claude Code).
        let text = rendered["content"][0]["text"].as_str().unwrap();
        let parsed: Value = serde_json::from_str(text).unwrap();
        assert_eq!(parsed["tasks"][0]["task_id"], "t1");
        assert_eq!(parsed["tasks"][1]["status"], "running");
        // Mixed statuses → not all failed → not flagged as an error.
        assert_eq!(rendered["isError"], false);
    }

    #[test]
    fn render_batch_report_is_error_only_when_all_failed() {
        let all_failed = json!({ "tasks": [
            { "task_id": "t1", "status": "failed", "message": "x" },
            { "task_id": "t2", "status": "failed", "message": "y" },
        ] });
        assert_eq!(render_status_result(&all_failed)["isError"], true);
        let mixed = json!({ "tasks": [
            { "task_id": "t1", "status": "failed" },
            { "task_id": "t2", "status": "canceled" },
        ] });
        assert_eq!(render_status_result(&mixed)["isError"], false);
    }

    #[test]
    fn join_input_requires_capability_literal_value_and_explicit_zero() {
        assert_eq!(parse_return_when(&json!({}), true).unwrap(), None);
        assert!(parse_return_when(
            &json!({"return_when":"all_terminal_or_attention","wait_ms":0}),
            false,
        )
        .is_err());
        assert!(
            parse_return_when(&json!({"return_when":"all_terminal_or_attention"}), true,).is_err()
        );
        assert!(parse_return_when(
            &json!({"return_when":"all_terminal_or_attention","wait_ms":1}),
            true,
        )
        .is_err());
        assert_eq!(
            parse_return_when(
                &json!({"return_when":"all_terminal_or_attention","wait_ms":0}),
                true,
            )
            .unwrap(),
            Some(DelegationReturnWhen::AllTerminalOrAttention)
        );
    }

    #[test]
    fn legacy_batch_omits_join_fields_on_the_wire() {
        use crate::acp::delegation::types::DelegationStatusBatch;
        let value = serde_json::to_value(DelegationStatusBatch::legacy(vec![])).unwrap();
        assert_eq!(value, json!({"tasks": []}));
    }

    #[test]
    fn joined_batch_includes_empty_attention_array() {
        use crate::acp::delegation::types::{DelegationStatusBatch, DelegationWakeReason};
        let value = serde_json::to_value(DelegationStatusBatch::joined(
            vec![],
            DelegationWakeReason::AllTerminal,
            vec![],
        ))
        .unwrap();
        assert_eq!(value["wake_reason"], "all_terminal");
        assert_eq!(value["attention_requests"], json!([]));
    }

    #[test]
    fn joined_status_renderer_preserves_attention_in_text_and_structured_content() {
        let outcome = json!({
            "tasks": [{"task_id":"task-1", "status":"running"}],
            "wake_reason": "attention_required",
            "attention_requests": [{
                "request_id":"request-1",
                "task_id":"task-1",
                "message":"Choose A or B",
                "created_at":"2026-07-17T10:00:00Z"
            }]
        });
        let rendered = render_status_result(&outcome);
        assert_eq!(rendered["structuredContent"], outcome);
        let text = rendered["content"][0]["text"].as_str().unwrap();
        assert_eq!(serde_json::from_str::<Value>(text).unwrap(), outcome);
    }

    #[test]
    fn legacy_status_renderer_keeps_the_exact_tasks_only_envelope() {
        let outcome = json!({"tasks": []});
        let rendered = render_status_result(&outcome);
        assert_eq!(rendered["structuredContent"], outcome);
        assert_eq!(rendered["content"][0]["text"], "{\"tasks\":[]}");
    }

    #[tokio::test]
    async fn coordination_and_legacy_tools_list_project_wait_contract() {
        let legacy = unwrap_respond(
            dispatch_for_test(r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#).await,
        );
        let tools = legacy.result.unwrap()["tools"].as_array().unwrap().clone();
        let status = tools
            .iter()
            .find(|t| t["name"] == "get_delegation_status")
            .unwrap();
        assert!(status["inputSchema"]["properties"]
            .get("return_when")
            .is_none());
        assert!(!tool_guidance(status).contains("all_terminal_or_attention"));
        let legacy_wait = &status["inputSchema"]["properties"]["wait_ms"];
        assert_eq!(legacy_wait["minimum"], 0);
        assert!(legacy_wait.get("maximum").is_none());
        let legacy_guidance = tool_guidance(status);
        assert!(legacy_guidance.contains("positive wait (max 60000 ms)"));
        assert!(!legacy_guidance.contains("positive wait_ms is rejected"));
        let delegate = tools
            .iter()
            .find(|t| t["name"] == "delegate_to_agent")
            .unwrap();
        assert!(!tool_guidance(delegate).contains("all_terminal_or_attention"));

        let coord = unwrap_respond(
            dispatch_with_features(
                COORDINATION,
                r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#,
            )
            .await,
        );
        let tools = coord.result.unwrap()["tools"].as_array().unwrap().clone();
        let status = tools
            .iter()
            .find(|t| t["name"] == "get_delegation_status")
            .unwrap();
        assert!(status["inputSchema"]["properties"]
            .get("return_when")
            .is_some());
        assert!(tool_guidance(status).contains("all_terminal_or_attention"));
        let coordination_wait = &status["inputSchema"]["properties"]["wait_ms"];
        assert_eq!(coordination_wait["minimum"], 0);
        assert_eq!(coordination_wait["maximum"], 0);
        let coordination_guidance = tool_guidance(status);
        for required in [
            "omit wait_ms for an immediate snapshot",
            "return_when=all_terminal_or_attention requires explicit wait_ms=0",
            "no positive wait_ms",
            "re-join only required running tasks",
        ] {
            assert!(
                coordination_guidance.contains(required),
                "coordination guidance missing {required:?}"
            );
        }
        assert!(!coordination_guidance.contains("positive wait (max 60000 ms)"));
        let delegate = tools
            .iter()
            .find(|t| t["name"] == "delegate_to_agent")
            .unwrap();
        assert!(tool_guidance(delegate).contains("join"));
    }

    // -- check_user_feedback feature gating + rendering --------------------

    const FEEDBACK_ONLY: CompanionFeatures = CompanionFeatures {
        delegation: false,
        coordination_v1: false,
        feedback: true,
        ask: false,
        sessions: false,
        workflow_v2: false,
        completion_v2: false,
    };
    const BOTH: CompanionFeatures = CompanionFeatures {
        delegation: true,
        coordination_v1: false,
        feedback: true,
        ask: false,
        sessions: false,
        workflow_v2: false,
        completion_v2: false,
    };
    const ASK_ONLY: CompanionFeatures = CompanionFeatures {
        delegation: false,
        coordination_v1: false,
        feedback: false,
        ask: true,
        sessions: false,
        workflow_v2: false,
        completion_v2: false,
    };
    const SESSIONS_ONLY: CompanionFeatures = CompanionFeatures {
        delegation: false,
        coordination_v1: false,
        feedback: false,
        ask: false,
        sessions: true,
        workflow_v2: false,
        completion_v2: false,
    };
    const GROK_FEATURES: CompanionFeatures = CompanionFeatures {
        delegation: true,
        coordination_v1: true,
        feedback: true,
        ask: false,
        sessions: true,
        workflow_v2: true,
        completion_v2: false,
    };
    const COORDINATION: CompanionFeatures = CompanionFeatures {
        delegation: true,
        coordination_v1: true,
        feedback: false,
        ask: false,
        sessions: false,
        workflow_v2: false,
        completion_v2: false,
    };
    const HISTORICAL_WORKFLOW_ROOT_FIXTURE: CompanionFeatures = CompanionFeatures {
        delegation: true,
        coordination_v1: false,
        feedback: false,
        ask: false,
        sessions: false,
        workflow_v2: true,
        completion_v2: false,
    };
    const HISTORICAL_COMPLETION_CHILD_FIXTURE: CompanionFeatures = CompanionFeatures {
        delegation: false,
        coordination_v1: false,
        feedback: false,
        ask: false,
        sessions: false,
        workflow_v2: false,
        completion_v2: true,
    };

    fn list_tool_names(action: LineAction) -> Vec<String> {
        let resp = unwrap_respond(action);
        resp.result.unwrap()["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_string())
            .collect()
    }

    fn legacy_root() -> CompanionContext {
        ctx_with(CompanionFeatures {
            delegation: true,
            coordination_v1: false,
            feedback: false,
            ask: false,
            sessions: false,
            workflow_v2: false,
            completion_v2: false,
        })
    }

    fn coordination_root() -> CompanionContext {
        let mut c = ctx_with(COORDINATION);
        c.role = CompanionRole::Root;
        c
    }

    fn coordination_child() -> CompanionContext {
        let mut c = ctx_with(COORDINATION);
        c.role = CompanionRole::DelegationChild;
        c
    }

    async fn dispatch_with_context(ctx: CompanionContext, line: &str) -> LineAction {
        dispatch_line(&ctx, Arc::new(InflightCalls::new()), line).await
    }

    fn tools_list() -> &'static str {
        r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#
    }

    fn call(id: i64, name: &str, arguments: Value) -> String {
        json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/call",
            "params": { "name": name, "arguments": arguments }
        })
        .to_string()
    }

    fn with_argument(mut arguments: Value, key: &str, value: Value) -> Value {
        arguments
            .as_object_mut()
            .expect("tool arguments object")
            .insert(key.to_string(), value);
        arguments
    }

    fn tool_names(action: LineAction) -> Vec<String> {
        list_tool_names(action)
    }

    fn schema_accepts(root: &Value, schema: &Value, candidate: &Value) -> bool {
        if let Some(reference) = schema.get("$ref").and_then(Value::as_str) {
            let Some(pointer) = reference.strip_prefix('#') else {
                return false;
            };
            let Some(resolved) = root.pointer(pointer) else {
                return false;
            };
            return schema_accepts(root, resolved, candidate);
        }
        if let Some(expected) = schema.get("const") {
            if candidate != expected {
                return false;
            }
        }
        if let Some(values) = schema.get("enum").and_then(Value::as_array) {
            if !values.contains(candidate) {
                return false;
            }
        }
        match schema.get("type").and_then(Value::as_str) {
            Some("object") => {
                let Some(object) = candidate.as_object() else {
                    return false;
                };
                let properties = schema
                    .get("properties")
                    .and_then(Value::as_object)
                    .cloned()
                    .unwrap_or_default();
                if schema.get("additionalProperties") == Some(&Value::Bool(false))
                    && object.keys().any(|key| !properties.contains_key(key))
                {
                    return false;
                }
                if schema
                    .get("required")
                    .and_then(Value::as_array)
                    .is_some_and(|required| {
                        required
                            .iter()
                            .filter_map(Value::as_str)
                            .any(|key| !object.contains_key(key))
                    })
                {
                    return false;
                }
                object.iter().all(|(key, value)| {
                    properties
                        .get(key)
                        .is_none_or(|property| schema_accepts(root, property, value))
                })
            }
            Some("string") => {
                let Some(value) = candidate.as_str() else {
                    return false;
                };
                let len = value.chars().count() as u64;
                if schema
                    .get("minLength")
                    .and_then(Value::as_u64)
                    .is_some_and(|minimum| len < minimum)
                    || schema
                        .get("maxLength")
                        .and_then(Value::as_u64)
                        .is_some_and(|maximum| len > maximum)
                {
                    return false;
                }
                schema
                    .get("pattern")
                    .and_then(Value::as_str)
                    .is_none_or(|pattern| regex::Regex::new(pattern).unwrap().is_match(value))
            }
            Some("integer") => {
                let Some(value) = candidate.as_u64() else {
                    return false;
                };
                schema
                    .get("minimum")
                    .and_then(Value::as_u64)
                    .is_none_or(|minimum| value >= minimum)
                    && schema
                        .get("maximum")
                        .and_then(Value::as_u64)
                        .is_none_or(|maximum| value <= maximum)
            }
            Some("boolean") => candidate.is_boolean(),
            Some(_) => false,
            None => true,
        }
    }

    #[test]
    fn orchestration_binding_transport_schemas_match_shared_corpus() {
        let corpus: Value = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/orchestration_binding_v1.json"
        )))
        .expect("valid orchestration binding corpus");
        let catalog: Value = serde_json::from_str(TOOL_SCHEMA_JSON).expect("valid tool schema");

        for tool_name in ["delegate_to_agent", "continue_delegation"] {
            let tool = catalog
                .as_array()
                .unwrap()
                .iter()
                .find(|tool| tool["name"] == tool_name)
                .unwrap();
            let schema = &tool["inputSchema"];
            assert!(
                schema["properties"]["orchestration_binding"].is_object(),
                "{tool_name} must publish orchestration_binding"
            );
            assert!(
                !schema["required"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|required| required == "orchestration_binding"),
                "{tool_name} binding must remain optional"
            );

            let omitted = if tool_name == "delegate_to_agent" {
                json!({
                    "agent_type": "grok",
                    "task": "unbound first dispatch",
                    "correlation_id": "binding-schema-omitted"
                })
            } else {
                json!({
                    "task_id": "source-task",
                    "task": "unbound continuation",
                    "correlation_id": "binding-schema-omitted"
                })
            };
            assert!(schema_accepts(schema, schema, &omitted));

            for case in corpus["cases"].as_array().unwrap() {
                let mut input = omitted.clone();
                input
                    .as_object_mut()
                    .unwrap()
                    .insert("orchestration_binding".into(), case["value"].clone());
                let expected = case["valid"].as_bool().unwrap();
                assert!(
                    schema_accepts(schema, schema, &input),
                    "{tool_name} compact catalog must defer {} to runtime",
                    case["name"]
                );
                assert_eq!(
                    serde_json::from_value::<OrchestrationBindingV1>(case["value"].clone())
                        .is_ok_and(|binding| binding.validate().is_ok()),
                    expected,
                    "semantic validation disagrees for {}",
                    case["name"]
                );
            }
        }
    }

    fn assert_no_generic_coordination_side_channel_tools(names: &[String]) {
        for forbidden in [
            "progress",
            "warning",
            "log",
            "heartbeat",
            "detach",
            "deliver_result",
            "send_result",
            "result_delivery",
        ] {
            assert!(
                !names.iter().any(|n| n.contains(forbidden)),
                "unexpected side-channel tool name containing {forbidden:?}: {names:?}"
            );
        }
    }

    fn collect_descriptions(value: &Value, output: &mut String) {
        match value {
            Value::Object(map) => {
                if let Some(description) = map.get("description").and_then(Value::as_str) {
                    output.push_str(description);
                    output.push(' ');
                }
                for (key, nested) in map {
                    if key != "description" || !nested.is_string() {
                        collect_descriptions(nested, output);
                    }
                }
            }
            Value::Array(items) => {
                for item in items {
                    collect_descriptions(item, output);
                }
            }
            _ => {}
        }
    }

    fn tool_guidance(tool: &Value) -> String {
        let mut output = String::new();
        collect_descriptions(tool, &mut output);
        output.to_ascii_lowercase()
    }

    #[test]
    fn tool_schema_retains_essential_agent_guidance() {
        let schema: Value = serde_json::from_str(TOOL_SCHEMA_JSON).unwrap();
        let tools = schema.as_array().unwrap();
        let ask_tool = tools
            .iter()
            .find(|tool| tool["name"] == "ask_user_question")
            .unwrap();
        assert!(
            tool_guidance(ask_tool).contains("meaning or trade-off"),
            "ask_user_question guidance lost nested option description"
        );
        let delegate_description = tools
            .iter()
            .find(|tool| tool["name"] == "delegate_to_agent")
            .and_then(|tool| tool["description"].as_str())
            .expect("delegate_to_agent description");
        assert!(
            delegate_description.contains(
                "For each distinct agent/profile mention, call once; pass profile_id for a profile"
            ),
            "delegate guidance must not imply that ordinary agent mentions require profile_id"
        );
        let cases: [(&str, &[&str]); 9] = [
            (
                "delegate_to_agent",
                &[
                    "asynchronous",
                    "task_id",
                    "cold",
                    "cannot see this conversation",
                    "task must include all context",
                    "fan out",
                    "join",
                    "each distinct",
                    "call once",
                    "profile_id",
                ],
            ),
            (
                "get_delegation_status",
                &[
                    "task_ids",
                    "wait_ms",
                    "return_when",
                    "all_terminal_or_attention",
                    "omit wait_ms for an immediate snapshot",
                    "return_when=all_terminal_or_attention",
                    "no positive wait_ms",
                    "re-join only required running tasks",
                    "all terminal",
                    "attention",
                    "unavailable",
                    "input order",
                    "wake_reason",
                    "attention_requests",
                ],
            ),
            (
                "cancel_delegation",
                &[
                    "only when its result is no longer wanted",
                    "timeout",
                    "non-canceling",
                    "keep waiting",
                    "wait_ms for slow work",
                    "already finished",
                    "final result",
                    "taskfail, usercancel, and others cancel",
                ],
            ),
            (
                "check_user_feedback",
                &[
                    "messages are available only through this tool",
                    "non-blocking",
                    "before starting implementation",
                    "significant decision",
                    "after a meaningful sub-task",
                    "high-priority",
                    "empty result means continue",
                ],
            ),
            (
                "ask_user_question",
                &[
                    "1-4 related",
                    "block until submitted or dismissed",
                    "genuinely user-owned discrete decision",
                    "cannot be resolved",
                    "do not ask merely whether to proceed",
                    "confirm an obvious default",
                    "open-ended input",
                    "other is added automatically",
                    "recommended",
                    "one call",
                    "meaning or trade-off",
                ],
            ),
            (
                "get_session_info",
                &[
                    "codeg://session/",
                    "read-only metadata",
                    "optional recent messages",
                    "internal conversation id",
                    "not the agent session id",
                    "found: false",
                    "not an error",
                ],
            ),
            (
                "request_parent_decision",
                &[
                    "direct parent",
                    "blocking decision",
                    "blocks until reply or closure",
                    "not for progress, logs, or warnings",
                ],
            ),
            (
                "reply_to_delegation",
                &[
                    "open direct-child join decision",
                    "first reply wins",
                    "idempotent",
                ],
            ),
            (
                "request_recovery_authorization",
                &[
                    "recovery_confirmation_required",
                    "exact rejected call",
                    "subject_kind",
                    "allowed_action",
                    "cause_code",
                    "expires_at",
                    "target_state",
                    "replacement_reason",
                ],
            ),
        ];

        for (name, required_phrases) in cases {
            let tool = tools.iter().find(|tool| tool["name"] == name).unwrap();
            let guidance = tool_guidance(tool);
            for phrase in required_phrases {
                assert!(
                    guidance.contains(phrase),
                    "{name} guidance lost required phrase: {phrase}"
                );
            }
        }
    }

    #[test]
    fn features_parse_defaults_and_tokens() {
        // Absent → delegation-only (backward compatible), no Join/workflow.
        let def = CompanionFeatures::parse(None);
        assert!(def.delegation && !def.feedback && !def.coordination_v1);
        assert!(!def.ask);
        assert!(!def.sessions);
        assert!(!def.workflow_v2);
        // Explicit list, whitespace + unknown tokens tolerated.
        let all = CompanionFeatures::parse(Some(concat!(
            " delegation , coordination_v1 , feedback , ask , sessions , ",
            "workflow_v2 , completion_v2 ,bogus"
        )));
        assert!(all.delegation && all.coordination_v1 && all.feedback && all.ask && all.sessions);
        assert!(!all.workflow_v2 && !all.completion_v2);
        let fb = CompanionFeatures::parse(Some("feedback"));
        assert!(!fb.delegation && fb.feedback && !fb.ask && !fb.coordination_v1);
        assert!(!fb.workflow_v2);
        let ask = CompanionFeatures::parse(Some("ask"));
        assert!(!ask.delegation && !ask.feedback && ask.ask);
        let sessions = CompanionFeatures::parse(Some("sessions"));
        assert!(!sessions.delegation && !sessions.feedback && !sessions.ask && sessions.sessions);
        let wf = CompanionFeatures::parse(Some("workflow_v2"));
        assert!(!wf.workflow_v2 && !wf.delegation);
        // Empty string → nothing enabled.
        let none = CompanionFeatures::parse(Some(""));
        assert!(!none.delegation && !none.feedback && !none.ask && !none.sessions);
        assert!(!none.coordination_v1);
        assert!(!none.workflow_v2);
    }

    #[tokio::test]
    async fn workflow_v2_stale_feature_tool_catalog_is_retired_for_root_and_child() {
        let stale = CompanionFeatures::parse(Some("delegation,workflow_v2,completion_v2"));
        assert!(!stale.workflow_tools_enabled());

        let mut root = ctx_with(stale);
        root.role = CompanionRole::Root;
        let root_names = list_tool_names(dispatch_with_context(root, tools_list()).await);
        assert!(root_names.iter().any(|name| name == "delegate_to_agent"));
        assert!(root_names
            .iter()
            .any(|name| name == "register_simple_workflow"));
        assert!(WORKFLOW_V2_TOOLS
            .iter()
            .all(|tool| !root_names.iter().any(|name| name == tool)));
        assert!(!root_names.iter().any(|name| name == "complete_work"));

        let mut child = ctx_with(stale);
        child.role = CompanionRole::DelegationChild;
        let child_names = list_tool_names(dispatch_with_context(child, tools_list()).await);
        assert!(child_names.iter().any(|name| name == "delegate_to_agent"));
        assert!(!child_names
            .iter()
            .any(|name| name == "register_simple_workflow"));
        assert!(!child_names.iter().any(|name| name == "complete_work"));
        assert!(WORKFLOW_V2_TOOLS
            .iter()
            .all(|tool| !child_names.iter().any(|name| name == tool)));

        let v1 = CompanionFeatures::parse(Some("workflow_v1"));
        assert!(
            !v1.workflow_tools_enabled(),
            "workflow_v1 must be ignored as an unknown token"
        );
    }

    #[tokio::test]
    async fn register_simple_workflow_is_delegation_root_only_and_schema_has_no_parent_id() {
        let root = legacy_root();
        let root_names = list_tool_names(dispatch_with_context(root.clone(), tools_list()).await);
        assert!(root_names
            .iter()
            .any(|name| name == "register_simple_workflow"));

        let mut child = root.clone();
        child.role = CompanionRole::DelegationChild;
        let child_names = list_tool_names(dispatch_with_context(child, tools_list()).await);
        assert!(!child_names
            .iter()
            .any(|name| name == "register_simple_workflow"));

        let disabled = ctx_with(CompanionFeatures::parse(Some("sessions")));
        let disabled_names = list_tool_names(dispatch_with_context(disabled, tools_list()).await);
        assert!(!disabled_names
            .iter()
            .any(|name| name == "register_simple_workflow"));

        let schema: Value = serde_json::from_str(TOOL_SCHEMA_JSON).expect("valid tool schema");
        let registration = schema
            .as_array()
            .expect("tool array")
            .iter()
            .find(|tool| tool["name"] == "register_simple_workflow")
            .expect("Simple registration tool");
        let properties = registration["inputSchema"]["properties"]
            .as_object()
            .expect("registration properties");
        assert_eq!(
            properties.keys().cloned().collect::<Vec<_>>(),
            vec!["plan_rel_path", "progress_rel_path"]
        );

        let response = unwrap_respond(
            dispatch_with_context(
                root,
                &call(
                    91,
                    "register_simple_workflow",
                    json!({
                        "plan_rel_path": "docs/plan.md",
                        "parent_conversation_id": 42
                    }),
                ),
            )
            .await,
        );
        assert_eq!(response.error.expect("unknown field error").code, -32602);
    }

    #[test]
    fn completion_v2_stale_feature_token_is_ignored() {
        let feature = CompanionFeatures::parse(Some("completion_v2"));
        let mut child = ctx_with(feature);
        child.role = CompanionRole::DelegationChild;
        assert!(!child.allows_tool("complete_work"));

        let mut root = ctx_with(feature);
        root.role = CompanionRole::Root;
        assert!(!root.allows_tool("complete_work"));

        let mut misspelled = ctx_with(CompanionFeatures::parse(Some("completion_v1")));
        misspelled.role = CompanionRole::DelegationChild;
        assert!(!misspelled.allows_tool("complete_work"));
    }

    #[test]
    fn complete_work_identity_prefers_tool_use_id_then_rpc_identity() {
        assert_eq!(
            derive_child_tool_call_id(Some("tool-77"), "inc-4", &json!(9)).unwrap(),
            "tool-77"
        );
        assert_eq!(
            derive_child_tool_call_id(None, "inc-4", &json!(9)).unwrap(),
            "rpc:inc-4:9"
        );
        assert_eq!(
            derive_child_tool_call_id(None, "inc-4", &json!("9")).unwrap(),
            "rpc:inc-4:\"9\""
        );
        assert!(derive_child_tool_call_id(None, "inc-4", &Value::Null).is_err());
    }

    #[tokio::test]
    async fn complete_work_rejects_unknown_arguments_before_broker_dispatch() {
        let mut child = ctx_with(HISTORICAL_COMPLETION_CHILD_FIXTURE);
        child.role = CompanionRole::DelegationChild;
        let response = unwrap_respond(
            dispatch_with_context(
                child,
                &call(
                    7,
                    "complete_work",
                    json!({
                        "outcome": "approve",
                        "task_id": "model-supplied-identity"
                    }),
                ),
            )
            .await,
        );
        assert_eq!(response.error.unwrap().code, -32602);
    }

    #[tokio::test]
    async fn complete_work_rejects_multibyte_strings_over_byte_bounds_before_dispatch() {
        let mut child = ctx_with(HISTORICAL_COMPLETION_CHILD_FIXTURE);
        child.role = CompanionRole::DelegationChild;
        for arguments in [
            json!({"outcome": "approve", "summary": "界".repeat(1366)}),
            json!({"outcome": "approve", "report_file": "界".repeat(342)}),
        ] {
            let response = unwrap_respond(
                dispatch_with_context(child.clone(), &call(7, "complete_work", arguments)).await,
            );
            assert_eq!(response.error.unwrap().code, -32602);
        }
    }

    #[test]
    fn complete_work_schema_is_semantic_only_and_exact() {
        let schema: Value = serde_json::from_str(TOOL_SCHEMA_JSON).unwrap();
        let tool = schema
            .as_array()
            .unwrap()
            .iter()
            .find(|tool| tool["name"] == "complete_work")
            .expect("complete_work schema");
        assert_eq!(
            tool,
            &json!({
                "name": "complete_work",
                "description": "Record the workflow child's semantic conclusion. This does not terminate the child.",
                "inputSchema": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["outcome"],
                    "properties": {
                        "outcome": {
                            "type": "string",
                            "enum": [
                                "approve",
                                "approve_with_minors",
                                "request_changes",
                                "block",
                                "done",
                                "done_with_concerns",
                                "blocked"
                            ]
                        },
                        "summary": {"type": "string", "maxLength": 4096},
                        "report_file": {"type": "string", "maxLength": 1024}
                    }
                }
            })
        );
    }

    #[test]
    fn workflow_v2_catalog_modes_fail_closed() {
        assert_eq!(
            format!("{:?}", classify_workflow_tool_catalog(std::iter::empty())),
            "Unavailable"
        );
        assert_eq!(
            format!(
                "{:?}",
                classify_workflow_tool_catalog(WORKFLOW_V2_TOOLS.iter().copied())
            ),
            "WorkflowManifestV2"
        );
        assert_eq!(
            classify_workflow_tool_catalog(["get_workflow_capabilities"]),
            WorkflowCapabilityMode::Inconsistent
        );
    }

    #[test]
    fn b9_partial_tool_set_is_inconsistent_hard_block() {
        assert_eq!(
            classify_workflow_tool_catalog(std::iter::empty()),
            WorkflowCapabilityMode::Unavailable
        );
        assert_eq!(
            classify_workflow_tool_catalog(WORKFLOW_V2_TOOLS.iter().copied()),
            WorkflowCapabilityMode::WorkflowManifestV2
        );
        // Any non-empty proper subset → hard-block, not legacy.
        assert_eq!(
            classify_workflow_tool_catalog(["get_workflow_capabilities"]),
            WorkflowCapabilityMode::Inconsistent
        );
        assert_eq!(
            classify_workflow_tool_catalog([
                "get_workflow_capabilities",
                "get_workflow_state",
                "publish_workflow_manifest",
            ]),
            WorkflowCapabilityMode::Inconsistent
        );
        assert_eq!(
            classify_workflow_tool_catalog(["publish_workflow_manifest", "settle_workflow_gate"]),
            WorkflowCapabilityMode::Inconsistent
        );
    }

    #[tokio::test]
    async fn historical_workflow_fixture_catalog_agrees_with_local_capabilities() {
        let names = list_tool_names(
            dispatch_with_features(HISTORICAL_WORKFLOW_ROOT_FIXTURE, tools_list()).await,
        );
        assert_eq!(
            WORKFLOW_V2_TOOLS,
            &[
                "get_workflow_capabilities",
                "get_workflow_state",
                "recover_workflow",
                "publish_workflow_manifest",
                "settle_workflow_gate",
            ]
        );
        assert!(names.contains(&"request_recovery_authorization".to_string()));
        for tool in WORKFLOW_V2_TOOLS {
            assert!(
                names.iter().any(|n| n == *tool),
                "historical workflow fixture must expose {tool}; names={names:?}"
            );
        }
        assert_eq!(
            classify_workflow_tool_catalog(names.iter().map(String::as_str)),
            WorkflowCapabilityMode::WorkflowManifestV2
        );
        let caps =
            local_workflow_capabilities(&HISTORICAL_WORKFLOW_ROOT_FIXTURE, CompanionRole::Root);
        assert_eq!(caps["workflow_manifest_v2"], true);
        assert_eq!(caps["versions"][WORKFLOW_CAPABILITY_VERSION], true);
        let ops: Vec<&str> = caps["operations"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(ops, WORKFLOW_V2_TOOLS);
        // Catalog ∩ capability operations agreement (A15.1 / B9).
        for op in &ops {
            assert!(names.iter().any(|n| n == *op));
        }
    }

    #[tokio::test]
    async fn workflow_v2_child_hides_all_workflow_tools() {
        let mut child = ctx_with(HISTORICAL_WORKFLOW_ROOT_FIXTURE);
        child.role = CompanionRole::DelegationChild;
        let names = list_tool_names(dispatch_with_context(child.clone(), tools_list()).await);
        for tool in WORKFLOW_V2_TOOLS {
            assert!(
                !names.iter().any(|n| n == *tool),
                "child must not expose {tool}; names={names:?}"
            );
        }
        // Call path: publish denied as unknown tool (Root-only gating).
        let action = dispatch_with_context(
            child,
            &call(
                2,
                "publish_workflow_manifest",
                json!({ "schema_version": 1 }),
            ),
        )
        .await;
        let resp = unwrap_respond(action);
        let err = resp.error.expect("child publish must be rejected");
        assert_eq!(err.code, -32602);
        assert!(err
            .message
            .contains("unknown tool: publish_workflow_manifest"));
    }

    #[tokio::test]
    async fn get_workflow_capabilities_answers_locally_without_broker() {
        let action = dispatch_with_features(
            HISTORICAL_WORKFLOW_ROOT_FIXTURE,
            &call(9, "get_workflow_capabilities", json!({})),
        )
        .await;
        // Local tool: Respond immediately (no Spawn / UDS).
        let resp = unwrap_respond(action);
        let result = resp.result.expect("capabilities result");
        assert_eq!(result["isError"], false);
        assert_eq!(result["structuredContent"]["workflow_manifest_v2"], true);
        assert_eq!(
            result["structuredContent"]["versions"][WORKFLOW_CAPABILITY_VERSION],
            true
        );
    }

    #[tokio::test]
    async fn get_workflow_state_detail_contract_rejects_before_inflight() {
        let omitted = parse_get_workflow_state_args(&json!({}), "token").unwrap();
        let explicit =
            parse_get_workflow_state_args(&json!({ "detail": "index" }), "token").unwrap();
        assert_eq!(
            serde_json::to_value(omitted).unwrap(),
            serde_json::to_value(explicit).unwrap()
        );

        for detail in [
            Value::Null,
            json!("full"),
            json!(1),
            json!([]),
            json!({}),
            json!(true),
            json!(false),
        ] {
            let inflight = Arc::new(InflightCalls::new());
            let line = json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/call",
                "params": { "name": "get_workflow_state", "arguments": { "detail": detail } }
            })
            .to_string();
            let response = unwrap_respond(
                dispatch_line(
                    &ctx_with(HISTORICAL_WORKFLOW_ROOT_FIXTURE),
                    inflight.clone(),
                    &line,
                )
                .await,
            );
            assert_eq!(response.error.unwrap().code, -32602);
            assert!(inflight.drain_all().await.is_empty());
        }
        assert!(matches!(
            dispatch_with_features(
                HISTORICAL_WORKFLOW_ROOT_FIXTURE,
                &call(2, "get_workflow_state", json!({})),
            )
            .await,
            LineAction::Spawn(_)
        ));
        assert!(matches!(
            dispatch_with_features(
                HISTORICAL_WORKFLOW_ROOT_FIXTURE,
                &call(3, "get_workflow_state", json!({ "detail": "index" }))
            )
            .await,
            LineAction::Spawn(_)
        ));
    }

    #[tokio::test]
    async fn get_workflow_state_request_id_limit_is_pre_inflight_and_bounded() {
        let accepted = ascii_string_id_with_serialized_len(256);
        let accepted_line = json!({
            "jsonrpc": "2.0", "id": accepted, "method": "tools/call",
            "params": { "name": "get_workflow_state", "arguments": {} }
        })
        .to_string();
        assert!(matches!(
            dispatch_with_features(HISTORICAL_WORKFLOW_ROOT_FIXTURE, &accepted_line).await,
            LineAction::Spawn(_)
        ));

        for rejected in [
            ascii_string_id_with_serialized_len(257),
            Value::String("\\".repeat(128)),
            Value::String("界".repeat(85)),
        ] {
            let inflight = Arc::new(InflightCalls::new());
            let line = json!({
                "jsonrpc": "2.0", "id": rejected, "method": "tools/call",
                "params": { "name": "get_workflow_state", "arguments": {} }
            })
            .to_string();
            let response = unwrap_respond(
                dispatch_line(
                    &ctx_with(HISTORICAL_WORKFLOW_ROOT_FIXTURE),
                    inflight.clone(),
                    &line,
                )
                .await,
            );
            assert_eq!(response.id, Value::Null);
            assert_eq!(response.error.as_ref().unwrap().code, -32600);
            assert!(inflight.drain_all().await.is_empty());
            assert!(
                serialize_jsonrpc_line(&response).unwrap().len()
                    <= GET_WORKFLOW_STATE_MAX_RESULT_BYTES
            );
        }
    }

    #[test]
    fn get_workflow_state_index_jsonrpc_line_under_7680_bytes() {
        for id in [json!(1), json!("quote\"slash\\界")] {
            let response = render_get_workflow_state_response_with_budget(
                id,
                representative_large_index(),
                GET_WORKFLOW_STATE_MAX_RESULT_BYTES,
            )
            .unwrap();
            let line = serialize_jsonrpc_line(&response).unwrap();
            assert!(
                line.len() <= GET_WORKFLOW_STATE_MAX_RESULT_BYTES,
                "{} bytes",
                line.len()
            );
            let result = response.result.as_ref().unwrap();
            assert_eq!(result["isError"], false);
            assert!(result.get("structuredContent").is_none());
            let index = response_index(&response);
            assert_eq!(index["manifest_revision"], 7);
            assert_eq!(index["graph_revision"], 11);
            assert_eq!(index["gates"][0]["next_gate_cycle"], 3);
            assert_eq!(
                index["latest_plan_review"]["next_action"],
                "continue_review"
            );
            assert_eq!(index["latest_plan_review"]["important_count"], 8);
            assert!(index
                .pointer("/latest_plan_review/findings/0/summary")
                .is_none());
            assert_eq!(index["completion"]["protocol_version"], 2);
            assert_eq!(
                index["completion"]["card"]["attention"]["attention_id"],
                "attention-budget"
            );
            assert_eq!(
                index["completion"]["card"]["summary"]
                    .as_str()
                    .unwrap()
                    .len(),
                1024
            );
        }
    }

    #[test]
    fn get_workflow_state_bounds_long_completion_cas_node_id_without_losing_truth() {
        let raw_node_id = format!("review/path/{}", "n".repeat(9_000));
        let mut index = representative_large_index();
        index
            .completion
            .as_mut()
            .unwrap()
            .card
            .attention
            .as_mut()
            .unwrap()
            .node_id = raw_node_id.clone();

        let response = render_get_workflow_state_response_with_budget(
            json!(1),
            index,
            GET_WORKFLOW_STATE_MAX_RESULT_BYTES,
        )
        .unwrap();
        let line = serialize_jsonrpc_line(&response).unwrap();
        assert!(line.len() <= GET_WORKFLOW_STATE_MAX_RESULT_BYTES);
        let result = response.result.as_ref().unwrap();
        assert_eq!(result["isError"], false);
        let projected = response_index(&response);
        assert_eq!(projected["completion"]["card"]["state"], "needs_decision");
        assert_eq!(
            projected["completion"]["card"]["attention"]["node_id"],
            crate::acp::delegation::workflow::safe_public_id(&raw_node_id)
        );
    }

    #[test]
    fn get_workflow_state_packaging_text_equals_projected_index_without_structured_copy() {
        let index = representative_large_index();
        let expected = index.public_value().unwrap();
        let response = render_get_workflow_state_response(json!(9), index);
        let result = response.result.as_ref().unwrap();
        assert_eq!(result["isError"], false);
        assert!(result.get("structuredContent").is_none());
        assert_eq!(response_index(&response), expected);
    }

    #[test]
    fn get_workflow_state_each_budget_transition_appends_exact_ordered_token() {
        let original = representative_large_index();
        let id = json!("quote\"slash\\界");
        let mut previous_len = serialize_jsonrpc_line(&omission_candidate(&original, id.clone()))
            .unwrap()
            .len();
        for (target_index, target_step) in WorkflowIndexOmissionStep::ALL.into_iter().enumerate() {
            let mut before_index = original.clone();
            for preceding in WorkflowIndexOmissionStep::ALL
                .into_iter()
                .take(target_index)
            {
                before_index.apply_omission_step(preceding);
            }
            let before = serialize_jsonrpc_line(&omission_candidate(&before_index, id.clone()))
                .unwrap()
                .len();
            assert_eq!(before, previous_len);

            let mut after_index = before_index.clone();
            assert!(
                after_index.apply_omission_step(target_step),
                "representative fixture must exercise {}",
                target_step.token()
            );
            let after = serialize_jsonrpc_line(&omission_candidate(&after_index, id.clone()))
                .unwrap()
                .len();
            assert!(
                after < before,
                "{} must reduce the line: before={before}, after={after}",
                target_step.token()
            );

            let response =
                render_get_workflow_state_response_with_budget(id.clone(), original.clone(), after)
                    .unwrap();
            assert_eq!(
                response_index(&response),
                after_index.public_value().unwrap()
            );
            assert_eq!(
                response_index(&response)["omitted"],
                after_index.public_value().unwrap()["omitted"],
                "{} must preserve canonical omission order",
                target_step.token()
            );
            previous_len = after;
        }
    }

    #[test]
    fn get_workflow_state_render_is_byte_deterministic() {
        let id = json!("quote\"slash\\界");
        let first = render_get_workflow_state_response_with_budget(
            id.clone(),
            representative_large_index(),
            GET_WORKFLOW_STATE_MAX_RESULT_BYTES,
        )
        .unwrap();
        let second = render_get_workflow_state_response_with_budget(
            id,
            representative_large_index(),
            GET_WORKFLOW_STATE_MAX_RESULT_BYTES,
        )
        .unwrap();
        assert_eq!(
            serialize_jsonrpc_line(&first).unwrap(),
            serialize_jsonrpc_line(&second).unwrap()
        );
    }

    #[test]
    fn get_workflow_state_protected_oversize_returns_bounded_typed_error() {
        let mut index = representative_large_index();
        let pathological = "quote\"slash\\界".repeat(2_000);
        index.workflow_id = pathological.clone();
        index.plan_target_rel_path = pathological.clone();
        if let Some(review) = index.latest_plan_review.as_mut() {
            review.covered_author_task_id = pathological.clone();
            for source in &mut review.recovery_sources {
                source.report_file = Some(pathological.clone());
                source.latest_task_id = None;
            }
        }
        let id = ascii_string_id_with_serialized_len(GET_WORKFLOW_STATE_MAX_REQUEST_ID_BYTES);
        let response = render_get_workflow_state_response_with_budget(
            id,
            index,
            GET_WORKFLOW_STATE_MAX_RESULT_BYTES,
        )
        .unwrap();
        let result = response.result.as_ref().unwrap();
        assert_eq!(result["isError"], true);
        assert_eq!(
            result["structuredContent"]["error"]["code"],
            "payload_too_large"
        );
        let line = serialize_jsonrpc_line(&response).unwrap();
        assert!(line.len() <= GET_WORKFLOW_STATE_MAX_RESULT_BYTES);
        assert!(!String::from_utf8(line).unwrap().contains(&pathological));
    }

    #[test]
    fn get_workflow_state_open_findings_never_lose_all_recovery_pointers() {
        let mut index = representative_large_index();
        for step in WorkflowIndexOmissionStep::ALL {
            index.apply_omission_step(step);
            index.validate_protected_minimum().unwrap();
            let review = index.latest_plan_review.as_ref().unwrap();
            assert!(review
                .recovery_sources
                .iter()
                .any(|source| source.report_file.is_some() || source.latest_task_id.is_some()));
        }
    }

    #[test]
    fn get_workflow_state_broker_errors_keep_existing_typed_tool_error_shape() {
        let outcome = json!({
            "error": { "code": "validation", "message": "invalid workflow state" }
        });
        let response = render_get_workflow_state_outcome_with_budget(
            json!(17),
            outcome.clone(),
            GET_WORKFLOW_STATE_MAX_RESULT_BYTES,
        )
        .unwrap();
        let result = response.result.as_ref().unwrap();
        assert_eq!(result["content"][0]["text"], "invalid workflow state");
        assert_eq!(result["isError"], true);
        assert_eq!(result["structuredContent"], outcome);
        assert!(
            serialize_jsonrpc_line(&response).unwrap().len() <= GET_WORKFLOW_STATE_MAX_RESULT_BYTES
        );
    }

    #[test]
    fn get_workflow_state_untrusted_error_shapes_use_bounded_internal_error() {
        let cases = [
            (
                "unknown code",
                json!({
                    "error": {
                        "code": "injected_unknown_code",
                        "message": "untrusted-unknown-code-message"
                    }
                }),
                "untrusted-unknown-code-message",
            ),
            (
                "missing code",
                json!({ "error": { "message": "untrusted-missing-code-message" } }),
                "untrusted-missing-code-message",
            ),
            (
                "non-string code",
                json!({
                    "error": {
                        "code": 7,
                        "message": "untrusted-non-string-code-message"
                    }
                }),
                "untrusted-non-string-code-message",
            ),
            (
                "string error",
                json!({ "error": "untrusted-string-error" }),
                "untrusted-string-error",
            ),
            (
                "null error",
                json!({ "error": null, "source": "untrusted-null-error" }),
                "untrusted-null-error",
            ),
            (
                "numeric error",
                json!({ "error": 9, "source": "untrusted-numeric-error" }),
                "untrusted-numeric-error",
            ),
            (
                "array error",
                json!({ "error": ["untrusted-array-error"] }),
                "untrusted-array-error",
            ),
            (
                "boolean error",
                json!({ "error": true, "source": "untrusted-boolean-error" }),
                "untrusted-boolean-error",
            ),
        ];

        for (label, outcome, untrusted) in cases {
            let id = ascii_string_id_with_serialized_len(GET_WORKFLOW_STATE_MAX_REQUEST_ID_BYTES);
            let response = render_get_workflow_state_outcome_with_budget(
                id,
                outcome,
                GET_WORKFLOW_STATE_MAX_RESULT_BYTES,
            )
            .unwrap_or_else(|error| {
                panic!("{label} returned JSON-RPC serialization error: {error}")
            });
            let result = response.result.as_ref().expect("bounded tool result");
            assert_eq!(result["isError"], true, "{label}");
            assert_eq!(
                result["content"][0]["text"],
                "get_workflow_state failed; inspect structuredContent.error.code",
                "{label}"
            );
            assert_eq!(
                result["structuredContent"]["error"]["code"], "internal_error",
                "{label}"
            );
            assert_eq!(
                result["structuredContent"]["error"]["message"], "get_workflow_state failed",
                "{label}"
            );
            let line = serialize_jsonrpc_line(&response).unwrap();
            assert!(
                line.len() <= GET_WORKFLOW_STATE_MAX_RESULT_BYTES,
                "{label} emitted {} bytes",
                line.len()
            );
            assert!(
                !String::from_utf8(line).unwrap().contains(untrusted),
                "{label} echoed untrusted broker bytes"
            );
        }
    }

    #[tokio::test]
    async fn get_workflow_state_oversized_missing_workflow_id_error_uses_bounded_typed_fallback() {
        let workflow_id = "missing-quote\"slash\\界".repeat(600);
        let broker_message = format!("workflow not found: {workflow_id}");
        let (socket_path, server) = workflow_broker_with_outcome(json!({
            "error": { "code": "not_found", "message": broker_message }
        }));
        let mut context = ctx_with(HISTORICAL_WORKFLOW_ROOT_FIXTURE);
        context.socket_path = socket_path;
        let line = json!({
            "jsonrpc": "2.0",
            "id": "short-id",
            "method": "tools/call",
            "params": {
                "name": "get_workflow_state",
                "arguments": { "workflow_id": workflow_id }
            }
        })
        .to_string();
        let action = dispatch_line(&context, Arc::new(InflightCalls::new()), &line).await;
        let LineAction::Spawn(spawned) = action else {
            panic!("expected workflow state spawn");
        };
        let response = spawned.future.await.response.unwrap();
        server.await.unwrap();
        let result = response.result.as_ref().unwrap();
        assert_eq!(result["isError"], true);
        assert_eq!(result["structuredContent"]["error"]["code"], "not_found");
        let serialized = serialize_jsonrpc_line(&response).unwrap();
        let serialized_text = String::from_utf8(serialized.clone()).unwrap();
        assert!(!serialized_text.contains(&workflow_id));
        assert!(!serialized_text.contains(&broker_message));
        assert!(serialized.len() <= GET_WORKFLOW_STATE_MAX_RESULT_BYTES);
    }

    #[test]
    fn get_workflow_state_synthetic_broker_error_uses_bounded_typed_fallback() {
        let source_message = "deterministic persistence failure ".repeat(512);
        let outcome = json!({
            "error": { "code": "persistence", "message": source_message }
        });
        let id = ascii_string_id_with_serialized_len(GET_WORKFLOW_STATE_MAX_REQUEST_ID_BYTES);
        let first = render_get_workflow_state_outcome_with_budget(
            id.clone(),
            outcome.clone(),
            GET_WORKFLOW_STATE_MAX_RESULT_BYTES,
        )
        .unwrap();
        let second = render_get_workflow_state_outcome_with_budget(
            id,
            outcome,
            GET_WORKFLOW_STATE_MAX_RESULT_BYTES,
        )
        .unwrap();
        let result = first.result.as_ref().unwrap();
        assert_eq!(result["isError"], true);
        assert_eq!(result["structuredContent"]["error"]["code"], "persistence");
        assert_eq!(
            result["content"][0]["text"],
            "get_workflow_state failed; inspect structuredContent.error.code"
        );
        assert_eq!(
            result["structuredContent"]["error"]["message"],
            "get_workflow_state failed"
        );
        let first_line = serialize_jsonrpc_line(&first).unwrap();
        assert_eq!(first_line, serialize_jsonrpc_line(&second).unwrap());
        assert!(first_line.len() <= GET_WORKFLOW_STATE_MAX_RESULT_BYTES);
        assert!(!String::from_utf8(first_line)
            .unwrap()
            .contains(&source_message));
    }

    #[test]
    fn get_workflow_state_all_known_bounded_error_codes_fit_with_max_request_id() {
        let codes = [
            "risk_assessment_invalid",
            "task_route_mismatch",
            "validation",
            "reviewer_set_mismatch",
            "plan_review",
            "not_found",
            "cross_parent",
            "stale_manifest_revision",
            "stale_graph_revision",
            "publication_token_mismatch",
            "publication_token_conflict",
            "admitted_node_identity_mutation",
            "cohort_frozen",
            "reviewed_task_stale",
            "artifact_digest_mismatch",
            "gate_not_ready",
            "gate_cycle_conflict",
            "execution_gate_settle_rejected",
            "approval_with_open_findings",
            "approval_rejected_failed_reviewer",
            "summary_too_large",
            "negative_finding_counts",
            "parent_not_found",
            "busy",
            "persistence",
            "internal_error",
        ];
        for code in codes {
            let id = ascii_string_id_with_serialized_len(GET_WORKFLOW_STATE_MAX_REQUEST_ID_BYTES);
            let response = render_bounded_workflow_error(id, code);
            assert_eq!(
                response.result.as_ref().unwrap()["structuredContent"]["error"]["code"],
                code
            );
            let line = serialize_jsonrpc_line(&response).unwrap();
            assert!(
                line.len() <= GET_WORKFLOW_STATE_MAX_RESULT_BYTES,
                "bounded {code} line was {} bytes",
                line.len()
            );
        }

        for untrusted in [
            json!({ "error": {} }),
            json!({ "error": { "code": 7 } }),
            json!({ "error": { "code": "unknown-injected-code" } }),
        ] {
            assert_eq!(
                workflow_state_stable_error_code(&untrusted),
                "internal_error"
            );
        }
    }

    #[tokio::test]
    async fn settle_workflow_gate_rejects_legacy_fields_synchronously() {
        for (field, value) in [
            ("manifest_revision", json!(1)),
            ("gate_cycle", json!(1)),
            ("outcome", json!("changes_requested")),
            ("evidence", json!({ "kind": "design" })),
        ] {
            let mut args = json!({
                "workflow_id": "wf-1",
                "gate_id": "design",
                "expected_graph_revision": 1,
                "expected_gate_cycle": 1,
                "expected_outcome": "changes_requested",
                "summary": "platform evidence decides",
            });
            args[field] = value;
            let action = dispatch_with_features(
                HISTORICAL_WORKFLOW_ROOT_FIXTURE,
                &call(11, "settle_workflow_gate", args),
            )
            .await;
            let resp = unwrap_respond(action);
            let err = resp
                .error
                .unwrap_or_else(|| panic!("expected -32602 for legacy field {field}"));
            assert_eq!(err.code, -32602, "legacy field {field}");
            assert!(
                err.message.contains(field) && err.message.contains("does not accept"),
                "legacy field {field}: {}",
                err.message
            );
        }
    }

    #[test]
    fn workflow_manifest_v2_plan_settlement_uses_reduced_request() {
        let req = parse_settle_workflow_args(
            &json!({
                "workflow_id": "wf-1",
                "gate_id": "plan",
                "expected_graph_revision": 1,
                "expected_review_round": 1,
                "expected_outcome": "changes_requested",
                "summary": "one important finding"
            }),
            "token",
        )
        .expect("reduced v2 settlement request parses");
        let wire = serde_json::to_value(req).expect("serialize settle broker request");
        for forbidden in ["manifest_revision", "gate_cycle", "outcome", "evidence"] {
            assert!(wire.get(forbidden).is_none(), "wire leaked {forbidden}");
        }
        assert_eq!(wire["expected_review_round"], 1);
        assert_eq!(wire["expected_outcome"], "changes_requested");
    }

    #[test]
    fn workflow_manifest_v2_settlement_rejects_unknown_arguments() {
        for field in [
            "manifest_revision",
            "gate_cycle",
            "outcome",
            "evidence",
            "critical_count",
            "reviewer_task_ids",
            "covered_digest",
            "required_node_set",
            "findings",
            "gate_lineage",
            "scope",
        ] {
            let mut arguments = json!({
                "workflow_id": "wf-1",
                "gate_id": "design",
                "expected_graph_revision": 1,
                "summary": "platform evidence decides"
            });
            arguments[field] = json!("caller-owned");

            let error = parse_settle_workflow_args(&arguments, "token")
                .expect_err("unknown settlement argument must be rejected");
            assert_eq!(
                error,
                format!("settle_workflow_gate does not accept `{field}`")
            );
        }
    }

    #[test]
    fn workflow_manifest_v2_schema_is_compact_and_constructible() {
        let schema: Value = serde_json::from_str(TOOL_SCHEMA_JSON).expect("valid tool schema JSON");
        let tools = schema.as_array().expect("tool catalog array");
        let capabilities = tools
            .iter()
            .find(|tool| tool["name"] == "get_workflow_capabilities")
            .expect("capabilities tool");
        assert_eq!(capabilities["inputSchema"], json!({}));
        assert!(capabilities.get("description").is_none());

        let publish = tools
            .iter()
            .find(|tool| tool["name"] == "publish_workflow_manifest")
            .expect("publish tool");
        assert_eq!(
            publish["inputSchema"]["properties"]["schema_version"]["const"],
            2
        );
        assert_eq!(
            publish["inputSchema"]["properties"]["risk_policy_version"]["const"],
            "b2d_task_risk_v1"
        );
        for required in [
            "plan_target_rel_path",
            "risk_policy_version",
            "task_policies",
        ] {
            assert!(
                publish["inputSchema"]["required"]
                    .as_array()
                    .expect("publish required")
                    .iter()
                    .any(|value| value == required),
                "publish schema must require {required}"
            );
        }

        let settle = tools
            .iter()
            .find(|tool| tool["name"] == "settle_workflow_gate")
            .expect("settle tool");
        let settle_schema = &settle["inputSchema"];
        assert_eq!(settle_schema["additionalProperties"], false);
        let required = settle_schema["required"]
            .as_array()
            .expect("settle required");
        for field in [
            "workflow_id",
            "gate_id",
            "expected_graph_revision",
            "summary",
        ] {
            assert!(required.iter().any(|value| value == field));
        }
        for property in [
            "expected_review_round",
            "expected_gate_cycle",
            "expected_outcome",
            "recovery_authorization_id",
        ] {
            assert!(
                settle_schema["properties"].get(property).is_some(),
                "settlement schema omits {property}"
            );
        }
        for property in ["manifest_revision", "gate_cycle", "outcome", "evidence"] {
            assert!(
                settle_schema["properties"].get(property).is_none(),
                "settlement schema retains legacy property {property}"
            );
        }
        assert!(settle_schema.get("oneOf").is_none());
    }

    #[test]
    fn get_workflow_state_schema_describes_index_recovery() {
        let schema: Value = serde_json::from_str(TOOL_SCHEMA_JSON).expect("valid tool schema JSON");
        let state = schema
            .as_array()
            .expect("tool catalog array")
            .iter()
            .find(|tool| tool["name"] == "get_workflow_state")
            .expect("get_workflow_state tool");

        assert_eq!(
            state["inputSchema"],
            json!({
                "type": "object",
                "properties": {
                    "workflow_id": {},
                    "detail": { "enum": ["index"], "default": "index" }
                }
            })
        );
        assert!(state.get("description").is_none());
    }

    #[tokio::test]
    async fn tools_list_hides_feedback_when_disabled() {
        // Default ctx is delegation-only: check_user_feedback must not appear.
        let names = list_tool_names(
            dispatch_for_test(r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#).await,
        );
        assert!(!names.contains(&"check_user_feedback".to_string()));
        // delegate + Simple registration + continue + status + cancel
        assert_eq!(names.len(), 5);
        assert!(names.contains(&"continue_delegation".to_string()));
    }

    #[tokio::test]
    async fn orchestration_binding_query_catalog_requires_root_delegation_and_coordination() {
        let production = CompanionFeatures {
            delegation: true,
            coordination_v1: true,
            feedback: false,
            ask: false,
            sessions: false,
            workflow_v2: false,
            completion_v2: false,
        };
        let response = unwrap_respond(
            dispatch_with_features(
                production,
                r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#,
            )
            .await,
        );
        let names: Vec<&str> = response.result.as_ref().unwrap()["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["name"].as_str().unwrap())
            .collect();

        assert!(
            names.contains(&"get_delegation_orchestration_bindings"),
            "production root catalog is missing the read-only binding query: {names:?}"
        );
        for retired in WORKFLOW_V2_TOOLS {
            assert!(!names.contains(retired), "retired tool leaked: {retired}");
        }

        for features in [
            CompanionFeatures {
                delegation: false,
                ..production
            },
            CompanionFeatures {
                coordination_v1: false,
                ..production
            },
        ] {
            let hidden = list_tool_names(
                dispatch_with_features(
                    features,
                    r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#,
                )
                .await,
            );
            assert!(!hidden.contains(&"get_delegation_orchestration_bindings".into()));
        }
        let mut child = ctx_with(production);
        child.role = CompanionRole::DelegationChild;
        let hidden = list_tool_names(
            dispatch_line(
                &child,
                Arc::new(InflightCalls::new()),
                r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#,
            )
            .await,
        );
        assert!(!hidden.contains(&"get_delegation_orchestration_bindings".into()));

        for retired in WORKFLOW_V2_TOOLS {
            let call = json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "tools/call",
                "params": { "name": retired, "arguments": {} }
            })
            .to_string();
            let response = unwrap_respond(dispatch_with_features(production, &call).await);
            let error = response.error.expect("retired call is unavailable");
            assert_eq!(error.code, -32602);
            assert!(error.message.contains("unknown tool"));
        }
    }

    #[tokio::test]
    async fn orchestration_binding_query_raw_call_is_strict_and_text_compatible() {
        let production = CompanionFeatures {
            delegation: true,
            coordination_v1: true,
            feedback: false,
            ask: false,
            sessions: false,
            workflow_v2: false,
            completion_v2: false,
        };
        let catalog = unwrap_respond(
            dispatch_with_features(
                production,
                r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#,
            )
            .await,
        );
        let tools = catalog.result.unwrap()["tools"].as_array().unwrap().clone();
        let schema = tools
            .iter()
            .find(|tool| tool["name"] == "get_delegation_orchestration_bindings")
            .unwrap()["inputSchema"]
            .clone();
        assert_eq!(schema["additionalProperties"], false);
        assert_eq!(schema["required"], json!(["namespace"]));
        assert_eq!(schema["properties"]["limit"]["default"], 100);
        assert_eq!(
            schema["dependentRequired"],
            json!({
                "snapshot_id": ["cursor"],
                "cursor": ["snapshot_id"]
            })
        );

        let page = json!({
            "schema_version": 1,
            "namespace": "brainstorm-to-delivery",
            "snapshot_id": "1a641e16-36f4-4ec5-aa4f-18d18e6ab107",
            "snapshot_revision": "0",
            "snapshot_created_at": "2026-08-17T08:00:00Z",
            "snapshot_expires_at": "2026-08-17T08:01:00Z",
            "total_rows": 0,
            "page_start": 0,
            "request_cursor": null,
            "runs": [],
            "next_cursor": null,
            "complete": true
        });
        let (socket_path, server) = orchestration_broker_with_outcome(page.clone());
        let mut context = ctx_with(production);
        context.socket_path = socket_path;
        let call = json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": {
                "name": "get_delegation_orchestration_bindings",
                "arguments": { "namespace": "brainstorm-to-delivery" }
            }
        })
        .to_string();
        let LineAction::Spawn(spawned) =
            dispatch_line(&context, Arc::new(InflightCalls::new()), &call).await
        else {
            panic!("valid query must reach the broker")
        };
        let response = spawned.future.await.response.unwrap();
        server.await.unwrap();
        let result = response.result.unwrap();
        assert_eq!(result["structuredContent"], page);
        let text = result["content"][0]["text"]
            .as_str()
            .expect("successful binding page must be available to text-only MCP hosts");
        assert_eq!(serde_json::from_str::<Value>(text).unwrap(), page);
        assert_eq!(result["isError"], false);
    }

    #[tokio::test]
    async fn delegation_catalog_compaction_page_mode_wire_bytes_are_unchanged() {
        let raw_call = r#"{"jsonrpc":"2.0","id":"page-fixture","method":"tools/call","params":{"name":"get_delegation_orchestration_bindings","arguments":{"namespace":"brainstorm-to-delivery","limit":100,"snapshot_id":"1a641e16-36f4-4ec5-aa4f-18d18e6ab107","cursor":"cursor_1"}}}"#;
        let request: JsonRpcRequest = serde_json::from_str(raw_call).unwrap();
        let query: OrchestrationBindingQueryRequest =
            serde_json::from_value(request.params["arguments"].clone()).unwrap();
        let request_bytes = serde_json::to_vec(&query).unwrap();

        let page = json!({
            "schema_version": 1,
            "namespace": "brainstorm-to-delivery",
            "snapshot_id": "1a641e16-36f4-4ec5-aa4f-18d18e6ab107",
            "snapshot_revision": "17",
            "snapshot_created_at": "2026-08-26T08:00:00Z",
            "snapshot_expires_at": "2026-08-26T08:01:00Z",
            "total_rows": 0,
            "page_start": 0,
            "request_cursor": "cursor_1",
            "runs": [],
            "next_cursor": null,
            "complete": true
        });
        let success_line = serialize_jsonrpc_line(&ok(
            json!("page-fixture"),
            render_orchestration_binding_page(&page),
        ))
        .unwrap();
        let error_response = unwrap_respond(
            dispatch_with_features(
                GROK_FEATURES,
                &call(
                    19,
                    "get_delegation_orchestration_bindings",
                    json!({ "namespace": 7 }),
                ),
            )
            .await,
        );
        let error_line = serialize_jsonrpc_line(&error_response).unwrap();

        println!(
            "page-mode bytes: request={}, success={}, error={}",
            request_bytes.len(),
            success_line.len(),
            error_line.len()
        );
        assert_eq!(
            request_bytes,
            br#"{"namespace":"brainstorm-to-delivery","limit":100,"snapshot_id":"1a641e16-36f4-4ec5-aa4f-18d18e6ab107","cursor":"cursor_1"}"#
        );
        assert_eq!(
            success_line,
            concat!(
                r#"{"jsonrpc":"2.0","id":"page-fixture","result":{"content":[{"text":"{\"complete\":true,\"namespace\":\"brainstorm-to-delivery\",\"next_cursor\":null,\"page_start\":0,\"request_cursor\":\"cursor_1\",\"runs\":[],\"schema_version\":1,\"snapshot_created_at\":\"2026-08-26T08:00:00Z\",\"snapshot_expires_at\":\"2026-08-26T08:01:00Z\",\"snapshot_id\":\"1a641e16-36f4-4ec5-aa4f-18d18e6ab107\",\"snapshot_revision\":\"17\",\"total_rows\":0}","type":"text"}],"isError":false,"structuredContent":{"complete":true,"namespace":"brainstorm-to-delivery","next_cursor":null,"page_start":0,"request_cursor":"cursor_1","runs":[],"schema_version":1,"snapshot_created_at":"2026-08-26T08:00:00Z","snapshot_expires_at":"2026-08-26T08:01:00Z","snapshot_id":"1a641e16-36f4-4ec5-aa4f-18d18e6ab107","snapshot_revision":"17","total_rows":0}}}"#,
                "\n"
            )
            .as_bytes()
        );
        assert_eq!(
            error_line,
            concat!(
                r#"{"jsonrpc":"2.0","id":19,"result":{"content":[{"text":"invalid orchestration binding query","type":"text"}],"isError":true,"structuredContent":{"error":{"code":"orchestration_binding_query_invalid","message":"invalid orchestration binding query"}}}}"#,
                "\n"
            )
            .as_bytes()
        );
    }

    #[tokio::test]
    async fn delegation_catalog_compaction_removed_leaf_runtime_parity() {
        let high_fractional: Value =
            serde_json::from_str("9007199254740992.5").expect("valid JSON number");
        let u64_overflow: Value =
            serde_json::from_str("18446744073709551616").expect("valid JSON number");
        let publish = json!({
            "schema_version": 2,
            "workflow_kind": "simple",
            "publication_token": "publication-token",
            "workflow_state": "draft",
            "plan_target_rel_path": "docs/plan.md",
            "risk_policy_version": "b2d_task_risk_v1",
            "task_policies": [],
            "phases": [],
            "nodes": [],
            "edges": [],
            "gates": []
        });
        let settle = json!({
            "workflow_id": "workflow-a",
            "gate_id": "gate-a",
            "expected_graph_revision": 0,
            "summary": "reviewed"
        });
        let recover = json!({
            "workflow_id": "workflow-a",
            "recovery_authorization_id": "authorization-a",
            "expected_manifest_revision": 1,
            "correlation_id": "recovery-a"
        });
        let authorization = json!({
            "subject_kind": "workflow",
            "subject_id": "workflow-a",
            "correlation_id": "authorization-a"
        });
        let cases = vec![
            (
                "registration plan type",
                "register_simple_workflow",
                json!({"plan_rel_path": 7}),
            ),
            (
                "registration plan empty",
                "register_simple_workflow",
                json!({"plan_rel_path": ""}),
            ),
            (
                "registration progress type",
                "register_simple_workflow",
                json!({"plan_rel_path": "docs/plan.md", "progress_rel_path": 7}),
            ),
            (
                "registration progress null",
                "register_simple_workflow",
                json!({"plan_rel_path": "docs/plan.md", "progress_rel_path": null}),
            ),
            (
                "registration progress empty",
                "register_simple_workflow",
                json!({"plan_rel_path": "docs/plan.md", "progress_rel_path": ""}),
            ),
            (
                "status wait type",
                "get_delegation_status",
                json!({"task_ids": ["task-a"], "wait_ms": "0"}),
            ),
            (
                "status wait below minimum",
                "get_delegation_status",
                json!({"task_ids": ["task-a"], "wait_ms": -1}),
            ),
            (
                "status wait above maximum",
                "get_delegation_status",
                json!({"task_ids": ["task-a"], "wait_ms": 1}),
            ),
            (
                "status return type",
                "get_delegation_status",
                json!({"task_ids": ["task-a"], "wait_ms": 0, "return_when": 7}),
            ),
            (
                "status return enum",
                "get_delegation_status",
                json!({"task_ids": ["task-a"], "wait_ms": 0, "return_when": "later"}),
            ),
            (
                "cancel task type",
                "cancel_delegation",
                json!({"task_id": 7, "reason": "taskfail"}),
            ),
            (
                "cancel task empty",
                "cancel_delegation",
                json!({"task_id": "", "reason": "taskfail"}),
            ),
            (
                "cancel reason type",
                "cancel_delegation",
                json!({"task_id": "task-a", "reason": 7}),
            ),
            (
                "cancel reason enum",
                "cancel_delegation",
                json!({"task_id": "task-a", "reason": "later"}),
            ),
            (
                "session max type",
                "get_session_info",
                json!({"session_id": 1, "max_messages": "20"}),
            ),
            (
                "session max below minimum",
                "get_session_info",
                json!({"session_id": 1, "max_messages": -1}),
            ),
            (
                "session max high fractional",
                "get_session_info",
                json!({"session_id": 1, "max_messages": high_fractional.clone()}),
            ),
            (
                "session max u64 overflow",
                "get_session_info",
                json!({"session_id": 1, "max_messages": u64_overflow.clone()}),
            ),
            (
                "reply request type",
                "reply_to_delegation",
                json!({"request_id": 7, "reply": "yes"}),
            ),
            (
                "reply request empty",
                "reply_to_delegation",
                json!({"request_id": "", "reply": "yes"}),
            ),
            (
                "reply type",
                "reply_to_delegation",
                json!({"request_id": "request-a", "reply": 7}),
            ),
            (
                "reply empty",
                "reply_to_delegation",
                json!({"request_id": "request-a", "reply": ""}),
            ),
            (
                "reply overlength",
                "reply_to_delegation",
                json!({"request_id": "request-a", "reply": "x".repeat(16 * 1024 + 1)}),
            ),
            (
                "authorization subject kind type",
                "request_recovery_authorization",
                with_argument(authorization.clone(), "subject_kind", json!(7)),
            ),
            (
                "authorization subject kind enum",
                "request_recovery_authorization",
                with_argument(authorization.clone(), "subject_kind", json!("other")),
            ),
            (
                "authorization subject type",
                "request_recovery_authorization",
                with_argument(authorization.clone(), "subject_id", json!(7)),
            ),
            (
                "authorization subject empty",
                "request_recovery_authorization",
                with_argument(authorization.clone(), "subject_id", json!("")),
            ),
            (
                "authorization correlation type",
                "request_recovery_authorization",
                with_argument(authorization.clone(), "correlation_id", json!(7)),
            ),
            (
                "authorization correlation malformed",
                "request_recovery_authorization",
                with_argument(authorization.clone(), "correlation_id", json!(".bad")),
            ),
            (
                "authorization correlation overlength",
                "request_recovery_authorization",
                with_argument(
                    authorization.clone(),
                    "correlation_id",
                    json!("a".repeat(129)),
                ),
            ),
            (
                "authorization reason type",
                "request_recovery_authorization",
                with_argument(authorization.clone(), "proposed_user_reason", json!(7)),
            ),
            (
                "authorization reason overlength",
                "request_recovery_authorization",
                with_argument(
                    authorization.clone(),
                    "proposed_user_reason",
                    json!("x".repeat(4097)),
                ),
            ),
            (
                "capabilities null root",
                "get_workflow_capabilities",
                Value::Null,
            ),
            (
                "capabilities array root",
                "get_workflow_capabilities",
                json!([]),
            ),
            (
                "workflow state id type",
                "get_workflow_state",
                json!({"workflow_id": 7}),
            ),
            (
                "workflow state id null",
                "get_workflow_state",
                json!({"workflow_id": null}),
            ),
            (
                "workflow state id empty",
                "get_workflow_state",
                json!({"workflow_id": ""}),
            ),
            (
                "workflow state detail type",
                "get_workflow_state",
                json!({"detail": 7}),
            ),
            (
                "workflow state detail enum",
                "get_workflow_state",
                json!({"detail": "full"}),
            ),
            (
                "recover workflow type",
                "recover_workflow",
                with_argument(recover.clone(), "workflow_id", json!(7)),
            ),
            (
                "recover workflow empty",
                "recover_workflow",
                with_argument(recover.clone(), "workflow_id", json!("")),
            ),
            (
                "recover authorization type",
                "recover_workflow",
                with_argument(recover.clone(), "recovery_authorization_id", json!(7)),
            ),
            (
                "recover authorization empty",
                "recover_workflow",
                with_argument(recover.clone(), "recovery_authorization_id", json!("")),
            ),
            (
                "recover revision type",
                "recover_workflow",
                with_argument(recover.clone(), "expected_manifest_revision", json!("1")),
            ),
            (
                "recover revision below minimum",
                "recover_workflow",
                with_argument(recover.clone(), "expected_manifest_revision", json!(0)),
            ),
            (
                "recover revision high fractional",
                "recover_workflow",
                with_argument(
                    recover.clone(),
                    "expected_manifest_revision",
                    high_fractional.clone(),
                ),
            ),
            (
                "recover revision u64 overflow",
                "recover_workflow",
                with_argument(
                    recover.clone(),
                    "expected_manifest_revision",
                    u64_overflow.clone(),
                ),
            ),
            (
                "recover correlation type",
                "recover_workflow",
                with_argument(recover.clone(), "correlation_id", json!(7)),
            ),
            (
                "recover correlation malformed",
                "recover_workflow",
                with_argument(recover.clone(), "correlation_id", json!(".bad")),
            ),
            (
                "recover correlation overlength",
                "recover_workflow",
                with_argument(recover.clone(), "correlation_id", json!("a".repeat(129))),
            ),
            (
                "publish schema type",
                "publish_workflow_manifest",
                with_argument(publish.clone(), "schema_version", json!("2")),
            ),
            (
                "publish schema const",
                "publish_workflow_manifest",
                with_argument(publish.clone(), "schema_version", json!(1)),
            ),
            (
                "publish workflow type",
                "publish_workflow_manifest",
                with_argument(publish.clone(), "workflow_id", json!(7)),
            ),
            (
                "publish workflow null",
                "publish_workflow_manifest",
                with_argument(publish.clone(), "workflow_id", Value::Null),
            ),
            (
                "publish revision type",
                "publish_workflow_manifest",
                with_argument(publish.clone(), "expected_manifest_revision", json!("0")),
            ),
            (
                "publish revision below minimum",
                "publish_workflow_manifest",
                with_argument(publish.clone(), "expected_manifest_revision", json!(-1)),
            ),
            (
                "publish revision high fractional",
                "publish_workflow_manifest",
                with_argument(
                    publish.clone(),
                    "expected_manifest_revision",
                    high_fractional.clone(),
                ),
            ),
            (
                "publish revision u64 overflow",
                "publish_workflow_manifest",
                with_argument(
                    publish.clone(),
                    "expected_manifest_revision",
                    u64_overflow.clone(),
                ),
            ),
            (
                "publish plan target type",
                "publish_workflow_manifest",
                with_argument(publish.clone(), "plan_target_rel_path", json!(7)),
            ),
            (
                "publish risk policy type",
                "publish_workflow_manifest",
                with_argument(publish.clone(), "risk_policy_version", json!(7)),
            ),
            (
                "publish risk policy const",
                "publish_workflow_manifest",
                with_argument(publish.clone(), "risk_policy_version", json!("other")),
            ),
            (
                "publish task policies type",
                "publish_workflow_manifest",
                with_argument(publish.clone(), "task_policies", json!({})),
            ),
            (
                "settle workflow type",
                "settle_workflow_gate",
                with_argument(settle.clone(), "workflow_id", json!(7)),
            ),
            (
                "settle workflow empty",
                "settle_workflow_gate",
                with_argument(settle.clone(), "workflow_id", json!("")),
            ),
            (
                "settle gate type",
                "settle_workflow_gate",
                with_argument(settle.clone(), "gate_id", json!(7)),
            ),
            (
                "settle gate empty",
                "settle_workflow_gate",
                with_argument(settle.clone(), "gate_id", json!("")),
            ),
            (
                "settle graph revision type",
                "settle_workflow_gate",
                with_argument(settle.clone(), "expected_graph_revision", json!("0")),
            ),
            (
                "settle graph revision below minimum",
                "settle_workflow_gate",
                with_argument(settle.clone(), "expected_graph_revision", json!(-1)),
            ),
            (
                "settle graph revision high fractional",
                "settle_workflow_gate",
                with_argument(
                    settle.clone(),
                    "expected_graph_revision",
                    high_fractional.clone(),
                ),
            ),
            (
                "settle graph revision u64 overflow",
                "settle_workflow_gate",
                with_argument(
                    settle.clone(),
                    "expected_graph_revision",
                    u64_overflow.clone(),
                ),
            ),
            (
                "settle review round type",
                "settle_workflow_gate",
                with_argument(settle.clone(), "expected_review_round", json!("1")),
            ),
            (
                "settle review round below minimum",
                "settle_workflow_gate",
                with_argument(settle.clone(), "expected_review_round", json!(0)),
            ),
            (
                "settle review round high fractional",
                "settle_workflow_gate",
                with_argument(
                    settle.clone(),
                    "expected_review_round",
                    high_fractional.clone(),
                ),
            ),
            (
                "settle review round u64 overflow",
                "settle_workflow_gate",
                with_argument(
                    settle.clone(),
                    "expected_review_round",
                    u64_overflow.clone(),
                ),
            ),
            (
                "settle gate cycle type",
                "settle_workflow_gate",
                with_argument(settle.clone(), "expected_gate_cycle", json!("1")),
            ),
            (
                "settle gate cycle below minimum",
                "settle_workflow_gate",
                with_argument(settle.clone(), "expected_gate_cycle", json!(0)),
            ),
            (
                "settle gate cycle high fractional",
                "settle_workflow_gate",
                with_argument(
                    settle.clone(),
                    "expected_gate_cycle",
                    high_fractional.clone(),
                ),
            ),
            (
                "settle gate cycle u64 overflow",
                "settle_workflow_gate",
                with_argument(settle.clone(), "expected_gate_cycle", u64_overflow.clone()),
            ),
            (
                "settle outcome type",
                "settle_workflow_gate",
                with_argument(settle.clone(), "expected_outcome", json!(7)),
            ),
            (
                "settle outcome enum",
                "settle_workflow_gate",
                with_argument(settle.clone(), "expected_outcome", json!("later")),
            ),
            (
                "settle authorization type",
                "settle_workflow_gate",
                with_argument(settle.clone(), "recovery_authorization_id", json!(7)),
            ),
            (
                "settle authorization null",
                "settle_workflow_gate",
                with_argument(settle.clone(), "recovery_authorization_id", Value::Null),
            ),
            (
                "settle authorization empty",
                "settle_workflow_gate",
                with_argument(settle.clone(), "recovery_authorization_id", json!("")),
            ),
            (
                "settle summary type",
                "settle_workflow_gate",
                with_argument(settle.clone(), "summary", json!(7)),
            ),
        ];

        for (label, tool, arguments) in cases {
            match dispatch_with_features(GROK_FEATURES, &call(91, tool, arguments)).await {
                LineAction::Respond(response) => {
                    let error = response
                        .error
                        .unwrap_or_else(|| panic!("{label}: invalid call returned success"));
                    assert_eq!(error.code, -32602, "{label}");
                }
                LineAction::Spawn(_) => panic!("{label}: invalid call reached broker"),
                LineAction::Silent => panic!("{label}: invalid call was silent"),
            }
        }

        let snapshot_id = "1a641e16-36f4-4ec5-aa4f-18d18e6ab107";
        for (label, arguments) in [
            ("binding namespace type", json!({"namespace": 7})),
            ("binding namespace empty", json!({"namespace": ""})),
            ("binding namespace malformed", json!({"namespace": "Upper"})),
            (
                "binding limit type",
                json!({"namespace": "brainstorm-to-delivery", "limit": "100"}),
            ),
            (
                "binding limit below minimum",
                json!({"namespace": "brainstorm-to-delivery", "limit": 0}),
            ),
            (
                "binding limit above runtime maximum",
                json!({"namespace": "brainstorm-to-delivery", "limit": 201}),
            ),
            (
                "binding snapshot type",
                json!({"namespace": "brainstorm-to-delivery", "snapshot_id": 7, "cursor": "cursor"}),
            ),
            (
                "binding snapshot null",
                json!({"namespace": "brainstorm-to-delivery", "snapshot_id": null, "cursor": "cursor"}),
            ),
            (
                "binding cursor type",
                json!({"namespace": "brainstorm-to-delivery", "snapshot_id": snapshot_id, "cursor": 7}),
            ),
            (
                "binding cursor empty",
                json!({"namespace": "brainstorm-to-delivery", "snapshot_id": snapshot_id, "cursor": ""}),
            ),
            (
                "binding cursor malformed",
                json!({"namespace": "brainstorm-to-delivery", "snapshot_id": snapshot_id, "cursor": "not+base64url"}),
            ),
            (
                "binding cursor overlength",
                json!({"namespace": "brainstorm-to-delivery", "snapshot_id": snapshot_id, "cursor": "x".repeat(129)}),
            ),
        ] {
            let response = unwrap_respond(
                dispatch_with_features(
                    GROK_FEATURES,
                    &call(92, "get_delegation_orchestration_bindings", arguments),
                )
                .await,
            );
            assert!(response.error.is_none(), "{label}");
            assert_eq!(
                response.result.unwrap()["structuredContent"]["error"]["code"],
                "orchestration_binding_query_invalid",
                "{label}"
            );
        }
    }

    #[tokio::test]
    async fn orchestration_binding_query_large_page_is_text_compatible_and_grok_safe() {
        let production = CompanionFeatures {
            delegation: true,
            coordination_v1: true,
            feedback: false,
            ask: false,
            sessions: false,
            workflow_v2: false,
            completion_v2: false,
        };
        let page = large_orchestration_binding_page();
        let id = json!("binding-budget");
        let preferred = ok(id.clone(), render_orchestration_binding_page(&page));
        let preferred_bytes = serialize_jsonrpc_line(&preferred).unwrap().len();
        assert!(
            preferred_bytes > 8_192,
            "fixture must cross Grok's split boundary, got {preferred_bytes} bytes"
        );

        let (socket_path, server) = orchestration_broker_with_adaptive_page(page.clone());
        let mut context = ctx_with(production);
        context.socket_path = socket_path;
        let call = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/call",
            "params": {
                "name": "get_delegation_orchestration_bindings",
                "arguments": {
                    "namespace": "brainstorm-to-delivery",
                    "limit": 100,
                    "snapshot_id": page["snapshot_id"],
                    "cursor": page["request_cursor"]
                }
            }
        })
        .to_string();
        let LineAction::Spawn(spawned) =
            dispatch_line(&context, Arc::new(InflightCalls::new()), &call).await
        else {
            panic!("valid query must reach the broker")
        };
        let response = spawned.future.await.response.unwrap();
        let line = serialize_jsonrpc_line(&response).unwrap();
        assert!(
            line.len() <= 7_680,
            "binding page JSONL is {} bytes; Grok-safe limit is 7680 bytes",
            line.len()
        );
        server.await.unwrap();

        let result = response.result.unwrap();
        let structured = &result["structuredContent"];
        let returned_runs = structured["runs"].as_array().unwrap();
        let original_runs = page["runs"].as_array().unwrap();
        assert!((1..original_runs.len()).contains(&returned_runs.len()));
        assert_eq!(returned_runs, &original_runs[..returned_runs.len()]);
        assert_eq!(structured["total_rows"], 25);
        assert_eq!(structured["page_start"], 5);
        assert_eq!(structured["complete"], false);
        assert!(structured["next_cursor"].as_str().is_some());
        let text = result["content"][0]["text"].as_str().unwrap();
        assert_eq!(serde_json::from_str::<Value>(text).unwrap(), *structured);
        assert_eq!(result["isError"], false);

        let mut one_more = structured.clone();
        one_more["runs"]
            .as_array_mut()
            .unwrap()
            .push(original_runs[returned_runs.len()].clone());
        let one_more_response = ok(
            json!("binding-budget"),
            render_orchestration_binding_page(&one_more),
        );
        assert!(serialize_jsonrpc_line(&one_more_response).unwrap().len() > 7_680);
    }

    #[tokio::test]
    async fn orchestration_binding_query_single_oversized_row_returns_bounded_typed_error() {
        let mut page = large_orchestration_binding_page();
        let pathological = "quote\"slash\\line\n\u{754c}".repeat(1_000);
        let mut row = page["runs"][0].clone();
        row["work_unit_key"] = json!(pathological);
        page["runs"] = json!([row]);
        page["total_rows"] = json!(1);
        page["page_start"] = json!(0);
        page["request_cursor"] = Value::Null;
        page["complete"] = json!(true);

        let (socket_path, server) = orchestration_broker_with_outcome(page);
        let mut context = ctx_with(CompanionFeatures {
            delegation: true,
            coordination_v1: true,
            feedback: false,
            ask: false,
            sessions: false,
            workflow_v2: false,
            completion_v2: false,
        });
        context.socket_path = socket_path;
        let id =
            ascii_string_id_with_serialized_len(GET_ORCHESTRATION_BINDINGS_MAX_REQUEST_ID_BYTES);
        let call = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/call",
            "params": {
                "name": "get_delegation_orchestration_bindings",
                "arguments": { "namespace": "brainstorm-to-delivery" }
            }
        })
        .to_string();
        let LineAction::Spawn(spawned) =
            dispatch_line(&context, Arc::new(InflightCalls::new()), &call).await
        else {
            panic!("valid query must reach the broker")
        };
        let response = spawned.future.await.response.unwrap();
        server.await.unwrap();
        let line = serialize_jsonrpc_line(&response).unwrap();
        assert!(line.len() <= GET_ORCHESTRATION_BINDINGS_MAX_RESULT_BYTES);
        let result = response.result.unwrap();
        assert_eq!(result["isError"], true);
        assert_eq!(
            result["structuredContent"]["error"]["code"],
            "payload_too_large"
        );
    }

    #[tokio::test]
    async fn orchestration_binding_query_oversized_broker_error_keeps_stable_code_and_budget() {
        let source_message = "stale-snapshot-quote\"slash\\\u{754c}".repeat(1_000);
        let outcome = json!({
            "error": {
                "code": "orchestration_binding_snapshot_stale",
                "message": source_message
            }
        });
        let (socket_path, server) = orchestration_broker_with_outcome(outcome);
        let mut context = ctx_with(CompanionFeatures {
            delegation: true,
            coordination_v1: true,
            feedback: false,
            ask: false,
            sessions: false,
            workflow_v2: false,
            completion_v2: false,
        });
        context.socket_path = socket_path;
        let id =
            ascii_string_id_with_serialized_len(GET_ORCHESTRATION_BINDINGS_MAX_REQUEST_ID_BYTES);
        let call = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/call",
            "params": {
                "name": "get_delegation_orchestration_bindings",
                "arguments": { "namespace": "brainstorm-to-delivery" }
            }
        })
        .to_string();
        let LineAction::Spawn(spawned) =
            dispatch_line(&context, Arc::new(InflightCalls::new()), &call).await
        else {
            panic!("valid query must reach the broker")
        };
        let response = spawned.future.await.response.unwrap();
        server.await.unwrap();
        let line = serialize_jsonrpc_line(&response).unwrap();
        assert!(line.len() <= GET_ORCHESTRATION_BINDINGS_MAX_RESULT_BYTES);
        assert!(!String::from_utf8(line).unwrap().contains(&source_message));
        let result = response.result.unwrap();
        assert_eq!(result["isError"], true);
        assert_eq!(
            result["structuredContent"]["error"]["code"],
            "orchestration_binding_snapshot_stale"
        );
        assert_eq!(
            result["content"][0]["text"],
            "orchestration binding snapshot is stale"
        );
    }

    #[tokio::test]
    async fn orchestration_binding_query_rejects_oversized_request_id_before_broker_work() {
        let id = ascii_string_id_with_serialized_len(
            GET_ORCHESTRATION_BINDINGS_MAX_REQUEST_ID_BYTES + 1,
        );
        let call = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/call",
            "params": {
                "name": "get_delegation_orchestration_bindings",
                "arguments": { "namespace": "brainstorm-to-delivery" }
            }
        })
        .to_string();
        let response =
            unwrap_respond(dispatch_with_features(HISTORICAL_WORKFLOW_ROOT_FIXTURE, &call).await);
        assert_eq!(response.id, Value::Null);
        assert_eq!(response.error.unwrap().code, -32600);
    }

    #[tokio::test]
    async fn orchestration_binding_query_invalid_public_calls_are_structured() {
        let production = CompanionFeatures {
            delegation: true,
            coordination_v1: true,
            feedback: false,
            ask: false,
            sessions: false,
            workflow_v2: false,
            completion_v2: false,
        };
        let snapshot_id = "1a641e16-36f4-4ec5-aa4f-18d18e6ab107";

        for arguments in [
            Value::Null,
            json!({}),
            json!({ "namespace": 7 }),
            json!({
                "namespace": "brainstorm-to-delivery",
                "parent_conversation_id": 42
            }),
            json!({ "namespace": "Upper" }),
            json!({ "namespace": "brainstorm-to-delivery", "limit": 0 }),
            json!({ "namespace": "brainstorm-to-delivery", "limit": "100" }),
            json!({ "namespace": "brainstorm-to-delivery", "snapshot_id": snapshot_id }),
            json!({ "namespace": "brainstorm-to-delivery", "cursor": "abc" }),
            json!({
                "namespace": "brainstorm-to-delivery",
                "snapshot_id": "not-a-uuid",
                "cursor": "abc"
            }),
            json!({
                "namespace": "brainstorm-to-delivery",
                "snapshot_id": snapshot_id,
                "cursor": "not+base64url"
            }),
            json!({
                "namespace": "brainstorm-to-delivery",
                "snapshot_id": snapshot_id,
                "cursor": 7
            }),
            json!({
                "namespace": "brainstorm-to-delivery",
                "snapshot_id": null,
                "cursor": null
            }),
        ] {
            let invalid = json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "tools/call",
                "params": {
                    "name": "get_delegation_orchestration_bindings",
                    "arguments": arguments
                }
            })
            .to_string();
            let response = unwrap_respond(dispatch_with_features(production, &invalid).await);
            assert!(response.error.is_none());
            let result = response.result.unwrap();
            assert_eq!(result["isError"], true);
            assert_eq!(
                result["structuredContent"]["error"]["code"],
                "orchestration_binding_query_invalid"
            );
            assert!(result["structuredContent"].get("runs").is_none());
        }
    }

    #[tokio::test]
    async fn grok_tools_list_excludes_companion_ask_and_stays_within_fixed_stdio_budget() {
        let response = unwrap_respond(
            dispatch_with_features(
                GROK_FEATURES,
                r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#,
            )
            .await,
        );
        let names: Vec<&str> = response.result.as_ref().unwrap()["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["name"].as_str().unwrap())
            .collect();
        let mut line = serde_json::to_vec(&response).unwrap();
        line.push(b'\n');

        // Compatibility contract: Grok splits a JSONL line at 8,192 bytes and
        // does not reassemble it. Keep 512 bytes of headroom; do not raise this
        // literal to make a growing catalog pass.
        println!("Grok tools/list JSONL bytes: {}", line.len());
        assert!(
            line.len() <= 7_680,
            "Grok tools/list line is {} bytes; fixed host-safe limit is 7680 bytes",
            line.len(),
        );
        // Root + coordination_v1 + workflow_v2: binding query, reply,
        // authorization, and five historical workflow tools.
        assert_eq!(
            names,
            vec![
                "delegate_to_agent",
                "register_simple_workflow",
                "get_delegation_orchestration_bindings",
                "continue_delegation",
                "get_delegation_status",
                "cancel_delegation",
                "check_user_feedback",
                "get_session_info",
                "reply_to_delegation",
                "request_recovery_authorization",
                "get_workflow_capabilities",
                "get_workflow_state",
                "recover_workflow",
                "publish_workflow_manifest",
                "settle_workflow_gate",
            ]
        );
    }

    #[tokio::test]
    async fn delegation_catalog_compaction_grok_base_is_at_most_5500_bytes() {
        let response = unwrap_respond(
            dispatch_with_features(
                GROK_FEATURES,
                r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#,
            )
            .await,
        );
        let mut line = serde_json::to_vec(&response).unwrap();
        line.push(b'\n');

        println!("Grok compact tools/list JSONL bytes: {}", line.len());
        assert!(
            line.len() <= 7_680,
            "Grok tools/list line is {} bytes; fixed host-safe limit is 7680 bytes",
            line.len()
        );
        assert!(
            line.len() <= 5_500,
            "compacted Grok catalog is {} bytes",
            line.len()
        );
    }

    #[tokio::test]
    async fn tools_list_includes_feedback_when_enabled() {
        let names = list_tool_names(
            dispatch_with_features(BOTH, r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#).await,
        );
        assert!(names.contains(&"check_user_feedback".to_string()));
        // delegation tools (5, including Simple registration) + feedback (1)
        assert_eq!(names.len(), 6);
        assert!(names.contains(&"continue_delegation".to_string()));
    }

    #[tokio::test]
    async fn tools_list_feedback_only_hides_delegation_tools() {
        let names = list_tool_names(
            dispatch_with_features(
                FEEDBACK_ONLY,
                r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#,
            )
            .await,
        );
        assert_eq!(names, vec!["check_user_feedback".to_string()]);
    }

    #[tokio::test]
    async fn check_user_feedback_spawns_when_enabled() {
        let line = json!({
            "jsonrpc": "2.0", "id": 30, "method": "tools/call",
            "params": { "name": "check_user_feedback", "arguments": {} }
        })
        .to_string();
        assert!(matches!(
            dispatch_with_features(FEEDBACK_ONLY, &line).await,
            LineAction::Spawn(_)
        ));
    }

    #[tokio::test]
    async fn check_user_feedback_rejected_as_unknown_when_feature_off() {
        // Delegation-only ctx: the feedback tool is indistinguishable from a
        // nonexistent one (-32602 unknown tool), not a "disabled" leak.
        let line = json!({
            "jsonrpc": "2.0", "id": 31, "method": "tools/call",
            "params": { "name": "check_user_feedback", "arguments": {} }
        })
        .to_string();
        let resp = unwrap_respond(dispatch_for_test(&line).await);
        let e = resp.error.unwrap();
        assert_eq!(e.code, -32602);
        assert!(e.message.contains("unknown tool"));
    }

    /// tools/list and tools/call independently gate disabled delegation.
    #[tokio::test]
    async fn disabled_feature_absent_from_list_and_rejected_on_direct_call() {
        let names = list_tool_names(
            dispatch_with_features(
                FEEDBACK_ONLY,
                r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#,
            )
            .await,
        );
        assert!(
            !names.iter().any(|n| n.contains("delegat")),
            "disabled delegation tools must not appear in tools/list: {names:?}"
        );
        for tool in [
            "delegate_to_agent",
            "get_delegation_status",
            "cancel_delegation",
        ] {
            let line = json!({
                "jsonrpc": "2.0", "id": 99, "method": "tools/call",
                "params": { "name": tool, "arguments": {} }
            })
            .to_string();
            let resp = unwrap_respond(dispatch_with_features(FEEDBACK_ONLY, &line).await);
            assert_eq!(
                resp.error.as_ref().map(|e| e.code),
                Some(-32602),
                "direct call to disabled {tool} must be rejected"
            );
        }
    }

    // -- ask_user_question feature gating + validation + rendering ----------

    #[tokio::test]
    async fn tools_list_includes_ask_only_when_enabled() {
        let off = list_tool_names(
            dispatch_for_test(r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#).await,
        );
        assert!(!off.contains(&"ask_user_question".to_string()));
        let on = list_tool_names(
            dispatch_with_features(
                ASK_ONLY,
                r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#,
            )
            .await,
        );
        assert_eq!(on, vec!["ask_user_question".to_string()]);
    }

    fn ask_args() -> Value {
        json!({
            "questions": [{
                "question": "Which approach?",
                "header": "Approach",
                "multiSelect": false,
                "options": [
                    { "label": "Incremental", "description": "smaller diffs" },
                    { "label": "Rewrite", "description": "clean slate" }
                ]
            }]
        })
    }

    #[tokio::test]
    async fn ask_user_question_spawns_when_valid_and_enabled() {
        let line = json!({
            "jsonrpc": "2.0", "id": 40, "method": "tools/call",
            "params": { "name": "ask_user_question", "arguments": ask_args() }
        })
        .to_string();
        assert!(matches!(
            dispatch_with_features(ASK_ONLY, &line).await,
            LineAction::Spawn(_)
        ));
    }

    #[tokio::test]
    async fn ask_user_question_invalid_args_rejected_synchronously() {
        // Empty questions array → -32602, fixable by the LLM without a round-trip.
        let line = json!({
            "jsonrpc": "2.0", "id": 41, "method": "tools/call",
            "params": { "name": "ask_user_question", "arguments": { "questions": [] } }
        })
        .to_string();
        let resp = unwrap_respond(dispatch_with_features(ASK_ONLY, &line).await);
        assert_eq!(resp.error.unwrap().code, -32602);
    }

    #[tokio::test]
    async fn ask_user_question_rejected_as_unknown_when_feature_off() {
        let line = json!({
            "jsonrpc": "2.0", "id": 42, "method": "tools/call",
            "params": { "name": "ask_user_question", "arguments": ask_args() }
        })
        .to_string();
        let resp = unwrap_respond(dispatch_for_test(&line).await);
        let e = resp.error.unwrap();
        assert_eq!(e.code, -32602);
        assert!(e.message.contains("unknown tool"));
    }

    #[test]
    fn render_ask_result_lists_selections() {
        let outcome = json!({
            "declined": false,
            "answers": [
                { "question": "Which approach?", "header": "Approach", "multiSelect": false,
                  "selected": ["Incremental"] }
            ]
        });
        let rendered = render_ask_result(&outcome);
        assert_eq!(rendered["isError"], false);
        let text = rendered["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("Approach"));
        assert!(text.contains("Incremental"));
        assert_eq!(rendered["structuredContent"]["declined"], false);
    }

    #[test]
    fn render_ask_result_declined_tells_agent_to_proceed() {
        let rendered = render_ask_result(&json!({ "declined": true, "answers": [] }));
        assert_eq!(rendered["isError"], false);
        let text = rendered["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("dismissed"));
    }

    // -- get_session_info feature gating + parsing + rendering -------------

    #[tokio::test]
    async fn tools_list_includes_session_only_when_enabled() {
        // Default ctx is delegation-only: get_session_info must NOT appear.
        let names = list_tool_names(
            dispatch_for_test(r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#).await,
        );
        assert!(!names.contains(&"get_session_info".to_string()));
        // sessions feature on → exactly that one tool surfaces.
        let names = list_tool_names(
            dispatch_with_features(
                SESSIONS_ONLY,
                r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#,
            )
            .await,
        );
        assert_eq!(names, vec!["get_session_info".to_string()]);
    }

    #[tokio::test]
    async fn get_session_info_spawns_when_valid_and_enabled() {
        let line = json!({
            "jsonrpc": "2.0", "id": 30, "method": "tools/call",
            "params": { "name": "get_session_info", "arguments": { "session_id": 214 } }
        })
        .to_string();
        assert!(matches!(
            dispatch_with_features(SESSIONS_ONLY, &line).await,
            LineAction::Spawn(_)
        ));
    }

    #[tokio::test]
    async fn get_session_info_accepts_numeric_string_id() {
        // Some hosts stringify integer args — still resolves to a Spawn.
        let line = json!({
            "jsonrpc": "2.0", "id": 31, "method": "tools/call",
            "params": { "name": "get_session_info", "arguments": { "session_id": "214" } }
        })
        .to_string();
        assert!(matches!(
            dispatch_with_features(SESSIONS_ONLY, &line).await,
            LineAction::Spawn(_)
        ));
    }

    #[tokio::test]
    async fn get_session_info_missing_or_bad_id_rejected_synchronously() {
        for args in [
            json!({}),
            json!({ "session_id": "abc" }),
            json!({ "session_id": true }),
        ] {
            let line = json!({
                "jsonrpc": "2.0", "id": 32, "method": "tools/call",
                "params": { "name": "get_session_info", "arguments": args }
            })
            .to_string();
            let resp = unwrap_respond(dispatch_with_features(SESSIONS_ONLY, &line).await);
            let e = resp.error.expect("bad session_id must be rejected");
            assert_eq!(e.code, -32602);
            assert!(e.message.contains("session_id"));
        }
    }

    #[tokio::test]
    async fn get_session_info_rejected_as_unknown_when_feature_off() {
        // Default ctx is delegation-only — calling the tool by name is rejected
        // uniformly as an unknown tool (no leak that the feature exists but is off).
        let line = json!({
            "jsonrpc": "2.0", "id": 33, "method": "tools/call",
            "params": { "name": "get_session_info", "arguments": { "session_id": 1 } }
        })
        .to_string();
        let resp = unwrap_respond(dispatch_for_test(&line).await);
        let e = resp.error.unwrap();
        assert_eq!(e.code, -32602);
        assert!(e.message.contains("unknown tool"));
    }

    #[test]
    fn parse_session_id_tolerates_number_string_and_whole_float() {
        assert_eq!(parse_session_id(&json!({ "session_id": 7 })), Some(7));
        assert_eq!(parse_session_id(&json!({ "session_id": " 7 " })), Some(7));
        assert_eq!(parse_session_id(&json!({ "session_id": 7.0 })), Some(7));
        assert_eq!(parse_session_id(&json!({ "session_id": "abc" })), None);
        assert_eq!(parse_session_id(&json!({ "session_id": 7.5 })), None);
        assert_eq!(parse_session_id(&json!({})), None);
    }

    #[test]
    fn parse_max_messages_is_robust() {
        // Omitted → default.
        assert_eq!(parse_max_messages(&json!({})), 20);
        // Explicit 0 (number AND string) is preserved → metadata-only.
        assert_eq!(parse_max_messages(&json!({ "max_messages": 0 })), 0);
        assert_eq!(parse_max_messages(&json!({ "max_messages": "0" })), 0);
        // Plain value within range.
        assert_eq!(parse_max_messages(&json!({ "max_messages": 5 })), 5);
        assert_eq!(parse_max_messages(&json!({ "max_messages": "5" })), 5);
        // Whole float ok; over the cap clamps to MAX_SESSION_MESSAGES.
        assert_eq!(parse_max_messages(&json!({ "max_messages": 50.0 })), 50);
        assert_eq!(parse_max_messages(&json!({ "max_messages": 999 })), 200);
        // A huge value must SATURATE to the cap, not wrap to a small number.
        assert_eq!(
            parse_max_messages(&json!({ "max_messages": 4_294_967_296_u64 })),
            200
        );
        assert_eq!(parse_max_messages(&json!({ "max_messages": 1e30 })), 200);
        // Invalid / negative / fractional → default (optional knob, not an error).
        assert_eq!(parse_max_messages(&json!({ "max_messages": "abc" })), 20);
        assert_eq!(parse_max_messages(&json!({ "max_messages": -5 })), 20);
        assert_eq!(parse_max_messages(&json!({ "max_messages": 5.5 })), 20);
        assert_eq!(parse_max_messages(&json!({ "max_messages": true })), 20);
    }

    #[test]
    fn render_session_result_not_found_is_soft_with_note_text() {
        let outcome = json!({
            "found": false, "session_id": 9,
            "note": "No session matches id 9. It may have been deleted, or never imported into codeg."
        });
        let rendered = render_session_result(&outcome);
        assert_eq!(rendered["isError"], false);
        let text = rendered["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("No session matches id 9"));
        assert_eq!(rendered["structuredContent"]["found"], false);
    }

    #[test]
    fn render_session_result_found_renders_metadata_and_messages() {
        let outcome = json!({
            "found": true,
            "session_id": 214,
            "agent_type": "claude_code",
            "title": "Fix auth flow",
            "status": "completed",
            "git_branch": "main",
            "model": "claude-opus-4-8",
            "workspace_path": "/home/me/proj",
            "message_count": 12,
            "is_delegation_child": false,
            "stats": { "total_tokens": 4242 },
            "messages": {
                "total": 12, "included": 2, "truncated": true,
                "items": [
                    { "role": "user", "text": "fix the login", "tools": [] },
                    { "role": "assistant", "text": "done", "tools": ["Read", "Edit"] }
                ]
            }
        });
        let rendered = render_session_result(&outcome);
        assert_eq!(rendered["isError"], false);
        let text = rendered["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("Session #214 (claude_code)"));
        assert!(text.contains("Fix auth flow"));
        assert!(text.contains("status: completed"));
        assert!(text.contains("Workspace: /home/me/proj"));
        assert!(text.contains("Total tokens: 4242"));
        assert!(text.contains("Recent messages (2/12, older turns omitted)"));
        assert!(text.contains("- [assistant] done (tools: Read, Edit)"));
        // Full structured envelope preserved for hosts that keep it.
        assert_eq!(rendered["structuredContent"]["session_id"], 214);
    }

    fn large_session_outcome(message_count: usize, text_chars: usize) -> Value {
        let items: Vec<Value> = (0..message_count)
            .map(|i| {
                let body = format!(
                    "turn-{i}-{}",
                    "中文内容with\"quotes\"and\\slash\n".repeat((text_chars / 20).max(1))
                );
                // Keep each item near `text_chars` chars.
                let text: String = body.chars().take(text_chars).collect();
                json!({
                    "role": if i % 2 == 0 { "user" } else { "assistant" },
                    "text": text,
                    "tools": ["Read", "Edit", "Bash", "grep"]
                })
            })
            .collect();
        json!({
            "found": true,
            "session_id": 2868,
            "agent_type": "codex",
            "title": "Grok MCP 8KB budget regression fixture",
            "status": "pending_review",
            "git_branch": "main",
            "model": "test-model",
            "workspace_path": "D:\\MyCodeBuddy",
            "message_count": message_count * 2,
            "is_delegation_child": false,
            "stats": { "total_tokens": 16_734_889 },
            "messages": {
                "total": message_count * 2,
                "included": message_count,
                "truncated": true,
                "items": items
            }
        })
    }

    #[test]
    fn get_session_info_50_messages_jsonrpc_line_under_7680_bytes() {
        let outcome = large_session_outcome(50, 1_200);
        let preferred = ok(json!(1), render_session_result(&outcome));
        let preferred_bytes = serialize_jsonrpc_line(&preferred).unwrap().len();
        assert!(
            preferred_bytes > 8_192,
            "fixture must exceed the Grok split boundary; got {preferred_bytes}"
        );

        for id in [
            json!(1),
            json!("quote\"slash\\界"),
            ascii_string_id_with_serialized_len(GET_SESSION_INFO_MAX_REQUEST_ID_BYTES),
        ] {
            let response = render_session_outcome_with_budget(
                id.clone(),
                outcome.clone(),
                GET_SESSION_INFO_MAX_RESULT_BYTES,
            )
            .unwrap();
            let line = serialize_jsonrpc_line(&response).unwrap();
            assert!(
                line.len() <= GET_SESSION_INFO_MAX_RESULT_BYTES,
                "id={id:?} line is {} bytes",
                line.len()
            );
            let result = response.result.as_ref().unwrap();
            assert_eq!(result["isError"], false);
            assert_eq!(result["structuredContent"]["session_id"], 2868);
            // Oversized preferred input must report truncation once omitted.
            if result["structuredContent"].get("messages").is_some() {
                assert_eq!(result["structuredContent"]["messages"]["truncated"], true);
            }
        }
    }

    #[test]
    fn get_session_info_200_messages_keeps_newest_fitting_context() {
        let outcome = large_session_outcome(200, 800);
        let response = render_session_outcome_with_budget(
            json!(42),
            outcome,
            GET_SESSION_INFO_MAX_RESULT_BYTES,
        )
        .unwrap();
        let line = serialize_jsonrpc_line(&response).unwrap();
        assert!(line.len() <= GET_SESSION_INFO_MAX_RESULT_BYTES);

        let structured = &response.result.as_ref().unwrap()["structuredContent"];
        assert_eq!(structured["found"], true);
        assert_eq!(structured["session_id"], 2868);
        if let Some(items) = structured
            .pointer("/messages/items")
            .and_then(Value::as_array)
        {
            // Newest turns are at the end (chronological order).
            let last = items.last().unwrap();
            let text = last["text"].as_str().unwrap_or("");
            assert!(
                text.contains("turn-199") || text.is_empty() || text.ends_with('…'),
                "should prefer newest turn context, got {text:?}"
            );
            assert_eq!(structured["messages"]["truncated"], true);
        } else {
            // Metadata-only fallback is acceptable when even one message overflows.
            assert!(structured["note"]
                .as_str()
                .unwrap_or("")
                .contains("7680-byte"));
        }
    }

    #[test]
    fn get_session_info_single_long_chinese_message_truncates_on_utf8_boundary() {
        let chinese = "中".repeat(6_000);
        let outcome = json!({
            "found": true,
            "session_id": 7,
            "agent_type": "grok",
            "title": "utf8",
            "message_count": 1,
            "is_delegation_child": false,
            "messages": {
                "total": 1,
                "included": 1,
                "truncated": false,
                "items": [{
                    "role": "assistant",
                    "text": chinese,
                    "tools": []
                }]
            }
        });
        let response = render_session_outcome_with_budget(
            json!("id"),
            outcome,
            GET_SESSION_INFO_MAX_RESULT_BYTES,
        )
        .unwrap();
        let line = serialize_jsonrpc_line(&response).unwrap();
        assert!(line.len() <= GET_SESSION_INFO_MAX_RESULT_BYTES);
        // Entire JSONL must be valid UTF-8 (serialize_jsonrpc_line already is).
        assert!(std::str::from_utf8(&line).is_ok());
        let structured = &response.result.as_ref().unwrap()["structuredContent"];
        if let Some(text) = structured
            .pointer("/messages/items/0/text")
            .and_then(Value::as_str)
        {
            assert!(text.chars().count() < 6_000);
            assert!(text.ends_with('…') || text.is_empty());
            // Truncation must not split a multi-byte codepoint — String is always valid UTF-8.
            assert!(text.is_char_boundary(text.len()));
        }
    }

    #[test]
    fn get_session_info_json_escaping_measured_not_estimated() {
        // Quotes/backslashes expand under JSON escaping; budget must measure the
        // serialized form, not the raw character count.
        let messy = "\"\\\n\t".repeat(2_000);
        let outcome = json!({
            "found": true,
            "session_id": 3,
            "agent_type": "codex",
            "message_count": 1,
            "is_delegation_child": false,
            "messages": {
                "total": 1, "included": 1, "truncated": false,
                "items": [{ "role": "user", "text": messy, "tools": [] }]
            }
        });
        let response = render_session_outcome_with_budget(
            json!(1),
            outcome,
            GET_SESSION_INFO_MAX_RESULT_BYTES,
        )
        .unwrap();
        let line = serialize_jsonrpc_line(&response).unwrap();
        assert!(line.len() <= GET_SESSION_INFO_MAX_RESULT_BYTES);
        assert_eq!(response.result.as_ref().unwrap()["isError"], false);
    }

    #[test]
    fn get_session_info_small_response_preserves_shape() {
        let outcome = json!({
            "found": true,
            "session_id": 214,
            "agent_type": "claude_code",
            "title": "Fix auth flow",
            "status": "completed",
            "message_count": 2,
            "is_delegation_child": false,
            "messages": {
                "total": 2, "included": 2, "truncated": false,
                "items": [
                    { "role": "user", "text": "fix login", "tools": [] },
                    { "role": "assistant", "text": "done", "tools": ["Edit"] }
                ]
            }
        });
        let response = render_session_outcome_with_budget(
            json!(9),
            outcome.clone(),
            GET_SESSION_INFO_MAX_RESULT_BYTES,
        )
        .unwrap();
        let preferred = ok(json!(9), render_session_result(&outcome));
        assert_eq!(
            serialize_jsonrpc_line(&response).unwrap(),
            serialize_jsonrpc_line(&preferred).unwrap()
        );
        assert_eq!(
            response.result.as_ref().unwrap()["structuredContent"]["messages"]["included"],
            2
        );
        assert_eq!(
            response.result.as_ref().unwrap()["structuredContent"]["messages"]["truncated"],
            false
        );
    }

    #[test]
    fn get_session_info_pathological_metadata_uses_bounded_fallback() {
        let outcome = json!({
            "found": true,
            "session_id": 99,
            "agent_type": "codex",
            "title": "T".repeat(20_000),
            "status": "S".repeat(20_000),
            "git_branch": "B".repeat(20_000),
            "model": "M".repeat(20_000),
            "workspace_path": format!("D:\\\\{}", "P".repeat(20_000)),
            "message_count": 1,
            "is_delegation_child": false,
            "stats": { "total_tokens": 1 },
            "messages": {
                "total": 1, "included": 1, "truncated": false,
                "items": [{
                    "role": "user",
                    "text": "x".repeat(20_000),
                    "tools": ["tool".repeat(200)]
                }]
            }
        });
        let response = render_session_outcome_with_budget(
            ascii_string_id_with_serialized_len(GET_SESSION_INFO_MAX_REQUEST_ID_BYTES),
            outcome,
            GET_SESSION_INFO_MAX_RESULT_BYTES,
        )
        .unwrap();
        let line = serialize_jsonrpc_line(&response).unwrap();
        assert!(
            line.len() <= GET_SESSION_INFO_MAX_RESULT_BYTES,
            "{} bytes",
            line.len()
        );
        let structured = &response.result.as_ref().unwrap()["structuredContent"];
        assert!(structured.get("messages").is_none());
        assert!(structured["note"]
            .as_str()
            .unwrap_or("")
            .contains("7680-byte"));
        assert_eq!(structured["session_id"], 99);
    }

    #[tokio::test]
    async fn get_session_info_request_id_limit_is_pre_inflight_and_bounded() {
        let accepted = ascii_string_id_with_serialized_len(GET_SESSION_INFO_MAX_REQUEST_ID_BYTES);
        let accepted_line = json!({
            "jsonrpc": "2.0", "id": accepted, "method": "tools/call",
            "params": { "name": "get_session_info", "arguments": { "session_id": 1 } }
        })
        .to_string();
        assert!(matches!(
            dispatch_with_features(SESSIONS_ONLY, &accepted_line).await,
            LineAction::Spawn(_)
        ));

        for rejected in [
            ascii_string_id_with_serialized_len(GET_SESSION_INFO_MAX_REQUEST_ID_BYTES + 1),
            Value::String("\\".repeat(128)),
            Value::String("界".repeat(85)),
        ] {
            let inflight = Arc::new(InflightCalls::new());
            let line = json!({
                "jsonrpc": "2.0", "id": rejected, "method": "tools/call",
                "params": { "name": "get_session_info", "arguments": { "session_id": 1 } }
            })
            .to_string();
            let response = unwrap_respond(
                dispatch_line(&ctx_with(SESSIONS_ONLY), inflight.clone(), &line).await,
            );
            assert_eq!(response.id, Value::Null);
            assert_eq!(response.error.as_ref().unwrap().code, -32600);
            assert!(response
                .error
                .as_ref()
                .unwrap()
                .message
                .contains("get_session_info"));
            assert!(inflight.drain_all().await.is_empty());
            assert!(
                serialize_jsonrpc_line(&response).unwrap().len()
                    <= GET_SESSION_INFO_MAX_RESULT_BYTES
            );
        }
    }

    #[test]
    fn render_feedback_empty_is_not_error_and_says_no_feedback() {
        let rendered = render_feedback_result(&json!({ "count": 0, "feedback": [] }));
        assert_eq!(rendered["isError"], false);
        let text = rendered["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("No new feedback"));
        assert_eq!(rendered["structuredContent"]["count"], 0);
    }

    #[test]
    fn render_feedback_lists_notes_as_high_priority_steering() {
        let outcome = json!({
            "count": 2,
            "feedback": [
                { "text": "use the existing UserService", "created_at": "2026-06-07T00:00:00Z" },
                { "text": "skip the migration", "created_at": "2026-06-07T00:00:01Z" },
            ]
        });
        let rendered = render_feedback_result(&outcome);
        assert_eq!(rendered["isError"], false);
        let text = rendered["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("high-priority steering"));
        assert!(text.contains("1. use the existing UserService"));
        assert!(text.contains("2. skip the migration"));
        // Structured payload carries the notes for hosts that keep it.
        assert_eq!(rendered["structuredContent"]["count"], 2);
    }

    #[test]
    fn render_feedback_strips_internal_commit_ids() {
        // The listener embeds `_commit_ids` for the companion to echo back; they
        // must NEVER leak into the agent-facing result (content or structured).
        let outcome = json!({
            "count": 1,
            "feedback": [{ "text": "note", "created_at": "2026-06-07T00:00:00Z" }],
            "_commit_ids": ["secret-id-1"],
        });
        let rendered = render_feedback_result(&outcome);
        assert!(rendered["structuredContent"].get("_commit_ids").is_none());
        assert_eq!(rendered["structuredContent"]["count"], 1);
        assert_eq!(rendered["structuredContent"]["feedback"][0]["text"], "note");
        let text = rendered["content"][0]["text"].as_str().unwrap();
        assert!(!text.contains("secret-id-1"));
    }

    // -- commit-on-delivery protocol (the at-least-once delivery guarantee) ---

    #[cfg(unix)]
    fn feedback_resp_with_ids(ids: &[&str]) -> BrokerResponse {
        BrokerResponse {
            outcome: json!({
                "count": 1,
                "feedback": [{ "text": "steer", "created_at": "x" }],
                "_commit_ids": ids,
            }),
        }
    }

    /// When the round-trip wins (no cancel), the companion COMMITS delivery by
    /// sending a `CommitFeedback` with the listener's `_commit_ids`.
    #[cfg(unix)]
    #[tokio::test]
    async fn feedback_spawn_commits_after_delivery() {
        use crate::acp::delegation::transport::{read_frame, write_frame, BrokerMessage};
        use tokio::net::UnixListener;

        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("fb.sock").to_string_lossy().to_string();
        let listener = UnixListener::bind(&sock).unwrap();
        let committed = Arc::new(Mutex::new(Vec::<Vec<String>>::new()));
        let committed2 = committed.clone();
        let server = tokio::spawn(async move {
            // 1) Feedback round-trip → respond with notes + _commit_ids.
            let (mut c1, _) = listener.accept().await.unwrap();
            let _: BrokerResponse = match read_frame::<_, BrokerMessage>(&mut c1).await.unwrap() {
                BrokerMessage::Feedback(_) => {
                    write_frame(&mut c1, &feedback_resp_with_ids(&["f1"]))
                        .await
                        .unwrap();
                    BrokerResponse {
                        outcome: Value::Null,
                    }
                }
                other => panic!("expected Feedback, got {other:?}"),
            };
            // 2) CommitFeedback → record the ids.
            let (mut c2, _) = listener.accept().await.unwrap();
            if let BrokerMessage::CommitFeedback(req) = read_frame(&mut c2).await.unwrap() {
                committed2.lock().await.push(req.ids);
            }
            write_frame(
                &mut c2,
                &BrokerResponse {
                    outcome: Value::Null,
                },
            )
            .await
            .unwrap();
        });

        let inflight = Arc::new(InflightCalls::new());
        let action = register_and_spawn_feedback(
            inflight,
            Value::from(1),
            sock,
            "tok".into(),
            BrokerFeedbackRequest {
                token: "tok".into(),
            },
        )
        .await;
        let LineAction::Spawn(call) = action else {
            panic!("expected Spawn")
        };
        let result = call.future.await;
        let resp = result.response.expect("feedback result");
        assert_eq!(resp.result.unwrap()["structuredContent"]["count"], 1);
        // The commit is deferred to `after_relay`, which the binary runs ONLY
        // after a successful stdout write — drive it here to simulate that relay.
        result
            .after_relay
            .expect("feedback must carry a post-relay commit")
            .await;
        server.await.unwrap();
        assert_eq!(*committed.lock().await, vec![vec!["f1".to_string()]]);
    }

    /// When a cancel wins the select, the companion suppresses the response AND
    /// sends NO commit — so the notes stay pending for the next check.
    #[cfg(unix)]
    #[tokio::test]
    async fn feedback_spawn_cancel_sends_no_commit() {
        use crate::acp::delegation::transport::{read_frame, write_frame, BrokerMessage};
        use tokio::net::UnixListener;

        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("fb.sock").to_string_lossy().to_string();
        let listener = UnixListener::bind(&sock).unwrap();
        let saw_commit = Arc::new(Mutex::new(false));
        let saw_commit2 = saw_commit.clone();
        let server = tokio::spawn(async move {
            // Accept the Feedback connection but DELAY responding, so the cancel
            // (fired below) wins the select first.
            if let Ok((mut c1, _)) = listener.accept().await {
                tokio::time::sleep(Duration::from_millis(150)).await;
                let _ = write_frame(&mut c1, &feedback_resp_with_ids(&["f1"])).await;
            }
            // A commit (if any) would arrive as a second connection. Wait briefly;
            // a timeout (no connection) is the expected, correct outcome.
            if let Ok(Ok((mut c2, _))) =
                tokio::time::timeout(Duration::from_millis(200), listener.accept()).await
            {
                if matches!(
                    read_frame::<_, BrokerMessage>(&mut c2).await,
                    Ok(BrokerMessage::CommitFeedback(_))
                ) {
                    *saw_commit2.lock().await = true;
                }
            }
        });

        let ctx = CompanionContext {
            parent_connection_id: "p".into(),
            socket_path: sock,
            token: "tok".into(),
            features: FEEDBACK_ONLY,
            role: CompanionRole::Root,
            can_spawn_child: true,
            connection_incarnation_id: "test-incarnation".into(),
            disabled_agents: Vec::new(),
        };
        let inflight = Arc::new(InflightCalls::new());
        // tools/call → Spawn (registers the inflight entry).
        let call_line = json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": { "name": "check_user_feedback", "arguments": {} }
        })
        .to_string();
        let action = dispatch_line(&ctx, inflight.clone(), &call_line).await;
        let LineAction::Spawn(call) = action else {
            panic!("expected Spawn")
        };
        // Cancel for the same id BEFORE the (delayed) response arrives.
        let cancel_line =
            json!({ "jsonrpc": "2.0", "method": "notifications/cancelled", "params": { "requestId": 1 } })
                .to_string();
        assert!(matches!(
            dispatch_line(&ctx, inflight.clone(), &cancel_line).await,
            LineAction::Silent
        ));
        // Cancel won → response suppressed AND no post-relay commit exists.
        let result = call.future.await;
        assert!(
            result.response.is_none(),
            "cancel must suppress the response"
        );
        assert!(
            result.after_relay.is_none(),
            "a suppressed response carries no commit"
        );
        server.abort();
        // Crucially: no commit was sent for a cancelled (undelivered) check.
        assert!(
            !*saw_commit.lock().await,
            "a cancelled check must not commit"
        );
    }

    // -- Role-aware decision tools (Task 6) ---------------------------------

    #[tokio::test]
    async fn decision_tools_are_capability_and_role_scoped() {
        let legacy = tool_names(dispatch_with_context(legacy_root(), tools_list()).await);
        assert!(!legacy.contains(&"request_parent_decision".into()));
        assert!(!legacy.contains(&"reply_to_delegation".into()));
        assert_no_generic_coordination_side_channel_tools(&legacy);

        let root = tool_names(dispatch_with_context(coordination_root(), tools_list()).await);
        assert!(!root.contains(&"request_parent_decision".into()));
        assert!(root.contains(&"reply_to_delegation".into()));
        assert_no_generic_coordination_side_channel_tools(&root);

        let child = tool_names(dispatch_with_context(coordination_child(), tools_list()).await);
        assert!(child.contains(&"request_parent_decision".into()));
        assert!(child.contains(&"reply_to_delegation".into()));
        assert_no_generic_coordination_side_channel_tools(&child);
    }

    #[tokio::test]
    async fn depth_limited_child_hides_only_new_delegation_tool() {
        let mut child = coordination_child();
        child.can_spawn_child = false;

        let names = tool_names(dispatch_with_context(child.clone(), tools_list()).await);
        assert!(!names.contains(&"delegate_to_agent".into()));
        assert!(names.contains(&"continue_delegation".into()));
        assert!(names.contains(&"get_delegation_status".into()));
        assert!(names.contains(&"cancel_delegation".into()));

        let response = unwrap_respond(
            dispatch_with_context(
                child,
                &call(
                    11,
                    "delegate_to_agent",
                    json!({"agent_type":"codex","task":"nested"}),
                ),
            )
            .await,
        );
        assert_eq!(response.error.unwrap().code, -32602);

        let mut child_with_capacity = coordination_child();
        child_with_capacity.can_spawn_child = true;
        let names = tool_names(dispatch_with_context(child_with_capacity, tools_list()).await);
        assert!(names.contains(&"delegate_to_agent".into()));
    }

    #[tokio::test]
    async fn root_direct_call_to_child_only_tool_is_rejected_without_socket_io() {
        let response = unwrap_respond(
            dispatch_with_context(
                coordination_root(),
                &call(7, "request_parent_decision", json!({"message":"choose"})),
            )
            .await,
        );
        assert_eq!(response.error.unwrap().code, -32602);
    }

    #[tokio::test]
    async fn legacy_direct_call_to_decision_tools_is_rejected() {
        for name in ["request_parent_decision", "reply_to_delegation"] {
            let args = if name == "request_parent_decision" {
                json!({"message":"choose"})
            } else {
                json!({"request_id":"r1","reply":"A"})
            };
            let response =
                unwrap_respond(dispatch_with_context(legacy_root(), &call(8, name, args)).await);
            assert_eq!(response.error.unwrap().code, -32602);
        }
    }

    #[tokio::test]
    async fn coordination_child_spawns_request_parent_decision() {
        let action = dispatch_with_context(
            coordination_child(),
            &call(9, "request_parent_decision", json!({"message":"choose"})),
        )
        .await;
        assert!(matches!(action, LineAction::Spawn(_)));
    }

    #[tokio::test]
    async fn coordination_root_spawns_reply_to_delegation() {
        let action = dispatch_with_context(
            coordination_root(),
            &call(
                10,
                "reply_to_delegation",
                json!({"request_id":"r1","reply":"A"}),
            ),
        )
        .await;
        assert!(matches!(action, LineAction::Spawn(_)));
    }

    #[test]
    fn decision_payload_validation_is_utf8_byte_bounded_and_exact_keyed() {
        assert!(parse_parent_decision_args(&json!({"message":"x".repeat(16 * 1024)})).is_ok());
        assert!(parse_parent_decision_args(&json!({"message":"界".repeat(6000)})).is_err());
        assert!(parse_parent_decision_args(&json!({"message":"  "})).is_err());
        assert!(
            parse_parent_decision_args(&json!({"message":"choose", "task_id":"foreign"})).is_err()
        );
        assert!(parse_reply_args(&json!({"request_id":"r1", "reply":"A", "parent_id":1})).is_err());
        assert!(parse_parent_decision_args(&json!({"message":"x".repeat(16 * 1024 + 1)})).is_err());
        assert!(
            parse_reply_args(&json!({"request_id":"r1","reply":"x".repeat(16 * 1024)})).is_ok()
        );
        assert!(
            parse_reply_args(&json!({"request_id":"r1","reply":"x".repeat(16 * 1024 + 1)}))
                .is_err()
        );
    }

    #[test]
    fn render_parent_decision_surfaces_reply_text_without_error_flag() {
        let rendered = render_parent_decision_result(&json!({
            "status": "replied",
            "request_id": "r1",
            "reply": "Use A",
        }));
        assert_eq!(rendered["isError"], false);
        assert_eq!(rendered["content"][0]["text"], "Use A");
        assert_eq!(rendered["structuredContent"]["status"], "replied");
    }

    #[test]
    fn render_reply_delegation_idempotent_is_not_error() {
        let rendered = render_reply_delegation_result(&json!({
            "status": "idempotent",
            "request_id": "r1",
        }));
        assert_eq!(rendered["isError"], false);
        assert_eq!(rendered["content"][0]["text"], "Reply already delivered");
    }

    #[test]
    fn render_reply_delegation_does_not_echo_reply_in_text() {
        let rendered = render_reply_delegation_result(&json!({
            "status": "replied",
            "request_id": "r1",
        }));
        assert_eq!(rendered["content"][0]["text"], "Reply delivered");
        let text = rendered["content"][0]["text"].as_str().unwrap();
        assert!(!text.contains("secret-reply-body"));
    }

    mod recovery_tool_contract {
        use super::*;

        #[tokio::test]
        async fn tools_list_exposes_exact_recovery_inputs_and_removes_broad_unresumable_copy() {
            let schema: Value = serde_json::from_str(TOOL_SCHEMA_JSON).expect("valid schema");
            let tools = schema.as_array().expect("tool array");
            let authorization = tools
                .iter()
                .find(|tool| tool["name"] == "request_recovery_authorization")
                .expect("request_recovery_authorization schema");
            assert!(authorization["description"]
                .as_str()
                .expect("authorization description")
                .contains("recovery_confirmation_required"));
            assert!(authorization["description"]
                .as_str()
                .expect("authorization description")
                .contains("exact rejected call"));
            for result_field in [
                "subject_kind",
                "allowed_action",
                "cause_code",
                "expires_at",
                "target_state",
                "replacement_reason",
            ] {
                assert!(authorization["description"]
                    .as_str()
                    .expect("authorization description")
                    .contains(result_field));
            }
            let authorization_schema = &authorization["inputSchema"];
            assert_eq!(authorization_schema["additionalProperties"], false);
            assert_eq!(
                authorization_schema["required"],
                json!(["subject_kind", "subject_id", "correlation_id"])
            );
            assert_eq!(
                authorization_schema["properties"]["subject_kind"]["enum"],
                json!(["delegation_task", "workflow"])
            );
            assert_eq!(
                authorization_schema["properties"]["correlation_id"]["pattern"],
                "^[A-Za-z0-9][A-Za-z0-9._:-]{0,127}$"
            );
            assert_eq!(
                authorization_schema["properties"]["proposed_user_reason"]["maxLength"],
                4096
            );
            for rejected in [
                "action",
                "target",
                "warning",
                "work_unit_key",
                "delegation_target",
                "recovery_authorization_id",
            ] {
                assert!(
                    authorization_schema["properties"].get(rejected).is_none(),
                    "caller must not supply {rejected}"
                );
            }

            let recover = tools
                .iter()
                .find(|tool| tool["name"] == "recover_workflow")
                .expect("recover_workflow schema");
            assert_eq!(recover["inputSchema"]["additionalProperties"], false);
            assert_eq!(
                recover["inputSchema"]["required"],
                json!([
                    "workflow_id",
                    "recovery_authorization_id",
                    "expected_manifest_revision",
                    "correlation_id"
                ])
            );
            assert_eq!(
                recover["inputSchema"]["properties"]["correlation_id"]["pattern"],
                "^[A-Za-z0-9][A-Za-z0-9._:-]{0,127}$"
            );

            let replacement_description = tools
                .iter()
                .find(|tool| tool["name"] == "delegate_to_agent")
                .unwrap()["inputSchema"]["properties"]["replacement_reason"]["description"]
                .as_str()
                .unwrap();
            for tool_name in ["delegate_to_agent", "continue_delegation"] {
                let tool = tools
                    .iter()
                    .find(|tool| tool["name"] == tool_name)
                    .expect("delegation replay tool schema");
                assert_eq!(
                    tool["inputSchema"]["properties"]["recovery_authorization_id"]["minLength"],
                    1
                );
            }
            for unsafe_source in [
                "parent_canceled",
                "parent_turn_failed",
                "join_abandoned",
                "user_cancelled",
                "tool_stalled_timeout",
            ] {
                assert!(
                    !replacement_description.contains(unsafe_source),
                    "schema must not map {unsafe_source} to unresumable"
                );
            }

            let advertised =
                unwrap_respond(dispatch_with_context(coordination_root(), tools_list()).await);
            let advertised_tools = advertised.result.as_ref().unwrap()["tools"]
                .as_array()
                .unwrap();
            let advertised_authorization = advertised_tools
                .iter()
                .find(|tool| tool["name"] == "request_recovery_authorization")
                .expect("advertised authorization tool");
            assert!(advertised_authorization["description"]
                .as_str()
                .expect("advertised authorization description")
                .contains("recovery_confirmation_required"));
            let names = advertised_tools
                .iter()
                .map(|tool| tool["name"].as_str().unwrap().to_string())
                .collect::<Vec<_>>();
            assert!(names.contains(&"request_recovery_authorization".to_string()));
            let workflow_names = list_tool_names(
                dispatch_with_features(HISTORICAL_WORKFLOW_ROOT_FIXTURE, tools_list()).await,
            );
            assert!(workflow_names.contains(&"recover_workflow".to_string()));

            for bad in [
                json!({"subject_kind":"delegation_task","subject_id":"task","correlation_id":" bad"}),
                json!({"subject_kind":"delegation_task","subject_id":"task","correlation_id":"x".repeat(129)}),
                json!({"subject_kind":"delegation_task","subject_id":"task","correlation_id":"ok","proposed_user_reason":"not allowed"}),
                json!({"subject_kind":"workflow","subject_id":"wf","correlation_id":"ok","proposed_user_reason":" ","action":"replace"}),
            ] {
                assert!(parse_recovery_authorization_args(&bad, "token").is_err());
            }
            assert!(parse_recovery_authorization_args(
                &json!({"subject_kind":"workflow","subject_id":"wf","correlation_id":"x".repeat(128),"proposed_user_reason":"x".repeat(4096)}),
                "token"
            )
            .is_ok());
            for reason in ["x".repeat(4097), "界".repeat(1366)] {
                assert!(parse_recovery_authorization_args(
                    &json!({"subject_kind":"workflow","subject_id":"wf","correlation_id":"ok","proposed_user_reason":reason}),
                    "token"
                )
                .is_err());
            }
            for rejected in [
                "action",
                "target",
                "warning",
                "work_unit_key",
                "delegation_target",
                "recovery_authorization_id",
            ] {
                let mut arguments = json!({
                    "subject_kind": "delegation_task",
                    "subject_id": "task",
                    "correlation_id": "valid-correlation",
                });
                arguments
                    .as_object_mut()
                    .unwrap()
                    .insert(rejected.to_string(), json!("caller-supplied"));
                let error = parse_recovery_authorization_args(&arguments, "token")
                    .expect_err("forbidden recovery input must fail");
                assert!(
                    error.contains(rejected),
                    "unexpected error for {rejected}: {error}"
                );
            }
        }

        #[test]
        fn companion_preserves_structured_authorization_contract_for_terminal_statuses() {
            for (status, reused) in [("approved", false), ("approved", true), ("declined", false)] {
                let outcome = json!({
                    "status": status,
                    "recovery_authorization_id": "authorization-a",
                    "reused": reused,
                    "subject_kind": "workflow",
                    "subject_id": "workflow-a",
                    "allowed_action": "recover_workflow",
                    "target_state": "estimated",
                    "cause_code": "plan_gate_blocked",
                    "expires_at": if status == "approved" { json!("2026-07-30T12:10:00Z") } else { Value::Null },
                });
                let rendered = render_recovery_authorization_result(&outcome);
                assert_eq!(rendered["structuredContent"], outcome);
                assert_eq!(rendered["isError"], false);
            }
        }

        #[tokio::test]
        async fn workflow_catalog_is_inconsistent_when_recover_workflow_is_missing() {
            let complete = WORKFLOW_V2_TOOLS.to_vec();
            assert_eq!(
                classify_workflow_tool_catalog(complete.iter().copied()),
                WorkflowCapabilityMode::WorkflowManifestV2
            );
            for omitted in WORKFLOW_V2_TOOLS {
                let partial = complete
                    .iter()
                    .copied()
                    .filter(|tool| tool != omitted)
                    .collect::<Vec<_>>();
                assert_eq!(
                    classify_workflow_tool_catalog(partial),
                    WorkflowCapabilityMode::Inconsistent,
                    "omitting {omitted} must fail closed"
                );
            }

            let historical_fixture = HISTORICAL_WORKFLOW_ROOT_FIXTURE;
            let names =
                list_tool_names(dispatch_with_features(historical_fixture, tools_list()).await);
            assert_eq!(
                classify_workflow_tool_catalog(names.iter().map(String::as_str)),
                WorkflowCapabilityMode::WorkflowManifestV2
            );
            let without_recover = names
                .iter()
                .map(String::as_str)
                .filter(|name| *name != "recover_workflow")
                .collect::<Vec<_>>();
            assert_eq!(
                classify_workflow_tool_catalog(without_recover),
                WorkflowCapabilityMode::Inconsistent
            );
            let capabilities =
                local_workflow_capabilities(&historical_fixture, CompanionRole::Root);
            assert_eq!(capabilities["workflow_manifest_v2"], true);
            assert_eq!(capabilities["operations"], json!(WORKFLOW_V2_TOOLS));

            for disabled in [
                COORDINATION,
                CompanionFeatures::parse(Some("")),
                CompanionFeatures::parse(Some(
                    "delegation,coordination_v1,feedback,ask,sessions,workflow_v2",
                )),
            ] {
                let disabled_names =
                    list_tool_names(dispatch_with_features(disabled, tools_list()).await);
                assert!(!disabled_names.contains(&"get_workflow_state".to_string()));
                assert!(!disabled_names.contains(&"recover_workflow".to_string()));
                let capabilities = local_workflow_capabilities(&disabled, CompanionRole::Root);
                assert_eq!(capabilities["workflow_manifest_v2"], false);
                assert_eq!(capabilities["versions"][WORKFLOW_CAPABILITY_VERSION], false);
                assert_eq!(capabilities["operations"], json!([]));
            }
        }
    }
}
