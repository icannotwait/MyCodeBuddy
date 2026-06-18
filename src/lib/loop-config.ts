import type { IssueConfig } from "./types"

/**
 * The engine's default `IssueConfig`, mirroring Rust `IssueConfig::default()`
 * (src-tauri/src/models/loops.rs). Used as the front-end fallback when a space
 * has no `default_config` set yet, so the space-defaults editor and an
 * inheriting issue's read-only preview show the same baseline the backend would
 * resolve to.
 */
export function defaultIssueConfig(): IssueConfig {
  return {
    agents: { default: { agent: "claude_code", config_values: {} } },
    validation_commands: [],
    // One reviewer that inherits the default agent.
    reviewers: [{ inherit: true }],
    review_pass_rule: "unanimous",
    max_attempts: 6,
    oscillation_limit: 2,
    auto_merge: false,
    force_route: null,
    iteration_timeout_secs: null,
    token_budget_per_turn: null,
    stall_alert_secs: null,
  }
}
