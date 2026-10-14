// Copyright (C), 2026 Quartz Systems. Some rights reserved. This work is
// licensed under the terms of the MIT license which can be found in the
// root directory of this project.

//! Coupler logic against an in-memory object model (no network).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

use std::collections::HashMap;
use std::time::Duration;

use ur20_fbc_eip::cip::codec::Reader;
use ur20_fbc_eip::cip::message::{CipStatus, MessageResponse, service};
use ur20_fbc_eip::cip::{CipService, MessageRequest};
use ur20_fbc_eip::{ChannelValue, Coupler, CouplerLayout, Error, modules};

/// Attributes keyed by (class, instance, attribute), all 8-bit path segments.
#[derive(Default)]
struct MockCoupler {
    attrs: HashMap<(u8, u8, u8), Vec<u8>>,
    log: Vec<(u8, (u8, u8, u8))>,
}

impl MockCoupler {
    /// Slot class and gateway attributes as the real coupler serves them (2026-10-13):
    /// 101 = STRING name, 104 = UDINT module ID, 109/110 = UINT bits, 113/114 = data (empty
    /// when the module has no data in that direction). Past the last slot: status 0x05.
    fn add_slots(&mut self, slots: &[(&str, u32, usize, usize)]) {
        let mut list = Vec::new();
        for (i, (name, id, in_bits, out_bits)) in slots.iter().enumerate() {
            let slot = u8::try_from(i + 1).unwrap();
            let mut s = (name.len() as u16).to_le_bytes().to_vec();
            s.extend_from_slice(name.as_bytes());
            self.attrs.insert((0x65, slot, 101), s);
            self.attrs
                .insert((0x65, slot, 104), id.to_le_bytes().to_vec());
            self.attrs
                .insert((0x65, slot, 109), (*in_bits as u16).to_le_bytes().to_vec());
            self.attrs
                .insert((0x65, slot, 110), (*out_bits as u16).to_le_bytes().to_vec());
            self.attrs
                .insert((0x65, slot, 113), vec![0; in_bits.div_ceil(8)]);
            self.attrs
                .insert((0x65, slot, 114), vec![0; out_bits.div_ceil(8)]);
            list.extend_from_slice(&id.to_le_bytes());
        }
        list.resize(256, 0);
        self.attrs.insert((0x64, 1, 111), list);
        self.attrs.insert((0x66, 1, 107), vec![0, 0]);
        self.attrs.insert((0x66, 1, 108), vec![0]);
    }

    /// TODO-verify-on-hardware: a station of 4DI-P, 4DO-P, 4AI-UI-16, 4AO-UI-16 with invented
    /// data (the header layout matches hardware).
    fn station() -> Self {
        let mut m = Self::default();
        m.add_slots(&[
            ("UR20-4DI-P", 0x0009_1F84, 4, 0),
            ("UR20-4DO-P", 0x0101_2FA0, 0, 4),
            ("UR20-4AI-UI-16", 0x0401_15C4, 64, 0),
            ("UR20-4AO-UI-16", 0x0502_25E0, 0, 64),
        ]);
        // input assembly 111: status word 0x0001, 4DI = ch0+ch3, 4AI = 1,2,3,4
        m.attrs.insert(
            (0x04, 111, 3),
            vec![0x01, 0x00, 0b1001, 1, 0, 2, 0, 3, 0, 4, 0],
        );
        // output assembly 112: header 0, 4DO = ch1, 4AO = 0
        m.attrs
            .insert((0x04, 112, 3), vec![0, 0, 0b0010, 0, 0, 0, 0, 0, 0, 0, 0]);
        m
    }

    /// The bench station read on 2026-10-13: UR20-16DI-P + UR20-16DO-P, all signals off.
    fn bench() -> Self {
        let mut m = Self::default();
        m.add_slots(&[
            ("UR20-16DI-P", 0x0004_9FC2, 16, 0),
            ("UR20-16DO-P", 0x0103_AFD0, 0, 16),
        ]);
        m.attrs.insert((0x04, 111, 3), vec![0, 0, 0, 0]);
        m.attrs.insert((0x04, 112, 3), vec![0, 0, 0, 0]);
        m
    }
}

