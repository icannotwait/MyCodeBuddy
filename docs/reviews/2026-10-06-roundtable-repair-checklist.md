# Roundtable repair checklist

Date: 2026-10-06 UTC

This checklist tracks the 21 findings from the review of `2f57bcde4418bb014b09ba6e127416af07396a81`. The repairs preserve upstream `a470c4aeedd3c6f8e8c999e8d4ec47b37b171870`, including the shared live/probe ACP exchange, production resolver configuration, phase-pinned tool schema, bounded submission repair and Grok session restrictions.

**Verification state:** repair implementation is integrated; required dynamic verification remains outstanding. Rust compilation, Rust tests, formatting, React/Vitest, project type checking, lint and application build have not run locally. Source review and native checks are not substitutes for those checks. The execution gate must remain disabled; this checklist does not certify a live adapter or host.

## Finding coverage

| Finding | Repair or retained behavior | State |
| --- | --- | --- |
| F01 | Stable desktop `ExitCode` return and matching test-only definition/export guards | Integrated; compiler verification pending |
| F02 | Ordinary ACP completion cannot prove a production result receipt or drain | Fail-closed security/correctness guard integrated; production receipt/drain capability unimplemented |
| F03 | Mounted credentials are reported present; failed evidence dominates passes; unknown visibility remains unknown | Fail-closed security/correctness guard integrated; credential/native-tool isolation capability unimplemented |
| F04 | Dedicated, private, exact installed scratch path is compatible with a HOME installation | Integrated; Rust isolation regressions pending |
| F05 | Preserve upstream resolver parity between production and qualification | Upstream retained; Rust isolation regressions pending |
| F06 | Shared typed member/moderator schemas, host-owned seat identity, mandatory targets and contentful aliases | Integrated; Rust regressions pending |
| F07 | Explicit bounded registered-workspace selection, frozen manifests, immutable hash-checked preview and second confirmation | Integrated; native preview smoke passes; Rust/React pending |
| F08 | Shared live/probe completion rejects terminal errors even after a staged result | Integrated; Rust regressions pending |
| F09 | Frozen publication recovery preserves paused/recovery-required state and explicit resume | Integrated; Rust regressions pending |
| F10 | Authoritative transactions acquire the SQLite writer reservation before reading decision state | Integrated; Rust concurrency regressions pending |
| F11 | Shared prepaid permission and local ownership allow revocation/reaping independently of database persistence | Integrated; Rust failure-injection regressions pending |
| F12 | Phase expiry can reap unfinished work, publish accepted quorum and continue synthesis without reusing successor intents | Integrated; Rust scheduler regressions pending |
| F13 | Durable pre-exec versus exec-pending ownership permits proven cleanup while retaining uncertain owners | Integrated; Rust isolation regressions pending |
| F14 | Full-plan per-seat context bounds, explicit request-envelope evidence and encoded optional-input ceilings | Shared correctness guard integrated; production envelope measurement capability unimplemented |
| F15 | Bounded auth-file secret extraction and safe diagnostic retention cover values and streaming fragments | Integrated; Rust regressions pending |
| F16 | All probe runners own timeout/cancellation cleanup and require final cleanup evidence | Integrated; Rust isolation regressions pending |
| F17 | Controls retain and settle the previous active-time lease after cleanup, including cleanup duration | Integrated; Rust regressions pending |
| F18 | Latest discussion coverage survives synthesis; failed/absent slots are distinct from abstention | Integrated; Rust regressions pending |
| F19 | Moderator and editor model/effort lookup use participant ordinal | Integrated; Rust/React regressions pending |
| F20 | Missing provider credentials produce a capability error without invalidating the application session | Integrated; native error smoke passes; Rust/React pending |
| F21 | Confirmation displays actual recipients and frozen sources and binds config/capability changes across cleanup | Integrated; native request replay smoke passes; Rust/React pending |

## Integration checks

