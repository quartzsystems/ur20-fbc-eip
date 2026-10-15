// Copyright (C), 2026 Quartz Systems. Some rights reserved. This work is
// licensed under the terms of the MIT license which can be found in the
// root directory of this project.

//! DO channel 0 follows DI channel 0, over explicit messaging (polling every 50 ms).
//!
//! Station: slot 1 = UR20-16DI-P, slot 2 = UR20-16DO-P (or any digital modules in those
//! slots). Channels count from 0; change the constants below to use others. Coupler at
//! 10.42.30.101 unless `UR20_EIP_ADDR` says otherwise; module list from `UR20_EIP_MODULES` or,
//! if unset, read from the coupler.
//!
//! ```sh
//! cargo run -p ur20-fbc-eip --example di0_to_do0
//! ```

use std::time::Duration;

use common::Result;
use ur20_fbc_eip::{Category, ChannelValue::Bit};

mod common;

const DI_SLOT: usize = 1;
const DI_CHANNEL: usize = 0;
const DO_SLOT: usize = 2;
const DO_CHANNEL: usize = 0;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let mut coupler = common::coupler().await?;
    common::require(&coupler, DI_SLOT, Category::DigitalInput, DI_CHANNEL + 1)?;
    common::require(&coupler, DO_SLOT, Category::DigitalOutput, DO_CHANNEL + 1)?;
    coupler
        .enable_explicit_outputs(Duration::from_secs(2))
        .await?;

    let mut out = None;
    loop {
        // Writes are rare here; make sure the 2 s write access has not lapsed (no I/O
        // unless it is about to).
        coupler.keep_explicit_outputs_alive().await?;
        let di = coupler
            .read_inputs()
            .await?
            .bit(DI_SLOT, DI_CHANNEL)
            .unwrap_or(false);
        if out != Some(di) {
            coupler.set_output(DO_SLOT, DO_CHANNEL, Bit(di)).await?;
            out = Some(di);
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}
