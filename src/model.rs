//! Core domain types shared by the CLI, the reconciler and the HTTP API.

use serde::{Deserialize, Serialize};

use crate::error::ReconError;

/// Amount in the token's smallest unit, carried as a decimal string so no
/// precision is lost in JSON round-trips.
pub type AmountAtomic = u128;

/// Network identity of a settlement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkInfo {
    pub chain_id: u64,
    pub token_address: String,
    pub token_decimals: u8,
}

/// One gateway-side settlement record, normalised from a ledger file.
///
/// `id` is the gateway's own identifier (request id, invoice id, ...). It is
/// echoed in the report so operators can map a finding back to their logs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LedgerRecord {
    pub id: String,
    pub pay_to: String,
    #[serde(default)]
    pub payer: Option<String>,
    /// Atomic units, decimal string, for example `"10000"` for 0.01 USDT with
    /// 6 decimals.
    pub amount_atomic: String,
    #[serde(default)]
    pub tx_hash: Option<String>,
    /// RFC 3339 timestamp of when the gateway observed the settlement.
    pub observed_at: String,
    #[serde(default)]
    pub network: Option<NetworkInfo>,
}

impl LedgerRecord {
    /// Parse the atomic amount, rejecting signs, separators and overflow.
    pub fn amount(&self) -> Result<AmountAtomic, ReconError> {
        parse_atomic(&self.amount_atomic)
    }

    /// Unix seconds of `observed_at`.
    pub fn observed_unix(&self) -> Result<i64, ReconError> {
        parse_rfc3339(&self.observed_at)
    }
}

/// Per-record outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// Chain evidence confirms a successful transfer of at least the requested
    /// amount to the configured receiver.
    Matched,
    /// No conclusive chain evidence yet (no receipt, too few confirmations, or
    /// nothing found in the search window).
    Pending,
    /// A receipt exists but the transfer does not match amount or receiver.
    Mismatched,
    /// The transaction executed and reverted.
    Failed,
    /// The record could not be evaluated (invalid amount, malformed hash...).
    Unknown,
}

impl Outcome {
    pub fn is_difference(self) -> bool {
        matches!(self, Outcome::Mismatched | Outcome::Failed)
    }
}

/// A single reconciled record.
#[derive(Debug, Clone, Serialize)]
pub struct ReconciledRecord {
    pub id: String,
    pub pay_to: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payer: Option<String>,
    pub amount_atomic: String,
    pub outcome: Outcome,
    pub reason: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tx_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub block_number: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confirmations: Option<u64>,
    /// Set when the payment was found by scanning logs instead of by `tx_hash`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recovered_by: Option<String>,
}

/// An on-chain transfer to the receiver with no matching ledger record.
#[derive(Debug, Clone, Serialize)]
pub struct OrphanTransfer {
    pub tx_hash: String,
    pub block_number: u64,
    pub from: String,
    pub to: String,
    pub amount_atomic: String,
}

/// Aggregated counts for the run.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Summary {
    pub total: usize,
    pub matched: usize,
    pub pending: usize,
    pub mismatched: usize,
    pub failed: usize,
    pub unknown: usize,
    pub orphans: usize,
    /// Records that claimed a transaction hash already claimed by an earlier
    /// record. Always counted inside `unknown` as well.
    pub duplicates: usize,
}

impl Summary {
    pub fn record(&mut self, outcome: Outcome) {
        self.total += 1;
        match outcome {
            Outcome::Matched => self.matched += 1,
            Outcome::Pending => self.pending += 1,
            Outcome::Mismatched => self.mismatched += 1,
            Outcome::Failed => self.failed += 1,
            Outcome::Unknown => self.unknown += 1,
        }
    }

    /// Number of findings that must fail a CI gate.
    pub fn differences(&self) -> usize {
        self.mismatched + self.failed
    }
}

/// Full report returned by `scan` and `/api/v1/reconcile`.
#[derive(Debug, Clone, Serialize)]
pub struct Report {
    /// `live` or `mock`; always surfaced so a fixture run can never be mistaken
    /// for on-chain evidence.
    pub mode: String,
    pub chain_id: u64,
    pub generated_at: String,
    pub window_minutes: u64,
    pub chain_tip: u64,
    pub summary: Summary,
    pub records: Vec<ReconciledRecord>,
    pub orphans: Vec<OrphanTransfer>,
}

/// Parse an atomic amount from a decimal string.
pub fn parse_atomic(raw: &str) -> Result<AmountAtomic, ReconError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(ReconError::InvalidAmount(raw.to_string()));
    }
    if !trimmed.bytes().all(|b| b.is_ascii_digit()) {
        return Err(ReconError::InvalidAmount(raw.to_string()));
    }
    trimmed
        .parse::<AmountAtomic>()
        .map_err(|_| ReconError::InvalidAmount(raw.to_string()))
}

/// Parse an RFC 3339 timestamp into unix seconds.
pub fn parse_rfc3339(raw: &str) -> Result<i64, ReconError> {
    let parsed = time::OffsetDateTime::parse(raw, &time::format_description::well_known::Rfc3339)
        .map_err(|_| ReconError::InvalidTimestamp(raw.to_string()))?;
    Ok(parsed.unix_timestamp())
}

/// Format unix seconds as RFC 3339 UTC.
pub fn format_unix(secs: i64) -> String {
    time::OffsetDateTime::from_unix_timestamp(secs)
        .unwrap_or(time::OffsetDateTime::UNIX_EPOCH)
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
}

/// Current unix time, injectable so tests stay deterministic.
pub fn now_unix() -> i64 {
    time::OffsetDateTime::now_utc().unix_timestamp()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn amounts_accept_plain_digits() {
        assert_eq!(parse_atomic("10000").unwrap_or(0), 10_000);
        assert_eq!(parse_atomic(" 7 ").unwrap_or(0), 7);
    }

    #[test]
    fn amounts_reject_signs_and_separators() {
        for bad in ["-1", "+1", "1.5", "1,000", "", "  ", "0x10"] {
            assert!(parse_atomic(bad).is_err(), "{bad} should be rejected");
        }
    }

    #[test]
    fn amounts_reject_overflow() {
        assert!(parse_atomic("340282366920938463463374607431768211456").is_err());
    }

    #[test]
    fn timestamps_round_trip() {
        let secs = parse_rfc3339("2026-11-22T14:00:00Z").unwrap_or(0);
        assert_eq!(format_unix(secs), "2026-11-22T14:00:00Z");
    }

    #[test]
    fn summary_counts_differences() {
        let mut s = Summary::default();
        s.record(Outcome::Matched);
        s.record(Outcome::Mismatched);
        s.record(Outcome::Failed);
        s.record(Outcome::Pending);
        assert_eq!(s.total, 4);
        assert_eq!(s.differences(), 2);
    }
}
