import { describe, expect, it } from "vitest"
import {
  buildSuggestedTranslationName,
  buildTranslationTabId,
  formatFromTranslatablePath,
  hashDocumentContent,
  intlLocaleToWire,
  isTranslatablePath,
  isTranslationEligible,
  MAX_INPUT_SCALARS,
  type TranslationEligibilityTab,
} from "./document-translate"

function baseTab(
  overrides: Partial<TranslationEligibilityTab> = {}
): TranslationEligibilityTab {
  return {
    kind: "file",
    loading: false,
    path: "/repo/README.md",
    title: "README.md",
    content: "# Hello",
    language: "markdown",
    transient: undefined,
    ...overrides,
  }
}

describe("isTranslatablePath", () => {
  it("accepts md, markdown, and txt case-insensitively", () => {
    expect(isTranslatablePath("a.md")).toBe(true)
    expect(isTranslatablePath("a.MD")).toBe(true)
    expect(isTranslatablePath("/x/y.markdown")).toBe(true)
    expect(isTranslatablePath("C:\\x\\notes.TXT")).toBe(true)
  })

  it("rejects mdx and other extensions", () => {
    expect(isTranslatablePath("a.mdx")).toBe(false)
    expect(isTranslatablePath("a.ts")).toBe(false)
    expect(isTranslatablePath("README")).toBe(false)
    expect(isTranslatablePath(null)).toBe(false)
    expect(isTranslatablePath(undefined)).toBe(false)
    expect(isTranslatablePath("")).toBe(false)
  })
})

describe("isTranslationEligible", () => {
  it("accepts a settled markdown file tab with content", () => {
    expect(isTranslationEligible(baseTab())).toBe(true)
  })

  it("accepts plain text files", () => {
    expect(
      isTranslationEligible(
        baseTab({
          path: "/repo/notes.txt",
          title: "notes.txt",
          language: "plaintext",
          content: "hello",
        })
      )
    ).toBe(true)
  })

  it("rejects non-file kinds, loading, empty, and transient results", () => {
    expect(isTranslationEligible(baseTab({ kind: "diff" }))).toBe(false)
    expect(isTranslationEligible(baseTab({ loading: true }))).toBe(false)
    expect(isTranslationEligible(baseTab({ content: "   " }))).toBe(false)
    expect(isTranslationEligible(baseTab({ content: "" }))).toBe(false)
    expect(
      isTranslationEligible(
        baseTab({
          transient: {
            type: "translation",
            sourceTabId: "src",
            sourcePath: "/repo/README.md",
            sourceContentHash: "abc",
            locale: "zh_cn",
            format: "markdown",
            suggestedName: "README.zh_cn.md",
          },
        })
      )
    ).toBe(false)
  })

  it("rejects image and office languages/paths", () => {
    expect(
      isTranslationEligible(
        baseTab({
          path: "/repo/pic.png",
          title: "pic.png",
          language: "plaintext",
          content: "x",
        })
      )
    ).toBe(false)
    expect(
      isTranslationEligible(
        baseTab({
          path: "/repo/deck.pptx",
          title: "deck.pptx",
          language: "plaintext",
          content: "x",
        })
      )
    ).toBe(false)
  })

  it("rejects unsupported extensions even with content", () => {
    expect(
      isTranslationEligible(
        baseTab({
          path: "/repo/a.ts",
          title: "a.ts",
          language: "typescript",
          content: "const x = 1",
        })
      )
    ).toBe(false)
  })

  it("uses title when path is null for extension checks", () => {
    expect(
      isTranslationEligible(
        baseTab({
          path: null,
          title: "notes.md",
          content: "body",
        })
      )
    ).toBe(true)
  })
})

describe("hashDocumentContent", () => {
  it("returns a stable non-empty hex for known input", () => {
    // djb2 over UTF-16 code units; locked vector for "abc".
    expect(hashDocumentContent("abc")).toBe("b885c8b")
    expect(hashDocumentContent("abc")).toBe(hashDocumentContent("abc"))
  })

  it("differs for different content and handles unicode code units", () => {
    expect(hashDocumentContent("ab")).not.toBe(hashDocumentContent("abc"))
    // Surrogate pair: two UTF-16 units, not one scalar.
    const withEmoji = "a😀b"
    expect(hashDocumentContent(withEmoji)).toMatch(/^[0-9a-f]+$/)
    expect(hashDocumentContent(withEmoji).length).toBeGreaterThan(0)
  })
})

describe("intlLocaleToWire", () => {
  it("maps next-intl BCP-47 tags to snake_case wire ids", () => {
    expect(intlLocaleToWire("en")).toBe("en")
    expect(intlLocaleToWire("zh-CN")).toBe("zh_cn")
    expect(intlLocaleToWire("zh-TW")).toBe("zh_tw")
    expect(intlLocaleToWire("ja")).toBe("ja")
  })

  it("falls back to en for unknown tags", () => {
    expect(intlLocaleToWire("klingon")).toBe("en")
    expect(intlLocaleToWire("")).toBe("en")
  })

  it("accepts already-wire AppLocale ids", () => {
    expect(intlLocaleToWire("zh_cn")).toBe("zh_cn")
  })
})

describe("format and naming helpers", () => {
  it("picks markdown vs plainText from path", () => {
    expect(formatFromTranslatablePath("/a/b.md")).toBe("markdown")
    expect(formatFromTranslatablePath("/a/b.markdown")).toBe("markdown")
    expect(formatFromTranslatablePath("/a/b.txt")).toBe("plainText")
  })

  it("builds suggested name stem.locale.ext", () => {
    expect(buildSuggestedTranslationName("README.md", "zh_cn")).toBe(
      "README.zh_cn.md"
    )
    expect(buildSuggestedTranslationName("notes.txt", "ja")).toBe(
      "notes.ja.txt"
    )
    expect(buildSuggestedTranslationName("noext", "en")).toBe("noext.en")
  })

  it("builds stable translation tab ids", () => {
    expect(buildTranslationTabId("tab-1", "zh_cn", 3)).toBe(
      "translate:tab-1:zh_cn:3"
    )
  })

  it("exports the shared input scalar limit", () => {
    expect(MAX_INPUT_SCALARS).toBe(24_000)
  })
})
