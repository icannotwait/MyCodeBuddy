# Task 3 Brief

### Task 3: Implement Lifecycle, Async Requests, Completions, and Immutable Frames

**Milestone:** M0/M1 bridge completion.

**Files:**

- Expand: `src-tauri/codeg-eui-core/src/abi.rs`
- Expand: `src-tauri/codeg-eui-core/src/runtime.rs`
- Create: `src-tauri/codeg-eui-core/src/model.rs`
- Create: `src-tauri/codeg-eui-core/src/commands.rs`
- Modify: `src-tauri/codeg-eui-core/src/lib.rs`
- Modify: `codeg-eui/app/bridge/codeg_eui_bridge.h`
- Create: `codeg-eui/app/bridge/ui_snapshot.h`
- Modify: `codeg-eui/CMakeLists.txt`
- Create: `codeg-eui/tests/ui_snapshot_test.cpp`
- Create: `codeg-eui/tests/shutdown_drain_test.cpp`
- Test: `src-tauri/codeg-eui-core/tests/bridge_contract.rs`
- Test: `src-tauri/codeg-eui-core/tests/shutdown_contract.rs`

**Interfaces:**

- Consumes: `EuiBootstrap`, Tokio bounded `mpsc`, and `AppState`.
- Produces: stable error constants `0..9`, lifecycle enum, request op/status enums, `CodegEuiSlice`, `CodegEuiSessionSummary`, `CodegEuiCompletion`, complete `CodegEuiFrame`, generic enqueue helper, monotonic request IDs, immutable `OwnedFrame` retention, and `begin_shutdown -> stopping polls -> final shutdown`.
- Completion transfer: pending completions are removed only while constructing a successful frame; the constructed `OwnedFrame` retains all copied bytes until replacement/shutdown.
- Shutdown visibility: `shutdown_ready=1` is returned only by a successful `stopping` poll that also exposes every remaining accepted request completion. Final `codeg_eui_shutdown` is rejected with `NOT_READY` until that exact frame has been observed.

**Task Routing Matrix:**

| task_index | title | files/modules | hard triggers evidence | soft signals evidence + soft total | final risk level + reason | implementer agent | reviewer set | policy version |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 3 | Lifecycle, async requests, completions, immutable frames | ABI, runtime, queues, model, contract tests | `unsafe_ffi`: pointers/panics; `concurrency_lifecycle`: threads/queues/shutdown | `cross_runtime_or_process=2`, `shared_interface=1`, `multi_layer_without_test_seam=1`; total `4` | `high`: hard triggers and soft threshold | `codex` | `codex (separate) + grok` | `b2d_task_risk_v1` |

- [ ] **Step 1: Write ABI layout, lifecycle-order, and input RED tests**

Cover this exact table in `bridge_contract.rs`:

```rust
#[test]
fn lifecycle_rejects_invalid_order_and_wrong_thread() {
    assert_eq!(shutdown(), ERR_INVALID_STATE);
    assert_eq!(init_empty(), OK);
    assert_eq!(init_empty(), ERR_INVALID_STATE);
    assert_eq!(poll_frame().unwrap().lifecycle_state, RUNNING);
    assert_eq!(std::thread::spawn(poll_code).join().unwrap(), ERR_WRONG_THREAD);
    assert_eq!(begin_shutdown(), OK);
    assert_eq!(enqueue_noop(), ERR_INVALID_STATE);
    assert_eq!(shutdown(), ERR_NOT_READY);
    assert_eq!(poll_frame().unwrap().lifecycle_state, STOPPING);
    drain_until_ready();
    assert_eq!(shutdown(), OK);
    assert_eq!(shutdown(), ERR_INVALID_STATE);
}

#[test]
fn strings_reject_null_invalid_utf8_and_bounds_without_accepting_a_request() {
    assert_eq!(enqueue_message(std::ptr::null(), 1), ERR_NULL_POINTER);
    assert_eq!(enqueue_message([0xff].as_ptr(), 1), ERR_INVALID_UTF8);
    assert_eq!(enqueue_message(vec![b'x'; MAX_MESSAGE + 1].as_ptr(), MAX_MESSAGE + 1),
               ERR_TOO_LARGE);
    assert_eq!(accepted_request_count(), 0);
}
```

