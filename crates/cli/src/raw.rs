// Copyright (C), 2026 Quartz Systems. Some rights reserved. This work is
// licensed under the terms of the MIT license which can be found in the
// root directory of this project.

//! `ur20eip raw …`: poke arbitrary objects during bring-up.

use anyhow::Result;
use clap::Subcommand;
use ur20_fbc_eip_cip::{
    AttributePath, CipService, Client, EPath, MessageRequest, ObjectPath, Segment, hex,
};

use crate::parse;

#[derive(Debug, Subcommand)]
pub enum RawCommand {
    /// Get_Attribute_Single (0x0E).
    Get {
        /// Class ID (decimal or 0x hex).
        #[arg(value_parser = parse::int::<u16>)]
        class: u16,
        /// Instance ID.
        #[arg(value_parser = parse::int::<u32>)]
        instance: u32,
        /// Attribute ID.
        #[arg(value_parser = parse::int::<u16>)]
        attribute: u16,
    },
    /// Get_Attributes_All (0x01).
    GetAll {
        /// Class ID.
        #[arg(value_parser = parse::int::<u16>)]
        class: u16,
        /// Instance ID.
        #[arg(value_parser = parse::int::<u32>)]
        instance: u32,
    },
    /// Set_Attribute_Single (0x10).
    Set {
        /// Class ID.
        #[arg(value_parser = parse::int::<u16>)]
        class: u16,
        /// Instance ID.
        #[arg(value_parser = parse::int::<u32>)]
        instance: u32,
        /// Attribute ID.
        #[arg(value_parser = parse::int::<u16>)]
        attribute: u16,
        /// Value as hex bytes in wire order, e.g. "01" or "05 00".
        #[arg(value_parser = parse::hex_bytes)]
        value: Vec<u8>,
    },
    /// Any service code against class/instance[/attribute] with optional request data.
    Service {
        /// Service code.
        #[arg(value_parser = parse::int::<u8>)]
        service: u8,
        /// Class ID.
        #[arg(value_parser = parse::int::<u16>)]
        class: u16,
        /// Instance ID.
        #[arg(value_parser = parse::int::<u32>)]
        instance: u32,
        /// Attribute ID.
        #[arg(long, value_parser = parse::int::<u16>)]
        attribute: Option<u16>,
        /// Request data as hex bytes.
        #[arg(long, value_parser = parse::hex_bytes)]
        data: Option<Vec<u8>>,
    },
    /// Get_Attribute_Single over a range of attributes, skipping unsupported ones.
    Scan {
        /// Class ID.
        #[arg(value_parser = parse::int::<u16>)]
        class: u16,
        /// Instance ID.
        #[arg(value_parser = parse::int::<u32>)]
        instance: u32,
        /// First attribute.
        #[arg(value_parser = parse::int::<u16>)]
        first: u16,
        /// Last attribute (inclusive).
        #[arg(value_parser = parse::int::<u16>)]
        last: u16,
    },
}

fn print_bytes(data: &[u8]) {
    println!("{} byte(s)", data.len());
    for (i, chunk) in data.chunks(16).enumerate() {
        let ascii: String = chunk
            .iter()
            .map(|b| {
                if b.is_ascii_graphic() || *b == b' ' {
                    char::from(*b)
                } else {
                    '.'
                }
            })
            .collect();
        println!("{:04x}: {:<47}  {ascii}", i * 16, hex(chunk));
    }
}

pub async fn run(client: &mut Client, cmd: &RawCommand) -> Result<()> {
    match *cmd {
        RawCommand::Get {
            class,
            instance,
            attribute,
        } => {
            let data = client
                .get_attribute_single(AttributePath::new(class, instance, attribute))
                .await?;
            print_bytes(&data);
        }
        RawCommand::GetAll { class, instance } => {
            let data = client
                .get_attributes_all(ObjectPath::new(class, instance))
                .await?;
            print_bytes(&data);
        }
        RawCommand::Set {
            class,
            instance,
            attribute,
            ref value,
        } => {
            client
                .set_attribute_single(AttributePath::new(class, instance, attribute), value)
                .await?;
            println!("ok");
        }
        RawCommand::Service {
            service,
            class,
            instance,
            attribute,
            ref data,
        } => {
            let mut path = EPath::from(ObjectPath::new(class, instance));
            if let Some(a) = attribute {
                path = path.with(Segment::Attribute(a));
            }
            let request = MessageRequest::new(service, path, data.clone().unwrap_or_default());
            let response = client.send(request).await?;
            println!(
                "reply service {:#04x}: {}",
                response.service, response.status
            );
            print_bytes(&response.data);
        }
        RawCommand::Scan {
            class,
            instance,
            first,
            last,
        } => {
            for attribute in first..=last {
                let path = AttributePath::new(class, instance, attribute);
                match client.get_attribute_single(path).await {
                    Ok(data) => println!("attr {attribute:>3}: [{:>3}] {}", data.len(), hex(&data)),
                    Err(e) if e.cip_status().is_some() => {
                        tracing::debug!(attribute, error = %e, "skipped")
                    }
                    Err(e) => return Err(e.into()),
                }
            }
        }
    }
    Ok(())
}
