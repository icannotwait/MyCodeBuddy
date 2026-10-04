//! Service-owned ACP admission.
//!
//! A roundtable turn is not a hidden-generation session and not an observer
//! window. The owned command permit is reserved before the state gate. Inside
//! the gate, one `try_write` rechecks that nothing is in flight, applies the
//! synchronous generation tail, and only then sends. Production does not exec
//! the member: the spawn gate stays closed unless a test failpoint installs
//! the in-memory lane.

use std::sync::{Arc, Mutex};

use roundtable_protocol::{AttemptId, Epoch, ErrorCode, RoomId, RtResult};
use tokio::sync::{mpsc, RwLock};

use crate::acp::agent_process::{plan_roundtable_process, spawn_roundtable_process};
use crate::acp::connection::{ConnectionCommand, LaneOwnedPermit, LaneSender};
use crate::acp::host_tools_policy::{
    roundtable_companion_groups, roundtable_hosts_fs, roundtable_hosts_terminal,
    roundtable_interactive_permission,
};
use crate::acp::manager::ConnectionManager;
use crate::acp::types::PromptInputBlock;
use crate::acp::SessionState;
use crate::auto_title::ConnectionPurpose;
use crate::roundtable::PreparedSandbox;

pub use crate::acp::host_tools_policy::InteractivePermission;
pub use crate::acp::manager::{ConnectionOwner, ROUNDTABLE_SERVICE_LABEL};

use super::rt_error;

/// Private bus for roundtable lifecycle. Ordinary `AcpEvent` capture is not
/// this sink: admission does not import a conversation row.
pub trait PrivateRuntimeSink: Send + Sync {
    fn attach(&self, connection_id: &str, owner: &ConnectionOwner);
}

/// What the service would launch, plus the already-prepared sandbox. The
/// sandbox is not spawned by this task.
pub struct RoundtableLaunch {
    pub room_id: RoomId,
    pub attempt_id: AttemptId,
    pub boot_epoch: Epoch,
    pub sandbox: PreparedSandbox,
    /// When set, install the in-memory lane and do not enter the spawn path.
    /// Absent from production builds, which always refuse to exec.
    #[cfg(any(test, feature = "test-utils"))]
    pub spawn_failpoint: bool,
}

/// Deny-by-default launch policy for one service-owned member.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoundtableLaunchPolicy {
    pub service_owned: bool,
    pub interactive_permissions: InteractivePermission,
    pub host_fs: bool,
    pub host_terminal: bool,
    pub companion_groups: Vec<String>,
    pub ordinary_conversation_import: bool,
    pub automatic_title: bool,
    pub hidden_generation: bool,
}

/// A connection the service owns. The command receiver stays here so the
/// lane is not closed before admission sends.
pub struct PreparedRoundtableConnection {
    pub connection_id: String,
    pub owner: ConnectionOwner,
    pub compatibility_label: String,
    pub policy: RoundtableLaunchPolicy,
    pub launch: crate::acp::agent_process::RoundtableProcessPlan,
    pub state: Arc<RwLock<SessionState>>,
    cmd_tx: LaneSender<ConnectionCommand>,
    /// Kept so the command lane stays open. Tests read it through
    /// [`Self::command_inbox`]; production has no other reader.
    #[allow(dead_code)]
    cmd_rx: Arc<Mutex<mpsc::Receiver<ConnectionCommand>>>,
    _control_rx: Mutex<mpsc::Receiver<crate::acp::connection::ConnectionControl>>,
}

/// Prompt text already accepted by the caller. The permit is not reserved yet.
pub struct AdmittedPrompt {
    pub blocks: Vec<PromptInputBlock>,
    /// Test observation between the generation write and `permit.send`.
    /// Must not lock `state`: the gate already holds it.
    #[cfg(any(test, feature = "test-utils"))]
    pub before_send: Option<Arc<dyn Fn(&SessionState) + Send + Sync>>,
}

/// Owned reservation plus the state the gate will recheck.
pub struct PreparedPrompt {
    pub owned_permit: LaneOwnedPermit<ConnectionCommand>,
    pub state: Arc<RwLock<SessionState>>,
    pub prompt: AdmittedPrompt,
}

/// Generation written by a successful enqueue. Matches `active_turn_generation`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TurnGeneration(pub u64);

/// The gate refused the turn. Neither variant changes generation or
/// `turn_in_flight`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueueReject {
    /// The queue is full, the state lock is busy, or a turn is already in flight.
    QueueRejected,
    /// `parent_turn_generation` cannot advance without wrapping.
    Overflow,
}

