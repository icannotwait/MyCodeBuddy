import { describe, expect, it } from "vitest"
import {
  findAbsoluteLocalPathRanges,
  toSafeLocalPathHref,
} from "./local-path-links"

function links(text: string) {
  return findAbsoluteLocalPathRanges(text).map((match) => ({
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
    const [match] = findAbsoluteLocalPathRanges(text)
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
    const [match] = findAbsoluteLocalPathRanges("/tmp/目录/a%#?b.ts")
    expect(toSafeLocalPathHref(match)).toBe(
      "/tmp/%E7%9B%AE%E5%BD%95/a%25%23%3Fb.ts"
    )
  })

  it("fails closed on malformed Unicode without throwing", () => {
    const [match] = findAbsoluteLocalPathRanges("/tmp/\uD800.ts")
    expect(toSafeLocalPathHref(match)).toBeNull()
  })

  it.each([
    "/review",
    "/README",
    "https://example.com/src/app.ts",
    "//server/share/app.ts",
    String.raw`\\server\share\app.ts`,
    "@/repo/src/app.ts",
    "abc-/tmp/app.ts",
    String.raw`D:\repo$math$\app.ts`,
    "~/repo/app.ts",
    "./src/app.ts",
    "../src/app.ts",
    "src/app.ts",
    String.raw`\"D:\My Project\app.ts\"`,
    String.raw`"D:\My \"Quoted\" Project\app.ts"`,
    String.raw`"D:\My 'Nested' Project\app.ts"`,
    String.raw`"D:\unterminated path\app.ts`,
  ])("rejects unsupported or ambiguous candidate %s", (text) => {
    expect(findAbsoluteLocalPathRanges(text)).toEqual([])
  })

  it("handles many matches without a timing-sensitive assertion", () => {
    const expected = Array.from(
      { length: 2_000 },
      (_, index) => `/repo/src/file-${index}.ts`
    )
    const text = expected.join(" ")
    const found = findAbsoluteLocalPathRanges(text)
    expect(found).toHaveLength(2_000)
    expect(found.map((match) => match.label)).toEqual(expected)
  })
})
