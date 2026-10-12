# VynaDCM Ecosystem Comparison & Roadmap

| Field | Value |
|---|---|
| Document | Roadmap (living) |
| Version | 0.3.0 |
| Date | 2026-10-12 |
| Status | Living — reprioritized as VynaECG/VynaLink needs evolve |
| Applies to | VynaDCM workspace v0.3.0 |

Revision history: 0.1.0 (2026-10-11) — initial issue, from comparison
with dcm4che, DCMTK, pydicom (+pynetdicom), and fo-dicom;
0.2.0 (2026-10-12) — version bump; all P0–P2 items and both
comparison rounds DONE;
0.3.0 (2026-10-12) — post-0.2.0 hardening batch DONE.

## Contents

- [1. Method](#1-method)
- [2. Where VynaDCM leads](#2-where-vynadcm-leads)
- [3. Priority 0: interoperability foundations](#3-priority-0-interoperability-foundations)
- [4. Priority 1: interface-engine depth](#4-priority-1-interface-engine-depth)
- [5. Priority 2: deployment and robustness](#5-priority-2-deployment-and-robustness)
- [6. Cross-toolkit comparison rounds](#6-cross-toolkit-comparison-rounds)
- [7. Deliberate non-goals](#7-deliberate-non-goals)

## 1. Method

Compared against the reference toolkits' shipped capabilities
([dcm4che modules & tools](https://github.com/dcm4che/dcm4che),
[DCMTK modules](https://github.com/DCMTK/dcmtk),
[pydicom user guide](https://pydicom.github.io/pydicom/stable/guides/user/index.html),
fo-dicom DIMSE/DICOMweb coverage). Each gap below is ranked by value
to the two consumers (VynaECG, VynaLink) and given acceptance criteria.
Items move to the SRS (`docs/requirements.md`) when scheduled.

## 2. Where VynaDCM leads

- DICOMweb client **and** server in one dependency-light library
  (DCMTK: none; fo-dicom: weak; pydicom: separate packages).
- ECG waveform IOD modeling (recording model, MDC codes, SOP
  builder) — no rival ships ECG-specific mapping.
- HTJ2K transfer syntaxes modeled; single-`ring` TLS stack shared by
  DIMSE and HTTPS; fully async networking.

## 3. Priority 0: interoperability foundations

### P0.1 Full PS3.6 dictionary + private creators — DONE

Replaced the 266-entry curated table with the complete public registry
(5,239 entries, PS3.6 2026d) plus private-creator mechanics (424
creators / 8,860 entries) and the missing `SV` VR (35 variants).
Accepted: registry/private spot tests, 96/96 green, clippy clean.

### P0.2 Bundled JPEG / JPEG-LS / JPEG 2000 backend — DONE

Shipped `BundledCodec` (`.50` via `jpeg-decoder`/`jpeg-encoder`,
`.70` via `jpegli`, `.80`/`.81` via `jpegls`, `.90`/`.91` via
vendored `openjp2` with a null-`dealloc` patch) plus `encode_pixels`
and multi-frame decode; `decode_pixels` works without an explicit
codec. Accepted: exact lossless round-trips, NEAR-bound and
lossy-error tests, independent-encoder cross-checks, truncated-input
robustness, 117/117 green, clippy clean. `.51`, J2K Part 2, and
HTJ2K stay out of scope.

### P0.3 IOD validator (`dciodvfy` / `dcmvalidate`-like) — DONE

Shipped `vyna-dcm-iod` (`validate` + `Report`/`Finding`) with 18
generated IOD tables (89 macro-expanded modules, strictest-type
merge, 7 conditional rules) plus the `dcmvalidate` CLI with
file-meta consistency checks. Accepted: per-IOD valid fixtures
validate clean, negatives/conditions flag correctly,
Part-10 round-trips green, 125/125 tests, clippy clean. Out of
scope (documented): sequence items, value-type gating, value
formats, overlays.

### P0.4 Encapsulated PDF helpers (`pdf2dcm` / `dcm2pdf`-like) — DONE

Shipped `vyna-dcm-pdf` (`pdf_to_dataset` + `ExtractedPdf` via
`pdf_from_dataset`): real Type 1s, always-present Type 2s, recorded
document length for exact padding removal, UTF-8 charset only when
needed — plus the `pdf2dcm` (IOD self-check) and `dcm2pdf` CLIs.
Accepted: built instances validate clean in all native syntaxes,
odd-length PDFs round-trip byte-exactly, 137/137 tests, clippy
clean. Out of scope (documented): CDA, PDF/A or magic enforcement.

## 4. Priority 1: interface-engine depth

- **Tag-editor CLI** (`dcmodify`-like) — DONE: shipped the
  `dcmodify` bin (modify/insert/erase, keyword/numeric tags,
  VR-aware values, `@path` binary input, file-meta UID sync,
  in-place or `--output`, transfer-syntax override). Accepted:
  7 CLI unit tests + edit→`dcmvalidate`→fix loop, 144/144 tests,
  clippy clean. Out of scope (documented): sequence setting.
- **Transcode CLI/API** (`dcmconv`/`dcm2dcm`-like) — DONE: shipped
  `transcode_dataset` (11-syntax matrix incl. big endian, RGB
  normalization, per-target photometrics, refragmentation, lossy
  history + fresh UID on lossy targets) plus the `dcm2dcm` CLI with
  quality knobs; also fixed `.50` color to 4:2:2 sampling and native
  422 length math. Accepted: 16 transcode tests incl. exact lossless
  matrix, 161/161 tests, clippy clean. Out of scope (documented):
  palette expansion, `YBR_PARTIAL_*`, SOP-constrains-syntax checks.
- **DICOMDIR read/create** (`dcmdir`/`dcmgpdir`, pydicom filesets)
  — DONE: shipped `read_dicomdir` (offset-exact link following,
  skips reported) + `build_dicomdir` (grouped records, measured
  two-pass offsets) plus the `dcmdir` create/`--list` CLI.
  Accepted: every emitted link verified to point at an item tag,
  10 dicomdir tests, 171/171 tests, clippy clean. Out of scope
  (documented): `MRDR` following, pydicom cross-read (no
  independent reader available here).
- **WADO-URI client** — DONE: shipped `WadoUriRequest` +
  `wado_uri`/`wado_uri_dataset` (validated UIDs/ranges, verbatim
  rendering knobs, `application/dicom` gate on the parsed form).
  Accepted: param unit tests + stub-server e2e (DICOM parse,
  rendered passthrough, 404 surfacing), 176/176 tests, clippy
  clean. Out of scope (documented): WADO-URI server side.
- **Missing SCU tools** (`movescu`, `getscu`, `mppsscu`,
  `stgcmtscu`) — DONE: shipped all four (shared `-k` query builder
  with `findscu`, generated UIDs/dates where sensible) plus the
  `Association::storage_commit` request-with-report helper.
  Accepted: 13 CLI unit tests + 2 loopback commitment tests, live
  C-MOVE/C-GET/MPPS/commitment-request legs against a scripted peer
  (0x0000; retrieved files are well-formed Part-10), 191/191 tests,
  clippy clean. Out of scope (documented): N-EVENT-REPORT emission
  through our SCP framework (no post-response hook exists, so the
  report leg is verified by scripted loopback instead), and C-GET
  Role Selection proposals against strict archives.
- **DICOM Conformance Statement** — DONE: shipped
  `docs/generate-conformance.py` emitting PS3.2-structured
  `docs/conformance.md` (per-tool AE specs, association/TLS/media/
  web/charset/security sections, SOP + transfer-syntax annexes with
  UIDs). Accepted: every claim extracted from sources with
  fail-loud drift asserts, deterministic `--check` mode wired into
  a `conformance_fresh` gate test, 192/192 tests, clippy clean.
  Out of scope (documented): SCU-side Role Selection proposals
  (the SCP already echoes proposed roles).

## 5. Priority 2: deployment and robustness

- **De-identification profiles** (PS3.15 E, dcm4che `deidentify`)
  — DONE: shipped `vyna-dcm-deid` (`Deidentifier` + 174-tag
  `BASIC_PROFILE` with dummy/zero/remove/UID-remap/date-shift
  actions, sequence recursion, private/overlay rules, burned-in
  refusal) plus the batch `deidentify` CLI. Accepted: 14 engine
  unit tests + doc test, 3 CLI tests, ECG stays IOD-valid with
  identical samples post-run, 211/211 tests, clippy clean. Out of
  scope (documented): full E.1 transcription, coded profile
  claims, free-text scrubbing, charset conversion, UID-map
  persistence, pixel redaction.
- **ATNA audit-trail emission** (dcm4che `audit`/syslog) — DONE:
  shipped `vyna-dcm-atna` (RFC 3881 message builders with pinned
  code triples, `udp`/`tcp`/`tls` syslog delivery) plus the
  `auditsend` CLI and wired `storescp`/`storescu`/`findscu`
  emission (best-effort, status-mapped outcomes). Accepted: 11
  lib unit tests + doc test (incl. TLS loopback and untrusted
  rejection), 8 CLI/wiring tests, live UDP/TCP/TLS ARR-stub runs
  with XML well-formedness check, 233/233 tests, clippy clean.
  Out of scope (documented): peer socket IPs in handler audits,
  movescu/getscu/mppsscu/stgcmtscu wiring, ARR (server) side.
- **Specific Character Set conversion** (DCMTK `oficonv`-like)
  — DONE: shipped `codec::charset` (`Decoding` + whole-value
  `decode_text`/`encode_text` over `encoding_rs`, wired through
  decode/encode with sequence inheritance) covering the `ISO_IR`
  Latin/Greek/Arabic/Hebrew/Cyrillic/Thai sets, UTF-8, JIS X 0201,
  Japanese ISO-2022 pairs, Korean ISO 2022 IR 149, and GBK/GB18030.
  Accepted: CPython-oracle vectors, 4 dataset round-trip tests,
  `dcmdump` JP/CN smoke runs, 242/242 tests, clippy clean. Out of
  scope (documented): ISO 8859-10/13/14/15/16, UTF-16, vendor code
  pages.
- **XML presentation** (`dcm2xml`/`xml2dcm`) — DONE: shipped
  `codec::xml` (PS3.19 Native DICOM Model: `Value`/`PersonName`/
  `Item`/`InlineBinary`, `DS` lexical preservation, strict VR
  child checks, liberal keyword/numbering) plus the `dcm2xml`
  (`--with-meta`) / `xml2dcm` (`--transfer-syntax`, encapsulated
  re-fragment) CLIs. Accepted: 4 round-trip/rejection tests, JP
  CJK round-trip identical, 246/246 tests, clippy clean. Out of
  scope (documented): `BulkData` resolution, DCMTK `<element>`
  format.
- **UPS / IAN** — EVALUATED (2026-10-12): demand gate not met,
  no code shipped. Verified reusable on adoption day: SCU
  `n_create`/`n_set`/`n_get`/`n_action`/`n_delete`/
  `n_event_report` + `storage_commit` N-ACTION/report-wait, SCP
  `on_n_*` handler hooks, UPS tags in the generated dictionary.
  Adoption plan when demanded: UPS Push/Pull/Watch/Event + IAN SOP
  Class UIDs, UPS state machine (transitions, locking,
  subscriptions) with an SCP-side store, `upsscu` CLI, IAN
  emission, verification against a real peer (dcm4chee UPS SCP) —
  self-loopback alone proves little for a workflow protocol.

## 6. Cross-toolkit comparison rounds

- **dicom-rs comparison** — DONE: shipped the three gaps found —
  the `GB2312` Specific Character Set term (via its GBK superset,
  with a CPython oracle vector), single-frame PNG export/import
  (`codec::img` over the `image` crate plus `dcm2img`/`img2dcm`
  CLIs; MONO 8/16-bit and RGB 8-bit, no windowing), and a vendored
  real-file regression corpus (`vendor/dicom-corpus`, 5 MIT
  pydicom-data files + README) with `codec::corpus` tests. The
  corpus immediately paid off: RLE MSB-first component-major
  segment order (was little-endian), even-length segment padding
  tolerated/emitted, bogus basic offset tables validated with
  fragment-per-frame fallback, subsampled (422) RLE refused.
  Accepted: oracle vectors, CLI round-trip byte-identical,
  cross-codec agreement tests, 256/256 tests, clippy clean. Out of
  scope (documented): windowing/display, `dcm2img` multi-frame.
- **dcm4che/DCMTK comparison** — DONE: shipped the two gaps found —
  QR/MWL SCP (`net::qrdb` file-backed index with PS3.4 matching
  and level grouping, `findscp` with QR FIND/MOVE/GET plus
  `--route` AE table, `mwlscp` worklist FIND; `get_with` threaded
  with each instance's transfer syntax so `getscu` transcodes
  encapsulated arrivals, C-GET storage proposals extended) and
  DICOMweb CLIs (`qidors` with `-k` keys/limit/offset/text-JSON,
  `wadors` instance/metadata/frames/bulkdata, `stowrs` multipart
  store, `wadorsd` stub server over a directory). Accepted: 11
  qrdb unit tests + 4 SCP tests, 4 web loopback tests over real
  HTTP, live findscp/move/getscu and wadorsd CLI smokes, 271/271
  tests, clippy clean. Out of scope (documented): persistent
  archive behind `wadorsd` (STOW lands in memory), UPS/IAN (§5).
- **Post-0.2.0 hardening batch** — DONE: the eight follow-ups from
  the second comparison pass — `dcmodify` sequence paths, ATNA
  wiring for the remaining SCUs (echo excepted), C-CANCEL both
  sides + `TCP_NODELAY`, encapsulated `img2dcm`, `storescp`
  `--exec`, `mppsscp`, and `stgcmtscp` over a new SCP-initiated
  N-EVENT-REPORT framework. Accepted: unit + loopback tests per
  item (incl. full commitment Push handshake and cancel
  truncation), live CLI smokes for each, 298/298 tests, clippy
  clean. Out of scope (documented): `dump2dcm`, `mkkos`,
  relational C-FIND, persistent QR index, UPS/IAN (still
  demand-gated).

## 7. Deliberate non-goals

Image display/windowing, RT/SR specialty modules, print management,
HL7 (covered by the RS7 library), LDAP configuration management,
media burning beyond DICOMDIR, FIPS mode.