fn err(svc: u8, general: u8) -> MessageResponse {
    MessageResponse {
        service: svc | 0x80,
        status: CipStatus {
            general,
            extended: vec![],
        },
        data: vec![],
    }
}

impl CipService for MockCoupler {
    async fn send(&mut self, req: MessageRequest) -> ur20_fbc_eip::cip::Result<MessageResponse> {
        let bytes = req.encode()?;
        let mut r = Reader::new(&bytes, "mock");
        let svc = r.u8()?;
        let _words = r.u8()?;
        let p = r.take(6)?;
        let key = (p[1], p[3], p[5]);
        let data = r.rest().to_vec();
        self.log.push((svc, key));
        Ok(match svc {
            service::GET_ATTRIBUTE_SINGLE => match self.attrs.get(&key) {
                Some(v) => MessageResponse::success(svc, v.clone()),
                // past the last slot, as the real coupler answers
                None if key.0 == 0x65 => err(svc, 0x05),
                None => err(svc, 0x14),
            },
            service::SET_ATTRIBUTE_SINGLE => {
                if key.0 == 0x04 && self.attrs[&(0x66, 1, 108)] != [1] {
                    err(svc, 0x0F) // privilege violation until write access is enabled
                } else if let Some(v) = self.attrs.get_mut(&key) {
                    if v.len() != data.len() {
                        err(svc, if data.len() < v.len() { 0x13 } else { 0x15 })
                    } else {
                        *v = data;
                        MessageResponse::success(svc, vec![])
                    }
                } else {
                    err(svc, 0x16)
                }
            }
            _ => err(svc, 0x08),
        })
    }
}

fn coupler() -> Coupler<MockCoupler> {
    let modules = modules::create_all(&[
        "UR20-4DI-P",
        "UR20-4DO-P",
        "UR20-4AI-UI-16",
        "UR20-4AO-UI-16",
    ])
    .unwrap();
    Coupler::new(MockCoupler::station(), CouplerLayout::default(), modules).unwrap()
}

#[tokio::test]
async fn read_inputs() {
    let mut c = coupler();
    let img = c.read_inputs().await.unwrap();
    assert_eq!(img.status_word, Some(0x0001));
    assert_eq!(img.slots.len(), 2);
    assert_eq!(img.slots[0].position, 1);
    assert_eq!(
        img.slots[0].values,
        [
            ChannelValue::Bit(true),
            ChannelValue::Bit(false),
            ChannelValue::Bit(false),
            ChannelValue::Bit(true)
        ]
    );
    assert_eq!(img.slots[1].position, 3);
    assert_eq!(img.slots[1].values[3], ChannelValue::Int16(4));
    assert!(img.trailer.is_empty());
}

#[tokio::test]
async fn set_output_requires_write_access_and_preserves_other_channels() {
    let mut c = coupler();
    let e = c
        .set_output(2, 0, ChannelValue::Bit(true))
        .await
        .unwrap_err();
    assert_eq!(
        match &e {
            Error::Cip(c) => c.cip_status().map(|s| s.general),
            _ => None,
        },
        Some(0x0F)
    );

    c.enable_explicit_outputs(Duration::from_millis(4500))
        .await
        .unwrap();
    assert!(c.explicit_outputs_enabled().await.unwrap());
    assert_eq!(c.service_mut().attrs[&(0x66, 1, 107)], [5, 0]); // rounded up to 5 s

    c.set_output(2, 0, ChannelValue::Bit(true)).await.unwrap();
    c.set_output(4, 2, ChannelValue::Int16(-2)).await.unwrap();
    assert_eq!(
        c.service_mut().attrs[&(0x04, 112, 3)],
        [0, 0, 0b0011, 0, 0, 0, 0, 0xfe, 0xff, 0, 0]
    );
    let out = c.read_outputs().await.unwrap();
    assert_eq!(out.slots[0].values[1], ChannelValue::Bit(true));
    assert_eq!(out.slots[1].values[2], ChannelValue::Int16(-2));

    // wrong types / channels / slots never reach the device
    let before = c.service_mut().log.len();
    assert!(c.set_output(2, 0, ChannelValue::Float(1.0)).await.is_err());
    assert!(c.set_output(2, 9, ChannelValue::Bit(true)).await.is_err());
    assert!(c.set_output(9, 0, ChannelValue::Bit(true)).await.is_err());
    let writes = c.service_mut().log[before..]
        .iter()
        .filter(|(svc, _)| *svc == service::SET_ATTRIBUTE_SINGLE)
        .count();
    assert_eq!(writes, 0);
}

