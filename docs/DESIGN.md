# ur20-fbc-eip — design

Status: phase 1 (CIP + identity + raw CLI) and phase 2 (assemblies + modules) are implemented
and were exercised read-only against a UR20-FBC-EIP (firmware string 01.21.00) on 2026-10-13.
Class 1 implicit I/O is designed for but **not** implemented.

## 1. Workspace layout

```
ur20-fbc-eip/                      (Cargo workspace root)
├── Cargo.toml                     workspace + shared package metadata
├── README.md  CONTRIBUTING.md  LICENSE (MIT)
├── docs/DESIGN.md                 this file
└── crates/
    ├── cip/                       ur20-fbc-eip-cip   transport only, knows nothing about u-remote
    │   └── src/
    │       ├── codec.rs           bounds-checked little-endian reader/writer (no panics)
    │       ├── encap.rs           24-byte encapsulation header, commands, status codes
    │       ├── cpf.rs             Common Packet Format items (incl. sockaddr items for class 1)
    │       ├── epath.rs           logical-segment EPATH (class/instance/attribute/connection point)
    │       ├── message.rs         Message Router request/response, service codes, CipStatus
    │       ├── connection.rs      Forward_Open / Forward_Close, ConnectionParams (class 1 & 3)
    │       ├── io.rs              class 1 implicit I/O *types only* (run/idle header, sequence)
    │       ├── identity.rs        Identity Object (class 0x01) + ListIdentity
    │       ├── service.rs         `CipService` trait: get/set_attribute_single, get_attributes_all
    │       └── client.rs          tokio TCP client: session, UCMM, connected (class 3)
    ├── ur20-fbc-eip/              ur20-fbc-eip       coupler + module library
    │   └── src/
    │       ├── layout.rs          `CouplerLayout` (assembly instances, header sizes, …)
    │       ├── module.rs          `Module` trait, `ChannelValue`, `ModuleDescriptor`
    │       ├── modules/           one file per module family + registry macro
    │       ├── station.rs         slot list → byte offsets in the process images
    │       └── coupler.rs         `Coupler<S: CipService>`
    └── cli/                       ur20-fbc-eip-cli   binary `ur20eip`
```

Dependency direction: `cli → ur20-fbc-eip → ur20-fbc-eip-cip`. The module codecs
(`module.rs`, `modules/`, `station.rs`) are pure and need no network, so they can be
unit-tested byte by byte.

## 2. Build vs. buy: `rseip`

`rseip` 0.3.1 (Joylei/eip-rs) was evaluated. It builds on current stable and covers
session registration, UCMM and class 3 Forward_Open. **Decision: implement ourselves.** Reasons:

* Last commit 2023-03, marked passively maintained; logs via `log`, not `tracing`.
* Its `Decode<'de>`/`Encode` generic codec leaks into every call site; we only ever need
  `&[u8]` in and out.
* Class 1 (the planned phase 3) needs the sockaddr CPF items from the Forward_Open reply,
  a UDP socket and the run/idle header. rseip has none of that, so we would have to
  fork it anyway.
* What we need is ~1k lines and fully fixture-testable.

## 3. Facts taken from Weidmüller documents (vs. guesses)

Source: *QSG0028v3 UR20-FBC-EIP Quick Start Guide – MicroLogix 1400*, ch. 3, and
*AN0050v3 Importing module data … into Studio 5000*.