The C header layout test also asserts the `shutdown_ready` offset and declares `codeg_eui_begin_shutdown`. Keep queue behavior for its own RED step below.

- [ ] **Step 2: Run focused bridge tests to verify RED**

```bash
cargo test --manifest-path src-tauri/codeg-eui-core/Cargo.toml --test bridge_contract
```

Expected: FAIL on the first missing expanded session/completion ABI symbol or unimplemented wrong-thread/input contract; the M0 `shutdown_ready` field and basic two-phase order remain intact.

- [ ] **Step 3: Define the complete ABI v1 layout in Rust and C**

Use matching repr(C) definitions and discriminants:

```rust
#[repr(C)]
pub struct CodegEuiSlice { pub ptr: *const u8, pub len: usize }

#[repr(C)]
pub struct CodegEuiCompletion {
    pub request_id: u64,
    pub op: u32,
    pub status: u32, // ok=0, error=1, stale=2, cancelled=3
    pub result_payload: CodegEuiSlice,
    pub error: CodegEuiSlice,
}

#[repr(C)]
pub struct CodegEuiFrame {
    pub api_version: u32,
    pub lifecycle_state: u32,
    pub generation: u64,
    pub selection_epoch: u64,
    pub sessions: *const CodegEuiSessionSummary,
    pub sessions_len: usize,
    pub connection_id: CodegEuiSlice,
    pub event_seq: u64,
    pub transcript_json: CodegEuiSlice,
    pub live_assistant: CodegEuiSlice,
    pub stream_active: u8,
    pub needs_resync: u8,
    pub shutdown_ready: u8,
    pub _reserved: [u8; 5],
    pub error_strip: CodegEuiSlice,
    pub completions: *const CodegEuiCompletion,
    pub completions_len: usize,
    pub t0_ns: u64,
    pub t_first_token_ns: u64,
    pub t_end_ns: u64,
}
```

Run C `static_assert` checks for every struct size/alignment and Rust `size_of/align_of/offset_of` tests. Keep all booleans as `uint8_t`; do not expose Rust `bool`, enums, `String`, `Vec`, or references. Mirror these lifecycle declarations exactly:

```c
int codeg_eui_begin_shutdown(void);
int codeg_eui_poll(CodegEuiFrame *out); /* legal in running and stopping */
int codeg_eui_shutdown(void);           /* final join/free after ready poll */
```

- [ ] **Step 4: Add the process-global state skeleton and panic boundary**

Use `OnceLock<Mutex<BridgeSlot>>`, capture `std::thread::ThreadId` on successful init, and wrap every exported function:

```rust
fn ffi_guard(body: impl FnOnce() -> i32 + UnwindSafe) -> i32 {
    match std::panic::catch_unwind(body) {
        Ok(code) => code,
        Err(_) => {
            record_panic_diagnostic("Rust panic contained at codeg-eui ABI");
            CODEG_EUI_ERR_PANIC
        }
    }
}
```

Stable errors are: `OK=0`, `INVALID_STATE=1`, `NULL_POINTER=2`, `INVALID_UTF8=3`, `TOO_LARGE=4`, `QUEUE_FULL=5`, `WRONG_THREAD=6`, `PANIC=7`, `INTERNAL=8`, `NOT_READY=9`. `codeg_eui_api_version` is the only call allowed from any thread and lifecycle state.

Use these concrete ownership types; methods are filled in by the following RED/GREEN pairs:

```rust
#[repr(u32)]
enum LifecycleState { Uninitialized = 0, Starting = 1, Running = 2,
                      Stopping = 3, Stopped = 4 }

struct BridgeSlot {
    lifecycle: LifecycleState,
    ui_thread: Option<std::thread::ThreadId>,
    runtime: Option<RuntimeOwner>,
    model: SharedModel,
    last_frame: Option<OwnedFrame>,
    shutdown_ready_observed: bool,
}

static BRIDGE: OnceLock<Mutex<BridgeSlot>> = OnceLock::new();
```

- [ ] **Step 5: Run layout/lifecycle tests to verify GREEN**

