# Task 4 Review Package
BASE: 66f7cff1ee5b02773f19f938482c3a112792ecb0 HEAD: 89c0889f6faf8d3ad482c9e4e1a6a34df65d8cbf
Parent: SKIP all full cargo test
89c0889f feat(eui): expose Grok and Codex settings facade
 .../task-4-report.md                               | 139 ++++++++
 codeg-eui/app/bridge/codeg_eui_bridge.h            |   2 +
 src-tauri/codeg-eui-core/src/abi.rs                |   5 +
 src-tauri/codeg-eui-core/src/bootstrap.rs          |   9 +-
 src-tauri/codeg-eui-core/src/runtime.rs            | 208 +++++++++++-
 .../codeg-eui-core/tests/settings_contract.rs      | 252 +++++++++++++++
 src-tauri/src/commands/eui_facade.rs               | 359 +++++++++++++++++++++
 src-tauri/src/commands/mod.rs                      |   1 +
 8 files changed, 958 insertions(+), 17 deletions(-)
diff --git a/.superpowers/sdd/2026-08-09-eui-neo-frontend-spike/task-4-report.md b/.superpowers/sdd/2026-08-09-eui-neo-frontend-spike/task-4-report.md
new file mode 100644
index 00000000..0d38e407
--- /dev/null
+++ b/.superpowers/sdd/2026-08-09-eui-neo-frontend-spike/task-4-report.md
@@ -0,0 +1,139 @@
+# Task 4 Implementer Report
+
+## Status
+
+DONE_WITH_CONCERNS
+
+Task 4 adds the narrow Grok/Codex settings facade and routes the existing EUI
+settings/probe ABI operations through asynchronous Tokio workers. Settings
+writes remain owned by the existing ACP persistence helpers, and accepted
+requests use the Task 3 completion ledger for one terminal completion.
+
+## Implementation
+
+- Added public backend-aligned `EuiAgentSettings`, request-only
+  `EuiAgentSettingsPatch`, `EuiAgentProbe`, and `EuiFacadeError` types.
+- Restricted the wire vocabulary to exact `"codex"` and `"grok"` values.
+  Typed and wire-level unsupported agents are rejected before DB or native
+  config access.
+- Projected settings from `AcpAgentInfo`, omitting fields owned by the other
+  supported agent.
+- Validated cross-agent patch fields before reading current settings.
+- Delegated environment/provider writes only to
+  `acp_update_agent_env_and_refresh` and native config writes only to
+  `acp_update_agent_config_and_refresh`; the facade performs no direct file
+  writes and adds no persistence schema.
+- Delegated probes to `acp_preflight_core(agent, Some(true), db)` and returned
+  launchability, installed version, and non-passing diagnostic messages.
+- Shared `AppState` with workers through `Arc<AppState>` and added an injectable
+  `CoreOps` boundary. Get, set, and probe work runs in spawned Tokio tasks and
+  serializes result DTOs into completion JSON.
+- Added pre-acceptance bounded JSON parsing with outer
+  `deny_unknown_fields` for `codeg_eui_set_agent_settings`.
+- Added a deterministic slow-probe worker test and an isolated child-process
+  settings contract covering malformed patch rejection, polled completion,
+  Codex/Grok DTO projection, and native `CODEX_HOME`/`GROK_HOME` files.
+- Documented the asynchronous completion contract beside the public C
+  declarations.
+
+`commands.rs` and `model.rs` required no Task 4 edit: Task 3 already defined
+the frozen operation discriminants, settings payload shape, completion
+ledger, and JSON result storage used by this implementation.
+
+## TDD Evidence
+
+### RED
+
+Two reversible mutations were run against the final focused probes:
+
+- Removing the typed-agent pre-access guard made
+  `unsupported_typed_agent_is_rejected_by_the_pre_access_guard` fail with the
+  expected assertion (`0 passed, 1 failed`).
+- Replacing the probe worker route with an error made
+  `slow_probe_does_not_block_frame_build_and_completes_once` fail on the
+  expected completion status (`0 passed, 1 failed`). The same mutation was
+  also rejected by the `-D warnings` compile because `probe_agent` became dead
+  code.
+
+The malformed-patch contract also fixes the acceptance boundary: unknown
+outer fields return `CODEG_EUI_ERR_INVALID_STATE` and leave the caller's
+request ID unchanged.
+
+### GREEN
+
+After restoring the production paths:
+
+- Actual `eui_facade.rs` compiled with `rustc -D warnings` against a
+  shape-compatible ACP/AppState boundary; **4/4 facade unit tests passed**.
+- Actual Task 4 `abi.rs` and `runtime.rs` compiled with `rustc -D warnings`
+  against a shape-compatible facade/AppState boundary; **7/7 focused
+  ABI/model/runtime tests passed**, including the slow-probe completion test.
+- Contracts-only CMake/CTest: **3/3 passed** (harness, ABI layout, UI
+  snapshot).
+
+The shape-compatible probes validate the actual changed modules but do not
+replace compiling or running them against the full shared `codeg` crate.
+
+## Verification
+
+Passed:
+
+- `cargo fmt --manifest-path src-tauri/Cargo.toml -- --check`
+- `cargo fmt --manifest-path src-tauri/codeg-eui-core/Cargo.toml -- --check`
+- `cargo metadata --manifest-path src-tauri/codeg-eui-core/Cargo.toml --no-deps`
+- Direct actual-source Rust probes with `-D warnings`: **11/11 passed**.
+- Contracts-only CMake build and CTest: **3/3 passed**.
+- Task 4 raw/nested JSON fixture parsing probe.
+- `git diff --check`.
+- Approved design SHA-256 matched
+  `b3446ec31cc8b0457ed1ca3e7c6e8b3ec421eb4b997af6efae3d8975ecd2bdef`.
+- No standalone `src-tauri/codeg-eui-core/Cargo.lock` remains.
+
+Per the parent instruction, **all full Cargo tests were skipped**. No full
+package/workspace test, `cargo test --lib --features test-utils`, or broad
+shared-codeg suite was run.
+
+A focused dependency-complete standalone-crate check was attempted with the
+repository's one-job/no-debug low-memory configuration. It reached the shared
+`codeg` crate with no emitted Rust diagnostic, then the kernel killed `rustc`
+with `SIGKILL` on the 3.8 GiB/no-swap host. The focused
+`settings_contract` Cargo binary therefore could not be dependency-completely
+compiled or run on this host. The generated standalone lockfile was removed.
+
+## Files Changed
+
+- `src-tauri/src/commands/eui_facade.rs`
+- `src-tauri/src/commands/mod.rs`
+- `src-tauri/codeg-eui-core/src/abi.rs`
+- `src-tauri/codeg-eui-core/src/bootstrap.rs`
+- `src-tauri/codeg-eui-core/src/runtime.rs`
+- `src-tauri/codeg-eui-core/tests/settings_contract.rs`
+- `codeg-eui/app/bridge/codeg_eui_bridge.h`
+
+## Self-Review
+
+- Unsupported wire agents are parsed before an `AppState` reference is used;
+  typed facade callers pass the same guard before DB/config access.
+- Cross-agent fields are rejected before the facade reads current settings.
+- The facade contains no filesystem write calls and widens none of the
+  existing ACP helper visibilities.
+- Settings patch deserialization is request-only and rejects unknown outer
+  fields before request acceptance; output DTOs are serialization-only.
+- Get/set/probe work is spawned off the UI thread. Worker success, error, and
+  panic paths all terminalize through the existing exactly-once ledger.
+- Completion payloads are JSON bytes owned by the retained frame. Contract
+  helpers copy ABI slices before another successful poll can invalidate them.
+- No auth or environment value is added to tracing/logging. Errors contain
+  operation diagnostics, not patch contents.
+- Generated Cargo/CMake outputs and temporary shape probes are excluded from
+  the implementation package.
+
+## Concern
+
+Dependency-complete Rust verification, including the isolated native-file
+round-trip integration test, must be rerun on a host with more memory or usable
+swap. This is a verification limitation, not a known Task 4 behavior defect.
+
+<!-- codeg-card-summary-v1
+{"kind":"implementation","phase":"implementation","status":"done_with_concerns","summary":"Added the Grok/Codex settings facade and asynchronous get/set/probe bridge workers over existing ACP persistence and preflight helpers.","commits":[{"subject":"feat(eui): expose Grok and Codex settings facade"}],"tests":{"status":"partial","passed":14,"failed":0,"summary":"11 actual-source shape-compatible Rust probes and 3 contracts-only CTest cases pass; full Cargo tests were skipped by parent instruction and dependency-complete codeg checking was host-SIGKILLed."},"concerns":["Dependency-complete settings_contract and shared-codeg verification require more than the available 3.8 GiB memory or usable swap."],"report_file":".superpowers/sdd/2026-08-09-eui-neo-frontend-spike/task-4-report.md"}
+-->
diff --git a/codeg-eui/app/bridge/codeg_eui_bridge.h b/codeg-eui/app/bridge/codeg_eui_bridge.h
index 3e93320b..cbe696b5 100644
--- a/codeg-eui/app/bridge/codeg_eui_bridge.h
+++ b/codeg-eui/app/bridge/codeg_eui_bridge.h
@@ -115,6 +115,8 @@ int codeg_eui_cancel_active_turn(uint64_t* out_request_id);
 int codeg_eui_get_agent_settings(const uint8_t* agent_utf8,
                                  size_t agent_len,
                                  uint64_t* out_request_id);
