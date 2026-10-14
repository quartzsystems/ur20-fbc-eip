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

### Changed

- The examples read the coupler address from `UR20_EIP_ADDR` and the module list from
  `UR20_EIP_MODULES`, or detect the modules from the coupler, the same way the CLI does.

### Fixed

- The workspace builds on its declared MSRV (Rust 1.85) again: removed `let` chains, which
  need 1.88.

## 0.1.0 (unreleased)

Phases 1 and 2: EtherNet/IP encapsulation, CIP explicit messaging (UCMM and class 3),
Identity Object, assembly reads and writes, module codecs, auto-detection of modules
(Gateway 0x64/1/111), station verification (Slot class 0x65) and the `ur20eip` CLI.