```bash
cargo test --manifest-path src-tauri/codeg-eui-core/Cargo.toml --test abi_smoke
cargo test --manifest-path src-tauri/codeg-eui-core/Cargo.toml --test bridge_contract lifecycle -- --nocapture
cmake -S codeg-eui -B codeg-eui/build-contract -DCODEG_EUI_CONTRACTS_ONLY=ON
cmake --build codeg-eui/build-contract --parallel
codeg-eui/tests/assert_ctest_registered.sh codeg-eui/build-contract codeg_eui_abi_layout
ctest --test-dir codeg-eui/build-contract -R '^codeg_eui_abi_layout$' --output-on-failure
```

Expected: complete layout, lifecycle order, wrong-thread, panic, and input cases pass; queue/frame/drain-with-in-flight-work cases remain unimplemented.

- [ ] **Step 6: Write bounded acceptance and exactly-once RED tests**

```rust
#[tokio::test]
async fn the_257th_request_is_rejected_before_acceptance() {
    let bridge = TestBridge::running_with_blocked_worker();
    let ids = (0..256).map(|_| bridge.enqueue_noop().unwrap()).collect::<Vec<_>>();
    assert!(ids.iter().all(|id| *id != 0));
    assert_eq!(ids.iter().collect::<HashSet<_>>().len(), 256);
    assert_eq!(bridge.enqueue_noop(), Err(CODEG_EUI_ERR_QUEUE_FULL));
    assert_eq!(bridge.accepted_count(), 256);
}
```

Add worker error, worker panic, selection-stale, and duplicate-terminalization cases. Each accepted ID must have one reserved completion slot and exactly one terminal state.

- [ ] **Step 7: Run queue tests to verify RED**

```bash
cargo test --manifest-path src-tauri/codeg-eui-core/Cargo.toml --test bridge_contract queue -- --nocapture
```

Expected: FAIL because command admission and completion reservation are absent.

- [ ] **Step 8: Implement bounded async acceptance and terminalization**

`RuntimeCommand` carries `{request_id, selection_epoch, op}`. The UI call validates input, reserves completion capacity, allocates the next ID with checked increment, and `try_send`s into a 256-slot channel. The worker catches task errors/panics and calls one terminalization function guarded by a `HashSet<u64>`:

```rust
fn terminalize(&mut self, completion: OwnedCompletion) {
    assert!(self.accepted.remove(&completion.request_id),
            "accepted request terminalized more than once");
    self.completions.push_back(completion);
}
```

Capacity accounting reserves one terminal completion slot per accepted request; the 256 bound therefore cannot overflow after acceptance. Mark a result `stale` when its captured `selection_epoch` differs from the model's current epoch, but still emit it exactly once.

Define the queue boundary explicitly:

```rust
struct RuntimeCommand {
    request_id: NonZeroU64,
    selection_epoch: u64,
    op: Operation,
    payload: CommandPayload,
}

struct CompletionLedger {
    accepted: HashSet<NonZeroU64>,
    ready: VecDeque<OwnedCompletion>,
    reserved: usize,
}
```

- [ ] **Step 9: Run queue tests to verify GREEN**

Run the Step 7 command. Expected: 256 requests are accepted, request 257 is rejected before acceptance, and all released requests terminalize once.

- [ ] **Step 10: Write immutable-frame and completion-transfer RED tests**

Prove enqueue/model mutation cannot invalidate frame A, a failed poll leaves frame A valid, successful poll B drains each ready completion once, and B's bytes stay readable through later enqueue calls. Assert the same request ID is absent from frame C.

```bash
cargo test --manifest-path src-tauri/codeg-eui-core/Cargo.toml --test bridge_contract frame -- --nocapture
```

Expected: FAIL because `OwnedFrame` retention and atomic completion transfer are absent.

- [ ] **Step 11: Implement immutable frame construction and completion transfer**

`OwnedFrame` owns `Vec<u8>` for every string, `Vec<OwnedCompletion>`, and parallel repr(C) views. Build every pointer only after all owning vectors have final capacity. On successful poll: lock model, move at most all currently pending completions into a new `OwnedFrame`, increment frame generation, swap it into `BridgeSlot.last_frame`, then copy only the top-level repr(C) value to `*out`. Enqueue and model updates never touch `last_frame`.

- [ ] **Step 12: Run frame tests to verify GREEN**

