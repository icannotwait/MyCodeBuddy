import type { PromptInputBlock } from "@/lib/types"
import { randomUUID } from "@/lib/utils"

import type { InputAttachment } from "../message-input-attachments"
import { parseCodegReferenceUri as parseReferenceUri } from "./reference-uri"
import type { ReferenceAttrs } from "./types"

/**
 * Restore serialization (loose inverse of
 * {@link "./to-prompt-blocks".docToPromptBlocks}): turn a sent
 * `PromptInputBlock[]` back into editor content + attachments, so a queued
 * message can be re-opened for editing with its references and attachments
 * intact.
 *
 * The split mirrors the send rule:
 * - `text` blocks → plain-text segments replayed into the editor verbatim. Every
 *   inline reference that was serialized *as text* comes back in that text form:
 *   file links `[name](file://…)` (which `docToPromptBlocks` keeps inline) and
 *   session/commit/agent/skill references alike replay as their inline link/token
 *   text, not re-hydrated badges — consistent across every reference kind on a
 *   queue-edit.
 * - `resource_link` blocks whose uri is a composer scheme (`file:` / `codeg:`)
 *   → reference badge segments. `docToPromptBlocks` no longer emits file
 *   resource_links (files stay inline above), but this branch still restores any
 *   composer-scheme resource_link the host appended out of band (e.g. an embedded
 *   payload).
 * - everything else (`image`, embedded `resource`, non-composer `resource_link`)
 *   → out-of-band attachments.
 *
 * The host replays `segments` in order against a live editor (text via
 * `insertTextAtCursor`, references via `insertReference`) and sets
 * `attachments`. Pure and deterministic given an injected `makeId`.
 */
export type RestoreSegment =
  | { kind: "text"; text: string }
  | { kind: "reference"; attrs: ReferenceAttrs }

export interface RestoredDraft {
  segments: RestoreSegment[]
  attachments: InputAttachment[]
}

export function blocksToRestoredDraft(
  blocks: PromptInputBlock[],
  makeId: () => string = randomUUID
): RestoredDraft {
  const segments: RestoreSegment[] = []
  const attachments: InputAttachment[] = []

  for (const block of blocks) {
    switch (block.type) {
      case "text": {
        if (block.text.trim().length > 0) {
          segments.push({ kind: "text", text: block.text })
        }
        break
      }
      case "resource_link": {
        const attrs = parseReferenceUri(block.uri, block.name)
        if (attrs) {
          segments.push({ kind: "reference", attrs })
        } else {
          attachments.push({
            id: makeId(),
            type: "resource",
            kind: "link",
            uri: block.uri,
            name: block.name,
            mimeType: block.mime_type ?? null,
          })
        }
        break
      }
      case "resource": {
        attachments.push({
          id: makeId(),
          type: "resource",
          kind: "embedded",
          uri: block.uri,
          name: fileBaseName(block.uri) || block.uri,
          mimeType: block.mime_type ?? null,
          text: block.text ?? null,
          blob: block.blob ?? null,
        })
        break
      }
      case "image": {
        attachments.push({
          id: makeId(),
          type: "image",
          data: block.data,
          uri: block.uri ?? null,
          name: imageName(block),
          mimeType: block.mime_type,
        })
        break
      }
    }
  }

  return { segments, attachments }
}

// The reference uri grammar (file:/codeg: → ReferenceAttrs) now lives in
// ./reference-uri, shared with transcript badge rendering. Re-exported here
// under its historical name so existing importers (tests, queue-edit restore)
// keep working.
export { parseReferenceUri }

/** Best-effort basename of a `file://` (or any path-shaped) uri. */
function fileBaseName(uri: string): string {
  const path = uri.replace(/^[a-z]+:\/+/i, "")
  const last = path.split("/").filter(Boolean).pop() ?? ""
  try {
    return decodeURIComponent(last)
  } catch {
    return last
  }
}

/** Derive a display name for an image block (mirrors the transcript adapter). */
function imageName(
  block: Extract<PromptInputBlock, { type: "image" }>
): string {
  if (block.uri && block.uri.trim().length > 0) {
    const base = fileBaseName(block.uri)
    if (base) return base
  }
  const ext = block.mime_type.split("/")[1]?.split("+")[0] ?? "image"
  return `image.${ext}`
}
