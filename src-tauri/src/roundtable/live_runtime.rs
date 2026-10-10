//! Qualified OCI + ACP participant implementation. No ordinary conversation
//! event bus is attached; the only data capabilities are the private brokers.
use super::attempt_trace::AttemptTrace;
use super::installed_runtime::InstalledRuntime;
use super::live_gateway::{LiveGatewayServer, LiveModelGateway};
use super::owned_runtime::{
    RoundtableTurnExecutor, RoundtableTurnOutcome, RoundtableTurnRequest, RuntimeCapability,
};
use super::sandbox::*;
use super::store::{column, exec, num, one_row, optional_row, text, RoundtableStore};
use super::{
    rt_error, AdmissionFacts, DurableToolStore, ExecutionGate, ExecutionPolicy, ExecutionScope,
    GateToolAuthority, ObjectStore, ReservationLedger, ServiceBroker, TokenBinding, ToolSession,
};
use async_trait::async_trait;
use roundtable_protocol::*;
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};

struct Active {
    store: RoundtableStore,
    room: RoomId,
    attempt: AttemptId,
    scratch: PathBuf,
    launched: std::sync::atomic::AtomicBool,
    instance: Mutex<Option<SandboxInstance>>,
    child: tokio::sync::Mutex<Option<tokio::process::Child>>,
    broker: tokio::sync::Mutex<Option<ServiceBroker>>,
    gateway_server: tokio::sync::Mutex<Option<LiveGatewayServer>>,
    gateway: Arc<LiveModelGateway>,
    authority: Arc<GateToolAuthority>,
    agent: String,
    cleanup: tokio::sync::Mutex<Option<CleanupProof>>,
    stderr: tokio::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
    assistant: Mutex<Option<super::DiagnosticCapture>>,
    finish_reason: Mutex<Option<String>>,
    /// Diagnostics-only per-attempt trace (observability; no behaviour).
    trace: Option<Arc<AttemptTrace>>,
    /// Display-only live output for the room page. Never read back.
    live: Arc<super::live_stream::LiveSink>,
    /// Host path behind `/workspace-ro`, set once the room's folder is
    /// resolved. Native read permissions are checked against it.
    workspace_root: std::sync::OnceLock<PathBuf>,
}

pub(crate) struct LiveParticipantExecutor {
    data_dir: PathBuf,
    adapters: BTreeMap<String, InstalledRuntime>,
    isolator: Arc<LinuxOciIsolator>,
    db_identity: DbIdentity,
    active: Mutex<HashMap<IncarnationId, Arc<Active>>>,
    completed_cleanup: Mutex<HashMap<IncarnationId, CleanupProof>>,
    launches: Mutex<HashMap<IncarnationId, bool>>,
    registry: tokio::sync::Mutex<Option<super::RoundtableSessionRegistry>>,
}

fn auth_denylist(installed: &InstalledRuntime, seeds: &[String]) -> RtResult<Vec<String>> {
    let mut secrets = seeds.to_vec();
    for mount in installed
        .oci
        .host_held_credentials
        .iter()
        .chain(installed.oci.auth_mounts.iter())
    {
        secrets.extend(super::diagnostics::auth_file_secrets(&mount.source)?);
    }
    secrets.sort();
    secrets.dedup();
    Ok(secrets)
}

/// Capability already verifies the certificate. Rechecking its exact bytes here
/// prevents a changed file from supplying a different envelope proof afterward.
fn verified_request_envelope_bound(installed: &InstalledRuntime) -> RtResult<Option<u64>> {
    use std::io::Read;
    let invalid = || {
        rt_error(
            ErrorCode::CapabilityUnqualified,
            "request_envelope_unqualified",
        )
    };
    let mut bytes = Vec::new();
    std::fs::File::open(&installed.report_path)
        .map_err(|_| invalid())?
        .take(1_048_577)
        .read_to_end(&mut bytes)
        .map_err(|_| invalid())?;
    if bytes.len() > 1_048_576 || Hash256::sha256(&bytes) != installed.report_sha256 {
        return Err(rt_error(
            ErrorCode::CapabilityUnqualified,
            "qualification_report_changed",
        ));
    }
    let report = parse_strict_json(
        &bytes,
        &ParseLimits {
            origin: LimitsOrigin::Custom,
            max_bytes: 1_048_576,
            max_depth: 32,
        },
    )?;
    let proof = &report["request_envelope"];
    if proof.is_null() || proof["status"] == "not_tested" {
        return Ok(None);
    }
    if proof["status"] != "passed" {
        return Err(invalid());
    }
    let bound = proof["max_bytes"]
        .as_u64()
        .filter(|bound| *bound <= installed.context_profile.max_request_body_bytes)
        .ok_or_else(invalid)?;
    let evidence: Hash256 =
        serde_json::from_value(proof["evidence_hash"].clone()).map_err(|_| invalid())?;
    if evidence == Hash256::from_bytes([0; 32]) {
        return Err(invalid());
    }
    Ok(Some(bound))
}

fn participant_agent(participant: &ParticipantV1) -> RtResult<String> {
    let agent = participant
        .agent
        .clone()
        .unwrap_or_else(|| "codex".to_string());
    if !is_roundtable_agent(&agent) {
        return Err(rt_error(ErrorCode::InvalidArgument, "unknown_agent"));
    }
    Ok(agent)
}

fn agent_type_for(agent: &str) -> RtResult<crate::models::AgentType> {
    crate::models::AgentType::from_wire(agent)
        .filter(|value| is_roundtable_agent(value.as_wire().as_ref()))
        .ok_or_else(|| rt_error(ErrorCode::InvalidArgument, "unknown_agent"))
}

fn file_auth_ready(installed: &InstalledRuntime) -> bool {
    // Only host-held files count. Sandbox mounts are refused and are not a
    // credential boundary.
    !installed.oci.host_held_credentials.is_empty()
        && installed.oci.host_held_credentials.iter().all(|mount| {
            let path = std::path::Path::new(&mount.source);
            path.is_absolute() && path.is_file()
        })
}

fn host_gateway_secret(
    installed: &InstalledRuntime,
    binding: &super::installed_runtime::ProviderBinding,
    settings_env: Option<&[(String, String)]>,
) -> RtResult<String> {
    if let Ok(value) = std::env::var(&binding.credential_env) {
        if !value.is_empty() {
            return Ok(value);
        }
    }
    // Direct-egress adapters (Cursor custom mode, CodeBuddy) send their own
    // settings key; the gateway only needs a host credential for its origin.
    if let Some((_, value)) = settings_env.and_then(|env| env.first()) {
        return Ok(value.clone());
    }
    for mount in &installed.oci.host_held_credentials {
        if let Ok(text) = std::fs::read_to_string(&mount.source) {
            if !text.is_empty() {
                return Ok(text);
            }
        }
    }
    Err(rt_error(
        ErrorCode::CapabilityUnqualified,
        "provider_credential_missing",
    ))
}

fn credential_ready(
    installed: &InstalledRuntime,
    binding: &super::installed_runtime::ProviderBinding,
    settings_credential: bool,
) -> bool {
    settings_credential
        || std::env::var_os(&binding.credential_env).is_some()
        || file_auth_ready(installed)
}

fn require_provider_credential(
    installed: &InstalledRuntime,
    binding: &super::installed_runtime::ProviderBinding,
    settings_credential: bool,
) -> RtResult<()> {
    if !credential_ready(installed, binding, settings_credential) {
        return Err(rt_error(
            ErrorCode::CapabilityUnqualified,
            "provider_credential_missing",
        ));
    }
    Ok(())
}

impl LiveParticipantExecutor {
    pub(crate) fn load(data_dir: PathBuf) -> RtResult<Self> {
        let adapters = InstalledRuntime::load_catalog(&data_dir)?;
        let canonical = data_dir
            .join(crate::db::database_file_name())
            .canonicalize()
            .map_err(|_| rt_error(ErrorCode::StorageUnavailable, "db_identity"))?;
        let db_identity = DbIdentity::new(canonical.to_string_lossy().into_owned())?;
        let journal = JournalLaunchIntentStore::open(&data_dir.join("roundtable/launch-journal"))?;
        let profile = adapters
            .values()
            .next()
            .ok_or_else(|| rt_error(ErrorCode::CapabilityUnqualified, "runtime_installation"))?
            .oci
            .clone();
        let isolator = Arc::new(LinuxOciIsolator::with_profile(journal, profile));
        Ok(Self {
            data_dir,
            adapters,
            isolator,
            db_identity,
            active: Mutex::new(HashMap::new()),
            completed_cleanup: Mutex::new(HashMap::new()),
            launches: Mutex::new(HashMap::new()),
            registry: tokio::sync::Mutex::new(None),
        })
    }

    /// The seat's settings credential (Codeg agent settings), read fresh for
    /// every turn so a key the user just saved applies without a restart.
    fn settings_env(&self, agent: &str) -> Option<Vec<(String, String)>> {
        let profile = super::qualification_profiles::profile_for_agent(agent)?;
        let settings = super::agent_credentials::read_agent_settings(&self.data_dir, agent);
        super::agent_credentials::settings_credential_env(profile, &settings)
    }

    /// The seat's saved CodeBuddy profile (read fresh each turn), or the
    /// named reason it cannot run: missing, disabled, or not a CodeBuddy seat.
    fn seat_profile(
        &self,
        participant: &ParticipantV1,
    ) -> RtResult<Option<super::agent_credentials::CodeBuddyProfile>> {
        let Some(id) = participant.profile_id.as_deref() else {
            return Ok(None);
        };
        if participant.agent.as_deref() != Some("code_buddy") {
            return Err(rt_error(ErrorCode::InvalidArgument, "profile"));
        }
        super::agent_credentials::codebuddy_profile(&self.data_dir, id)
            .map(Some)
            .map_err(|reason| rt_error(ErrorCode::CapabilityUnqualified, reason))
    }

    fn adapter(&self, agent: &str) -> RtResult<InstalledRuntime> {
        self.adapters
            .get(agent)
            .cloned()
            .ok_or_else(|| rt_error(ErrorCode::CapabilityUnqualified, "adapter_unqualified"))
    }

    fn isolator_for(&self, agent: &str) -> RtResult<LinuxOciIsolator> {
        Ok(self.isolator.for_profile(self.adapter(agent)?.oci))
    }

    fn retire_incarnation(&self, incarnation: IncarnationId) -> RtResult<()> {
        let name = incarnation.to_string();
        for installed in self.adapters.values() {
            retire_attempt_files(&installed.oci.runtime_root, &name)?;
        }
        Ok(())
    }
    pub(crate) fn discovery(&self) -> Arc<dyn IsolationProvider + Send + Sync> {
        self.isolator.clone()
    }

    async fn verified(&self, agent: &str) -> RtResult<InstalledRuntime> {
        let installed = self.adapter(agent)?;
        tokio::task::spawn_blocking(move || {
            installed.verify_report()?;
            installed.verify_host()?;
            if !cfg!(target_os = "linux") {
                return Err(rt_error(
                    ErrorCode::PolicyUnenforceable,
                    "platform_unqualified",
                ));
            }
            verify_qualified_oci_profile(&installed.oci, &installed.qualification_key)?;
            let manifest = super::capabilities::sealed_service_manifest();
            if installed.qualification_key.policy_hash
                != super::capabilities::policy_hash_for(&manifest)?
            {
                return Err(rt_error(
                    ErrorCode::CapabilityUnqualified,
                    "qualification_policy_changed",
                ));
            }
            Ok(installed)
        })
        .await
        .map_err(|_| rt_error(ErrorCode::RuntimeUnavailable, "qualification_worker"))?
    }
    fn policy(
        &self,
        installed: &InstalledRuntime,
        model: &str,
    ) -> RtResult<(ExecutionScope, AdmissionFacts)> {
        let policy: ExecutionPolicy = serde_json::from_slice(
            &std::fs::read(self.data_dir.join("roundtable/execution-policy.json"))
                .map_err(|_| rt_error(ErrorCode::CapabilityUnqualified, "product_disabled"))?,
        )
        .map_err(|_| rt_error(ErrorCode::CapabilityUnqualified, "policy_unreadable"))?;
        let scope = ExecutionScope::Product {
            rollout_generation: policy.generation,
        };
        let facts = AdmissionFacts {
            certificate: QualificationStatus::Passed,
            presented_key: installed.qualification_key.clone(),
            qualification_attempts_used: 0,
            qualification_spend_used: 0,
            fixture_hash: Hash256::from_bytes([0; 32]),
            recipient: model.to_owned(),
        };
        ExecutionGate::open(&self.data_dir).check(&scope, &facts, MonoMs(0))?;
        Ok((scope, facts))
    }

    async fn reap_active(&self, incarnation: IncarnationId) -> RtResult<CleanupProof> {
        if let Some(proof) = self
            .completed_cleanup
            .lock()
            .expect("completed cleanup")
            .get(&incarnation)
            .cloned()
        {
            return Ok(proof);
        }
        let active = self
            .active
            .lock()
            .expect("active runtimes")
            .get(&incarnation)
            .cloned();
        let Some(active) = active else {
            let not_spawned = self
                .launches
                .lock()
                .expect("launch lifecycle")
                .get(&incarnation)
                == Some(&false);
            if not_spawned {
                // Preserve known pre-exec ownership if file retirement fails.
                self.retire_incarnation(incarnation)?;
                self.launches
                    .lock()
                    .expect("launch lifecycle")
                    .remove(&incarnation);
                return Ok(CleanupProof {
                    process: ProcessTreeProof {
                        instance_id: format!("not-spawned-{incarnation}"),
                        incarnation,
                        process_tree_empty: true,
                    },
                    mailbox_empty: true,
                    tools_drained: true,
                    ingress_drained: true,
                });
            }
            let instance = self
                .isolator
                .recorded_instance(&self.db_identity, incarnation)?
                .ok_or_else(|| {
                    rt_error(ErrorCode::RuntimeUnavailable, "cleanup_identity_unknown")
                })?;
            let process = self.isolator.reap(&instance).await?;
            self.retire_incarnation(incarnation)?;
            return Ok(CleanupProof {
                process,
                mailbox_empty: true,
                tools_drained: true,
                ingress_drained: true,
            });
        };
        let mut cached = active.cleanup.lock().await;
        if let Some(proof) = cached.as_ref() {
            // Physical cleanup already succeeded. This record now owns only
            // pending durable facts; never rerun a process to retry storage.
            return self.persist_reaped(&active, proof).await;
        }
        trace_reap_snapshot(&active).await;
        active.live.end();
        active.authority.stop();
        active.gateway.revoke();
        if let Some(broker) = active.broker.lock().await.take() {
            broker.shutdown().await;
        }
        if let Some(server) = active.gateway_server.lock().await.take() {
            server.shutdown().await;
        }
        if let Some(mut child) = active.child.lock().await.take() {
            let _ = child.start_kill();
            tokio::time::timeout(std::time::Duration::from_secs(5), child.wait())
                .await
                .map_err(|_| rt_error(ErrorCode::RuntimeUnavailable, "launcher_reap_timeout"))?
                .map_err(|_| rt_error(ErrorCode::RuntimeUnavailable, "launcher_reap"))?;
        }
        if let Some(task) = active.stderr.lock().await.take() {
            task.abort();
            let _ = task.await;
        }
        if !active.launched.load(std::sync::atomic::Ordering::Acquire) {
            let proof = CleanupProof {
                process: ProcessTreeProof {
                    instance_id: format!("not-spawned-{incarnation}"),
                    incarnation,
                    process_tree_empty: true,
                },
                mailbox_empty: true,
                tools_drained: active.authority.pending_handlers() == 0,
                ingress_drained: true,
            };
            if !proof.tools_drained {
                return Err(rt_error(ErrorCode::RuntimeUnavailable, "cleanup_unproven"));
            }
            *cached = Some(proof.clone());
            return self.persist_reaped(&active, &proof).await;
        }
        let isolator = self.isolator_for(&active.agent)?;
        let instance = active
            .instance
            .lock()
            .expect("runtime instance")
            .clone()
            .or_else(|| {
                isolator
                    .recorded_instance(&self.db_identity, incarnation)
                    .ok()?
            })
            .ok_or_else(|| rt_error(ErrorCode::RuntimeUnavailable, "cleanup_identity_unknown"))?;
        let process = isolator.reap(&instance).await?;
        let proof = CleanupProof {
            process,
            mailbox_empty: true,
            tools_drained: active.authority.pending_handlers() == 0,
            ingress_drained: true,
        };
        if !proof.process.process_tree_empty || !proof.tools_drained {
            return Err(rt_error(ErrorCode::RuntimeUnavailable, "cleanup_unproven"));
        }
        // Retain the proof independently of the now-reaped child. A failed
        // write keeps this record and its ownership available for retry.
        *cached = Some(proof.clone());
        self.persist_reaped(&active, &proof).await
    }

    async fn persist_reaped(
        &self,
        active: &Arc<Active>,
        proof: &CleanupProof,
    ) -> RtResult<CleanupProof> {
        let incarnation = proof.process.incarnation;
        self.retire_incarnation(incarnation)?;
        // Mandatory cleanup facts commit atomically. Optional diagnostics may
        // neither skip these writes nor turn their success into a failed run.
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            let txn = active.store.write_transaction().await?;
            exec(&txn, "UPDATE rt_launch_intents SET reaped=1 WHERE incarnation=?", vec![text(&incarnation.to_string())]).await?;
            exec(&txn,"UPDATE rt_attempts SET cleanup_state='confirmed',residual_remote_work=? WHERE room_id=? AND attempt_id=?",vec![num(if active.gateway.remote_work_uncertain(){1}else{0}),text(&active.room.to_string()),text(&active.attempt.to_string())]).await?;
            txn.commit().await.map_err(super::store::storage_err)?;
            Ok::<(), roundtable_protocol::RtError>(())
        }).await.map_err(|_| rt_error(ErrorCode::StorageUnavailable, "cleanup_persistence_pending"))??;
        if !matches!(
            tokio::time::timeout(
                std::time::Duration::from_millis(100),
                persist_diagnostic(active)
            )
            .await,
            Ok(Ok(()))
        ) {
            tracing::warn!(%incarnation, "roundtable optional diagnostic persistence failed");
        }
        // A control may cancel the driver after cleanup but before its final
        // candidate read returns to LeasedExecutor. Keep this boot's committed
        // proof so that cancellation cannot lose cleanup ownership in that gap.
        let mut completed = self.completed_cleanup.lock().expect("completed cleanup");
        if completed.len() >= 1024 {
            if let Some(expired) = completed.keys().next().copied() {
                completed.remove(&expired);
            }
        }
        completed.insert(incarnation, proof.clone());
        self.active
            .lock()
            .expect("active runtimes")
            .remove(&incarnation);
        self.launches
            .lock()
            .expect("launch lifecycle")
            .remove(&incarnation);
        Ok(proof.clone())
    }
}

