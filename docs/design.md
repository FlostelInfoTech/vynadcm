# VynaDCM Software Design Specification

| Field | Value |
|---|---|
| Document | SDS (living) |
| Version | 0.6.0 |
| Date | 2026-10-15 |
| Status | Living draft — updated with every release |
| Applies to | VynaDCM workspace v0.6.0 |

Revision history: 0.1.0 (2026-10-11) — initial issue; full-dictionary
(P0.1) update applied same day (generated tables, private dict, `SV`);
0.2.0 (2026-10-12) — version bump, no design change;
0.3.0 (2026-10-12) — cancel polling + follow-ups +
TCP_NODELAY design notes;
0.4.0 (2026-10-13) — security/correctness batch notes folded in;
0.5.0 (2026-10-13) — version bump; §8 CI-ladder wording aligned;
0.6.0 (2026-10-15) — version bump to match workspace.

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

1. **Layered, acyclic**: `core ← codec ← {net, web, ecg, iod, pdf,
   deid, atna} ← facade`, with `codec ← vyna-openjp2` (JPEG 2000
   fork) below the codec. A lower layer never names an upper one;
   each crate builds standalone.
2. **Parse, don't validate**: wire bytes become typed values
   (`Tag`, `Vr`, `Command`, PDU structs) at the boundary; downstream
   code works with types, not offsets.
3. **Total functions on input**: every decoder returns `Result`; the
   `#[cfg(test)]` suites assert `Err` (not panic) on truncated,
   over-long, and contradictory input.
4. **Explicit async**: only `net`/`web` are async (tokio); `core`,
   `codec`, `ecg`, `iod`, `pdf`, `deid`, `atna` are sync and
   runtime-free so VynaECG can call them from any executor.
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
| Private dict | 424 creators / 8,898 entries via `lookup_private(creator, group, suffix)` with block + group-wildcard fallbacks; `Tag::{private_block, private_creator_element, private_suffix}` mechanics |
| `TransferSyntax` | 16 UIDs; `is_encapsulated()`, `is_deflated()`, endianness/VR-explicitness; two defaults (FR-CORE-05): enum `Default` = Implicit VR LE (PS3.5 §10), new associations prefer Explicit VR LE |
| `PersonName`, `Da`, `Tm` | `PersonName::parse` is infallible; `Da`/`Tm` validate (`Tm`: `HH[MM[SS[.FFFFFF]]]`, colons optional) |

### 2.2 `vyna-dcm-codec` — encoding

- `decode_dataset(bytes, ts)` / `encode_dataset(ds, ts)`: total over
  the four uncompressed syntaxes (implicit/explicit LE, explicit BE,
  deflated). Encode is canonical: elements in dataset order, correct
  padding (space for text, zero for binary), no group lengths.
- Text values convert per Specific Character Set (0008,0005):
  `charset::parse_specific_character_set` maps the top-level terms
  to a `Decoding` (threaded `&mut` through decode, inheriting into
  sequence items; recomputed per dataset on encode); whole-value
  `decode_text`/`encode_text` so ISO-2022 state spans the value,
  delimiters split in Unicode space afterwards. ASCII stays on a
  fast path except under stateful sets (ISO-2022-JP/KR escapes and
  JIS X 0201 Yen/overline live in ASCII range).
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
  (`.50`), `jpegli` (`.70`), `jpegls` (`.80`/`.81`), and our
  `vyna-openjp2` fork (`.90`/`.91`, see
  `crates/vyna-openjp2/README.md` for the null-`dealloc` patch).
  `.51`, J2K Part 2, and HTJ2K stay unsupported.
- `transcode_dataset(ds, src, dst, opts)`: decode → normalize
  (big-endian swap, planar de-interleave, BT.601 YBR→RGB) →
  re-encode; native/RLE length math is `YBR_FULL_422`-aware (2
  bytes/pixel stored). `.50` color forces 4:2:2 sampling (SOF-checked
  in tests). Target photometrics (`.50`→422, `.90`→RCT, `.91`→ICT),
  ≤64 KiB refragmentation with offset tables, lossy flag/ratio/method
  history, fresh SOP UID on lossy targets.