#[tokio::test]
async fn discover_and_verify() {
    let mut c = coupler();
    let slots = c.verify_station().await.unwrap();
    assert_eq!(slots.len(), 4);
    assert_eq!(slots[2].name.as_deref(), Some("UR20-4AI-UI-16"));
    assert_eq!(slots[2].module_id, Some(0x0401_15C4));
    assert_eq!((slots[2].input_bits, slots[2].output_bits), (64, 0));
    assert_eq!((slots[0].input_bytes(), slots[0].output_bytes()), (1, 0));

    let short = modules::create_all(&["UR20-4DI-P", "UR20-4DO-P", "UR20-4AI-UI-16"]).unwrap();
    let mut c = Coupler::new(MockCoupler::station(), CouplerLayout::default(), short).unwrap();
    assert!(matches!(
        c.verify_station().await,
        Err(Error::StationMismatch { position: 0, .. })
    ));

    // same size, different module: caught by module ID
    let swapped = modules::create_all(&[
        "UR20-4DI-P-3W",
        "UR20-4DO-P",
        "UR20-4AI-UI-16",
        "UR20-4AO-UI-16",
    ])
    .unwrap();
    let mut c = Coupler::new(MockCoupler::station(), CouplerLayout::default(), swapped).unwrap();
    let e = c.verify_station().await.unwrap_err();
    assert!(e.to_string().contains("coupler has UR20-4DI-P"), "{e}");
}

#[tokio::test]
async fn detect_bench_station() {
    let mut c = Coupler::detect(MockCoupler::bench(), CouplerLayout::default())
        .await
        .unwrap();
    let names: Vec<_> = c
        .station()
        .slots()
        .iter()
        .map(|s| s.module.name())
        .collect();
    assert_eq!(names, ["UR20-16DI-P", "UR20-16DO-P"]);
    c.verify_station().await.unwrap();
    let img = c.read_inputs().await.unwrap();
    assert_eq!(img.status_word, Some(0));
    assert_eq!(img.slots[0].values, vec![ChannelValue::Bit(false); 16]);
    let out = c.read_outputs().await.unwrap();
    assert_eq!(out.header, [0, 0]);
    assert_eq!(out.slots[0].values, vec![ChannelValue::Bit(false); 16]);
}

#[tokio::test]
async fn bench_assemblies_from_hardware() {
    // Raw assembly 111 read on 2026-10-13: DI channel 8 on, then 16DO pulled.
    let mut mock = MockCoupler::bench();
    mock.attrs
        .insert((0x04, 111, 3), vec![0x00, 0x00, 0x00, 0x01]);
    let mut c = Coupler::detect(mock, CouplerLayout::default())
        .await
        .unwrap();
    let img = c.read_inputs().await.unwrap();
    assert_eq!(img.status_word, Some(0x0000));
    let on: Vec<usize> = (0..16)
        .filter(|i| img.slots[0].values[*i] == ChannelValue::Bit(true))
        .collect();
    assert_eq!(on, [8]);
    assert_eq!(img.bit(1, 8), Some(true));
    assert_eq!(img.bit(1, 7), Some(false));
    assert_eq!(img.bit(1, 16), None); // no such channel
    assert_eq!(img.bit(2, 8), None); // the 16DO has no inputs

    c.service_mut()
        .attrs
        .insert((0x04, 111, 3), vec![0x40, 0x00, 0x00, 0x01]);
    let img = c.read_inputs().await.unwrap();
    assert_eq!(img.status_word, Some(0x0040));
}

