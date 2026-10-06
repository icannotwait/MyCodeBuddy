# Roundtable lifecycle remediation, 2026-10-06

Base: `15313e62d3e76b8ad03932c7dd2766bf2e0deb75` (rebased from the original `a8ee4042ff64cb54d16acd5830b8afa23d39beb6` review base)
Scope: F09, F10, F11, F12, F17 from the approved review. No model processes, host certificates, deployment, push, or execution-policy enablement were used.

## Revalidated findings and changes

- **F09:** Recovery previously changed a paused room back to `running` solely to satisfy publication's fence check. Publication now admits a paused recovery only with its matching durable recovery operation, boot, run, phase, and frozen-set authorization. It never changes a nonterminal recovered room to running. The latest published projection remains paused/recovery-required; explicit resume remains available.
- **F10:** `accept` used a read-first deferred WAL transaction. Authoritative store/control/budget/publication/product mutations now acquire SQLite's writer reservation before reading their decision state, using a no-op write and the existing five-second busy timeout. Acceptance still rechecks fences and samples its decision deadline inside that transaction. Read-only queries remain ordinary read transactions. No model retry is introduced.
- **F11:** The runtime now retains local room/incarnation ownership independent of SQLite. All known incarnations are revoked/reaped concurrently before failure persistence, and a dropped room future revokes shared permission. A checkpoint has a timeout bounded by the last acknowledged prepaid deadline; a late acknowledgment cannot renew expired permission. The same permission is checked by executor enqueue, the real tool authority, and the real gateway (including its post-queue send check). Live physical cleanup proof is cached until mandatory launch/attempt cleanup facts commit. Failed or stalled persistence retains that record for retry. Optional diagnostic persistence is independent and cannot suppress mandatory cleanup. The service retries storage failures to fence and persist matching cleanup after storage recovers. If precise remote-work accounting could not be persisted, interrupted dispatched attempts remain conservatively marked as residual/uncertain.
- **F12:** Phase and room prepaid amounts are tracked separately. Phase expiry no longer exhausts room cleanup/publication time. The scheduler stops dispatch/retry at the phase deadline and does not interpret successor-phase intents as current-phase seats. Completed quorum is closed/published after unfinished turns are reaped, then synthesis may proceed.
- **F17:** Pause, Stop, and RestartCurrent preserve the prior lease when revoking the run epoch. They settle it only after cleanup is proved, before applying the control/successor. Cleanup duration is charged; known unused prepaid time is refunded once. Old-boot crash recovery remains conservative and never refunds an unknown remainder.

## Schema/backup compatibility

`rt_active_time_leases.phase_prepaid_ms` is an integer in `[0,1000]`, default zero. `prepaid_ms` remains the durable room reservation; the new column is the portion reserved against the current phase. Existing leases with a phase receive their previous `prepaid_ms` once during the transactional upgrade; phase-less leases receive zero. No room or phase balance is changed during migration.

The startup path in `db::open_connection` invokes `roundtable::migrate_roundtable` after the registered migrator, including already-migrated installations. `apply_roundtable_schema` checks table columns before alteration. Backup uses SQLite `VACUUM INTO`, preserving the complete table schema and rows; restored old databases receive the same startup upgrade. The logical model identifier and table list are unchanged.

## Regression coverage added

- Frozen proposal recovery asserts paused room state, paused latest projection, recovery-required reason, resume validation, zero active room occupancy, and no duplicate publication
- Five-connection SQLite fixture with two accepts released from a barrier while another transaction owns the WAL writer lock
- Real durable Pause/Stop/RestartCurrent request → blocked cleanup → confirmed cleanup → exact lease settlement
- Public pause command holds a controlled cleanup boundary; the lease remains present while cleanup is held and settles exactly after release
- Room time continues to fund cleanup after a 250 ms phase expires; exhausted phase time is never refunded from the room reservation
- Legacy prepaid schema migration twice preserves existing balances/reservations, followed by one exact settlement
- Owned runtime with an actual SQLite write-failing trigger affecting checkpoint and failure fencing; process cleanup occurs before removing the trigger, then durable state is reconciled
- Owned runtime with a held SQLite write lock; local cleanup and tool/gateway permission revoke occur before releasing the lock
- Owned scheduler with three members: two real scoped submissions accepted, third times out at phase deadline, quorum publishes, synthesis completes, and no expired-seat retry occurs
- Real `GateToolAuthority` admission rejects expired room permission and late renewal
- Real `LiveModelGateway::forward` rejects expired room permission before network admission

## Verification

- `git diff --check`: passed during implementation
- Python SQLite legacy migration applied twice and `VACUUM INTO` roundtrip: preserved phase and phase-less reservations without changing prepaid room amounts
- Python SQLite WAL interleaving using the production reservation SQL: baseline deferred transactions returned `SQLITE_BUSY` for both consumers; write-first transactions both completed after the writer was released
- Rust regression invocation attempted: `cargo test --no-default-features --features test-utils --test roundtable_runtime lifecycle -- --nocapture`
- Result at implementation time: exit 127, `cargo: command not found`; Rust tests and rustfmt could not execute (`cargo fmt` also exited 127). They have **not yet run**. This is not a passing Rust verification claim.

Required integration verification:

```sh
cd src-tauri
cargo fmt --all -- --check
cargo test --no-default-features --features test-utils --test roundtable_protocol_io lifecycle -- --nocapture
cargo test --no-default-features --features test-utils --test roundtable_protocol_io roundtable_acceptance::storage_fix_recovery_publishes_frozen_set_once_without_new_attempts -- --exact
cargo test --no-default-features --features test-utils --test roundtable_runtime lifecycle -- --nocapture
cargo test --no-default-features --features test-utils --lib lifecycle_live_gateway -- --nocapture
cargo test --no-default-features --features test-utils --test roundtable_protocol_io
cargo test --no-default-features --features test-utils --test roundtable_runtime
cargo test --no-default-features --features server,test-utils
```

No live qualification evidence is inferred from these controlled tests.


## Follow-up corrections

A further source review identified four defects in the initial change. They are addressed as follows:

1. **Checkpoint polling:** The scheduler and budget monitor are separate futures polled by the same outer `select!`. The monitor may wait for SQLite without suspending a scheduler that already owns the writer transaction. Checkpoint timeout and late-acknowledgment checks remain bounded by the acknowledged prepaid deadline.
2. **Durable cleanup completion:** The live reaper caches the physical cleanup proof before attempting persistence. Mandatory launch retirement and attempt cleanup/remote-uncertainty rows commit together. A timeout or write failure returns a storage error while retaining the cached proof and owner record for retry. No successful cleanup result is returned with those facts uncommitted. Optional diagnostics run independently after the mandatory transaction and cannot fail an otherwise successful cleanup. A bounded process-local cache retains committed proofs across cancellation between cleanup and the driver’s final candidate read; a fresh process still requires normal recovery proof.
3. **Shutdown:** After aborting tasks, shutdown snapshots and reaps all runtime-local owners and known quarantined owners before querying storage. It attempts all subsequently discovered owners before persisting any proofs. Active-time leases remain present until cleanup is proved and settled. A storage failure keeps coordinator ownership and permits a later reconciliation attempt. Terminal room status is preserved when settling its final lease.
4. **Control usage:** Projection sampling includes still-owned same-boot reservations even after control increments the run epoch. This avoids recording unused prepaid time as consumed and permanently inflating the usage view's maximum sample.

Additional focused regressions (authored; Rust execution remains blocked):

- `lifecycle_checkpoint_keeps_polling_scheduler_that_owns_writer`: production acceptance holds its writer reservation across the 250 ms checkpoint boundary, then releases before prepaid expiry; the full owned run must complete
- `lifecycle_diagnostic_write_failure_cannot_orphan_successful_cleanup`: a SQLite diagnostic-insert failure exercises the actual live reaper's controlled pre-exec cleanup path; mandatory retirement and simulated uncertainty must persist, execution completes, and the public resume command admits another room
- `lifecycle_live_cleanup_retains_proof_until_mandatory_rows_commit`: a real SQLite retirement-write failure is removed before retrying the same live owner; the retained proof must permit reconciliation without another process execution
- `lifecycle_shutdown_reaps_local_owners_before_failed_storage_write`: two controlled active owners are reaped while database writes fail; the lease remains until storage is restored and shutdown is retried
- Public Pause, Stop and RestartCurrent command tests now assert the usage response and final projection report 600 ms after a 1,000 ms reservation settles, in addition to the remaining balance
- Durable control tests also assert final projection and maximum persisted usage samples for all three control kinds

Additional verification performed:

- `git diff --check`: passed
- `python3` with in-memory SQLite reproduced the control-epoch usage defect: the run-filtered reservation expression followed by `MAX` reported 1,000 ms; the same-boot reservation expression followed by final settlement reported 600 ms
- Rust compilation, focused regressions, formatting and the full suite remain unrun because no Rust toolchain is installed. No dependencies, workflows or runtime certificates were changed to perform validation


## Upstream cleanup reconciliation

The lifecycle changes now preserve upstream `15313e6`, including its ACP request handling and auth/scratch retirement. The cached-physical-proof path calls `retire_incarnation` before committing mandatory cleanup rows; filesystem or database failures retain retryable ownership. Pre-exec retirement likewise keeps its known-not-spawned identity until file cleanup succeeds.

A concrete upstream retention race was corrected: `prune_run_dirs` previously deleted old run directories based only on modification time, protecting only the incarnation currently being retired. Another live participant, an untracked owner from another room, or a preparing attempt could therefore lose its files when more than eight directories existed.

Retention now considers only directories bearing a matching host-written retirement record and having no scratch tree. Records are atomically published after successful scratch/auth deletion. Active and unretired directories are preserved regardless of age, and eligibility is checked again immediately before deletion. A stale retirement record does not authorize deletion after scratch is recreated. The retention limit applies to proven-retired directories rather than all run directories.

Regression updates:

- `reap_deletes_grok_auth_and_limits_old_run_directories` explicitly retires each test owner before checking retention
- `retirement_pruning_preserves_other_unretired_attempts` preserves old active auth/scratch, an old preparing directory, and scratch recreated beside a stale retirement record while pruning excess retired directories
- Controlled live-cleanup fixtures now create a real fixture auth copy under the runtime scratch path and assert its directory is removed before successful cleanup, including the mandatory-database-write retry case

Verification: `git diff --check` passed; `git merge-base --is-ancestor 15313e6 HEAD` returned 0. Rust compilation, tests and formatting remain unrun because the toolchain is unavailable. No live adapter, provider, credential or qualification certificate was used.
