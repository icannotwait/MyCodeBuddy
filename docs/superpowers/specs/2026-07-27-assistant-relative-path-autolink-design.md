# Assistant Relative Path Autolinking Design

## Status

Approved in conversation (2026-07-27). Design-review r2 locks (scanner
starts, dual predicates, fixtures, basename case, separators, traversal
policy) applied after parallel Grok + Codex design review. Extends the
absolute-only local path autolinker from
`docs/superpowers/specs/2026-07-16-assistant-local-path-autolink-design.md`.

## Problem

Assistant prose often cites workspace-relative files without absolute paths:

```text
See docs/superpowers/plans/2026-07-27-empty-folder-workspace-visibility.md.
Also check ./src/lib/markdown/local-path-links.ts:12.
```

The existing autolinker only recognizes Windows-drive and POSIX absolute paths.
Those relative strings stay plain text even though click routing already knows
how to open folder-relative paths via `openFilePreview` against the active
workspace folder.

Explicit relative Markdown links (`[x](./src/a.ts)`) partially work when the
href starts with `./` or `../`, but bare relative hrefs such as `docs/a.md`
are not classified as local files (`isLocalPathLike` / `classifyResourceKind`
treat them as untagged), and bare prose is never turned into links at all.

## Decision

Extend the existing opt-in completed-assistant remark pipeline (方案 1):

1. Teach the pure path scanner to recognize **relative** candidates with a
   **code / document / media extension whitelist** (no filesystem existence
   checks at render time).
2. Split **openability** vs **autolink confidence** into shared pure helpers
   (same module, two contracts) so explicit Markdown links keep today's
   extensionless `./` / `../` behavior while prose autolink stays gated.
3. Keep activation, presentation, and open path unchanged (including existing
   outside-workspace open policy for `../` joins).

## Goals

- Autolink bare relative, `./…` / `.\…`, and `../…` / `..\…` paths in completed
  top-level assistant prose when the final path segment has an allowed
  extension (or a special basename).
- Render them as the existing inline file badge.
- Open via `useOpenLinkOrFile` → `openFilePreview` against the active folder.
- Preserve transcript text; no live-stream scanning.
- Keep absolute-path behavior unchanged.
- Make bare relative **explicit Markdown** hrefs (`[x](docs/a.md)`) open as
  files when they pass the bare-relative openability gate.

## Non-Goals

- Filesystem existence checks during render or scan.
- Streaming, user, system, tool, reasoning, plan, or collaboration Markdown.
- Unscoped bare filenames without a directory separator (e.g. only `README.md`).
- Home-relative (`~/…`) or UNC paths (still out of scope for *new* relative
  rules; existing absolute/UNC/`~/` open behavior is preserved).
- Changing badge visuals or persistence format.
- Making relative paths open without an active folder (existing toast stands).
- Requiring active-folder containment for `../` (see Parent traversal).

## Product choices (locked)

| Choice | Value |
|--------|--------|
| Detection | Shape heuristic only (no existence check) |
| Path forms | Bare relative + `./` / `.\` + `../` / `..\` (mixed separators OK) |
| Activation | Same as absolute: completed top-level assistant only |
| Extensions | Wide set: code, config, docs, media/archives + Office previews |
| Special basename case | Case-insensitive ASCII fold for the full special set |
| Parent traversal | Allowed (reuse existing open policy; no new containment) |
| Helper model | Two predicates: openability vs autolink confidence |

## Architecture

```text
persisted assistant top-level text part
  → MessageResponse(autolinkLocalPaths=true)
  → remarkAutolinkLocalPaths
  → findLocalPathRanges (absolute + relative)
  → toSafeLocalPathHref
  → remarkRewriteFileUriLinks
  → remark-rehype → sanitize / harden
  → MarkdownLink (file badge)
  → useOpenLinkOrFile / parseLocalFileTarget
  → openFilePreview(relativePath)
```

### `src/lib/markdown/local-path-links.ts`

Own detection, safe href construction, and **shared pure classification** for
all local path families.

```ts
type LocalPathKind = "windows-drive" | "posix" | "relative"

interface LocalPathMatch {
  start: number
  end: number
  label: string
  path: string
  locationSuffix: string | null
  kind: LocalPathKind
}

// Required naming: export findLocalPathRanges as the scanner entry.
// Keep findAbsoluteLocalPathRanges as a deprecated thin alias in the same PR
// (same implementation) so call sites can migrate without dual logic.
findLocalPathRanges(text: string): LocalPathMatch[]
toSafeLocalPathHref(match: LocalPathMatch): string | null

