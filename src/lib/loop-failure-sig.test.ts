import { describe, it, expect } from "vitest"

import { failureAttempt, humanizeFailureSig } from "./loop-failure-sig"
import type { LoopInboxItemRow } from "./types"

function card(
  payload: unknown,
  kind: LoopInboxItemRow["kind"] = "blocked"
): LoopInboxItemRow {
  return {
    id: 1,
    issue_id: 7,
    issue_seq: 3,
    iteration_id: null,
    kind,
    subject_key: "no_progress:42",
    payload,
    status: "pending",
    subject_artifact_id: 42,
    subject_title: "Task A",
    created_at: "2026-06-17T00:00:00Z",
  }
}

describe("humanizeFailureSig", () => {
  it("maps failure_sig prefixes to a family + key (sig wins over reason)", () => {
    expect(
      humanizeFailureSig(card({ failure_sig: "empty_diff:implement" }))
    ).toEqual({ family: "emptyDiff", key: "emptyDiff" })
    expect(
      humanizeFailureSig(card({ failure_sig: "validation_failed:9f3a" }))
    ).toEqual({ family: "validation", key: "validationFailed" })
    expect(
      humanizeFailureSig(card({ failure_sig: "no_artifacts:design" }))
    ).toEqual({ family: "noArtifacts", key: "noArtifacts" })
    expect(
      humanizeFailureSig(card({ failure_sig: "infra_failure:7" }))
    ).toEqual({ family: "infra", key: "infraFailure" })
    // failure_sig present alongside a breaker reason → sig wins.
    expect(
      humanizeFailureSig(
        card({ failure_sig: "empty_diff:implement", reason: "max_attempts" })
      )
    ).toEqual({ family: "emptyDiff", key: "emptyDiff" })
  })

  it("oscillation reason wins over the underlying failure_sig it carries (D14)", () => {
    // An oscillation card carries the repeated cause's sig, but its escalation
    // message must take precedence.
    expect(
      humanizeFailureSig(
        card({
          reason: "oscillation",
          failure_sig: "validation_failed:9f3a",
          count: 2,
        })
      )
    ).toEqual({ family: "oscillation", key: "oscillation" })
  })

  it("falls back to reason when there is no failure_sig", () => {
    expect(
      humanizeFailureSig(card({ reason: "stalled", stage: "implement" }))
    ).toEqual({ family: "stalled", key: "stalled" })
    expect(humanizeFailureSig(card({ reason: "max_attempts" }))).toEqual({
      family: "breaker",
      key: "maxAttempts",
    })
    expect(humanizeFailureSig(card({ reason: "repeated_failure" }))).toEqual({
      family: "breaker",
      key: "repeatedFailure",
    })
    expect(
      humanizeFailureSig(card({ reason: "dependency_unsatisfiable" }))
    ).toEqual({ family: "dependency", key: "dependencyUnsatisfiable" })
    expect(
      humanizeFailureSig(card({ reason: "integration_gap_exhausted" }))
    ).toEqual({ family: "integration", key: "integrationGap" })
  })

  it("returns null for an unknown / empty payload (card keeps its own desc)", () => {
    expect(humanizeFailureSig(card({}))).toBeNull()
    expect(humanizeFailureSig(card({ reason: "something_new" }))).toBeNull()
    expect(humanizeFailureSig(card(null))).toBeNull()
    // an unknown failure_sig prefix with no known reason → null
    expect(humanizeFailureSig(card({ failure_sig: "mystery:1" }))).toBeNull()
  })

  it("reads the positive integer attempt, else null", () => {
    expect(failureAttempt(card({ attempt: 3 }))).toBe(3)
    expect(failureAttempt(card({ attempt: 0 }))).toBeNull()
    expect(failureAttempt(card({}))).toBeNull()
    expect(failureAttempt(card({ attempt: "2" }))).toBeNull()
  })
})
