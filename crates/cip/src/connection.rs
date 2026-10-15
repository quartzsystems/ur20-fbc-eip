// Copyright (C), 2026 Quartz Systems. Some rights reserved. This work is
// licensed under the terms of the MIT license which can be found in the
// root directory of this project.

//! Connection Manager services: Forward_Open and Forward_Close (CIP Vol. 1, 3-5).
//!
//! The same [`ConnectionParams`] describe a class 3 explicit connection (implemented) and a
//! class 1 implicit I/O connection (planned): only transport type, sizes, RPI and the
//! connection path differ.

use std::net::SocketAddrV4;
use std::time::Duration;

use crate::codec::{Reader, WriteLe};
use crate::epath::{EPath, ObjectPath, Segment};
use crate::error::{Error, Result};
use crate::message::{MessageRequest, service};

/// Connection Manager object, instance 1.
pub const CONNECTION_MANAGER: ObjectPath = ObjectPath::new(0x06, 1);

/// Message Router object, instance 1: the target of explicit connections.
pub const MESSAGE_ROUTER: ObjectPath = ObjectPath::new(0x02, 1);

/// Transport class.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportClass {
    /// Class 0: no sequence count (rare).
    Class0 = 0,
    /// Class 1: cyclic implicit I/O over UDP with 16-bit sequence count.
    Class1 = 1,
    /// Class 3: explicit messaging over TCP (SendUnitData).
    Class3 = 3,
}

/// Production trigger.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trigger {
    /// Produce every RPI.
    Cyclic = 0,
    /// Produce on change of state (and at the RPI as heartbeat).
    ChangeOfState = 1,
    /// Produce when the application says so (explicit messaging).
    Application = 2,
}

/// Transport type/trigger byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransportType {
    /// Server direction bit. Set for class 3 explicit connections.
    pub server: bool,
    /// Production trigger.
    pub trigger: Trigger,
    /// Transport class.
    pub class: TransportClass,
}

impl TransportType {
    /// Class 3 explicit messaging server: 0xA3.
    pub const EXPLICIT: Self = Self {
        server: true,
        trigger: Trigger::Application,
        class: TransportClass::Class3,
    };

    /// Class 1 cyclic I/O client: 0x01.
    pub const CYCLIC_IO: Self = Self {
        server: false,
        trigger: Trigger::Cyclic,
        class: TransportClass::Class1,
    };

    /// Wire encoding.
    pub fn to_byte(self) -> u8 {
        (u8::from(self.server) << 7) | ((self.trigger as u8) << 4) | (self.class as u8)
    }
}

/// Network connection type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionType {
    /// Null.
    Null = 0,
    /// Multicast (typical for T→O class 1).
    Multicast = 1,
    /// Point to point.
    PointToPoint = 2,
}

/// Connection priority.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Priority {
    /// Low.
    Low = 0,
    /// High.
    High = 1,
    /// Scheduled (typical for class 1).
    Scheduled = 2,
    /// Urgent.
    Urgent = 3,
}

/// Per-direction network connection parameters (16-bit form used by Forward_Open).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NetworkParams {
    /// Redundant owner bit.
    pub redundant_owner: bool,
    /// Connection type.
    pub connection_type: ConnectionType,
    /// Priority.
    pub priority: Priority,
    /// Variable (true) or fixed (false) size.
    pub variable: bool,
    /// Connection size in bytes (max 511). For class 1 this includes the 2-byte sequence
    /// count and, for O→T, the 4-byte run/idle header.
    pub size: u16,
}

impl NetworkParams {
    /// Point-to-point, low priority, variable size: the usual class 3 choice.
    pub const fn explicit(size: u16) -> Self {
        Self {
            redundant_owner: false,
            connection_type: ConnectionType::PointToPoint,
            priority: Priority::Low,
            variable: true,
            size,
        }
    }

    /// Wire encoding.
    pub fn to_word(self) -> Result<u16> {
        if self.size > 0x01FF {
            return Err(Error::TooLarge {
                context: "connection size (use Large_Forward_Open)",
                len: usize::from(self.size),
                max: 0x01FF,
            });
        }
        Ok((u16::from(self.redundant_owner) << 15)
            | ((self.connection_type as u16) << 13)
            | ((self.priority as u16) << 10)
            | (u16::from(self.variable) << 9)
            | self.size)
    }
}

