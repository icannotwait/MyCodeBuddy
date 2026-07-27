# Assistant Relative Path Autolinking Design

## Status

Approved in conversation (2026-07-27). Extends the absolute-only local path
autolinker from `docs/superpowers/specs/2026-07-16-assistant-local-path-autolink-design.md`.

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
2. Align `isLocalPathLike` in link-safety and resource-kind so badges and
   clicks treat the same relative shapes as files.
3. Keep activation, presentation, and open path unchanged.

## Goals

- Autolink bare relative, `./…`, and `../…` paths in completed top-level
  assistant prose when the final path segment has an allowed extension (or a
  special basename).
- Render them as the existing inline file badge.
- Open via `useOpenLinkOrFile` → `openFilePreview` against the active folder.
- Preserve transcript text; no live-stream scanning.
- Keep absolute-path behavior unchanged.

## Non-Goals

- Filesystem existence checks during render or scan.
- Streaming, user, system, tool, reasoning, plan, or collaboration Markdown.
- Unscoped bare filenames without a directory separator (e.g. only `README.md`).
- Home-relative (`~/…`) or UNC paths (still out of scope).
- Changing badge visuals or persistence format.
- Making relative paths open without an active folder (existing toast stands).

## Product choices (locked)

| Choice | Value |
|--------|--------|
| Detection | Shape heuristic only (no existence check) |
| Path forms | Bare relative + `./` + `../` |
| Activation | Same as absolute: completed top-level assistant only |
| Extensions | Wide set: code, config, docs, and media/archives |

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

Own detection and safe href construction for all local path families.

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

// Prefer a unified name; keep findAbsoluteLocalPathRanges as a thin alias
// or migrate call sites in the same change.
findLocalPathRanges(text: string): LocalPathMatch[]
toSafeLocalPathHref(match: LocalPathMatch): string | null

// Shared classification for link-safety + resource-kind (export).
isRelativeWorkspacePathLike(path: string): boolean
// or a single isLocalPathLike used by both consumers
```

Absolute rules remain as today. Relative rules are additive (below).

### `src/components/ai-elements/remark-autolink-local-paths.ts`

Unchanged structure: visit eligible mdast `text` nodes, call the pure scanner,
replace matches with link nodes. No relative-specific logic beyond the scanner
return values.

### `src/components/ai-elements/link-safety.tsx`

`isLocalPathLike` / `parseLocalFileTarget` must accept:

- existing shapes (`/…`, `\\…`, `./…`, `../…`, `~/…`, Windows drive), and
- **bare relative** paths that pass the relative shape + extension/basename
  check (same helper as the scanner / resource-kind).

Opening behavior for relative paths already strips a leading `./` and requires
an active folder when the path is not self-locating.

### `src/lib/resource-kind.ts`

Mirror the same `isLocalPathLike` helper so `docs/a.md` and `./src/a.ts`
classify as `"file"` (icon + badge path), not `null`.

**Drift prevention:** extract one pure helper (from `local-path-links` or a
tiny shared module both import) so remark, click parsing, and icons cannot
diverge.

### Activation (`message.tsx` / history adapters)

No change. Still only when `autolinkLocalPaths` is true on completed top-level
assistant text parts.

## Relative detection rules

A candidate is **relative** only if it fails absolute classification and then
passes all of the following.

### Forms

1. **Bare relative:** at least one `/` or `\` separator; must **not** start
   with `/`, `//`, `\\`, or a Windows drive prefix.
   - Example: `docs/superpowers/plans/foo.md`
2. **Explicit relative:** starts with `./` or `../` (including `../../x.ts`).
   - Example: `./src/app.ts`, `../plans/x.md`

Bare single-segment names with an extension (`README.md` alone) are **not**
linked.

### Extension / basename gate

The final path segment (after stripping location suffixes) must either:

- have a file extension in the whitelist (case-insensitive; the part after the
  last `.` in the basename), or
- equal one of the special basenames (case rules below).

This gate applies to **relative** candidates only. Absolute paths keep the
existing confidence rules without requiring an extension.

### Whitelist — code / config / docs

`ts`, `tsx`, `js`, `jsx`, `mjs`, `cjs`, `json`, `jsonc`, `md`, `mdx`, `txt`,
`rs`, `go`, `py`, `java`, `kt`, `cs`, `cpp`, `cc`, `c`, `h`, `hpp`, `css`,
`scss`, `less`, `html`, `htm`, `xml`, `yml`, `yaml`, `toml`, `ini`, `sh`,
`bash`, `zsh`, `ps1`, `bat`, `cmd`, `sql`, `graphql`, `gql`, `proto`, `vue`,
`svelte`, `astro`, `swift`, `rb`, `php`, `r`, `lua`, `zig`, `dart`, `gradle`,
`properties`, `env`, `lock`, `cmake`, `wasm`, `map`, `ipynb`

### Whitelist — media / archives

