//! Server (primary) runtime.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context};
use kvm_core::{LocalEvent, ServerAction, ServerConfig, ServerMachine};
use kvm_net::{FramedConn, NetError};
use kvm_proto::{Message, KEEP_ALIVE_SECS, PROTOCOL_MAJOR, PROTOCOL_MINOR};
use tokio::net::TcpListener;
use tokio::sync::mpsc;

use crate::hooks::ServerHooks;

/// Status updates emitted as the server runs (for a GUI/CLI to display).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ServerStatus {
    Listening(SocketAddr),
    ClientConnected(String),
    ClientDisconnected(String),
    ActiveScreen(String),
    Grab(bool),
}

/// Internal events funnelled into the single-threaded controller so the
/// [`ServerMachine`] never needs locking.
enum ServerEvent {
    Local(LocalEvent),
    ClientConnected {
        screen: String,
        outbound: mpsc::Sender<Message>,
    },
    ClientDisconnected {
        screen: String,
    },
    FromClient {
        screen: String,
        msg: Message,
    },
    KeepAliveTick,
    #[cfg(feature = "clipboard")]
    ClipboardOut(String),
}

pub struct ServerRuntime {
    listener: TcpListener,
    config: Arc<ServerConfig>,
}

impl ServerRuntime {
    /// Bind the listening socket. Use port 0 to get an ephemeral port (tests).
    pub async fn bind(config: ServerConfig) -> anyhow::Result<Self> {
        let addr = format!("{}:{}", config.bind, config.port);
        let listener = TcpListener::bind(&addr)
            .await
            .with_context(|| format!("binding {addr}"))?;
        Ok(Self {
            listener,
            config: Arc::new(config),
        })
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.listener.local_addr().expect("bound listener")
    }

