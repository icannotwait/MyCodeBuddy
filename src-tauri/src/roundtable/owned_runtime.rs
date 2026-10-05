//! Service-owned room execution. A missing qualified executor is a capability
//! failure, never an empty successful run or a process-cleanup certificate.

use std::collections::HashMap;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;
use std::sync::Mutex;

use async_trait::async_trait;
use roundtable_protocol::*;
use sea_orm::TransactionTrait;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::acp::manager::ConnectionManager;

use super::acceptance::{AcceptInput, CloseInput, PublishInput};
use super::rt_error;
use super::runtime::{PreparedRoundtableConnection, RoundtableLaunch};
use super::sandbox::IsolationProvider;
use super::service::{ParticipantRuntime, RuntimeIdentity};
use super::store::{
    column, exec, num, one_row, optional_row, rows, storage_err, text, NewAttempt, NewBinding,
    NewTurn, RoundtableStore,
};

/// Application startup owns this runtime even on hosts where the isolated
/// provider is unavailable. Such hosts can inspect history but cannot launch.
pub struct OwnedParticipantRuntime {
    data_dir: PathBuf,
    manager: Arc<ConnectionManager>,
    executor: Option<Arc<dyn RoundtableTurnExecutor>>,
    discovery: Option<Arc<dyn IsolationProvider + Send + Sync>>,
}

/// Resolved by the host provider, never supplied in a command request.
pub struct RuntimeCapability {
    pub recipients: Value,
    pub qualification_keys: Value,
    pub policy_hash: Hash256,
    pub profile: QualifiedContextProfile,
}

#[derive(Clone)]
pub struct RoundtableTurnRequest {
    pub store: RoundtableStore,
    pub room_id: RoomId,
    pub participant: ParticipantV1,
    pub speaker_id: SpeakerId,
    pub fence: Fence,
    pub phase: PhaseSnapshotV1,
    pub scope: ResultScope,
    pub prompt: Vec<u8>,
    pub deadline_mono: u64,
}

pub struct RoundtableTurnOutcome {
    pub completion: RuntimeTurnCompleted,
    pub cleanup: CleanupProof,
}

/// The scheduler and durable acceptance path are shared by an installed
/// provider and a controlled test provider. An executor must stage through
/// the scoped tools; arbitrary return text is never accepted as a result.
#[async_trait]
pub trait RoundtableTurnExecutor: Send + Sync {
    async fn capability(&self, config: &RoundtableConfigV1) -> RtResult<RuntimeCapability>;
    fn token_bound(&self) -> &(dyn TokenBound + Send + Sync);
    async fn execute_turn(&self, request: RoundtableTurnRequest)
        -> RtResult<RoundtableTurnOutcome>;
    async fn cancel_and_reap(&self, identity: RuntimeIdentity) -> RtResult<CleanupProof>;
}

struct LeasedExecutor {
    inner: Arc<dyn RoundtableTurnExecutor>,
    allocator: super::resources::ResourceAllocator,
    leases: Mutex<HashMap<IncarnationId, super::resources::PermitBundle>>,
    proofs: Mutex<HashMap<IncarnationId, CleanupProof>>,
}
impl LeasedExecutor {
    fn wrap(inner: Arc<dyn RoundtableTurnExecutor>) -> Arc<dyn RoundtableTurnExecutor> {
        Arc::new(Self {
            inner,
            allocator: super::resources::ResourceAllocator::new(16),
            leases: Mutex::new(HashMap::new()),
            proofs: Mutex::new(HashMap::new()),
        })
    }
    fn release(&self, proof: &CleanupProof) -> RtResult<()> {
        if !proof.process.process_tree_empty
            || !proof.mailbox_empty
            || !proof.tools_drained
            || !proof.ingress_drained
        {
            return Err(rt_error(ErrorCode::RuntimeUnavailable, "cleanup_unproven"));
        }
        let mut leases = self.leases.lock().expect("runtime leases");
        if let Some(bundle) = leases.get(&proof.process.incarnation) {
            self.allocator.release(
                bundle,
                &PermitReleaseProof {
                    lease_id: bundle.lease_id.clone(),
                    proofs: [(proof.process.incarnation, proof.clone())]
                        .into_iter()
                        .collect(),
                },
            )?;
            leases.remove(&proof.process.incarnation);
        }
        let mut proofs = self.proofs.lock().expect("runtime proofs");
        if proofs.len() >= 1024 {
            if let Some(expired) = proofs.keys().next().copied() {
                proofs.remove(&expired);
            }
        }
        proofs.insert(proof.process.incarnation, proof.clone());
        debug_assert!(proofs.len() <= 1024);
        Ok(())
    }
}
#[async_trait]
impl RoundtableTurnExecutor for LeasedExecutor {
    async fn capability(&self, config: &RoundtableConfigV1) -> RtResult<RuntimeCapability> {
        self.inner.capability(config).await
    }
    fn token_bound(&self) -> &(dyn TokenBound + Send + Sync) {
        self.inner.token_bound()
    }
    async fn execute_turn(
        &self,
        request: RoundtableTurnRequest,
    ) -> RtResult<RoundtableTurnOutcome> {
        let incarnation = request.fence.incarnation;
        let lease = self
            .allocator
            .acquire_incarnation(request.room_id, incarnation)?;
        self.leases
            .lock()
            .expect("runtime leases")
            .insert(incarnation, lease);
        let result = self.inner.execute_turn(request).await;
        if let Ok(outcome) = &result {
            if outcome.cleanup.process.incarnation != incarnation {
                return Err(rt_error(ErrorCode::RuntimeUnavailable, "cleanup_identity"));
            }
            self.release(&outcome.cleanup)?;
        }
        result
    }
    async fn cancel_and_reap(&self, identity: RuntimeIdentity) -> RtResult<CleanupProof> {
        if let Some(proof) = self
            .proofs
            .lock()
            .expect("runtime proofs")
            .get(&identity.incarnation)
            .cloned()
        {
            return Ok(proof);
        }
        let incarnation = identity.incarnation;
        let proof = self.inner.cancel_and_reap(identity).await?;
        if proof.process.incarnation != incarnation {
            return Err(rt_error(ErrorCode::RuntimeUnavailable, "cleanup_identity"));
        }
        self.release(&proof)?;
        Ok(proof)
    }
}

impl OwnedParticipantRuntime {
    pub fn new(data_dir: PathBuf, manager: Arc<ConnectionManager>) -> Self {
        let live = super::live_runtime::LiveParticipantExecutor::load(data_dir.clone())
            .ok()
            .map(Arc::new);
        let discovery = live.as_ref().map(|executor| executor.discovery());
        let executor = live.map(|executor| LeasedExecutor::wrap(executor));
        Self {
            data_dir,
            manager,
            executor,
            discovery,
        }
    }

    #[cfg(any(test, feature = "test-utils"))]
    pub fn with_executor(
        data_dir: PathBuf,
        manager: Arc<ConnectionManager>,
        executor: Arc<dyn RoundtableTurnExecutor>,
    ) -> Self {
        Self {
            data_dir,
            manager,
            executor: Some(LeasedExecutor::wrap(executor)),
            discovery: None,
        }
    }

