/**
 * Durable per-run card snapshots. Unlike the child-conversation projection,
 * these entries are keyed by the parent and task id, so a later continuation
 * cannot overwrite an earlier parent card.
 */

import { getDelegationRunSnapshot } from "@/lib/api"
import { getActiveBackendCacheKey } from "@/lib/transport"
import type { CardSummary, DelegationRunSnapshot } from "@/lib/types"

const SUMMARY_MAX_CHARS = 240
const TEST_STATUS_MAX_CHARS = 64
const COUNT_MAX = 1_000_000
const COMMITS_MAX = 20
const CONCERNS_MAX = 20
const PLAN_DIGEST_MAX = 128
const REPORT_FILE_MAX = 512

type ReviewSummary = Extract<CardSummary, { kind: "review" }>
type AuthorSummary = Extract<CardSummary, { kind: "author" }>
type ImplementationSummary = Extract<CardSummary, { kind: "implementation" }>

function isRecord(value: unknown): value is Record<string, unknown> {
  return !!value && typeof value === "object" && !Array.isArray(value)
}

function isBoundedString(
  value: unknown,
  max = SUMMARY_MAX_CHARS
): value is string {
  return typeof value === "string" && Array.from(value).length <= max
}

function isCount(value: unknown): value is number {
  return (
    typeof value === "number" &&
    Number.isInteger(value) &&
    value >= 0 &&
    value <= COUNT_MAX
  )
}

/** Match server `validate_report_file`: length + workspace-relative only. */
function isValidReportFile(path: string): boolean {
  if (Array.from(path).length > REPORT_FILE_MAX) return false
  if (path.startsWith("/") || path.startsWith("\\")) return false
  if (path.length >= 2 && path[1] === ":") return false
  for (const seg of path.split(/[/\\]/)) {
    if (seg === "..") return false
  }
  return true
}

/**
 * Defence in depth for historical rows and event payloads. The backend
 * validates summaries at settlement, but a malformed persisted value should
 * still leave a usable status-only card.
 */
export function normalizeCardSummary(value: unknown): CardSummary | null {
  if (!isRecord(value) || !isBoundedString(value.summary)) return null

  if (value.kind === "review") {
    if (
      !["approve", "approve_with_minors", "request_changes", "block"].includes(
        String(value.verdict)
      ) ||
      !isCount(value.critical) ||
      !isCount(value.important) ||
      !isCount(value.minor)
    ) {
      return null
    }
    if (
      value.report_file != null &&
      (typeof value.report_file !== "string" ||
        !isValidReportFile(value.report_file))
    ) {
      return null
    }
    return {
      kind: "review",
      verdict: value.verdict as ReviewSummary["verdict"],
      critical: value.critical,
      important: value.important,
      minor: value.minor,
      summary: value.summary,
      ...(value.report_file == null ? {} : { report_file: value.report_file }),
    }
  }

  if (value.kind === "author") {
    if (
      !["done", "done_with_concerns", "blocked", "needs_context"].includes(
        String(value.status)
      ) ||
      !isBoundedString(value.plan_digest, PLAN_DIGEST_MAX) ||
      value.plan_digest.trim().length === 0 ||
      typeof value.report_file !== "string" ||
      value.report_file.length === 0 ||
      !isValidReportFile(value.report_file)
    ) {
      return null
    }
    return {
      kind: "author",
      status: value.status as AuthorSummary["status"],
      summary: value.summary,
      plan_digest: value.plan_digest,
      report_file: value.report_file,
    }
  }

  if (value.kind !== "implementation") return null
  if (
    !["implementation", "fix"].includes(String(value.phase)) ||
    !["done", "done_with_concerns", "blocked", "needs_context"].includes(
      String(value.status)
    )
  ) {
    return null
  }

  const commits = value.commits
  if (commits != null) {
    if (!Array.isArray(commits) || commits.length > COMMITS_MAX) return null
    if (
      commits.some(
        (commit) =>
          !isRecord(commit) ||
          !isBoundedString(commit.sha, 64) ||
          !isBoundedString(commit.subject, 200)
      )
    ) {
      return null
    }
  }
  const concerns = value.concerns
  if (
    concerns != null &&
    (!Array.isArray(concerns) ||
      concerns.length > CONCERNS_MAX ||
      concerns.some((concern) => !isBoundedString(concern)))
  ) {
    return null
  }

  let tests: NonNullable<ImplementationSummary["tests"]> | undefined
  if (value.tests != null) {
    if (
      !isRecord(value.tests) ||
      !isBoundedString(value.tests.status, TEST_STATUS_MAX_CHARS)
    ) {
      return null
    }
    if (
      (value.tests.passed != null && !isCount(value.tests.passed)) ||
      (value.tests.failed != null && !isCount(value.tests.failed)) ||
      (value.tests.summary != null && !isBoundedString(value.tests.summary))
    ) {
      return null
    }
    tests = {
      status: value.tests.status,
      ...(value.tests.passed == null ? {} : { passed: value.tests.passed }),
      ...(value.tests.failed == null ? {} : { failed: value.tests.failed }),
      ...(value.tests.summary == null ? {} : { summary: value.tests.summary }),
    }
  }

  // Invalid report_file fails the whole summary (matches server settlement).
  if (value.report_file != null) {
    if (
      typeof value.report_file !== "string" ||
      !isValidReportFile(value.report_file)
    ) {
      return null
    }
  }

  return {
    kind: "implementation",
    phase: value.phase as "implementation" | "fix",
    status: value.status as
      | "done"
      | "done_with_concerns"
      | "blocked"
      | "needs_context",
    summary: value.summary,
    ...(commits == null
      ? {}
      : {
          commits: commits.map((commit) => ({
            sha: (commit as Record<string, string>).sha,
            subject: (commit as Record<string, string>).subject,
          })),
        }),
    ...(tests == null ? {} : { tests }),
    ...(concerns == null ? {} : { concerns: concerns as string[] }),
    ...(value.report_file == null ? {} : { report_file: value.report_file }),
  }
}

