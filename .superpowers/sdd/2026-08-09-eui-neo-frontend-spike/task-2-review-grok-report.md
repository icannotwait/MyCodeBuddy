# Task 2 Independent Review (Grok) — Isolated Data Root & EUI AppState Profile

## Review Basis

| Item | Value |
| --- | --- |
| Role | Independent Task 2 high-risk Reviewer 2 (Grok) |
| Work unit | `task\|2\|reviewer\|grok\|none` |
| Workspace | `/workspace/MyCodeBuddy/.worktrees/feat/eui-neo-frontend-spike` |
| reviewed_task_id | `eb250a5f-e61e-441f-af46-f5130a615ed8` |
| Commit (HEAD) | `8bac8d78bcdf7f189304fa714d068e2d73ddb541` |
| BASE | `6fcfd6999d69d16d829b0410c1e828069aec0628` |
| Producer status | `DONE_WITH_CONCERNS` (host SIGKILL on full cargo) |
| Brief | `.superpowers/sdd/2026-08-09-eui-neo-frontend-spike/task-2-brief.md` |
| Report | `.superpowers/sdd/2026-08-09-eui-neo-frontend-spike/task-2-report.md` |
| Review package | `.superpowers/sdd/2026-08-09-eui-neo-frontend-spike/task-2-review-package.md` |
| Global constraints | `.superpowers/sdd/2026-08-09-eui-neo-frontend-spike/global-constraints.md` |
| Risk | `high` (`security_trust_boundary` + `concurrency_lifecycle`); policy `b2d_task_risk_v1` |

This review is independent of the implementer and of any Codex reviewer thread.
Findings are from commit contents, brief/spec cross-check, static source audit,
a pure resolver logic probe, and a re-attempt of the mandatory Cargo check on
this host.

## Overall Assessment

Task 2 pins an isolated EUI data root before logging/Tokio/DB, owns real
bootstrap state through the public ABI, constructs a `WebOnly` dormant
`AppState` profile that matches the brief field map, and covers isolation with
child-process tests (ambient main roots, authoritative ABI argument, same-root
restart, divergent-root rejection, bound/UTF-8/NUL rejection).

Hard-trigger surfaces (trust-boundary env mutation order; process-once pin;
lifecycle ownership and joined shutdown) check out under independent review.
No Critical or Important source defect that blocks Task 3 was found.

**Verdict: `approve_with_minors`**

Minors are residual host Cargo evidence debt and a soft dormancy-test seam
nit. Neither requires a code change before continuing the task sequence.

## Spec Compliance Matrix

| Requirement | Result | Notes |
| --- | --- | --- |
| Pure `EuiRootInputs` + `resolve_eui_data_root` precedence | Pass | `CODEG_EUI_DATA_DIR` → `$XDG_DATA_HOME/codeg-eui` → `~/.local/share/codeg-eui`; empty EUI dir falls through |
| Relative roots absolutized against captured startup cwd | Pass | `startup_working_directory` OnceLock + lexical join/normalize |
| Ambient `CODEG_DATA_DIR` / `CODEG_HOME` never choose the EUI root | Pass | Resolver ignores them; isolation child sets both ambient main roots |
| Non-empty ABI argument authoritative over `CODEG_EUI_DATA_DIR` | Pass | `parse_data_root_argument` + `start_with_data_root_argument` |
| Empty ABI argument consults env defaults | Pass | Product path uses `None` / empty |
| Process-once pin; same normalized root re-init OK; different → error | Pass | `OnceLock` pin + `AlreadyPinned`; ABI maps to `INVALID_STATE` |
| Pin order: remove `CODEG_HOME`, overwrite `CODEG_DATA_DIR` before logging/runtime/DB | Pass | `resolve_bootstrap_root` → `prepare_root` → `init_eui` → runtime → DB/`new_eui` |
| Embedded NUL rejected before pin/env commit | Pass | Pin + ABI byte checks before `set_var` |
| Path bound `CODEG_EUI_MAX_PATH_BYTES = 32768` | Pass | ABI rejects `len > 32768` |
| UTF-8 + null-pointer checks on non-empty ABI path | Pass | |
| `init_eui()` file prefix `codeg-eui`; process-idempotent for same-root restart | Pass | `OnceLock` + `Arc<WorkerGuard>` clone; no general `LogGuard` Clone |
| `AppState::new_eui` uses `EventEmitter::web_only` | Pass | |
| Complete brief field map; zero connections | Pass | Manual field audit of `Ok(Self { … })`; profile test asserts `list_connections().len() == 0` |
| No excluded service start functions from profile | Pass | Static audit: no `recover_and_start`, outbox spawn, sweeper, web server, listener, supervisor, pet start |
| `DocumentTranslationService::new_disabled`; `new_inert` test alias retained | Pass | |
| Bootstrap owns runtime; joined shutdown after stopping-ready poll | Pass | ABI `take` + `bootstrap.shutdown()`; Drop keeps `shutdown_background` fallback |
| Same-root ABI restart after full two-phase shutdown | Pass | Isolation child case |
| SQLite/logs only under EUI root | Pass | Child asserts `codeg.db` + `logs` under EUI/argument root; main roots absent |
| Product isolation (no root workspace member; default features off) | Pass | `codeg-eui-core` not in `src-tauri/Cargo.toml`; `default-features = false` |
| Design SHA-256 + EUI-NEO gitlink unchanged | Pass | `b3446ec3…d2bdef`; gitlink `cb70ea8bea263efa7805a40c07135df028ad44b1` |
| Commit stages only owned sources | Pass | 10 files / +741 −9; includes justified `abi.rs` + `abi_smoke.rs` beyond brief example list |