impl TokenBound for LiveParticipantExecutor {
    fn upper_bound_for_length(&self, len: u64) -> RtResult<u64> {
        Ok(len)
    }
    fn upper_bound(&self, bytes: &[u8]) -> RtResult<u64> {
        Ok(bytes.len() as u64)
    }
    fn capacity_tokens(&self) -> Option<u64> {
        self.adapters
            .values()
            .map(|installed| installed.context_profile.model_capacity_tokens)
            .min()
    }
}
#[async_trait]
impl RoundtableTurnExecutor for LiveParticipantExecutor {
    async fn capability(&self, config: &RoundtableConfigV1) -> RtResult<RuntimeCapability> {
        let mut recipients = Vec::new();
        let mut keys = Vec::new();
        let mut policy_hash = None;
        let mut profile = None;
        for participant in &config.participants {
            let named = |error| {
                super::agent_credentials::name_seat(
                    error,
                    participant.ordinal,
                    participant.agent.as_deref().unwrap_or("codex"),
                )
            };
            let agent = participant_agent(participant).map_err(named)?;
            let installed = self.verified(&agent).await.map_err(named)?;
            let settings_credential = self.settings_env(&agent).is_some();
            self.seat_profile(participant).map_err(named)?;
            let checked: RtResult<_> = (|| {
                let binding = installed
                    .providers
                    .iter()
                    .find(|binding| {
                        binding.provider_ref == participant.provider_ref
                            && participant
                                .model
                                .as_ref()
                                .is_none_or(|model| model == &binding.model)
                    })
                    .ok_or_else(|| {
                        rt_error(ErrorCode::CapabilityUnqualified, "provider_unqualified")
                    })?;
                if participant
                    .effort
                    .as_ref()
                    .is_some_and(|effort| !binding.supported_efforts.contains(effort))
                {
                    return Err(rt_error(
                        ErrorCode::CapabilityUnqualified,
                        "effort_unqualified",
                    ));
                }
                self.policy(&installed, &binding.model)?;
                super::ApprovedOrigin::parse(&binding.origin)?;
                require_provider_credential(&installed, binding, settings_credential)?;
                Ok(binding)
            })();
            let binding = checked.map_err(named)?;
            if policy_hash.is_some_and(|hash| hash != installed.qualification_key.policy_hash) {
                return Err(rt_error(
                    ErrorCode::CapabilityUnqualified,
                    "qualification_policy_changed",
                ));
            }
            policy_hash = Some(installed.qualification_key.policy_hash);
            profile = Some(installed.context_profile.clone());
            keys.push(json!(installed.qualification_key));
            recipients.push(json!({"provider_ref":binding.provider_ref,"model":binding.model,"origin":binding.origin,"agent":agent,"ordinal":participant.ordinal,"effort":participant.effort}));
        }
        Ok(RuntimeCapability {
            recipients: Value::Array(recipients),
            qualification_keys: Value::Array(keys),
            policy_hash: policy_hash
                .ok_or_else(|| rt_error(ErrorCode::InvalidArgument, "participants"))?,
            profile: profile.ok_or_else(|| rt_error(ErrorCode::InvalidArgument, "participants"))?,
        })
    }
    fn token_bound(&self) -> &(dyn TokenBound + Send + Sync) {
        self
    }
    fn context_profile(&self, participant: &ParticipantV1) -> Option<QualifiedContextProfile> {
        self.adapters
            .get(participant.agent.as_deref().unwrap_or("codex"))
            .map(|installed| installed.context_profile.clone())
    }
    fn request_envelope_bound_bytes(&self, participant: &ParticipantV1) -> RtResult<Option<u64>> {
        verified_request_envelope_bound(&self.adapter(&participant_agent(participant)?)?)
    }
    fn request_envelope_proof_hash(
        &self,
        participant: &ParticipantV1,
    ) -> RtResult<Option<Hash256>> {
        let installed = self.adapter(&participant_agent(participant)?)?;
        Ok(verified_request_envelope_bound(&installed)?.map(|_| installed.report_sha256))
    }
    async fn cancel_and_reap(&self, identity: super::RuntimeIdentity) -> RtResult<CleanupProof> {
        self.reap_active(identity.incarnation).await
    }

    async fn execute_turn(
        &self,
        request: RoundtableTurnRequest,
    ) -> RtResult<RoundtableTurnOutcome> {
        self.launches
            .lock()
            .expect("launch lifecycle")
            .insert(request.fence.incarnation, false);
        let agent = participant_agent(&request.participant)?;
        let installed = self.verified(&agent).await?;
        let provider = installed
            .providers
            .iter()
            .find(|provider| {
                provider.provider_ref == request.participant.provider_ref
                    && request
                        .participant
                        .model
                        .as_ref()
                        .is_none_or(|model| model == &provider.model)
            })
            .cloned()
            .ok_or_else(|| rt_error(ErrorCode::CapabilityUnqualified, "provider_unqualified"))?;
        let settings_env = self.settings_env(&agent);
        require_provider_credential(&installed, &provider, settings_env.is_some())?;
        let seat_profile = self.seat_profile(&request.participant)?;
        let mut seeds = vec![std::env::var(&provider.credential_env).unwrap_or_default()];
        seeds.extend(
            settings_env
                .iter()
                .flatten()
                .map(|(_, value)| value.clone()),
        );
        let mut diagnostic_secrets = auth_denylist(&installed, &seeds)?;
        let (execution, facts) = self.policy(&installed, &provider.model)?;
        let directory = installed
            .oci
            .runtime_root
            .join("runs")
            .join(request.fence.incarnation.to_string());
        std::fs::create_dir_all(&directory)
            .map_err(|_| rt_error(ErrorCode::StorageUnavailable, "runtime_directory"))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700))
                .map_err(|_| rt_error(ErrorCode::StorageUnavailable, "runtime_directory"))?;
        }
        let service_path = super::companion::transport::short_socket_path("s");
        let gateway_path = super::companion::transport::short_socket_path("w");
        super::companion::transport::ensure_unix_socket_path(&service_path.to_string_lossy())?;
        super::companion::transport::ensure_unix_socket_path(&gateway_path.to_string_lossy())?;
        let clock = Arc::new(StoreClock(request.store.clone()));
        let binding = TokenBinding {
            attempt_id: request.fence.attempt_id,
            room_id: request.room_id,
            fence: request.fence.clone(),
            speaker_id: request.speaker_id,
            tool_version: super::SERVICE_TOOL_VERSION.into(),
            aliases: request.scope.aliases.clone(),
            result_scope: request.scope.clone(),
            evidence: evidence_objects(&request.prompt)?,
            profile: installed.context_profile.clone(),
        };
        let authority = Arc::new(
            GateToolAuthority::open(
                &self.data_dir,
                execution.clone(),
                facts.clone(),
                MonoMs(request.store.clock_sample().0),
                MonoMs(request.deadline_mono),
                binding,
            )
            .with_clock(clock)
            .with_execution_lease(request.execution_lease.clone()),
        );
        let token = Arc::new(authority.issue());
        let provider_token = uuid::Uuid::new_v4().to_string();
        diagnostic_secrets.extend([
            provider_token.clone(),
            token.reveal_for_same_sandbox().to_owned(),
        ]);
        let mut env_allowlist: BTreeMap<String, String> =
            super::qualification_profiles::live_model_env(agent.as_str(), &provider_token)
                .into_iter()
                .collect();
        // Settings credential (Cursor custom mode, CodeBuddy): into the
        // container env only. Values are already on the redaction list.
        for (key, value) in settings_env.iter().flatten() {
            env_allowlist.insert(key.clone(), value.clone());
        }
        let scrub_keys: Vec<String> = settings_env
            .iter()
            .flatten()
            .map(|(key, _)| key.clone())
            .collect();
        let store_clock = request.store.clone();
        let mut gateway = LiveModelGateway::new(
            self.data_dir.clone(),
            (
                super::ApprovedOrigin::parse(&provider.origin)?,
                super::HostCredential::injected(host_gateway_secret(
                    &installed,
                    &provider,
                    settings_env.as_deref(),
                )?),
                provider.model.clone(),
                request.participant.effort.clone(),
            ),
            provider_token.clone(),
            (execution, facts),
            MonoMs(request.deadline_mono),
            Arc::new(move || MonoMs(store_clock.clock_sample().0)),
            installed.context_profile.clone(),
        )?;
        if matches!(agent.as_str(), "grok" | "antigravity") {
            let files: Vec<PathBuf> = installed
                .oci
                .host_held_credentials
                .iter()
                .map(|mount| PathBuf::from(&mount.source))
                .collect();
            let version = super::qualification_profiles::profile_for_agent(agent.as_str())
                .map(|profile| profile.version_needle)
                .unwrap_or("");
            let upstream = super::host_model_auth::resolve_model_upstream(
                super::host_model_auth::ResolveInput {
                    agent: agent.as_str(),
                    binding_origin: &provider.origin,
                    env_secret: std::env::var(&provider.credential_env).ok(),
                    auth_files: &files,
                    adapter_version: version,
                    now_unix: super::host_model_auth::unix_now(),
                },
            )
            .await?;
            diagnostic_secrets.push(upstream.bearer.clone());
            gateway = gateway.with_native_upstream(upstream)?;
        }
        let gateway = Arc::new(gateway.with_execution_lease(request.execution_lease.clone()));
        let trace = AttemptTrace::open(
            &self.data_dir,
            &request.room_id.to_string(),
            &request.fence.attempt_id.to_string(),
            diagnostic_secrets.clone(),
        )
        .map(Arc::new);
        if let Some(trace) = &trace {
            gateway.set_trace(trace.clone());
            authority.set_trace(trace.clone());
            trace.record(
                "acp",
                json!({
                    "event": "attempt_start",
                    "agent": agent,
                    "phase_kind": format!("{:?}", authority.pinned_phase_kind()),
                    "model": provider.model,
                    "native_relay": matches!(agent.as_str(), "grok" | "antigravity"),
                    "phase_id": request.phase.phase_id.to_string(),
                    "speaker_id": request.speaker_id.to_string(),
                    "incarnation": request.fence.incarnation.to_string(),
                    "binding_id": request.fence.binding_id.to_string(),
                    "prompt_bytes": request.prompt.len(),
                    "deadline_in_ms": request.deadline_mono.saturating_sub(request.store.clock_sample().0),
                }),
            );
        }
        let live = super::live_stream::LiveSink::begin(
            &request.room_id.to_string(),
            &request.fence.attempt_id.to_string(),
            &request.speaker_id.to_string(),
            &request.phase.phase_id.to_string(),
            &format!("{:?}", authority.pinned_phase_kind()).to_ascii_lowercase(),
            &request.fence.incarnation.to_string(),
            diagnostic_secrets.clone(),
        );
        let principal_row = one_row(
            request.store.connection(),
            "SELECT principal_id FROM rt_rooms WHERE room_id=?",
            vec![text(&request.room_id.to_string())],
        )
        .await?;
        let principal: PrincipalId = column::<String>(&principal_row, 0)?
            .parse()
            .map_err(|_| rt_error(ErrorCode::StorageUnavailable, "principal"))?;
        let objects = ObjectStore::open(
            self.data_dir.join("roundtable/objects"),
            Arc::new(ReservationLedger::new(2_147_483_648)),
            principal,
        )?;
        let tools = Arc::new(DurableToolStore::open(
            request.store.clone(),
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
            request.scope.clone(),
        ));
        let broker = ServiceBroker::bind(
            &service_path.to_string_lossy(),
            &request.fence.incarnation.to_string(),
            token.clone(),
            authority.clone(),
            tools,
        )
        .await?;
        let gateway_server = LiveGatewayServer::bind(&gateway_path, gateway.clone()).await?;
        let scratch = directory.join("scratch");
        std::fs::create_dir_all(&scratch)
            .map_err(|_| rt_error(ErrorCode::StorageUnavailable, "runtime_directory"))?;
        let active = Arc::new(Active {
            store: request.store.clone(),
            room: request.room_id,
            attempt: request.fence.attempt_id,
            scratch: scratch.clone(),
            launched: std::sync::atomic::AtomicBool::new(false),
            instance: Mutex::new(None),
            child: tokio::sync::Mutex::new(None),
            broker: tokio::sync::Mutex::new(Some(broker)),
            gateway_server: tokio::sync::Mutex::new(Some(gateway_server)),
            gateway,
            authority,
            cleanup: tokio::sync::Mutex::new(None),
            stderr: tokio::sync::Mutex::new(None),
            assistant: Mutex::new(Some(super::DiagnosticCapture::new(diagnostic_secrets))),
            finish_reason: Mutex::new(None),
            agent: agent.clone(),
            trace: trace.clone(),
            live,
            workspace_root: std::sync::OnceLock::new(),
        });
        self.active
            .lock()
            .expect("active runtimes")
            .insert(request.fence.incarnation, active.clone());
        let attempt_isolator = self
            .isolator_for(&agent)?
            .with_attempt_sockets(service_path.clone(), gateway_path.clone())?;
        let mut oci = installed.oci.clone();
        oci.service_socket = Some(service_path);
        oci.gateway_socket = Some(gateway_path);
        let config = one_row(
            request.store.connection(),
            "SELECT config_ref FROM rt_rooms WHERE room_id=?",
            vec![text(&request.room_id.to_string())],
        )
        .await?;
        let config: RoundtableConfigV1 = serde_json::from_str(&column::<String>(&config, 0)?)
            .map_err(|_| rt_error(ErrorCode::StorageUnavailable, "room_config"))?;
        let folder_id = config
            .workspace_id
            .parse::<i32>()
            .map_err(|_| rt_error(ErrorCode::InvalidArgument, "workspace_id"))?;
        let folder = one_row(
            request.store.connection(),
            "SELECT path FROM folder WHERE id=? AND deleted_at IS NULL",
            vec![num(i64::from(folder_id))],
        )
        .await?;
        let project = PathBuf::from(column::<String>(&folder, 0)?)
            .canonicalize()
            .map_err(|_| rt_error(ErrorCode::InvalidArgument, "workspace_path"))?;
        let _ = active.workspace_root.set(project.clone());
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or_else(|| rt_error(ErrorCode::PolicyUnenforceable, "host_home"))?
            .canonicalize()
            .map_err(|_| rt_error(ErrorCode::PolicyUnenforceable, "host_home"))?;
        if agent == "codex" {
            // Fresh routing copy each turn, so a provider the user just
            // changed in Codeg's Codex settings applies; the mount source
            // named on the certificate must exist.
            super::agent_credentials::refresh_codex_routing(&home)
                .map_err(|_| rt_error(ErrorCode::StorageUnavailable, "codex_routing"))?;
        }
        let mut decoy_paths = Vec::new();
        for row in super::store::rows(
            request.store.connection(),
            "SELECT target_path FROM folder_link WHERE folder_id=?",
            vec![num(i64::from(folder_id))],
        )
        .await?
        {
            decoy_paths.push(
                PathBuf::from(column::<String>(&row, 0)?)
                    .canonicalize()
                    .map_err(|_| rt_error(ErrorCode::InvalidArgument, "linked_workspace_path"))?,
            );
        }
        let input = SandboxInput {
            certificate: installed.qualification_key.clone(),
            db: self.db_identity.clone(),
            boot_epoch: request.fence.boot_epoch,
            incarnation: request.fence.incarnation,
            scratch: scratch.clone(),
            project,
            home,
            other_scratches: Vec::new(),
            decoy_paths,
            inherited_env: BTreeMap::new(),
            env_allowlist,
            global_mcp: false,
        };
        // Plan verification hashes crun, the rootfs and the container
        // binaries. That is seconds of synchronous I/O and must not run on the
        // room's async task.
        let plan = tokio::task::spawn_blocking(move || build_qualified_sandbox_plan(&input, &oci))
            .await
            .map_err(|_| rt_error(ErrorCode::RuntimeUnavailable, "sandbox_plan_worker"))??;
        if let Some(trace) = &trace {
            trace.record("acp", json!({"event":"sandbox_plan_built"}));
        }
        let prepared = attempt_isolator.prepare(&plan).await?;
        let intent = LaunchIntent::from_plan(&plan);
        request.store.record_launch(intent.clone()).await?;
        active
            .launched
            .store(true, std::sync::atomic::Ordering::Release);
        self.launches
            .lock()
            .expect("launch lifecycle")
            .insert(request.fence.incarnation, true);
        let (instance, mut child) = attempt_isolator.spawn_attached(&prepared, &intent).await?;
        // crun has read the bundle; the copied credential values must not
        // outlive the attempt in config.json.
        let _bundle_scrub = super::agent_credentials::BundleScrub {
            config: installed
                .oci
                .runtime_root
                .join("bundles")
                .join(&instance.runtime_id)
                .join("config.json"),
            keys: scrub_keys,
        };
        *active.instance.lock().expect("runtime instance") = Some(instance.clone());
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| rt_error(ErrorCode::RuntimeUnavailable, "acp_stdin"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| rt_error(ErrorCode::RuntimeUnavailable, "acp_stdout"))?;
        if let Some(trace) = &trace {
            trace.record("acp", json!({"event":"spawned","child_pid":child.id()}));
        }
        if let Some(mut stderr) = child.stderr.take() {
            let stderr_trace = trace.clone();
            *active.stderr.lock().await = Some(tokio::spawn(async move {
                let mut buf = [0u8; 4096];
                loop {
                    match stderr.read(&mut buf).await {
                        Ok(count) if count > 0 => {
                            if let Some(trace) = &stderr_trace {
                                trace.stderr_chunk(&buf[..count]);
                            }
                        }
                        _ => break,
                    }
                }
                if let Some(trace) = &stderr_trace {
                    trace.stderr_finish();
                }
            }));
        }
        *active.child.lock().await = Some(child);
        request
            .store
            .mark_launch_spawned(request.fence.incarnation, instance)
            .await?;
        let mcp = installed
            .qualification_key
            .binaries
            .iter()
            .find(|binary| binary.role == "mcp")
            .ok_or_else(|| {
                rt_error(ErrorCode::CapabilityUnqualified, "companion_binary_missing")
            })?;
        let result = drive_acp(
            (stdin, stdout),
            &request,
            &mcp.absolute_path,
            token.reveal_for_same_sandbox(),
            &active,
            &self.registry,
            agent_type_for(&agent)?,
            seat_profile.as_ref(),
        )
        .await;
        *active.finish_reason.lock().expect("finish reason") = Some(
            result
                .as_ref()
                .err()
                .and_then(|error| error.details.reason.as_deref())
                .unwrap_or("completed")
                .to_owned(),
        );
        if let Some(trace) = &trace {
            trace.record(
                "acp",
                json!({
                    "event": "drive_acp_returned",
                    "ok": result.is_ok(),
                    "reason": result.as_ref().err().and_then(|error| error.details.reason.clone()),
                    "message": result.as_ref().err().map(|error| error.message.clone()),
                    "inflight_gateway": trace.inflight_snapshot(),
                }),
            );
        }
        let cleanup = self.reap_active(request.fence.incarnation).await?;
        let watermark = complete_after_gateway_drain(result, &active.gateway)?;
        let row = optional_row(
            request.store.connection(),
            "SELECT payload_hash FROM rt_submissions WHERE room_id=? AND attempt_id=? AND sealed=1",
            vec![
                text(&request.room_id.to_string()),
                text(&request.fence.attempt_id.to_string()),
            ],
        )
        .await?
        .ok_or_else(|| rt_error(ErrorCode::InvalidState, "candidate_missing"))?;
        Ok(RoundtableTurnOutcome {
            completion: RuntimeTurnCompleted {
                fence: request.fence,
                finish_reason: "completed".into(),
                ingress_watermark: Seq(watermark),
                tool_barrier: ToolBarrierV1 {
                    drained: cleanup.tools_drained,
                },
                candidate_id: Some(column(&row, 0)?),
            },
            cleanup,
        })
    }
}

