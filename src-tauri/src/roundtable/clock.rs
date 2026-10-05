//! Process-local monotonic clock for accept decisions.
//!
//! Absolute values never cross a process. A fake clock can move forward and
//! can jump its UTC label backward; it cannot move the monotonic sample back.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use tokio::sync::Notify;

pub trait MonoClock: Send + Sync {
    fn now_ms(&self) -> u64;
    fn utc(&self) -> String;
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

pub struct SystemMono {
    start: Instant,
}

impl SystemMono {
    pub fn new() -> Self {
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
        u64::try_from(self.start.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    fn utc(&self) -> String {
        chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
    }
}

/// Test clock. `jump_on_call` moves the sample forward on that 1-based read.
pub struct FakeClock {
    now: Mutex<u64>,
    calls: Mutex<u64>,
    jump_at: Mutex<Option<(u64, u64)>>,
    utc: Mutex<String>,
}

impl FakeClock {
    pub fn new(start_ms: u64) -> Self {
        Self {
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
}
