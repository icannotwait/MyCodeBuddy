//! Authenticated commands share persistence and admission across transports.

use std::sync::Arc;

use roundtable_protocol::{
    canonical_bytes, decode_json, validate_config, ActorContext, AttachRequest, CloneRequest,
    ControlKind, CreateRequest, DetachRequest, Epoch, ErrorCode, EventsRequest, EvidenceRequest,
    GetReadV1, GetRequest, InterjectMode, InterjectRequest, ListRequest, MessagesRequest,
    MutationAck, ObjectKind, ObjectRefV1, OperationRequest, ParseLimits, PauseRequest,
    PreflightRequest, PrincipalId, RequestId, ResourceLimits, ResumeRequest, RetrySynthesisRequest,
    Revision, RoomId, RoomState, RoundtableConfigV1, RtResult, Seq, StartRequest, StopRequest,
    UpdateDraftRequest,
};
use sea_orm::{DatabaseTransaction, TransactionTrait};
use serde::de::DeserializeOwned;
use serde_json::{json, Value};
use uuid::Uuid;

use super::control::ControlRequest;
use super::store::{column, exec, num, optional_row, rows, storage_err, text, RoundtableStore};
use super::{rt_error, RoundtableService, ServiceReadiness};

#[derive(Clone)]
pub(crate) struct ConfirmedPreflight {
    principal: PrincipalId,
    room: RoomId,
    revision: Revision,
    config_hash: String,
    capability_hash: String,
    source_hash: String,
    expires_ms: u64,
}

impl RoundtableService {
    #[cfg(any(test, feature = "test-utils"))]
    pub async fn record_run_failure_for_test(
        &self,
        room: RoomId,
        run_epoch: u64,
        error: &roundtable_protocol::RtError,
    ) -> RtResult<()> {
        fail_run(
            &self.command_store()?,
            room,
            self.boot_epoch(),
            run_epoch,
            error,
        )
        .await
        .map(|_| ())
    }

    pub async fn execute_command(
        self: &Arc<Self>,
        actor: &ActorContext,
        command: &str,
        body: Value,
    ) -> RtResult<Value> {
        if super::conclusion::CONCLUSION_COMMANDS.contains(&command) {
            return self.execute_conclusion(actor, command, body).await;
        }
        if command == super::agent_credentials::AGENTS_COMMAND {
            // Read-only: agent settings presence, installed versions and the
            // qualified catalog. No room, no mutation, no setting values.
            if !body.is_object() || body.get("principal_id").is_some() {
                return Err(rt_error(ErrorCode::InvalidArgument, "principal"));
            }
            let _ = actor;
            let data_dir = self.data_dir.clone();
            let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
            return tokio::task::spawn_blocking(move || {
                super::agent_credentials::roundtable_agents_status(&data_dir, home.as_deref())
            })
            .await
            .map_err(|_| rt_error(ErrorCode::RuntimeUnavailable, "agents_status_worker"));
        }
        let room = body
            .get("room_id")
            .and_then(Value::as_str)
            .and_then(|id| id.parse::<RoomId>().ok());
        let result = self.execute_scoped(actor, command, body, false).await;
        if matches!(
            command,
            "roundtable_start"
                | "roundtable_resume"
                | "roundtable_interject"
                | "roundtable_retry_synthesis"
        ) {
            let _gate = self.command_gate.lock().await;
            if self.writable() && !self.draining() {
                if let (Some(room), Err(error), Ok(store)) = (room, &result, self.command_store()) {
                    if authorize(&store, actor, &room).await.is_ok() {
                        if let Err(metric_error) = record_rejection(&store, room, error).await {
                            tracing::warn!(reason=?metric_error.details.reason,"roundtable rejection measurement unavailable");
                        }
                    }
                }
            }
        }
        result
    }

    #[cfg(any(test, feature = "test-utils"))]
    pub async fn execute_fake_command(
        self: &Arc<Self>,
        actor: &ActorContext,
        command: &str,
        body: Value,
    ) -> RtResult<Value> {
        self.execute_scoped(actor, command, body, true).await
    }

