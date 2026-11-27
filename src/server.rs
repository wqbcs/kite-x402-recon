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
use crate::model::{now_unix, Report};
use crate::reconcile::Reconciler;

#[derive(Clone)]
pub struct AppState {
    pub reconciler: Arc<Reconciler>,
    pub ledger: Arc<dyn LedgerSource + Send + Sync>,
    pub token: Option<String>,
    pub mode: String,
    pub refresh_seconds: u64,
    /// Longest a request waits for one reconciliation before answering `504`.
    pub request_timeout: std::time::Duration,
    /// The report `serve` keeps until `refresh_seconds` elapses.
    pub cache: ReportCache,
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

#[derive(Debug, Clone)]
struct CachedReport {
    report: Report,
    stored_at_unix: i64,
}

/// Holds one reconciled report until the advertised `refresh_seconds` elapses.
///
/// The handler used to reconcile on every request while advertising
/// `Cache-Control: max-age=<refresh_seconds>`. Anyone caching on that header
/// saw a report that was up to a refresh window old, while the service itself
/// paid a chain tip read and a ledger parse for every poll — the two sides never
/// agreed on what the window meant. Now the server honours the same window, so
/// a dashboard polling faster than it gets byte-identical answers and the RPC
/// sees one read per window instead of one per request.
#[derive(Debug, Clone, Default)]
pub struct ReportCache(Arc<tokio::sync::Mutex<Option<CachedReport>>>);

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
    // A hung RPC must read as "not ready", not as a request that never returns:
    // a probe with no deadline cannot fail the way a load balancer needs.
    let readiness = tokio::time::timeout(state.request_timeout, probe_readiness(&state)).await;
    match readiness {
        Ok(Ok((tip, records))) => (
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
        Ok(Err(reason)) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({ "status": "unavailable", "reason": reason })),
        )
            .into_response(),
        Err(_) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({
                "status": "unavailable",
                "reason": format!(
                    "readiness did not finish within {}s",
                    state.request_timeout.as_secs()
                ),
            })),
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
    let format = match parse_format(&params) {
        Ok(format) => format,
        Err(message) => return error_response(StatusCode::BAD_REQUEST, &message),
    };
    // A replayed window is a distinct question about a distinct block range,
    // so it is answered from the chain every time and never enters the cache.
    if let Some(raw) = params.get("at_block") {
        return match tokio::time::timeout(
            state.request_timeout,
            replayed_window(&state, format, raw),
        )
        .await
        {
            Ok(response) => response,
            Err(_) => timeout_response(&state),
        };
    }

    let now = now_unix();
    // The refresh window is the same one `Cache-Control` advertises. Holding
    // the lock across the reconciliation makes the window single-flight: a
    // burst of polls waits for one report instead of queueing an RPC read each.
    let mut cached = state.cache.0.lock().await;
    if let Some(hit) = cached
        .as_ref()
        .filter(|hit| now.saturating_sub(hit.stored_at_unix) < state.refresh_seconds as i64)
    {
        return report_response(&state, &hit.report, format, CacheState::Hit);
    }

    // The single-flight lock makes one slow chain fatal to every poll, so the
    // reconciliation itself is bounded: a request that cannot get an answer in
    // time says so and leaves the window unpopulated for the next attempt.
    let report = match tokio::time::timeout(
        state.request_timeout,
        state.reconciler.report(&*state.ledger, now),
    )
    .await
    {
        Ok(Ok(report)) => report,
        Ok(Err(e)) => return error_response(StatusCode::BAD_GATEWAY, &e.to_string()),
        Err(_) => return timeout_response(&state),
    };
    *cached = Some(CachedReport {
        report: report.clone(),
        stored_at_unix: now,
    });
    report_response(&state, &report, format, CacheState::Miss)
}

/// `?at_block=<n>`: reconcile a past window against the current chain.
async fn replayed_window(state: &AppState, format: OutputFormat, raw: &str) -> Response {
    let block = match raw.parse::<u64>() {
        Ok(block) if block > 0 => block,
        _ => {
            return error_response(
                StatusCode::BAD_REQUEST,
                "at_block must be a positive integer",
            )
        }
    };
    let mut reconciler = (*state.reconciler).clone();
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
    match reconciler.report(&*state.ledger, now_unix()).await {
        Ok(report) => report_response(state, &report, format, CacheState::Bypass),
        Err(e) => error_response(StatusCode::BAD_GATEWAY, &e.to_string()),
    }
}

fn parse_format(params: &HashMap<String, String>) -> std::result::Result<OutputFormat, String> {
    match params.get("format").map(String::as_str) {
        None | Some("json") => Ok(OutputFormat::Json),
        Some("csv") => Ok(OutputFormat::Csv),
        Some(other) => Err(format!("format must be json or csv, got {other:?}")),
    }
}

/// How the response was produced, surfaced as `X-Recon-Cache`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CacheState {
    /// Served from the report retained for this refresh window.
    Hit,
    /// Reconciled for this request and stored for the next one.
    Miss,
    /// A replayed window: never cached, always reconciled.
    Bypass,
}

impl CacheState {
    fn as_str(self) -> &'static str {
        match self {
            CacheState::Hit => "hit",
            CacheState::Miss => "miss",
            CacheState::Bypass => "bypass",
        }
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
    format: OutputFormat,
    cache_state: CacheState,
) -> Response {
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
    response.headers_mut().insert(
        CACHE_STATE_HEADER,
        header::HeaderValue::from_static(cache_state.as_str()),
    );
    response
}

/// Response header naming whether the report came from the refresh cache.
pub const CACHE_STATE_HEADER: &str = "X-Recon-Cache";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum OutputFormat {
    Json,
    Csv,
}

fn error_response(status: StatusCode, message: &str) -> Response {
    (status, Json(json!({ "error": message }))).into_response()
}

/// A reconciliation that outlived `server.request_timeout_seconds`.
///
/// `504` rather than `502`: the chain and the ledger are not known to be
/// wrong, the service just could not finish reading them in time.
fn timeout_response(state: &AppState) -> Response {
    let seconds = state.request_timeout.as_secs();
    error_response(
        StatusCode::GATEWAY_TIMEOUT,
        &format!("reconciliation did not finish within {seconds}s"),
    )
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
