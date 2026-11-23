# CLI reference

Binary: `recon`. Global option:

| Option | Env | Meaning |
| --- | --- | --- |
| `--config <path>` | `RECON_CONFIG` | Path to `recon.toml`. If omitted, `./recon.toml` is used when it exists, otherwise built-in defaults (mock mode). |

Logs go to stderr (`RUST_LOG`, default `info`). Report output goes to stdout.

## `recon probe`

Checks the configured chain reader without touching the ledger.

| Option | Meaning |
| --- | --- |
| `--json` | Emit one JSON object instead of a single text line. |

```bash
recon --config recon.toml probe
# mode=mock chain_id=2368 tip=1000 block_s=12 token=USDT decimals=6 receiver=0x2222…

recon --config recon.toml probe --json
# {"mode":"mock","chain_id":2368,"rpc_url":"…","tip":1000,"tip_time":"…",
#  "block_seconds":12,"token_address":"0x…","token_symbol":"USDT",
#  "token_decimals":6,"receiver":"0x…","ledger":"file"}
```

- `tip` requires a chain call; in `live` mode an unreachable RPC exits `69`.
- `block_seconds` is the measured average block time (tip vs. up to 1000 blocks back), the same
  measurement `capture --window-minutes` and `scan --align-window` use. It is the number that tells
  you whether `search_lookback_blocks` is sane for this chain: 2000 blocks is ~7 hours on a 12 s
  chain and ~20 days on Kite testnet (886 s). `null` / `-` means it could not be measured.
- `token_symbol` / `token_decimals` failures are warnings, not failures (non-standard tokens).
- `tip_time` is the local observation time, not block time.

## `recon scan`

Reconciles one ledger batch.

| Option | Default | Meaning |
| --- | --- | --- |
| `--format text\|json\|markdown` | `text` | Output rendering. |
| `--strict` | off | Exit `2` when anything is `pending`. |
| `--out <path>` | – | Also write the rendered report to this file. |
| `--at-block <n>` | current tip | Audit as of this block: the aligned range, orphan scan and confirmation counts all move with it, so a past window can be replayed later with the same verdicts. Refused (`78`) when ahead of the tip. |
| `--align-window` | on | Force alignment for this run: search a block range derived from `reconcile.window_minutes` and the chain's measured block time instead of `reconcile.search_lookback_blocks`. Only needed when config or `RECON_ALIGN_WINDOW` turned it off. |
| `--config` | see above | Configuration. |

Alignment is on by default (`reconcile.align_window = true`, `RECON_ALIGN_WINDOW=true`) and the
served HTTP report uses the same setting, so a CLI run and a `/api/v1/reconcile` call answer the
same question about the same period. It makes the searched range and the audit window describe
the same period. Without it, the two use different units and a slow chain silently widens the
sweep: on Kite testnet (886 s/block) `search_lookback_blocks = 2000` reaches back ~20 days, so a
24-hour audit reported 15 orphans that were all older than the window. Aligned, the same ledger
and chain report 0. Alignment needs two `eth_getBlockByNumber` reads (tip and tip−1000) the first time it sees a
tip; the pair is cached per tip, so `serve` does not repeat it per request. Set
`RECON_ALIGN_WINDOW=false` if that call is rate-limited, but then set `search_lookback_blocks`
from a measured `block_seconds` yourself.

Examples:

```bash
# Human summary, exit 1 because the fixture contains differences
recon --config recon.toml scan

# Machine-readable, pipe into jq
recon --config recon.toml scan --format json | jq '.summary'

# Markdown table for an issue or a submission record
recon --config recon.toml scan --format markdown --out report.md

# CI gate: also fail when nothing is final yet
recon --config recon.toml scan --strict

# Audit a one-day window: search exactly the blocks that one day covers
recon --config recon.toml scan --align-window
```

Environment shortcuts override the TOML for one run, which is how CI and cron jobs usually
configure this:

```bash
RECON_CHAIN_MODE=live \
RECON_CHAIN_ID=2368 \
RECON_TOKEN_ADDRESS=0x… \
RECON_RECEIVER=0x… \
RECON_LEDGER_KIND=http \
RECON_LEDGER_URL=https://gateway.example.com/settlements \
recon scan --format json
```

