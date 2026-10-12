# VynaDCM Software Requirements Specification

| Field | Value |
|---|---|
| Document | SRS (living) |
| Version | 0.1.0 |
| Date | 2026-10-11 |
| Status | Living draft — updated with every release |
| Applies to | VynaDCM workspace v0.1.0 (crates `vyna-dcm-*`) |

Revision history: 0.1.0 (2026-10-11) — initial issue, describing the
implemented baseline (176 tests green, IOD validator + PDF helpers
+ tag editor + transcoder + DICOMDIR + WADO-URI applied).

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
  measured two-pass offsets. `MRDR` following stays out of scope.

### 3.3 DIMSE networking (`vyna-dcm-net`)

- **FR-NET-01 (M)** Upper Layer: encode/decode A-ASSOCIATE-RQ/AC/RJ,
  P-DATA (with max-PDU chunking), RELEASE, and ABORT PDUs.
- **FR-NET-02 (M)** SCU: request associations with odd presentation
  context IDs, negotiate transfer syntaxes per context, send/receive
  DIMSE, release/abort.
- **FR-NET-03 (M)** SCU operations: C-ECHO, C-STORE, C-FIND
  (incl. cancelable), C-MOVE, C-GET (with C-STORE sub-operation
  handler), N-CREATE/SET/GET/ACTION/DELETE/EVENT-REPORT, and
  Storage Commitment request-with-report (`storage_commit`).
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
  STOW-RS upload, WADO-URI retrieve (raw or parsed dataset);
  Basic and Bearer auth.
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
- **FR-PDF-03 (S)** Scope honesty: Encapsulated CDA, PDF/A or magic
  enforcement, and sequence-item validation stay out of scope.

### 3.8 De-identification (`vyna-dcm-deid`)

- **FR-DEID-01 (M)** PS3.15 Annex E action semantics (dummy, zero,
  remove, UID remap, date shift) over a curated 174-tag basic
  profile: patient/staff/institution/device identity,
  visit/admission/order/trial data, icon/annotation free text, all
  instance UIDs, and clinical dates. Sequences recurse; private
  groups and overlay planes are removed by rule (private retention
  optional); UID maps stay consistent per run; dates shift
  (longitudinal) or fall back to a fixed dummy; times, clinical
  content, anatomy, and technique are kept. Burned-in `YES`
  refuses the run; the longitudinal-temporal flag is set factually.
- **FR-DEID-02 (M)** `deidentify` CLI: multi-file batches with
  `--output-dir` / `--output` / `--in-place`, `--date-offset`,
  `--patient-id` / `--patient-name`, `--retain-private-tags`,
  file-meta UID sync, per-file summaries, exit 0/1.
- **FR-DEID-03 (S)** Scope honesty: curated subset (not a full E.1
  transcription), no coded profile claims, no free-text scrubbing,
  no charset conversion, no UID-map persistence, no pixel redaction.

### 3.9 CLI tools (`vyna-dcm`, feature `cli`)

- **FR-CLI-01 (M)** Network tools: `echoscu`, `echoscp`, `storescu`,
  `storescp`, `findscu` (incl. MWL model, `-k` keys, JSON output),
  `movescu` (C-MOVE to a `--dest` AE), `getscu` (C-GET into
  `--output-dir`), `mppsscu` (MPPS N-CREATE + N-SET), `stgcmtscu`
  (Storage Commitment N-ACTION + report wait), `auditsend` (one
  ATNA message per invocation), each with AE titles, PDU/timeout
  tuning, and TLS options; `storescp`/`storescu`/`findscu` also
  take `--audit-server` for wired emission.
- **FR-CLI-02 (M)** File tools: `dcmdump`, `dcm2json` (incl. file-meta
  option), `json2dcm` (incl. transfer-syntax selection),
  `dcmvalidate` (IOD check with SOP-Class override; exit 0/1/2),
  `pdf2dcm` (PDF → Part-10 with IOD self-check; exit 0/1),
  `dcm2pdf` (Part-10 → PDF extraction; exit 0/1),
  `dcmodify` (tag modify/insert/erase with VR-aware values,
  file-meta UID sync, in-place or `--output`; exit 0/1),
  `dcm2dcm` (transcode with syntax names/UIDs and quality knobs;
  exit 0/1),
  `dcmdir` (create a DICOMDIR from files/dirs, or `--list` one;
  exit 0/1),
  `deidentify` (basic-profile de-identification with date offset,
  dummy overrides, and batch outputs; exit 0/1).

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
  `findscu` (per-query) via shared `--audit-server` /
  `--audit-source` / `--audit-ca` flags; best-effort (transport
  failures warn, never break the clinical path).
- **FR-ATNA-04 (S)** Scope honesty: code triples pinned by tests
  per PS3.15/DCMR; peer socket IPs not plumbed (AE titles only
  from handlers); movescu/getscu/mppsscu/stgcmtscu not yet wired.

