// Copyright (C), 2026 Quartz Systems. Some rights reserved. This work is
// licensed under the terms of the MIT license which can be found in the
// root directory of this project.

//! EtherNet/IP encapsulation layer (CIP Vol. 2, chapter 2).
//!
//! Every TCP frame is a 24-byte header followed by `length` bytes of command data.

use std::fmt;

use crate::codec::{Reader, WriteLe, len_u16};
use crate::error::{Error, Result};

/// Size of the encapsulation header in bytes.
pub const HEADER_LEN: usize = 24;

/// Default TCP (and UDP) port for explicit messaging.
pub const DEFAULT_PORT: u16 = 0xAF12; // 44818

/// Encapsulation protocol version sent in RegisterSession.
pub const PROTOCOL_VERSION: u16 = 1;

/// Encapsulation commands.
#[allow(missing_docs)]
pub mod command {
    pub const NOP: u16 = 0x0000;
    pub const LIST_SERVICES: u16 = 0x0004;
    pub const LIST_IDENTITY: u16 = 0x0063;
    pub const LIST_INTERFACES: u16 = 0x0064;
    pub const REGISTER_SESSION: u16 = 0x0065;
    pub const UNREGISTER_SESSION: u16 = 0x0066;
    pub const SEND_RR_DATA: u16 = 0x006F;
    pub const SEND_UNIT_DATA: u16 = 0x0070;
}

/// Status field of the encapsulation header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EncapStatus(pub u32);

impl EncapStatus {
    /// Success.
    pub const SUCCESS: Self = Self(0);

    /// Human readable name from CIP Vol. 2 table 2-3.4.
    pub fn name(self) -> &'static str {
        match self.0 {
            0x0000 => "success",
            0x0001 => "invalid or unsupported command",
            0x0002 => "insufficient memory in receiver",
            0x0003 => "poorly formed or incorrect data",
            0x0064 => "invalid session handle",
            0x0065 => "invalid length",
            0x0069 => "unsupported encapsulation protocol revision",
            _ => "unknown status",
        }
    }
}

impl fmt::Display for EncapStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:#06x} ({})", self.0, self.name())
    }
}

/// The 24-byte encapsulation header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    /// Command code, see [`command`].
    pub command: u16,
    /// Length of the data following the header.
    pub length: u16,
    /// Session handle from RegisterSession (0 before registration).
    pub session: u32,
    /// Status (0 in requests).
    pub status: EncapStatus,
    /// Opaque value echoed by the target; we use it to match replies.
    pub context: u64,
    /// Options (must be 0).
    pub options: u32,
}

impl Header {
    /// Decode a header from at least 24 bytes.
    pub fn decode(buf: &[u8]) -> Result<Self> {
        let mut r = Reader::new(buf, "encapsulation header");
        Ok(Self {
            command: r.u16()?,
            length: r.u16()?,
            session: r.u32()?,
            status: EncapStatus(r.u32()?),
            context: r.u64()?,
            options: r.u32()?,
        })
    }

    fn encode_into(&self, out: &mut Vec<u8>) {
        out.put_u16(self.command);
        out.put_u16(self.length);
        out.put_u32(self.session);
        out.put_u32(self.status.0);
        out.put_u64(self.context);
        out.put_u32(self.options);
    }
}

/// A complete encapsulation frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    /// Header (its `length` is recomputed on encode).
    pub header: Header,
    /// Command-specific data.
    pub data: Vec<u8>,
}

impl Frame {
    /// Build a request frame.
    pub fn request(command: u16, session: u32, context: u64, data: Vec<u8>) -> Self {
        Self {
            header: Header {
                command,
                length: 0,
                session,
                status: EncapStatus::SUCCESS,
                context,
                options: 0,
            },
            data,
        }
    }

    /// Serialize to bytes.
    pub fn encode(&self) -> Result<Vec<u8>> {
        let mut out = Vec::with_capacity(HEADER_LEN + self.data.len());
        let header = Header {
            length: len_u16(self.data.len(), "encapsulation data")?,
            ..self.header
        };
        header.encode_into(&mut out);
        out.extend_from_slice(&self.data);
        Ok(out)
    }

