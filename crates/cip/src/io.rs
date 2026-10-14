// Copyright (C), 2026 Quartz Systems. Some rights reserved. This work is
// licensed under the terms of the MIT license which can be found in the
// root directory of this project.

//! Class 1 implicit I/O — **types only, not implemented yet** (phase 3).
//!
//! The planned shape:
//!
//! ```text
//! ImplicitConnection::open(&mut client, ConnectionParams::implicit_class1(..))
//!   ├─ Forward_Open over the existing TCP session (Client::forward_open)
//!   ├─ bind UDP 0.0.0.0:2222, join the T→O multicast group from ForwardOpenReply::t_o_sockaddr
//!   ├─ send O→T every RPI:  CPF[SequencedAddress{o_t_id, seq32}, ConnectedData(seq16 + RunIdleHeader + outputs)]
//!   └─ receive T→O:          CPF[SequencedAddress{t_o_id, seq32}, ConnectedData(seq16 + inputs)]
//! ```
//!
//! Only plain data types live here so the public API can be reviewed before the socket code
//! exists. No sockets are opened by this crate today.

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
}
