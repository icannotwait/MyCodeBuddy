//! Tool-execution cancel escalation supervisor.
//!
//! After a lease claims cancellation, invoke the narrowest capability, wait up
//! to [`CANCEL_CONVERGENCE_SECS`] for the **lease** to leave the live map, then
//! escalate generation-guarded turn cancel and finally incarnation-guarded
//! disconnect. Terminal process exit alone while the turn stays Prompting is
//! not enough to skip escalation.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use super::registry::{CancelCause, CancellationClaim, ToolExecutionLeaseRegistry};
use super::types::{
    CancellationCapability, CancellationScope, ERROR_CODE_TOOL_STALLED_TIMEOUT,
    ERROR_CODE_USER_CANCELLED, LeaseStamp, McpCancelToken, WaitStamp,
};

/// Bounded wait to admit any control-lane message when the channel is full
/// (CancelTurn, Disconnect, CancelTerminal, …). Escalation stages must not
/// hang forever on a stalled receiver.
pub const CONTROL_LANE_ADMIT_TIMEOUT: Duration = Duration::from_millis(200);
/// Admission + ack budget for control-lane terminal cancel.
pub const TERMINAL_ADMIT_TIMEOUT: Duration = CONTROL_LANE_ADMIT_TIMEOUT;
/// Wait for the connection loop's admission oneshot.
pub const TERMINAL_ACK_TIMEOUT: Duration = Duration::from_millis(200);
/// Detached process-tree kill deadline (must not block control loop).
pub const TERMINAL_KILL_EXECUTOR_TIMEOUT: Duration = Duration::from_secs(8);

/// Result of a single specific-cancel attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpecificCancelOutcome {
    /// Capability work admitted/invoked (may still need convergence wait).
    Invoked,
    /// Admit/ack/capability failed — continue escalation budget.
    Failed,
    /// Capability is Turn: skip straight to turn cancel.
    SkipToTurn,
}

/// Escalation stage that produced convergence (or failure).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EscalationStage {
    Specific,
    Turn,
    Disconnect,
    /// Lease already gone before/during (completion-before-claim winner path).
    AlreadyTerminal,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EscalationReport {
    pub stage: EscalationStage,
    pub error_code: String,
    pub cancellation_scope: CancellationScope,
    /// True when specific cancel was attempted and the lease left the map
    /// within the first convergence window.
    pub specific_converged: bool,
    pub turn_converged: bool,
    pub disconnected: bool,
    /// Specific cancel returned [`SpecificCancelOutcome::Failed`].
    pub specific_failed: bool,
    /// Generation-guarded turn cancel returned `Err`.
    pub turn_failed: bool,
    /// Incarnation-guarded disconnect returned `Err`.
    pub disconnect_failed: bool,
    /// TimedOut projection from supervisor-owned `settle_cancel`, when the
    /// supervisor itself removed the lease (not when a host path already did).
    pub settled_projection: Option<super::types::ToolWatchdogProjection>,
}

impl EscalationReport {
    /// True when any specific/turn/disconnect operation failed.
    pub fn had_operation_failure(&self) -> bool {
        self.specific_failed || self.turn_failed || self.disconnect_failed
    }
}

/// Host capabilities the supervisor needs to cancel a claimed lease.
pub trait CancelHost: Send + Sync {
    fn admit_cancel_terminal(
        &self,
        stamp: &LeaseStamp,
        session_id: &str,
        terminal_id: &str,
    ) -> Pin<Box<dyn Future<Output = Result<(), SpecificCancelOutcome>> + Send + '_>>;

    /// Specific-stage Broker cancel for a verified singleton task.
    /// `cause` must be forwarded so UserStop maps to `user_cancelled` and
    /// AutoTimeout maps to `tool_stalled_timeout` (never hard-code timeout).
    fn cancel_delegation_task(
        &self,
        stamp: &LeaseStamp,
        task_id: &str,
        cause: CancelCause,
    ) -> Pin<Box<dyn Future<Output = Result<(), SpecificCancelOutcome>> + Send + '_>>;

    fn cancel_delegation_wait(
        &self,
        stamp: &LeaseStamp,
        wait_id: &str,
        cause: CancelCause,
    ) -> Pin<Box<dyn Future<Output = Result<(), SpecificCancelOutcome>> + Send + '_>>;

    fn cancel_mcp(
        &self,
        stamp: &LeaseStamp,
        token: McpCancelToken,
    ) -> Pin<Box<dyn Future<Output = Result<(), SpecificCancelOutcome>> + Send + '_>>;

    /// Generation-guarded session/cancel. `cause` distinguishes automatic
    /// `tool_stalled_timeout` from user `user_cancelled` (must not route
    /// AutoTimeout through user-cancel cascade semantics).
    fn cancel_turn(
        &self,
        stamp: &LeaseStamp,
        cause: CancelCause,
    ) -> Pin<Box<dyn Future<Output = Result<(), ()>> + Send + '_>>;

    fn disconnect_incarnation(
        &self,
        connection_id: &str,
        incarnation: &str,
    ) -> Pin<Box<dyn Future<Output = Result<(), ()>> + Send + '_>>;
}

/// Observe lease liveness and optional turn state for convergence.
pub trait ConvergenceProbe: Send + Sync {
    fn lease_is_live(
        &self,
        lease_id: &str,
    ) -> Pin<Box<dyn Future<Output = bool> + Send + '_>>;

    /// True when the stamped turn is still Prompting with no approved
    /// semantic progress after specific cancel (forces escalation).
    fn turn_still_prompting(
        &self,
        stamp: &LeaseStamp,
    ) -> Pin<Box<dyn Future<Output = bool> + Send + '_>>;
}

/// Build a wait stamp for DelegationWait cancel from the lease stamp.
pub fn wait_stamp_from_lease(
    lease: &LeaseStamp,
    wait_id: &str,
    parent_conversation_id: i32,
) -> WaitStamp {
    WaitStamp {
        wait_id: wait_id.to_string(),
        connection_id: lease.connection_id.clone(),
        connection_incarnation: lease.connection_incarnation.clone(),
        turn_generation: lease.turn_generation,
        parent_conversation_id,
        parent_tool_use_id: lease.tool_call_id.clone(),
    }
}

pub fn error_code_for_cause(cause: CancelCause) -> &'static str {
    match cause {
        CancelCause::AutoTimeout => ERROR_CODE_TOOL_STALLED_TIMEOUT,
        CancelCause::UserStop => ERROR_CODE_USER_CANCELLED,
    }
}

