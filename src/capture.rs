//! Bootstrapping a ledger from the chain itself.
//!
//! A gateway is the usual source of ledger records, but when there is no
//! ledger yet — or when records were lost — the chain is the only surviving
//! witness. `recon capture` reads `Transfer` logs for the configured token
//! over a block range and writes them as a ledger file that `recon scan`
//! accepts unchanged.

use std::collections::HashSet;

use crate::chain::Transfer;
use crate::error::Result;
use crate::model::LedgerRecord;

/// Blocks needed to cover a wall-clock window at a measured block time.
///
/// `avg_block_seconds` must be at least 1; callers clamp a measured 0 or
/// sub-second average before calling. The result is rounded up: half a block
/// short would hide a transfer that settled right at the window edge.
pub fn blocks_for_window(window_minutes: u64, avg_block_seconds: u64) -> u64 {
    let seconds = window_minutes.saturating_mul(60);
    let per_block = avg_block_seconds.max(1);
    seconds.div_ceil(per_block).max(1)
}

/// Turn decoded transfers into ledger records.
///
/// `observed_at` is the capture time, not the payment time: `eth_getLogs` does
/// not expose a wall clock and block timestamps would cost one extra call per
/// block. The report therefore says when the evidence was read, and the
/// `tx_hash` is what ties the record back to its true settlement moment.
///
/// Record ids are the lowercased transaction hash. A transaction can emit
/// several `Transfer` logs (batch payouts), so later collisions get a
/// `#2`, `#3` … suffix instead of silently overwriting the first one.
pub fn ledger_from_transfers(transfers: &[Transfer], observed_at: &str) -> Vec<LedgerRecord> {
    let mut used: HashSet<String> = HashSet::with_capacity(transfers.len());
    let mut records = Vec::with_capacity(transfers.len());
    for transfer in transfers {
        let base = transfer.tx_hash.to_ascii_lowercase();
        let mut id = base.clone();
        let mut suffix = 1u32;
        while !used.insert(id.clone()) {
            suffix += 1;
            id = format!("{base}#{suffix}");
        }
        records.push(LedgerRecord {
            id,
            pay_to: transfer.to.clone(),
            payer: Some(transfer.from.clone()),
            amount_atomic: transfer.amount_atomic.to_string(),
            tx_hash: Some(transfer.tx_hash.clone()),
            observed_at: observed_at.to_string(),
            network: None,
        });
    }
    records
}

/// Render records as the JSONL format `ledger.kind = "file"` reads.
pub fn to_jsonl(records: &[LedgerRecord]) -> Result<String> {
    let mut out = String::new();
    for record in records {
        let line = serde_json::to_string(record)?;
        out.push_str(&line);
        out.push('\n');
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::{blocks_for_window, ledger_from_transfers, to_jsonl};
    use crate::chain::Transfer;

    const PAYER: &str = "0x4444444444444444444444444444444444444444";
    const PAY_TO: &str = "0x2222222222222222222222222222222222222222";
    const HASH: &str = "0x1111111111111111111111111111111111111111111111111111111111111111";

    fn transfer(hash: &str, amount: u128) -> Transfer {
        Transfer {
            tx_hash: hash.to_string(),
            block_number: 990,
            from: PAYER.to_string(),
            to: PAY_TO.to_string(),
            amount_atomic: amount,
        }
    }

    #[test]
    fn window_converts_to_blocks() {
        // 60 minutes at 12 s/block is 300 blocks; 61 minutes rounds up.
        assert_eq!(blocks_for_window(60, 12), 300);
        assert_eq!(blocks_for_window(61, 12), 305);
        // Degenerate averages must not divide by zero or return an empty range.
        assert_eq!(blocks_for_window(1, 0), 60);
        assert_eq!(blocks_for_window(0, 12), 1);
    }

    #[test]
    fn transfers_become_records() {
        let records = ledger_from_transfers(&[transfer(HASH, 10_000)], "2026-11-22T16:00:00Z");
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].id, HASH);
        assert_eq!(records[0].amount_atomic, "10000");
        assert_eq!(records[0].pay_to, PAY_TO);
        assert_eq!(records[0].payer.as_deref(), Some(PAYER));
        assert_eq!(records[0].observed_at, "2026-11-22T16:00:00Z");
    }

    #[test]
    fn ids_stay_unique_for_batch_transfers() {
        let records = ledger_from_transfers(
            &[transfer(HASH, 10_000), transfer(HASH, 20_000)],
            "2026-11-22T16:00:00Z",
        );
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].id, HASH);
        assert_eq!(records[1].id, format!("{HASH}#2"));
        assert_ne!(records[0].id, records[1].id);
    }

    #[test]
    fn jsonl_round_trips_through_the_file_ledger_shape() {
        let records = ledger_from_transfers(&[transfer(HASH, 10_000)], "2026-11-22T16:00:00Z");
        let text = match to_jsonl(&records) {
            Ok(text) => text,
            Err(e) => panic!("render failed: {e}"),
        };
        assert_eq!(text.lines().count(), 1);
        let parsed: crate::model::LedgerRecord = match serde_json::from_str(text.trim()) {
            Ok(record) => record,
            Err(e) => panic!("captured line is not a ledger record: {e}"),
        };
        assert_eq!(parsed.id, HASH);
    }
}
