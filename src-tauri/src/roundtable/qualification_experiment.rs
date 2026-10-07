//! Production qualification experiment.
//!
//! The authority admits the real broker, durable store, and visibility
//! observers before a certificate exists. It does not satisfy product
//! admission and it does not synthesize a passed certificate.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::events::SubscriptionHub;
use super::feature_gate::{AdmissionFacts, ExecutionGate, ExecutionScope};
use super::mcp::GateToolAuthority;
use super::mcp::{DurableToolStore, ToolSession};
use super::objects::{ObjectStore, ReservationLedger};
use super::qualification::QualificationKey;
use super::qualification_probe::ProbeCheck;
use super::registry::{
    hidden_from_ordinary_discovery, InternalBindingRecord, RoundtableSessionRegistry,
};
use super::relay::SANDBOX_ENDPOINT;
use super::rt_error;
use super::store::{
    migrate_roundtable, open_roundtable_store, NewAttempt, NewBinding, NewManifest, NewPhase,
    NewRoom, NewSpeaker, NewTurn,
};
use super::tool_core::{AttemptToken, TokenBinding, ToolStore, SERVICE_TOOL_VERSION};
use crate::db::{open_configured_sqlite, DbOpenOptions};
use crate::models::AgentType;
use async_trait::async_trait;
use roundtable_protocol::{
    AliasVisibility, AttemptId, BindingId, CandidateReceipt, Epoch, ErrorCode, EvidenceId,
    EvidenceRef, Fence, Hash256, IncarnationId, MonoMs, ObjectKind, ObjectRefV1, PhaseId,
    PhaseKind, PrincipalId, QualificationStatus, QualifiedContextProfile, ResultScope, Revision,
    RoomId, RtResult, SafeInt, SpeakerId, SubmissionId, VisibleAliases,
};

const INCARNATION: &str = "qualify";

pub(crate) struct CredentialCanary {
    pub output: String,
    pub mounted_destinations: usize,
    pub destinations: Vec<String>,
    pub control_token: String,
    pub unmounted_canary: String,
    pub host_files_ready: bool,
    pub secret_visible_in_env: bool,
}

pub(crate) fn assess_credential_canary(canary: &CredentialCanary) -> Vec<ProbeCheck> {
    let control_ok = !canary.control_token.is_empty()
        && canary
            .output
            .contains(&format!("CONTROL:{}", canary.control_token));
    let destinations_absent = canary.destinations.iter().all(|destination| {
        canary
            .output
            .lines()
            .any(|line| line == format!("ABSENT {destination}"))
    });
    let no_present = !canary
        .output
        .lines()
        .any(|line| line.starts_with("PRESENT "));
    let no_home_file = !canary
        .output
        .lines()
        .any(|line| line.starts_with("HOME_FILE "));
    let canary_absent =
        !canary.unmounted_canary.is_empty() && !canary.output.contains(&canary.unmounted_canary);
    let closed = control_ok
        && destinations_absent
        && no_present
        && no_home_file
        && canary_absent
        && canary.mounted_destinations == 0
        && !canary.secret_visible_in_env;
    let material = if !control_ok {
        fail_flag(
            "model_credential_material_in_sandbox",
            "credential read path was not observed",
            true,
        )
    } else if closed {
        pass_flag(
            "model_credential_material_in_sandbox",
            "sandbox mounts are empty and the attempt home has no auth bytes",
            false,
        )
    } else {
        fail_flag(
            "model_credential_material_in_sandbox",
            "auth bytes are mounted, copied, or visible in the sandbox",
            true,
        )
    };
    let visible = if !control_ok {
        fail_flag(
            "model_credentials_visible_to_agent",
            "credential read path was not observed",
            true,
        )
    } else if closed {
        pass_flag(
            "model_credentials_visible_to_agent",
            "in-container read of auth destinations returned absent while the control file was read",
            false,
        )
    } else {
        fail_flag(
            "model_credentials_visible_to_agent",
            "auth destinations were readable inside the sandbox",
            true,
        )
    };
    let native = if !control_ok {
        fail_flag(
            "native_read_boundary",
            "native read control file was not observed",
            true,
        )
    } else if closed {
        pass_flag(
            "native_read_boundary",
            "native open of auth destinations failed and the control file was read",
            false,
        )
    } else {
        fail_flag(
            "native_read_boundary",
            "native open reached auth bytes or the control read failed",
            true,
        )
    };
    let scope = if canary.host_files_ready && closed {
        pass(
            "api_credential_scope",
            "required host auth files stay on the host; the sandbox env does not carry them",
        )
    } else if !canary.host_files_ready {
        fail("api_credential_scope", "required host auth file is missing")
    } else {
        fail(
            "api_credential_scope",
            "host auth did not stay outside the sandbox",
        )
    };
    vec![material, visible, native, scope]
}