/// Diagnostics only. When an attempt is reaped before `drive_acp` returned
/// (attempt_timeout, room stop), record the agent process tree, in-flight
/// gateway requests and per-stream idle times before anything is torn down.
async fn trace_reap_snapshot(active: &Active) {
    let Some(trace) = &active.trace else {
        return;
    };
    let finished = active
        .finish_reason
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .clone();
    let db_reason = match optional_row(
        active.store.connection(),
        "SELECT finish_reason FROM rt_attempts WHERE room_id=? AND attempt_id=?",
        vec![
            text(&active.room.to_string()),
            text(&active.attempt.to_string()),
        ],
    )
    .await
    {
        Ok(Some(row)) => column::<Option<String>>(&row, 0).ok().flatten(),
        _ => None,
    };
    let pid = active
        .child
        .lock()
        .await
        .as_ref()
        .and_then(tokio::process::Child::id);
    let reason = if finished.is_some() {
        "reap_after_finish"
    } else {
        db_reason.as_deref().unwrap_or("reaped_without_finish")
    };
    trace.stderr_flush_partial();
    trace.snapshot(
        reason,
        pid,
        json!({
            "drive_acp_finish_reason": finished,
            "db_finish_reason": db_reason,
            "gateway_exchange_log": active.gateway.exchange_log(),
            "gateway_completion_error": active.gateway.completion_error().and_then(|error| error.details.reason),
            "pending_tool_handlers": active.authority.pending_handlers(),
        }),
    );
}

struct StoreClock(RoundtableStore);
impl super::clock::MonoClock for StoreClock {
    fn now_ms(&self) -> u64 {
        self.0.clock_sample().0
    }
    fn utc(&self) -> String {
        self.0.clock_sample().1
    }
}

fn evidence_objects(prompt: &[u8]) -> RtResult<BTreeMap<String, roundtable_protocol::ObjectRefV1>> {
    let prompt: Value = serde_json::from_slice(prompt)
        .map_err(|_| rt_error(ErrorCode::InvalidArgument, "prompt_encoding"))?;
    let mut objects = BTreeMap::new();
    for evidence in prompt["context"]["evidence"]
        .as_array()
        .into_iter()
        .flatten()
    {
        let alias = evidence["alias"]
            .as_str()
            .ok_or_else(|| rt_error(ErrorCode::InvalidArgument, "evidence_alias"))?;
        let object = serde_json::from_value(evidence["object"].clone())
            .map_err(|_| rt_error(ErrorCode::InvalidArgument, "evidence_object"))?;
        objects.insert(alias.to_owned(), object);
    }
    Ok(objects)
}

async fn drive_acp(
    (mut stdin, stdout): (tokio::process::ChildStdin, tokio::process::ChildStdout),
    request: &RoundtableTurnRequest,
    mcp: &str,
    token: &str,
    active: &Active,
    registry: &tokio::sync::Mutex<Option<super::RoundtableSessionRegistry>>,
    agent: crate::models::AgentType,
    profile: Option<&super::agent_credentials::CodeBuddyProfile>,
) -> RtResult<u64> {
    let mut stdout = BufReader::new(stdout);
    let mut seq = 0;
    let initialize = rpc(
        &mut stdin,
        &mut stdout,
        1,
        "initialize",
        roundtable_initialize_params("codeg-roundtable"),
        &mut seq,
        active,
    )
    .await?;
    if initialize["protocolVersion"] != 1 {
        return Err(rt_error(ErrorCode::CapabilityUnqualified, "acp_version"));
    }
    let (registry_copy, _root, discovery) = {
        let mut registry = registry.lock().await;
        if registry.is_none() {
            *registry =
                Some(super::RoundtableSessionRegistry::open_live(request.store.clone()).await?);
        }
        let registry = registry.as_ref().expect("registry initialized").clone();
        let root = registry.reserve_root(active.scratch.clone());
        let discovery = registry.begin_discovery(agent).await;
        (registry, root, discovery)
    };
    let session = rpc(
        &mut stdin,
        &mut stdout,
        2,
        "session/new",
        roundtable_session_params_for(
            agent,
            &json!([{
                "name": "roundtable",
                "command": mcp,
                "args": [
                    "--service-roundtable",
                    "--socket-path",
                    "/run/codeg/roundtable.sock",
                    "--incarnation",
                    request.fence.incarnation.to_string()
                ],
                "env": [
                    {"name": super::ATTEMPT_TOKEN_ENV, "value": token},
                    {"name": "CODEG_RT_MODEL_SOCKET", "value": "/run/codeg/gateway.sock"}
                ]
            }]),
        ),
        &mut seq,
        active,
    )
    .await?;
    let session_id = session["sessionId"]
        .as_str()
        .ok_or_else(|| rt_error(ErrorCode::RuntimeUnavailable, "acp_session"))?;
    {
        let registry = registry_copy;
        let record = super::InternalBindingRecord {
            room_id: request.room_id,
            binding_id: request.fence.binding_id,
            incarnation: request.fence.incarnation,
            agent,
            external_id: session_id.to_owned().into(),
            reserved_root: active.scratch.clone(),
        };
        tokio::task::spawn_blocking(move || registry.register(record))
            .await
            .map_err(|_| rt_error(ErrorCode::RuntimeUnavailable, "registry_worker"))??;
    }
    drop(discovery);
    exec(
        request.store.connection(),
        "UPDATE rt_bindings SET external_session_id=? WHERE room_id=? AND binding_id=?",
        vec![
            text(session_id),
            text(&request.room_id.to_string()),
            text(&request.fence.binding_id.to_string()),
        ],
    )
    .await?;
    let requested_model = request
        .participant
        .model
        .as_deref()
        .ok_or_else(|| rt_error(ErrorCode::CapabilityUnqualified, "model_not_resolved"))?;
    let selected = rpc(
        &mut stdin,
        &mut stdout,
        3,
        "session/set_config_option",
        json!({"sessionId":session_id,"configId":"model","value":requested_model}),
        &mut seq,
        active,
    )
    .await?;
    verify_confirmed_option(&selected, "model", requested_model)?;
    if let Some(effort) = request.participant.effort.as_ref() {
        let selected = rpc(
            &mut stdin,
            &mut stdout,
            4,
            "session/set_config_option",
            json!({"sessionId":session_id,"configId":"reasoning_effort","value":effort}),
            &mut seq,
            active,
        )
        .await?;
        verify_confirmed_option(&selected, "model", requested_model)?;
        verify_confirmed_option(&selected, "reasoning_effort", effort)?;
    }
    if let Some(profile) = profile {
        apply_seat_profile(
            &mut stdin,
            &mut stdout,
            session_id,
            profile,
            &mut seq,
            active,
        )
        .await?;
    }
    let prompt = std::str::from_utf8(&request.prompt)
        .map_err(|_| rt_error(ErrorCode::InvalidArgument, "prompt_encoding"))?;
    finish_seat_prompt(
        &mut stdin,
        &mut stdout,
        session_id,
        prompt,
        &mut seq,
        SeatPromptControl {
            assistant: Some(&active.assistant),
            deadline: None,
            gateway: Some(&active.gateway),
            trace: active.trace.as_deref(),
            live: Some(&active.live),
            workspace_root: active.workspace_root.get().map(PathBuf::as_path),
        },
    )
    .await?;
    Ok(seq)
}
/// ACP client parameters shared by live seats and the qualification probe.
/// Terminal and filesystem edit/read capabilities stay off. `session/new`
/// has no capabilities field in ACP, so that request omits them.
pub(crate) fn roundtable_initialize_params(client_name: &str) -> Value {
    let mut capabilities = super::service_client_capabilities_value();
    // Keep explicit denials for adapters that distinguish absent from false.
    capabilities["fs"] = json!({"readTextFile":false,"writeTextFile":false});
    capabilities["terminal"] = json!(false);
    json!({
        "protocolVersion": 1,
        "clientCapabilities": capabilities,
        "clientInfo": {"name": client_name, "version": "1"}
    })
}

pub(crate) fn roundtable_session_params(mcp_servers: &Value) -> Value {
    json!({
        "cwd": "/scratch",
        "mcpServers": mcp_servers
    })
}

/// Grok `agent stdio` has no ACP flag that drops native tools. An empty
/// `tools` allowlist inherits every tool. `_meta.agentProfile.disallowedTools`
/// on `session/new` is the denylist that applies to this session. `use_tool`
/// and `search_tool` stay available because Grok calls MCP through them.
/// `maxTurns` and `permissionMode` stay unset.
pub(crate) fn roundtable_session_params_for(
    agent: crate::models::AgentType,
    mcp_servers: &Value,
) -> Value {
    let mut params = roundtable_session_params(mcp_servers);
    if agent == crate::models::AgentType::Grok {
        params["_meta"] = grok_roundtable_session_meta();
    }
    if agent == crate::models::AgentType::Antigravity {
        // Antigravity 1.3.0 hard-scopes its own file tools (view_file, ...) to
        // `cwd` + its GEMINI_HOME + skills dirs, by realpath, and asks no
        // permission for them. It accepts `additionalDirectories` but ignores
        // it. The session cwd is therefore the read-only bind; the process cwd
        // stays /scratch. Writes there fail on the mount, terminals and edits
        // still need a permission that is rejected, and realpath keeps every
        // symlink out of `/workspace-ro` outside its scope.
        params["cwd"] = json!(super::sandbox::WORKSPACE_MOUNT_DESTINATION);
    }
    params
}

/// Native Grok tools. Copied from the hidden-generation catalog minus the MCP
/// meta-tools `search_tool` and `use_tool`, and minus the read-only file tools
/// `read_file`, `grep` and `list_dir`: members read the workspace themselves
/// through the read-only `/workspace-ro` bind. Writes, terminals, network and
/// subagents stay denied; the mount is read-only regardless. CLI `--disallowed-tools` is
/// headless-only and does not apply to `grok agent stdio`.
const GROK_ROUNDTABLE_DISALLOWED_TOOLS: &[&str] = &[
    "run_terminal_cmd",
    "run_terminal_command",
    "search_replace",
    "write",
    "web_search",
    "x_search",
    "web_fetch",
    "image_gen",
    "image_edit",
    "image_to_video",
    "reference_to_video",
    "todo_write",
    "task",
    "get_task_output",
    "kill_task",
    "wait_tasks",
    "monitor",
    "update_goal",
    "enter_plan_mode",
    "exit_plan_mode",
    "ask_user_question",
    "Agent",
    "spawn_subagent",
    "send_subagent_message",
    "get_command_or_subagent_output",
    "kill_command_or_subagent",
    "workflow",
    "memory_search",
    "memory_get",
    "scheduler_create",
    "scheduler_list",
    "scheduler_delete",
    "lsp",
    "get_terminal_command_output",
    "kill_terminal_command",
];

fn grok_roundtable_session_meta() -> Value {
    let disallowed: Vec<Value> = GROK_ROUNDTABLE_DISALLOWED_TOOLS
        .iter()
        .map(|name| Value::String((*name).to_string()))
        .collect();
    json!({
        "agentProfile": {
            "name": "codeg-roundtable",
            "description": "Roundtable seat. Call only the roundtable MCP tools.",
            "disallowedTools": disallowed,
            "agentsMd": false,
            "discoverSkills": false
        }
    })
}

/// Follow-up prompts after Grok cancels a turn on a rejected native tool.
const MAX_PERMISSION_CANCEL_REPAIRS: u8 = 2;
const PERMISSION_REPAIR_PROMPT: &str = "Only use roundtable__read_evidence, roundtable__search_evidence, and roundtable__submit_result. Call submit_result now.";

#[derive(Clone, Copy)]
struct SeatPromptControl<'a> {
    assistant: Option<&'a Mutex<Option<super::DiagnosticCapture>>>,
    deadline: Option<Duration>,
    gateway: Option<&'a LiveModelGateway>,
    trace: Option<&'a AttemptTrace>,
    live: Option<&'a super::live_stream::LiveSink>,
    workspace_root: Option<&'a Path>,
}
impl SeatPromptControl<'_> {
    fn check_completion(&self) -> RtResult<()> {
        if let Some(error) = self.gateway.and_then(LiveModelGateway::completion_error) {
            return Err(error);
        }
        Ok(())
    }
}

async fn finish_seat_prompt<W, R>(
    stdin: &mut W,
    stdout: &mut BufReader<R>,
    session_id: &str,
    prompt: &str,
    seq: &mut u64,
    control: SeatPromptControl<'_>,
) -> RtResult<Value>
where
    W: AsyncWrite + Unpin,
    R: AsyncRead + Unpin,
{
    let rejected = AtomicBool::new(false);
    let mut prompt_id = 5u64;
    let mut text = prompt.to_owned();
    let mut repairs = 0u8;
    loop {
        control.check_completion()?;
        rejected.store(false, Ordering::Relaxed);
        let result = acp_exchange(
            stdin,
            stdout,
            &mut AcpExchange {
                id: prompt_id,
                method: "session/prompt",
                params: json!({
                    "sessionId": session_id,
                    "prompt": [{"type": "text", "text": text}]
                }),
                seq,
                assistant: control.assistant,
                deadline: control.deadline,
                rejected_permission: &rejected,
                private_log: None,
                trace: control.trace,
                live: control.live,
                workspace_root: control.workspace_root,
            },
        )
        .await?;
        control.check_completion()?;
        if result["stopReason"] == "end_turn" {
            return Ok(result);
        }
        if result["stopReason"] == "cancelled"
            && rejected.load(Ordering::Relaxed)
            && repairs < MAX_PERMISSION_CANCEL_REPAIRS
        {
            repairs = repairs.saturating_add(1);
            prompt_id = prompt_id.saturating_add(1);
            text = PERMISSION_REPAIR_PROMPT.to_owned();
            continue;
        }
        return Err(rt_error(
            ErrorCode::RuntimeUnavailable,
            "acp_abnormal_finish",
        ));
    }
}

pub(crate) struct AcpExchange<'a> {
    pub(crate) id: u64,
    pub(crate) method: &'a str,
    pub(crate) params: Value,
    pub(crate) seq: &'a mut u64,
    pub(crate) assistant: Option<&'a Mutex<Option<super::DiagnosticCapture>>>,
    /// Overall budget and per-read budget. Live seats pass `None`.
    pub(crate) deadline: Option<Duration>,
    /// Set when this exchange rejects a tool. A later `cancelled` stop can
    /// be retried by [`finish_seat_prompt`].
    pub(crate) rejected_permission: &'a AtomicBool,
    /// Qualification probe log of private ACP frames. Live seats pass `None`.
    pub(crate) private_log: Option<&'a std::sync::Mutex<Vec<String>>>,
    /// Diagnostics-only attempt trace. Live seats pass the attempt trace.
    pub(crate) trace: Option<&'a AttemptTrace>,
    /// Display-only live output sink. Live seats pass theirs; probes `None`.
    pub(crate) live: Option<&'a super::live_stream::LiveSink>,
    /// Host path behind `/workspace-ro` for live seats; `None` in probes.
    pub(crate) workspace_root: Option<&'a Path>,
}

async fn rpc(
    stdin: &mut tokio::process::ChildStdin,
    stdout: &mut BufReader<tokio::process::ChildStdout>,
    id: u64,
    method: &str,
    params: Value,
    seq: &mut u64,
    active: &Active,
) -> RtResult<Value> {
    let rejected = AtomicBool::new(false);
    acp_exchange(
        stdin,
        stdout,
        &mut AcpExchange {
            id,
            method,
            params,
            seq,
            assistant: Some(&active.assistant),
            deadline: None,
            rejected_permission: &rejected,
            private_log: None,
            trace: active.trace.as_deref(),
            live: Some(&active.live),
            workspace_root: active.workspace_root.get().map(PathBuf::as_path),
        },
    )
    .await
}

/// JSON-RPC error text for a qualification trace. `code`, `message`, and
/// `data.details` stay on separate lines. Secret-like fields are removed so
/// the trace writer does not drop the whole diagnostic.
pub(crate) fn describe_acp_rejection(error: &Value) -> String {
    let code = match error.get("code") {
        Some(Value::Number(number)) => number.to_string(),
        Some(Value::String(text)) => text.clone(),
        _ => "absent".to_string(),
    };
    let message = error
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("")
        .replace(['\n', '\r'], " ");
    let details = match error.pointer("/data/details") {
        Some(value) => compact_acp_details(value),
        None => "absent".to_string(),
    };
    format!("code={code}\nmessage={message}\ndetails={details}")
        .lines()
        .map(scrub_trace_line)
        .collect::<Vec<_>>()
        .join("\n")
}

fn compact_acp_details(value: &Value) -> String {
    let mut copy = value.clone();
    redact_acp_value(&mut copy);
    let text = serde_json::to_string(&copy).unwrap_or_else(|_| "unprintable".to_string());
    if text.len() > 1024 {
        format!("{}…", &text[..1024])
    } else {
        text
    }
}