fn launch_policy() -> RoundtableLaunchPolicy {
    RoundtableLaunchPolicy {
        service_owned: true,
        interactive_permissions: roundtable_interactive_permission(),
        host_fs: roundtable_hosts_fs(),
        host_terminal: roundtable_hosts_terminal(),
        companion_groups: roundtable_companion_groups()
            .iter()
            .map(|group| (*group).to_string())
            .collect(),
        ordinary_conversation_import: false,
        automatic_title: false,
        hidden_generation: ConnectionPurpose::Roundtable.is_hidden_generation(),
    }
}

/// Install one service-owned lane, or refuse while the spawn gate is closed.
///
/// The failpoint is test-only. It skips `Command::spawn` and does not inherit
/// the host environment. There is no ordinary conversation import and no
/// mandatory delegation route.
pub async fn prepare_roundtable_connection(
    manager: &ConnectionManager,
    launch: RoundtableLaunch,
    sink: Arc<dyn PrivateRuntimeSink>,
) -> RtResult<PreparedRoundtableConnection> {
    let process = plan_roundtable_process(&launch.sandbox)?;
    let policy = launch_policy();
    #[cfg(any(test, feature = "test-utils"))]
    let install_without_exec = launch.spawn_failpoint;
    #[cfg(not(any(test, feature = "test-utils")))]
    let install_without_exec = false;
    if !install_without_exec {
        spawn_roundtable_process(&process)?;
        return Err(rt_error(
            ErrorCode::PolicyUnenforceable,
            "policy_unenforceable",
        ));
    }
    let owner = ConnectionOwner::Service {
        room_id: launch.room_id,
        attempt_id: launch.attempt_id,
        boot_epoch: launch.boot_epoch,
    };
    let lane = manager
        .register_roundtable_service_connection(owner.clone())
        .await
        .map_err(|_| rt_error(ErrorCode::PolicyUnenforceable, "policy_unenforceable"))?;
    sink.attach(&lane.connection_id, &owner);
    Ok(PreparedRoundtableConnection {
        connection_id: lane.connection_id,
        owner,
        compatibility_label: ROUNDTABLE_SERVICE_LABEL.to_string(),
        policy,
        launch: process,
        state: lane.state,
        cmd_tx: lane.cmd_tx,
        cmd_rx: Arc::new(Mutex::new(lane.cmd_rx)),
        _control_rx: Mutex::new(lane.control_rx),
    })
}

impl PreparedRoundtableConnection {
    /// Reserve the owned permit outside the state gate.
    pub fn prepare_prompt(&self, prompt: AdmittedPrompt) -> Result<PreparedPrompt, QueueReject> {
        if prompt.blocks.is_empty() {
            return Err(QueueReject::QueueRejected);
        }
        let owned_permit = self
            .cmd_tx
            .try_reserve_owned()
            .map_err(|_| QueueReject::QueueRejected)?;
        Ok(PreparedPrompt {
            owned_permit,
            state: Arc::clone(&self.state),
            prompt,
        })
    }

    /// Shared inbox so a test can see that `permit.send` has not happened yet.
    #[cfg(any(test, feature = "test-utils"))]
    pub fn command_inbox(&self) -> Arc<Mutex<mpsc::Receiver<ConnectionCommand>>> {
        Arc::clone(&self.cmd_rx)
    }
}

/// Recheck the lane under one non-awaited write guard, then send.
///
/// `try_write` failure, a turn already in flight, and a full queue (the
/// permit could not be reserved by [`PreparedRoundtableConnection::prepare_prompt`])
/// are [`QueueReject::QueueRejected`]. Overflow leaves the previous provider
/// id and generation untouched. Success increments `parent_turn_generation`,
/// sets `active_turn_generation` to that value and `turn_in_flight`, and
/// clears `active_provider_turn_id`. No conversation is captured.
pub fn try_enqueue(prepared: PreparedPrompt) -> Result<TurnGeneration, QueueReject> {
    let PreparedPrompt {
        owned_permit,
        state,
        prompt,
    } = prepared;
    if prompt.blocks.is_empty() {
        return Err(QueueReject::QueueRejected);
    }
    let mut guard = match state.try_write() {
        Ok(guard) => guard,
        Err(_) => return Err(QueueReject::QueueRejected),
    };
    if guard.turn_in_flight {
        return Err(QueueReject::QueueRejected);
    }
    let Some(turn_generation) = guard.parent_turn_generation.checked_add(1) else {
        return Err(QueueReject::Overflow);
    };
    guard.parent_turn_generation = turn_generation;
    guard.active_turn_generation = Some(turn_generation);
    guard.turn_in_flight = true;
    guard.active_provider_turn_id = None;
    #[cfg(any(test, feature = "test-utils"))]
    if let Some(hook) = &prompt.before_send {
        hook(&guard);
    }
    owned_permit.send(ConnectionCommand::Prompt {
        blocks: prompt.blocks,
        user_message: None,
        mark_awaiting_reply: false,
        bypass_autonomous_hold: false,
        turn_generation,
    });
    Ok(TurnGeneration(turn_generation))
}
