# Task 1 Brief

Read this first — requirements verbatim from the approved Plan.

### Task 1: Establish the Optional EUI Build Spine and Hello Window

**Milestone:** M0.

**Files:**

- Modify: `.gitmodules`
- Create gitlink: `codeg-eui/third_party/EUI-NEO`
- Create: `codeg-eui/.gitignore`
- Create: `src-tauri/codeg-eui-core/.gitignore`
- Create: `src-tauri/codeg-eui-core/Cargo.toml`
- Create: `src-tauri/codeg-eui-core/src/lib.rs`
- Create: `src-tauri/codeg-eui-core/src/abi.rs`
- Create: `src-tauri/codeg-eui-core/tests/abi_smoke.rs`
- Create: `codeg-eui/CMakeLists.txt`
- Create: `codeg-eui/app/app.cpp`
- Create: `codeg-eui/app/bridge/codeg_eui_bridge.h`
- Create: `codeg-eui/tests/test_harness.h`
- Create: `codeg-eui/tests/test_main.cpp`
- Create: `codeg-eui/tests/harness_self_test.cpp`
- Create: `codeg-eui/tests/assert_ctest_registered.sh`
- Create: `codeg-eui/tests/assert_ctest_red.sh`
- Create: `codeg-eui/tests/abi_layout_test.cpp`
- Create: `codeg-eui/scripts/build.sh`

**Interfaces:**

- Consumes: `codeg_lib` from `src-tauri` with `default-features = false`; EUI `glfw_app_main.cpp`, `eui_neo_configure_app`, and `app::{dslAppConfig,compose}`.
- Produces: ABI version constant `CODEG_EUI_API_VERSION=1`; exported `codeg_eui_api_version`, `codeg_eui_init`, `codeg_eui_poll`, initial `codeg_eui_begin_shutdown`, and `codeg_eui_shutdown`; CMake target `codeg-eui`; Rust artifact `libcodeg_eui_core.a`; repository-owned C++ harness v1 and `codeg_eui_add_contract_test`; explicit generated-output ignores.
- Preserves: no root Cargo workspace and no reference from `src-tauri/Cargo.toml`, `package.json`, or default CMake/build paths to the new crate or submodule.

**Task Routing Matrix:**

| task_index | title | files/modules | hard triggers evidence | soft signals evidence + soft total | final risk level + reason | implementer agent | reviewer set | policy version |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 1 | Establish the optional EUI build spine and hello window | submodule, CMake, bridge header, staticlib skeleton, app entry | `unsafe_ffi`: exported layout/symbols; `public_compatibility`: ABI v1 starts here | `cross_runtime_or_process=2`, `multiple_ownership_modules=1`, `shared_interface=1`, `dependency_or_build=1`; total `5` | `high`: hard ABI triggers | `codex` | `codex (separate) + grok` | `b2d_task_risk_v1` |

- [ ] **Step 1: Add the pinned EUI-NEO submodule**

```bash
git submodule add https://github.com/sudoevolve/EUI-NEO.git codeg-eui/third_party/EUI-NEO
git -C codeg-eui/third_party/EUI-NEO checkout cb70ea8bea263efa7805a40c07135df028ad44b1
test "$(git -C codeg-eui/third_party/EUI-NEO rev-parse HEAD)" = cb70ea8bea263efa7805a40c07135df028ad44b1
git diff --submodule=short -- .gitmodules codeg-eui/third_party/EUI-NEO
```

Expected: `.gitmodules` names the GitHub URL and the gitlink points exactly at the peeled `v0.5.5` commit.

- [ ] **Step 2: Write the failing Rust ABI smoke test**

Create `src-tauri/codeg-eui-core/tests/abi_smoke.rs`:

```rust
use codeg_eui_core::{
    codeg_eui_api_version, codeg_eui_poll, CodegEuiFrame, CODEG_EUI_API_VERSION,
    CODEG_EUI_ERR_NULL_POINTER,
};

#[test]
fn abi_version_and_null_poll_are_stable() {
    assert_eq!(codeg_eui_api_version(), CODEG_EUI_API_VERSION);
    assert_eq!(CODEG_EUI_API_VERSION, 1);
    assert_eq!(codeg_eui_poll(std::ptr::null_mut::<CodegEuiFrame>()),
               CODEG_EUI_ERR_NULL_POINTER);
}
```

