// Copyright (C), 2026 Quartz Systems. Some rights reserved. This work is
// licensed under the terms of the MIT license which can be found in the
// root directory of this project.

//! Running light: switch on each DO channel in turn, 0 to 15, and repeat.
//!
//! Station: slot 2 = UR20-16DO-P (or another 16-channel digital output module). Coupler
//! address from `UR20_EIP_ADDR`; module list from
//! `UR20_EIP_MODULES` or, if unset, read from the coupler.
//!
//! ```sh
//! UR20_EIP_ADDR=192.168.1.10 cargo run -p ur20-fbc-eip --example do_chase
//! ```

use std::time::Duration;

use common::Result;
use ur20_fbc_eip::{Category, ChannelValue::Bit};

mod common;

/// How long each channel stays on.
const STEP: Duration = Duration::from_millis(100);

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let mut coupler = common::coupler().await?;
    common::require(&coupler, 2, Category::DigitalOutput, 16)?;
    coupler
        .enable_explicit_outputs(Duration::from_secs(2))
        .await?;

    let mut previous = 15;
    loop {
        for channel in 0..16 {
            coupler.set_output(2, previous, Bit(false)).await?;
            coupler.set_output(2, channel, Bit(true)).await?;
            previous = channel;
            tokio::time::sleep(STEP).await;
        }
    }
}
