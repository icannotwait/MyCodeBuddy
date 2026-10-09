import { act, fireEvent, render, screen, waitFor } from "@testing-library/react"
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest"
import { RoundtableWorkbench } from "@/components/roundtable/roundtable-workbench"
import fixture from "../../../docs/roundtable/fixtures/projection.json"
import { roundtableHash } from "@/lib/roundtable/api"
import { webcrypto } from "node:crypto"

const call = vi.hoisted(() => vi.fn())
const subscribers = vi.hoisted(() => [] as ((payload: unknown) => void)[])
vi.mock("@/lib/transport", () => ({
  getTransport: () => ({
    call,
    subscribe: async (
      _channel: string,
      handler: (payload: unknown) => void
    ) => {
      subscribers.push(handler)
      return () => undefined
    },
  }),
}))
const folders = vi.hoisted(() => [
  {
    id: 7,
    name: "project",
    path: "/repo/project",
    kind: "regular",
    parent_id: null,
    alias: null,
  },
  {
    id: 9,
    name: "other",
    path: "/repo/other",
    kind: "regular",
    parent_id: null,
    alias: "Other",
  },
  {
    id: 11,
    name: "chat",
    path: "/tmp/chat",
    kind: "chat",
    parent_id: null,
    alias: null,
  },
])
const listAllFolderDetails = vi.hoisted(() => vi.fn(async () => folders))
vi.mock("@/lib/api", () => ({
  listModelProviders: async () => [{ id: 1, name: "Provider", model: null }],
  listAllFolderDetails,
}))
vi.mock("next-intl", () => ({
  useTranslations: () => (key: string) => key,
  useLocale: () => "en",
}))

