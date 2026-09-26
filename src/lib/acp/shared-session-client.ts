import { randomUUID } from "@/lib/utils"

const DEVICE_ID_STORAGE_KEY = "codeg.sharedSession.deviceId.v1"
const UUID_V4_PATTERN =
  /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i

const clientInstanceId = randomUUID()
let deviceId: string | null = null

export interface SharedClientIdentity {
  /** Diagnostic label only; authentication remains the server bearer token. */
  deviceId: string
  /** Document/attachment identity, never an authorization or path input. */
  clientInstanceId: string
}

export function getSharedClientIdentity(
  attachmentId?: string
): SharedClientIdentity {
  if (deviceId === null) {
    deviceId = readOrCreateDeviceId()
  }
  // The broker keeps one active lease per client per session. Independent
  // views must not rotate each other's leases; retries keep the attachment id.
  return {
    deviceId,
    clientInstanceId: attachmentId
      ? `${clientInstanceId}:${attachmentId}`
      : clientInstanceId,
  }
}

export function newSharedRequestId(): string {
  return randomUUID()
}

function readOrCreateDeviceId(): string {
  try {
    const stored = globalThis.localStorage?.getItem(DEVICE_ID_STORAGE_KEY)
    if (stored && UUID_V4_PATTERN.test(stored)) return stored
  } catch {
    return randomUUID()
  }

  const generated = randomUUID()
  try {
    globalThis.localStorage?.setItem(DEVICE_ID_STORAGE_KEY, generated)
  } catch {
    // The module-level value remains stable when storage is unavailable/full.
  }
  return generated
}
