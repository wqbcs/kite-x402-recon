//! `recon` CLI: `probe`, `scan` and `serve`.

use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand, ValueEnum};
use kite_x402_recon::chain::{ChainClient, TransferQuery};
use kite_x402_recon::config::Config;
use kite_x402_recon::model::{format_unix, now_unix};
use kite_x402_recon::server::{router, AppState};
use kite_x402_recon::{
    capture, chain_from_config, ledger_from_config, mode_label, reconciler_from_config, report,
};
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(
    name = "recon",
    version,
    about = "Read-only x402 settlement reconciliation for Kite"
)]
struct Cli {
    /// Path to recon.toml. When omitted, ./recon.toml is used if it exists.
    #[arg(long, global = true, env = "RECON_CONFIG")]
    config: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Check chain connectivity and token metadata, then exit.
    Probe {
        #[arg(long)]
        json: bool,
    },
    /// Reconcile one ledger batch against the chain window.
    Scan {
        #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
        format: OutputFormat,
        /// Exit 2 when anything is still pending, not only on differences.
        #[arg(long)]
        strict: bool,
        /// Also write the rendered report to this path.
        #[arg(long)]
        out: Option<PathBuf>,
        /// Derive the searched block range from `reconcile.window_minutes` using
        /// the chain's measured block time, instead of a fixed lookback in blocks.
        #[arg(long)]
        align_window: bool,
        /// Audit as of this block instead of the current tip, so a past window
        /// can be replayed with the same range and verdicts.
        #[arg(long)]
        at_block: Option<u64>,
    },
    /// Write a ledger file from the token transfers a block range contains.
    Capture {
        /// First block, inclusive. Defaults to `to_block - search_lookback_blocks`.
        #[arg(long)]
        from_block: Option<u64>,
        /// Last block, inclusive. Defaults to the current tip.
        #[arg(long)]
        to_block: Option<u64>,
        /// Derive `from_block` from an audit window, using the chain's own block
        /// time instead of a fixed lookback. Conflicts with `--from-block`.
        #[arg(long, conflicts_with = "from_block")]
        window_minutes: Option<u64>,
        /// Only keep transfers paid to this address. Defaults to the configured receiver.
        #[arg(long)]
        receiver: Option<String>,
        /// Destination JSONL file (required, so log output stays unattended).
        #[arg(long)]
        out: PathBuf,
    },
    /// Serve the read-only HTTP API.
    Serve {
        /// Override server.bind from the config file.
        #[arg(long)]
        bind: Option<String>,
    },
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
enum OutputFormat {
    Text,
    Json,
    Markdown,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    // Logs go to stderr so `--format json` on stdout stays machine-readable.
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();

    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(e) => {
            tracing::error!("cannot start async runtime: {e}");
            return ExitCode::from(78);
        }
    };
    runtime.block_on(run(cli))
}

async fn run(cli: Cli) -> ExitCode {
    let path = cli.config.or_else(default_config_path);
    let config = match Config::load(path.as_deref()) {
        Ok(config) => config,
        Err(e) => {
            tracing::error!("{e}");
            return ExitCode::from(78);
        }
    };
    tracing::info!(
        mode = mode_label(&config),
        chain_id = config.chain.chain_id,
        ledger = %config.ledger.kind.as_str(),
        "configuration loaded"
    );

    let chain = match chain_from_config(&config) {
        Ok(chain) => chain,
        Err(e) => {
            tracing::error!("{e}");
            return ExitCode::from(78);
        }
    };

    match cli.command {
        Command::Probe { json } => probe(&config, chain.as_ref(), json).await,
        Command::Scan {
            format,
            strict,
            out,
            align_window,
            at_block,
        } => {
            scan(
                &config,
                chain,
                format,
                strict,
                out.as_deref(),
                align_window,
                at_block,
            )
            .await
        }
        Command::Capture {
            from_block,
            to_block,
            window_minutes,
            receiver,
            out,
        } => {
            capture_ledger(
                &config,
                chain.as_ref(),
                from_block,
                to_block,
                window_minutes,
                receiver,
                &out,
            )
            .await
        }
        Command::Serve { bind } => serve(&config, chain, bind).await,
    }
}

