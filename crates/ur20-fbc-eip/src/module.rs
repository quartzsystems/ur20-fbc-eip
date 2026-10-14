// Copyright (C), 2026 Quartz Systems. Some rights reserved. This work is
// licensed under the terms of the MIT license which can be found in the
// root directory of this project.

//! The [`Module`] trait and the values that flow through it.
//!
//! A module codec only ever sees **its own** bytes of the process image; [`crate::Station`]
//! cuts the assembly into per-module slices.

use std::fmt;

use crate::error::{Error, Result};

/// One channel's value.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum ChannelValue {
    /// Digital channel.
    Bit(bool),
    /// Signed 16-bit (raw analog value).
    Int16(i16),
    /// Unsigned 16-bit (status/control words).
    UInt16(u16),
    /// Signed 32-bit (counter values).
    Int32(i32),
    /// Unsigned 32-bit.
    UInt32(u32),
    /// Scaled engineering value (V, mA, …).
    Float(f32),
    /// Uninterpreted bytes.
    Raw(Vec<u8>),
}

impl ChannelValue {
    /// The kind of this value.
    pub fn kind(&self) -> ValueKind {
        match self {
            Self::Bit(_) => ValueKind::Bit,
            Self::Int16(_) => ValueKind::Int16,
            Self::UInt16(_) => ValueKind::UInt16,
            Self::Int32(_) => ValueKind::Int32,
            Self::UInt32(_) => ValueKind::UInt32,
            Self::Float(_) => ValueKind::Float,
            Self::Raw(b) => ValueKind::Raw(b.len()),
        }
    }

    /// Parse text into a value of `kind` (for CLIs and config files).
    ///
    /// Bits accept `1/0/true/false/on/off`; integers accept decimal or `0x` hex; raw accepts hex.
    pub fn parse(input: &str, kind: ValueKind) -> Result<Self> {
        let err = || Error::ParseValue {
            input: input.to_owned(),
            kind,
        };
        let s = input.trim();
        let int = |s: &str| -> Option<i64> {
            match s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
                Some(h) => i64::from_str_radix(h, 16).ok(),
                None => s.parse().ok(),
            }
        };
        Ok(match kind {
            ValueKind::Bit => match s.to_ascii_lowercase().as_str() {
                "1" | "true" | "on" | "high" => Self::Bit(true),
                "0" | "false" | "off" | "low" => Self::Bit(false),
                _ => return Err(err()),
            },
            ValueKind::Int16 => {
                Self::Int16(int(s).and_then(|v| v.try_into().ok()).ok_or_else(err)?)
            }
            ValueKind::UInt16 => {
                Self::UInt16(int(s).and_then(|v| v.try_into().ok()).ok_or_else(err)?)
            }
            ValueKind::Int32 => {
                Self::Int32(int(s).and_then(|v| v.try_into().ok()).ok_or_else(err)?)
            }
            ValueKind::UInt32 => {
                Self::UInt32(int(s).and_then(|v| v.try_into().ok()).ok_or_else(err)?)
            }
            ValueKind::Float => Self::Float(s.parse().map_err(|_| err())?),
            ValueKind::Raw(n) => {
                let h: String = s.trim_start_matches("0x").split_whitespace().collect();
                let bytes = (0..h.len())
                    .step_by(2)
                    .map(|i| h.get(i..i + 2).and_then(|b| u8::from_str_radix(b, 16).ok()))
                    .collect::<Option<Vec<u8>>>()
                    .ok_or_else(err)?;
                if bytes.len() != n {
                    return Err(err());
                }
                Self::Raw(bytes)
            }
        })
    }
}

impl fmt::Display for ChannelValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Bit(b) => write!(f, "{}", u8::from(*b)),
            Self::Int16(v) => write!(f, "{v}"),
            Self::UInt16(v) => write!(f, "{v:#06x}"),
            Self::Int32(v) => write!(f, "{v}"),
            Self::UInt32(v) => write!(f, "{v}"),
            Self::Float(v) => write!(f, "{v:.3}"),
            Self::Raw(b) => f.write_str(&ur20_fbc_eip_cip::hex(b)),
        }
    }
}

/// The type of a channel, without a value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ValueKind {
    /// [`ChannelValue::Bit`].
    Bit,
    /// [`ChannelValue::Int16`].
    Int16,
    /// [`ChannelValue::UInt16`].
    UInt16,
    /// [`ChannelValue::Int32`].
    Int32,
    /// [`ChannelValue::UInt32`].
    UInt32,
    /// [`ChannelValue::Float`].
    Float,
    /// [`ChannelValue::Raw`] with this many bytes.
    Raw(usize),
}

impl fmt::Display for ValueKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Raw(n) => write!(f, "Raw({n} bytes)"),
            other => write!(f, "{other:?}"),
        }
    }
}

/// Broad module class.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Category {
    /// Digital input.
    DigitalInput,
    /// Digital output.
    DigitalOutput,
    /// Analog input.
    AnalogInput,
    /// Analog output.
    AnalogOutput,
    /// Counter.
    Counter,
}

