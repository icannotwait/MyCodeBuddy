# Task 3 Review Package
BASE: be8b41cf8545470694e2d0b490ec5b6f6cb1a227 HEAD: b55f20ddb97706ebd78126e5ffd5ef4cb249ab57
Parent rule: SKIP all full cargo test
b55f20dd feat(eui): implement async bridge lifecycle
 .../task-3-report.md                               | 116 +++++
 codeg-eui/CMakeLists.txt                           |   1 +
 codeg-eui/app/bridge/codeg_eui_bridge.h            | 143 ++++-
 codeg-eui/app/bridge/ui_snapshot.h                 | 104 ++++
 codeg-eui/tests/abi_layout_test.cpp                |  96 +++-
 codeg-eui/tests/shutdown_drain_test.cpp            |  82 +++
 codeg-eui/tests/ui_snapshot_test.cpp               |  87 ++++
 src-tauri/codeg-eui-core/Cargo.toml                |   6 +-
 src-tauri/codeg-eui-core/src/abi.rs                | 578 +++++++++++++++++----
 src-tauri/codeg-eui-core/src/bootstrap.rs          |   9 +
 src-tauri/codeg-eui-core/src/commands.rs           |  48 ++
 src-tauri/codeg-eui-core/src/data_root.rs          |  14 +-
 src-tauri/codeg-eui-core/src/lib.rs                |   9 +
 src-tauri/codeg-eui-core/src/model.rs              | 444 ++++++++++++++++
 src-tauri/codeg-eui-core/src/runtime.rs            | 330 ++++++++++++
 src-tauri/codeg-eui-core/tests/abi_smoke.rs        |  16 +-
 src-tauri/codeg-eui-core/tests/bridge_contract.rs  | 376 ++++++++++++++
 .../codeg-eui-core/tests/data_root_isolation.rs    |  33 +-
 .../codeg-eui-core/tests/shutdown_contract.rs      |  74 +++
 19 files changed, 2452 insertions(+), 114 deletions(-)
diff --git a/.superpowers/sdd/2026-08-09-eui-neo-frontend-spike/task-3-report.md b/.superpowers/sdd/2026-08-09-eui-neo-frontend-spike/task-3-report.md
new file mode 100644
index 00000000..22ee0321
--- /dev/null
+++ b/.superpowers/sdd/2026-08-09-eui-neo-frontend-spike/task-3-report.md
@@ -0,0 +1,116 @@
+# Task 3 Implementer Report
+
+## Status
+
+DONE_WITH_CONCERNS
+
+Task 3 implements the asynchronous EUI bridge lifecycle, bounded command
+admission, exactly-once request completions, immutable polled frames, and the
+two-phase shutdown drain. The public ABI and C++ value-copy boundary are
+covered by focused headless contracts.
+
+## Implementation
+
+- Expanded ABI v1 with stable errors `0..9`, lifecycle/operation/completion
+  discriminants, pointer-plus-length slices, session summaries, completion
+  records, and the complete 160-byte frame layout.
+- Replaced the bootstrap slot/atomics with a process-global
+  `OnceLock<Mutex<BridgeSlot>>`, UI-thread affinity checks, checked frame
+  generations, contained FFI panics, and diagnostic error-strip recording.
+- Added Tokio bounded command admission (256), monotonic non-zero request IDs,
+  completion-capacity reservation (256), worker error/panic conversion,
+  selection-epoch stale marking, and exactly-once terminalization.
+- Added immutable `OwnedFrame` backing with owned nested strings and parallel
+  C views. Successful polls atomically transfer ready completions into the
+  retained frame; failed polls leave the prior frame untouched.
+- Added shutdown admission closure, worker/task cancellation, ACP disconnect,
+  stopping polls, observable `shutdown_ready`, final runtime teardown, frame
+  free, and stopped-state behavior. Worker-exit admission is serialized so an
+  unexpected worker exit cannot strand a request accepted concurrently with
+  cancellation.
+- Added the opt-in `ffi-test-hooks` feature exposing only the blocked-request
+  C stimulus, plus Rust shutdown contracts and C++ shutdown-drain and
+  deep-copy `UiSnapshot` contracts.
+- Corrected message input routing so `send_user_message` accepts message UTF-8
+  (including embedded NUL bytes) while path-like APIs reject embedded NULs.
+
+## TDD Evidence
+
+### RED
+
+- The legacy C++ ABI layout test failed against the old 24-byte frame once the
+  expanded session/completion frame contract was asserted.
+- Direct Rust bridge-contract compilation against the pre-Task-3 archive
+  failed because the async request/completion symbols and model types were
+  absent.
+- A temporary drain implementation that mutated/cancelled the wrong lifecycle
+  path failed the named C++ shutdown-drain assertion.
+- A neutral snapshot copier failed the named ownership and null/length
+  validation assertions.
+- The final input regression was reproduced when `send_user_message` was
+  accidentally routed through the path validator; the corrected focused path
+  NUL contract now passes.
+
+### GREEN
+
+The Task 3 modules were compiled directly with `rustc -D warnings` against a
+shape-compatible `codeg_lib`/`EuiBootstrap` boundary, then exercised without
+the memory-heavy shared crate:
+
+- Internal ABI/model/runtime tests: **6 passed, 0 failed**.
+- `bridge_contract`: **6 passed, 0 failed** (layout, lifecycle/thread,
+  invalid input, path NUL rejection, 256-request admission, immutable frame
+  retention/completion transfer).
+- `shutdown_contract`: **1 passed, 0 failed**.
+- ABI smoke: **1 passed, 0 failed**.
+
+## Verification
+
+Passed:
+
+- Direct Rust compilation with `-D warnings` for rlib and staticlib outputs.
+- C11 header syntax check with `-Wall -Wextra -Wpedantic -Werror`.
+- Contracts-only CMake build and CTest: **3/3 passed** (harness, ABI layout,
+  UI snapshot).
+- ABI-linked CMake build using the `ffi-test-hooks` archive and CTest:
+  **4/4 passed**, including `codeg_eui_shutdown_drain`.
+- Exact CTest registration checks for all four Task 3 targets.
+- Normal static archive exports no `codeg_eui_test_*` symbols; the
+  `ffi-test-hooks` archive exports exactly `codeg_eui_test_enqueue_blocked`.
+- `cargo fmt --manifest-path src-tauri/codeg-eui-core/Cargo.toml -- --check`.
+- `cargo fmt --manifest-path src-tauri/Cargo.toml -- --check`.
+- `git diff --check`.
+
+Per the parent instruction, **all full Cargo tests were skipped for this Task
+and the remaining spike work**. No broad package/workspace test command was
+run. A focused dependency-complete Cargo check was attempted with the
+repository low-memory configuration; the existing shared `codeg` rustc was
+killed by the 4 GiB/no-swap host with `SIGKILL` before the EUI target could
+compile. This is an authorized host limitation, not an open Task 3 defect.
+
+## Files Changed
+
+- `src-tauri/codeg-eui-core/Cargo.toml`
+- `src-tauri/codeg-eui-core/src/{abi,commands,model,runtime}.rs`
+- `src-tauri/codeg-eui-core/src/{bootstrap,data_root,lib}.rs`
+- `src-tauri/codeg-eui-core/tests/{abi_smoke,bridge_contract,data_root_isolation,shutdown_contract}.rs`
+- `codeg-eui/CMakeLists.txt`
+- `codeg-eui/app/bridge/{codeg_eui_bridge.h,ui_snapshot.h}`
+- `codeg-eui/tests/{abi_layout_test,ui_snapshot_test,shutdown_drain_test}.cpp`
+
+## Self-Review
+
+- Rust/C field order, sizes, alignment, offsets, discriminants, and exported
+  lifecycle declarations match ABI v1.
+- All public operations validate UI affinity, lifecycle state, output/input
+  pointers, UTF-8, frozen bounds, and path NUL policy before acceptance.
+- Raw frame pointers reference only backing vectors owned by the retained
+  `OwnedFrame`; empty slices/arrays use null pointers with zero lengths.
+- Completion reservation and terminalization are guarded against duplicate
+  IDs, and shutdown readiness is latched only after the successful frame copy.
+- Generated Cargo/CMake outputs and temporary archives remain ignored and are
+  excluded from the implementation package.
+
+<!-- codeg-card-summary-v1
+{"kind":"implementation","phase":"implementation","status":"done_with_concerns","summary":"Implemented EUI ABI v1 async lifecycle, bounded request/completion ledger, immutable frames, shutdown drain, and C++ deep-copy boundary.","commits":[{"subject":"feat(eui): implement async bridge lifecycle"}],"tests":{"status":"pass","passed":14,"failed":0,"summary":"14 focused Rust tests plus 3 contracts-only and 4 ABI-linked CTest cases pass; full Cargo tests skipped by parent instruction and shared-core Cargo check SIGKILLed on the 4GiB/no-swap host."},"concerns":["Dependency-complete Cargo verification requires a host with more memory or usable swap; the parent explicitly authorized skipping full Cargo tests for this spike."],"report_file":".superpowers/sdd/2026-08-09-eui-neo-frontend-spike/task-3-report.md"}
+-->
diff --git a/codeg-eui/CMakeLists.txt b/codeg-eui/CMakeLists.txt
index 8e41471a..25511a8f 100644
--- a/codeg-eui/CMakeLists.txt
+++ b/codeg-eui/CMakeLists.txt
@@ -18,6 +18,7 @@ endfunction()
 
 codeg_eui_add_contract_test(codeg_eui_harness_self tests/harness_self_test.cpp)
 codeg_eui_add_contract_test(codeg_eui_abi_layout tests/abi_layout_test.cpp)
+codeg_eui_add_contract_test(codeg_eui_ui_snapshot tests/ui_snapshot_test.cpp)
 
 if(CODEG_EUI_ABI_LINK_TESTS OR NOT CODEG_EUI_CONTRACTS_ONLY)
   if(NOT IS_ABSOLUTE "${CODEG_EUI_RUST_LIB}" OR
diff --git a/codeg-eui/app/bridge/codeg_eui_bridge.h b/codeg-eui/app/bridge/codeg_eui_bridge.h
index 9359d8e8..3e93320b 100644
--- a/codeg-eui/app/bridge/codeg_eui_bridge.h
+++ b/codeg-eui/app/bridge/codeg_eui_bridge.h
@@ -7,14 +7,88 @@
 #define CODEG_EUI_OK 0
 #define CODEG_EUI_ERR_INVALID_STATE 1
 #define CODEG_EUI_ERR_NULL_POINTER 2
+#define CODEG_EUI_ERR_INVALID_UTF8 3
+#define CODEG_EUI_ERR_TOO_LARGE 4
+#define CODEG_EUI_ERR_QUEUE_FULL 5
+#define CODEG_EUI_ERR_WRONG_THREAD 6
+#define CODEG_EUI_ERR_PANIC 7
+#define CODEG_EUI_ERR_INTERNAL 8
 #define CODEG_EUI_ERR_NOT_READY 9
 
+#define CODEG_EUI_MAX_PATH_BYTES 32768u
+#define CODEG_EUI_MAX_MESSAGE_BYTES 1048576u
+#define CODEG_EUI_MAX_SETTINGS_JSON_BYTES 2097152u
+#define CODEG_EUI_COMMAND_QUEUE_CAPACITY 256u
+#define CODEG_EUI_COMPLETION_CAPACITY 256u
+
+typedef enum CodegEuiLifecycleState {
+    CODEG_EUI_LIFECYCLE_UNINITIALIZED = 0,
+    CODEG_EUI_LIFECYCLE_STARTING = 1,
+    CODEG_EUI_LIFECYCLE_RUNNING = 2,
+    CODEG_EUI_LIFECYCLE_STOPPING = 3,
+    CODEG_EUI_LIFECYCLE_STOPPED = 4,
+} CodegEuiLifecycleState;
+
+typedef enum CodegEuiOperation {
+    CODEG_EUI_OP_SET_WORKSPACE = 1,
+    CODEG_EUI_OP_CREATE_SESSION = 2,
+    CODEG_EUI_OP_SELECT_SESSION = 3,
+    CODEG_EUI_OP_SEND_USER_MESSAGE = 4,
+    CODEG_EUI_OP_CANCEL_ACTIVE_TURN = 5,
+    CODEG_EUI_OP_GET_AGENT_SETTINGS = 6,
+    CODEG_EUI_OP_SET_AGENT_SETTINGS = 7,
+    CODEG_EUI_OP_PROBE_AGENT = 8,
+} CodegEuiOperation;
+
+typedef enum CodegEuiCompletionStatus {
+    CODEG_EUI_COMPLETION_OK = 0,
+    CODEG_EUI_COMPLETION_ERROR = 1,
+    CODEG_EUI_COMPLETION_STALE = 2,
+    CODEG_EUI_COMPLETION_CANCELLED = 3,
+} CodegEuiCompletionStatus;
+
+typedef struct CodegEuiSlice {
+    const uint8_t* ptr;
+    size_t len;
+} CodegEuiSlice;
+
+typedef struct CodegEuiSessionSummary {
+    int32_t conversation_id;
+    uint32_t reserved;
+    CodegEuiSlice title;
+    CodegEuiSlice agent;
+    int64_t updated_at_ms;
+} CodegEuiSessionSummary;
+
+typedef struct CodegEuiCompletion {
+    uint64_t request_id;
+    uint32_t op;
+    uint32_t status;
+    CodegEuiSlice result_payload;
+    CodegEuiSlice error;
+} CodegEuiCompletion;
+
 typedef struct CodegEuiFrame {
     uint32_t api_version;
     uint32_t lifecycle_state;
     uint64_t generation;
+    uint64_t selection_epoch;
+    const CodegEuiSessionSummary* sessions;
+    size_t sessions_len;
+    CodegEuiSlice connection_id;
+    uint64_t event_seq;
+    CodegEuiSlice transcript_json;
+    CodegEuiSlice live_assistant;
+    uint8_t stream_active;
+    uint8_t needs_resync;
     uint8_t shutdown_ready;
-    uint8_t reserved[7];
+    uint8_t reserved[5];
+    CodegEuiSlice error_strip;
+    const CodegEuiCompletion* completions;
+    size_t completions_len;
+    uint64_t t0_ns;
+    uint64_t t_first_token_ns;
+    uint64_t t_end_ns;
 } CodegEuiFrame;
 
 #if defined(__cplusplus)
@@ -26,9 +100,74 @@ int codeg_eui_init(const uint8_t* data_dir_utf8, size_t data_dir_len);
 int codeg_eui_poll(CodegEuiFrame* out);
 int codeg_eui_begin_shutdown(void);
 int codeg_eui_shutdown(void);
+int codeg_eui_set_workspace(const uint8_t* path_utf8,
+                            size_t path_len,
+                            uint64_t* out_request_id);
+int codeg_eui_create_session(const uint8_t* agent_utf8,
+                             size_t agent_len,
+                             uint64_t* out_request_id);
+int codeg_eui_select_session(int32_t conversation_id,
+                             uint64_t* out_request_id);
+int codeg_eui_send_user_message(const uint8_t* text_utf8,
+                                size_t text_len,
+                                uint64_t* out_request_id);
+int codeg_eui_cancel_active_turn(uint64_t* out_request_id);
+int codeg_eui_get_agent_settings(const uint8_t* agent_utf8,
+                                 size_t agent_len,
+                                 uint64_t* out_request_id);
+int codeg_eui_set_agent_settings(const uint8_t* agent_utf8,
+                                 size_t agent_len,
+                                 const uint8_t* json_utf8,
+                                 size_t json_len,
+                                 uint64_t* out_request_id);
+int codeg_eui_probe_agent(const uint8_t* agent_utf8,
+                          size_t agent_len,
+                          uint64_t* out_request_id);
+
+#if defined(CODEG_EUI_TEST_HOOKS)
+int codeg_eui_test_enqueue_blocked(uint64_t* out_request_id);
+#endif
 
 #if defined(__cplusplus)
 }
 
