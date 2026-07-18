import { act, render, screen } from "@testing-library/react"
import type {
  BundledLanguage,
  BundledTheme,
  HighlighterGeneric,
  ThemedToken,
} from "shiki"
import { afterEach, describe, expect, it, vi } from "vitest"
import {
  CodeBlockContent,
  clearHighlightCaches,
  highlightCode,
  type TokenizedCode,
  __getHighlightCacheStatsForTest,
  __putHighlightCacheForTest,
  __resetHighlightCachesForTest,
  __setHighlighterFactoryForTest,
} from "./code-block"

afterEach(() => {
  __resetHighlightCachesForTest()
})

function deferred<T>() {
  let resolve!: (value: T) => void
  const promise = new Promise<T>((done) => {
    resolve = done
  })
  return { promise, resolve }
}

function tokenized(content: string): TokenizedCode {
  return {
    bg: "transparent",
    fg: "inherit",
    tokens: [[{ content, color: "inherit" } as ThemedToken]],
  }
}

function shikiResult(content: string) {
  return {
    bg: "transparent",
    fg: "inherit",
    tokens: tokenized(content).tokens,
  }
}

function fakeHighlighter(
  tokenize: (code: string) => ReturnType<typeof shikiResult>
): HighlighterGeneric<BundledLanguage, BundledTheme> {
  return {
    getLoadedLanguages: () => ["ts"],
    codeToTokens: (code: string) => tokenize(code),
  } as unknown as HighlighterGeneric<BundledLanguage, BundledTheme>
}

describe("highlightCode", () => {
  it("starts one highlight for one code-language version", async () => {
    const engine = deferred<HighlighterGeneric<BundledLanguage, BundledTheme>>()
    const tokenize = vi.fn((code: string) => shikiResult(`${code}-token`))
    const factory = vi.fn(() => engine.promise)
    __setHighlighterFactoryForTest(factory)
    const callbackA = vi.fn()
    const callbackB = vi.fn()
    expect(highlightCode("const x = 1", "ts", callbackA)).toBeNull()
    expect(highlightCode("const x = 1", "ts", callbackB)).toBeNull()
    expect(factory).toHaveBeenCalledTimes(1)
    engine.resolve(fakeHighlighter(tokenize))
    await vi.waitFor(() => expect(callbackA).toHaveBeenCalledTimes(1))
    expect(tokenize).toHaveBeenCalledTimes(1)
    expect(callbackA).toHaveBeenCalledTimes(1)
    expect(callbackB).toHaveBeenCalledTimes(1)
  })

  it("evicts completed tokens by 128-entry or 8MiB budget", () => {
    for (let index = 0; index < 129; index += 1) {
      __putHighlightCacheForTest(`entry-${index}`, tokenized("x"))
    }
    expect(__getHighlightCacheStatsForTest().entries).toBe(128)
    __resetHighlightCachesForTest()
    __putHighlightCacheForTest(
      "large-a",
      tokenized("x".repeat(5 * 1024 * 1024))
    )
    __putHighlightCacheForTest(
      "large-b",
      tokenized("y".repeat(5 * 1024 * 1024))
    )
    expect(__getHighlightCacheStatsForTest().bytes).toBeLessThanOrEqual(
      8 * 1024 * 1024
    )
  })

  it("does not let a pre-reset inflight job repopulate the cache", async () => {
    const staleEngine =
      deferred<HighlighterGeneric<BundledLanguage, BundledTheme>>()
    const freshEngine =
      deferred<HighlighterGeneric<BundledLanguage, BundledTheme>>()
    let factoryCalls = 0
    __setHighlighterFactoryForTest(() => {
      factoryCalls += 1
      return factoryCalls === 1 ? staleEngine.promise : freshEngine.promise
    })
    const staleCallback = vi.fn()
    expect(highlightCode("stale-source", "ts", staleCallback)).toBeNull()

    // Generation bump without clearing the test factory override.
    clearHighlightCaches()
    expect(__getHighlightCacheStatsForTest().entries).toBe(0)

    const freshCallback = vi.fn()
    // Same source key after reset must start a new-generation job.
    expect(highlightCode("stale-source", "ts", freshCallback)).toBeNull()
    expect(factoryCalls).toBe(2)

    // Resolve the stale job first — it must not repopulate completedTokens.
    await act(async () => {
      staleEngine.resolve(
        fakeHighlighter((code) => shikiResult(`${code}-STALE`))
      )
    })
    await Promise.resolve()
    expect(staleCallback).not.toHaveBeenCalled()
    expect(highlightCode("stale-source", "ts")).toBeNull()

    await act(async () => {
      freshEngine.resolve(
        fakeHighlighter((code) => shikiResult(`${code}-token`))
      )
    })
    await vi.waitFor(() => expect(freshCallback).toHaveBeenCalledTimes(1))
    expect(staleCallback).not.toHaveBeenCalled()
    expect(highlightCode("stale-source", "ts")).not.toBeNull()
    expect(__getHighlightCacheStatsForTest().entries).toBe(1)
  })
})

describe("CodeBlockContent", () => {
  it("ignores a stale async result after props change", async () => {
    const engine = deferred<HighlighterGeneric<BundledLanguage, BundledTheme>>()
    __setHighlighterFactoryForTest(() => engine.promise)
    const { rerender } = render(<CodeBlockContent code="old" language="ts" />)
    rerender(<CodeBlockContent code="new" language="ts" />)
    await act(async () => {
      engine.resolve(fakeHighlighter((code) => shikiResult(`${code}-token`)))
    })
    await vi.waitFor(() =>
      expect(screen.getByText("new-token")).toBeInTheDocument()
    )
    expect(screen.queryByText("old-token")).not.toBeInTheDocument()
  })

  it("keeps raw code visible when Shiki rejects", async () => {
    __setHighlighterFactoryForTest(() =>
      Promise.reject(new Error("shiki unavailable"))
    )
    render(<CodeBlockContent code="const secret = 1" language="ts" />)
    // Immediate raw paint path — source remains visible regardless of rejection.
    expect(screen.getByText("const secret = 1")).toBeInTheDocument()
  })
})
