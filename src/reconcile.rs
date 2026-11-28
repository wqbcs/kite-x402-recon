//! Reconciliation rules. One pass of the ledger, one pass of the chain window,
//! and a deterministic outcome per record. Every decision records the reason so
//! a difference can be explained without re-running anything.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use crate::chain::{ChainClient, Receipt, Transfer, TransferQuery};
use crate::error::Result;
use crate::ledger::LedgerSource;
use crate::model::{
    format_unix, OrphanTransfer, Outcome, ReconciledRecord, Report, SearchWindow, Summary,
};

/// How far back to scan blocks when a record has no transaction hash.
pub const DEFAULT_LOOKBACK_BLOCKS: u64 = 2_000;
/// How many measured tips to remember. More than one keeps a dashboard that
/// alternates between a live read and a fixed replayed anchor from re-measuring
/// both on every pair of requests.
const MEASUREMENT_CACHE: usize = 8;

/// Cap on orphans reported per run, so one noisy receiver cannot flood the JSON.
const MAX_ORPHANS: usize = 50;

#[derive(Clone)]
pub struct Reconciler {
    chain: Arc<dyn ChainClient + Send + Sync>,
    receiver: String,
    token: String,
    window_minutes: u64,
    min_confirmations: u64,
    lookback_blocks: u64,
    max_records: usize,
    /// Derive the searched range from `window_minutes` and the chain's measured
    /// block time instead of trusting `lookback_blocks`.
    align_window: bool,
    /// Audit as of this block instead of the chain tip, so a past window can be
    /// re-checked later and produce the same verdicts.
    anchor_tip: Option<u64>,
    /// Filled in when alignment ran, so the report can state the measurement it
    /// used instead of only the resulting range.
    measured_block_seconds: Option<u64>,
    /// Recent `(tip, seconds)` measurements, newest first. The average is a pure
    /// function of the tip, so a long-lived instance (the HTTP server) reuses it
    /// instead of issuing two block reads per request; several entries keep a
    /// dashboard alternating between a live read and a replayed anchor from
    /// thrashing a single slot.
    clock: Arc<Mutex<Vec<(u64, u64)>>>,
    mode: String,
}

impl std::fmt::Debug for Reconciler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Reconciler")
            .field("receiver", &self.receiver)
            .field("token", &self.token)
            .field("window_minutes", &self.window_minutes)
            .field("min_confirmations", &self.min_confirmations)
            .field("lookback_blocks", &self.lookback_blocks)
            .field("align_window", &self.align_window)
            .field("anchor_tip", &self.anchor_tip)
            .field("max_records", &self.max_records)
            .field("mode", &self.mode)
            .finish_non_exhaustive()
    }
}

