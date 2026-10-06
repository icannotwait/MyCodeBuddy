import { describe, expect, it } from "vitest"
import {
  prepareRoundtableMutation,
  isDefinitiveRoundtableRejection,
} from "./mutation"

describe("roundtable paid-command replay", () => {
  it("keeps the exact serialized request after a lost ACK, newer projection and preflight", () => {
    const first = prepareRoundtableMutation(
      null,
      "roundtable_interject",
      "room",
      "7",
      {
        text: "same input",
        mode: "restart_current",
        confirmed_preflight_id: "P",
      },
      () => "R"
    )
    const pending = { ...first, uncertain: true }
    const retry = prepareRoundtableMutation(
      pending,
      "roundtable_interject",
      "room",
      "9",
      {
        text: "same input",
        mode: "restart_current",
        confirmed_preflight_id: "P2",
      },
      () => "R2"
    )
    expect(retry).toBe(pending)
    expect(JSON.stringify(retry.request)).toBe(JSON.stringify(first.request))
    expect(retry.request).toMatchObject({
      request_id: "R",
      expected_revision: "7",
      confirmed_preflight_id: "P",
    })
  })

  it("does not replace an uncertain paid intent with another operation", () => {
    const first = prepareRoundtableMutation(
      null,
      "roundtable_resume",
      "room",
      "7",
      { recovery_consent: true, confirmed_preflight_id: "P" },
      () => "R"
    )
    expect(() =>
      prepareRoundtableMutation(
        { ...first, uncertain: true },
        "roundtable_interject",
        "room",
        "9",
        { text: "new", mode: "restart_current", confirmed_preflight_id: "P2" },
        () => "R2"
      )
    ).toThrow("paid_outcome_unknown")
  })

  it("distinguishes definitive rejection from unknown execution outcome", () => {
    expect(isDefinitiveRoundtableRejection(new Error("reply_lost"))).toBe(false)
    expect(
      isDefinitiveRoundtableRejection({ code: "storage_unavailable" })
    ).toBe(false)
    expect(
      isDefinitiveRoundtableRejection({ code: "command_in_progress" })
    ).toBe(false)
    expect(isDefinitiveRoundtableRejection({ code: "invalid_argument" })).toBe(
      true
    )
    expect(
      isDefinitiveRoundtableRejection({ code: "capability_unqualified" })
    ).toBe(true)
  })
})

it("retains a paid request by room across component navigation until a definitive outcome", async () => {
  const {
    rememberPendingPaidMutation,
    pendingPaidMutation,
    clearPendingPaidMutation,
  } = await import("./mutation")
  const pending = prepareRoundtableMutation(
    null,
    "roundtable_resume",
    "navigation-room",
    "7",
    { recovery_consent: true, confirmed_preflight_id: "P" },
    () => "R"
  )
  rememberPendingPaidMutation("workspace/navigation-room", pending)
  expect(pendingPaidMutation("workspace/navigation-room")?.request).toEqual(
    pending.request
  )
  expect(pendingPaidMutation("workspace/navigation-room")?.uncertain).toBe(true)
  expect(pendingPaidMutation("workspace/other-room")).toBeNull()
  clearPendingPaidMutation("workspace/navigation-room", "unrelated")
  expect(pendingPaidMutation("workspace/navigation-room")).not.toBeNull()
  clearPendingPaidMutation("workspace/navigation-room", "R")
  expect(pendingPaidMutation("workspace/navigation-room")).toBeNull()
})

it.each(["roundtable_pause", "roundtable_stop"] as const)(
  "permits %s without replacing the unresolved paid request",
  async (command) => {
    const {
      rememberPendingPaidMutation,
      pendingPaidMutation,
      clearPendingPaidMutation,
    } = await import("./mutation")
    const scope = `cancel-control/${command}`
    const paid = prepareRoundtableMutation(
      null,
      "roundtable_resume",
      "room",
      "7",
      { recovery_consent: true, confirmed_preflight_id: "P" },
      () => "R"
    )
    rememberPendingPaidMutation(scope, paid)
    const control = prepareRoundtableMutation(
      pendingPaidMutation(scope),
      command,
      "room",
      "9",
      command === "roundtable_pause" ? { reason: "operator" } : {},
      () => "C"
    )
    expect(control.paid).toBe(false)
    expect(control.request).toMatchObject({
      request_id: "C",
      expected_revision: "9",
    })
    rememberPendingPaidMutation(scope, control)
    clearPendingPaidMutation(scope, control.request.request_id)
    const retry = prepareRoundtableMutation(
      pendingPaidMutation(scope),
      "roundtable_resume",
      "room",
      "11",
      { recovery_consent: true, confirmed_preflight_id: "P2" },
      () => "R2"
    )
    expect(retry.request).toEqual(paid.request)
    expect(retry.request).toMatchObject({
      request_id: "R",
      expected_revision: "7",
      confirmed_preflight_id: "P",
    })
    clearPendingPaidMutation(scope, "R")
  }
)