- [ ] **Step 3: Run the Rust test to verify RED**

```bash
cargo test --manifest-path src-tauri/codeg-eui-core/Cargo.toml --test abi_smoke
```

Expected: FAIL because the crate and ABI symbols do not exist.

- [ ] **Step 4: Add the independent staticlib manifest and ABI skeleton**

Create `src-tauri/codeg-eui-core/Cargo.toml` with this dependency boundary:

```toml
[package]
name = "codeg-eui-core"
version = "0.1.0"
edition = "2021"
publish = false

[lib]
name = "codeg_eui_core"
crate-type = ["staticlib", "rlib"]

[dependencies]
codeg = { package = "codeg", path = "..", default-features = false }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
tokio = { version = "1", features = ["rt-multi-thread", "sync", "time"] }
thiserror = "2"

[dev-dependencies]
temp-env = "0.3"
tempfile = "3"
```

Define the initial stable surface in `abi.rs` and re-export it from `lib.rs`:

```rust
pub const CODEG_EUI_API_VERSION: u32 = 1;
pub const CODEG_EUI_OK: i32 = 0;
pub const CODEG_EUI_ERR_INVALID_STATE: i32 = 1;
pub const CODEG_EUI_ERR_NULL_POINTER: i32 = 2;
pub const CODEG_EUI_ERR_NOT_READY: i32 = 9;

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct CodegEuiFrame {
    pub api_version: u32,
    pub lifecycle_state: u32,
    pub generation: u64,
    pub shutdown_ready: u8,
    pub _reserved: [u8; 7],
}

#[no_mangle]
pub extern "C" fn codeg_eui_api_version() -> u32 {
    CODEG_EUI_API_VERSION
}

#[no_mangle]
pub extern "C" fn codeg_eui_poll(out: *mut CodegEuiFrame) -> i32 {
    if out.is_null() {
        return CODEG_EUI_ERR_NULL_POINTER;
    }
    CODEG_EUI_ERR_NOT_READY
}
```

The M0 lifecycle already follows the public call order used by the final bridge: `init` enters `running`, `begin_shutdown` enters `stopping` and rejects new work, the first successful stopping `poll` returns `shutdown_ready=1`, and only then does `shutdown` enter `stopped`. Because M0 accepts no async requests, that stopping poll is immediately ready. Task 3 replaces the minimal process-local state with the full worker/drain state machine without changing these symbols or weakening their ordering.

- [ ] **Step 5: Add the self-contained C++ test harness before any C++ RED**

Create `test_harness.h` as a dependency-free C++17 registry. It records an
expectation failure without aborting the process, aborts only the current test
for `ASSERT_EQ`, prints `[FAIL] Suite.Name` for a failing case, and returns `1`
from the shared runner when any case fails:

```cpp
#pragma once
#include <exception>
#include <iostream>
#include <string>
#include <utility>
#include <vector>

#define CODEG_EUI_TEST_HARNESS_VERSION 1

namespace codeg_eui::test {
struct Case { const char* name; void (*body)(); };
struct AbortCase final {};
inline std::vector<Case>& registry() { static std::vector<Case> value; return value; }
inline int& failures() { static int value = 0; return value; }
struct Registrar {
  Registrar(const char* name, void (*body)()) { registry().push_back({name, body}); }
};
inline void expect(bool ok, const char* expression, const char* file, int line) {
  if (!ok) { ++failures(); std::cerr << file << ':' << line << ": " << expression << '\n'; }
}
template <class A, class B>
inline void expectEq(const A& actual, const B& expected, const char* expression,
                     const char* file, int line, bool fatal) {
  if (!(actual == expected)) {
    expect(false, expression, file, line);
    if (fatal) throw AbortCase{};
  }
}
inline int runAll() {
  int failedCases = 0;
  for (const Case& test : registry()) {
    const int before = failures();
    try { test.body(); }
    catch (const AbortCase&) {}
    catch (const std::exception& error) {
      expect(false, error.what(), __FILE__, __LINE__);
    }
    if (failures() != before) {
      ++failedCases;
      std::cerr << "[FAIL] " << test.name << '\n';
    } else {
      std::cout << "[PASS] " << test.name << '\n';
    }
  }
  return failedCases == 0 ? 0 : 1;
}
}  // namespace codeg_eui::test

#define TEST(suite, name) \
  static void suite##_##name(); \
  static ::codeg_eui::test::Registrar suite##_##name##_registrar( \
      #suite "." #name, &suite##_##name); \
  static void suite##_##name()
#define EXPECT_TRUE(value) ::codeg_eui::test::expect(!!(value), #value, __FILE__, __LINE__)
#define EXPECT_FALSE(value) ::codeg_eui::test::expect(!(value), "!(" #value ")", __FILE__, __LINE__)
#define EXPECT_EQ(actual, expected) ::codeg_eui::test::expectEq((actual), (expected), #actual " == " #expected, __FILE__, __LINE__, false)
#define EXPECT_GE(actual, expected) ::codeg_eui::test::expect(((actual) >= (expected)), #actual " >= " #expected, __FILE__, __LINE__)
#define ASSERT_EQ(actual, expected) ::codeg_eui::test::expectEq((actual), (expected), #actual " == " #expected, __FILE__, __LINE__, true)
```