    /// Parse a whole frame (header + exactly `length` data bytes).
    pub fn decode(buf: &[u8]) -> Result<Self> {
        let header = Header::decode(buf)?;
        let mut r = Reader::new(buf, "encapsulation frame");
        r.take(HEADER_LEN)?;
        let data = r.take(usize::from(header.length))?.to_vec();
        if r.remaining() != 0 {
            return Err(Error::malformed(
                "encapsulation frame",
                format!("{} trailing byte(s)", r.remaining()),
            ));
        }
        Ok(Self { header, data })
    }

    /// Fail with [`Error::Encapsulation`] unless the status is success.
    pub fn check_status(&self) -> Result<()> {
        if self.header.status == EncapStatus::SUCCESS {
            Ok(())
        } else {
            Err(Error::Encapsulation {
                command: self.header.command,
                status: self.header.status,
            })
        }
    }
}

/// Command data for RegisterSession.
pub fn register_session_data() -> Vec<u8> {
    let mut out = Vec::with_capacity(4);
    out.put_u16(PROTOCOL_VERSION);
    out.put_u16(0); // options
    out
}

/// Command data for SendRRData / SendUnitData: interface handle, timeout, then a CPF packet.
pub fn send_data(timeout_s: u16, cpf: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(6 + cpf.len());
    out.put_u32(0); // interface handle: CIP
    out.put_u16(timeout_s);
    out.extend_from_slice(cpf);
    out
}

/// Strip the interface handle and timeout off SendRRData/SendUnitData reply data.
pub fn parse_send_data(data: &[u8]) -> Result<&[u8]> {
    let mut r = Reader::new(data, "SendRRData/SendUnitData reply");
    let _interface = r.u32()?;
    let _timeout = r.u16()?;
    Ok(r.rest())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RegisterSession request as seen in Wireshark from any EtherNet/IP scanner.
    const REGISTER_SESSION_REQ: [u8; 28] = [
        0x65, 0x00, 0x04, 0x00, // command, length
        0x00, 0x00, 0x00, 0x00, // session
        0x00, 0x00, 0x00, 0x00, // status
        0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // context
        0x00, 0x00, 0x00, 0x00, // options
        0x01, 0x00, 0x00, 0x00, // protocol version 1, options 0
    ];

    #[test]
    fn encode_register_session() {
        let f = Frame::request(command::REGISTER_SESSION, 0, 1, register_session_data());
        assert_eq!(f.encode().ok().as_deref(), Some(&REGISTER_SESSION_REQ[..]));
    }

    #[test]
    fn decode_register_session_reply() {
        let mut reply = REGISTER_SESSION_REQ;
        reply[4..8].copy_from_slice(&0x1234_5678u32.to_le_bytes());
        let f = Frame::decode(&reply).expect("valid fixture");
        assert_eq!(f.header.command, command::REGISTER_SESSION);
        assert_eq!(f.header.session, 0x1234_5678);
        assert_eq!(f.header.context, 1);
        assert_eq!(f.data, [1, 0, 0, 0]);
        assert!(f.check_status().is_ok());
    }

    #[test]
    fn error_status() {
        let mut reply = REGISTER_SESSION_REQ;
        reply[8] = 0x69;
        let f = Frame::decode(&reply).expect("valid fixture");
        let e = f.check_status().expect_err("status is non-zero");
        assert!(
            e.to_string()
                .contains("unsupported encapsulation protocol revision")
        );
    }

    #[test]
    fn length_mismatch_is_an_error() {
        assert!(Frame::decode(&REGISTER_SESSION_REQ[..27]).is_err());
        let mut long = REGISTER_SESSION_REQ.to_vec();
        long.push(0);
        assert!(Frame::decode(&long).is_err());
        assert!(Frame::decode(&[]).is_err());
    }

    #[test]
    fn send_data_roundtrip() {
        let d = send_data(5, &[0xaa, 0xbb]);
        assert_eq!(d, [0, 0, 0, 0, 5, 0, 0xaa, 0xbb]);
        assert_eq!(parse_send_data(&d).ok(), Some(&[0xaa, 0xbb][..]));
        assert!(parse_send_data(&d[..3]).is_err());
    }
}
