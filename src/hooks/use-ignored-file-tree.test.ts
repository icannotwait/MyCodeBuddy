import { act, renderHook, waitFor } from "@testing-library/react"
import { beforeEach, describe, expect, it, vi } from "vitest"

import type { WorkspaceEnvelopeListener } from "./use-workspace-state-store"
import type { FileTreeNode } from "@/lib/types"
import { FILE_TREE_SHOW_IGNORED_STORAGE_KEY } from "@/lib/file-tree-display-prefs"
import {
  shouldRefreshIgnoredTree,
  useIgnoredFileTree,
} from "./use-ignored-file-tree"

interface Deferred<T> {
  promise: Promise<T>
  resolve: (value: T) => void
  reject: (reason?: unknown) => void
}

function deferred<T>(): Deferred<T> {
  let resolve!: (value: T) => void
  let reject!: (reason?: unknown) => void
  const promise = new Promise<T>((resolvePromise, rejectPromise) => {
    resolve = resolvePromise
    reject = rejectPromise
  })
  return { promise, resolve, reject }
}

const mocks = vi.hoisted(() => ({
  getFileTree: vi.fn(),
}))

vi.mock("@/lib/api", () => ({
  getFileTree: (...args: unknown[]) => mocks.getFileTree(...args),
}))

const fallbackA: FileTreeNode[] = [
  { kind: "file", name: "visible-a.ts", path: "visible-a.ts" },
]
const fallbackB: FileTreeNode[] = [
  { kind: "file", name: "visible-b.ts", path: "visible-b.ts" },
]
const ignoredA: FileTreeNode[] = [
  { kind: "file", name: "ignored-a.ts", path: "dist/ignored-a.ts" },
]
const ignoredB: FileTreeNode[] = [
  { kind: "file", name: "ignored-b.ts", path: "dist/ignored-b.ts" },
]

function createEnvelopeHarness() {
  let listener: WorkspaceEnvelopeListener | null = null
  return {
    subscribe: vi.fn((next: WorkspaceEnvelopeListener) => {
      listener = next
      return () => {
        if (listener === next) listener = null
      }
    }),
    emit(envelope: Parameters<WorkspaceEnvelopeListener>[0]) {
      act(() => listener?.(envelope))
    },
  }
}

describe("shouldRefreshIgnoredTree", () => {
  it("distinguishes structural, ignore-control, and content events", () => {
    expect(shouldRefreshIgnoredTree("create", ["src/new.ts"])).toBe(true)
    expect(shouldRefreshIgnoredTree("remove", ["src/old.ts"])).toBe(true)
    expect(shouldRefreshIgnoredTree("modify", ["src/main.ts"])).toBe(false)
    expect(shouldRefreshIgnoredTree("modify", [".gitignore"])).toBe(true)
    expect(shouldRefreshIgnoredTree("modify", ["nested/.ignore"])).toBe(true)
    expect(shouldRefreshIgnoredTree("modify", [])).toBe(true)
    expect(shouldRefreshIgnoredTree(undefined, ["src/unknown.ts"])).toBe(true)
  })
})

