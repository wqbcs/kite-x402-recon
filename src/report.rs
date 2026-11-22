//! Report rendering. Pure functions over [`Report`] so the CLI, the HTTP API and
//! the tests all share one shape.

use std::fmt::Write as _;

use crate::error::Result;
use crate::model::{Outcome, Report};

/// Pretty JSON, the canonical machine-readable form.
pub fn to_json(report: &Report) -> Result<String> {
    Ok(serde_json::to_string_pretty(report)?)
}

/// One line per finding, suitable for a terminal or a CI log.
pub fn to_text(report: &Report) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "mode={} chain_id={} tip={} window={}m generated_at={}",
        report.mode, report.chain_id, report.chain_tip, report.window_minutes, report.generated_at
    );
    let s = &report.summary;
    let _ = writeln!(
        out,
        "total={} matched={} pending={} mismatched={} failed={} unknown={} orphans={}",
        s.total, s.matched, s.pending, s.mismatched, s.failed, s.unknown, s.orphans
    );
    if report.mode == "mock" {
        let _ = writeln!(
            out,
            "note: mock mode reads fixtures/chain_mock.json; nothing here came from a chain"
        );
    }
    for record in &report.records {
        let _ = writeln!(
            out,
            "{:<10} {:<9} {} amount={} tx={} :: {}",
            label(record.outcome),
            record.id,
            record.pay_to,
            record.amount_atomic,
            record.tx_hash.as_deref().unwrap_or("-"),
            record.reason
        );
    }
    for orphan in &report.orphans {
        let _ = writeln!(
            out,
            "ORPHAN     block={} tx={} from={} to={} amount={}",
            orphan.block_number, orphan.tx_hash, orphan.from, orphan.to, orphan.amount_atomic
        );
    }
    out
}

/// Markdown table for copy-pasting into an issue or a submission record.
pub fn to_markdown(report: &Report) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "| outcome | id | amount | tx | reason |");
    let _ = writeln!(out, "| --- | --- | --- | --- | --- |");
    for record in &report.records {
        let _ = writeln!(
            out,
            "| {} | {} | {} | {} | {} |",
            label(record.outcome),
            record.id,
            record.amount_atomic,
            record.tx_hash.as_deref().unwrap_or("-"),
            record.reason.replace('|', "\\|")
        );
    }
    if !report.orphans.is_empty() {
        let _ = writeln!(out);
        let _ = writeln!(out, "| orphan tx | block | from | amount |");
        let _ = writeln!(out, "| --- | --- | --- | --- |");
        for orphan in &report.orphans {
            let _ = writeln!(
                out,
                "| {} | {} | {} | {} |",
                orphan.tx_hash, orphan.block_number, orphan.from, orphan.amount_atomic
            );
        }
    }
    out
}

fn label(outcome: Outcome) -> &'static str {
    match outcome {
        Outcome::Matched => "MATCHED",
        Outcome::Pending => "PENDING",
        Outcome::Mismatched => "MISMATCH",
        Outcome::Failed => "FAILED",
        Outcome::Unknown => "UNKNOWN",
    }
}

/// Exit code for CLI and CI use: 0 clean, 1 differences, 2 pending but no
/// differences when `strict` is off.
pub fn exit_code(report: &Report, strict: bool) -> i32 {
    if report.summary.differences() > 0 {
        1
    } else if strict && report.summary.pending > 0 {
        2
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use crate::model::{OrphanTransfer, ReconciledRecord, Summary};

    use super::*;

    fn sample() -> Report {
        Report {
            mode: "mock".into(),
            chain_id: 2368,
            generated_at: "2026-11-22T10:00:00Z".into(),
            window_minutes: 60,
            chain_tip: 1000,
            summary: Summary {
                total: 1,
                mismatched: 1,
                orphans: 1,
                ..Summary::default()
            },
            records: vec![ReconciledRecord {
                id: "inv|1".into(),
                pay_to: "0x2222222222222222222222222222222222222222".into(),
                payer: None,
                amount_atomic: "10000".into(),
                outcome: Outcome::Mismatched,
                reason: "receipt settled 1 atomic units".into(),
                tx_hash: None,
                block_number: None,
                confirmations: None,
                recovered_by: None,
            }],
            orphans: vec![OrphanTransfer {
                tx_hash: "0xbb".into(),
                block_number: 990,
                from: "0x1111111111111111111111111111111111111111".into(),
                to: "0x2222222222222222222222222222222222222222".into(),
                amount_atomic: "5000".into(),
            }],
        }
    }

    #[test]
    fn json_round_trips_through_value() {
        let report = sample();
        let json = to_json(&report).unwrap_or_default();
        let value: serde_json::Value = match serde_json::from_str(&json) {
            Ok(v) => v,
            Err(e) => panic!("{e}"),
        };
        assert_eq!(value["mode"], "mock");
        assert_eq!(value["records"][0]["outcome"], "mismatched");
    }

    #[test]
    fn text_and_markdown_name_the_mode() {
        let report = sample();
        assert!(to_text(&report).contains("mode=mock"));
        assert!(to_markdown(&report).contains("MISMATCH"));
    }

    #[test]
    fn exit_codes_distinguish_findings_from_pending() {
        let mut report = sample();
        assert_eq!(exit_code(&report, false), 1);
        report.summary.mismatched = 0;
        report.summary.pending = 1;
        assert_eq!(exit_code(&report, false), 0);
        assert_eq!(exit_code(&report, true), 2);
    }
}
