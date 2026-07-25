//! Atomic tool-execution lease registry and state machine.
//!
//! Deadlines are derived from recorded timestamps (`WatchdogInstant`), not from
//! scan cadence, so scan jitter cannot accumulate. Semantic progress is the only
//! progress clock; keepalive never renews leases here.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use chrono::{DateTime, Utc};
use tokio::time::Instant;
use uuid::Uuid;

use super::progress::{apply_semantic_progress, ProgressFingerprint};
use super::types::{
    CancellationCapability, CancellationScope, LeaseStamp, PauseReason, ToolCategory,
    ToolLeasePhase, ToolWatchdogPhase, ToolWatchdogProjection, ToolWatchdogSettings,
    DEFAULT_GRACE_SECS, ERROR_CODE_TOOL_STALLED_TIMEOUT, ERROR_CODE_USER_CANCELLED,
    UNTRACKED_WARNING_AFTER_SECS,
};

/// Fixed host discriminator for the untracked-turn fallback lease.
pub const FALLBACK_TOOL_CALL_ID: &str = "__untracked_turn__";

/// Result of applying semantic tool progress to a live lease.
///
/// When progress demotes Warning/Grace back to Running, `cleared` carries a
/// Cleared projection at the post-renew version so attach snapshots drop the
/// lease. `Deref` targets [`LeaseStamp`] so call sites can keep using
/// `.lease_id` / `.version`.
#[derive(Debug, Clone)]
pub struct ToolProgressApply {
    pub stamp: LeaseStamp,
    pub cleared: Option<ToolWatchdogProjection>,
}

impl std::ops::Deref for ToolProgressApply {
    type Target = LeaseStamp;
    fn deref(&self) -> &Self::Target {
        &self.stamp
    }
}

/// Result of admitting a tracked tool lease.
///
/// When first admission retires an untracked fallback that was still
/// Warning/Grace/Cancelling, `cleared` carries a Cleared projection so hosts
/// can drop the retired fallback from the attach replay map. `Deref` targets
/// [`LeaseStamp`] for call sites that only need the new tool stamp.
#[derive(Debug, Clone)]
pub struct RegisterToolOutcome {
    pub stamp: LeaseStamp,
    pub cleared: Option<ToolWatchdogProjection>,
}

impl std::ops::Deref for RegisterToolOutcome {
    type Target = LeaseStamp;
    fn deref(&self) -> &Self::Target {
        &self.stamp
    }
}

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

    /// Advance by whole milliseconds (same wall second when `millis < 1000`).
    pub fn advanced_millis(self, millis: u64) -> Self {
        Self {
            mono: self.mono + Duration::from_millis(millis),
            wall: self.wall + chrono::Duration::milliseconds(millis as i64),
        }
    }

    /// Wall clock for public projections. Millis precision so concurrent
    /// same-second transitions remain totally ordered on the wire.
    pub fn wall_rfc3339(self) -> String {
        self.wall
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
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
    /// Terminal byte offset. When `terminal_id_hash` is set, progress is tracked
    /// per-terminal so a lower-offset peer can still renew a multi-terminal tool.
    TerminalOffset {
        terminal_id_hash: Option<u64>,
        next_offset: u64,
    },
    TerminalExit,
    ToolStatusChanged {
        status_fingerprint: u64,
    },
    McpProgress {
        token_or_hash: u64,
    },
    /// Per-lease monotonic progress token (not wall-clock ms). Prefer
    /// [`ToolExecutionLeaseRegistry::record_delegation_activity`], which
    /// allocates the next token from the lease fingerprint.
    DelegationActivity {
        at_mono_ms: u64,
    },
    /// Untracked fallback only unless the caller associates it with a tool key.
    AgentActivity {
        content_hash: u64,
    },
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
        /// Cancelling projection published with the claim (same CAS version).
        projection: ToolWatchdogProjection,
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

pub use super::types::CancelCause;

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
    /// Host-global monotonic counter for projection-producing transitions.
    /// Lives outside the lease map so callers can allocate while holding a
    /// lease mut ref without borrow conflicts.
    transition_seq: AtomicU64,
}

impl std::fmt::Debug for ToolExecutionLeaseRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ToolExecutionLeaseRegistry")
    }
}

struct RegistryInner {
    settings: ToolWatchdogSettings,
    leases: HashMap<String, LeaseRecord>,
    tool_index: HashMap<ToolLeaseKey, String>,
    /// Logical tool keys completed while a generation is still Prompting.
    /// Blocks same-key replay from resurrecting a tracked lease or retiring a
    /// re-armed fallback. Cleared for a generation on `complete_turn` (and for
    /// the connection on `remove_connection`); not retained for the full
    /// connection lifetime.
    completed_tools: HashSet<ToolLeaseKey>,
    turns: HashMap<TurnKey, TurnRecord>,
    /// Closed connection incarnations. Once fenced, `register_tool` /
    /// `start_turn` reject admission for that (connection_id, incarnation)
    /// forever so a still-running connection loop cannot recreate leases after
    /// disconnect cleanup clears the registry.
    fenced: HashSet<IncarnationKey>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct IncarnationKey {
    connection_id: String,
    connection_incarnation: String,
}

impl IncarnationKey {
    fn new(connection_id: &str, incarnation: &str) -> Self {
        Self {
            connection_id: connection_id.to_string(),
            connection_incarnation: incarnation.to_string(),
        }
    }