| Object | Class | Instance | Attr | Meaning |
|---|---|---|---|---|
| Assembly | 0x04 | 101 / 102 | 3 | in / out, 2-byte header + **packed** data |
| Assembly | 0x04 | 103 / 104 | 3 | as 101/102 + 8 bytes diagnostics |
| Assembly | 0x04 | **111 / 112** | 3 | in / out, 2-byte header + **unpacked** data (default) |
| Assembly | 0x04 | 113 / 114 | 3 | as 111/112 + 8 bytes diagnostics |
| Slot | 0x65 | 1…64 | 113 / 114 | per-module process data in / out (empty if none) |
| Slot | 0x65 | 1…64 | 101 / 104 | module name (STRING) / module ID (UDINT) ✔ |
| Slot | 0x65 | 1…64 | 109 / 110 | input / output size in bits (UINT) ✔ |
| Slot | 0x65 | 1…64 | 116 / 117 | 47 diagnostic bytes / 4 process-alarm bytes, 0 bytes if the module is pulled ✔ |
| Process Data | 0x66 | 1 | 101 / 103 | packed data in / out, no header |
| Process Data | 0x66 | 1 | 107 (WORD) | explicit-write timeout in s |
| Process Data | 0x66 | 1 | 108 (BOOL) | "Start Explicit Process Data Exchange" |
| Gateway | 0x64 | 1 | 101… | coupler identity/status/params (101 = HW version) |
| Gateway | 0x64 | 1 | 104 | status word, same value as the 111 header ✔ |
| Gateway | 0x64 | 1 | 108 | latest diagnosis event, 4 bytes; empty when none is active ✔ |
| Gateway | 0x64 | 1 | 111 | 64 × UDINT module IDs, slot 1 first, 0 = empty ✔ |
| Gateway | 0x64 | 1 | 118 | 8 bytes unacknowledged diagnosis ✔ (no bit seen set yet) |
| Module Params | 0x67 | 1…64 | 101… | per-module parameters |

✔ = read from hardware, not in the QSG. Attribute 100 of each instance holds its highest
attribute number. Past the last slot the coupler answers 0x05 (path destination unknown);
Get_Attributes_All on class 0x65 is not supported (0x08). With a module pulled, its slot
instance stays: name, ID, data and diagnosis read as 0 bytes, but 109/110 keep the old sizes.
The diagnosis bits are documented in `ur20_fbc_eip::diagnosis`.

The input header is the **coupler status word** (AN0050 §5.3). Hardware confirms 2-byte
headers on **both** 111 and 112 (each 4 bytes with one 16DI and one 16DO).

Important consequence: **explicit output writes are rejected until 0x66/1/108 is set to 1**,
and that is refused while a PLC holds an Exclusive Owner connection. `Coupler` exposes this as
`enable_explicit_outputs(timeout)`.

Still marked `TODO-verify-on-hardware` in code (and listed in §7):
* bit order of 16-channel modules (needs a live signal) and byte order of 16-bit analog data;
* status word bits other than 6 (module removed) and 13 (output supply error), and the
  output header (0x0000 in every state seen);
* most of the 47-byte module diagnosis, 0x64/1/108 and all of 0x64/1/118 (see §7);
* where the 8 diagnostic bytes sit in 113/114 (assumed after process data);
* UR20-2CNT-100 process data layout.

## 4. Public API — `ur20-fbc-eip-cip`

```rust
let mut c = Client::connect("192.168.1.10:44818").await?;          // RegisterSession
let id: Identity = c.identity().await?;                             // 0x01/1 Get_Attributes_All
let v: Vec<u8> = c.get_attribute_single(AttributePath::new(0x04, 111, 3)).await?;
c.set_attribute_single(AttributePath::new(0x66, 1, 108), &[1]).await?;
let all = c.get_attributes_all(ObjectPath::new(0x01, 1)).await?;
let r: MessageResponse = c.send(MessageRequest::new(svc, epath, data)).await?; // any service

c.open_connected(ConnectionParams::explicit_class3()).await?;      // Forward_Open
// subsequent requests go over SendUnitData with sequence counts
c.close().await?;                                                   // Forward_Close + UnRegister
```

`CipService` is a trait (`send` + provided `get_attribute_single` etc.) so the coupler can be
tested against an in-memory mock. Errors are a single `cip::Error` (thiserror) carrying
encapsulation status, CIP general/extended status, timeouts and malformed-frame details.

**Class 1 hook:** `ConnectionParams` already carries `TransportClass`, trigger, per-direction
`NetworkParams` (size, fixed/variable, priority, P2P/multicast) and RPI, and its connection
path can contain configuration/consumed/produced connection points. `ForwardOpenReply` keeps
any sockaddr CPF items. `io.rs` defines `RunIdleHeader` and `SequenceCount` types. Phase 3
adds an `ImplicitConnection` that owns a UDP socket on 2222.

## 5. Public API — `ur20-fbc-eip`

