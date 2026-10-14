// Copyright (C), 2026 Quartz Systems. Some rights reserved. This work is
// licensed under the terms of the MIT license which can be found in the
// root directory of this project.

//! Padded EPATH built from logical segments (CIP Vol. 1, appendix C).

use std::fmt;

use crate::codec::WriteLe;

/// One logical segment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Segment {
    /// Class ID.
    Class(u16),
    /// Instance ID.
    Instance(u32),
    /// Attribute ID.
    Attribute(u16),
    /// Connection point (used in Forward_Open paths to name assemblies).
    ConnectionPoint(u32),
    /// Port segment: backplane/port number and one-byte link address (for routing).
    Port {
        /// Port number (1 = backplane).
        port: u8,
        /// Link address (slot).
        link: u8,
    },
}

impl Segment {
    fn encode_into(&self, out: &mut Vec<u8>) {
        // logical type in bits 2..4, format in bits 0..1: 0 = 8 bit, 1 = 16 bit, 2 = 32 bit
        let logical = |out: &mut Vec<u8>, ty: u8, v: u32| {
            if let Ok(b) = u8::try_from(v) {
                out.put_u8(0x20 | ty);
                out.put_u8(b);
            } else if let Ok(w) = u16::try_from(v) {
                out.put_u8(0x21 | ty);
                out.put_u8(0); // pad
                out.put_u16(w);
            } else {
                out.put_u8(0x22 | ty);
                out.put_u8(0); // pad
                out.put_u32(v);
            }
        };
        match *self {
            Self::Class(c) => logical(out, 0x00, u32::from(c)),
            Self::Instance(i) => logical(out, 0x04, i),
            Self::ConnectionPoint(p) => logical(out, 0x0C, p),
            Self::Attribute(a) => logical(out, 0x10, u32::from(a)),
            Self::Port { port, link } => {
                out.put_u8(port & 0x0F);
                out.put_u8(link);
            }
        }
    }
}

/// An EPATH: a sequence of segments.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct EPath(pub Vec<Segment>);

impl EPath {
    /// Empty path.
    pub fn new() -> Self {
        Self::default()
    }

    /// Append a segment.
    pub fn with(mut self, s: Segment) -> Self {
        self.0.push(s);
        self
    }

    /// Encoded bytes (always an even number: padded format).
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.0.len() * 4);
        for s in &self.0 {
            s.encode_into(&mut out);
        }
        out
    }

    /// Encoded length in 16-bit words.
    pub fn word_len(&self) -> usize {
        self.encode().len() / 2
    }
}

impl fmt::Display for EPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut first = true;
        for s in &self.0 {
            if !first {
                f.write_str("/")?;
            }
            first = false;
            match s {
                Segment::Class(c) => write!(f, "class {c:#04x}")?,
                Segment::Instance(i) => write!(f, "inst {i}")?,
                Segment::Attribute(a) => write!(f, "attr {a}")?,
                Segment::ConnectionPoint(p) => write!(f, "conn-pt {p}")?,
                Segment::Port { port, link } => write!(f, "port {port} link {link}")?,
            }
        }
        if first {
            f.write_str("<empty>")?;
        }
        Ok(())
    }
}

/// Class + instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ObjectPath {
    /// Class ID.
    pub class: u16,
    /// Instance ID (0 addresses the class itself).
    pub instance: u32,
}

impl ObjectPath {
    /// Construct.
    pub const fn new(class: u16, instance: u32) -> Self {
        Self { class, instance }
    }

    /// Address one attribute of this object.
    pub const fn attribute(self, attribute: u16) -> AttributePath {
        AttributePath {
            class: self.class,
            instance: self.instance,
            attribute,
        }
    }
}

impl From<ObjectPath> for EPath {
    fn from(p: ObjectPath) -> Self {
        EPath(vec![Segment::Class(p.class), Segment::Instance(p.instance)])
    }
}

impl fmt::Display for ObjectPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&EPath::from(*self), f)
    }
}

/// Class + instance + attribute.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AttributePath {
    /// Class ID.
    pub class: u16,
    /// Instance ID.
    pub instance: u32,
    /// Attribute ID.
    pub attribute: u16,
}

impl AttributePath {
    /// Construct.
    pub const fn new(class: u16, instance: u32, attribute: u16) -> Self {
        Self {
            class,
            instance,
            attribute,
        }
    }

    /// The object this attribute belongs to.
    pub const fn object(self) -> ObjectPath {
        ObjectPath::new(self.class, self.instance)
    }
}

impl From<AttributePath> for EPath {
    fn from(p: AttributePath) -> Self {
        EPath(vec![
            Segment::Class(p.class),
            Segment::Instance(p.instance),
            Segment::Attribute(p.attribute),
        ])
    }
}

impl fmt::Display for AttributePath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&EPath::from(*self), f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eight_bit_segments() {
        let p: EPath = AttributePath::new(0x04, 111, 3).into();
        assert_eq!(p.encode(), [0x20, 0x04, 0x24, 0x6f, 0x30, 0x03]);
        assert_eq!(p.word_len(), 3);
    }

    #[test]
    fn sixteen_and_thirty_two_bit_segments() {
        let p: EPath = AttributePath::new(0x0300, 0x1234, 0x0101).into();
        assert_eq!(
            p.encode(),
            [
                0x21, 0x00, 0x00, 0x03, 0x25, 0x00, 0x34, 0x12, 0x31, 0x00, 0x01, 0x01
            ]
        );
        let p = EPath::new().with(Segment::Instance(0x0001_0000));
        assert_eq!(p.encode(), [0x26, 0x00, 0x00, 0x00, 0x01, 0x00]);
    }

    #[test]
    fn connection_points_and_ports() {
        let p = EPath::new()
            .with(Segment::Port { port: 1, link: 0 })
            .with(Segment::Class(0x04))
            .with(Segment::Instance(0x66))
            .with(Segment::ConnectionPoint(0x70))
            .with(Segment::ConnectionPoint(0x6f));
        assert_eq!(
            p.encode(),
            [0x01, 0x00, 0x20, 0x04, 0x24, 0x66, 0x2c, 0x70, 0x2c, 0x6f]
        );
    }

    #[test]
    fn display() {
        assert_eq!(
            AttributePath::new(0x04, 111, 3).to_string(),
            "class 0x04/inst 111/attr 3"
        );
        assert_eq!(EPath::new().to_string(), "<empty>");
    }
}
