//! Pure, I/O-free file-transfer logic.
//!
//! The wire side ([`kvm_proto::Message::FileOffer`] / `FileChunk` / `FileEnd`)
//! is dumb framing; this module is the brain that gives it integrity:
//!
//! * [`sanitize_filename`] reduces a peer-supplied name to a safe base name so
//!   a malicious sender cannot write outside the receiver's download directory;
//! * [`plan_messages`] / [`chunk_count`] split a byte stream into the exact
//!   message sequence the sender emits (the daemon streams the same sequence
//!   straight off disk instead of buffering the whole file);
//! * [`FileReassembler`] is the receive-side validator — it enforces in-order
//!   delivery, the declared total size, and the final chunk count, but does NO
//!   I/O itself: the caller writes each accepted chunk to its own sink.
//!
//! Keeping this layer pure is what lets the whole transfer protocol be
//! exhaustively unit-tested with no sockets, no filesystem, and no permissions
//! — matching how the rest of `kvm-core` is tested.

use kvm_proto::Message;
use thiserror::Error;

/// Default chunk payload size: 256 KiB. Comfortably under the 4 MiB frame cap
/// (leaving room for the opcode, id/seq and the length prefix) while keeping
/// per-chunk framing and flush overhead low.
pub const DEFAULT_CHUNK_SIZE: usize = 256 * 1024;

/// Hard ceiling a receiver will tolerate for a single chunk's payload, sized
/// just under the transport's 4 MiB frame cap with header headroom. The sender
/// uses [`DEFAULT_CHUNK_SIZE`]; this only exists to reject a hostile/desynced
/// peer rather than trust its chunk sizes.
pub const MAX_CHUNK_SIZE: usize = 4 * 1024 * 1024 - 1024;

/// Default ceiling on a single received file (10 GiB). A finite default matters:
/// without it a hostile/buggy sender can declare an enormous size and stream
/// chunks until the receiver's disk fills. Operators raise/lower it explicitly.
pub const DEFAULT_MAX_FILE_SIZE: u64 = 10 * 1024 * 1024 * 1024;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum FileError {
    #[error("invalid file name {0:?}")]
    InvalidName(String),
    #[error("file too large: {size} bytes (limit {limit})")]
    TooLarge { size: u64, limit: u64 },
    #[error("offer declined by receiver")]
    Declined,
    #[error("empty chunk (seq {seq}) not allowed")]
    EmptyChunk { seq: u32 },
    #[error("too many chunks")]
    TooManyChunks,
    #[error("chunk id mismatch: expected {expected}, got {got}")]
    IdMismatch { expected: u32, got: u32 },
    #[error("chunk out of order: expected seq {expected}, got {got}")]
    OutOfOrder { expected: u32, got: u32 },
    #[error("chunk too large: {size} bytes (limit {limit})")]
    ChunkTooLarge { size: usize, limit: usize },
    #[error("received more bytes than declared ({got} > {declared})")]
    SizeOverflow { got: u64, declared: u64 },
    #[error("chunk count mismatch: expected {expected}, got {got}")]
    CountMismatch { expected: u32, got: u32 },
    #[error("size mismatch: declared {declared}, received {received}")]
    SizeMismatch { declared: u64, received: u64 },
}

pub type Result<T> = std::result::Result<T, FileError>;