async fn probe(config: &Config, chain: &(dyn ChainClient + Send + Sync), json: bool) -> ExitCode {
    let tip = match chain.tip().await {
        Ok(tip) => tip,
        Err(e) => {
            tracing::error!("chain probe failed: {e}");
            return ExitCode::from(69);
        }
    };
    let symbol = chain.token_symbol().await.unwrap_or_else(|e| {
        tracing::warn!("token symbol unavailable: {e}");
        String::from("unknown")
    });
    let decimals = chain
        .token_decimals()
        .await
        .unwrap_or(config.chain.token_decimals);
    // Block time decides how a wall-clock window maps to blocks, so it belongs in
    // the probe: an operator choosing `window_minutes` needs it before the first
    // scan, not after a run reports hundreds of orphans.
    let block_seconds = match capture::average_block_seconds(chain, tip).await {
        Ok(seconds) => Some(seconds),
        Err(e) => {
            tracing::warn!("block time unavailable: {e}");
            None
        }
    };
    let body = if json {
        serde_json::json!({
            "mode": mode_label(config),
            "chain_id": config.chain.chain_id,
            "rpc_url": config.chain.rpc_url,
            "tip": tip,
            "tip_time": format_unix(now_unix()),
            "block_seconds": block_seconds,
            "token_address": config.chain.token_address,
            "token_symbol": symbol,
            "token_decimals": decimals,
            "receiver": config.reconcile.receiver,
            "ledger": config.ledger.kind.as_str(),
        })
        .to_string()
    } else {
        format!(
            "mode={} chain_id={} tip={} block_s={} token={} decimals={} receiver={}\n",
            mode_label(config),
            config.chain.chain_id,
            tip,
            block_seconds
                .map(|seconds| seconds.to_string())
                .unwrap_or_else(|| "-".to_string()),
            if symbol.is_empty() {
                "-"
            } else {
                symbol.as_str()
            },
            decimals,
            config.reconcile.receiver
        )
    };
    match write_stdout(&body) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            tracing::error!("cannot write to stdout: {e}");
            ExitCode::from(74)
        }
    }
}

async fn scan(
    config: &Config,
    chain: std::sync::Arc<dyn ChainClient + Send + Sync>,
    format: OutputFormat,
    strict: bool,
    out: Option<&std::path::Path>,
    align_window: bool,
    at_block: Option<u64>,
) -> ExitCode {
    let ledger = match ledger_from_config(config) {
        Ok(ledger) => ledger,
        Err(e) => {
            tracing::error!("{e}");
            return ExitCode::from(78);
        }
    };
    let mut reconciler = reconciler_from_config(config, chain.clone());
    if align_window && !config.reconcile.align_window {
        // Alignment is on by default via config; the flag still forces it for a
        // single run, because an operator passing it means what it says.
        reconciler = reconciler.with_alignment(true);
    }
    if let Some(block) = at_block {
        if block == 0 {
            tracing::error!("--at-block must be at least 1");
            return ExitCode::from(78);
        }
        let tip = match chain.tip().await {
            Ok(tip) => tip,
            Err(e) => {
                tracing::error!("cannot verify --at-block against the chain: {e}");
                return ExitCode::from(69);
            }
        };
        if block > tip {
            tracing::error!(
                at_block = block,
                tip,
                "--at-block is ahead of the chain tip"
            );
            return ExitCode::from(78);
        }
        reconciler = reconciler.with_anchor_tip(block);
    }
    let result = reconciler.report(&*ledger, now_unix()).await;
    let report = match result {
        Ok(report) => report,
        Err(e) => {
            tracing::error!("reconciliation failed: {e}");
            return ExitCode::from(69);
        }
    };
    let rendered = match format {
        OutputFormat::Text => report::to_text(&report),
        OutputFormat::Markdown => report::to_markdown(&report),
        OutputFormat::Json => match report::to_json(&report) {
            Ok(text) => text,
            Err(e) => {
                tracing::error!("cannot serialise report: {e}");
                return ExitCode::from(70);
            }
        },
    };
    if let Some(path) = out {
        if let Err(e) = std::fs::write(path, &rendered) {
            tracing::error!("cannot write {}: {e}", path.display());
            return ExitCode::from(73);
        }
        tracing::info!(path = %path.display(), "report written");
    }
    if let Err(e) = write_stdout(&format!("{rendered}\n")) {
        tracing::error!("cannot write to stdout: {e}");
        return ExitCode::from(74);
    }
    let code = report::exit_code(&report, strict);
    tracing::info!(
        total = report.summary.total,
        matched = report.summary.matched,
        pending = report.summary.pending,
        mismatched = report.summary.mismatched,
        failed = report.summary.failed,
        orphans = report.summary.orphans,
        exit_code = code,
        "scan finished"
    );
    ExitCode::from(u8::try_from(code).unwrap_or(1))
}

