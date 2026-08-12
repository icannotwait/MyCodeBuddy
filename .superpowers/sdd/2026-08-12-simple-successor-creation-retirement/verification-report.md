# Simple Successor Creation Retirement Verification Report

## Reviewed State

- Branch: `feature/simple-workflow-v2-retirement`
- Reviewed implementation commit: `15024f989b0dba487008370c004362c32665eef8` (`15024f98 refactor(workflow): remove simple successor persistence`)
- Tracked tree at that commit: clean. Untracked protected paths remain: `.codex-tmp-*` and `.task-runtimes/`. Commit `16684d6d` is an ancestor of HEAD (`git merge-base --is-ancestor` exit 0).
- Task implementation commits:
  - Task 1: `1ce09c84ac7bffafb6691d61944a53af83caf9fe` `refactor(workflow): retire automatic simple successors`
  - Task 2: `26d776820e15f2b821986e8a9cd2d6034af4d192` `refactor(workflow): remove archived successor UI`
  - Task 3: `18942cc26799e808996b95e6a04fa8db932b6764` `refactor(workflow): retire archived successor metadata`
  - Task 4: `15024f989b0dba487008370c004362c32665eef8` `refactor(workflow): remove simple successor persistence`

## Static Contract

Both forbidden scans exited 1 with no matches:

- Frontend successor forbidden scan (`continueArchivedWorkflowInSimple|SimpleSuccessorResult|archivedContinue|archivedContinuing|archivedOpenSuccessor` over `src`): exit 1, no matches.
- Rust successor forbidden scan (`SimpleBootstrapPromptSink|admit_pending_simple_successor_bootstrap|admit_simple_successor_bootstrap_after_connect|register_simple_workflow_with_source|eligible_simple_successor_plan|normalize_simple_successor_plan_locator|simple_successor_bootstraps` over `src-tauri/src`): exit 1, no matches.

Every positive preservation/contract scan exited 0 and had matches. Reviewed matches belong to the stable rejection API, archived compatibility DTO, or explicitly preserved unrelated identity:

- `continue_archived_workflow_in_simple` preservation scan (exit 0): Tauri registry in `src-tauri/src/lib.rs`, authenticated route in `src-tauri/src/web/router.rs`, parameterless core/wrapper in `src-tauri/src/commands/simple_workflow.rs`, parameterless handler in `src-tauri/src/web/handlers/simple_workflow.rs`, plus contract tests in those command/handler files.
- Retirement contract scan (exit 0): wire code `simple_successor_creation_retired` in `src-tauri/src/app_error.rs`; exact message constant and tests in `src-tauri/src/commands/simple_workflow.rs`; HTTP 409 body assertions in `src-tauri/src/web/handlers/simple_workflow.rs`.
- Archived compatibility field scan (exit 0): `successor_conversation_id` / `can_create_simple_successor` on the TypeScript snapshot in `src/lib/types.ts` and the Rust DTO plus serialization fixture (`null` / `false`) in `src-tauri/src/acp/delegation/workflow/dto.rs`.
- `legacy_source_workflow_id` preservation scan (exit 0): entity column and the existing completion-protocol migrations only.
- `v2_successor` preservation scan (exit 0): TypeScript optional field, `types.rs` DTO, restart lookup, and a `state_dto.rs` fixture. Unrelated identity, not Simple successor creation.

## Frontend

- `pnpm test`: exit 0. Vitest `v2.1.9`: Test Files 349 passed (349); Tests 5178 passed (5178). Duration 28.36s.
- `pnpm eslint .`: exit 0. `23 problems (0 errors, 23 warnings)`. No warning is in a Task 5 owned file. Pre-existing warnings remain in unrelated chat, settings, context, hook, popout, delegation, and store files.
- Detached production build at reviewed commit `15024f989b0dba487008370c004362c32665eef8`:
  - `$relativeToParent`: `MyCodeBuddy-build-15024f989b0d`
  - containment: accepted
  - `$installCode`: 0
  - `$buildCode`: 0
  - `$removeCode`: 0
  - `pnpm install --frozen-lockfile` completed (`Done in 11.8s using pnpm v11.9.0`).
  - `pnpm build` compiled successfully and generated all static pages (`✓ Compiled successfully in 5.5s`; `✓ Generating static pages using 23 workers (33/33)`). All listed routes are static (`○`).
  - After cleanup, `git worktree list` no longer contains `MyCodeBuddy-build-15024f989b0d`. Other worktrees were not removed or modified.

