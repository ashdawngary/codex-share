mod mask;
mod picker;
mod source;
mod tunnel;
mod web;

use std::{io::IsTerminal, net::SocketAddr, path::PathBuf, sync::Arc};

use anyhow::{Context, Result};
use clap::{Parser, ValueEnum};
use mask::Masker;
use source::{JsonlSource, SharedFeed};
use tokio::sync::watch;
use tracing_subscriber::EnvFilter;
use tunnel::TunnelKind;

#[derive(Debug, Parser)]
#[command(version, about)]
struct Cli {
    /// Rollout JSONL to watch. Skips the interactive thread picker.
    #[arg(long)]
    file: Option<PathBuf>,

    /// Share the newest session without opening the interactive picker.
    #[arg(long, conflicts_with = "file")]
    latest: bool,

    /// Number of recent threads to show in the interactive picker.
    #[arg(long, default_value_t = 12)]
    recent: usize,

    /// Address for the local read-only server.
    #[arg(long, default_value = "127.0.0.1:48123")]
    listen: SocketAddr,

    /// Optional public exposure provider. Its CLI must already be installed.
    #[arg(long, value_enum, default_value_t = TunnelArg::None)]
    tunnel: TunnelArg,

    /// Report local ngrok and Cloudflare tunnel readiness, then exit.
    #[arg(long)]
    check_tunnels: bool,

    /// Additional literal value to replace everywhere in public text.
    #[arg(long = "redact", value_name = "TEXT")]
    redactions: Vec<String>,

    /// Keep at most this many normalized events in memory.
    #[arg(long, default_value_t = 2_000)]
    history: usize,

    /// Information granted to this share link.
    #[arg(long, value_enum, default_value_t = PermissionArg::Conversation)]
    permission: PermissionArg,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum TunnelArg {
    None,
    Ngrok,
    Cloudflare,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum PermissionArg {
    /// User and assistant messages only.
    Conversation,
    /// Messages plus tool names and coarse started/completed state.
    Activity,
    /// Messages plus masked file diffs, without tool activity.
    Diffs,
    /// Messages, sanitized tool activity, and masked file diffs.
    ActivityDiffs,
}

impl From<PermissionArg> for web::SharePermission {
    fn from(value: PermissionArg) -> Self {
        match value {
            PermissionArg::Conversation => Self::Conversation,
            PermissionArg::Activity => Self::Activity,
            PermissionArg::Diffs => Self::Diffs,
            PermissionArg::ActivityDiffs => Self::ActivityDiffs,
        }
    }
}

impl From<TunnelArg> for TunnelKind {
    fn from(value: TunnelArg) -> Self {
        match value {
            TunnelArg::None => Self::None,
            TunnelArg::Ngrok => Self::Ngrok,
            TunnelArg::Cloudflare => Self::Cloudflare,
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| "codex_share=info".into()),
        )
        .with_target(false)
        .init();

    let cli = Cli::parse();
    if cli.check_tunnels {
        tunnel::print_availability();
        return Ok(());
    }
    let tunnel = TunnelKind::from(cli.tunnel);
    tunnel.preflight()?;
    let file = match cli.file {
        Some(path) => path,
        None if cli.latest || !std::io::stdin().is_terminal() => {
            source::newest_rollout().context("no Codex rollout found; pass --file explicitly")?
        }
        None => picker::choose_recent_session(cli.recent)?.context("sharing cancelled")?,
    };
    let file = file
        .canonicalize()
        .with_context(|| format!("cannot open {}", file.display()))?;

    let masker = Masker::new(cli.redactions)?;
    let feed = Arc::new(SharedFeed::new(cli.history));
    let source = JsonlSource::new(file.clone(), masker, feed.clone());
    let source_task = tokio::spawn(async move { source.run().await });

    let grant = web::ShareGrant::mint(cli.permission.into());
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let app = web::router(feed, file.clone(), grant.clone(), shutdown_rx);
    let listener = tokio::net::TcpListener::bind(cli.listen)
        .await
        .with_context(|| format!("cannot listen on {}", cli.listen))?;
    let local_url = format!("http://{}", listener.local_addr()?);

    let provider = tunnel.provider();
    let mut exposure = provider.expose(&local_url).await?;
    let base_url = exposure.public_url().unwrap_or(&local_url);
    let share_url = web::share_url(base_url, &grant);

    println!("Watching: {}", file.display());
    println!("Share URL: {share_url}");
    println!("Viewer permissions: {}", grant.permission().label());
    println!("Press Ctrl-C to stop sharing.");

    let server = axum::serve(listener, app).with_graceful_shutdown(shutdown(shutdown_tx));
    tokio::select! {
        result = server => result.context("HTTP server failed")?,
        result = source_task => result.context("source task panicked")??,
    }

    exposure.stop().await;
    Ok(())
}

async fn shutdown(shutdown_tx: watch::Sender<bool>) {
    let _ = tokio::signal::ctrl_c().await;
    let _ = shutdown_tx.send(true);
}
