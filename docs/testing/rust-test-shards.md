# Low-memory native-Linux library test workflow

This is an **experimental opt-in compilation-sharding workflow**. The measured
attempt did not compile the complete suite in this VM and is not a reliable
full-suite low-memory solution. It does not replace the standard Cargo workflow
or establish that the whole application test matrix passes. The scope is exactly `--no-default-features --features test-utils --lib` on native
Linux. Desktop/default features, server binaries, integration tests, and other
platforms are not exercised by this runner.

## Why this organization

The original library test executable exceeds the available memory even with one
Cargo job, no incremental compilation/debug info, and codegen units 1 or 256.
Running a filtered test does not reduce the compilation unit. These shards apply
conditional compilation to test containers, while compiling the actual original
library with `cfg(test)`. They preserve private access, the test-only hooks, and
all original assertions. They do not extract or mock product implementations.

Every source file's test groups stay together so sibling test helper dependencies
remain intact. Ordinary Cargo commands retain the complete original suite when
`CODEG_TEST_SHARD` is unset. Setting the variable directly selects only one shard
and emits an explicit warning; **a passing shard is not a passing suite**.

## Commands

Run from the repository root on native Linux with Python 3.11+, the project's
normal Rust/Cargo toolchain and native build dependencies, and enough free disk
space for the target cache and logs. The runner uses only the Python standard
library; inventory regeneration also builds the locked Rust `syn` helper.
`--offline` requires the locked dependencies for both projects to be cached;
omit it for a normal dependency-resolving run when network access is available.
**Cargo `--offline` only controls dependency resolution. It does not prevent test
code from making network requests.** Use an appropriately authorized test
environment; do not treat the flag as network isolation or permission to contact
external services.

Do not run another Cargo build concurrently, and do not edit Rust sources or
regenerate gates while a run is active. Leave `CODEG_TEST_SHARD` unset: the wrapper
owns shard selection. Do not pass compiler, target, rustflags, or target-runner
overrides; unsupported overrides fail closed rather than silently changing the
coverage scope.

```sh
# Full workflow alias (same wrapper; normal dependency resolution).
pnpm rust:test:sharded:low-memory
```

For explicit auditing, offline execution, and diagnostics:

```sh
# Verify source bytes, gate coordinates, feature/platform cfg and inventory.
python3 scripts/rust-test-shards.py --audit-only

# Build, list, audit, and execute EVERY shard sequentially.
python3 scripts/rust-test-shards.py --offline

# Explicitly incomplete diagnostic pilot. Never use as the sole CI check.
python3 scripts/rust-test-shards.py --offline --shard 0

# Infrastructure regression tests.
python3 scripts/test_rust_test_shards.py
```

The runner fixes Cargo jobs and harness threads to 1, disables test incremental
compilation/debug information, uses test opt-level 0 and 256 codegen units for the
`codeg` package, and preserves the existing 32 MiB `RUST_MIN_STACK` reservation.
These settings apply to this invocation; do not keep experimenting with flags
between shards or turn on multiple builds to shorten the run. The stack setting
is a virtual-address reservation, not a claim about committed RAM.

The complete runner enumerates every shard in the checked-in manifest. Each
compiled listing must match its complete expected set of fully qualified names,
and the final union must have no missing, unexpected, or duplicate names.
Default-ignored tests remain listed and ignored exactly as before; execution
accounting rejects filtered-out tests. Runtime assertion failures remain failures.
No retries, skipping, or expectation changes turn a failing test green. Every
build failure (including a killed compiler/OOM), list/union mismatch, accounting
error, or runtime failure yields a nonzero exit. Preserve that status in CI and
shell wrappers; never append `|| true`. `--audit-only` verifies source inventory
without compiling or executing tests; `--list-only` compiles and verifies names
without executing. Neither proves the suite passed.

Only `summary.json` with `complete_selection: true`, `list_only: false`,
`union_audit: "passed"`, and `complete_core_suite_passed: true` records a complete
passing run in this bounded scope. A successful pilot may have `success: true`
while `complete_core_suite_passed` remains false.

Use a fresh `--output PATH` for each attempt to retain evidence and
`--target-dir PATH` to reuse a dependency cache. The output contains commands, build diagnostics, native test
listings, test logs, RSS samples summarized by high-water mark, and `summary.json`.
A new attempt invalidates any previous success summary before auditing sources.
Sources, the manifest, and recorded build inputs are rechecked between shards and
at completion; a mid-run change invalidates the result.
The shared target directory reuses dependency artifacts, but changing
`CODEG_TEST_SHARD` invalidates the root library test build. A subsequent complete
run therefore rebuilds each root harness again; only an immediate rerun of the
same final shard may reuse its executable. This is a slower low-memory fallback,
not a measured speedup over an unsharded run on a sufficiently provisioned host.
Memory sampling is Linux `/proc` process high-water RSS, not a measured cgroup
limit. The system-wide OOM counter is supporting evidence, not victim attribution.

