# kite-x402-recon

Read-only settlement reconciliation for [x402](https://x402.org) services that settle on the
**Kite** chain. You give it two inputs — a list of settlements your gateway *believes* happened,
and a chain window — and it tells you, per record, whether the money actually moved.

```
$ recon --config recon.example.toml scan
mode=mock chain_id=2368 tip=1000 window=60m generated_at=...
total=7 matched=2 pending=1 mismatched=1 failed=1 unknown=2 orphans=1 duplicates=0
MATCHED    inv_ok 0x2222…2222 amount=10000 tx=0x1111…1111 :: transfer of 10000 atomic units confirmed at block 990
PENDING    inv_pending 0x2222…2222 amount=3000 tx=0x5555…5555 :: no receipt yet; …
ORPHAN     block=999 tx=0x6666…6666 from=0x9999…9999 to=0x2222…2222 amount=25000
```

## Why this exists

The x402 ecosystem on Kite already has a gateway template
([`kite-x402-axum`](https://github.com/tianzeshi-study/kite-x402-axum)) that records settlements,
and sibling projects for gateway deployment, exploration and attestation. None of them answer the
one question an operator asks at 2 a.m.:

> The gateway says the payment settled with tx `0x…`, but did the token actually arrive at my
> address, in the right amount, and is it final?

`kite-x402-recon` is that tool. It is deliberately read-only: no private keys, no signing, no
re-submitting settlements, no writing to a chain.

## What it does

| Command | Purpose |
| --- | --- |
| `recon probe` | Verify RPC reachability, chain id, tip, measured block time and token metadata (symbol/decimals). |
| `recon scan` | Reconcile one ledger batch against the chain window; render `text`, `json`, `markdown` or `csv`. |
| `recon capture` | Rebuild a ledger file from the token transfers a block range contains (recovery path). |
| `recon serve` | Expose the same report over a small read-only HTTP API (`/healthz`, `/api/v1/reconcile`). |

Per record outcome:

| Outcome | Meaning |
| --- | --- |
| `matched` | A successful ERC-20 `Transfer` to your receiver, for at least the expected amount, with enough confirmations. |
| `pending` | Not conclusive yet: no receipt, too few confirmations, or nothing in the search window. |
| `mismatched` | The receipt exists and succeeded, but the transfer to your receiver is short or absent. |
| `failed` | The transaction reverted. |
| `unknown` | The record itself is unusable (bad amount, wrong receiver, malformed hash, stale). |

Plus **orphans**: transfers to your receiver in the window that no ledger record claims — the
classic "money arrived but the gateway lost the invoice" case.

And **duplicate claims**: two ledger records pointing at the same transaction hash. One transfer
cannot pay two invoices; the second record is reported as `unknown` with a reason naming the first
claim, and `summary.duplicates` counts it.

## How this differs from what already exists

Verified against the ecosystem as of 2026-11-22, so the claim is honest rather than promotional:

| Existing thing | What it does | What it does **not** do |
| --- | --- | --- |
| `kite-x402-hono` `SettlementReconciler` (the reference gateway) | Stores facilitator-reported receipts in memory: `txHash`, `payer`, `payTo`, `amount`, `success` | Never reads the chain. Its name is about bookkeeping; `success` is whatever the facilitator said. Our `hono_file` source consumes exactly that export and verifies each receipt on-chain. |
| Block explorer (kitescan) | Lets a human look up one transaction | Cannot answer "which of my 500 invoices did not land", cannot map a transfer back to an invoice id, and has no batch/CI story. |
| [x402trace](https://github.com/fardinvahdat/x402trace) (Base Sepolia, Apache-2.0) | Inline proxy that joins facilitator-rejected requests to USDC transfers by `(payer, payee, value, nonce)` | Requires sitting in the request path, is Base/USDC/EIP-3009-specific, has no ledger-file mode, no orphan detection, no confirmation policy, no CI gate, no HTTP API. |
| piero `x402` facilitators `/verify`, `/settle` | Decide whether to accept or execute a payment | Say nothing after the fact when a settle times out or the gateway goes down mid-request. |
| [hiero-x402](https://github.com/hiero-hackers/hiero-x402) | Independent settlement check + receipt on Hedera | Hedera-only, per-payment receipts for agents, not a batch auditor of an existing ledger. |

So: the *idea* of post-settlement verification is not novel, and this project does not claim it is.
The narrow gap it fills is a **batch, offline, ledger-fed audit for Kite**: you already have a
ledger export (in our format or `kite-x402-hono`'s receipt dump), you do not want a proxy in the
payment path, and you want per-invoice adjudication, orphan money, duplicate claims and a CI exit
code. If you run Base and only need inline capture, use `x402trace`; if you need Hedera receipts,
use `hiero-x402`. Related-work comparison lives in this README so reviewers can check the delta
themselves.

When a ledger record has no `tx_hash`, the reconciler searches ERC-20 `Transfer` logs for
`(payer, payTo, amount ≥ expected)` inside `search_lookback_blocks` and reports the record as
matched with `"recovered_by": "log_search"`, or as pending if several candidates are ambiguous.

## Relation to KiteAI / x402

- **Chain**: reads Kite testnet (`chain_id 2368`, `https://rpc-testnet.gokite.ai/`) or mainnet
  (`2366`) presets; every RPC call is plain JSON-RPC (`eth_blockNumber`,
  `eth_getTransactionReceipt`, `eth_getLogs`, `eth_call`) implemented in
  [`src/chain.rs`](src/chain.rs).
- **Testnet payment token preset**: `0x0fF5393387ad2f9f691FD6Fd28e07E3969e27e63` from the Kite
  Service Provider Guide (<https://docs.gokite.ai/kite-agent-passport/service-provider-guide>).
  `probe` against it on 2026-11-22 returned `symbol() = USDT`, `decimals() = 18`; reports keep
  atomic units and take decimals from the chain, so `chain.token_decimals` is only a fallback.
  Re-verify on <https://testnet.kitescan.ai/> before trusting it; mainnet has no baked-in token.
- **Reference-gateway receipts**: `ledger.kind = "hono_file"` consumes `kite-x402-hono`'s
  `SettlementReceipt` export and verifies it against the chain instead of trusting its `success`
  field.
- **Verifiable interaction (not just config)**: `recon capture` read real `Transfer` logs on
  Kite testnet at tip `22045329`, wrote them to
  [`fixtures/kite_testnet_capture.jsonl`](fixtures/kite_testnet_capture.jsonl), and `scan` then
  matched both records against real receipts — see the run in
  [`docs/EVIDENCE.md`](docs/EVIDENCE.md). No mock data, no mocked RPC. Window alignment is on by
default for every surface (`scan`, `report`, `serve`): block ranges are derived from the chain's
measured block time (~886 s on testnet, 898 s a day later), so a wall-clock audit window and a
block range cannot drift apart. `RECON_ALIGN_WINDOW=false` (or `align_window = false` in the config)
restores the fixed `search_lookback_blocks` when you deliberately want a wider sweep; the
`--align-window` flag forces alignment back on for a single run. `scan --at-block N` pins the
whole audit to a past block (range, orphans, confirmations), so a verdict you published can be
replayed after the chain has moved on. A captured window replays exactly: use the `to_block`
`capture` printed as the anchor with the same `--window-minutes`.
- **Ecosystem fit**: reconciles the settlement records produced by the x402 gateway flow used by
  `kite-x402-axum` and the other reference projects. The report shape is designed to be consumed
  next to a gateway's own ledger export — see [`fixtures/ledger.jsonl`](fixtures/ledger.jsonl) for
  the accepted record format.
- **No Kite-only SDK dependency**: the association is the chain + the x402 settlement semantics,
  not a proprietary client. That keeps the tool usable if the RPC provider or SDK changes.

Verification evidence for reviewers (EC-style): source locations above, dependency list in
[`Cargo.toml`](Cargo.toml) (`reqwest`, `axum`, `tokio`; no chain SDK), the default network/token
configuration in [`recon.example.toml`](recon.example.toml), and the reproducible mock run below
that needs no network.

## Requirements

- Rust 1.88+ (edition 2021). `cargo build` on Windows, Linux and macOS.
- No database, no Docker requirement for the CLI. A reachable RPC endpoint only for `mode = "live"`.

## Install and build

```bash
git clone https://github.com/wqbcs/kite-x402-recon.git
cd kite-x402-recon
cargo build --release
# binary at target/release/recon(.exe)
```

## Quick start (no network, mock mode)

```bash
cp recon.example.toml recon.toml       # already points at fixtures
cargo run -- --config recon.toml probe --json
cargo run -- --config recon.toml scan --format text
```

The bundled fixtures intentionally contain one record of each outcome, so the summary is exactly
`total=7 matched=2 pending=1 mismatched=1 failed=1 unknown=2 orphans=1 duplicates=0`. Mock mode is
labelled in every output (`mode=mock`) and `scan` prints a note — a fixture run can never be
mistaken for on-chain evidence.

## Capture a ledger from the chain (recovery path)

When the gateway's ledger is missing, the chain is the surviving witness. `recon capture` reads the
token's `Transfer` logs over a block range and writes a normal ledger file:

```bash
RECON_CHAIN_MODE=live \
RECON_TOKEN_ADDRESS=0x0fF5393387ad2f9f691FD6Fd28e07E3969e27e63 \
recon --config recon.example.toml capture \
  --from-block 22045179 --to-block 22045329 \
  --receiver 0x…  --out captured.jsonl
# mode=live chain_id=2368 blocks=22045179..22045329 receiver=0x… records=2 out=captured.jsonl
```

Then audit it exactly like a gateway ledger (`RECON_LEDGER_PATH=captured.jsonl recon scan`). Two
things to know:

- **The range can be derived from the audit window**: `capture --window-minutes 1440` samples the
  chain's actual block time (tip vs. up to 1000 blocks back) and rounds the block count up, so the
  range is never short. On Kite testnet that measured ~886 s/block on 2026-11-23 — a fixed
  lookback in blocks would have been off by orders of magnitude.
- `observed_at` in captured records is the **capture time**, not the settlement time: `eth_getLogs`
  has no wall clock, and asking per block would double the RPC calls. The `tx_hash` is the link to
  the real settlement moment.
- Capture narrows the block range; `scan` searches the `window_minutes` window. A transfer that
  settled inside the audit window but outside your capture range shows up as an `orphan` — that is
  not a false positive, it is money your ledger never recorded. If the estimated range is smaller
  than `search_lookback_blocks`, `capture` logs a warning telling you to run `scan --align-window`
  (or set `RECON_LOOKBACK_BLOCKS`); `scan --align-window` derives the searched range from the same
  measured block time, so the two ranges agree by construction.

## Configuration

`recon.toml` (TOML) with optional `RECON_*` environment overrides; see
[`recon.example.toml`](recon.example.toml) and [`.env.example`](.env.example). Secrets never live
in the file: `ledger.bearer_token_env` names the variable holding the ledger token, and
`server.api_token_env` names the variable holding the API token.

Key fields:

| Field | Notes |
| --- | --- |
| `chain.mode` | `mock` reads a fixture; `live` reads RPC and fails closed (never silently falls back). |
| `chain.token_address` | Settlement token contract. Always operator-provided: testnet and mainnet bridged assets differ. |
| `chain.min_confirmations` | Confirmations before `matched`. Counting includes the receipt's own block. |
| `reconcile.receiver` | Your x402 pay-to address; records for other addresses are `unknown`. |
| `reconcile.window_minutes` | Records older than this are `unknown` **in live mode**; fixtures are frozen recordings and skip the check. |
| `reconcile.search_lookback_blocks` | Block span for log search when a record has no tx hash. |
| `ledger.kind` | `file` (JSONL), `hono_file` (`kite-x402-hono` receipt dump), or `http` (gateway endpoint returning an array or `{"records":[…]}`). |

## Ledger record format

One JSON object per line (or per array element over HTTP):

```json
{"id":"inv_1","pay_to":"0x…","payer":"0x…","amount_atomic":"10000","tx_hash":"0x…","observed_at":"2026-11-22T10:00:00Z"}
```

- `amount_atomic` is a plain decimal string in the token's smallest unit. No signs, dots or separators.
- `observed_at` is RFC 3339.
- `tx_hash` and `payer` are optional; without `tx_hash` the log search path is used.
- The HTTP response may be a bare array or `{"records":[…]}`; both are accepted.

### `kite-x402-hono` receipts

Point `ledger.kind = "hono_file"` at a JSONL dump of the gateway's `SettlementReceipt` objects
(`txHash`, `payer`, `payTo`, `amount`, `network`, `timestamp`, `success`). The adapter maps them to
the internal record shape (`timestamp` may be seconds or milliseconds) and then verifies each one
against the chain — the gateway's `success` flag is **not** trusted. See
[`fixtures/hono_receipts.jsonl`](fixtures/hono_receipts.jsonl) and:

```bash
RECON_LEDGER_KIND=hono_file \
RECON_LEDGER_PATH=fixtures/hono_receipts.jsonl \
RECON_CHAIN_MODE=mock \
recon --config recon.example.toml scan --format text
```

That run prints `total=2 matched=1 pending=1 orphans=3 duplicates=0`: the gateway's
`success=true` receipt is confirmed on chain, while the receipt it reported as failed is still
pending — proof the adapter follows the chain, not the claim.

### Duplicate claims

[`fixtures/ledger_duplicate.jsonl`](fixtures/ledger_duplicate.jsonl) contains two invoices pointing
at the same transfer:

```bash
RECON_LEDGER_PATH=fixtures/ledger_duplicate.jsonl \
RECON_CHAIN_MODE=mock \
recon --config recon.example.toml scan --format text
```

```
total=2 matched=1 pending=0 mismatched=0 failed=0 unknown=1 orphans=3 duplicates=1
UNKNOWN inv_second … :: transaction hash already claimed by record inv_first;
                             one transfer cannot pay two invoices
```

## HTTP API

`recon serve` (default `127.0.0.1:8081`):

| Route | Behavior |
| --- | --- |
| `GET /healthz` | `{"status":"ok","mode":…,"chain_id":…,"auth":"bearer"\|"disabled"}` — liveness, never touches the chain |
| `GET /readyz` | Readiness: reads a chain tip **and** parses the ledger; `{"status":"ready","tip":…,"records":…}`, or `503` naming the failing side |
| `GET /api/v1/reconcile` | Full report JSON, same shape as `scan --format json`; `Cache-Control: max-age=<refresh_seconds>` |

Auth: when `RECON_API_TOKEN` (the variable named by `server.api_token_env`) is set, `/api/v1/reconcile`
requires `Authorization: Bearer <token>`; `/healthz` and `/readyz` stay open. Unset token is allowed
only for loopback binds and logs a warning. Errors are JSON: `{"error":"…"}`.

See [`docs/API.md`](docs/API.md) for the full contract.

## Exit codes

| Code | Meaning |
| --- | --- |
| 0 | No selected finding class (pending is allowed unless asked for). |
| 1 | A selected finding class is present: `differences` (`mismatched`/`failed`, the default), `orphans` or `unknown`. |
| 2 | `pending` is selected (`--strict` or `--fail-on pending`) and something is still pending. |
| 69/70/71/73/74/78 | Chain error / internal / bind / write / stdout / config (sysexits-style). |

`scan --fail-on <classes>` chooses which findings turn red, so a CI job can gate on money
differences and orphaned transfers while treating unknown records as warnings, or run
`--fail-on none` for a report-only nightly job.

## Testing

```bash
cargo test              # unit + CLI + HTTP integration tests
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

Coverage: amount and timestamp parsing, config validation, topic/hex/ABI decoding, all five
outcomes, log-search recovery, orphan detection, duplicate claims, stale records, `kite-x402-hono`
receipt normalisation, `capture → scan` round trip, bearer auth, JSON shape, exit codes. See
[`docs/TESTING.md`](docs/TESTING.md) and the recorded runs in
[`docs/EVIDENCE.md`](docs/EVIDENCE.md).

## Documentation map

| Document | Contents |
| --- | --- |
| [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) | layout, module responsibilities, key flows, extension points |
| [`docs/CLI.md`](docs/CLI.md) | every command, option, env override and exit code |
| [`docs/API.md`](docs/API.md) | HTTP routes, auth, response shape, errors |
| [`docs/TESTING.md`](docs/TESTING.md) | coverage map, determinism rules, fixture contract |
| [`docs/DEPLOYMENT.md`](docs/DEPLOYMENT.md) | build, systemd/cron sketches, container, failure modes |
| [`docs/EVIDENCE.md`](docs/EVIDENCE.md) | dated records: CI, mock run, hono dump, duplicate claims, real testnet round trip |
| [`docs/SUBMISSION.md`](docs/SUBMISSION.md) | review bundle: repo, Open Dev Data status, commits, test results, demo commands |
| [`CHANGELOG.md`](CHANGELOG.md) | version history |
| [`CONTRIBUTING.md`](CONTRIBUTING.md) | local dev, review expectations, commit style |

## Known limitations

- **Read-only by design**: no key handling, no settlement retry, no state storage.
- **No UI**: JSON/text/markdown/CSV only. The HTTP API is for dashboards and probes, not a console.
- **Gateway adapters are thin**: the file JSONL, the `kite-x402-hono` receipt dump and the generic
  HTTP endpoint are implemented; no other gateway's private store shape is supported yet.
- **Log search is block-window based**, not timestamp-optimised: a very old record needs a larger
  `search_lookback_blocks` (bounded to 200 000) and a provider that allows that range for `eth_getLogs`.
- **No websocket subscriptions**: reconciliation is pull-based (`scan` or polling `serve`).
- **Token decimals come from config or `eth_call`** at probe time; if the contract is non-standard,
  set `chain.token_decimals` explicitly.
- **Mainnet is untested by this repo's CI**: mainnet parameters are presets only; verify the token
  contract yourself before pointing it at production.
- **Fixture provenance is mixed and labelled**: `ledger.jsonl`, `chain_mock.json`,
  `hono_receipts.jsonl` and `ledger_duplicate.jsonl` are synthetic; `kite_testnet_capture.jsonl`
  is two real testnet transfers captured 2026-11-22 (public data, reused address, not this
  project's wallet).
- **`capture` is not a settlement service**: it never signs, never retries, and its `observed_at`
  is capture time, not payment time.

## License

Apache-2.0. Not affiliated with or endorsed by Kite AI or the x402 project.
