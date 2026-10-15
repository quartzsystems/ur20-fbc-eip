// Copyright (C), 2026 Quartz Systems. Some rights reserved. This work is
// licensed under the terms of the MIT license which can be found in the
// root directory of this project.

//! Class 1 bench check: drive DO channel 8 over implicit I/O in run and idle mode, then end
//! the connection without and with Forward_Close, printing what the coupler reports at each
//! step. Watch an LED on DO channel 8 while it runs (about a minute).
//!
//! Station: slot 2 = a digital output module with at least 9 channels (UR20-16DO-P).
//! Coupler address from `UR20_EIP_ADDR`; module list from `UR20_EIP_MODULES` or, if unset,
//! read from the coupler.
//!
//! ```sh
//! UR20_EIP_ADDR=192.168.1.10 cargo run -p ur20-fbc-eip --example class1_check
//! ```

use std::io::Write;
use std::time::Duration;

use common::Result;
use tokio::time::Instant;
use ur20_fbc_eip::cip::{AttributePath, CipService, ImplicitOptions, hex};
use ur20_fbc_eip::{Category, ChannelValue::Bit, Coupler, InputImage};

mod common;

const RPI: Duration = Duration::from_millis(20);
const PHASE: Duration = Duration::from_secs(10);

fn banner(text: &str) {
    println!("\x07\n==== {text} ====");
    std::io::stdout().flush().ok();
}

async fn get(c: &mut Coupler, class: u16, instance: u32, attribute: u16) -> String {
    match c
        .service_mut()
        .get_attribute_single(AttributePath::new(class, instance, attribute))
        .await
    {
        Ok(v) => hex(&v),
        Err(e) => format!("error: {e}"),
    }
}

/// Explicit reads: what the module drives (114), status word, write access; plus the class 1
/// connection's state and counters.
async fn probe(c: &mut Coupler, start: Instant) {
    let driven = get(c, 0x65, 2, 114).await;
    let status = get(c, 0x64, 1, 104).await;
    let access = get(c, 0x66, 1, 108).await;
    let conn = c
        .implicit()
        .map(|io| format!("{:?} {:?}", io.current_state(), io.counters()))
        .unwrap_or_else(|| "no class 1 connection".into());
    println!(
        "{:>6.2}s  114={driven}  status={status}  108={access}  {conn}",
        start.elapsed().as_secs_f64()
    );
}

/// Run for `d`: print DI changes from class 1 frames, probe once a second.
async fn hold(c: &mut Coupler, d: Duration, start: Instant, last: &mut Option<InputImage>) {
    let end = Instant::now() + d;
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    loop {
        tokio::select! {
            _ = tokio::time::sleep_until(end) => return,
            _ = tick.tick() => probe(c, start).await,
            r = c.next_implicit_inputs(), if c.implicit().is_some() => match r {
                Ok(img) => {
                    if last.as_ref().map(|l| &l.slots) != Some(&img.slots) {
                        let on: Vec<usize> = (0..16).filter(|ch| img.bit(1, *ch) == Some(true)).collect();
                        println!("{:>6.2}s  inputs: status {:?}, DI on {on:?}", start.elapsed().as_secs_f64(), img.status());
                    }
                    *last = Some(img);
                }
                Err(e) => {
                    println!("{:>6.2}s  class 1: {e}", start.elapsed().as_secs_f64());
                    tokio::time::sleep_until(end).await;
                    return;
                }
            },
        }
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let mut c = common::coupler().await?;
    common::require(&c, 2, Category::DigitalOutput, 9)?;
    let start = Instant::now();
    let mut last = None;

    banner(
        "A (10 s): class 1 open, DO ch8 ON in O→T, IDLE mode -> expect LED OFF; check web server",
    );
    c.open_implicit(RPI, ImplicitOptions::default()).await?;
    c.set_output(2, 8, Bit(true)).await?;
    hold(&mut c, PHASE, start, &mut last).await;

    banner("B (10 s): RUN mode -> expect LED ON (24 V on DI8 now would show as an input change)");
    c.set_implicit_run(true)?;
    hold(&mut c, Duration::from_secs(3), start, &mut last).await;
    // Bypass the local refusals to see what the coupler itself does.
    let r = c
        .service_mut()
        .set_attribute_single(AttributePath::new(0x66, 1, 108), &[1])
        .await;
    println!("        explicit 0x66/1/108 := 1 while class 1 owns the outputs: {r:?}");
    let r = c
        .service_mut()
        .set_attribute_single(AttributePath::new(0x04, 112, 3), &[0, 0, 0, 0])
        .await;
    let driven = get(&mut c, 0x65, 2, 114).await;
    println!("        explicit 0x04/112/3 := zeros: {r:?}; 114 right after = {driven}");
    hold(&mut c, PHASE - Duration::from_secs(3), start, &mut last).await;

    banner("C (10 s): back to IDLE -> expect LED OFF");
    c.set_implicit_run(false)?;
    hold(&mut c, PHASE, start, &mut last).await;

    banner(
        "D (10 s): RUN 3 s (LED ON), then stop sending WITHOUT Forward_Close -> LED OFF ~0.3 s later",
    );
    c.set_implicit_run(true)?;
    hold(&mut c, Duration::from_secs(3), start, &mut last).await;
    c.drop_implicit();
    println!("{:>6.2}s  O→T stopped", start.elapsed().as_secs_f64());
    for _ in 0..4 {
        probe(&mut c, start).await;
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
    hold(&mut c, PHASE - Duration::from_secs(4), start, &mut last).await;

    banner("E (10 s): new connection, RUN, ch8 ON (LED ON) 5 s, then Forward_Close -> LED OFF?");
    c.open_implicit(
        RPI,
        ImplicitOptions {
            run: true,
            ..ImplicitOptions::default()
        },
    )
    .await?;
    c.set_output(2, 8, Bit(true)).await?;
    hold(&mut c, Duration::from_secs(5), start, &mut last).await;
    c.close_implicit().await?;
    println!(
        "{:>6.2}s  Forward_Close sent",
        start.elapsed().as_secs_f64()
    );
    for _ in 0..4 {
        probe(&mut c, start).await;
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
    hold(&mut c, Duration::from_secs(4), start, &mut last).await;

    banner("Done");
    c.close().await?;
    Ok(())
}
