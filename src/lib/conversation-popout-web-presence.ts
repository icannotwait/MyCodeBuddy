/**
 * Same-origin liveness for a web conversation pop-out.
 *
 * The opener keeps the `window` handle (authoritative while that document
 * lives). Every pop-out also heartbeats through localStorage and answers
 * BroadcastChannel ping/focus so a reloaded workspace can still refuse a
 * second owner attach. Closing the pop-out clears the record after a short
 * reload grace so the main workspace can reclaim the conversation.
 */

import { FOCUS_COMPOSER_EVENT } from "@/lib/conversation-popout-detached-bootstrap"
import type { AgentType } from "@/lib/types"

export const WEB_POPOUT_PRESENCE_STORAGE_KEY = "codeg.webConversationPopout.v1"
export const WEB_POPOUT_PRESENCE_CHANNEL = "codeg-web-conversation-popout"

const HEARTBEAT_MS = 2_000
const DEFAULT_STALE_MS = 8_000
const DEFAULT_RELOAD_GRACE_MS = 3_000
const DEFAULT_ACK_TIMEOUT_MS = 400

export type WebPopoutFocusResult = "absent" | "focused" | "refused"

type PresencePhase = "open" | "maybe-closed"

type PresenceRecord = {
  conversationId: number
  folderId: number
  agentType: string
  heartbeatAt: number
  phase: PresencePhase
}

type PresenceFile = Record<string, PresenceRecord>

type ChannelMessage =
  | {
      type: "claimed" | "heartbeat"
      conversationId: number
      folderId: number
      agentType: string
      heartbeatAt: number
    }
  | {
      type: "maybe-closed"
      conversationId: number
      heartbeatAt: number
    }
  | {
      type: "ping" | "focus"
      conversationId: number
      requestId: string
    }
  | {
      type: "pong" | "focused"
      conversationId: number
      requestId: string
    }

const windows = new Map<number, Window>()
const pendingAcks = new Map<
  string,
  { type: "pong" | "focused"; resolve: () => void }
>()

let staleMs = DEFAULT_STALE_MS
let reloadGraceMs = DEFAULT_RELOAD_GRACE_MS
let ackTimeoutMs = DEFAULT_ACK_TIMEOUT_MS
let openerTimer: ReturnType<typeof setInterval> | null = null
let openerChannel: BroadcastChannel | null = null
const bindCleanups = new Set<() => void>()

function nowMs(): number {
  return Date.now()
}

function storage(): Storage | null {
  try {
    return globalThis.localStorage ?? null
  } catch {
    return null
  }
}

function readFile(): PresenceFile {
  const raw = storage()?.getItem(WEB_POPOUT_PRESENCE_STORAGE_KEY)
  if (!raw) return {}
  try {
    const parsed = JSON.parse(raw) as unknown
    if (!parsed || typeof parsed !== "object") return {}
    return parsed as PresenceFile
  } catch {
    return {}
  }
}

function writeFile(file: PresenceFile): void {
  try {
    const store = storage()
    if (!store) return
    if (Object.keys(file).length === 0) {
      store.removeItem(WEB_POPOUT_PRESENCE_STORAGE_KEY)
      return
    }
    store.setItem(WEB_POPOUT_PRESENCE_STORAGE_KEY, JSON.stringify(file))
  } catch {
    /* storage full or blocked */
  }
}

function readRecord(conversationId: number): PresenceRecord | null {
  const record = readFile()[String(conversationId)]
  if (!record || record.conversationId !== conversationId) return null
  if (!Number.isFinite(record.heartbeatAt)) return null
  return record
}

function writeRecord(record: PresenceRecord): void {
  const file = readFile()
  file[String(record.conversationId)] = record
  writeFile(file)
}

function clearRecord(conversationId: number): void {
  const file = readFile()
  if (!(String(conversationId) in file)) return
  delete file[String(conversationId)]
  writeFile(file)
}

function openChannel(): BroadcastChannel | null {
  if (typeof BroadcastChannel === "undefined") return null
  try {
    return new BroadcastChannel(WEB_POPOUT_PRESENCE_CHANNEL)
  } catch {
    return null
  }
}

function post(message: ChannelMessage): void {
  const channel = openerChannel ?? openChannel()
  if (!channel) return
  try {
    channel.postMessage(message)
  } catch {
    /* closed channel */
  }
  if (channel !== openerChannel) {
    try {
      channel.close()
    } catch {
      /* ignore */
    }
  }
}

function ensureOpenerChannel(): void {
  if (openerChannel) return
  openerChannel = openChannel()
  if (!openerChannel) return
  openerChannel.onmessage = (event: MessageEvent<ChannelMessage>) => {
    const data = event.data
    if (!data || (data.type !== "pong" && data.type !== "focused")) return
    const pending = pendingAcks.get(data.requestId)
    if (!pending || pending.type !== data.type) return
    if (data.conversationId <= 0) return
    pendingAcks.delete(data.requestId)
    pending.resolve()
  }
}

