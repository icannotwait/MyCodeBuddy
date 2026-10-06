import { act, fireEvent, render, screen, waitFor } from "@testing-library/react"
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest"
import { RoundtableWorkbench } from "@/components/roundtable/roundtable-workbench"
import fixture from "../../../docs/roundtable/fixtures/projection.json"
import { roundtableHash } from "@/lib/roundtable/api"
import { webcrypto } from "node:crypto"

const call = vi.hoisted(() => vi.fn())
vi.mock("@/lib/transport", () => ({
  getTransport: () => ({ call, subscribe: async () => () => undefined }),
}))
vi.mock("@/lib/api", () => ({
  listModelProviders: async () => [{ id: 1, name: "Provider", model: null }],
}))
vi.mock("next-intl", () => ({ useTranslations: () => (key: string) => key }))

describe("roundtable product page", () => {
  beforeEach(() => {
    call.mockReset()
    vi.stubGlobal("crypto", webcrypto)
  })

  afterEach(() => vi.restoreAllMocks())

  async function room(status: string) {
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
    const projection = {
      projection_ref: { id: "projection", hash: await roundtableHash(body) },
      body,
    }
    call.mockImplementation(async (command: string) => {
      if (command === "roundtable_list") return { rooms: [], cursor: null }
      if (command === "roundtable_get" || command === "roundtable_attach")
        return { projection, message_manifest_id: "projection" }
      if (command === "roundtable_messages")
        return { messages: [], cursor: null }
      if (command === "roundtable_preflight")
        return {
          enabled: true,
          readiness: "ready",
          tools: [],
          config_hash: await roundtableHash(body.replay.config),
          network: "model_gateway_only",
          writes: "scratch_only",
          confirmed_preflight_id: "confirmed",
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
    vi.spyOn(window, "setInterval").mockImplementation((callback, delay) => {
      if (delay !== 5000) return nativeInterval(callback, delay)
      poll = callback as () => void
      return 987654
    })
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
    await waitFor(() => expect(screen.queryByText("reply_lost")).toBeNull())
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
    render(<RoundtableWorkbench workspaceId="workspace" />)
    fireEvent.change(screen.getByLabelText("topic"), {
      target: { value: "Question" },
    })
    await waitFor(() =>
      expect(screen.getAllByRole("option", { name: "Provider" })).toHaveLength(
        3
      )
    )
    fireEvent.change(screen.getByLabelText("moderator"), {
      target: { value: "2" },
    })
    fireEvent.click(screen.getByRole("button", { name: "preflight" }))
    await screen.findByText("disabled")
    expect(screen.getByRole("button", { name: "create" })).toBeDisabled()
    expect(call).toHaveBeenCalledWith("roundtable_preflight", {
      request: {
        config: expect.objectContaining({
          topic: "Question",
          workspace_id: "workspace",
          participants: [
            expect.objectContaining({
              ordinal: 0,
              provider_ref: "provider:1",
              agent: "grok",
            }),
            expect.objectContaining({
              ordinal: 1,
              provider_ref: "provider:1",
              agent: "cursor",
            }),
            expect.objectContaining({
              ordinal: 2,
              provider_ref: "provider:1",
              agent: "antigravity",
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
})
