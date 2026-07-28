# Assistant Relative Path Autolinking Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Autolink bare and explicit relative workspace paths in completed assistant prose (extension/special-basename gated) and classify the same bare-relative shapes as files for click/icon, without changing absolute-path behavior or requiring render-time FS checks.

**Architecture:** Extend the pure scanner in `src/lib/markdown/local-path-links.ts` with a `relative` kind, dual pure predicates (`isLocalPathLike` openability vs `passesRelativeAutolinkGate` confidence), and safe relative href construction (never root-prefix; `\`→`/`). Remark plugin stays structural; `link-safety` and `resource-kind` import the shared openability helper. Activation remains `autolinkLocalPaths` on completed top-level assistant text only.

**Tech Stack:** TypeScript (strict), Vitest, React Testing Library, remark/mdast, existing Streamdown Markdown pipeline.

## Global Constraints

- Work only in worktree `D:\MyCodeBuddy\.worktrees\assistant-relative-path-autolink` on branch `feat/assistant-relative-path-autolink`.
- Design baseline (locked): `docs/superpowers/specs/2026-07-27-assistant-relative-path-autolink-design.md` digest `sha256:618fdbb22ca891a3dfaf2eadcc3117b7fb7388c5b5abf3bbd444c2f8ff89494f`.
- No filesystem existence checks in scan/render path.
- Dual predicates: prose autolink gates **all** relative forms on extension/special basename; explicit Markdown `./` / `../` openability **without** extension stays (compatibility).
- Relative hrefs never gain a leading `/`; normalize `\` → `/` before segment encode for **relative** kind only.
- Parent traversal outside active folder reuses existing open policy (no new containment).
- Export `findLocalPathRanges`; keep `findAbsoluteLocalPathRanges` as deprecated thin alias (`/** @deprecated Use findLocalPathRanges. */`).
- **UNC / protocol-relative / openability order (Critical lock):** In `isLocalPathLike`:
  1. Raw UNC `\\…` → local (before any normalize).
  2. Raw protocol-relative `//…` → not local.
  3. Raw POSIX `/…` (not `//`) → local (do **not** accept single leading `\` as POSIX).
  4. Raw `~/…` only (not `~\…`) → local.
  5. Then normalize `\`→`/` **only** for explicit-relative `./`/`../` and Windows-drive prefix checks.
  6. Else bare-relative openability.
  Never turn `\\server\share` into web `//…`. Single-leading-backslash `\server\…` and `~\notes.md` stay **non-local** (preserve today).
- Exact whitelist and special basenames: copy from design (see Task 1 constants block). Case-insensitive ASCII fold for extensions and the full special-basename set. Special basenames are exact whole-basename matches after fold.
- 2,000-match mixed absolute+relative stress is **required** (not optional).
- Targeted verification + scoped eslint on all modified TS/TSX (incl. tests) + `pnpm build` (or project typecheck) before Task 3 done.
- Local commits only; no push/PR.

## File map

| File | Responsibility |
|------|----------------|
| `src/lib/markdown/local-path-links.ts` | Scanner, relative kind, whitelist, dual predicates, safe href |
| `src/lib/markdown/local-path-links.test.ts` | Unit tests for scan + predicates + href |
| `src/components/ai-elements/remark-autolink-local-paths.ts` | Call `findLocalPathRanges` (alias OK) |
| `src/components/ai-elements/link-safety.tsx` | Import shared `isLocalPathLike`; delete local duplicate |
| `src/lib/resource-kind.ts` | Import shared `isLocalPathLike`; delete local duplicate |
| `src/lib/resource-kind.test.ts` | Fixture flips + bare relative |
| `src/components/ai-elements/link-safety.test.tsx` | Open path + no-workspace + parent-traversal + compatibility |
| `src/components/ai-elements/markdown-link.test.tsx` | Icon for bare relative |
| `src/components/ai-elements/message-local-path-autolink.test.tsx` | Real pipeline prose + explicit MD + open args |
| `docs/superpowers/specs/2026-07-16-assistant-local-path-autolink-design.md` | Optional one-line cross-ref only |

## Exact policy constants (Task 1 must hard-code these)

**Extensions (case-insensitive):**  
`ts`, `tsx`, `js`, `jsx`, `mjs`, `cjs`, `json`, `jsonc`, `md`, `mdx`, `txt`, `rs`, `go`, `py`, `java`, `kt`, `cs`, `cpp`, `cc`, `c`, `h`, `hpp`, `css`, `scss`, `less`, `html`, `htm`, `xml`, `yml`, `yaml`, `toml`, `ini`, `sh`, `bash`, `zsh`, `ps1`, `bat`, `cmd`, `sql`, `graphql`, `gql`, `proto`, `vue`, `svelte`, `astro`, `swift`, `rb`, `php`, `r`, `lua`, `zig`, `dart`, `gradle`, `properties`, `env`, `lock`, `cmake`, `wasm`, `map`, `ipynb`, `csv`, `pdf`, `png`, `jpg`, `jpeg`, `svg`, `webp`, `gif`, `ico`, `bmp`, `avif`, `mp4`, `webm`, `mov`, `mp3`, `wav`, `ogg`, `flac`, `zip`, `tar`, `gz`, `tgz`, `7z`, `rar`, `docx`, `xlsx`, `pptx`

**Special basenames (case-insensitive whole basename):**  
`Dockerfile`, `Makefile`, `CMakeLists.txt`, `.gitignore`, `.env`, `.env.local`, `.editorconfig`, `.npmrc`, `.eslintrc`, `.prettierrc`

---

### Task 1: Relative scanner, dual predicates, and safe href

**Files:**
- Modify: `src/lib/markdown/local-path-links.ts`
- Modify: `src/lib/markdown/local-path-links.test.ts`
- Modify: `src/components/ai-elements/remark-autolink-local-paths.ts` (switch import to `findLocalPathRanges` if desired; alias keeps old import working)

**Interfaces:**
- Consumes: existing tokenizer structure
- Produces:
  - `export type LocalPathKind = "windows-drive" | "posix" | "relative"`
  - `export function findLocalPathRanges(text: string): LocalPathMatch[]`
  - `/** @deprecated Use findLocalPathRanges. */ export function findAbsoluteLocalPathRanges(text: string): LocalPathMatch[]` (same impl)
  - `export function toSafeLocalPathHref(match: LocalPathMatch): string | null`
  - `export function isLocalPathLike(path: string): boolean`
  - `export function isBareRelativeWorkspacePathLike(path: string): boolean`
  - `export function passesRelativeAutolinkGate(path: string): boolean`

#### Slice A — full scanner RED on existing export (module loads)

- [ ] **Step 1A: Write the complete relative scanner/guard/href suite using only `findAbsoluteLocalPathRanges` + `toSafeLocalPathHref`**

Do **not** import new symbols yet. Keep calling the existing export name so the file loads. Prefer a local helper:

```ts
function scan(text: string) {
  return findAbsoluteLocalPathRanges(text)
}
```

Add **all** of the following expectations (they must FAIL behaviorally before implementation):

```ts
it("matches dot-prefixed bare directory at string start (index 0)", () => {
  const text = ".github/workflows/ci.yml"
  const found = scan(text)
  expect(found[0]?.start).toBe(0)
  expect(found[0]?.label).toBe(text)
  expect(toSafeLocalPathHref(found[0]!)).toBe(".github/workflows/ci.yml")
})

it.each([
  ["docs/a.md", "docs/a.md", "docs/a.md"],
  ["src/app.ts", "src/app.ts", "src/app.ts"],
  ["./src/lib/app.ts", "./src/lib/app.ts", "./src/lib/app.ts"],
  ["../plans/x.md", "../plans/x.md", "../plans/x.md"],
  ["../../x.ts", "../../x.ts", "../../x.ts"],
  [String.raw`src\lib\app.ts`, String.raw`src\lib\app.ts`, "src/lib/app.ts"],
  [String.raw`.\src\a.ts`, String.raw`.\src\a.ts`, "./src/a.ts"],
  [String.raw`..\plans\x.md`, String.raw`..\plans\x.md`, "../plans/x.md"],
  ["deploy/dockerfile", "deploy/dockerfile", "deploy/dockerfile"],
  ["assets/logo.png", "assets/logo.png", "assets/logo.png"],
  ["docs/spec.pdf", "docs/spec.pdf", "docs/spec.pdf"],
  ["reports/status.docx", "reports/status.docx", "reports/status.docx"],
])("matches relative %s", (token, path, href) => {
  const [m] = scan(`see ${token} now`)
  expect(m?.label).toBe(token)
  expect(m?.path).toBe(path)
  expect(toSafeLocalPathHref(m!)).toBe(href)
})

it("quoted relative with spaces encodes href", () => {
  const [m] = scan('see "docs/My File.md" now')
  expect(m?.path).toBe("docs/My File.md")
  expect(toSafeLocalPathHref(m!)).toBe("docs/My%20File.md")
})

it.each([
  ["./src/app.ts:12", "./src/app.ts", ":12", "./src/app.ts:12"],
  ["./src/app.ts#L12", "./src/app.ts", "#L12", "./src/app.ts#L12"],
])("relative location suffix %s", (text, path, suffix, href) => {
  const [m] = scan(text)
  expect(m?.path).toBe(path)
  expect(m?.locationSuffix).toBe(suffix)
  expect(toSafeLocalPathHref(m!)).toBe(href)
})

it.each([
  "src/app",
  "README.md",
  "www.example.com/docs/a.md",
  "进度/耗时/工具统计",
  "docs//a.md",
  "./src/app",
  ".env",
  "deploy/Dockerfiles",
  "docs/a$b.md",
])("does not match relative-only candidate %s", (text) => {
  expect(scan(text)).toEqual([])
})

// Whole-token: no local match of any kind
it.each([
  "https://example.com/docs/a.md",
  "//cdn.example.com/docs/a.md",
  "@/repo/src/app.ts",
  "@scope/pkg/src/a.ts",
])("whole-token reject yields zero matches for %s", (text) => {
  expect(scan(text)).toEqual([])
})

it("mixed absolute+relative stress (2000 matches)", () => {
  const parts: string[] = []
  const expectedLabels: string[] = []
  for (let i = 0; i < 1000; i += 1) {
    const abs = `/repo/src/file-${i}.ts`
    const rel = `pkg/src/file-${i}.ts`
    parts.push(abs, rel)
    expectedLabels.push(abs, rel)
  }
  const found = scan(parts.join(" "))
  expect(found).toHaveLength(2000)
  expect(found.map((m) => m.label)).toEqual(expectedLabels)
})
```

Remove `./src/app.ts`, `../src/app.ts`, `src/app.ts` from the old absolute-only reject table (they become positives). Keep absolute reject cases that must stay negative.

- [ ] **Step 2A: Run — expect behavioral FAIL**

```powershell
pnpm exec vitest run src/lib/markdown/local-path-links.test.ts
```

Expected: FAIL on assertions (empty matches / wrong href), **not** transform/import error.

#### Slice B — implement only scanner/href (GREEN for Slice A)

- [ ] **Step 3B: Implement relative scan, whitelist, starts, safe href (no new predicate exports yet)**

In `local-path-links.ts`:

1. `LocalPathKind` += `"relative"`.
2. Hard-code exact extension + special basename sets from **Exact policy constants**.
3. Candidate starts (after absolute): explicit `./` `.\\` `../` `..\\`; then dot-prefixed bare (`.github/…`); then letter/digit bare.
4. Relative form + autolink gate after absolute classify fails (gate may be internal until Slice C).
5. Whole-token reject scheme URLs and `//host…` (no interior autolink).
6. Reject empty segments after `\`→`/` normalize for relative.
7. `toSafeLocalPathHref` for relative: normalize `\`, encode segments, **never** leading `/`, append `locationSuffix` raw.
8. Export `findLocalPathRanges`; alias:

```ts
/** @deprecated Use findLocalPathRanges. */
export function findAbsoluteLocalPathRanges(text: string): LocalPathMatch[] {
  return findLocalPathRanges(text)
}
```

- [ ] **Step 4B: Run scan tests — expect PASS**

```powershell
pnpm exec vitest run src/lib/markdown/local-path-links.test.ts
```

#### Slice C — dual predicates + UNC order (RED then GREEN)

- [ ] **Step 5C: Add export stubs so tests load, then write predicate tests and run RED**

First, if needed for module load only, add thin stubs that throw or return `false` is OK **only if** the first run shows assertion failures (not import errors). Prefer exporting real signatures returning `false` initially:

```ts
export function isLocalPathLike(_path: string): boolean {
  return false
}
export function isBareRelativeWorkspacePathLike(_path: string): boolean {
  return false
}
export function passesRelativeAutolinkGate(_path: string): boolean {
  return false
}
```

Then add tests (import the three exports):

```ts
describe("isLocalPathLike dual contract + UNC order", () => {
  it("treats backslash UNC as local before slash normalize", () => {
    expect(isLocalPathLike(String.raw`\\server\share\a.md`)).toBe(true)
  })
  it("rejects forward-slash protocol-relative as local", () => {
    expect(isLocalPathLike("//cdn.example.com/a.md")).toBe(false)
  })
  it("does not treat single-leading-backslash as POSIX absolute", () => {
    expect(isLocalPathLike(String.raw`\server\share\a.md`)).toBe(false)
  })
  it("does not treat tilde-backslash as home-relative", () => {
    expect(isLocalPathLike(String.raw`~\notes.md`)).toBe(false)
  })
  it("explicit relative without extension remains openable", () => {
    expect(isLocalPathLike("./src/app")).toBe(true)
    expect(isLocalPathLike(String.raw`.\src\app`)).toBe(true)
    expect(isLocalPathLike("../src/app")).toBe(true)
    expect(passesRelativeAutolinkGate("./src/app")).toBe(false)
  })
  it("bare relative openability requires extension/special basename", () => {
    expect(isBareRelativeWorkspacePathLike("docs/a.md")).toBe(true)
    expect(isBareRelativeWorkspacePathLike("src/app")).toBe(false)
    expect(isLocalPathLike("docs/a.md")).toBe(true)
    expect(isLocalPathLike("src/app")).toBe(false)
    expect(passesRelativeAutolinkGate("docs/a.md")).toBe(true)
    expect(passesRelativeAutolinkGate("./src/app.ts")).toBe(true)
  })
  it("strips location suffixes before bare-relative gate", () => {
    expect(isBareRelativeWorkspacePathLike("docs/a.md:12")).toBe(true)
    expect(isBareRelativeWorkspacePathLike("docs/a.md#L12")).toBe(true)
    expect(isLocalPathLike("docs/a.md:12")).toBe(true)
    expect(isLocalPathLike("docs/a.md#L12")).toBe(true)
    expect(isBareRelativeWorkspacePathLike("README.md:12")).toBe(false)
  })
})
```

Run RED:

```powershell
pnpm exec vitest run src/lib/markdown/local-path-links.test.ts
```

Expected: FAIL on `true` expectations while stubs return `false`.

- [ ] **Step 6C: Implement predicates with locked order + suffix strip**

```ts
export function isLocalPathLike(path: string): boolean {
  if (!path) return false
  if (path.startsWith("\\\\")) return true // raw UNC
  if (path.startsWith("//")) return false // protocol-relative
  if (path.startsWith("/")) return true // raw POSIX only
  if (path.startsWith("~/")) return true // raw home only
  const normalized = path.replace(/\\/g, "/")
  if (normalized.startsWith("./") || normalized.startsWith("../")) return true
  if (/^[a-zA-Z]:\//.test(normalized)) return true
  return isBareRelativeWorkspacePathLike(path)
}
```

`isBareRelativeWorkspacePathLike` / `passesRelativeAutolinkGate` **must strip** locked location suffixes (`:12`, `:12:8`, `#L12`, `#L12-L20`, `#L12-20`) **before** extension/special/basename gates (same regex family as scanner). Autolink gate applies extension to **all** relative forms including `./`.

- [ ] **Step 7C: Run full unit file — PASS**

```powershell
pnpm exec vitest run src/lib/markdown/local-path-links.test.ts
pnpm exec vitest run src/components/ai-elements/remark-autolink-local-paths.test.ts
```

- [ ] **Step 8C: Commit**

```powershell
git add src/lib/markdown/local-path-links.ts src/lib/markdown/local-path-links.test.ts src/components/ai-elements/remark-autolink-local-paths.ts
git commit -m "feat: autolink relative workspace paths in pure scanner"
```

---

### Task 2: Shared openability in link-safety and resource-kind

**Files:**
- Modify: `src/components/ai-elements/link-safety.tsx`
- Modify: `src/components/ai-elements/link-safety.test.tsx`
- Modify: `src/lib/resource-kind.ts`
- Modify: `src/lib/resource-kind.test.ts`
- Modify: `src/components/ai-elements/markdown-link.test.tsx`

**Interfaces:**
- Consumes: `isLocalPathLike` from `@/lib/markdown/local-path-links` (suffix-aware bare gate from Task 1)
- Produces: bare relative open + icons; UNC/protocol-relative unchanged; no-workspace toast for bare relative

**Ownership (locked):** Task 2 may **only** modify the consumer files listed above. If a shared-helper defect is discovered (`local-path-links.ts`), **stop**, report hand-back to Task 1 owner with failing test evidence, and do **not** silently edit/commit the shared module in Task 2. Re-run Task 1 fix + re-review if needed.

- [ ] **Step 1: Update mocks + write failing consumer tests (executable)**

In `link-safety.test.tsx`, change the active-folder mock to support null folder:

```ts
const mocks = vi.hoisted(() => ({
  // ...existing...
  activeFolderPath: "/repo" as string | null,
}))

vi.mock("@/contexts/active-folder-context", () => ({
  useActiveFolder: () => ({
    activeFolder:
      mocks.activeFolderPath === null
        ? null
        : { path: mocks.activeFolderPath },
  }),
}))
```

`resource-kind.test.ts` additions/flips (executable expects):

```ts
expect(classifyResourceKind("src/main.rs")).toBe("file")
expect(classifyResourceKind("docs/a.md")).toBe("file")
expect(classifyResourceKind("docs/a.md:12")).toBe("file")
expect(classifyResourceKind("docs/a.md#L12")).toBe("file")
expect(classifyResourceKind("./src/app")).toBe("file")
expect(classifyResourceKind("src/app")).toBeNull()
expect(classifyResourceKind("README.md:12")).toBeNull()
expect(classifyResourceKind("www.example.com/docs/a.md")).toBeNull()
expect(classifyResourceKind(String.raw`\\server\share\a.md`)).toBe("file")
expect(classifyResourceKind("//cdn.example.com/a.md")).toBe("web")
```

`link-safety.test.tsx` cases using existing `LinkSafetyHarness` (second-arg shape matches file: `{ line: undefined }` / `{ line: 12 }`):

```ts
it("opens bare relative docs/a.md via openFilePreview", async () => {
  render(<LinkSafetyHarness url="docs/a.md" />)
  fireEvent.click(screen.getByRole("button", { name: "Trigger link" }))
  await waitFor(() => {
    expect(mocks.openFilePreview).toHaveBeenCalledWith("docs/a.md", {
      line: undefined,
    })
  })
  expect(mocks.openUrl).not.toHaveBeenCalled()
  expect(window.open).not.toHaveBeenCalled()
})

it("opens ./src/a.ts:12 with line and strips ./", async () => {
  render(<LinkSafetyHarness url="./src/a.ts:12" />)
  fireEvent.click(screen.getByRole("button", { name: "Trigger link" }))
  await waitFor(() => {
    expect(mocks.openFilePreview).toHaveBeenCalledWith("src/a.ts", {
      line: 12,
    })
  })
})

it("opens extensionless ./src/app for compatibility", async () => {
  render(<LinkSafetyHarness url="./src/app" />)
  fireEvent.click(screen.getByRole("button", { name: "Trigger link" }))
  await waitFor(() => {
    expect(mocks.openFilePreview).toHaveBeenCalledWith("src/app", {
      line: undefined,
    })
  })
})

it("toasts errorNoWorkspace for bare relative when no active folder", async () => {
  mocks.activeFolderPath = null
  render(<LinkSafetyHarness url="docs/a.md" />)
  fireEvent.click(screen.getByRole("button", { name: "Trigger link" }))
  await waitFor(() => {
    expect(mocks.toastError).toHaveBeenCalled()
  })
  // useTranslations mock returns the key; toast receives description: "errorNoWorkspace"
  expect(mocks.toastError.mock.calls.some((c) =>
    JSON.stringify(c).includes("errorNoWorkspace")
  )).toBe(true)
  expect(mocks.openFilePreview).not.toHaveBeenCalled()
  expect(mocks.openUrl).not.toHaveBeenCalled()
  expect(window.open).not.toHaveBeenCalled()
})

it("parent traversal ../outside.md still attempts open (no containment block)", async () => {
  render(<LinkSafetyHarness url="../outside.md" />)
  fireEvent.click(screen.getByRole("button", { name: "Trigger link" }))
  await waitFor(() => {
    expect(mocks.openFilePreview).toHaveBeenCalledWith("../outside.md", {
      line: undefined,
    })
  })
  expect(mocks.toastError).not.toHaveBeenCalled()
})
```

`markdown-link.test.tsx`: flip `src/main.rs` to expect file type icon if currently asserting none.

- [ ] **Step 2: Run — expect FAIL on new expectations**

```powershell
pnpm exec vitest run src/lib/resource-kind.test.ts src/components/ai-elements/link-safety.test.tsx src/components/ai-elements/markdown-link.test.tsx
```

- [ ] **Step 3: Wire shared helper (consumers only)**

```ts
// link-safety.tsx and resource-kind.ts
import { isLocalPathLike } from "@/lib/markdown/local-path-links"
// delete local isLocalPathLike implementations
```

Update `resource-kind.ts` header comment: gated bare-relative paths (incl. suffix forms) are now file.

- [ ] **Step 4: Run — PASS**

```powershell
pnpm exec vitest run src/lib/resource-kind.test.ts src/components/ai-elements/link-safety.test.tsx src/components/ai-elements/markdown-link.test.tsx src/lib/markdown/local-path-links.test.ts
```

- [ ] **Step 5: Commit (consumer files only)**

```powershell
git add src/components/ai-elements/link-safety.tsx src/components/ai-elements/link-safety.test.tsx src/lib/resource-kind.ts src/lib/resource-kind.test.ts src/components/ai-elements/markdown-link.test.tsx
git commit -m "feat: open bare relative paths via shared isLocalPathLike"
```

---

### Task 3: MessageResponse integration, optional design cross-ref, verification gate

**Files:**
- Modify: `src/components/ai-elements/message-local-path-autolink.test.tsx`
- Optionally modify: `docs/superpowers/specs/2026-07-16-assistant-local-path-autolink-design.md` (one-line cross-ref only)
- **Test-only for pipeline:** If a test fails due to production defect, **stop and report** which Task 1/2 module owns the bug; fix in that module (or a follow-up fix commit on this branch) with evidence. Do not invent a third open path.

**Interfaces:**
- Consumes: completed Task 1–2 exports and wiring
- Produces: integration evidence for design success criteria + lint/typecheck gate

#### Task 3 role: post-feature regression / pipeline proof (not a RED feature task)

Tasks 1–2 own production TDD. Task 3 expands real-pipeline coverage and the
verification gate. Tests may be green on first run after Task 1–2; that is
expected. Do not claim Task 3 as the primary RED proof for relative autolink.

- [ ] **Step 1: Add complete integration tests**

Use existing mocks (`activeFolder: { path: "/repo" }`, `openFilePreview`).
Second-arg contract is **exactly** `{ line: undefined }` or `{ line: N }` as in
`link-safety.test.tsx` / absolute cases in this file.

```tsx
it("autolinks bare relative prose when enabled and opens via openFilePreview", async () => {
  const rel =
    "docs/superpowers/plans/2026-07-27-empty-folder-workspace-visibility.md"
  const { container } = render(
    <MessageResponse autolinkLocalPaths>{`see ${rel} now`}</MessageResponse>
  )
  const button = await waitFor(() => {
    const el = container.querySelector<HTMLButtonElement>(
      "button[data-resource-kind='file']"
    )
    expect(el).not.toBeNull()
    return el!
  })
  fireEvent.click(button)
  await waitFor(() => {
    expect(mocks.openFilePreview).toHaveBeenCalledWith(rel, {
      line: undefined,
    })
  })
})

it.each([
  ["./src/a.ts", "src/a.ts"],
  ["../plans/x.md", "../plans/x.md"],
])("opens relative prose %s as %s", async (prose, expectedPath) => {
  const { container } = render(
    <MessageResponse autolinkLocalPaths>{`see ${prose} now`}</MessageResponse>
  )
  const button = await waitFor(() => {
    const el = container.querySelector<HTMLButtonElement>(
      "button[data-resource-kind='file']"
    )
    expect(el).not.toBeNull()
    return el!
  })
  fireEvent.click(button)
  await waitFor(() => {
    expect(mocks.openFilePreview).toHaveBeenCalledWith(expectedPath, {
      line: undefined,
    })
  })
})
it("does not autolink relative prose when flag off", async () => {
  const { container } = render(
    <MessageResponse>{"see docs/a.md now"}</MessageResponse>
  )
  await waitFor(() => expect(container.textContent).toContain("docs/a.md"))
  expect(
    container.querySelector("[data-reference-badge][data-ref-type='file']")
  ).toBeNull()
})

it("does not autolink relative path in inline code", async () => {
  const { container } = render(
    <MessageResponse autolinkLocalPaths>{"`docs/a.md`"}</MessageResponse>
  )
  await waitFor(() => expect(container.querySelector("code")).not.toBeNull())
  expect(
    container.querySelector("[data-reference-badge][data-ref-type='file']")
  ).toBeNull()
})

it("opens explicit bare relative markdown link [x](docs/a.md)", async () => {
  const { container } = render(
    <MessageResponse autolinkLocalPaths>{"[x](docs/a.md)"}</MessageResponse>
  )
  const button = await waitFor(() => {
    const el = container.querySelector<HTMLButtonElement>(
      "button[data-resource-kind='file']"
    )
    expect(el).not.toBeNull()
    return el!
  })
  fireEvent.click(button)
  await waitFor(() => {
    expect(mocks.openFilePreview).toHaveBeenCalledWith("docs/a.md", {
      line: undefined,
    })
  })
})

it("keeps extensionless explicit relative markdown [app](./src/app) as file", async () => {
  const { container } = render(
    <MessageResponse autolinkLocalPaths>{"[app](./src/app)"}</MessageResponse>
  )
  await waitFor(() => {
    expect(
      container.querySelector("button[data-resource-kind='file']")
    ).not.toBeNull()
  })
})

it("quoted relative with spaces and line suffix round-trips", async () => {
  const { container } = render(
    <MessageResponse autolinkLocalPaths>
      {'see "docs/My File.md:12" now'}
    </MessageResponse>
  )
  const button = await waitFor(() => {
    const el = container.querySelector<HTMLButtonElement>(
      "button[data-resource-kind='file']"
    )
    expect(el).not.toBeNull()
    return el!
  })
  fireEvent.click(button)
  await waitFor(() => {
    expect(mocks.openFilePreview).toHaveBeenCalledWith("docs/My File.md", {
      line: 12,
    })
  })
})
```

- [ ] **Step 2: Run integration**

```powershell
pnpm exec vitest run src/components/ai-elements/message-local-path-autolink.test.tsx
```

- [ ] **Step 3: Optional parent design one-liner**

If parent still claims relative paths are non-goals without pointer, add:

```markdown
> Relative workspace-path autolinking is specified in
> `docs/superpowers/specs/2026-07-27-assistant-relative-path-autolink-design.md`.
```

- [ ] **Step 4: Full verification gate**

```powershell
pnpm exec vitest run src/lib/markdown/local-path-links.test.ts src/lib/resource-kind.test.ts src/components/ai-elements/link-safety.test.tsx src/components/ai-elements/markdown-link.test.tsx src/components/ai-elements/remark-autolink-local-paths.test.ts src/components/ai-elements/message-local-path-autolink.test.tsx src/components/ai-elements/message-windows-file-link.test.tsx src/components/ai-elements/message-file-uri-pipeline.test.tsx

pnpm exec eslint src/lib/markdown/local-path-links.ts src/lib/markdown/local-path-links.test.ts src/lib/resource-kind.ts src/lib/resource-kind.test.ts src/components/ai-elements/link-safety.tsx src/components/ai-elements/link-safety.test.tsx src/components/ai-elements/markdown-link.test.tsx src/components/ai-elements/remark-autolink-local-paths.ts src/components/ai-elements/message-local-path-autolink.test.tsx

pnpm build
```

Expected: all green. Record command outputs in `.superpowers/sdd/task-3-report.md`.

- [ ] **Step 5: Commit**

```powershell
git add src/components/ai-elements/message-local-path-autolink.test.tsx docs/superpowers/specs/2026-07-16-assistant-local-path-autolink-design.md
git commit -m "test: cover relative path autolink in MessageResponse pipeline"
```

---

## Spec coverage checklist

| Design requirement | Task |
|--------------------|------|
| Relative forms bare + `./` `.\` `../` `..\` + `../../` | 1 |
| Candidate starts incl. `.github` index 0 | 1 |
| Exact whitelist + Office + special basenames | 1 constants |
| Dual predicates | 1 |
| UNC before normalize; `//` not local | 1 |
| Safe href no leading `/`, `\` normalize | 1 |
| Whole-token URL/alias reject | 1 |
| 2000 mixed stress | 1 |
| Shared helper consumers | 2 |
| No-workspace | 2 |
| Parent traversal open | 2 |
| Explicit MD bare + extensionless `./` | 2, 3 |
| Quoted relative + line suffix pipeline | 1, 3 |
| Absolute/file:// regressions | 1, 3 |
| Optional parent cross-ref | 3 |
| eslint + build gate | 3 |

## Plan self-review notes (post r1 plan review)

- Critical UNC order locked in Global Constraints + Task 1 Step 6C sample.
- RED Slice A uses existing export only (behavioral fail).
- Task 4 folded into Task 3 verification (no empty final task).
- Placeholders replaced with complete test bodies; openFilePreview second arg must match existing harness in each test file.
- Task 3 ownership: test-only; production fixes return to Task 1/2 modules.
