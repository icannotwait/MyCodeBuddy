# Task 5 Fix Review Package

- BASE: `624fa8c37c82233a07eaa25cfc166992ee8c9c96`
- HEAD: `7cb516b83793f57bf7bd1b4a3f2645493d05b0df`
- Fix subject: `fix(eui): bind session context and eligibility for task 5`
- Range: `624fa8c37c82233a07eaa25cfc166992ee8c9c96..7cb516b83793f57bf7bd1b4a3f2645493d05b0df`
- Prior producer task ID: `ca07b7cb-bc13-437d-afaa-3060e6f50523`

## Review Scope

This is the consolidated Task 5 C1/I1/I2 fix only:

- Accepted create/select/send commands carry immutable workspace or selection
  snapshots captured under the runtime admission lock.
- EUI list/select eligibility is centralized as Regular plus Grok/Codex and
  checked before history, live lookup, or ACP work.
- Successful create/resume spawn emits the canonical ConversationLinked state
  event before returning, making the connection discoverable before first send.
- Focused regressions cover immutable queued context, stale-send identity and
  exactly-once completion, unsupported/non-regular rows, and pre-send reuse.

## Verification

- Actual-source ABI/runtime/model probe: 11 passed, 0 failed, warnings denied.
- Exact facade orchestration/eligibility/binding cases: 5 passed, 0 failed.
- Full facade test module and session contract compile with warnings denied.
- Contracts-only CTest: 3 passed, 0 failed.
- Both Cargo formatting scopes and `git diff --check` pass.
- Approved design SHA-256 remains
  `b3446ec31cc8b0457ed1ca3e7c6e8b3ec421eb4b997af6efae3d8975ecd2bdef`.
- Full Cargo tests were skipped by parent policy. Dependency-complete shared-codeg
  verification remains host-OOM-limited.

## Stat

```text
 .../task-5-report.md                               |  48 ++-
 src-tauri/codeg-eui-core/src/commands.rs           |   1 +
 src-tauri/codeg-eui-core/src/runtime.rs            | 408 +++++++++++++++++++--
 src-tauri/src/commands/eui_facade.rs               | 329 ++++++++++++++++-
 4 files changed, 714 insertions(+), 72 deletions(-)
```

## Full Diff

Blank context-marker trailing spaces are display-normalized; hunk metadata and
all context/addition/deletion content are otherwise the exact BASE..HEAD diff.

