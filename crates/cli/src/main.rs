// Copyright (C), 2026 Quartz Systems. Some rights reserved. This work is
// licensed under the terms of the MIT license which can be found in the
// root directory of this project.

//! `ur20eip`: bring-up tool for the Weidmüller UR20-FBC-EIP EtherNet/IP coupler.

mod parse;
mod raw;
mod station;

use std::net::SocketAddr;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use tracing_subscriber::EnvFilter;
use ur20_fbc_eip_cip::{CipService, Client, ConnectionParams};

/// Talk to a Weidmüller u-remote UR20-FBC-EIP coupler over EtherNet/IP.
///
/// Set UR20_EIP_LOG=debug (or trace) for protocol logs on stderr.
#[derive(Debug, Parser)]
#[command(name = "ur20eip", version, about)]
struct Cli {
    /// Coupler address: IP or IP:PORT (default port 44818).
    #[arg(short, long, env = "UR20_EIP_ADDR", value_parser = parse::socket_addr, global = true)]
    addr: Option<SocketAddr>,

    /// Per-request timeout in milliseconds.
    #[arg(long, default_value_t = 3000, global = true)]
    timeout_ms: u64,

    /// Use a class 3 connection (Forward_Open) instead of unconnected messaging.
    #[arg(long, global = true)]
    connected: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Read the Identity Object (class 0x01) and ListIdentity.
    Identity,
    /// List per-slot input/output sizes reported by the coupler (class 0x65).
    Slots,
    /// Read and decode the input assembly once.
    DumpInputs {
        #[command(flatten)]
        station: station::StationArgs,
        /// Also print the raw assembly bytes.
        #[arg(long)]
        raw: bool,
    },
    /// Read and decode the output assembly once.
    DumpOutputs {
        #[command(flatten)]
        station: station::StationArgs,
    },
    /// Poll the input assembly and print channels that change. Ctrl-C to stop.
    Watch {
        #[command(flatten)]
        station: station::StationArgs,
        /// Poll interval, e.g. 250ms, 1s.
        #[arg(short, long, default_value = "250ms", value_parser = parse::duration)]
        interval: Duration,
    },
    /// Set one output channel (read-modify-write of the output assembly).
    Set {
        /// Slot number, 1 = first module next to the coupler.
        slot: usize,
        /// Channel number, from 0.
        channel: usize,
        /// Value: 1/0/on/off for digital, raw counts for analog, 0x hex allowed.
        value: String,
        #[command(flatten)]
        station: station::StationArgs,
        /// Explicit-write timeout written to the coupler before writing.
        #[arg(long, default_value = "5s", value_parser = parse::duration)]
        write_timeout: Duration,
        /// Do not request explicit write access first (it may already be active).
        #[arg(long)]
        no_enable: bool,
    },
    /// Arbitrary class/instance/attribute access.
    #[command(subcommand)]
    Raw(raw::RawCommand),
}

impl Cli {
    fn station_args(&self) -> Option<&station::StationArgs> {
        match &self.command {
            Command::DumpInputs { station, .. }
            | Command::DumpOutputs { station }
            | Command::Watch { station, .. }
            | Command::Set { station, .. } => Some(station),
            _ => None,
        }
    }

    async fn client(&self) -> Result<Client> {
        let addr = self
            .addr
            .context("no coupler address: pass --addr or set UR20_EIP_ADDR")?;
        let mut client = Client::connect_with_timeout(addr, Duration::from_millis(self.timeout_ms))
            .await
            .with_context(|| format!("connecting to {addr}"))?;
        if self.connected {
            client
                .open_connected(ConnectionParams::explicit_class3())
                .await
                .context("Forward_Open")?;
        }
        Ok(client)
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_env("UR20_EIP_LOG").unwrap_or_else(|_| EnvFilter::new("warn")),
        )
        .with_writer(std::io::stderr)
        .init();

    let cli = Cli::parse();
    if let Some(station) = cli.station_args() {
        station.check_modules()?;
    }
    let mut client = cli.client().await?;
    let result = match &cli.command {
        Command::Identity => identity(&mut client).await,
        Command::Raw(cmd) => raw::run(&mut client, cmd).await,
        Command::Slots => return station::slots(client).await,
        Command::DumpInputs { station, raw } => {
            let mut coupler = station.coupler(client).await?;
            let r = station::dump_inputs(&mut coupler, *raw).await;
            client = coupler.into_inner();
            r
        }
        Command::DumpOutputs { station } => {
            let mut coupler = station.coupler(client).await?;
            let r = station::dump_outputs(&mut coupler).await;
            client = coupler.into_inner();
            r
        }
        Command::Watch { station, interval } => {
            let mut coupler = station.coupler(client).await?;
            let r = station::watch(&mut coupler, *interval).await;
            client = coupler.into_inner();
            r
        }
        Command::Set {
            slot,
            channel,
            value,
            station,
            write_timeout,
            no_enable,
        } => {
            let mut coupler = station.coupler(client).await?;
            let r = station::set(
                &mut coupler,
                *slot,
                *channel,
                value,
                *write_timeout,
                !*no_enable,
            )
            .await;
            client = coupler.into_inner();
            r
        }
    };
    if let Err(e) = client.close().await {
        tracing::debug!(error = %e, "closing session");
    }
    result
}

