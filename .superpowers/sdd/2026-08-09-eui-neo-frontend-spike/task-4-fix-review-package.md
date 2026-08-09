# Task 4 Fix Review Package
FIX_BASE: 89c0889f6faf8d3ad482c9e4e1a6a34df65d8cbf HEAD: 29904a3a8fe6a741372809dfccb08f7a2e194e9f
Parent: SKIP all full cargo test
29904a3a fix(eui): compile settings bridge contract
 .../task-4-report.md                               | 40 +++++++++++++
 .../codeg-eui-core/tests/settings_contract.rs      | 66 +++++++++++++++++++++-
 2 files changed, 104 insertions(+), 2 deletions(-)
diff --git a/.superpowers/sdd/2026-08-09-eui-neo-frontend-spike/task-4-report.md b/.superpowers/sdd/2026-08-09-eui-neo-frontend-spike/task-4-report.md
index 0d38e407..bea95956 100644
--- a/.superpowers/sdd/2026-08-09-eui-neo-frontend-spike/task-4-report.md
+++ b/.superpowers/sdd/2026-08-09-eui-neo-frontend-spike/task-4-report.md
@@ -135,5 +135,45 @@ round-trip integration test, must be rerun on a host with more memory or usable
 swap. This is a verification limitation, not a known Task 4 behavior defect.
 
 <!-- codeg-card-summary-v1
 {"kind":"implementation","phase":"implementation","status":"done_with_concerns","summary":"Added the Grok/Codex settings facade and asynchronous get/set/probe bridge workers over existing ACP persistence and preflight helpers.","commits":[{"subject":"feat(eui): expose Grok and Codex settings facade"}],"tests":{"status":"partial","passed":14,"failed":0,"summary":"11 actual-source shape-compatible Rust probes and 3 contracts-only CTest cases pass; full Cargo tests were skipped by parent instruction and dependency-complete codeg checking was host-SIGKILLed."},"concerns":["Dependency-complete settings_contract and shared-codeg verification require more than the available 3.8 GiB memory or usable swap."],"report_file":".superpowers/sdd/2026-08-09-eui-neo-frontend-spike/task-4-report.md"}
 -->
+
+## High-Gate Fix Round 1/5
+
+Status: DONE_WITH_CONCERNS
+
+The two independent reviewers found the same Important defect: the focused
+`settings_contract` called `codeg_eui_shutdown` without importing it, so the
+integration target could not resolve that name. The import is now present.
+
+The cheap review minors were also covered in the same contract target:
+
+- A settings body of `CODEG_EUI_MAX_SETTINGS_JSON_BYTES + 1` is rejected with
+  `CODEG_EUI_ERR_TOO_LARGE` before acceptance and leaves the request ID
+  unchanged.
+- The public get-settings ABI accepts an unsupported wire request
+  asynchronously, then returns exactly one error completion with no settings
+  payload.
+- The public probe ABI returns a JSON completion containing `launchable`,
+  `installedVersion`, and `message` through the worker/facade route.
+
+TDD and verification evidence:
+
+- RED: direct `rustc --test` compilation of the committed contract failed with
+  `E0425: cannot find function codeg_eui_shutdown in this scope` at
+  `complete_shutdown`.
+- GREEN: the complete `settings_contract.rs` compiled with `-D warnings`
+  against the established shape-compatible Task 4 boundary.
+- Five selected real-ABI contract cases passed: malformed, oversized,
+  unsupported agent, probe completion, and get completion.
+- Contracts-only CTest remained **3/3 passed**.
+- Both Cargo format checks, `git diff --check`, approved-design digest, and
+  standalone-lockfile absence passed.
+
+Per parent instruction, **all full Cargo tests remain skipped**. The isolated
+native-file round-trip still requires dependency-complete execution on a host
+with more memory or usable swap; this fix round does not claim otherwise.
+
+<!-- codeg-card-summary-v1
+{"kind":"implementation","phase":"fix","status":"done_with_concerns","summary":"Task 4 high-gate I1 fixed: settings_contract now imports codeg_eui_shutdown and covers oversized JSON, unsupported agents, and the public probe ABI.","commits":[{"subject":"fix(eui): compile settings bridge contract"}],"tests":{"status":"partial","passed":8,"failed":0,"summary":"5 selected settings ABI contract cases and 3 contracts-only CTest cases pass; the full Cargo suite remains skipped by parent instruction."},"concerns":["Dependency-complete native-file round-trip and shared-codeg verification still require more than the available 3.8 GiB memory or usable swap."],"report_file":".superpowers/sdd/2026-08-09-eui-neo-frontend-spike/task-4-report.md"}
+-->
diff --git a/src-tauri/codeg-eui-core/tests/settings_contract.rs b/src-tauri/codeg-eui-core/tests/settings_contract.rs
index 7554ebc4..5a80cf66 100644
--- a/src-tauri/codeg-eui-core/tests/settings_contract.rs
+++ b/src-tauri/codeg-eui-core/tests/settings_contract.rs
@@ -2,12 +2,13 @@ use std::process::Command;
 use std::thread;
 use std::time::Duration;
 
 use codeg_eui_core::{
     codeg_eui_begin_shutdown, codeg_eui_get_agent_settings, codeg_eui_init, codeg_eui_poll,
-    codeg_eui_set_agent_settings, CodegEuiCompletion, CodegEuiFrame, CodegEuiSlice,
-    CODEG_EUI_ERR_INVALID_STATE, CODEG_EUI_OK,
+    codeg_eui_probe_agent, codeg_eui_set_agent_settings, codeg_eui_shutdown, CodegEuiCompletion,
+    CodegEuiFrame, CodegEuiSlice, CODEG_EUI_ERR_INVALID_STATE, CODEG_EUI_ERR_TOO_LARGE,
+    CODEG_EUI_MAX_SETTINGS_JSON_BYTES, CODEG_EUI_OK,
 };
 use serde_json::Value;
 
 const CASE_ENV: &str = "CODEG_EUI_SETTINGS_CONTRACT_CASE";
 const ROOT_ENV: &str = "CODEG_EUI_SETTINGS_CONTRACT_ROOT";