## Rust

Commands ran serially from `src-tauri/` with `RUST_MIN_STACK=16777216`. No overlapping Cargo processes were started.

- `cargo test --features test-utils`: every crate summary printed `test result: ok` with 0 failed. Aggregate 4501 passed, 0 failed, 1 ignored. The ignored test is `parsers::codex::tests::conversation_2582_response_item_checkpoint_message_is_reconstructed`. The cmd wrapper used to persist `%ERRORLEVEL%` wrote an empty exit file (`echo %ERRORLEVEL%>file` is a handle redirect when the value is a single digit), so a numeric `$desktopTestCode` was not captured. Cargo itself reported no `error: test failed` and no `error: could not compile`.
- `cargo test --no-default-features --features server --bin codeg-server --lib`: exit 0. `4223 passed; 0 failed; 1 ignored` (`codeg_lib`) plus `1 passed; 0 failed; 0 ignored` (`codeg-server` bin). Same ignored Codex parser test.
- `cargo clippy --all-targets --features test-utils -- -D warnings`: exit 101. Failed gate. Exact diagnostic:

```
error: unnecessary use of `get("successor_conversation_id").is_none()`
   --> src\commands\workflow_completion.rs:270:28
    |
270 |         assert!(navigation.get("successor_conversation_id").is_none());
    |                 -----------^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^
    |                 |
    |                 help: replace it with: `!navigation.contains_key("successor_conversation_id")`
    |
    = help: for further information visit https://rust-lang.github.io/rust-clippy/rust-1.97.0/index.html#unnecessary_get_then_check
    = note: `-D clippy::unnecessary-get-then-check` implied by `-D warnings`

error: could not compile `codeg` (lib test) due to 1 previous error
```

  This is a test-only Clippy deny in `completion_entry_guard_preserves_retirement_navigation`. Task 5 did not change the file.

- `cargo clippy --no-default-features --features server --bin codeg-server --lib -- -D warnings`: exit 0. `Finished dev profile` with no warning or error lines.
- `cargo clippy --no-default-features --bin codeg-mcp -- -D warnings`: exit 0. `Finished dev profile` with no warning or error lines.

## Preserved Scope

- `continue_archived_workflow_in_simple` remains registered in the production Tauri command list and the authenticated server router. The shared core is `continue_archived_workflow_in_simple_core() -> Result<(), AppCommandError>` and the wrappers call it with no operation arguments.
- Archived compatibility fields remain present and the DTO fixture serializes `successor_conversation_id: null` and `can_create_simple_successor: false`.
- `legacy_source_workflow_id` and `v2_successor` remain present on their unrelated compatibility surfaces.

## Remaining Risks

- Required desktop Clippy gate is red: `cargo clippy --all-targets --features test-utils -- -D warnings` exit 101 on `src-tauri/src/commands/workflow_completion.rs:270` (`clippy::unnecessary_get_then_check`). Independent review of the retirement product is blocked on that `-D warnings` surface until an owned later change rewrites the assertion. This report does not change that test.
- Parked deferred coverage, not a new product defect: `manifest_publication_is_retired_for_stale_features_without_writes` in `src-tauri/src/acp/delegation/listener.rs` still does not assert `successor_conversation_id: null` or `can_create_simple_successor: false` on the stale-feature fallback path.
- Desktop `cargo test` numeric exit code was not persisted by the cmd wrapper. The claim that the suite is green rests on cargo's own `test result: ok` summaries (4501 passed, 0 failed, 1 ignored), not on a captured `$LASTEXITCODE`.
- Pre-existing ESLint warnings: 23 warnings, 0 errors, none in Task 5 owned files.
- Protected untracked paths `.codex-tmp-*` and `.task-runtimes/` remain visible and were not stashed, reset, cleaned, or committed.

A delivery blocker remains: desktop Clippy `-D warnings` is not green.
