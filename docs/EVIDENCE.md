# Evidence log

Concrete, reproducible checks for this repository. Everything below was produced from a clean
checkout of `main` on 2026-11-22.

## Build and checks

| Check | Command | Result |
| --- | --- | --- |
| Format | `cargo fmt --all --check` | clean |
| Lints | `cargo clippy --all-targets -- -D warnings` | clean |
| Tests | `cargo test --all-targets` | 43 passed (35 unit, 2 capture CLI, 3 scan CLI, 3 HTTP), 0 failed |
| Docs | `cargo doc --no-deps` | no warnings |
| CI | GitHub Actions `CI` on push `418a69c` | #3 completed successfully (2m 2s): fmt+clippy+test on ubuntu-latest ✅, windows-latest ✅, rustdoc ✅ |

CI runs: <https://github.com/wqbcs/kite-x402-recon/actions/runs/45039227007> (`f30ed70`),
<https://github.com/wqbcs/kite-x402-recon/actions/runs/45040413110> (`7ab7a80`), and the latest on
`418a69c` on the same Actions page.

## Mock run (no network)

```bash
cargo run -- --config recon.example.toml scan
```

```
mode=mock chain_id=2368 tip=1000 window=60m generated_at=2026-11-22T15:37:52Z
total=7 matched=2 pending=1 mismatched=1 failed=1 unknown=2 orphans=1 duplicates=0
note: mock mode reads fixtures/chain_mock.json; nothing here came from a chain
MATCHED    inv_ok     … amount=10000 tx=0x1111… :: transfer of 10000 atomic units confirmed at block 990
MATCHED    inv_search … amount=10000 tx=0x2222… :: matched by log search (no tx hash in the ledger record)
MISMATCH   inv_short  … amount=10000 tx=0x3333… :: receipt settled 5000 atomic units, 10000 were expected
FAILED     inv_reverted … amount=20000 tx=0x4444… :: transaction reverted on chain
PENDING    inv_pending … amount=3000 tx=0x5555… :: no receipt yet; the gateway may have submitted it very recently
UNKNOWN    inv_wrong_receiver … :: pay_to 0xaaaa… does not match the configured receiver 0x2222…
UNKNOWN    inv_bad_amount … :: invalid amount: 1.5
ORPHAN     block=999 tx=0x6666… from=0x9999… to=0x2222… amount=25000
```

Exit code `1` — differences exist, as the report says. `scan --strict` also exits `2` when only
pending records remain (asserted in `tests/cli_scan.rs`).

## Consuming a `kite-x402-hono` receipt dump

The reference gateway's `SettlementReconciler` keeps receipts in memory and never reads the chain
(`refs/kite-x402-hono/src/settlement-reconciler.ts`), so its `success` flag is only a claim. Our
`hono_file` source reads that export and re-verifies every hash:

```bash
RECON_LEDGER_KIND=hono_file RECON_LEDGER_PATH=fixtures/hono_receipts.jsonl \
RECON_CHAIN_MODE=mock recon --config recon.example.toml scan
```

```
mode=mock chain_id=2368 tip=1000 window=60m generated_at=2026-11-22T16:15:10Z
total=2 matched=1 pending=1 mismatched=0 failed=0 unknown=0 orphans=3 duplicates=0
MATCHED  0x1111… 0x2222… amount=10000 tx=0x1111… :: transfer of 10000 atomic units confirmed at block 990
PENDING  0x5555… 0x2222… amount=3000  tx=0x5555… :: no receipt yet; the gateway may have submitted it very recently
ORPHAN   block=995 tx=0x2222… from=0x5555… to=0x2222… amount=10000
ORPHAN   block=995 tx=0x3333… from=0x4444… to=0x2222… amount=5000
ORPHAN   block=999 tx=0x6666… from=0x9999… to=0x2222… amount=25000
```

Note the second receipt: the gateway recorded `success=false`, the chain shows no receipt yet, and
the tool reports `pending` — it follows the chain, not the claim.

## Duplicate claims (one transfer, two invoices)

```bash
RECON_LEDGER_PATH=fixtures/ledger_duplicate.jsonl RECON_CHAIN_MODE=mock \
recon --config recon.example.toml scan
```

