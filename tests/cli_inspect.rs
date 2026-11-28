//! Black-box checks for `recon inspect`: one transaction, one verdict.
//!
//! The interesting part is not the happy path but the boundaries: a hash that
//! cannot address anything is a usage error, a hash the ledger never heard of
//! is "no input", and an undecided verdict must not read as a money difference.

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

const OK_HASH: &str = "0x1111111111111111111111111111111111111111111111111111111111111111";
const REVERTED_HASH: &str = "0x4444444444444444444444444444444444444444444444444444444444444444";
const PENDING_HASH: &str = "0x5555555555555555555555555555555555555555555555555555555555555555";

#[test]
fn inspect_answers_the_matched_settlement_with_the_same_shape_as_the_api() {
    let output = run(&[
        "--config",
        "recon.example.toml",
        "inspect",
        OK_HASH,
        "--format",
        "json",
    ]);
    assert_eq!(
        output.status.code(),
        Some(0),
        "a matched record must exit 0"
    );
    let answer = json_of(&output);
    assert_eq!(answer["mode"], "mock");
    assert_eq!(answer["chain_id"], 2368);
    assert_eq!(answer["chain_tip"], 1000);
    assert_eq!(answer["ledger"], "file:fixtures/ledger.jsonl");
    assert_eq!(
        answer["receiver"],
        "0x2222222222222222222222222222222222222222"
    );
    assert_eq!(answer["record"]["id"], "inv_ok");
    assert_eq!(answer["record"]["outcome"], "matched");
    assert_eq!(answer["record"]["block_number"], 990);
    assert_eq!(answer["record"]["confirmations"], 11);
    assert_eq!(answer["record"]["tx_hash"], OK_HASH);
}

#[test]
fn inspect_accepts_an_upper_case_hash_and_writes_it_back_canonically() {
    let upper = format!("0X{}", OK_HASH.trim_start_matches("0x").to_uppercase());
    let output = run(&[
        "--config",
        "recon.example.toml",
        "inspect",
        &upper,
        "--format",
        "json",
    ]);
    assert_eq!(output.status.code(), Some(0));
    let answer = json_of(&output);
    assert_eq!(answer["record"]["id"], "inv_ok");
    assert_eq!(answer["record"]["tx_hash"], OK_HASH);
}

#[test]
fn inspect_text_names_the_verdict_the_tip_and_the_ledger() {
    let output = run(&[
        "--config",
        "recon.example.toml",
        "inspect",
        OK_HASH,
        "--format",
        "text",
    ]);
    assert_eq!(output.status.code(), Some(0));
    let text = String::from_utf8_lossy(&output.stdout).to_string();
    assert!(text.contains("mode=mock"), "{text}");
    assert!(text.contains("tip=1000"), "{text}");
    assert!(text.contains("ledger=file:fixtures/ledger.jsonl"), "{text}");
    assert!(text.contains("MATCHED"), "{text}");
    assert!(text.contains("inv_ok"), "{text}");
    assert!(text.contains("fixtures/chain_mock.json"), "{text}");
}

#[test]
fn inspect_exits_1_only_for_a_money_difference() {
    let failed = run(&[
        "--config",
        "recon.example.toml",
        "inspect",
        REVERTED_HASH,
        "--format",
        "json",
    ]);
    assert_eq!(
        failed.status.code(),
        Some(1),
        "a reverted transfer is a difference"
    );
    assert_eq!(json_of(&failed)["record"]["outcome"], "failed");

    // Undecided is not a difference: exit 2 keeps "not yet" apart from "wrong".
    let pending = run(&[
        "--config",
        "recon.example.toml",
        "inspect",
        PENDING_HASH,
        "--format",
        "json",
    ]);
    assert_eq!(pending.status.code(), Some(2));
    assert_eq!(json_of(&pending)["record"]["outcome"], "pending");
}

#[test]
fn inspect_rejects_a_hash_that_cannot_address_a_record() {
    for bad in ["0x123", "1111", "0xzzzz"] {
        let output = run(&["--config", "recon.example.toml", "inspect", bad]);
        assert_eq!(output.status.code(), Some(78), "{bad}");
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        assert!(stderr.contains("32-byte"), "{bad}: {stderr}");
    }
}

#[test]
fn inspect_says_so_when_the_ledger_never_heard_of_the_hash() {
    let unknown = format!("0x{}", "99".repeat(32));
    let output = run(&["--config", "recon.example.toml", "inspect", &unknown]);
    assert_eq!(output.status.code(), Some(66), "no such record is no input");
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    assert!(stderr.contains(&unknown), "{stderr}");
    assert!(String::from_utf8_lossy(&output.stdout).trim().is_empty());
}

#[test]
fn inspect_honours_an_explicit_anchor_and_refuses_a_future_one() {
    let anchored = run(&[
        "--config",
        "recon.example.toml",
        "inspect",
        OK_HASH,
        "--format",
        "json",
        "--at-block",
        "995",
    ]);
    assert_eq!(anchored.status.code(), Some(0));
    let answer = json_of(&anchored);
    assert_eq!(answer["chain_tip"], 995);
    // 990 is 995 blocks in, inclusive of the block itself: six confirmations.
    assert_eq!(answer["record"]["confirmations"], 6);

    let future = run(&[
        "--config",
        "recon.example.toml",
        "inspect",
        OK_HASH,
        "--format",
        "json",
        "--at-block",
        "2000",
    ]);
    assert_eq!(
        future.status.code(),
        Some(78),
        "a future anchor is a usage error"
    );
}

#[test]
fn inspect_refuses_csv_because_a_single_record_is_not_a_window() {
    let output = run(&[
        "--config",
        "recon.example.toml",
        "inspect",
        OK_HASH,
        "--format",
        "csv",
    ]);
    assert_eq!(output.status.code(), Some(78));
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    assert!(stderr.contains("csv"), "{stderr}");
}
