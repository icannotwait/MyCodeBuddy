# EUI-NEO Frontend Spike Plan Review (Codex)

## Review Basis

| Item | Value |
| --- | --- |
| Role | Independent Plan Reviewer (Codex), separate from the Plan Author |
| Work unit | `plan\|docs/superpowers/plans/2026-08-09-eui-neo-frontend-spike.md\|reviewer\|codex\|none` |
| Design | `docs/superpowers/specs/2026-08-09-eui-neo-frontend-spike-design.md` |
| Design digest (verified) | `sha256:b3446ec31cc8b0457ed1ca3e7c6e8b3ec421eb4b997af6efae3d8975ecd2bdef` |
| Plan | `docs/superpowers/plans/2026-08-09-eui-neo-frontend-spike.md` |
| Plan digest (verified) | `sha256:4256189d01c83f97adb8c53f04952eebf5d73c3d7a627f8e940e38422792a2db` |
| Plan commit (verified) | `6a573c602817b77887b4dbcf2d6a8c96e04f2f19` (`docs: plan EUI-NEO frontend spike`) |
| Policy | `b2d_task_risk_v1` |

Both required digests were recomputed with `sha256sum` and match the review
brief and Author report. The Plan commit exists, has the reported subject, and
adds only the Plan file. Repository API spot checks were made against the
current `AppState`, ACP settings/session cores, `SessionState`,
`ConnectionManager`, logging, and frontend send/render paths.

## Overall Assessment

The Plan has a sound milestone decomposition, a correct risk-routing matrix,
strong coverage of most M0-M6 contracts, and no human mid-sequence gate. It is
not ready for admission unchanged, however. Seven Important findings affect
observable bridge semantics, performance validity, executable delivery
commands, acceptance evidence, recovery from missed interaction events, and
the required TDD granularity. Two Minor inconsistencies should be corrected in
the same revision.

**Verdict: `request_changes`**

## Findings

### Critical

None.

### Important

#### I1. Shutdown cannot satisfy the public exactly-once completion contract

The Plan requires every accepted request to produce exactly one terminal
completion (lines 21-24 and 692-704), including cancellation during shutdown
(lines 710-719). At the same time, all public calls are UI-thread-only,
`codeg_eui_shutdown` is synchronous, poll is legal only in `running`, and
shutdown frees the last frame before entering `stopped` (lines 18-20, 674-690,
and 710-719). A request terminalized while the UI thread is blocked inside
shutdown therefore has no later successful poll through which C++ can observe
its completion. An internal test helper counting terminalizations is not the
public frame contract required by design lines 282-292.

Required revision: choose and document an externally observable shutdown
protocol. For example, add an asynchronous stopping/drain phase during which
poll remains legal until all accepted IDs are exposed, then allow the final
synchronous free/join; or explicitly change the approved completion guarantee
and obtain a design revision. Add a black-box ABI test that accepts a blocked
request, initiates shutdown, and proves how C++ observes its one terminal
result before its bytes are freed.

#### I2. The WebView primary latency marker is before paint, not after presentation

Design line 492 defines `t_first_presented` as the first paint/presentation of
assistant text. Task 9 correctly delays the EUI marker until an `onFrame`
callback after the prior EUI frame was presented (Plan line 1435), but the
WebView path schedules one `requestAnimationFrame` from `useLayoutEffect` and
timestamps that callback (lines 1437-1453). A layout effect runs after DOM
commit but before paint, and the scheduled RAF callback also runs before that
frame's paint. The two shells therefore use opposite sides of presentation,
invalidating the primary comparison.

Required revision: define one equivalent post-presentation proxy for both
shells. In the current React path, the existing two-RAF pattern is a viable
candidate: the first RAF precedes the first eligible paint and the second runs
after it. Add scheduler-driven hook tests that distinguish commit, pre-paint,
paint, and post-paint ordering; metric arithmetic tests alone do not validate
the marker.

#### I3. The prescribed Git/artifact sequence stages generated output and cannot commit the Final report

Task 1 runs Cargo and CMake builds (lines 318-326), then recursively stages
`codeg-eui` and `src-tauri/codeg-eui-core` (lines 330-337). The current ignore
rules do not ignore `codeg-eui/build-contract`, the build directory created by
`build.sh`, performance result directories, or
`src-tauri/codeg-eui-core/target`. `src-tauri/.gitignore` only anchors
`/target/` at `src-tauri/target`, so the nested target is not covered. The
recursive `git add` can therefore commit build trees and binaries, while Task
10 later demands a clean worktree (lines 1578-1586).

The Final report command has the inverse problem: root `.gitignore` ignores
`.superpowers`, so the plain `git add` at lines 1768-1777 cannot stage the new
report. Performance output has the same unresolved lifecycle: Task 11 requires
`codeg-eui/results/local-comparison.json`, but the Plan neither commits it nor
adds an ignore rule before requiring a clean status.

Required revision: add explicit ignore ownership in Task 1/Task 9 (for example,
`codeg-eui/.gitignore` and `src-tauri/codeg-eui-core/.gitignore`), stage exact
source paths instead of parent directories, and define whether the aggregate
JSON is a tracked evidence artifact or ignored local evidence. Use `git add -f`
for the intentionally ignored Final report, or change the report location and
allowlist consistently. Add dry-run/status checks before each producer commit.

