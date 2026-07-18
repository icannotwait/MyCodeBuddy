import { describe, expect, it } from "vitest"
import {
  advanceLazyLoadGeneration,
  finishLazyLoad,
  hasIgnoredAncestor,
  isIgnoreFileName,
  IGNORE_FILE_NAMES,
} from "./use-file-tree"

describe("isIgnoreFileName", () => {
  it("recognizes ripgrep's default ignore file set", () => {
    expect(isIgnoreFileName(".gitignore")).toBe(true)
    expect(isIgnoreFileName(".ignore")).toBe(true)
    expect(isIgnoreFileName(".rgignore")).toBe(true)
  })

  it("rejects unrelated names", () => {
    expect(isIgnoreFileName("gitignore")).toBe(false)
    expect(isIgnoreFileName(".gitignores")).toBe(false)
    expect(isIgnoreFileName("rgignore")).toBe(false)
    expect(isIgnoreFileName(".fdignore")).toBe(false)
  })

  it("exposes the same set for callers that want to iterate", () => {
    expect([...IGNORE_FILE_NAMES].sort()).toEqual([
      ".gitignore",
      ".ignore",
      ".rgignore",
    ])
  })
})

describe("hasIgnoredAncestor", () => {
  it("returns true when an ancestor path is ignored", () => {
    const ignored = new Set(["node_modules", "dist/vendor"])
    expect(hasIgnoredAncestor("node_modules/pkg/index.js", ignored)).toBe(true)
    expect(hasIgnoredAncestor("dist/vendor/lib.js", ignored)).toBe(true)
  })

  it("returns false when no ancestor is ignored", () => {
    const ignored = new Set(["node_modules"])
    expect(hasIgnoredAncestor("src/index.ts", ignored)).toBe(false)
    expect(hasIgnoredAncestor("node_modules", ignored)).toBe(false)
  })
})

describe("finishLazyLoad", () => {
  it("only clears the in-flight marker owned by the finishing generation", () => {
    const inFlight = new Map([["src", 2]])

    finishLazyLoad(inFlight, "src", 1)
    expect(inFlight.get("src")).toBe(2)

    finishLazyLoad(inFlight, "src", 2)
    expect(inFlight.has("src")).toBe(false)
  })

  it("keeps an old same-mode request from owning a post-refresh marker", () => {
    const inFlight = new Map([["src", 1]])

    const nextGeneration = advanceLazyLoadGeneration(inFlight, 1)
    inFlight.set("src", nextGeneration)

    finishLazyLoad(inFlight, "src", 1)
    expect(inFlight.get("src")).toBe(nextGeneration)
  })
})