@@ -32,10 +33,71 @@ fn malformed_patch_is_rejected_before_acceptance() {
         assert_eq!(request_id, 1234);
         complete_shutdown();
     });
 }
 
+#[test]
+fn oversized_patch_is_rejected_before_acceptance() {
+    run_isolated("oversized", || {
+        assert_eq!(init(), CODEG_EUI_OK);
+        let mut request_id = 1234;
+        let oversized = vec![b' '; CODEG_EUI_MAX_SETTINGS_JSON_BYTES + 1];
+        assert_eq!(
+            codeg_eui_set_agent_settings(
+                b"codex".as_ptr(),
+                5,
+                oversized.as_ptr(),
+                oversized.len(),
+                &mut request_id,
+            ),
+            CODEG_EUI_ERR_TOO_LARGE
+        );
+        assert_eq!(request_id, 1234);
+        complete_shutdown();
+    });
+}
+
+#[test]
+fn unsupported_agent_completes_with_an_error() {
+    run_isolated("unsupported", || {
+        assert_eq!(init(), CODEG_EUI_OK);
+        let agent = b"claude_code";
+        let mut request_id = 0;
+        assert_eq!(
+            codeg_eui_get_agent_settings(agent.as_ptr(), agent.len(), &mut request_id),
+            CODEG_EUI_OK
+        );
+
+        let completion = wait_for_completion(request_id);
+        assert_eq!(completion.status, 1);
+        assert!(completion.result_payload.is_empty());
+        assert!(String::from_utf8_lossy(&completion.error).contains("unsupported EUI agent"));
+        complete_shutdown();
+    });
+}
+
+#[test]
+fn probe_result_arrives_through_the_public_abi() {
+    run_isolated("probe", || {
+        assert_eq!(init(), CODEG_EUI_OK);
+        let mut request_id = 0;
+        assert_eq!(
+            codeg_eui_probe_agent(b"codex".as_ptr(), 5, &mut request_id),
+            CODEG_EUI_OK
+        );
+
+        let completion = wait_for_completion(request_id);
+        assert_completion_ok(&completion);
+        let probe: Value =
+            serde_json::from_slice(&completion.result_payload).expect("probe completion JSON");
+        assert!(probe["launchable"].is_boolean());
+        assert!(probe["message"].is_string());
+        assert!(probe["installedVersion"].is_null() || probe["installedVersion"].is_string());
+        complete_shutdown();
+    });
+}
+
 #[test]
 fn settings_get_result_arrives_through_poll_completion() {
     run_isolated("get", || {
         assert_eq!(init(), CODEG_EUI_OK);
         let mut request_id = 0;
