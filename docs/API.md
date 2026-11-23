# HTTP API

`recon serve` exposes a read-only API. There is no write endpoint, no settlement submission and
no key material in the process.

Base URL: whatever `server.bind` (default `127.0.0.1:8081`) resolves to.

## Authentication

`server.api_token_env` (default `RECON_API_TOKEN`) names the environment variable that holds the
token.

| Token set | Behavior |
| --- | --- |
| yes | `/api/v1/reconcile` requires `Authorization: Bearer <token>`; wrong or missing → `401`. |
| no | API is open. Intended for loopback only; the server logs a warning at startup. |

Comparisons are constant-time. `/healthz` never requires auth (probes must work without secrets).

## `GET /healthz`

`200 OK`, no body schema beyond:

```json
{
  "status": "ok",
  "service": "kite-x402-recon",
  "version": "0.1.0",
  "mode": "mock",
  "chain_id": 2368,
  "auth": "bearer"
}
```

`auth` is `"bearer"` or `"disabled"`. This endpoint does not call the chain or the ledger.

## `GET /api/v1/reconcile`

Runs one reconciliation pass with `now = server time` and returns the report.

Optional query parameter `at_block=<n>` evaluates the ledger as of that block instead of the
current tip — the HTTP equivalent of `scan --at-block`. The anchored range, orphan scan and
confirmation counts all move with it, and `anchored` becomes `true` in the response, so a dashboard
can render a historical audit without a second deployment. `at_block=0`, a non-numeric value, or a
block ahead of the live tip returns `400` with `{"error": "..."}`. Each anchored request is a clone
of the shared reconciler, so concurrent live requests are unaffected.

Success: `200 OK`, `Content-Type: application/json`,
`Cache-Control: max-age=<server.refresh_seconds>`.

```json
{
  "mode": "mock",
  "chain_id": 2368,
  "generated_at": "2026-11-22T15:31:30Z",
  "window_minutes": 60,
  "chain_tip": 1000,
  "anchored": false,
  "search": { "from_block": 700, "to_block": 1000, "lookback_blocks": 300, "block_seconds": 12 },
  "summary": {
    "total": 7, "matched": 2, "pending": 1, "mismatched": 1,
    "failed": 1, "unknown": 2, "orphans": 1, "duplicates": 0
  },
  "records": [
    {
      "id": "inv_ok",
      "pay_to": "0x2222…",
      "payer": "0x4444…",
      "amount_atomic": "10000",
      "outcome": "matched",
      "reason": "transfer of 10000 atomic units confirmed at block 990",
      "tx_hash": "0x1111…",
      "block_number": 990,
      "confirmations": 11
    }
  ],
  "orphans": [
    {
      "tx_hash": "0x6666…",
      "block_number": 999,
      "from": "0x9999…",
      "to": "0x2222…",
      "amount_atomic": "25000"
    }
  ]
}
```

Field notes:

- `mode` is always present; `mock` means the numbers came from a fixture, never from a chain.
- `anchored` is `true` when `chain_tip` is an explicit `--at-block` anchor rather than the live tip,
  so an old-looking tip in a report is explainable instead of being read as a stalled chain.
- `search` is the block range the verdicts came from. Without it a reader cannot tell "the money is
  missing" from "the search stopped early", so treat it as part of the result rather than metadata.
  `block_seconds` is the measured block time that produced the range (`null` when alignment is off
  or the chain could not be measured); it keeps the range auditable instead of magic.
- `serve` builds its reconciler from the same config as the CLI, so window alignment applies here
  too: a `window_minutes=60` deployment searches what an hour actually covers on that chain rather
  than the raw `search_lookback_blocks` default (2000 blocks ≈ 20 days on Kite testnet). The
  measurement is cached for the last eight tips, so repeated live and replayed requests do not each
  pay for two block reads.
- `outcome` ∈ `matched | pending | mismatched | failed | unknown`.
- `recovered_by` appears only when the log-search path produced the match (`"log_search"`).
- `tx_hash`, `payer`, `block_number`, `confirmations` are omitted when not applicable.
- `summary.orphans` equals `orphans.length`.
- `summary.duplicates` counts records rejected because their `tx_hash` was already claimed by an
  earlier record; those records are counted inside `summary.unknown`.

## Errors

All errors are JSON: `{"error": "<message>"}`.

| Status | When |
| --- | --- |
| 401 | Missing/invalid bearer token. |
| 502 | Ledger read or chain RPC failed; message names the failing step. |
| 500 | Serialisation failure (should not happen; reported rather than panicking). |

There is no request body, so no 400 class beyond auth. Retries are the client's business; the
endpoint is idempotent per `now`.

## Example

```bash
export RECON_API_TOKEN=dev-token
recon --config recon.toml serve &
curl -s localhost:8081/healthz
curl -s -H "Authorization: Bearer dev-token" localhost:8081/api/v1/reconcile \
  | jq '{mode, summary}'
```

## Contract tests

`tests/http_api.rs` exercises the router directly (no port binding): health, 401 without token,
200 with token, and the exact summary of the bundled fixtures.
