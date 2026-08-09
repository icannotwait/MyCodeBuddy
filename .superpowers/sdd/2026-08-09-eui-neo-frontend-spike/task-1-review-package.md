# Task 1 Review Package
BASE: ac1e38d52dc48d9038a33e964086f665d1b21148
HEAD: 6fcfd6999d69d16d829b0410c1e828069aec0628

## Commits
6fcfd699 feat(eui): add optional native shell build spine

## Stat
 .gitmodules                                 |   3 +
 codeg-eui/.gitignore                        |   4 +
 codeg-eui/CMakeLists.txt                    |  52 ++++++++++++
 codeg-eui/app/app.cpp                       |  75 +++++++++++++++++
 codeg-eui/app/bridge/codeg_eui_bridge.h     |  34 ++++++++
 codeg-eui/scripts/build.sh                  |  34 ++++++++
 codeg-eui/tests/abi_layout_test.cpp         |  19 +++++
 codeg-eui/tests/assert_ctest_red.sh         |  17 ++++
 codeg-eui/tests/assert_ctest_registered.sh  |  14 ++++
 codeg-eui/tests/harness_self_test.cpp       |   9 +++
 codeg-eui/tests/test_harness.h              |  97 ++++++++++++++++++++++
 codeg-eui/tests/test_main.cpp               |   5 ++
 codeg-eui/third_party/EUI-NEO               |   1 +
 src-tauri/codeg-eui-core/.gitignore         |   1 +
 src-tauri/codeg-eui-core/Cargo.toml         |  24 ++++++
 src-tauri/codeg-eui-core/src/abi.rs         | 121 ++++++++++++++++++++++++++++
 src-tauri/codeg-eui-core/src/lib.rs         |   3 +
 src-tauri/codeg-eui-core/tests/abi_smoke.rs |  25 ++++++
 18 files changed, 538 insertions(+)

## Diff
diff --git a/.gitmodules b/.gitmodules
index d1bbe67e..930543b3 100644
--- a/.gitmodules
+++ b/.gitmodules
@@ -1,4 +1,7 @@
 [submodule "src-tauri/vendor/codex-acp"]
 	path = src-tauri/vendor/codex-acp
 	url = https://github.com/icannotwait/codex-acp.git
 	branch = codex/codex-acp-cli-runtime
