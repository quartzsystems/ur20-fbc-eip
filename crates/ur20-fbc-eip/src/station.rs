// Copyright (C), 2026 Quartz Systems. Some rights reserved. This work is
// licensed under the terms of the MIT license which can be found in the
// root directory of this project.

//! A station is the ordered list of modules behind the coupler, plus where each module's
//! bytes sit in the input and output process images.

use std::ops::Range;

use crate::error::{Error, Result};
use crate::layout::ImageFormat;
use crate::module::{ChannelValue, Module};

/// One module and its byte ranges (relative to the start of module data, after the header).
#[derive(Debug)]
pub struct Slot {
    /// Slot number, 1 = first module right of the coupler.
    pub position: usize,
    /// Codec.
    pub module: Box<dyn Module>,
    /// Byte range in the input module data.
    pub inputs: Range<usize>,
    /// Byte range in the output module data.
    pub outputs: Range<usize>,
}

/// What the coupler reports about one slot (Slot class 0x65).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlotInfo {
    /// Slot number (1-based).
    pub position: usize,
    /// Module ID (attribute 104), if the coupler provides it.
    pub module_id: Option<u32>,
    /// Module name (attribute 101), if the coupler provides it.
    pub name: Option<String>,
    /// Input process data size in bits (attribute 109).
    pub input_bits: usize,
    /// Output process data size in bits (attribute 110).
    pub output_bits: usize,
}

impl SlotInfo {
    /// Input bytes in an unpacked image.
    pub fn input_bytes(&self) -> usize {
        self.input_bits.div_ceil(8)
    }

    /// Output bytes in an unpacked image.
    pub fn output_bytes(&self) -> usize {
        self.output_bits.div_ceil(8)
    }
}

/// Decoded channel values of one slot.
#[derive(Debug, Clone, PartialEq)]
pub struct SlotValues {
    /// Slot number (1-based).
    pub position: usize,
    /// Module name.
    pub module: &'static str,
    /// One value per channel.
    pub values: Vec<ChannelValue>,
}

/// Modules in slot order with computed offsets.
#[derive(Debug)]
pub struct Station {
    slots: Vec<Slot>,
    input_len: usize,
    output_len: usize,
}

impl Station {
    /// Lay out `modules` (slot 1 first).
    pub fn new(modules: Vec<Box<dyn Module>>, format: ImageFormat) -> Result<Self> {
        if format == ImageFormat::Packed {
            return Err(Error::Unsupported(
                "packed assemblies (101/102); use CouplerLayout::unpacked() (111/112)".into(),
            ));
        }
        let mut input_len = 0;
        let mut output_len = 0;
        let slots = modules
            .into_iter()
            .enumerate()
            .map(|(i, module)| {
                let inputs = input_len..input_len + module.input_bytes();
                let outputs = output_len..output_len + module.output_bytes();
                input_len = inputs.end;
                output_len = outputs.end;
                Slot {
                    position: i + 1,
                    module,
                    inputs,
                    outputs,
                }
            })
            .collect();
        Ok(Self {
            slots,
            input_len,
            output_len,
        })
    }

    /// All slots.
    pub fn slots(&self) -> &[Slot] {
        &self.slots
    }

    /// The slot at `position` (1-based).
    pub fn slot(&self, position: usize) -> Result<&Slot> {
        position
            .checked_sub(1)
            .and_then(|i| self.slots.get(i))
            .ok_or(Error::NoSuchSlot {
                position,
                count: self.slots.len(),
            })
    }

    /// Total input module-data bytes.
    pub fn input_len(&self) -> usize {
        self.input_len
    }

    /// Total output module-data bytes.
    pub fn output_len(&self) -> usize {
        self.output_len
    }

    fn check_image(&self, what: &str, data: &[u8], expected: usize) -> Result<()> {
        if data.len() < expected {
            return Err(Error::len(
                format!("{what} module data"),
                expected,
                data.len(),
            ));
        }
        if data.len() > expected {
            tracing::debug!(
                what,
                expected,
                actual = data.len(),
                "process image longer than configured modules; ignoring the rest"
            );
        }
        Ok(())
    }

