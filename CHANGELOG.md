# Changelog

All notable changes to VynaDCM are documented here. Versions follow
the workspace crate version (`Cargo.toml`).

## [Unreleased]

### Fixed

- Documentation/code tally pass: corrected table counts (5,217
  dictionary entries, 8,898 private entries, 191-tag de-id profile,
  84 IOD modules), fixed the stale out-of-scope list, the
  `propose()` integration snippet (now `Result`), the `get_with`
  callback arity, bulk-UUID/server-auth design notes, the error
  taxonomy (added `Xml`/`Image`/`MessageTooLarge`/pdf/deid/atna),
  and per-tool flag references; `wadors --series` help no longer
  names a nonexistent `--study` flag.
- `vyna-dcm-iod` enables `#![warn(missing_docs)]` like every other
  first-party crate (zero warnings).
- Publish-readiness hardening batch (1 critical, 32 major):
  pixel decode/encode now validates geometry with checked math and
  a 1 GiB cap before allocating (zero dims rejected); RLE segment
  math checked with `u64` offsets; `Tm` parse, `dump` truncate,
  and de-id date shifts no longer panic/overflow; multipart parts,
  in-memory DICOMweb/QR stores, QIDO params, QR matches (new
  `A701` status for MOVE/GET), and HTTP connections/conn-lifetime
  are capped; DICOMweb `serve_*` and `EcgRecording::new` return
  typed errors; `Auth`, error enums, and IOD `Kind`/`CondRule` are
  `#[non_exhaustive]`; de-id gates `YES\0`, exposes `uid_map()`,
  stamps fresh markers, and writes temp files symlink-safely;
  `Debug` impls redact secrets; ATNA handles IPv6 UDP, TLS read
  timeouts, XML C0 controls, and syslog header injection; Native
  DICOM Model XML maps illegal controls to U+FFFD; examples carry
  `required-features`; intra-doc links fixed (`cargo doc` clean
  under `-D warnings`).

### Changed

- **Breaking:** MSRV raised from Rust 1.98 to 1.99 (workspace
  `rust-version`, CI images, README/docs).
- **Breaking (pre-1.0):** `EcgRecording::new` returns
  `vyna_dcm_ecg::Error` instead of `String`; DICOMweb `serve_*`
  functions return `vyna_dcm_web::Error` instead of `String`.
  Downstream matches on `Auth`, `Kind`, and `CondRule` need a
  wildcard arm.

### Changed

- The vendored `openjp2` patch is now a `vyna-openjp2` 0.6.1 fork
  crate (workspace member, BSD-2-Clause, published separately) so
  crates.io builds get the null-`dealloc` soundness fix; the
  `[patch.crates-io]` section is gone.
- Test fixtures moved from `vendor/dicom-corpus/` into crate
  `tests/data/` dirs so published crates verify standalone;
  `vendor/` is removed.

## [0.4.0] — 2026-10-13

### Changed (performance)

- JPEG framing joins fragments once instead of twice.
- DICOMweb request bodies are `Bytes` end to end (hyper streams
  into a buffer that freezes without copying); metadata responses
  move blob bytes into the bulk store instead of cloning.
- The `ItemCapture` helper is defined once and shared by the
  Part-10 encoder and decoder.

### Fixed

- Worklist loading skips symlinks, matching the other directory
  walks.
- New CLI coverage: `--help` smoke test over all 32 binaries plus
  JSON/XML converter round-trips and a `dcmdump` functional check.

### Fixed (security)

- DICOMweb client no longer forwards `Authorization` to cross-origin
  `BulkDataURI`s (SSRF/credential leak); same-origin keeps auth.
- Part-10 decoder bounds every read by the enclosing item end
  (overrunning element lengths now fail instead of silently
  consuming the next item); length arithmetic is saturating.
- Planar Configuration 1 conversion handles every frame (frames 2+
  were zero-filled) and errors on truncated input.