impl Reconciler {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        chain: Arc<dyn ChainClient + Send + Sync>,
        receiver: impl Into<String>,
        token: impl Into<String>,
        window_minutes: u64,
        min_confirmations: u64,
        lookback_blocks: u64,
        max_records: usize,
        mode: impl Into<String>,
    ) -> Self {
        Self {
            chain,
            receiver: receiver.into().trim().to_ascii_lowercase(),
            token: token.into(),
            window_minutes,
            min_confirmations: min_confirmations.max(1),
            lookback_blocks: lookback_blocks.max(1),
            max_records: max_records.max(1),
            align_window: false,
            anchor_tip: None,
            measured_block_seconds: None,
            clock: Arc::new(Mutex::new(Vec::new())),
            mode: mode.into(),
        }
    }

    pub fn chain_id(&self) -> u64 {
        self.chain.chain_id()
    }

    /// The receiver every record is judged against.
    #[must_use]
    pub fn receiver(&self) -> &str {
        &self.receiver
    }

    /// The chain tip right now, regardless of any anchor.
    pub async fn current_tip(&self) -> Result<u64> {
        self.chain.tip().await
    }

    /// Override how far back the chain window is searched.
    ///
    /// The configured value is in blocks, which only matches a wall-clock audit
    /// window when the block time is known. Callers that measured the chain's
    /// block time pass the derived block count here so the search range and the
    /// audit window describe the same period.
    pub fn with_lookback_blocks(mut self, blocks: u64) -> Self {
        self.lookback_blocks = blocks.max(1);
        self
    }

    /// Search the range `window_minutes` actually covers, measured from the chain.
    ///
    /// A lookback in blocks means nothing without the block time: 2000 blocks is
    /// hours on a fast chain and weeks on a slow one, and when it is weeks every
    /// audit drowns in transfers that settled long before the window opened.
    pub fn with_alignment(mut self, aligned: bool) -> Self {
        self.align_window = aligned;
        self
    }

    /// Evaluate the ledger as of `block` instead of the current tip.
    ///
    /// Orphan detection, confirmation counts and the aligned lookback all move
    /// with the anchor, so an audit of a past window can be replayed tomorrow
    /// and yield the same block range and the same verdicts.
    pub fn with_anchor_tip(mut self, block: u64) -> Self {
        self.anchor_tip = Some(block.max(1));
        self
    }

    /// Resolve the block range for this pass, measuring the chain when aligned.
    ///
    /// A failed measurement must not fail the audit: falling back to the
    /// configured lookback keeps the run useful and the report still states the
    /// range it used.
    async fn effective(self) -> Self {
        if !self.align_window {
            return self;
        }
        let tip = match self.anchor_tip {
            Some(block) => block,
            None => match self.chain.tip().await {
                Ok(tip) => tip,
                Err(e) => {
                    tracing::warn!("cannot align window, keeping the configured lookback: {e}");
                    return self;
                }
            },
        };
        let cached = self.clock.lock().ok().and_then(|clock| {
            clock
                .iter()
                .find(|(measured_tip, _)| *measured_tip == tip)
                .map(|(_, seconds)| *seconds)
        });
        let seconds = match cached {
            Some(seconds) => seconds,
            None => {
                let seconds = match crate::capture::average_block_seconds(&*self.chain, tip).await {
                    Ok(seconds) => seconds,
                    Err(e) => {
                        tracing::warn!(
                            "cannot measure block time, keeping the configured lookback: {e}"
                        );
                        return self;
                    }
                };
                if let Ok(mut clock) = self.clock.lock() {
                    clock.retain(|(measured_tip, _)| *measured_tip != tip);
                    clock.insert(0, (tip, seconds));
                    clock.truncate(MEASUREMENT_CACHE);
                }
                seconds
            }
        };
        let blocks = crate::capture::blocks_for_window(self.window_minutes, seconds);
        tracing::debug!(
            window_minutes = self.window_minutes,
            avg_block_s = seconds,
            lookback_blocks = %blocks,
            configured_lookback_blocks = self.lookback_blocks,
            "aligned the searched range to the measured block time"
        );
        self.with_lookback_blocks(blocks)
            .with_measured_block_seconds(seconds)
    }

    fn with_measured_block_seconds(mut self, seconds: u64) -> Self {
        self.measured_block_seconds = Some(seconds);
        self
    }

    /// Run one reconciliation pass at `now` (unix seconds).
    pub async fn report(
        &self,
        ledger: &(dyn LedgerSource + Send + Sync),
        now: i64,
    ) -> Result<Report> {
        self.clone().effective().await.pass(ledger, now).await
    }

    /// Evaluate exactly one ledger record against the chain, at the current tip
    /// (or the configured anchor).
    ///
    /// `/api/v1/reconcile` answers a whole-window question; this answers the
    /// narrower one an operator asks holding a support ticket — "what happened
    /// to this transaction?" — without paying for a window pass that also scans
    /// for orphans and re-evaluates every other record. The duplicate-claim
    /// rule is a cross-record rule, so a single record never reports `unknown`
    /// for a hash another record also claims: a windowed report stays
    /// authoritative for that verdict.
    pub async fn record(
        &self,
        record: &crate::model::LedgerRecord,
        now: i64,
    ) -> Result<(u64, ReconciledRecord)> {
        let tip = match self.anchor_tip {
            Some(block) => block,
            None => self.chain.tip().await?,
        };
        Ok((tip, self.evaluate(record, tip, now).await))
    }

    async fn pass(&self, ledger: &(dyn LedgerSource + Send + Sync), now: i64) -> Result<Report> {
        let tip = match self.anchor_tip {
            Some(block) => block,
            None => self.chain.tip().await?,
        };
        let mut records = ledger.fetch().await?;
        records.truncate(self.max_records);

        let mut summary = Summary::default();
        let mut findings = Vec::with_capacity(records.len());
        let mut consumed: HashSet<String> = HashSet::new();
        let mut claimed: HashMap<String, String> = HashMap::new();

        for record in &records {
            let mut finding = self.evaluate(record, tip, now).await;
            if let Some(hash) = finding.tx_hash.clone() {
                let key = hash.to_ascii_lowercase();
                if let Some(first_id) = claimed.get(&key) {
                    finding.outcome = Outcome::Unknown;
                    finding.reason = format!(
                        "transaction hash already claimed by record {first_id}; one transfer cannot pay two invoices"
                    );
                    summary.duplicates += 1;
                } else {
                    claimed.insert(key.clone(), finding.id.clone());
                }
                consumed.insert(key);
            }
            summary.record(finding.outcome);
            findings.push(finding);
        }

        let orphans = self.orphans(tip, &consumed).await?;
        summary.orphans = orphans.len();

        Ok(Report {
            mode: self.mode.clone(),
            chain_id: self.chain.chain_id(),
            receiver: self.receiver.clone(),
            generated_at: format_unix(now),
            window_minutes: self.window_minutes,
            chain_tip: tip,
            anchored: self.anchor_tip.is_some(),
            search: SearchWindow {
                from_block: tip.saturating_sub(self.lookback_blocks),
                to_block: tip,
                lookback_blocks: self.lookback_blocks,
                block_seconds: self.measured_block_seconds,
            },
            summary,
            records: findings,
            orphans,
        })
    }

    async fn evaluate(
        &self,
        record: &crate::model::LedgerRecord,
        tip: u64,
        now: i64,
    ) -> ReconciledRecord {
        let mut finding = ReconciledRecord {
            id: record.id.clone(),
            pay_to: record.pay_to.clone(),
            payer: record.payer.clone(),
            amount_atomic: record.amount_atomic.clone(),
            outcome: Outcome::Unknown,
            reason: String::new(),
            tx_hash: None,
            block_number: None,
            confirmations: None,
            recovered_by: None,
        };

        let expected = match record.amount() {
            Ok(value) if value > 0 => value,
            Ok(_) => {
                finding.reason = "amount must be greater than zero".into();
                return finding;
            }
            Err(e) => {
                finding.reason = e.to_string();
                return finding;
            }
        };

        if !record.pay_to.eq_ignore_ascii_case(&self.receiver) {
            finding.reason = format!(
                "pay_to {} does not match the configured receiver {}",
                record.pay_to, self.receiver
            );
            return finding;
        }

        let observed = match record.observed_unix() {
            Ok(value) => value,
            Err(e) => {
                finding.reason = e.to_string();
                return finding;
            }
        };
        // Mock fixtures are frozen recordings, so their timestamps are old by
        // design; only live runs enforce the freshness window.
        let window_start = now - i64::try_from(self.window_minutes).unwrap_or(0) * 60;
        if self.mode != "mock" && observed < window_start {
            finding.reason = format!(
                "record is older than the {} minute window",
                self.window_minutes
            );
            return finding;
        }

        match record.tx_hash.as_deref().map(str::trim) {
            Some(hash) if !hash.is_empty() => {
                self.evaluate_with_hash(&mut finding, hash, expected, tip)
                    .await
            }
            _ => {
                self.evaluate_by_search(
                    &mut finding,
                    record.payer.as_deref(),
                    expected,
                    tip,
                    observed,
                )
                .await
            }
        }

        finding
    }

    async fn evaluate_with_hash(
        &self,
        finding: &mut ReconciledRecord,
        hash: &str,
        expected: u128,
        tip: u64,
    ) {
        if !is_tx_hash(hash) {
            finding.reason = format!("{hash} is not a 0x-prefixed 32-byte transaction hash");
            return;
        }
        finding.tx_hash = Some(hash.to_ascii_lowercase());
        let receipt = match self.chain.receipt(hash).await {
            Ok(Some(receipt)) => receipt,
            Ok(None) => {
                finding.outcome = Outcome::Pending;
                finding.reason =
                    "no receipt yet; the gateway may have submitted it very recently".into();
                return;
            }
            Err(e) => {
                finding.reason = e.to_string();
                return;
            }
        };
        finding.block_number = Some(receipt.block_number);
        if !receipt.status {
            finding.outcome = Outcome::Failed;
            finding.reason = "transaction reverted on chain".into();
            return;
        }
        self.judge_receipt(finding, &receipt, expected, tip);
    }

    /// Shared receipt judgement used by both the hash path and the search path.
    fn judge_receipt(
        &self,
        finding: &mut ReconciledRecord,
        receipt: &Receipt,
        expected: u128,
        tip: u64,
    ) {
        let payer = finding.payer.as_deref();
        let to_receiver: Vec<&Transfer> = receipt
            .logs
            .iter()
            .filter(|t| t.to.eq_ignore_ascii_case(&self.receiver))
            .filter(|t| match payer {
                Some(payer) => t.from.eq_ignore_ascii_case(payer),
                None => true,
            })
            .collect();

        let best = to_receiver
            .iter()
            .find(|t| t.amount_atomic >= expected)
            .copied();

        match best {
            Some(transfer) => {
                let confirmations = confirmations(tip, receipt.block_number);
                finding.tx_hash = Some(transfer.tx_hash.to_ascii_lowercase());
                finding.block_number = Some(receipt.block_number);
                finding.confirmations = Some(confirmations);
                if confirmations < self.min_confirmations {
                    finding.outcome = Outcome::Pending;
                    finding.reason = format!(
                        "{confirmations} of {} required confirmations",
                        self.min_confirmations
                    );
                } else {
                    finding.outcome = Outcome::Matched;
                    finding.reason = format!(
                        "transfer of {} atomic units confirmed at block {}",
                        transfer.amount_atomic, receipt.block_number
                    );
                }
            }
            None => {
                finding.outcome = Outcome::Mismatched;
                finding.reason = match to_receiver.first() {
                    Some(transfer) => format!(
                        "receipt settled {} atomic units, {} were expected",
                        transfer.amount_atomic, expected
                    ),
                    None => "receipt contains no Transfer to the configured receiver".into(),
                };
            }
        }
    }

    async fn evaluate_by_search(
        &self,
        finding: &mut ReconciledRecord,
        payer: Option<&str>,
        expected: u128,
        tip: u64,
        observed: i64,
    ) {
        let from_block = tip.saturating_sub(self.lookback_blocks);
        let query = TransferQuery {
            token: self.token.clone(),
            from: payer.map(|p| p.to_string()),
            to: self.receiver.clone(),
            from_block,
            to_block: tip,
            limit: 200,
        };
        let transfers = match self.chain.find_transfers(query).await {
            Ok(list) => list,
            Err(e) => {
                finding.reason = e.to_string();
                return;
            }
        };

        // Only transfers at or after the record was observed can belong to it,
        // and only those that would satisfy the amount.
        let candidates: Vec<&Transfer> = transfers
            .iter()
            .filter(|t| t.amount_atomic >= expected && t.block_number >= from_block)
            .collect();

        match candidates.len() {
            0 => {
                finding.outcome = Outcome::Pending;
                finding.reason = format!(
                    "no Transfer to the receiver at or above {expected} atomic units in the last {} blocks ({} at {})",
                    self.lookback_blocks,
                    format_unix(observed),
                    observed
                );
            }
            1 => {
                let transfer = candidates[0];
                let confirmations = confirmations(tip, transfer.block_number);
                finding.tx_hash = Some(transfer.tx_hash.to_ascii_lowercase());
                finding.block_number = Some(transfer.block_number);
                finding.confirmations = Some(confirmations);
                finding.recovered_by = Some("log_search".into());
                if confirmations < self.min_confirmations {
                    finding.outcome = Outcome::Pending;
                    finding.reason = format!(
                        "matched by log search but has {confirmations} of {} confirmations",
                        self.min_confirmations
                    );
                } else {
                    finding.outcome = Outcome::Matched;
                    finding.reason =
                        "matched by log search (no tx hash in the ledger record)".into();
                }
            }
            n => {
                finding.outcome = Outcome::Pending;
                finding.reason = format!(
                    "{n} candidate Transfers satisfy the amount; add a tx hash or narrow the window"
                );
            }
        }
    }

    async fn orphans(&self, tip: u64, consumed: &HashSet<String>) -> Result<Vec<OrphanTransfer>> {
        let from_block = tip.saturating_sub(self.lookback_blocks);
        let transfers = self
            .chain
            .find_transfers(TransferQuery {
                token: self.token.clone(),
                from: None,
                to: self.receiver.clone(),
                from_block,
                to_block: tip,
                limit: MAX_ORPHANS * 2,
            })
            .await?;
        Ok(transfers
            .into_iter()
            .filter(|t| !consumed.contains(&t.tx_hash.to_ascii_lowercase()))
            .take(MAX_ORPHANS)
            .map(|t| OrphanTransfer {
                tx_hash: t.tx_hash,
                block_number: t.block_number,
                from: t.from,
                to: t.to,
                amount_atomic: t.amount_atomic.to_string(),
            })
            .collect())
    }
}

