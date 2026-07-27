import { describe, expect, it } from "vitest"
import {
  findAbsoluteLocalPathRanges,
  isBareRelativeWorkspacePathLike,
  isLocalPathLike,
  passesRelativeAutolinkGate,
  toSafeLocalPathHref,
} from "./local-path-links"

function scan(text: string) {
  return findAbsoluteLocalPathRanges(text)
}

function links(text: string) {
  return scan(text).map((match) => ({
    label: match.label,
    path: match.path,
    locationSuffix: match.locationSuffix,
    href: toSafeLocalPathHref(match),
    selected: text.slice(match.start, match.end),
  }))
}

describe("findAbsoluteLocalPathRanges", () => {
  it.each([
    [
      String.raw`changed D:\repo\src\app.ts now`,
      String.raw`D:\repo\src\app.ts`,
      "/D:/repo/src/app.ts",
    ],
    [
      "changed D:/repo/src/app.ts now",
      "D:/repo/src/app.ts",
      "/D:/repo/src/app.ts",
    ],
    [
      "changed /Users/me/repo/src/app.ts now",
      "/Users/me/repo/src/app.ts",
      "/Users/me/repo/src/app.ts",
    ],
    [
      "changed /C:/repo/src/app.ts now",
      "/C:/repo/src/app.ts",
      "/C%3A/repo/src/app.ts",
    ],
    ["changed /README.md now", "/README.md", "/README.md"],
    ["changed /etc/hosts now", "/etc/hosts", "/etc/hosts"],
  ])("recognizes %s", (text, label, href) => {
    expect(links(text)).toEqual([
      {
        label,
        path: label,
        locationSuffix: null,
        href,
        selected: label,
      },
    ])
  })

  it.each([
    [String.raw`D:\repo\src\app.ts:12`, ":12"],
    [String.raw`D:\repo\src\app.ts:12:8`, ":12:8"],
    ["/Users/me/app.ts#L12", "#L12"],
    ["/Users/me/app.ts#L12-L20", "#L12-L20"],
    ["/Users/me/app.ts#L12-20", "#L12-20"],
  ])("preserves the location suffix in %s", (text, suffix) => {
    const [match] = scan(text)
    expect(match.locationSuffix).toBe(suffix)
    expect(toSafeLocalPathHref(match)?.endsWith(suffix)).toBe(true)
  })

  it("uses matching quotes as the only whitespace boundary", () => {
    const text = String.raw`see "D:\My Project\src\app.ts" and '/Users/me/My Project/a.ts'`
    const found = links(text)
    expect(found.map((item) => item.label)).toEqual([
      String.raw`D:\My Project\src\app.ts`,
      "/Users/me/My Project/a.ts",
    ])
    expect(found.map((item) => item.href)).toEqual([
      "/D:/My%20Project/src/app.ts",
      "/Users/me/My%20Project/a.ts",
    ])
    expect(found.every((item) => item.selected === item.label)).toBe(true)
  })

  it("tracks nested brackets and stops before an unmatched closer", () => {
    expect(links("see /tmp/a_[one_(2)].ts). next")[0]).toEqual(
      expect.objectContaining({
        label: "/tmp/a_[one_(2)].ts",
        href: "/tmp/a_%5Bone_(2)%5D.ts",
      })
    )
    expect(links("see /tmp/a)b(c).ts")[0]).toEqual(
      expect.objectContaining({
        label: "/tmp/a",
        href: "/tmp/a",
      })
    )
    expect(links("see /tmp/a(1].ts")[0]).toEqual(
      expect.objectContaining({
        label: "/tmp/a(1",
        href: "/tmp/a(1",
      })
    )
  })

  it("keeps adjacent ASCII and CJK sentence punctuation outside links", () => {
    expect(
      links("see /tmp/a.ts,/tmp/b.ts! next").map((item) => item.label)
    ).toEqual(["/tmp/a.ts", "/tmp/b.ts"])
    expect(
      links("see /tmp/c.ts. then /tmp/d.ts?").map((item) => item.label)
    ).toEqual(["/tmp/c.ts", "/tmp/d.ts"])
    expect(links("见 /Users/me/app.ts。下一项")[0]).toEqual(
      expect.objectContaining({
        label: "/Users/me/app.ts",
        href: "/Users/me/app.ts",
      })
    )
  })

  it("encodes filesystem data without confusing it with URI syntax", () => {
    const [match] = scan("/tmp/目录/a%#?b.ts")
    expect(toSafeLocalPathHref(match)).toBe(
      "/tmp/%E7%9B%AE%E5%BD%95/a%25%23%3Fb.ts"
    )
  })

  it("fails closed on malformed Unicode without throwing", () => {
    const [match] = scan("/tmp/\uD800.ts")
    expect(toSafeLocalPathHref(match)).toBeNull()
  })

  it.each([
    "/review",
    "/README",
    "https://example.com/src/app.ts",
    "//server/share/app.ts",
    String.raw`\\server\share\app.ts`,
    "@/repo/src/app.ts",
    // Note: "abc-/tmp/app.ts" is now a bare-relative positive (sep + .ts);
    // the absolute glued-boundary case is covered below separately.
    String.raw`D:\repo$math$\app.ts`,
    "~/repo/app.ts",
    // Relative positives moved out of this table (Slice A).
    String.raw`\"D:\My Project\app.ts\"`,
    String.raw`"D:\My \"Quoted\" Project\app.ts"`,
    String.raw`"D:\My 'Nested' Project\app.ts"`,
    String.raw`"D:\unterminated path\app.ts`,
    // CJK category labels with slashes (not filesystem paths). The leading
    // CJK character is not a START_BLOCKER, so `/…/…` would otherwise match.
    "进度/耗时/工具统计",
    "进度/耗时/工具统计走 Delegation Card 事件",
    "/耗时/工具统计",
    "/进度/耗时/工具",
  ])("rejects unsupported or ambiguous candidate %s", (text) => {
    expect(scan(text)).toEqual([])
  })

  it("still accepts POSIX paths that contain ASCII after CJK prose", () => {
    expect(
      links("进度走 /Users/me/app.ts 与 /tmp/目录/a.ts").map(
        (item) => item.label
      )
    ).toEqual(["/Users/me/app.ts", "/tmp/目录/a.ts"])
  })

  it("does not start a POSIX absolute path after a glued ASCII prefix", () => {
    const found = scan("abc-/tmp/app.ts")
    // Whole token may be bare-relative; never match interior "/tmp/app.ts".
    expect(found.map((m) => m.path)).not.toContain("/tmp/app.ts")
    expect(found.every((m) => m.start === 0 || m.path[0] !== "/")).toBe(true)
  })

  it("handles many matches without a timing-sensitive assertion", () => {
    const expected = Array.from(
      { length: 2_000 },
      (_, index) => `/repo/src/file-${index}.ts`
    )
    const text = expected.join(" ")
    const found = scan(text)
    expect(found).toHaveLength(2_000)
    expect(found.map((match) => match.label)).toEqual(expected)
  })
})

describe("relative path scanner (Slice A)", () => {
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
})

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
