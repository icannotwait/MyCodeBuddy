//! Atomic accept, close, and publish.
//!
//! A staged candidate is not accepted. Callers hear `accepted` only after the
//! write transaction commits. The accept decision reads the monotonic clock
//! after the semantic writes and before commit; equality with the deadline
//! rolls every row back. A later commit sample does not.

use std::str::FromStr;

use roundtable_protocol::{
    project, CleanupProof, Epoch, ErrorCode, Fence, Hash256, ManifestId, MessageId, PhaseId,
    PhaseKind, PhaseRefV1, ProjectionBodyV1, ProjectionId, ProjectionRef, ProjectionV1,
    PublishedMessageRef, Revision, RoomAggregate, RoomId, RuntimeTurnCompleted, Seq,
};
use sea_orm::{ConnectionTrait, DatabaseTransaction, TransactionTrait};
use serde_json::{json, Value};
use uuid::Uuid;

use super::clock::{deadline_reached, AcceptFaults, AcceptStep};
use super::rt_error;
use super::store::{
    column, exec, num, one_row, opt, optional_row, query_i64, rows, storage_err, text,
    RoundtableStore,
};
use roundtable_protocol::RtResult;

#[derive(Clone, Debug)]
pub struct AcceptInput {
    pub room_id: RoomId,
    pub completion: RuntimeTurnCompleted,
    pub candidate_id: String,
    pub fence: Fence,
    pub deadline_mono: u64,
    pub cleanup: CleanupProof,
    pub gate_held: bool,
}

#[derive(Clone, Debug)]
pub struct CloseInput {
    pub room_id: RoomId,
    pub phase_id: PhaseId,
    pub fence: Fence,
    pub deadline_mono: u64,
    pub cancel_unfinished: bool,
}

#[derive(Clone, Debug)]
pub struct PublishInput {
    pub room_id: RoomId,
    pub phase_id: PhaseId,
    pub fence: Fence,
    pub deadline_mono: u64,
    pub cleanup_confirmed: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClosingSetRef {
    pub phase_id: PhaseId,
    pub frozen: bool,
    pub closing_set_ref: Option<String>,
    pub phase_status: String,
    pub room_status: String,
    pub blocked_reason: Option<String>,
}

impl RoundtableStore {
    /// Caller owns the mutation transaction and has advanced revision/last_seq.
    pub(crate) async fn emit_current_in(
        &self,
        txn: &DatabaseTransaction,
        room: &str,
        cause: &str,
    ) -> RtResult<ProjectionRef> {
        let (now, utc) = self.clock_sample();
        self.emit(
            txn,
            &self.fault_snapshot(),
            room,
            (cause, &Uuid::new_v4().to_string()),
            (&[], &[]),
            (now, now, &utc),
        )
        .await
    }
    pub async fn accept(&self, input: AcceptInput) -> RtResult<ProjectionRef> {
        if let Some(gate) = self.fault_snapshot().lock_gate {
            gate.wait().await;
        }
        let txn = self.connection().begin().await.map_err(storage_err)?;
        match self.accept_in(&txn, &input).await {
            Ok(value) => {
                txn.commit().await.map_err(storage_err)?;
                Ok(value)
            }
            Err(err) => {
                let _ = txn.rollback().await;
                Err(err)
            }
        }
    }

    pub async fn close_phase(&self, input: CloseInput) -> RtResult<ClosingSetRef> {
        let txn = self.connection().begin().await.map_err(storage_err)?;
        match self.close_in(&txn, &input).await {
            Ok(value) => {
                txn.commit().await.map_err(storage_err)?;
                Ok(value)
            }
            Err(err) => {
                let _ = txn.rollback().await;
                Err(err)
            }
        }
    }

    pub async fn publish(&self, input: PublishInput) -> RtResult<ProjectionRef> {
        let txn = self.connection().begin().await.map_err(storage_err)?;
        match self.publish_in(&txn, &input).await {
            Ok(value) => {
                txn.commit().await.map_err(storage_err)?;
                Ok(value)
            }
            Err(err) => {
                let _ = txn.rollback().await;
                Err(err)
            }
        }
    }

    pub async fn projection(
        &self,
        room: &RoomId,
        id: Option<&ProjectionId>,
    ) -> RtResult<ProjectionV1> {
        let conn = self.connection();
        let projection = match id {
            Some(id) => {
                one_row(
                    conn,
                    "SELECT projection_id, projection_hash, body_json, seq
                     FROM rt_projection_versions
                     WHERE room_id = ? AND projection_id = ?",
                    vec![text(&room.to_string()), text(&id.to_string())],
                )
                .await?
            }
            None => {
                one_row(
                    conn,
                    "SELECT projection_id, projection_hash, body_json, seq
                     FROM rt_projection_versions
                     WHERE room_id = ?
                     ORDER BY seq DESC LIMIT 1",
                    vec![text(&room.to_string())],
                )
                .await?
            }
        };
        let projection_id: String = column(&projection, 0)?;
        let projection_hash: String = column(&projection, 1)?;
        let body_json: String = column(&projection, 2)?;
        let seq: i64 = column(&projection, 3)?;
        let event = one_row(
            conn,
            "SELECT seq, resulting_revision, changed_entities_json
             FROM rt_events WHERE room_id = ? AND projection_id = ?",
            vec![text(&room.to_string()), text(&projection_id)],
        )
        .await?;
        let event_seq: i64 = column(&event, 0)?;
        let resulting_revision: i64 = column(&event, 1)?;
        let changed: String = column(&event, 2)?;
        if event_seq != seq {
            return Err(rt_error(ErrorCode::InvalidState, "projection_seq"));
        }
        let body: ProjectionBodyV1 = serde_json::from_str(&body_json)
            .map_err(|_| rt_error(ErrorCode::InvalidState, "projection_body"))?;
        if body.room_id != *room || body.revision.0 != u64_from(resulting_revision)? {
            return Err(rt_error(ErrorCode::InvalidState, "projection_revision"));
        }
        if body.last_seq.0 != u64_from(seq)? {
            return Err(rt_error(ErrorCode::InvalidState, "projection_seq"));
        }
        let _ = changed;
        let projected = project(&RoomAggregate {
            projection_id: parse_id(&projection_id)?,
            body: body.clone(),
            messages: body.messages.clone(),
            evidence_manifests: body.evidence_manifests.clone(),
            required_message_ids: body.messages.iter().map(|item| item.message_id).collect(),
            required_evidence_manifests: body.evidence_manifests.clone(),
        })?;
        for message in &body.messages {
            let stored = query_text_local(
                conn,
                "SELECT body_hash FROM rt_messages WHERE room_id=? AND message_id=?",
                vec![
                    text(&room.to_string()),
                    text(&message.message_id.to_string()),
                ],
            )
            .await?;
            if stored != message.hash.to_hex() {
                return Err(rt_error(
                    ErrorCode::InvalidState,
                    "message_manifest_missing",
                ));
            }
        }
        for evidence in &body.replay.evidence {
            let row = one_row(
                conn,
                "SELECT body_json FROM rt_evidence WHERE room_id=? AND evidence_id=?",
                vec![
                    text(&room.to_string()),
                    text(&evidence.evidence_id.to_string()),
                ],
            )
            .await?;
            let raw: Option<String> = column(&row, 0)?;
            if evidence_body_hash(raw.as_deref())? != evidence.body_hash {
                return Err(rt_error(
                    ErrorCode::StorageUnavailable,
                    "evidence_body_changed",
                ));
            }
        }
        if projected.projection_ref.hash.to_hex() != projection_hash {
            return Err(rt_error(ErrorCode::InvalidState, "projection_hash"));
        }
        if Hash256::sha256(body_json.as_bytes()).to_hex() != projection_hash {
            return Err(rt_error(ErrorCode::InvalidState, "projection_hash"));
        }
        Ok(projected)
    }