    fn from_turn(turn: &TurnStamp) -> Self {
        Self {
            connection_id: turn.connection_id.clone(),
            connection_incarnation: turn.connection_incarnation.clone(),
        }
    }
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
    /// Turn-level agent activity fingerprint; baseline advances only on new hash.
    agent_content_hash: Option<u64>,
    is_prompting: bool,
    /// `true` once `start_turn` has observed Prompting admission for this generation.
    /// Provisional records created by `register_tool` start as `false`.
    prompt_admitted: bool,
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
    /// Wall time of the most recent public phase/version transition (warning,
    /// grace, extend, cancel, timeout, clear). Drives `transition_at` on the
    /// wire so session-details can order concurrent leases without using
    /// per-lease version as a connection-wide sequence.
    last_transition_at: WatchdogInstant,
    /// Host-global sequence of the most recent public transition (0 until first).
    transition_seq: u64,
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
            transition_at: self.last_transition_at.wall_rfc3339(),
            transition_seq: self.transition_seq,
            grace_deadline: self.grace_deadline.map(|g| g.wall_rfc3339()),
            cancellation_scope,
            error_code: None,
        }
    }

    /// Bump CAS version, stamp public transition wall time, and record global seq.
    fn bump_transition(&mut self, at: WatchdogInstant, seq: u64) {
        self.last_transition_at = at;
        self.transition_seq = seq;
        self.bump();
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

    /// Tracked lease still occupies the turn for fallback eligibility (includes claim).
    fn is_tracked_present(&self) -> bool {
        self.is_live_active() || matches!(self.phase, ToolLeasePhase::Cancelling)
    }

    /// Snapshot/action clients only see Grace+; Warning is publish-transition only.
    fn is_actionable_public(&self) -> bool {
        matches!(
            self.phase,
            ToolLeasePhase::Grace | ToolLeasePhase::Cancelling
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
                completed_tools: HashSet::new(),
                turns: HashMap::new(),
                fenced: HashSet::new(),
            }),
            transition_seq: AtomicU64::new(0),
        }
    }

    /// Close lease admission for a connection incarnation.
    ///
    /// After this returns, `register_tool` and `start_turn` for the same
    /// `(connection_id, incarnation)` no-op / reject. Call **before**
    /// `remove_connection` and map removal so a concurrent tool event cannot
    /// recreate leases in the disconnect gap.
    pub async fn fence_connection(&self, connection_id: &str, incarnation: &str) {
        let mut inner = self.inner.lock().await;
        inner
            .fenced
            .insert(IncarnationKey::new(connection_id, incarnation));
    }

    /// Whether admission is closed for this incarnation (host/test helper).
    pub async fn is_fenced(&self, connection_id: &str, incarnation: &str) -> bool {
        let inner = self.inner.lock().await;
        inner
            .fenced
            .contains(&IncarnationKey::new(connection_id, incarnation))
    }

    /// Apply clamped settings. When disabling, demotes Warning/Grace to Running
    /// without inventing progress and returns Cleared projections so hosts can
    /// drop those leases from the attach replay map.
    pub async fn apply_settings(
        &self,
        settings: ToolWatchdogSettings,
    ) -> Vec<ToolWatchdogProjection> {
        let mut inner = self.inner.lock().await;
        let next = settings.clamp();
        let was_enabled = inner.settings.enabled;
        inner.settings = next;
        let mut cleared = Vec::new();
        if was_enabled && !inner.settings.enabled {
            // Disable clears warning/grace without inventing progress.
            let mut to_clear: Vec<String> = Vec::new();
            for (id, lease) in inner.leases.iter() {
                if matches!(lease.phase, ToolLeasePhase::Warning | ToolLeasePhase::Grace) {
                    to_clear.push(id.clone());
                }
            }
            let clear_at = WatchdogInstant::now();
            for id in to_clear {
                let seq = self.transition_seq.fetch_add(1, Ordering::Relaxed) + 1;
                if let Some(lease) = inner.leases.get_mut(&id) {
                    lease.phase = ToolLeasePhase::Running;
                    lease.clear_warning_fields();
                    lease.bump_transition(clear_at, seq);
                    cleared.push(lease.to_projection(ToolWatchdogPhase::Cleared));
                }
            }
        }
        cleared
    }

    /// Current clamped live settings (host/test helper).
    pub async fn settings(&self) -> ToolWatchdogSettings {
        self.inner.lock().await.settings
    }

    /// Number of live leases currently held (startup begins at zero).
    pub async fn live_lease_count(&self) -> usize {
        self.inner.lock().await.leases.len()
    }

    /// Public phase projection for a live lease, if any.
    pub async fn live_projection(&self, lease_id: &str) -> Option<ToolWatchdogProjection> {
        let inner = self.inner.lock().await;
        let lease = inner.leases.get(lease_id)?;
        let phase = lease.public_phase()?;
        Some(lease.to_projection(phase))
    }

    /// Coarse tool category for a live lease (metrics labels).
    pub async fn lease_category(&self, lease_id: &str) -> Option<ToolCategory> {
        let inner = self.inner.lock().await;
        inner.leases.get(lease_id).map(|l| l.category)
    }

    pub async fn start_turn(&self, turn: TurnStamp, at: WatchdogInstant) {
        let mut inner = self.inner.lock().await;
        // Disconnect fence: refuse new Prompting admission for a closed incarnation.
        if inner.fenced.contains(&IncarnationKey::from_turn(&turn)) {
            return;
        }
        let key = TurnKey::from_turn(&turn);
        // After complete_turn (is_prompting=false), do not revive the generation.
        // After prompt admission, further start_turn calls are idempotent: keep
        // the existing TurnRecord and fallback_lease_id (never replace/orphan).
        // Provisional records from register_tool (prompt_admitted=false) merge
        // the real admission timestamp without touching live tool leases.
        if let Some(rec) = inner.turns.get_mut(&key) {
            if !rec.is_prompting || rec.prompt_admitted {
                return;
            }
            // Admit provisional turn: overwrite turn_start_at with Prompting time.
            rec.turn = turn.clone();
            rec.turn_start_at = at;
            rec.prompt_admitted = true;
            let fb_id = rec.fallback_lease_id.clone();
            let last_agent = rec.last_verified_agent_activity_at;
            let baseline = max_progress_baseline(at, last_agent);
            if let Some(id) = fb_id {
                // Rebase existing fallback clock; do not replace the lease id.
                if let Some(lease) = inner.leases.get_mut(&id) {
                    if matches!(lease.phase, ToolLeasePhase::Running) {
                        lease.last_progress_at = baseline;
                    }
                }
            } else {
                inner.maybe_register_fallback(&key, at);
            }
            return;
        }
        let record = TurnRecord {
            turn: turn.clone(),
            turn_start_at: at,
            last_verified_agent_activity_at: None,
            agent_content_hash: None,
            is_prompting: true,
            prompt_admitted: true,
            pending_permission: false,
            pending_user_input: false,
            verified_background_work: false,
            fallback_lease_id: None,
        };
        inner.turns.insert(key.clone(), record);
        inner.maybe_register_fallback(&key, at);
    }

    /// Retires fallback while tracked leases exist; re-arms when last tracked ends.
    ///
    /// # Returns
    ///
    /// - `Ok(stamp)` for a newly admitted tracked lease, or a live duplicate
    ///   registration of the same logical key (returns the existing stamp;
    ///   no re-allocation, no second fallback retirement).
    /// - `Err(StaleLease)` when registration is rejected:
    ///   - the connection incarnation is fenced (disconnect admission closed),
    ///   - generation is no longer Prompting (`complete_turn` already set
    ///     `is_prompting = false`), or
    ///   - the logical tool key is tombstoned in `completed_tools` (still
    ///     Prompting after `complete_tool`; tombstones are held only while
    ///     the turn can accept replayed registrations).
    ///
    /// Rejects never retire fallback or allocate a phantom lease. `StaleLease`
    /// is reused as the deliberate checked rejection type for this API (same
    /// error surface as other CAS/stale registry methods).
    ///
    /// On success, [`RegisterToolOutcome::cleared`] is set when first admission
    /// retires a fallback that was still Warning/Grace/Cancelling.
    pub async fn register_tool(
        &self,
        input: RegisterTool,
    ) -> Result<RegisterToolOutcome, StaleLease> {
        let mut inner = self.inner.lock().await;
        // Disconnect fence: refuse re-admission after incarnation cleanup.
        if inner
            .fenced
            .contains(&IncarnationKey::from_turn(&input.turn))
        {
            return Err(StaleLease);
        }
        let turn_key = TurnKey::from_turn(&input.turn);

        // Refuse admission for a generation that already finished Prompting.
        if let Some(rec) = inner.turns.get(&turn_key) {
            if !rec.is_prompting {
                return Err(StaleLease);
            }
        }

        let tool_key = ToolLeaseKey {
            connection_id: input.turn.connection_id.clone(),
            connection_incarnation: input.turn.connection_incarnation.clone(),
            turn_generation: input.turn.turn_generation,
            tool_call_id: input.tool_call_id.clone(),
        };

        // Completed-key tombstone: do not retire fallback or allocate a lease.
        if inner.completed_tools.contains(&tool_key) {
            return Err(StaleLease);
        }

        // Live duplicate: return existing stamp without re-allocation.
        if let Some(existing_id) = inner.tool_index.get(&tool_key).cloned() {
            if let Some(lease) = inner.leases.get(&existing_id) {
                return Ok(RegisterToolOutcome {
                    stamp: lease.stamp(),
                    cleared: None,
                });
            }
        }

        // Ensure turn exists (prompt admission may race). Provisional: first
        // real start_turn will overwrite turn_start_at with admission time.
        inner
            .turns
            .entry(turn_key.clone())
            .or_insert_with(|| TurnRecord {
                turn: input.turn.clone(),
                turn_start_at: input.at,
                last_verified_agent_activity_at: None,
                agent_content_hash: None,
                is_prompting: true,
                prompt_admitted: false,
                pending_permission: false,
                pending_user_input: false,
                verified_background_work: false,
                fallback_lease_id: None,
            });

        // Retire fallback while any tracked lease exists.
        let cleared = inner.retire_fallback(
            &turn_key,
            self.transition_seq.fetch_add(1, Ordering::Relaxed) + 1,
        );

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
            last_transition_at: input.at,
            transition_seq: 0,
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
        Ok(RegisterToolOutcome { stamp, cleared })
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
        // Allow bind while Running/Paused/Warning/Grace so multi-task wait park
        // can upgrade to DelegationWait even if the turn is paused for input.
        // Cancelling/terminal phases refuse capability mutation.
        if !matches!(
            lease.phase,
            ToolLeasePhase::Running
                | ToolLeasePhase::Paused { .. }
                | ToolLeasePhase::Warning
                | ToolLeasePhase::Grace
        ) {
            return Err(StaleLease);
        }
        // Idempotent when capability already matches — avoid needless version bumps.
        if lease.capability == capability {
            return Ok(lease.stamp());
        }
        lease.capability = capability;
        lease.bump();
        Ok(lease.stamp())
    }

    /// Current CAS stamp for an exact tool lease, if still live.
    pub async fn tool_stamp(&self, key: &ToolLeaseKey) -> Option<LeaseStamp> {
        let inner = self.inner.lock().await;
        let lease_id = inner.tool_index.get(key)?;
        let lease = inner.leases.get(lease_id)?;
        Some(lease.stamp())
    }

    /// Host/test helper: inspect cancellation capability for a live lease.
    pub async fn lease_capability(&self, lease_id: &str) -> Option<CancellationCapability> {
        let inner = self.inner.lock().await;
        inner.leases.get(lease_id).map(|l| l.capability.clone())
    }

    /// Host/test helper: inspect the current CAS stamp for a live lease.
    pub async fn lease_stamp(&self, lease_id: &str) -> Option<LeaseStamp> {
        let inner = self.inner.lock().await;
        inner.leases.get(lease_id).map(|l| l.stamp())
    }

    /// Record tool-associated semantic progress using the host wall/mono clock.
    ///
    /// Prefer [`Self::record_tool_progress_at`] when a controlled clock is required
    /// (tests and injected host time).
    pub async fn record_tool_progress(
        &self,
        key: ToolProgressKey,
        fact: SemanticProgress,
    ) -> Option<ToolProgressApply> {
        self.record_tool_progress_at(key, fact, WatchdogInstant::now())
            .await
    }

    /// Verified delegated-child activity: renew the parent tool lease using a
    /// **per-lease monotonic sequence** as the progress token.
    ///
    /// Callers must not pass wall-clock milliseconds — a clock rollback would
    /// reject renewals under the strict `<= prev` fingerprint gate.
    pub async fn record_delegation_activity(
        &self,
        key: ToolProgressKey,
        at: WatchdogInstant,
    ) -> Option<ToolProgressApply> {
        let mut inner = self.inner.lock().await;
        let seq = self.transition_seq.fetch_add(1, Ordering::Relaxed) + 1;
        let tool_key = ToolLeaseKey {
            connection_id: key.connection_id.clone(),
            connection_incarnation: key.connection_incarnation.clone(),
            turn_generation: key.turn_generation,
            tool_call_id: key.tool_call_id.clone(),
        };
        let lease_id = inner.tool_index.get(&tool_key)?.clone();
        let next_token = {
            let lease = inner.leases.get(&lease_id)?;
            lease
                .fingerprint
                .delegation_at_mono_ms
                .map(|p| p.saturating_add(1))
                .unwrap_or(1)
        };
        inner.record_tool_progress_at(
            key,
            SemanticProgress::DelegationActivity {
                at_mono_ms: next_token,
            },
            at,
            seq,
        )
    }

    /// Controlled-clock progress path used by host wiring and tests.
    pub async fn record_tool_progress_at(
        &self,
        key: ToolProgressKey,
        fact: SemanticProgress,
        at: WatchdogInstant,
    ) -> Option<ToolProgressApply> {
        let mut inner = self.inner.lock().await;
        let seq = self.transition_seq.fetch_add(1, Ordering::Relaxed) + 1;
        inner.record_tool_progress_at(key, fact, at, seq)
    }

    pub async fn record_turn_progress(
        &self,
        turn: &TurnStamp,
        fact: SemanticProgress,
    ) -> Option<ToolWatchdogProjection> {
        self.record_turn_progress_at(turn, fact, WatchdogInstant::now())
            .await
    }

    /// Renews the untracked fallback on new agent activity. Returns a Cleared
    /// projection when the fallback was demoted from Warning/Grace.
    pub async fn record_turn_progress_at(
        &self,
        turn: &TurnStamp,
        fact: SemanticProgress,
        at: WatchdogInstant,
    ) -> Option<ToolWatchdogProjection> {
        let mut inner = self.inner.lock().await;
        let turn_key = TurnKey::from_turn(turn);
        let turn_rec = inner.turns.get_mut(&turn_key)?;
        // Generic agent transcript activity renews only the untracked fallback.
        let SemanticProgress::AgentActivity { content_hash } = fact else {
            return None;
        };
        // Turn-level fingerprint: baseline advances only when the hash is new,
        // including while a tracked tool has retired the fallback.
        let is_new_agent_fact = turn_rec.agent_content_hash != Some(content_hash);
        if is_new_agent_fact {
            turn_rec.agent_content_hash = Some(content_hash);
            turn_rec.last_verified_agent_activity_at = Some(at);
        }
        let fallback_id = turn_rec.fallback_lease_id.clone();
        let lease_id = fallback_id?;
        let lease = inner.leases.get_mut(&lease_id)?;
        if matches!(lease.phase, ToolLeasePhase::Cancelling) {
            lease.late_activity = lease.late_activity.saturating_add(1);
            return None;
        }
        if matches!(lease.phase, ToolLeasePhase::Paused { .. }) {
            if is_new_agent_fact {
                let _ = apply_semantic_progress(&mut lease.fingerprint, &fact);
            }
            return None;
        }
        if !is_new_agent_fact {
            return None;
        }
        let _ = apply_semantic_progress(&mut lease.fingerprint, &fact);
        let seq = self.transition_seq.fetch_add(1, Ordering::Relaxed) + 1;
        renew_lease_to_running(lease, at, seq)
    }

    /// Pause live leases on `turn`. Returns Cleared projections for any
    /// Warning/Grace lease demoted to Paused so hosts can drop them from the
    /// attach replay map.
    pub async fn pause_turn(
        &self,
        turn: &TurnStamp,
        reason: PauseReason,
    ) -> Vec<ToolWatchdogProjection> {
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
        let mut cleared = Vec::new();
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
                let demoted_actionable =
                    matches!(lease.phase, ToolLeasePhase::Warning | ToolLeasePhase::Grace);
                lease.phase = ToolLeasePhase::Paused {
                    reason: reason.clone(),
                };
                lease.clear_warning_fields();
                if demoted_actionable {
                    let seq = self.transition_seq.fetch_add(1, Ordering::Relaxed) + 1;
                    lease.bump_transition(WatchdogInstant::now(), seq);
                    cleared.push(lease.to_projection(ToolWatchdogPhase::Cleared));
                } else {
                    lease.bump();
                }
            }
        }
        // Pending input retires fallback eligibility (not re-armed while paused input).
        if let Some(rec) = inner.turns.get(&turn_key) {
            if rec.fallback_lease_id.is_some() && !inner.turn_is_fallback_eligible(&turn_key) {
                // Keep fallback lease paused rather than removed: still no tracked
                // tool, but eligibility false. Pause already applied above.
            }
        }
        cleared
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
        // Cancel claim already owns the outcome: completion cannot emit Cleared.
        // Settle as TimedOut/user_cancelled so the lease leaves the live map
        // (supervisor convergence) and a failed-tool projection is produced.
        if matches!(lease.phase, ToolLeasePhase::Cancelling) {
            lease.late_activity = lease.late_activity.saturating_add(1);
            let seq = self.transition_seq.fetch_add(1, Ordering::Relaxed) + 1;
            return Some(inner.settle_cancelling_locked(&lease_id, seq));
        }
        if !lease.is_live_active() {
            return None;
        }
        lease.phase = ToolLeasePhase::Completed;
        let seq = self.transition_seq.fetch_add(1, Ordering::Relaxed) + 1;
        lease.bump_transition(WatchdogInstant::now(), seq);
        let projection = lease.to_projection(ToolWatchdogPhase::Cleared);
        // Remove from live map; retain tombstone while the turn is still
        // Prompting so same-key re-register cannot resurrect. Reclaimed on
        // complete_turn once is_prompting=false is the admission guard.
        inner.leases.remove(&lease_id);
        inner.tool_index.remove(key);
        inner.completed_tools.insert(key.clone());

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
            // Cleared emission for a Grace fallback on this path is not wired
            // (handoff completes the tracked tool first); first-tool admission
            // is the permanent-stale case that must publish Cleared.
            let _ = inner.retire_fallback(
                &turn_key,
                self.transition_seq.fetch_add(1, Ordering::Relaxed) + 1,
            );
        } else {
            let at = inner
                .turns
                .get(&turn_key)
                .map(|t| max_progress_baseline(t.turn_start_at, t.last_verified_agent_activity_at))
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
            let phase = match inner.leases.get(&id) {
                Some(lease) => lease.phase.clone(),
                None => continue,
            };
            // Cancellation claim already won: do not emit Cleared. Settle as
            // TimedOut/user_cancelled so the lease leaves the map (convergence)
            // and a failed-tool projection is produced.
            if matches!(phase, ToolLeasePhase::Cancelling) {
                if let Some(lease) = inner.leases.get_mut(&id) {
                    lease.late_activity = lease.late_activity.saturating_add(1);
                }
                let seq = self.transition_seq.fetch_add(1, Ordering::Relaxed) + 1;
                cleared.push(inner.settle_cancelling_locked(&id, seq));
                continue;
            }
            let Some(mut lease) = inner.leases.remove(&id) else {
                continue;
            };
            if let Some(tool_id) = lease.tool_call_id.clone() {
                let tool_key = ToolLeaseKey {
                    connection_id: lease.connection_id.clone(),
                    connection_incarnation: lease.connection_incarnation.clone(),
                    turn_generation: lease.turn_generation,
                    tool_call_id: tool_id,
                };
                inner.tool_index.remove(&tool_key);
            }
            if lease.is_live_active() {
                lease.phase = ToolLeasePhase::Completed;
                let seq = self.transition_seq.fetch_add(1, Ordering::Relaxed) + 1;
                lease.bump_transition(WatchdogInstant::now(), seq);
                cleared.push(lease.to_projection(ToolWatchdogPhase::Cleared));
            }
            // Settled non-live (e.g. TimedOut) drops without inventing Cleared.
        }
        if let Some(rec) = inner.turns.get_mut(&turn_key) {
            rec.is_prompting = false;
            rec.fallback_lease_id = None;
            rec.pending_permission = false;
            rec.pending_user_input = false;
            rec.verified_background_work = false;
        }
        // Tombstones are needed only while the generation is still Prompting
        // (blocks same-key replay after complete_tool). After complete_turn,
        // is_prompting=false already rejects register_tool, so reclaim the
        // generation's completed_tools entries to bound registry state.
        inner.completed_tools.retain(|key| {
            !(key.connection_id == turn.connection_id
                && key.connection_incarnation == turn.connection_incarnation
                && key.turn_generation == turn.turn_generation)
        });
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
                    let seq = self.transition_seq.fetch_add(1, Ordering::Relaxed) + 1;
                    let lease = inner.leases.get_mut(&id).expect("lease present");
                    lease.phase = ToolLeasePhase::Warning;
                    lease.bump_transition(at, seq);
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
                    let seq = self.transition_seq.fetch_add(1, Ordering::Relaxed) + 1;
                    let lease = inner.leases.get_mut(&id).expect("lease present");
                    lease.phase = ToolLeasePhase::Cancelling;
                    lease.cancel_cause = Some(CancelCause::AutoTimeout);
                    // Transition wall time for later TimedOut/Cancelling projections.
                    lease.bump_transition(at, seq);
                    let claim = CancellationClaim {
                        stamp: lease.stamp(),
                        capability: lease.capability.clone(),
                        cause: CancelCause::AutoTimeout,
                    };
                    let projection = lease.to_projection(ToolWatchdogPhase::Cancelling);
                    actions.push(RegistryAction::ClaimCancel { claim, projection });
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
        let settings_grace = inner.settings.grace_seconds;
        let lease = inner.leases.get_mut(lease_id).ok_or(StaleLease)?;
        if lease.version != version {
            return Err(StaleLease);
        }
        if !matches!(lease.phase, ToolLeasePhase::Warning) {
            return Err(StaleLease);
        }
        // Fallback grace is fixed at DEFAULT_GRACE_SECS, independent of live settings.
        let grace_secs = if lease.is_fallback {
            DEFAULT_GRACE_SECS
        } else {
            settings_grace
        };
        lease.phase = ToolLeasePhase::Grace;
        lease.warning_emitted_at = Some(at);
        lease.captured_grace_secs = Some(grace_secs);
        lease.grace_deadline = Some(at.advanced(grace_secs as u64));
        let seq = self.transition_seq.fetch_add(1, Ordering::Relaxed) + 1;
        lease.bump_transition(at, seq);
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
        let seq = self.transition_seq.fetch_add(1, Ordering::Relaxed) + 1;
        lease.bump_transition(at, seq);
        Ok(lease.to_projection(ToolWatchdogPhase::Grace))
    }

    /// CAS into Cancelling. Returns the claim **and** the Cancelling projection
    /// under the same lock so callers never re-lookup after a concurrent settle.
    pub async fn claim_cancel(
        &self,
        lease_id: &str,
        version: u64,
        cause: CancelCause,
    ) -> Result<(CancellationClaim, ToolWatchdogProjection), StaleLease> {
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
        let seq = self.transition_seq.fetch_add(1, Ordering::Relaxed) + 1;
        lease.bump_transition(WatchdogInstant::now(), seq);
        let claim = CancellationClaim {
            stamp: lease.stamp(),
            capability: lease.capability.clone(),
            cause,
        };
        let projection = lease.to_projection(ToolWatchdogPhase::Cancelling);
        Ok((claim, projection))
    }

    /// Settle a Cancelling lease as TimedOut and remove it from the live map.
    ///
    /// Losers (missing / wrong version / not Cancelling) receive `StaleLease`.
    /// Never reverts to Running and never auto-retries.
    pub async fn settle_cancel(
        &self,
        lease_id: &str,
        expected_version: u64,
        scope: CancellationScope,
        error_code: &str,
    ) -> Result<ToolWatchdogProjection, StaleLease> {
        let mut inner = self.inner.lock().await;
        let lease = inner.leases.get_mut(lease_id).ok_or(StaleLease)?;
        if lease.version != expected_version {
            return Err(StaleLease);
        }
        if !matches!(lease.phase, ToolLeasePhase::Cancelling) {
            return Err(StaleLease);
        }
        lease.phase = ToolLeasePhase::TimedOut;
        let seq = self.transition_seq.fetch_add(1, Ordering::Relaxed) + 1;
        lease.bump_transition(WatchdogInstant::now(), seq);
        let mut projection = lease.to_projection(ToolWatchdogPhase::TimedOut);
        projection.cancellation_scope = Some(scope);
        projection.error_code = Some(error_code.to_string());

        let tool_call_id = lease.tool_call_id.clone();
        let connection_id = lease.connection_id.clone();
        let connection_incarnation = lease.connection_incarnation.clone();
        let turn_generation = lease.turn_generation;
        let is_fallback = lease.is_fallback;

        inner.leases.remove(lease_id);
        if let Some(tool_id) = tool_call_id {
            let key = ToolLeaseKey {
                connection_id: connection_id.clone(),
                connection_incarnation: connection_incarnation.clone(),
                turn_generation,
                tool_call_id: tool_id,
            };
            inner.tool_index.remove(&key);
            inner.completed_tools.insert(key);
        }
        if is_fallback {
            let turn_key = TurnKey {
                connection_id,
                connection_incarnation,
                turn_generation,
            };
            if let Some(rec) = inner.turns.get_mut(&turn_key) {
                if rec.fallback_lease_id.as_deref() == Some(lease_id) {
                    rec.fallback_lease_id = None;
                }
            }
        }
        Ok(projection)
    }

    /// Whether the lease is still present in the live map (any phase).
    pub async fn is_live(&self, lease_id: &str) -> bool {
        self.inner.lock().await.leases.contains_key(lease_id)
    }

    pub async fn remove_connection(
        &self,
        connection_id: &str,
        incarnation: &str,
    ) -> Vec<ToolWatchdogProjection> {
        let mut inner = self.inner.lock().await;
        // Fence under the same lock as clear so a concurrent register cannot
        // recreate leases between admission close and lease removal.
        inner
            .fenced
            .insert(IncarnationKey::new(connection_id, incarnation));
        let mut cleared = Vec::new();
        let ids: Vec<String> = inner
            .leases
            .values()
            .filter(|l| l.connection_id == connection_id && l.connection_incarnation == incarnation)
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
                let seq = self.transition_seq.fetch_add(1, Ordering::Relaxed) + 1;
                lease.bump_transition(WatchdogInstant::now(), seq);
                cleared.push(lease.to_projection(ToolWatchdogPhase::Cleared));
            }
        }
        let turn_keys: Vec<TurnKey> = inner
            .turns
            .keys()
            .filter(|k| k.connection_id == connection_id && k.connection_incarnation == incarnation)
            .cloned()
            .collect();
        for k in turn_keys {
            inner.turns.remove(&k);
        }
        // Drop completed-key tombstones for this connection/incarnation.
        inner.completed_tools.retain(|key| {
            !(key.connection_id == connection_id && key.connection_incarnation == incarnation)
        });
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

    /// Test/host helper: count of completed-key tombstones for a generation.
    pub async fn completed_tool_tombstone_count(&self, turn: &TurnStamp) -> usize {
        let inner = self.inner.lock().await;
        inner
            .completed_tools
            .iter()
            .filter(|key| {
                key.connection_id == turn.connection_id
                    && key.connection_incarnation == turn.connection_incarnation
                    && key.turn_generation == turn.turn_generation
            })
            .count()
    }

    /// Test/host helper: whether a logical tool key is currently tombstoned.
    pub async fn has_completed_tool_tombstone(&self, key: &ToolLeaseKey) -> bool {
        let inner = self.inner.lock().await;
        inner.completed_tools.contains(key)
    }
}