    /// No journal-only implementation may claim an empty process tree.
    pub fn discovery(&self) -> Option<Arc<dyn IsolationProvider + Send + Sync>> {
        self.discovery.clone()
    }

    fn unavailable(&self) -> roundtable_protocol::RtError {
        let _ = (&self.data_dir, &self.manager);
        if !cfg!(target_os = "linux") {
            rt_error(ErrorCode::PolicyUnenforceable, "platform_unqualified")
        } else {
            rt_error(ErrorCode::CapabilityUnqualified, "certificate_not_passed")
        }
    }
}

#[async_trait]
impl ParticipantRuntime for OwnedParticipantRuntime {
    async fn preflight(&self, config: &RoundtableConfigV1) -> RtResult<Value> {
        let executor = self.executor.as_ref().ok_or_else(|| self.unavailable())?;
        let capability = executor.capability(config).await?;
        Ok(
            json!({"recipients":capability.recipients,"qualification_keys":capability.qualification_keys,
            "policy_hash":capability.policy_hash,"limits_hash":canonical_hash(&capability.profile)?}),
        )
    }

    async fn prepare(&self, _launch: RoundtableLaunch) -> RtResult<PreparedRoundtableConnection> {
        Err(self.unavailable())
    }

    async fn cancel_and_reap(&self, identity: RuntimeIdentity) -> RtResult<CleanupProof> {
        self.executor
            .as_ref()
            .ok_or_else(|| rt_error(ErrorCode::RuntimeUnavailable, "cleanup_unproven"))?
            .cancel_and_reap(identity)
            .await
    }

    async fn run_room(
        &self,
        store: RoundtableStore,
        room: RoomId,
        config: RoundtableConfigV1,
    ) -> RtResult<()> {
        let executor = self.executor.as_ref().ok_or_else(|| self.unavailable())?;
        let row = one_row(
            store.connection(),
            "SELECT boot_epoch,run_epoch FROM rt_rooms WHERE room_id=?",
            vec![text(&room.to_string())],
        )
        .await?;
        let mut lease = super::budget_ledger::ActiveBudgetLease::begin(
            store.clone(),
            room,
            Epoch(nonnegative(column(&row, 0)?)?),
            Epoch(nonnegative(column(&row, 1)?)?),
        )
        .await?;
        let stop = tokio_util::sync::CancellationToken::new();
        let monitor_stop = stop.clone();
        let mut monitor = BudgetMonitor(tokio::spawn(async move {
            loop {
                tokio::select! {
                    _=monitor_stop.cancelled()=>{lease.finish().await?;return Ok(());},
                    _=tokio::time::sleep(std::time::Duration::from_secs(1))=>{
                        let ledger=lease.checkpoint().await?;
                        if ledger.prepaid_until.0<=ledger.last_sample_mono.0 {return Err(rt_error(ErrorCode::InsufficientBudget,"budget_exhausted"));}
                    }
                }
            }
        }));
        let (result, monitor_finished) = tokio::select! {
            result=run_room(store,room,config,Arc::clone(executor))=>(result,false),
            budget=&mut monitor.0=>(budget.map_err(|_|rt_error(ErrorCode::RuntimeUnavailable,"budget_monitor"))?,true),
        };
        stop.cancel();
        if !monitor_finished {
            (&mut monitor.0)
                .await
                .map_err(|_| rt_error(ErrorCode::RuntimeUnavailable, "budget_monitor"))??;
        }
        result
    }
}

struct BudgetMonitor(tokio::task::JoinHandle<RtResult<()>>);
impl Drop for BudgetMonitor {
    fn drop(&mut self) {
        self.0.abort();
    }
}

fn parse<T: FromStr>(value: &str) -> RtResult<T> {
    value
        .parse()
        .map_err(|_| rt_error(ErrorCode::StorageUnavailable, "runtime_identity"))
}
fn fresh<T: FromStr>() -> RtResult<T> {
    parse(&Uuid::new_v4().to_string())
}
fn from_json<T: serde::de::DeserializeOwned>(value: &str) -> RtResult<T> {
    serde_json::from_str(value)
        .map_err(|_| rt_error(ErrorCode::StorageUnavailable, "runtime_snapshot"))
}
fn serialized<T: serde::Serialize>(value: &T) -> RtResult<String> {
    String::from_utf8(canonical_bytes(value)?)
        .map_err(|_| rt_error(ErrorCode::InvalidArgument, "runtime_encoding"))
}
fn nonnegative(value: i64) -> RtResult<u64> {
    u64::try_from(value).map_err(|_| rt_error(ErrorCode::StorageUnavailable, "runtime_counter"))
}

