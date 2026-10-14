// Copyright (C), 2026 Quartz Systems. Some rights reserved. This work is
// licensed under the terms of the MIT license which can be found in the
// root directory of this project.

//! Tests against a real EtherNet/IP adapter. Ignored by default.
//!
//! ```sh
//! UR20_EIP_ADDR=192.168.1.10 cargo test -p ur20-fbc-eip-cip --test hardware -- --ignored --nocapture
//! ```

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::net::SocketAddr;

use ur20_fbc_eip_cip::{AttributePath, CipService, Client, ConnectionParams};

fn addr() -> SocketAddr {
    let s = std::env::var("UR20_EIP_ADDR").expect("set UR20_EIP_ADDR to the coupler IP");
    s.parse::<SocketAddr>()
        .or_else(|_| {
            s.parse::<std::net::IpAddr>()
                .map(|ip| SocketAddr::new(ip, 44818))
        })
        .expect("UR20_EIP_ADDR must be IP or IP:PORT")
}

#[tokio::test]
#[ignore = "needs hardware: set UR20_EIP_ADDR"]
async fn identity_unconnected() {
    let mut c = Client::connect(addr()).await.unwrap();
    let id = c.identity().await.unwrap();
    println!("{id}");
    assert!(!id.product_name.is_empty());
    let li = c.list_identity().await.unwrap();
    println!("{li:#?}");
    c.close().await.unwrap();
}

#[tokio::test]
#[ignore = "needs hardware: set UR20_EIP_ADDR"]
async fn identity_connected() {
    let mut c = Client::connect(addr()).await.unwrap();
    let reply = c
        .open_connected(ConnectionParams::explicit_class3())
        .await
        .unwrap();
    println!("{reply:#?}");
    let name = c
        .get_attribute_single(AttributePath::new(0x01, 1, 7))
        .await
        .unwrap();
    println!("product name attr: {}", ur20_fbc_eip_cip::hex(&name));
    c.close().await.unwrap();
}
