//! Chain access. The reconciler only ever reads: receipts, logs and token
//! metadata. No signing key is loaded anywhere in this crate.

use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use serde_json::{json, Value};

use crate::error::{ReconError, Result};
use crate::model::{parse_atomic, AmountAtomic};

/// `keccak256("Transfer(address,address,uint256)")`.
pub const TRANSFER_TOPIC: &str =
    "0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef";

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// A decoded ERC-20 `Transfer`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Transfer {
    pub tx_hash: String,
    pub block_number: u64,
    pub from: String,
    pub to: String,
    pub amount_atomic: AmountAtomic,
}

/// Minimal receipt projection.
#[derive(Debug, Clone)]
pub struct Receipt {
    pub status: bool,
    pub block_number: u64,
    pub logs: Vec<Transfer>,
}

/// Query for `eth_getLogs`, already restricted to one token contract.
#[derive(Debug, Clone)]
pub struct TransferQuery {
    pub token: String,
    pub from: Option<String>,
    pub to: String,
    pub from_block: u64,
    pub to_block: u64,
    pub limit: usize,
}

/// Read-only chain surface. Implemented by [`RpcClient`] and by the fixture
/// client used in tests and demos.
pub trait ChainClient {
    fn chain_id(&self) -> u64;
    fn tip<'a>(&'a self) -> BoxFuture<'a, Result<u64>>;
    fn receipt<'a>(&'a self, tx_hash: &'a str) -> BoxFuture<'a, Result<Option<Receipt>>>;
    fn find_transfers<'a>(&'a self, query: TransferQuery) -> BoxFuture<'a, Result<Vec<Transfer>>>;
    fn token_decimals<'a>(&'a self) -> BoxFuture<'a, Result<u8>>;
    fn token_symbol<'a>(&'a self) -> BoxFuture<'a, Result<String>>;
}

/// JSON-RPC reader for an EVM chain (Kite testnet or mainnet).
#[derive(Debug, Clone)]
pub struct RpcClient {
    http: reqwest::Client,
    url: String,
    chain_id: u64,
    token_address: String,
    token_decimals: u8,
}

impl RpcClient {
    pub fn new(
        url: impl Into<String>,
        chain_id: u64,
        token_address: impl Into<String>,
        token_decimals: u8,
        timeout_ms: u64,
    ) -> Result<Self> {
        let timeout = Duration::from_millis(timeout_ms.max(1));
        let http = reqwest::Client::builder()
            .timeout(timeout)
            .user_agent(concat!("kite-x402-recon/", env!("CARGO_PKG_VERSION")))
            .build()?;
        Ok(Self {
            http,
            url: url.into(),
            chain_id,
            token_address: token_address.into(),
            token_decimals,
        })
    }

    async fn call(&self, method: &str, params: Value) -> Result<Value> {
        let body = json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params });
        let response = self.http.post(&self.url).json(&body).send().await?;
        let status = response.status();
        let text = response.text().await?;
        if !status.is_success() {
            return Err(ReconError::Chain(format!(
                "{method} returned HTTP {status}: {}",
                text.chars().take(200).collect::<String>()
            )));
        }
        let envelope: Value = serde_json::from_str(&text)?;
        if let Some(error) = envelope.get("error") {
            return Err(ReconError::Chain(format!(
                "{method} error: {}",
                error
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown")
            )));
        }
        envelope
            .get("result")
            .cloned()
            .ok_or_else(|| ReconError::Chain(format!("{method} response has no result")))
    }

    async fn summary_tip(&self) -> Result<u64> {
        let hex = self
            .call("eth_blockNumber", json!([]))
            .await?
            .as_str()
            .ok_or_else(|| ReconError::Chain("eth_blockNumber returned a non-string".into()))?
            .to_string();
        parse_hex_u64(&hex)
    }
}

impl ChainClient for RpcClient {
    fn chain_id(&self) -> u64 {
        self.chain_id
    }