async fn run_room(
    store: RoundtableStore,
    room: RoomId,
    config: RoundtableConfigV1,
    executor: Arc<dyn RoundtableTurnExecutor>,
) -> RtResult<()> {
    let capability = executor.capability(&config).await?;
    let room_text = room.to_string();
    let room_row = one_row(
        store.connection(),
        "SELECT boot_epoch,run_epoch,remaining_active_ms,status FROM rt_rooms WHERE room_id=?",
        vec![text(&room_text)],
    )
    .await?;
    if column::<String>(&room_row, 3)? != "running" {
        return Err(rt_error(ErrorCode::InvalidState, "room_not_running"));
    }
    let boot_epoch = Epoch(nonnegative(column(&room_row, 0)?)?);
    let run_epoch = Epoch(nonnegative(column(&room_row, 1)?)?);
    let started = store.clock_sample().0;
    let prepaid = super::store::query_i64(store.connection(),"SELECT COALESCE(SUM(MAX(0,prepaid_ms-MAX(0,?-last_sample_mono))),0) FROM rt_active_time_leases WHERE room_id=? AND boot_epoch=? AND run_epoch=?",vec![num(started as i64),text(&room_text),num(boot_epoch.0 as i64),num(run_epoch.0 as i64)]).await?;
    let room_deadline = started.saturating_add(
        nonnegative(column(&room_row, 2)?)?
            .saturating_add(nonnegative(prepaid)?)
            .min(config.budgets.room_budget.0),
    );
    let mut speakers = Vec::new();
    for row in rows(store.connection(),"SELECT speaker_id,ordinal,model_snapshot_json FROM rt_speakers WHERE room_id=? ORDER BY ordinal",vec![text(&room_text)]).await? {
        speakers.push((SpeakerOrdinal{speaker_id:parse(&column::<String>(&row,0)?)?,ordinal:u32::try_from(column::<i64>(&row,1)?).map_err(|_|rt_error(ErrorCode::StorageUnavailable,"speaker_ordinal"))?},from_json::<ParticipantV1>(&column::<String>(&row,2)?)?));
    }
    for (_, participant) in &mut speakers {
        if participant.model.is_none() {
            participant.model = capability
                .recipients
                .as_array()
                .into_iter()
                .flatten()
                .find(|recipient| recipient["provider_ref"] == participant.provider_ref)
                .and_then(|recipient| recipient["model"].as_str())
                .map(str::to_owned);
        }
    }
    let moderator = speakers
        .iter()
        .find(|(speaker, _)| speaker.ordinal == config.participants.len() as u32)
        .cloned()
        .ok_or_else(|| rt_error(ErrorCode::InvalidState, "moderator_missing"))?;
    let members: Vec<_> = speakers
        .iter()
        .filter(|(speaker, _)| speaker.ordinal < config.participants.len() as u32)
        .cloned()
        .collect();
    let n = u32::try_from(members.len())
        .map_err(|_| rt_error(ErrorCode::InvalidArgument, "participant_count"))?;
    let timeouts = Timeouts {
        launch_ms: 0,
        prompt_ms: config.timeouts.attempt_timeout.0,
        validation_ms: 0,
        cleanup_ms: 0,
    };
    let plan = budget_plan(
        n,
        config.strategy.critique_rounds,
        config.concurrency.min(n),
        timeouts,
    )?;
    let mut budget = BudgetState::fresh(
        n,
        config.strategy.critique_rounds,
        config.concurrency.min(n),
        plan,
    );
    for row in rows(store.connection(),"SELECT p.phase_index,p.revision,s.ordinal,s.role,a.attempt_no FROM rt_attempts a JOIN rt_turns t ON t.room_id=a.room_id AND t.turn_id=a.turn_id JOIN rt_phases p ON p.room_id=t.room_id AND p.phase_id=t.phase_id JOIN rt_speakers s ON s.room_id=t.room_id AND s.speaker_id=t.speaker_id WHERE a.room_id=? AND a.dispatch_state IN ('sent','unknown') ORDER BY p.phase_index,p.revision,s.ordinal,a.attempt_no",vec![text(&room_text)]).await? {
        let phase_index=u32::try_from(column::<i64>(&row,0)?).map_err(|_|rt_error(ErrorCode::StorageUnavailable,"phase_index"))?;
        budget.current_phase_index=phase_index;
        let slot=if column::<String>(&row,3)?=="moderator" {AttemptSlot::Moderator} else {AttemptSlot::Member{phase_index,ordinal:u32::try_from(column::<i64>(&row,2)?).map_err(|_|rt_error(ErrorCode::StorageUnavailable,"speaker_ordinal"))?}};
        budget=reserve_attempts(&budget,ReservationRequest{kind:if column::<i64>(&row,4)?==1 {ReservationKind::FirstLaunch} else {ReservationKind::OptionalRetry},revision:nonnegative(column(&row,1)?)?,slot,observation:DispatchObservation::Admitted,now:MonoMs(0),deadline:MonoMs(u64::MAX)})?.state;
    }
    if super::store::query_i64(
        store.connection(),
        "SELECT COUNT(*) FROM rt_attempts WHERE room_id=? AND cleanup_state<>'confirmed'",
        vec![text(&room_text)],
    )
    .await?
        != 0
    {
        return Err(rt_error(ErrorCode::RuntimeUnavailable, "cleanup_unproven"));
    }
    let txn = store.connection().begin().await.map_err(storage_err)?;
    store.recover_frozen_in(&txn, &room_text).await?;
    txn.commit().await.map_err(storage_err)?;
    let mut published = load_history(&store, &room).await?;
    if published
        .phases
        .iter()
        .any(|phase| phase.kind == PhaseKind::Synthesis)
    {
        return Ok(());
    }
    let start_index = published
        .phases
        .last()
        .map_or(0, |phase| phase.phase_index + 1);
    for index in start_index..=config.strategy.critique_rounds + 1 {
        require_running(&store, &room, boot_epoch, run_epoch).await?;
        let kind = if index == config.strategy.critique_rounds + 1 {
            PhaseKind::Synthesis
        } else if index == 0 {
            PhaseKind::Proposal
        } else {
            PhaseKind::Critique
        };
        let phase_speakers = if kind == PhaseKind::Synthesis {
            vec![moderator.clone()]
        } else {
            members.clone()
        };
        budget.current_phase_index = index;
        let mut phase_deadline = store
            .clock_sample()
            .0
            .saturating_add(config.budgets.phase_budget.0)
            .min(room_deadline);
        let phase_id: PhaseId = fresh()?;
        let current=optional_row(store.connection(),"SELECT p.phase_id,p.remaining_ms,c.snapshot_json,c.context_json FROM rt_rooms r JOIN rt_phases p ON p.room_id=r.room_id AND p.phase_id=r.current_phase_id LEFT JOIN rt_phase_contexts c ON c.room_id=p.room_id AND c.phase_id=p.phase_id WHERE r.room_id=? AND p.phase_index=? AND p.status='running'",vec![text(&room_text),num(index as i64)]).await?;
        let (snapshot, context, aliases) = if let Some(current) = current {
            phase_deadline = store
                .clock_sample()
                .0
                .saturating_add(nonnegative(column(&current, 1)?)?)
                .min(room_deadline);
            let snapshot: PhaseSnapshotV1 = from_json(&column::<String>(&current, 2)?)?;
            let context: Value = from_json(&column::<String>(&current, 3)?)?;
            let aliases: VisibleAliases = serde_json::from_value(context["aliases"].clone())
                .map_err(|_| rt_error(ErrorCode::StorageUnavailable, "phase_aliases"))?;
            (snapshot, context, aliases)
        } else {
            if let Some(ready)=optional_row(store.connection(),"SELECT remaining_ms FROM rt_phases WHERE room_id=? AND phase_index=? AND status='ready' ORDER BY revision DESC LIMIT 1",vec![text(&room_text),num(index as i64)]).await? {
                phase_deadline=phase_deadline.min(store.clock_sample().0.saturating_add(nonnegative(column(&ready,0)?)?));
            }
            freeze_phase(
                &store,
                &room,
                &config,
                &published,
                &phase_speakers,
                (phase_id, index, kind, phase_deadline),
                capability.policy_hash,
            )
            .await?
        };
        let phase_id = snapshot.phase_id;
        let mut slots: Vec<_> = phase_speakers
            .iter()
            .map(|(speaker, _)| SlotSnapshot {
                phase_id,
                kind,
                speaker: *speaker,
                outcome: SlotOutcome::Open,
                first_launch_spent: false,
                retry_requested: false,
            })
            .collect();
        for slot in &mut slots {
            if let Some(turn)=optional_row(store.connection(),"SELECT status,admitted_attempt_count FROM rt_turns WHERE room_id=? AND phase_id=? AND speaker_id=?",vec![text(&room_text),text(&phase_id.to_string()),text(&slot.speaker.speaker_id.to_string())]).await? {
                let status:String=column(&turn,0)?;
                let attempts:i64=column(&turn,1)?;
                slot.first_launch_spent=attempts>0;
                slot.outcome=match status.as_str() {"valid"|"accepted"=>SlotOutcome::Valid,"abstained"=>SlotOutcome::Abstained,_=>SlotOutcome::Failed};
                if !matches!(slot.outcome,SlotOutcome::Valid|SlotOutcome::Abstained)&&attempts<2 {slot.outcome=SlotOutcome::Open;slot.retry_requested=true;}
            }
        }
        let mut closing_fence =
            existing_fence(&store, &room, phase_id, boot_epoch, run_epoch).await?;
        // Every first launch runs once before any retry. A wave cannot exceed C.
        for retry in [false, true] {
            if retry {
                for slot in &mut slots {
                    if matches!(slot.outcome, SlotOutcome::Failed | SlotOutcome::Invalid) {
                        slot.outcome = SlotOutcome::Open;
                        slot.retry_requested = true;
                    }
                }
                if !slots.iter().any(|slot| slot.retry_requested) {
                    break;
                }
            }
            if !slots.iter().any(|slot| slot.outcome == SlotOutcome::Open) {
                break;
            }
            let intents = next_intents(&StrategyView {
                published: published.clone(),
                slots: slots.clone(),
                budget: StrategyBudget {
                    state: budget.clone(),
                    now: MonoMs(store.clock_sample().0),
                    phase_deadline: MonoMs(phase_deadline),
                },
                moderator: moderator.0,
                next_phase_id: fresh()?,
            })?;
            for wave in intents.chunks(config.concurrency as usize) {
                let mut pending = Vec::new();
                for intent in wave {
                    let local = phase_speakers
                        .iter()
                        .position(|(speaker, _)| speaker.speaker_id == intent.speaker_id)
                        .ok_or_else(|| rt_error(ErrorCode::InvalidState, "speaker_missing"))?;
                    let slot = if kind == PhaseKind::Synthesis {
                        AttemptSlot::Moderator
                    } else {
                        AttemptSlot::Member {
                            phase_index: index,
                            ordinal: local as u32,
                        }
                    };
                    let reservation = reserve_attempts(
                        &budget,
                        ReservationRequest {
                            kind: if intent.is_retry {
                                ReservationKind::OptionalRetry
                            } else {
                                ReservationKind::FirstLaunch
                            },
                            revision: snapshot.revision.0,
                            slot,
                            observation: DispatchObservation::Admitted,
                            now: MonoMs(store.clock_sample().0),
                            deadline: MonoMs(phase_deadline),
                        },
                    );
                    match reservation {
                        Ok(reservation) => budget = reservation.state,
                        Err(_) if intent.is_retry => {
                            slots[local].outcome = SlotOutcome::Failed;
                            continue;
                        }
                        Err(error) => return Err(error),
                    }
                    slots[local].first_launch_spent = true;
                    let request = prepare_turn(
                        &store,
                        room,
                        &config,
                        (&snapshot, &context, &aliases, &published),
                        &phase_speakers[local],
                        (boot_epoch, run_epoch, phase_deadline),
                        (&capability, executor.token_bound()),
                    )
                    .await?;
                    closing_fence = Some(request.fence.clone());
                    pending.push(run_turn(Arc::clone(&executor), request));
                }
                for result in futures::future::join_all(pending).await {
                    let (speaker, outcome) = result?;
                    if let Some(slot) = slots
                        .iter_mut()
                        .find(|slot| slot.speaker.speaker_id == speaker)
                    {
                        slot.outcome = outcome;
                    }
                }
            }
        }
        let fence =
            closing_fence.ok_or_else(|| rt_error(ErrorCode::InvalidState, "empty_phase"))?;
        require_running(&store, &room, boot_epoch, run_epoch).await?;
        store
            .close_phase(CloseInput {
                room_id: room,
                phase_id,
                fence: fence.clone(),
                deadline_mono: phase_deadline,
                cancel_unfinished: false,
            })
            .await?;
        store
            .publish(PublishInput {
                room_id: room,
                phase_id,
                fence,
                deadline_mono: phase_deadline,
                cleanup_confirmed: true,
            })
            .await?;
        published = load_history(&store, &room).await?;
    }
    Ok(())
}