// --- Shared pure helpers (single module; no second source of truth) ---

/** Openability: can this string be treated as a local file target for click/icon? */
isLocalPathLike(path: string): boolean

/**
 * Bare-relative openability (used by isLocalPathLike).
 * Requires separator + not absolute + extension/special basename +
 * hostname-first-segment + $ reject + ASCII alnum + not @/-prefixed.
 * Does NOT apply to paths that already start with ./ or ../ (those stay
 * openable without extension, matching today).
 */
isBareRelativeWorkspacePathLike(path: string): boolean

/**
 * Autolink confidence for prose scanner (kind === "relative").
 * Applies extension/special basename to ALL relative forms (bare and
 * explicit ./ ../). Scanner guards (hostname, @/, scheme, whole-token
 * reject) also apply here.
 */
passesRelativeAutolinkGate(path: string): boolean
```

#### Dual-predicate contract (locked)

| Consumer | Predicate | Extension gate? |
|----------|-----------|-----------------|
| Prose scanner (`findLocalPathRanges` → relative match) | absolute classify first, else relative form + `passesRelativeAutolinkGate` | **Yes** (bare and `./` / `../`) |
| Explicit MD link click/icon (`link-safety` / `resource-kind`) | `isLocalPathLike` | Bare relative: **yes**. Explicit `./` / `../` / abs / `~/` / UNC: **no** (preserve today) |

Compatibility lock:

- Explicit `[app](./src/app)` (extensionless) **remains** a file open target.
- Prose `./src/app` (extensionless) is **not** autolinked.
- Explicit `[x](docs/a.md)` and prose `docs/a.md` both become files when the
  bare-relative openability / autolink gates pass.

Absolute rules remain as today. Relative rules are additive (below).

### `src/components/ai-elements/remark-autolink-local-paths.ts`

Unchanged structure: visit eligible mdast `text` nodes, call
`findLocalPathRanges` (or the alias), replace matches with link nodes. No
relative-specific policy beyond the scanner return values.

### `src/components/ai-elements/link-safety.tsx`

Import shared `isLocalPathLike` from `local-path-links` (or a tiny re-export
module that only re-exports those pure helpers). Delete the local duplicate.

`parseLocalFileTarget` continues to:

- reject scheme-bearing URLs (existing),
- accept absolute / UNC / `~/` / explicit `./` / `../` without extension,
- accept **bare relative** only via `isBareRelativeWorkspacePathLike`.

Opening behavior for relative paths already strips a leading `./` and requires
an active folder when the path is not self-locating.

### `src/lib/resource-kind.ts`

Import the same shared `isLocalPathLike` so `docs/a.md` and `./src/a.ts`
classify as `"file"` (icon + badge path), not `null`. Explicit extensionless
`./src/app` remains `"file"`. Bare `src/app` (no extension) remains `null`.

### Activation (`message.tsx` / history adapters)

No change. Still only when `autolinkLocalPaths` is true on completed top-level
assistant text parts.

## Relative detection rules

A candidate is **relative** only if it fails absolute classification and then
passes all of the following.

### Candidate start (scanner rewrite — locked)

Extend the existing single-pass tokenizer. Prefer absolute starts first
(unchanged: Windows drive `X:/` / `X:\`, POSIX `/` not followed by `/`).

When `hasStartBoundary(text, index)` is true, also allow a **relative start** at
`index` when any of (evaluated in this order):

1. **Explicit relative prefix:** `.` followed by `/` or `\` (`./`, `.\`), or
   `..` followed by `/` or `\` (`../`, `..\`), including longer chains scanned
   as one token (`../../x.ts`).
2. **Dot-prefixed bare directory/file segment:** `.` followed by a **non-dot,
   non-separator** segment character (e.g. `.github/…`, `.env.local` under a
   directory only after a separator appears in the full token). This is **not**
   explicit-relative (next char is not `/` or `\`). After token scan + trim,
   the candidate must still pass bare-relative form rules (at least one
   separator) and `passesRelativeAutolinkGate`. This admits
   `.github/workflows/ci.yml` at start-of-token; unscoped `.env` alone still
   fails the separator rule and is not autolinked.
3. **Bare relative letter/digit start:** ASCII letter or digit (not `@`, not
   `$`). After unquoted/quoted token scan + trim, the candidate must pass
   relative classification (separator + gates). If classification fails,
   advance with `max(scannedEnd, index + 1)` (same amortized-linear discipline
   as absolute).

If classification fails for a started token, advance with
`max(scannedEnd, index + 1)` (do not rescan interior segments of a rejected
token as a different path).

Do **not** start a candidate at `@` (composer alias / scoped package). A
candidate whose path begins with `@/` or `@scope/` is never relative.

Scheme-bearing tokens (`https://…`, `http://…`, other `scheme:`) and
protocol-relative `//host/…` are **consumed/rejected as whole tokens** — do
not rescan an interior segment such as `docs/a.md` inside
`https://example.com/docs/a.md` or `//cdn.example.com/docs/a.md`.

