# Roundtable build repair verification

Date: 2026-10-06 UTC

Base: `a8ee4042ff64cb54d16acd5830b8afa23d39beb6`

Scope: F01 and ordinary build/test prerequisites only

## Changes

- [x] Replace the unstable `ExitCode::exit_process` call with a stable `main() -> ExitCode` return, matching the existing server entry point
- [x] Preserve qualification status codes and successful credential-helper/desktop returns
- [x] Align only the two `legacy_test_model` definition/export guards with their existing `cfg(any(test, feature = "test-utils"))` consumers
- [x] Add an actual desktop-entrypoint compilation/subprocess regression that uses an isolated test library rather than Tauri or live providers
- [ ] Run the new regression with stable Rust
- [ ] Run the server library test target without `test-utils`, plus selected roundtable targets
- [ ] Run Rust formatting/clippy and final regression after integration

## Revalidated original failures

The review's saved CI evidence is for [run 37407932927](https://github.com/icannotwait/MyCodeBuddy/actions/runs/37407932927), head `2f57bcde4418bb014b09ba6e127416af07396a81`. Its relevant source is unchanged at the repair base: desktop `main.rs`, `control.rs`, `command_processor.rs`, `recovery.rs`, and the relevant control export in `roundtable/mod.rs`. The only intervening change in the inspected module file adds unrelated sandbox test exports.

Exact compiler diagnostics from that existing CI run:

```text
error[E0658]: use of unstable library feature `exitcode_exit_method`
  --> src/main.rs:15:57
15 |         codeg_lib::roundtable::run_roundtable_qualify().exit_process();
error: could not compile `codeg` (bin "codeg" test) due to 1 previous error

error[E0432]: unresolved import `super::control::ControlBook`
 --> src/roundtable/command_processor.rs:5:5
error[E0432]: unresolved import `super::control::MatrixRow`
 --> src/roundtable/recovery.rs:8:5
error[E0432]: unresolved imports `control::advance_control`, `control::apply_command`, `control::ControlBook`, `control::MatrixOutcome`, `control::MatrixRow`, `control::MutationCommandV1`
  --> src/roundtable/mod.rs:88:5
error: could not compile `codeg` (lib test) due to 3 previous errors
```

These are previous CI failures, not newly executed local compiler results. The new test was authored before the fix; its first local run could not reach compilation because Rust was missing. A fresh red/green run remains required once Rust is authorized and available.

## Environment and prerequisite checks

- Debian GNU/Linux 13 (trixie), 9.7 GiB RAM, no swap, approximately 28 GiB free workspace disk
- `node --version`: `v24.19.0`
- `pnpm --version`: `11.19.0`; repository metadata requests `pnpm@11.9.0`
- `cc --version`: `cc (Debian 14.2.0-19) 14.2.0`
- OpenSSL pkg-config version: `3.5.7`
- No installed `cargo`, `rustc`, `rustfmt`, or `rustup` was available for local verification
- GTK, WebKit, GIO/GLib, ATK, libsoup, appindicator, and dbus development pkg-config metadata is absent; no OS packages were installed
- Official Rust install documentation: <https://rust-lang.org/tools/install/>

## Local commands and current results

1. `cargo test --locked --manifest-path src-tauri/Cargo.toml --no-default-features --features server --bin codeg-server --lib`

   Not executed: `/bin/bash: cargo: command not found`.

2. `node --test scripts/roundtable-entrypoint.test.mjs` before the production change

   Failed before compilation: `spawnSync rustc ENOENT`. This is a toolchain blocker, not a reproduced E0658 or a passing regression.

3. `node --test scripts/*.test.mjs src-tauri/scripts/*.test.mjs` before the production change

   Exact summary: `tests 95`, `pass 93`, `fail 2`, `cancelled 0`, `skipped 0`, `todo 0`.

   Both failures are recorded, including the existing unrelated prerequisite failure:
   - `desktop entry point preserves qualification exit codes on stable Rust` (new test, subsequently shortened to `desktop qualification preserves exit codes`): `spawnSync rustc ENOENT`
   - `repository Cargo config keeps dev and test artifacts lean`: cargo is unavailable, so subprocess status is `null` rather than `0`

4. `node --check scripts/roundtable-entrypoint.test.mjs`

   Exit 0, no output.

5. `git diff --check`

   Exit 0, no output.

## Verification boundaries

The entrypoint test compiles the real `src-tauri/src/main.rs`, substituting only `codeg_lib` with a small local test library. It proves stable entrypoint compilation and return-code/control-flow behavior; it does not exercise real qualification, credentials, the GUI, or sandboxing. The server CI target remains necessary to prove the cfg repair. No live models, credentials, crun qualification, cloud coding task, push, merge, or deployment was used.

Full desktop Cargo verification is not claimed without its platform development libraries. Local tool installation and compilation remain blocked pending approval; no package installation is inferred from the repair authorization. The existing pull request's CI is the authorized verification route while local prerequisites remain unavailable.
