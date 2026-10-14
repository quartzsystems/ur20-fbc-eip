// Copyright (C), 2026 Quartz Systems. Some rights reserved. This work is
// licensed under the terms of the MIT license which can be found in the
// root directory of this project.

//! Module implementations and the registry that finds them by name or ID.
//!
//! To add a module, see `CONTRIBUTING.md`. In short: add a file here that defines one or more
//! `pub static …: ModuleDescriptor` and a `pub(crate) static DESCRIPTORS`, then add the file's
//! name to `register_modules!` below.

use crate::error::{Error, Result};
use crate::module::{Module, ModuleDescriptor};

macro_rules! register_modules {
    ($($file:ident),* $(,)?) => {
        $(pub mod $file;)*

        /// Every implemented module.
        pub fn all() -> impl Iterator<Item = &'static ModuleDescriptor> {
            [$($file::DESCRIPTORS),*].into_iter().flatten().copied()
        }
    };
}

register_modules! {
    digital_in,
    digital_out,
    analog_in,
    analog_out,
    counter,
}

/// Normalize `ur20_4di_p`, `UR20-4DI-P`, `4di-p` to `UR20-4DI-P`.
fn normalize(name: &str) -> String {
    let upper = name.trim().to_ascii_uppercase().replace('_', "-");
    if upper.starts_with("UR20-") {
        upper
    } else {
        format!("UR20-{upper}")
    }
}

/// Look up an implemented module by catalog name (case and `-`/`_` insensitive).
///
/// Known-but-unimplemented modules yield [`Error::UnsupportedModule`]; anything else
/// [`Error::UnknownModule`].
pub fn by_name(name: &str) -> Result<&'static ModuleDescriptor> {
    let n = normalize(name);
    if let Some(d) = all().find(|d| d.name == n) {
        return Ok(d);
    }
    if KNOWN_MODULES.iter().any(|(_, known)| *known == n) {
        return Err(Error::UnsupportedModule { name: n });
    }
    Err(Error::UnknownModule(name.to_owned()))
}

/// Look up an implemented module by module ID.
pub fn by_id(id: u32) -> Result<&'static ModuleDescriptor> {
    if let Some(d) = all().find(|d| d.id == id) {
        return Ok(d);
    }
    match KNOWN_MODULES.iter().find(|(known, _)| *known == id) {
        Some((_, name)) => Err(Error::UnsupportedModule {
            name: (*name).to_owned(),
        }),
        None => Err(Error::UnknownModuleId(id)),
    }
}

/// Instantiate codecs for a list of module names, in slot order.
pub fn create_all<S: AsRef<str>>(names: &[S]) -> Result<Vec<Box<dyn Module>>> {
    names
        .iter()
        .map(|n| by_name(n.as_ref()).map(|d| d.instantiate()))
        .collect()
}

