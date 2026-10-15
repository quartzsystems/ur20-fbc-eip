// Copyright (C), 2026 Quartz Systems. Some rights reserved. This work is
// licensed under the terms of the MIT license which can be found in the
// root directory of this project.

//! Async EtherNet/IP client over TCP: session registration, UCMM and class 3 messaging.

use std::net::SocketAddr;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpStream, ToSocketAddrs};
use tracing::{debug, trace, warn};

use crate::codec::{Reader, hex};
use crate::connection::{ConnectionParams, ForwardOpenReply};
use crate::cpf::{self, Item};
use crate::encap::{self, Frame, HEADER_LEN, Header, command};
use crate::error::{Error, Result};
use crate::identity::ListIdentityEntry;
use crate::message::{MessageRequest, MessageResponse};
use crate::service::CipService;

/// Default per-request timeout.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Debug)]
struct Connected {
    params: ConnectionParams,
    reply: ForwardOpenReply,
    sequence: u16,
}

/// An EtherNet/IP session with one target.
///
/// Requests go out as UCMM (SendRRData) until [`Client::open_connected`] succeeds; after that,
/// [`CipService::send`] uses the class 3 connection (SendUnitData).
///
/// # Closing
///
/// Call [`Client::close`] when done: it sends Forward_Close for an open class 3 connection,
/// unregisters the session, shuts the socket down and reports errors. Rust has no async
/// `Drop`, so a client that is simply dropped can only do part of that: it sends
/// UnRegisterSession with a non-blocking write (no runtime needed, never blocks, errors
/// ignored) unless the stream is [broken](Client::is_broken), and then closes the socket.
/// The target closes any class 3 connection of a session whose TCP connection ends.
///
/// # Reconnecting
///
/// After a timeout or framing error the client is [broken](Client::is_broken) and every
/// request fails with [`Error::Broken`]. [`Client::reconnect`] replaces the TCP connection
/// and session in place and re-opens a class 3 connection if one was open.
#[derive(Debug)]
pub struct Client {
    stream: TcpStream,
    peer: SocketAddr,
    session: u32,
    next_context: u64,
    timeout: Duration,
    connected: Option<Connected>,
    broken: bool,
    /// [`Client::close`] ran, so `Drop` has nothing left to do.
    closed: bool,
}

impl Client {
    /// Connect to `addr` (port 44818 if you pass a bare IP via [`Client::connect_ip`])
    /// and register a session.
    pub async fn connect(addr: impl ToSocketAddrs) -> Result<Self> {
        Self::connect_with_timeout(addr, DEFAULT_TIMEOUT).await
    }

    /// Connect to `ip:44818`.
    pub async fn connect_ip(ip: std::net::IpAddr) -> Result<Self> {
        Self::connect(SocketAddr::new(ip, encap::DEFAULT_PORT)).await
    }

    /// Connect with a custom per-request timeout.
    pub async fn connect_with_timeout(addr: impl ToSocketAddrs, timeout: Duration) -> Result<Self> {
        let stream = tokio::time::timeout(timeout, TcpStream::connect(addr))
            .await
            .map_err(|_| Error::Timeout(timeout))??;
        stream.set_nodelay(true)?;
        let peer = stream.peer_addr()?;
        let mut client = Self {
            stream,
            peer,
            session: 0,
            next_context: 1,
            timeout,
            connected: None,
            broken: false,
            closed: false,
        };
        let reply = client
            .transact(command::REGISTER_SESSION, encap::register_session_data())
            .await?;
        client.session = reply.header.session;
        debug!(%peer, session = format_args!("{:#010x}", client.session), "registered session");
        Ok(client)
    }

    /// Session handle assigned by the target.
    pub fn session_handle(&self) -> u32 {
        self.session
    }

    /// Remote address.
    pub fn peer_addr(&self) -> SocketAddr {
        self.peer
    }

    /// Per-request timeout.
    pub fn timeout(&self) -> Duration {
        self.timeout
    }

