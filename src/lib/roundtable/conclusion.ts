import { isDesktop } from "@/lib/platform"
import { getTransport } from "@/lib/transport"

/** Server-rendered Markdown of the accepted synthesis (redacted). */
export type RoundtableConclusionExport = {
  room_id: string
  message_id: string
  body_hash: string
  file_name: string
  relative_path: string
  markdown: string
  redactions: number
  workspace_available: boolean
  saves: RoundtableConclusionSave[]
}

export type RoundtableConclusionSaveMode = "create" | "overwrite" | "save_as"

export type RoundtableConclusionSave = {
  request_id: string
  mode: RoundtableConclusionSaveMode
  relative_path: string
  sha256: string
  bytes: number
  status: "created" | "overwritten"
  saved_at: string
  replayed?: boolean
}

// These two read-only commands live outside the sealed protocol command set.
export function exportRoundtableConclusion(roomId: string, locale: string) {
  return getTransport().call<RoundtableConclusionExport>(
    "roundtable_conclusion_export",
    { request: { room_id: roomId, locale } }
  )
}

export function saveRoundtableConclusion(
  roomId: string,
  locale: string,
  mode: RoundtableConclusionSaveMode,
  requestId: string = crypto.randomUUID()
) {
  return getTransport().call<RoundtableConclusionSave>(
    "roundtable_conclusion_save",
    { request: { room_id: roomId, request_id: requestId, mode, locale } }
  )
}

export function conclusionErrorReason(error: unknown): string | null {
  if (
    error &&
    typeof error === "object" &&
    "details" in error &&
    error.details &&
    typeof error.details === "object" &&
    "reason" in error.details &&
    typeof error.details.reason === "string"
  )
    return error.details.reason
  return null
}

/** Desktop: native Save As. Web: browser download. False when cancelled. */
export async function downloadMarkdown(
  fileName: string,
  markdown: string
): Promise<boolean> {
  if (isDesktop()) {
    const { save } = await import("@tauri-apps/plugin-dialog")
    const { invoke } = await import("@tauri-apps/api/core")
    const path = await save({
      defaultPath: fileName,
      filters: [{ name: "Markdown", extensions: ["md"] }],
    })
    if (!path) return false
    const bytes = new TextEncoder().encode(markdown)
    let binary = ""
    for (const byte of bytes) binary += String.fromCharCode(byte)
    await invoke("save_binary_file", { path, dataBase64: btoa(binary) })
    return true
  }
  const blob = new Blob([markdown], { type: "text/markdown;charset=utf-8" })
  const url = URL.createObjectURL(blob)
  const link = document.createElement("a")
  link.href = url
  link.download = fileName
  document.body.append(link)
  link.click()
  link.remove()
  URL.revokeObjectURL(url)
  return true
}