/// `recon capture`: write a ledger file from token transfers in a block range.
///
/// This is the recovery path: when the gateway's ledger is missing and the
/// chain is the only witness, capture what the chain already settled, then scan
/// it like any other batch.
async fn capture_ledger(
    config: &Config,
    chain: &(dyn ChainClient + Send + Sync),
    from_block: Option<u64>,
    to_block: Option<u64>,
    window_minutes: Option<u64>,
    receiver: Option<String>,
    out: &std::path::Path,
) -> ExitCode {
    let tip = match chain.tip().await {
        Ok(tip) => tip,
        Err(e) => {
            tracing::error!("chain probe failed: {e}");
            return ExitCode::from(69);
        }
    };
    let to_block = to_block.unwrap_or(tip);
    let lookback = config.reconcile.search_lookback_blocks.max(1);
    let mut window_note = String::new();
    let from_block = match (from_block, window_minutes) {
        (Some(_), Some(_)) => {
            tracing::error!("--from-block and --window-minutes are mutually exclusive");
            return ExitCode::from(78);
        }
        (Some(from), None) => from,
        (None, None) => to_block.saturating_sub(lookback),
        (None, Some(minutes)) => {
            // Measure the chain's own block time instead of assuming one, so a
            // 60-minute audit window maps to the right number of blocks on any
            // chain. The helper floors a sub-second average and the block count
            // is rounded up, so the range can only be wider — over-capturing is
            // the safe direction for an audit.
            let avg_seconds = match capture::average_block_seconds(chain, to_block).await {
                Ok(seconds) => seconds,
                Err(e) => {
                    tracing::error!("cannot estimate block time: {e}");
                    return ExitCode::from(69);
                }
            };
            let blocks = capture::blocks_for_window(minutes, avg_seconds);
            window_note = format!(" window={minutes}m avg_block_s={avg_seconds} ");
            if blocks < config.reconcile.search_lookback_blocks {
                tracing::warn!(
                    captured_blocks = %blocks,
                    search_lookback_blocks = config.reconcile.search_lookback_blocks,
                    "capture covers fewer blocks than the configured fixed lookback; scan searches that range and will report transfers outside the captured range as orphans — alignment (the default) removes this mismatch, or set RECON_LOOKBACK_BLOCKS to the captured block count"
                );
            }
            to_block.saturating_sub(blocks)
        }
    };
    if from_block > to_block {
        tracing::error!("from_block {from_block} is above to_block {to_block}");
        return ExitCode::from(78);
    }
    let receiver = receiver.unwrap_or_else(|| config.reconcile.receiver.clone());
    if let Err(e) = kite_x402_recon::config::validate_address("receiver", &receiver) {
        tracing::error!("{e}");
        return ExitCode::from(78);
    }
    if config.chain.token_address.trim().is_empty() {
        tracing::error!("chain.token_address is empty; capture only reads token transfers");
        return ExitCode::from(78);
    }

    let query = TransferQuery {
        token: config.chain.token_address.clone(),
        from: None,
        to: receiver.clone(),
        from_block,
        to_block,
        limit: config.reconcile.max_records,
    };
    let transfers = match chain.find_transfers(query).await {
        Ok(transfers) => transfers,
        Err(e) => {
            tracing::error!("cannot read transfers: {e}");
            return ExitCode::from(69);
        }
    };
    let records = capture::ledger_from_transfers(&transfers, &format_unix(now_unix()));
    let jsonl = match capture::to_jsonl(&records) {
        Ok(jsonl) => jsonl,
        Err(e) => {
            tracing::error!("cannot serialise captured ledger: {e}");
            return ExitCode::from(70);
        }
    };
    if let Err(e) = std::fs::write(out, jsonl) {
        tracing::error!("cannot write {}: {e}", out.display());
        return ExitCode::from(73);
    }
    let line = format!(
        "mode={} chain_id={} blocks={from_block}..{to_block}{window_note}receiver={receiver} records={} out={}\n",
        mode_label(config),
        config.chain.chain_id,
        records.len(),
        out.display()
    );
    if let Err(e) = write_stdout(&line) {
        tracing::error!("cannot write to stdout: {e}");
        return ExitCode::from(74);
    }
    tracing::info!(
        records = records.len(),
        path = %out.display(),
        from_block,
        to_block,
        receiver = %receiver,
        "ledger captured"
    );
    ExitCode::SUCCESS
}

async fn serve(
    config: &Config,
    chain: std::sync::Arc<dyn ChainClient + Send + Sync>,
    bind: Option<String>,
) -> ExitCode {
    let ledger = match ledger_from_config(config) {
        Ok(ledger) => ledger,
        Err(e) => {
            tracing::error!("{e}");
            return ExitCode::from(78);
        }
    };
    let state = AppState {
        reconciler: std::sync::Arc::new(reconciler_from_config(config, chain)),
        ledger,
        token: config.api_token(),
        mode: mode_label(config),
        refresh_seconds: config.server.refresh_seconds,
    };
    let bind = bind.unwrap_or_else(|| config.server.bind.clone());
    let listener = match tokio::net::TcpListener::bind(&bind).await {
        Ok(listener) => listener,
        Err(e) => {
            tracing::error!("cannot bind {bind}: {e}");
            return ExitCode::from(71);
        }
    };
    if state.token.is_none() {
        tracing::warn!("api token is not set; mount this only on loopback or behind your own auth");
    }
    tracing::info!(addr = %bind, mode = mode_label(config), "serving read-only API");
    match axum::serve(listener, router(state))
        .with_graceful_shutdown(shutdown())
        .await
    {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            tracing::error!("server stopped: {e}");
            ExitCode::from(70)
        }
    }
}

async fn shutdown() {
    let ctrl_c = async {
        if tokio::signal::ctrl_c().await.is_err() {
            std::future::pending::<()>().await;
        }
    };
    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
}

fn default_config_path() -> Option<PathBuf> {
    let candidate = PathBuf::from("recon.toml");
    candidate.exists().then_some(candidate)
}

fn write_stdout(text: &str) -> std::io::Result<()> {
    let mut out = std::io::stdout();
    out.write_all(text.as_bytes())?;
    out.flush()
}
