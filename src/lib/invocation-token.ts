// Bare invocation tokens — `/slash` commands and `$skill`/`$expert` tokens — that
// user messages send literally (the agent CLI needs them verbatim, so there is no
// link to key a badge off). This regex finds them for badge *display*. It is
// intentionally a HEURISTIC: the token is indistinguishable from typed text.
//
// The slug starts with a letter (so `/123` / `$5` don't match) and the boundary
// before it must be start-of-text or whitespace. A trailing `/` (a path like
// `/usr/bin`) or word char disqualifies it. Shared by the user-message renderer
// (`user-message-segments.ts`) and the transcript rehype plugin
// (`ai-elements/rehype-command-badges.ts`) so both badge exactly the same tokens.
//
// Stateful (`g` flag): reset `lastIndex` before an `exec` loop, or use `matchAll`
// (which operates on a private copy). Capture groups: [1] = the leading
// boundary (start-of-text or the whitespace char), [2] = the token incl. prefix.
export const INVOCATION_TOKEN_RE =
  /(^|\s)([/$][A-Za-z][A-Za-z0-9_-]*)(?![/\w-])/g
