// Copyright (C), 2026 Quartz Systems. Some rights reserved. This work is
// licensed under the terms of the MIT license which can be found in the
// root directory of this project.

//! Digital output modules: one bit per channel.
//!
//! Same bit layout as [`super::digital_in`]. Padding bits of 4-channel modules are written as 0.

use crate::error::Result;
use crate::module::{
    Category, ChannelValue, Module, ModuleDescriptor, ValueKind, check_count, check_len,
    wrong_kind,
};
use crate::modules::digital_in::unpack_bits;

/// A digital output module with `descriptor.channels` channels.
#[derive(Debug)]
pub struct DigitalOutput {
    descriptor: &'static ModuleDescriptor,
}

impl DigitalOutput {
    /// Codec for `descriptor`.
    pub fn new(descriptor: &'static ModuleDescriptor) -> Self {
        Self { descriptor }
    }
}

fn create(d: &'static ModuleDescriptor) -> Box<dyn Module> {
    Box::new(DigitalOutput::new(d))
}

macro_rules! dout {
    ($ident:ident, $name:literal, $id:literal, $channels:literal) => {
        #[doc = concat!("`", $name, "`: ", stringify!($channels), " digital outputs.")]
        pub static $ident: ModuleDescriptor = ModuleDescriptor {
            name: $name,
            id: $id,
            category: Category::DigitalOutput,
            channels: $channels,
            create,
        };
    };
}

dout!(UR20_4DO_P, "UR20-4DO-P", 0x0101_2FA0, 4);
dout!(UR20_4DO_P_2A, "UR20-4DO-P-2A", 0x0105_2FA0, 4);
dout!(UR20_8DO_P, "UR20-8DO-P", 0x0102_2FC8, 8);
dout!(UR20_16DO_P, "UR20-16DO-P", 0x0103_AFD0, 16);
dout!(UR20_4RO_CO_255, "UR20-4RO-CO-255", 0x0106_2FA0, 4);

pub(crate) static DESCRIPTORS: &[&ModuleDescriptor] = &[
    &UR20_4DO_P,
    &UR20_4DO_P_2A,
    &UR20_8DO_P,
    &UR20_16DO_P,
    &UR20_4RO_CO_255,
];

impl Module for DigitalOutput {
    fn descriptor(&self) -> &'static ModuleDescriptor {
        self.descriptor
    }
    fn input_bits(&self) -> usize {
        0
    }
    fn output_bits(&self) -> usize {
        self.descriptor.channels
    }
    fn input_channels(&self) -> usize {
        0
    }
    fn output_channels(&self) -> usize {
        self.descriptor.channels
    }
    fn output_kind(&self, channel: usize) -> Option<ValueKind> {
        (channel < self.descriptor.channels).then_some(ValueKind::Bit)
    }
    fn decode_inputs(&self, data: &[u8]) -> Result<Vec<ChannelValue>> {
        check_len(self, "inputs", data, 0)?;
        Ok(Vec::new())
    }
    fn decode_outputs(&self, data: &[u8]) -> Result<Vec<ChannelValue>> {
        check_len(self, "outputs", data, self.output_bytes())?;
        Ok(unpack_bits(data, self.descriptor.channels))
    }
    fn encode_outputs(&self, values: &[ChannelValue], out: &mut [u8]) -> Result<()> {
        check_count(self, values)?;
        check_len(self, "outputs", out, self.output_bytes())?;
        let mut packed = vec![0u8; out.len()];
        for (n, v) in values.iter().enumerate() {
            let on = match v {
                ChannelValue::Bit(b) => *b,
                other => return Err(wrong_kind(self, n, ValueKind::Bit, other)),
            };
            if let Some(byte) = packed.get_mut(n / 8).filter(|_| on) {
                *byte |= 1 << (n % 8);
            }
        }
        out.copy_from_slice(&packed);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ChannelValue::Bit;

    #[test]
    fn encode_four() {
        let m = UR20_4DO_P.instantiate();
        let mut out = [0xFF];
        m.encode_outputs(&[Bit(true), Bit(false), Bit(false), Bit(true)], &mut out)
            .expect("valid");
        assert_eq!(out, [0b0000_1001]); // padding cleared
    }

    #[test]
    fn encode_single_channel_preserves_others() {
        let m = UR20_8DO_P.instantiate();
        let mut out = [0b0000_0101];
        m.encode_output_channel(7, &Bit(true), &mut out).expect("valid");
        assert_eq!(out, [0b1000_0101]);
        m.encode_output_channel(0, &Bit(false), &mut out).expect("valid");
        assert_eq!(out, [0b1000_0100]);
        assert!(m.encode_output_channel(8, &Bit(true), &mut out).is_err());
    }

    #[test]
    fn encode_sixteen() {
        // Verified on a UR20-16DO-P on 2026-10-13: writing module bytes `00 01` lit an LED
        // wired to DO channel 8.
        let m = UR20_16DO_P.instantiate();
        let mut values = vec![Bit(false); 16];
        values[0] = Bit(true);
        values[9] = Bit(true);
        let mut out = [0, 0];
        m.encode_outputs(&values, &mut out).expect("valid");
        assert_eq!(out, [0x01, 0x02]);
        assert_eq!(m.decode_outputs(&out).ok(), Some(values));
    }

    #[test]
    fn rejects_bad_input() {
        let m = UR20_4DO_P.instantiate();
        let mut out = [0];
        assert!(m.encode_outputs(&vec![Bit(true); 3], &mut out).is_err());
        assert!(
            m.encode_outputs(
                &[Bit(true), ChannelValue::Float(1.0), Bit(true), Bit(true)],
                &mut out
            )
            .is_err()
        );
        assert!(m.encode_outputs(&vec![Bit(true); 4], &mut [0, 0]).is_err());
        assert_eq!(out, [0], "buffer untouched on error");
    }
}
