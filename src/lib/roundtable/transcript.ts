import { currentMessageMembership } from "@/lib/roundtable/api"
import type {
  RoundtableConfig,
  RoundtableMessage,
  RoundtableProjection,
} from "@/lib/roundtable/types"

export type TranscriptPhaseKind =
  | "proposal"
  | "critique"
  | "synthesis"
  | "unknown"

export type TranscriptVisibility = "staged" | "published" | "preview"

export type TranscriptTitleKey =
  | "phaseProposal"
  | "phaseCritique"
  | "phaseSynthesis"
  | "phaseUnknown"
  | "abstain"

export interface TranscriptAlias {
  kind: string
  alias: string
}

export interface TranscriptConclusion {
  text: string
  inference: boolean
  aliases: TranscriptAlias[]
}

export interface TranscriptClaim {
  text: string
  confidence: string | null
  evidenceAliases: string[]
}

export interface TranscriptResponseItem {
  text: string
  stance: string | null
  priority: string | null
  evidenceAliases: string[]
}

export interface TranscriptConsensus {
  text: string
  agreement: string | null
  inference: boolean
  aliases: TranscriptAlias[]
  /** Seat aliases (`s0`, `s1`, …) the moderator lists as supporters. */
  supporterAliases: string[]
}

export interface TranscriptSpeaker {
  speakerId: string | null
  role: "member" | "moderator" | "unknown"
  /** Member ordinal; null for the moderator seat. */
  ordinal: number | null
  /**
   * Configured participant behind this speaker. A moderator turn points at
   * the member it was assigned to, so both seats share one identity color.
   */
  seatOrdinal: number | null
  /** The member's configured role. Null on moderator turns. */
  participantRole: string | null
  providerRef: string | null
  modelId: string | null
  agent: string
}

export interface TranscriptTurn {
  key: string
  messageId: string | null
  bodyHash: string | null
  visibility: TranscriptVisibility
  kind: string | null
  phaseId: string
  speaker: TranscriptSpeaker
  attemptId: string | null
  attemptNo: number | null
  attemptState: string | null
  finishedAt: string | null
  publishedSeq: string | null
  summary: string | null
  reason: string | null
  claims: TranscriptClaim[]
  responses: TranscriptResponseItem[]
  openQuestions: string[]
  positionChanges: { reason: string }[]
  recommendation: TranscriptConclusion | null
  alternatives: TranscriptConclusion[]
  consensus: TranscriptConsensus[]
  disagreements: TranscriptConclusion[]
  risks: TranscriptConclusion[]
  decisionRequests: TranscriptConclusion[]
  coverage: { succeeded: number; absent: number } | null
  evidenceAliases: TranscriptAlias[]
  raw: unknown
  /** Present only on unverified live preview turns. */
  live?: RoundtableLivePreview
}

export interface TranscriptPhase {
  id: string
  kind: TranscriptPhaseKind
  index: number
  revision: string
  critiqueRound: number | null
  titleKey: TranscriptTitleKey
  turns: TranscriptTurn[]
}

export interface RoundtableTranscriptModel {
  phases: TranscriptPhase[]
  critiqueRoundCount: number
  /** Seat aliases the runtime shows models (`s<ordinal>`), for display only. */
  seats: Record<string, TranscriptSpeaker>
}

/** Display-only live output attached to a preview turn (never verified). */
export interface RoundtableLivePreview {
  thought: string
  /** Older thinking was dropped by the buffer cap. */
  thoughtOmitted: boolean
  activity: string | null
  /** The member finished streaming; verification is pending. */
  ended: boolean
  /** The message buffer hit its cap; later text is not shown. */
  truncated: boolean
}

export interface RoundtablePreviewInput {
  attemptId: string
  text: string
  live?: RoundtableLivePreview
  /**
   * Seat and phase from the live frame, used only while the verified
   * projection does not list the attempt yet (it is re-read on events).
   */
  speakerId?: string
  phaseId?: string
  phaseKind?: string
}

