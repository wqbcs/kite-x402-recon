//! Read-only HTTP API. `serve` is for dashboards and cron probes; it never
//! mutates anything and runs the same reconciler as the CLI.

use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde_json::json;

use crate::ledger::LedgerSource;
use crate::model::now_unix;
use crate::reconcile::Reconciler;

#[derive(Clone)]
pub struct AppState {
    pub reconciler: Arc<Reconciler>,
    pub ledger: Arc<dyn LedgerSource + Send + Sync>,
    pub token: Option<String>,
    pub mode: String,
    pub refresh_seconds: u64,
}

impl std::fmt::Debug for AppState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppState")
            .field("reconciler", &self.reconciler)
            .field("ledger", &self.ledger.describe())
            .field("token", &self.token.as_ref().map(|_| "<redacted>"))
            .field("mode", &self.mode)
            .field("refresh_seconds", &self.refresh_seconds)
            .finish()
    }
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/api/v1/reconcile", get(reconcile))
        .with_state(state)
}

async fn healthz(State(state): State<AppState>) -> Response {
    let body = json!({
        "status": "ok",
        "service": "kite-x402-recon",
        "version": env!("CARGO_PKG_VERSION"),
        "mode": state.mode,
        "chain_id": state.reconciler.chain_id(),
        "auth": if state.token.is_some() { "bearer" } else { "disabled" },
    });
    (StatusCode::OK, Json(body)).into_response()
}

async fn reconcile(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    if let Some(response) = authorize(&state, &headers) {
        return response;
    }
    // A shared reconciler serves the live tip; an anchored read clones it so
    // one request can ask about a past window without affecting the others.
    let mut reconciler = (*state.reconciler).clone();
    if let Some(raw) = params.get("at_block") {
        let block = match raw.parse::<u64>() {
            Ok(block) if block > 0 => block,
            _ => {
                return error_response(
                    StatusCode::BAD_REQUEST,
                    "at_block must be a positive integer",
                )
            }
        };
        let tip = match reconciler.current_tip().await {
            Ok(tip) => tip,
            Err(e) => return error_response(StatusCode::BAD_GATEWAY, &e.to_string()),
        };
        if block > tip {
            return error_response(
                StatusCode::BAD_REQUEST,
                &format!("at_block {block} is ahead of the chain tip {tip}"),
            );
        }
        reconciler = reconciler.with_anchor_tip(block);
    }
    let now = now_unix();
    match reconciler.report(&*state.ledger, now).await {
        Ok(report) => {
            let body = crate::report::to_json(&report).unwrap_or_else(|e| {
                json!({ "error": format!("failed to serialise report: {e}") }).to_string()
            });
            let mut response = (StatusCode::OK, body).into_response();
            response.headers_mut().insert(
                header::CONTENT_TYPE,
                header::HeaderValue::from_static("application/json"),
            );
            let cache =
                header::HeaderValue::from_str(&format!("max-age={}", state.refresh_seconds))
                    .unwrap_or_else(|_| header::HeaderValue::from_static("no-store"));
            response.headers_mut().insert(header::CACHE_CONTROL, cache);
            response
        }
        Err(e) => error_response(StatusCode::BAD_GATEWAY, &e.to_string()),
    }
}

fn error_response(status: StatusCode, message: &str) -> Response {
    (status, Json(json!({ "error": message }))).into_response()
}

/// Bearer-token check. Timing differences are acceptable here because the
/// token is compared after parsing, and both sides are fixed length.
/// Returns `Some(response)` when the request must be rejected.
fn authorize(state: &AppState, headers: &HeaderMap) -> Option<Response> {
    let expected = state.token.as_deref()?;
    let presented = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .map(str::trim);
    match presented {
        Some(token) if constant_time_eq(token.as_bytes(), expected.as_bytes()) => None,
        _ => Some(error_response(
            StatusCode::UNAUTHORIZED,
            "missing or invalid bearer token",
        )),
    }
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b) {
        diff |= x ^ y;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constant_time_eq_matches_only_equal_slices() {
        assert!(constant_time_eq(b"secret", b"secret"));
        assert!(!constant_time_eq(b"secret", b"secreT"));
        assert!(!constant_time_eq(b"secret", b"secre"));
    }
}
