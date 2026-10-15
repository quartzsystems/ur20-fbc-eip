// Copyright (C), 2026 Quartz Systems. Some rights reserved. This work is
// licensed under the terms of the MIT license which can be found in the
// root directory of this project.

//! Class 1 implicit I/O packets (CIP Vol. 2, 3-2.2): the pure, socket-free part.
//!
//! A class 1 UDP datagram is a bare Common Packet Format packet, with no encapsulation
//! header:
//!
//! ```text
//! CPF[ SequencedAddress { connection_id, encapsulation sequence (u32) },
//!      ConnectedData    { CIP sequence count (u16), [run/idle header (u32)], data } ]
//! ```
//!
//! O→T data carries the 32-bit run/idle header; T→O data normally does not (the
//! connection's "real-time format" decides). [`crate::implicit`] runs the sockets.
//!
//! TODO-verify-on-hardware: the byte layout follows the CIP specification and has not yet
//! been captured from a UR20-FBC-EIP.

use crate::codec::{Reader, WriteLe};
use crate::cpf::{self, Item};
use crate::error::{Error, Result};

/// UDP port for class 1 I/O.
pub const IO_PORT: u16 = 2222;

/// The 32-bit run/idle header prepended to O→T data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RunIdleHeader(pub u32);

impl RunIdleHeader {
    /// Run mode: outputs are applied.
    pub const RUN: Self = Self(1);
    /// Idle mode: outputs go to their configured idle state.
    pub const IDLE: Self = Self(0);

    /// True when bit 0 (run) is set.
    pub fn is_run(self) -> bool {
        self.0 & 1 != 0
    }
}

/// 16-bit CIP sequence count carried in connected data (wraps).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SequenceCount(pub u16);

impl SequenceCount {
    /// Advance, wrapping at 0xFFFF.
    pub fn next(self) -> Self {
        Self(self.0.wrapping_add(1))
    }

    /// True if `other` is newer than `self` under serial-number arithmetic.
    pub fn is_newer(self, other: Self) -> bool {
        let diff = other.0.wrapping_sub(self.0);
        diff != 0 && diff < 0x8000
    }
}

/// One class 1 datagram, either direction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IoPacket {
    /// Connection ID from the sequenced address item.
    pub connection_id: u32,
    /// Encapsulation sequence number (32 bit), incremented for every packet sent.
    pub encap_sequence: u32,
    /// CIP sequence count (16 bit).
    pub sequence: SequenceCount,
    /// Run/idle header, if the direction carries one.
    pub run_idle: Option<RunIdleHeader>,
    /// Application data (the assembly).
    pub data: Vec<u8>,
}

impl IoPacket {
    /// Encode as a CPF datagram.
    pub fn encode(&self) -> Result<Vec<u8>> {
        let mut payload = Vec::with_capacity(6 + self.data.len());
        payload.put_u16(self.sequence.0);
        if let Some(h) = self.run_idle {
            payload.put_u32(h.0);
        }
        payload.extend_from_slice(&self.data);
        cpf::encode(&[
            Item::SequencedAddress {
                connection_id: self.connection_id,
                sequence: self.encap_sequence,
            },
            Item::ConnectedData(payload),
        ])
    }

    /// Decode a CPF datagram. `run_idle` says whether the connected data starts (after the
    /// sequence count) with a run/idle header.
    pub fn decode(bytes: &[u8], run_idle: bool) -> Result<Self> {
        let mut address = None;
        let mut payload = None;
        for item in cpf::decode(bytes)? {
            match item {
                Item::SequencedAddress {
                    connection_id,
                    sequence,
                } => address = Some((connection_id, sequence)),
                Item::ConnectedData(d) => payload = Some(d),
                _ => {}
            }
        }
        let (connection_id, encap_sequence) = address
            .ok_or_else(|| Error::malformed("class 1 packet", "no sequenced address item"))?;
        let payload =
            payload.ok_or_else(|| Error::malformed("class 1 packet", "no connected data item"))?;
        let mut r = Reader::new(&payload, "class 1 data");
        let sequence = SequenceCount(r.u16()?);
        let run_idle = if run_idle {
            Some(RunIdleHeader(r.u32()?))
        } else {
            None
        };
        Ok(Self {
            connection_id,
            encap_sequence,
            sequence,
            run_idle,
            data: r.rest().to_vec(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sequence_wraps() {
        assert_eq!(SequenceCount(0xFFFF).next(), SequenceCount(0));
        assert!(SequenceCount(0xFFFF).is_newer(SequenceCount(0)));
        assert!(!SequenceCount(5).is_newer(SequenceCount(5)));
        assert!(!SequenceCount(5).is_newer(SequenceCount(4)));
    }

    #[test]
    fn run_idle() {
        assert!(RunIdleHeader::RUN.is_run());
        assert!(!RunIdleHeader::IDLE.is_run());
    }

    // TODO-verify-on-hardware: built from CIP Vol. 2 3-2.2, not captured.
    #[rustfmt::skip]
    const O_T: [u8; 32] = [
        0x02, 0x00,                         // 2 items
        0x02, 0x80, 0x08, 0x00,             // sequenced address, 8 bytes
        0x01, 0x00, 0x00, 0x10,             //   connection ID 0x1000_0001
        0x07, 0x00, 0x00, 0x00,             //   encapsulation sequence 7
        0xb1, 0x00, 0x0a, 0x00,             // connected data, 10 bytes
        0x03, 0x00,                         //   CIP sequence count 3
        0x01, 0x00, 0x00, 0x00,             //   run/idle: run
        0x00, 0x00, 0x00, 0x01,             //   assembly 112: header 0, DO ch 8
        0x00, 0x00, 0x00, 0x00,             // (padding, not part of the packet)
    ];

    #[test]
    fn o_t_fixture_round_trip() {
        let p = IoPacket {
            connection_id: 0x1000_0001,
            encap_sequence: 7,
            sequence: SequenceCount(3),
            run_idle: Some(RunIdleHeader::RUN),
            data: vec![0, 0, 0, 1],
        };
        let bytes = p.encode().expect("encodes");
        assert_eq!(bytes, O_T[..28]);
        assert_eq!(IoPacket::decode(&bytes, true).ok(), Some(p));
    }

    #[test]
    fn t_o_without_run_idle() {
        let p = IoPacket {
            connection_id: 0xC0DE,
            encap_sequence: u32::MAX,
            sequence: SequenceCount(0xFFFF),
            run_idle: None,
            data: vec![0x00, 0x20, 0x00, 0x01],
        };
        let bytes = p.encode().expect("encodes");
        assert_eq!(IoPacket::decode(&bytes, false).ok(), Some(p.clone()));
        // read with the wrong real-time format, the header eats the data
        let wrong = IoPacket::decode(&bytes, true).expect("still parses");
        assert_eq!(wrong.data, Vec::<u8>::new());
    }

    #[test]
    fn malformed_packets_are_errors() {
        assert!(IoPacket::decode(&[], false).is_err());
        assert!(IoPacket::decode(&O_T[..13], true).is_err()); // cut inside an item
        // only an address item
        let only_address = cpf::encode(&[Item::SequencedAddress {
            connection_id: 1,
            sequence: 1,
        }])
        .expect("encodes");
        assert!(IoPacket::decode(&only_address, false).is_err());
        // data too short for the sequence count
        let short = cpf::encode(&[
            Item::SequencedAddress {
                connection_id: 1,
                sequence: 1,
            },
            Item::ConnectedData(vec![1]),
        ])
        .expect("encodes");
        assert!(IoPacket::decode(&short, false).is_err());
    }
}
