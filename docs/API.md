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

Comparisons are constant-time. `/healthz` and `/readyz` never require auth (probes must work
without secrets).

## `GET /healthz`

Liveness. `200 OK`, no body schema beyond:

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

`auth` is `"bearer"` or `"disabled"`. This endpoint does not call the chain or the ledger, so it
stays cheap and answers even while an upstream is down.

## `GET /readyz`

Readiness: it touches both inputs a report needs instead of trusting the process state.

```json
{
  "status": "ready",
  "chain_id": 2368,
  "tip": 1000,
  "ledger": "memory",
  "records": 7
}
```

`200 OK` means a chain tip was read **and** the ledger parsed. If the RPC is unreachable or the
ledger cannot be read, the answer is `503` with the failing side named so an orchestrator can keep
the instance out of rotation:

```json
{ "status": "unavailable", "reason": "chain: rpc error: 429 Too Many Requests" }
```

Wire `/healthz` to a liveness probe and `/readyz` to a readiness probe. A load balancer pointed at
`/healthz` alone would keep sending report requests to a process whose RPC has been rate-limited;
pointed at `/readyz`, it drains that instance instead.

## `GET /api/v1/reconcile`

Runs one reconciliation pass with `now = server time` and returns the report.

Optional query parameter `at_block=<n>` evaluates the ledger as of that block instead of the
current tip — the HTTP equivalent of `scan --at-block`. The anchored range, orphan scan and
confirmation counts all move with it, and `anchored` becomes `true` in the response, so a dashboard
can render a historical audit without a second deployment. `at_block=0`, a non-numeric value, or a
block ahead of the live tip returns `400` with `{"error": "..."}`. Each anchored request is a clone
of the shared reconciler, so concurrent live requests are unaffected.

Optional query parameter `format=csv` returns the same RFC 4180 export as `scan --format csv`
instead of JSON: `Content-Type: text/csv; charset=utf-8` plus
`Content-Disposition: attachment; filename="reconcile-<from>-<to>.csv"`, so the window is named in
the download. `format=json` (or omitting it) keeps the default. Any other value is a `400` rather
than a silent fallback to JSON.

Success: `200 OK`, `Content-Type: application/json` (or `text/csv; charset=utf-8` for
`format=csv`), `Cache-Control: max-age=<server.refresh_seconds>`, and `X-Recon-Cache` telling you
how the body was produced:

| `X-Recon-Cache` | Meaning |
| --- | --- |
| `miss` | Reconciled for this request from a chain tip read and a ledger parse; stored for the rest of the window. |
| `hit` | The report retained for this `refresh_seconds` window; byte-identical to the `miss` that produced it. |
| `bypass` | A replayed window (`at_block`); always reconciled, never cached. |

The service keeps one report for the window it advertises, so a dashboard polling faster than
`refresh_seconds` sees identical bodies and the RPC sees one read per window instead of one per poll.
Concurrent polls wait for the single in-flight reconciliation rather than each starting their own.

Failures:

| Status | When | Body |
| --- | --- | --- |
| `401` | `server.api_token_env` is set and the bearer token is missing or wrong | `{"error":…}` |
| `400` | `format` is neither `json` nor `csv`, or `at_block` is not a positive integer / is ahead of the tip | `{"error":…}` |
| `502` | The chain or the ledger answered with an error | `{"error":…}` |
| `504` | The reconciliation did not finish within `server.request_timeout_seconds` | `{"error":"reconciliation did not finish within 15s"}` |

`504` is deliberate rather than `502`: the inputs are not known to be broken, the service just could
not finish reading them. Nothing is cached on that path, so the next poll tries again.

```json
{
  "mode": "mock",
  "chain_id": 2368,
  "receiver": "0x2222222222222222222222222222222222222222",
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
- `receiver` is the address the verdicts were taken against, normalized to lowercase. It has to
  travel with the report: a "paid to the wrong address" finding is only meaningful relative to an
  expected address, and two runs with different receivers otherwise look identical.
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

## `GET /api/v1/records/{transaction}`

Answers the support-ticket question — "what happened to this transaction?" — for exactly one
settlement, using the same reconciler and the same receiver/window configuration as a report.

The path segment must be `0x` plus 64 hex digits (either case); anything else is a `400`. The
ledger is searched for a record whose `tx_hash` matches, ignoring case; a hash the ledger never
mentions is a `404` because that is an answer, not an upstream failure. The record is then judged
against the current tip, so the response carries a verdict, a reason, and the block/confirmation
evidence that produced it.

```json
{
  "mode": "mock",
  "chain_id": 2368,
  "receiver": "0x2222222222222222222222222222222222222222",
  "ledger": "memory(7 records)",
  "generated_at": "2026-11-28T02:50:00Z",
  "chain_tip": 1000,
  "record": {
    "id": "inv_ok",
    "pay_to": "0x2222222222222222222222222222222222222222",
    "payer": "0x4444444444444444444444444444444444444444",
    "amount_atomic": "10000",
    "outcome": "matched",
    "reason": "transfer of 10000 atomic units confirmed at block 990",
    "tx_hash": "0x1111111111111111111111111111111111111111111111111111111111111111",
    "block_number": 990,
    "confirmations": 11
  }
}
```

Differences from the window endpoint:

- `Cache-Control: no-store` — confirmations move with every block and the body names the tip it
  was judged at, so a cached copy would silently age.
- No `X-Recon-Cache` header and no `format` parameter: this is one record, not an export.
- The duplicate-claim rule is a cross-record rule, so a single-record answer never reports
  `unknown` because another record also claimed the hash. The windowed report stays authoritative
  for that verdict; the reason text for `unknown` says which case applied.
- `recon inspect <transaction> --format json` returns this same document (same field names, same
  verdict), so a script can move between the CLI and the API without remapping. The CLI reuses the
  same hash normalisation and ledger lookup in `src/lookup.rs` rather than reimplementing them.

## Errors

All errors are JSON: `{"error": "<message>"}`.

| Status | When |
| --- | --- |
| 400 | Invalid `at_block` (zero, non-numeric, ahead of the tip), unsupported `format`, or a `records` path segment that is not a 32-byte hash. |
| 401 | Missing/invalid bearer token. |
| 404 | `/api/v1/records/{transaction}`: the ledger was readable but carries no record with that hash. |
| 502 | Ledger read or chain RPC failed; message names the failing step. |
| 504 | The request outlived `server.request_timeout_seconds` (both endpoints). |
| 500 | Serialisation failure (should not happen; reported rather than panicking). |

There is no request body. Retries are the client's business; the endpoint is idempotent per `now`.

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
200 with token, the exact summary of the bundled fixtures, `at_block` replay and its `400` cases,
the CSV representation (content type, header row, row count and download filename), and the
per-record lookup (`200` for a ledger hash in either case, `400` for a malformed one, `404` for an
unknown one, `401` without a token, `504` under the request deadline, plus `Cache-Control: no-store`).