    /// Change the per-request timeout.
    pub fn set_timeout(&mut self, timeout: Duration) {
        self.timeout = timeout;
    }

    /// An earlier timeout or framing error left the stream in an unknown state; every
    /// request now fails with [`Error::Broken`] until [`Client::reconnect`].
    pub fn is_broken(&self) -> bool {
        self.broken
    }

    /// Replace the TCP connection and session with fresh ones to the same peer, with the same
    /// timeout. If a class 3 connection was open, a new one is opened with the same
    /// parameters (and a new connection serial). The old session is closed best-effort.
    ///
    /// On error `self` is left as it was, so it can be retried.
    pub async fn reconnect(&mut self) -> Result<()> {
        let mut fresh = Self::connect_with_timeout(self.peer, self.timeout).await?;
        if let Some(state) = &self.connected {
            let params = ConnectionParams {
                connection_serial: 0,
                t_o_connection_id: 0,
                ..state.params.clone()
            };
            fresh.open_connected(params).await?;
        }
        let old = std::mem::replace(self, fresh);
        debug!(peer = %self.peer, was_broken = old.broken, "reconnected");
        if let Err(e) = old.close().await {
            debug!(error = %e, "closing the old session");
        }
        Ok(())
    }

    /// The open class 3 connection, if any.
    pub fn connection(&self) -> Option<&ForwardOpenReply> {
        self.connected.as_ref().map(|c| &c.reply)
    }

    /// ListIdentity over this TCP connection.
    pub async fn list_identity(&mut self) -> Result<Vec<ListIdentityEntry>> {
        let reply = self.transact(command::LIST_IDENTITY, Vec::new()).await?;
        cpf::decode(&reply.data)?
            .into_iter()
            .filter_map(|item| match item {
                Item::Other {
                    type_id: cpf::item_type::LIST_IDENTITY,
                    data,
                } => Some(ListIdentityEntry::decode(&data)),
                _ => None,
            })
            .collect()
    }

    /// Send a request as an unconnected message (UCMM, SendRRData).
    pub async fn send_unconnected(&mut self, request: &MessageRequest) -> Result<MessageResponse> {
        let (response, _) = self.send_rr(request, &[]).await?;
        Ok(response)
    }

    async fn send_rr(
        &mut self,
        request: &MessageRequest,
        extra: &[Item],
    ) -> Result<(MessageResponse, Vec<Item>)> {
        let mr = request.encode()?;
        trace!(path = %request.path, service = request.service, req = %hex(&mr), "UCMM request");
        let mut items = vec![Item::NullAddress, Item::UnconnectedData(mr)];
        items.extend_from_slice(extra);
        let packet = cpf::encode(&items)?;
        let timeout_s = u16::try_from(self.timeout.as_secs().max(1)).unwrap_or(u16::MAX);
        let reply = self
            .transact(command::SEND_RR_DATA, encap::send_data(timeout_s, &packet))
            .await?;
        let items = cpf::decode(encap::parse_send_data(&reply.data)?)?;
        let data = items
            .iter()
            .find_map(|i| match i {
                Item::UnconnectedData(d) => Some(d.clone()),
                _ => None,
            })
            .ok_or_else(|| Error::malformed("SendRRData reply", "no unconnected data item"))?;
        trace!(resp = %hex(&data), "UCMM reply");
        Ok((MessageResponse::decode(&data)?, items))
    }

    /// Send Forward_Open with arbitrary parameters and return the parsed reply.
    ///
    /// This does not change the client's mode; use [`Client::open_connected`] for class 3 and
    /// [`crate::ImplicitConnection::open`] for class 1.
    pub async fn forward_open(&mut self, params: &ConnectionParams) -> Result<ForwardOpenReply> {
        self.forward_open_with(params, &[]).await
    }

