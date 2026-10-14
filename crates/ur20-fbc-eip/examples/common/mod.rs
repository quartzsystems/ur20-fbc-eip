// Copyright (C), 2026 Quartz Systems. Some rights reserved. This work is
// licensed under the terms of the MIT license which can be found in the
// root directory of this project.

//! Setup shared by the examples, configured from the environment like `ur20eip`:
//!
//! * `UR20_EIP_ADDR`: coupler IP or IP:PORT (port 44818 if omitted). Required.
//! * `UR20_EIP_MODULES`: modules in slot order, comma separated, checked against the coupler.
//!   If unset, the module list is read from the coupler ([`Coupler::connect_auto`]).

use std::net::{IpAddr, SocketAddr};

use ur20_fbc_eip::cip::encap::DEFAULT_PORT;
use ur20_fbc_eip::{Category, Coupler, CouplerLayout, modules};

/// Examples report any error and exit.
pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// `UR20_EIP_ADDR` as a socket address.
pub fn addr() -> Result<SocketAddr> {
    let s = std::env::var("UR20_EIP_ADDR")
        .map_err(|_| "set UR20_EIP_ADDR to the coupler IP or IP:PORT")?;
    let s = s.trim();
    s.parse::<SocketAddr>()
        .or_else(|_| {
            s.parse::<IpAddr>()
                .map(|ip| SocketAddr::new(ip, DEFAULT_PORT))
        })
        .map_err(|_| format!("UR20_EIP_ADDR=`{s}` is not an IP or IP:PORT").into())
}

/// `UR20_EIP_MODULES`, empty if unset.
pub fn module_names() -> Vec<String> {
    std::env::var("UR20_EIP_MODULES")
        .map(|s| {
            s.split(',')
                .map(|m| m.trim().to_owned())
                .filter(|m| !m.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

/// Connect to the coupler and set up the station: from `UR20_EIP_MODULES` (verified against
/// the coupler) if set, otherwise from the coupler's module list.
pub async fn coupler() -> Result<Coupler> {
    let addr = addr()?;
    let names = module_names();
    let coupler = if names.is_empty() {
        Coupler::connect_auto(addr, CouplerLayout::default()).await?
    } else {
        let modules = modules::create_all(&names)?;
        let mut c = Coupler::connect(addr, CouplerLayout::default(), modules).await?;
        c.verify_station().await?;
        c
    };
    let names: Vec<_> = coupler
        .station()
        .slots()
        .iter()
        .map(|s| s.module.name())
        .collect();
    println!("{addr}: {}", names.join(", "));
    Ok(coupler)
}

/// Fail unless the module in `slot` is of `category` and has at least `channels` channels.
pub fn require(coupler: &Coupler, slot: usize, category: Category, channels: usize) -> Result<()> {
    let d = coupler.station().slot(slot)?.module.descriptor();
    if d.category != category || d.channels < channels {
        return Err(format!(
            "this example needs a {category:?} module with at least {channels} channels in \
             slot {slot}, found {}",
            d.name
        )
        .into());
    }
    Ok(())
}