    /// Decode inputs of every module that has inputs.
    pub fn decode_inputs(&self, data: &[u8]) -> Result<Vec<SlotValues>> {
        self.check_image("input", data, self.input_len)?;
        self.slots
            .iter()
            .filter(|s| s.module.input_channels() > 0)
            .map(|s| {
                let bytes = data
                    .get(s.inputs.clone())
                    .ok_or_else(|| Error::len("input slot", s.inputs.end, data.len()))?;
                Ok(SlotValues {
                    position: s.position,
                    module: s.module.name(),
                    values: s.module.decode_inputs(bytes)?,
                })
            })
            .collect()
    }

    /// Decode outputs of every module that has outputs.
    pub fn decode_outputs(&self, data: &[u8]) -> Result<Vec<SlotValues>> {
        self.check_image("output", data, self.output_len)?;
        self.slots
            .iter()
            .filter(|s| s.module.output_channels() > 0)
            .map(|s| {
                let bytes = data
                    .get(s.outputs.clone())
                    .ok_or_else(|| Error::len("output slot", s.outputs.end, data.len()))?;
                Ok(SlotValues {
                    position: s.position,
                    module: s.module.name(),
                    values: s.module.decode_outputs(bytes)?,
                })
            })
            .collect()
    }

    /// Change one output channel inside the output module data.
    pub fn set_output(
        &self,
        data: &mut [u8],
        position: usize,
        channel: usize,
        value: &ChannelValue,
    ) -> Result<()> {
        self.check_image("output", data, self.output_len)?;
        let slot = self.slot(position)?;
        let len = data.len();
        let bytes = data
            .get_mut(slot.outputs.clone())
            .ok_or_else(|| Error::len("output slot", slot.outputs.end, len))?;
        slot.module.encode_output_channel(channel, value, bytes)
    }

