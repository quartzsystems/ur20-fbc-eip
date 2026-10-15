// Copyright (C), 2026 Quartz Systems. Some rights reserved. This work is
// licensed under the terms of the MIT license which can be found in the
// root directory of this project.

//! Class 1 implicit I/O against an in-process adapter: Forward_Open over TCP, then T→O
//! produced at the RPI and O→T recorded, over real UDP on loopback.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use ur20_fbc_eip_cip::codec::Reader;
use ur20_fbc_eip_cip::cpf::{self, Item};
use ur20_fbc_eip_cip::encap::{self, Frame, HEADER_LEN, Header, command};
use ur20_fbc_eip_cip::io::{IoPacket, RunIdleHeader, SequenceCount};
use ur20_fbc_eip_cip::message::{CipStatus, MessageResponse, service};
use ur20_fbc_eip_cip::{
    Client, ConnectionParams, Error, ImplicitConnection, ImplicitOptions, ImplicitState,
};

const O_T_ID: u32 = 0x1000_0001;
const RPI: Duration = Duration::from_millis(10);

/// The bench station's assemblies: 111 and 112 are 4 bytes each.
const ASSEMBLY: u16 = 4;

#[derive(Default)]
struct Adapter {
    /// Assembly 111 as produced T→O.
    inputs: Mutex<Vec<u8>>,
    /// Every O→T packet received.
    received: Mutex<Vec<IoPacket>>,
    /// Stop producing T→O (the connection should time out).
    silent: AtomicBool,
    /// Forward_Open / Forward_Close seen.
    opens: AtomicU16,
    closes: AtomicU16,
    /// The T→O port the originator asked for in its sockaddr item.
    requested_port: AtomicU16,
}

impl Adapter {
    fn new() -> Arc<Self> {
        let a = Self::default();
        *a.inputs.lock().unwrap() = vec![0, 0, 0, 0];
        Arc::new(a)
    }

    fn received(&self) -> Vec<IoPacket> {
        self.received.lock().unwrap().clone()
    }

    /// Forward_Open: check class 1 and sizes, start producing, answer with our UDP port.
    async fn forward_open(
        self: &Arc<Self>,
        data: &[u8],
        extra: &[Item],
        peer: SocketAddr,
    ) -> (MessageResponse, Vec<Item>) {
        let mut r = Reader::new(data, "fo");
        r.take(2).unwrap(); // tick, ticks
        r.u32().unwrap(); // O→T id: ours to choose
        let t_o_id = r.u32().unwrap();
        let triad = r.take(8).unwrap().to_vec();
        r.take(4).unwrap(); // multiplier + reserved
        let o_t_rpi = r.u32().unwrap();
        let o_t_params = r.u16().unwrap();
        let t_o_rpi = r.u32().unwrap();
        let t_o_params = r.u16().unwrap();
        let transport = r.u8().unwrap();
        assert_eq!(transport, 0x01, "class 1 cyclic");
        self.opens.fetch_add(1, Ordering::SeqCst);
        // like a real adapter: the sizes must match the assemblies
        if o_t_params & 0x1FF != ASSEMBLY + 6 || t_o_params & 0x1FF != ASSEMBLY + 2 {
            return (
                MessageResponse {
                    service: service::FORWARD_OPEN | 0x80,
                    status: CipStatus {
                        general: 0x01,
                        extended: vec![0x0109],
                    },
                    data: vec![],
                },
                vec![],
            );
        }
        let port = extra
            .iter()
            .find_map(|i| match i {
                Item::SockaddrTO(a) => Some(a.port()),
                _ => None,
            })
            .unwrap_or(2222);
        self.requested_port.store(port, Ordering::SeqCst);
        let udp = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let udp_port = udp.local_addr().unwrap().port();
        let dest = SocketAddr::new(peer.ip(), port);
        tokio::spawn(self.clone().run_udp(
            udp,
            dest,
            t_o_id,
            Duration::from_micros(t_o_rpi.into()),
        ));

        let mut reply = O_T_ID.to_le_bytes().to_vec();
        reply.extend_from_slice(&t_o_id.to_le_bytes());
        reply.extend_from_slice(&triad);
        reply.extend_from_slice(&o_t_rpi.to_le_bytes());
        reply.extend_from_slice(&t_o_rpi.to_le_bytes());
        reply.extend_from_slice(&[0, 0]);
        (
            MessageResponse::success(service::FORWARD_OPEN, reply),
            vec![Item::SockaddrOT(SocketAddrV4::new(
                Ipv4Addr::LOCALHOST,
                udp_port,
            ))],
        )
    }

