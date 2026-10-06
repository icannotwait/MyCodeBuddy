//! Qualified OCI + ACP participant implementation. No ordinary conversation
//! event bus is attached; the only data capabilities are the private brokers.
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
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

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
}

pub(crate) struct LiveParticipantExecutor {
    data_dir: PathBuf,
    adapters: BTreeMap<String, InstalledRuntime>,
    isolator: Arc<LinuxOciIsolator>,
    db_identity: DbIdentity,
    active: Mutex<HashMap<IncarnationId, Arc<Active>>>,
    launches: Mutex<HashMap<IncarnationId, bool>>,
    registry: tokio::sync::Mutex<Option<super::RoundtableSessionRegistry>>,
}

fn auth_denylist(installed: &InstalledRuntime, seeds: &[String]) -> Vec<String> {
    let mut secrets = seeds.to_vec();
    for mount in &installed.oci.auth_mounts {
        let Ok(bytes) = std::fs::read(&mount.source) else {
            continue;
        };
        if bytes.len() > 65_536 {
            continue;
        }
        if let Ok(text) = String::from_utf8(bytes) {
            if !text.trim().is_empty() {
                secrets.push(text);
            }
        }
    }
    secrets
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
    !installed.oci.auth_mounts.is_empty()
        && installed.oci.auth_mounts.iter().all(|mount| {
            let path = std::path::Path::new(&mount.source);
            path.is_absolute() && path.is_file()
        })
}

fn credential_ready(
    installed: &InstalledRuntime,
    binding: &super::installed_runtime::ProviderBinding,
) -> bool {
    std::env::var_os(&binding.credential_env).is_some() || file_auth_ready(installed)
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
            launches: Mutex::new(HashMap::new()),
            registry: tokio::sync::Mutex::new(None),
        })
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
            return Ok(CleanupProof {
                process,
                mailbox_empty: true,
                tools_drained: true,
                ingress_drained: true,
            });
        };
        let mut cached = active.cleanup.lock().await;
        if let Some(proof) = cached.as_ref() {
            return Ok(proof.clone());
        }
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
            *cached = Some(proof.clone());
            self.active
                .lock()
                .expect("active runtimes")
                .remove(&incarnation);
            self.launches
                .lock()
                .expect("launch lifecycle")
                .remove(&incarnation);
            return Ok(proof);
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
        persist_diagnostic(&active).await?;
        active.store.mark_launch_reaped(incarnation).await?;
        exec(active.store.connection(),"UPDATE rt_attempts SET cleanup_state='confirmed',residual_remote_work=? WHERE room_id=? AND attempt_id=?",vec![num(if active.gateway.remote_work_uncertain(){1}else{0}),text(&active.room.to_string()),text(&active.attempt.to_string())]).await?;
        *cached = Some(proof.clone());
        self.active
            .lock()
            .expect("active runtimes")
            .remove(&incarnation);
        self.launches
            .lock()
            .expect("launch lifecycle")
            .remove(&incarnation);
        Ok(proof)
    }
}

