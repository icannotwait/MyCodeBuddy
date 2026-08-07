# Final Fixer Report (Grok)

## Identity

| Field | Value |
| --- | --- |
| work_unit_key | `final_review\|fixer\|grok\|none` |
| branch | `feat/platform-generated-completion-evidence` |
| prior Final review task | `a74116fb-b15e-48e1-90c4-2050bc1a9c76` |
| prior frozen tip | `50405e831f6ae0f7b79681c647361ef0a105380d` |
| product tip (re-review artifact) | `f970b17586556270c0c731c8543002df1b6d8bfb` |
| Plan/Design edits | none |

## Verdict

**fixed** — both Important findings from Final Codex review are closed with
production-path regressions. Five retained Minors left untouched.

## Closures

### `FINAL-CODEX-I1` — WebTransport root completion capability

**Root cause:** Snapshot HTTP responses issue `x-codeg-completion-context`, but
`WebTransport.call()` only sent bearer `Authorization` and discarded response
headers. Server auth maps bearer-only to `GlobalOperator`, which
`authorize_completion_root` rejects (403).

**Fix:**
- Capture the capability from successful `get_workflow_graph_snapshot`
  responses.
- Index by root `conversationId` and every `attention_id` projected in the
  snapshot body.
- Replay on `resolve_completion_decision`, `retry_completion_artifact`,
  `resolve_design_self_review`, and `restart_legacy_workflow` with root
  scoping (attention map for CAS; `sourceConversationId` for restart).

**Files:**
- `src/lib/transport/web-transport.ts`
- `src/lib/transport/web-transport.test.ts`

**Regression:** end-to-end WebTransport client path tests cover capture/replay,
multi-root scoping, and bearer-only mutation without a prior snapshot.

### `FINAL-CODEX-I2` — Selective Final delivery preflight

**Root cause:** `guard_final_delivery_txn` is selection-aware, but production
callers build requests via `current_final_delivery_request`, which still
required every required Final Reviewer's latest binding to equal
`current_review_round`. After roster-only add/remove, retained siblings stay
on earlier rounds → preflight returned `None` → selection-aware guard never
ran.

**Fix:**
- Align preflight with `binding_covers_current_selection` (selected =
  current round; unselected retained = earlier same-lineage rounds).
- Prefer a selected reviewer as the current-delivery anchor when present.
- Export `guard_task_final_delivery_core` / `guard_current_final_delivery_core`
  for production-path coverage.

**Files:**
- `src-tauri/src/acp/delegation/workflow/store.rs`
- `src-tauri/src/acp/delegation/workflow/mod.rs`
- `src-tauri/tests/completion_protocol_v2.rs`

**Regression:** `roster_only_final_republication_delivers_after_add_and_remove`
now drives both production cores after roster add **and** remove (Ready).

## Retained Minors (unchanged)

1. `T16-CODEX-M1` — legacy restart availability projection still absent
2. `T15-CODEX-M1` / `T16-CODEX-M2` — live Tauri command/event DTO coverage
3. `T17-CODEX-M1` — Card-authority false-positive prohibitions
4. `T17-GROK-M1` — semantic-ID-only requests outside validator fence
5. `T17-GROK-M2` — soft “cleaner completion answer” outside format-repair fence

## Commits

| SHA | Summary |
| --- | --- |
| `f970b17586556270c0c731c8543002df1b6d8bfb` | fix: close final review I1/I2 production composition gaps |

Local commits only — no push/PR. Docs report commit follows on the same
branch; re-admit Final review against `git rev-parse HEAD` after fixer
delivery (includes product + this report).

## Verification

| Check | Result |
| --- | --- |
| `pnpm exec vitest run src/lib/transport/web-transport.test.ts` | 20 passed |
| `cargo test --features test-utils --test completion_protocol_v2` | 23 passed |
| `cargo test --features test-utils --test completion_transport_parity` | 7 passed |
| Plan/Design edits | none |
| Unrelated dirt staged | none (progress/task-13/connection/launch_snapshot/publish*.json/credential bat left alone) |

## Card summary

Fixed Final I1/I2: WebTransport captures/replays root completion context; selective Final preflight reaches production guards after roster delta. Product tip f970b175. 5 minors retained.

<!-- codeg-card-summary-v1
{"kind":"fix","verdict":"fixed","summary":"Fixed Final I1/I2: WebTransport captures/replays root completion context; selective Final preflight reaches production guards after roster delta. Product tip f970b175. 5 minors retained.","critical":0,"important":0,"minor":5,"commits":["f970b17586556270c0c731c8543002df1b6d8bfb"],"report_file":".superpowers/sdd/final-fixer-report.md"}
-->