interface PhaseBucket {
  id: string
  kind: TranscriptPhaseKind
  index: number
  revision: string
  titleKey: TranscriptTitleKey
  turns: {
    turn: TranscriptTurn
    seq: bigint | null
    ordinal: number | null
    source: number
  }[]
}

const KIND_RANK: Record<TranscriptPhaseKind, number> = {
  proposal: 0,
  critique: 1,
  synthesis: 2,
  unknown: 3,
}

function asRecord(value: unknown): Record<string, unknown> | null {
  if (!value || typeof value !== "object" || Array.isArray(value)) return null
  return value as Record<string, unknown>
}

function asString(value: unknown): string | null {
  return typeof value === "string" && value.length > 0 ? value : null
}

function asStringList(value: unknown): string[] {
  if (!Array.isArray(value)) return []
  return value.filter((item): item is string => typeof item === "string")
}

function asCount(value: unknown): number | null {
  if (typeof value === "number" && Number.isInteger(value) && value >= 0)
    return value
  if (typeof value === "string" && /^(0|[1-9][0-9]*)$/.test(value)) {
    const parsed = Number(value)
    if (Number.isSafeInteger(parsed)) return parsed
  }
  return null
}

function asSeq(value: string | null | undefined): bigint | null {
  if (typeof value !== "string" || !/^(0|[1-9][0-9]*)$/.test(value)) return null
  return BigInt(value)
}

function normalizeKind(value: unknown): TranscriptPhaseKind {
  if (value === "proposal" || value === "critique" || value === "synthesis")
    return value
  return "unknown"
}

function titleKeyFor(kind: TranscriptPhaseKind): TranscriptTitleKey {
  if (kind === "proposal") return "phaseProposal"
  if (kind === "critique") return "phaseCritique"
  if (kind === "synthesis") return "phaseSynthesis"
  return "phaseUnknown"
}

function readIndex(value: unknown, fallback: number): number {
  const count = asCount(value)
  return count === null ? fallback : count
}

function readAliases(value: unknown): TranscriptAlias[] {
  if (!Array.isArray(value)) return []
  const aliases: TranscriptAlias[] = []
  for (const item of value) {
    const record = asRecord(item)
    const kind = asString(record?.kind)
    const alias = asString(record?.alias)
    if (kind && alias) aliases.push({ kind, alias })
  }
  return aliases
}

function readConclusion(value: unknown): TranscriptConclusion | null {
  const record = asRecord(value)
  const text = asString(record?.text)
  if (!record || text === null) return null
  return {
    text,
    inference: record.inference === true,
    aliases: readAliases(record.aliases),
  }
}

function readConclusions(value: unknown): TranscriptConclusion[] {
  if (!Array.isArray(value)) return []
  return value.flatMap((item) => {
    const conclusion = readConclusion(item)
    return conclusion ? [conclusion] : []
  })
}

function readClaims(value: unknown): TranscriptClaim[] {
  if (!Array.isArray(value)) return []
  return value.flatMap((item) => {
    const record = asRecord(item)
    const text = asString(record?.text)
    if (!record || text === null) return []
    return [
      {
        text,
        confidence: asString(record.confidence),
        evidenceAliases: asStringList(record.evidence_aliases),
      },
    ]
  })
}

function readResponses(value: unknown): TranscriptResponseItem[] {
  if (!Array.isArray(value)) return []
  return value.flatMap((item) => {
    const record = asRecord(item)
    const text = asString(record?.text)
    if (!record || text === null) return []
    return [
      {
        text,
        stance: asString(record.stance),
        priority: asString(record.priority),
        evidenceAliases: asStringList(record.evidence_aliases),
      },
    ]
  })
}

function readConsensus(value: unknown): TranscriptConsensus[] {
  if (!Array.isArray(value)) return []
  return value.flatMap((item) => {
    const record = asRecord(item)
    const text = asString(record?.text)
    if (!record || text === null) return []
    return [
      {
        text,
        agreement: asString(record.agreement_level),
        inference: record.inference === true,
        aliases: readAliases(record.aliases),
        supporterAliases: asStringList(record.supporter_aliases),
      },
    ]
  })
}

