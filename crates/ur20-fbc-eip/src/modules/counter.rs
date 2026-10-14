// Copyright (C), 2026 Quartz Systems. Some rights reserved. This work is
// licensed under the terms of the MIT license which can be found in the
// root directory of this project.

//! Counter modules.
//!
//! **TODO-verify-on-hardware: the whole UR20-2CNT-100 layout below is a placeholder.** It was
//! not available in the documents we had. Before relying on it, compare
//! `ur20eip raw get 0x65 <slot> 113` / `114` lengths with `input_bytes()` / `output_bytes()`
//! (`Coupler::verify_station` does this) and fix the layout from the u-remote manual.
//!
//! Assumed layout, per channel, little-endian:
//!
//! | direction | bytes | content |
//! |---|---|---|
//! | input  | 4 | counter value (DINT) |
//! | input  | 2 | status word |
//! | output | 4 | load / compare value (DINT) |
//! | output | 2 | control word |
//!
//! Channel numbering in [`ChannelValue`] lists: `0..2` are the counter (input) or load
//! (output) values of counter 0 and 1 as [`ChannelValue::Int32`]; `2..4` are the status
//! (input) or control (output) words as [`ChannelValue::UInt16`].

use crate::error::Result;
use crate::module::{
    Category, ChannelValue, Module, ModuleDescriptor, ValueKind, check_count, check_len,
    wrong_kind,
};

const BYTES_PER_COUNTER: usize = 6;

/// A multi-channel 32-bit counter module.
#[derive(Debug)]
pub struct Counter {
    descriptor: &'static ModuleDescriptor,
}

impl Counter {
    /// Codec for `descriptor`.
    pub fn new(descriptor: &'static ModuleDescriptor) -> Self {
        Self { descriptor }
    }

    fn counters(&self) -> usize {
        self.descriptor.channels
    }

    fn decode(&self, data: &[u8], what: &str) -> Result<Vec<ChannelValue>> {
        check_len(self, what, data, self.counters() * BYTES_PER_COUNTER)?;
        let mut values = Vec::with_capacity(2 * self.counters());
        let mut words = Vec::with_capacity(self.counters());
        for chunk in data.chunks_exact(BYTES_PER_COUNTER) {
            let mut v = [0u8; 4];
            v.copy_from_slice(chunk.get(..4).unwrap_or(&[0; 4]));
            let mut w = [0u8; 2];
            w.copy_from_slice(chunk.get(4..6).unwrap_or(&[0; 2]));
            values.push(ChannelValue::Int32(i32::from_le_bytes(v)));
            words.push(ChannelValue::UInt16(u16::from_le_bytes(w)));
        }
        values.extend(words);
        Ok(values)
    }
}

fn create(d: &'static ModuleDescriptor) -> Box<dyn Module> {
    Box::new(Counter::new(d))
}

/// `UR20-2CNT-100`: 2 counters, 100 kHz. Layout TODO-verify-on-hardware.
pub static UR20_2CNT_100: ModuleDescriptor = ModuleDescriptor {
    name: "UR20-2CNT-100",
    id: 0x08C3_3800,
    category: Category::Counter,
    channels: 2,
    create,
};

pub(crate) static DESCRIPTORS: &[&ModuleDescriptor] = &[&UR20_2CNT_100];

impl Module for Counter {
    fn descriptor(&self) -> &'static ModuleDescriptor {
        self.descriptor
    }
    fn input_bits(&self) -> usize {
        8 * BYTES_PER_COUNTER * self.counters()
    }
    fn output_bits(&self) -> usize {
        8 * BYTES_PER_COUNTER * self.counters()
    }
    fn input_channels(&self) -> usize {
        2 * self.counters()
    }
    fn output_channels(&self) -> usize {
        2 * self.counters()
    }
    fn output_kind(&self, channel: usize) -> Option<ValueKind> {
        if channel < self.counters() {
            Some(ValueKind::Int32)
        } else if channel < 2 * self.counters() {
            Some(ValueKind::UInt16)
        } else {
            None
        }
    }
    fn decode_inputs(&self, data: &[u8]) -> Result<Vec<ChannelValue>> {
        self.decode(data, "inputs")
    }
    fn decode_outputs(&self, data: &[u8]) -> Result<Vec<ChannelValue>> {
        self.decode(data, "outputs")
    }
    fn encode_outputs(&self, values: &[ChannelValue], out: &mut [u8]) -> Result<()> {
        check_count(self, values)?;
        check_len(self, "outputs", out, self.output_bytes())?;
        let n = self.counters();
        let mut buf = vec![0u8; out.len()];
        for (ch, v) in values.iter().enumerate() {
            let (offset, bytes) = match v {
                ChannelValue::Int32(x) if ch < n => (ch * BYTES_PER_COUNTER, x.to_le_bytes().to_vec()),
                ChannelValue::UInt16(x) if ch >= n => {
                    ((ch - n) * BYTES_PER_COUNTER + 4, x.to_le_bytes().to_vec())
                }
                other => {
                    let expected = self.output_kind(ch).unwrap_or(ValueKind::Int32);
                    return Err(wrong_kind(self, ch, expected, other));
                }
            };
            if let Some(dst) = buf.get_mut(offset..offset + bytes.len()) {
                dst.copy_from_slice(&bytes);
            }
        }
        out.copy_from_slice(&buf);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ChannelValue::{Int32, UInt16};

    // TODO-verify-on-hardware: invented fixture matching the assumed layout.
    const INPUT_FIXTURE: [u8; 12] = [
        0x10, 0x27, 0x00, 0x00, 0x01, 0x00, // counter 0 = 10000, status 0x0001
        0xff, 0xff, 0xff, 0xff, 0x00, 0x80, // counter 1 = -1, status 0x8000
    ];

    #[test]
    fn decode() {
        let m = UR20_2CNT_100.instantiate();
        assert_eq!(m.input_bytes(), 12);
        assert_eq!(m.output_bytes(), 12);
        assert_eq!(
            m.decode_inputs(&INPUT_FIXTURE).ok(),
            Some(vec![Int32(10000), Int32(-1), UInt16(1), UInt16(0x8000)])
        );
        assert!(m.decode_inputs(&INPUT_FIXTURE[..11]).is_err());
    }

    #[test]
    fn encode() {
        let m = UR20_2CNT_100.instantiate();
        let mut out = [0u8; 12];
        m.encode_outputs(&[Int32(10000), Int32(-1), UInt16(1), UInt16(0x8000)], &mut out)
            .expect("valid");
        assert_eq!(out, INPUT_FIXTURE);
        m.encode_output_channel(3, &UInt16(0x0002), &mut out).expect("valid");
        assert_eq!(out[10..], [0x02, 0x00]);
        assert!(m.encode_output_channel(0, &UInt16(1), &mut out).is_err());
        assert!(m.encode_output_channel(2, &Int32(1), &mut out).is_err());
        assert_eq!(m.output_kind(1), Some(ValueKind::Int32));
        assert_eq!(m.output_kind(2), Some(ValueKind::UInt16));
    }
}