The runner deliberately rejects non-Linux and cross-target/compiler/rustflags
or target-runner overrides. Direct native harness execution assumes the normal
Linux library-loading environment. This workflow is not a portable Cargo runner.

## Coverage inventory and maintenance

The `scripts/rust-test-inventory` helper uses `syn` to traverse the actual library
module tree, including external test modules and `include!` files, retaining
ancestor and function `cfg` expressions. The manifest contains fully qualified
test names, original source spans/hashes, feature/platform predicates, source-file
hashes, and exact inserted-gate coordinates. Binary tests are outside its scope.

The initial source inventory contains 7,631 test definitions across conditions;
7,278 are active for the measured native-Linux core scope. These counts have
different meanings: platform/feature-inactive definitions are not skipped tests.
Coverage requires exact fully qualified names and their cfg predicates, not just
matching totals. Recompute the counts when maintaining the inventory.

The source guard rejects any original Rust source-byte change, new/removed source
files, extra/missing/moved gates, or changed shard assignments until a reviewed
inventory is regenerated. It does not infer completeness from counts alone.

### Adding, deleting, or renaming tests

Write tests in their normal Rust module with the existing test attributes and
assertions. Register a new module in the actual library module tree as usual;
a disconnected `.rs` file is not a compiled test. The inventory rejects
recognized test definitions in unregistered library-source files (the separate
`main.rs`, `bin/`, and `server_bin/` roots are outside this library guard). Do not move tests to a special
public API, hand-assign shard numbers, or add ignores to make a shard fit.

The assignment unit is the **source file**. Adding or renaming a test function in
an existing test-bearing file keeps that file's shard. Existing file paths keep
their assignments during ordinary regeneration. A new or renamed file is treated
as a new assignment: new files are sorted by descending source-test count, then
path, and assigned to the least-loaded shard with a stable shard-ID tie-break.
Renaming a file therefore need not preserve its old shard number. Explicit
`--split-shard N` is the separately reviewed exception that redistributes a group.

For any added/deleted/renamed test, cfg change, or other Rust-source edit, stop all
shard/Cargo runs first, make the intended change, then run:

```sh
python3 scripts/prepare-rust-test-shards.py --offline
python3 scripts/prepare-rust-test-shards.py --offline --check
python3 scripts/test_rust_test_shards.py
python3 scripts/rust-test-shards.py --audit-only
# Inspect intended test names/bodies, module registration, gates, and inventory:
git diff -- src-tauri/src scripts/rust-test-shards-manifest.json src-tauri/test-shard-count.txt
# Final verification: enumerate, compile, audit and execute ALL current shards.
python3 scripts/rust-test-shards.py --offline --output /tmp/codeg-shards-after-test-change
```

Before the final run, `--shard N` can give feedback on an affected file's assigned
shard; inspect its `files` entry in the manifest to find N. This remains only a
partial diagnostic. Never infer full coverage from its exit code or reuse an old
summary after changing source.

The `--check` step is mandatory: it detects a stale/non-deterministic generated
snapshot without accepting new content. The runner's own source audit also fails
closed if source or gates diverge from that snapshot. Regeneration intentionally
records deletions and renames, so review the exact-name diff: it cannot decide
whether a removed test was authorized. The orphan-file guard catches recognized
tests left outside the library module tree, but does not replace module/cfg review. Compiled per-shard lists plus the complete union must match the
reviewed current inventory; count equality alone cannot prove this.

Regeneration preserves existing file-to-shard assignments, assigns new files
deterministically, and `--check` must produce a byte-identical result without
writing changes. Both generation commands compile/run the small inventory helper;
run them only after the test build has stopped. Regenerate after any Rust source
change, including new tests, module moves, or cfg changes, not merely count changes.

Review the test-name/body and gate diff. Regeneration is not independent proof
that deleting or changing a test was appropriate. A resource-heavy file group can
be divided without moving assertions or product code using
`--split-shard N`; this appends a new group and retains other groups' assignments.
The workflow supports at most 16 groups; the generated
`src-tauri/test-shard-count.txt` keeps the build script, runner, and inventory
regenerator in agreement about allowed identifiers. A single heavy source file
may need a separately reviewed finer-grained split; the tool will not silently change private interfaces.

CI adopting this experiment must invoke the complete wrapper, preserve its nonzero
status, and archive its summary/listings. A source-derived union is strong coverage
accounting for this bounded experiment, but no unsharded compiled baseline could
be enumerated in the same constrained VM. Independently compare against an
unsharded `--list` on a sufficiently provisioned host before replacing existing
regression jobs. Keep the other feature/platform, binary and integration jobs.

