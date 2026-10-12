# VynaDCM Architecture

Dependency direction (each layer only depends on layers below it):

```text
vyna-dcm (facade: re-exports + bins + examples)
├── vyna-dcm-ecg ──┐
├── vyna-dcm-web ──┤
├── vyna-dcm-iod ──┤
├── vyna-dcm-pdf ──┤
└── vyna-dcm-net ──┴──> vyna-dcm-codec ──> vyna-dcm-core
```

`core` is dependency-light (thiserror + uuid, plus std). `codec` adds
serde_json/flate2/base64. `net`/`web` add tokio; TLS adds
tokio-rustls/rustls; the web client adds reqwest; the web server adds
hyper. There are no DICOM dependencies outside this workspace.

## vyna-dcm-core — data model

- `tag.rs` — `Tag(GGGG,EEEE)`, group/element accessors, delimiter tags.
- `vr.rs` — `Vr` enum: all 35 Value Representations (34 standard +
  retired `SI`) with text/binary classification and fixed-width sizes.
- `value.rs` — `Value` (typed multi-value storage incl. sequences and
  pixel fragments), `DataElement`, `FragmentSequence`.
- `dataset.rs` — `DataSet`: ordered element map with typed getters
  (`string`, `uint16`, `sequence`, …) and builders (`put_text`,
  `put_sequence`, …).
- `dictionary.rs` + `dictionary_generated.rs` — full PS3.6 registry:
  5,239 `DictEntry`s (tag, VR, VM, keyword, name, retired flag) sorted
  for binary search, plus hand-written and generated `tags` constants.
  Regenerate with `docs/generate-dictionary.py`.
- `private_generated.rs` — private dictionary: 424 creators, 8,860
  entries keyed by creator/group/suffix with wildcard fallbacks;
  `lookup_private` + `Tag` reservation mechanics.
- `transfer_syntax.rs` — `TransferSyntax`: 16 UIDs with wire properties
  (endianness, implicit/explicit VR, deflated, encapsulated). `Default` is
  Explicit VR Little Endian; Implicit VR LE is documented as the DICOM
  default per the standard.
- `uid.rs` — SOP Class UID constants (`sop_class`), well-known UIDs,
  `is_valid_uid`, `new_uid` generation.
- `person_name.rs`, `datetime.rs` — `PersonName` components, `Da`/`Tm`
  validation.

## vyna-dcm-codec — encoding, files, pixels, JSON

- `decode.rs` / `encode.rs` — dataset codec for implicit/explicit VR
  little endian, explicit VR big endian (retired but supported), and
  deflated; resolves context-dependent VRs (`ox`/`px`/`xs`, US/SS)
  from decoded siblings and the dictionary.
- `part10.rs` — Part-10 file format: preamble + `FileMeta` (group 0002)
  + dataset; `read_part10` / `write_part10`.
- `rle.rs` — from-scratch RLE Lossless frame codec (`encode_frame` /
  `decode_frame`).
- `json.rs` — DICOM JSON Model (PS3.18 F): `dataset_to_json` /
  `dataset_from_json`, with `InlineBinary` for byte VRs.
- `jpeg.rs` / `jpeg_lossless.rs` / `jpeg_ls.rs` / `jpeg_2000.rs` —
  bundled `PixelCodec` backends (`.50`, `.70`, `.80`/`.81`, `.90`/`.91`).
- `transcode.rs` — `transcode_dataset` across the native/RLE/JPEG
  matrix (RGB normalization, per-target photometrics, lossy history).
- `dicomdir.rs` — `read_dicomdir` / `build_dicomdir` (measured
  offsets, link following, reported skips).
- `lib.rs` — `decode_pixels` / `decode_pixels_with` / `encode_pixels`:
  native + RLE + raw deflated in-tree; JPEG families via the passed
  `PixelCodec` (`BundledCodec::default()` covers all six bundled
  syntaxes; `Error::UnsupportedTransferSyntax` if none is installed).
- `dump.rs` — `dump_dataset` human-readable text dump (backs `dcmdump`).

## vyna-dcm-net — DIMSE networking

Async (tokio) upper layer + DIMSE, usable as SCU, SCP, or both.

- `pdu.rs` — Upper Layer PDU codec: A-ASSOCIATE-RQ/AC/RJ, P-DATA,
  RELEASE, ABORT, including max-PDU chunking.
- `association.rs` — `Association`: TCP connect/request, `send`/`recv`
  over negotiated contexts, release/abort, message-ID counter.
  `recv` returns `DimseEvent::Message(Box<DimseMessage>)` (boxed: a
  message is ~280 bytes) or `DimseEvent::Released`.
- `dimse.rs` — `Command` builders/parsers for C-ECHO/C-STORE/C-FIND/
  C-MOVE/C-GET/C-CANCEL plus all N-services; `status` predicates
  (success/warning/failure/pending/cancel) and `priority`.
- `scu.rs` — SCU operations on `Association`: `echo`, `store`,
  `find_all`/`find_with`/`find_cancelable`, `move_to`, `get_with`
  (with C-STORE sub-operation handler), N-CREATE/SET/GET/ACTION/
  DELETE/EVENT-REPORT.
