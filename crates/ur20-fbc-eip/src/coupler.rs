// Copyright (C), 2026 Quartz Systems. Some rights reserved. This work is
// licensed under the terms of the MIT license which can be found in the
// root directory of this project.

//! The UR20-FBC-EIP coupler over explicit messaging.

use std::time::Duration;

use tokio::net::ToSocketAddrs;
use tokio::time::Instant;
use tracing::{debug, info, warn};
use ur20_fbc_eip_cip::{
    AttributePath, CipService, Client, ConnectionParams, Identity, ImplicitConnection,
    ImplicitOptions, ImplicitState, InputFrame,
};

use crate::diagnosis::{CouplerDiagnosis, GatewayDiagnosis, SlotDiagnosis, StatusWord};
use crate::error::{Error, Result};
use crate::layout::CouplerLayout;
use crate::module::{ChannelValue, Module};
use crate::modules;
use crate::station::{SlotInfo, SlotValues, Station};

/// Decoded input assembly.
#[derive(Debug, Clone, PartialEq)]
pub struct InputImage {
    /// Raw header bytes (normally 2).
    pub header: Vec<u8>,
    /// Header as a little-endian word, if it is 2 bytes long: the coupler status word.
    /// Use [`InputImage::status`] for the named bits ([`StatusWord`]).
    pub status_word: Option<u16>,
    /// Decoded module inputs, slot order, modules without inputs omitted.
    pub slots: Vec<SlotValues>,
    /// Bytes after the configured modules (e.g. diagnostics of assembly 113).
    pub trailer: Vec<u8>,
}

impl InputImage {
    /// The value of input `channel` of the module in slot `position` (1-based).
    pub fn value(&self, position: usize, channel: usize) -> Option<&ChannelValue> {
        self.slots
            .iter()
            .find(|s| s.position == position)
            .and_then(|s| s.values.get(channel))
    }

    /// The status word with its confirmed bits named.
    pub fn status(&self) -> Option<StatusWord> {
        self.status_word.map(StatusWord)
    }

    /// The digital input `channel` of the module in slot `position`, `None` if there is no
    /// such channel or it is not digital.
    pub fn bit(&self, position: usize, channel: usize) -> Option<bool> {
        match self.value(position, channel)? {
            ChannelValue::Bit(b) => Some(*b),
            _ => None,
        }
    }
}

/// Decoded output assembly (what the coupler is currently driving).
///
/// Read it with [`Coupler::read_outputs`], change values with [`OutputImage::set`] (or
/// directly), and write everything back in one request with [`Coupler::write_outputs`].
#[derive(Debug, Clone, PartialEq)]
pub struct OutputImage {
    /// Raw header bytes, written back unchanged.
    pub header: Vec<u8>,
    /// Decoded module outputs, slot order, modules without outputs omitted.
    pub slots: Vec<SlotValues>,
    /// Bytes after the configured modules, written back unchanged.
    pub trailer: Vec<u8>,
}

impl OutputImage {
    /// The value of output `channel` of the module in slot `position` (1-based).
    pub fn value(&self, position: usize, channel: usize) -> Option<&ChannelValue> {
        self.slots
            .iter()
            .find(|s| s.position == position)
            .and_then(|s| s.values.get(channel))
    }

    /// Change output `channel` of slot `position` in this image (nothing is sent). The value
    /// type is checked by the module codec when the image is written.
    pub fn set(&mut self, position: usize, channel: usize, value: ChannelValue) -> Result<()> {
        let count = self.slots.len();
        let slot = self
            .slots
            .iter_mut()
            .find(|s| s.position == position)
            .ok_or(Error::NoSuchSlot { position, count })?;
        let module = slot.module;
        let count = slot.values.len();
        let v = slot.values.get_mut(channel).ok_or(Error::InvalidChannel {
            module,
            direction: "output",
            channel,
            count,
        })?;
        *v = value;
        Ok(())
    }
}

fn split_header<'a>(what: &str, data: &'a [u8], header_len: usize) -> Result<(&'a [u8], &'a [u8])> {
    if data.len() < header_len {
        return Err(Error::len(
            format!("{what} assembly header"),
            header_len,
            data.len(),
        ));
    }
    Ok(data.split_at(header_len))
}