/// Reduce a peer-supplied name to a safe base filename, or reject it.
///
/// Takes only the final path component (under both `/` and `\\`), then rejects
/// the empty string, `.`, `..`, and anything still containing a separator or a
/// NUL. The result is a single name that cannot traverse out of a download
/// directory when joined to it.
pub fn sanitize_filename(name: &str) -> Result<String> {
    let reject = || FileError::InvalidName(name.to_string());
    // Split on BOTH separators and on a Windows drive colon, so "C:evil",
    // "a/b", "x\\y" all reduce to their final bare component.
    let base = name.rsplit(['/', '\\', ':']).next().unwrap_or("");
    if base.is_empty() || base == "." || base == ".." {
        return Err(reject());
    }
    // No residual separators, NUL, or ASCII control characters.
    if base
        .chars()
        .any(|c| matches!(c, '/' | '\\' | ':' | '\0') || c.is_control())
    {
        return Err(reject());
    }
    // Whitespace-only, leading dot (hidden/dotfiles), or trailing dot/space
    // (Windows strips these, opening a clobber/confusion gap) are all rejected.
    if base.trim().is_empty()
        || base.starts_with('.')
        || base.ends_with('.')
        || base.ends_with(' ')
    {
        return Err(reject());
    }
    // Windows reserved device names (with or without an extension).
    let stem = base.split('.').next().unwrap_or(base).to_ascii_uppercase();
    const RESERVED: &[&str] = &[
        "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7",
        "COM8", "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
    ];
    if RESERVED.contains(&stem.as_str()) {
        return Err(reject());
    }
    Ok(base.to_string())
}

/// Number of `chunk_size` blocks a file of `size` bytes splits into. A
/// zero-length file is zero chunks (offer + end frame the empty transfer).
pub fn chunk_count(size: u64, chunk_size: usize) -> u32 {
    let cs = chunk_size.max(1) as u64;
    // div_ceil avoids the `size + cs - 1` overflow near u64::MAX. The cast is
    // safe in practice: a >4 GiB file at the 256 KiB default still fits u32, and
    // any caller streaming enough data to overflow u32 chunks hits the size
    // limit long first.
    size.div_ceil(cs) as u32
}

/// Build the offer message for a transfer.
pub fn offer_msg(id: u32, name: &str, size: u64) -> Message {
    Message::FileOffer {
        id,
        name: name.to_string(),
        size,
    }
}

/// Build one chunk message.
pub fn chunk_msg(id: u32, seq: u32, data: Vec<u8>) -> Message {
    Message::FileChunk { id, seq, data }
}

/// Build the terminating message for a transfer.
pub fn end_msg(id: u32, count: u32) -> Message {
    Message::FileEnd { id, count }
}

/// Plan the full message sequence to send `data` as file `id`: one
/// [`Message::FileOffer`], then one [`Message::FileChunk`] per `chunk_size`
/// block, then [`Message::FileEnd`]. The daemon streams the identical sequence
/// from disk rather than buffering; this convenience is for small sends and
/// for the round-trip unit tests.
pub fn plan_messages(id: u32, name: &str, data: &[u8], chunk_size: usize) -> Vec<Message> {
    let cs = chunk_size.max(1);
    let mut msgs = Vec::with_capacity(2 + chunk_count(data.len() as u64, cs) as usize);
    msgs.push(offer_msg(id, name, data.len() as u64));
    let mut seq = 0u32;
    for block in data.chunks(cs) {
        msgs.push(chunk_msg(id, seq, block.to_vec()));
        seq += 1;
    }
    msgs.push(end_msg(id, seq));
    msgs
}

/// Receive-side validator and accountant for one transfer.
///
/// It performs NO I/O: the caller persists each accepted chunk's bytes to its
/// own sink (a file on the daemon, a `Vec` in tests). The reassembler enforces
/// that chunks arrive in order, that the running total never exceeds the
/// declared size, and that the final [`Message::FileEnd`] count and byte total
/// match — so a truncated, reordered, or padded transfer is rejected rather
/// than silently writing a corrupt file.
#[derive(Debug, Clone)]
pub struct FileReassembler {
    id: u32,
    name: String,
    declared_size: u64,
    next_seq: u32,
    received: u64,
}

impl FileReassembler {
    /// Begin from a [`Message::FileOffer`]'s fields. Sanitises `name` and
    /// rejects a file larger than `size_limit` (pass `u64::MAX` for no limit).
    pub fn begin(id: u32, name: &str, size: u64, size_limit: u64) -> Result<Self> {
        let name = sanitize_filename(name)?;
        if size > size_limit {
            return Err(FileError::TooLarge {
                size,
                limit: size_limit,
            });
        }
        Ok(Self {
            id,
            name,
            declared_size: size,
            next_seq: 0,
            received: 0,
        })
    }

