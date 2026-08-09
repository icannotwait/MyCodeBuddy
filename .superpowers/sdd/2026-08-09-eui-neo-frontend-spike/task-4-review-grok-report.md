# Task 4 Independent Review (Grok) — Grok/Codex Settings Facade and Async Bridge Ops

## Review Basis

| Item | Value |
| --- | --- |
| Role | Independent Task 4 high-risk Reviewer 2 (Grok) |
| Work unit | `task\|4\|reviewer\|grok\|none` |
| Workspace | `/workspace/MyCodeBuddy/.worktrees/feat/eui-neo-frontend-spike` |
| reviewed_task_id | `48d79f89-e4ef-4240-8092-f98bc9306cf2` |
| Commit (HEAD) | `89c0889f6faf8d3ad482c9e4e1a6a34df65d8cbf` |
| BASE | `66f7cff1ee5b02773f19f938482c3a112792ecb0` |
| Producer status | `DONE_WITH_CONCERNS` (full Cargo skipped by parent; dependency-complete `codeg` / `settings_contract` SIGKILL on 3.8 GiB host) |
| Brief | `.superpowers/sdd/2026-08-09-eui-neo-frontend-spike/task-4-brief.md` |
| Report | `.superpowers/sdd/2026-08-09-eui-neo-frontend-spike/task-4-report.md` |
| Review package | `.superpowers/sdd/2026-08-09-eui-neo-frontend-spike/task-4-review-package.md` |
| Global constraints | `.superpowers/sdd/2026-08-09-eui-neo-frontend-spike/global-constraints.md` |
| Risk | `high` (`security_trust_boundary` + `public_compatibility`; soft total 2); policy `b2d_task_risk_v1` |
| Parent rule | **SKIP all full cargo test** — package/source/CTest/ABI evidence only |

This review is independent of the implementer and of any Codex reviewer thread.
Findings are from commit contents, brief/spec cross-check, static source audit of
the facade/ACP delegation/async workers/ABI pre-accept path, and independent
host re-runs of contracts-only CTest plus C header syntax.

## Overall Assessment

Task 4’s production shape is largely right for M2:

- Narrow public `EuiAgentSettings` / request-only `EuiAgentSettingsPatch` /
  `EuiAgentProbe` over existing ACP cores (no direct filesystem writes, no new
  persistence schema, no Axum handlers).
- Wire vocabulary restricted to exact `"codex"` / `"grok"` before DB or native
  config access on the worker path.
- Cross-agent field exclusivity validated before the set path reads current
  settings.
- `set_agent_settings` rejects malformed outer JSON (`deny_unknown_fields`)
  **before** request acceptance; oversized payloads remain gated by
  `CODEG_EUI_MAX_SETTINGS_JSON_BYTES`.
- Get/set/probe work is spawned off the UI thread via injectable `CoreOps` and
  the Task 3 completion ledger; slow-probe unit coverage shows poll stays empty
  until the worker finishes, then exactly one terminal completion.
- `AppState` is shared with workers through `Arc<AppState>` (bootstrap change is
  justified and `Deref`-compatible with existing test field access).
- No new secret-bearing `tracing`/`log` in the facade or `AppCoreOps` paths.
  Completion result JSON intentionally carries settings payloads (including env /
  auth fields) for the settings UI — same trust surface as the existing web
  settings path, not a second schema.

Hard-trigger surfaces mostly check out under static review. However, the Task 4
integration contract that is supposed to prove pre-accept rejection, polled
completion, and Codex/Grok native-file round-trip **does not compile**:
`settings_contract.rs` calls `codeg_eui_shutdown` without importing it. That is
an Important package defect on the primary verification artifact, independent of
the host memory limitation that prevented Cargo execution.

**Verdict: `request_changes`**

## Spec Compliance Matrix

