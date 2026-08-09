### Spec Compliance

Review bindings match the card: `HEAD` is
`624fa8c37c82233a07eaa25cfc166992ee8c9c96`, the stated base is
`29904a3a8fe6a741372809dfccb08f7a2e194e9f`, and the approved design still
hashes to
`b3446ec31cc8b0457ed1ca3e7c6e8b3ec421eb4b997af6efae3d8975ecd2bdef`.
The producer range is one scoped session-loop commit, and `git diff --check`
passes.

The implementation satisfies most of the Task 5 route:

- `set_eui_workspace` canonicalizes and verifies a directory before
  `open_folder_core`; the facade tests check both a missing path and a regular
  file without creating a folder row.
- Conversation creation is delegated to
  `create_project_conversation_core`, with Grok/Codex guards on the create
  path. History is loaded through
  `get_folder_conversation_with_live_core` using
  `HistoryLoadOpts { user_turn_limit: Some(100), before_turn_id: None }`.
- Production create/resume performs installation verification, uses
  `AcpRouteRequest::root(Some(conversation_id), None)`, loads the user launch
  context, and spawns with the canonical workspace, owner `"eui"`, and no
  parent or operation ownership. Resume passes the persisted external session
  ID; the live branch looks up the connection by conversation ID.
- The facade send constructs one text block, generates a UUID client message
  ID, and delegates to `send_prompt_linked_with_message_id` with folder and
  conversation IDs.
- Workspace/create/select/send are dispatched by async `CoreOps` workers.
  Successful create/select payloads serialize both `conversationId` and
  `connectionId`. Selection-changing admissions reserve a completion and
  advance `selection_epoch` under the model lock; stale worker results remain
  exactly-once completions and model projection is epoch-guarded.
- `t0_ns` is recorded after the command-channel permit is sent. Positive
  conversation-ID validation runs after the common UI-thread/lifecycle guard
  and null output-pointer check, preserving the Task 3 admission precedence.
- The added DTOs contain scalar/path/session/history projections only. The
  scoped diff adds no AppState/DB/parser fields to DTOs, no Axum/Tauri handler,
  and no alternate settings or persistence schema.

The parent-authorized full Cargo omission is not a defect in this review. The
reported focused actual-source probes and contracts-only CTest are acceptable
evidence on this host; dependency-complete shared-codeg execution remains a
residual verification limitation.

### Strengths

- The facade reuses the existing folder, conversation, history, ACP launch,
  connection, and linked-send cores rather than duplicating persistence or
  transport behavior.
- Launch ownership and routing arguments are narrow and explicit. The
  recording seam checks verify/build/spawn order, absolute workspace use,
  owner `"eui"`, UUID generation, and linked folder/conversation IDs.
- Selection projection and completion terminalization share the captured
  epoch. `terminalize_with_update` applies workspace/session/transcript state
  only for the current epoch while still draining stale terminal completions
  once.
- ABI changes are small and retain panic containment, UI-thread admission,
  lifecycle admission, output-pointer precedence, and the frozen public frame
  layout.

### Issues (Critical / Important / Minor)

#### Critical

**C1. Accepted sends can be delivered to a different session after a selection
change.**

`CoreOps::send_user_message` carries only the text
(`src-tauri/codeg-eui-core/src/runtime.rs:44`). Although enqueue captures the
current epoch in the `RuntimeCommand` (`runtime.rs:364-376`), execute dispatch
drops that epoch for sends (`runtime.rs:535-539`). The async send worker later
rereads the mutable `AppCommandContext.selection` (`runtime.rs:143-154`).
Meanwhile, a newer create/select admission clears that selection
(`runtime.rs:63-72`), and its completion can install the new selection
(`runtime.rs:240-245`).

Consequently, this valid interleaving is possible:

1. A send for session A is accepted at epoch N.
2. Selection B is accepted at epoch N+1.
3. B completes and becomes the mutable context selection.
4. The older send worker runs, reads B, and calls the irreversible linked-send
   path with B's connection/folder/conversation.
5. Its completion is marked stale because it captured epoch N, but the prompt
   has already been persisted and sent to the wrong agent/workspace.

The same root problem exists for queued create/select work: those methods
receive `selection_epoch` but clone whichever workspace is current when their
future first runs without checking that the context epoch still matches
(`runtime.rs:101-139`). A stale create can therefore act on a newer workspace.
Epoch-guarding the later model update does not protect these external side
effects.

Required change: bind an immutable workspace/selection snapshot to each
accepted operation, or validate the captured epoch while taking that snapshot
before any DB/ACP side effect. A send that already captured A may finish on A
and report stale after a switch; a send that cannot capture its admitted
selection must terminalize stale/error without dispatching. Add a gated
slow/queued-send test that switches sessions and asserts the linked IDs remain
A (or that no send occurs), plus exactly one stale completion and no active
projection overwrite. Add the equivalent queued-create workspace assertion.

#### Important

**I1. The selectable session boundary is not restricted to the promised
Grok/Codex regular-session set.**

Workspace projection filters `ConversationKind::Regular` but does not filter
the agent (`src-tauri/src/commands/eui_facade.rs:296-302`), so regular Claude,
Gemini, or other persisted rows are exposed in the EUI session list. Selection
loads any conversation in the folder, without checking its kind
(`eui_facade.rs:440-467`), and checks `ensure_supported` only in the no-live-
connection resume branch (`eui_facade.rs:380-405`). An unsupported conversation
that already has a live connection therefore selects successfully; a direct
positive ABI ID can also target a chat/delegate/loop row that was intentionally
excluded from the projected list.

Required change: define one EUI-session eligibility check (`Regular` and
Grok/Codex), apply it while listing and immediately after loading a selection,
before live-connection lookup or any parser/ACP action. Cover unsupported live
rows and direct selection of non-regular rows.

#### Minor

**N1. Focused contracts omit several required session branches.**

No test invokes `select_eui_session_with_ops`, so live connection reuse and
resume with the persisted `external_id` are only source-inspected. The history
test uses an empty transcript and therefore does not prove the 100-user-turn
window. `session_contract.rs` exercises workspace admission and invalid IDs,
but does not call the public create or send ABI, assert create/select completion
IDs, or assert `t0_ns` changes after send acceptance. These gaps also allowed
C1 and I1 to escape the focused suite. Add narrow seam/ABI cases for these
branches; no full Cargo suite is required by this finding.

### Assessment (Task quality: Needs fixes)

The implementation has a sound facade shape and largely correct core mapping,
but the worker context is resolved too late. A stale request can perform an
irreversible action against a newer session, including sending user text to
the wrong workspace, so Task 5 is not safe to approve until admission-time
binding is fixed. The session eligibility guard should be centralized at the
same time. Task quality: Needs fixes.

VERDICT: request_changes
