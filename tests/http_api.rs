//! HTTP contract tests against the real router: auth, health, JSON shape.

use std::path::Path;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use kite_x402_recon::chain::ChainClient;
use kite_x402_recon::ledger::{FileLedger, MemoryLedger};
use kite_x402_recon::mock::FixtureChain;
use kite_x402_recon::reconcile::Reconciler;
use kite_x402_recon::server::{router, AppState};
use serde_json::Value;
use tower::ServiceExt;

fn state(token: Option<&str>) -> AppState {
    let fixture = match FixtureChain::load(Path::new("fixtures/chain_mock.json")) {
        Ok(chain) => chain,
        Err(e) => panic!("fixture: {e}"),
    };
    let tip = match blocking(fixture.tip()) {
        Ok(tip) => tip,
        Err(e) => panic!("tip: {e}"),
    };
    assert_eq!(tip, 1000);
    let ledger = match std::fs::read_to_string("fixtures/ledger.jsonl") {
        Ok(raw) => raw,
        Err(e) => panic!("ledger: {e}"),
    };
    let records = ledger
        .lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect::<Vec<_>>();
    AppState {
        reconciler: Arc::new(
            Reconciler::new(
                Arc::new(fixture),
                "0x2222222222222222222222222222222222222222",
                "",
                60,
                1,
                2000,
                500,
                "mock",
            )
            // Mirror what `reconciler_from_config` does for a served instance.
            .with_alignment(true),
        ),
        ledger: Arc::new(MemoryLedger::new(records)),
        token: token.map(str::to_string),
        mode: "mock".to_string(),
        refresh_seconds: 30,
        request_timeout: std::time::Duration::from_secs(15),
        cache: Default::default(),
    }
}

fn blocking<'a, T>(future: kite_x402_recon::chain::BoxFuture<'a, T>) -> T {
    match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt.block_on(future),
        Err(e) => panic!("runtime: {e}"),
    }
}

#[test]
fn healthz_is_open_and_names_the_mode() {
    let app = router(state(None));
    let response = match blocking_request(app, "/healthz", None) {
        Ok(response) => response,
        Err(e) => panic!("{e}"),
    };
    assert_eq!(response.0, StatusCode::OK);
    assert_eq!(response.1["mode"], "mock");
}

#[test]
fn readyz_touches_both_inputs_and_needs_no_token() {
    // A probe that needed a bearer token could not be wired into a container
    // readiness check, and one that only checked liveness would stay 200 with a
    // dead RPC. Assert both halves: it reports the tip it actually read and the
    // size of the ledger it actually parsed.
    let app = router(state(Some("s3cret")));
    let response = match blocking_request(app, "/readyz", None) {
        Ok(response) => response,
        Err(e) => panic!("{e}"),
    };
    assert_eq!(response.0, StatusCode::OK);
    assert_eq!(response.1["status"], "ready");
    assert_eq!(response.1["tip"], 1000);
    assert_eq!(response.1["records"], 7);
    assert_eq!(response.1["chain_id"], 2368);
    assert!(
        response.1["ledger"]
            .as_str()
            .unwrap_or_default()
            .starts_with("memory"),
        "{}",
        response.1
    );
}