| Requirement | Result | Notes |
| --- | --- | --- |
| Public DTOs match brief camelCase / field set | Pass | Serialize-only settings; Deserialize + outer `deny_unknown_fields` patch; probe shape |
| Wire agents only `"codex"` / `"grok"` | Pass | `parse_supported_agent` + `ensure_supported` on typed entry |
| Unsupported agent before file/DB access | Pass (impl) | Worker parses wire before facade; facade guards before `acp_*` |
| No direct TOML/JSON file writes in facade | Pass | Only `acp_update_agent_*_and_refresh` / `acp_list_agents_core` / `acp_preflight_core` |
| No second config schema / no Axum | Pass | Delegates to existing ACP helpers; helpers stay `pub(crate)` |
| `set` validates agent-field exclusivity | Pass | `EuiAgentSettingsPatch::validate_for` before read/write |
| Project other-agent fields to `None` | Pass | `project_agent_settings` strips cross-agent native fields |
| Probe via `acp_preflight_core(agent, Some(true), db)` | Pass | Launchable + non-pass messages + installed version |
| Async off UI thread; one terminal completion | Pass | `CoreOps` + `execute_command` + ledger; slow-probe unit test |
| Pre-accept bounded JSON + outer deny_unknown | Pass | `copy_utf8` 2 MiB bound then `from_slice::<EuiAgentSettingsPatch>` |
| Settings/probe ABI documented async | Pass | Header comment on set path; get/probe already async enqueue |
| Isolated CODEX_HOME/GROK_HOME round-trip contract | **Fail (artifact)** | Written in `settings_contract.rs` but file does not compile (I1) |
| Unit facade tests (wire, unsupported, projection, patch) | Pass (source) | 4 pure unit tests; no full-ACP fixture round-trip in unit layer |
| Design SHA-256 + EUI-NEO gitlink unchanged | Pass | `b3446ec3…d2bdef`; gitlink `cb70ea8bea263efa7805a40c07135df028ad44b1` |
| Commit stages owned sources + report only | Pass | 8 paths; no `target/` / build archives / standalone `Cargo.lock` |
| `commands.rs` / `model.rs` expand | N/A justified | Task 3 already freezes ops, payload shape, completion ledger |

### Justified deviations from brief snippets (not defects)

1. **No live `SettingsFixture` async unit round-trip** — native round-trip is
   expressed as an isolated-process `settings_contract` instead of in-module
   fixture tests. That is a valid placement **once the contract compiles and
   runs**.
2. **`commands.rs` / `model.rs` untouched** — operation discriminants and
   `CommandPayload::AgentSettings` already exist from Task 3; rewiring only
   `runtime`/`abi` is correct.
3. **Producer RED evidence via reversible mutations on shape probes** — weaker
   than full-crate RED, but consistent with the low-memory host pattern used on
   prior tasks under the parent SKIP rule.

## Independent Verification (this host)

Host: Linux, `MemTotal` ≈ 3.8 GiB, no swap.
`HEAD == 89c0889f…`; package BASE/HEAD/stat match the review package.

| Check | Result |
| --- | --- |
| Commit message / ancestry vs package | Match (`feat(eui): expose Grok and Codex settings facade`; BASE `66f7cff1`) |
| Design SHA-256 | `b3446ec31cc8b0457ed1ca3e7c6e8b3ec421eb4b997af6efae3d8975ecd2bdef` |
| EUI-NEO gitlink | `cb70ea8bea263efa7805a40c07135df028ad44b1` |
| C11 header syntax `-Wall -Wextra -Wpedantic -Werror` (`-c`) | **Pass** |
| Contracts-only CTest (`build-contract-task4`) | **3/3 Pass** (harness, abi_layout, ui_snapshot) |
| Static audit: facade has no `std::fs` / direct writes | **Pass** |
| Static audit: ACP helper signatures match call sites | **Pass** |
| Static audit: pre-accept patch parse leaves `out_request_id` unchanged on fail | **Pass** (returns before `accept_and_write`) |
| Static audit: unsupported wire/typed agents fail before `acp_*` | **Pass** |
| Static audit: `settings_contract` imports | **Fail** — `codeg_eui_shutdown` used, not imported (I1) |
| Full / package Cargo tests | **Skipped** (parent rule); dependency-complete `settings_contract` not re-attempted |

Producer-claimed focused Rust green counts (facade/ABI/runtime shape probes)
were not re-executed here because they require linking shared `codeg` via Cargo
or temporary stubs; residual host memory risk is unchanged from Tasks 1–3.

