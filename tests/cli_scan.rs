//! End-to-end checks that run the real binary against the bundled fixtures.
//! They are deliberately black-box: if the CLI contract changes, these fail.

use std::process::Command;

fn run(args: &[&str]) -> std::process::Output {
    match Command::new(env!("CARGO_BIN_EXE_recon"))
        .args(args)
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
    {
        Ok(output) => output,
        Err(e) => panic!("cannot run recon: {e}"),
    }
}

fn json_of(output: &std::process::Output) -> serde_json::Value {
    let stdout = String::from_utf8_lossy(&output.stdout);
    match serde_json::from_str::<serde_json::Value>(stdout.trim()) {
        Ok(value) => value,
        Err(e) => panic!("stdout is not JSON ({e}):\n{stdout}"),
    }
}

#[test]
fn scan_reports_every_fixture_outcome() {
    let output = run(&["--config", "recon.example.toml", "scan", "--format", "json"]);
    assert!(
        !output.status.success(),
        "fixture run contains differences, so the exit code must be 1"
    );
    let report = json_of(&output);
    assert_eq!(report["mode"], "mock");
    assert_eq!(report["chain_id"], 2368);
    assert_eq!(report["chain_tip"], 1000);
    assert_eq!(report["anchored"], false);
    let summary = &report["summary"];
    assert_eq!(summary["total"], 7);
    assert_eq!(summary["matched"], 2);
    assert_eq!(summary["mismatched"], 1);
    assert_eq!(summary["failed"], 1);
    assert_eq!(summary["pending"], 1);
    assert_eq!(summary["unknown"], 2);
    assert_eq!(summary["orphans"], 1);

    let records = report["records"]
        .as_array()
        .map(|records| records.len())
        .unwrap_or(0);
    assert_eq!(records, 7);

    let recovered = report["records"]
        .as_array()
        .and_then(|records| records.iter().find(|record| record["id"] == "inv_search"))
        .and_then(|record| record["recovered_by"].as_str())
        .map(str::to_string);
    assert_eq!(recovered.as_deref(), Some("log_search"));
}

#[test]
fn report_states_which_blocks_were_searched() {
    let output = run(&["--config", "recon.example.toml", "scan", "--format", "json"]);
    let report = json_of(&output);
    // Alignment is on by default: 60 minutes at the fixture's 12 s/block is 300
    // blocks, so the range must be derived rather than left at the 2000-block
    // default.
    assert_eq!(report["search"]["from_block"], 700);
    assert_eq!(report["search"]["to_block"], 1000);
    assert_eq!(report["search"]["lookback_blocks"], 300);
    assert_eq!(report["search"]["block_seconds"], 12);

    // The configured lookback stays available as an explicit escape hatch.
    let unaligned = Command::new(env!("CARGO_BIN_EXE_recon"))
        .args(["--config", "recon.example.toml", "scan", "--format", "json"])
        .env("RECON_ALIGN_WINDOW", "false")
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output();
    let unaligned = match unaligned {
        Ok(output) => output,
        Err(e) => panic!("cannot run recon: {e}"),
    };
    let report = json_of(&unaligned);
    assert_eq!(report["search"]["from_block"], 0);
    assert_eq!(report["search"]["lookback_blocks"], 2000);
    assert!(report["search"]["block_seconds"].is_null());
}

#[test]
fn at_block_anchors_the_window_in_the_past() {
    let output = run(&[
        "--config",
        "recon.example.toml",
        "scan",
        "--format",
        "json",
        "--at-block",
        "900",
    ]);
    let report = json_of(&output);
    assert_eq!(report["chain_tip"], 900);
    assert_eq!(report["anchored"], true);
    assert_eq!(report["search"]["to_block"], 900);
    // 60 minutes at 12 s/block is 300 blocks, anchored at 900.
    assert_eq!(report["search"]["from_block"], 600);
    assert_eq!(report["search"]["block_seconds"], 12);

    // A window that has not happened yet is a usage error, not a quiet pass.
    let future = run(&[
        "--config",
        "recon.example.toml",
        "scan",
        "--format",
        "json",
        "--at-block",
        "2000",
    ]);
    assert_eq!(future.status.code(), Some(78));
}

#[test]
fn csv_export_carries_every_record_and_orphan() {
    let output = run(&["--config", "recon.example.toml", "scan", "--format", "csv"]);
    assert!(
        !output.status.success(),
        "the fixture has differences, so the exit code must stay 1"
    );
    let csv = String::from_utf8_lossy(&output.stdout);
    let mut lines = csv.lines();
    assert_eq!(
        lines.next(),
        Some(
            "kind,id,outcome,pay_to,payer,amount_atomic,tx_hash,block_number,confirmations,recovered_by,reason"
        )
    );
    let rows: Vec<&str> = lines.collect();
    assert_eq!(rows.len(), 8, "7 ledger records plus 1 orphan: {csv}");
    assert_eq!(
        rows.iter().filter(|row| row.starts_with("orphan,")).count(),
        1,
        "{csv}"
    );
    assert_eq!(
        rows.iter()
            .filter(|row| row.starts_with("payment,"))
            .count(),
        7,
        "{csv}"
    );
    assert!(
        rows.iter()
            .any(|row| row.starts_with("payment,inv_search,MATCHED,")),
        "{csv}"
    );
}