+[submodule "codeg-eui/third_party/EUI-NEO"]
+	path = codeg-eui/third_party/EUI-NEO
+	url = https://github.com/sudoevolve/EUI-NEO.git
diff --git a/codeg-eui/.gitignore b/codeg-eui/.gitignore
new file mode 100644
index 00000000..0d65b4e7
--- /dev/null
+++ b/codeg-eui/.gitignore
@@ -0,0 +1,4 @@
+/build/
+/build-*/
+/results/
+/screenshots/
diff --git a/codeg-eui/CMakeLists.txt b/codeg-eui/CMakeLists.txt
new file mode 100644
index 00000000..8e41471a
--- /dev/null
+++ b/codeg-eui/CMakeLists.txt
@@ -0,0 +1,52 @@
+cmake_minimum_required(VERSION 3.20)
+project(codeg_eui LANGUAGES C CXX)
+
+set(CMAKE_CXX_STANDARD 17)
+set(CMAKE_CXX_STANDARD_REQUIRED ON)
+option(CODEG_EUI_CONTRACTS_ONLY "Build ABI tests without EUI/native deps" OFF)
+option(CODEG_EUI_ABI_LINK_TESTS "Link black-box ABI tests to the Rust archive" OFF)
+set(CODEG_EUI_RUST_LIB "" CACHE FILEPATH "Absolute libcodeg_eui_core.a path")
+
+enable_testing()
+
+function(codeg_eui_add_contract_test exact_name source)
+  set(target "${exact_name}_test")
+  add_executable(${target} "${source}" tests/test_main.cpp)
+  target_include_directories(${target} PRIVATE tests app app/bridge)
+  add_test(NAME "${exact_name}" COMMAND ${target})
+endfunction()
+
+codeg_eui_add_contract_test(codeg_eui_harness_self tests/harness_self_test.cpp)
+codeg_eui_add_contract_test(codeg_eui_abi_layout tests/abi_layout_test.cpp)
+
+if(CODEG_EUI_ABI_LINK_TESTS OR NOT CODEG_EUI_CONTRACTS_ONLY)
+  if(NOT IS_ABSOLUTE "${CODEG_EUI_RUST_LIB}" OR
+     NOT EXISTS "${CODEG_EUI_RUST_LIB}")
+    message(FATAL_ERROR "CODEG_EUI_RUST_LIB must name an existing absolute archive")
+  endif()
+  find_package(Threads REQUIRED)
+  add_library(codeg_eui_core STATIC IMPORTED GLOBAL)
+  set_target_properties(codeg_eui_core PROPERTIES
+    IMPORTED_LOCATION "${CODEG_EUI_RUST_LIB}")
+endif()
+
+if(CODEG_EUI_ABI_LINK_TESTS)
+  codeg_eui_add_contract_test(
+    codeg_eui_shutdown_drain tests/shutdown_drain_test.cpp)
+  target_compile_definitions(codeg_eui_shutdown_drain_test PRIVATE
+    CODEG_EUI_TEST_HOOKS=1)
+  target_include_directories(codeg_eui_shutdown_drain_test PRIVATE app/bridge)
+  target_link_libraries(codeg_eui_shutdown_drain_test PRIVATE
+    codeg_eui_core Threads::Threads ${CMAKE_DL_LIBS} m)
+endif()
+
+if(NOT CODEG_EUI_CONTRACTS_ONLY)
+  add_subdirectory(third_party/EUI-NEO)
+  add_executable(codeg-eui
+    third_party/EUI-NEO/core/app/glfw_app_main.cpp
+    app/app.cpp)
+  target_include_directories(codeg-eui PRIVATE app/bridge)
+  target_link_libraries(codeg-eui PRIVATE
+    codeg_eui_core Threads::Threads ${CMAKE_DL_LIBS} m)
+  eui_neo_configure_app(codeg-eui)
+endif()
diff --git a/codeg-eui/app/app.cpp b/codeg-eui/app/app.cpp
new file mode 100644
index 00000000..d08410df
--- /dev/null
+++ b/codeg-eui/app/app.cpp
@@ -0,0 +1,75 @@
+#include "codeg_eui_bridge.h"
+#include "eui_neo.h"
+
+namespace app {
+namespace {
+
+class BridgeLifecycle final {
+public:
+    BridgeLifecycle()
+        : initStatus_(codeg_eui_init(nullptr, 0)) {}
+
+    ~BridgeLifecycle() {
+        if (initStatus_ != CODEG_EUI_OK ||
+            codeg_eui_begin_shutdown() != CODEG_EUI_OK) {
+            return;
+        }
+
+        CodegEuiFrame frame{};
+        while (codeg_eui_poll(&frame) == CODEG_EUI_OK) {
+            if (frame.shutdown_ready == 1) {
+                (void)codeg_eui_shutdown();
+                return;
+            }
+        }
+    }
+
+    int poll(CodegEuiFrame& frame) const {
+        if (initStatus_ != CODEG_EUI_OK) {
+            return initStatus_;
+        }
+        return codeg_eui_poll(&frame);
+    }
+
+private:
+    int initStatus_;
+};
+
+BridgeLifecycle& bridge() {
+    static BridgeLifecycle value;
+    return value;
+}
+
+}  // namespace
+
+const DslAppConfig& dslAppConfig() {
+    (void)bridge();
+    static const DslAppConfig config = DslAppConfig{}
+        .title("Codeg EUI Spike")
+        .pageId("codeg_eui_spike")
+        .clearColor({0.055f, 0.062f, 0.075f, 1.0f})
+        .windowSize(1180, 760)
+        .fps(60.0);
+    return config;
+}
+
+void compose(eui::Ui& ui, const eui::Screen& screen) {
+    CodegEuiFrame frame{};
+    (void)bridge().poll(frame);
+
+    ui.stack("hello.root")
+        .size(screen.width, screen.height)
+        .align(eui::Align::CENTER, eui::Align::CENTER)
+        .content([&] {
+            ui.text("hello.title")
+                .size(420.0f, 48.0f)
+                .text("Codeg EUI / bridge v1")
+                .fontSize(30.0f)
+                .lineHeight(40.0f)
+                .color({0.94f, 0.96f, 0.98f, 1.0f})
+                .build();
+        })
+        .build();
+}
+
+}  // namespace app
diff --git a/codeg-eui/app/bridge/codeg_eui_bridge.h b/codeg-eui/app/bridge/codeg_eui_bridge.h
new file mode 100644
index 00000000..9359d8e8
--- /dev/null
+++ b/codeg-eui/app/bridge/codeg_eui_bridge.h
@@ -0,0 +1,34 @@
+#pragma once
+
+#include <stddef.h>
+#include <stdint.h>
+
+#define CODEG_EUI_API_VERSION 1u
+#define CODEG_EUI_OK 0
+#define CODEG_EUI_ERR_INVALID_STATE 1
+#define CODEG_EUI_ERR_NULL_POINTER 2
+#define CODEG_EUI_ERR_NOT_READY 9
+
+typedef struct CodegEuiFrame {
+    uint32_t api_version;
+    uint32_t lifecycle_state;
+    uint64_t generation;
+    uint8_t shutdown_ready;
+    uint8_t reserved[7];
+} CodegEuiFrame;
+
+#if defined(__cplusplus)
+extern "C" {
+#endif
+
+uint32_t codeg_eui_api_version(void);
+int codeg_eui_init(const uint8_t* data_dir_utf8, size_t data_dir_len);
+int codeg_eui_poll(CodegEuiFrame* out);
+int codeg_eui_begin_shutdown(void);
+int codeg_eui_shutdown(void);
+
+#if defined(__cplusplus)
+}
+
+static_assert(sizeof(CodegEuiFrame) == 24, "CodegEuiFrame ABI drift");
+#endif
diff --git a/codeg-eui/scripts/build.sh b/codeg-eui/scripts/build.sh
new file mode 100755
index 00000000..3802d68f
--- /dev/null
+++ b/codeg-eui/scripts/build.sh
@@ -0,0 +1,34 @@
+#!/bin/sh
+set -eu
+
+script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd -P)
+repo_root=$(CDPATH= cd -- "$script_dir/../.." && pwd -P)
+
+if [ "$(uname -s)" != Linux ]; then
+  printf '%s\n' 'codeg-eui is supported only on Linux' >&2
+  exit 1
+fi
+
+eui_dir="$repo_root/codeg-eui/third_party/EUI-NEO"
+expected_eui_commit=cb70ea8bea263efa7805a40c07135df028ad44b1
+actual_eui_commit=$(git -C "$eui_dir" rev-parse HEAD 2>/dev/null || true)
+if [ "$actual_eui_commit" != "$expected_eui_commit" ]; then
+  printf 'EUI-NEO must be initialized at %s; found %s\n' \
+    "$expected_eui_commit" "${actual_eui_commit:-uninitialized}" >&2
+  exit 1
+fi
+
+cargo build \
+  --manifest-path "$repo_root/src-tauri/codeg-eui-core/Cargo.toml" \
+  --release
+
+rust_lib="$repo_root/src-tauri/codeg-eui-core/target/release/libcodeg_eui_core.a"
+build_dir="$repo_root/codeg-eui/build"
+cmake -S "$repo_root/codeg-eui" -B "$build_dir" \
+  -DCMAKE_BUILD_TYPE=Release \
+  -DEUI_WINDOW_BACKEND=glfw \
+  -DEUI_RENDER_BACKEND=opengl \
+  -DCODEG_EUI_RUST_LIB="$rust_lib"
+cmake --build "$build_dir" --parallel
+
+printf '%s\n' "$build_dir/codeg-eui"
diff --git a/codeg-eui/tests/abi_layout_test.cpp b/codeg-eui/tests/abi_layout_test.cpp
new file mode 100644
index 00000000..1f447f48
--- /dev/null
+++ b/codeg-eui/tests/abi_layout_test.cpp
@@ -0,0 +1,19 @@
+#include "codeg_eui_bridge.h"
+#include "test_harness.h"
+
+#include <cstddef>
+
+static_assert(sizeof(CodegEuiFrame) == 24, "CodegEuiFrame ABI size drift");
+static_assert(alignof(CodegEuiFrame) == 8, "CodegEuiFrame ABI alignment drift");
+static_assert(offsetof(CodegEuiFrame, generation) == 8,
+              "CodegEuiFrame generation offset drift");
+static_assert(offsetof(CodegEuiFrame, shutdown_ready) == 16,
+              "CodegEuiFrame shutdown_ready offset drift");
+
+TEST(AbiLayout, matches_v1_size_alignment_and_offsets) {
+    EXPECT_EQ(CODEG_EUI_API_VERSION, 1u);
+    EXPECT_EQ(sizeof(CodegEuiFrame), static_cast<std::size_t>(24));
+    EXPECT_EQ(offsetof(CodegEuiFrame, generation), static_cast<std::size_t>(8));
+    EXPECT_EQ(offsetof(CodegEuiFrame, shutdown_ready),
+              static_cast<std::size_t>(16));
+}
diff --git a/codeg-eui/tests/assert_ctest_red.sh b/codeg-eui/tests/assert_ctest_red.sh
new file mode 100755
index 00000000..f91b8e99
--- /dev/null
+++ b/codeg-eui/tests/assert_ctest_red.sh
@@ -0,0 +1,17 @@
+#!/bin/sh
+set -eu
+
+build_dir=$1
+exact_name=$2
+failed_case=$3
+script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd -P)
+"$script_dir/assert_ctest_registered.sh" "$build_dir" "$exact_name"
+set +e
+output=$(ctest --test-dir "$build_dir" -R "^${exact_name}$" \
+  --output-on-failure 2>&1)
+status=$?
+set -e
+test "$status" -ne 0
+printf '%s\n' "$output" | grep -F "[FAIL] $failed_case" >/dev/null
+printf '%s\n' "$output" |
+  grep -F '0% tests passed, 1 tests failed out of 1' >/dev/null
diff --git a/codeg-eui/tests/assert_ctest_registered.sh b/codeg-eui/tests/assert_ctest_registered.sh
new file mode 100755
index 00000000..fcf614f2
--- /dev/null
+++ b/codeg-eui/tests/assert_ctest_registered.sh
@@ -0,0 +1,14 @@
+#!/bin/sh
+set -eu
+
+build_dir=$1
+shift
+for exact_name do
+  count=$(ctest --test-dir "$build_dir" -N -R "^${exact_name}$" |
+    awk '/Total Tests:/ { print $3 }')
+  test "$count" = 1 || {
+    printf 'expected one CTest named %s, found %s\n' \
+      "$exact_name" "${count:-0}" >&2
+    exit 1
+  }
+done
diff --git a/codeg-eui/tests/harness_self_test.cpp b/codeg-eui/tests/harness_self_test.cpp
new file mode 100644
index 00000000..ad71475d
--- /dev/null
+++ b/codeg-eui/tests/harness_self_test.cpp
@@ -0,0 +1,9 @@
+#include "test_harness.h"
+
+TEST(Harness, version_and_plan_assertions_are_available) {
+    EXPECT_EQ(CODEG_EUI_TEST_HARNESS_VERSION, 1);
+    EXPECT_TRUE(true);
+    EXPECT_FALSE(false);
+    EXPECT_GE(2, 1);
+    ASSERT_EQ(4, 2 + 2);
+}
diff --git a/codeg-eui/tests/test_harness.h b/codeg-eui/tests/test_harness.h
new file mode 100644
index 00000000..41384a70
--- /dev/null
+++ b/codeg-eui/tests/test_harness.h
@@ -0,0 +1,97 @@
+#pragma once
+
+#include <exception>
+#include <iostream>
+#include <string>
+#include <utility>
+#include <vector>
+
+#define CODEG_EUI_TEST_HARNESS_VERSION 1
+
+namespace codeg_eui::test {
+
+struct Case {
+    const char* name;
+    void (*body)();
+};
+
+struct AbortCase final {};
+
+inline std::vector<Case>& registry() {
+    static std::vector<Case> value;
+    return value;
+}
+
+inline int& failures() {
+    static int value = 0;
+    return value;
+}
+
+struct Registrar {
+    Registrar(const char* name, void (*body)()) {
+        registry().push_back({name, body});
+    }
+};
+
+inline void expect(bool ok, const char* expression, const char* file, int line) {
+    if (!ok) {
+        ++failures();
+        std::cerr << file << ':' << line << ": " << expression << '\n';
+    }
+}
+
+template <class A, class B>
+inline void expectEq(const A& actual,
+                     const B& expected,
+                     const char* expression,
+                     const char* file,
+                     int line,
+                     bool fatal) {
+    if (!(actual == expected)) {
+        expect(false, expression, file, line);
+        if (fatal) {
+            throw AbortCase{};
+        }
+    }
+}
+
+inline int runAll() {
+    int failedCases = 0;
+    for (const Case& test : registry()) {
+        const int before = failures();
+        try {
+            test.body();
+        } catch (const AbortCase&) {
+        } catch (const std::exception& error) {
+            expect(false, error.what(), __FILE__, __LINE__);
+        }
+        if (failures() != before) {
+            ++failedCases;
+            std::cerr << "[FAIL] " << test.name << '\n';
+        } else {
+            std::cout << "[PASS] " << test.name << '\n';
+        }
+    }
+    return failedCases == 0 ? 0 : 1;
+}
+
+}  // namespace codeg_eui::test
+
+#define TEST(suite, name)                                                        \
+    static void suite##_##name();                                                \
+    static ::codeg_eui::test::Registrar suite##_##name##_registrar(              \
+        #suite "." #name, &suite##_##name);                                     \
+    static void suite##_##name()
+#define EXPECT_TRUE(value)                                                       \
+    ::codeg_eui::test::expect(!!(value), #value, __FILE__, __LINE__)
+#define EXPECT_FALSE(value)                                                      \
+    ::codeg_eui::test::expect(!(value), "!(" #value ")", __FILE__, __LINE__)
+#define EXPECT_EQ(actual, expected)                                              \
+    ::codeg_eui::test::expectEq(                                                 \
+        (actual), (expected), #actual " == " #expected, __FILE__, __LINE__, false)
+#define EXPECT_GE(actual, expected)                                              \
+    ::codeg_eui::test::expect(                                                   \
+        ((actual) >= (expected)), #actual " >= " #expected, __FILE__, __LINE__)
+#define ASSERT_EQ(actual, expected)                                              \
+    ::codeg_eui::test::expectEq(                                                 \
+        (actual), (expected), #actual " == " #expected, __FILE__, __LINE__, true)
diff --git a/codeg-eui/tests/test_main.cpp b/codeg-eui/tests/test_main.cpp
new file mode 100644
index 00000000..7df53aac
--- /dev/null
+++ b/codeg-eui/tests/test_main.cpp
@@ -0,0 +1,5 @@
+#include "test_harness.h"
+
+int main() {
+    return codeg_eui::test::runAll();
+}
diff --git a/codeg-eui/third_party/EUI-NEO b/codeg-eui/third_party/EUI-NEO
new file mode 160000
index 00000000..cb70ea8b
--- /dev/null
+++ b/codeg-eui/third_party/EUI-NEO
@@ -0,0 +1 @@
+Subproject commit cb70ea8bea263efa7805a40c07135df028ad44b1
diff --git a/src-tauri/codeg-eui-core/.gitignore b/src-tauri/codeg-eui-core/.gitignore
new file mode 100644
index 00000000..b83d2226
--- /dev/null
+++ b/src-tauri/codeg-eui-core/.gitignore
@@ -0,0 +1 @@
+/target/
diff --git a/src-tauri/codeg-eui-core/Cargo.toml b/src-tauri/codeg-eui-core/Cargo.toml
new file mode 100644
index 00000000..ca67ed33
--- /dev/null
+++ b/src-tauri/codeg-eui-core/Cargo.toml
@@ -0,0 +1,24 @@
+[package]
+name = "codeg-eui-core"
+version = "0.1.0"
+edition = "2021"
+publish = false
+
+[lib]
+name = "codeg_eui_core"
+crate-type = ["staticlib", "rlib"]
+
+[dependencies]
+codeg = { package = "codeg", path = "..", default-features = false }
+serde = { version = "1", features = ["derive"] }
+serde_json = "1"
+tokio = { version = "1", features = ["rt-multi-thread", "sync", "time"] }
+thiserror = "2"
+
+[dev-dependencies]
+temp-env = "0.3"
+tempfile = "3"
+
+[patch.crates-io]
+sacp-tokio = { path = "../vendor/sacp-tokio" }
+kill_tree = { path = "../vendor/kill_tree" }
diff --git a/src-tauri/codeg-eui-core/src/abi.rs b/src-tauri/codeg-eui-core/src/abi.rs
new file mode 100644
index 00000000..01e33c26
--- /dev/null
+++ b/src-tauri/codeg-eui-core/src/abi.rs
@@ -0,0 +1,121 @@
+use std::panic::{catch_unwind, AssertUnwindSafe};
+use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
+
+pub const CODEG_EUI_API_VERSION: u32 = 1;
+pub const CODEG_EUI_OK: i32 = 0;
+pub const CODEG_EUI_ERR_INVALID_STATE: i32 = 1;
+pub const CODEG_EUI_ERR_NULL_POINTER: i32 = 2;
+pub const CODEG_EUI_ERR_NOT_READY: i32 = 9;
+
+const LIFECYCLE_UNINITIALIZED: u32 = 0;
+const LIFECYCLE_STARTING: u32 = 1;
+const LIFECYCLE_RUNNING: u32 = 2;
+const LIFECYCLE_STOPPING: u32 = 3;
+const LIFECYCLE_STOPPED: u32 = 4;
+
+static LIFECYCLE: AtomicU32 = AtomicU32::new(LIFECYCLE_UNINITIALIZED);
+static GENERATION: AtomicU64 = AtomicU64::new(0);
+static SHUTDOWN_READY: AtomicBool = AtomicBool::new(false);
+
+#[repr(C)]
+#[derive(Clone, Copy, Default)]
+pub struct CodegEuiFrame {
+    pub api_version: u32,
+    pub lifecycle_state: u32,
+    pub generation: u64,
+    pub shutdown_ready: u8,
+    pub _reserved: [u8; 7],
+}
+
+fn ffi_status(operation: impl FnOnce() -> i32) -> i32 {
+    catch_unwind(AssertUnwindSafe(operation)).unwrap_or(CODEG_EUI_ERR_INVALID_STATE)
+}
+
+#[no_mangle]
+pub extern "C" fn codeg_eui_api_version() -> u32 {
+    catch_unwind(|| CODEG_EUI_API_VERSION).unwrap_or_default()
+}
+
+#[no_mangle]
+pub extern "C" fn codeg_eui_init(data_dir_utf8: *const u8, data_dir_len: usize) -> i32 {
+    ffi_status(|| {
+        if data_dir_utf8.is_null() && data_dir_len > 0 {
+            return CODEG_EUI_ERR_NULL_POINTER;
+        }
+
+        let current = LIFECYCLE.load(Ordering::Acquire);
+        if current != LIFECYCLE_UNINITIALIZED && current != LIFECYCLE_STOPPED {
+            return CODEG_EUI_ERR_INVALID_STATE;
+        }
+
+        LIFECYCLE.store(LIFECYCLE_STARTING, Ordering::Release);
+        GENERATION.store(0, Ordering::Release);
+        SHUTDOWN_READY.store(false, Ordering::Release);
+        LIFECYCLE.store(LIFECYCLE_RUNNING, Ordering::Release);
+        CODEG_EUI_OK
+    })
+}
+
+#[no_mangle]
+pub extern "C" fn codeg_eui_poll(out: *mut CodegEuiFrame) -> i32 {
+    ffi_status(|| {
+        if out.is_null() {
+            return CODEG_EUI_ERR_NULL_POINTER;
+        }
+
+        let lifecycle_state = LIFECYCLE.load(Ordering::Acquire);
+        if lifecycle_state != LIFECYCLE_RUNNING && lifecycle_state != LIFECYCLE_STOPPING {
+            return CODEG_EUI_ERR_INVALID_STATE;
+        }
+
+        let shutdown_ready = lifecycle_state == LIFECYCLE_STOPPING;
+        let frame = CodegEuiFrame {
+            api_version: CODEG_EUI_API_VERSION,
+            lifecycle_state,
+            generation: GENERATION.fetch_add(1, Ordering::AcqRel) + 1,
+            shutdown_ready: u8::from(shutdown_ready),
+            _reserved: [0; 7],
+        };
+
+        // The caller owns `out` and must provide writable storage for one frame.
+        unsafe { out.write(frame) };
+        if shutdown_ready {
+            SHUTDOWN_READY.store(true, Ordering::Release);
+        }
+        CODEG_EUI_OK
+    })
+}
+
+#[no_mangle]
+pub extern "C" fn codeg_eui_begin_shutdown() -> i32 {
+    ffi_status(|| {
+        if LIFECYCLE
+            .compare_exchange(
+                LIFECYCLE_RUNNING,
+                LIFECYCLE_STOPPING,
+                Ordering::AcqRel,
+                Ordering::Acquire,
+            )
+            .is_err()
+        {
+            return CODEG_EUI_ERR_INVALID_STATE;
+        }
+        SHUTDOWN_READY.store(false, Ordering::Release);
+        CODEG_EUI_OK
+    })
+}
+
+#[no_mangle]
+pub extern "C" fn codeg_eui_shutdown() -> i32 {
+    ffi_status(|| {
+        if LIFECYCLE.load(Ordering::Acquire) != LIFECYCLE_STOPPING
+            || !SHUTDOWN_READY.load(Ordering::Acquire)
+        {
+            return CODEG_EUI_ERR_INVALID_STATE;
+        }
+
+        SHUTDOWN_READY.store(false, Ordering::Release);
+        LIFECYCLE.store(LIFECYCLE_STOPPED, Ordering::Release);
+        CODEG_EUI_OK
+    })
+}
diff --git a/src-tauri/codeg-eui-core/src/lib.rs b/src-tauri/codeg-eui-core/src/lib.rs
new file mode 100644
index 00000000..70943065
--- /dev/null
+++ b/src-tauri/codeg-eui-core/src/lib.rs
@@ -0,0 +1,3 @@
+mod abi;
+
+pub use abi::*;
diff --git a/src-tauri/codeg-eui-core/tests/abi_smoke.rs b/src-tauri/codeg-eui-core/tests/abi_smoke.rs
new file mode 100644
index 00000000..d67d5aa7
--- /dev/null
+++ b/src-tauri/codeg-eui-core/tests/abi_smoke.rs
@@ -0,0 +1,25 @@
+use codeg_eui_core::{
+    codeg_eui_api_version, codeg_eui_begin_shutdown, codeg_eui_init, codeg_eui_poll,
+    codeg_eui_shutdown, CodegEuiFrame, CODEG_EUI_API_VERSION, CODEG_EUI_ERR_INVALID_STATE,
+    CODEG_EUI_ERR_NULL_POINTER, CODEG_EUI_OK,
+};
+
+#[test]
+fn abi_version_and_null_poll_are_stable() {
+    assert_eq!(codeg_eui_api_version(), CODEG_EUI_API_VERSION);
+    assert_eq!(CODEG_EUI_API_VERSION, 1);
+    assert_eq!(
+        codeg_eui_poll(std::ptr::null_mut::<CodegEuiFrame>()),
+        CODEG_EUI_ERR_NULL_POINTER
+    );
+
+    assert_eq!(codeg_eui_init(std::ptr::null(), 0), CODEG_EUI_OK);
+    assert_eq!(codeg_eui_shutdown(), CODEG_EUI_ERR_INVALID_STATE);
+    assert_eq!(codeg_eui_begin_shutdown(), CODEG_EUI_OK);
+
+    let mut frame = CodegEuiFrame::default();
+    assert_eq!(codeg_eui_poll(&mut frame), CODEG_EUI_OK);
+    assert_eq!(frame.api_version, CODEG_EUI_API_VERSION);
+    assert_eq!(frame.shutdown_ready, 1);
+    assert_eq!(codeg_eui_shutdown(), CODEG_EUI_OK);
+}