Create the shared main:

```cpp
#include "test_harness.h"
int main() { return codeg_eui::test::runAll(); }
```

`harness_self_test.cpp` includes the header, asserts version `1`, and exercises
all five macros with passing values:

```cpp
#include "test_harness.h"
TEST(Harness, version_and_plan_assertions_are_available) {
  EXPECT_EQ(CODEG_EUI_TEST_HARNESS_VERSION, 1);
  EXPECT_TRUE(true);
  EXPECT_FALSE(false);
  EXPECT_GE(2, 1);
  ASSERT_EQ(4, 2 + 2);
}
```

Create `assert_ctest_registered.sh`:

```bash
#!/bin/sh
set -eu
build_dir=$1
shift
for exact_name do
  count=$(ctest --test-dir "$build_dir" -N -R "^${exact_name}$" |
    awk '/Total Tests:/ { print $3 }')
  test "$count" = 1 || {
    printf 'expected one CTest named %s, found %s\n' "$exact_name" "${count:-0}" >&2
    exit 1
  }
done
```

Create `assert_ctest_red.sh` so every RED command proves both selection and the
specific failed behavior while returning success to the outer evidence script:

```bash
#!/bin/sh
set -eu
build_dir=$1
exact_name=$2
failed_case=$3
script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd -P)
"$script_dir/assert_ctest_registered.sh" "$build_dir" "$exact_name"
set +e
output=$(ctest --test-dir "$build_dir" -R "^${exact_name}$" --output-on-failure 2>&1)
status=$?
set -e
test "$status" -ne 0
printf '%s\n' "$output" | grep -F "[FAIL] $failed_case" >/dev/null
printf '%s\n' "$output" | grep -F '0% tests passed, 1 tests failed out of 1' >/dev/null
```

Mark both scripts executable:

```bash
chmod +x codeg-eui/tests/assert_ctest_registered.sh \
  codeg-eui/tests/assert_ctest_red.sh
```

This is a repository-owned harness pin, not an external dependency: changing
its version or semantics requires an explicit Plan/contract review.

- [ ] **Step 6: Mirror ABI v1 in C and write the headless layout test**

The header must use only fixed-width C types and include compile-time assertions:

```c
#define CODEG_EUI_API_VERSION 1u
#define CODEG_EUI_OK 0
#define CODEG_EUI_ERR_INVALID_STATE 1
#define CODEG_EUI_ERR_NULL_POINTER 2
#define CODEG_EUI_ERR_NOT_READY 9

typedef struct CodegEuiFrame {
  uint32_t api_version;
  uint32_t lifecycle_state;
  uint64_t generation;
  uint8_t shutdown_ready;
  uint8_t reserved[7];
} CodegEuiFrame;

uint32_t codeg_eui_api_version(void);
int codeg_eui_init(const uint8_t *data_dir_utf8, size_t data_dir_len);
int codeg_eui_poll(CodegEuiFrame *out);
int codeg_eui_begin_shutdown(void);
int codeg_eui_shutdown(void);

#if defined(__cplusplus)
static_assert(sizeof(CodegEuiFrame) == 24, "CodegEuiFrame ABI drift");
#endif
```

