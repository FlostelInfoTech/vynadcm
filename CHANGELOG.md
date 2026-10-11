# Changelog

All notable changes to VynaDCM are documented here. Versions follow
the workspace crate version (`Cargo.toml`).

## [Unreleased]

## [0.1.0] — 2026-10-11

Initial release: from-scratch DICOM workspace for VynaECG and VynaLink.

### Added

- `vyna-dcm-core`: tags, 34 VRs, datasets, 266-entry PS3.6
  dictionary, UIDs, 16 transfer syntaxes, PersonName/DA/TM.
- `vyna-dcm-codec`: dataset codec (implicit/explicit LE, explicit BE,
  deflated), Part-10 files, from-scratch RLE, DICOM JSON Model,
  pixel helpers + `PixelCodec` plug-in trait, text dumps.
- `vyna-dcm-net`: PDU/association layer, DIMSE SCU (echo, store,
  find, move, get, all N-services) and SCP (`Handlers` dispatch,
  negotiation gating, C-MOVE routing, C-GET delivery), MWL/MPPS/
  Storage-Commitment builders, TLS + mutual TLS (feature `tls`).
- `vyna-dcm-web`: DICOMweb client (QIDO/WADO/STOW), `multipart/related`
  codec, framework-agnostic server dispatch + `MemoryStore`, hyper
  front end (feature `server`).
- `vyna-dcm-ecg`: 12-lead ECG model, PS3.3 C.10.9 physical conversion,
  Waveform Sequence mapping, SOP instance builder.
- `vyna-dcm` facade, 8 CLI tools
  (`echoscu/scp`, `storescu/scp`, `findscu`, `dcmdump`, `dcm2json`,
  `json2dcm`), and 3 runnable examples.
- Living documents: requirements, design, integration and user
  guides, SBOM (CycloneDX), architecture overview.

### Quality

- 82 unit + 5 doc tests green; `clippy --workspace --all-targets`
  zero warnings; `cargo fmt --check` clean; feature matrix verified.
- Live wire interop: C-ECHO `0x0000` against a third-party endpoint.

### Notes

- Toolchain: Rust 1.98+, edition 2024. License: proprietary.
- JPEG/JPEG 2000 pixel decoding is intentionally out of tree
  (plug-in via `PixelCodec`).
- HTTPS trust uses the OS certificate store; DICOM TLS defaults to
  WebPKI roots with optional private CA.
