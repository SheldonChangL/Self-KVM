//! Headless CLI for the Self-KVM daemon.
//!
//! ```text
//! kvm-daemon server --config layout.json
//! kvm-daemon client --server 192.168.1.10:24800 --name laptop --width 1280 --height 800
//! ```
//!
//! Real OS input capture/injection requires building with `--features real-input`
//! (and granting Accessibility / Input Monitoring permissions). Without it the
//! daemon still runs the full network/protocol path — handy for connectivity
//! testing — but captures and injects nothing.

use std::path::PathBuf;

use clap::{Parser, Subcommand};
use kvm_core::{ClientConfig, ScreenSize, ServerConfig};
use kvm_daemon::{ClientRuntime, ServerRuntime};
use kvm_input::Injector;
use tokio::sync::mpsc;

#[derive(Parser)]
#[command(name = "kvm-daemon", version, about = "Self-KVM server/client daemon")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run as the primary (shares this machine's keyboard/mouse).
    Server {
        /// Path to a JSON ServerConfig. If omitted, a default single-screen
        /// config is used.
        #[arg(long)]
        config: Option<PathBuf>,
        #[arg(long)]
        bind: Option<String>,
        #[arg(long)]
        port: Option<u16>,
    },
    /// Run as a secondary (receives input from a server).
    Client {
        #[arg(long)]
        server: String,
        #[arg(long)]
        name: String,
        #[arg(long, default_value_t = 1280)]
        width: i32,
        #[arg(long, default_value_t = 800)]
        height: i32,
    },
    /// Send a file to a receiver over a dedicated bulk channel (FTP-style put).
    Send {
        /// Receiver address, `host:port`.
        #[arg(long)]
        to: String,
        /// Path of the file to send.
        #[arg(long)]
        file: PathBuf,
        /// Override the basename advertised to the receiver.
        #[arg(long)]
        name: Option<String>,
        /// Chunk payload size in bytes.
        #[arg(long, default_value_t = kvm_core::file_transfer::DEFAULT_CHUNK_SIZE)]
        chunk: usize,
        /// Encrypt the transfer with TLS (requires the `tls` build feature).
        #[arg(long)]
        tls: bool,
    },
    /// Receive files into a directory over the bulk channel (long-lived).
    Recv {
        /// Address to bind. Defaults to loopback; bind a public interface
        /// (e.g. `0.0.0.0:24801`) only on a trusted network — the receive path
        /// is unauthenticated.
        #[arg(long, default_value_t = format!("127.0.0.1:{}", kvm_daemon::DEFAULT_FILE_PORT))]
        bind: String,
        /// Directory to save incoming files into.
        #[arg(long)]
        dir: PathBuf,
        /// Reject any offered file larger than this many bytes (default 10 GiB).
        #[arg(long)]
        max_size: Option<u64>,
        /// Accept TLS connections (requires the `tls` build feature).
        #[arg(long)]
        tls: bool,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    let cli = Cli::parse();
    match cli.command {
        Command::Server {
            config,
            bind,
            port,
        } => run_server(config, bind, port).await,
        Command::Client {
            server,
            name,
            width,
            height,
        } => run_client(server, name, width, height).await,
        Command::Send {
            to,
            file,
            name,
            chunk,
            tls,
        } => {
            kvm_daemon::send_file(
                &to,
                &file,
                name.as_deref(),
                kvm_daemon::SendOptions {
                    chunk_size: chunk,
                    tls,
                },
            )
            .await?;
            tracing::info!("sent {} to {to}", file.display());
            Ok(())
        }
        Command::Recv {
            bind,
            dir,
            max_size,
            tls,
        } => {
            kvm_daemon::serve_recv(
                &bind,
                dir,
                max_size.unwrap_or(kvm_core::file_transfer::DEFAULT_MAX_FILE_SIZE),
                tls,
            )
            .await
        }
    }
}

async fn run_server(
    config: Option<PathBuf>,
    bind: Option<String>,
    port: Option<u16>,
) -> anyhow::Result<()> {
    let mut cfg = match config {
        Some(path) => {
            let text = std::fs::read_to_string(&path)?;
            serde_json::from_str::<ServerConfig>(&text)?
        }
        None => ServerConfig::default(),
    };
    if let Some(b) = bind {
        cfg.bind = b;
    }
    if let Some(p) = port {
        cfg.port = p;
    }

    let runtime = ServerRuntime::bind(cfg).await?;
    tracing::info!("listening on {}", runtime.local_addr());

    let (status_tx, mut status_rx) = mpsc::channel(64);
    tokio::spawn(async move {
        while let Some(s) = status_rx.recv().await {
            tracing::info!("[server] {s:?}");
        }
    });

    // Wire the capture source + local hooks. With `real-input` this is the rdev
    // grab loop + a live grab switch; otherwise an inert channel.
    let (events_tx, events_rx) = mpsc::channel(256);
    let hooks = build_server_hooks(&events_tx);
    let _ = events_tx; // kept alive for the (no-capture) default build

    runtime.run(events_rx, hooks, status_tx).await
}

async fn run_client(
    server: String,
    name: String,
    width: i32,
    height: i32,
) -> anyhow::Result<()> {
    let cfg = ClientConfig {
        server_addr: server,
        name,
        screen: ScreenSize::new(width, height),
        tls: false,
    };

    let (status_tx, mut status_rx) = mpsc::channel(64);
    tokio::spawn(async move {
        while let Some(s) = status_rx.recv().await {
            tracing::info!("[client] {s:?}");
        }
    });

    let injector = build_injector();
    ClientRuntime::run(cfg, injector, status_tx).await
}

// --- backend selection ------------------------------------------------------

#[cfg(feature = "real-input")]
fn build_injector() -> Box<dyn Injector> {
    match kvm_input::enigo_backend::EnigoInjector::new() {
        Ok(inj) => Box::new(inj),
        Err(e) => {
            tracing::error!("enigo injector unavailable ({e}); injecting nothing");
            Box::new(kvm_input::NoopInjector)
        }
    }
}

#[cfg(not(feature = "real-input"))]
fn build_injector() -> Box<dyn Injector> {
    tracing::warn!("built without `real-input`: client will connect but inject nothing");
    Box::new(kvm_input::NoopInjector)
}

#[cfg(feature = "real-input")]
fn build_server_hooks(
    events_tx: &mpsc::Sender<kvm_core::LocalEvent>,
) -> Box<dyn kvm_daemon::ServerHooks> {
    use kvm_input::{GrabSwitch, InputCapture};

    let grab = GrabSwitch::new();
    let capture = kvm_input::rdev_backend::RdevCapture::new();
    let tx = events_tx.clone();
    let sink: kvm_input::EventSink = Box::new(move |ev| {
        let _ = tx.try_send(ev);
    });
    if let Err(e) = capture.start(sink, grab.clone()) {
        tracing::error!("input capture unavailable ({e}); server will forward nothing");
    }
    Box::new(kvm_daemon::hooks::LiveHooks::new(
        grab,
        Box::new(|_x, _y| { /* cursor warp on the primary: best-effort, no-op */ }),
    ))
}

#[cfg(not(feature = "real-input"))]
fn build_server_hooks(
    _events_tx: &mpsc::Sender<kvm_core::LocalEvent>,
) -> Box<dyn kvm_daemon::ServerHooks> {
    tracing::warn!("built without `real-input`: server will accept clients but capture nothing");
    Box::new(kvm_daemon::NoopHooks)
}