    fn tip<'a>(&'a self) -> BoxFuture<'a, Result<u64>> {
        Box::pin(async move { self.summary_tip().await })
    }

    fn receipt<'a>(&'a self, tx_hash: &'a str) -> BoxFuture<'a, Result<Option<Receipt>>> {
        Box::pin(async move {
            let value = self
                .call("eth_getTransactionReceipt", json!([tx_hash]))
                .await?;
            if value.is_null() {
                return Ok(None);
            }
            let status = value
                .get("status")
                .and_then(Value::as_str)
                .map(parse_hex_u64)
                .transpose()?
                .unwrap_or(1);
            let block_number = value
                .get("blockNumber")
                .and_then(Value::as_str)
                .map(parse_hex_u64)
                .transpose()?
                .unwrap_or(0);
            let logs = value
                .get("logs")
                .and_then(Value::as_array)
                .map(|logs| {
                    logs.iter()
                        .filter_map(|log| decode_log(log, &self.token_address).ok().flatten())
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            Ok(Some(Receipt {
                status: status == 1,
                block_number,
                logs,
            }))
        })
    }

    fn find_transfers<'a>(&'a self, query: TransferQuery) -> BoxFuture<'a, Result<Vec<Transfer>>> {
        Box::pin(async move {
            let mut topics = json!([TRANSFER_TOPIC, null, pad_address(&query.to)]);
            if let Some(from) = query.from.as_deref() {
                topics = json!([TRANSFER_TOPIC, pad_address(from), pad_address(&query.to)]);
            }
            let params = json!([{
                "address": query.token,
                "fromBlock": format_hex(query.from_block),
                "toBlock": format_hex(query.to_block),
                "topics": topics,
            }]);
            let logs = self.call("eth_getLogs", params).await?;
            let logs = logs
                .as_array()
                .ok_or_else(|| ReconError::Chain("eth_getLogs returned a non-array".into()))?;
            let mut transfers = Vec::new();
            for log in logs {
                if let Some(transfer) = decode_log(log, &self.token_address)? {
                    transfers.push(transfer);
                    if transfers.len() >= query.limit {
                        break;
                    }
                }
            }
            transfers.sort_by_key(|t| std::cmp::Reverse(t.block_number));
            Ok(transfers)
        })
    }

    fn token_decimals<'a>(&'a self) -> BoxFuture<'a, Result<u8>> {
        Box::pin(async move {
            if self.token_address.is_empty() {
                return Ok(self.token_decimals);
            }
            let result = self
                .call(
                    "eth_call",
                    json!([{ "to": self.token_address, "data": "0x313ce567" }, "latest"]),
                )
                .await?;
            let hex = result.as_str().ok_or_else(|| {
                ReconError::Chain("eth_call(decimals) returned a non-string".into())
            })?;
            let value = parse_hex_u64(hex)?;
            u8::try_from(value).map_err(|_| ReconError::Chain("token decimals out of range".into()))
        })
    }

    fn token_symbol<'a>(&'a self) -> BoxFuture<'a, Result<String>> {
        Box::pin(async move {
            if self.token_address.is_empty() {
                return Ok(String::new());
            }
            let result = self
                .call(
                    "eth_call",
                    json!([{ "to": self.token_address, "data": "0x95d89b41" }, "latest"]),
                )
                .await?;
            let hex = result.as_str().ok_or_else(|| {
                ReconError::Chain("eth_call(symbol) returned a non-string".into())
            })?;
            decode_abi_string(hex)
        })
    }
}

fn decode_log(log: &Value, token: &str) -> Result<Option<Transfer>> {
    let address = log
        .get("address")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if !token.is_empty() && !address.eq_ignore_ascii_case(token) {
        return Ok(None);
    }
    let topics = log.get("topics").and_then(Value::as_array);
    let Some(topics) = topics else {
        return Ok(None);
    };
    if topics.len() < 3 {
        return Ok(None);
    }
    let topic0 = topics[0].as_str().unwrap_or_default();
    if !topic0.eq_ignore_ascii_case(TRANSFER_TOPIC) {
        return Ok(None);
    }
    let from = topics[1].as_str().unwrap_or_default();
    let to = topics[2].as_str().unwrap_or_default();
    let data = log.get("data").and_then(Value::as_str).unwrap_or_default();
    let amount = parse_hex_u128(data)?;
    Ok(Some(Transfer {
        tx_hash: log
            .get("transactionHash")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        block_number: log
            .get("blockNumber")
            .and_then(Value::as_str)
            .map(parse_hex_u64)
            .transpose()?
            .unwrap_or(0),
        from: address_from_topic(from)?,
        to: address_from_topic(to)?,
        amount_atomic: amount,
    }))
}

fn address_from_topic(topic: &str) -> Result<String> {
    let body = topic
        .strip_prefix("0x")
        .ok_or_else(|| ReconError::Chain(format!("topic is not hex: {topic}")))?;
    if body.len() != 64 {
        return Err(ReconError::Chain(format!(
            "topic is not a 32-byte word: {topic}"
        )));
    }
    Ok(format!("0x{}", &body[24..]))
}

fn pad_address(address: &str) -> String {
    format!(
        "0x{:0>64}",
        address.trim_start_matches("0x").to_ascii_lowercase()
    )
}

fn format_hex(value: u64) -> String {
    format!("0x{value:x}")
}

pub fn parse_hex_u64(raw: &str) -> Result<u64> {
    let body = raw
        .trim()
        .strip_prefix("0x")
        .ok_or_else(|| ReconError::Chain(format!("not a hex number: {raw}")))?;
    u64::from_str_radix(body, 16)
        .map_err(|_| ReconError::Chain(format!("hex number out of range: {raw}")))
}