- `dicomdir::{read_dicomdir, build_dicomdir}`: media directory
  read/create. Both sides use measured sequence-item offsets
  (`decode/encode_dataset_capture_items`, inert unless asked):
  the reader follows next/lower links with loop/dangling checks
  and reports skips; the writer groups patient → study → series,
  copies record keys, and stamps offsets in a layout-stable second
  pass (Explicit LE, `MRDR` not followed).
- `dataset_to_json` / `dataset_from_json`: PS3.18 F; byte VRs use
  `InlineBinary` (base64); `DS`/`IS` accept JSON numbers or strings.
  Known normalization: Part-10 → JSON → Part-10 rewrites DS lexical
  form (`500` → `500.0`); values stay numerically identical.
- `dataset_to_xml` / `dataset_from_xml`: PS3.19 Native DICOM
  Model (`quick-xml` reader); `Value`/`PersonName`/`Item`/
  `InlineBinary` children per VR, `DS` lexical form preserved
  (no float trip, unlike JSON), `OL`/`OV`/`OD`/`OF` numeric like
  the JSON mapping, empty attributes normalize to `Value::Empty`,
  `BulkData` refused.
- `frame_to_png` / `inject_png`: single-frame PNG export/import over
  `Geometry` + `decode_pixels` (MONO 8/16, RGB 8; MONO1 inverted,
  signed offset, no windowing) and the `image` crate (PNG only).

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
- C-MOVE: `on_move_plan` returns a lazy `RetrievePlan` (total +
  storage classes up front); the framework opens one outbound
  association (via `ScpMoveConfig`: `AeDirectory` + options + calling
  AE) proposing every needed storage class, then streams
  load-and-send one instance at a time, emitting pending/final
  responses with sub-operation counts (a failed load counts one
  failed sub-operation while the rest continue). The eager
  `on_move` still works via a default bridge. Without
  `move_config`, C-MOVE fails with Move Destination Unknown.
- C-GET: same instance flow over the *same* association; a C-CANCEL
  for the GET aborts delivery and answers CANCEL. `get_with` hands the
  callback each instance's negotiated transfer syntax so receivers can
  transcode encapsulated arrivals (`getscu` stores Explicit LE).
- QR/MWL SCP (`qrdb.rs`): `QrIndex::scan` walks a directory of Part-10
  files (pixels/overlays stripped, failures skipped); `dataset_matches`
  implements single/wildcard/range/list/sequence matching with level
  grouping; `QrScp`/`MwlScp` implement `Handlers` (FIND/MOVE/GET and
  worklist FIND), MOVE/GET re-reading matched files in full.
- `storage_commit` (`scu.rs`): N-ACTION request (action type 1),
  then a single `recv` for the same-association N-EVENT-REPORT
  (event type 1 only); acknowledges it SUCCESS and parses the
  result. A failed N-ACTION returns `(status, None)` without
  waiting; the association read timeout bounds the wait.
- SCP cancel polling (`service.rs`): C-FIND/C-MOVE poll for C-CANCEL
  between pending responses (1 ms window, final FE00); anything else
  arriving mid-operation is stashed in `Association::pending` for the
  dispatch loop, never swallowed. All sockets set `TCP_NODELAY`
  (Nagle stalls small PDU writes ~40 ms on delayed ACKs).
- SCP-initiated follow-ups (`service.rs`): `on_n_action` may return
  `ScpFollowUp::EventReport`s, emitted after the RSP on success with
  the RSP awaited; `StgCommitScp` (`qrdb.rs`) commits index holdings
  this way, `MppsScp` (`worklist.rs`) persists N-CREATE/N-SET steps
  as Part-10 by MPPS UID.
- SCU CLIs (`movescu`, `getscu`, `mppsscu`, `stgcmtscu`) share
  `common.rs` helpers with `findscu`/`storescp`: `build_qr_query`
  (`-k KEY=value` with derived Query/Retrieve Level) and
  `safe_instance_filename`.
- `dimse::status` predicates partition the 16-bit space into
  success / warning / failure / pending / cancel for response logic.

### 2.4 `vyna-dcm-web` — DICOMweb

