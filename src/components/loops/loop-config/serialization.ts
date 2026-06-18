/** Pure form-state model for the loop config editor: the `IssueConfig` ↔
 *  form-state conversions plus the shared sentinels and small parse helpers.
 *  No JSX — imported by the config-form shell and every loop-config sub-module. */

import type {
  AgentSpec,
  AgentType,
  IssueConfig,
  LoopIssueRoute,
  LoopStage,
  ReviewerEntry,
  StageAgents,
} from "@/lib/types"

export const AGENT_TYPES: AgentType[] = [
  "claude_code",
  "codex",
  "open_code",
  "gemini",
  "open_claw",
  "cline",
  "hermes",
]

// Stages that run a single agent (one sub-tab each). `review` is special — it
// runs the configured reviewers list (its own sub-tab) instead of one agent.
export const SINGLE_STAGES: LoopStage[] = [
  "triage",
  "refine",
  "design",
  "plan",
  "implement",
  "finalize",
  "reflect",
]

// Select can't carry an empty value, so these sentinels stand in for "no value".
export const INHERIT = "__inherit__" // a stage with no per-stage agent override
export const ROUTE_AUTO = "__auto__" // force_route = null (triage decides)
// Mode/config "use the agent's own default" (clears the override).
export const DEFAULT_SENTINEL = "__codeg_default__"

/** Form-state shape of an {@link AgentSpec}: agent + optional startup mode/config.
 *  `mode_id` is normalized to `null` (never undefined) for controlled selects. */
export interface AgentSpecForm {
  agent: AgentType
  mode_id: string | null
  config_values: Record<string, string>
}

/** A reviewer in form state: a concrete agent spec, or "use default agent"
 *  (`{ inherit: true }`), which resolves at dispatch to the issue's default
 *  review agent — mirroring how a single stage can defer to the default. */
export type ReviewerForm = AgentSpecForm | { inherit: true }

export const isInheritReviewer = (r: ReviewerForm): r is { inherit: true } =>
  "inherit" in r

/**
 * Form-state mirror of `IssueConfig`. Numeric fields are kept as strings so a
 * field can be cleared / typed through intermediate values without snapping to
 * a parsed number; `formStateToConfig` serializes on save. The per-stage agents
 * (default + each single stage) carry full mode/config; the review stage uses
 * the structured `reviewers` list. The per-issue total token budget is NOT here
 * — it lives outside `IssueConfig`, owned by the issue-settings host (rendered
 * into the Limits tab via `limitsExtra`).
 */
export interface LoopConfigFormState {
  defaultSpec: AgentSpecForm
  /** Per single-stage override, or `INHERIT` to follow the default. Keyed by the
   *  stages in {@link SINGLE_STAGES} (the review stage is not here). */
  stageSpecs: Record<string, AgentSpecForm | typeof INHERIT>
  validationCommands: string[]
  reviewers: ReviewerForm[]
  /** Form-state string (kept loose for the select); narrowed to the
   *  `ReviewPassRule` union in {@link formStateToConfig}. */
  reviewPassRule: string
  maxAttempts: string
  oscillationLimit: string
  autoMerge: boolean
  forceRoute: string
  iterationTimeoutSecs: string
  tokenBudgetPerTurn: string
  stallAlertSecs: string
}

function intField(n: number | null | undefined): string {
  return n == null ? "" : String(n)
}

/** Empty / non-positive → null (unlimited); otherwise the floored integer. */
function parsePositiveOrNull(s: string): number | null {
  const n = Number(s.trim())
  return Number.isFinite(n) && n > 0 ? Math.floor(n) : null
}

/** A bounded integer field with a fallback when blank or unparseable. A blank
 *  field means "unspecified" → the fallback, NOT 0 (note `Number("")` is 0, which
 *  would otherwise let a cleared field silently mean zero). */
function parseCount(s: string, min: number, fallback: number): number {
  const trimmed = s.trim()
  if (trimmed === "") return fallback
  const n = Number(trimmed)
  return Number.isFinite(n) ? Math.max(min, Math.floor(n)) : fallback
}

