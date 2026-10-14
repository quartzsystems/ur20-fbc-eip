// Copyright (C), 2026 Quartz Systems. Some rights reserved. This work is
// licensed under the terms of the MIT license which can be found in the
// root directory of this project.

//! Common Packet Format (CIP Vol. 2, 2-6).
//!
//! A CPF packet is an item count followed by `(type, length, data)` items. Explicit messaging
//! uses an address item plus a data item; class 1 I/O and Forward_Open replies may add
//! sockaddr info items, which are parsed here so phase 3 can use them.

use std::net::{Ipv4Addr, SocketAddrV4};

use crate::codec::{Reader, WriteLe, len_u16};
use crate::error::{Error, Result};

/// CPF item type IDs.
#[allow(missing_docs)]
pub mod item_type {
    pub const NULL_ADDRESS: u16 = 0x0000;
    pub const LIST_IDENTITY: u16 = 0x000C;
    pub const CONNECTED_ADDRESS: u16 = 0x00A1;
    pub const CONNECTED_DATA: u16 = 0x00B1;
    pub const UNCONNECTED_DATA: u16 = 0x00B2;
    pub const LIST_SERVICES: u16 = 0x0100;
    pub const SOCKADDR_O_T: u16 = 0x8000;
    pub const SOCKADDR_T_O: u16 = 0x8001;
    pub const SEQUENCED_ADDRESS: u16 = 0x8002;
}

/// One CPF item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Item {
    /// Null address (unconnected messaging).
    NullAddress,
    /// Connected address: connection ID.
    ConnectedAddress(u32),
    /// Sequenced address (class 1): connection ID and encapsulation sequence number.
    SequencedAddress {
        /// Connection ID.
        connection_id: u32,
        /// 32-bit sequence number.
        sequence: u32,
    },
    /// Unconnected data: a Message Router request/response.
    UnconnectedData(Vec<u8>),
    /// Connected data: for class 3 this starts with a 16-bit sequence count.
    ConnectedData(Vec<u8>),
    /// Socket address info originator→target.
    SockaddrOT(SocketAddrV4),
    /// Socket address info target→originator.
    SockaddrTO(SocketAddrV4),
    /// Any item we do not interpret.
    Other {
        /// Item type ID.
        type_id: u16,
        /// Raw item data.
        data: Vec<u8>,
    },
}

impl Item {
    /// Type ID on the wire.
    pub fn type_id(&self) -> u16 {
        use item_type::*;
        match self {
            Self::NullAddress => NULL_ADDRESS,
            Self::ConnectedAddress(_) => CONNECTED_ADDRESS,
            Self::SequencedAddress { .. } => SEQUENCED_ADDRESS,
            Self::UnconnectedData(_) => UNCONNECTED_DATA,
            Self::ConnectedData(_) => CONNECTED_DATA,
            Self::SockaddrOT(_) => SOCKADDR_O_T,
            Self::SockaddrTO(_) => SOCKADDR_T_O,
            Self::Other { type_id, .. } => *type_id,
        }
    }

    fn encode_data(&self) -> Vec<u8> {
        let mut out = Vec::new();
        match self {
            Self::NullAddress => {}
            Self::ConnectedAddress(id) => out.put_u32(*id),
            Self::SequencedAddress {
                connection_id,
                sequence,
            } => {
                out.put_u32(*connection_id);
                out.put_u32(*sequence);
            }
            Self::UnconnectedData(d) | Self::ConnectedData(d) | Self::Other { data: d, .. } => {
                out.extend_from_slice(d)
            }
            Self::SockaddrOT(a) | Self::SockaddrTO(a) => encode_sockaddr(a, &mut out),
        }
        out
    }

    fn decode(type_id: u16, data: &[u8]) -> Result<Self> {
        use item_type::*;
        let mut r = Reader::new(data, "CPF item");
        let item = match type_id {
            NULL_ADDRESS => Self::NullAddress,
            CONNECTED_ADDRESS => Self::ConnectedAddress(r.u32()?),
            SEQUENCED_ADDRESS => Self::SequencedAddress {
                connection_id: r.u32()?,
                sequence: r.u32()?,
            },
            UNCONNECTED_DATA => return Ok(Self::UnconnectedData(data.to_vec())),
            CONNECTED_DATA => return Ok(Self::ConnectedData(data.to_vec())),
            SOCKADDR_O_T => Self::SockaddrOT(decode_sockaddr(&mut r)?),
            SOCKADDR_T_O => Self::SockaddrTO(decode_sockaddr(&mut r)?),
            _ => {
                return Ok(Self::Other {
                    type_id,
                    data: data.to_vec(),
                });
            }
        };
        if r.remaining() != 0 {
            return Err(Error::malformed(
                "CPF item",
                format!("item {type_id:#06x} has {} extra byte(s)", r.remaining()),
            ));
        }
        Ok(item)
    }
}