/// A UR20-FBC-EIP coupler and the modules plugged into it.
///
/// Generic over [`CipService`] so it can run on a [`Client`] or on a test double.
///
/// # Explicit write access
///
/// The coupler drives the modules from the output assembly only while explicit write access
/// is active (0x66/1/108 = 1). Seen on hardware (2026-10-14):
///
/// * Without access, a write to the output assembly is **accepted but ignored**: no error,
///   the assembly reads back the new bytes, the module outputs (0x65/n/114) do not change.
/// * Access lapses when no request arrives for the timeout. The module outputs then **drop
///   to 0**, while the assembly still reads back the last bytes written.
/// * Setting 108 again does not re-apply the assembly; outputs change at the next write.
///
/// So after [`Coupler::enable_explicit_outputs`] this type remembers when it last talked to
/// the coupler. If there was ever a gap of ¾ of the timeout or more since access was last
/// asserted, the next output write first re-asserts it, even if other requests came after
/// the gap. That write sends the whole assembly, so outputs that went off with the lapse come
/// back as the assembly says. Loops that may idle without writing can call
/// [`Coupler::keep_explicit_outputs_alive`]. Requests sent through
/// [`Coupler::service_mut`] are not tracked. Writing outputs without ever requesting access
/// through this type logs a warning, since the coupler would ignore them unless another
/// client holds access.
#[derive(Debug)]
pub struct Coupler<S = Client> {
    service: S,
    layout: CouplerLayout,
    station: Station,
    /// Timeout passed to the last [`Coupler::enable_explicit_outputs`], as written.
    explicit_timeout: Option<Duration>,
    /// When the last request through this coupler got an answer from the device.
    last_request: Option<Instant>,
    /// A gap of ≥ ¾ timeout happened since access was last asserted: it may have lapsed.
    access_gap: bool,
    /// Warned once about writing without requested access.
    warned_no_access: bool,
    /// Open class 1 connection, see [`Coupler::open_implicit`].
    implicit: Option<Implicit>,
}

/// A class 1 connection owned by the coupler, plus our own cursor into its inputs.
#[derive(Debug)]
struct Implicit {
    conn: ImplicitConnection,
    inputs: tokio::sync::watch::Receiver<Option<InputFrame>>,
    state: tokio::sync::watch::Receiver<ImplicitState>,
}

impl Coupler<Client> {
    /// Connect (UCMM) and set up the station from `modules` (slot 1 first).
    pub async fn connect(
        addr: impl ToSocketAddrs,
        layout: CouplerLayout,
        modules: Vec<Box<dyn Module>>,
    ) -> Result<Self> {
        let client = Client::connect(addr).await?;
        Self::new(client, layout, modules)
    }

    /// Connect (UCMM) and build the station from the modules the coupler reports.
    pub async fn connect_auto(addr: impl ToSocketAddrs, layout: CouplerLayout) -> Result<Self> {
        let client = Client::connect(addr).await?;
        Self::detect(client, layout).await
    }

    /// The client is unusable after a timeout or framing error; see [`Coupler::reconnect`].
    pub fn is_broken(&self) -> bool {
        self.service.is_broken()
    }

    /// Replace the TCP connection and session ([`Client::reconnect`]), keeping the layout and
    /// station. A class 3 connection is re-opened if one was open, and explicit write access
    /// is requested again if [`Coupler::enable_explicit_outputs`] was called. A class 1
    /// connection is dropped (its task stops, the coupler times it out); open a new one.
    ///
    /// On error the coupler is unchanged and the call can be retried.
    pub async fn reconnect(&mut self) -> Result<()> {
        self.service.reconnect().await?;
        if self.implicit.take().is_some() {
            info!("class 1 connection dropped by reconnect");
        }
        info!(peer = %self.service.peer_addr(), "reconnected");
        if let Some(timeout) = self.explicit_timeout {
            self.write_explicit_enable(timeout).await?;
            self.access_gap = false;
        }
        Ok(())
    }

    /// Close the class 1 connection, if any, and the session ([`Client::close`]). Dropping the
    /// coupler instead only sends a best-effort UnRegisterSession.
    pub async fn close(mut self) -> Result<()> {
        if let Err(e) = self.close_implicit().await {
            debug!(error = %e, "closing class 1 connection");
        }
        Ok(self.service.close().await?)
    }