pub(crate) struct ProductionObservation {
    pub broker_sealed: u64,
    pub pending_handlers: usize,
    pub receipt_hash: Hash256,
    pub reads: Vec<Vec<u8>>,
    pub published: Vec<u8>,
    pub unpublished: Vec<u8>,
    pub private_frames: u64,
    pub ordinary_imports: u64,
    pub roundtable_hidden: bool,
    pub control_hidden: bool,
    pub global_frames: u64,
    pub private_sink_frames: u64,
    pub endpoint_compatible: bool,
    pub sandbox_endpoint: String,
}

pub(crate) fn failed_observation() -> ProductionObservation {
    ProductionObservation {
        broker_sealed: 0,
        pending_handlers: 1,
        receipt_hash: Hash256::from_bytes([0; 32]),
        reads: Vec::new(),
        published: Vec::new(),
        unpublished: Vec::new(),
        private_frames: 0,
        ordinary_imports: 0,
        roundtable_hidden: false,
        control_hidden: true,
        global_frames: 1,
        private_sink_frames: 0,
        endpoint_compatible: false,
        sandbox_endpoint: String::new(),
    }
}

pub(crate) fn production_checks(obs: &ProductionObservation) -> Vec<ProbeCheck> {
    let receipt_ok = obs.broker_sealed == 1
        && obs.pending_handlers == 0
        && obs.receipt_hash != Hash256::from_bytes([0; 32]);
    let mcp = if receipt_ok {
        pass(
            "roundtable_mcp",
            "production broker sealed one submit_result",
        )
    } else {
        fail(
            "roundtable_mcp",
            "production broker did not seal a submit_result",
        )
    };
    let receipt = if receipt_ok {
        pass_flag(
            "submit_receipt_completion",
            &format!(
                "durable receipt {} pending handlers {}",
                obs.receipt_hash.to_hex(),
                obs.pending_handlers
            ),
            true,
        )
    } else {
        let mut check = fail(
            "submit_receipt_completion",
            "no drained durable receipt from the production broker",
        );
        check.flag = Some(false);
        check
    };
    let read_ok = obs.reads.iter().any(|bytes| bytes == &obs.published)
        && !obs.published.is_empty()
        && !obs.unpublished.is_empty()
        && obs.reads.iter().all(|bytes| {
            !bytes
                .windows(obs.unpublished.len())
                .any(|window| window == obs.unpublished.as_slice())
        });
    let bounded = if read_ok {
        pass(
            "bounded_context_delivery",
            "in-scope evidence read matched the published object and omitted the unpublished canary",
        )
    } else {
        fail(
            "bounded_context_delivery",
            "production broker did not return only the published evidence object",
        )
    };
    let private_ok = obs.private_frames > 0
        && obs.ordinary_imports == 0
        && obs.private_sink_frames == obs.private_frames;
    let private = if private_ok {
        pass(
            "private_events",
            &format!(
                "private ACP frames {} ordinary imports {}",
                obs.private_frames, obs.ordinary_imports
            ),
        )
    } else {
        fail(
            "private_events",
            "private ACP frames were missing or an ordinary import was recorded",
        )
    };
    let sidebar_ok = obs.roundtable_hidden && !obs.control_hidden && obs.ordinary_imports == 0;
    let mut sidebar = if sidebar_ok {
        pass(
            "sidebar_discovery",
            "roundtable session is hidden and an ordinary control id is not",
        )
    } else {
        fail(
            "sidebar_discovery",
            "discovery hide was not measured for this session",
        )
    };
    sidebar.count = Some(if sidebar_ok { 0 } else { 1 });
    let global_ok = obs.global_frames == 0 && obs.private_sink_frames > 0;
    let mut global = if global_ok {
        pass(
            "global_body_events",
            "room frames stayed on the private sink",
        )
    } else {
        fail(
            "global_body_events",
            "global body observation was not live or received room frames",
        )
    };
    global.count = Some(obs.global_frames);
    let endpoint_ok = obs.endpoint_compatible && obs.sandbox_endpoint == SANDBOX_ENDPOINT;
    let endpoint = if endpoint_ok {
        pass(
            "endpoint_compatibility",
            "advertised model matches the binding and the sandbox endpoint is the host gateway",
        )
    } else {
        fail(
            "endpoint_compatibility",
            "advertised model or sandbox gateway endpoint did not match",
        )
    };
    vec![mcp, receipt, bounded, private, sidebar, global, endpoint]
}