#### I4. Final Rust regression coverage is narrower than the design acceptance gate

The implementation changes shared `AppState`, logging, document translation,
ACP settings/session facades, and frontend integration points. Design lines
516-520 and acceptance line 570 require default builds and tests to remain
independent of EUI. Task 11 Step 2 runs only the new facade test module plus
library clippy (Plan lines 1639-1649); Step 5 runs `cargo check` for desktop,
server, and MCP while EUI sources are absent (lines 1672-1691). No broad shared
Rust test suite, server test suite, full desktop regression, or server/MCP
clippy route is included.

Required revision: add the repository-prescribed broad Rust gates, including at
least `cargo test --lib --features test-utils`, the server-mode test command,
and server/MCP clippy commands. Because optionality is an acceptance property,
run the relevant compile/test gates while the EUI source directory is held
aside, or otherwise prove that tests also resolve without that source. Keep
the focused EUI tests, but do not substitute them for shared-core regression.

#### I5. The Plan weakens the Grok-and-Codex E2E goal into a skippable check

The approved design requires a real streaming loop with Grok and Codex (Goal 2,
lines 43-46), lists `Linux real Grok + real Codex streaming` in the testing
strategy (line 519), and conditions acceptance on correct local agent setup
(lines 566-569). The performance scenario permits documented skips when an
agent is not installed, but that allowance does not replace the product-loop
goal.

Task 9 requires only one installed agent to produce a filled row (Plan lines
1486-1497), and Task 11 treats either Grok or Codex as an acceptable
`agent not installed` skip (lines 1714-1718). Delivery can therefore complete
without real evidence for one of the two required session/send/stream paths.

Required revision: make the prepared producer host have working Grok and Codex
for the E2E acceptance run, or state that inability to run either required
agent blocks Final delivery pending an explicit scope/design exception. Keep
the design's skip notation for additional performance rows, but do not use it
to satisfy the dual-agent product-loop acceptance item.

#### I6. Snapshot recovery does not explicitly decline interactions already pending in the snapshot

The canonical attach/recovery path takes an authoritative snapshot before it
subscribes (Plan lines 1045-1089). The decline implementation is described in
terms of received permission/question/plan events (lines 1091-1116). If attach
occurs after such an event, or lag/saturation loses it, the event will not be
replayed after the snapshot cursor; only `pending_permission`,
`pending_question`, or `pending_plan_approval` in `SessionState::to_snapshot()`
reveals the parked responder. The Plan does not explicitly require
`replace_from_snapshot` to inspect and terminally decline those pending values.
That can violate the design's no-parked-responder requirement (design lines
450-465) on exactly the recovery path meant to handle loss.

Required revision: make initial attach and every authoritative resync invoke
the same deduplicated decline policy for pending interactions found in the
snapshot before resuming at `event_seq + 1`. Add tests that begin from snapshots
containing each pending interaction type without a corresponding subsequent
event and prove the responder resolves once and the turn reaches `t_end` or a
hard error.

#### I7. Several producer steps are not bite-sized RED-GREEN units

The `writing-plans` contract requires one small action per step, actual code for
implementation steps, and test-first behavior changes. Multiple steps still
delegate substantial design work to the implementer:

- Task 7 Steps 5-7 implement the entire shell, chat, and settings pages in
  prose, then Step 8 adds their page-state tests (Plan lines 1225-1239).
- Task 8 adds crash/settings/cancel UI behavior in Steps 3-5 and only extends
  C++ tests in Step 6 (lines 1322-1342).
- Task 9's initial RED tests cover metric arithmetic, while presentation-hook
  ordering, RSS sampling, metadata validation, and script subcommands are
  implemented before the broad/self-test step (lines 1399-1484).
- Large implementation steps such as Tasks 3, 6, and 7 omit the actual module
  skeletons/signatures needed to connect the named interfaces and are much
  larger than a 2-5 minute action.

This also conflicts with the Plan's own global statement that every producer
Task writes focused failing tests before implementation (lines 37-38).

Required revision: split UI, bridge, projector, instrumentation, and script
work into test-first substeps with one observable behavior each. Put each page
or hook's failing test before its implementation and show the concrete EUI/C++
or TypeScript skeleton needed to compile it. Preserve milestone-sized review
Tasks if desired, but make their internal steps executable without fresh design.

### Minor

#### M1. Non-empty `codeg_eui_init` data-dir semantics diverge from the approved ABI

Design lines 252-254 imply that a non-empty `data_dir` argument supplies the
root while an empty argument resolves the documented default. Plan line 432
instead rejects any non-empty argument that disagrees with
`CODEG_EUI_DATA_DIR`, and the C++ entry always passes empty. Either make the
argument authoritative and process-pin it using the same isolation rules, or
remove it in a deliberate ABI/design revision. Do not retain an undocumented
agreement requirement between two root inputs.

#### M2. `perf_compare.sh` declares fewer subcommands than the Plan executes

