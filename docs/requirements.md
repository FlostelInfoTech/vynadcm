# VynaDCM Software Requirements Specification

| Field | Value |
|---|---|
| Document | SRS (living) |
| Version | 0.6.0 |
| Date | 2026-10-15 |
| Status | Living draft — updated with every release |
| Applies to | VynaDCM workspace v0.6.0 (crates `vyna-dcm-*`) |

Revision history: 0.1.0 (2026-10-11) — initial issue, describing the
implemented baseline (176 tests green, IOD validator + PDF helpers
+ tag editor + transcoder + DICOMDIR + WADO-URI applied);
0.2.0 (2026-10-12) — version bump; FR-WEB-04 + QR/MWL SCP +
charset/XML/img requirements folded in since 0.1.0;
0.3.0 (2026-10-12) — FR-NET-11 (MPPS/commitment SCP) + cancel
+ ATNA completion + CLI deltas folded in;
0.4.0 (2026-10-13) — MIT license (NFR-03), quality-gate wording;
0.5.0 (2026-10-13) — version bump; FR-CORE-05 default-TS clarification;
0.6.0 (2026-10-15) — version bump to match workspace;
Unreleased — relational QR, MRDR, BulkData resolution, WADO-URI
server, pixel redaction, FileStore, ATNA async folded in.

## Contents

