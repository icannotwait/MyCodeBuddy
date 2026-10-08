//! One actor per room. The gate is a std mutex and never waits on the network.

use std::sync::Mutex;

use roundtable_protocol::{InternalReason, RoomId, RtError, RtResult};

pub struct RoomGate {
    stopped: bool,
    admission_open: bool,
    admissions: u32,
    post_stop_admissions: u32,
}

pub struct RoomActor {
    room_id: RoomId,
    boot_epoch: u64,
    gate: Mutex<RoomGate>,
}

impl RoomActor {
    pub fn new(room_id: RoomId, boot_epoch: u64) -> Self {
        Self {
            room_id,
            boot_epoch,
            gate: Mutex::new(RoomGate {
                stopped: false,
                admission_open: false,
                admissions: 0,
                post_stop_admissions: 0,
            }),
        }
    }

    pub fn room_id(&self) -> &RoomId {
        &self.room_id
    }

    pub fn boot_epoch(&self) -> u64 {
        self.boot_epoch
    }

    /// Reject a callback from a previous boot. The check does not touch I/O.
    pub fn callback(&self, boot_epoch: u64) -> RtResult<()> {
        let _gate = self.gate.lock().expect("room gate");
        if boot_epoch != self.boot_epoch {
            return Err(RtError::from_reason(InternalReason::StaleFence));
        }
        Ok(())
    }

    /// One open admission at a time. A second caller loses while the first is open.
    pub fn admit(&self) -> bool {
        let mut gate = self.gate.lock().expect("room gate");
        if gate.stopped {
            return false;
        }
        if gate.admission_open {
            return false;
        }
        gate.admission_open = true;
        gate.admissions = gate.admissions.saturating_add(1);
        true
    }

    pub fn finish_admission(&self) {
        let mut gate = self.gate.lock().expect("room gate");
        gate.admission_open = false;
    }

    pub fn stop(&self) {
        let mut gate = self.gate.lock().expect("room gate");
        gate.stopped = true;
        gate.admission_open = false;
    }

    pub fn stopped(&self) -> bool {
        self.gate.lock().expect("room gate").stopped
    }

    pub fn post_stop_new_admissions(&self) -> u32 {
        self.gate.lock().expect("room gate").post_stop_admissions
    }

    pub fn admissions(&self) -> u32 {
        self.gate.lock().expect("room gate").admissions
    }
}