    async fn accept_in(
        &self,
        txn: &DatabaseTransaction,
        input: &AcceptInput,
    ) -> RtResult<ProjectionRef> {
        let room_id = input.room_id.to_string();
        let attempt_id = input.fence.attempt_id.to_string();
        if !input.gate_held {
            return Err(rt_error(ErrorCode::InvalidState, "gate_not_held"));
        }
        if input.completion.fence != input.fence {
            return Err(rt_error(ErrorCode::InvalidState, "fence_mismatch"));
        }
        if input.completion.finish_reason != "completed" || !input.completion.tool_barrier.drained {
            return Err(rt_error(ErrorCode::InvalidState, "finish_not_normal"));
        }
        if input
            .completion
            .candidate_id
            .as_ref()
            .is_none_or(|candidate| candidate != &input.candidate_id)
        {
            return Err(rt_error(ErrorCode::InvalidState, "candidate_mismatch"));
        }
        if !cleanup_ready(&input.cleanup, &input.fence) {
            return Err(rt_error(ErrorCode::InvalidState, "cleanup_incomplete"));
        }
        let attempt = one_row(
            txn,
            "SELECT turn_id, binding_id, state FROM rt_attempts
             WHERE room_id = ? AND attempt_id = ?",
            vec![text(&room_id), text(&attempt_id)],
        )
        .await?;
        let turn_id: String = column(&attempt, 0)?;
        let binding_id: String = column(&attempt, 1)?;
        let state: String = column(&attempt, 2)?;
        if state == "accepted" {
            return self
                .replay_accept(txn, &room_id, &attempt_id, &input.candidate_id)
                .await;
        }
        if state != "validating" {
            return Err(rt_error(ErrorCode::InvalidState, "attempt_not_ready"));
        }
        let turn = one_row(
            txn,
            "SELECT phase_id, speaker_id, admitted_attempt_count FROM rt_turns
             WHERE room_id = ? AND turn_id = ?",
            vec![text(&room_id), text(&turn_id)],
        )
        .await?;
        let phase_id: String = column(&turn, 0)?;
        let speaker_id: String = column(&turn, 1)?;
        if phase_id != input.fence.phase_id.to_string()
            || binding_id != input.fence.binding_id.to_string()
        {
            return Err(rt_error(ErrorCode::InvalidState, "fence_mismatch"));
        }
        let room = room_row(txn, &room_id).await?;
        if room.status != "running" {
            return Err(rt_error(ErrorCode::InvalidState, "room_not_running"));
        }
        if room.run_epoch != i64_from(input.fence.run_epoch.0)?
            || room.boot_epoch != i64_from(input.fence.boot_epoch.0)?
        {
            return Err(rt_error(ErrorCode::InvalidState, "fence_mismatch"));
        }
        check_phase_fence(txn, &room_id, &phase_id, &input.fence).await?;
        let phase = phase_row(txn, &room_id, &phase_id).await?;
        if phase.status != "running" || phase.revision != i64_from(input.fence.phase_revision.0)? {
            return Err(rt_error(ErrorCode::InvalidState, "fence_mismatch"));
        }
        let binding = one_row(
            txn,
            "SELECT policy_ref, incarnation FROM rt_bindings
             WHERE room_id = ? AND binding_id = ?",
            vec![text(&room_id), text(&binding_id)],
        )
        .await?;
        let policy_ref: String = column(&binding, 0)?;
        let incarnation: String = column(&binding, 1)?;
        if policy_ref != input.fence.policy_hash.to_hex()
            || incarnation != input.fence.incarnation.to_string()
        {
            return Err(rt_error(ErrorCode::InvalidState, "fence_mismatch"));
        }
        let delivery_hash = query_text_local(
            txn,
            "SELECT delivery_hash FROM rt_attempts WHERE room_id = ? AND attempt_id = ?",
            vec![text(&room_id), text(&attempt_id)],
        )
        .await?;
        if delivery_hash != input.fence.context_hash.to_hex() {
            return Err(rt_error(ErrorCode::InvalidState, "fence_mismatch"));
        }
        let submission = optional_row(
            txn,
            "SELECT payload_hash, candidate_ref FROM rt_submissions
             WHERE room_id = ? AND attempt_id = ? AND sealed = 1",
            vec![text(&room_id), text(&attempt_id)],
        )
        .await?;
        let Some(submission) = submission else {
            return Err(rt_error(ErrorCode::InvalidState, "candidate_missing"));
        };
        let payload_hash: String = column(&submission, 0)?;
        let candidate: String = column(&submission, 1)?;
        let value: Value = serde_json::from_str(&candidate)
            .map_err(|_| rt_error(ErrorCode::InvalidState, "candidate_body"))?;
        let canonical = roundtable_protocol::canonical_bytes(&value)?;
        if canonical != candidate.as_bytes() || Hash256::sha256(&canonical).to_hex() != payload_hash
        {
            return Err(rt_error(ErrorCode::InvalidState, "candidate_hash"));
        }
        let scope_json = query_text_local(
            txn,
            "SELECT scope_json FROM rt_submission_scopes WHERE room_id=? AND attempt_id=?",
            vec![text(&room_id), text(&attempt_id)],
        )
        .await?;
        let scope: roundtable_protocol::ResultScope = serde_json::from_str(&scope_json)
            .map_err(|_| rt_error(ErrorCode::InvalidState, "candidate_scope"))?;
        if scope.speaker_id.to_string() != speaker_id
            || scope.phase_kind != phase_kind(&phase.snapshot_ref)
        {
            return Err(rt_error(ErrorCode::InvalidState, "candidate_scope"));
        }
        let validated = roundtable_protocol::validate_result(&canonical, &scope)
            .map_err(|_| rt_error(ErrorCode::InvalidState, "candidate_invalid"))?;
        if payload_hash != input.candidate_id {
            return Err(rt_error(ErrorCode::InvalidState, "candidate_mismatch"));
        }
        let faults = self.fault_snapshot();
        let message_id = Uuid::new_v4().to_string();
        trip(&faults, AcceptStep::Message)?;
        exec(
            txn,
            "INSERT INTO rt_messages (
                room_id, message_id, attempt_id, speaker_id, body_json, body_hash
             ) VALUES (?, ?, ?, ?, ?, ?)",
            vec![
                text(&room_id),
                text(&message_id),
                text(&attempt_id),
                text(&speaker_id),
                text(&candidate),
                text(&payload_hash),
            ],
        )
        .await?;
        let visibility = "staged";
        exec(
            txn,
            "INSERT INTO rt_message_memberships (
                room_id, message_id, membership_version, visibility, published_seq
             ) VALUES (?, ?, 1, ?, NULL)",
            vec![text(&room_id), text(&message_id), text(visibility)],
        )
        .await?;
        let receipt_json = Some(candidate.as_str());
        trip(&faults, AcceptStep::Claims)?;
        insert_claims(txn, &room_id, &message_id, receipt_json).await?;
        trip(&faults, AcceptStep::Responses)?;
        insert_responses(txn, &room_id, &message_id, receipt_json, &scope).await?;
        trip(&faults, AcceptStep::PositionChanges)?;
        insert_positions(txn, &room_id, &message_id, receipt_json, &scope).await?;
        trip(&faults, AcceptStep::Evidence)?;
        let evidence_ids =
            insert_evidence_links(txn, &room_id, &message_id, &attempt_id, &value, &scope).await?;
        trip(&faults, AcceptStep::Budget)?;
        exec(
            txn,
            "INSERT INTO rt_budget_reservations (
                room_id, reservation_id, principal_id, purpose, amount_ms, amount_bytes, state
             ) VALUES (?, ?, ?, 'accept', 0, 0, 'sampled')",
            vec![
                text(&room_id),
                text(&Uuid::new_v4().to_string()),
                text(&room.principal_id),
            ],
        )
        .await?;
        trip(&faults, AcceptStep::Revision)?;
        let bumped = bump_room(txn, &room_id).await?;
        let (decision_mono, utc) = self.clock_sample();
        if deadline_reached(decision_mono, input.deadline_mono) {
            return Err(rt_error(ErrorCode::InvalidState, "deadline_exceeded"));
        }
        let accepted = exec(
            txn,
            "UPDATE rt_attempts
             SET state = 'accepted', cleanup_state = 'confirmed', finished_at = ?, finish_reason = 'completed'
             WHERE room_id = ? AND attempt_id = ? AND state = 'validating'",
            vec![text(&utc), text(&room_id), text(&attempt_id)],
        )
        .await?;
        if accepted != 1 {
            return Err(rt_error(ErrorCode::InvalidState, "attempt_not_ready"));
        }
        let turned = exec(
            txn,
            "UPDATE rt_turns SET accepted_attempt_id = ?, status = ?
             WHERE room_id = ? AND turn_id = ? AND accepted_attempt_id IS NULL",
            vec![
                text(&attempt_id),
                text(if validated.abstained {
                    "abstained"
                } else {
                    "valid"
                }),
                text(&room_id),
                text(&turn_id),
            ],
        )
        .await?;
        if turned != 1 {
            return Err(rt_error(ErrorCode::InvalidState, "attempt_not_ready"));
        }
        let event_id = format!("accept-{attempt_id}");
        let projection = self
            .emit(
                txn,
                &faults,
                &room_id,
                ("accept", &event_id),
                (std::slice::from_ref(&message_id), &evidence_ids),
                (decision_mono, 0, &utc),
            )
            .await?;
        let (commit_mono, _) = self.clock_sample();
        exec(
            txn,
            "UPDATE rt_events SET changed_entities_json = ? WHERE room_id = ? AND event_id = ?",
            vec![
                text(&audit_json(
                    &attempt_id,
                    decision_mono,
                    commit_mono,
                    &[message_id],
                    &evidence_ids,
                    &phase.manifest_id,
                )),
                text(&room_id),
                text(&event_id),
            ],
        )
        .await?;
        let _ = bumped;
        trip(&faults, AcceptStep::Commit)?;
        Ok(projection)
    }

    async fn replay_accept(
        &self,
        txn: &DatabaseTransaction,
        room_id: &str,
        attempt_id: &str,
        candidate_id: &str,
    ) -> RtResult<ProjectionRef> {
        let hash = query_text_local(
            txn,
            "SELECT body_hash FROM rt_messages WHERE room_id = ? AND attempt_id = ?",
            vec![text(room_id), text(attempt_id)],
        )
        .await?;
        if hash != candidate_id {
            return Err(rt_error(
                ErrorCode::IdempotencyConflict,
                "submission_conflict",
            ));
        }
        let event = one_row(
            txn,
            "SELECT projection_id, projection_hash FROM rt_events
             WHERE room_id = ? AND event_id = ?",
            vec![text(room_id), text(&format!("accept-{attempt_id}"))],
        )
        .await?;
        let id: String = column(&event, 0)?;
        let projection_hash: String = column(&event, 1)?;
        Ok(ProjectionRef {
            id: parse_id(&id)?,
            hash: hash_hex(&projection_hash)?,
        })
    }

    async fn close_in(
        &self,
        txn: &DatabaseTransaction,
        input: &CloseInput,
    ) -> RtResult<ClosingSetRef> {
        let room_id = input.room_id.to_string();
        let phase_id = input.phase_id.to_string();
        check_phase_fence(txn, &room_id, &phase_id, &input.fence).await?;
        let phase = phase_row(txn, &room_id, &phase_id).await?;
        if phase.revision != i64_from(input.fence.phase_revision.0)? {
            return Err(rt_error(ErrorCode::InvalidState, "fence_mismatch"));
        }
        let (now, _) = self.clock_sample();
        if deadline_reached(now, input.deadline_mono) {
            exec(txn,"UPDATE rt_attempts SET state='timed_out',finish_reason='phase_timeout',cleanup_state='cleaning' WHERE room_id=? AND turn_id IN(SELECT turn_id FROM rt_turns WHERE room_id=? AND phase_id=?) AND state IN('reserved','launching','admitting','admitted','streaming','validating','active')",vec![text(&room_id),text(&room_id),text(&phase_id)]).await?;
        }
        let turns = query_i64(
            txn,
            "SELECT COUNT(*) FROM rt_turns WHERE room_id = ? AND phase_id = ?",
            vec![text(&room_id), text(&phase_id)],
        )
        .await?;
        let terminal = query_i64(
            txn,
            "SELECT COUNT(*) FROM rt_turns t
             JOIN rt_attempts a ON a.room_id = t.room_id AND a.turn_id = t.turn_id
             WHERE t.room_id = ? AND t.phase_id = ?
               AND a.attempt_no = (SELECT MAX(newer.attempt_no) FROM rt_attempts newer WHERE newer.room_id=a.room_id AND newer.turn_id=a.turn_id)
               AND a.state IN ('accepted','invalid','failed','timed_out','interrupted')",
            vec![text(&room_id), text(&phase_id)],
        )
        .await?;
        if terminal < turns
            || (turns < phase.expected && !deadline_reached(now, input.deadline_mono))
        {
            if input.cancel_unfinished {
                self.insert_cancel(txn, &room_id, &phase_id, phase.revision)
                    .await?;
            }
            return self.status_ref(txn, &room_id, &phase_id, false, None).await;
        }
        let accepted = query_i64(
            txn,
            "SELECT COUNT(*) FROM rt_turns t
             JOIN rt_attempts a ON a.room_id = t.room_id AND a.turn_id = t.turn_id
             WHERE t.room_id = ? AND t.phase_id = ? AND a.state = 'accepted' AND t.accepted_attempt_id=a.attempt_id AND t.status!='abstained'",
            vec![text(&room_id), text(&phase_id)],
        )
        .await?;
        let moderator_failed = query_i64(
            txn,
            "SELECT COUNT(*) FROM rt_turns t
             JOIN rt_attempts a ON a.room_id = t.room_id AND a.turn_id = t.turn_id
             JOIN rt_speakers s ON s.room_id = t.room_id AND s.speaker_id = t.speaker_id
             WHERE t.room_id = ? AND t.phase_id = ? AND s.role = 'moderator'
               AND a.attempt_no = (SELECT MAX(newer.attempt_no) FROM rt_attempts newer WHERE newer.room_id=a.room_id AND newer.turn_id=a.turn_id)
               AND a.state IN ('failed','timed_out','invalid','interrupted')",
            vec![text(&room_id), text(&phase_id)],
        )
        .await?;
        let closing_ref = if phase.snapshot_ref == "synthesis" && moderator_failed > 0 {
            exec(
                txn,
                "UPDATE rt_rooms SET status = 'paused', blocked_reason = 'synthesis_failed'
                 WHERE room_id = ?",
                vec![text(&room_id)],
            )
            .await?;
            exec(
                txn,
                "UPDATE rt_phases SET status = 'failed', closing_set_ref = 'synthesis_failed'
                 WHERE room_id = ? AND phase_id = ? AND status = 'running'",
                vec![text(&room_id), text(&phase_id)],
            )
            .await?;
            "synthesis_failed".to_string()
        } else if accepted < phase.quorum {
            exec(
                txn,
                "UPDATE rt_phases SET status = 'failed', closing_set_ref = 'phase_failed'
                 WHERE room_id = ? AND phase_id = ? AND status = 'running'",
                vec![text(&room_id), text(&phase_id)],
            )
            .await?;
            "phase_failed".to_string()
        } else {
            let frozen = self.accepted_attempt_list(txn, &room_id, &phase_id).await?;
            let updated = exec(
                txn,
                "UPDATE rt_phases SET status = 'closing', closing_set_ref = ?
                 WHERE room_id = ? AND phase_id = ? AND status = 'running' AND revision = ?",
                vec![
                    text(&frozen),
                    text(&room_id),
                    text(&phase_id),
                    num(phase.revision),
                ],
            )
            .await?;
            if updated != 1 {
                return Err(rt_error(ErrorCode::InvalidState, "close_conflict"));
            }
            frozen
        };
        let mut slots = Vec::new();
        for row in rows(txn,"SELECT t.turn_id,t.speaker_id,CASE WHEN t.accepted_attempt_id IS NOT NULL THEN t.status ELSE COALESCE((SELECT a.state FROM rt_attempts a WHERE a.room_id=t.room_id AND a.turn_id=t.turn_id ORDER BY a.attempt_no DESC LIMIT 1),'absent') END,t.accepted_attempt_id,m.message_id,m.body_hash FROM rt_turns t LEFT JOIN rt_messages m ON m.room_id=t.room_id AND m.attempt_id=t.accepted_attempt_id WHERE t.room_id=? AND t.phase_id=? ORDER BY t.turn_id",vec![text(&room_id),text(&phase_id)]).await? {
            slots.push(json!({"turn_id":column::<String>(&row,0)?,"speaker_id":column::<String>(&row,1)?,"outcome":column::<String>(&row,2)?,"attempt_id":column::<Option<String>>(&row,3)?,"message_id":column::<Option<String>>(&row,4)?,"body_hash":column::<Option<String>>(&row,5)?}));
        }
        let expected = if let Some(row) = optional_row(
            txn,
            "SELECT snapshot_json FROM rt_phase_contexts WHERE room_id=? AND phase_id=?",
            vec![text(&room_id), text(&phase_id)],
        )
        .await?
        {
            let snapshot: roundtable_protocol::PhaseSnapshotV1 =
                serde_json::from_str(&column::<String>(&row, 0)?)
                    .map_err(|_| rt_error(ErrorCode::StorageUnavailable, "phase_snapshot"))?;
            if snapshot.phase_id != input.phase_id
                || snapshot.members.len() as i64 != phase.expected
            {
                return Err(rt_error(ErrorCode::StorageUnavailable, "phase_members"));
            }
            snapshot
                .members
                .into_iter()
                .map(|member| member.speaker_id.to_string())
                .collect::<Vec<_>>()
        } else {
            // Legacy persisted phases lack a context record. Keep their
            // existing turns and recover missing members from the room roster.
            let members=rows(txn,"SELECT s.speaker_id FROM rt_speakers s WHERE s.room_id=? AND (s.role=? OR EXISTS(SELECT 1 FROM rt_turns t WHERE t.room_id=s.room_id AND t.speaker_id=s.speaker_id AND t.phase_id=?)) ORDER BY s.ordinal",vec![text(&room_id),text(if phase.snapshot_ref=="synthesis" {"moderator"} else {"member"}),text(&phase_id)]).await?;
            members
                .iter()
                .map(|row| column::<String>(row, 0))
                .collect::<RtResult<Vec<_>>>()?
        };
        let mut complete_slots = Vec::with_capacity(expected.len());
        for speaker in expected {
            let slot = if let Some(index) =
                slots.iter().position(|slot| slot["speaker_id"] == speaker)
            {
                slots.remove(index)
            } else {
                json!({"turn_id":null,"speaker_id":speaker,"outcome":"absent","attempt_id":null,"message_id":null,"body_hash":null})
            };
            complete_slots.push(slot);
        }
        if !slots.is_empty() {
            return Err(rt_error(ErrorCode::StorageUnavailable, "phase_members"));
        }
        let slots = complete_slots;
        let frozen=String::from_utf8(roundtable_protocol::canonical_bytes(&json!({"fence":input.fence,"context_hash":input.fence.context_hash,"accepted_attempts":closing_ref,"slots":slots}))?).map_err(|_|rt_error(ErrorCode::StorageUnavailable,"closing_set"))?;
        exec(
            txn,
            "INSERT INTO rt_closing_sets(room_id,phase_id,body_json) VALUES(?,?,?)",
            vec![text(&room_id), text(&phase_id), text(&frozen)],
        )
        .await?;
        let (now, utc) = self.clock_sample();
        let _ = bump_room(txn, &room_id).await?;
        let event_id = format!("close-{phase_id}");
        self.emit(
            txn,
            &self.fault_snapshot(),
            &room_id,
            ("close", &event_id),
            (&[], &[]),
            (now, now, &utc),
        )
        .await?;
        self.status_ref(txn, &room_id, &phase_id, true, Some(closing_ref))
            .await
    }

    pub(crate) async fn recover_frozen_in(
        &self,
        txn: &DatabaseTransaction,
        room_id: &str,
    ) -> RtResult<()> {
        let row=optional_row(txn,"SELECT p.phase_id,p.revision,a.attempt_id,a.binding_id,b.incarnation,b.policy_ref,a.delivery_hash,r.run_epoch,r.boot_epoch FROM rt_rooms r JOIN rt_phases p ON p.room_id=r.room_id AND p.phase_id=r.current_phase_id JOIN rt_turns t ON t.room_id=p.room_id AND t.phase_id=p.phase_id JOIN rt_attempts a ON a.room_id=t.room_id AND a.attempt_id=t.accepted_attempt_id JOIN rt_bindings b ON b.room_id=a.room_id AND b.binding_id=a.binding_id WHERE r.room_id=? AND r.active_control_id IS NULL AND p.status='closing' ORDER BY t.turn_id LIMIT 1",vec![text(room_id)]).await?;
        let Some(row) = row else {
            return Ok(());
        };
        let phase: String = column(&row, 0)?;
        let fence = Fence {
            phase_id: parse_id(&phase)?,
            phase_revision: Revision(u64_from(column(&row, 1)?)?),
            attempt_id: parse_id(&column::<String>(&row, 2)?)?,
            binding_id: parse_id(&column::<String>(&row, 3)?)?,
            incarnation: parse_id(&column::<String>(&row, 4)?)?,
            policy_hash: hash_hex(&column::<String>(&row, 5)?)?,
            context_hash: hash_hex(&column::<String>(&row, 6)?)?,
            run_epoch: Epoch(u64_from(column(&row, 7)?)?),
            boot_epoch: Epoch(u64_from(column(&row, 8)?)?),
        };
        let original = one_row(
            txn,
            "SELECT body_json FROM rt_closing_sets WHERE room_id=? AND phase_id=?",
            vec![text(room_id), text(&phase)],
        )
        .await?;
        let _: Value = serde_json::from_str(&column::<String>(&original, 0)?)
            .map_err(|_| rt_error(ErrorCode::StorageUnavailable, "closing_set"))?;
        let operation = Uuid::new_v4().to_string();
        exec(txn,"INSERT INTO rt_control_operations(room_id,operation_id,kind,target_phase_id,target_revision,requested_epoch,step,status) VALUES(?,?,'recover',?,?,?,'done','completed')",vec![text(room_id),text(&operation),text(&phase),num(i64_from(fence.phase_revision.0)?),num(i64_from(fence.run_epoch.0)?)]).await?;
        exec(txn,"UPDATE rt_closing_sets SET recovery_boot_epoch=?,recovery_operation_id=? WHERE room_id=? AND phase_id=?",vec![num(i64_from(fence.boot_epoch.0)?),text(&operation),text(room_id),text(&phase)]).await?;
        exec(
            txn,
            "UPDATE rt_rooms SET status='running' WHERE room_id=?",
            vec![text(room_id)],
        )
        .await?;
        self.publish_in(
            txn,
            &PublishInput {
                room_id: parse_id(room_id)?,
                phase_id: parse_id(&phase)?,
                fence,
                deadline_mono: u64::MAX,
                cleanup_confirmed: true,
            },
        )
        .await?;
        Ok(())
    }

    async fn publish_in(
        &self,
        txn: &DatabaseTransaction,
        input: &PublishInput,
    ) -> RtResult<ProjectionRef> {
        if !input.cleanup_confirmed {
            return Err(rt_error(ErrorCode::InvalidState, "cleanup_incomplete"));
        }
        let room_id = input.room_id.to_string();
        let phase_id = input.phase_id.to_string();
        check_phase_fence(txn, &room_id, &phase_id, &input.fence).await?;
        let phase = phase_row(txn, &room_id, &phase_id).await?;
        if phase.status != "closing" {
            return Err(rt_error(ErrorCode::InvalidState, "phase_not_closing"));
        }
        let frozen = query_text_local(
            txn,
            "SELECT closing_set_ref FROM rt_phases WHERE room_id=? AND phase_id=?",
            vec![text(&room_id), text(&phase_id)],
        )
        .await?;
        let set=one_row(txn,"SELECT body_json,recovery_boot_epoch,recovery_operation_id FROM rt_closing_sets WHERE room_id=? AND phase_id=?",vec![text(&room_id),text(&phase_id)]).await?;
        let raw: String = column(&set, 0)?;
        let document: Value = serde_json::from_str(&raw)
            .map_err(|_| rt_error(ErrorCode::StorageUnavailable, "closing_set"))?;
        let original: Fence = serde_json::from_value(document["fence"].clone())
            .map_err(|_| rt_error(ErrorCode::StorageUnavailable, "closing_fence"))?;
        if document["accepted_attempts"].as_str() != Some(frozen.as_str()) {
            return Err(rt_error(ErrorCode::InvalidState, "closing_set_changed"));
        }
        if original != input.fence {
            let boot: Option<i64> = column(&set, 1)?;
            let operation: Option<String> = column(&set, 2)?;
            let operation =
                operation.ok_or_else(|| rt_error(ErrorCode::InvalidState, "closing_fence"))?;
            if boot!=Some(i64_from(input.fence.boot_epoch.0)?) || query_i64(txn,"SELECT COUNT(*) FROM rt_control_operations WHERE room_id=? AND operation_id=? AND kind='recover' AND requested_epoch=? AND step='done'",vec![text(&room_id),text(&operation),num(i64_from(input.fence.run_epoch.0)?)]).await?!=1 {return Err(rt_error(ErrorCode::InvalidState,"closing_fence"));}
        }
        for slot in document["slots"]
            .as_array()
            .ok_or_else(|| rt_error(ErrorCode::StorageUnavailable, "closing_set"))?
        {
            if let Some(id) = slot["message_id"].as_str() {
                let row = one_row(
                    txn,
                    "SELECT body_json,body_hash FROM rt_messages WHERE room_id=? AND message_id=?",
                    vec![text(&room_id), text(id)],
                )
                .await?;
                let body: String = column(&row, 0)?;
                let hash: String = column(&row, 1)?;
                if slot["body_hash"].as_str() != Some(hash.as_str())
                    || Hash256::sha256(body.as_bytes()).to_hex() != hash
                {
                    return Err(rt_error(
                        ErrorCode::StorageUnavailable,
                        "closing_message_changed",
                    ));
                }
            }
        }
        if frozen != self.accepted_attempt_list(txn, &room_id, &phase_id).await? {
            return Err(rt_error(ErrorCode::InvalidState, "closing_set_changed"));
        }
        if query_i64(txn,"SELECT COUNT(*) FROM rt_attempts a JOIN rt_turns t ON t.room_id=a.room_id AND t.turn_id=a.turn_id WHERE t.room_id=? AND t.phase_id=? AND a.cleanup_state!='confirmed'",vec![text(&room_id),text(&phase_id)]).await? != 0 {return Err(rt_error(ErrorCode::InvalidState,"cleanup_incomplete"));}
        let accepted = query_i64(
            txn,
            "SELECT COUNT(*) FROM rt_turns t
             JOIN rt_attempts a ON a.room_id = t.room_id AND a.turn_id = t.turn_id
             WHERE t.room_id = ? AND t.phase_id = ? AND a.state = 'accepted' AND t.accepted_attempt_id=a.attempt_id AND t.status!='abstained'
               AND a.cleanup_state = 'confirmed'",
            vec![text(&room_id), text(&phase_id)],
        )
        .await?;
        if accepted < phase.quorum {
            return Err(rt_error(ErrorCode::InvalidState, "coverage_short"));
        }
        let ordinals = rows(
            txn,
            "SELECT msg.message_id, s.ordinal
             FROM rt_messages msg
             JOIN rt_attempts a ON a.room_id = msg.room_id AND a.attempt_id = msg.attempt_id
             JOIN rt_turns t ON t.room_id = a.room_id AND t.turn_id = a.turn_id
             JOIN rt_speakers s ON s.room_id = msg.room_id AND s.speaker_id = msg.speaker_id
             WHERE msg.room_id = ? AND t.phase_id = ? AND a.state = 'accepted' AND t.accepted_attempt_id=a.attempt_id
             ORDER BY s.ordinal ASC",
            vec![text(&room_id), text(&phase_id)],
        )
        .await?;
        let bumped = bump_room(txn, &room_id).await?;
        let mut message_ids = Vec::new();
        for row in &ordinals {
            let message_id: String = column(row, 0)?;

            exec(
                txn,
                "INSERT INTO rt_message_memberships(room_id,message_id,membership_version,visibility,published_seq) SELECT room_id,message_id,MAX(membership_version)+1,'published',? FROM rt_message_memberships WHERE room_id=? AND message_id=? GROUP BY room_id,message_id",
                vec![num(bumped.seq), text(&room_id), text(&message_id)],
            )
            .await?;
            message_ids.push(message_id);
        }
        exec(
            txn,
            "UPDATE rt_phases SET status = 'published', published_seq = ?
             WHERE room_id = ? AND phase_id = ? AND status = 'closing'",
            vec![num(bumped.seq), text(&room_id), text(&phase_id)],
        )
        .await?;
        exec(
            txn,
            "UPDATE rt_evidence SET publish_seq = ?
             WHERE room_id = ? AND publish_seq IS NULL AND evidence_id IN (
                 SELECT me.evidence_id FROM rt_message_evidence me JOIN rt_messages m ON m.room_id=me.room_id AND m.message_id=me.message_id JOIN rt_attempts a ON a.room_id=m.room_id AND a.attempt_id=m.attempt_id JOIN rt_turns t ON t.room_id=a.room_id AND t.turn_id=a.turn_id WHERE t.room_id=? AND t.phase_id=? AND a.state='accepted'
             )",
            vec![num(bumped.seq), text(&room_id), text(&room_id), text(&phase_id)],
        )
        .await?;
        if phase.snapshot_ref == "synthesis" {
            exec(
                txn,
                "UPDATE rt_rooms SET status = 'completed' WHERE room_id = ?",
                vec![text(&room_id)],
            )
            .await?;
        }
        let (now, utc) = self.clock_sample();
        self.emit(
            txn,
            &self.fault_snapshot(),
            &room_id,
            ("publish", &format!("publish-{phase_id}")),
            (&message_ids, &[]),
            (now, now, &utc),
        )
        .await
    }

    async fn emit(
        &self,
        txn: &DatabaseTransaction,
        faults: &AcceptFaults,
        room_id: &str,
        event: (&str, &str),
        entities: (&[String], &[String]),
        timing: (u64, u64, &str),
    ) -> RtResult<ProjectionRef> {
        let (cause, event_id) = event;
        let (message_ids, evidence_ids) = entities;
        let (decision_mono, commit_mono, utc) = timing;
        let room = room_row(txn, room_id).await?;
        let phase_rows = rows(
            txn,
            "SELECT phase_id, phase_index, revision, status, snapshot_ref
             FROM rt_phases WHERE room_id = ? ORDER BY phase_index ASC, revision ASC",
            vec![text(room_id)],
        )
        .await?;
        let mut phase_refs = Vec::new();
        let mut manifest_ids = Vec::new();
        for row in &phase_rows {
            let phase_id: String = column(row, 0)?;
            let index: i64 = column(row, 1)?;
            let revision: i64 = column(row, 2)?;
            let status: String = column(row, 3)?;
            let snapshot_ref: String = column(row, 4)?;
            phase_refs.push(PhaseRefV1 {
                phase_id: parse_id(&phase_id)?,
                index: u32::try_from(index)
                    .map_err(|_| rt_error(ErrorCode::InvalidArgument, "phase_index"))?,
                revision: Revision(u64_from(revision)?),
                kind: phase_kind(&snapshot_ref),
                state: parse_snake(&status)?,
            });
        }
        for row in rows(
            txn,
            "SELECT manifest_id FROM rt_source_manifests WHERE room_id=? ORDER BY version",
            vec![text(room_id)],
        )
        .await?
        {
            manifest_ids.push(column::<String>(&row, 0)?);
        }
        let moderator = optional_row(
            txn,
            "SELECT speaker_id FROM rt_speakers WHERE room_id = ? AND role = 'moderator'",
            vec![text(room_id)],
        )
        .await?;
        let moderator_speaker_id = match moderator {
            Some(row) => {
                let speaker: String = column(&row, 0)?;
                Some(parse_id(&speaker)?)
            }
            None => None,
        };
        let remaining = query_i64(
            txn,
            "SELECT remaining_active_ms FROM rt_rooms WHERE room_id=?",
            vec![text(room_id)],
        )
        .await?
        .max(0) as u64;
        let config: Option<roundtable_protocol::RoundtableConfigV1> =
            serde_json::from_str(&room.config_ref).ok();
        let prepaid=query_i64(txn,"SELECT COALESCE(SUM(prepaid_ms),0) FROM rt_active_time_leases WHERE room_id=? AND boot_epoch=? AND run_epoch=?",vec![text(room_id),num(room.boot_epoch),num(room.run_epoch)]).await?.max(0) as u64;
        let sampled = config
            .map(|config| {
                config
                    .budgets
                    .room_budget
                    .0
                    .saturating_sub(remaining.saturating_add(prepaid))
            })
            .unwrap_or(0);
        let ledger_seq = query_i64(
            txn,
            "SELECT COALESCE(MAX(ledger_seq),0)+1 FROM rt_measurements WHERE room_id=?",
            vec![text(room_id)],
        )
        .await?;
        exec(txn,"INSERT INTO rt_measurements(room_id,measurement_id,ledger_seq,sampled_active_ms,sampled_at_utc,kind) VALUES(?,?,?,?,?,'active')",vec![text(room_id),text(&Uuid::new_v4().to_string()),num(ledger_seq),num(i64_from(sampled)?),text(utc)]).await?;
        let projection_id = Uuid::new_v4().to_string();
        let body = ProjectionBodyV1 {
            schema_version: 1,
            room_id: parse_id(room_id)?,
            revision: Revision(u64_from(room.revision)?),
            run_epoch: Epoch(u64_from(room.run_epoch)?),
            last_seq: Seq(u64_from(room.last_seq)?),
            status: parse_snake(&room.status)?,
            blocked_reason: match room.blocked_reason.as_deref() {
                Some(reason) => Some(parse_snake(reason)?),
                None => None,
            },
            config_hash: Hash256::sha256(room.config_ref.as_bytes()),
            moderator_speaker_id,
            phase_refs,
            messages: Vec::new(),
            evidence_manifests: Vec::new(),
            replay: load_replay(txn, room_id, &room).await?,
            ledger_seq: Seq(u64_from(ledger_seq)?),
            sampled_active_ms: roundtable_protocol::DurationMs(sampled),
            sampled_at_utc: utc.to_string(),
        };
        let message_rows = rows(
            txn,
            "SELECT message_id,body_hash FROM rt_messages WHERE room_id=? ORDER BY message_id",
            vec![text(room_id)],
        )
        .await?;
        let mut messages = Vec::new();
        for row in message_rows {
            let id: String = column(&row, 0)?;
            let hash: String = column(&row, 1)?;
            messages.push(PublishedMessageRef {
                message_id: parse_id(&id)?,
                hash: hash_hex(&hash)?,
            });
        }
        let evidence_manifests = manifest_ids
            .iter()
            .map(|item| parse_id(item))
            .collect::<RtResult<Vec<ManifestId>>>()?;
        let aggregate = RoomAggregate {
            projection_id: parse_id(&projection_id)?,
            body,
            messages,
            evidence_manifests: evidence_manifests.clone(),
            required_message_ids: message_ids
                .iter()
                .map(|item| parse_id(item))
                .collect::<RtResult<Vec<MessageId>>>()?,
            required_evidence_manifests: evidence_manifests,
        };
        let projected = project(&aggregate)?;
        let body_json = String::from_utf8(roundtable_protocol::canonical_bytes(&projected.body)?)
            .map_err(|_| rt_error(ErrorCode::InvalidArgument, "projection_body"))?;
        trip(faults, AcceptStep::Projection)?;
        exec(
            txn,
            "INSERT INTO rt_projection_versions (
                room_id, projection_id, seq, projection_hash, schema_version, body_json
             ) VALUES (?, ?, ?, ?, 1, ?)",
            vec![
                text(room_id),
                text(&projection_id),
                num(room.last_seq),
                text(&projected.projection_ref.hash.to_hex()),
                text(&body_json),
            ],
        )
        .await?;
        trip(faults, AcceptStep::Event)?;
        exec(txn, "INSERT INTO rt_page_manifests(room_id,manifest_id,projection_id,high_water_seq) VALUES(?,?,?,?)",
            vec![text(room_id),text(&projection_id),text(&projection_id),num(room.last_seq)]).await?;
        for (offset, message) in projected.body.messages.iter().enumerate() {
            let member = projected
                .body
                .replay
                .message_memberships
                .iter()
                .filter(|member| member.message_id == message.message_id)
                .max_by_key(|member| member.membership_version);
            let (version, visibility) = member
                .map(|member| (member.membership_version.0, member.visibility))
                .unwrap_or((1, roundtable_protocol::MessageVisibility::Staged));
            let visibility = match visibility {
                roundtable_protocol::MessageVisibility::Staged => "staged",
                roundtable_protocol::MessageVisibility::Published => "published",
                roundtable_protocol::MessageVisibility::Void => "void",
            };
            exec(txn,"INSERT INTO rt_page_manifest_entries(room_id,manifest_id,entry_offset,message_id,body_hash,membership_version,visibility,staged) VALUES(?,?,?,?,?,?,?,?)",
                vec![text(room_id),text(&projection_id),num(i64_from(offset as u64)?),text(&message.message_id.to_string()),text(&message.hash.to_hex()),num(i64_from(version)?),text(visibility),num(i64::from(visibility=="staged"))]).await?;
        }
        exec(
            txn,
            "INSERT INTO rt_events (
                room_id, seq, event_id, schema_version, resulting_revision, run_epoch, boot_epoch,
                occurred_at, projection_id, projection_hash, cause, changed_entities_json
             ) VALUES (?, ?, ?, 1, ?, ?, ?, ?, ?, ?, ?, ?)",
            vec![
                text(room_id),
                num(room.last_seq),
                text(event_id),
                num(room.revision),
                num(room.run_epoch),
                num(room.boot_epoch),
                text(utc),
                text(&projection_id),
                text(&projected.projection_ref.hash.to_hex()),
                text(cause),
                text(&audit_json(
                    event_id,
                    decision_mono,
                    commit_mono,
                    message_ids,
                    evidence_ids,
                    &manifest_ids.first().cloned(),
                )),
            ],
        )
        .await?;
        Ok(projected.projection_ref)
    }

    async fn insert_cancel(
        &self,
        txn: &DatabaseTransaction,
        room_id: &str,
        phase_id: &str,
        revision: i64,
    ) -> RtResult<()> {
        let epoch = query_i64(
            txn,
            "SELECT run_epoch FROM rt_rooms WHERE room_id = ?",
            vec![text(room_id)],
        )
        .await?;
        exec(
            txn,
            "INSERT INTO rt_control_operations (
                room_id, operation_id, kind, target_phase_id, target_revision, requested_epoch,
                step, status, blocked_reason, superseded_by, result_ref, budget_reservation_id,
                successor_phase_id
             ) VALUES (?, ?, 'stop', ?, ?, ?, 'requested', 'open', NULL, NULL, NULL, NULL, NULL)",
            vec![
                text(room_id),
                text(&Uuid::new_v4().to_string()),
                text(phase_id),
                num(revision),
                num(epoch),
            ],
        )
        .await?;
        Ok(())
    }

    async fn accepted_attempt_list(
        &self,
        txn: &DatabaseTransaction,
        room_id: &str,
        phase_id: &str,
    ) -> RtResult<String> {
        let listed = rows(
            txn,
            "SELECT a.attempt_id FROM rt_attempts a
             JOIN rt_turns t ON t.room_id = a.room_id AND t.turn_id = a.turn_id
             JOIN rt_speakers s ON s.room_id = t.room_id AND s.speaker_id = t.speaker_id
             WHERE a.room_id = ? AND t.phase_id = ? AND a.state = 'accepted' AND t.accepted_attempt_id=a.attempt_id
             ORDER BY s.ordinal ASC",
            vec![text(room_id), text(phase_id)],
        )
        .await?;
        let mut ids = Vec::new();
        for row in listed {
            ids.push(column::<String>(&row, 0)?);
        }
        Ok(ids.join(","))
    }

    async fn status_ref(
        &self,
        txn: &DatabaseTransaction,
        room_id: &str,
        phase_id: &str,
        frozen: bool,
        closing_set_ref: Option<String>,
    ) -> RtResult<ClosingSetRef> {
        let phase = query_text_local(
            txn,
            "SELECT status FROM rt_phases WHERE room_id = ? AND phase_id = ?",
            vec![text(room_id), text(phase_id)],
        )
        .await?;
        let room = room_row(txn, room_id).await?;
        Ok(ClosingSetRef {
            phase_id: parse_id(phase_id)?,
            frozen,
            closing_set_ref,
            phase_status: phase,
            room_status: room.status,
            blocked_reason: room.blocked_reason,
        })
    }
}

