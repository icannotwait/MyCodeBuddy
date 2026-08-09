# Task 3 Fix Review Package
FIX_BASE: b55f20ddb97706ebd78126e5ffd5ef4cb249ab57 HEAD: 66f7cff1ee5b02773f19f938482c3a112792ecb0
Parent: SKIP all full cargo test
66f7cff1 fix(eui): gate blocked shutdown test hook
 .../task-3-report.md                               | 35 ++++++++++++++++++++++
 codeg-eui/tests/assert_rust_hook_absent.sh         | 26 ++++++++++++++++
 src-tauri/codeg-eui-core/src/abi.rs                |  1 +
 src-tauri/codeg-eui-core/src/commands.rs           |  1 +
 src-tauri/codeg-eui-core/src/runtime.rs            |  2 ++
 .../codeg-eui-core/tests/shutdown_contract.rs      |  2 ++
 6 files changed, 67 insertions(+)
diff --git a/.superpowers/sdd/2026-08-09-eui-neo-frontend-spike/task-3-report.md b/.superpowers/sdd/2026-08-09-eui-neo-frontend-spike/task-3-report.md
index 22ee0321..f87d7769 100644
--- a/.superpowers/sdd/2026-08-09-eui-neo-frontend-spike/task-3-report.md
+++ b/.superpowers/sdd/2026-08-09-eui-neo-frontend-spike/task-3-report.md
@@ -112,5 +112,40 @@ compile. This is an authorized host limitation, not an open Task 3 defect.
   excluded from the implementation package.
 
 <!-- codeg-card-summary-v1
 {"kind":"implementation","phase":"implementation","status":"done_with_concerns","summary":"Implemented EUI ABI v1 async lifecycle, bounded request/completion ledger, immutable frames, shutdown drain, and C++ deep-copy boundary.","commits":[{"subject":"feat(eui): implement async bridge lifecycle"}],"tests":{"status":"pass","passed":14,"failed":0,"summary":"14 focused Rust tests plus 3 contracts-only and 4 ABI-linked CTest cases pass; full Cargo tests skipped by parent instruction and shared-core Cargo check SIGKILLed on the 4GiB/no-swap host."},"concerns":["Dependency-complete Cargo verification requires a host with more memory or usable swap; the parent explicitly authorized skipping full Cargo tests for this spike."],"report_file":".superpowers/sdd/2026-08-09-eui-neo-frontend-spike/task-3-report.md"}
 -->
+
+## High-Gate Fix Round 1/5
+
+Status: DONE_WITH_CONCERNS
+
+Codex I1 is resolved. The synthetic blocked-request stimulus is now present
+only under `ffi-test-hooks`:
+
+- `enqueue_blocked_for_test` is feature-gated and is absent from the normal
+  Rust rlib API surface.
+- `CommandPayload::Blocked`, the forever-pending executor branch, and the C
+  export remain available only with `ffi-test-hooks`.
+- `shutdown_contract.rs` is an opt-in feature test, while the C header/export
+  remains conditionally declared under `CODEG_EUI_TEST_HOOKS`.
+- Added `assert_rust_hook_absent.sh`, which compiles a normal-feature rlib
+  probe and requires the expected unresolved-symbol/API diagnostic.
+
+Focused fix verification passed:
+
+- Normal-feature direct rlib compile with `-D warnings`.
+- Normal-feature rlib API probe: compile fails on
+  `enqueue_blocked_for_test` as required.
+- Normal static archive exports zero `codeg_eui_test_*` symbols; the opt-in
+  archive exports exactly `codeg_eui_test_enqueue_blocked`.
+- Normal focused unit/bridge/ABI probes: **13 passed, 0 failed**.
+- Opt-in focused unit/shutdown probes: **7 passed, 0 failed**.
+- ABI-linked CTest after rebuilding against the gated archive: **4/4 passed**.
+- C11 header syntax, shell syntax, CTest registration, formatting, and diff
+  checks passed.
+
+The parent instruction remains in force: no full Cargo test was run.
+
+<!-- codeg-card-summary-v1
+{"kind":"implementation","phase":"fix","status":"done_with_concerns","summary":"High-gate I1 fixed: blocked shutdown stimulus, payload, and forever-pending executor are now opt-in under ffi-test-hooks; normal rlib API is proven unusable.","commits":[{"subject":"fix(eui): gate blocked shutdown test hook"}],"tests":{"status":"pass","passed":20,"failed":0,"summary":"13 normal-feature focused probes + 7 opt-in focused probes pass; 4/4 ABI-linked CTest pass; full Cargo tests skipped by parent."},"concerns":["Full Cargo tests remain skipped per parent instruction."],"report_file":".superpowers/sdd/2026-08-09-eui-neo-frontend-spike/task-3-report.md"}
+-->
diff --git a/codeg-eui/tests/assert_rust_hook_absent.sh b/codeg-eui/tests/assert_rust_hook_absent.sh
new file mode 100755
index 00000000..882d0eb8
--- /dev/null
+++ b/codeg-eui/tests/assert_rust_hook_absent.sh
@@ -0,0 +1,26 @@
+#!/bin/sh
+set -eu
+
+normal_rlib=$1
+test -f "$normal_rlib"
+
+probe_dir=$(mktemp -d)
+trap 'rm -rf "$probe_dir"' EXIT HUP INT TERM
+probe="$probe_dir/hook_probe.rs"
+cat >"$probe" <<'EOF'
+extern crate codeg_eui_core;
+
+fn main() {
+    let _hook: fn() -> Result<u64, i32> = codeg_eui_core::enqueue_blocked_for_test;
+    let _ = _hook;
+}
+EOF
+
+if rustc --edition=2021 --extern "codeg_eui_core=$normal_rlib" \
+    -L "dependency=$(dirname "$normal_rlib")" -o "$probe_dir/hook_probe" "$probe" \
+    >"$probe_dir/stdout" 2>"$probe_dir/stderr"; then
+    printf 'normal-feature rlib unexpectedly exposes enqueue_blocked_for_test\n' >&2
+    exit 1
+fi
+
+grep -F 'enqueue_blocked_for_test' "$probe_dir/stderr" >/dev/null
diff --git a/src-tauri/codeg-eui-core/src/abi.rs b/src-tauri/codeg-eui-core/src/abi.rs
index 9b9d8794..69446958 100644
--- a/src-tauri/codeg-eui-core/src/abi.rs
+++ b/src-tauri/codeg-eui-core/src/abi.rs
@@ -427,10 +427,11 @@ pub extern "C" fn codeg_eui_probe_agent(
         out_request_id,
         Operation::ProbeAgent,
     )
 }
 