-static_assert(sizeof(CodegEuiFrame) == 24, "CodegEuiFrame ABI drift");
+static_assert(sizeof(CodegEuiLifecycleState) == 4,
+              "CodegEuiLifecycleState ABI drift");
+static_assert(sizeof(CodegEuiOperation) == 4,
+              "CodegEuiOperation ABI drift");
+static_assert(sizeof(CodegEuiCompletionStatus) == 4,
+              "CodegEuiCompletionStatus ABI drift");
+static_assert(sizeof(CodegEuiSlice) == 16, "CodegEuiSlice ABI drift");
+static_assert(alignof(CodegEuiSlice) == 8, "CodegEuiSlice alignment drift");
+static_assert(sizeof(CodegEuiSessionSummary) == 48,
+              "CodegEuiSessionSummary ABI drift");
+static_assert(alignof(CodegEuiSessionSummary) == 8,
+              "CodegEuiSessionSummary alignment drift");
+static_assert(sizeof(CodegEuiCompletion) == 48,
+              "CodegEuiCompletion ABI drift");
+static_assert(alignof(CodegEuiCompletion) == 8,
+              "CodegEuiCompletion alignment drift");
+static_assert(sizeof(CodegEuiFrame) == 160, "CodegEuiFrame ABI drift");
+static_assert(alignof(CodegEuiFrame) == 8, "CodegEuiFrame alignment drift");
+#elif defined(__STDC_VERSION__) && __STDC_VERSION__ >= 201112L
+_Static_assert(sizeof(CodegEuiLifecycleState) == 4,
+               "CodegEuiLifecycleState ABI drift");
+_Static_assert(sizeof(CodegEuiOperation) == 4,
+               "CodegEuiOperation ABI drift");
+_Static_assert(sizeof(CodegEuiCompletionStatus) == 4,
+               "CodegEuiCompletionStatus ABI drift");
+_Static_assert(sizeof(CodegEuiSlice) == 16, "CodegEuiSlice ABI drift");
+_Static_assert(_Alignof(CodegEuiSlice) == 8,
+               "CodegEuiSlice alignment drift");
+_Static_assert(sizeof(CodegEuiSessionSummary) == 48,
+               "CodegEuiSessionSummary ABI drift");
+_Static_assert(_Alignof(CodegEuiSessionSummary) == 8,
+               "CodegEuiSessionSummary alignment drift");
+_Static_assert(sizeof(CodegEuiCompletion) == 48,
+               "CodegEuiCompletion ABI drift");
+_Static_assert(_Alignof(CodegEuiCompletion) == 8,
+               "CodegEuiCompletion alignment drift");
+_Static_assert(sizeof(CodegEuiFrame) == 160, "CodegEuiFrame ABI drift");
+_Static_assert(_Alignof(CodegEuiFrame) == 8,
+               "CodegEuiFrame alignment drift");
 #endif
diff --git a/codeg-eui/app/bridge/ui_snapshot.h b/codeg-eui/app/bridge/ui_snapshot.h
new file mode 100644
index 00000000..922a20e4
--- /dev/null
+++ b/codeg-eui/app/bridge/ui_snapshot.h
@@ -0,0 +1,104 @@
+#pragma once
+
+#include "codeg_eui_bridge.h"
+
+#include <cstddef>
+#include <cstdint>
+#include <stdexcept>
+#include <string>
+#include <vector>
+
+struct UiSessionSummary {
+    std::int32_t conversationId{};
+    std::string title;
+    std::string agent;
+    std::int64_t updatedAtMs{};
+};
+
+struct UiCompletion {
+    std::uint64_t requestId{};
+    std::uint32_t op{};
+    std::uint32_t status{};
+    std::string resultPayload;
+    std::string error;
+};
+
+struct UiSnapshot {
+    std::uint32_t apiVersion{};
+    std::uint32_t lifecycleState{};
+    std::uint64_t generation{};
+    std::uint64_t selectionEpoch{};
+    std::vector<UiSessionSummary> sessions;
+    std::string connectionId;
+    std::uint64_t eventSeq{};
+    std::string transcriptJson;
+    std::string liveAssistant;
+    bool streamActive{};
+    bool needsResync{};
+    bool shutdownReady{};
+    std::string errorStrip;
+    std::vector<UiCompletion> completions;
+    std::uint64_t t0Ns{};
+    std::uint64_t tFirstTokenNs{};
+    std::uint64_t tEndNs{};
+};
+
+inline std::string copy_slice(CodegEuiSlice slice, const char* field) {
+    if (slice.len == 0) {
+        return {};
+    }
+    if (slice.ptr == nullptr) {
+        throw std::invalid_argument(std::string(field) + " has null data");
+    }
+    return {reinterpret_cast<const char*>(slice.ptr), slice.len};
+}
+
+inline UiSnapshot copy_frame(const CodegEuiFrame& frame) {
+    if (frame.sessions_len > 0 && frame.sessions == nullptr) {
+        throw std::invalid_argument("sessions has null data");
+    }
+    if (frame.completions_len > 0 && frame.completions == nullptr) {
+        throw std::invalid_argument("completions has null data");
+    }
+
+    UiSnapshot snapshot;
+    snapshot.apiVersion = frame.api_version;
+    snapshot.lifecycleState = frame.lifecycle_state;
+    snapshot.generation = frame.generation;
+    snapshot.selectionEpoch = frame.selection_epoch;
+    snapshot.sessions.reserve(frame.sessions_len);
+    for (std::size_t index = 0; index < frame.sessions_len; ++index) {
+        const CodegEuiSessionSummary& session = frame.sessions[index];
+        snapshot.sessions.push_back({
+            session.conversation_id,
+            copy_slice(session.title, "session.title"),
+            copy_slice(session.agent, "session.agent"),
+            session.updated_at_ms,
+        });
+    }
+    snapshot.connectionId = copy_slice(frame.connection_id, "connection_id");
+    snapshot.eventSeq = frame.event_seq;
+    snapshot.transcriptJson =
+        copy_slice(frame.transcript_json, "transcript_json");
+    snapshot.liveAssistant =
+        copy_slice(frame.live_assistant, "live_assistant");
+    snapshot.streamActive = frame.stream_active != 0;
+    snapshot.needsResync = frame.needs_resync != 0;
+    snapshot.shutdownReady = frame.shutdown_ready != 0;
+    snapshot.errorStrip = copy_slice(frame.error_strip, "error_strip");
+    snapshot.completions.reserve(frame.completions_len);
+    for (std::size_t index = 0; index < frame.completions_len; ++index) {
+        const CodegEuiCompletion& completion = frame.completions[index];
+        snapshot.completions.push_back({
+            completion.request_id,
+            completion.op,
+            completion.status,
+            copy_slice(completion.result_payload, "completion.result_payload"),
+            copy_slice(completion.error, "completion.error"),
+        });
+    }
+    snapshot.t0Ns = frame.t0_ns;
+    snapshot.tFirstTokenNs = frame.t_first_token_ns;
+    snapshot.tEndNs = frame.t_end_ns;
+    return snapshot;
+}
diff --git a/codeg-eui/tests/abi_layout_test.cpp b/codeg-eui/tests/abi_layout_test.cpp
index 1f447f48..bd12a237 100644
--- a/codeg-eui/tests/abi_layout_test.cpp
+++ b/codeg-eui/tests/abi_layout_test.cpp
@@ -3,17 +3,105 @@
 
 #include <cstddef>
 
-static_assert(sizeof(CodegEuiFrame) == 24, "CodegEuiFrame ABI size drift");
+static_assert(sizeof(CodegEuiLifecycleState) == 4,
+              "CodegEuiLifecycleState ABI size drift");
+static_assert(sizeof(CodegEuiOperation) == 4,
+              "CodegEuiOperation ABI size drift");
+static_assert(sizeof(CodegEuiCompletionStatus) == 4,
+              "CodegEuiCompletionStatus ABI size drift");
+static_assert(sizeof(CodegEuiSlice) == 16, "CodegEuiSlice ABI size drift");
+static_assert(alignof(CodegEuiSlice) == 8,
+              "CodegEuiSlice ABI alignment drift");
+static_assert(offsetof(CodegEuiSlice, len) == 8,
+              "CodegEuiSlice len offset drift");
+static_assert(sizeof(CodegEuiSessionSummary) == 48,
+              "CodegEuiSessionSummary ABI size drift");
+static_assert(alignof(CodegEuiSessionSummary) == 8,
+              "CodegEuiSessionSummary ABI alignment drift");
+static_assert(offsetof(CodegEuiSessionSummary, conversation_id) == 0,
+              "CodegEuiSessionSummary id offset drift");
+static_assert(offsetof(CodegEuiSessionSummary, reserved) == 4,
+              "CodegEuiSessionSummary reserved offset drift");
+static_assert(offsetof(CodegEuiSessionSummary, title) == 8,
+              "CodegEuiSessionSummary title offset drift");
+static_assert(offsetof(CodegEuiSessionSummary, agent) == 24,
+              "CodegEuiSessionSummary agent offset drift");
+static_assert(offsetof(CodegEuiSessionSummary, updated_at_ms) == 40,
+              "CodegEuiSessionSummary updated_at offset drift");
+static_assert(sizeof(CodegEuiCompletion) == 48,
+              "CodegEuiCompletion ABI size drift");
+static_assert(alignof(CodegEuiCompletion) == 8,
+              "CodegEuiCompletion ABI alignment drift");
+static_assert(offsetof(CodegEuiCompletion, request_id) == 0,
+              "CodegEuiCompletion request offset drift");
+static_assert(offsetof(CodegEuiCompletion, op) == 8,
+              "CodegEuiCompletion op offset drift");
+static_assert(offsetof(CodegEuiCompletion, status) == 12,
+              "CodegEuiCompletion status offset drift");
+static_assert(offsetof(CodegEuiCompletion, result_payload) == 16,
+              "CodegEuiCompletion result offset drift");
+static_assert(offsetof(CodegEuiCompletion, error) == 32,
+              "CodegEuiCompletion error offset drift");
+static_assert(sizeof(CodegEuiFrame) == 160, "CodegEuiFrame ABI size drift");
 static_assert(alignof(CodegEuiFrame) == 8, "CodegEuiFrame ABI alignment drift");
+static_assert(offsetof(CodegEuiFrame, api_version) == 0,
+              "CodegEuiFrame version offset drift");
+static_assert(offsetof(CodegEuiFrame, lifecycle_state) == 4,
+              "CodegEuiFrame lifecycle offset drift");
 static_assert(offsetof(CodegEuiFrame, generation) == 8,
               "CodegEuiFrame generation offset drift");
-static_assert(offsetof(CodegEuiFrame, shutdown_ready) == 16,
+static_assert(offsetof(CodegEuiFrame, selection_epoch) == 16,
+              "CodegEuiFrame selection_epoch offset drift");
+static_assert(offsetof(CodegEuiFrame, sessions) == 24,
+              "CodegEuiFrame sessions offset drift");
+static_assert(offsetof(CodegEuiFrame, sessions_len) == 32,
+              "CodegEuiFrame sessions length offset drift");
+static_assert(offsetof(CodegEuiFrame, connection_id) == 40,
+              "CodegEuiFrame connection_id offset drift");
+static_assert(offsetof(CodegEuiFrame, event_seq) == 56,
+              "CodegEuiFrame event sequence offset drift");
+static_assert(offsetof(CodegEuiFrame, transcript_json) == 64,
+              "CodegEuiFrame transcript_json offset drift");
+static_assert(offsetof(CodegEuiFrame, live_assistant) == 80,
+              "CodegEuiFrame live_assistant offset drift");
+static_assert(offsetof(CodegEuiFrame, stream_active) == 96,
+              "CodegEuiFrame stream flag offset drift");
+static_assert(offsetof(CodegEuiFrame, needs_resync) == 97,
+              "CodegEuiFrame resync flag offset drift");
+static_assert(offsetof(CodegEuiFrame, shutdown_ready) == 98,
               "CodegEuiFrame shutdown_ready offset drift");
+static_assert(offsetof(CodegEuiFrame, reserved) == 99,
+              "CodegEuiFrame reserved offset drift");
+static_assert(offsetof(CodegEuiFrame, error_strip) == 104,
+              "CodegEuiFrame error_strip offset drift");
+static_assert(offsetof(CodegEuiFrame, completions) == 120,
+              "CodegEuiFrame completions offset drift");
+static_assert(offsetof(CodegEuiFrame, completions_len) == 128,
+              "CodegEuiFrame completions length offset drift");
+static_assert(offsetof(CodegEuiFrame, t0_ns) == 136,
+              "CodegEuiFrame t0 offset drift");
+static_assert(offsetof(CodegEuiFrame, t_first_token_ns) == 144,
+              "CodegEuiFrame first token offset drift");
+static_assert(offsetof(CodegEuiFrame, t_end_ns) == 152,
+              "CodegEuiFrame end offset drift");
 
 TEST(AbiLayout, matches_v1_size_alignment_and_offsets) {
     EXPECT_EQ(CODEG_EUI_API_VERSION, 1u);
-    EXPECT_EQ(sizeof(CodegEuiFrame), static_cast<std::size_t>(24));
+    EXPECT_EQ(CODEG_EUI_OK, 0);
+    EXPECT_EQ(CODEG_EUI_ERR_INVALID_STATE, 1);
+    EXPECT_EQ(CODEG_EUI_ERR_NULL_POINTER, 2);
+    EXPECT_EQ(CODEG_EUI_ERR_INVALID_UTF8, 3);
+    EXPECT_EQ(CODEG_EUI_ERR_TOO_LARGE, 4);
+    EXPECT_EQ(CODEG_EUI_ERR_QUEUE_FULL, 5);
+    EXPECT_EQ(CODEG_EUI_ERR_WRONG_THREAD, 6);
+    EXPECT_EQ(CODEG_EUI_ERR_PANIC, 7);
+    EXPECT_EQ(CODEG_EUI_ERR_INTERNAL, 8);
+    EXPECT_EQ(CODEG_EUI_ERR_NOT_READY, 9);
+    EXPECT_EQ(CODEG_EUI_LIFECYCLE_STOPPED, 4);
+    EXPECT_EQ(CODEG_EUI_OP_PROBE_AGENT, 8);
+    EXPECT_EQ(CODEG_EUI_COMPLETION_CANCELLED, 3);
+    EXPECT_EQ(sizeof(CodegEuiFrame), static_cast<std::size_t>(160));
     EXPECT_EQ(offsetof(CodegEuiFrame, generation), static_cast<std::size_t>(8));
     EXPECT_EQ(offsetof(CodegEuiFrame, shutdown_ready),
-              static_cast<std::size_t>(16));
+              static_cast<std::size_t>(98));
 }