### Forms

1. **Bare relative:** at least one `/` or `\` separator; must **not** start
   with `/`, `//`, `\\`, or a Windows drive prefix.
   - Example: `docs/superpowers/plans/foo.md`, `src\lib\app.ts`
2. **Explicit relative:** starts with `./`, `.\`, `../`, or `..\` (including
   `../../x.ts` and mixed separators such as `.\src\app.ts`).
   - Example: `./src/app.ts`, `../plans/x.md`, `..\plans\x.md`

Bare single-segment names with an extension (`README.md` alone) are **not**
linked (no directory separator).

Empty or duplicate-only separators after normalization (`docs//a.md` → empty
segment) are **rejected** (fail-closed).

### Extension / basename gate

The final path segment (after stripping location suffixes and normalizing
separators for inspection) must either:

- have a file extension in the whitelist (case-insensitive; the part after the
  last `.` in the basename), or
- equal one of the special basenames (case rules below).

This gate applies to:

- **Autolink:** all relative candidates (bare and explicit).
- **Openability bare relative:** bare forms only.

Absolute paths keep the existing confidence rules without requiring an
extension. Explicit `./` / `../` openability does **not** require an extension.

Special basenames are matched as whole basenames and are **not** treated as
“extension = last dotted part” (e.g. `.env.local` is special, not ext `local`).

### Whitelist — code / config / docs

`ts`, `tsx`, `js`, `jsx`, `mjs`, `cjs`, `json`, `jsonc`, `md`, `mdx`, `txt`,
`rs`, `go`, `py`, `java`, `kt`, `cs`, `cpp`, `cc`, `c`, `h`, `hpp`, `css`,
`scss`, `less`, `html`, `htm`, `xml`, `yml`, `yaml`, `toml`, `ini`, `sh`,
`bash`, `zsh`, `ps1`, `bat`, `cmd`, `sql`, `graphql`, `gql`, `proto`, `vue`,
`svelte`, `astro`, `swift`, `rb`, `php`, `r`, `lua`, `zig`, `dart`, `gradle`,
`properties`, `env`, `lock`, `cmake`, `wasm`, `map`, `ipynb`, `csv`

### Whitelist — media / archives / office

`pdf`, `png`, `jpg`, `jpeg`, `svg`, `webp`, `gif`, `ico`, `bmp`, `avif`,
`mp4`, `webm`, `mov`, `mp3`, `wav`, `ogg`, `flac`, `zip`, `tar`, `gz`, `tgz`,
`7z`, `rar`, `docx`, `xlsx`, `pptx`

Office extensions are included because `openFilePreview` already has a
dedicated in-app Office preview path. Not every media/archive type has a rich
preview; missing or unsupported types fail at open time with the existing
toast (accepted).

### Special basenames

Match the last path segment with **case-insensitive ASCII fold** for the
entire special set (shape-only; FS case sensitivity is irrelevant at render):

`Dockerfile`, `Makefile`, `CMakeLists.txt`, `.gitignore`, `.env`, `.env.local`,
`.editorconfig`, `.npmrc`, `.eslintrc`, `.prettierrc`

Positive examples: `deploy/Dockerfile`, `deploy/dockerfile`, `build/Makefile`.
Negative: `deploy/Dockerfiles` (not an exact basename match after fold).

### Shared guards (with absolute scanner)

- Deterministic single-pass scan; no superlinear regex over the full text.
- Require at least one ASCII alphanumeric in the path body (reject pure CJK
  slash labels such as `进度/耗时/工具统计`).
- Reject paths containing `$` (math-delimiter fail-closed).
- Location suffixes on the label and href: `:12`, `:12:8`, `#L12`,
  `#L12-L20`, `#L12-20`. Click opens at the first line only.
- Quoted paths may contain spaces; quotes stay outside the link node.
- Skip subtrees: link, linkReference, inlineCode, code, html, image,
  imageReference, definition.
- Skip text nodes whose source slice no longer matches `node.value`
  (CommonMark escape consumption).