### 3.11 Specific Character Set conversion (`vyna-dcm-codec`)

- **FR-CHARSET-01 (M)** Decode PN/LO/SH/ST/LT/UT values per the
  top-level Specific Character Set (0008,0005), inherited by nested
  sequence items: `ISO_IR 6/100/101/109/110/126/127/138/144/148/
  166/192`, `ISO_IR 13`, Japanese code-extension pairs (`ISO 2022
  IR 13/87/159`), Korean `ISO 2022 IR 149` (SO/SI + KS X 1001 in
  G1), `GBK`/`GB18030`. Whole-value conversion (code-extension
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
  green (currently 167 + 2 + 7), `cargo clippy --workspace --all-targets`
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
| FR-CODEC-09 | `mono8_lossless_matrix_is_exact`, `mono16_and_bigendian_roundtrip_exact`, `rgb_*`, `ybr422_*`, `ybr_full_*`, `lossy_source_keeps_history_*`, `multiframe_*`, `bad_inputs_rejected`, `baseline_rgb_odd_sizes_roundtrip`, CLI `dcm2dcm` matrix runs |
| FR-CODEC-10 | `roundtrip_tree` (every link points at an item tag), `empty_dir_roundtrip`, `record_type_mapping`, `entry_validation`, `rejects_non_directory`, `walker_*` (skips, loops, dangling, deleted), `capture_points_at_item_tags`, CLI `dcmdir` create+list runs |
| FR-NET-01…06 | `loopback_echo_store_find`, `loopback_move_and_get`, `loopback_storage_commit`, `storage_commit_skips_wait_on_action_failure`, PDU/DIMSE tests (20); live C-ECHO against third-party endpoint `0x0000` |
| FR-NET-07 | worklist/MPPS/commitment tests |
| FR-NET-08 | `tls_loopback_echo`, `rejects_empty_chain_and_key` |
| FR-WEB-01…03 | `end_to_end_client_against_hyper`, `wado_uri_against_stub`, `wado_uri_*` param tests, dispatch/multipart tests |
| FR-ECG-01…03 | `vyna-dcm-ecg` tests (7); `ecg_write` example |
| FR-CLI-01/02 | built + exercised (`echoscu/scp`, `storescu/scp`, `findscu`, `movescu`, `getscu`, `mppsscu`, `stgcmtscu`, `dcmdump`, `dcm2json`, `json2dcm`, `dcmvalidate`, `pdf2dcm`, `dcm2pdf`, `dcmodify`, `dcm2dcm`, `dcmdir`); `link_route` example |
| FR-IOD-01…03 | `every_iod_accepts_a_valid_fixture` (all 18 IODs), `ct_negative_paths`, `conditional_rules_fire`, `part10_*` round-trips, `dcmvalidate` smoke runs |
| FR-PDF-01…03 | `build_extract_round_trip`, `built_instance_validates_clean`, `odd_length_pdf_survives_part10`, `round_trips_all_native_syntaxes` (validates each), `non_ascii_patient_adds_utf8_charset`, `extract_*`, CLI `pdf2dcm` → `dcmvalidate` → `dcm2pdf` byte-exact run |
| FR-DEID-01…03 | `every_action_fires`, `shift_keeps_longitudinal_intervals`, `datetimes_shift_or_dummy_by_vr`, `burned_in_annotation_refuses_before_mutation`, `uid_maps_stay_consistent_across_datasets`, `shifts_cross_month_year_and_leap_day`, profile self-checks, CLI `output_plans_*` + `process_file_round_trip`, `deidentified_ecg_validates_with_identical_samples` |
| FR-ATNA-01…04 | `event_code_triples_are_pinned`, `transferred_xml_has_shape_and_roles`, `values_escape`, `urls_parse_with_scheme_defaults`, UDP/TCP/TLS loopbacks (+ untrusted rejection), CLI `flow_messages_*` + `subcommands_parse`, wired `audit_flags_parse` ×3, `statuses_map_to_outcomes`, live UDP/TCP/TLS ARR-stub runs |
| FR-CHARSET-01…03 | `oracle_vectors_decode`, `oracle_vectors_round_trip` (CPython latin-1/8859-2/8859-5/8859-7/cp1254/cp874/iso2022-jp/iso2022-kr/gb18030 vectors), `jis_x_0201_is_mechanical`, `ascii_and_fallbacks_behave`, `unmappable_encodes_to_question_mark`, `round_trips_latin1_patient_name`, `round_trips_japanese_pn_component_groups`, `sequence_items_inherit_charset`, `decodes_oracle_gb18030_bytes`, `dcmdump` CJK smoke runs |
| NFR-04 | `rejects_garbage`, `rejects_non_part10`, `rejects_bad_json`, `rejects_missing_boundary`, unknown-route 404 tests |
| NFR-07 | CI-equivalent gate run per change (see §4) |
