/**
 * Recognizing the backend's "a turn is already in flight on this connection"
 * rejection across transports.
 *
 * Tauri and web both surface structured `AppCommandError` for
 * `TurnInProgress` (stable code `turn_in_progress`) and related busy signals
 * such as `ContinuationInProgress`. Recognition prefers that stable `code`
 * when present, and also accepts a backend-controlled marker substring of the
 * Display/`message` text for string-shaped rejections. Kept in its own
 * dependency-free module so the recognition is unit-testable without loading
 * the full API client.
 */

/**
 * Thrown by `acpPrompt` when the backend rejects the send because a turn is
 * already in flight on the connection (a second, concurrent prompt — e.g. two
 * clients co-controlling one conversation). Callers re-queue the draft in the
 * message queue rather than surfacing an error.
 */
export class TurnBusyError extends Error {
  constructor() {
    super("turn already in progress for this connection")
    this.name = "TurnBusyError"
  }
}

// Substring of the backend `AcpError::TurnInProgress` Display string. Matching a
// substring (not the whole string) keeps recognition working if the backend
// later elaborates the message.
const TURN_IN_PROGRESS_MARKER = "turn already in progress"

// Stable code from structured `AppCommandError` for `TurnInProgress` (Tauri and
// web HTTP 409) and `AcpError::code()`.
const TURN_IN_PROGRESS_CODE = "turn_in_progress"

/**
 * True when `err` is the backend's turn-in-progress rejection, in any of the
 * shapes the transports produce: a bare string (legacy Display), an object
 * with a `message` carrying the marker, or an object with the stable `code`
 * (structured `AppCommandError` on Tauri and web). Anything else is a genuine
 * error and returns false.
 */
export function isTurnInProgressRejection(err: unknown): boolean {
  if (typeof err === "string") return err.includes(TURN_IN_PROGRESS_MARKER)
  if (err && typeof err === "object") {
    // Structured AppCommandError (Tauri and web): prefer stable `code`.
    if ((err as { code?: unknown }).code === TURN_IN_PROGRESS_CODE) return true
    const message = (err as { message?: unknown }).message
    if (typeof message === "string")
      return message.includes(TURN_IN_PROGRESS_MARKER)
  }
  return false
}

// Substring of the backend `AcpError::NoActiveTurn` Display string. Returned
// when live feedback is submitted but no turn is in flight (the agent already
// finished). The caller falls back to sending the text as an ordinary prompt.
const NO_ACTIVE_TURN_MARKER = "no active turn"

/**
 * True when `err` is the backend's "no active turn for feedback" rejection, in
 * any transport shape: a bare string (Tauri `AcpError` Display), or an object
 * with a `message` carrying the marker (web `AppCommandError`). The web path
 * maps this to a 4xx, so it is an expected, recoverable signal — not a fault.
 */
export function isNoActiveTurnRejection(err: unknown): boolean {
  if (typeof err === "string") return err.includes(NO_ACTIVE_TURN_MARKER)
  if (err && typeof err === "object") {
    const message = (err as { message?: unknown }).message
    if (typeof message === "string")
      return message.includes(NO_ACTIVE_TURN_MARKER)
  }
  return false
}
