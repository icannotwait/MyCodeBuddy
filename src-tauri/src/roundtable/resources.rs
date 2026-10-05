//! One service-wide permit. A second room cannot start while the first holds it.
//!
//! Release compares the whole incarnation set. One process proof cannot free
//! several slots, and a partial proof stays quarantined.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;

use roundtable_protocol::{ErrorCode, IncarnationId, PermitReleaseProof, RoomId, RtResult};

use super::rt_error;

#[derive(Clone, Debug)]
pub struct PermitBundle {
    pub lease_id: String,
    pub room_id: RoomId,
    incarnations: Vec<IncarnationId>,
    slots: u32,
}

impl PermitBundle {
    pub fn slot_count(&self) -> u32 {
        self.slots
    }

    pub fn incarnations(&self) -> &[IncarnationId] {
        &self.incarnations
    }
}

struct PermitState {
    active_room: Option<String>,
    held_slots: u32,
    slot_capacity: u32,
    leases: BTreeMap<String, Vec<IncarnationId>>,
    next_lease: u64,
    quarantined: bool,
}

pub struct ResourceAllocator {
    inner: Mutex<PermitState>,
}

impl ResourceAllocator {
    pub fn new(slot_capacity: u32) -> Self {
        Self {
            inner: Mutex::new(PermitState {
                active_room: None,
                held_slots: 0,
                slot_capacity: slot_capacity.max(1),
                leases: BTreeMap::new(),
                next_lease: 1,
                quarantined: false,
            }),
        }
    }

    pub fn held_slots(&self) -> u32 {
        self.inner.lock().expect("permits").held_slots
    }

    pub fn quarantined(&self) -> bool {
        self.inner.lock().expect("permits").quarantined
    }

    /// All or nothing. A failed acquire leaves the previous occupancy unchanged
    /// and never keeps a partial slot.
    pub fn try_acquire(&self, room: RoomId, slots: u32) -> RtResult<PermitBundle> {
        if slots == 0 {
            return Err(rt_error(ErrorCode::CapacityLimited, "capacity_limited"));
        }
        let mut state = self.inner.lock().expect("permits");
        if state.quarantined {
            return Err(rt_error(ErrorCode::CapacityLimited, "capacity_limited"));
        }
        if let Some(active) = &state.active_room {
            if active != &room.to_string() {
                return Err(rt_error(ErrorCode::CapacityLimited, "capacity_limited"));
            }
        }
        let next = state
            .held_slots
            .checked_add(slots)
            .ok_or_else(|| rt_error(ErrorCode::InvalidArgument, "overflow"))?;
        if next > state.slot_capacity {
            return Err(rt_error(ErrorCode::CapacityLimited, "capacity_limited"));
        }
        state.held_slots = next;
        state.active_room = Some(room.to_string());
        let lease_id = format!("lease-{}", state.next_lease);
        state.next_lease += 1;
        let incarnations = (0..slots)
            .map(|index| incarnation_for(state.next_lease, index))
            .collect::<Vec<_>>();
        state.leases.insert(lease_id.clone(), incarnations.clone());
        Ok(PermitBundle {
            lease_id,
            room_id: room,
            incarnations,
            slots,
        })
    }

    /// Resume takes `min(C, remaining)`, never the larger concurrency.
    pub fn try_resume(&self, room: RoomId, concurrency: u32, remaining: u32) -> RtResult<PermitBundle> {
        let slots = remaining.min(concurrency);
        if slots == 0 {
            return Err(rt_error(ErrorCode::CapacityLimited, "capacity_limited"));
        }
        self.try_acquire(room, slots)
    }

    pub fn release(&self, bundle: &PermitBundle, proof: &PermitReleaseProof) -> RtResult<()> {
        let mut state = self.inner.lock().expect("permits");
        let Some(expected) = state.leases.get(&bundle.lease_id).cloned() else {
            return Err(rt_error(ErrorCode::InvalidState, "unknown_lease"));
        };
        if proof.lease_id != bundle.lease_id || !proof.releases(&expected) {
            state.quarantined = true;
            return Err(rt_error(ErrorCode::InvalidState, "partial_cleanup"));
        }
        state.leases.remove(&bundle.lease_id);
        state.held_slots = state.held_slots.saturating_sub(bundle.slots);
        if state.held_slots == 0 {
            state.active_room = None;
            state.quarantined = false;
        }
        Ok(())
    }
}

fn incarnation_for(lease: u64, index: u32) -> IncarnationId {
    let text = format!("00000000-0000-4000-8000-{lease:08}{index:04}");
    text.parse().expect("incarnation")
}

/// Local execution permission. It expires without waiting for SQLite.
pub struct ExecutionLease {
    generation: AtomicU64,
    prepaid_until: AtomicU64,
    revoked: AtomicBool,
    residual: AtomicBool,
    durable_stopped: AtomicBool,
}

impl ExecutionLease {
    pub fn issue(now_ms: u64, slice_ms: u64) -> Self {
        let slice = slice_ms.min(1_000);
        Self {
            generation: AtomicU64::new(1),
            prepaid_until: AtomicU64::new(now_ms.saturating_add(slice)),
            revoked: AtomicBool::new(false),
            residual: AtomicBool::new(false),
            durable_stopped: AtomicBool::new(false),
        }
    }

    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::SeqCst)
    }

    pub fn prepaid_until(&self) -> u64 {
        self.prepaid_until.load(Ordering::SeqCst)
    }

    pub fn revoked(&self) -> bool {
        self.revoked.load(Ordering::SeqCst)
    }

    pub fn remote_residual(&self) -> bool {
        self.residual.load(Ordering::SeqCst)
    }

    /// Stopped is a durable fact. A revoked local lease is not enough.
    pub fn ui_shows_stopped(&self) -> bool {
        self.durable_stopped.load(Ordering::SeqCst)
    }

    pub fn on_storage_wait(&self, waited_ms: u64) {
        if waited_ms > 1_000 {
            self.revoke_local();
        }
    }

    pub fn revoke_local(&self) {
        self.revoked.store(true, Ordering::SeqCst);
        self.residual.store(true, Ordering::SeqCst);
    }

    pub fn admit_enqueue(&self, now_ms: u64) -> u32 {
        self.admit(now_ms)
    }

    pub fn admit_forward(&self, now_ms: u64) -> u32 {
        self.admit(now_ms)
    }

    pub fn admit_tool(&self, now_ms: u64) -> u32 {
        self.admit(now_ms)
    }

    pub fn late_ack(&self, generation: u64, now_ms: u64) -> bool {
        if self.revoked() || generation != self.generation() || now_ms >= self.prepaid_until() {
            return false;
        }
        let prepaid = self.prepaid_until();
        let next = now_ms.saturating_add(1_000);
        self.prepaid_until
            .compare_exchange(prepaid, next, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
    }

    fn admit(&self, now_ms: u64) -> u32 {
        if self.revoked() || now_ms >= self.prepaid_until() {
            0
        } else {
            1
        }
    }
}