- [1. Introduction](#1-introduction)
- [2. Overall description](#2-overall-description)
- [3. Functional requirements](#3-functional-requirements)
- [4. Non-functional requirements](#4-non-functional-requirements)
- [5. Out of scope (v0.5)](#5-out-of-scope-v05)
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
waveform mapping, Encapsulated PDF mapping, and operator CLI tools.
Out of scope: §5.

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

Rust 1.99+, edition 2024, async via tokio, MIT license
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
  creator ID, group, and suffix (currently 424 creators / 8,898
  entries with wildcard fallbacks); unknown creators or suffixes
  resolve to “unknown”, never to a wrong entry.
- **FR-CORE-05 (M)** Transfer syntaxes: model 16 transfer syntax UIDs
  with wire properties (endianness, implicit/explicit VR, deflated,
  encapsulated). Two distinct defaults: the enum `Default` is Implicit
  VR Little Endian (the DICOM Default Transfer Syntax, PS3.5 §10, used
  when nothing is negotiated), while new associations prefer Explicit
  VR Little Endian (SCU proposal lists lead with it; file writers take
  an explicit syntax per call).
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
- **FR-CODEC-09 (M)** Transcode pixel data across the native
  syntaxes, RLE, and the bundled JPEG families
  (`transcode_dataset`): planar/YBR normalization through RGB,
  per-target photometrics, ≤64 KiB refragmentation with offset
  tables when needed, lossy history + fresh SOP UID on lossy
  targets, same-syntax no-op. Palette expansion and `YBR_PARTIAL_*`
  stay out of scope.
- **FR-CODEC-10 (M)** DICOMDIR media directories: read files into a
  patient → study → series → instance tree by following the offset
  links exactly (`read_dicomdir`; loops and dangling offsets are
  errors, unusable records are reported, never silently dropped),
  and write them from referenced datasets (`build_dicomdir`) with
  measured two-pass offsets. Multi-referenced (`MRDR`) records
  resolve to their target instance (loop-guarded; unresolvable
  targets are reported with the `MRDR` type, never fatal).
- **FR-CODEC-11 (M)** Convert datasets to/from Native DICOM Model
  XML (PS3.19 A): `Value` children for text/numbers/AT (8-hex),
  `PersonName` component groups, `InlineBinary` for byte VRs,
  nested `Item` datasets; `DS` keeps its wire lexical form (no
  float trip). `BulkData` references resolve through a
  caller-supplied fetch callback (without one they are refused);
  DCMTK's proprietary `<element>` format stays out of scope.
- **FR-CODEC-12 (M)** Single-frame PNG export/import for debugging:
  `frame_to_png` renders MONOCHROME1/2 (8/16-bit, signed offset by
  2^15, MONO1 inverted) and interleaved/planar RGB 8-bit; `inject_png`
  replaces Pixel Data from `L8`/`L16`/`RGB8` PNGs and rewrites the
  geometry tags (UIDs kept: test/debug injection). YBR, palette,
  and sub-byte depths are refused; windowing stays out of scope.

### 3.3 DIMSE networking (`vyna-dcm-net`)

- **FR-NET-01 (M)** Upper Layer: encode/decode A-ASSOCIATE-RQ/AC/RJ,
  P-DATA (with max-PDU chunking), RELEASE, and ABORT PDUs.
- **FR-NET-02 (M)** SCU: request associations with odd presentation
  context IDs, negotiate transfer syntaxes per context, send/receive
  DIMSE, release/abort. QR SCU tools propose relational-queries
  extended negotiation (`0x56`) behind `--relational`; the exchange
  counts as relational only when the peer echoes the proposal
  (gratuitous echoes are ignored).
- **FR-NET-03 (M)** SCU operations: C-ECHO, C-STORE, C-FIND
  (incl. cancelable), C-MOVE, C-GET (with C-STORE sub-operation
  handler), N-CREATE/SET/GET/ACTION/DELETE/EVENT-REPORT, and
  Storage Commitment request-with-report (`storage_commit`).
- **FR-NET-04 (M)** SCP: accept associations, negotiate with gating on
  AE title (optional strict mode), transfer syntaxes, SOP classes,
  and an optional user-identity callback; dispatch each service to
  application `Handlers`. Relational-queries proposals (`0x56`)
  are echoed for QR C-FIND/C-MOVE/C-GET contexts only.
- **FR-NET-05 (M)** C-MOVE: route instances to a configured destination
  AE over a dedicated outbound association; C-GET: deliver over the
  same association and abort delivery on C-CANCEL; C-FIND/C-MOVE
  honor C-CANCEL mid-stream (final FE00, other mid-operation events
  stashed, never swallowed).
- **FR-NET-06 (M)** Status handling: classify statuses
  (success/warning/failure/pending/cancel) and count sub-operations
  (completed/failed/warning) for MOVE/GET responses.
- **FR-NET-07 (M)** Worklist helpers: MWL C-FIND datasets, MPPS
  N-CREATE/N-SET datasets, Storage Commitment N-ACTION/N-EVENT-REPORT
  builders and result parsing.
- **FR-NET-08 (M)** TLS (PS3.15, feature `tls`): TLS acceptors and
  connectors over TCP, mutual TLS, extra-CA trust, on top of the same
  association machinery.
- **FR-NET-09 (M)** QR SCP (`net::qrdb` + `findscp`): recursive
  directory index (pixel data stripped, unreadable files skipped);
  C-FIND matching (universal/single/wildcard/UID-list/date-time and
  numeric ranges/nested sequences, leading-component PN) with level
  grouping and requested-key responses; C-MOVE via an AE route table;
  C-GET over the same association. Missing level fails 0xA900;
  Patient/Study Only rejects SERIES/IMAGE levels.
- **FR-NET-10 (M)** MWL SCP (`net::qrdb` + `mwlscp`): worklist C-FIND
  without a level key, with Scheduled Procedure Step sequence
  matching, one response per matching item.
- **FR-NET-11 (M)** MPPS SCP (`net::worklist::MppsScp` + `mppsscp`):
  N-CREATE inserts a step, N-SET merges over it (duplicates 0x0111,
  unknown steps 0x0112, final states reject updates), persisted as
  Part-10 by MPPS UID with reload. Storage Commitment SCP
  (`net::qrdb::StgCommitScp` + `stgcmtscp`): index holdings commit,
  the rest fail 0x0112, verdict via an N-EVENT-REPORT follow-up on
  the same association (framework `ScpFollowUp`, RSP awaited).

### 3.4 DICOMweb (`vyna-dcm-web`)

- **FR-WEB-01 (M)** Client: QIDO-RS search (studies/series/instances
  with query filters), WADO-RS (file/dataset/metadata/frames/bulkdata),
  STOW-RS upload, WADO-URI retrieve (raw or parsed dataset);
  Basic and Bearer auth.
- **FR-WEB-02 (M)** `multipart/related` encode/decode from scratch,
  tolerating boundary-like bytes inside bodies.
- **FR-WEB-03 (M)** Server core: route QIDO/WADO/STOW plus legacy
  WADO-URI (`GET /wado`, DICOM-only with `transferSyntax`
  transcode; rendering knobs refused) against any `DicomWebStore`
  implementation; ship an in-memory store and a durable `FileStore`
  directory store; expose a hyper HTTP/1.1 front end
  (feature `server`).
- **FR-WEB-04 (M)** CLIs: `qidors` (search with `-k` keys, limit/offset,
  text or JSON output), `wadors` (instance/metadata/frames/bulkdata
  retrieve), `stowrs` (multipart store), `wadorsd` (stub server over
  a directory; `--persist` serves from a durable `FileStore` so
  STOW uploads survive restart, otherwise STOW lands in memory;
  `--bearer-token-file` reads the Bearer [REDACTED] a file).

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
  (18 SOP Classes across CR/CT/MR/MG/DX/US/SC/PT/NM/XA/VL/KO/PDF/ECG,
  with DX, MG, US, and ECG covering two classes each):
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

### 3.7 Encapsulated PDF mapping (`vyna-dcm-pdf`)

- **FR-PDF-01 (M)** Build complete Encapsulated PDF Storage SOP
  instances from PDF bytes: real Type 1 values (SOP Class/Instance,
  Study/Series UIDs generated unless supplied, Modality, Conversion
  Type, Burned In Annotation, MIME `application/pdf`), every Type 2
  attribute present (empty when unknown), the exact byte length
  recorded, and Specific Character Set added only when text needs
  non-ASCII — so built instances validate clean and are C-STORE-able.
- **FR-PDF-02 (M)** Extract the PDF back byte-exactly: truncate wire
  padding via the recorded length, join fragmented storage, and
  return MIME/title/SOP UID metadata. Without a usable recorded
  length the stored bytes are returned unchanged — never guessed at.
- **FR-PDF-03 (S)** Scope honesty: Encapsulated CDA, PDF/A, and
  sequence-item validation stay out of scope. The builder does
  enforce the `%PDF-` magic (non-PDF bytes are `Error::NotPdf`,
  never mislabeled `application/pdf`).

### 3.8 De-identification (`vyna-dcm-deid`)

- **FR-DEID-01 (M)** PS3.15 Annex E action semantics (dummy, zero,
  remove, UID remap, date shift) over a 636-row E.1-transcribed basic
  profile: patient/staff/institution/device identity,
  visit/admission/order/trial data, icon/annotation free text, all
  instance UIDs, and clinical dates. Sequences recurse; private
  groups and overlay planes are removed by rule (private retention
  optional); UID maps stay consistent per run; dates shift
  (longitudinal) or fall back to a fixed dummy; times, clinical
  content, anatomy, and technique are kept. Burned-in `YES`
  refuses the run unless `--redact` regions (or the library
  `with_allow_burned_in` override) admit it, in which case the run
  proceeds warned with conformance markers withheld; the
  longitudinal-temporal flag is set factually.
- **FR-DEID-02 (M)** `deidentify` CLI: multi-file batches with
  `--output-dir` / `--output` / `--in-place`, `--date-offset`,
  `--patient-id` / `--patient-name`, `--retain-private-tags`,
  `--redact X,Y,W,H[:FRAME]` (repeatable: black rectangles over
  native 8/16-bit frames before de-identifying; out-of-bounds
  regions fail the file), file-meta UID sync, per-file summaries,
  exit 0/1.
- **FR-DEID-03 (S)** Scope honesty: curated subset (not a full E.1
  transcription), no coded profile claims, no free-text scrubbing,
  no charset conversion, no UID-map persistence.

### 3.9 CLI tools (`vyna-dcm`, feature `cli`)

- **FR-CLI-01 (M)** Network tools: `echoscu`, `echoscp`, `storescu`,
  `storescp` (with `--exec` receive hooks), `findscu` (incl. MWL
  model, `-k` keys, JSON output, `--cancel N`, `--relational`),
  `movescu` (C-MOVE to a `--dest` AE, `--relational`), `getscu`
  (C-GET into `--output-dir`, transcoding encapsulated arrivals
  to native, `--relational`), `findscp` (QR FIND/MOVE/GET from a
  directory, `--route` AE table), `mwlscp`
  (worklist FIND from a directory), `mppsscu` (MPPS N-CREATE +
  N-SET), `mppsscp` (MPPS N-CREATE/N-SET into a directory),
  `stgcmtscu` (Storage Commitment N-ACTION + report wait),
  `stgcmtscp` (commitment verdicts from a directory), each with AE
  titles, PDU/timeout tuning, and TLS options; `qidors`, `wadors`,
  `stowrs` (DICOMweb client tools: `--url` plus `--bearer`/`--basic`
  auth), `wadorsd` (stub server over a directory), `auditsend` (one
  ATNA message per invocation to a syslog server); every DIMSE tool
  but `echoscu`/`echoscp` also takes `--audit-server` for wired
  emission.
- **FR-CLI-02 (M)** File tools: `dcmdump`, `dcm2json` (incl. file-meta
  option), `json2dcm` (incl. transfer-syntax selection),
  `dcm2xml` (incl. file-meta option), `xml2dcm` (incl.
  transfer-syntax selection and `--bulk-data-dir`), `dcm2img`
  (`--frame`), `img2dcm`
  (PNG + template, native by default, `--transfer-syntax` +
  quality knobs for encapsulated targets),
  `dcmvalidate` (IOD check with SOP-Class override; exit 0/1/2),
  `pdf2dcm` (PDF → Part-10 with IOD self-check; exit 0/1),
  `dcm2pdf` (Part-10 → PDF extraction; exit 0/1),
  `dcmodify` (tag modify/insert/erase with VR-aware values and
  `SEQ[i].TAG` sequence paths, file-meta UID sync, in-place or
  `--output`; exit 0/1),
  `dcm2dcm` (transcode with syntax names/UIDs and quality knobs;
  exit 0/1),
  `dcmdir` (create a DICOMDIR from files/dirs, or `--list` one;
  exit 0/1),
  `deidentify` (basic-profile de-identification with date offset,
  dummy overrides, `--redact` regions, and batch outputs;
  exit 0/1).

### 3.10 ATNA audit trail (`vyna-dcm-atna`)

- **FR-ATNA-01 (M)** DICOM audit messages (PS3.15 A.5 / RFC 3881
  XML, fully escaped): Application Activity, Begin Transferring,
  Instances Transferred, Query, Study Deleted, Export, Import,
  plus custom triples; participant roles, study/patient objects
  with base64 instance details, outcomes 0/4/8/12.
- **FR-ATNA-02 (M)** Syslog delivery to `udp://`, `tcp://`
  (octet-counting), and `tls://` (WebPKI + optional private CA,
  hostname verification) ARRs; facility audit(13) with
  outcome-mapped severity; `--dry-run` XML preview.
- **FR-ATNA-03 (M)** Wired emission: `storescp` (actor-start +
  per-store), `storescu` (per-store with failure outcomes),
  `findscu`/`findscp`/`mwlscp` (per-query), `movescu`
  (begin-transferring), `getscu` (per-arrival), `mppsscu`/`mppsscp`
  (per-step), `stgcmtscu`/`stgcmtscp` (per-instance) via shared
  `--audit-server` / `--audit-source` / `--audit-ca` flags;
  best-effort (transport failures warn, never break the clinical
  path).
- **FR-ATNA-04 (S)** Scope honesty: code triples pinned by tests
  per PS3.15/DCMR; peer socket IPs not plumbed (AE titles only
  from handlers).

### 3.11 Specific Character Set conversion (`vyna-dcm-codec`)

- **FR-CHARSET-01 (M)** Decode PN/LO/SH/ST/LT/UT values per the
  top-level Specific Character Set (0008,0005), inherited by nested
  sequence items: `ISO_IR 6/100/101/109/110/126/127/138/144/148/
  166/192`, `ISO_IR 13`, Japanese code-extension pairs (`ISO 2022
  IR 13/87/159`), Korean `ISO 2022 IR 149` (SO/SI + KS X 1001 in
  G1), `GBK`/`GB18030`/`GB2312` (GB2312 via its GBK superset).
  Whole-value conversion (code-extension
  state spans the value) with delimiters split in Unicode space
  afterwards; UC/UR stay UTF-8; undeclared or unknown sets keep
  the historical Latin-1 fallback.
- **FR-CHARSET-02 (M)** Encode mirror: text/PN values are written
  in the dataset's declared set (one code-extension run per value
  or name, `?` for unmappable characters); pure-ASCII traffic is
  byte-identical with or without a declaration.
- **FR-CHARSET-03 (S)** Scope honesty: no ISO 8859-10/13/14/15/16,
  no UTF-16, no vendor code pages; multi-term sets outside the
  Japanese/Korean pairs fall back to Latin-1 on decode and pass
  UTF-8 bytes through on encode.

## 4. Non-functional requirements

- **NFR-01 (M)** Toolchain: Rust 1.99+, edition 2024; MSRV changes are
  breaking and must be announced in the changelog.
- **NFR-02 (M)** From-scratch DICOM: no DICOM crates outside this
  workspace; third-party crates only for generic infrastructure
  (async, HTTP, TLS, crypto, CLI, serialization).
- **NFR-03 (M)** License: MIT (see `LICENSE`).
- **NFR-04 (M)** Robustness: malformed input (files, PDUs, DIMSE,
  JSON, HTTP) returns typed errors; the library never panics on
  input. Incoming PDU sizes are capped (default 64 MiB), as are
  reassembled DIMSE messages (default 1 GiB).
- **NFR-05 (M)** Security: TLS 1.2+ via rustls with the ring provider
  only (exactly one `CryptoProvider` process-wide); no `unsafe`
  code beyond what dependencies require — the one first-party
  exception is the confined wrapper driving the C-style
  `vyna-openjp2` API (`codec::jpeg_2000`, see its safety notes).
- **NFR-06 (M)** Determinism: encoders emit canonical output so
  dataset/Part-10/JSON round-trips are stable.
- **NFR-07 (M)** Quality gates for every change: unit + integration
  + doc tests green, `cargo clippy --workspace --all-targets`
  with zero warnings, `cargo fmt --check` clean.
- **NFR-08 (S)** Documentation: public API documented
  (`#![warn(missing_docs)]` in every first-party crate); this SRS,
  the design spec, integration and user guides, and the SBOM are
  maintained as living documents.

## 5. Out of scope (v0.5)

JPEG Extended 12-bit (`.51`), JPEG 2000 Part 2, and HTJ2K pixel
codecs; print management; image rendering/windowing; FIPS mode;
C-MOVE destination auto-discovery (static directory only);
IPv6-specific handling.

## Appendix A: Acceptance traceability

| Requirement | Evidence |
|---|---|
| FR-CORE-01…08 | `vyna-dcm-core` unit tests (46), incl. dictionary/VR/UID/TM/private tests |
| FR-CODEC-01…04 | `round_trips_all_uncompressed_syntaxes`, `round_trips_part10`, `round_trips_implicit_and_deflated`, sequence/fragment tests |
| FR-CODEC-05 | `packbits_known_vectors`, `packbits_round_trip`, `frame_round_trip_16bit`, `frame_round_trip_rgb_planar`, `frame_rejects_bad_headers` |
| FR-CODEC-05-b | `bot_usable_rejects_header_counted_tables`, `rle_falls_back_on_bogus_bot`, `jpeg_bot_needs_codestream_starts`, corpus `lossless_family_agrees` (MSB-first planes, even-pad segments, validated BOT) |
| FR-CODEC-06 | `round_trips_json_model`, `rejects_bad_json` |
| FR-CODEC-07 | native/RLE path covered indirectly via `frames_and_stow` (WADO frames); bundled JPEG paths by `baseline_*_roundtrip_is_close`, `lossless_roundtrips_exactly`, `near_lossless_honours_error_bound`, `flat_frames_roundtrip`, `lossy_roundtrip_is_close`, `cross_checks_against_independent_j2k`, `bundled_multi_frame_roundtrip`, `truncated_input_*`, plus the `PixelCodec` plug-in tests |
| FR-CODEC-08 | `dump_contains_expected_lines` |
| FR-CODEC-09 | `mono8_lossless_matrix_is_exact`, `mono16_and_bigendian_roundtrip_exact`, `rgb_*`, `ybr422_*`, `ybr_full_*`, `lossy_source_keeps_history_*`, `multiframe_*`, `bad_inputs_rejected`, `baseline_rgb_odd_sizes_roundtrip`, CLI `dcm2dcm` matrix runs |
| FR-CODEC-10 | `roundtrip_tree` (every link points at an item tag), `empty_dir_roundtrip`, `record_type_mapping`, `entry_validation`, `rejects_non_directory`, `walker_*` (skips, loops, dangling, deleted), `capture_points_at_item_tags`, `mrdr_follows_to_shared_leaf`, `mrdr_failures_skip_loudly`, CLI `dcmdir` create+list runs |
| FR-CODEC-11 | `round_trips_mixed_dataset`, `person_name_component_groups`, `rejects_bad_inputs`, `liberal_parse_accepts_minimal`, `bulk_data_resolves_through_callback`, `bulk_file_policy_accepts_relative_and_rejects_escapes`, CLI `dcm2xml` → `xml2dcm` JP round-trip run, CLI `bulk_data_dir_end_to_end` |
| FR-CODEC-12 | `png_round_trip_mono16_signed`, `mono1_inverts_and_planar_rgb_renders`, `rejects_unsupported` (img), `corpus_renders_png`, CLI `dcm2img` → `img2dcm` byte-identical run |
| FR-NET-01…06 | `loopback_echo_store_find`, `loopback_move_and_get`, `loopback_find_cancel_truncates`, `loopback_storage_commit`, `storage_commit_skips_wait_on_action_failure`, PDU/DIMSE unit tests (13), `qr_scp` integration tests (17), `relational_requested_matches_bit_and_sop`, `adopt_ac_sets_relational_only_when_proposed_and_echoed`, `negotiates_relational_for_qr_contexts`, `relational_proposal_builds_bit_set_entry`; live C-ECHO against third-party endpoint `0x0000`, live `findscu --cancel` vs `findscp` (FE00, exit 0) |
| FR-NET-07 | worklist/MPPS/commitment tests |
| FR-NET-08 | `tls_loopback_echo`, `rejects_empty_chain_and_key` |
| FR-NET-09…11 | `wildcard_and_range_match`, `pn_matches_leading_components`, `level_grouping_dedupes`, `responses_echo_requested_keys_only`, `sequence_keys_match_nested`, loopback `find_study_level_groups`, `find_missing_level_fails`, `move_delivers_to_destination`, `get_serves_over_same_association`, `mwl_matches_station`, `mpps_create_then_set_merges`, `mpps_rejects_bad_requests`, `mpps_final_states_reject_updates`, `mpps_reloads_persisted_steps`, `commitment_holds_partition`, `commitment_rejects_bad_requests`, `relational_cross_level_queries_answer`, loopback `loopback_mpps_create_set`, `loopback_storage_commit_push`, CLI `findscp`/`mwlscp`/`mppsscp`/`stgcmtscp` smoke runs |
| FR-WEB-01…03 | `end_to_end_client_against_hyper`, `wado_uri_against_stub`, `wado_uri_*` param tests, `wado_uri_serves_instances_and_transcodes`, `wado_uri_rejects_loudly`, `store_round_trips_through_reopen`, `scan_skips_garbage_and_counts_it`, `restore_replaces_and_hint_mismatch_writes_nothing`, `traversal_uids_have_no_path`, `invalid_uid_files_are_rejected_without_disk_trace`, dispatch/multipart tests |
| FR-WEB-04 | `qido_finds_seeded_instance`, `wado_round_trips_file_metadata_frames`, `stow_lands_and_qido_sees_it`, `open_store_memory_and_persist_modes`, `token_file_resolves_trimmed_token`, `token_file_missing_and_empty_fail_loudly`, `token_and_token_file_conflict`, wadors `frames_parse`, CLI smoke runs against `wadorsd` |
| FR-ECG-01…03 | `vyna-dcm-ecg` tests (25); `ecg_write` example |
| FR-CLI-01/02 | built + exercised (`echoscu/scp`, `storescu/scp`, `findscu`, `findscp`, `mwlscp`, `movescu`, `getscu`, `mppsscu`, `mppsscp`, `stgcmtscu`, `stgcmtscp`, `qidors`, `wadors`, `stowrs`, `wadorsd`, `dcmdump`, `dcm2json`, `json2dcm`, `dcm2xml`, `xml2dcm`, `dcm2img`, `img2dcm`, `dcmvalidate`, `pdf2dcm`, `dcm2pdf`, `dcmodify`, `dcm2dcm`, `dcmdir`, `deidentify`, `auditsend`); `link_route` example; `cancel_flag_parses`, `sequence_paths_parse`, `sequence_edits_apply_per_item`, `transfer_syntax_names_parse`, `quality_knobs_validate`, `injected_png_survives_lossless_targets`, `exec_template_parses_and_expands`, `exec_hook_runs_and_reports_exit`, `study_uid_prefers_top_level` |
| FR-IOD-01…03 | `every_iod_accepts_a_valid_fixture` (all 18 IODs), `ct_negative_paths`, `conditional_rules_fire`, `part10_*` round-trips, `dcmvalidate` smoke runs |
| FR-PDF-01…03 | `build_extract_round_trip`, `built_instance_validates_clean`, `odd_length_pdf_survives_part10`, `round_trips_all_native_syntaxes` (validates each), `non_ascii_patient_adds_utf8_charset`, `extract_*`, CLI `pdf2dcm` → `dcmvalidate` → `dcm2pdf` byte-exact run |
| FR-DEID-01…03 | `every_action_fires`, `shift_keeps_longitudinal_intervals`, `datetimes_shift_or_dummy_by_vr`, `burned_in_annotation_refuses_before_mutation`, `burned_in_override_proceeds_warned_and_unmarked`, `uid_maps_stay_consistent_across_datasets`, `shifts_cross_month_year_and_leap_day`, `mono8_all_frames_redacts_inside_only`, `rgb_planar_layouts_redact_all_components`, `mono1_fills_max_and_single_frame_scopes`, `mono16_fills_whole_samples`, `bad_regions_geometries_and_syntaxes_fail_closed`, profile self-checks, CLI `output_plans_*` + `process_file_round_trip` + `process_file_redacts_pixels_before_deidentifying` + `parse_redact_accepts_forms_and_rejects_garbage`, `deidentified_ecg_validates_with_identical_samples` |
| FR-ATNA-01…04 | `event_code_triples_are_pinned`, `transferred_xml_has_shape_and_roles`, `values_escape`, `urls_parse_with_scheme_defaults`, UDP/TCP/TLS loopbacks (+ untrusted rejection), CLI `flow_messages_*` + `subcommands_parse`, wired `audit_flags_parse` ×11, `statuses_map_to_outcomes`, live UDP/TCP/TLS ARR-stub runs |
| FR-CHARSET-01…03 | `oracle_vectors_decode`, `oracle_vectors_round_trip` (CPython latin-1/8859-2/8859-5/8859-7/cp1254/cp874/iso2022-jp/iso2022-kr/gb18030 vectors), `jis_x_0201_is_mechanical`, `ascii_and_fallbacks_behave`, `unmappable_encodes_to_question_mark`, `round_trips_latin1_patient_name`, `round_trips_japanese_pn_component_groups`, `sequence_items_inherit_charset`, `decodes_oracle_gb18030_bytes`, `dcmdump` CJK smoke runs |
| NFR-04 | `rejects_garbage`, `rejects_non_part10`, `rejects_bad_json`, `rejects_missing_boundary`, `truncated_and_mutated_datasets_never_panic`, `truncated_and_mutated_files_never_panic`, `truncated_and_mutated_frames_never_panic`, unknown-route 404 tests |
| NFR-07 | CI-equivalent gate run per change (see §4) |
