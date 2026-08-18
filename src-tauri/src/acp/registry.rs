use crate::models::agent::{
    AgentType, BUILTIN_AGENT_REGISTRY_ALIASES, BUILTIN_AGENT_REGISTRY_IDS, BUILTIN_AGENT_WIRE_NAMES,
};

#[derive(Debug, Clone)]
pub enum AgentDistribution {
    Npx {
        version: &'static str,
        package: &'static str,
        /// The command name provided by this npx package (e.g. "gemini").
        cmd: &'static str,
        args: &'static [&'static str],
        env: &'static [(&'static str, &'static str)],
        /// Minimum Node.js version required, e.g. "22.12.0". None means no specific requirement.
        node_required: Option<&'static str>,
    },
    Binary {
        version: &'static str,
        /// Command name on PATH (fallback launch + `which` probes). For
        /// single-file archives this is also the file name copied out of the
        /// archive into the cache.
        cmd: &'static str,
        args: &'static [&'static str],
        env: &'static [(&'static str, &'static str)],
        platforms: &'static [PlatformBinary],
        /// `None`: the archive contains one self-contained binary named `cmd`,
        /// which is copied out into the cache (OpenCode). `Some`: the archive
        /// is a whole directory tree that must stay intact (bundled runtime,
        /// e.g. Cursor's agent-cli-package); everything is extracted into the
        /// per-version cache dir and the entry path inside it is launched.
        dir_entry: Option<BinaryDirEntry>,
    },
    Bundled {
        version: &'static str,
        cmd: &'static str,
        args: &'static [&'static str],
        env: &'static [(&'static str, &'static str)],
        override_env: &'static str,
        platforms: &'static [&'static str],
    },
    /// Custom Python agents launched through `uvx` (the `uv` tool runner),
    /// which fetches and caches the pinned package on first use.
    Uvx {
        version: &'static str,
        /// The `uvx --from` package spec.
        package: &'static str,
        /// The console-script entry point to run, e.g. "hermes-acp".
        cmd: &'static str,
        args: &'static [&'static str],
        env: &'static [(&'static str, &'static str)],
        /// Minimum `uv` version required, e.g. "0.5.0". None means no specific requirement.
        uv_required: Option<&'static str>,
        /// Interpreter to pin via `uvx --python <ver>`, e.g. `Some("3.13")`.
        /// `None` lets uvx pick its default interpreter. Set this when the
        /// package (or a transitive dep) does not support the machine's default
        /// Python — uv auto-downloads a managed build of the pinned version.
        python: Option<&'static str>,
        /// Fallback command resolvable on PATH when `uvx` is unavailable, e.g.
        /// `Some(("hermes", &["acp"]))` — lets users who installed the agent via
        /// its official installer launch it without `uv`.
        system_cmd: Option<(&'static str, &'static [&'static str])>,
    },
}

#[derive(Debug, Clone)]
pub struct PlatformBinary {
    pub platform: &'static str,
    pub url: &'static str,
    /// Expected hex SHA-256 of the downloaded archive, verified before the
    /// archive is unpacked. `None` for built-ins: their URLs are repository
    /// constants reviewed with the code. Custom agents download from
    /// user-supplied URLs, so the ACP registry's `sha256` is carried through
    /// and enforced whenever it is published.
    pub sha256: Option<&'static str>,
}

/// Launch entry inside an extracted directory-tree archive (see
/// [`AgentDistribution::Binary::dir_entry`]). Paths are relative to the
/// archive root, '/'-separated; `windows` names the `.cmd`/`.bat` shim.
#[derive(Debug, Clone, Copy)]
pub struct BinaryDirEntry {
    pub unix: &'static str,
    pub windows: &'static str,
}

impl BinaryDirEntry {
    /// Entry path for the current platform.
    pub fn for_current_platform(&self) -> &'static str {
        if cfg!(windows) {
            self.windows
        } else {
            self.unix
        }
    }
}

#[derive(Debug, Clone)]
pub struct AcpAgentMeta {
    pub agent_type: AgentType,
    /// 是否经 ACP 线缆（session/new 的 `mcpServers` 字段）向该 agent 转发 MCP
    /// 服务器——既包括用户配置的服务器，也包括内置 codeg-mcp 伴生进程。
    pub supports_mcp: bool,
    pub name: &'static str,
    pub description: &'static str,
    pub distribution: AgentDistribution,
}

impl AgentDistribution {
    pub fn env(&self) -> &'static [(&'static str, &'static str)] {
        match self {
            Self::Npx { env, .. }
            | Self::Binary { env, .. }
            | Self::Bundled { env, .. }
            | Self::Uvx { env, .. } => env,
        }
    }
}

impl AcpAgentMeta {
    pub fn registry_version(&self) -> Option<&'static str> {
        match &self.distribution {
            AgentDistribution::Npx { version, .. }
            | AgentDistribution::Binary { version, .. }
            | AgentDistribution::Bundled { version, .. }
            | AgentDistribution::Uvx { version, .. } => Some(*version),
        }
    }
}

pub fn current_platform() -> &'static str {
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    {
        "darwin-aarch64"
    }
    #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
    {
        "darwin-x86_64"
    }
    #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
    {
        "linux-aarch64"
    }
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    {
        "linux-x86_64"
    }
    #[cfg(all(target_os = "windows", target_arch = "aarch64"))]
    {
        "windows-aarch64"
    }
    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    {
        "windows-x86_64"
    }
}

/// The twelve built-in agents. Excludes user-registered custom agents — use
/// [`all_acp_agents`] for the live set.
pub fn builtin_acp_agents() -> Vec<AgentType> {
    vec![
        AgentType::ClaudeCode,
        AgentType::Codex,
        AgentType::Gemini,
        AgentType::OpenCode,
        AgentType::Cline,
        AgentType::Hermes,
        AgentType::CodeBuddy,
        AgentType::KimiCode,
        AgentType::Pi,
        AgentType::Grok,
        AgentType::Cursor,
        AgentType::DeepSeek,
    ]
}

/// Every agent codeg can currently drive: the twelve built-ins followed by the
/// user's registered custom ACP agents (sorted by id).
pub fn all_acp_agents() -> Vec<AgentType> {
    let mut agents = builtin_acp_agents();
    agents.extend(crate::acp::custom_registry::all());
    agents
}