async fn existing_fence(
    store: &RoundtableStore,
    room: &RoomId,
    phase: PhaseId,
    boot: Epoch,
    run: Epoch,
) -> RtResult<Option<Fence>> {
    let row=optional_row(store.connection(),"SELECT p.revision,a.attempt_id,a.binding_id,b.incarnation,b.policy_ref,a.delivery_hash FROM rt_phases p JOIN rt_turns t ON t.room_id=p.room_id AND t.phase_id=p.phase_id JOIN rt_attempts a ON a.room_id=t.room_id AND a.turn_id=t.turn_id JOIN rt_bindings b ON b.room_id=a.room_id AND b.binding_id=a.binding_id WHERE p.room_id=? AND p.phase_id=? ORDER BY a.attempt_no DESC LIMIT 1",vec![text(&room.to_string()),text(&phase.to_string())]).await?;
    row.map(|row| {
        Ok(Fence {
            phase_id: phase,
            phase_revision: Revision(nonnegative(column(&row, 0)?)?),
            attempt_id: parse(&column::<String>(&row, 1)?)?,
            binding_id: parse(&column::<String>(&row, 2)?)?,
            incarnation: parse(&column::<String>(&row, 3)?)?,
            policy_hash: from_json(&serialized(&column::<String>(&row, 4)?)?)?,
            context_hash: from_json(&serialized(&column::<String>(&row, 5)?)?)?,
            boot_epoch: boot,
            run_epoch: run,
        })
    })
    .transpose()
}

async fn require_running(
    store: &RoundtableStore,
    room: &RoomId,
    boot: Epoch,
    run: Epoch,
) -> RtResult<()> {
    let row = one_row(
        store.connection(),
        "SELECT status,boot_epoch,run_epoch,active_control_id FROM rt_rooms WHERE room_id=?",
        vec![text(&room.to_string())],
    )
    .await?;
    if column::<String>(&row, 0)? != "running"
        || nonnegative(column(&row, 1)?)? != boot.0
        || nonnegative(column(&row, 2)?)? != run.0
        || column::<Option<String>>(&row, 3)?.is_some()
    {
        return Err(rt_error(ErrorCode::InvalidState, "stale_runtime"));
    }
    Ok(())
}