struct RoomSnap {
    principal_id: String,
    status: String,
    config_ref: String,
    revision: i64,
    run_epoch: i64,
    boot_epoch: i64,
    last_seq: i64,
    blocked_reason: Option<String>,
}

struct PhaseSnap {
    revision: i64,
    status: String,
    snapshot_ref: String,
    expected: i64,
    quorum: i64,
    manifest_id: Option<String>,
}

struct Bump {
    seq: i64,
}

async fn room_row(txn: &impl ConnectionTrait, room_id: &str) -> RtResult<RoomSnap> {
    let row = one_row(
        txn,
        "SELECT principal_id, status, config_ref, revision, run_epoch, boot_epoch, last_seq, blocked_reason
         FROM rt_rooms WHERE room_id = ?",
        vec![text(room_id)],
    )
    .await?;
    Ok(RoomSnap {
        principal_id: column(&row, 0)?,
        status: column(&row, 1)?,
        config_ref: column(&row, 2)?,
        revision: column(&row, 3)?,
        run_epoch: column(&row, 4)?,
        boot_epoch: column(&row, 5)?,
        last_seq: column(&row, 6)?,
        blocked_reason: column(&row, 7).unwrap_or(None),
    })
}

async fn phase_row(
    txn: &impl ConnectionTrait,
    room_id: &str,
    phase_id: &str,
) -> RtResult<PhaseSnap> {
    let row = one_row(
        txn,
        "SELECT revision, status, phase_index, snapshot_ref, snapshot_hash, expected, quorum,
                remaining_ms, manifest_id
         FROM rt_phases WHERE room_id = ? AND phase_id = ?",
        vec![text(room_id), text(phase_id)],
    )
    .await?;
    Ok(PhaseSnap {
        revision: column(&row, 0)?,
        status: column(&row, 1)?,
        snapshot_ref: column(&row, 3)?,
        expected: column(&row, 5)?,
        quorum: column(&row, 6)?,
        manifest_id: column(&row, 8).unwrap_or(None),
    })
}