    /// Run until the controller channel closes. `events` is the capture source;
    /// `hooks` performs local grab/warp; `status` receives updates.
    pub async fn run(
        self,
        mut events: mpsc::Receiver<LocalEvent>,
        mut hooks: Box<dyn ServerHooks>,
        status: mpsc::Sender<ServerStatus>,
    ) -> anyhow::Result<()> {
        let local_addr = self.local_addr();
        let config = self.config;
        let listener = self.listener;
        let (ctl_tx, mut ctl_rx) = mpsc::channel::<ServerEvent>(256);

        // capture events -> controller
        {
            let ctl_tx = ctl_tx.clone();
            tokio::spawn(async move {
                while let Some(ev) = events.recv().await {
                    if ctl_tx.send(ServerEvent::Local(ev)).await.is_err() {
                        break;
                    }
                }
            });
        }

        // Optional TLS acceptor (self-signed cert generated per run).
        #[cfg(feature = "tls")]
        let tls_acceptor = if config.tls {
            match kvm_net::tls::server_acceptor() {
                Ok((acc, fp)) => {
                    tracing::info!("TLS enabled — server fingerprint {fp}");
                    let _ = status.send(ServerStatus::ActiveScreen(format!("tls:{fp}"))).await;
                    Some(acc)
                }
                Err(e) => {
                    tracing::error!("TLS init failed ({e}); refusing to run plaintext");
                    return Err(anyhow::anyhow!("tls init: {e}"));
                }
            }
        } else {
            None
        };
        #[cfg(not(feature = "tls"))]
        if config.tls {
            tracing::warn!("config requests TLS but daemon built without the `tls` feature; running plaintext");
        }

        // accept loop
        {
            let ctl_tx = ctl_tx.clone();
            let config = config.clone();
            #[cfg(feature = "tls")]
            let tls_acceptor = tls_acceptor.clone();
            tokio::spawn(async move {
                loop {
                    match listener.accept().await {
                        Ok((sock, peer)) => {
                            sock.set_nodelay(true).ok();
                            let ctl_tx = ctl_tx.clone();
                            let config = config.clone();
                            #[cfg(feature = "tls")]
                            let tls_acceptor = tls_acceptor.clone();
                            tokio::spawn(async move {
                                #[cfg(feature = "tls")]
                                if let Some(acc) = tls_acceptor {
                                    match acc.accept(sock).await {
                                        Ok(tls) => {
                                            if let Err(e) =
                                                handle_client(tls, peer, config, ctl_tx).await
                                            {
                                                tracing::warn!("client {peer} ended: {e:#}");
                                            }
                                        }
                                        Err(e) => tracing::warn!("tls accept {peer}: {e}"),
                                    }
                                    return;
                                }
                                if let Err(e) = handle_client(sock, peer, config, ctl_tx).await {
                                    tracing::warn!("client {peer} ended: {e:#}");
                                }
                            });
                        }
                        Err(e) => {
                            tracing::warn!("accept error: {e}");
                        }
                    }
                }
            });
        }

        // keep-alive ticker
        {
            let ctl_tx = ctl_tx.clone();
            tokio::spawn(async move {
                let mut iv = tokio::time::interval(Duration::from_secs(KEEP_ALIVE_SECS));
                iv.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                loop {
                    iv.tick().await;
                    if ctl_tx.send(ServerEvent::KeepAliveTick).await.is_err() {
                        break;
                    }
                }
            });
        }

        let _ = status.send(ServerStatus::Listening(local_addr)).await;

        // clipboard bridge: forward local changes into the controller as
        // ClipboardOut, and keep a setter to apply data received from clients.
        #[cfg(feature = "clipboard")]
        let clip_setter = {
            let (mut changes, setter) = crate::clipboard::ClipboardBridge::spawn();
            let ctl = ctl_tx.clone();
            tokio::spawn(async move {
                while let Some(text) = changes.recv().await {
                    if ctl.send(ServerEvent::ClipboardOut(text)).await.is_err() {
                        break;
                    }
                }
            });
            setter
        };

        // single-threaded controller owning the machine + client table
        let mut machine = ServerMachine::new(config.layout.clone(), config.local_screen.clone());
        let mut clients: HashMap<String, mpsc::Sender<Message>> = HashMap::new();
        let mut clip_seq: u32 = 0;
        let _ = &mut clip_seq; // used only with the clipboard feature

        while let Some(ev) = ctl_rx.recv().await {
            match ev {
                ServerEvent::Local(le) => {
                    for action in machine.handle(le) {
                        dispatch(action, &mut clients, hooks.as_mut(), &status).await;
                    }
                }
                ServerEvent::ClientConnected { screen, outbound } => {
                    clients.insert(screen.clone(), outbound);
                    let _ = status.send(ServerStatus::ClientConnected(screen)).await;
                }
                ServerEvent::ClientDisconnected { screen } => {
                    clients.remove(&screen);
                    if machine.active_screen() == screen {
                        for action in machine.go_home() {
                            dispatch(action, &mut clients, hooks.as_mut(), &status).await;
                        }
                    }
                    let _ = status.send(ServerStatus::ClientDisconnected(screen)).await;
                }
                ServerEvent::FromClient { screen, msg } => {
                    // Client->server traffic: keep-alive echoes and clipboard
                    // data pushed from a secondary.
                    #[cfg(feature = "clipboard")]
                    if let Message::ClipboardData { data, .. } = &msg {
                        clip_setter.set(String::from_utf8_lossy(data).to_string());
                    }
                    tracing::trace!("from {screen}: {msg:?}");
                }
                ServerEvent::KeepAliveTick => {
                    for tx in clients.values() {
                        let _ = tx.send(Message::KeepAlive).await;
                    }
                }
                #[cfg(feature = "clipboard")]
                ServerEvent::ClipboardOut(text) => {
                    clip_seq = clip_seq.wrapping_add(1);
                    let msg = Message::ClipboardData {
                        id: 0,
                        seq: clip_seq,
                        mark: 0,
                        data: text.into_bytes(),
                    };
                    for tx in clients.values() {
                        let _ = tx.send(msg.clone()).await;
                    }
                }
            }
        }
        Ok(())
    }
}

