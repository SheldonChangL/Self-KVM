//! Fast file exchange over a dedicated bulk connection.
//!
//! This is Self-KVM's answer to "can it do FTP/Samba-style transfers". Bulk
//! file bytes run on their OWN TCP (and optionally TLS) connection, completely
//! separate from the keyboard/mouse control connection. That separation is the
//! whole point: a multi-gigabyte file streamed over the control connection
//! would head-of-line-block input frames behind it (a single TCP stream is one
//! ordered byte sequence); a second connection lets a transfer saturate the
//! link without ever making the cursor stutter.
//!
//! The wire protocol is `FileOffer` → (`FileAccept`) → N×`FileChunk` →
//! `FileEnd`. All integrity logic — filename sanitisation, in-order chunking,
//! size/count validation — lives in the pure, unit-tested
//! [`kvm_core::file_transfer`]; this module only adds sockets and disk I/O on
//! top, and is itself covered by a localhost end-to-end test.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context};
use kvm_core::file_transfer::{chunk_msg, end_msg, offer_msg, FileReassembler, DEFAULT_CHUNK_SIZE, MAX_CHUNK_SIZE};
use kvm_net::FramedConn;
use kvm_proto::Message;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Semaphore;
use tokio::time::timeout;

/// Default port for the bulk file channel: one above the control port so the
/// two never collide on the default config.
pub const DEFAULT_FILE_PORT: u16 = kvm_proto::DEFAULT_PORT + 1;

/// Abort a receive if no frame arrives within this window — stops a peer from
/// pinning a slot (and a partial file) open forever by stalling mid-transfer.
const IDLE_TIMEOUT: Duration = Duration::from_secs(30);

/// Cap on simultaneous inbound transfers in [`serve_recv`]; connections beyond
/// it are dropped rather than spawned unboundedly (fd/disk fan-out guard).
pub const MAX_CONCURRENT_TRANSFERS: usize = 8;

/// Monotonic token giving each inbound transfer a unique temp filename.
static RX_SEQ: AtomicU64 = AtomicU64::new(0);

/// Knobs for [`send_file`].
#[derive(Clone, Debug)]
pub struct SendOptions {
    /// Payload bytes per chunk. Clamped to `[1, MAX_CHUNK_SIZE]`.
    pub chunk_size: usize,
    /// Wrap the connection in TLS (requires the `tls` feature).
    pub tls: bool,
}

impl Default for SendOptions {
    fn default() -> Self {
        Self {
            chunk_size: DEFAULT_CHUNK_SIZE,
            tls: false,
        }
    }
}

/// Send one file to a receiver listening at `addr` (`host:port`). Streams the
/// file off disk in chunks; never holds the whole file in memory. `name`
/// overrides the basename advertised to the peer (defaults to the source
/// file's own name).
pub async fn send_file(
    addr: &str,
    path: &Path,
    name: Option<&str>,
    opts: SendOptions,
) -> anyhow::Result<()> {
    let meta = tokio::fs::metadata(path)
        .await
        .with_context(|| format!("stat {}", path.display()))?;
    let size = meta.len();
    let name = match name {
        Some(n) => n.to_string(),
        None => path
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "file.bin".to_string()),
    };
    let file = tokio::fs::File::open(path)
        .await
        .with_context(|| format!("open {}", path.display()))?;

    let stream = TcpStream::connect(addr)
        .await
        .with_context(|| format!("connecting to {addr}"))?;
    stream.set_nodelay(true).ok();

    if opts.tls {
        #[cfg(feature = "tls")]
        {
            let store = std::sync::Arc::new(kvm_net::tls::FileTrustStore::load(trust_store_path()));
            let connector = kvm_net::tls::client_connector(addr.to_string(), store);
            let tls = connector
                .connect(kvm_net::tls::server_name(), stream)
                .await
                .map_err(|e| anyhow::anyhow!("tls connect: {e}"))?;
            let mut conn = FramedConn::new(tls);
            return send_framed(&mut conn, 1, &name, size, file, opts.chunk_size).await;
        }
        #[cfg(not(feature = "tls"))]
        bail!("--tls requested but this build lacks the `tls` feature");
    }

    let mut conn = FramedConn::new(stream);
    send_framed(&mut conn, 1, &name, size, file, opts.chunk_size).await
}

