// Dependency-free smoke coverage for Node versions exposing stripTypeScriptTypes.
// This does not replace Vitest, the TypeScript project check or Rust tests.
import fs from "node:fs"
import assert from "node:assert/strict"
import { stripTypeScriptTypes } from "node:module"

const source = (path) => stripTypeScriptTypes(
  fs.readFileSync(new URL(`../${path}`, import.meta.url), "utf8"),
  { mode: "transform" }
)
const load = (text) => import(`data:text/javascript;base64,${Buffer.from(text).toString("base64")}`)
const { prepareRoundtableMutation: prepare, isDefinitiveRoundtableRejection: definitive,
  rememberPendingPaidMutation: remember, pendingPaidMutation: readPending, clearPendingPaidMutation: clear } =
  await load(source("src/lib/roundtable/mutation.ts"))
const first = prepare(null, "roundtable_interject", "room", "7", {
  text: "same", mode: "restart_current", confirmed_preflight_id: "P",
}, () => "R")
const pending = { ...first, uncertain: true }
const retry = prepare(pending, "roundtable_interject", "room", "9", {
  text: "same", mode: "restart_current", confirmed_preflight_id: "P2",
}, () => "R2")
assert.equal(retry, pending)
assert.equal(JSON.stringify(retry.request), JSON.stringify(first.request))
assert.throws(() => prepare(pending, "roundtable_resume", "room", "9", { recovery_consent: true }, () => "R2"), /paid_outcome_unknown/)
assert.equal(definitive(new Error("reply_lost")), false)
assert.equal(definitive({ code: "storage_unavailable" }), false)
assert.equal(definitive({ code: "invalid_argument" }), true)
remember("room-scope", pending)
assert.equal(readPending("room-scope"), pending)
assert.equal(readPending("another-room"), null)
clear("room-scope", "another-request")
assert.equal(readPending("room-scope"), pending)
for (const command of ["roundtable_pause", "roundtable_stop"]) {
  const control = prepare(readPending("room-scope"), command, "room", "9",
    command === "roundtable_pause" ? { reason: "operator" } : {}, () => `C-${command}`)
  assert.equal(control.paid, false)
  assert.equal(control.request.expected_revision, "9")
  remember("room-scope", control)
  clear("room-scope", control.request.request_id)
  const afterCancel = prepare(readPending("room-scope"), "roundtable_interject", "room", "11",
    { text: "same", mode: "restart_current", confirmed_preflight_id: "P2" }, () => "R2")
  assert.equal(JSON.stringify(afterCancel.request), JSON.stringify(first.request))
}
clear("room-scope", "R")
assert.equal(readPending("room-scope"), null)
console.log("Paid request replay/navigation/cancellation: 16 assertions passed")

const transportImport = 'import { getTransport } from "@/lib/transport";'
const apiSource = source("src/lib/roundtable/api.ts")
assert.ok(apiSource.includes(transportImport), "transport import must be the only runtime dependency stub")
const api = await load(apiSource.replace(transportImport, "const getTransport = () => globalThis.roundtableSmokeTransport;"))
assert.match(api.roundtableError({ message: "generic", details: { reason: "provider_credential_missing" } }), /Provider credentials are missing/)
const content = "frozen ∑ source\n"
const hash = await api.roundtableTextHash(content)
const size = new TextEncoder().encode(content).length
const entry = { path: "selected.ts", size, content_hash: hash, text_admissible: true,
  object: { object_id: hash, content_hash: hash, total_bytes: size } }
globalThis.roundtableSmokeTransport = { call: async (_command, { request }) => ({
  object_ref: request.read.object.object_ref, offset: 0, text: content, cursor: null,
}) }
assert.equal(await api.loadRoundtableSource("room", entry), content)
globalThis.roundtableSmokeTransport = { call: async (_command, { request }) => ({
  object_ref: request.read.object.object_ref, offset: 0, text: "wrong", cursor: null,
}) }
await assert.rejects(api.loadRoundtableSource("room", entry), /source_hash/)
await assert.rejects(api.loadRoundtableSource("room", { ...entry, size: 1024 * 1024 + 1 }), /source_reference/)
let calls = 0
globalThis.roundtableSmokeTransport = { call: async (_command, { request }) => {
  calls += 1
  return { object_ref: request.read.object.object_ref, offset: calls === 1 ? 0 : 7,
    text: calls === 1 ? "frozen " : "∑ source\n", cursor: calls === 1 ? "next" : null }
} }
assert.equal(await api.loadRoundtableSource("room", entry), content)
delete globalThis.roundtableSmokeTransport
console.log("Immutable preview/provider error: 5 assertions passed")
