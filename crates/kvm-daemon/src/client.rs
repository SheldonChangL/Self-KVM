//! Client (secondary) runtime.

use anyhow::Context;
use kvm_core::{ClientAction, ClientConfig, ClientMachine};
use kvm_input::Injector;
use kvm_net::{FramedConn, NetError};
use tokio::net::TcpStream;
use tokio::sync::mpsc;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClientStatus {
    Connecting,
    Connected,
    Entered,
    Left,
    Disconnected(String),
}

pub struct ClientRuntime;

impl ClientRuntime {
    /// Connect to the server (optionally over TLS) and serve input until the
    /// connection closes.
    pub async fn run(
        config: ClientConfig,
        injector: Box<dyn Injector>,
        status: mpsc::Sender<ClientStatus>,
    ) -> anyhow::Result<()> {
        let _ = status.send(ClientStatus::Connecting).await;
        let stream = TcpStream::connect(&config.server_addr)
            .await
            .with_context(|| format!("connecting to {}", config.server_addr))?;
        stream.set_nodelay(true).ok();
        let machine = ClientMachine::new(config.name.clone(), config.screen);

        #[cfg(feature = "tls")]
        if config.tls {
            let store = std::sync::Arc::new(kvm_net::tls::FileTrustStore::load(trust_store_path()));
            let connector = kvm_net::tls::client_connector(config.server_addr.clone(), store);
            let tls = connector
                .connect(kvm_net::tls::server_name(), stream)
                .await
                .map_err(|e| anyhow::anyhow!("tls connect: {e}"))?;
            return serve(FramedConn::new(tls), machine, injector, status).await;
        }
        #[cfg(not(feature = "tls"))]
        if config.tls {
            let _ = status
                .send(ClientStatus::Disconnected(
                    "TLS requested but daemon built without the `tls` feature".into(),
                ))
                .await;
            anyhow::bail!("tls requested but unsupported in this build");
        }

        serve(FramedConn::new(stream), machine, injector, status).await
    }
}

/// The reactive serve loop, generic over the (optionally TLS-wrapped) stream.
async fn serve<S>(
    mut conn: FramedConn<S>,
    mut machine: ClientMachine,
    mut injector: Box<dyn Injector>,
    status: mpsc::Sender<ClientStatus>,
) -> anyhow::Result<()>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send,
{
    #[cfg(feature = "clipboard")]
    let clip = crate::clipboard::ClipboardBridge::spawn_sink();

    loop {
        let msg = match conn.recv().await {
            Ok(m) => m,
            Err(NetError::Closed) => {
                let _ = status
                    .send(ClientStatus::Disconnected("server closed".into()))
                    .await;
                return Ok(());
            }
            Err(e) => {
                let _ = status.send(ClientStatus::Disconnected(e.to_string())).await;
                return Err(e.into());
            }
        };

        #[cfg(feature = "clipboard")]
        if let kvm_proto::Message::ClipboardData { data, .. } = &msg {
            clip.set(String::from_utf8_lossy(data).to_string());
        }

        for action in machine.handle(msg) {
            match action {
                ClientAction::Inject(cmd) => {
                    if let Err(e) = injector.inject(cmd) {
                        tracing::warn!("injection failed: {e}");
                    }
                }
                ClientAction::Reply(reply) => conn.send(&reply).await?,
                ClientAction::Connected => {
                    let _ = status.send(ClientStatus::Connected).await;
                }
                ClientAction::Entered => {
                    let _ = status.send(ClientStatus::Entered).await;
                }
                ClientAction::Left => {
                    let _ = status.send(ClientStatus::Left).await;
                }
                ClientAction::Disconnect(reason) => {
                    let _ = status
                        .send(ClientStatus::Disconnected(format!("{reason:?}")))
                        .await;
                    return Ok(());
                }
            }
        }
    }
}

/// Where pinned server fingerprints are stored.
#[cfg(feature = "tls")]
fn trust_store_path() -> std::path::PathBuf {
    let base = std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    base.join(".config/self-kvm/trusted_servers.txt")
}
