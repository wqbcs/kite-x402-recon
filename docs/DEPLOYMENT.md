# Deployment

The deliverable is a single static-ish binary plus one config file. No database, no migrations.

## Build

```bash
cargo build --release            # target/release/recon
cargo test --release             # optional sanity on the optimised build
```

Artifact: `target/release/recon` (≈ a few MB after `strip = true` and `lto = "thin"`).

## Configuration for a live deployment

```toml
[chain]
mode = "live"
rpc_url = "https://rpc-testnet.gokite.ai/"   # or your own proxy in front of it
chain_id = 2368
token_address = "0x…"                        # copy from the official contract list, verify on kitescan
token_decimals = 6
min_confirmations = 5
timeout_ms = 15000

[ledger]
kind = "http"
url = "https://gateway.internal/settlements"
bearer_token_env = "RECON_LEDGER_TOKEN"

[reconcile]
receiver = "0x…"                             # your pay-to address
window_minutes = 120
max_records = 500
search_lookback_blocks = 5000

[server]
bind = "127.0.0.1:8081"
api_token_env = "RECON_API_TOKEN"
refresh_seconds = 30
```

Secrets:

```bash
export RECON_LEDGER_TOKEN=…   # read by the ledger client, never logged
export RECON_API_TOKEN=…      # bearer token for /api/v1/reconcile
```

## Run as a service (systemd sketch)

```ini
[Unit]
Description=kite-x402-recon
After=network-online.target

[Service]
User=recon
Environment=RUST_LOG=info
EnvironmentFile=/etc/kite-x402-recon/env      # 0600, contains the two tokens
ExecStart=/usr/local/bin/recon --config /etc/kite-x402-recon/recon.toml serve
Restart=on-failure
NoNewPrivileges=true
PrivateTmp=true
ProtectSystem=strict
ProtectHome=true

[Install]
WantedBy=multi-user.target
```

The process only needs outbound HTTPS to the RPC and ledger endpoints; no elevated privileges.

## Scheduled reconciliation (cron / CI)

```bash
*/5 * * * * /usr/local/bin/recon --config /etc/kite-x402-recon/recon.toml \
  scan --format json --out /var/log/recon/last.json \
  >> /var/log/recon/cron.log 2>&1
```

Exit codes are the contract: `1` = differences (page someone), `2` = pending under `--strict`
(informational), `0` = clean. Codes are documented in [`docs/CLI.md`](CLI.md).

## Recovering a missing ledger

If the gateway lost its ledger, rebuild the evidence from the chain before auditing:

```bash
recon --config /etc/kite-x402-recon/recon.toml capture \
  --window-minutes 1440 \
  --out /var/lib/recon/captured-$(date +%F).jsonl
```

- Prefer `--window-minutes` over a hand-computed `--from-block`: it measures the chain's block
  time, so the range matches the audit window even when block production is irregular.
- Capture and audit must cover overlapping ranges: an `orphan` is money inside the audit window
  but outside the capture range, which is the intended signal, not noise. Keep
  `RECON_LOOKBACK_BLOCKS` near the captured block count to make the two ranges agree.
- Captured `observed_at` is capture time. Keep the file: it is a dated snapshot, and re-running
  capture later will include newer transfers.
- `capture` is read-only like everything else; it cannot move funds or repair a payment.

## Container (optional)

```dockerfile
FROM rust:1.88 AS build
WORKDIR /src
COPY . .
RUN cargo build --release

FROM debian:bookworm-slim
RUN useradd -r -u 10001 recon
WORKDIR /app
COPY --from=build /src/target/release/recon /usr/local/bin/recon
COPY recon.example.toml /app/recon.toml
COPY fixtures /app/fixtures
USER recon
EXPOSE 8081
ENTRYPOINT ["recon", "--config", "/app/recon.toml"]
CMD ["serve", "--bind", "0.0.0.0:8081"]
```

Bake no secrets into the image; pass `RECON_LEDGER_TOKEN` / `RECON_API_TOKEN` at runtime.

Probes: point liveness at `GET /healthz` (process only) and readiness at `GET /readyz` (reads a chain
tip and parses the ledger; `503` while either is broken). Compose:

```yaml
healthcheck:
  test: ["CMD", "curl", "-fsS", "http://127.0.0.1:8081/readyz"]
  interval: 60s
  timeout: 10s
  retries: 3
  start_period: 15s
```

A liveness-only check would keep an instance marked healthy through an RPC outage and fail every
report request it receives; readiness drains it instead, and the container restarts only when the
process itself is stuck.

## Network surface

| Direction | Destination | Required |
| --- | --- | --- |
| outbound | RPC endpoint (`chain.rpc_url`) | yes in live mode |
| outbound | ledger endpoint (`ledger.url`) | only for `kind = "http"` |
| inbound | `server.bind` | only for `serve`; default loopback |

Nothing else is contacted. There is no telemetry.

## Failure modes

| Symptom | Cause | Action |
| --- | --- | --- |
| exit 78 | config invalid/missing | message names the field; fix the TOML/env |
| exit 69 on `probe` | RPC unreachable/rate-limited | check provider status; the message carries the HTTP status |
| all records `pending`, no receipts | gateway submitted nothing, or you query the wrong network | compare `chain_id` and `token_address` with the gateway |
| all records `unknown`, reason "does not match receiver" | `reconcile.receiver` typo | fix the address; comparisons are case-insensitive |
| `eth_getLogs` range errors | provider cap below `search_lookback_blocks` | lower the window or use a provider with a larger range |
| HTTP 502 on `/api/v1/reconcile` | ledger or RPC failed | read the `error` field; it names the failing step |

## Upgrade / rollback

Stateless: deploy the new binary, restart, done. Rollback = previous binary + unchanged config.
Keep `CHANGELOG.md` accurate so the config delta is visible before deploying.
