// Copyright (C), 2026 Quartz Systems. Some rights reserved. This work is
// licensed under the terms of the MIT license which can be found in the
// root directory of this project.

//! Bounds-checked little-endian reading and writing.
//!
//! Everything that parses bytes coming off the wire goes through [`Reader`], which returns
//! [`Error::Truncated`] instead of panicking when the data is shorter than expected.

use crate::error::{Error, Result};

/// A cursor over a byte slice that never panics.
#[derive(Debug, Clone)]
pub struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
    context: &'static str,
}

impl<'a> Reader<'a> {
    /// Create a reader. `context` names the structure being parsed and ends up in errors.
    pub fn new(buf: &'a [u8], context: &'static str) -> Self {
        Self {
            buf,
            pos: 0,
            context,
        }
    }

    /// Number of bytes not yet consumed.
    pub fn remaining(&self) -> usize {
        self.buf.len().saturating_sub(self.pos)
    }

    /// Current position from the start of the buffer.
    pub fn position(&self) -> usize {
        self.pos
    }

    /// Consume exactly `n` bytes.
    pub fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self.pos.checked_add(n).ok_or(Error::Truncated {
            context: self.context,
            needed: n,
            available: self.remaining(),
        })?;
        let slice = self.buf.get(self.pos..end).ok_or(Error::Truncated {
            context: self.context,
            needed: n,
            available: self.remaining(),
        })?;
        self.pos = end;
        Ok(slice)
    }

    /// Consume all remaining bytes.
    pub fn rest(&mut self) -> &'a [u8] {
        let slice = self.buf.get(self.pos..).unwrap_or(&[]);
        self.pos = self.buf.len();
        slice
    }

    /// Consume `N` bytes into an array.
    pub fn array<const N: usize>(&mut self) -> Result<[u8; N]> {
        let mut out = [0u8; N];
        out.copy_from_slice(self.take(N)?);
        Ok(out)
    }

    /// Read a `u8`.
    pub fn u8(&mut self) -> Result<u8> {
        Ok(u8::from_le_bytes(self.array()?))
    }

    /// Read a little-endian `u16`.
    pub fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_le_bytes(self.array()?))
    }

    /// Read a little-endian `u32`.
    pub fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.array()?))
    }

    /// Read a little-endian `u64`.
    pub fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_le_bytes(self.array()?))
    }

    /// Read a big-endian `u16` (used by the sockaddr items, which are in network order).
    pub fn u16_be(&mut self) -> Result<u16> {
        Ok(u16::from_be_bytes(self.array()?))
    }

    /// Read a big-endian `u32`.
    pub fn u32_be(&mut self) -> Result<u32> {
        Ok(u32::from_be_bytes(self.array()?))
    }

    /// Read a CIP SHORT_STRING (1-byte length followed by that many bytes).
    ///
    /// Non-UTF-8 bytes are replaced rather than rejected: product names are informational.
    pub fn short_string(&mut self) -> Result<String> {
        let len = usize::from(self.u8()?);
        Ok(String::from_utf8_lossy(self.take(len)?).into_owned())
    }

    /// Read a CIP STRING (2-byte little-endian length followed by that many bytes).
    pub fn cip_string(&mut self) -> Result<String> {
        let len = usize::from(self.u16()?);
        Ok(String::from_utf8_lossy(self.take(len)?).into_owned())
    }
}

/// Little-endian helpers for building frames into a `Vec<u8>`.
pub(crate) trait WriteLe {
    fn put_u8(&mut self, v: u8);
    fn put_u16(&mut self, v: u16);
    fn put_u32(&mut self, v: u32);
    fn put_u64(&mut self, v: u64);
}

impl WriteLe for Vec<u8> {
    fn put_u8(&mut self, v: u8) {
        self.push(v);
    }
    fn put_u16(&mut self, v: u16) {
        self.extend_from_slice(&v.to_le_bytes());
    }
    fn put_u32(&mut self, v: u32) {
        self.extend_from_slice(&v.to_le_bytes());
    }
    fn put_u64(&mut self, v: u64) {
        self.extend_from_slice(&v.to_le_bytes());
    }
}

/// Convert a length to the `u16` the wire format wants, or fail.
pub(crate) fn len_u16(len: usize, context: &'static str) -> Result<u16> {
    u16::try_from(len).map_err(|_| Error::TooLarge {
        context,
        len,
        max: usize::from(u16::MAX),
    })
}

/// Format bytes as space-separated hex, for logs and the CLI.
pub fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 3);
    for (i, b) in bytes.iter().enumerate() {
        if i > 0 {
            s.push(' ');
        }
        s.push_str(&format!("{b:02x}"));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_little_endian() {
        let mut r = Reader::new(&[0x01, 0x34, 0x12, 0x78, 0x56, 0x34, 0x12], "test");
        assert_eq!(r.u8().ok(), Some(0x01));
        assert_eq!(r.u16().ok(), Some(0x1234));
        assert_eq!(r.u32().ok(), Some(0x1234_5678));
        assert_eq!(r.remaining(), 0);
    }

    #[test]
    fn truncation_is_an_error_not_a_panic() {
        let mut r = Reader::new(&[0x01], "test");
        assert!(matches!(
            r.u16(),
            Err(Error::Truncated {
                needed: 2,
                available: 1,
                ..
            })
        ));
        // position unchanged after a failed read
        assert_eq!(r.u8().ok(), Some(0x01));
        assert!(r.take(usize::MAX).is_err());
    }

    #[test]
    fn short_string() {
        let mut r = Reader::new(&[3, b'a', b'b', b'c', 9], "test");
        assert_eq!(r.short_string().ok().as_deref(), Some("abc"));
        assert_eq!(r.rest(), &[9]);
        let mut r = Reader::new(&[5, b'a'], "test");
        assert!(r.short_string().is_err());
    }

    #[test]
    fn cip_string() {
        // UR20-FBC-EIP slot class attribute 101, captured 2026-10-13.
        let mut r = Reader::new(
            &[
                0x0b, 0x00, b'U', b'R', b'2', b'0', b'-', b'1', b'6', b'D', b'I', b'-', b'P',
            ],
            "test",
        );
        assert_eq!(r.cip_string().ok().as_deref(), Some("UR20-16DI-P"));
        assert!(
            Reader::new(&[0x05, 0x00, b'a'], "test")
                .cip_string()
                .is_err()
        );
    }

    #[test]
    fn hex_format() {
        assert_eq!(hex(&[0x00, 0xab, 0x10]), "00 ab 10");
        assert_eq!(hex(&[]), "");
    }
}