/// Redacts values under secret-bearing keys. Matching is on whole key
/// segments (`access_token`, `apiKey`, `client_secret`), so usage counters such
/// as `input_tokens` or `totalContextTokens` stay readable, and numbers or
/// booleans are never treated as secrets. Registered secret values are masked
/// again by `AttemptTrace::redact` on every record.
fn redact_acp_value(value: &mut Value) {
    match value {
        Value::Object(map) => {
            for (key, child) in map.iter_mut() {
                if super::attempt_trace::key_is_secret(key)
                    && !matches!(child, Value::Number(_) | Value::Bool(_) | Value::Null)
                {
                    *child = Value::String("[redacted]".to_string());
                } else {
                    redact_acp_value(child);
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                redact_acp_value(item);
            }
        }
        Value::String(text) if text.to_ascii_lowercase().contains("sk-") => {
            *text = "[redacted]".to_string();
        }
        _ => {}
    }
}

fn scrub_trace_line(line: &str) -> String {
    const TRIGGERS: &[&str] = &[
        "authorization",
        "bearer",
        "api_key",
        "token",
        "secret",
        "password",
        "sk-",
    ];
    let mut out = line.to_string();
    loop {
        let lower = out.to_ascii_lowercase();
        let Some(trigger) = TRIGGERS.iter().find(|trigger| lower.contains(*trigger)) else {
            break;
        };
        let Some(pos) = lower.find(trigger) else {
            break;
        };
        out.replace_range(pos..pos + trigger.len(), "[redacted]");
    }
    out
}

/// Live and probe ACP loop. Transport frames are lenient JSON. A rejected
/// tool stays inside this loop; only the prompt's `stopReason` ends the turn.
pub(crate) async fn acp_exchange<W, R>(
    stdin: &mut W,
    stdout: &mut BufReader<R>,
    exchange: &mut AcpExchange<'_>,
) -> RtResult<Value>
where
    W: AsyncWrite + Unpin,
    R: AsyncRead + Unpin,
{
    write_rpc(
        stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": exchange.id,
            "method": exchange.method,
            "params": exchange.params
        }),
    )
    .await?;
    if let Some(trace) = exchange.trace {
        trace.record(
            "acp",
            json!({
                "event": "send",
                "id": exchange.id,
                "method": exchange.method,
                "prompt_bytes": exchange.params.pointer("/prompt/0/text").and_then(Value::as_str).map(str::len),
            }),
        );
    }
    let mut chunks = ChunkAgg::new(exchange.trace);
    let started = tokio::time::Instant::now();
    loop {
        if let Some(limit) = exchange.deadline {
            if started.elapsed() > limit {
                return Err(rt_error(ErrorCode::RuntimeUnavailable, "acp_timeout"));
            }
        }
        let mut line = Vec::new();
        let count = if let Some(limit) = exchange.deadline {
            match tokio::time::timeout(
                limit,
                (&mut *stdout).take(1_048_577).read_until(b'\n', &mut line),
            )
            .await
            {
                Err(_) => return Err(rt_error(ErrorCode::RuntimeUnavailable, "acp_timeout")),
                Ok(Err(_)) => return Err(rt_error(ErrorCode::RuntimeUnavailable, "acp_read")),
                Ok(Ok(count)) => count,
            }
        } else {
            (&mut *stdout)
                .take(1_048_577)
                .read_until(b'\n', &mut line)
                .await
                .map_err(|_| rt_error(ErrorCode::RuntimeUnavailable, "acp_read"))?
        };
        if count == 0 {
            return Err(rt_error(ErrorCode::RuntimeUnavailable, "acp_closed"));
        }
        if line.len() > 1_048_576 {
            tracing::warn!(
                excerpt = %super::diagnostics::redact_untrusted_excerpt(&line),
                "rejected ACP frame"
            );
            return Err(rt_error(ErrorCode::RuntimeUnavailable, "acp_frame"));
        }
        let message = parse_acp_frame(&line)?;
        if exchange.trace.is_some() {
            trace_acp_frame(&mut chunks, &message, line.len(), exchange.id);
        }
        if let Some(live) = exchange.live {
            mirror_live_frame(live, &message);
        }
        *exchange.seq = exchange
            .seq
            .checked_add(1)
            .ok_or_else(|| rt_error(ErrorCode::InvalidState, "ingress_overflow"))?;
        // Ordered failure observations are consumed before any terminal
        // response can release a previously staged submission for acceptance.
        if message["method"] == "session/update" {
            note_private_frame(exchange, "session/update");
            check_failure_metadata(&message["params"]["update"], super::FailureSource::Update)?;
            check_failure_metadata(&message["params"], super::FailureSource::Update)?;
        }
        if message.get("method").is_some() {
            if let Some(request_id) = message.get("id").cloned() {
                let response = if message["method"] == "session/request_permission" {
                    if !tool_call_is_roundtable(&message["params"])
                        && !tool_call_is_workspace_read(&message["params"], exchange.workspace_root)
                    {
                        exchange.rejected_permission.store(true, Ordering::Relaxed);
                    }
                    let reply = permission_reply_in(
                        &message["params"],
                        &request_id,
                        exchange.workspace_root,
                    );
                    if let Some(trace) = exchange.trace {
                        trace.record(
                            "acp",
                            json!({
                                "event": "permission_reply",
                                "id": request_id,
                                "option_id": reply["result"]["outcome"]["optionId"],
                                "outcome": reply["result"]["outcome"]["outcome"],
                            }),
                        );
                    }
                    reply
                } else {
                    json!({
                        "jsonrpc": "2.0",
                        "id": request_id,
                        "error": {"code": -32601, "message": "Capability not available"}
                    })
                };
                write_rpc(stdin, &response).await?;
            } else if message["method"] == "session/update"
                && message["params"]["update"]["sessionUpdate"] == "agent_message_chunk"
            {
                if let Some(text) = message["params"]["update"]["content"]["text"].as_str() {
                    if let Some(assistant) = exchange.assistant {
                        if let Some(capture) =
                            assistant.lock().expect("assistant diagnostic").as_mut()
                        {
                            capture.push(text);
                        }
                    }
                }
            }
            continue;
        }
        if message["id"] == exchange.id {
            if let Some(rejected) = message.get("error") {
                let mut error = rt_error(ErrorCode::RuntimeUnavailable, "acp_rejected");
                error.message = describe_acp_rejection(rejected);
                return Err(error);
            }
            let result = message
                .get("result")
                .cloned()
                .ok_or_else(|| rt_error(ErrorCode::RuntimeUnavailable, "acp_response"))?;
            check_failure_metadata(&result, super::FailureSource::Response)?;
            check_failure_metadata(&message, super::FailureSource::Response)?;
            if exchange.method == "session/prompt"
                && result["stopReason"] != "end_turn"
                && !(result["stopReason"] == "cancelled"
                    && exchange.rejected_permission.load(Ordering::Relaxed))
            {
                return Err(rt_error(
                    ErrorCode::RuntimeUnavailable,
                    "acp_abnormal_finish",
                ));
            }
            note_private_frame(exchange, exchange.method);
            return Ok(result);
        }
    }
}

/// Coalesces consecutive message/thought chunks into one trace record
/// (count, bytes, first/last time, short text preview for message chunks).
///
/// Long same-type streams are not opaque: every record carries the largest
/// inter-chunk gap inside it, a gap of `STREAM_GAP_RECORD_MS` or more is
/// written as its own `stream_gap` record when the next chunk arrives, and a
/// stream is cut into segments of at most `STREAM_SEGMENT_MS` so the timeline
/// shows progress while a 3-minute thinking stream is still running.
struct ChunkAgg<'a> {
    trace: Option<&'a AttemptTrace>,
    kind: Option<String>,
    count: u64,
    bytes: u64,
    first_ms: u64,
    last_ms: u64,
    max_gap_ms: u64,
    max_gap_at_ms: u64,
    gaps_over_1s: u64,
    segment: u64,
    preview: String,
}

const STREAM_GAP_RECORD_MS: u64 = 5_000;
const STREAM_SEGMENT_MS: u64 = 30_000;

impl<'a> ChunkAgg<'a> {
    fn new(trace: Option<&'a AttemptTrace>) -> Self {
        Self {
            trace,
            kind: None,
            count: 0,
            bytes: 0,
            first_ms: 0,
            last_ms: 0,
            max_gap_ms: 0,
            max_gap_at_ms: 0,
            gaps_over_1s: 0,
            segment: 0,
            preview: String::new(),
        }
    }

    fn add(&mut self, kind: &str, text: &str) {
        let Some(trace) = self.trace else {
            return;
        };
        self.add_at(kind, text, trace.elapsed_ms());
    }

    fn add_at(&mut self, kind: &str, text: &str, now: u64) {
        let Some(trace) = self.trace else {
            return;
        };
        if self.kind.as_deref() != Some(kind) {
            self.flush();
            self.kind = Some(kind.to_owned());
            self.first_ms = now;
            self.last_ms = now;
            self.segment = 0;
        } else {
            let gap = now.saturating_sub(self.last_ms);
            if gap >= STREAM_GAP_RECORD_MS {
                trace.record(
                    "acp",
                    json!({
                        "event": "stream_gap",
                        "update": kind,
                        "gap_ms": gap,
                        "from_t_ms": self.last_ms,
                        "to_t_ms": now,
                        "chunks_before": self.count,
                    }),
                );
            }
            if gap >= 1_000 {
                self.gaps_over_1s += 1;
            }
            if gap > self.max_gap_ms {
                self.max_gap_ms = gap;
                self.max_gap_at_ms = now;
            }
            if now.saturating_sub(self.first_ms) >= STREAM_SEGMENT_MS {
                let segment = self.segment + 1;
                self.flush_segment(true);
                self.kind = Some(kind.to_owned());
                self.first_ms = self.last_ms;
                self.segment = segment;
            }
        }
        self.count += 1;
        self.bytes += text.len() as u64;
        self.last_ms = now;
        if kind == "agent_message_chunk" && self.preview.len() < 400 && !trace.metadata_only() {
            self.preview.push_str(text);
        }
    }

    fn flush(&mut self) {
        self.flush_segment(false);
    }

    fn flush_segment(&mut self, continues: bool) {
        let (Some(trace), Some(kind)) = (self.trace, self.kind.take()) else {
            return;
        };
        trace.record(
            "acp",
            json!({
                "event": "chunks",
                "update": kind,
                "count": self.count,
                "bytes": self.bytes,
                "first_t_ms": self.first_ms,
                "last_t_ms": self.last_ms,
                "max_gap_ms": self.max_gap_ms,
                "max_gap_at_t_ms": if self.max_gap_ms > 0 { Some(self.max_gap_at_ms) } else { None },
                "gaps_over_1s": self.gaps_over_1s,
                "segment": self.segment,
                "continues": continues,
                "preview": if self.preview.is_empty() { None } else { Some(self.preview.clone()) },
            }),
        );
        self.count = 0;
        self.bytes = 0;
        self.max_gap_ms = 0;
        self.max_gap_at_ms = 0;
        self.gaps_over_1s = 0;
        self.preview.clear();
    }
}

impl Drop for ChunkAgg<'_> {
    fn drop(&mut self) {
        self.flush();
    }
}

fn clipped_redacted(value: &Value, limit: usize) -> Value {
    let mut copy = value.clone();
    redact_acp_value(&mut copy);
    let text = serde_json::to_string(&copy).unwrap_or_default();
    if text.len() <= limit {
        copy
    } else {
        let mut cut = limit;
        while !text.is_char_boundary(cut) {
            cut -= 1;
        }
        Value::String(format!("{}…(+{} bytes)", &text[..cut], text.len() - cut))
    }
}

/// Records one ACP frame as types, ids and sizes (diagnostics only).
/// Display-only mirror of streamed output. Text goes to the room page's live
/// view and nowhere else: acceptance reads only the sealed `submit_result`.
fn mirror_live_frame(live: &super::live_stream::LiveSink, message: &Value) {
    if message.get("method").and_then(Value::as_str) != Some("session/update") {
        return;
    }
    let update = &message["params"]["update"];
    match update["sessionUpdate"].as_str() {
        Some("agent_message_chunk") => {
            if let Some(text) = update["content"]["text"].as_str() {
                live.push(super::live_stream::LiveKind::Message, text);
            }
        }
        Some("agent_thought_chunk") => {
            if let Some(text) = update["content"]["text"].as_str() {
                live.push(super::live_stream::LiveKind::Thought, text);
            }
        }
        Some("tool_call") => {
            if let Some(title) = update["title"].as_str() {
                live.activity(title);
            }
        }
        _ => {}
    }
}

fn trace_acp_frame(
    chunks: &mut ChunkAgg<'_>,
    message: &Value,
    line_bytes: usize,
    exchange_id: u64,
) {
    let Some(trace) = chunks.trace else {
        return;
    };
    let method = message.get("method").and_then(Value::as_str);
    if method == Some("session/update") {
        let update = &message["params"]["update"];
        let kind = update["sessionUpdate"].as_str().unwrap_or("?");
        if matches!(
            kind,
            "agent_message_chunk" | "agent_thought_chunk" | "user_message_chunk"
        ) {
            chunks.add(kind, update["content"]["text"].as_str().unwrap_or(""));
            return;
        }
        chunks.flush();
        let mut record = json!({"event":"update","update":kind,"line_bytes":line_bytes});
        match kind {
            "tool_call" | "tool_call_update" => {
                for key in ["toolCallId", "title", "kind", "status"] {
                    if let Some(value) = update.get(key) {
                        record[key] = value.clone();
                    }
                }
                if let Some(raw) = update.get("rawInput") {
                    record["raw_input_bytes"] = json!(raw.to_string().len());
                    if let Some(name) = raw
                        .get("name")
                        .or_else(|| raw.get("tool_name"))
                        .or_else(|| raw.get("toolName"))
                    {
                        record["raw_input_tool"] = name.clone();
                    }
                }
                if let Some(raw) = update.get("rawOutput") {
                    record["raw_output_bytes"] = json!(raw.to_string().len());
                }
                if let Some(content) = update.get("content") {
                    record["content_bytes"] = json!(content.to_string().len());
                    // A failed call's content is the adapter's error text
                    // (e.g. MCP startup, reviewer failure): kept, redacted
                    // and clipped, because it is the diagnosis.
                    if update.get("status").and_then(Value::as_str) == Some("failed") {
                        record["failure"] = clipped_redacted(content, 600);
                    }
                }
                if update.get("status").and_then(Value::as_str) == Some("failed") {
                    if let Some(raw) = update.get("rawOutput") {
                        record["failure_output"] = clipped_redacted(raw, 600);
                    }
                }
            }
            "plan" => {
                record["entries"] = json!(update["entries"].as_array().map(Vec::len));
            }
            _ => {
                record["detail"] = clipped_redacted(update, 800);
            }
        }
        trace.record("acp", record);
        return;
    }
    chunks.flush();
    if let Some(method) = method {
        let mut record = json!({"event":"notification","method":method,"line_bytes":line_bytes});
        if let Some(id) = message.get("id") {
            record["event"] = json!("agent_request");
            record["id"] = id.clone();
        }
        if method == "session/request_permission" {
            record["tool_title"] = message["params"]["toolCall"]["title"].clone();
            record["roundtable_tool"] = json!(tool_call_is_roundtable(&message["params"]));
            record["workspace_read"] = json!(tool_call_is_workspace_read(&message["params"], None));
            record["tool_kind"] = message["params"]["toolCall"]["kind"].clone();
            record["locations"] =
                clipped_redacted(&message["params"]["toolCall"]["locations"], 600);
        } else {
            let params = &message["params"];
            let inner = params.get("update").unwrap_or(params);
            record["update"] = inner.get("sessionUpdate").cloned().unwrap_or(Value::Null);
            record["detail"] = clipped_redacted(inner, 1500);
        }
        trace.record("acp", record);
        return;
    }
    let mut record = json!({"event":"response","id":message.get("id"),"line_bytes":line_bytes,"for_exchange": message.get("id") == Some(&json!(exchange_id))});
    if let Some(error) = message.get("error") {
        record["error"] = json!(describe_acp_rejection(error));
    } else if let Some(result) = message.get("result") {
        record["stop_reason"] = result.get("stopReason").cloned().unwrap_or(Value::Null);
        record["result"] = clipped_redacted(result, 1200);
    }
    trace.record("acp", record);
}

fn note_private_frame(exchange: &AcpExchange<'_>, label: &str) {
    let Some(log) = exchange.private_log else {
        return;
    };
    if let Ok(mut frames) = log.lock() {
        frames.push(label.to_string());
    }
}

fn check_failure_metadata(carrier: &Value, source: super::FailureSource) -> RtResult<()> {
    let meta = carrier.get("_meta").and_then(Value::as_object);
    // The shared classifier owns the envelope/version domain and guarantees
    // that a declared sessionFailure is classified or marked incompatible.
    let classification = super::classify_service_failure(meta, source, None, None);
    if classification.incompatible {
        return Err(rt_error(
            ErrorCode::CapabilityUnqualified,
            "adapter_incompatible",
        ));
    }
    if classification
        .records
        .iter()
        .any(|record| record.severity != "warning")
    {
        return Err(rt_error(ErrorCode::RuntimeUnavailable, "session_failure"));
    }
    Ok(())
}

fn parse_acp_frame(line: &[u8]) -> RtResult<Value> {
    let line = line.strip_suffix(b"\n").unwrap_or(line);
    let line = line.strip_suffix(b"\r").unwrap_or(line);
    match serde_json::from_slice::<Value>(line) {
        Ok(value) if value.is_object() => Ok(value),
        _ => {
            tracing::warn!(
                excerpt = %super::diagnostics::redact_untrusted_excerpt(line),
                "rejected ACP frame"
            );
            Err(rt_error(ErrorCode::RuntimeUnavailable, "acp_frame"))
        }
    }
}

const ROUNDTABLE_TOOL_NAMES: [&str; 3] = ["submit_result", "read_evidence", "search_evidence"];

#[cfg(test)]
fn permission_reply(params: &Value, request_id: &Value) -> Value {
    permission_reply_in(params, request_id, None)
}