    /// [`Client::forward_open`] with extra CPF items after the request, e.g. a sockaddr
    /// T→O item telling the target which UDP port to send class 1 data to.
    pub async fn forward_open_with(
        &mut self,
        params: &ConnectionParams,
        extra: &[Item],
    ) -> Result<ForwardOpenReply> {
        let request = params.forward_open_request()?;
        let (response, items) = self.send_rr(&request, extra).await?;
        if !response.status.is_success() {
            return Err(Error::ForwardOpenRejected {
                status: response.status,
            });
        }
        let data = response.into_result(&request)?;
        let mut reply = ForwardOpenReply::decode(&data)?;
        for item in items {
            match item {
                Item::SockaddrOT(a) => reply.o_t_sockaddr = Some(a),
                Item::SockaddrTO(a) => reply.t_o_sockaddr = Some(a),
                _ => {}
            }
        }
        debug!(?reply, "Forward_Open accepted");
        Ok(reply)
    }

    /// Open a class 3 explicit connection. Subsequent [`CipService`] calls use it.
    pub async fn open_connected(
        &mut self,
        mut params: ConnectionParams,
    ) -> Result<&ForwardOpenReply> {
        if self.connected.is_some() {
            self.close_connected().await?;
        }
        fill_triad(&mut params);
        let reply = self.forward_open(&params).await?;
        let state = self.connected.insert(Connected {
            params,
            reply,
            sequence: 0,
        });
        Ok(&state.reply)
    }

    /// Forward_Close the class 3 connection, if one is open.
    pub async fn close_connected(&mut self) -> Result<()> {
        let Some(state) = self.connected.take() else {
            return Ok(());
        };
        let request = state.params.forward_close_request()?;
        let response = self.send_unconnected(&request).await?;
        response.into_result(&request)?;
        debug!("Forward_Close done");
        Ok(())
    }

    /// Send a request over the open class 3 connection (SendUnitData).
    pub async fn send_connected(&mut self, request: &MessageRequest) -> Result<MessageResponse> {
        let (o_t_id, t_o_id, sequence) = {
            let state = self.connected.as_mut().ok_or(Error::NotConnected)?;
            state.sequence = state.sequence.wrapping_add(1);
            (
                state.reply.o_t_connection_id,
                state.reply.t_o_connection_id,
                state.sequence,
            )
        };
        let mut data = sequence.to_le_bytes().to_vec();
        data.extend_from_slice(&request.encode()?);
        trace!(path = %request.path, sequence, req = %hex(&data), "connected request");
        let packet = cpf::encode(&[Item::ConnectedAddress(o_t_id), Item::ConnectedData(data)])?;
        let reply = self
            .transact(command::SEND_UNIT_DATA, encap::send_data(0, &packet))
            .await?;
        let items = cpf::decode(encap::parse_send_data(&reply.data)?)?;
        let mut address = None;
        let mut payload = None;
        for item in items {
            match item {
                Item::ConnectedAddress(id) => address = Some(id),
                Item::ConnectedData(d) => payload = Some(d),
                _ => {}
            }
        }
        if address != Some(t_o_id) {
            warn!(
                ?address,
                expected = t_o_id,
                "connected reply with unexpected connection ID"
            );
        }
        let payload = payload
            .ok_or_else(|| Error::malformed("SendUnitData reply", "no connected data item"))?;
        let mut r = Reader::new(&payload, "connected data");
        let echoed = r.u16()?;
        if echoed != sequence {
            self.broken = true;
            return Err(Error::malformed(
                "SendUnitData reply",
                format!("sequence count {echoed} does not match request {sequence}"),
            ));
        }
        MessageResponse::decode(r.rest())
    }

    /// Close the class 3 connection (if any) and unregister the session.
    pub async fn close(mut self) -> Result<()> {
        self.closed = true;
        if !self.broken {
            if let Err(e) = self.close_connected().await {
                warn!(error = %e, "Forward_Close failed");
            }
            let frame = Frame::request(command::UNREGISTER_SESSION, self.session, 0, Vec::new());
            self.stream.write_all(&frame.encode()?).await?;
        }
        self.stream.shutdown().await?;
        Ok(())
    }

