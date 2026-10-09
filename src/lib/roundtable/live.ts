import type { RoundtableProjection } from "@/lib/roundtable/types"
import type { RoundtablePreviewInput } from "@/lib/roundtable/transcript"

/**
 * Display-only live output of in-flight attempts (`roundtable_live` frames).
 *
 * This text is unverified: it is never hashed, never checked against the
 * projection and never treated as a room message. The transcript shows it
 * only until a verified (staged or published) message claims the attempt.
 */
export interface RoundtableLiveAttempt {
  attemptId: string
  speakerId: string
  phaseId: string
  phaseKind: string
  incarnation: string
  ended: boolean
  activity: string | null
  message: string
  /** UTF-8 byte length of `message` as sent by the server. */
  messageBytes: number
  messageTruncated: boolean
  thought: string
  /** Absolute byte offset of the first retained thinking byte. */
  thoughtStart: number
  /** Absolute byte offset just past the retained thinking. */
  thoughtEnd: number
}

export type RoundtableLiveState = Record<string, RoundtableLiveAttempt>

/** Client-side caps mirror the server's (32 KiB message, 16 KiB thinking). */
export const LIVE_MESSAGE_CAP_BYTES = 32 * 1024
export const LIVE_THOUGHT_CAP_BYTES = 16 * 1024

const encoder = new TextEncoder()
const byteLength = (text: string) => encoder.encode(text).byteLength

function keepTail(text: string, start: number, cap: number) {
  const bytes = encoder.encode(text)
  if (bytes.byteLength <= cap) return { text, start }
  let cut = bytes.byteLength - cap
  // Never split a UTF-8 sequence: skip continuation bytes.
  while (cut < bytes.byteLength && (bytes[cut] & 0xc0) === 0x80) cut += 1
  return {
    text: new TextDecoder().decode(bytes.subarray(cut)),
    start: start + cut,
  }
}

function isNonNegativeInt(value: unknown): value is number {
  return typeof value === "number" && Number.isSafeInteger(value) && value >= 0
}

/**
 * Apply one server frame. `resync` asks the caller to re-attach (which makes
 * the server resend whole buffers) when a delta does not line up.
 */
export function applyRoundtableLive(
  state: RoundtableLiveState,
  payload: unknown,
  roomId: string
): { state: RoundtableLiveState; resync: boolean } {
  if (!payload || typeof payload !== "object") return { state, resync: false }
  const frame = payload as Record<string, unknown>
  if (frame.type !== "roundtable_live" || frame.room_id !== roomId)
    return { state, resync: false }
  const next: RoundtableLiveState = { ...state }
  let resync = false
  if (Array.isArray(frame.removed))
    for (const id of frame.removed) if (typeof id === "string") delete next[id]
  for (const raw of Array.isArray(frame.attempts) ? frame.attempts : []) {
    if (!raw || typeof raw !== "object") continue
    const item = raw as Record<string, unknown>
    const message = item.message as Record<string, unknown> | undefined
    const thought = item.thought as Record<string, unknown> | undefined
    if (
      typeof item.attempt_id !== "string" ||
      typeof item.incarnation !== "string" ||
      !message ||
      !thought ||
      typeof message.text !== "string" ||
      typeof thought.text !== "string" ||
      !isNonNegativeInt(message.offset) ||
      !isNonNegativeInt(thought.offset)
    )
      continue
    const id = item.attempt_id
    const previous =
      next[id]?.incarnation === item.incarnation ? next[id] : undefined
    let text = previous?.message ?? ""
    let bytes = previous?.messageBytes ?? 0
    if (message.offset === 0) {
      text = message.text
      bytes = byteLength(message.text)
    } else if (previous && message.offset === bytes) {
      text += message.text
      bytes += byteLength(message.text)
    } else {
      resync = true
      continue
    }
    if (bytes > LIVE_MESSAGE_CAP_BYTES) {
      // The server caps first; anything beyond is a protocol error.
      resync = true
      continue
    }
    let thoughtText = previous?.thought ?? ""
    let thoughtStart = previous?.thoughtStart ?? 0
    let thoughtEnd = previous?.thoughtEnd ?? 0
    if (previous && thought.offset === thoughtEnd) {
      thoughtText += thought.text
      thoughtEnd += byteLength(thought.text)
    } else {
      thoughtText = thought.text
      thoughtStart = thought.offset
      thoughtEnd = thought.offset + byteLength(thought.text)
    }
    const tail = keepTail(thoughtText, thoughtStart, LIVE_THOUGHT_CAP_BYTES)
    next[id] = {
      attemptId: id,
      speakerId: typeof item.speaker_id === "string" ? item.speaker_id : "",
      phaseId: typeof item.phase_id === "string" ? item.phase_id : "",
      phaseKind: typeof item.phase_kind === "string" ? item.phase_kind : "",
      incarnation: item.incarnation,
      ended: item.ended === true,
      activity: typeof item.activity === "string" ? item.activity : null,
      message: text,
      messageBytes: bytes,
      messageTruncated: message.truncated === true,
      thought: tail.text,
      thoughtStart: tail.start,
      thoughtEnd,
    }
  }
  return { state: next, resync }
}

/**
 * Attempt states that may still show live output. Terminal failures
 * (failed, timed_out, invalid, interrupted, uncertain) drop it at once;
 * accepted attempts keep it only until their verified message is listed.
 */
const LIVE_ATTEMPT_STATES = new Set([
  "reserved",
  "launching",
  "admitting",
  "admitted",
  "streaming",
  "validating",
  "active",
  "accepted",
])

/**
 * Live buffers the transcript may show for this projection, of a running
 * room. An attempt the verified projection already lists must still be live.
 * The projection is re-read on room events, so a just-started attempt may not
 * be listed yet; its live output is shown only when its seat belongs to this
 * room and its phase is not already published. Claiming by a verified message
 * is handled by the transcript itself.
 */
export function liveTranscriptInputs(
  state: RoundtableLiveState,
  projection: RoundtableProjection | null | undefined
): RoundtablePreviewInput[] {
  if (!projection) return []
  if (!["running", "pausing", "stopping"].includes(projection.body.status))
    return []
  const attempts = new Map(
    projection.body.replay.attempts.map((attempt) => [
      attempt.attempt_id,
      attempt,
    ])
  )
  const speakers = new Set(
    projection.body.replay.speakers.map((speaker) => speaker.speaker_id)
  )
  const published = new Set(
    projection.body.phase_refs
      .filter((ref) => ref.state === "published")
      .map((ref) => ref.phase_id)
  )
  return Object.values(state).flatMap((live) => {
    const attempt = attempts.get(live.attemptId)
    if (attempt ? !LIVE_ATTEMPT_STATES.has(attempt.state) : false) return []
    if (
      !attempt &&
      (!speakers.has(live.speakerId) || published.has(live.phaseId))
    )
      return []
    return [
      {
        attemptId: live.attemptId,
        text: live.message,
        speakerId: live.speakerId,
        phaseId: live.phaseId,
        phaseKind: live.phaseKind,
        live: {
          thought: live.thought,
          thoughtOmitted: live.thoughtStart > 0,
          activity: live.activity,
          ended: live.ended,
          truncated: live.messageTruncated,
        },
      },
    ]
  })
}