/** Wire `AgentSpec` → controlled form shape (mode_id normalized to null). */
function toSpecForm(s: AgentSpec): AgentSpecForm {
  return {
    agent: s.agent,
    mode_id: s.mode_id ?? null,
    config_values: { ...s.config_values },
  }
}

/** Form shape → wire `AgentSpec` (omit mode_id when unset, like the backend). */
function specFromForm(s: AgentSpecForm): AgentSpec {
  return {
    agent: s.agent,
    ...(s.mode_id ? { mode_id: s.mode_id } : {}),
    config_values: s.config_values,
  }
}

export function configToFormState(c: IssueConfig): LoopConfigFormState {
  const stageSpecs: Record<string, AgentSpecForm | typeof INHERIT> = {}
  for (const s of SINGLE_STAGES) {
    const spec = c.agents[s as keyof StageAgents]
    stageSpecs[s] = spec ? toSpecForm(spec) : INHERIT
  }
  const route = c.force_route
  return {
    defaultSpec: toSpecForm(c.agents.default),
    stageSpecs,
    validationCommands: [...c.validation_commands],
    reviewers: (c.reviewers ?? []).map<ReviewerForm>((r) =>
      "inherit" in r
        ? { inherit: true }
        : {
            agent: r.agent,
            mode_id: r.mode_id ?? null,
            config_values: { ...r.config_values },
          }
    ),
    reviewPassRule: c.review_pass_rule || "unanimous",
    maxAttempts: String(c.max_attempts ?? 0),
    oscillationLimit: String(c.oscillation_limit ?? 2),
    autoMerge: !!c.auto_merge,
    forceRoute: route && route !== "undecided" ? route : ROUTE_AUTO,
    iterationTimeoutSecs: intField(c.iteration_timeout_secs),
    tokenBudgetPerTurn: intField(c.token_budget_per_turn),
    stallAlertSecs: intField(c.stall_alert_secs),
  }
}

export function formStateToConfig(form: LoopConfigFormState): IssueConfig {
  // Build as a record (keyed by stage), then narrow to StageAgents — `default`
  // is always set and every other key is a valid optional stage field.
  const agentsRecord: Record<string, AgentSpec> = {
    default: specFromForm(form.defaultSpec),
  }
  for (const s of SINGLE_STAGES) {
    const v = form.stageSpecs[s]
    if (v !== INHERIT) agentsRecord[s] = specFromForm(v)
  }
  const agents = agentsRecord as unknown as StageAgents
  const reviewers: ReviewerEntry[] = form.reviewers.map((r) =>
    isInheritReviewer(r)
      ? { inherit: true }
      : {
          agent: r.agent,
          ...(r.mode_id ? { mode_id: r.mode_id } : {}),
          config_values: r.config_values,
        }
  )
  return {
    agents,
    validation_commands: form.validationCommands
      .map((s) => s.trim())
      .filter(Boolean),
    // At least one reviewer — the backend rejects an empty list. Fall back to a
    // single inherit-the-default reviewer if the user cleared them all.
    reviewers: reviewers.length > 0 ? reviewers : [{ inherit: true }],
    review_pass_rule:
      form.reviewPassRule === "majority" ? "majority" : "unanimous",
    max_attempts: parseCount(form.maxAttempts, 0, 0),
    // min 0 so "0 = off" survives (the backend breaker is disabled at 0); blank /
    // unparseable falls back to the default of 2, never to 0 (which would silently
    // disable the breaker the user never asked to turn off).
    oscillation_limit: parseCount(form.oscillationLimit, 0, 2),
    auto_merge: form.autoMerge,
    force_route:
      form.forceRoute === ROUTE_AUTO
        ? null
        : (form.forceRoute as LoopIssueRoute),
    iteration_timeout_secs: parsePositiveOrNull(form.iterationTimeoutSecs),
    token_budget_per_turn: parsePositiveOrNull(form.tokenBudgetPerTurn),
    stall_alert_secs: parsePositiveOrNull(form.stallAlertSecs),
  }
}
