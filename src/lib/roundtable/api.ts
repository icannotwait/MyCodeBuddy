import type {
  RoundtableCommand,
  RoundtableMessage,
  RoundtableProjection,
  RoundtableEvidence,
  RoundtableSourceEntry,
} from "@/lib/roundtable/types"
import { getTransport } from "@/lib/transport"

export const ROUNDTABLE_COMMANDS = [
  "roundtable_preflight",
  "roundtable_create",
  "roundtable_update_draft",
  "roundtable_start",
  "roundtable_get",
  "roundtable_list",
  "roundtable_pause",
  "roundtable_resume",
  "roundtable_stop",
  "roundtable_interject",
  "roundtable_retry_synthesis",
  "roundtable_events",
  "roundtable_messages",
  "roundtable_evidence",
  "roundtable_operation",
  "roundtable_clone",
  "roundtable_attach",
  "roundtable_detach",
] as const

export type RoundtableCommandName = (typeof ROUNDTABLE_COMMANDS)[number]

export async function roundtableCall<T>(
  command: RoundtableCommandName,
  request: Record<string, unknown>
): Promise<T> {
  return getTransport().call<T>(command, { request })
}

export function roundtableError(error: unknown): string {
  if (
    error &&
    typeof error === "object" &&
    "details" in error &&
    error.details &&
    typeof error.details === "object" &&
    "reason" in error.details &&
    error.details.reason === "provider_credential_missing"
  ) {
    return "Provider credentials are missing. Update the provider settings, then retry."
  }
  if (error instanceof Error) {
    if (error.message === "paid_outcome_unknown")
      return "The previous paid operation is still unconfirmed. Retry it to recover its acknowledgment before submitting another operation."
    return error.message
  }
  if (error && typeof error === "object" && "message" in error) {
    return String(error.message)
  }
  return String(error)
}

export async function verifyRoundtableProjection(
  projection: RoundtableProjection,
  roomId: string
) {
  if (
    projection.body.schema_version !== 1 ||
    projection.body.room_id !== roomId
  ) {
    throw new Error("projection_scope")
  }
  const hash = await roundtableHash(projection.body)
  if (hash !== projection.projection_ref.hash)
    throw new Error("projection_hash")
}

function membershipRevision(value: unknown): bigint | null {
  if (typeof value !== "string" || !/^(0|[1-9][0-9]*)$/.test(value)) return null
  return BigInt(value)
}

/** Page manifests record each message's highest membership_version. */
export function currentMessageMembership(
  memberships: RoundtableProjection["body"]["replay"]["message_memberships"],
  messageId: string
) {
  const rows = memberships.filter((item) => item.message_id === messageId)
  if (rows.length === 0) return undefined
  if (rows.length === 1) return rows[0]
  let current = rows[0]
  let currentVersion = membershipRevision(current.membership_version)
  if (currentVersion === null) return undefined
  for (const row of rows.slice(1)) {
    const version = membershipRevision(row.membership_version)
    if (version === null || version === currentVersion) return undefined
    if (version > currentVersion) {
      current = row
      currentVersion = version
    }
  }
  return current
}

export async function loadRoundtable(roomId: string, projectionId?: string) {
  const response = await roundtableCall<{
    projection: RoundtableProjection
    message_manifest_id: string
  }>("roundtable_get", {
    room_id: roomId,
    ...(projectionId
      ? { read: { projection: { projection_id: projectionId } } }
      : {}),
  })
  const { projection } = response
  await verifyRoundtableProjection(projection, roomId)
  const messages: RoundtableMessage[] = []
  const seen = new Set<string>()
  const cursors = new Set<string>()
  let cursor: string | null = null
  do {
    const page: { messages: RoundtableMessage[]; cursor: string | null } =
      await roundtableCall("roundtable_messages", {
        room_id: roomId,
        manifest_id: response.message_manifest_id,
        ...(cursor ? { cursor } : {}),
      })
    for (const message of page.messages) {
      const ref = projection.body.messages.find(
        (ref) => ref.message_id === message.message_id
      )
      const membership = currentMessageMembership(
        projection.body.replay.message_memberships,
        message.message_id
      )
      if (
        !ref ||
        !membership ||
        seen.has(message.message_id) ||
        membership.visibility !== message.visibility
      )
        throw new Error("message_membership")
      if (
        ref.hash !== message.body_hash ||
        (await roundtableHash(message.body)) !== ref.hash
      )
        throw new Error("message_hash")
      seen.add(message.message_id)
      messages.push(message)
    }
    if (page.cursor !== null) {
      if (typeof page.cursor !== "string" || cursors.has(page.cursor))
        throw new Error("page_cursor")
      cursors.add(page.cursor)
      // Every nonterminal page must advance the fixed membership.
      if (page.messages.length === 0) throw new Error("page_cursor")
    }
    cursor = page.cursor
  } while (cursor !== null)
  if (messages.length !== projection.body.messages.length)
    throw new Error("message_missing")
  return { projection, messages }
}