    async fn execute_scoped(
        self: &Arc<Self>,
        actor: &ActorContext,
        command: &str,
        body: Value,
        fake: bool,
    ) -> RtResult<Value> {
        if !body.is_object() || body.get("principal_id").is_some() {
            return Err(rt_error(ErrorCode::InvalidArgument, "principal"));
        }
        if !super::api::COMMANDS.contains(&command) {
            return Err(rt_error(ErrorCode::InvalidArgument, "command"));
        }
        let body = normalize(command, &body)?;
        let store = self.command_store()?;
        // ponytail: serialize command admission globally; per-room gates can
        // replace this if command throughput becomes a bottleneck.
        let _gate = self.command_gate.lock().await;
        if matches!(
            command,
            "roundtable_create"
                | "roundtable_update_draft"
                | "roundtable_start"
                | "roundtable_resume"
                | "roundtable_clone"
        ) || (command == "roundtable_interject" && body["mode"] == "next_phase")
        {
            let request: RequestId = decode(
                body.get("request_id")
                    .ok_or_else(|| rt_error(ErrorCode::InvalidArgument, "request_id"))?,
            )?;
            let txn = store.connection().begin().await.map_err(storage_err)?;
            let replay = saved(&txn, actor, command, request, &body).await?;
            txn.rollback().await.map_err(storage_err)?;
            if let Some(replay) = replay {
                return Ok(replay);
            }
        }
        match command {
            "roundtable_preflight" => {
                let request: PreflightRequest = decode(&body)?;
                validate_config(&request.config, &ResourceLimits::suggested_profile())?;
                let current = match request.room_id {
                    Some(room) => {
                        let current = authorized_room(&store, actor, &room).await?;
                        check_revision(
                            &current,
                            request
                                .revision
                                .ok_or_else(|| rt_error(ErrorCode::InvalidArgument, "revision"))?,
                        )?;
                        let mut prospective: RoundtableConfigV1 = decode(&current.config)?;
                        // Resume may change concurrency only. Bind the exact prospective
                        // config to this paused revision; all other edits still conflict.
                        if current.status == "paused" {
                            prospective.concurrency = request.config.concurrency;
                        }
                        if config_hash(&request.config)? != config_hash(&prospective)? {
                            return Err(rt_error(ErrorCode::RevisionConflict, "config_changed"));
                        }
                        Some((room, current))
                    }
                    None => {
                        if request.revision.is_some() {
                            return Err(rt_error(ErrorCode::InvalidArgument, "room_revision"));
                        }
                        None
                    }
                };
                let sources =
                    frozen_sources(&store, actor, request.room_id, &request.config).await?;
                if let Some(room) = request.room_id {
                    check_interjection_context(store.connection(), room, &request.config, None)
                        .await?;
                }
                let runtime = self.participant_runtime().preflight(&request.config).await;
                let mut confirmation = None;
                if let (Some((room, current)), Ok(capability)) = (&current, &runtime) {
                    if fake
                        || (self.execution_gate.enabled()
                            && matches!(self.readiness(), ServiceReadiness::Ready))
                    {
                        let now = store.clock_sample().0;
                        let id = Uuid::new_v4().to_string();
                        let record = ConfirmedPreflight {
                            principal: actor.principal_id(),
                            room: *room,
                            revision: Revision(current.revision as u64),
                            config_hash: config_hash(&request.config)?,
                            capability_hash: roundtable_protocol::canonical_hash(capability)?
                                .to_hex(),
                            source_hash: roundtable_protocol::canonical_hash(&sources)?.to_hex(),
                            expires_ms: now.saturating_add(15 * 60 * 1000),
                        };
                        let mut records = self.preflights.lock().expect("preflights");
                        records.retain(|_, record| record.expires_ms > now);
                        if records.len() >= 1000 {
                            return Err(rt_error(ErrorCode::CapacityLimited, "preflight_capacity"));
                        }
                        records.insert(id.clone(), record);
                        confirmation = Some(id);
                    }
                }
                Ok(json!({
                    "config_hash": config_hash(&request.config)?,
                    "confirmed_preflight_id": confirmation,
                    "source_manifests":sources,
                    "enabled": self.execution_gate.enabled(),
                    "readiness": format!("{:?}", self.readiness()).to_lowercase(),
                    "capability": runtime.as_ref().ok(),
                    "error": runtime.err(),
                    "tools": super::service_tool_names(),
                    "network": "model_gateway_only", "writes":"scratch_only",
                    "workspace_mount": workspace_mount_status(&store, &self.data_dir, &request.config).await,
                }))
            }
            "roundtable_create" => {
                let request: CreateRequest = decode(&body)?;
                self.require_mutation(fake, true)?;
                create(
                    &store,
                    actor,
                    command,
                    &body,
                    (request.request_id, request.config),
                    self.boot_epoch(),
                    Some((&self.data_dir, &request.selected_source_paths)),
                )
                .await
            }
            "roundtable_update_draft" => {
                let request: UpdateDraftRequest = decode(&body)?;
                self.require_mutation(fake, false)?;
                update(&store, actor, command, &body, request).await
            }
            "roundtable_get" => {
                let request: GetRequest = decode(&body)?;
                authorize(&store, actor, &request.room_id).await?;
                match request.read {
                    None
                    | Some(GetReadV1::Projection {
                        projection_id: None,
                    }) => room_view(&store, &request.room_id).await,
                    Some(GetReadV1::Projection {
                        projection_id: Some(id),
                    }) => {
                        let projection = store.projection(&request.room_id, Some(&id)).await?;
                        Ok(
                            json!({"high_water_seq":projection.body.last_seq,"projection":projection,"message_manifest_id":id}),
                        )
                    }
                    Some(GetReadV1::Metrics {}) => {
                        metrics_view(self, &store, &request.room_id).await
                    }
                    Some(GetReadV1::Usage {
                        after_usage_version,
                    }) => usage_view(&store, &request.room_id, after_usage_version).await,
                    Some(GetReadV1::Object { object_ref, cursor }) => {
                        object_view(self, &store, actor, &request.room_id, object_ref, cursor).await
                    }
                }
            }
            "roundtable_list" => {
                let request: ListRequest = decode(&body)?;
                list(&store, actor, request).await
            }
            "roundtable_events" => {
                let request: EventsRequest = decode(&body)?;
                authorize(&store, actor, &request.room_id).await?;
                event_page(self, &store, actor, request).await
            }
            "roundtable_messages" => {
                let request: MessagesRequest = decode(&body)?;
                authorize(&store, actor, &request.room_id).await?;
                message_page(self, &store, actor, request).await
            }
            "roundtable_evidence" => {
                let request: EvidenceRequest = decode(&body)?;
                authorize(&store, actor, &request.room_id).await?;
                let row = optional_row(store.connection(), "SELECT body_json,content_hash FROM rt_evidence WHERE room_id=? AND evidence_id=? AND publish_seq IS NOT NULL", vec![text(&request.room_id.to_string()), text(&request.evidence_id.to_string())]).await?
                    .ok_or_else(|| rt_error(ErrorCode::Forbidden,"not_found"))?;
                Ok(
                    json!({"body":json_text(&column::<String>(&row,0)?)?,"hash":column::<String>(&row,1)?}),
                )
            }
            "roundtable_operation" => {
                let request: OperationRequest = decode(&body)?;
                authorize(&store, actor, &request.room_id).await?;
                let row = optional_row(store.connection(), "SELECT kind,step,status,blocked_reason,successor_phase_id FROM rt_control_operations WHERE room_id=? AND operation_id=?", vec![text(&request.room_id.to_string()),text(&request.operation_id.to_string())]).await?
                    .ok_or_else(||rt_error(ErrorCode::Forbidden,"not_found"))?;
                Ok(
                    json!({"operation_id":request.operation_id,"kind":column::<String>(&row,0)?,"step":column::<String>(&row,1)?,"status":column::<String>(&row,2)?,"blocked_reason":column::<Option<String>>(&row,3)?,"successor_phase_id":column::<Option<String>>(&row,4)?}),
                )
            }
            "roundtable_clone" => {
                let request: CloneRequest = decode(&body)?;
                self.require_mutation(fake, true)?;
                let source = authorized_room(&store, actor, &request.room_id).await?;
                check_revision(&source, request.expected_revision)?;
                let config = match request.config_override {
                    Some(config) => config,
                    None => decode(&source.config)?,
                };
                create(
                    &store,
                    actor,
                    command,
                    &body,
                    (request.request_id, config),
                    self.boot_epoch(),
                    None,
                )
                .await
            }
            "roundtable_attach" => {
                let request: AttachRequest = decode(&body)?;
                if request.protocol_version != 1 {
                    return Err(rt_error(ErrorCode::InvalidArgument, "protocol_version"));
                }
                authorize(&store, actor, &request.room_id).await?;
                let projection = store.projection(&request.room_id, None).await?;
                if request
                    .since_seq
                    .is_some_and(|seq| seq.0 > projection.body.last_seq.0)
                {
                    return Err(rt_error(
                        ErrorCode::InvalidArgument,
                        "subscription_watermark",
                    ));
                }
                if let Some(hash) = request.projection_hash {
                    if request.since_seq == Some(projection.body.last_seq)
                        && hash != projection.projection_ref.hash.to_hex()
                    {
                        return Err(rt_error(ErrorCode::InvalidArgument, "projection_hash"));
                    }
                }
                Ok(
                    json!({"subscription_id":request.subscription_id,"message_manifest_id":projection.projection_ref.id,"high_water_seq":projection.body.last_seq,"projection":projection}),
                )
            }
            "roundtable_detach" => {
                let request: DetachRequest = decode(&body)?;
                authorize(&store, actor, &request.room_id).await?;
                Ok(json!({"detached":true}))
            }
            "roundtable_pause"
            | "roundtable_stop"
            | "roundtable_interject"
            | "roundtable_retry_synthesis" => {
                if command == "roundtable_interject" {
                    let request: InterjectRequest = decode(&body)?;
                    if request.mode == InterjectMode::NextPhase {
                        self.require_mutation(fake, false)?;
                        return next_input(&store, actor, command, &body, request).await;
                    }
                }
                let request = control_request(command, &body)?;
                if let Some(ack) = store.replay_control(actor, &request).await? {
                    return Ok(json!(ack));
                }
                self.require_mutation(fake, false)?;
                let mut confirmed = None;
                if matches!(
                    request.kind,
                    ControlKind::RestartCurrent | ControlKind::RetrySynthesis
                ) {
                    self.require_mutation(fake, true)?;
                    self.require_capacity(&store, request.room_id, true).await?;
                    let current = authorized_room(&store, actor, &request.room_id).await?;
                    let config = decode(&current.config)?;
                    let proposed = if request.kind == ControlKind::RestartCurrent {
                        request
                            .input
                            .get("text")
                            .and_then(Value::as_str)
                            .map(|text| (request.request_id.to_string(), text))
                    } else {
                        None
                    };
                    check_interjection_context(
                        store.connection(),
                        request.room_id,
                        &config,
                        proposed,
                    )
                    .await?;
                    let capability = self.participant_runtime().preflight(&config).await?;
                    let id = body.get("confirmed_preflight_id").and_then(Value::as_str);
                    if !fake || id.is_some() {
                        confirmed = Some(
                            self.confirmed_inputs(
                                &store,
                                actor,
                                (request.room_id, Revision(current.revision as u64)),
                                &config,
                                &capability,
                                id,
                            )
                            .await?,
                        );
                    }
                }
                let ack = store.request_control(actor, &request).await?;
                if let Some(operation) = ack.operation_id {
                    let active = optional_row(
                        store.connection(),
                        "SELECT active_control_id FROM rt_rooms WHERE room_id=?",
                        vec![text(&request.room_id.to_string())],
                    )
                    .await?
                    .ok_or_else(|| rt_error(ErrorCode::Forbidden, "not_found"))?;
                    if column::<Option<String>>(&active, 0)?.as_deref()
                        != Some(&operation.to_string())
                    {
                        return Ok(json!(ack));
                    }
                    self.stop_room_task(request.room_id).await;
                    // Local revocation never waits for a successful database write.
                    if let Err(error) = self
                        .participant_runtime()
                        .cleanup_local_room(request.room_id)
                        .await
                    {
                        tracing::warn!(room=%request.room_id, reason=?error.details.reason, "roundtable local control cleanup pending");
                    }
                    // The store itself verifies whether real cleanup is necessary.
                    // A pending process leaves a durable operation for supervision.
                    match store
                        .advance_durable_control(actor, request.room_id, operation, false)
                        .await
                    {
                        Ok(_) => {
                            self.continue_control(
                                &store,
                                actor,
                                request.room_id,
                                operation,
                                fake,
                                confirmed.clone(),
                            )
                            .await?;
                            Ok(json!(ack))
                        }
                        Err(error)
                            if error.details.reason.as_deref() == Some("cleanup_incomplete") =>
                        {
                            let key = operation.to_string();
                            let mut tasks = self.control_tasks.lock().expect("control tasks");
                            tasks.retain(|_, task| !task.is_finished());
                            if !tasks.contains_key(&key) {
                                let service = Arc::clone(self);
                                let actor = actor.clone();
                                let room = request.room_id;
                                tasks.insert(key,tokio::spawn(async move {
                                    if let Err(error)=service.cleanup_room(room).await {
                                        let _gate=service.command_gate.lock().await;
                                        if !service.draining() {
                                            if let Ok(store)=service.command_store() {
                                                if let Err(error)=block_control_failure(&store,room,operation,&error).await {tracing::warn!(room=%room,reason=?error.details.reason,"roundtable blocked control persistence failed");}
                                            }
                                        }
                                        return;
                                    }
                                    let _gate=service.command_gate.lock().await;
                                    if !service.draining() {
                                        match service.command_store() {
                                            Ok(store)=>{
                                                let result=async {
                                                    store.advance_durable_control(&actor,room,operation,false).await?;
                                                    service.continue_control(&store,&actor,room,operation,fake,confirmed.clone()).await
                                                }.await;
                                                if let Err(error)=result {
                                                    if let Err(error)=block_control_failure(&store,room,operation,&error).await {tracing::warn!(room=%room,reason=?error.details.reason,"roundtable blocked control persistence failed");}
                                                }
                                            },
                                            Err(error)=>tracing::warn!(reason=?error.details.reason,"roundtable control store unavailable"),
                                        }
                                    }
                                }));
                            }
                            Ok(json!(ack))
                        }
                        Err(error) => {
                            block_control_failure(&store, request.room_id, operation, &error)
                                .await?;
                            Ok(json!(ack))
                        }
                    }
                } else {
                    Ok(json!(ack))
                }
            }
            "roundtable_start" | "roundtable_resume" => {
                self.require_mutation(fake, true)?;
                {
                    let mut tasks = self.room_tasks.lock().expect("room tasks");
                    tasks.retain(|_, task| !task.is_finished());
                    if !tasks.is_empty() {
                        return Err(rt_error(ErrorCode::CapacityLimited, "active_room"));
                    }
                }
                let (room, request_id, revision, confirmation, concurrency) =
                    if command == "roundtable_start" {
                        let request: StartRequest = decode(&body)?;
                        (
                            request.room_id,
                            request.request_id,
                            request.expected_revision,
                            request.confirmed_preflight_id,
                            None,
                        )
                    } else {
                        let request: ResumeRequest = decode(&body)?;
                        if !request.recovery_consent {
                            return Err(rt_error(ErrorCode::InvalidArgument, "recovery_consent"));
                        }
                        (
                            request.room_id,
                            request.request_id,
                            request.expected_revision,
                            request.confirmed_preflight_id,
                            request.concurrency,
                        )
                    };
                let current = authorized_room(&store, actor, &room).await?;
                check_revision(&current, revision)?;
                self.require_capacity(&store, room, false).await?;
                let mut config: RoundtableConfigV1 = decode(&current.config)?;
                if let Some(concurrency) = concurrency {
                    config.concurrency = concurrency;
                    validate_config(&config, &ResourceLimits::suggested_profile())?;
                }
                if command == "roundtable_resume" && current.status != "paused" {
                    return Err(rt_error(ErrorCode::InvalidState, "paused_required"));
                }
                if command == "roundtable_start"
                    && !matches!(current.status.as_str(), "draft" | "ready")
                {
                    return Err(rt_error(ErrorCode::InvalidState, "draft_required"));
                }
                check_interjection_context(store.connection(), room, &config, None).await?;
                let capability = self.participant_runtime().preflight(&config).await?;
                if command == "roundtable_start" || !fake || confirmation.is_some() {
                    self.confirmed_inputs(
                        &store,
                        actor,
                        (room, revision),
                        &config,
                        &capability,
                        confirmation.as_deref(),
                    )
                    .await?;
                }
                let ack = start(
                    &store,
                    actor,
                    command,
                    &body,
                    (room, request_id, revision),
                    self.boot_epoch(),
                    &config,
                )
                .await?;
                let committed: MutationAck = decode(&ack)?;
                self.spawn_room(store, room, config, committed.run_epoch.0);
                Ok(ack)
            }
            _ => Err(rt_error(ErrorCode::InvalidArgument, "command")),
        }
    }

    fn spawn_room(
        self: &Arc<Self>,
        store: RoundtableStore,
        room: RoomId,
        config: RoundtableConfigV1,
        epoch: u64,
    ) {
        let boot = self.boot_epoch();
        let service = Arc::clone(self);
        let task = tokio::spawn(async move {
            if let Err(error) = service
                .participant_runtime()
                .run_room(store.clone(), room, config)
                .await
            {
                loop {
                    // Always revoke/reap known owners first, including when the
                    // same storage outage also prevents fail_run from committing.
                    if let Err(cleanup) =
                        service.participant_runtime().cleanup_local_room(room).await
                    {
                        tracing::warn!(%room, reason=?cleanup.details.reason, "roundtable local failure cleanup pending");
                    }
                    let persisted = {
                        let _gate = service.command_gate.lock().await;
                        match fail_run(&store, room, boot, epoch, &error).await {
                            Ok(_) => service.cleanup_room(room).await,
                            Err(error) => Err(error),
                        }
                    };
                    match persisted {
                        Ok(()) => break,
                        Err(error)
                            if error.code == ErrorCode::StorageUnavailable
                                && !service.draining() =>
                        {
                            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
                        }
                        Err(error) => {
                            tracing::warn!(%room, reason=?error.details.reason, "roundtable failure cleanup blocked");
                            break;
                        }
                    }
                }
            }
        });
        let mut tasks = self.room_tasks.lock().expect("room tasks");
        tasks.retain(|_, task| !task.is_finished());
        if let Some(old) = tasks.insert(room.to_string(), task) {
            old.abort();
        }
    }