impl TokenBound for LiveParticipantExecutor {
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
            let agent = participant_agent(participant)?;
            let installed = self.verified(&agent).await?;
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
            if !credential_ready(&installed, binding) {
                return Err(rt_error(
                    ErrorCode::Unauthenticated,
                    "provider_credential_missing",
                ));
            }
            if policy_hash.is_some_and(|hash| hash != installed.qualification_key.policy_hash) {
                return Err(rt_error(
                    ErrorCode::CapabilityUnqualified,
                    "qualification_policy_changed",
                ));
            }
            policy_hash = Some(installed.qualification_key.policy_hash);
            profile = Some(installed.context_profile.clone());
            keys.push(json!(installed.qualification_key));
            recipients.push(json!({"provider_ref":binding.provider_ref,"model":binding.model,"origin":binding.origin,"agent":agent}));
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
        if !credential_ready(&installed, &provider) {
            return Err(rt_error(
                ErrorCode::Unauthenticated,
                "provider_credential_missing",
            ));
        }
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
        let service_path = directory.join("roundtable.sock");
        let gateway_path = directory.join("gateway.sock");
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
            .with_clock(clock),
        );
        let token = Arc::new(authority.issue());
        let provider_token = uuid::Uuid::new_v4().to_string();
        let store_clock = request.store.clone();
        let gateway = Arc::new(LiveModelGateway::new(
            self.data_dir.clone(),
            (
                super::ApprovedOrigin::parse(&provider.origin)?,
                super::HostCredential::injected(
                    std::env::var(&provider.credential_env).unwrap_or_default(),
                ),
                provider.model.clone(),
                request.participant.effort.clone(),
            ),
            provider_token.clone(),
            (execution, facts),
            MonoMs(request.deadline_mono),
            Arc::new(move || MonoMs(store_clock.clock_sample().0)),
            installed.context_profile.clone(),
        )?);
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
            assistant: Mutex::new(Some(super::DiagnosticCapture::new(auth_denylist(
                &installed,
                &[
                    provider_token.clone(),
                    token.reveal_for_same_sandbox().to_owned(),
                    std::env::var(&provider.credential_env).unwrap_or_default(),
                ],
            )))),
            finish_reason: Mutex::new(None),
            agent: agent.clone(),
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
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or_else(|| rt_error(ErrorCode::PolicyUnenforceable, "host_home"))?
            .canonicalize()
            .map_err(|_| rt_error(ErrorCode::PolicyUnenforceable, "host_home"))?;
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
            env_allowlist: [
                (
                    "OPENAI_BASE_URL".to_owned(),
                    super::relay::SANDBOX_ENDPOINT.to_owned(),
                ),
                ("OPENAI_API_KEY".to_owned(), provider_token),
            ]
            .into_iter()
            .collect(),
            global_mcp: false,
        };
        let plan = build_qualified_sandbox_plan(&input, &oci)?;
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
        *active.instance.lock().expect("runtime instance") = Some(instance.clone());
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| rt_error(ErrorCode::RuntimeUnavailable, "acp_stdin"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| rt_error(ErrorCode::RuntimeUnavailable, "acp_stdout"))?;
        if let Some(mut stderr) = child.stderr.take() {
            *active.stderr.lock().await = Some(tokio::spawn(async move {
                let mut buf = [0u8; 4096];
                while stderr.read(&mut buf).await.is_ok_and(|count| count > 0) {}
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
            stdin,
            stdout,
            &request,
            &mcp.absolute_path,
            token.reveal_for_same_sandbox(),
            &active,
            &self.registry,
            agent_type_for(&agent)?,
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
        let cleanup = self.reap_active(request.fence.incarnation).await?;
        let watermark = result?;
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
    mut stdin: tokio::process::ChildStdin,
    stdout: tokio::process::ChildStdout,
    request: &RoundtableTurnRequest,
    mcp: &str,
    token: &str,
    active: &Active,
    registry: &tokio::sync::Mutex<Option<super::RoundtableSessionRegistry>>,
    agent: crate::models::AgentType,
) -> RtResult<u64> {
    let mut stdout = BufReader::new(stdout);
    let mut seq = 0;
    let initialize=rpc(&mut stdin,&mut stdout,1,"initialize",json!({"protocolVersion":1,"clientCapabilities":{"fs":{"readTextFile":false,"writeTextFile":false},"terminal":false},"clientInfo":{"name":"codeg-roundtable","version":"1"}}),&mut seq,active).await?;
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
    let session=rpc(&mut stdin,&mut stdout,2,"session/new",json!({"cwd":"/scratch","mcpServers":[{"name":"roundtable","command":mcp,"args":["--service-roundtable","--socket-path","/run/codeg/roundtable.sock","--incarnation",request.fence.incarnation.to_string()],"env":[{"name":super::ATTEMPT_TOKEN_ENV,"value":token},{"name":"CODEG_RT_MODEL_SOCKET","value":"/run/codeg/gateway.sock"}]}]}),&mut seq,active).await?;
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
    let result=rpc(&mut stdin,&mut stdout,5,"session/prompt",json!({"sessionId":session_id,"prompt":[{"type":"text","text":std::str::from_utf8(&request.prompt).map_err(|_|rt_error(ErrorCode::InvalidArgument,"prompt_encoding"))?}]}),&mut seq,active).await?;
    if result["stopReason"] != "end_turn" {
        return Err(rt_error(
            ErrorCode::RuntimeUnavailable,
            "acp_abnormal_finish",
        ));
    }
    Ok(seq)
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
    write_rpc(
        stdin,
        &json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}),
    )
    .await?;
    loop {
        let mut line = Vec::new();
        let count = (&mut *stdout)
            .take(1_048_577)
            .read_until(b'\n', &mut line)
            .await
            .map_err(|_| rt_error(ErrorCode::RuntimeUnavailable, "acp_read"))?;
        if count == 0 || line.len() > 1_048_576 {
            return Err(rt_error(ErrorCode::RuntimeUnavailable, "acp_frame"));
        }
        let message = parse_strict_json(&line, &ParseLimits::suggested_profile())?;
        *seq = seq
            .checked_add(1)
            .ok_or_else(|| rt_error(ErrorCode::InvalidState, "ingress_overflow"))?;
        if message.get("method").is_some() {
            if let Some(request_id) = message.get("id") {
                let response = if message["method"] == "session/request_permission" {
                    json!({"jsonrpc":"2.0","id":request_id,"result":{"outcome":{"outcome":"cancelled"}}})
                } else {
                    json!({"jsonrpc":"2.0","id":request_id,"error":{"code":-32601,"message":"Capability not available"}})
                };
                write_rpc(stdin, &response).await?;
            } else if message["method"] == "session/update"
                && message["params"]["update"]["sessionUpdate"] == "agent_message_chunk"
            {
                if let Some(text) = message["params"]["update"]["content"]["text"].as_str() {
                    if let Some(capture) = active
                        .assistant
                        .lock()
                        .expect("assistant diagnostic")
                        .as_mut()
                    {
                        capture.push(text);
                    }
                }
            }
            continue;
        }
        if message["id"] == id {
            if message.get("error").is_some() {
                return Err(rt_error(ErrorCode::RuntimeUnavailable, "acp_rejected"));
            }
            return message
                .get("result")
                .cloned()
                .ok_or_else(|| rt_error(ErrorCode::RuntimeUnavailable, "acp_response"));
        }
    }
}
async fn write_rpc(stdin: &mut tokio::process::ChildStdin, value: &Value) -> RtResult<()> {
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
async fn persist_diagnostic(active: &Active) -> RtResult<()> {
    let reason = active
        .finish_reason
        .lock()
        .expect("finish reason")
        .clone()
        .unwrap_or_else(|| "cancelled".into());
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
    unqualified_live_fixture(root, None)
}
#[cfg(any(test, feature = "test-utils"))]
pub fn retired_live_executor_fixture(
    root: PathBuf,
    incarnation: IncarnationId,
) -> RtResult<Arc<dyn RoundtableTurnExecutor>> {
    unqualified_live_fixture(root, Some(incarnation))
}
#[cfg(any(test, feature = "test-utils"))]
fn unqualified_live_fixture(
    root: PathBuf,
    retired: Option<IncarnationId>,
) -> RtResult<Arc<dyn RoundtableTurnExecutor>> {
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