### Relative-only false-positive guards

- **Hostname-like first segment (bare relative only):** if the first segment
  contains `.` and does **not** start with `.` (dotfile/dir), reject.
  - Reject: `www.example.com/docs/a.md`
  - Allow: `.github/workflows/ci.yml`, `docs/a.md`
- A candidate whose path begins with `@/` or a scoped `@name/` token is never
  relative (composer path alias / npm scope). Reject the whole token; do not
  autolink an interior path segment.
- Do not treat `//host/…` or scheme-bearing URLs as local paths; whole-token
  reject as above.

### Accepted false negatives (not bugs)

- Dotted first-segment directories rejected by the hostname rule, e.g.
  `v2.0/CHANGELOG.md`, `packages.v2/src/a.ts`, `jquery-3.7.1/dist/x.js`.
- Short-extension residual FPs in prose (`option/a.h`, `see x/y.go`) — accepted
  for v1; separator + whitelist only, no NLP.
- Unscoped single-segment basenames (`README.md`, `Dockerfile` alone).
- Prose paths whose first segment does not begin with `.`, ASCII letter, or
  digit (e.g. `_generated/a.ts`, `-generated/a.ts`, pure non-ASCII first
  segment) — start-rule limitation; explicit Markdown links unaffected.
- Positive start-at-zero fixture **required**: `.github/workflows/ci.yml`
  (dot-prefixed bare directory form).

### Classification priority

When scanning, classify a match as absolute (windows-drive / posix) first;
only remaining candidates may become `relative`. A single range is never both.

## Safe href construction

| Kind | Href shape |
|------|------------|
| `windows-drive` | Existing: `/{drive}:/…` with encoded segments (harden-safe) |
| `posix` | Existing: encoded absolute path |
| `relative` | **No** leading `/`. Normalize `\` → `/` first; split on `/`; drop empty segments by **rejecting** the match if any empty segment remains after normalization (no `docs//a.md`); encode each segment with `encodeURIComponent`; join with `/`; preserve `..` and `.` segments from `../` / `./` forms; append `locationSuffix` raw |

Examples:

```text
docs/a.md          → docs/a.md
src\lib\app.ts     → src/lib/app.ts
./src/app.ts:12    → ./src/app.ts:12
.\src\app.ts       → ./src/app.ts
../plans/x.md#L3   → ../plans/x.md#L3
..\plans\x.md      → ../plans/x.md
docs/My File.md    → docs/My%20File.md  (when matched inside quotes)
```

Relative hrefs must not be rewritten into root-relative form, or
`parseLocalFileTarget` would treat them as POSIX absolute paths.

## Click and open semantics

1. `classifyResourceKind` → `"file"` via shared `isLocalPathLike`.
2. `parseLocalFileTarget` returns `{ path, line }` with slash-normalized path.
3. If path is not self-locating (absolute / `~/` / UNC) and there is no active
   folder → existing `errorNoWorkspace` toast; **neither** `openFilePreview`
   nor a browser opener runs.
4. Otherwise `openFilePreview(path.replace(/^\.\/+/, ""), { line })`.
5. Missing files fail at open time with the existing open-error toast.

### Parent traversal (locked)

`openFilePreview` joins the relative target to the active folder and normalizes
dot segments **without** enforcing containment inside the folder root. Paths
such as `../secrets.txt` may resolve outside the active workspace. This
**intentionally reuses** the existing user-initiated outside-workspace open
policy (absolute paths and user-authored Markdown links already can). Relative
autolinking does **not** add a new containment check or a new error class.

Threat-model note: autolinking increases the visibility of `../` in assistant
prose, but open remains user-initiated (click) and uses the same join/normalize
path as today.

## Testing

### Unit — `local-path-links`

Positive:

- `docs/superpowers/plans/2026-07-27-empty-folder-workspace-visibility.md`
- `src/app.ts`, `src/main.rs` (intentional flip from previous reject fixtures)
- `./src/lib/app.ts`, `../plans/x.md`
- `.\src\a.ts`, `..\plans\x.md`, `src\lib\app.ts` → normalized hrefs
- Quoted relative with spaces
- Location suffixes on relative paths (`:12`, `#L12`)
- Special basenames under a directory (`deploy/Dockerfile`,
  `deploy/dockerfile`)
- Dot-prefixed bare directory start-at-zero: `.github/workflows/ci.yml`
  (must match from index 0; not only as an interior token)
- Media / Office extensions (`assets/logo.png`, `docs/spec.pdf`,
  `reports/status.docx`)

