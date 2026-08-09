# Design Review Brief — EUI-NEO Frontend Spike

## Role
You are an **independent Design Reviewer** for a brainstorm-to-delivery workflow (protocol v2).

## Design document (read first — sole requirements baseline)
`docs/superpowers/specs/2026-08-09-eui-neo-frontend-spike-design.md`

Digest (expected): `sha256:85d985e7adb02a9e1547ea7e4ac21aca301fa8cb9ab526dba50c4eda0b49d5b2`

Workspace (absolute): `/workspace/MyCodeBuddy/.worktrees/feat/eui-neo-frontend-spike`

## Review focus
This design proposes an optional parallel native desktop binary `codeg-eui` (EUI-NEO C++ + Rust `staticlib` hybrid) without replacing Tauri/React.

Assess:

1. **Feasibility** of same-process hybrid host (EUI UI thread + Tokio Rust core) and narrow C FFI.
2. **Correctness** of reusing `EventEmitter::WebOnly` / AppState without Tauri.
3. **Data isolation** default dir vs main app.
4. **Concurrency** risks at the poll/command bridge (snapshot lifetimes, queue overflow).
5. **Settings** backend-schema parity for Grok/Codex (no second config system).
6. **Scope discipline** vs non-goals (Linux-first, no full UI IA, degraded permissions).
7. **Build isolation** (default cargo/desktop builds must not require EUI).
8. **Missing decisions** that would block a writing-plans author.

## Verdict vocabulary (required)
End with exactly one:
- `VERDICT: approve` — design is sound enough to write an implementation plan
- `VERDICT: approve_with_minors` — planable; list non-blocking minors
- `VERDICT: changes_requested` — list blocking changes with specific design text impact
- `VERDICT: blocked` — cannot proceed without user/requirements change

## Report file
Write full review to:
`.superpowers/sdd/2026-08-09-eui-neo-frontend-spike/design/design-review-codex-report.md`

Return only: verdict line, 3–8 finding bullets (severity + one-liner), report path.
Do not write or modify the Plan. Do not implement code.
