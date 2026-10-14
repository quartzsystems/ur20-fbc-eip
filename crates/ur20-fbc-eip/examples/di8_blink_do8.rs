// Copyright (C), 2026 Quartz Systems. Some rights reserved. This work is
// licensed under the terms of the MIT license which can be found in the
// root directory of this project.

//! DO channel 8 blinks fast while DI channel 8 is on, and is off otherwise.
//!
//! Station: slot 1 = UR20-16DI-P, slot 2 = UR20-16DO-P (or any digital modules with at
//! least 9 channels in those slots). Coupler address from `UR20_EIP_ADDR`; module list from
//! `UR20_EIP_MODULES` or, if unset, read from the coupler.
//!
//! ```sh
//! UR20_EIP_ADDR=192.168.1.10 cargo run -p ur20-fbc-eip --example di8_blink_do8
//! ```

use std::time::Duration;

use common::Result;
use ur20_fbc_eip::{Category, ChannelValue::Bit};

mod common;

/// Time between toggles: 20 ms on, 20 ms off = 25 Hz.
const TOGGLE: Duration = Duration::from_millis(20);

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let mut coupler = common::coupler().await?;
    common::require(&coupler, 1, Category::DigitalInput, 9)?;
    common::require(&coupler, 2, Category::DigitalOutput, 9)?;
    coupler
        .enable_explicit_outputs(Duration::from_secs(2))
        .await?;

    let mut do8 = false;
    loop {
        // Writes are rare here; make sure the 2 s write access has not lapsed (no I/O
        // unless it is about to).
        coupler.keep_explicit_outputs_alive().await?;
        let di8 = coupler.read_inputs().await?.bit(1, 8).unwrap_or(false);
        let next = di8 && !do8; // flip while DI8 is on, otherwise off
        if next != do8 {
            coupler.set_output(2, 8, Bit(next)).await?;
            do8 = next;
        }
        tokio::time::sleep(TOGGLE).await;
    }
}