    async fn require_capacity(
        &self,
        store: &RoundtableStore,
        room: RoomId,
        allow_current: bool,
    ) -> RtResult<()> {
        let occupied = super::store::query_i64(store.connection(),"SELECT COUNT(*) FROM rt_rooms r WHERE (?=0 OR r.room_id<>?) AND (r.status IN('running','pausing','stopping','recovering') OR r.active_control_id IS NOT NULL OR EXISTS(SELECT 1 FROM rt_attempts a WHERE a.room_id=r.room_id AND a.cleanup_state<>'confirmed') OR EXISTS(SELECT 1 FROM rt_bindings b JOIN rt_launch_intents l ON l.incarnation=b.incarnation WHERE b.room_id=r.room_id AND l.reaped=0))",vec![num(i64::from(allow_current)),text(&room.to_string())]).await?;
        if occupied != 0 {
            return Err(rt_error(ErrorCode::CapacityLimited, "active_room_capacity"));
        }
        Ok(())
    }

    async fn confirmed_inputs(
        &self,
        store: &RoundtableStore,
        actor: &ActorContext,
        (room, revision): (RoomId, Revision),
        config: &RoundtableConfigV1,
        capability: &Value,
        id: Option<&str>,
    ) -> RtResult<ConfirmedPreflight> {
        let record = id
            .and_then(|id| self.preflights.lock().expect("preflights").get(id).cloned())
            .ok_or_else(|| rt_error(ErrorCode::InvalidArgument, "preflight_confirmation"))?;
        if record.principal != actor.principal_id()
            || record.room != room
            || record.revision != revision
        {
            return Err(rt_error(
                ErrorCode::InvalidArgument,
                "preflight_confirmation",
            ));
        }
        self.verify_confirmed_inputs(store, actor, config, capability, &record)
            .await?;
        Ok(record)
    }

    async fn verify_confirmed_inputs(
        &self,
        store: &RoundtableStore,
        actor: &ActorContext,
        config: &RoundtableConfigV1,
        capability: &Value,
        record: &ConfirmedPreflight,
    ) -> RtResult<()> {
        let sources = frozen_sources(store, actor, Some(record.room), config).await?;
        if record.config_hash != config_hash(config)?
            || record.expires_ms <= store.clock_sample().0
            || record.capability_hash != roundtable_protocol::canonical_hash(capability)?.to_hex()
            || record.source_hash != roundtable_protocol::canonical_hash(&sources)?.to_hex()
        {
            return Err(rt_error(
                ErrorCode::InvalidArgument,
                "preflight_confirmation",
            ));
        }
        Ok(())
    }

    /// Explicit restart/retry completes cleanup before dispatching its successor.
    /// Startup recovery uses only the store and never calls this paid path.
    async fn continue_control(
        self: &Arc<Self>,
        store: &RoundtableStore,
        actor: &ActorContext,
        room: RoomId,
        operation: roundtable_protocol::OperationId,
        fake: bool,
        confirmed: Option<ConfirmedPreflight>,
    ) -> RtResult<()> {
        let op = optional_row(store.connection(),"SELECT o.kind,o.status,o.requested_epoch,o.successor_phase_id,r.run_epoch,r.status,r.current_phase_id,r.active_control_id FROM rt_control_operations o JOIN rt_rooms r ON r.room_id=o.room_id WHERE o.room_id=? AND o.operation_id=?",vec![text(&room.to_string()),text(&operation.to_string())]).await?.ok_or_else(||rt_error(ErrorCode::InvalidState,"control_missing"))?;
        let kind: String = column(&op, 0)?;
        if !matches!(kind.as_str(), "restart_current" | "retry_synthesis")
            || column::<String>(&op, 1)? != "completed"
            || column::<i64>(&op, 2)? != column::<i64>(&op, 4)?
            || column::<String>(&op, 5)? != "paused"
            || column::<Option<String>>(&op, 3)?.is_none()
            || column::<Option<String>>(&op, 3)? != column::<Option<String>>(&op, 6)?
            || column::<Option<String>>(&op, 7)?.is_some()
        {
            return Ok(());
        }
        let result=async {
            self.require_mutation(fake,true)?;
            self.require_capacity(store,room,true).await?;
            let config:RoundtableConfigV1=decode(&authorized_room(store,actor,&room).await?.config)?;
            check_interjection_context(store.connection(), room, &config, None).await?;
            let capability = self.participant_runtime().preflight(&config).await?;
            if let Some(record) = &confirmed {
                self.verify_confirmed_inputs(store, actor, &config, &capability, record).await?;
            } else if !fake {
                return Err(rt_error(ErrorCode::InvalidArgument, "preflight_confirmation"));
            }
            let txn=store.write_transaction().await?;
            let updated=exec(&txn,"UPDATE rt_rooms SET status='running',boot_epoch=?,run_epoch=run_epoch+1,revision=revision+1,last_seq=last_seq+1,blocked_reason=NULL WHERE room_id=? AND status='paused' AND run_epoch=? AND active_control_id IS NULL",vec![num(as_i64(self.boot_epoch())?),text(&room.to_string()),num(column(&op,4)?)]).await?;
            if updated!=1 { return Err(rt_error(ErrorCode::InvalidState,"control_superseded")); }
            store.emit_current_in(&txn,&room.to_string(),"control").await?;
            let epoch=super::store::query_i64(&txn,"SELECT run_epoch FROM rt_rooms WHERE room_id=?",vec![text(&room.to_string())]).await?;
            txn.commit().await.map_err(storage_err)?;
            self.spawn_room(store.clone(),room,config,u64::try_from(epoch).map_err(|_|rt_error(ErrorCode::StorageUnavailable,"run_epoch"))?);
            Ok::<(),roundtable_protocol::RtError>(())
        }.await;
        if let Err(error) = result {
            block_control_failure(store, room, operation, &error).await?;
        }
        Ok(())
    }

    fn require_mutation(&self, fake: bool, execution: bool) -> RtResult<()> {
        if self.draining() {
            return Err(rt_error(ErrorCode::RuntimeUnavailable, "service_draining"));
        }
        if !self.writable() {
            return Err(rt_error(ErrorCode::Forbidden, "read_only_coordinator"));
        }
        if !fake && !matches!(self.readiness(), ServiceReadiness::Ready) {
            return Err(rt_error(
                ErrorCode::RuntimeUnavailable,
                "coordinator_not_ready",
            ));
        }
        if execution && !fake && !self.execution_gate.enabled() {
            return Err(rt_error(
                ErrorCode::CapabilityUnqualified,
                "product_disabled",
            ));
        }
        Ok(())
    }
}

fn decode<T: DeserializeOwned>(body: &Value) -> RtResult<T> {
    decode_json(&canonical_bytes(body)?, &ParseLimits::suggested_profile())
}
fn json_text(body: &str) -> RtResult<Value> {
    serde_json::from_str(body).map_err(|_| rt_error(ErrorCode::StorageUnavailable, "stored_json"))
}
fn config_hash(config: &RoundtableConfigV1) -> RtResult<String> {
    Ok(roundtable_protocol::Hash256::sha256(&canonical_bytes(config)?).to_hex())
}

/// Whether every seat's qualified sandbox will mount this workspace. Rooms
/// whose workspace overlaps a host-only path (codeg data, HOME, credentials)
/// fail closed at run time; preflight says so before anything is spent.
async fn workspace_mount_status(
    store: &RoundtableStore,
    data_dir: &std::path::Path,
    config: &RoundtableConfigV1,
) -> Value {
    let path = super::sandbox::WORKSPACE_MOUNT_DESTINATION;
    let unavailable = |reason: &str| json!({"path": path, "access": "read_only", "available": false, "reason": reason});
    let Some(id) = config.workspace_id.parse::<i64>().ok().filter(|id| *id > 0) else {
        return unavailable("workspace_id");
    };
    let root = match optional_row(
        store.connection(),
        "SELECT path FROM folder WHERE id=? AND deleted_at IS NULL",
        vec![num(id)],
    )
    .await
    {
        Ok(Some(row)) => column::<String>(&row, 0).ok().map(std::path::PathBuf::from),
        _ => None,
    };
    let Some(root) = root.and_then(|root| root.canonicalize().ok()) else {
        return unavailable("workspace_mount_path");
    };
    let home = std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .and_then(|home| home.canonicalize().ok());
    if let Ok(catalog) = super::installed_runtime::InstalledRuntime::load_catalog(data_dir) {
        for participant in &config.participants {
            let agent = participant.agent.as_deref().unwrap_or("codex");
            if let Some(installed) = catalog.get(agent) {
                if let Err(error) =
                    super::sandbox::workspace_mount_check(&root, home.as_deref(), &installed.oci)
                {
                    return unavailable(
                        error.details.reason.as_deref().unwrap_or("workspace_mount"),
                    );
                }
            }
        }
    }
    json!({"path": path, "access": "read_only", "available": true})
}

struct RoomRow {
    config: Value,
    status: String,
    revision: i64,
    run_epoch: i64,
    last_seq: i64,
}
async fn authorized_room(
    store: &RoundtableStore,
    actor: &ActorContext,
    room: &RoomId,
) -> RtResult<RoomRow> {
    let row=optional_row(store.connection(),"SELECT config_ref,status,revision,run_epoch,last_seq FROM rt_rooms WHERE room_id=? AND principal_id=?",vec![text(&room.to_string()),text(&actor.principal_id().to_string())]).await?
        .ok_or_else(||rt_error(ErrorCode::Forbidden,"not_found"))?;
    Ok(RoomRow {
        config: json_text(&column::<String>(&row, 0)?)?,
        status: column(&row, 1)?,
        revision: column(&row, 2)?,
        run_epoch: column(&row, 3)?,
        last_seq: column(&row, 4)?,
    })
}
async fn authorize(store: &RoundtableStore, actor: &ActorContext, room: &RoomId) -> RtResult<()> {
    authorized_room(store, actor, room).await.map(|_| ())
}
fn check_revision(row: &RoomRow, expected: Revision) -> RtResult<()> {
    if i64::try_from(expected.0).ok() != Some(row.revision) {
        return Err(rt_error(ErrorCode::RevisionConflict, "revision_conflict"));
    }
    Ok(())
}
async fn room_view(store: &RoundtableStore, room: &RoomId) -> RtResult<Value> {
    let projection = store.projection(room, None).await?;
    Ok(
        json!({"room_id":room,"config":projection.body.replay.config,"message_manifest_id":projection.projection_ref.id,"high_water_seq":projection.body.last_seq,"projection":projection}),
    )
}

