//! Configuration: TOML file plus environment overrides, validated up front so
//! a bad deploy fails fast with an actionable message instead of a mystery at
//! request time.

use std::path::Path;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::error::{ReconError, Result};

/// Kite chain presets. Only values published in the official docs are baked in;
/// token addresses are always supplied by the operator because mainnet and
/// testnet use different bridged assets.
pub fn preset_rpc(chain_id: u64) -> Option<&'static str> {
    match chain_id {
        2368 => Some("https://rpc-testnet.gokite.ai/"),
        2366 => Some("https://rpc.gokite.ai/"),
        _ => None,
    }
}

/// Token addresses published for Kite. Testnet comes from the Service Provider
/// Guide in the Kite docs; mainnet pairs assets via bridges and is intentionally
/// left to the operator, because guessing the settlement asset is unacceptable.
pub fn preset_token(chain_id: u64) -> Option<&'static str> {
    match chain_id {
        2368 => Some("0x0fF5393387ad2f9f691FD6Fd28e07E3969e27e63"),
        _ => None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChainMode {
    /// Read the configured RPC endpoint.
    Live,
    /// Read a recorded fixture; never contacts the network.
    Mock,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LedgerKind {
    /// One `LedgerRecord` per line (this project's canonical shape).
    File,
    /// JSONL of `kite-x402-hono` `SettlementReceipt` objects.
    HonoFile,
    Http,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ChainConfig {
    pub mode: ChainMode,
    pub rpc_url: String,
    pub chain_id: u64,
    /// ERC-20 settlement token. Required in live mode; the examples in the docs
    /// link the official contract list so operators can verify it.
    pub token_address: String,
    pub token_decimals: u8,
    /// Blocks that must be buried before a payment counts as final.
    pub min_confirmations: u64,
    pub timeout_ms: u64,
}

impl Default for ChainConfig {
    fn default() -> Self {
        Self {
            mode: ChainMode::Mock,
            rpc_url: preset_rpc(2368).unwrap_or_default().to_string(),
            chain_id: 2368,
            token_address: String::new(),
            token_decimals: 6,
            min_confirmations: 1,
            timeout_ms: 15_000,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LedgerConfig {
    pub kind: LedgerKind,
    pub path: String,
    pub url: String,
    /// Environment variable holding a bearer token for the ledger endpoint.
    /// The value itself never appears in a config file or a log line.
    pub bearer_token_env: String,
}

impl Default for LedgerConfig {
    fn default() -> Self {
        Self {
            kind: LedgerKind::File,
            path: "fixtures/ledger.jsonl".to_string(),
            url: String::new(),
            bearer_token_env: String::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ReconcileConfig {
    /// Address the gateway is supposed to be paying into. Every ledger record is
    /// checked against it, and orphan transfers are searched for it.
    pub receiver: String,
    pub window_minutes: u64,
    pub max_records: usize,
    /// Blocks scanned backwards from the tip when a record carries no tx hash.
    /// Ignored when `align_window` is on.
    pub search_lookback_blocks: u64,
    /// Search the block range `window_minutes` actually covers, measured from the
    /// chain. On by default: a lookback in blocks is misleading without the block
    /// time, and a month-wide sweep is not what "audit the last day" means.
    pub align_window: bool,
}

impl Default for ReconcileConfig {
    fn default() -> Self {
        Self {
            receiver: String::new(),
            window_minutes: 60,
            max_records: 500,
            search_lookback_blocks: 2_000,
            align_window: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ServerConfig {
    pub bind: String,
    /// Environment variable holding the API bearer token. Empty means the API
    /// is unauthenticated, which is only acceptable on a loopback bind.
    pub api_token_env: String,
    pub refresh_seconds: u64,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            bind: "127.0.0.1:8081".to_string(),
            api_token_env: "RECON_API_TOKEN".to_string(),
            refresh_seconds: 30,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MockConfig {
    pub fixture: String,
}

impl Default for MockConfig {
    fn default() -> Self {
        Self {
            fixture: "fixtures/chain_mock.json".to_string(),
        }
    }
}

/// Root configuration document (`recon.toml`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub chain: ChainConfig,
    pub ledger: LedgerConfig,
    pub reconcile: ReconcileConfig,
    pub server: ServerConfig,
    pub mock: MockConfig,
}

impl Config {
    /// Load from an optional TOML file, then apply environment overrides.
    pub fn load(path: Option<&Path>) -> Result<Self> {
        let mut config = match path {
            Some(path) if path.exists() => {
                let raw = std::fs::read_to_string(path)?;
                toml::from_str::<Config>(&raw)?
            }
            Some(path) => {
                return Err(ReconError::Config(format!(
                    "config file not found: {}",
                    path.display()
                )))
            }
            None => Config::default(),
        };
        config.apply_env()?;
        config.validate()?;
        Ok(config)
    }

    fn apply_env(&mut self) -> Result<()> {
        if let Some(value) = env("RECON_CHAIN_MODE") {
            self.chain.mode = ChainMode::from_str(&value)
                .map_err(|_| ReconError::Config("RECON_CHAIN_MODE must be live or mock".into()))?;
        }
        if let Some(value) = env("RECON_RPC_URL") {
            self.chain.rpc_url = value;
        }
        if let Some(value) = env("RECON_CHAIN_ID") {
            self.chain.chain_id = value
                .parse()
                .map_err(|_| ReconError::Config("RECON_CHAIN_ID must be an integer".into()))?;
        }
        if let Some(value) = env("RECON_TOKEN_ADDRESS") {
            self.chain.token_address = value;
        }
        if let Some(value) = env("RECON_TOKEN_DECIMALS") {
            self.chain.token_decimals = value
                .parse()
                .map_err(|_| ReconError::Config("RECON_TOKEN_DECIMALS must be 0..=36".into()))?;
        }
        if let Some(value) = env("RECON_MIN_CONFIRMATIONS") {
            self.chain.min_confirmations = value.parse().map_err(|_| {
                ReconError::Config("RECON_MIN_CONFIRMATIONS must be an integer".into())
            })?;
        }
        if let Some(value) = env("RECON_LEDGER_KIND") {
            self.ledger.kind = LedgerKind::from_str(&value).map_err(|_| {
                ReconError::Config("RECON_LEDGER_KIND must be file, hono_file or http".into())
            })?;
        }
        if let Some(value) = env("RECON_LEDGER_PATH") {
            self.ledger.path = value;
        }
        if let Some(value) = env("RECON_LEDGER_URL") {
            self.ledger.url = value;
        }
        if let Some(value) = env("RECON_LEDGER_TOKEN_ENV") {
            self.ledger.bearer_token_env = value;
        }
        if let Some(value) = env("RECON_WINDOW_MINUTES") {
            self.reconcile.window_minutes = value.parse().map_err(|_| {
                ReconError::Config("RECON_WINDOW_MINUTES must be an integer".into())
            })?;
        }
        if let Some(value) = env("RECON_RECEIVER") {
            self.reconcile.receiver = value;
        }
        if let Some(value) = env("RECON_LOOKBACK_BLOCKS") {
            self.reconcile.search_lookback_blocks = value.parse().map_err(|_| {
                ReconError::Config("RECON_LOOKBACK_BLOCKS must be an integer".into())
            })?;
        }
        if let Some(value) = env("RECON_ALIGN_WINDOW") {
            self.reconcile.align_window = parse_bool(&value).ok_or_else(|| {
                ReconError::Config("RECON_ALIGN_WINDOW must be true or false".into())
            })?;
        }
        if let Some(value) = env("RECON_MAX_RECORDS") {
            self.reconcile.max_records = value
                .parse()
                .map_err(|_| ReconError::Config("RECON_MAX_RECORDS must be an integer".into()))?;
        }
        if let Some(value) = env("RECON_BIND") {
            self.server.bind = value;
        }
        if let Some(value) = env("RECON_REFRESH_SECONDS") {
            self.server.refresh_seconds = value.parse().map_err(|_| {
                ReconError::Config("RECON_REFRESH_SECONDS must be an integer".into())
            })?;
        }
        if let Some(value) = env("RECON_MOCK_FIXTURE") {
            self.mock.fixture = value;
        }
        Ok(())
    }

    /// Reject combinations that would otherwise fail at request time.
    pub fn validate(&self) -> Result<()> {
        if self.chain.chain_id == 0 {
            return Err(ReconError::Config("chain.chain_id must be positive".into()));
        }
        if self.chain.token_decimals > 36 {
            return Err(ReconError::Config(
                "chain.token_decimals must be 0..=36".into(),
            ));
        }
        if !(1..=600_000).contains(&self.chain.timeout_ms) {
            return Err(ReconError::Config(
                "chain.timeout_ms must be 1..=600000 (milliseconds)".into(),
            ));
        }
        match self.chain.mode {
            ChainMode::Live => {
                require_http_url("chain.rpc_url", &self.chain.rpc_url)?;
                validate_address("chain.token_address", &self.chain.token_address)?;
            }
            ChainMode::Mock => {
                if self.mock.fixture.trim().is_empty() {
                    return Err(ReconError::Config(
                        "mock.fixture is required in mock mode".into(),
                    ));
                }
            }
        }
        match self.ledger.kind {
            LedgerKind::File | LedgerKind::HonoFile => {
                if self.ledger.path.trim().is_empty() {
                    return Err(ReconError::Config(
                        "ledger.path is required for file ledgers".into(),
                    ));
                }
            }
            LedgerKind::Http => {
                require_http_url("ledger.url", &self.ledger.url)?;
            }
        }
        validate_address("reconcile.receiver", &self.reconcile.receiver)?;
        if !(1..=10_080).contains(&self.reconcile.window_minutes) {
            return Err(ReconError::Config(
                "reconcile.window_minutes must be 1..=10080".into(),
            ));
        }
        if !(1..=10_000).contains(&self.reconcile.max_records) {
            return Err(ReconError::Config(
                "reconcile.max_records must be 1..=10000".into(),
            ));
        }
        if !(1..=200_000).contains(&self.reconcile.search_lookback_blocks) {
            return Err(ReconError::Config(
                "reconcile.search_lookback_blocks must be 1..=200000".into(),
            ));
        }
        if !(1..=3_600).contains(&self.server.refresh_seconds) {
            return Err(ReconError::Config(
                "server.refresh_seconds must be 1..=3600".into(),
            ));
        }
        if self.server.bind.trim().is_empty() {
            return Err(ReconError::Config("server.bind is required".into()));
        }
        Ok(())
    }

    /// Resolve the API token from the configured environment variable.
    pub fn api_token(&self) -> Option<String> {
        env(&self.server.api_token_env).filter(|v| !v.trim().is_empty())
    }

    /// Resolve the ledger bearer token from the configured environment variable.
    pub fn ledger_token(&self) -> Option<String> {
        if self.ledger.bearer_token_env.trim().is_empty() {
            None
        } else {
            env(&self.ledger.bearer_token_env).filter(|v| !v.trim().is_empty())
        }
    }
}

impl FromStr for ChainMode {
    type Err = ();

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "live" => Ok(Self::Live),
            "mock" => Ok(Self::Mock),
            _ => Err(()),
        }
    }
}

impl FromStr for LedgerKind {
    type Err = ();

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "file" => Ok(Self::File),
            "hono" | "hono_file" => Ok(Self::HonoFile),
            "http" => Ok(Self::Http),
            _ => Err(()),
        }
    }
}

impl LedgerKind {
    /// Canonical configuration spelling, used in logs and `probe --json`.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::File => "file",
            Self::HonoFile => "hono_file",
            Self::Http => "http",
        }
    }
}

impl std::fmt::Display for LedgerKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

/// Accept the spellings operators actually type, instead of only `true`/`false`.
fn parse_bool(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

fn require_http_url(field: &str, value: &str) -> Result<()> {
    let lower = value.trim().to_ascii_lowercase();
    if !(lower.starts_with("http://") || lower.starts_with("https://")) {
        return Err(ReconError::Config(format!(
            "{field} must start with http:// or https://"
        )));
    }
    Ok(())
}

/// `0x` + 40 hex characters, compared case-insensitively elsewhere.
pub fn validate_address(field: &str, value: &str) -> Result<()> {
    let value = value.trim();
    let ok = value.len() == 42
        && value.starts_with("0x")
        && value[2..].bytes().all(|b| b.is_ascii_hexdigit());
    if !ok {
        return Err(ReconError::Config(format!(
            "{field} must be a 0x-prefixed 20-byte address"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn live_config() -> Config {
        let mut cfg = Config::default();
        cfg.chain.mode = ChainMode::Live;
        cfg.chain.token_address = "0x3Fdd283C4c43A60398bf93CA01a8a8BD773a755b".to_string();
        cfg.reconcile.receiver = "0x2222222222222222222222222222222222222222".to_string();
        cfg
    }

    #[test]
    fn receiver_is_required_in_every_mode() {
        let mut cfg = live_config();
        cfg.reconcile.receiver.clear();
        assert!(cfg.validate().is_err());
        cfg.reconcile.receiver = "0x2222".to_string();
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn live_mode_requires_a_token_address() {
        let mut cfg = live_config();
        cfg.chain.token_address.clear();
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn live_mode_requires_http_rpc() {
        let mut cfg = live_config();
        cfg.chain.rpc_url = "rpc-testnet.gokite.ai".to_string();
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn window_and_record_bounds_are_enforced() {
        let mut cfg = live_config();
        cfg.reconcile.window_minutes = 0;
        assert!(cfg.validate().is_err());

        let mut cfg = live_config();
        cfg.reconcile.max_records = 0;
        assert!(cfg.validate().is_err());

        let mut cfg = live_config();
        cfg.reconcile.search_lookback_blocks = 0;
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn http_ledger_requires_url() {
        let mut cfg = live_config();
        cfg.ledger.kind = LedgerKind::Http;
        cfg.ledger.url.clear();
        assert!(cfg.validate().is_err());
        cfg.ledger.url = "https://gateway.example.com/settlements".to_string();
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn addresses_are_shape_checked() {
        assert!(validate_address("pay_to", "0x3Fdd283C4c43A60398bf93CA01a8a8BD773a755b").is_ok());
        assert!(validate_address("pay_to", "0x123").is_err());
        assert!(validate_address("pay_to", "3Fdd283C4c43A60398bf93CA01a8a8BD773a755b").is_err());
    }
}