async fn dispatch(
    action: ServerAction,
    clients: &mut HashMap<String, mpsc::Sender<Message>>,
    hooks: &mut dyn ServerHooks,
    status: &mpsc::Sender<ServerStatus>,
) {
    match action {
        ServerAction::Send { screen, msg } => {
            if let Some(tx) = clients.get(&screen) {
                let _ = tx.send(msg).await;
            }
        }
        ServerAction::SetGrab(on) => {
            hooks.set_grab(on);
            let _ = status.send(ServerStatus::Grab(on)).await;
        }
        ServerAction::WarpCursor { x, y } => hooks.warp_cursor(x, y),
        ServerAction::ActiveChanged { screen } => {
            let _ = status.send(ServerStatus::ActiveScreen(screen)).await;
        }
    }
}

/// Greet a freshly-accepted client, validate it, then pump its connection.
/// Generic over the stream so it serves both plain TCP and a TLS-wrapped
/// stream identically.
async fn handle_client<S>(
    stream: S,
    peer: SocketAddr,
    config: Arc<ServerConfig>,
    ctl_tx: mpsc::Sender<ServerEvent>,
) -> anyhow::Result<()>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let mut conn = FramedConn::new(stream);

    // Handshake: Hello -> HelloBack -> QueryInfo -> DeviceInfo -> ack/options.
    conn.send(&Message::Hello {
        major: PROTOCOL_MAJOR,
        minor: PROTOCOL_MINOR,
    })
    .await?;

    let name = match conn.recv().await? {
        Message::HelloBack { major, name, .. } => {
            if major != PROTOCOL_MAJOR {
                conn.send(&Message::ErrIncompatible {
                    major: PROTOCOL_MAJOR,
                    minor: PROTOCOL_MINOR,
                })
                .await
                .ok();
                bail!("client {peer} runs incompatible protocol v{major}");
            }
            if !config.layout.contains(&name) {
                conn.send(&Message::ErrUnknown).await.ok();
                bail!("client {peer} screen {name:?} not in layout");
            }
            name
        }
        other => bail!("client {peer}: expected HelloBack, got {other:?}"),
    };

    conn.send(&Message::QueryInfo).await?;
    match conn.recv().await? {
        Message::DeviceInfo { w, h, .. } => {
            tracing::info!("client {name:?} connected ({w}x{h}) from {peer}");
        }
        other => bail!("client {name:?}: expected DeviceInfo, got {other:?}"),
    }
    conn.send(&Message::InfoAck).await?;
    conn.send(&Message::ResetOptions).await?;
    conn.send(&Message::SetOptions { opts: vec![] }).await?;

    // Split: a writer task drains the outbound queue; the reader feeds the
    // controller until the peer closes.
    let (mut reader, mut writer) = conn.split();
    let (out_tx, mut out_rx) = mpsc::channel::<Message>(256);
    ctl_tx
        .send(ServerEvent::ClientConnected {
            screen: name.clone(),
            outbound: out_tx,
        })
        .await
        .ok();

    let writer_task = tokio::spawn(async move {
        while let Some(m) = out_rx.recv().await {
            if writer.send(&m).await.is_err() {
                break;
            }
        }
    });

    let pump = async {
        loop {
            match reader.recv().await {
                Ok(msg) => {
                    ctl_tx
                        .send(ServerEvent::FromClient {
                            screen: name.clone(),
                            msg,
                        })
                        .await
                        .ok();
                }
                Err(NetError::Closed) => break,
                Err(e) => return Err(anyhow::anyhow!(e)),
            }
        }
        Ok::<(), anyhow::Error>(())
    };
    let result = pump.await;

    ctl_tx
        .send(ServerEvent::ClientDisconnected { screen: name })
        .await
        .ok();
    writer_task.abort();
    result
}
