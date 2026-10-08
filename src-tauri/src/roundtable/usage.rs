//! Late usage is an archive. It does not move the room.

use roundtable_protocol::{
    fold_measurement, AttemptId, ErrorCode, MeasurementV1, RtResult, UsageState,
};
use sea_orm::{ConnectionTrait, TransactionTrait};
use serde_json::{json, Value};

use super::rt_error;
use super::store::{column, exec, num, optional_row, query_i64, rows, storage_err, text};

use super::service::RoundtableService;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RoomMeter {
    pub status: &'static str,
    pub revision: u64,
    pub accepted: u64,
    pub published: u64,
    pub schedule: &'static str,
}

impl RoomMeter {
    pub const fn frozen() -> Self {
        Self {
            status: "recovering",
            revision: 1,
            accepted: 0,
            published: 0,
            schedule: "frozen",
        }
    }
}

pub async fn archive_late_measurement(
    service: &RoundtableService,
    attempt: AttemptId,
    measurement: MeasurementV1,
) -> RtResult<()> {
    if !service.writable() {
        return Err(rt_error(ErrorCode::Forbidden, "read_only_coordinator"));
    }
    let store = service.command_store()?;
    let body = roundtable_protocol::canonical_bytes(&measurement)?;
    let id = roundtable_protocol::canonical_hash(&(attempt, &measurement))?.to_hex();
    let txn = store.connection().begin().await.map_err(storage_err)?;
    let room = optional_row(
        &txn,
        "SELECT room_id FROM rt_attempts WHERE attempt_id=?",
        vec![text(&attempt.to_string())],
    )
    .await?
    .ok_or_else(|| rt_error(ErrorCode::InvalidArgument, "attempt_missing"))?;
    let room: String = column(&room, 0)?;
    let seq = query_i64(
        &txn,
        "SELECT COALESCE(MAX(ledger_seq),0)+1 FROM rt_usage_archive WHERE room_id=?",
        vec![text(&room)],
    )
    .await?;
    exec(&txn,"INSERT OR IGNORE INTO rt_usage_archive(room_id,measurement_id,attempt_id,ledger_seq,body_json) VALUES(?,?,?,?,?)",vec![text(&room),text(&id),text(&attempt.to_string()),num(seq),text(&String::from_utf8(body).map_err(|_|rt_error(ErrorCode::InvalidArgument,"measurement"))?)]).await?;
    txn.commit().await.map_err(storage_err)?;
    Ok(())
}

pub(crate) async fn usage_summary_in(conn: &impl ConnectionTrait, room: &str) -> RtResult<Value> {
    let samples = rows(
        conn,
        "SELECT ledger_seq,body_json,attempt_id FROM rt_usage_archive WHERE room_id=? ORDER BY ledger_seq",
        vec![text(room)],
    )
    .await?;
    let mut state = UsageState::empty();
    let mut unknown = 0u64;
    let mut version = 0i64;
    for sample in samples {
        version = column(&sample, 0)?;
        let attempt: String = column(&sample, 2)?;
        let mut sample: MeasurementV1 = serde_json::from_str(&column::<String>(&sample, 1)?)
            .map_err(|_| rt_error(ErrorCode::StorageUnavailable, "measurement"))?;
        // Provider scopes and dedupe keys are only authoritative within their
        // owning attempt; tuple encoding preserves the original scope exactly.
        sample.scope = serde_json::to_string(&(attempt, &sample.scope))
            .map_err(|_| rt_error(ErrorCode::StorageUnavailable, "measurement_scope"))?;
        if sample.value.is_none() || !sample.attributed {
            unknown += 1;
        }
        state = fold_measurement(&state, sample);
    }
    Ok(
        json!({"usage_version":version,"confirmed_output_tokens":state.confirmed_output_tokens,"unknown_count":unknown,"uncertain":state.uncertain,"unknown_total":state.unknown_total}),
    )
}
