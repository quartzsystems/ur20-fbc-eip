// Copyright (C), 2026 Quartz Systems. Some rights reserved. This work is
// licensed under the terms of the MIT license which can be found in the
// root directory of this project.

//! Class 1 implicit I/O: a Forward_Open over an existing [`Client`] session, then cyclic
//! UDP exchange run by one tokio task.
//!
//! ```no_run
//! use std::time::Duration;
//! use ur20_fbc_eip_cip::{Client, ConnectionParams, ImplicitConnection, ImplicitOptions};
//!
//! # async fn demo() -> ur20_fbc_eip_cip::Result<()> {
//! let mut client = Client::connect("192.168.1.10:44818").await?;
//! // "Exclusive Owner": config instance 1, O→T 112 (4 bytes), T→O 111 (4 bytes), 20 ms
//! let params = ConnectionParams::implicit_class1(1, 112, 111, 4, 4, Duration::from_millis(20));
//! let io = ImplicitConnection::open(&mut client, params, ImplicitOptions::default()).await?;
//! let mut inputs = io.inputs();
//! inputs.changed().await.ok();
//! println!("{:?}", inputs.borrow().as_ref().map(|f| &f.data));
//! io.set_outputs(&[0, 0, 0, 1])?;
//! io.set_run(true);
//! io.close(&mut client).await?;
//! # Ok(()) }
//! ```
//!
//! # Lifecycle
//!
//! The task sends O→T every actual packet interval (from the Forward_Open reply) with the
//! latest outputs and receives T→O. With no valid T→O packet for the connection timeout
//! (RPI × 4 × 2^multiplier) the state becomes [`ImplicitState::TimedOut`] and the task ends.
//! Open a new connection to recover. [`ImplicitConnection::close`] stops the task and sends
//! Forward_Close. Dropping the handle only stops the task: the target notices the missing
//! O→T packets and times the connection out on its side.
//!
//! The connection starts in **idle** mode (run/idle header 0) unless
//! [`ImplicitOptions::run`] is set, so nothing is driven until the caller asks for it.

use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use tokio::net::UdpSocket;
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tokio::time::Instant;
use tracing::{debug, info, trace, warn};

use crate::client::{Client, fill_triad};
use crate::connection::{ConnectionParams, ConnectionType, ForwardOpenReply, TransportClass};
use crate::cpf::Item;
use crate::error::{Error, Result};
use crate::io::{IO_PORT, IoPacket, RunIdleHeader, SequenceCount};

/// Options for [`ImplicitConnection::open`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImplicitOptions {
    /// Local UDP port to receive T→O on. Default [`IO_PORT`] (2222). With any other port a
    /// sockaddr T→O item tells the target where to send; 0 picks a free port.
    pub local_port: u16,
    /// T→O data starts with a run/idle header (the connection's real-time format).
    /// Default `false`, the usual format for adapter inputs.
    pub t_o_run_idle: bool,
    /// Start in run mode instead of idle.
    pub run: bool,
}

impl Default for ImplicitOptions {
    fn default() -> Self {
        Self {
            local_port: IO_PORT,
            t_o_run_idle: false,
            run: false,
        }
    }
}

/// Where the connection is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImplicitState {
    /// Forward_Open accepted, no T→O packet yet.
    WaitingForData,
    /// T→O data is arriving.
    Running,
    /// No valid T→O packet within the connection timeout; the task has ended.
    TimedOut,
    /// [`ImplicitConnection::close`] was called.
    Closed,
    /// A socket error ended the task.
    Failed(String),
}

impl ImplicitState {
    /// The task has ended and the connection cannot be used any more.
    pub fn is_finished(&self) -> bool {
        matches!(self, Self::TimedOut | Self::Closed | Self::Failed(_))
    }
}

/// One received T→O frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputFrame {
    /// The assembly data.
    pub data: Vec<u8>,
    /// CIP sequence count of the packet.
    pub sequence: SequenceCount,
    /// Run/idle header, if the T→O format has one.
    pub run_idle: Option<RunIdleHeader>,
    /// When it arrived.
    pub received: Instant,
}

/// Packet counters, for diagnosis.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct IoCounters {
    /// O→T packets sent.
    pub sent: u64,
    /// T→O packets accepted.
    pub received: u64,
    /// T→O packets dropped as old or duplicate (sequence count not newer).
    pub stale: u64,
    /// Datagrams dropped as foreign, malformed, for another connection, or of the wrong size.
    pub rejected: u64,
}