Task 9 says the script accepts `record-eui`, `record-webview`, and `aggregate`
(Plan lines 1455-1457), then invokes `self-test` at line 1481 and both
`self-test` and `validate` in Task 11 lines 1722-1725. Add `self-test` and
`validate` to the produced interface, implementation steps, usage/error tests,
and README contract.

## Design Coverage Audit

| Requirement | Plan coverage | Review status |
| --- | --- | --- |
| M0 optional staticlib, pin, CMake hello, ABI link | Task 1 | Covered; Git artifact handling must be repaired (I3) |
| M1 isolated root and `WebOnly` AppState | Task 2 | Covered; init argument mismatch is M1 |
| Async FFI, bounded queues, frames, lifecycle | Task 3 | Mostly covered; shutdown/completion contradiction is I1 |
| M2 Grok/Codex settings and probe | Task 4 | Covered through existing ACP helpers |
| M3 workspace, conversation, history, spawn/resume, send | Task 5 | Covered with repository-grounded core paths |
| M4 snapshot/subscribe, resync, live projection, decline | Task 6 | Recovery covered; pending-snapshot decline gap is I6 |
| Native shell/chat/P0 settings | Task 7 | Functional scope covered; execution granularity is I7 |
| M5 switch/cancel/error/P1 | Task 8 | Covered; TDD ordering is part of I7 |
| M6 comparable metrics and filled table | Task 9 | Partial until presentation anchors are equivalent (I2) |
| Real Grok and Codex streaming | Tasks 5-7, 11 | Partial; unavailable-agent skip weakens acceptance (I5) |
| Default product independence | Tasks 1, 10, 11 | Compile checks covered; required broad tests are missing (I4) |
| Final evidence/report | Tasks 10-11 | Covered in intent; prescribed Git commands fail (I3) |

The four minors from the approved design re-review are otherwise carried into
the Plan: `int codeg_eui_shutdown(void)`, ambient `CODEG_HOME` removal,
successful-poll completion draining with retained bytes, and a fixed shared
50 ms threshold.

## `b2d_task_risk_v1` Audit

The global and local matrices contain 11 rows each and agree. Recomputed soft
totals, final levels, and routes are:

| Task | Hard trigger result | Soft total | Expected level | Expected route | Plan match |
| --- | --- | ---: | --- | --- | --- |
| 1 | `unsafe_ffi` (and claimed `public_compatibility`) | 5 | high | Codex + separate Codex/Grok review | Yes |
| 2 | `security_trust_boundary`, `concurrency_lifecycle` | 2 | high | Codex + separate Codex/Grok review | Yes |
| 3 | `unsafe_ffi`, `concurrency_lifecycle` | 4 | high | Codex + separate Codex/Grok review | Yes |
| 4 | `security_trust_boundary` (and claimed `public_compatibility`) | 2 | high | Codex + separate Codex/Grok review | Yes |
| 5 | `concurrency_lifecycle` | 5 | high | Codex + separate Codex/Grok review | Yes |
| 6 | `concurrency_lifecycle`, `security_trust_boundary` | 4 | high | Codex + separate Codex/Grok review | Yes |
| 7 | none | 5 | high | Codex + separate Codex/Grok review | Yes |
| 8 | `concurrency_lifecycle` | 4 | high | Codex + separate Codex/Grok review | Yes |
| 9 | none | 6 | high | Codex + separate Codex/Grok review | Yes |
| 10 | none | 1 | normal | Grok + Codex review | Yes |
| 11 | none | 3 | high | Codex + separate Codex/Grok review | Yes |

All arithmetic is correct, no hard-trigger Task is under-routed, soft-only
Tasks at or above 3 are high, and Task 10 is correctly the sole normal route.
The Author is Codex, every high implementer is Codex, and normal Task 10 is
Grok. The risk matrix requires no correction.

## Human Gate Audit

No mid-sequence human UAT, manual sign-off, or user-approval gate exists.
Producer native-window, real-agent, and performance evidence is run by agents;
Task 10 proceeds to Task 11 without human acceptance; Final review uses the two
specified independent agent reviewers; human evaluation is confined to the
post-delivery residual section. This portion passes the brief.

## Required Revision Summary

1. Reconcile shutdown with externally observable exactly-once completions.
2. Use equivalent post-presentation markers and tests in both shells.
3. Make generated-output, performance-artifact, and ignored-report Git handling executable.
4. Add broad default/shared Rust tests and route-specific clippy gates.
5. Require real E2E evidence for both Grok and Codex or an explicit scope exception.
6. Decline interactions found in initial/resync snapshots, not only live events.
7. Split large prose implementation steps into concrete test-first actions.
8. Resolve the two Minor interface inconsistencies.

VERDICT: request_changes

<!-- codeg-card-summary-v1
{"kind":"review","verdict":"request_changes","critical":0,"important":7,"minor":2,"summary":"Risk routes and no-human-gate policy pass; revise shutdown completions, perf anchors, Git evidence flow, regressions, dual-agent E2E, snapshot decline, and TDD granularity.","report_file":".superpowers/sdd/2026-08-09-eui-neo-frontend-spike/plan/plan-review-codex-report.md"}
-->
