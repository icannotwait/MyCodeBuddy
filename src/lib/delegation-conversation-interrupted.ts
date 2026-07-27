/**
 * Detect Codex `*Conversation interrupted*` / `_Conversation interrupted_`
 * agent assistant text for delegated-child presentation suppress (Task 3).
 *
 * Full-string only after trim. Single emphasis wrapper only — bare, bold
 * multi-marker, multi-paragraph, and partial matches are rejected.
 */

const STAR = "*Conversation interrupted*"
const UNDERSCORE = "_Conversation interrupted_"

export function isConversationInterruptedAgentText(text: string): boolean {
  const normalized = text.trim()
  return normalized === STAR || normalized === UNDERSCORE
}
