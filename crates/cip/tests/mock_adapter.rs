// Copyright (C), 2026 Quartz Systems. Some rights reserved. This work is
// licensed under the terms of the MIT license which can be found in the
// root directory of this project.

//! End-to-end client tests against a minimal in-process EtherNet/IP adapter.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use ur20_fbc_eip_cip::codec::Reader;
use ur20_fbc_eip_cip::connection::ConnectionParams;
use ur20_fbc_eip_cip::cpf::{self, Item};
use ur20_fbc_eip_cip::encap::{self, Frame, HEADER_LEN, Header, command};
use ur20_fbc_eip_cip::message::{CipStatus, MessageResponse, service};
use ur20_fbc_eip_cip::{AttributePath, CipService, Client, Error};

const SESSION: u32 = 0xC0FF_EE01;
const O_T_ID: u32 = 0x0BAD_F00D;

/// Answer a Message Router request the way a tiny adapter would.
fn handle_mr(req: &[u8]) -> MessageResponse {
    let mut r = Reader::new(req, "test request");
    let svc = r.u8().unwrap();
    let words = r.u8().unwrap();
    let path = r.take(usize::from(words) * 2).unwrap().to_vec();
    let data = r.rest().to_vec();
    match (svc, path.as_slice()) {
        // Identity, Get_Attributes_All
        (service::GET_ATTRIBUTES_ALL, [0x20, 0x01, 0x24, 0x01]) => MessageResponse::success(
            svc,
            vec![
                0x99, 0x01, 0x0c, 0x00, 0x2a, 0x00, 0x02, 0x09, 0x60, 0x00, 0x78, 0x56, 0x34, 0x12,
                4, b'M', b'O', b'C', b'K',
            ],
        ),
        (service::GET_ATTRIBUTE_SINGLE, [0x20, 0x04, 0x24, 0x6f, 0x30, 0x03]) => {
            MessageResponse::success(svc, vec![0x00, 0x00, 0x05])
        }
        (service::SET_ATTRIBUTE_SINGLE, [0x20, 0x66, 0x24, 0x01, 0x30, 0x6c]) => {
            assert_eq!(data, [1]);
            MessageResponse::success(svc, vec![])
        }
        (service::FORWARD_OPEN, [0x20, 0x06, 0x24, 0x01]) => {
            let mut r = Reader::new(&data, "fo");
            r.take(6).unwrap();
            let t_o = r.u32().unwrap();
            let triad = r.take(8).unwrap().to_vec();
            let mut reply = O_T_ID.to_le_bytes().to_vec();
            reply.extend_from_slice(&t_o.to_le_bytes());
            reply.extend_from_slice(&triad);
            reply.extend_from_slice(&[0x40, 0x42, 0x0f, 0x00, 0x40, 0x42, 0x0f, 0x00, 0, 0]);
            MessageResponse::success(svc, reply)
        }
        (service::FORWARD_CLOSE, _) => MessageResponse::success(svc, vec![0; 10]),
        _ => MessageResponse {
            service: svc | 0x80,
            status: CipStatus {
                general: 0x16,
                extended: vec![],
            },
            data: vec![],
        },
    }
}

/// What the adapter saw, and a switch to make it hang up.
#[derive(Default)]
struct AdapterState {
    /// Encapsulation commands received, all connections.
    log: Mutex<Vec<u16>>,
    /// Read the next SendRRData/SendUnitData, then close the socket without replying.
    kill_next: AtomicBool,
}

impl AdapterState {
    fn count(&self, cmd: u16) -> usize {
        self.log
            .lock()
            .unwrap()
            .iter()
            .filter(|c| **c == cmd)
            .count()
    }

