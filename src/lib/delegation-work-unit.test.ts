import { describe, expect, it } from "vitest"

import type { DelegationRunIdentity } from "@/lib/delegation-card"
import {
  groupDelegationRuns,
  type DelegationRunRecord,
} from "@/lib/delegation-work-unit"

function run(
  value: string,
  taskId: string | null,
  workUnitKey: string | null,
  childConversationId: number | null,
  linkedTaskIds: string[],
  parentConversationId = 2075
): DelegationRunRecord<string> {
  const identity: DelegationRunIdentity = {
    parentConversationId,
    parentToolUseId: value,
    workUnitKey,
    taskId,
    childConversationId,
    linkedTaskIds,
  }
  return { value, identity }
}

describe("groupDelegationRuns", () => {
  it("unions initial and continued runs by work key and task link", () => {
    const grouped = groupDelegationRuns([
      run("tool-1", "run-1", "unit-a", null, []),
      run("tool-2", "run-2", "unit-a", 3001, ["run-1"]),
    ])

    expect(grouped.units).toHaveLength(1)
    expect(grouped.units[0].key).toBe("unit-a")
    expect(grouped.units[0].runs.map((entry) => entry.value)).toEqual([
      "tool-1",
      "tool-2",
    ])
    expect(grouped.index.taskToUnitKey.get("run-2")).toBe("unit-a")
  })

  it("unions out-of-order continuation links without changing run order", () => {
    const grouped = groupDelegationRuns([
      run("tool-continue", "run-2", null, 3001, ["run-1"]),
      run("tool-initial", "run-1", null, null, []),
    ])

    expect(grouped.units).toHaveLength(1)
    expect(grouped.units[0].runs.map((entry) => entry.value)).toEqual([
      "tool-continue",
      "tool-initial",
    ])
  })

  it("keeps equal task and child ids from different parents isolated", () => {
    const grouped = groupDelegationRuns([
      run("a", "same", null, 10, [], 1),
      run("b", "same", null, 10, [], 2),
    ])

    expect(grouped.units).toHaveLength(2)
    expect(grouped.units.map((unit) => unit.runs[0].value)).toEqual(["a", "b"])
  })

  it("keeps conflicting explicit work keys separate when a child id is reused", () => {
    const grouped = groupDelegationRuns([
      run("a", "run-a", "unit-a", 10, []),
      run("b", "run-b", "unit-b", 10, []),
    ])

    expect(grouped.units).toHaveLength(2)
    expect(grouped.units.map((unit) => unit.key)).toEqual(["unit-a", "unit-b"])
  })

  it("chooses an explicit work key when linked fallback identities converge", () => {
    const grouped = groupDelegationRuns([
      run("fallback", "run-1", null, 3001, []),
      run("explicit", "run-2", "unit-a", 3001, ["run-1"]),
    ])

    expect(grouped.units).toHaveLength(1)
    expect(grouped.units[0].key).toBe("unit-a")
    expect(grouped.index.workUnitToUnitKey.get("unit-a")).toBe("unit-a")
  })

  it("keeps identity-free dispatches separate", () => {
    const grouped = groupDelegationRuns([
      run("tool-a", null, null, null, []),
      run("tool-b", null, null, null, []),
    ])

    expect(grouped.units).toHaveLength(2)
    expect(grouped.units[0].key).not.toBe(grouped.units[1].key)
  })

  it("marks only exact primary task ids as known", () => {
    const grouped = groupDelegationRuns([
      run("tool-a", "run-a", "unit-a", 3001, ["linked-only"]),
    ])

    expect(grouped.index.knownTaskIds.has("run-a")).toBe(true)
    expect(grouped.index.knownTaskIds.has("linked-only")).toBe(false)
    expect(grouped.index.knownTaskIds.has("run")).toBe(false)
    expect(grouped.index.knownTaskIds.has("3001")).toBe(false)
  })

  it("rejects one exact task id owned by two distinct runs", () => {
    const grouped = groupDelegationRuns([
      run("tool-a", "duplicate", "unit-a", null, []),
      run("tool-b", "duplicate", "unit-a", null, []),
    ])

    expect(grouped.index.taskToRunKey.has("duplicate")).toBe(false)
    expect(grouped.index.knownTaskIds.has("duplicate")).toBe(false)
  })

  it("does not let a foreign parent make an ambiguous local id known", () => {
    const grouped = groupDelegationRuns([
      run("tool-a", "same", null, null, [], 2075),
      run("tool-b", "same", null, null, [], 2076),
    ])

    expect(grouped.index.knownTaskIds.has("same")).toBe(false)
  })
})
