import type { AdaptedContentPart } from "@/lib/adapters/ai-elements-adapter"

const INTERRUPTION_TEXT = "Conversation interrupted"
const EMPHASIS_MARKERS = ["**", "__", "*", "_"] as const

export function isConversationInterruptedAgentText(text: string): boolean {
  const trimmed = text.trim()
  if (trimmed === INTERRUPTION_TEXT) return true
  for (const marker of EMPHASIS_MARKERS) {
    if (
      trimmed.startsWith(marker) &&
      trimmed.endsWith(marker) &&
      trimmed.length > marker.length * 2 &&
      trimmed.slice(marker.length, -marker.length) === INTERRUPTION_TEXT
    ) {
      return true
    }
  }
  return false
}

export function filterDelegatedInterruptParts(
  parts: AdaptedContentPart[],
  isDelegatedChild: boolean
): AdaptedContentPart[] {
  if (!isDelegatedChild) return parts

  let changed = false
  const filtered: AdaptedContentPart[] = []
  for (const part of parts) {
    if (part.type === "text" && isConversationInterruptedAgentText(part.text)) {
      changed = true
      continue
    }
    if (part.type === "goal-run") {
      const items = filterDelegatedInterruptParts(part.items, true)
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