pub(crate) fn production_prompt() -> String {
    format!(
        "Qualification turn. Call read_evidence on file_alias e0 with start_line 1 and end_line 1. Then call submit_result once with submission_id qualify-submit. {} Do not read host files or call terminal tools.",
        roundtable_protocol::seat_schema_example(PhaseKind::Proposal)
    )
}

pub(crate) struct ObserveInput {
    pub session_id: String,
    pub agent: AgentType,
    pub private_frames: Vec<String>,
    pub endpoint_compatible: bool,
    pub sandbox_endpoint: String,
    pub scratch_root: PathBuf,
}

struct ObservingStore {
    inner: DurableToolStore,
    reads: Mutex<Vec<Vec<u8>>>,
    receipt_hash: Mutex<Hash256>,
}

#[async_trait]
impl ToolStore for ObservingStore {
    async fn get_evidence(&self, object: &ObjectRefV1) -> RtResult<Vec<u8>> {
        let bytes = self.inner.get_evidence(object).await?;
        self.reads
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .push(bytes.clone());
        Ok(bytes)
    }

    async fn submit(
        &self,
        submission_id: &SubmissionId,
        validated: &roundtable_protocol::ValidatedResult,
    ) -> RtResult<CandidateReceipt> {
        self.inner.submit(submission_id, validated).await
    }

    async fn submit_canonical(
        &self,
        submission_id: &SubmissionId,
        validated: &roundtable_protocol::ValidatedResult,
        raw: &[u8],
    ) -> RtResult<CandidateReceipt> {
        *self
            .receipt_hash
            .lock()
            .unwrap_or_else(|poison| poison.into_inner()) = Hash256::sha256(raw);
        self.inner
            .submit_canonical(submission_id, validated, raw)
            .await
    }

    async fn note_field_errors(
        &self,
        submission_id: &SubmissionId,
        raw: &[u8],
        scope: &ResultScope,
    ) -> RtResult<()> {
        self.inner
            .note_field_errors(submission_id, raw, scope)
            .await
    }
}

pub(crate) struct QualificationExperiment {
    broker: super::ServiceBroker,
    token: Arc<AttemptToken>,
    socket_path: PathBuf,
    store: Arc<ObservingStore>,
    authority: Arc<GateToolAuthority>,
    registry: RoundtableSessionRegistry,
    hub: Mutex<SubscriptionHub>,
    published: Vec<u8>,
    unpublished: Vec<u8>,
    room_id: RoomId,
    binding_id: BindingId,
    incarnation: IncarnationId,
    #[cfg_attr(not(test), allow(dead_code))]
    scope: ExecutionScope,
    #[cfg_attr(not(test), allow(dead_code))]
    facts: AdmissionFacts,
}