async fn freeze_phase(
    store: &RoundtableStore,
    room: &RoomId,
    config: &RoundtableConfigV1,
    history: &PublishedHistory,
    speakers: &[(SpeakerOrdinal, ParticipantV1)],
    phase: (PhaseId, u32, PhaseKind, u64),
    policy_hash: Hash256,
) -> RtResult<(PhaseSnapshotV1, Value, VisibleAliases)> {
    let (phase_id, index, kind, deadline) = phase;
    let mut aliases = VisibleAliases::default();
    for phase in &history.phases {
        for member in &phase.members {
            aliases.speakers.insert(
                format!("s{}", member.speaker.ordinal),
                member.speaker.speaker_id,
            );
            for claim in &member.claims {
                aliases.claims.insert(
                    format!("c{}", aliases.claims.len()),
                    ClaimRef {
                        claim_id: claim.claim_id,
                        speaker_id: member.speaker.speaker_id,
                        published: true,
                    },
                );
            }
            for response in &member.responses {
                aliases.responses.insert(
                    format!("r{}", aliases.responses.len()),
                    ResponseRef {
                        response_id: response.response_id,
                        speaker_id: member.speaker.speaker_id,
                        published: true,
                    },
                );
            }
        }
    }
    let room_text = room.to_string();
    let ready=optional_row(store.connection(),"SELECT phase_id,revision FROM rt_phases WHERE room_id=? AND phase_index=? AND status='ready' ORDER BY revision DESC LIMIT 1",vec![text(&room_text),num(index as i64)]).await?;
    let phase_id = match &ready {
        Some(row) => parse(&column::<String>(row, 0)?)?,
        None => phase_id,
    };
    let phase_revision = if let Some(row) = &ready {
        column::<i64>(row, 1)?
    } else {
        super::store::query_i64(
            store.connection(),
            "SELECT COALESCE(MAX(revision),0)+1 FROM rt_phases WHERE room_id=? AND phase_index=?",
            vec![text(&room_text), num(index as i64)],
        )
        .await?
    };
    let mut messages = Vec::new();
    let mut published_messages = Vec::new();
    for row in rows(store.connection(),"SELECT m.message_id,m.body_hash,m.body_json FROM rt_messages m WHERE m.room_id=? AND EXISTS(SELECT 1 FROM rt_message_memberships mm WHERE mm.room_id=m.room_id AND mm.message_id=m.message_id AND mm.visibility='published') ORDER BY m.message_id",vec![text(&room_text)]).await? {
        let id:MessageId=parse(&column::<String>(&row,0)?)?;
        let body:Value=from_json(&column::<String>(&row,2)?)?;
        let hash:Hash256=from_json(&serialized(&column::<String>(&row,1)?)?)?;
        if canonical_hash(&body)?!=hash {return Err(rt_error(ErrorCode::StorageUnavailable,"published_hash"));}
        aliases.messages.insert(format!("m{}",aliases.messages.len()),id);
        published_messages.push(PublishedMessageRef{message_id:id,hash});
        messages.push(json!({"message_id":id,"body":body}));
    }
    let mut mandatory_targets = Vec::new();
    if kind == PhaseKind::Critique {
        let previous = history
            .phases
            .last()
            .ok_or_else(|| rt_error(ErrorCode::InvalidState, "published_phase_missing"))?;
        for targets in assigned_targets(
            previous,
            &speakers
                .iter()
                .map(|(speaker, _)| *speaker)
                .collect::<Vec<_>>(),
            history,
        )? {
            for target in targets.required {
                mandatory_targets.push(MandatoryTargetV1 {
                    speaker_id: targets.speaker.speaker_id,
                    claim_id: target.claim_id,
                    response_id: target.response_id,
                });
            }
        }
    }
    let version = super::store::query_i64(
        store.connection(),
        "SELECT COALESCE(MAX(version),0)+1 FROM rt_source_manifests WHERE room_id=?",
        vec![text(&room_text)],
    )
    .await?;
    let mut entries = Vec::new();
    for source in &config.source_refs {
        let row=one_row(store.connection(),"SELECT body_json,manifest_hash FROM rt_source_manifests WHERE room_id=? AND manifest_id=?",vec![text(&room_text),text(&source.snapshot_id.to_string())]).await?;
        let source: super::SourceManifestV1 = from_json(&column::<String>(&row, 0)?)?;
        let checked = super::rehome_manifest(&source, *room, source.version)?;
        if source.manifest_hash != checked.manifest_hash
            || source.manifest_hash.to_hex() != column::<String>(&row, 1)?
        {
            return Err(rt_error(
                ErrorCode::StorageUnavailable,
                "source_manifest_hash",
            ));
        }
        for entry in source.entries {
            if let Some(existing) = entries
                .iter()
                .find(|existing: &&super::SourceEntryV1| existing.path == entry.path)
            {
                if existing.content_hash != entry.content_hash {
                    return Err(rt_error(ErrorCode::InvalidArgument, "source_path_conflict"));
                }
            } else {
                entries.push(entry);
            }
        }
    }
    let seed = super::SourceManifestV1 {
        schema_version: 1,
        manifest_id: fresh()?,
        room_id: *room,
        version,
        base_commit: None,
        read_limit: super::MAX_SNAPSHOT_READS,
        entries,
        manifest_hash: Hash256::from_bytes([0; 32]),
    };
    let manifest = super::rehome_manifest(&seed, *room, version)?;
    let source_manifest_id = manifest.manifest_id;
    let source_manifest_hash = manifest.manifest_hash;
    let sources = json!([manifest]);
    let mut evidence = Vec::new();
    for entry in &manifest.entries {
        if !entry.text_admissible {
            continue;
        }
        let alias = format!("e{}", evidence.len());
        let evidence_id: EvidenceId = fresh()?;
        aliases.evidence.insert(
            alias.clone(),
            EvidenceRef {
                evidence_id,
                visibility: AliasVisibility::Published,
            },
        );
        evidence.push(json!({"alias":alias,"evidence_id":evidence_id,"path":entry.path,"object":ObjectRefV1{object_id:entry.object.object_id.clone(),kind:ObjectKind::SourceExcerpt,content_hash:entry.object.content_hash,total_bytes:SafeInt(entry.object.total_bytes)}}));
    }
    let snapshot = PhaseSnapshotV1 {
        schema_version: 1,
        phase_id,
        phase_index: index,
        revision: Revision(nonnegative(phase_revision)?),
        kind,
        critique_round: if kind == PhaseKind::Critique {
            Some(index)
        } else {
            None
        },
        config_version: Revision(1),
        question_version: Revision(1),
        interjection_version: Revision(1),
        source_manifest_id,
        source_manifest_hash,
        published_messages,
        members: speakers
            .iter()
            .map(|(speaker, _)| {
                Ok(OrderedMemberV1 {
                    ordinal: speaker.ordinal,
                    speaker_id: speaker.speaker_id,
                    participant_id: parse(&speaker.speaker_id.to_string())?,
                })
            })
            .collect::<RtResult<Vec<_>>>()?,
        mandatory_targets,
        policy_hash,
        output_byte_limit: config.quotas.output_byte_limit,
        tool_quota: ToolQuotaV1 {
            per_call_bytes: SafeInt(8192),
            per_attempt_bytes: SafeInt(32768),
        },
    };
    let txn = store.connection().begin().await.map_err(storage_err)?;
    let room_row = one_row(
        &txn,
        "SELECT status,active_control_id FROM rt_rooms WHERE room_id=?",
        vec![text(&room_text)],
    )
    .await?;
    if column::<String>(&room_row, 0)? != "running"
        || column::<Option<String>>(&room_row, 1)?.is_some()
    {
        return Err(rt_error(ErrorCode::InvalidState, "room_not_running"));
    }
    let mut inputs = Vec::new();
    for row in rows(&txn,"SELECT input_id,text FROM rt_user_inputs WHERE room_id=? AND state IN ('queued','applied') AND target_phase_index<=? ORDER BY accepted_seq",vec![text(&room_text),num(index as i64)]).await? {
        inputs.push(json!({"input_id":column::<String>(&row,0)?,"text":column::<String>(&row,1)?}));
    }
    let context = json!({"topic":config.topic,"sources":sources,"published_messages":messages,"inputs":inputs,"aliases":aliases,"evidence":evidence});
    exec(&txn,"INSERT INTO rt_source_manifests(room_id,manifest_id,version,manifest_hash,body_json) VALUES(?,?,?,?,?)",vec![text(&room_text),text(&source_manifest_id.to_string()),num(version),text(&source_manifest_hash.to_hex()),text(&serialized(&manifest)?)]).await?;
    for entry in &evidence {
        let body = json!({"file_alias":entry["alias"],"workspace_snapshot_id":source_manifest_id,"manifest_hash":source_manifest_hash.to_hex(),"path":entry["path"],"owner_attempt_id":null,"staged_phase_id":phase_id,"phase_revision":phase_revision,"line_start":null,"line_end":null,"byte_start":null,"byte_end":null,"excerpt_hash":Hash256::sha256(b"").to_hex(),"excerpt":"","verified":true,"origin":"snapshot"});
        exec(&txn,"INSERT INTO rt_evidence(room_id,evidence_id,manifest_id,owner_speaker_id,content_hash,attribution,publish_seq,body_json) VALUES(?,?,?,?,?,'verified',0,?)",vec![text(&room_text),text(entry["evidence_id"].as_str().ok_or_else(||rt_error(ErrorCode::InvalidArgument,"evidence_id"))?),text(&source_manifest_id.to_string()),text(&speakers[0].0.speaker_id.to_string()),text(entry["object"]["content_hash"].as_str().ok_or_else(||rt_error(ErrorCode::InvalidArgument,"evidence_hash"))?),text(&serialized(&body)?)]).await?;
    }
    if ready.is_none() {
        exec(&txn,"UPDATE rt_phases SET status='superseded' WHERE room_id=? AND phase_index=? AND status IN ('failed')",vec![text(&room_text),num(index as i64)]).await?;
        exec(&txn,"INSERT INTO rt_phases(room_id,phase_id,phase_index,revision,status,snapshot_ref,snapshot_hash,expected,quorum,remaining_ms,manifest_id) VALUES(?,?,?,?,'running',?,?,?,?,?,?)",vec![text(&room_text),text(&phase_id.to_string()),num(index as i64),num(phase_revision),text(match kind {PhaseKind::Proposal=>"proposal",PhaseKind::Critique=>"critique",PhaseKind::Synthesis=>"synthesis"}),text(&canonical_hash(&snapshot)?.to_hex()),num(speakers.len() as i64),num(phase_quorum(config.participants.len() as u32,kind) as i64),num(deadline.saturating_sub(store.clock_sample().0) as i64),text(&source_manifest_id.to_string())]).await?;
    } else {
        exec(&txn,"UPDATE rt_phases SET status='running',snapshot_ref=?,snapshot_hash=?,expected=?,quorum=?,remaining_ms=?,manifest_id=? WHERE room_id=? AND phase_id=? AND status='ready'",vec![text(match kind{PhaseKind::Proposal=>"proposal",PhaseKind::Critique=>"critique",PhaseKind::Synthesis=>"synthesis"}),text(&canonical_hash(&snapshot)?.to_hex()),num(speakers.len() as i64),num(phase_quorum(config.participants.len() as u32,kind) as i64),num(deadline.saturating_sub(store.clock_sample().0) as i64),text(&source_manifest_id.to_string()),text(&room_text),text(&phase_id.to_string())]).await?;
    }
    exec(&txn,"INSERT INTO rt_phase_contexts(room_id,phase_id,snapshot_json,context_json) VALUES(?,?,?,?)",vec![text(&room_text),text(&phase_id.to_string()),text(&serialized(&snapshot)?),text(&serialized(&context)?)]).await?;
    exec(&txn,"UPDATE rt_user_inputs SET state='applied',applied_phase_id=?,applied_seq=(SELECT last_seq+1 FROM rt_rooms WHERE room_id=?) WHERE room_id=? AND state='queued' AND target_phase_index<=?",vec![text(&phase_id.to_string()),text(&room_text),text(&room_text),num(index as i64)]).await?;
    exec(&txn,"UPDATE rt_rooms SET current_phase_id=?,revision=revision+1,last_seq=last_seq+1 WHERE room_id=?",vec![text(&phase_id.to_string()),text(&room_text)]).await?;
    store.emit_current_in(&txn, &room_text, "attempt").await?;
    txn.commit().await.map_err(storage_err)?;
    Ok((snapshot, context, aliases))
}

