import type { PlanEntryInfo } from "@/lib/types"

/**
 * Pure plan/TodoWrite parsing helpers.
 *
 * Kept in a dependency-free module (no imports from the adapter or React
 * components) so both `agent-plan.ts` (overlay extraction) and
 * `ai-elements-adapter.ts` (turning a persisted TodoWrite tool_use into a
 * first-class `plan` part) can share them without an import cycle.
 */

export function asRecord(value: unknown): Record<string, unknown> | null {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    return null
  }
  return value as Record<string, unknown>
}

export function normalizeStatus(raw: string | null | undefined): string {
  const normalized = (raw ?? "").trim().toLowerCase()
  if (normalized === "completed" || normalized === "done") return "completed"
  if (
    normalized === "in_progress" ||
    normalized === "in-progress" ||
    normalized === "in progress" ||
    normalized === "running" ||
    normalized === "active"
  ) {
    return "in_progress"
  }
  return "pending"
}

export function normalizePriority(raw: string | null | undefined): string {
  const normalized = (raw ?? "").trim().toLowerCase()
  if (normalized === "high" || normalized === "urgent") return "high"
  if (normalized === "low") return "low"
  return "medium"
}

export function parsePlanEntriesArray(items: unknown[]): PlanEntryInfo[] {
  const entries: PlanEntryInfo[] = []

  for (const item of items) {
    const record = asRecord(item)
    if (!record) continue

    const contentCandidate =
      typeof record.content === "string"
        ? record.content
        : typeof record.step === "string"
          ? record.step
          : typeof record.title === "string"
            ? record.title
            : typeof record.name === "string"
              ? record.name
              : ""
    const content = contentCandidate.trim()
    if (!content) continue

    entries.push({
      content,
      status: normalizeStatus(
        typeof record.status === "string" ? record.status : undefined
      ),
      priority: normalizePriority(
        typeof record.priority === "string" ? record.priority : undefined
      ),
    })
  }

  return entries
}

export function parseTodosFromJson(input: string): PlanEntryInfo[] {
  try {
    const parsed: unknown = JSON.parse(input)
    const obj = asRecord(parsed)
    if (!obj) return []

    const candidateLists: unknown[][] = []
    if (Array.isArray(obj.todos)) {
      candidateLists.push(obj.todos)
    }
    if (Array.isArray(obj.entries)) {
      candidateLists.push(obj.entries)
    }
    if (Array.isArray(obj.plan)) {
      candidateLists.push(obj.plan)
    }

    for (const list of candidateLists) {
      const parsedEntries = parsePlanEntriesArray(list)
      if (parsedEntries.length > 0) {
        return parsedEntries
      }
    }

    return []
  } catch {
    return []
  }
}

export function normalizeToolName(toolName: string): string {
  return toolName.toLowerCase().replace(/[^a-z0-9]/g, "")
}

export function isPlanLikeToolName(toolName: string): boolean {
  const normalized = normalizeToolName(toolName)
  if (normalized === "todowrite") return true
  return normalized.includes("plan")
}