## High-Risk Focus

### Security / trust boundary (config + auth writes)

- Settings **writes** only go through
  `acp_update_agent_env_and_refresh` and
  `acp_update_agent_config_and_refresh`. The facade does not open or write
  `CODEX_HOME` / `GROK_HOME` itself.
- Pre-accept path bounds and parses the settings JSON; unknown **outer** fields
  fail closed with `CODEG_EUI_ERR_INVALID_STATE` and do not allocate a
  `request_id`.
- Cross-agent patch fields are rejected after acceptance on the worker as an
  error completion (not pre-accept). That still avoids applying foreign fields
  and still runs after `validate_for`, before native writes.
- Nested structured types (`GrokStructuredConfig`,
  `CodexSandboxStructuredConfig`) do **not** use `deny_unknown_fields`; unknown
  nested keys are ignored by serde defaults. Outer-bound is what the brief
  freezes; nested looseness is noted as Minor.
- Errors from `AppCoreOps` use `EuiFacadeError` / `AcpError` `Display` text and
  do not format patch bodies or env maps. No new tracing of auth/env was added
  in Task 4 surfaces. Successful completion payloads **do** include env/auth
  content by design for settings readback.
- Partial apply remains possible if env refresh succeeds and config refresh
  fails (two sequential helper calls). That mirrors the existing multi-step ACP
  command surfaces rather than inventing a new transactional store.

### Public compatibility (shared facade + ABI)

- Public Rust DTO names/fields match the brief; `AgentType` serializes via
  existing wire strings (`"codex"` / `"grok"`).
- Nested `GrokSettings` remains snake_case field names (no `rename_all`), so
  completion JSON is `grokSettings.default_reasoning_effort` etc. The contract
  asserts that shape; it matches the stock backend type rather than inventing a
  second projection.
- C ABI symbols for get/set/probe were already declared in Task 3; Task 4 only
  wires implementations and adds the set-path comment. Layout/CTest regressions
  were not introduced (3/3 still pass).
- Expanding `codeg` with public `commands::eui_facade` is intentional for the
  standalone `codeg-eui-core` path dependency (`default-features = false`).

### Async / completion

- `CoreOps` is injected into `run_worker`; production uses `AppCoreOps` holding
  `Arc<AppState>`.
- Get/probe payloads are `CommandPayload::Utf8`; set is
  `CommandPayload::AgentSettings { agent, json }` with an op guard.
- Worker success/error/panic still terminalize once through the existing ledger
  (unchanged Task 3 machinery).
- Slow-probe test proves an empty completion frame while the op is gated, then
  a single OK completion after `notify`.

## Findings

### Critical

None.

### Important

#### I1. `settings_contract.rs` does not compile: missing `codeg_eui_shutdown` import

`complete_shutdown` calls `codeg_eui_shutdown()`:

```242:247:src-tauri/codeg-eui-core/tests/settings_contract.rs
fn complete_shutdown() {
    assert_eq!(codeg_eui_begin_shutdown(), CODEG_EUI_OK);
    for _ in 0..200 {
        if poll().shutdown_ready == 1 {
            assert_eq!(codeg_eui_shutdown(), CODEG_EUI_OK);
            return;
```

but the import list omits it:

```5:9:src-tauri/codeg-eui-core/tests/settings_contract.rs
use codeg_eui_core::{
    codeg_eui_begin_shutdown, codeg_eui_get_agent_settings, codeg_eui_init, codeg_eui_poll,
    codeg_eui_set_agent_settings, CodegEuiCompletion, CodegEuiFrame, CodegEuiSlice,
    CODEG_EUI_ERR_INVALID_STATE, CODEG_EUI_OK,
};
```

Sibling contracts (`bridge_contract`, `shutdown_contract`, `abi_smoke`,
`data_root_isolation`) all import `codeg_eui_shutdown` explicitly. Under Rust
2018+ path hygiene this is an unresolved name for every case that reaches
shutdown (all three tests).

Why this is Important for Task 4:

- The brief’s primary automated proof of pre-accept malformed rejection, polled
  get completion, and Codex/Grok native-file round-trip **is this file**.