fn permission_reply_in(params: &Value, request_id: &Value, workspace_root: Option<&Path>) -> Value {
    let allow =
        tool_call_is_roundtable(params) || tool_call_is_workspace_read(params, workspace_root);
    // ACP does not prove that allow_always is confined to this sealed attempt.
    // If one-shot consent is unavailable, select a rejection instead.
    let selected = selected_option(params, allow).or_else(|| {
        if allow {
            selected_option(params, false)
        } else {
            None
        }
    });
    match selected {
        Some(option_id) => json!({
            "jsonrpc": "2.0",
            "id": request_id,
            "result": {"outcome": {"outcome": "selected", "optionId": option_id}}
        }),
        None => json!({
            "jsonrpc": "2.0",
            "id": request_id,
            "error": {"code": -32601, "message": "Capability not available"}
        }),
    }
}

fn selected_option(params: &Value, allow: bool) -> Option<String> {
    let options = params.get("options")?.as_array()?;
    let kinds: &[&str] = if allow {
        &["allow_once"]
    } else {
        &["reject_once", "reject_always"]
    };
    for kind in kinds {
        if let Some(id) = options.iter().find_map(|option| {
            (option.get("kind").and_then(Value::as_str) == Some(*kind))
                .then(|| option.get("optionId").and_then(Value::as_str))
                .flatten()
                .map(str::to_owned)
        }) {
            return Some(id);
        }
    }
    None
}

/// A native read or search whose every declared location lies inside the
/// read-only workspace bind. Anything without locations, with a relative or
/// `..` path, or of any other kind (execute, edit, fetch, ...) is not one.
fn tool_call_is_workspace_read(params: &Value, workspace_root: Option<&Path>) -> bool {
    let call = &params["toolCall"];
    if !matches!(
        call.get("kind").and_then(Value::as_str),
        Some("read" | "search")
    ) {
        return false;
    }
    let Some(locations) = call.get("locations").and_then(Value::as_array) else {
        return false;
    };
    !locations.is_empty()
        && locations.iter().all(|location| {
            location
                .get("path")
                .and_then(Value::as_str)
                .is_some_and(|path| {
                    path_is_in_workspace_mount(path)
                        && workspace_root.is_none_or(|root| workspace_path_is_plain(root, path))
                })
        })
}

/// Live seats: the host file behind a `/workspace-ro` location exists and no
/// component under the workspace root is a symlink. A planted symlink would
/// resolve inside the container (for example to the seat's own `/rt-home`
/// credential overlay) and is never approved, even though it cannot reach
/// the host.
fn workspace_path_is_plain(root: &Path, path: &str) -> bool {
    let Some(rest) = path.strip_prefix(super::sandbox::WORKSPACE_MOUNT_DESTINATION) else {
        return false;
    };
    let mut current = root.to_path_buf();
    for part in rest.split('/').filter(|part| !part.is_empty()) {
        current.push(part);
        match std::fs::symlink_metadata(&current) {
            Ok(metadata) if !metadata.file_type().is_symlink() => {}
            _ => return false,
        }
    }
    true
}

fn path_is_in_workspace_mount(path: &str) -> bool {
    let root = super::sandbox::WORKSPACE_MOUNT_DESTINATION;
    let Some(rest) = path.strip_prefix(root) else {
        return false;
    };
    (rest.is_empty() || rest.starts_with('/'))
        && !rest
            .split('/')
            .any(|part| part == ".." || part == "." || part.contains('\0'))
}

/// Structured identity only. A free-text command such as `echo submit_result`
/// does not match. Grok's `use_tool` wrapper matches `rawInput.tool_name`
/// when it is exactly `roundtable__<tool>`. Antigravity 1.3.0 matches
/// `toolCall._meta` when it names the roundtable MCP server and tool.
/// The title `roundtable_<tool>` is accepted only together with that meta.
fn tool_call_is_roundtable(params: &Value) -> bool {
    let call = &params["toolCall"];
    if matches!(
        call.get("kind").and_then(Value::as_str),
        Some("execute" | "edit" | "delete" | "move" | "switch_mode")
    ) {
        return false;
    }
    if antigravity_roundtable_meta(call) {
        return true;
    }
    if cursor_roundtable_title(call) {
        return true;
    }
    // A machine name wins over display text. Only the explicit Grok use_tool
    // wrapper may route through its arguments, and only to a scoped MCP name.
    let Some(identity) = call
        .get("name")
        .or_else(|| call.get("title"))
        .and_then(Value::as_str)
    else {
        return false;
    };
    if identity == "use_tool" {
        let Some(input) = call.get("rawInput").and_then(Value::as_object) else {
            return false;
        };
        if input
            .keys()
            .any(|key| !matches!(key.as_str(), "tool_name" | "tool_input"))
            || !input.get("tool_input").is_some_and(Value::is_object)
        {
            return false;
        }
        return input
            .get("tool_name")
            .and_then(Value::as_str)
            .and_then(scoped_roundtable_tool)
            .is_some();
    }
    text_names_roundtable_tool(identity)
}

/// Antigravity 1.3.0 MCP permission identity. The single-underscore title is
/// not a match by itself; when present it has to name the same tool as
/// `_meta.mcp.tool`. A native title cannot be promoted by this meta.
fn antigravity_roundtable_meta(call: &Value) -> bool {
    let Some(meta) = call.get("_meta").and_then(Value::as_object) else {
        return false;
    };
    if meta.get("is_mcp_tool_call").and_then(Value::as_bool) != Some(true) {
        return false;
    }
    let Some(mcp) = meta.get("mcp").and_then(Value::as_object) else {
        return false;
    };
    if mcp.get("server").and_then(Value::as_str) != Some("roundtable") {
        return false;
    }
    let Some(tool) = mcp.get("tool").and_then(Value::as_str) else {
        return false;
    };
    if !ROUNDTABLE_TOOL_NAMES.contains(&tool) {
        return false;
    }
    if let Some(name) = call.get("name").and_then(Value::as_str) {
        if !single_underscore_title_agrees(name, tool) {
            return false;
        }
    }
    match call.get("title").and_then(Value::as_str) {
        Some(title) => single_underscore_title_agrees(title, tool),
        None => true,
    }
}

/// cursor-agent ACP asks for MCP consent with no machine name: kind
/// `other` and the title `"<server>: <tool>"` built from the session's
/// mcpServers key. Native cursor tools title themselves differently
/// ("Read `path`", "Edit `path`"), so only this exact shape counts.
fn cursor_roundtable_title(call: &Value) -> bool {
    if call.get("name").is_some() || call.get("kind").and_then(Value::as_str) != Some("other") {
        return false;
    }
    let Some(title) = call.get("title").and_then(Value::as_str) else {
        return false;
    };
    // Tool-call updates title the call `"roundtable: <tool>"`; the consent
    // request of cursor-agent 2026.09.28 titles it
    // `"roundtable-<tool>: <tool>"` (probe trace, 2026-10-10). Both name the
    // same tool twice or under the session's own server key; nothing looser.
    if let Some(tool) = title.strip_prefix("roundtable: ") {
        return ROUNDTABLE_TOOL_NAMES.contains(&tool);
    }
    title
        .strip_prefix("roundtable-")
        .and_then(|rest| rest.split_once(": "))
        .is_some_and(|(server_tool, tool)| {
            server_tool == tool && ROUNDTABLE_TOOL_NAMES.contains(&tool)
        })
}

fn single_underscore_title_agrees(title: &str, tool: &str) -> bool {
    if text_names_roundtable_tool(title) {
        return true;
    }
    title
        .strip_prefix("roundtable_")
        .is_some_and(|rest| !rest.starts_with('_') && rest == tool)
}

fn scoped_roundtable_tool(text: &str) -> Option<&str> {
    let name = text
        .strip_prefix("roundtable__")
        .or_else(|| text.strip_prefix("roundtable/"))
        .or_else(|| text.strip_prefix("mcp__roundtable__"))?;
    ROUNDTABLE_TOOL_NAMES.contains(&name).then_some(name)
}
fn text_names_roundtable_tool(text: &str) -> bool {
    ROUNDTABLE_TOOL_NAMES.contains(&text) || scoped_roundtable_tool(text).is_some()
}

async fn write_rpc<W: AsyncWrite + Unpin>(stdin: &mut W, value: &Value) -> RtResult<()> {
    let mut bytes = canonical_bytes(value)?;
    bytes.push(b'\n');
    stdin
        .write_all(&bytes)
        .await
        .map_err(|_| rt_error(ErrorCode::RuntimeUnavailable, "acp_write"))?;
    stdin
        .flush()
        .await
        .map_err(|_| rt_error(ErrorCode::RuntimeUnavailable, "acp_write"))
}

/// Newest proven-retired directories kept in addition to all active or
/// untracked attempts. A directory's age alone never authorizes deletion.
const RUN_DIR_RETENTION: usize = 8;
const RETIREMENT_MARKER: &str = ".roundtable-retired";

fn retire_attempt_files(runtime_root: &Path, incarnation: &str) -> RtResult<()> {
    let Some(runs) = anchored_runs(runtime_root)? else {
        return Ok(());
    };
    if !incarnation_name_is_safe(incarnation) {
        return Err(rt_error(ErrorCode::StorageUnavailable, "runtime_directory"));
    }
    let run_dir = runs.join(incarnation);
    delete_attempt_scratch(&runs, &run_dir)?;
    match std::fs::symlink_metadata(&run_dir) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(_) => return Err(rt_error(ErrorCode::StorageUnavailable, "runtime_directory")),
        Ok(meta) if meta.file_type().is_symlink() || !meta.is_dir() => {
            return Err(rt_error(ErrorCode::StorageUnavailable, "runtime_directory"))
        }
        Ok(_) => {}
    }
    let run_dir = contained_directory(&runs, &run_dir)?;
    // Only the host reaper writes this marker, after proof and scratch removal.
    let marker = run_dir.join(RETIREMENT_MARKER);
    match std::fs::symlink_metadata(&marker) {
        Ok(_) if retirement_record_matches(&run_dir) => {}
        Ok(_) => return Err(rt_error(ErrorCode::StorageUnavailable, "retirement_marker")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            // Publish only complete records. A failed write must not leave an
            // incomplete marker that makes later cleanup retries impossible.
            let temporary = run_dir.join(format!("{RETIREMENT_MARKER}.{}", uuid::Uuid::new_v4()));
            let written = (|| -> std::io::Result<()> {
                let mut file = std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&temporary)?;
                std::io::Write::write_all(&mut file, retirement_record(incarnation).as_bytes())?;
                file.sync_all()?;
                drop(file);
                std::fs::rename(&temporary, &marker)
            })();
            if written.is_err() {
                let _ = std::fs::remove_file(&temporary);
                return Err(rt_error(ErrorCode::StorageUnavailable, "retirement_marker"));
            }
        }
        Err(_) => return Err(rt_error(ErrorCode::StorageUnavailable, "retirement_marker")),
    }
    prune_run_dirs(&runs, RUN_DIR_RETENTION, incarnation);
    Ok(())
}

fn retirement_record(incarnation: &str) -> String {
    format!("roundtable-retired-v1:{incarnation}\n")
}

fn retirement_record_matches(run_dir: &Path) -> bool {
    let Some(name) = run_dir.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    // Even a stale marker cannot authorize removing newly created scratch.
    if !matches!(std::fs::symlink_metadata(run_dir.join("scratch")), Err(error) if error.kind() == std::io::ErrorKind::NotFound)
    {
        return false;
    }
    let marker = run_dir.join(RETIREMENT_MARKER);
    let expected = retirement_record(name);
    let Ok(meta) = std::fs::symlink_metadata(&marker) else {
        return false;
    };
    if meta.file_type().is_symlink() || !meta.is_file() || meta.len() != expected.len() as u64 {
        return false;
    }
    std::fs::read(marker).is_ok_and(|bytes| bytes == expected.as_bytes())
}

fn incarnation_name_is_safe(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && !name.contains('/')
        && !name.contains('\\')
        && !name.contains('\0')
}

fn anchored_runs(runtime_root: &Path) -> RtResult<Option<PathBuf>> {
    let runs = runtime_root.join("runs");
    match std::fs::symlink_metadata(&runs) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(rt_error(ErrorCode::StorageUnavailable, "runtime_directory")),
        Ok(meta) if meta.file_type().is_symlink() || !meta.is_dir() => {
            return Err(rt_error(ErrorCode::StorageUnavailable, "runtime_directory"));
        }
        Ok(_) => {}
    }
    let runs = runs
        .canonicalize()
        .map_err(|_| rt_error(ErrorCode::StorageUnavailable, "runtime_directory"))?;
    let root = runtime_root
        .canonicalize()
        .map_err(|_| rt_error(ErrorCode::StorageUnavailable, "runtime_directory"))?;
    if runs.parent() != Some(root.as_path()) {
        return Err(rt_error(ErrorCode::StorageUnavailable, "runtime_directory"));
    }
    Ok(Some(runs))
}

fn delete_attempt_scratch(runs: &Path, run_dir: &Path) -> RtResult<()> {
    match std::fs::symlink_metadata(run_dir) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(_) => return Err(rt_error(ErrorCode::StorageUnavailable, "runtime_directory")),
        Ok(meta) if meta.file_type().is_symlink() || !meta.is_dir() => {
            return Err(rt_error(ErrorCode::StorageUnavailable, "runtime_directory"));
        }
        Ok(_) => {}
    }
    let run_dir = contained_directory(runs, run_dir)?;
    let scratch = run_dir.join("scratch");
    match std::fs::symlink_metadata(&scratch) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(_) => return Err(rt_error(ErrorCode::StorageUnavailable, "runtime_directory")),
        Ok(meta) if meta.file_type().is_symlink() || !meta.is_dir() => {
            return Err(rt_error(ErrorCode::StorageUnavailable, "runtime_directory"));
        }
        Ok(_) => {}
    }
    let scratch = contained_directory(&run_dir, &scratch)?;
    remove_auth_copy(&scratch)?;
    std::fs::remove_dir_all(&scratch)
        .map_err(|_| rt_error(ErrorCode::StorageUnavailable, "runtime_directory"))?;
    Ok(())
}

/// Deletes `scratch/rt-home/.grok/auth.json` without following a symlink.
/// An intermediate symlink fails the reap so a host credential is not removed.
fn remove_auth_copy(scratch: &Path) -> RtResult<()> {
    let mut current = scratch.to_path_buf();
    let parts = ["rt-home", ".grok", "auth.json"];
    for (index, part) in parts.iter().enumerate() {
        let next = current.join(part);
        let last = index + 1 == parts.len();
        match std::fs::symlink_metadata(&next) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(_) => return Err(rt_error(ErrorCode::StorageUnavailable, "auth_copy")),
            Ok(meta) if meta.file_type().is_symlink() && last => {
                return std::fs::remove_file(&next)
                    .map_err(|_| rt_error(ErrorCode::StorageUnavailable, "auth_copy"));
            }
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(rt_error(ErrorCode::StorageUnavailable, "runtime_directory"));
            }
            Ok(_) if last => {
                return std::fs::remove_file(&next)
                    .map_err(|_| rt_error(ErrorCode::StorageUnavailable, "auth_copy"));
            }
            Ok(meta) if !meta.is_dir() => {
                return Err(rt_error(ErrorCode::StorageUnavailable, "runtime_directory"));
            }
            Ok(_) => current = next,
        }
    }
    Ok(())
}

fn contained_directory(parent: &Path, candidate: &Path) -> RtResult<PathBuf> {
    let canonical = candidate
        .canonicalize()
        .map_err(|_| rt_error(ErrorCode::StorageUnavailable, "runtime_directory"))?;
    if !canonical.starts_with(parent) || canonical == parent {
        return Err(rt_error(ErrorCode::StorageUnavailable, "runtime_directory"));
    }
    Ok(canonical)
}

fn prune_run_dirs(runs: &Path, keep: usize, protect: &str) {
    let Ok(entries) = std::fs::read_dir(runs) else {
        tracing::warn!("roundtable run retention unreadable");
        return;
    };
    let mut dirs = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(meta) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if meta.file_type().is_symlink() || !meta.is_dir() {
            continue;
        }
        let Ok(canonical) = path.canonicalize() else {
            continue;
        };
        if !canonical.starts_with(runs) || canonical == *runs {
            continue;
        }
        if canonical.file_name().and_then(|name| name.to_str()) == Some(protect)
            || !retirement_record_matches(&canonical)
        {
            continue;
        }
        let modified = meta.modified().unwrap_or(std::time::SystemTime::UNIX_EPOCH);
        dirs.push((modified, canonical));
    }
    dirs.sort_by(|left, right| right.0.cmp(&left.0).then_with(|| right.1.cmp(&left.1)));
    let protected = usize::from(
        std::fs::symlink_metadata(runs.join(protect))
            .ok()
            .is_some_and(|meta| meta.is_dir() && !meta.file_type().is_symlink()),
    );
    let drop_after = keep.saturating_sub(protected);
    for (_, path) in dirs.into_iter().skip(drop_after) {
        // Recheck immediately before deletion in case scratch was recreated.
        if !retirement_record_matches(&path) {
            continue;
        }
        if let Err(error) = std::fs::remove_dir_all(&path) {
            tracing::warn!(
                path = %path.display(),
                %error,
                "roundtable run retention"
            );
        }
    }
}
/// This gate runs after gateway handlers are drained and before reading the
/// sealed candidate. A normal ACP end_turn cannot erase a gateway failure.
pub(crate) fn complete_after_gateway_drain(
    result: RtResult<u64>,
    gateway: &LiveModelGateway,
) -> RtResult<u64> {
    let watermark = result?;
    if let Some(error) = gateway.completion_error() {
        return Err(error);
    }
    Ok(watermark)
}

/// `attempt_timeout` is written on the attempt before reap. It must win over
/// the unset-reason default `"cancelled"`, which is a permission cancel.
#[cfg(any(test, feature = "test-utils"))]
pub fn diagnostic_finish_reason_fixture(
    stored: Option<&str>,
    gateway: Option<String>,
    recorded: Option<String>,
) -> String {
    diagnostic_finish_reason(stored, gateway, recorded)
}

fn diagnostic_finish_reason(
    stored: Option<&str>,
    gateway: Option<String>,
    recorded: Option<String>,
) -> String {
    if stored == Some("attempt_timeout") {
        return "attempt_timeout".to_string();
    }
    gateway
        .or(recorded)
        .unwrap_or_else(|| "cancelled".to_string())
}