async fn prepare_turn(
    store: &RoundtableStore,
    room: RoomId,
    config: &RoundtableConfigV1,
    frozen: (&PhaseSnapshotV1, &Value, &VisibleAliases, &PublishedHistory),
    speaker: &(SpeakerOrdinal, ParticipantV1),
    lease: (Epoch, Epoch, u64),
    encoder: (&RuntimeCapability, &(dyn TokenBound + Send + Sync)),
) -> RtResult<RoundtableTurnRequest> {
    let (phase, context, aliases, history) = frozen;
    let (boot, run, deadline) = lease;
    let (capability, tokens) = encoder;
    require_running(store, &room, boot, run).await?;
    let binding_id: BindingId = fresh()?;
    let attempt_id: AttemptId = fresh()?;
    let incarnation: IncarnationId = fresh()?;
    let participant = &speaker.1;
    let role=RoleSnapshot{role:participant.role.clone(),model:participant.model.clone().unwrap_or_else(||"default".into()),effort:participant.effort.clone().unwrap_or_default(),provider_ref:participant.provider_ref.clone(),prompt_version:"roundtable.v1".into(),template_version:"roundtable.v1".into(),schema_id:if phase.kind==PhaseKind::Synthesis {"moderator"} else {"member"}.into(),schema_text:if phase.kind==PhaseKind::Synthesis {"Submit a moderator result using submit_result; cite visible aliases and published support."} else {"Submit a member result using submit_result; address every mandatory target and cite visible aliases."}.into(),tool_version:super::tool_core::SERVICE_TOOL_VERSION.into(),tool_text:"read_evidence, search_evidence, submit_result".into()};
    let delivery = DeliveryEncoder::encode_with_context(
        phase,
        &role,
        &binding_id,
        context,
        tokens,
        &capability.profile,
    )?;
    let prompt = DeliveryEncoder::prompt_with_context(phase, &role, &binding_id, context)?;
    let fence = Fence {
        boot_epoch: boot,
        run_epoch: run,
        phase_id: phase.phase_id,
        phase_revision: phase.revision,
        attempt_id,
        binding_id,
        incarnation,
        context_hash: canonical_hash(&delivery)?,
        policy_hash: capability.policy_hash,
    };
    let mut mandatory_targets = Vec::new();
    for target in phase
        .mandatory_targets
        .iter()
        .filter(|target| target.speaker_id == speaker.0.speaker_id)
    {
        let claim_alias = aliases
            .claims
            .iter()
            .find(|(_, reference)| reference.claim_id == target.claim_id)
            .map(|(alias, _)| alias.clone())
            .ok_or_else(|| rt_error(ErrorCode::InvalidState, "target_alias"))?;
        let response_alias = target
            .response_id
            .map(|id| {
                aliases
                    .responses
                    .iter()
                    .find(|(_, reference)| reference.response_id == id)
                    .map(|(alias, _)| alias.clone())
                    .ok_or_else(|| rt_error(ErrorCode::InvalidState, "target_alias"))
            })
            .transpose()?;
        mandatory_targets.push(RequiredTarget {
            claim_alias,
            claim_id: target.claim_id,
            response_alias,
            response_id: target.response_id,
        });
    }
    let scope = ResultScope {
        phase_kind: phase.kind,
        speaker_id: speaker.0.speaker_id,
        aliases: aliases.clone(),
        mandatory_targets,
        published: history.clone(),
        quota_bytes: u32::try_from(config.quotas.output_byte_limit.0)
            .map_err(|_| rt_error(ErrorCode::InvalidArgument, "output_quota"))?,
    };
    let room_text = room.to_string();
    let generation = super::store::query_i64(
        store.connection(),
        "SELECT COALESCE(MAX(generation),0)+1 FROM rt_bindings WHERE room_id=? AND speaker_id=?",
        vec![text(&room_text), text(&speaker.0.speaker_id.to_string())],
    )
    .await?;
    store
        .insert_binding(&NewBinding {
            room_id: room_text.clone(),
            binding_id: binding_id.to_string(),
            speaker_id: speaker.0.speaker_id.to_string(),
            generation,
            incarnation: incarnation.to_string(),
            external_session_id: attempt_id.to_string(),
            policy_ref: fence.policy_hash.to_hex(),
            certificate_ref: serialized(&capability.qualification_keys)?,
            context_state: "fresh".into(),
            state: "active".into(),
        })
        .await?;
    let existing=optional_row(store.connection(),
        "SELECT turn_id,admitted_attempt_count FROM rt_turns WHERE room_id=? AND phase_id=? AND speaker_id=?",
        vec![text(&room_text),text(&phase.phase_id.to_string()),text(&speaker.0.speaker_id.to_string())]).await?;
    let (turn_id, attempt_no) = match existing {
        Some(row) => (column::<String>(&row, 0)?, column::<i64>(&row, 1)? + 1),
        None => (Uuid::new_v4().to_string(), 1),
    };
    if attempt_no == 1 {
        store
            .insert_turn(&NewTurn {
                room_id: room_text.clone(),
                turn_id: turn_id.clone(),
                phase_id: phase.phase_id.to_string(),
                speaker_id: speaker.0.speaker_id.to_string(),
                status: "open".into(),
                admitted_attempt_count: 1,
            })
            .await?;
    } else {
        exec(
            store.connection(),
            "UPDATE rt_turns SET admitted_attempt_count=? WHERE room_id=? AND turn_id=?",
            vec![num(attempt_no), text(&room_text), text(&turn_id)],
        )
        .await?;
    }
    store
        .insert_attempt(&NewAttempt {
            room_id: room_text.clone(),
            attempt_id: attempt_id.to_string(),
            turn_id,
            attempt_no,
            binding_id: binding_id.to_string(),
            fence: 1,
            dispatch_state: "sent".into(),
            state: "active".into(),
            prompt_hash: delivery.prompt_hash.to_hex(),
            delivery_hash: fence.context_hash.to_hex(),
            cleanup_state: "pending".into(),
            residual_remote_work: 0,
        })
        .await?;
    exec(
        store.connection(),
        "INSERT INTO rt_deliveries(room_id,attempt_id,body_json,prompt_utf8) VALUES(?,?,?,?)",
        vec![
            text(&room_text),
            text(&attempt_id.to_string()),
            text(&serialized(&delivery)?),
            text(
                std::str::from_utf8(&prompt)
                    .map_err(|_| rt_error(ErrorCode::InvalidArgument, "prompt_encoding"))?,
            ),
        ],
    )
    .await?;
    Ok(RoundtableTurnRequest {
        store: store.clone(),
        room_id: room,
        participant: participant.clone(),
        speaker_id: speaker.0.speaker_id,
        fence,
        phase: phase.clone(),
        scope,
        prompt,
        deadline_mono: deadline.min(
            store
                .clock_sample()
                .0
                .saturating_add(config.timeouts.attempt_timeout.0),
        ),
    })
}

