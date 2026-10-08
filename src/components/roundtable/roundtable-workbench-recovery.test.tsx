import {
  act,
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react"
import { webcrypto } from "node:crypto"
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest"
import { RoundtableWorkbench } from "@/components/roundtable/roundtable-workbench"
import { roundtableHash } from "@/lib/roundtable/api"
import type {
  RoundtableConfig,
  RoundtablePreflight,
  RoundtableProjection,
} from "@/lib/roundtable/types"
import fixture from "../../../docs/roundtable/fixtures/projection.json"

const transport = vi.hoisted(() => ({
  call: vi.fn(),
  subscribe: vi.fn(),
  onReconnect: vi.fn(),
}))
const listModelProviders = vi.hoisted(() => vi.fn())
vi.mock("@/lib/transport", () => ({ getTransport: () => transport }))
vi.mock("@/lib/api", () => ({ listModelProviders }))
vi.mock("next-intl", () => ({
  useTranslations: () => (key: string) => key,
  useLocale: () => "en",
}))

type RoomResponse = {
  projection: RoundtableProjection
  message_manifest_id: string
}
type RoomPage = {
  rooms: { room_id: string; status: string; config: RoundtableConfig }[]
  cursor: string | null
}
type Request = { request: Record<string, unknown> }

function deferred<T>() {
  let resolve!: (value: T) => void
  let reject!: (reason: Error) => void
  const promise = new Promise<T>((resolvePromise, rejectPromise) => {
    resolve = resolvePromise
    reject = rejectPromise
  })
  return { promise, resolve, reject }
}

async function roomResponse(topic = "Saved question"): Promise<RoomResponse> {
  const body: RoundtableProjection["body"] = JSON.parse(
    Buffer.from(fixture.cases[0].original_hex, "hex").toString()
  )
  body.status = "running"
  body.messages = []
  body.replay.message_memberships = []
  body.replay.config = {
    schema_version: 1,
    topic,
    workspace_id: "workspace",
    source_refs: [],
    participants: [0, 1, 2].map((ordinal) => ({
      ordinal,
      role: `Role ${ordinal}`,
      provider_ref: "provider:1",
      model: "fixed-model",
      effort: "high",
    })),
    moderator_ordinal: 0,
    strategy: { type: "phased_rounds", version: 1, critique_rounds: 2 },
    concurrency: 3,
    strict_snapshot_v1: true,
    budgets: { room_budget: "1800000", phase_budget: "450000" },
    timeouts: { attempt_timeout: "225000" },
    quotas: {
      output_byte_limit: 8192,
      input_byte_limit: 16384,
      interjection_byte_limit: 16384,
    },
  }
  return {
    projection: {
      projection_ref: { id: "projection", hash: await roundtableHash(body) },
      body,
    },
    message_manifest_id: "projection",
  }
}

function page(response: RoomResponse, ids: string[], cursor: string | null) {
  return {
    rooms: ids.map((id) => ({
      room_id: id,
      status: "running",
      config: { ...response.projection.body.replay.config!, topic: id },
    })),
    cursor,
  }
}

async function readyPreflight(config: RoundtableConfig) {
  return {
    enabled: true,
    readiness: "ready",
    tools: [],
    config_hash: await roundtableHash(config),
    network: "model_gateway_only",
    writes: "scratch_only",
    confirmed_preflight_id: "confirmed",
    source_manifests: [],
    capability: {
      recipients: config.participants.map((member) => ({
        ordinal: member.ordinal,
        provider_ref: member.provider_ref,
        model: member.model || "fixed-model",
        origin: "https://provider.test",
        agent: "codex",
        effort: member.effort ?? null,
      })),
    },
    error: null,
  } satisfies RoundtablePreflight
}

describe("roundtable workbench recovery", () => {
  let response: RoomResponse
  let poll: () => void
  let reconnect: () => void
  let unsubscribe: ReturnType<typeof vi.fn>
  let removeReconnect: ReturnType<typeof vi.fn>

  beforeEach(async () => {
    vi.stubGlobal("crypto", webcrypto)
    response = await roomResponse()
    transport.call.mockReset()
    transport.subscribe.mockReset()
    transport.onReconnect.mockReset()
    listModelProviders.mockReset()
    listModelProviders.mockResolvedValue([
      { id: 1, name: "Available provider", model: null },
    ])
    unsubscribe = vi.fn()
    removeReconnect = vi.fn()
    transport.subscribe.mockResolvedValue(unsubscribe)
    transport.onReconnect.mockImplementation((callback: () => void) => {
      reconnect = callback
      return removeReconnect
    })
    const nativeInterval = window.setInterval.bind(window)
    vi.spyOn(window, "setInterval").mockImplementation(((
      callback: TimerHandler,
      delay?: number
    ) => {
      if (delay !== 5000) return nativeInterval(callback, delay)
      poll = callback as () => void
      return 987660
    }) as typeof window.setInterval)
    transport.call.mockImplementation(
      async (command: string, args: Request) => {
        if (command === "roundtable_list") return { rooms: [], cursor: null }
        if (command === "roundtable_get" || command === "roundtable_attach")
          return response
        if (command === "roundtable_messages")
          return { messages: [], cursor: null }
        if (command === "roundtable_preflight")
          return readyPreflight(args.request.config as RoundtableConfig)
        if (command === "roundtable_detach") return null
        throw new Error(`Unexpected command: ${command}`)
      }
    )
  })

  afterEach(() => {
    cleanup()
    vi.restoreAllMocks()
    vi.unstubAllGlobals()
  })

  const renderRoom = (response: RoomResponse) =>
    render(
      <RoundtableWorkbench
        workspaceId="workspace"
        roomId={response.projection.body.room_id}
      />
    )

  it("keeps successful provider options when the room list fails", async () => {
    const original = transport.call.getMockImplementation()!
    transport.call.mockImplementation((command: string, args: Request) => {
      if (command === "roundtable_list")
        return Promise.reject(new Error("list unavailable"))
      return original(command, args)
    })
    render(<RoundtableWorkbench workspaceId="workspace" />)

    await screen.findByText("list unavailable")
    await waitFor(() =>
      expect(
        within(screen.getByRole("combobox", { name: "provider 1" })).getByRole(
          "option",
          { name: "Available provider" }
        )
      ).toBeInTheDocument()
    )
    fireEvent.change(screen.getByRole("textbox", { name: "topic" }), {
      target: { value: "A new discussion" },
    })
    expect(screen.getByRole("button", { name: "preflight" })).toBeEnabled()
  })

  it("retries provider failures without throwing away loaded discussions", async () => {
    listModelProviders.mockRejectedValueOnce(new Error("providers unavailable"))
    const original = transport.call.getMockImplementation()!
    transport.call.mockImplementation((command: string, args: Request) => {
      if (command === "roundtable_list")
        return Promise.resolve(page(response, ["Room A"], null))
      return original(command, args)
    })
    render(<RoundtableWorkbench workspaceId="workspace" />)

    await screen.findByText("providers unavailable")
    expect(screen.getByRole("link", { name: /Room A/ })).toBeInTheDocument()
    fireEvent.click(screen.getByRole("button", { name: "retryProviders" }))

    await waitFor(() =>
      expect(
        within(screen.getByRole("combobox", { name: "provider 1" })).getByRole(
          "option",
          { name: "Available provider" }
        )
      ).toHaveValue("1")
    )
    expect(screen.queryByText("providers unavailable")).not.toBeInTheDocument()
    expect(screen.getByRole("link", { name: /Room A/ })).toBeInTheDocument()
  })

  it("distinguishes a loading discussion list from a successful empty list", async () => {
    const list = deferred<RoomPage>()
    const original = transport.call.getMockImplementation()!
    transport.call.mockImplementation((command: string, args: Request) => {
      if (command === "roundtable_list") return list.promise
      return original(command, args)
    })
    render(<RoundtableWorkbench workspaceId="workspace" />)

    expect(screen.getByText("loadingRooms")).toBeInTheDocument()
    expect(screen.queryByText("noRooms")).not.toBeInTheDocument()
    await act(async () => list.resolve({ rooms: [], cursor: null }))
    expect(screen.getByText("noRooms")).toBeInTheDocument()
    expect(screen.queryByText("loadingRooms")).not.toBeInTheDocument()
  })

  it("disables room actions while the initial verified snapshot is pending", async () => {
    const read = deferred<RoomResponse>()
    const original = transport.call.getMockImplementation()!
    transport.call.mockImplementation((command: string, args: Request) => {
      if (command === "roundtable_get") return read.promise
      return original(command, args)
    })
    renderRoom(response)

    expect(screen.getByText("loading")).toBeInTheDocument()
    expect(screen.getByRole("button", { name: "preflight" })).toBeDisabled()
    expect(screen.getByRole("button", { name: "usage" })).toBeDisabled()
    await act(async () => read.resolve(response))
    await screen.findByText("Saved question")
    expect(screen.getByRole("button", { name: "preflight" })).toBeEnabled()
    expect(screen.getByRole("button", { name: "usage" })).toBeEnabled()
  })

  it("replaces failed room loading with an explicit working retry", async () => {
    let failed = false
    const original = transport.call.getMockImplementation()!
    transport.call.mockImplementation((command: string, args: Request) => {
      if (command === "roundtable_get" && !failed) {
        failed = true
        return Promise.reject(new Error("snapshot unavailable"))
      }
      return original(command, args)
    })
    renderRoom(response)

    await screen.findByText("snapshot unavailable")
    expect(screen.queryByText("loading")).not.toBeInTheDocument()
    expect(screen.getByRole("button", { name: "preflight" })).toBeDisabled()
    expect(screen.getByRole("button", { name: "usage" })).toBeDisabled()
    fireEvent.click(screen.getByRole("button", { name: "retryLoad" }))

    await screen.findByText("Saved question")
    expect(screen.queryByText("snapshot unavailable")).not.toBeInTheDocument()
    expect(screen.getByRole("button", { name: "preflight" })).toBeEnabled()
  })

  it("keeps stale room actions locked until an explicit retry verifies its snapshot", async () => {
    const recovery = deferred<RoomResponse>()
    const original = transport.call.getMockImplementation()!
    let readState: "ready" | "failed" | "recovering" = "ready"
    transport.call.mockImplementation((command: string, args: Request) => {
      if (command === "roundtable_get") {
        if (readState === "failed")
          return Promise.reject(new Error("snapshot refresh unavailable"))
        if (readState === "recovering") return recovery.promise
      }
      return original(command, args)
    })
    renderRoom(response)
    await screen.findByText("Saved question")
    fireEvent.click(screen.getByRole("button", { name: "preflight" }))
    await screen.findByText("enabled")
    fireEvent.click(screen.getByRole("checkbox", { name: "confirm" }))
    expect(screen.getByRole("button", { name: "clone" })).toBeEnabled()

    readState = "failed"
    await act(async () => poll())
    await screen.findByText("snapshot refresh unavailable")
    expect(screen.getByText("refreshFailed")).toBeInTheDocument()
    for (const name of ["preflight", "usage", "clone"]) {
      expect(screen.getByRole("button", { name })).toBeDisabled()
    }

    readState = "recovering"
    fireEvent.click(screen.getByRole("button", { name: "retryLoad" }))
    await waitFor(() =>
      expect(
        transport.call.mock.calls.filter(
          ([command]) => command === "roundtable_get"
        )
      ).toHaveLength(3)
    )
    expect(screen.getByText("Saved question")).toBeInTheDocument()
    for (const name of ["preflight", "usage", "clone"]) {
      expect.soft(screen.getByRole("button", { name })).toBeDisabled()
    }

    await act(async () => recovery.resolve(response))
    await waitFor(() =>
      expect(screen.getByRole("button", { name: "preflight" })).toBeEnabled()
    )
    expect(screen.getByRole("button", { name: "usage" })).toBeEnabled()
    expect(screen.getByRole("button", { name: "clone" })).toBeEnabled()
  })

  it("preserves an action failure after a successful background room poll", async () => {
    const original = transport.call.getMockImplementation()!
    const previous = response
    transport.call.mockImplementation((command: string, args: Request) => {
      if (command === "roundtable_pause")
        return Promise.reject(new Error("pause rejected"))
      if (command === "roundtable_events")
        return Promise.resolve({
          events: [
            {
              seq: response.projection.body.last_seq,
              schema_version: 1,
              cause: "config",
              projection_ref: response.projection.projection_ref,
            },
          ],
          cursor: null,
        })
      return original(command, args)
    })
    renderRoom(response)
    await screen.findByText("Saved question")
    fireEvent.click(screen.getByRole("button", { name: "pause" }))
    await screen.findByText("pause rejected")

    response = await roomResponse("Freshly polled question")
    response.projection.body.last_seq = String(
      BigInt(previous.projection.body.last_seq) + BigInt(1)
    )
    response.projection.body.revision = String(
      BigInt(previous.projection.body.revision) + BigInt(1)
    )
    response.projection.projection_ref.hash = await roundtableHash(
      response.projection.body
    )
    await act(async () => poll())
    await screen.findByText("Freshly polled question")

    expect(screen.getByText("pause rejected")).toBeInTheDocument()
  })

  it("preserves an action failure after a successful discussion-list refresh", async () => {
    const original = transport.call.getMockImplementation()!
    let refreshed = false
    transport.call.mockImplementation((command: string, args: Request) => {
      if (command === "roundtable_pause")
        return Promise.reject(new Error("pause rejected"))
      if (command === "roundtable_list" && refreshed)
        return Promise.resolve(page(response, ["Refreshed discussion"], null))
      return original(command, args)
    })
    renderRoom(response)
    await screen.findByText("Saved question")
    fireEvent.click(screen.getByRole("button", { name: "pause" }))
    await screen.findByText("pause rejected")

    refreshed = true
    fireEvent.click(screen.getByRole("button", { name: "refresh" }))
    await screen.findByRole("link", { name: /Refreshed discussion/ })

    expect(screen.getByText("pause rejected")).toBeInTheDocument()
  })

  it("keeps pending preflight actions locked when a faster list refresh finishes", async () => {
    const preflight = deferred<RoundtablePreflight>()
    const original = transport.call.getMockImplementation()!
    let refreshed = false
    transport.call.mockImplementation((command: string, args: Request) => {
      if (command === "roundtable_preflight") return preflight.promise
      if (command === "roundtable_list" && refreshed)
        return Promise.resolve(page(response, ["Refreshed discussion"], null))
      return original(command, args)
    })
    renderRoom(response)
    await screen.findByText("Saved question")
    fireEvent.click(screen.getByRole("button", { name: "preflight" }))
    expect(screen.getByRole("button", { name: "preflight" })).toBeDisabled()

    refreshed = true
    fireEvent.click(screen.getByRole("button", { name: "refresh" }))
    await screen.findByRole("link", { name: /Refreshed discussion/ })
    expect(screen.getByRole("button", { name: "preflight" })).toBeDisabled()
    expect(screen.getByRole("button", { name: "pause" })).toBeDisabled()

    await act(async () =>
      preflight.resolve(
        await readyPreflight(response.projection.body.replay.config!)
      )
    )
    await screen.findByText("enabled")
    expect(screen.getByRole("button", { name: "preflight" })).toBeEnabled()
  })

  it("sends only one pagination request when More is clicked repeatedly", async () => {
    const nextPage = deferred<RoomPage>()
    const original = transport.call.getMockImplementation()!
    transport.call.mockImplementation((command: string, args: Request) => {
      if (command === "roundtable_list")
        return args.request.cursor
          ? nextPage.promise
          : Promise.resolve(page(response, ["Room A"], "next"))
      return original(command, args)
    })
    render(<RoundtableWorkbench workspaceId="workspace" />)
    const more = await screen.findByRole("button", { name: "more" })
    fireEvent.click(more)
    fireEvent.click(more)

    expect(more).toBeDisabled()
    expect(
      transport.call.mock.calls.filter(
        ([command, args]) =>
          command === "roundtable_list" && args.request.cursor === "next"
      )
    ).toHaveLength(1)
    await act(async () => nextPage.resolve(page(response, ["Room B"], null)))
    expect(screen.getByRole("link", { name: /Room B/ })).toBeInTheDocument()
    expect(
      screen.queryByRole("button", { name: "more" })
    ).not.toBeInTheDocument()
  })

  it("deduplicates discussion IDs shared by adjacent list pages", async () => {
    const original = transport.call.getMockImplementation()!
    transport.call.mockImplementation((command: string, args: Request) => {
      if (command === "roundtable_list")
        return Promise.resolve(
          args.request.cursor
            ? page(response, ["Room A", "Room B"], null)
            : page(response, ["Room A"], "next")
        )
      return original(command, args)
    })
    render(<RoundtableWorkbench workspaceId="workspace" />)
    fireEvent.click(await screen.findByRole("button", { name: "more" }))
    await screen.findByRole("link", { name: /Room B/ })

    expect(screen.getAllByRole("link", { name: /Room A/ })).toHaveLength(1)
    expect(screen.getAllByRole("link", { name: /Room B/ })).toHaveLength(1)
  })

  it("still reads the initial snapshot when live-update attachment fails", async () => {
    const original = transport.call.getMockImplementation()!
    transport.call.mockImplementation((command: string, args: Request) => {
      if (command === "roundtable_attach")
        return Promise.reject(new Error("attach unavailable"))
      return original(command, args)
    })
    renderRoom(response)

    await screen.findByText("Saved question")
    expect(screen.getByText("liveUpdatesUnavailable")).toBeInTheDocument()
    expect(screen.getByRole("button", { name: "preflight" })).toBeEnabled()
  })

  it("reads a fresh snapshot after reconnect even when reattachment fails", async () => {
    let reconnected = false
    const original = transport.call.getMockImplementation()!
    transport.call.mockImplementation((command: string, args: Request) => {
      if (command === "roundtable_attach" && reconnected)
        return Promise.reject(new Error("reattach unavailable"))
      return original(command, args)
    })
    renderRoom(response)
    await screen.findByText("Saved question")

    response = await roomResponse("Recovered question")
    reconnected = true
    await act(async () => reconnect())

    await screen.findByText("Recovered question")
    expect(screen.getByText("liveUpdatesUnavailable")).toBeInTheDocument()
  })

  it("ignores subscription and reconnect callbacks that arrive after unmount", async () => {
    const subscription = deferred<() => void>()
    transport.subscribe.mockReturnValue(subscription.promise)
    const view = renderRoom(response)
    view.unmount()
    const startedReads = transport.call.mock.calls.filter(([command]) =>
      ["roundtable_attach", "roundtable_get"].includes(command)
    ).length

    await act(async () => {
      reconnect()
      subscription.resolve(unsubscribe)
    })

    expect(unsubscribe).toHaveBeenCalledTimes(1)
    expect(removeReconnect).toHaveBeenCalledTimes(1)
    expect(
      transport.call.mock.calls.filter(([command]) =>
        ["roundtable_attach", "roundtable_get"].includes(command)
      )
    ).toHaveLength(startedReads)
  })

  it("ignores a late snapshot after unmount and a replacement room mounts", async () => {
    const lateRead = deferred<RoomResponse>()
    const original = transport.call.getMockImplementation()!
    const previous = response
    transport.call.mockImplementation((command: string, args: Request) => {
      if (
        command === "roundtable_get" &&
        args.request.room_id === previous.projection.body.room_id
      )
        return lateRead.promise
      return original(command, args)
    })
    const view = renderRoom(previous)
    await waitFor(() =>
      expect(transport.call).toHaveBeenCalledWith("roundtable_get", {
        request: { room_id: previous.projection.body.room_id },
      })
    )
    view.unmount()

    response = await roomResponse("Replacement question")
    response.projection.body.room_id = "replacement-room"
    response.projection.projection_ref.hash = await roundtableHash(
      response.projection.body
    )
    renderRoom(response)
    await screen.findByText("Replacement question")
    await act(async () => lateRead.resolve(previous))
    await waitFor(() =>
      expect(transport.call).toHaveBeenCalledWith("roundtable_messages", {
        request: {
          room_id: previous.projection.body.room_id,
          manifest_id: previous.message_manifest_id,
        },
      })
    )

    expect(screen.getByText("Replacement question")).toBeInTheDocument()
    expect(screen.queryByText("Saved question")).not.toBeInTheDocument()
  })
})
