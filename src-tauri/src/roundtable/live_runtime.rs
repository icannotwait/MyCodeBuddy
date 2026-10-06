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
use std::cell::Cell;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
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
                self.retire_incarnation(incarnation)?;
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
            self.retire_incarnation(incarnation)?;
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
        self.retire_incarnation(incarnation)?;
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
    let prompt = std::str::from_utf8(&request.prompt)
        .map_err(|_| rt_error(ErrorCode::InvalidArgument, "prompt_encoding"))?;
    finish_seat_prompt(
        &mut stdin,
        &mut stdout,
        session_id,
        prompt,
        &mut seq,
        Some(&active.assistant),
        None,
    )
    .await?;
    Ok(seq)
}
/// ACP client parameters shared by live seats and the qualification probe.
/// Terminal and filesystem edit/read capabilities stay off. `session/new`
/// has no capabilities field in ACP, so that request omits them.
pub(crate) fn roundtable_initialize_params(client_name: &str) -> Value {
    json!({
        "protocolVersion": 1,
        "clientCapabilities": {
            "fs": {"readTextFile": false, "writeTextFile": false},
            "terminal": false
        },
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
    params
}

/// Native Grok tools. Copied from the hidden-generation catalog minus the MCP
/// meta-tools `search_tool` and `use_tool`. CLI `--disallowed-tools` is
/// headless-only and does not apply to `grok agent stdio`.
const GROK_ROUNDTABLE_DISALLOWED_TOOLS: &[&str] = &[
    "run_terminal_cmd",
    "run_terminal_command",
    "read_file",
    "search_replace",
    "write",
    "grep",
    "list_dir",
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

async fn finish_seat_prompt<W, R>(
    stdin: &mut W,
    stdout: &mut BufReader<R>,
    session_id: &str,
    prompt: &str,
    seq: &mut u64,
    assistant: Option<&Mutex<Option<super::DiagnosticCapture>>>,
    deadline: Option<Duration>,
) -> RtResult<Value>
where
    W: AsyncWrite + Unpin,
    R: AsyncRead + Unpin,
{
    let rejected = Cell::new(false);
    let mut prompt_id = 5u64;
    let mut text = prompt.to_owned();
    let mut repairs = 0u8;
    loop {
        rejected.set(false);
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
                assistant,
                deadline,
                rejected_permission: &rejected,
            },
        )
        .await?;
        if result["stopReason"] == "end_turn" {
            return Ok(result);
        }
        if result["stopReason"] == "cancelled"
            && rejected.get()
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
    pub(crate) rejected_permission: &'a Cell<bool>,
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
    let rejected = Cell::new(false);
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
        },
    )
    .await
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
        *exchange.seq = exchange
            .seq
            .checked_add(1)
            .ok_or_else(|| rt_error(ErrorCode::InvalidState, "ingress_overflow"))?;
        if message.get("method").is_some() {
            if let Some(request_id) = message.get("id").cloned() {
                let response = if message["method"] == "session/request_permission" {
                    if !tool_call_is_roundtable(&message["params"]) {
                        exchange.rejected_permission.set(true);
                    }
                    permission_reply(&message["params"], &request_id)
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

fn permission_reply(params: &Value, request_id: &Value) -> Value {
    let allow = tool_call_is_roundtable(params);
    match selected_option(params, allow) {
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
        &["allow_once", "allow_always"]
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

/// Structured identity only. A free-text command such as `echo submit_result`
/// does not match. Grok's `use_tool` wrapper matches `rawInput.tool_name`
/// when it is exactly `roundtable__<tool>`.
fn tool_call_is_roundtable(params: &Value) -> bool {
    let call = params.get("toolCall").cloned().unwrap_or(Value::Null);
    exact_tool_field(call.get("title"))
        || exact_tool_field(call.get("name"))
        || exact_tool_field(call.get("kind"))
        || raw_input_names_tool(call.get("rawInput"))
}

fn exact_tool_field(value: Option<&Value>) -> bool {
    value
        .and_then(Value::as_str)
        .is_some_and(exact_roundtable_tool)
}

fn raw_input_names_tool(value: Option<&Value>) -> bool {
    let Some(Value::Object(map)) = value else {
        return false;
    };
    ["name", "tool", "toolName", "tool_name"].iter().any(|key| {
        map.get(*key)
            .and_then(Value::as_str)
            .is_some_and(exact_roundtable_tool)
    })
}

fn exact_roundtable_tool(token: &str) -> bool {
    let name = token
        .strip_prefix("mcp__roundtable__")
        .or_else(|| token.strip_prefix("roundtable__"))
        .or_else(|| token.strip_prefix("roundtable/"))
        .unwrap_or(token);
    ROUNDTABLE_TOOL_NAMES.contains(&name)
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

/// Newest run directories kept under `oci/runs/` after an attempt is reaped.
const RUN_DIR_RETENTION: usize = 8;

fn retire_attempt_files(runtime_root: &Path, incarnation: &str) -> RtResult<()> {
    let Some(runs) = anchored_runs(runtime_root)? else {
        return Ok(());
    };
    if !incarnation_name_is_safe(incarnation) {
        return Err(rt_error(ErrorCode::StorageUnavailable, "runtime_directory"));
    }
    delete_attempt_scratch(&runs, &runs.join(incarnation))?;
    prune_run_dirs(&runs, RUN_DIR_RETENTION, incarnation);
    Ok(())
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
        if canonical.file_name().and_then(|name| name.to_str()) == Some(protect) {
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
        if let Err(error) = std::fs::remove_dir_all(&path) {
            tracing::warn!(
                path = %path.display(),
                %error,
                "roundtable run retention"
            );
        }
    }
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
    let rejected = Cell::new(false);
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
    permission_reply(&params, &json!(7))
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
    let rejected = Cell::new(false);
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
            },
        )
        .await?;
        finish_seat_prompt(
            &mut client_write,
            &mut client_read,
            "s",
            "submit the proposal",
            &mut seq,
            Some(&capture),
            Some(Duration::from_secs(5)),
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
