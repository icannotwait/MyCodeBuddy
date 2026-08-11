import type {
  AdaptedContentPart,
  AdaptedDelegationWorkUnitPart,
  AdaptedMessage,
  AdaptedToolCallPart,
} from "@/lib/adapters/ai-elements-adapter"
import {
  isDelegateToAgentToolName,
  parseCancelDelegationReason,
  parseDelegateRunIdentity,
} from "@/lib/delegation-card"
import {
  parseDelegationStatusIdentity,
  parseStatusReports,
  parseTaskIds,
} from "@/lib/delegation-status"
import {
  groupDelegationRuns,
  type DelegationIdentityIndex,
  type DelegationRunRecord,
} from "@/lib/delegation-work-unit"
import { normalizeToolName } from "@/lib/tool-call-normalization"

function exactRunKey<T>(record: DelegationRunRecord<T>): string {
  const { parentConversationId, parentToolUseId } = record.identity
  return `${parentConversationId}\u0000run\u0000${parentToolUseId}`
}

function walkToolCalls(
  parts: readonly AdaptedContentPart[],
  visit: (part: AdaptedToolCallPart) => void
): void {
  for (const part of parts) {
    if (part.type === "tool-call") visit(part)
    else if (part.type === "goal-run") walkToolCalls(part.items, visit)
  }
}

export function delegationRunRecord(
  part: AdaptedToolCallPart,
  parentConversationId: number
): DelegationRunRecord<AdaptedToolCallPart> {
  return {
    value: part,
    identity: parseDelegateRunIdentity({
      parentConversationId,
      parentToolUseId: part.toolCallId,
      input: part.input,
      output: part.output,
      errorText: part.errorText,
      meta: part.meta,
    }),
  }
}

/** Collect exact-run records for delegate/continue tools in adapted messages. */
export function collectDelegationRunRecords(
  messages: readonly AdaptedMessage[],
  parentConversationId: number
): DelegationRunRecord<AdaptedToolCallPart>[] {
  const records: DelegationRunRecord<AdaptedToolCallPart>[] = []
  for (const message of messages) {
    walkToolCalls(message.content, (part) => {
      if (isDelegateToAgentToolName(part.toolName)) {
        records.push(delegationRunRecord(part, parentConversationId))
      }
    })
  }
  return records
}

/**
 * Identity index for live status folding: historical/local delegate runs plus
 * current-live delegate/continue tools, through the same exact-run ambiguity
 * rules as historical projection.
 */
export function buildLiveDelegationIdentityIndex(
  historicalRecords: readonly DelegationRunRecord<AdaptedToolCallPart>[],
  liveDelegateParts: readonly AdaptedToolCallPart[],
  parentConversationId: number
): DelegationIdentityIndex {
  const records: DelegationRunRecord<AdaptedToolCallPart>[] = [
    ...historicalRecords,
  ]
  for (const part of liveDelegateParts) {
    if (isDelegateToAgentToolName(part.toolName)) {
      records.push(delegationRunRecord(part, parentConversationId))
    }
  }
  return groupDelegationRuns(records).index
}

function successfulCancellation(part: AdaptedToolCallPart): boolean {
  if (parseCancelDelegationReason(part.input) === "timeout") return false
  if (
    parseStatusReports(part.output, part.errorText).some(
      (report) => report.status === "canceled"
    )
  ) {
    return true
  }
  return part.state === "output-available" && !part.errorText?.trim()
}

/** Task ids that received a successful explicit user cancel (not timeout). */
function cancellationTaskIds(messages: readonly AdaptedMessage[]): Set<string> {
  const keys = new Set<string>()
  for (const message of messages) {
    walkToolCalls(message.content, (part) => {
      if (normalizeToolName(part.toolName) !== "cancel_delegation") return
      if (!successfulCancellation(part)) return
      for (const taskId of parseTaskIds(part.input)) {
        if (taskId) keys.add(taskId)
      }
      for (const report of parseStatusReports(part.output, part.errorText)) {
        if (report.taskId) keys.add(report.taskId)
      }
    })
  }
  return keys
}

function sameIds(left: readonly string[], right: readonly string[]): boolean {
  if (left.length !== right.length) return false
  const rightSet = new Set(right)
  return left.every((id) => rightSet.has(id))
}

export function shouldFoldDelegationStatusCall(
  part: AdaptedToolCallPart,
  index: DelegationIdentityIndex
): boolean {
  if (normalizeToolName(part.toolName) !== "get_delegation_status") {
    return false
  }
  const identity = parseDelegationStatusIdentity(part)
  if (!identity.valid || identity.candidateIds.length === 0) return false
  if (
    identity.requestIds.length > 0 &&
    identity.reportIds.length > 0 &&
    !sameIds(identity.requestIds, identity.reportIds)
  ) {
    return false
  }
  return identity.candidateIds.every((id) => index.knownTaskIds.has(id))
}

