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
/// `send_prompt_linked` / `send_prompt_linked_with_message_id` set
/// `mark_awaiting_reply = true` for roots (`delegation=None`). Background
/// automation roots are not awaiting-reply eligible, so `launch` must call
/// `.send_prompt_linked_background(` (hard-codes `mark_awaiting_reply=false`).
/// Scoped to the `launch` body so comments/unrelated helpers cannot satisfy
/// the positive check.
#[test]
fn automation_engine_uses_background_prompt_path_not_generic_with_message_id() {
    let engine_src = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/automation/engine.rs"
    ));
    let launch_body = automation_launch_fn_body(engine_src);

    let bg_calls = launch_body.matches(".send_prompt_linked_background(").count();
    assert_eq!(
        bg_calls, 1,
        "automation launch must call .send_prompt_linked_background( exactly once \
         (mark_awaiting_reply=false for background roots); found {bg_calls}"
    );
    assert!(
        !launch_body.contains(".send_prompt_linked_with_message_id("),
        "automation launch must not call .send_prompt_linked_with_message_id( \
         (delegation=None would mark roots awaiting-reply eligible)"
    );
    // Bare `.send_prompt_linked(` also marks roots awaiting-reply eligible.
    // Strip the two longer names first so a residual bare call is detectable.
    let without_longer = launch_body
        .replace(".send_prompt_linked_background(", "")
        .replace(".send_prompt_linked_with_message_id(", "");
    assert!(
        !without_longer.contains(".send_prompt_linked("),
        "automation launch must not call bare .send_prompt_linked( \
         (mark_awaiting_reply=true for roots)"
    );
}

/// Slice of `engine.rs` for `AutomationEngine::launch`, bounded by stable
/// method signatures (not line numbers).
fn automation_launch_fn_body(engine_src: &str) -> &str {
    const START: &str = "async fn launch(&self, auto: &AutomationInfo, run_id: i32)";
    const END: &str = "async fn resolve_cwd(&self, auto: &AutomationInfo, run_id: i32)";
    let start = engine_src
        .find(START)
        .unwrap_or_else(|| panic!("missing launch marker: {START}"));
    let from_start = &engine_src[start..];
    let end = from_start
        .find(END)
        .unwrap_or_else(|| panic!("missing resolve_cwd marker after launch: {END}"));
    &from_start[..end]
}
