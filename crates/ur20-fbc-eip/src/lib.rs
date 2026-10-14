// Copyright (C), 2026 Quartz Systems. Some rights reserved. This work is
// licensed under the terms of the MIT license which can be found in the
// root directory of this project.

//! Weidmüller u-remote **UR20-FBC-EIP** EtherNet/IP coupler and UR20 I/O modules.
//!
//! The EtherNet/IP sibling of [`ur20`](https://crates.io/crates/ur20), which targets the
//! Modbus TCP coupler. The process data of a u-remote module is the same on every coupler; only
//! the transport differs. Here the transport is CIP explicit messaging to the coupler's
//! Assembly Objects, provided by [`ur20_fbc_eip_cip`].
//!
//! ```no_run
//! use std::time::Duration;
//! use ur20_fbc_eip::{ChannelValue, Coupler, CouplerLayout, modules};
//!
//! # async fn demo() -> ur20_fbc_eip::Result<()> {
//! let modules = modules::create_all(&["UR20-4DI-P", "UR20-4DO-P", "UR20-4AI-UI-16"])?;
//! let mut coupler = Coupler::connect("192.168.1.10:44818", CouplerLayout::default(), modules).await?;
//! coupler.verify_station().await?;
//!
//! for slot in coupler.read_inputs().await?.slots {
//!     println!("slot {} {}: {:?}", slot.position, slot.module, slot.values);
//! }
//!
//! coupler.enable_explicit_outputs(Duration::from_secs(5)).await?;
//! coupler.set_output(2, 0, ChannelValue::Bit(true)).await?;
//! # Ok(()) }
//! ```

#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::panic
    )
)]

pub mod coupler;
pub mod error;
pub mod layout;
pub mod module;
pub mod modules;
pub mod station;

pub use coupler::{Coupler, InputImage, OutputImage};
pub use error::{Error, Result};
pub use layout::{CouplerLayout, ImageFormat};
pub use module::{Category, ChannelValue, Module, ModuleDescriptor, ValueKind};
pub use station::{Slot, SlotInfo, SlotValues, Station};
pub use ur20_fbc_eip_cip as cip;
