### Re-review Basis

This review uses only the replacement bindings: reviewed task
`07f57b49-9563-463e-a93d-0818c3afe49a`, producer artifact
`1b4712060387299c21c6780ccdf3a346fed63864`, and fix commit
`7cb516b83793f57bf7bd1b4a3f2645493d05b0df`. `HEAD` is the artifact commit,
its parent is the stated fix, and the fix parent is the original Task 5 feature
commit `624fa8c37c82233a07eaa25cfc166992ee8c9c96`. The approved design digest
remains
`b3446ec31cc8b0457ed1ca3e7c6e8b3ec421eb4b997af6efae3d8975ecd2bdef`.

The fix is scoped to `commands.rs`, `runtime.rs`, `eui_facade.rs`, the updated
Task 5 report, and the separate review-package commit. `git diff --check` passes
for `624fa8c3..1b471206`.

### Prior Finding Disposition

#### C1: Admission-time session/workspace binding - ADDRESSED

`capture_command_context` now checks the current epoch and clones the admitted
workspace for create/select or admitted selection for send
(`src-tauri/codeg-eui-core/src/runtime.rs:86-113`). `RuntimeOwner::enqueue`
captures that context while holding the runtime admission lock, before the
selection-changing reserve/begin sequence, and stores it in `RuntimeCommand`
(`runtime.rs:393-435`). Worker dispatch consumes the owned context and no longer
rereads mutable `AppCommandContext` before DB or linked-send side effects
(`runtime.rs:577-623`).

The regression at `runtime.rs:681-716` proves captured workspace/session IDs do
not change when the mutable context advances. The gated send case at
`runtime.rs:1155-1228` proves a send admitted for connection A still uses A's
folder/conversation after a newer selection is accepted, produces exactly one
stale completion, and does not overwrite the newer projection. This closes the
cross-session prompt misrouting and stale-create/new-workspace risks from C1.

#### I1: Grok/Codex regular-session eligibility - ADDRESSED

Workspace projection now uses the centralized `is_eui_session_eligible`
predicate (`src-tauri/src/commands/eui_facade.rs:322-328`), which requires both
`ConversationKind::Regular` and Grok/Codex (`eui_facade.rs:553-556`). Selection
first loads the persisted summary, validates folder ownership and eligibility,
and only then invokes history parsing or live connection lookup
(`eui_facade.rs:482-509`). The prior live-hit bypass therefore no longer exists.

Focused tests cover list exclusion for an unsupported regular row and a
supported non-regular row (`eui_facade.rs:1021-1065`), plus direct selection of
both categories before `find_connection` is called (`eui_facade.rs:1123-1165`).

#### N1: Focused session branch and ABI coverage - PARTIAL

The highest-risk missing coverage is now present: the fix invokes
`select_eui_session_with_ops` and proves create-then-select live reuse before a
send (`src-tauri/src/commands/eui_facade.rs:1169-1189`), verifies the production
state-binding primitive (`eui_facade.rs:1192-1227`), exercises immutable queued
context, and covers stale-send identity/exactly-once behavior.

Three lower-risk gaps from N1 remain:

- No focused selection test creates a persisted non-empty `external_id` and
  proves the cold resume branch forwards it through build/spawn.
- The history test still selects a new empty conversation
  (`eui_facade.rs:1230-1248`), so it does not behaviorally prove clipping at 100
  user turns even though the production option remains visibly correct at
  `eui_facade.rs:504-507`.
- `session_contract.rs` still imports/calls only workspace selection and invalid
  conversation selection (`src-tauri/codeg-eui-core/tests/session_contract.rs:4-83`);
  it does not exercise public create/send completion JSON or assert the
  post-admission `t0_ns` marker.

These are focused coverage omissions, not observed production defects, and
remain Minor under the authorized host policy.

#### Dual-review I2: Bind spawned connection before first send - ADDRESSED

Both new-session create and cold resume now bind the returned connection before
returning success (`src-tauri/src/commands/eui_facade.rs:373-390` and
`eui_facade.rs:427-445`). `bind_eui_connection` rejects conflicting ownership
and atomically emits the canonical `ConversationLinked` event only while the
connection is unbound (`eui_facade.rs:558-614`). The shared emitter applies the
gate, state mutation, and event sequence under one write lock
(`src-tauri/src/web/event_bridge.rs:415-453`), and `SessionState` sets both
conversation and folder IDs for that event
(`src-tauri/src/acp/session_state.rs:1222-1229`).

The seam test proves create followed by select of the same conversation calls
only `find_connection` (`eui_facade.rs:1169-1189`), while the real manager-state
test proves `find_connection_by_conversation_id` discovers the pre-send binding
(`eui_facade.rs:1192-1227`). The original double-spawn window is closed.

### New Findings

No new Critical or Important issues were found in the fix range.

Minor: the remaining N1 coverage items above should be added when a suitable
focused fixture is available. They do not require a full Cargo suite and do not
invalidate the source-level fix.

### Verification Assessment

The producer reports 11 focused actual-source ABI/runtime/model tests, five
focused facade orchestration/eligibility/binding tests, and three contracts-only
CTest cases passing with warnings denied where applicable. Source inspection
matches those claimed behaviors. Per parent policy, full Cargo suites and
dependency-complete shared-codeg linking were not rerun and are not treated as
Critical or Important.

### Assessment

All blocking Task 5 findings are addressed. The immutable command context
prevents newer selections from changing queued side-effect targets, the
eligible-session boundary is consistently enforced, and spawned connections
are discoverable before first send. The remaining test gaps are Minor.

VERDICT: approve_with_minors
