// Copyright (C), 2026 Quartz Systems. Some rights reserved. This work is
// licensed under the terms of the MIT license which can be found in the
// root directory of this project.

//! Message Router request and response (CIP Vol. 1, 2-4).

use std::fmt;

use crate::codec::{Reader, WriteLe, hex};
use crate::epath::EPath;
use crate::error::{Error, Result};

/// Common service codes.
#[allow(missing_docs)]
pub mod service {
    pub const GET_ATTRIBUTES_ALL: u8 = 0x01;
    pub const SET_ATTRIBUTES_ALL: u8 = 0x02;
    pub const GET_ATTRIBUTE_LIST: u8 = 0x03;
    pub const RESET: u8 = 0x05;
    pub const GET_ATTRIBUTE_SINGLE: u8 = 0x0E;
    pub const SET_ATTRIBUTE_SINGLE: u8 = 0x10;
    pub const FORWARD_CLOSE: u8 = 0x4E;
    pub const UNCONNECTED_SEND: u8 = 0x52;
    pub const FORWARD_OPEN: u8 = 0x54;
    pub const LARGE_FORWARD_OPEN: u8 = 0x5B;
    /// Bit set in the reply service code.
    pub const REPLY_MASK: u8 = 0x80;
}

/// A request to the Message Router.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageRequest {
    /// Service code.
    pub service: u8,
    /// Request path.
    pub path: EPath,
    /// Service data.
    pub data: Vec<u8>,
}

impl MessageRequest {
    /// Construct a request.
    pub fn new(service: u8, path: impl Into<EPath>, data: Vec<u8>) -> Self {
        Self {
            service,
            path: path.into(),
            data,
        }
    }

    /// Serialize.
    pub fn encode(&self) -> Result<Vec<u8>> {
        let path = self.path.encode();
        let words = u8::try_from(path.len() / 2).map_err(|_| Error::TooLarge {
            context: "request path",
            len: path.len(),
            max: 2 * usize::from(u8::MAX),
        })?;
        let mut out = Vec::with_capacity(2 + path.len() + self.data.len());
        out.put_u8(self.service);
        out.put_u8(words);
        out.extend_from_slice(&path);
        out.extend_from_slice(&self.data);
        Ok(out)
    }
}

/// General status plus optional extended status words.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CipStatus {
    /// General status (0 = success).
    pub general: u8,
    /// Additional status words.
    pub extended: Vec<u16>,
}

impl CipStatus {
    /// True when general status is 0.
    pub fn is_success(&self) -> bool {
        self.general == 0
    }

    /// Name of the general status code (CIP Vol. 1, appendix B).
    pub fn general_name(&self) -> &'static str {
        general_status_name(self.general)
    }
}

/// Name of a general status code.
pub fn general_status_name(code: u8) -> &'static str {
    match code {
        0x00 => "success",
        0x01 => "connection failure",
        0x02 => "resource unavailable",
        0x03 => "invalid parameter value",
        0x04 => "path segment error",
        0x05 => "path destination unknown",
        0x06 => "partial transfer",
        0x07 => "connection lost",
        0x08 => "service not supported",
        0x09 => "invalid attribute value",
        0x0A => "attribute list error",
        0x0B => "already in requested mode/state",
        0x0C => "object state conflict",
        0x0D => "object already exists",
        0x0E => "attribute not settable",
        0x0F => "privilege violation",
        0x10 => "device state conflict",
        0x11 => "reply data too large",
        0x12 => "fragmentation of a primitive value",
        0x13 => "not enough data",
        0x14 => "attribute not supported",
        0x15 => "too much data",
        0x16 => "object does not exist",
        0x17 => "service fragmentation sequence not in progress",
        0x18 => "no stored attribute data",
        0x19 => "store operation failure",
        0x1A => "routing failure, request packet too large",
        0x1B => "routing failure, response packet too large",
        0x1C => "missing attribute list entry data",
        0x1D => "invalid attribute value list",
        0x1E => "embedded service error",
        0x1F => "vendor specific error",
        0x20 => "invalid parameter",
        0x21 => "write-once value already written",
        0x22 => "invalid reply received",
        0x25 => "key failure in path",
        0x26 => "path size invalid",
        0x27 => "unexpected attribute in list",
        0x28 => "invalid member ID",
        0x29 => "member not settable",
        _ => "unknown status",
    }
}

/// Name of a Connection Manager extended status (general status 0x01).
pub fn connection_manager_ext_name(code: u16) -> Option<&'static str> {
    Some(match code {
        0x0100 => "connection in use or duplicate Forward_Open",
        0x0103 => "transport class and trigger combination not supported",
        0x0106 => "ownership conflict",
        0x0107 => "target connection not found",
        0x0108 => "invalid network connection parameter",
        0x0109 => "invalid connection size",
        0x0110 => "target for connection not configured",
        0x0111 => "RPI not supported",
        0x0113 => "out of connections",
        0x0114 => "vendor ID or product code mismatch",
        0x0115 => "device type mismatch",
        0x0116 => "revision mismatch",
        0x0117 => "invalid produced or consumed application path",
        0x0118 => "invalid or inconsistent configuration application path",
        0x0119 => "non-listen-only connection not opened",
        0x011A => "target object out of connections",
        0x0203 => "connection timed out",
        0x0204 => "unconnected request timed out",
        0x0311 => "invalid port",
        0x0312 => "invalid link address",
        0x0315 => "invalid segment in connection path",
        _ => return None,
    })
}