- `service.rs` — SCP side: `propose` (odd context IDs), `negotiate`
  (AE/title/TS gating via `AcceptorOptions`), `accept` + `dispatch` to
  the `Handlers` trait, C-MOVE destination routing through
  `ScpMoveConfig`, C-GET delivery with C-CANCEL abort.
- `worklist.rs` — MWL/MPPS dataset builders and Storage Commitment
  request/result helpers (`CommitmentResult`).
- `tls.rs` (feature `tls`) — `TlsAcceptor`/`TlsConnector` over
  tokio-rustls; `Association::request_over` runs DIMSE on any stream.

Presentation negotiation accepts transfer syntaxes in SCU-offered order
filtered by `accepted_transfer_syntaxes` (default: all 16 understood).

## vyna-dcm-web — DICOMweb (PS3.18)

- `client.rs` — `DicomWebClient` (reqwest + rustls): QIDO-RS search
  (studies/series/instances), WADO-RS (file/dataset/metadata/frames/
  bulkdata), STOW-RS, WADO-URI (raw/dataset).
- `multipart.rs` — from-scratch `multipart/related` encoder/decoder.
- `server.rs` — framework-agnostic core: `dispatch` routes QIDO/WADO/
  STOW requests against any `DicomWebStore`; `MemoryStore` provided.
- `hyper_server.rs` (feature `server`) — hyper HTTP/1.1 adapter over
  the dispatch core.

## vyna-dcm-ecg — ECG mapping for VynaECG

- `model.rs` — `EcgLead` (samples in LSB + sensitivity) with
  physical-unit conversion, `EcgRecording` (leads + sampling rate),
  `STANDARD_12_LEAD` labels with verified MDC codes.
- `waveform.rs` — `recording_to_waveform_item` /
  `recording_from_waveform_item` (Waveform Sequence multiplex group),
  `recording_to_dataset` (complete 12-Lead/General ECG SOP instance),
  `EcgInstanceMeta` patient/study attributes.

The physics convention follows PS3.3 C.10.9:
`physical = sample × sensitivity × correction_factor + baseline`
(sensitivity in physical units per LSB, usually mV).

## vyna-dcm-iod — IOD validation

- `iod_generated.rs` — 18 IOD tables (CR/CT/MR/MG/DX/US/SC/PT/NM/XA/
  VL/KO/PDF/ECG) from NEMA PS3.3 via `docs/generate-iod.py` (89
  modules, macro-expanded, strictest-type merge).
- `lib.rs` — `validate(ds, sop_uid, ts)`: Type 1/2 presence, VR/VM
  checks, 7 conditional rules; `Finding`/`Report` in tag order.

## vyna-dcm-pdf — Encapsulated PDF mapping

- `document.rs` — `PdfInstanceMeta` (identity + document defaults),
  `pdf_to_dataset` (validatable SOP instance), `pdf_from_dataset`
  (`ExtractedPdf`: byte-exact bytes + metadata), `file_meta_for_dataset`.

## vyna-dcm — facade, tools, examples

- `lib.rs` — re-exports `core`/`codec`/`iod`/`pdf` always;
  `net`/`web`/`ecg` behind same-named features; common types
  (`DataSet`, `Tag`, `Vr`, `tags`, codec entry points) at the root.
- `src/bin/` — 18 CLIs (feature `cli`; network tools also require `net`):
  `echoscu`, `echoscp`, `storescu`, `storescp`, `findscu`, `movescu`,
  `getscu`, `mppsscu`, `stgcmtscu`, `dcmdump`,
  `dcm2json`, `json2dcm`, `dcmvalidate`, `pdf2dcm`, `dcm2pdf`, `dcmodify`,
  `dcm2dcm`, `dcmdir` (`common.rs` holds shared arg parsing).
- `examples/` — `ecg_write` (VynaECG pattern), `wado_fetch` (DICOMweb
  client), `link_route` (VynaLink store-and-forward chain: C-STORE in,
  routes to a downstream SCP).

## Cross-cutting conventions

- **Errors**: each crate has `error.rs` with a thiserror `Error` and a
  `Result<T>` alias; `#![warn(missing_docs)]` is on everywhere.
- **No panics on input**: decoders validate lengths/VRs and return
  errors; PDU sizes are capped (`max_incoming_pdu`, default 64 MiB).
- **Determinism**: encoders emit canonical byte order; dataset, Part-10
  (`round_trips_part10`, `round_trips_implicit_and_deflated`) and JSON
  (`round_trips_json_model`) round-trips are covered by tests, as is a
  live C-STORE loopback (`loopback_echo_store_find`).
- **Wire interop**: beyond loopback tests, the SCU has been exercised
  live against a third-party DICOM endpoint (C-ECHO `0x0000`, well-formed
  C-FIND responses).

## Test layout

167 unit tests live next to the code (`#[cfg(test)]` modules, incl. CLI
parsers) plus 2 integration tests (`tests/`), 7 doc tests
on the crate roots. Network tests bind `127.0.0.1:0` (ephemeral ports)
with in-process SCP peers — no fixed ports, no external services.
Feature-gated tests (`tls`, `server`) compile out with their features.