    /// Open a class 1 ("Exclusive Owner") connection to the layout's assemblies over this
    /// session, with requested packet interval `rpi`.
    ///
    /// The sizes are taken from one explicit read of each assembly and checked against the
    /// station first, so a wrong module list fails here with [`Error::StationMismatch`].
    /// Outputs start as zeros in idle mode (unless [`ImplicitOptions::run`]); set them with
    /// the usual output methods and switch to run with [`Coupler::set_implicit_run`].
    ///
    /// While the connection is open, output writes go into the next O→T packet instead of
    /// explicit messages: [`Coupler::set_output`], [`Coupler::set_outputs`],
    /// [`Coupler::write_slot_outputs`] and [`Coupler::write_outputs`] change the O→T image,
    /// and [`Coupler::read_outputs`] returns it. [`Coupler::write_output_assembly`] and
    /// [`Coupler::enable_explicit_outputs`] fail with [`Error::ImplicitOpen`]. Explicit reads
    /// keep working.
    pub async fn open_implicit(&mut self, rpi: Duration, options: ImplicitOptions) -> Result<()> {
        if self.implicit.is_some() {
            return Err(Error::ImplicitOpen("close it before opening another"));
        }
        let config = self.layout.config_instance.ok_or_else(|| {
            Error::Unsupported("class 1 needs CouplerLayout::config_instance".into())
        })?;
        let in_len = self.read_input_assembly().await?.len();
        let out_len = self.read_output_assembly().await?.len();
        let expected_in = self.layout.input_header_len + self.station.input_len();
        let expected_out = self.layout.output_header_len + self.station.output_len();
        if in_len < expected_in || out_len < expected_out {
            return Err(Error::StationMismatch {
                position: 0,
                detail: format!(
                    "assemblies are {in_len}/{out_len} bytes in/out, the configured station \
                     needs {expected_in}/{expected_out}"
                ),
            });
        }
        let size = |n: usize| {
            u16::try_from(n).map_err(|_| Error::Unsupported(format!("{n}-byte assembly")))
        };
        let params = ConnectionParams::implicit_class1(
            config,
            self.layout.output.instance,
            self.layout.input.instance,
            size(out_len)?,
            size(in_len)?,
            rpi,
        );
        // The coupler refuses explicit write access while class 1 owns the outputs.
        self.explicit_timeout = None;
        let conn = ImplicitConnection::open(&mut self.service, params, options).await?;
        info!(?rpi, in_len, out_len, "class 1 connection open");
        self.implicit = Some(Implicit {
            inputs: conn.inputs(),
            state: conn.state(),
            conn,
        });
        Ok(())
    }

    /// Close the class 1 connection (Forward_Close), if one is open. Output methods go back
    /// to explicit messaging.
    pub async fn close_implicit(&mut self) -> Result<()> {
        match self.implicit.take() {
            Some(i) => Ok(i.conn.close(&mut self.service).await?),
            None => Ok(()),
        }
    }
}

/// Parse the Gateway module list: little-endian UDINT module IDs, slot 1 first, trailing
/// zeros for empty slots.
pub fn parse_module_list(data: &[u8]) -> Result<Vec<u32>> {
    if data.len() % 4 != 0 {
        return Err(Error::len(
            "module list (multiple of 4)",
            data.len().next_multiple_of(4),
            data.len(),
        ));
    }
    let mut ids: Vec<u32> = data
        .chunks_exact(4)
        .filter_map(|c| <[u8; 4]>::try_from(c).ok())
        .map(u32::from_le_bytes)
        .collect();
    while ids.last() == Some(&0) {
        ids.pop();
    }
    if let Some(i) = ids.iter().position(|id| *id == 0) {
        return Err(Error::StationMismatch {
            position: i + 1,
            detail: "coupler reports an empty slot between modules".into(),
        });
    }
    Ok(ids)
}

/// Little-endian unsigned integer of 1-8 bytes.
fn le_uint(bytes: &[u8]) -> Option<u64> {
    if bytes.is_empty() || bytes.len() > 8 {
        return None;
    }
    let mut buf = [0u8; 8];
    buf.get_mut(..bytes.len())?.copy_from_slice(bytes);
    Some(u64::from_le_bytes(buf))
}

/// Result of reading one Slot class attribute.
enum SlotAttr {
    Data(Vec<u8>),
    /// The slot exists but not this attribute.
    Missing,
    /// No module at this slot: we are past the end of the station.
    NoSlot,
}

impl<S: CipService> Coupler<S> {
    /// Build the station from the module IDs the coupler reports
    /// ([`CouplerLayout::module_list`]). Fails with [`Error::UnsupportedModule`] if any
    /// module has no codec yet.
    pub async fn detect(mut service: S, layout: CouplerLayout) -> Result<Self> {
        let path = layout.module_list.ok_or_else(|| {
            Error::Unsupported("module list disabled in CouplerLayout; pass modules".into())
        })?;
        let ids = parse_module_list(&service.get_attribute_single(path).await?)?;
        info!(count = ids.len(), "detected modules");
        let modules = ids
            .into_iter()
            .map(|id| modules::by_id(id).map(|d| d.instantiate()))
            .collect::<Result<Vec<_>>>()?;
        let mut coupler = Self::new(service, layout, modules)?;
        coupler.last_request = Some(Instant::now());
        Ok(coupler)
    }

