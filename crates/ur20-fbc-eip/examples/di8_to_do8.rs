// Copyright (C), 2026 Quartz Systems. Some rights reserved. This work is
// licensed under the terms of the MIT license which can be found in the
// root directory of this project.

//! DO channel 8 follows DI channel 8.
//!
//! Station: UR20-FBC-EIP at 10.42.30.101, slot 1 = UR20-16DI-P, slot 2 = UR20-16DO-P.
//!
//! ```sh
//! cargo run -p ur20-fbc-eip --example di8_to_do8
//! ```

use std::time::Duration;

use ur20_fbc_eip::{ChannelValue::Bit, Coupler, CouplerLayout, Result, modules};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let modules = modules::create_all(&["UR20-16DI-P", "UR20-16DO-P"])?;
    let mut coupler =
        Coupler::connect("10.42.30.101:44818", CouplerLayout::default(), modules).await?;
    coupler.verify_station().await?;
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