- Producer correctly reports that dependency-complete Cargo could not run on the
  3.8 GiB host, but the artifact would still fail to compile on a higher-memory
  host until the import is fixed.
- This is not a host residual; it is a source defect in the review package.

Required change: add `codeg_eui_shutdown` to the `use codeg_eui_core::{…}` list
(or equivalent), then re-run focused
`cargo test --manifest-path src-tauri/codeg-eui-core/Cargo.toml --test settings_contract -- --test-threads=1`
on a host that can link `codeg`. Optionally add a probe/assert that an
unsupported agent errors without requiring a native home (if still missing from
the contract suite).

### Minor

1. **Residual dependency-complete Cargo verification on ≤4 GiB hosts**  
   Parent rule forbids full Cargo. Producer already reports shared-core `rustc`
   SIGKILL under low-memory config. Independent review therefore cannot treat
   `settings_contract` or focused crate tests as host-green even after I1 is
   fixed. Re-run on a higher-memory host before CI claims. **Not a substitute
   for fixing I1.**

2. **“Rejected before filesystem path is touched” is only a unit guard test**  
   `unsupported_typed_agent_is_rejected_by_the_pre_access_guard` exercises
   `ensure_supported` in isolation. Placement of the guard in get/set/probe is
   correct in source, but there is no test double proving zero ACP/DB/fs calls.
   Optional hardening after I1.

3. **Nested structured patch objects are not `deny_unknown_fields`**  
   Outer patch rejects unknown keys; nested `grokStructured` /
   `codexSandbox` objects silently ignore extras. Matches existing ACP nested
   types; slightly weaker public fail-closed story for typos. Optional.

4. **No ABI-level probe contract in `settings_contract`**  
   Probe async non-blocking is covered by the runtime unit test with
   `SlowProbeOps`. An end-to-end `codeg_eui_probe_agent` poll completion case
   would strengthen M2 evidence but is not required to fix I1.

## Non-Findings / Notes

- `config_json` on Grok/Codex set is validated as a JSON object by
  `acp_update_agent_config_core` but is not the native write path for those
  agents (native TOML/auth/catalog/sandbox/structured fields are). That is
  pre-existing ACP semantics, not a Task 4 invent.
- Worker re-parses the already-accepted JSON; with the same type and owned
  bytes this is redundant but safe. ABI reject remains the acceptance gate.
- Header async-completion comment sits above `set_agent_settings` only; get and
  probe are already documented as async enqueue ops in the design/Task 3.
- Generated Cargo/CMake outputs remain unstaged; package file list matches
  `git show --stat` on `89c0889f`.

## Verdict Card

```text
VERDICT: request_changes
critical: 0
important: 1
minor: 4
reviewed_commit: 89c0889f6faf8d3ad482c9e4e1a6a34df65d8cbf
reviewed_task_id: 48d79f89-e4ef-4240-8092-f98bc9306cf2
continue_sequence: no
code_changes_required: yes
residual: fix settings_contract codeg_eui_shutdown import and re-run settings_contract on higher-memory host; optional unsupported-agent zero-touch proof; optional nested deny_unknown_fields; optional probe ABI contract
```

<!-- codeg-card-summary-v1
{"kind":"review","phase":"review","status":"request_changes","verdict":"request_changes","critical":0,"important":1,"minor":4,"summary":"Task 4 facade/async CoreOps/pre-accept deny_unknown largely match the brief, but settings_contract does not compile (missing codeg_eui_shutdown import), so the primary M2 integration proof is broken.","reviewed_task_id":"48d79f89-e4ef-4240-8092-f98bc9306cf2","artifact_digest":"89c0889f6faf8d3ad482c9e4e1a6a34df65d8cbf","concerns":["Add codeg_eui_shutdown to settings_contract imports and re-run the focused settings_contract test on a host that can link codeg.","Dependency-complete Cargo/settings_contract still residual on 3.8GiB hosts after the import fix.","Optional: stronger unsupported-agent zero FS/DB proof; nested deny_unknown_fields; probe ABI contract."],"report_file":".superpowers/sdd/2026-08-09-eui-neo-frontend-spike/task-4-review-grok-report.md"}
-->
