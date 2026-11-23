# Testing

## Commands

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

`cargo test` runs three suites:

| Suite | File | What it proves |
| --- | --- | --- |
| Unit (lib) | `src/*::tests` | parsing, config, decoding, all reconciliation outcomes, rendering |
| CLI black-box | `tests/cli_scan.rs` | the real binary produces the documented JSON, exits 1 on differences, exits 2 under `--strict` when pending, and `probe` works offline |
| HTTP contract | `tests/http_api.rs` | router shape: health open, 401 without token, 200 with token, fixture summary identical to CLI |

## Current coverage map

| Area | Cases |
| --- | --- |
| Amounts | plain digits, trim, reject `-1 / +1 / 1.5 / 1,000 / 0x10 / ""`, u128 overflow |
| Timestamps | RFC 3339 round-trip, invalid input |
| Config | live needs token address + http RPC, receiver address required in every mode, window/max-records/lookback bounds, http ledger needs URL, address shape |
| Chain decoding | `Transfer` topic → address, ABI `string` (symbol), hex numbers, decimals mismatch |
| Reconciliation | matched, reverted → failed, short payment → mismatched, too few confirmations → pending, no receipt → pending, missing hash recovered by log search, ambiguous/absent candidates, orphans, wrong receiver → unknown, bad amount → unknown, stale record in live mode → unknown, duplicate claim → second record unknown + `duplicates` |
| Mock client | block-window + receiver filtering, tx-hash validation |
| Ledger | JSONL line numbers on parse errors, `{records:[…]}` vs array vs garbage, `kite-x402-hono` receipt normalisation (camelCase fields, ms/second timestamps, numeric amounts) |
| Report | JSON round-trip, text/markdown mode label, exit-code matrix |
| Capture | transfers → records, unique ids for several transfers in one transaction, JSONL round-trip, capture → scan round trip, window→block arithmetic (`blocks_for_window` incl. empty/rounding cases), chain-measured window alignment, inverted block range rejected, conflicting range flags rejected |
| Server | constant-time compare, bearer enforcement |

## Determinism

- `Reconciler::report` takes `now: i64`, so the freshness check never depends on wall-clock time
  inside tests.
- `FixtureChain` reads a static JSON file; no sockets are opened.
- Fixture timestamps are frozen; mock mode deliberately skips the live freshness window.

## Fixtures

`fixtures/ledger.jsonl` and `fixtures/chain_mock.json` are synthetic and intentionally describe
one record per outcome plus an orphan. Expected summary:

```
total=7 matched=2 pending=1 mismatched=1 failed=1 unknown=2 orphans=1 duplicates=0
```

`fixtures/hono_receipts.jsonl` (two `kite-x402-hono` receipts → `total=2 matched=1 pending=1
orphans=3`) and `fixtures/ledger_duplicate.jsonl` (two invoices claiming one transfer →
`duplicates=1`) cover the newer paths.

`fixtures/kite_testnet_capture.jsonl` is the one **real** fixture: two testnet transfers captured
by `recon capture` on 2026-11-22 (public chain data, receiver is not the project's wallet). Do not
"refresh" it casually — it is a dated observation, and `docs/EVIDENCE.md` records the block range
it came from.

`tests/cli_capture.rs` runs `capture` against the mock chain, then feeds its output back into
`scan`, so the two commands cannot drift apart.

If you edit the fixtures, update `tests/cli_scan.rs` and `tests/http_api.rs` in the same commit —
they assert exact counts on purpose, so silent semantic drift fails CI.

## Adding a test

- Pure rule change → add a unit test in `src/reconcile.rs` with `in_memory(...)`; no file I/O.
- New config field → add a validation test in `src/config.rs` and extend `recon.example.toml`.
- New output field → extend `report.rs` tests and the CLI/HTTP assertions so all three views agree.
- New endpoint → add a `http_api.rs` case including the unauthenticated path.

## Reproducing a live probe

```bash
cargo run -- --config recon.toml probe --json     # with chain.mode = "live"
```

This is the only test-adjacent command that needs the network; it is excluded from CI to keep the
build hermetic. If the RPC provider rate-limits, `probe` exits 69 with the HTTP status in the
message.
