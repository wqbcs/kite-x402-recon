//! Ledger ingestion. Two sources ship today: a JSONL file and an HTTP endpoint
//! that returns `{"records": [...]}`. Both produce validated [`LedgerRecord`]s
//! so the reconciler never has to defend against malformed input twice.

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::chain::BoxFuture;
use crate::error::{ReconError, Result};
use crate::model::LedgerRecord;

/// A source of gateway settlement records.
pub trait LedgerSource {
    fn describe(&self) -> String;
    fn fetch<'a>(&'a self) -> BoxFuture<'a, Result<Vec<LedgerRecord>>>;
}

/// JSONL on disk: one object per line, blank lines and `#` comments ignored.
#[derive(Debug, Clone)]
pub struct FileLedger {
    path: PathBuf,
    limit: usize,
}

impl FileLedger {
    pub fn new(path: impl Into<PathBuf>, limit: usize) -> Self {
        Self {
            path: path.into(),
            limit: limit.max(1),
        }
    }
}

impl LedgerSource for FileLedger {
    fn describe(&self) -> String {
        format!("file:{}", self.path.display())
    }

    fn fetch<'a>(&'a self) -> BoxFuture<'a, Result<Vec<LedgerRecord>>> {
        Box::pin(async move {
            let raw = std::fs::read_to_string(&self.path).map_err(|e| {
                ReconError::Ledger(format!("cannot read {}: {e}", self.path.display()))
            })?;
            let mut records = Vec::new();
            for (index, line) in raw.lines().enumerate() {
                let line = line.trim();
                if line.is_empty() || line.starts_with('#') {
                    continue;
                }
                let record = serde_json::from_str::<LedgerRecord>(line).map_err(|e| {
                    ReconError::Ledger(format!(
                        "{}:{} is not a valid ledger record: {e}",
                        self.path.display(),
                        index + 1
                    ))
                })?;
                records.push(record);
                if records.len() >= self.limit {
                    break;
                }
            }
            Ok(records)
        })
    }
}

/// HTTP ledger. The bearer token is read from the environment by the caller and
/// never stored in this struct's debug output.
#[derive(Clone)]
pub struct HttpLedger {
    client: reqwest::Client,
    url: String,
    token: Option<String>,
    limit: usize,
}

impl std::fmt::Debug for HttpLedger {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpLedger")
            .field("url", &self.url)
            .field("token", &self.token.as_ref().map(|_| "<redacted>"))
            .field("limit", &self.limit)
            .finish()
    }
}

impl HttpLedger {
    pub fn new(url: impl Into<String>, token: Option<String>, limit: usize) -> Result<Self> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(20))
            .user_agent(concat!("kite-x402-recon/", env!("CARGO_PKG_VERSION")))
            .build()?;
        Ok(Self {
            client,
            url: url.into(),
            token: token.filter(|t| !t.trim().is_empty()),
            limit: limit.max(1),
        })
    }
}

impl LedgerSource for HttpLedger {
    fn describe(&self) -> String {
        format!("http:{}", self.url)
    }

    fn fetch<'a>(&'a self) -> BoxFuture<'a, Result<Vec<LedgerRecord>>> {
        Box::pin(async move {
            let mut request = self
                .client
                .get(&self.url)
                .query(&[("limit", self.limit.to_string())]);
            if let Some(token) = &self.token {
                request = request.bearer_auth(token);
            }
            let response = request.send().await?;
            let status = response.status();
            let body = response.text().await?;
            if !status.is_success() {
                return Err(ReconError::Ledger(format!(
                    "{} returned HTTP {status}: {}",
                    self.url,
                    body.chars().take(200).collect::<String>()
                )));
            }
            parse_http_body(&body, self.limit)
        })
    }
}