#[derive(Debug, Default)]
struct Counters {
    sent: AtomicU64,
    received: AtomicU64,
    stale: AtomicU64,
    rejected: AtomicU64,
}

impl Counters {
    fn bump(c: &AtomicU64) {
        c.fetch_add(1, Ordering::Relaxed);
    }

    fn snapshot(&self) -> IoCounters {
        IoCounters {
            sent: self.sent.load(Ordering::Relaxed),
            received: self.received.load(Ordering::Relaxed),
            stale: self.stale.load(Ordering::Relaxed),
            rejected: self.rejected.load(Ordering::Relaxed),
        }
    }
}

#[derive(Debug, Clone)]
struct Outputs {
    data: Vec<u8>,
    run: bool,
}

/// An open class 1 connection.
#[derive(Debug)]
pub struct ImplicitConnection {
    params: ConnectionParams,
    reply: ForwardOpenReply,
    output_len: usize,
    input_len: usize,
    outputs: watch::Sender<Outputs>,
    inputs: watch::Receiver<Option<InputFrame>>,
    state_tx: Arc<watch::Sender<ImplicitState>>,
    state: watch::Receiver<ImplicitState>,
    counters: Arc<Counters>,
    local_addr: SocketAddr,
    o_t_addr: SocketAddr,
    timeout: Duration,
    task: JoinHandle<()>,
}

/// Data bytes in a class 1 connection of `size` with `headers` bytes of headers.
fn data_len(what: &str, size: u16, headers: u16) -> Result<usize> {
    size.checked_sub(headers)
        .map(usize::from)
        .ok_or_else(|| Error::InvalidParams(format!("{what} size {size} < {headers} header bytes")))
}