diff --git a/codeg-eui/tests/shutdown_drain_test.cpp b/codeg-eui/tests/shutdown_drain_test.cpp
new file mode 100644
index 00000000..8276ac13
--- /dev/null
+++ b/codeg-eui/tests/shutdown_drain_test.cpp
@@ -0,0 +1,82 @@
+#include "codeg_eui_bridge.h"
+#include "test_harness.h"
+
+#include <chrono>
+#include <cstdint>
+#include <filesystem>
+#include <string>
+#include <thread>
+#include <vector>
+
+#include <unistd.h>
+
+namespace {
+
+struct Completion {
+    std::uint64_t requestId;
+    std::uint32_t status;
+};
+
+void appendCopiedCompletions(const CodegEuiFrame& frame,
+                             std::vector<Completion>& target) {
+    if (frame.completions_len == 0) {
+        EXPECT_TRUE(frame.completions == nullptr);
+        return;
+    }
+    ASSERT_EQ(frame.completions == nullptr, false);
+    for (std::size_t index = 0; index < frame.completions_len; ++index) {
+        const CodegEuiCompletion& completion = frame.completions[index];
+        target.push_back({completion.request_id, completion.status});
+    }
+}
+
+std::size_t countCompletion(const std::vector<Completion>& values,
+                            std::uint64_t requestId,
+                            std::uint32_t status) {
+    std::size_t count = 0;
+    for (const Completion& completion : values) {
+        if (completion.requestId == requestId && completion.status == status) {
+            ++count;
+        }
+    }
+    return count;
+}
+
+}  // namespace
+
+TEST(ShutdownDrain, exposes_cancelled_completion_before_final_free) {
+    const std::filesystem::path root =
+        std::filesystem::temp_directory_path() /
+        ("codeg-eui-shutdown-drain-" + std::to_string(getpid()));
+    std::filesystem::remove_all(root);
+    const std::string rootString = root.string();
+
+    ASSERT_EQ(codeg_eui_init(
+                  reinterpret_cast<const std::uint8_t*>(rootString.data()),
+                  rootString.size()),
+              CODEG_EUI_OK);
+    std::uint64_t requestId = 0;
+    ASSERT_EQ(codeg_eui_test_enqueue_blocked(&requestId), CODEG_EUI_OK);
+    ASSERT_EQ(codeg_eui_begin_shutdown(), CODEG_EUI_OK);
+    ASSERT_EQ(codeg_eui_shutdown(), CODEG_EUI_ERR_NOT_READY);
+
+    std::vector<Completion> seen;
+    bool ready = false;
+    for (int attempt = 0; attempt < 200; ++attempt) {
+        CodegEuiFrame frame{};
+        ASSERT_EQ(codeg_eui_poll(&frame), CODEG_EUI_OK);
+        appendCopiedCompletions(frame, seen);
+        if (frame.shutdown_ready == 1) {
+            ready = true;
+            break;
+        }
+        std::this_thread::sleep_for(std::chrono::milliseconds(5));
+    }
+
+    EXPECT_TRUE(ready);
+    ASSERT_EQ(countCompletion(
+                  seen, requestId, CODEG_EUI_COMPLETION_CANCELLED),
+              static_cast<std::size_t>(1));
+    ASSERT_EQ(codeg_eui_shutdown(), CODEG_EUI_OK);
+    std::filesystem::remove_all(root);
+}
diff --git a/codeg-eui/tests/ui_snapshot_test.cpp b/codeg-eui/tests/ui_snapshot_test.cpp
new file mode 100644
index 00000000..a1bd3efa
--- /dev/null
+++ b/codeg-eui/tests/ui_snapshot_test.cpp
@@ -0,0 +1,87 @@
+#include "codeg_eui_bridge.h"
+#include "test_harness.h"
+#include "ui_snapshot.h"
+
+#include <stdexcept>
+#include <string>
+
+TEST(UiSnapshot, owns_frame_a_after_frame_b_and_shutdown) {
+    std::string rustBacking = "frame-a";
+    std::string sessionTitle = "Session A";
+    std::string sessionAgent = "codex";
+    std::string completionResult = "result-a";
+    std::string completionError = "error-a";
+    CodegEuiSessionSummary session{};
+    session.conversation_id = 17;
+    session.title = {
+        reinterpret_cast<const std::uint8_t*>(sessionTitle.data()),
+        sessionTitle.size(),
+    };
+    session.agent = {
+        reinterpret_cast<const std::uint8_t*>(sessionAgent.data()),
+        sessionAgent.size(),
+    };
+    session.updated_at_ms = 42;
+    CodegEuiCompletion completion{};
+    completion.request_id = 23;
+    completion.op = CODEG_EUI_OP_SEND_USER_MESSAGE;
+    completion.status = CODEG_EUI_COMPLETION_ERROR;
+    completion.result_payload = {
+        reinterpret_cast<const std::uint8_t*>(completionResult.data()),
+        completionResult.size(),
+    };
+    completion.error = {
+        reinterpret_cast<const std::uint8_t*>(completionError.data()),
+        completionError.size(),
+    };
+    CodegEuiFrame frameA{};
+    frameA.sessions = &session;
+    frameA.sessions_len = 1;
+    frameA.live_assistant = {
+        reinterpret_cast<const std::uint8_t*>(rustBacking.data()),
+        rustBacking.size(),
+    };
+    frameA.completions = &completion;
+    frameA.completions_len = 1;
+
+    const UiSnapshot copied = copy_frame(frameA);
+    rustBacking = "frame-b";
+    rustBacking.clear();
+    sessionTitle.clear();
+    sessionAgent.clear();
+    completionResult.clear();
+    completionError.clear();
+
+    EXPECT_EQ(copied.liveAssistant, std::string("frame-a"));
+    ASSERT_EQ(copied.sessions.size(), static_cast<std::size_t>(1));
+    EXPECT_EQ(copied.sessions[0].conversationId, 17);
+    EXPECT_EQ(copied.sessions[0].title, std::string("Session A"));
+    EXPECT_EQ(copied.sessions[0].agent, std::string("codex"));
+    ASSERT_EQ(copied.completions.size(), static_cast<std::size_t>(1));
+    EXPECT_EQ(copied.completions[0].requestId, static_cast<std::uint64_t>(23));
+    EXPECT_EQ(copied.completions[0].resultPayload, std::string("result-a"));
+    EXPECT_EQ(copied.completions[0].error, std::string("error-a"));
+}
+
+TEST(UiSnapshot, validates_null_and_length_pairs) {
+    CodegEuiFrame invalid{};
+    invalid.live_assistant = {nullptr, 1};
+    bool rejected = false;
+    try {
+        (void)copy_frame(invalid);
+    } catch (const std::invalid_argument&) {
+        rejected = true;
+    }
+    EXPECT_TRUE(rejected);
+
+    CodegEuiFrame empty{};
+    empty.live_assistant = {nullptr, 0};
+    bool accepted = true;
+    try {
+        const UiSnapshot copied = copy_frame(empty);
+        accepted = copied.liveAssistant.empty();
+    } catch (...) {
+        accepted = false;
+    }
+    EXPECT_TRUE(accepted);
+}
diff --git a/src-tauri/codeg-eui-core/Cargo.toml b/src-tauri/codeg-eui-core/Cargo.toml
index ca67ed33..1469c384 100644
--- a/src-tauri/codeg-eui-core/Cargo.toml
+++ b/src-tauri/codeg-eui-core/Cargo.toml
@@ -4,6 +4,10 @@ version = "0.1.0"
 edition = "2021"
 publish = false
 
+[features]
+default = []
+ffi-test-hooks = []
+
 [lib]
 name = "codeg_eui_core"
 crate-type = ["staticlib", "rlib"]
@@ -12,7 +16,7 @@ crate-type = ["staticlib", "rlib"]
 codeg = { package = "codeg", path = "..", default-features = false }
 serde = { version = "1", features = ["derive"] }
 serde_json = "1"
-tokio = { version = "1", features = ["rt-multi-thread", "sync", "time"] }
+tokio = { version = "1", features = ["macros", "rt-multi-thread", "sync", "time"] }
 thiserror = "2"
 
 [dev-dependencies]
diff --git a/src-tauri/codeg-eui-core/src/abi.rs b/src-tauri/codeg-eui-core/src/abi.rs
index 66cc050d..9b9d8794 100644
--- a/src-tauri/codeg-eui-core/src/abi.rs
+++ b/src-tauri/codeg-eui-core/src/abi.rs
@@ -1,40 +1,156 @@
 use std::panic::{catch_unwind, AssertUnwindSafe};
 use std::path::PathBuf;
-use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
-use std::sync::{Mutex, OnceLock};
+use std::sync::{Mutex, MutexGuard, OnceLock};
 
-use crate::EuiBootstrap;
+use crate::commands::{enqueue, CommandPayload, Operation};
+use crate::model::{CodegEuiCompletion, CodegEuiSessionSummary, CodegEuiSlice, OwnedFrame};
+use crate::runtime::RuntimeOwner;
+use crate::{BootstrapError, DataRootError, EuiBootstrap, SharedModel};
 
 pub const CODEG_EUI_API_VERSION: u32 = 1;
 pub const CODEG_EUI_OK: i32 = 0;
 pub const CODEG_EUI_ERR_INVALID_STATE: i32 = 1;
 pub const CODEG_EUI_ERR_NULL_POINTER: i32 = 2;
+pub const CODEG_EUI_ERR_INVALID_UTF8: i32 = 3;
+pub const CODEG_EUI_ERR_TOO_LARGE: i32 = 4;
+pub const CODEG_EUI_ERR_QUEUE_FULL: i32 = 5;
+pub const CODEG_EUI_ERR_WRONG_THREAD: i32 = 6;
+pub const CODEG_EUI_ERR_PANIC: i32 = 7;
+pub const CODEG_EUI_ERR_INTERNAL: i32 = 8;
 pub const CODEG_EUI_ERR_NOT_READY: i32 = 9;
 
-const LIFECYCLE_UNINITIALIZED: u32 = 0;
-const LIFECYCLE_STARTING: u32 = 1;
-const LIFECYCLE_RUNNING: u32 = 2;
-const LIFECYCLE_STOPPING: u32 = 3;
-const LIFECYCLE_STOPPED: u32 = 4;
-const CODEG_EUI_MAX_PATH_BYTES: usize = 32_768;
+pub const CODEG_EUI_MAX_PATH_BYTES: usize = 32_768;
+pub const CODEG_EUI_MAX_MESSAGE_BYTES: usize = 1_048_576;
+pub const CODEG_EUI_MAX_SETTINGS_JSON_BYTES: usize = 2_097_152;
+pub const CODEG_EUI_COMMAND_QUEUE_CAPACITY: usize = 256;
+pub const CODEG_EUI_COMPLETION_CAPACITY: usize = 256;
 
-static LIFECYCLE: AtomicU32 = AtomicU32::new(LIFECYCLE_UNINITIALIZED);
-static GENERATION: AtomicU64 = AtomicU64::new(0);
-static SHUTDOWN_READY: AtomicBool = AtomicBool::new(false);
-static BOOTSTRAP: OnceLock<Mutex<Option<EuiBootstrap>>> = OnceLock::new();
+#[repr(u32)]
+#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
+pub enum LifecycleState {
+    #[default]
+    Uninitialized = 0,
+    Starting = 1,
+    Running = 2,
+    Stopping = 3,
+    Stopped = 4,
+}
 
 #[repr(C)]
-#[derive(Clone, Copy, Default)]
+#[derive(Clone, Copy)]
 pub struct CodegEuiFrame {
     pub api_version: u32,
     pub lifecycle_state: u32,
     pub generation: u64,
+    pub selection_epoch: u64,
+    pub sessions: *const CodegEuiSessionSummary,
+    pub sessions_len: usize,
+    pub connection_id: CodegEuiSlice,
+    pub event_seq: u64,
+    pub transcript_json: CodegEuiSlice,
+    pub live_assistant: CodegEuiSlice,
+    pub stream_active: u8,
+    pub needs_resync: u8,
     pub shutdown_ready: u8,
-    pub _reserved: [u8; 7],
+    pub _reserved: [u8; 5],
+    pub error_strip: CodegEuiSlice,
+    pub completions: *const CodegEuiCompletion,
+    pub completions_len: usize,
+    pub t0_ns: u64,
+    pub t_first_token_ns: u64,
+    pub t_end_ns: u64,
+}
+
+impl Default for CodegEuiFrame {
+    fn default() -> Self {
+        Self {
+            api_version: 0,
+            lifecycle_state: LifecycleState::Uninitialized as u32,
+            generation: 0,
+            selection_epoch: 0,
+            sessions: std::ptr::null(),
+            sessions_len: 0,
+            connection_id: CodegEuiSlice::default(),
+            event_seq: 0,
+            transcript_json: CodegEuiSlice::default(),
+            live_assistant: CodegEuiSlice::default(),
+            stream_active: 0,
+            needs_resync: 0,
+            shutdown_ready: 0,
+            _reserved: [0; 5],
+            error_strip: CodegEuiSlice::default(),
+            completions: std::ptr::null(),
+            completions_len: 0,
+            t0_ns: 0,
+            t_first_token_ns: 0,
+            t_end_ns: 0,
+        }
+    }
+}
+
+struct BridgeSlot {
+    lifecycle: LifecycleState,
+    ui_thread: Option<std::thread::ThreadId>,
+    runtime: Option<RuntimeOwner>,
+    model: SharedModel,
+    last_frame: Option<OwnedFrame>,
+    generation: u64,
+    shutdown_ready_observed: bool,
+}
+
+impl Default for BridgeSlot {
+    fn default() -> Self {
+        Self {
+            lifecycle: LifecycleState::Uninitialized,
+            ui_thread: None,
+            runtime: None,
+            model: SharedModel::new(),
+            last_frame: None,
+            generation: 0,
+            shutdown_ready_observed: false,
+        }
+    }
+}
+
+static BRIDGE: OnceLock<Mutex<BridgeSlot>> = OnceLock::new();
+
+fn bridge() -> &'static Mutex<BridgeSlot> {
+    BRIDGE.get_or_init(|| Mutex::new(BridgeSlot::default()))
+}
+
+fn lock_bridge() -> MutexGuard<'static, BridgeSlot> {
+    bridge().lock().unwrap_or_else(|error| error.into_inner())
+}
+
+fn ffi_guard(body: impl FnOnce() -> i32) -> i32 {
+    match catch_unwind(AssertUnwindSafe(body)) {
+        Ok(code) => code,
+        Err(_) => {
+            let _ = catch_unwind(AssertUnwindSafe(|| {
+                record_panic_diagnostic("Rust panic contained at codeg-eui ABI");
+            }));
+            CODEG_EUI_ERR_PANIC
+        }
+    }
+}
+
+fn record_panic_diagnostic(message: &str) {
+    eprintln!("{message}");
+    lock_bridge()
+        .model
+        .set_error_strip(message.as_bytes().to_vec());
 }
 