describe("roundtable product page", () => {
  beforeEach(() => {
    call.mockReset()
    subscribers.length = 0
    listAllFolderDetails.mockReset()
    listAllFolderDetails.mockResolvedValue(folders)
    vi.stubGlobal("crypto", webcrypto)
  })

  afterEach(() => vi.restoreAllMocks())

  async function room(
    status: string,
    reverseParticipants = false,
    seed?: (body: {
      messages: { message_id: string; hash: string }[]
      phase_refs: unknown[]
      replay: Record<string, unknown>
    }) => Promise<
      {
        message_id: string
        body_hash: string
        visibility: "staged" | "published" | "void"
        speaker_id?: string
        attempt_id?: string
        phase_id?: string
        attempt_no?: number
        attempt_state?: string
        finished_at?: string
        body: Record<string, unknown>
      }[]
    >
  ) {
    const body = JSON.parse(
      Buffer.from(fixture.cases[0].original_hex, "hex").toString()
    )
    body.status = status
    body.messages = []
    body.replay.message_memberships = []
    body.replay.config = {
      schema_version: 1,
      display_name: "Saved label",
      topic: "Saved question",
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
    if (reverseParticipants) body.replay.config.participants.reverse()
    const pageMessages = seed ? await seed(body) : []
    const projection = {
      projection_ref: { id: "projection", hash: await roundtableHash(body) },
      body,
    }
    call.mockImplementation(async (command: string) => {
      if (command === "roundtable_list") return { rooms: [], cursor: null }
      if (command === "roundtable_get" || command === "roundtable_attach")
        return { projection, message_manifest_id: "projection" }
      if (command === "roundtable_messages")
        return { messages: pageMessages, cursor: null }
      if (command === "roundtable_preflight")
        return {
          enabled: true,
          readiness: "ready",
          tools: [],
          config_hash: await roundtableHash(body.replay.config),
          network: "model_gateway_only",
          writes: "scratch_only",
          confirmed_preflight_id: "confirmed",
          source_manifests: [],
          capability: {
            recipients: body.replay.config.participants.map(
              (member: {
                ordinal: number
                provider_ref: string
                model: string
                effort: string
              }) => ({
                ordinal: member.ordinal,
                provider_ref: member.provider_ref,
                model: member.model,
                origin: "https://provider.test",
                agent: "codex",
                effort: member.effort,
              })
            ),
          },
          error: null,
        }
      return { room_id: body.room_id, operation_id: null }
    })
    return body
  }

  it("reuses the original body and UUID after a lost reply and projection poll", async () => {
    const body = await room("running")
    const initial = call.getMockImplementation()!
    const nextBody = {
      ...body,
      revision: String(BigInt(body.revision) + BigInt(1)),
      last_seq: String(BigInt(body.last_seq) + BigInt(1)),
    }
    const nextProjection = {
      body: nextBody,
      projection_ref: { id: "polled", hash: await roundtableHash(nextBody) },
    }
    let polled = false
    let poll: () => void = () => undefined
    const nativeInterval = window.setInterval.bind(window)
    vi.spyOn(window, "setInterval").mockImplementation(((
      callback: TimerHandler,
      delay?: number
    ) => {
      if (delay !== 5000) return nativeInterval(callback, delay)
      poll = callback as () => void
      return 987654
    }) as typeof window.setInterval)
    const requests: Record<string, unknown>[] = []
    call.mockImplementation(
      async (command: string, args: { request: Record<string, unknown> }) => {
        if (command === "roundtable_interject") {
          requests.push(args.request)
          if (requests.length <= 2) throw new Error("reply_lost")
          return { room_id: body.room_id, operation_id: null }
        }
        if (polled && command === "roundtable_get")
          return { projection: nextProjection, message_manifest_id: "polled" }
        if (command === "roundtable_events")
          return {
            events: [
              {
                seq: nextBody.last_seq,
                schema_version: 1,
                cause: "input",
                projection_ref: nextProjection.projection_ref,
              },
            ],
            cursor: null,
          }
        return initial(command, args)
      }
    )
    render(
      <RoundtableWorkbench workspaceId="workspace" roomId={body.room_id} />
    )
    await screen.findByText("Saved question")
    const input = screen.getByRole("textbox")
    fireEvent.change(input, { target: { value: "Frozen retry text" } })
    fireEvent.click(screen.getByRole("button", { name: "send" }))
    await screen.findByText("reply_lost")
    polled = true
    await act(async () => {
      poll()
    })
    await waitFor(() =>
      expect(call).toHaveBeenCalledWith("roundtable_get", {
        request: {
          room_id: body.room_id,
          read: { projection: { projection_id: "polled" } },
        },
      })
    )
    expect(screen.getByText("reply_lost")).toBeInTheDocument()
    fireEvent.click(screen.getByRole("button", { name: "send" }))
    await screen.findByText("reply_lost")
    expect(requests).toHaveLength(2)
    expect(requests[1]).toEqual(requests[0])
    expect(requests[1].expected_revision).toBe(body.revision)
    fireEvent.change(input, { target: { value: "New intentional input" } })
    fireEvent.click(screen.getByRole("button", { name: "send" }))
    await waitFor(() => expect(requests).toHaveLength(3))
    expect(requests[2].request_id).not.toBe(requests[0].request_id)
    expect(requests[2].expected_revision).toBe(nextBody.revision)
  })

  it.each(["roundtable_pause", "roundtable_stop"] as const)(
    "preserves lost paid ACK replay after projection advance, preflight and %s",
    async (cancelCommand) => {
      const body = await room("running")
      const initial = call.getMockImplementation()!
      const nextBody = {
        ...body,
        revision: String(BigInt(body.revision) + BigInt(1)),
        last_seq: String(BigInt(body.last_seq) + BigInt(1)),
      }
      const nextProjection = {
        body: nextBody,
        projection_ref: {
          id: "paid-polled",
          hash: await roundtableHash(nextBody),
        },
      }
      let polled = false
      let poll: () => void = () => undefined
      const nativeInterval = window.setInterval.bind(window)
      vi.spyOn(window, "setInterval").mockImplementation(((
        callback: TimerHandler,
        delay?: number
      ) => {
        if (delay !== 5000) return nativeInterval(callback, delay)
        poll = callback as () => void
        return 987655
      }) as typeof window.setInterval)
      const requests: Record<string, unknown>[] = []
      const dispatches = new Set<unknown>()
      let checks = 0
      call.mockImplementation(
        async (command: string, args: { request: Record<string, unknown> }) => {
          if (command === "roundtable_preflight")
            return {
              ...(await initial(command, args)),
              confirmed_preflight_id: `P${++checks}`,
            }
          if (command === "roundtable_interject") {
            requests.push(JSON.parse(JSON.stringify(args.request)))
            dispatches.add(args.request.request_id)
            if (requests.length === 1) throw new Error("paid_ack_lost")
            return { room_id: body.room_id, operation_id: null }
          }
          if (polled && command === "roundtable_get")
            return {
              projection: nextProjection,
              message_manifest_id: "paid-polled",
            }
          if (command === "roundtable_events")
            return {
              events: [
                {
                  seq: nextBody.last_seq,
                  schema_version: 1,
                  cause: "control",
                  projection_ref: nextProjection.projection_ref,
                },
              ],
              cursor: null,
            }
          return initial(command, args)
        }
      )
      const mounted = render(
        <RoundtableWorkbench workspaceId="workspace" roomId={body.room_id} />
      )
      await screen.findByText("Saved question")
      fireEvent.change(screen.getByRole("textbox"), {
        target: { value: "Frozen paid intent" },
      })
      fireEvent.change(screen.getByLabelText("interjectionMode"), {
        target: { value: "restart_current" },
      })
      fireEvent.click(screen.getByRole("button", { name: "preflight" }))
      await screen.findByText("enabled")
      fireEvent.click(screen.getByLabelText("confirm"))
      fireEvent.click(screen.getByRole("button", { name: "send" }))
      await screen.findByText("paid_ack_lost")
      polled = true
      await act(async () => {
        poll()
      })
      await waitFor(() =>
        expect(call).toHaveBeenCalledWith("roundtable_get", {
          request: {
            room_id: body.room_id,
            read: { projection: { projection_id: "paid-polled" } },
          },
        })
      )
      fireEvent.click(screen.getByRole("button", { name: "preflight" }))
      await waitFor(() => expect(checks).toBe(2))
      await screen.findByText("enabled")
      fireEvent.click(
        screen.getByRole("button", {
          name: cancelCommand === "roundtable_pause" ? "pause" : "stop",
        })
      )
      await waitFor(() =>
        expect(call).toHaveBeenCalledWith(cancelCommand, {
          request: expect.objectContaining({
            room_id: body.room_id,
            expected_revision: nextBody.revision,
          }),
        })
      )
      await waitFor(() =>
        expect(
          screen.getByRole("button", { name: "retryPendingOperation" })
        ).toBeEnabled()
      )
      mounted.unmount()
      render(
        <RoundtableWorkbench workspaceId="workspace" roomId={body.room_id} />
      )
      await screen.findByText("Saved question")
      fireEvent.click(
        screen.getByRole("button", { name: "retryPendingOperation" })
      )
      await waitFor(() => expect(requests).toHaveLength(2))
      expect(requests[1]).toEqual(requests[0])
      expect(requests[1].confirmed_preflight_id).toBe("P1")
      expect(requests[1].expected_revision).toBe(body.revision)
      expect(dispatches.size).toBe(1)
      await waitFor(() =>
        expect(
          screen.queryByRole("button", { name: "retryPendingOperation" })
        ).toBeNull()
      )
    }
  )

  it("updates a saved draft with its expected revision and retains its configuration", async () => {
    const body = await room("draft")
    render(
      <RoundtableWorkbench workspaceId="workspace" roomId={body.room_id} />
    )
    await screen.findByText("Saved question")
    fireEvent.click(screen.getByRole("button", { name: "editDraft" }))
    fireEvent.change(screen.getByLabelText("topic"), {
      target: { value: "Revised question" },
    })
    fireEvent.click(screen.getByRole("button", { name: "saveDraft" }))
    await waitFor(() =>
      expect(call).toHaveBeenCalledWith("roundtable_update_draft", {
        request: expect.objectContaining({
          room_id: body.room_id,
          expected_revision: body.revision,
          config: expect.objectContaining({
            topic: "Revised question",
            display_name: "Saved label",
            source_refs: [],
            participants: body.replay.config.participants.map(
              (member: { agent?: string }) => ({
                ...member,
                agent: member.agent || "codex",
              })
            ),
          }),
        }),
      })
    )
  })

  it("keeps ordinal identity when editing a legally permuted draft", async () => {
    const body = await room("draft", true)
    render(
      <RoundtableWorkbench workspaceId="workspace" roomId={body.room_id} />
    )
    await screen.findByText("Saved question")
    fireEvent.click(screen.getByRole("button", { name: "editDraft" }))
    expect(screen.getByLabelText("role 1")).toHaveValue("Role 0")
    expect(screen.getByLabelText("role 3")).toHaveValue("Role 2")
  })

  it("requires separate recovery consent after budget confirmation", async () => {
    const body = await room("paused")
    render(
      <RoundtableWorkbench workspaceId="workspace" roomId={body.room_id} />
    )
    await screen.findByText("Saved question")
    fireEvent.click(screen.getByRole("button", { name: "preflight" }))
    await screen.findByText("enabled")
    fireEvent.click(screen.getByLabelText("confirm"))
    expect(screen.getByRole("button", { name: "resume" })).toBeDisabled()
    fireEvent.click(screen.getByLabelText("recoveryConsent"))
    expect(screen.getByRole("button", { name: "resume" })).toBeEnabled()
  })
  it("preflights actual config and blocks creation when execution is disabled", async () => {
    call.mockImplementation(
      async (command: string, args: { request: { config: unknown } }) =>
        command === "roundtable_list"
          ? { rooms: [], cursor: null }
          : {
              enabled: false,
              readiness: "ready",
              tools: [],
              network: "model_gateway_only",
              writes: "scratch_only",
              confirmed_preflight_id: "fixed",
              config_hash: await roundtableHash(args.request.config),
              error: null,
            }
    )
    render(<RoundtableWorkbench workspaceId="7" />)
    fireEvent.change(screen.getByLabelText("topic"), {
      target: { value: "Question" },
    })
    fireEvent.click(screen.getByRole("button", { name: "addMember" }))
    await waitFor(() =>
      expect(screen.getAllByRole("option", { name: "Provider" })).toHaveLength(
        3
      )
    )
    fireEvent.click(screen.getByRole("radio", { name: "moderator 3" }))
    fireEvent.change(screen.getByRole("combobox", { name: "provider 1" }), {
      target: { value: "1" },
    })
    fireEvent.click(screen.getByRole("button", { name: "preflight" }))
    await screen.findByText("disabled")
    expect(screen.getByRole("button", { name: "create" })).toBeDisabled()
    expect(call).toHaveBeenCalledWith("roundtable_preflight", {
      request: {
        config: expect.objectContaining({
          topic: "Question",
          workspace_id: "7",
          participants: [
            expect.objectContaining({
              ordinal: 0,
              provider_ref: "provider:1",
              agent: "grok",
            }),
            // Without an explicit provider, a seat uses its agent's
            // qualified default binding.
            expect.objectContaining({
              ordinal: 1,
              provider_ref: "provider:antigravity",
              agent: "antigravity",
            }),
            expect.objectContaining({
              ordinal: 2,
              provider_ref: "provider:cursor",
              agent: "cursor",
            }),
          ],
          moderator_ordinal: 2,
        }),
      },
    })
    expect(
      call.mock.calls.some(([command]) => command === "roundtable_create")
    ).toBe(false)
  })

  it("shows a verified message in the transcript with its phase and member", async () => {
    const summary = "Prepaid slices stay elapsed."
    const loaded = await room("completed", false, async (body) => {
      ;(body.replay.config as { topic: string }).topic =
        "Store tokens in `HttpOnly` cookies?"
      const message = { kind: "proposal", summary, claims: [] }
      const hash = await roundtableHash(message)
      body.messages = [{ message_id: "message-1", hash }]
      body.phase_refs = [
        {
          phase_id: "phase-proposal",
          revision: "1",
          state: "published",
          index: 0,
          kind: "proposal",
        },
      ]
      body.replay.speakers = [
        {
          speaker_id: "speaker-1",
          ordinal: 0,
          role: "member",
          provider_ref: "provider:grok",
          model_id: "grok-4.6",
        },
      ]
      body.replay.turns = [
        {
          turn_id: "turn-1",
          phase_id: "phase-proposal",
          speaker_id: "speaker-1",
          accepted_attempt_id: "attempt-1",
        },
      ]
      body.replay.attempts = [
        {
          attempt_id: "attempt-1",
          state: "accepted",
          turn_id: "turn-1",
          attempt_no: 1,
        },
      ]
      body.replay.message_memberships = [
        {
          message_id: "message-1",
          membership_version: "2",
          visibility: "published",
          published_seq: "4",
        },
        {
          message_id: "message-1",
          membership_version: "1",
          visibility: "staged",
          published_seq: null,
        },
      ]
      return [
        {
          message_id: "message-1",
          body_hash: hash,
          visibility: "published" as const,
          speaker_id: "speaker-1",
          attempt_id: "attempt-1",
          phase_id: "phase-proposal",
          attempt_no: 1,
          attempt_state: "accepted",
          finished_at: "2026-10-08T03:41:00.000Z",
          body: message,
        },
      ]
    })
    render(
      <RoundtableWorkbench workspaceId="workspace" roomId={loaded.room_id} />
    )
    const transcript = await screen.findByTestId("roundtable-transcript")
    expect(transcript).toHaveTextContent("phaseProposal")
    expect(transcript).toHaveTextContent(summary)
    expect(transcript).toHaveTextContent("member 1")
    expect(transcript).toHaveTextContent("grok-4.6")
    expect(transcript).not.toHaveTextContent("staged")
    const title = screen
      .getAllByRole("heading", { level: 2 })
      .find((node) => node.textContent?.includes("HttpOnly"))
    expect(title?.querySelector("code")).toHaveTextContent("HttpOnly")
    expect(title).not.toHaveTextContent("`")
  })

  it("streams unverified live output and re-attaches when a delta is lost", async () => {
    const loaded = await room("running", false, async (body) => {
      body.phase_refs = [
        {
          phase_id: "phase-proposal",
          revision: "1",
          state: "running",
          index: 0,
          kind: "proposal",
        },
      ]
      body.replay.speakers = [
        {
          speaker_id: "speaker-1",
          ordinal: 0,
          role: "member",
          provider_ref: "provider:grok",
          model_id: "grok-4.6",
        },
      ]
      body.replay.turns = [
        {
          turn_id: "turn-1",
          phase_id: "phase-proposal",
          speaker_id: "speaker-1",
          accepted_attempt_id: null,
        },
      ]
      body.replay.attempts = [
        {
          attempt_id: "attempt-1",
          state: "streaming",
          turn_id: "turn-1",
          attempt_no: 1,
        },
      ]
      return []
    })
    render(
      <RoundtableWorkbench workspaceId="workspace" roomId={loaded.room_id} />
    )
    await screen.findByTestId("roundtable-transcript")
    await waitFor(() => expect(subscribers.length).toBeGreaterThan(0))
    const push = (offset: number, text: string) =>
      act(() => {
        for (const handler of subscribers)
          handler({
            type: "roundtable_live",
            room_id: loaded.room_id,
            verified: false,
            removed: [],
            attempts: [
              {
                attempt_id: "attempt-1",
                speaker_id: "speaker-1",
                phase_id: "phase-proposal",
                phase_kind: "proposal",
                incarnation: "inc",
                ended: false,
                activity: null,
                message: { offset, text, truncated: false },
                thought: { offset: 0, text: "", start: 0 },
              },
            ],
          })
      })
    push(0, "Live draft")
    push(10, " grows")
    expect(await screen.findByTestId("live-text")).toHaveTextContent(
      "Live draft grows"
    )
    expect(screen.getByTestId("live-badge")).toHaveTextContent("liveBadge")
    const attaches = () =>
      call.mock.calls.filter(([command]) => command === "roundtable_attach")
        .length
    const before = attaches()
    push(99, "lost")
    await waitFor(() => expect(attaches()).toBe(before + 1))
    expect(screen.queryByTestId("live-text")).toBeNull()
  })

  it("labels an unpinned draft seat by its agent instead of default", async () => {
    const body = await room("draft", false, async (draft) => {
      const config = draft.replay.config as {
        participants: { model?: string; agent?: string }[]
      }
      for (const member of config.participants) {
        delete member.model
        member.agent = "grok"
      }
      draft.replay.speakers = [
        {
          speaker_id: "speaker-0",
          ordinal: 0,
          role: "member",
          provider_ref: "provider:grok",
          model_id: "default",
        },
        {
          speaker_id: "speaker-m",
          ordinal: 3,
          role: "moderator",
          provider_ref: "provider:grok",
          model_id: "default",
        },
      ]
      return []
    })
    render(
      <RoundtableWorkbench workspaceId="workspace" roomId={body.room_id} />
    )
    expect(
      (await screen.findAllByText("member 1 · Grok")).length
    ).toBeGreaterThan(0)
    expect(screen.getByText("moderator · Grok")).toBeInTheDocument()
    expect(screen.queryByText(/· default$/)).not.toBeInTheDocument()
  })
})

it("includes only explicit relative source selections and invalidates changed selections", async () => {
  call.mockReset()
  vi.stubGlobal("crypto", webcrypto)
  call.mockImplementation(
    async (
      command: string,
      args: {
        request: {
          config: { participants: { ordinal: number; provider_ref: string }[] }
        }
      }
    ) => {
      if (command === "roundtable_list") return { rooms: [], cursor: null }
      if (command === "roundtable_preflight")
        return {
          enabled: true,
          readiness: "ready",
          tools: [],
          network: "model_gateway_only",
          writes: "scratch_only",
          config_hash: await roundtableHash(args.request.config),
          error: null,
          source_manifests: [],
          confirmed_preflight_id: null,
          capability: {
            recipients: args.request.config.participants.map(
              ({ ordinal, provider_ref }) => ({
                ordinal,
                provider_ref,
                model: "resolved-model",
                origin: "https://example.test",
                agent: "grok",
                effort: null,
              })
            ),
          },
        }
      if (command === "roundtable_create")
        throw new Error("draft-created-test-stop")
    }
  )
  render(<RoundtableWorkbench workspaceId="7" />)
  fireEvent.change(screen.getByLabelText("topic"), {
    target: { value: "Review selected source" },
  })
  fireEvent.change(screen.getByLabelText("selectedSourcePaths"), {
    target: { value: "src/a.ts\nREADME.md" },
  })
  await waitFor(() =>
    expect(screen.getAllByRole("option", { name: "Provider" })).toHaveLength(2)
  )
  fireEvent.click(screen.getByRole("button", { name: "preflight" }))
  await screen.findByText("enabled")
  expect(
    screen.getAllByText("resolved-model", { exact: false }).length
  ).toBeGreaterThan(0)
  fireEvent.click(screen.getByLabelText("confirm"))
  expect(screen.getByRole("button", { name: "create" })).toBeEnabled()
  fireEvent.change(screen.getByLabelText("selectedSourcePaths"), {
    target: { value: "src/a.ts" },
  })
  expect(screen.getByRole("button", { name: "create" })).toBeDisabled()
  fireEvent.click(screen.getByRole("button", { name: "preflight" }))
  await screen.findByText("enabled")
  fireEvent.click(screen.getByLabelText("confirm"))
  fireEvent.click(screen.getByRole("button", { name: "create" }))
  await screen.findByText("draft-created-test-stop")
  expect(call).toHaveBeenCalledWith("roundtable_create", {
    request: expect.objectContaining({
      selected_source_paths: ["src/a.ts"],
      config: expect.objectContaining({ source_refs: [] }),
    }),
  })
})

it("does not present a late preflight as confirmation for an edited selection", async () => {
  call.mockReset()
  vi.stubGlobal("crypto", webcrypto)
  let release: (value: unknown) => void = () => undefined
  let checkedConfig: unknown
  call.mockImplementation(
    async (command: string, args: { request: { config: unknown } }) => {
      if (command === "roundtable_list") return { rooms: [], cursor: null }
      checkedConfig = args.request.config
      return new Promise((resolve) => {
        release = resolve
      })
    }
  )
  render(<RoundtableWorkbench workspaceId="7" />)
  fireEvent.change(screen.getByLabelText("topic"), {
    target: { value: "Original" },
  })
  await waitFor(() =>
    expect(screen.getAllByRole("option", { name: "Provider" })).toHaveLength(2)
  )
  fireEvent.click(screen.getByRole("button", { name: "preflight" }))
  await waitFor(() => expect(checkedConfig).toBeTruthy())
  fireEvent.change(screen.getByLabelText("selectedSourcePaths"), {
    target: { value: "new.ts" },
  })
  await act(async () => {
    release({
      enabled: true,
      readiness: "ready",
      config_hash: await roundtableHash(checkedConfig),
      source_manifests: [],
      capability: { recipients: [] },
      tools: [],
      network: "none",
      writes: "none",
      error: null,
    })
  })
  expect(screen.queryByLabelText("confirm")).toBeNull()
  expect(screen.getByRole("button", { name: "create" })).toBeDisabled()
})

it("removes a specific member card and keeps the moderator on the same seat", async () => {
  call.mockReset()
  vi.stubGlobal("crypto", webcrypto)
  let checkedConfig:
    | {
        participants: { ordinal: number; role: string; agent: string }[]
        moderator_ordinal: number
        concurrency: number
      }
    | undefined
  call.mockImplementation(
    async (command: string, args: { request: { config: unknown } }) => {
      if (command === "roundtable_list") return { rooms: [], cursor: null }
      checkedConfig = args.request.config as typeof checkedConfig
      return new Promise(() => undefined)
    }
  )
  render(<RoundtableWorkbench workspaceId="7" />)
  expect(
    screen.queryByRole("button", { name: "removeMember 1" })
  ).not.toBeInTheDocument()
  fireEvent.click(screen.getByRole("button", { name: "addMember" }))
  fireEvent.change(screen.getByLabelText("role 2"), {
    target: { value: "Leaving" },
  })
  fireEvent.change(screen.getByLabelText("role 3"), {
    target: { value: "Host" },
  })
  fireEvent.click(screen.getByRole("radio", { name: "moderator 3" }))
  fireEvent.click(screen.getByRole("button", { name: "removeMember 2" }))
  expect(screen.getByRole("radio", { name: "moderator 2" })).toBeChecked()
  expect(screen.getByLabelText("role 2")).toHaveValue("Host")
  fireEvent.change(screen.getByLabelText("topic"), {
    target: { value: "Question" },
  })
  fireEvent.click(screen.getByRole("button", { name: "preflight" }))
  await waitFor(() => expect(checkedConfig).toBeTruthy())
  expect(checkedConfig?.moderator_ordinal).toBe(1)
  expect(checkedConfig?.participants.map((member) => member.role)).toEqual([
    "member 1",
    "Host",
  ])
  expect(checkedConfig?.participants.map((member) => member.agent)).toEqual([
    "grok",
    "cursor",
  ])
  expect(checkedConfig?.concurrency).toBeLessThanOrEqual(2)
})