/// Accept either a bare array or `{"records": [...]}` so operators do not have
/// to reshape an existing gateway endpoint.
fn parse_http_body(body: &str, limit: usize) -> Result<Vec<LedgerRecord>> {
    let value: serde_json::Value = serde_json::from_str(body)?;
    let records = match value {
        serde_json::Value::Array(items) => items,
        serde_json::Value::Object(mut map) => match map.remove("records") {
            Some(serde_json::Value::Array(items)) => items,
            _ => {
                return Err(ReconError::Ledger(
                    "ledger response must be an array or {\"records\": [...]}".into(),
                ))
            }
        },
        _ => {
            return Err(ReconError::Ledger(
                "ledger response must be an array or {\"records\": [...]}".into(),
            ))
        }
    };
    records
        .into_iter()
        .take(limit)
        .map(|item| {
            serde_json::from_value::<LedgerRecord>(item)
                .map_err(|e| ReconError::Ledger(format!("invalid ledger record in response: {e}")))
        })
        .collect()
}

/// JSONL dump of `kite-x402-hono` `SettlementReceipt` objects. This is the
/// ecosystem hook: the reference gateway already records these receipts in
/// memory; export them and this tool verifies each one against the chain
/// instead of trusting the gateway's own `success` flag.
#[derive(Debug, Clone)]
pub struct HonoReceiptFile {
    path: PathBuf,
    limit: usize,
}

/// One `SettlementReceipt` line. Extra fields the gateway may add later
/// (`success`, `network`, `errorReason`, …) are ignored on purpose: the chain
/// decides the outcome, so the gateway's own verdict is never imported.
#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct HonoReceipt {
    tx_hash: String,
    #[serde(default)]
    payer: Option<String>,
    pay_to: String,
    amount: serde_json::Value,
    #[serde(default)]
    timestamp: Option<serde_json::Value>,
}

impl HonoReceiptFile {
    pub fn new(path: impl Into<PathBuf>, limit: usize) -> Self {
        Self {
            path: path.into(),
            limit: limit.max(1),
        }
    }

    fn convert(receipt: HonoReceipt, line: usize, path: &Path) -> Result<LedgerRecord> {
        let amount_atomic = match receipt.amount {
            serde_json::Value::String(value) => value,
            serde_json::Value::Number(value) => value.to_string(),
            other => {
                return Err(ReconError::Ledger(format!(
                    "{}:{line} amount must be a string or a number, got {other}",
                    path.display()
                )));
            }
        };
        let observed_at = match receipt.timestamp {
            Some(serde_json::Value::String(value)) => value,
            Some(serde_json::Value::Number(value)) => {
                let raw = value.as_i64().unwrap_or(0);
                // Milliseconds are common in JS receipts; seconds fit in 10^11.
                let secs = if raw > 100_000_000_000 {
                    raw / 1_000
                } else {
                    raw
                };
                crate::model::format_unix(secs)
            }
            Some(_) | None => {
                return Err(ReconError::Ledger(format!(
                    "{}:{line} timestamp must be unix seconds/millis or RFC 3339",
                    path.display()
                )))
            }
        };
        Ok(LedgerRecord {
            id: receipt.tx_hash.to_ascii_lowercase(),
            pay_to: receipt.pay_to,
            payer: receipt.payer,
            amount_atomic,
            tx_hash: Some(receipt.tx_hash),
            observed_at,
            network: None,
        })
    }
}

impl LedgerSource for HonoReceiptFile {
    fn describe(&self) -> String {
        format!("hono_file:{}", self.path.display())
    }

    fn fetch<'a>(&'a self) -> BoxFuture<'a, Result<Vec<LedgerRecord>>> {
        Box::pin(async move {
            let raw = std::fs::read_to_string(&self.path).map_err(|e| {
                ReconError::Ledger(format!("cannot read {}: {e}", self.path.display()))
            })?;
            let mut records = Vec::new();
            for (index, line) in raw.lines().enumerate() {
                let line = line.trim();
                if line.is_empty() || line.starts_with('#') {
                    continue;
                }
                let receipt = serde_json::from_str::<HonoReceipt>(line).map_err(|e| {
                    ReconError::Ledger(format!(
                        "{}:{} is not a hono SettlementReceipt: {e}",
                        self.path.display(),
                        index + 1
                    ))
                })?;
                records.push(Self::convert(receipt, index + 1, &self.path)?);
                if records.len() >= self.limit {
                    break;
                }
            }
            Ok(records)
        })
    }
}

