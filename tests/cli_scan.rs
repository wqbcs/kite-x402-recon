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
    // Fixture tip 1000 with the default 2000-block lookback clamps at block 0.
    assert_eq!(report["search"]["from_block"], 0);
    assert_eq!(report["search"]["to_block"], 1000);
    assert_eq!(report["search"]["lookback_blocks"], 2000);

    // 60 minutes at the fixture's 12 s/block is 300 blocks, so alignment must
    // narrow the reported range instead of leaving the reader to guess.
    let aligned = run(&[
        "--config",
        "recon.example.toml",
        "scan",
        "--format",
        "json",
        "--align-window",
    ]);
    let report = json_of(&aligned);
    assert_eq!(report["search"]["from_block"], 700);
    assert_eq!(report["search"]["lookback_blocks"], 300);
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
