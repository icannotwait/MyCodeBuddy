//! Process-local monotonic clock for accept decisions.
//!
//! Absolute values never cross a process. A fake clock can move forward and
//! can jump its UTC label backward; it cannot move the monotonic sample back.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use tokio::sync::Notify;

pub trait MonoClock: Send + Sync {
    /// Active time: never advances while the host is suspended, so a
    /// suspended interval is neither charged to a room nor able to expire its
    /// prepaid slice (in this clock, the deadline moves out by the suspend).
    fn now_ms(&self) -> u64;
    fn utc(&self) -> String;
    /// Cumulative host suspend this clock has excluded since it started
    /// (CLOCK_BOOTTIME growth over CLOCK_MONOTONIC on Linux). Diagnostic only.
    fn suspended_ms(&self) -> u64 {
        0
    }
}

/// P14 name for the same monotonic sample. Absolute values stay in-process.
pub trait MonotonicClock: Send + Sync {
    fn now_ms(&self) -> roundtable_protocol::MonoMs;
}

impl<T: MonoClock + ?Sized> MonotonicClock for T {
    fn now_ms(&self) -> roundtable_protocol::MonoMs {
        roundtable_protocol::MonoMs(MonoClock::now_ms(self))
    }
}

/// Process clock pair. On Linux, active time is read from CLOCK_MONOTONIC
/// explicitly (it excludes suspend by definition; std's `Instant` does not
/// promise that) and suspend is measured as CLOCK_BOOTTIME minus
/// CLOCK_MONOTONIC. Elsewhere `Instant` is used and suspend is not measured.
pub struct SystemMono {
    start: Instant,
    #[cfg(target_os = "linux")]
    start_mono_ms: u64,
    #[cfg(target_os = "linux")]
    start_suspended_ms: u64,
}

#[cfg(target_os = "linux")]
fn clock_ms(clock: libc::clockid_t) -> u64 {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `ts` is a valid, writable timespec; both clock ids exist on
    // every supported Linux kernel.
    let rc = unsafe { libc::clock_gettime(clock, &mut ts) };
    if rc != 0 {
        return 0;
    }
    (ts.tv_sec as u64)
        .saturating_mul(1_000)
        .saturating_add(ts.tv_nsec as u64 / 1_000_000)
}

/// Pure suspend arithmetic over one clock-pair reading.
pub fn suspended_since(start_gap_ms: u64, boottime_ms: u64, monotonic_ms: u64) -> u64 {
    boottime_ms
        .saturating_sub(monotonic_ms)
        .saturating_sub(start_gap_ms)
}

impl SystemMono {
    pub fn new() -> Self {
        #[cfg(target_os = "linux")]
        {
            let mono = clock_ms(libc::CLOCK_MONOTONIC);
            let boot = clock_ms(libc::CLOCK_BOOTTIME);
            Self {
                start: Instant::now(),
                start_mono_ms: mono,
                start_suspended_ms: boot.saturating_sub(mono),
            }
        }
        #[cfg(not(target_os = "linux"))]
        Self {
            start: Instant::now(),
        }
    }
}

impl Default for SystemMono {
    fn default() -> Self {
        Self::new()
    }
}

impl MonoClock for SystemMono {
    fn now_ms(&self) -> u64 {
        #[cfg(target_os = "linux")]
        {
            let _ = self.start;
            clock_ms(libc::CLOCK_MONOTONIC).saturating_sub(self.start_mono_ms)
        }
        #[cfg(not(target_os = "linux"))]
        {
            u64::try_from(self.start.elapsed().as_millis()).unwrap_or(u64::MAX)
        }
    }

    fn utc(&self) -> String {
        chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
    }

    fn suspended_ms(&self) -> u64 {
        #[cfg(target_os = "linux")]
        {
            suspended_since(
                self.start_suspended_ms,
                clock_ms(libc::CLOCK_BOOTTIME),
                clock_ms(libc::CLOCK_MONOTONIC),
            )
        }
        #[cfg(not(target_os = "linux"))]
        {
            0
        }
    }
}

/// Test clock. `jump_on_call` moves the sample forward on that 1-based read.
pub struct FakeClock {
    suspended: Mutex<u64>,
    now: Mutex<u64>,
    calls: Mutex<u64>,
    jump_at: Mutex<Option<(u64, u64)>>,
    utc: Mutex<String>,
}