async fn saved(
    txn: &DatabaseTransaction,
    actor: &ActorContext,
    command: &str,
    request: RequestId,
    body: &Value,
) -> RtResult<Option<Value>> {
    let row=optional_row(txn,"SELECT canonical_request_hash,method,status,immutable_response FROM rt_commands WHERE principal_id=? AND api_major=1 AND request_id=?",vec![text(&actor.principal_id().to_string()),text(&request.to_string())]).await?;
    match row {
        None => Ok(None),
        Some(row) => {
            if column::<String>(&row, 0)? != request_hash(command, body)?
                || column::<String>(&row, 1)? != command
            {
                return Err(rt_error(
                    ErrorCode::IdempotencyConflict,
                    "idempotency_conflict",
                ));
            }
            if column::<String>(&row, 2)? != "completed" {
                return Err(rt_error(
                    ErrorCode::CommandInProgress,
                    "command_in_progress",
                ));
            }
            Ok(Some(json_text(&column::<String>(&row, 3)?)?))
        }
    }
}
fn request_hash(command: &str, body: &Value) -> RtResult<String> {
    Ok(roundtable_protocol::Hash256::sha256(&canonical_bytes(
        &json!({"command":command,"body":body}),
    )?)
    .to_hex())
}
async fn save(
    txn: &DatabaseTransaction,
    actor: &ActorContext,
    command: &str,
    request: RequestId,
    body: &Value,
    room: &RoomId,
    response: &Value,
) -> RtResult<()> {
    exec(txn,"INSERT INTO rt_commands(principal_id,api_major,request_id,canonical_request_hash,method,room_id,status,immutable_response) VALUES(?,1,?,?,?,?, 'completed',?)",vec![text(&actor.principal_id().to_string()),text(&request.to_string()),text(&request_hash(command,body)?),text(command),text(&room.to_string()),text(&response.to_string())]).await?;
    Ok(())
}
async fn create(
    store: &RoundtableStore,
    actor: &ActorContext,
    command: &str,
    body: &Value,
    (request, mut config): (RequestId, RoundtableConfigV1),
    boot: u64,
    capture: Option<(&std::path::Path, &[String])>,
) -> RtResult<Value> {
    validate_config(&config, &ResourceLimits::suggested_profile())?;
    if command == "roundtable_create" && !config.source_refs.is_empty() {
        return Err(rt_error(ErrorCode::InvalidArgument, "source_room_required"));
    }
    let txn = store.write_transaction().await?;
    if let Some(value) = saved(&txn, actor, command, request, body).await? {
        txn.rollback().await.map_err(storage_err)?;
        return Ok(value);
    }
    let room: RoomId = Uuid::new_v4()
        .to_string()
        .parse()
        .map_err(|_| rt_error(ErrorCode::InvalidArgument, "room_id"))?;
    let captured = match capture {
        Some((data_dir, paths)) if !paths.is_empty() => {
            let (manifest, objects) =
                capture_selected_sources(&txn, actor, room, &config, data_dir, paths).await?;
            config.source_refs.push(roundtable_protocol::SourceRefV1 {
                snapshot_id: manifest
                    .manifest_id
                    .to_string()
                    .parse()
                    .map_err(|_| rt_error(ErrorCode::InvalidArgument, "manifest_id"))?,
                base_commit: manifest.base_commit.clone(),
            });
            Some((manifest, objects))
        }
        _ => None,
    };
    if let Some((manifest, _)) = &captured {
        check_source_metadata_budget(
            &config,
            &[json!({"manifest":manifest,"hash":manifest.manifest_hash.to_hex()})],
        )?;
    } else if config.source_refs.is_empty() {
        check_source_metadata_budget(&config, &[])?;
    }
    exec(&txn,"INSERT INTO rt_rooms(room_id,principal_id,status,config_ref,revision,run_epoch,boot_epoch,last_seq,remaining_active_ms) VALUES(?,?,'draft',?,1,0,?,1,?)",vec![text(&room.to_string()),text(&actor.principal_id().to_string()),text(&String::from_utf8(canonical_bytes(&config)?).map_err(|_|rt_error(ErrorCode::InvalidArgument,"config"))?),num(as_i64(boot)?),num(as_i64(config.budgets.room_budget.0)?)]).await?;
    if let Some((manifest, _)) = &captured {
        exec(&txn, "INSERT INTO rt_source_manifests(room_id,manifest_id,version,manifest_hash,body_json) VALUES(?,?,?,?,?)", vec![
            text(&room.to_string()), text(&manifest.manifest_id.to_string()), num(manifest.version),
            text(&manifest.manifest_hash.to_hex()), text(&serde_json::to_string(manifest)
                .map_err(|_| rt_error(ErrorCode::InvalidArgument, "manifest"))?),
        ]).await?;
    }
    if command == "roundtable_clone" {
        let source: RoomId = decode(&body["room_id"])?;
        let mut cloned_sources = Vec::new();
        for (index, source_ref) in config.source_refs.iter_mut().enumerate() {
            let row = optional_row(
                &txn,
                "SELECT body_json FROM rt_source_manifests WHERE room_id=? AND manifest_id=?",
                vec![
                    text(&source.to_string()),
                    text(&source_ref.snapshot_id.to_string()),
                ],
            )
            .await?
            .ok_or_else(|| rt_error(ErrorCode::Forbidden, "not_found"))?;
            let manifest: super::SourceManifestV1 =
                decode(&json_text(&column::<String>(&row, 0)?)?)?;
            let copied = super::rehome_manifest(&manifest, room, index as i64 + 1)?;
            cloned_sources.push(json!({"manifest":copied,"hash":copied.manifest_hash.to_hex()}));
            source_ref.snapshot_id = copied
                .manifest_id
                .to_string()
                .parse()
                .map_err(|_| rt_error(ErrorCode::InvalidArgument, "manifest_id"))?;
            exec(&txn,"INSERT INTO rt_source_manifests(room_id,manifest_id,version,manifest_hash,body_json) VALUES(?,?,?,?,?)",vec![text(&room.to_string()),text(&copied.manifest_id.to_string()),num(copied.version),text(&copied.manifest_hash.to_hex()),text(&serde_json::to_string(&copied).map_err(|_|rt_error(ErrorCode::InvalidArgument,"manifest"))?)]).await?;
        }
        check_source_metadata_budget(&config, &cloned_sources)?;
        exec(
            &txn,
            "UPDATE rt_rooms SET config_ref=? WHERE room_id=?",
            vec![
                text(
                    &String::from_utf8(canonical_bytes(&config)?)
                        .map_err(|_| rt_error(ErrorCode::InvalidArgument, "config"))?,
                ),
                text(&room.to_string()),
            ],
        )
        .await?;
    }
    write_speakers(&txn, &room, &config).await?;
    if command == "roundtable_clone" && body["carry_published_context"] == true {
        let source: RoomId = decode(&body["room_id"])?;
        let messages=rows(&txn,"SELECT m.body_json FROM rt_messages m JOIN rt_message_memberships p ON p.room_id=m.room_id AND p.message_id=m.message_id WHERE m.room_id=? AND p.visibility='published' AND p.membership_version=(SELECT MAX(q.membership_version) FROM rt_message_memberships q WHERE q.room_id=p.room_id AND q.message_id=p.message_id) ORDER BY m.message_id",vec![text(&source.to_string())]).await?;
        let context = messages
            .iter()
            .map(|row| column::<String>(row, 0))
            .collect::<RtResult<Vec<_>>>()?
            .join("\n");
        if context.len() as u64 > config.quotas.input_byte_limit.0 {
            return Err(rt_error(
                ErrorCode::InsufficientBudget,
                "published_context_bytes",
            ));
        }
        if !context.is_empty() {
            exec(&txn,"INSERT INTO rt_user_inputs(room_id,input_id,text,mode,accepted_seq,target_phase_index,state) VALUES(?,?,?,'next_phase',1,0,'queued')",vec![text(&room.to_string()),text(&Uuid::new_v4().to_string()),text(&context)]).await?;
        }
    }
    if command == "roundtable_clone" && body["carry_published_context"] == true {
        check_interjection_context(&txn, room, &config, None).await?;
    }
    let projection = store
        .emit_current_in(&txn, &room.to_string(), "create")
        .await?;
    let response = json!({"request_id":request,"accepted":true,"operation_id":null,"room_id":room,"revision":Revision(1),"run_epoch":Epoch(0),"last_seq":Seq(1),"status":"draft","projection_ref":projection});
    save(&txn, actor, command, request, body, &room, &response).await?;
    txn.commit().await.map_err(storage_err)?;
    if let Some((manifest, objects)) = captured {
        objects.mark_committed(&manifest.object_ids());
    }
    Ok(response)
}

async fn capture_selected_sources(
    txn: &DatabaseTransaction,
    actor: &ActorContext,
    room: RoomId,
    config: &RoundtableConfigV1,
    data_dir: &std::path::Path,
    paths: &[String],
) -> RtResult<(super::SourceManifestV1, super::ObjectStore)> {
    // Full-path Windows walks are not proven safe against in-place reparse
    // mutation. Keep the public capture route closed until handle-relative
    // containment has a qualified implementation and regression evidence.
    if cfg!(windows) {
        return Err(rt_error(
            ErrorCode::PolicyUnenforceable,
            "source_capture_unqualified",
        ));
    }
    const MAX_FILES: u32 = 32;
    const MAX_FILE_BYTES: u64 = 1024 * 1024;
    const MAX_TOTAL_BYTES: u64 = 4 * 1024 * 1024;
    const OBJECT_QUOTA_BYTES: u64 = 256 * 1024 * 1024;
    if paths.len() > MAX_FILES as usize || paths.iter().any(|path| path.len() > 4096) {
        return Err(rt_error(ErrorCode::InvalidArgument, "source_limit"));
    }
    // workspace_id is the registered folder ID, never a caller-supplied root.
    let workspace = config
        .workspace_id
        .parse::<i32>()
        .ok()
        .filter(|id| *id > 0)
        .ok_or_else(|| rt_error(ErrorCode::InvalidArgument, "workspace_id"))?;
    let row = optional_row(
        txn,
        "SELECT path FROM folder WHERE id=? AND deleted_at IS NULL AND kind='regular'",
        vec![num(i64::from(workspace))],
    )
    .await?
    .ok_or_else(|| rt_error(ErrorCode::Forbidden, "workspace_not_found"))?;
    let root = std::path::PathBuf::from(column::<String>(&row, 0)?);
    if !root.is_absolute() {
        return Err(rt_error(ErrorCode::InvalidArgument, "source_root"));
    }
    let files = paths
        .iter()
        .map(|path| {
            Ok(super::SelectedFile {
                relative_path: super::validate_relative_path(path)?,
                // Explicit selection is independent of Git status. No Git command
                // or repository-wide enumeration is needed to freeze these bytes.
                class: super::SourceClass::Selected,
            })
        })
        .collect::<RtResult<Vec<_>>>()?;
    let objects = super::ObjectStore::open(
        data_dir.join("roundtable/objects"),
        Arc::new(super::ReservationLedger::new(OBJECT_QUOTA_BYTES)),
        actor.principal_id(),
    )?;
    let manifest = super::snapshot::capture_snapshot_validated(
        super::SourceSelection {
            root,
            room_id: room,
            version: 1,
            base_commit: None,
            files,
            mutate_while_open: None,
        },
        super::SnapshotLimits {
            estimated_bytes: MAX_TOTAL_BYTES,
            max_file_bytes: MAX_FILE_BYTES,
            max_total_bytes: MAX_TOTAL_BYTES,
            max_files: MAX_FILES,
        },
        &objects,
        |manifest| {
            check_source_metadata_budget(
                config,
                &[json!({"manifest":manifest,"hash":manifest.manifest_hash.to_hex()})],
            )
        },
    )
    .await?;
    Ok((manifest, objects))
}