`abi_layout_test.cpp` includes `test_harness.h`, has no local `main`, retains
the compile-time `static_assert`s, and registers
`TEST(AbiLayout, matches_v1_size_alignment_and_offsets)` with runtime
`EXPECT_EQ` checks for version `1`, size `24`, `offsetof(generation)==8`, and
`offsetof(shutdown_ready)==16` without linking the EUI application. The Rust
smoke test also drives `init -> begin_shutdown -> stopping poll with
shutdown_ready=1 -> shutdown` and rejects final shutdown before the ready poll.

- [ ] **Step 7: Add CMake test registration and the hello window**

Use the EUI integration contract exactly:

```cmake
cmake_minimum_required(VERSION 3.20)
project(codeg_eui LANGUAGES C CXX)
set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
option(CODEG_EUI_CONTRACTS_ONLY "Build ABI tests without EUI/native deps" OFF)
option(CODEG_EUI_ABI_LINK_TESTS "Link black-box ABI tests to the Rust archive" OFF)
set(CODEG_EUI_RUST_LIB "" CACHE FILEPATH "Absolute libcodeg_eui_core.a path")

enable_testing()
function(codeg_eui_add_contract_test exact_name source)
  set(target "${exact_name}_test")
  add_executable(${target} "${source}" tests/test_main.cpp)
  target_include_directories(${target} PRIVATE tests app app/bridge)
  add_test(NAME "${exact_name}" COMMAND ${target})
endfunction()

codeg_eui_add_contract_test(codeg_eui_harness_self tests/harness_self_test.cpp)
codeg_eui_add_contract_test(codeg_eui_abi_layout tests/abi_layout_test.cpp)

if(CODEG_EUI_ABI_LINK_TESTS OR NOT CODEG_EUI_CONTRACTS_ONLY)
  if(NOT IS_ABSOLUTE "${CODEG_EUI_RUST_LIB}" OR
     NOT EXISTS "${CODEG_EUI_RUST_LIB}")
    message(FATAL_ERROR "CODEG_EUI_RUST_LIB must name an existing absolute archive")
  endif()
  find_package(Threads REQUIRED)
  add_library(codeg_eui_core STATIC IMPORTED GLOBAL)
  set_target_properties(codeg_eui_core PROPERTIES
    IMPORTED_LOCATION "${CODEG_EUI_RUST_LIB}")
endif()

if(CODEG_EUI_ABI_LINK_TESTS)
  codeg_eui_add_contract_test(
    codeg_eui_shutdown_drain tests/shutdown_drain_test.cpp)
  target_compile_definitions(codeg_eui_shutdown_drain_test PRIVATE
    CODEG_EUI_TEST_HOOKS=1)
  target_include_directories(codeg_eui_shutdown_drain_test PRIVATE app/bridge)
  target_link_libraries(codeg_eui_shutdown_drain_test PRIVATE
    codeg_eui_core Threads::Threads ${CMAKE_DL_LIBS} m)
endif()

if(NOT CODEG_EUI_CONTRACTS_ONLY)
  add_subdirectory(third_party/EUI-NEO)
  add_executable(codeg-eui
    third_party/EUI-NEO/core/app/glfw_app_main.cpp
    app/app.cpp)
  target_include_directories(codeg-eui PRIVATE app/bridge)
  target_link_libraries(codeg-eui PRIVATE
    codeg_eui_core Threads::Threads ${CMAKE_DL_LIBS} m)
  eui_neo_configure_app(codeg-eui)
endif()
```

`app.cpp` defines a 1180x760, 60 fps `Codeg EUI Spike` window, calls `codeg_eui_init(nullptr, 0)` once before rendering, calls `codeg_eui_poll` during compose, draws `Codeg EUI / bridge v1`, and owns an RAII guard that calls `codeg_eui_begin_shutdown`, polls until the minimal frame exposes `shutdown_ready=1`, then calls `codeg_eui_shutdown`. Task 3 expands the same guard to dispatch terminal completions while draining.

Create exact ignore files before any build:

```gitignore
# codeg-eui/.gitignore
/build/
/build-*/
/results/
/screenshots/
```

