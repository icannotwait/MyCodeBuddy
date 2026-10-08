//! Product admission stays off until an operator enables it. Disable drains
//! work that already started and does not delete evidence.

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rollout {
    pub enabled: bool,
    pub active: u32,
    pub evidence: u32,
}

impl Rollout {
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            active: 0,
            evidence: 0,
        }
    }
}

pub fn may_start(rollout: &Rollout) -> bool {
    rollout.enabled
}

pub fn disable_and_drain(rollout: &mut Rollout) {
    rollout.enabled = false;
    rollout.active = 0;
}