+/* Settings/probe work is asynchronous; consume the terminal JSON completion
+ * from CodegEuiFrame::completions on a later successful poll. */
 int codeg_eui_set_agent_settings(const uint8_t* agent_utf8,
                                  size_t agent_len,
                                  const uint8_t* json_utf8,
diff --git a/src-tauri/codeg-eui-core/src/abi.rs b/src-tauri/codeg-eui-core/src/abi.rs
index 69446958..905c1f55 100644
--- a/src-tauri/codeg-eui-core/src/abi.rs
+++ b/src-tauri/codeg-eui-core/src/abi.rs
@@ -405,6 +405,11 @@ pub extern "C" fn codeg_eui_set_agent_settings(
             Ok(json) => json,
             Err(error) => return error,
         };
+        if serde_json::from_slice::<codeg_lib::commands::eui_facade::EuiAgentSettingsPatch>(&json)
+            .is_err()
+        {
+            return CODEG_EUI_ERR_INVALID_STATE;
+        }
         accept_and_write(
             &mut slot,
             out_request_id,
diff --git a/src-tauri/codeg-eui-core/src/bootstrap.rs b/src-tauri/codeg-eui-core/src/bootstrap.rs
index f51bf5c3..b12a0ab4 100644
--- a/src-tauri/codeg-eui-core/src/bootstrap.rs
+++ b/src-tauri/codeg-eui-core/src/bootstrap.rs
@@ -1,4 +1,5 @@
 use std::path::{Path, PathBuf};
+use std::sync::Arc;
 
 use codeg_lib::app_state::AppState;
 use codeg_lib::logging::init::LogGuard;
@@ -45,7 +46,7 @@ pub enum BootstrapError {
 }
 
 pub struct EuiBootstrap {
-    pub state: AppState,
+    pub state: Arc<AppState>,
     pub started_services: StartedServices,
     runtime: Option<Runtime>,
     _log_guard: Option<LogGuard>,
@@ -65,7 +66,7 @@ impl EuiBootstrap {
         let runtime = build_runtime()?;
         let state = runtime.block_on(initialize_state(root))?;
 
-        Ok(Self::new(state, runtime, log_guard))
+        Ok(Self::new(Arc::new(state), runtime, log_guard))
     }
 
     pub async fn start_for_test(root: impl AsRef<Path>) -> Result<Self, BootstrapError> {
@@ -79,7 +80,7 @@ impl EuiBootstrap {
             .await
             .map_err(|error| BootstrapError::RuntimeTask(error.to_string()))??;
 
-        Ok(Self::new(state, runtime, log_guard))
+        Ok(Self::new(Arc::new(state), runtime, log_guard))
     }
 
     /// Join the owned runtime before releasing the shared application state.
@@ -97,7 +98,7 @@ impl EuiBootstrap {
             .clone()
     }
 
-    fn new(state: AppState, runtime: Runtime, log_guard: LogGuard) -> Self {
+    fn new(state: Arc<AppState>, runtime: Runtime, log_guard: LogGuard) -> Self {
         Self {
             state,
             started_services: StartedServices::default(),
diff --git a/src-tauri/codeg-eui-core/src/runtime.rs b/src-tauri/codeg-eui-core/src/runtime.rs
index f0c43b0f..4922c487 100644
--- a/src-tauri/codeg-eui-core/src/runtime.rs
+++ b/src-tauri/codeg-eui-core/src/runtime.rs
@@ -1,8 +1,10 @@
 use std::collections::HashMap;
 #[cfg(feature = "ffi-test-hooks")]
 use std::future::pending;
+use std::future::Future;
 use std::num::NonZeroU64;
 use std::panic::{catch_unwind, AssertUnwindSafe};
+use std::pin::Pin;
 use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
 use std::sync::{Arc, Mutex};
 
@@ -18,6 +20,64 @@ use crate::{
     CODEG_EUI_ERR_INVALID_STATE, CODEG_EUI_ERR_QUEUE_FULL,
 };
 
+pub(crate) type CoreFuture = Pin<Box<dyn Future<Output = Result<Vec<u8>, String>> + Send>>;
+
+pub(crate) trait CoreOps: Send + Sync {
+    fn get_agent_settings(&self, agent: Vec<u8>) -> CoreFuture;
+    fn set_agent_settings(&self, agent: Vec<u8>, json: Vec<u8>) -> CoreFuture;
+    fn probe_agent(&self, agent: Vec<u8>) -> CoreFuture;
+}
+
+struct AppCoreOps {
+    state: Arc<codeg_lib::app_state::AppState>,
+}
+
+impl CoreOps for AppCoreOps {
+    fn get_agent_settings(&self, agent: Vec<u8>) -> CoreFuture {
+        let state = Arc::clone(&self.state);
+        Box::pin(async move {
+            let wire = String::from_utf8(agent).map_err(|_| "agent is not UTF-8".to_string())?;
+            let agent = codeg_lib::commands::eui_facade::parse_supported_agent(&wire)
+                .map_err(|error| error.to_string())?;
+            let settings = codeg_lib::commands::eui_facade::get_eui_agent_settings(&state, agent)
+                .await
+                .map_err(|error| error.to_string())?;
+            serde_json::to_vec(&settings).map_err(|error| error.to_string())
+        })
+    }
+
+    fn set_agent_settings(&self, agent: Vec<u8>, json: Vec<u8>) -> CoreFuture {
+        let state = Arc::clone(&self.state);
+        Box::pin(async move {
+            let wire = String::from_utf8(agent).map_err(|_| "agent is not UTF-8".to_string())?;
+            let agent = codeg_lib::commands::eui_facade::parse_supported_agent(&wire)
+                .map_err(|error| error.to_string())?;
+            let patch = serde_json::from_slice::<
+                codeg_lib::commands::eui_facade::EuiAgentSettingsPatch,
+            >(&json)
+            .map_err(|error| format!("invalid agent settings patch: {error}"))?;
+            let settings =
+                codeg_lib::commands::eui_facade::set_eui_agent_settings(&state, agent, patch)
+                    .await
+                    .map_err(|error| error.to_string())?;
+            serde_json::to_vec(&settings).map_err(|error| error.to_string())
+        })
+    }
+
+    fn probe_agent(&self, agent: Vec<u8>) -> CoreFuture {
+        let state = Arc::clone(&self.state);
+        Box::pin(async move {
+            let wire = String::from_utf8(agent).map_err(|_| "agent is not UTF-8".to_string())?;
+            let agent = codeg_lib::commands::eui_facade::parse_supported_agent(&wire)
+                .map_err(|error| error.to_string())?;
+            let probe = codeg_lib::commands::eui_facade::probe_eui_agent(&state, agent)
+                .await
+                .map_err(|error| error.to_string())?;
+            serde_json::to_vec(&probe).map_err(|error| error.to_string())
+        })
+    }
+}
+
 static NEXT_REQUEST_ID: AtomicU64 = AtomicU64::new(1);
 
 #[derive(Clone, Copy)]
@@ -61,6 +121,9 @@ impl RuntimeOwner {
         let admission = Arc::new(Mutex::new(()));
         let quiesced = Arc::new(AtomicBool::new(false));
         let connections = bootstrap.state.connection_manager.clone_ref();
+        let core_ops: Arc<dyn CoreOps> = Arc::new(AppCoreOps {
+            state: Arc::clone(&bootstrap.state),
+        });
         let worker = bootstrap.runtime_handle().spawn(run_worker(
             command_rx,
             shutdown_rx,
@@ -68,6 +131,7 @@ impl RuntimeOwner {
             connections,
             Arc::clone(&admission),
             Arc::clone(&quiesced),
+            core_ops,
         ));
 
         Self {
@@ -158,6 +222,7 @@ async fn run_worker(
     connections: ConnectionManager,
     admission: Arc<Mutex<()>>,
     quiesced: Arc<AtomicBool>,
+    core_ops: Arc<dyn CoreOps>,
 ) {
     let _exit_guard = WorkerExitGuard {
         model: model.clone(),
@@ -187,7 +252,7 @@ async fn run_worker(
                     selection_epoch: command.selection_epoch,
                     op: command.op,
                 };
-                let abort = tasks.spawn(execute_command(command.payload));
+                let abort = tasks.spawn(execute_command(command.op, command.payload, Arc::clone(&core_ops)));
                 metadata.insert(abort.id(), command_metadata);
             }
         }
@@ -235,7 +300,11 @@ fn terminalize_task(
     model.terminalize(command.selection_epoch, completion);
 }
 
-async fn execute_command(payload: CommandPayload) -> Result<Vec<u8>, String> {
+async fn execute_command(
+    op: Operation,
+    payload: CommandPayload,
+    core_ops: Arc<dyn CoreOps>,
+) -> Result<Vec<u8>, String> {
     match payload {
         #[cfg(feature = "ffi-test-hooks")]
         CommandPayload::Blocked => pending().await,
@@ -244,17 +313,20 @@ async fn execute_command(payload: CommandPayload) -> Result<Vec<u8>, String> {
         #[cfg(test)]
         CommandPayload::Panic => panic!("test worker panic"),
         CommandPayload::Empty => Err("operation is not implemented in Task 3".to_string()),
-        CommandPayload::Utf8(value) => {
-            let _ = value;
-            Err("operation is not implemented in Task 3".to_string())
-        }
+        CommandPayload::Utf8(value) => match op {
+            Operation::GetAgentSettings => core_ops.get_agent_settings(value).await,
+            Operation::ProbeAgent => core_ops.probe_agent(value).await,
+            _ => Err("operation is not implemented in Task 3".to_string()),
+        },
         CommandPayload::SelectSession(conversation_id) => {
             let _ = conversation_id;
             Err("operation is not implemented in Task 3".to_string())
         }
         CommandPayload::AgentSettings { agent, json } => {
-            let _ = (agent, json);
-            Err("operation is not implemented in Task 3".to_string())
+            if op != Operation::SetAgentSettings {
+                return Err("invalid settings operation".to_string());
+            }
+            core_ops.set_agent_settings(agent, json).await
         }
     }
 }
@@ -264,24 +336,54 @@ mod tests {
     use std::collections::HashMap;
     use std::num::NonZeroU64;
     use std::sync::atomic::AtomicBool;
+    use std::sync::Arc;
 
+    use tokio::sync::{mpsc, watch, Notify};
     use tokio::task::JoinSet;
 
-    use super::{execute_command, terminalize_task, CommandMetadata};
-    use crate::commands::{CommandPayload, Operation};
+    use super::{
+        execute_command, run_worker, terminalize_task, CommandMetadata, CoreFuture, CoreOps,
+    };
+    use crate::commands::{CommandPayload, Operation, RuntimeCommand};
     use crate::{CompletionStatus, LifecycleState, SharedModel};
 
+    struct ErrorOps;
+
+    impl CoreOps for ErrorOps {
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
     #[tokio::test]
     async fn worker_errors_are_terminal_results() {
         assert_eq!(
-            execute_command(CommandPayload::Error("expected".to_string())).await,
+            execute_command(
+                Operation::SendUserMessage,
+                CommandPayload::Error("expected".to_string()),
+                Arc::new(ErrorOps),
+            )
+            .await,
             Err("expected".to_string())
         );
     }
 
     #[tokio::test]
     async fn worker_panics_are_visible_to_the_join_boundary() {
-        let joined = tokio::spawn(execute_command(CommandPayload::Panic)).await;
+        let joined = tokio::spawn(execute_command(
+            Operation::SendUserMessage,
+            CommandPayload::Panic,
+            Arc::new(ErrorOps),
+        ))
+        .await;
         assert!(joined
             .expect_err("worker panic must be caught by join")
             .is_panic());
@@ -304,7 +406,11 @@ mod tests {
                 .reserve(request_id, Operation::SendUserMessage, 0)
                 .unwrap();
             let mut tasks = JoinSet::new();
-            let abort = tasks.spawn(execute_command(payload));
+            let abort = tasks.spawn(execute_command(
+                Operation::SendUserMessage,
+                payload,
+                Arc::new(ErrorOps),
+            ));
             metadata.insert(
                 abort.id(),
                 CommandMetadata {
@@ -329,4 +435,80 @@ mod tests {
             .iter()
             .all(|completion| completion.error.len > 0));
     }
+
+    struct SlowProbeOps {
+        gate: Arc<Notify>,
+    }
+
+    impl CoreOps for SlowProbeOps {
+        fn get_agent_settings(&self, _agent: Vec<u8>) -> CoreFuture {
+            Box::pin(async { Err("unexpected get".to_string()) })
+        }
+
+        fn set_agent_settings(&self, _agent: Vec<u8>, _json: Vec<u8>) -> CoreFuture {
+            Box::pin(async { Err("unexpected set".to_string()) })
+        }
+
+        fn probe_agent(&self, _agent: Vec<u8>) -> CoreFuture {
+            let gate = Arc::clone(&self.gate);
+            Box::pin(async move {
+                gate.notified().await;
+                Ok(br#"{"launchable":true}"#.to_vec())
+            })
+        }
+    }
+
+    #[tokio::test]
+    async fn slow_probe_does_not_block_frame_build_and_completes_once() {
+        let gate = Arc::new(Notify::new());
+        let model = SharedModel::new();
+        let request_id = NonZeroU64::new(41).unwrap();
+        model.reserve(request_id, Operation::ProbeAgent, 0).unwrap();
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
+            Arc::new(SlowProbeOps {
+                gate: Arc::clone(&gate),
+            }),
+        ));
+        command_tx
+            .send(RuntimeCommand {
+                request_id,
+                selection_epoch: 0,
+                op: Operation::ProbeAgent,
+                payload: CommandPayload::Utf8(b"codex".to_vec()),
+            })
+            .await
+            .unwrap();
+
+        let (first, _) = model.build_frame(false, &quiesced);
+        let first_abi = first.as_abi(LifecycleState::Running, 1, false);
+        assert_eq!(first_abi.completions_len, 0);
+
+        gate.notify_one();
+        let mut completions_seen = 0;
+        for generation in 2..=100 {
+            tokio::task::yield_now().await;
+            let (frame, _) = model.build_frame(false, &quiesced);
+            let abi = frame.as_abi(LifecycleState::Running, generation, false);
+            completions_seen += abi.completions_len;
+            if abi.completions_len == 1 {
+                let completion = unsafe { &*abi.completions };
+                assert_eq!(completion.request_id, request_id.get());
+                assert_eq!(completion.op, Operation::ProbeAgent as u32);
+                assert_eq!(completion.status, CompletionStatus::Ok as u32);
+                break;
+            }
+        }
+        assert_eq!(completions_seen, 1);
+        shutdown_tx.send(true).unwrap();
+        worker.await.unwrap();
+    }
 }
diff --git a/src-tauri/codeg-eui-core/tests/settings_contract.rs b/src-tauri/codeg-eui-core/tests/settings_contract.rs
new file mode 100644
index 00000000..7554ebc4
--- /dev/null
+++ b/src-tauri/codeg-eui-core/tests/settings_contract.rs
@@ -0,0 +1,252 @@
+use std::process::Command;
+use std::thread;
+use std::time::Duration;
+
+use codeg_eui_core::{
+    codeg_eui_begin_shutdown, codeg_eui_get_agent_settings, codeg_eui_init, codeg_eui_poll,
+    codeg_eui_set_agent_settings, CodegEuiCompletion, CodegEuiFrame, CodegEuiSlice,
+    CODEG_EUI_ERR_INVALID_STATE, CODEG_EUI_OK,
+};
+use serde_json::Value;
+
+const CASE_ENV: &str = "CODEG_EUI_SETTINGS_CONTRACT_CASE";
+const ROOT_ENV: &str = "CODEG_EUI_SETTINGS_CONTRACT_ROOT";
+
+#[test]
+fn malformed_patch_is_rejected_before_acceptance() {
+    run_isolated("malformed", || {
+        assert_eq!(init(), CODEG_EUI_OK);
+        let mut request_id = 1234;
+        let agent = b"codex";
+        let malformed = br#"{"enabled":true,"unknown":1}"#;
+        assert_eq!(
+            codeg_eui_set_agent_settings(
+                agent.as_ptr(),
+                agent.len(),
+                malformed.as_ptr(),
+                malformed.len(),
+                &mut request_id,
+            ),
+            CODEG_EUI_ERR_INVALID_STATE
+        );
+        assert_eq!(request_id, 1234);
+        complete_shutdown();
+    });
+}
+
+#[test]
+fn settings_get_result_arrives_through_poll_completion() {
+    run_isolated("get", || {
+        assert_eq!(init(), CODEG_EUI_OK);
+        let mut request_id = 0;
+        let agent = b"codex";
+        assert_eq!(
+            codeg_eui_get_agent_settings(agent.as_ptr(), agent.len(), &mut request_id),
+            CODEG_EUI_OK
+        );
+        let completion = wait_for_completion(request_id);
+        assert_eq!(completion.request_id, request_id);
+        assert_eq!(completion.status, 0);
+        assert!(!completion.result_payload.is_empty());
+        complete_shutdown();
+    });
+}
+
+#[test]
+fn codex_and_grok_settings_round_trip_through_native_files() {
+    run_isolated("round_trip", || {
+        assert_eq!(init(), CODEG_EUI_OK);
+
+        let codex_patch = br#"{
+            "enabled":true,
+            "env":{"OPENAI_API_KEY":"test-key"},
+            "codexAuthJson":"{\"OPENAI_API_KEY\":\"test-key\"}",
+            "codexConfigToml":"model = \"gpt-5\"\napproval_policy = \"never\"\n"
+        }"#;
+        let mut codex_set_id = 0;
+        assert_eq!(
+            codeg_eui_set_agent_settings(
+                b"codex".as_ptr(),
+                5,
+                codex_patch.as_ptr(),
+                codex_patch.len(),
+                &mut codex_set_id,
+            ),
+            CODEG_EUI_OK
+        );
+        let codex_set = wait_for_completion(codex_set_id);
+        assert_completion_ok(&codex_set);
+
+        let codex = get_settings("codex");
+        assert_eq!(codex["agentType"], "codex");
+        assert_eq!(codex["enabled"], true);
+        assert_eq!(codex["env"]["OPENAI_API_KEY"], "test-key");
+        assert_eq!(
+            codex["codexConfigToml"],
+            "model = \"gpt-5\"\napproval_policy = \"never\"\n"
+        );
+        assert_eq!(codex["codexAuthJson"], r#"{"OPENAI_API_KEY":"test-key"}"#);
+        assert!(codex["grokConfigToml"].is_null());
+
+        let codex_home = std::env::var("CODEX_HOME").expect("isolated CODEX_HOME");
+        assert_eq!(
+            std::fs::read_to_string(std::path::Path::new(&codex_home).join("config.toml"))
+                .expect("Codex config.toml"),
+            codex["codexConfigToml"].as_str().unwrap()
+        );
+        assert_eq!(
+            std::fs::read_to_string(std::path::Path::new(&codex_home).join("auth.json"))
+                .expect("Codex auth.json"),
+            codex["codexAuthJson"].as_str().unwrap()
+        );
+
+        let grok_patch = br#"{
+            "grokConfigToml":"[ui]\npermission_mode = \"default\"\n",
+            "grokStructured":{"defaultReasoningEffort":"high","permissionMode":"plan"}
+        }"#;
+        let mut grok_set_id = 0;
+        assert_eq!(
+            codeg_eui_set_agent_settings(
+                b"grok".as_ptr(),
+                4,
+                grok_patch.as_ptr(),
+                grok_patch.len(),
+                &mut grok_set_id,
+            ),
+            CODEG_EUI_OK
+        );
+        let grok_set = wait_for_completion(grok_set_id);
+        assert_completion_ok(&grok_set);
+
+        let grok = get_settings("grok");
+        assert_eq!(grok["agentType"], "grok");
+        assert_eq!(grok["grokSettings"]["default_reasoning_effort"], "high");
+        assert_eq!(grok["grokSettings"]["permission_mode"], "plan");
+        assert!(grok["codexConfigToml"].is_null());
+        let grok_toml = grok["grokConfigToml"].as_str().expect("Grok raw TOML");
+        assert!(grok_toml.contains("default_reasoning_effort = \"high\""));
+        assert!(grok_toml.contains("permission_mode = \"plan\""));
+
+        let grok_home = std::env::var("GROK_HOME").expect("isolated GROK_HOME");
+        assert_eq!(
+            std::fs::read_to_string(std::path::Path::new(&grok_home).join("config.toml"))
+                .expect("Grok config.toml"),
+            grok_toml
+        );
+
+        complete_shutdown();
+    });
+}
+
+fn run_isolated(case: &str, body: impl FnOnce()) {
+    if std::env::var(CASE_ENV).as_deref() == Ok(case) {
+        body();
+        return;
+    }
+    if std::env::var_os(CASE_ENV).is_some() {
+        return;
+    }
+
+    let root = tempfile::tempdir().expect("tempdir");
+    let status = Command::new(std::env::current_exe().expect("test executable"))
+        .args(["--exact", thread::current().name().expect("test name")])
+        .env(CASE_ENV, case)
+        .env(ROOT_ENV, root.path())
+        .env("CODEX_HOME", root.path().join("codex"))
+        .env("GROK_HOME", root.path().join("grok"))
+        .status()
+        .expect("run isolated settings contract");
+    assert!(status.success(), "isolated case {case} failed");
+}
+
+fn init() -> i32 {
+    let root = std::env::var(ROOT_ENV).expect("isolated root");
+    codeg_eui_init(root.as_ptr(), root.len())
+}
+
+fn poll() -> CodegEuiFrame {
+    let mut frame = CodegEuiFrame::default();
+    assert_eq!(codeg_eui_poll(&mut frame), CODEG_EUI_OK);
+    frame
+}
+
+#[derive(Debug)]
+struct OwnedCompletion {
+    request_id: u64,
+    status: u32,
+    result_payload: Vec<u8>,
+    error: Vec<u8>,
+}
+
+impl OwnedCompletion {
+    fn copy_from(completion: CodegEuiCompletion) -> Self {
+        Self {
+            request_id: completion.request_id,
+            status: completion.status,
+            result_payload: copy_slice(completion.result_payload),
+            error: copy_slice(completion.error),
+        }
+    }
+}
+
+fn copy_slice(slice: CodegEuiSlice) -> Vec<u8> {
+    if slice.len == 0 {
+        return Vec::new();
+    }
+    assert!(
+        !slice.ptr.is_null(),
+        "non-empty ABI slice must have a pointer"
+    );
+    unsafe { std::slice::from_raw_parts(slice.ptr, slice.len).to_vec() }
+}
+
+fn get_settings(agent: &str) -> Value {
+    let mut request_id = 0;
+    assert_eq!(
+        codeg_eui_get_agent_settings(agent.as_ptr(), agent.len(), &mut request_id),
+        CODEG_EUI_OK
+    );
+    let completion = wait_for_completion(request_id);
+    assert_completion_ok(&completion);
+    serde_json::from_slice(&completion.result_payload).expect("settings completion JSON")
+}
+
+fn assert_completion_ok(completion: &OwnedCompletion) {
+    assert_eq!(
+        completion.status,
+        0,
+        "completion failed: {}",
+        String::from_utf8_lossy(&completion.error)
+    );
+    assert!(!completion.result_payload.is_empty());
+}
+
+fn wait_for_completion(request_id: u64) -> OwnedCompletion {
+    for _ in 0..200 {
+        let frame = poll();
+        if frame.completions_len > 0 {
+            let completions =
+                unsafe { std::slice::from_raw_parts(frame.completions, frame.completions_len) };
+            if let Some(completion) = completions
+                .iter()
+                .find(|completion| completion.request_id == request_id)
+            {
+                return OwnedCompletion::copy_from(*completion);
+            }
+        }
+        thread::sleep(Duration::from_millis(5));
+    }
+    panic!("request {request_id} did not complete");
+}
+
+fn complete_shutdown() {
+    assert_eq!(codeg_eui_begin_shutdown(), CODEG_EUI_OK);
+    for _ in 0..200 {
+        if poll().shutdown_ready == 1 {
+            assert_eq!(codeg_eui_shutdown(), CODEG_EUI_OK);
+            return;
+        }
+        thread::sleep(Duration::from_millis(5));
+    }
+    panic!("shutdown did not become ready");
+}
diff --git a/src-tauri/src/commands/eui_facade.rs b/src-tauri/src/commands/eui_facade.rs
new file mode 100644
index 00000000..c0f67f40
--- /dev/null
+++ b/src-tauri/src/commands/eui_facade.rs
@@ -0,0 +1,359 @@
+use std::collections::BTreeMap;
+
+use serde::{Deserialize, Serialize};
+use thiserror::Error;
+
+use crate::acp::preflight::CheckStatus;
+use crate::acp::types::{
+    AcpAgentInfo, CodexSandboxSettings, CodexSandboxStructuredConfig, GrokSettings,
+    GrokStructuredConfig,
+};
+use crate::app_state::AppState;
+use crate::commands::acp::{
+    acp_list_agents_core, acp_preflight_core, acp_update_agent_config_and_refresh,
+    acp_update_agent_env_and_refresh,
+};
+use crate::models::agent::AgentType;
+
+/// The native EUI settings contract intentionally contains only fields owned
+/// by the existing Grok/Codex ACP settings paths.
+#[derive(Debug, Clone, Serialize)]
+#[serde(rename_all = "camelCase")]
+pub struct EuiAgentSettings {
+    pub agent_type: AgentType,
+    pub available: bool,
+    pub enabled: bool,
+    pub installed_version: Option<String>,
+    pub env: BTreeMap<String, String>,
+    pub config_json: Option<String>,
+    pub codex_auth_json: Option<String>,
+    pub codex_config_toml: Option<String>,
+    pub codex_model_catalog: Option<String>,
+    pub codex_sandbox: Option<CodexSandboxSettings>,
+    pub grok_config_toml: Option<String>,
+    pub grok_settings: Option<GrokSettings>,
+    pub model_provider_id: Option<i32>,
+}
+
+#[derive(Debug, Clone, Default, Deserialize)]
+#[serde(rename_all = "camelCase", deny_unknown_fields)]
+pub struct EuiAgentSettingsPatch {
+    pub enabled: Option<bool>,
+    pub env: Option<BTreeMap<String, String>>,
+    pub model_provider_id: Option<i32>,
+    pub config_json: Option<String>,
+    pub codex_auth_json: Option<String>,
+    pub codex_config_toml: Option<String>,
+    pub codex_model_catalog: Option<String>,
+    pub codex_sandbox: Option<CodexSandboxStructuredConfig>,
+    pub grok_config_toml: Option<String>,
+    pub grok_structured: Option<GrokStructuredConfig>,
+}
+
+#[derive(Debug, Clone, Serialize)]
+#[serde(rename_all = "camelCase")]
+pub struct EuiAgentProbe {
+    pub launchable: bool,
+    pub installed_version: Option<String>,
+    pub message: String,
+}
+
+#[derive(Debug, Error)]
+pub enum EuiFacadeError {
+    #[error("unsupported EUI agent: {0}")]
+    UnsupportedAgent(String),
+    #[error("settings field is not valid for {agent}: {field}")]
+    AgentFieldConflict {
+        agent: AgentType,
+        field: &'static str,
+    },
+    #[error("agent settings row was not found for {0}")]
+    AgentNotFound(AgentType),
+    #[error("invalid agent settings patch: {0}")]
+    InvalidPatch(String),
+    #[error("ACP settings operation failed: {0}")]
+    Acp(#[from] crate::acp::error::AcpError),
+}
+
+impl EuiAgentSettingsPatch {
+    pub(crate) fn validate_for(&self, agent: AgentType) -> Result<(), EuiFacadeError> {
+        match agent {
+            AgentType::Codex => {
+                if self.grok_config_toml.is_some() {
+                    return Err(EuiFacadeError::AgentFieldConflict {
+                        agent,
+                        field: "grokConfigToml",
+                    });
+                }
+                if self.grok_structured.is_some() {
+                    return Err(EuiFacadeError::AgentFieldConflict {
+                        agent,
+                        field: "grokStructured",
+                    });
+                }
+            }
+            AgentType::Grok => {
+                if self.codex_auth_json.is_some() {
+                    return Err(EuiFacadeError::AgentFieldConflict {
+                        agent,
+                        field: "codexAuthJson",
+                    });
+                }
+                if self.codex_config_toml.is_some() {
+                    return Err(EuiFacadeError::AgentFieldConflict {
+                        agent,
+                        field: "codexConfigToml",
+                    });
+                }
+                if self.codex_model_catalog.is_some() {
+                    return Err(EuiFacadeError::AgentFieldConflict {
+                        agent,
+                        field: "codexModelCatalog",
+                    });
+                }
+                if self.codex_sandbox.is_some() {
+                    return Err(EuiFacadeError::AgentFieldConflict {
+                        agent,
+                        field: "codexSandbox",
+                    });
+                }
+            }
+            _ => {
+                return Err(EuiFacadeError::UnsupportedAgent(
+                    agent.as_wire().into_owned(),
+                ))
+            }
+        }
+        Ok(())
+    }
+}
+
+/// Parse the intentionally smaller EUI wire vocabulary. This is called before
+/// touching an AppState so unsupported agents fail closed without DB or config
+/// access.
+pub fn parse_supported_agent(wire: &str) -> Result<AgentType, EuiFacadeError> {
+    match wire {
+        "codex" => Ok(AgentType::Codex),
+        "grok" => Ok(AgentType::Grok),
+        other => Err(EuiFacadeError::UnsupportedAgent(other.to_string())),
+    }
+}
+
+pub(crate) fn project_agent_settings(info: AcpAgentInfo) -> EuiAgentSettings {
+    let is_codex = info.agent_type == AgentType::Codex;
+    let is_grok = info.agent_type == AgentType::Grok;
+    EuiAgentSettings {
+        agent_type: info.agent_type,
+        available: info.available,
+        enabled: info.enabled,
+        installed_version: info.installed_version,
+        env: info.env,
+        config_json: info.config_json,
+        codex_auth_json: is_codex.then_some(info.codex_auth_json).flatten(),
+        codex_config_toml: is_codex.then_some(info.codex_config_toml).flatten(),
+        codex_model_catalog: is_codex.then_some(info.codex_model_catalog).flatten(),
+        codex_sandbox: is_codex.then_some(info.codex_sandbox_settings).flatten(),
+        grok_config_toml: is_grok.then_some(info.grok_config_toml).flatten(),
+        grok_settings: is_grok.then_some(info.grok_settings).flatten(),
+        model_provider_id: info.model_provider_id,
+    }
+}
+
+pub async fn get_eui_agent_settings(
+    state: &AppState,
+    agent: AgentType,
+) -> Result<EuiAgentSettings, EuiFacadeError> {
+    ensure_supported(agent)?;
+    let agents = acp_list_agents_core(&state.db).await?;
+    let info = agents
+        .into_iter()
+        .find(|candidate| candidate.agent_type == agent)
+        .ok_or(EuiFacadeError::AgentNotFound(agent))?;
+    Ok(project_agent_settings(info))
+}
+
+pub async fn set_eui_agent_settings(
+    state: &AppState,
+    agent: AgentType,
+    patch: EuiAgentSettingsPatch,
+) -> Result<EuiAgentSettings, EuiFacadeError> {
+    ensure_supported(agent)?;
+    patch.validate_for(agent)?;
+    let current = get_eui_agent_settings(state, agent).await?;
+
+    let enabled = patch.enabled.unwrap_or(current.enabled);
+    let env = patch.env.clone().unwrap_or_else(|| current.env.clone());
+    let model_provider_id = patch.model_provider_id.or(current.model_provider_id);
+    if patch.enabled.is_some() || patch.env.is_some() || patch.model_provider_id.is_some() {
+        update_env(state, agent, enabled, env, model_provider_id).await?;
+    }
+
+    let has_native_config = patch.config_json.is_some()
+        || patch.codex_auth_json.is_some()
+        || patch.codex_config_toml.is_some()
+        || patch.codex_model_catalog.is_some()
+        || patch.codex_sandbox.is_some()
+        || patch.grok_config_toml.is_some()
+        || patch.grok_structured.is_some();
+    if has_native_config {
+        acp_update_agent_config_and_refresh(
+            agent,
+            patch.config_json,
+            None,
+            patch.codex_auth_json,
+            patch.codex_config_toml,
+            patch.codex_model_catalog,
+            patch.codex_sandbox,
+            patch.grok_config_toml,
+            patch.grok_structured,
+            None,
+            None,
+            &state.db,
+            &state.connection_manager,
+            &state.data_dir,
+            &state.emitter,
+        )
+        .await?;
+    }
+
+    get_eui_agent_settings(state, agent).await
+}
+
+async fn update_env(
+    state: &AppState,
+    agent: AgentType,
+    enabled: bool,
+    env: BTreeMap<String, String>,
+    model_provider_id: Option<i32>,
+) -> Result<(), EuiFacadeError> {
+    acp_update_agent_env_and_refresh(
+        agent,
+        enabled,
+        env,
+        model_provider_id,
+        &state.db,
+        &state.connection_manager,
+        &state.data_dir,
+        &state.emitter,
+    )
+    .await?;
+    Ok(())
+}
+
+pub async fn probe_eui_agent(
+    state: &AppState,
+    agent: AgentType,
+) -> Result<EuiAgentProbe, EuiFacadeError> {
+    ensure_supported(agent)?;
+    let preflight = acp_preflight_core(agent, Some(true), &state.db).await?;
+    let installed_version = get_eui_agent_settings(state, agent)
+        .await?
+        .installed_version;
+    let message = preflight
+        .checks
+        .iter()
+        .filter(|check| !matches!(check.status, CheckStatus::Pass))
+        .map(|check| check.message.clone())
+        .collect::<Vec<_>>()
+        .join("; ");
+    Ok(EuiAgentProbe {
+        launchable: preflight.passed,
+        installed_version,
+        message,
+    })
+}
+
+fn ensure_supported(agent: AgentType) -> Result<(), EuiFacadeError> {
+    match agent {
+        AgentType::Codex | AgentType::Grok => Ok(()),
+        _ => Err(EuiFacadeError::UnsupportedAgent(
+            agent.as_wire().into_owned(),
+        )),
+    }
+}
+
+#[cfg(test)]
+mod tests {
+    use std::collections::BTreeMap;
+
+    use super::{
+        ensure_supported, parse_supported_agent, project_agent_settings, EuiAgentSettingsPatch,
+        EuiFacadeError,
+    };
+    use crate::acp::types::AcpAgentInfo;
+    use crate::models::agent::AgentType;
+
+    #[test]
+    fn only_codex_and_grok_wire_values_are_supported() {
+        assert_eq!(parse_supported_agent("codex").unwrap(), AgentType::Codex);
+        assert_eq!(parse_supported_agent("grok").unwrap(), AgentType::Grok);
+        assert!(matches!(
+            parse_supported_agent("claude"),
+            Err(EuiFacadeError::UnsupportedAgent(_))
+        ));
+    }
+
+    #[test]
+    fn unsupported_typed_agent_is_rejected_by_the_pre_access_guard() {
+        assert!(matches!(
+            ensure_supported(AgentType::ClaudeCode),
+            Err(EuiFacadeError::UnsupportedAgent(_))
+        ));
+    }
+
+    #[test]
+    fn projected_codex_settings_do_not_expose_grok_fields() {
+        let info = AcpAgentInfo {
+            agent_type: AgentType::Codex,
+            skills_capable: false,
+            registry_id: "codex".into(),
+            registry_version: None,
+            name: "Codex".into(),
+            description: String::new(),
+            available: true,
+            distribution_type: "npx".into(),
+            custom_source: None,
+            enabled: true,
+            show_thinking: false,
+            sort_order: 0,
+            installed_version: Some("1.0.0".into()),
+            env: BTreeMap::from([(String::from("OPENAI_API_KEY"), String::from("secret"))]),
+            config_json: None,
+            config_file_path: None,
+            opencode_auth_json: None,
+            codex_auth_json: Some(String::from("{}")),
+            codex_config_toml: Some(String::from("model = \"gpt-5\"\n")),
+            codex_model_catalog: None,
+            codex_sandbox_settings: None,
+            cline_secrets_json: None,
+            hermes_config_yaml: None,
+            grok_config_toml: Some(String::from("must be omitted")),
+            grok_settings: None,
+            cursor_cli_config_json: None,
+            cursor_settings: None,
+            model_provider_id: Some(7),
+            icon_url: None,
+        };
+
+        let projected = project_agent_settings(info);
+        assert_eq!(projected.agent_type, AgentType::Codex);
+        assert_eq!(
+            projected.codex_config_toml.as_deref(),
+            Some("model = \"gpt-5\"\n")
+        );
+        assert!(projected.grok_config_toml.is_none());
+        assert!(projected.grok_settings.is_none());
+    }
+
+    #[test]
+    fn patch_rejects_fields_owned_by_the_other_agent() {
+        let patch = EuiAgentSettingsPatch {
+            grok_config_toml: Some(String::from("[ui]\n")),
+            ..Default::default()
+        };
+        assert!(matches!(
+            patch.validate_for(AgentType::Codex),
+            Err(EuiFacadeError::AgentFieldConflict { .. })
+        ));
+    }
+}
diff --git a/src-tauri/src/commands/mod.rs b/src-tauri/src/commands/mod.rs
index e7b73b91..b78a083c 100644
--- a/src-tauri/src/commands/mod.rs
+++ b/src-tauri/src/commands/mod.rs
@@ -14,6 +14,7 @@ pub mod custom_skills;
 pub mod delegate_access;
 pub mod delegation;
 pub mod document_translate;
+pub mod eui_facade;
 pub mod experts;
 pub mod feedback;
 #[cfg(feature = "tauri-runtime")]
