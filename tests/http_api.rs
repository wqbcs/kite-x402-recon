//! HTTP contract tests against the real router: auth, health, JSON shape.

use std::path::Path;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use kite_x402_recon::chain::ChainClient;
use kite_x402_recon::ledger::MemoryLedger;
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

fn blocking_request(
    app: axum::Router,
    path: &str,
    token: Option<&str>,
) -> std::result::Result<(StatusCode, Value), String> {
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
    let bytes = runtime
        .block_on(async move { response.into_body().collect().await })
        .map_err(|e| e.to_string())?
        .to_bytes();
    let value: Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    Ok((status, value))
}