- `DicomWebClient`: sync-constructed, `async` ops; base URL + optional
  Basic/Bearer auth; HTTPS via OS trust store (see §7).
  WADO-URI via `WadoUriRequest` (validated params) → raw bytes or
  parsed dataset; rendering knobs pass through verbatim.
- `dispatch(store, public_base, req)`: pure function
  `HttpRequest → HttpResponse` over `&dyn DicomWebStore`; no HTTP
  framework types leak into the core. Unknown routes → 404.
- `DicomWebStore` is sync and `Send + Sync`: `search` (QIDO scope +
  filters + limit/offset), `instance_file` / `study_files` /
  `series_files` (retrieve), `instance_metadata` (+ bulk blobs),
  `instance_frames` (1-based, `Result`: undecodable is `Err`),
  `store_instance` (Part-10 in, IDs out), `put_bulk` / `bulk_data`
  (opaque blob ids for `BulkDataURI`).
- `MemoryStore`: `Mutex`-guarded vectors; replace-on-re-store by SOP
  Instance UID; bulk ids are unpredictable UUIDs with oldest-first
  eviction past 10k blobs / 1 GiB.
- `hyper_server::serve_connection`: adapts one TCP stream to
  `dispatch`; bodies capped at 1 GiB (`MAX_BODY_LEN`). The
  `*_with_auth` variants take a `ServerAuth` (optional bearer
  token) guarding every request.

### 2.5 `vyna-dcm-ecg` — ECG mapping

- `EcgLead { samples: Vec<i32>, sensitivity, units, correction_factor,
  baseline, … }`; `EcgRecording::new` validates rectangular shape
  (non-empty, all leads equal length); conversion rejects
  non-positive or non-finite sampling rates.
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

### 2.8 `vyna-dcm-deid` — de-identification

- `Action::{Dummy, Zero, Remove, RemapUid, ShiftDate,
  ShiftDateOrZero}` per-element actions; `BASIC_PROFILE` is a
  636-row E.1 transcription (406 remove / 63 dummy / 59 zero /
  55 UID remap / 46 date shift / 7 shift-or-zero). Unlisted tags
  keep; sequences recurse; odd groups (private), `0x6000..=0x60FF`
  (overlays), and `0x5000..=0x50FE` (curves) remove by rule.
- `Deidentifier`: dummies (`ANONYMIZED` defaults, PN-aware), one UID
  map per instance (consistent across files), optional whole-day
  `DA`/`DT` offset (Hinnant round-trip; time-of-day kept), dummy
  dates without an offset so Type 1 dates stay valid.
- `apply` refuses `YES` Burned In Annotation before mutating and
  returns a `DeidReport` (counts + kept-value warnings); the
  longitudinal-temporal flag is set factually (`MODIFIED` with an
  offset, `REMOVED` without) only when a shiftable date was seen.
- `deidentify` bin mirrors `dcmodify`'s meta-sync and temp+rename
  in-place writes; multi-file plans resolve in `plan_outputs`.

### 2.9 `vyna-dcm-atna` — audit emission

- `message.rs`: `AuditMessage` builders (activity, begin-transfer,
  transferred, query, deleted, export/import, custom) rendering
  RFC 3881 XML with full escaping; participant roles 110150–153
  (110150 application, 110152 destination, 110153 source),
  study objects with base64 `(class, instance)` details, patient
  objects; code triples pinned by tests.
- `syslog.rs`: `SyslogTarget` URL parsing (`udp`/`tcp`/`tls`,
  default ports 514/6514), RFC 5424 frames at facility audit(13)
  with outcome-mapped severity, octet-counting for TCP/TLS,
  blocking `SyslogSender` (TLS: WebPKI + optional PEM CA,
  hostname verification, shared ring provider install).
- Bins share `AuditArgs` (`--audit-server/--audit-source/
  --audit-ca`); `emit_best_effort` never fails the clinical path;
  `outcome_for_status` maps DIMSE statuses to 0/4/12.

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
PackBits stream per segment, component-major and MSB-first within each
component (16-bit mono is `[high, low]`). PackBits: replicate runs of
≥3 bytes (max 128), else literal runs (max 128); `0x80` header is a
no-op. Segments are even-length (one pad byte tolerated past the known
plane length); basic offset tables are validated (RLE header / JPEG
magic at each entry) with fragment-per-frame fallback for bogus tables.
Subsampled (422) storage has no segment form and is refused.

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
app → Association::request(opts, calling, called, addr, propose([sop…], [ts…])?)   // propose: Result, errs past 128 SOP classes
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