```rust
pub enum ChannelValue { Bit(bool), Int16(i16), UInt16(u16), Int32(i32), UInt32(u32), Float(f32), Raw(Vec<u8>) }

pub trait Module: Debug + Send + Sync {
    fn descriptor(&self) -> &'static ModuleDescriptor;    // name, module id, category
    fn input_bits(&self) -> usize;
    fn output_bits(&self) -> usize;
    fn input_channels(&self) -> usize;  fn output_channels(&self) -> usize;
    fn output_kind(&self, channel: usize) -> Option<ValueKind>;
    fn decode_inputs(&self, data: &[u8]) -> Result<Vec<ChannelValue>>;
    fn decode_outputs(&self, data: &[u8]) -> Result<Vec<ChannelValue>>;
    fn encode_outputs(&self, values: &[ChannelValue], out: &mut [u8]) -> Result<()>;
    fn encode_output_channel(&self, ch: usize, v: &ChannelValue, out: &mut [u8]) -> Result<()>; // provided
}

let layout = CouplerLayout::default()           // unpacked 111/112, 2-byte headers
    .with_input_instance(111).with_output_instance(112);
let mut cpl = Coupler::connect_auto(addr, layout).await?;    // modules from 0x64/1/111
// or by hand:
let modules = modules::create_all(&["UR20-4DI-P", "UR20-4DO-P", "UR20-4AI-UI-16"])?; // unknown → Err
let mut cpl = Coupler::connect(addr, layout, modules).await?;
let img = cpl.read_inputs().await?;              // InputImage { status_word, slots: [...] }
cpl.enable_explicit_outputs(Duration::from_secs(5)).await?;
cpl.set_output(slot, channel, ChannelValue::Bit(true)).await?;   // read-modify-write of 112
cpl.set_outputs(&[(2, 0, Bit(true)), (2, 1, Bit(false))]).await?; // one read + one write, all or nothing
cpl.write_slot_outputs(2, &values).await?;       // every channel of one module, one write
let mut out = cpl.read_outputs().await?;         // OutputImage { header, slots, trailer }
out.set(2, 3, Bit(true))?;                       // edit locally, nothing sent
cpl.write_outputs(&out).await?;                  // one write, no read; header/trailer kept
let slots = cpl.discover_slots().await?;         // class 0x65 → SlotInfo { id, name, bits }
let d = cpl.diagnosis().await?;                  // CouplerDiagnosis { status: StatusWord, … }
d.status.output_supply_error();                  // named only where confirmed on hardware
let s = cpl.slot_diagnosis(2).await?;            // SlotDiagnosis { module: Option<ModuleDiagnosis>, … }
cpl.verify_station().await?;                     // ID or size mismatch → Err
```

Adding a module is one new file in `modules/` plus one identifier in the `register_modules!`
list (see CONTRIBUTING.md). `Station` computes offsets from the module sizes; the unpacked
format rounds each module up to whole bytes, packed format returns `Error::Unsupported`
until verified.

## 6. CLI

```
ur20eip [--addr IP[:PORT] | $UR20_EIP_ADDR] [--timeout-ms N] [--connected]
  identity                                   Identity object (+ ListIdentity)
  slots                                      per-slot name, ID and bit sizes (class 0x65)
  dump-inputs  --modules A,B,C [layout opts] [--raw]  decoded input image
  dump-outputs --modules …                   decoded output image
  watch        --modules … --interval 250ms  print changed input channels
  set <slot> <channel> <value> --modules … [--write-timeout 5s] [--no-enable]
                                             read-modify-write one output channel
  set-many <slot:ch=value>… --modules … [--write-timeout 5s] [--no-enable]
                                             several channels, one read + one write
  diag [--slot N]… [--raw]                   status word, coupler and slot diagnosis
  raw get     <class> <instance> <attr>
  raw get-all <class> <instance>
  raw set     <class> <instance> <attr> <hex-bytes>
  raw scan    <class> <instance> <first> <last>
  raw service <svc> <class> <instance> [--attribute A] [--data HEX]
```
Layout options: `--input-assembly`, `--output-assembly`, `--input-header`, `--output-header`.
`--modules` (`$UR20_EIP_MODULES`) is optional: without it the module list is read from the
coupler; with it the list is verified against the coupler unless `--no-verify` is given.
`UR20_EIP_LOG` sets the tracing filter (e.g. `trace` for hex dumps of every frame).

