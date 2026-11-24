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
        "mode={} chain_id={} tip={}{} window={}m search={}..{} ({} blocks{}) generated_at={}",
        report.mode,
        report.chain_id,
        report.chain_tip,
        if report.anchored { " (anchored)" } else { "" },
        report.window_minutes,
        report.search.from_block,
        report.search.to_block,
        report.search.lookback_blocks,
        match report.search.block_seconds {
            Some(seconds) => format!(", {seconds}s/block"),
            None => String::new(),
        },
        report.generated_at
    );
    let s = &report.summary;
    let _ = writeln!(
        out,
        "total={} matched={} pending={} mismatched={} failed={} unknown={} orphans={} duplicates={}",
        s.total, s.matched, s.pending, s.mismatched, s.failed, s.unknown, s.orphans, s.duplicates
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
    let _ = writeln!(
        out,
        "> `{}` chain {} · tip {}{} · audit window {}m · searched blocks {}..{} ({} blocks{})",
        report.mode,
        report.chain_id,
        report.chain_tip,
        if report.anchored { " (anchored)" } else { "" },
        report.window_minutes,
        report.search.from_block,
        report.search.to_block,
        report.search.lookback_blocks,
        match report.search.block_seconds {
            Some(seconds) => format!(", {seconds}s/block"),
            None => String::new(),
        }
    );
    let _ = writeln!(out);
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

/// The report as RFC 4180 CSV: one row per ledger record and one row per
/// orphan transfer, so a reconciliation can be opened directly in a
/// spreadsheet. Nullable fields are left empty rather than invented.
pub fn to_csv(report: &Report) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "kind,id,outcome,pay_to,payer,amount_atomic,tx_hash,block_number,confirmations,recovered_by,reason"
    );
    for record in &report.records {
        write_csv_row(
            &mut out,
            &[
                "payment".to_owned(),
                record.id.clone(),
                label(record.outcome).to_owned(),
                record.pay_to.clone(),
                record.payer.clone().unwrap_or_default(),
                record.amount_atomic.clone(),
                record.tx_hash.clone().unwrap_or_default(),
                record
                    .block_number
                    .map(|number| number.to_string())
                    .unwrap_or_default(),
                record
                    .confirmations
                    .map(|count| count.to_string())
                    .unwrap_or_default(),
                record.recovered_by.clone().unwrap_or_default(),
                record.reason.clone(),
            ],
        );
    }
    for orphan in &report.orphans {
        write_csv_row(
            &mut out,
            &[
                "orphan".to_owned(),
                String::new(),
                "ORPHAN".to_owned(),
                orphan.to.clone(),
                orphan.from.clone(),
                orphan.amount_atomic.clone(),
                orphan.tx_hash.clone(),
                orphan.block_number.to_string(),
                String::new(),
                String::new(),
                "no ledger record backed this transfer".to_owned(),
            ],
        );
    }
    out
}

fn write_csv_row(out: &mut String, fields: &[String]) {
    let escaped: Vec<String> = fields.iter().map(|field| csv_field(field)).collect();
    let _ = writeln!(out, "{}", escaped.join(","));
}

/// Quotes a field only when RFC 4180 requires it, doubling embedded quotes.
fn csv_field(value: &str) -> String {
    if value.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_owned()
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
            anchored: false,
            search: crate::model::SearchWindow {
                from_block: 0,
                to_block: 1000,
                lookback_blocks: 2000,
                block_seconds: None,
            },
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
        assert_eq!(value["search"]["to_block"], 1000);
        assert_eq!(value["search"]["lookback_blocks"], 2000);
    }

    #[test]
    fn text_and_markdown_name_the_mode_and_the_searched_range() {
        let report = sample();
        let text = to_text(&report);
        assert!(text.contains("mode=mock"));
        assert!(text.contains("search=0..1000"));
        let markdown = to_markdown(&report);
        assert!(markdown.contains("MISMATCH"));
        assert!(markdown.contains("searched blocks 0..1000"));
    }

    #[test]
    fn text_states_the_measured_block_time_when_aligned() {
        let mut report = sample();
        report.search.block_seconds = Some(886);
        assert!(to_text(&report).contains("886s/block"));
        assert!(!to_text(&sample()).contains("s/block"));
    }

    #[test]
    fn the_three_views_agree_on_the_searched_range() {
        let mut report = sample();
        report.search.from_block = 700;
        report.search.lookback_blocks = 300;
        report.search.block_seconds = Some(886);
        let text = to_text(&report);
        let markdown = to_markdown(&report);
        let json = to_json(&report).unwrap_or_default();
        let expected = "700..1000 (300 blocks";
        assert!(
            text.contains(&format!("search={expected}, 886s/block)")),
            "{text}"
        );
        assert!(
            markdown.contains(&format!("searched blocks {expected}, 886s/block)")),
            "{markdown}"
        );
        assert!(json.contains(r#""from_block": 700"#), "{json}");
        assert!(json.contains(r#""lookback_blocks": 300"#), "{json}");
        assert!(json.contains(r#""block_seconds": 886"#), "{json}");
    }

    #[test]
    fn an_anchored_report_says_the_tip_is_an_anchor() {
        let mut report = sample();
        report.chain_tip = 900;
        report.anchored = true;
        assert!(to_text(&report).contains("tip=900 (anchored)"));
        assert!(to_markdown(&report).contains("tip 900 (anchored)"));
        assert!(to_json(&report)
            .unwrap_or_default()
            .contains(r#""anchored": true"#));
        assert!(!to_text(&sample()).contains("(anchored)"));
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

    #[test]
    fn csv_has_one_row_per_record_and_orphan() {
        let csv = to_csv(&sample());
        let lines: Vec<&str> = csv.lines().collect();
        assert_eq!(
            lines[0],
            "kind,id,outcome,pay_to,payer,amount_atomic,tx_hash,block_number,confirmations,recovered_by,reason"
        );
        assert_eq!(lines.len(), 3, "one header, one payment, one orphan: {csv}");
        for line in &lines {
            assert_eq!(
                line.split(',').count(),
                11,
                "unaltered sample fields must not need quoting: {line}"
            );
        }
        assert!(lines[1].starts_with("payment,inv|1,MISMATCH,0x2222"));
        assert!(lines[2].starts_with("orphan,,ORPHAN,0x2222"));
    }

    #[test]
    fn csv_quotes_commas_quotes_and_newlines() {
        let mut report = sample();
        report.records[0].reason = "receipt, \"settled\"\nlate".into();
        let csv = to_csv(&report);
        assert!(
            csv.contains("\"receipt, \"\"settled\"\"\nlate\""),
            "RFC 4180 quoting is missing: {csv}"
        );
        // The quoted newline stays inside one field instead of splitting the row.
        assert_eq!(csv.matches("payment,").count(), 1);
        assert_eq!(csv.matches("orphan,").count(), 1);
    }
}