/// Module IDs of u-remote modules, implemented or not, for useful error messages.
///
/// The IDs are the same on every u-remote coupler; this table matches the one published by
/// slowtec/ur20 for the Modbus TCP coupler.
pub const KNOWN_MODULES: &[(u32, &str)] = &[
    (0x0009_1F84, "UR20-4DI-P"),
    (0x001B_1F84, "UR20-4DI-P-3W"),
    (0x0013_1FC1, "UR20-8DI-P-2W"),
    (0x000A_1FC1, "UR20-8DI-P-3W"),
    (0x0003_1FC1, "UR20-8DI-P-3W-HD"),
    (0x0004_9FC2, "UR20-16DI-P"),
    (0x0005_9FC2, "UR20-16DI-P-PLC-INT"),
    (0x0F01_4700, "UR20-2DI-P-TS"),
    (0x0F02_4700, "UR20-4DI-P-TS"),
    (0x0001_1F84, "UR20-4DI-N"),
    (0x0002_1FC1, "UR20-8DI-N-3W"),
    (0x000C_9FC2, "UR20-16DI-N"),
    (0x000D_9FC2, "UR20-16DI-N-PLC-INT"),
    (0x0016_9F84, "UR20-4DI-2W-230V-AC"),
    (0x0101_2FA0, "UR20-4DO-P"),
    (0x0105_2FA0, "UR20-4DO-P-2A"),
    (0x0115_2FC8, "UR20-4DO-PN-2A"),
    (0x0102_2FC8, "UR20-8DO-P"),
    (0x0119_2FC8, "UR20-8DO-P-2W-HD"),
    (0x0103_AFD0, "UR20-16DO-P"),
    (0x0104_AFD0, "UR20-16DO-P-PLC-INT"),
    (0x010A_2FA0, "UR20-4DO-N"),
    (0x010B_2FA0, "UR20-4DO-N-2A"),
    (0x010C_2FC8, "UR20-8DO-N"),
    (0x010D_AFD0, "UR20-16DO-N"),
    (0x010E_AFD0, "UR20-16DO-N-PLC-INT"),
    (0x0107_2FA0, "UR20-4RO-SSR-255"),
    (0x0106_2FA0, "UR20-4RO-CO-255"),
    (0x0908_4880, "UR20-2PWM-PN-0.5A"),
    (0x0909_4880, "UR20-2PWM-PN-2A"),
    (0x0401_15C4, "UR20-4AI-UI-16"),
    (0x0402_1544, "UR20-4AI-UI-16-DIAG"),
    (0x041E_1544, "UR20-4AI-UI-DIF-16-DIAG"),
    (0x0413_15C4, "UR20-4AI-UI-16-HD"),
    (0x0414_1544, "UR20-4AI-UI-16-DIAG-HD"),
    (0x0411_15C4, "UR20-4AI-UI-12"),
    (0x0404_15C5, "UR20-8AI-I-16-HD"),
    (0x0405_1545, "UR20-8AI-I-16-DIAG-HD"),
    (0x0409_15C5, "UR20-8AI-I-PLC-INT"),
    (0x041C_1544, "UR20-4AI-R-HS-16-DIAG"),
    (0x041B_356D, "UR20-2AI-SG-24-DIAG"),
    (0x0418_356D, "UR20-3EM-230V-AC"),
    (0x0406_1544, "UR20-4AI-RTD-DIAG"),
    (0x0407_1544, "UR20-4AI-TC-DIAG"),
    (0x0502_25E0, "UR20-4AO-UI-16"),
    (0x0506_25E0, "UR20-4AO-UI-16-M"),
    (0x0501_2560, "UR20-4AO-UI-16-DIAG"),
    (0x0505_2560, "UR20-4AO-UI-16-M-DIAG"),
    (0x0504_25E0, "UR20-4AO-UI-16-HD"),
    (0x0503_2560, "UR20-4AO-UI-16-DIAG-HD"),
    (0x08C1_3800, "UR20-1CNT-100-1DO"),
    (0x08C3_3800, "UR20-2CNT-100"),
    (0x08C4_3801, "UR20-1CNT-500"),
    (0x0881_28EE, "UR20-2FCNT-100"),
    (0x09C1_7880, "UR20-1SSI"),
    (0x0E41_3FED, "UR20-1COM-232-485-422"),
    (0x0BC1_E800, "UR20-1COM-SAI-PRO"),
    (0x0E81_276D, "UR20-4COM-IO-LINK"),
    (0x1801_9F43, "UR20-PF-O-1DI-SIL"),
    (0x1803_9F43, "UR20-PF-O-2DI-SIL"),
    (0x1802_9F43, "UR20-PF-O-2DI-DELAY-SIL"),
];

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn lookup_by_name() {
        assert_eq!(
            by_name("UR20-4DI-P").map(|d| d.name).ok(),
            Some("UR20-4DI-P")
        );
        assert_eq!(
            by_name("ur20_4do_p").map(|d| d.name).ok(),
            Some("UR20-4DO-P")
        );
        assert_eq!(
            by_name("4ai-ui-16").map(|d| d.name).ok(),
            Some("UR20-4AI-UI-16")
        );
        assert!(matches!(
            by_name("UR20-4AI-RTD-DIAG"),
            Err(Error::UnsupportedModule { .. })
        ));
        assert!(matches!(by_name("UR20-NOPE"), Err(Error::UnknownModule(_))));
    }

    #[test]
    fn lookup_by_id() {
        assert_eq!(by_id(0x0009_1F84).map(|d| d.name).ok(), Some("UR20-4DI-P"));
        assert!(matches!(
            by_id(0x0406_1544),
            Err(Error::UnsupportedModule { .. })
        ));
        assert!(matches!(by_id(0x1234_5678), Err(Error::UnknownModuleId(_))));
    }

    #[test]
    fn registry_is_consistent() {
        let mut names = HashSet::new();
        let mut ids = HashSet::new();
        for d in all() {
            assert!(names.insert(d.name), "duplicate name {}", d.name);
            assert!(ids.insert(d.id), "duplicate id {:#x}", d.id);
            assert!(
                KNOWN_MODULES.contains(&(d.id, d.name)),
                "{} id {:#x} not in KNOWN_MODULES",
                d.name,
                d.id
            );
            let m = d.instantiate();
            assert_eq!(m.descriptor().name, d.name);
            assert_eq!(m.name(), d.name);
            // every module decodes zeros without error
            assert!(
                m.decode_inputs(&vec![0; m.input_bytes()]).is_ok(),
                "{}",
                d.name
            );
            assert!(
                m.decode_outputs(&vec![0; m.output_bytes()]).is_ok(),
                "{}",
                d.name
            );
            // and rejects wrong lengths without panicking
            assert!(
                m.decode_inputs(&vec![0; m.input_bytes() + 1]).is_err(),
                "{}",
                d.name
            );
            assert!(
                m.decode_outputs(&vec![0; m.output_bytes() + 1]).is_err(),
                "{}",
                d.name
            );
            // round trip of zero outputs
            let zeros = m
                .decode_outputs(&vec![0; m.output_bytes()])
                .unwrap_or_default();
            let mut buf = vec![0xAA; m.output_bytes()];
            assert!(m.encode_outputs(&zeros, &mut buf).is_ok(), "{}", d.name);
            assert_eq!(m.decode_outputs(&buf).ok(), Some(zeros), "{}", d.name);
            assert!(m.output_kind(m.output_channels()).is_none());
        }
        assert!(names.len() >= 10);
    }

    #[test]
    fn create_all_preserves_order_and_fails_cleanly() {
        let ms = create_all(&["UR20-4DI-P", "UR20-4DO-P"]).expect("known modules");
        assert_eq!(
            ms.iter().map(|m| m.name()).collect::<Vec<_>>(),
            ["UR20-4DI-P", "UR20-4DO-P"]
        );
        assert!(create_all(&["UR20-4DI-P", "UR20-1SSI"]).is_err());
    }
}