pub fn registry_id_for(agent_type: AgentType) -> &'static str {
    match agent_type {
        AgentType::ClaudeCode => "claude-acp",
        AgentType::Codex => "codex-acp",
        AgentType::Gemini => "gemini",
        AgentType::OpenCode => "opencode",
        AgentType::Cline => "cline",
        AgentType::Hermes => "hermes",
        AgentType::CodeBuddy => "codebuddy-code",
        AgentType::KimiCode => "kimi-code",
        AgentType::Pi => "pi-acp",
        AgentType::Grok => "grok-build",
        AgentType::Cursor => "cursor",
        AgentType::DeepSeek => "deepseek-acp",
        // A custom agent's registry id IS its identity.
        AgentType::Custom(id) => id,
    }
}

/// Whether an id is owned by a built-in integration in any accepted namespace:
/// wire/storage name, canonical ACP registry id, or public-catalog alias.
pub fn is_reserved_builtin_id(id: &str) -> bool {
    BUILTIN_AGENT_WIRE_NAMES.contains(&id)
        || BUILTIN_AGENT_REGISTRY_IDS.contains(&id)
        || BUILTIN_AGENT_REGISTRY_ALIASES.contains(&id)
}

pub fn from_registry_id(id: &str) -> Option<AgentType> {
    match id {
        "claude-acp" => Some(AgentType::ClaudeCode),
        "codex-acp" => Some(AgentType::Codex),
        "gemini" => Some(AgentType::Gemini),
        "opencode" => Some(AgentType::OpenCode),
        "cline" => Some(AgentType::Cline),
        "hermes" => Some(AgentType::Hermes),
        "codebuddy-code" => Some(AgentType::CodeBuddy),
        "kimi-code" => Some(AgentType::KimiCode),
        "pi-acp" => Some(AgentType::Pi),
        "grok-build" => Some(AgentType::Grok),
        "cursor" => Some(AgentType::Cursor),
        "deepseek-acp" => Some(AgentType::DeepSeek),
        // Only ids the user has actually registered resolve. An unregistered
        // id must stay `None` so the ACP-registry picker still offers it as
        // "addable" rather than treating it as already supported.
        other => crate::acp::custom_registry::is_registered(other)
            .then(|| AgentType::custom(other))
            .flatten(),
    }
}

#[derive(Debug, Clone, Copy)]
pub struct AcpAdapterRelation {
    pub native_cmd: &'static str,
    pub native_label: &'static str,
    pub shared_config_dir: &'static str,
    pub extra_dirs: &'static [&'static str],
    pub docs_url: &'static str,
}

pub fn acp_adapter_relation(agent_type: AgentType) -> Option<AcpAdapterRelation> {
    match agent_type {
        AgentType::ClaudeCode => Some(AcpAdapterRelation {
            native_cmd: "claude",
            native_label: "Claude Code CLI",
            shared_config_dir: "~/.claude",
            extra_dirs: &[".local/bin", ".claude/local"],
            docs_url: ACP_ADAPTER_DOCS_URL,
        }),
        AgentType::Codex => Some(AcpAdapterRelation {
            native_cmd: "codex",
            native_label: "Codex CLI",
            shared_config_dir: "~/.codex",
            extra_dirs: &[".local/bin"],
            docs_url: ACP_ADAPTER_DOCS_URL,
        }),
        _ => None,
    }
}

const ACP_ADAPTER_DOCS_URL: &str = "https://docs.codeg.app/guide/supported-agents#acp-adapters";

/// All platforms default app-server (`CODEX_ACP_USE_CLI=0`); Agent Settings
/// can pin `CODEX_ACP_USE_CLI=1` for CLI exec (`codex exec --json`).
const CODEX_CLI_RUNTIME_DEFAULT_ENV: &[(&str, &str)] = &[("CODEX_ACP_USE_CLI", "0")];

/// Official npm package for Codex ACP on every platform (including Windows).
///
/// Windows previously shipped a sibling `codex-acp.exe` MyCodeBuddy fork; that
/// path is retired so launch always resolves through npm/npx the same way as
/// macOS/Linux. The package embeds `@openai/codex` for app-server; host
/// `CODEX_PATH` is only required when the user opts into CLI exec mode.
fn codex_distribution() -> AgentDistribution {
    AgentDistribution::Npx {
        version: "1.4.0",
        package: "@agentclientprotocol/codex-acp@1.4.0",
        cmd: "codex-acp",
        args: &[],
        env: CODEX_CLI_RUNTIME_DEFAULT_ENV,
        node_required: Some("20.0.0"),
    }
}

/// Minimum adapter version whose `_session/steering` honors the
/// `_meta.steering.idleBehavior = "promptRequired"` opt-in — one of the three
/// gates for codeg's NATIVE live-feedback push channel (synthesized into
/// `SessionState.native_steering_available` at initialize; see
/// `connection.rs::init_advertises_steering`).
///
/// `None` means "never steer natively" even when the adapter advertises
/// `_meta.steering.supported`: an adapter that ignores the opt-in falls back
/// to `startedNewTurn` on the turn-end race — a detached turn no host request
/// owns, which codeg's turn-scoped runtime must never trigger. codex-acp
/// ships `_session/steering` but not `promptRequired` — re-verified against
/// the published 1.3.0 tarball (zero hits, same as 1.1.9) — so it stays
/// `None` until a release implements the opt-in — then this is a one-line
/// flip plus tests.
///
/// Honoring the opt-in is necessary but not sufficient: the ACTIVE path must
/// also keep the owning `session/prompt` in flight across the steered work
/// (see the per-arm rationale below).
///
/// The static policy alone is NOT enough — launch prefers a PATH-resolved,
/// user-installed adapter over the pinned npx package (see
/// `commands::acp::acp_get_agent_status_core`, "Launch already prefers the
/// PATH resolution"), so the synthesis must ALSO prove the running binary's
/// `agent_info.version` meets this minimum.
pub fn steering_prompt_required_min_version(agent_type: AgentType) -> Option<&'static str> {
    match agent_type {
        // 0.64.0 (#919) added the `promptRequired` opt-in, but the ACTIVE path
        // stayed unsound until 0.65.0 (#958): steering is delivered at priority
        // `now`, which makes the CLI ABORT the running cycle, and that cycle's
        // ordinary result settled the owning `session/prompt` as a clean
        // `end_turn` while the steered work was still going — the continuation
        // then streamed with no turn in flight (#934, reported and reproduced
        // from codeg). 0.65.0 records a steered turn's results instead of
        // settling on them and settles at the SDK `idle` spanning both cycles,
        // so the floor is the FIRST release carrying that fix, not the one that
        // introduced the opt-in. Every 0.64.x — including 0.64.2, which only
        // reverted an unrelated ExitPlanMode change — still carries the bug and
        // is held to the pull channel by the runtime version gate.
        AgentType::ClaudeCode => Some("0.65.0"),
        _ => None,
    }
}

