# kite-x402-recon

Read-only settlement reconciliation for [x402](https://x402.org) services that settle on the
**Kite** chain. You give it two inputs — a list of settlements your gateway *believes* happened,
and a chain window — and it tells you, per record, whether the money actually moved.

```
$ recon --config recon.example.toml scan
mode=mock chain_id=2368 tip=1000 window=60m generated_at=...
total=7 matched=2 pending=1 mismatched=1 failed=1 unknown=2 orphans=1
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
| `recon probe` | Verify RPC reachability, chain id, tip and token metadata (symbol/decimals). |
| `recon scan` | Reconcile one ledger batch against the chain window; render `text`, `json` or `markdown`. |
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

When a ledger record has no `tx_hash`, the reconciler searches ERC-20 `Transfer` logs for
`(payer, payTo, amount ≥ expected)` inside `search_lookback_blocks` and reports the record as
matched with `"recovered_by": "log_search"`, or as pending if several candidates are ambiguous.

## Relation to KiteAI / x402

- **Chain**: reads Kite testnet (`chain_id 2368`, `https://rpc-testnet.gokite.ai/`) or mainnet
  (`2366`) presets; every RPC call is plain JSON-RPC (`eth_blockNumber`,
  `eth_getTransactionReceipt`, `eth_getLogs`, `eth_call`) implemented in
  [`src/chain.rs`](src/chain.rs).
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
`total=7 matched=2 pending=1 mismatched=1 failed=1 unknown=2 orphans=1`. Mock mode is labelled in
every output (`mode=mock`) and `scan` prints a note — a fixture run can never be mistaken for
on-chain evidence.

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
| `ledger.kind` | `file` (JSONL) or `http` (gateway endpoint returning an array or `{"records":[…]}`). |

## Ledger record format

One JSON object per line (or per array element over HTTP):

```json
{"id":"inv_1","pay_to":"0x…","payer":"0x…","amount_atomic":"10000","tx_hash":"0x…","observed_at":"2026-11-22T10:00:00Z"}
```

- `amount_atomic` is a plain decimal string in the token's smallest unit. No signs, dots or separators.
- `observed_at` is RFC 3339.
- `tx_hash` and `payer` are optional; without `tx_hash` the log search path is used.
- The HTTP response may be a bare array or `{"records":[…]}`; both are accepted.

## HTTP API

`recon serve` (default `127.0.0.1:8081`):

| Route | Behavior |
| --- | --- |
| `GET /healthz` | `{"status":"ok","mode":…,"chain_id":…,"auth":"bearer"\|"disabled"}` |
| `GET /api/v1/reconcile` | Full report JSON, same shape as `scan --format json`; `Cache-Control: max-age=<refresh_seconds>` |

Auth: when `RECON_API_TOKEN` (the variable named by `server.api_token_env`) is set, `/api/v1/reconcile`
requires `Authorization: Bearer <token>`; `/healthz` stays open. Unset token is allowed only for
loopback binds and logs a warning. Errors are JSON: `{"error":"…"}`.

See [`docs/API.md`](docs/API.md) for the full contract.

## Exit codes

| Code | Meaning |
| --- | --- |
| 0 | No differences (pending is allowed unless `--strict`). |
| 1 | At least one `mismatched` or `failed` record. |
| 2 | `--strict` and something is still `pending`. |
| 69/70/71/73/74/78 | Chain error / internal / bind / write / stdout / config (sysexits-style). |

## Testing

```bash
cargo test              # unit + CLI + HTTP integration tests
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

Coverage: amount and timestamp parsing, config validation, topic/hex/ABI decoding, all five
outcomes, log-search recovery, orphan detection, stale records, bearer auth, JSON shape, exit
codes. See [`docs/TESTING.md`](docs/TESTING.md) and the recorded runs in
[`docs/EVIDENCE.md`](docs/EVIDENCE.md).

## Known limitations

- **Read-only by design**: no key handling, no settlement retry, no state storage.
- **No UI**: JSON/text/markdown only. The HTTP API is for dashboards and probes, not a console.
- **Gateway adapters are thin**: the file JSONL and the generic HTTP endpoint are implemented; a
  dedicated adapter for a specific gateway's internal store is not. The HTTP source expects the
  documented shape.
- **Log search is block-window based**, not timestamp-optimised: a very old record needs a larger
  `search_lookback_blocks` (bounded to 200 000) and a provider that allows that range for `eth_getLogs`.
- **No websocket subscriptions**: reconciliation is pull-based (`scan` or polling `serve`).
- **Token decimals come from config or `eth_call`** at probe time; if the contract is non-standard,
  set `chain.token_decimals` explicitly.
- **Mainnet is untested by this repo's CI**: mainnet parameters are presets only; verify the token
  contract yourself before pointing it at production.
- Fixtures are synthetic; they are not recorded mainnet/testnet data.

## License

Apache-2.0. Not affiliated with or endorsed by Kite AI or the x402 project.
