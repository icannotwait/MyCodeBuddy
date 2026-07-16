//! Debug endpoint exposing `EventBusMetrics` for the operator-facing
//! `/api/debug/event_metrics` route.
//!
//! Behind the same auth layer as every other handler — operators tail it
//! with `curl -H "Authorization: Bearer $TOKEN" http://host:port/api/debug/event_metrics`.
//! Returns the current snapshot as JSON; pollers can compute deltas
//! between calls (no server-side rate computation, no histograms — keep
//! it dependency-free).

use std::sync::Arc;

use axum::{extract::Extension, Json};

use crate::acp::EventBusMetricsSnapshot;
use crate::app_error::AppCommandError;
use crate::app_state::AppState;
use crate::commands::acp::acp_get_event_metrics_core;

/// Snapshot the current ACP event bus metrics.
pub async fn get_event_metrics(
    Extension(state): Extension<Arc<AppState>>,
) -> Result<Json<EventBusMetricsSnapshot>, AppCommandError> {
    Ok(Json(acp_get_event_metrics_core(
        state.acp_event_bus.metrics().as_ref(),
    )))
}
