/**
 * Label for a session config select option in the UI.
 *
 * Cursor (and similar agents) put compound parameters in `value` — e.g.
 * `claude-opus-4-6[thinking=true,context=200k,effort=high,fast=false]` — while
 * `name` is only the short base label. Prefer the wire value whenever it
 * extends the name so those params stay visible (same policy as Multi-Agent
 * agent defaults). Keep a distinct human name when it is not a prefix of the
 * value (e.g. OpenCode "Big Pickle" vs `opencode/big-pickle`).
 */
export function configOptionDisplayLabel(item: {
  value: string
  name: string
}): string {
  const name = item.name.trim()
  const value = item.value.trim()
  if (!value) return name
  if (!name || name === value) return value
  if (value.startsWith(name) && value.length > name.length) return value
  return name
}