async fn persist_diagnostic(active: &Active) -> RtResult<()> {
    let gateway_reason = active
        .gateway
        .completion_error()
        .and_then(|error| error.details.reason);
    let stored = optional_row(
        active.store.connection(),
        "SELECT finish_reason FROM rt_attempts WHERE room_id=? AND attempt_id=?",
        vec![
            text(&active.room.to_string()),
            text(&active.attempt.to_string()),
        ],
    )
    .await?;
    let stored_reason = stored
        .as_ref()
        .map(|row| column::<Option<String>>(row, 0))
        .transpose()?
        .flatten();
    let reason = diagnostic_finish_reason(
        stored_reason.as_deref(),
        gateway_reason,
        active.finish_reason.lock().expect("finish reason").clone(),
    );
    persist_capture(
        &active.store,
        active.room,
        active.attempt,
        &active.assistant,
        &reason,
    )
    .await
}
async fn persist_capture(
    store: &RoundtableStore,
    room: RoomId,
    attempt: AttemptId,
    capture: &Mutex<Option<super::DiagnosticCapture>>,
    reason: &str,
) -> RtResult<()> {
    use sea_orm::TransactionTrait;
    let copy = capture.lock().expect("assistant diagnostic").clone();
    let Some(copy) = copy else {
        return Ok(());
    };
    let (excerpt, stream_hash) = copy.finish().await?;
    let mut split = if excerpt.truncated {
        excerpt.text.len() / 2
    } else {
        excerpt.text.len()
    };
    while !excerpt.text.is_char_boundary(split) {
        split -= 1;
    }
    let (prefix, suffix) = excerpt.text.split_at(split);
    let diagnostic = uuid::Uuid::new_v4().to_string();
    let txn = store
        .connection()
        .begin()
        .await
        .map_err(super::store::storage_err)?;
    exec(&txn,"INSERT INTO rt_diagnostics(room_id,diagnostic_id,attempt_id,finish_reason,error_class,assistant_prefix,assistant_suffix,total_bytes,stream_hash,truncated,ingress_seq,cleanup_result,redacted,byte_len) VALUES(?,?,?,?,?,?,?,?,?,?,0,'confirmed',1,?)",vec![text(&room.to_string()),text(&diagnostic),text(&attempt.to_string()),text(reason),text(reason),text(prefix),text(suffix),num(excerpt.total_bytes.min(i64::MAX as usize) as i64),text(&stream_hash.to_hex()),num(if excerpt.truncated{1}else{0}),num(excerpt.text.len() as i64)]).await?;
    let updated = exec(
        &txn,
        "UPDATE rt_attempts SET diagnostic_ref=?,finish_reason=? WHERE room_id=? AND attempt_id=?",
        vec![
            text(&diagnostic),
            text(reason),
            text(&room.to_string()),
            text(&attempt.to_string()),
        ],
    )
    .await?;
    if updated != 1 {
        return Err(rt_error(
            ErrorCode::StorageUnavailable,
            "diagnostic_attempt",
        ));
    }
    txn.commit().await.map_err(super::store::storage_err)?;
    capture.lock().expect("assistant diagnostic").take();
    Ok(())
}
#[cfg(any(test, feature = "test-utils"))]
pub async fn persist_runtime_diagnostic_fixture(
    store: &RoundtableStore,
    room: RoomId,
    attempt: AttemptId,
    capture: &Mutex<Option<super::DiagnosticCapture>>,
) -> RtResult<()> {
    persist_capture(store, room, attempt, capture, "fixture").await
}

/// A missing-artifact executor exercises the real pre-spawn rejection path.
/// Its report cannot pass, its OCI binary paths do not exist, and no model runs.
#[cfg(any(test, feature = "test-utils"))]
pub fn rejected_live_executor_fixture(root: PathBuf) -> RtResult<Arc<dyn RoundtableTurnExecutor>> {
    Ok(unqualified_live_fixture(root, None)?)
}
#[cfg(any(test, feature = "test-utils"))]
pub fn retired_live_executor_fixture(
    root: PathBuf,
    incarnation: IncarnationId,
) -> RtResult<Arc<dyn RoundtableTurnExecutor>> {
    Ok(unqualified_live_fixture(root, Some(incarnation))?)
}
#[cfg(any(test, feature = "test-utils"))]
fn unqualified_live_fixture(
    root: PathBuf,
    retired: Option<IncarnationId>,
) -> RtResult<Arc<LiveParticipantExecutor>> {
    let zero = Hash256::from_bytes([0; 32]);
    let binary = super::CertifiedBinary {
        role: "crun".into(),
        absolute_path: root.join("missing-crun").to_string_lossy().into_owned(),
        version: "fixture".into(),
        sha256: zero,
    };
    let key = super::QualificationKey {
        os: super::OsIdentity {
            name: "fixture".into(),
            version: "0".into(),
        },
        binaries: vec![binary.clone()],
        image_digest: String::new(),
        policy_hash: zero,
        tool_contract_hash: zero,
        core_hash: zero,
        adapter_version: "fixture".into(),
        isolator_version: "fixture".into(),
        plan_hash: zero,
    };
    let oci = QualifiedOciProfile {
        runtime: binary,
        rootfs: root.join("missing-rootfs"),
        rootfs_sha256: zero,
        runtime_root: root.join("runtime"),
        cgroup_root: root.join("missing-cgroup"),
        cli_args: Vec::new(),
        service_socket: None,
        gateway_socket: None,
        auth_mounts: Vec::new(),
        host_held_credentials: Vec::new(),
        container_env: BTreeMap::new(),
    };
    let installed = InstalledRuntime {
        schema_version: 1,
        qualification_key: key,
        report_path: root.join("missing-report"),
        report_sha256: zero,
        oci: oci.clone(),
        context_profile: QualifiedContextProfile::proposed(
            "fixture", zero, 2_000_000, 0, "fixture",
        ),
        providers: vec![super::installed_runtime::ProviderBinding {
            provider_ref: "fixture".into(),
            model: "fixture-model".into(),
            origin: "https://fixture.invalid".into(),
            credential_env: "MISSING_FIXTURE_CREDENTIAL".into(),
            supported_efforts: Vec::new(),
        }],
    };
    std::fs::create_dir_all(root.join("roundtable"))
        .map_err(|_| rt_error(ErrorCode::StorageUnavailable, "fixture_directory"))?;
    let db = root.join(crate::db::database_file_name());
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&db)
    {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(_) => return Err(rt_error(ErrorCode::StorageUnavailable, "fixture_database")),
    }
    let config = serde_json::to_vec(&installed)
        .map_err(|_| rt_error(ErrorCode::InvalidArgument, "fixture_config"))?;
    std::fs::write(root.join("roundtable/qualified-runtime.json"), config)
        .map_err(|_| rt_error(ErrorCode::StorageUnavailable, "fixture_config"))?;
    if let Some(incarnation) = retired {
        let journal = JournalLaunchIntentStore::open(&root.join("roundtable/launch-journal"))?;
        let db = DbIdentity::new(
            db.canonicalize()
                .map_err(|_| rt_error(ErrorCode::StorageUnavailable, "fixture_database"))?
                .to_string_lossy()
                .into_owned(),
        )?;
        let intent = LaunchIntent {
            owner_label: super::sandbox::owner_label(&db, Epoch(1), &incarnation),
            db,
            boot_epoch: Epoch(1),
            incarnation,
            image_digest: String::new(),
            plan_hash: zero,
            spawned: None,
            reaped: false,
        };
        journal.record(&intent)?;
        journal.mark_reaped(incarnation)?;
        drop(journal);
    }
    // Actual constructor must retain cleanup despite the missing report.
    Ok(Arc::new(LiveParticipantExecutor::load(root)?))
}

/// Applies a saved CodeBuddy profile to the fresh session: its mode, then
/// every saved config option (model, ...), each confirmed by the agent. The
/// adapter was qualified once; a profile only selects among the options the
/// agent itself advertises, so an option it rejects fails the seat.
async fn apply_seat_profile(
    stdin: &mut tokio::process::ChildStdin,
    stdout: &mut BufReader<tokio::process::ChildStdout>,
    session_id: &str,
    profile: &super::agent_credentials::CodeBuddyProfile,
    seq: &mut u64,
    active: &Active,
) -> RtResult<()> {
    let mut id = 100;
    if let Some(mode) = profile.mode_id.as_deref().filter(|mode| !mode.is_empty()) {
        rpc(
            stdin,
            stdout,
            id,
            "session/set_mode",
            json!({"sessionId": session_id, "modeId": mode}),
            seq,
            active,
        )
        .await
        .map_err(|_| {
            rt_error(
                ErrorCode::CapabilityUnqualified,
                "codebuddy_profile_rejected",
            )
        })?;
        id += 1;
    }
    for (key, value) in &profile.config_values {
        let selected = rpc(
            stdin,
            stdout,
            id,
            "session/set_config_option",
            json!({"sessionId": session_id, "configId": key, "value": value}),
            seq,
            active,
        )
        .await
        .map_err(|_| {
            rt_error(
                ErrorCode::CapabilityUnqualified,
                "codebuddy_profile_rejected",
            )
        })?;
        verify_confirmed_option(&selected, key, value).map_err(|_| {
            rt_error(
                ErrorCode::CapabilityUnqualified,
                "codebuddy_profile_rejected",
            )
        })?;
        id += 1;
    }
    Ok(())
}

fn verify_confirmed_option(response: &Value, id: &str, value: &str) -> RtResult<()> {
    if !response["configOptions"].as_array().is_some_and(|options| {
        options
            .iter()
            .any(|option| option["id"] == id && option["currentValue"] == value)
    }) {
        return Err(rt_error(
            ErrorCode::CapabilityUnqualified,
            "acp_config_unconfirmed",
        ));
    }
    Ok(())
}
#[cfg(any(test, feature = "test-utils"))]
pub fn verify_confirmed_option_fixture(response: &Value, id: &str, value: &str) -> RtResult<()> {
    verify_confirmed_option(response, id, value)
}

#[cfg(any(test, feature = "test-utils"))]
pub struct LiveAcpRpcObservation {
    pub terminal_disabled: bool,
    pub fs_write_disabled: bool,
    pub fs_read_disabled: bool,
    pub session_omits_capabilities: bool,
    pub allow_option_id: String,
    pub reject_option_id: String,
    pub saw_cancelled: bool,
    pub stop_reason: String,
    pub assistant_text: String,
}

#[cfg(any(test, feature = "test-utils"))]
pub async fn exercise_live_acp_rpc() -> RtResult<LiveAcpRpcObservation> {
    let session = roundtable_session_params(&json!([]));
    let session_omits_capabilities = session.get("clientCapabilities").is_none()
        && session.get("terminal").is_none()
        && session["cwd"] == "/scratch";
    let (client_io, adapter_io) = tokio::io::duplex(256 * 1024);
    let (client_read, mut client_write) = tokio::io::split(client_io);
    let (adapter_read, adapter_write) = tokio::io::split(adapter_io);
    let mut client_read = BufReader::new(client_read);
    let adapter = tokio::spawn(async move { fake_acp_adapter(adapter_read, adapter_write).await });
    let capture = Mutex::new(Some(super::DiagnosticCapture::new(Vec::new())));
    let rejected = AtomicBool::new(false);
    let mut seq = 0u64;
    let exchanged = async {
        let initialize = acp_exchange(
            &mut client_write,
            &mut client_read,
            &mut AcpExchange {
                id: 1,
                method: "initialize",
                params: roundtable_initialize_params("codeg-roundtable"),
                seq: &mut seq,
                assistant: Some(&capture),
                deadline: Some(Duration::from_secs(5)),
                rejected_permission: &rejected,
                private_log: None,
                trace: None,
                live: None,
                workspace_root: None,
            },
        )
        .await?;
        if initialize["protocolVersion"] != 1 {
            return Err(rt_error(ErrorCode::CapabilityUnqualified, "acp_version"));
        }
        acp_exchange(
            &mut client_write,
            &mut client_read,
            &mut AcpExchange {
                id: 5,
                method: "session/prompt",
                params: json!({"sessionId": "s", "prompt": [{"type": "text", "text": "submit"}]}),
                seq: &mut seq,
                assistant: Some(&capture),
                deadline: Some(Duration::from_secs(5)),
                rejected_permission: &rejected,
                private_log: None,
                trace: None,
                live: None,
                workspace_root: None,
            },
        )
        .await
    }
    .await;
    let result = match exchanged {
        Ok(result) => result,
        Err(error) => {
            adapter.abort();
            return Err(error);
        }
    };
    let seen = adapter
        .await
        .map_err(|_| rt_error(ErrorCode::RuntimeUnavailable, "acp_frame"))?
        .map_err(|_| rt_error(ErrorCode::RuntimeUnavailable, "acp_frame"))?;
    let assistant = capture
        .lock()
        .expect("assistant diagnostic")
        .clone()
        .ok_or_else(|| rt_error(ErrorCode::RuntimeUnavailable, "acp_frame"))?;
    let (excerpt, _) = assistant.finish().await?;
    let allow_body = seen.allow.to_string();
    let reject_body = seen.reject.to_string();
    Ok(LiveAcpRpcObservation {
        terminal_disabled: seen.initialize["clientCapabilities"]["terminal"] == false,
        fs_write_disabled: seen.initialize["clientCapabilities"]["fs"]["writeTextFile"] == false,
        fs_read_disabled: seen.initialize["clientCapabilities"]["fs"]["readTextFile"] == false,
        session_omits_capabilities,
        allow_option_id: seen.allow["result"]["outcome"]["optionId"]
            .as_str()
            .unwrap_or_default()
            .to_owned(),
        reject_option_id: seen.reject["result"]["outcome"]["optionId"]
            .as_str()
            .unwrap_or_default()
            .to_owned(),
        saw_cancelled: allow_body.contains("cancelled") || reject_body.contains("cancelled"),
        stop_reason: result["stopReason"].as_str().unwrap_or_default().to_owned(),
        assistant_text: excerpt.text,
    })
}

#[cfg(any(test, feature = "test-utils"))]
struct AdapterSeen {
    initialize: Value,
    allow: Value,
    reject: Value,
}

#[cfg(any(test, feature = "test-utils"))]
async fn fake_acp_adapter<R, W>(read: R, mut write: W) -> Result<AdapterSeen, ()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut read = BufReader::new(read);
    let initialize = read_adapter_frame(&mut read).await?;
    write_lenient(
        &mut write,
        &json!({
            "jsonrpc": "2.0",
            "id": initialize["id"],
            "result": {"protocolVersion": 1}
        }),
    )
    .await?;
    let _prompt = read_adapter_frame(&mut read).await?;
    write_lenient(
        &mut write,
        &json!({
            "jsonrpc": "2.0",
            "method": "session/update",
            "params": {
                "update": {
                    "sessionUpdate": "agent_message_chunk",
                    "content": {"type": "text", "text": "partial answer", "score": 0.25}
                }
            }
        }),
    )
    .await?;
    write_lenient(
        &mut write,
        &permission_request(
            11,
            "roundtable/submit_result",
            "allow-submit",
            "reject-submit",
        ),
    )
    .await?;
    let allow = read_adapter_frame(&mut read).await?;
    write_lenient(
        &mut write,
        &permission_request(
            12,
            "run_terminal_command",
            "allow-terminal",
            "reject-terminal",
        ),
    )
    .await?;
    let reject = read_adapter_frame(&mut read).await?;
    write_lenient(
        &mut write,
        &json!({"jsonrpc": "2.0", "id": 5, "result": {"stopReason": "end_turn"}}),
    )
    .await?;
    Ok(AdapterSeen {
        initialize: initialize["params"].clone(),
        allow,
        reject,
    })
}

#[cfg(any(test, feature = "test-utils"))]
fn permission_request(id: u64, title: &str, allow: &str, reject: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "session/request_permission",
        "params": {
            "sessionId": "s",
            "toolCall": {
                "toolCallId": format!("call-{id}"),
                "title": title,
                "kind": "other",
                "status": "pending",
                "rawInput": {"command": "echo submit_result"}
            },
            "options": [
                {"optionId": allow, "name": "Allow once", "kind": "allow_once"},
                {"optionId": format!("always-{allow}"), "name": "Allow always", "kind": "allow_always"},
                {"optionId": reject, "name": "Reject once", "kind": "reject_once"}
            ]
        }
    })
}

#[cfg(any(test, feature = "test-utils"))]
async fn read_adapter_frame<R: AsyncRead + Unpin>(read: &mut BufReader<R>) -> Result<Value, ()> {
    let mut line = String::new();
    let count = tokio::time::timeout(Duration::from_secs(5), read.read_line(&mut line))
        .await
        .map_err(|_| ())?
        .map_err(|_| ())?;
    if count == 0 {
        return Err(());
    }
    serde_json::from_str(&line).map_err(|_| ())
}

#[cfg(any(test, feature = "test-utils"))]
async fn write_lenient<W: AsyncWrite + Unpin>(write: &mut W, value: &Value) -> Result<(), ()> {
    let mut bytes = serde_json::to_vec(value).map_err(|_| ())?;
    bytes.push(b'\n');
    write.write_all(&bytes).await.map_err(|_| ())?;
    write.flush().await.map_err(|_| ())
}

#[cfg(any(test, feature = "test-utils"))]
pub fn permission_reply_fixture(params: Value) -> Value {
    permission_reply_in(&params, &json!(7), None)
}

#[cfg(any(test, feature = "test-utils"))]
pub fn acp_frame_fixture(bytes: &[u8]) -> RtResult<Value> {
    parse_acp_frame(bytes)
}

#[cfg(any(test, feature = "test-utils"))]
pub fn retire_attempt_files_fixture(runtime_root: &Path, incarnation: &str) -> RtResult<()> {
    retire_attempt_files(runtime_root, incarnation)
}

#[cfg(any(test, feature = "test-utils"))]
pub fn redact_untrusted_excerpt_fixture(bytes: &[u8]) -> String {
    super::diagnostics::redact_untrusted_excerpt(bytes)
}

#[cfg(any(test, feature = "test-utils"))]
pub fn run_dir_retention_fixture() -> usize {
    RUN_DIR_RETENTION
}