```gitignore
# src-tauri/codeg-eui-core/.gitignore
/target/
```

- [ ] **Step 8: Add the deterministic build orchestrator**

`codeg-eui/scripts/build.sh` must use `set -eu`, resolve the absolute repository root from the script path, reject non-Linux hosts, verify the submodule commit, build `codeg-eui-core --release`, pass `-DCODEG_EUI_RUST_LIB="${repo_root}/src-tauri/codeg-eui-core/target/release/libcodeg_eui_core.a"`, and print the absolute `codeg-eui` binary path as its final output line. It must not run `git submodule update` implicitly or mutate the pin.

- [ ] **Step 9: Verify both exact C++ registrations and run the prepared-host build**

```bash
cargo test --manifest-path src-tauri/codeg-eui-core/Cargo.toml --test abi_smoke
cmake -S codeg-eui -B codeg-eui/build-contract -DCODEG_EUI_CONTRACTS_ONLY=ON
cmake --build codeg-eui/build-contract --parallel
codeg-eui/tests/assert_ctest_registered.sh codeg-eui/build-contract \
  codeg_eui_harness_self codeg_eui_abi_layout
ctest --test-dir codeg-eui/build-contract -R '^codeg_eui_harness_self$' --output-on-failure
ctest --test-dir codeg-eui/build-contract -R '^codeg_eui_abi_layout$' --output-on-failure
ctest --test-dir codeg-eui/build-contract --output-on-failure
codeg-eui/scripts/build.sh
```

Expected: Rust and headless C++ tests pass. On a Linux host with CMake, a C++17 compiler, GLFW/OpenGL development libraries, and initialized submodules, `build.sh` produces a runnable hello window. Missing native packages are recorded in the review package and do not weaken the Rust/headless gates.

- [ ] **Step 10: Commit and prepare the Task 1 review package**

```bash
git add --dry-run -- .gitmodules codeg-eui/.gitignore codeg-eui/CMakeLists.txt codeg-eui/app/app.cpp codeg-eui/app/bridge/codeg_eui_bridge.h codeg-eui/scripts/build.sh codeg-eui/tests/test_harness.h codeg-eui/tests/test_main.cpp codeg-eui/tests/harness_self_test.cpp codeg-eui/tests/assert_ctest_registered.sh codeg-eui/tests/assert_ctest_red.sh codeg-eui/tests/abi_layout_test.cpp codeg-eui/third_party/EUI-NEO src-tauri/codeg-eui-core/.gitignore src-tauri/codeg-eui-core/Cargo.toml src-tauri/codeg-eui-core/src/lib.rs src-tauri/codeg-eui-core/src/abi.rs src-tauri/codeg-eui-core/tests/abi_smoke.rs
git add -- .gitmodules codeg-eui/.gitignore codeg-eui/CMakeLists.txt codeg-eui/app/app.cpp codeg-eui/app/bridge/codeg_eui_bridge.h codeg-eui/scripts/build.sh codeg-eui/tests/test_harness.h codeg-eui/tests/test_main.cpp codeg-eui/tests/harness_self_test.cpp codeg-eui/tests/assert_ctest_registered.sh codeg-eui/tests/assert_ctest_red.sh codeg-eui/tests/abi_layout_test.cpp codeg-eui/third_party/EUI-NEO src-tauri/codeg-eui-core/.gitignore src-tauri/codeg-eui-core/Cargo.toml src-tauri/codeg-eui-core/src/lib.rs src-tauri/codeg-eui-core/src/abi.rs src-tauri/codeg-eui-core/tests/abi_smoke.rs
git diff --cached --name-only
git status --short --untracked-files=all
git commit -m "feat(eui): add optional native shell build spine"
git show --stat --oneline HEAD
git diff HEAD^ -- .gitmodules codeg-eui src-tauri/codeg-eui-core
```

Expected package: one commit with the exact submodule pin, independent crate, ABI v1 skeleton, CMake target, hello app, self-contained harness v1, exact CTest registration guard, headless tests, and ignore ownership. The staged list contains no `target`, `build`, `results`, screenshot, binary, or object path. Attach Task 1 command evidence and route it to both high-risk reviewers, then continue directly to Task 2.

