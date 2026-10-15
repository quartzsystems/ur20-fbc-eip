// Copyright (C), 2026 Quartz Systems. Some rights reserved. This work is
// licensed under the terms of the MIT license which can be found in the
// root directory of this project.

//! Error type for the coupler and module layer.

use crate::module::ValueKind;

/// Result alias using [`Error`].
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Errors from module codecs, station layout and the coupler.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// Transport error.
    #[error(transparent)]
    Cip(#[from] ur20_fbc_eip_cip::Error),

    /// The module name is not a u-remote module we know of.
    #[error("unknown module `{0}`")]
    UnknownModule(String),

    /// The module ID is not one we know of.
    #[error("unknown module ID {0:#010x}")]
    UnknownModuleId(u32),

    /// A real u-remote module that this crate does not implement yet.
    #[error("module {name} is not supported yet (see CONTRIBUTING.md to add it)")]
    UnsupportedModule {
        /// Module name.
        name: String,
    },

    /// A byte slice had the wrong length.
    #[error("{context}: expected {expected} byte(s), got {actual}")]
    BufferLength {
        /// What was being decoded or encoded.
        context: String,
        /// Expected length.
        expected: usize,
        /// Actual length.
        actual: usize,
    },

    /// Channel index out of range.
    #[error("{module} has {count} {direction} channel(s), channel {channel} does not exist")]
    InvalidChannel {
        /// Module name.
        module: &'static str,
        /// "input" or "output".
        direction: &'static str,
        /// Requested channel.
        channel: usize,
        /// Number of channels.
        count: usize,
    },

    /// Wrong value type for a channel.
    #[error("{module} channel {channel} expects {expected}, got {got}")]
    ValueType {
        /// Module name.
        module: &'static str,
        /// Channel.
        channel: usize,
        /// Expected kind.
        expected: ValueKind,
        /// Supplied kind.
        got: ValueKind,
    },

    /// Value of the right type but outside the representable range.
    #[error("{module} channel {channel}: {detail}")]
    ValueRange {
        /// Module name.
        module: &'static str,
        /// Channel.
        channel: usize,
        /// Explanation.
        detail: String,
    },

    /// The wrong number of channel values was supplied.
    #[error("{module} expects {expected} output value(s), got {actual}")]
    ValueCount {
        /// Module name.
        module: &'static str,
        /// Expected count.
        expected: usize,
        /// Supplied count.
        actual: usize,
    },

    /// A text value could not be parsed for a channel.
    #[error("cannot parse `{input}` as {kind}")]
    ParseValue {
        /// Input text.
        input: String,
        /// Target kind.
        kind: ValueKind,
    },

    /// No module at this slot position.
    #[error("no module at slot {position} (station has {count} module(s), first is slot 1)")]
    NoSuchSlot {
        /// Requested slot (1-based).
        position: usize,
        /// Number of modules.
        count: usize,
    },

    /// Configured modules do not match what the coupler reports.
    #[error("station mismatch at slot {position}: {detail}")]
    StationMismatch {
        /// Slot (1-based), 0 for whole-station problems.
        position: usize,
        /// Explanation.
        detail: String,
    },

    /// The operation needs explicit messaging to the outputs, but a class 1 connection owns
    /// them (see [`crate::Coupler::open_implicit`]).
    #[error("a class 1 connection owns the outputs; {0}")]
    ImplicitOpen(&'static str),

    /// No class 1 connection is open, or it has ended (timed out, failed, closed).
    #[error("class 1 connection not running: {0}")]
    ImplicitNotRunning(String),

    /// Feature not available with the current layout.
    #[error("not supported: {0}")]
    Unsupported(String),
}

impl Error {
    /// The CIP status if the coupler rejected a request.
    pub fn cip_status(&self) -> Option<&ur20_fbc_eip_cip::CipStatus> {
        match self {
            Self::Cip(e) => e.cip_status(),
            _ => None,
        }
    }

    /// The coupler said this slot does not exist: CIP status 0x05 (path destination
    /// unknown, seen on hardware past the last slot) or 0x16 (object does not exist).
    pub fn is_no_slot(&self) -> bool {
        matches!(self.cip_status().map(|s| s.general), Some(0x05 | 0x16))
    }

    pub(crate) fn len(context: impl Into<String>, expected: usize, actual: usize) -> Self {
        Self::BufferLength {
            context: context.into(),
            expected,
            actual,
        }
    }
}