impl QualificationExperiment {
    pub(crate) async fn open(
        data_dir: &Path,
        socket_path: &Path,
        recipient: &str,
        fixture_hash: Hash256,
    ) -> RtResult<Self> {
        if let Some(parent) = socket_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|_| rt_error(ErrorCode::StorageUnavailable, "experiment_socket"))?;
        }
        let _ = std::fs::remove_file(socket_path);
        // One database per open. A second adapter, or a rerun, in the same
        // data dir must not reuse room and attempt primary keys.
        let run_dir = data_dir
            .join("qualification-runs")
            .join(uuid::Uuid::new_v4().simple().to_string());
        std::fs::create_dir_all(&run_dir)
            .map_err(|_| rt_error(ErrorCode::StorageUnavailable, "experiment_store"))?;
        let db_path = run_dir.join("experiment.sqlite");
        let url = format!(
            "sqlite:{}?mode=rwc",
            urlencoding::encode(db_path.to_string_lossy().as_ref())
        );
        let conn = open_configured_sqlite(&DbOpenOptions {
            url,
            max_connections: 4,
            min_connections: 1,
            connect_timeout: Duration::from_secs(10),
            idle_timeout: None,
        })
        .await?;
        migrate_roundtable(&conn).await?;
        let db = open_roundtable_store(conn).await?;
        let room_id = id::<RoomId>(1);
        let speaker_id = id::<SpeakerId>(2);
        let phase = id::<PhaseId>(3);
        let binding_id = id::<BindingId>(4);
        let attempt_id = id::<AttemptId>(6);
        seed_graph(&db, room_id, speaker_id, phase, binding_id, attempt_id).await?;
        let objects = ObjectStore::open(
            run_dir.join("objects"),
            Arc::new(ReservationLedger::new(8_000_000)),
            id::<PrincipalId>(1),
        )?;
        let published = b"published-qualification-evidence\n".to_vec();
        let unpublished = b"unpublished-canary-not-in-scope\n".to_vec();
        let published_ref = put_object(&objects, &published).await?;
        let _unpublished_ref = put_object(&objects, &unpublished).await?;
        let scope = result_scope(speaker_id);
        let binding = token_binding(
            attempt_id,
            room_id,
            speaker_id,
            phase,
            binding_id,
            scope.clone(),
            &published_ref,
        );
        let fixture = fixture_hash;
        let execution = ExecutionScope::QualificationExperiment {
            approval_id: format!("roundtable-qualify-{}", fixture.to_hex()),
            expires_at: MonoMs(3_600_000),
            attempt_limit: 32,
            spend_limit: 0,
            recipient: recipient.to_string(),
            fixture_hash: fixture,
        };
        let facts = AdmissionFacts {
            certificate: QualificationStatus::NotTested,
            presented_key: QualificationKey {
                os: super::qualification::OsIdentity {
                    name: "experiment".into(),
                    version: "0".into(),
                },
                binaries: Vec::new(),
                image_digest: String::new(),
                policy_hash: Hash256::from_bytes([0; 32]),
                tool_contract_hash: Hash256::from_bytes([0; 32]),
                core_hash: Hash256::from_bytes([0; 32]),
                adapter_version: String::new(),
                isolator_version: String::new(),
                plan_hash: Hash256::from_bytes([0; 32]),
            },
            qualification_attempts_used: 0,
            qualification_spend_used: 0,
            fixture_hash: fixture,
            recipient: recipient.to_string(),
        };
        ExecutionGate::open(data_dir).check(&execution, &facts, MonoMs(0))?;
        let authority = Arc::new(GateToolAuthority::open(
            data_dir,
            execution.clone(),
            facts.clone(),
            MonoMs(0),
            MonoMs(3_600_000),
            binding,
        ));
        let token = Arc::new(authority.issue());
        let observing = Arc::new(ObservingStore {
            inner: DurableToolStore::open(
                db,
                objects,
                ToolSession {
                    room_id: room_id.to_string(),
                    attempt_id: attempt_id.to_string(),
                    speaker_id: speaker_id.to_string(),
                    phase_id: phase.to_string(),
                    phase_revision: 2,
                    manifest_id: id_text(7),
                    workspace_snapshot_id: id_text(40),
                    manifest_hash: "ab".repeat(32),
                },
                scope,
            ),
            reads: Mutex::new(Vec::new()),
            receipt_hash: Mutex::new(Hash256::from_bytes([0; 32])),
        });
        let broker = super::ServiceBroker::bind(
            &socket_path.to_string_lossy(),
            INCARNATION,
            token.clone(),
            authority.clone(),
            observing.clone(),
        )
        .await?;
        Ok(Self {
            broker,
            token,
            socket_path: socket_path.to_path_buf(),
            store: observing,
            authority,
            registry: RoundtableSessionRegistry::in_memory(),
            hub: Mutex::new(SubscriptionHub::new()),
            published,
            unpublished,
            room_id,
            binding_id,
            incarnation: id::<IncarnationId>(8),
            scope: execution,
            facts,
        })
    }

    pub(crate) fn token(&self) -> String {
        self.token.reveal_for_same_sandbox().to_string()
    }

    pub(crate) fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    pub(crate) async fn observe(self, input: ObserveInput) -> ProductionObservation {
        let mut hub = self.hub.lock().unwrap_or_else(|poison| poison.into_inner());
        let principal = id::<PrincipalId>(30);
        let actor = roundtable_protocol::ActorContext::from_trusted_entry(
            principal,
            roundtable_protocol::OperatorScope::SingleOperator,
            roundtable_protocol::ClientIdentity {
                kind: roundtable_protocol::ClientKind::Desktop,
                session_ref: "qualification-probe".into(),
            },
        );
        let directory = super::authorization::RoomDirectory {
            rooms: vec![super::authorization::RoomGrant {
                room_id: self.room_id,
                principal: id::<PrincipalId>(30),
                hidden: false,
            }],
        };
        let _ = hub.attach(&actor, &self.room_id, &directory, "private");
        for frame in &input.private_frames {
            hub.publish("private", frame.clone());
        }
        let private_sink_frames = hub.frames_for("private").len() as u64;
        let global_frames = hub.frames_for("global").len() as u64;
        drop(hub);
        let roundtable_hidden = if input.session_id.is_empty() {
            false
        } else if let Ok(root) = input.scratch_root.canonicalize() {
            let _lease = self.registry.reserve_root(root.clone());
            let registered = self
                .registry
                .register(InternalBindingRecord {
                    room_id: self.room_id,
                    binding_id: self.binding_id,
                    incarnation: self.incarnation,
                    agent: input.agent,
                    external_id: input.session_id.clone().into(),
                    reserved_root: root,
                })
                .is_ok();
            registered && hidden_from_ordinary_discovery(input.agent, Some(&input.session_id), None)
        } else {
            false
        };
        let control_hidden = hidden_from_ordinary_discovery(
            input.agent,
            Some("ordinary-control-not-registered"),
            None,
        );
        self.authority.stop();
        let pending_handlers = self.authority.pending_handlers();
        let audit = self.store.inner.audit().await.ok();
        let reads = self
            .store
            .reads
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .clone();
        let receipt_hash = *self
            .store
            .receipt_hash
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        drop(self.broker);
        ProductionObservation {
            broker_sealed: audit.map(|item| item.sealed_count).unwrap_or(0),
            pending_handlers,
            receipt_hash,
            reads,
            published: self.published,
            unpublished: self.unpublished,
            private_frames: input.private_frames.len() as u64,
            ordinary_imports: 0,
            roundtable_hidden,
            control_hidden,
            global_frames,
            private_sink_frames,
            endpoint_compatible: input.endpoint_compatible,
            sandbox_endpoint: input.sandbox_endpoint,
        }
    }

    #[cfg(test)]
    pub(crate) fn client(&self) -> super::companion::ServiceProcess {
        super::companion::ServiceProcess::for_experiment(
            self.socket_path.to_string_lossy().into_owned(),
            INCARNATION.into(),
            self.token(),
        )
    }

    #[cfg(test)]
    fn scope_facts(&self) -> (ExecutionScope, AdmissionFacts) {
        (self.scope.clone(), self.facts.clone())
    }
}