export function normalizeDelegationRunSnapshot(
  snapshot: DelegationRunSnapshot
): DelegationRunSnapshot {
  return {
    ...snapshot,
    card_summary: normalizeCardSummary(snapshot.card_summary),
  }
}

function isTerminal(snapshot: DelegationRunSnapshot): boolean {
  return ["completed", "failed", "canceled"].includes(snapshot.status)
}

function cacheKey(parentConversationId: number, taskId: string): string {
  return `${getActiveBackendCacheKey()}\0${parentConversationId}\0${taskId}`
}

/** Backend-scoped cache for immutable per-run card snapshots. */
export class DelegationRunSnapshotCache {
  private readonly entries = new Map<string, DelegationRunSnapshot>()
  private readonly inFlight = new Map<string, Promise<void>>()
  private readonly listeners = new Set<() => void>()
  private version = 0

  get(
    parentConversationId: number | null | undefined,
    taskId: string | null | undefined
  ): DelegationRunSnapshot | null {
    if (parentConversationId == null || !taskId) return null
    return this.entries.get(cacheKey(parentConversationId, taskId)) ?? null
  }

  ensure(
    parentConversationId: number | null | undefined,
    taskId: string | null | undefined
  ): void {
    if (parentConversationId == null || !taskId) return
    const key = cacheKey(parentConversationId, taskId)
    const current = this.entries.get(key)
    // A terminal server response is the immutable card record. Until then,
    // continue checking so a missed completion event cannot leave cold cards
    // stuck in their earlier running state.
    if ((current && isTerminal(current)) || this.inFlight.has(key)) return
    const flight = getDelegationRunSnapshot(parentConversationId, taskId)
      .then((snapshot) => this.install(key, snapshot))
      .catch(() => {
        // Historical cards remain usable from their tool-call data on a miss.
      })
      .finally(() => {
        if (this.inFlight.get(key) === flight) this.inFlight.delete(key)
      })
    this.inFlight.set(key, flight)
  }

  /** Install is public for event/reconnect tests and still enforces freezing. */
  install(key: string, incoming: DelegationRunSnapshot): void {
    const current = this.entries.get(key)
    if (current && isTerminal(current)) return
    this.entries.set(key, normalizeDelegationRunSnapshot(incoming))
    this.version += 1
    for (const listener of this.listeners) listener()
  }

  getVersion(): number {
    return this.version
  }

  subscribe(listener: () => void): () => void {
    this.listeners.add(listener)
    return () => this.listeners.delete(listener)
  }

  reset(): void {
    const changed = this.entries.size > 0 || this.inFlight.size > 0
    this.entries.clear()
    this.inFlight.clear()
    if (!changed) return
    this.version += 1
    for (const listener of this.listeners) listener()
  }
}

export const delegationRunSnapshotCache = new DelegationRunSnapshotCache()
