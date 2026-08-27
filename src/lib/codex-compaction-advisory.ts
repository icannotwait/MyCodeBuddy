import type { AdaptedContentPart } from "@/lib/adapters/ai-elements-adapter"

const ADVISORY =
  "Heads up: Long threads and multiple compactions can cause the model to be less accurate. Start a new thread when possible to keep threads small and targeted."

export function isCodexCompactionAdvisoryText(text: string): boolean {
  const trimmed = text.trim()
  return trimmed === ADVISORY || trimmed === `Warning: ${ADVISORY}`
}

/**
 * Drop Codex's post-compaction "start a new thread" tip from display.
 * The compaction card already reports the outcome; this text is advisory noise.
 */
export function filterCodexCompactionAdvisoryParts(
  parts: AdaptedContentPart[]
): AdaptedContentPart[] {
  let changed = false
  const filtered: AdaptedContentPart[] = []
  for (const part of parts) {
    if (part.type === "text" && isCodexCompactionAdvisoryText(part.text)) {
      changed = true
      continue
    }
    if (part.type === "goal-run") {
      const items = filterCodexCompactionAdvisoryParts(part.items)
      if (items !== part.items) {
        changed = true
        filtered.push({ ...part, items })
        continue
      }
    }
    filtered.push(part)
  }
  return changed ? filtered : parts
}