`pdf`, `png`, `jpg`, `jpeg`, `svg`, `webp`, `gif`, `ico`, `bmp`, `avif`,
`mp4`, `webm`, `mov`, `mp3`, `wav`, `ogg`, `flac`, `zip`, `tar`, `gz`, `tgz`,
`7z`, `rar`

### Special basenames

Match the last path segment (optionally case-insensitive for well-known
tooling names):

`Dockerfile`, `Makefile`, `CMakeLists.txt`, `.gitignore`, `.env`, `.env.local`,
`.editorconfig`, `.npmrc`, `.eslintrc`, `.prettierrc`

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
- Do not treat `@/…` composer-style tokens as relative files.
- Do not treat `//host/…` or scheme-bearing URLs as local paths.

### Classification priority

When scanning, classify a match as absolute (windows-drive / posix) first;
only remaining candidates may become `relative`. A single range is never both.

## Safe href construction

| Kind | Href shape |
|------|------------|
| `windows-drive` | Existing: `/{drive}:/…` with encoded segments (harden-safe) |
| `posix` | Existing: encoded absolute path |
| `relative` | **No** leading `/`. Encode each segment with `encodeURIComponent`; join with `/`; preserve `..` and `.` segments from `../` / `./` forms; append `locationSuffix` raw |

Examples:

```text
docs/a.md          → docs/a.md
./src/app.ts:12    → ./src/app.ts:12
../plans/x.md#L3   → ../plans/x.md#L3
docs/My File.md    → docs/My%20File.md  (when matched inside quotes)
```

Relative hrefs must not be rewritten into root-relative form, or
`parseLocalFileTarget` would treat them as POSIX absolute paths.

## Click and open semantics

1. `classifyResourceKind` → `"file"` for allowed relative shapes.
2. `parseLocalFileTarget` returns `{ path, line }` with slash-normalized path.
3. If path is not self-locating (absolute / `~/` / UNC) and there is no active
   folder → existing `errorNoWorkspace` toast.
4. Otherwise `openFilePreview(path.replace(/^\.\/+/, ""), { line })`.
5. Missing files fail at open time with the existing open-error toast.

## Testing

### Unit — `local-path-links`

Positive:

- `docs/superpowers/plans/2026-07-27-empty-folder-workspace-visibility.md`
- `./src/lib/app.ts`, `../plans/x.md`
- Quoted relative with spaces
- Location suffixes on relative paths
- Special basenames under a directory (`.github/workflows/ci.yml`,
  `deploy/Dockerfile`)
- Media extensions (`assets/logo.png`, `docs/spec.pdf`)

Negative:

- `src/app` (no extension)
- `README.md` (no separator)
- `www.example.com/docs/a.md`
- `进度/耗时/工具统计`
- `@/repo/src/app.ts`
- Absolute regressions still pass; previous “rejects `src/app.ts`” fixtures
  that should now match must be updated intentionally

### Unit — `resource-kind` / `link-safety`

- Bare `docs/a.md` and `./src/a.ts` → file open path
- Bare relative without allowed extension → not local file
- Absolute and external URL cases unchanged

### Integration — `message-local-path-autolink`

- With `autolinkLocalPaths`, prose containing a relative path renders a file
  badge; click calls `openFilePreview` with the relative path (and line when
  present).
- Default (flag off) leaves relative paths as text.
- Inline code still not autolinked.

## Risks and mitigations

| Risk | Mitigation |
|------|------------|
| False-positive prose | Extension gate + separator requirement + hostname first-segment rule |
| Badge for missing files | Accepted (no FS check); click surfaces existing error |
| `isLocalPathLike` drift | One shared pure helper |
| Relative href mistaken for absolute | Never prefix relative hrefs with `/` |
| Performance | Same single-pass tokenizer discipline as absolute scanner |

## Alternatives considered

### Independent relative scanner module + second remark pass

Clearer file split, but duplicates quote/boundary/suffix logic. Rejected for
v1 in favor of extending the existing scanner.

### Existence-checked linking

More accurate badges, but requires async/cache and complicates completed
message rendering. Explicitly out of scope (product choice A).

### Only classify explicit Markdown links as relative files

Does not address bare prose paths. Rejected.

## Implementation notes for the plan

1. Add relative classification + whitelist constants + tests first (TDD).
2. Export shared `isLocalPathLike` (or equivalent) and switch
   `link-safety` / `resource-kind` to it.
3. Wire scanner rename/alias; keep remark plugin call site simple.
4. Extend message integration tests.
5. Update this design’s parent absolute-autolink non-goals in a one-line
   cross-reference only if needed; do not rewrite the older doc’s history.

## Success criteria

- The example path
  `docs/superpowers/plans/2026-07-27-empty-folder-workspace-visibility.md`
  becomes a clickable file badge in completed assistant prose when
  `autolinkLocalPaths` is enabled and an active folder is set for open.
- Absolute path autolinking and Windows `file://` harden behavior remain green
  in existing tests.
- No disk I/O is introduced in the Markdown render path.
