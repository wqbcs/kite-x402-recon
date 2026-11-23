# Submission record

Material for the KiteAI developer-program review, kept in the repository so a reviewer can check
claims against the code instead of trusting a chat message. Everything here is reproducible from a
clean checkout on date-stamped runs.

## 1. Repository

| Item | Value |
| --- | --- |
| Repository | <https://github.com/wqbcs/kite-x402-recon> |
| Visibility | public |
| License | Apache-2.0 (`LICENSE`) |
| Default branch | `main` |
| Language / stack | Rust 2021, `reqwest` + `axum` + `tokio`, no chain SDK |
| Author identity | commits authored and pushed by account `wqbcs` (`wqbcs@users.noreply.github.com`) |

## 2. Open Dev Data (EC) registration status

**Status as of 2026-11-22: not yet registered with Open Dev Data.** No entry has been submitted
for this repository, so no EC ecosystem mapping exists yet. The registration steps listed by the
program (repository registration → PR merge → public mapping → commit attribution) are separate
stages; this document is the evidence bundle for the first of them.

Evidence supplied for registration review:

- Repository description and README state the KiteAI relation, the use case and the reproducible
  commands.
- Source-level relation: `src/chain.rs` speaks Kite's JSON-RPC directly against
  `https://rpc-testnet.gokite.ai/` (chain id 2368) and `https://rpc.gokite.ai/` (2366); the token
  address preset comes from the Kite Service Provider Guide.
- Dependency/config relation: no vendor SDK; `recon.example.toml` ships the Kite testnet defaults.
- Verifiable interaction: a recorded live run against Kite testnet (see §5).
- Ecosystem interoperability: `ledger.kind = "hono_file"` consumes the receipt export of the
  reference gateway `kite-x402-hono`.

## 3. Commits in this period

| Date | Commit | Change |
| --- | --- | --- |
| 2026-11-22 | `fc4188a` | repository created via the web UI (`.gitignore` + LICENSE); **not counted as contribution** |
| 2026-11-22 | `110522d` | core implementation: model / error / config / chain / ledger / mock / reconcile / report / CLI / API |
| 2026-11-22 | `6446559` | synthetic fixtures covering five outcomes plus an orphan |
| 2026-11-22 | `f93e739` | CLI black-box tests + HTTP contract tests |
| 2026-11-22 | `d4611dc` | README / ARCHITECTURE / CLI / API / TESTING / DEPLOYMENT docs |
| 2026-11-22 | `f30ed70` | CI: fmt + clippy + test + release build on ubuntu and windows |
| 2026-11-22 | `7ab7a80` | evidence log: CI, mock run, live probe |
| 2026-11-22 | `a1bff88` | `ledger.kind = hono_file`: consume `kite-x402-hono` receipts and verify on chain |
| 2026-11-22 | `c708b78` | duplicate-claim detection (`summary.duplicates`) |
| 2026-11-22 | `418a69c` | related-work comparison, new fixtures, verified token evidence |
| 2026-11-22 | `f049c26` | CI evidence refreshed |
| 2026-11-22 | `0f46f95` | `recon capture`: rebuild a ledger from chain transfers |
| 2026-11-22 | `072d869` | capture docs + real testnet capture fixture |
| 2026-11-22 | `50d05a2` | `chore(release): 0.2.0` + this submission record |
| 2026-11-22 | `v0.2.0` tag | release marker for the above |
| 2026-11-23 | `e851ce7` | `capture --window-minutes`: derive the block range from the audit window |
| 2026-11-23 | `6b651a7` | docs: window-aligned capture + the measured 886 s/block finding |
| 2026-11-23 | `93e67be` | `scan --align-window`: search the range the window actually covers |
| 2026-11-23 | `42c4139` | docs: aligned vs unaligned run (15 orphans → 0) in EVIDENCE |
| 2026-11-23 | `6526037` | `chore(release): 0.3.0` + submission refresh |
| 2026-11-23 | `v0.3.0` tag | release marker for window alignment |
| 2026-11-23 | `9b29fb5` | `report.search`: the report states the block range it searched |
| 2026-11-23 | `8f359d5` | docs: search range in API/coverage/CHANGELOG/EVIDENCE |
| 2026-11-23 | `d261ca4` | `probe`: report the measured block time (`block_s` / `block_seconds`) |
| 2026-11-23 | `31a9ad2` | docs: block time in the probe contract |
| 2026-11-23 | `6ad5728` | window alignment moves into the reconciler/config (on by default, `serve` included) |
| 2026-11-23 | `81569ca` | docs: alignment everywhere + second live measurement (898 s/block, 5-block window) |
| 2026-11-23 | `ddc3864` | reuse the block-time measurement until the tip moves |
| 2026-11-23 | `6a8056a` | docs: one measurement per tip |
| 2026-11-23 | `9ed15c3` | `scan --at-block`: pin an audit to a past block so verdicts replay |
| 2026-11-23 | `297a960` | docs: replay evidence + 55-test count |
| 2026-11-23 | `152a089` | docs: submission commit table refresh |
| 2026-11-23 | `0188b0f` | test: text/markdown/JSON agree on the searched range |
| 2026-11-23 | `23f0e8c` | `report.anchored`: mark a replayed audit instead of passing an old tip as live |
| 2026-11-23 | `af1b11d` | docs: anchored field + replay example |
| 2026-11-23 | `6550427` | test: anchored label asserted in all three views |

