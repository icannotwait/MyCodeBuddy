import { randomUUID } from "@/lib/utils"

const DEVICE_ID_STORAGE_KEY = "codeg.sharedSession.deviceId.v1"

const clientInstanceId = randomUUID()
let deviceId: string | null = null

export interface SharedClientIdentity {
  /** Diagnostic label only; authentication remains the server bearer token. */
  deviceId: string
  /** Document-lifetime diagnostic label, never an authorization or path input. */
  clientInstanceId: string
}

export function getSharedClientIdentity(): SharedClientIdentity {
  if (deviceId === null) {
    deviceId = readOrCreateDeviceId()
  }
  return { deviceId, clientInstanceId }
}

export function newSharedRequestId(): string {
  return randomUUID()
}

function readOrCreateDeviceId(): string {
  try {
    const stored = globalThis.localStorage?.getItem(DEVICE_ID_STORAGE_KEY)
    if (stored) return stored
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
