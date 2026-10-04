//! Roundtable protocol contracts.
//!
//! `contract_profile=roundtable_plan_1_2`. This crate has no I/O, no agent
//! runtime, and no dependency on the application crate.

pub mod canonical;
pub mod dto;
pub mod model;

pub use canonical::{canonical_bytes, parse_strict_json, to_hex};
pub use dto::*;
pub use model::*;

pub fn decode_config(bytes: &[u8], limits: &ParseLimits) -> RtResult<RoundtableConfigV1> {
    decode_json(bytes, limits)
}