    /// Wrap an existing transport.
    pub fn new(service: S, layout: CouplerLayout, modules: Vec<Box<dyn Module>>) -> Result<Self> {
        let station = Station::new(modules, layout.format)?;
        Ok(Self {
            service,
            layout,
            station,
            explicit_timeout: None,
            last_request: None,
            access_gap: false,
            warned_no_access: false,
            implicit: None,
        })
    }

    /// Note that the device answered (any reply, error status included, is explicit traffic).
    fn track<T>(&mut self, r: ur20_fbc_eip_cip::Result<T>) -> Result<T> {
        if r.is_ok() || r.as_ref().is_err_and(|e| e.cip_status().is_some()) {
            if self.idle_exceeds_keepalive() {
                self.access_gap = true;
            }
            self.last_request = Some(Instant::now());
        }
        Ok(r?)
    }

    /// Explicit access is being kept alive and the link has been idle for ≥ ¾ of its timeout.
    fn idle_exceeds_keepalive(&self) -> bool {
        let Some(timeout) = self.explicit_timeout.filter(|t| !t.is_zero()) else {
            return false;
        };
        self.idle_time().is_none_or(|idle| idle >= timeout * 3 / 4)
    }

    /// Get_Attribute_Single, tracked.
    async fn get(&mut self, path: AttributePath) -> Result<Vec<u8>> {
        let r = self.service.get_attribute_single(path).await;
        self.track(r)
    }

    /// Set_Attribute_Single, tracked.
    async fn set(&mut self, path: AttributePath, data: &[u8]) -> Result<()> {
        let r = self.service.set_attribute_single(path, data).await;
        self.track(r)
    }

    /// The open class 1 connection, for its state, counters and raw data.
    pub fn implicit(&self) -> Option<&ImplicitConnection> {
        self.implicit.as_ref().map(|i| &i.conn)
    }

    fn running_implicit(&self) -> Result<&Implicit> {
        let i = self
            .implicit
            .as_ref()
            .ok_or_else(|| Error::ImplicitNotRunning("none open".into()))?;
        let state = i.conn.current_state();
        if state.is_finished() {
            return Err(Error::ImplicitNotRunning(format!("{state:?}")));
        }
        Ok(i)
    }

    /// The latest class 1 input frame, decoded; `None` before the first one arrives.
    pub fn implicit_inputs(&self) -> Result<Option<InputImage>> {
        let i = self.running_implicit()?;
        let frame = i.conn.latest_input();
        frame
            .map(|f| self.decode_input_assembly(&f.data))
            .transpose()
    }

    /// Wait for the next class 1 input frame that this coupler has not returned yet, and
    /// decode it. Fails with [`Error::ImplicitNotRunning`] when the connection ends.
    pub async fn next_implicit_inputs(&mut self) -> Result<InputImage> {
        self.running_implicit()?;
        let Some(i) = self.implicit.as_mut() else {
            return Err(Error::ImplicitNotRunning("none open".into()));
        };
        let data = loop {
            tokio::select! {
                r = i.inputs.changed() => {
                    if r.is_err() {
                        // The task publishes its final state before it drops the inputs
                        // sender, so the state says why it ended.
                        let state = format!("{:?}", i.conn.current_state());
                        return Err(Error::ImplicitNotRunning(state));
                    }
                    if let Some(f) = i.inputs.borrow_and_update().as_ref() {
                        break f.data.clone();
                    }
                }
                s = i.state.wait_for(ImplicitState::is_finished) => {
                    let state = s.map(|s| format!("{:?}", *s)).unwrap_or_else(|_| "task gone".into());
                    return Err(Error::ImplicitNotRunning(state));
                }
            }
        };
        self.decode_input_assembly(&data)
    }

    /// Switch the class 1 connection between run (outputs applied) and idle.
    pub fn set_implicit_run(&self, run: bool) -> Result<()> {
        self.running_implicit()?.conn.set_run(run);
        Ok(())
    }

    /// Time since the device last answered a request sent through this coupler.
    pub fn idle_time(&self) -> Option<Duration> {
        self.last_request.map(|t| t.elapsed())
    }

    /// The configured station.
    pub fn station(&self) -> &Station {
        &self.station
    }

    /// The layout in use.
    pub fn layout(&self) -> &CouplerLayout {
        &self.layout
    }

    /// The underlying transport, e.g. for raw requests.
    pub fn service_mut(&mut self) -> &mut S {
        &mut self.service
    }

    /// Give back the transport.
    pub fn into_inner(self) -> S {
        self.service
    }

    /// Identity Object of the coupler.
    pub async fn identity(&mut self) -> Result<Identity> {
        let r = self.service.identity().await;
        self.track(r)
    }

    /// Raw bytes of the input assembly, header included.
    pub async fn read_input_assembly(&mut self) -> Result<Vec<u8>> {
        self.get(self.layout.input).await
    }

