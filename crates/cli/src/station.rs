// Copyright (C), 2026 Quartz Systems. Some rights reserved. This work is
// licensed under the terms of the MIT license which can be found in the
// root directory of this project.

//! Commands that use the module layer: slots, dump-inputs, watch, set, io.

use std::collections::HashMap;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::Args;
use ur20_fbc_eip::cip::{Client, ImplicitOptions, hex};
use ur20_fbc_eip::{ChannelValue, Coupler, CouplerLayout, InputImage, modules};

use crate::parse;

/// Which modules are plugged in and where the coupler keeps their data.
#[derive(Debug, Args)]
pub struct StationArgs {
    /// Modules in slot order, comma separated (e.g. UR20-4DI-P,UR20-4DO-P).
    /// Omit to use the module list the coupler reports.
    #[arg(short, long, env = "UR20_EIP_MODULES", value_delimiter = ',')]
    pub modules: Vec<String>,

    /// Input assembly instance (attribute 3).
    #[arg(long, default_value_t = 111, value_parser = parse::int::<u32>)]
    pub input_assembly: u32,

    /// Output assembly instance (attribute 3).
    #[arg(long, default_value_t = 112, value_parser = parse::int::<u32>)]
    pub output_assembly: u32,

    /// Header bytes before module data in the input assembly.
    #[arg(long, default_value_t = 2)]
    pub input_header: usize,

    /// Header bytes before module data in the output assembly.
    #[arg(long, default_value_t = 2)]
    pub output_header: usize,

    /// Skip comparing the module list with the coupler's slot sizes.
    #[arg(long)]
    pub no_verify: bool,
}

impl StationArgs {
    fn layout(&self) -> CouplerLayout {
        CouplerLayout::default()
            .with_input_instance(self.input_assembly)
            .with_output_instance(self.output_assembly)
            .with_header_lens(self.input_header, self.output_header)
    }

    /// Fail early, before connecting, on an unknown module name.
    pub fn check_modules(&self) -> Result<()> {
        modules::create_all(&self.modules)?;
        Ok(())
    }

    pub async fn coupler(&self, client: Client) -> Result<Coupler<Client>> {
        if self.modules.is_empty() {
            let coupler = Coupler::detect(client, self.layout())
                .await
                .context("detecting modules (pass --modules to set them by hand)")?;
            let names: Vec<_> = coupler
                .station()
                .slots()
                .iter()
                .map(|s| s.module.name())
                .collect();
            tracing::info!(modules = %names.join(","), "detected station");
            return Ok(coupler);
        }
        let modules = modules::create_all(&self.modules)?;
        let mut coupler = Coupler::new(client, self.layout(), modules)?;
        if !self.no_verify {
            coupler
                .verify_station()
                .await
                .context("module list does not match the coupler (use --no-verify to skip)")?;
        }
        Ok(coupler)
    }
}

/// `ur20eip slots`
pub async fn slots(client: Client) -> Result<()> {
    let mut coupler = Coupler::new(client, CouplerLayout::default(), Vec::new())?;
    let slots = coupler.discover_slots().await?;
    if slots.is_empty() {
        println!("no slots reported");
    }
    for s in slots {
        let id = s
            .module_id
            .map(|id| format!("{id:#010x}"))
            .unwrap_or_else(|| "-".into());
        let support = match s.module_id.map(modules::by_id) {
            Some(Ok(_)) => "supported",
            Some(Err(_)) => "not supported yet",
            None => "",
        };
        println!(
            "slot {:>2}: {:<20} id {id}  in {:>3} bit(s)  out {:>3} bit(s)  {support}",
            s.position,
            s.name.as_deref().unwrap_or("?"),
            s.input_bits,
            s.output_bits,
        );
    }
    Ok(())
}

fn print_image(img: &InputImage, raw: Option<&[u8]>) {
    if let Some(raw) = raw {
        println!("raw [{}]: {}", raw.len(), hex(raw));
    }
    match img.status_word {
        Some(w) => println!("status word: {w:#06x}"),
        None => println!("header: {}", hex(&img.header)),
    }
    for slot in &img.slots {
        let values: Vec<String> = slot.values.iter().map(ToString::to_string).collect();
        println!(
            "slot {:>2} {:<18} {}",
            slot.position,
            slot.module,
            values.join(" ")
        );
    }
    if !img.trailer.is_empty() {
        println!("trailer: {}", hex(&img.trailer));
    }
}