-fn ffi_status(operation: impl FnOnce() -> i32) -> i32 {
-    catch_unwind(AssertUnwindSafe(operation)).unwrap_or(CODEG_EUI_ERR_INVALID_STATE)
+fn ensure_ui_thread(slot: &BridgeSlot) -> Result<(), i32> {
+    if slot
+        .ui_thread
+        .as_ref()
+        .is_some_and(|thread| *thread != std::thread::current().id())
+    {
+        Err(CODEG_EUI_ERR_WRONG_THREAD)
+    } else {
+        Ok(())
+    }
 }
 
 #[no_mangle]
@@ -44,66 +160,94 @@ pub extern "C" fn codeg_eui_api_version() -> u32 {
 
 #[no_mangle]
 pub extern "C" fn codeg_eui_init(data_dir_utf8: *const u8, data_dir_len: usize) -> i32 {
-    let status = ffi_status(|| {
-        if data_dir_utf8.is_null() && data_dir_len > 0 {
-            return CODEG_EUI_ERR_NULL_POINTER;
+    let status = ffi_guard(|| {
+        let mut slot = lock_bridge();
+        if let Err(error) = ensure_ui_thread(&slot) {
+            return error;
         }
-
-        let current = LIFECYCLE.load(Ordering::Acquire);
-        if current != LIFECYCLE_UNINITIALIZED && current != LIFECYCLE_STOPPED {
+        if !matches!(
+            slot.lifecycle,
+            LifecycleState::Uninitialized | LifecycleState::Stopped
+        ) {
             return CODEG_EUI_ERR_INVALID_STATE;
         }
 
-        LIFECYCLE.store(LIFECYCLE_STARTING, Ordering::Release);
-        GENERATION.store(0, Ordering::Release);
-        SHUTDOWN_READY.store(false, Ordering::Release);
+        slot.lifecycle = LifecycleState::Starting;
+        slot.generation = 0;
+        slot.shutdown_ready_observed = false;
+        slot.last_frame = None;
 
         let argument_root = match parse_data_root_argument(data_dir_utf8, data_dir_len) {
             Ok(argument_root) => argument_root,
-            Err(error) => return error,
+            Err(error) => {
+                slot.lifecycle = LifecycleState::Stopped;
+                return error;
+            }
         };
         let bootstrap = match EuiBootstrap::start_with_data_root_argument(argument_root) {
             Ok(bootstrap) => bootstrap,
-            Err(_) => return CODEG_EUI_ERR_INVALID_STATE,
+            Err(BootstrapError::DataRoot(DataRootError::AlreadyPinned { .. })) => {
+                slot.lifecycle = LifecycleState::Stopped;
+                return CODEG_EUI_ERR_INVALID_STATE;
+            }
+            Err(_) => {
+                slot.lifecycle = LifecycleState::Stopped;
+                return CODEG_EUI_ERR_INTERNAL;
+            }
         };
-        *bootstrap_slot()
-            .lock()
-            .unwrap_or_else(|error| error.into_inner()) = Some(bootstrap);
-        LIFECYCLE.store(LIFECYCLE_RUNNING, Ordering::Release);
+
+        let model = SharedModel::new();
+        slot.runtime = Some(RuntimeOwner::start(bootstrap, model.clone()));
+        slot.model = model;
+        slot.ui_thread = Some(std::thread::current().id());
+        slot.lifecycle = LifecycleState::Running;
         CODEG_EUI_OK
     });
-
-    if status != CODEG_EUI_OK && LIFECYCLE.load(Ordering::Acquire) == LIFECYCLE_STARTING {
-        LIFECYCLE.store(LIFECYCLE_STOPPED, Ordering::Release);
+    if status != CODEG_EUI_OK {
+        let mut slot = lock_bridge();
+        if slot.lifecycle == LifecycleState::Starting {
+            slot.runtime = None;
+            slot.last_frame = None;
+            slot.lifecycle = LifecycleState::Stopped;
+        }
     }
     status
 }
 
 #[no_mangle]
 pub extern "C" fn codeg_eui_poll(out: *mut CodegEuiFrame) -> i32 {
-    ffi_status(|| {
+    ffi_guard(|| {
+        let mut slot = lock_bridge();
+        if let Err(error) = ensure_ui_thread(&slot) {
+            return error;
+        }
         if out.is_null() {
             return CODEG_EUI_ERR_NULL_POINTER;
         }
-
-        let lifecycle_state = LIFECYCLE.load(Ordering::Acquire);
-        if lifecycle_state != LIFECYCLE_RUNNING && lifecycle_state != LIFECYCLE_STOPPING {
+        if !matches!(
+            slot.lifecycle,
+            LifecycleState::Running | LifecycleState::Stopping
+        ) {
             return CODEG_EUI_ERR_INVALID_STATE;
         }
 
-        let shutdown_ready = lifecycle_state == LIFECYCLE_STOPPING;
-        let frame = CodegEuiFrame {
-            api_version: CODEG_EUI_API_VERSION,
-            lifecycle_state,
-            generation: GENERATION.fetch_add(1, Ordering::AcqRel) + 1,
-            shutdown_ready: u8::from(shutdown_ready),
-            _reserved: [0; 7],
+        let generation = match slot.generation.checked_add(1) {
+            Some(generation) => generation,
+            None => return CODEG_EUI_ERR_INTERNAL,
+        };
+        let stopping = slot.lifecycle == LifecycleState::Stopping;
+        let quiesced = match slot.runtime.as_ref() {
+            Some(runtime) => runtime.quiesced_flag(),
+            None => return CODEG_EUI_ERR_INTERNAL,
         };
+        let (owned_frame, shutdown_ready) = slot.model.build_frame(stopping, &quiesced);
+        let frame = owned_frame.as_abi(slot.lifecycle, generation, shutdown_ready);
 
-        // The caller owns `out` and must provide writable storage for one frame.
+        slot.generation = generation;
+        slot.last_frame = Some(owned_frame);
         unsafe { out.write(frame) };
         if shutdown_ready {
-            SHUTDOWN_READY.store(true, Ordering::Release);
+            slot.shutdown_ready_observed = true;
         }
         CODEG_EUI_OK
     })
@@ -111,73 +255,331 @@ pub extern "C" fn codeg_eui_poll(out: *mut CodegEuiFrame) -> i32 {
 
 #[no_mangle]
 pub extern "C" fn codeg_eui_begin_shutdown() -> i32 {
-    ffi_status(|| {
-        if LIFECYCLE
-            .compare_exchange(
-                LIFECYCLE_RUNNING,
-                LIFECYCLE_STOPPING,
-                Ordering::AcqRel,
-                Ordering::Acquire,
-            )
-            .is_err()
-        {
+    ffi_guard(|| {
+        let mut slot = lock_bridge();
+        if let Err(error) = ensure_ui_thread(&slot) {
+            return error;
+        }
+        if slot.lifecycle != LifecycleState::Running {
             return CODEG_EUI_ERR_INVALID_STATE;
         }
-        SHUTDOWN_READY.store(false, Ordering::Release);
+
+        slot.lifecycle = LifecycleState::Stopping;
+        slot.shutdown_ready_observed = false;
+        match slot.runtime.as_mut() {
+            Some(runtime) => runtime.begin_shutdown(),
+            None => return CODEG_EUI_ERR_INTERNAL,
+        }
         CODEG_EUI_OK
     })
 }
 
 #[no_mangle]
 pub extern "C" fn codeg_eui_shutdown() -> i32 {
-    ffi_status(|| {
-        if LIFECYCLE.load(Ordering::Acquire) != LIFECYCLE_STOPPING
-            || !SHUTDOWN_READY.load(Ordering::Acquire)
-        {
+    ffi_guard(|| {
+        let mut slot = lock_bridge();
+        if let Err(error) = ensure_ui_thread(&slot) {
+            return error;
+        }
+        if slot.lifecycle != LifecycleState::Stopping {
             return CODEG_EUI_ERR_INVALID_STATE;
         }
+        if !slot.shutdown_ready_observed {
+            return CODEG_EUI_ERR_NOT_READY;
+        }
 
-        let bootstrap = bootstrap_slot()
-            .lock()
-            .unwrap_or_else(|error| error.into_inner())
-            .take()
-            .ok_or(CODEG_EUI_ERR_INVALID_STATE);
-        let bootstrap = match bootstrap {
-            Ok(bootstrap) => bootstrap,
+        let runtime = match slot.runtime.take() {
+            Some(runtime) => runtime,
+            None => return CODEG_EUI_ERR_INTERNAL,
+        };
+        runtime.join();
+        slot.last_frame = None;
+        slot.lifecycle = LifecycleState::Stopped;
+        slot.shutdown_ready_observed = false;
+        CODEG_EUI_OK
+    })
+}
+
+#[no_mangle]
+pub extern "C" fn codeg_eui_set_workspace(
+    path_utf8: *const u8,
+    path_len: usize,
+    out_request_id: *mut u64,
+) -> i32 {
+    enqueue_path(
+        path_utf8,
+        path_len,
+        CODEG_EUI_MAX_PATH_BYTES,
+        out_request_id,
+        Operation::SetWorkspace,
+    )
+}
+
+#[no_mangle]
+pub extern "C" fn codeg_eui_create_session(
+    agent_utf8: *const u8,
+    agent_len: usize,
+    out_request_id: *mut u64,
+) -> i32 {
+    enqueue_path(
+        agent_utf8,
+        agent_len,
+        CODEG_EUI_MAX_PATH_BYTES,
+        out_request_id,
+        Operation::CreateSession,
+    )
+}
+
+#[no_mangle]
+pub extern "C" fn codeg_eui_select_session(conversation_id: i32, out_request_id: *mut u64) -> i32 {
+    enqueue_payload(
+        out_request_id,
+        Operation::SelectSession,
+        CommandPayload::SelectSession(conversation_id),
+    )
+}
+
+#[no_mangle]
+pub extern "C" fn codeg_eui_send_user_message(
+    text_utf8: *const u8,
+    text_len: usize,
+    out_request_id: *mut u64,
+) -> i32 {
+    enqueue_utf8(
+        text_utf8,
+        text_len,
+        CODEG_EUI_MAX_MESSAGE_BYTES,
+        out_request_id,
+        Operation::SendUserMessage,
+    )
+}
+
+#[no_mangle]
+pub extern "C" fn codeg_eui_cancel_active_turn(out_request_id: *mut u64) -> i32 {
+    enqueue_payload(
+        out_request_id,
+        Operation::CancelActiveTurn,
+        CommandPayload::Empty,
+    )
+}
+
+#[no_mangle]
+pub extern "C" fn codeg_eui_get_agent_settings(
+    agent_utf8: *const u8,
+    agent_len: usize,
+    out_request_id: *mut u64,
+) -> i32 {
+    enqueue_path(
+        agent_utf8,
+        agent_len,
+        CODEG_EUI_MAX_PATH_BYTES,
+        out_request_id,
+        Operation::GetAgentSettings,
+    )
+}
+
+#[no_mangle]
+pub extern "C" fn codeg_eui_set_agent_settings(
+    agent_utf8: *const u8,
+    agent_len: usize,
+    json_utf8: *const u8,
+    json_len: usize,
+    out_request_id: *mut u64,
+) -> i32 {
+    ffi_guard(|| {
+        let mut slot = lock_bridge();
+        if let Err(error) = ensure_running(&slot) {
+            return error;
+        }
+        if out_request_id.is_null() {
+            return CODEG_EUI_ERR_NULL_POINTER;
+        }
+        let agent = match copy_utf8(agent_utf8, agent_len, CODEG_EUI_MAX_PATH_BYTES) {
+            Ok(agent) => agent,
             Err(error) => return error,
         };
-        bootstrap.shutdown();
+        if agent.contains(&0) {
+            return CODEG_EUI_ERR_INVALID_STATE;
+        }
+        let json = match copy_utf8(json_utf8, json_len, CODEG_EUI_MAX_SETTINGS_JSON_BYTES) {
+            Ok(json) => json,
+            Err(error) => return error,
+        };
+        accept_and_write(
+            &mut slot,
+            out_request_id,
+            Operation::SetAgentSettings,
+            CommandPayload::AgentSettings { agent, json },
+        )
+    })
+}
 
-        SHUTDOWN_READY.store(false, Ordering::Release);
-        LIFECYCLE.store(LIFECYCLE_STOPPED, Ordering::Release);
-        CODEG_EUI_OK
+#[no_mangle]
+pub extern "C" fn codeg_eui_probe_agent(
+    agent_utf8: *const u8,
+    agent_len: usize,
+    out_request_id: *mut u64,
+) -> i32 {
+    enqueue_path(
+        agent_utf8,
+        agent_len,
+        CODEG_EUI_MAX_PATH_BYTES,
+        out_request_id,
+        Operation::ProbeAgent,
+    )
+}
+
+#[doc(hidden)]
+pub fn enqueue_blocked_for_test() -> Result<u64, i32> {
+    let mut request_id = 0;
+    let code = enqueue_payload(
+        &mut request_id,
+        Operation::SendUserMessage,
+        CommandPayload::Blocked,
+    );
+    if code == CODEG_EUI_OK {
+        Ok(request_id)
+    } else {
+        Err(code)
+    }
+}
+
+#[cfg(feature = "ffi-test-hooks")]
+#[no_mangle]
+pub extern "C" fn codeg_eui_test_enqueue_blocked(out_request_id: *mut u64) -> i32 {
+    enqueue_payload(
+        out_request_id,
+        Operation::SendUserMessage,
+        CommandPayload::Blocked,
+    )
+}
+
+fn enqueue_utf8(
+    ptr: *const u8,
+    len: usize,
+    max_len: usize,
+    out_request_id: *mut u64,
+    op: Operation,
+) -> i32 {
+    enqueue_utf8_with_policy(ptr, len, max_len, out_request_id, op, false)
+}
+
+fn enqueue_path(
+    ptr: *const u8,
+    len: usize,
+    max_len: usize,
+    out_request_id: *mut u64,
+    op: Operation,
+) -> i32 {
+    enqueue_utf8_with_policy(ptr, len, max_len, out_request_id, op, true)
+}
+
+fn enqueue_utf8_with_policy(
+    ptr: *const u8,
+    len: usize,
+    max_len: usize,
+    out_request_id: *mut u64,
+    op: Operation,
+    reject_nul: bool,
+) -> i32 {
+    ffi_guard(|| {
+        let mut slot = lock_bridge();
+        if let Err(error) = ensure_running(&slot) {
+            return error;
+        }
+        if out_request_id.is_null() {
+            return CODEG_EUI_ERR_NULL_POINTER;
+        }
+        let value = match copy_utf8(ptr, len, max_len) {
+            Ok(value) => value,
+            Err(error) => return error,
+        };
+        if reject_nul && value.contains(&0) {
+            return CODEG_EUI_ERR_INVALID_STATE;
+        }
+        accept_and_write(&mut slot, out_request_id, op, CommandPayload::Utf8(value))
     })
 }
 
-fn bootstrap_slot() -> &'static Mutex<Option<EuiBootstrap>> {
-    BOOTSTRAP.get_or_init(|| Mutex::new(None))
+fn enqueue_payload(out_request_id: *mut u64, op: Operation, payload: CommandPayload) -> i32 {
+    ffi_guard(|| {
+        let mut slot = lock_bridge();
+        if let Err(error) = ensure_running(&slot) {
+            return error;
+        }
+        if out_request_id.is_null() {
+            return CODEG_EUI_ERR_NULL_POINTER;
+        }
+        accept_and_write(&mut slot, out_request_id, op, payload)
+    })
+}
+
+fn ensure_running(slot: &BridgeSlot) -> Result<(), i32> {
+    ensure_ui_thread(slot)?;
+    if slot.lifecycle == LifecycleState::Running {
+        Ok(())
+    } else {
+        Err(CODEG_EUI_ERR_INVALID_STATE)
+    }
+}
+
+fn accept_and_write(
+    slot: &mut BridgeSlot,
+    out_request_id: *mut u64,
+    op: Operation,
+    payload: CommandPayload,
+) -> i32 {
+    let runtime = match slot.runtime.as_ref() {
+        Some(runtime) => runtime,
+        None => return CODEG_EUI_ERR_INTERNAL,
+    };
+    match enqueue(runtime, &slot.model, op, payload) {
+        Ok(request_id) => {
+            unsafe { out_request_id.write(request_id.get()) };
+            CODEG_EUI_OK
+        }
+        Err(error) => error,
+    }
+}
+
+fn copy_utf8(ptr: *const u8, len: usize, max_len: usize) -> Result<Vec<u8>, i32> {
+    if len > max_len {
+        return Err(CODEG_EUI_ERR_TOO_LARGE);
+    }
+    if len == 0 {
+        return Ok(Vec::new());
+    }
+    if ptr.is_null() {
+        return Err(CODEG_EUI_ERR_NULL_POINTER);
+    }
+
+    let bytes = unsafe { std::slice::from_raw_parts(ptr, len) };
+    std::str::from_utf8(bytes).map_err(|_| CODEG_EUI_ERR_INVALID_UTF8)?;
+    Ok(bytes.to_vec())
 }
 
 fn parse_data_root_argument(
     data_dir_utf8: *const u8,
     data_dir_len: usize,
 ) -> Result<Option<PathBuf>, i32> {
-    if data_dir_len == 0 {
+    let bytes = copy_utf8(data_dir_utf8, data_dir_len, CODEG_EUI_MAX_PATH_BYTES)?;
+    if bytes.is_empty() {
         return Ok(None);
     }
-    if data_dir_utf8.is_null() {
-        return Err(CODEG_EUI_ERR_NULL_POINTER);
-    }
-    if data_dir_len > CODEG_EUI_MAX_PATH_BYTES {
+    if bytes.contains(&0) {
         return Err(CODEG_EUI_ERR_INVALID_STATE);
     }
+    Ok(Some(PathBuf::from(
+        String::from_utf8(bytes).map_err(|_| CODEG_EUI_ERR_INVALID_UTF8)?,
+    )))
+}
 
-    // The ABI contract guarantees `data_dir_utf8` is readable for exactly
-    // `data_dir_len` bytes. Bounds and nullness are checked before this read.
-    let bytes = unsafe { std::slice::from_raw_parts(data_dir_utf8, data_dir_len) };
-    if bytes.contains(&0) {
-        return Err(CODEG_EUI_ERR_INVALID_STATE);
+#[cfg(test)]
+mod tests {
+    use super::{ffi_guard, CODEG_EUI_ERR_PANIC};
+
+    #[test]
+    fn ffi_guard_contains_panics() {
+        assert_eq!(ffi_guard(|| panic!("contained")), CODEG_EUI_ERR_PANIC);
     }
-    let path = std::str::from_utf8(bytes).map_err(|_| CODEG_EUI_ERR_INVALID_STATE)?;
-    Ok(Some(PathBuf::from(path)))
 }
diff --git a/src-tauri/codeg-eui-core/src/bootstrap.rs b/src-tauri/codeg-eui-core/src/bootstrap.rs
index e575ee7f..f51bf5c3 100644
--- a/src-tauri/codeg-eui-core/src/bootstrap.rs
+++ b/src-tauri/codeg-eui-core/src/bootstrap.rs
@@ -3,6 +3,7 @@ use std::path::{Path, PathBuf};
 use codeg_lib::app_state::AppState;
 use codeg_lib::logging::init::LogGuard;
 use thiserror::Error;
+use tokio::runtime::Handle;
 use tokio::runtime::{Builder, Runtime};
 
 use crate::data_root::{absolutize_from, startup_working_directory};
@@ -88,6 +89,14 @@ impl EuiBootstrap {
         }
     }
 
+    pub(crate) fn runtime_handle(&self) -> Handle {
+        self.runtime
+            .as_ref()
+            .expect("EUI runtime available before shutdown")
+            .handle()
+            .clone()
+    }
+
     fn new(state: AppState, runtime: Runtime, log_guard: LogGuard) -> Self {
         Self {
             state,
diff --git a/src-tauri/codeg-eui-core/src/commands.rs b/src-tauri/codeg-eui-core/src/commands.rs
new file mode 100644
index 00000000..c1c89ebc
--- /dev/null
+++ b/src-tauri/codeg-eui-core/src/commands.rs
@@ -0,0 +1,48 @@
+use std::num::NonZeroU64;
+
+use crate::model::SharedModel;
+use crate::runtime::RuntimeOwner;
+
+#[repr(u32)]
+#[derive(Clone, Copy, Debug, PartialEq, Eq)]
+pub enum Operation {
+    SetWorkspace = 1,
+    CreateSession = 2,
+    SelectSession = 3,
+    SendUserMessage = 4,
+    CancelActiveTurn = 5,
+    GetAgentSettings = 6,
+    SetAgentSettings = 7,
+    ProbeAgent = 8,
+}
+
+pub(crate) enum CommandPayload {
+    Empty,
+    Utf8(Vec<u8>),
+    SelectSession(i32),
+    AgentSettings {
+        agent: Vec<u8>,
+        json: Vec<u8>,
+    },
+    Blocked,
+    #[cfg(test)]
+    Error(String),
+    #[cfg(test)]
+    Panic,
+}
+
+pub(crate) struct RuntimeCommand {
+    pub request_id: NonZeroU64,
+    pub selection_epoch: u64,
+    pub op: Operation,
+    pub payload: CommandPayload,
+}
+
+pub(crate) fn enqueue(
+    runtime: &RuntimeOwner,
+    model: &SharedModel,
+    op: Operation,
+    payload: CommandPayload,
+) -> Result<NonZeroU64, i32> {
+    runtime.enqueue(model, op, payload)
+}
diff --git a/src-tauri/codeg-eui-core/src/data_root.rs b/src-tauri/codeg-eui-core/src/data_root.rs
index f1b08e67..b31f93e7 100644
--- a/src-tauri/codeg-eui-core/src/data_root.rs
+++ b/src-tauri/codeg-eui-core/src/data_root.rs
@@ -231,9 +231,15 @@ mod tests {
 
     fn complete_shutdown() {
         assert_eq!(codeg_eui_begin_shutdown(), CODEG_EUI_OK);
-        let mut frame = CodegEuiFrame::default();
-        assert_eq!(codeg_eui_poll(&mut frame), CODEG_EUI_OK);
-        assert_eq!(frame.shutdown_ready, 1);
-        assert_eq!(codeg_eui_shutdown(), CODEG_EUI_OK);
+        for _ in 0..200 {
+            let mut frame = CodegEuiFrame::default();
+            assert_eq!(codeg_eui_poll(&mut frame), CODEG_EUI_OK);
+            if frame.shutdown_ready == 1 {
+                assert_eq!(codeg_eui_shutdown(), CODEG_EUI_OK);
+                return;
+            }
+            std::thread::sleep(std::time::Duration::from_millis(5));
+        }
+        panic!("shutdown did not become ready");
     }
 }
diff --git a/src-tauri/codeg-eui-core/src/lib.rs b/src-tauri/codeg-eui-core/src/lib.rs
index 154846e3..90d4010c 100644
--- a/src-tauri/codeg-eui-core/src/lib.rs
+++ b/src-tauri/codeg-eui-core/src/lib.rs
@@ -1,7 +1,16 @@
 mod abi;
 mod bootstrap;
+mod commands;
 mod data_root;
+mod model;
+mod runtime;
 
 pub use abi::*;
 pub use bootstrap::{BootstrapError, EuiBootstrap, StartedServices};
+pub use commands::Operation;
 pub use data_root::{pin_eui_data_root, resolve_eui_data_root, DataRootError, EuiRootInputs};
+pub use model::{
+    CodegEuiCompletion, CodegEuiSessionSummary, CodegEuiSlice, CompletionStatus, SharedModel,
+    CODEG_EUI_COMPLETION_CANCELLED, CODEG_EUI_COMPLETION_ERROR, CODEG_EUI_COMPLETION_OK,
+    CODEG_EUI_COMPLETION_STALE,
+};
diff --git a/src-tauri/codeg-eui-core/src/model.rs b/src-tauri/codeg-eui-core/src/model.rs
new file mode 100644
index 00000000..1af919f5
--- /dev/null
+++ b/src-tauri/codeg-eui-core/src/model.rs
@@ -0,0 +1,444 @@
+use std::collections::{HashMap, HashSet, VecDeque};
+use std::num::NonZeroU64;
+use std::sync::atomic::{AtomicBool, Ordering};
+use std::sync::{Arc, Mutex, MutexGuard};
+
+use crate::abi::{CodegEuiFrame, LifecycleState, CODEG_EUI_API_VERSION};
+use crate::commands::Operation;
+use crate::{CODEG_EUI_COMPLETION_CAPACITY, CODEG_EUI_ERR_QUEUE_FULL};
+
+pub const CODEG_EUI_COMPLETION_OK: u32 = CompletionStatus::Ok as u32;
+pub const CODEG_EUI_COMPLETION_ERROR: u32 = CompletionStatus::Error as u32;
+pub const CODEG_EUI_COMPLETION_STALE: u32 = CompletionStatus::Stale as u32;
+pub const CODEG_EUI_COMPLETION_CANCELLED: u32 = CompletionStatus::Cancelled as u32;
+
+#[repr(u32)]
+#[derive(Clone, Copy, Debug, PartialEq, Eq)]
+pub enum CompletionStatus {
+    Ok = 0,
+    Error = 1,
+    Stale = 2,
+    Cancelled = 3,
+}
+
+#[repr(C)]
+#[derive(Clone, Copy, Default)]
+pub struct CodegEuiSlice {
+    pub ptr: *const u8,
+    pub len: usize,
+}
+
+#[repr(C)]
+#[derive(Clone, Copy, Default)]
+pub struct CodegEuiSessionSummary {
+    pub conversation_id: i32,
+    pub _reserved: u32,
+    pub title: CodegEuiSlice,
+    pub agent: CodegEuiSlice,
+    pub updated_at_ms: i64,
+}
+
+#[repr(C)]
+#[derive(Clone, Copy, Default)]
+pub struct CodegEuiCompletion {
+    pub request_id: u64,
+    pub op: u32,
+    pub status: u32,
+    pub result_payload: CodegEuiSlice,
+    pub error: CodegEuiSlice,
+}
+
+#[derive(Clone, Debug, Default)]
+pub struct OwnedSessionSummary {
+    pub conversation_id: i32,
+    pub title: Vec<u8>,
+    pub agent: Vec<u8>,
+    pub updated_at_ms: i64,
+}
+
+#[derive(Clone, Debug)]
+pub(crate) struct OwnedCompletion {
+    pub request_id: NonZeroU64,
+    pub op: Operation,
+    pub status: CompletionStatus,
+    pub result_payload: Vec<u8>,
+    pub error: Vec<u8>,
+}
+
+impl OwnedCompletion {
+    pub(crate) fn ok(request_id: NonZeroU64, op: Operation, payload: Vec<u8>) -> Self {
+        Self {
+            request_id,
+            op,
+            status: CompletionStatus::Ok,
+            result_payload: payload,
+            error: Vec::new(),
+        }
+    }
+
+    pub(crate) fn error(request_id: NonZeroU64, op: Operation, error: String) -> Self {
+        Self {
+            request_id,
+            op,
+            status: CompletionStatus::Error,
+            result_payload: Vec::new(),
+            error: error.into_bytes(),
+        }
+    }
+}
+
+#[derive(Clone, Copy)]
+struct AcceptedRequest {
+    op: Operation,
+    selection_epoch: u64,
+}
+
+#[derive(Default)]
+struct CompletionLedger {
+    accepted: HashSet<NonZeroU64>,
+    accepted_metadata: HashMap<NonZeroU64, AcceptedRequest>,
+    ready: VecDeque<OwnedCompletion>,
+    reserved: usize,
+}
+
+impl CompletionLedger {
+    fn reserve(
+        &mut self,
+        request_id: NonZeroU64,
+        op: Operation,
+        selection_epoch: u64,
+    ) -> Result<(), i32> {
+        if self.reserved >= CODEG_EUI_COMPLETION_CAPACITY {
+            return Err(CODEG_EUI_ERR_QUEUE_FULL);
+        }
+        assert!(self.accepted.insert(request_id), "request ID reused");
+        assert!(
+            self.accepted_metadata
+                .insert(
+                    request_id,
+                    AcceptedRequest {
+                        op,
+                        selection_epoch,
+                    },
+                )
+                .is_none(),
+            "request metadata reused"
+        );
+        self.reserved += 1;
+        Ok(())
+    }
+
+    fn terminalize(
+        &mut self,
+        current_selection_epoch: u64,
+        captured_selection_epoch: u64,
+        mut completion: OwnedCompletion,
+    ) {
+        assert!(
+            self.accepted.remove(&completion.request_id),
+            "accepted request terminalized more than once"
+        );
+        self.accepted_metadata.remove(&completion.request_id);
+        if completion.status != CompletionStatus::Cancelled
+            && captured_selection_epoch != current_selection_epoch
+        {
+            completion.status = CompletionStatus::Stale;
+        }
+        self.ready.push_back(completion);
+    }
+
+    fn cancel_all(&mut self) {
+        let accepted = self
+            .accepted_metadata
+            .iter()
+            .map(|(request_id, metadata)| (*request_id, *metadata))
+            .collect::<Vec<_>>();
+        for (request_id, metadata) in accepted {
+            self.terminalize(
+                metadata.selection_epoch,
+                metadata.selection_epoch,
+                OwnedCompletion {
+                    request_id,
+                    op: metadata.op,
+                    status: CompletionStatus::Cancelled,
+                    result_payload: Vec::new(),
+                    error: Vec::new(),
+                },
+            );
+        }
+    }
+
+    fn commit_ready(&mut self, count: usize) {
+        assert!(count <= self.ready.len(), "completion commit out of range");
+        self.ready.drain(..count);
+        self.reserved -= count;
+    }
+}
+
+#[derive(Default)]
+struct ModelState {
+    selection_epoch: u64,
+    sessions: Vec<OwnedSessionSummary>,
+    connection_id: Vec<u8>,
+    event_seq: u64,
+    transcript_json: Vec<u8>,
+    live_assistant: Vec<u8>,
+    stream_active: bool,
+    needs_resync: bool,
+    error_strip: Vec<u8>,
+    t0_ns: u64,
+    t_first_token_ns: u64,
+    t_end_ns: u64,
+    ledger: CompletionLedger,
+}
+
+#[derive(Clone, Default)]
+pub struct SharedModel(Arc<Mutex<ModelState>>);
+
+impl SharedModel {
+    pub fn new() -> Self {
+        Self::default()
+    }
+
+    fn lock(&self) -> MutexGuard<'_, ModelState> {
+        self.0.lock().unwrap_or_else(|error| error.into_inner())
+    }
+
+    pub(crate) fn selection_epoch(&self) -> u64 {
+        self.lock().selection_epoch
+    }
+
+    pub(crate) fn reserve(
+        &self,
+        request_id: NonZeroU64,
+        op: Operation,
+        selection_epoch: u64,
+    ) -> Result<(), i32> {
+        self.lock().ledger.reserve(request_id, op, selection_epoch)
+    }
+
+    pub(crate) fn terminalize(&self, captured_selection_epoch: u64, completion: OwnedCompletion) {
+        let mut state = self.lock();
+        let current_selection_epoch = state.selection_epoch;
+        state.ledger.terminalize(
+            current_selection_epoch,
+            captured_selection_epoch,
+            completion,
+        );
+    }
+
+    pub(crate) fn cancel_all(&self) {
+        self.lock().ledger.cancel_all();
+    }
+
+    pub fn set_error_strip(&self, message: Vec<u8>) {
+        self.lock().error_strip = message;
+    }
+
+    pub(crate) fn build_frame(
+        &self,
+        stopping: bool,
+        worker_quiesced: &AtomicBool,
+    ) -> (OwnedFrame, bool) {
+        let mut state = self.lock();
+        let shutdown_ready =
+            stopping && worker_quiesced.load(Ordering::Acquire) && state.ledger.accepted.is_empty();
+        let snapshot = ModelSnapshot {
+            selection_epoch: state.selection_epoch,
+            sessions: state.sessions.clone(),
+            connection_id: state.connection_id.clone(),
+            event_seq: state.event_seq,
+            transcript_json: state.transcript_json.clone(),
+            live_assistant: state.live_assistant.clone(),
+            stream_active: state.stream_active,
+            needs_resync: state.needs_resync,
+            error_strip: state.error_strip.clone(),
+            completions: state.ledger.ready.iter().cloned().collect(),
+            t0_ns: state.t0_ns,
+            t_first_token_ns: state.t_first_token_ns,
+            t_end_ns: state.t_end_ns,
+        };
+        let completion_count = snapshot.completions.len();
+        let frame = OwnedFrame::new(snapshot);
+        state.ledger.commit_ready(completion_count);
+        (frame, shutdown_ready)
+    }
+}
+
+struct ModelSnapshot {
+    selection_epoch: u64,
+    sessions: Vec<OwnedSessionSummary>,
+    connection_id: Vec<u8>,
+    event_seq: u64,
+    transcript_json: Vec<u8>,
+    live_assistant: Vec<u8>,
+    stream_active: bool,
+    needs_resync: bool,
+    error_strip: Vec<u8>,
+    completions: Vec<OwnedCompletion>,
+    t0_ns: u64,
+    t_first_token_ns: u64,
+    t_end_ns: u64,
+}
+
+pub(crate) struct OwnedFrame {
+    selection_epoch: u64,
+    _sessions: Vec<OwnedSessionSummary>,
+    session_views: Vec<CodegEuiSessionSummary>,
+    connection_id: Vec<u8>,
+    event_seq: u64,
+    transcript_json: Vec<u8>,
+    live_assistant: Vec<u8>,
+    stream_active: bool,
+    needs_resync: bool,
+    error_strip: Vec<u8>,
+    _completions: Vec<OwnedCompletion>,
+    completion_views: Vec<CodegEuiCompletion>,
+    t0_ns: u64,
+    t_first_token_ns: u64,
+    t_end_ns: u64,
+}
+
+// The raw pointers in the C views point only into heap allocations owned by
+// this frame. Moving the frame does not move those allocations, and public
+// access remains restricted to the captured UI thread.
+unsafe impl Send for OwnedFrame {}
+
+impl OwnedFrame {
+    fn new(snapshot: ModelSnapshot) -> Self {
+        let session_views = snapshot
+            .sessions
+            .iter()
+            .map(|session| CodegEuiSessionSummary {
+                conversation_id: session.conversation_id,
+                _reserved: 0,
+                title: slice(&session.title),
+                agent: slice(&session.agent),
+                updated_at_ms: session.updated_at_ms,
+            })
+            .collect();
+        let completion_views = snapshot
+            .completions
+            .iter()
+            .map(|completion| CodegEuiCompletion {
+                request_id: completion.request_id.get(),
+                op: completion.op as u32,
+                status: completion.status as u32,
+                result_payload: slice(&completion.result_payload),
+                error: slice(&completion.error),
+            })
+            .collect();
+
+        Self {
+            selection_epoch: snapshot.selection_epoch,
+            _sessions: snapshot.sessions,
+            session_views,
+            connection_id: snapshot.connection_id,
+            event_seq: snapshot.event_seq,
+            transcript_json: snapshot.transcript_json,
+            live_assistant: snapshot.live_assistant,
+            stream_active: snapshot.stream_active,
+            needs_resync: snapshot.needs_resync,
+            error_strip: snapshot.error_strip,
+            _completions: snapshot.completions,
+            completion_views,
+            t0_ns: snapshot.t0_ns,
+            t_first_token_ns: snapshot.t_first_token_ns,
+            t_end_ns: snapshot.t_end_ns,
+        }
+    }
+
+    pub(crate) fn as_abi(
+        &self,
+        lifecycle: LifecycleState,
+        generation: u64,
+        shutdown_ready: bool,
+    ) -> CodegEuiFrame {
+        CodegEuiFrame {
+            api_version: CODEG_EUI_API_VERSION,
+            lifecycle_state: lifecycle as u32,
+            generation,
+            selection_epoch: self.selection_epoch,
+            sessions: ptr_or_null(&self.session_views),
+            sessions_len: self.session_views.len(),
+            connection_id: slice(&self.connection_id),
+            event_seq: self.event_seq,
+            transcript_json: slice(&self.transcript_json),
+            live_assistant: slice(&self.live_assistant),
+            stream_active: u8::from(self.stream_active),
+            needs_resync: u8::from(self.needs_resync),
+            shutdown_ready: u8::from(shutdown_ready),
+            _reserved: [0; 5],
+            error_strip: slice(&self.error_strip),
+            completions: ptr_or_null(&self.completion_views),
+            completions_len: self.completion_views.len(),
+            t0_ns: self.t0_ns,
+            t_first_token_ns: self.t_first_token_ns,
+            t_end_ns: self.t_end_ns,
+        }
+    }
+}
+
+fn slice(bytes: &[u8]) -> CodegEuiSlice {
+    CodegEuiSlice {
+        ptr: if bytes.is_empty() {
+            std::ptr::null()
+        } else {
+            bytes.as_ptr()
+        },
+        len: bytes.len(),
+    }
+}
+
+fn ptr_or_null<T>(values: &[T]) -> *const T {
+    if values.is_empty() {
+        std::ptr::null()
+    } else {
+        values.as_ptr()
+    }
+}
+
+#[cfg(test)]
+mod tests {
+    use std::num::NonZeroU64;
+
+    use super::{CompletionStatus, OwnedCompletion, SharedModel};
+    use crate::commands::Operation;
+
+    #[test]
+    fn selection_changes_mark_one_terminal_completion_stale() {
+        let model = SharedModel::new();
+        let request_id = NonZeroU64::new(1).unwrap();
+        model
+            .reserve(request_id, Operation::SendUserMessage, 0)
+            .unwrap();
+        model.lock().selection_epoch = 1;
+        model.terminalize(
+            0,
+            OwnedCompletion::ok(request_id, Operation::SendUserMessage, Vec::new()),
+        );
+
+        assert_eq!(
+            model.lock().ledger.ready.front().unwrap().status,
+            CompletionStatus::Stale
+        );
+    }
+
+    #[test]
+    #[should_panic(expected = "accepted request terminalized more than once")]
+    fn duplicate_terminalization_is_rejected() {
+        let model = SharedModel::new();
+        let request_id = NonZeroU64::new(1).unwrap();
+        model
+            .reserve(request_id, Operation::SendUserMessage, 0)
+            .unwrap();
+        model.terminalize(
+            0,
+            OwnedCompletion::ok(request_id, Operation::SendUserMessage, Vec::new()),
+        );
+        model.terminalize(
+            0,
+            OwnedCompletion::ok(request_id, Operation::SendUserMessage, Vec::new()),
+        );
+    }
+}
diff --git a/src-tauri/codeg-eui-core/src/runtime.rs b/src-tauri/codeg-eui-core/src/runtime.rs
new file mode 100644
index 00000000..9d4242b3
--- /dev/null
+++ b/src-tauri/codeg-eui-core/src/runtime.rs
@@ -0,0 +1,330 @@
+use std::collections::HashMap;
+use std::future::pending;
+use std::num::NonZeroU64;
+use std::panic::{catch_unwind, AssertUnwindSafe};
+use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
+use std::sync::{Arc, Mutex};
+
+use codeg_lib::acp::manager::ConnectionManager;
+use codeg_lib::acp::termination::AcpDisconnectOrigin;
+use tokio::sync::{mpsc, watch};
+use tokio::task::{Id, JoinHandle, JoinSet};
+
+use crate::commands::{CommandPayload, Operation, RuntimeCommand};
+use crate::model::{OwnedCompletion, SharedModel};
+use crate::{
+    EuiBootstrap, CODEG_EUI_COMMAND_QUEUE_CAPACITY, CODEG_EUI_ERR_INTERNAL,
+    CODEG_EUI_ERR_INVALID_STATE, CODEG_EUI_ERR_QUEUE_FULL,
+};
+
+static NEXT_REQUEST_ID: AtomicU64 = AtomicU64::new(1);
+
+#[derive(Clone, Copy)]
+struct CommandMetadata {
+    request_id: NonZeroU64,
+    selection_epoch: u64,
+    op: Operation,
+}
+
+struct WorkerExitGuard {
+    model: SharedModel,
+    admission: Arc<Mutex<()>>,
+    quiesced: Arc<AtomicBool>,
+}
+
+impl Drop for WorkerExitGuard {
+    fn drop(&mut self) {
+        let _admission = self
+            .admission
+            .lock()
+            .unwrap_or_else(|error| error.into_inner());
+        let _ = catch_unwind(AssertUnwindSafe(|| self.model.cancel_all()));
+        self.quiesced.store(true, Ordering::Release);
+    }
+}
+
+pub(crate) struct RuntimeOwner {
+    bootstrap: EuiBootstrap,
+    model: SharedModel,
+    command_tx: Option<mpsc::Sender<RuntimeCommand>>,
+    shutdown_tx: Option<watch::Sender<bool>>,
+    worker: JoinHandle<()>,
+    admission: Arc<Mutex<()>>,
+    quiesced: Arc<AtomicBool>,
+}
+
+impl RuntimeOwner {
+    pub(crate) fn start(bootstrap: EuiBootstrap, model: SharedModel) -> Self {
+        let (command_tx, command_rx) = mpsc::channel(CODEG_EUI_COMMAND_QUEUE_CAPACITY);
+        let (shutdown_tx, shutdown_rx) = watch::channel(false);
+        let admission = Arc::new(Mutex::new(()));
+        let quiesced = Arc::new(AtomicBool::new(false));
+        let connections = bootstrap.state.connection_manager.clone_ref();
+        let worker = bootstrap.runtime_handle().spawn(run_worker(
+            command_rx,
+            shutdown_rx,
+            model.clone(),
+            connections,
+            Arc::clone(&admission),
+            Arc::clone(&quiesced),
+        ));
+
+        Self {
+            bootstrap,
+            model,
+            command_tx: Some(command_tx),
+            shutdown_tx: Some(shutdown_tx),
+            worker,
+            admission,
+            quiesced,
+        }
+    }
+
+    pub(crate) fn enqueue(
+        &self,
+        model: &SharedModel,
+        op: Operation,
+        payload: CommandPayload,
+    ) -> Result<NonZeroU64, i32> {
+        let _admission = self
+            .admission
+            .lock()
+            .unwrap_or_else(|error| error.into_inner());
+        if self.quiesced.load(Ordering::Acquire) {
+            return Err(CODEG_EUI_ERR_INTERNAL);
+        }
+        if self.worker.is_finished() {
+            return Err(CODEG_EUI_ERR_INTERNAL);
+        }
+        let sender = self
+            .command_tx
+            .as_ref()
+            .ok_or(CODEG_EUI_ERR_INVALID_STATE)?;
+        let permit = sender.try_reserve().map_err(|error| match error {
+            mpsc::error::TrySendError::Full(_) => CODEG_EUI_ERR_QUEUE_FULL,
+            mpsc::error::TrySendError::Closed(_) => CODEG_EUI_ERR_INVALID_STATE,
+        })?;
+        let request_id = next_request_id()?;
+        let selection_epoch = model.selection_epoch();
+        model.reserve(request_id, op, selection_epoch)?;
+        permit.send(RuntimeCommand {
+            request_id,
+            selection_epoch,
+            op,
+            payload,
+        });
+        Ok(request_id)
+    }
+
+    pub(crate) fn begin_shutdown(&mut self) {
+        self.command_tx.take();
+        if self
+            .shutdown_tx
+            .take()
+            .is_some_and(|shutdown| shutdown.send(true).is_err())
+        {
+            self.model.cancel_all();
+            self.quiesced.store(true, Ordering::Release);
+        }
+    }
+
+    pub(crate) fn quiesced_flag(&self) -> Arc<AtomicBool> {
+        Arc::clone(&self.quiesced)
+    }
+
+    pub(crate) fn join(self) {
+        let Self {
+            bootstrap, worker, ..
+        } = self;
+        drop(worker);
+        bootstrap.shutdown();
+    }
+}
+
+fn next_request_id() -> Result<NonZeroU64, i32> {
+    let value = NEXT_REQUEST_ID
+        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
+            current.checked_add(1)
+        })
+        .map_err(|_| CODEG_EUI_ERR_INTERNAL)?;
+    NonZeroU64::new(value).ok_or(CODEG_EUI_ERR_INTERNAL)
+}
+
+async fn run_worker(
+    mut commands: mpsc::Receiver<RuntimeCommand>,
+    mut shutdown: watch::Receiver<bool>,
+    model: SharedModel,
+    connections: ConnectionManager,
+    admission: Arc<Mutex<()>>,
+    quiesced: Arc<AtomicBool>,
+) {
+    let _exit_guard = WorkerExitGuard {
+        model: model.clone(),
+        admission,
+        quiesced,
+    };
+    let mut tasks = JoinSet::new();
+    let mut metadata = HashMap::<Id, CommandMetadata>::new();
+
+    loop {
+        tokio::select! {
+            biased;
+            changed = shutdown.changed() => {
+                if changed.is_err() || *shutdown.borrow() {
+                    break;
+                }
+            }
+            completed = tasks.join_next_with_id(), if !tasks.is_empty() => {
+                terminalize_task(&model, &mut metadata, completed);
+            }
+            command = commands.recv() => {
+                let Some(command) = command else {
+                    break;
+                };
+                let command_metadata = CommandMetadata {
+                    request_id: command.request_id,
+                    selection_epoch: command.selection_epoch,
+                    op: command.op,
+                };
+                let abort = tasks.spawn(execute_command(command.payload));
+                metadata.insert(abort.id(), command_metadata);
+            }
+        }
+    }
+
+    commands.close();
+    tasks.abort_all();
+    while let Some(completed) = tasks.join_next_with_id().await {
+        if let Ok((id, _)) = &completed {
+            metadata.remove(id);
+        } else if let Err(error) = &completed {
+            metadata.remove(&error.id());
+        }
+    }
+    metadata.clear();
+    while commands.try_recv().is_ok() {}
+    model.cancel_all();
+    connections
+        .disconnect_all(AcpDisconnectOrigin::ApplicationShutdown)
+        .await;
+}
+
+fn terminalize_task(
+    model: &SharedModel,
+    metadata: &mut HashMap<Id, CommandMetadata>,
+    completed: Option<Result<(Id, Result<Vec<u8>, String>), tokio::task::JoinError>>,
+) {
+    let Some(completed) = completed else {
+        return;
+    };
+    let (task_id, result) = match completed {
+        Ok((task_id, result)) => (task_id, result),
+        Err(error) => {
+            let task_id = error.id();
+            (task_id, Err(format!("worker panic: {error}")))
+        }
+    };
+    let command = metadata
+        .remove(&task_id)
+        .expect("metadata exists for every worker task");
+    let completion = match result {
+        Ok(payload) => OwnedCompletion::ok(command.request_id, command.op, payload),
+        Err(error) => OwnedCompletion::error(command.request_id, command.op, error),
+    };
+    model.terminalize(command.selection_epoch, completion);
+}
+
+async fn execute_command(payload: CommandPayload) -> Result<Vec<u8>, String> {
+    match payload {
+        CommandPayload::Blocked => pending().await,
+        #[cfg(test)]
+        CommandPayload::Error(error) => Err(error),
+        #[cfg(test)]
+        CommandPayload::Panic => panic!("test worker panic"),
+        CommandPayload::Empty => Err("operation is not implemented in Task 3".to_string()),
+        CommandPayload::Utf8(value) => {
+            let _ = value;
+            Err("operation is not implemented in Task 3".to_string())
+        }
+        CommandPayload::SelectSession(conversation_id) => {
+            let _ = conversation_id;
+            Err("operation is not implemented in Task 3".to_string())
+        }
+        CommandPayload::AgentSettings { agent, json } => {
+            let _ = (agent, json);
+            Err("operation is not implemented in Task 3".to_string())
+        }
+    }
+}
+
+#[cfg(test)]
+mod tests {
+    use std::collections::HashMap;
+    use std::num::NonZeroU64;
+    use std::sync::atomic::AtomicBool;
+
+    use tokio::task::JoinSet;
+
+    use super::{execute_command, terminalize_task, CommandMetadata};
+    use crate::commands::{CommandPayload, Operation};
+    use crate::{CompletionStatus, LifecycleState, SharedModel};
+
+    #[tokio::test]
+    async fn worker_errors_are_terminal_results() {
+        assert_eq!(
+            execute_command(CommandPayload::Error("expected".to_string())).await,
+            Err("expected".to_string())
+        );
+    }
+
+    #[tokio::test]
+    async fn worker_panics_are_visible_to_the_join_boundary() {
+        let joined = tokio::spawn(execute_command(CommandPayload::Panic)).await;
+        assert!(joined
+            .expect_err("worker panic must be caught by join")
+            .is_panic());
+    }
+
+    #[tokio::test]
+    async fn worker_error_and_panic_each_terminalize_once() {
+        let model = SharedModel::new();
+        let mut metadata = HashMap::new();
+        let cases = [
+            (
+                NonZeroU64::new(1).unwrap(),
+                CommandPayload::Error("expected error".to_string()),
+            ),
+            (NonZeroU64::new(2).unwrap(), CommandPayload::Panic),
+        ];
+
+        for (request_id, payload) in cases {
+            model
+                .reserve(request_id, Operation::SendUserMessage, 0)
+                .unwrap();
+            let mut tasks = JoinSet::new();
+            let abort = tasks.spawn(execute_command(payload));
+            metadata.insert(
+                abort.id(),
+                CommandMetadata {
+                    request_id,
+                    selection_epoch: 0,
+                    op: Operation::SendUserMessage,
+                },
+            );
+            terminalize_task(&model, &mut metadata, tasks.join_next_with_id().await);
+        }
+
+        let (owned, ready) = model.build_frame(false, &AtomicBool::new(false));
+        assert!(!ready);
+        let frame = owned.as_abi(LifecycleState::Running, 1, false);
+        let completions =
+            unsafe { std::slice::from_raw_parts(frame.completions, frame.completions_len) };
+        assert_eq!(completions.len(), 2);
+        assert!(completions
+            .iter()
+            .all(|completion| completion.status == CompletionStatus::Error as u32));
+        assert!(completions
+            .iter()
+            .all(|completion| completion.error.len > 0));
+    }
+}
diff --git a/src-tauri/codeg-eui-core/tests/abi_smoke.rs b/src-tauri/codeg-eui-core/tests/abi_smoke.rs
index d2ef718c..359b3bbd 100644
--- a/src-tauri/codeg-eui-core/tests/abi_smoke.rs
+++ b/src-tauri/codeg-eui-core/tests/abi_smoke.rs
@@ -3,6 +3,7 @@ use codeg_eui_core::{
     codeg_eui_shutdown, CodegEuiFrame, CODEG_EUI_API_VERSION, CODEG_EUI_ERR_INVALID_STATE,
     CODEG_EUI_ERR_NULL_POINTER, CODEG_EUI_OK,
 };
+use std::time::Duration;
 
 #[test]
 fn abi_version_and_null_poll_are_stable() {
@@ -23,9 +24,20 @@ fn abi_version_and_null_poll_are_stable() {
     assert_eq!(codeg_eui_shutdown(), CODEG_EUI_ERR_INVALID_STATE);
     assert_eq!(codeg_eui_begin_shutdown(), CODEG_EUI_OK);
 
-    let mut frame = CodegEuiFrame::default();
-    assert_eq!(codeg_eui_poll(&mut frame), CODEG_EUI_OK);
+    let frame = drain_until_ready();
     assert_eq!(frame.api_version, CODEG_EUI_API_VERSION);
     assert_eq!(frame.shutdown_ready, 1);
     assert_eq!(codeg_eui_shutdown(), CODEG_EUI_OK);
 }
+
+fn drain_until_ready() -> CodegEuiFrame {
+    for _ in 0..200 {
+        let mut frame = CodegEuiFrame::default();
+        assert_eq!(codeg_eui_poll(&mut frame), CODEG_EUI_OK);
+        if frame.shutdown_ready == 1 {
+            return frame;
+        }
+        std::thread::sleep(Duration::from_millis(5));
+    }
+    panic!("shutdown did not become ready");
+}
diff --git a/src-tauri/codeg-eui-core/tests/bridge_contract.rs b/src-tauri/codeg-eui-core/tests/bridge_contract.rs
new file mode 100644
index 00000000..1663fd60
--- /dev/null
+++ b/src-tauri/codeg-eui-core/tests/bridge_contract.rs
@@ -0,0 +1,376 @@
+use std::collections::HashSet;
+use std::process::Command;
+use std::time::Duration;
+
+use codeg_eui_core::{
+    codeg_eui_begin_shutdown, codeg_eui_init, codeg_eui_poll, codeg_eui_send_user_message,
+    codeg_eui_set_workspace, codeg_eui_shutdown, CodegEuiCompletion, CodegEuiFrame,
+    CodegEuiSessionSummary, CodegEuiSlice, CompletionStatus, LifecycleState, Operation,
+    CODEG_EUI_API_VERSION, CODEG_EUI_COMPLETION_CAPACITY, CODEG_EUI_ERR_INTERNAL,
+    CODEG_EUI_ERR_INVALID_STATE, CODEG_EUI_ERR_INVALID_UTF8, CODEG_EUI_ERR_NOT_READY,
+    CODEG_EUI_ERR_NULL_POINTER, CODEG_EUI_ERR_PANIC, CODEG_EUI_ERR_QUEUE_FULL,
+    CODEG_EUI_ERR_TOO_LARGE, CODEG_EUI_ERR_WRONG_THREAD, CODEG_EUI_MAX_MESSAGE_BYTES, CODEG_EUI_OK,
+};
+
+const CHILD_CASE: &str = "CODEG_EUI_BRIDGE_CONTRACT_CASE";
+const CHILD_ROOT: &str = "CODEG_EUI_BRIDGE_CONTRACT_ROOT";
+
+#[test]
+fn complete_abi_layout_matches_v1() {
+    assert_eq!(CODEG_EUI_OK, 0);
+    assert_eq!(CODEG_EUI_ERR_INVALID_STATE, 1);
+    assert_eq!(CODEG_EUI_ERR_NULL_POINTER, 2);
+    assert_eq!(CODEG_EUI_ERR_INVALID_UTF8, 3);
+    assert_eq!(CODEG_EUI_ERR_TOO_LARGE, 4);
+    assert_eq!(CODEG_EUI_ERR_QUEUE_FULL, 5);
+    assert_eq!(CODEG_EUI_ERR_WRONG_THREAD, 6);
+    assert_eq!(CODEG_EUI_ERR_PANIC, 7);
+    assert_eq!(CODEG_EUI_ERR_INTERNAL, 8);
+    assert_eq!(CODEG_EUI_ERR_NOT_READY, 9);
+    assert_eq!(std::mem::size_of::<LifecycleState>(), 4);
+    assert_eq!(LifecycleState::Uninitialized as u32, 0);
+    assert_eq!(LifecycleState::Starting as u32, 1);
+    assert_eq!(LifecycleState::Running as u32, 2);
+    assert_eq!(LifecycleState::Stopping as u32, 3);
+    assert_eq!(LifecycleState::Stopped as u32, 4);
+    assert_eq!(std::mem::size_of::<Operation>(), 4);
+    assert_eq!(Operation::SetWorkspace as u32, 1);
+    assert_eq!(Operation::CreateSession as u32, 2);
+    assert_eq!(Operation::SelectSession as u32, 3);
+    assert_eq!(Operation::SendUserMessage as u32, 4);
+    assert_eq!(Operation::CancelActiveTurn as u32, 5);
+    assert_eq!(Operation::GetAgentSettings as u32, 6);
+    assert_eq!(Operation::SetAgentSettings as u32, 7);
+    assert_eq!(Operation::ProbeAgent as u32, 8);
+    assert_eq!(std::mem::size_of::<CompletionStatus>(), 4);
+    assert_eq!(CompletionStatus::Ok as u32, 0);
+    assert_eq!(CompletionStatus::Error as u32, 1);
+    assert_eq!(CompletionStatus::Stale as u32, 2);
+    assert_eq!(CompletionStatus::Cancelled as u32, 3);
+
+    assert_eq!(std::mem::size_of::<CodegEuiSlice>(), 16);
+    assert_eq!(std::mem::align_of::<CodegEuiSlice>(), 8);
+    assert_eq!(std::mem::offset_of!(CodegEuiSlice, ptr), 0);
+    assert_eq!(std::mem::offset_of!(CodegEuiSlice, len), 8);
+
+    assert_eq!(std::mem::size_of::<CodegEuiSessionSummary>(), 48);
+    assert_eq!(std::mem::align_of::<CodegEuiSessionSummary>(), 8);
+    assert_eq!(
+        std::mem::offset_of!(CodegEuiSessionSummary, conversation_id),
+        0
+    );
+    assert_eq!(std::mem::offset_of!(CodegEuiSessionSummary, _reserved), 4);
+    assert_eq!(std::mem::offset_of!(CodegEuiSessionSummary, title), 8);
+    assert_eq!(std::mem::offset_of!(CodegEuiSessionSummary, agent), 24);
+    assert_eq!(
+        std::mem::offset_of!(CodegEuiSessionSummary, updated_at_ms),
+        40
+    );
+
+    assert_eq!(std::mem::size_of::<CodegEuiCompletion>(), 48);
+    assert_eq!(std::mem::align_of::<CodegEuiCompletion>(), 8);
+    assert_eq!(std::mem::offset_of!(CodegEuiCompletion, request_id), 0);
+    assert_eq!(std::mem::offset_of!(CodegEuiCompletion, op), 8);
+    assert_eq!(std::mem::offset_of!(CodegEuiCompletion, status), 12);
+    assert_eq!(std::mem::offset_of!(CodegEuiCompletion, result_payload), 16);
+    assert_eq!(std::mem::offset_of!(CodegEuiCompletion, error), 32);
+
+    assert_eq!(std::mem::size_of::<CodegEuiFrame>(), 160);
+    assert_eq!(std::mem::align_of::<CodegEuiFrame>(), 8);
+    assert_eq!(std::mem::offset_of!(CodegEuiFrame, api_version), 0);
+    assert_eq!(std::mem::offset_of!(CodegEuiFrame, lifecycle_state), 4);
+    assert_eq!(std::mem::offset_of!(CodegEuiFrame, generation), 8);
+    assert_eq!(std::mem::offset_of!(CodegEuiFrame, selection_epoch), 16);
+    assert_eq!(std::mem::offset_of!(CodegEuiFrame, sessions), 24);
+    assert_eq!(std::mem::offset_of!(CodegEuiFrame, sessions_len), 32);
+    assert_eq!(std::mem::offset_of!(CodegEuiFrame, connection_id), 40);
+    assert_eq!(std::mem::offset_of!(CodegEuiFrame, event_seq), 56);
+    assert_eq!(std::mem::offset_of!(CodegEuiFrame, transcript_json), 64);
+    assert_eq!(std::mem::offset_of!(CodegEuiFrame, live_assistant), 80);
+    assert_eq!(std::mem::offset_of!(CodegEuiFrame, stream_active), 96);
+    assert_eq!(std::mem::offset_of!(CodegEuiFrame, needs_resync), 97);
+    assert_eq!(std::mem::offset_of!(CodegEuiFrame, shutdown_ready), 98);
+    assert_eq!(std::mem::offset_of!(CodegEuiFrame, _reserved), 99);
+    assert_eq!(std::mem::offset_of!(CodegEuiFrame, error_strip), 104);
+    assert_eq!(std::mem::offset_of!(CodegEuiFrame, completions), 120);
+    assert_eq!(std::mem::offset_of!(CodegEuiFrame, completions_len), 128);
+    assert_eq!(std::mem::offset_of!(CodegEuiFrame, t0_ns), 136);
+    assert_eq!(std::mem::offset_of!(CodegEuiFrame, t_first_token_ns), 144);
+    assert_eq!(std::mem::offset_of!(CodegEuiFrame, t_end_ns), 152);
+}
+
+#[test]
+fn lifecycle_rejects_invalid_order_and_wrong_thread() {
+    run_isolated("lifecycle", || {
+        assert_eq!(codeg_eui_shutdown(), CODEG_EUI_ERR_INVALID_STATE);
+        assert_eq!(init(), CODEG_EUI_OK);
+        assert_eq!(init(), CODEG_EUI_ERR_INVALID_STATE);
+        assert_eq!(poll().lifecycle_state, LifecycleState::Running as u32);
+        assert_eq!(
+            std::thread::spawn(|| {
+                let mut frame = CodegEuiFrame::default();
+                codeg_eui_poll(&mut frame)
+            })
+            .join()
+            .expect("wrong-thread poll joined"),
+            CODEG_EUI_ERR_WRONG_THREAD
+        );
+        assert_eq!(codeg_eui_begin_shutdown(), CODEG_EUI_OK);
+        let mut request_id = 0;
+        assert_eq!(
+            codeg_eui_send_user_message(b"x".as_ptr(), 1, &mut request_id),
+            CODEG_EUI_ERR_INVALID_STATE
+        );
+        assert_eq!(
+            codeg_eui_shutdown(),
+            codeg_eui_core::CODEG_EUI_ERR_NOT_READY
+        );
+        assert_eq!(poll().lifecycle_state, LifecycleState::Stopping as u32);
+        drain_until_ready();
+        assert_eq!(codeg_eui_shutdown(), CODEG_EUI_OK);
+        assert_eq!(codeg_eui_shutdown(), CODEG_EUI_ERR_INVALID_STATE);
+    });
+}
+
+#[test]
+fn strings_reject_null_invalid_utf8_and_bounds_without_accepting_a_request() {
+    run_isolated("input", || {
+        assert_eq!(init(), CODEG_EUI_OK);
+        let mut request_id = 91;
+        assert_eq!(
+            codeg_eui_send_user_message(std::ptr::null(), 1, &mut request_id),
+            CODEG_EUI_ERR_NULL_POINTER
+        );
+        assert_eq!(request_id, 91);
+        assert_eq!(
+            codeg_eui_send_user_message([0xff].as_ptr(), 1, &mut request_id),
+            CODEG_EUI_ERR_INVALID_UTF8
+        );
+        assert_eq!(request_id, 91);
+        let oversized = vec![b'x'; CODEG_EUI_MAX_MESSAGE_BYTES + 1];
+        assert_eq!(
+            codeg_eui_send_user_message(oversized.as_ptr(), oversized.len(), &mut request_id,),
+            CODEG_EUI_ERR_TOO_LARGE
+        );
+        assert_eq!(request_id, 91);
+        assert_eq!(
+            codeg_eui_send_user_message(b"x".as_ptr(), 1, std::ptr::null_mut()),
+            CODEG_EUI_ERR_NULL_POINTER
+        );
+        assert!(copy_completions(&poll()).is_empty());
+        complete_shutdown();
+    });
+}
+
+#[test]
+fn path_inputs_reject_embedded_nul_without_accepting_a_request() {
+    run_isolated("path_nul", || {
+        assert_eq!(init(), CODEG_EUI_OK);
+        let mut request_id = 91;
+        let path = b"workspace\0suffix";
+        assert_eq!(
+            codeg_eui_set_workspace(path.as_ptr(), path.len(), &mut request_id),
+            CODEG_EUI_ERR_INVALID_STATE
+        );
+        assert_eq!(request_id, 91);
+        assert!(copy_completions(&poll()).is_empty());
+        complete_shutdown();
+    });
+}
+
+#[test]
+fn queue_rejects_the_257th_request_before_acceptance() {
+    run_isolated("queue", || {
+        assert_eq!(init(), CODEG_EUI_OK);
+        let mut ids = Vec::with_capacity(CODEG_EUI_COMPLETION_CAPACITY);
+        for _ in 0..CODEG_EUI_COMPLETION_CAPACITY {
+            let mut request_id = 0;
+            assert_eq!(
+                codeg_eui_send_user_message(b"x".as_ptr(), 1, &mut request_id),
+                CODEG_EUI_OK
+            );
+            ids.push(request_id);
+        }
+        assert!(ids.iter().all(|request_id| *request_id != 0));
+        assert_eq!(ids.iter().copied().collect::<HashSet<_>>().len(), ids.len());
+        let max_request_id = *ids.iter().max().expect("accepted request IDs");
+
+        let mut rejected_id = 777;
+        assert_eq!(
+            codeg_eui_send_user_message(b"x".as_ptr(), 1, &mut rejected_id),
+            CODEG_EUI_ERR_QUEUE_FULL
+        );
+        assert_eq!(rejected_id, 777);
+
+        let seen = collect_completions(ids.len());
+        assert_eq!(seen.len(), ids.len());
+        assert_eq!(
+            seen.iter()
+                .map(|item| item.request_id)
+                .collect::<HashSet<_>>(),
+            ids.into_iter().collect::<HashSet<_>>()
+        );
+        complete_shutdown();
+
+        assert_eq!(init(), CODEG_EUI_OK);
+        let mut restarted_id = 0;
+        assert_eq!(
+            codeg_eui_send_user_message(b"restart".as_ptr(), 7, &mut restarted_id),
+            CODEG_EUI_OK
+        );
+        assert!(restarted_id > max_request_id);
+        complete_shutdown();
+    });
+}
+
+#[test]
+fn frame_bytes_survive_enqueue_and_failed_poll_then_transfer_once() {
+    run_isolated("frame", || {
+        assert_eq!(init(), CODEG_EUI_OK);
+        let frame_a = poll();
+        let generation_a = frame_a.generation;
+
+        let mut request_id = 0;
+        assert_eq!(
+            codeg_eui_send_user_message(b"frame-a".as_ptr(), 7, &mut request_id),
+            CODEG_EUI_OK
+        );
+        assert_eq!(frame_a.generation, generation_a);
+
+        let frame_b = poll_until_completion(request_id);
+        let completion_b = unsafe {
+            std::slice::from_raw_parts(frame_b.completions, frame_b.completions_len)
+                .iter()
+                .find(|completion| completion.request_id == request_id)
+                .copied()
+                .expect("completion in frame B")
+        };
+        let expected_error = copy_slice(completion_b.error);
+        assert!(!expected_error.is_empty());
+
+        let mut later_id = 0;
+        assert_eq!(
+            codeg_eui_send_user_message(b"later".as_ptr(), 5, &mut later_id),
+            CODEG_EUI_OK
+        );
+        assert_eq!(copy_slice(completion_b.error), expected_error);
+
+        assert_eq!(
+            std::thread::spawn(|| {
+                let mut frame = CodegEuiFrame::default();
+                codeg_eui_poll(&mut frame)
+            })
+            .join()
+            .expect("failed poll joined"),
+            CODEG_EUI_ERR_WRONG_THREAD
+        );
+        assert_eq!(copy_slice(completion_b.error), expected_error);
+
+        let frame_c = poll();
+        assert!(copy_completions(&frame_c)
+            .iter()
+            .all(|completion| completion.request_id != request_id));
+        complete_shutdown();
+    });
+}
+
+#[derive(Debug)]
+struct CompletionCopy {
+    request_id: u64,
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
+    let root = tempfile::tempdir().expect("tempdir");
+    let status = Command::new(std::env::current_exe().expect("current test executable"))
+        .args(["--exact", std::thread::current().name().expect("test name")])
+        .env(CHILD_CASE, case)
+        .env(CHILD_ROOT, root.path())
+        .status()
+        .expect("run isolated bridge contract");
+    assert!(status.success(), "isolated bridge case {case} failed");
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
+    assert_eq!(frame.api_version, CODEG_EUI_API_VERSION);
+    frame
+}
+
+fn poll_until_completion(request_id: u64) -> CodegEuiFrame {
+    for _ in 0..200 {
+        let frame = poll();
+        if copy_completions(&frame)
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
+fn collect_completions(expected: usize) -> Vec<CompletionCopy> {
+    let mut seen = Vec::new();
+    for _ in 0..200 {
+        seen.extend(copy_completions(&poll()));
+        if seen.len() == expected {
+            return seen;
+        }
+        std::thread::sleep(Duration::from_millis(5));
+    }
+    panic!("observed {} of {expected} completions", seen.len());
+}
+
+fn copy_completions(frame: &CodegEuiFrame) -> Vec<CompletionCopy> {
+    if frame.completions_len == 0 {
+        assert!(frame.completions.is_null());
+        return Vec::new();
+    }
+    assert!(!frame.completions.is_null());
+    unsafe { std::slice::from_raw_parts(frame.completions, frame.completions_len) }
+        .iter()
+        .map(|completion| CompletionCopy {
+            request_id: completion.request_id,
+        })
+        .collect()
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
+    drain_until_ready();
+    assert_eq!(codeg_eui_shutdown(), CODEG_EUI_OK);
+}
+
+fn drain_until_ready() {
+    for _ in 0..200 {
+        if poll().shutdown_ready == 1 {
+            return;
+        }
+        std::thread::sleep(Duration::from_millis(5));
+    }
+    panic!("shutdown did not become ready");
+}
diff --git a/src-tauri/codeg-eui-core/tests/data_root_isolation.rs b/src-tauri/codeg-eui-core/tests/data_root_isolation.rs
index b0fc9e59..4a8839e9 100644
--- a/src-tauri/codeg-eui-core/tests/data_root_isolation.rs
+++ b/src-tauri/codeg-eui-core/tests/data_root_isolation.rs
@@ -5,7 +5,8 @@ use std::sync::Mutex;
 use codeg_eui_core::{
     codeg_eui_begin_shutdown, codeg_eui_init, codeg_eui_poll, codeg_eui_shutdown,
     pin_eui_data_root, resolve_eui_data_root, CodegEuiFrame, DataRootError, EuiBootstrap,
-    EuiRootInputs, CODEG_EUI_ERR_INVALID_STATE, CODEG_EUI_OK,
+    EuiRootInputs, CODEG_EUI_ERR_INVALID_STATE, CODEG_EUI_ERR_INVALID_UTF8,
+    CODEG_EUI_ERR_TOO_LARGE, CODEG_EUI_OK,
 };
 use tempfile::TempDir;
 
@@ -134,12 +135,12 @@ fn isolated_process_case() {
             let invalid_utf8 = [0xff];
             assert_eq!(
                 codeg_eui_init(invalid_utf8.as_ptr(), invalid_utf8.len()),
-                CODEG_EUI_ERR_INVALID_STATE
+                CODEG_EUI_ERR_INVALID_UTF8
             );
             let oversized = vec![b'x'; 32_769];
             assert_eq!(
                 codeg_eui_init(oversized.as_ptr(), oversized.len()),
-                CODEG_EUI_ERR_INVALID_STATE
+                CODEG_EUI_ERR_TOO_LARGE
             );
             let embedded_nul = b"invalid\0root";
             assert_eq!(
@@ -156,22 +157,14 @@ fn isolated_process_case() {
                 Some(argument_root.clone().into_os_string())
             );
             assert!(std::env::var_os("CODEG_HOME").is_none());
-            assert_eq!(codeg_eui_begin_shutdown(), CODEG_EUI_OK);
-            let mut frame = CodegEuiFrame::default();
-            assert_eq!(codeg_eui_poll(&mut frame), CODEG_EUI_OK);
-            assert_eq!(frame.shutdown_ready, 1);
-            assert_eq!(codeg_eui_shutdown(), CODEG_EUI_OK);
+            complete_abi_shutdown();
 
             assert_eq!(
                 codeg_eui_init(argument.as_ptr(), argument.len()),
                 CODEG_EUI_OK,
                 "re-init with the same normalized root must remain legal"
             );
-            assert_eq!(codeg_eui_begin_shutdown(), CODEG_EUI_OK);
-            let mut second_frame = CodegEuiFrame::default();
-            assert_eq!(codeg_eui_poll(&mut second_frame), CODEG_EUI_OK);
-            assert_eq!(second_frame.shutdown_ready, 1);
-            assert_eq!(codeg_eui_shutdown(), CODEG_EUI_OK);
+            complete_abi_shutdown();
 
             let different = different_root.to_str().expect("UTF-8 temp path").as_bytes();
             assert_eq!(
@@ -225,3 +218,17 @@ fn run_child_case(case: &str, fixture: &IsolationFixture) {
 fn path_from_env(name: &str) -> PathBuf {
     Path::new(&std::env::var_os(name).unwrap_or_else(|| panic!("{name} is set"))).to_path_buf()
 }
+
+fn complete_abi_shutdown() {
+    assert_eq!(codeg_eui_begin_shutdown(), CODEG_EUI_OK);
+    for _ in 0..200 {
+        let mut frame = CodegEuiFrame::default();
+        assert_eq!(codeg_eui_poll(&mut frame), CODEG_EUI_OK);
+        if frame.shutdown_ready == 1 {
+            assert_eq!(codeg_eui_shutdown(), CODEG_EUI_OK);
+            return;
+        }
+        std::thread::sleep(std::time::Duration::from_millis(5));
+    }
+    panic!("shutdown did not become ready");
+}
diff --git a/src-tauri/codeg-eui-core/tests/shutdown_contract.rs b/src-tauri/codeg-eui-core/tests/shutdown_contract.rs
new file mode 100644
index 00000000..e7ccf6a0
--- /dev/null
+++ b/src-tauri/codeg-eui-core/tests/shutdown_contract.rs
@@ -0,0 +1,74 @@
+use std::process::Command;
+use std::time::Duration;
+
+use codeg_eui_core::{
+    codeg_eui_begin_shutdown, codeg_eui_init, codeg_eui_poll, codeg_eui_send_user_message,
+    codeg_eui_shutdown, enqueue_blocked_for_test, CodegEuiFrame, LifecycleState,
+    CODEG_EUI_COMPLETION_CANCELLED, CODEG_EUI_ERR_INVALID_STATE, CODEG_EUI_ERR_NOT_READY,
+    CODEG_EUI_OK,
+};
+
+const CHILD_CASE: &str = "CODEG_EUI_SHUTDOWN_CONTRACT_CASE";
+const CHILD_ROOT: &str = "CODEG_EUI_SHUTDOWN_CONTRACT_ROOT";
+
+#[test]
+fn stopping_poll_exposes_cancelled_completion_before_final_free() {
+    if std::env::var_os(CHILD_CASE).is_none() {
+        let root = tempfile::tempdir().expect("tempdir");
+        let status = Command::new(std::env::current_exe().expect("current test executable"))
+            .args([
+                "--exact",
+                "stopping_poll_exposes_cancelled_completion_before_final_free",
+            ])
+            .env(CHILD_CASE, "child")
+            .env(CHILD_ROOT, root.path())
+            .status()
+            .expect("run isolated shutdown contract");
+        assert!(status.success(), "isolated shutdown contract failed");
+        return;
+    }
+
+    let root = std::env::var(CHILD_ROOT).expect("isolated root");
+    assert_eq!(codeg_eui_init(root.as_ptr(), root.len()), CODEG_EUI_OK);
+
+    let request_id = enqueue_blocked_for_test().expect("blocked request accepted");
+    assert_eq!(codeg_eui_begin_shutdown(), CODEG_EUI_OK);
+    let mut rejected_id = 0;
+    assert_eq!(
+        codeg_eui_send_user_message(b"late".as_ptr(), 4, &mut rejected_id),
+        CODEG_EUI_ERR_INVALID_STATE
+    );
+    assert_eq!(codeg_eui_shutdown(), CODEG_EUI_ERR_NOT_READY);
+
+    let mut cancelled_count = 0;
+    let mut ready = false;
+    for _ in 0..200 {
+        let mut frame = CodegEuiFrame::default();
+        assert_eq!(codeg_eui_poll(&mut frame), CODEG_EUI_OK);
+        assert_eq!(frame.lifecycle_state, LifecycleState::Stopping as u32);
+        if frame.completions_len > 0 {
+            assert!(!frame.completions.is_null());
+            let completions =
+                unsafe { std::slice::from_raw_parts(frame.completions, frame.completions_len) };
+            cancelled_count += completions
+                .iter()
+                .filter(|completion| {
+                    completion.request_id == request_id
+                        && completion.status == CODEG_EUI_COMPLETION_CANCELLED
+                })
+                .count();
+        }
+        if frame.shutdown_ready == 1 {
+            ready = true;
+            break;
+        }
+        std::thread::sleep(Duration::from_millis(5));
+    }
+
+    assert!(ready, "shutdown-ready frame was not observed");
+    assert_eq!(cancelled_count, 1);
+    assert_eq!(codeg_eui_shutdown(), CODEG_EUI_OK);
+    let mut after = CodegEuiFrame::default();
+    assert_eq!(codeg_eui_poll(&mut after), CODEG_EUI_ERR_INVALID_STATE);
+    assert_eq!(codeg_eui_shutdown(), CODEG_EUI_ERR_INVALID_STATE);
+}
