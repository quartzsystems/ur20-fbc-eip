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

Important consequence: **the modules follow the output assembly only while 0x66/1/108 = 1**
(explicit write access), which per QSG0028 is refused while a PLC holds an Exclusive Owner
connection. Measured on 2026-10-14 (§7): without access, a write to 112 is *accepted and
ignored* (no error; 112 reads back the new bytes, the module's 0x65/n/114 does not change).
When access lapses, the module outputs drop to 0 while 112 keeps the stale image. So 112 shows
what was last written, and 0x65/n/114 shows what is driven. `Coupler` exposes this as
`enable_explicit_outputs(timeout)` and re-asserts access before writes when it may have
lapsed.

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

Lifecycle: there is no async `Drop`, so **call `close()`**. A client that is only dropped
sends UnRegisterSession with a synchronous non-blocking `try_write` (works without a runtime
context and during runtime shutdown; skipped if the stream is broken) and closes the socket;
the target then drops any class 3 connection of that session. After a timeout or framing
error the client is `is_broken()` and refuses requests with `Error::Broken`;
`reconnect()` rebuilds the TCP connection and session in place (same peer and timeout) and
re-opens class 3 if it was open. On failure the client is left unchanged, so it can be retried.

```rust
if c.is_broken() { c.reconnect().await?; }
```

`CipService` is a trait (`send` + provided `get_attribute_single` etc.) so the coupler can be
tested against an in-memory mock. Errors are a single `cip::Error` (thiserror) carrying
encapsulation status, CIP general/extended status, timeouts and malformed-frame details.

**Class 1 hook:** `ConnectionParams` already carries `TransportClass`, trigger, per-direction
`NetworkParams` (size, fixed/variable, priority, P2P/multicast) and RPI, and its connection
path can contain configuration/consumed/produced connection points. `ForwardOpenReply` keeps
any sockaddr CPF items. `io.rs` defines `RunIdleHeader` and `SequenceCount` types. Phase 3
adds an `ImplicitConnection` that owns a UDP socket on 2222.

**Phase 3, M1 (implemented, mock-tested only):** `io::IoPacket` encodes and decodes class 1
datagrams: a bare CPF packet with no encapsulation header, holding a sequenced address
item, then connected data (sequence count, run/idle header O→T only, data).
`ImplicitConnection` runs one connection:

```rust
let params = ConnectionParams::implicit_class1(1, 112, 111, out_len, in_len, rpi); // P2P T→O
let io = ImplicitConnection::open(&mut client, params, ImplicitOptions::default()).await?;
let mut inputs = io.inputs();          // watch::Receiver<Option<InputFrame>>
io.set_outputs(&bytes)?;               // exact O→T data length
io.set_run(true);                      // starts idle unless ImplicitOptions::run
io.state();                            // WaitingForData → Running → TimedOut / Closed / Failed
io.counters();                         // sent / received / unchanged / stale / rejected
io.close(&mut client).await?;          // stop task + Forward_Close; drop only stops the task
```

* It binds UDP before Forward_Open. On a port other than 2222 it sends a sockaddr T→O item
  naming the port. O→T goes to the target IP, on the reply's O→T sockaddr port or 2222.
* The task sends O→T every actual packet interval from the reply. It accepts T→O only from
  the target IP with our T→O connection ID and the right size. Stale or duplicate sequence
  counts are dropped but still reset the watchdog. With no valid T→O for RPI × 4 × 2^mult
  (default multiplier 2, i.e. 16 × RPI) the state becomes `TimedOut` and the task ends.
* Windows' `ConnectionReset` on `recv_from` (an ICMP "port unreachable" from an earlier send)
  is ignored.
* Both sequence numbers advance with every O→T packet.
* A multicast T→O group from the reply is joined. That is not tried on hardware.

**Phase 3, M2 (implemented, mock-tested only):** the `Coupler` owns the class 1
connection, so the output methods can route into it. While it is open:

* `set_output`, `set_outputs`, `write_slot_outputs` and `write_outputs` edit the O→T image
  and send no request; `read_outputs` returns that image.
* `write_output_assembly` and `enable_explicit_outputs` fail with `Error::ImplicitOpen`,
  because the coupler would ignore explicit output writes anyway.
* Explicit reads keep working.
* `reconnect` drops the class 1 connection; `close` closes it.

The CLI's `ur20eip io` opens a connection, prints input changes and can drive outputs with
`--set … --run`.

Phase 3 design notes (collected while doing phases 1–2; M1 and M2 follow them):

* **Forward_Open for "Exclusive Owner Unpacked":** path `20 04 24 01 2C 70 2C 6F`
  (configuration instance 1, O→T point 112, T→O point 111; §7 item 4). O→T size = 112 length
  + 4 bytes 32-bit run/idle header; T→O size = 111 length (the 2-byte status word header is
  part of the assembly, not a CIP header). Input Only / Listen Only use O→T points 0xC6 /
  0xC7 with no data. Size both from `Station` so a station change fails at Forward_Open
  (expect general status 0x01 / extended 0x0109 "invalid connection size") rather than
  later.
* **Decode with what exists:** each T→O packet is the 111 bytes, so
  `Coupler::decode_input_assembly` and `encode_output_assembly` work unchanged. The
  `ImplicitConnection` should hand out raw bytes plus sequence count and let the caller pick
  the codec layer, which keeps `cip` free of u-remote knowledge.
* **Explicit and implicit are exclusive for outputs:** while an Exclusive Owner connection
  is up, the coupler refuses explicit write access (0x66/1/108, status 0x1F) and rejects
  explicit writes to 112 (0x0C, object state conflict), unlike the silent ignore without
  access (§7, 2026-10-15). The coupler type refuses both locally and routes writes into
  the next O→T packet.
* **Fail-safe:** confirmed with the LED on DO channel 8 (§7): idle mode, a connection
  timeout and Forward_Close all switch the outputs off. Under class 1 idle, 0x65/n/114 is
  **not** the physical output (it keeps the last run values).
* **Still to verify on the bench:** multicast T→O. Module pull, cable pull and input
  changes were verified on 2026-10-15 (§7).
* **Runtime shape:** one tokio task per connection that owns the UDP socket and a
  `tokio::time::interval(rpi)` for O→T, sharing the latest T→O image through a
  `tokio::sync::watch` channel and taking outputs through a `watch` or `mpsc`. Close with
  Forward_Close over the existing TCP session. Drop has the same limits as `Client` (§4
  lifecycle).

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
cpl.keep_explicit_outputs_alive().await?;        // re-assert 0x66/1/108 after ≥ ¾ timeout idle
if cpl.is_broken() { cpl.reconnect().await?; }   // Coupler<Client>: keeps layout + station,
                                                 // re-requests write access if it was enabled

// class 1 (Coupler<Client>): sizes from one read of 111/112, checked against the station
cpl.open_implicit(Duration::from_millis(20), ImplicitOptions::default()).await?;
let img = cpl.next_implicit_inputs().await?;     // InputImage from the next T→O frame
cpl.set_output(2, 8, Bit(true)).await?;          // goes into O→T, no request; likewise
                                                 // set_outputs / write_slot_outputs / write_outputs
cpl.set_implicit_run(true)?;                     // starts idle
cpl.close_implicit().await?;                     // Forward_Close; outputs back to explicit
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
  watch        --modules … --interval 250ms [--reconnect] [--write-access 2s]
                                             print changed input channels
  set <slot> <channel> <value> --modules … [--write-timeout 5s] [--no-enable]
                                             read-modify-write one output channel
  set-many <slot:ch=value>… --modules … [--write-timeout 5s] [--no-enable]
                                             several channels, one read + one write
  io [--rpi 20ms] [--set S:C=V]… [--run] [--duration 10s] [--reconnect] [--local-port 2222]
     [--t-o-run-idle]
                                             class 1 connection, print input changes
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

Answered with the DI/DO write test and raw probes (2026-10-14, 2 s explicit-write timeout):

* ✔ All output write paths drive the module: `set_outputs` (one write, ch 0 + 15 →
  112 `00 00 01 80`, 0x65/2/114 `01 80`), `write_slot_outputs` (16 steps, one write each)
  and `read_outputs` → `OutputImage::set` → `write_outputs`. Each was checked against
  0x65/2/114, not only the 112 readback.
* ✔ **Writes without access are accepted and ignored.** With 0x66/1/108 = 0, Set_Attribute
  on 0x04/112/3 returns success and 112 reads back the new bytes, but 0x65/2/114 stays
  `00 00`. With access, 114 follows at once. (Contradicts the earlier assumption, taken from
  the QSG, that such writes are rejected; the mock coupler now models this.)
* ✔ **Lapse is fail-safe but 112 is stale.** 2 s timeout, DO ch 8 on, 4 s idle: 108 → 0,
  0x65/2/114 → `00 00`, 112 still `00 00 00 01`. Setting 108 = 1 again does not re-apply
  112; the next write does. Confirmed with the LED on DO channel 8 (see below).
* ✔ Access is not tied to the session: enabling it in one TCP session (one `ur20eip raw set`
  call) and writing from another drove the module.
* ✔ Keepalive: writes more than the timeout apart (2.5 s with a 2 s timeout) are all
  driven, because `Coupler` re-asserts 107/108 first. The first run of the write test found
  a bug here: a read after the lapse reset the idle clock, so the next write skipped the
  re-assert and was silently ignored. Fixed by remembering any gap of ≥ ¾ timeout until the
  next re-assert.
* ✔ Read-only hardware tests (identity unconnected/connected, assemblies and slots,
  configured station, diagnosis) all pass against the bench station.

Class 1 implicit I/O (2026-10-15, `ur20eip io`, the `class1_check` example and the
`class1_*` hardware tests; LED on DO channel 8):

* ✔ **Forward_Open "Exclusive Owner Unpacked" accepted** with point-to-point T→O: path
  `20 04 24 01 2C 70 2C 6F`, O→T 4 + 6 bytes, T→O 4 + 2 bytes (no run/idle header). The reply
  carried an O→T sockaddr `0.0.0.0:2222` and no T→O sockaddr. Actual intervals equal the
  RPI. **RPI 1 ms, 2 ms, 5 ms, 10 ms, 20 ms all accepted**, and 1 ms ran for 3 s from this
  Windows PC without a timeout.
* ✔ The web server's "EIPNoConnection" clears while the connection is up. The status word
  stays `0x0000` and 0x66/1/108 stays 0.
* ✔ **T→O sequence count:** the coupler repeats the same count while its data is unchanged
  (1 new frame, then only repeats over 10 s), so "unchanged" packets are the normal
  heartbeat, counted apart from out-of-order ones.
* ✔ **Run/idle:** idle (from the start) → LED off, 114 `00 00`. Run → LED on, 114 `00 01`.
  Idle again → **LED off, but 114 stays `00 01`**. So under class 1, 114 is the last data
  applied in run mode, not the physical output.
* ✔ **Explicit writes while class 1 owns the outputs:** 0x66/1/108 := 1 → 0x1F; 0x04/112/3
  := zeros → **0x0C** (object state conflict), 114 unchanged.
* ✔ **Fail-safe:** O→T stopped without Forward_Close → 114 `00 00` and LED off 0.32 s later
  (timeout 16 × 20 ms). Forward_Close → off within 0.16 s. Re-opening right after a
  timed-out connection works.
* ✔ **Input changes over T→O:** 24 V pulses on DI ch 0, some only 80–140 ms long, each
  showed up as a new frame (sequence count advanced), on both edges.
* ✔ **Cable pull** (`ur20eip io --reconnect --set 2:8=on --run`): the connection timed out
  0.3 s after the pull, and the LED went off. The first reopen ran Forward_Open on the dead
  TCP session and hit the 3 s request timeout, which marks the client broken. TCP reconnects
  then retried with backoff (0.5 → 4 s) until the cable was back. After that came a new
  session, a new class 1 connection, and ch 8 on again with no help.
* ✔ **Module pull with class 1 up:** the coupler does **not** end the connection. T→O and
  O→T continue, and the DI module's inputs keep arriving. The status word in each T→O frame
  shows `0x0040`. When the DO module goes back in, it **picks up the current O→T outputs on
  its own** (LED on again, no reopen). So with class 1, a module inserted into a running
  connection is driven immediately.
* Status word during that pull: `0x0040` (out) → `0x0000` (insertion that did not seat) →
  `0x0040` (pulled again) → **`0x0048` for 0.8 s** → `0x0040` → `0x0000` (final
  insertion). Bit 3 appeared only during the final, successful insertion; unnamed, see 8.

Still open:

7. Analog byte order and 2CNT-100 layout: need those modules on the station.
8. Status word bits other than 6 and 13 (bit 3 was set for 0.8 s while a module was being
   inserted; one observation, so it has no name yet); the rest of the module diagnosis;
   0x64/1/118, which no fault has set so far (candidates: a short circuit on a DO, a wire
   break on a DIAG module, a parameter error).

Answered on the bench (2026-10-14, LED on DO channel 8, raw `ur20eip` calls, 2 s timeout):

* ✔ **0x65/n/114 is the physical output.** The LED matched 114 in every phase: write ch 8
  without access → 112 `00 00 00 01`, 114 `00 00`, LED off. Access on + write → 114 `00 01`,
  LED on. Traffic stopped → LED off about 2 s later, 108 `00`, 114 `00 00`, 112 still
  `00 00 00 01`. 108 set again without a write → LED stays off. Write off + access off →
  off.
* ✔ **Reconnect on real hardware.** `ur20eip watch -i 500ms --reconnect --write-access 2s`,
  Ethernet cable pulled for about 25 s: "connection lost: no reply within 3s" at 12.0 s,
  "reconnected" at 36.5 s (retry delay 0.5 → 5 s). Afterwards 0x66/1/108 = 01 and 107 = 2 s,
  still 01 after 4 s with no other client talking, so the reconnected session holds write
  access by itself. `diag` clean (status word 0x0000, both slots ok).

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
