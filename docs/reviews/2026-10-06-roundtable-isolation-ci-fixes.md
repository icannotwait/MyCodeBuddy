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

## Scoped discovery provenance after 382fd2e

Runtime CI at `382fd2e` reports 120 passed, 2 failed, 1 ignored. Both helper
positives retain `slirp_proc_environment_denied_live`. Linux server CI reports
7760 passed, 3 failed, 1 ignored; its remaining failures are both-root cleanup,
prepared/no-helper cleanup and retained-descendant proof. The new zombie,
best-effort termination and bounded auth-copy regressions pass there. These
results establish a live unreadable host candidate beyond the zombie case;
they do not identify that process or its environment.

### Design and source basis

Each new helper lifecycle now writes a private, bounded, write-once birth
record containing its creator's process start tick, boot UUID, PID/time
namespace identity and whether the boot-clock comparison is usable. The
creator's birth predates all subsequently launched crun hooks and their
children. Linux initializes child birth time before publishing the new task,
and reports process start time in clock ticks. The comparison is strictly
older, so rounding into the same tick cannot exclude a possible helper.
[Linux fork implementation](https://raw.githubusercontent.com/torvalds/linux/master/kernel/fork.c),
[proc_pid_stat](https://man7.org/linux/man-pages/man5/proc_pid_stat.5.html)

A boot UUID alone is insufficient. Proc stat applies the reader's time
namespace offset; a negative offset can wrap a historical unsigned birth
timestamp. Age exclusion therefore requires a known matching time namespace
and an explicitly zero boottime offset. The offsets interface describes the
children namespace, which must equal the reader's current namespace.
Unavailable, malformed, nonzero or mismatched clock information disables age
exclusion. [Proc stat implementation](https://raw.githubusercontent.com/torvalds/linux/master/fs/proc/array.c),
[time-namespace arithmetic](https://raw.githubusercontent.com/torvalds/linux/master/include/linux/time_namespace.h),
[time_namespaces](https://man7.org/linux/man-pages/man7/time_namespaces.7.html)

Context is read for the calling thread. Numeric `/proc/<tid>` lookup exposes
the top-level proc entry table, including `timens_offsets`; the
`/proc/thread-self` task table lacks that entry. Before associating proc
numbers with pidfds, both `/proc/self` and `/proc/thread-self` must match the
calling process/thread IDs. [Proc lookup and entry tables](https://raw.githubusercontent.com/torvalds/linux/master/fs/proc/base.c)

### Publication, cleanup and recovery

- Initialization owns a fresh lifecycle lock and provisional `preparing`
  state, rejects existing artifacts, syncs the birth record and directory,
  and only then publishes `prepared`. The production attachment is returned
  afterward. Linux provenance capture is guarded so non-Linux document-only
  tests retain their existing behavior
- Cleanup validates bounded private evidence, required fields, duplicate
  rejection and the original context. Missing, malformed or mismatched
  evidence stays unproven and is never regenerated. Independently verified
  identities still receive bounded best-effort cleanup when record validation
  fails; current boot/PID/proc-number identity failures prevent unsafe use of
  numeric identities
- With a usable record, discovery pins a process before reading start time.
  Only an identity that remains alive across that read and started strictly
  before the persisted creator birth can be excluded. Same-tick/newer
  unreadable candidates remain unproven. No raw PID authorizes a signal
- The bound is not renewed after restart. Durable interrupted-cleanup
  quarantine, retained descendant pidfds, startup serialization and the final
  post-container sweep remain required
- Safe failure reasons now distinguish unreadable live processes inside a
  usable lifecycle scope from inability to establish any age exclusion.
  They contain no process environment data

### Regression and verification boundary

Tests cover strict/equal/newer ordering, zero/nonzero/unavailable clock
contexts, boot/PID/time drift, proc-number mismatch, write-once publication,
pre-existing artifacts, missing/duplicate/malformed evidence and unchanged
records on cleanup. A controlled subprocess test initializes the real record,
then recovers in a later process: an older injected-unreadable fake child
must be excluded only in a usable clock domain. A second child born after
initialization but before another restarter must remain in scope, detecting
any attempt to replace the persisted bound with the restarter's birth.
Unsupported clocks explicitly expect strict-scan quarantine instead.
Integration fixtures now initialize the real lifecycle before spawning their
fake helpers.

Local fake hook tests pass 3/3. Native aggregate remains 96/98, with the two
missing Rust-tool prerequisites. Cargo test attempts exit 127; local Rust
compilation, test execution and rustfmt remain UNRUN. The next existing CI
run must verify this patch. A live unreadable same-tick/newer process, an
unsupported clock domain, or legacy/incomplete lifecycle evidence can still
prevent cleanup proof. Such evidence requires separate host-level recovery;
resetting quarantine or synthesizing a fresh birth record is not supported.
No live qualification capability, native credential isolation, receipt/drain
measurement, request-envelope measurement or gate enablement is claimed.

## Proc mount PID-domain guard

Matching `/proc/self` and `/proc/thread-self` numeric IDs is necessary but
not sufficient. An ancestor-mounted procfs can coincidentally give this
caller the same IDs while mapping another numeric PID differently. That
could associate one process's proc ownership markers with another process's
pidfd. Linux status emits every namespace level in `NStgid` and `NSpid`,
including repeated equal numbers. [Linux v6.8 status implementation](https://raw.githubusercontent.com/torvalds/linux/v6.8/fs/proc/array.c)

Context capture now also reads at most 4097 bytes from
`/proc/thread-self/status`, rejects a body exceeding 4096 bytes, and requires
exactly one `NStgid` value matching `getpid` and one `NSpid` value matching
`gettid`. Missing, malformed or duplicate fields and multi-level vectors
are rejected before any helper scan or pidfd association. Both existing
symlink checks remain. This is a read-only guard; no namespace is created
or changed.

Pure parser regressions cover valid single-level status, distinct and equal
multi-level IDs, duplicate fields, wrong IDs, absent/empty fields, signs,
overflow, malformed decimal text and oversized input. A local read-only
probe confirms this host supplies bounded single-level matching fields;
that is OS-interface evidence, not execution of the Rust implementation.
Fake hook tests pass 3/3 and the native aggregate remains 96/98 with the same
two missing Rust prerequisites. Local Rust tests and rustfmt remain UNRUN;
existing CI must verify the final patch.

## Retained identity continuity after c1dc084

Linux server CI at `c1dc084` reports 7768 passed, 1 failed, 1 ignored. The
remaining failure is
`verified_descendant_remains_owned_after_term_clears_its_environment`, with
`slirp_proc_environment_denied_within_scope`. The runtime suite passes
122 tests with one live test ignored. The server error does not identify the
denied process, so attribution to the marker-clearing descendant remains
an inference pending the next CI run.

Code inspection establishes a separate concrete continuity defect: every
sweep reread proc ownership data even for identities already verified and
retained by pidfd. A subsequent denied environment read became a sticky
unknown-discovery failure despite the existing ownership proof.

Discovery now skips reclassification only when the original retained pidfd
positively reports live through a successful zero-readiness poll. An exited,
invalid or inconclusive handle cannot exempt a numeric PID. The same retained
fd remains responsible for signaling and proving exit. Unknown live processes
continue through the complete birth, environment and identity checks; their
read failures still quarantine cleanup. No initial root verification,
interrupted-cleanup quarantine or termination assertion is relaxed.

A deterministic regression allows the first sweep to verify a marked,
TERM-ignoring descendant, then arms an environment denial on subsequent
sweeps. Cleanup must still reach SIGKILL and durable successful proof. Its
shell uses only builtins after readiness, avoiding incidental newly spawned
processes in this controlled case. A second regression checks that an exited
retained fd under a reused numeric map key cannot exempt a live foreign child.
The existing unknown-denial and real marker-clearing regressions remain.

The controlled shell readiness and TERM-ignore behavior were checked locally;
fake hook tests pass 3/3. Native aggregate remains 96/98 with missing rustc and
Cargo prerequisites. Rust test commands exit 127; local Rust tests and rustfmt
remain UNRUN. Existing CI must verify the patch and whether it resolves the
reported failure; genuine unknown live unreadable processes remain unproven.

## Desktop-only denial attribution at 4850a5d

The Linux server job at `4850a5d` passes 7771 library tests, with one ignored,
plus four binary tests and Clippy. The Linux desktop job reports 8080 passed,
2 failed, 1 ignored. Its failures are the retained-identity diagnostic
regression and the original marker-clearing descendant test, both with
`slirp_proc_environment_denied_within_scope`. The contrast does not identify
the denied process and does not justify suppressing the error.

This batch adds test-only diagnostics, armed only by those two process
fixtures. A thread-local capture retains at most 16 denied-candidate records
and an omitted-record count. Each record includes the sweep, candidate PID,
fixed fixture-role comparison, actual retained-gate poll result/revents/errno,
actual candidate-exit poll observation, original environment-read errno and
whether the test injected that denial. Numeric PPID, start tick and creator
birth tick come from the stat buffer already read by discovery. Observed
parentage is diagnostic only: reparenting can make a fixture child appear to
have another parent.

No environment bytes, process names, commands or paths enter the diagnostic
records. Poll results are captured from the existing calls, with errno read
immediately; there are no diagnostic polls or additional proc reads. Pending
observations reset per candidate and per sweep, preventing reused fd numbers
from inheriting another candidate's data. The snapshot is printed only when
the existing cleanup result assertion fails, after the fixture's existing
child cleanup. No cleanup is rerun and no production proof/signal decision or
termination assertion changes.

A pure collector regression checks role classification, injected versus real
read errors, EINTR preservation, candidate-cache reset, the record cap and
thread-local release. Local Rust execution remains UNRUN (Cargo command exits
127), as does rustfmt. Fake hook tests pass 3/3; native aggregate remains
96/98, with the missing-rustc and missing-Cargo prerequisites. Existing CI
must provide the denied-candidate evidence before any further behavioral fix.

## Controlled component inventories after b1d5ee5

The diagnostic evidence identifies a violated test precondition. Linux server
CI reports 7771 passed, 1 failed, 1 ignored. Its failed retained-unreadability
fixture owns PIDs 16107–16109, but the denied candidate is PID 16111, observed
parent 2490. It is newer than the creator bound, unretained, live by the existing
pidfd poll, and has a real EACCES read rather than the injected denial. Linux
desktop reports 8082 passed, 1 failed, 1 ignored; its equivalent candidates
19267 and 19275 are outside fixture PIDs 19263–19265 and have observed parent
5453. The exact concurrent owners are not established. The three intended
workers in this fixture do not fork after readiness.

The production proof correctly remains unavailable for an unknown, live,
unreadable in-scope candidate. Expecting unconditional whole-host success from
these unit fixtures was invalid on a shared process space. This patch changes
the component-test input boundary, not that production outcome.

### Boundary and safeguards

- Only `cfg(test)` unit builds can install an explicit candidate inventory.
  Production and `test-utils` integration library builds retain the direct
  full `/proc` iterator. Without an installed unit inventory, the unit adapter
  also forwards the real directory entries and errors
- Inventories come from declared fake-child IDs, including intentionally
  unknown children in negative cases. They are never derived by filtering
  markers, unreadability or errors. Candidate metadata/stat/environ, namespace
  checks, pidfds, signals and lifecycle files remain real
- Inventory and denial guards are thread-local, non-Send and RAII-reset.
  A regression checks explicit membership, cross-thread isolation and unwind
  restoration. Controlled children have kill/wait-on-drop protection
- Marker-clearing fixtures retain the real TERM-to-exec/environment-clear
  transition. Their pre-TERM wait is builtin-only so each declared inventory
  is complete. The simple-helper case captures first-call pidfd exit and
  durable-state evidence before retry; reaping is delayed until afterward to
  prevent numeric ID reuse without letting retry repair the first assertion
- The mutation control has a fresh lifecycle and forces rediscovery only
  inside its unit inventory scope. It requires the exact injected descendant
  denial, a successful original read and a live retained poll, while still
  asserting SIGKILL and durable quarantine. The normal fresh fixture requires
  successful proof; neither case accepts either outcome

### Explicit test mapping

The following existing cases are reclassified and named as controlled
component tests; none is skipped or removed:

- `controlled_persisted_birth_excludes_only_an_older_unreadable_child_after_restart`
- `controlled_helper_cleanup_waits_for_both_pinned_helper_and_watcher`
- `controlled_retained_live_identity_survives_unreadable_environment_on_later_sweeps`
- `controlled_verified_descendant_remains_owned_after_term_clears_its_environment`
- `controlled_interrupted_cleanup_cannot_forget_a_descendant_on_retry`
- `controlled_no_helper_and_durable_never_started_are_distinct_from_missing_configuration`

Added cases are the inventory isolation/unwind regression,
`controlled_forced_rediscovery_is_a_fail_closed_mutation_control`, and
`controlled_inventory_unknown_live_candidate_still_quarantines_cleanup`.
The latter explicitly includes the denied foreign PID and requires quarantine,
foreign survival and owned-worker termination.

The original
`unreadable_live_process_quarantines_proof_but_does_not_prevent_owned_termination`
still uses unrestricted discovery and explicitly asserts no inventory override.
The forged/reused PID negatives and the real-process integration tests in
`tests/roundtable_cases/qualification_probe.rs` retain their unrestricted scope.

### What the result can establish

Controlled success establishes the cleanup algorithm's behavior, real kernel
identity/termination operations and durable state for the declared complete
fixture input. It does not establish whole-host discovery completeness, live
qualification or shared-host cleanup availability. Production still quarantines
unknown live unreadable candidates; replacing that availability limitation
would require a separately designed, demonstrably exclusive helper boundary.
No production EACCES exception, suite serialization, retry-to-green, test skip,
namespace/cgroup setting or live gate change is introduced.

Local checks: the builtin-only TERM fixture reaches readiness and its actual
exec transition, then remains alive until KILL; fake hook tests pass 3/3.
The native aggregate remains 96/98 with missing rustc/Cargo prerequisites.
Rust test attempts exit 127, so local Rust tests, the mutation control and
rustfmt remain UNRUN. Existing CI must compile and run this exact patch before
any new Rust GREEN or full-matrix claim.

## Run 236: retain the exact remaining cleanup failure branch

Observed at `1b71372`: the runtime target passed 121 tests, failed
`acp_session_error_stops_slirp_and_deletes_the_container`, and ignored the one
live probe. The failed assertion reported `session/new rejected; cleanup:
slirp_cleanup_unproven` and durable `cleanup-in-progress-started`. Both fake
container kill/delete assertions had already passed. The low-memory Cargo
configuration forces one test thread; the preceding test completed at
16:37:14.6084668 UTC and this test completed at 16:37:14.7142038 UTC (about
106 ms later), excluding the three-second cleanup-loop deadline for this run.
The child statuses were sampled before fixture rescue cleanup but were not
printed, so their exit state cannot be inferred from this failure log.

The reason does not identify the earlier, specifically labeled live environment
read denial. Possible remaining branches include pin validation, pinned-process
verification, pidfd operations, or limits. Linux
[`pidfd_open`/`pidfd_create` in v5.15](https://github.com/torvalds/linux/blob/v5.15/kernel/pid.c#L524-L573)
and [`pidfd_prepare` in v6.8](https://github.com/torvalds/linux/blob/v6.8/kernel/fork.c#L2053-L2057)
show that losing the thread-group task between PID lookup and pidfd preparation
can produce `EINVAL`. That is a source-supported race candidate, not an observed
attribution of this CI failure. `EINVAL` remains a rejection.

This diagnostic-only delta replaces the remaining generic Linux branches with
fixed operation labels. Failed pidfd opens distinguish durable-pin verification
from the host census; pinned-process metadata/stat/environment failures also
retain that caller distinction. Syscall failures append the immediately captured
numeric errno (or `unknown`), and filesystem failures use their returned
`io::Error` errno without a new OS read. No PID, environment, path, command, or raw
proc content is included. The existing failing assertion now also prints its
already-sampled child exit statuses, still only after rescue cleanup.

Success/failure predicates, ESRCH handling, read/poll/signal order, signal values,
limits, retry/deadline behavior, host inventory, and durable phase writes are
unchanged. This does not add an inventory override, ignore an error, skip a test,
or retry until green. No new local process/socket probe was attempted; Rust and
rustfmt remain unavailable locally. The next existing CI run must establish the
actual failing operation and errno before any behavioral correction is justified.
