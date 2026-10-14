# Contributing

Thanks for helping. This workspace talks to real industrial hardware, so the main rule is
simple: **say how you know.** Every byte layout is either confirmed on a coupler, quoted from a
Weidmüller document, or marked `TODO-verify-on-hardware`.

## Before you open a pull request

CI runs these on stable and on the MSRV (Rust 1.85, `rust-version` in `Cargo.toml`). Run
them locally first:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps
cargo +1.85 test --workspace          # rustup toolchain install 1.85
```

The workspace lints are strict on purpose. `unsafe` is forbidden; `unwrap`, `expect`, `panic!`
and slice indexing produce warnings, and CI denies warnings. Bytes from the device must never
panic the driver, so use `.get()`, `codec::Reader` and the `check_len` / `check_count`
helpers. Tests may opt out with `#![allow(clippy::unwrap_used, …)]`. Every public item needs
a doc comment (`missing_docs`). Don't use language features newer than 1.85, such as `let`
chains.

Add a line under `## [Unreleased]` in [CHANGELOG.md](CHANGELOG.md) for anything a user
would notice.

## Adding a module

A module is a pure codec: it gets exactly its own bytes of the process image and turns them
into `ChannelValue`s and back. It does no I/O. `Station` works out the offsets from
`input_bits()` / `output_bits()`.

1. **Check that it isn't already covered.** Many modules share a family codec. A new 8-channel
   DI only needs one more `di!(…)` line in `modules/digital_in.rs` and an entry in that
   file's `DESCRIPTORS`. Skip to step 4.

2. **Create one file** in `crates/ur20-fbc-eip/src/modules/`, e.g. `pwm.rs`:

   ```rust
   //! UR20-2PWM-PN-0.5A: … (say where the layout comes from: manual section, or
   //! "verified on hardware on YYYY-MM-DD", or TODO-verify-on-hardware)

   use crate::error::Result;
   use crate::module::{
       Category, ChannelValue, Module, ModuleDescriptor, ValueKind, check_count, check_len,
   };

   /// Codec for the PWM modules.
   #[derive(Debug)]
   pub struct Pwm {
       descriptor: &'static ModuleDescriptor,
   }

   fn create(d: &'static ModuleDescriptor) -> Box<dyn Module> {
       Box::new(Pwm { descriptor: d })
   }

   /// `UR20-2PWM-PN-0.5A`: 2 PWM outputs.
   pub static UR20_2PWM_PN_0_5A: ModuleDescriptor = ModuleDescriptor {
       name: "UR20-2PWM-PN-0.5A",
       id: 0x0908_4880, // must match KNOWN_MODULES
       category: Category::AnalogOutput,
       channels: 2,
       create,
   };

   pub(crate) static DESCRIPTORS: &[&ModuleDescriptor] = &[&UR20_2PWM_PN_0_5A];

   impl Module for Pwm {
       fn descriptor(&self) -> &'static ModuleDescriptor { self.descriptor }
       fn input_bits(&self) -> usize { /* from the manual */ }
       fn output_bits(&self) -> usize { /* from the manual */ }
       // input_channels, output_channels, output_kind,
       // decode_inputs, decode_outputs, encode_outputs
   }
   ```

   Only `encode_outputs` is required for writing. The provided `encode_output_channel`
   decodes the module's outputs, replaces one value and re-encodes them, so single-channel
   writes work automatically.

3. **Register the file** by adding its name to the `register_modules!` list in
   [`modules/mod.rs`](crates/ur20-fbc-eip/src/modules/mod.rs):

   ```rust
   register_modules! {
       digital_in,
       digital_out,
       analog_in,
       analog_out,
       counter,
       pwm,
   }
   ```

   The macro declares `pub mod pwm;` and adds `pwm::DESCRIPTORS` to `modules::all()`, which
   `by_name`, `by_id`, `create_all` and `Coupler::detect` all use. Nothing else needs editing.

4. **Check the module ID** against `KNOWN_MODULES` in the same file. If the module is
   missing there, add it with its source. `registry_is_consistent` fails on a name/ID pair that
   isn't in the table, and it also checks that every registered module decodes zeros,
   rejects wrong lengths without panicking, and round-trips its outputs.

5. **Write tests** in the new file (`#[cfg(test)] mod tests`) using the fixture conventions
   below. Add a row to the *Supported modules* table in the README.

## Fixture conventions

Fixtures are byte arrays in tests. Every one says where its bytes came from:

* **From hardware:** a comment with the date, the station and what was applied, e.g.
  `// Gateway 0x64/1/111 from a UR20-FBC-EIP with UR20-16DI-P + UR20-16DO-P (2026-10-13).`
  or `// 24 V on DI channel 8 → module bytes 00 01 (2026-10-13)`. Paste the bytes exactly as
  `ur20eip raw get …` printed them, header included where relevant.
* **From a document:** cite it (`QSG0028 §3.2`, `u-remote manual 1432790000 p. …`).
* **Invented:** mark it `// TODO-verify-on-hardware: …` and say what is assumed (for
  example "little-endian INTs").

Keep fixtures as small as possible, comment byte groups (`// 4AI: 27648, 0, 0, 1`) and use
`#[rustfmt::skip]` on the array so the comments stay aligned. Unit tests of codecs go next to
the codec. Tests of coupler logic go in `crates/ur20-fbc-eip/tests/coupler_mock.rs`, which
models the coupler's objects as a `(class, instance, attribute) → bytes` map and answers the
way the real coupler does (0x05 past the last slot, 0x0F for output writes before explicit
write access). Transport tests go in `crates/cip/tests/mock_adapter.rs`, which runs an
in-process TCP adapter.

## The `TODO-verify-on-hardware` marker

Use this exact string, so `grep -rn TODO-verify-on-hardware` finds every assumption. Put it:

* in the doc comment of anything whose layout or meaning is a guess, saying what the guess is;
* on invented test fixtures;
* in [docs/DESIGN.md](docs/DESIGN.md) §3 and §7, which list the open questions.

Remove a marker only after confirming the fact on a real coupler. Then add a hardware fixture
(see above) and move the item to "answered" in DESIGN.md §7 with the date and how you checked
it. If you only confirmed part of it, for example a size but not the meaning of each bit,
keep the marker on the rest.

## Hardware tests

Tests that need a coupler are `#[ignore]`d, so `cargo test` stays offline. To run them:

```sh
export UR20_EIP_ADDR=192.168.1.10          # IP or IP:PORT, port 44818 by default
export UR20_EIP_MODULES=UR20-16DI-P,UR20-16DO-P   # optional; detected from the coupler if unset
cargo test --workspace -- --ignored --nocapture
```

On PowerShell use `$env:UR20_EIP_ADDR = "192.168.1.10"`.

These tests are **read-only**, except those whose name contains `writes_outputs`. Those
switch real outputs and do nothing unless `UR20_EIP_ALLOW_WRITES=1` is also set, so
`--ignored` alone never drives an output. Set it only when nothing dangerous is wired to the
DO module. A PLC holding an Exclusive Owner connection makes them fail with a privilege
violation (0x0F); that is expected. Any new test that writes outputs must follow the same
naming and gating.

Run one crate or one test with
`cargo test -p ur20-fbc-eip --test hardware -- --ignored --nocapture <name>`. Set
`UR20_EIP_LOG=trace` with the CLI to see every request and reply in hex. For bring-up of new
objects, `ur20eip raw scan <class> <instance> <first> <last>` lists the attributes that exist.

## Licence

By contributing you agree that your work is licensed under the [MIT license](LICENSE).