/// `ur20eip dump-inputs`
pub async fn dump_inputs(coupler: &mut Coupler<Client>, raw: bool) -> Result<()> {
    let data = coupler.read_input_assembly().await?;
    let img = coupler.decode_input_assembly(&data)?;
    print_image(&img, raw.then_some(data.as_slice()));
    Ok(())
}

/// `ur20eip dump-outputs`
pub async fn dump_outputs(coupler: &mut Coupler<Client>) -> Result<()> {
    let img = coupler.read_outputs().await?;
    println!("header: {}", hex(&img.header));
    for slot in &img.slots {
        let values: Vec<String> = slot.values.iter().map(ToString::to_string).collect();
        println!(
            "slot {:>2} {:<18} {}",
            slot.position,
            slot.module,
            values.join(" ")
        );
    }
    Ok(())
}

/// Options of `ur20eip watch`.
#[derive(Debug, Args)]
pub struct WatchArgs {
    /// Poll interval, e.g. 250ms, 1s.
    #[arg(short, long, default_value = "250ms", value_parser = parse::duration)]
    pub interval: Duration,

    /// When the connection breaks (timeout, reset, coupler reboot), keep trying to
    /// reconnect instead of exiting.
    #[arg(long)]
    pub reconnect: bool,

    /// Request explicit output write access with this timeout and keep it alive while
    /// watching, even if --interval is longer than the timeout.
    #[arg(long, value_name = "TIMEOUT", value_parser = parse::duration)]
    pub write_access: Option<Duration>,
}

/// Retry [`Coupler::reconnect`] with backoff until it works; `false` on Ctrl-C.
async fn reconnect(coupler: &mut Coupler<Client>, start: tokio::time::Instant) -> bool {
    let mut delay = Duration::from_millis(500);
    loop {
        tokio::select! {
            _ = tokio::time::sleep(delay) => {}
            _ = tokio::signal::ctrl_c() => return false,
        }
        let t = start.elapsed().as_secs_f64();
        match coupler.reconnect().await {
            Ok(()) => {
                println!("{t:>9.3}s reconnected");
                return true;
            }
            Err(e) => {
                tracing::info!(error = %e, "reconnect failed");
                delay = (delay * 2).min(Duration::from_secs(5));
            }
        }
    }
}

/// Wait for the next tick of `ticker`, or forever without one.
async fn tick(ticker: &mut Option<tokio::time::Interval>) {
    match ticker {
        Some(t) => {
            t.tick().await;
        }
        None => std::future::pending().await,
    }
}

/// `ur20eip watch`
pub async fn watch(coupler: &mut Coupler<Client>, args: &WatchArgs) -> Result<()> {
    let start = tokio::time::Instant::now();
    if let Some(timeout) = args.write_access {
        coupler
            .enable_explicit_outputs(timeout)
            .await
            .context("enabling explicit output writes (is a PLC connected?)")?;
    }
    // Between polls, wake often enough to re-assert write access before it lapses.
    let mut keepalive = coupler
        .explicit_outputs_timeout()
        .filter(|t| !t.is_zero() && args.interval >= *t / 2)
        .map(|t| {
            let mut i = tokio::time::interval(t / 2);
            i.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            i
        });

    let mut changes = Changes::default();
    let mut ticker = tokio::time::interval(args.interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        let poll = tokio::select! {
            _ = ticker.tick() => true,
            _ = tick(&mut keepalive) => false,
            _ = tokio::signal::ctrl_c() => return Ok(()),
        };
        let result = match coupler.keep_explicit_outputs_alive().await {
            Ok(_) if poll => coupler.read_inputs().await.map(Some),
            Ok(_) => Ok(None),
            Err(e) => Err(e),
        };
        let t = start.elapsed().as_secs_f64();
        let img = match result {
            Ok(Some(img)) => img,
            Ok(None) => continue,
            Err(e) if args.reconnect && coupler.is_broken() => {
                println!("{t:>9.3}s connection lost: {e}; reconnecting");
                if !reconnect(coupler, start).await {
                    return Ok(());
                }
                continue;
            }
            Err(e) => return Err(e.into()),
        };
        changes.print(t, img);
    }
}