#[test]
fn probe_reads_mock_metadata_without_network() {
    let output = run(&["--config", "recon.example.toml", "probe", "--json"]);
    assert!(output.status.success(), "probe in mock mode must succeed");
    let body = json_of(&output);
    assert_eq!(body["mode"], "mock");
    assert_eq!(body["tip"], 1000);
    assert_eq!(body["token_symbol"], "USDT");
    assert_eq!(body["token_decimals"], 6);
    // Fixture blocks are 12 s apart, which is what `--window-minutes` and
    // `--align-window` rely on; the probe must expose it.
    assert_eq!(body["block_seconds"], 12);
}

#[test]
fn fail_on_none_keeps_the_report_and_drops_the_failure() {
    // The fixture has mismatched/failed records, so the default gate exits 1.
    // `none` is the report-only mode: same findings, green exit.
    let output = run(&[
        "--config",
        "recon.example.toml",
        "scan",
        "--format",
        "json",
        "--fail-on",
        "none",
    ]);
    assert_eq!(output.status.code(), Some(0), "--fail-on none must exit 0");
    let report = json_of(&output);
    assert_eq!(report["summary"]["mismatched"], 1);
    assert_eq!(report["summary"]["failed"], 1);

    // A single named class narrows, it does not widen: `unknown` alone is 1
    // because the fixture carries two unknown records, and it ignores the
    // mismatched ones the default gate would have caught.
    let narrow = run(&[
        "--config",
        "recon.example.toml",
        "scan",
        "--format",
        "json",
        "--fail-on",
        "unknown",
    ]);
    assert_eq!(narrow.status.code(), Some(1));

    // Comma-separated classes union.
    let union = run(&[
        "--config",
        "recon.example.toml",
        "scan",
        "--format",
        "json",
        "--fail-on",
        "differences,orphans",
    ]);
    assert_eq!(union.status.code(), Some(1));
}

#[test]
fn fail_on_pending_and_orphans_read_the_ledger_not_the_defaults() {
    let dir = std::env::temp_dir().join(format!("recon-cli-gate-{}", std::process::id()));
    std::fs::create_dir_all(&dir).ok();
    let ledger = dir.join("only_pending.jsonl");
    let record = r#"{"id":"inv_pending","pay_to":"0x2222222222222222222222222222222222222222","payer":"0x8888888888888888888888888888888888888888","amount_atomic":"3000","tx_hash":"0x5555555555555555555555555555555555555555555555555555555555555555","observed_at":"2026-11-22T10:04:00Z"}"#;
    std::fs::write(&ledger, record).ok();

    let scan_with = |gate: &str| {
        let output = Command::new(env!("CARGO_BIN_EXE_recon"))
            .args([
                "--config",
                "recon.example.toml",
                "scan",
                "--format",
                "text",
                "--fail-on",
                gate,
            ])
            .env("RECON_LEDGER_PATH", &ledger)
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .output();
        match output {
            Ok(output) => output,
            Err(e) => panic!("cannot run recon: {e}"),
        }
    };

    // Nothing final yet: `pending` is undecided, so it exits 2 and not 1.
    assert_eq!(scan_with("pending").status.code(), Some(2));
    // The default gate ignores that pending record entirely.
    assert_eq!(scan_with("differences").status.code(), Some(0));
    // Chain transfers with no ledger record are orphans; gating on them
    // catches a ledger that is missing entries the chain already settled.
    assert_eq!(scan_with("orphans").status.code(), Some(1));

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn strict_mode_distinguishes_pending_from_clean() {
    // A ledger that only has the pending record yields exit code 2 when strict.
    let dir = std::env::temp_dir().join(format!("recon-cli-{}", std::process::id()));
    std::fs::create_dir_all(&dir).ok();
    let ledger = dir.join("only_pending.jsonl");
    let record = r#"{"id":"inv_pending","pay_to":"0x2222222222222222222222222222222222222222","payer":"0x8888888888888888888888888888888888888888","amount_atomic":"3000","tx_hash":"0x5555555555555555555555555555555555555555555555555555555555555555","observed_at":"2026-11-22T10:04:00Z"}"#;
    std::fs::write(&ledger, record).ok();

    let output = Command::new(env!("CARGO_BIN_EXE_recon"))
        .args([
            "--config",
            "recon.example.toml",
            "scan",
            "--format",
            "text",
            "--strict",
        ])
        .env("RECON_LEDGER_PATH", &ledger)
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output();
    let output = match output {
        Ok(output) => output,
        Err(e) => panic!("cannot run recon: {e}"),
    };
    assert_eq!(output.status.code(), Some(2), "strict pending must exit 2");
    std::fs::remove_dir_all(&dir).ok();
}
