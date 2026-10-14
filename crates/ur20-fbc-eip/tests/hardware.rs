// Copyright (C), 2026 Quartz Systems. Some rights reserved. This work is
// licensed under the terms of the MIT license which can be found in the
// root directory of this project.

//! Tests against a real UR20-FBC-EIP. Ignored by default; read-only unless noted.
//!
//! ```sh
//! export UR20_EIP_ADDR=192.168.1.10
//! export UR20_EIP_MODULES=UR20-4DI-P,UR20-4DO-P      # optional; detected if unset
//! cargo test -p ur20-fbc-eip --test hardware -- --ignored --nocapture
//! ```

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use std::net::SocketAddr;

use ur20_fbc_eip::cip::{Client, hex};
use ur20_fbc_eip::{Coupler, CouplerLayout, modules};

fn addr() -> SocketAddr {
    let s = std::env::var("UR20_EIP_ADDR").expect("set UR20_EIP_ADDR to the coupler IP");
    s.parse::<SocketAddr>()
        .or_else(|_| {
            s.parse::<std::net::IpAddr>()
                .map(|ip| SocketAddr::new(ip, 44818))
        })
        .expect("UR20_EIP_ADDR must be IP or IP:PORT")
}

fn configured_modules() -> Vec<String> {
    std::env::var("UR20_EIP_MODULES")
        .map(|s| s.split(',').map(|m| m.trim().to_owned()).collect())
        .unwrap_or_default()
}

#[tokio::test]
#[ignore = "needs hardware: set UR20_EIP_ADDR"]
async fn dump_assemblies_and_slots() {
    let client = Client::connect(addr()).await.unwrap();
    let mut c = Coupler::new(client, CouplerLayout::default(), Vec::new()).unwrap();
    println!("{}", c.identity().await.unwrap());
    let input = c.read_input_assembly().await.unwrap();
    println!("input assembly  [{}]: {}", input.len(), hex(&input));
    let output = c.read_output_assembly().await.unwrap();
    println!("output assembly [{}]: {}", output.len(), hex(&output));
    println!("module ids: {:08x?}", c.module_ids().await.unwrap());
    for s in c.discover_slots().await.unwrap() {
        println!(
            "slot {:>2}: {:?} {:08x?} in {:>3} bits, out {:>3} bits",
            s.position, s.name, s.module_id, s.input_bits, s.output_bits
        );
    }
}

#[tokio::test]
#[ignore = "needs hardware: set UR20_EIP_ADDR (UR20_EIP_MODULES optional)"]
async fn configured_station_matches_hardware() {
    let names = configured_modules();
    let mut c = if names.is_empty() {
        Coupler::connect_auto(addr(), CouplerLayout::default())
            .await
            .unwrap()
    } else {
        let modules = modules::create_all(&names).unwrap();
        Coupler::connect(addr(), CouplerLayout::default(), modules)
            .await
            .unwrap()
    };
    c.verify_station().await.unwrap();
    let img = c.read_inputs().await.unwrap();
    println!("{img:#?}");
}

/// What the module in `slot` is actually driving: Slot class 0x65/n/114. The output assembly
/// is not enough: without write access the coupler accepts writes there but ignores them,
/// and after a lapse it keeps reporting the last image while the outputs are off
/// (2026-10-14).
async fn driven(c: &mut Coupler, slot: usize) -> Vec<u8> {
    use ur20_fbc_eip::cip::{AttributePath, CipService};
    let instance = u32::try_from(slot).unwrap();
    c.service_mut()
        .get_attribute_single(AttributePath::new(0x65, instance, 114))
        .await
        .unwrap()
}