/// Prints the status word and input channels when they change.
#[derive(Default)]
struct Changes {
    last: HashMap<(usize, usize), ChannelValue>,
    last_status: Option<u16>,
}

impl Changes {
    fn print(&mut self, t: f64, img: InputImage) {
        if img.status_word != self.last_status {
            if let Some(s) = img.status() {
                println!("{t:>9.3}s status word {s}");
            }
            self.last_status = img.status_word;
        }
        for slot in img.slots {
            for (ch, value) in slot.values.into_iter().enumerate() {
                let key = (slot.position, ch);
                if self.last.get(&key) != Some(&value) {
                    let was = self
                        .last
                        .get(&key)
                        .map(|v| format!(" (was {v})"))
                        .unwrap_or_default();
                    println!(
                        "{t:>9.3}s slot {:>2} {:<18} ch {ch:>2} = {value}{was}",
                        slot.position, slot.module
                    );
                    self.last.insert(key, value);
                }
            }
        }
    }
}

/// Parse `slot:channel=value` changes for the channels' types.
fn typed_changes(
    coupler: &Coupler<Client>,
    changes: &[parse::Assignment],
) -> Result<Vec<(usize, usize, ChannelValue)>> {
    changes
        .iter()
        .map(|a| {
            let module = &coupler.station().slot(a.slot)?.module;
            let kind = module.output_kind(a.channel).with_context(|| {
                format!(
                    "{} in slot {} has {} output channel(s)",
                    module.name(),
                    a.slot,
                    module.output_channels()
                )
            })?;
            Ok((a.slot, a.channel, ChannelValue::parse(&a.value, kind)?))
        })
        .collect()
}

/// Options of `ur20eip io`.
#[derive(Debug, Args)]
pub struct IoArgs {
    /// Requested packet interval, both directions.
    #[arg(long, default_value = "20ms", value_parser = parse::duration)]
    pub rpi: Duration,

    /// Output changes as slot:channel=value, put into the O→T image before the loop starts.
    #[arg(long = "set", value_name = "SLOT:CH=VALUE", value_parser = parse::assignment)]
    pub set: Vec<parse::Assignment>,

    /// Send in run mode, so the coupler applies the outputs. Without it the connection
    /// stays idle: outputs are sent but not applied.
    #[arg(long)]
    pub run: bool,

    /// Stop after this long instead of waiting for Ctrl-C.
    #[arg(long, value_parser = parse::duration)]
    pub duration: Option<Duration>,

    /// Local UDP port for T→O data (the coupler is told if it is not 2222).
    #[arg(long, default_value_t = 2222)]
    pub local_port: u16,

    /// T→O data starts with a run/idle header (try if inputs look shifted by 4 bytes).
    #[arg(long)]
    pub t_o_run_idle: bool,

    /// When the connection ends (timeout, cable or module pulled), keep reopening it,
    /// reconnecting TCP first if needed.
    #[arg(long)]
    pub reconnect: bool,
}

