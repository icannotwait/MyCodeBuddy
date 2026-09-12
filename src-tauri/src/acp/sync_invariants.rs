//! Invariants the 0.30.7 sync must not lose. Written before the merge so a
//! resolve that restores OpenClaw, or bumps the registry's CodeBuddy pin
//! without the fork-only `route.rs` constant, fails a named test.

use crate::acp::delegation::route::PINNED_CODEBUDDY_VERSION;
use crate::acp::registry::get_agent_meta;
use crate::models::agent::{AgentType, BUILTIN_AGENT_TYPES};

#[test]
fn openclaw_is_not_a_builtin_agent() {
    for agent in BUILTIN_AGENT_TYPES {
        assert_ne!(
            agent.as_wire().as_ref(),
            "open_claw",
            "{agent:?} must not serialize as open_claw"
        );
    }
    assert_eq!(
        AgentType::from_wire("open_claw"),
        None,
        "from_wire must not revive OpenClaw"
    );
}

/// `route.rs` is fork-only and never conflicts, so a registry bump (Task 5
/// takes upstream's 2.149.0) leaves it stale unless someone edits it by hand.
/// No version literal here on purpose: the invariant is agreement, not a number.
#[test]
fn codebuddy_registry_pin_matches_route_constant() {
    assert_eq!(
        get_agent_meta(AgentType::CodeBuddy).registry_version(),
        Some(PINNED_CODEBUDDY_VERSION)
    );
}