#[test]
fn readyz_is_503_when_the_ledger_cannot_be_read() {
    let mut app_state = state(None);
    app_state.ledger = Arc::new(FileLedger::new(
        std::path::Path::new("fixtures/absent_ledger.jsonl"),
        500,
    ));
    let app = router(app_state);
    let response = match blocking_request(app, "/readyz", None) {
        Ok(response) => response,
        Err(e) => panic!("{e}"),
    };
    assert_eq!(response.0, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(response.1["status"], "unavailable");
    let reason = response.1["reason"].as_str().unwrap_or_default();
    assert!(reason.starts_with("ledger:"), "{reason}");
    assert!(reason.contains("absent_ledger.jsonl"), "{reason}");
}

#[test]
fn reconcile_requires_the_bearer_token_when_configured() {
    let app = router(state(Some("s3cret")));
    let response = match blocking_request(app, "/api/v1/reconcile", None) {
        Ok(response) => response,
        Err(e) => panic!("{e}"),
    };
    assert_eq!(response.0, StatusCode::UNAUTHORIZED);
    assert!(response.1["error"].as_str().is_some());
}

#[test]
fn reconcile_returns_the_same_summary_as_the_cli() {
    let app = router(state(Some("s3cret")));
    let response = match blocking_request(app, "/api/v1/reconcile", Some("s3cret")) {
        Ok(response) => response,
        Err(e) => panic!("{e}"),
    };
    assert_eq!(response.0, StatusCode::OK);
    assert_eq!(response.1["summary"]["total"], 7);
    assert_eq!(response.1["summary"]["matched"], 2);
    assert_eq!(response.1["summary"]["orphans"], 1);
    // The HTTP path goes through the same config as the CLI, so it inherits
    // window alignment instead of reporting a different scope.
    assert_eq!(response.1["search"]["lookback_blocks"], 300);
    assert_eq!(response.1["search"]["block_seconds"], 12);
}

#[test]
fn at_block_serves_a_replayed_window() {
    let app = router(state(Some("s3cret")));
    let response = match blocking_request(app, "/api/v1/reconcile?at_block=900", Some("s3cret")) {
        Ok(response) => response,
        Err(e) => panic!("{e}"),
    };
    assert_eq!(response.0, StatusCode::OK);
    assert_eq!(response.1["chain_tip"], 900);
    assert_eq!(response.1["anchored"], true);
    assert_eq!(response.1["search"]["to_block"], 900);
    assert_eq!(response.1["search"]["from_block"], 600);
    assert_eq!(response.1["search"]["block_seconds"], 12);
}

#[test]
fn at_block_rejects_bad_or_future_values() {
    for (query, message) in [
        ("?at_block=0", "positive integer"),
        ("?at_block=abc", "positive integer"),
        ("?at_block=99999", "ahead of the chain tip"),
    ] {
        let app = router(state(Some("s3cret")));
        let path = format!("/api/v1/reconcile{query}");
        let response = match blocking_request(app, &path, Some("s3cret")) {
            Ok(response) => response,
            Err(e) => panic!("{e}"),
        };
        assert_eq!(response.0, StatusCode::BAD_REQUEST, "{query}");
        let body = response.1["error"].as_str().unwrap_or_default();
        assert!(body.contains(message), "{query}: {body}");
    }
}

#[test]
fn a_poll_inside_the_refresh_window_is_served_from_the_cache() {
    // `Cache-Control: max-age=<refresh_seconds>` promises callers that a report
    // stays valid for one window. The service now honours the same window, so a
    // dashboard polling every few seconds gets one report per window rather than
    // one chain read per poll.
    let app = router(state(Some("s3cret")));
    let first = match blocking_request_raw(app.clone(), "/api/v1/reconcile", Some("s3cret")) {
        Ok(response) => response,
        Err(e) => panic!("{e}"),
    };
    assert_eq!(first.0, StatusCode::OK);
    assert_eq!(cache_state(&first.1), "miss");

    let second = match blocking_request_raw(app, "/api/v1/reconcile", Some("s3cret")) {
        Ok(response) => response,
        Err(e) => panic!("{e}"),
    };
    assert_eq!(second.0, StatusCode::OK);
    assert_eq!(cache_state(&second.1), "hit");
    assert_eq!(
        second.2, first.2,
        "a hit must replay the stored report, not a fresh one"
    );
}

#[test]
fn replayed_windows_are_never_served_from_the_cache() {
    // Two different `at_block` values share the same URL path, so a cache keyed
    // on the path alone would answer the second with the first window's verdict.
    let app = router(state(Some("s3cret")));
    let first = match blocking_request_raw(
        app.clone(),
        "/api/v1/reconcile?at_block=900",
        Some("s3cret"),
    ) {
        Ok(response) => response,
        Err(e) => panic!("{e}"),
    };
    assert_eq!(cache_state(&first.1), "bypass");
    let second = match blocking_request_raw(app, "/api/v1/reconcile?at_block=950", Some("s3cret")) {
        Ok(response) => response,
        Err(e) => panic!("{e}"),
    };
    assert_eq!(cache_state(&second.1), "bypass");
    let body = serde_json::from_slice::<Value>(&second.2).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(body["chain_tip"], 950);
}

fn cache_state(headers: &axum::http::HeaderMap) -> String {
    headers
        .get("x-recon-cache")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_owned()
}

/// A chain whose tip read hangs, to exercise the request deadline.
struct SlowChain {
    inner: FixtureChain,
    delay: std::time::Duration,
}

impl ChainClient for SlowChain {
    fn chain_id(&self) -> u64 {
        self.inner.chain_id()
    }

    fn tip<'a>(
        &'a self,
    ) -> kite_x402_recon::chain::BoxFuture<'a, kite_x402_recon::error::Result<u64>> {
        let delay = self.delay;
        Box::pin(async move {
            tokio::time::sleep(delay).await;
            self.inner.tip().await
        })
    }

    fn receipt<'a>(
        &'a self,
        tx_hash: &'a str,
    ) -> kite_x402_recon::chain::BoxFuture<
        'a,
        kite_x402_recon::error::Result<Option<kite_x402_recon::chain::Receipt>>,
    > {
        self.inner.receipt(tx_hash)
    }

    fn find_transfers<'a>(
        &'a self,
        query: kite_x402_recon::chain::TransferQuery,
    ) -> kite_x402_recon::chain::BoxFuture<
        'a,
        kite_x402_recon::error::Result<Vec<kite_x402_recon::chain::Transfer>>,
    > {
        self.inner.find_transfers(query)
    }

    fn token_decimals<'a>(
        &'a self,
    ) -> kite_x402_recon::chain::BoxFuture<'a, kite_x402_recon::error::Result<u8>> {
        self.inner.token_decimals()
    }

    fn token_symbol<'a>(
        &'a self,
    ) -> kite_x402_recon::chain::BoxFuture<'a, kite_x402_recon::error::Result<String>> {
        self.inner.token_symbol()
    }

    fn block_timestamp<'a>(
        &'a self,
        number: u64,
    ) -> kite_x402_recon::chain::BoxFuture<'a, kite_x402_recon::error::Result<u64>> {
        self.inner.block_timestamp(number)
    }
}