async fn run_turn(
    executor: Arc<dyn RoundtableTurnExecutor>,
    request: RoundtableTurnRequest,
) -> RtResult<(SpeakerId, SlotOutcome)> {
    let store = request.store.clone();
    let room = request.room_id;
    let speaker = request.speaker_id;
    let fence = request.fence.clone();
    let deadline = request.deadline_mono;
    let remaining = deadline.saturating_sub(store.clock_sample().0);
    let completed = tokio::select! {
        result=executor.execute_turn(request)=>Some(result),
        _=tokio::time::sleep(std::time::Duration::from_millis(remaining))=>None,
        _=async {
            loop {
                tokio::time::sleep(std::time::Duration::from_millis(250)).await;
                if require_running(&store,&room,fence.boot_epoch,fence.run_epoch).await.is_err() {break;}
            }
        }=>None,
    };
    let outcome = match completed {
        Some(Ok(outcome)) => outcome,
        result => {
            let proof = executor
                .cancel_and_reap(RuntimeIdentity {
                    incarnation: fence.incarnation,
                    pid: 0,
                })
                .await?;
            if !cleanup_proven(&proof, &fence) {
                return Err(rt_error(ErrorCode::RuntimeUnavailable, "cleanup_unproven"));
            }
            exec(store.connection(),"UPDATE rt_attempts SET state='failed',cleanup_state='confirmed' WHERE room_id=? AND attempt_id=?",vec![text(&room.to_string()),text(&fence.attempt_id.to_string())]).await?;
            let _ = result;
            return Ok((speaker, SlotOutcome::Failed));
        }
    };
    if !cleanup_proven(&outcome.cleanup, &fence) {
        return Err(rt_error(ErrorCode::RuntimeUnavailable, "cleanup_unproven"));
    }
    require_running(&store, &room, fence.boot_epoch, fence.run_epoch).await?;
    let Some(candidate) = outcome.completion.candidate_id.clone() else {
        exec(store.connection(),"UPDATE rt_attempts SET state='failed',cleanup_state='confirmed' WHERE room_id=? AND attempt_id=?",vec![text(&room.to_string()),text(&fence.attempt_id.to_string())]).await?;
        return Ok((speaker, SlotOutcome::Failed));
    };
    exec(store.connection(),"UPDATE rt_attempts SET state='validating',cleanup_state='confirmed' WHERE room_id=? AND attempt_id=?",vec![text(&room.to_string()),text(&fence.attempt_id.to_string())]).await?;
    let accepted = store
        .accept(AcceptInput {
            room_id: room,
            completion: outcome.completion,
            candidate_id: candidate,
            fence: fence.clone(),
            deadline_mono: deadline,
            cleanup: outcome.cleanup,
            gate_held: true,
        })
        .await;
    if let Err(error) = accepted {
        exec(store.connection(),"UPDATE rt_attempts SET state='failed',cleanup_state='confirmed' WHERE room_id=? AND attempt_id=? AND state='validating'",vec![text(&room.to_string()),text(&fence.attempt_id.to_string())]).await?;
        if matches!(error.code, ErrorCode::StorageUnavailable) {
            return Err(error);
        }
        return Ok((speaker, SlotOutcome::Failed));
    }
    Ok((speaker, SlotOutcome::Valid))
}

