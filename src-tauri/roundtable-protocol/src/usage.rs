//! Usage folding. Confirmed totals move only forward inside one epoch.

use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MeasureSemantics {
    Cumulative,
    Gauge,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MeasurementV1 {
    pub unit: String,
    pub source: String,
    pub scope: String,
    pub semantics: MeasureSemantics,
    pub counter_id: String,
    pub epoch: u64,
    pub seq: Option<u64>,
    pub dedupe: String,
    pub value: Option<u64>,
    pub observed_at: String,
    pub attributed: bool,
    pub trusted_reset: bool,
    pub billable: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Water {
    max_seq: Option<u64>,
    value: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UsageState {
    pub confirmed_output_tokens: Option<u64>,
    pub unknown_total: bool,
    pub uncertain: bool,
    seen: BTreeSet<String>,
    waters: BTreeMap<(String, u64), Water>,
}

impl UsageState {
    pub fn empty() -> Self {
        Self {
            confirmed_output_tokens: None,
            unknown_total: false,
            uncertain: false,
            seen: BTreeSet::new(),
            waters: BTreeMap::new(),
        }
    }
}

pub fn fold_measurement(state: &UsageState, measurement: MeasurementV1) -> UsageState {
    let mut next = state.clone();
    if !next.seen.insert(measurement.dedupe.clone()) {
        return next;
    }
    if measurement.value.is_none() {
        next.unknown_total = true;
        return next;
    }
    if !measurement.attributed || !measurement.billable || measurement.unit == "occupancy" {
        return next;
    }
    let Some(value) = measurement.value else {
        return next;
    };
    let key = (measurement.counter_id.clone(), measurement.epoch);
    let current = next.waters.get(&key).cloned();
    if measurement.trusted_reset {
        next.waters.insert(
            key,
            Water {
                max_seq: measurement.seq,
                value,
            },
        );
        publish_output(&mut next, &measurement.counter_id, value);
        return next;
    }
    match current {
        None => {
            next.waters.insert(
                key,
                Water {
                    max_seq: measurement.seq,
                    value,
                },
            );
            publish_output(&mut next, &measurement.counter_id, value);
        }
        Some(water) => {
            if let (Some(seq), Some(max_seq)) = (measurement.seq, water.max_seq) {
                if seq < max_seq {
                    return next;
                }
            }
            if measurement.seq.is_none() && value < water.value {
                next.uncertain = true;
                return next;
            }
            let confirmed = water.value.max(value);
            let max_seq = match (water.max_seq, measurement.seq) {
                (Some(left), Some(right)) => Some(left.max(right)),
                (Some(left), None) => Some(left),
                (None, right) => right,
            };
            next.waters.insert(
                key,
                Water {
                    max_seq,
                    value: confirmed,
                },
            );
            publish_output(&mut next, &measurement.counter_id, confirmed);
        }
    }
    next
}

fn publish_output(state: &mut UsageState, counter: &str, value: u64) {
    if counter == "output_tokens" {
        state.confirmed_output_tokens = Some(value);
    }
}
