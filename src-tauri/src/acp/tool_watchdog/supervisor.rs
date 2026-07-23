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

    fn cancel_delegation_task(
        &self,
        stamp: &LeaseStamp,
        task_id: &str,
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

    // Completion-before-claim: lease already gone.
    if !probe.lease_is_live(&claim.stamp.lease_id).await {
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
        };
    }

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
            match host.cancel_delegation_task(&claim.stamp, task_id).await {
                Ok(()) => SpecificCancelOutcome::Invoked,
                Err(o) => o,
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
    let specific_failed = matches!(specific_outcome, SpecificCancelOutcome::Failed);

    let mut specific_converged = false;
    if matches!(specific_outcome, SpecificCancelOutcome::Invoked) {
        // Wait for lease terminal. Tool exit alone while turn still Prompting
        // (Cancelling lease still live) is not enough — escalate after budget.
        specific_converged = wait_lease_converged(probe, &claim.stamp, convergence).await;
        if specific_converged {
            // Lease already removed by host settle, or settle now if still live.
            let _ = registry
                .settle_cancel(
                    &claim.stamp.lease_id,
                    claim.stamp.version,
                    scope,
                    &error_code,
                )
                .await;
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
            };
        }
    }
    // SpecificCancelOutcome::Failed / SkipToTurn: continue escalation budget.

    // Turn cancel (generation-guarded). Cause is required so AutoTimeout never
    // routes through user-cancel parent-tree cascade semantics.
    scope = CancellationScope::Turn;
    let turn_failed = host.cancel_turn(&claim.stamp, claim.cause).await.is_err();
    let turn_converged = wait_lease_converged(probe, &claim.stamp, convergence).await;
    if turn_converged {
        let _ = registry
            .settle_cancel(
                &claim.stamp.lease_id,
                claim.stamp.version,
                scope,
                &error_code,
            )
            .await;
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
    if probe.lease_is_live(&claim.stamp.lease_id).await {
        let _ = registry
            .settle_cancel(
                &claim.stamp.lease_id,
                claim.stamp.version,
                scope,
                &error_code,
            )
            .await;
    }

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
    }
}

async fn wait_lease_converged<P: ConvergenceProbe>(
    probe: &P,
    stamp: &LeaseStamp,
    budget: Duration,
) -> bool {
    let deadline = tokio::time::Instant::now() + budget;
    loop {
        if !probe.lease_is_live(&stamp.lease_id).await {
            return true;
        }
        // Terminal exit / capability ack is not enough while turn stays Prompting
        // and the Cancelling lease is still live — keep waiting until budget.
        let still_prompting = probe.turn_still_prompting(stamp).await;
        if !still_prompting && !probe.lease_is_live(&stamp.lease_id).await {
            return true;
        }
        if tokio::time::Instant::now() >= deadline {
            // Lease still live after budget → not converged, even if a terminal
            // process exit was observed while turn remained Prompting.
            return !probe.lease_is_live(&stamp.lease_id).await;
        }
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        let slice = remaining.min(Duration::from_millis(20));
        if slice.is_zero() {
            return !probe.lease_is_live(&stamp.lease_id).await;
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
        ) -> Pin<Box<dyn Future<Output = Result<(), SpecificCancelOutcome>> + Send + '_>> {
            self.delegation_calls.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { Ok(()) })
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
    async fn completion_before_claim_is_already_terminal() {
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
        assert_eq!(report.stage, EscalationStage::AlreadyTerminal);
        assert_eq!(report.error_code, ERROR_CODE_TOOL_STALLED_TIMEOUT);
        assert_eq!(host.turn_calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn claim_before_late_completion_settles_timeout() {
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

        // Escalate sees lease already terminal after settle.
        let host = ScriptedHost::new();
        let probe = RegistryProbe {
            registry: reg.clone(),
            force_prompting: Some(true),
        };
        let report = escalate_claimed_lease(
            &host,
            &probe,
            reg.as_ref(),
            &claim,
            Duration::from_millis(200),
        )
        .await;
        assert_eq!(report.stage, EscalationStage::AlreadyTerminal);
        assert_eq!(host.turn_calls.load(Ordering::SeqCst), 0);
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
            ) -> Pin<Box<dyn Future<Output = Result<(), SpecificCancelOutcome>> + Send + '_>>
            {
                self.inner.cancel_delegation_task(stamp, task_id)
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
        let probe = RegistryProbe {
            registry: reg.clone(),
            force_prompting: Some(true),
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
            force_prompting: Some(true),
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
            force_prompting: Some(true),
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
            force_prompting: Some(true),
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
            force_prompting: Some(true),
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
            force_prompting: Some(true),
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
    async fn convergence_constants_match_design() {
        use crate::acp::tool_watchdog::types::CANCEL_CONVERGENCE_SECS;
        assert_eq!(CANCEL_CONVERGENCE_SECS, 10);
        assert_eq!(CONTROL_LANE_ADMIT_TIMEOUT, Duration::from_millis(200));
        assert_eq!(TERMINAL_ADMIT_TIMEOUT, CONTROL_LANE_ADMIT_TIMEOUT);
        assert_eq!(TERMINAL_ACK_TIMEOUT, Duration::from_millis(200));
        assert_eq!(TERMINAL_KILL_EXECUTOR_TIMEOUT, Duration::from_secs(8));
    }
}
