import { act, renderHook } from "@testing-library/react"
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest"

import type { WorkspaceFileEntry } from "@/lib/types"
import { useWorkspaceFileSearch } from "./use-workspace-file-search"

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
  searchWorkspaceFiles: vi.fn(),
  randomUUID: vi.fn(),
  uuidCounter: 0,
}))

vi.mock("@/lib/api", () => ({
  searchWorkspaceFiles: (...args: unknown[]) =>
    mocks.searchWorkspaceFiles(...args),
}))

vi.mock("@/lib/utils", async (importOriginal) => ({
  ...(await importOriginal<typeof import("@/lib/utils")>()),
  randomUUID: () => mocks.randomUUID(),
}))

function result(path: string): WorkspaceFileEntry[] {
  const name = path.split("/").pop() ?? path
  return [{ name, path, kind: "file" }]
}

function signalOf(call: unknown[]): AbortSignal {
  const options = call[2] as { signal: AbortSignal }
  return options.signal
}

describe("useWorkspaceFileSearch", () => {
  beforeEach(() => {
    vi.useFakeTimers()
    mocks.searchWorkspaceFiles.mockReset()
    mocks.uuidCounter = 0
    mocks.randomUUID.mockReset().mockImplementation(() => {
      mocks.uuidCounter += 1
      return `uuid-${mocks.uuidCounter}`
    })
  })

  afterEach(() => {
    vi.useRealTimers()
  })

  it("hides settled rows immediately when the query changes", async () => {
    mocks.searchWorkspaceFiles.mockResolvedValueOnce(result("foo.ts"))
    const { result: hook, rerender } = renderHook(
      ({ query }) =>
        useWorkspaceFileSearch({
          folderPath: "/repo",
          query,
          enabled: true,
          limit: 100,
          debounceMs: 200,
        }),
      { initialProps: { query: "foo" } }
    )

    await act(async () => {
      await vi.advanceTimersByTimeAsync(200)
    })
    expect(hook.current.files.map((file) => file.relativePath)).toEqual([
      "foo.ts",
    ])
    expect(hook.current.loading).toBe(false)

    rerender({ query: "bar" })
    expect(hook.current.files).toEqual([])
    expect(hook.current.loading).toBe(true)
    expect(mocks.searchWorkspaceFiles).toHaveBeenCalledTimes(1)
  })

  it("does not revive cached rows when a query cycles back", async () => {
    mocks.searchWorkspaceFiles.mockResolvedValueOnce(result("foo.ts"))
    const { result: hook, rerender } = renderHook(
      ({ query }) =>
        useWorkspaceFileSearch({
          folderPath: "/repo",
          query,
          enabled: true,
          debounceMs: 200,
        }),
      { initialProps: { query: "foo" } }
    )

    await act(async () => {
      await vi.advanceTimersByTimeAsync(200)
    })
    expect(hook.current.files).toHaveLength(1)

    rerender({ query: "bar" })
    rerender({ query: "foo" })
    expect(hook.current.files).toEqual([])
    expect(hook.current.loading).toBe(true)
  })

  it("cancels the old request and ignores its late resolution", async () => {
    const first = deferred<WorkspaceFileEntry[]>()
    const second = deferred<WorkspaceFileEntry[]>()
    mocks.searchWorkspaceFiles
      .mockReturnValueOnce(first.promise)
      .mockReturnValueOnce(second.promise)
    const { result: hook, rerender } = renderHook(
      ({ query }) =>
        useWorkspaceFileSearch({
          folderPath: "/repo",
          query,
          enabled: true,
          limit: 100,
          debounceMs: 200,
        }),
      { initialProps: { query: "foo" } }
    )

    await act(async () => {
      await vi.advanceTimersByTimeAsync(200)
    })
    const firstSignal = signalOf(mocks.searchWorkspaceFiles.mock.calls[0])
    expect(mocks.searchWorkspaceFiles).toHaveBeenNthCalledWith(
      1,
      "/repo",
      "foo",
      expect.objectContaining({
        limit: 100,
        request: { consumerId: "uuid-1", generation: 1 },
        signal: firstSignal,
      })
    )

    rerender({ query: "bar" })
    expect(hook.current.files).toEqual([])
    expect(hook.current.loading).toBe(true)
    expect(firstSignal.aborted).toBe(true)

    await act(async () => {
      first.resolve(result("foo.ts"))
      await Promise.resolve()
    })
    expect(hook.current.files).toEqual([])

    await act(async () => {
      await vi.advanceTimersByTimeAsync(200)
    })
    const secondSignal = signalOf(mocks.searchWorkspaceFiles.mock.calls[1])
    expect(mocks.searchWorkspaceFiles).toHaveBeenNthCalledWith(
      2,
      "/repo",
      "bar",
      expect.objectContaining({
        limit: 100,
        request: { consumerId: "uuid-1", generation: 3 },
        signal: secondSignal,
      })
    )

    await act(async () => {
      second.resolve(result("bar.ts"))
      await Promise.resolve()
    })
    expect(hook.current.files.map((file) => file.relativePath)).toEqual([
      "bar.ts",
    ])
  })

  it("settles a current-query failure to an empty non-loading result", async () => {
    mocks.searchWorkspaceFiles.mockRejectedValueOnce(new Error("failed"))
    const { result: hook } = renderHook(() =>
      useWorkspaceFileSearch({
        folderPath: "/repo",
        query: "broken",
        enabled: true,
        debounceMs: 200,
      })
    )

    await act(async () => {
      await vi.advanceTimersByTimeAsync(200)
    })
    expect(hook.current.files).toEqual([])
    expect(hook.current.loading).toBe(false)
  })

  it("isolates sessions and cancels each active request on unmount", async () => {
    const first = deferred<WorkspaceFileEntry[]>()
    const second = deferred<WorkspaceFileEntry[]>()
    mocks.searchWorkspaceFiles
      .mockReturnValueOnce(first.promise)
      .mockReturnValueOnce(second.promise)
    const a = renderHook(() =>
      useWorkspaceFileSearch({
        folderPath: "/repo",
        query: "a",
        enabled: true,
        debounceMs: 200,
      })
    )
    const b = renderHook(() =>
      useWorkspaceFileSearch({
        folderPath: "/repo",
        query: "b",
        enabled: true,
        debounceMs: 200,
      })
    )

    await act(async () => {
      await vi.advanceTimersByTimeAsync(200)
    })
    const firstCall = mocks.searchWorkspaceFiles.mock.calls[0][2] as {
      request: { consumerId: string }
      signal: AbortSignal
    }
    const secondCall = mocks.searchWorkspaceFiles.mock.calls[1][2] as {
      request: { consumerId: string }
      signal: AbortSignal
    }
    expect(firstCall.request.consumerId).toBe("uuid-1")
    expect(secondCall.request.consumerId).toBe("uuid-2")

    a.unmount()
    b.unmount()
    expect(firstCall.signal.aborted).toBe(true)
    expect(secondCall.signal.aborted).toBe(true)

    first.resolve(result("a.ts"))
    second.resolve(result("b.ts"))
    await Promise.all([first.promise, second.promise])
  })
})