/// Controlled pre-exec owner exercising the actual live cancellation and
/// mandatory cleanup persistence path. No subprocess, provider or certificate
/// is run. The gateway carries explicit simulated remote uncertainty.
#[cfg(any(test, feature = "test-utils"))]
pub async fn prepared_live_cleanup_fixture(
    root: PathBuf,
    request: RoundtableTurnRequest,
) -> RtResult<Arc<dyn RoundtableTurnExecutor>> {
    let executor = unqualified_live_fixture(root.clone(), None)?;
    let installed = executor.adapters.values().next().expect("fixture adapter");
    let binding = TokenBinding {
        attempt_id: request.fence.attempt_id,
        room_id: request.room_id,
        fence: request.fence.clone(),
        speaker_id: request.speaker_id,
        tool_version: super::SERVICE_TOOL_VERSION.into(),
        aliases: request.scope.aliases.clone(),
        result_scope: request.scope.clone(),
        evidence: Default::default(),
        profile: installed.context_profile.clone(),
    };
    let facts = AdmissionFacts {
        certificate: QualificationStatus::NotTested,
        presented_key: installed.qualification_key.clone(),
        qualification_attempts_used: 0,
        qualification_spend_used: 0,
        fixture_hash: Hash256::from_bytes([0; 32]),
        recipient: "fixture-model".into(),
    };
    let authority = Arc::new(GateToolAuthority::open(
        &root,
        ExecutionScope::Fake,
        facts,
        MonoMs(0),
        MonoMs(u64::MAX),
        binding,
    ));
    let gateway = Arc::new(super::live_gateway::uncertain_gateway_for_cleanup_fixture(
        &root,
        installed.context_profile.clone(),
    )?);
    let mut diagnostic = super::DiagnosticCapture::new(Vec::new());
    diagnostic.push("controlled cleanup diagnostic");
    let incarnation = request.fence.incarnation;
    let scratch = installed
        .oci
        .runtime_root
        .join("runs")
        .join(incarnation.to_string())
        .join("scratch");
    let auth = scratch.join("rt-home/.grok/auth.json");
    std::fs::create_dir_all(auth.parent().expect("fixture auth parent"))
        .and_then(|_| std::fs::write(&auth, b"controlled-fixture-auth"))
        .map_err(|_| rt_error(ErrorCode::StorageUnavailable, "fixture_scratch"))?;
    request
        .store
        .record_launch(LaunchIntent {
            owner_label: super::sandbox::owner_label(
                &executor.db_identity,
                request.fence.boot_epoch,
                &incarnation,
            ),
            db: executor.db_identity.clone(),
            boot_epoch: request.fence.boot_epoch,
            incarnation,
            image_digest: String::new(),
            plan_hash: Hash256::from_bytes([0; 32]),
            spawned: None,
            reaped: false,
        })
        .await?;
    let active = Arc::new(Active {
        store: request.store,
        room: request.room_id,
        attempt: request.fence.attempt_id,
        scratch,
        launched: std::sync::atomic::AtomicBool::new(false),
        instance: Mutex::new(None),
        child: tokio::sync::Mutex::new(None),
        broker: tokio::sync::Mutex::new(None),
        gateway_server: tokio::sync::Mutex::new(None),
        gateway,
        authority,
        agent: "codex".into(),
        cleanup: tokio::sync::Mutex::new(None),
        stderr: tokio::sync::Mutex::new(None),
        assistant: Mutex::new(Some(diagnostic)),
        finish_reason: Mutex::new(Some("completed".into())),
        trace: None,
        live: super::live_stream::LiveSink::begin(
            "fixture-room",
            "fixture-attempt",
            "fixture-speaker",
            "fixture-phase",
            "proposal",
            "fixture-incarnation",
            Vec::new(),
        ),
        workspace_root: std::sync::OnceLock::new(),
    });
    executor
        .active
        .lock()
        .expect("active runtimes")
        .insert(incarnation, active);
    Ok(executor)
}

/// In-memory frames pass through the exact exchange used by live and probe
/// drivers. Callers may first stage a real durable scoped submission.
#[cfg(any(test, feature = "test-utils"))]
pub async fn drive_prompt_frames_fixture(frames: &[Value]) -> RtResult<u64> {
    let mut bytes = Vec::new();
    for frame in frames {
        bytes.extend(
            serde_json::to_vec(frame)
                .map_err(|_| rt_error(ErrorCode::InvalidArgument, "fixture_frame"))?,
        );
        bytes.push(b'\n');
    }
    let mut reader = BufReader::new(bytes.as_slice());
    let mut writer = tokio::io::sink();
    let rejected = AtomicBool::new(false);
    let mut seq = 0;
    acp_exchange(
        &mut writer,
        &mut reader,
        &mut AcpExchange {
            id: 5,
            method: "session/prompt",
            params: json!({}),
            seq: &mut seq,
            assistant: None,
            deadline: None,
            rejected_permission: &rejected,
            private_log: None,
            trace: None,
            live: None,
            workspace_root: None,
        },
    )
    .await?;
    Ok(seq)
}

#[cfg(any(test, feature = "test-utils"))]
pub fn session_params_fixture(agent: crate::models::AgentType, servers: &Value) -> Value {
    roundtable_session_params_for(agent, servers)
}

#[cfg(any(test, feature = "test-utils"))]
pub struct SchemaSeatObservation {
    pub search_option_id: String,
    pub grok_submit_option_id: String,
    pub antigravity_option_id: String,
    pub terminal_option_id: String,
    pub saw_cancelled_outcome: bool,
    pub repair_prompt: String,
    pub stop_reason: String,
    pub grok_disallows_terminal: bool,
    pub grok_keeps_use_tool: bool,
}

#[cfg(any(test, feature = "test-utils"))]
pub async fn exercise_schema_seat_rpc() -> RtResult<SchemaSeatObservation> {
    let (client_io, adapter_io) = tokio::io::duplex(256 * 1024);
    let (client_read, mut client_write) = tokio::io::split(client_io);
    let (adapter_read, adapter_write) = tokio::io::split(adapter_io);
    let mut client_read = BufReader::new(client_read);
    let adapter =
        tokio::spawn(async move { schema_seat_adapter(adapter_read, adapter_write).await });
    let capture = Mutex::new(Some(super::DiagnosticCapture::new(Vec::new())));
    let rejected = AtomicBool::new(false);
    let mut seq = 0u64;
    let exchanged = async {
        let initialize = acp_exchange(
            &mut client_write,
            &mut client_read,
            &mut AcpExchange {
                id: 1,
                method: "initialize",
                params: roundtable_initialize_params("codeg-roundtable"),
                seq: &mut seq,
                assistant: Some(&capture),
                deadline: Some(Duration::from_secs(5)),
                rejected_permission: &rejected,
                private_log: None,
                trace: None,
                live: None,
                workspace_root: None,
            },
        )
        .await?;
        if initialize["protocolVersion"] != 1 {
            return Err(rt_error(ErrorCode::CapabilityUnqualified, "acp_version"));
        }
        let session_params =
            roundtable_session_params_for(crate::models::AgentType::Grok, &json!([]));
        acp_exchange(
            &mut client_write,
            &mut client_read,
            &mut AcpExchange {
                id: 2,
                method: "session/new",
                params: session_params,
                seq: &mut seq,
                assistant: Some(&capture),
                deadline: Some(Duration::from_secs(5)),
                rejected_permission: &rejected,
                private_log: None,
                trace: None,
                live: None,
                workspace_root: None,
            },
        )
        .await?;
        finish_seat_prompt(
            &mut client_write,
            &mut client_read,
            "s",
            "submit the proposal",
            &mut seq,
            SeatPromptControl {
                assistant: Some(&capture),
                deadline: Some(Duration::from_secs(5)),
                gateway: None,
                trace: None,
                live: None,
                workspace_root: None,
            },
        )
        .await
    }
    .await;
    let result = match exchanged {
        Ok(result) => result,
        Err(error) => {
            adapter.abort();
            return Err(error);
        }
    };
    let seen = adapter
        .await
        .map_err(|_| rt_error(ErrorCode::RuntimeUnavailable, "acp_frame"))?
        .map_err(|_| rt_error(ErrorCode::RuntimeUnavailable, "acp_frame"))?;
    let bodies = format!(
        "{}{}{}{}",
        seen.search, seen.grok_submit, seen.antigravity, seen.terminal
    );
    Ok(SchemaSeatObservation {
        search_option_id: seen.search["result"]["outcome"]["optionId"]
            .as_str()
            .unwrap_or_default()
            .to_owned(),
        grok_submit_option_id: seen.grok_submit["result"]["outcome"]["optionId"]
            .as_str()
            .unwrap_or_default()
            .to_owned(),
        antigravity_option_id: seen.antigravity["result"]["outcome"]["optionId"]
            .as_str()
            .unwrap_or_default()
            .to_owned(),
        terminal_option_id: seen.terminal["result"]["outcome"]["optionId"]
            .as_str()
            .unwrap_or_default()
            .to_owned(),
        saw_cancelled_outcome: bodies.contains("cancelled"),
        repair_prompt: seen.repair_prompt,
        stop_reason: result["stopReason"].as_str().unwrap_or_default().to_owned(),
        grok_disallows_terminal: seen.session["_meta"]["agentProfile"]["disallowedTools"]
            .as_array()
            .is_some_and(|tools| tools.iter().any(|tool| tool == "run_terminal_command")),
        grok_keeps_use_tool: seen.session["_meta"]["agentProfile"]["disallowedTools"]
            .as_array()
            .is_some_and(|tools| {
                tools
                    .iter()
                    .all(|tool| tool != "use_tool" && tool != "search_tool")
            }),
    })
}

#[cfg(any(test, feature = "test-utils"))]
struct SchemaAdapterSeen {
    session: Value,
    search: Value,
    grok_submit: Value,
    antigravity: Value,
    terminal: Value,
    repair_prompt: String,
}

#[cfg(any(test, feature = "test-utils"))]
async fn schema_seat_adapter<R, W>(read: R, mut write: W) -> Result<SchemaAdapterSeen, ()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut read = BufReader::new(read);
    let _initialize = read_adapter_frame(&mut read).await?;
    write_lenient(
        &mut write,
        &json!({"jsonrpc":"2.0","id":1,"result":{"protocolVersion":1}}),
    )
    .await?;
    let session = read_adapter_frame(&mut read).await?;
    write_lenient(
        &mut write,
        &json!({"jsonrpc":"2.0","id":2,"result":{"sessionId":"s"}}),
    )
    .await?;
    let _prompt = read_adapter_frame(&mut read).await?;
    write_lenient(
        &mut write,
        &structured_permission(
            11,
            "use_tool",
            json!({
                "tool_name": "roundtable__search_evidence",
                "tool_input": {"file_alias": "e0", "query": "alpha", "limit": 5}
            }),
            "allow-search",
            "reject-search",
        ),
    )
    .await?;
    let search = read_adapter_frame(&mut read).await?;
    write_lenient(
        &mut write,
        &structured_permission(
            12,
            "use_tool",
            json!({
                "tool_name": "roundtable__submit_result",
                "tool_input": {
                    "submission_id": "s1",
                    "result": {
                        "kind": "proposal",
                        "summary": "one sentence",
                        "claims": [{
                            "local_key": "c1",
                            "text": "claim",
                            "evidence_aliases": [],
                            "confidence": "medium"
                        }]
                    }
                }
            }),
            "allow-grok-submit",
            "reject-grok-submit",
        ),
    )
    .await?;
    let grok_submit = read_adapter_frame(&mut read).await?;
    write_lenient(
        &mut write,
        &structured_permission(
            13,
            "roundtable/submit_result",
            json!({
                "submission_id": "s2",
                "result": {
                    "kind": "proposal",
                    "summary": "echo submit_result is not a tool name",
                    "claims": [{
                        "local_key": "c1",
                        "text": "claim",
                        "evidence_aliases": [],
                        "confidence": "medium"
                    }]
                }
            }),
            "allow-antigravity",
            "reject-antigravity",
        ),
    )
    .await?;
    let antigravity = read_adapter_frame(&mut read).await?;
    write_lenient(
        &mut write,
        &structured_permission(
            14,
            "run_terminal_command",
            json!({"command": "echo submit_result"}),
            "allow-terminal",
            "reject-terminal",
        ),
    )
    .await?;
    let terminal = read_adapter_frame(&mut read).await?;
    write_lenient(
        &mut write,
        &json!({"jsonrpc":"2.0","id":5,"result":{"stopReason":"cancelled"}}),
    )
    .await?;
    let repair = read_adapter_frame(&mut read).await?;
    let repair_prompt = repair["params"]["prompt"][0]["text"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    write_lenient(
        &mut write,
        &json!({"jsonrpc":"2.0","id": repair["id"],"result":{"stopReason":"end_turn"}}),
    )
    .await?;
    Ok(SchemaAdapterSeen {
        session: session["params"].clone(),
        search,
        grok_submit,
        antigravity,
        terminal,
        repair_prompt,
    })
}

#[cfg(any(test, feature = "test-utils"))]
fn structured_permission(
    id: u64,
    title: &str,
    raw_input: Value,
    allow: &str,
    reject: &str,
) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "session/request_permission",
        "params": {
            "sessionId": "s",
            "toolCall": {
                "toolCallId": format!("call-{id}"),
                "title": title,
                "kind": "other",
                "status": "pending",
                "rawInput": raw_input
            },
            "options": [
                {"optionId": allow, "name": "Allow once", "kind": "allow_once"},
                {"optionId": reject, "name": "Reject once", "kind": "reject_once"}
            ]
        }
    })
}

#[cfg(any(test, feature = "test-utils"))]
struct PromptFrameWriter(Vec<u8>);
#[cfg(any(test, feature = "test-utils"))]
impl AsyncWrite for PromptFrameWriter {
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
        bytes: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        self.0.extend_from_slice(bytes);
        std::task::Poll::Ready(Ok(bytes.len()))
    }
    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }
    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }
}

#[cfg(any(test, feature = "test-utils"))]
pub(crate) async fn permission_repair_frames_fixture(
    frames: &[Value],
    gateway: Option<&LiveModelGateway>,
) -> (RtResult<u64>, usize) {
    let mut bytes = Vec::new();
    for frame in frames {
        bytes.extend(serde_json::to_vec(frame).expect("fixture JSON"));
        bytes.push(b'\n');
    }
    let mut reader = BufReader::new(bytes.as_slice());
    let mut writer = PromptFrameWriter(Vec::new());
    let mut seq = 0;
    let result = finish_seat_prompt(
        &mut writer,
        &mut reader,
        "fixture-session",
        "fixture prompt",
        &mut seq,
        SeatPromptControl {
            assistant: None,
            deadline: None,
            gateway,
            trace: None,
            live: None,
            workspace_root: None,
        },
    )
    .await
    .map(|_| seq);
    let prompts = writer
        .0
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .filter(|line| {
            serde_json::from_slice::<Value>(line)
                .is_ok_and(|value| value["method"] == "session/prompt")
        })
        .count();
    (result, prompts)
}
#[cfg(any(test, feature = "test-utils"))]
pub async fn drive_permission_repair_frames_fixture(frames: &[Value]) -> (RtResult<u64>, usize) {
    permission_repair_frames_fixture(frames, None).await
}

#[cfg(test)]
mod completion_contract_tests {
    use super::*;

    #[test]
    fn attempt_timeout_is_not_recorded_as_cancelled() {
        assert_eq!(
            diagnostic_finish_reason(Some("attempt_timeout"), None, None),
            "attempt_timeout"
        );
        assert_eq!(
            diagnostic_finish_reason(Some("attempt_timeout"), Some("cancelled".to_string()), None),
            "attempt_timeout"
        );
        assert_eq!(diagnostic_finish_reason(None, None, None), "cancelled");
    }

    fn failure(severity: &str) -> Value {
        json!({"jetbrains":{"air":{"version":1,"sessionFailure":{"id":"provider","revision":1,"severity":severity}}}})
    }

