// Copyright (C), 2026 Quartz Systems. Some rights reserved. This work is
// licensed under the terms of the MIT license which can be found in the
// root directory of this project.

//! Identity Object (class 0x01) and the ListIdentity encapsulation reply.

use std::fmt;
use std::net::SocketAddrV4;

use crate::codec::Reader;
use crate::cpf::decode_sockaddr;
use crate::epath::ObjectPath;
use crate::error::Result;

/// Identity Object instance 1.
pub const IDENTITY_OBJECT: ObjectPath = ObjectPath::new(0x01, 1);

/// Identity Object attributes 1–7 (the required set).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    /// Attr 1: ODVA vendor ID.
    pub vendor_id: u16,
    /// Attr 2: device type (0x0C = communications adapter).
    pub device_type: u16,
    /// Attr 3: product code.
    pub product_code: u16,
    /// Attr 4: revision.
    pub revision: Revision,
    /// Attr 5: status word.
    pub status: IdentityStatus,
    /// Attr 6: serial number.
    pub serial_number: u32,
    /// Attr 7: product name.
    pub product_name: String,
}

/// Major.minor revision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Revision {
    /// Major.
    pub major: u8,
    /// Minor.
    pub minor: u8,
}

impl fmt::Display for Revision {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{:03}", self.major, self.minor)
    }
}

/// Identity status word (attribute 5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IdentityStatus(pub u16);

impl IdentityStatus {
    /// Bit 0: owned by a master.
    pub fn owned(self) -> bool {
        self.0 & 0x0001 != 0
    }
    /// Bit 2: configured (non-default config).
    pub fn configured(self) -> bool {
        self.0 & 0x0004 != 0
    }
    /// Bits 4–7: extended device status.
    pub fn extended_status(self) -> u8 {
        ((self.0 >> 4) & 0x0F) as u8
    }
    /// Bits 8–11: any fault bit (minor/major, recoverable/unrecoverable).
    pub fn faulted(self) -> bool {
        self.0 & 0x0F00 != 0
    }
}

impl fmt::Display for IdentityStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:#06x}", self.0)?;
        let mut flags = Vec::new();
        if self.owned() {
            flags.push("owned");
        }
        if self.configured() {
            flags.push("configured");
        }
        for (bit, name) in [
            (8, "minor recoverable fault"),
            (9, "minor unrecoverable fault"),
            (10, "major recoverable fault"),
            (11, "major unrecoverable fault"),
        ] {
            if self.0 & (1 << bit) != 0 {
                flags.push(name);
            }
        }
        if !flags.is_empty() {
            write!(f, " ({})", flags.join(", "))?;
        }
        Ok(())
    }
}

impl Identity {
    /// Parse Get_Attributes_All data of instance 1. Trailing optional attributes are ignored.
    pub fn decode(data: &[u8]) -> Result<Self> {
        let mut r = Reader::new(data, "Identity object");
        Self::decode_from(&mut r)
    }

    fn decode_from(r: &mut Reader<'_>) -> Result<Self> {
        Ok(Self {
            vendor_id: r.u16()?,
            device_type: r.u16()?,
            product_code: r.u16()?,
            revision: Revision {
                major: r.u8()?,
                minor: r.u8()?,
            },
            status: IdentityStatus(r.u16()?),
            serial_number: r.u32()?,
            product_name: r.short_string()?,
        })
    }
}

impl fmt::Display for Identity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "product name : {}", self.product_name)?;
        writeln!(
            f,
            "vendor id    : {} ({:#06x})",
            self.vendor_id, self.vendor_id
        )?;
        writeln!(
            f,
            "device type  : {} ({:#06x})",
            self.device_type, self.device_type
        )?;
        writeln!(
            f,
            "product code : {} ({:#06x})",
            self.product_code, self.product_code
        )?;
        writeln!(f, "revision     : {}", self.revision)?;
        writeln!(f, "serial number: {:#010x}", self.serial_number)?;
        write!(f, "status       : {}", self.status)
    }
}

/// One entry of a ListIdentity reply (CPF item 0x0C).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListIdentityEntry {
    /// Encapsulation protocol version.
    pub protocol_version: u16,
    /// Socket address the device reports for itself.
    pub socket_addr: SocketAddrV4,
    /// Identity attributes.
    pub identity: Identity,
    /// Device state.
    pub state: u8,
}

impl ListIdentityEntry {
    /// Parse the data of a CPF ListIdentity item.
    pub fn decode(data: &[u8]) -> Result<Self> {
        let mut r = Reader::new(data, "ListIdentity item");
        let protocol_version = r.u16()?;
        let socket_addr = decode_sockaddr(&mut r)?;
        let identity = Identity::decode_from(&mut r)?;
        let state = r.u8()?;
        Ok(Self {
            protocol_version,
            socket_addr,
            identity,
            state,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    /// Get_Attributes_All on 0x01/1 of a UR20-FBC-EIP, read 2026-10-13.
    pub(crate) const IDENTITY_FIXTURE: [u8; 31] = [
        0xf7, 0x03, // vendor 1015 (Weidmüller)
        0x0c, 0x00, // device type: communications adapter
        0xd4, 0x82, // product code 33492
        0x01, 0x05, // revision 1.005
        0x34, 0x00, // status: configured, extended status 3
        0xf3, 0xd1, 0x13, 0x28, // serial
        12, b'U', b'R', b'2', b'0', b'-', b'F', b'B', b'C', b'-', b'E', b'I', b'P', 0xff, 0x00,
        0x00, 0x00, // trailing optional attributes, ignored
    ];

    #[test]
    fn decode_identity() {
        let id = Identity::decode(&IDENTITY_FIXTURE).expect("decodes");
        assert_eq!(id.vendor_id, 1015);
        assert_eq!(id.device_type, 0x0c);
        assert_eq!(id.product_code, 33492);
        assert_eq!(id.revision.to_string(), "1.005");
        assert!(id.status.configured());
        assert!(!id.status.owned());
        assert_eq!(id.status.extended_status(), 3);
        assert!(!id.status.faulted());
        assert_eq!(id.serial_number, 0x2813_d1f3);
        assert_eq!(id.product_name, "UR20-FBC-EIP");
    }

    #[test]
    fn truncated_identity() {
        // attributes 1-7 end after the product name, at byte 27
        for n in 0..27 {
            assert!(Identity::decode(&IDENTITY_FIXTURE[..n]).is_err(), "len {n}");
        }
    }

    #[test]
    fn status_display() {
        assert_eq!(
            IdentityStatus(0x0405).to_string(),
            "0x0405 (owned, configured, major recoverable fault)"
        );
    }

    #[test]
    fn decode_list_identity_entry() {
        let mut data = vec![0x01, 0x00, 0x00, 0x02, 0xaf, 0x12, 192, 168, 1, 10];
        data.extend_from_slice(&[0; 8]);
        data.extend_from_slice(&IDENTITY_FIXTURE[..27]);
        data.push(0x03);
        let e = ListIdentityEntry::decode(&data).expect("decodes");
        assert_eq!(
            e.socket_addr,
            SocketAddrV4::new(Ipv4Addr::new(192, 168, 1, 10), 44818)
        );
        assert_eq!(e.identity.product_name, "UR20-FBC-EIP");
        assert_eq!(e.state, 3);
    }
}