function readCoverage(
  value: unknown
): { succeeded: number; absent: number } | null {
  const record = asRecord(value)
  const succeeded = asCount(record?.succeeded)
  const absent = asCount(record?.absent)
  if (succeeded === null || absent === null) return null
  return { succeeded, absent }
}

function collectAliases(turn: {
  claims: TranscriptClaim[]
  responses: TranscriptResponseItem[]
  recommendation: TranscriptConclusion | null
  alternatives: TranscriptConclusion[]
  consensus: TranscriptConsensus[]
  disagreements: TranscriptConclusion[]
  risks: TranscriptConclusion[]
  decisionRequests: TranscriptConclusion[]
}): TranscriptAlias[] {
  const seen = new Set<string>()
  const aliases: TranscriptAlias[] = []
  const push = (kind: string, alias: string) => {
    const key = `${kind}\0${alias}`
    if (seen.has(key)) return
    seen.add(key)
    aliases.push({ kind, alias })
  }
  for (const claim of turn.claims) {
    for (const alias of claim.evidenceAliases) push("evidence", alias)
  }
  for (const response of turn.responses) {
    for (const alias of response.evidenceAliases) push("evidence", alias)
  }
  const conclusions = [
    turn.recommendation,
    ...turn.alternatives,
    ...turn.disagreements,
    ...turn.risks,
    ...turn.decisionRequests,
  ]
  for (const conclusion of conclusions) {
    if (!conclusion) continue
    for (const alias of conclusion.aliases) push(alias.kind, alias.alias)
  }
  for (const item of turn.consensus) {
    for (const alias of item.aliases) push(alias.kind, alias.alias)
  }
  return aliases
}

function readSpeaker(
  projection: RoundtableProjection,
  speakerId: string | null
): TranscriptSpeaker {
  const speaker = speakerId
    ? projection.body.replay.speakers.find(
        (item) => item.speaker_id === speakerId
      )
    : undefined
  if (!speaker) {
    return {
      speakerId,
      role: "unknown",
      ordinal: null,
      seatOrdinal: null,
      participantRole: null,
      providerRef: null,
      modelId: null,
      agent: "codex",
    }
  }
  const role =
    speaker.role === "moderator"
      ? "moderator"
      : speaker.role === "member"
        ? "member"
        : "unknown"
  const config = projection.body.replay.config
  const participant = participantFor(config, role, speaker.ordinal)
  const seatOrdinal =
    role === "moderator"
      ? (asCount(config?.moderator_ordinal) ?? null)
      : role === "member"
        ? speaker.ordinal
        : null
  return {
    speakerId: speaker.speaker_id,
    role,
    ordinal: role === "member" ? speaker.ordinal : null,
    seatOrdinal,
    // A moderator turn speaks as moderator, not in its member role.
    participantRole: role === "member" ? asString(participant?.role) : null,
    providerRef: speaker.provider_ref,
    modelId: speaker.model_id,
    agent: participant?.agent || "codex",
  }
}

function participantFor(
  config: RoundtableConfig | null,
  role: TranscriptSpeaker["role"],
  ordinal: number
) {
  if (!config) return undefined
  if (role === "moderator") {
    return config.participants.find(
      (member) => member.ordinal === config.moderator_ordinal
    )
  }
  return config.participants.find((member) => member.ordinal === ordinal)
}

function emptyTurn(
  partial: Pick<
    TranscriptTurn,
    | "key"
    | "messageId"
    | "bodyHash"
    | "visibility"
    | "kind"
    | "phaseId"
    | "speaker"
    | "attemptId"
    | "attemptNo"
    | "attemptState"
    | "finishedAt"
    | "publishedSeq"
    | "summary"
    | "raw"
  >
): TranscriptTurn {
  return {
    reason: null,
    claims: [],
    responses: [],
    openQuestions: [],
    positionChanges: [],
    recommendation: null,
    alternatives: [],
    consensus: [],
    disagreements: [],
    risks: [],
    decisionRequests: [],
    coverage: null,
    evidenceAliases: [],
    ...partial,
  }
}

