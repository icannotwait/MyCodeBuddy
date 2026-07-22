//! Atomic tool-execution lease registry and state machine.
//!
//! Deadlines are derived from recorded timestamps (`WatchdogInstant`), not from
//! scan cadence, so scan jitter cannot accumulate. Semantic progress is the only
//! progress clock; keepalive never renews leases here.

use std::collections::HashMap;
use std::time::Duration;

use chrono::{DateTime, Utc};
use tokio::time::Instant;
use uuid::Uuid;

use super::progress::{apply_semantic_progress, ProgressFingerprint};
use super::types::{
    CancellationCapability, CancellationScope, LeaseStamp, PauseReason, ToolCategory,
    ToolLeasePhase, ToolWatchdogPhase, ToolWatchdogProjection, ToolWatchdogSettings,
    UNTRACKED_WARNING_AFTER_SECS,
};

/// Fixed host discriminator for the untracked-turn fallback lease.
pub const FALLBACK_TOOL_CALL_ID: &str = "__untracked_turn__";

/// Injectable clock: monotonic for deadlines, wall for public timestamps.
#[derive(Debug, Clone, Copy)]
pub struct WatchdogInstant {
    pub mono: Instant,
    pub wall: DateTime<Utc>,
}

impl WatchdogInstant {
    pub fn now() -> Self {
        Self {
            mono: Instant::now(),
            wall: Utc::now(),
        }
    }

    pub fn advanced(self, secs: u64) -> Self {
        Self {
            mono: self.mono + Duration::from_secs(secs),
            wall: self.wall + chrono::Duration::seconds(secs as i64),
        }
    }

