// Copyright (C), 2026 Quartz Systems. Some rights reserved. This work is
// licensed under the terms of the MIT license which can be found in the
// root directory of this project.

//! DO channel 0 follows DI channel 0 over class 1 implicit I/O.
//!
//! Event-driven: each new input frame is checked and a change goes out with the next O→T
//! packet, so the output follows within one RPI (plus the coupler's own cycle). Pulses
//! shorter than the RPI can be missed.
//!
//! * While the status word reports a removed module, the connection is put in idle mode
//!   (outputs off). A module inserted into a running class 1 connection is otherwise
//!   driven at once (seen on hardware, 2026-10-15).
//! * If the connection ends (timeout, cable pulled) it is reopened, reconnecting TCP first
//!   if needed.
//! * Ctrl-C closes it with Forward_Close, which switches the outputs off.
//!
//! Station: slot 1 = UR20-16DI-P, slot 2 = UR20-16DO-P (or any digital modules in those
//! slots). Channels count from 0; change the constants below to use others. Coupler at
//! 10.42.30.101 unless `UR20_EIP_ADDR` says otherwise; module list from `UR20_EIP_MODULES` or,
//! if unset, read from the coupler.
//!
//! ```sh
//! cargo run -p ur20-fbc-eip --example class1_di0_to_do0
//! ```

use std::time::Duration;

use common::Result;
use ur20_fbc_eip::cip::ImplicitOptions;
use ur20_fbc_eip::{Category, ChannelValue::Bit, Coupler};

mod common;

const DI_SLOT: usize = 1;
const DI_CHANNEL: usize = 0;
const DO_SLOT: usize = 2;
const DO_CHANNEL: usize = 0;
const RPI: Duration = Duration::from_millis(10);

/// Open the connection and follow the input until Ctrl-C (`Ok`) or the connection ends.
async fn follow(c: &mut Coupler) -> Result<()> {
    c.open_implicit(RPI, ImplicitOptions::default()).await?;
    println!("class 1 open, RPI {RPI:?}: DO{DO_CHANNEL} follows DI{DI_CHANNEL}");
    let mut last_di = None;
    let mut running = false;
    loop {
        let img = tokio::select! {
            r = c.next_implicit_inputs() => r?,
            _ = tokio::signal::ctrl_c() => return Ok(()),
        };
        let di = img.bit(DI_SLOT, DI_CHANNEL).unwrap_or(false);
        if last_di != Some(di) {
            c.set_output(DO_SLOT, DO_CHANNEL, Bit(di)).await?;
            println!("DI{DI_CHANNEL} = {} -> DO{DO_CHANNEL}", u8::from(di));
            last_di = Some(di);
        }
        let healthy = img.status().is_some_and(|s| !s.module_removed());
        if healthy != running {
            c.set_implicit_run(healthy)?;
            running = healthy;
            println!(
                "{}",
                if healthy {
                    "run: outputs applied"
                } else {
                    "module removed: idle, outputs off"
                }
            );
        }
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let mut c = common::coupler().await?;
    common::require(&c, DI_SLOT, Category::DigitalInput, DI_CHANNEL + 1)?;
    common::require(&c, DO_SLOT, Category::DigitalOutput, DO_CHANNEL + 1)?;
    loop {
        match follow(&mut c).await {
            Ok(()) => break,
            Err(e) => {
                println!("connection ended: {e}; reopening in 1 s");
                c.drop_implicit();
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_secs(1)) => {}
                    _ = tokio::signal::ctrl_c() => break,
                }
                if c.is_broken() {
                    if let Err(e) = c.reconnect().await {
                        println!("TCP reconnect failed: {e}");
                    }
                }
            }
        }
    }
    println!("closing");
    c.close().await?;
    Ok(())
}