#[tokio::test]
async fn detect_rejects_unsupported_modules() {
    let mut mock = MockCoupler::default();
    mock.add_slots(&[("UR20-4AI-RTD-DIAG", 0x0406_1544, 64, 0)]);
    assert!(matches!(
        Coupler::detect(mock, CouplerLayout::default()).await,
        Err(Error::UnsupportedModule { .. })
    ));
}

#[tokio::test]
async fn short_assembly_is_an_error() {
    let mut mock = MockCoupler::station();
    mock.attrs.insert((0x04, 111, 3), vec![0x01]);
    let modules = modules::create_all(&["UR20-4DI-P"]).unwrap();
    let mut c = Coupler::new(mock, CouplerLayout::default(), modules).unwrap();
    assert!(matches!(
        c.read_inputs().await,
        Err(Error::BufferLength { .. })
    ));
}

#[tokio::test]
async fn header_override() {
    let mut mock = MockCoupler::station();
    mock.attrs.insert((0x04, 120, 3), vec![0b0001]);
    let modules = modules::create_all(&["UR20-4DI-P"]).unwrap();
    let layout = CouplerLayout::default()
        .with_input_instance(120)
        .with_header_lens(0, 0);
    let mut c = Coupler::new(mock, layout, modules).unwrap();
    let img = c.read_inputs().await.unwrap();
    assert_eq!(img.status_word, None);
    assert_eq!(img.slots[0].values[0], ChannelValue::Bit(true));
}

/// Set_Attribute_Single requests to the output assembly since log entry `from`.
fn assembly_writes(c: &mut Coupler<MockCoupler>, from: usize) -> usize {
    c.service_mut().log[from..]
        .iter()
        .filter(|(svc, key)| *svc == service::SET_ATTRIBUTE_SINGLE && *key == (0x04, 112, 3))
        .count()
}

#[tokio::test]
async fn set_outputs_is_one_write() {
    let mut c = coupler();
    c.enable_explicit_outputs(Duration::from_secs(2))
        .await
        .unwrap();
    let before = c.service_mut().log.len();
    c.set_outputs(&[
        (2, 0, ChannelValue::Bit(true)),
        (2, 3, ChannelValue::Bit(true)),
        (2, 1, ChannelValue::Bit(false)), // was on
        (4, 3, ChannelValue::Int16(-1)),
    ])
    .await
    .unwrap();
    assert_eq!(assembly_writes(&mut c, before), 1);
    assert_eq!(c.service_mut().log.len() - before, 2, "one read, one write");
    assert_eq!(
        c.service_mut().attrs[&(0x04, 112, 3)],
        [0, 0, 0b1001, 0, 0, 0, 0, 0, 0, 0xff, 0xff]
    );

    // all or nothing: one bad entry and nothing is written
    let before = c.service_mut().log.len();
    let image = c.service_mut().attrs[&(0x04, 112, 3)].clone();
    for bad in [
        vec![
            (2, 0, ChannelValue::Bit(false)),
            (2, 4, ChannelValue::Bit(true)),
        ],
        vec![
            (2, 0, ChannelValue::Bit(false)),
            (9, 0, ChannelValue::Bit(true)),
        ],
        vec![
            (2, 0, ChannelValue::Bit(false)),
            (1, 0, ChannelValue::Bit(true)),
        ],
        vec![
            (2, 0, ChannelValue::Bit(false)),
            (4, 0, ChannelValue::Bit(true)),
        ],
    ] {
        assert!(c.set_outputs(&bad).await.is_err(), "{bad:?}");
    }
    assert_eq!(assembly_writes(&mut c, before), 0);
    assert_eq!(c.service_mut().attrs[&(0x04, 112, 3)], image);

    // empty batch: no traffic at all
    let before = c.service_mut().log.len();
    c.set_outputs(&[]).await.unwrap();
    assert_eq!(c.service_mut().log.len(), before);
}