/// sockaddr_in as used in CPF items: family, port, address in **big-endian**, 8 zero bytes.
fn encode_sockaddr(a: &SocketAddrV4, out: &mut Vec<u8>) {
    out.extend_from_slice(&2u16.to_be_bytes()); // AF_INET
    out.extend_from_slice(&a.port().to_be_bytes());
    out.extend_from_slice(&a.ip().octets());
    out.extend_from_slice(&[0; 8]);
}

pub(crate) fn decode_sockaddr(r: &mut Reader<'_>) -> Result<SocketAddrV4> {
    let _family = r.u16_be()?;
    let port = r.u16_be()?;
    let ip = Ipv4Addr::from(r.u32_be()?);
    r.take(8)?;
    Ok(SocketAddrV4::new(ip, port))
}

/// Encode a CPF packet.
pub fn encode(items: &[Item]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    out.put_u16(len_u16(items.len(), "CPF item count")?);
    for item in items {
        let data = item.encode_data();
        out.put_u16(item.type_id());
        out.put_u16(len_u16(data.len(), "CPF item")?);
        out.extend_from_slice(&data);
    }
    Ok(out)
}

/// Decode a CPF packet.
pub fn decode(buf: &[u8]) -> Result<Vec<Item>> {
    let mut r = Reader::new(buf, "CPF packet");
    let count = r.u16()?;
    let mut items = Vec::with_capacity(usize::from(count.min(16)));
    for _ in 0..count {
        let type_id = r.u16()?;
        let len = r.u16()?;
        let data = r.take(usize::from(len))?;
        items.push(Item::decode(type_id, data)?);
    }
    Ok(items)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unconnected_request_layout() {
        // Null address + unconnected data containing Get_Attributes_All on 0x01/1.
        let mr = vec![0x01, 0x02, 0x20, 0x01, 0x24, 0x01];
        let bytes =
            encode(&[Item::NullAddress, Item::UnconnectedData(mr.clone())]).expect("encodes");
        assert_eq!(
            bytes,
            [
                0x02, 0x00, // 2 items
                0x00, 0x00, 0x00, 0x00, // null address, len 0
                0xb2, 0x00, 0x06, 0x00, // unconnected data, len 6
                0x01, 0x02, 0x20, 0x01, 0x24, 0x01,
            ]
        );
        let items = decode(&bytes).expect("decodes");
        assert_eq!(items, [Item::NullAddress, Item::UnconnectedData(mr)]);
    }

    #[test]
    fn connected_items() {
        let bytes = encode(&[
            Item::ConnectedAddress(0x1122_3344),
            Item::ConnectedData(vec![0x01, 0x00, 0x0e]),
        ])
        .expect("encodes");
        assert_eq!(
            bytes,
            [
                0x02, 0x00, 0xa1, 0x00, 0x04, 0x00, 0x44, 0x33, 0x22, 0x11, 0xb1, 0x00, 0x03, 0x00,
                0x01, 0x00, 0x0e
            ]
        );
        assert_eq!(decode(&bytes).expect("decodes").len(), 2);
    }

    #[test]
    fn sockaddr_is_big_endian() {
        let a = SocketAddrV4::new(Ipv4Addr::new(192, 168, 1, 10), 2222);
        let bytes = encode(&[Item::SockaddrTO(a)]).expect("encodes");
        assert_eq!(
            bytes,
            [
                0x01, 0x00, 0x01, 0x80, 0x10, 0x00, // 1 item, type 0x8001, len 16
                0x00, 0x02, 0x08, 0xae, 192, 168, 1, 10, 0, 0, 0, 0, 0, 0, 0, 0
            ]
        );
        assert_eq!(decode(&bytes).expect("decodes"), [Item::SockaddrTO(a)]);
    }

    #[test]
    fn malformed_packets() {
        assert!(decode(&[]).is_err());
        assert!(decode(&[0x01, 0x00, 0xb2, 0x00, 0x05, 0x00, 0x01]).is_err());
        // connected address with wrong length
        assert!(decode(&[0x01, 0x00, 0xa1, 0x00, 0x02, 0x00, 0x01, 0x02]).is_err());
        // unknown items are preserved
        assert_eq!(
            decode(&[0x01, 0x00, 0x34, 0x12, 0x01, 0x00, 0xff]).expect("decodes"),
            [Item::Other {
                type_id: 0x1234,
                data: vec![0xff]
            }]
        );
    }
}