const knownCauses = new Set([
  "create",
  "config",
  "start",
  "attempt",
  "accept",
  "close",
  "publish",
  "input",
  "control",
  "recovery",
  "terminal",
])

export async function verifyRoundtableReplay(
  previous: RoundtableProjection,
  latest: RoundtableProjection
): Promise<void> {
  const roomId = previous.body.room_id
  const low = previous.body.last_seq
  const high = latest.body.last_seq
  await verifyRoundtableProjection(previous, roomId)
  await verifyRoundtableProjection(latest, roomId)
  if (latest.body.room_id !== roomId || BigInt(high) < BigInt(low))
    throw new Error("replay_watermark")
  if (low === high) {
    if (previous.projection_ref.hash !== latest.projection_ref.hash)
      throw new Error("projection_hash")
    return
  }
  let applied = low
  let finalHash = previous.projection_ref.hash
  let cursor: string | null = null
  const cursors = new Set<string>()
  do {
    const page: {
      events: {
        seq: string | number
        schema_version: number
        cause: string
        projection_ref: { id: string; hash: string }
      }[]
      cursor: string | null
    } = await roundtableCall("roundtable_events", {
      room_id: roomId,
      after_seq: low,
      through_seq: high,
      ...(cursor ? { cursor } : {}),
    })
    for (const event of page.events) {
      if (event.schema_version !== 1 || !knownCauses.has(event.cause))
        throw new Error("unknown_event")
      if (
        BigInt(event.seq) !== BigInt(applied) + BigInt(1) ||
        BigInt(event.seq) > BigInt(high)
      )
        throw new Error("resync_required")
      const version = await loadRoundtable(roomId, event.projection_ref.id)
      if (
        version.projection.projection_ref.hash !== event.projection_ref.hash ||
        BigInt(version.projection.body.last_seq) !== BigInt(event.seq)
      )
        throw new Error("projection_hash")
      applied = String(event.seq)
      finalHash = event.projection_ref.hash
    }
    if (page.cursor !== null) {
      if (
        typeof page.cursor !== "string" ||
        cursors.has(page.cursor) ||
        page.events.length === 0
      )
        throw new Error("page_cursor")
      cursors.add(page.cursor)
    }
    cursor = page.cursor
  } while (cursor !== null)
  if (applied !== high) throw new Error("resync_required")
  if (finalHash !== latest.projection_ref.hash)
    throw new Error("projection_hash")
}

export async function roundtableHash(value: unknown): Promise<string> {
  return roundtableTextHash(canonical(value))
}

export async function loadRoundtableEvidence(
  projection: RoundtableProjection,
  evidenceId: string
): Promise<RoundtableEvidence> {
  const member = projection.body.replay.evidence.find(
    (item) => item.evidence_id === evidenceId
  )
  if (
    !member ||
    member.published_seq === null ||
    BigInt(member.published_seq) > BigInt(projection.body.last_seq)
  )
    throw new Error("evidence_membership")
  const response = await roundtableCall<RoundtableEvidence>(
    "roundtable_evidence",
    {
      room_id: projection.body.room_id,
      evidence_id: evidenceId,
    }
  )
  if (
    response.hash !== member.content_hash ||
    (await roundtableHash(response.body)) !== member.body_hash
  )
    throw new Error("evidence_hash")
  if (
    (await roundtableTextHash(response.body.excerpt)) !==
    response.body.excerpt_hash
  )
    throw new Error("evidence_excerpt_hash")
  return response
}