    async fn run_udp(
        self: Arc<Self>,
        udp: UdpSocket,
        dest: SocketAddr,
        t_o_id: u32,
        rpi: Duration,
    ) {
        let mut tick = tokio::time::interval(rpi);
        let mut seq = SequenceCount(0);
        let mut encap = 0u32;
        let mut buf = [0u8; 1500];
        loop {
            tokio::select! {
                _ = tick.tick() => {
                    if self.silent.load(Ordering::SeqCst) || self.closes.load(Ordering::SeqCst) > 0 {
                        continue;
                    }
                    seq = seq.next();
                    encap = encap.wrapping_add(1);
                    let p = IoPacket {
                        connection_id: t_o_id,
                        encap_sequence: encap,
                        sequence: seq,
                        run_idle: None,
                        data: self.inputs.lock().unwrap().clone(),
                    };
                    let _ = udp.send_to(&p.encode().unwrap(), dest).await;
                }
                r = udp.recv_from(&mut buf) => {
                    let Ok((n, _)) = r else { continue };
                    let p = IoPacket::decode(&buf[..n], true).unwrap();
                    assert_eq!(p.connection_id, O_T_ID);
                    self.received.lock().unwrap().push(p);
                }
            }
        }
    }

    async fn serve(self: Arc<Self>, mut s: TcpStream) {
        let peer = s.peer_addr().unwrap();
        loop {
            let mut head = [0u8; HEADER_LEN];
            if s.read_exact(&mut head).await.is_err() {
                return;
            }
            let h = Header::decode(&head).unwrap();
            let mut data = vec![0; usize::from(h.length)];
            if s.read_exact(&mut data).await.is_err() {
                return;
            }
            let reply = match h.command {
                command::REGISTER_SESSION => data,
                command::UNREGISTER_SESSION => return,
                command::SEND_RR_DATA => {
                    let items = cpf::decode(encap::parse_send_data(&data).unwrap()).unwrap();
                    let Item::UnconnectedData(mr) = &items[1] else {
                        panic!("{items:?}")
                    };
                    let (svc, rest) = (mr[0], &mr[2 + usize::from(mr[1]) * 2..]);
                    let (resp, extra) = match svc {
                        service::FORWARD_OPEN => self.forward_open(rest, &items[2..], peer).await,
                        service::FORWARD_CLOSE => {
                            self.closes.fetch_add(1, Ordering::SeqCst);
                            (MessageResponse::success(svc, vec![0; 10]), vec![])
                        }
                        other => panic!("unexpected service {other:#x}"),
                    };
                    let mut out = vec![Item::NullAddress, Item::UnconnectedData(resp.encode())];
                    out.extend(extra);
                    encap::send_data(0, &cpf::encode(&out).unwrap())
                }
                other => panic!("unexpected command {other:#x}"),
            };
            let mut f = Frame::request(h.command, 0x5E55, h.context, reply);
            f.header.session = 0x5E55;
            s.write_all(&f.encode().unwrap()).await.unwrap();
        }
    }
}

async fn spawn() -> (Arc<Adapter>, SocketAddr) {
    let adapter = Adapter::new();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let a = adapter.clone();
    tokio::spawn(async move {
        while let Ok((s, _)) = listener.accept().await {
            tokio::spawn(a.clone().serve(s));
        }
    });
    (adapter, addr)
}

fn params() -> ConnectionParams {
    ConnectionParams::implicit_class1(1, 112, 111, ASSEMBLY, ASSEMBLY, RPI)
}

/// Ephemeral local port, so tests do not need 2222.
fn options() -> ImplicitOptions {
    ImplicitOptions {
        local_port: 0,
        ..ImplicitOptions::default()
    }
}

