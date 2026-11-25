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
        .route("/readyz", get(readyz))
        .route("/api/v1/reconcile", get(reconcile))
        .with_state(state)
}

/// Liveness only: the process answered. It deliberately says nothing about
/// whether a report could be produced right now, so it stays cheap and open.
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

/// Readiness: both inputs a report needs are usable, not just that the process
/// is alive. `/healthz` returning ok while the RPC or the ledger is broken is
/// the failure a load balancer must not route around, so this endpoint touches
/// the chain tip and reads the ledger, and answers 503 when either fails.
async fn readyz(State(state): State<AppState>) -> Response {
    match probe_readiness(&state).await {
        Ok((tip, records)) => (
            StatusCode::OK,
            Json(json!({
                "status": "ready",
                "chain_id": state.reconciler.chain_id(),
                "tip": tip,
                "ledger": state.ledger.describe(),
                "records": records,
            })),
        )
            .into_response(),
        Err(reason) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({ "status": "unavailable", "reason": reason })),
        )
            .into_response(),
    }
}

async fn probe_readiness(state: &AppState) -> std::result::Result<(u64, usize), String> {
    let tip = state
        .reconciler
        .current_tip()
        .await
        .map_err(|e| format!("chain: {e}"))?;
    let records = state
        .ledger
        .fetch()
        .await
        .map_err(|e| format!("ledger: {e}"))?;
    Ok((tip, records.len()))
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
        Ok(report) => report_response(&state, &report, &params),
        Err(e) => error_response(StatusCode::BAD_GATEWAY, &e.to_string()),
    }
}

/// Serialises a report in the representation the caller asked for.
///
/// JSON stays the default so existing consumers keep working; `format=csv`
/// serves the same bytes the CLI writes, which lets a spreadsheet or cron job
/// pull an evidence file straight from the read-only endpoint.
fn report_response(
    state: &AppState,
    report: &crate::model::Report,
    params: &HashMap<String, String>,
) -> Response {
    let format = match params.get("format").map(String::as_str) {
        None | Some("json") => OutputFormat::Json,
        Some("csv") => OutputFormat::Csv,
        Some(other) => {
            return error_response(
                StatusCode::BAD_REQUEST,
                &format!("format must be json or csv, got {other:?}"),
            )
        }
    };
    let (content_type, body, disposition) = match format {
        OutputFormat::Json => (
            "application/json",
            crate::report::to_json(report).unwrap_or_else(|e| {
                json!({ "error": format!("failed to serialise report: {e}") }).to_string()
            }),
            None,
        ),
        OutputFormat::Csv => (
            "text/csv; charset=utf-8",
            crate::report::to_csv(report),
            Some(format!(
                "attachment; filename=\"reconcile-{}-{}.csv\"",
                report.search.from_block, report.search.to_block
            )),
        ),
    };
    let mut response = (StatusCode::OK, body).into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static(content_type),
    );
    if let Some(value) = disposition.and_then(|raw| header::HeaderValue::from_str(&raw).ok()) {
        response
            .headers_mut()
            .insert(header::CONTENT_DISPOSITION, value);
    }
    let cache = header::HeaderValue::from_str(&format!("max-age={}", state.refresh_seconds))
        .unwrap_or_else(|_| header::HeaderValue::from_static("no-store"));
    response.headers_mut().insert(header::CACHE_CONTROL, cache);
    response
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum OutputFormat {
    Json,
    Csv,
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