function dropClosedHandles(): void {
  for (const [conversationId, popup] of windows) {
    if (!popup.closed) continue
    windows.delete(conversationId)
    clearRecord(conversationId)
  }
}

function ensureOpenerHeartbeat(): void {
  if (openerTimer != null) return
  openerTimer = setInterval(() => {
    dropClosedHandles()
    let live = 0
    for (const [conversationId, popup] of windows) {
      if (popup.closed) continue
      live += 1
      const current = readRecord(conversationId)
      if (!current) continue
      writeRecord({
        ...current,
        phase: "open",
        heartbeatAt: nowMs(),
      })
      post({
        type: "heartbeat",
        conversationId,
        folderId: current.folderId,
        agentType: current.agentType,
        heartbeatAt: nowMs(),
      })
    }
    if (live === 0 && openerTimer != null) {
      clearInterval(openerTimer)
      openerTimer = null
    }
  }, HEARTBEAT_MS)
}

function requestId(): string {
  if (typeof crypto !== "undefined" && crypto.randomUUID) {
    return crypto.randomUUID()
  }
  return `popout-${nowMs()}-${Math.random().toString(16).slice(2)}`
}

function waitForAck(
  type: "pong" | "focused",
  conversationId: number,
  timeoutMs: number
): Promise<boolean> {
  ensureOpenerChannel()
  if (!openerChannel) return Promise.resolve(false)
  const id = requestId()
  return new Promise((resolve) => {
    const timer = setTimeout(() => {
      pendingAcks.delete(id)
      resolve(false)
    }, timeoutMs)
    pendingAcks.set(id, {
      type,
      resolve: () => {
        clearTimeout(timer)
        resolve(true)
      },
    })
    post(
      type === "pong"
        ? { type: "ping", conversationId, requestId: id }
        : { type: "focus", conversationId, requestId: id }
    )
  })
}

/**
 * Conversation ids the main workspace must not attach as an owner.
 * Includes a fresh pop-out, a window handle, a reload grace, and a stale
 * `open` record that still needs a ping before it can be reclaimed.
 */
export function webPopoutConversationIdsBlockingMain(): Set<number> {
  dropClosedHandles()
  const ids = new Set<number>()
  for (const conversationId of windows.keys()) ids.add(conversationId)
  const now = nowMs()
  for (const record of Object.values(readFile())) {
    if (!record || record.conversationId <= 0) continue
    if (record.phase === "maybe-closed") {
      if (now - record.heartbeatAt < reloadGraceMs) {
        ids.add(record.conversationId)
      }
      continue
    }
    if (record.phase === "open") ids.add(record.conversationId)
  }
  return ids
}

export function publishWebPopoutOpened(args: {
  conversationId: number
  folderId: number
  agentType: AgentType | string
}): void {
  if (args.conversationId <= 0) return
  const heartbeatAt = nowMs()
  const record: PresenceRecord = {
    conversationId: args.conversationId,
    folderId: args.folderId,
    agentType: args.agentType,
    heartbeatAt,
    phase: "open",
  }
  writeRecord(record)
  post({
    type: "claimed",
    conversationId: args.conversationId,
    folderId: args.folderId,
    agentType: args.agentType,
    heartbeatAt,
  })
}

export function rememberWebPopoutWindow(
  conversationId: number,
  popup: Window
): void {
  if (conversationId <= 0 || popup.closed) return
  windows.set(conversationId, popup)
  ensureOpenerChannel()
  ensureOpenerHeartbeat()
}

function markMaybeClosed(conversationId: number): void {
  const current = readRecord(conversationId)
  const heartbeatAt = nowMs()
  writeRecord({
    conversationId,
    folderId: current?.folderId ?? 0,
    agentType: current?.agentType ?? "",
    heartbeatAt,
    phase: "maybe-closed",
  })
  post({ type: "maybe-closed", conversationId, heartbeatAt })
}

/**
 * Pop-out document: announce ownership, answer focus/ping, and drop presence
 * when this document is leaving. The page stays the interactive owner; this
 * does not change connection intent.
 */