/// In-memory source; used by tests and by the server when a cached snapshot
/// should be replayed.
#[derive(Debug, Clone, Default)]
pub struct MemoryLedger {
    pub records: Vec<LedgerRecord>,
}

impl MemoryLedger {
    pub fn new(records: Vec<LedgerRecord>) -> Self {
        Self { records }
    }
}

impl LedgerSource for MemoryLedger {
    fn describe(&self) -> String {
        format!("memory:{}", self.records.len())
    }

    fn fetch<'a>(&'a self) -> BoxFuture<'a, Result<Vec<LedgerRecord>>> {
        Box::pin(async move { Ok(self.records.clone()) })
    }
}

/// Build a source from configuration.
pub fn build(
    kind: crate::config::LedgerKind,
    path: &str,
    url: &str,
    token: Option<String>,
    limit: usize,
) -> Result<Box<dyn LedgerSource + Send + Sync>> {
    match kind {
        crate::config::LedgerKind::File => Ok(Box::new(FileLedger::new(path, limit))),
        crate::config::LedgerKind::HonoFile => Ok(Box::new(HonoReceiptFile::new(path, limit))),
        crate::config::LedgerKind::Http => Ok(Box::new(HttpLedger::new(url, token, limit)?)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn http_bodies_are_tolerant_of_two_shapes() {
        let one = r#"[{"id":"inv_1","pay_to":"0x2222222222222222222222222222222222222222","amount_atomic":"10000","observed_at":"2026-11-22T10:00:00Z"}]"#;
        let two = r#"{"records":[{"id":"inv_1","pay_to":"0x2222222222222222222222222222222222222222","amount_atomic":"10000","observed_at":"2026-11-22T10:00:00Z"}]}"#;
        assert_eq!(parse_http_body(one, 10).map(|r| r.len()).unwrap_or(0), 1);
        assert_eq!(parse_http_body(two, 10).map(|r| r.len()).unwrap_or(0), 1);
        assert!(parse_http_body(r#"{"items":[]}"#, 10).is_err());
        assert!(parse_http_body("not json", 10).is_err());
    }

    #[test]
    fn file_ledger_reports_line_numbers() {
        let dir = std::env::temp_dir().join(format!("recon-ledger-{}", std::process::id()));
        std::fs::create_dir_all(&dir).ok();
        let path = dir.join("ledger.jsonl");
        let body = "# comment\n{\n";
        std::fs::write(&path, body).ok();
        let ledger = FileLedger::new(path.clone(), 10);
        let result = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map(|rt| rt.block_on(ledger.fetch()));
        match result {
            Ok(Err(ReconError::Ledger(message))) => {
                assert!(message.contains(":2"), "message was {message}");
            }
            other => panic!("unexpected result: {other:?}"),
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn hono_receipts_are_normalised() {
        let dir = std::env::temp_dir().join(format!("recon-hono-{}", std::process::id()));
        std::fs::create_dir_all(&dir).ok();
        let path = dir.join("receipts.jsonl");
        let body = concat!(
            "{\"txHash\":\"0xAAAA\",\"payer\":\"0x1111111111111111111111111111111111111111\",",
            "\"payTo\":\"0x2222222222222222222222222222222222222222\",\"amount\":\"10000\",",
            "\"network\":\"eip155:2368\",\"timestamp\":1763805600000,\"success\":true}\n"
        );
        std::fs::write(&path, body).ok();
        let ledger = HonoReceiptFile::new(path.clone(), 10);
        let records = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map(|rt| rt.block_on(ledger.fetch()));
        let records = match records {
            Ok(Ok(records)) => records,
            other => panic!("unexpected result: {other:?}"),
        };
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].id, "0xaaaa");
        assert_eq!(records[0].amount_atomic, "10000");
        assert_eq!(records[0].tx_hash.as_deref(), Some("0xAAAA"));
        // 1763805600000 ms -> 1763805600 s
        assert_eq!(
            records[0].observed_at,
            crate::model::format_unix(1_763_805_600)
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}
