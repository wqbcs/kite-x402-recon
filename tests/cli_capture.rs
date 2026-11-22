//! Black-box check for `recon capture` against the mock chain.

use std::process::Command;

fn run_with(args: &[&str], envs: &[(&str, &str)]) -> std::process::Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_recon"));
    command.args(args).current_dir(env!("CARGO_MANIFEST_DIR"));
    for (key, value) in envs {
        command.env(key, value);
    }
    match command.output() {
        Ok(output) => output,
        Err(e) => panic!("cannot run recon: {e}"),
    }
}

fn run(args: &[&str]) -> std::process::Output {
    run_with(args, &[])
}

#[test]
fn capture_writes_a_ledger_the_scanner_accepts() {
    let path = std::env::temp_dir().join("recon-cli-capture.jsonl");
    let _ = std::fs::remove_file(&path);
    let path_arg = path.to_string_lossy().to_string();

    let output = run(&[
        "--config",
        "recon.example.toml",
        "capture",
        "--out",
        &path_arg,
    ]);
    assert!(output.status.success(), "capture exited non-zero");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("mode=mock"), "stdout was: {stdout}");
    assert!(stdout.contains("records=4"), "stdout was: {stdout}");

    let body = match std::fs::read_to_string(&path) {
        Ok(body) => body,
        Err(e) => panic!("capture did not write {path_arg}: {e}"),
    };
    let lines: Vec<&str> = body
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    assert_eq!(lines.len(), 4, "captured ledger: {body}");
    for line in &lines {
        let record: serde_json::Value = match serde_json::from_str(line) {
            Ok(record) => record,
            Err(e) => panic!("captured line is not JSON ({e}): {line}"),
        };
        assert_eq!(
            record["pay_to"], "0x2222222222222222222222222222222222222222",
            "capture must keep the receiver it filtered on"
        );
        assert!(record["tx_hash"].is_string(), "record has no tx hash");
    }

    // The scanner must accept the file capture just produced, and because the
    // records were written from the same transfers, every one must match.
    let scan = run_with(
        &["--config", "recon.example.toml", "scan", "--format", "json"],
        &[("RECON_LEDGER_PATH", &path_arg)],
    );
    let stdout = String::from_utf8_lossy(&scan.stdout);
    let report: serde_json::Value = match serde_json::from_str(stdout.trim()) {
        Ok(report) => report,
        Err(e) => panic!("scan stdout is not JSON ({e}):\n{stdout}"),
    };
    assert_eq!(report["summary"]["total"], 4);
    // Two captured transfers have a receipt in the fixture; the other two are
    // fixture transfers without one, so they stay pending rather than failing.
    assert_eq!(report["summary"]["matched"], 2);
    assert_eq!(report["summary"]["pending"], 2);
    assert_eq!(report["summary"]["mismatched"], 0);
    assert_eq!(report["summary"]["failed"], 0);
    assert_eq!(report["summary"]["unknown"], 0);
    assert_eq!(report["summary"]["orphans"], 0);
    assert_eq!(report["summary"]["duplicates"], 0);

    std::fs::remove_file(&path).ok();
}

#[test]
fn capture_rejects_a_inverted_block_range() {
    let path = std::env::temp_dir().join("recon-cli-capture-bad.jsonl");
    let path_arg = path.to_string_lossy().to_string();
    let output = run(&[
        "--config",
        "recon.example.toml",
        "capture",
        "--from-block",
        "2000",
        "--to-block",
        "1000",
        "--out",
        &path_arg,
    ]);
    assert_eq!(
        output.status.code(),
        Some(78),
        "bad range must be a config error"
    );
    assert!(
        !std::path::Path::new(&path_arg).exists(),
        "no file on failure"
    );
}
