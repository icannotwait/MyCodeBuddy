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
