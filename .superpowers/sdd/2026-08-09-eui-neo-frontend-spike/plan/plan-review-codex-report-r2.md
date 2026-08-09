# EUI-NEO Frontend Spike Plan Re-review (Codex R2)

## Review Basis

| Item | Value |
| --- | --- |
| Role | Independent Plan Reviewer (Codex), separate from the Plan Author |
| Work unit | `plan\|docs/superpowers/plans/2026-08-09-eui-neo-frontend-spike.md\|reviewer\|codex\|none` |
| Revised Plan | `docs/superpowers/plans/2026-08-09-eui-neo-frontend-spike.md` |
| Revised digest (verified) | `sha256:06ad8b84d175fde69662e2b29a6a7f975554f69f962fbda165ee2c6b37e50767` |
| Revision commit (verified) | `255f965c607fb7cb42bbdf70008b33f0144e49ec` (`docs: revise EUI-NEO spike plan after review`) |
| Parent Plan commit | `6a573c602817b77887b4dbcf2d6a8c96e04f2f19` |
| Prior review | `.superpowers/sdd/2026-08-09-eui-neo-frontend-spike/plan/plan-review-codex-report.md` |

The digest was recomputed with `sha256sum`. Commit `255f965c` is a direct child
of the prior Plan commit and changes only the Plan. `git show --check` and the
revision diff whitespace check both exit zero.

## Overall Assessment

The revision fully addresses I1-I6 and M1-M2. It also substantially improves
I7 by splitting the bridge, live projector, native pages, M5 controls, and
performance work into explicit RED/GREEN pairs with concrete interfaces.

I7 is not fully addressed because the new C++ RED/GREEN plan has no executable
test harness or complete target-registration contract. The only explicit CMake
test targets are ABI layout and shutdown drain, while the later suites use
undefined GoogleTest-style macros. As written, several `ctest -R` commands can
select zero tests rather than prove RED or GREEN. This is an unresolved
Important plan-executability issue.

**Verdict: `request_changes`**

## Prior Finding Disposition

### I1. Shutdown cannot satisfy the public exactly-once completion contract

**Status: ADDRESSED**

Evidence:

- Global constraints define `codeg_eui_begin_shutdown`, legal stopping polls,
  and final shutdown only after a successful frame exposes
  `shutdown_ready=1` (Plan lines 19-20).
- Task 3 freezes the visibility invariant: the ready frame also exposes every
  remaining accepted completion, and final shutdown is `NOT_READY` before that
  frame is observed (lines 643-645).
- Rust and black-box C++ RED tests cover a blocked accepted request, stopping
  poll, copied cancelled completion, and final free (lines 870-913).
- The implementation order prevents final shutdown from creating an
  unobservable completion (lines 915-929), with full verification at lines
  953-965 and C++ close-loop coverage at lines 1585-1647.

The public consumer now has an observable drain phase before join/free.

### I2. WebView primary latency marker is before paint

**Status: ADDRESSED**

Evidence:

- The global marker contract now uses EUI next-update `onFrame` and WebView
  RAF2 after an eligible paint opportunity (line 34).
- Task 9 adds an explicit EUI compose/present/next-update phase test before
  implementation (lines 2047-2081).
- WebView tests distinguish `commit -> RAF1 -> paint -> RAF2` and require the
  marker only at RAF2 (lines 2083-2107).
- The implementation schedules and cancels both RAF handles, enforces one mark
  per run, and records only at RAF2 (lines 2109-2129).

The two shells now use equivalent post-presentation proxies with ordering
tests, not only metric-arithmetic tests.

### I3. Git/artifact sequence stages generated output and cannot commit the Final report

**Status: ADDRESSED**

Evidence:

- Global constraints forbid recursive parent staging and define generated
  artifacts as ignored local evidence; ignored delivery reports use
  `git add -f` (line 40).
- Task 1 creates both scoped ignore files before any build, covering CMake
  builds/results/screenshots and the nested Cargo target (lines 347-360).
- Producer commits use exact source allowlists plus `git add --dry-run`, staged
  name inspection, and untracked-file status checks; Task 1 is explicit at
  lines 378-390 and Task 9 at lines 2227-2239.
- Performance JSON remains ignored local evidence while aggregate values are
  copied into the tracked README (lines 2195 and 2213-2225).
- Task 10 verifies source cleanliness separately from expected ignored paths
  (lines 2310-2319).
- Final delivery uses `git add -f` and verifies that the report is the sole
  staged path (lines 2518-2530).

### I4. Final Rust regression coverage is too narrow

**Status: ADDRESSED**

Evidence:

- Task 11 moves the EUI source aside under a trap and then runs default check,
  library tests, the full `test-utils` regression, desktop all-target clippy,
  server check/tests/clippy, and MCP check/clippy (lines 2408-2438).
- The expected result explicitly requires all those routes to pass while EUI
  sources are unavailable and after restoration leaves no status change
  (lines 2439-2440).

### I5. Grok-and-Codex E2E goal is weakened into a skip

