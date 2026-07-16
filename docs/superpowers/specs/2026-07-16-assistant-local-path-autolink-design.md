# Assistant Local Path Autolinking Design

## Status

Approved in conversation. This document covers automatic local-file links in
completed assistant prose and the Windows `file://` compatibility fix required
for those links to survive Streamdown's sanitization pipeline.

## Problem

Assistant responses frequently mention local files as plain text:

```text
Updated D:\MyCodeBuddy\src\lib\session-files.ts.
See /Users/me/repo/src/app.ts#L12.
```

Streamdown parses those values as ordinary Markdown text, so they never reach
the existing `MarkdownLink -> useOpenLinkOrFile -> openFilePreview` path.

Turning the text into a Markdown link is not sufficient on Windows. Streamdown
runs `rehype-sanitize` and `rehype-harden` before the React link component:

- `D:\repo\a.ts` and `D:/repo/a.ts` are interpreted as URLs with a `d:`
  protocol and become `[blocked]`;
- `file:///D:/repo/a.ts` is deliberately blocked as a `file:` URL;
- `/D:/repo/a.ts` survives as a root-relative URL, and the existing click
  parser already converts it back to `D:/repo/a.ts`.

The feature therefore needs both prose detection and a harden-safe href
representation. It must not change persisted transcript text or add work to
the live token-rendering path.

## Decision

Use an opt-in remark AST transform on completed top-level assistant text parts.
The transform will recognize a conservative set of Windows-drive and POSIX
absolute paths, create normal mdast link nodes, and encode their destinations
as hrefs that survive the existing sanitize/harden pipeline.

No new URL protocol will be introduced. The existing file badge and click
handler remain the only presentation and navigation path.

## Goals

- Turn supported bare absolute paths in completed assistant prose into the
  existing inline file badge.
- Open a clicked badge in the right-side file panel through
  `openFilePreview`.
- Fix explicit Windows `file:///D:/...` Markdown links so they no longer render
  with `[blocked]`.
- Preserve the canonical assistant text used by persistence, search, and the
  message copy action.
- Keep the live streaming path free of path scanning and autolink rendering.
- Keep user, system, reasoning, tool, plan, permission, and collaboration
  Markdown unchanged.

## Non-Goals

- Home-relative (`~/...`), UNC, explicitly relative (`./...`, `../...`), or
  bare relative (`src/app.ts`) paths.
- Paths embedded in inline code, fenced code, or raw HTML.
- Filesystem existence checks during rendering.
- Autolinking tool output, reasoning, plans, system messages, or user text.
- Making `file://` images renderable.
- Changing the visual design of the existing inline file badge.
- Correcting non-openable attachment chips. That is a separate follow-up
  because it concerns user-resource semantics rather than assistant prose.

## Alternatives Considered

### Safe href normalization plus remark AST transform

Selected. It fits the existing Markdown pipeline, requires no new protocol,
and limits behavior to an explicit `MessageResponse` opt-in.

### A new `codeg-file://` protocol

This would encode every path family unambiguously, but it would require changes
to the sanitize allow-list, resource classification, link parsing, and click
routing. That additional security and compatibility surface is not justified
for the v1 path set.

### React-level text replacement

Replacing strings after Markdown rendering would avoid rehype hardening, but it
would couple the feature to Streamdown's React tree and complicate copying,
memoization, accessibility, and nested Markdown behavior.

## Architecture

The completed-message flow is:

```text
persisted assistant top-level text part
  -> MessageResponse(autolinkLocalPaths=true)
  -> remarkAutolinkLocalPaths
  -> remarkRewriteFileUriLinks
  -> remark-rehype
  -> rehype-sanitize / rehype-harden
  -> MarkdownLink
  -> inline file badge
  -> useOpenLinkOrFile
  -> openFilePreview
```

The implementation has these boundaries:

### `src/lib/markdown/local-path-links.ts`

A React-free module that owns path detection and safe href construction:

```ts
interface LocalPathMatch {
  start: number
  end: number
  label: string
  path: string
  locationSuffix: string | null
  kind: "windows-drive" | "posix"
}

findAbsoluteLocalPathRanges(text: string): LocalPathMatch[]
toSafeLocalPathHref(match: LocalPathMatch): string | null
```

`start` and `end` select the link label only. Matching outer quotes remain
outside the link node.

### `src/components/ai-elements/remark-autolink-local-paths.ts`

A remark transform that visits eligible mdast `text` nodes, calls the pure
scanner, and replaces each matched node with alternating text and link nodes.
It performs no path parsing of its own.

### `src/components/ai-elements/remark-file-uri-links.ts`

The existing transform continues to handle explicit Markdown links. For local
Windows-drive and POSIX `file://` destinations, it uses the same safe href
normalization rules as the autolinker. Images remain untouched. The existing
UNC behavior is outside this design and is not claimed as v1 support.