Negative:

- `src/app` (no extension) — prose not autolinked
- `README.md` (no separator)
- `www.example.com/docs/a.md`
- `进度/耗时/工具统计`
- `@/repo/src/app.ts`, `@scope/pkg/src/a.ts`
- `https://example.com/docs/a.md`, `//cdn.example.com/docs/a.md` (whole-token;
  no interior autolink)
- `docs//a.md` (empty segment)
- `$`-containing paths
- Absolute regressions still pass

### Unit — `resource-kind` / `link-safety` / `markdown-link`

Fixture migration (intentional flips):

- `src/main.rs` / `src/app.ts` → file icon + local open path
- Bare `docs/a.md` → file
- `./src/a.ts` and `./src/a.ts:12` → file open with line
- Extensionless `./src/app` → **still** file (compatibility)
- Bare relative without allowed extension (`src/app`, `docs/folder`) → not file
- Absolute and external URL cases unchanged

Required new cases:

- Gated bare relative with **no active folder** → `errorNoWorkspace`; no
  `openFilePreview`, no browser open
- `toSafeLocalPathHref` relative never gains a leading `/`
- Parent-traversal lock: `../outside.txt` (with active folder) still attempts
  open via existing join semantics (no new containment error)

### Integration — `message-local-path-autolink` / real `MessageResponse`

With `autolinkLocalPaths` and active folder:

- Prose `docs/a.md`, `./src/a.ts`, `../plans/x.md` each render a file badge
- Click asserts exact relative path (and line when present) passed to
  `openFilePreview` (no accidental leading `/`; preserve `../` prefix where
  present after `./` strip rules)
- Quoted spaces + line suffix round-trip through sanitize/harden
- Explicit Markdown `[x](docs/a.md)` → file badge + open
- Compatibility: `[x](./src/app)` extensionless remains file badge
- Default (flag off) leaves relative prose as text
- Inline code still not autolinked

## Risks and mitigations

| Risk | Mitigation |
|------|------------|
| False-positive prose | Extension gate + separator + hostname first-segment + whole-token URL/alias reject |
| Badge for missing files | Accepted (no FS check); click surfaces existing error |
| Openability vs autolink drift | Two predicates, one module; table locks consumers |
| Relative href mistaken for absolute | Never prefix relative hrefs with `/`; normalize `\` → `/` only |
| Performance | Boundary-gated starts; same single-pass discipline; keep 2k-match stress with mixed abs+rel |
| `../` outside folder | Explicit reuse of existing open policy; documented threat model |
| Dotted first-segment dirs | Accepted false negative (hostname rule) |
| Short-extension FPs | Accepted residual for v1 |

## Alternatives considered

### Independent relative scanner module + second remark pass

Clearer file split, but duplicates quote/boundary/suffix logic. Rejected for
v1 in favor of extending the existing scanner.

### Existence-checked linking

More accurate badges, but requires async/cache and complicates completed
message rendering. Explicitly out of scope (product choice A).

### Only classify explicit Markdown links as relative files

Does not address bare prose paths. Rejected.

### Single boolean helper for both scan and click

Would either autolink extensionless `./src/app` prose or break explicit
`[app](./src/app)` openability. Rejected in favor of dual predicates.

## Implementation notes for the plan

1. Add relative classification + whitelist constants + dual predicates + tests
   first (TDD).
2. Export shared helpers; switch `link-safety` / `resource-kind` to
   `isLocalPathLike` from the shared module.
3. Extend scanner start rules + whole-token reject; export
   `findLocalPathRanges` with deprecated `findAbsoluteLocalPathRanges` alias.
4. Expand unit fixtures (scan, resource-kind, markdown-link, link-safety) and
   message integration tests (including no-workspace and explicit MD bare
   relative).
5. Update this design’s parent absolute-autolink non-goals in a one-line
   cross-reference only if needed; do not rewrite the older doc’s history.

## Success criteria

- The example path
  `docs/superpowers/plans/2026-07-27-empty-folder-workspace-visibility.md`
  becomes a clickable file badge in completed assistant prose when
  `autolinkLocalPaths` is enabled and an active folder is set for open.
- Bare `docs/a.md` explicit Markdown links classify as files.
- Extensionless `./src/app` explicit links still open as files; extensionless
  prose `./src/app` is not autolinked.
- Absolute path autolinking and Windows `file://` harden behavior remain green
  in existing tests.
- No disk I/O is introduced in the Markdown render path.
- Relative safe hrefs never gain a leading `/`; `\` is normalized to `/`.
