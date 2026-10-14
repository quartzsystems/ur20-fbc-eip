// Copyright (C), 2026 Quartz Systems. Some rights reserved. This work is
// licensed under the terms of the MIT license which can be found in the
// root directory of this project.

//! DO channel 8 follows DI channel 8.
//!
//! Station: slot 1 = UR20-16DI-P, slot 2 = UR20-16DO-P (or any digital modules with at
//! least 9 channels in those slots). Coupler address from `UR20_EIP_ADDR`; module list from
//! `UR20_EIP_MODULES` or, if unset, read from the coupler.
//!
//! ```sh
//! UR20_EIP_ADDR=192.168.1.10 cargo run -p ur20-fbc-eip --example di8_to_do8
//! ```

use std::time::Duration;

use common::Result;
use ur20_fbc_eip::{Category, ChannelValue::Bit};

mod common;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let mut coupler = common::coupler().await?;
    common::require(&coupler, 1, Category::DigitalInput, 9)?;
    common::require(&coupler, 2, Category::DigitalOutput, 9)?;
    coupler
        .enable_explicit_outputs(Duration::from_secs(2))
        .await?;

    let mut do8 = None;
    loop {
        let di8 = coupler.read_inputs().await?.bit(1, 8).unwrap_or(false);
        if do8 != Some(di8) {
            coupler.set_output(2, 8, Bit(di8)).await?;
            do8 = Some(di8);
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}