function projectStatusPart(
  part: Extract<AdaptedContentPart, { type: "delegation-status-group" }>,
  index: DelegationIdentityIndex
): AdaptedContentPart | null {
  const polls = part.polls.filter(
    (poll) => !shouldFoldDelegationStatusCall(poll, index)
  )
  if (polls.length === part.polls.length && part.visibleTaskIds === undefined) {
    return part
  }
  if (polls.length === 0) return null
  return { type: "delegation-status-group", polls }
}

function rewriteParts(
  parts: AdaptedContentPart[],
  sourceReplacement: ReadonlyMap<
    AdaptedToolCallPart,
    AdaptedDelegationWorkUnitPart | null
  >,
  index: DelegationIdentityIndex
): AdaptedContentPart[] {
  let changed = false
  const result: AdaptedContentPart[] = []
  for (const part of parts) {
    if (part.type === "tool-call" && sourceReplacement.has(part)) {
      const replacement = sourceReplacement.get(part) ?? null
      changed = true
      if (replacement) result.push(replacement)
      continue
    }
    if (part.type === "delegation-status-group") {
      const replacement = projectStatusPart(part, index)
      if (replacement !== part) changed = true
      if (replacement) result.push(replacement)
      continue
    }
    if (part.type === "goal-run") {
      const items = rewriteParts(part.items, sourceReplacement, index)
      if (items !== part.items) {
        changed = true
        result.push({ ...part, items })
      } else {
        result.push(part)
      }
      continue
    }
    result.push(part)
  }
  return changed ? result : parts
}

export function projectDelegationTranscript(
  messages: readonly AdaptedMessage[],
  parentConversationId: number
): {
  messages: AdaptedMessage[]
  identityIndex: DelegationIdentityIndex
  runRecords: DelegationRunRecord<AdaptedToolCallPart>[]
} {
  const records = collectDelegationRunRecords(messages, parentConversationId)

  // Cold-history replay can materialize one tool call in multiple turns. Keep
  // its first timeline position while the latest snapshot defines identity.
  const snapshotsByRun = new Map<
    string,
    {
      latest: DelegationRunRecord<AdaptedToolCallPart>
      sources: AdaptedToolCallPart[]
    }
  >()
  for (const record of records) {
    const runKey = exactRunKey(record)
    const snapshots = snapshotsByRun.get(runKey)
    if (snapshots) {
      snapshots.latest = record
      snapshots.sources.push(record.value)
    } else {
      snapshotsByRun.set(runKey, {
        latest: record,
        sources: [record.value],
      })
    }
  }

  const grouped = groupDelegationRuns(
    [...snapshotsByRun.values()].map((snapshots) => snapshots.latest)
  )
  // Identity grouping still folds status polls / live residual rows, but each
  // turn (run) keeps its own card at its original transcript position so multi-
  // turn continue/re-entry results stay visible as separate cards.
  const canceledTaskIds = cancellationTaskIds(messages)
  const sourceReplacement = new Map<
    AdaptedToolCallPart,
    AdaptedDelegationWorkUnitPart | null
  >()
  for (const unit of grouped.units) {
    for (const run of unit.runs) {
      const sources = snapshotsByRun.get(exactRunKey(run))?.sources ?? [
        run.value,
      ]
      const runId =
        run.identity.taskId ??
        run.identity.parentToolUseId ??
        run.value.toolCallId
      const canonical: AdaptedDelegationWorkUnitPart = {
        type: "delegation-work-unit",
        // Per-run key keeps React list identity stable when a work unit has
        // multiple turns; unit.key alone would collide across cards.
        key: `wu:${unit.key}:${runId}`,
        sources,
        explicitUserCancel: Boolean(
          run.identity.taskId && canceledTaskIds.has(run.identity.taskId)
        ),
      }
      sources.forEach((source, index) => {
        sourceReplacement.set(source, index === 0 ? canonical : null)
      })
    }
  }

  return {
    messages: messages.map((message) => {
      const content = rewriteParts(
        message.content,
        sourceReplacement,
        grouped.index
      )
      return content === message.content ? message : { ...message, content }
    }),
    identityIndex: grouped.index,
    runRecords: records,
  }
}

export function shouldFoldLiveDelegationTool(
  part: AdaptedToolCallPart,
  index: DelegationIdentityIndex,
  _parentConversationId: number
): boolean {
  return shouldFoldDelegationStatusCall(part, index)
}