    /// Raw bytes of the output assembly, header included.
    pub async fn read_output_assembly(&mut self) -> Result<Vec<u8>> {
        self.get(self.layout.output).await
    }

    /// Write the whole output assembly, header included.
    ///
    /// Requires explicit write access, see [`Coupler::enable_explicit_outputs`]; re-asserts
    /// it first if it may have lapsed ([`Coupler::keep_explicit_outputs_alive`]).
    pub async fn write_output_assembly(&mut self, data: &[u8]) -> Result<()> {
        if self.implicit.is_some() {
            return Err(Error::ImplicitOpen(
                "explicit output writes would be ignored",
            ));
        }
        self.keep_explicit_outputs_alive().await?;
        if self.layout.explicit_write.is_some()
            && self.explicit_timeout.is_none()
            && !self.warned_no_access
        {
            self.warned_no_access = true;
            warn!(
                "writing outputs without explicit write access requested through this \
                 Coupler; the coupler ignores such writes unless access is active \
                 (call enable_explicit_outputs)"
            );
        }
        self.set(self.layout.output, data).await
    }

    /// Decode an input assembly read earlier.
    pub fn decode_input_assembly(&self, data: &[u8]) -> Result<InputImage> {
        let (header, modules) = split_header("input", data, self.layout.input_header_len)?;
        let slots = self.station.decode_inputs(modules)?;
        let trailer = modules
            .get(self.station.input_len()..)
            .unwrap_or_default()
            .to_vec();
        Ok(InputImage {
            status_word: <[u8; 2]>::try_from(header).ok().map(u16::from_le_bytes),
            header: header.to_vec(),
            slots,
            trailer,
        })
    }

    /// Read and decode the input assembly.
    pub async fn read_inputs(&mut self) -> Result<InputImage> {
        let data = self.read_input_assembly().await?;
        self.decode_input_assembly(&data)
    }

    /// Decode an output assembly read earlier.
    pub fn decode_output_assembly(&self, data: &[u8]) -> Result<OutputImage> {
        let (header, modules) = split_header("output", data, self.layout.output_header_len)?;
        Ok(OutputImage {
            header: header.to_vec(),
            slots: self.station.decode_outputs(modules)?,
            trailer: modules
                .get(self.station.output_len()..)
                .unwrap_or_default()
                .to_vec(),
        })
    }

    /// Encode an output image into assembly bytes (header, module data, trailer).
    ///
    /// Every module with outputs must be in `image`; see [`Station::encode_outputs`].
    pub fn encode_output_assembly(&self, image: &OutputImage) -> Result<Vec<u8>> {
        if image.header.len() != self.layout.output_header_len {
            return Err(Error::len(
                "output image header",
                self.layout.output_header_len,
                image.header.len(),
            ));
        }
        let mut modules = vec![0; self.station.output_len()];
        self.station.encode_outputs(&image.slots, &mut modules)?;
        let mut data = image.header.clone();
        data.extend_from_slice(&modules);
        data.extend_from_slice(&image.trailer);
        Ok(data)
    }

    /// Read and decode the output assembly. With a class 1 connection open this is the O→T
    /// image being sent (no request).
    pub async fn read_outputs(&mut self) -> Result<OutputImage> {
        if let Some(i) = &self.implicit {
            return self.decode_output_assembly(&i.conn.outputs().0);
        }
        let data = self.read_output_assembly().await?;
        self.decode_output_assembly(&data)
    }

    /// Write a whole output image in one request, e.g. one from [`Coupler::read_outputs`]
    /// after changing some values. Nothing is sent if any value is invalid.
    ///
    /// Requires explicit write access, see [`Coupler::enable_explicit_outputs`].
    pub async fn write_outputs(&mut self, image: &OutputImage) -> Result<()> {
        let data = self.encode_output_assembly(image)?;
        debug!(bytes = data.len(), "writing output image");
        if let Some(i) = &self.implicit {
            return Ok(i.conn.set_outputs(&data)?);
        }
        self.write_output_assembly(&data).await
    }

    /// Request explicit messaging write access to outputs.
    ///
    /// Writes the timeout (seconds, rounded up, 0 = until all explicit connections close)
    /// and then sets "Start Explicit Process Data Exchange". The coupler refuses this while a
    /// PLC holds an Exclusive Owner (class 1) connection. Access lapses if no explicit
    /// request arrives within `timeout`. This coupler keeps it alive across idle periods; see
    /// the [type docs](Coupler#explicit-write-access).
    pub async fn enable_explicit_outputs(&mut self, timeout: Duration) -> Result<()> {
        if self.implicit.is_some() {
            return Err(Error::ImplicitOpen(
                "the coupler refuses explicit write access",
            ));
        }
        let secs = timeout.as_secs() + u64::from(timeout.subsec_nanos() > 0);
        let secs = u16::try_from(secs).unwrap_or(u16::MAX);
        let timeout = Duration::from_secs(u64::from(secs));
        self.write_explicit_enable(timeout).await?;
        self.explicit_timeout = self.layout.explicit_write.map(|_| timeout);
        self.access_gap = false;
        info!(timeout_s = secs, "explicit output write access enabled");
        Ok(())
    }

