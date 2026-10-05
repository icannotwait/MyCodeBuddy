#[path = "roundtable_support/mod.rs"]
pub mod support;

#[path = "roundtable_cases/sandbox.rs"]
mod sandbox;

#[path = "roundtable_cases/gateway.rs"]
mod gateway;

#[path = "roundtable_cases/runtime_admission.rs"]
mod runtime_admission;

#[path = "roundtable_cases/private_ingress.rs"]
mod private_ingress;

#[path = "roundtable_cases/companion.rs"]
mod companion;

#[path = "roundtable_cases/capability_boundary.rs"]
mod capability_boundary;

#[path = "roundtable_cases/registry.rs"]
mod registry;

#[path = "roundtable_cases/roundtable_qualification.rs"]
mod roundtable_qualification;

#[path = "roundtable_cases/runtime_scheduler.rs"]
mod runtime_scheduler;
