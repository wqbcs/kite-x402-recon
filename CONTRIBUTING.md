# Contributing

This is a small, deliberately narrow tool: read-only settlement reconciliation. Contributions that
keep it read-only and dependency-light are welcome; anything that needs a private key, writes to a
chain, or adds a UI framework is out of scope.

## Before you start

```bash
cargo fmt
cargo clippy --all-targets -- -D warnings
cargo test
```

All three must be clean. CI runs the same commands on every pull request.

## Ground rules

- **No new heavy dependencies.** The current stack is `tokio`, `axum`, `reqwest`, `serde`, `clap`,
  `toml`, `tracing`, `thiserror`, `time`. Propose a dependency with a justification (size, license,
  maintenance) before adding it.
- **No panics on untrusted input.** Amounts, hashes, addresses, timestamps and JSON all come from
  outside. Return `ReconError`; `unwrap`/`expect` are denied by lint config.
- **Keep the mock/live boundary explicit.** Mock mode must stay labelled in report output, and live
  mode must never fall back to fixtures.
- **Tests come with the change.** Rule changes need a unit test; output/endpoint changes need an
  assertion in `tests/cli_scan.rs` or `tests/http_api.rs`.
- **Do not edit fixtures and assertions separately.** Counts are asserted on purpose.

## Commit style

Conventional commits, small and scoped:

```
feat(reconcile): recover missing hashes via log search
fix(config): accept millisecond timeouts up to 600000
docs(api): document the 502 path
test(cli): assert strict pending exit code
```

One logical change per commit; no drive-by reformatting of unrelated files.

## Pull request checklist

- [ ] `cargo fmt --check`, `clippy -D warnings`, `cargo test` clean locally
- [ ] New behavior documented (README / docs/ / CHANGELOG `Unreleased`)
- [ ] No secrets, no real RPC tokens, no private endpoints in tests or fixtures
- [ ] `recon.example.toml` and `.env.example` updated if config changed
- [ ] Mock output still labelled `mode=mock`

## Reporting bugs

Include: `recon --version`, the config with secrets removed, the exact command, exit code, and the
stderr log. For chain issues, the transaction hash and block number are enough to reproduce
independently on kitescan.

## License

By contributing you agree your work is licensed under Apache-2.0, the license of this repository.