### `src/components/ai-elements/message.tsx`

`MessageResponse` gains an app-owned prop:

```ts
autolinkLocalPaths?: boolean
```

The default is `false`. The component selects between two module-stable remark
plugin arrays. The enabled array places `remarkAutolinkLocalPaths` before
`remarkRewriteFileUriLinks`.

The `MessageResponse` memo comparator must include the new prop.

### Completed assistant text activation

`ContentPartsRenderer` enables the prop only for a top-level `text` part whose
message role is `assistant`. Recursive rendering inside structured goal/tool
containers does not inherit the opt-in.

The direct history path passes the prop to `MessageResponse`. The completed
streaming-partition handoff passes it through `StreamingMarkdownDocument` and
`SealedBlock`. Their prop types and memo comparators must include the flag.

Live transcript components never set the flag, regardless of whether deferred
streaming rich content is enabled.

## Detection Rules

The scanner is a deterministic, single-pass tokenizer. It must not use a regex
whose runtime can grow superlinearly with input length.

### Supported forms

```text
D:\repo\src\app.ts
D:/repo/src/app.ts
/Users/me/repo/src/app.ts
"D:\My Project\src\app.ts"
'/Users/me/My Project/src/app.ts'
```

The following location suffixes are part of the link label and destination:

```text
:12
:12:8
#L12
#L12-L20
#L12-20
```

The existing file opener uses the first line in a line/column or line-range
suffix. Column and range-end values remain display information only.

### POSIX confidence rule

A POSIX candidate must start with exactly one `/` and satisfy at least one of:

- another `/` occurs after the root, as in `/etc/hosts`; or
- the sole root-level name has an extension, as in `/README.md`.

This deliberately leaves `/review` as text while retaining common absolute
file paths. A `//host/path` candidate is never treated as a local POSIX path.

### Boundaries

An unquoted candidate ends at whitespace or a Markdown delimiter. Common
sentence punctuation is removed from its tail. A closing bracket is retained
when it balances an opening bracket inside the candidate; otherwise it remains
outside the link.

A path may contain spaces only when enclosed by matching ASCII single or double
quotes. The quotes remain ordinary prose around the badge. Escaped or nested
quotes are not supported in v1.

The scanner rejects a candidate that begins inside an HTTP(S) URL, a
protocol-relative URL, a slash command, or another path token.

### Markdown exclusions

The remark transform only changes `text` nodes. It does not descend into an
existing `link` or `linkReference`. `inlineCode`, `code`, HTML, image, and
definition nodes are not candidates.

Replacement walks the original child list once and does not revisit newly
created link children. Running the transform twice is therefore idempotent.

## Safe Href Encoding

The displayed label and the href have different responsibilities:

| Assistant prose | Badge label | Safe href |
| --- | --- | --- |
| `D:\repo\src\a.ts` | `D:\repo\src\a.ts` | `/D:/repo/src/a.ts` |
| `D:/repo/src/a.ts` | `D:/repo/src/a.ts` | `/D:/repo/src/a.ts` |
| `/Users/me/repo/a.ts` | `/Users/me/repo/a.ts` | `/Users/me/repo/a.ts` |
| `"D:\My Project\a.ts"` | `D:\My Project\a.ts` | `/D:/My%20Project/a.ts` |

The quotes in the final table row are surrounding prose, not part of the href.

Safe href construction performs these steps:

1. Separate a recognized line/column/range suffix from the filesystem path.
2. Normalize Windows separators to `/`.
3. Prefix a Windows drive path with `/`.
4. Percent-encode path data, including spaces, Unicode, `%`, `#`, and `?`,
   while preserving path separators and the Windows drive colon.
5. Append the normalized location suffix.

Encoding happens exactly once and treats the assistant text as a filesystem
path, not as an already encoded URI. A literal `%20` in bare prose therefore
means a filename containing those three characters. Explicit `file://` links
continue to be parsed as URIs before normalization.

On click, the existing `parseLocalFileTarget` logic decodes the href and
`stripLeadingSlashOnWindows` converts `/D:/...` back to `D:/...`.

## Streaming and Rendering Lifecycle

No live component opts into the plugin. Sealed live blocks and the growing tail
therefore render exactly as they do today, and paths remain plain text for the
entire active turn.

When the turn completes, the runtime completes and caches any incremental
Markdown partition before the live projection is retired. The historical
`TextPart` then renders the canonical text with the opt-in enabled:

- a valid completed partition applies the transform to all sealed blocks;
- a missing or invalid partition applies it through the full
  `MessageResponse` fallback;
- a directly loaded historical conversation follows the same fallback or
  partition path.