/// Everything needed to send a Forward_Open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectionParams {
    /// Priority/time tick byte for the unconnected request timeout.
    pub priority_time_tick: u8,
    /// Timeout ticks for the unconnected request.
    pub timeout_ticks: u8,
    /// T→O connection ID chosen by us.
    pub t_o_connection_id: u32,
    /// Connection serial number (part of the connection triad).
    pub connection_serial: u16,
    /// Originator vendor ID (part of the connection triad).
    pub vendor_id: u16,
    /// Originator serial number (part of the connection triad).
    pub originator_serial: u32,
    /// Connection timeout = RPI × 4 × 2^multiplier.
    pub timeout_multiplier: u8,
    /// Requested packet interval O→T.
    pub o_t_rpi: Duration,
    /// O→T parameters.
    pub o_t: NetworkParams,
    /// Requested packet interval T→O.
    pub t_o_rpi: Duration,
    /// T→O parameters.
    pub t_o: NetworkParams,
    /// Transport type/trigger.
    pub transport: TransportType,
    /// Connection path (e.g. Message Router for class 3, assembly connection points for class 1).
    pub path: EPath,
}

impl ConnectionParams {
    /// A class 3 explicit connection to the Message Router.
    ///
    /// RPI 1 s with multiplier 2 gives a 16 s inactivity timeout.
    pub fn explicit_class3() -> Self {
        Self {
            priority_time_tick: 0x0A,
            timeout_ticks: 0x0E,
            t_o_connection_id: 0,
            connection_serial: 0,
            vendor_id: 0xFFFE,
            originator_serial: 0x5552_3230, // "UR20"
            timeout_multiplier: 2,
            o_t_rpi: Duration::from_secs(1),
            o_t: NetworkParams::explicit(500),
            t_o_rpi: Duration::from_secs(1),
            t_o: NetworkParams::explicit(500),
            transport: TransportType::EXPLICIT,
            path: MESSAGE_ROUTER.into(),
        }
    }

    /// Class 1 parameters for an exclusive-owner connection to assembly connection points,
    /// for [`crate::ImplicitConnection::open`].
    ///
    /// `o_t_bytes` / `t_o_bytes` are the assembly sizes; the 2-byte sequence count (both
    /// directions) and the 4-byte run/idle header (O→T) are added here. T→O is point to
    /// point; set `t_o.connection_type` to [`ConnectionType::Multicast`] for multicast.
    /// Timeout multiplier 2: the connection times out after 16 × RPI without data.
    pub fn implicit_class1(
        config_instance: u32,
        output_instance: u32,
        input_instance: u32,
        o_t_bytes: u16,
        t_o_bytes: u16,
        rpi: Duration,
    ) -> Self {
        let class1 = |size: u16, connection_type| NetworkParams {
            redundant_owner: false,
            connection_type,
            priority: Priority::Scheduled,
            variable: false,
            size,
        };
        Self {
            timeout_multiplier: 2,
            o_t_rpi: rpi,
            // 2-byte sequence count + 4-byte run/idle header
            o_t: class1(o_t_bytes.saturating_add(6), ConnectionType::PointToPoint),
            t_o_rpi: rpi,
            // 2-byte sequence count; T→O has no run/idle header on most adapters
            t_o: class1(t_o_bytes.saturating_add(2), ConnectionType::PointToPoint),
            transport: TransportType::CYCLIC_IO,
            path: EPath::new()
                .with(Segment::Class(0x04))
                .with(Segment::Instance(config_instance))
                .with(Segment::ConnectionPoint(output_instance))
                .with(Segment::ConnectionPoint(input_instance)),
            ..Self::explicit_class3()
        }
    }

    /// Connection timeout for data arriving at `rpi`: RPI × 4 × 2^multiplier.
    pub fn timeout_for(&self, rpi: Duration) -> Duration {
        let factor = 4u32.saturating_mul(1 << self.timeout_multiplier.min(7));
        rpi.saturating_mul(factor)
    }

    fn rpi_us(d: Duration) -> u32 {
        u32::try_from(d.as_micros()).unwrap_or(u32::MAX)
    }