    /// Write 0x66/1/107 (timeout) and 0x66/1/108 = 1.
    async fn write_explicit_enable(&mut self, timeout: Duration) -> Result<()> {
        let Some(ctrl) = self.layout.explicit_write else {
            return Ok(());
        };
        let secs = u16::try_from(timeout.as_secs()).unwrap_or(u16::MAX);
        self.set(ctrl.timeout, &secs.to_le_bytes()).await?;
        self.set(ctrl.start, &[1]).await
    }

    /// Give up explicit write access (0x66/1/108 = 0) and stop keeping it alive.
    pub async fn disable_explicit_outputs(&mut self) -> Result<()> {
        self.explicit_timeout = None;
        let Some(ctrl) = self.layout.explicit_write else {
            return Ok(());
        };
        self.set(ctrl.start, &[0]).await
    }

    /// Re-assert explicit write access if [`Coupler::enable_explicit_outputs`] was called
    /// and it may have lapsed: nothing sent for ¾ of its timeout or longer, now or at any
    /// point since access was last asserted. Returns whether it sent anything. Does nothing
    /// with a timeout of 0 (no lapse) or when access was never requested.
    ///
    /// Output writes call this themselves; call it from loops that may sit idle longer
    /// than the timeout without writing. Re-asserting does not switch outputs back on by
    /// itself (they dropped to 0 with the lapse); the next output write does.
    pub async fn keep_explicit_outputs_alive(&mut self) -> Result<bool> {
        let Some(timeout) = self.explicit_timeout.filter(|t| !t.is_zero()) else {
            return Ok(false);
        };
        if !self.access_gap && !self.idle_exceeds_keepalive() {
            return Ok(false);
        }
        debug!(idle = ?self.idle_time(), gap = self.access_gap, ?timeout, "re-asserting explicit write access");
        self.write_explicit_enable(timeout).await?;
        self.access_gap = false;
        Ok(true)
    }

    /// The timeout of the explicit write access this coupler keeps alive, if any.
    pub fn explicit_outputs_timeout(&self) -> Option<Duration> {
        self.explicit_timeout
    }

    /// Whether explicit write access is currently active (reads 0x66/1/108).
    pub async fn explicit_outputs_enabled(&mut self) -> Result<bool> {
        let Some(ctrl) = self.layout.explicit_write else {
            return Ok(true);
        };
        let v = self.get(ctrl.start).await?;
        Ok(v.first().is_some_and(|b| *b != 0))
    }

    /// Set one output channel: read the output assembly, change the channel, write it back.
    ///
    /// `position` is the 1-based slot. Other outputs keep their current values.
    pub async fn set_output(
        &mut self,
        position: usize,
        channel: usize,
        value: ChannelValue,
    ) -> Result<()> {
        debug!(position, channel, %value, "set output");
        self.modify_outputs(|station, modules| {
            station.set_output(modules, position, channel, &value)
        })
        .await
    }

    /// Set several output channels, given as `(slot, channel, value)`, with one read and one
    /// write of the output assembly.
    ///
    /// All or nothing: if any entry names a missing slot or channel or has the wrong value
    /// type, nothing is written. Later entries for the same channel win. An empty list sends
    /// nothing.
    pub async fn set_outputs(&mut self, changes: &[(usize, usize, ChannelValue)]) -> Result<()> {
        if changes.is_empty() {
            return Ok(());
        }
        debug!(count = changes.len(), "set outputs");
        self.modify_outputs(|station, modules| station.set_outputs(modules, changes))
            .await
    }

    /// Set every output channel of the module in slot `position` (one value per channel),
    /// with one read and one write of the output assembly. Other slots keep their values.
    pub async fn write_slot_outputs(
        &mut self,
        position: usize,
        values: &[ChannelValue],
    ) -> Result<()> {
        debug!(position, count = values.len(), "write slot outputs");
        self.modify_outputs(|station, modules| {
            station.encode_slot_outputs(modules, position, values)
        })
        .await
    }