#[tokio::test]
async fn write_slot_outputs_replaces_one_slot() {
    let mut c = Coupler::detect(MockCoupler::bench(), CouplerLayout::default())
        .await
        .unwrap();
    c.enable_explicit_outputs(Duration::from_secs(2))
        .await
        .unwrap();
    let mut values = vec![ChannelValue::Bit(false); 16];
    values[8] = ChannelValue::Bit(true);
    let before = c.service_mut().log.len();
    c.write_slot_outputs(2, &values).await.unwrap();
    assert_eq!(assembly_writes(&mut c, before), 1);
    // hardware-confirmed (2026-10-13): DO channel 8 = module bytes 00 01
    assert_eq!(c.service_mut().attrs[&(0x04, 112, 3)], [0, 0, 0x00, 0x01]);

    let before = c.service_mut().log.len();
    assert!(c.write_slot_outputs(2, &values[..15]).await.is_err());
    assert!(c.write_slot_outputs(1, &values).await.is_err()); // 16DI has no outputs
    assert_eq!(assembly_writes(&mut c, before), 0);
}

#[tokio::test]
async fn output_image_round_trip() {
    let mut c = coupler();
    c.enable_explicit_outputs(Duration::from_secs(2))
        .await
        .unwrap();
    // a header byte the coupler set, which must survive the round trip
    c.service_mut().attrs.get_mut(&(0x04, 112, 3)).unwrap()[1] = 0x80;

    let mut img = c.read_outputs().await.unwrap();
    assert_eq!(img.header, [0, 0x80]);
    assert!(img.trailer.is_empty());
    assert_eq!(img.value(2, 1), Some(&ChannelValue::Bit(true)));
    img.set(2, 1, ChannelValue::Bit(false)).unwrap();
    img.set(2, 2, ChannelValue::Bit(true)).unwrap();
    img.set(4, 0, ChannelValue::Int16(-1)).unwrap();
    assert!(matches!(
        img.set(1, 0, ChannelValue::Bit(true)),
        Err(Error::NoSuchSlot { .. })
    ));
    assert!(matches!(
        img.set(2, 4, ChannelValue::Bit(true)),
        Err(Error::InvalidChannel { .. })
    ));

    let before = c.service_mut().log.len();
    c.write_outputs(&img).await.unwrap();
    assert_eq!(c.service_mut().log.len() - before, 1, "one write, no read");
    assert_eq!(
        c.service_mut().attrs[&(0x04, 112, 3)],
        [0, 0x80, 0b0100, 0xff, 0xff, 0, 0, 0, 0, 0, 0]
    );
    assert_eq!(c.read_outputs().await.unwrap(), img);

    // a wrong type is caught by the codec before anything is sent
    let mut bad = img.clone();
    bad.set(2, 0, ChannelValue::Int16(1)).unwrap();
    let mut partial = img.clone();
    partial.slots.pop();
    let mut short_header = img.clone();
    short_header.header.pop();
    let before = c.service_mut().log.len();
    for image in [bad, partial, short_header] {
        assert!(c.write_outputs(&image).await.is_err());
    }
    assert_eq!(c.service_mut().log.len(), before);
}

#[tokio::test]
async fn output_trailer_is_written_back() {
    let mut mock = MockCoupler::bench();
    mock.attrs.insert((0x04, 112, 3), vec![0, 0, 0, 0, 7, 7]);
    mock.attrs.insert((0x66, 1, 108), vec![1]);
    let mut c = Coupler::detect(mock, CouplerLayout::default())
        .await
        .unwrap();
    let mut img = c.read_outputs().await.unwrap();
    assert_eq!(img.trailer, [7, 7]);
    img.set(2, 0, ChannelValue::Bit(true)).unwrap();
    c.write_outputs(&img).await.unwrap();
    assert_eq!(c.service_mut().attrs[&(0x04, 112, 3)], [0, 0, 1, 0, 7, 7]);
}