fn slow_state(
    token: Option<&str>,
    delay: std::time::Duration,
    timeout: std::time::Duration,
) -> AppState {
    let base = state(token);
    let fixture = match FixtureChain::load(Path::new("fixtures/chain_mock.json")) {
        Ok(chain) => chain,
        Err(e) => panic!("fixture: {e}"),
    };
    let records = match std::fs::read_to_string("fixtures/ledger.jsonl") {
        Ok(raw) => raw
            .lines()
            .filter(|line| !line.trim().is_empty())
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect::<Vec<_>>(),
        Err(e) => panic!("ledger: {e}"),
    };
    AppState {
        reconciler: Arc::new(
            Reconciler::new(
                Arc::new(SlowChain {
                    inner: fixture,
                    delay,
                }),
                "0x2222222222222222222222222222222222222222",
                "",
                60,
                1,
                2000,
                500,
                "mock",
            )
            .with_alignment(true),
        ),
        ledger: Arc::new(MemoryLedger::new(records)),
        request_timeout: timeout,
        ..base
    }
}

#[test]
fn a_chain_that_never_answers_ends_the_request_instead_of_pinning_it() {
    // The cache lock is held across the reconciliation, so an unbounded read
    // would park the request and every poll queued behind it. The deadline
    // turns that into a `504` and leaves the window empty for the next poll.
    let app = router(slow_state(
        Some("s3cret"),
        std::time::Duration::from_secs(5),
        std::time::Duration::from_millis(80),
    ));
    let (status, headers, body) =
        match blocking_request_raw(app.clone(), "/api/v1/reconcile", Some("s3cret")) {
            Ok(response) => response,
            Err(e) => panic!("{e}"),
        };
    assert_eq!(status, StatusCode::GATEWAY_TIMEOUT);
    assert!(
        headers.get("x-recon-cache").is_none(),
        "a timeout is not a report, so it must not claim a cache state"
    );
    let body = serde_json::from_slice::<Value>(&body).unwrap_or_else(|e| panic!("{e}"));
    assert!(
        body["error"]
            .as_str()
            .unwrap_or_default()
            .contains("did not finish"),
        "unexpected body: {body}"
    );

    let (status, _, body) = match blocking_request_raw(app, "/readyz", None) {
        Ok(response) => response,
        Err(e) => panic!("{e}"),
    };
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    let body = serde_json::from_slice::<Value>(&body).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(body["status"], "unavailable");
}