impl fmt::Display for CipStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "general status {:#04x} ({})",
            self.general,
            self.general_name()
        )?;
        if !self.extended.is_empty() {
            f.write_str(", extended")?;
            for e in &self.extended {
                write!(f, " {e:#06x}")?;
                if let (0x01, Some(n)) = (self.general, connection_manager_ext_name(*e)) {
                    write!(f, " ({n})")?;
                }
            }
        }
        Ok(())
    }
}

/// A Message Router response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageResponse {
    /// Reply service code (request service | 0x80).
    pub service: u8,
    /// Status.
    pub status: CipStatus,
    /// Response data.
    pub data: Vec<u8>,
}

impl MessageResponse {
    /// Parse a response.
    pub fn decode(buf: &[u8]) -> Result<Self> {
        let mut r = Reader::new(buf, "message router response");
        let service = r.u8()?;
        if service & service::REPLY_MASK == 0 {
            return Err(Error::malformed(
                "message router response",
                format!("service {service:#04x} is not a reply"),
            ));
        }
        let _reserved = r.u8()?;
        let general = r.u8()?;
        let ext_words = r.u8()?;
        let extended = (0..ext_words)
            .map(|_| r.u16())
            .collect::<Result<Vec<_>>>()?;
        Ok(Self {
            service,
            status: CipStatus { general, extended },
            data: r.rest().to_vec(),
        })
    }

    /// Serialize (used by tests and mock servers).
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(4 + self.data.len());
        out.put_u8(self.service);
        out.put_u8(0);
        out.put_u8(self.status.general);
        out.put_u8(u8::try_from(self.status.extended.len()).unwrap_or(u8::MAX));
        for e in self.status.extended.iter().take(usize::from(u8::MAX)) {
            out.put_u16(*e);
        }
        out.extend_from_slice(&self.data);
        out
    }

    /// Build a success reply for `request_service` (used by tests and mock servers).
    pub fn success(request_service: u8, data: Vec<u8>) -> Self {
        Self {
            service: request_service | service::REPLY_MASK,
            status: CipStatus::default(),
            data,
        }
    }

    /// Check that this reply matches `req` and has success status; return the data.
    pub fn into_result(self, req: &MessageRequest) -> Result<Vec<u8>> {
        if self.service != req.service | service::REPLY_MASK {
            return Err(Error::malformed(
                "message router response",
                format!(
                    "reply service {:#04x} does not match request {:#04x}",
                    self.service, req.service
                ),
            ));
        }
        if !self.status.is_success() {
            tracing::debug!(
                status = %self.status,
                data = %hex(&self.data),
                "CIP error reply"
            );
            return Err(Error::Cip {
                service: req.service,
                path: req.path.to_string(),
                status: self.status,
            });
        }
        Ok(self.data)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::epath::AttributePath;

    #[test]
    fn encode_get_attribute_single() {
        let req = MessageRequest::new(
            service::GET_ATTRIBUTE_SINGLE,
            AttributePath::new(0x04, 111, 3),
            vec![],
        );
        assert_eq!(
            req.encode().ok(),
            Some(vec![0x0e, 0x03, 0x20, 0x04, 0x24, 0x6f, 0x30, 0x03])
        );
    }

    #[test]
    fn encode_set_attribute_single() {
        let req = MessageRequest::new(
            service::SET_ATTRIBUTE_SINGLE,
            AttributePath::new(0x66, 1, 108),
            vec![0x01],
        );
        assert_eq!(
            req.encode().ok(),
            Some(vec![0x10, 0x03, 0x20, 0x66, 0x24, 0x01, 0x30, 0x6c, 0x01])
        );
    }

    #[test]
    fn decode_success() {
        let r = MessageResponse::decode(&[0x8e, 0x00, 0x00, 0x00, 0x01, 0x02]).expect("decodes");
        assert_eq!(r.service, 0x8e);
        assert!(r.status.is_success());
        assert_eq!(r.data, [1, 2]);
    }

    #[test]
    fn decode_error_with_extended_status() {
        let bytes = [0xd4, 0x00, 0x01, 0x01, 0x00, 0x01, 0xaa];
        let r = MessageResponse::decode(&bytes).expect("decodes");
        assert_eq!(r.status.general, 0x01);
        assert_eq!(r.status.extended, [0x0100]);
        assert_eq!(r.data, [0xaa]);
        assert!(r.status.to_string().contains("duplicate Forward_Open"));
        assert_eq!(r.encode(), bytes);
    }

    #[test]
    fn into_result_maps_errors() {
        let req = MessageRequest::new(
            service::GET_ATTRIBUTE_SINGLE,
            AttributePath::new(0x64, 1, 200),
            vec![],
        );
        let r = MessageResponse::decode(&[0x8e, 0x00, 0x14, 0x00]).expect("decodes");
        let e = r.into_result(&req).expect_err("status 0x14");
        assert!(e.to_string().contains("attribute not supported"));
        assert!(e.to_string().contains("class 0x64/inst 1/attr 200"));

        let wrong = MessageResponse::success(service::SET_ATTRIBUTE_SINGLE, vec![]);
        assert!(wrong.into_result(&req).is_err());
    }

    #[test]
    fn decode_malformed() {
        assert!(MessageResponse::decode(&[]).is_err());
        assert!(MessageResponse::decode(&[0x0e, 0, 0, 0]).is_err()); // not a reply
        assert!(MessageResponse::decode(&[0x8e, 0, 0x01, 0x02, 0x00]).is_err()); // ext truncated
    }
}
