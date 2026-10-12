# Changelog

All notable changes to VynaDCM are documented here. Versions follow
the workspace crate version (`Cargo.toml`).

## [Unreleased]

### Added

- Full PS3.6 2026d dictionary (5,239 entries, retired flags,
  generated `tags` constants) replacing the 266-entry curated table;
  regenerate with `docs/generate-dictionary.py`.
- Private dictionary (424 creators / 8,860 entries) with
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
  actions over a curated 174-tag table, sequence recursion,
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