`RECON_LEDGER_KIND` accepts `file`, `hono_file`/`hono` (a `kite-x402-hono` `SettlementReceipt`
JSONL dump) and `http`. Both file-backed kinds use `RECON_LEDGER_PATH`; `hono_file` ignores the
gateway's `success` flag and verifies each hash on chain.

```bash
RECON_LEDGER_KIND=hono_file \
RECON_LEDGER_PATH=fixtures/hono_receipts.jsonl \
RECON_CHAIN_MODE=mock \
recon --config recon.example.toml scan
```

## `recon serve`

| Option | Meaning |
| --- | --- |
| `--bind <addr>` | Override `server.bind` for this run. |

```bash
RECON_API_TOKEN=change-me recon --config recon.toml serve --bind 127.0.0.1:8081
curl -s localhost:8081/healthz
curl -s -H "Authorization: Bearer change-me" localhost:8081/api/v1/reconcile | jq '.summary'
```

Stops on Ctrl-C / SIGTERM with a graceful shutdown.

## `recon capture`

Writes a ledger file from the token transfers a block range contains. Use it when there is no
gateway ledger to audit (or it was lost), or to seed a regression fixture from real settlements.

| Option | Default | Meaning |
| --- | --- | --- |
| `--from-block <n>` | `to_block - search_lookback_blocks` | First block, inclusive. |
| `--window-minutes <n>` | off | Derive `from_block` from an audit window using the chain's measured block time (two `eth_getBlockByNumber` calls). Conflicts with `--from-block`. |
| `--to-block <n>` | current tip | Last block, inclusive. |
| `--receiver <addr>` | `reconcile.receiver` | Keep only transfers paid to this address. |
| `--out <path>` | required | JSONL destination; stdout only carries the one-line summary. |

```bash
recon --config recon.toml capture --from-block 22045179 --to-block 22045329 \
  --receiver 0x66336f24e38b5b155cd3b50f635b1bb0f1098b1d --out captured.jsonl
# mode=live chain_id=2368 blocks=22045179..22045329 receiver=0x6633…8b1d records=2 out=captured.jsonl

recon --config recon.toml capture --window-minutes 1440 --out captured-24h.jsonl
# mode=live chain_id=2368 blocks=22045237..22045335 window=1440m avg_block_s=886 records=1 out=…
```

- `--window-minutes` is measured, not assumed: the tool samples the tip block and a block up to
  1000 back, then rounds the block count up so the range can only be wider, never short.
  On Kite testnet (2026-11-23) this measured ~886 s per block, i.e. a 24-hour window was only
  ~97 blocks — a fixed `search_lookback_blocks` would have been wildly oversized.
- When the derived block count is smaller than `reconcile.search_lookback_blocks`, a warning is
  logged: with alignment on (the default) `scan` now searches the same measured period, so the
  warning is informational; with `RECON_ALIGN_WINDOW=false` the larger fixed range still applies,
  and transfers outside the captured range will surface as orphans.

- Capture-to-scan replay: the summary prints `blocks=A..B`. Scanning the same file with
  `scan --at-block B` and the same `--window-minutes` searches exactly that captured range
  (anchored measurement at B reproduces the same block count), so a captured window can be
  re-checked later without drift. `tests/cli_capture.rs` pins this recipe.
- `observed_at` is the capture time, not the settlement time; `tx_hash` ties the record to the
  transfer. This is documented in the README and in `src/capture.rs`.
- The written file is exactly `ledger.kind = "file"` input, so `scan` accepts it unchanged.
- A block range greater than `reconcile.max_records` is capped by that limit, and capture exits
  `0` even when zero transfers were found (an empty receiver is a valid answer).
- Exit codes: `78` bad block range / receiver / missing token address, `69` RPC failure, `73`
  cannot write the file.

## Exit codes

| Code | Meaning |
| --- | --- |
| 0 | Clean (pending allowed unless `--strict`). |
| 1 | Differences: at least one `mismatched` or `failed`. |
| 2 | `--strict` and at least one `pending`. |
| 69 | Chain/RPC failure. |
| 70 | Internal failure (serialisation, server crash). |
| 71 | Cannot bind the API address. |
| 73 | Cannot write `--out`. |
| 74 | Cannot write to stdout. |
| 78 | Configuration invalid or unreadable. |

`scan` always finishes a full pass before exiting, so exit code `1` still comes with a complete
report of every record.
