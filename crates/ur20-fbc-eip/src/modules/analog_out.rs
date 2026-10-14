// Copyright (C), 2026 Quartz Systems. Some rights reserved. This work is
// licensed under the terms of the MIT license which can be found in the
// root directory of this project.

//! Analog output modules (16-bit, one INT per channel).
//!
//! Same byte layout and scaling rules as [`super::analog_in`].
//! TODO-verify-on-hardware: byte order of analog channels in assembly 112.

use crate::error::{Error, Result};
use crate::module::{
    Category, ChannelValue, Module, ModuleDescriptor, ValueKind, check_count, check_len,
};
use crate::modules::analog_in::{Scaling, raw_to_value, read_i16s, value_to_raw};

/// A 16-bit analog output module.
#[derive(Debug)]
pub struct AnalogOutput {
    descriptor: &'static ModuleDescriptor,
    scaling: Vec<Option<Scaling>>,
}

impl AnalogOutput {
    /// Codec taking raw counts ([`ChannelValue::Int16`]).
    pub fn new(descriptor: &'static ModuleDescriptor) -> Self {
        Self {
            descriptor,
            scaling: vec![None; descriptor.channels],
        }
    }

    /// Accept engineering values ([`ChannelValue::Float`]) on scaled channels.
    /// Raw [`ChannelValue::Int16`] is always accepted.
    pub fn with_scaling(
        descriptor: &'static ModuleDescriptor,
        scaling: &[Option<Scaling>],
    ) -> Result<Self> {
        if scaling.len() != descriptor.channels {
            return Err(Error::ValueCount {
                module: descriptor.name,
                expected: descriptor.channels,
                actual: scaling.len(),
            });
        }
        Ok(Self {
            descriptor,
            scaling: scaling.to_vec(),
        })
    }
}

fn create(d: &'static ModuleDescriptor) -> Box<dyn Module> {
    Box::new(AnalogOutput::new(d))
}

/// `UR20-4AO-UI-16`: 4 analog outputs, ±10 V / 0–20 mA, 16 bit.
pub static UR20_4AO_UI_16: ModuleDescriptor = ModuleDescriptor {
    name: "UR20-4AO-UI-16",
    id: 0x0502_25E0,
    category: Category::AnalogOutput,
    channels: 4,
    create,
};

/// `UR20-4AO-UI-16-DIAG`: as 4AO-UI-16 with diagnostics; same process data.
pub static UR20_4AO_UI_16_DIAG: ModuleDescriptor = ModuleDescriptor {
    name: "UR20-4AO-UI-16-DIAG",
    id: 0x0501_2560,
    category: Category::AnalogOutput,
    channels: 4,
    create,
};

pub(crate) static DESCRIPTORS: &[&ModuleDescriptor] = &[&UR20_4AO_UI_16, &UR20_4AO_UI_16_DIAG];

impl Module for AnalogOutput {
    fn descriptor(&self) -> &'static ModuleDescriptor {
        self.descriptor
    }
    fn input_bits(&self) -> usize {
        0
    }
    fn output_bits(&self) -> usize {
        16 * self.descriptor.channels
    }
    fn input_channels(&self) -> usize {
        0
    }
    fn output_channels(&self) -> usize {
        self.descriptor.channels
    }
    fn output_kind(&self, channel: usize) -> Option<ValueKind> {
        self.scaling.get(channel).map(|s| {
            if s.is_some() {
                ValueKind::Float
            } else {
                ValueKind::Int16
            }
        })
    }
    fn decode_inputs(&self, data: &[u8]) -> Result<Vec<ChannelValue>> {
        check_len(self, "inputs", data, 0)?;
        Ok(Vec::new())
    }
    fn decode_outputs(&self, data: &[u8]) -> Result<Vec<ChannelValue>> {
        check_len(self, "outputs", data, self.output_bytes())?;
        Ok(read_i16s(data, self.descriptor.channels)
            .into_iter()
            .zip(&self.scaling)
            .map(|(raw, s)| raw_to_value(raw, *s))
            .collect())
    }
    fn encode_outputs(&self, values: &[ChannelValue], out: &mut [u8]) -> Result<()> {
        check_count(self, values)?;
        check_len(self, "outputs", out, self.output_bytes())?;
        let raws = values
            .iter()
            .enumerate()
            .map(|(ch, v)| value_to_raw(self, ch, v, self.scaling.get(ch).copied().flatten()))
            .collect::<Result<Vec<i16>>>()?;
        for (chunk, raw) in out.chunks_exact_mut(2).zip(raws) {
            chunk.copy_from_slice(&raw.to_le_bytes());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modules::analog_in::AnalogRange;
    use ChannelValue::{Bit, Float, Int16};

    #[test]
    fn encode_raw() {
        let m = UR20_4AO_UI_16.instantiate();
        assert_eq!(m.output_bytes(), 8);
        assert_eq!(m.output_kind(0), Some(ValueKind::Int16));
        let mut out = [0u8; 8];
        m.encode_outputs(&[Int16(27648), Int16(0), Int16(-27648), Int16(1)], &mut out)
            .expect("valid");
        // TODO-verify-on-hardware: little-endian INTs.
        assert_eq!(out, [0x00, 0x6c, 0x00, 0x00, 0x00, 0x94, 0x01, 0x00]);
        assert!(m.encode_outputs(&vec![Float(1.0); 4], &mut out).is_err());
        assert!(m.encode_outputs(&vec![Bit(true); 4], &mut out).is_err());
    }

    #[test]
    fn encode_scaled_channel() {
        let m = AnalogOutput::with_scaling(
            &UR20_4AO_UI_16,
            &[Some(Scaling::s7(AnalogRange::V0To10)), None, None, None],
        )
        .expect("4 channels");
        assert_eq!(m.output_kind(0), Some(ValueKind::Float));
        let mut out = [0u8; 8];
        m.encode_output_channel(0, &Float(5.0), &mut out).expect("valid");
        assert_eq!(out[..2], [0x00, 0x36]);
        assert_eq!(m.decode_outputs(&out).ok().map(|v| v[0].clone()), Some(Float(5.0)));
        assert!(m.encode_output_channel(0, &Float(20.0), &mut out).is_err());
    }
}