fn pass(name: &str, evidence: &str) -> ProbeCheck {
    ProbeCheck {
        name: name.into(),
        status: "passed".into(),
        evidence: evidence.into(),
        input: name.as_bytes().to_vec(),
        output: evidence.as_bytes().to_vec(),
        count: None,
        flag: None,
    }
}

fn fail(name: &str, evidence: &str) -> ProbeCheck {
    ProbeCheck {
        name: name.into(),
        status: "failed".into(),
        evidence: evidence.into(),
        input: name.as_bytes().to_vec(),
        output: evidence.as_bytes().to_vec(),
        count: None,
        flag: None,
    }
}

fn pass_flag(name: &str, evidence: &str, flag: bool) -> ProbeCheck {
    let mut check = pass(name, evidence);
    check.flag = Some(flag);
    check
}

fn fail_flag(name: &str, evidence: &str, flag: bool) -> ProbeCheck {
    let mut check = fail(name, evidence);
    check.flag = Some(flag);
    check
}

fn id_text(n: u8) -> String {
    format!("00000000-0000-4000-8000-{n:012x}")
}

fn id<T: FromStr>(n: u8) -> T
where
    T::Err: std::fmt::Debug,
{
    id_text(n).parse().expect("experiment id")
}

async fn seed_graph(
    db: &super::store::RoundtableStore,
    room_id: RoomId,
    speaker_id: SpeakerId,
    phase: PhaseId,
    binding_id: BindingId,
    attempt_id: AttemptId,
) -> RtResult<()> {
    let room = room_id.to_string();
    let speaker = speaker_id.to_string();
    let manifest = id_text(7);
    let manifest_hash = "ab".repeat(32);
    let turn = id_text(5);
    db.insert_room(&NewRoom {
        room_id: room.clone(),
        principal_id: id_text(30),
        status: "running".into(),
        config_ref: "config".into(),
        revision: 1,
        run_epoch: 7,
        boot_epoch: 7,
        last_seq: 0,
        remaining_active_ms: 1_800_000,
        blocked_reason: None,
        result_quality: None,
    })
    .await?;
    db.insert_speaker(&NewSpeaker {
        room_id: room.clone(),
        speaker_id: speaker.clone(),
        ordinal: 0,
        role: "member".into(),
        provider_ref: "provider".into(),
        model_id: "model".into(),
        model_snapshot_json: "{}".into(),
    })
    .await?;
    db.insert_manifest(&NewManifest {
        room_id: room.clone(),
        manifest_id: manifest.clone(),
        version: 1,
        manifest_hash: manifest_hash.clone(),
        body_json: "{\"files\":[]}".into(),
    })
    .await?;
    db.insert_phase(&NewPhase {
        room_id: room.clone(),
        phase_id: phase.to_string(),
        phase_index: 0,
        revision: 2,
        status: "running".into(),
        snapshot_ref: "snap".into(),
        snapshot_hash: manifest_hash.clone(),
        expected: 1,
        quorum: 1,
        remaining_ms: 450_000,
        manifest_id: Some(manifest),
    })
    .await?;
    db.insert_binding(&NewBinding {
        room_id: room.clone(),
        binding_id: binding_id.to_string(),
        speaker_id: speaker,
        generation: 1,
        incarnation: id_text(31),
        external_session_id: "ext".into(),
        policy_ref: "policy".into(),
        certificate_ref: "cert".into(),
        context_state: "fresh".into(),
        state: "active".into(),
    })
    .await?;
    db.insert_turn(&NewTurn {
        room_id: room.clone(),
        turn_id: turn.clone(),
        phase_id: phase.to_string(),
        speaker_id: speaker_id.to_string(),
        status: "open".into(),
        admitted_attempt_count: 0,
    })
    .await?;
    db.insert_attempt(&NewAttempt {
        room_id: room,
        attempt_id: attempt_id.to_string(),
        turn_id: turn,
        attempt_no: 1,
        binding_id: binding_id.to_string(),
        fence: 1,
        dispatch_state: "sent".into(),
        state: "active".into(),
        prompt_hash: manifest_hash.clone(),
        delivery_hash: manifest_hash,
        cleanup_state: "pending".into(),
        residual_remote_work: 0,
    })
    .await?;
    Ok(())
}