fn cleanup_proven(proof: &CleanupProof, fence: &Fence) -> bool {
    proof.process.incarnation == fence.incarnation
        && proof.process.process_tree_empty
        && proof.mailbox_empty
        && proof.tools_drained
        && proof.ingress_drained
}

async fn load_history(store: &RoundtableStore, room: &RoomId) -> RtResult<PublishedHistory> {
    load_history_in(store.connection(), room).await
}

pub(crate) async fn load_history_in(
    conn: &impl sea_orm::ConnectionTrait,
    room: &RoomId,
) -> RtResult<PublishedHistory> {
    let mut history = PublishedHistory::default();
    for row in rows(conn,"SELECT phase_id,phase_index,snapshot_ref,published_seq FROM rt_phases WHERE room_id=? AND status='published' ORDER BY phase_index",vec![text(&room.to_string())]).await? {
        let phase_id:PhaseId=parse(&column::<String>(&row,0)?)?;
        let phase_index=u32::try_from(column::<i64>(&row,1)?).map_err(|_|rt_error(ErrorCode::StorageUnavailable,"phase_index"))?;
        let kind=match column::<String>(&row,2)?.as_str() {"synthesis"=>PhaseKind::Synthesis,"critique"=>PhaseKind::Critique,_=>PhaseKind::Proposal};
        let publication_seq=Seq(nonnegative(column(&row,3)?)?);
        let mut members=Vec::new();
        if kind!=PhaseKind::Synthesis {
            for row in rows(conn,"SELECT m.message_id,m.body_json,m.speaker_id,s.ordinal FROM rt_messages m JOIN rt_attempts a ON a.room_id=m.room_id AND a.attempt_id=m.attempt_id JOIN rt_turns t ON t.room_id=a.room_id AND t.turn_id=a.turn_id JOIN rt_speakers s ON s.room_id=m.room_id AND s.speaker_id=m.speaker_id WHERE m.room_id=? AND t.phase_id=? AND EXISTS(SELECT 1 FROM rt_message_memberships mm WHERE mm.room_id=m.room_id AND mm.message_id=m.message_id AND mm.visibility='published') ORDER BY s.ordinal",vec![text(&room.to_string()),text(&phase_id.to_string())]).await? {
                let message_id:String=column(&row,0)?;
                let body:MemberResultV1=from_json(&column::<String>(&row,1)?)?;
                let speaker=SpeakerOrdinal{speaker_id:parse(&column::<String>(&row,2)?)?,ordinal:u32::try_from(column::<i64>(&row,3)?).map_err(|_|rt_error(ErrorCode::StorageUnavailable,"speaker_ordinal"))?};
                let mut claims=Vec::new();
                for claim in rows(conn,"SELECT i.claim_id,c.statement FROM rt_claim_ids i JOIN rt_claims c ON c.room_id=i.room_id AND c.message_id=i.message_id AND c.local_key=i.local_key WHERE i.room_id=? AND i.message_id=? ORDER BY i.local_key",vec![text(&room.to_string()),text(&message_id)]).await? {claims.push(PublishedClaim{claim_id:parse(&column::<String>(&claim,0)?)?,text:column(&claim,1)?});}
                let mut responses=Vec::new();
                for response in rows(conn,"SELECT r.response_id,r.stance,i.claim_id,d.target_response_id,d.body_json FROM rt_responses r JOIN rt_claim_ids i ON i.room_id=r.room_id AND i.message_id=r.target_message_id AND i.local_key=r.target_local_key JOIN rt_response_details d ON d.room_id=r.room_id AND d.response_id=r.response_id WHERE r.room_id=? AND r.message_id=? ORDER BY r.response_id",vec![text(&room.to_string()),text(&message_id)]).await? {
                    let data:Value=from_json(&column::<String>(&response,4)?)?;
                    let priority=data.get("priority").cloned().map(serde_json::from_value).transpose().map_err(|_|rt_error(ErrorCode::StorageUnavailable,"response_priority"))?.unwrap_or(Priority::Normal);
                    responses.push(PublishedResponse{response_id:parse(&column::<String>(&response,0)?)?,publication_seq,stance:from_json(&serialized(&column::<String>(&response,1)?)?)?,priority,target_claim_id:if column::<Option<String>>(&response,3)?.is_none(){Some(parse(&column::<String>(&response,2)?)?)}else{None},target_response_id:column::<Option<String>>(&response,3)?.map(|value|parse(&value)).transpose()?});
                }
                let status=if body.kind==MemberKind::Abstain {PublicationStatus::Abstained} else {PublicationStatus::Accepted};
                members.push(PublishedMember{speaker,kind:body.kind,summary:body.summary,status,claims,responses});
            }
        }
        if kind != PhaseKind::Synthesis {
            let snapshot=optional_row(conn,"SELECT snapshot_json FROM rt_phase_contexts WHERE room_id=? AND phase_id=?",vec![text(&room.to_string()),text(&phase_id.to_string())]).await?;
            let expected:Vec<SpeakerOrdinal>=if let Some(snapshot)=snapshot {
                from_json::<PhaseSnapshotV1>(&column::<String>(&snapshot,0)?)?.members.into_iter().map(|member|SpeakerOrdinal{speaker_id:member.speaker_id,ordinal:member.ordinal}).collect()
            } else {
                let mut expected=Vec::new();
                for row in rows(conn,"SELECT speaker_id,ordinal FROM rt_speakers WHERE room_id=? ORDER BY ordinal LIMIT (SELECT expected FROM rt_phases WHERE room_id=? AND phase_id=?)",vec![text(&room.to_string()),text(&room.to_string()),text(&phase_id.to_string())]).await? {
                    expected.push(SpeakerOrdinal{speaker_id:parse(&column::<String>(&row,0)?)?,ordinal:u32::try_from(column::<i64>(&row,1)?).map_err(|_|rt_error(ErrorCode::StorageUnavailable,"speaker_ordinal"))?});
                }
                expected
            };
            for speaker in expected {
                if members.iter().any(|member|member.speaker.speaker_id==speaker.speaker_id){continue;}
                let latest=optional_row(conn,"SELECT a.state FROM rt_turns t JOIN rt_attempts a ON a.room_id=t.room_id AND a.turn_id=t.turn_id WHERE t.room_id=? AND t.phase_id=? AND t.speaker_id=? ORDER BY a.attempt_no DESC LIMIT 1",vec![text(&room.to_string()),text(&phase_id.to_string()),text(&speaker.speaker_id.to_string())]).await?;
                let state=latest.as_ref().map(|row|column::<String>(row,0)).transpose()?;
                let status=match state.as_deref(){Some("failed"|"invalid"|"timed_out")=>PublicationStatus::Failed,_=>PublicationStatus::Absent};
                members.push(PublishedMember{speaker,kind:MemberKind::Abstain,summary:String::new(),status,claims:Vec::new(),responses:Vec::new()});
            }
            members.sort_by_key(|member|member.speaker.ordinal);
        }
        history.phases.push(PublishedPhase{phase_id,phase_index,kind,publication_seq,members});
    }
    Ok(history)
}