/// Demote Warning/Grace (or refresh Running) to Running. Returns a Cleared
/// projection when the lease was publicly actionable so hosts can drop it from
/// the attach replay map.
fn renew_lease_to_running(
    lease: &mut LeaseRecord,
    at: WatchdogInstant,
    seq: u64,
) -> Option<ToolWatchdogProjection> {
    let demoted_actionable = matches!(lease.phase, ToolLeasePhase::Warning | ToolLeasePhase::Grace);
    lease.phase = ToolLeasePhase::Running;
    lease.last_progress_at = at;
    lease.clear_warning_fields();
    if demoted_actionable {
        lease.bump_transition(at, seq);
        Some(lease.to_projection(ToolWatchdogPhase::Cleared))
    } else {
        let _ = seq;
        lease.bump();
        None
    }
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
    /// Remove a Cancelling lease as TimedOut with the claim's error code.
    ///
    /// Caller must have already verified the lease is Cancelling (and may
    /// have incremented `late_activity`). Does not re-arm fallback.
    fn settle_cancelling_locked(&mut self, lease_id: &str, seq: u64) -> ToolWatchdogProjection {
        let lease = self
            .leases
            .get_mut(lease_id)
            .expect("Cancelling lease present for settle");
        let scope = lease.cancellation_scope();
        let error_code = match lease.cancel_cause {
            Some(CancelCause::UserStop) => ERROR_CODE_USER_CANCELLED,
            Some(CancelCause::AutoTimeout) | None => ERROR_CODE_TOOL_STALLED_TIMEOUT,
        };
        lease.phase = ToolLeasePhase::TimedOut;
        lease.bump_transition(WatchdogInstant::now(), seq);
        let mut projection = lease.to_projection(ToolWatchdogPhase::TimedOut);
        projection.cancellation_scope = Some(scope);
        projection.error_code = Some(error_code.to_string());

        let tool_call_id = lease.tool_call_id.clone();
        let connection_id = lease.connection_id.clone();
        let connection_incarnation = lease.connection_incarnation.clone();
        let turn_generation = lease.turn_generation;
        let is_fallback = lease.is_fallback;

        self.leases.remove(lease_id);
        if let Some(tool_id) = tool_call_id {
            let key = ToolLeaseKey {
                connection_id: connection_id.clone(),
                connection_incarnation: connection_incarnation.clone(),
                turn_generation,
                tool_call_id: tool_id,
            };
            self.tool_index.remove(&key);
            self.completed_tools.insert(key);
        }
        if is_fallback {
            let turn_key = TurnKey {
                connection_id,
                connection_incarnation,
                turn_generation,
            };
            if let Some(rec) = self.turns.get_mut(&turn_key) {
                if rec.fallback_lease_id.as_deref() == Some(lease_id) {
                    rec.fallback_lease_id = None;
                }
            }
        }
        projection
    }

    fn turn_is_fallback_eligible(&self, turn_key: &TurnKey) -> bool {
        let Some(turn) = self.turns.get(turn_key) else {
            return false;
        };
        // Cancelling tracked leases still block re-arm until settled/removed.
        let has_tracked = self.leases.values().any(|l| {
            !l.is_fallback
                && l.connection_id == turn_key.connection_id
                && l.connection_incarnation == turn_key.connection_incarnation
                && l.turn_generation == turn_key.turn_generation
                && l.is_tracked_present()
        });
        fallback_eligible(
            turn.is_prompting,
            has_tracked,
            turn.pending_permission,
            turn.pending_user_input,
            turn.verified_background_work,
        )
    }

    /// Remove the untracked fallback for `turn_key`. When the fallback was
    /// still Warning/Grace/Cancelling, returns a Cleared projection at a
    /// bumped version so attach snapshots drop it (the lease is gone and
    /// `complete_turn` cannot clear it later).
    fn retire_fallback(&mut self, turn_key: &TurnKey, seq: u64) -> Option<ToolWatchdogProjection> {
        let turn = self.turns.get_mut(turn_key)?;
        let id = turn.fallback_lease_id.take()?;
        let mut lease = self.leases.remove(&id)?;
        if matches!(
            lease.phase,
            ToolLeasePhase::Warning | ToolLeasePhase::Grace | ToolLeasePhase::Cancelling
        ) {
            lease.bump_transition(WatchdogInstant::now(), seq);
            Some(lease.to_projection(ToolWatchdogPhase::Cleared))
        } else {
            let _ = seq;
            None
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
            last_transition_at: at,
            transition_seq: 0,
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
        seq: u64,
    ) -> Option<ToolProgressApply> {
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

        let cleared = renew_lease_to_running(lease, at, seq);
        Some(ToolProgressApply {
            stamp: lease.stamp(),
            cleared,
        })
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

    #[test]
    fn wall_rfc3339_uses_millisecond_precision() {
        let t0 = clock_base();
        let early = t0.advanced_millis(100);
        let late = t0.advanced_millis(900);
        assert_eq!(early.wall_rfc3339(), "2026-07-22T12:00:00.100Z");
        assert_eq!(late.wall_rfc3339(), "2026-07-22T12:00:00.900Z");
        // Same wall second prefix; millis distinguish concurrent transitions.
        assert_eq!(&early.wall_rfc3339()[..19], "2026-07-22T12:00:00");
        assert_eq!(&late.wall_rfc3339()[..19], "2026-07-22T12:00:00");
        assert!(early.wall_rfc3339() < late.wall_rfc3339());
    }

    #[tokio::test]
    async fn same_second_projections_order_by_transition_millis() {
        use crate::acp::tool_watchdog::is_newer_diagnostic;

        let reg = ToolExecutionLeaseRegistry::new(ToolWatchdogSettings::default());
        let turn = sample_turn();
        let t0 = clock_base();
        reg.start_turn(turn.clone(), t0).await;
        let old_stamp = register_running_tool(&reg, &turn, "tool-old", t0).await;
        let new_stamp = register_running_tool(&reg, &turn, "tool-new", t0).await;

        // Scan past threshold so both enter Warning (transition stamps equal on
        // this scan). Enter Grace at distinct sub-second instants in the same
        // wall second — production used to serialize those as identical seconds.
        let scan_at = t0.advanced(600);
        let actions = reg.scan(scan_at).await;
        assert_eq!(actions.len(), 2, "both tools should publish warning");

        let mut stamps_by_lease = std::collections::HashMap::new();
        for action in &actions {
            match action {
                RegistryAction::PublishWarning { stamp, .. } => {
                    stamps_by_lease.insert(stamp.lease_id.clone(), stamp.clone());
                }
                other => panic!("expected PublishWarning, got {other:?}"),
            }
        }
        let old_w = stamps_by_lease
            .get(&old_stamp.lease_id)
            .expect("old warning stamp");
        let new_w = stamps_by_lease
            .get(&new_stamp.lease_id)
            .expect("new warning stamp");

        let grace_old_at = t0.advanced(600).advanced_millis(100);
        let grace_new_at = t0.advanced(600).advanced_millis(900);
        let older = reg
            .warning_published(&old_w.lease_id, old_w.version, grace_old_at)
            .await
            .expect("old grace");
        let newer = reg
            .warning_published(&new_w.lease_id, new_w.version, grace_new_at)
            .await
            .expect("new grace");

        assert_eq!(older.transition_at, "2026-07-22T12:10:00.100Z");
        assert_eq!(newer.transition_at, "2026-07-22T12:10:00.900Z");
        // Same wall-second prefix; only millis distinguish them.
        assert_eq!(&older.transition_at[..19], "2026-07-22T12:10:00");
        assert_eq!(&newer.transition_at[..19], "2026-07-22T12:10:00");
        assert!(
            is_newer_diagnostic(&newer, &older),
            "same-second concurrent transitions must order by millis, not lease-local grace"
        );
    }

    /// One scan stamps both leases with identical wall millis; host-global
    /// `transition_seq` still distinguishes apply order.
    #[tokio::test]
    async fn equal_millis_scan_assigns_monotonic_transition_seq() {
        use crate::acp::tool_watchdog::is_newer_diagnostic;

        let reg = ToolExecutionLeaseRegistry::new(ToolWatchdogSettings::default());
        let turn = sample_turn();
        let t0 = clock_base();
        reg.start_turn(turn.clone(), t0).await;
        let _old = register_running_tool(&reg, &turn, "tool-a", t0).await;
        let _new = register_running_tool(&reg, &turn, "tool-z", t0).await;

        let scan_at = t0.advanced(600);
        let actions = reg.scan(scan_at).await;
        assert_eq!(actions.len(), 2);

        let mut projections = Vec::new();
        for action in actions {
            match action {
                RegistryAction::PublishWarning { projection, .. } => {
                    projections.push(projection);
                }
                other => panic!("expected PublishWarning, got {other:?}"),
            }
        }
        assert_eq!(projections[0].transition_at, projections[1].transition_at);
        assert_ne!(
            projections[0].transition_seq, projections[1].transition_seq,
            "equal-millis scan peers must receive distinct transition_seq"
        );
        let (first, second) = if projections[0].transition_seq < projections[1].transition_seq {
            (&projections[0], &projections[1])
        } else {
            (&projections[1], &projections[0])
        };
        assert!(
            is_newer_diagnostic(second, first),
            "higher transition_seq must win on equal millis"
        );
        assert!(!is_newer_diagnostic(first, second));
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
        .expect("register_tool should admit while generation is Prompting")
        .stamp
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
        let RegistryAction::PublishWarning { stamp: wstamp, .. } = &actions[0] else {
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
            RegistryAction::ClaimCancel { claim, projection } => {
                assert_eq!(claim.cause, CancelCause::AutoTimeout);
                assert_eq!(claim.stamp.lease_id, stamp.lease_id);
                assert_eq!(projection.phase, ToolWatchdogPhase::Cancelling);
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
                SemanticProgress::TerminalOffset {
                    terminal_id_hash: None,
                    next_offset: 1,
                },
                t0.advanced(601),
            )
            .await;
        let renewed = renewed.expect("progress renews warning");
        assert!(
            renewed.cleared.is_some(),
            "Warning→Running must yield Cleared for attach map"
        );
        assert_eq!(
            renewed.cleared.as_ref().unwrap().phase,
            ToolWatchdogPhase::Cleared
        );
        assert_eq!(renewed.cleared.as_ref().unwrap().version, renewed.version);
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
                SemanticProgress::TerminalOffset {
                    terminal_id_hash: None,
                    next_offset: 2,
                },
                t0.advanced(601 + 650),
            )
            .await
            .expect("progress renews grace");
        assert!(
            renewed2.cleared.is_some(),
            "Grace→Running must yield Cleared for attach map"
        );
        assert_eq!(renewed2.cleared.as_ref().unwrap().version, renewed2.version);
        assert!(reg.actionable_projections().await.is_empty());
        let _ = wstamp;
    }

    /// Running progress renews without a Cleared projection (nothing to drop).
    #[tokio::test]
    async fn progress_from_running_does_not_emit_cleared() {
        let reg = ToolExecutionLeaseRegistry::new(ToolWatchdogSettings::default());
        let turn = sample_turn();
        let t0 = clock_base();
        reg.start_turn(turn.clone(), t0).await;
        let _ = register_running_tool(&reg, &turn, "tool-1", t0).await;
        let apply = reg
            .record_tool_progress_at(
                progress_key(&turn, "tool-1"),
                SemanticProgress::TerminalOffset {
                    terminal_id_hash: None,
                    next_offset: 1,
                },
                t0.advanced(1),
            )
            .await
            .expect("renew");
        assert!(
            apply.cleared.is_none(),
            "Running→Running must not invent Cleared"
        );
    }

    /// Delegated child activity (semantic progress) demotes Grace and returns
    /// Cleared so the production event-emitter path can clear the attach map.
    #[tokio::test]
    async fn delegation_activity_in_grace_returns_cleared() {
        let reg = ToolExecutionLeaseRegistry::new(ToolWatchdogSettings::default());
        let turn = sample_turn();
        let t0 = clock_base();
        reg.start_turn(turn.clone(), t0).await;
        let stamp = register_running_tool(&reg, &turn, "parent-tool", t0).await;

        let actions = reg.scan(t0.advanced(600)).await;
        let RegistryAction::PublishWarning { stamp: w, .. } = &actions[0] else {
            panic!("warn");
        };
        let grace = reg
            .warning_published(&w.lease_id, w.version, t0.advanced(600))
            .await
            .unwrap();
        assert_eq!(grace.phase, ToolWatchdogPhase::Grace);
        assert_eq!(grace.lease_id, stamp.lease_id);

        let apply = reg
            .record_delegation_activity(progress_key(&turn, "parent-tool"), t0.advanced(650))
            .await
            .expect("child activity renews parent");
        let cleared = apply
            .cleared
            .as_ref()
            .expect("Grace→Running must yield Cleared");
        assert_eq!(cleared.phase, ToolWatchdogPhase::Cleared);
        assert_eq!(cleared.lease_id, stamp.lease_id);
        assert_eq!(cleared.version, apply.version);
        assert!(reg.actionable_projections().await.is_empty());
        assert_eq!(
            reg.lease_phase(&stamp.lease_id).await,
            Some(ToolLeasePhase::Running)
        );
    }

    /// Wall-clock ms as progress token rejects renewals on clock rollback.
    /// Per-lease monotonic allocation must still renew when `at.wall` moves
    /// backward (or when two activities share the same wall instant).
    #[tokio::test]
    async fn delegation_activity_renews_despite_wall_clock_rollback() {
        let reg = ToolExecutionLeaseRegistry::new(ToolWatchdogSettings::default());
        let turn = sample_turn();
        let t0 = clock_base();
        reg.start_turn(turn.clone(), t0).await;
        let stamp = register_running_tool(&reg, &turn, "parent-tool", t0).await;
        let key = progress_key(&turn, "parent-tool");

        let first = reg
            .record_delegation_activity(key.clone(), t0.advanced(10))
            .await
            .expect("first child activity renews");
        assert!(first.version > stamp.version);

        // Wall clock rolls back relative to the prior progress instant.
        let rolled_back = WatchdogInstant {
            mono: t0.mono + std::time::Duration::from_secs(20),
            wall: t0.wall - chrono::Duration::seconds(30),
        };
        let second = reg
            .record_delegation_activity(key, rolled_back)
            .await
            .expect("second activity must renew despite wall rollback");
        assert!(
            second.version > first.version,
            "monotonic progress token must advance past prior renewals"
        );
        assert_eq!(
            reg.lease_phase(&stamp.lease_id).await,
            Some(ToolLeasePhase::Running)
        );
    }

    /// Settings disable demotes Warning/Grace and returns Cleared projections.
    #[tokio::test]
    async fn apply_settings_disable_returns_cleared_for_grace() {
        let reg = ToolExecutionLeaseRegistry::new(ToolWatchdogSettings::default());
        let turn = sample_turn();
        let t0 = clock_base();
        reg.start_turn(turn.clone(), t0).await;
        let stamp = register_running_tool(&reg, &turn, "tool-1", t0).await;

        let actions = reg.scan(t0.advanced(600)).await;
        let RegistryAction::PublishWarning { stamp: w, .. } = &actions[0] else {
            panic!("warn");
        };
        let _ = reg
            .warning_published(&w.lease_id, w.version, t0.advanced(600))
            .await
            .unwrap();

        let cleared = reg
            .apply_settings(ToolWatchdogSettings {
                enabled: false,
                warning_after_seconds: 600,
                grace_seconds: 600,
            })
            .await;
        assert_eq!(cleared.len(), 1);
        assert_eq!(cleared[0].phase, ToolWatchdogPhase::Cleared);
        assert_eq!(cleared[0].lease_id, stamp.lease_id);
        assert!(reg.actionable_projections().await.is_empty());
        assert_eq!(
            reg.lease_phase(&stamp.lease_id).await,
            Some(ToolLeasePhase::Running)
        );
    }

    /// pause_turn demotes Grace to Paused and returns Cleared for the attach map.
    #[tokio::test]
    async fn pause_turn_returns_cleared_for_grace_leases() {
        let reg = ToolExecutionLeaseRegistry::new(ToolWatchdogSettings::default());
        let turn = sample_turn();
        let t0 = clock_base();
        reg.start_turn(turn.clone(), t0).await;
        let stamp = register_running_tool(&reg, &turn, "tool-1", t0).await;

        let actions = reg.scan(t0.advanced(600)).await;
        let RegistryAction::PublishWarning { stamp: w, .. } = &actions[0] else {
            panic!("warn");
        };
        let _ = reg
            .warning_published(&w.lease_id, w.version, t0.advanced(600))
            .await
            .unwrap();

        let cleared = reg.pause_turn(&turn, PauseReason::Permission).await;
        assert_eq!(cleared.len(), 1);
        assert_eq!(cleared[0].phase, ToolWatchdogPhase::Cleared);
        assert_eq!(cleared[0].lease_id, stamp.lease_id);
        assert!(reg.actionable_projections().await.is_empty());
        assert!(matches!(
            reg.lease_phase(&stamp.lease_id).await,
            Some(ToolLeasePhase::Paused {
                reason: PauseReason::Permission
            })
        ));
    }

    /// First tracked-tool admission retires a Grace fallback with Cleared
    /// (complete_turn cannot clear a lease that was already removed).
    #[tokio::test]
    async fn register_tool_retires_grace_fallback_with_cleared() {
        let reg = ToolExecutionLeaseRegistry::new(ToolWatchdogSettings::default());
        let turn = sample_turn();
        let t0 = clock_base();
        reg.start_turn(turn.clone(), t0).await;
        let fb = reg.fallback_stamp(&turn).await.expect("fallback at start");

        // Drive fallback to Grace via the fixed 1800s untracked threshold.
        let warn_at = t0.advanced(1_800);
        let actions = reg.scan(warn_at).await;
        let RegistryAction::PublishWarning { stamp: w, .. } = &actions[0] else {
            panic!("fallback warn: {actions:?}");
        };
        assert_eq!(w.lease_id, fb.lease_id);
        let grace = reg
            .warning_published(&w.lease_id, w.version, warn_at)
            .await
            .unwrap();
        assert_eq!(grace.phase, ToolWatchdogPhase::Grace);

        let outcome = reg
            .register_tool(RegisterTool {
                turn: turn.clone(),
                tool_call_id: "first-tracked".into(),
                category: ToolCategory::Terminal,
                at: warn_at.advanced(1),
            })
            .await
            .expect("first tracked tool admits");
        let cleared = outcome
            .cleared
            .expect("retiring Grace fallback must yield Cleared");
        assert_eq!(cleared.phase, ToolWatchdogPhase::Cleared);
        assert_eq!(cleared.lease_id, fb.lease_id);
        assert!(cleared.version > grace.version);
        assert!(!reg.has_fallback(&turn).await);
        assert!(
            reg.lease_phase(&fb.lease_id).await.is_none(),
            "fallback lease removed; Cleared is the only way attach drops it"
        );
        assert!(reg.actionable_projections().await.is_empty());
    }

    /// complete_tool Cleared + progress Cleared both reconcile SessionState map.
    #[tokio::test]
    async fn grace_progress_and_complete_clear_projections_for_replay_map() {
        let reg = ToolExecutionLeaseRegistry::new(ToolWatchdogSettings::default());
        let turn = sample_turn();
        let t0 = clock_base();
        reg.start_turn(turn.clone(), t0).await;
        let stamp = register_running_tool(&reg, &turn, "tool-a", t0).await;

        // Drive to Grace.
        let actions = reg.scan(t0.advanced(600)).await;
        let RegistryAction::PublishWarning { stamp: w, .. } = &actions[0] else {
            panic!("warn");
        };
        let grace = reg
            .warning_published(&w.lease_id, w.version, t0.advanced(600))
            .await
            .unwrap();
        assert_eq!(grace.phase, ToolWatchdogPhase::Grace);

        // Path 1: progress demotes Grace → Running with Cleared.
        let progress_clear = reg
            .record_tool_progress_at(
                progress_key(&turn, "tool-a"),
                SemanticProgress::ToolStatusChanged {
                    status_fingerprint: 99,
                },
                t0.advanced(650),
            )
            .await
            .expect("status progress")
            .cleared
            .expect("cleared on grace progress");
        assert_eq!(progress_clear.phase, ToolWatchdogPhase::Cleared);
        assert_eq!(progress_clear.lease_id, stamp.lease_id);

        // Re-enter Grace then complete normally → Cleared without error_code.
        let actions2 = reg.scan(t0.advanced(650 + 600)).await;
        let RegistryAction::PublishWarning { stamp: w2, .. } = &actions2[0] else {
            panic!("warn2");
        };
        let _ = reg
            .warning_published(&w2.lease_id, w2.version, t0.advanced(650 + 600))
            .await
            .unwrap();
        let complete_clear = reg
            .complete_tool(&tool_key(&turn, "tool-a"))
            .await
            .expect("complete");
        assert_eq!(complete_clear.phase, ToolWatchdogPhase::Cleared);
        assert!(complete_clear.error_code.is_none());
        assert!(reg.lease_phase(&stamp.lease_id).await.is_none());
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
                SemanticProgress::TerminalOffset {
                    terminal_id_hash: None,
                    next_offset: 10,
                },
                t0.advanced(10),
            )
            .await;
        assert!(first.is_some());
        let v1 = first.unwrap().version;

        let dup = reg
            .record_tool_progress_at(
                key.clone(),
                SemanticProgress::TerminalOffset {
                    terminal_id_hash: None,
                    next_offset: 10,
                },
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

        // Case B: user stop wins; late completion settles as user_cancelled
        // (never Cleared) and removes the lease so convergence succeeds.
        let reg = ToolExecutionLeaseRegistry::new(ToolWatchdogSettings::default());
        reg.start_turn(turn.clone(), t0).await;
        let stamp = register_running_tool(&reg, &turn, "tool-b", t0).await;
        let (claim, claim_projection) = reg
            .claim_cancel(&stamp.lease_id, stamp.version, CancelCause::UserStop)
            .await
            .unwrap();
        assert_eq!(claim.cause, CancelCause::UserStop);
        assert_eq!(claim_projection.phase, ToolWatchdogPhase::Cancelling);
        assert_eq!(claim_projection.version, claim.stamp.version);
        assert_eq!(
            reg.lease_phase(&stamp.lease_id).await,
            Some(ToolLeasePhase::Cancelling)
        );
        let settled = reg
            .complete_tool(&tool_key(&turn, "tool-b"))
            .await
            .expect("settle cancel projection");
        // After settle the lease is gone — live re-lookup would miss, but the
        // atomic claim projection remains the emit-safe Cancelling snapshot.
        assert!(reg.live_projection(&claim.stamp.lease_id).await.is_none());
        assert_eq!(claim_projection.phase, ToolWatchdogPhase::Cancelling);
        assert_eq!(settled.phase, ToolWatchdogPhase::TimedOut);
        assert_eq!(
            settled.error_code.as_deref(),
            Some(ERROR_CODE_USER_CANCELLED)
        );
        assert!(!reg.is_live(&stamp.lease_id).await);
        // Progress after settle cannot revive a removed lease.
        let prog = reg
            .record_tool_progress_at(
                progress_key(&turn, "tool-b"),
                SemanticProgress::TerminalOffset {
                    terminal_id_hash: None,
                    next_offset: 99,
                },
                t0.advanced(1),
            )
            .await;
        assert!(prog.is_none());
        assert_eq!(reg.lease_phase(&stamp.lease_id).await, None);

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
        let RegistryAction::ClaimCancel { claim, projection } = &cancel[0] else {
            panic!("cancel");
        };
        assert_eq!(claim.cause, CancelCause::AutoTimeout);
        assert_eq!(projection.phase, ToolWatchdogPhase::Cancelling);
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
        assert!(reg.warning_published("missing", 1, t0).await.is_err());
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
        let RegistryAction::ClaimCancel { claim, projection } = &cancel[0] else {
            panic!("cancel");
        };
        assert_eq!(claim.capability, CancellationCapability::Turn);
        assert_eq!(claim.stamp.lease_id, stamp.lease_id);
        assert_eq!(projection.phase, ToolWatchdogPhase::Cancelling);

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
        let (claim2, claim2_projection) = reg2
            .claim_cancel(&bound.lease_id, bound.version, CancelCause::UserStop)
            .await
            .unwrap();
        assert_eq!(claim2_projection.phase, ToolWatchdogPhase::Cancelling);
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
                },
                ..
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
        assert!(matches!(actions[0], RegistryAction::PublishWarning { .. }));
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
        .await
        .expect("register mcp tool");
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

    #[tokio::test]
    async fn untracked_fallback_grace_ignores_live_grace_seconds_setting() {
        // Live tracked grace is 60s; fallback must still use DEFAULT_GRACE_SECS (600).
        let reg = ToolExecutionLeaseRegistry::new(ToolWatchdogSettings {
            enabled: true,
            warning_after_seconds: 60,
            grace_seconds: 60,
        });
        let turn = sample_turn();
        let t0 = clock_base();
        reg.start_turn(turn.clone(), t0).await;

        let actions = reg.scan(t0.advanced(1_800)).await;
        let RegistryAction::PublishWarning { stamp, .. } = &actions[0] else {
            panic!("expected fallback warning");
        };
        let warn_at = t0.advanced(1_800);
        let grace = reg
            .warning_published(&stamp.lease_id, stamp.version, warn_at)
            .await
            .unwrap();
        assert_eq!(grace.phase, ToolWatchdogPhase::Grace);
        assert_eq!(
            grace.grace_deadline.as_deref(),
            Some(
                warn_at
                    .advanced(DEFAULT_GRACE_SECS as u64)
                    .wall_rfc3339()
                    .as_str()
            ),
            "fallback grace must be DEFAULT_GRACE_SECS, not live 60s"
        );

        // Live 60s grace would cancel here; fixed 600 must not.
        assert!(
            reg.scan(warn_at.advanced(60)).await.is_empty(),
            "must not cancel at live grace_seconds=60"
        );
        assert!(reg.scan(warn_at.advanced(599)).await.is_empty());
        let cancel = reg.scan(warn_at.advanced(DEFAULT_GRACE_SECS as u64)).await;
        assert!(matches!(
            cancel.as_slice(),
            [RegistryAction::ClaimCancel {
                claim: CancellationClaim {
                    cause: CancelCause::AutoTimeout,
                    capability: CancellationCapability::Turn,
                    ..
                },
                ..
            }]
        ));
    }

    #[tokio::test]
    async fn complete_turn_settles_cancelling_lease_as_user_cancelled() {
        let reg = ToolExecutionLeaseRegistry::new(ToolWatchdogSettings::default());
        let turn = sample_turn();
        let t0 = clock_base();
        reg.start_turn(turn.clone(), t0).await;
        let stamp = register_running_tool(&reg, &turn, "tool-c", t0).await;
        let (claim, claim_projection) = reg
            .claim_cancel(&stamp.lease_id, stamp.version, CancelCause::UserStop)
            .await
            .unwrap();
        assert_eq!(claim.cause, CancelCause::UserStop);
        assert_eq!(claim_projection.phase, ToolWatchdogPhase::Cancelling);
        assert_eq!(
            reg.lease_phase(&stamp.lease_id).await,
            Some(ToolLeasePhase::Cancelling)
        );

        let projections = reg.complete_turn(&turn).await;
        assert_eq!(projections.len(), 1);
        assert_eq!(projections[0].phase, ToolWatchdogPhase::TimedOut);
        assert_eq!(
            projections[0].error_code.as_deref(),
            Some(ERROR_CODE_USER_CANCELLED),
            "complete_turn must not emit Cleared for Cancelling"
        );
        assert!(
            !reg.is_live(&stamp.lease_id).await,
            "settled Cancelling lease must leave the live map"
        );
    }

    #[tokio::test]
    async fn cancelling_tracked_lease_blocks_fallback_rearm() {
        let reg = ToolExecutionLeaseRegistry::new(ToolWatchdogSettings::default());
        let turn = sample_turn();
        let t0 = clock_base();
        reg.start_turn(turn.clone(), t0).await;
        let stamp = register_running_tool(&reg, &turn, "tool-1", t0).await;
        assert!(!reg.has_fallback(&turn).await);

        let _ = reg
            .claim_cancel(&stamp.lease_id, stamp.version, CancelCause::AutoTimeout)
            .await
            .unwrap();
        assert_eq!(
            reg.lease_phase(&stamp.lease_id).await,
            Some(ToolLeasePhase::Cancelling)
        );
        // While Cancelling, tracked lease still blocks fallback re-arm.
        assert!(!reg.has_fallback(&turn).await);
        reg.resume_turn(&turn, t0.advanced(1)).await;
        assert!(
            !reg.has_fallback(&turn).await,
            "resume must not re-arm fallback while Cancelling tracked lease exists"
        );
        reg.set_verified_background_work(&turn, false).await;
        assert!(
            !reg.has_fallback(&turn).await,
            "background clear must not re-arm while Cancelling tracked lease exists"
        );
        // complete_tool settles cancel (TimedOut) — still not Cleared.
        let settled = reg
            .complete_tool(&tool_key(&turn, "tool-1"))
            .await
            .expect("settle");
        assert_eq!(settled.phase, ToolWatchdogPhase::TimedOut);
        assert_eq!(
            settled.error_code.as_deref(),
            Some(ERROR_CODE_TOOL_STALLED_TIMEOUT)
        );
        assert!(!reg.is_live(&stamp.lease_id).await);
    }

    #[tokio::test]
    async fn duplicate_agent_activity_hash_does_not_postpone_fallback_rearm() {
        let reg = ToolExecutionLeaseRegistry::new(ToolWatchdogSettings::default());
        let turn = sample_turn();
        let t0 = clock_base();
        reg.start_turn(turn.clone(), t0).await;
        let _stamp = register_running_tool(&reg, &turn, "tool-1", t0).await;
        assert!(!reg.has_fallback(&turn).await);

        // First agent hash is a new fact → accepted as re-arm baseline.
        reg.record_turn_progress_at(
            &turn,
            SemanticProgress::AgentActivity { content_hash: 42 },
            t0.advanced(100),
        )
        .await;
        // Duplicate hash while tracked lease is active must not advance baseline.
        reg.record_turn_progress_at(
            &turn,
            SemanticProgress::AgentActivity { content_hash: 42 },
            t0.advanced(500),
        )
        .await;

        let _ = reg.complete_tool(&tool_key(&turn, "tool-1")).await;
        assert!(reg.has_fallback(&turn).await);

        // Rearm baseline must stay at t0+100 (first accepted hash), not t0+500.
        // If the duplicate had postponed the baseline to t0+500, this scan would be empty.
        assert!(
            reg.scan(t0.advanced(100 + 1_799)).await.is_empty(),
            "quiet until 1800s after first accepted agent activity"
        );
        let warn = reg.scan(t0.advanced(100 + 1_800)).await;
        assert!(
            matches!(warn.as_slice(), [RegistryAction::PublishWarning { .. }]),
            "must warn from first accepted agent activity, not postponed duplicate: {warn:?}"
        );
    }

    #[tokio::test]
    async fn actionable_projections_exclude_warning_until_grace() {
        let reg = ToolExecutionLeaseRegistry::new(ToolWatchdogSettings::default());
        let turn = sample_turn();
        let t0 = clock_base();
        reg.start_turn(turn.clone(), t0).await;
        let stamp = register_running_tool(&reg, &turn, "tool-1", t0).await;

        let actions = reg.scan(t0.advanced(600)).await;
        let RegistryAction::PublishWarning {
            stamp: wstamp,
            projection,
        } = &actions[0]
        else {
            panic!("expected PublishWarning");
        };
        assert_eq!(projection.phase, ToolWatchdogPhase::Warning);
        assert_eq!(wstamp.lease_id, stamp.lease_id);

        // Between scan and warning_published: attach/replay must not see Warning.
        assert!(
            reg.actionable_projections().await.is_empty(),
            "Warning is publish-transition only; not actionable for snapshot clients"
        );

        let grace = reg
            .warning_published(&wstamp.lease_id, wstamp.version, t0.advanced(600))
            .await
            .unwrap();
        assert_eq!(grace.phase, ToolWatchdogPhase::Grace);

        let actionable = reg.actionable_projections().await;
        assert_eq!(actionable.len(), 1);
        assert_eq!(actionable[0].phase, ToolWatchdogPhase::Grace);
        assert_eq!(actionable[0].lease_id, stamp.lease_id);
    }

    #[tokio::test]
    async fn double_start_turn_keeps_single_fallback_warning_path() {
        let reg = ToolExecutionLeaseRegistry::new(ToolWatchdogSettings::default());
        let turn = sample_turn();
        let t0 = clock_base();
        reg.start_turn(turn.clone(), t0).await;
        let first_fb = reg
            .fallback_stamp(&turn)
            .await
            .expect("first start_turn registers fallback");

        // Duplicate Prompting admission for the same generation must be idempotent.
        reg.start_turn(turn.clone(), t0.advanced(5)).await;
        let second_fb = reg
            .fallback_stamp(&turn)
            .await
            .expect("fallback still present after second start_turn");
        assert_eq!(
            second_fb.lease_id, first_fb.lease_id,
            "must keep the same fallback_lease_id (no orphaned duplicate)"
        );
        assert_eq!(second_fb.version, first_fb.version);

        // One fallback => one warning path at 1800s, then one cancel after grace.
        let warn = reg.scan(t0.advanced(1_800)).await;
        assert_eq!(
            warn.len(),
            1,
            "duplicate start_turn must not produce two fallback warnings: {warn:?}"
        );
        let RegistryAction::PublishWarning { stamp, .. } = &warn[0] else {
            panic!("expected PublishWarning, got {warn:?}");
        };
        assert_eq!(stamp.lease_id, first_fb.lease_id);

        let warn_at = t0.advanced(1_800);
        let _ = reg
            .warning_published(&stamp.lease_id, stamp.version, warn_at)
            .await
            .expect("enter grace");
        let cancel = reg.scan(warn_at.advanced(DEFAULT_GRACE_SECS as u64)).await;
        assert_eq!(
            cancel.len(),
            1,
            "single cancellation claim for one fallback: {cancel:?}"
        );
        assert!(matches!(
            cancel.as_slice(),
            [RegistryAction::ClaimCancel {
                claim: CancellationClaim {
                    cause: CancelCause::AutoTimeout,
                    capability: CancellationCapability::Turn,
                    ..
                },
                ..
            }]
        ));
    }

    #[tokio::test]
    async fn start_turn_after_complete_does_not_revive_generation() {
        let reg = ToolExecutionLeaseRegistry::new(ToolWatchdogSettings::default());
        let turn = sample_turn();
        let t0 = clock_base();
        reg.start_turn(turn.clone(), t0).await;
        assert!(reg.has_fallback(&turn).await);

        let _ = reg.complete_turn(&turn).await;
        assert!(
            !reg.has_fallback(&turn).await,
            "complete_turn clears fallback for a finished generation"
        );

        // Late / duplicate start must not revive a completed generation.
        reg.start_turn(turn.clone(), t0.advanced(10)).await;
        assert!(
            !reg.has_fallback(&turn).await,
            "start_turn after complete_turn must not re-arm fallback"
        );
        assert!(
            reg.scan(t0.advanced(10 + 1_800)).await.is_empty(),
            "completed generation must not emit a late fallback warning"
        );
        assert!(
            reg.fallback_stamp(&turn).await.is_none(),
            "no fallback lease stamp after completed generation"
        );
    }

    /// Tool registration can race ahead of Prompting admission. The provisional
    /// TurnRecord must not lock `turn_start_at` to the later tool time: first real
    /// `start_turn` merges the admission timestamp so fallback re-arm uses it.
    #[tokio::test]
    async fn tool_first_start_turn_merges_admission_turn_start_at() {
        let reg = ToolExecutionLeaseRegistry::new(ToolWatchdogSettings::default());
        let turn = sample_turn();
        let t0 = clock_base(); // genuine Prompting admission time
        let t1 = t0.advanced(30); // tool registration arrives first

        let stamp = register_running_tool(&reg, &turn, "tool-race", t1).await;
        assert!(
            !reg.has_fallback(&turn).await,
            "tracked tool retires fallback"
        );

        // Admission observes the real start after the provisional tool-created turn.
        reg.start_turn(turn.clone(), t0).await;
        // Tool still live: no fallback yet, and start_turn must not orphan the tool lease.
        assert!(!reg.has_fallback(&turn).await);
        assert_eq!(
            reg.lease_phase(&stamp.lease_id).await,
            Some(ToolLeasePhase::Running),
            "admission merge must not drop the live tool lease"
        );

        let _ = reg
            .complete_tool(&tool_key(&turn, "tool-race"))
            .await
            .expect("complete tracked tool");
        assert!(
            reg.has_fallback(&turn).await,
            "fallback re-arms after last tracked tool completes"
        );
        let fb = reg
            .fallback_stamp(&turn)
            .await
            .expect("fallback lease after re-arm");

        // Must warn from admission t0+1800, not provisional tool time t1+1800.
        assert!(
            reg.scan(t0.advanced(1_799)).await.is_empty(),
            "no warning just before admission-based threshold"
        );
        let warn = reg.scan(t0.advanced(1_800)).await;
        assert_eq!(
            warn.len(),
            1,
            "fallback must warn at t0+1800 (admission), not t1+1800: {warn:?}"
        );
        let RegistryAction::PublishWarning { stamp: wstamp, .. } = &warn[0] else {
            panic!("expected PublishWarning, got {warn:?}");
        };
        assert_eq!(wstamp.lease_id, fb.lease_id);

        // After admission, further start_turn is idempotent (no second fallback).
        let fb_before = fb.lease_id.clone();
        reg.start_turn(turn.clone(), t0.advanced(5)).await;
        let fb_after = reg
            .fallback_stamp(&turn)
            .await
            .expect("fallback retained after duplicate start_turn");
        assert_eq!(
            fb_after.lease_id, fb_before,
            "post-admission start_turn must keep the same fallback lease"
        );
    }

    /// After complete_tool, a replayed provider registration for the same logical
    /// tool key must not resurrect a tracked lease or retire the re-armed fallback.
    /// Only the fallback warning path remains at the fixed 1,800s threshold.
    #[tokio::test]
    async fn completed_tool_key_replay_does_not_resurrect_or_retire_fallback() {
        let reg = ToolExecutionLeaseRegistry::new(ToolWatchdogSettings::default());
        let turn = sample_turn();
        let t0 = clock_base();
        reg.start_turn(turn.clone(), t0).await;

        let _ = register_running_tool(&reg, &turn, "tool-1", t0).await;
        let _ = reg
            .complete_tool(&tool_key(&turn, "tool-1"))
            .await
            .expect("complete tracked tool");
        assert!(
            reg.has_fallback(&turn).await,
            "fallback re-arms after last tracked tool completes"
        );
        let fb = reg
            .fallback_stamp(&turn)
            .await
            .expect("fallback lease after re-arm");

        // Tombstone held while turn still Prompting; same-key replay is Err.
        assert!(
            reg.has_completed_tool_tombstone(&tool_key(&turn, "tool-1"))
                .await,
            "complete_tool must tombstone the logical key while still Prompting"
        );
        assert_eq!(
            reg.completed_tool_tombstone_count(&turn).await,
            1,
            "exactly one generation tombstone after complete_tool"
        );
        let replay = reg
            .register_tool(RegisterTool {
                turn: turn.clone(),
                tool_call_id: "tool-1".into(),
                category: ToolCategory::Terminal,
                at: t0.advanced(1),
            })
            .await;
        assert!(
            replay.is_err(),
            "must reject resurrecting a completed tool key: got {replay:?}"
        );
        assert!(
            reg.has_fallback(&turn).await,
            "replay must not retire the re-armed fallback"
        );
        let fb_after = reg
            .fallback_stamp(&turn)
            .await
            .expect("fallback retained after rejected replay");
        assert_eq!(
            fb_after.lease_id, fb.lease_id,
            "fallback lease identity must be unchanged"
        );

        // No phantom tracked lease: no warning at tracked 600s.
        assert!(
            reg.scan(t0.advanced(600)).await.is_empty(),
            "completed-key replay must not create a tracked warning path"
        );
        // Fallback remains the only warning path at 1,800s.
        let warn = reg.scan(t0.advanced(1_800)).await;
        assert_eq!(
            warn.len(),
            1,
            "only fallback warning expected at 1800s: {warn:?}"
        );
        let RegistryAction::PublishWarning { stamp: wstamp, .. } = &warn[0] else {
            panic!("expected PublishWarning, got {warn:?}");
        };
        assert_eq!(
            wstamp.lease_id, fb.lease_id,
            "warning must come from the original fallback lease"
        );
    }

    /// After complete_turn (!is_prompting), register_tool must not admit a new
    /// lease or invent a warning/cancellation path for that generation.
    #[tokio::test]
    async fn register_tool_after_complete_turn_is_rejected() {
        let reg = ToolExecutionLeaseRegistry::new(ToolWatchdogSettings::default());
        let turn = sample_turn();
        let t0 = clock_base();
        reg.start_turn(turn.clone(), t0).await;
        let _ = reg.complete_turn(&turn).await;
        assert!(
            !reg.has_fallback(&turn).await,
            "complete_turn clears fallback for a finished generation"
        );

        let replay = reg
            .register_tool(RegisterTool {
                turn: turn.clone(),
                tool_call_id: "late-tool".into(),
                category: ToolCategory::Terminal,
                at: t0.advanced(1),
            })
            .await;
        assert!(
            replay.is_err(),
            "must reject register_tool after complete_turn: got {replay:?}"
        );
        assert!(
            !reg.has_fallback(&turn).await,
            "rejected register must not create a fallback"
        );

        assert!(
            reg.scan(t0.advanced(600)).await.is_empty(),
            "no tracked warning after completed generation"
        );
        assert!(
            reg.scan(t0.advanced(1_800)).await.is_empty(),
            "no fallback warning after completed generation"
        );
        assert!(
            reg.scan(t0.advanced(1_800 + 600)).await.is_empty(),
            "no cancellation path after completed generation"
        );
    }

    /// I5: tombstones live only while the generation is still Prompting.
    /// After complete_tool they block same-key replay; after complete_turn they
    /// are reclaimed, and re-register remains rejected via is_prompting=false.
    #[tokio::test]
    async fn complete_turn_reclaims_generation_tombstones_while_still_rejecting_register() {
        let reg = ToolExecutionLeaseRegistry::new(ToolWatchdogSettings::default());
        let turn = sample_turn();
        let t0 = clock_base();
        reg.start_turn(turn.clone(), t0).await;

        let _ = register_running_tool(&reg, &turn, "tool-1", t0).await;
        let _ = reg
            .complete_tool(&tool_key(&turn, "tool-1"))
            .await
            .expect("complete tracked tool");
        assert!(
            reg.has_completed_tool_tombstone(&tool_key(&turn, "tool-1"))
                .await,
            "tombstone must block same-key while turn still Prompting"
        );
        assert!(
            reg.register_tool(RegisterTool {
                turn: turn.clone(),
                tool_call_id: "tool-1".into(),
                category: ToolCategory::Terminal,
                at: t0.advanced(1),
            })
            .await
            .is_err(),
            "tombstone path must reject same-key re-register before turn ends"
        );

        let _ = reg.complete_turn(&turn).await;
        assert_eq!(
            reg.completed_tool_tombstone_count(&turn).await,
            0,
            "complete_turn must clear completed_tools for that generation"
        );
        assert!(
            !reg.has_completed_tool_tombstone(&tool_key(&turn, "tool-1"))
                .await,
            "generation tombstone must be reclaimed after complete_turn"
        );

        // Rejection still holds via is_prompting=false, not via tombstone.
        assert!(
            reg.register_tool(RegisterTool {
                turn: turn.clone(),
                tool_call_id: "tool-1".into(),
                category: ToolCategory::Terminal,
                at: t0.advanced(2),
            })
            .await
            .is_err(),
            "re-register after complete_turn must still be rejected"
        );
        assert!(
            reg.register_tool(RegisterTool {
                turn: turn.clone(),
                tool_call_id: "other-tool".into(),
                category: ToolCategory::Terminal,
                at: t0.advanced(2),
            })
            .await
            .is_err(),
            "any register_tool after complete_turn must be rejected"
        );
        assert_eq!(
            reg.completed_tool_tombstone_count(&turn).await,
            0,
            "rejected post-turn register must not reintroduce tombstones"
        );
    }

    /// Task 5 r3 I1: fence closes admission so late tool events cannot recreate
    /// leases after disconnect clear (even while the map entry may still exist).
    #[tokio::test]
    async fn fence_connection_rejects_register_and_start_turn_after_clear() {
        let reg = ToolExecutionLeaseRegistry::new(ToolWatchdogSettings::default());
        let turn = sample_turn();
        let t0 = clock_base();
        reg.start_turn(turn.clone(), t0).await;
        let stamp = register_running_tool(&reg, &turn, "tool-late", t0).await;

        // Manager order: fence admission, then clear leases.
        reg.fence_connection(&turn.connection_id, &turn.connection_incarnation)
            .await;
        assert!(
            reg.is_fenced(&turn.connection_id, &turn.connection_incarnation)
                .await
        );
        let _ = reg
            .remove_connection(&turn.connection_id, &turn.connection_incarnation)
            .await;
        assert!(reg.lease_phase(&stamp.lease_id).await.is_none());

        // Tool event after fence must no-op registration (pre-fix: recreated lease).
        assert!(
            reg.register_tool(RegisterTool {
                turn: turn.clone(),
                tool_call_id: "tool-late".into(),
                category: ToolCategory::Other,
                at: t0.advanced(1),
            })
            .await
            .is_err(),
            "fenced incarnation must reject register_tool"
        );
        assert!(
            reg.register_tool(RegisterTool {
                turn: turn.clone(),
                tool_call_id: "brand-new-after-fence".into(),
                category: ToolCategory::Other,
                at: t0.advanced(1),
            })
            .await
            .is_err(),
            "fenced incarnation must reject any new tool key"
        );
        // start_turn must not re-admit fallback for the closed incarnation.
        reg.start_turn(turn.clone(), t0.advanced(2)).await;
        assert!(
            !reg.has_fallback(&turn).await,
            "fenced incarnation must reject start_turn admission"
        );
        assert!(
            reg.scan(t0.advanced(10_000)).await.is_empty(),
            "scan must not see recreated leases after fence"
        );
    }

    /// New incarnation is independent of a fenced prior incarnation.
    #[tokio::test]
    async fn fence_does_not_block_new_incarnation() {
        let reg = ToolExecutionLeaseRegistry::new(ToolWatchdogSettings::default());
        let t0 = clock_base();
        let old = sample_turn();
        reg.fence_connection(&old.connection_id, &old.connection_incarnation)
            .await;
        let neu = TurnStamp {
            connection_id: old.connection_id.clone(),
            connection_incarnation: "inc-new".into(),
            session_id: old.session_id.clone(),
            turn_generation: 1,
        };
        reg.start_turn(neu.clone(), t0).await;
        assert!(reg.has_fallback(&neu).await);
        assert!(!register_running_tool(&reg, &neu, "tool-ok", t0)
            .await
            .lease_id
            .is_empty());
    }
}