/// `ur20eip io`: class 1 connection, print input changes, optionally drive outputs, and with
/// `--reconnect` reopen it whenever it ends.
pub async fn io(coupler: &mut Coupler<Client>, args: &IoArgs) -> Result<()> {
    let changes = typed_changes(coupler, &args.set)?;
    let start = tokio::time::Instant::now();
    let t = || start.elapsed().as_secs_f64();
    let stop = async {
        match args.duration {
            Some(d) => tokio::time::sleep(d).await,
            None => std::future::pending().await,
        }
    };
    tokio::pin!(stop);
    let mut printer = Changes::default();
    let mut delay = Duration::from_millis(500);
    loop {
        let opened = tokio::select! {
            r = open_class1(coupler, args, &changes) => r,
            _ = &mut stop => return finish(coupler).await,
            _ = tokio::signal::ctrl_c() => return finish(coupler).await,
        };
        if let Err(e) = opened {
            coupler.drop_implicit();
            if !args.reconnect {
                return Err(e.context(
                    "opening the class 1 connection (a PLC connected? wrong module list?)",
                ));
            }
            println!("{:>9.3}s open failed: {e:#}; retrying in {delay:?}", t());
            tokio::select! {
                _ = tokio::time::sleep(delay) => {}
                _ = &mut stop => return finish(coupler).await,
                _ = tokio::signal::ctrl_c() => return finish(coupler).await,
            }
            delay = (delay * 2).min(Duration::from_secs(5));
            continue;
        }
        delay = Duration::from_millis(500);
        let ended = loop {
            tokio::select! {
                r = coupler.next_implicit_inputs() => match r {
                    Ok(img) => printer.print(t(), img),
                    Err(e) => break e,
                },
                _ = &mut stop => return finish(coupler).await,
                _ = tokio::signal::ctrl_c() => return finish(coupler).await,
            }
        };
        let counters = coupler.implicit().map(|io| io.counters());
        println!("{:>9.3}s class 1 ended: {ended}; {counters:?}", t());
        coupler.drop_implicit();
        if !args.reconnect {
            return Err(ended.into());
        }
    }
}

/// Open the class 1 connection (reconnecting TCP first if it broke), then apply `--set` and
/// the mode.
async fn open_class1(
    coupler: &mut Coupler<Client>,
    args: &IoArgs,
    changes: &[(usize, usize, ChannelValue)],
) -> Result<()> {
    if coupler.is_broken() {
        coupler.reconnect().await.context("reconnecting TCP")?;
        println!("TCP session reconnected");
    }
    let options = ImplicitOptions {
        local_port: args.local_port,
        t_o_run_idle: args.t_o_run_idle,
        run: false,
    };
    coupler.open_implicit(args.rpi, options).await?;
    if let Some(io) = coupler.implicit() {
        let r = io.reply();
        println!(
            "class 1 open: RPI {:?}, actual O→T {:?} / T→O {:?}, timeout {:?}, {} → {}, {}/{} bytes out/in",
            args.rpi,
            r.o_t_api,
            r.t_o_api,
            io.timeout(),
            io.local_addr(),
            io.o_t_addr(),
            io.output_len(),
            io.input_len()
        );
    }
    coupler.set_outputs(changes).await?;
    for (slot, channel, value) in changes {
        println!("O→T slot {slot} channel {channel} = {value}");
    }
    coupler.set_implicit_run(args.run)?;
    println!(
        "mode: {}",
        if args.run {
            "run"
        } else {
            "idle (outputs not applied)"
        }
    );
    Ok(())
}

/// Print the final state and counters, then Forward_Close.
async fn finish(coupler: &mut Coupler<Client>) -> Result<()> {
    if let Some(io) = coupler.implicit() {
        println!("state {:?}, {:?}", io.current_state(), io.counters());
    }
    if let Err(e) = coupler.close_implicit().await {
        println!("Forward_Close: {e}");
    }
    Ok(())
}

/// Options for commands that write outputs.
#[derive(Debug, Args)]
pub struct WriteArgs {
    /// Explicit-write timeout written to the coupler before writing.
    #[arg(long, default_value = "5s", value_parser = parse::duration)]
    pub write_timeout: Duration,

    /// Do not request explicit write access first (it may already be active).
    #[arg(long)]
    pub no_enable: bool,
}

/// `ur20eip set` and `ur20eip set-many`: parse every value for its channel's type, then one
/// read-modify-write of the output assembly.
pub async fn set(
    coupler: &mut Coupler<Client>,
    changes: &[parse::Assignment],
    write: &WriteArgs,
) -> Result<()> {
    let changes = typed_changes(coupler, changes)?;
    if !write.no_enable {
        coupler
            .enable_explicit_outputs(write.write_timeout)
            .await
            .context("enabling explicit output writes (is a PLC connected?)")?;
    }
    coupler.set_outputs(&changes).await?;
    for (slot, channel, value) in &changes {
        println!("slot {slot} channel {channel} = {value}");
    }
    Ok(())
}
