// Copyright (C), 2026 Quartz Systems. Some rights reserved. This work is
// licensed under the terms of the MIT license which can be found in the
// root directory of this project.

//! The UR20-FBC-EIP coupler over explicit messaging.

use std::time::Duration;

use tokio::net::ToSocketAddrs;
use tracing::{debug, info};
use ur20_fbc_eip_cip::{AttributePath, CipService, Client, Identity};

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
#[derive(Debug)]
pub struct Coupler<S = Client> {
    service: S,
    layout: CouplerLayout,
    station: Station,
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
        Self::new(service, layout, modules)
    }

    /// Wrap an existing transport.
    pub fn new(service: S, layout: CouplerLayout, modules: Vec<Box<dyn Module>>) -> Result<Self> {
        let station = Station::new(modules, layout.format)?;
        Ok(Self {
            service,
            layout,
            station,
        })
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
        Ok(self.service.identity().await?)
    }

    /// Raw bytes of the input assembly, header included.
    pub async fn read_input_assembly(&mut self) -> Result<Vec<u8>> {
        Ok(self.service.get_attribute_single(self.layout.input).await?)
    }

    /// Raw bytes of the output assembly, header included.
    pub async fn read_output_assembly(&mut self) -> Result<Vec<u8>> {
        Ok(self
            .service
            .get_attribute_single(self.layout.output)
            .await?)
    }

    /// Write the whole output assembly, header included.
    ///
    /// Requires explicit write access, see [`Coupler::enable_explicit_outputs`].
    pub async fn write_output_assembly(&mut self, data: &[u8]) -> Result<()> {
        Ok(self
            .service
            .set_attribute_single(self.layout.output, data)
            .await?)
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

    /// Read and decode the output assembly.
    pub async fn read_outputs(&mut self) -> Result<OutputImage> {
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
        self.write_output_assembly(&data).await
    }

    /// Request explicit messaging write access to outputs.
    ///
    /// Writes the timeout (seconds, rounded up, 0 = until all explicit connections close)
    /// and then sets "Start Explicit Process Data Exchange". The coupler refuses this while a
    /// PLC holds an Exclusive Owner (class 1) connection. Access lapses if no explicit
    /// request arrives within `timeout`, so keep polling (e.g. [`Coupler::read_inputs`]).
    pub async fn enable_explicit_outputs(&mut self, timeout: Duration) -> Result<()> {
        let Some(ctrl) = self.layout.explicit_write else {
            return Ok(());
        };
        let secs = timeout.as_secs() + u64::from(timeout.subsec_nanos() > 0);
        let secs = u16::try_from(secs).unwrap_or(u16::MAX);
        self.service
            .set_attribute_single(ctrl.timeout, &secs.to_le_bytes())
            .await?;
        self.service.set_attribute_single(ctrl.start, &[1]).await?;
        info!(timeout_s = secs, "explicit output write access enabled");
        Ok(())
    }

    /// Whether explicit write access is currently active.
    pub async fn explicit_outputs_enabled(&mut self) -> Result<bool> {
        let Some(ctrl) = self.layout.explicit_write else {
            return Ok(true);
        };
        let v = self.service.get_attribute_single(ctrl.start).await?;
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

    /// Read the output assembly, let `change` edit the module data, write it back.
    async fn modify_outputs(
        &mut self,
        change: impl FnOnce(&Station, &mut [u8]) -> Result<()>,
    ) -> Result<()> {
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
        match self.service.get_attribute_single(path).await {
            Ok(v) => Ok(SlotAttr::Data(v)),
            Err(e) => match e.cip_status().map(|s| s.general) {
                Some(0x05 | 0x16) => Ok(SlotAttr::NoSlot),
                Some(0x14) => Ok(SlotAttr::Missing),
                _ => Err(e.into()),
            },
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
        let status =
            StatusWord::decode(&self.service.get_attribute_single(paths.status_word).await?)?;
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
        parse_module_list(&self.service.get_attribute_single(path).await?)
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