    pub fn wall_rfc3339(self) -> String {
        self.wall.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnStamp {
    pub connection_id: String,
    pub connection_incarnation: String,
    pub session_id: String,
    pub turn_generation: u64,
}

#[derive(Debug, Clone)]
pub struct RegisterTool {
    pub turn: TurnStamp,
    pub tool_call_id: String,
    pub category: ToolCategory,
    pub at: WatchdogInstant,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ToolLeaseKey {
    pub connection_id: String,
    pub connection_incarnation: String,
    pub turn_generation: u64,
    pub tool_call_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ToolProgressKey {
    pub connection_id: String,
    pub connection_incarnation: String,
    pub turn_generation: u64,
    pub tool_call_id: String,
}

impl From<&ToolLeaseKey> for ToolProgressKey {
    fn from(key: &ToolLeaseKey) -> Self {
        Self {
            connection_id: key.connection_id.clone(),
            connection_incarnation: key.connection_incarnation.clone(),
            turn_generation: key.turn_generation,
            tool_call_id: key.tool_call_id.clone(),
        }
    }
}

/// Bounded semantic progress facts only (no output text).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SemanticProgress {
    TerminalOffset { next_offset: u64 },
    TerminalExit,
    ToolStatusChanged { status_fingerprint: u64 },
    McpProgress { token_or_hash: u64 },
    DelegationActivity { at_mono_ms: u64 },
    /// Untracked fallback only unless the caller associates it with a tool key.
    AgentActivity { content_hash: u64 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegistryAction {
    PublishWarning {
        stamp: LeaseStamp,
        projection: ToolWatchdogProjection,
    },
    EnterGrace {
        stamp: LeaseStamp,
        projection: ToolWatchdogProjection,
    },
    ClaimCancel {
        claim: CancellationClaim,
    },
    EmitCleared {
        projection: ToolWatchdogProjection,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CancellationClaim {
    pub stamp: LeaseStamp,
    pub capability: CancellationCapability,
    pub cause: CancelCause,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CancelCause {
    AutoTimeout,
    UserStop,
}

#[derive(Debug, thiserror::Error)]
#[error("stale_tool_watchdog_lease")]
pub struct StaleLease;

/// Single source of truth for untracked-turn fallback eligibility.
pub fn fallback_eligible(
    is_prompting: bool,
    has_tracked_lease: bool,
    pending_permission: bool,
    pending_user_input: bool,
    verified_background_work: bool,
) -> bool {
    is_prompting
        && !has_tracked_lease
        && !pending_permission
        && !pending_user_input
        && !verified_background_work
}

pub struct ToolExecutionLeaseRegistry {
    inner: tokio::sync::Mutex<RegistryInner>,
}

struct RegistryInner {
    settings: ToolWatchdogSettings,
    leases: HashMap<String, LeaseRecord>,
    tool_index: HashMap<ToolLeaseKey, String>,
    turns: HashMap<TurnKey, TurnRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct TurnKey {
    connection_id: String,
    connection_incarnation: String,
    turn_generation: u64,
}

impl TurnKey {
    fn from_turn(turn: &TurnStamp) -> Self {
        Self {
            connection_id: turn.connection_id.clone(),
            connection_incarnation: turn.connection_incarnation.clone(),
            turn_generation: turn.turn_generation,
        }
    }

    fn from_tool_key(key: &ToolLeaseKey) -> Self {
        Self {
            connection_id: key.connection_id.clone(),
            connection_incarnation: key.connection_incarnation.clone(),
            turn_generation: key.turn_generation,
        }
    }
}

struct TurnRecord {
    turn: TurnStamp,
    turn_start_at: WatchdogInstant,
    last_verified_agent_activity_at: Option<WatchdogInstant>,
    is_prompting: bool,
    pending_permission: bool,
    pending_user_input: bool,
    verified_background_work: bool,
    fallback_lease_id: Option<String>,
}

struct LeaseRecord {
    lease_id: String,
    version: u64,
    connection_id: String,
    connection_incarnation: String,
    /// Session association for Task 5 cancel routing (retained host-side).
    #[allow(dead_code)]
    session_id: String,
    turn_generation: u64,
    tool_call_id: Option<String>,
    category: ToolCategory,
    is_fallback: bool,
    phase: ToolLeasePhase,
    last_progress_at: WatchdogInstant,
    warning_emitted_at: Option<WatchdogInstant>,
    grace_deadline: Option<WatchdogInstant>,
    captured_grace_secs: Option<u32>,
    capability: CancellationCapability,
    fingerprint: ProgressFingerprint,
    late_activity: u32,
    cancel_cause: Option<CancelCause>,
}

impl LeaseRecord {
    fn stamp(&self) -> LeaseStamp {
        LeaseStamp {
            lease_id: self.lease_id.clone(),
            version: self.version,
            connection_id: self.connection_id.clone(),
            connection_incarnation: self.connection_incarnation.clone(),
            turn_generation: self.turn_generation,
            tool_call_id: self.tool_call_id.clone(),
        }
    }

    fn bump(&mut self) {
        self.version = self.version.saturating_add(1);
    }

    fn cancellation_scope(&self) -> CancellationScope {
        match &self.capability {
            CancellationCapability::Terminal { .. } => CancellationScope::Terminal,
            CancellationCapability::Delegation { .. } => CancellationScope::Delegation,
            CancellationCapability::DelegationWait { .. } => CancellationScope::DelegationWait,
            CancellationCapability::McpRequest { .. } => CancellationScope::McpRequest,
            CancellationCapability::Turn => CancellationScope::Turn,
        }
    }

    fn to_projection(&self, phase: ToolWatchdogPhase) -> ToolWatchdogProjection {
        let cancellation_scope = match phase {
            ToolWatchdogPhase::Cancelling
            | ToolWatchdogPhase::TimedOut
            | ToolWatchdogPhase::Grace
            | ToolWatchdogPhase::Warning => Some(self.cancellation_scope()),
            ToolWatchdogPhase::Cleared => None,
        };
        ToolWatchdogProjection {
            lease_id: self.lease_id.clone(),
            version: self.version,
            tool_title: self.category,
            phase,
            last_progress_at: self.last_progress_at.wall_rfc3339(),
            grace_deadline: self.grace_deadline.map(|g| g.wall_rfc3339()),
            cancellation_scope,
            error_code: None,
        }
    }

    fn clear_warning_fields(&mut self) {
        self.warning_emitted_at = None;
        self.grace_deadline = None;
        self.captured_grace_secs = None;
    }

    fn is_live_active(&self) -> bool {
        matches!(
            self.phase,
            ToolLeasePhase::Running
                | ToolLeasePhase::Paused { .. }
                | ToolLeasePhase::Warning
                | ToolLeasePhase::Grace
        )
    }

    fn is_actionable_public(&self) -> bool {
        matches!(
            self.phase,
            ToolLeasePhase::Warning | ToolLeasePhase::Grace | ToolLeasePhase::Cancelling
        )
    }

    fn public_phase(&self) -> Option<ToolWatchdogPhase> {
        match self.phase {
            ToolLeasePhase::Warning => Some(ToolWatchdogPhase::Warning),
            ToolLeasePhase::Grace => Some(ToolWatchdogPhase::Grace),
            ToolLeasePhase::Cancelling => Some(ToolWatchdogPhase::Cancelling),
            ToolLeasePhase::TimedOut => Some(ToolWatchdogPhase::TimedOut),
            ToolLeasePhase::Completed => Some(ToolWatchdogPhase::Cleared),
            ToolLeasePhase::Running | ToolLeasePhase::Paused { .. } => None,
        }
    }
}

impl ToolExecutionLeaseRegistry {
    pub fn new(settings: ToolWatchdogSettings) -> Self {
        Self {
            inner: tokio::sync::Mutex::new(RegistryInner {
                settings: settings.clamp(),
                leases: HashMap::new(),
                tool_index: HashMap::new(),
                turns: HashMap::new(),
            }),
        }
    }

    pub async fn apply_settings(&self, settings: ToolWatchdogSettings) {
        let mut inner = self.inner.lock().await;
        let next = settings.clamp();
        let was_enabled = inner.settings.enabled;
        inner.settings = next;
        if was_enabled && !inner.settings.enabled {
            // Disable clears warning/grace without inventing progress.
            let mut to_clear: Vec<String> = Vec::new();
            for (id, lease) in inner.leases.iter() {
                if matches!(
                    lease.phase,
                    ToolLeasePhase::Warning | ToolLeasePhase::Grace
                ) {
                    to_clear.push(id.clone());
                }
            }
            for id in to_clear {
                if let Some(lease) = inner.leases.get_mut(&id) {
                    lease.phase = ToolLeasePhase::Running;
                    lease.clear_warning_fields();
                    lease.bump();
                }
            }
        }
    }

    pub async fn start_turn(&self, turn: TurnStamp, at: WatchdogInstant) {
        let mut inner = self.inner.lock().await;
        let key = TurnKey::from_turn(&turn);
        let record = TurnRecord {
            turn: turn.clone(),
            turn_start_at: at,
            last_verified_agent_activity_at: None,
            is_prompting: true,
            pending_permission: false,
            pending_user_input: false,
            verified_background_work: false,
            fallback_lease_id: None,
        };
        inner.turns.insert(key.clone(), record);
        inner.maybe_register_fallback(&key, at);
    }

    /// Retires fallback while tracked leases exist; re-arms when last tracked ends.
    pub async fn register_tool(&self, input: RegisterTool) -> LeaseStamp {
        let mut inner = self.inner.lock().await;
        let turn_key = TurnKey::from_turn(&input.turn);
        // Ensure turn exists (prompt admission may race).
        inner.turns.entry(turn_key.clone()).or_insert_with(|| TurnRecord {
            turn: input.turn.clone(),
            turn_start_at: input.at,
            last_verified_agent_activity_at: None,
            is_prompting: true,
            pending_permission: false,
            pending_user_input: false,
            verified_background_work: false,
            fallback_lease_id: None,
        });

        // Retire fallback while any tracked lease exists.
        inner.retire_fallback(&turn_key);

        let tool_key = ToolLeaseKey {
            connection_id: input.turn.connection_id.clone(),
            connection_incarnation: input.turn.connection_incarnation.clone(),
            turn_generation: input.turn.turn_generation,
            tool_call_id: input.tool_call_id.clone(),
        };

        if let Some(existing_id) = inner.tool_index.get(&tool_key).cloned() {
            if let Some(lease) = inner.leases.get(&existing_id) {
                return lease.stamp();
            }
        }

        let lease_id = Uuid::new_v4().to_string();
        let lease = LeaseRecord {
            lease_id: lease_id.clone(),
            version: 1,
            connection_id: input.turn.connection_id.clone(),
            connection_incarnation: input.turn.connection_incarnation.clone(),
            session_id: input.turn.session_id.clone(),
            turn_generation: input.turn.turn_generation,
            tool_call_id: Some(input.tool_call_id.clone()),
            category: input.category,
            is_fallback: false,
            phase: ToolLeasePhase::Running,
            last_progress_at: input.at,
            warning_emitted_at: None,
            grace_deadline: None,
            captured_grace_secs: None,
            capability: CancellationCapability::Turn,
            fingerprint: ProgressFingerprint::default(),
            late_activity: 0,
            cancel_cause: None,
        };
        let stamp = lease.stamp();
        inner.leases.insert(lease_id.clone(), lease);
        inner.tool_index.insert(tool_key, lease_id);
        stamp
    }

    pub async fn bind_capability(
        &self,
        stamp: &LeaseStamp,
        capability: CancellationCapability,
    ) -> Result<LeaseStamp, StaleLease> {
        let mut inner = self.inner.lock().await;
        let lease = inner.leases.get_mut(&stamp.lease_id).ok_or(StaleLease)?;
        if lease.version != stamp.version
            || lease.connection_id != stamp.connection_id
            || lease.connection_incarnation != stamp.connection_incarnation
            || lease.turn_generation != stamp.turn_generation
        {
            return Err(StaleLease);
        }
        if !matches!(lease.phase, ToolLeasePhase::Running) {
            return Err(StaleLease);
        }
        lease.capability = capability;
        lease.bump();
        Ok(lease.stamp())
    }

    /// Record tool-associated semantic progress using the host wall/mono clock.
    ///
    /// Prefer [`Self::record_tool_progress_at`] when a controlled clock is required
    /// (tests and injected host time).
    pub async fn record_tool_progress(
        &self,
        key: ToolProgressKey,
        fact: SemanticProgress,
    ) -> Option<LeaseStamp> {
        self.record_tool_progress_at(key, fact, WatchdogInstant::now())
            .await
    }

    /// Controlled-clock progress path used by host wiring and tests.
    pub async fn record_tool_progress_at(
        &self,
        key: ToolProgressKey,
        fact: SemanticProgress,
        at: WatchdogInstant,
    ) -> Option<LeaseStamp> {
        let mut inner = self.inner.lock().await;
        inner.record_tool_progress_at(key, fact, at)
    }

    pub async fn record_turn_progress(&self, turn: &TurnStamp, fact: SemanticProgress) {
        self.record_turn_progress_at(turn, fact, WatchdogInstant::now())
            .await;
    }

    pub async fn record_turn_progress_at(
        &self,
        turn: &TurnStamp,
        fact: SemanticProgress,
        at: WatchdogInstant,
    ) {
        let mut inner = self.inner.lock().await;
        let turn_key = TurnKey::from_turn(turn);
        let Some(turn_rec) = inner.turns.get_mut(&turn_key) else {
            return;
        };
        // Generic agent transcript activity renews only the untracked fallback.
        if !matches!(fact, SemanticProgress::AgentActivity { .. }) {
            return;
        }
        turn_rec.last_verified_agent_activity_at = Some(at);
        let fallback_id = turn_rec.fallback_lease_id.clone();
        let Some(lease_id) = fallback_id else {
            return;
        };
        let Some(lease) = inner.leases.get_mut(&lease_id) else {
            return;
        };
        if matches!(lease.phase, ToolLeasePhase::Cancelling) {
            lease.late_activity = lease.late_activity.saturating_add(1);
            return;
        }
        if matches!(lease.phase, ToolLeasePhase::Paused { .. }) {
            let _ = apply_semantic_progress(&mut lease.fingerprint, &fact);
            return;
        }
        if !apply_semantic_progress(&mut lease.fingerprint, &fact) {
            return;
        }
        renew_lease_to_running(lease, at);
    }

    pub async fn pause_turn(&self, turn: &TurnStamp, reason: PauseReason) {
        let mut inner = self.inner.lock().await;
        let turn_key = TurnKey::from_turn(turn);
        if let Some(rec) = inner.turns.get_mut(&turn_key) {
            match reason {
                PauseReason::Permission => rec.pending_permission = true,
                PauseReason::AgentQuestion
                | PauseReason::UserInput
                | PauseReason::DelegationWaitingInput => {
                    rec.pending_user_input = true;
                }
            }
        }
        for lease in inner.leases.values_mut() {
            if lease.connection_id != turn.connection_id
                || lease.connection_incarnation != turn.connection_incarnation
                || lease.turn_generation != turn.turn_generation
            {
                continue;
            }
            if matches!(
                lease.phase,
                ToolLeasePhase::Running | ToolLeasePhase::Warning | ToolLeasePhase::Grace
            ) {
                lease.phase = ToolLeasePhase::Paused {
                    reason: reason.clone(),
                };
                lease.clear_warning_fields();
                lease.bump();
            }
        }
        // Pending input retires fallback eligibility (not re-armed while paused input).
        if let Some(rec) = inner.turns.get(&turn_key) {
            if rec.fallback_lease_id.is_some()
                && !inner.turn_is_fallback_eligible(&turn_key)
            {
                // Keep fallback lease paused rather than removed: still no tracked
                // tool, but eligibility false. Pause already applied above.
            }
        }
    }

    pub async fn resume_turn(&self, turn: &TurnStamp, at: WatchdogInstant) {
        let mut inner = self.inner.lock().await;
        let turn_key = TurnKey::from_turn(turn);
        if let Some(rec) = inner.turns.get_mut(&turn_key) {
            rec.pending_permission = false;
            rec.pending_user_input = false;
        }
        for lease in inner.leases.values_mut() {
            if lease.connection_id != turn.connection_id
                || lease.connection_incarnation != turn.connection_incarnation
                || lease.turn_generation != turn.turn_generation
            {
                continue;
            }
            if matches!(lease.phase, ToolLeasePhase::Paused { .. }) {
                lease.phase = ToolLeasePhase::Running;
                lease.last_progress_at = at;
                lease.clear_warning_fields();
                lease.bump();
            }
        }
        inner.maybe_register_fallback(&turn_key, at);
    }

    pub async fn complete_tool(&self, key: &ToolLeaseKey) -> Option<ToolWatchdogProjection> {
        let mut inner = self.inner.lock().await;
        let lease_id = inner.tool_index.get(key)?.clone();
        let lease = inner.leases.get_mut(&lease_id)?;
        if matches!(lease.phase, ToolLeasePhase::Cancelling) {
            lease.late_activity = lease.late_activity.saturating_add(1);
            return None;
        }
        if !lease.is_live_active() {
            return None;
        }
        lease.phase = ToolLeasePhase::Completed;
        lease.bump();
        let projection = lease.to_projection(ToolWatchdogPhase::Cleared);
        // Remove from live map.
        inner.leases.remove(&lease_id);
        inner.tool_index.remove(key);

        let turn_key = TurnKey::from_tool_key(key);
        // Re-arm fallback only if still eligible.
        if let Some(turn_rec) = inner.turns.get(&turn_key) {
            let rearm_at = max_progress_baseline(
                turn_rec.turn_start_at,
                turn_rec.last_verified_agent_activity_at,
            );
            if inner.turn_is_fallback_eligible(&turn_key) {
                inner.maybe_register_fallback(&turn_key, rearm_at);
            }
        }
        Some(projection)
    }

    /// Mark whether verified background work accounts for the turn (Task 5 handoff).
    pub async fn set_verified_background_work(&self, turn: &TurnStamp, active: bool) {
        let mut inner = self.inner.lock().await;
        let turn_key = TurnKey::from_turn(turn);
        if let Some(rec) = inner.turns.get_mut(&turn_key) {
            rec.verified_background_work = active;
        }
        if active {
            // Background accounts for turn: retire fallback if present.
            inner.retire_fallback(&turn_key);
        } else {
            let at = inner
                .turns
                .get(&turn_key)
                .map(|t| {
                    max_progress_baseline(t.turn_start_at, t.last_verified_agent_activity_at)
                })
                .unwrap_or_else(WatchdogInstant::now);
            inner.maybe_register_fallback(&turn_key, at);
        }
    }

    pub async fn complete_turn(&self, turn: &TurnStamp) -> Vec<ToolWatchdogProjection> {
        let mut inner = self.inner.lock().await;
        let turn_key = TurnKey::from_turn(turn);
        let mut cleared = Vec::new();
        let ids: Vec<String> = inner
            .leases
            .values()
            .filter(|l| {
                l.connection_id == turn.connection_id
                    && l.connection_incarnation == turn.connection_incarnation
                    && l.turn_generation == turn.turn_generation
            })
            .map(|l| l.lease_id.clone())
            .collect();
        for id in ids {
            if let Some(mut lease) = inner.leases.remove(&id) {
                if let Some(tool_id) = lease.tool_call_id.clone() {
                    inner.tool_index.remove(&ToolLeaseKey {
                        connection_id: lease.connection_id.clone(),
                        connection_incarnation: lease.connection_incarnation.clone(),
                        turn_generation: lease.turn_generation,
                        tool_call_id: tool_id,
                    });
                }
                if lease.is_live_active() || matches!(lease.phase, ToolLeasePhase::Cancelling)
                {
                    lease.phase = ToolLeasePhase::Completed;
                    lease.bump();
                    cleared.push(lease.to_projection(ToolWatchdogPhase::Cleared));
                }
            }
        }
        if let Some(rec) = inner.turns.get_mut(&turn_key) {
            rec.is_prompting = false;
            rec.fallback_lease_id = None;
            rec.pending_permission = false;
            rec.pending_user_input = false;
            rec.verified_background_work = false;
        }
        cleared
    }

    /// Emits `PublishWarning` for overdue Running leases and `ClaimCancel` for
    /// Grace leases past `grace_deadline`. Never both for the same lease in one scan.
    pub async fn scan(&self, at: WatchdogInstant) -> Vec<RegistryAction> {
        let mut inner = self.inner.lock().await;
        if !inner.settings.enabled {
            return Vec::new();
        }
        let mut actions = Vec::new();
        let lease_ids: Vec<String> = inner.leases.keys().cloned().collect();
        for id in lease_ids {
            let Some(lease) = inner.leases.get(&id) else {
                continue;
            };
            // Skip paused — no warn/cancel while paused.
            if matches!(lease.phase, ToolLeasePhase::Paused { .. }) {
                continue;
            }

            if matches!(lease.phase, ToolLeasePhase::Running) {
                let threshold = if lease.is_fallback {
                    UNTRACKED_WARNING_AFTER_SECS
                } else {
                    inner.settings.warning_after_seconds
                };
                let elapsed = at
                    .mono
                    .saturating_duration_since(lease.last_progress_at.mono);
                if elapsed >= Duration::from_secs(threshold as u64) {
                    let lease = inner.leases.get_mut(&id).expect("lease present");
                    lease.phase = ToolLeasePhase::Warning;
                    lease.bump();
                    let stamp = lease.stamp();
                    let projection = lease.to_projection(ToolWatchdogPhase::Warning);
                    actions.push(RegistryAction::PublishWarning { stamp, projection });
                }
                continue;
            }

            if matches!(lease.phase, ToolLeasePhase::Grace) {
                let Some(deadline) = lease.grace_deadline else {
                    continue;
                };
                if at.mono >= deadline.mono {
                    let lease = inner.leases.get_mut(&id).expect("lease present");
                    lease.phase = ToolLeasePhase::Cancelling;
                    lease.cancel_cause = Some(CancelCause::AutoTimeout);
                    lease.bump();
                    let claim = CancellationClaim {
                        stamp: lease.stamp(),
                        capability: lease.capability.clone(),
                        cause: CancelCause::AutoTimeout,
                    };
                    actions.push(RegistryAction::ClaimCancel { claim });
                }
            }
        }
        actions
    }

    /// After warning publish, transitions Warning → Grace with captured grace.
    pub async fn warning_published(
        &self,
        lease_id: &str,
        version: u64,
        at: WatchdogInstant,
    ) -> Result<ToolWatchdogProjection, StaleLease> {
        let mut inner = self.inner.lock().await;
        let grace_secs = inner.settings.grace_seconds;
        let lease = inner.leases.get_mut(lease_id).ok_or(StaleLease)?;
        if lease.version != version {
            return Err(StaleLease);
        }
        if !matches!(lease.phase, ToolLeasePhase::Warning) {
            return Err(StaleLease);
        }
        lease.phase = ToolLeasePhase::Grace;
        lease.warning_emitted_at = Some(at);
        lease.captured_grace_secs = Some(grace_secs);
        lease.grace_deadline = Some(at.advanced(grace_secs as u64));
        lease.bump();
        Ok(lease.to_projection(ToolWatchdogPhase::Grace))
    }

    pub async fn extend(
        &self,
        lease_id: &str,
        version: u64,
        at: WatchdogInstant,
    ) -> Result<ToolWatchdogProjection, StaleLease> {
        let mut inner = self.inner.lock().await;
        let settings_grace = inner.settings.grace_seconds;
        let lease = inner.leases.get_mut(lease_id).ok_or(StaleLease)?;
        if lease.version != version {
            return Err(StaleLease);
        }
        if !matches!(lease.phase, ToolLeasePhase::Grace) {
            return Err(StaleLease);
        }
        let grace_secs = lease.captured_grace_secs.unwrap_or(settings_grace);
        // Extension does not update last_progress_at.
        lease.grace_deadline = Some(at.advanced(grace_secs as u64));
        lease.bump();
        Ok(lease.to_projection(ToolWatchdogPhase::Grace))
    }

    pub async fn claim_cancel(
        &self,
        lease_id: &str,
        version: u64,
        cause: CancelCause,
    ) -> Result<CancellationClaim, StaleLease> {
        let mut inner = self.inner.lock().await;
        let lease = inner.leases.get_mut(lease_id).ok_or(StaleLease)?;
        if lease.version != version {
            return Err(StaleLease);
        }
        if !matches!(
            lease.phase,
            ToolLeasePhase::Running | ToolLeasePhase::Warning | ToolLeasePhase::Grace
        ) {
            return Err(StaleLease);
        }
        lease.phase = ToolLeasePhase::Cancelling;
        lease.cancel_cause = Some(cause);
        lease.bump();
        Ok(CancellationClaim {
            stamp: lease.stamp(),
            capability: lease.capability.clone(),
            cause,
        })
    }

    pub async fn remove_connection(
        &self,
        connection_id: &str,
        incarnation: &str,
    ) -> Vec<ToolWatchdogProjection> {
        let mut inner = self.inner.lock().await;
        let mut cleared = Vec::new();
        let ids: Vec<String> = inner
            .leases
            .values()
            .filter(|l| {
                l.connection_id == connection_id && l.connection_incarnation == incarnation
            })
            .map(|l| l.lease_id.clone())
            .collect();
        for id in ids {
            if let Some(mut lease) = inner.leases.remove(&id) {
                if let Some(tool_id) = lease.tool_call_id.clone() {
                    inner.tool_index.remove(&ToolLeaseKey {
                        connection_id: lease.connection_id.clone(),
                        connection_incarnation: lease.connection_incarnation.clone(),
                        turn_generation: lease.turn_generation,
                        tool_call_id: tool_id,
                    });
                }
                lease.phase = ToolLeasePhase::Completed;
                lease.bump();
                cleared.push(lease.to_projection(ToolWatchdogPhase::Cleared));
            }
        }
        let turn_keys: Vec<TurnKey> = inner
            .turns
            .keys()
            .filter(|k| {
                k.connection_id == connection_id && k.connection_incarnation == incarnation
            })
            .cloned()
            .collect();
        for k in turn_keys {
            inner.turns.remove(&k);
        }
        cleared
    }

    pub async fn actionable_projections(&self) -> Vec<ToolWatchdogProjection> {
        let inner = self.inner.lock().await;
        let mut out = Vec::new();
        for lease in inner.leases.values() {
            if lease.is_actionable_public() {
                if let Some(phase) = lease.public_phase() {
                    out.push(lease.to_projection(phase));
                }
            }
        }
        out
    }

    /// Test/host helper: late_activity counter after Cancelling claim.
    pub async fn late_activity(&self, lease_id: &str) -> Option<u32> {
        let inner = self.inner.lock().await;
        inner.leases.get(lease_id).map(|l| l.late_activity)
    }

    pub async fn lease_phase(&self, lease_id: &str) -> Option<ToolLeasePhase> {
        let inner = self.inner.lock().await;
        inner.leases.get(lease_id).map(|l| l.phase.clone())
    }

    pub async fn has_fallback(&self, turn: &TurnStamp) -> bool {
        let inner = self.inner.lock().await;
        let key = TurnKey::from_turn(turn);
        inner
            .turns
            .get(&key)
            .and_then(|t| t.fallback_lease_id.as_ref())
            .is_some()
    }

    pub async fn fallback_stamp(&self, turn: &TurnStamp) -> Option<LeaseStamp> {
        let inner = self.inner.lock().await;
        let key = TurnKey::from_turn(turn);
        let id = inner.turns.get(&key)?.fallback_lease_id.as_ref()?;
        inner.leases.get(id).map(|l| l.stamp())
    }
}

fn renew_lease_to_running(lease: &mut LeaseRecord, at: WatchdogInstant) {
    lease.phase = ToolLeasePhase::Running;
    lease.last_progress_at = at;
    lease.clear_warning_fields();
    lease.bump();
}

fn max_progress_baseline(
    turn_start: WatchdogInstant,
    last_agent: Option<WatchdogInstant>,
) -> WatchdogInstant {
    match last_agent {
        Some(agent) if agent.mono >= turn_start.mono => agent,
        _ => turn_start,
    }
}

impl RegistryInner {
    fn turn_is_fallback_eligible(&self, turn_key: &TurnKey) -> bool {
        let Some(turn) = self.turns.get(turn_key) else {
            return false;
        };
        let has_tracked = self.leases.values().any(|l| {
            !l.is_fallback
                && l.connection_id == turn_key.connection_id
                && l.connection_incarnation == turn_key.connection_incarnation
                && l.turn_generation == turn_key.turn_generation
                && l.is_live_active()
        });
        fallback_eligible(
            turn.is_prompting,
            has_tracked,
            turn.pending_permission,
            turn.pending_user_input,
            turn.verified_background_work,
        )
    }

    fn retire_fallback(&mut self, turn_key: &TurnKey) {
        let Some(turn) = self.turns.get_mut(turn_key) else {
            return;
        };
        if let Some(id) = turn.fallback_lease_id.take() {
            self.leases.remove(&id);
        }
    }

    fn maybe_register_fallback(&mut self, turn_key: &TurnKey, at: WatchdogInstant) {
        if !self.turn_is_fallback_eligible(turn_key) {
            return;
        }
        if let Some(turn) = self.turns.get(turn_key) {
            if turn.fallback_lease_id.is_some() {
                return;
            }
        }
        let Some(turn) = self.turns.get(turn_key) else {
            return;
        };
        let turn_stamp = turn.turn.clone();
        let lease_id = Uuid::new_v4().to_string();
        let lease = LeaseRecord {
            lease_id: lease_id.clone(),
            version: 1,
            connection_id: turn_stamp.connection_id.clone(),
            connection_incarnation: turn_stamp.connection_incarnation.clone(),
            session_id: turn_stamp.session_id.clone(),
            turn_generation: turn_stamp.turn_generation,
            tool_call_id: Some(FALLBACK_TOOL_CALL_ID.to_string()),
            category: ToolCategory::Other,
            is_fallback: true,
            phase: ToolLeasePhase::Running,
            last_progress_at: at,
            warning_emitted_at: None,
            grace_deadline: None,
            captured_grace_secs: None,
            capability: CancellationCapability::Turn,
            fingerprint: ProgressFingerprint::default(),
            late_activity: 0,
            cancel_cause: None,
        };
        self.leases.insert(lease_id.clone(), lease);
        if let Some(turn) = self.turns.get_mut(turn_key) {
            turn.fallback_lease_id = Some(lease_id);
        }
    }

    fn record_tool_progress_at(
        &mut self,
        key: ToolProgressKey,
        fact: SemanticProgress,
        at: WatchdogInstant,
    ) -> Option<LeaseStamp> {
        let tool_key = ToolLeaseKey {
            connection_id: key.connection_id.clone(),
            connection_incarnation: key.connection_incarnation.clone(),
            turn_generation: key.turn_generation,
            tool_call_id: key.tool_call_id.clone(),
        };
        let lease_id = self.tool_index.get(&tool_key)?.clone();
        let lease = self.leases.get_mut(&lease_id)?;

        if matches!(lease.phase, ToolLeasePhase::Cancelling) {
            lease.late_activity = lease.late_activity.saturating_add(1);
            return None;
        }
        if matches!(
            lease.phase,
            ToolLeasePhase::TimedOut | ToolLeasePhase::Completed
        ) {
            return None;
        }
        // Paused leases still accept progress fingerprints but do not leave pause
        // via progress — only resume_turn does. Design: progress in warning/grace
        // returns to running. Progress while paused: retain pause.
        if matches!(lease.phase, ToolLeasePhase::Paused { .. }) {
            if apply_semantic_progress(&mut lease.fingerprint, &fact) {
                // Update fingerprint only; do not change phase or last_progress.
            }
            return None;
        }

        if !apply_semantic_progress(&mut lease.fingerprint, &fact) {
            return None;
        }

        renew_lease_to_running(lease, at);
        Some(lease.stamp())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clock_base() -> WatchdogInstant {
        WatchdogInstant {
            mono: Instant::now(),
            wall: DateTime::parse_from_rfc3339("2026-07-22T12:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
        }
    }

    fn sample_turn() -> TurnStamp {
        TurnStamp {
            connection_id: "conn-1".into(),
            connection_incarnation: "inc-1".into(),
            session_id: "sess-1".into(),
            turn_generation: 1,
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

    async fn register_running_tool(
        reg: &ToolExecutionLeaseRegistry,
        turn: &TurnStamp,
        tool_id: &str,
        at: WatchdogInstant,
    ) -> LeaseStamp {
        reg.register_tool(RegisterTool {
            turn: turn.clone(),
            tool_call_id: tool_id.into(),
            category: ToolCategory::Terminal,
            at,
        })
        .await
    }

    #[test]
    fn fallback_eligible_predicate() {
        assert!(fallback_eligible(true, false, false, false, false));
        assert!(!fallback_eligible(false, false, false, false, false));
        assert!(!fallback_eligible(true, true, false, false, false));
        assert!(!fallback_eligible(true, false, true, false, false));
        assert!(!fallback_eligible(true, false, false, true, false));
        assert!(!fallback_eligible(true, false, false, false, true));
    }

    #[tokio::test]
    async fn running_599s_no_warning_600s_warning_only() {
        let reg = ToolExecutionLeaseRegistry::new(ToolWatchdogSettings::default());
        let turn = sample_turn();
        let t0 = clock_base();
        reg.start_turn(turn.clone(), t0).await;
        let stamp = register_running_tool(&reg, &turn, "tool-1", t0).await;

        let actions_599 = reg.scan(t0.advanced(599)).await;
        assert!(
            actions_599.is_empty(),
            "expected no warning at 599s, got {actions_599:?}"
        );

        let actions_600 = reg.scan(t0.advanced(600)).await;
        assert_eq!(actions_600.len(), 1);
        match &actions_600[0] {
            RegistryAction::PublishWarning {
                stamp: wstamp,
                projection,
            } => {
                assert_eq!(wstamp.lease_id, stamp.lease_id);
                assert_eq!(projection.phase, ToolWatchdogPhase::Warning);
                assert_eq!(projection.tool_title, ToolCategory::Terminal);
                // No cancel on same pass.
            }
            other => panic!("expected PublishWarning, got {other:?}"),
        }
        // Same lease cannot also claim cancel in this scan.
        assert!(!actions_600
            .iter()
            .any(|a| matches!(a, RegistryAction::ClaimCancel { .. })));
    }

    #[tokio::test]
    async fn warning_publication_starts_new_600s_grace() {
        let reg = ToolExecutionLeaseRegistry::new(ToolWatchdogSettings::default());
        let turn = sample_turn();
        let t0 = clock_base();
        reg.start_turn(turn.clone(), t0).await;
        let stamp = register_running_tool(&reg, &turn, "tool-1", t0).await;

        let actions = reg.scan(t0.advanced(600)).await;
        let RegistryAction::PublishWarning {
            stamp: wstamp,
            ..
        } = &actions[0]
        else {
            panic!("expected warning");
        };
        assert_eq!(wstamp.lease_id, stamp.lease_id);

        let warn_at = t0.advanced(600);
        let grace_proj = reg
            .warning_published(&wstamp.lease_id, wstamp.version, warn_at)
            .await
            .expect("enter grace");
        assert_eq!(grace_proj.phase, ToolWatchdogPhase::Grace);
        assert_eq!(
            grace_proj.grace_deadline.as_deref(),
            Some(warn_at.advanced(600).wall_rfc3339().as_str())
        );

        // Still within grace at warn_at + 599: no cancel.
        let mid = reg.scan(warn_at.advanced(599)).await;
        assert!(mid.is_empty(), "no cancel before grace end: {mid:?}");

        // Past grace deadline: ClaimCancel (separate pass from first warning).
        let end = reg.scan(warn_at.advanced(600)).await;
        assert_eq!(end.len(), 1);
        match &end[0] {
            RegistryAction::ClaimCancel { claim } => {
                assert_eq!(claim.cause, CancelCause::AutoTimeout);
                assert_eq!(claim.stamp.lease_id, stamp.lease_id);
            }
            other => panic!("expected ClaimCancel, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn extension_changes_version_and_deadline_not_last_progress() {
        let reg = ToolExecutionLeaseRegistry::new(ToolWatchdogSettings::default());
        let turn = sample_turn();
        let t0 = clock_base();
        reg.start_turn(turn.clone(), t0).await;
        let stamp = register_running_tool(&reg, &turn, "tool-1", t0).await;

        let actions = reg.scan(t0.advanced(600)).await;
        let RegistryAction::PublishWarning { stamp: wstamp, .. } = &actions[0] else {
            panic!("warn");
        };
        let warn_at = t0.advanced(600);
        let grace = reg
            .warning_published(&wstamp.lease_id, wstamp.version, warn_at)
            .await
            .unwrap();
        let last_progress_before = grace.last_progress_at.clone();
        let version_before = grace.version;

        let extend_at = warn_at.advanced(100);
        let extended = reg
            .extend(&grace.lease_id, grace.version, extend_at)
            .await
            .unwrap();
        assert!(extended.version > version_before);
        assert_eq!(extended.last_progress_at, last_progress_before);
        assert_eq!(
            extended.grace_deadline.as_deref(),
            Some(extend_at.advanced(600).wall_rfc3339().as_str())
        );
        // Original deadline would have been warn_at+600; after extend, cancel only after new deadline.
        let early = reg.scan(warn_at.advanced(600)).await;
        assert!(
            early.is_empty(),
            "old deadline must not cancel after extend: {early:?}"
        );
        let late = reg.scan(extend_at.advanced(600)).await;
        assert!(matches!(
            late.as_slice(),
            [RegistryAction::ClaimCancel { .. }]
        ));
        let _ = stamp;
    }

    #[tokio::test]
    async fn progress_in_warning_and_grace_returns_to_running() {
        let reg = ToolExecutionLeaseRegistry::new(ToolWatchdogSettings::default());
        let turn = sample_turn();
        let t0 = clock_base();
        reg.start_turn(turn.clone(), t0).await;
        let stamp = register_running_tool(&reg, &turn, "tool-1", t0).await;
        let key = progress_key(&turn, "tool-1");

        let actions = reg.scan(t0.advanced(600)).await;
        let RegistryAction::PublishWarning { stamp: wstamp, .. } = &actions[0] else {
            panic!("warn");
        };
        // Progress during Warning.
        let renewed = reg
            .record_tool_progress_at(
                key.clone(),
                SemanticProgress::TerminalOffset { next_offset: 1 },
                t0.advanced(601),
            )
            .await;
        assert!(renewed.is_some());
        assert!(reg.actionable_projections().await.is_empty());
        assert_eq!(
            reg.lease_phase(&stamp.lease_id).await,
            Some(ToolLeasePhase::Running)
        );

        // Re-warn and enter grace, then progress during Grace.
        let actions2 = reg.scan(t0.advanced(601 + 600)).await;
        let RegistryAction::PublishWarning { stamp: w2, .. } = &actions2[0] else {
            panic!("warn2");
        };
        let grace = reg
            .warning_published(&w2.lease_id, w2.version, t0.advanced(601 + 600))
            .await
            .unwrap();
        assert_eq!(grace.phase, ToolWatchdogPhase::Grace);
        let renewed2 = reg
            .record_tool_progress_at(
                key,
                SemanticProgress::TerminalOffset { next_offset: 2 },
                t0.advanced(601 + 650),
            )
            .await;
        assert!(renewed2.is_some());
        assert!(reg.actionable_projections().await.is_empty());
        let _ = wstamp;
    }

    #[tokio::test]
    async fn duplicate_terminal_snapshot_and_unchanged_offset_do_not_renew() {
        let reg = ToolExecutionLeaseRegistry::new(ToolWatchdogSettings::default());
        let turn = sample_turn();
        let t0 = clock_base();
        reg.start_turn(turn.clone(), t0).await;
        let stamp = register_running_tool(&reg, &turn, "tool-1", t0).await;
        let key = progress_key(&turn, "tool-1");

        let first = reg
            .record_tool_progress_at(
                key.clone(),
                SemanticProgress::TerminalOffset { next_offset: 10 },
                t0.advanced(10),
            )
            .await;
        assert!(first.is_some());
        let v1 = first.unwrap().version;

        let dup = reg
            .record_tool_progress_at(
                key.clone(),
                SemanticProgress::TerminalOffset { next_offset: 10 },
                t0.advanced(20),
            )
            .await;
        assert!(dup.is_none());

        let status = reg
            .record_tool_progress_at(
                key.clone(),
                SemanticProgress::ToolStatusChanged {
                    status_fingerprint: 3,
                },
                t0.advanced(30),
            )
            .await;
        assert!(status.is_some());
        let status_dup = reg
            .record_tool_progress_at(
                key,
                SemanticProgress::ToolStatusChanged {
                    status_fingerprint: 3,
                },
                t0.advanced(40),
            )
            .await;
        assert!(status_dup.is_none());

        // Unchanged progress means original silence still counts from last real renew.
        // After first offset at t0+10 and status at t0+30, warn at t0+30+600.
        let actions = reg.scan(t0.advanced(30 + 599)).await;
        assert!(actions.is_empty());
        let actions = reg.scan(t0.advanced(30 + 600)).await;
        assert_eq!(actions.len(), 1);
        assert!(v1 >= 1);
        let _ = stamp;
    }

    #[tokio::test]
    async fn permission_pause_and_resume_fresh_progress_window() {
        let reg = ToolExecutionLeaseRegistry::new(ToolWatchdogSettings::default());
        let turn = sample_turn();
        let t0 = clock_base();
        reg.start_turn(turn.clone(), t0).await;
        let stamp = register_running_tool(&reg, &turn, "tool-1", t0).await;

        // Almost overdue, then pause for permission.
        reg.pause_turn(&turn, PauseReason::Permission).await;
        let during_pause = reg.scan(t0.advanced(10_000)).await;
        assert!(
            during_pause.is_empty(),
            "paused must suppress warn/cancel: {during_pause:?}"
        );
        assert!(matches!(
            reg.lease_phase(&stamp.lease_id).await,
            Some(ToolLeasePhase::Paused {
                reason: PauseReason::Permission
            })
        ));

        let resume_at = t0.advanced(10_000);
        reg.resume_turn(&turn, resume_at).await;
        assert_eq!(
            reg.lease_phase(&stamp.lease_id).await,
            Some(ToolLeasePhase::Running)
        );
        // Fresh window: 599s after resume still quiet.
        assert!(reg.scan(resume_at.advanced(599)).await.is_empty());
        let warn = reg.scan(resume_at.advanced(600)).await;
        assert!(matches!(
            warn.as_slice(),
            [RegistryAction::PublishWarning { .. }]
        ));
    }

    #[tokio::test]
    async fn disable_clears_warning_grace_without_inventing_progress() {
        let reg = ToolExecutionLeaseRegistry::new(ToolWatchdogSettings::default());
        let turn = sample_turn();
        let t0 = clock_base();
        reg.start_turn(turn.clone(), t0).await;
        let stamp = register_running_tool(&reg, &turn, "tool-1", t0).await;

        let actions = reg.scan(t0.advanced(600)).await;
        let RegistryAction::PublishWarning { stamp: wstamp, .. } = &actions[0] else {
            panic!("warn");
        };
        let grace = reg
            .warning_published(&wstamp.lease_id, wstamp.version, t0.advanced(600))
            .await
            .unwrap();
        assert_eq!(grace.phase, ToolWatchdogPhase::Grace);
        let last_progress = grace.last_progress_at.clone();

        reg.apply_settings(ToolWatchdogSettings {
            enabled: false,
            warning_after_seconds: 600,
            grace_seconds: 600,
        })
        .await;

        assert!(reg.actionable_projections().await.is_empty());
        assert_eq!(
            reg.lease_phase(&stamp.lease_id).await,
            Some(ToolLeasePhase::Running)
        );
        // No scan cancel/warn while disabled.
        assert!(reg.scan(t0.advanced(10_000)).await.is_empty());

        // last_progress not invented: still original wall time after re-enable path check.
        reg.apply_settings(ToolWatchdogSettings::default()).await;
        // Immediately overdue under original last_progress_at = t0.
        let re_warn = reg.scan(t0.advanced(10_000)).await;
        assert_eq!(re_warn.len(), 1);
        match &re_warn[0] {
            RegistryAction::PublishWarning { projection, .. } => {
                assert_eq!(projection.last_progress_at, last_progress);
                assert_eq!(projection.phase, ToolWatchdogPhase::Warning);
            }
            other => panic!("expected warning only on re-enable scan: {other:?}"),
        }
        // Cannot cancel in the same re-enable scan.
        assert!(!re_warn
            .iter()
            .any(|a| matches!(a, RegistryAction::ClaimCancel { .. })));
    }

    #[tokio::test]
    async fn completion_progress_user_stop_timeout_single_winner() {
        // Case A: completion wins before cancel.
        let reg = ToolExecutionLeaseRegistry::new(ToolWatchdogSettings::default());
        let turn = sample_turn();
        let t0 = clock_base();
        reg.start_turn(turn.clone(), t0).await;
        let stamp = register_running_tool(&reg, &turn, "tool-a", t0).await;
        let cleared = reg.complete_tool(&tool_key(&turn, "tool-a")).await;
        assert!(cleared.is_some());
        assert_eq!(cleared.unwrap().phase, ToolWatchdogPhase::Cleared);
        assert!(reg
            .claim_cancel(&stamp.lease_id, stamp.version, CancelCause::UserStop)
            .await
            .is_err());

        // Case B: user stop wins; completion loses (late_activity only).
        let reg = ToolExecutionLeaseRegistry::new(ToolWatchdogSettings::default());
        reg.start_turn(turn.clone(), t0).await;
        let stamp = register_running_tool(&reg, &turn, "tool-b", t0).await;
        let claim = reg
            .claim_cancel(&stamp.lease_id, stamp.version, CancelCause::UserStop)
            .await
            .unwrap();
        assert_eq!(claim.cause, CancelCause::UserStop);
        assert_eq!(
            reg.lease_phase(&stamp.lease_id).await,
            Some(ToolLeasePhase::Cancelling)
        );
        let complete_loses = reg.complete_tool(&tool_key(&turn, "tool-b")).await;
        assert!(complete_loses.is_none());
        assert_eq!(reg.late_activity(&stamp.lease_id).await, Some(1));
        // Progress after cancel does not revive.
        let prog = reg
            .record_tool_progress_at(
                progress_key(&turn, "tool-b"),
                SemanticProgress::TerminalOffset { next_offset: 99 },
                t0.advanced(1),
            )
            .await;
        assert!(prog.is_none());
        assert_eq!(reg.late_activity(&stamp.lease_id).await, Some(2));
        assert_eq!(
            reg.lease_phase(&stamp.lease_id).await,
            Some(ToolLeasePhase::Cancelling)
        );

        // Case C: timeout claim is the single auto winner.
        let reg = ToolExecutionLeaseRegistry::new(ToolWatchdogSettings::default());
        reg.start_turn(turn.clone(), t0).await;
        let stamp = register_running_tool(&reg, &turn, "tool-c", t0).await;
        let actions = reg.scan(t0.advanced(600)).await;
        let RegistryAction::PublishWarning { stamp: w, .. } = &actions[0] else {
            panic!("warn");
        };
        let grace = reg
            .warning_published(&w.lease_id, w.version, t0.advanced(600))
            .await
            .unwrap();
        let cancel = reg.scan(t0.advanced(1200)).await;
        let RegistryAction::ClaimCancel { claim } = &cancel[0] else {
            panic!("cancel");
        };
        assert_eq!(claim.cause, CancelCause::AutoTimeout);
        // Second claim loses.
        assert!(reg
            .claim_cancel(&grace.lease_id, grace.version, CancelCause::UserStop)
            .await
            .is_err());
        let _ = stamp;
    }

    #[tokio::test]
    async fn stale_lease_version_incarnation_turn_generation_rejected() {
        let reg = ToolExecutionLeaseRegistry::new(ToolWatchdogSettings::default());
        let turn = sample_turn();
        let t0 = clock_base();
        reg.start_turn(turn.clone(), t0).await;
        let stamp = register_running_tool(&reg, &turn, "tool-1", t0).await;

        // Stale version.
        assert!(reg
            .claim_cancel(&stamp.lease_id, stamp.version + 99, CancelCause::UserStop)
            .await
            .is_err());
        assert!(reg
            .extend(&stamp.lease_id, stamp.version, t0.advanced(1))
            .await
            .is_err()); // not in Grace

        // Bind with wrong incarnation.
        let mut bad = stamp.clone();
        bad.connection_incarnation = "other-inc".into();
        assert!(reg
            .bind_capability(&bad, CancellationCapability::Turn)
            .await
            .is_err());

        // Wrong turn generation.
        let mut bad_turn = stamp.clone();
        bad_turn.turn_generation = 99;
        assert!(reg
            .bind_capability(&bad_turn, CancellationCapability::Turn)
            .await
            .is_err());

        // Unknown lease id.
        assert!(reg
            .warning_published("missing", 1, t0)
            .await
            .is_err());
    }

    #[tokio::test]
    async fn ambiguous_terminal_binding_retains_only_turn_capability() {
        let reg = ToolExecutionLeaseRegistry::new(ToolWatchdogSettings::default());
        let turn = sample_turn();
        let t0 = clock_base();
        reg.start_turn(turn.clone(), t0).await;
        let stamp = register_running_tool(&reg, &turn, "tool-1", t0).await;
        // Default capability is Turn (ambiguous / no terminal association).
        let actions = reg.scan(t0.advanced(600)).await;
        let RegistryAction::PublishWarning { stamp: w, .. } = &actions[0] else {
            panic!("warn");
        };
        let grace = reg
            .warning_published(&w.lease_id, w.version, t0.advanced(600))
            .await
            .unwrap();
        let cancel = reg.scan(t0.advanced(1200)).await;
        let RegistryAction::ClaimCancel { claim } = &cancel[0] else {
            panic!("cancel");
        };
        assert_eq!(claim.capability, CancellationCapability::Turn);
        assert_eq!(claim.stamp.lease_id, stamp.lease_id);

        // Explicit unambiguous bind upgrades capability.
        let reg2 = ToolExecutionLeaseRegistry::new(ToolWatchdogSettings::default());
        reg2.start_turn(turn.clone(), t0).await;
        let stamp2 = register_running_tool(&reg2, &turn, "tool-2", t0).await;
        let bound = reg2
            .bind_capability(
                &stamp2,
                CancellationCapability::Terminal {
                    session_id: "sess-1".into(),
                    terminal_id: "term-9".into(),
                },
            )
            .await
            .unwrap();
        assert!(bound.version > stamp2.version);
        let claim2 = reg2
            .claim_cancel(&bound.lease_id, bound.version, CancelCause::UserStop)
            .await
            .unwrap();
        assert_eq!(
            claim2.capability,
            CancellationCapability::Terminal {
                session_id: "sess-1".into(),
                terminal_id: "term-9".into(),
            }
        );
        let _ = grace;
    }

    #[tokio::test]
    async fn untracked_turn_uses_1800_plus_600_timing() {
        let reg = ToolExecutionLeaseRegistry::new(ToolWatchdogSettings {
            enabled: true,
            // Live tracked warning is short; fallback must stay fixed 1800.
            warning_after_seconds: 60,
            grace_seconds: 600,
        });
        let turn = sample_turn();
        let t0 = clock_base();
        reg.start_turn(turn.clone(), t0).await;
        assert!(reg.has_fallback(&turn).await);

        // Not warned at 1799s.
        assert!(reg.scan(t0.advanced(1_799)).await.is_empty());
        // Would already be warned if using live 60s tracked threshold.
        let actions = reg.scan(t0.advanced(1_800)).await;
        assert_eq!(actions.len(), 1);
        let RegistryAction::PublishWarning { stamp, projection } = &actions[0] else {
            panic!("expected fallback warning");
        };
        assert_eq!(projection.tool_title, ToolCategory::Other);
        assert_eq!(stamp.tool_call_id.as_deref(), Some(FALLBACK_TOOL_CALL_ID));

        let grace = reg
            .warning_published(&stamp.lease_id, stamp.version, t0.advanced(1_800))
            .await
            .unwrap();
        assert!(reg.scan(t0.advanced(1_800 + 599)).await.is_empty());
        let cancel = reg.scan(t0.advanced(1_800 + 600)).await;
        assert!(matches!(
            cancel.as_slice(),
            [RegistryAction::ClaimCancel {
                claim: CancellationClaim {
                    cause: CancelCause::AutoTimeout,
                    capability: CancellationCapability::Turn,
                    ..
                }
            }]
        ));
        let _ = grace;
    }

    #[tokio::test]
    async fn register_tool_retires_fallback_complete_rearms_when_eligible() {
        let reg = ToolExecutionLeaseRegistry::new(ToolWatchdogSettings::default());
        let turn = sample_turn();
        let t0 = clock_base();
        reg.start_turn(turn.clone(), t0).await;
        assert!(reg.has_fallback(&turn).await);

        let _stamp = register_running_tool(&reg, &turn, "tool-1", t0).await;
        assert!(!reg.has_fallback(&turn).await);

        let cleared = reg.complete_tool(&tool_key(&turn, "tool-1")).await;
        assert!(cleared.is_some());
        assert!(reg.has_fallback(&turn).await);

        // Background handoff: complete last tool but background accounts for turn.
        let _stamp = register_running_tool(&reg, &turn, "tool-2", t0.advanced(1)).await;
        assert!(!reg.has_fallback(&turn).await);
        reg.set_verified_background_work(&turn, true).await;
        let _ = reg.complete_tool(&tool_key(&turn, "tool-2")).await;
        assert!(
            !reg.has_fallback(&turn).await,
            "must not re-arm while background work accounts for the turn"
        );
    }

    #[tokio::test]
    async fn setting_reduction_warns_but_no_same_pass_cancel() {
        let reg = ToolExecutionLeaseRegistry::new(ToolWatchdogSettings {
            enabled: true,
            warning_after_seconds: 600,
            grace_seconds: 60,
        });
        let turn = sample_turn();
        let t0 = clock_base();
        reg.start_turn(turn.clone(), t0).await;
        let _ = register_running_tool(&reg, &turn, "tool-1", t0).await;

        // Reduce warning threshold so work is immediately overdue.
        reg.apply_settings(ToolWatchdogSettings {
            enabled: true,
            warning_after_seconds: 60,
            grace_seconds: 60,
        })
        .await;
        let actions = reg.scan(t0.advanced(120)).await;
        assert_eq!(actions.len(), 1);
        assert!(matches!(
            actions[0],
            RegistryAction::PublishWarning { .. }
        ));
        assert!(!actions
            .iter()
            .any(|a| matches!(a, RegistryAction::ClaimCancel { .. })));
    }

    #[tokio::test]
    async fn public_projection_uses_tool_category_not_free_form_title() {
        let reg = ToolExecutionLeaseRegistry::new(ToolWatchdogSettings::default());
        let turn = sample_turn();
        let t0 = clock_base();
        reg.start_turn(turn.clone(), t0).await;
        reg.register_tool(RegisterTool {
            turn: turn.clone(),
            tool_call_id: "toolu_secret".into(),
            category: ToolCategory::Mcp,
            at: t0,
        })
        .await;
        let actions = reg.scan(t0.advanced(600)).await;
        let RegistryAction::PublishWarning { projection, stamp } = &actions[0] else {
            panic!("warn");
        };
        assert_eq!(projection.tool_title, ToolCategory::Mcp);
        let json = serde_json::to_string(projection).unwrap();
        assert!(!json.contains("toolu_secret"));
        assert!(!json.contains("tool_call_id"));
        assert!(json.contains("\"tool_title\":\"mcp\""));
        // Internal stamp may hold tool id; projection must not.
        assert_eq!(stamp.tool_call_id.as_deref(), Some("toolu_secret"));
    }

    #[tokio::test]
    async fn agent_activity_renews_fallback_only() {
        let reg = ToolExecutionLeaseRegistry::new(ToolWatchdogSettings::default());
        let turn = sample_turn();
        let t0 = clock_base();
        reg.start_turn(turn.clone(), t0).await;
        let fb = reg.fallback_stamp(&turn).await.unwrap();

        reg.record_turn_progress_at(
            &turn,
            SemanticProgress::AgentActivity { content_hash: 1 },
            t0.advanced(100),
        )
        .await;
        // After renew, warn clock resets: 1799 from t0+100 still quiet for untracked.
        // Using default tracked 600 — fallback uses 1800.
        assert!(reg.scan(t0.advanced(100 + 1_799)).await.is_empty());
        let warn = reg.scan(t0.advanced(100 + 1_800)).await;
        assert!(matches!(
            warn.as_slice(),
            [RegistryAction::PublishWarning { .. }]
        ));
        let _ = fb;
    }
}