fn confirmations(tip: u64, block: u64) -> u64 {
    tip.saturating_sub(block) + 1
}

fn is_tx_hash(value: &str) -> bool {
    let value = value.trim();
    value.len() == 66
        && value.starts_with("0x")
        && value[2..].bytes().all(|b| b.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crate::chain::Receipt;
    use crate::ledger::MemoryLedger;
    use crate::mock::{in_memory, transfer};
    use crate::model::{parse_rfc3339, LedgerRecord};

    use super::*;

    const RECEIVER: &str = "0x2222222222222222222222222222222222222222";
    const PAYER: &str = "0x1111111111111111111111111111111111111111";
    const TOKEN: &str = "0x3333333333333333333333333333333333333333";
    const HASH: &str = "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    fn record(id: &str, amount: &str, hash: Option<&str>) -> LedgerRecord {
        LedgerRecord {
            id: id.to_string(),
            pay_to: RECEIVER.to_string(),
            payer: Some(PAYER.to_string()),
            amount_atomic: amount.to_string(),
            tx_hash: hash.map(str::to_string),
            observed_at: "2026-11-22T10:00:00Z".to_string(),
            network: None,
        }
    }

    fn block_on<F: std::future::Future>(fut: F) -> F::Output {
        match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(rt) => rt.block_on(fut),
            Err(e) => panic!("runtime: {e}"),
        }
    }

    fn reconciler(chain: crate::mock::FixtureChain, min_confirmations: u64) -> Reconciler {
        Reconciler::new(
            Arc::new(chain),
            RECEIVER,
            TOKEN,
            60,
            min_confirmations,
            2_000,
            500,
            "mock",
        )
    }

    fn now() -> i64 {
        parse_rfc3339("2026-11-22T10:05:00Z").unwrap_or(0)
    }

    #[test]
    fn a_confirmed_transfer_is_matched() {
        let chain = in_memory(
            2368,
            1_000,
            vec![(
                HASH.to_string(),
                Receipt {
                    status: true,
                    block_number: 990,
                    logs: vec![transfer(HASH, 990, PAYER, RECEIVER, 10_000)],
                },
            )],
            vec![transfer(HASH, 990, PAYER, RECEIVER, 10_000)],
        );
        let ledger = MemoryLedger::new(vec![record("inv_1", "10000", Some(HASH))]);
        let report = block_on(reconciler(chain, 1).report(&ledger, now()));
        let report = match report {
            Ok(r) => r,
            Err(e) => panic!("reconcile failed: {e}"),
        };
        assert_eq!(report.summary.matched, 1);
        assert_eq!(report.records[0].outcome, Outcome::Matched);
        assert_eq!(report.records[0].confirmations, Some(11));
        assert_eq!(report.orphans.len(), 0);
    }

    /// A saved report must say which address the verdicts were relative to.
    /// "pay_to does not match the configured receiver" only means something if
    /// the report names that receiver, and a mixed-case configuration value
    /// must be reported in the same normalized form the verdicts used.
    #[test]
    fn report_names_the_normalized_receiver() {
        let report = match block_on(
            reconciler(in_memory(2368, 1_000, vec![], vec![]), 1)
                .report(&MemoryLedger::new(vec![]), now()),
        ) {
            Ok(report) => report,
            Err(e) => panic!("reconcile failed: {e}"),
        };
        assert_eq!(report.receiver, RECEIVER);

        let mixed_case = Reconciler::new(
            Arc::new(in_memory(2368, 1_000, vec![], vec![])),
            format!("  {}  ", RECEIVER.to_ascii_uppercase()),
            TOKEN,
            60,
            1,
            2_000,
            500,
            "mock",
        );
        let report = match block_on(mixed_case.report(&MemoryLedger::new(vec![]), now())) {
            Ok(report) => report,
            Err(e) => panic!("reconcile failed: {e}"),
        };
        assert_eq!(report.receiver, RECEIVER);
    }

    #[test]
    fn a_reverted_transaction_is_failed() {
        let chain = in_memory(
            2368,
            1_000,
            vec![(
                HASH.to_string(),
                Receipt {
                    status: false,
                    block_number: 995,
                    logs: vec![],
                },
            )],
            vec![],
        );
        let ledger = MemoryLedger::new(vec![record("inv_2", "10000", Some(HASH))]);
        let report = block_on(reconciler(chain, 1).report(&ledger, now()));
        let report = match report {
            Ok(r) => r,
            Err(e) => panic!("reconcile failed: {e}"),
        };
        assert_eq!(report.records[0].outcome, Outcome::Failed);
        assert_eq!(report.summary.differences(), 1);
    }

    #[test]
    fn too_few_confirmations_stay_pending() {
        let chain = in_memory(
            2368,
            1_000,
            vec![(
                HASH.to_string(),
                Receipt {
                    status: true,
                    block_number: 1_000,
                    logs: vec![transfer(HASH, 1_000, PAYER, RECEIVER, 10_000)],
                },
            )],
            vec![],
        );
        let ledger = MemoryLedger::new(vec![record("inv_3", "10000", Some(HASH))]);
        let report = block_on(reconciler(chain, 3).report(&ledger, now()));
        let report = match report {
            Ok(r) => r,
            Err(e) => panic!("reconcile failed: {e}"),
        };
        assert_eq!(report.records[0].outcome, Outcome::Pending);
        assert_eq!(report.records[0].confirmations, Some(1));
    }

    #[test]
    fn a_short_payment_is_mismatched() {
        let chain = in_memory(
            2368,
            1_000,
            vec![(
                HASH.to_string(),
                Receipt {
                    status: true,
                    block_number: 990,
                    logs: vec![transfer(HASH, 990, PAYER, RECEIVER, 1)],
                },
            )],
            vec![],
        );
        let ledger = MemoryLedger::new(vec![record("inv_4", "10000", Some(HASH))]);
        let report = block_on(reconciler(chain, 1).report(&ledger, now()));
        let report = match report {
            Ok(r) => r,
            Err(e) => panic!("reconcile failed: {e}"),
        };
        assert_eq!(report.records[0].outcome, Outcome::Mismatched);
        assert!(report.records[0].reason.contains("receipt settled"));
    }

    #[test]
    fn a_missing_hash_can_be_recovered_by_log_search() {
        let chain = in_memory(
            2368,
            1_000,
            vec![],
            vec![transfer(HASH, 995, PAYER, RECEIVER, 10_000)],
        );
        let ledger = MemoryLedger::new(vec![record("inv_5", "10000", None)]);
        let report = block_on(reconciler(chain, 1).report(&ledger, now()));
        let report = match report {
            Ok(r) => r,
            Err(e) => panic!("reconcile failed: {e}"),
        };
        assert_eq!(report.records[0].outcome, Outcome::Matched);
        assert_eq!(
            report.records[0].recovered_by.as_deref(),
            Some("log_search")
        );
        assert_eq!(report.orphans.len(), 0);
    }

    #[test]
    fn an_unclaimed_transfer_is_reported_as_orphan() {
        let other = "0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        let chain = in_memory(
            2368,
            1_000,
            vec![],
            vec![transfer(other, 990, PAYER, RECEIVER, 5_000)],
        );
        let ledger = MemoryLedger::new(vec![]);
        let report = block_on(reconciler(chain, 1).report(&ledger, now()));
        let report = match report {
            Ok(r) => r,
            Err(e) => panic!("reconcile failed: {e}"),
        };
        assert_eq!(report.orphans.len(), 1);
        assert_eq!(report.orphans[0].tx_hash, other);
    }

    #[test]
    fn malformed_input_is_unknown_not_a_panic() {
        let chain = in_memory(2368, 1_000, vec![], vec![]);
        let mut bad_hash = record("inv_6", "10000", Some("0xdeadbeef"));
        bad_hash.pay_to = "0x4444444444444444444444444444444444444444".to_string();
        let ledger = MemoryLedger::new(vec![record("inv_7", "-5", None), bad_hash]);
        let report = block_on(reconciler(chain, 1).report(&ledger, now()));
        let report = match report {
            Ok(r) => r,
            Err(e) => panic!("reconcile failed: {e}"),
        };
        assert_eq!(report.summary.unknown, 2);
        assert!(report.records.iter().all(|r| !r.reason.is_empty()));
    }

    #[test]
    fn one_transfer_cannot_claim_two_invoices() {
        let chain = in_memory(
            2368,
            1_000,
            vec![(
                HASH.to_string(),
                Receipt {
                    status: true,
                    block_number: 990,
                    logs: vec![transfer(HASH, 990, PAYER, RECEIVER, 10_000)],
                },
            )],
            vec![],
        );
        let ledger = MemoryLedger::new(vec![
            record("inv_a", "10000", Some(HASH)),
            record("inv_b", "10000", Some(HASH)),
        ]);
        let report = block_on(reconciler(chain, 1).report(&ledger, now()));
        let report = match report {
            Ok(r) => r,
            Err(e) => panic!("reconcile failed: {e}"),
        };
        assert_eq!(report.records[0].outcome, Outcome::Matched);
        assert_eq!(report.records[1].outcome, Outcome::Unknown);
        assert_eq!(report.summary.duplicates, 1);
        assert!(report.records[1].reason.contains("already claimed"));
    }

    #[test]
    fn live_mode_rejects_records_outside_the_window() {
        let chain = in_memory(2368, 1_000, vec![], vec![]);
        let reconciler =
            Reconciler::new(Arc::new(chain), RECEIVER, TOKEN, 60, 1, 2_000, 500, "live");
        let ledger = MemoryLedger::new(vec![record("inv_old", "10000", None)]);
        let report = block_on(reconciler.report(&ledger, now() + 10 * 3_600));
        let report = match report {
            Ok(r) => r,
            Err(e) => panic!("reconcile failed: {e}"),
        };
        assert_eq!(report.records[0].outcome, Outcome::Unknown);
        assert!(report.records[0].reason.contains("window"));
    }

    #[test]
    fn alignment_derives_the_range_from_the_fixture_clock() {
        let old = "0xcccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";
        let chain = in_memory(
            2368,
            1_000,
            vec![],
            vec![transfer(old, 100, PAYER, RECEIVER, 5_000)],
        );
        let ledger = MemoryLedger::new(vec![]);
        // 60 minutes at the fixture's 12 s/block is 300 blocks: block 100 is
        // outside it, so the transfer is not an orphan of this window.
        let aligned = reconciler(chain, 1).with_alignment(true);
        let report = block_on(aligned.report(&ledger, now()));
        let report = match report {
            Ok(r) => r,
            Err(e) => panic!("reconcile failed: {e}"),
        };
        assert_eq!(report.search.lookback_blocks, 300);
        assert_eq!(report.search.from_block, 700);
        assert_eq!(report.search.block_seconds, Some(12));
        assert_eq!(report.orphans.len(), 0);
    }

    #[test]
    fn an_anchor_replays_a_past_window_deterministically() {
        let old = "0xcccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";
        let future = "0xdddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd";
        let chain = in_memory(
            2368,
            1_000,
            vec![],
            vec![
                transfer(old, 100, PAYER, RECEIVER, 5_000),
                transfer(future, 995, PAYER, RECEIVER, 5_000),
            ],
        );
        let ledger = MemoryLedger::new(vec![]);
        let anchored = reconciler(chain, 1)
            .with_alignment(true)
            .with_anchor_tip(900);
        let report = match block_on(anchored.report(&ledger, now())) {
            Ok(r) => r,
            Err(e) => panic!("reconcile failed: {e}"),
        };
        // As of block 900 the aligned 60-minute window is 600..900, so the
        // transfer at 995 is in the future and must not appear as an orphan.
        assert_eq!(report.chain_tip, 900);
        assert_eq!(report.search.to_block, 900);
        assert_eq!(report.search.from_block, 600);
        assert_eq!(report.search.block_seconds, Some(12));
        assert_eq!(report.orphans.len(), 0);
    }

    /// Fixture chain that counts block-timestamp reads, so a test can assert how
    /// many measurements a sequence of reports paid for.
    struct Counting {
        inner: crate::mock::FixtureChain,
        block_reads: std::sync::atomic::AtomicU64,
    }

    impl crate::chain::ChainClient for Counting {
        fn chain_id(&self) -> u64 {
            self.inner.chain_id()
        }
        fn tip<'a>(&'a self) -> crate::chain::BoxFuture<'a, Result<u64>> {
            self.inner.tip()
        }
        fn receipt<'a>(
            &'a self,
            tx_hash: &'a str,
        ) -> crate::chain::BoxFuture<'a, Result<Option<crate::chain::Receipt>>> {
            self.inner.receipt(tx_hash)
        }
        fn find_transfers<'a>(
            &'a self,
            query: crate::chain::TransferQuery,
        ) -> crate::chain::BoxFuture<'a, Result<Vec<crate::chain::Transfer>>> {
            self.inner.find_transfers(query)
        }
        fn token_decimals<'a>(&'a self) -> crate::chain::BoxFuture<'a, Result<u8>> {
            self.inner.token_decimals()
        }
        fn token_symbol<'a>(&'a self) -> crate::chain::BoxFuture<'a, Result<String>> {
            self.inner.token_symbol()
        }
        fn block_timestamp<'a>(&'a self, number: u64) -> crate::chain::BoxFuture<'a, Result<u64>> {
            self.block_reads
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            self.inner.block_timestamp(number)
        }
    }

    impl Counting {
        fn new(tip: u64) -> std::sync::Arc<Self> {
            std::sync::Arc::new(Self {
                inner: in_memory(2368, tip, vec![], vec![]),
                block_reads: std::sync::atomic::AtomicU64::new(0),
            })
        }

        fn reads(&self) -> u64 {
            self.block_reads.load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    #[test]
    fn the_measurement_is_reused_until_the_chain_advances() {
        let chain = Counting::new(1_000);
        let probe = std::sync::Arc::clone(&chain);
        let aligned =
            Reconciler::new(chain, RECEIVER, TOKEN, 60, 1, 2_000, 500, "mock").with_alignment(true);
        let ledger = MemoryLedger::new(vec![]);
        for _ in 0..2 {
            let report = match block_on(aligned.report(&ledger, now())) {
                Ok(r) => r,
                Err(e) => panic!("reconcile failed: {e}"),
            };
            assert_eq!(report.search.block_seconds, Some(12));
            assert_eq!(report.search.lookback_blocks, 300);
        }
        // One measurement = two block reads (tip and tip-1000), reused across
        // passes while the tip is unchanged. A served instance must not spend
        // two RPC calls per HTTP request on the same tip.
        assert_eq!(probe.reads(), 2);
    }

    #[test]
    fn alternating_live_and_anchored_reads_keep_both_measurements() {
        let chain = Counting::new(1_000);
        let probe = std::sync::Arc::clone(&chain);
        let live =
            Reconciler::new(chain, RECEIVER, TOKEN, 60, 1, 2_000, 500, "mock").with_alignment(true);
        let ledger = MemoryLedger::new(vec![]);
        for _ in 0..2 {
            for anchor in [Some(900), None] {
                let reconciler = match anchor {
                    Some(block) => live.clone().with_anchor_tip(block),
                    None => live.clone(),
                };
                let report = match block_on(reconciler.report(&ledger, now())) {
                    Ok(r) => r,
                    Err(e) => panic!("reconcile failed: {e}"),
                };
                assert_eq!(report.search.block_seconds, Some(12));
            }
        }
        // Two distinct tips (live 1000, anchor 900), each measured once: a
        // dashboard alternating live and replayed views must not re-measure on
        // every pair of requests.
        assert_eq!(probe.reads(), 4);
    }

    #[test]
    fn an_aligned_lookback_excludes_transfers_outside_the_window() {
        let old = "0xcccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";
        let recent = "0xdddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd";
        let chain = in_memory(
            2368,
            1_000,
            vec![],
            vec![
                transfer(old, 100, PAYER, RECEIVER, 5_000),
                transfer(recent, 995, PAYER, RECEIVER, 5_000),
            ],
        );
        let ledger = MemoryLedger::new(vec![]);
        // 2000 blocks back reaches block 100: both transfers are orphans.
        let wide = block_on(reconciler(chain.clone(), 1).report(&ledger, now()));
        let wide = match wide {
            Ok(r) => r,
            Err(e) => panic!("reconcile failed: {e}"),
        };
        assert_eq!(wide.orphans.len(), 2);

        // A window aligned to 10 blocks only sees the recent transfer.
        let aligned = reconciler(chain, 1).with_lookback_blocks(10);
        let narrow = block_on(aligned.report(&ledger, now()));
        let narrow = match narrow {
            Ok(r) => r,
            Err(e) => panic!("reconcile failed: {e}"),
        };
        assert_eq!(narrow.orphans.len(), 1);
        assert_eq!(narrow.orphans[0].tx_hash, recent);
    }
}