fn blocking_request(
    app: axum::Router,
    path: &str,
    token: Option<&str>,
) -> std::result::Result<(StatusCode, Value), String> {
    let (status, _headers, bytes) = blocking_request_raw(app, path, token)?;
    let value: Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    Ok((status, value))
}

fn blocking_request_raw(
    app: axum::Router,
    path: &str,
    token: Option<&str>,
) -> std::result::Result<(StatusCode, axum::http::HeaderMap, Vec<u8>), String> {
    let mut request = Request::builder().uri(path);
    if let Some(token) = token {
        request = request.header("authorization", format!("Bearer {token}"));
    }
    let request = request.body(Body::empty()).map_err(|e| e.to_string())?;

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    let response = runtime
        .block_on(async move { app.oneshot(request).await })
        .map_err(|e| e.to_string())?;
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = runtime
        .block_on(async move { response.into_body().collect().await })
        .map_err(|e| e.to_string())?
        .to_bytes()
        .to_vec();
    Ok((status, headers, bytes))
}

/// `format=csv` must serve the exact export the CLI writes, so an evidence
/// file can be pulled from the read-only endpoint without a second tool.
#[test]
fn csv_format_serves_the_same_export_as_the_cli() {
    let app = router(state(Some("s3cret")));
    let response = match blocking_request_raw(app, "/api/v1/reconcile?format=csv", Some("s3cret")) {
        Ok(response) => response,
        Err(e) => panic!("{e}"),
    };
    assert_eq!(response.0, StatusCode::OK);
    assert_eq!(
        response.1.get("content-type").and_then(|v| v.to_str().ok()),
        Some("text/csv; charset=utf-8")
    );
    let text = String::from_utf8(response.2).unwrap_or_else(|e| panic!("utf8: {e}"));
    assert_eq!(
        text.lines().next(),
        Some(
            "kind,id,outcome,pay_to,payer,amount_atomic,tx_hash,block_number,confirmations,recovered_by,reason"
        )
    );
    assert_eq!(text.lines().count(), 9, "{text}");
    assert!(text.contains("payment,inv_search,MATCHED,"), "{text}");
    assert!(text.contains("orphan,"), "{text}");
}

#[test]
fn csv_download_names_the_replayed_window() {
    let app = router(state(Some("s3cret")));
    let response = match blocking_request_raw(
        app,
        "/api/v1/reconcile?format=csv&at_block=900",
        Some("s3cret"),
    ) {
        Ok(response) => response,
        Err(e) => panic!("{e}"),
    };
    assert_eq!(response.0, StatusCode::OK);
    assert_eq!(
        response
            .1
            .get("content-disposition")
            .and_then(|v| v.to_str().ok()),
        Some("attachment; filename=\"reconcile-600-900.csv\"")
    );
}

#[test]
fn a_record_lookup_answers_one_transaction_without_a_window_pass() {
    // The support-ticket question is about one hash, not a block range: this
    // endpoint must return the same verdict the windowed report gives that
    // record, with the tip it judged it at, and must never be cached because
    // confirmations move with every block.
    let app = router(state(Some("s3cret")));
    let hash = "0x1111111111111111111111111111111111111111111111111111111111111111";
    let path = format!("/api/v1/records/{hash}");
    let (status, headers, bytes) = match blocking_request_raw(app, &path, Some("s3cret")) {
        Ok(response) => response,
        Err(e) => panic!("{e}"),
    };
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        headers.get("cache-control").and_then(|v| v.to_str().ok()),
        Some("no-store")
    );
    let body = serde_json::from_slice::<Value>(&bytes).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(body["chain_tip"], 1000);
    assert_eq!(body["chain_id"], 2368);
    assert_eq!(body["record"]["id"], "inv_ok");
    assert_eq!(body["record"]["outcome"], "matched");
    assert_eq!(
        body["record"]["tx_hash"],
        "0x1111111111111111111111111111111111111111111111111111111111111111"
    );
    assert!(body["record"]["confirmations"].as_u64().unwrap_or(0) > 0);
}