## 4. Test, build and lint results

| Check | Command | Result |
| --- | --- | --- |
| Tests | `cargo test --all-targets` | 57 passed (45 unit, 9 CLI black-box, 3 HTTP), 0 failed |
| Lints | `cargo clippy --all-targets -- -D warnings` | clean |
| Format | `cargo fmt --all --check` | clean |
| Docs | `cargo doc --no-deps` | no warnings |
| CI | GitHub Actions `CI` (ubuntu-latest + windows-latest) | green on `42c4139`: <https://github.com/wqbcs/kite-x402-recon/actions/runs/45097642882> |

## 5. Runnable demonstration

No network (mock mode):

```bash
cargo run -- --config recon.example.toml scan --format text
# total=7 matched=2 pending=1 mismatched=1 failed=1 unknown=2 orphans=1 duplicates=0, exit 1
```

Real Kite testnet (2026-11-22, tip `22045329`):

```bash
RECON_CHAIN_MODE=live \
RECON_TOKEN_ADDRESS=0x0fF5393387ad2f9f691FD6Fd28e07E3969e27e63 \
cargo run -- --config recon.example.toml capture \
  --from-block 22045179 --to-block 22045329 \
  --receiver 0x66336f24e38b5b155cd3b50f635b1bb0f1098b1d \
  --out fixtures/kite_testnet_capture.jsonl

RECON_CHAIN_MODE=live RECON_CHAIN_ID=2368 \
RECON_RECEIVER=0x66336f24e38b5b155cd3b50f635b1bb0f1098b1d \
RECON_LEDGER_PATH=fixtures/kite_testnet_capture.jsonl \
cargo run -- --config recon.example.toml scan --format text
# total=2 matched=2 unknown=0 ... orphans=14
```

The second command verifies both real settlements against real receipts. The 14 orphans are
transfers inside the 60-minute audit window that fell outside the 150-block capture range — the
"money arrived, ledger has no record" case. Details and raw outputs:
[`EVIDENCE.md`](EVIDENCE.md).

Window-aligned variant (2026-11-23, tip `22045335`), which removes the unit mismatch instead of
relying on the operator to tune `search_lookback_blocks` by hand:

```bash
RECON_CHAIN_MODE=live \
RECON_TOKEN_ADDRESS=0x0fF5393387ad2f9f691FD6Fd28e07E3969e27e63 \
cargo run -- --config recon.example.toml capture --window-minutes 1440 \
  --receiver 0x66336f24e38b5b155cd3b50f635b1bb0f1098b1d --out /tmp/captured-24h.jsonl
# mode=live chain_id=2368 blocks=22045237..22045335 window=1440m avg_block_s=886 records=1

RECON_WINDOW_MINUTES=1440 RECON_LEDGER_PATH=/tmp/captured-24h.jsonl \
cargo run -- --config recon.example.toml scan --align-window --format text
# total=1 matched=1 ... orphans=0
```

The same ledger scanned with the fixed 2000-block lookback reports `orphans=15`; every one of
those transfers settled weeks before the 24-hour window opened.

## 6. Scope and boundaries (for reviewers)

- Read-only: no private key is loaded, no transaction is signed, no settlement is retried.
- No dedicated frontend: this is a CLI/service tool, so the contract surface is the CLI plus a
  read-only HTTP API (`docs/API.md`); no browser UI is built or claimed.
- Mainnet presets are untested by CI; verify the token contract before production use.
- The idea of post-settlement verification is not original to this project — the comparison table
  in the README names the projects that do it (notably `x402trace` and `hiero-x402`) and states
  what this tool adds on Kite.