async fn bump_room(txn: &impl ConnectionTrait, room_id: &str) -> RtResult<Bump> {
    let updated = exec(
        txn,
        "UPDATE rt_rooms SET revision = revision + 1, last_seq = last_seq + 1 WHERE room_id = ?",
        vec![text(room_id)],
    )
    .await?;
    if updated != 1 {
        return Err(rt_error(ErrorCode::InvalidState, "room_missing"));
    }
    let seq = query_i64(
        txn,
        "SELECT last_seq FROM rt_rooms WHERE room_id = ?",
        vec![text(room_id)],
    )
    .await?;
    Ok(Bump { seq })
}

async fn check_phase_fence(
    txn: &impl ConnectionTrait,
    room: &str,
    phase: &str,
    fence: &Fence,
) -> RtResult<()> {
    if phase != fence.phase_id.to_string() {
        return Err(rt_error(ErrorCode::InvalidState, "fence_mismatch"));
    }
    let valid=query_i64(txn,"SELECT COUNT(*) FROM rt_rooms r JOIN rt_phases p ON p.room_id=r.room_id AND p.phase_id=r.current_phase_id JOIN rt_turns t ON t.room_id=p.room_id AND t.phase_id=p.phase_id JOIN rt_attempts a ON a.room_id=t.room_id AND a.turn_id=t.turn_id JOIN rt_bindings b ON b.room_id=a.room_id AND b.binding_id=a.binding_id WHERE r.room_id=? AND r.status='running' AND r.active_control_id IS NULL AND r.boot_epoch=? AND r.run_epoch=? AND p.phase_id=? AND p.revision=? AND a.attempt_id=? AND b.binding_id=? AND b.incarnation=? AND b.policy_ref=? AND a.delivery_hash=?",vec![text(room),num(i64_from(fence.boot_epoch.0)?),num(i64_from(fence.run_epoch.0)?),text(phase),num(i64_from(fence.phase_revision.0)?),text(&fence.attempt_id.to_string()),text(&fence.binding_id.to_string()),text(&fence.incarnation.to_string()),text(&fence.policy_hash.to_hex()),text(&fence.context_hash.to_hex())]).await?;
    if valid != 1 {
        return Err(rt_error(ErrorCode::InvalidState, "fence_mismatch"));
    }
    Ok(())
}

