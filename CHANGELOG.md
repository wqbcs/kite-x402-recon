# Changelog

All notable changes to this project are documented here. Format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); this project uses semantic versioning.

## [Unreleased]

### Planned

- A gateway-specific ledger adapter (beyond generic JSONL/HTTP).
- Block-timestamp binary search to replace the fixed `search_lookback_blocks` window.
- Prometheus text exposition for `mismatched`/`pending` gauges.

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