    /// Wait (up to 2 s) until `cmd` has been received `n` times.
    async fn wait_for(&self, cmd: u16, n: usize) -> bool {
        for _ in 0..200 {
            if self.count(cmd) >= n {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        false
    }
}

async fn serve(mut s: TcpStream, state: Arc<AdapterState>) {
    let mut t_o_id = 0;
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
        state.log.lock().unwrap().push(h.command);
        if matches!(h.command, command::SEND_RR_DATA | command::SEND_UNIT_DATA)
            && state.kill_next.swap(false, Ordering::SeqCst)
        {
            return; // hang up mid-request
        }
        let reply_data = match h.command {
            command::REGISTER_SESSION => data,
            command::UNREGISTER_SESSION => return,
            command::SEND_RR_DATA => {
                let items = cpf::decode(encap::parse_send_data(&data).unwrap()).unwrap();
                let Item::UnconnectedData(mr) = &items[1] else {
                    panic!()
                };
                let resp = handle_mr(mr);
                if mr[0] == service::FORWARD_OPEN && resp.status.is_success() {
                    t_o_id = u32::from_le_bytes(resp.data[4..8].try_into().unwrap());
                }
                let packet =
                    cpf::encode(&[Item::NullAddress, Item::UnconnectedData(resp.encode())])
                        .unwrap();
                encap::send_data(0, &packet)
            }
            command::SEND_UNIT_DATA => {
                let items = cpf::decode(encap::parse_send_data(&data).unwrap()).unwrap();
                assert_eq!(items[0], Item::ConnectedAddress(O_T_ID));
                let Item::ConnectedData(d) = &items[1] else {
                    panic!()
                };
                let mut out = d[..2].to_vec(); // echo sequence count
                out.extend_from_slice(&handle_mr(&d[2..]).encode());
                let packet =
                    cpf::encode(&[Item::ConnectedAddress(t_o_id), Item::ConnectedData(out)])
                        .unwrap();
                encap::send_data(0, &packet)
            }
            command::LIST_IDENTITY => vec![0, 0],
            _ => panic!("unexpected command {:#x}", h.command),
        };
        let mut f = Frame::request(h.command, SESSION, h.context, reply_data);
        f.header.session = SESSION;
        s.write_all(&f.encode().unwrap()).await.unwrap();
    }
}

async fn spawn_adapter() -> std::net::SocketAddr {
    spawn_adapter_with_state().await.0
}

async fn spawn_adapter_with_state() -> (std::net::SocketAddr, Arc<AdapterState>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let state = Arc::new(AdapterState::default());
    let shared = state.clone();
    tokio::spawn(async move {
        while let Ok((s, _)) = listener.accept().await {
            tokio::spawn(serve(s, shared.clone()));
        }
    });
    (addr, state)
}

#[tokio::test]
async fn unconnected_messaging() {
    let addr = spawn_adapter().await;
    let mut c = Client::connect(addr).await.unwrap();
    assert_eq!(c.session_handle(), SESSION);

    let id = c.identity().await.unwrap();
    assert_eq!(id.product_name, "MOCK");

    let v = c
        .get_attribute_single(AttributePath::new(0x04, 111, 3))
        .await
        .unwrap();
    assert_eq!(v, [0, 0, 5]);

    c.set_attribute_single(AttributePath::new(0x66, 1, 108), &[1])
        .await
        .unwrap();

    let e = c
        .get_attribute_single(AttributePath::new(0x04, 200, 3))
        .await
        .unwrap_err();
    assert_eq!(e.cip_status().map(|s| s.general), Some(0x16));

    assert!(c.list_identity().await.unwrap().is_empty());
    c.close().await.unwrap();
}

#[tokio::test]
async fn connected_messaging() {
    let addr = spawn_adapter().await;
    let mut c = Client::connect(addr).await.unwrap();
    let reply = c
        .open_connected(ConnectionParams::explicit_class3())
        .await
        .unwrap();
    assert_eq!(reply.o_t_connection_id, O_T_ID);
    for _ in 0..3 {
        let v = c
            .get_attribute_single(AttributePath::new(0x04, 111, 3))
            .await
            .unwrap();
        assert_eq!(v, [0, 0, 5]);
    }
    c.close_connected().await.unwrap();
    assert!(matches!(
        c.send_connected(&ur20_fbc_eip_cip::MessageRequest::new(
            1,
            ur20_fbc_eip_cip::EPath::new(),
            vec![]
        ))
        .await,
        Err(Error::NotConnected)
    ));
    c.close().await.unwrap();
}

#[tokio::test]
async fn timeout_marks_client_broken() {
    // A server that accepts the session and then never answers.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut s, _) = listener.accept().await.unwrap();
        let mut buf = [0u8; 28];
        s.read_exact(&mut buf).await.unwrap();
        let h = Header::decode(&buf).unwrap();
        let mut f = Frame::request(h.command, SESSION, h.context, buf[24..].to_vec());
        f.header.session = SESSION;
        s.write_all(&f.encode().unwrap()).await.unwrap();
        tokio::time::sleep(Duration::from_secs(10)).await;
    });
    let mut c = Client::connect_with_timeout(addr, Duration::from_millis(100))
        .await
        .unwrap();
    let r = c.get_attribute_single(AttributePath::new(1, 1, 1)).await;
    assert!(matches!(r, Err(Error::Timeout(_))));
    let r = c.get_attribute_single(AttributePath::new(1, 1, 1)).await;
    assert!(matches!(r, Err(Error::Broken)));
}

const INPUTS: AttributePath = AttributePath::new(0x04, 111, 3);

#[tokio::test]
async fn close_unregisters_once() {
    let (addr, state) = spawn_adapter_with_state().await;
    let c = Client::connect(addr).await.unwrap();
    c.close().await.unwrap();
    assert!(state.wait_for(command::UNREGISTER_SESSION, 1).await);
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(
        state.count(command::UNREGISTER_SESSION),
        1,
        "Drop must not send it again"
    );
}