async fn insert_claims(
    txn: &DatabaseTransaction,
    room: &str,
    message: &str,
    body: Option<&str>,
) -> RtResult<()> {
    for claim in receipt_array(body, "claims") {
        let key = claim_str(&claim, "local_key")?;
        let statement = claim_str(&claim, "text")?;
        exec(
            txn,
            "INSERT INTO rt_claims(room_id,message_id,local_key,statement) VALUES(?,?,?,?)",
            vec![text(room), text(message), text(&key), text(&statement)],
        )
        .await?;
        exec(
            txn,
            "INSERT INTO rt_claim_ids(room_id,claim_id,message_id,local_key) VALUES(?,?,?,?)",
            vec![
                text(room),
                text(&Uuid::new_v4().to_string()),
                text(message),
                text(&key),
            ],
        )
        .await?;
    }
    Ok(())
}

async fn claim_target(
    txn: &DatabaseTransaction,
    room: &str,
    alias: &str,
    scope: &roundtable_protocol::ResultScope,
) -> RtResult<(String, String)> {
    let target = scope
        .aliases
        .claims
        .get(alias)
        .ok_or_else(|| rt_error(ErrorCode::InvalidState, "claim_alias"))?;
    let row=one_row(txn,"SELECT c.message_id,c.local_key FROM rt_claim_ids c WHERE c.room_id=? AND c.claim_id=? AND EXISTS(SELECT 1 FROM rt_message_memberships m WHERE m.room_id=c.room_id AND m.message_id=c.message_id AND m.visibility='published')",vec![text(room),text(&target.claim_id.to_string())]).await?;
    Ok((column(&row, 0)?, column(&row, 1)?))
}

