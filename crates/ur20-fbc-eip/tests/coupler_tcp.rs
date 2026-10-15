// Copyright (C), 2026 Quartz Systems. Some rights reserved. This work is
// licensed under the terms of the MIT license which can be found in the
// root directory of this project.

//! `Coupler<Client>` over real TCP against an in-process adapter that serves the bench
//! station's attributes and can hang up mid-request: broken client, reconnect, drop.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use ur20_fbc_eip::cip::codec::Reader;
use ur20_fbc_eip::cip::cpf::{self, Item};
use ur20_fbc_eip::cip::encap::{self, Frame, HEADER_LEN, Header, command};
use ur20_fbc_eip::cip::io::{IoPacket, RunIdleHeader, SequenceCount};
use ur20_fbc_eip::cip::message::{CipStatus, MessageResponse, service};
use ur20_fbc_eip::cip::{ImplicitOptions, ImplicitState};
use ur20_fbc_eip::{ChannelValue, Coupler, CouplerLayout, Error};

type Key = (u8, u8, u8);

#[derive(Default)]
struct Adapter {
    attrs: Mutex<HashMap<Key, Vec<u8>>>,
    /// Encapsulation commands and (service, path) of every request, all connections.
    commands: Mutex<Vec<u16>>,
    requests: Mutex<Vec<(u8, Key)>>,
    /// Read the next SendRRData, then close the socket without replying.
    kill_next: AtomicBool,
    /// Class 1: O→T packets received, T→O production switched off, Forward_Open/Close count.
    o_t: Mutex<Vec<IoPacket>>,
    silent: AtomicBool,
    opens: AtomicUsize,
    closes: AtomicUsize,
}

impl Adapter {
    /// The bench station: UR20-16DI-P + UR20-16DO-P, all signals off (2026-10-13).
    fn bench() -> Arc<Self> {
        let a = Self::default();
        let mut m = a.attrs.lock().unwrap();
        let mut list = vec![0xc2, 0x9f, 0x04, 0x00, 0xd0, 0xaf, 0x03, 0x01];
        list.resize(256, 0);
        m.insert((0x64, 1, 111), list);
        m.insert((0x04, 111, 3), vec![0; 4]);
        m.insert((0x04, 112, 3), vec![0; 4]);
        m.insert((0x66, 1, 107), vec![0, 0]);
        m.insert((0x66, 1, 108), vec![0]);
        drop(m);
        Arc::new(a)
    }

    fn count(&self, req: (u8, Key)) -> usize {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .filter(|r| **r == req)
            .count()
    }

    fn commands(&self, cmd: u16) -> usize {
        self.commands
            .lock()
            .unwrap()
            .iter()
            .filter(|c| **c == cmd)
            .count()
    }

    fn handle(&self, mr: &[u8]) -> MessageResponse {
        let mut r = Reader::new(mr, "request");
        let svc = r.u8().unwrap();
        let _words = r.u8().unwrap();
        let p = r.take(6).unwrap();
        let key = (p[1], p[3], p[5]);
        let data = r.rest().to_vec();
        self.requests.lock().unwrap().push((svc, key));
        let mut attrs = self.attrs.lock().unwrap();
        let err = |general| MessageResponse {
            service: svc | 0x80,
            status: CipStatus {
                general,
                extended: vec![],
            },
            data: vec![],
        };
        match (svc, attrs.get_mut(&key)) {
            (service::GET_ATTRIBUTE_SINGLE, Some(v)) => MessageResponse::success(svc, v.clone()),
            (service::SET_ATTRIBUTE_SINGLE, Some(v)) => {
                *v = data;
                MessageResponse::success(svc, vec![])
            }
            _ => err(0x14),
        }
    }

    async fn serve(self: Arc<Self>, mut s: TcpStream) {
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
            self.commands.lock().unwrap().push(h.command);
            let reply = match h.command {
                command::REGISTER_SESSION => data,
                command::UNREGISTER_SESSION => return,
                command::SEND_RR_DATA => {
                    if self.kill_next.swap(false, Ordering::SeqCst) {
                        return;
                    }
                    let items = cpf::decode(encap::parse_send_data(&data).unwrap()).unwrap();
                    let Item::UnconnectedData(mr) = &items[1] else {
                        panic!("{items:?}")
                    };
                    let (resp, extra) = match mr[0] {
                        service::FORWARD_OPEN => {
                            let rest = &mr[2 + usize::from(mr[1]) * 2..];
                            let peer = s.peer_addr().unwrap();
                            self.forward_open(rest, &items[2..], peer).await
                        }
                        service::FORWARD_CLOSE => {
                            self.closes.fetch_add(1, Ordering::SeqCst);
                            (MessageResponse::success(mr[0], vec![0; 10]), vec![])
                        }
                        _ => (self.handle(mr), vec![]),
                    };
                    let mut out = vec![Item::NullAddress, Item::UnconnectedData(resp.encode())];
                    out.extend(extra);
                    encap::send_data(0, &cpf::encode(&out).unwrap())
                }
                other => panic!("unexpected command {other:#x}"),
            };
            let mut f = Frame::request(h.command, 0x1234, h.context, reply);
            f.header.session = 0x1234;
            s.write_all(&f.encode().unwrap()).await.unwrap();
        }
    }
}