- Product confirmation/source checks and lifecycle write-first transactions are both retained in `product.rs`
- A paid control retains its confirmed snapshot across asynchronous cleanup and compares the same snapshot again before successor execution
- Local cleanup remains independent of successful failure-state persistence; cleanup proof persistence and budget settlement remain mandatory before successful retirement
- The leased executor forwards the context profile, request-envelope byte bound and evidence hash; lifecycle fixture wrappers forward the same methods
- Turn requests initialize the local lease field, and production dispatch attaches the room's shared prepaid permission
- Both lifecycle cleanup/deadline regressions and contract completion/context regressions are retained
- Upstream `a470c4a` is an ancestor of the integrated branch
- Certificate core bindings include 68 source keys covering actual admission, completion, persistent control/cleanup, diagnostics, private ingress/discovery and public entry points. Full external source modules deliberately invalidate certificates even on unrelated edits rather than silently omit a security path. A regression checks the exact key set and representative source-byte hashes

## Executed checks

Final local source checkpoint, 2026-10-06 06:24 UTC:

- `node scripts/roundtable-product-smoke.mjs`: 21 assertions passed, including exact paid-request replay across cancellation/navigation and immutable source preview/hash checks. Transport I/O is stubbed; these are not React or provider tests
- All three native slirp hook tests passed, covering lifetime, failed ownership-pin publication and cleanup/startup serialization
- `node scripts/check-roundtable-locks.mjs`: 35 shared lock packages match
- JSON decoding: all 10 translation catalogues passed
- Node native TypeScript transform: six changed `.ts` files parsed; this excludes TSX and type checking
- `git diff --check`: passed
- `node --test scripts/*.test.mjs src-tauri/scripts/*.test.mjs`: 98 tests, 96 passed, 2 failed before Rust execution
  - `desktop qualification preserves exit codes`: `spawnSync rustc ENOENT`
  - `repository Cargo config keeps dev and test artifacts lean`: Cargo subprocess unavailable
- An intermediate aggregate run observed 98 tests, 95 passed and 3 failed: the two missing-tool failures above plus a real slirp test race reading `/proc/<pid>/stat` after helper exit (`ESRCH`). The test now treats `ESRCH`, like `ENOENT`, as confirmed process exit and still propagates other errors. The corrected focused hook run passed all 3 tests; the implementation report also records 10 consecutive runs, 30/30 passed. The final integrated aggregate was rerun after this correction and returned 98 tests, 96 passed and only the 2 unavailable-Rust failures listed above

No local dependency/toolchain installation, provider call, credential injection, execution-policy enablement, deployment or pull-request merge was performed for these checks. Rust and installed frontend prerequisites are absent. No Rust syntax/type/formatting success is implied.

## Required verification before accepting the branch

Run the repository's existing CI against the final repair commit, including:

1. Rust formatting for both the application and protocol crate
2. Protocol tests and clippy
3. Roundtable runtime, transport and protocol-I/O targets with `test-utils`
4. Server tests without `test-utils`, proving the cfg repair, plus the server/desktop compilation and clippy matrix
5. Frontend lint, full Vitest, release-script tests and static export build

Focused regression commands and implementation limits are recorded in the [build report](2026-10-06-roundtable-build-fixes.md), [product report](2026-10-06-roundtable-product-fixes.md), [lifecycle report](2026-10-06-roundtable-lifecycle-fixes.md), [contract report](2026-10-06-roundtable-contract-fixes.md), and [qualification/isolation report](2026-10-06-roundtable-isolation-fixes.md). The [qualification completion memo](2026-10-06-roundtable-qualification-completion.md) separates component regressions from unimplemented live guarantees.

## Capability boundaries retained

The false-success and unsafe-admission defects are addressed by fail-closed guards, but the corresponding live capabilities in F02, F03 and F14 remain unimplemented. They are not closed feature-delivery items. Rerunning the current host probe cannot produce a usable certificate or supply the missing measurements.

- A fake or diagnostic probe does not qualify production receipt/drain, privacy, credential/native-tool isolation or complete adapter request-envelope behavior
- Missing request-envelope evidence remains unknown capacity; no token count is substituted for a byte measurement
- Selected-file capture fails closed on Windows until handle-relative containment is qualified
- Unreferenced content-addressed objects retained after a database rollback are quota-bounded; automatic garbage collection remains unavailable
- Unresolved paid-request replay survives client-side navigation in memory, but does not survive a full document reload or tab close