/// Poll `f` every 5 ms for up to 2 s.
async fn eventually(what: &str, mut f: impl FnMut() -> bool) {
    for _ in 0..400 {
        if f() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("timed out waiting for {what}");
}

#[tokio::test]
async fn exchange_inputs_and_outputs_then_close() {
    let (adapter, addr) = spawn().await;
    let mut client = Client::connect(addr).await.unwrap();
    let io = ImplicitConnection::open(&mut client, params(), options())
        .await
        .unwrap();
    assert_eq!((io.output_len(), io.input_len()), (4, 4));
    assert_eq!(io.reply().o_t_connection_id, O_T_ID);
    assert_eq!(
        adapter.requested_port.load(Ordering::SeqCst),
        io.local_addr().port(),
        "sockaddr T→O item names our port"
    );
    assert_eq!(io.o_t_addr().ip(), addr.ip());
    // 16 × RPI with the default multiplier 2
    assert_eq!(io.timeout(), RPI * 16);

    // inputs arrive
    let mut inputs = io.inputs();
    inputs.changed().await.unwrap();
    assert_eq!(io.current_state(), ImplicitState::Running);
    assert_eq!(io.latest_input().unwrap().data, [0, 0, 0, 0]);
    *adapter.inputs.lock().unwrap() = vec![0, 0, 0, 1]; // DI ch 8
    eventually("new input", || {
        io.latest_input().is_some_and(|f| f.data == [0, 0, 0, 1])
    })
    .await;

    // outputs start as zeros in idle mode
    eventually("first O→T", || !adapter.received().is_empty()).await;
    let first = adapter.received()[0].clone();
    assert_eq!(first.run_idle, Some(RunIdleHeader::IDLE));
    assert_eq!(first.data, [0, 0, 0, 0]);

    io.set_outputs(&[0, 0, 0, 1]).unwrap();
    io.set_run(true);
    assert_eq!(io.outputs(), (vec![0, 0, 0, 1], true));
    eventually("run + outputs", || {
        adapter
            .received()
            .last()
            .is_some_and(|p| p.run_idle == Some(RunIdleHeader::RUN) && p.data == [0, 0, 0, 1])
    })
    .await;

    // sequence numbers advance with every packet
    let got = adapter.received();
    for pair in got.windows(2) {
        assert_eq!(
            pair[1].encap_sequence,
            pair[0].encap_sequence.wrapping_add(1)
        );
        assert!(pair[0].sequence.is_newer(pair[1].sequence));
    }
    let c = io.counters();
    assert!(c.sent >= 2 && c.received >= 2, "{c:?}");
    assert_eq!(c.rejected, 0);

    io.close(&mut client).await.unwrap();
    assert_eq!(adapter.closes.load(Ordering::SeqCst), 1);
    let n = adapter.received().len();
    tokio::time::sleep(RPI * 5).await;
    assert_eq!(adapter.received().len(), n, "no O→T after close");
    client.close().await.unwrap();
}

#[tokio::test]
async fn silence_times_the_connection_out() {
    let (adapter, addr) = spawn().await;
    let mut client = Client::connect(addr).await.unwrap();
    let io = ImplicitConnection::open(&mut client, params(), options())
        .await
        .unwrap();
    let mut state = io.state();
    state
        .wait_for(|s| *s == ImplicitState::Running)
        .await
        .unwrap();
    adapter.silent.store(true, Ordering::SeqCst);
    let started = tokio::time::Instant::now();
    state
        .wait_for(|s| *s == ImplicitState::TimedOut)
        .await
        .unwrap();
    let took = started.elapsed();
    assert!(took >= io.timeout() - RPI * 2, "too early: {took:?}");
    assert!(io.current_state().is_finished());
    // the task has ended: O→T stops (after packets already in flight)
    tokio::time::sleep(RPI * 2).await;
    let n = adapter.received().len();
    tokio::time::sleep(RPI * 5).await;
    assert_eq!(adapter.received().len(), n);
}

#[tokio::test]
async fn no_data_at_all_times_out_too() {
    let (adapter, addr) = spawn().await;
    adapter.silent.store(true, Ordering::SeqCst);
    let mut client = Client::connect(addr).await.unwrap();
    let io = ImplicitConnection::open(&mut client, params(), options())
        .await
        .unwrap();
    assert_eq!(io.current_state(), ImplicitState::WaitingForData);
    io.state()
        .wait_for(|s| *s == ImplicitState::TimedOut)
        .await
        .unwrap();
    assert!(io.latest_input().is_none());
}

#[tokio::test]
async fn foreign_wrong_and_stale_packets_are_dropped() {
    let (adapter, addr) = spawn().await;
    let mut client = Client::connect(addr).await.unwrap();
    let io = ImplicitConnection::open(&mut client, params(), options())
        .await
        .unwrap();
    io.state()
        .wait_for(|s| *s == ImplicitState::Running)
        .await
        .unwrap();
    adapter.silent.store(true, Ordering::SeqCst);
    tokio::time::sleep(RPI * 3).await; // let in-flight packets land
    let last = io.latest_input().unwrap();
    let before = io.counters();

    let t_o_id = io.params().t_o_connection_id;
    let packet = |id, seq: u16, data: Vec<u8>| {
        IoPacket {
            connection_id: id,
            encap_sequence: 1,
            sequence: SequenceCount(seq),
            run_idle: None,
            data,
        }
        .encode()
        .unwrap()
    };
    let to = SocketAddr::new(Ipv4Addr::LOCALHOST.into(), io.local_addr().port());
    let sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let newer = last.sequence.0.wrapping_add(100);
    sock.send_to(&packet(t_o_id ^ 1, newer, vec![9; 4]), to)
        .await
        .unwrap(); // other connection
    sock.send_to(&packet(t_o_id, newer, vec![9; 3]), to)
        .await
        .unwrap(); // wrong size
    sock.send_to(b"not a CPF packet", to).await.unwrap();
    sock.send_to(&packet(t_o_id, last.sequence.0, vec![9; 4]), to)
        .await
        .unwrap(); // duplicate
    sock.send_to(
        &packet(t_o_id, last.sequence.0.wrapping_sub(1), vec![9; 4]),
        to,
    )
    .await
    .unwrap(); // older
    eventually("counted", || {
        let c = io.counters();
        c.rejected == before.rejected + 3 && c.stale == before.stale + 2
    })
    .await;
    assert_eq!(
        io.latest_input().unwrap().data,
        last.data,
        "nothing accepted"
    );

    sock.send_to(&packet(t_o_id, newer, vec![7; 4]), to)
        .await
        .unwrap();
    eventually("newer accepted", || {
        io.latest_input().is_some_and(|f| f.data == [7; 4])
    })
    .await;
}

#[tokio::test]
async fn bad_outputs_and_params_are_refused() {
    let (adapter, addr) = spawn().await;
    let mut client = Client::connect(addr).await.unwrap();

    // not class 1
    let e = ImplicitConnection::open(&mut client, ConnectionParams::explicit_class3(), options())
        .await
        .unwrap_err();
    assert!(matches!(e, Error::InvalidParams(_)), "{e}");
    // too small for the headers
    let mut tiny = params();
    tiny.o_t.size = 5;
    assert!(matches!(
        ImplicitConnection::open(&mut client, tiny, options()).await,
        Err(Error::InvalidParams(_))
    ));
    assert_eq!(adapter.opens.load(Ordering::SeqCst), 0, "refused locally");

    // sizes the adapter does not accept: its own rejection, 0x01/0x0109
    let wrong = ConnectionParams::implicit_class1(1, 112, 111, 6, 4, RPI);
    let e = ImplicitConnection::open(&mut client, wrong, options())
        .await
        .unwrap_err();
    let status = e.cip_status().expect("CIP status");
    assert_eq!(
        (status.general, status.extended.as_slice()),
        (0x01, &[0x0109][..])
    );

    let io = ImplicitConnection::open(&mut client, params(), options())
        .await
        .unwrap();
    assert!(io.set_outputs(&[0; 3]).is_err());
    assert!(io.set_outputs(&[0; 5]).is_err());
    assert_eq!(io.outputs().0, [0; 4], "unchanged");
}

#[tokio::test]
async fn dropping_the_handle_stops_o_t() {
    let (adapter, addr) = spawn().await;
    let mut client = Client::connect(addr).await.unwrap();
    let io = ImplicitConnection::open(&mut client, params(), options())
        .await
        .unwrap();
    eventually("O→T flowing", || adapter.received().len() >= 3).await;
    drop(io);
    tokio::time::sleep(RPI * 2).await;
    let n = adapter.received().len();
    tokio::time::sleep(RPI * 5).await;
    assert_eq!(adapter.received().len(), n);
    assert_eq!(
        adapter.closes.load(Ordering::SeqCst),
        0,
        "drop cannot Forward_Close"
    );
}

#[tokio::test]
async fn start_in_run_mode() {
    let (adapter, addr) = spawn().await;
    let mut client = Client::connect(addr).await.unwrap();
    let opts = ImplicitOptions {
        run: true,
        ..options()
    };
    let _io = ImplicitConnection::open(&mut client, params(), opts)
        .await
        .unwrap();
    eventually("first O→T", || !adapter.received().is_empty()).await;
    assert_eq!(adapter.received()[0].run_idle, Some(RunIdleHeader::RUN));
}
