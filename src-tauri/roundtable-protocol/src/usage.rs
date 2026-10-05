//! Usage folding. Confirmed totals accumulate positive deltas across counters and epochs.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MeasureSemantics {
    Incremental,
    Cumulative,
    ContextOccupancy,
    Gauge,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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
    seen: BTreeSet<(CounterIdentity, String)>,
    waters: BTreeMap<CounterIdentity, Water>,
}

type CounterIdentity = (String, String, String, String, u64);

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
    let key = (
        measurement.unit.clone(),
        measurement.source.clone(),
        measurement.scope.clone(),
        measurement.counter_id.clone(),
        measurement.epoch,
    );
    if !next.seen.insert((key.clone(), measurement.dedupe.clone())) {
        return next;
    }
    if !measurement.attributed
        || !measurement.billable
        || measurement.unit == "occupancy"
        || matches!(
            measurement.semantics,
            MeasureSemantics::Gauge | MeasureSemantics::ContextOccupancy
        )
    {
        return next;
    }
    if measurement.value.is_none() {
        next.unknown_total = true;
        return next;
    }
    let Some(value) = measurement.value else {
        return next;
    };
    if measurement.semantics == MeasureSemantics::Incremental {
        publish_output(&mut next, &measurement, value);
        return next;
    }
    let current = next.waters.get(&key).cloned();
    if measurement.trusted_reset {
        next.waters.insert(
            key,
            Water {
                max_seq: measurement.seq,
                value,
            },
        );
        publish_output(&mut next, &measurement, value);
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
            publish_output(&mut next, &measurement, value);
        }
        Some(water) => {
            if let (Some(seq), Some(max_seq)) = (measurement.seq, water.max_seq) {
                if seq <= max_seq {
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
            publish_output(&mut next, &measurement, confirmed - water.value);
        }
    }
    next
}

fn publish_output(state: &mut UsageState, measurement: &MeasurementV1, value: u64) {
    if measurement.counter_id == "output_tokens"
        && matches!(measurement.unit.as_str(), "token" | "tokens")
    {
        match state
            .confirmed_output_tokens
            .unwrap_or(0)
            .checked_add(value)
        {
            Some(total) => state.confirmed_output_tokens = Some(total),
            None => {
                state.unknown_total = true;
                state.uncertain = true;
            }
        }
    }
}