/// Whether this adapter's goal-control request reaches the agent OUT OF BAND —
/// i.e. it changes the goal through its own channel instead of riding the
/// session's prompt stream.
///
/// This is what decides whether pausing/clearing a goal may ALSO interrupt the
/// turn that is running. Neither adapter's control request stops a turn on its
/// own: codex's `pause` is `thread/goal/set{status:"paused"}` and its `clear`
/// is `thread/goal/clear` — pure app-server metadata that only takes effect at
/// the next idle point, so the agent visibly keeps working for as long as the
/// current turn lasts (which, mid goal loop, is "forever" as far as the user is
/// concerned). Interrupting is the missing half of the button, and it is safe
/// there precisely because the goal RPC already landed before the interrupt.
///
/// claude is the counter-example and the reason this is a policy bit rather
/// than an unconditional behavior: `claude-agent-acp`'s `_session/goal` handler
/// rewrites the request into the text `"/goal clear"` and delivers it as a
/// STEERING message (falling back to a fresh `session/prompt` when idle).
/// Cancelling the turn would kill the very message that carries the clear, so
/// the goal would stay armed — strictly worse than doing nothing. Everything
/// else, custom agents included, fails closed onto `false`: an unknown
/// adapter's control channel is not something to guess at with a destructive
/// action.
pub fn goal_control_is_out_of_band(agent_type: AgentType) -> bool {
    matches!(agent_type, AgentType::Codex)
}