    /// Read the output assembly, let `change` edit the module data, write it back. With a
    /// class 1 connection open: the same on the O→T image, no requests.
    async fn modify_outputs(
        &mut self,
        change: impl FnOnce(&Station, &mut [u8]) -> Result<()>,
    ) -> Result<()> {
        if let Some(i) = &self.implicit {
            let mut data = i.conn.outputs().0;
            let header_len = self.layout.output_header_len;
            let modules = data
                .get_mut(header_len..)
                .ok_or_else(|| Error::len("O→T image header", header_len, 0))?;
            change(&self.station, modules)?;
            return Ok(i.conn.set_outputs(&data)?);
        }
        // Before the read: the read itself counts as traffic and would hide a lapse.
        self.keep_explicit_outputs_alive().await?;
        let mut data = self.read_output_assembly().await?;
        let header_len = self.layout.output_header_len;
        if data.len() < header_len {
            return Err(Error::len("output assembly header", header_len, data.len()));
        }
        let (_, modules) = data.split_at_mut(header_len);
        change(&self.station, modules)?;
        self.write_output_assembly(&data).await
    }

    /// Ask the coupler what is in each slot (Slot class 0x65): module ID, name and
    /// process data size in bits.
    ///
    /// Iterates instances 1… until the coupler answers "path destination unknown" (0x05, seen
    /// on hardware) or "object does not exist" (0x16). If the bit-size attributes are absent,
    /// falls back to the length of the data attributes.
    pub async fn discover_slots(&mut self) -> Result<Vec<SlotInfo>> {
        let Some(sc) = self.layout.slot_class else {
            return Err(Error::Unsupported(
                "slot class disabled in CouplerLayout".into(),
            ));
        };
        let mut out = Vec::new();
        for instance in 1..=sc.max_slots {
            let attr = |a| AttributePath::new(sc.class, instance, a);
            let module_id = match self.slot_attr(attr(sc.id_attribute)).await? {
                SlotAttr::NoSlot => break,
                SlotAttr::Missing => None,
                // pulled module: the slot remains, its ID reads empty (2026-10-14)
                SlotAttr::Data(d) if d.is_empty() => Some(0),
                SlotAttr::Data(d) => le_uint(&d).and_then(|v| u32::try_from(v).ok()),
            };
            let name = match self.slot_attr(attr(sc.name_attribute)).await? {
                SlotAttr::NoSlot => break,
                SlotAttr::Missing => None,
                SlotAttr::Data(d) => ur20_fbc_eip_cip::codec::Reader::new(&d, "module name")
                    .cip_string()
                    .ok(),
            };
            let input_bits = self
                .slot_bits(attr(sc.input_bits_attribute), attr(sc.input_attribute))
                .await?;
            let output_bits = self
                .slot_bits(attr(sc.output_bits_attribute), attr(sc.output_attribute))
                .await?;
            let (Some(input_bits), Some(output_bits)) = (input_bits, output_bits) else {
                break;
            };
            out.push(SlotInfo {
                position: usize::try_from(instance).unwrap_or(usize::MAX),
                module_id,
                name,
                input_bits,
                output_bits,
            });
        }
        Ok(out)
    }

    /// Bit size from `bits_attr`, else 8 × length of `data_attr`. `None` past the last slot.
    async fn slot_bits(
        &mut self,
        bits_attr: AttributePath,
        data_attr: AttributePath,
    ) -> Result<Option<usize>> {
        match self.slot_attr(bits_attr).await? {
            SlotAttr::NoSlot => return Ok(None),
            SlotAttr::Data(d) => {
                if let Some(bits) = le_uint(&d).and_then(|v| usize::try_from(v).ok()) {
                    return Ok(Some(bits));
                }
            }
            SlotAttr::Missing => {}
        }
        Ok(match self.slot_attr(data_attr).await? {
            SlotAttr::NoSlot => None,
            SlotAttr::Missing => Some(0),
            SlotAttr::Data(d) => Some(8 * d.len()),
        })
    }

    async fn slot_attr(&mut self, path: AttributePath) -> Result<SlotAttr> {
        match self.get(path).await {
            Ok(v) => Ok(SlotAttr::Data(v)),
            Err(e) if e.is_no_slot() => Ok(SlotAttr::NoSlot),
            Err(e) if e.cip_status().is_some_and(|s| s.general == 0x14) => Ok(SlotAttr::Missing),
            Err(e) => Err(e),
        }
    }