/// Drive the send half of the protocol over an established framed connection.
async fn send_framed<S, R>(
    conn: &mut FramedConn<S>,
    id: u32,
    name: &str,
    size: u64,
    mut reader: R,
    chunk_size: usize,
) -> anyhow::Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
    R: AsyncRead + Unpin,
{
    conn.send(&offer_msg(id, name, size)).await?;
    match conn.recv().await? {
        Message::FileAccept { accept: true, .. } => {}
        Message::FileAccept { accept: false, .. } => bail!("receiver declined the file"),
        other => bail!("expected FileAccept, got {other:?}"),
    }

    let cs = chunk_size.clamp(1, MAX_CHUNK_SIZE);
    let mut buf = vec![0u8; cs];
    let mut seq = 0u32;
    let mut sent = 0u64;
    loop {
        let n = fill(&mut reader, &mut buf).await?;
        if n == 0 {
            break;
        }
        conn.send(&chunk_msg(id, seq, buf[..n].to_vec())).await?;
        seq = seq.checked_add(1).context("too many chunks")?;
        sent += n as u64;
    }
    if sent != size {
        bail!("file changed during send: declared {size} bytes, read {sent}");
    }
    conn.send(&end_msg(id, seq)).await?;
    Ok(())
}

/// Read up to `buf.len()` bytes, looping over short reads so every chunk is
/// full except the final one. Returns 0 only at EOF.
async fn fill<R: AsyncRead + Unpin>(reader: &mut R, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        let n = reader.read(&mut buf[filled..]).await?;
        if n == 0 {
            break;
        }
        filled += n;
    }
    Ok(filled)
}

/// Receive one file over an established framed connection, writing it into
/// `dir` under its sanitised name. Returns the path written. On any error the
/// partial file is removed so a failed transfer never leaves a corrupt file.
pub async fn recv_file_to_dir<S>(
    mut conn: FramedConn<S>,
    dir: &Path,
    size_limit: u64,
) -> anyhow::Result<PathBuf>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let first = timeout(IDLE_TIMEOUT, conn.recv())
        .await
        .context("timed out waiting for file offer")??;
    let (id, name, size) = match first {
        Message::FileOffer { id, name, size } => (id, name, size),
        other => bail!("expected FileOffer, got {other:?}"),
    };

    let mut re = match FileReassembler::begin(id, &name, size, size_limit) {
        Ok(re) => re,
        Err(e) => {
            // Tell the sender we're declining before surfacing the error.
            let _ = conn.send(&Message::FileAccept { id, accept: false }).await;
            return Err(anyhow::anyhow!(e)).context("rejecting file offer");
        }
    };
    conn.send(&Message::FileAccept { id, accept: true }).await?;

    tokio::fs::create_dir_all(dir)
        .await
        .with_context(|| format!("creating {}", dir.display()))?;

    // Stream into a unique temp file, then atomically publish under a
    // non-clobbering final name. This is what makes the receive path safe:
    //  * a fresh temp opened with create_new never follows a symlink an
    //    attacker planted at the temp path;
    //  * the real destination only appears, fully written, on success — a
    //    failed/stalled transfer never leaves a corrupt file under the real
    //    name, and a pre-existing file with that name is never touched;
    //  * a unique token means concurrent transfers of the same name don't
    //    corrupt each other.
    let token = RX_SEQ.fetch_add(1, Ordering::Relaxed);
    let tmp = dir.join(format!(".incoming-{token}.part"));

    let body = recv_body(&mut conn, &mut re, &tmp).await;
    let published = match body {
        Ok(()) => publish(&tmp, dir, re.filename(), token).await,
        Err(e) => Err(e),
    };
    if published.is_err() {
        let _ = tokio::fs::remove_file(&tmp).await;
    }
    published
}