## 7. Bench results and open questions

Answered on 2026-10-13 (station: UR20-16DI-P, UR20-16DO-P; all signals off), read-only:

1. ✔ 111 and 112 each return 2 header bytes + 2 module bytes; layout matches `Station`.
2. ✔ Module IDs: Gateway 0x64/1/111 → auto-layout implemented (`Coupler::detect`).
3. ✔ Slot class attributes 101–117 (see §3); verify now compares module ID and bits.
4. ✔ Config assembly for "Exclusive Owner Unpacked" is instance 1 (EDS v1.6/v1.7 path
   `20 04 24 01 2C 70 2C 6F`); instance 123 (400 bytes, module parameters) only for packed
   "with Config Assembly". Input Only / Listen Only use output points 0xC6 / 0xC7.
5. ✔ Explicit write access lapses on its own: 0x66/1/108 fell back to 0 about 5 s after
   the last request with a 5 s timeout.
6. ✔ Connected (class 3) messaging works; Wireshark dissected all 1,371 captured frames
   without warnings.

Still open:

7. Analog byte order and 2CNT-100 layout: need those modules on the station.
8. Status word bits other than 6 and 13; the rest of the module diagnosis; 0x64/1/118,
   which no fault has set so far (candidates: a short circuit on a DO, a wire break on a
   DIAG module, a parameter error).

Answered with signals on the bench (2026-10-13):

* ✔ 16-channel bit order: 24 V on DI channel 8 → module bytes `00 01`; the coupler web
  server showed channel 8 = 1. Channel n = bit n%8 of byte n/8.
* ✔ Explicit output write: DO channel 8 on → assembly 112 read back `00 00 00 01`, and an
  LED wired to DO channel 8 lit; off → zeros.
* ✔ Module pulled (16DO, slot 2): status word 0x0000 → 0x0040; module list entry 2 → 0;
  0x64/1/118 briefly failed with 0x1F (vendor specific); identity status stayed 0x0034;
  DI data kept updating. All recovered about 8 s later.

Diagnosis experiments (2026-10-14, read-only, `ur20eip raw get` snapshots before, twice
during, and after each fault):

* ✔ **Uout disconnected** (web server: "Output power supply error" on the coupler): status
  word and 0x64/1/104 `0x0000 → 0x2000` (bit 13); 16DO diagnosis byte 0 `00 → 15`, bytes
  43–46 `00 00 07 09 → 63 ae ef 56`; 0x64/1/108 empty → `02 82 15 0f`. 16DI (on Uin)
  unchanged. Stable for 20 s. After Uout returned everything cleared without any
  acknowledgement, except bytes 43–46, which took another new value (`6a 70 39 a3`).
* ✔ **16DO pulled from slot 2** (web: "Module removed", "Configuration Mismatch",
  "EIPModMismatch"): status word `0x0040` (bit 6, as on 2026-10-13); 0x64/1/104 `40 00`;
  module list entry 2 → 0; slot 2 101/104/113/114/116/117 → 0 bytes, 109/110 → 0/16;
  0x64/1/108 `02 80 00 0f`; slot 3 still 0x05. Everything cleared after re-insertion; the
  new module's bytes 43–46 read `00 00 07 09` again (power-up value).
* In both: 0x64/1/118 stayed `00 × 8`, process alarms stayed zero, identity status 0x0034.
  "EIPNoConnection" is always shown while no PLC holds a class 1 connection.
* Found and fixed by this: `verify_station` passed with the 16DO pulled (no ID to compare,
  and 0/16 bits still matched). An empty module ID now reads as 0 = no module.
* Healthy module diagnosis is **not** all zeros: byte 1 = `0f`; byte 4 = `70` (16DI) /
  `72` (16DO). Together with byte 0 = `15` on Uout loss, this fits the S7 DS0/DS1 record
  layout (module fault / external fault / missing auxiliary voltage; channel type 70/72).
  That is a hypothesis; only the observed bits are named in code.