export function bindWebPopoutOwnerPresence(args: {
  conversationId: number
  folderId: number
  agentType: AgentType | string
}): () => void {
  if (args.conversationId <= 0 || typeof window === "undefined") {
    return () => {}
  }
  publishWebPopoutOpened(args)
  const channel = openChannel()
  const beat = () => {
    const heartbeatAt = nowMs()
    writeRecord({
      conversationId: args.conversationId,
      folderId: args.folderId,
      agentType: args.agentType,
      heartbeatAt,
      phase: "open",
    })
    channel?.postMessage({
      type: "heartbeat",
      conversationId: args.conversationId,
      folderId: args.folderId,
      agentType: args.agentType,
      heartbeatAt,
    } satisfies ChannelMessage)
  }
  const timer = setInterval(beat, HEARTBEAT_MS)
  const onMessage = (event: MessageEvent<ChannelMessage>) => {
    const data = event.data
    if (!data || data.conversationId !== args.conversationId) return
    if (data.type === "ping") {
      beat()
      channel?.postMessage({
        type: "pong",
        conversationId: args.conversationId,
        requestId: data.requestId,
      } satisfies ChannelMessage)
      return
    }
    if (data.type === "focus") {
      try {
        window.focus()
      } catch {
        /* ignore */
      }
      window.dispatchEvent(new Event(FOCUS_COMPOSER_EVENT))
      channel?.postMessage({
        type: "focused",
        conversationId: args.conversationId,
        requestId: data.requestId,
      } satisfies ChannelMessage)
    }
  }
  channel?.addEventListener("message", onMessage)
  const onPageHide = () => {
    markMaybeClosed(args.conversationId)
  }
  window.addEventListener("pagehide", onPageHide)
  const cleanup = () => {
    clearInterval(timer)
    window.removeEventListener("pagehide", onPageHide)
    channel?.removeEventListener("message", onMessage)
    try {
      channel?.close()
    } catch {
      /* ignore */
    }
    markMaybeClosed(args.conversationId)
  }
  bindCleanups.add(cleanup)
  return () => {
    if (!bindCleanups.delete(cleanup)) return
    cleanup()
  }
}

async function confirmAlive(conversationId: number): Promise<boolean> {
  dropClosedHandles()
  const popup = windows.get(conversationId)
  if (popup && !popup.closed) return true
  const record = readRecord(conversationId)
  if (!record) return false
  const age = nowMs() - record.heartbeatAt
  if (record.phase === "maybe-closed") {
    if (age < reloadGraceMs) return true
    clearRecord(conversationId)
    return false
  }
  if (age < staleMs) return true
  const answered = await waitForAck("pong", conversationId, ackTimeoutMs)
  if (answered) return true
  clearRecord(conversationId)
  windows.delete(conversationId)
  return false
}

/**
 * Focus a live web pop-out. `absent` means main may open it. `focused` means
 * the existing window was brought forward. `refused` means it is still alive
 * but could not be focused — main must not attach.
 */
export async function focusOrRefuseWebPopout(
  conversationId: number
): Promise<WebPopoutFocusResult> {
  if (conversationId <= 0) return "absent"
  if (!(await confirmAlive(conversationId))) return "absent"
  const popup = windows.get(conversationId)
  if (popup && !popup.closed) {
    try {
      popup.focus()
    } catch {
      /* the handle is still the live owner */
    }
    return "focused"
  }
  const focused = await waitForAck("focused", conversationId, ackTimeoutMs)
  return focused ? "focused" : "refused"
}

export function installWorkspaceWebPopoutClaimListener(
  onClaimed: (conversationId: number) => void
): () => void {
  if (typeof window === "undefined") return () => {}
  const channel = openChannel()
  if (!channel) return () => {}
  const onMessage = (event: MessageEvent<ChannelMessage>) => {
    const data = event.data
    if (!data) return
    if (data.type !== "claimed" && data.type !== "heartbeat") return
    if (data.conversationId <= 0) return
    onClaimed(data.conversationId)
  }
  channel.addEventListener("message", onMessage)
  return () => {
    channel.removeEventListener("message", onMessage)
    try {
      channel.close()
    } catch {
      /* ignore */
    }
  }
}

/** Test helper: drop handles, records, and opener timers. */
export function __resetWebPopoutPresenceForTests(): void {
  const cleanups = [...bindCleanups]
  bindCleanups.clear()
  for (const cleanup of cleanups) cleanup()
  windows.clear()
  pendingAcks.clear()
  if (openerTimer != null) {
    clearInterval(openerTimer)
    openerTimer = null
  }
  if (openerChannel) {
    try {
      openerChannel.close()
    } catch {
      /* ignore */
    }
    openerChannel = null
  }
  staleMs = DEFAULT_STALE_MS
  reloadGraceMs = DEFAULT_RELOAD_GRACE_MS
  ackTimeoutMs = DEFAULT_ACK_TIMEOUT_MS
  try {
    storage()?.removeItem(WEB_POPOUT_PRESENCE_STORAGE_KEY)
  } catch {
    /* ignore */
  }
}

/** Test helper: shorten liveness waits. `null` restores defaults. */
export function __setWebPopoutTimingsForTests(
  opts: {
    ackTimeoutMs?: number
    staleMs?: number
    reloadGraceMs?: number
  } | null
): void {
  if (opts == null) {
    ackTimeoutMs = DEFAULT_ACK_TIMEOUT_MS
    staleMs = DEFAULT_STALE_MS
    reloadGraceMs = DEFAULT_RELOAD_GRACE_MS
    return
  }
  if (opts.ackTimeoutMs != null) ackTimeoutMs = opts.ackTimeoutMs
  if (opts.staleMs != null) staleMs = opts.staleMs
  if (opts.reloadGraceMs != null) reloadGraceMs = opts.reloadGraceMs
}