pub fn get_agent_meta(agent_type: AgentType) -> AcpAgentMeta {
    if let AgentType::Custom(id) = agent_type {
        return crate::acp::custom_registry::get(id)
            .cloned()
            .unwrap_or_else(|| crate::acp::custom_registry::unregistered_meta(id));
    }
    debug_assert_eq!(
        from_registry_id(registry_id_for(agent_type)),
        Some(agent_type)
    );
    match agent_type {
        AgentType::ClaudeCode => AcpAgentMeta {
            agent_type,
            supports_mcp: true,
            name: "Claude Code",
            description: "ACP wrapper for Anthropic's Claude",
            // 0.63.0 (claude-agent-sdk 0.3.220) adds the opt-in
            // `clientCapabilities._meta["subagent-transcript"]` capability
            // (#881): when advertised (see `build_client_capabilities`),
            // subagent text/thought chunks stream with update-level
            // `_meta.claudeCode.parentToolUseId` instead of being filtered;
            // codeg routes them into the live Agent capsule. Independent of
            // the capability, every tool_call now carries
            // `_meta.claudeCode.subagent: true` on Agent/Task launches and
            // `_meta.claudeCode.title` (the Bash `description` input) on
            // normal AND eager-permission tool calls. 0.63.0 also fixes
            // phantom `tool_progress` heartbeat entries under never-announced
            // ids (#916), Bash terminal metas keyed off an empty id (#917),
            // and `permission_denied` resolving unannounced tool calls
            // (#923). Fast mode's config option now folds the SDK's
            // `fast_mode_disabled_reason` into its description (#921).
            // 0.64.0 carries the SAME claude-agent-sdk (0.3.220) and ACP SDK
            // (1.3.0), so Claude Code's own behavior is unchanged. It adds an
            // opt-in host-owned steering fallback (#919): a `_session/steering`
            // request may carry `_meta.steering.idleBehavior = "promptRequired"`,
            // and when the turn it meant to steer already settled the adapter
            // returns `{outcome:"promptRequired", reason:"noRunningTurn"}`
            // WITHOUT consuming the content, so the host resubmits it through a
            // normal `session/prompt` it owns. 0.64.0 also marks the
            // per-question
            // free-text "Other" elicitation field with the deliberately
            // un-namespaced `_meta._askUserQuestionCustomAnswer` (#929, omitted
            // from the release notes) — see `question::is_custom_answer_property`.
            // 0.64.1 (#930, likewise absent from its notes) adopts the
            // option-level `_meta.permission = {version, changes[]}` contract
            // codex already speaks, so Claude permission cards now spell out
            // what each button grants; its `lifetime` is what
            // `parsePermissionOptionChanges` reads for the duration, since
            // Claude — unlike codex — never states it in the description.
            // 0.65.0 (#958) completes the steering contract the opt-in started:
            // a steered turn's results now only RECORD their outcome and the
            // turn settles at the SDK `idle` spanning both the interrupted and
            // the steered cycle, so the owning prompt stays in flight until the
            // steered work is actually done. That is the fix for #934, which
            // had forced codeg's native push channel off; it is back on for
            // Claude alone via `steering_prompt_required_min_version` (see
            // `manager::submit_feedback` for the two channels). Its other
            // releases carry nothing else for codeg: 0.64.2 reverted #938's
            // ExitPlanMode `plan_update` experiment outright, and 0.65.0's
            // remaining commits are devDependency bumps — the runtime deps and
            // the Node floor still match 0.64.0's.
            // 0.66.0 (#964) introduces the provider-neutral goal extension:
            // initialize advertises `_meta.goal = {version: 1, controlMethod:
            // "_session/goal", actions}` (claude offers ["set", "clear"]) and
            // goal state arrives as `session_info_update._meta.goal` snapshots
            // — {objective, status (active|paused|blocked|limited|complete),
            // iterations?, lastReason?, createdAt/updatedAt (Unix ms),
            // tokenBudget?, tokensUsed?, timeUsedSeconds?, controlMethod},
            // `goal: null` clears. codeg picks the goal channel per connection
            // at initialize (advertised ⇒ neutral only; see the
            // SessionInfoUpdate arm in connection.rs), the same selection that
            // keeps codex goals alive after its silent 1.2.0 switch. #967
            // fixes goal publish/replace reliability.
            // 0.67.0 bumps claude-agent-sdk 0.3.220→0.3.232 and joins the
            // JetBrains AIR extension codex 1.2.0 speaks (#979; record shape
            // finalized in 0.68.0/#992): typed session failures ride
            // `session_info_update._meta.jetbrains.air.sessionFailure` as
            // {id, revision (per-id from 1), category (connection|access|
            // limit|request|service|unknown), severity (warning|error), title,
            // details?, actions (subset of retry|login|new_session)} — upsert
            // records ONLY, no resolve/tombstone wire; publication is STRICTLY
            // gated on the client advertising
            // `clientCapabilities._meta.jetbrains.air = {version >= 1,
            // capabilities: ["sessionFailure"]}`. codeg advertises it
            // (`build_client_capabilities`) and projects the records into the
            // session-failure banner (`AcpEvent::SessionFailure`); on
            // session/load the adapter re-publishes still-active failures
            // (deliberate; id+revision merging absorbs it). A model fallback
            // (`model_refusal_fallback`)
            // publishes an AIR "advisory" record behind the same gate (#990).
            // Skill tool calls now carry `_meta.claudeCode.skill` plus
            // `skillPath` when a SKILL.md is located (#986) — input for a
            // future dedicated card. Task plans survive across prompts
            // (#974), file-preparing tool calls get a pending title (#978),
            // and the default model option description names the resolved
            // model (#982) — all through existing generic render paths.
            // Compaction remains plain text ("Compacting...") with NO
            // `contextCompaction` meta, so the compaction card stays a
            // codex/grok surface. 0.68.0 (#992) only realigns the failure
            // record (categories narrowed to the six above, actions to the
            // three above). The steering `promptRequired` path is intact
            // across 0.66–0.68 (tarball-verified), so the 0.65.0 floor keeps
            // holding, and `engines.node` stays ">=22".
            // 0.69.0 adds exactly ONE thing (tarball-diffed: the only new
            // string literals in `acp-agent.js` are "collecting",
            // "notReported", "providerError", plus the new
            // `dist/file-change-audit.js`) — the AIR `agentFileChangeReport`
            // capability, shipped in lockstep with codex-acp 1.4.0. It is
            // OFF unless the client asks for it twice, and codeg deliberately
            // asks for neither; see `build_client_capabilities` in
            // connection.rs for the reasoning. The only ambient change is that
            // `airSessionFailureCapabilityMeta` became variadic so the agent
            // can advertise `["sessionFailure", "agentFileChangeReport"]` — an
            // ADDITIVE element in an array codeg only ever membership-tests,
            // so the session-failure gate is unaffected.
            distribution: AgentDistribution::Npx {
                version: "0.69.0",
                package: "@agentclientprotocol/claude-agent-acp@0.69.0",
                cmd: "claude-agent-acp",
                args: &[],
                env: &[],
                node_required: Some("22.0.0"),
            },
        },
        AgentType::Codex => AcpAgentMeta {
            agent_type,
            supports_mcp: true,
            name: "Codex CLI",
            description: "ACP adapter for OpenAI's coding assistant",
            // codex-acp moved from zed-industries (Rust binary) to the
            // agentclientprotocol org (TypeScript rewrite, npx-distributed).
            // codex-acp 1.1.x can drive either
            // `codex app-server` or (when `CODEX_ACP_USE_CLI=1`) `codex exec`.
            // All platforms (including Windows) use the same npm package —
            // no bundled sibling `codex-acp.exe`. Default app-server via
            // distribution env; opt in with CODEX_ACP_USE_CLI=1 in Agent
            // Settings. Since 1.0.1 it also resolves the resumed
            // `model_provider` from `~/.codex/config.toml` (#224), so codeg
            // no longer injects `MODEL_PROVIDER` to keep resumed sessions on
            // the custom provider. 1.1.0 (#263) also reports `/goal`
            // transitions as a structured `session_info_update`
            // (`_meta.codex.goal`) rather than live agent text — see
            // `crate::acp::codex_goal`. 1.1.3+ adds three
            // new live signals codeg handles in `connection::emit_conversation_update`:
            // `subAgentActivity` tool calls (#304, suppressed via
            // `is_codex_subagent_activity` — redundant with the collab capsule),
            // retryable turn errors (#289, `_meta.codex.error` → a transient
            // retry banner via `codex_retry_indicator`), and the
            // context-compaction lifecycle (#288, `_meta.contextCompaction` tool
            // call → a dedicated frontend card). 1.1.x also adds Plan mode: the
            // `collaboration_mode` config option (rendered by the generic
            // config-option path) and native `request_user_input`, delivered as
            // an ACP `elicitation/create` request — codeg advertises
            // `elicitation.form` for Codex and bridges the WHOLE form surface
            // (Plan-mode questions, MCP-server forms, MCP tool-call approvals)
            // in `handle_elicitation_request` / `question::classify_elicitation`.
            // 1.1.5 (#322) also widened codex-acp's MCP config filtering to
            // project `.codex` layers, which is why codeg forces
            // `DISABLE_MCP_CONFIG_FILTERING` (see `apply_codex_env_policy`) so
            // the injected `codeg-mcp` server always survives. 1.1.6 adds
            // steering (#309): `_session/steering` injects a user prompt into
            // the LIVE turn (initialize advertises `_meta.steering.supported`)
            // — not wired into codeg yet. 1.1.7 (#326) emits Plan-mode plan
            // contents as a plain `agent_message_chunk`
            // (`_meta.codex.phase = "final_answer"`, no `<proposed_plan>` tags),
            // which the adapter's tag-splitter simply no-ops on — tagged output
            // from older codex still renders as the proposed-plan card. 1.1.8
            // (#351) gates Plan mode behind a review confirmation: when a plan
            // item completes while `collaboration_mode` is `plan`, codex-acp
            // sends a `session/request_permission` marked
            // `_meta.codex = {kind:"plan_review", planItemId}` whose `toolCall`
            // (`plan-review:<itemId>`, kind `switch_mode`, `rawInput.plan`) was
            // NEVER announced as a `tool_call` — codeg seeds it from the request
            // (see `is_codex_plan_review` / `handle_permission_request`) so the
            // follow-up `tool_call_update` has a card to merge into. On approval
            // codex flips the mode back to default (a mid-turn
            // `config_option_update`) and runs the implementation turn inside
            // the SAME `session/prompt`. 1.1.8 (#342) also hangs a structured
            // `_meta.permission = {version, changes[]}` on each permission
            // option, whose `changes[].description` codeg surfaces in the
            // permission card — the contract claude-agent-acp joined in 0.64.1
            // (#930), so that rendering is no longer codex-only. The
            // `clientCapabilities.plan` path (structured
            // `plan_update`s) does NOT apply: sacp 11.0.0's schema has neither
            // the capability nor the session-update variant, so plans keep
            // arriving as `agent_message_chunk`s. That also makes 1.1.9 (#354,
            // which coalesces streamed plan snapshots to one `plan_update`
            // every 150ms and flushes them at item/turn/permission boundaries)
            // inert here — it only runs behind that capability, and its
            // dependency set is byte-identical to 1.1.8's. 1.1.9 still declares
            // no `engines.node`, so the 20.0.0 floor is retained.
            // 1.2.0 rewires two surfaces UNANNOUNCED in its release notes (the
            // #929/#930 pattern again; both tarball-verified): (a) `/goal`
            // transitions move to the provider-neutral
            // `session_info_update._meta.goal` snapshot the claude adapter
            // speaks since 0.66.0 — the legacy `_meta.codex.goal` key is GONE,
            // so codeg's initialize-pinned channel selection (SessionInfoUpdate
            // arm, connection.rs) is what keeps GoalCard alive from this
            // release on. The neutral snapshot collapses usageLimited/
            // budgetLimited into status "limited", adds paused/blocked,
            // carries createdAt/updatedAt in Unix ms and embeds controlMethod
            // "_session/goal" (actions [set, pause, resume, clear]; the old
            // "_codex/session/goal_control" name is kept as an accepted
            // alias). (b) It joins the JetBrains AIR extension (#383; aligned
            // to the final record shape in 1.3.0/#393, same wire as
            // claude-agent-acp 0.67.0 — see that entry): typed session
            // failures gated STRICTLY on the client advertising
            // `clientCapabilities._meta.jetbrains.air`, which codeg does
            // (`build_client_capabilities`). Advertising REPLACES the legacy
            // surfaces on the wire: `_meta.codex.error` (both willRetry
            // branches), warning/config-warning text chunks and dropped
            // deprecation notices all become AIR records (willRetry ⇒
            // severity "warning", terminal ⇒ "error" that deliberately stays
            // active; recovery is implied by turn progress, never published).
            // `codex_retry_indicator` therefore no longer fires on these
            // connections; severity-"warning" records take over the
            // retry-banner role (see the SessionFailure consumer in
            // connection.rs). #377 also normalizes cwd
            // filters for Windows session listing. 1.3.0 (#396) upgrades the
            // compaction lifecycle to the claude-aligned synthetic tool call
            // (kind "think", title "Compact conversation") whose meta is now
            // the versioned object `{contextCompaction: {version: 1}}` — the
            // `contextCompaction: true` boolean marker from 1.1.3 is gone
            // (the schema reserves trigger/preTokens/postTokens/durationMs/
            // error, but 1.3.0 emits none of them). The frontend predicate
            // (`src/lib/context-compaction.ts`) accepts both shapes: the Grok
            // bridge still synthesizes the boolean. #400 restores native
            // provider state after overrides (matters for BYO-provider model
            // switching). New in the bundle and fine on generic cards:
            // synthetic "Guardian Review" tool calls (kind "think") and
            // fuzzyFileSearch ids. Structured plan_update stays inert — its
            // gate is a TOP-LEVEL `clientCapabilities.plan` field sacp 11.0.0
            // cannot express. 1.3.0 still ships NO steering `promptRequired`
            // opt-in (tarball grep: zero hits ⇒ the arm below stays None) and
            // still declares no `engines.node`, so the 20.0.0 floor is
            // retained.
            // 1.4.0 is the codex half of the same AIR `agentFileChangeReport`
            // release as claude-agent-acp 0.69.0 and carries nothing else
            // (tarball-diffed: the only new string literals are that feature's
            // plus the `thread/fork` app-server method it is built on;
            // `@openai/codex` stays ^0.147.0, so the CLI and its native
            // team-of-agents surface are unchanged). codex implements the
            // audit by forking the thread (`approvalPolicy: "never"`,
            // `sandbox: "read-only"`, `ephemeral: true`) and running an extra
            // turn on the fork; claude uses a Stop hook plus a hidden
            // continuation. Either way it is an extra model round-trip per
            // prompt, and it is gated on a client advertisement codeg does not
            // make — see `build_client_capabilities` in connection.rs.
            distribution: codex_distribution(),
        },
        AgentType::Gemini => AcpAgentMeta {
            agent_type,
            supports_mcp: true,
            name: "Gemini CLI",
            description: "Google's official CLI for Gemini",
            distribution: AgentDistribution::Npx {
                version: "0.55.1",
                package: "@google/gemini-cli@0.55.1",
                cmd: "gemini",
                args: &["--acp", "--skip-trust"],
                env: &[],
                node_required: Some("20.0.0"),
            },
        },
        AgentType::Cline => AcpAgentMeta {
            agent_type,
            supports_mcp: true,
            name: "Cline",
            description: "Autonomous coding agent CLI",
            distribution: AgentDistribution::Npx {
                version: "3.0.55",
                package: "cline@3.0.55",
                cmd: "cline",
                args: &["--acp"],
                env: &[],
                node_required: Some("22.0.0"),
            },
        },
        AgentType::OpenCode => AcpAgentMeta {
            agent_type,
            supports_mcp: true,
            name: "OpenCode",
            description: "The open source coding agent",
            distribution: AgentDistribution::Binary {
                version: "1.18.18",
                cmd: "opencode",
                args: &["acp"],
                env: &[],
                platforms: &[
                    PlatformBinary {
                        platform: "darwin-aarch64",
                        url: "https://github.com/anomalyco/opencode/releases/download/v1.18.18/opencode-darwin-arm64.zip",
                        sha256: None,
                    },
                    PlatformBinary {
                        platform: "darwin-x86_64",
                        url: "https://github.com/anomalyco/opencode/releases/download/v1.18.18/opencode-darwin-x64.zip",
                        sha256: None,
                    },
                    PlatformBinary {
                        platform: "linux-aarch64",
                        url: "https://github.com/anomalyco/opencode/releases/download/v1.18.18/opencode-linux-arm64.tar.gz",
                        sha256: None,
                    },
                    PlatformBinary {
                        platform: "linux-x86_64",
                        url: "https://github.com/anomalyco/opencode/releases/download/v1.18.18/opencode-linux-x64.tar.gz",
                        sha256: None,
                    },
                    PlatformBinary {
                        platform: "windows-aarch64",
                        url: "https://github.com/anomalyco/opencode/releases/download/v1.18.18/opencode-windows-arm64.zip",
                        sha256: None,
                    },
                    PlatformBinary {
                        platform: "windows-x86_64",
                        url: "https://github.com/anomalyco/opencode/releases/download/v1.18.18/opencode-windows-x64.zip",
                        sha256: None,
                    },
                ],
                dir_entry: None,
            },
        },
        AgentType::Hermes => AcpAgentMeta {
            agent_type,
            supports_mcp: true,
            name: "Hermes Agent",
            description: "Nous Research's self-improving agent (ACP)",
            distribution: AgentDistribution::Npx {
                version: "0.20.1",
                package: "hermes-agent@0.20.1",
                cmd: "hermes",
                args: &["acp"],
                env: &[],
                node_required: Some("20.0.0"),
            },
        },
        AgentType::CodeBuddy => AcpAgentMeta {
            agent_type,
            supports_mcp: true,
            name: "CodeBuddy",
            description: "Tencent Cloud's official AI coding assistant (ACP)",
            distribution: AgentDistribution::Npx {
                version: "2.137.0",
                package: "@tencent-ai/codebuddy-code@2.137.0",
                cmd: "codebuddy",
                args: &["--acp"],
                env: &[],
                node_required: Some("22.0.0"),
            },
        },
        AgentType::KimiCode => AcpAgentMeta {
            agent_type,
            supports_mcp: true,
            name: "Kimi Code",
            description: "Moonshot AI's official CLI coding assistant (ACP)",
            distribution: AgentDistribution::Npx {
                version: "0.36.1",
                package: "@moonshot-ai/kimi-code@0.36.1",
                cmd: "kimi",
                args: &["acp"],
                env: &[],
                node_required: Some("22.19.0"),
            },
        },
        AgentType::Pi => AcpAgentMeta {
            agent_type,
            // pi-acp accepts ACP-wire `mcpServers` but drops them (does not
            // forward to pi), and pi has no native MCP. supports_mcp stays
            // Actual wire forwarding is short-circuited in `connection.rs` (see
            // the skip-list), so neither user servers nor the codeg-mcp companion
            // are futilely forwarded.
            supports_mcp: true,
            name: "Pi",
            description: "Self-extensible coding agent (ACP via pi-acp)",
            // pi-acp 0.0.33 spawns `pi --mode rpc` as a child, so `pi` (npm
            // `@earendil-works/pi-coding-agent`) must be resolvable on PATH —
            // or pointed at a custom build via the `PI_ACP_PI_COMMAND` env
            // (see BYO-pi). Args are empty: the ACP server is the default mode
            // (`npx -y pi-acp`, no subcommand). `node_required` follows pi's
            // 22+ requirement (pi-acp's own engines say >=20). The embedded
            // context env lets pi-acp advertise `promptCapabilities.embeddedContext`.
            distribution: AgentDistribution::Npx {
                version: "0.0.33",
                package: "pi-acp@0.0.33",
                cmd: "pi-acp",
                args: &[],
                env: &[("PI_ACP_ENABLE_EMBEDDED_CONTEXT", "true")],
                node_required: Some("22.0.0"),
            },
        },
        AgentType::Grok => AcpAgentMeta {
            agent_type,
            supports_mcp: true,
            name: "Grok",
            description: "xAI's official coding agent and CLI (ACP via grok agent stdio)",
            // `@xai-official/grok` ships each platform's native binary as a
            // brotli-compressed **optional dependency** (`@xai-official/grok-<os>-<arch>`);
            // the npm `bin/grok` trampoline decompresses it into `~/.grok/bin` on
            // first run. Public mirrors (e.g. registry.npmmirror.com, a common CN
            // default) lag far behind this package — at time of writing only 0.1.4,
            // which predates the `grok agent stdio` ACP subcommand — so the pinned
            // version isn't resolvable there.
            //
            // Both concerns are handled by codeg's shared `npm install -g` path
            // (`install_npm_global_package_streaming` in commands/acp.rs), which
            // always passes `--include=optional` (pulls the platform binary) and
            // `--registry=https://registry.npmjs.org` (bypasses lagging mirrors)
            // for every npx agent — so no per-agent launch env is needed here.
            // (It couldn't live here anyway: the launch env is serialized as
            // leading `KEY=value` argv and sacp's `parse_env_var` only accepts
            // `[A-Za-z0-9_]` env names, which npm's `@scope:registry` key is not.)
            //
            // 1.0.0 changed ONE thing that reaches codeg without any code change
            // here: its `initialize` advertises `sessionCapabilities.resume`
            // (0.2.118 advertised only `list`), so reconnecting to an existing
            // Grok session takes `connect_agent`'s resume → load → new chain at
            // the FIRST rung instead of the second. Verified live against the
            // 1.0.0 binary: `session/resume` restores conversation context, its
            // reply carries the `_meta["x.ai/sessionConfig"]` and per-model
            // `models` that the composer's selectors and context ring read, and
            // prompting straight after it works. It also skips `session/load`'s
            // history replay, which codeg only drained to discard. The 1.0.1–
            // 1.0.4 patches add nothing further here: re-probed live against the
            // 1.0.4 binary, `initialize` still answers `sessionCapabilities:
            // {list, resume, close}` plus the same
            // `promptCapabilities.embeddedContext`, so the resume rung stands.
            distribution: AgentDistribution::Npx {
                version: "1.0.4",
                package: "@xai-official/grok@1.0.4",
                cmd: "grok",
                // Only the ACP subcommand lives here. Grok's ROOT-level launch
                // flags (`--no-auto-update` always, `--permission-mode <value>`
                // only for a non-default permission mode) MUST precede this
                // subcommand — `grok agent stdio` itself rejects them (re-verified
                // against 1.0.4: it still only accepts --debug/--debug-file/
                // --leader-socket) — so `build_agent` inserts them ahead of these
                // args rather than appending after. Since 1.0.3 `grok --help` no
                // longer LISTS `--no-auto-update`, but it is still accepted:
                // clap hard-errors on an unknown argument, and
                // `grok --no-auto-update agent stdio` initializes clean.
                args: &["agent", "stdio"],
                env: &[],
                // `@xai-official/grok@1.0.4` declares `engines.node: ">=20"`;
                // surface that in preflight so Node 18 isn't silently accepted.
                node_required: Some("20.0.0"),
            },
        },
        AgentType::Cursor => AcpAgentMeta {
            agent_type,
            supports_mcp: true,
            name: "Cursor",
            description: "Cursor's coding agent (ACP via cursor-agent acp)",
            // Cursor's CLI ships as a ~230MB directory-tree archive (webpack
            // chunks + bundled Node runtime + ripgrep); the `cursor-agent`
            // entry is a shell script that resolves its own directory and
            // execs the sibling `node`, so the tree must stay intact —
            // `dir_entry` switches the binary cache to whole-tree extraction.
            // codeg deliberately does NOT run Cursor's official install
            // script: it symlinks `~/.local/bin/agent`, which collides with
            // Grok's CLI of the same name (observed overwriting it).
            // URL layout follows the ACP registry's `cursor` entry
            // (downloads.cursor.com/lab/<version>/<os>/<arch>/...); custom
            // versions substitute into the same pattern.
            distribution: AgentDistribution::Binary {
                version: "2026.08.11-e8db854",
                cmd: "cursor-agent",
                args: &["acp"],
                env: &[],
                platforms: &[
                    PlatformBinary {
                        platform: "darwin-aarch64",
                        url: "https://downloads.cursor.com/lab/2026.08.11-e8db854/darwin/arm64/agent-cli-package.tar.gz",
                        sha256: None,
                    },
                    PlatformBinary {
                        platform: "darwin-x86_64",
                        url: "https://downloads.cursor.com/lab/2026.08.11-e8db854/darwin/x64/agent-cli-package.tar.gz",
                        sha256: None,
                    },
                    PlatformBinary {
                        platform: "linux-aarch64",
                        url: "https://downloads.cursor.com/lab/2026.08.11-e8db854/linux/arm64/agent-cli-package.tar.gz",
                        sha256: None,
                    },
                    PlatformBinary {
                        platform: "linux-x86_64",
                        url: "https://downloads.cursor.com/lab/2026.08.11-e8db854/linux/x64/agent-cli-package.tar.gz",
                        sha256: None,
                    },
                    PlatformBinary {
                        platform: "windows-aarch64",
                        url: "https://downloads.cursor.com/lab/2026.08.11-e8db854/windows/arm64/agent-cli-package.zip",
                        sha256: None,
                    },
                    PlatformBinary {
                        platform: "windows-x86_64",
                        url: "https://downloads.cursor.com/lab/2026.08.11-e8db854/windows/x64/agent-cli-package.zip",
                        sha256: None,
                    },
                ],
                dir_entry: Some(BinaryDirEntry {
                    unix: "dist-package/cursor-agent",
                    windows: "dist-package/cursor-agent.cmd",
                }),
            },
        },
        AgentType::DeepSeek => AcpAgentMeta {
            agent_type,
            supports_mcp: true,
            name: "DeepSeek Harness",
            description: "Editor-facing DeepSeek Harness agent (ACP via deepseek-acp)",
            // `deepseek-acp` is the community editor bridge for DeepSeek
            // Harness (DSH): the harness's own `@deepseek-ai/dsh-acp` is an
            // automation-only transport (no streaming, no tool presentation,
            // rejects MCP), so codeg drives this adapter instead. It speaks
            // ACP over stdio with NO arguments; auth is the `DEEPSEEK_API_KEY`
            // env (or `~/.dsh/.credentials.yaml`), and per-session model /
            // reasoning-effort / sandbox selection arrives through standard
            // `configOptions`, so the composer selectors need no per-agent
            // code. Session logs land under `$DSH_HOME/sessions` (default
            // `~/.dsh/sessions`), which `parsers::deepseek` reads for history.
            // It advertises loadSession + sessionCapabilities.list/resume and
            // accepts wire `mcpServers` (stdio + streamable HTTP; SSE and the
            // `acp` transport are explicitly rejected), so both the resume rung
            // and the codeg-mcp companion work out of the box. 0.3.0 adds the
            // upstream skills chain (`skill_storage_spec` mirrors its roots)
            // and, since 0.2.0, `--setup` terminal auth for storing the key in
            // `$DSH_HOME/.credentials.yaml`.
            distribution: AgentDistribution::Npx {
                version: "0.3.0",
                package: "deepseek-acp@0.3.0",
                cmd: "deepseek-acp",
                args: &[],
                env: &[],
                // package.json declares `engines.node: ">=22"`.
                node_required: Some("22.0.0"),
            },
        },
        // Handled by the early return above; kept so the match stays
        // exhaustive without a catch-all that could swallow a new built-in.
        AgentType::Custom(_) => unreachable!("custom agents resolve via custom_registry"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_npx_version(
        agent_type: AgentType,
        expected_version: &str,
        expected_package: &str,
        expected_node_required: Option<&str>,
    ) {
        let meta = get_agent_meta(agent_type);
        match meta.distribution {
            AgentDistribution::Npx {
                version,
                package,
                node_required,
                ..
            } => {
                assert_eq!(version, expected_version);
                assert_eq!(package, expected_package);
                assert_eq!(node_required, expected_node_required);
                assert_eq!(meta.registry_version(), Some(expected_version));
            }
            other => {
                panic!("expected npx distribution for {agent_type:?}, got {other:?}");
            }
        }
    }

    fn assert_binary_version(
        agent_type: AgentType,
        expected_version: &str,
        expected_release_path: &str,
    ) {
        let meta = get_agent_meta(agent_type);
        match meta.distribution {
            AgentDistribution::Binary {
                version, platforms, ..
            } => {
                assert_eq!(version, expected_version);
                assert_eq!(meta.registry_version(), Some(expected_version));
                for platform in platforms {
                    assert!(
                        platform.url.contains(expected_release_path),
                        "{} URL did not use {expected_release_path}: {}",
                        platform.platform,
                        platform.url
                    );
                }
            }
            other => {
                panic!("expected binary distribution for {agent_type:?}, got {other:?}");
            }
        }
    }

    // Cursor is the only dir-tree binary agent: the archive must be kept
    // intact (bundled Node runtime) and launched via the in-tree entry
    // script, never copied out as a single file.
    #[test]
    fn cursor_pins_dir_tree_binary() {
        let meta = get_agent_meta(AgentType::Cursor);
        assert_binary_version(
            AgentType::Cursor,
            "2026.08.11-e8db854",
            "/lab/2026.08.11-e8db854/",
        );
        match meta.distribution {
            AgentDistribution::Binary {
                cmd,
                args,
                platforms,
                dir_entry,
                ..
            } => {
                assert_eq!(cmd, "cursor-agent");
                assert_eq!(args, &["acp"]);
                assert_eq!(platforms.len(), 6);
                let entry = dir_entry.expect("cursor must use dir-tree extraction");
                assert_eq!(entry.unix, "dist-package/cursor-agent");
                assert_eq!(entry.windows, "dist-package/cursor-agent.cmd");
            }
            other => panic!("expected binary distribution for Cursor, got {other:?}"),
        }
        // OpenCode stays on the single-binary copy-out path.
        match get_agent_meta(AgentType::OpenCode).distribution {
            AgentDistribution::Binary { dir_entry, .. } => assert!(dir_entry.is_none()),
            other => panic!("expected binary distribution for OpenCode, got {other:?}"),
        }
    }

    #[test]
    fn steering_prompt_required_min_version_gates_claude_only() {
        // The native-steering policy bit: only an adapter that honors the
        // `promptRequired` opt-in AND keeps the owning prompt in flight across
        // a steered turn gets a minimum version. The floor is the release that
        // fixed the latter (claude-agent-acp 0.65.0 / #958), NOT the one that
        // introduced the opt-in — every 0.64.x settles the prompt early (#934).
        // Everyone else stays None and rides the MCP pull channel; codex-acp
        // ships steering without the opt-in at all (re-verified on the 1.3.0
        // tarball). Flipping an agent on here without the runtime
        // `agent_info.version` proof is not enough by design.
        assert_eq!(
            steering_prompt_required_min_version(AgentType::ClaudeCode),
            Some("0.65.0")
        );
        assert_eq!(steering_prompt_required_min_version(AgentType::Codex), None);
        for agent in [
            AgentType::Gemini,
            AgentType::DeepSeek,
            AgentType::Grok,
            AgentType::Custom("acme"),
        ] {
            assert_eq!(steering_prompt_required_min_version(agent), None);
        }
    }

    #[test]
    fn goal_control_is_out_of_band_gates_codex_only() {
        // codex changes the goal through an app-server RPC, so codeg may follow
        // a pause/clear with the interrupt that actually stops the work. claude
        // delivers the same request as the prompt text "/goal clear" — killing
        // that turn would kill the clear — and every unverified adapter fails
        // closed onto the same "don't touch the turn".
        assert!(goal_control_is_out_of_band(AgentType::Codex));
        assert!(!goal_control_is_out_of_band(AgentType::ClaudeCode));
        for agent in [
            AgentType::Gemini,
            AgentType::DeepSeek,
            AgentType::Grok,
            AgentType::Custom("acme"),
        ] {
            assert!(!goal_control_is_out_of_band(agent));
        }
    }

    #[test]
    fn registry_pins_current_acp_agent_versions() {
        assert_npx_version(
            AgentType::ClaudeCode,
            "0.69.0",
            "@agentclientprotocol/claude-agent-acp@0.69.0",
            Some("22.0.0"),
        );
        assert_npx_version(
            AgentType::Gemini,
            "0.55.1",
            "@google/gemini-cli@0.55.1",
            Some("20.0.0"),
        );
        assert_npx_version(AgentType::Cline, "3.0.55", "cline@3.0.55", Some("22.0.0"));
        assert_npx_version(
            AgentType::CodeBuddy,
            "2.137.0",
            "@tencent-ai/codebuddy-code@2.137.0",
            Some("22.0.0"),
        );
        assert_npx_version(
            AgentType::KimiCode,
            "0.36.1",
            "@moonshot-ai/kimi-code@0.36.1",
            Some("22.19.0"),
        );
        assert_npx_version(
            AgentType::Codex,
            "1.4.0",
            "@agentclientprotocol/codex-acp@1.4.0",
            Some("20.0.0"),
        );
        assert_npx_version(AgentType::Pi, "0.0.33", "pi-acp@0.0.33", Some("22.0.0"));
        assert_npx_version(
            AgentType::Grok,
            "1.0.4",
            "@xai-official/grok@1.0.4",
            Some("20.0.0"),
        );
        assert_npx_version(
            AgentType::DeepSeek,
            "0.3.0",
            "deepseek-acp@0.3.0",
            Some("22.0.0"),
        );
        assert_binary_version(
            AgentType::OpenCode,
            "1.18.18",
            "/releases/download/v1.18.18/",
        );
        // Hermes rides the community npm bridge (upstream retired its PyPI
        // channel at 0.19.0; see the registry entry). The npm package version
        // tracks the upstream version 1:1, and the pin must stay EXACT — the
        // audited wrapper code is only what the pinned version ships.
        assert_npx_version(
            AgentType::Hermes,
            "0.20.1",
            "hermes-agent@0.20.1",
            Some("20.0.0"),
        );
        assert_binary_version(
            AgentType::Cursor,
            "2026.08.11-e8db854",
            "/lab/2026.08.11-e8db854/",
        );
    }

    #[test]
    fn codex_is_npx_on_all_platforms() {
        match codex_distribution() {
            AgentDistribution::Npx {
                version,
                package,
                node_required,
                env,
                cmd,
                ..
            } => {
                assert_eq!(version, "1.4.0");
                assert_eq!(package, "@agentclientprotocol/codex-acp@1.4.0");
                assert_eq!(cmd, "codex-acp");
                assert_eq!(node_required, Some("20.0.0"));
                assert_eq!(
                    env, CODEX_CLI_RUNTIME_DEFAULT_ENV,
                    "npx codex-acp must default CLI runtime off, got {env:?}"
                );
            }
            other => panic!("expected npx Codex distribution on all platforms, got {other:?}"),
        }
    }

    #[test]
    fn canonical_registry_ids_match_the_reserved_identity_table() {
        let actual: Vec<&str> = builtin_acp_agents()
            .into_iter()
            .map(registry_id_for)
            .collect();
        assert_eq!(actual.len(), BUILTIN_AGENT_REGISTRY_IDS.len());
        for id in actual {
            assert!(
                BUILTIN_AGENT_REGISTRY_IDS.contains(&id),
                "registry id {id} must be reserved from custom agents"
            );
        }
        for alias in BUILTIN_AGENT_REGISTRY_ALIASES {
            assert!(is_reserved_builtin_id(alias));
        }
        for wire in crate::models::agent::BUILTIN_AGENT_WIRE_NAMES {
            assert!(is_reserved_builtin_id(wire));
        }
    }

    #[test]
    fn adapter_relation_covers_only_wrapper_agents() {
        for agent_type in builtin_acp_agents() {
            let expected = matches!(agent_type, AgentType::ClaudeCode | AgentType::Codex);
            assert_eq!(
                acp_adapter_relation(agent_type).is_some(),
                expected,
                "unexpected adapter relation for {agent_type:?}"
            );
        }
    }

    #[test]
    fn removed_agent_is_not_registered() {
        let removed_registry_id = ["open", "cl", "aw-acp"].join("");
        assert!(
            !all_acp_agents()
                .iter()
                .any(|agent_type| registry_id_for(*agent_type) == removed_registry_id),
            "removed agent must not be exposed as an ACP agent"
        );
        assert_eq!(from_registry_id(&removed_registry_id), None);
    }

    #[test]
    fn all_registered_agents_support_mcp() {
        for agent_type in all_acp_agents() {
            let meta = get_agent_meta(agent_type);
            assert!(
                meta.supports_mcp,
                "unexpected supports_mcp for {agent_type:?}"
            );
        }
    }
}
