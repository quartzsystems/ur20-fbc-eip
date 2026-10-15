// Copyright (C), 2026 Quartz Systems. Some rights reserved. This work is
// licensed under the terms of the MIT license which can be found in the
// root directory of this project.

//! Coupler and module diagnosis, as the UR20-FBC-EIP reports it over explicit messaging.
//!
//! | Object | Path | Bytes |
//! |---|---|---|
//! | Gateway, status word (same as the input assembly header) | `0x64/1/104` | 2 |
//! | Gateway, latest diagnosis event | `0x64/1/108` | 0 or 4 |
//! | Gateway, unacknowledged diagnosis | `0x64/1/118` | 8 |
//! | Slot, module diagnosis | `0x65/n/116` | 47, or 0 with no module |
//! | Slot, process alarm | `0x65/n/117` | 4, or 0 with no module |
//!
//! Neither Weidmüller document we work from (QSG0028, AN0050) describes the bits. A bit has
//! a name here only if a bench experiment showed what sets it. Station: UR20-FBC-EIP
//! (firmware 01.21.00), slot 1 UR20-16DI-P, slot 2 UR20-16DO-P, on 2026-10-13 and 2026-10-14.
//!
//! * **Uout field supply disconnected** (web server: "Output power supply error"): status
//!   word `0x2000`; 16DO diagnosis byte 0 `0x00 → 0x15`; `0x64/1/108` `02 82 15 0f`. Slot 1
//!   (16DI, supplied from Uin) unchanged. All cleared on their own when Uout returned.
//! * **16DO pulled from slot 2** (web server: "Module removed", "Configuration Mismatch"):
//!   status word `0x0040`; slot 2 116/117 empty; module list entry 2 → 0; `0x64/1/108`
//!   `02 80 00 0f`. All cleared on their own when the module went back in.
//! * In both experiments `0x64/1/118` stayed all zeros and the process alarms (117) stayed
//!   zero.
//!
//! Everything else is TODO-verify-on-hardware and is kept raw.

use std::fmt;

use crate::error::{Error, Result};

/// Length of the Gateway unacknowledged diagnosis (0x64/1/118).
pub const GATEWAY_DIAGNOSIS_LEN: usize = 8;
/// Length of a slot's module diagnosis (0x65/n/116).
pub const MODULE_DIAGNOSIS_LEN: usize = 47;
/// Length of a slot's process alarm (0x65/n/117).
pub const PROCESS_ALARM_LEN: usize = 4;

/// One set bit, by byte and bit position (bit 0 = least significant).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RawBit {
    /// Byte offset in the attribute.
    pub byte: usize,
    /// Bit in that byte, 0–7.
    pub bit: u8,
}

impl fmt::Display for RawBit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "byte {} bit {}", self.byte, self.bit)
    }
}

/// Every set bit in `data`, in order.
pub fn set_bits(data: &[u8]) -> Vec<RawBit> {
    data.iter()
        .enumerate()
        .flat_map(|(byte, b)| {
            (0..8u8)
                .filter(move |bit| b & (1 << bit) != 0)
                .map(move |bit| RawBit { byte, bit })
        })
        .collect()
}

fn fixed<const N: usize>(what: &str, data: &[u8]) -> Result<[u8; N]> {
    <[u8; N]>::try_from(data).map_err(|_| Error::len(what, N, data.len()))
}

/// The coupler status word: the 2-byte header of the input assembly, also readable as
/// Gateway `0x64/1/104` (same value in all states observed). Little-endian on the wire.
///
/// `0x0000` on a healthy station. Bits without a constant here are TODO-verify-on-hardware.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct StatusWord(pub u16);

impl StatusWord {
    /// Bit 6: set while a module was pulled (2026-10-13 and 2026-10-14). Whether a *wrong*
    /// module also sets it is TODO-verify-on-hardware.
    pub const MODULE_REMOVED: u16 = 1 << 6;
    /// Bit 13: set while the Uout field supply was disconnected; the web server showed
    /// "Output power supply error" (2026-10-14).
    pub const OUTPUT_SUPPLY_ERROR: u16 = 1 << 13;
    // Bit 3 was set for about 0.8 s while a pulled module was being re-inserted
    // (2026-10-15, once). TODO-verify-on-hardware before naming it.