pub fn scope_for_capability(cap: &CancellationCapability) -> CancellationScope {
    match cap {
        CancellationCapability::Terminal { .. } => CancellationScope::Terminal,
        CancellationCapability::Delegation { .. } => CancellationScope::Delegation,
        CancellationCapability::DelegationWait { .. } => CancellationScope::DelegationWait,
        CancellationCapability::McpRequest { .. } => CancellationScope::McpRequest,
        CancellationCapability::Turn => CancellationScope::Turn,
    }
}

/// Run specific-cancel → turn → disconnect escalation for a claimed lease.
pub async fn escalate_claimed_lease<H, P>(
    host: &H,
    probe: &P,
    registry: &ToolExecutionLeaseRegistry,
    claim: &CancellationClaim,
    convergence: Duration,
) -> EscalationReport
where
    H: CancelHost,
    P: ConvergenceProbe,
{
    let error_code = error_code_for_cause(claim.cause).to_string();
    let mut scope = scope_for_capability(&claim.capability);

    let mut specific_failed = false;
    let mut specific_converged = false;

    // Lease already gone before the detached supervisor starts (claim won the
    // registry, then a late tool final settled TimedOut). Do **not** return
    // AlreadyTerminal without consulting the stamped turn: if still Prompting,
    // skip specific cancel and proceed to generation-guarded CancelTurn.
    if !probe.lease_is_live(&claim.stamp.lease_id).await {
        if !probe.turn_still_prompting(&claim.stamp).await {
            return EscalationReport {
                stage: EscalationStage::AlreadyTerminal,
                error_code,
                cancellation_scope: scope,
                specific_converged: true,
                turn_converged: true,
                disconnected: false,
                specific_failed: false,
                turn_failed: false,
                disconnect_failed: false,
                settled_projection: None,
            };
        }
        // Lease terminal but turn still Prompting — escalate from turn stage.
        specific_converged = true;
    } else {
        let specific_outcome = match &claim.capability {
            CancellationCapability::Turn => SpecificCancelOutcome::SkipToTurn,
            CancellationCapability::Terminal {
                session_id,
                terminal_id,
            } => match host
                .admit_cancel_terminal(&claim.stamp, session_id, terminal_id)
                .await
            {
                Ok(()) => SpecificCancelOutcome::Invoked,
                Err(o) => o,
            },
            CancellationCapability::Delegation { task_id } => {
                // Bound the Broker cancel: a stalled child cancel/settle must
                // not block forever before generation-guarded CancelTurn.
                // Budget matches the convergence window passed by the caller
                // (production: CANCEL_CONVERGENCE_SECS).
                match tokio::time::timeout(
                    convergence,
                    host.cancel_delegation_task(&claim.stamp, task_id, claim.cause),
                )
                .await
                {
                    Ok(Ok(())) => SpecificCancelOutcome::Invoked,
                    Ok(Err(o)) => o,
                    Err(_elapsed) => SpecificCancelOutcome::Failed,
                }
            }
            CancellationCapability::DelegationWait { wait_id } => {
                match host
                    .cancel_delegation_wait(&claim.stamp, wait_id, claim.cause)
                    .await
                {
                    Ok(()) => SpecificCancelOutcome::Invoked,
                    Err(o) => o,
                }
            }
            CancellationCapability::McpRequest { cancel_token } => {
                match host.cancel_mcp(&claim.stamp, *cancel_token).await {
                    Ok(()) => SpecificCancelOutcome::Invoked,
                    Err(o) => o,
                }
            }
        };
        specific_failed = matches!(specific_outcome, SpecificCancelOutcome::Failed);

        if matches!(specific_outcome, SpecificCancelOutcome::Invoked) {
            // Wait for lease terminal AND turn exit. Tool final/settlement alone
            // while the stamped turn remains Prompting is not enough — escalate.
            specific_converged = wait_lease_converged(probe, &claim.stamp, convergence).await;
            if specific_converged {
                // Lease already removed by host settle, or settle now if still live.
                let settled_projection = registry
                    .settle_cancel(
                        &claim.stamp.lease_id,
                        claim.stamp.version,
                        scope,
                        &error_code,
                    )
                    .await
                    .ok();
                return EscalationReport {
                    stage: EscalationStage::Specific,
                    error_code,
                    cancellation_scope: scope,
                    specific_converged: true,
                    turn_converged: true,
                    disconnected: false,
                    specific_failed: false,
                    turn_failed: false,
                    disconnect_failed: false,
                    settled_projection,
                };
            }
        }
        // SpecificCancelOutcome::Failed / SkipToTurn: continue escalation budget.
    }

    // Turn cancel (generation-guarded). Cause is required so AutoTimeout never
    // routes through user-cancel parent-tree cascade semantics.
    scope = CancellationScope::Turn;
    let turn_failed = host.cancel_turn(&claim.stamp, claim.cause).await.is_err();
    let turn_converged = wait_lease_converged(probe, &claim.stamp, convergence).await;
    if turn_converged {
        let settled_projection = registry
            .settle_cancel(
                &claim.stamp.lease_id,
                claim.stamp.version,
                scope,
                &error_code,
            )
            .await
            .ok();
        return EscalationReport {
            stage: EscalationStage::Turn,
            error_code,
            cancellation_scope: scope,
            specific_converged,
            turn_converged: true,
            disconnected: false,
            specific_failed,
            turn_failed,
            disconnect_failed: false,
            settled_projection,
        };
    }

    // Disconnect fallback (incarnation-guarded).
    scope = CancellationScope::Connection;
    let disconnect_failed = host
        .disconnect_incarnation(
            &claim.stamp.connection_id,
            &claim.stamp.connection_incarnation,
        )
        .await
        .is_err();
    // Disconnect must clear leases via existing remove_connection path; settle
    // if still present so the lease never strands as Cancelling.
    let settled_projection = if probe.lease_is_live(&claim.stamp.lease_id).await {
        registry
            .settle_cancel(
                &claim.stamp.lease_id,
                claim.stamp.version,
                scope,
                &error_code,
            )
            .await
            .ok()
    } else {
        None
    };

    EscalationReport {
        stage: EscalationStage::Disconnect,
        error_code,
        cancellation_scope: scope,
        specific_converged,
        turn_converged: false,
        // True when disconnect was invoked (success or fail); lease is settled.
        disconnected: true,
        specific_failed,
        turn_failed,
        disconnect_failed,
        settled_projection,
    }
}

