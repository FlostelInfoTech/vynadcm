# VynaDCM Software Design Specification

| Field | Value |
|---|---|
| Document | SDS (living) |
| Version | 0.1.0 |
| Date | 2026-10-11 |
| Status | Living draft — updated with every release |
| Applies to | VynaDCM workspace v0.1.0 |

Revision history: 0.1.0 (2026-10-11) — initial issue; full-dictionary
(P0.1) update applied same day (generated tables, private dict, `SV`).

## Contents

- [1. Design principles](#1-design-principles)
- [2. Component contracts](#2-component-contracts)
- [3. Key algorithms](#3-key-algorithms)
- [4. Sequences](#4-sequences)
- [5. Error taxonomy](#5-error-taxonomy)
- [6. Concurrency and runtime](#6-concurrency-and-runtime)
- [7. TLS design (PS3.15 + HTTPS)](#7-tls-design-ps315--https)
- [8. Feature gating](#8-feature-gating)
- [9. Invariants and limits](#9-invariants-and-limits)

This document details *how* the requirements in `docs/requirements.md`
are met. `ARCHITECTURE.md` stays the one-page layer/module map; this
spec adds component contracts, algorithms, sequences, and invariants.

## 1. Design principles

1. **Layered, acyclic**: `core ← codec ← {net, web, ecg, iod, pdf} ← facade`.
   A lower layer never names an upper one; each crate builds standalone.
2. **Parse, don't validate**: wire bytes become typed values
   (`Tag`, `Vr`, `Command`, PDU structs) at the boundary; downstream
   code works with types, not offsets.
3. **Total functions on input**: every decoder returns `Result`; the
   `#[cfg(test)]` suites assert `Err` (not panic) on truncated,
   over-long, and contradictory input.
4. **Explicit async**: only `net`/`web` are async (tokio); `core`,
   `codec`, `ecg`, `iod`, `pdf` are sync and runtime-free so VynaECG
   can call them from any executor.
5. **One TLS stack**: rustls 0.23 + `ring` everywhere; reqwest 0.13
   runs provider-less and shares the same install (see §7).

## 2. Component contracts

### 2.1 `vyna-dcm-core` — data model

| Type | Contract |
|---|---|
| `Tag` | 32-bit `(group, element)`; `Copy`, ordered; `is_delimiter()` identifies item/sequence delimiters |
| `Vr` | 35 variants (34 standard + retired `SI`); `is_text()`, fixed byte width where applicable |
| `Value` | Sum type: text/numbers (per VR width) / bytes / sequences / fragments / tags / person names; `is_empty()` for QIDO pruning |
| `DataSet` | Insertion-ordered map; typed getters return `Option` (never panic); `put_*` builders; `PartialEq` for round-trip asserts |
| `ENTRIES` / `tags` | 5,239 sorted entries (full PS3.6 2026d + command group); `lookup(Tag)`, `lookup_keyword(&str)`; retired flagged; repeating (`xx`) groups unlisted → `None` |
| Private dict | 424 creators / 8,860 entries via `lookup_private(creator, group, suffix)` with block + group-wildcard fallbacks; `Tag::{private_block, private_creator_element, private_suffix}` mechanics |
| `TransferSyntax` | 16 UIDs; `is_encapsulated()`, `is_deflated()`, endianness/VR-explicitness; `Default` = Explicit VR LE |
| `PersonName`, `Da`, `Tm` | `PersonName::parse` is infallible; `Da`/`Tm` validate (`Tm`: `HH[MM[SS[.FFFFFF]]]`, colons optional) |

### 2.2 `vyna-dcm-codec` — encoding

- `decode_dataset(bytes, ts)` / `encode_dataset(ds, ts)`: total over
  the four uncompressed syntaxes (implicit/explicit LE, explicit BE,
  deflated). Encode is canonical: elements in dataset order, correct
  padding (space for text, zero for binary), no group lengths.
- VR resolution order for implicit VR: delimiter rules → dictionary
  → contextual rules (`resolve_ambiguous` for Pixel/Waveform data,
  `US/SS` from Pixel Representation) → `UN` fallback.
- `read_part10` validates the 132-byte preamble + `DICM` magic, then
  decodes group 0002 as Explicit VR LE and the body per its transfer
  syntax; `write_part10` emits the same layout.
- `decode_pixels(ds, ts)`: native bytes pass through; RLE goes to
  the in-tree frame codec; JPEG families go to the bundled backends
  (`.50`, `.70`, `.80`/`.81`, `.90`/`.91`). `decode_pixels_with`
  takes an explicit `PixelCodec` override (`None` rejects JPEG with
  `Error::UnsupportedTransferSyntax`, as does a codec that declines
  the syntax). Multi-frame JPEG framing prefers the Basic Offset
  Table, else one fragment per frame.
- `encode_pixels(ds, ts, raw, codec)`: the encode mirror — RLE
  built in, JPEG families via the codec, one fragment per frame.
  Bundled backends are pure Rust: `jpeg-decoder`/`jpeg-encoder`
  (`.50`), `jpegli` (`.70`), `jpegls` (`.80`/`.81`), and a vendored
  `openjp2` (`.90`/`.91`, see `vendor/openjp2/README.VYNA.md` for the
  null-`dealloc` patch). `.51`, J2K Part 2, and HTJ2K stay unsupported.
- `dataset_to_json` / `dataset_from_json`: PS3.18 F; byte VRs use
  `InlineBinary` (base64); `DS`/`IS` accept JSON numbers or strings.
  Known normalization: Part-10 → JSON → Part-10 rewrites DS lexical
  form (`500` → `500.0`); values stay numerically identical.

### 2.3 `vyna-dcm-net` — DIMSE

- `Association`: owns one connection. `request()` connects + negotiates;
  `request_over()` runs over any stream (used for TLS); `send/recv`
  frame PDVs against a negotiated context; `next_message_id()` rolls
  1…65535 (never 0); `release()` / `abort()` close cleanly.
- `recv()` returns `DimseEvent::Message(Box<DimseMessage>)` (boxed:
  ~280 bytes) or `Released`. Command and dataset PDVs are reassembled
  before return; cross-context or dataset-before-command PDVs are `Err`.
- SCU ops (`scu.rs`) are methods on `Association`; each resolves its
  presentation context first (`NoPresentationContext` if missing),
  sends, then awaits the matching response ID.
- SCP (`service.rs`): `accept(stream, …)` negotiates once, then loops
  `recv → dispatch`. `dispatch` matches the command field and calls
  one `Handlers` method; handler results map to response primitives.
  Dispatch failure aborts the association (A-ABORT source 2).
- `Handlers` has a default for every method (echo succeeds; everything
  else `SOP_CLASS_NOT_SUPPORTED`, store `PROCESSING_FAILURE`), so SCPs
  implement only what they serve.
- C-MOVE: `on_move` returns instances; the framework opens one outbound
  association (via `ScpMoveConfig`: `AeDirectory` + options + calling
  AE) proposing every needed storage class, C-STOREs each instance,
  and emits pending/final responses with sub-operation counts. Without
  `move_config`, C-MOVE fails with Move Destination Unknown.
- C-GET: same instance flow over the *same* association; a C-CANCEL
  for the GET aborts delivery and answers CANCEL.
- `dimse::status` predicates partition the 16-bit space into
  success / warning / failure / pending / cancel for response logic.

### 2.4 `vyna-dcm-web` — DICOMweb

- `DicomWebClient`: sync-constructed, `async` ops; base URL + optional
  Basic/Bearer auth; HTTPS via OS trust store (see §7).
- `dispatch(store, public_base, req)`: pure function
  `HttpRequest → HttpResponse` over `&dyn DicomWebStore`; no HTTP
  framework types leak into the core. Unknown routes → 404.
- `DicomWebStore` is sync and `Send + Sync`: `search` (QIDO scope +
  filters + limit/offset), `instance_metadata` (+ bulk blobs),
  `instance_frames` (1-based), `store_instance` (Part-10 in, IDs out),
  `put_bulk` / `bulk_data` (opaque blob ids for `BulkDataURI`).
- `MemoryStore`: `Mutex`-guarded vectors; replace-on-re-store by SOP
  Instance UID; bulk ids are hex sequence numbers.
- `hyper_server::serve_connection`: adapts one TCP stream to
  `dispatch`; bodies capped at 1 GiB (`MAX_BODY_LEN`).

### 2.5 `vyna-dcm-ecg` — ECG mapping

- `EcgLead { samples: Vec<i32>, sensitivity, units, correction_factor,
  baseline, … }`; `EcgRecording { leads, sampling_frequency_hz }`
  validates rectangular shape (all leads equal length, rate > 0).
- Physics: `physical = sample × sensitivity × correction + baseline`
  (PS3.3 C.10.9); VynaECG default sensitivity `0.001` (mV units).
- `recording_to_waveform_item`: one multiplex group (all channels;
  `WaveformBitsAllocated` 8 or 16, default 16);
  `recording_to_dataset`: full SOP instance (defaults: 12-lead class,
  generated UIDs via `EcgInstanceMeta::fill_uids`, Modality ECG).

### 2.6 `vyna-dcm-iod` — IOD validation

- `IODS`: 18 SOP Classes × merged module rows, generated from NEMA
  PS3.3 by `docs/generate-iod.py` (macro expansion, strictest-type
  merge; 1C/2C fold to if-present except 7 curated `CondRule`s).
- `validate(ds, sop_uid, ts) -> Option<Report>`: Type 1/2 presence,
  VR-vs-dictionary (primary or `alts`; explicit `UN` warns), VM
  range check for countable values; findings in tag order.
- `dcmvalidate` bin adds file-meta SOP UID consistency and exit
  codes (0 clean, 1 errors, 2 unsupported SOP Class).

### 2.7 `vyna-dcm-pdf` — Encapsulated PDF mapping

- `PdfInstanceMeta` (patient/study/series identity, document
  title/concept, Modality/Conversion/Burned-In defaults `OT`/`WSD`/
  `YES`); empty UIDs are generated via `new_uid`.
- `pdf_to_dataset`: real Type 1s, every Type 2 present (empty when
  unknown), exact length in (0042,0015), `ISO_IR 192` only when
  text needs non-ASCII — output validates clean.
- `pdf_from_dataset -> ExtractedPdf { bytes, mime_type, title,
  sop_instance_uid }`: truncates via (0042,0015) when sane, joins
  fragments, never guesses without a length.
- `pdf2dcm` bin self-validates before writing (exit 0/1);
  `dcm2pdf` bin extracts (exit 0/1).

## 3. Key algorithms

### 3.1 Negotiation (`service::negotiate`)

```text
for each proposed context:
  reject(result=3) if SOP class not in accepted_sop_classes (when set)
  else pick first SCU-offered TS present in accepted_transfer_syntaxes
       (SCU order wins) → result=0 + NegotiatedContext
       else reject(result=4, no acceptable TS)
reject whole association (result=1) when: bad application context,
  bad Called-AE in strict mode, user-identity denied, or zero acceptable
```

### 3.2 RLE frame codec (`codec::rle`)

DICOM RLE: header (`u32` segment count + 15 offsets, 64 bytes) then one
PackBits stream per byte-plane (byte-interleaved: plane `s` holds byte
`s` of every sample). PackBits: replicate runs of ≥3 bytes (max 128),
else literal runs (max 128); `0x80` header is a no-op. Decode validates
offsets against the buffer before reading.

### 3.3 DICOM JSON mapping (`codec::json`)

Tag → 8-hex-digit key; value object `{vr, Value|InlineBinary|
DataFragment, BulkDataURI?}`. Numbers serialize natively; `DS`/`IS`
parse from number or string; text VRs join multi-values; sequences
recurse; `AT` uses `"GGGGEEEE"` strings.

### 3.4 `multipart/related` (`web::multipart`)

Boundary-param parsing per RFC 2046 (quoted or bare); streaming split
that only accepts boundaries at line starts (tolerates boundary-like
bytes inside bodies); per-part headers + bytes preserved verbatim.

## 4. Sequences

### 4.1 SCU C-STORE

```text
app → Association::request(opts, calling, called, addr, propose([sop…], [ts…]))
app → assoc.store(&ds, class, inst, MEDIUM) → status: u16
app → assoc.release()
```

`store` resolves the context, allocates a message ID, sends command +
dataset PDVs (chunked to max PDU), and awaits the matching C-STORE-RSP.

### 4.2 SCP dispatch loop

```text
listener.accept() → TlsAcceptor.accept()? → service::accept(stream, acc, h, scp)
  loop { recv() → Message(m) → dispatch(m) → on_*(…) → send(RSP) | Released → return }
```

### 4.3 VynaLink store-and-forward (`examples/link_route`)

Inbound SCP `on_store` persists the dataset, then acts as SCU:
opens an association to the downstream PACS and re-STOREs the same
bytes (receive → store → forward, one downstream association per
inbound instance in the example; production code should pool).

## 5. Error taxonomy

Each crate has `error.rs` (`thiserror::Error` + `Result<T>` alias):

- `core`: `UnknownVr`, `InvalidTag`, `InvalidUid`, `Value`,
  `MissingAttribute`, `Parse`; dictionary misses surface as `Option`,
  not errors.
- `codec`: `Truncated`, `UnknownVr`, `MissingDictionaryVr`,
  `BadUndefinedLength`, `BadSequence`, `BadFragments`, `BadValue`,
  `Unencodable`, `NotPart10`, `BadFileMeta`,
  `UnsupportedTransferSyntax`, `Rle`, `Deflate`, `Json`, `Base64`
  (plus a `Core` passthrough).
- `net`: `Io`, `BadPdu`, `UnexpectedPdu { expected, got }`,
  `Rejected`, `Aborted`, `Released`, `NoPresentationContext`,
  `UnknownContext`, `BadCommand`, `DimseStatus`, `Timeout`,
  `BadAeTitle`, `Tls`, `UnknownMoveDestination` (plus `Codec`/`Core`
  passthroughs).
- `web`: `Http`, `Status`, `Multipart`, `Json`, `BadRequest` (plus
  `Codec`/`Core` passthroughs).
- `ecg`: `Waveform`, `Samples` (ragged leads / bad rate), `Missing`
  (plus `Codec`/`Core` passthroughs).

Errors carry human-readable context (offsets, tags, UIDs) and never
expose internal buffers.

## 6. Concurrency and runtime

- One tokio task per association/connection; `Association` is
  `!Clone` and driven with `&mut` (no internal locking).
- `Handlers` / `DicomWebStore` / `AeDirectory` are `Send + Sync` and
  shared by reference; `MemoryStore` uses one `Mutex`.
- `codec`/`core`/`ecg` are sync with no I/O: safe to call from
  `spawn_blocking` or any executor.
- Timeouts: per-read `read_timeout` (SCU default 30 s in tools,
  300 s SCP-side); no global association timer in v0.1.

## 7. TLS design (PS3.15 + HTTPS)

- One stack: rustls 0.23 + `ring`; `aws-lc-rs` must never enter the
  graph (verified via `cargo tree -i aws-lc-rs` → no match).
- DIMSE TLS (`net::tls`, feature `tls`): `TlsConnector::new(server,
  extra_ca, identity)` → `connect_to(addr)` → `request_over(tls)` ;
  `TlsAcceptor::new(chain, key, client_ca)` → `accept(tcp)` →
  `service::accept(tls)`. WebPKI roots (v1) + optional extra CA;
  mutual TLS optional both sides.
- HTTPS (reqwest 0.13, `rustls-no-provider`): reqwest ships no
  provider, so `DicomWebClient::new` installs the shared ring
  provider (`install_default`; repeat installs are ignored);
  certificate roots come from the OS store via
  `rustls-platform-verifier`.
- Minimum TLS 1.2 everywhere (`tls12` features on rustls,
  tokio-rustls, hyper-rustls).

## 8. Feature gating

| Feature | Effect when off |
|---|---|
| `net/tls` | `net::tls` module absent; plain-TCP only |
| `web/server` | hyper front end absent; client + `dispatch` core remain |
| facade `net`/`web`/`ecg` | corresponding re-export absent |
| facade `cli` | bins absent (`required-features` skips them) |

Gated tests compile out with their features. The supported matrix
(default / each off / `--all-features` / `--no-default-features`)
must all build; see `README.md`.

## 9. Invariants and limits

- Message IDs cycle 1…65535, never 0; PDV chunking honors both our
  max PDU and the peer's (capped at 1 MiB sends when peer says 0).
- Incoming PDU cap 64 MiB default (`max_incoming_pdu`); STOW bodies
  1 GiB (`MAX_BODY_LEN`).
- Dictionary `ENTRIES` sorted + unique (test-enforced); `tags`
  constants resolve (spot-checked per group).
- Documentation and lint hygiene: `#![warn(missing_docs)]` in every
  crate; zero clippy warnings; `cargo fmt --check` clean.
