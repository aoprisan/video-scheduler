use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use release_room::{
    store::Store,
    web::{App, router},
    worker,
    youtube::YouTube,
};
use std::{net::SocketAddr, path::PathBuf, sync::Arc};
use tokio::sync::{Mutex, watch};
use uuid::Uuid;

#[derive(Parser)]
#[command(
    version,
    about = "Release Room — YouTube uploads and scheduled publishing"
)]
struct Args {
    #[arg(long, env = "VIDEO_MARKETING_DATA_DIR", default_value = ".data")]
    data_dir: PathBuf,
    #[arg(long, env = "RELEASE_ROOM_BIND", default_value = "127.0.0.1:3000")]
    bind: SocketAddr,
    #[arg(
        long,
        env = "RELEASE_ROOM_PUBLIC_URL",
        default_value = "http://localhost:3000"
    )]
    public_url: String,
    #[arg(long, env = "YOUTUBE_CLIENT_SECRETS")]
    client_secrets: Option<PathBuf>,
    #[command(subcommand)]
    command: Option<Command>,
}
#[derive(Subcommand)]
enum Command {
    Serve,
    List,
    Worker {
        #[arg(long)]
        once: bool,
    },
}
#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "release_room=info".into()),
        )
        .init();
    let args = Args::parse();
    std::fs::create_dir_all(&args.data_dir)?;
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(args.data_dir.join("worker.lock"))?;
    if !matches!(args.command, Some(Command::List)) {
        fs2::FileExt::try_lock_exclusive(&lock).context(
            "Another worker is using this data directory. Stop it before starting Release Room.",
        )?;
    }
    let store = Arc::new(Store::open(&args.data_dir)?);
    if matches!(args.command, Some(Command::List)) {
        println!("{}", serde_json::to_string_pretty(&store.list()?)?);
        return Ok(());
    }
    let yt = Arc::new(YouTube::new(store.clone())?);
    let (stop_tx, stop_rx) = watch::channel(false);
    if let Some(Command::Worker { once }) = args.command {
        store.recover()?;
        if !yt.connected().await {
            bail!("Connect YouTube through the web UI first");
        }
        if once {
            while let Some(j) = store.claim(release_room::model::now())? {
                worker::process(&store, yt.as_ref(), j).await?;
            }
            if store.list()?.iter().any(|j| j.state.attention()) {
                bail!("Some jobs need attention; run list or inspect the web UI");
            }
            return Ok(());
        }
        let task = tokio::spawn(worker::run(store, yt, stop_rx));
        shutdown_signal().await;
        let _ = stop_tx.send(true);
        task.await??;
        return Ok(());
    }
    let public_url = args.public_url.trim_end_matches('/').to_owned();
    let url = reqwest::Url::parse(&public_url)?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
        || !url.username().is_empty()
    {
        bail!("Public URL must be an http(s) origin without a path, query, or user information");
    }
    let password = std::env::var("RELEASE_ROOM_PASSWORD").ok();
    if !args.bind.ip().is_loopback()
        && (password.as_ref().is_none_or(|s| s.len() < 24) || url.scheme() != "https")
    {
        bail!(
            "For network access, set a password of at least 24 characters and an HTTPS public URL behind your reverse proxy"
        );
    }
    let client_path = args
        .client_secrets
        .unwrap_or_else(|| store.dir.join("client_secret.json"));
    let app = Arc::new(App {
        store: store.clone(),
        youtube: yt.clone(),
        csrf: format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple()),
        public_url: public_url.clone(),
        password,
        client_path,
        pending: Mutex::new(Default::default()),
    });
    let listener = tokio::net::TcpListener::bind(args.bind).await?;
    tracing::info!(url=%public_url,"Release Room is ready");
    let mut worker = tokio::spawn(worker::run(store, yt, stop_rx));
    let server = axum::serve(listener, router(app)).with_graceful_shutdown(async move {
        shutdown_signal().await;
        let _ = stop_tx.send(true);
    });
    tokio::select! {r=server=>{r?;worker.await??;},r=&mut worker=>{r??;bail!("Upload worker stopped unexpectedly");}}
    Ok(())
}
async fn shutdown_signal() {
    #[cfg(unix)]
    {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("signal handler");
        tokio::select! {_ = tokio::signal::ctrl_c()=>{},_ = term.recv()=>{}}
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}
