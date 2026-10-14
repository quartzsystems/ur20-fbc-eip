// Copyright (C), 2026 Quartz Systems. Some rights reserved. This work is
// licensed under the terms of the MIT license which can be found in the
// root directory of this project.

//! DO channel 8 blinks fast while DI channel 8 is on, and is off otherwise.
//!
//! Station: UR20-FBC-EIP at 10.42.30.101, slot 1 = UR20-16DI-P, slot 2 = UR20-16DO-P.
//!
//! ```sh
//! cargo run -p ur20-fbc-eip --example di8_blink_do8
//! ```

use std::time::Duration;

use ur20_fbc_eip::{ChannelValue::Bit, Coupler, CouplerLayout, Result, modules};

/// Time between toggles: 20 ms on, 20 ms off = 25 Hz.
const TOGGLE: Duration = Duration::from_millis(20);

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let modules = modules::create_all(&["UR20-16DI-P", "UR20-16DO-P"])?;
    let mut coupler =
        Coupler::connect("10.42.30.101:44818", CouplerLayout::default(), modules).await?;
    coupler.verify_station().await?;
    coupler
        .enable_explicit_outputs(Duration::from_secs(2))
        .await?;

    let mut do8 = false;
    loop {
        let di8 = coupler.read_inputs().await?.bit(1, 8).unwrap_or(false);
        let next = di8 && !do8; // flip while DI8 is on, otherwise off
        if next != do8 {
            coupler.set_output(2, 8, Bit(next)).await?;
            do8 = next;
        }
        tokio::time::sleep(TOGGLE).await;
    }
}