async fn write_speakers(
    txn: &DatabaseTransaction,
    room: &RoomId,
    config: &RoundtableConfigV1,
) -> RtResult<()> {
    for member in &config.participants {
        exec(txn,"INSERT INTO rt_speakers(room_id,speaker_id,ordinal,role,provider_ref,model_id,model_snapshot_json) VALUES(?,?,?,?,?,?,?)",vec![text(&room.to_string()),text(&Uuid::new_v4().to_string()),num(i64::from(member.ordinal)),text("member"),text(&member.provider_ref),text(member.model.as_deref().unwrap_or("default")),text(&serde_json::to_string(member).map_err(|_|rt_error(ErrorCode::InvalidArgument,"participant"))?)]).await?;
    }
    let moderator = config
        .participants
        .iter()
        .find(|member| member.ordinal == config.moderator_ordinal)
        .ok_or_else(|| rt_error(ErrorCode::InvalidArgument, "moderator_ordinal"))?;
    exec(txn,"INSERT INTO rt_speakers(room_id,speaker_id,ordinal,role,provider_ref,model_id,model_snapshot_json) VALUES(?,?,?,'moderator',?,?,?)",vec![text(&room.to_string()),text(&Uuid::new_v4().to_string()),num(config.participants.len() as i64),text(&moderator.provider_ref),text(moderator.model.as_deref().unwrap_or("default")),text(&serde_json::to_string(moderator).map_err(|_|rt_error(ErrorCode::InvalidArgument,"moderator"))?)]).await?;
    Ok(())
}
async fn update(
    store: &RoundtableStore,
    actor: &ActorContext,
    command: &str,
    body: &Value,
    request: UpdateDraftRequest,
) -> RtResult<Value> {
    validate_config(&request.config, &ResourceLimits::suggested_profile())?;
    let current = authorized_room(store, actor, &request.room_id).await?;
    frozen_sources(store, actor, Some(request.room_id), &request.config).await?;
    let txn = store.write_transaction().await?;
    if let Some(value) = saved(&txn, actor, command, request.request_id, body).await? {
        txn.rollback().await.map_err(storage_err)?;
        return Ok(value);
    }
    check_revision(&current, request.expected_revision)?;
    if current.status != "draft" {
        return Err(rt_error(ErrorCode::InvalidState, "draft_required"));
    }
    let changed=exec(&txn,"UPDATE rt_rooms SET config_ref=?,revision=revision+1,last_seq=last_seq+1,remaining_active_ms=? WHERE room_id=? AND revision=? AND status='draft'",vec![text(&serde_json::to_string(&request.config).map_err(|_|rt_error(ErrorCode::InvalidArgument,"config"))?),num(as_i64(request.config.budgets.room_budget.0)?),text(&request.room_id.to_string()),num(current.revision)]).await?;
    if changed != 1 {
        return Err(rt_error(ErrorCode::RevisionConflict, "revision_conflict"));
    }
    exec(
        &txn,
        "DELETE FROM rt_speakers WHERE room_id=?",
        vec![text(&request.room_id.to_string())],
    )
    .await?;
    write_speakers(&txn, &request.room_id, &request.config).await?;
    store
        .emit_current_in(&txn, &request.room_id.to_string(), "config")
        .await?;
    let response = json!({"request_id":request.request_id,"accepted":true,"operation_id":null,"room_id":request.room_id,"revision":decimal::<Revision>(current.revision+1)?,"run_epoch":decimal::<Epoch>(current.run_epoch)?,"last_seq":decimal::<Seq>(current.last_seq+1)?,"status":"draft"});
    save(
        &txn,
        actor,
        command,
        request.request_id,
        body,
        &request.room_id,
        &response,
    )
    .await?;
    txn.commit().await.map_err(storage_err)?;
    Ok(response)
}
fn as_i64(n: u64) -> RtResult<i64> {
    i64::try_from(n).map_err(|_| rt_error(ErrorCode::InvalidArgument, "overflow"))
}
fn decimal<T: DeserializeOwned>(n: i64) -> RtResult<T> {
    if n < 0 {
        return Err(rt_error(ErrorCode::StorageUnavailable, "negative_counter"));
    }
    serde_json::from_value(json!(n.to_string()))
        .map_err(|_| rt_error(ErrorCode::StorageUnavailable, "counter"))
}

fn control_request(command: &str, body: &Value) -> RtResult<ControlRequest> {
    let (room_id, request_id, expected_revision, kind, force_latest) = match command {
        "roundtable_pause" => {
            let r: PauseRequest = decode(body)?;
            (
                r.room_id,
                r.request_id,
                r.expected_revision,
                ControlKind::Pause,
                false,
            )
        }
        "roundtable_stop" => {
            let r: StopRequest = decode(body)?;
            (
                r.room_id,
                r.request_id,
                r.expected_revision,
                ControlKind::Stop,
                r.force_latest,
            )
        }
        "roundtable_retry_synthesis" => {
            let r: RetrySynthesisRequest = decode(body)?;
            (
                r.room_id,
                r.request_id,
                r.expected_revision,
                ControlKind::RetrySynthesis,
                false,
            )
        }
        _ => {
            let r: InterjectRequest = decode(body)?;
            (
                r.room_id,
                r.request_id,
                r.expected_revision,
                ControlKind::RestartCurrent,
                false,
            )
        }
    };
    Ok(ControlRequest {
        room_id,
        request_id,
        expected_revision,
        kind,
        input: body.clone(),
        force_latest,
    })
}

async fn block_control_failure(
    store: &RoundtableStore,
    room: RoomId,
    operation: roundtable_protocol::OperationId,
    error: &roundtable_protocol::RtError,
) -> RtResult<()> {
    let txn = store.write_transaction().await?;
    let reason: String = error
        .details
        .reason
        .as_deref()
        .unwrap_or("control_blocked")
        .chars()
        .take(512)
        .collect();
    let updated = exec(&txn,
        "UPDATE rt_control_operations SET status='blocked',step='blocked',blocked_reason=? WHERE room_id=? AND operation_id=? AND superseded_by IS NULL AND status IN('open','completed') AND EXISTS(SELECT 1 FROM rt_rooms r WHERE r.room_id=rt_control_operations.room_id AND r.run_epoch=rt_control_operations.requested_epoch AND (r.active_control_id=rt_control_operations.operation_id OR (rt_control_operations.status='completed' AND r.active_control_id IS NULL AND r.status='paused' AND r.current_phase_id=rt_control_operations.successor_phase_id)))",
        vec![text(&reason),text(&room.to_string()),text(&operation.to_string())]).await?;
    if updated == 1 {
        exec(&txn,"UPDATE rt_rooms SET blocked_reason='recovery_required',revision=revision+1,last_seq=last_seq+1 WHERE room_id=?",vec![text(&room.to_string())]).await?;
        store
            .emit_current_in(&txn, &room.to_string(), "control")
            .await?;
    }
    txn.commit().await.map_err(storage_err)?;
    Ok(())
}

async fn start(
    store: &RoundtableStore,
    actor: &ActorContext,
    command: &str,
    body: &Value,
    identity: (RoomId, RequestId, Revision),
    boot: u64,
    config: &RoundtableConfigV1,
) -> RtResult<Value> {
    let (room, request, revision) = identity;
    let txn = store.write_transaction().await?;
    if let Some(response) = saved(&txn, actor, command, request, body).await? {
        txn.rollback().await.map_err(storage_err)?;
        return Ok(response);
    }
    if command == "roundtable_resume" {
        super::control::validate_resume_in(&txn, &room, config).await?;
    }
    let config_body = String::from_utf8(canonical_bytes(config)?)
        .map_err(|_| rt_error(ErrorCode::InvalidArgument, "config"))?;
    let updated=exec(&txn,"UPDATE rt_rooms SET status='running',config_ref=?,revision=revision+1,run_epoch=run_epoch+1,last_seq=last_seq+1,boot_epoch=?,blocked_reason=NULL WHERE room_id=? AND principal_id=? AND revision=? AND status IN('draft','ready','paused') AND active_control_id IS NULL",vec![text(&config_body),num(as_i64(boot)?),text(&room.to_string()),text(&actor.principal_id().to_string()),num(as_i64(revision.0)?)]).await?;
    if updated != 1 {
        return Err(rt_error(ErrorCode::InvalidState, "start_state"));
    }
    store
        .emit_current_in(&txn, &room.to_string(), "start")
        .await?;
    let row = optional_row(
        &txn,
        "SELECT revision,run_epoch,last_seq FROM rt_rooms WHERE room_id=?",
        vec![text(&room.to_string())],
    )
    .await?
    .ok_or_else(|| rt_error(ErrorCode::InvalidState, "room_missing"))?;
    let response = json!(MutationAck {
        request_id: request,
        accepted: true,
        operation_id: None,
        room_id: room,
        revision: Revision(column::<i64>(&row, 0)? as u64),
        run_epoch: Epoch(column::<i64>(&row, 1)? as u64),
        last_seq: Seq(column::<i64>(&row, 2)? as u64),
        status: RoomState::Running
    });
    save(&txn, actor, command, request, body, &room, &response).await?;
    txn.commit().await.map_err(storage_err)?;
    Ok(response)
}
async fn fail_run(
    store: &RoundtableStore,
    room: RoomId,
    boot: u64,
    epoch: u64,
    error: &roundtable_protocol::RtError,
) -> RtResult<bool> {
    let txn = store.write_transaction().await?;
    let fenced = exec(&txn,"UPDATE rt_rooms SET status='paused',run_epoch=run_epoch+1,revision=revision+1,last_seq=last_seq+1,blocked_reason='recovery_required' WHERE room_id=? AND boot_epoch=? AND run_epoch=? AND status='running'",vec![text(&room.to_string()),num(as_i64(boot)?),num(as_i64(epoch)?)]).await?==1;
    if fenced {
        store
            .emit_current_in(&txn, &room.to_string(), "terminal")
            .await?;
    }
    tracing::warn!(code=?error.code, reason=?error.details.reason, "roundtable run ended before completion");
    txn.commit().await.map_err(storage_err)?;
    Ok(fenced)
}

