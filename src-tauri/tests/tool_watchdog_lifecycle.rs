//! Focused end-to-end lifecycle fixtures for the tool-execution watchdog.
//!
//! Uses controlled clocks + the public registry/supervisor/settings cores so
//! desktop and server share the same outcomes without wall-clock waits.

use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{DateTime, Utc};
use codeg_lib::acp::manager::ConnectionManager;
use codeg_lib::acp::tool_watchdog::{
    escalate_claimed_lease, CancelCause, CancelHost, CancellationCapability, CancellationScope,
    ConvergenceProbe, EscalationStage, LeaseStamp, McpCancelToken, RegisterTool, RegistryAction,
    RegistryProbe, SemanticProgress, SpecificCancelOutcome, ToolCategory,
    ToolExecutionLeaseRegistry, ToolLeaseKey, ToolProgressKey, ToolWatchdogPhase,
    ToolWatchdogSettings, TurnStamp, WatchdogInstant, ERROR_CODE_TOOL_STALLED_TIMEOUT,
    ERROR_CODE_USER_CANCELLED,
};
use codeg_lib::commands::tool_watchdog::{
    acp_get_tool_watchdog_settings_core, acp_set_tool_watchdog_settings_core,
    acp_tool_watchdog_cancel_core, acp_tool_watchdog_extend_core,
};
use codeg_lib::db::test_helpers::fresh_in_memory_db;
use tokio::time::Instant;