async fn recv_body<S>(
    conn: &mut FramedConn<S>,
    re: &mut FileReassembler,
    tmp: &Path,
) -> anyhow::Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let file = tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(tmp)
        .await
        .with_context(|| format!("creating {}", tmp.display()))?;
    let mut writer = tokio::io::BufWriter::new(file);
    loop {
        let msg = timeout(IDLE_TIMEOUT, conn.recv())
            .await
            .context("transfer stalled")??;
        match msg {
            Message::FileChunk { id, seq, data } => {
                let bytes = re.accept(id, seq, &data)?;
                writer.write_all(bytes).await?;
            }
            Message::FileEnd { id, count } => {
                re.complete(id, count)?;
                writer.flush().await?;
                return Ok(());
            }
            other => bail!("unexpected message during transfer: {other:?}"),
        }
    }
}

/// Atomically move the completed temp file to its final name without ever
/// clobbering or following an existing entry. `create_new` reserves the name
/// (failing if anything already occupies it, regular file or symlink); on a
/// collision we fall back to a unique token-prefixed name rather than overwrite.
async fn publish(tmp: &Path, dir: &Path, name: &str, token: u64) -> anyhow::Result<PathBuf> {
    let preferred = dir.join(name);
    match reserve(&preferred).await {
        Ok(()) => {
            tokio::fs::rename(tmp, &preferred).await?;
            Ok(preferred)
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            let alt = dir.join(format!("{token}-{name}"));
            reserve(&alt).await?;
            tokio::fs::rename(tmp, &alt).await?;
            Ok(alt)
        }
        Err(e) => Err(e).with_context(|| format!("publishing {}", preferred.display())),
    }
}

/// Atomically create an empty placeholder at `path`, failing if it already
/// exists (so we never clobber or follow a symlink at the destination).
async fn reserve(path: &Path) -> std::io::Result<()> {
    tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .await
        .map(|_| ())
}

/// Listen on `bind` and receive files into `dir` until cancelled. Each
/// connection delivers one file; transfers are handled concurrently.
pub async fn serve_recv(
    bind: &str,
    dir: PathBuf,
    size_limit: u64,
    tls: bool,
) -> anyhow::Result<()> {
    let listener = TcpListener::bind(bind)
        .await
        .with_context(|| format!("binding {bind}"))?;
    tracing::info!("file receiver listening on {}", listener.local_addr()?);
    tracing::info!("saving files to {}", dir.display());

    if tls && !cfg!(feature = "tls") {
        bail!("--tls requested but this build lacks the `tls` feature");
    }

    // Bound concurrent transfers; connections beyond the cap are dropped rather
    // than spawned unboundedly. A single transient accept() error must not kill
    // the long-lived receiver, so we log and continue instead of `?`.
    let sem = Arc::new(Semaphore::new(MAX_CONCURRENT_TRANSFERS));

    #[cfg(feature = "tls")]
    let acceptor = if tls {
        let (acc, fp) =
            kvm_net::tls::server_acceptor().map_err(|e| anyhow::anyhow!("tls init: {e}"))?;
        tracing::info!("TLS enabled — receiver fingerprint {fp}");
        Some(acc)
    } else {
        None
    };

    loop {
        let (sock, peer) = match listener.accept().await {
            Ok(pair) => pair,
            Err(e) => {
                tracing::warn!("accept error: {e}");
                continue;
            }
        };
        sock.set_nodelay(true).ok();

        let permit = match sem.clone().try_acquire_owned() {
            Ok(p) => p,
            Err(_) => {
                tracing::warn!("at capacity ({MAX_CONCURRENT_TRANSFERS}); dropping {peer}");
                continue;
            }
        };
        let dir = dir.clone();
        #[cfg(feature = "tls")]
        let acceptor = acceptor.clone();

        tokio::spawn(async move {
            let _permit = permit; // released when the transfer finishes
            #[cfg(feature = "tls")]
            if let Some(acc) = acceptor {
                match acc.accept(sock).await {
                    Ok(tls) => handle_one(FramedConn::new(tls), &dir, size_limit, peer).await,
                    Err(e) => tracing::warn!("tls accept {peer}: {e}"),
                }
                return;
            }
            handle_one(FramedConn::new(sock), &dir, size_limit, peer).await;
        });
    }
}