export async function roundtableTextHash(value: string): Promise<string> {
  const bytes = new TextEncoder().encode(value)
  const hash = await crypto.subtle.digest("SHA-256", bytes)
  return Array.from(new Uint8Array(hash), (byte) =>
    byte.toString(16).padStart(2, "0")
  ).join("")
}

/** CanonicalV1 uses UTF-8 key order and uniform lowercase control escapes. */
function canonical(value: unknown): string {
  if (typeof value === "string") {
    return (
      '"' +
      value.replace(/["\\\u0000-\u001f]/g, (ch) =>
        ch === '"'
          ? '\\"'
          : ch === "\\"
            ? "\\\\"
            : `\\u${ch.charCodeAt(0).toString(16).padStart(4, "0")}`
      ) +
      '"'
    )
  }
  if (Array.isArray(value)) return `[${value.map(canonical).join(",")}]`
  if (value !== null && typeof value === "object") {
    const object = value as Record<string, unknown>
    const keys = Object.keys(object).sort((left, right) => {
      const a = Array.from(left, (ch) => ch.codePointAt(0)!)
      const b = Array.from(right, (ch) => ch.codePointAt(0)!)
      for (let i = 0; i < Math.min(a.length, b.length); i++) {
        if (a[i] !== b[i]) return a[i] - b[i]
      }
      return a.length - b.length
    })
    return `{${keys.map((key) => `${canonical(key)}:${canonical(object[key])}`).join(",")}}`
  }
  if (
    value === null ||
    typeof value === "boolean" ||
    (typeof value === "number" && Number.isSafeInteger(value))
  )
    return JSON.stringify(value)
  throw new Error("projection_value")
}

const STATUS: Record<string, number> = {
  capacity_limited: 429,
  insufficient_budget: 422,
  forbidden: 403,
  not_found: 404,
}

export function roundtableBody(command: RoundtableCommand) {
  const limit = command.pageLimit ?? 100
  if (limit < 1 || limit > 500) {
    throw new Error("page_limit")
  }
  return {
    command: command.command,
    room_id: command.roomId ?? null,
    page_limit: limit,
  }
}

export function roundtableStatus(code: string) {
  return STATUS[code] ?? 400
}

/** Read immutable source objects only, checking the complete bounded byte stream. */
export async function loadRoundtableSource(
  roomId: string,
  entry: RoundtableSourceEntry
): Promise<string> {
  if (
    !entry.text_admissible ||
    entry.size > 1024 * 1024 ||
    entry.size < 0 ||
    entry.size !== entry.object.total_bytes ||
    entry.content_hash !== entry.object.content_hash ||
    entry.object.object_id !== entry.content_hash
  )
    throw new Error("source_reference")
  const object = { ...entry.object, kind: "source_excerpt" }
  let cursor: string | null = null
  const seen = new Set<string>()
  let text = ""
  let length = 0
  do {
    const page: {
      object_ref: typeof object
      offset: number
      text: string
      cursor: string | null
    } = await roundtableCall("roundtable_get", {
      room_id: roomId,
      read: { object: { object_ref: object, ...(cursor ? { cursor } : {}) } },
    })
    if (
      page.offset !== length ||
      (await roundtableHash(page.object_ref)) !== (await roundtableHash(object))
    )
      throw new Error("source_reference")
    length += new TextEncoder().encode(page.text).length
    if (length > entry.size) throw new Error("source_hash")
    text += page.text
    cursor = page.cursor
    if (cursor !== null) {
      if (
        typeof cursor !== "string" ||
        seen.has(cursor) ||
        page.text.length === 0
      )
        throw new Error("page_cursor")
      seen.add(cursor)
    }
  } while (cursor !== null)
  if (
    length !== entry.size ||
    (await roundtableTextHash(text)) !== entry.content_hash
  )
    throw new Error("source_hash")
  return text
}
