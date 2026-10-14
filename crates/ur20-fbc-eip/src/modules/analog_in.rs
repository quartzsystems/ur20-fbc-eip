// Copyright (C), 2026 Quartz Systems. Some rights reserved. This work is
// licensed under the terms of the MIT license which can be found in the
// root directory of this project.

//! Analog input modules (16-bit, one INT per channel) and the shared analog scaling.
//!
//! Each channel is a signed 16-bit value. CIP data is little-endian, so the raw INT is assumed
//! little-endian in the assembly (the Modbus coupler uses big-endian registers instead).
//! TODO-verify-on-hardware: byte order of analog channels in assembly 111.
//!
//! By default channels decode to [`ChannelValue::Int16`] (raw counts), because the
//! measurement range is a module *parameter* the coupler owns. Give the codec the configured
//! ranges with [`AnalogInput::with_scaling`] to get [`ChannelValue::Float`] in V or mA.

use crate::error::{Error, Result};
use crate::module::{
    Category, ChannelValue, Module, ModuleDescriptor, ValueKind, check_count, check_len,
};

/// Measurement or output range of a current/voltage channel.
#[allow(non_camel_case_types)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnalogRange {
    /// 0 … 20 mA.
    mA0To20,
    /// 4 … 20 mA.
    mA4To20,
    /// 0 … 10 V.
    V0To10,
    /// −10 … 10 V.
    VMinus10To10,
    /// 0 … 5 V.
    V0To5,
    /// −5 … 5 V.
    VMinus5To5,
    /// 1 … 5 V.
    V1To5,
    /// 2 … 10 V.
    V2To10,
}

impl AnalogRange {
    /// Engineering value at raw 0 and at raw full scale.
    fn span(self) -> (f32, f32) {
        match self {
            Self::mA0To20 => (0.0, 20.0),
            Self::mA4To20 => (4.0, 20.0),
            Self::V0To10 | Self::VMinus10To10 => (0.0, 10.0),
            Self::V0To5 | Self::VMinus5To5 => (0.0, 5.0),
            Self::V1To5 => (1.0, 5.0),
            Self::V2To10 => (2.0, 10.0),
        }
    }
}

/// Raw value that represents full scale.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DataFormat {
    /// Siemens S7 format: full scale = 27648 (u-remote default).
    #[default]
    S7,
    /// Siemens S5 format: full scale = 16384.
    S5,
}

impl DataFormat {
    fn full_scale(self) -> f32 {
        match self {
            Self::S7 => 27_648.0,
            Self::S5 => 16_384.0,
        }
    }
}

/// How to convert a channel between raw counts and engineering units.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Scaling {
    /// Configured range.
    pub range: AnalogRange,
    /// Configured data format.
    pub format: DataFormat,
}

impl Scaling {
    /// S7 format with `range`.
    pub const fn s7(range: AnalogRange) -> Self {
        Self {
            range,
            format: DataFormat::S7,
        }
    }

    /// Raw counts → engineering units.
    pub fn to_engineering(self, raw: i16) -> f32 {
        let (zero, full) = self.range.span();
        zero + f32::from(raw) * (full - zero) / self.format.full_scale()
    }

    /// Engineering units → raw counts, `None` if not representable in an i16.
    pub fn to_raw(self, value: f32) -> Option<i16> {
        let (zero, full) = self.range.span();
        let raw = ((value - zero) * self.format.full_scale() / (full - zero)).round();
        if raw.is_finite() && raw >= f32::from(i16::MIN) && raw <= f32::from(i16::MAX) {
            Some(raw as i16)
        } else {
            None
        }
    }
}

/// Decode `n` little-endian i16 values.
pub(crate) fn read_i16s(data: &[u8], n: usize) -> Vec<i16> {
    (0..n)
        .map(|i| {
            let lo = data.get(2 * i).copied().unwrap_or(0);
            let hi = data.get(2 * i + 1).copied().unwrap_or(0);
            i16::from_le_bytes([lo, hi])
        })
        .collect()
}

/// Convert one value for a 16-bit analog channel, honoring optional scaling.
pub(crate) fn value_to_raw(
    module: &dyn Module,
    channel: usize,
    value: &ChannelValue,
    scaling: Option<Scaling>,
) -> Result<i16> {
    match (value, scaling) {
        (ChannelValue::Int16(v), _) => Ok(*v),
        (ChannelValue::Float(f), Some(s)) => s.to_raw(*f).ok_or_else(|| Error::ValueRange {
            module: module.name(),
            channel,
            detail: format!("{f} is outside the representable range for {:?}", s.range),
        }),
        (other, scaling) => Err(crate::module::wrong_kind(
            module,
            channel,
            if scaling.is_some() {
                ValueKind::Float
            } else {
                ValueKind::Int16
            },
            other,
        )),
    }
}

