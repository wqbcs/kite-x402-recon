# Evidence log

Concrete, reproducible checks for this repository. Everything below was produced from a clean
checkout of `main` on 2026-11-22.

## Build and checks

| Check | Command | Result |
| --- | --- | --- |
| Format | `cargo fmt --all --check` | clean |
| Lints | `cargo clippy --all-targets -- -D warnings` | clean |
| Tests | `cargo test --all-targets` | 36 passed (30 unit, 3 CLI, 3 HTTP), 0 failed |
| Docs | `cargo doc --no-deps` | no warnings |
| CI | GitHub Actions `CI` on push `f30ed70` | fmt+clippy+test on ubuntu-latest ✅, windows-latest ✅, rustdoc ✅ |

CI run: <https://github.com/wqbcs/kite-x402-recon/actions/runs/45039227007>

## Mock run (no network)

```bash
cargo run -- --config recon.example.toml scan
```

```
mode=mock chain_id=2368 tip=1000 window=60m generated_at=2026-11-22T15:37:52Z
total=7 matched=2 pending=1 mismatched=1 failed=1 unknown=2 orphans=1
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

## Live probe against Kite testnet

```bash
RECON_CHAIN_MODE=live \
RECON_TOKEN_ADDRESS=0x3Fdd283C4c43A60398bf93CA01a8a8BD773a755b \
recon --config recon.example.toml probe --json
```

```json
{"chain_id":2368,"mode":"live","rpc_url":"https://rpc-testnet.gokite.ai/",
 "tip":22045327,"tip_time":"2026-11-22T15:36:49Z",
 "token_address":"0x3Fdd283C4c43A60398bf93CA01a8a8BD773a755b",
 "token_symbol":"unknown","token_decimals":6}
```

- The RPC path works end to end: `eth_blockNumber` returned tip `22045327`.
- `token_symbol` came back empty: the `symbol()` call against that address returned data shorter
  than the ABI `string` envelope. The tool logs a warning and keeps `token_symbol` informational;
  `chain.token_decimals` in the config remains authoritative. This is exactly why the token
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