    /// Send one encapsulation command and wait for the reply with the same context.
    async fn transact(&mut self, cmd: u16, data: Vec<u8>) -> Result<Frame> {
        if self.broken {
            return Err(Error::Broken);
        }
        let context = self.next_context;
        self.next_context = self.next_context.wrapping_add(1);
        let bytes = Frame::request(cmd, self.session, context, data).encode()?;
        let timeout = self.timeout;
        let result = tokio::time::timeout(timeout, async {
            self.stream.write_all(&bytes).await?;
            loop {
                let frame = self.read_frame().await?;
                // SendUnitData replies carry no meaningful context.
                if cmd != command::SEND_UNIT_DATA && frame.header.context != context {
                    warn!(
                        got = frame.header.context,
                        expected = context,
                        "discarding stale reply"
                    );
                    continue;
                }
                if frame.header.command != cmd {
                    return Err(Error::malformed(
                        "encapsulation reply",
                        format!("command {:#06x}, expected {cmd:#06x}", frame.header.command),
                    ));
                }
                frame.check_status()?;
                return Ok(frame);
            }
        })
        .await;
        match result {
            Ok(Ok(frame)) => Ok(frame),
            Ok(Err(e)) => {
                if matches!(
                    e,
                    Error::Io(_)
                        | Error::Closed
                        | Error::Truncated { .. }
                        | Error::Malformed { .. }
                ) {
                    self.broken = true;
                }
                Err(e)
            }
            Err(_) => {
                self.broken = true;
                Err(Error::Timeout(timeout))
            }
        }
    }

    async fn read_frame(&mut self) -> Result<Frame> {
        let mut head = [0u8; HEADER_LEN];
        read_exact(&mut self.stream, &mut head).await?;
        let header = Header::decode(&head)?;
        let mut data = vec![0u8; usize::from(header.length)];
        read_exact(&mut self.stream, &mut data).await?;
        Ok(Frame { header, data })
    }
}

impl Drop for Client {
    /// Best-effort UnRegisterSession for a client that was not [closed](Client::close).
    fn drop(&mut self) {
        if self.closed || self.broken {
            return;
        }
        let Ok(frame) =
            Frame::request(command::UNREGISTER_SESSION, self.session, 0, Vec::new()).encode()
        else {
            return;
        };
        // Synchronous and non-blocking: works without a runtime context and during runtime
        // shutdown. 28 bytes fit any socket send buffer, so a short write means the socket
        // is already unusable.
        match self.stream.try_write(&frame) {
            Ok(n) if n == frame.len() => debug!(peer = %self.peer, "dropped: session unregistered"),
            Ok(n) => debug!(n, "dropped: partial UnRegisterSession"),
            Err(e) => debug!(error = %e, "dropped: UnRegisterSession not sent"),
        }
    }
}

async fn read_exact(stream: &mut TcpStream, buf: &mut [u8]) -> Result<()> {
    match stream.read_exact(buf).await {
        Ok(_) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => Err(Error::Closed),
        Err(e) => Err(e.into()),
    }
}

/// Give `params` a connection serial and T→O connection ID where they are 0.
pub(crate) fn fill_triad(params: &mut ConnectionParams) {
    let (serial, t_o_id) = triad_seed();
    if params.connection_serial == 0 {
        params.connection_serial = serial;
    }
    if params.t_o_connection_id == 0 {
        params.t_o_connection_id = t_o_id;
    }
}

/// Pseudo-random connection serial and T→O connection ID (only uniqueness matters).
fn triad_seed() -> (u16, u32) {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos() ^ (d.as_secs() as u32))
        .unwrap_or(0x5A5A_5A5A);
    let mixed = nanos.wrapping_mul(0x9E37_79B9) ^ std::process::id();
    ((mixed >> 16) as u16 | 1, mixed | 1)
}

impl CipService for Client {
    async fn send(&mut self, request: MessageRequest) -> Result<MessageResponse> {
        if self.connected.is_some() {
            self.send_connected(&request).await
        } else {
            self.send_unconnected(&request).await
        }
    }
}