async fn identity(client: &mut Client) -> Result<()> {
    let id = client.identity().await.context("reading Identity Object")?;
    println!("{id}");
    match client.list_identity().await {
        Ok(entries) => {
            for e in entries {
                println!(
                    "list identity: {} (encap v{}, state {})",
                    e.socket_addr, e.protocol_version, e.state
                );
            }
        }
        Err(e) => tracing::info!(error = %e, "ListIdentity not answered"),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::panic)]

    use clap::{CommandFactory, Parser};

    use super::*;
    use crate::parse::HexBytes;

    fn parse(args: &[&str]) -> Cli {
        let mut argv = vec!["ur20eip", "--addr", "10.0.0.1"];
        argv.extend_from_slice(args);
        match Cli::try_parse_from(argv) {
            Ok(cli) => cli,
            Err(e) => panic!("{args:?}: {e}"),
        }
    }

    #[test]
    fn definition_is_consistent() {
        Cli::command().debug_assert();
    }

    #[test]
    fn every_command_parses() {
        for args in [
            &["identity"][..],
            &["slots"],
            &["dump-inputs", "--raw"],
            &["dump-outputs", "-m", "UR20-16DI-P,UR20-16DO-P"],
            &["watch", "-i", "100ms"],
            &["set", "2", "8", "on", "--write-timeout", "5s"],
            &["raw", "get", "0x04", "111", "3"],
            &["raw", "get-all", "1", "1"],
            &["raw", "scan", "0x64", "1", "100", "120"],
        ] {
            parse(args);
        }
    }

    const README: &str = include_str!("../../../README.md");

    /// Every `ur20eip …` line in the README's `sh` blocks parses.
    #[test]
    fn readme_commands_parse() {
        let mut count = 0;
        let mut in_sh = false;
        for line in README.lines().map(str::trim) {
            if line.starts_with("```") {
                in_sh = line == "```sh";
                continue;
            }
            let Some(rest) = line.strip_prefix("ur20eip ").filter(|_| in_sh) else {
                continue;
            };
            let args: Vec<&str> = rest
                .split('#')
                .next()
                .unwrap_or_default()
                .split_whitespace()
                .collect();
            parse(&args);
            count += 1;
        }
        assert!(count >= 10, "only {count} README commands found");
    }

    /// Every `UR20_EIP_*` variable the README mentions is read by the CLI.
    #[test]
    fn readme_env_vars_exist() {
        fn envs(cmd: &clap::Command, out: &mut Vec<String>) {
            for a in cmd.get_arguments() {
                if let Some(e) = a.get_env() {
                    out.push(e.to_string_lossy().into_owned());
                }
            }
            for sub in cmd.get_subcommands() {
                envs(sub, out);
            }
        }
        let mut known = vec!["UR20_EIP_LOG".to_owned()]; // read by main() for tracing
        envs(&Cli::command(), &mut known);
        let mentioned = README
            .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
            .filter(|w| w.starts_with("UR20_EIP_"));
        for var in mentioned {
            assert!(known.iter().any(|k| k == var), "README mentions {var}");
        }
    }

    #[test]
    fn raw_set_takes_one_hex_argument() {
        let cli = parse(&["raw", "set", "0x66", "1", "107", "05 00"]);
        match cli.command {
            Command::Raw(raw::RawCommand::Set { value, .. }) => {
                assert_eq!(value, HexBytes(vec![0x05, 0x00]));
            }
            other => panic!("{other:?}"),
        }
        let cli = parse(&[
            "raw",
            "service",
            "0x0e",
            "1",
            "1",
            "--attribute",
            "7",
            "--data",
            "0102",
        ]);
        match cli.command {
            Command::Raw(raw::RawCommand::Service { data, .. }) => {
                assert_eq!(data, Some(HexBytes(vec![1, 2])));
            }
            other => panic!("{other:?}"),
        }
    }
}
