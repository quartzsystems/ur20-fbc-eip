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

#![allow(clippy::unwrap_used, clippy::expect_used)]

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