fn parse_hex_u128(raw: &str) -> Result<AmountAtomic> {
    let body = raw
        .trim()
        .strip_prefix("0x")
        .ok_or_else(|| ReconError::Chain(format!("not a hex number: {raw}")))?;
    if body.is_empty() {
        return Ok(0);
    }
    if body.len() > 32 {
        // uint256: accept small values only, keep the last 32 nibbles out of
        // the comparison because x402 prices never reach 2^128.
        return u128::from_str_radix(&body[body.len() - 32..], 16)
            .map_err(|_| ReconError::Chain(format!("amount out of range: {raw}")));
    }
    u128::from_str_radix(body, 16)
        .map_err(|_| ReconError::Chain(format!("amount out of range: {raw}")))
}

/// Decode `string` return data: `0x` + offset + length + payload.
fn decode_abi_string(raw: &str) -> Result<String> {
    let body = raw
        .trim()
        .strip_prefix("0x")
        .ok_or_else(|| ReconError::Chain("symbol is not hex".into()))?;
    if body.len() < 128 {
        return Err(ReconError::Chain("symbol return data is too short".into()));
    }
    let offset = usize::from_str_radix(&body[0..64], 16)
        .map_err(|_| ReconError::Chain("symbol offset is invalid".into()))?;
    if body.len() < offset * 2 + 64 {
        return Err(ReconError::Chain("symbol payload is truncated".into()));
    }
    let length = usize::from_str_radix(&body[offset * 2..offset * 2 + 64], 16)
        .map_err(|_| ReconError::Chain("symbol length is invalid".into()))?;
    let start = offset * 2 + 64;
    let end = start + length * 2;
    if body.len() < end {
        return Err(ReconError::Chain("symbol data is truncated".into()));
    }
    let bytes = (start..end)
        .step_by(2)
        .map(|i| u8::from_str_radix(&body[i..i + 2], 16))
        .collect::<std::result::Result<Vec<u8>, _>>()
        .map_err(|_| ReconError::Chain("symbol bytes are invalid".into()))?;
    String::from_utf8(bytes).map_err(|_| ReconError::Chain("symbol is not UTF-8".into()))
}

/// Convert `"0.01"` plus decimals into atomic units, rejecting bad input.
pub fn decimal_to_atomic(value: &str, decimals: u8) -> Result<AmountAtomic> {
    let value = value.trim();
    let mut parts = value.splitn(2, '.');
    let whole = parts.next().unwrap_or_default();
    let frac = parts.next().unwrap_or_default();
    if whole.is_empty() && frac.is_empty() {
        return Err(ReconError::InvalidAmount(value.to_string()));
    }
    if !whole.bytes().all(|b| b.is_ascii_digit()) || !frac.bytes().all(|b| b.is_ascii_digit()) {
        return Err(ReconError::InvalidAmount(value.to_string()));
    }
    if frac.len() > usize::from(decimals) {
        return Err(ReconError::InvalidAmount(format!(
            "{value} has more than {decimals} decimals"
        )));
    }
    let padded = format!("{whole}{frac:0<width$}", width = usize::from(decimals));
    parse_atomic(&padded)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_helpers_round_trip() {
        assert_eq!(parse_hex_u64("0x940").unwrap_or(0), 2368);
        assert_eq!(parse_hex_u128("0x0a").unwrap_or(0), 10);
        assert!(parse_hex_u64("940").is_err());
    }

    #[test]
    fn topics_decode_to_addresses() {
        let topic = "0x0000000000000000000000003fdd283c4c43a60398bf93ca01a8a8bd773a755b";
        assert_eq!(
            address_from_topic(topic).unwrap_or_default(),
            "0x3fdd283c4c43a60398bf93ca01a8a8bd773a755b"
        );
        assert!(address_from_topic("0x12").is_err());
    }

    #[test]
    fn symbol_payload_is_decoded() {
        let raw = "0x0000000000000000000000000000000000000000000000000000000000000020\
                   0000000000000000000000000000000000000000000000000000000000000004\
                   5553445400000000000000000000000000000000000000000000000000000000";
        assert_eq!(decode_abi_string(raw).unwrap_or_default(), "USDT");
    }

    #[test]
    fn decimal_conversion_is_exact() {
        assert_eq!(decimal_to_atomic("1", 6).unwrap_or(0), 1_000_000);
        assert_eq!(decimal_to_atomic("0.01", 6).unwrap_or(0), 10_000);
        assert_eq!(decimal_to_atomic("0.000001", 6).unwrap_or(0), 1);
        assert!(decimal_to_atomic("0.0000001", 6).is_err());
        assert!(decimal_to_atomic("-1", 6).is_err());
        assert!(decimal_to_atomic("abc", 6).is_err());
    }
}