    const NAMED: [(u16, &'static str); 2] = [
        (Self::MODULE_REMOVED, "module removed"),
        (Self::OUTPUT_SUPPLY_ERROR, "output supply error"),
    ];

    /// Decode the 2 little-endian bytes of the header or of `0x64/1/104`.
    pub fn decode(data: &[u8]) -> Result<Self> {
        Ok(Self(u16::from_le_bytes(fixed("status word", data)?)))
    }

    /// No bit set, as on a healthy station.
    pub fn is_ok(self) -> bool {
        self.0 == 0
    }

    /// See [`StatusWord::MODULE_REMOVED`].
    pub fn module_removed(self) -> bool {
        self.0 & Self::MODULE_REMOVED != 0
    }

    /// See [`StatusWord::OUTPUT_SUPPLY_ERROR`].
    pub fn output_supply_error(self) -> bool {
        self.0 & Self::OUTPUT_SUPPLY_ERROR != 0
    }

    /// Set bits that have no name yet.
    pub fn unknown_bits(self) -> u16 {
        Self::NAMED.iter().fold(self.0, |w, (bit, _)| w & !bit)
    }
}

impl From<u16> for StatusWord {
    fn from(w: u16) -> Self {
        Self(w)
    }
}

impl fmt::Display for StatusWord {
    /// `0x2040 (module removed, output supply error)`, `0x0000 (ok)`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:#06x}", self.0)?;
        let mut parts: Vec<String> = Self::NAMED
            .iter()
            .filter(|(bit, _)| self.0 & bit != 0)
            .map(|(_, name)| (*name).to_owned())
            .collect();
        if self.unknown_bits() != 0 {
            parts.push(format!("unknown bits {:#06x}", self.unknown_bits()));
        }
        if parts.is_empty() {
            parts.push("ok".into());
        }
        write!(f, " ({})", parts.join(", "))
    }
}

/// Gateway unacknowledged diagnosis (0x64/1/118), 8 bytes.
///
/// All zeros on a healthy station, and it **stayed** all zeros with Uout missing and with a
/// module pulled, so no bit is known yet (TODO-verify-on-hardware). On 2026-10-13 the read
/// briefly failed with CIP status 0x1F while a module was being pulled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GatewayDiagnosis {
    /// The bytes as read.
    pub raw: [u8; GATEWAY_DIAGNOSIS_LEN],
}

impl GatewayDiagnosis {
    /// Decode the attribute. Fails unless it is exactly 8 bytes.
    pub fn decode(data: &[u8]) -> Result<Self> {
        Ok(Self {
            raw: fixed("gateway diagnosis (0x64/1/118)", data)?,
        })
    }

    /// No bit set.
    pub fn is_clear(&self) -> bool {
        self.raw.iter().all(|b| *b == 0)
    }

    /// Every set bit.
    pub fn set_bits(&self) -> Vec<RawBit> {
        set_bits(&self.raw)
    }
}

/// Coupler-level diagnosis, from [`crate::Coupler::diagnosis`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CouplerDiagnosis {
    /// Status word (`0x64/1/104`).
    pub status: StatusWord,
    /// Unacknowledged diagnosis (`0x64/1/118`).
    pub unacknowledged: GatewayDiagnosis,
    /// Latest diagnosis event (`0x64/1/108`), raw; empty while no diagnosis is active.
    ///
    /// Observed: `02 82 15 0f` with Uout missing (slot 2 = the 16DO, whose diagnosis bytes
    /// 0–1 were `15 0f`) and `02 80 00 0f` with the module in slot 2 pulled. That fits
    /// "slot, event type, first two bytes of the module diagnosis", but with one slot and two
    /// events that is a guess. TODO-verify-on-hardware.
    pub latest_event: Vec<u8>,
}

impl CouplerDiagnosis {
    /// Status word 0 and no event pending: what a healthy station showed. (The
    /// unacknowledged diagnosis is not considered, since no fault seen so far set it.)
    pub fn is_ok(&self) -> bool {
        self.status.is_ok() && self.latest_event.is_empty()
    }
}