impl Adapter {
    fn attr(&self, key: Key) -> Vec<u8> {
        self.attrs.lock().unwrap()[&key].clone()
    }

    fn o_t(&self) -> Vec<IoPacket> {
        self.o_t.lock().unwrap().clone()
    }

    /// Class 1 Forward_Open: sizes must match the assemblies (+2 sequence, +4 run/idle O→T),
    /// as a real adapter checks; then produce 111 at the RPI and record O→T.
    async fn forward_open(
        self: &Arc<Self>,
        data: &[u8],
        extra: &[Item],
        peer: SocketAddr,
    ) -> (MessageResponse, Vec<Item>) {
        let mut r = Reader::new(data, "fo");
        r.take(6).unwrap(); // ticks, O→T id
        let t_o_id = r.u32().unwrap();
        let triad = r.take(8).unwrap().to_vec();
        r.take(4).unwrap();
        let o_t_rpi = r.u32().unwrap();
        let o_t_size = usize::from(r.u16().unwrap() & 0x1FF);
        let t_o_rpi = r.u32().unwrap();
        let t_o_size = usize::from(r.u16().unwrap() & 0x1FF);
        assert_eq!(r.u8().unwrap(), 0x01, "class 1");
        self.opens.fetch_add(1, Ordering::SeqCst);
        if o_t_size != self.attr((0x04, 112, 3)).len() + 6
            || t_o_size != self.attr((0x04, 111, 3)).len() + 2
        {
            let refuse = MessageResponse {
                service: service::FORWARD_OPEN | 0x80,
                status: CipStatus {
                    general: 0x01,
                    extended: vec![0x0109],
                },
                data: vec![],
            };
            return (refuse, vec![]);
        }
        let port = extra
            .iter()
            .find_map(|i| match i {
                Item::SockaddrTO(a) => Some(a.port()),
                _ => None,
            })
            .unwrap_or(2222);
        let udp = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let udp_port = udp.local_addr().unwrap().port();
        let dest = SocketAddr::new(peer.ip(), port);
        let rpi = Duration::from_micros(t_o_rpi.into());
        tokio::spawn(self.clone().run_udp(udp, dest, t_o_id, rpi));
        let mut reply = 0x2000_0002u32.to_le_bytes().to_vec();
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
        let (mut seq, mut encap) = (SequenceCount(0), 0u32);
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
                        data: self.attr((0x04, 111, 3)),
                    };
                    let _ = udp.send_to(&p.encode().unwrap(), dest).await;
                }
                r = udp.recv_from(&mut buf) => {
                    let Ok((n, _)) = r else { continue };
                    self.o_t.lock().unwrap().push(IoPacket::decode(&buf[..n], true).unwrap());
                }
            }
        }
    }
}

async fn spawn(adapter: Arc<Adapter>) -> std::net::SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((s, _)) = listener.accept().await {
            tokio::spawn(adapter.clone().serve(s));
        }
    });
    addr
}

const START: (u8, Key) = (service::SET_ATTRIBUTE_SINGLE, (0x66, 1, 108));

#[tokio::test]
async fn socket_closed_mid_request_then_coupler_reconnects() {
    let adapter = Adapter::bench();
    let addr = spawn(adapter.clone()).await;
    let mut c = Coupler::connect_auto(addr, CouplerLayout::unpacked())
        .await
        .unwrap();
    c.enable_explicit_outputs(Duration::from_secs(5))
        .await
        .unwrap();
    c.set_output(2, 8, ChannelValue::Bit(true)).await.unwrap();
    assert_eq!(adapter.count(START), 1);

    adapter.kill_next.store(true, Ordering::SeqCst);
    let e = c.read_inputs().await.unwrap_err();
    assert!(matches!(&e, Error::Cip(_)), "{e:?}");
    assert!(c.is_broken());
    let e = c.read_inputs().await.unwrap_err();
    assert!(e.to_string().contains("reconnect"), "{e}");

    let layout = c.layout().clone();
    c.reconnect().await.unwrap();
    assert!(!c.is_broken());
    assert_eq!(c.layout(), &layout);
    let names: Vec<_> = c
        .station()
        .slots()
        .iter()
        .map(|s| s.module.name())
        .collect();
    assert_eq!(names, ["UR20-16DI-P", "UR20-16DO-P"]);
    assert_eq!(adapter.commands(command::REGISTER_SESSION), 2);
    assert_eq!(adapter.count(START), 2, "write access requested again");
    assert_eq!(c.read_inputs().await.unwrap().status_word, Some(0));
    assert_eq!(
        c.read_outputs().await.unwrap().slots[0].values[8],
        ChannelValue::Bit(true)
    );
    c.set_output(2, 8, ChannelValue::Bit(false)).await.unwrap();
    c.close().await.unwrap();
    assert_eq!(
        adapter.commands(command::UNREGISTER_SESSION),
        0,
        "dead one skipped"
    );
}