```
total=2 matched=1 pending=0 mismatched=0 failed=0 unknown=1 orphans=3 duplicates=1
MATCHED  inv_first  … tx=0x1111… :: transfer of 10000 atomic units confirmed at block 990
UNKNOWN  inv_second … tx=0x1111… :: transaction hash already claimed by record inv_first; one transfer cannot pay two invoices
```

Orphans are the mirror case (money without an invoice). Duplicate claims are invoices competing for
the same money — the failure mode that makes a naive "sum matched amounts" reconciliation overcount.

## Window-aligned capture (2026-11-23)

The fixed-lookback default does not fit every chain. `capture --window-minutes` measures block
time: it reads the tip block and a block up to 1000 back, computes the average, and rounds the
block count up.

At tip `22045335` on Kite testnet the measurement was **886 seconds per block** (the chain is not a
12-second chain), and `probe` now prints it directly:

```bash
recon --config recon.example.toml probe
# mode=live chain_id=2368 tip=22045335 block_s=886 token=USDT decimals=18 receiver=0x6633…8b1d
```

```bash
RECON_CHAIN_MODE=live RECON_TOKEN_ADDRESS=0x0fF5…7e63 \
recon --config recon.example.toml capture --window-minutes 60 \
  --receiver 0x66336f24e38b5b155cd3b50f635b1bb0f1098b1d --out /tmp/60m.jsonl
# mode=live chain_id=2368 blocks=22045330..22045335 window=60m avg_block_s=886 records=0

recon --config recon.example.toml capture --window-minutes 1440 \
  --receiver 0x66336f24e38b5b155cd3b50f635b1bb0f1098b1d --out /tmp/24h.jsonl
# mode=live chain_id=2368 blocks=22045237..22045335 window=1440m avg_block_s=886 records=1
```

Auditing the 24-hour capture against the chain:

```
total=1 matched=1 pending=0 mismatched=0 failed=0 unknown=0 orphans=15 duplicates=0
MATCHED 0x13a25fa7…fd39e amount=2000000000000000000 :: confirmed at block 22045327
```

Findings that came out of this and are now written into the docs:

- A 60-minute window is only ~4 blocks on this chain, so "nothing captured" is a correct answer,
  not a failure. The 24-hour window captured the real settlement and it matched.
- The 15 orphans are transfers inside `scan`'s 2000-block search range — which at 886 s/block is
  roughly **20 days**, far wider than a 24-hour audit. That is why `capture` warns when the
  estimated range is below `search_lookback_blocks`: the two ranges must be aligned deliberately
  (`RECON_LOOKBACK_BLOCKS` close to the captured block count) or the orphan list will include
  genuinely old money.
- The `search_lookback_blocks = 2000` default was chosen for ~12 s chains; on Kite testnet it is
  an order of magnitude too wide. Operators should derive it from `window_minutes` with this
  measurement, not copy the default.

## Aligning the search range with the window (2026-11-23)

The 24-hour capture above was audited twice against the same chain, once with the fixed
`search_lookback_blocks = 2000` and once with `--align-window`:

```
# fixed lookback (2000 blocks == ~20 days at 886 s/block)
total=1 matched=1 pending=0 mismatched=0 unknown=0 orphans=15 duplicates=0

# --align-window (window_minutes=1440 -> lookback_blocks=98)
mode=live chain_id=2368 tip=22045335 window=1440m search=22045237..22045335 (98 blocks)
total=1 matched=1 pending=0 mismatched=0 unknown=0 orphans=0 duplicates=0
MATCHED 0x13a25fa7…fd39e amount=2000000000000000000 :: confirmed at block 22045327
```

The report states the searched range it drew its verdicts from (`search=22045237..22045335`,
98 blocks). Without that line a reader cannot separate "the money is missing" from "the search
stopped too early", and the difference between the two runs below would be invisible.

Nothing about the ledger or the chain changed between the two runs — only the range. The 15
orphans were real transfers to the receiver, but every one of them settled weeks before the
24-hour window opened. Reporting them is not wrong, yet as an audit signal it is noise: the
operator asked "what is unaccounted for in the last day", and got a month's worth of history.

`--align-window` measures block time the same way `capture --window-minutes` does and passes the
derived block count to the reconciler, so the search range and the audit window describe the same
period. Use it together with `capture --window-minutes`, then a clean ledger means a clean window.