/// Switches real outputs. Needs `UR20_EIP_ALLOW_WRITES=1` as well as `--ignored`, a digital
/// output module somewhere on the station, and no PLC owning the outputs. Uses a 2 s
/// explicit-write timeout, checks every write against what the module drives, lets access
/// lapse, and restores the original outputs.
#[tokio::test]
#[ignore = "needs hardware and UR20_EIP_ALLOW_WRITES=1: switches outputs"]
async fn digital_writes_outputs() {
    use std::time::Duration;
    use ur20_fbc_eip::{Category, ChannelValue::Bit, OutputImage};

    if std::env::var("UR20_EIP_ALLOW_WRITES").as_deref() != Ok("1") {
        println!("skipped: set UR20_EIP_ALLOW_WRITES=1 to switch real outputs");
        return;
    }
    let timeout = Duration::from_secs(2);
    let mut c = Coupler::connect_auto(addr(), CouplerLayout::default())
        .await
        .unwrap();
    let (slot, channels, range) = c
        .station()
        .slots()
        .iter()
        .find(|s| s.module.descriptor().category == Category::DigitalOutput)
        .map(|s| (s.position, s.module.output_channels(), s.outputs.clone()))
        .expect("a digital output module on the station");
    println!("DO module in slot {slot}, {channels} channels");
    let header = c.layout().output_header_len;
    // the slot's bytes of an output image, as the module should drive them
    let bytes = |c: &Coupler, img: &OutputImage| -> Vec<u8> {
        c.encode_output_assembly(img).unwrap()[header + range.start..header + range.end].to_vec()
    };

    c.enable_explicit_outputs(timeout).await.unwrap();
    assert!(c.explicit_outputs_enabled().await.unwrap());
    let saved = c.read_outputs().await.unwrap();

    // batch: first and last channel on, everything else off, in one write
    let mut changes: Vec<_> = (0..channels).map(|ch| (slot, ch, Bit(false))).collect();
    changes.push((slot, 0, Bit(true)));
    changes.push((slot, channels - 1, Bit(true)));
    c.set_outputs(&changes).await.unwrap();
    let img = c.read_outputs().await.unwrap();
    for ch in 0..channels {
        let on = ch == 0 || ch == channels - 1;
        assert_eq!(img.value(slot, ch), Some(&Bit(on)), "channel {ch}");
    }
    assert_eq!(driven(&mut c, slot).await, bytes(&c, &img));
    println!(
        "set_outputs: 112 = {}, 114 = {}",
        hex(&c.read_output_assembly().await.unwrap()),
        hex(&driven(&mut c, slot).await)
    );

    // whole slot, one write per step
    for ch in 0..channels {
        let pattern: Vec<_> = (0..channels).map(|n| Bit(n == ch)).collect();
        c.write_slot_outputs(slot, &pattern).await.unwrap();
        let img = c.read_outputs().await.unwrap();
        assert_eq!(
            img.slots
                .iter()
                .find(|s| s.position == slot)
                .unwrap()
                .values,
            pattern
        );
        assert_eq!(driven(&mut c, slot).await, bytes(&c, &img), "channel {ch}");
    }

    // image path: read, modify, write
    let mut img = c.read_outputs().await.unwrap();
    img.set(slot, 0, Bit(true)).unwrap();
    c.write_outputs(&img).await.unwrap();
    assert_eq!(c.read_outputs().await.unwrap(), img);
    assert_eq!(driven(&mut c, slot).await, bytes(&c, &img));

    // Leave channel 0 on and go quiet for longer than the timeout.
    let mut on = saved.clone();
    on.set(slot, 0, Bit(true)).unwrap();
    c.write_outputs(&on).await.unwrap();
    tokio::time::sleep(timeout + Duration::from_millis(1500)).await;
    let enabled = c.explicit_outputs_enabled().await.unwrap();
    let image_after = c.read_output_assembly().await.unwrap();
    let driven_after = driven(&mut c, slot).await;
    println!(
        "after {timeout:?} + 1.5 s idle: 0x66/1/108 = {enabled}, 112 = {}, 114 = {}",
        hex(&image_after),
        hex(&driven_after)
    );
    assert!(
        !enabled,
        "write access should lapse after {timeout:?} without traffic"
    );
    assert!(
        driven_after.iter().all(|b| *b == 0),
        "outputs drop when access lapses"
    );

    // The reads above came after the lapse; the next write must still re-assert access.
    c.write_outputs(&on).await.unwrap();
    assert!(c.explicit_outputs_enabled().await.unwrap());
    assert_eq!(driven(&mut c, slot).await, bytes(&c, &on), "driven again");

    // Polling slower than the timeout, writing every poll: every write must be driven.
    for i in 0..3 {
        tokio::time::sleep(timeout + Duration::from_millis(500)).await;
        c.set_output(slot, 0, Bit(i % 2 == 0)).await.unwrap();
        let img = c.read_outputs().await.unwrap();
        assert_eq!(
            driven(&mut c, slot).await,
            bytes(&c, &img),
            "slow write {i}"
        );
    }

    c.write_outputs(&saved).await.unwrap();
    assert_eq!(driven(&mut c, slot).await, bytes(&c, &saved));
    c.disable_explicit_outputs().await.unwrap();
    c.close().await.unwrap();
}

#[tokio::test]
#[ignore = "needs hardware: set UR20_EIP_ADDR"]
async fn diagnosis() {
    let mut c = Coupler::connect_auto(addr(), CouplerLayout::default())
        .await
        .unwrap();
    let d = c.diagnosis().await.unwrap();
    println!("{d:?}\nstatus word: {}", d.status);
    let slots = c.slot_diagnoses().await.unwrap();
    for s in &slots {
        println!("{s:?}");
    }
    // every slot of the detected station answers, plus none beyond it
    assert_eq!(slots.len(), c.station().slots().len());
    let img = c.read_inputs().await.unwrap();
    assert_eq!(
        img.status(),
        Some(d.status),
        "0x64/1/104 mirrors the 111 header"
    );
}