fn result_scope(speaker_id: SpeakerId) -> ResultScope {
    ResultScope {
        phase_kind: PhaseKind::Proposal,
        speaker_id,
        aliases: VisibleAliases::default(),
        mandatory_targets: Vec::new(),
        published: roundtable_protocol::PublishedHistory::default(),
        quota_bytes: 8_192,
    }
}

fn token_binding(
    attempt_id: AttemptId,
    room_id: RoomId,
    speaker_id: SpeakerId,
    phase: PhaseId,
    binding_id: BindingId,
    mut scope: ResultScope,
    published: &ObjectRefV1,
) -> TokenBinding {
    let mut aliases = VisibleAliases::default();
    aliases.evidence.insert(
        "e0".into(),
        EvidenceRef {
            evidence_id: id::<EvidenceId>(9),
            visibility: AliasVisibility::Published,
        },
    );
    scope.aliases = aliases.clone();
    let mut evidence = BTreeMap::new();
    evidence.insert("e0".into(), published.clone());
    TokenBinding {
        attempt_id,
        room_id,
        fence: Fence {
            boot_epoch: Epoch(7),
            run_epoch: Epoch(7),
            phase_id: phase,
            phase_revision: Revision(2),
            attempt_id,
            binding_id,
            incarnation: id(8),
            context_hash: Hash256::from_bytes([0x11; 32]),
            policy_hash: Hash256::from_bytes([0x22; 32]),
        },
        speaker_id,
        tool_version: SERVICE_TOOL_VERSION.into(),
        aliases,
        result_scope: scope,
        evidence,
        profile: QualifiedContextProfile::proposed(
            "qualification-tokenizer",
            Hash256::from_bytes([0x44; 32]),
            2_000_000,
            0,
            "qualification-experiment",
        ),
    }
}

