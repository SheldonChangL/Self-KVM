//! Low-level wire primitives.
//!
//! The wire format mirrors the Synergy/Barrier family: everything is
//! **big-endian** (network byte order). Three composite encodings recur:
//!
//! * length-prefixed bytes (`%s`): a 4-byte big-endian length followed by the
//!   raw bytes (NOT NUL-terminated);
//! * an `i32` vector (`%4I`): a 4-byte big-endian element count followed by that
//!   many big-endian `i32`s;
//! * fixed-width strings (`%7s`): exactly N raw bytes, used only by the hello
//!   greeting magic.

use crate::error::{ProtoError, Result};

/// Hard ceiling on any length-prefixed field, matching the reference protocol's
/// 4 MiB message cap. Guards against a hostile peer asking us to allocate wildly.
pub const MAX_FIELD_LEN: usize = 4 * 1024 * 1024;

/// Cursor-based reader over a borrowed payload slice.
#[derive(Debug)]
pub struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    /// Number of unconsumed bytes.
    pub fn remaining(&self) -> usize {
        self.buf.len() - self.pos
    }

    pub fn is_empty(&self) -> bool {
        self.remaining() == 0
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        if self.remaining() < n {
            return Err(ProtoError::UnexpectedEof {
                needed: n,
                had: self.remaining(),
            });
        }
        let slice = &self.buf[self.pos..self.pos + n];
        self.pos += n;
        Ok(slice)
    }

    pub fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }

    pub fn i8(&mut self) -> Result<i8> {
        Ok(self.u8()? as i8)
    }

    pub fn u16(&mut self) -> Result<u16> {
        let b = self.take(2)?;
        Ok(u16::from_be_bytes([b[0], b[1]]))
    }

    pub fn i16(&mut self) -> Result<i16> {
        Ok(self.u16()? as i16)
    }

    pub fn u32(&mut self) -> Result<u32> {
        let b = self.take(4)?;
        Ok(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }

    pub fn i32(&mut self) -> Result<i32> {
        Ok(self.u32()? as i32)
    }

    /// Read a big-endian `u64` (used for byte sizes that may exceed 4 GiB).
    pub fn u64(&mut self) -> Result<u64> {
        let b = self.take(8)?;
        Ok(u64::from_be_bytes([
            b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
        ]))
    }

    /// Read exactly `n` raw bytes (fixed-width field, e.g. the hello magic).
    pub fn fixed(&mut self, n: usize) -> Result<&'a [u8]> {
        self.take(n)
    }

    /// Read a length-prefixed byte string (`%s`).
    pub fn bytes(&mut self) -> Result<Vec<u8>> {
        let len = self.u32()? as usize;
        if len > MAX_FIELD_LEN {
            return Err(ProtoError::TooLong {
                declared: len,
                max: MAX_FIELD_LEN,
            });
        }
        Ok(self.take(len)?.to_vec())
    }

    /// Read a length-prefixed UTF-8 string (`%s` interpreted as text).
    pub fn string(&mut self) -> Result<String> {
        let bytes = self.bytes()?;
        String::from_utf8(bytes).map_err(|_| ProtoError::InvalidUtf8)
    }

    /// Read an `i32` vector (`%4I`): count followed by that many big-endian i32s.
    pub fn i32_vec(&mut self) -> Result<Vec<i32>> {
        let count = self.u32()? as usize;
        if count > MAX_FIELD_LEN / 4 {
            return Err(ProtoError::TooLong {
                declared: count,
                max: MAX_FIELD_LEN / 4,
            });
        }
        let mut out = Vec::with_capacity(count);
        for _ in 0..count {
            out.push(self.i32()?);
        }
        Ok(out)
    }
}

/// Append-only writer building a payload buffer.
#[derive(Debug, Default)]
pub struct Writer {
    buf: Vec<u8>,
}

