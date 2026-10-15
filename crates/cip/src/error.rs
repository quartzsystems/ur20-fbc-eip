// Copyright (C), 2026 Quartz Systems. Some rights reserved. This work is
// licensed under the terms of the MIT license which can be found in the
// root directory of this project.

//! Error type for the CIP transport.

use std::time::Duration;

use crate::encap::EncapStatus;
use crate::message::CipStatus;

/// Result alias using [`Error`].
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Everything that can go wrong talking EtherNet/IP.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// Socket-level failure.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// The device did not answer in time.
    #[error("no reply within {0:?}")]
    Timeout(Duration),

    /// The encapsulation layer returned a non-zero status.
    #[error("encapsulation command {command:#06x} failed: {status}")]
    Encapsulation {
        /// Command that failed.
        command: u16,
        /// Status returned by the target.
        status: EncapStatus,
    },

    /// A CIP service returned a non-zero general status.
    #[error("CIP service {service:#04x} on {path} failed: {status}")]
    Cip {
        /// Request service code.
        service: u8,
        /// Request path, formatted.
        path: String,
        /// General and extended status.
        status: CipStatus,
    },

    /// Bytes from the device ended before a structure was complete.
    #[error("truncated {context}: needed {needed} more byte(s), {available} available")]
    Truncated {
        /// What was being parsed.
        context: &'static str,
        /// Bytes needed.
        needed: usize,
        /// Bytes remaining.
        available: usize,
    },

    /// Bytes from the device were well-sized but not valid.
    #[error("malformed {context}: {detail}")]
    Malformed {
        /// What was being parsed.
        context: &'static str,
        /// Explanation.
        detail: String,
    },

    /// Data too large to fit the wire format.
    #[error("{context} is {len} bytes, maximum is {max}")]
    TooLarge {
        /// What was being encoded.
        context: &'static str,
        /// Actual length.
        len: usize,
        /// Maximum length.
        max: usize,
    },

    /// Forward_Open was rejected by the target.
    #[error("Forward_Open rejected: {status}")]
    ForwardOpenRejected {
        /// General and extended status (extended status names the reason).
        status: CipStatus,
    },

    /// Connection parameters that cannot work (e.g. a class 1 size too small for its
    /// headers).
    #[error("invalid connection parameters: {0}")]
    InvalidParams(String),

    /// Connected messaging was requested but no connection is open.
    #[error("no connected (class 3) session is open")]
    NotConnected,

    /// The peer closed the TCP connection.
    #[error("connection closed by peer")]
    Closed,

    /// An earlier timeout or framing error left the TCP stream in an unknown state.
    #[error("connection is unusable after an earlier timeout or framing error; reconnect")]
    Broken,
}

impl Error {
    pub(crate) fn malformed(context: &'static str, detail: impl Into<String>) -> Self {
        Self::Malformed {
            context,
            detail: detail.into(),
        }
    }

    /// The CIP status if this is a CIP-level error.
    pub fn cip_status(&self) -> Option<&CipStatus> {
        match self {
            Self::Cip { status, .. } | Self::ForwardOpenRejected { status } => Some(status),
            _ => None,
        }
    }
}
