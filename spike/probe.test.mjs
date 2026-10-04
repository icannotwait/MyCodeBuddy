import assert from "node:assert/strict"
import test from "node:test"

import { probeEnvironment } from "./probe.mjs"

const FORBIDDEN_SECRET = "probe-sentinel-do-not-leak"

function assertCaseEvidence(item) {
  assert.equal(typeof item, "object")
  assert.ok(item)
  assert.ok("command_sha256" in item)
  assert.ok("exit_status" in item)
  assert.ok("expected_rejection" in item)
  assert.ok("actual_rejection" in item)
  assert.equal(typeof item.block_reason, "string")
  assert.ok(item.block_reason.length > 0)
  assert.notEqual(item.status, "passed")
}

test("missing_profile_is_blocked", async () => {
  const report = await probeEnvironment(undefined, undefined)

  assert.equal(report.can_probe, false)
  assert.equal(report.model_request_count, 0)
  assert.equal(typeof report.block_reason, "string")
  assert.ok(report.block_reason.length > 0)
  assert.ok(Array.isArray(report.cases))
  assert.ok(Array.isArray(report.untested))
  assert.equal(report.qualification_issued, false)
  assert.equal(report.g1_unblocked, false)
  for (const item of report.cases) {
    assertCaseEvidence(item)
  }
  assert.equal(JSON.stringify(report).includes(FORBIDDEN_SECRET), false)
})

test("no_approval_never_contacts_model", async () => {
  let modelCalls = 0
  const report = await probeEnvironment(
    {
      approval: {
        model_request: false,
        probe: false,
        install: false,
        credentials: false,
      },
      binaries: {
        crun_sha256: "a".repeat(64),
        image_digest: `sha256:${"b".repeat(64)}`,
        adapter_sha256: "c".repeat(64),
        codex_sha256: "d".repeat(64),
        node_sha256: "e".repeat(64),
        mcp_sha256: "f".repeat(64),
      },
      model_credential: FORBIDDEN_SECRET,
      async contactModel() {
        modelCalls += 1
        return { ok: true }
      },
    },
    [
      {
        id: "endpoint_compatibility",
        kind: "model_request",
        expected_rejection: false,
      },
    ]
  )

  assert.equal(modelCalls, 0)
  assert.equal(report.can_probe, false)
  assert.equal(report.model_request_count, 0)
  assert.equal(report.endpoint_compatibility, "not_tested")
  assert.equal(JSON.stringify(report).includes(FORBIDDEN_SECRET), false)
  assert.equal(report.qualification_issued, false)
  assert.equal(report.g1_unblocked, false)

  const endpoint = report.cases.find(
    (item) => item.id === "endpoint_compatibility"
  )
  assert.ok(endpoint)
  assert.equal(endpoint.status, "not_tested")
  assert.equal(endpoint.exit_status, null)
  assert.equal(endpoint.actual_rejection, "not_executed")
  assert.equal(endpoint.expected_rejection, false)
  for (const item of report.cases) {
    assertCaseEvidence(item)
  }
})