## Real transfers, captured and reconciled against Kite testnet

This is the run that makes the tool more than a fixture demo. On 2026-11-22 at tip `22045329`,
`capture` read real `Transfer` logs of the testnet payment token for one address, then `scan`
reconciled the captured file against the same chain:

```bash
RECON_CHAIN_MODE=live \
RECON_TOKEN_ADDRESS=0x0fF5393387ad2f9f691FD6Fd28e07E3969e27e63 \
recon --config recon.example.toml capture \
  --from-block 22045179 --to-block 22045329 \
  --receiver 0x66336f24e38b5b155cd3b50f635b1bb0f1098b1d \
  --out fixtures/kite_testnet_capture.jsonl
# mode=live chain_id=2368 blocks=22045179..22045329 receiver=0x6633…8b1d records=2 out=…
```

```bash
RECON_CHAIN_MODE=live RECON_RECEIVER=0x6633…8b1d \
RECON_LEDGER_PATH=fixtures/kite_testnet_capture.jsonl \
recon --config recon.example.toml scan --format text
```

```
mode=live chain_id=2368 tip=22045329 window=60m generated_at=2026-11-22T16:33:21Z
total=2 matched=2 pending=0 mismatched=0 failed=0 unknown=0 orphans=14 duplicates=0
MATCHED 0x13a25fa7…fd39e amount=2000000000000000000 :: confirmed at block 22045327
MATCHED 0x71ddfc73…2a8482 amount=2000000000000000000 :: confirmed at block 22045198
ORPHAN block=22045168 tx=0x06da7657… gate: 14 more transfers arrived in the 60-minute window
        outside the 150-block capture range
```

What this shows, honestly:

- Both captured records are **real testnet settlements** verified through real receipts; the
  amounts are `2e18` atomic units, consistent with `decimals() = 18` read from the contract.
- The 14 orphans are not a bug: transfers settled inside the audit window but outside the capture
  range. That is exactly the "money arrived, ledger has no record" case the tool exists to surface.
- `fixtures/kite_testnet_capture.jsonl` ships those two real records (public chain data). The
  receiver is an address observed on testnet, **not** this project's own wallet, so no private
  data or ownership claim is involved.

## Live probe against Kite testnet

```bash
RECON_CHAIN_MODE=live \
RECON_TOKEN_ADDRESS=0x0fF5393387ad2f9f691FD6Fd28e07E3969e27e63 \
recon --config recon.example.toml probe --json
```

```json
{"chain_id":2368,"ledger":"file","mode":"live",
 "receiver":"0x2222222222222222222222222222222222222222",
 "rpc_url":"https://rpc-testnet.gokite.ai/","tip":22045328,"tip_time":"2026-11-22T16:17:54Z",
 "token_address":"0x0fF5393387ad2f9f691FD6Fd28e07E3969e27e63",
 "token_decimals":18,"token_symbol":"USDT"}
```

- The RPC path works end to end: `eth_blockNumber` returned tip `22045328`.
- The testnet payment token answers `symbol() = USDT` and `decimals() = 18` — **not** the 6 used by
  the fixture. `token_decimals` in the report comes from the chain; the TOML value is only a
  fallback when `decimals()` cannot be read, and amounts in reports stay in atomic units.
- The address above is the one named in the Kite Service Provider Guide
  (<https://docs.gokite.ai/kite-agent-passport/service-provider-guide>). An earlier run against
  `0x3Fdd283C4c43A60398bf93CA01a8a8BD773a755b` returned an empty `symbol()` (data shorter than the
  ABI `string` envelope); the tool logged a warning instead of failing, which is why the token
  contract is never baked into the binary.

## Test surface

See [`TESTING.md`](TESTING.md) for the coverage map and fixture contract. `tests/cli_scan.rs` and
`tests/http_api.rs` assert exact counts on purpose, so fixture drift fails CI instead of silently
changing the demo numbers.

## Boundaries stated up front

- Fixtures are synthetic; no recorded mainnet/testnet settlement data ships with the repo.
- No write path, no key handling, no UI, no settlement retry.
- Mainnet presets exist but are not exercised by CI; verify the contract and decimals yourself.
- Log search is bounded by `search_lookback_blocks`, not by timestamps.
