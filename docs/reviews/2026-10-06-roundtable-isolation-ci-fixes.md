# Isolation follow-up from executable CI failures

CI baseline: [`1606ca3e048d8e11ec34a2cebed847adf210a951`](https://github.com/icannotwait/MyCodeBuddy/commit/1606ca3e048d8e11ec34a2cebed847adf210a951).

## Observed evidence

At `1606ca3`, the runtime target compiled and ran: 114 passed, 4 failed,
1 live test ignored. The isolation failures were
`acp_session_error_stops_slirp_and_deletes_the_container` and
`probe_cleanup_cannot_leave_a_slirp_helper_that_ignores_term`.
The first fake helper exited naturally after its 30-second sleep; this was
not a kill(0) zombie-detection false positive.

The Linux server library suite also ran: 7752 passed, 6 failed, 1 ignored.
Five helper-cleanup tests failed, including the prepared/no-helper case.
Those additional failures establish that the two old integration fixtures
cannot by themselves explain the complete failure set. The old generic
`slirp_cleanup_unproven` reason did not identify the failed operation.

A local, owned fake process reproduced proc-stat disappearance: open its
`/proc/<pid>/stat`, kill/wait the child, then read the already-open file;
the read returned ESRCH. The equivalent environ read returned EOF on this
host. No environment values were printed or saved. This establishes the
legitimate exit-race class, not the exact errno from the earlier CI run.

## Changes

- Make the two positive integration fixtures reflect the real two-root
  slirp/watcher lifecycle. Both roots have their own marked process and
  PID/start-time pin. Fake cleanup commands succeed; the simulated ACP
  session still fails
- Assert both process termination and `cleanup-proven-started`. The
  TERM-ignoring helper must terminate through SIGKILL. Always kill/wait
  remaining fake children before asserting, so a failure cannot wait for a
  natural 30-second exit or leave test children behind
- Keep forged two-root pins as negative coverage; they cannot authorize
  signaling an unrelated process
- Treat only ENOENT/ESRCH as a vanished process during helper enumeration.
  Add a classifier regression that rejects EACCES, EPERM, EIO and EAGAIN as
  disappearance. Permission and live-unreadable failures remain quarantined
- Add safe operation/error-class reasons for private evidence, lifecycle
  state/lock and proc enumeration failures. They contain no environment
  data. Preserve the first helper-cleanup error instead of replacing it with
  the retry's interrupted-quarantine error; still perform the final check

## Verification boundary

The executable CI failures above are genuine RED evidence. Local Rust
compilation/tests/rustfmt remain unavailable because Cargo is absent.
The next existing CI run must verify the corrected fixtures and identify
any remaining early proof failure from its precise reason. This change
does not claim the broad helper cleanup failures are already GREEN.
No pidfd identity check, private-file rule, descendant retention, durable
quarantine, or certificate/execution gate was relaxed.

Available local checks: fake hook tests 3/3 pass; native release-script
aggregate 96/98 passes. Its two failures are the missing-rustc desktop
entrypoint prerequisite and the missing-Cargo build-policy prerequisite.
`git diff --check` passes. Rust test attempts exit 127; no local Rust GREEN
or rustfmt pass is claimed.

## Runtime proof diagnostics at 247106d

The next runtime suite ran with 115 passed, 3 failed, 1 ignored. The two
helper tests now fail at the durable-state assertion: the lifecycle remains
`cleanup-in-progress-started` instead of `cleanup-proven-started`. Their
termination assertions follow that state assertion, so the failure log does
not yet establish successful helper termination. The third failure is
`staged_candidate_then_actual_gateway_http_error_cannot_be_accepted`.

The Drop test fixture now captures its real synchronous cleanup result in a
test-only thread-local slot. It appends the safe cleanup reason to the
simulated ACP error, and both state assertions print that error. A missing
configured-helper lifecycle regression requires the first cleanup reason,
`slirp_evidence_file_unreadable`, to remain visible. This does not replace
the Drop path, repeat cleanup for diagnostics, or relax the durable proof.

Checks for this diagnostic delta: fake hook suite 3/3 pass; native aggregate
96/98 pass, with the same missing-rustc and missing-Cargo prerequisites.
The focused Rust test attempt exits 127 because Cargo is absent. The
underlying cleanup failure remains unresolved pending operation-specific
CI evidence; local Rust tests and rustfmt are UNRUN.

## Confirmed zombie-read failure and bounded best-effort cleanup

The Linux server job at `247106d` identifies
`slirp_proc_environment_denied` in the five cleanup failures, including the
prepared/no-helper case. A local owned-child reproduction then confirmed
that an unreaped exited process can return EACCES from `/proc/<pid>/environ`
while its pidfd already reports POLLIN. The reproduction recorded only
exit readiness and errno 13; it did not print or save environment values.

Cleanup now pins each eligible same-UID candidate before reading its
environment. A failed read can exclude an exited candidate only using that
same pidfd's exit readiness (or the existing ENOENT/ESRCH disappearance
classification). A live unreadable candidate retains a distinct failure
reason. No permission error is treated as evidence of unrelated ownership.

Discovery returns independently verified identities alongside its first
error and continues checking other candidates. Cleanup retains the error,
continues bounded TERM/KILL for verified roots and descendants, and leaves
the durable quarantine even when those known processes have exited. A
subsequent empty scan or retry cannot turn incomplete discovery into proof.
No birth-time metadata, startup-state shortcut, or host privilege change is
introduced.

Regressions cover denial for a pinned live child versus its unreaped exited
identity, plus a live unreadable foreign process during cleanup: verified
roots and descendants must terminate, the foreign process must survive,
and the incomplete proof must remain quarantined on retry. The existing
fatal-interruption observer still checks proof continuity when the owning
cleanup invocation stops unexpectedly.

Local fake hook tests pass 3/3. The focused Rust command still exits 127
(Cargo absent); actual Rust compilation, tests and formatting remain UNRUN.
The next existing CI run must establish whether zombie denial explains all
reported failures. A live unreadable process intentionally remains a
cleanup-proof limitation.

## Bounded attempt-local Grok auth copy

The earlier `is_file` caller checks do not cover a source replaced before
`copy_regular_nofollow` opens it. The source open now includes O_NONBLOCK and
O_CLOEXEC alongside O_NOFOLLOW, so a FIFO cannot block before the regular-file
check. The read is capped at `limit + 1` before allocating from source bytes,
so growth after the metadata size check cannot cause an unbounded read.
The existing 64 KiB cap and attempt-local copy behavior are unchanged.

Tests use only generated local files. One requires FIFO rejection without a
writer and rescues the former blocking behavior before joining the test
thread. Another grows the same regular inode after its metadata check,
then checks a duplicated input descriptor's shared offset: at a four-byte
limit the copy must consume exactly five bytes, fail, and publish no copy.
Local Rust execution remains UNRUN because Cargo is absent; no real auth
material or provider was used.