async fn insert_responses(
    txn: &DatabaseTransaction,
    room: &str,
    message: &str,
    body: Option<&str>,
    scope: &roundtable_protocol::ResultScope,
) -> RtResult<()> {
    for response in receipt_array(body, "responses") {
        let mut response_target = None;
        let (target_message, target_key) = if let Some(alias) =
            response.get("target_claim_alias").and_then(Value::as_str)
        {
            claim_target(txn, room, alias, scope).await?
        } else {
            let alias = claim_str(&response, "target_response_alias")?;
            let target = scope
                .aliases
                .responses
                .get(&alias)
                .ok_or_else(|| rt_error(ErrorCode::InvalidState, "response_alias"))?;
            response_target = Some(target.response_id.to_string());
            let row=one_row(txn,"SELECT r.target_message_id,r.target_local_key FROM rt_responses r WHERE r.room_id=? AND r.response_id=? AND EXISTS(SELECT 1 FROM rt_message_memberships m WHERE m.room_id=r.room_id AND m.message_id=r.message_id AND m.visibility='published')",vec![text(room),text(&target.response_id.to_string())]).await?;
            (column::<String>(&row, 0)?, column::<String>(&row, 1)?)
        };
        let id = Uuid::new_v4().to_string();
        exec(txn,"INSERT INTO rt_responses(room_id,response_id,message_id,target_message_id,target_local_key,stance) VALUES(?,?,?,?,?,?)",vec![text(room),text(&id),text(message),text(&target_message),text(&target_key),text(&claim_str(&response,"stance")?)]).await?;
        exec(txn,"INSERT INTO rt_response_details(room_id,response_id,target_response_id,body_json) VALUES(?,?,?,?)",vec![text(room),text(&id),opt(response_target.as_deref()),text(&response.to_string())]).await?;
    }
    Ok(())
}