/// Static facts about a module type, plus a constructor.
#[derive(Debug)]
pub struct ModuleDescriptor {
    /// Catalog name, e.g. `UR20-4DI-P`.
    pub name: &'static str,
    /// Module ID as reported by u-remote couplers (same ID on every fieldbus).
    pub id: u32,
    /// Category.
    pub category: Category,
    /// Number of channels.
    pub channels: usize,
    /// Build a codec with default parameters.
    pub create: fn(&'static ModuleDescriptor) -> Box<dyn Module>,
}

impl ModuleDescriptor {
    /// Build a codec with default parameters.
    pub fn instantiate(&'static self) -> Box<dyn Module> {
        (self.create)(self)
    }
}

/// Codec for one I/O module's process data.
///
/// Byte slices passed in and out are exactly [`Module::input_bytes`] /
/// [`Module::output_bytes`] long; implementations must return [`Error::BufferLength`] for
/// anything else and must never panic.
pub trait Module: fmt::Debug + Send + Sync {
    /// Static description.
    fn descriptor(&self) -> &'static ModuleDescriptor;

    /// Size of the module's input data in bits.
    fn input_bits(&self) -> usize;

    /// Size of the module's output data in bits.
    fn output_bits(&self) -> usize;

    /// Number of decoded input values.
    fn input_channels(&self) -> usize;

    /// Number of output values.
    fn output_channels(&self) -> usize;

    /// The value kind each output channel accepts (`None` if out of range).
    fn output_kind(&self, channel: usize) -> Option<ValueKind>;

    /// Decode the module's input bytes.
    fn decode_inputs(&self, data: &[u8]) -> Result<Vec<ChannelValue>>;

    /// Decode the module's output bytes (to read back what is being driven).
    fn decode_outputs(&self, data: &[u8]) -> Result<Vec<ChannelValue>>;

    /// Encode all output values into `out`.
    fn encode_outputs(&self, values: &[ChannelValue], out: &mut [u8]) -> Result<()>;

    /// Catalog name.
    fn name(&self) -> &'static str {
        self.descriptor().name
    }

    /// Input bytes occupied in an unpacked (byte-granular) process image.
    fn input_bytes(&self) -> usize {
        self.input_bits().div_ceil(8)
    }

    /// Output bytes occupied in an unpacked (byte-granular) process image.
    fn output_bytes(&self) -> usize {
        self.output_bits().div_ceil(8)
    }

    /// Change one output channel in `out`, leaving the others as they are.
    fn encode_output_channel(
        &self,
        channel: usize,
        value: &ChannelValue,
        out: &mut [u8],
    ) -> Result<()> {
        let mut values = self.decode_outputs(out)?;
        let count = values.len();
        let slot = values.get_mut(channel).ok_or(Error::InvalidChannel {
            module: self.name(),
            direction: "output",
            channel,
            count,
        })?;
        *slot = value.clone();
        self.encode_outputs(&values, out)
    }
}

/// Fail unless `data` has exactly `expected` bytes.
pub(crate) fn check_len(
    module: &dyn Module,
    what: &str,
    data: &[u8],
    expected: usize,
) -> Result<()> {
    if data.len() == expected {
        Ok(())
    } else {
        Err(Error::len(
            format!("{} {what}", module.name()),
            expected,
            data.len(),
        ))
    }
}

/// Fail unless `values` has exactly `module.output_channels()` entries.
pub(crate) fn check_count(module: &dyn Module, values: &[ChannelValue]) -> Result<()> {
    if values.len() == module.output_channels() {
        Ok(())
    } else {
        Err(Error::ValueCount {
            module: module.name(),
            expected: module.output_channels(),
            actual: values.len(),
        })
    }
}

/// Error for a value of the wrong kind.
pub(crate) fn wrong_kind(
    module: &dyn Module,
    channel: usize,
    expected: ValueKind,
    got: &ChannelValue,
) -> Error {
    Error::ValueType {
        module: module.name(),
        channel,
        expected,
        got: got.kind(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_values() {
        assert_eq!(
            ChannelValue::parse("on", ValueKind::Bit).ok(),
            Some(ChannelValue::Bit(true))
        );
        assert_eq!(
            ChannelValue::parse("0", ValueKind::Bit).ok(),
            Some(ChannelValue::Bit(false))
        );
        assert!(ChannelValue::parse("2", ValueKind::Bit).is_err());
        assert_eq!(
            ChannelValue::parse("-27648", ValueKind::Int16).ok(),
            Some(ChannelValue::Int16(-27648))
        );
        assert!(ChannelValue::parse("40000", ValueKind::Int16).is_err());
        assert_eq!(
            ChannelValue::parse("0x8000", ValueKind::UInt16).ok(),
            Some(ChannelValue::UInt16(0x8000))
        );
        assert_eq!(
            ChannelValue::parse("2.5", ValueKind::Float).ok(),
            Some(ChannelValue::Float(2.5))
        );
        assert_eq!(
            ChannelValue::parse("01 ff", ValueKind::Raw(2)).ok(),
            Some(ChannelValue::Raw(vec![1, 0xff]))
        );
        assert!(ChannelValue::parse("01", ValueKind::Raw(2)).is_err());
    }

    #[test]
    fn display() {
        assert_eq!(ChannelValue::Bit(true).to_string(), "1");
        assert_eq!(ChannelValue::UInt16(0x12).to_string(), "0x0012");
        assert_eq!(ChannelValue::Float(1.0).to_string(), "1.000");
        assert_eq!(ValueKind::Raw(3).to_string(), "Raw(3 bytes)");
    }
}