async fn handle_one<S>(conn: FramedConn<S>, dir: &Path, size_limit: u64, peer: std::net::SocketAddr)
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    match recv_file_to_dir(conn, dir, size_limit).await {
        Ok(path) => tracing::info!("received {} from {peer}", path.display()),
        Err(e) => tracing::warn!("transfer from {peer} failed: {e:#}"),
    }
}

/// Where the file sender pins receiver fingerprints (shared with the control
/// client's trust store).
#[cfg(feature = "tls")]
fn trust_store_path() -> std::path::PathBuf {
    let base = std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    base.join(".config/self-kvm/trusted_servers.txt")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Make a unique temp dir for one test, returns its path.
    async fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "self-kvm-ft-{}-{tag}",
            std::process::id()
        ));
        let _ = tokio::fs::remove_dir_all(&dir).await;
        tokio::fs::create_dir_all(&dir).await.unwrap();
        dir
    }

    #[tokio::test]
    async fn file_transfer_roundtrip_localhost() {
        let tmp = temp_dir("roundtrip").await;
        let src = tmp.join("src.bin");
        let recv_dir = tmp.join("recv");
        // ~50 KB so a small chunk size forces many chunks (real multi-chunk path).
        let data: Vec<u8> = (0..50_000u32).map(|i| (i % 251) as u8).collect();
        tokio::fs::write(&src, &data).await.unwrap();

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let recv_dir2 = recv_dir.clone();
        let server = tokio::spawn(async move {
            let (sock, _) = listener.accept().await.unwrap();
            recv_file_to_dir(FramedConn::new(sock), &recv_dir2, u64::MAX)
                .await
                .unwrap()
        });

        let opts = SendOptions {
            chunk_size: 4096,
            tls: false,
        };
        send_file(&addr.to_string(), &src, Some("out.bin"), opts)
            .await
            .unwrap();
        let dest = server.await.unwrap();

        assert_eq!(dest.file_name().unwrap(), "out.bin");
        let got = tokio::fs::read(&dest).await.unwrap();
        assert_eq!(got, data, "received bytes must match source exactly");

        let _ = tokio::fs::remove_dir_all(&tmp).await;
    }

    #[tokio::test]
    async fn path_traversal_name_is_contained() {
        let tmp = temp_dir("traversal").await;
        let src = tmp.join("src.bin");
        let recv_dir = tmp.join("recv");
        tokio::fs::write(&src, b"payload").await.unwrap();

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let recv_dir2 = recv_dir.clone();
        let server = tokio::spawn(async move {
            let (sock, _) = listener.accept().await.unwrap();
            recv_file_to_dir(FramedConn::new(sock), &recv_dir2, u64::MAX)
                .await
                .unwrap()
        });

        // A hostile name must not escape the download directory.
        send_file(
            &addr.to_string(),
            &src,
            Some("../../escape.bin"),
            SendOptions::default(),
        )
        .await
        .unwrap();
        let dest = server.await.unwrap();

        assert_eq!(dest.file_name().unwrap(), "escape.bin");
        assert_eq!(dest.parent().unwrap(), recv_dir);

        let _ = tokio::fs::remove_dir_all(&tmp).await;
    }

    #[tokio::test]
    async fn oversize_file_is_declined() {
        let tmp = temp_dir("decline").await;
        let src = tmp.join("big.bin");
        let recv_dir = tmp.join("recv");
        tokio::fs::write(&src, vec![0u8; 10_000]).await.unwrap();

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let recv_dir2 = recv_dir.clone();
        let server = tokio::spawn(async move {
            let (sock, _) = listener.accept().await.unwrap();
            // 1 KB limit < 10 KB file => must be declined.
            recv_file_to_dir(FramedConn::new(sock), &recv_dir2, 1024).await
        });

        let send = send_file(&addr.to_string(), &src, None, SendOptions::default()).await;
        assert!(send.is_err(), "sender should observe the decline");
        assert!(server.await.unwrap().is_err(), "receiver should reject");

        let _ = tokio::fs::remove_dir_all(&tmp).await;
    }

    #[tokio::test]
    async fn empty_file_roundtrip() {
        let tmp = temp_dir("empty").await;
        let src = tmp.join("empty.bin");
        let recv_dir = tmp.join("recv");
        tokio::fs::write(&src, b"").await.unwrap();

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let recv_dir2 = recv_dir.clone();
        let server = tokio::spawn(async move {
            let (sock, _) = listener.accept().await.unwrap();
            recv_file_to_dir(FramedConn::new(sock), &recv_dir2, u64::MAX)
                .await
                .unwrap()
        });
        send_file(&addr.to_string(), &src, None, SendOptions::default())
            .await
            .unwrap();
        let dest = server.await.unwrap();

        assert_eq!(dest.file_name().unwrap(), "empty.bin");
        assert!(tokio::fs::read(&dest).await.unwrap().is_empty());

        let _ = tokio::fs::remove_dir_all(&tmp).await;
    }

    #[tokio::test]
    async fn mid_stream_disconnect_leaves_no_file() {
        let tmp = temp_dir("disconnect").await;
        let recv_dir = tmp.join("recv");
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let recv_dir2 = recv_dir.clone();
        let server = tokio::spawn(async move {
            let (sock, _) = listener.accept().await.unwrap();
            recv_file_to_dir(FramedConn::new(sock), &recv_dir2, u64::MAX).await
        });

        // Manual sender: offer 100 bytes, deliver one 10-byte chunk, then vanish.
        let mut conn = FramedConn::new(TcpStream::connect(addr).await.unwrap());
        conn.send(&Message::FileOffer {
            id: 1,
            name: "half.bin".into(),
            size: 100,
        })
        .await
        .unwrap();
        assert!(matches!(
            conn.recv().await.unwrap(),
            Message::FileAccept { accept: true, .. }
        ));
        conn.send(&Message::FileChunk {
            id: 1,
            seq: 0,
            data: vec![7u8; 10],
        })
        .await
        .unwrap();
        drop(conn); // disconnect mid-transfer

        assert!(
            server.await.unwrap().is_err(),
            "receiver must error on truncation"
        );
        // Neither the real name nor the temp file survives.
        let mut entries = tokio::fs::read_dir(&recv_dir).await.unwrap();
        assert!(
            entries.next_entry().await.unwrap().is_none(),
            "download dir must be empty after a failed transfer"
        );

        let _ = tokio::fs::remove_dir_all(&tmp).await;
    }

    #[tokio::test]
    async fn out_of_order_chunk_over_wire_rejected() {
        let tmp = temp_dir("ooo").await;
        let recv_dir = tmp.join("recv");
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let recv_dir2 = recv_dir.clone();
        let server = tokio::spawn(async move {
            let (sock, _) = listener.accept().await.unwrap();
            recv_file_to_dir(FramedConn::new(sock), &recv_dir2, u64::MAX).await
        });

        let mut conn = FramedConn::new(TcpStream::connect(addr).await.unwrap());
        conn.send(&Message::FileOffer {
            id: 1,
            name: "x.bin".into(),
            size: 20,
        })
        .await
        .unwrap();
        assert!(matches!(
            conn.recv().await.unwrap(),
            Message::FileAccept { accept: true, .. }
        ));
        // Skipping seq 0 is an out-of-order violation.
        conn.send(&Message::FileChunk {
            id: 1,
            seq: 1,
            data: vec![0u8; 10],
        })
        .await
        .unwrap();

        assert!(
            server.await.unwrap().is_err(),
            "receiver must reject an out-of-order chunk"
        );

        let _ = tokio::fs::remove_dir_all(&tmp).await;
    }
}