async fn insert_positions(
    txn: &DatabaseTransaction,
    room: &str,
    message: &str,
    body: Option<&str>,
    scope: &roundtable_protocol::ResultScope,
) -> RtResult<()> {
    for change in receipt_array(body, "position_changes") {
        let alias = claim_str(&change, "own_prior_claim_alias")?;
        if scope
            .aliases
            .claims
            .get(&alias)
            .is_none_or(|claim| claim.speaker_id != scope.speaker_id)
        {
            return Err(rt_error(ErrorCode::InvalidState, "claim_owner"));
        }
        let (target_message, target_key) = claim_target(txn, room, &alias, scope).await?;
        let old = query_text_local(
            txn,
            "SELECT statement FROM rt_claims WHERE room_id=? AND message_id=? AND local_key=?",
            vec![text(room), text(&target_message), text(&target_key)],
        )
        .await?;
        let new = query_text_local(
            txn,
            "SELECT statement FROM rt_claims WHERE room_id=? AND message_id=? AND local_key=?",
            vec![
                text(room),
                text(message),
                text(&claim_str(&change, "new_local_claim_key")?),
            ],
        )
        .await?;
        exec(txn,"INSERT INTO rt_position_changes(room_id,position_change_id,message_id,target_message_id,target_local_key,from_position,to_position) VALUES(?,?,?,?,?,?,?)",vec![text(room),text(&Uuid::new_v4().to_string()),text(message),text(&target_message),text(&target_key),text(&old),text(&new)]).await?;
    }
    Ok(())
}

fn evidence_aliases(value: &Value, aliases: &mut std::collections::BTreeSet<String>) {
    match value {
        Value::Object(map) => {
            if let Some(items) = map.get("evidence_aliases").and_then(Value::as_array) {
                for item in items {
                    if let Some(alias) = item.as_str() {
                        aliases.insert(alias.to_owned());
                    }
                }
            }
            if map.get("kind").and_then(Value::as_str) == Some("evidence") {
                if let Some(alias) = map.get("alias").and_then(Value::as_str) {
                    aliases.insert(alias.to_owned());
                }
            }
            for child in map.values() {
                evidence_aliases(child, aliases);
            }
        }
        Value::Array(items) => {
            for item in items {
                evidence_aliases(item, aliases);
            }
        }
        _ => {}
    }
}

async fn insert_evidence_links(
    txn: &DatabaseTransaction,
    room: &str,
    message: &str,
    attempt: &str,
    body: &Value,
    scope: &roundtable_protocol::ResultScope,
) -> RtResult<Vec<String>> {
    let mut aliases = std::collections::BTreeSet::new();
    evidence_aliases(body, &mut aliases);
    let mut ids = Vec::new();
    for alias in aliases {
        let target = scope
            .aliases
            .evidence
            .get(&alias)
            .ok_or_else(|| rt_error(ErrorCode::InvalidState, "evidence_alias"))?;
        let id = target.evidence_id.to_string();
        let row = one_row(
            txn,
            "SELECT publish_seq,body_json FROM rt_evidence WHERE room_id=? AND evidence_id=?",
            vec![text(room), text(&id)],
        )
        .await?;
        let published: Option<i64> = column(&row, 0)?;
        let raw: Option<String> = column(&row, 1)?;
        let evidence: Value = serde_json::from_str(raw.as_deref().unwrap_or("null"))
            .map_err(|_| rt_error(ErrorCode::InvalidState, "evidence_body"))?;
        if published.is_none()
            && evidence.get("owner_attempt_id").and_then(Value::as_str) != Some(attempt)
        {
            return Err(rt_error(ErrorCode::InvalidState, "evidence_visibility"));
        }
        exec(
            txn,
            "INSERT INTO rt_message_evidence(room_id,message_id,evidence_id) VALUES(?,?,?)",
            vec![text(room), text(message), text(&id)],
        )
        .await?;
        ids.push(id);
    }
    Ok(ids)
}

fn receipt_array(receipt: Option<&str>, key: &str) -> Vec<Value> {
    let Some(text) = receipt else {
        return Vec::new();
    };
    let Ok(value) = serde_json::from_str::<Value>(text) else {
        return Vec::new();
    };
    value
        .get(key)
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

fn claim_str(value: &Value, key: &str) -> RtResult<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .ok_or_else(|| rt_error(ErrorCode::InvalidArgument, "candidate_field"))
}

fn trip(faults: &AcceptFaults, step: AcceptStep) -> RtResult<()> {
    if faults.fail_step == Some(step) {
        Err(rt_error(ErrorCode::StorageUnavailable, "injected_failure"))
    } else {
        Ok(())
    }
}

fn cleanup_ready(proof: &CleanupProof, fence: &Fence) -> bool {
    proof.process.incarnation == fence.incarnation
        && proof.process.process_tree_empty
        && proof.mailbox_empty
        && proof.tools_drained
        && proof.ingress_drained
}

fn audit_json(
    attempt_or_event: &str,
    decision_mono: u64,
    commit_mono: u64,
    message_ids: &[String],
    evidence_ids: &[String],
    manifest_id: &Option<String>,
) -> String {
    json!({
        "attempt_id": attempt_or_event,
        "decision_mono": decision_mono,
        "commit_mono": commit_mono,
        "message_ids": message_ids,
        "evidence_ids": evidence_ids,
        "manifest_ids": manifest_id.iter().cloned().collect::<Vec<_>>(),
    })
    .to_string()
}

fn phase_kind(snapshot_ref: &str) -> PhaseKind {
    match snapshot_ref {
        "critique" => PhaseKind::Critique,
        "synthesis" => PhaseKind::Synthesis,
        _ => PhaseKind::Proposal,
    }
}