async fn list(
    store: &RoundtableStore,
    actor: &ActorContext,
    request: ListRequest,
) -> RtResult<Value> {
    let limit = request.limit.map_or(100, |limit| limit.0);
    if !(1..=100).contains(&limit) {
        return Err(rt_error(ErrorCode::InvalidArgument, "page_limit"));
    }
    let after = request.cursor.unwrap_or_default();
    if !after.is_empty() {
        let _: RoomId = after
            .parse()
            .map_err(|_| rt_error(ErrorCode::InvalidArgument, "cursor"))?;
    }
    let candidates=rows(store.connection(),"SELECT room_id,status,revision,config_ref FROM rt_rooms WHERE principal_id=? AND json_extract(config_ref,'$.workspace_id')=? AND room_id>? ORDER BY room_id LIMIT ?",vec![text(&actor.principal_id().to_string()),text(&request.workspace_id),text(&after),num(as_i64(limit+1)?)]).await?;
    let mut items = Vec::new();
    let mut bytes = 4096usize;
    let mut byte_limited = false;
    for row in candidates {
        let config = json_text(&column::<String>(&row, 3)?)?;
        if config["workspace_id"] != request.workspace_id {
            continue;
        }
        let item = json!({"room_id":column::<String>(&row,0)?,"status":column::<String>(&row,1)?,"revision":column::<i64>(&row,2)?.to_string(),"config":config});
        bytes = bytes.saturating_add(canonical_bytes(&item)?.len() + 1);
        if bytes > 1024 * 1024 {
            byte_limited = true;
            break;
        }
        items.push(item);
    }
    let more = byte_limited || items.len() > limit as usize;
    items.truncate(limit as usize);
    let cursor = if more {
        items
            .last()
            .and_then(|item| item["room_id"].as_str())
            .map(str::to_owned)
    } else {
        None
    };
    Ok(json!({"rooms":items,"cursor":cursor}))
}

async fn event_page(
    service: &RoundtableService,
    store: &RoundtableStore,
    actor: &ActorContext,
    request: EventsRequest,
) -> RtResult<Value> {
    let projection = optional_row(
        store.connection(),
        "SELECT projection_id FROM rt_events WHERE room_id=? AND seq=?",
        vec![
            text(&request.room_id.to_string()),
            num(as_i64(request.through_seq.0)?),
        ],
    )
    .await?
    .ok_or_else(|| rt_error(ErrorCode::InvalidArgument, "event_watermark"))?;
    let id: String = column(&projection, 0)?;
    let scope = super::CursorScope {
        principal_id: actor.principal_id(),
        room_id: request.room_id,
        projection_id: id
            .parse()
            .map_err(|_| rt_error(ErrorCode::StorageUnavailable, "projection_id"))?,
        manifest_id: id
            .parse()
            .map_err(|_| rt_error(ErrorCode::StorageUnavailable, "projection_id"))?,
    };
    let now = store.clock_sample().0;
    let cursor = match request.cursor.as_deref() {
        Some(cursor) => Some(
            service
                .cursors
                .lock()
                .expect("cursors")
                .resolve(cursor, &scope, now)?,
        ),
        None => None,
    };
    if request.after_seq.0 > request.through_seq.0 {
        return Err(rt_error(ErrorCode::InvalidArgument, "event_bounds"));
    }
    let start = match cursor {
        Some(cursor) => {
            let (after, offset) = cursor
                .split_once(':')
                .ok_or_else(|| rt_error(ErrorCode::InvalidArgument, "cursor"))?;
            if after != request.after_seq.0.to_string() {
                return Err(rt_error(ErrorCode::InvalidArgument, "cursor_scope"));
            }
            offset
                .parse::<u64>()
                .map_err(|_| rt_error(ErrorCode::InvalidArgument, "cursor"))?
        }
        None => request.after_seq.0,
    };
    if start < request.after_seq.0 || start > request.through_seq.0 {
        return Err(rt_error(ErrorCode::InvalidArgument, "cursor"));
    }
    let page=rows(store.connection(),"SELECT seq,projection_id,projection_hash,cause,schema_version,event_id,resulting_revision,run_epoch,boot_epoch,occurred_at,changed_entities_json FROM rt_events WHERE room_id=? AND seq>? AND seq<=? ORDER BY seq LIMIT 101",vec![text(&request.room_id.to_string()),num(as_i64(start)?),num(as_i64(request.through_seq.0)?)]).await?;
    let mut items = Vec::new();
    let mut page_bytes = 4096usize;
    let mut byte_limited = false;
    for row in page {
        let projection_id: String = column(&row, 1)?;
        let projection_hash: String = column(&row, 2)?;
        let cause: String = column(&row, 3)?;
        let item = json!({"seq":decimal::<Seq>(column::<i64>(&row,0)?)?,"room_id":request.room_id,"event_id":column::<String>(&row,5)?,"resulting_revision":decimal::<Revision>(column(&row,6)?)?,"run_epoch":decimal::<Epoch>(column(&row,7)?)?,"boot_epoch":decimal::<Epoch>(column(&row,8)?)?,"occurred_at":column::<String>(&row,9)?,"projection_ref":{"id":projection_id,"hash":projection_hash},"cause":cause,"schema_version":column::<i64>(&row,4)?,"payload":{"projection_id":projection_id,"projection_hash":projection_hash,"cause":cause,"changed_entities":json_text(&column::<String>(&row,10)?)?}});
        page_bytes = page_bytes.saturating_add(canonical_bytes(&item)?.len() + 1);
        if page_bytes > 1024 * 1024 {
            if items.is_empty() {
                return Err(rt_error(ErrorCode::CapacityLimited, "event_response_bytes"));
            }
            byte_limited = true;
            break;
        }
        items.push(item);
    }
    let more = byte_limited || items.len() > 100;
    items.truncate(100);
    let cursor = if more {
        Some(service.cursors.lock().expect("cursors").issue(
            scope,
            format!(
                    "{}:{}",
                    request.after_seq.0,
                    items
                        .last()
                        .and_then(|item| item["seq"].as_str())
                        .ok_or_else(|| rt_error(ErrorCode::StorageUnavailable, "event_page"))?
                ),
            now,
        )?)
    } else {
        None
    };
    Ok(json!({"events":items,"cursor":cursor,"through_seq":request.through_seq}))
}

async fn message_page(
    service: &RoundtableService,
    store: &RoundtableStore,
    actor: &ActorContext,
    request: MessagesRequest,
) -> RtResult<Value> {
    let manifest = optional_row(
        store.connection(),
        "SELECT projection_id FROM rt_page_manifests WHERE room_id=? AND manifest_id=?",
        vec![
            text(&request.room_id.to_string()),
            text(&request.manifest_id.to_string()),
        ],
    )
    .await?
    .ok_or_else(|| rt_error(ErrorCode::Forbidden, "not_found"))?;
    let scope = super::CursorScope {
        principal_id: actor.principal_id(),
        room_id: request.room_id,
        projection_id: column::<String>(&manifest, 0)?
            .parse()
            .map_err(|_| rt_error(ErrorCode::StorageUnavailable, "projection_id"))?,
        manifest_id: request.manifest_id,
    };
    let now = store.clock_sample().0;
    let cursor = match request.cursor.as_deref() {
        Some(cursor) => Some(
            service
                .cursors
                .lock()
                .expect("cursors")
                .resolve(cursor, &scope, now)?,
        ),
        None => None,
    };
    // Speaker, attempt, and phase are envelope metadata for the transcript.
    // `body_hash` still covers only `body`; membership visibility is unchanged.
    let entries = rows(
        store.connection(),
        "SELECT e.message_id, e.body_hash, e.visibility, m.body_json,
                m.speaker_id, m.attempt_id, t.phase_id, a.attempt_no, a.state,
                a.finished_at
         FROM rt_page_manifest_entries e
         JOIN rt_messages m
           ON m.room_id = e.room_id AND m.message_id = e.message_id
         LEFT JOIN rt_attempts a
           ON a.room_id = m.room_id AND a.attempt_id = m.attempt_id
         LEFT JOIN rt_turns t
           ON t.room_id = a.room_id AND t.turn_id = a.turn_id
         WHERE e.room_id = ? AND e.manifest_id = ?
         ORDER BY e.entry_offset",
        vec![
            text(&request.room_id.to_string()),
            text(&request.manifest_id.to_string()),
        ],
    )
    .await?;
    let mut serialized = Vec::new();
    for entry in entries {
        let body = json_text(&column::<String>(&entry, 3)?)?;
        let body_hash: String = column(&entry, 1)?;
        if roundtable_protocol::canonical_hash(&body)?.to_hex() != body_hash {
            return Err(rt_error(ErrorCode::StorageUnavailable, "message_hash"));
        }
        let message_id: String = column(&entry, 0)?;
        let visibility: String = column(&entry, 2)?;
        let speaker_id: String = column(&entry, 4)?;
        let attempt_id: String = column(&entry, 5)?;
        let phase_id: Option<String> = column(&entry, 6)?;
        let attempt_no: Option<i64> = column(&entry, 7)?;
        let attempt_state: Option<String> = column(&entry, 8)?;
        let finished_at: Option<String> = column(&entry, 9)?;
        serialized.push(
            json!({
                "message_id": message_id,
                "body_hash": body_hash,
                "visibility": visibility,
                "speaker_id": speaker_id,
                "attempt_id": attempt_id,
                "phase_id": phase_id,
                "attempt_no": attempt_no,
                "attempt_state": attempt_state,
                "finished_at": finished_at,
                "body": body,
            })
            .to_string(),
        );
    }
    let hash = roundtable_protocol::Hash256::sha256(&canonical_bytes(&serialized)?);
    let page = super::read_manifest_page(&serialized, hash, cursor.as_deref(), 100)?;
    let offset = page
        .cursor
        .rsplit_once(':')
        .and_then(|(_, offset)| offset.parse::<usize>().ok())
        .ok_or_else(|| rt_error(ErrorCode::StorageUnavailable, "page_cursor"))?;
    let next = if offset < serialized.len() {
        Some(
            service
                .cursors
                .lock()
                .expect("cursors")
                .issue(scope, page.cursor, now)?,
        )
    } else {
        None
    };
    Ok(
        json!({"messages":page.entries.iter().map(|body|json_text(body)).collect::<RtResult<Vec<_>>>()?,"body_hash":page.body_hash,"cursor":next}),
    )
}
async fn usage_view(
    store: &RoundtableStore,
    room: &RoomId,
    after: Option<Revision>,
) -> RtResult<Value> {
    let txn = store.connection().begin().await.map_err(storage_err)?;
    let attempts = super::store::query_i64(
        &txn,
        "SELECT COUNT(*) FROM rt_attempts WHERE room_id=?",
        vec![text(&room.to_string())],
    )
    .await?;
    let sampled=super::store::query_i64(&txn,"SELECT COALESCE(MAX(sampled_active_ms),0) FROM rt_measurements WHERE room_id=? AND kind='active'",vec![text(&room.to_string())]).await?;
    let version = super::store::query_i64(
        &txn,
        "SELECT COALESCE(MAX(ledger_seq),0) FROM rt_measurements WHERE room_id=?",
        vec![text(&room.to_string())],
    )
    .await?;
    let summary = super::usage::usage_summary_in(&txn, &room.to_string()).await?;
    // The immutable archives have independent sequences. Their sum advances
    // whenever either stream advances, including a late provider measurement.
    let version = version
        .checked_add(
            summary["usage_version"]
                .as_i64()
                .ok_or_else(|| rt_error(ErrorCode::StorageUnavailable, "usage_version"))?,
        )
        .ok_or_else(|| rt_error(ErrorCode::StorageUnavailable, "usage_version"))?;
    let version = decimal::<Revision>(version)?;
    if after.is_some_and(|after| after > version) {
        return Err(rt_error(ErrorCode::InvalidArgument, "usage_version"));
    }
    let totals = vec![
        roundtable_protocol::NamedCountV1 {
            key: "active_ms".into(),
            value: count(sampled)?,
        },
        roundtable_protocol::NamedCountV1 {
            key: "attempts".into(),
            value: count(attempts)?,
        },
    ];
    let response = json!(roundtable_protocol::UsageViewV1 {
        usage_version: version,
        measurements: if after == Some(version) {
            vec![]
        } else {
            totals.clone()
        },
        totals,
        unknown_count: count(
            summary["unknown_count"]
                .as_u64()
                .and_then(|n| i64::try_from(n).ok())
                .ok_or_else(|| rt_error(ErrorCode::StorageUnavailable, "usage_count"))?
        )?,
        confirmed_output_tokens: match summary["confirmed_output_tokens"].as_u64() {
            Some(tokens) =>
                Some(count(i64::try_from(tokens).map_err(|_| {
                    rt_error(ErrorCode::StorageUnavailable, "usage_count")
                })?)?),
            None if summary["confirmed_output_tokens"].is_null() => None,
            None => return Err(rt_error(ErrorCode::StorageUnavailable, "usage_count")),
        },
        unknown_total: summary["unknown_total"]
            .as_bool()
            .ok_or_else(|| { rt_error(ErrorCode::StorageUnavailable, "usage_count") })?,
        uncertain: summary["uncertain"]
            .as_bool()
            .ok_or_else(|| { rt_error(ErrorCode::StorageUnavailable, "usage_count") })?,
    });
    txn.rollback().await.map_err(storage_err)?;
    Ok(response)
}

