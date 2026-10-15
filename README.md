# ur20-fbc-eip

Rust driver for the **Weidmüller u-remote UR20-FBC-EIP** EtherNet/IP fieldbus coupler and the
UR20 IP20 I/O modules plugged into it. Async on tokio. Comes with a CLI, `ur20eip`, for
bench bring-up.

> Status: early. CIP explicit messaging, identity, assembly reads, module auto-detection and
> station verification have run against a real UR20-FBC-EIP (read-only). Output writes and most
> module codecs are tested against fixtures and mocks only. Items marked
> **TODO-verify-on-hardware** in the code are still assumptions (see
> [docs/DESIGN.md §7](docs/DESIGN.md)). Class 1 implicit I/O (`ImplicitConnection`) is
> implemented in the CIP crate and tested against a mock adapter, not yet on hardware.

## FBC-EIP vs. FBC-MOD-TCP

u-remote stations have the same I/O modules and the same per-module process data on every
coupler. Only the fieldbus between your PC/PLC and the coupler differs:

| | UR20-FBC-MOD-TCP | **UR20-FBC-EIP** |
|---|---|---|
| Protocol | Modbus TCP, port 502 | EtherNet/IP (CIP), TCP 44818 / UDP 2222 |
| Process data | holding/input registers | Assembly Object (class 0x04) instances |
| Inputs | read input registers | Get_Attribute_Single 0x04/111/3 |
| Outputs | write holding registers | Set_Attribute_Single 0x04/112/3, after enabling explicit write access |
| Cyclic I/O | polling | class 1 connection with an RPI (planned) |
| Rust crate | [`ur20`](https://crates.io/crates/ur20) (slowtec) | **this workspace** |

If your coupler says MOD-TCP on the front, use slowtec's `ur20`. This workspace mirrors its
module model (a `Module` trait, sizes per module, process image carved per slot) but is an
independent implementation.

## Layering

```
ur20eip (CLI)                       crates/cli
   │
ur20-fbc-eip                        crates/ur20-fbc-eip
   Coupler<S: CipService>           reads/writes assemblies, enables explicit writes,
   │                                detects modules (0x64), verifies slots (0x65)
   ├─ CouplerLayout                 which instances/headers; defaults from Weidmüller docs
   │                                and bench reads, overridable
   ├─ Station                       module list → byte offsets in the process images
   └─ Module trait + modules/       pure codecs: bytes ⇄ ChannelValue, no I/O
   │
ur20-fbc-eip-cip                    crates/cip
   Client / CipService              Get/Set_Attribute_Single, Get_Attributes_All, any service
   connection                       Forward_Open/Close (class 3 now, class 1 ready)
   message, epath                   Message Router request/response, EPATH
   cpf, encap                       Common Packet Format, 24-byte encapsulation, sessions
```

The CIP crate knows nothing about u-remote and works with any EtherNet/IP adapter. It does
not use `rseip`; see [docs/DESIGN.md §2](docs/DESIGN.md) for why.

## Quick start (library)

```rust,no_run
use std::time::Duration;
use ur20_fbc_eip::{ChannelValue, Coupler, CouplerLayout, modules};

#[tokio::main]
async fn main() -> ur20_fbc_eip::Result<()> {
    // Module list read from the coupler (Gateway 0x64/1/111).
    let mut c = Coupler::connect_auto("192.168.1.10:44818", CouplerLayout::default()).await?;
    // Or state it and check it against the hardware:
    // let modules = modules::create_all(&["UR20-4DI-P", "UR20-4DO-P"])?;
    // let mut c = Coupler::connect(addr, CouplerLayout::default(), modules).await?;
    // c.verify_station().await?;

    let inputs = c.read_inputs().await?;
    println!("status word {:?}", inputs.status_word);
    for slot in inputs.slots {
        println!("slot {} {}: {:?}", slot.position, slot.module, slot.values);
    }

    // Outputs follow writes only with explicit write access; without it the coupler accepts
    // and ignores them. Kept alive automatically. Refused while a PLC owns the outputs.
    c.enable_explicit_outputs(Duration::from_secs(5)).await?;
    c.set_output(2, 0, ChannelValue::Bit(true)).await?;
    // Several channels in one read-modify-write:
    c.set_outputs(&[(2, 1, ChannelValue::Bit(true)), (2, 2, ChannelValue::Bit(false))]).await?;
    // Or edit the whole image and write it back in one request:
    let mut out = c.read_outputs().await?;
    out.set(2, 3, ChannelValue::Bit(true))?;
    c.write_outputs(&out).await?;
    Ok(())
}
```

## Quick start (CLI)

```sh
cargo install --path crates/cli
export UR20_EIP_ADDR=192.168.1.10
# optional; detected from the coupler if unset:
# export UR20_EIP_MODULES=UR20-4DI-P,UR20-4DO-P,UR20-4AI-UI-16

ur20eip identity
ur20eip slots                      # module name, ID and bit sizes per slot
ur20eip dump-inputs --raw
ur20eip dump-outputs
ur20eip watch --interval 100ms
ur20eip watch --reconnect --write-access 2s   # survive coupler reboots, hold write access
ur20eip io --rpi 20ms              # class 1 connection, print input changes (outputs idle)
ur20eip io --set 2:8=on --run      # ... and drive DO ch 8 through O→T
ur20eip set 2 0 on                 # slot 2, channel 0
ur20eip set-many 2:0=on 2:1=off    # one read and one write, all or nothing
ur20eip diag                       # status word, coupler and per-slot diagnosis
ur20eip raw get 0x04 111 3
ur20eip raw get-all 0x01 1
ur20eip raw set 0x66 1 108 01
ur20eip raw scan 0x64 1 101 130    # which gateway attributes exist?
ur20eip raw service 0x0e 0x65 1 --attribute 101
```

`--connected` uses a class 3 connection instead of unconnected messages; `--timeout-ms`
sets the per-request timeout (default 3000). `--modules`/`UR20_EIP_MODULES` is checked against
the coupler unless you pass `--no-verify`. `set` requests explicit write access first
(`--write-timeout 5s`; `--no-enable` skips that). `ur20eip help <command>` lists everything.
`UR20_EIP_LOG=trace` prints every request and reply in hex.

## Supported modules

| Module | Inputs | Outputs | Notes |
|---|---|---|---|
| UR20-4DI-P, UR20-4DI-P-3W | 4 × Bit | – | 1 byte, upper nibble padding |
| UR20-8DI-P-2W, UR20-8DI-P-3W | 8 × Bit | – | |
| UR20-16DI-P | 16 × Bit | – | |
| UR20-4DO-P, UR20-4DO-P-2A, UR20-4RO-CO-255 | – | 4 × Bit | |
| UR20-8DO-P | – | 8 × Bit | |
| UR20-16DO-P | – | 16 × Bit | |
| UR20-4AI-UI-16(-DIAG) | 4 × Int16 / Float | – | Float with `AnalogInput::with_scaling` |
| UR20-4AO-UI-16(-DIAG) | – | 4 × Int16 / Float | |
| UR20-2CNT-100 | 2 × Int32 + 2 × UInt16 | 2 × Int32 + 2 × UInt16 | layout unverified |

Any other u-remote module returns `Error::UnsupportedModule`; nothing panics on device data.
Adding one is a single file; see [CONTRIBUTING.md](CONTRIBUTING.md).

## Testing

```sh
cargo test --workspace                       # codecs, mock adapter, mock coupler
UR20_EIP_ADDR=192.168.1.10 \
  cargo test --workspace -- --ignored --nocapture   # real hardware, read-only
```

## Sources

* Weidmüller, *UR20-FBC-EIP Quick Start Guide: Commissioning with Allen-Bradley MicroLogix 1400*
  (QSG0028v3), chapter 3: object model, assembly instances, explicit write enable.
* Weidmüller, *AN0050v3: Importing module data from the u-remote web server into Studio 5000*:
  the input header is the coupler status word.
* Weidmüller, *Remote-I/O-System u-remote Manual* (1432790000): module process data.
* [slowtec/ur20](https://github.com/slowtec/ur20): module ID table and module model.

## License

Copyright (C) 2026 Quartz Systems. Licensed under the [MIT license](LICENSE).

Not affiliated with Weidmüller; "u-remote" is a trademark of Weidmüller Interface
GmbH & Co. KG.
