# Changelog

All notable changes to this project are documented here. Format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); this project uses semantic versioning.

## [Unreleased]

### Added

- `probe` reports `block_seconds` (text `block_s=`, JSON `block_seconds`): the measured average
  block time, `null` when it cannot be read. It is the number that decides whether
  `search_lookback_blocks` makes sense on a given chain (2000 blocks ≈ 7 h at 12 s, ≈ 20 days at
  Kite testnet's 886 s), so it belongs in the first command an operator runs.
- `report.search` (`{from_block, to_block, lookback_blocks, block_seconds}`) in JSON, `search=…`
  in text and a provenance line in Markdown: a verdict now carries the block range it was derived
  from, so "no transfer found" can be distinguished from "the search stopped too early".

### Changed

- Window alignment moved from a `scan` flag into the reconciler and config: `align_window`
  defaults to `true` (`RECON_ALIGN_WINDOW`, `align_window` in `[reconcile]`), and `serve` /
  `/api/v1/reconcile` inherit it. Previously only a CLI flag could align the range, so a served
  instance kept searching the raw 2000-block lookback while the CLI reported the window.
  `--align-window` remains as an explicit override for a single run; the probe cost is one
  `eth_getBlockByNumber` per report.

## [0.3.0] - 2026-11-23

Audit windows and block ranges now describe the same period. Both new flags measure the chain's
own block time instead of assuming one, because a lookback expressed in blocks is meaningless
without it: Kite testnet averaged **886 s/block** on this date, so the 2000-block default reached
back roughly 20 days.

### Added

- `capture --window-minutes <n>`: derive the block range from an audit window using the chain's
  measured block time (`eth_getBlockByNumber` at the tip and up to 1000 blocks back), rounding up
  so the range is never short. Conflicts with `--from-block`. A warning is logged when `scan` will
  search a wider range than was captured.
- `scan --align-window`: search the block range that `reconcile.window_minutes` actually covers,
  measured from the chain, instead of the fixed `search_lookback_blocks`. The same 24-hour testnet
  ledger reported 15 orphans unaligned and 0 aligned — the 15 were old money outside the window.
- `capture::average_block_seconds` and `Reconciler::with_lookback_blocks` as library entry points,
  so a caller embedding this crate can align its own ranges the same way.

## [0.2.0] - 2026-11-22

Real transaction data end to end, and the ecosystem hook the first version lacked.

### Added

- `recon capture`: writes a `ledger.kind = "file"` JSONL from the token transfers a block range
  contains (`--from-block`, `--to-block`, `--receiver`, `--out`), so a missing gateway ledger can
  be rebuilt from the chain and then audited by `scan`. One transaction emitting several
  transfers gets unique ids (`hash`, `hash#2`, …). Records carry the capture time, not the
  settlement time.
- `ledger.kind = "hono_file"` (alias `hono`): consumes a `kite-x402-hono` `SettlementReceipt`
  JSONL dump (camelCase fields, second or millisecond `timestamp`) and verifies each receipt on
  chain instead of trusting its `success` flag. Fixture: `fixtures/hono_receipts.jsonl`.
- Duplicate-claim detection: when two ledger records point at the same `tx_hash`, the later record
  is reported `unknown` with a reason naming the first claim, and `summary.duplicates` counts it.
- `LedgerKind::as_str` / `Display` so logs and `probe --json` print `hono_file` rather than a
  derived debug name.
- Testnet payment-token guidance updated to the Kite Service Provider Guide address
  `0x0fF5393387ad2f9f691FD6Fd28e07E3969e27e63`; the fixture placeholder in
  `recon.example.toml` is unchanged and explained inline.
- `fixtures/kite_testnet_capture.jsonl`: two real testnet transfers captured 2026-11-22, used by
  the recorded live round trip in [`docs/EVIDENCE.md`](docs/EVIDENCE.md).

### Planned

- Block-timestamp binary search to replace the fixed `search_lookback_blocks` window.
- Prometheus text exposition for `mismatched`/`pending` gauges.
- Optional CSV ledger source.

## [0.1.0] - 2026-11-22

First runnable version: read-only reconciliation end to end, no network needed for the demo.

### Added

- `recon probe`: chain reader check (tip, chain id, token symbol/decimals) for `live` and `mock`.
- `recon scan`: per-record reconciliation with five outcomes (`matched`, `pending`, `mismatched`,
  `failed`, `unknown`), reasons per record, orphan transfers, `text`/`json`/`markdown` output,
  `--strict` exit code, `--out` file.
- `recon serve`: read-only HTTP API (`/healthz`, `/api/v1/reconcile`) with constant-time bearer
  auth controlled by an env-named token.
- Chain access via plain JSON-RPC (`eth_blockNumber`, `eth_getTransactionReceipt`, `eth_getLogs`,
  `eth_call`) in `src/chain.rs`; no chain SDK, no key handling.
- Log-search recovery when a ledger record has no `tx_hash`, marked `recovered_by = "log_search"`.
- Ledger sources: JSONL file, HTTP endpoint (`{records:[…]}` or bare array), in-memory.
- Fixture chain (`fixtures/chain_mock.json`) and synthetic ledger (`fixtures/ledger.jsonl`)
  covering one record per outcome plus an orphan; mock mode is labelled in every output.
- Config: TOML + `RECON_*` overrides, fail-fast validation (addresses, ranges, URL shapes),
  secrets only through named environment variables.
- Tests: 30 unit + 3 CLI black-box + 3 HTTP contract cases; `clippy -D warnings` clean; formatted.
- Documentation: README, ARCHITECTURE, CLI, API, TESTING, DEPLOYMENT, CONTRIBUTING, this changelog,
  `recon.example.toml`, `.env.example`.

### Known limitations

- Mainnet presets are untested by CI; fixtures are synthetic.
- No UI, no write path, no settlement retry, no websocket subscriptions.
- Log search is bounded by blocks, not timestamps.
- A dedicated gateway adapter is not included; the HTTP source expects the documented shape.
