use codeg_lib::acp::delegation::route::{
    resolve_route, DelegationConnectionOrigin, DelegationRoutePolicy, RouteResolutionInput,
    SuppressionCapability,
};
use codeg_lib::models::AgentType;

#[test]
fn managed_platforms_never_resolve_a_mixed_creation_route() {
    for agent in [
        AgentType::Codex,
        AgentType::Grok,
        AgentType::CodeBuddy,
        AgentType::ClaudeCode,
    ] {
        for policy in [DelegationRoutePolicy::Codeg, DelegationRoutePolicy::Native] {
            let plan = resolve_route(RouteResolutionInput {
                agent_type: agent,
                origin: DelegationConnectionOrigin::Root,
                session_override: Some(policy),
                global_policy: DelegationRoutePolicy::Codeg,
                delegation_enabled: true,
                suppression: SuppressionCapability::supported("delegation-route-v1"),
                agent_mcp_supported: true,
                companion_binary_available: true,
            })
            .unwrap();
            plan.assert_exclusive().unwrap();
            assert!(
                !(plan.native_creation_exposed() && plan.expose_codeg_delegation),
                "mixed route for {agent:?}",
            );
        }
    }
}

/// Regression: automation launch must use the explicit background prompt path.
///
/// `send_prompt_linked_with_message_id(..., delegation=None, ...)` sets
/// `mark_awaiting_reply = true` for roots. Background automation roots are not
/// awaiting-reply eligible, so the engine must call
/// `send_prompt_linked_background` (which hard-codes `mark_awaiting_reply=false`).
/// Source contract: the launch call site is not injectable without a larger
/// harness.
#[test]
fn automation_engine_uses_background_prompt_path_not_generic_with_message_id() {
    let engine_src = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/automation/engine.rs"
    ));

    assert!(
        engine_src.contains("send_prompt_linked_background("),
        "automation engine launch must call send_prompt_linked_background \
         (mark_awaiting_reply=false for background roots)"
    );
    assert!(
        !engine_src.contains("send_prompt_linked_with_message_id("),
        "automation engine launch must not call send_prompt_linked_with_message_id \
         (delegation=None would mark roots awaiting-reply eligible)"
    );
}
