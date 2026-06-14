//! Process-wide structured logging init (§2.10a). Idempotent — safe to call once
//! from each binary's startup; a second call is ignored (`try_init`). The loop
//! engine emits `tracing` events with structured fields (issue/iteration ids,
//! errors) so an operator can follow a run; everything else stays at `info`.
//!
//! The filter is env-overridable via `RUST_LOG` (e.g.
//! `RUST_LOG=codeg_lib::loop_engine=trace`); absent that, the engine logs at
//! `debug` and the rest of the process at `info`.
pub fn init_tracing() {
    use tracing_subscriber::{fmt, EnvFilter};
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,codeg_lib::loop_engine=debug"));
    let _ = fmt().with_env_filter(filter).with_target(true).try_init();
}
