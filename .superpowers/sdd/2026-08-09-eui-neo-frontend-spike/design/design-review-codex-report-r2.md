# EUI-NEO Frontend Spike Design Re-review (Codex R2)

## Review Basis

- Revised requirements baseline:
  `docs/superpowers/specs/2026-08-09-eui-neo-frontend-spike-design.md`
- Verified SHA-256:
  `b3446ec31cc8b0457ed1ca3e7c6e8b3ec421eb4b997af6efae3d8975ecd2bdef`
- Revision commit verified:
  `066ce16401cbd5de0822f5f721806f6624f1eade`
  (`docs: tighten EUI-NEO spike design bridge contracts`)
- Prior review re-read:
  `.superpowers/sdd/2026-08-09-eui-neo-frontend-spike/design/design-review-codex-report.md`

## Overall Assessment

The revision addresses all six prior blocking findings with chosen, testable
contracts. The same-process EUI/Tokio host remains feasible; the design now
defines the asynchronous FFI protocol, frame ownership, snapshot recovery,
process-wide data-root behavior, terminal interaction degradation, and
comparable performance anchors well enough for a `writing-plans` author to
decompose the work.

No new blocking issue was found. Four small inconsistencies should be resolved
in the implementation plan or by an editorial design follow-up, but none
changes the architecture or prevents planning.

## Prior Blocking Findings

### 1. Async commands versus the non-blocking UI thread

**R2 status: ADDRESSED**

Evidence: **Threading and call affinity** and **Asynchronous command contract
(chosen)** (design lines 203-292) make all non-lifecycle operations enqueue-only
and return a monotonic `request_id`. Real results arrive through
`CodegEuiFrame.completions[]`; `create_session`, settings, and probe no longer
return synchronous DB/agent results. The design also defines one terminal
completion per accepted request, stale-result behavior after selection changes,
and slow-probe/non-blocking contract tests.

### 2. FFI frame ownership and shutdown safety

**R2 status: ADDRESSED**

Evidence: **Threading and call affinity**, **Bridge lifecycle**, **Input rules**,
and **Frame ownership (chosen)** (design lines 203-307) establish UI-thread call
affinity, lifecycle states, shutdown ordering, panic containment,
pointer-and-length input validation, and one immutable frame backing store that
survives until the next successful poll or completed shutdown. The bridge-test
matrix at lines 514-520 covers the important failure and lifecycle cases.

The `void` shutdown prototype versus the text requiring an error code is a
non-blocking minor listed below; it does not reopen the ownership/lifecycle
decision.

### 3. Event overflow and final-state convergence

**R2 status: ADDRESSED**

Evidence: **Event surface (Rust -> UI) - snapshot + subscribe with recovery**
(design lines 309-341) chooses the existing per-connection sequence-numbered
snapshot-and-subscribe pattern as canonical. It explicitly demotes
`InternalEventBus` from lossless frontend transport, detects gaps/lag/local
saturation, performs authoritative `SessionState` resync, avoids producer
blocking, and requires race, overflow, session-switch, and final-parity tests.

### 4. EUI data-root isolation from the main app

**R2 status: ADDRESSED**

Evidence: **Data directory (single process-wide root)** (design lines 177-199)
resolves the EUI root before Tokio/logging/core initialization, overwrites an
ambient main-app `CODEG_DATA_DIR`, uses the result for SQLite, `AppState`, logs,
credentials, and child inheritance, and explicitly leaves native Grok/Codex
configuration shared. The testing strategy at line 516 repeats the ambient
main-root isolation requirement.

`CODEG_HOME` precedence needs one wording cleanup, but the default database
isolation contract is now unambiguous and testable.

### 5. Unsupported interactive prompts parking turns

**R2 status: ADDRESSED**

Evidence: **Error handling and degradation** (design lines 450-465) selects a
terminal P0 policy: reject/deny ACP permissions or cancel through the existing
core, disable or immediately decline `ask_user_question`, and terminally
decline plan approvals and other unsupported interactions. It expressly
forbids parked responders and requires Grok/Codex tests to reach `t_end` or a
hard error.

### 6. First-visible and jank measurement validity

**R2 status: ADDRESSED**

Evidence: **Performance measurement** and **Instrumentation (common anchors)**
(design lines 470-510) separate diagnostic `t_first_token` from primary
`t_first_presented`, define presentation points for EUI and WebView, align `t0`,
choose presented-frame interval p95 and a long-interval count during the active
stream, define warm-up/measured runs, limit RSS to the shell process, and list
the required result metadata and table columns.

## New Blockers

None.

## Non-Blocking Minors

1. **MINOR - shutdown signature:** **Bridge lifecycle** says double shutdown
   returns an explicit error code (lines 224-225), while the illustrative ABI
   declares `void codeg_eui_shutdown(void)` (line 254). The plan should use an
   `int` return or revise the double-shutdown contract to a documented idempotent
   no-op.
2. **MINOR - `CODEG_HOME` precedence:** Lines 189-191 promise the same root for
   logs/support state while deferring to existing rules; current `paths.rs`
   gives non-empty `CODEG_HOME` precedence over `CODEG_DATA_DIR` for logs,
   uploads, pets, timing, and ACP transcripts. The plan should either pin/clear
   `CODEG_HOME` for EUI or document it as an explicit operator override and
   narrow the one-root claim/test accordingly.
3. **MINOR - completion consumption:** The design guarantees exactly one
   terminal completion but does not say when entries leave the pending
   completion set. The plan should specify that a successful poll atomically
   transfers/drains the included completions (or use an explicit acknowledgment)
   and define bounded-queue behavior so a completion cannot disappear before a
   frame exposes it.
4. **MINOR - jank threshold:** The metric requires a fixed long-interval
   threshold but currently gives `50 ms` as an example (line 497). Freeze one
   shared value for both shells in the plan/performance protocol before
   collecting the filled comparison table.

## Plan Readiness

The design is ready for `writing-plans`. The plan should carry the four minors
as explicit ABI/bootstrap/instrumentation decisions and preserve the revised
contract tests as milestone acceptance criteria. No design-level architecture
choice remains open in a way that blocks task decomposition.

VERDICT: approve_with_minors

<!-- codeg-card-summary-v1
{"kind":"review","verdict":"approve_with_minors","critical":0,"important":0,"minor":4,"summary":"Design r2: six blockers addressed; four minors for Plan Author.","report_file":".superpowers/sdd/2026-08-09-eui-neo-frontend-spike/design/design-review-codex-report-r2.md"}
-->
