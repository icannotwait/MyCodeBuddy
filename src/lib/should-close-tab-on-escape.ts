/**
 * Pure predicate: bare Escape (no modifiers, not already handled) should
 * close the active conversation tab. Fixed binding — not rebindable via
 * shortcut settings (those keep close_current_tab as mod+w by default).
 */
export function shouldCloseTabOnEscape(event: {
  key: string
  defaultPrevented: boolean
  metaKey: boolean
  ctrlKey: boolean
  altKey: boolean
  shiftKey: boolean
}): boolean {
  if (event.defaultPrevented) return false
  if (event.metaKey || event.ctrlKey || event.altKey || event.shiftKey) {
    return false
  }
  return event.key === "Escape" || event.key === "Esc"
}
