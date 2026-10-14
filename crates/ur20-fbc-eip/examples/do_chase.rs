// Copyright (C), 2026 Quartz Systems. Some rights reserved. This work is
// licensed under the terms of the MIT license which can be found in the
// root directory of this project.

//! Running light: switch on each DO channel in turn, 0 to 15, and repeat.
//!
//! Station: UR20-FBC-EIP at 10.42.30.101, slot 1 = UR20-16DI-P, slot 2 = UR20-16DO-P.
//!
//! ```sh
//! cargo run -p ur20-fbc-eip --example do_chase
//! ```

use std::time::Duration;

use ur20_fbc_eip::{ChannelValue::Bit, Coupler, CouplerLayout, Result, modules};

/// How long each channel stays on.
const STEP: Duration = Duration::from_millis(100);

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let modules = modules::create_all(&["UR20-16DI-P", "UR20-16DO-P"])?;
    let mut coupler =
        Coupler::connect("10.42.30.101:44818", CouplerLayout::default(), modules).await?;
    coupler.verify_station().await?;
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
