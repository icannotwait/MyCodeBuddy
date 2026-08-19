import { describe, expect, it } from "vitest"

import type {
  AcpAgentInfo,
  AcpAgentStatus,
  ConversationTurnsPage,
  DbConversationDetail,
  PermissionOptionInfo,
  PreflightResult,
  SessionConfigOptionInfo,
} from "@/lib/types"

describe("merge repair transport contracts", () => {
  it("keeps ACP adapter fields on preflight, agent info, and status", () => {
    const preflight = {
      agent_type: "codex",
      agent_name: "Codex",
      passed: true,
      checks: [],
      adapter: {
        adapter_package: "@agentclientprotocol/codex-acp@1.1.9",
        adapter_cmd: "codex-acp",
        adapter_installed: true,
        native_cmd: "codex",
        native_label: "Codex CLI",
        native_path: "/usr/local/bin/codex",
        shared_config_dir: "/tmp/.codex",
        docs_url: "https://example.invalid/codex-acp",
      },
    } satisfies PreflightResult

    const agent = {
      agent_type: "codex",
      skills_capable: true,
      registry_id: "codex",
      registry_version: "1.1.9",
      name: "Codex",
      description: "Codex through its ACP adapter",
      available: true,
      distribution_type: "npx",
      is_acp_adapter: true,
      custom_source: null,
      enabled: true,
      show_thinking: true,
      sort_order: 0,
      installed_version: "1.1.9",
      env: {},
      config_json: null,
      config_file_path: null,
      opencode_auth_json: null,
      codex_auth_json: null,
      codex_config_toml: null,
      codex_model_catalog: null,
      codex_sandbox_settings: null,
      cline_secrets_json: null,
      hermes_config_yaml: null,
      grok_config_toml: null,
      grok_settings: null,
      cursor_cli_config_json: null,
      cursor_settings: null,
      model_provider_id: null,
      icon_url: null,
    } satisfies AcpAgentInfo

    const status = {
      agent_type: "codex",
      available: true,
      enabled: true,
      installed_version: "1.1.9",
      is_acp_adapter: true,
    } satisfies AcpAgentStatus

    expect(preflight.adapter.adapter_cmd).toBe("codex-acp")
    expect(agent.is_acp_adapter).toBe(true)
    expect(status.is_acp_adapter).toBe(true)
  })

  it("keeps boolean config and permission metadata discriminators", () => {
    const booleanOption = {
      id: "auto_approve",
      name: "Auto approve",
      description: null,
      category: null,
      kind: { type: "boolean", current_value: true },
    } satisfies SessionConfigOptionInfo

    const permissionOption = {
      option_id: "allow_always",
      name: "Always allow",
      kind: "allow_always",
      meta: {
        permission: {
          version: 1,
          changes: [],
        },
      },
    } satisfies PermissionOptionInfo

    expect(booleanOption.kind.type).toBe("boolean")
    expect(booleanOption.kind.current_value).toBe(true)
    expect(permissionOption.meta.permission.version).toBe(1)
  })

  it("keeps both conversation history window contracts", () => {
    const indexWindow = {
      turns: [],
      turns_offset: 40,
      turns_total: 80,
      assistant_turns_before_offset: 20,
      user_turns_before_offset: 20,
      prefix_hash: "0000000000000040",
      prefix_hash_before_index: "0000000000000080",
      uncovered_prefix_max_ts: "2026-08-15T00:00:00.000Z",
    } satisfies ConversationTurnsPage

    const userWindow = {
      summary: {
        id: 1,
        folder_id: 1,
        title: "Contract fixture",
        title_locked: false,
        auto_title_finalized: true,
        agent_type: "codex",
        status: "idle",
        awaiting_reply_token: null,
        kind: "regular",
        model: null,
        git_branch: null,
        external_id: "fixture-session",
        message_count: 0,
        child_count: 0,
        created_at: "2026-08-15T00:00:00.000Z",
        updated_at: "2026-08-15T00:00:00.000Z",
        pinned_at: null,
      },
      turns: [],
      history_window: {
        has_more_before: true,
        total_turn_count: 80,
        total_user_turn_count: 40,
        user_turn_limit: 20,
        returned_user_turn_count: 20,
      },
    } satisfies DbConversationDetail

    expect(indexWindow.turns_offset).toBe(40)
    expect(userWindow.history_window.user_turn_limit).toBe(20)
  })
})
