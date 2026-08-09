# Task 5 Review Package
BASE: 29904a3a8fe6a741372809dfccb08f7a2e194e9f HEAD: 624fa8c37c82233a07eaa25cfc166992ee8c9c96
Parent: SKIP all full cargo test
624fa8c3 feat(eui): add workspace and session command loop
 .../task-5-report.md                               | 152 +++++
 codeg-eui/app/bridge/codeg_eui_bridge.h            |   3 +
 src-tauri/codeg-eui-core/src/abi.rs                |  23 +-
 src-tauri/codeg-eui-core/src/commands.rs           |   9 +
 src-tauri/codeg-eui-core/src/model.rs              | 106 ++-
 src-tauri/codeg-eui-core/src/runtime.rs            | 462 ++++++++++++-
 src-tauri/codeg-eui-core/tests/session_contract.rs | 168 +++++
 src-tauri/src/commands/eui_facade.rs               | 746 ++++++++++++++++++++-
 8 files changed, 1626 insertions(+), 43 deletions(-)
diff --git a/.superpowers/sdd/2026-08-09-eui-neo-frontend-spike/task-5-report.md b/.superpowers/sdd/2026-08-09-eui-neo-frontend-spike/task-5-report.md
new file mode 100644
index 00000000..ace66378
--- /dev/null
+++ b/.superpowers/sdd/2026-08-09-eui-neo-frontend-spike/task-5-report.md
@@ -0,0 +1,152 @@
+# Task 5 Implementer Report
+
+## Status
+
+DONE_WITH_CONCERNS
+
+Task 5 adds the EUI workspace/session command loop over the existing folder,
+conversation, ACP connection, history, and linked-send cores. Selection-changing
+requests advance the model epoch at acceptance, and stale worker results remain
+exactly-once terminal completions without overwriting the active projection.
+
+## Implementation
+
+- Added narrow `EuiWorkspace`, `EuiSessionSummary`, and
+  `EuiSessionSelection` DTOs. They expose canonical workspace identity,
+  persisted session fields, connection identity, and backend `MessageTurn`
+  history without exposing AppState, database handles, or parsers.
+- Canonicalized and verified existing directories before calling
+  `open_folder_core`; invalid and non-directory paths cannot create folder
+  rows. Workspace selection projects regular persisted conversations in
+  activity order.
+- Restricted conversation/session creation to Grok and Codex, delegated row
+  creation to `create_project_conversation_core`, and delegated history loading
+  to `get_folder_conversation_with_live_core` with a 100-user-turn window.
+- Added an injected `EuiSessionOps` seam. Production session creation performs
+  `verify_agent_installed`, builds launch inputs with
+  `AcpRouteRequest::root(Some(conversation_id), None)`, loads the persisted user
+  launch context, and calls `spawn_agent` with owner `"eui"` and no delegation
+  override. A recording test proves verify/build/spawn order and arguments.
+- Session selection reuses a live connection by conversation ID or resumes via
+  the persisted external session ID. Sends build exactly one text block, create
+  a UUID client message ID, and call
+  `send_prompt_linked_with_message_id` with the selected folder/conversation.
+- Routed set-workspace, create-session, select-session, and send operations
+  through asynchronous `CoreOps` workers. Successful create/select completion
+  JSON includes `conversationId` and `connectionId`; model session/transcript
+  projections are applied only at the captured selection epoch.
+- Advanced `selection_epoch` atomically with accepted workspace/create/select
+  completion reservation, cleared the previous active projection immediately,
+  and added a gated slow-create contract proving one stale completion and no
+  stale model application after a newer selection.
+- Recorded `t0_ns` immediately after successful send enqueue. Positive
+  conversation IDs are validated inside the standard UI-thread/lifecycle ABI
+  admission guard, preserving Task 3 error precedence.
+- Added `session_contract.rs` for real ABI workspace selection, canonical path
+  JSON, epoch/session projection, invalid workspace terminalization, and
+  pre-accept invalid conversation IDs. Updated the public header's async
+  session completion/timing documentation.
+
+## TDD Evidence
+
+### RED
+
+The actual `model.rs` was compiled in isolation against its narrow ABI/command
+boundary before the epoch implementation. The focused test
+`accepted_workspace_and_session_changes_advance_the_selection_epoch` failed as
+intended with `left: 0`, `right: 1`.
+
+The dependency-complete `session_contract` target was also attempted before
+implementation, but the kernel killed shared-codeg `rustc` before the test
+binary linked. That host failure is not counted as behavioral RED evidence.
+
+### GREEN
+
+- Actual Task 5 `abi.rs`, `commands.rs`, `model.rs`, and `runtime.rs` compiled
+  with `-D warnings` against the established narrow shared-core boundary; **9/9
+  focused unit tests passed**.
+- Actual `eui_facade.rs`, including its test module, compiled with `-D warnings`
+  against shape-compatible existing-core signatures. The deterministic
+  create/send orchestration test passed (**1/1**).
+- The complete committed `session_contract.rs` compiled with `-D warnings`
+  against the actual ABI/runtime/model modules.
+- Contracts-only CMake/CTest passed **3/3** (harness, ABI layout, UI snapshot).
+
+Shape-compatible probes validate the actual changed modules and their boundary
+types, but do not replace compiling/running them against the complete shared
+`codeg` crate.
+
+## Verification
+
+Passed:
+
+- `cargo fmt --check` for the shared facade and standalone EUI crate files.
+- Actual-source facade check with `RUSTFLAGS='-D warnings'`.
+- Actual-source ABI/runtime/model tests with `RUSTFLAGS='-D warnings'`: **9/9**.
+- Deterministic session orchestration test: **1/1**.
+- Actual `session_contract.rs` compile-only check with `-D warnings`.
+- Fresh contracts-only CMake build and CTest: **3/3**.
+- `git diff --check`.
+- Approved design SHA-256 matched
+  `b3446ec31cc8b0457ed1ca3e7c6e8b3ec421eb4b997af6efae3d8975ecd2bdef`.
+- No standalone `src-tauri/codeg-eui-core/Cargo.lock` remains.
+
+Per parent instruction, **all full Cargo tests were skipped**. No full package,
+workspace, or `cargo test --lib --features test-utils` command was run.
+
+A one-job, non-incremental, no-debug dependency-complete standalone-crate
+`cargo check` reached the shared `codeg` crate with no emitted Rust diagnostic,
+then the kernel OOM-killed `rustc`. Kernel evidence records approximately
+3.07 GiB anonymous RSS for that compiler on a 3.8 GiB host with no swap.
+
+## Files Changed
+
+- `src-tauri/src/commands/eui_facade.rs`
+- `src-tauri/codeg-eui-core/src/commands.rs`
+- `src-tauri/codeg-eui-core/src/model.rs`
+- `src-tauri/codeg-eui-core/src/runtime.rs`
+- `src-tauri/codeg-eui-core/src/abi.rs`
+- `src-tauri/codeg-eui-core/tests/session_contract.rs`
+- `codeg-eui/app/bridge/codeg_eui_bridge.h`
+- `.superpowers/sdd/2026-08-09-eui-neo-frontend-spike/task-5-report.md`
+
+`runtime.rs` is a scoped Task 5 dependency even though the brief's enumerated
+file list omits it: Task 4 established the asynchronous worker dispatch in that
+module, and Task 5 must extend that dispatch to execute the already-exposed
+workspace/session/send enqueue operations and apply their epoch-guarded model
+updates. No unrelated runtime behavior was changed.
+
+## Self-Review
+
+- Workspace validation precedes folder persistence; only regular conversations
+  enter the EUI session list.
+- Grok/Codex guards execute before conversation or ACP access. The facade adds
+  no direct persistence schema, parser, Axum/Tauri handler call, or filesystem
+  write path.
+- Create/resume launch uses the selected absolute workspace, persisted external
+  ID, root route with no override, user launch context, owner `"eui"`, and no
+  parent/operation ownership.
+- Linked sends carry one text block, a UUID client ID, and the exact selected
+  folder/conversation/connection IDs.
+- Selection epoch advancement and completion reservation share one model lock.
+  Stale results never mutate sessions, connection ID, or transcript, but still
+  drain once through the existing completion ledger.
+- The worker context is invalidated synchronously at accepted selection change,
+  preventing sends from borrowing an old selection while new selection work is
+  in flight.
+- ABI input validation stays inside panic containment and the Task 3
+  UI-thread/lifecycle checks. Existing frame layout and header constants remain
+  unchanged.
+- Generated Cargo/CMake outputs and temporary probe crates are excluded from
+  the implementation package.
+
+## Concern
+
+Dependency-complete shared-codeg compilation and execution of the real
+`session_contract`/facade tests must be rerun on a host with more memory or
+usable swap. The focused actual-source probes and C++ contracts are green; the
+remaining limitation is host capacity, not a known Task 5 diagnostic.
+
+<!-- codeg-card-summary-v1
+{"kind":"implementation","phase":"implementation","status":"done_with_concerns","summary":"Added the EUI workspace/session/history/send command loop with canonical workspace persistence, Grok/Codex ACP orchestration, epoch-safe model projection, and linked send timing admission.","commits":[{"subject":"feat(eui): add workspace and session command loop"}],"tests":{"status":"partial","passed":13,"failed":0,"summary":"9 actual-source ABI/runtime/model tests, 1 deterministic facade orchestration test, and 3 contracts-only CTest cases pass; the real session contract compiles against the focused boundary, while dependency-complete shared-codeg checking is host-OOM-limited."},"concerns":["Dependency-complete session_contract and shared-codeg verification require more than the available 3.8 GiB memory or usable swap."],"report_file":".superpowers/sdd/2026-08-09-eui-neo-frontend-spike/task-5-report.md"}
+-->
diff --git a/codeg-eui/app/bridge/codeg_eui_bridge.h b/codeg-eui/app/bridge/codeg_eui_bridge.h
index cbe696b5..a938a8dd 100644
--- a/codeg-eui/app/bridge/codeg_eui_bridge.h
+++ b/codeg-eui/app/bridge/codeg_eui_bridge.h
@@ -100,6 +100,9 @@ int codeg_eui_init(const uint8_t* data_dir_utf8, size_t data_dir_len);
 int codeg_eui_poll(CodegEuiFrame* out);
 int codeg_eui_begin_shutdown(void);
 int codeg_eui_shutdown(void);