impl ImplicitConnection {
    /// Forward_Open a class 1 connection over `client`'s session and start the I/O task.
    ///
    /// `params` must be class 1, e.g. from [`ConnectionParams::implicit_class1`]. A zero
    /// connection serial or T→O connection ID is filled in. Outputs start as zeros.
    pub async fn open(
        client: &mut Client,
        mut params: ConnectionParams,
        options: ImplicitOptions,
    ) -> Result<Self> {
        if params.transport.class != TransportClass::Class1 {
            return Err(Error::InvalidParams(format!(
                "transport class {:?}, expected Class1",
                params.transport.class
            )));
        }
        let output_len = data_len("O→T", params.o_t.size, 6)?;
        let input_len = data_len(
            "T→O",
            params.t_o.size,
            if options.t_o_run_idle { 6 } else { 2 },
        )?;
        let IpAddr::V4(peer_ip) = client.peer_addr().ip() else {
            return Err(Error::InvalidParams("class 1 needs an IPv4 target".into()));
        };

        // Bind first, so the port is known and no early packet is lost.
        let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, options.local_port)).await?;
        let local_addr = socket.local_addr()?;
        fill_triad(&mut params);
        let mut extra = Vec::new();
        if local_addr.port() != IO_PORT
            && params.t_o.connection_type == ConnectionType::PointToPoint
        {
            extra.push(Item::SockaddrTO(SocketAddrV4::new(
                Ipv4Addr::UNSPECIFIED,
                local_addr.port(),
            )));
        }
        let reply = client.forward_open_with(&params, &extra).await?;
        let o_t_addr = SocketAddr::new(
            IpAddr::V4(peer_ip),
            reply.o_t_sockaddr.map_or(IO_PORT, |a| a.port()),
        );
        if let Some(group) = reply
            .t_o_sockaddr
            .map(|a| *a.ip())
            .filter(Ipv4Addr::is_multicast)
        {
            // TODO-verify-on-hardware: multicast T→O has not been tried on a UR20-FBC-EIP.
            if let Err(e) = socket.join_multicast_v4(group, Ipv4Addr::UNSPECIFIED) {
                close_quietly(client, &params).await;
                return Err(e.into());
            }
            debug!(%group, "joined T→O multicast group");
        }

        let period = nonzero_or(reply.o_t_api, params.o_t_rpi);
        let timeout = params.timeout_for(nonzero_or(reply.t_o_api, params.t_o_rpi));
        let (outputs, outputs_rx) = watch::channel(Outputs {
            data: vec![0; output_len],
            run: options.run,
        });
        let (inputs_tx, inputs) = watch::channel(None);
        let (state_tx, state) = watch::channel(ImplicitState::WaitingForData);
        let state_tx = Arc::new(state_tx);
        let counters = Arc::new(Counters::default());
        let task = tokio::spawn(
            Task {
                socket,
                o_t_addr,
                o_t_id: reply.o_t_connection_id,
                t_o_id: reply.t_o_connection_id,
                t_o_run_idle: options.t_o_run_idle,
                input_len,
                fixed_input: !params.t_o.variable,
                period,
                timeout,
                outputs: outputs_rx,
                inputs: inputs_tx,
                state: state_tx.clone(),
                counters: counters.clone(),
            }
            .run(),
        );
        info!(
            %o_t_addr, %local_addr, ?period, ?timeout,
            o_t_id = format_args!("{:#010x}", reply.o_t_connection_id),
            t_o_id = format_args!("{:#010x}", reply.t_o_connection_id),
            "class 1 connection open"
        );
        Ok(Self {
            params,
            reply,
            output_len,
            input_len,
            outputs,
            inputs,
            state_tx,
            state,
            counters,
            local_addr,
            o_t_addr,
            timeout,
            task,
        })
    }

    /// Latest-input channel: `None` until the first T→O packet. Use
    /// [`watch::Receiver::changed`] to wait for new data.
    pub fn inputs(&self) -> watch::Receiver<Option<InputFrame>> {
        self.inputs.clone()
    }

    /// The most recent input frame.
    pub fn latest_input(&self) -> Option<InputFrame> {
        self.inputs.borrow().clone()
    }

    /// State channel.
    pub fn state(&self) -> watch::Receiver<ImplicitState> {
        self.state.clone()
    }

    /// Current state.
    pub fn current_state(&self) -> ImplicitState {
        self.state.borrow().clone()
    }

    /// Set the output data sent from the next O→T packet on. Must be exactly
    /// [`ImplicitConnection::output_len`] bytes (at most that for a variable-size connection).
    pub fn set_outputs(&self, data: &[u8]) -> Result<()> {
        let ok = if self.params.o_t.variable {
            data.len() <= self.output_len
        } else {
            data.len() == self.output_len
        };
        if !ok {
            return Err(Error::InvalidParams(format!(
                "{} output byte(s), connection carries {}",
                data.len(),
                self.output_len
            )));
        }
        self.outputs.send_modify(|o| o.data = data.to_vec());
        Ok(())
    }

    /// Switch between run (outputs applied) and idle mode.
    pub fn set_run(&self, run: bool) {
        self.outputs.send_modify(|o| o.run = run);
    }

    /// Outputs and run flag currently being sent.
    pub fn outputs(&self) -> (Vec<u8>, bool) {
        let o = self.outputs.borrow();
        (o.data.clone(), o.run)
    }

    /// O→T data bytes per packet.
    pub fn output_len(&self) -> usize {
        self.output_len
    }

    /// T→O data bytes per packet.
    pub fn input_len(&self) -> usize {
        self.input_len
    }

    /// Packet counters.
    pub fn counters(&self) -> IoCounters {
        self.counters.snapshot()
    }

    /// The Forward_Open reply (connection IDs, actual packet intervals).
    pub fn reply(&self) -> &ForwardOpenReply {
        &self.reply
    }

    /// The parameters the connection was opened with (triad filled in).
    pub fn params(&self) -> &ConnectionParams {
        &self.params
    }

    /// Local UDP address receiving T→O.
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// Where O→T packets go.
    pub fn o_t_addr(&self) -> SocketAddr {
        self.o_t_addr
    }

    /// Connection timeout.
    pub fn timeout(&self) -> Duration {
        self.timeout
    }

    /// Stop the task and send Forward_Close over `client` (the session the connection was
    /// opened on).
    pub async fn close(self, client: &mut Client) -> Result<()> {
        self.task.abort();
        self.state_tx.send_replace(ImplicitState::Closed);
        let request = self.params.forward_close_request()?;
        client
            .send_unconnected(&request)
            .await?
            .into_result(&request)?;
        debug!("class 1 connection closed");
        Ok(())
    }
}

