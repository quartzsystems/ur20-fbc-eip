// Copyright (C), 2026 Quartz Systems. Some rights reserved. This work is
// licensed under the terms of the MIT license which can be found in the
// root directory of this project.

//! Argument parsers shared by the subcommands.

use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

/// `IP` or `IP:PORT`.
pub fn socket_addr(s: &str) -> Result<SocketAddr, String> {
    s.parse::<SocketAddr>()
        .or_else(|_| s.parse::<IpAddr>().map(|ip| SocketAddr::new(ip, 44818)))
        .map_err(|_| format!("`{s}` is not an IP or IP:PORT"))
}

/// Decimal or `0x` hex integer.
pub fn int<T: TryFrom<u64>>(s: &str) -> Result<T, String> {
    let v = if let Some(h) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        u64::from_str_radix(&h.replace('_', ""), 16)
    } else {
        s.replace('_', "").parse::<u64>()
    }
    .map_err(|e| format!("`{s}`: {e}"))?;
    T::try_from(v).map_err(|_| format!("`{s}` is out of range"))
}

/// `250ms`, `2s`, `1.5s` or a bare number of milliseconds.
pub fn duration(s: &str) -> Result<Duration, String> {
    let bad = || format!("`{s}`: expected e.g. 250ms or 2s");
    let s = s.trim();
    let (num, scale) = if let Some(n) = s.strip_suffix("ms") {
        (n, 1e-3)
    } else if let Some(n) = s.strip_suffix('s') {
        (n, 1.0)
    } else {
        (s, 1e-3)
    };
    let v: f64 = num.trim().parse().map_err(|_| bad())?;
    Duration::try_from_secs_f64(v * scale).map_err(|_| bad())
}

/// Bytes given as one hex argument. A newtype, because clap reads a plain `Vec<u8>` field
/// as a list of separate `u8` arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HexBytes(pub Vec<u8>);

/// [`hex_bytes`] wrapped in [`HexBytes`], for use as a clap value parser.
pub fn hex_arg(s: &str) -> Result<HexBytes, String> {
    hex_bytes(s).map(HexBytes)
}

/// Hex bytes: `01 02 ff`, `0102ff`, `01,02,ff` or `0x0102ff`.
pub fn hex_bytes(s: &str) -> Result<Vec<u8>, String> {
    let cleaned: String = s
        .trim_start_matches("0x")
        .chars()
        .filter(|c| !matches!(c, ' ' | ',' | ':' | '_'))
        .collect();
    if cleaned.len() % 2 != 0 {
        return Err(format!("`{s}`: odd number of hex digits"));
    }
    (0..cleaned.len())
        .step_by(2)
        .map(|i| {
            cleaned
                .get(i..i + 2)
                .and_then(|b| u8::from_str_radix(b, 16).ok())
                .ok_or_else(|| format!("`{s}`: invalid hex"))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ints() {
        assert_eq!(int::<u16>("0x64"), Ok(100));
        assert_eq!(int::<u16>("111"), Ok(111));
        assert!(int::<u8>("0x100").is_err());
        assert!(int::<u8>("x").is_err());
    }

    #[test]
    fn bytes() {
        assert_eq!(hex_bytes("01 02 ff"), Ok(vec![1, 2, 0xff]));
        assert_eq!(hex_bytes("0x0102"), Ok(vec![1, 2]));
        assert_eq!(hex_bytes("01,02"), Ok(vec![1, 2]));
        assert_eq!(hex_bytes(""), Ok(vec![]));
        assert!(hex_bytes("012").is_err());
        assert!(hex_bytes("zz").is_err());
    }

    #[test]
    fn durations() {
        assert_eq!(duration("250ms"), Ok(Duration::from_millis(250)));
        assert_eq!(duration("2s"), Ok(Duration::from_secs(2)));
        assert_eq!(duration("1.5s"), Ok(Duration::from_millis(1500)));
        assert_eq!(duration("100"), Ok(Duration::from_millis(100)));
        assert!(duration("-1s").is_err());
        assert!(duration("fast").is_err());
    }

    #[test]
    fn addrs() {
        assert_eq!(socket_addr("10.0.0.1").map(|a| a.port()), Ok(44818));
        assert_eq!(socket_addr("10.0.0.1:2000").map(|a| a.port()), Ok(2000));
        assert!(socket_addr("nope").is_err());
    }
}
