// Copyright (C), 2026 Quartz Systems. Some rights reserved. This work is
// licensed under the terms of the MIT license which can be found in the
// root directory of this project.

//! Where the coupler keeps its process data, as a value you can override.
//!
//! Defaults come from Weidmüller *QSG0028v3 UR20-FBC-EIP Quick Start Guide*, chapter 3, and
//! *AN0050v3*, §5.3. Anything not stated there is marked TODO-verify-on-hardware. Confirm
//! with `ur20eip raw get 0x04 111 3` and Wireshark, then override fields as needed:
//!
//! ```
//! use ur20_fbc_eip::CouplerLayout;
//! let layout = CouplerLayout::default()
//!     .with_input_instance(111)
//!     .with_output_instance(112)
//!     .with_header_lens(2, 2);
//! ```

use std::time::Duration;

use ur20_fbc_eip_cip::AttributePath;

/// Assembly Object class.
pub const ASSEMBLY_CLASS: u16 = 0x04;
/// Assembly data attribute.
pub const ASSEMBLY_DATA_ATTRIBUTE: u16 = 3;

/// How module data is arranged inside the assemblies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageFormat {
    /// Every module starts on a byte boundary; 4-bit modules get 4 padding bits
    /// (assemblies 111/112, 113/114).
    Unpacked,
    /// DI/DO bits packed back to back across modules (assemblies 101/102, 103/104).
    /// Not supported yet: the exact packing rule needs confirming on hardware.
    Packed,
}

/// Process Data class attributes that grant explicit messaging write access to outputs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExplicitWriteControl {
    /// WORD: seconds without explicit traffic before write access lapses (0 = never).
    pub timeout: AttributePath,
    /// BOOL: "Start Explicit Process Data Exchange".
    pub start: AttributePath,
}

impl Default for ExplicitWriteControl {
    /// Class 0x66, instance 1, attributes 107 and 108 (QSG0028 table 1).
    fn default() -> Self {
        Self {
            timeout: AttributePath::new(0x66, 1, 107),
            start: AttributePath::new(0x66, 1, 108),
        }
    }
}

/// Per-slot objects (Slot class).
///
/// Attribute numbers 101–117 were read from a UR20-FBC-EIP on 2026-10-13 (firmware 01.21.00);
/// 113/114 also appear in QSG0028 table 3.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SlotClass {
    /// Class ID.
    pub class: u16,
    /// STRING: module name, e.g. `UR20-16DI-P`.
    pub name_attribute: u16,
    /// UDINT: module ID.
    pub id_attribute: u16,
    /// UINT: input process data size in bits.
    pub input_bits_attribute: u16,
    /// UINT: output process data size in bits.
    pub output_bits_attribute: u16,
    /// Attribute holding the slot's input data.
    pub input_attribute: u16,
    /// Attribute holding the slot's output data.
    pub output_attribute: u16,
    /// Module diagnosis, 47 bytes (see [`crate::diagnosis`]).
    pub diagnosis_attribute: u16,
    /// Process alarm, 4 bytes (see [`crate::diagnosis`]).
    pub process_alarm_attribute: u16,
    /// Highest slot number.
    pub max_slots: u32,
}

impl Default for SlotClass {
    /// Class 0x65, instances 1–64.
    fn default() -> Self {
        Self {
            class: 0x65,
            name_attribute: 101,
            id_attribute: 104,
            input_bits_attribute: 109,
            output_bits_attribute: 110,
            input_attribute: 113,
            output_attribute: 114,
            diagnosis_attribute: 116,
            process_alarm_attribute: 117,
            max_slots: 64,
        }
    }
}

/// Where and how the coupler exposes process data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CouplerLayout {
    /// Input assembly data attribute. Default `0x04/111/3` (unpacked).
    pub input: AttributePath,
    /// Output assembly data attribute. Default `0x04/112/3` (unpacked).
    pub output: AttributePath,
    /// Configuration assembly instance for class 1 Forward_Open. Default 1: the Weidmüller
    /// EDS (v1.6/v1.7) uses `20 04 24 01 2C 70 2C 6F` for "Exclusive Owner Unpacked".
    /// Instance 123 ("Module Parameter", 400 bytes) is only used by the
    /// "Exclusive Owner with Config Assembly" connections.
    pub config_instance: Option<u32>,
    /// Bytes before module data in the input assembly. Default 2: the coupler status word
    /// (AN0050 §5.3), length confirmed on hardware.
    pub input_header_len: usize,
    /// Bytes before module data in the output assembly. Default 2: confirmed on hardware
    /// (assembly 112 = 2 header bytes + module data). TODO-verify-on-hardware: its meaning; we
    /// preserve it on read-modify-write.
    pub output_header_len: usize,
    /// Byte arrangement of module data.
    pub format: ImageFormat,
    /// Attributes to enable explicit output writes; `None` to skip that step.
    pub explicit_write: Option<ExplicitWriteControl>,
    /// Per-slot objects used by [`crate::Coupler::discover_slots`]; `None` to disable.
    pub slot_class: Option<SlotClass>,
    /// Array of 64 little-endian UDINT module IDs, slot 1 first, unused slots 0. Used by
    /// [`crate::Coupler::detect`]. Default `0x64/1/111` (Gateway class), read from hardware
    /// on 2026-10-13.
    pub module_list: Option<AttributePath>,
    /// Coupler diagnosis attributes used by [`crate::Coupler::diagnosis`]; `None` to
    /// disable. See [`crate::diagnosis`].
    pub diagnosis: Option<DiagnosisPaths>,
}

