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
use super::store::{
    column, exec, num, one_row, opt, optional_row, query_i64, rows, storage_err, text,
    RoundtableStore,
};
use super::rt_error;
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
        let parsed: Value = serde_json::from_str(&changed)
            .map_err(|_| rt_error(ErrorCode::InvalidState, "projection_refs"))?;
        let message_ids = string_list(&parsed, "message_ids");
        let evidence_ids = string_list(&parsed, "evidence_ids");
        let manifest_ids = string_list(&parsed, "manifest_ids");
        let mut messages = Vec::new();
        for message_id in &message_ids {
            let row = optional_row(
                conn,
                "SELECT body_hash FROM rt_messages WHERE room_id = ? AND message_id = ?",
                vec![text(&room.to_string()), text(message_id)],
            )
            .await?;
            let Some(row) = row else {
                return Err(rt_error(ErrorCode::InvalidState, "message_manifest_missing"));
            };
            let hash: String = column(&row, 0)?;
            messages.push(PublishedMessageRef {
                message_id: parse_id(message_id)?,
                hash: hash_hex(&hash)?,
            });
        }
        for evidence_id in &evidence_ids {
            let count = query_i64(
                conn,
                "SELECT COUNT(*) FROM rt_evidence WHERE room_id = ? AND evidence_id = ?",
                vec![text(&room.to_string()), text(evidence_id)],
            )
            .await?;
            if count != 1 {
                return Err(rt_error(
                    ErrorCode::InvalidState,
                    "evidence_manifest_missing",
                ));
            }
        }
        for manifest_id in &manifest_ids {
            let count = query_i64(
                conn,
                "SELECT COUNT(*) FROM rt_source_manifests WHERE room_id = ? AND manifest_id = ?",
                vec![text(&room.to_string()), text(manifest_id)],
            )
            .await?;
            if count != 1 {
                return Err(rt_error(
                    ErrorCode::InvalidState,
                    "evidence_manifest_missing",
                ));
            }
        }
        let id = parse_id::<ProjectionId>(&projection_id)?;
        let projected = project(&RoomAggregate {
            projection_id: id,
            body: body.clone(),
            messages,
            evidence_manifests: manifest_ids
                .iter()
                .map(|item| parse_id(item))
                .collect::<RtResult<Vec<_>>>()?,
            required_message_ids: message_ids
                .iter()
                .map(|item| parse_id(item))
                .collect::<RtResult<Vec<_>>>()?,
            required_evidence_manifests: manifest_ids
                .iter()
                .map(|item| parse_id(item))
                .collect::<RtResult<Vec<_>>>()?,
        })?;
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
        if input.completion.finish_reason != "completed" || !input.completion.tool_barrier.drained
        {
            return Err(rt_error(ErrorCode::InvalidState, "finish_not_normal"));
        }
        if input
            .completion
            .candidate_id
            .as_ref()
            .is_some_and(|candidate| candidate != &input.candidate_id)
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
            return self.replay_accept(txn, &room_id, &attempt_id, &input.candidate_id).await;
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
        let phase = phase_row(txn, &room_id, &phase_id).await?;
        if phase.status != "running" || phase.revision != i64_from(input.fence.phase_revision.0)?
        {
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
            "SELECT payload_hash, receipt_json FROM rt_submissions
             WHERE room_id = ? AND attempt_id = ? AND sealed = 1",
            vec![text(&room_id), text(&attempt_id)],
        )
        .await?;
        let Some(submission) = submission else {
            return Err(rt_error(ErrorCode::InvalidState, "candidate_missing"));
        };
        let payload_hash: String = column(&submission, 0)?;
        let receipt: Option<String> = column(&submission, 1).unwrap_or(None);
        if payload_hash != input.candidate_id {
            return Err(rt_error(ErrorCode::InvalidState, "candidate_mismatch"));
        }
        let role = query_text_local(
            txn,
            "SELECT role FROM rt_speakers WHERE room_id = ? AND speaker_id = ?",
            vec![text(&room_id), text(&speaker_id)],
        )
        .await?;
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
                text(receipt.as_deref().unwrap_or("{}")),
                text(&payload_hash),
            ],
        )
        .await?;
        let visibility = if role == "moderator" {
            "published"
        } else {
            "staged"
        };
        exec(
            txn,
            "INSERT INTO rt_message_memberships (
                room_id, message_id, membership_version, visibility, published_seq
             ) VALUES (?, ?, 1, ?, NULL)",
            vec![text(&room_id), text(&message_id), text(visibility)],
        )
        .await?;
        let receipt_json = receipt.as_deref();
        trip(&faults, AcceptStep::Claims)?;
        insert_claims(txn, &room_id, &message_id, receipt_json).await?;
        trip(&faults, AcceptStep::Responses)?;
        insert_responses(txn, &room_id, &message_id, receipt_json).await?;
        trip(&faults, AcceptStep::PositionChanges)?;
        insert_positions(txn, &room_id, &message_id, receipt_json).await?;
        trip(&faults, AcceptStep::Evidence)?;
        let evidence_ids = insert_evidence_links(txn, &room_id, &message_id, receipt_json).await?;
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
        let event_id = format!("accept-{attempt_id}");
        let projection = self
            .emit(
                txn,
                &faults,
                &room_id,
                "accept",
                &event_id,
                &[message_id.clone()],
                &evidence_ids,
                decision_mono,
                0,
                &utc,
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
            "UPDATE rt_turns SET accepted_attempt_id = ?
             WHERE room_id = ? AND turn_id = ? AND accepted_attempt_id IS NULL",
            vec![text(&attempt_id), text(&room_id), text(&turn_id)],
        )
        .await?;
        if turned != 1 {
            return Err(rt_error(ErrorCode::InvalidState, "attempt_not_ready"));
        }
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
        let phase = phase_row(txn, &room_id, &phase_id).await?;
        if phase.revision != i64_from(input.fence.phase_revision.0)? {
            return Err(rt_error(ErrorCode::InvalidState, "fence_mismatch"));
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
               AND a.state IN ('accepted','invalid','failed','timed_out','interrupted')",
            vec![text(&room_id), text(&phase_id)],
        )
        .await?;
        if terminal < turns {
            let (now, _) = self.clock_sample();
            if deadline_reached(now, input.deadline_mono) {
                return Err(rt_error(ErrorCode::InvalidState, "deadline_exceeded"));
            }
            if input.cancel_unfinished {
                self.insert_cancel(txn, &room_id, &phase_id, phase.revision)
                    .await?;
            }
            return Ok(self.status_ref(txn, &room_id, &phase_id, false, None).await?);
        }
        let accepted = query_i64(
            txn,
            "SELECT COUNT(*) FROM rt_turns t
             JOIN rt_attempts a ON a.room_id = t.room_id AND a.turn_id = t.turn_id
             WHERE t.room_id = ? AND t.phase_id = ? AND a.state = 'accepted'",
            vec![text(&room_id), text(&phase_id)],
        )
        .await?;
        let moderator_failed = query_i64(
            txn,
            "SELECT COUNT(*) FROM rt_turns t
             JOIN rt_attempts a ON a.room_id = t.room_id AND a.turn_id = t.turn_id
             JOIN rt_speakers s ON s.room_id = t.room_id AND s.speaker_id = t.speaker_id
             WHERE t.room_id = ? AND t.phase_id = ? AND s.role = 'moderator'
               AND a.state IN ('failed','timed_out','invalid','interrupted')",
            vec![text(&room_id), text(&phase_id)],
        )
        .await?;
        let closing_ref = if moderator_failed > 0 {
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
        let (now, utc) = self.clock_sample();
        if deadline_reached(now, input.deadline_mono) {
            return Err(rt_error(ErrorCode::InvalidState, "deadline_exceeded"));
        }
        let _ = bump_room(txn, &room_id).await?;
        let event_id = format!("close-{phase_id}");
        self.emit(
            txn,
            &self.fault_snapshot(),
            &room_id,
            "close",
            &event_id,
            &[],
            &[],
            now,
            now,
            &utc,
        )
        .await?;
        self.status_ref(txn, &room_id, &phase_id, true, Some(closing_ref))
            .await
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
        let phase = phase_row(txn, &room_id, &phase_id).await?;
        if phase.status != "closing" {
            return Err(rt_error(ErrorCode::InvalidState, "phase_not_closing"));
        }
        let accepted = query_i64(
            txn,
            "SELECT COUNT(*) FROM rt_turns t
             JOIN rt_attempts a ON a.room_id = t.room_id AND a.turn_id = t.turn_id
             WHERE t.room_id = ? AND t.phase_id = ? AND a.state = 'accepted'
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
             WHERE msg.room_id = ? AND t.phase_id = ? AND a.state = 'accepted'
             ORDER BY s.ordinal ASC",
            vec![text(&room_id), text(&phase_id)],
        )
        .await?;
        let mut message_ids = Vec::new();
        for row in &ordinals {
            let message_id: String = column(row, 0)?;
            let ordinal: i64 = column(row, 1)?;
            exec(
                txn,
                "UPDATE rt_message_memberships
                 SET visibility = 'published', published_seq = ?
                 WHERE room_id = ? AND message_id = ? AND membership_version = 1",
                vec![num(ordinal), text(&room_id), text(&message_id)],
            )
            .await?;
            message_ids.push(message_id);
        }
        let bumped = bump_room(txn, &room_id).await?;
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
             WHERE room_id = ? AND evidence_id IN (
                 SELECT evidence_id FROM rt_message_evidence WHERE room_id = ?
             )",
            vec![num(bumped.seq), text(&room_id), text(&room_id)],
        )
        .await?;
        if phase.snapshot_ref != "synthesis" {
            let next_id = Uuid::new_v4().to_string();
            exec(
                txn,
                "INSERT INTO rt_phases (
                    room_id, phase_id, phase_index, revision, status, snapshot_ref, snapshot_hash,
                    expected, quorum, remaining_ms, closing_set_ref, published_seq, manifest_id
                 ) VALUES (?, ?, ?, 1, 'ready', ?, ?, ?, ?, ?, NULL, NULL, ?)",
                vec![
                    text(&room_id),
                    text(&next_id),
                    num(phase.index + 1),
                    text(&phase.snapshot_ref),
                    text(&phase.snapshot_hash),
                    num(phase.expected),
                    num(phase.quorum),
                    num(phase.remaining_ms),
                    opt(phase.manifest_id.as_deref()),
                ],
            )
            .await?;
            exec(
                txn,
                "UPDATE rt_rooms SET current_phase_id = ? WHERE room_id = ?",
                vec![text(&next_id), text(&room_id)],
            )
            .await?;
        } else {
            exec(
                txn,
                "UPDATE rt_rooms SET status = 'completed' WHERE room_id = ?",
                vec![text(&room_id)],
            )
            .await?;
        }
        let (now, utc) = self.clock_sample();
        if deadline_reached(now, input.deadline_mono) {
            return Err(rt_error(ErrorCode::InvalidState, "deadline_exceeded"));
        }
        self.emit(
            txn,
            &self.fault_snapshot(),
            &room_id,
            "publish",
            &format!("publish-{phase_id}"),
            &message_ids,
            &[],
            now,
            now,
            &utc,
        )
        .await
    }

    async fn emit(
        &self,
        txn: &DatabaseTransaction,
        faults: &AcceptFaults,
        room_id: &str,
        cause: &str,
        event_id: &str,
        message_ids: &[String],
        evidence_ids: &[String],
        decision_mono: u64,
        commit_mono: u64,
        utc: &str,
    ) -> RtResult<ProjectionRef> {
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
        if let Some(manifest) = room_manifest(txn, room_id).await? {
            manifest_ids.push(manifest);
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
            ledger_seq: Seq(u64_from(room.revision)?),
            sampled_active_ms: roundtable_protocol::DurationMs(0),
            sampled_at_utc: utc.to_string(),
        };
        let mut messages = Vec::new();
        for message_id in message_ids {
            let hash = query_text_local(
                txn,
                "SELECT body_hash FROM rt_messages WHERE room_id = ? AND message_id = ?",
                vec![text(room_id), text(message_id)],
            )
            .await?;
            messages.push(PublishedMessageRef {
                message_id: parse_id(message_id)?,
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
             WHERE a.room_id = ? AND t.phase_id = ? AND a.state = 'accepted'
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
    index: i64,
    snapshot_ref: String,
    snapshot_hash: String,
    expected: i64,
    quorum: i64,
    remaining_ms: i64,
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

async fn phase_row(txn: &impl ConnectionTrait, room_id: &str, phase_id: &str) -> RtResult<PhaseSnap> {
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
        index: column(&row, 2)?,
        snapshot_ref: column(&row, 3)?,
        snapshot_hash: column(&row, 4)?,
        expected: column(&row, 5)?,
        quorum: column(&row, 6)?,
        remaining_ms: column(&row, 7)?,
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

async fn room_manifest(txn: &impl ConnectionTrait, room_id: &str) -> RtResult<Option<String>> {
    let row = optional_row(
        txn,
        "SELECT manifest_id FROM rt_phases
         WHERE room_id = ? AND manifest_id IS NOT NULL
         ORDER BY phase_index ASC LIMIT 1",
        vec![text(room_id)],
    )
    .await?;
    match row {
        Some(row) => Ok(Some(column(&row, 0)?)),
        None => Ok(None),
    }
}

async fn insert_claims(
    txn: &DatabaseTransaction,
    room_id: &str,
    message_id: &str,
    receipt: Option<&str>,
) -> RtResult<()> {
    for claim in receipt_array(receipt, "claims") {
        let key = claim_str(&claim, "local_key")?;
        let statement = claim_str(&claim, "statement")?;
        exec(
            txn,
            "INSERT INTO rt_claims (room_id, message_id, local_key, statement) VALUES (?, ?, ?, ?)",
            vec![text(room_id), text(message_id), text(&key), text(&statement)],
        )
        .await?;
    }
    Ok(())
}

async fn insert_responses(
    txn: &DatabaseTransaction,
    room_id: &str,
    message_id: &str,
    receipt: Option<&str>,
) -> RtResult<()> {
    for response in receipt_array(receipt, "responses") {
        exec(
            txn,
            "INSERT INTO rt_responses (
                room_id, response_id, message_id, target_message_id, target_local_key, stance
             ) VALUES (?, ?, ?, ?, ?, ?)",
            vec![
                text(room_id),
                text(&claim_str(&response, "response_id")?),
                text(message_id),
                text(&claim_str(&response, "target_message_id")?),
                text(&claim_str(&response, "target_local_key")?),
                text(&claim_str(&response, "stance")?),
            ],
        )
        .await?;
    }
    Ok(())
}

async fn insert_positions(
    txn: &DatabaseTransaction,
    room_id: &str,
    message_id: &str,
    receipt: Option<&str>,
) -> RtResult<()> {
    for change in receipt_array(receipt, "position_changes") {
        exec(
            txn,
            "INSERT INTO rt_position_changes (
                room_id, position_change_id, message_id, target_message_id, target_local_key,
                from_position, to_position
             ) VALUES (?, ?, ?, ?, ?, ?, ?)",
            vec![
                text(room_id),
                text(&claim_str(&change, "position_change_id")?),
                text(message_id),
                text(&claim_str(&change, "target_message_id")?),
                text(&claim_str(&change, "target_local_key")?),
                text(&claim_str(&change, "from_position")?),
                text(&claim_str(&change, "to_position")?),
            ],
        )
        .await?;
    }
    Ok(())
}

async fn insert_evidence_links(
    txn: &DatabaseTransaction,
    room_id: &str,
    message_id: &str,
    receipt: Option<&str>,
) -> RtResult<Vec<String>> {
    let mut ids = Vec::new();
    for evidence in receipt_array(receipt, "evidence_ids") {
        let Some(evidence_id) = evidence.as_str() else {
            return Err(rt_error(ErrorCode::InvalidArgument, "evidence_id"));
        };
        exec(
            txn,
            "INSERT INTO rt_message_evidence (room_id, message_id, evidence_id) VALUES (?, ?, ?)",
            vec![text(room_id), text(message_id), text(evidence_id)],
        )
        .await?;
        ids.push(evidence_id.to_string());
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

fn string_list(value: &Value, key: &str) -> Vec<String> {
    value
        .get(key)
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(ToOwned::to_owned)
                .collect()
        })
        .unwrap_or_default()
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