    /// Compare configured modules with what the coupler reports: count, module ID (when
    /// reported) and process data size in bits.
    pub fn verify(&self, reported: &[SlotInfo]) -> Result<()> {
        if reported.len() != self.slots.len() {
            return Err(Error::StationMismatch {
                position: 0,
                detail: format!(
                    "{} module(s) configured, coupler reports {}",
                    self.slots.len(),
                    reported.len()
                ),
            });
        }
        for (slot, r) in self.slots.iter().zip(reported) {
            let expected = slot.module.descriptor();
            if let Some(id) = r.module_id.filter(|id| *id != expected.id) {
                let actual = r
                    .name
                    .clone()
                    .unwrap_or_else(|| format!("module {id:#010x}"));
                return Err(Error::StationMismatch {
                    position: slot.position,
                    detail: format!("configured {}, coupler has {actual}", expected.name),
                });
            }
            let configured = (slot.module.input_bits(), slot.module.output_bits());
            let actual = (r.input_bits, r.output_bits);
            if configured != actual {
                return Err(Error::StationMismatch {
                    position: slot.position,
                    detail: format!(
                        "{} expects {}/{} in/out bits, coupler reports {}/{}",
                        expected.name, configured.0, configured.1, actual.0, actual.1
                    ),
                });
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modules::create_all;
    use ChannelValue::{Bit, Int16};

    fn station() -> Station {
        let modules = create_all(&[
            "UR20-4DI-P",     // in 0..1
            "UR20-4DO-P",     // out 0..1
            "UR20-4AI-UI-16", // in 1..9
            "UR20-16DI-P",    // in 9..11
            "UR20-4AO-UI-16", // out 1..9
            "UR20-8DO-P",     // out 9..10
        ])
        .expect("known modules");
        Station::new(modules, ImageFormat::Unpacked).expect("unpacked")
    }

    // TODO-verify-on-hardware: unpacked module data for the station above, invented.
    // (Header-free; the 2-byte header is stripped by the coupler.)
    #[rustfmt::skip]
    const INPUT_FIXTURE: [u8; 11] = [
        0b0000_0011,                                    // 4DI: ch0, ch1
        0x00, 0x6c, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, // 4AI: 27648, 0, 0, 1
        0x00, 0x80,                                     // 16DI: ch15
    ];

    #[test]
    fn offsets() {
        let s = station();
        assert_eq!(s.input_len(), 11);
        assert_eq!(s.output_len(), 10);
        let ranges: Vec<_> = s
            .slots()
            .iter()
            .map(|x| (x.position, x.inputs.clone(), x.outputs.clone()))
            .collect();
        assert_eq!(
            ranges,
            [
                (1, 0..1, 0..0),
                (2, 1..1, 0..1),
                (3, 1..9, 1..1),
                (4, 9..11, 1..1),
                (5, 11..11, 1..9),
                (6, 11..11, 9..10),
            ]
        );
    }

    #[test]
    fn decode_inputs_fixture() {
        let s = station();
        let v = s.decode_inputs(&INPUT_FIXTURE).expect("valid");
        assert_eq!(v.len(), 3);
        assert_eq!(v[0].position, 1);
        assert_eq!(v[0].values, [Bit(true), Bit(true), Bit(false), Bit(false)]);
        assert_eq!(v[1].module, "UR20-4AI-UI-16");
        assert_eq!(v[1].values, [Int16(27648), Int16(0), Int16(0), Int16(1)]);
        assert_eq!(v[2].values[15], Bit(true));
        assert!(s.decode_inputs(&INPUT_FIXTURE[..10]).is_err());
        let mut longer = INPUT_FIXTURE.to_vec();
        longer.extend_from_slice(&[0; 8]); // e.g. diagnostics trailer
        assert!(s.decode_inputs(&longer).is_ok());
    }

    #[test]
    fn set_output_touches_only_its_bytes() {
        let s = station();
        let mut out = [0xFFu8; 10];
        s.set_output(&mut out, 6, 1, &Bit(false)).expect("valid");
        assert_eq!(out[9], 0b1111_1101);
        s.set_output(&mut out, 5, 3, &Int16(-1)).expect("valid");
        assert_eq!(out[..9], [0xFF; 9]);
        s.set_output(&mut out, 2, 0, &Bit(false)).expect("valid");
        assert_eq!(out[0], 0b0000_1110);
        let v = s.decode_outputs(&out).expect("valid");
        assert_eq!(v.iter().map(|x| x.position).collect::<Vec<_>>(), [2, 5, 6]);

        assert!(matches!(
            s.set_output(&mut out, 7, 0, &Bit(true)),
            Err(Error::NoSuchSlot { .. })
        ));
        assert!(s.set_output(&mut out, 0, 0, &Bit(true)).is_err());
        assert!(s.set_output(&mut out, 1, 0, &Bit(true)).is_err()); // DI has no outputs
        assert!(s.set_output(&mut out[..5], 6, 0, &Bit(true)).is_err());
    }

    fn info(v: &[(Option<u32>, usize, usize)]) -> Vec<SlotInfo> {
        v.iter()
            .enumerate()
            .map(|(i, (id, a, b))| SlotInfo {
                position: i + 1,
                module_id: *id,
                name: None,
                input_bits: *a,
                output_bits: *b,
            })
            .collect()
    }

    #[test]
    fn verify_against_reported_slots() {
        let s = station();
        let ok = info(&[
            (Some(0x0009_1F84), 4, 0),
            (Some(0x0101_2FA0), 0, 4),
            (None, 64, 0), // module ID not reported: sizes still checked
            (Some(0x0004_9FC2), 16, 0),
            (Some(0x0502_25E0), 0, 64),
            (Some(0x0102_2FC8), 0, 8),
        ]);
        assert!(s.verify(&ok).is_ok());
        assert!(s.verify(&ok[..5]).is_err());

        let mut wrong_size = ok.clone();
        wrong_size[3].input_bits = 8;
        let e = s.verify(&wrong_size).expect_err("slot 4 differs");
        assert!(e.to_string().contains("slot 4"), "{e}");

        let mut wrong_module = ok;
        wrong_module[0].module_id = Some(0x001B_1F84); // 4DI-P-3W: same size, other module
        wrong_module[0].name = Some("UR20-4DI-P-3W".into());
        let e = s.verify(&wrong_module).expect_err("slot 1 differs");
        assert!(e.to_string().contains("coupler has UR20-4DI-P-3W"), "{e}");
    }

    #[test]
    fn packed_is_unsupported() {
        assert!(Station::new(Vec::new(), ImageFormat::Packed).is_err());
    }
}