/// Gateway class attributes read by [`crate::Coupler::diagnosis`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiagnosisPaths {
    /// UINT status word, same value as the input assembly header.
    pub status_word: AttributePath,
    /// Latest diagnosis event, 4 bytes, empty when none is active.
    pub latest_event: AttributePath,
    /// Unacknowledged diagnosis, 8 bytes.
    pub unacknowledged: AttributePath,
}

impl Default for DiagnosisPaths {
    /// `0x64/1/104`, `108` and `118`, read from hardware on 2026-10-13/14.
    fn default() -> Self {
        Self {
            status_word: AttributePath::new(0x64, 1, 104),
            latest_event: AttributePath::new(0x64, 1, 108),
            unacknowledged: AttributePath::new(0x64, 1, 118),
        }
    }
}

impl Default for CouplerLayout {
    fn default() -> Self {
        Self::unpacked()
    }
}

impl CouplerLayout {
    /// Assemblies 111/112: 2-byte header + byte-aligned module data. Recommended.
    pub fn unpacked() -> Self {
        Self {
            input: AttributePath::new(ASSEMBLY_CLASS, 111, ASSEMBLY_DATA_ATTRIBUTE),
            output: AttributePath::new(ASSEMBLY_CLASS, 112, ASSEMBLY_DATA_ATTRIBUTE),
            config_instance: Some(1),
            input_header_len: 2,
            output_header_len: 2,
            format: ImageFormat::Unpacked,
            explicit_write: Some(ExplicitWriteControl::default()),
            slot_class: Some(SlotClass::default()),
            module_list: Some(AttributePath::new(0x64, 1, 111)),
            diagnosis: Some(DiagnosisPaths::default()),
        }
    }

    /// Assemblies 113/114: like [`Self::unpacked`] plus 8 diagnostic bytes, which are
    /// assumed to follow the module data (TODO-verify-on-hardware) and are returned as
    /// [`crate::InputImage::trailer`].
    pub fn unpacked_with_diagnostics() -> Self {
        Self::unpacked()
            .with_input_instance(113)
            .with_output_instance(114)
    }

    /// Assemblies 101/102 (packed). Decoding is not supported yet.
    pub fn packed() -> Self {
        Self {
            format: ImageFormat::Packed,
            ..Self::unpacked()
                .with_input_instance(101)
                .with_output_instance(102)
        }
    }

    /// Use a different input assembly instance (attribute 3).
    pub fn with_input_instance(mut self, instance: u32) -> Self {
        self.input = AttributePath::new(ASSEMBLY_CLASS, instance, ASSEMBLY_DATA_ATTRIBUTE);
        self
    }

    /// Use a different output assembly instance (attribute 3).
    pub fn with_output_instance(mut self, instance: u32) -> Self {
        self.output = AttributePath::new(ASSEMBLY_CLASS, instance, ASSEMBLY_DATA_ATTRIBUTE);
        self
    }

    /// Override header lengths.
    pub fn with_header_lens(mut self, input: usize, output: usize) -> Self {
        self.input_header_len = input;
        self.output_header_len = output;
        self
    }

    /// Set the configuration assembly instance.
    pub fn with_config_instance(mut self, instance: u32) -> Self {
        self.config_instance = Some(instance);
        self
    }
}

/// Default explicit-write timeout written by [`crate::Coupler::enable_explicit_outputs`].
pub const DEFAULT_WRITE_TIMEOUT: Duration = Duration::from_secs(5);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_quick_start_guide() {
        let l = CouplerLayout::default();
        assert_eq!(l.input, AttributePath::new(0x04, 0x6F, 3));
        assert_eq!(l.output, AttributePath::new(0x04, 0x70, 3));
        assert_eq!(l.input_header_len, 2);
        assert_eq!(l.format, ImageFormat::Unpacked);
        let w = l.explicit_write.expect("enabled by default");
        assert_eq!(w.start, AttributePath::new(0x66, 1, 0x6C));
        assert_eq!(w.timeout, AttributePath::new(0x66, 1, 0x6B));
        assert_eq!(l.config_instance, Some(1));
    }

    #[test]
    fn overrides() {
        let l = CouplerLayout::packed()
            .with_header_lens(0, 0)
            .with_config_instance(123);
        assert_eq!(l.input.instance, 101);
        assert_eq!(l.output.instance, 102);
        assert_eq!(l.format, ImageFormat::Packed);
        assert_eq!((l.input_header_len, l.output_header_len), (0, 0));
        assert_eq!(l.config_instance, Some(123));
        assert_eq!(
            CouplerLayout::unpacked_with_diagnostics().input.instance,
            113
        );
    }
}