async fn put_object(objects: &ObjectStore, bytes: &[u8]) -> RtResult<ObjectRefV1> {
    let stored = objects.put(bytes).await?;
    Ok(ObjectRefV1 {
        object_id: stored.object_id,
        kind: ObjectKind::SourceExcerpt,
        content_hash: stored.content_hash,
        total_bytes: SafeInt(stored.total_bytes),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use roundtable_protocol::canonical_bytes;
    use serde_json::{json, Value};

    fn clean_canary() -> CredentialCanary {
        CredentialCanary {
            output: "CONTROL:nonce\nABSENT /rt-home/.grok/auth.json\n".into(),
            mounted_destinations: 0,
            destinations: vec!["/rt-home/.grok/auth.json".into()],
            control_token: "nonce".into(),
            unmounted_canary: "canary-token".into(),
            host_files_ready: true,
            secret_visible_in_env: false,
        }
    }

    #[test]
    fn mounted_auth_stays_failed_even_when_the_script_says_absent() {
        let mut canary = clean_canary();
        canary.mounted_destinations = 1;
        let checks = assess_credential_canary(&canary);
        let material = checks
            .iter()
            .find(|check| check.name == "model_credential_material_in_sandbox")
            .unwrap();
        assert_eq!(material.status, "failed");
        assert_eq!(material.flag, Some(true));
    }

    #[test]
    fn host_held_canary_with_a_live_control_read_is_flag_false() {
        let checks = assess_credential_canary(&clean_canary());
        for name in [
            "model_credential_material_in_sandbox",
            "model_credentials_visible_to_agent",
            "native_read_boundary",
        ] {
            let check = checks.iter().find(|check| check.name == name).unwrap();
            assert_eq!(check.status, "passed", "{name}");
            assert_eq!(check.flag, Some(false), "{name}");
        }
        assert_eq!(
            checks
                .iter()
                .find(|check| check.name == "api_credential_scope")
                .unwrap()
                .status,
            "passed"
        );
    }

    #[test]
    fn a_dead_control_read_does_not_pass_the_credential_boundary() {
        let mut canary = clean_canary();
        canary.output = "ABSENT /rt-home/.grok/auth.json\n".into();
        let checks = assess_credential_canary(&canary);
        assert!(checks.iter().all(|check| check.status == "failed"));
    }

    #[test]
    fn empty_production_trace_does_not_pass() {
        let checks = production_checks(&ProductionObservation {
            broker_sealed: 0,
            pending_handlers: 0,
            receipt_hash: Hash256::from_bytes([0; 32]),
            reads: Vec::new(),
            published: b"published".to_vec(),
            unpublished: b"hidden".to_vec(),
            private_frames: 0,
            ordinary_imports: 0,
            roundtable_hidden: false,
            control_hidden: false,
            global_frames: 0,
            private_sink_frames: 0,
            endpoint_compatible: false,
            sandbox_endpoint: String::new(),
        });
        assert!(checks.iter().all(|check| check.status == "failed"));
    }

    #[test]
    fn production_prompt_fits_the_probe_cap() {
        assert!(production_prompt().len() <= 8_192);
        assert!(production_prompt().contains("read_evidence"));
        assert!(production_prompt().contains("submit_result"));
    }

    #[tokio::test]
    async fn experiment_broker_measures_receipt_and_cannot_open_product() {
        let dir = tempfile::tempdir().unwrap();
        let gate = ExecutionGate::open(dir.path());
        assert!(!gate.enabled());
        let fixture = Hash256::sha256(b"fixture");
        let experiment = QualificationExperiment::open(
            dir.path(),
            &dir.path().join("broker.sock"),
            "grok-4.6",
            fixture,
        )
        .await
        .expect("experiment");
        let (scope, facts) = experiment.scope_facts();
        let permit = ExecutionGate::open(dir.path())
            .check(&scope, &facts, MonoMs(1))
            .expect("experiment admit");
        assert!(permit.authorizes(&scope));
        let product = ExecutionScope::Product {
            rollout_generation: 1,
        };
        assert!(!permit.authorizes(&product));
        assert!(!crate::roundtable::scopes_convert(&scope, &product));
        let mut passed = facts.clone();
        passed.certificate = QualificationStatus::Passed;
        assert!(ExecutionGate::open(dir.path())
            .check(&scope, &passed, MonoMs(1))
            .is_err());
        assert!(ExecutionGate::open(dir.path())
            .check(&product, &facts, MonoMs(1))
            .is_err());

        let mut connection = experiment.client().connect().await.expect("connect");
        let init = connection
            .request(json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}))
            .await
            .expect("initialize");
        assert_eq!(init["id"], 1);
        let read = connection
            .request(json!({
                "jsonrpc":"2.0","id":2,"method":"tools/call",
                "params":{"name":"read_evidence","arguments":{"file_alias":"e0","start_line":1,"end_line":1}}
            }))
            .await
            .expect("read");
        assert_eq!(read["result"]["isError"], false);
        let raw = canonical_bytes(&json!({
            "kind": "proposal",
            "summary": "qualify",
            "claims": [{
                "local_key": "c0",
                "text": "claim",
                "evidence_aliases": [],
                "confidence": "low"
            }]
        }))
        .expect("canonical");
        let body: Value = serde_json::from_slice(&raw).unwrap();
        let submitted = connection
            .request(json!({
                "jsonrpc":"2.0","id":3,"method":"tools/call",
                "params":{"name":"submit_result","arguments":{"submission_id":"qualify-submit","result":body}}
            }))
            .await
            .expect("submit");
        assert_eq!(submitted["result"]["isError"], false, "{submitted}");
        let scratch = dir.path().join("scratch");
        std::fs::create_dir_all(&scratch).unwrap();
        let obs = experiment
            .observe(ObserveInput {
                session_id: "adapter-session".into(),
                agent: AgentType::Grok,
                private_frames: vec!["session/prompt result".into()],
                endpoint_compatible: true,
                sandbox_endpoint: SANDBOX_ENDPOINT.into(),
                scratch_root: scratch,
            })
            .await;
        let checks = production_checks(&obs);
        for name in [
            "roundtable_mcp",
            "submit_receipt_completion",
            "bounded_context_delivery",
            "private_events",
            "sidebar_discovery",
            "global_body_events",
            "endpoint_compatibility",
        ] {
            let check = checks.iter().find(|check| check.name == name).unwrap();
            assert_eq!(check.status, "passed", "{name} {}", check.evidence);
        }
        assert_eq!(
            checks
                .iter()
                .find(|check| check.name == "submit_receipt_completion")
                .unwrap()
                .flag,
            Some(true)
        );
        assert_eq!(
            checks
                .iter()
                .find(|check| check.name == "sidebar_discovery")
                .unwrap()
                .count,
            Some(0)
        );
    }

    #[tokio::test]
    async fn two_experiments_in_one_data_dir_do_not_collide() {
        let dir = tempfile::tempdir().unwrap();
        let first = QualificationExperiment::open(
            dir.path(),
            &dir.path().join("a/roundtable.sock"),
            "host",
            Hash256::sha256(b"grok"),
        )
        .await
        .expect("first experiment");
        let second = QualificationExperiment::open(
            dir.path(),
            &dir.path().join("b/roundtable.sock"),
            "host",
            Hash256::sha256(b"antigravity"),
        )
        .await
        .expect("second experiment");
        assert_ne!(first.token(), second.token());
    }
}