### Justified deviations from brief snippets (not defects)

1. **`abi.rs` / `abi_smoke.rs` staged** — Required to wire authoritative root parsing
   and real `EuiBootstrap` ownership into the public ABI and keep smoke green.
   Producer report documents this; no scope creep into later tasks’ features.
2. **Lexical normalize + empty XDG/HOME filtering** — Stronger than the minimal
   brief snippet; preserves precedence and rejects empty env strings cleanly.
3. **`StartedServices` extended flags** — Extra dormancy bits beyond the brief’s
   example set; defaults remain false and match excluded-service intent.
4. **Process-idempotent `init_eui`** — Required for legal same-root ABI restart
   without reinstalling the global tracing subscriber (Codex residual fixed in
   this package).

## Independent Verification (this host)

Host: Linux, `MemTotal` ≈ 4.0 GiB, no swap (same class as producer).
`HEAD == 8bac8d78…`; tree matches the review package.

| Check | Result |
| --- | --- |
| Commit message / file list vs package | Match (`feat(eui): add isolated core bootstrap profile`, 10 paths) |
| Design SHA-256 | `b3446ec31cc8b0457ed1ca3e7c6e8b3ec421eb4b997af6efae3d8975ecd2bdef` |
| EUI-NEO gitlink | `cb70ea8bea263efa7805a40c07135df028ad44b1` |
| Pure resolver precedence / absolutize / NUL-byte probe (`rustc -D warnings`) | **Pass** (`resolver_probe_ok`) |
| Static audit: bootstrap order pin → log → runtime → state | **Pass** |
| Static audit: `new_eui` body lacks excluded start calls | **Pass** |
| Product isolation (`codeg-eui-core` not product workspace member) | **Pass** |
| `cargo --config .cargo/low-memory.toml check --manifest-path src-tauri/codeg-eui-core/Cargo.toml` | **SIGKILL (signal 9)** while compiling shared `codeg` at ~588/590; no Rust diagnostic failure |

Full real-shared-core Cargo tests remain unpaid on this memory class, matching
the producer’s DONE_WITH_CONCERNS. Headless behavior is still reviewed from
source + isolation-test design + pure resolver evidence.

## High-Risk Focus

### Security / trust boundary

- Resolver never reads ambient `CODEG_DATA_DIR` or `CODEG_HOME` when choosing
  the EUI root; those names appear only when the pin clears/overwrites them.
- Pin rejects embedded NUL **before** `OnceLock` commit and env mutation, so a
  hostile path cannot poison a later valid pin via `set_var` panic.
- ABI argument path is bounds-checked (≤ 32,768), non-null when `len > 0`,
  UTF-8 validated, and embedded-NUL rejected before bootstrap.
- Logging uses `codeg_logs_root()`, which prefers `CODEG_HOME` then
  `CODEG_DATA_DIR`; pin removes home and sets data dir first, so file logs land
  under the EUI root (isolation tests assert this on disk).
- Failed init after `LIFECYCLE_STARTING` rolls lifecycle back to `STOPPED`
  without leaving a half-installed bootstrap slot; pin remains immutable once
  committed (correct for the process-once invariant).

### Concurrency / lifecycle

```
uninitialized → starting → running → stopping → stopped
```

