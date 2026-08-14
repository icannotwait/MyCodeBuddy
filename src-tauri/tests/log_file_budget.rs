//! End-to-end guard on the daily byte ceiling for the on-disk log.
//!
//! `logging::budget`'s unit tests cover the accounting; this covers the *wiring*,
//! which is where the bug actually lived: `Rotation::DAILY` + `max_log_files`
//! bounds days, not bytes, so nothing stopped one day's file from reaching the
//! 34.36 GB a 0.23.3 field report measured (issue #427). A refactor that unhooks
//! `BudgetedWriter` from `init_file_writer`, or an appender swap that bypasses
//! it, would leave every unit test green — so assert on the real file: build the
//! real subscriber, emit far more than the ceiling, and read the size back off
//! disk.
//!
//! It also covers the **same-day restart**, which is the subtler half of the
//! same bug: the appender re-opens an existing dated file with `append(true)`
//! (`tracing-appender-0.2.5/src/rolling.rs:791`), so a budget that started
//! counting at zero handed out a whole new ceiling on every launch — and this
//! app restarts itself (the `--supervise` worker, auto update), so a crash loop
//! could still grow one file without bound. The test seeds today's file first and
//! asserts the *total* stays under the ceiling. That doubles as a check on
//! `budget::resume_point`'s filename reconstruction: if it guessed the name
//! wrong it would measure 0, and the file would end up at seed + ceiling.
//!
//! One `#[test]` per file on purpose: installing a subscriber is a process-global
//! one-shot, and the env vars this sets (`CODEG_HOME`, `CODEG_LOG_MAX_BYTES`) are
//! read during that install.

use std::io::Write;

/// Small enough to blow through quickly, large enough to hold several lines
/// first (so the test proves "capped", not "wrote nothing").
const CEILING: u64 = 32 * 1024;

/// Bytes pre-planted in today's file, standing in for what a previous run of the
/// same day left behind. Three quarters of the ceiling, so there is still room to
/// write — the test needs to see logging happen *and* stop.
const SEEDED: u64 = CEILING / 4 * 3;

#[test]
fn file_log_stops_growing_at_the_daily_ceiling() {
    let home = tempfile::tempdir().expect("tempdir");
    // Both are read inside `init_desktop()` below: CODEG_HOME relocates
    // `codeg_logs_root()`, CODEG_LOG_MAX_BYTES sets the ceiling. Set them before
    // the install rather than via `temp_env`, because the guard returned by the
    // install must outlive the closure.
    std::env::set_var("CODEG_HOME", home.path());
    std::env::set_var("CODEG_LOG_MAX_BYTES", CEILING.to_string());
    // A stray RUST_LOG/CODEG_LOG in the developer's shell would otherwise pick
    // the level (and could filter our test events out entirely).
    std::env::remove_var("RUST_LOG");
    std::env::remove_var("CODEG_LOG");

    let logs_dir = codeg_lib::paths::codeg_logs_root();
    assert!(
        logs_dir.starts_with(home.path()),
        "the test must not write to the developer's real log dir: {}",
        logs_dir.display()
    );

    // Stand in for "a previous run today already wrote this much". The name is
    // the one `Rotation::DAILY` uses (`{prefix}.{%Y-%m-%d}.{suffix}`, UTC) — the
    // appender will append to this very file, and `resume_point` has to find it.
    std::fs::create_dir_all(&logs_dir).expect("create logs dir");
    let today_file = logs_dir.join(format!(
        "codeg.{}.log",
        chrono::Utc::now().format("%Y-%m-%d")
    ));
    std::fs::write(&today_file, vec![b'-'; SEEDED as usize]).expect("seed today's file");

    // Real subscriber: EnvFilter + stderr + buffer + the budgeted rolling file.
    let guard = codeg_lib::logging::init::init_desktop();

    // ~1.2 KB of JSON per line × 300 lines ≈ 360 KB offered against a 32 KB
    // ceiling — the same shape as the reported storm, just compressed. Few, fat
    // lines rather than many thin ones: the real subscriber also echoes every
    // event to stderr, and that lands in the test output.
    let payload = "x".repeat(1160);
    for i in 0..300 {
        tracing::info!("budget-e2e line {i} {payload}");
    }

    // Flush the non-blocking worker: dropping the guard shuts it down and drains
    // the queue, so every line above has been offered to the budgeted writer by
    // the time this returns.
    drop(guard);
    std::io::stderr().flush().ok();

    let mut total = 0u64;
    let mut files = Vec::new();
    for entry in std::fs::read_dir(&logs_dir).expect("logs dir exists") {
        let entry = entry.expect("dir entry");
        let len = entry.metadata().expect("metadata").len();
        files.push((entry.file_name(), len));
        total += len;
    }

    assert!(
        !files.is_empty(),
        "the subscriber should have created a log file in {}",
        logs_dir.display()
    );
    assert!(
        total > SEEDED,
        "the ceiling must cap the file, not disable logging — nothing was \
         appended past the {SEEDED} seeded bytes: {files:?}"
    );
    // The ceiling is per calendar day == per rotated file, and the run is far
    // shorter than a day, so the total is one day's budget. The writer refuses a
    // line that *would* cross the ceiling, so the file never exceeds it — and the
    // seeded bytes count toward it, so a restart can't win a second ceiling.
    assert!(
        total <= CEILING,
        "one day's log grew past the {CEILING}-byte ceiling ({SEEDED} of it \
         seeded before init): {total} bytes in {files:?}"
    );
}
