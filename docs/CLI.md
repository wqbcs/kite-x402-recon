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
# mode=mock chain_id=2368 tip=1000 token=USDT decimals=6 receiver=0x2222…

recon --config recon.toml probe --json
# {"mode":"mock","chain_id":2368,"rpc_url":"…","tip":1000,"tip_time":"…",
#  "token_address":"0x…","token_symbol":"USDT","token_decimals":6,
#  "receiver":"0x…","ledger":"file"}
```

- `tip` requires a chain call; in `live` mode an unreachable RPC exits `69`.
- `token_symbol` / `token_decimals` failures are warnings, not failures (non-standard tokens).
- `tip_time` is the local observation time, not block time.

## `recon scan`

Reconciles one ledger batch.

| Option | Default | Meaning |
| --- | --- | --- |
| `--format text\|json\|markdown` | `text` | Output rendering. |
| `--strict` | off | Exit `2` when anything is `pending`. |
| `--out <path>` | – | Also write the rendered report to this file. |
| `--config` | see above | Configuration. |

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
