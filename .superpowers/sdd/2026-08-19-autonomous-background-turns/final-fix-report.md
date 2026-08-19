# Autonomous Background Turns: Final Review Fix Report

Date: 2026-08-19

## Status

All four Important findings from `final-review.md` are fixed.

## Changes

1. Terminal recovery now crosses the backend/frontend boundary as the one-shot
   `background_activity.detail_refetch` field. Grok and Codex emit an
   accounting-only event when necessary, and the frontend requests detail with
   `preserveLive: true` after applying overlay upserts. Overlay retirement
   remains gated by the returned parser watermark.
2. Idle Stop now generation-fences and rolls back an admitted prompt held
   behind autonomous work. It clears the exact prompt admission state without
   foreground terminal handling; a cancelled generation still queued in the
   normal lane is discarded when received.
3. Grok and Codex now use bounded complete-record batches, a 1,024-entry
   provider-record identity LRU, 512/1,024 rotation policy, distinct IDs after
   forced rotation, and a 2 MiB serialized normalized-turn cap. Codex rollout
   authority validation is also a bounded prefix scan rather than a full-file
   read.
4. Both adapters track native file identity and length. Shrink or generation
   replacement discards partial episode/tombstone/budget state, schedules a
   detail refetch, and rebaselines to the new complete-file parser watermark.
   Codex additionally revokes rollout authority and requires a fresh exact
   `session_meta.payload.id` proof.

## Covering Tests

- Frontend terminal event requests `refetchDetail(..., { preserveLive: true })`
  while retaining the just-applied overlay.
- Session-state queued rollback rejects stale generations and clears only the
  exact admitted generation without foreground terminal/suspension effects.
- Shared bound tests cover the 1,024 identity LRU, 512/1,024 rotation decision,
  and 2 MiB serialized payload cap.
- Grok and Codex adapter tests cover 1,025-record forced segmentation, distinct
  part IDs, terminal-only post-rotation closure, and bounded retained state.
- Grok and Codex replacement tests cover episode reset, refetch scheduling,
  watermark rebaseline, and Codex authority revocation/re-proof.

## Verification

- `cargo test --lib --features test-utils acp::grok_autonomous::`
  - PASS: 17 passed, 0 failed.
- `cargo test --lib --features test-utils acp::codex_autonomous::`
  - PASS: 15 passed, 0 failed.
- `cargo test --lib --features test-utils acp::autonomous_activity::tests::`
  - PASS: 7 passed, 0 failed.
- `cargo test --lib --features test-utils acp::session_state::tests::queued_prompt_rollback_is_generation_fenced_and_not_a_foreground_terminal -- --exact`
  - PASS: 1 passed, 0 failed.
- `cargo test --lib --features test-utils acp::event_stream::tests::estimate_never_undercounts_serialized_for_background_activity -- --exact`
  - PASS: 1 passed, 0 failed.
- `pnpm test -- src/contexts/acp-connections-context.test.tsx -t "terminal background_activity requests a preserved-live detail refetch without dropping its overlay"`
  - PASS: 1 passed, 0 failed (226 skipped by filter).
- `pnpm eslint src/contexts/acp-connections-context.tsx src/contexts/acp-connections-context.test.tsx src/lib/types.ts`
  - PASS: 0 errors; 9 pre-existing exhaustive-deps warnings.
- `cargo fmt -- --check`
  - PASS.
- `git diff --check`
  - PASS (Git emitted only LF-to-CRLF worktree notices).

## Non-Gating Diagnostic

`cargo clippy --lib --features test-utils -- -D warnings` remains red with 12
library-only/test-helper and existing style findings, including dead-code for
test-only helper APIs, the existing delegation `too_many_arguments`, existing
adapter `collapsible_match`, and parser `byte_char_slices`. The new mechanical
clippy findings from this wave (`question_mark`, `ptr_arg`, and
`needless_borrow`) were corrected. The requested focused acceptance tests are
green.

The repeated Rust commands also emitted the existing warning that the
`codeg-mcp` sidecar placeholder is missing; this does not affect library tests.
