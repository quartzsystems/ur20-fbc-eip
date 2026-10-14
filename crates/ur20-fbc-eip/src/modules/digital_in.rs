// Copyright (C), 2026 Quartz Systems. Some rights reserved. This work is
// licensed under the terms of the MIT license which can be found in the
// root directory of this project.

//! Digital input modules: one bit per channel.
//!
//! Channel *n* is bit `n % 8` of byte `n / 8` of the module's input data.
//! In the unpacked assemblies (111/113) a 4-channel module occupies one byte whose upper four
//! bits are padding (QSG0028 §3.2). Verified on a UR20-16DI-P on 2026-10-13: 24 V on channel 8
//! gave module bytes `00 01`, and the coupler web server showed channel 8 = 1.

use crate::error::Result;
use crate::module::{
    Category, ChannelValue, Module, ModuleDescriptor, ValueKind, check_len,
};

/// A digital input module with `descriptor.channels` channels.
#[derive(Debug)]
pub struct DigitalInput {
    descriptor: &'static ModuleDescriptor,
}

impl DigitalInput {
    /// Codec for `descriptor`.
    pub fn new(descriptor: &'static ModuleDescriptor) -> Self {
        Self { descriptor }
    }
}

fn create(d: &'static ModuleDescriptor) -> Box<dyn Module> {
    Box::new(DigitalInput::new(d))
}

macro_rules! di {
    ($ident:ident, $name:literal, $id:literal, $channels:literal) => {
        #[doc = concat!("`", $name, "`: ", stringify!($channels), " digital inputs.")]
        pub static $ident: ModuleDescriptor = ModuleDescriptor {
            name: $name,
            id: $id,
            category: Category::DigitalInput,
            channels: $channels,
            create,
        };
    };
}

di!(UR20_4DI_P, "UR20-4DI-P", 0x0009_1F84, 4);
di!(UR20_4DI_P_3W, "UR20-4DI-P-3W", 0x001B_1F84, 4);
di!(UR20_8DI_P_2W, "UR20-8DI-P-2W", 0x0013_1FC1, 8);
di!(UR20_8DI_P_3W, "UR20-8DI-P-3W", 0x000A_1FC1, 8);
di!(UR20_16DI_P, "UR20-16DI-P", 0x0004_9FC2, 16);

pub(crate) static DESCRIPTORS: &[&ModuleDescriptor] = &[
    &UR20_4DI_P,
    &UR20_4DI_P_3W,
    &UR20_8DI_P_2W,
    &UR20_8DI_P_3W,
    &UR20_16DI_P,
];

/// Read `count` bits, channel n at byte n/8 bit n%8.
pub(crate) fn unpack_bits(data: &[u8], count: usize) -> Vec<ChannelValue> {
    (0..count)
        .map(|n| {
            let byte = data.get(n / 8).copied().unwrap_or(0);
            ChannelValue::Bit(byte & (1 << (n % 8)) != 0)
        })
        .collect()
}

impl Module for DigitalInput {
    fn descriptor(&self) -> &'static ModuleDescriptor {
        self.descriptor
    }
    fn input_bits(&self) -> usize {
        self.descriptor.channels
    }
    fn output_bits(&self) -> usize {
        0
    }
    fn input_channels(&self) -> usize {
        self.descriptor.channels
    }
    fn output_channels(&self) -> usize {
        0
    }
    fn output_kind(&self, _channel: usize) -> Option<ValueKind> {
        None
    }
    fn decode_inputs(&self, data: &[u8]) -> Result<Vec<ChannelValue>> {
        check_len(self, "inputs", data, self.input_bytes())?;
        Ok(unpack_bits(data, self.descriptor.channels))
    }
    fn decode_outputs(&self, data: &[u8]) -> Result<Vec<ChannelValue>> {
        check_len(self, "outputs", data, 0)?;
        Ok(Vec::new())
    }
    fn encode_outputs(&self, values: &[ChannelValue], out: &mut [u8]) -> Result<()> {
        crate::module::check_count(self, values)?;
        check_len(self, "outputs", out, 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ChannelValue::Bit;

    #[test]
    fn four_channels_ignore_padding_bits() {
        let m = UR20_4DI_P.instantiate();
        assert_eq!(m.input_bits(), 4);
        assert_eq!(m.input_bytes(), 1);
        assert_eq!(m.output_bytes(), 0);
        // upper nibble is padding and must be ignored
        assert_eq!(
            m.decode_inputs(&[0b1111_0101]).ok(),
            Some(vec![Bit(true), Bit(false), Bit(true), Bit(false)])
        );
        assert!(m.decode_inputs(&[]).is_err());
        assert!(m.decode_inputs(&[0, 0]).is_err());
    }

    #[test]
    fn eight_channels() {
        let m = UR20_8DI_P_3W.instantiate();
        let v = m.decode_inputs(&[0b1000_0001]).expect("1 byte");
        assert_eq!(v.len(), 8);
        assert_eq!(v[0], Bit(true));
        assert_eq!(v[6], Bit(false));
        assert_eq!(v[7], Bit(true));
    }

    #[test]
    fn sixteen_channels_little_endian() {
        let m = UR20_16DI_P.instantiate();
        assert_eq!(m.input_bytes(), 2);
        let v = m.decode_inputs(&[0x01, 0x80]).expect("2 bytes");
        let on: Vec<usize> = v
            .iter()
            .enumerate()
            .filter(|(_, x)| **x == Bit(true))
            .map(|(i, _)| i)
            .collect();
        assert_eq!(on, [0, 15]);
    }

    #[test]
    fn sixteen_channels_hardware_fixture() {
        // UR20-16DI-P, 2026-10-13: 24 V on channel 8 (web server: ch 8 = 1).
        let m = UR20_16DI_P.instantiate();
        let v = m.decode_inputs(&[0x00, 0x01]).expect("2 bytes");
        let on: Vec<usize> = (0..16).filter(|i| v[*i] == Bit(true)).collect();
        assert_eq!(on, [8]);
    }

    #[test]
    fn no_outputs() {
        let m = UR20_4DI_P.instantiate();
        assert!(m.encode_outputs(&[], &mut []).is_ok());
        assert!(m.encode_outputs(&[Bit(true)], &mut []).is_err());
        assert!(m.encode_output_channel(0, &Bit(true), &mut []).is_err());
    }
}