Run the Step 10 command. Expected: all pointer-lifetime and completion-transfer cases pass.

- [ ] **Step 13: Write public two-phase shutdown RED tests**

`shutdown_contract.rs` uses barriers to accept a controllable blocked request, calls `codeg_eui_begin_shutdown`, and asserts: new enqueue is rejected; poll remains legal in `stopping`; final shutdown returns `NOT_READY` before a ready frame; cancellation terminalizes the blocked ID; one successful poll exposes that completion and `shutdown_ready=1`; only then final shutdown returns `OK`, joins the runtime, frees the frame, and enters `stopped`.

Create `shutdown_drain_test.cpp` as the black-box consumer and wrap the body in
`TEST(ShutdownDrain, exposes_cancelled_completion_before_final_free)`. Build the
staticlib with Cargo feature `ffi-test-hooks`; the feature exposes only
`codeg_eui_test_enqueue_blocked(uint64_t*)` as a stimulus. The RED setup must
export a compile-safe hook that accepts one synthetic request and returns its
ID, but it does not yet terminalize that ID during drain; the harness therefore
builds and fails the final completion assertion. The C++ test uses the ordinary
public lifecycle/poll ABI and must copy the result before final free:

```cpp
TEST(ShutdownDrain, exposes_cancelled_completion_before_final_free) {
ASSERT_EQ(codeg_eui_init(root.data(), root.size()), CODEG_EUI_OK);
uint64_t requestId = 0;
ASSERT_EQ(codeg_eui_test_enqueue_blocked(&requestId), CODEG_EUI_OK);
ASSERT_EQ(codeg_eui_begin_shutdown(), CODEG_EUI_OK);
ASSERT_EQ(codeg_eui_shutdown(), CODEG_EUI_ERR_NOT_READY);

std::vector<Completion> seen;
for (int attempt = 0; attempt < 200; ++attempt) {
  CodegEuiFrame frame{};
  ASSERT_EQ(codeg_eui_poll(&frame), CODEG_EUI_OK);
  appendCopiedCompletions(frame, seen);
  if (frame.shutdown_ready == 1) break;
  std::this_thread::sleep_for(std::chrono::milliseconds(5));
}
ASSERT_EQ(countCompletion(seen, requestId, CODEG_EUI_COMPLETION_CANCELLED), 1);
ASSERT_EQ(codeg_eui_shutdown(), CODEG_EUI_OK);
}
```

The test hook is absent from normal builds and normal headers unless `CODEG_EUI_TEST_HOOKS` is defined.

Add this opt-in feature to `src-tauri/codeg-eui-core/Cargo.toml`; do not include it in `default`:

```toml
[features]
default = []
ffi-test-hooks = []
```

```bash
cargo test --manifest-path src-tauri/codeg-eui-core/Cargo.toml --test shutdown_contract
cargo build --manifest-path src-tauri/codeg-eui-core/Cargo.toml --features ffi-test-hooks
cmake -S codeg-eui -B codeg-eui/build-abi-link -DCODEG_EUI_CONTRACTS_ONLY=ON -DCODEG_EUI_ABI_LINK_TESTS=ON -DCODEG_EUI_RUST_LIB="$PWD/src-tauri/codeg-eui-core/target/debug/libcodeg_eui_core.a"
cmake --build codeg-eui/build-abi-link --target codeg_eui_shutdown_drain_test --parallel
codeg-eui/tests/assert_ctest_registered.sh codeg-eui/build-abi-link codeg_eui_shutdown_drain
set +e
red_output=$(ctest --test-dir codeg-eui/build-abi-link -R '^codeg_eui_shutdown_drain$' --output-on-failure 2>&1)
red_status=$?
set -e
test "$red_status" -ne 0
printf '%s\n' "$red_output" | rg -q '\[FAIL\] ShutdownDrain.exposes_cancelled_completion_before_final_free'
printf '%s\n' "$red_output" | rg -q '0% tests passed, 1 tests failed out of 1'
```

Expected: the Rust contract test fails on missing stopping behavior; the C++
target compiles, registration count is exactly one, and the harness reports the
named `ShutdownDrain` assertion failure. Configure/build failure, `No tests were
found`, or `Total Tests: 0` does not count as RED.