impl Drop for ImplicitConnection {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn nonzero_or(d: Duration, fallback: Duration) -> Duration {
    if d.is_zero() { fallback } else { d }
}

async fn close_quietly(client: &mut Client, params: &ConnectionParams) {
    if let Ok(request) = params.forward_close_request() {
        if let Err(e) = client.send_unconnected(&request).await {
            debug!(error = %e, "Forward_Close after a failed open");
        }
    }
}

struct Task {
    socket: UdpSocket,
    o_t_addr: SocketAddr,
    o_t_id: u32,
    t_o_id: u32,
    t_o_run_idle: bool,
    input_len: usize,
    fixed_input: bool,
    period: Duration,
    timeout: Duration,
    outputs: watch::Receiver<Outputs>,
    inputs: watch::Sender<Option<InputFrame>>,
    state: Arc<watch::Sender<ImplicitState>>,
    counters: Arc<Counters>,
}

impl Task {
    async fn run(self) {
        let end = self.exchange().await;
        warn!(state = ?end, "class 1 connection ended");
        self.state.send_replace(end);
    }

    /// Run until the connection times out or fails; returns the final state.
    async fn exchange(&self) -> ImplicitState {
        let mut tick = tokio::time::interval(self.period);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut encap_sequence = 0u32;
        let mut sequence = SequenceCount(0);
        let mut last_in: Option<SequenceCount> = None;
        let mut deadline = Instant::now() + self.timeout;
        let mut buf = vec![0u8; 1500];
        loop {
            tokio::select! {
                _ = tick.tick() => {
                    encap_sequence = encap_sequence.wrapping_add(1);
                    sequence = sequence.next();
                    let packet = {
                        let o = self.outputs.borrow();
                        IoPacket {
                            connection_id: self.o_t_id,
                            encap_sequence,
                            sequence,
                            run_idle: Some(if o.run { RunIdleHeader::RUN } else { RunIdleHeader::IDLE }),
                            data: o.data.clone(),
                        }
                    };
                    let bytes = match packet.encode() {
                        Ok(b) => b,
                        Err(e) => return ImplicitState::Failed(e.to_string()),
                    };
                    if let Err(e) = self.socket.send_to(&bytes, self.o_t_addr).await {
                        return ImplicitState::Failed(format!("sending O→T: {e}"));
                    }
                    Counters::bump(&self.counters.sent);
                }
                r = self.socket.recv_from(&mut buf) => match r {
                    Ok((n, from)) => {
                        let datagram = buf.get(..n).unwrap_or_default();
                        if self.accept(datagram, from, &mut last_in) {
                            deadline = Instant::now() + self.timeout;
                        }
                    }
                    // Windows reports an ICMP "port unreachable" for an earlier send as an
                    // error on the next receive; it says nothing about this socket.
                    Err(e) if e.kind() == std::io::ErrorKind::ConnectionReset => {
                        trace!("ignoring ICMP-induced connection reset");
                    }
                    Err(e) => return ImplicitState::Failed(format!("receiving T→O: {e}")),
                },
                _ = tokio::time::sleep_until(deadline) => return ImplicitState::TimedOut,
            }
        }
    }

    /// Handle one datagram; `true` if it was a valid packet of this connection (which resets
    /// the watchdog, even when its data is stale).
    fn accept(
        &self,
        datagram: &[u8],
        from: SocketAddr,
        last_in: &mut Option<SequenceCount>,
    ) -> bool {
        if from.ip() != self.o_t_addr.ip() {
            Counters::bump(&self.counters.rejected);
            return false;
        }
        let packet = match IoPacket::decode(datagram, self.t_o_run_idle) {
            Ok(p) if p.connection_id == self.t_o_id => p,
            _ => {
                Counters::bump(&self.counters.rejected);
                return false;
            }
        };
        let size_ok = if self.fixed_input {
            packet.data.len() == self.input_len
        } else {
            packet.data.len() <= self.input_len
        };
        if !size_ok {
            Counters::bump(&self.counters.rejected);
            return false;
        }
        if last_in.is_some_and(|last| !last.is_newer(packet.sequence)) {
            Counters::bump(&self.counters.stale);
            return true;
        }
        *last_in = Some(packet.sequence);
        Counters::bump(&self.counters.received);
        if *self.state.borrow() == ImplicitState::WaitingForData {
            self.state.send_replace(ImplicitState::Running);
        }
        self.inputs.send_replace(Some(InputFrame {
            data: packet.data,
            sequence: packet.sequence,
            run_idle: packet.run_idle,
            received: Instant::now(),
        }));
        true
    }
}