/// Convergence requires **both** lease removal and the stamped turn leaving
/// Prompting. Lease removal alone (e.g. `complete_tool` settling a Cancelling
/// claim after terminal exit) is not enough while the turn remains active —
/// the supervisor must escalate to generation-guarded turn cancel.
async fn wait_lease_converged<P: ConvergenceProbe>(
    probe: &P,
    stamp: &LeaseStamp,
    budget: Duration,
) -> bool {
    let deadline = tokio::time::Instant::now() + budget;
    loop {
        let lease_live = probe.lease_is_live(&stamp.lease_id).await;
        let still_prompting = probe.turn_still_prompting(stamp).await;
        if !lease_live && !still_prompting {
            return true;
        }
        if tokio::time::Instant::now() >= deadline {
            let lease_live = probe.lease_is_live(&stamp.lease_id).await;
            let still_prompting = probe.turn_still_prompting(stamp).await;
            return !lease_live && !still_prompting;
        }
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        let slice = remaining.min(Duration::from_millis(20));
        if slice.is_zero() {
            let lease_live = probe.lease_is_live(&stamp.lease_id).await;
            let still_prompting = probe.turn_still_prompting(stamp).await;
            return !lease_live && !still_prompting;
        }
        tokio::time::sleep(slice).await;
    }
}

/// Registry-backed convergence probe.
pub struct RegistryProbe {
    pub registry: Arc<ToolExecutionLeaseRegistry>,
    /// Optional override: when set, forces "turn still prompting" for tests.
    pub force_prompting: Option<bool>,
}