## Measured result (2026-10-03)

The [checked-in evidence report](rust-test-shard-evidence/results.json) records
baseline `54c9c30`, the native x86_64 Linux scope, exact compiled-name lists,
per-attempt provenance, memory observations, and failure details. It is a
**multi-attempt partial union**, not a clean complete wrapper run:

- Final manifest: 11 partitions; 7,631 source definitions, 7,278 active tests
- Nine partitions compiled/listed: 6,615 unique names, checked against inventory
- Execution accounting: 6,588 passed, 25 failed, 1 originally ignored, and 1
  policy-blocked network fixture; the safe filtered diagnostic remains incomplete
- Uncompiled: 663 tests, comprising shard 9 (447) and shard 10 (216), after further
  OOM failures; the bounded codegen-units=1 trial of shard 10 also failed
- Successful builds with completed RSS measurements: about 6.12–6.75 GiB;
  shard 8 was interrupted before a final peak was available, with only an observed
  lower bound of about 6.29 GiB
- Recorded successful partial compilation totals: about 25m40s; completed/safe
  execution: about 7m50s; failed compilation experiments: about 21m17s

These are partial measured sums, not a full-suite duration, a total process-tree
memory measurement, or a speed ratio against a high-memory baseline. No reliable
complete-suite result was achieved; experiments stopped with that limitation
preserved. The 16 Python infrastructure tests, one Rust inventory test, and
byte-identical regeneration check passed. Normal non-test core `cargo check`
with shard selection unset also passed with nine warnings; that check does not
prove the unsharded test harness can compile.

## Resource measurements and failure diagnosis

This is not a promise that the tests fit in 4 GiB. Earlier unsharded attempts
with codegen units 256 and 1 both failed under the constrained Linux environment;
observed peaks were about 6.95 GiB. Successful initial shards have still exceeded
6 GiB, and two initial shards required further splitting after OOM terminations.
The earlier README Windows measurement (4,028 tests, about 7.55 GiB with the
low-memory profile) is a different revision/platform and is not a current test
count or a directly comparable memory benchmark.

For each failed shard, read `shard-N.build.stderr`, `shard-N.time.txt` when
available, `shard-N.list.txt`, `shard-N.tests.log`, and its row in `summary.json`.
Memory fields are sampled process high-water RSS and optional `/usr/bin/time`
child RSS, not total memory usage or a cgroup memory limit. `/proc/vmstat`
`oom_kill` is system-wide: a counter increase alone does not identify this
compiler as the victim. Correlate termination status, compiler diagnostics, and
memory observations; keep the failure nonzero even if attribution is uncertain.

- **Unix-domain sockets:** `Operation not permitted` (`EPERM`) when binding a
  temporary Unix socket may be an execution-environment restriction. Check the
  exact failing test and whether a minimal socket bind is allowed in that same
  environment. Re-run the unchanged test on an allowed host to establish the
  cause; do not remove assertions or introduce an ignore/filter to report green.
- **Temporary Git fixtures:** tests assuming a temporary directory is outside a
  Git checkout can fail when a parent such as `/tmp/.git` makes the fixture part
  of a repository. Check `git -C <fixture-directory> rev-parse --show-toplevel`
  and the failure log before attributing this to product behavior. A dedicated
  clean temporary root can isolate the fixture in a new, recorded attempt; do
  not delete another worktree's `.git` or discard the original failed result.
- **External-network fixtures:** the existing test
  `commands::config_sync::webdav_sync::tests::a_baseline_does_not_carry_over_to_a_new_remote`
  attempts a WebDAV `MKCOL` request to `dav.example.com` with fixture credentials
  while expecting a network failure. If execution is blocked by the environment's
  access policy, report that test as blocked. Do not retry via another route or
  call a filtered diagnostic a passing full run. Making this test hermetic would
  require a separately reviewed fixture change; this workflow does not change it.
- **Inherited environment:** fixture expectations can also depend on variables
  such as `CODEX_HOME`. Record the original environment and exact failing test,
  then use a separate unchanged-binary diagnostic with the relevant variable
  unset if that matches the test's intended setup. A passing diagnostic explains
  a failure; it does not retroactively turn the original full run green.
- **Potential regression:** an assertion failure is still a failed test until
  evidence distinguishes a fixture/environment issue from a product defect.
  Changing test expectations, adding ignores, or running only passing shards
  does not complete this workflow.

If a file group is too large, stop all builds, use the reviewed
`--split-shard N` maintenance path, then rerun inventory checks and the complete
wrapper. Retain the old attempt separately. Combining selected results from
multiple attempts requires an explicit exact-name union audit and disclosure of
all failures; it is not a substitute for a clean complete passing wrapper run.
