# VynaDCM Software Requirements Specification

| Field | Value |
|---|---|
| Document | SRS (living) |
| Version | 0.1.0 |
| Date | 2026-10-11 |
| Status | Living draft — updated with every release |
| Applies to | VynaDCM workspace v0.1.0 (crates `vyna-dcm-*`) |

Revision history: 0.1.0 (2026-10-11) — initial issue, describing the
implemented baseline (125 tests green, IOD validator applied).

## Contents

- [1. Introduction](#1-introduction)
- [2. Overall description](#2-overall-description)
- [3. Functional requirements](#3-functional-requirements)
- [4. Non-functional requirements](#4-non-functional-requirements)
- [5. Out of scope (v0.1)](#5-out-of-scope-v01)
- [Appendix A: Acceptance traceability](#appendix-a-acceptance-traceability)

## 1. Introduction

### 1.1 Purpose

VynaDCM is a from-scratch DICOM library in Rust that replaces the
open-source DICOM dependencies currently used by **VynaECG** (ECG
management) and **VynaLink** (interface engine). This document specifies
the required behavior so that both consumers can rely on a stable,
verified contract.

### 1.2 Scope

In scope: DICOM data model, encoding/decoding, Part-10 files, DICOM
JSON, DIMSE networking (SCU + SCP), DICOMweb client + server, ECG
waveform mapping, and operator CLI tools. Out of scope: §5.

### 1.3 References

- NEMA PS3.3 (IODs), PS3.4 (DIMSE services), PS3.5 (encoding),
  PS3.6 (dictionary), PS3.7 (message exchange), PS3.8 (network),
  PS3.10 (files), PS3.15 (security/TLS), PS3.18 (DICOMweb + JSON Model F)
- `ARCHITECTURE.md`, `docs/design.md`, `docs/integration.md`,
  `docs/user-guide.md`, `docs/sbom.md`, `docs/roadmap.md`

### 1.4 Conventions

Requirements carry IDs (`FR-<area>-<nn>`, `NFR-<nn>`) and a priority:
**M** (must), **S** (should). Each maps to acceptance evidence in
Appendix A (test names or manual procedures).

## 2. Overall description

### 2.1 Product perspective

VynaDCM is a library workspace plus CLI tools. VynaECG embeds the ECG
mapping, codec, and storage/network pieces; VynaLink embeds the
networking, routing (SCP + SCU chaining), and DICOMweb pieces. No DICOM
functionality may come from outside this workspace (NFR-02).

### 2.2 User classes

- **Integrators**: Rust developers embedding VynaDCM in VynaECG/VynaLink.
- **Operators**: staff running the CLI tools for echo/store/query,
  conversion, and troubleshooting.

### 2.3 Constraints

Rust 1.98+, edition 2024, async via tokio, proprietary license
(see NFR-01…NFR-03).

## 3. Functional requirements

### 3.1 Core data model (`vyna-dcm-core`)

- **FR-CORE-01 (M)** Tags: represent `(GGGG,EEEE)` tags with group /
  element accessors, command/delimiter identification, and ordering.
- **FR-CORE-02 (M)** VRs: support all 34 standard DICOM Value Representations
  (plus retired `SI` for legacy files)
  with text/binary classification and fixed-width sizes.
- **FR-CORE-03 (M)** Datasets: ordered element map with typed getters
  (text, numbers, UIDs, sequences, fragments) and builders; value
  multiplicity preserved.
- **FR-CORE-04 (M)** Dictionary: ship the full PS3.6 registry
  (currently 5,239 entries from PS3.6 2026d plus the command group)
  mapping tag → (VR, VM, keyword, name, retired flag), sorted for
  binary search, plus `tags` constants; repeating (`xx`) groups stay
  unlisted.
- **FR-CORE-08 (M)** Private dictionary: resolve private elements by
  creator ID, group, and suffix (currently 424 creators / 8,860
  entries with wildcard fallbacks); unknown creators or suffixes
  resolve to “unknown”, never to a wrong entry.
- **FR-CORE-05 (M)** Transfer syntaxes: model 16 transfer syntax UIDs
  with wire properties (endianness, implicit/explicit VR, deflated,
  encapsulated); default to Explicit VR Little Endian.
- **FR-CORE-06 (M)** UIDs: validate UID strings, generate new UIDs
  (`2.25.*` UUID-based), and provide SOP Class UID constants.
- **FR-CORE-07 (M)** Text values: parse PersonName components and
  validate DA/TM (incl. fractional seconds, optional colons).

### 3.2 Codec, files, pixels, JSON (`vyna-dcm-codec`)

- **FR-CODEC-01 (M)** Decode and encode datasets in Implicit VR Little
  Endian, Explicit VR Little Endian, Explicit VR Big Endian (retired
  but supported), and Deflated Explicit VR Little Endian.
- **FR-CODEC-02 (M)** Resolve context-dependent VRs (`OB/OW`, `US/SS`,
  waveform `OB/OW`) from already-decoded siblings and the dictionary.
- **FR-CODEC-03 (M)** Support defined- and undefined-length sequences
  and items, nested datasets, and encapsulated fragment tables
  (incl. Basic Offset Table).
- **FR-CODEC-04 (M)** Read and write Part-10 files (128-byte preamble +
  `DICM` + group-0002 meta + dataset); reject non-Part-10 input.
- **FR-CODEC-05 (M)** Provide a from-scratch RLE Lossless frame codec
  (PackBits planes + header with segment offsets).
- **FR-CODEC-06 (M)** Convert datasets to/from the DICOM JSON Model
  (PS3.18 F), incl. `InlineBinary` for byte VRs and `DataFragment`
  for encapsulated data.
- **FR-CODEC-07 (M)** Decode native, RLE, and deflated pixel data to
  raw bytes; decode and encode the JPEG families in-tree via the
  bundled `BundledCodec` (`.50` baseline, `.70` lossless, `.80`/`.81`
  JPEG-LS, `.90`/`.91` JPEG 2000, single- and multi-frame). The
  `PixelCodec` trait stays as the override point and errors cleanly
  when no codec is installed; `.51` (12-bit), J2K Part 2, and HTJ2K
  remain unsupported.
- **FR-CODEC-08 (S)** Render human-readable text dumps
  (`(GGGG,EEEE) VR [value] # keyword`).

### 3.3 DIMSE networking (`vyna-dcm-net`)

- **FR-NET-01 (M)** Upper Layer: encode/decode A-ASSOCIATE-RQ/AC/RJ,
  P-DATA (with max-PDU chunking), RELEASE, and ABORT PDUs.
- **FR-NET-02 (M)** SCU: request associations with odd presentation
  context IDs, negotiate transfer syntaxes per context, send/receive
  DIMSE, release/abort.
- **FR-NET-03 (M)** SCU operations: C-ECHO, C-STORE, C-FIND
  (incl. cancelable), C-MOVE, C-GET (with C-STORE sub-operation
  handler), and N-CREATE/SET/GET/ACTION/DELETE/EVENT-REPORT.
- **FR-NET-04 (M)** SCP: accept associations, negotiate with gating on
  AE title (optional strict mode), transfer syntaxes, SOP classes,
  and an optional user-identity callback; dispatch each service to
  application `Handlers`.
- **FR-NET-05 (M)** C-MOVE: route instances to a configured destination
  AE over a dedicated outbound association; C-GET: deliver over the
  same association and abort delivery on C-CANCEL.
- **FR-NET-06 (M)** Status handling: classify statuses
  (success/warning/failure/pending/cancel) and count sub-operations
  (completed/failed/warning) for MOVE/GET responses.
- **FR-NET-07 (M)** Worklist helpers: MWL C-FIND datasets, MPPS
  N-CREATE/N-SET datasets, Storage Commitment N-ACTION/N-EVENT-REPORT
  builders and result parsing.
- **FR-NET-08 (M)** TLS (PS3.15, feature `tls`): TLS acceptors and
  connectors over TCP, mutual TLS, extra-CA trust, on top of the same
  association machinery.

### 3.4 DICOMweb (`vyna-dcm-web`)

- **FR-WEB-01 (M)** Client: QIDO-RS search (studies/series/instances
  with query filters), WADO-RS (file/dataset/metadata/frames/bulkdata),
  STOW-RS upload; Basic and Bearer auth.
- **FR-WEB-02 (M)** `multipart/related` encode/decode from scratch,
  tolerating boundary-like bytes inside bodies.
- **FR-WEB-03 (M)** Server core: route QIDO/WADO/STOW against any
  `DicomWebStore` implementation; ship an in-memory store; expose a
  hyper HTTP/1.1 front end (feature `server`).

### 3.5 ECG mapping (`vyna-dcm-ecg`)

- **FR-ECG-01 (M)** Model 12-lead recordings: per-lead raw samples +
  sensitivity, sampling rate, standard lead labels with MDC codes.
- **FR-ECG-02 (M)** Convert raw samples to physical units per PS3.3
  C.10.9: `physical = sample × sensitivity × correction + baseline`.
- **FR-ECG-03 (M)** Map recordings to/from Waveform Sequence multiplex
  items and build complete 12-Lead/General ECG SOP instances ready
  for C-STORE, Part-10, or STOW-RS.

### 3.6 IOD validation (`vyna-dcm-iod`)

- **FR-IOD-01 (M)** Validate datasets against generated IOD tables
  (18 SOP Classes: CR/CT/MR/MG/DX/US/SC/PT/NM/XA/VL/KO/PDF/ECG):
  Type 1 presence + non-emptiness, Type 2 presence, Type 3
  if-present checks, dictionary VR (or listed alternate) and VM
  conformance, plus a curated set of Type-1C conditions
  (multi-frame, color, lossy syntax, charset, offset tables).
- **FR-IOD-02 (M)** Report machine-readable findings (severity,
  tag, module, category) in tag order; unknown SOP Classes return
  no report instead of guessing.
- **FR-IOD-03 (S)** Scope honesty: sequence items, non-Mandatory
  module presence, uncurated conditions, overlay groups, and value
  *formats* are out of scope and documented as such.

### 3.7 CLI tools (`vyna-dcm`, feature `cli`)

- **FR-CLI-01 (M)** Network tools: `echoscu`, `echoscp`, `storescu`,
  `storescp`, `findscu` (incl. MWL model, `-k` keys, JSON output),
  each with AE titles, PDU/timeout tuning, and TLS options.
- **FR-CLI-02 (M)** File tools: `dcmdump`, `dcm2json` (incl. file-meta
  option), `json2dcm` (incl. transfer-syntax selection),
  `dcmvalidate` (IOD check with SOP-Class override; exit 0/1/2).

## 4. Non-functional requirements

- **NFR-01 (M)** Toolchain: Rust 1.98+, edition 2024; MSRV changes are
  breaking and must be announced in the changelog.
- **NFR-02 (M)** From-scratch DICOM: no DICOM crates outside this
  workspace; third-party crates only for generic infrastructure
  (async, HTTP, TLS, crypto, CLI, serialization).
- **NFR-03 (M)** License: proprietary, all rights reserved.
- **NFR-04 (M)** Robustness: malformed input (files, PDUs, DIMSE,
  JSON, HTTP) returns typed errors; the library never panics on
  input. Incoming PDU sizes are capped (default 64 MiB).
- **NFR-05 (M)** Security: TLS 1.2+ via rustls with the ring provider
  only (exactly one `CryptoProvider` process-wide); no `unsafe`
  code beyond what dependencies require.
- **NFR-06 (M)** Determinism: encoders emit canonical output so
  dataset/Part-10/JSON round-trips are stable.
- **NFR-07 (M)** Quality gates for every change: unit + doc tests
  green (currently 117 + 2 + 6), `cargo clippy --workspace --all-targets`
  with zero warnings, `cargo fmt --check` clean.
- **NFR-08 (S)** Documentation: public API documented
  (`missing_docs` enforced); this SRS, the design spec, integration
  and user guides, and the SBOM are maintained as living documents.

## 5. Out of scope (v0.1)

JPEG Extended 12-bit (`.51`), JPEG 2000 Part 2, and HTJ2K pixel
codecs; private-vendor dictionaries; media interchange (PS3.11/12); print
management; image rendering/windowing; FIPS mode; C-MOVE destination
auto-discovery (static directory only); IPv6-specific handling;
ATNA audit-trail emission.

## Appendix A: Acceptance traceability

| Requirement | Evidence |
|---|---|
| FR-CORE-01…08 | `vyna-dcm-core` unit tests (27), incl. dictionary/VR/UID/TM/private tests |
| FR-CODEC-01…04 | `round_trips_all_uncompressed_syntaxes`, `round_trips_part10`, `round_trips_implicit_and_deflated`, sequence/fragment tests |
| FR-CODEC-05 | `packbits_known_vectors`, `packbits_round_trip`, `frame_round_trip_16bit`, `frame_round_trip_rgb_planar`, `frame_rejects_bad_headers` |
| FR-CODEC-06 | `round_trips_json_model`, `rejects_bad_json` |
| FR-CODEC-07 | native/RLE path covered indirectly via `frames_and_stow` (WADO frames); bundled JPEG paths by `baseline_*_roundtrip_is_close`, `lossless_roundtrips_exactly`, `near_lossless_honours_error_bound`, `flat_frames_roundtrip`, `lossy_roundtrip_is_close`, `cross_checks_against_independent_j2k`, `bundled_multi_frame_roundtrip`, `truncated_input_*`, plus the `PixelCodec` plug-in tests |
| FR-CODEC-08 | `dump_contains_expected_lines` |
| FR-NET-01…06 | `loopback_echo_store_find`, `loopback_move_and_get`, PDU/DIMSE tests (20); live C-ECHO against third-party endpoint `0x0000` |
| FR-NET-07 | worklist/MPPS/commitment tests |
| FR-NET-08 | `tls_loopback_echo`, `rejects_empty_chain_and_key` |
| FR-WEB-01…03 | `end_to_end_client_against_hyper`, dispatch/multipart tests (11) |
| FR-ECG-01…03 | `vyna-dcm-ecg` tests (7); `ecg_write` example |
| FR-CLI-01/02 | built + exercised (`echoscu/scp`, `storescu/scp`, `findscu`, `dcmdump`, `dcm2json`, `json2dcm`, `dcmvalidate`); `link_route` example |
| FR-IOD-01…03 | `every_iod_accepts_a_valid_fixture` (all 18 IODs), `ct_negative_paths`, `conditional_rules_fire`, `part10_*` round-trips, `dcmvalidate` smoke runs |
| NFR-04 | `rejects_garbage`, `rejects_non_part10`, `rejects_bad_json`, `rejects_missing_boundary`, unknown-route 404 tests |
| NFR-07 | CI-equivalent gate run per change (see §4) |