**Status: ADDRESSED**

Evidence:

- Global constraints require successful real product-loop evidence for both
  agents and make missing installation/credentials/launchability a delivery
  blocker absent an approved design revision (line 39).
- Traceability requires both agents to present non-empty text and complete
  successfully (line 124).
- Task 11 requires working Grok and Codex, successful probe/session/send,
  presented assistant text, and successful `TurnComplete`; no primary path may
  be converted to a skip (lines 2463-2467).
- The Final failure protocol preserves that blocker (line 2480).

Performance-only skip notation remains separate at lines 2213-2225, matching
the approved design's performance allowance without weakening product-loop
acceptance.

### I6. Snapshot recovery does not decline interactions already pending in the snapshot

**Status: ADDRESSED**

Evidence:

- Task 6's interface and recovery invariant require pending snapshot
  interactions to be declined exactly once before event receive resumes
  (lines 1271-1275).
- Initial-attach and resync RED fixtures cover permission, question, and plan
  fields with no later request event, verify decline-before-receive, repeat
  resync deduplication, and terminal convergence (lines 1376-1418).
- Event and snapshot paths share one `InteractionKey`/`decline_once` function;
  every snapshot replacement reconciles all three fields before resuming at
  `event_seq + 1`, with cancel/hard-error fallback on decline failure (lines
  1420-1446).

### I7. Producer steps are not executable bite-sized RED/GREEN units

**Status: NOT ADDRESSED**

The decomposition is materially better:

- Task 3 now has separate layout/lifecycle, queue, immutable-frame, public
  shutdown, and deep-copy RED/GREEN pairs (lines 653-965).
- Task 6 separates attach, recovery, interaction, and marker behavior
  (lines 1283-1478).
- Tasks 7-9 split client/lifecycle/pages, M5 controls, presentation ordering,
  and CLI behavior into test-first pairs (lines 1526-1763, 1820-1960, and
  2009-2211).

However, the C++ path is not executable as specified:

- The complete explicit CMake block registers only
  `codeg_eui_abi_layout_test` and optional
  `codeg_eui_shutdown_drain_test` (lines 306-331).
- The Plan later uses `TEST`, `ASSERT_EQ`, `EXPECT_*`, and related macros in
  shutdown, client, lifecycle, page, M5, and metric tests (for example lines
  876-892, 1530-1540, 1589-1597, 1659-1666, 1695-1702, 1893-1900, and
  2051-2062), but defines no test harness, `main`, GoogleTest/Catch2 dependency,
  or CMake linkage anywhere.
- Task 7's RED command expects configure/compile failure for
  `client_completion` (lines 1543-1551), yet no explicit step registers that
  target. With the shown CMake, configuration succeeds and `ctest -R` may
  report no selected tests. The same gap affects the other later filters and
  can create false GREEN evidence.

Required revision: choose a self-contained headless C++ harness or a pinned
test dependency, list its files/dependency, define a CMake helper that registers
every named test executable, and make target registration part of each RED
step before its first run. Add a `ctest -N` assertion for the exact expected
test names so an empty selection is a hard failure. Keep contracts-only mode
independent of EUI/native UI packages.

### M1. Non-empty init data-dir semantics diverge from the approved ABI

**Status: ADDRESSED**

Evidence:

- A non-empty ABI argument is now authoritative; only an empty argument falls
  back to `CODEG_EUI_DATA_DIR`, XDG, or home, with no agreement requirement
  (line 25).
- Task 2 adds a conflicting argument/environment test and verifies storage is
  created only under the argument root (line 450).
- The implementation rules apply the same validation, absolutization, process
  pin, `CODEG_HOME` removal, and `CODEG_DATA_DIR` overwrite to both paths
  (lines 460-483).

### M2. `perf_compare.sh` declares fewer subcommands than it executes

**Status: ADDRESSED**

Evidence:

- Task 9's produced interface lists all five subcommands: `record-eui`,
  `record-webview`, `aggregate`, `validate`, and `self-test` (lines 1997-2001).
- RED tests require all five in help, exercise both recorders, validation,
  aggregation, and self-test through the public dispatcher, and cover usage
  failure (lines 2131-2166).
- Implementation and GREEN checks define all commands end to end (lines
  2168-2182); the README and Final validation include them (lines 2195 and
  2469-2476).

## New Critical/Important Findings

No new Critical finding. No separate new Important finding beyond the
unresolved executability portion of prior I7.

## Verdict

```text
VERDICT: request_changes
critical: 0
important: 1
minor: 0
```

<!-- codeg-card-summary-v1
{"kind":"review","verdict":"request_changes","critical":0,"important":1,"minor":0,"summary":"Eight prior findings are addressed; I7 remains because C++ RED/GREEN tests lack a defined harness and complete CMake registration, allowing empty ctest selections.","report_file":".superpowers/sdd/2026-08-09-eui-neo-frontend-spike/plan/plan-review-codex-report-r2.md"}
-->
