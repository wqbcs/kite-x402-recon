# Changelog

All notable changes to this project are documented here. Format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); this project uses semantic versioning.

## [Unreleased]

### Added

- `server.request_timeout_seconds` (default 15, `1..=300`, `RECON_REQUEST_TIMEOUT_SECONDS`): every
  request now waits at most that long for one reconciliation, then answers `504` and leaves the
  window unpopulated. `/readyz` uses the same deadline and reports `503` for a probe it could not
  finish. The refresh cache holds its lock across the reconciliation, so before this one stuck chain
  read parked the request and every poll queued behind it with no deadline at all.
- `serve` keeps one report for `server.refresh_seconds` and answers later polls from it. The
  endpoint has always advertised `Cache-Control: max-age=<refresh_seconds>`, but it reconciled on
  every request: a caller caching on that header served a report up to one window old while the
  service itself paid a chain tip read and a ledger parse per poll. Both sides now mean the same
  window — a burst of polls gets byte-identical bodies, one RPC read per window, and concurrent
  polls wait on the single in-flight reconciliation. The response says how it was produced in
  `X-Recon-Cache`: `hit`, `miss`, or `bypass` for a replayed `at_block` window, which is never
  cached because two windows share one URL path.
- `scan --fail-on <classes>`: choose which findings make the run exit non-zero — `differences`
  (`mismatched`/`failed`, the default), `orphans`, `unknown`, `pending`, `any` or `none`.
  Comma-separated classes union (`--fail-on differences,orphans`), so a CI gate can fail on money
  differences and chain transfers with no ledger record while treating unresolved transactions as
  warnings, and `--fail-on none` turns a run into report-only. `--strict` keeps its documented
  meaning and now reads as "add `pending` to the gate". Exit `1` for a selected finding class, `2`
  for `pending`, and money differences still win over pending.
- `GET /readyz`: readiness separate from liveness. It reads a chain tip and parses the ledger, then
  answers `200` with `{status, chain_id, tip, ledger, records}` or `503` naming the failing side
  (`chain:` or `ledger:`). `/healthz` still never touches an upstream, so a load balancer can use the
  pair to drain an instance whose RPC has been rate-limited instead of routing reports into it.
- `report.receiver`: the address the verdicts were taken against, normalized to lowercase. A saved
  report can now be re-verified without guessing which wallet the run was aimed at; "paid to the
  wrong address" is meaningless unless the expected address travels with the finding.
- `GET /api/v1/reconcile?format=csv`: the read-only endpoint serves the same CSV export as the
  CLI (`text/csv; charset=utf-8`, `Content-Disposition: attachment; filename="reconcile-<from>-<to>.csv"`),
  so a spreadsheet or incident archive can pull evidence without shell access. Unknown `format`
  values return `400` instead of silently falling back to JSON.
- `scan --format csv`: the reconciliation as RFC 4180 CSV — one row per ledger record plus one row
  per orphan transfer (`kind` tells them apart), columns
  `kind,id,outcome,pay_to,payer,amount_atomic,tx_hash,block_number,confirmations,recovered_by,reason`.
  Nullable fields stay empty rather than invented, and commas/quotes/newlines are quoted, so the
  file opens in a spreadsheet and parses with a standard CSV reader. The bundled fixture already
  exercises quoting: its mismatch reason contains a comma.
- `probe` reports `block_seconds` (text `block_s=`, JSON `block_seconds`): the measured average
  block time, `null` when it cannot be read. It is the number that decides whether
  `search_lookback_blocks` makes sense on a given chain (2000 blocks ≈ 7 h at 12 s, ≈ 20 days at
  Kite testnet's 886 s), so it belongs in the first command an operator runs.
- `report.search` (`{from_block, to_block, lookback_blocks, block_seconds}`) in JSON, `search=…`
  in text and a provenance line in Markdown: a verdict now carries the block range it was derived
  from, so "no transfer found" can be distinguished from "the search stopped too early".

### Added

- `GET /api/v1/reconcile?at_block=<n>`: the served API can replay a past window, mirroring
  `scan --at-block`; each anchored read clones the shared reconciler, so live requests are
  unaffected. Invalid or future anchors return `400`.
- `scan --at-block <n>` / `Reconciler::with_anchor_tip`: evaluate the ledger as of a past block
  instead of the current tip. The aligned range, orphan scan and confirmation counts all move with
  the anchor, so "what did this window look like at block X" can be replayed days later and yield
  the same answer — and the same measured block time. `report.anchored` / `tip=N (anchored)` mark
  such a run, so an old tip in a report is not mistaken for a stalled chain. Refused with exit `78`
  when the anchor is ahead of the tip.

### Changed

- Window alignment moved from a `scan` flag into the reconciler and config: `align_window`
  defaults to `true` (`RECON_ALIGN_WINDOW`, `align_window` in `[reconcile]`), and `serve` /
  `/api/v1/reconcile` inherit it. Previously only a CLI flag could align the range, so a served
  instance kept searching the raw 2000-block lookback while the CLI reported the window.
  `--align-window` remains as an explicit override for a single run. Recent measurements are
  cached per tip (eight entries), so a served instance spends one probe pair when a tip is first
  seen — including a dashboard alternating between a live read and a replayed anchor.
- Rendered reports now end with exactly one newline. `scan` used to append a second one, which left
  a blank line at the end of every redirected output (and an empty final row in a saved CSV).

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