impl ConvergenceProbe for RegistryProbe {
    fn lease_is_live(
        &self,
        lease_id: &str,
    ) -> Pin<Box<dyn Future<Output = bool> + Send + '_>> {
        let registry = self.registry.clone();
        let id = lease_id.to_string();
        Box::pin(async move { registry.is_live(&id).await })
    }

    fn turn_still_prompting(
        &self,
        _stamp: &LeaseStamp,
    ) -> Pin<Box<dyn Future<Output = bool> + Send + '_>> {
        let forced = self.force_prompting;
        Box::pin(async move { forced.unwrap_or(true) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acp::tool_watchdog::registry::{
        CancelCause, RegisterTool, ToolExecutionLeaseRegistry, WatchdogInstant,
    };
    use crate::acp::tool_watchdog::types::{
        CancellationCapability, ToolCategory, ToolWatchdogSettings, ERROR_CODE_TOOL_STALLED_TIMEOUT,
        ERROR_CODE_USER_CANCELLED,
    };
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    struct ScriptedHost {
        terminal_result: Mutex<Result<(), SpecificCancelOutcome>>,
        turn_calls: AtomicUsize,
        disconnect_calls: AtomicUsize,
        delegation_calls: AtomicUsize,
        wait_calls: AtomicUsize,
        mcp_calls: AtomicUsize,
        /// When true, `cancel_delegation_task` never resolves (Critical R3).
        hang_delegation_cancel: bool,
        last_delegation_cause: Mutex<Option<CancelCause>>,
        settle_lease_after_ms: Mutex<Option<(String, u64, Arc<ToolExecutionLeaseRegistry>)>>,
    }

    impl ScriptedHost {
        fn new() -> Self {
            Self {
                terminal_result: Mutex::new(Ok(())),
                turn_calls: AtomicUsize::new(0),
                disconnect_calls: AtomicUsize::new(0),
                delegation_calls: AtomicUsize::new(0),
                wait_calls: AtomicUsize::new(0),
                mcp_calls: AtomicUsize::new(0),
                hang_delegation_cancel: false,
                last_delegation_cause: Mutex::new(None),
                settle_lease_after_ms: Mutex::new(None),
            }
        }
    }

    impl CancelHost for ScriptedHost {
        fn admit_cancel_terminal(
            &self,
            stamp: &LeaseStamp,
            _session_id: &str,
            _terminal_id: &str,
        ) -> Pin<Box<dyn Future<Output = Result<(), SpecificCancelOutcome>> + Send + '_>> {
            let result = *self.terminal_result.lock().expect("lock");
            let settle = self.settle_lease_after_ms.lock().expect("lock").clone();
            let stamp = stamp.clone();
            Box::pin(async move {
                if let Some((lease_id, ms, reg)) = settle {
                    if lease_id == stamp.lease_id {
                        let reg2 = reg.clone();
                        let lid = lease_id.clone();
                        let ver = stamp.version;
                        tokio::spawn(async move {
                            tokio::time::sleep(Duration::from_millis(ms)).await;
                            let _ = reg2
                                .settle_cancel(
                                    &lid,
                                    ver,
                                    CancellationScope::Terminal,
                                    ERROR_CODE_TOOL_STALLED_TIMEOUT,
                                )
                                .await;
                        });
                    }
                }
                result
            })
        }

        fn cancel_delegation_task(
            &self,
            _stamp: &LeaseStamp,
            _task_id: &str,
            cause: CancelCause,
        ) -> Pin<Box<dyn Future<Output = Result<(), SpecificCancelOutcome>> + Send + '_>> {
            self.delegation_calls.fetch_add(1, Ordering::SeqCst);
            *self.last_delegation_cause.lock().expect("lock") = Some(cause);
            let hang = self.hang_delegation_cancel;
            Box::pin(async move {
                if hang {
                    std::future::pending::<()>().await;
                }
                Ok(())
            })
        }

        fn cancel_delegation_wait(
            &self,
            _stamp: &LeaseStamp,
            _wait_id: &str,
            _cause: CancelCause,
        ) -> Pin<Box<dyn Future<Output = Result<(), SpecificCancelOutcome>> + Send + '_>> {
            self.wait_calls.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { Ok(()) })
        }

        fn cancel_mcp(
            &self,
            _stamp: &LeaseStamp,
            _token: McpCancelToken,
        ) -> Pin<Box<dyn Future<Output = Result<(), SpecificCancelOutcome>> + Send + '_>> {
            self.mcp_calls.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { Ok(()) })
        }

        fn cancel_turn(
            &self,
            stamp: &LeaseStamp,
            _cause: CancelCause,
        ) -> Pin<Box<dyn Future<Output = Result<(), ()>> + Send + '_>> {
            self.turn_calls.fetch_add(1, Ordering::SeqCst);
            let settle = self.settle_lease_after_ms.lock().expect("lock").clone();
            let stamp = stamp.clone();
            Box::pin(async move {
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
                Ok(())
            })
        }

        fn disconnect_incarnation(
            &self,
            connection_id: &str,
            incarnation: &str,
        ) -> Pin<Box<dyn Future<Output = Result<(), ()>> + Send + '_>> {
            self.disconnect_calls.fetch_add(1, Ordering::SeqCst);
            let settle = self.settle_lease_after_ms.lock().expect("lock").clone();
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

    async fn register_cancelling(
        reg: &ToolExecutionLeaseRegistry,
        cap: CancellationCapability,
        cause: CancelCause,
    ) -> CancellationClaim {
        use crate::acp::tool_watchdog::TurnStamp;
        let at = WatchdogInstant::now();
        let turn = TurnStamp {
            connection_id: "conn".into(),
            connection_incarnation: "inc".into(),
            session_id: "sess".into(),
            turn_generation: 1,
        };
        let stamp = reg
            .register_tool(RegisterTool {
                turn: turn.clone(),
                tool_call_id: "tool-1".into(),
                category: ToolCategory::Terminal,
                at,
            })
            .await
            .expect("register")
            .stamp;
        let stamp = reg
            .bind_capability(&stamp, cap)
            .await
            .expect("bind capability");
        reg.claim_cancel(&stamp.lease_id, stamp.version, cause)
            .await
            .expect("claim")
            .0
    }

    #[tokio::test]
    async fn lease_gone_and_turn_idle_is_already_terminal() {
        let reg = Arc::new(ToolExecutionLeaseRegistry::new(
            ToolWatchdogSettings::default(),
        ));
        // Claim then settle (simulates completion race winner before escalate).
        let claim = register_cancelling(
            &reg,
            CancellationCapability::Terminal {
                session_id: "s".into(),
                terminal_id: "t".into(),
            },
            CancelCause::AutoTimeout,
        )
        .await;
        let _ = reg
            .settle_cancel(
                &claim.stamp.lease_id,
                claim.stamp.version,
                CancellationScope::Terminal,
                ERROR_CODE_TOOL_STALLED_TIMEOUT,
            )
            .await
            .unwrap();

        let host = ScriptedHost::new();
        // Turn already left Prompting → genuine terminal, no CancelTurn.
        let probe = RegistryProbe {
            registry: reg.clone(),
            force_prompting: Some(false),
        };
        let report = escalate_claimed_lease(
            &host,
            &probe,
            reg.as_ref(),
            &claim,
            Duration::from_millis(50),
        )
        .await;
        assert_eq!(report.stage, EscalationStage::AlreadyTerminal);
        assert_eq!(report.error_code, ERROR_CODE_TOOL_STALLED_TIMEOUT);
        assert_eq!(host.turn_calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn lease_gone_before_supervisor_start_while_prompting_cancels_turn() {
        // C2 pre-supervisor-start race: claim committed, late tool final settles
        // lease as TimedOut before the spawned escalate task runs, but the
        // stamped turn is still Prompting → must generation-guarded CancelTurn.
        let reg = Arc::new(ToolExecutionLeaseRegistry::new(
            ToolWatchdogSettings::default(),
        ));
        let claim = register_cancelling(
            &reg,
            CancellationCapability::Terminal {
                session_id: "s".into(),
                terminal_id: "t".into(),
            },
            CancelCause::AutoTimeout,
        )
        .await;
        // Late complete_tool settles as TimedOut (claim owns outcome, not Cleared).
        let key = crate::acp::tool_watchdog::ToolLeaseKey {
            connection_id: "conn".into(),
            connection_incarnation: "inc".into(),
            turn_generation: 1,
            tool_call_id: "tool-1".into(),
        };
        let settled = reg.complete_tool(&key).await.expect("settle cancel");
        assert_eq!(settled.phase, crate::acp::tool_watchdog::ToolWatchdogPhase::TimedOut);
        assert_eq!(
            settled.error_code.as_deref(),
            Some(ERROR_CODE_TOOL_STALLED_TIMEOUT)
        );
        assert!(!reg.is_live(&claim.stamp.lease_id).await);

        let host = ScriptedHost::new();
        let probe = RegistryProbe {
            registry: reg.clone(),
            // Turn remains Prompting after tool settlement.
            force_prompting: Some(true),
        };
        let report = escalate_claimed_lease(
            &host,
            &probe,
            reg.as_ref(),
            &claim,
            Duration::from_millis(60),
        )
        .await;
        assert!(
            matches!(
                report.stage,
                EscalationStage::Turn | EscalationStage::Disconnect
            ),
            "expected turn escalation when lease gone but turn still Prompting, got {:?}",
            report.stage
        );
        assert_eq!(
            host.turn_calls.load(Ordering::SeqCst),
            1,
            "generation-guarded CancelTurn must run when turn still Prompting"
        );
    }

    #[tokio::test]
    async fn terminal_exit_while_turn_prompting_escalates_to_turn() {
        // Specific cancel "succeeds" at capability level but lease stays live
        // (Cancelling) while turn remains Prompting → escalate after budget.
        let reg = Arc::new(ToolExecutionLeaseRegistry::new(
            ToolWatchdogSettings::default(),
        ));
        let claim = register_cancelling(
            &reg,
            CancellationCapability::Terminal {
                session_id: "s".into(),
                terminal_id: "t".into(),
            },
            CancelCause::AutoTimeout,
        )
        .await;

        let host = ScriptedHost::new();
        // Settle only when turn cancel is invoked (after specific budget).
        // Use a custom host path: leave lease live during specific window.
        struct EscalateHost {
            inner: ScriptedHost,
            reg: Arc<ToolExecutionLeaseRegistry>,
            lease_id: String,
            version: u64,
        }
        impl CancelHost for EscalateHost {
            fn admit_cancel_terminal(
                &self,
                stamp: &LeaseStamp,
                session_id: &str,
                terminal_id: &str,
            ) -> Pin<Box<dyn Future<Output = Result<(), SpecificCancelOutcome>> + Send + '_>>
            {
                self.inner
                    .admit_cancel_terminal(stamp, session_id, terminal_id)
            }
            fn cancel_delegation_task(
                &self,
                stamp: &LeaseStamp,
                task_id: &str,
                cause: CancelCause,
            ) -> Pin<Box<dyn Future<Output = Result<(), SpecificCancelOutcome>> + Send + '_>>
            {
                self.inner.cancel_delegation_task(stamp, task_id, cause)
            }
            fn cancel_delegation_wait(
                &self,
                stamp: &LeaseStamp,
                wait_id: &str,
                cause: CancelCause,
            ) -> Pin<Box<dyn Future<Output = Result<(), SpecificCancelOutcome>> + Send + '_>>
            {
                self.inner.cancel_delegation_wait(stamp, wait_id, cause)
            }
            fn cancel_mcp(
                &self,
                stamp: &LeaseStamp,
                token: McpCancelToken,
            ) -> Pin<Box<dyn Future<Output = Result<(), SpecificCancelOutcome>> + Send + '_>>
            {
                self.inner.cancel_mcp(stamp, token)
            }
            fn cancel_turn(
                &self,
                stamp: &LeaseStamp,
                cause: CancelCause,
            ) -> Pin<Box<dyn Future<Output = Result<(), ()>> + Send + '_>> {
                self.inner.turn_calls.fetch_add(1, Ordering::SeqCst);
                let reg = self.reg.clone();
                let lease_id = self.lease_id.clone();
                let version = self.version;
                let stamp = stamp.clone();
                Box::pin(async move {
                    let _ = reg
                        .settle_cancel(
                            &lease_id,
                            version,
                            CancellationScope::Turn,
                            error_code_for_cause(cause),
                        )
                        .await;
                    let _ = stamp;
                    Ok(())
                })
            }
            fn disconnect_incarnation(
                &self,
                connection_id: &str,
                incarnation: &str,
            ) -> Pin<Box<dyn Future<Output = Result<(), ()>> + Send + '_>> {
                self.inner.disconnect_incarnation(connection_id, incarnation)
            }
        }

        let host = EscalateHost {
            inner: host,
            reg: reg.clone(),
            lease_id: claim.stamp.lease_id.clone(),
            version: claim.stamp.version,
        };
        // force_prompting false: after turn-cancel settles the lease, both
        // conditions of wait_lease_converged hold (lease gone + not Prompting).
        // During the specific window the lease stays live so Specific cannot
        // converge even though turn is not forced-prompting.
        let probe = RegistryProbe {
            registry: reg.clone(),
            force_prompting: Some(false),
        };
        let report = escalate_claimed_lease(
            &host,
            &probe,
            reg.as_ref(),
            &claim,
            Duration::from_millis(40),
        )
        .await;
        assert_eq!(report.stage, EscalationStage::Turn);
        assert!(!report.specific_converged);
        assert_eq!(host.inner.turn_calls.load(Ordering::SeqCst), 1);
        assert_eq!(host.inner.disconnect_calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn complete_tool_while_turn_prompting_escalates_to_turn() {
        // C2: final tool event settles Cancelling (lease leaves map) but the
        // stamped turn is still Prompting → specific stage must NOT converge;
        // escalate to generation-guarded turn cancel.
        let reg = Arc::new(ToolExecutionLeaseRegistry::new(
            ToolWatchdogSettings::default(),
        ));
        let claim = register_cancelling(
            &reg,
            CancellationCapability::Terminal {
                session_id: "s".into(),
                terminal_id: "t".into(),
            },
            CancelCause::AutoTimeout,
        )
        .await;

        struct EscalateHost {
            inner: ScriptedHost,
            reg: Arc<ToolExecutionLeaseRegistry>,
            lease_id: String,
            version: u64,
        }
        impl CancelHost for EscalateHost {
            fn admit_cancel_terminal(
                &self,
                stamp: &LeaseStamp,
                session_id: &str,
                terminal_id: &str,
            ) -> Pin<Box<dyn Future<Output = Result<(), SpecificCancelOutcome>> + Send + '_>>
            {
                // Simulate final tool event mid-specific-wait: lease removed
                // while turn still Prompting.
                let reg = self.reg.clone();
                let key = crate::acp::tool_watchdog::ToolLeaseKey {
                    connection_id: stamp.connection_id.clone(),
                    connection_incarnation: stamp.connection_incarnation.clone(),
                    turn_generation: stamp.turn_generation,
                    tool_call_id: "tool-1".into(),
                };
                let inner = self.inner.admit_cancel_terminal(stamp, session_id, terminal_id);
                Box::pin(async move {
                    let result = inner.await;
                    let _ = reg.complete_tool(&key).await;
                    result
                })
            }
            fn cancel_delegation_task(
                &self,
                stamp: &LeaseStamp,
                task_id: &str,
                cause: CancelCause,
            ) -> Pin<Box<dyn Future<Output = Result<(), SpecificCancelOutcome>> + Send + '_>>
            {
                self.inner.cancel_delegation_task(stamp, task_id, cause)
            }
            fn cancel_delegation_wait(
                &self,
                stamp: &LeaseStamp,
                wait_id: &str,
                cause: CancelCause,
            ) -> Pin<Box<dyn Future<Output = Result<(), SpecificCancelOutcome>> + Send + '_>>
            {
                self.inner.cancel_delegation_wait(stamp, wait_id, cause)
            }
            fn cancel_mcp(
                &self,
                stamp: &LeaseStamp,
                token: McpCancelToken,
            ) -> Pin<Box<dyn Future<Output = Result<(), SpecificCancelOutcome>> + Send + '_>>
            {
                self.inner.cancel_mcp(stamp, token)
            }
            fn cancel_turn(
                &self,
                stamp: &LeaseStamp,
                cause: CancelCause,
            ) -> Pin<Box<dyn Future<Output = Result<(), ()>> + Send + '_>> {
                self.inner.turn_calls.fetch_add(1, Ordering::SeqCst);
                let reg = self.reg.clone();
                let lease_id = self.lease_id.clone();
                let version = self.version;
                let stamp = stamp.clone();
                Box::pin(async move {
                    let _ = reg
                        .settle_cancel(
                            &lease_id,
                            version,
                            CancellationScope::Turn,
                            error_code_for_cause(cause),
                        )
                        .await;
                    let _ = stamp;
                    Ok(())
                })
            }
            fn disconnect_incarnation(
                &self,
                connection_id: &str,
                incarnation: &str,
            ) -> Pin<Box<dyn Future<Output = Result<(), ()>> + Send + '_>> {
                self.inner.disconnect_incarnation(connection_id, incarnation)
            }
        }

        let host = EscalateHost {
            inner: ScriptedHost::new(),
            reg: reg.clone(),
            lease_id: claim.stamp.lease_id.clone(),
            version: claim.stamp.version,
        };
        let probe = RegistryProbe {
            registry: reg.clone(),
            // Turn remains Prompting after tool settlement.
            force_prompting: Some(true),
        };
        let report = escalate_claimed_lease(
            &host,
            &probe,
            reg.as_ref(),
            &claim,
            Duration::from_millis(60),
        )
        .await;
        // force_prompting stays true so turn stage also cannot fully converge
        // in this unit probe — but turn cancel must still be invoked.
        assert!(
            matches!(
                report.stage,
                EscalationStage::Turn | EscalationStage::Disconnect
            ),
            "expected turn escalation, got {:?}",
            report.stage
        );
        assert!(!report.specific_converged);
        assert_eq!(host.inner.turn_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn turn_then_disconnect_at_convergence_boundaries() {
        let reg = Arc::new(ToolExecutionLeaseRegistry::new(
            ToolWatchdogSettings::default(),
        ));
        let claim = register_cancelling(
            &reg,
            CancellationCapability::Turn,
            CancelCause::AutoTimeout,
        )
        .await;

        // Custom host: turn cancel is a no-op; only disconnect clears leases.
        struct DisconnectOnlyHost {
            turn_calls: AtomicUsize,
            disconnect_calls: AtomicUsize,
            reg: Arc<ToolExecutionLeaseRegistry>,
        }
        impl CancelHost for DisconnectOnlyHost {
            fn admit_cancel_terminal(
                &self,
                _stamp: &LeaseStamp,
                _session_id: &str,
                _terminal_id: &str,
            ) -> Pin<Box<dyn Future<Output = Result<(), SpecificCancelOutcome>> + Send + '_>>
            {
                Box::pin(async { Ok(()) })
            }
            fn cancel_delegation_task(
                &self,
                _stamp: &LeaseStamp,
                _task_id: &str,
                _cause: CancelCause,
            ) -> Pin<Box<dyn Future<Output = Result<(), SpecificCancelOutcome>> + Send + '_>>
            {
                Box::pin(async { Ok(()) })
            }
            fn cancel_delegation_wait(
                &self,
                _stamp: &LeaseStamp,
                _wait_id: &str,
                _cause: CancelCause,
            ) -> Pin<Box<dyn Future<Output = Result<(), SpecificCancelOutcome>> + Send + '_>>
            {
                Box::pin(async { Ok(()) })
            }
            fn cancel_mcp(
                &self,
                _stamp: &LeaseStamp,
                _token: McpCancelToken,
            ) -> Pin<Box<dyn Future<Output = Result<(), SpecificCancelOutcome>> + Send + '_>>
            {
                Box::pin(async { Ok(()) })
            }
            fn cancel_turn(
                &self,
                _stamp: &LeaseStamp,
                _cause: CancelCause,
            ) -> Pin<Box<dyn Future<Output = Result<(), ()>> + Send + '_>> {
                self.turn_calls.fetch_add(1, Ordering::SeqCst);
                Box::pin(async { Ok(()) })
            }
            fn disconnect_incarnation(
                &self,
                connection_id: &str,
                incarnation: &str,
            ) -> Pin<Box<dyn Future<Output = Result<(), ()>> + Send + '_>> {
                self.disconnect_calls.fetch_add(1, Ordering::SeqCst);
                let reg = self.reg.clone();
                let connection_id = connection_id.to_string();
                let incarnation = incarnation.to_string();
                Box::pin(async move {
                    let _ = reg.remove_connection(&connection_id, &incarnation).await;
                    Ok(())
                })
            }
        }

        let host = DisconnectOnlyHost {
            turn_calls: AtomicUsize::new(0),
            disconnect_calls: AtomicUsize::new(0),
            reg: reg.clone(),
        };
        let probe = RegistryProbe {
            registry: reg.clone(),
            force_prompting: Some(true),
        };

        let report = escalate_claimed_lease(
            &host,
            &probe,
            reg.as_ref(),
            &claim,
            Duration::from_millis(30),
        )
        .await;
        assert_eq!(report.stage, EscalationStage::Disconnect);
        assert!(report.disconnected);
        assert_eq!(host.turn_calls.load(Ordering::SeqCst), 1);
        assert_eq!(host.disconnect_calls.load(Ordering::SeqCst), 1);
        assert_eq!(report.error_code, ERROR_CODE_TOOL_STALLED_TIMEOUT);
    }

    #[tokio::test]
    async fn user_stop_emits_user_cancelled_code() {
        let reg = Arc::new(ToolExecutionLeaseRegistry::new(
            ToolWatchdogSettings::default(),
        ));
        let claim = register_cancelling(
            &reg,
            CancellationCapability::Turn,
            CancelCause::UserStop,
        )
        .await;
        let host = ScriptedHost::new();
        *host.settle_lease_after_ms.lock().unwrap() =
            Some((claim.stamp.lease_id.clone(), 0, reg.clone()));
        let probe = RegistryProbe {
            registry: reg.clone(),
            force_prompting: Some(false),
        };
        // Force turn settle by removing on turn via settle_lease_after_ms in cancel_turn
        // — ScriptedHost cancel_turn settles when settle_lease_after_ms is set.
        let report = escalate_claimed_lease(
            &host,
            &probe,
            reg.as_ref(),
            &claim,
            Duration::from_millis(100),
        )
        .await;
        assert_eq!(report.error_code, ERROR_CODE_USER_CANCELLED);
        assert_ne!(report.error_code, ERROR_CODE_TOOL_STALLED_TIMEOUT);
    }

    #[tokio::test]
    async fn host_only_delegation_cancel_invokes_broker_path() {
        let reg = Arc::new(ToolExecutionLeaseRegistry::new(
            ToolWatchdogSettings::default(),
        ));
        let claim = register_cancelling(
            &reg,
            CancellationCapability::Delegation {
                task_id: "task-1".into(),
            },
            CancelCause::AutoTimeout,
        )
        .await;
        let host = ScriptedHost::new();
        *host.settle_lease_after_ms.lock().unwrap() =
            Some((claim.stamp.lease_id.clone(), 5, reg.clone()));
        // Reuse terminal settle path — patch by settling after admit via delegation:
        // ScriptedHost only auto-settles on admit_cancel_terminal. Settle manually:
        let host_reg = reg.clone();
        let lease_id = claim.stamp.lease_id.clone();
        let version = claim.stamp.version;
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(5)).await;
            let _ = host_reg
                .settle_cancel(
                    &lease_id,
                    version,
                    CancellationScope::Delegation,
                    ERROR_CODE_TOOL_STALLED_TIMEOUT,
                )
                .await;
        });
        let probe = RegistryProbe {
            registry: reg.clone(),
            // Lease settled and turn not prompting → specific stage converges.
            force_prompting: Some(false),
        };
        let report = escalate_claimed_lease(
            &host,
            &probe,
            reg.as_ref(),
            &claim,
            Duration::from_millis(200),
        )
        .await;
        assert_eq!(host.delegation_calls.load(Ordering::SeqCst), 1);
        assert_eq!(report.stage, EscalationStage::Specific);
    }

    #[tokio::test]
    async fn delegation_wait_cancel_does_not_call_task_cancel() {
        let reg = Arc::new(ToolExecutionLeaseRegistry::new(
            ToolWatchdogSettings::default(),
        ));
        let claim = register_cancelling(
            &reg,
            CancellationCapability::DelegationWait {
                wait_id: "wait-1".into(),
            },
            CancelCause::AutoTimeout,
        )
        .await;
        let host = ScriptedHost::new();
        let host_reg = reg.clone();
        let lease_id = claim.stamp.lease_id.clone();
        let version = claim.stamp.version;
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(5)).await;
            let _ = host_reg
                .settle_cancel(
                    &lease_id,
                    version,
                    CancellationScope::DelegationWait,
                    ERROR_CODE_TOOL_STALLED_TIMEOUT,
                )
                .await;
        });
        let probe = RegistryProbe {
            registry: reg.clone(),
            force_prompting: Some(false),
        };
        let _ = escalate_claimed_lease(
            &host,
            &probe,
            reg.as_ref(),
            &claim,
            Duration::from_millis(200),
        )
        .await;
        assert_eq!(host.wait_calls.load(Ordering::SeqCst), 1);
        assert_eq!(host.delegation_calls.load(Ordering::SeqCst), 0);
    }

    /// Controlled clock: default 600s warn + 600s grace, then wait-only cancel.
    /// No Broker child cancel and no disconnect on the healthy Specific path.
    #[tokio::test]
    async fn armed_wait_600s_warn_then_600s_grace_wait_only_cancel() {
        use crate::acp::tool_watchdog::registry::{RegistryAction, TurnStamp};
        use crate::acp::tool_watchdog::types::ToolWatchdogPhase;
        use chrono::{DateTime, Utc};
        use tokio::time::Instant;

        let reg = Arc::new(ToolExecutionLeaseRegistry::new(
            ToolWatchdogSettings::default(),
        ));
        let t0 = WatchdogInstant {
            mono: Instant::now(),
            wall: DateTime::parse_from_rfc3339("2026-07-23T12:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
        };
        let turn = TurnStamp {
            connection_id: "conn-wait-armed".into(),
            connection_incarnation: "inc".into(),
            session_id: "sess".into(),
            turn_generation: 1,
        };
        reg.start_turn(turn.clone(), t0).await;
        let stamp = reg
            .register_tool(RegisterTool {
                turn: turn.clone(),
                tool_call_id: "wait-tool".into(),
                category: ToolCategory::Delegation,
                at: t0,
            })
            .await
            .expect("register armed wait tool")
            .stamp;
        let stamp = reg
            .bind_capability(
                &stamp,
                CancellationCapability::DelegationWait {
                    wait_id: "wait-armed-1".into(),
                },
            )
            .await
            .expect("bind DelegationWait");

        // Pass 1: t+600s silence → PublishWarning only (never warn+cancel same pass).
        let warn_at = t0.advanced(600);
        let actions = reg.scan(warn_at).await;
        assert_eq!(actions.len(), 1, "exactly one action at 600s: {actions:?}");
        let RegistryAction::PublishWarning { stamp: w, .. } = &actions[0] else {
            panic!("expected PublishWarning at 600s, got {actions:?}");
        };
        assert_eq!(w.lease_id, stamp.lease_id);
        assert!(
            !actions
                .iter()
                .any(|a| matches!(a, RegistryAction::ClaimCancel { .. })),
            "must not ClaimCancel on the warning pass"
        );
        let grace = reg
            .warning_published(&w.lease_id, w.version, warn_at)
            .await
            .expect("enter grace");
        assert_eq!(grace.phase, ToolWatchdogPhase::Grace);

        // Mid-grace remains quiet.
        assert!(
            reg.scan(warn_at.advanced(599)).await.is_empty(),
            "no cancel before grace ends"
        );

        // Pass 2: warn_at + 600s → ClaimCancel with DelegationWait capability.
        let cancel_at = warn_at.advanced(600);
        let end = reg.scan(cancel_at).await;
        assert_eq!(end.len(), 1, "exactly one action at 1200s: {end:?}");
        let RegistryAction::ClaimCancel { claim, projection } = &end[0] else {
            panic!("expected ClaimCancel at 1200s, got {end:?}");
        };
        assert_eq!(claim.cause, CancelCause::AutoTimeout);
        assert_eq!(
            claim.capability,
            CancellationCapability::DelegationWait {
                wait_id: "wait-armed-1".into(),
            }
        );
        assert_eq!(projection.phase, ToolWatchdogPhase::Cancelling);

        let host = ScriptedHost::new();
        let host_reg = reg.clone();
        let lease_id = claim.stamp.lease_id.clone();
        let version = claim.stamp.version;
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(5)).await;
            let _ = host_reg
                .settle_cancel(
                    &lease_id,
                    version,
                    CancellationScope::DelegationWait,
                    ERROR_CODE_TOOL_STALLED_TIMEOUT,
                )
                .await;
        });
        let probe = RegistryProbe {
            registry: reg.clone(),
            force_prompting: Some(false),
        };
        let report = escalate_claimed_lease(
            &host,
            &probe,
            reg.as_ref(),
            claim,
            Duration::from_millis(200),
        )
        .await;
        assert_eq!(report.stage, EscalationStage::Specific);
        assert_eq!(
            host.wait_calls.load(Ordering::SeqCst),
            1,
            "must cancel the wait handle"
        );
        assert_eq!(
            host.delegation_calls.load(Ordering::SeqCst),
            0,
            "wait-only timeout must not Broker-cancel children"
        );
        assert_eq!(
            host.disconnect_calls.load(Ordering::SeqCst),
            0,
            "healthy wait-only Specific path must not disconnect"
        );
        assert_eq!(
            host.turn_calls.load(Ordering::SeqCst),
            0,
            "converged wait cancel must not escalate to turn cancel"
        );
    }

    #[tokio::test]
    async fn ambiguous_terminal_turn_only_skips_specific() {
        let reg = Arc::new(ToolExecutionLeaseRegistry::new(
            ToolWatchdogSettings::default(),
        ));
        let claim =
            register_cancelling(&reg, CancellationCapability::Turn, CancelCause::AutoTimeout)
                .await;
        let host = ScriptedHost::new();
        *host.settle_lease_after_ms.lock().unwrap() =
            Some((claim.stamp.lease_id.clone(), 0, reg.clone()));
        let probe = RegistryProbe {
            registry: reg.clone(),
            force_prompting: Some(false),
        };
        let report = escalate_claimed_lease(
            &host,
            &probe,
            reg.as_ref(),
            &claim,
            Duration::from_millis(100),
        )
        .await;
        // Turn capability → no terminal admit
        assert!(matches!(
            report.stage,
            EscalationStage::Turn | EscalationStage::Disconnect
        ));
        assert_eq!(host.turn_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn saturated_control_lane_admit_failure_continues_escalation() {
        let reg = Arc::new(ToolExecutionLeaseRegistry::new(
            ToolWatchdogSettings::default(),
        ));
        let claim = register_cancelling(
            &reg,
            CancellationCapability::Terminal {
                session_id: "s".into(),
                terminal_id: "t".into(),
            },
            CancelCause::AutoTimeout,
        )
        .await;
        let host = ScriptedHost::new();
        *host.terminal_result.lock().unwrap() = Err(SpecificCancelOutcome::Failed);
        *host.settle_lease_after_ms.lock().unwrap() =
            Some((claim.stamp.lease_id.clone(), 0, reg.clone()));
        let probe = RegistryProbe {
            registry: reg.clone(),
            force_prompting: Some(false),
        };
        let report = escalate_claimed_lease(
            &host,
            &probe,
            reg.as_ref(),
            &claim,
            Duration::from_millis(40),
        )
        .await;
        assert!(matches!(
            report.stage,
            EscalationStage::Turn | EscalationStage::Disconnect
        ));
        assert_eq!(host.turn_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn mcp_cancel_invoked_under_capability() {
        let reg = Arc::new(ToolExecutionLeaseRegistry::new(
            ToolWatchdogSettings::default(),
        ));
        let claim = register_cancelling(
            &reg,
            CancellationCapability::McpRequest {
                cancel_token: McpCancelToken::new(7),
            },
            CancelCause::AutoTimeout,
        )
        .await;
        let host = ScriptedHost::new();
        let host_reg = reg.clone();
        let lease_id = claim.stamp.lease_id.clone();
        let version = claim.stamp.version;
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(5)).await;
            let _ = host_reg
                .settle_cancel(
                    &lease_id,
                    version,
                    CancellationScope::McpRequest,
                    ERROR_CODE_TOOL_STALLED_TIMEOUT,
                )
                .await;
        });
        let probe = RegistryProbe {
            registry: reg.clone(),
            force_prompting: Some(false),
        };
        let report = escalate_claimed_lease(
            &host,
            &probe,
            reg.as_ref(),
            &claim,
            Duration::from_millis(200),
        )
        .await;
        assert_eq!(host.mcp_calls.load(Ordering::SeqCst), 1);
        assert_eq!(report.stage, EscalationStage::Specific);
    }

    #[tokio::test]
    async fn hanging_delegation_cancel_still_reaches_cancel_turn() {
        // Critical R3: a never-resolving Broker cancel must not strand the
        // lease as Cancelling — after the specific budget, escalate to
        // generation-guarded CancelTurn.
        let reg = Arc::new(ToolExecutionLeaseRegistry::new(
            ToolWatchdogSettings::default(),
        ));
        let claim = register_cancelling(
            &reg,
            CancellationCapability::Delegation {
                task_id: "task-hang".into(),
            },
            CancelCause::AutoTimeout,
        )
        .await;

        let mut host = ScriptedHost::new();
        host.hang_delegation_cancel = true;
        *host.settle_lease_after_ms.lock().unwrap() =
            Some((claim.stamp.lease_id.clone(), 0, reg.clone()));
        let probe = RegistryProbe {
            registry: reg.clone(),
            force_prompting: Some(false),
        };
        let report = escalate_claimed_lease(
            &host,
            &probe,
            reg.as_ref(),
            &claim,
            Duration::from_millis(40),
        )
        .await;
        assert_eq!(
            host.delegation_calls.load(Ordering::SeqCst),
            1,
            "specific-stage delegation cancel must still be attempted"
        );
        assert!(
            report.specific_failed,
            "hanging cancel must time out as specific_failed"
        );
        assert_eq!(
            host.turn_calls.load(Ordering::SeqCst),
            1,
            "CancelTurn must run after specific cancel budget"
        );
        assert!(
            matches!(
                report.stage,
                EscalationStage::Turn | EscalationStage::Disconnect
            ),
            "expected turn escalation after hanging cancel, got {:?}",
            report.stage
        );
        assert!(!reg.is_live(&claim.stamp.lease_id).await);
    }

    #[tokio::test]
    async fn user_stop_delegation_forwards_user_cancelled_cause() {
        // Important R3: UserStop on a delegation must plumb user_cancelled,
        // never hard-coded tool_stalled_timeout.
        let reg = Arc::new(ToolExecutionLeaseRegistry::new(
            ToolWatchdogSettings::default(),
        ));
        let claim = register_cancelling(
            &reg,
            CancellationCapability::Delegation {
                task_id: "task-user-stop".into(),
            },
            CancelCause::UserStop,
        )
        .await;
        let host = ScriptedHost::new();
        let host_reg = reg.clone();
        let lease_id = claim.stamp.lease_id.clone();
        let version = claim.stamp.version;
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(5)).await;
            let _ = host_reg
                .settle_cancel(
                    &lease_id,
                    version,
                    CancellationScope::Delegation,
                    ERROR_CODE_USER_CANCELLED,
                )
                .await;
        });
        let probe = RegistryProbe {
            registry: reg.clone(),
            force_prompting: Some(false),
        };
        let report = escalate_claimed_lease(
            &host,
            &probe,
            reg.as_ref(),
            &claim,
            Duration::from_millis(200),
        )
        .await;
        assert_eq!(host.delegation_calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            *host.last_delegation_cause.lock().unwrap(),
            Some(CancelCause::UserStop)
        );
        assert_eq!(report.error_code, ERROR_CODE_USER_CANCELLED);
        assert_ne!(report.error_code, ERROR_CODE_TOOL_STALLED_TIMEOUT);
        assert_eq!(report.stage, EscalationStage::Specific);
    }

    #[tokio::test]
    async fn convergence_constants_match_design() {
        use crate::acp::tool_watchdog::types::CANCEL_CONVERGENCE_SECS;
        assert_eq!(CANCEL_CONVERGENCE_SECS, 10);
        assert_eq!(CONTROL_LANE_ADMIT_TIMEOUT, Duration::from_millis(200));
        assert_eq!(TERMINAL_ADMIT_TIMEOUT, CONTROL_LANE_ADMIT_TIMEOUT);
        assert_eq!(TERMINAL_ACK_TIMEOUT, Duration::from_millis(200));
        assert_eq!(TERMINAL_KILL_EXECUTOR_TIMEOUT, Duration::from_secs(8));
    }
}