/// Module diagnosis of a present module (0x65/n/116), 47 bytes.
///
/// Observed on a UR20-16DI-P and a UR20-16DO-P:
///
/// | Byte | Healthy | Notes |
/// |---|---|---|
/// | 0 | `00` | `15` on the 16DO with Uout missing; see [`ModuleDiagnosis::has_error`] |
/// | 1 | `0f` | constant in every state seen |
/// | 4 | `70` (DI), `72` (DO) | constant per module type |
/// | 43–46 | `00 00 07 09` after power-up | new value at each diagnosis event, coming and going |
/// | others | `00` | |
///
/// The bytes are consistent with the S7 diagnostic data record (DS0/DS1) layout. That is a
/// hypothesis, not a confirmed fact, so only the observations above get names
/// (TODO-verify-on-hardware).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModuleDiagnosis {
    /// The bytes as read.
    pub raw: [u8; MODULE_DIAGNOSIS_LEN],
}

impl ModuleDiagnosis {
    /// Bits 0, 2 and 4 of byte 0, which all went up together when Uout was disconnected
    /// from a UR20-16DO-P (2026-10-14). Their individual meanings are TODO-verify-on-hardware.
    pub const OUTPUT_SUPPLY_MISSING: u8 = 0x15;

    /// Decode the attribute. Fails unless it is exactly 47 bytes.
    pub fn decode(data: &[u8]) -> Result<Self> {
        Ok(Self {
            raw: fixed("module diagnosis (0x65/n/116)", data)?,
        })
    }

    /// Byte 0: `0x00` on healthy modules, non-zero during a module fault.
    pub fn error_byte(&self) -> u8 {
        self.raw.first().copied().unwrap_or(0)
    }

    /// Byte 0 is non-zero. Confirmed for one fault (Uout missing); other faults may report
    /// elsewhere (TODO-verify-on-hardware).
    pub fn has_error(&self) -> bool {
        self.error_byte() != 0
    }

    /// All of [`ModuleDiagnosis::OUTPUT_SUPPLY_MISSING`] are set.
    pub fn output_supply_missing(&self) -> bool {
        self.error_byte() & Self::OUTPUT_SUPPLY_MISSING == Self::OUTPUT_SUPPLY_MISSING
    }

    /// Byte 4: `0x70` on a 16DI-P, `0x72` on a 16DO-P.
    pub fn type_byte(&self) -> u8 {
        self.raw.get(4).copied().unwrap_or(0)
    }
}

/// Diagnosis of one slot: module diagnosis (0x65/n/116) and process alarm (0x65/n/117).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SlotDiagnosis {
    /// Slot number, 1-based.
    pub position: usize,
    /// Module diagnosis; `None` if the coupler returned no bytes, which it does for a slot
    /// whose module has been pulled.
    pub module: Option<ModuleDiagnosis>,
    /// Process alarm, raw (zero in every state seen; TODO-verify-on-hardware). `None` if the
    /// coupler returned no bytes.
    pub process_alarm: Option<[u8; PROCESS_ALARM_LEN]>,
}

impl SlotDiagnosis {
    /// Decode both attributes. Each must be empty or exactly 47 / 4 bytes.
    pub fn decode(position: usize, diagnosis: &[u8], process_alarm: &[u8]) -> Result<Self> {
        Ok(Self {
            position,
            module: (!diagnosis.is_empty())
                .then(|| ModuleDiagnosis::decode(diagnosis))
                .transpose()?,
            process_alarm: (!process_alarm.is_empty())
                .then(|| fixed("process alarm (0x65/n/117)", process_alarm))
                .transpose()?,
        })
    }

    /// The coupler reports a module here (its diagnosis is not empty).
    pub fn is_present(&self) -> bool {
        self.module.is_some()
    }

    /// The process alarm has a bit set.
    pub fn has_process_alarm(&self) -> bool {
        self.process_alarm
            .is_some_and(|a| a.iter().any(|b| *b != 0))
    }

