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
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use ur20_fbc_eip::cip::codec::Reader;
use ur20_fbc_eip::cip::cpf::{self, Item};
use ur20_fbc_eip::cip::encap::{self, Frame, HEADER_LEN, Header, command};
use ur20_fbc_eip::cip::message::{CipStatus, MessageResponse, service};
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
                    let resp = self.handle(mr).encode();
                    let packet =
                        cpf::encode(&[Item::NullAddress, Item::UnconnectedData(resp)]).unwrap();
                    encap::send_data(0, &packet)
                }
                other => panic!("unexpected command {other:#x}"),
            };
            let mut f = Frame::request(h.command, 0x1234, h.context, reply);
            f.header.session = 0x1234;
            s.write_all(&f.encode().unwrap()).await.unwrap();
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