+/* Session operations are asynchronous. Successful workspace/create/select
+ * completions carry JSON; create/select include conversationId and
+ * connectionId. A successful send admission updates CodegEuiFrame::t0_ns. */
 int codeg_eui_set_workspace(const uint8_t* path_utf8,
                             size_t path_len,
                             uint64_t* out_request_id);
diff --git a/src-tauri/codeg-eui-core/src/abi.rs b/src-tauri/codeg-eui-core/src/abi.rs
index 905c1f55..5050dfe0 100644
--- a/src-tauri/codeg-eui-core/src/abi.rs
+++ b/src-tauri/codeg-eui-core/src/abi.rs
@@ -332,11 +332,24 @@ pub extern "C" fn codeg_eui_create_session(
 
 #[no_mangle]
 pub extern "C" fn codeg_eui_select_session(conversation_id: i32, out_request_id: *mut u64) -> i32 {
-    enqueue_payload(
-        out_request_id,
-        Operation::SelectSession,
-        CommandPayload::SelectSession(conversation_id),
-    )
+    ffi_guard(|| {
+        let mut slot = lock_bridge();
+        if let Err(error) = ensure_running(&slot) {
+            return error;
+        }
+        if out_request_id.is_null() {
+            return CODEG_EUI_ERR_NULL_POINTER;
+        }
+        if conversation_id <= 0 {
+            return CODEG_EUI_ERR_INVALID_STATE;
+        }
+        accept_and_write(
+            &mut slot,
+            out_request_id,
+            Operation::SelectSession,
+            CommandPayload::SelectSession(conversation_id),
+        )
+    })
 }
 
 #[no_mangle]
diff --git a/src-tauri/codeg-eui-core/src/commands.rs b/src-tauri/codeg-eui-core/src/commands.rs
index 327b2b6c..806af1c3 100644
--- a/src-tauri/codeg-eui-core/src/commands.rs
+++ b/src-tauri/codeg-eui-core/src/commands.rs
@@ -16,6 +16,15 @@ pub enum Operation {
     ProbeAgent = 8,
 }
 
+impl Operation {
+    pub(crate) fn changes_selection(self) -> bool {
+        matches!(
+            self,
+            Self::SetWorkspace | Self::CreateSession | Self::SelectSession
+        )
+    }
+}
+
 pub(crate) enum CommandPayload {
     Empty,
     Utf8(Vec<u8>),
diff --git a/src-tauri/codeg-eui-core/src/model.rs b/src-tauri/codeg-eui-core/src/model.rs
index 1af919f5..eace9ea2 100644
--- a/src-tauri/codeg-eui-core/src/model.rs
+++ b/src-tauri/codeg-eui-core/src/model.rs
@@ -5,7 +5,7 @@ use std::sync::{Arc, Mutex, MutexGuard};
 
 use crate::abi::{CodegEuiFrame, LifecycleState, CODEG_EUI_API_VERSION};
 use crate::commands::Operation;
-use crate::{CODEG_EUI_COMPLETION_CAPACITY, CODEG_EUI_ERR_QUEUE_FULL};
+use crate::{CODEG_EUI_COMPLETION_CAPACITY, CODEG_EUI_ERR_INTERNAL, CODEG_EUI_ERR_QUEUE_FULL};
 
 pub const CODEG_EUI_COMPLETION_OK: u32 = CompletionStatus::Ok as u32;
 pub const CODEG_EUI_COMPLETION_ERROR: u32 = CompletionStatus::Error as u32;
@@ -56,6 +56,17 @@ pub struct OwnedSessionSummary {
     pub updated_at_ms: i64,
 }
 
+pub(crate) enum ModelUpdate {
+    Workspace {
+        sessions: Vec<OwnedSessionSummary>,
+    },
+    Selection {
+        sessions: Vec<OwnedSessionSummary>,
+        connection_id: Vec<u8>,
+        transcript_json: Vec<u8>,
+    },
+}
+
 #[derive(Clone, Debug)]
 pub(crate) struct OwnedCompletion {
     pub request_id: NonZeroU64,
@@ -214,12 +225,64 @@ impl SharedModel {
         op: Operation,
         selection_epoch: u64,
     ) -> Result<(), i32> {
-        self.lock().ledger.reserve(request_id, op, selection_epoch)
+        let mut state = self.lock();
+        let changes_selection = op.changes_selection();
+        let captured_epoch = if changes_selection {
+            state
+                .selection_epoch
+                .checked_add(1)
+                .ok_or(CODEG_EUI_ERR_INTERNAL)?
+        } else {
+            selection_epoch
+        };
+        state.ledger.reserve(request_id, op, captured_epoch)?;
+        if changes_selection {
+            state.selection_epoch = captured_epoch;
+            if op == Operation::SetWorkspace {
+                state.sessions.clear();
+            }
+            state.connection_id.clear();
+            state.event_seq = 0;
+            state.transcript_json.clear();
+            state.live_assistant.clear();
+            state.stream_active = false;
+            state.needs_resync = false;
+            state.t0_ns = 0;
+            state.t_first_token_ns = 0;
+            state.t_end_ns = 0;
+        }
+        Ok(())
     }
 
     pub(crate) fn terminalize(&self, captured_selection_epoch: u64, completion: OwnedCompletion) {
+        self.terminalize_with_update(captured_selection_epoch, completion, None);
+    }
+
+    pub(crate) fn terminalize_with_update(
+        &self,
+        captured_selection_epoch: u64,
+        completion: OwnedCompletion,
+        update: Option<ModelUpdate>,
+    ) {
         let mut state = self.lock();
         let current_selection_epoch = state.selection_epoch;
+        if captured_selection_epoch == current_selection_epoch {
+            match update {
+                Some(ModelUpdate::Workspace { sessions }) => {
+                    state.sessions = sessions;
+                }
+                Some(ModelUpdate::Selection {
+                    sessions,
+                    connection_id,
+                    transcript_json,
+                }) => {
+                    state.sessions = sessions;
+                    state.connection_id = connection_id;
+                    state.transcript_json = transcript_json;
+                }
+                None => {}
+            }
+        }
         state.ledger.terminalize(
             current_selection_epoch,
             captured_selection_epoch,
@@ -231,6 +294,13 @@ impl SharedModel {
         self.lock().ledger.cancel_all();
     }
 
+    pub(crate) fn record_send_accepted(&self, t0_ns: u64) {
+        let mut state = self.lock();
+        state.t0_ns = t0_ns;
+        state.t_first_token_ns = 0;
+        state.t_end_ns = 0;
+    }
+
     pub fn set_error_strip(&self, message: Vec<u8>) {
         self.lock().error_strip = message;
     }
@@ -405,6 +475,38 @@ mod tests {
     use super::{CompletionStatus, OwnedCompletion, SharedModel};
     use crate::commands::Operation;
 
+    #[test]
+    fn accepted_workspace_and_session_changes_advance_the_selection_epoch() {
+        let model = SharedModel::new();
+
+        model
+            .reserve(
+                NonZeroU64::new(1).unwrap(),
+                Operation::SetWorkspace,
+                model.selection_epoch(),
+            )
+            .unwrap();
+        assert_eq!(model.selection_epoch(), 1);
+
+        model
+            .reserve(
+                NonZeroU64::new(2).unwrap(),
+                Operation::CreateSession,
+                model.selection_epoch(),
+            )
+            .unwrap();
+        assert_eq!(model.selection_epoch(), 2);
+
+        model
+            .reserve(
+                NonZeroU64::new(3).unwrap(),
+                Operation::SelectSession,
+                model.selection_epoch(),
+            )
+            .unwrap();
+        assert_eq!(model.selection_epoch(), 3);
+    }
+
     #[test]
     fn selection_changes_mark_one_terminal_completion_stale() {
         let model = SharedModel::new();
diff --git a/src-tauri/codeg-eui-core/src/runtime.rs b/src-tauri/codeg-eui-core/src/runtime.rs
index 4922c487..7fa3ee83 100644
--- a/src-tauri/codeg-eui-core/src/runtime.rs
+++ b/src-tauri/codeg-eui-core/src/runtime.rs
@@ -14,15 +14,34 @@ use tokio::sync::{mpsc, watch};
 use tokio::task::{Id, JoinHandle, JoinSet};
 
 use crate::commands::{CommandPayload, Operation, RuntimeCommand};
-use crate::model::{OwnedCompletion, SharedModel};
+use crate::model::{ModelUpdate, OwnedCompletion, OwnedSessionSummary, SharedModel};
 use crate::{
     EuiBootstrap, CODEG_EUI_COMMAND_QUEUE_CAPACITY, CODEG_EUI_ERR_INTERNAL,
     CODEG_EUI_ERR_INVALID_STATE, CODEG_EUI_ERR_QUEUE_FULL,
 };
 
-pub(crate) type CoreFuture = Pin<Box<dyn Future<Output = Result<Vec<u8>, String>> + Send>>;
+pub(crate) type CoreFuture = Pin<Box<dyn Future<Output = Result<CoreResult, String>> + Send>>;
+
+pub(crate) struct CoreResult {
+    payload: Vec<u8>,
+    update: Option<ModelUpdate>,
+}
+
+impl CoreResult {
+    fn json(payload: Vec<u8>) -> Self {
+        Self {
+            payload,
+            update: None,
+        }
+    }
+}
 
 pub(crate) trait CoreOps: Send + Sync {
+    fn begin_selection(&self, selection_epoch: u64, op: Operation);
+    fn set_workspace(&self, selection_epoch: u64, path: Vec<u8>) -> CoreFuture;
+    fn create_session(&self, selection_epoch: u64, agent: Vec<u8>) -> CoreFuture;
+    fn select_session(&self, selection_epoch: u64, conversation_id: i32) -> CoreFuture;
+    fn send_user_message(&self, text: Vec<u8>) -> CoreFuture;
     fn get_agent_settings(&self, agent: Vec<u8>) -> CoreFuture;
     fn set_agent_settings(&self, agent: Vec<u8>, json: Vec<u8>) -> CoreFuture;
     fn probe_agent(&self, agent: Vec<u8>) -> CoreFuture;
@@ -30,9 +49,115 @@ pub(crate) trait CoreOps: Send + Sync {
 
 struct AppCoreOps {
     state: Arc<codeg_lib::app_state::AppState>,
+    context: Arc<Mutex<AppCommandContext>>,
+}
+
+#[derive(Default)]
+struct AppCommandContext {
+    selection_epoch: u64,
+    workspace: Option<codeg_lib::commands::eui_facade::EuiWorkspace>,
+    selection: Option<codeg_lib::commands::eui_facade::EuiSessionSelection>,
 }
 
 impl CoreOps for AppCoreOps {
+    fn begin_selection(&self, selection_epoch: u64, op: Operation) {
+        let mut context = self
+            .context
+            .lock()
+            .unwrap_or_else(|error| error.into_inner());
+        context.selection_epoch = selection_epoch;
+        context.selection = None;
+        if op == Operation::SetWorkspace {
+            context.workspace = None;
+        }
+    }
+
+    fn set_workspace(&self, selection_epoch: u64, path: Vec<u8>) -> CoreFuture {
+        let state = Arc::clone(&self.state);
+        let context = Arc::clone(&self.context);
+        Box::pin(async move {
+            let path = String::from_utf8(path).map_err(|_| "workspace is not UTF-8".to_string())?;
+            let workspace = codeg_lib::commands::eui_facade::set_eui_workspace(
+                &state,
+                std::path::PathBuf::from(path),
+            )
+            .await
+            .map_err(|error| error.to_string())?;
+            let payload = serde_json::to_vec(&workspace).map_err(|error| error.to_string())?;
+            let sessions = owned_sessions(&workspace.sessions);
+            let mut current = context.lock().unwrap_or_else(|error| error.into_inner());
+            if selection_epoch == current.selection_epoch {
+                current.selection_epoch = selection_epoch;
+                current.workspace = Some(workspace);
+                current.selection = None;
+            }
+            Ok(CoreResult {
+                payload,
+                update: Some(ModelUpdate::Workspace { sessions }),
+            })
+        })
+    }
+
+    fn create_session(&self, selection_epoch: u64, agent: Vec<u8>) -> CoreFuture {
+        let state = Arc::clone(&self.state);
+        let context = Arc::clone(&self.context);
+        Box::pin(async move {
+            let wire = String::from_utf8(agent).map_err(|_| "agent is not UTF-8".to_string())?;
+            let agent = codeg_lib::commands::eui_facade::parse_supported_agent(&wire)
+                .map_err(|error| error.to_string())?;
+            let workspace = context
+                .lock()
+                .unwrap_or_else(|error| error.into_inner())
+                .workspace
+                .clone()
+                .ok_or_else(|| "no EUI workspace is selected".to_string())?;
+            let selection =
+                codeg_lib::commands::eui_facade::create_eui_session(&state, &workspace, agent)
+                    .await
+                    .map_err(|error| error.to_string())?;
+            selection_result(context, selection_epoch, workspace, selection)
+        })
+    }
+
+    fn select_session(&self, selection_epoch: u64, conversation_id: i32) -> CoreFuture {
+        let state = Arc::clone(&self.state);
+        let context = Arc::clone(&self.context);
+        Box::pin(async move {
+            let workspace = context
+                .lock()
+                .unwrap_or_else(|error| error.into_inner())
+                .workspace
+                .clone()
+                .ok_or_else(|| "no EUI workspace is selected".to_string())?;
+            let selection = codeg_lib::commands::eui_facade::select_eui_session(
+                &state,
+                &workspace,
+                conversation_id,
+            )
+            .await
+            .map_err(|error| error.to_string())?;
+            selection_result(context, selection_epoch, workspace, selection)
+        })
+    }
+
+    fn send_user_message(&self, text: Vec<u8>) -> CoreFuture {
+        let state = Arc::clone(&self.state);
+        let context = Arc::clone(&self.context);
+        Box::pin(async move {
+            let text = String::from_utf8(text).map_err(|_| "message is not UTF-8".to_string())?;
+            let selection = context
+                .lock()
+                .unwrap_or_else(|error| error.into_inner())
+                .selection
+                .clone()
+                .ok_or_else(|| "no EUI session is selected".to_string())?;
+            codeg_lib::commands::eui_facade::send_eui_message(&state, &selection, text)
+                .await
+                .map_err(|error| error.to_string())?;
+            Ok(CoreResult::json(Vec::new()))
+        })
+    }
+
     fn get_agent_settings(&self, agent: Vec<u8>) -> CoreFuture {
         let state = Arc::clone(&self.state);
         Box::pin(async move {
@@ -42,7 +167,9 @@ impl CoreOps for AppCoreOps {
             let settings = codeg_lib::commands::eui_facade::get_eui_agent_settings(&state, agent)
                 .await
                 .map_err(|error| error.to_string())?;
-            serde_json::to_vec(&settings).map_err(|error| error.to_string())
+            serde_json::to_vec(&settings)
+                .map(CoreResult::json)
+                .map_err(|error| error.to_string())
         })
     }
 
@@ -60,7 +187,9 @@ impl CoreOps for AppCoreOps {
                 codeg_lib::commands::eui_facade::set_eui_agent_settings(&state, agent, patch)
                     .await
                     .map_err(|error| error.to_string())?;
-            serde_json::to_vec(&settings).map_err(|error| error.to_string())
+            serde_json::to_vec(&settings)
+                .map(CoreResult::json)
+                .map_err(|error| error.to_string())
         })
     }
 
@@ -73,11 +202,71 @@ impl CoreOps for AppCoreOps {
             let probe = codeg_lib::commands::eui_facade::probe_eui_agent(&state, agent)
                 .await
                 .map_err(|error| error.to_string())?;
-            serde_json::to_vec(&probe).map_err(|error| error.to_string())
+            serde_json::to_vec(&probe)
+                .map(CoreResult::json)
+                .map_err(|error| error.to_string())
         })
     }
 }
 
+fn selection_result(
+    context: Arc<Mutex<AppCommandContext>>,
+    selection_epoch: u64,
+    mut workspace: codeg_lib::commands::eui_facade::EuiWorkspace,
+    selection: codeg_lib::commands::eui_facade::EuiSessionSelection,
+) -> Result<CoreResult, String> {
+    let summary = codeg_lib::commands::eui_facade::EuiSessionSummary {
+        conversation_id: selection.conversation_id,
+        title: selection.title.clone(),
+        agent_type: selection.agent_type,
+        status: selection.status.clone(),
+        external_session_id: selection.external_session_id.clone(),
+        updated_at_ms: selection.updated_at_ms,
+    };
+    if let Some(existing) = workspace
+        .sessions
+        .iter_mut()
+        .find(|item| item.conversation_id == summary.conversation_id)
+    {
+        *existing = summary;
+    } else {
+        workspace.sessions.insert(0, summary);
+    }
+    let payload = serde_json::to_vec(&selection).map_err(|error| error.to_string())?;
+    let transcript_json =
+        serde_json::to_vec(&selection.transcript).map_err(|error| error.to_string())?;
+    let sessions = owned_sessions(&workspace.sessions);
+    let connection_id = selection.connection_id.as_bytes().to_vec();
+    let mut current = context.lock().unwrap_or_else(|error| error.into_inner());
+    if selection_epoch == current.selection_epoch {
+        current.selection_epoch = selection_epoch;
+        current.workspace = Some(workspace);
+        current.selection = Some(selection);
+    }
+    Ok(CoreResult {
+        payload,
+        update: Some(ModelUpdate::Selection {
+            sessions,
+            connection_id,
+            transcript_json,
+        }),
+    })
+}
+
+fn owned_sessions(
+    sessions: &[codeg_lib::commands::eui_facade::EuiSessionSummary],
+) -> Vec<OwnedSessionSummary> {
+    sessions
+        .iter()
+        .map(|session| OwnedSessionSummary {
+            conversation_id: session.conversation_id,
+            title: session.title.clone().unwrap_or_default().into_bytes(),
+            agent: session.agent_type.as_wire().as_bytes().to_vec(),
+            updated_at_ms: session.updated_at_ms,
+        })
+        .collect()
+}
+
 static NEXT_REQUEST_ID: AtomicU64 = AtomicU64::new(1);
 
 #[derive(Clone, Copy)]
@@ -108,6 +297,7 @@ pub(crate) struct RuntimeOwner {
     bootstrap: EuiBootstrap,
     model: SharedModel,
     command_tx: Option<mpsc::Sender<RuntimeCommand>>,
+    core_ops: Arc<dyn CoreOps>,
     shutdown_tx: Option<watch::Sender<bool>>,
     worker: JoinHandle<()>,
     admission: Arc<Mutex<()>>,
@@ -123,6 +313,7 @@ impl RuntimeOwner {
         let connections = bootstrap.state.connection_manager.clone_ref();
         let core_ops: Arc<dyn CoreOps> = Arc::new(AppCoreOps {
             state: Arc::clone(&bootstrap.state),
+            context: Arc::new(Mutex::new(AppCommandContext::default())),
         });
         let worker = bootstrap.runtime_handle().spawn(run_worker(
             command_rx,
@@ -131,13 +322,14 @@ impl RuntimeOwner {
             connections,
             Arc::clone(&admission),
             Arc::clone(&quiesced),
-            core_ops,
+            Arc::clone(&core_ops),
         ));
 
         Self {
             bootstrap,
             model,
             command_tx: Some(command_tx),
+            core_ops,
             shutdown_tx: Some(shutdown_tx),
             worker,
             admission,
@@ -172,12 +364,19 @@ impl RuntimeOwner {
         let request_id = next_request_id()?;
         let selection_epoch = model.selection_epoch();
         model.reserve(request_id, op, selection_epoch)?;
+        let selection_epoch = model.selection_epoch();
+        if op.changes_selection() {
+            self.core_ops.begin_selection(selection_epoch, op);
+        }
         permit.send(RuntimeCommand {
             request_id,
             selection_epoch,
             op,
             payload,
         });
+        if op == Operation::SendUserMessage {
+            model.record_send_accepted(native_timestamp_ns());
+        }
         Ok(request_id)
     }
 
@@ -206,6 +405,14 @@ impl RuntimeOwner {
     }
 }
 
+fn native_timestamp_ns() -> u64 {
+    std::time::SystemTime::now()
+        .duration_since(std::time::UNIX_EPOCH)
+        .unwrap_or_default()
+        .as_nanos()
+        .min(u64::MAX as u128) as u64
+}
+
 fn next_request_id() -> Result<NonZeroU64, i32> {
     let value = NEXT_REQUEST_ID
         .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
@@ -252,7 +459,12 @@ async fn run_worker(
                     selection_epoch: command.selection_epoch,
                     op: command.op,
                 };
-                let abort = tasks.spawn(execute_command(command.op, command.payload, Arc::clone(&core_ops)));
+                let abort = tasks.spawn(execute_command(
+                    command.selection_epoch,
+                    command.op,
+                    command.payload,
+                    Arc::clone(&core_ops),
+                ));
                 metadata.insert(abort.id(), command_metadata);
             }
         }
@@ -278,7 +490,7 @@ async fn run_worker(
 fn terminalize_task(
     model: &SharedModel,
     metadata: &mut HashMap<Id, CommandMetadata>,
-    completed: Option<Result<(Id, Result<Vec<u8>, String>), tokio::task::JoinError>>,
+    completed: Option<Result<(Id, Result<CoreResult, String>), tokio::task::JoinError>>,
 ) {
     let Some(completed) = completed else {
         return;
@@ -293,18 +505,25 @@ fn terminalize_task(
     let command = metadata
         .remove(&task_id)
         .expect("metadata exists for every worker task");
-    let completion = match result {
-        Ok(payload) => OwnedCompletion::ok(command.request_id, command.op, payload),
-        Err(error) => OwnedCompletion::error(command.request_id, command.op, error),
-    };
-    model.terminalize(command.selection_epoch, completion);
+    match result {
+        Ok(result) => model.terminalize_with_update(
+            command.selection_epoch,
+            OwnedCompletion::ok(command.request_id, command.op, result.payload),
+            result.update,
+        ),
+        Err(error) => model.terminalize(
+            command.selection_epoch,
+            OwnedCompletion::error(command.request_id, command.op, error),
+        ),
+    }
 }
 
 async fn execute_command(
+    selection_epoch: u64,
     op: Operation,
     payload: CommandPayload,
     core_ops: Arc<dyn CoreOps>,
-) -> Result<Vec<u8>, String> {
+) -> Result<CoreResult, String> {
     match payload {
         #[cfg(feature = "ffi-test-hooks")]
         CommandPayload::Blocked => pending().await,
@@ -312,15 +531,19 @@ async fn execute_command(
         CommandPayload::Error(error) => Err(error),
         #[cfg(test)]
         CommandPayload::Panic => panic!("test worker panic"),
-        CommandPayload::Empty => Err("operation is not implemented in Task 3".to_string()),
+        CommandPayload::Empty => Err("operation is not implemented in Task 5".to_string()),
         CommandPayload::Utf8(value) => match op {
+            Operation::SetWorkspace => core_ops.set_workspace(selection_epoch, value).await,
+            Operation::CreateSession => core_ops.create_session(selection_epoch, value).await,
+            Operation::SendUserMessage => core_ops.send_user_message(value).await,
             Operation::GetAgentSettings => core_ops.get_agent_settings(value).await,
             Operation::ProbeAgent => core_ops.probe_agent(value).await,
-            _ => Err("operation is not implemented in Task 3".to_string()),
+            _ => Err("invalid UTF-8 command payload".to_string()),
         },
         CommandPayload::SelectSession(conversation_id) => {
-            let _ = conversation_id;
-            Err("operation is not implemented in Task 3".to_string())
+            core_ops
+                .select_session(selection_epoch, conversation_id)
+                .await
         }
         CommandPayload::AgentSettings { agent, json } => {
             if op != Operation::SetAgentSettings {
@@ -343,13 +566,33 @@ mod tests {
 
     use super::{
         execute_command, run_worker, terminalize_task, CommandMetadata, CoreFuture, CoreOps,
+        CoreResult,
     };
     use crate::commands::{CommandPayload, Operation, RuntimeCommand};
+    use crate::model::{ModelUpdate, OwnedCompletion, OwnedSessionSummary};
     use crate::{CompletionStatus, LifecycleState, SharedModel};
 
     struct ErrorOps;
 
     impl CoreOps for ErrorOps {
+        fn begin_selection(&self, _selection_epoch: u64, _op: Operation) {}
+
+        fn set_workspace(&self, _selection_epoch: u64, _path: Vec<u8>) -> CoreFuture {
+            Box::pin(async { Err("unexpected workspace".to_string()) })
+        }
+
+        fn create_session(&self, _selection_epoch: u64, _agent: Vec<u8>) -> CoreFuture {
+            Box::pin(async { Err("unexpected create".to_string()) })
+        }
+
+        fn select_session(&self, _selection_epoch: u64, _conversation_id: i32) -> CoreFuture {
+            Box::pin(async { Err("unexpected select".to_string()) })
+        }
+
+        fn send_user_message(&self, _text: Vec<u8>) -> CoreFuture {
+            Box::pin(async { Err("unexpected send".to_string()) })
+        }
+
         fn get_agent_settings(&self, _agent: Vec<u8>) -> CoreFuture {
             Box::pin(async { Err("unexpected get".to_string()) })
         }
@@ -365,28 +608,31 @@ mod tests {
 
     #[tokio::test]
     async fn worker_errors_are_terminal_results() {
-        assert_eq!(
-            execute_command(
-                Operation::SendUserMessage,
-                CommandPayload::Error("expected".to_string()),
-                Arc::new(ErrorOps),
-            )
-            .await,
-            Err("expected".to_string())
-        );
+        let error = execute_command(
+            0,
+            Operation::SendUserMessage,
+            CommandPayload::Error("expected".to_string()),
+            Arc::new(ErrorOps),
+        )
+        .await
+        .err();
+        assert_eq!(error.as_deref(), Some("expected"));
     }
 
     #[tokio::test]
     async fn worker_panics_are_visible_to_the_join_boundary() {
         let joined = tokio::spawn(execute_command(
+            0,
             Operation::SendUserMessage,
             CommandPayload::Panic,
             Arc::new(ErrorOps),
         ))
         .await;
-        assert!(joined
-            .expect_err("worker panic must be caught by join")
-            .is_panic());
+        let error = match joined {
+            Err(error) => error,
+            Ok(_) => panic!("worker panic must be caught by join"),
+        };
+        assert!(error.is_panic());
     }
 
     #[tokio::test]
@@ -407,6 +653,7 @@ mod tests {
                 .unwrap();
             let mut tasks = JoinSet::new();
             let abort = tasks.spawn(execute_command(
+                0,
                 Operation::SendUserMessage,
                 payload,
                 Arc::new(ErrorOps),
@@ -441,6 +688,24 @@ mod tests {
     }
 
     impl CoreOps for SlowProbeOps {
+        fn begin_selection(&self, _selection_epoch: u64, _op: Operation) {}
+
+        fn set_workspace(&self, _selection_epoch: u64, _path: Vec<u8>) -> CoreFuture {
+            Box::pin(async { Err("unexpected workspace".to_string()) })
+        }
+
+        fn create_session(&self, _selection_epoch: u64, _agent: Vec<u8>) -> CoreFuture {
+            Box::pin(async { Err("unexpected create".to_string()) })
+        }
+
+        fn select_session(&self, _selection_epoch: u64, _conversation_id: i32) -> CoreFuture {
+            Box::pin(async { Err("unexpected select".to_string()) })
+        }
+
+        fn send_user_message(&self, _text: Vec<u8>) -> CoreFuture {
+            Box::pin(async { Err("unexpected send".to_string()) })
+        }
+
         fn get_agent_settings(&self, _agent: Vec<u8>) -> CoreFuture {
             Box::pin(async { Err("unexpected get".to_string()) })
         }
@@ -453,7 +718,7 @@ mod tests {
             let gate = Arc::clone(&self.gate);
             Box::pin(async move {
                 gate.notified().await;
-                Ok(br#"{"launchable":true}"#.to_vec())
+                Ok(CoreResult::json(br#"{"launchable":true}"#.to_vec()))
             })
         }
     }
@@ -511,4 +776,139 @@ mod tests {
         shutdown_tx.send(true).unwrap();
         worker.await.unwrap();
     }
+
+    struct SlowCreateOps {
+        started: Arc<Notify>,
+        gate: Arc<Notify>,
+    }
+
+    impl CoreOps for SlowCreateOps {
+        fn begin_selection(&self, _selection_epoch: u64, _op: Operation) {}
+
+        fn set_workspace(&self, _selection_epoch: u64, _path: Vec<u8>) -> CoreFuture {
+            Box::pin(async { Err("unexpected workspace".to_string()) })
+        }
+
+        fn create_session(&self, _selection_epoch: u64, _agent: Vec<u8>) -> CoreFuture {
+            let started = Arc::clone(&self.started);
+            let gate = Arc::clone(&self.gate);
+            Box::pin(async move {
+                started.notify_one();
+                gate.notified().await;
+                Ok(CoreResult {
+                    payload: br#"{"conversationId":7,"connectionId":"old"}"#.to_vec(),
+                    update: Some(ModelUpdate::Selection {
+                        sessions: vec![OwnedSessionSummary {
+                            conversation_id: 7,
+                            title: b"Old".to_vec(),
+                            agent: b"codex".to_vec(),
+                            updated_at_ms: 1,
+                        }],
+                        connection_id: b"old".to_vec(),
+                        transcript_json: b"[]".to_vec(),
+                    }),
+                })
+            })
+        }
+
+        fn select_session(&self, _selection_epoch: u64, _conversation_id: i32) -> CoreFuture {
+            Box::pin(async { Err("unexpected select".to_string()) })
+        }
+
+        fn send_user_message(&self, _text: Vec<u8>) -> CoreFuture {
+            Box::pin(async { Err("unexpected send".to_string()) })
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
+    async fn selection_change_marks_slow_create_stale_once_without_applying_it() {
+        let started = Arc::new(Notify::new());
+        let gate = Arc::new(Notify::new());
+        let model = SharedModel::new();
+        let create_id = NonZeroU64::new(51).unwrap();
+        model
+            .reserve(create_id, Operation::CreateSession, 0)
+            .unwrap();
+        let create_epoch = model.selection_epoch();
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
+            Arc::new(SlowCreateOps {
+                started: Arc::clone(&started),
+                gate: Arc::clone(&gate),
+            }),
+        ));
+        command_tx
+            .send(RuntimeCommand {
+                request_id: create_id,
+                selection_epoch: create_epoch,
+                op: Operation::CreateSession,
+                payload: CommandPayload::Utf8(b"codex".to_vec()),
+            })
+            .await
+            .unwrap();
+        started.notified().await;
+
+        let newer_id = NonZeroU64::new(52).unwrap();
+        model
+            .reserve(newer_id, Operation::SelectSession, model.selection_epoch())
+            .unwrap();
+        let newer_epoch = model.selection_epoch();
+        gate.notify_one();
+
+        let mut create_completions = 0;
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
+                if completion.request_id == create_id.get() {
+                    create_completions += 1;
+                    assert_eq!(completion.status, CompletionStatus::Stale as u32);
+                }
+            }
+            if create_completions == 1 {
+                break;
+            }
+        }
+        assert_eq!(create_completions, 1);
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
diff --git a/src-tauri/codeg-eui-core/tests/session_contract.rs b/src-tauri/codeg-eui-core/tests/session_contract.rs
new file mode 100644
index 00000000..b18d64f3
--- /dev/null
+++ b/src-tauri/codeg-eui-core/tests/session_contract.rs
@@ -0,0 +1,168 @@
+use std::process::Command;
+use std::time::Duration;
+
+use codeg_eui_core::{
+    codeg_eui_begin_shutdown, codeg_eui_init, codeg_eui_poll, codeg_eui_select_session,
+    codeg_eui_set_workspace, codeg_eui_shutdown, CodegEuiCompletion, CodegEuiFrame, CodegEuiSlice,
+    CompletionStatus, Operation, CODEG_EUI_ERR_INVALID_STATE, CODEG_EUI_OK,
+};
+
+const CHILD_CASE: &str = "CODEG_EUI_SESSION_CONTRACT_CASE";
+const CHILD_ROOT: &str = "CODEG_EUI_SESSION_CONTRACT_ROOT";
+const CHILD_WORKSPACE: &str = "CODEG_EUI_SESSION_CONTRACT_WORKSPACE";
+
+#[test]
+fn workspace_selection_uses_the_canonical_directory_and_advances_the_epoch() {
+    run_isolated("workspace", || {
+        assert_eq!(init(), CODEG_EUI_OK);
+        let workspace =
+            std::fs::canonicalize(std::env::var(CHILD_WORKSPACE).expect("isolated workspace path"))
+                .expect("canonical workspace");
+        let path = workspace.to_string_lossy();
+        let mut request_id = 0;
+
+        assert_eq!(
+            codeg_eui_set_workspace(path.as_ptr(), path.len(), &mut request_id),
+            CODEG_EUI_OK
+        );
+
+        let frame = poll_until_completion(request_id);
+        let completion = completion_for(&frame, request_id);
+        assert_eq!(completion.op, Operation::SetWorkspace as u32);
+        assert_eq!(completion.status, CompletionStatus::Ok as u32);
+        let payload: serde_json::Value =
+            serde_json::from_slice(&copy_slice(completion.result_payload))
+                .expect("workspace completion JSON");
+        assert_eq!(payload["path"], path.as_ref());
+        assert!(payload["folderId"].as_i64().is_some_and(|id| id > 0));
+        assert_eq!(payload["sessions"], serde_json::json!([]));
+        assert_eq!(frame.selection_epoch, 1);
+        assert_eq!(frame.sessions_len, 0);
+        complete_shutdown();
+    });
+}
+
+#[test]
+fn non_directory_workspace_terminalizes_as_an_error() {
+    run_isolated("workspace_file", || {
+        assert_eq!(init(), CODEG_EUI_OK);
+        let workspace = std::env::var(CHILD_WORKSPACE).expect("isolated workspace path");
+        let file = std::path::Path::new(&workspace).join("not-a-directory.txt");
+        std::fs::write(&file, b"fixture").expect("workspace file fixture");
+        let path = file.to_string_lossy();
+        let mut request_id = 0;
+
+        assert_eq!(
+            codeg_eui_set_workspace(path.as_ptr(), path.len(), &mut request_id),
+            CODEG_EUI_OK
+        );
+
+        let frame = poll_until_completion(request_id);
+        let completion = completion_for(&frame, request_id);
+        assert_eq!(completion.status, CompletionStatus::Error as u32);
+        assert!(copy_slice(completion.result_payload).is_empty());
+        assert!(!copy_slice(completion.error).is_empty());
+        assert_eq!(frame.selection_epoch, 1);
+        assert_eq!(frame.sessions_len, 0);
+        complete_shutdown();
+    });
+}
+
+#[test]
+fn invalid_conversation_id_is_rejected_before_acceptance() {
+    run_isolated("invalid_conversation", || {
+        assert_eq!(init(), CODEG_EUI_OK);
+        let mut request_id = 91;
+        assert_eq!(
+            codeg_eui_select_session(0, &mut request_id),
+            CODEG_EUI_ERR_INVALID_STATE
+        );
+        assert_eq!(request_id, 91);
+        assert!(completions(&poll()).is_empty());
+        complete_shutdown();
+    });
+}
+
+fn run_isolated(case: &str, body: impl FnOnce()) {
+    if std::env::var(CHILD_CASE).as_deref() == Ok(case) {
+        body();
+        return;
+    }
+    if std::env::var_os(CHILD_CASE).is_some() {
+        return;
+    }
+
+    let root = tempfile::tempdir().expect("data root");
+    let workspace = tempfile::tempdir().expect("workspace root");
+    let status = Command::new(std::env::current_exe().expect("current test executable"))
+        .args(["--exact", std::thread::current().name().expect("test name")])
+        .env(CHILD_CASE, case)
+        .env(CHILD_ROOT, root.path())
+        .env(CHILD_WORKSPACE, workspace.path())
+        .status()
+        .expect("run isolated session contract");
+    assert!(status.success(), "isolated session case {case} failed");
+}
+
+fn init() -> i32 {
+    let root = std::env::var(CHILD_ROOT).expect("isolated root");
+    codeg_eui_init(root.as_ptr(), root.len())
+}
+
+fn poll() -> CodegEuiFrame {
+    let mut frame = CodegEuiFrame::default();
+    assert_eq!(codeg_eui_poll(&mut frame), CODEG_EUI_OK);
+    frame
+}
+
+fn poll_until_completion(request_id: u64) -> CodegEuiFrame {
+    for _ in 0..400 {
+        let frame = poll();
+        if completions(&frame)
+            .iter()
+            .any(|completion| completion.request_id == request_id)
+        {
+            return frame;
+        }
+        std::thread::sleep(Duration::from_millis(5));
+    }
+    panic!("request {request_id} did not complete");
+}
+
+fn completion_for(frame: &CodegEuiFrame, request_id: u64) -> CodegEuiCompletion {
+    completions(frame)
+        .iter()
+        .find(|completion| completion.request_id == request_id)
+        .copied()
+        .expect("completion for request")
+}
+
+fn completions(frame: &CodegEuiFrame) -> &[CodegEuiCompletion] {
+    if frame.completions_len == 0 {
+        assert!(frame.completions.is_null());
+        return &[];
+    }
+    assert!(!frame.completions.is_null());
+    unsafe { std::slice::from_raw_parts(frame.completions, frame.completions_len) }
+}
+
+fn copy_slice(slice: CodegEuiSlice) -> Vec<u8> {
+    if slice.len == 0 {
+        assert!(slice.ptr.is_null());
+        return Vec::new();
+    }
+    assert!(!slice.ptr.is_null());
+    unsafe { std::slice::from_raw_parts(slice.ptr, slice.len) }.to_vec()
+}
+
+fn complete_shutdown() {
+    assert_eq!(codeg_eui_begin_shutdown(), CODEG_EUI_OK);
+    for _ in 0..400 {
+        if poll().shutdown_ready == 1 {
+            assert_eq!(codeg_eui_shutdown(), CODEG_EUI_OK);
+            return;
+        }
+        std::thread::sleep(Duration::from_millis(5));
+    }
+    panic!("shutdown did not become ready");
+}
diff --git a/src-tauri/src/commands/eui_facade.rs b/src-tauri/src/commands/eui_facade.rs
index c0f67f40..f4968d2c 100644
--- a/src-tauri/src/commands/eui_facade.rs
+++ b/src-tauri/src/commands/eui_facade.rs
@@ -1,19 +1,202 @@
 use std::collections::BTreeMap;
+use std::path::PathBuf;
 
 use serde::{Deserialize, Serialize};
 use thiserror::Error;
 
 use crate::acp::preflight::CheckStatus;
+use crate::acp::terminal_context::{build_acp_launch_inputs, AcpRouteRequest};
 use crate::acp::types::{
     AcpAgentInfo, CodexSandboxSettings, CodexSandboxStructuredConfig, GrokSettings,
-    GrokStructuredConfig,
+    GrokStructuredConfig, PromptInputBlock,
 };
 use crate::app_state::AppState;
 use crate::commands::acp::{
     acp_list_agents_core, acp_preflight_core, acp_update_agent_config_and_refresh,
-    acp_update_agent_env_and_refresh,
+    acp_update_agent_env_and_refresh, verify_agent_installed,
 };
-use crate::models::agent::AgentType;
+use crate::commands::conversations::{
+    create_project_conversation_core, get_folder_conversation_with_live_core,
+};
+use crate::commands::folders::open_folder_core;
+use crate::commands::history_window::HistoryLoadOpts;
+use crate::db::entities::conversation::ConversationKind;
+use crate::db::service::conversation_service;
+use crate::models::{AgentType, DbConversationSummary, MessageTurn};
+
+#[derive(Debug, Clone, PartialEq, Serialize)]
+#[serde(rename_all = "camelCase")]
+pub struct EuiWorkspace {
+    pub folder_id: i32,
+    pub path: PathBuf,
+    pub sessions: Vec<EuiSessionSummary>,
+}
+
+#[derive(Debug, Clone, PartialEq, Serialize)]
+#[serde(rename_all = "camelCase")]
+pub struct EuiSessionSummary {
+    pub conversation_id: i32,
+    pub title: Option<String>,
+    pub agent_type: AgentType,
+    pub status: String,
+    pub external_session_id: Option<String>,
+    pub updated_at_ms: i64,
+}
+
+#[derive(Debug, Clone, Serialize)]
+#[serde(rename_all = "camelCase")]
+pub struct EuiSessionSelection {
+    pub folder_id: i32,
+    pub path: PathBuf,
+    pub conversation_id: i32,
+    pub title: Option<String>,
+    pub agent_type: AgentType,
+    pub status: String,
+    pub external_session_id: Option<String>,
+    pub updated_at_ms: i64,
+    pub connection_id: String,
+    pub transcript: Vec<MessageTurn>,
+}
+
+#[derive(Debug)]
+pub(crate) struct LoadedEuiSession {
+    pub summary: EuiSessionSummary,
+    pub transcript: Vec<MessageTurn>,
+}
+
+#[async_trait::async_trait]
+pub(crate) trait EuiSessionOps: Send + Sync {
+    type LaunchInputs: Send;
+
+    async fn verify_installed(&self, agent_type: AgentType) -> Result<(), EuiFacadeError>;
+
+    async fn build_launch_inputs(
+        &self,
+        state: &AppState,
+        agent_type: AgentType,
+        external_session_id: Option<&str>,
+        conversation_id: i32,
+    ) -> Result<Self::LaunchInputs, EuiFacadeError>;
+
+    #[allow(clippy::too_many_arguments)]
+    async fn spawn_agent(
+        &self,
+        state: &AppState,
+        agent_type: AgentType,
+        workspace_path: &std::path::Path,
+        external_session_id: Option<String>,
+        conversation_id: i32,
+        launch_inputs: Self::LaunchInputs,
+        owner: &str,
+    ) -> Result<String, EuiFacadeError>;
+
+    async fn find_connection(&self, state: &AppState, conversation_id: i32) -> Option<String>;
+
+    #[allow(clippy::too_many_arguments)]
+    async fn send_linked(
+        &self,
+        state: &AppState,
+        connection_id: &str,
+        blocks: Vec<PromptInputBlock>,
+        folder_id: i32,
+        conversation_id: i32,
+        client_message_id: String,
+    ) -> Result<(), EuiFacadeError>;
+}
+
+struct ProductionEuiSessionOps;
+
+#[async_trait::async_trait]
+impl EuiSessionOps for ProductionEuiSessionOps {
+    type LaunchInputs = crate::acp::terminal_context::AcpLaunchInputs;
+
+    async fn verify_installed(&self, agent_type: AgentType) -> Result<(), EuiFacadeError> {
+        verify_agent_installed(agent_type)
+            .await
+            .map_err(EuiFacadeError::from)
+    }
+
+    async fn build_launch_inputs(
+        &self,
+        state: &AppState,
+        agent_type: AgentType,
+        external_session_id: Option<&str>,
+        conversation_id: i32,
+    ) -> Result<Self::LaunchInputs, EuiFacadeError> {
+        build_acp_launch_inputs(
+            &state.db,
+            agent_type,
+            external_session_id,
+            &state.data_dir,
+            AcpRouteRequest::root(Some(conversation_id), None),
+            &state.delegation_runtime_settings.snapshot(),
+        )
+        .await
+        .map_err(EuiFacadeError::from)
+    }
+
+    async fn spawn_agent(
+        &self,
+        state: &AppState,
+        agent_type: AgentType,
+        workspace_path: &std::path::Path,
+        external_session_id: Option<String>,
+        _conversation_id: i32,
+        launch_inputs: Self::LaunchInputs,
+        owner: &str,
+    ) -> Result<String, EuiFacadeError> {
+        let launch_context = crate::auto_title::user_launch_context_from_db(&state.db.conn).await;
+        state
+            .connection_manager
+            .spawn_agent(
+                agent_type,
+                Some(workspace_path.to_string_lossy().into_owned()),
+                external_session_id,
+                launch_inputs,
+                owner.to_string(),
+                state.emitter.clone(),
+                None,
+                BTreeMap::new(),
+                launch_context,
+                None,
+                None,
+            )
+            .await
+            .map_err(EuiFacadeError::from)
+    }
+
+    async fn find_connection(&self, state: &AppState, conversation_id: i32) -> Option<String> {
+        state
+            .connection_manager
+            .find_connection_by_conversation_id(conversation_id)
+            .await
+    }
+
+    async fn send_linked(
+        &self,
+        state: &AppState,
+        connection_id: &str,
+        blocks: Vec<PromptInputBlock>,
+        folder_id: i32,
+        conversation_id: i32,
+        client_message_id: String,
+    ) -> Result<(), EuiFacadeError> {
+        state
+            .connection_manager
+            .send_prompt_linked_with_message_id(
+                &state.db,
+                connection_id,
+                blocks,
+                Some(folder_id),
+                Some(conversation_id),
+                None,
+                Some(client_message_id),
+                None,
+            )
+            .await?;
+        Ok(())
+    }
+}
 
 /// The native EUI settings contract intentionally contains only fields owned
 /// by the existing Grok/Codex ACP settings paths.
@@ -71,10 +254,250 @@ pub enum EuiFacadeError {
     AgentNotFound(AgentType),
     #[error("invalid agent settings patch: {0}")]
     InvalidPatch(String),
+    #[error("invalid EUI workspace {path}: {reason}")]
+    InvalidWorkspace { path: String, reason: String },
+    #[error("conversation {conversation_id} does not belong to workspace folder {folder_id}")]
+    ConversationOutsideWorkspace {
+        conversation_id: i32,
+        folder_id: i32,
+    },
+    #[error("EUI application operation failed: {0}")]
+    App(#[from] crate::app_error::AppCommandError),
+    #[error("EUI database operation failed: {0}")]
+    Database(#[from] crate::db::error::DbError),
     #[error("ACP settings operation failed: {0}")]
     Acp(#[from] crate::acp::error::AcpError),
 }
 
+pub async fn set_eui_workspace(
+    state: &AppState,
+    requested_path: PathBuf,
+) -> Result<EuiWorkspace, EuiFacadeError> {
+    let path = std::fs::canonicalize(&requested_path).map_err(|error| {
+        EuiFacadeError::InvalidWorkspace {
+            path: requested_path.display().to_string(),
+            reason: error.to_string(),
+        }
+    })?;
+    if !path.is_dir() {
+        return Err(EuiFacadeError::InvalidWorkspace {
+            path: path.display().to_string(),
+            reason: "path is not a directory".to_string(),
+        });
+    }
+    let wire_path = path
+        .to_str()
+        .ok_or_else(|| EuiFacadeError::InvalidWorkspace {
+            path: path.display().to_string(),
+            reason: "canonical path is not valid UTF-8".to_string(),
+        })?
+        .to_string();
+    let folder = open_folder_core(&state.db, wire_path).await?;
+    let sessions =
+        conversation_service::list_by_folder(&state.db.conn, folder.id, None, None, None, None)
+            .await?
+            .into_iter()
+            .filter(|row| row.kind == ConversationKind::Regular)
+            .map(project_session_summary)
+            .collect();
+    Ok(EuiWorkspace {
+        folder_id: folder.id,
+        path,
+        sessions,
+    })
+}
+
+pub async fn create_eui_conversation(
+    state: &AppState,
+    folder_id: i32,
+    agent_type: AgentType,
+) -> Result<EuiSessionSummary, EuiFacadeError> {
+    ensure_supported(agent_type)?;
+    let created =
+        create_project_conversation_core(&state.db.conn, folder_id, agent_type, None, None).await?;
+    let row = conversation_service::get_by_id(&state.db.conn, created.conversation_id).await?;
+    Ok(project_session_summary(row))
+}
+
+pub async fn create_eui_session(
+    state: &AppState,
+    workspace: &EuiWorkspace,
+    agent_type: AgentType,
+) -> Result<EuiSessionSelection, EuiFacadeError> {
+    create_eui_session_with_ops(state, workspace, agent_type, &ProductionEuiSessionOps).await
+}
+
+pub(crate) async fn create_eui_session_with_ops<O: EuiSessionOps>(
+    state: &AppState,
+    workspace: &EuiWorkspace,
+    agent_type: AgentType,
+    ops: &O,
+) -> Result<EuiSessionSelection, EuiFacadeError> {
+    ensure_supported(agent_type)?;
+    ops.verify_installed(agent_type).await?;
+    let summary = create_eui_conversation(state, workspace.folder_id, agent_type).await?;
+    let launch_inputs = ops
+        .build_launch_inputs(
+            state,
+            summary.agent_type,
+            summary.external_session_id.as_deref(),
+            summary.conversation_id,
+        )
+        .await?;
+    let connection_id = ops
+        .spawn_agent(
+            state,
+            summary.agent_type,
+            &workspace.path,
+            summary.external_session_id.clone(),
+            summary.conversation_id,
+            launch_inputs,
+            "eui",
+        )
+        .await?;
+    Ok(selection_from_parts(
+        workspace,
+        summary,
+        connection_id,
+        Vec::new(),
+    ))
+}
+
+pub async fn select_eui_session(
+    state: &AppState,
+    workspace: &EuiWorkspace,
+    conversation_id: i32,
+) -> Result<EuiSessionSelection, EuiFacadeError> {
+    select_eui_session_with_ops(state, workspace, conversation_id, &ProductionEuiSessionOps).await
+}
+
+pub(crate) async fn select_eui_session_with_ops<O: EuiSessionOps>(
+    state: &AppState,
+    workspace: &EuiWorkspace,
+    conversation_id: i32,
+    ops: &O,
+) -> Result<EuiSessionSelection, EuiFacadeError> {
+    let loaded = load_eui_session(state, workspace, conversation_id).await?;
+    let connection_id = match ops.find_connection(state, conversation_id).await {
+        Some(connection_id) => connection_id,
+        None => {
+            ensure_supported(loaded.summary.agent_type)?;
+            ops.verify_installed(loaded.summary.agent_type).await?;
+            let launch_inputs = ops
+                .build_launch_inputs(
+                    state,
+                    loaded.summary.agent_type,
+                    loaded.summary.external_session_id.as_deref(),
+                    loaded.summary.conversation_id,
+                )
+                .await?;
+            ops.spawn_agent(
+                state,
+                loaded.summary.agent_type,
+                &workspace.path,
+                loaded.summary.external_session_id.clone(),
+                loaded.summary.conversation_id,
+                launch_inputs,
+                "eui",
+            )
+            .await?
+        }
+    };
+    Ok(selection_from_parts(
+        workspace,
+        loaded.summary,
+        connection_id,
+        loaded.transcript,
+    ))
+}
+
+pub async fn send_eui_message(
+    state: &AppState,
+    selection: &EuiSessionSelection,
+    text: String,
+) -> Result<(), EuiFacadeError> {
+    send_eui_message_with_ops(state, selection, text, &ProductionEuiSessionOps).await
+}
+
+pub(crate) async fn send_eui_message_with_ops<O: EuiSessionOps>(
+    state: &AppState,
+    selection: &EuiSessionSelection,
+    text: String,
+    ops: &O,
+) -> Result<(), EuiFacadeError> {
+    let blocks = vec![PromptInputBlock::Text { text }];
+    ops.send_linked(
+        state,
+        &selection.connection_id,
+        blocks,
+        selection.folder_id,
+        selection.conversation_id,
+        uuid::Uuid::new_v4().to_string(),
+    )
+    .await
+}
+
+pub(crate) async fn load_eui_session(
+    state: &AppState,
+    workspace: &EuiWorkspace,
+    conversation_id: i32,
+) -> Result<LoadedEuiSession, EuiFacadeError> {
+    let detail = get_folder_conversation_with_live_core(
+        &state.db.conn,
+        &state.connection_manager,
+        &state.chat_channel_manager,
+        &state.emitter,
+        state.internal_sessions.as_ref(),
+        conversation_id,
+        HistoryLoadOpts {
+            user_turn_limit: Some(100),
+            before_turn_id: None,
+        },
+    )
+    .await?;
+    if detail.summary.folder_id != workspace.folder_id {
+        return Err(EuiFacadeError::ConversationOutsideWorkspace {
+            conversation_id,
+            folder_id: workspace.folder_id,
+        });
+    }
+    Ok(LoadedEuiSession {
+        summary: project_session_summary(detail.summary),
+        transcript: detail.turns,
+    })
+}
+
+fn selection_from_parts(
+    workspace: &EuiWorkspace,
+    summary: EuiSessionSummary,
+    connection_id: String,
+    transcript: Vec<MessageTurn>,
+) -> EuiSessionSelection {
+    EuiSessionSelection {
+        folder_id: workspace.folder_id,
+        path: workspace.path.clone(),
+        conversation_id: summary.conversation_id,
+        title: summary.title,
+        agent_type: summary.agent_type,
+        status: summary.status,
+        external_session_id: summary.external_session_id,
+        updated_at_ms: summary.updated_at_ms,
+        connection_id,
+        transcript,
+    }
+}
+
+fn project_session_summary(row: DbConversationSummary) -> EuiSessionSummary {
+    EuiSessionSummary {
+        conversation_id: row.id,
+        title: row.title,
+        agent_type: row.agent_type,
+        status: row.status,
+        external_session_id: row.external_id,
+        updated_at_ms: row.updated_at.timestamp_millis(),
+    }
+}
+
 impl EuiAgentSettingsPatch {
     pub(crate) fn validate_for(&self, agent: AgentType) -> Result<(), EuiFacadeError> {
         match agent {
@@ -275,14 +698,327 @@ fn ensure_supported(agent: AgentType) -> Result<(), EuiFacadeError> {
 #[cfg(test)]
 mod tests {
     use std::collections::BTreeMap;
+    use std::sync::{Arc, Mutex};
 
     use super::{
-        ensure_supported, parse_supported_agent, project_agent_settings, EuiAgentSettingsPatch,
-        EuiFacadeError,
+        create_eui_conversation, create_eui_session_with_ops, ensure_supported, load_eui_session,
+        parse_supported_agent, project_agent_settings, send_eui_message, send_eui_message_with_ops,
+        set_eui_workspace, EuiAgentSettingsPatch, EuiFacadeError, EuiSessionOps,
+        EuiSessionSelection,
     };
+    use crate::acp::connection::ConnectionCommand;
     use crate::acp::types::AcpAgentInfo;
+    use crate::app_state::AppState;
+    use crate::db::service::{conversation_service, folder_service};
+    use crate::db::test_helpers::fresh_disk_db;
     use crate::models::agent::AgentType;
 
+    async fn eui_test_state(root: &std::path::Path) -> AppState {
+        let db = fresh_disk_db(root).await;
+        AppState::new_for_test(db, root.to_path_buf())
+    }
+
+    #[derive(Clone, Default)]
+    struct RecordingSessionOps {
+        calls: Arc<Mutex<Vec<&'static str>>>,
+        last_send: Arc<Mutex<Option<(String, i32, i32, String, String)>>>,
+    }
+
+    impl RecordingSessionOps {
+        fn calls(&self) -> Vec<&'static str> {
+            self.calls.lock().unwrap().clone()
+        }
+
+        fn record(&self, call: &'static str) {
+            self.calls.lock().unwrap().push(call);
+        }
+    }
+
+    #[async_trait::async_trait]
+    impl EuiSessionOps for RecordingSessionOps {
+        type LaunchInputs = (AgentType, Option<String>, i32);
+
+        async fn verify_installed(&self, agent_type: AgentType) -> Result<(), EuiFacadeError> {
+            assert!(matches!(agent_type, AgentType::Codex | AgentType::Grok));
+            self.record("verify_installed");
+            Ok(())
+        }
+
+        async fn build_launch_inputs(
+            &self,
+            _state: &AppState,
+            agent_type: AgentType,
+            external_session_id: Option<&str>,
+            conversation_id: i32,
+        ) -> Result<Self::LaunchInputs, EuiFacadeError> {
+            assert!(conversation_id > 0);
+            self.record("build_launch_inputs");
+            Ok((
+                agent_type,
+                external_session_id.map(str::to_string),
+                conversation_id,
+            ))
+        }
+
+        async fn spawn_agent(
+            &self,
+            _state: &AppState,
+            agent_type: AgentType,
+            workspace_path: &std::path::Path,
+            external_session_id: Option<String>,
+            conversation_id: i32,
+            launch_inputs: Self::LaunchInputs,
+            owner: &str,
+        ) -> Result<String, EuiFacadeError> {
+            assert!(workspace_path.is_absolute());
+            assert_eq!(owner, "eui");
+            assert_eq!(
+                launch_inputs,
+                (agent_type, external_session_id, conversation_id)
+            );
+            self.record("spawn_agent");
+            Ok("recorded-connection".to_string())
+        }
+
+        async fn find_connection(
+            &self,
+            _state: &AppState,
+            _conversation_id: i32,
+        ) -> Option<String> {
+            self.record("find_connection");
+            None
+        }
+
+        async fn send_linked(
+            &self,
+            _state: &AppState,
+            connection_id: &str,
+            blocks: Vec<crate::acp::types::PromptInputBlock>,
+            folder_id: i32,
+            conversation_id: i32,
+            client_message_id: String,
+        ) -> Result<(), EuiFacadeError> {
+            let [crate::acp::types::PromptInputBlock::Text { text }] = blocks.as_slice() else {
+                panic!("EUI send must contain exactly one text block");
+            };
+            self.record("send_prompt_linked");
+            *self.last_send.lock().unwrap() = Some((
+                connection_id.to_string(),
+                folder_id,
+                conversation_id,
+                client_message_id,
+                text.clone(),
+            ));
+            Ok(())
+        }
+    }
+
+    #[tokio::test]
+    async fn create_session_verifies_builds_then_spawns_with_eui_ownership() {
+        let root = tempfile::tempdir().unwrap();
+        let workspace_dir = root.path().join("workspace");
+        std::fs::create_dir(&workspace_dir).unwrap();
+        let state = eui_test_state(root.path()).await;
+        let workspace = set_eui_workspace(&state, workspace_dir).await.unwrap();
+        let ops = RecordingSessionOps::default();
+
+        let selection = create_eui_session_with_ops(&state, &workspace, AgentType::Codex, &ops)
+            .await
+            .unwrap();
+
+        assert_eq!(
+            ops.calls(),
+            ["verify_installed", "build_launch_inputs", "spawn_agent"]
+        );
+        assert!(selection.conversation_id > 0);
+        assert_eq!(selection.connection_id, "recorded-connection");
+
+        ops.calls.lock().unwrap().clear();
+        send_eui_message_with_ops(&state, &selection, "hello".to_string(), &ops)
+            .await
+            .unwrap();
+        assert_eq!(ops.calls(), ["send_prompt_linked"]);
+        let send = ops.last_send.lock().unwrap().clone().unwrap();
+        assert_eq!(send.0, selection.connection_id);
+        assert_eq!(send.1, selection.folder_id);
+        assert_eq!(send.2, selection.conversation_id);
+        assert!(uuid::Uuid::parse_str(&send.3).is_ok());
+        assert_eq!(send.4, "hello");
+    }
+
+    #[tokio::test]
+    async fn workspace_and_conversation_reuse_existing_database_cores() {
+        let root = tempfile::tempdir().unwrap();
+        let workspace_dir = root.path().join("workspace");
+        std::fs::create_dir(&workspace_dir).unwrap();
+        let state = eui_test_state(root.path()).await;
+
+        let workspace = set_eui_workspace(&state, workspace_dir.clone())
+            .await
+            .unwrap();
+        assert_eq!(
+            workspace.path,
+            std::fs::canonicalize(&workspace_dir).unwrap()
+        );
+        let row = create_eui_conversation(&state, workspace.folder_id, AgentType::Grok)
+            .await
+            .unwrap();
+        assert!(row.conversation_id > 0);
+        assert_eq!(row.agent_type, AgentType::Grok);
+
+        let rows = conversation_service::list_by_folder(
+            &state.db.conn,
+            workspace.folder_id,
+            None,
+            None,
+            None,
+            None,
+        )
+        .await
+        .unwrap();
+        assert_eq!(rows.len(), 1);
+    }
+
+    #[tokio::test]
+    async fn invalid_workspace_does_not_create_a_folder_row() {
+        let root = tempfile::tempdir().unwrap();
+        let state = eui_test_state(root.path()).await;
+        let file = root.path().join("file.txt");
+        std::fs::write(&file, b"not a directory").unwrap();
+
+        assert!(matches!(
+            set_eui_workspace(&state, file).await,
+            Err(EuiFacadeError::InvalidWorkspace { .. })
+        ));
+        assert!(matches!(
+            set_eui_workspace(&state, root.path().join("missing")).await,
+            Err(EuiFacadeError::InvalidWorkspace { .. })
+        ));
+        assert!(folder_service::list_folders(&state.db.conn)
+            .await
+            .unwrap()
+            .is_empty());
+    }
+
+    #[tokio::test]
+    async fn only_codex_and_grok_conversations_are_created() {
+        let root = tempfile::tempdir().unwrap();
+        let workspace_dir = root.path().join("workspace");
+        std::fs::create_dir(&workspace_dir).unwrap();
+        let state = eui_test_state(root.path()).await;
+        let workspace = set_eui_workspace(&state, workspace_dir).await.unwrap();
+
+        for agent in [AgentType::Codex, AgentType::Grok] {
+            assert_eq!(
+                create_eui_conversation(&state, workspace.folder_id, agent)
+                    .await
+                    .unwrap()
+                    .agent_type,
+                agent
+            );
+        }
+        assert!(matches!(
+            create_eui_conversation(&state, workspace.folder_id, AgentType::ClaudeCode).await,
+            Err(EuiFacadeError::UnsupportedAgent(_))
+        ));
+        let rows = conversation_service::list_by_folder(
+            &state.db.conn,
+            workspace.folder_id,
+            None,
+            None,
+            None,
+            None,
+        )
+        .await
+        .unwrap();
+        assert_eq!(rows.len(), 2);
+    }
+
+    #[tokio::test]
+    async fn history_projection_is_backend_message_turn_json() {
+        let root = tempfile::tempdir().unwrap();
+        let workspace_dir = root.path().join("workspace");
+        std::fs::create_dir(&workspace_dir).unwrap();
+        let state = eui_test_state(root.path()).await;
+        let workspace = set_eui_workspace(&state, workspace_dir).await.unwrap();
+        let row = create_eui_conversation(&state, workspace.folder_id, AgentType::Codex)
+            .await
+            .unwrap();
+
+        let loaded = load_eui_session(&state, &workspace, row.conversation_id)
+            .await
+            .unwrap();
+        assert_eq!(loaded.summary, row);
+        assert_eq!(
+            serde_json::to_value(&loaded.transcript).unwrap(),
+            serde_json::json!([])
+        );
+    }
+
+    #[tokio::test]
+    async fn send_uses_one_text_block_and_binds_the_selected_ids() {
+        let root = tempfile::tempdir().unwrap();
+        let workspace_dir = root.path().join("workspace");
+        std::fs::create_dir(&workspace_dir).unwrap();
+        let state = eui_test_state(root.path()).await;
+        let workspace = set_eui_workspace(&state, workspace_dir).await.unwrap();
+        let row = create_eui_conversation(&state, workspace.folder_id, AgentType::Codex)
+            .await
+            .unwrap();
+        let mut commands = state
+            .connection_manager
+            .insert_test_connection_live(
+                "eui-test-connection",
+                AgentType::Codex,
+                Some(workspace.path.clone()),
+                state.emitter.clone(),
+            )
+            .await;
+        let selection = EuiSessionSelection {
+            folder_id: workspace.folder_id,
+            path: workspace.path,
+            conversation_id: row.conversation_id,
+            title: row.title,
+            agent_type: row.agent_type,
+            status: row.status,
+            external_session_id: row.external_session_id,
+            updated_at_ms: row.updated_at_ms,
+            connection_id: "eui-test-connection".to_string(),
+            transcript: Vec::new(),
+        };
+
+        send_eui_message(&state, &selection, "hello".to_string())
+            .await
+            .unwrap();
+
+        let command = commands.recv().await.expect("one prompt command");
+        let ConnectionCommand::Prompt {
+            blocks,
+            user_message,
+            ..
+        } = command
+        else {
+            panic!("expected prompt command");
+        };
+        assert!(matches!(
+            blocks.as_slice(),
+            [crate::acp::types::PromptInputBlock::Text { text }] if text == "hello"
+        ));
+        let message_id = user_message.expect("linked user message").0;
+        assert!(uuid::Uuid::parse_str(&message_id).is_ok());
+        assert_eq!(
+            state
+                .connection_manager
+                .get_state("eui-test-connection")
+                .await
+                .unwrap()
+                .read()
+                .await
+                .conversation_id,
+            Some(selection.conversation_id)
+        );
+    }
+
     #[test]
     fn only_codex_and_grok_wire_values_are_supported() {
         assert_eq!(parse_supported_agent("codex").unwrap(), AgentType::Codex);
