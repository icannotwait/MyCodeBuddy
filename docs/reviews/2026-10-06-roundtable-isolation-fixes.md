# Roundtable qualification and isolation remediation

Base: `15313e62d3e76b8ad03932c7dd2766bf2e0deb75` (rebased from `a8ee4042ff64cb54d16acd5830b8afa23d39beb6`).
Scope: F02, F03, F04, F05, F13, F16, plus the diagnostic request-envelope field required by F14.

## Changes

- **F02:** Separate ordinary ACP completion from `submit_receipt_completion`. Socket connection, advertised model IDs and a bounded smoke prompt cannot stand in for production broker submission, receipt/completion/drain, model selection, full-context or privacy observations. Missing product observations remain `not_tested`. The diagnostic report emits an unmeasured request envelope.
- **F03:** Failures dominate duplicate passes. Mounted auth files and Grok's attempt-local auth copy are reported as credential material present. Unknown native-tool visibility remains null/not-tested rather than a fabricated boolean. Probe/profile implementation sources now participate in the existing shared-core certificate hash.
- **F04:** The qualified builder permits only the exact installed `runtime_root/runs/<incarnation>/scratch` with a private operator-owned parent. It checks canonical, non-symlink paths and protected project/decoy/other-attempt overlap. Final profile verification rechecks the owned scratch. The generic builder still rejects HOME descendants and parent mounts.
- **F05:** Preserve the upstream shared resolver mount/hook, mount validation, and per-attempt Grok auth copy. Correct the guide: implementation changes invalidate old certificates through the shared-core hash, even when the resolver is not a separate key field.
- **F13:** Add explicit durable pre-exec journal records and a durable exec-pending transition immediately before synchronous process creation. Pre-exec cleanup can prove no process without a bundle; retirement prevents a concurrent transition into exec. Legacy records, pre-existing artifacts and exec-pending records remain uncertain and require OS cleanup proof. Generic retired markers never replace that proof.
- **F16:** Use the same owned-container guard for ACP, MCP and other probe runners. Normal timeout/error returns kill and wait for the launcher and run cleanup; cancelled futures retain synchronous fallback cleanup. Bound probe output and cleanup-command time. Refuse cleanup proof when OCI state or cgroup state remains or cannot be inspected. A failed runner without a returned final cleanup proof cannot inherit an earlier isolation runner's zero-process count. Give probe containers unique IDs. Abort the ACP socket-holder task on every exit.
- **Slirp ownership:** Hooks record a private PID/start-time pin and per-container/runtime-root environment markers. Cleanup opens a Linux pidfd, verifies ownership before signaling, waits for TERM, and escalates through the same pinned fd if needed. The normal watcher closes a pipe through the [upstream slirp4netns exit-fd contract](https://github.com/rootless-containers/slirp4netns/blob/master/slirp4netns.1.md), rather than signaling a reusable PID. Missing, stale, legacy or tampered ownership evidence cannot authorize signaling another process; unresolved cleanup remains failed.

## Regression coverage added

Tests use synthetic facts, temporary directories and local fake processes only:

- Ordinary ACP completion without a production submit receipt is refused
- Duplicate failed evidence dominates an earlier pass
- Native auth material cannot be reported absent; unknown visibility stays unknown
- Qualification/probe/profile source hashes appear in certificates
- Default HOME installation reaches the certificate check; parent and unrelated mounts are refused
- Pre-exec failures retain a durable no-process proof across retry/restart
- Retired pre-exec intent cannot enter exec; exec-pending without a bundle stays uncertain
- Version/isolation/egress timeouts and generic/MCP future cancellation reap owned fake containers
- Successful delete with residual state does not prove cleanup
- Owned fake slirp ignoring TERM is stopped; a forged PID/start-time pin cannot signal an unrelated helper

Existing DNS, Grok copy, symlink, journal-retirement and fake-qualification regressions are preserved. The rebase retains upstream shared ACP exchange/permission handling, bounded lenient transport parsing, and attempt-scratch deletion/pruning with their tests.

## Verification performed

- `git diff --check`: exit 0
- Extracted `SLIRP_HOOK_SCRIPT` and ran `sh -n` on it: exit 0
- `node --test scripts/roundtable-slirp-hook.test.mjs`: observed RED (hook exited 1 without lifecycle exit-fd), then GREEN (1 passed, 0 failed) after the pipe-lifecycle change. This executes the extracted hook against a fake local Node helper and fake container process; it checks owner/start-time recording, live helper state and helper termination after the fake container exits
- `node --test scripts/*.test.mjs src-tauri/scripts/*.test.mjs`: 95 tests, 94 passed, 1 failed. The failure is `repository Cargo config keeps dev and test artifacts lean` in `scripts/rust-build-policy.test.mjs`, because Cargo is unavailable (`spawnSync` status null rather than 0)
- Initial focused RED attempt: `cargo test --manifest-path src-tauri/Cargo.toml --no-default-features --features test-utils --test roundtable_runtime qualification_probe -- --nocapture`: exit 127, `cargo: command not found`
- Final local attempts for `cargo fmt --all -- --check`, the full `roundtable_runtime` target, `--lib`, and `cargo test --features test-utils`: all exit 127 because Cargo is unavailable

No local Rust RED/GREEN, compilation, formatting or suite pass is claimed. These checks require the existing CI before accepting the changes. No real crun, adapter, provider, credential injection, gate enablement, deployment or merge was performed.

## Remaining capability boundary

This is a fail-closed correction, not evidence of working live qualification. The production broker receipt/drain experiment, product privacy observations, complete adapter request-envelope measurement and a verifiable native-tool credential boundary remain unimplemented by the host probe. New product certificates must remain unavailable until those checks and boundaries are implemented and measured on an authorized host. Keep the execution gate disabled.


## Scoped review correction: configured helper lifecycle

The first implementation incorrectly accepted an absent slirp pin as cleanup
proof and did not account for a concurrent poststart hook or the watcher tree.

- The hook and cleanup now share a private `flock` startup gate and durable
  prepared/starting/running/cancelled state. Pin and state writes must succeed
  and sync before hook success; the helper and watcher close the gate fd
- Both helper roots have PID/start-time pins. Hook, watcher and child commands
  inherit bounded per-container owner/root/role markers
- Configured cleanup closes the gate, validates pins with pidfds, and stops and
  waits for the complete owner-marked helper set. It rechecks after container
  and launcher quiescence. Missing pins after startup remain unproven even when
  independently verified survivors can be stopped safely
- Intentionally no-slirp probes carry that fact into their guard. Configured
  never-started state is independently durable; missing configuration is not
  treated as either case
- Added fake-hook pin-write and publication-race tests. The pin-write test was
  observed RED (hook returned success), then GREEN. The actual extracted hook
  is paused between child creation and pin publication; cleanup cannot pass
  the shared gate, and cancelled state rejects a late hook
- Added Rust fake-process cases for missing configured pins, helper/watcher/
  unpinned marked descendants, no-slirp versus never-started state, and forged,
  legacy and reused-PID evidence. Their execution still requires existing CI
- Bind the shared ACP service-failure classifier implementation slice into the
  certificate core hash. If either slice marker moves or disappears, hash the
  full source rather than silently dropping the binding

Round1 verification: `node --test scripts/roundtable-slirp-hook.test.mjs`
passes 3/3. Full release-script suite is 97 tests, 96 passed and the same
Cargo-prerequisite failure in `scripts/rust-build-policy.test.mjs`.
`git diff --check`, `node --check` and extracted-hook `sh -n` pass.
No Rust compile/test/rustfmt success is claimed; Cargo remains unavailable.


## Second scoped review correction

- Retain every independently verified helper/descendant pidfd across sweeps,
  not just root pins. A descendant that clears its owner environment after
  TERM remains in the success and KILL-escalation conditions until its pinned
  process exits. Deduplicate by live PID identity and cap retained live
  handles; an exited identity may be replaced after PID reuse
- Open cleanup evidence with `O_NONBLOCK | O_NOFOLLOW | O_CLOEXEC`, then reject
  nonregular files with fstat. A FIFO cannot block the read-only pin open
  before metadata validation or any cleanup deadline
- Added Rust regressions for an owner-marked descendant that traps TERM,
  execs with cleared environment and ignores TERM until KILL, plus a FIFO pin
  with no writer that must promptly fail without signaling an unrelated
  process. The FIFO test rescues an old blocking reader before reporting RED
- Focused Rust test attempt exited 127 (`cargo: command not found`). Rust
  compilation, test execution and rustfmt remain UNRUN locally

Second-correction checks: fake hook suite 3/3 passed; full release-script
suite 96/97 passed, with the same missing-Cargo prerequisite failure.
`git diff --check` passed. These do not substitute for the pending Rust
regression execution.


## Third scoped review correction: cross-invocation proof continuity

- Cleanup persists an in-progress quarantine under the startup lock before
  any signaling. Only the same successful invocation, after all retained
  pidfds have exited and helper enumeration is empty, writes a proven state
- Enumeration/signal errors, timeout, cancellation, process exit or panic
  leave that quarantine durable. Retry/restart refuses to turn missing owner
  markers or dead root pins into success after descendant obligations were
  lost. Previously written legacy cancelled states are also quarantined
- This deliberately has a manual-recovery boundary: explicit host-level
  evidence must establish that the full owned tree is gone before an
  interrupted quarantine is resolved. Do not reset the marker or signal a
  bare saved PID to force progress. Automatic recovery of lost pidfd
  obligations is not claimed
- A unit-test-only, thread-local observer deterministically fails a sweep
  only after the fake descendant actually execs with cleared environment.
  Removing that observer and reopening cleanup must still return
  `slirp_cleanup_interrupted`, while the descendant remains alive. The test
  then explicitly kills/waits its own fake children. A separate regression
  refuses legacy cancelled markers
- Focused Rust execution attempt: exit 127, `cargo: command not found`.
  Rust compilation, regressions and rustfmt remain UNRUN locally

Third-correction available checks: fake hook tests pass 3/3; release scripts
pass 96/97 with only the existing missing-Cargo prerequisite failure.
Whitespace, JavaScript syntax and extracted-hook shell syntax checks pass.