- [ ] **Step 14: Implement begin-drain, stopping polls, and final shutdown**

`codeg_eui_begin_shutdown` is UI-thread-only and legal only in `running`. It changes state to `stopping`, closes command admission, cancels all accepted work, and starts `disconnect_all(ApplicationShutdown)` without joining or freeing. `codeg_eui_poll` remains non-blocking in `stopping`; after workers/connections quiesce and `accepted` is empty, its next successful frame transfers the final completions, sets `shutdown_ready=1`, and records `shutdown_ready_observed=true` only after copying the frame to `*out`. `codeg_eui_shutdown` requires `stopping && shutdown_ready_observed`, joins Tokio, drops `last_frame`, and sets `stopped`; it never creates an unobservable completion.

```rust
fn final_shutdown(slot: &mut BridgeSlot) -> i32 {
    if slot.lifecycle != LifecycleState::Stopping || !slot.shutdown_ready_observed {
        return CODEG_EUI_ERR_NOT_READY;
    }
    slot.runtime.take().expect("runtime while stopping").join();
    slot.last_frame = None;
    slot.lifecycle = LifecycleState::Stopped;
    CODEG_EUI_OK
}
```

- [ ] **Step 15: Run public shutdown tests to verify GREEN**

```bash
cargo test --manifest-path src-tauri/codeg-eui-core/Cargo.toml --test shutdown_contract
cargo build --manifest-path src-tauri/codeg-eui-core/Cargo.toml --features ffi-test-hooks
cmake -S codeg-eui -B codeg-eui/build-abi-link -DCODEG_EUI_CONTRACTS_ONLY=ON -DCODEG_EUI_ABI_LINK_TESTS=ON -DCODEG_EUI_RUST_LIB="$PWD/src-tauri/codeg-eui-core/target/debug/libcodeg_eui_core.a"
cmake --build codeg-eui/build-abi-link --target codeg_eui_shutdown_drain_test --parallel
codeg-eui/tests/assert_ctest_registered.sh codeg-eui/build-abi-link codeg_eui_shutdown_drain
ctest --test-dir codeg-eui/build-abi-link -R '^codeg_eui_shutdown_drain$' --output-on-failure
```

Expected: the exact registered C++ consumer test passes after observing the
cancelled request once before final shutdown, and Rust verifies
disconnect/join/free ordering without timing sleeps.

- [ ] **Step 16: Write the C++ deep-copy RED test**

`ui_snapshot_test.cpp` copies frame A, triggers successful frame B, completes the two-phase shutdown, and then reads only the copied A value strings. It also rejects `(ptr=null,len>0)` and accepts `(ptr=null,len=0)`.

Add a compile-safe neutral `UiSnapshot`/`copy_frame` signature in
`ui_snapshot.h`; the RED implementation returns an empty value. Register the
source in `CMakeLists.txt` before configuring:

```cmake
codeg_eui_add_contract_test(
  codeg_eui_ui_snapshot tests/ui_snapshot_test.cpp)
```

The behavioral assertion must name its failure:

```cpp
TEST(UiSnapshot, owns_frame_a_after_frame_b_and_shutdown) {
  const UiSnapshot copied = copy_frame(frameWithAssistant("frame-a"));
  advanceToFrameBAndShutdown();
  EXPECT_EQ(copied.liveAssistant, std::string("frame-a"));
}
```

```bash
cmake -S codeg-eui -B codeg-eui/build-contract -DCODEG_EUI_CONTRACTS_ONLY=ON
cmake --build codeg-eui/build-contract --target codeg_eui_ui_snapshot_test --parallel
codeg-eui/tests/assert_ctest_registered.sh codeg-eui/build-contract codeg_eui_ui_snapshot
set +e
red_output=$(ctest --test-dir codeg-eui/build-contract -R '^codeg_eui_ui_snapshot$' --output-on-failure 2>&1)
red_status=$?
set -e
test "$red_status" -ne 0
printf '%s\n' "$red_output" | rg -q '\[FAIL\] UiSnapshot.owns_frame_a_after_frame_b_and_shutdown'
printf '%s\n' "$red_output" | rg -q '0% tests passed, 1 tests failed out of 1'
```

