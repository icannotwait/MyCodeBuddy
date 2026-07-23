/**
 * Pure predicate: bare Escape (no modifiers) should close the active
 * conversation / file tab. Fixed binding — not rebindable via shortcut
 * settings (those keep close_current_tab as mod+w by default).
 *
 * `defaultPrevented` normally means a closer UI already handled Escape
 * (dialog, Monaco suggest, etc.). Exception: ProseMirror's captureKeyDown
 * always preventDefault()s Escape (keyCode 27) even when no editor command
 * ran, which would otherwise block tab-close while the composer is focused.
 * Real composer consumers (slash menu, @-mention, queue-edit cancel) must
 * stopPropagation so this handler never sees the event.
 */
export function shouldCloseTabOnEscape(event: {
  key: string
  defaultPrevented: boolean
  metaKey: boolean
  ctrlKey: boolean
  altKey: boolean
  shiftKey: boolean
  /** Used to recognize ProseMirror's always-on Escape preventDefault. */
  target?: EventTarget | null
}): boolean {
  if (event.metaKey || event.ctrlKey || event.altKey || event.shiftKey) {
    return false
  }
  if (event.key !== "Escape" && event.key !== "Esc") return false

  if (event.defaultPrevented) {
    const target = event.target
    const fromProseMirror =
      target instanceof Element && target.closest(".ProseMirror") != null
    if (!fromProseMirror) return false
  }

  return true
}