pub(crate) fn raw_to_value(raw: i16, scaling: Option<Scaling>) -> ChannelValue {
    match scaling {
        Some(s) => ChannelValue::Float(s.to_engineering(raw)),
        None => ChannelValue::Int16(raw),
    }
}

/// A 16-bit analog input module.
#[derive(Debug)]
pub struct AnalogInput {
    descriptor: &'static ModuleDescriptor,
    scaling: Vec<Option<Scaling>>,
}

impl AnalogInput {
    /// Codec returning raw counts.
    pub fn new(descriptor: &'static ModuleDescriptor) -> Self {
        Self {
            descriptor,
            scaling: vec![None; descriptor.channels],
        }
    }

    /// Return engineering values for the channels given here (`None` keeps raw counts).
    ///
    /// The ranges must match the module parameters configured on the coupler.
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
    Box::new(AnalogInput::new(d))
}

/// `UR20-4AI-UI-16`: 4 analog inputs, ±10 V / 0–20 mA, 16 bit.
pub static UR20_4AI_UI_16: ModuleDescriptor = ModuleDescriptor {
    name: "UR20-4AI-UI-16",
    id: 0x0401_15C4,
    category: Category::AnalogInput,
    channels: 4,
    create,
};

/// `UR20-4AI-UI-16-DIAG`: as 4AI-UI-16 with diagnostics. The process data is the same; the
/// diagnostics travel in the separate diagnostic area of assemblies 103/113.
pub static UR20_4AI_UI_16_DIAG: ModuleDescriptor = ModuleDescriptor {
    name: "UR20-4AI-UI-16-DIAG",
    id: 0x0402_1544,
    category: Category::AnalogInput,
    channels: 4,
    create,
};

pub(crate) static DESCRIPTORS: &[&ModuleDescriptor] = &[&UR20_4AI_UI_16, &UR20_4AI_UI_16_DIAG];

impl Module for AnalogInput {
    fn descriptor(&self) -> &'static ModuleDescriptor {
        self.descriptor
    }
    fn input_bits(&self) -> usize {
        16 * self.descriptor.channels
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
        Ok(read_i16s(data, self.descriptor.channels)
            .into_iter()
            .zip(&self.scaling)
            .map(|(raw, s)| raw_to_value(raw, *s))
            .collect())
    }
    fn decode_outputs(&self, data: &[u8]) -> Result<Vec<ChannelValue>> {
        check_len(self, "outputs", data, 0)?;
        Ok(Vec::new())
    }
    fn encode_outputs(&self, values: &[ChannelValue], out: &mut [u8]) -> Result<()> {
        check_count(self, values)?;
        check_len(self, "outputs", out, 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ChannelValue::{Float, Int16};

    #[test]
    fn raw_decode() {
        let m = UR20_4AI_UI_16.instantiate();
        assert_eq!(m.input_bytes(), 8);
        // TODO-verify-on-hardware: little-endian INTs.
        let data = [0x00, 0x6c, 0x00, 0x36, 0x00, 0x94, 0xff, 0x7f];
        assert_eq!(
            m.decode_inputs(&data).ok(),
            Some(vec![Int16(27648), Int16(13824), Int16(-27648), Int16(i16::MAX)])
        );
        assert!(m.decode_inputs(&data[..7]).is_err());
    }

    #[test]
    fn scaled_decode() {
        use AnalogRange::*;
        let m = AnalogInput::with_scaling(
            &UR20_4AI_UI_16,
            &[
                Some(Scaling::s7(V0To10)),
                Some(Scaling::s7(mA4To20)),
                Some(Scaling::s7(VMinus10To10)),
                Some(Scaling {
                    range: mA0To20,
                    format: DataFormat::S5,
                }),
            ],
        )
        .expect("4 channels");
        let data = [0x00, 0x6c, 0x00, 0x36, 0x00, 0x94, 0x00, 0x20];
        assert_eq!(
            m.decode_inputs(&data).ok(),
            Some(vec![Float(10.0), Float(12.0), Float(-10.0), Float(10.0)])
        );
        assert!(AnalogInput::with_scaling(&UR20_4AI_UI_16, &[None]).is_err());
    }

    #[test]
    fn scaling_roundtrip() {
        let s = Scaling::s7(AnalogRange::mA4To20);
        assert_eq!(s.to_raw(4.0), Some(0));
        assert_eq!(s.to_raw(20.0), Some(27648));
        assert_eq!(s.to_engineering(13824), 12.0);
        assert_eq!(s.to_raw(f32::NAN), None);
        assert_eq!(s.to_raw(1000.0), None);
    }
}