Expected: the target builds and one registered test fails because the neutral
copy has no `frame-a` bytes. Missing source/symbol/target or zero selected tests
is an invalid RED result.

- [ ] **Step 17: Implement and verify the C++ deep-copy boundary**

`ui_snapshot.h` defines value-owned C++ strings/vectors and performs the copy immediately after `codeg_eui_poll` returns `OK`. Implement the null/length checks and copy every nested slice/vector exercised by the Step 16 test. C++ never stores any raw Rust pointer in page state.

Build `codeg_eui_ui_snapshot_test`, assert the exact registration again, then
run `ctest --test-dir codeg-eui/build-contract -R '^codeg_eui_ui_snapshot$'
--output-on-failure`. Expected: PASS.

- [ ] **Step 18: Run Task 3 automated verification**

```bash
cargo test --manifest-path src-tauri/codeg-eui-core/Cargo.toml --test abi_smoke
cargo test --manifest-path src-tauri/codeg-eui-core/Cargo.toml --test bridge_contract
cargo test --manifest-path src-tauri/codeg-eui-core/Cargo.toml --test shutdown_contract
cargo build --manifest-path src-tauri/codeg-eui-core/Cargo.toml --features ffi-test-hooks
cmake -S codeg-eui -B codeg-eui/build-contract -DCODEG_EUI_CONTRACTS_ONLY=ON -DCODEG_EUI_ABI_LINK_TESTS=ON -DCODEG_EUI_RUST_LIB="$PWD/src-tauri/codeg-eui-core/target/debug/libcodeg_eui_core.a"
cmake --build codeg-eui/build-contract --parallel
codeg-eui/tests/assert_ctest_registered.sh codeg-eui/build-contract \
  codeg_eui_harness_self codeg_eui_abi_layout \
  codeg_eui_shutdown_drain codeg_eui_ui_snapshot
ctest --test-dir codeg-eui/build-contract --output-on-failure
```

Expected: lifecycle order, UI affinity, panic containment, input bounds, queue backpressure, one completion per accepted ID, stale marking, frame lifetime, externally observable drain, and final shutdown all pass.

- [ ] **Step 19: Commit and prepare the Task 3 review package**

```bash
git add --dry-run -- src-tauri/codeg-eui-core/Cargo.toml src-tauri/codeg-eui-core/src/lib.rs src-tauri/codeg-eui-core/src/abi.rs src-tauri/codeg-eui-core/src/runtime.rs src-tauri/codeg-eui-core/src/model.rs src-tauri/codeg-eui-core/src/commands.rs src-tauri/codeg-eui-core/tests/bridge_contract.rs src-tauri/codeg-eui-core/tests/shutdown_contract.rs codeg-eui/CMakeLists.txt codeg-eui/app/bridge/codeg_eui_bridge.h codeg-eui/app/bridge/ui_snapshot.h codeg-eui/tests/ui_snapshot_test.cpp codeg-eui/tests/shutdown_drain_test.cpp
git add -- src-tauri/codeg-eui-core/Cargo.toml src-tauri/codeg-eui-core/src/lib.rs src-tauri/codeg-eui-core/src/abi.rs src-tauri/codeg-eui-core/src/runtime.rs src-tauri/codeg-eui-core/src/model.rs src-tauri/codeg-eui-core/src/commands.rs src-tauri/codeg-eui-core/tests/bridge_contract.rs src-tauri/codeg-eui-core/tests/shutdown_contract.rs codeg-eui/CMakeLists.txt codeg-eui/app/bridge/codeg_eui_bridge.h codeg-eui/app/bridge/ui_snapshot.h codeg-eui/tests/ui_snapshot_test.cpp codeg-eui/tests/shutdown_drain_test.cpp
git diff --cached --name-only
git status --short --untracked-files=all
git commit -m "feat(eui): implement async bridge lifecycle"
git show --stat --oneline HEAD
git diff HEAD^ -- src-tauri/codeg-eui-core codeg-eui/app/bridge codeg-eui/tests codeg-eui/CMakeLists.txt
```

Expected package: one ABI/lifecycle commit with matching Rust/C layouts, public stopping-poll evidence, and no generated build/archive path staged. Route it to both high-risk reviewers, then continue directly to Task 4.

