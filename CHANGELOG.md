# Changelog

All notable changes to this workspace are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versions follow
[Semantic Versioning](https://semver.org/spec/v2.0.0.html). The three crates
(`ur20-fbc-eip-cip`, `ur20-fbc-eip`, `ur20-fbc-eip-cli`) share one version.

## [Unreleased]

### Added

- `CONTRIBUTING.md`, this changelog, and GitHub Actions CI (fmt, clippy, test, doc on stable
  and on the 1.85 MSRV).
- `repository` metadata for all crates.
- Batch output writes: `Coupler::set_outputs(&[(slot, channel, value)])` and
  `Coupler::write_slot_outputs(slot, &values)` (one read and one write, all or nothing),
  and `Coupler::write_outputs(&OutputImage)` (one write, no read) with `OutputImage::set` /
  `OutputImage::value`. `Coupler::decode_output_assembly` / `encode_output_assembly`,
  `Station::set_outputs`, `Station::encode_slot_outputs`, `Station::encode_outputs`.
- `ur20eip set-many 2:0=on 2:1=off …`.
- Hardware test `digital_writes_outputs`, gated by `UR20_EIP_ALLOW_WRITES=1`.
- Diagnosis (`ur20_fbc_eip::diagnosis`): `Coupler::diagnosis()` → `CouplerDiagnosis`
  (status word from 0x64/1/104, latest event 0x64/1/108, unacknowledged diagnosis
  0x64/1/118), `Coupler::slot_diagnosis(slot)` / `slot_diagnoses()` → `SlotDiagnosis`
  (0x65/n/116 and 117). Named only where confirmed on the bench: `StatusWord::MODULE_REMOVED`
  (bit 6), `StatusWord::OUTPUT_SUPPLY_ERROR` (bit 13), `ModuleDiagnosis::has_error` /
  `output_supply_missing` (byte 0); everything else is raw. `InputImage::status()`,
  `Error::cip_status()`, `Error::is_no_slot()`, `CouplerLayout::diagnosis`, and
  `SlotClass::diagnosis_attribute` / `process_alarm_attribute`.
- `ur20eip diag [--slot N] [--raw]`.
- `Client::reconnect()` / `Client::is_broken()`, and `Coupler::reconnect()` /
  `Coupler::is_broken()` / `Coupler::close()` for `Coupler<Client>`. Reconnect keeps the
  layout and station, re-opens class 3 and re-requests explicit write access if it was on.
- Explicit write access is kept alive: the coupler tracks its last request and re-asserts
  0x66/1/107+108 before output writes after ≥ ¾ of the timeout idle.
  `Coupler::keep_explicit_outputs_alive()`, `disable_explicit_outputs()`,
  `explicit_outputs_timeout()`, `idle_time()`.
- `ur20eip watch --reconnect` and `--write-access TIMEOUT`.

### Changed

- `OutputImage` has a `trailer` field (bytes after the configured modules), which
  `write_outputs` writes back unchanged.
- The `do_chase` example writes the whole DO module once per step.
- The examples read the coupler address from `UR20_EIP_ADDR` and the module list from
  `UR20_EIP_MODULES`, or detect the modules from the coupler, the same way the CLI does.

### Fixed

- Explicit write access keepalive: a read after a lapse no longer hides it. Any gap of
  ≥ ¾ of the timeout since access was last asserted makes the next output write re-assert
  it first. (Found by the hardware write test: the write after the lapse was silently
  ignored by the coupler.)
- Docs and the mock coupler assumed output writes without explicit access fail with 0x0F.
  On hardware they are accepted and ignored, and a lapse switches the module outputs off
  while the output assembly keeps the old image. `Coupler` now logs a warning when outputs
  are written without access requested through it.
- A dropped `Client` (or `Coupler`) now sends UnRegisterSession instead of leaving the
  session to time out on the coupler; `close()` is still the clean way.
- `verify_station` passed with a module pulled: the coupler keeps the slot and its old bit
  sizes, so only the module ID shows it. An empty module ID is now reported as 0 (no module)
  and fails verification.
- The workspace builds on its declared MSRV (Rust 1.85) again: removed `let` chains, which
  need 1.88.

## 0.1.0 (unreleased)

Phases 1 and 2: EtherNet/IP encapsulation, CIP explicit messaging (UCMM and class 3),
Identity Object, assembly reads and writes, module codecs, auto-detection of modules
(Gateway 0x64/1/111), station verification (Slot class 0x65) and the `ur20eip` CLI.