- ABI stores `EuiBootstrap` only after successful start; shutdown `take`s it and
  joins the runtime via `shutdown()` **after** a stopping poll latched
  `shutdown_ready`.
- Explicit `shutdown` drops the `Runtime` (join) before `AppState` drops;
  `Drop` keeps `shutdown_background` only as fallback — correct lifecycle
  ownership for the UI-thread ABI contract.
- Same-root re-init reuses the process pin and process-wide EUI log guard;
  divergent root returns stable `CODEG_EUI_ERR_INVALID_STATE`.
- No `tokio::spawn` before env pin on the production start path (`block_on`
  after pin + log + runtime build). Test-only `start_for_test` still pins first.

### Dormant profile

- `new_eui` builds the full shared field map, including
  `build_delegation_stack` (brief-mandated), without calling excluded start
  functions.
- Observable dormancy signal: `delegation_socket_path` does not exist (profile
  test); connections list empty; emitter is `WebOnly`.
- Residual (design-required, not a Task 2 defect): `build_delegation_stack`
  calls `recovery_authorizations.start_maintenance()`, which can spawn a
  prune loop on the live EUI runtime. That helper is outside the explicit
  excluded-service list; it is not a listener/supervisor/outbox/web/pet path.
  Documented by the producer; acceptable residual for later tightening if the
  design ever expands the exclude list.

## Findings

### Critical

None.

### Important

None.

### Minor

1. **Residual mandatory Cargo verification on ≤4 GiB hosts**  
   Independent re-run of
   `cargo --config .cargo/low-memory.toml check --manifest-path src-tauri/codeg-eui-core/Cargo.toml`
   reached shared `codeg` and was SIGKILLed (signal 9) with no compile
   diagnostic. Producer’s four mandatory Cargo targets remain unverified on
   this host class. Re-run
   `data_root_isolation`, `bootstrap_profile`, crate check, and
   `src-tauri` `--no-default-features --lib` check on a machine with >4 GiB
   usable memory or usable swap before treating the Cargo-orchestrated M1 path
   as CI-green. **No source change required for Task 2 acceptance.**

2. **`StartedServices` is a synthetic dormancy seam**  
   `EuiBootstrap::new` always sets `started_services: StartedServices::default()`
   and never flips flags from real start/stop paths. Profile assertions on those
   booleans therefore cannot fail if a future edit accidentally starts a service
   without also updating the struct. Mitigations already present: source-level
   absence of excluded start calls, empty connections, and
   `!delegation_socket_path.exists()`. Optional follow-up: derive flags from
   real side effects or assert additional observable non-starts. Not blocking.

## Non-Findings / Notes

- **Recovery-authorization maintenance** starts via the required shared
  delegation-stack constructor. Not on the global excluded-service list; uses
  the EUI DB connection (isolated root). Treat as residual concern, not a brief
  violation.
- **Process-retained EUI log writer** cannot be finalized at ABI shutdown while
  same-root restart remains legal; abrupt process exit may leave a small
  unflushed tail. Acceptable for the restart invariant; matches producer note.
- **Brief staging list omitted `abi.rs` / `abi_smoke.rs`** — inclusion is
  necessary and correctly limited.
- **Lexical `..` normalization** does not resolve symlinks; authoritative
  absolute ABI/env roots are caller-controlled by design (trust the host
  integrator), not an ambient main-app root leak.
- Producer TDD narrative (embedded-NUL poison, double subscriber install) is
  consistent with the shipped guards; this reviewer did not re-run the full
  shared-core child-process suite because Cargo cannot finish compiling
  `codeg` here.

## Verdict Card

```text
VERDICT: approve_with_minors
critical: 0
important: 0
minor: 2
reviewed_commit: 8bac8d78bcdf7f189304fa714d068e2d73ddb541
reviewed_task_id: eb250a5f-e61e-441f-af46-f5130a615ed8
continue_sequence: yes
code_changes_required: no
residual: re-run four mandatory Cargo targets on higher-memory host; optional stronger dormancy observability beyond StartedServices defaults
```

<!-- codeg-card-summary-v1
{"kind":"review","verdict":"approve_with_minors","critical":0,"important":0,"minor":2,"summary":"Task 2 OK: isolated root pin, WebOnly dormant AppState, ABI lifecycle ownership; minors: host SIGKILL cargo evidence; synthetic StartedServices dormancy flags.","report_file":".superpowers/sdd/2026-08-09-eui-neo-frontend-spike/task-2-review-grok-report.md"}
-->