    #[test]
    fn native_reads_are_allowed_only_inside_the_read_only_workspace() {
        let reply = |tool_call: Value| {
            let params = json!({"toolCall":tool_call,"options":[{"optionId":"allow","kind":"allow_once"},{"optionId":"reject","kind":"reject_once"}]});
            permission_reply(&params, &json!(3))["result"]["outcome"]["optionId"].clone()
        };
        for allowed in [
            json!({"title":"view_file","kind":"read","locations":[{"path":"/workspace-ro/src/main.rs"}]}),
            json!({"title":"grep","kind":"search","locations":[{"path":"/workspace-ro"},{"path":"/workspace-ro/docs/a.md","line":3}]}),
        ] {
            assert_eq!(reply(allowed.clone()), "allow", "{allowed}");
        }
        for rejected in [
            json!({"title":"view_file","kind":"read"}),
            json!({"title":"view_file","kind":"read","locations":[]}),
            json!({"title":"view_file","kind":"read","locations":[{"path":"/rt-home/.grok/auth.json"}]}),
            json!({"title":"view_file","kind":"read","locations":[{"path":"/workspace-ro/../rt-home/x"}]}),
            json!({"title":"view_file","kind":"read","locations":[{"path":"/workspace-rogue/x"}]}),
            json!({"title":"view_file","kind":"read","locations":[{"path":"workspace-ro/x"}]}),
            json!({"title":"view_file","kind":"read","locations":[{"path":"/workspace-ro/a"},{"path":"/scratch/b"}]}),
            json!({"title":"edit","kind":"edit","locations":[{"path":"/workspace-ro/a"}]}),
            json!({"title":"rm","kind":"delete","locations":[{"path":"/workspace-ro/a"}]}),
            json!({"title":"cat /workspace-ro/a","kind":"execute","locations":[{"path":"/workspace-ro/a"}]}),
            json!({"title":"fetch","kind":"fetch","locations":[{"path":"/workspace-ro/a"}]}),
        ] {
            assert_eq!(reply(rejected.clone()), "reject", "{rejected}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn live_reads_through_a_planted_symlink_are_rejected() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("src")).unwrap();
        std::fs::write(root.path().join("src/a.rs"), b"fn a() {}").unwrap();
        std::os::unix::fs::symlink("/rt-home/.grok/auth.json", root.path().join("creds")).unwrap();
        std::os::unix::fs::symlink("src", root.path().join("alias")).unwrap();
        let reply = |path: &str| {
            let params = json!({"toolCall":{"title":"view_file","kind":"read","locations":[{"path":path}]},"options":[{"optionId":"allow","kind":"allow_once"},{"optionId":"reject","kind":"reject_once"}]});
            permission_reply_in(&params, &json!(1), Some(root.path()))["result"]["outcome"]
                ["optionId"]
                .clone()
        };
        assert_eq!(reply("/workspace-ro/src/a.rs"), "allow");
        assert_eq!(reply("/workspace-ro"), "allow");
        assert_eq!(reply("/workspace-ro/creds"), "reject");
        assert_eq!(reply("/workspace-ro/alias/a.rs"), "reject");
        assert_eq!(reply("/workspace-ro/missing.rs"), "reject");
    }

    #[test]
    fn grok_keeps_read_tools_and_denies_writes_terminals_and_network() {
        for kept in ["read_file", "grep", "list_dir", "use_tool", "search_tool"] {
            assert!(!GROK_ROUNDTABLE_DISALLOWED_TOOLS.contains(&kept), "{kept}");
        }
        for denied in [
            "run_terminal_cmd",
            "run_terminal_command",
            "search_replace",
            "write",
            "web_search",
            "web_fetch",
            "task",
            "spawn_subagent",
        ] {
            assert!(
                GROK_ROUNDTABLE_DISALLOWED_TOOLS.contains(&denied),
                "{denied}"
            );
        }
    }

    #[test]
    fn permission_identity_cannot_be_supplied_by_descriptive_text_or_arguments() {
        for tool_call in [
            json!({"title":"run_terminal_command submit_result","kind":"execute"}),
            json!({"title":"run_terminal_command","_meta":{"description":"read_evidence"}}),
            json!({"title":"run_terminal_command","rawInput":"submit_result"}),
            json!({"title":"run_terminal_command","rawInput":{"name":"search_evidence","command":"execute"}}),
            json!({"name":"run_terminal_command","title":"roundtable/submit_result"}),
            json!({"title":"roundtable/submit_result","kind":"execute"}),
            json!({"rawInput":{"toolName":"read_evidence"}}),
            json!({"name":"run_terminal_command","title":"use_tool","rawInput":{"tool_name":"roundtable__submit_result","tool_input":{}}}),
            json!({"title":"use_tool","rawInput":{"tool_name":"submit_result","tool_input":{}}}),
            json!({"title":"use_tool","rawInput":{"tool_name":"run_terminal_command","tool_input":{}}}),
            json!({"title":"use_tool","rawInput":{"tool_name":"roundtable__read_evidence","tool_input":{},"server":"other"}}),
            json!({"title":"use_tool","rawInput":{"tool_name":"roundtable__read_evidence","tool_input":[]}}),
            // Cursor consent shape: only `roundtable: <roundtable tool>`, kind other, no name.
            json!({"title":"roundtable: run_terminal_command","kind":"other"}),
            json!({"title":"other: submit_result","kind":"other"}),
            json!({"title":"roundtable: submit_result","kind":"execute"}),
            json!({"title":"roundtable: submit_result"}),
            json!({"name":"run_terminal_command","title":"roundtable: submit_result","kind":"other"}),
            json!({"title":"roundtable:  submit_result","kind":"other"}),
            json!({"title":"roundtable-run_terminal_command: run_terminal_command","kind":"other"}),
            json!({"title":"roundtable-submit_result: read_evidence","kind":"other"}),
            json!({"title":"other-submit_result: submit_result","kind":"other"}),
            json!({"title":"roundtable-submit_result:submit_result","kind":"other"}),
            json!({"title":"roundtable-submit_result: submit_result","kind":"execute"}),
            json!({"name":"shell","title":"roundtable-submit_result: submit_result","kind":"other"}),
            json!({"title":"roundtable: read_file","kind":"other"}),
            json!({"title":"roundtable: view_file","kind":"other"}),
            json!({"title":"roundtable: submit_result ","kind":"other"}),
            json!({"title":"Roundtable: submit_result","kind":"other"}),
        ] {
            let params = json!({"toolCall":tool_call,"options":[{"optionId":"allow","kind":"allow_once"},{"optionId":"reject","kind":"reject_once"}]});
            assert_eq!(
                permission_reply(&params, &json!(9))["result"]["outcome"]["optionId"],
                "reject",
                "{params}"
            );
        }
        for name in [
            "roundtable/submit_result",
            "mcp__roundtable__read_evidence",
            "search_evidence",
            "roundtable: submit_result",
            "roundtable: read_evidence",
            "roundtable: search_evidence",
            "roundtable-submit_result: submit_result",
            "roundtable-read_evidence: read_evidence",
            "roundtable-search_evidence: search_evidence",
        ] {
            let params = json!({"toolCall":{"title":name,"kind":"other"},"options":[{"optionId":"allow","kind":"allow_once"},{"optionId":"reject","kind":"reject_once"}]});
            assert_eq!(
                permission_reply(&params, &json!(9))["result"]["outcome"]["optionId"],
                "allow"
            );
        }
    }

    #[test]
    fn sealed_tools_do_not_gain_unqualified_persistent_permission() {
        for options in [
            json!([{"optionId":"always","kind":"allow_always"},{"optionId":"reject","kind":"reject_once"}]),
            json!([{"optionId":"always","kind":"allow_always"}]),
        ] {
            let response = permission_reply(
                &json!({"toolCall":{"title":"roundtable/submit_result"},"options":options}),
                &json!(1),
            );
            assert_ne!(response["result"]["outcome"]["optionId"], "always");
            assert!(
                response["result"]["outcome"]["optionId"] == "reject"
                    || response["error"]["code"] == -32601
            );
        }
    }

    #[test]
    fn request_envelope_proof_is_missing_bounded_and_hash_verified() {
        let dir = tempfile::tempdir().unwrap();
        rejected_live_executor_fixture(dir.path().into()).unwrap();
        let mut installed = InstalledRuntime::load(dir.path()).unwrap();
        let save = |installed: &mut InstalledRuntime, report: Value| {
            // Invalid protocol values must reach the production report reader.
            let bytes = serde_json::to_vec(&report).unwrap();
            std::fs::write(&installed.report_path, &bytes).unwrap();
            installed.report_sha256 = Hash256::sha256(&bytes);
        };
        save(&mut installed, json!({}));
        assert_eq!(verified_request_envelope_bound(&installed).unwrap(), None);
        save(
            &mut installed,
            json!({"request_envelope":{"status":"not_tested","max_bytes":null,"evidence_hash":null}}),
        );
        assert_eq!(verified_request_envelope_bound(&installed).unwrap(), None);
        for bytes in [json!(-1), json!(1.5), json!(1_048_577), Value::Null] {
            save(
                &mut installed,
                json!({"request_envelope":{"status":"passed","max_bytes":bytes,"evidence_hash":Hash256::sha256(b"fixture envelope measurement")}}),
            );
            assert!(verified_request_envelope_bound(&installed).is_err());
        }
        save(
            &mut installed,
            json!({"request_envelope":{"status":"passed","max_bytes":4096,"evidence_hash":Hash256::sha256(b"fixture envelope measurement")}}),
        );
        assert_eq!(
            verified_request_envelope_bound(&installed).unwrap(),
            Some(4096)
        );
        std::fs::write(&installed.report_path, b"{}").unwrap();
        assert_eq!(
            verified_request_envelope_bound(&installed)
                .unwrap_err()
                .details
                .reason
                .as_deref(),
            Some("qualification_report_changed")
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn runtime_auth_denylist_redacts_mounted_json_value_without_environment_seed() {
        let dir = tempfile::tempdir().unwrap();
        rejected_live_executor_fixture(dir.path().into()).unwrap();
        let mut installed = InstalledRuntime::load(dir.path()).unwrap();
        let path = dir.path().join("auth.json");
        std::fs::write(
            &path,
            r#"{"tokens":{"access_token":"fixture-mount-secret"}}"#,
        )
        .unwrap();
        installed.oci.auth_mounts.push(AuthMount {
            source: path,
            destination: "/rt-home/auth.json".into(),
        });
        let mut capture =
            super::super::DiagnosticCapture::new(auth_denylist(&installed, &[]).unwrap());
        capture.push("token: fixture-mount-");
        capture.push("secret");
        let (excerpt, _) = capture.finish().await.unwrap();
        assert_eq!(excerpt.text, "token: [redacted]");
    }

    #[test]
    fn missing_provider_credential_is_a_capability_error_not_app_authentication() {
        let dir = tempfile::tempdir().unwrap();
        rejected_live_executor_fixture(dir.path().into()).unwrap();
        let installed = InstalledRuntime::load(dir.path()).unwrap();
        let mut provider = installed.providers[0].clone();
        provider.credential_env = format!("ABSENT_FIXTURE_{}", uuid::Uuid::new_v4().simple());
        let error = require_provider_credential(&installed, &provider, false).unwrap_err();
        assert_eq!(error.code, ErrorCode::CapabilityUnqualified);
        assert_eq!(
            error.details.reason.as_deref(),
            Some("provider_credential_missing")
        );
        // A key saved in the agent's Codeg settings satisfies the seat.
        assert!(require_provider_credential(&installed, &provider, true).is_ok());
    }

    #[tokio::test]
    async fn actual_prompt_rpc_rejects_update_or_response_failure_before_end_turn() {
        for frames in [
            vec![
                json!({"method":"session/update","params":{"update":{"sessionUpdate":"session_info_update","_meta":failure("error")}}}),
                json!({"id":5,"result":{"stopReason":"end_turn"}}),
            ],
            vec![json!({"id":5,"result":{"stopReason":"end_turn","_meta":failure("error")}})],
        ] {
            assert!(drive_prompt_frames_fixture(&frames).await.is_err());
        }
        assert!(drive_prompt_frames_fixture(&[
            json!({"id":5,"result":{"stopReason":"end_turn","_meta":failure("warning")}})
        ])
        .await
        .is_ok());
    }

    #[tokio::test]
    async fn permission_cancel_repairs_are_bounded_and_cannot_hide_terminal_failure() {
        let denied = |id| permission_request(id, "run_terminal_command", "allow", "reject");
        let cancelled = |id| json!({"id":id,"result":{"stopReason":"cancelled"}});
        let ended = |id| json!({"id":id,"result":{"stopReason":"end_turn"}});
        let (clean, prompts) = drive_permission_repair_frames_fixture(&[
            denied(11),
            cancelled(5),
            denied(12),
            cancelled(6),
            ended(7),
        ])
        .await;
        assert!(clean.is_ok());
        assert_eq!(prompts, 3);
        let (exhausted, prompts) = drive_permission_repair_frames_fixture(&[
            denied(11),
            cancelled(5),
            denied(12),
            cancelled(6),
            denied(13),
            cancelled(7),
            ended(8),
        ])
        .await;
        assert!(exhausted.is_err());
        assert_eq!(prompts, 3);
        let (failed, prompts) = drive_permission_repair_frames_fixture(&[
            denied(11),
            json!({"id":5,"result":{"stopReason":"cancelled","_meta":failure("error")}}),
            ended(6),
        ])
        .await;
        assert_eq!(
            failed.unwrap_err().details.reason.as_deref(),
            Some("session_failure")
        );
        assert_eq!(prompts, 1);
        let (warning, prompts) = drive_permission_repair_frames_fixture(&[
            denied(11),
            json!({"id":5,"result":{"stopReason":"cancelled","_meta":failure("warning")}}),
            ended(6),
        ])
        .await;
        assert!(warning.is_ok());
        assert_eq!(prompts, 2);
    }

    #[tokio::test]
    async fn unsigned_out_of_parser_domain_failure_version_never_completes() {
        let mut declared = failure("error");
        declared["jetbrains"]["air"]["version"] = json!(9_223_372_036_854_775_808u64);
        for frames in [
            vec![
                json!({"method":"session/update","params":{"update":{"sessionUpdate":"session_info_update","_meta":declared}}}),
                json!({"id":5,"result":{"stopReason":"end_turn"}}),
            ],
            vec![json!({"id":5,"result":{"stopReason":"end_turn","_meta":declared}})],
        ] {
            let error = drive_prompt_frames_fixture(&frames).await.unwrap_err();
            assert_eq!(
                error.details.reason.as_deref(),
                Some("adapter_incompatible")
            );
        }
        assert!(drive_prompt_frames_fixture(&[
            json!({"id":5,"result":{"stopReason":"end_turn","_meta":failure("warning")}})
        ])
        .await
        .is_ok());
    }

    #[tokio::test]
    async fn actual_prompt_rpc_fails_closed_on_malformed_failure_http_and_abnormal_stop() {
        for response in [
            json!({"stopReason":"end_turn","_meta":{"jetbrains":{"air":{"version":1,"sessionFailure":{}}}}}),
            json!({"stopReason":"end_turn","_meta":{"jetbrains":{"air":{"sessionFailure":{}}}}}),
            json!({"stopReason":"max_tokens"}),
        ] {
            assert!(
                drive_prompt_frames_fixture(&[json!({"id":5,"result":response})])
                    .await
                    .is_err()
            );
        }
        assert!(drive_prompt_frames_fixture(&[
            json!({"id":5,"error":{"code":-32000,"data":{"httpStatus":429}}})
        ])
        .await
        .is_err());
    }

    #[test]
    fn acp_rejection_keeps_code_message_and_details() {
        let text = describe_acp_rejection(&json!({
            "code": -32603,
            "message": "fetchAvailableModels failed: [Errno 111] Connection refused",
            "data": {
                "details": {
                    "errno": 111,
                    "access_token": "secret-value",
                    "trace": "sk-live"
                }
            }
        }));
        assert!(text.contains("code=-32603"), "{text}");
        assert!(
            text.contains("message=fetchAvailableModels failed: [Errno 111] Connection refused"),
            "{text}"
        );
        assert!(text.contains("\"errno\":111"), "{text}");
        assert!(!text.contains("secret-value"), "{text}");
        assert!(!text.to_ascii_lowercase().contains("token"), "{text}");
        assert!(!text.contains("sk-"), "{text}");
    }
}

#[cfg(test)]
mod attempt_trace_acp_tests {
    use super::*;

    #[test]
    fn acp_frames_are_traced_as_shapes_and_chunks_coalesce() {
        let dir = tempfile::tempdir().unwrap();
        let trace = AttemptTrace::open(
            dir.path(),
            "room-a",
            "attempt-a",
            vec!["seat-secret-123456".into()],
        )
        .unwrap();
        {
            let mut chunks = ChunkAgg::new(Some(&trace));
            for text in ["Synthesis ", "submitted."] {
                trace_acp_frame(
                    &mut chunks,
                    &json!({"method":"session/update","params":{"update":{"sessionUpdate":"agent_message_chunk","content":{"text":text}}}}),
                    10,
                    5,
                );
            }
            trace_acp_frame(
                &mut chunks,
                &json!({"method":"_x.ai/session_notification","params":{"update":{"sessionUpdate":"retry_state","type":"retrying","attempt":1,"reason":"API error seat-secret-123456","token":"sk-abc"}}}),
                10,
                5,
            );
            trace_acp_frame(
                &mut chunks,
                &json!({"method":"session/update","params":{"update":{"sessionUpdate":"tool_call","toolCallId":"t1","title":"use_tool","status":"pending","rawInput":{"name":"submit_result","arguments":{"x":"BODY"}}}}}),
                10,
                5,
            );
            trace_acp_frame(
                &mut chunks,
                &json!({"id":5,"result":{"stopReason":"end_turn"}}),
                10,
                5,
            );
        }
        let text = std::fs::read_to_string(
            dir.path()
                .join("roundtable/diag/room-a/attempt-a/acp.jsonl"),
        )
        .unwrap();
        assert!(text.contains("\"count\":2"), "{text}");
        assert!(text.contains("Synthesis submitted."), "{text}");
        assert!(text.contains("retry_state"), "{text}");
        assert!(text.contains("\"stop_reason\":\"end_turn\""), "{text}");
        assert!(
            text.contains("\"raw_input_tool\":\"submit_result\""),
            "{text}"
        );
        assert!(!text.contains("seat-secret-123456"), "{text}");
        assert!(!text.contains("sk-abc"), "{text}");
        assert!(!text.contains("BODY"), "{text}");
    }

    #[test]
    fn long_same_type_streams_record_gaps_and_segments() {
        let dir = tempfile::tempdir().unwrap();
        let trace = AttemptTrace::open(dir.path(), "room-g", "attempt-g", Vec::new()).unwrap();
        {
            let mut chunks = ChunkAgg::new(Some(&trace));
            // Steady thinking, one 7s stall, then 50s more: two 30s segments.
            let mut t = 1_000;
            for _ in 0..20 {
                chunks.add_at("agent_thought_chunk", "abc", t);
                t += 250;
            }
            t += 7_000;
            for _ in 0..200 {
                chunks.add_at("agent_thought_chunk", "abc", t);
                t += 250;
            }
            chunks.add_at("agent_message_chunk", "done", t);
        }
        let text = std::fs::read_to_string(
            dir.path()
                .join("roundtable/diag/room-g/attempt-g/acp.jsonl"),
        )
        .unwrap();
        let records: Vec<Value> = text
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        let gaps: Vec<_> = records
            .iter()
            .filter(|record| record["event"] == "stream_gap")
            .collect();
        assert_eq!(gaps.len(), 1, "{text}");
        assert_eq!(gaps[0]["gap_ms"], 7_250);
        let thought: Vec<_> = records
            .iter()
            .filter(|record| {
                record["event"] == "chunks" && record["update"] == "agent_thought_chunk"
            })
            .collect();
        assert!(thought.len() >= 2, "segmented: {text}");
        assert_eq!(thought[0]["continues"], true);
        assert_eq!(thought[0]["max_gap_ms"], 7_250);
        assert_eq!(thought[0]["gaps_over_1s"], 1);
        assert_eq!(thought.last().unwrap()["continues"], false);
        let total: u64 = thought
            .iter()
            .map(|record| record["count"].as_u64().unwrap())
            .sum();
        assert_eq!(total, 220);
    }

    #[test]
    fn usage_token_counts_survive_redaction_but_secret_keys_do_not() {
        let mut frame = json!({
            "usage": {"input_tokens": 1234, "output_tokens": 56, "cache_read_input_tokens": 7, "reasoning_tokens": 890},
            "_meta": {"totalContextTokens": "256000"},
            "inputTokens": 12,
            "access_token": "tok-live-1",
            "apiKey": "key-live-2",
            "client_secret": "sec-live-3",
            "x-xai-token-auth": "hdr-live-4",
            "token": "tok-live-5",
            "tokenizer": "o200k",
            "sessionId": "s-1"
        });
        redact_acp_value(&mut frame);
        assert_eq!(frame["usage"]["input_tokens"], 1234);
        assert_eq!(frame["usage"]["output_tokens"], 56);
        assert_eq!(frame["usage"]["reasoning_tokens"], 890);
        assert_eq!(frame["_meta"]["totalContextTokens"], "256000");
        assert_eq!(frame["inputTokens"], 12);
        assert_eq!(frame["tokenizer"], "o200k");
        assert_eq!(frame["sessionId"], "s-1");
        for key in [
            "access_token",
            "apiKey",
            "client_secret",
            "x-xai-token-auth",
            "token",
        ] {
            assert_eq!(frame[key], "[redacted]", "{key}");
        }
    }
}