    /// Build the Forward_Open request.
    pub fn forward_open_request(&self) -> Result<MessageRequest> {
        let path = self.path.encode();
        let mut d = Vec::with_capacity(36 + path.len());
        d.put_u8(self.priority_time_tick);
        d.put_u8(self.timeout_ticks);
        d.put_u32(0); // O→T connection ID: chosen by target
        d.put_u32(self.t_o_connection_id);
        d.put_u16(self.connection_serial);
        d.put_u16(self.vendor_id);
        d.put_u32(self.originator_serial);
        d.put_u8(self.timeout_multiplier);
        d.extend_from_slice(&[0, 0, 0]);
        d.put_u32(Self::rpi_us(self.o_t_rpi));
        d.put_u16(self.o_t.to_word()?);
        d.put_u32(Self::rpi_us(self.t_o_rpi));
        d.put_u16(self.t_o.to_word()?);
        d.put_u8(self.transport.to_byte());
        d.put_u8(path_words(&path)?);
        d.extend_from_slice(&path);
        Ok(MessageRequest::new(
            service::FORWARD_OPEN,
            CONNECTION_MANAGER,
            d,
        ))
    }

    /// Build the matching Forward_Close request.
    pub fn forward_close_request(&self) -> Result<MessageRequest> {
        let path = self.path.encode();
        let mut d = Vec::with_capacity(12 + path.len());
        d.put_u8(self.priority_time_tick);
        d.put_u8(self.timeout_ticks);
        d.put_u16(self.connection_serial);
        d.put_u16(self.vendor_id);
        d.put_u32(self.originator_serial);
        d.put_u8(path_words(&path)?);
        d.put_u8(0);
        d.extend_from_slice(&path);
        Ok(MessageRequest::new(
            service::FORWARD_CLOSE,
            CONNECTION_MANAGER,
            d,
        ))
    }
}

fn path_words(path: &[u8]) -> Result<u8> {
    u8::try_from(path.len() / 2).map_err(|_| Error::TooLarge {
        context: "connection path",
        len: path.len(),
        max: 2 * usize::from(u8::MAX),
    })
}

/// Successful Forward_Open reply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForwardOpenReply {
    /// Connection ID to put in O→T packets (chosen by the target).
    pub o_t_connection_id: u32,
    /// Connection ID the target uses in T→O packets.
    pub t_o_connection_id: u32,
    /// Echo of the connection serial.
    pub connection_serial: u16,
    /// Echo of the originator vendor ID.
    pub vendor_id: u16,
    /// Echo of the originator serial.
    pub originator_serial: u32,
    /// Actual packet interval O→T.
    pub o_t_api: Duration,
    /// Actual packet interval T→O.
    pub t_o_api: Duration,
    /// Application reply data.
    pub application_reply: Vec<u8>,
    /// O→T sockaddr item from the CPF reply, if any (class 1).
    pub o_t_sockaddr: Option<SocketAddrV4>,
    /// T→O sockaddr item from the CPF reply, if any (class 1 multicast address).
    pub t_o_sockaddr: Option<SocketAddrV4>,
}