fn parse_snake<T: serde::de::DeserializeOwned>(text: &str) -> RtResult<T> {
    serde_json::from_str(&format!("\"{text}\""))
        .map_err(|_| rt_error(ErrorCode::StorageUnavailable, "roundtable_column"))
}

fn parse_id<T: FromStr>(text: &str) -> RtResult<T> {
    text.parse()
        .map_err(|_| rt_error(ErrorCode::StorageUnavailable, "roundtable_column"))
}

fn hash_hex(text: &str) -> RtResult<Hash256> {
    serde_json::from_str(&format!("\"{text}\""))
        .map_err(|_| rt_error(ErrorCode::InvalidArgument, "object_hash"))
}

fn u64_from(value: i64) -> RtResult<u64> {
    u64::try_from(value).map_err(|_| rt_error(ErrorCode::InvalidArgument, "wire_u64"))
}

fn i64_from(value: u64) -> RtResult<i64> {
    i64::try_from(value).map_err(|_| rt_error(ErrorCode::InvalidArgument, "wire_u64"))
}

async fn query_text_local(
    conn: &impl ConnectionTrait,
    sql: &str,
    values: Vec<sea_orm::Value>,
) -> RtResult<String> {
    let row = one_row(conn, sql, values).await?;
    column(&row, 0)
}

async fn json_rows<T: serde::de::DeserializeOwned>(
    txn: &impl ConnectionTrait,
    room: &str,
    sql: &str,
) -> RtResult<Vec<T>> {
    let mut result = Vec::new();
    for row in rows(txn, sql, vec![text(room)]).await? {
        let raw: String = column(&row, 0)?;
        result.push(
            serde_json::from_str(&raw)
                .map_err(|_| rt_error(ErrorCode::StorageUnavailable, "projection_row"))?,
        );
    }
    Ok(result)
}

async fn load_replay(
    txn: &impl ConnectionTrait,
    room_id: &str,
    room: &RoomSnap,
) -> RtResult<roundtable_protocol::ProjectionReplayV1> {
    use roundtable_protocol::{DurationMs, ProjectionReplayV1, SafeInt};
    let row=one_row(txn,"SELECT current_phase_id,active_control_id,result_quality,remaining_active_ms FROM rt_rooms WHERE room_id=?",vec![text(room_id)]).await?;
    let phase: Option<String> = column(&row, 0)?;
    let operation: Option<String> = column(&row, 1)?;
    let mut replay = ProjectionReplayV1 {
        config: serde_json::from_str(&room.config_ref).ok(),
        boot_epoch: Epoch(u64_from(room.boot_epoch)?),
        current_phase_id: phase.as_deref().map(parse_id).transpose()?,
        active_control_id: operation.as_deref().map(parse_id).transpose()?,
        result_quality: column(&row, 2)?,
        ..Default::default()
    };
    replay.budget.remaining_active_ms = DurationMs(u64_from(column(&row, 3)?)?);
    replay.budget.admitted_attempts = SafeInt(u64_from(
        query_i64(
            txn,
            "SELECT COALESCE(SUM(admitted_attempt_count),0) FROM rt_turns WHERE room_id=?",
            vec![text(room_id)],
        )
        .await?,
    )?);
    replay.speakers=json_rows(txn,room_id,"SELECT json_object('speaker_id',speaker_id,'ordinal',ordinal,'role',role,'provider_ref',provider_ref,'model_id',model_id) FROM rt_speakers WHERE room_id=? ORDER BY ordinal").await?;
    replay.turns=json_rows(txn,room_id,"SELECT json_object('turn_id',turn_id,'phase_id',phase_id,'speaker_id',speaker_id,'status',status,'accepted_attempt_id',accepted_attempt_id,'admitted_attempt_count',admitted_attempt_count) FROM rt_turns WHERE room_id=? ORDER BY turn_id").await?;
    replay.attempts=json_rows(txn,room_id,"SELECT json_object('attempt_id',attempt_id,'turn_id',turn_id,'attempt_no',attempt_no,'binding_id',binding_id,'state',state,'dispatch_state',dispatch_state,'cleanup_state',cleanup_state,'residual_remote_work',json(CASE residual_remote_work WHEN 1 THEN 'true' ELSE 'false' END)) FROM rt_attempts WHERE room_id=? ORDER BY turn_id,attempt_no").await?;
    replay.bindings=json_rows(txn,room_id,"SELECT json_object('binding_id',binding_id,'speaker_id',speaker_id,'generation',CAST(generation AS TEXT),'state',state,'context_state',context_state,'retire_reason',retire_reason) FROM rt_bindings WHERE room_id=? ORDER BY binding_id").await?;
    replay.control_operations=json_rows(txn,room_id,"SELECT json_object('operation_id',operation_id,'kind',kind,'step',step,'target_phase_id',target_phase_id,'target_revision',CAST(target_revision AS TEXT),'requested_epoch',CAST(requested_epoch AS TEXT),'status',status,'blocked_reason',blocked_reason,'successor_phase_id',successor_phase_id,'superseded_by',superseded_by) FROM rt_control_operations WHERE room_id=? ORDER BY operation_id").await?;
    replay.inputs=json_rows(txn,room_id,"SELECT json_object('input_id',input_id,'text',text,'mode',mode,'accepted_seq',CAST(accepted_seq AS TEXT),'target_phase_index',target_phase_index,'applied_phase_id',applied_phase_id,'applied_seq',CAST(applied_seq AS TEXT),'state',state) FROM rt_user_inputs WHERE room_id=? ORDER BY accepted_seq").await?;
    replay.budget.reservations=json_rows(txn,room_id,"SELECT json_object('reservation_id',reservation_id,'purpose',purpose,'amount_ms',CAST(amount_ms AS TEXT),'amount_bytes',amount_bytes,'state',state) FROM rt_budget_reservations WHERE room_id=? ORDER BY reservation_id").await?;
    replay.message_memberships=json_rows(txn,room_id,"SELECT json_object('message_id',message_id,'membership_version',CAST(membership_version AS TEXT),'visibility',visibility,'published_seq',CAST(published_seq AS TEXT)) FROM rt_message_memberships WHERE room_id=? ORDER BY message_id,membership_version").await?;
    for row in rows(txn,"SELECT evidence_id,manifest_id,owner_speaker_id,content_hash,publish_seq,body_json FROM rt_evidence WHERE room_id=? ORDER BY evidence_id",vec![text(room_id)]).await? {
        let id:String=column(&row,0)?;let manifest:String=column(&row,1)?;let speaker:String=column(&row,2)?;let hash:String=column(&row,3)?;
        let seq:Option<i64>=column(&row,4)?;let body:Option<String>=column(&row,5)?;
        replay.evidence.push(roundtable_protocol::ProjectionEvidenceV1 {
            evidence_id:parse_id(&id)?,manifest_id:parse_id(&manifest)?,owner_speaker_id:parse_id(&speaker)?,
            content_hash:hash_hex(&hash)?,published_seq:seq.map(u64_from).transpose()?.map(Seq),body_hash:evidence_body_hash(body.as_deref())?,
        });
    }
    let history = super::owned_runtime::load_history_in(txn, &parse_id(room_id)?).await?;
    let mut targets = Vec::new();
    if let Some(row)=optional_row(txn,"SELECT pc.snapshot_json FROM rt_phase_contexts pc JOIN rt_rooms r ON r.room_id=pc.room_id AND r.current_phase_id=pc.phase_id WHERE r.room_id=?",vec![text(room_id)]).await? {
        let snapshot:roundtable_protocol::PhaseSnapshotV1=serde_json::from_str(&column::<String>(&row,0)?).map_err(|_|rt_error(ErrorCode::StorageUnavailable,"phase_snapshot"))?;
        for speaker in &replay.speakers {
            let required=snapshot.mandatory_targets.iter().filter(|target|target.speaker_id==speaker.speaker_id).map(|target|roundtable_protocol::AssignedTarget{claim_id:target.claim_id,response_id:target.response_id,source_ordinal:0,publication_seq:Seq(0),priority:roundtable_protocol::Priority::Normal}).collect();
            targets.push(roundtable_protocol::SpeakerTargets{speaker:roundtable_protocol::SpeakerOrdinal{speaker_id:speaker.speaker_id,ordinal:speaker.ordinal},required});
        }
    }
    replay.coverage = Some(roundtable_protocol::coverage(&history, &targets));
    replay.source_manifests=json_rows(txn,room_id,"SELECT json_object('manifest_id',manifest_id,'hash',manifest_hash) FROM rt_source_manifests WHERE room_id=? ORDER BY version").await?;
    Ok(replay)
}

fn evidence_body_hash(body: Option<&str>) -> RtResult<Hash256> {
    let parsed: Value = serde_json::from_str(body.unwrap_or("null"))
        .map_err(|_| rt_error(ErrorCode::StorageUnavailable, "evidence_body"))?;
    Ok(Hash256::sha256(&roundtable_protocol::canonical_bytes(
        &parsed,
    )?))
}
