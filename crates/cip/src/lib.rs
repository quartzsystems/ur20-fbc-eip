// Copyright (C), 2026 Quartz Systems. Some rights reserved. This work is
// licensed under the terms of the MIT license which can be found in the
// root directory of this project.

//! EtherNet/IP encapsulation and CIP explicit messaging for tokio.
//!
//! This crate is the transport layer of [`ur20-fbc-eip`](https://docs.rs/ur20-fbc-eip) and
//! knows nothing about Weidmüller u-remote. It is usable against any EtherNet/IP adapter.
//!
//! ```no_run
//! use ur20_fbc_eip_cip::{AttributePath, CipService, Client};
//!
//! # async fn demo() -> ur20_fbc_eip_cip::Result<()> {
//! let mut client = Client::connect("192.168.1.10:44818").await?;
//! println!("{}", client.identity().await?);
//! let bytes = client.get_attribute_single(AttributePath::new(0x04, 111, 3)).await?;
//! client.close().await?;
//! # Ok(()) }
//! ```
//!
//! Layers, bottom up: [`encap`] (24-byte header, sessions) → [`cpf`] (Common Packet Format) →
//! [`message`] (Message Router request/response, [`epath`]) → [`connection`]
//! (Forward_Open/Close) → [`Client`] / [`CipService`]. [`io`] holds the types for class 1
//! implicit I/O, which is not implemented yet.

#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::panic
    )
)]

pub mod client;
pub mod codec;
pub mod connection;
pub mod cpf;
pub mod encap;
pub mod epath;
pub mod error;
pub mod identity;
pub mod io;
pub mod message;
pub mod service;

pub use client::Client;
pub use codec::hex;
pub use connection::{ConnectionParams, ForwardOpenReply};
pub use epath::{AttributePath, EPath, ObjectPath, Segment};
pub use error::{Error, Result};
pub use identity::{Identity, ListIdentityEntry};
pub use message::{CipStatus, MessageRequest, MessageResponse};
pub use service::CipService;
