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
            assert_ne!(
                plan.native_creation_exposed() && plan.expose_codeg_delegation,
                true,
                "mixed route for {agent:?}",
            );
        }
    }
}