#[tokio::test]
async fn drop_unregisters_session() {
    let (addr, state) = spawn_adapter_with_state().await;
    let mut c = Client::connect(addr).await.unwrap();
    c.get_attribute_single(INPUTS).await.unwrap();
    drop(c);
    assert!(
        state.wait_for(command::UNREGISTER_SESSION, 1).await,
        "{:x?}",
        state.log.lock().unwrap()
    );

    // with a class 3 connection open as well
    let mut c = Client::connect(addr).await.unwrap();
    c.open_connected(ConnectionParams::explicit_class3())
        .await
        .unwrap();
    drop(c);
    assert!(state.wait_for(command::UNREGISTER_SESSION, 2).await);
}

#[test]
fn drop_after_runtime_shutdown_does_not_panic() {
    // The adapter runs on its own thread and runtime, which outlives the client runtime.
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async move {
            let (addr, _state) = spawn_adapter_with_state().await;
            tx.send(addr).unwrap();
            std::future::pending::<()>().await;
        });
    });
    let addr = rx.recv().unwrap();
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let c = rt.block_on(Client::connect(addr)).unwrap();
    drop(rt);
    drop(c);
}

#[tokio::test]
async fn socket_closed_mid_request_then_reconnect() {
    let (addr, state) = spawn_adapter_with_state().await;
    let mut c = Client::connect(addr).await.unwrap();
    assert_eq!(c.get_attribute_single(INPUTS).await.unwrap(), [0, 0, 5]);
    assert!(!c.is_broken());

    state.kill_next.store(true, Ordering::SeqCst);
    let e = c.get_attribute_single(INPUTS).await.unwrap_err();
    assert!(matches!(e, Error::Closed | Error::Io(_)), "{e:?}");
    assert!(c.is_broken());
    assert!(matches!(
        c.get_attribute_single(INPUTS).await,
        Err(Error::Broken)
    ));

    c.reconnect().await.unwrap();
    assert!(!c.is_broken());
    assert_eq!(c.get_attribute_single(INPUTS).await.unwrap(), [0, 0, 5]);
    assert_eq!(state.count(command::REGISTER_SESSION), 2);
    // a broken stream is never written to: no UnRegisterSession for the dead session
    assert_eq!(state.count(command::UNREGISTER_SESSION), 0);
    c.close().await.unwrap();
}

#[tokio::test]
async fn reconnect_reopens_class3() {
    let (addr, state) = spawn_adapter_with_state().await;
    let mut c = Client::connect(addr).await.unwrap();
    c.open_connected(ConnectionParams::explicit_class3())
        .await
        .unwrap();
    let first = c.connection().unwrap().clone();

    state.kill_next.store(true, Ordering::SeqCst);
    assert!(c.get_attribute_single(INPUTS).await.is_err());
    assert!(c.is_broken());

    c.reconnect().await.unwrap();
    let second = c.connection().expect("class 3 re-opened").clone();
    assert_ne!(
        first.connection_serial, second.connection_serial,
        "new connection triad"
    );
    // goes over SendUnitData; the adapter checks the connection ID
    assert_eq!(c.get_attribute_single(INPUTS).await.unwrap(), [0, 0, 5]);
    assert!(state.count(command::SEND_UNIT_DATA) >= 1);
    c.close().await.unwrap();
}

#[tokio::test]
async fn reconnect_keeps_peer_and_timeout_and_closes_old_session() {
    let (addr, state) = spawn_adapter_with_state().await;
    let mut c = Client::connect_with_timeout(addr, Duration::from_millis(200))
        .await
        .unwrap();
    // reconnecting a healthy client is allowed and unregisters the old session
    c.reconnect().await.unwrap();
    assert!(!c.is_broken());
    assert_eq!(c.peer_addr(), addr);
    assert_eq!(c.timeout(), Duration::from_millis(200));
    assert!(state.wait_for(command::UNREGISTER_SESSION, 1).await);
    assert_eq!(c.get_attribute_single(INPUTS).await.unwrap(), [0, 0, 5]);
}

#[tokio::test]
async fn reconnect_failure_leaves_client_broken_and_retryable() {
    // a target that accepts exactly one TCP connection, then goes away
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let state = Arc::new(AdapterState::default());
    let shared = state.clone();
    tokio::spawn(async move {
        let (s, _) = listener.accept().await.unwrap();
        drop(listener);
        serve(s, shared).await;
    });
    let mut c = Client::connect_with_timeout(addr, Duration::from_millis(200))
        .await
        .unwrap();
    state.kill_next.store(true, Ordering::SeqCst);
    assert!(c.get_attribute_single(INPUTS).await.is_err());
    assert!(c.reconnect().await.is_err());
    assert!(c.is_broken(), "unchanged after a failed reconnect");
    assert_eq!(c.peer_addr(), addr);
}
