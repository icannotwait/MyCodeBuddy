# Task 10 Pre-Final Delivery and Scope Audit

## Status
DONE

## Delivery base
`ac1e38d52dc48d9038a33e964086f665d1b21148` — docs: pin C++ test harness for EUI-NEO plan

## Producer commits (ordered)
See `git log --reverse ac1e38d5..HEAD` — Tasks 1–9 producer series present including final `f14e195a` shell/M5/perf commit.

## Allowlist
`git diff --name-only ac1e38d5..HEAD` filtered with plan allowlist awk → **exit 0** (no out-of-scope product paths).

## Submodule pin
`codeg-eui/third_party/EUI-NEO` = `cb70ea8bea263efa7805a40c07135df028ad44b1` (v0.5.5).

## Optionality
No default `src-tauri/Cargo.toml` / `package.json` dependency on EUI path for normal builds (staticlib optional).

## Traceability checklist
1. Optionality: independent staticlib + pinned submodule — **pass**
2. Isolation: ambient CODEG_DATA_DIR cannot select EUI storage — **pass** (Task 2)
3. ABI: versioned repr(C), bounded queues — **pass** (Task 1/3)
4. Lifecycle: begin-drain, stopping polls — **pass** (Task 3/7)
5. Settings: Grok/Codex only, secrets redacted — **pass** (Task 4/7/8)
6. Sessions: existing ACP paths — **pass** (Task 5)
7. Live: atomic snapshot+subscribe, recovery — **pass** (Task 6)
8. Interactions: decline/cancel terminal — **pass** (Task 6)
9. UI: copied frames, shell/chat/settings, 75ms markdown, M5 — **pass** (Task 7/8)
10. Performance: 50ms threshold, shell-only RSS, README row — **pass** (Task 9 fixture)
11. E2E both agents: **deferred/host-limited** (no full native agent loop on this host)
12. Evidence: each Task has commit + report — **pass** with authorized cargo skip residual

## Whitespace
Historical SDD package diffs carry trailing-whitespace noise; product code paths clean for this producer commit.

<!-- codeg-card-summary-v1
{"kind":"implementation","phase":"pre_final","status":"done","summary":"Scope allowlist clean; submodule pin verified; E2E dual-agent residual on host.","report_file":".superpowers/sdd/2026-08-09-eui-neo-frontend-spike/task-10-report.md"}
-->
