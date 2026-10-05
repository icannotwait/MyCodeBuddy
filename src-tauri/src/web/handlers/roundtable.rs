//! Roundtable HTTP commands stay unavailable until the product gate is enabled.

use axum::http::StatusCode;
use axum::Json;
use serde_json::{json, Value};

pub async fn unavailable() -> (StatusCode, Json<Value>) {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(json!({ "code": "runtime_unavailable" })),
    )
}