fn count(n: i64) -> RtResult<roundtable_protocol::SafeInt> {
    if !(0..=9_007_199_254_740_991).contains(&n) {
        return Err(rt_error(ErrorCode::StorageUnavailable, "counter_range"));
    }
    Ok(roundtable_protocol::SafeInt(n as u64))
}

async fn record_rejection(
    store: &RoundtableStore,
    room: RoomId,
    error: &roundtable_protocol::RtError,
) -> RtResult<()> {
    let txn = store.write_transaction().await?;
    let kind = format!(
        "admission_rejected:{}",
        serde_json::to_value(error.code)
            .map_err(|_| rt_error(ErrorCode::StorageUnavailable, "error_code"))?
            .as_str()
            .ok_or_else(|| rt_error(ErrorCode::StorageUnavailable, "error_code"))?
    );
    exec(&txn,"INSERT INTO rt_metric_counters(room_id,key,value) VALUES(?,?,1) ON CONFLICT(room_id,key) DO UPDATE SET value=value+1 WHERE value<9007199254740991",vec![text(&room.to_string()),text(&kind)]).await?;
    txn.commit().await.map_err(storage_err)?;
    Ok(())
}

async fn metrics_view(
    service: &RoundtableService,
    store: &RoundtableStore,
    room: &RoomId,
) -> RtResult<Value> {
    use super::store::query_i64;
    let value =
        |sql: &'static str| query_i64(store.connection(), sql, vec![text(&room.to_string())]);
    let mut rejected = std::collections::BTreeMap::new();
    for row in rows(store.connection(),"SELECT substr(key,20),value FROM rt_metric_counters WHERE room_id=? AND key LIKE 'admission_rejected:%'",vec![text(&room.to_string())]).await? {rejected.insert(column::<String>(&row,0)?,count(column(&row,1)?)?);}
    let mut storage_bytes=value("SELECT COALESCE(SUM(length(CAST(body_json AS BLOB))),0) FROM (SELECT room_id,body_json FROM rt_messages UNION ALL SELECT room_id,body_json FROM rt_projection_versions UNION ALL SELECT room_id,body_json FROM rt_evidence UNION ALL SELECT room_id,body_json FROM rt_source_manifests) WHERE room_id=?").await?;
    let mut hashes = std::collections::BTreeSet::new();
    for row in rows(
        store.connection(),
        "SELECT body_json FROM rt_source_manifests WHERE room_id=?",
        vec![text(&room.to_string())],
    )
    .await?
    {
        let manifest: super::SourceManifestV1 =
            json_text(&column::<String>(&row, 0)?).and_then(|body| decode(&body))?;
        for entry in manifest.entries {
            if hashes.insert(entry.object.object_id.clone()) {
                let path = service
                    .data_dir
                    .join("roundtable/objects")
                    .join(entry.object.object_id);
                let metadata = std::fs::symlink_metadata(path)
                    .map_err(|_| rt_error(ErrorCode::StorageUnavailable, "object_missing"))?;
                if !metadata.is_file()
                    || metadata.file_type().is_symlink()
                    || metadata.len() != entry.object.total_bytes
                {
                    return Err(rt_error(ErrorCode::StorageUnavailable, "object_shape"));
                }
                storage_bytes = storage_bytes
                    .checked_add(as_i64(metadata.len())?)
                    .ok_or_else(|| rt_error(ErrorCode::StorageUnavailable, "storage_count"))?;
            }
        }
    }
    Ok(json!(roundtable_protocol::MetricsViewV1{
        uncertain_count:count(value("SELECT COUNT(*) FROM rt_attempts WHERE room_id=? AND state='uncertain'").await?)?,
        cleanup_overrun_ms:decimal(value("SELECT COALESCE(SUM(sampled_active_ms),0) FROM rt_measurements WHERE room_id=? AND kind='cleanup_overrun'").await?)?,
        admission_rejections_by_reason:rejected,
        event_count:count(value("SELECT COUNT(*) FROM rt_events WHERE room_id=?").await?)?,
        reserved_event_slots:count(value("SELECT COALESCE(SUM(amount_bytes),0) FROM rt_budget_reservations WHERE room_id=? AND purpose='event_slots' AND state='held'").await?)?,
        storage_used_bytes:count(storage_bytes)?,
        storage_reserved_bytes:count(value("SELECT COALESCE(SUM(amount_bytes),0) FROM rt_budget_reservations WHERE room_id=? AND purpose<>'event_slots' AND state='held'").await?)?,
        unknown_measurements:count(value("SELECT COUNT(*) FROM rt_usage_archive WHERE room_id=? AND (json_extract(body_json,'$.value') IS NULL OR json_extract(body_json,'$.attributed')=0)").await?)?,
    }))
}

async fn frozen_sources(
    store: &RoundtableStore,
    actor: &ActorContext,
    room: Option<RoomId>,
    config: &RoundtableConfigV1,
) -> RtResult<Vec<Value>> {
    let mut manifests = Vec::new();
    for source in &config.source_refs {
        let room =
            room.ok_or_else(|| rt_error(ErrorCode::InvalidArgument, "source_room_required"))?;
        authorize(store, actor, &room).await?;
        let row=optional_row(store.connection(),"SELECT body_json,manifest_hash FROM rt_source_manifests WHERE room_id=? AND manifest_id=?",vec![text(&room.to_string()),text(&source.snapshot_id.to_string())]).await?.ok_or_else(||rt_error(ErrorCode::Forbidden,"not_found"))?;
        let body = json_text(&column::<String>(&row, 0)?)?;
        if body["base_commit"] != json!(source.base_commit) {
            return Err(rt_error(ErrorCode::InvalidArgument, "source_version"));
        }
        let manifest: super::SourceManifestV1 = decode(&body)?;
        let checked = super::rehome_manifest(&manifest, room, manifest.version)?;
        if checked.manifest_hash != manifest.manifest_hash
            || manifest.manifest_hash.to_hex() != column::<String>(&row, 1)?
        {
            return Err(rt_error(
                ErrorCode::StorageUnavailable,
                "source_manifest_hash",
            ));
        }
        manifests.push(json!({"manifest":body,"hash":column::<String>(&row,1)?}));
    }
    check_source_metadata_budget(config, &manifests)?;
    Ok(manifests)
}

/// Conservative bound for the initial question, source manifest and the
/// per-file evidence/alias descriptors constructed by the runtime. Include
/// space for a freshly rehomed manifest even in topic-only rooms.
fn check_source_metadata_budget(config: &RoundtableConfigV1, manifests: &[Value]) -> RtResult<()> {
    let mut size =
        canonical_bytes(&json!({"topic":config.topic,"sources":manifests}))?.len() as u64;
    size = size
        .checked_add(1024)
        .ok_or_else(|| rt_error(ErrorCode::InvalidArgument, "overflow"))?;
    for manifest in manifests {
        let entries = manifest["manifest"]["entries"]
            .as_array()
            .ok_or_else(|| rt_error(ErrorCode::StorageUnavailable, "source_manifest"))?;
        for entry in entries {
            let path = canonical_bytes(&entry["path"])?;
            // UUID aliases, object references and JSON syntax, plus escaped path.
            size = size
                .checked_add(1024 + path.len() as u64)
                .ok_or_else(|| rt_error(ErrorCode::InvalidArgument, "overflow"))?;
        }
    }
    // Stored manifests must also remain decodable by the strict public reader.
    let maximum = config
        .quotas
        .input_byte_limit
        .0
        .min(ParseLimits::suggested_profile().max_bytes as u64);
    if size > maximum {
        return Err(rt_error(
            ErrorCode::InsufficientBudget,
            "source_metadata_bytes",
        ));
    }
    Ok(())
}

async fn check_interjection_context(
    conn: &impl sea_orm::ConnectionTrait,
    room: RoomId,
    config: &RoundtableConfigV1,
    proposed: Option<(String, &str)>,
) -> RtResult<()> {
    let stored = rows(
        conn,
        "SELECT input_id,text FROM rt_user_inputs WHERE room_id=? ORDER BY accepted_seq,input_id",
        vec![text(&room.to_string())],
    )
    .await?;
    let mut inputs = Vec::with_capacity(stored.len() + usize::from(proposed.is_some()));
    for row in stored {
        inputs.push(json!({"input_id":column::<String>(&row,0)?,"text":column::<String>(&row,1)?}));
    }
    if let Some((id, value)) = proposed {
        inputs.push(json!({"input_id":id,"text":value}));
    }
    roundtable_protocol::validate_interjection_context(
        &json!(inputs),
        config.quotas.interjection_byte_limit.0,
    )?;
    Ok(())
}