function turnFromMessage(
  projection: RoundtableProjection,
  message: RoundtableMessage
): { turn: TranscriptTurn; phaseId: string | null } {
  const attemptId = asString(message.attempt_id)
  const attempt = attemptId
    ? projection.body.replay.attempts.find(
        (item) => item.attempt_id === attemptId
      )
    : undefined
  const replayTurn = attempt
    ? projection.body.replay.turns.find(
        (item) => item.turn_id === attempt.turn_id
      )
    : undefined
  const speakerId =
    asString(message.speaker_id) ??
    replayTurn?.speaker_id ??
    asString(message.body.speaker_id)
  const phaseId = asString(message.phase_id) ?? replayTurn?.phase_id ?? null
  const membership = currentMessageMembership(
    projection.body.replay.message_memberships,
    message.message_id
  )
  const body = message.body
  const claims = readClaims(body.claims)
  const responses = readResponses(body.responses)
  const recommendation = readConclusion(body.recommendation)
  const alternatives = readConclusions(body.alternatives)
  const consensus = readConsensus(body.consensus_items)
  const disagreements = readConclusions(body.disagreements)
  const risks = readConclusions(body.risks)
  const decisionRequests = readConclusions(body.decision_requests)
  const positionChanges = Array.isArray(body.position_changes)
    ? body.position_changes.flatMap((item) => {
        const reason = asString(asRecord(item)?.reason)
        return reason ? [{ reason }] : []
      })
    : []
  const partial = {
    claims,
    responses,
    recommendation,
    alternatives,
    consensus,
    disagreements,
    risks,
    decisionRequests,
  }
  const turn = emptyTurn({
    key: message.message_id,
    messageId: message.message_id,
    bodyHash: message.body_hash,
    visibility: message.visibility === "published" ? "published" : "staged",
    kind: asString(body.kind),
    phaseId: phaseId ?? "",
    speaker: readSpeaker(projection, speakerId),
    attemptId,
    attemptNo: asCount(message.attempt_no) ?? asCount(attempt?.attempt_no),
    attemptState: asString(message.attempt_state) ?? attempt?.state ?? null,
    finishedAt: asString(message.finished_at),
    publishedSeq:
      typeof membership?.published_seq === "string"
        ? membership.published_seq
        : null,
    summary: asString(body.summary),
    raw: body,
  })
  turn.reason = asString(body.reason)
  turn.claims = claims
  turn.responses = responses
  turn.openQuestions = asStringList(body.open_questions)
  turn.positionChanges = positionChanges
  turn.recommendation = recommendation
  turn.alternatives = alternatives
  turn.consensus = consensus
  turn.disagreements = disagreements
  turn.risks = risks
  turn.decisionRequests = decisionRequests
  turn.coverage = readCoverage(body.coverage)
  turn.evidenceAliases = collectAliases(partial)
  return { turn, phaseId }
}

function inferKind(turns: TranscriptTurn[]): TranscriptPhaseKind {
  const kinds = turns
    .map((turn) => turn.kind)
    .filter(
      (kind): kind is string =>
        kind === "proposal" || kind === "critique" || kind === "synthesis"
    )
  if (kinds.length === 0) return "unknown"
  if (kinds.every((kind) => kind === "synthesis")) return "synthesis"
  if (kinds.every((kind) => kind === "critique")) return "critique"
  if (kinds.every((kind) => kind === "proposal")) return "proposal"
  if (kinds.some((kind) => kind === "critique")) return "critique"
  if (kinds.some((kind) => kind === "proposal")) return "proposal"
  return "unknown"
}