impl FakeClock {
    pub fn new(start_ms: u64) -> Self {
        Self {
            suspended: Mutex::new(0),
            now: Mutex::new(start_ms),
            calls: Mutex::new(0),
            jump_at: Mutex::new(None),
            utc: Mutex::new("2026-10-05T00:00:00Z".to_string()),
        }
    }

    pub fn set(&self, next_ms: u64) -> Result<(), &'static str> {
        let mut now = self.now.lock().expect("clock");
        if next_ms < *now {
            return Err("clock_rewind");
        }
        *now = next_ms;
        Ok(())
    }

    /// Simulate a host suspend: the boot clock moves on, active time does not.
    pub fn suspend(&self, ms: u64) {
        let mut suspended = self.suspended.lock().expect("suspended");
        *suspended = suspended.saturating_add(ms);
    }

    pub fn set_utc(&self, value: impl Into<String>) {
        *self.utc.lock().expect("utc") = value.into();
    }

    pub fn jump_on_call(&self, call: u64, to_ms: u64) {
        *self.jump_at.lock().expect("jump") = Some((call, to_ms));
    }
}

impl MonoClock for FakeClock {
    fn now_ms(&self) -> u64 {
        let mut calls = self.calls.lock().expect("calls");
        *calls += 1;
        let call = *calls;
        drop(calls);
        if let Some((at, to_ms)) = *self.jump_at.lock().expect("jump") {
            if call == at {
                let mut now = self.now.lock().expect("clock");
                if to_ms >= *now {
                    *now = to_ms;
                }
                return *now;
            }
        }
        *self.now.lock().expect("clock")
    }

    fn utc(&self) -> String {
        self.utc.lock().expect("utc").clone()
    }

    fn suspended_ms(&self) -> u64 {
        *self.suspended.lock().expect("suspended")
    }
}

/// Equality with the deadline is already a timeout.
pub fn deadline_reached(now_ms: u64, deadline_mono: u64) -> bool {
    now_ms >= deadline_mono
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AcceptStep {
    Message,
    Claims,
    Responses,
    PositionChanges,
    Evidence,
    Budget,
    Revision,
    Projection,
    Event,
    Commit,
}

/// Async gate the accept path waits on before it takes the write transaction.
pub struct LockGate {
    waiting: AtomicBool,
    notify: Notify,
}

impl LockGate {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            waiting: AtomicBool::new(false),
            notify: Notify::new(),
        })
    }

    pub fn is_waiting(&self) -> bool {
        self.waiting.load(Ordering::SeqCst)
    }

    pub async fn wait(&self) {
        let notified = self.notify.notified();
        self.waiting.store(true, Ordering::SeqCst);
        notified.await;
    }

    pub fn release(&self) {
        self.notify.notify_one();
    }
}

#[derive(Clone, Default)]
pub struct AcceptFaults {
    pub fail_step: Option<AcceptStep>,
    pub lock_gate: Option<Arc<LockGate>>,
    #[cfg(any(test, feature = "test-utils"))]
    pub writer_gate: Option<Arc<LockGate>>,
    #[cfg(any(test, feature = "test-utils"))]
    pub completed_run_gate: Option<Arc<LockGate>>,
    #[cfg(any(test, feature = "test-utils"))]
    pub completion_observation_gate: Option<Arc<LockGate>>,
}

#[cfg(test)]
mod suspend_tests {
    use super::*;

    #[test]
    fn suspend_is_measured_as_boottime_growth_over_monotonic() {
        // Started with a 5s pre-existing gap; a 240s suspend later.
        assert_eq!(
            suspended_since(5_000, 1_000_000 + 245_000, 1_000_000),
            240_000
        );
        // Clocks moving together (a hypervisor pause freezing both) is not a suspend.
        assert_eq!(suspended_since(5_000, 2_005_000, 2_000_000), 0);
    }

    #[test]
    fn a_simulated_suspend_does_not_advance_active_time() {
        let clock = FakeClock::new(1_000);
        clock.suspend(300_000);
        assert_eq!(MonoClock::now_ms(&clock), 1_000);
        assert_eq!(clock.suspended_ms(), 300_000);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn system_clock_pair_reads_real_clocks() {
        let clock = SystemMono::new();
        let a = MonoClock::now_ms(&clock);
        std::thread::sleep(std::time::Duration::from_millis(20));
        assert!(MonoClock::now_ms(&clock) >= a + 15);
        assert!(clock.suspended_ms() < 1_000);
    }
}