#[tokio::test]
async fn reconnect_without_write_access_does_not_request_it() {
    let adapter = Adapter::bench();
    let addr = spawn(adapter.clone()).await;
    let mut c = Coupler::connect_auto(addr, CouplerLayout::unpacked())
        .await
        .unwrap();
    adapter.kill_next.store(true, Ordering::SeqCst);
    assert!(c.read_inputs().await.is_err());
    c.reconnect().await.unwrap();
    assert_eq!(adapter.count(START), 0);
    c.read_inputs().await.unwrap();
}

#[tokio::test]
async fn dropped_coupler_unregisters() {
    let adapter = Adapter::bench();
    let addr = spawn(adapter.clone()).await;
    let mut c = Coupler::connect_auto(addr, CouplerLayout::unpacked())
        .await
        .unwrap();
    c.read_inputs().await.unwrap();
    drop(c);
    for _ in 0..200 {
        if adapter.commands(command::UNREGISTER_SESSION) == 1 {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("no UnRegisterSession after drop");
}

const RPI: Duration = Duration::from_millis(10);

fn class1_options() -> ImplicitOptions {
    ImplicitOptions {
        local_port: 0,
        ..ImplicitOptions::default()
    }
}

async fn class1_bench() -> (Arc<Adapter>, Coupler) {
    let adapter = Adapter::bench();
    let addr = spawn(adapter.clone()).await;
    let mut c = Coupler::connect_auto(addr, CouplerLayout::unpacked())
        .await
        .unwrap();
    c.open_implicit(RPI, class1_options()).await.unwrap();
    (adapter, c)
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
async fn class1_inputs_are_decoded_with_the_station() {
    let (adapter, mut c) = class1_bench().await;
    let io = c.implicit().unwrap();
    assert_eq!((io.output_len(), io.input_len()), (4, 4));
    let img = c.next_implicit_inputs().await.unwrap();
    assert_eq!(img.status_word, Some(0));
    assert_eq!(img.bit(1, 8), Some(false));

    // DI ch 8 on and the status word showing a pulled module, as read on hardware
    adapter
        .attrs
        .lock()
        .unwrap()
        .insert((0x04, 111, 3), vec![0x40, 0x00, 0x00, 0x01]);
    let img = loop {
        let img = c.next_implicit_inputs().await.unwrap();
        if img.bit(1, 8) == Some(true) {
            break img;
        }
    };
    assert!(img.status().unwrap().module_removed());
    assert_eq!(c.implicit_inputs().unwrap().unwrap().bit(1, 8), Some(true));
}

#[tokio::test]
async fn class1_outputs_go_into_o_t_not_explicit_writes() {
    let (adapter, mut c) = class1_bench().await;
    let explicit_writes = || adapter.count((service::SET_ATTRIBUTE_SINGLE, (0x04, 112, 3)));
    let before = adapter.requests.lock().unwrap().len();

    c.set_output(2, 8, ChannelValue::Bit(true)).await.unwrap();
    c.set_implicit_run(true).unwrap();
    eventually("DO ch 8 in a run packet", || {
        adapter
            .o_t()
            .last()
            .is_some_and(|p| p.run_idle == Some(RunIdleHeader::RUN) && p.data == [0, 0, 0x00, 0x01])
    })
    .await;

    c.set_outputs(&[
        (2, 0, ChannelValue::Bit(true)),
        (2, 8, ChannelValue::Bit(false)),
    ])
    .await
    .unwrap();
    assert_eq!(c.implicit().unwrap().outputs().0, [0, 0, 0x01, 0x00]);
    let mut pattern = vec![ChannelValue::Bit(false); 16];
    pattern[15] = ChannelValue::Bit(true);
    c.write_slot_outputs(2, &pattern).await.unwrap();
    assert_eq!(c.implicit().unwrap().outputs().0, [0, 0, 0x00, 0x80]);

    // the image path reads and writes the O→T buffer
    let mut img = c.read_outputs().await.unwrap();
    assert_eq!(img.value(2, 15), Some(&ChannelValue::Bit(true)));
    img.set(2, 1, ChannelValue::Bit(true)).unwrap();
    c.write_outputs(&img).await.unwrap();
    eventually("image in O→T", || {
        adapter
            .o_t()
            .last()
            .is_some_and(|p| p.data == [0, 0, 0x02, 0x80])
    })
    .await;

    // all of that without a single explicit request
    assert_eq!(adapter.requests.lock().unwrap().len(), before);
    assert_eq!(explicit_writes(), 0);
    // invalid changes are still refused before anything changes
    assert!(c.set_output(2, 16, ChannelValue::Bit(true)).await.is_err());
    assert!(c.set_output(1, 0, ChannelValue::Bit(true)).await.is_err());
    assert_eq!(c.implicit().unwrap().outputs().0, [0, 0, 0x02, 0x80]);
}

#[tokio::test]
async fn class1_refuses_explicit_output_paths() {
    let (adapter, mut c) = class1_bench().await;
    assert!(matches!(
        c.write_output_assembly(&[0, 0, 0, 1]).await,
        Err(Error::ImplicitOpen(_))
    ));
    assert!(matches!(
        c.enable_explicit_outputs(Duration::from_secs(2)).await,
        Err(Error::ImplicitOpen(_))
    ));
    assert!(matches!(
        c.open_implicit(RPI, class1_options()).await,
        Err(Error::ImplicitOpen(_))
    ));
    assert_eq!(adapter.opens.load(Ordering::SeqCst), 1);
    // explicit reads still work
    assert_eq!(c.read_inputs().await.unwrap().status_word, Some(0));
}

#[tokio::test]
async fn class1_close_returns_outputs_to_explicit() {
    let (adapter, mut c) = class1_bench().await;
    c.close_implicit().await.unwrap();
    assert_eq!(adapter.closes.load(Ordering::SeqCst), 1);
    assert!(c.implicit().is_none());
    assert!(matches!(
        c.next_implicit_inputs().await,
        Err(Error::ImplicitNotRunning(_))
    ));
    c.close_implicit().await.unwrap(); // nothing open: fine
    c.enable_explicit_outputs(Duration::from_secs(2))
        .await
        .unwrap();
    c.set_output(2, 8, ChannelValue::Bit(true)).await.unwrap();
    assert_eq!(adapter.attr((0x04, 112, 3)), [0, 0, 0, 1]);
}

#[tokio::test]
async fn class1_timeout_ends_next_implicit_inputs() {
    let (adapter, mut c) = class1_bench().await;
    c.next_implicit_inputs().await.unwrap();
    adapter.silent.store(true, Ordering::SeqCst);
    let e = loop {
        match c.next_implicit_inputs().await {
            Ok(_) => continue, // frames already in flight
            Err(e) => break e,
        }
    };
    assert!(
        matches!(&e, Error::ImplicitNotRunning(s) if s.contains("TimedOut")),
        "{e}"
    );
    assert_eq!(
        c.implicit().unwrap().current_state(),
        ImplicitState::TimedOut
    );
    assert!(c.set_implicit_run(true).is_err());
    assert!(c.implicit_inputs().is_err());
    // closing a timed-out connection still sends Forward_Close
    c.close_implicit().await.unwrap();
}

#[tokio::test]
async fn class1_station_mismatch_fails_before_forward_open() {
    let adapter = Adapter::bench();
    let addr = spawn(adapter.clone()).await;
    // three modules configured, the coupler's assemblies only hold two
    let modules =
        ur20_fbc_eip::modules::create_all(&["UR20-16DI-P", "UR20-16DO-P", "UR20-16DO-P"]).unwrap();
    let mut c = Coupler::connect(addr, CouplerLayout::unpacked(), modules)
        .await
        .unwrap();
    let e = c.open_implicit(RPI, class1_options()).await.unwrap_err();
    assert!(
        matches!(e, Error::StationMismatch { position: 0, .. }),
        "{e}"
    );
    assert_eq!(adapter.opens.load(Ordering::SeqCst), 0);
    assert!(c.implicit().is_none());
}

#[tokio::test]
async fn class1_reconnect_drops_the_connection() {
    let (adapter, mut c) = class1_bench().await;
    adapter.kill_next.store(true, Ordering::SeqCst);
    assert!(c.read_inputs().await.is_err());
    c.reconnect().await.unwrap();
    assert!(c.implicit().is_none());
    c.open_implicit(RPI, class1_options()).await.unwrap();
    c.next_implicit_inputs().await.unwrap();
    assert_eq!(adapter.opens.load(Ordering::SeqCst), 2);
    c.close().await.unwrap();
    assert_eq!(
        adapter.closes.load(Ordering::SeqCst),
        1,
        "close() closes class 1 too"
    );
}
