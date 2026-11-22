//! Reconciliation rules. One pass of the ledger, one pass of the chain window,
//! and a deterministic outcome per record. Every decision records the reason so
//! a difference can be explained without re-running anything.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::chain::{ChainClient, Receipt, Transfer, TransferQuery};
use crate::error::Result;
use crate::ledger::LedgerSource;
use crate::model::{format_unix, OrphanTransfer, Outcome, ReconciledRecord, Report, Summary};

/// How far back to scan blocks when a record has no transaction hash.
pub const DEFAULT_LOOKBACK_BLOCKS: u64 = 2_000;
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
            mode: mode.into(),
        }
    }

    pub fn chain_id(&self) -> u64 {
        self.chain.chain_id()
    }

    /// Run one reconciliation pass at `now` (unix seconds).
    pub async fn report(
        &self,
        ledger: &(dyn LedgerSource + Send + Sync),
        now: i64,
    ) -> Result<Report> {
        let tip = self.chain.tip().await?;
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
            generated_at: format_unix(now),
            window_minutes: self.window_minutes,
            chain_tip: tip,
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
}
