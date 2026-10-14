// Copyright (C), 2026 Quartz Systems. Some rights reserved. This work is
// licensed under the terms of the MIT license which can be found in the
// root directory of this project.

//! `ur20eip diag`: status word, coupler diagnosis and per-slot diagnosis.
//!
//! Keeps going past individual failures: diagnosis is what you read when things are broken.

use anyhow::Result;
use ur20_fbc_eip::cip::{Client, hex};
use ur20_fbc_eip::{Coupler, RawBit, SlotDiagnosis};

fn bits(bits: &[RawBit]) -> String {
    let list: Vec<String> = bits.iter().map(ToString::to_string).collect();
    list.join(", ")
}

fn rows(indent: &str, data: &[u8]) {
    for (i, chunk) in data.chunks(16).enumerate() {
        println!("{indent}{:02x}: {}", i * 16, hex(chunk));
    }
}

fn print_slot(d: &SlotDiagnosis, verbose: bool) {
    let p = d.position;
    let Some(m) = d.module else {
        println!("slot {p:>2}: no module (pulled?)");
        return;
    };
    let state = if m.output_supply_missing() {
        format!("error byte {:#04x}: output supply missing", m.error_byte())
    } else if m.has_error() {
        format!("error byte {:#04x}", m.error_byte())
    } else {
        "ok".into()
    };
    println!("slot {p:>2}: {state}");
    if let Some(alarm) = d.process_alarm.filter(|_| d.has_process_alarm()) {
        println!("    process alarm 117: {}", hex(&alarm));
    }
    if verbose || m.has_error() {
        println!("    diagnosis 116:");
        rows("      ", &m.raw);
    }
}

/// `ur20eip diag [--slot N]… [--raw]`
pub async fn run(c: &mut Coupler<Client>, slots: &[usize], verbose: bool) -> Result<()> {
    match c.diagnosis().await {
        Ok(d) => {
            println!("status word: {}", d.status);
            if d.latest_event.is_empty() {
                println!("latest event 0x64/1/108: none");
            } else {
                println!("latest event 0x64/1/108: {}", hex(&d.latest_event));
            }
            let u = &d.unacknowledged;
            if u.is_clear() {
                println!("unacknowledged 0x64/1/118: clear");
            } else {
                println!(
                    "unacknowledged 0x64/1/118: {} (unnamed bits: {})",
                    hex(&u.raw),
                    bits(&u.set_bits())
                );
            }
        }
        Err(e) => println!("coupler diagnosis: error: {e}"),
    }

    let all = slots.is_empty();
    let wanted: Vec<usize> = if all {
        (1..=64).collect()
    } else {
        slots.to_vec()
    };
    for position in wanted {
        match c.slot_diagnosis(position).await {
            Ok(d) => print_slot(&d, verbose),
            Err(e) if all && e.is_no_slot() => break,
            Err(e) => println!("slot {position:>2}: error: {e}"),
        }
    }
    Ok(())
}