#[test]
fn a_record_lookup_accepts_the_hash_in_either_case() {
    // Ledger rows and block explorers disagree about hex case; a 404 for
    // `0X...` would look like "this payment never happened".
    let app = router(state(Some("s3cret")));
    let hash = "0X1111111111111111111111111111111111111111111111111111111111111111";
    let path = format!("/api/v1/records/{hash}");
    let response = match blocking_request(app, &path, Some("s3cret")) {
        Ok(response) => response,
        Err(e) => panic!("{e}"),
    };
    assert_eq!(response.0, StatusCode::OK);
    assert_eq!(response.1["record"]["id"], "inv_ok");
}

#[test]
fn a_record_lookup_rejects_a_malformed_hash() {
    for hash in ["0x123", "1111", "0xzzzz"] {
        let app = router(state(Some("s3cret")));
        let path = format!("/api/v1/records/{hash}");
        let response = match blocking_request(app, &path, Some("s3cret")) {
            Ok(response) => response,
            Err(e) => panic!("{e}"),
        };
        assert_eq!(response.0, StatusCode::BAD_REQUEST, "{hash}");
        let message = response.1["error"].as_str().unwrap_or_default();
        assert!(message.contains("32-byte hash"), "{hash}: {message}");
    }
}

#[test]
fn a_record_lookup_separates_an_unknown_hash_from_a_broken_input() {
    // A hash the ledger never mentions is an answer ("we did not record it"),
    // not an upstream failure; 404 keeps that distinct from 502.
    let app = router(state(Some("s3cret")));
    let hash = "0x9999999999999999999999999999999999999999999999999999999999999999";
    let path = format!("/api/v1/records/{hash}");
    let response = match blocking_request(app, &path, Some("s3cret")) {
        Ok(response) => response,
        Err(e) => panic!("{e}"),
    };
    assert_eq!(response.0, StatusCode::NOT_FOUND);
    let message = response.1["error"].as_str().unwrap_or_default();
    assert!(message.contains(hash), "{message}");
}

#[test]
fn a_record_lookup_needs_the_same_token_as_the_report() {
    let app = router(state(Some("s3cret")));
    let hash = "0x1111111111111111111111111111111111111111111111111111111111111111";
    let path = format!("/api/v1/records/{hash}");
    let response = match blocking_request(app, &path, None) {
        Ok(response) => response,
        Err(e) => panic!("{e}"),
    };
    assert_eq!(response.0, StatusCode::UNAUTHORIZED);
}

#[test]
fn a_record_lookup_respects_the_request_deadline() {
    // The same deadline that bounds a window pass bounds this lookup: a hung
    // tip read must end in `504`, not a request parked forever.
    let app = router(slow_state(
        Some("s3cret"),
        std::time::Duration::from_secs(5),
        std::time::Duration::from_millis(80),
    ));
    let hash = "0x1111111111111111111111111111111111111111111111111111111111111111";
    let path = format!("/api/v1/records/{hash}");
    let response = match blocking_request(app, &path, Some("s3cret")) {
        Ok(response) => response,
        Err(e) => panic!("{e}"),
    };
    assert_eq!(response.0, StatusCode::GATEWAY_TIMEOUT);
    let message = response.1["error"].as_str().unwrap_or_default();
    assert!(message.contains("did not finish"), "{message}");
}

#[test]
fn unknown_format_is_rejected_instead_of_silently_serving_json() {
    let app = router(state(Some("s3cret")));
    let response = match blocking_request(app, "/api/v1/reconcile?format=xml", Some("s3cret")) {
        Ok(response) => response,
        Err(e) => panic!("{e}"),
    };
    assert_eq!(response.0, StatusCode::BAD_REQUEST);
    let body = response.1["error"].as_str().unwrap_or_default();
    assert!(body.contains("format must be json or csv"), "{body}");
}