    /// Module present, no error byte, no process alarm.
    pub fn is_ok(&self) -> bool {
        self.module.is_some_and(|m| !m.has_error()) && !self.has_process_alarm()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 0x65/n/116 read on 2026-10-14.
    #[rustfmt::skip]
    fn captured(slot: usize, state: &str) -> Vec<u8> {
        let mut v = vec![0u8; 47];
        v[1] = 0x0f;
        v[4] = if slot == 1 { 0x70 } else { 0x72 };
        let tail: [u8; 4] = match state {
            "healthy" => [0x00, 0x00, 0x07, 0x09],
            "uout" => { v[0] = 0x15; [0x63, 0xae, 0xef, 0x56] }
            _ => [0x6a, 0x70, 0x39, 0xa3], // after Uout returned
        };
        v[43..].copy_from_slice(&tail);
        v
    }

    #[test]
    fn raw_bits() {
        assert_eq!(set_bits(&[0, 0]), []);
        assert_eq!(
            set_bits(&[0b1000_0001, 0, 0b0100_0000]),
            [
                RawBit { byte: 0, bit: 0 },
                RawBit { byte: 0, bit: 7 },
                RawBit { byte: 2, bit: 6 }
            ]
        );
        assert_eq!(RawBit { byte: 3, bit: 1 }.to_string(), "byte 3 bit 1");
    }

    #[test]
    fn status_words_from_hardware() {
        // 0x64/1/104 and the 111 header: healthy, Uout missing, module pulled
        let ok = StatusWord::decode(&[0x00, 0x00]).unwrap();
        let uout = StatusWord::decode(&[0x00, 0x20]).unwrap();
        let pulled = StatusWord::decode(&[0x40, 0x00]).unwrap();
        assert!(ok.is_ok() && !ok.module_removed() && !ok.output_supply_error());
        assert!(uout.output_supply_error() && !uout.module_removed());
        assert!(pulled.module_removed() && !pulled.output_supply_error());
        assert_eq!(ok.to_string(), "0x0000 (ok)");
        assert_eq!(uout.to_string(), "0x2000 (output supply error)");
        assert_eq!(
            StatusWord(0x2041).to_string(),
            "0x2041 (module removed, output supply error, unknown bits 0x0001)"
        );
        assert!(StatusWord::decode(&[0]).is_err());
    }

    #[test]
    fn module_diagnosis_from_hardware() {
        for slot in [1, 2] {
            let d = SlotDiagnosis::decode(slot, &captured(slot, "healthy"), &[0; 4]).unwrap();
            assert!(d.is_ok() && d.is_present(), "slot {slot}");
            assert!(!d.module.unwrap().output_supply_missing());
        }
        assert_eq!(
            ModuleDiagnosis::decode(&captured(1, "healthy"))
                .unwrap()
                .type_byte(),
            0x70
        );
        assert_eq!(
            ModuleDiagnosis::decode(&captured(2, "healthy"))
                .unwrap()
                .type_byte(),
            0x72
        );

        let uout = SlotDiagnosis::decode(2, &captured(2, "uout"), &[0; 4]).unwrap();
        let m = uout.module.unwrap();
        assert!(!uout.is_ok() && m.has_error() && m.output_supply_missing());
        assert_eq!(m.error_byte(), 0x15);

        // bytes 43–46 changed after the fault cleared, nothing else: still healthy
        assert!(
            SlotDiagnosis::decode(2, &captured(2, "after"), &[0; 4])
                .unwrap()
                .is_ok()
        );

        // module pulled: both attributes empty
        let pulled = SlotDiagnosis::decode(2, &[], &[]).unwrap();
        assert!(!pulled.is_present() && !pulled.is_ok() && !pulled.has_process_alarm());
    }

    #[test]
    fn exact_lengths() {
        assert!(GatewayDiagnosis::decode(&[0; 8]).is_ok_and(|d| d.is_clear()));
        assert!(GatewayDiagnosis::decode(&[0; 7]).is_err());
        assert!(GatewayDiagnosis::decode(&[0; 9]).is_err());
        let d = SlotDiagnosis::decode(2, &[0; 47], &[0, 0, 0, 1]).unwrap();
        assert!(d.has_process_alarm() && !d.is_ok());
        assert!(SlotDiagnosis::decode(2, &[0; 46], &[0; 4]).is_err());
        assert!(SlotDiagnosis::decode(2, &[0; 47], &[0; 3]).is_err());
    }
}