impl Writer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Pre-seed with a 4-byte message opcode (or any fixed magic).
    pub fn with_code(code: &[u8; 4]) -> Self {
        let mut w = Self::new();
        w.raw(code);
        w
    }

    pub fn raw(&mut self, bytes: &[u8]) -> &mut Self {
        self.buf.extend_from_slice(bytes);
        self
    }

    pub fn u8(&mut self, v: u8) -> &mut Self {
        self.buf.push(v);
        self
    }

    pub fn i8(&mut self, v: i8) -> &mut Self {
        self.buf.push(v as u8);
        self
    }

    pub fn u16(&mut self, v: u16) -> &mut Self {
        self.buf.extend_from_slice(&v.to_be_bytes());
        self
    }

    pub fn i16(&mut self, v: i16) -> &mut Self {
        self.u16(v as u16)
    }

    pub fn u32(&mut self, v: u32) -> &mut Self {
        self.buf.extend_from_slice(&v.to_be_bytes());
        self
    }

    pub fn i32(&mut self, v: i32) -> &mut Self {
        self.u32(v as u32)
    }

    /// Write a big-endian `u64`.
    pub fn u64(&mut self, v: u64) -> &mut Self {
        self.buf.extend_from_slice(&v.to_be_bytes());
        self
    }

    /// Write a length-prefixed byte string (`%s`).
    pub fn bytes(&mut self, b: &[u8]) -> &mut Self {
        self.u32(b.len() as u32);
        self.buf.extend_from_slice(b);
        self
    }

    /// Write a length-prefixed UTF-8 string (`%s`).
    pub fn string(&mut self, s: &str) -> &mut Self {
        self.bytes(s.as_bytes())
    }

    /// Write an `i32` vector (`%4I`).
    pub fn i32_vec(&mut self, v: &[i32]) -> &mut Self {
        self.u32(v.len() as u32);
        for &x in v {
            self.i32(x);
        }
        self
    }

    pub fn into_vec(self) -> Vec<u8> {
        self.buf
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_primitives() {
        let mut w = Writer::new();
        w.u8(0xAB)
            .i8(-5)
            .u16(0x1234)
            .i16(-1000)
            .u32(0xDEADBEEF)
            .i32(-123456)
            .u64(0x0102_0304_0506_0708);
        let buf = w.into_vec();

        let mut r = Reader::new(&buf);
        assert_eq!(r.u8().unwrap(), 0xAB);
        assert_eq!(r.i8().unwrap(), -5);
        assert_eq!(r.u16().unwrap(), 0x1234);
        assert_eq!(r.i16().unwrap(), -1000);
        assert_eq!(r.u32().unwrap(), 0xDEADBEEF);
        assert_eq!(r.i32().unwrap(), -123456);
        assert_eq!(r.u64().unwrap(), 0x0102_0304_0506_0708);
        assert!(r.is_empty());
    }

    #[test]
    fn big_endian_layout() {
        let mut w = Writer::new();
        w.u32(0x01020304);
        assert_eq!(w.into_vec(), vec![0x01, 0x02, 0x03, 0x04]);
    }

    #[test]
    fn roundtrip_string_and_vec() {
        let mut w = Writer::new();
        w.string("hello").i32_vec(&[1, -2, 3]);
        let buf = w.into_vec();

        // "hello" => 4-byte len (5) + 5 bytes; vec => 4-byte count (3) + 3 i32.
        assert_eq!(&buf[0..4], &[0, 0, 0, 5]);

        let mut r = Reader::new(&buf);
        assert_eq!(r.string().unwrap(), "hello");
        assert_eq!(r.i32_vec().unwrap(), vec![1, -2, 3]);
        assert!(r.is_empty());
    }

    #[test]
    fn eof_is_reported() {
        let buf = [0u8, 1];
        let mut r = Reader::new(&buf);
        assert!(matches!(
            r.u32(),
            Err(ProtoError::UnexpectedEof { needed: 4, had: 2 })
        ));
    }

    #[test]
    fn oversized_field_rejected() {
        // declares a 1 GiB string but provides nothing
        let buf = [0x40, 0, 0, 0];
        let mut r = Reader::new(&buf);
        assert!(matches!(r.bytes(), Err(ProtoError::TooLong { .. })));
    }
}