impl ForwardOpenReply {
    /// Parse the success reply data.
    pub fn decode(data: &[u8]) -> Result<Self> {
        let mut r = Reader::new(data, "Forward_Open reply");
        let o_t_connection_id = r.u32()?;
        let t_o_connection_id = r.u32()?;
        let connection_serial = r.u16()?;
        let vendor_id = r.u16()?;
        let originator_serial = r.u32()?;
        let o_t_api = Duration::from_micros(u64::from(r.u32()?));
        let t_o_api = Duration::from_micros(u64::from(r.u32()?));
        let app_words = r.u8()?;
        let _reserved = r.u8()?;
        let application_reply = r.take(usize::from(app_words) * 2)?.to_vec();
        Ok(Self {
            o_t_connection_id,
            t_o_connection_id,
            connection_serial,
            vendor_id,
            originator_serial,
            o_t_api,
            t_o_api,
            application_reply,
            o_t_sockaddr: None,
            t_o_sockaddr: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transport_bytes() {
        assert_eq!(TransportType::EXPLICIT.to_byte(), 0xA3);
        assert_eq!(TransportType::CYCLIC_IO.to_byte(), 0x01);
    }

    #[test]
    fn network_params_word() {
        assert_eq!(NetworkParams::explicit(500).to_word().ok(), Some(0x43F4));
        assert!(NetworkParams::explicit(512).to_word().is_err());
        let p = NetworkParams {
            redundant_owner: false,
            connection_type: ConnectionType::Multicast,
            priority: Priority::Scheduled,
            variable: false,
            size: 34,
        };
        assert_eq!(p.to_word().ok(), Some(0x2822));
    }

    #[test]
    fn forward_open_explicit_bytes() {
        let mut p = ConnectionParams::explicit_class3();
        p.t_o_connection_id = 0x1122_3344;
        p.connection_serial = 0x0102;
        let req = p.forward_open_request().expect("encodes");
        let bytes = req.encode().expect("encodes");
        let expected: Vec<u8> = vec![
            0x54, 0x02, 0x20, 0x06, 0x24, 0x01, // Forward_Open → Connection Manager
            0x0a, 0x0e, // priority/tick, timeout ticks
            0x00, 0x00, 0x00, 0x00, // O→T id
            0x44, 0x33, 0x22, 0x11, // T→O id
            0x02, 0x01, // connection serial
            0xfe, 0xff, // vendor
            0x30, 0x32, 0x52, 0x55, // originator serial
            0x02, 0x00, 0x00, 0x00, // multiplier + reserved
            0x40, 0x42, 0x0f, 0x00, 0xf4, 0x43, // O→T RPI 1 s, params
            0x40, 0x42, 0x0f, 0x00, 0xf4, 0x43, // T→O RPI 1 s, params
            0xa3, // transport
            0x02, 0x20, 0x02, 0x24, 0x01, // path: Message Router
        ];
        assert_eq!(bytes, expected);
    }

    #[test]
    fn forward_close_bytes() {
        let mut p = ConnectionParams::explicit_class3();
        p.connection_serial = 0x0102;
        let bytes = p
            .forward_close_request()
            .and_then(|r| r.encode())
            .expect("encodes");
        assert_eq!(
            bytes,
            [
                0x4e, 0x02, 0x20, 0x06, 0x24, 0x01, 0x0a, 0x0e, 0x02, 0x01, 0xfe, 0xff, 0x30, 0x32,
                0x52, 0x55, 0x02, 0x00, 0x20, 0x02, 0x24, 0x01
            ]
        );
    }

    #[test]
    fn class1_path_matches_eds() {
        let p = ConnectionParams::implicit_class1(
            1, // config instance, from the Weidmüller EDS "Exclusive Owner Unpacked"
            112,
            111,
            10,
            12,
            Duration::from_millis(10),
        );
        // UR20-FBC-EIP EDS v1.6, Connection9 "Exclusive Owner Unpacked":
        // "20 04 24 01 2C 70 2C 6F"
        assert_eq!(
            p.path.encode(),
            [0x20, 0x04, 0x24, 0x01, 0x2c, 0x70, 0x2c, 0x6f]
        );
        assert_eq!(p.o_t.size, 16);
        assert_eq!(p.t_o.size, 14);
        assert_eq!(p.transport.to_byte(), 0x01);
        assert_eq!(p.t_o.connection_type, ConnectionType::PointToPoint);
        // fixed size, scheduled, point to point: 0x4800 | size
        assert_eq!(p.t_o.to_word().ok(), Some(0x480E));
        assert_eq!(
            p.timeout_for(Duration::from_millis(10)),
            Duration::from_millis(160)
        );
        let mut q = p.clone();
        q.timeout_multiplier = 0;
        assert_eq!(
            q.timeout_for(Duration::from_millis(10)),
            Duration::from_millis(40)
        );
        q.timeout_multiplier = 200; // clamped, no overflow
        assert!(q.timeout_for(Duration::from_secs(1)) >= Duration::from_secs(512));
    }

    #[test]
    fn decode_forward_open_reply() {
        let data = [
            0x78, 0x56, 0x34, 0x12, // O→T id
            0x44, 0x33, 0x22, 0x11, // T→O id
            0x02, 0x01, 0xfe, 0xff, 0x30, 0x32, 0x52, 0x55, // triad
            0x40, 0x42, 0x0f, 0x00, 0x80, 0x84, 0x1e, 0x00, // APIs
            0x01, 0x00, 0xaa, 0xbb, // 1 word app reply
        ];
        let r = ForwardOpenReply::decode(&data).expect("decodes");
        assert_eq!(r.o_t_connection_id, 0x1234_5678);
        assert_eq!(r.t_o_connection_id, 0x1122_3344);
        assert_eq!(r.o_t_api, Duration::from_secs(1));
        assert_eq!(r.t_o_api, Duration::from_secs(2));
        assert_eq!(r.application_reply, [0xaa, 0xbb]);
        assert!(ForwardOpenReply::decode(&data[..27]).is_err());
    }
}
