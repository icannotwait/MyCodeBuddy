//! Late usage is an archive. It does not move the room.

use roundtable_protocol::{AttemptId, MeasurementV1, RtResult};

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
    let _ = attempt;
    service.record_usage(measurement);
    Ok(())
}