```diff
diff --git a/.superpowers/sdd/2026-08-09-eui-neo-frontend-spike/task-5-report.md b/.superpowers/sdd/2026-08-09-eui-neo-frontend-spike/task-5-report.md
index ace66378..e11f7a1b 100644
--- a/.superpowers/sdd/2026-08-09-eui-neo-frontend-spike/task-5-report.md
+++ b/.superpowers/sdd/2026-08-09-eui-neo-frontend-spike/task-5-report.md
@@ -17,7 +17,7 @@ exactly-once terminal completions without overwriting the active projection.
   history without exposing AppState, database handles, or parsers.
 - Canonicalized and verified existing directories before calling
   `open_folder_core`; invalid and non-directory paths cannot create folder
-  rows. Workspace selection projects regular persisted conversations in
+  rows. Workspace selection projects only Grok/Codex regular conversations in
   activity order.
 - Restricted conversation/session creation to Grok and Codex, delegated row
   creation to `create_project_conversation_core`, and delegated history loading
@@ -26,7 +26,9 @@ exactly-once terminal completions without overwriting the active projection.
   `verify_agent_installed`, builds launch inputs with
   `AcpRouteRequest::root(Some(conversation_id), None)`, loads the persisted user
   launch context, and calls `spawn_agent` with owner `"eui"` and no delegation
-  override. A recording test proves verify/build/spawn order and arguments.
+  override. The returned connection is immediately bound to its folder and
+  conversation through the canonical `ConversationLinked` state event. A
+  recording test proves verify/build/spawn/bind order and arguments.
 - Session selection reuses a live connection by conversation ID or resumes via
   the persisted external session ID. Sends build exactly one text block, create
   a UUID client message ID, and call
@@ -35,6 +37,13 @@ exactly-once terminal completions without overwriting the active projection.
   through asynchronous `CoreOps` workers. Successful create/select completion
   JSON includes `conversationId` and `connectionId`; model session/transcript
   projections are applied only at the captured selection epoch.
+- Captured an immutable workspace or session selection while each command is
+  admitted under the runtime admission lock. Queued create/select/send work
+  consumes that owned snapshot and never re-reads a newer mutable selection
+  before DB or ACP side effects.
+- Centralized EUI session eligibility as `Regular` plus Grok/Codex. Selection
+  checks the persisted row before history/live lookup, so unsupported live rows
+  and direct non-regular IDs fail before ACP or parser access.
 - Advanced `selection_epoch` atomically with accepted workspace/create/select
   completion reservation, cleared the previous active projection immediately,
   and added a gated slow-create contract proving one stale completion and no
@@ -56,6 +65,13 @@ boundary before the epoch implementation. The focused test
 `accepted_workspace_and_session_changes_advance_the_selection_epoch` failed as
 intended with `left: 0`, `right: 1`.

+For the consolidated review fix, focused RED runs proved all three reported
+defects: create-then-select called verify/build/spawn a second time; unsupported
+selection reached `find_connection` and the workspace list included a regular
+Claude row; and the runtime regression could not compile because accepted
+commands carried no immutable context. The latter failure named the missing
+`CommandContext`, worker method arguments, and queue field directly.
+
 The dependency-complete `session_contract` target was also attempted before
 implementation, but the kernel killed shared-codeg `rustc` before the test
 binary linked. That host failure is not counted as behavioral RED evidence.
@@ -63,11 +79,13 @@ binary linked. That host failure is not counted as behavioral RED evidence.
 ### GREEN

 - Actual Task 5 `abi.rs`, `commands.rs`, `model.rs`, and `runtime.rs` compiled
-  with `-D warnings` against the established narrow shared-core boundary; **9/9
+  with `-D warnings` against the established narrow shared-core boundary; **11/11
   focused unit tests passed**.
 - Actual `eui_facade.rs`, including its test module, compiled with `-D warnings`
-  against shape-compatible existing-core signatures. The deterministic
-  create/send orchestration test passed (**1/1**).
+  against shape-compatible existing-core signatures. Five focused facade tests
+  passed for create/send/bind ordering, create-before-send reuse, real
+  connection-state binding, list eligibility, and pre-lookup selection rejection
+  (**5/5**).
 - The complete committed `session_contract.rs` compiled with `-D warnings`
   against the actual ABI/runtime/model modules.
 - Contracts-only CMake/CTest passed **3/3** (harness, ABI layout, UI snapshot).
@@ -82,8 +100,8 @@ Passed:

 - `cargo fmt --check` for the shared facade and standalone EUI crate files.
 - Actual-source facade check with `RUSTFLAGS='-D warnings'`.
-- Actual-source ABI/runtime/model tests with `RUSTFLAGS='-D warnings'`: **9/9**.
-- Deterministic session orchestration test: **1/1**.
+- Actual-source ABI/runtime/model tests with `RUSTFLAGS='-D warnings'`: **11/11**.
+- Focused facade orchestration, eligibility, and binding tests: **5/5**.
 - Actual `session_contract.rs` compile-only check with `-D warnings`.
 - Fresh contracts-only CMake build and CTest: **3/3**.
 - `git diff --check`.
@@ -118,22 +136,24 @@ updates. No unrelated runtime behavior was changed.

 ## Self-Review

-- Workspace validation precedes folder persistence; only regular conversations
-  enter the EUI session list.
+- Workspace validation precedes folder persistence; only Grok/Codex regular
+  conversations enter the EUI session list or pass direct selection.
 - Grok/Codex guards execute before conversation or ACP access. The facade adds
   no direct persistence schema, parser, Axum/Tauri handler call, or filesystem
   write path.
 - Create/resume launch uses the selected absolute workspace, persisted external
   ID, root route with no override, user launch context, owner `"eui"`, and no
-  parent/operation ownership.
+  parent/operation ownership. Successful spawn binds folder/conversation IDs
+  before returning, so reselect before first send reuses the live connection.
 - Linked sends carry one text block, a UUID client ID, and the exact selected
   folder/conversation/connection IDs.
 - Selection epoch advancement and completion reservation share one model lock.
   Stale results never mutate sessions, connection ID, or transcript, but still
   drain once through the existing completion ledger.
-- The worker context is invalidated synchronously at accepted selection change,
-  preventing sends from borrowing an old selection while new selection work is
-  in flight.
+- Every worker receives the workspace/selection captured synchronously at
+  admission. A delayed send either dispatches to its admitted IDs and later
+  terminalizes stale or fails without dispatch; it cannot borrow a newer
+  selection.
 - ABI input validation stays inside panic containment and the Task 3
   UI-thread/lifecycle checks. Existing frame layout and header constants remain
   unchanged.
@@ -148,5 +168,5 @@ usable swap. The focused actual-source probes and C++ contracts are green; the
 remaining limitation is host capacity, not a known Task 5 diagnostic.

 <!-- codeg-card-summary-v1
-{"kind":"implementation","phase":"implementation","status":"done_with_concerns","summary":"Added the EUI workspace/session/history/send command loop with canonical workspace persistence, Grok/Codex ACP orchestration, epoch-safe model projection, and linked send timing admission.","commits":[{"subject":"feat(eui): add workspace and session command loop"}],"tests":{"status":"partial","passed":13,"failed":0,"summary":"9 actual-source ABI/runtime/model tests, 1 deterministic facade orchestration test, and 3 contracts-only CTest cases pass; the real session contract compiles against the focused boundary, while dependency-complete shared-codeg checking is host-OOM-limited."},"concerns":["Dependency-complete session_contract and shared-codeg verification require more than the available 3.8 GiB memory or usable swap."],"report_file":".superpowers/sdd/2026-08-09-eui-neo-frontend-spike/task-5-report.md"}
+{"kind":"implementation","phase":"implementation","status":"done_with_concerns","summary":"Added the EUI workspace/session/history/send loop and consolidated review fixes for immutable admission context, eligible-session boundaries, and pre-send live reuse.","commits":[{"subject":"feat(eui): add workspace and session command loop"},{"subject":"fix(eui): bind session context and eligibility for task 5"}],"tests":{"status":"pass","passed":19,"failed":0,"summary":"11 actual-source ABI/runtime/model tests, 5 focused facade orchestration/eligibility/binding tests, and 3 contracts-only CTest cases pass; the real session contract compiles against the focused boundary."},"concerns":["Full Cargo tests remain parent-skipped; dependency-complete shared-codeg verification requires more than the available 3.8 GiB memory or usable swap."],"report_file":".superpowers/sdd/2026-08-09-eui-neo-frontend-spike/task-5-report.md"}
 -->
diff --git a/src-tauri/codeg-eui-core/src/commands.rs b/src-tauri/codeg-eui-core/src/commands.rs
index 806af1c3..f570862e 100644
--- a/src-tauri/codeg-eui-core/src/commands.rs
+++ b/src-tauri/codeg-eui-core/src/commands.rs
@@ -46,6 +46,7 @@ pub(crate) struct RuntimeCommand {
     pub selection_epoch: u64,
     pub op: Operation,
     pub payload: CommandPayload,
+    pub context: crate::runtime::CommandContext,
 }

 pub(crate) fn enqueue(
diff --git a/src-tauri/codeg-eui-core/src/runtime.rs b/src-tauri/codeg-eui-core/src/runtime.rs
index 7fa3ee83..abbeea7f 100644
--- a/src-tauri/codeg-eui-core/src/runtime.rs
+++ b/src-tauri/codeg-eui-core/src/runtime.rs
@@ -37,11 +37,28 @@ impl CoreResult {
 }

 pub(crate) trait CoreOps: Send + Sync {
+    fn capture_context(&self, _selection_epoch: u64, _op: Operation) -> CommandContext {
+        CommandContext::None
+    }
     fn begin_selection(&self, selection_epoch: u64, op: Operation);
     fn set_workspace(&self, selection_epoch: u64, path: Vec<u8>) -> CoreFuture;
-    fn create_session(&self, selection_epoch: u64, agent: Vec<u8>) -> CoreFuture;
-    fn select_session(&self, selection_epoch: u64, conversation_id: i32) -> CoreFuture;
-    fn send_user_message(&self, text: Vec<u8>) -> CoreFuture;
+    fn create_session(
+        &self,
+        selection_epoch: u64,
+        workspace: codeg_lib::commands::eui_facade::EuiWorkspace,
+        agent: Vec<u8>,
+    ) -> CoreFuture;
+    fn select_session(
+        &self,
+        selection_epoch: u64,
+        workspace: codeg_lib::commands::eui_facade::EuiWorkspace,
+        conversation_id: i32,
+    ) -> CoreFuture;
+    fn send_user_message(
+        &self,
+        selection: codeg_lib::commands::eui_facade::EuiSessionSelection,
+        text: Vec<u8>,
+    ) -> CoreFuture;
     fn get_agent_settings(&self, agent: Vec<u8>) -> CoreFuture;
     fn set_agent_settings(&self, agent: Vec<u8>, json: Vec<u8>) -> CoreFuture;
     fn probe_agent(&self, agent: Vec<u8>) -> CoreFuture;
@@ -59,7 +76,48 @@ struct AppCommandContext {
     selection: Option<codeg_lib::commands::eui_facade::EuiSessionSelection>,
 }

+pub(crate) enum CommandContext {
+    None,
+    Workspace(codeg_lib::commands::eui_facade::EuiWorkspace),
+    Selection(codeg_lib::commands::eui_facade::EuiSessionSelection),
+    Unavailable(String),
+}
+
+fn capture_command_context(
+    context: &Arc<Mutex<AppCommandContext>>,
+    selection_epoch: u64,
+    op: Operation,
+) -> CommandContext {
+    let current = context.lock().unwrap_or_else(|error| error.into_inner());
+    if current.selection_epoch != selection_epoch {
+        return CommandContext::Unavailable(
+            "EUI selection changed before command context was captured".to_string(),
+        );
+    }
+    match op {
+        Operation::CreateSession | Operation::SelectSession => current
+            .workspace
+            .clone()
+            .map(CommandContext::Workspace)
+            .unwrap_or_else(|| {
+                CommandContext::Unavailable("no EUI workspace is selected".to_string())
+            }),
+        Operation::SendUserMessage => current
+            .selection
+            .clone()
+            .map(CommandContext::Selection)
+            .unwrap_or_else(|| {
+                CommandContext::Unavailable("no EUI session is selected".to_string())
+            }),
+        _ => CommandContext::None,
+    }
+}
+
 impl CoreOps for AppCoreOps {
+    fn capture_context(&self, selection_epoch: u64, op: Operation) -> CommandContext {
+        capture_command_context(&self.context, selection_epoch, op)
+    }
+
     fn begin_selection(&self, selection_epoch: u64, op: Operation) {
         let mut context = self
             .context
@@ -98,19 +156,18 @@ impl CoreOps for AppCoreOps {
         })
     }

-    fn create_session(&self, selection_epoch: u64, agent: Vec<u8>) -> CoreFuture {
+    fn create_session(
+        &self,
+        selection_epoch: u64,
+        workspace: codeg_lib::commands::eui_facade::EuiWorkspace,
+        agent: Vec<u8>,
+    ) -> CoreFuture {
         let state = Arc::clone(&self.state);
         let context = Arc::clone(&self.context);
         Box::pin(async move {
             let wire = String::from_utf8(agent).map_err(|_| "agent is not UTF-8".to_string())?;
             let agent = codeg_lib::commands::eui_facade::parse_supported_agent(&wire)
                 .map_err(|error| error.to_string())?;
-            let workspace = context
-                .lock()
-                .unwrap_or_else(|error| error.into_inner())
-                .workspace
-                .clone()
-                .ok_or_else(|| "no EUI workspace is selected".to_string())?;
             let selection =
                 codeg_lib::commands::eui_facade::create_eui_session(&state, &workspace, agent)
                     .await
@@ -119,16 +176,15 @@ impl CoreOps for AppCoreOps {
         })
     }

-    fn select_session(&self, selection_epoch: u64, conversation_id: i32) -> CoreFuture {
+    fn select_session(
+        &self,
+        selection_epoch: u64,
+        workspace: codeg_lib::commands::eui_facade::EuiWorkspace,
+        conversation_id: i32,
+    ) -> CoreFuture {
         let state = Arc::clone(&self.state);
         let context = Arc::clone(&self.context);
         Box::pin(async move {
-            let workspace = context
-                .lock()
-                .unwrap_or_else(|error| error.into_inner())
-                .workspace
-                .clone()
-                .ok_or_else(|| "no EUI workspace is selected".to_string())?;
             let selection = codeg_lib::commands::eui_facade::select_eui_session(
                 &state,
                 &workspace,
@@ -140,17 +196,14 @@ impl CoreOps for AppCoreOps {
         })
     }

-    fn send_user_message(&self, text: Vec<u8>) -> CoreFuture {
+    fn send_user_message(
+        &self,
+        selection: codeg_lib::commands::eui_facade::EuiSessionSelection,
+        text: Vec<u8>,
+    ) -> CoreFuture {
         let state = Arc::clone(&self.state);
-        let context = Arc::clone(&self.context);
         Box::pin(async move {
             let text = String::from_utf8(text).map_err(|_| "message is not UTF-8".to_string())?;
-            let selection = context
-                .lock()
-                .unwrap_or_else(|error| error.into_inner())
-                .selection
-                .clone()
-                .ok_or_else(|| "no EUI session is selected".to_string())?;
             codeg_lib::commands::eui_facade::send_eui_message(&state, &selection, text)
                 .await
                 .map_err(|error| error.to_string())?;
@@ -363,6 +416,7 @@ impl RuntimeOwner {
         })?;
         let request_id = next_request_id()?;
         let selection_epoch = model.selection_epoch();
+        let context = self.core_ops.capture_context(selection_epoch, op);
         model.reserve(request_id, op, selection_epoch)?;
         let selection_epoch = model.selection_epoch();
         if op.changes_selection() {
@@ -373,6 +427,7 @@ impl RuntimeOwner {
             selection_epoch,
             op,
             payload,
+            context,
         });
         if op == Operation::SendUserMessage {
             model.record_send_accepted(native_timestamp_ns());
@@ -463,6 +518,7 @@ async fn run_worker(
                     command.selection_epoch,
                     command.op,
                     command.payload,
+                    command.context,
                     Arc::clone(&core_ops),
                 ));
                 metadata.insert(abort.id(), command_metadata);
@@ -522,8 +578,13 @@ async fn execute_command(
     selection_epoch: u64,
     op: Operation,
     payload: CommandPayload,
+    context: CommandContext,
     core_ops: Arc<dyn CoreOps>,
 ) -> Result<CoreResult, String> {
+    let context = match context {
+        CommandContext::Unavailable(error) => return Err(error),
+        context => context,
+    };
     match payload {
         #[cfg(feature = "ffi-test-hooks")]
         CommandPayload::Blocked => pending().await,
@@ -534,15 +595,30 @@ async fn execute_command(
         CommandPayload::Empty => Err("operation is not implemented in Task 5".to_string()),
         CommandPayload::Utf8(value) => match op {
             Operation::SetWorkspace => core_ops.set_workspace(selection_epoch, value).await,
-            Operation::CreateSession => core_ops.create_session(selection_epoch, value).await,
-            Operation::SendUserMessage => core_ops.send_user_message(value).await,
+            Operation::CreateSession => {
+                let CommandContext::Workspace(workspace) = context else {
+                    return Err("create session is missing its admitted workspace".to_string());
+                };
+                core_ops
+                    .create_session(selection_epoch, workspace, value)
+                    .await
+            }
+            Operation::SendUserMessage => {
+                let CommandContext::Selection(selection) = context else {
+                    return Err("send is missing its admitted session".to_string());
+                };
+                core_ops.send_user_message(selection, value).await
+            }
             Operation::GetAgentSettings => core_ops.get_agent_settings(value).await,
             Operation::ProbeAgent => core_ops.probe_agent(value).await,
             _ => Err("invalid UTF-8 command payload".to_string()),
         },
         CommandPayload::SelectSession(conversation_id) => {
+            let CommandContext::Workspace(workspace) = context else {
+                return Err("select session is missing its admitted workspace".to_string());
+            };
             core_ops
-                .select_session(selection_epoch, conversation_id)
+                .select_session(selection_epoch, workspace, conversation_id)
                 .await
         }
         CommandPayload::AgentSettings { agent, json } => {
@@ -559,18 +635,85 @@ mod tests {
     use std::collections::HashMap;
     use std::num::NonZeroU64;
     use std::sync::atomic::AtomicBool;
-    use std::sync::Arc;
+    use std::sync::{Arc, Mutex};

     use tokio::sync::{mpsc, watch, Notify};
     use tokio::task::JoinSet;

     use super::{
-        execute_command, run_worker, terminalize_task, CommandMetadata, CoreFuture, CoreOps,
-        CoreResult,
+        capture_command_context, execute_command, run_worker, terminalize_task, AppCommandContext,
+        CommandContext, CommandMetadata, CoreFuture, CoreOps, CoreResult,
     };
     use crate::commands::{CommandPayload, Operation, RuntimeCommand};
     use crate::model::{ModelUpdate, OwnedCompletion, OwnedSessionSummary};
     use crate::{CompletionStatus, LifecycleState, SharedModel};
+    use codeg_lib::commands::eui_facade::{EuiSessionSelection, EuiWorkspace};
+    use codeg_lib::models::AgentType;
+
+    fn test_workspace(folder_id: i32, path: &str) -> EuiWorkspace {
+        EuiWorkspace {
+            folder_id,
+            path: std::path::PathBuf::from(path),
+            sessions: Vec::new(),
+        }
+    }
+
+    fn test_selection(
+        workspace: &EuiWorkspace,
+        conversation_id: i32,
+        connection_id: &str,
+    ) -> EuiSessionSelection {
+        EuiSessionSelection {
+            folder_id: workspace.folder_id,
+            path: workspace.path.clone(),
+            conversation_id,
+            title: Some(format!("Session {conversation_id}")),
+            agent_type: AgentType::Codex,
+            status: "active".to_string(),
+            external_session_id: None,
+            updated_at_ms: 1,
+            connection_id: connection_id.to_string(),
+            transcript: Vec::new(),
+        }
+    }
+
+    #[test]
+    fn accepted_commands_keep_their_original_workspace_and_selection() {
+        let workspace_a = test_workspace(11, "/workspace-a");
+        let selection_a = test_selection(&workspace_a, 101, "connection-a");
+        let context = Arc::new(Mutex::new(AppCommandContext {
+            selection_epoch: 7,
+            workspace: Some(workspace_a.clone()),
+            selection: Some(selection_a),
+        }));
+
+        let create_context = capture_command_context(&context, 7, Operation::CreateSession);
+        let send_context = capture_command_context(&context, 7, Operation::SendUserMessage);
+
+        let workspace_b = test_workspace(22, "/workspace-b");
+        let selection_b = test_selection(&workspace_b, 202, "connection-b");
+        *context.lock().unwrap() = AppCommandContext {
+            selection_epoch: 8,
+            workspace: Some(workspace_b),
+            selection: Some(selection_b),
+        };
+
+        let CommandContext::Workspace(captured_workspace) = create_context else {
+            panic!("create must capture a workspace");
+        };
+        assert_eq!(captured_workspace.folder_id, 11);
+        assert_eq!(
+            captured_workspace.path,
+            std::path::PathBuf::from("/workspace-a")
+        );
+
+        let CommandContext::Selection(captured_selection) = send_context else {
+            panic!("send must capture a selection");
+        };
+        assert_eq!(captured_selection.folder_id, 11);
+        assert_eq!(captured_selection.conversation_id, 101);
+        assert_eq!(captured_selection.connection_id, "connection-a");
+    }

     struct ErrorOps;

@@ -581,15 +724,25 @@ mod tests {
             Box::pin(async { Err("unexpected workspace".to_string()) })
         }

-        fn create_session(&self, _selection_epoch: u64, _agent: Vec<u8>) -> CoreFuture {
+        fn create_session(
+            &self,
+            _selection_epoch: u64,
+            _workspace: EuiWorkspace,
+            _agent: Vec<u8>,
+        ) -> CoreFuture {
             Box::pin(async { Err("unexpected create".to_string()) })
         }

-        fn select_session(&self, _selection_epoch: u64, _conversation_id: i32) -> CoreFuture {
+        fn select_session(
+            &self,
+            _selection_epoch: u64,
+            _workspace: EuiWorkspace,
+            _conversation_id: i32,
+        ) -> CoreFuture {
             Box::pin(async { Err("unexpected select".to_string()) })
         }

-        fn send_user_message(&self, _text: Vec<u8>) -> CoreFuture {
+        fn send_user_message(&self, _selection: EuiSessionSelection, _text: Vec<u8>) -> CoreFuture {
             Box::pin(async { Err("unexpected send".to_string()) })
         }

@@ -612,6 +765,7 @@ mod tests {
             0,
             Operation::SendUserMessage,
             CommandPayload::Error("expected".to_string()),
+            CommandContext::None,
             Arc::new(ErrorOps),
         )
         .await
@@ -625,6 +779,7 @@ mod tests {
             0,
             Operation::SendUserMessage,
             CommandPayload::Panic,
+            CommandContext::None,
             Arc::new(ErrorOps),
         ))
         .await;
@@ -656,6 +811,7 @@ mod tests {
                 0,
                 Operation::SendUserMessage,
                 payload,
+                CommandContext::None,
                 Arc::new(ErrorOps),
             ));
             metadata.insert(
@@ -694,15 +850,25 @@ mod tests {
             Box::pin(async { Err("unexpected workspace".to_string()) })
         }

-        fn create_session(&self, _selection_epoch: u64, _agent: Vec<u8>) -> CoreFuture {
+        fn create_session(
+            &self,
+            _selection_epoch: u64,
+            _workspace: EuiWorkspace,
+            _agent: Vec<u8>,
+        ) -> CoreFuture {
             Box::pin(async { Err("unexpected create".to_string()) })
         }

-        fn select_session(&self, _selection_epoch: u64, _conversation_id: i32) -> CoreFuture {
+        fn select_session(
+            &self,
+            _selection_epoch: u64,
+            _workspace: EuiWorkspace,
+            _conversation_id: i32,
+        ) -> CoreFuture {
             Box::pin(async { Err("unexpected select".to_string()) })
         }

-        fn send_user_message(&self, _text: Vec<u8>) -> CoreFuture {
+        fn send_user_message(&self, _selection: EuiSessionSelection, _text: Vec<u8>) -> CoreFuture {
             Box::pin(async { Err("unexpected send".to_string()) })
         }

@@ -749,6 +915,7 @@ mod tests {
                 selection_epoch: 0,
                 op: Operation::ProbeAgent,
                 payload: CommandPayload::Utf8(b"codex".to_vec()),
+                context: CommandContext::None,
             })
             .await
             .unwrap();
@@ -789,7 +956,12 @@ mod tests {
             Box::pin(async { Err("unexpected workspace".to_string()) })
         }

-        fn create_session(&self, _selection_epoch: u64, _agent: Vec<u8>) -> CoreFuture {
+        fn create_session(
+            &self,
+            _selection_epoch: u64,
+            _workspace: EuiWorkspace,
+            _agent: Vec<u8>,
+        ) -> CoreFuture {
             let started = Arc::clone(&self.started);
             let gate = Arc::clone(&self.gate);
             Box::pin(async move {
@@ -811,11 +983,16 @@ mod tests {
             })
         }

-        fn select_session(&self, _selection_epoch: u64, _conversation_id: i32) -> CoreFuture {
+        fn select_session(
+            &self,
+            _selection_epoch: u64,
+            _workspace: EuiWorkspace,
+            _conversation_id: i32,
+        ) -> CoreFuture {
             Box::pin(async { Err("unexpected select".to_string()) })
         }

-        fn send_user_message(&self, _text: Vec<u8>) -> CoreFuture {
+        fn send_user_message(&self, _selection: EuiSessionSelection, _text: Vec<u8>) -> CoreFuture {
             Box::pin(async { Err("unexpected send".to_string()) })
         }

@@ -863,6 +1040,7 @@ mod tests {
                 selection_epoch: create_epoch,
                 op: Operation::CreateSession,
                 payload: CommandPayload::Utf8(b"codex".to_vec()),
+                context: CommandContext::Workspace(test_workspace(1, "/workspace")),
             })
             .await
             .unwrap();
@@ -911,4 +1089,154 @@ mod tests {
         shutdown_tx.send(true).unwrap();
         worker.await.unwrap();
     }
+
+    struct SlowBoundSendOps {
+        started: Arc<Notify>,
+        gate: Arc<Notify>,
+        linked: Arc<Mutex<Vec<(String, i32, i32, String)>>>,
+    }
+
+    impl CoreOps for SlowBoundSendOps {
+        fn begin_selection(&self, _selection_epoch: u64, _op: Operation) {}
+
+        fn set_workspace(&self, _selection_epoch: u64, _path: Vec<u8>) -> CoreFuture {
+            Box::pin(async { Err("unexpected workspace".to_string()) })
+        }
+
+        fn create_session(
+            &self,
+            _selection_epoch: u64,
+            _workspace: EuiWorkspace,
+            _agent: Vec<u8>,
+        ) -> CoreFuture {
+            Box::pin(async { Err("unexpected create".to_string()) })
+        }
+
+        fn select_session(
+            &self,
+            _selection_epoch: u64,
+            _workspace: EuiWorkspace,
+            _conversation_id: i32,
+        ) -> CoreFuture {
+            Box::pin(async { Err("unexpected select".to_string()) })
+        }
+
+        fn send_user_message(&self, selection: EuiSessionSelection, text: Vec<u8>) -> CoreFuture {
+            let started = Arc::clone(&self.started);
+            let gate = Arc::clone(&self.gate);
+            let linked = Arc::clone(&self.linked);
+            Box::pin(async move {
+                started.notify_one();
+                gate.notified().await;
+                linked.lock().unwrap().push((
+                    selection.connection_id,
+                    selection.folder_id,
+                    selection.conversation_id,
+                    String::from_utf8(text).unwrap(),
+                ));
+                Ok(CoreResult::json(Vec::new()))
+            })
+        }
+
+        fn get_agent_settings(&self, _agent: Vec<u8>) -> CoreFuture {
+            Box::pin(async { Err("unexpected get".to_string()) })
+        }
+
+        fn set_agent_settings(&self, _agent: Vec<u8>, _json: Vec<u8>) -> CoreFuture {
+            Box::pin(async { Err("unexpected set".to_string()) })
+        }
+
+        fn probe_agent(&self, _agent: Vec<u8>) -> CoreFuture {
+            Box::pin(async { Err("unexpected probe".to_string()) })
+        }
+    }
+
+    #[tokio::test]
+    async fn admitted_send_keeps_original_ids_and_terminalizes_stale_once() {
+        let started = Arc::new(Notify::new());
+        let gate = Arc::new(Notify::new());
+        let linked = Arc::new(Mutex::new(Vec::new()));
+        let workspace_a = test_workspace(11, "/workspace-a");
+        let selection_a = test_selection(&workspace_a, 101, "connection-a");
+        let command_context = CommandContext::Selection(selection_a);
+        let model = SharedModel::new();
+        let send_id = NonZeroU64::new(61).unwrap();
+        model
+            .reserve(send_id, Operation::SendUserMessage, 0)
+            .unwrap();
+        let (command_tx, command_rx) = mpsc::channel(4);
+        let (shutdown_tx, shutdown_rx) = watch::channel(false);
+        let quiesced = Arc::new(AtomicBool::new(false));
+        let worker = tokio::spawn(run_worker(
+            command_rx,
+            shutdown_rx,
+            model.clone(),
+            codeg_lib::acp::manager::ConnectionManager::new(),
+            Arc::new(std::sync::Mutex::new(())),
+            Arc::clone(&quiesced),
+            Arc::new(SlowBoundSendOps {
+                started: Arc::clone(&started),
+                gate: Arc::clone(&gate),
+                linked: Arc::clone(&linked),
+            }),
+        ));
+        command_tx
+            .send(RuntimeCommand {
+                request_id: send_id,
+                selection_epoch: 0,
+                op: Operation::SendUserMessage,
+                payload: CommandPayload::Utf8(b"hello".to_vec()),
+                context: command_context,
+            })
+            .await
+            .unwrap();
+        started.notified().await;
+
+        let newer_id = NonZeroU64::new(62).unwrap();
+        model
+            .reserve(newer_id, Operation::SelectSession, model.selection_epoch())
+            .unwrap();
+        let newer_epoch = model.selection_epoch();
+        gate.notify_one();
+
+        let mut send_completions = 0;
+        for generation in 1..=100 {
+            tokio::task::yield_now().await;
+            let (frame, _) = model.build_frame(false, &quiesced);
+            let abi = frame.as_abi(LifecycleState::Running, generation, false);
+            assert_eq!(abi.connection_id.len, 0);
+            assert_eq!(abi.transcript_json.len, 0);
+            let completions = if abi.completions_len == 0 {
+                &[][..]
+            } else {
+                unsafe { std::slice::from_raw_parts(abi.completions, abi.completions_len) }
+            };
+            for completion in completions {
+                if completion.request_id == send_id.get() {
+                    send_completions += 1;
+                    assert_eq!(completion.status, CompletionStatus::Stale as u32);
+                }
+            }
+            if send_completions == 1 {
+                break;
+            }
+        }
+        assert_eq!(send_completions, 1);
+        assert_eq!(
+            linked.lock().unwrap().as_slice(),
+            &[("connection-a".to_string(), 11, 101, "hello".to_string())]
+        );
+
+        model.terminalize(
+            newer_epoch,
+            OwnedCompletion::error(
+                newer_id,
+                Operation::SelectSession,
+                "test cleanup".to_string(),
+            ),
+        );
+        let _ = model.build_frame(false, &quiesced);
+        shutdown_tx.send(true).unwrap();
+        worker.await.unwrap();
+    }
 }
diff --git a/src-tauri/src/commands/eui_facade.rs b/src-tauri/src/commands/eui_facade.rs
index f4968d2c..388d2e32 100644
--- a/src-tauri/src/commands/eui_facade.rs
+++ b/src-tauri/src/commands/eui_facade.rs
@@ -7,7 +7,7 @@ use thiserror::Error;
 use crate::acp::preflight::CheckStatus;
 use crate::acp::terminal_context::{build_acp_launch_inputs, AcpRouteRequest};
 use crate::acp::types::{
-    AcpAgentInfo, CodexSandboxSettings, CodexSandboxStructuredConfig, GrokSettings,
+    AcpAgentInfo, AcpEvent, CodexSandboxSettings, CodexSandboxStructuredConfig, GrokSettings,
     GrokStructuredConfig, PromptInputBlock,
 };
 use crate::app_state::AppState;
@@ -23,6 +23,7 @@ use crate::commands::history_window::HistoryLoadOpts;
 use crate::db::entities::conversation::ConversationKind;
 use crate::db::service::conversation_service;
 use crate::models::{AgentType, DbConversationSummary, MessageTurn};
+use crate::web::event_bridge::emit_with_state_gated;

 #[derive(Debug, Clone, PartialEq, Serialize)]
 #[serde(rename_all = "camelCase")]
@@ -92,6 +93,14 @@ pub(crate) trait EuiSessionOps: Send + Sync {

     async fn find_connection(&self, state: &AppState, conversation_id: i32) -> Option<String>;

+    async fn bind_connection(
+        &self,
+        state: &AppState,
+        connection_id: &str,
+        folder_id: i32,
+        conversation_id: i32,
+    ) -> Result<(), EuiFacadeError>;
+
     #[allow(clippy::too_many_arguments)]
     async fn send_linked(
         &self,
@@ -172,6 +181,16 @@ impl EuiSessionOps for ProductionEuiSessionOps {
             .await
     }

+    async fn bind_connection(
+        &self,
+        state: &AppState,
+        connection_id: &str,
+        folder_id: i32,
+        conversation_id: i32,
+    ) -> Result<(), EuiFacadeError> {
+        bind_eui_connection(state, connection_id, folder_id, conversation_id).await
+    }
+
     async fn send_linked(
         &self,
         state: &AppState,
@@ -261,6 +280,13 @@ pub enum EuiFacadeError {
         conversation_id: i32,
         folder_id: i32,
     },
+    #[error("conversation {conversation_id} is not an eligible EUI session")]
+    IneligibleConversation { conversation_id: i32 },
+    #[error("could not bind EUI connection {connection_id}: {reason}")]
+    ConnectionBinding {
+        connection_id: String,
+        reason: String,
+    },
     #[error("EUI application operation failed: {0}")]
     App(#[from] crate::app_error::AppCommandError),
     #[error("EUI database operation failed: {0}")]
@@ -297,7 +323,7 @@ pub async fn set_eui_workspace(
         conversation_service::list_by_folder(&state.db.conn, folder.id, None, None, None, None)
             .await?
             .into_iter()
-            .filter(|row| row.kind == ConversationKind::Regular)
+            .filter(is_eui_session_eligible)
             .map(project_session_summary)
             .collect();
     Ok(EuiWorkspace {
@@ -355,6 +381,13 @@ pub(crate) async fn create_eui_session_with_ops<O: EuiSessionOps>(
             "eui",
         )
         .await?;
+    ops.bind_connection(
+        state,
+        &connection_id,
+        workspace.folder_id,
+        summary.conversation_id,
+    )
+    .await?;
     Ok(selection_from_parts(
         workspace,
         summary,
@@ -391,16 +424,25 @@ pub(crate) async fn select_eui_session_with_ops<O: EuiSessionOps>(
                     loaded.summary.conversation_id,
                 )
                 .await?;
-            ops.spawn_agent(
+            let connection_id = ops
+                .spawn_agent(
+                    state,
+                    loaded.summary.agent_type,
+                    &workspace.path,
+                    loaded.summary.external_session_id.clone(),
+                    loaded.summary.conversation_id,
+                    launch_inputs,
+                    "eui",
+                )
+                .await?;
+            ops.bind_connection(
                 state,
-                loaded.summary.agent_type,
-                &workspace.path,
-                loaded.summary.external_session_id.clone(),
+                &connection_id,
+                workspace.folder_id,
                 loaded.summary.conversation_id,
-                launch_inputs,
-                "eui",
             )
-            .await?
+            .await?;
+            connection_id
         }
     };
     Ok(selection_from_parts(
@@ -442,6 +484,16 @@ pub(crate) async fn load_eui_session(
     workspace: &EuiWorkspace,
     conversation_id: i32,
 ) -> Result<LoadedEuiSession, EuiFacadeError> {
+    let row = conversation_service::get_by_id(&state.db.conn, conversation_id).await?;
+    if row.folder_id != workspace.folder_id {
+        return Err(EuiFacadeError::ConversationOutsideWorkspace {
+            conversation_id,
+            folder_id: workspace.folder_id,
+        });
+    }
+    if !is_eui_session_eligible(&row) {
+        return Err(EuiFacadeError::IneligibleConversation { conversation_id });
+    }
     let detail = get_folder_conversation_with_live_core(
         &state.db.conn,
         &state.connection_manager,
@@ -498,6 +550,70 @@ fn project_session_summary(row: DbConversationSummary) -> EuiSessionSummary {
     }
 }

+fn is_eui_session_eligible(row: &DbConversationSummary) -> bool {
+    row.kind == ConversationKind::Regular
+        && matches!(row.agent_type, AgentType::Codex | AgentType::Grok)
+}
+
+async fn bind_eui_connection(
+    state: &AppState,
+    connection_id: &str,
+    folder_id: i32,
+    conversation_id: i32,
+) -> Result<(), EuiFacadeError> {
+    let (session, emitter) = state
+        .connection_manager
+        .get_state_and_emitter(connection_id)
+        .await
+        .ok_or_else(|| EuiFacadeError::ConnectionBinding {
+            connection_id: connection_id.to_string(),
+            reason: "connection is no longer live".to_string(),
+        })?;
+    {
+        let current = session.read().await;
+        match (current.conversation_id, current.folder_id) {
+            (Some(current_conversation), Some(current_folder))
+                if current_conversation == conversation_id && current_folder == folder_id =>
+            {
+                return Ok(())
+            }
+            (Some(current_conversation), _) => {
+                return Err(EuiFacadeError::ConnectionBinding {
+                    connection_id: connection_id.to_string(),
+                    reason: format!("already belongs to conversation {current_conversation}"),
+                })
+            }
+            _ => {}
+        }
+    }
+
+    let applied = emit_with_state_gated(
+        &session,
+        &emitter,
+        AcpEvent::ConversationLinked {
+            conversation_id,
+            folder_id,
+            parent_conversation_id: None,
+            parent_tool_use_id: None,
+        },
+        |current| current.conversation_id.is_none(),
+    )
+    .await;
+    if applied {
+        return Ok(());
+    }
+
+    let current = session.read().await;
+    if current.conversation_id == Some(conversation_id) && current.folder_id == Some(folder_id) {
+        Ok(())
+    } else {
+        Err(EuiFacadeError::ConnectionBinding {
+            connection_id: connection_id.to_string(),
+            reason: "a concurrent operation bound it to another conversation".to_string(),
+        })
+    }
+}
+
 impl EuiAgentSettingsPatch {
     pub(crate) fn validate_for(&self, agent: AgentType) -> Result<(), EuiFacadeError> {
         match agent {
@@ -697,12 +813,13 @@ fn ensure_supported(agent: AgentType) -> Result<(), EuiFacadeError> {

 #[cfg(test)]
 mod tests {
-    use std::collections::BTreeMap;
+    use std::collections::{BTreeMap, HashMap};
     use std::sync::{Arc, Mutex};

     use super::{
-        create_eui_conversation, create_eui_session_with_ops, ensure_supported, load_eui_session,
-        parse_supported_agent, project_agent_settings, send_eui_message, send_eui_message_with_ops,
+        bind_eui_connection, create_eui_conversation, create_eui_session_with_ops,
+        ensure_supported, load_eui_session, parse_supported_agent, project_agent_settings,
+        select_eui_session_with_ops, send_eui_message, send_eui_message_with_ops,
         set_eui_workspace, EuiAgentSettingsPatch, EuiFacadeError, EuiSessionOps,
         EuiSessionSelection,
     };
@@ -722,6 +839,7 @@ mod tests {
     struct RecordingSessionOps {
         calls: Arc<Mutex<Vec<&'static str>>>,
         last_send: Arc<Mutex<Option<(String, i32, i32, String, String)>>>,
+        live_connections: Arc<Mutex<HashMap<i32, String>>>,
     }

     impl RecordingSessionOps {
@@ -780,13 +898,28 @@ mod tests {
             Ok("recorded-connection".to_string())
         }

-        async fn find_connection(
+        async fn find_connection(&self, _state: &AppState, conversation_id: i32) -> Option<String> {
+            self.record("find_connection");
+            self.live_connections
+                .lock()
+                .unwrap()
+                .get(&conversation_id)
+                .cloned()
+        }
+
+        async fn bind_connection(
             &self,
             _state: &AppState,
-            _conversation_id: i32,
-        ) -> Option<String> {
-            self.record("find_connection");
-            None
+            connection_id: &str,
+            _folder_id: i32,
+            conversation_id: i32,
+        ) -> Result<(), EuiFacadeError> {
+            self.record("bind_connection");
+            self.live_connections
+                .lock()
+                .unwrap()
+                .insert(conversation_id, connection_id.to_string());
+            Ok(())
         }

         async fn send_linked(
@@ -828,7 +961,12 @@ mod tests {

         assert_eq!(
             ops.calls(),
-            ["verify_installed", "build_launch_inputs", "spawn_agent"]
+            [
+                "verify_installed",
+                "build_launch_inputs",
+                "spawn_agent",
+                "bind_connection"
+            ]
         );
         assert!(selection.conversation_id > 0);
         assert_eq!(selection.connection_id, "recorded-connection");
@@ -879,6 +1017,53 @@ mod tests {
         assert_eq!(rows.len(), 1);
     }

+    #[tokio::test]
+    async fn workspace_list_contains_only_supported_regular_sessions() {
+        let root = tempfile::tempdir().unwrap();
+        let workspace_dir = root.path().join("workspace");
+        std::fs::create_dir(&workspace_dir).unwrap();
+        let state = eui_test_state(root.path()).await;
+        let workspace = set_eui_workspace(&state, workspace_dir).await.unwrap();
+
+        let eligible = conversation_service::create(
+            &state.db.conn,
+            workspace.folder_id,
+            AgentType::Codex,
+            None,
+            None,
+        )
+        .await
+        .unwrap();
+        conversation_service::create(
+            &state.db.conn,
+            workspace.folder_id,
+            AgentType::ClaudeCode,
+            None,
+            None,
+        )
+        .await
+        .unwrap();
+        conversation_service::create_chat(
+            &state.db.conn,
+            workspace.folder_id,
+            AgentType::Grok,
+            None,
+            None,
+        )
+        .await
+        .unwrap();
+
+        let workspace = set_eui_workspace(&state, workspace.path).await.unwrap();
+        assert_eq!(
+            workspace
+                .sessions
+                .iter()
+                .map(|session| session.conversation_id)
+                .collect::<Vec<_>>(),
+            [eligible.id]
+        );
+    }
+
     #[tokio::test]
     async fn invalid_workspace_does_not_create_a_folder_row() {
         let root = tempfile::tempdir().unwrap();
@@ -934,6 +1119,114 @@ mod tests {
         assert_eq!(rows.len(), 2);
     }

+    #[tokio::test]
+    async fn selection_rejects_ineligible_rows_before_connection_lookup() {
+        let root = tempfile::tempdir().unwrap();
+        let workspace_dir = root.path().join("workspace");
+        std::fs::create_dir(&workspace_dir).unwrap();
+        let state = eui_test_state(root.path()).await;
+        let workspace = set_eui_workspace(&state, workspace_dir).await.unwrap();
+        let unsupported = conversation_service::create(
+            &state.db.conn,
+            workspace.folder_id,
+            AgentType::ClaudeCode,
+            None,
+            None,
+        )
+        .await
+        .unwrap();
+        let non_regular = conversation_service::create_chat(
+            &state.db.conn,
+            workspace.folder_id,
+            AgentType::Codex,
+            None,
+            None,
+        )
+        .await
+        .unwrap();
+        let ops = RecordingSessionOps::default();
+        ops.live_connections
+            .lock()
+            .unwrap()
+            .insert(unsupported.id, "unsupported-live-connection".to_string());
+
+        assert!(matches!(
+            select_eui_session_with_ops(&state, &workspace, unsupported.id, &ops).await,
+            Err(EuiFacadeError::IneligibleConversation { conversation_id })
+                if conversation_id == unsupported.id
+        ));
+        assert!(ops.calls().is_empty());
+
+        assert!(matches!(
+            select_eui_session_with_ops(&state, &workspace, non_regular.id, &ops).await,
+            Err(EuiFacadeError::IneligibleConversation { conversation_id })
+                if conversation_id == non_regular.id
+        ));
+        assert!(ops.calls().is_empty());
+    }
+
+    #[tokio::test]
+    async fn create_then_select_before_send_reuses_the_spawned_connection() {
+        let root = tempfile::tempdir().unwrap();
+        let workspace_dir = root.path().join("workspace");
+        std::fs::create_dir(&workspace_dir).unwrap();
+        let state = eui_test_state(root.path()).await;
+        let workspace = set_eui_workspace(&state, workspace_dir).await.unwrap();
+        let ops = RecordingSessionOps::default();
+
+        let created = create_eui_session_with_ops(&state, &workspace, AgentType::Codex, &ops)
+            .await
+            .unwrap();
+        ops.calls.lock().unwrap().clear();
+
+        let selected =
+            select_eui_session_with_ops(&state, &workspace, created.conversation_id, &ops)
+                .await
+                .unwrap();
+
+        assert_eq!(selected.connection_id, created.connection_id);
+        assert_eq!(ops.calls(), ["find_connection"]);
+    }
+
+    #[tokio::test]
+    async fn connection_binding_makes_a_spawn_discoverable_before_send() {
+        let root = tempfile::tempdir().unwrap();
+        let workspace_dir = root.path().join("workspace");
+        std::fs::create_dir(&workspace_dir).unwrap();
+        let state = eui_test_state(root.path()).await;
+        let workspace = set_eui_workspace(&state, workspace_dir).await.unwrap();
+        let row = create_eui_conversation(&state, workspace.folder_id, AgentType::Codex)
+            .await
+            .unwrap();
+        let _commands = state
+            .connection_manager
+            .insert_test_connection_live(
+                "eui-pre-send-connection",
+                AgentType::Codex,
+                Some(workspace.path),
+                state.emitter.clone(),
+            )
+            .await;
+
+        bind_eui_connection(
+            &state,
+            "eui-pre-send-connection",
+            workspace.folder_id,
+            row.conversation_id,
+        )
+        .await
+        .unwrap();
+
+        assert_eq!(
+            state
+                .connection_manager
+                .find_connection_by_conversation_id(row.conversation_id)
+                .await
+                .as_deref(),
+            Some("eui-pre-send-connection")
+        );
+    }
+
     #[tokio::test]
     async fn history_projection_is_backend_message_turn_json() {
         let root = tempfile::tempdir().unwrap();
```