fn clock_base() -> WatchdogInstant {
    WatchdogInstant {
        mono: Instant::now(),
        wall: DateTime::parse_from_rfc3339("2026-07-22T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc),
    }
}

fn sample_turn(gen: u64) -> TurnStamp {
    TurnStamp {
        connection_id: "conn-1".into(),
        connection_incarnation: "inc-1".into(),
        session_id: "sess-1".into(),
        turn_generation: gen,
    }
}

fn tool_key(turn: &TurnStamp, tool_call_id: &str) -> ToolLeaseKey {
    ToolLeaseKey {
        connection_id: turn.connection_id.clone(),
        connection_incarnation: turn.connection_incarnation.clone(),
        turn_generation: turn.turn_generation,
        tool_call_id: tool_call_id.into(),
    }
}

fn progress_key(turn: &TurnStamp, tool_call_id: &str) -> ToolProgressKey {
    ToolProgressKey {
        connection_id: turn.connection_id.clone(),
        connection_incarnation: turn.connection_incarnation.clone(),
        turn_generation: turn.turn_generation,
        tool_call_id: tool_call_id.into(),
    }
}

async fn register_tool(
    reg: &ToolExecutionLeaseRegistry,
    turn: &TurnStamp,
    tool_id: &str,
    category: ToolCategory,
    at: WatchdogInstant,
) -> LeaseStamp {
    reg.register_tool(RegisterTool {
        turn: turn.clone(),
        tool_call_id: tool_id.into(),
        category,
        at,
    })
    .await
    .expect("register")
    .stamp
}

async fn advance_to_grace(
    reg: &ToolExecutionLeaseRegistry,
    t0: WatchdogInstant,
) -> codeg_lib::acp::tool_watchdog::ToolWatchdogProjection {
    let actions = reg.scan(t0.advanced(600)).await;
    let RegistryAction::PublishWarning { stamp, .. } = &actions[0] else {
        panic!("expected warning at +600s: {actions:?}");
    };
    reg.warning_published(&stamp.lease_id, stamp.version, t0.advanced(600))
        .await
        .expect("enter grace")
}

/// silent terminal: 600s warning + 600s grace -> process tree kill claim -> settle
#[tokio::test]
async fn silent_terminal_600_600_claims_terminal_then_settles() {
    let reg = ToolExecutionLeaseRegistry::new(ToolWatchdogSettings::default());
    let turn = sample_turn(1);
    let t0 = clock_base();
    reg.start_turn(turn.clone(), t0).await;
    let stamp = register_tool(&reg, &turn, "term-tool", ToolCategory::Terminal, t0).await;
    let bound = reg
        .bind_capability(
            &stamp,
            CancellationCapability::Terminal {
                session_id: "sess-1".into(),
                terminal_id: "pty-1".into(),
            },
        )
        .await
        .unwrap();

    let grace = advance_to_grace(&reg, t0).await;
    assert_eq!(grace.phase, ToolWatchdogPhase::Grace);
    assert_eq!(grace.lease_id, bound.lease_id);

    let cancel = reg.scan(t0.advanced(1200)).await;
    let RegistryAction::ClaimCancel { claim } = &cancel[0] else {
        panic!("expected auto cancel at +1200s: {cancel:?}");
    };
    assert_eq!(claim.cause, CancelCause::AutoTimeout);
    assert_eq!(
        claim.capability,
        CancellationCapability::Terminal {
            session_id: "sess-1".into(),
            terminal_id: "pty-1".into(),
        }
    );

    let settled = reg
        .settle_cancel(
            &claim.stamp.lease_id,
            claim.stamp.version,
            CancellationScope::Terminal,
            ERROR_CODE_TOOL_STALLED_TIMEOUT,
        )
        .await
        .expect("settle");
    assert_eq!(settled.phase, ToolWatchdogPhase::TimedOut);
    assert_eq!(
        settled.error_code.as_deref(),
        Some(ERROR_CODE_TOOL_STALLED_TIMEOUT)
    );
    assert!(!reg.is_live(&claim.stamp.lease_id).await);
}

/// Truncation-cap offsets still renew without unbounded registry growth.
#[tokio::test]
async fn truncation_cap_offset_renews_without_registry_growth() {
    let reg = ToolExecutionLeaseRegistry::new(ToolWatchdogSettings::default());
    let turn = sample_turn(1);
    let t0 = clock_base();
    reg.start_turn(turn.clone(), t0).await;
    let stamp = register_tool(&reg, &turn, "term-tool", ToolCategory::Terminal, t0).await;
    let key = progress_key(&turn, "term-tool");

    let baseline = reg.live_lease_count().await;
    for i in 1..=64u64 {
        let at = t0.advanced(i * 10);
        let applied = reg
            .record_tool_progress_at(
                key.clone(),
                SemanticProgress::TerminalOffset {
                    terminal_id_hash: Some(1),
                    next_offset: i * 10_000,
                },
                at,
            )
            .await;
        assert!(applied.is_some(), "offset {i} must renew");
    }
    assert_eq!(reg.live_lease_count().await, baseline);
    assert!(reg.is_live(&stamp.lease_id).await);

    let late = reg.scan(t0.advanced(600)).await;
    assert!(
        late.is_empty(),
        "renewed lease must not warn at +600s: {late:?}"
    );
}

/// Cancellable MCP settles at specific cancel without turn cancel.
#[tokio::test]
async fn cancellable_mcp_settles_without_turn_cancel() {
    let reg = Arc::new(ToolExecutionLeaseRegistry::new(
        ToolWatchdogSettings::default(),
    ));
    let turn = sample_turn(1);
    let t0 = clock_base();
    reg.start_turn(turn.clone(), t0).await;
    let stamp = register_tool(&reg, &turn, "mcp-tool", ToolCategory::Mcp, t0).await;
    let bound = reg
        .bind_capability(
            &stamp,
            CancellationCapability::McpRequest {
                cancel_token: McpCancelToken::new(42),
            },
        )
        .await
        .unwrap();
    let grace = advance_to_grace(&reg, t0).await;
    assert_eq!(grace.lease_id, bound.lease_id);
    let (claim, _) = reg
        .claim_cancel(&grace.lease_id, grace.version, CancelCause::AutoTimeout)
        .await
        .expect("claim");

    let host = ScriptedHost::new();
    *host.settle_lease_after_ms.lock().unwrap() =
        Some((claim.stamp.lease_id.clone(), 0, reg.clone()));
    *host.mcp_ok.lock().unwrap() = true;
    let probe = RegistryProbe {
        registry: reg.clone(),
        force_prompting: Some(true),
    };
    let report = escalate_claimed_lease(
        &host,
        &probe,
        reg.as_ref(),
        &claim,
        Duration::from_millis(50),
    )
    .await;
    assert_eq!(report.stage, EscalationStage::Specific);
    assert!(report.specific_converged);
    assert_eq!(host.turn_calls.load(Ordering::SeqCst), 0);
    assert_eq!(host.disconnect_calls.load(Ordering::SeqCst), 0);
    assert_eq!(host.mcp_calls.load(Ordering::SeqCst), 1);
}

/// Uncancellable MCP escalates to turn (and may disconnect if turn does not settle).
#[tokio::test]
async fn uncancellable_mcp_escalates_turn_then_optionally_disconnect() {
    let reg = Arc::new(ToolExecutionLeaseRegistry::new(
        ToolWatchdogSettings::default(),
    ));
    let turn = sample_turn(1);
    let t0 = clock_base();
    reg.start_turn(turn.clone(), t0).await;
    let stamp = register_tool(&reg, &turn, "mcp-tool", ToolCategory::Mcp, t0).await;
    let _bound = reg
        .bind_capability(
            &stamp,
            CancellationCapability::McpRequest {
                cancel_token: McpCancelToken::new(7),
            },
        )
        .await
        .unwrap();
    let grace = advance_to_grace(&reg, t0).await;
    let (claim, _) = reg
        .claim_cancel(&grace.lease_id, grace.version, CancelCause::AutoTimeout)
        .await
        .unwrap();

    let host = ScriptedHost::new();
    *host.mcp_ok.lock().unwrap() = false;
    // Settle only after turn cancel is admitted.
    *host.settle_on_turn.lock().unwrap() = true;
    *host.settle_lease_after_ms.lock().unwrap() =
        Some((claim.stamp.lease_id.clone(), 0, reg.clone()));
    let probe = RegistryProbe {
        registry: reg.clone(),
        force_prompting: Some(true),
    };
    let report = escalate_claimed_lease(
        &host,
        &probe,
        reg.as_ref(),
        &claim,
        Duration::from_millis(20),
    )
    .await;
    assert!(matches!(
        report.stage,
        EscalationStage::Turn | EscalationStage::Disconnect
    ));
    assert!(host.turn_calls.load(Ordering::SeqCst) >= 1);
    assert_eq!(host.mcp_calls.load(Ordering::SeqCst), 1);
}

/// Broker child activity renews the exact parent wait lease only.
#[tokio::test]
async fn broker_child_activity_renews_parent_delegation_lease() {
    let reg = ToolExecutionLeaseRegistry::new(ToolWatchdogSettings::default());
    let turn = sample_turn(1);
    let t0 = clock_base();
    reg.start_turn(turn.clone(), t0).await;
    let parent = register_tool(&reg, &turn, "parent-wait", ToolCategory::Delegation, t0).await;
    let sibling = register_tool(&reg, &turn, "other-tool", ToolCategory::Other, t0).await;

    let near = t0.advanced(590);
    let _ = reg
        .record_tool_progress_at(
            progress_key(&turn, "parent-wait"),
            SemanticProgress::DelegationActivity { at_mono_ms: 1 },
            near,
        )
        .await;

    let scan = reg.scan(t0.advanced(600)).await;
    let warned: Vec<_> = scan
        .iter()
        .filter_map(|a| match a {
            RegistryAction::PublishWarning { stamp, .. } => Some(stamp.lease_id.clone()),
            _ => None,
        })
        .collect();
    assert!(
        !warned.contains(&parent.lease_id),
        "parent should have been renewed: {warned:?}"
    );
    assert!(
        warned.contains(&sibling.lease_id),
        "silent sibling must warn: {warned:?}"
    );
}

/// Ambiguous parallel terminals never bind a guessed kill — turn fallback only.
#[tokio::test]
async fn ambiguous_parallel_terminals_keep_turn_capability() {
    let reg = ToolExecutionLeaseRegistry::new(ToolWatchdogSettings::default());
    let turn = sample_turn(1);
    let t0 = clock_base();
    reg.start_turn(turn.clone(), t0).await;
    let stamp = register_tool(&reg, &turn, "multi-term", ToolCategory::Terminal, t0).await;
    let grace = advance_to_grace(&reg, t0).await;
    let cancel = reg.scan(t0.advanced(1200)).await;
    let RegistryAction::ClaimCancel { claim } = &cancel[0] else {
        panic!("cancel: {cancel:?}");
    };
    assert_eq!(claim.capability, CancellationCapability::Turn);
    assert_eq!(claim.stamp.lease_id, grace.lease_id);
    assert_eq!(claim.stamp.lease_id, stamp.lease_id);
}

/// Exactly one non-in_progress terminal outcome across timeout/user/complete/disconnect paths.
#[tokio::test]
async fn single_terminal_outcome_across_settlement_paths() {
    // Path A: automatic timeout
    {
        let reg = ToolExecutionLeaseRegistry::new(ToolWatchdogSettings::default());
        let turn = sample_turn(1);
        let t0 = clock_base();
        reg.start_turn(turn.clone(), t0).await;
        let stamp = register_tool(&reg, &turn, "a", ToolCategory::Other, t0).await;
        let grace = advance_to_grace(&reg, t0).await;
        let (claim, _) = reg
            .claim_cancel(&grace.lease_id, grace.version, CancelCause::AutoTimeout)
            .await
            .unwrap();
        let out = reg
            .settle_cancel(
                &claim.stamp.lease_id,
                claim.stamp.version,
                CancellationScope::Turn,
                ERROR_CODE_TOOL_STALLED_TIMEOUT,
            )
            .await
            .unwrap();
        assert_eq!(out.phase, ToolWatchdogPhase::TimedOut);
        assert!(!reg.is_live(&stamp.lease_id).await);
        assert!(reg
            .settle_cancel(
                &claim.stamp.lease_id,
                claim.stamp.version,
                CancellationScope::Turn,
                ERROR_CODE_TOOL_STALLED_TIMEOUT,
            )
            .await
            .is_err());
    }

    // Path B: user stop
    {
        let reg = ToolExecutionLeaseRegistry::new(ToolWatchdogSettings::default());
        let turn = sample_turn(1);
        let t0 = clock_base();
        reg.start_turn(turn.clone(), t0).await;
        let stamp = register_tool(&reg, &turn, "b", ToolCategory::Other, t0).await;
        let grace = advance_to_grace(&reg, t0).await;
        let (claim, _) = reg
            .claim_cancel(&grace.lease_id, grace.version, CancelCause::UserStop)
            .await
            .unwrap();
        let out = reg
            .settle_cancel(
                &claim.stamp.lease_id,
                claim.stamp.version,
                CancellationScope::Turn,
                ERROR_CODE_USER_CANCELLED,
            )
            .await
            .unwrap();
        assert_eq!(out.error_code.as_deref(), Some(ERROR_CODE_USER_CANCELLED));
        assert!(!reg.is_live(&stamp.lease_id).await);
    }

    // Path C: completion clears without timeout
    {
        let reg = ToolExecutionLeaseRegistry::new(ToolWatchdogSettings::default());
        let turn = sample_turn(1);
        let t0 = clock_base();
        reg.start_turn(turn.clone(), t0).await;
        let stamp = register_tool(&reg, &turn, "c", ToolCategory::Other, t0).await;
        let cleared = reg.complete_tool(&tool_key(&turn, "c")).await;
        assert!(cleared.is_some());
        assert!(!reg.is_live(&stamp.lease_id).await);
        let actions = reg.scan(t0.advanced(1200)).await;
        assert!(actions.is_empty(), "completed tool must not cancel later");
    }

    // Path D: disconnect clears leases
    {
        let reg = ToolExecutionLeaseRegistry::new(ToolWatchdogSettings::default());
        let turn = sample_turn(1);
        let t0 = clock_base();
        reg.start_turn(turn.clone(), t0).await;
        let stamp = register_tool(&reg, &turn, "d", ToolCategory::Other, t0).await;
        let cleared = reg
            .remove_connection(&turn.connection_id, &turn.connection_incarnation)
            .await;
        assert!(!cleared.is_empty());
        assert!(!reg.is_live(&stamp.lease_id).await);
    }
}

/// After specific cancel of one turn, the same external session can accept another prompt.
#[tokio::test]
async fn same_session_accepts_next_prompt_after_specific_cancel() {
    let reg = ToolExecutionLeaseRegistry::new(ToolWatchdogSettings::default());
    let turn1 = sample_turn(1);
    let t0 = clock_base();
    reg.start_turn(turn1.clone(), t0).await;
    let stamp = register_tool(&reg, &turn1, "tool", ToolCategory::Other, t0).await;
    let grace = advance_to_grace(&reg, t0).await;
    let (claim, _) = reg
        .claim_cancel(&grace.lease_id, grace.version, CancelCause::UserStop)
        .await
        .unwrap();
    let _ = reg
        .settle_cancel(
            &claim.stamp.lease_id,
            claim.stamp.version,
            CancellationScope::Turn,
            ERROR_CODE_USER_CANCELLED,
        )
        .await
        .unwrap();

    let turn2 = sample_turn(2);
    let t1 = t0.advanced(1300);
    reg.start_turn(turn2.clone(), t1).await;
    let stamp2 = register_tool(&reg, &turn2, "tool-next", ToolCategory::Other, t1).await;
    assert!(reg.is_live(&stamp2.lease_id).await);
    assert!(!reg.is_live(&stamp.lease_id).await);
}

/// Desktop/server settings cores produce equivalent clamped projections/outcomes.
#[tokio::test]
async fn desktop_and_server_settings_cores_equivalent() {
    let db = fresh_in_memory_db().await;
    let manager = ConnectionManager::new();

    let defaults = acp_get_tool_watchdog_settings_core(&db.conn).await;
    assert!(defaults.enabled);
    assert_eq!(defaults.warning_after_seconds, 600);
    assert_eq!(defaults.grace_seconds, 600);

    let applied = acp_set_tool_watchdog_settings_core(
        &db.conn,
        &manager,
        ToolWatchdogSettings {
            enabled: true,
            warning_after_seconds: 59,
            grace_seconds: 3601,
        },
    )
    .await
    .expect("set");
    assert_eq!(applied.warning_after_seconds, 60);
    assert_eq!(applied.grace_seconds, 3600);

    let reloaded = acp_get_tool_watchdog_settings_core(&db.conn).await;
    assert_eq!(reloaded, applied);

    let live = manager.tool_lease_registry().settings().await;
    assert_eq!(live, applied);

    let err = acp_tool_watchdog_extend_core(&manager, "missing".into(), 1)
        .await
        .unwrap_err();
    assert!(
        format!("{err:?}").contains("stale_tool_watchdog_lease")
            || format!("{err}").contains("stale_tool_watchdog_lease")
    );
    let err = acp_tool_watchdog_cancel_core(&manager, "missing".into(), 1)
        .await
        .unwrap_err();
    assert!(
        format!("{err:?}").contains("stale_tool_watchdog_lease")
            || format!("{err}").contains("stale_tool_watchdog_lease")
    );
}

// ─── Test doubles ──────────────────────────────────────────────────────────

struct ScriptedHost {
    mcp_ok: Mutex<bool>,
    mcp_calls: AtomicUsize,
    turn_calls: AtomicUsize,
    disconnect_calls: AtomicUsize,
    settle_on_turn: Mutex<bool>,
    settle_lease_after_ms: Mutex<Option<(String, u64, Arc<ToolExecutionLeaseRegistry>)>>,
}

impl ScriptedHost {
    fn new() -> Self {
        Self {
            mcp_ok: Mutex::new(true),
            mcp_calls: AtomicUsize::new(0),
            turn_calls: AtomicUsize::new(0),
            disconnect_calls: AtomicUsize::new(0),
            settle_on_turn: Mutex::new(false),
            settle_lease_after_ms: Mutex::new(None),
        }
    }
}

impl CancelHost for ScriptedHost {
    fn admit_cancel_terminal(
        &self,
        _stamp: &LeaseStamp,
        _session_id: &str,
        _terminal_id: &str,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<(), SpecificCancelOutcome>> + Send + '_>>
    {
        Box::pin(async { Ok(()) })
    }

    fn cancel_delegation_task(
        &self,
        _stamp: &LeaseStamp,
        _task_id: &str,
        _cause: CancelCause,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<(), SpecificCancelOutcome>> + Send + '_>>
    {
        Box::pin(async { Ok(()) })
    }

    fn cancel_delegation_wait(
        &self,
        _stamp: &LeaseStamp,
        _wait_id: &str,
        _cause: CancelCause,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<(), SpecificCancelOutcome>> + Send + '_>>
    {
        Box::pin(async { Ok(()) })
    }

    fn cancel_mcp(
        &self,
        stamp: &LeaseStamp,
        _token: McpCancelToken,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<(), SpecificCancelOutcome>> + Send + '_>>
    {
        self.mcp_calls.fetch_add(1, Ordering::SeqCst);
        let ok = *self.mcp_ok.lock().unwrap();
        let settle = self.settle_lease_after_ms.lock().unwrap().clone();
        let settle_on_turn = *self.settle_on_turn.lock().unwrap();
        let stamp = stamp.clone();
        Box::pin(async move {
            if ok {
                if let Some((lease_id, ms, reg)) = settle {
                    if lease_id == stamp.lease_id && !settle_on_turn {
                        tokio::time::sleep(Duration::from_millis(ms)).await;
                        let _ = reg
                            .settle_cancel(
                                &lease_id,
                                stamp.version,
                                CancellationScope::McpRequest,
                                ERROR_CODE_TOOL_STALLED_TIMEOUT,
                            )
                            .await;
                    }
                }
                Ok(())
            } else {
                Err(SpecificCancelOutcome::Failed)
            }
        })
    }

    fn cancel_turn(
        &self,
        stamp: &LeaseStamp,
        _cause: CancelCause,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<(), ()>> + Send + '_>> {
        self.turn_calls.fetch_add(1, Ordering::SeqCst);
        let settle = self.settle_lease_after_ms.lock().unwrap().clone();
        let settle_on_turn = *self.settle_on_turn.lock().unwrap();
        let stamp = stamp.clone();
        Box::pin(async move {
            if settle_on_turn {
                if let Some((lease_id, ms, reg)) = settle {
                    if lease_id == stamp.lease_id {
                        tokio::time::sleep(Duration::from_millis(ms)).await;
                        let _ = reg
                            .settle_cancel(
                                &lease_id,
                                stamp.version,
                                CancellationScope::Turn,
                                ERROR_CODE_TOOL_STALLED_TIMEOUT,
                            )
                            .await;
                    }
                }
            }
            Ok(())
        })
    }

    fn disconnect_incarnation(
        &self,
        connection_id: &str,
        incarnation: &str,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<(), ()>> + Send + '_>> {
        self.disconnect_calls.fetch_add(1, Ordering::SeqCst);
        let settle = self.settle_lease_after_ms.lock().unwrap().clone();
        let connection_id = connection_id.to_string();
        let incarnation = incarnation.to_string();
        Box::pin(async move {
            if let Some((_lease_id, _ms, reg)) = settle {
                let _ = reg.remove_connection(&connection_id, &incarnation).await;
            }
            Ok(())
        })
    }
}

// Keep ConvergenceProbe in scope for type checks (RegistryProbe implements it).
#[allow(dead_code)]
fn _assert_probe() {
    let _: &dyn ConvergenceProbe = &RegistryProbe {
        registry: Arc::new(ToolExecutionLeaseRegistry::new(
            ToolWatchdogSettings::default(),
        )),
        force_prompting: Some(true),
    };
}