describe("useIgnoredFileTree", () => {
  beforeEach(() => {
    localStorage.clear()
    mocks.getFileTree.mockReset()
  })

  it("uses the fallback without an extra request until enabled", async () => {
    mocks.getFileTree.mockResolvedValueOnce(ignoredA)
    const envelopes = createEnvelopeHarness()
    const { result } = renderHook(() =>
      useIgnoredFileTree({
        folderPath: "/repo",
        fallbackTree: fallbackA,
        workspaceSeq: 5,
        subscribeEnvelopes: envelopes.subscribe,
      })
    )

    await waitFor(() => expect(result.current.restored).toBe(true))
    expect(result.current.showIgnored).toBe(false)
    expect(result.current.tree).toBe(fallbackA)
    expect(result.current.loading).toBe(false)
    expect(mocks.getFileTree).not.toHaveBeenCalled()

    act(() => result.current.setShowIgnored(true))
    expect(result.current.loading).toBe(true)
    await waitFor(() =>
      expect(mocks.getFileTree).toHaveBeenCalledWith("/repo", 2, true)
    )
    await waitFor(() => expect(result.current.tree).toBe(ignoredA))
    expect(result.current.loading).toBe(false)
  })

  it("coalesces event refreshes and skips ordinary modifies", async () => {
    localStorage.setItem(FILE_TREE_SHOW_IGNORED_STORAGE_KEY, "true")
    const initial = deferred<FileTreeNode[]>()
    const queued = deferred<FileTreeNode[]>()
    const ignoreRefresh = deferred<FileTreeNode[]>()
    const sweepRefresh = deferred<FileTreeNode[]>()
    mocks.getFileTree
      .mockReturnValueOnce(initial.promise)
      .mockReturnValueOnce(queued.promise)
      .mockReturnValueOnce(ignoreRefresh.promise)
      .mockReturnValueOnce(sweepRefresh.promise)
    const envelopes = createEnvelopeHarness()
    renderHook(() =>
      useIgnoredFileTree({
        folderPath: "/repo",
        fallbackTree: fallbackA,
        workspaceSeq: 10,
        subscribeEnvelopes: envelopes.subscribe,
      })
    )
    await waitFor(() => expect(mocks.getFileTree).toHaveBeenCalledTimes(1))

    envelopes.emit({
      seq: 11,
      kind: "meta",
      fs_event_kind: "create",
      changed_paths: ["dist/new.js"],
    })
    envelopes.emit({
      seq: 12,
      kind: "meta",
      fs_event_kind: "remove",
      changed_paths: ["dist/old.js"],
    })
    expect(mocks.getFileTree).toHaveBeenCalledTimes(1)

    await act(async () => {
      initial.resolve(ignoredA)
      await initial.promise
    })
    await waitFor(() => expect(mocks.getFileTree).toHaveBeenCalledTimes(2))
    await act(async () => {
      queued.resolve(ignoredA)
      await queued.promise
    })

    envelopes.emit({
      seq: 13,
      kind: "meta",
      fs_event_kind: "modify",
      changed_paths: ["src/main.ts"],
    })
    expect(mocks.getFileTree).toHaveBeenCalledTimes(2)

    envelopes.emit({
      seq: 14,
      kind: "meta",
      fs_event_kind: "modify",
      changed_paths: ["nested/.rgignore"],
    })
    await waitFor(() => expect(mocks.getFileTree).toHaveBeenCalledTimes(3))
    envelopes.emit({
      seq: 15,
      kind: "meta",
      fs_event_kind: "modify",
      changed_paths: [],
    })
    expect(mocks.getFileTree).toHaveBeenCalledTimes(3)

    await act(async () => {
      ignoreRefresh.resolve(ignoredA)
      await ignoreRefresh.promise
    })
    await waitFor(() => expect(mocks.getFileTree).toHaveBeenCalledTimes(4))
    await act(async () => {
      sweepRefresh.resolve(ignoredA)
      await sweepRefresh.promise
    })
  })

  it("refreshes a seq-only recovery without duplicating a normal envelope", async () => {
    localStorage.setItem(FILE_TREE_SHOW_IGNORED_STORAGE_KEY, "true")
    mocks.getFileTree.mockResolvedValue(ignoredA)
    const envelopes = createEnvelopeHarness()
    const { rerender } = renderHook(
      ({ seq }) =>
        useIgnoredFileTree({
          folderPath: "/repo",
          fallbackTree: fallbackA,
          workspaceSeq: seq,
          subscribeEnvelopes: envelopes.subscribe,
        }),
      { initialProps: { seq: 5 } }
    )
    await waitFor(() => expect(mocks.getFileTree).toHaveBeenCalledTimes(1))

    rerender({ seq: 6 })
    await waitFor(() => expect(mocks.getFileTree).toHaveBeenCalledTimes(2))

    envelopes.emit({
      seq: 7,
      kind: "meta",
      fs_event_kind: "modify",
      changed_paths: ["src/main.ts"],
    })
    rerender({ seq: 7 })
    await Promise.resolve()
    expect(mocks.getFileTree).toHaveBeenCalledTimes(2)
  })

  it("discards old-folder and disabled-mode responses", async () => {
    localStorage.setItem(FILE_TREE_SHOW_IGNORED_STORAGE_KEY, "true")
    const oldFolder = deferred<FileTreeNode[]>()
    const newFolder = deferred<FileTreeNode[]>()
    const manual = deferred<FileTreeNode[]>()
    mocks.getFileTree
      .mockReturnValueOnce(oldFolder.promise)
      .mockReturnValueOnce(newFolder.promise)
      .mockReturnValueOnce(manual.promise)
    const envelopes = createEnvelopeHarness()
    const { result, rerender } = renderHook(
      ({ folderPath, fallbackTree }) =>
        useIgnoredFileTree({
          folderPath,
          fallbackTree,
          workspaceSeq: 1,
          subscribeEnvelopes: envelopes.subscribe,
        }),
      {
        initialProps: { folderPath: "/repo-a", fallbackTree: fallbackA },
      }
    )
    await waitFor(() => expect(mocks.getFileTree).toHaveBeenCalledTimes(1))
    const firstGeneration = result.current.treeGeneration

    rerender({ folderPath: "/repo-b", fallbackTree: fallbackB })
    await waitFor(() => expect(mocks.getFileTree).toHaveBeenCalledTimes(2))
    expect(result.current.treeGeneration).not.toBe(firstGeneration)

    await act(async () => {
      oldFolder.resolve(ignoredA)
      await oldFolder.promise
    })
    expect(result.current.tree).toBe(fallbackB)
    await act(async () => {
      newFolder.resolve(ignoredB)
      await newFolder.promise
    })
    expect(result.current.tree).toBe(ignoredB)

    act(() => {
      void result.current.refresh()
    })
    await waitFor(() => expect(mocks.getFileTree).toHaveBeenCalledTimes(3))
    const enabledGeneration = result.current.treeGeneration
    act(() => result.current.setShowIgnored(false))
    expect(result.current.tree).toBe(fallbackB)
    expect(result.current.treeGeneration).not.toBe(enabledGeneration)

    await act(async () => {
      manual.resolve(ignoredA)
      await manual.promise
    })
    expect(result.current.tree).toBe(fallbackB)
  })

  it("reverts the preference and reports an initial enable failure", async () => {
    localStorage.setItem(FILE_TREE_SHOW_IGNORED_STORAGE_KEY, "true")
    mocks.getFileTree.mockRejectedValueOnce(new Error("failed"))
    const onError = vi.fn()
    const envelopes = createEnvelopeHarness()
    const { result } = renderHook(() =>
      useIgnoredFileTree({
        folderPath: "/repo",
        fallbackTree: fallbackA,
        workspaceSeq: 1,
        subscribeEnvelopes: envelopes.subscribe,
        onError,
      })
    )

    await waitFor(() => expect(result.current.showIgnored).toBe(false))
    expect(localStorage.getItem(FILE_TREE_SHOW_IGNORED_STORAGE_KEY)).toBe(
      "false"
    )
    expect(result.current.tree).toBe(fallbackA)
    expect(onError).toHaveBeenCalledTimes(1)
    expect(onError).toHaveBeenCalledWith("enable")
  })

  it("keeps the last tree and suppresses background refresh errors", async () => {
    localStorage.setItem(FILE_TREE_SHOW_IGNORED_STORAGE_KEY, "true")
    mocks.getFileTree
      .mockResolvedValueOnce(ignoredA)
      .mockRejectedValueOnce(new Error("background failed"))
    const onError = vi.fn()
    const envelopes = createEnvelopeHarness()
    const { result } = renderHook(() =>
      useIgnoredFileTree({
        folderPath: "/repo",
        fallbackTree: fallbackA,
        workspaceSeq: 1,
        subscribeEnvelopes: envelopes.subscribe,
        onError,
      })
    )
    await waitFor(() => expect(result.current.tree).toBe(ignoredA))

    envelopes.emit({
      seq: 2,
      kind: "meta",
      fs_event_kind: "create",
      changed_paths: ["dist/new.js"],
    })
    await waitFor(() => expect(mocks.getFileTree).toHaveBeenCalledTimes(2))
    await waitFor(() => expect(result.current.loading).toBe(false))
    expect(result.current.tree).toBe(ignoredA)
    expect(result.current.showIgnored).toBe(true)
    expect(onError).not.toHaveBeenCalled()
  })

  it("reports a manual refresh failure once without dropping the tree", async () => {
    localStorage.setItem(FILE_TREE_SHOW_IGNORED_STORAGE_KEY, "true")
    mocks.getFileTree
      .mockResolvedValueOnce(ignoredA)
      .mockRejectedValueOnce(new Error("manual failed"))
    const onError = vi.fn()
    const envelopes = createEnvelopeHarness()
    const { result } = renderHook(() =>
      useIgnoredFileTree({
        folderPath: "/repo",
        fallbackTree: fallbackA,
        workspaceSeq: 1,
        subscribeEnvelopes: envelopes.subscribe,
        onError,
      })
    )
    await waitFor(() => expect(result.current.tree).toBe(ignoredA))

    await act(async () => {
      await result.current.refresh()
    })
    expect(result.current.tree).toBe(ignoredA)
    expect(onError).toHaveBeenCalledTimes(1)
    expect(onError).toHaveBeenCalledWith("manual")
  })
})
