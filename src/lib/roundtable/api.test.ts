import { describe, expect, it, vi } from "vitest"

import {
  roundtableBody,
  roundtableCall,
  roundtableError,
  roundtableStatus,
  ROUNDTABLE_COMMANDS,
  verifyRoundtableProjection,
  loadRoundtable,
  roundtableHash,
  roundtableTextHash,
  loadRoundtableEvidence,
  loadRoundtableSource,
  verifyRoundtableReplay,
} from "@/lib/roundtable/api"
import fixture from "../../../docs/roundtable/fixtures/projection.json"
import { webcrypto } from "node:crypto"
import type { RoundtableProjection } from "@/lib/roundtable/types"

const call = vi.hoisted(() => vi.fn().mockResolvedValue({ accepted: true }))
vi.mock("@/lib/transport", () => ({ getTransport: () => ({ call }) }))

describe("roundtable api", () => {
  it("keeps L and H fixed throughout replay pages and rejects unknown causes", async () => {
    vi.stubGlobal("crypto", webcrypto)
    const golden = fixture.cases[0]
    const base = JSON.parse(Buffer.from(golden.original_hex, "hex").toString())
    base.messages = []
    base.replay.message_memberships = []
    const versions = await Promise.all(
      [4, 5, 6].map(async (seq) => {
        const body = { ...base, last_seq: String(seq) }
        return {
          body,
          projection_ref: { id: `p${seq}`, hash: await roundtableHash(body) },
        } as RoundtableProjection
      })
    )
    const events = versions.slice(1).map((version) => ({
      seq: version.body.last_seq,
      schema_version: 1,
      cause: "attempt",
      projection_ref: version.projection_ref,
    }))
    call.mockImplementation(
      async (
        command: string,
        args: {
          request: {
            cursor?: string
            read?: { projection?: { projection_id?: string } }
          }
        }
      ) => {
        if (command === "roundtable_events")
          return {
            events: [events[args.request.cursor ? 1 : 0]],
            cursor: args.request.cursor ? null : "next",
          }
        if (command === "roundtable_get")
          return {
            projection: versions.find(
              (v) =>
                v.projection_ref.id ===
                args.request.read?.projection?.projection_id
            ),
            message_manifest_id: "fixed",
          }
        return { messages: [], cursor: null }
      }
    )
    await expect(
      verifyRoundtableReplay(versions[0], versions[2])
    ).resolves.toBeUndefined()
    expect(call).toHaveBeenCalledWith("roundtable_events", {
      request: {
        room_id: base.room_id,
        after_seq: "4",
        through_seq: "6",
        cursor: "next",
      },
    })
    events[0].cause = "future_cause"
    await expect(
      verifyRoundtableReplay(versions[0], versions[2])
    ).rejects.toThrow("unknown_event")
    vi.unstubAllGlobals()
  })
  it("verifies published evidence metadata and excerpt against the fixed projection", async () => {
    vi.stubGlobal("crypto", webcrypto)
    const body = {
      excerpt: "source line",
      excerpt_hash: await roundtableTextHash("source line"),
      verified: true,
      origin: "workspace_snapshot",
    }
    const projection = {
      body: {
        room_id: "room",
        last_seq: "8",
        replay: {
          evidence: [
            {
              evidence_id: "evidence",
              content_hash: "filehash",
              body_hash: await roundtableHash(body),
              published_seq: "7",
            },
          ],
        },
      },
    } as RoundtableProjection
    call.mockResolvedValue({ body, hash: "filehash" })
    await expect(
      loadRoundtableEvidence(projection, "evidence")
    ).resolves.toEqual({ body, hash: "filehash" })
    call.mockResolvedValue({
      body: { ...body, verified: false },
      hash: "filehash",
    })
    await expect(
      loadRoundtableEvidence(projection, "evidence")
    ).rejects.toThrow("evidence_hash")
    await expect(loadRoundtableEvidence(projection, "foreign")).rejects.toThrow(
      "evidence_membership"
    )
    vi.unstubAllGlobals()
  })
  it("rejects a cyclic manifest cursor without looping", async () => {
    vi.stubGlobal("crypto", webcrypto)
    const golden = fixture.cases[0]
    const body = JSON.parse(Buffer.from(golden.original_hex, "hex").toString())
    let page = 0
    call.mockImplementation(async (command: string) => {
      if (command === "roundtable_get")
        return {
          projection: {
            projection_ref: { id: "projection", hash: golden.sha256 },
            body,
          },
          message_manifest_id: "manifest",
        }
      if (++page > 3) throw new Error("unbounded_cursor")
      return { messages: [], cursor: page % 2 ? "a" : "b" }
    })
    await expect(loadRoundtable(body.room_id)).rejects.toThrow("page_cursor")
    vi.unstubAllGlobals()
  })

  it("loads all frozen messages and ignores current membership changes", async () => {
    vi.stubGlobal("crypto", webcrypto)
    const golden = fixture.cases[0]
    const body = JSON.parse(Buffer.from(golden.original_hex, "hex").toString())
    const message = { summary: "accepted" }
    const hash = await roundtableHash(message)
    body.messages = [{ message_id: "fixed-message", hash }]
    body.replay.message_memberships = [
      { message_id: "fixed-message", visibility: "staged" },
    ]
    const projection = {
      projection_ref: { id: "fixed", hash: await roundtableHash(body) },
      body,
    }
    call.mockImplementation(async (command: string) =>
      command === "roundtable_get"
        ? { projection, message_manifest_id: "fixed" }
        : {
            messages: [
              {
                message_id: "fixed-message",
                body_hash: hash,
                body: message,
                visibility: "staged",
              },
            ],
            cursor: null,
          }
    )
    expect((await loadRoundtable(body.room_id, "fixed")).messages).toHaveLength(
      1
    )
    vi.unstubAllGlobals()
  })
  it("loads exactly the frozen manifest and verifies body membership", async () => {
    vi.stubGlobal("crypto", webcrypto)
    const golden = fixture.cases[0]
    const body = JSON.parse(Buffer.from(golden.original_hex, "hex").toString())
    const projection = {
      projection_ref: { id: "projection", hash: golden.sha256 },
      body,
    }
    call.mockImplementation(async (command: string) =>
      command === "roundtable_get"
        ? { projection, message_manifest_id: "manifest" }
        : {
            messages: [
              {
                message_id: body.messages[0].message_id,
                body_hash: body.messages[0].hash,
                visibility: "staged",
                body: { summary: "corrupted" },
              },
            ],
            cursor: null,
          }
    )
    await expect(loadRoundtable(body.room_id)).rejects.toThrow("message_hash")
    expect(call).toHaveBeenLastCalledWith("roundtable_messages", {
      request: { room_id: body.room_id, manifest_id: "manifest" },
    })
    call.mockReset().mockResolvedValue({ accepted: true })
    vi.unstubAllGlobals()
  })
  it("verifies the Rust canonical golden projection and rejects tampering", async () => {
    vi.stubGlobal("crypto", webcrypto)
    const golden = fixture.cases[0]
    const body = JSON.parse(Buffer.from(golden.original_hex, "hex").toString())
    const projection = {
      projection_ref: { id: "projection", hash: golden.sha256 },
      body,
    } as RoundtableProjection
    await expect(
      verifyRoundtableProjection(projection, body.room_id)
    ).resolves.toBeUndefined()
    body.status = "completed"
    await expect(
      verifyRoundtableProjection(projection, body.room_id)
    ).rejects.toThrow("projection_hash")
    await expect(
      verifyRoundtableProjection(projection, "other-room")
    ).rejects.toThrow("projection_scope")
    vi.unstubAllGlobals()
  })
  it("dispatches every frozen command through the shared authenticated transport", async () => {
    expect(ROUNDTABLE_COMMANDS).toHaveLength(18)
    for (const command of ROUNDTABLE_COMMANDS) {
      const request = { room_id: "room", request_id: "request" }
      await roundtableCall(command, request)
      expect(call).toHaveBeenLastCalledWith(command, { request })
    }
  })
  it("omits principal and clamps the page", () => {
    const body = roundtableBody({ command: "roundtable_list", pageLimit: 100 })
    expect(body).not.toHaveProperty("principal")
    expect(body.page_limit).toBe(100)
    expect(roundtableStatus("capacity_limited")).toBe(429)
    expect(() =>
      roundtableBody({ command: "roundtable_events", pageLimit: 501 })
    ).toThrow()
  })
})

it("loads selected frozen source pages and rejects a mismatched content hash", async () => {
  vi.stubGlobal("crypto", webcrypto)
  const text = "frozen source\n"
  const hash = await roundtableTextHash(text)
  const entry = {
    path: "src/a.ts",
    size: text.length,
    content_hash: hash,
    text_admissible: true,
    object: { object_id: hash, content_hash: hash, total_bytes: text.length },
  }
  call.mockImplementation(async (_command, args) => ({
    object_ref: args.request.read.object.object_ref,
    offset: 0,
    text,
    cursor: null,
  }))
  expect(await loadRoundtableSource("room", entry)).toBe(text)
  call.mockImplementation(async (_command, args) => ({
    object_ref: args.request.read.object.object_ref,
    offset: 0,
    text: "changed source",
    cursor: null,
  }))
  await expect(loadRoundtableSource("room", entry)).rejects.toThrow(
    "source_hash"
  )
})

it("explains provider credentials separately from application login", () => {
  expect(
    roundtableError({
      code: "capability_unqualified",
      message: "The capability is not qualified.",
      details: { reason: "provider_credential_missing" },
    })
  ).toBe(
    "Provider credentials are missing. Update the provider settings, then retry."
  )
})
