/**
 * Pure map transitions for live parent ↔ child delegation bindings.
 *
 * Side effects (attach / detach grace timers) stay in `DelegationProvider`.
 * This module only decides the next `Map` and whether a completion was
 * accepted (match or synthesize) so the provider can schedule detach safely.
 */

import type {
  AgentType,
  AttentionRequestSummary,
  DelegationRuntimeStats,
  EventEnvelope,
  TaskObservation,
} from "@/lib/types"

/** Lifecycle only — badge/observation status is derived elsewhere. */
export type DelegationStatus = "running" | "ok" | "err"

export interface DelegationBinding {
  parentConnectionId: string
  parentToolUseId: string
  childConnectionId: string
  childConversationId: number
  agentType: AgentType
  status: DelegationStatus
  errorCode?: string
  taskId: string
  startedAt: string
  runtimeStats: DelegationRuntimeStats
  attentionRequest?: AttentionRequestSummary | null
  finishedAt?: string | null
  completedDurationMs?: number | null
  observation?: TaskObservation | null
  lastAgentActivityAt?: string | null
  stalledSince?: string | null
}

export type ApplyDelegationEnvelopeResult = {
  next: Map<string, DelegationBinding>
  /** Set only when a completion was accepted (match or synthesize). */
  acceptedCompletionToolUseId: string | null
}

function unchanged(
  prev: Map<string, DelegationBinding>
): ApplyDelegationEnvelopeResult {
  return { next: prev, acceptedCompletionToolUseId: null }
}

/**
 * Pure map transition. Returns `{ next, acceptedCompletionToolUseId }`.
 * `acceptedCompletionToolUseId` is set only when a completion was accepted
 * (matching `task_id`, or synthesized when no binding exists).
 */
export function applyDelegationEnvelope(
  prev: Map<string, DelegationBinding>,
  envelope: EventEnvelope
): ApplyDelegationEnvelopeResult {
  switch (envelope.type) {
    case "delegation_started": {
      const nextBinding: DelegationBinding = {
        parentConnectionId: envelope.parent_connection_id,
        parentToolUseId: envelope.parent_tool_use_id,
        childConnectionId: envelope.child_connection_id,
        childConversationId: envelope.child_conversation_id,
        agentType: envelope.agent_type,
        // Lifecycle stays running. Observation is non-terminal health only.
        status: "running",
        taskId: envelope.task_id,
        startedAt: envelope.started_at,
        runtimeStats: envelope.runtime_stats,
        attentionRequest: envelope.attention_request ?? null,
        // Clear any terminal leftovers when the same tool-use id restarts.
        finishedAt: null,
        completedDurationMs: null,
        errorCode: undefined,
        observation: envelope.observation ?? "active",
        lastAgentActivityAt: envelope.last_agent_activity_at ?? null,
        stalledSince: envelope.stalled_since ?? null,
      }
      const next = new Map(prev)
      next.set(envelope.parent_tool_use_id, nextBinding)
      return { next, acceptedCompletionToolUseId: null }
    }

    case "delegation_runtime_stats_changed": {
      const existing = prev.get(envelope.parent_tool_use_id)
      if (!existing || existing.taskId !== envelope.task_id) {
        return unchanged(prev)
      }
      const next = new Map(prev)
      next.set(envelope.parent_tool_use_id, {
        ...existing,
        runtimeStats: envelope.runtime_stats,
      })
      return { next, acceptedCompletionToolUseId: null }
    }

    case "delegation_attention_changed": {
      const existing = prev.get(envelope.parent_tool_use_id)
      if (!existing || existing.taskId !== envelope.task_id) {
        return unchanged(prev)
      }
      const next = new Map(prev)
      next.set(envelope.parent_tool_use_id, {
        ...existing,
        // Omitted or null → authoritative clear (do not preserve prior).
        attentionRequest: envelope.attention_request ?? null,
      })
      return { next, acceptedCompletionToolUseId: null }
    }

    case "delegation_observation_changed": {
      const existing = prev.get(envelope.parent_tool_use_id)
      // Apply-only: never create a card; require lifecycle running + task match.
      if (
        !existing ||
        existing.status !== "running" ||
        existing.taskId !== envelope.task_id
      ) {
        return unchanged(prev)
      }
      const next = new Map(prev)
      next.set(envelope.parent_tool_use_id, {
        ...existing,
        observation: envelope.observation,
        lastAgentActivityAt: envelope.last_agent_activity_at,
        stalledSince: envelope.stalled_since ?? null,
      })
      return { next, acceptedCompletionToolUseId: null }
    }

    case "delegation_completed": {
      const existing = prev.get(envelope.parent_tool_use_id)
      // Mismatched task_id on an existing binding: ignore completely.
      if (existing && existing.taskId !== envelope.task_id) {
        return unchanged(prev)
      }

      // Synthesize only when no binding exists (mid-flight mount / reconnect).
      const base: DelegationBinding = existing ?? {
        parentConnectionId: envelope.parent_connection_id,
        parentToolUseId: envelope.parent_tool_use_id,
        childConnectionId: envelope.child_connection_id,
        childConversationId: envelope.child_conversation_id,
        agentType: envelope.agent_type,
        status: "running",
        taskId: envelope.task_id,
        startedAt: envelope.runtime_stats.started_at,
        runtimeStats: envelope.runtime_stats,
      }

      const result = envelope.result
      const updated: DelegationBinding =
        result.kind === "ok"
          ? {
              ...base,
              status: "ok",
              errorCode: undefined,
              runtimeStats: envelope.runtime_stats,
              finishedAt: envelope.runtime_stats.finished_at ?? null,
              completedDurationMs: result.duration_ms,
              attentionRequest: null,
              observation: null,
              stalledSince: null,
            }
          : {
              ...base,
              status: "err",
              errorCode: result.error_code,
              runtimeStats: envelope.runtime_stats,
              finishedAt: envelope.runtime_stats.finished_at ?? null,
              completedDurationMs: null,
              attentionRequest: null,
              observation: null,
              stalledSince: null,
            }

      const next = new Map(prev)
      next.set(envelope.parent_tool_use_id, updated)
      return {
        next,
        acceptedCompletionToolUseId: envelope.parent_tool_use_id,
      }
    }

    default:
      return unchanged(prev)
  }
}