async fn next_input(
    store: &RoundtableStore,
    actor: &ActorContext,
    command: &str,
    body: &Value,
    request: InterjectRequest,
) -> RtResult<Value> {
    let txn = store.write_transaction().await?;
    if let Some(replay) = saved(&txn, actor, command, request.request_id, body).await? {
        txn.rollback().await.map_err(storage_err)?;
        return Ok(replay);
    }
    let row=optional_row(&txn,"SELECT revision,status,config_ref,current_phase_id FROM rt_rooms WHERE room_id=? AND principal_id=?",vec![text(&request.room_id.to_string()),text(&actor.principal_id().to_string())]).await?.ok_or_else(||rt_error(ErrorCode::Forbidden,"not_found"))?;
    let revision: i64 = column(&row, 0)?;
    if u64::try_from(revision).ok() != Some(request.expected_revision.0) {
        return Err(rt_error(ErrorCode::RevisionConflict, "revision_conflict"));
    }
    let status: String = column(&row, 1)?;
    if !matches!(status.as_str(), "running" | "paused") {
        return Err(rt_error(ErrorCode::InvalidState, "input_state"));
    }
    let config: RoundtableConfigV1 = decode(&json_text(&column::<String>(&row, 2)?)?)?;
    if request.text.trim().is_empty() {
        return Err(rt_error(ErrorCode::InvalidArgument, "input_empty"));
    }
    let used = super::store::query_i64(
        &txn,
        "SELECT COALESCE(SUM(length(CAST(text AS BLOB))),0) FROM rt_user_inputs WHERE room_id=?",
        vec![text(&request.room_id.to_string())],
    )
    .await?;
    if (used as u64)
        .checked_add(request.text.len() as u64)
        .is_none_or(|sum| sum > config.quotas.interjection_byte_limit.0)
    {
        return Err(rt_error(
            ErrorCode::InsufficientBudget,
            "interjection_bytes",
        ));
    }
    let input_id = Uuid::new_v4().to_string();
    check_interjection_context(
        &txn,
        request.room_id,
        &config,
        Some((input_id.clone(), &request.text)),
    )
    .await?;
    let phase: Option<String> = column(&row, 3)?;
    let phase = phase.ok_or_else(|| rt_error(ErrorCode::NoNextPhase, "no_next_phase"))?;
    let phase_row = optional_row(
        &txn,
        "SELECT phase_index FROM rt_phases WHERE room_id=? AND phase_id=?",
        vec![text(&request.room_id.to_string()), text(&phase)],
    )
    .await?
    .ok_or_else(|| rt_error(ErrorCode::NoNextPhase, "no_next_phase"))?;
    let index: i64 = column(&phase_row, 0)?;
    if index > i64::from(config.strategy.critique_rounds) {
        return Err(rt_error(ErrorCode::NoNextPhase, "no_next_phase"));
    }
    exec(&txn,"UPDATE rt_rooms SET revision=revision+1,last_seq=last_seq+1 WHERE room_id=? AND revision=?",vec![text(&request.room_id.to_string()),num(revision)]).await?;
    let seq = super::store::query_i64(
        &txn,
        "SELECT last_seq FROM rt_rooms WHERE room_id=?",
        vec![text(&request.room_id.to_string())],
    )
    .await?;
    exec(&txn,"INSERT INTO rt_user_inputs(room_id,input_id,text,mode,accepted_seq,target_phase_index,state) VALUES(?,?,?,'next_phase',?,?,'queued')",vec![text(&request.room_id.to_string()),text(&input_id),text(&request.text),num(seq),num(index+1)]).await?;
    store
        .emit_current_in(&txn, &request.room_id.to_string(), "input")
        .await?;
    let epoch = super::store::query_i64(
        &txn,
        "SELECT run_epoch FROM rt_rooms WHERE room_id=?",
        vec![text(&request.room_id.to_string())],
    )
    .await?;
    let response = json!({"request_id":request.request_id,"accepted":true,"operation_id":null,"room_id":request.room_id,"revision":decimal::<Revision>(revision+1)?,"run_epoch":decimal::<Epoch>(epoch)?,"last_seq":decimal::<Seq>(seq)?,"status":status});
    save(
        &txn,
        actor,
        command,
        request.request_id,
        body,
        &request.room_id,
        &response,
    )
    .await?;
    txn.commit().await.map_err(storage_err)?;
    Ok(response)
}

async fn object_view(
    service: &RoundtableService,
    store: &RoundtableStore,
    actor: &ActorContext,
    room: &RoomId,
    object: ObjectRefV1,
    cursor: Option<String>,
) -> RtResult<Value> {
    let body = match object.kind {
        ObjectKind::Projection => {
            let id = object
                .object_id
                .parse()
                .map_err(|_| rt_error(ErrorCode::InvalidArgument, "projection_id"))?;
            json!(store.projection(room, Some(&id)).await?.body)
        }
        ObjectKind::Message | ObjectKind::Manifest => {
            let query = if object.kind == ObjectKind::Message {
                "SELECT body_json FROM rt_messages WHERE room_id=? AND message_id=?"
            } else {
                "SELECT body_json FROM rt_source_manifests WHERE room_id=? AND manifest_id=?"
            };
            let row = optional_row(
                store.connection(),
                query,
                vec![text(&room.to_string()), text(&object.object_id)],
            )
            .await?
            .ok_or_else(|| rt_error(ErrorCode::Forbidden, "not_found"))?;
            json_text(&column::<String>(&row, 0)?)?
        }
        ObjectKind::Diagnostic => {
            let row=optional_row(store.connection(),"SELECT json_object('finish_reason',finish_reason,'error_class',error_class,'assistant_prefix',assistant_prefix,'assistant_suffix',assistant_suffix,'total_bytes',CAST(total_bytes AS TEXT),'truncated',truncated,'cleanup_result',cleanup_result) FROM rt_diagnostics WHERE room_id=? AND diagnostic_id=? AND redacted=1",vec![text(&room.to_string()),text(&object.object_id)]).await?.ok_or_else(||rt_error(ErrorCode::Forbidden,"not_found"))?;
            json_text(&column::<String>(&row, 0)?)?
        }
        ObjectKind::SourceExcerpt => {
            if object.object_id != object.content_hash.to_hex() {
                return Err(rt_error(ErrorCode::InvalidArgument, "object_id"));
            }
            let manifests = rows(
                store.connection(),
                "SELECT body_json FROM rt_source_manifests WHERE room_id=?",
                vec![text(&room.to_string())],
            )
            .await?;
            let mut found = false;
            for manifest in manifests {
                let manifest: super::SourceManifestV1 =
                    decode(&json_text(&column::<String>(&manifest, 0)?)?)?;
                found |= manifest.entries.iter().any(|entry| {
                    entry.object.object_id == object.object_id
                        && entry.object.content_hash == object.content_hash
                        && entry.object.total_bytes == object.total_bytes.0
                });
            }
            if !found {
                return Err(rt_error(ErrorCode::Forbidden, "not_found"));
            }
            let path = service
                .data_dir
                .join("roundtable")
                .join("objects")
                .join(&object.object_id);
            let metadata = std::fs::symlink_metadata(&path)
                .map_err(|_| rt_error(ErrorCode::StorageUnavailable, "object_missing"))?;
            if !metadata.is_file()
                || metadata.file_type().is_symlink()
                || metadata.len() != object.total_bytes.0
                || metadata.len() > 16 * 1024 * 1024
            {
                return Err(rt_error(ErrorCode::StorageUnavailable, "object_shape"));
            }
            let bytes = std::fs::read(path)
                .map_err(|_| rt_error(ErrorCode::StorageUnavailable, "object_read"))?;
            if roundtable_protocol::Hash256::sha256(&bytes) != object.content_hash {
                return Err(rt_error(ErrorCode::StorageUnavailable, "object_hash"));
            }
            return paged_text(
                service,
                store,
                actor,
                room,
                &object,
                String::from_utf8(bytes)
                    .map_err(|_| rt_error(ErrorCode::InvalidArgument, "binary_source"))?,
                cursor,
            );
        }
    };
    let bytes = canonical_bytes(&body)?;
    if roundtable_protocol::Hash256::sha256(&bytes) != object.content_hash
        || bytes.len() as u64 != object.total_bytes.0
    {
        return Err(rt_error(ErrorCode::InvalidArgument, "object_reference"));
    }
    paged_text(
        service,
        store,
        actor,
        room,
        &object,
        String::from_utf8(bytes)
            .map_err(|_| rt_error(ErrorCode::StorageUnavailable, "object_body"))?,
        cursor,
    )
}

fn paged_text(
    service: &RoundtableService,
    store: &RoundtableStore,
    actor: &ActorContext,
    room: &RoomId,
    object: &ObjectRefV1,
    text: String,
    cursor: Option<String>,
) -> RtResult<Value> {
    // Bind non-UUID content-addressed objects to a UUID derived from their hash.
    let id = Uuid::parse_str(&object.content_hash.to_hex()[..32])
        .map_err(|_| rt_error(ErrorCode::InvalidArgument, "object_hash"))?
        .to_string();
    let scope = super::CursorScope {
        principal_id: actor.principal_id(),
        room_id: *room,
        projection_id: id
            .parse()
            .map_err(|_| rt_error(ErrorCode::InvalidArgument, "object_hash"))?,
        manifest_id: id
            .parse()
            .map_err(|_| rt_error(ErrorCode::InvalidArgument, "object_hash"))?,
    };
    let now = store.clock_sample().0;
    let start = match cursor {
        Some(cursor) => service
            .cursors
            .lock()
            .expect("cursors")
            .resolve(&cursor, &scope, now)?
            .parse::<usize>()
            .map_err(|_| rt_error(ErrorCode::InvalidArgument, "cursor"))?,
        None => 0,
    };
    if start > text.len() || !text.is_char_boundary(start) {
        return Err(rt_error(ErrorCode::InvalidArgument, "cursor"));
    }
    let mut end = start.saturating_add(32 * 1024).min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let next = if end < text.len() {
        Some(
            service
                .cursors
                .lock()
                .expect("cursors")
                .issue(scope, end.to_string(), now)?,
        )
    } else {
        None
    };
    Ok(json!({"object_ref":object,"offset":start,"text":&text[start..end],"cursor":next}))
}

fn normalize(command: &str, body: &Value) -> RtResult<Value> {
    macro_rules! dto {
        ($ty:ty) => {
            serde_json::to_value(decode::<$ty>(body)?)
                .map_err(|_| rt_error(ErrorCode::InvalidArgument, "request"))
        };
    }
    match command {
        "roundtable_preflight" => dto!(PreflightRequest),
        "roundtable_create" => dto!(CreateRequest),
        "roundtable_update_draft" => dto!(UpdateDraftRequest),
        "roundtable_start" => dto!(StartRequest),
        "roundtable_get" => dto!(GetRequest),
        "roundtable_list" => dto!(ListRequest),
        "roundtable_pause" => dto!(PauseRequest),
        "roundtable_resume" => dto!(ResumeRequest),
        "roundtable_stop" => dto!(StopRequest),
        "roundtable_interject" => dto!(InterjectRequest),
        "roundtable_retry_synthesis" => dto!(RetrySynthesisRequest),
        "roundtable_events" => dto!(EventsRequest),
        "roundtable_messages" => dto!(MessagesRequest),
        "roundtable_evidence" => dto!(EvidenceRequest),
        "roundtable_operation" => dto!(OperationRequest),
        "roundtable_clone" => dto!(CloneRequest),
        "roundtable_attach" => dto!(AttachRequest),
        "roundtable_detach" => dto!(DetachRequest),
        _ => Err(rt_error(ErrorCode::InvalidArgument, "command")),
    }
}
