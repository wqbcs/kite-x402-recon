//! Read-only settlement reconciliation for x402 services on Kite.
//!
//! Library entry points are kept small: build a chain reader, build a ledger
//! source, build a reconciler, then call [`Reconciler::report`]. The CLI and the
//! HTTP server are thin shells around exactly that.

pub mod chain;
pub mod config;
pub mod error;
pub mod ledger;
pub mod mock;
pub mod model;
pub mod reconcile;
pub mod report;
pub mod server;

use std::path::Path;
use std::sync::Arc;

use chain::{ChainClient, RpcClient};
use config::{ChainMode, Config, LedgerKind};
use error::Result;
use ledger::LedgerSource;
use mock::FixtureChain;
use reconcile::Reconciler;

/// Chain reader for the configured mode. `live` never falls back to fixtures.
pub fn chain_from_config(config: &Config) -> Result<Arc<dyn ChainClient + Send + Sync>> {
    match config.chain.mode {
        ChainMode::Live => {
            let client = RpcClient::new(
                config.chain.rpc_url.clone(),
                config.chain.chain_id,
                config.chain.token_address.clone(),
                config.chain.token_decimals,
                config.chain.timeout_ms,
            )?;
            Ok(Arc::new(client))
        }
        ChainMode::Mock => {
            let fixture = FixtureChain::load(Path::new(&config.mock.fixture))?;
            Ok(Arc::new(fixture))
        }
    }
}

/// Ledger source for the configured kind.
pub fn ledger_from_config(config: &Config) -> Result<Arc<dyn LedgerSource + Send + Sync>> {
    let source = ledger::build(
        match config.ledger.kind {
            LedgerKind::File => LedgerKind::File,
            LedgerKind::Http => LedgerKind::Http,
        },
        &config.ledger.path,
        &config.ledger.url,
        config.ledger_token(),
        config.reconcile.max_records,
    )?;
    Ok(Arc::from(source))
}

/// Reconciler wired from configuration, sharing the supplied chain reader.
pub fn reconciler_from_config(
    config: &Config,
    chain: Arc<dyn ChainClient + Send + Sync>,
) -> Reconciler {
    Reconciler::new(
        chain,
        config.reconcile.receiver.clone(),
        config.chain.token_address.clone(),
        config.reconcile.window_minutes,
        config.chain.min_confirmations,
        config.reconcile.search_lookback_blocks,
        config.reconcile.max_records,
        mode_label(config),
    )
}

/// `live` or `mock`, surfaced in every report.
pub fn mode_label(config: &Config) -> String {
    match config.chain.mode {
        ChainMode::Live => "live".to_string(),
        ChainMode::Mock => "mock".to_string(),
    }
}