Inbound SCP `on_store` acts as SCU: opens an association to the
downstream PACS and re-STOREs the same bytes (receive → forward,
one downstream association per inbound instance in the example;
production code should pool).

## 5. Error taxonomy

Each crate except `iod` has `error.rs` (`thiserror::Error` +
`Result<T>` alias); `iod::validate` returns `Option<Report>` instead
(unknown SOP Classes yield `None`):

- `core`: `UnknownVr`, `InvalidTag`, `InvalidUid`, `Value`,
  `MissingAttribute`, `Parse`; dictionary misses surface as `Option`,
  not errors.
- `codec`: `Truncated`, `UnknownVr`, `MissingDictionaryVr`,
  `BadUndefinedLength`, `BadSequence`, `BadFragments`, `BadValue`,
  `Unencodable`, `NotPart10`, `BadFileMeta`,
  `UnsupportedTransferSyntax`, `Rle`, `Deflate`, `Json`, `Base64`,
  `Xml`, `Image` (plus a `Core` passthrough).
- `net`: `Io`, `BadPdu`, `UnexpectedPdu { expected, got }`,
  `Rejected`, `Aborted`, `Released`, `NoPresentationContext`,
  `UnknownContext`, `BadCommand`, `DimseStatus`, `Timeout`,
  `MessageTooLarge { limit }`, `TooManyMatches`, `MatchesTooLarge`,
  `TooManyResponses`, `TooManyStashed`, `StashedTooLarge`,
  `BadAeTitle`, `Tls`, `UnknownMoveDestination` (plus
  `Codec`/`Core` passthroughs).
- `web`: `Http`, `Status`, `Multipart`, `Json`, `BadRequest` (plus
  `Codec`/`Core` passthroughs).
- `ecg`: `Waveform`, `Samples` (ragged leads / bad rate), `Missing`,
  `InvalidMeta` (plus `Codec`/`Core` passthroughs).
- `pdf`: `EmptyDocument`, `NotPdf`, `DocumentTooLarge`, `Missing`,
  `NoDocumentBytes`, `InvalidMeta`.
- `deid`: `BurnedInAnnotation`, `InvalidReplacementName`,
  `InvalidReplacementId`.
- `atna`: `BadUrl`, `TlsDisabled`, `Io` (kept `io::ErrorKind`),
  `Transport`, `Tls`.

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
  300 s SCP-side); no global association timer in v0.5.

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
- Minimum TLS 1.2 everywhere (`tls12` features on rustls and
  tokio-rustls; reqwest inherits it from rustls).

## 8. Feature gating

| Feature | Effect when off |
|---|---|
| `net/tls` | `net::tls` module absent; plain-TCP only |
| `web/server` | hyper front end absent; client + `dispatch` core remain |
| facade `net`/`web`/`ecg` | corresponding re-export absent |
| facade `cli` | bins absent (`required-features` skips them) |

Gated tests compile out with their features. The supported matrix
(default, `--all-features`, `--no-default-features`, and each
facade feature off in turn) must all build; CI checks the default
workspace plus the `--no-default-features` ladder
(bare / `cli` / `cli,net` / `cli,web` / `cli,ecg` / `cli,net,web`).

## 9. Invariants and limits

- Message IDs cycle 1…65535, never 0; PDV chunking honors both our
  max PDU and the peer's (capped at 1 MiB sends when peer says 0).
- Incoming PDU cap 64 MiB default (`max_incoming_pdu`); STOW bodies
  1 GiB (`MAX_BODY_LEN`).
- Dictionary `ENTRIES` sorted + unique (test-enforced); `tags`
  constants resolve (spot-checked per group).
- Documentation and lint hygiene: `#![warn(missing_docs)]` in every
  crate; zero clippy warnings; `cargo fmt --check` clean.