This produces one intentional completion-time layout change from path text to
file badges. It adds no per-token scanning or link-component rendering.

## Data and Copy Semantics

The transform exists only in the Markdown rendering tree. It does not mutate:

- Rust parser output;
- `AdaptedContentPart.text`;
- live transcript state;
- database records;
- search input; or
- the message copy action's source text.

Manual DOM selection follows the current `ReferenceBadge` behavior. A long
badge may copy its visually middle-truncated label. This design does not change
that existing presentation behavior.

## Error and Security Handling

- Detection or encoding failure leaves the original text node unchanged.
- Malformed or unterminated quoted paths remain text.
- The renderer performs no filesystem reads and no asynchronous existence
  checks.
- Opening a missing or unreadable file uses the existing toast error path.
- No sanitize allow-list or harden protocol setting is widened.
- Every generated href is a root-relative path intercepted by the existing
  local-file click classifier.
- Opening remains a user-initiated action. The assistant cannot trigger file
  access merely by emitting a matching path.

## Explicit `file://` Compatibility

The existing file URI transform currently removes the slash before a Windows
drive letter, producing `D:/...`, which harden treats as a custom protocol.
The transform will instead retain the slash and produce `/D:/...`.

Examples:

```text
file:///D:/repo/a.ts       -> /D:/repo/a.ts
file:///D:/My%20Repo/a.ts -> /D:/My%20Repo/a.ts
file:///Users/me/a.ts     -> /Users/me/a.ts
```

Fragments and queries that encode path characters remain encoded. Recognized
line fragments such as `#L12` remain location metadata. `file://` image nodes
are still skipped so Streamdown's blocked-image placeholder remains intact.

## Testing

### Pure scanner and encoder tests

Cover:

- Windows backslash and slash paths;
- POSIX paths and the `/review` confidence boundary;
- single- and double-quoted paths with spaces;
- Unicode path segments;
- every supported location suffix;
- trailing ASCII and CJK punctuation;
- balanced and unbalanced closing brackets;
- literal `%`, `#`, and `?` path characters;
- adjacent text and multiple matches;
- HTTP(S), `//host`, slash command, `~/`, UNC, relative, and bare-relative
  negatives; and
- a large input regression that validates complete output without relying on a
  timing threshold.

### Remark AST tests

Verify:

- a text node becomes the expected `text/link/text` sequence;
- outer quotes remain text;
- visible text is unchanged;
- existing links and link references are not nested;
- inline code, fenced code, HTML, image, and definition nodes are unchanged;
  and
- applying the transform twice is idempotent.

### Real Streamdown pipeline tests

Tests must render the real `MessageResponse` pipeline through remark,
sanitize, and harden. Testing `MarkdownLink` in isolation is insufficient.

Verify:

- supported Windows, POSIX, and quoted-space paths render file badges;
- rendered text does not contain `[blocked]`;
- an explicit Windows `file:///D:/...` Markdown link survives;
- clicking passes the decoded path and starting line to `openFilePreview`;
- no click opens a browser; and
- unsupported candidates remain ordinary text.

### Scope and lifecycle tests

Verify:

- live text does not invoke the scanner and contains no automatic file badge;
- the same canonical text renders a badge after history handoff;
- completed-partition and full fallback rendering agree;
- user, system, reasoning, tool, plan, and permission content do not opt in;
- nested structured content does not inherit the top-level assistant opt-in;
- `MessageResponse`, `StreamingMarkdownDocument`, and `SealedBlock` memo
  comparisons react to the flag; and
- existing Markdown and explicit web links remain unchanged.

After focused tests, run:

```bash
pnpm test
pnpm eslint .
pnpm build
```

No Rust verification is required because the design changes only frontend
TypeScript and rendering behavior.

## Acceptance Criteria

1. A supported bare absolute path in a completed top-level assistant text part
   renders as the existing inline file badge.
2. The same text stays plain for the entire live turn.
3. Windows-drive, POSIX, quoted-space, and supported line-suffix cases open the
   intended file and starting line in the right-side file panel.
4. Supported Windows paths and explicit Windows `file://` links never render
   `[blocked]`.
5. User, system, reasoning, tool, plan, permission, collaboration, code, and
   unsupported path content remain unchanged.
6. Canonical transcript text, persistence, search, and message-copy input are
   unchanged.
7. The implementation performs no live per-token scan, no render-time file IO,
   and no sanitize/protocol expansion.
8. Focused tests, the full frontend suite, ESLint, and the static export build
   pass.

## Rollout and Rollback

The change needs no database migration, backend change, or feature flag. Its
grammar is intentionally conservative, and the opt-in limits the blast radius
to completed assistant prose.

Rollback consists of removing the top-level assistant `TextPart` opt-in. The
scanner and explicit Windows `file://` compatibility code can be reverted
independently if required.