function compareRevision(left: string, right: string): number {
  const leftSeq = asSeq(left)
  const rightSeq = asSeq(right)
  if (leftSeq !== null && rightSeq !== null && leftSeq !== rightSeq)
    return leftSeq < rightSeq ? -1 : 1
  return left.localeCompare(right)
}

/**
 * Group verified messages into proposal, critique, and synthesis.
 * Visibility stays on the message the loader already checked. The highest
 * membership_version supplies published_seq; it does not relabel visibility.
 */
export function buildRoundtableTranscript(
  projection: RoundtableProjection,
  messages: RoundtableMessage[],
  previews: RoundtablePreviewInput[] = []
): RoundtableTranscriptModel {
  const phaseRefs = new Map<
    string,
    { kind: TranscriptPhaseKind; index: number; revision: string }
  >()
  projection.body.phase_refs.forEach((ref, position) => {
    const kind = normalizeKind(ref.kind)
    phaseRefs.set(ref.phase_id, {
      kind,
      index: readIndex(ref.index, KIND_RANK[kind] * 1000 + position),
      revision: ref.revision,
    })
  })
  const buckets = new Map<string, PhaseBucket>()
  const ensure = (
    id: string,
    kind: TranscriptPhaseKind,
    index: number,
    revision: string,
    titleKey: TranscriptTitleKey
  ) => {
    const existing = buckets.get(id)
    if (existing) return existing
    const created: PhaseBucket = {
      id,
      kind,
      index,
      revision,
      titleKey,
      turns: [],
    }
    buckets.set(id, created)
    return created
  }
  const bucketFor = (phaseId: string | null, kind: string | null) => {
    if (phaseId && phaseRefs.has(phaseId)) {
      const ref = phaseRefs.get(phaseId)!
      return ensure(
        phaseId,
        ref.kind,
        ref.index,
        ref.revision,
        ref.kind === "unknown" ? "phaseUnknown" : titleKeyFor(ref.kind)
      )
    }
    if (kind === "proposal")
      return ensure("kind:proposal", "proposal", 0, "0", "phaseProposal")
    if (kind === "critique")
      return ensure("kind:critique", "critique", 1, "0", "phaseCritique")
    if (kind === "synthesis")
      return ensure("kind:synthesis", "synthesis", 2, "0", "phaseSynthesis")
    if (kind === "abstain")
      return ensure("kind:abstain", "unknown", 1500, "0", "abstain")
    return ensure("kind:unknown", "unknown", 3000, "0", "phaseUnknown")
  }

  messages.forEach((message, source) => {
    if (message.visibility === "void") return
    const { turn, phaseId } = turnFromMessage(projection, message)
    const bucket = bucketFor(phaseId, turn.kind)
    turn.phaseId = bucket.id
    bucket.turns.push({
      turn,
      seq: asSeq(turn.publishedSeq),
      ordinal: turn.speaker.ordinal,
      source,
    })
  })

  const claimedAttempts = new Set(
    messages
      .filter((message) => message.visibility !== "void")
      .map((message) => asString(message.attempt_id))
      .filter((attemptId): attemptId is string => attemptId !== null)
  )
  previews.forEach((preview, index) => {
    if (claimedAttempts.has(preview.attemptId)) return
    const attempt = projection.body.replay.attempts.find(
      (item) => item.attempt_id === preview.attemptId
    )
    const replayTurn = attempt
      ? projection.body.replay.turns.find(
          (item) => item.turn_id === attempt.turn_id
        )
      : undefined
    const phaseId = replayTurn?.phase_id ?? preview.phaseId ?? null
    const speaker = readSpeaker(
      projection,
      replayTurn?.speaker_id ?? preview.speakerId ?? null
    )
    const liveKind = preview.phaseKind
    const bucket =
      phaseId &&
      !phaseRefs.has(phaseId) &&
      (liveKind === "proposal" ||
        liveKind === "critique" ||
        liveKind === "synthesis")
        ? ensure(
            phaseId,
            liveKind,
            Math.max(-1, ...[...phaseRefs.values()].map((ref) => ref.index)) +
              1,
            "0",
            titleKeyFor(liveKind)
          )
        : bucketFor(phaseId, null)
    const turn = emptyTurn({
      key: `preview:${preview.attemptId}`,
      messageId: null,
      bodyHash: null,
      visibility: "preview",
      kind: null,
      phaseId: bucket.id,
      speaker,
      attemptId: preview.attemptId,
      attemptNo: asCount(attempt?.attempt_no),
      attemptState: attempt?.state ?? null,
      finishedAt: null,
      publishedSeq: null,
      summary: preview.text,
      raw: null,
    })
    if (preview.live) turn.live = preview.live
    bucket.turns.push({
      turn,
      seq: null,
      ordinal: speaker.ordinal,
      source: messages.length + index,
    })
  })

  const phases: TranscriptPhase[] = [...buckets.values()].map((bucket) => {
    if (bucket.titleKey === "phaseUnknown") {
      const inferred = inferKind(bucket.turns.map((item) => item.turn))
      if (inferred !== "unknown") {
        bucket.kind = inferred
        bucket.titleKey = titleKeyFor(inferred)
      }
    }
    const turns = [...bucket.turns].sort((left, right) => {
      if (left.seq === null && right.seq !== null) return 1
      if (left.seq !== null && right.seq === null) return -1
      if (left.seq !== null && right.seq !== null && left.seq !== right.seq)
        return left.seq < right.seq ? -1 : 1
      if (left.ordinal === null && right.ordinal !== null) return 1
      if (left.ordinal !== null && right.ordinal === null) return -1
      if (
        left.ordinal !== null &&
        right.ordinal !== null &&
        left.ordinal !== right.ordinal
      )
        return left.ordinal - right.ordinal
      return left.source - right.source
    })
    return {
      id: bucket.id,
      kind: bucket.kind,
      index: bucket.index,
      revision: bucket.revision,
      critiqueRound: null,
      titleKey: bucket.titleKey,
      turns: turns.map((item) => item.turn),
    }
  })
  phases.sort(
    (left, right) =>
      left.index - right.index ||
      compareRevision(left.revision, right.revision) ||
      left.id.localeCompare(right.id)
  )
  const critiqueIndexes = [
    ...new Set(
      phases
        .filter((phase) => phase.kind === "critique")
        .map((phase) => phase.index)
    ),
  ].sort((left, right) => left - right)
  const roundByIndex = new Map(
    critiqueIndexes.map((index, position) => [index, position + 1])
  )
  for (const phase of phases) {
    if (phase.kind === "critique")
      phase.critiqueRound = roundByIndex.get(phase.index) ?? null
  }
  const seats: Record<string, TranscriptSpeaker> = {}
  for (const speaker of projection.body.replay.speakers) {
    if (!Number.isSafeInteger(speaker.ordinal) || speaker.ordinal < 0) continue
    seats[`s${speaker.ordinal}`] = readSpeaker(projection, speaker.speaker_id)
  }
  return { phases, critiqueRoundCount: critiqueIndexes.length, seats }
}

const SEAT_ALIAS = /(?<![\w-])s(0|[1-9][0-9]{0,2})(?![\w-])/g
const CODE_SEGMENT = /(```[\s\S]*?(?:```|$)|`[^`\n]*`)/g

/**
 * Replace seat aliases such as `s1` with a reader-facing label. Display only:
 * the verified message body and its hash are never touched. Unknown aliases
 * and anything inside code spans or fences stay as written.
 */
export function replaceSeatAliases(
  text: string,
  labelFor: (alias: string) => string | null
): string {
  if (!/s[0-9]/.test(text)) return text
  return text
    .split(CODE_SEGMENT)
    .map((segment, index) =>
      index % 2 === 1
        ? segment
        : segment.replace(SEAT_ALIAS, (match) => labelFor(match) ?? match)
    )
    .join("")
}
