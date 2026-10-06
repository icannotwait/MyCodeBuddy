//! Roundtable protocol contracts.
//!
//! `contract_profile=roundtable_plan_1_2`. This crate has no I/O, no agent
//! runtime, and no dependency on the application crate.

pub mod admission;
pub mod budget;
pub mod canonical;
pub mod completion;
pub mod context;
pub mod dto;
pub mod model;
pub mod projection;
pub mod strategy;
pub mod usage;
pub mod validation;

pub use admission::*;
pub use budget::*;
pub use canonical::{canonical_bytes, parse_strict_json, to_hex};
pub use completion::*;
pub use context::*;
pub use dto::*;
pub use model::*;
pub use projection::{project, RoomAggregate};
pub use strategy::*;
pub use usage::{fold_measurement, MeasureSemantics, MeasurementV1, UsageState};
pub use validation::{
    seat_schema_example, submit_candidate, submit_result_input_schema, validate_consensus,
    validate_result, AliasVisibility, ClaimRef, DecisionKind, EvidenceRef, FieldCode,
    RequiredTarget, ResponseRef, ResultScope, SubmissionDecision, SubmissionState, ValidatedResult,
    VisibleAliases, MAX_REPAIRABLE_INVALID_SUBMISSIONS, MAX_REPAIRABLE_SHAPE_SUBMISSIONS,
};

pub fn decode_config(bytes: &[u8], limits: &ParseLimits) -> RtResult<RoundtableConfigV1> {
    decode_json(bytes, limits)
}