    /// The sanitised destination filename.
    pub fn filename(&self) -> &str {
        &self.name
    }

    /// Total bytes the sender declared in the offer.
    pub fn declared_size(&self) -> u64 {
        self.declared_size
    }

    /// Bytes accepted so far.
    pub fn received(&self) -> u64 {
        self.received
    }

    /// Validate and account one chunk, returning the bytes the caller should
    /// write. Errors (and leaves state unchanged) on a wrong id, an
    /// out-of-order seq, an oversized chunk, or a total that would exceed the
    /// declared size.
    pub fn accept<'a>(&mut self, id: u32, seq: u32, data: &'a [u8]) -> Result<&'a [u8]> {
        if id != self.id {
            return Err(FileError::IdMismatch {
                expected: self.id,
                got: id,
            });
        }
        if seq != self.next_seq {
            return Err(FileError::OutOfOrder {
                expected: self.next_seq,
                got: seq,
            });
        }
        // Reject empty chunks: a legitimate sender never emits one, and allowing
        // them lets a peer advance the transfer forever without delivering bytes
        // (each accepted chunk must make progress toward `declared_size`).
        if data.is_empty() {
            return Err(FileError::EmptyChunk { seq });
        }
        if data.len() > MAX_CHUNK_SIZE {
            return Err(FileError::ChunkTooLarge {
                size: data.len(),
                limit: MAX_CHUNK_SIZE,
            });
        }
        let new_total = self.received + data.len() as u64;
        if new_total > self.declared_size {
            return Err(FileError::SizeOverflow {
                got: new_total,
                declared: self.declared_size,
            });
        }
        self.next_seq = self.next_seq.checked_add(1).ok_or(FileError::TooManyChunks)?;
        self.received = new_total;
        Ok(data)
    }

    /// Validate the terminating [`Message::FileEnd`]. On `Ok`, exactly
    /// `declared_size` bytes were delivered in order across `count` chunks.
    pub fn complete(&self, id: u32, count: u32) -> Result<()> {
        if id != self.id {
            return Err(FileError::IdMismatch {
                expected: self.id,
                got: id,
            });
        }
        if count != self.next_seq {
            return Err(FileError::CountMismatch {
                expected: self.next_seq,
                got: count,
            });
        }
        if self.received != self.declared_size {
            return Err(FileError::SizeMismatch {
                declared: self.declared_size,
                received: self.received,
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Drive a full transfer through plan + reassemble and return the bytes the
    /// receiver would have written.
    fn run_transfer(data: &[u8], chunk_size: usize, limit: u64) -> Result<Vec<u8>> {
        let msgs = plan_messages(7, "payload.bin", data, chunk_size);
        let mut out = Vec::new();
        let mut re: Option<FileReassembler> = None;
        for m in msgs {
            match m {
                Message::FileOffer { id, name, size } => {
                    re = Some(FileReassembler::begin(id, &name, size, limit)?);
                }
                Message::FileChunk { id, seq, data } => {
                    let bytes = re.as_mut().unwrap().accept(id, seq, &data)?;
                    out.extend_from_slice(bytes);
                }
                Message::FileEnd { id, count } => {
                    re.as_ref().unwrap().complete(id, count)?;
                }
                other => panic!("unexpected message in plan: {other:?}"),
            }
        }
        Ok(out)
    }

    #[test]
    fn roundtrip_various_sizes() {
        // Boundaries around the chunk size, plus empty and many-chunk cases.
        for &size in &[0usize, 1, 255, 256, 257, 512, 1000, 4096, 100_000] {
            let data: Vec<u8> = (0..size).map(|i| (i % 251) as u8).collect();
            let got = run_transfer(&data, 256, u64::MAX).unwrap();
            assert_eq!(got, data, "size {size}");
        }
    }

    #[test]
    fn empty_file_is_zero_chunks() {
        let msgs = plan_messages(1, "e", &[], 256);
        assert_eq!(msgs.len(), 2); // offer + end, no chunks
        assert!(matches!(msgs[1], Message::FileEnd { count: 0, .. }));
    }

    #[test]
    fn chunk_count_matches_plan() {
        assert_eq!(chunk_count(0, 256), 0);
        assert_eq!(chunk_count(1, 256), 1);
        assert_eq!(chunk_count(256, 256), 1);
        assert_eq!(chunk_count(257, 256), 2);
        assert_eq!(chunk_count(100_000, 256), 391);
    }

    #[test]
    fn out_of_order_chunk_rejected() {
        let mut re = FileReassembler::begin(1, "f", 10, u64::MAX).unwrap();
        re.accept(1, 0, &[0u8; 4]).unwrap();
        assert_eq!(
            re.accept(1, 2, &[0u8; 4]),
            Err(FileError::OutOfOrder {
                expected: 1,
                got: 2
            })
        );
    }

    #[test]
    fn wrong_id_rejected() {
        let mut re = FileReassembler::begin(5, "f", 4, u64::MAX).unwrap();
        assert_eq!(
            re.accept(6, 0, &[0u8; 4]),
            Err(FileError::IdMismatch {
                expected: 5,
                got: 6
            })
        );
    }

    #[test]
    fn overflow_beyond_declared_size_rejected() {
        let mut re = FileReassembler::begin(1, "f", 4, u64::MAX).unwrap();
        assert_eq!(
            re.accept(1, 0, &[0u8; 8]),
            Err(FileError::SizeOverflow {
                got: 8,
                declared: 4
            })
        );
    }

    #[test]
    fn truncated_transfer_rejected_at_complete() {
        // Declared 8 bytes but only one 4-byte chunk delivered.
        let mut re = FileReassembler::begin(1, "f", 8, u64::MAX).unwrap();
        re.accept(1, 0, &[0u8; 4]).unwrap();
        assert_eq!(
            re.complete(1, 1),
            Err(FileError::SizeMismatch {
                declared: 8,
                received: 4
            })
        );
    }

    #[test]
    fn wrong_chunk_count_rejected_at_complete() {
        let mut re = FileReassembler::begin(1, "f", 8, u64::MAX).unwrap();
        re.accept(1, 0, &[0u8; 8]).unwrap();
        assert_eq!(
            re.complete(1, 5),
            Err(FileError::CountMismatch {
                expected: 1,
                got: 5
            })
        );
    }

    #[test]
    fn oversized_offer_rejected() {
        assert_eq!(
            FileReassembler::begin(1, "f", 10_000, 4096).map(|_| ()),
            Err(FileError::TooLarge {
                size: 10_000,
                limit: 4096
            })
        );
    }

    #[test]
    fn filename_sanitisation() {
        assert_eq!(sanitize_filename("report.pdf").unwrap(), "report.pdf");
        // Path traversal / drive prefixes reduce to the bare base name.
        assert_eq!(sanitize_filename("../../etc/passwd").unwrap(), "passwd");
        assert_eq!(sanitize_filename("/abs/path/x.bin").unwrap(), "x.bin");
        assert_eq!(sanitize_filename("..\\..\\windows\\sysfile").unwrap(), "sysfile");
        assert_eq!(sanitize_filename("C:evil.txt").unwrap(), "evil.txt");
        // Rejected: traversal, empty, dotfiles, trailing dot/space, control
        // chars, reserved device names, whitespace-only.
        for bad in [
            "", ".", "..", "foo/", "a/..", ".hidden", "name.", "name ", " ", "\t",
            "a\nb", "CON", "nul.txt", "LPT1",
        ] {
            assert!(
                matches!(sanitize_filename(bad), Err(FileError::InvalidName(_))),
                "expected reject for {bad:?}"
            );
        }
    }

    #[test]
    fn empty_chunk_rejected() {
        let mut re = FileReassembler::begin(1, "f", 4, u64::MAX).unwrap();
        assert_eq!(
            re.accept(1, 0, &[]),
            Err(FileError::EmptyChunk { seq: 0 })
        );
    }
}