    /// Read the coupler diagnosis: status word, latest diagnosis event and unacknowledged
    /// diagnosis ([`CouplerLayout::diagnosis`]). Needs no configured modules.
    ///
    /// Seen on hardware (2026-10-13): while a module was being pulled the unacknowledged
    /// diagnosis briefly failed with CIP status 0x1F (vendor specific), so this call can
    /// fail transiently; it answered again about 8 s later.
    pub async fn diagnosis(&mut self) -> Result<CouplerDiagnosis> {
        let paths = self
            .layout
            .diagnosis
            .ok_or_else(|| Error::Unsupported("diagnosis disabled in CouplerLayout".into()))?;
        let status = StatusWord::decode(&self.get(paths.status_word).await?)?;
        let latest_event = self
            .service
            .get_attribute_single(paths.latest_event)
            .await?;
        let unacknowledged = GatewayDiagnosis::decode(
            &self
                .service
                .get_attribute_single(paths.unacknowledged)
                .await?,
        )?;
        Ok(CouplerDiagnosis {
            status,
            unacknowledged,
            latest_event,
        })
    }

    /// Read the module diagnosis and process alarm of slot `position` (1-based) from the
    /// Slot class. Works for any slot the coupler has, configured in the station or not;
    /// past the last slot the coupler answers 0x05 (see [`Error::cip_status`]).
    pub async fn slot_diagnosis(&mut self, position: usize) -> Result<SlotDiagnosis> {
        let sc = self
            .layout
            .slot_class
            .ok_or_else(|| Error::Unsupported("slot class disabled in CouplerLayout".into()))?;
        let instance = u32::try_from(position)
            .ok()
            .filter(|i| (1..=sc.max_slots).contains(i))
            .ok_or(Error::NoSuchSlot {
                position,
                count: usize::try_from(sc.max_slots).unwrap_or(usize::MAX),
            })?;
        let diag = self
            .service
            .get_attribute_single(AttributePath::new(
                sc.class,
                instance,
                sc.diagnosis_attribute,
            ))
            .await?;
        let alarm = self
            .service
            .get_attribute_single(AttributePath::new(
                sc.class,
                instance,
                sc.process_alarm_attribute,
            ))
            .await?;
        SlotDiagnosis::decode(position, &diag, &alarm)
    }

    /// [`Coupler::slot_diagnosis`] of every slot, from 1 until the coupler reports no
    /// further slot.
    pub async fn slot_diagnoses(&mut self) -> Result<Vec<SlotDiagnosis>> {
        let max = self.layout.slot_class.map_or(0, |sc| sc.max_slots);
        let mut out = Vec::new();
        for position in 1..=usize::try_from(max).unwrap_or(usize::MAX) {
            match self.slot_diagnosis(position).await {
                Ok(d) => out.push(d),
                Err(e) if e.is_no_slot() => break,
                Err(e) => return Err(e),
            }
        }
        Ok(out)
    }

    /// Read the module IDs from the Gateway module list.
    pub async fn module_ids(&mut self) -> Result<Vec<u32>> {
        let path = self
            .layout
            .module_list
            .ok_or_else(|| Error::Unsupported("module list disabled in CouplerLayout".into()))?;
        parse_module_list(&self.get(path).await?)
    }

    /// [`Coupler::discover_slots`] and compare with the configured modules.
    pub async fn verify_station(&mut self) -> Result<Vec<SlotInfo>> {
        let slots = self.discover_slots().await?;
        self.station.verify(&slots)?;
        Ok(slots)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Gateway 0x64/1/111 from a UR20-FBC-EIP with UR20-16DI-P + UR20-16DO-P (2026-10-13).
    fn captured_module_list() -> Vec<u8> {
        let mut v = vec![0xc2, 0x9f, 0x04, 0x00, 0xd0, 0xaf, 0x03, 0x01];
        v.resize(256, 0);
        v
    }

    #[test]
    fn module_list_from_hardware() {
        let ids = parse_module_list(&captured_module_list()).expect("valid");
        assert_eq!(ids, [0x0004_9FC2, 0x0103_AFD0]);
        let names: Vec<_> = ids
            .iter()
            .map(|id| modules::by_id(*id).map(|d| d.name).ok())
            .collect();
        assert_eq!(names, [Some("UR20-16DI-P"), Some("UR20-16DO-P")]);
    }

    #[test]
    fn module_list_errors() {
        assert!(parse_module_list(&[0; 255]).is_err());
        assert_eq!(parse_module_list(&[0; 256]).ok(), Some(vec![]));
        let mut gap = captured_module_list();
        gap[0..4].copy_from_slice(&[0; 4]);
        assert!(matches!(
            parse_module_list(&gap),
            Err(Error::StationMismatch { position: 1, .. })
        ));
    }

    #[test]
    fn le_uints() {
        assert_eq!(le_uint(&[0x10, 0x00]), Some(16));
        assert_eq!(le_uint(&[0x2f]), Some(47));
        assert_eq!(le_uint(&[]), None);
        assert_eq!(le_uint(&[0; 9]), None);
    }
}
