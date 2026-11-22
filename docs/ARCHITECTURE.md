# Architecture

## Goals

1. Answer "did this settlement actually land?" with evidence, not guesses.
2. Never mutate chain state, never hold a key, never replay a settlement.
3. Make the mock/live boundary impossible to blur: fixtures are labelled everywhere.
4. Keep each module replaceable — a new chain, ledger source or transport should not touch the
   reconciliation rules.

## Layers

```
            ┌──────────────┐   ┌───────────┐
  CLI ──────►  reconcile   ◄──►│  report   │──► stdout / file / HTTP
  HTTP ─────┘       │        └───────────┘
                    │
          ┌─────────┴─────────┐
          ▼                   ▼
   ledger::LedgerSource   chain::ChainClient
   (FileLedger,           (RpcClient,
    HonoReceiptFile,       mock::FixtureChain)
    HttpLedger,
    MemoryLedger)
```

- `reconcile` is the only module that knows the rules. It depends on two traits, never on concrete
  transports.
- `chain` owns JSON-RPC and decoding (`Transfer` topic, hex, ABI `string`).
- `ledger` owns ingestion and normalisation into `LedgerRecord`.
- `model` owns the serialisable domain types (`LedgerRecord`, `Outcome`, `Report`, `Summary`).
- `report` renders; `server` and `main` are shells.
- `config` validates everything before any I/O starts (fail fast, actionable messages).
- `mock` implements `ChainClient` from a JSON fixture, so tests and demos need no network.

## Directory structure

```
src/
  main.rs        CLI (clap): probe / scan / capture / serve, logging to stderr
  lib.rs         module wiring + config → (chain, ledger, reconciler) builders
  model.rs       domain types + parsing helpers
  error.rs       ReconError taxonomy (no panics on untrusted input)
  config.rs      TOML + RECON_* env overrides + validation
  chain.rs       ChainClient trait, RpcClient (JSON-RPC), decoding helpers
  mock.rs        FixtureChain from fixtures/chain_mock.json
  capture.rs     Chain → ledger bootstrap: transfers become ledger records
  ledger.rs      LedgerSource trait, FileLedger (JSONL), HonoReceiptFile
                 (kite-x402-hono SettlementReceipt export), HttpLedger, MemoryLedger
  reconcile.rs   Reconciler: per-record judgement, duplicate-claim detection, orphan detection
  report.rs      text / json / markdown rendering + exit codes
  server.rs      axum router, bearer auth, AppState
fixtures/
  ledger.jsonl                 one record per outcome, synthetic
  chain_mock.json              matching receipts and transfers, synthetic
  hono_receipts.jsonl          gateway receipt dump, synthetic
  ledger_duplicate.jsonl       two invoices claiming one transfer, synthetic
  kite_testnet_capture.jsonl   two real testnet transfers, captured 2026-11-22
tests/
  cli_scan.rs       black-box CLI runs against fixtures (JSON, strict exit codes)
  cli_capture.rs    black-box capture → scan round trip
  http_api.rs       router tests: health, auth, report shape
docs/
```

## Key flows

### `capture`

1. `chain.tip()` once (unless `--to-block` was given) and derive the range.
2. `find_transfers` for the configured token and receiver over that range; the RPC filter is
   already narrowed to the receiver so unrelated volume never enters the process twice.
3. `capture::ledger_from_transfers` maps each transfer to a `LedgerRecord` with the lowercased
   tx hash as id (`#2`, `#3` suffixes when one transaction emits several transfers).
4. Write JSONL; nothing is signed, nothing is retried.

### `scan`

1. `Config::load` — TOML, env overrides, validation (receiver address, ranges, URL shapes).
2. Build chain reader by mode. `live` → `RpcClient`; `mock` → `FixtureChain`.
3. Build ledger source by kind; token resolved from the named env var, only then.
4. `chain.tip()` once per run — every confirmation count is relative to this tip.
5. For each record (capped by `max_records`):
   - parse amount → reject non-digits/overflow → `unknown`;
   - `pay_to` must equal `reconcile.receiver` (case-insensitive) → else `unknown`;
   - `observed_at` must parse; in live mode it must be inside `window_minutes`;
   - with `tx_hash`: validate shape → fetch receipt → `failed` if reverted, else inspect logs;
   - without `tx_hash`: search `Transfer` logs in `search_lookback_blocks`.
6. Logs are filtered by `to == receiver` and, when the record has a payer, `from == payer`.
   A transfer of `amount ≥ expected` marks the record; confirmations = `tip - block + 1`.
7. Duplicate claims: a tx hash already claimed by an earlier record makes the later record
   `unknown` (one transfer cannot pay two invoices); `summary.duplicates` counts them.
8. Orphans: transfers to the receiver in the same window not claimed by any record's tx hash,
   capped at 50.
9. Render, emit exit code, optionally write `--out`.

### `serve`

Same reconciler, executed per request (no background cache). Auth is a constant-time comparison of
the bearer token from the configured env var. The response is the identical `Report` shape as the
CLI, so a CI job and a dashboard can share one parser.

## Design decisions

- **Trait objects over generics** at the boundary: `Arc<dyn ChainClient + Send + Sync>` keeps
  async lifetimes simple and lets tests swap in fixtures without generic noise in the CLI.
- **Amounts as `u128` + decimal string in JSON**: no float rounding, JSON round-trips preserved.
- **`Outcome::Unknown` instead of errors for bad records**: one broken ledger row must not abort a
  batch; the reason is carried in the report.
- **No silent mode fallback**: mock output always says `mode=mock`; live never reads fixtures.
- **Time is injected**: `report(ledger, now)` receives `now` so tests are deterministic and
  fixture timestamps do not rot.
- **Deterministic ordering**: transfers are sorted newest-first; orphans are capped; report order
  follows ledger order.

## Extension points

| Want to… | Touch |
| --- | --- |
| Support another chain | `chain.rs` (RPC shape) + config presets; rules stay unchanged. |
| Add a gateway-specific ledger | new `LedgerSource` impl in `ledger.rs`; normalise into `LedgerRecord`. |
| Change matching semantics | `reconcile.rs` only; document the reason strings. |
| Add a transport | `server.rs` or a new binary that reuses `lib.rs` builders. |

## Local development

```bash
cargo fmt
cargo clippy --all-targets -- -D warnings
cargo test
cargo run -- --config recon.example.toml scan --format markdown
```

Windows note: the CLI writes reports to stdout via `write_all`, and logs to stderr, so
`--format json` can be piped into `jq` safely.