- DICOMweb server: optional bearer-token auth (`ServerAuth`,
  `wadorsd --Bearer [REDACTED] plus plaintext/PHI warnings.

### Fixed (denial of service)

- STOW bodies stream against the 1 GiB cap instead of
  collect-then-check; Deflated datasets inflate against a 512 MiB
  cap; DICOM JSON/XML parsing caps nesting depth at 64.
- Associations cap reassembled DIMSE messages (new
  `AssociationOptions::max_message_bytes`, default 1 GiB, new
  `Error::MessageTooLarge`).
- Bulk store uses unpredictable UUID ids with oldest-first eviction
  (10k blobs / 1 GiB).

### Fixed (correctness)

- QIDO `offset`/`limit` paginate matched rows, not raw store order.
- QIDO ranges: open-ended `lo-`/`-hi` and bare `-` match per DICOM
  semantics; PN prefix matching is case-insensitive.
- WADO frame lists reject bad entries with 400 (frame 0 included);
  absent frames stay 404.
- PDU item/user-identity/role lengths error past 65535 instead of
  truncating; `propose` errors past 128 SOP classes instead of
  wrapping context IDs (now returns `Result`).
- MPPS N-SET can no longer rewrite the stamped SOP Class/Instance
  UIDs; poisoned store locks recover instead of panicking.
- DICOM JSON round-trips non-finite floats as `"NaN"`/`"Infinity"`/
  `"-Infinity"` (DS `"NaN"` stays a string) instead of null;
  `InlineBinary` is restricted to bulk-data VRs (JSON and XML).
- XML numeric VRs reject `Item`/`PersonName` children instead of
  dropping them; multipart bodies require the closing delimiter.
- `movescu`/`getscu`/`mppsscu`/`stgcmtscu` require feature `net`
  (cli-only builds were broken).
- Smaller fixes: big-endian PNG export swaps pairs; `float_list`
  covers integer VRs; bad Number of Frames errors (absent still
  means 1); fragment padding only at stream end; ECG recordings
  validate shape/rate; zero Encapsulated Document Length no longer
  empties the document; directory scans never follow symlinks;
  DICOMweb client defaults to a 300 s timeout; TLS server names
  handle IPv6; `dcmodify` in-place writes use unique create-new
  temp files; `storescp` logs patient names only with `--verbose`.

### Added

- `.gitlab-ci.yml` gates (fmt, clippy `-D warnings`, workspace
  tests, feature-combination builds); root `Cargo.lock` is now
  tracked for reproducible binary builds.
- `AcceptorOptions::strict()` plus Called-AE/SOP hardening docs;
  stale doc counts/versions refreshed.

## [0.3.0] — 2026-10-12

### Added

- `dcmodify` sequence paths (`SEQ[i].TAG`, 0-based repeatable hops;
  hops must exist).
- ATNA `--audit-server` wiring for `movescu`, `getscu`, `mppsscu`,
  `stgcmtscu` (plus `mppsscp`/`stgcmtscp` and parse tests for
  `findscp`/`mwlscp`): every DIMSE tool but echo is now wired.
- C-CANCEL both sides: `findscu --cancel N` (requested FE00 exits
  0), SCP honoring mid-stream during C-FIND/C-MOVE (C-GET already
  aborted); `TCP_NODELAY` on all sockets (Nagle stalled each PDU
  ~40 ms; loopback suite 0.37 s → 0.01 s).
- `img2dcm --transfer-syntax` (+ `--quality`/`--near`/
  `--lossy-ratio`): encapsulated RLE/JPEG/JPEG-LS/JPEG 2000
  injection via transcode (also fixes big-endian templates).
- `storescp --exec` receive hooks (`{file}`/`{class}`/`{instance}`/
  `{patient}`/`{study}`/`{caller}`/`{called}`, no shell,
  best-effort).
- MPPS SCP: `net::worklist::MppsScp` (N-CREATE insert, N-SET merge,
  0x0111/0x0112/final-state discipline, Part-10 persistence +
  reload) plus the `mppsscp` CLI.
- Storage Commitment SCP: framework `ScpFollowUp` (SCP-initiated
  N-EVENT-REPORT after the RSP, RSP awaited) + `net::qrdb::`
  `StgCommitScp` (index holdings commit, rest fail 0x0112) plus the
  `stgcmtscp` CLI; 298 tests green.

## [0.2.0] — 2026-10-12

### Added

- Full PS3.6 2026d dictionary (5,217 entries, retired flags,
  generated `tags` constants) replacing the 266-entry curated table;
  regenerate with `docs/generate-dictionary.py`.
- Private dictionary (424 creators / 8,898 entries) with
  `lookup_private` and `Tag` reservation mechanics.
- `Vr::Sv` (signed 64-bit) with decode/encode/JSON support;
  35 VR variants including retired `SI`.
- Bundled JPEG pixel codecs (`BundledCodec` + `encode_pixels`):
  `.50` baseline, `.70` lossless, `.80`/`.81` JPEG-LS, `.90`/`.91`
  JPEG 2000 (pure Rust; `openjp2` vendored with a null-`dealloc`
  patch, see `vendor/openjp2/README.VYNA.md`). Multi-frame decode
  via Basic Offset Table or one fragment per frame.
- New `vyna-dcm-iod` crate: IOD validation for 18 SOP Classes
  (Type 1/2/3 presence, VR/VM checks, 7 conditional rules) with
  generated PS3.3 tables (`docs/generate-iod.py`) plus the
  `dcmvalidate` CLI (exit 0/1/2); 125 tests green.
- `TransferSyntax::is_lossy`, dictionary multi-VR alternates
  (`DictEntry.alts`), and 11 new SOP Class UID constants.
- New `vyna-dcm-pdf` crate: Encapsulated PDF SOP-instance builder
  (`pdf_to_dataset` with generated UIDs, always-present Type 2s,
  recorded document length, UTF-8 charset when needed) and
  extractor (`pdf_from_dataset` → byte-exact `ExtractedPdf`),
  plus the `pdf2dcm` (with IOD self-check) and `dcm2pdf` CLIs;
  137 tests green.
- `dcmodify` CLI: tag modify/insert/erase with keyword/numeric tags,
  VR-aware values (`\` multi-values, `@path` binary input), guarded
  groups, file-meta UID sync, safe in-place replacement, and an
  optional transfer-syntax override; 144 tests green.
- Pixel transcoding (`transcode_dataset` + `dcm2dcm` CLI): native /
  RLE / bundled-JPEG matrix with planar/YBR normalization,
  per-target photometrics, ≤64 KiB refragmentation, lossy history,
  and fresh SOP UIDs on lossy targets; 161 tests green.
- Fixes: `.50` color encodes now use DICOM-mandated 4:2:2 sampling
  (was 4:4:4/4:2:0 depending on quality); native/RLE pixel length
  math is `YBR_FULL_422`-aware.
- DICOMDIR read/create (`read_dicomdir` + `build_dicomdir` +
  `dcmdir` CLI): offset-exact links via measured item capture,
  patient → study → series → instance trees, reported skips;
  171 tests green.
- WADO-URI client (`WadoUriRequest`, `wado_uri`,
  `wado_uri_dataset`) for legacy PACS retrieve; 176 tests green.
- Missing SCU tools: `movescu` (C-MOVE to `--dest`), `getscu`
  (C-GET into `--output-dir`), `mppsscu` (MPPS N-CREATE + N-SET
  with generated UIDs and UTC dates), `stgcmtscu` (Storage
  Commitment `--file`/`--instance` request + report wait), sharing
  the `-k` query builder and filename sanitizer with
  `findscu`/`storescp`; new `Association::storage_commit`
  request-with-report helper; 191 tests green.
- Generated DICOM Conformance Statement
  (`docs/generate-conformance.py` → `docs/conformance.md`,
  PS3.2 structure, per-tool AE specs, SOP/transfer-syntax annexes)
  with extraction asserts and a `conformance_fresh` gate test;
  192 tests green.
- New `vyna-dcm-deid` crate: basic-profile de-identification
  (`Deidentifier` with dummy/zero/remove/UID-remap/date-shift
  actions over a curated 191-tag table, sequence recursion,
  private/overlay rules, burned-in refusal, factual
  longitudinal-temporal flag) plus the batch `deidentify` CLI
  (`--output-dir`/`--output`/`--in-place`, `--date-offset`, dummy
  overrides); de-identified ECG instances stay IOD-valid with
  identical samples; 211 tests green.
- New `vyna-dcm-atna` crate: ATNA audit-trail emission (RFC 3881
  message builders for activity/transfer/query/delete/
  export/import/custom events, `udp`/`tcp`/`tls` syslog delivery
  with hostname verification) plus the `auditsend` CLI and wired
  `storescp`/`storescu`/`findscu` emission (best-effort,
  status-mapped outcomes); 233 tests green.
- Specific Character Set conversion (DCMTK `oficonv`-like):
  `codec::charset` (`Decoding`, whole-value `decode_text`/
  `encode_text` over `encoding_rs`) wired through decode/encode
  with sequence inheritance — `ISO_IR` Latin/Greek/Arabic/Hebrew/
  Cyrillic/Thai sets, UTF-8, JIS X 0201, Japanese ISO-2022 pairs,
  Korean ISO 2022 IR 149, GBK/GB18030; CPython-oracle vectors,
  dataset round-trip tests, `dcmdump` CJK smoke; 242 tests green.
- Native DICOM Model XML (PS3.19 A): `codec::xml`
  (`dataset_to_xml`/`dataset_from_xml` over `quick-xml`, `DS`
  lexical preservation, `PersonName` component groups, `BulkData`
  refused) plus `dcm2xml`/`xml2dcm` CLIs; JP CJK round-trip
  identical; 246 tests green.
- `GB2312` Specific Character Set term (via its GBK superset) with
  a CPython oracle vector.
- Single-frame PNG export/import (`codec::img` over the `image`
  crate) plus `dcm2img`/`img2dcm` CLIs; MONO 8/16-bit and RGB 8-bit,
  no windowing; CLI round-trip byte-identical.
- Vendored real-file regression corpus (`vendor/dicom-corpus`, MIT
  pydicom-data: matched emri_small native/RLE/JPEG-LS/JPEG 2000 +
  SC RGB) with cross-codec agreement and exact XML round-trip tests.
- RLE interop fixes found by the corpus: MSB-first component-major
  segment order (was little-endian), even-length segment padding
  tolerated on decode and emitted on encode, bogus basic offset
  tables validated with fragment-per-frame fallback (RLE and JPEG
  arms), subsampled (422) RLE refused instead of written
  non-standard; 256 tests green.
- Query/Retrieve + Worklist SCP: `net::qrdb` (file-backed index,
  C-FIND single/wildcard/range/list/sequence matching with level
  grouping) plus `findscp` (QR FIND/MOVE/GET, `--route` AE table)
  and `mwlscp` (worklist FIND) CLIs; `get_with` now reports each
  instance's transfer syntax so `getscu` transcodes encapsulated
  arrivals to native, and default C-GET storage proposals cover the
  enhanced/common image classes; 267 tests green.
- DICOMweb CLIs: `qidors` (search, `-k` keys, limit/offset, text/JSON),
  `wadors` (instance/metadata/frames/bulkdata retrieve), `stowrs`
  (multipart store), `wadorsd` (stub server over a directory);
  client↔server loopback tests over real HTTP; 271 tests green.

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

- 85 unit + 5 doc tests green; `clippy --workspace --all-targets`
  zero warnings; `cargo fmt --check` clean; feature matrix verified.
- Live wire interop: C-ECHO `0x0000` against a third-party endpoint.

### Notes

- Toolchain: Rust 1.98+, edition 2024. License: proprietary.
- JPEG/JPEG 2000 pixel decoding is intentionally out of tree
  (plug-in via `PixelCodec`).
- HTTPS trust uses the OS certificate store; DICOM TLS defaults to
  WebPKI roots with optional private CA.