+#[cfg(feature = "ffi-test-hooks")]
 #[doc(hidden)]
 pub fn enqueue_blocked_for_test() -> Result<u64, i32> {
     let mut request_id = 0;
     let code = enqueue_payload(
         &mut request_id,
diff --git a/src-tauri/codeg-eui-core/src/commands.rs b/src-tauri/codeg-eui-core/src/commands.rs
index c1c89ebc..327b2b6c 100644
--- a/src-tauri/codeg-eui-core/src/commands.rs
+++ b/src-tauri/codeg-eui-core/src/commands.rs
@@ -22,10 +22,11 @@ pub(crate) enum CommandPayload {
     SelectSession(i32),
     AgentSettings {
         agent: Vec<u8>,
         json: Vec<u8>,
     },
+    #[cfg(feature = "ffi-test-hooks")]
     Blocked,
     #[cfg(test)]
     Error(String),
     #[cfg(test)]
     Panic,
diff --git a/src-tauri/codeg-eui-core/src/runtime.rs b/src-tauri/codeg-eui-core/src/runtime.rs
index 9d4242b3..f0c43b0f 100644
--- a/src-tauri/codeg-eui-core/src/runtime.rs
+++ b/src-tauri/codeg-eui-core/src/runtime.rs
@@ -1,6 +1,7 @@
 use std::collections::HashMap;
+#[cfg(feature = "ffi-test-hooks")]
 use std::future::pending;
 use std::num::NonZeroU64;
 use std::panic::{catch_unwind, AssertUnwindSafe};
 use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
 use std::sync::{Arc, Mutex};
@@ -234,10 +235,11 @@ fn terminalize_task(
     model.terminalize(command.selection_epoch, completion);
 }
 
 async fn execute_command(payload: CommandPayload) -> Result<Vec<u8>, String> {
     match payload {
+        #[cfg(feature = "ffi-test-hooks")]
         CommandPayload::Blocked => pending().await,
         #[cfg(test)]
         CommandPayload::Error(error) => Err(error),
         #[cfg(test)]
         CommandPayload::Panic => panic!("test worker panic"),
diff --git a/src-tauri/codeg-eui-core/tests/shutdown_contract.rs b/src-tauri/codeg-eui-core/tests/shutdown_contract.rs
index e7ccf6a0..5b275d2a 100644
--- a/src-tauri/codeg-eui-core/tests/shutdown_contract.rs
+++ b/src-tauri/codeg-eui-core/tests/shutdown_contract.rs
@@ -1,5 +1,7 @@
+#![cfg(feature = "ffi-test-hooks")]
+
 use std::process::Command;
 use std::time::Duration;
 
 use codeg_eui_core::{
     codeg_eui_begin_shutdown, codeg_eui_init, codeg_eui_poll, codeg_eui_send_user_message,
