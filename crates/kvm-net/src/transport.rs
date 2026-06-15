use kvm_proto::{Message, ProtoError};
use thiserror::Error;
use tokio::io::{
    AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadHalf, WriteHalf,
};

/// Upper bound on a single frame's payload, matching the protocol's 4 MiB cap.
/// A peer declaring a larger frame is treated as hostile/desynced.
pub const MAX_FRAME_LEN: usize = 4 * 1024 * 1024;

#[derive(Debug, Error)]
pub enum NetError {
    #[error("connection closed by peer")]
    Closed,
    #[error("frame too large: {0} bytes (max {MAX_FRAME_LEN})")]
    FrameTooLarge(usize),
    #[error("protocol decode error: {0}")]
    Proto(#[from] ProtoError),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

/// Read side of a framed connection.
pub struct FramedReader<R> {
    inner: R,
    buf: Vec<u8>,
}

impl<R: AsyncRead + Unpin> FramedReader<R> {
    pub fn new(inner: R) -> Self {
        Self {
            inner,
            buf: Vec::with_capacity(256),
        }
    }

    /// Read and decode the next message. Returns [`NetError::Closed`] on a clean
    /// EOF at a frame boundary.
    pub async fn recv(&mut self) -> Result<Message, NetError> {
        let mut len_bytes = [0u8; 4];
        match self.inner.read_exact(&mut len_bytes).await {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                return Err(NetError::Closed)
            }
            Err(e) => return Err(NetError::Io(e)),
        }
        let len = u32::from_be_bytes(len_bytes) as usize;
        if len > MAX_FRAME_LEN {
            return Err(NetError::FrameTooLarge(len));
        }
        self.buf.resize(len, 0);
        self.inner
            .read_exact(&mut self.buf)
            .await
            .map_err(|e| {
                if e.kind() == std::io::ErrorKind::UnexpectedEof {
                    NetError::Closed
                } else {
                    NetError::Io(e)
                }
            })?;
        Ok(Message::decode(&self.buf)?)
    }
}

/// Write side of a framed connection.
pub struct FramedWriter<W> {
    inner: W,
}

impl<W: AsyncWrite + Unpin> FramedWriter<W> {
    pub fn new(inner: W) -> Self {
        Self { inner }
    }

    /// Encode and write one message, flushing it to the wire.
    pub async fn send(&mut self, msg: &Message) -> Result<(), NetError> {
        let payload = msg.encode();
        let len = payload.len() as u32;
        // One combined buffer => one write syscall, no Nagle-induced split.
        let mut frame = Vec::with_capacity(4 + payload.len());
        frame.extend_from_slice(&len.to_be_bytes());
        frame.extend_from_slice(&payload);
        self.inner.write_all(&frame).await?;
        self.inner.flush().await?;
        Ok(())
    }
}

/// A full-duplex framed connection. Use [`FramedConn::split`] to drive sending
/// and receiving from independent tasks.
pub struct FramedConn<S> {
    reader: FramedReader<ReadHalf<S>>,
    writer: FramedWriter<WriteHalf<S>>,
}

impl<S: AsyncRead + AsyncWrite + Unpin> FramedConn<S> {
    pub fn new(stream: S) -> Self {
        let (r, w) = tokio::io::split(stream);
        Self {
            reader: FramedReader::new(r),
            writer: FramedWriter::new(w),
        }
    }

    pub async fn send(&mut self, msg: &Message) -> Result<(), NetError> {
        self.writer.send(msg).await
    }

    pub async fn recv(&mut self) -> Result<Message, NetError> {
        self.reader.recv().await
    }

    /// Split into independently-owned read and write halves.
    pub fn split(self) -> (FramedReader<ReadHalf<S>>, FramedWriter<WriteHalf<S>>) {
        (self.reader, self.writer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::{TcpListener, TcpStream};

    #[tokio::test]
    async fn frames_roundtrip_over_tcp() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server = tokio::spawn(async move {
            let (sock, _) = listener.accept().await.unwrap();
            let mut conn = FramedConn::new(sock);
            // Echo the first three messages back.
            for _ in 0..3 {
                let m = conn.recv().await.unwrap();
                conn.send(&m).await.unwrap();
            }
        });

        let sock = TcpStream::connect(addr).await.unwrap();
        let mut conn = FramedConn::new(sock);

        let sent = vec![
            Message::Hello { major: 1, minor: 8 },
            Message::MouseMove { x: 123, y: -45 },
            Message::ClipboardData {
                id: 0,
                seq: 1,
                mark: 0,
                data: vec![7u8; 5000], // multi-frame-sized payload
            },
        ];
        for m in &sent {
            conn.send(m).await.unwrap();
            let back = conn.recv().await.unwrap();
            assert_eq!(*m, back);
        }
        server.await.unwrap();
    }

    #[tokio::test]
    async fn clean_close_is_reported() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server = tokio::spawn(async move {
            let (sock, _) = listener.accept().await.unwrap();
            // Drop immediately => peer should observe a clean Closed.
            drop(sock);
        });

        let sock = TcpStream::connect(addr).await.unwrap();
        let mut conn = FramedConn::new(sock);
        assert!(matches!(conn.recv().await, Err(NetError::Closed)));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn oversized_frame_rejected() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            // Announce a frame larger than the cap.
            let bogus = (MAX_FRAME_LEN as u32 + 1).to_be_bytes();
            sock.write_all(&bogus).await.unwrap();
            sock.flush().await.unwrap();
            // keep the socket open so the reader sees the length, not EOF
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        });

        let sock = TcpStream::connect(addr).await.unwrap();
        let mut conn = FramedConn::new(sock);
        assert!(matches!(
            conn.recv().await,
            Err(NetError::FrameTooLarge(_))
        ));
        server.await.unwrap();
    }
}
