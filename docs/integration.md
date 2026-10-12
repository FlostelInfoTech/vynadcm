# VynaDCM Integration Guide

| Field | Value |
|---|---|
| Document | Integration guide (living) |
| Version | 0.2.0 |
| Date | 2026-10-12 |
| Status | Living draft — updated with every release |
| Applies to | VynaDCM workspace v0.2.0 |

Revision history: 0.1.0 (2026-10-11) — initial issue. Every snippet
below is compile-checked against the workspace (see §12);
0.2.0 (2026-10-12) — version bump, snippets re-checked.

Audience: Rust developers embedding VynaDCM in VynaECG and VynaLink.
Operators should start with `docs/user-guide.md`.

## Contents

- [1. Adding the dependency](#1-adding-the-dependency)
- [2. Files: read, edit, write](#2-files-read-edit-write)
- [3. DICOM JSON](#3-dicom-json)
- [4. SCU: echo, store, find](#4-scu-echo-store-find)
- [5. SCP: serve DIMSE](#5-scp-serve-dimse)
- [6. TLS (PS3.15)](#6-tls-ps315)
- [7. MWL, MPPS, Storage Commitment](#7-mwl-mpps-storage-commitment)
- [8. DICOMweb client](#8-dicomweb-client)
- [9. DICOMweb server](#9-dicomweb-server)
- [10. ECG end-to-end (VynaECG recipe)](#10-ecg-end-to-end-vynaecg-recipe)
- [11. JPEG pixel codecs (bundled + overrides)](#11-jpeg-pixel-codecs-bundled--overrides)
- [12. Cross-cutting notes](#12-cross-cutting-notes)

## 1. Adding the dependency

```toml
[dependencies]
vyna-dcm = { git = "https://gitlab.flostel.com/alexshao/VynaDCM", version = "0.1" }
tokio = { version = "1", features = ["rt-multi-thread", "macros", "net"] }
```

Requires Rust 1.98+, edition 2024. `core` + `codec` are always
included; gate the rest by feature:

| Consumer profile | Features |
|---|---|
| Minimal (files, JSON, datasets) | `default-features = false`, no features |
| VynaECG (ECG + store/query to PACS) | `default-features = false, features = ["ecg", "net"]` |
| VynaLink (DIMSE + DICOMweb routing) | `default-features = false, features = ["net", "web"]` |
| Full workspace (libraries + CLI tools) | defaults (`net`, `web`, `ecg`, `cli`) |

Sub-features: `vyna-dcm-net` without `tls` drops TLS (plain TCP only);
`vyna-dcm-web` without `server` drops the hyper front end (client +
dispatch core remain).

## 2. Files: read, edit, write

```rust
use vyna_dcm::{FileMeta, TransferSyntax, Vr, read_part10, tags, write_part10};

let bytes = std::fs::read("in.dcm").unwrap();
let (_meta, mut ds) = read_part10(&bytes).unwrap();

ds.put_text(tags::PATIENT_NAME, Vr::Pn, "Doe^Jane");

let ts = TransferSyntax::ExplicitVrLittleEndian;
let meta = FileMeta::new(
    ds.string(tags::SOP_CLASS_UID).unwrap(),
    ds.string(tags::SOP_INSTANCE_UID).unwrap(),
    ts,
);
let out = write_part10(&meta, &ds).unwrap();
std::fs::write("out.dcm", out).unwrap();
```

Key dataset APIs: `string`, `uint16`, `sequence` (typed getters
returning `Option`), `put_text`, `put_uid`, `put_sequence`,
`put_null` (empty element, e.g. Q/R return keys), `remove`, `get`.

## 3. DICOM JSON

```rust
use vyna_dcm::{dataset_from_json, dataset_to_json};

let json = dataset_to_json(&ds); // serde_json::Value, PS3.18 F
let back = dataset_from_json(&json).unwrap();
```

Byte VRs travel as `InlineBinary` (base64). Note: a Part-10 →
JSON → Part-10 round-trip normalizes `DS` float formatting
(`500` → `500.0`); values stay numerically identical.

## 4. SCU: echo, store, find

```rust
use vyna_dcm::core::sop_class;
use vyna_dcm::net::service::propose;
use vyna_dcm::net::{AeTitle, Association, AssociationOptions, priority, status};
use vyna_dcm::TransferSyntax;

let mut assoc = Association::request(
    &AssociationOptions::default(),
    &AeTitle::new("VYNA_SCU").unwrap(),
    &AeTitle::new("PACS").unwrap(),
    "127.0.0.1:11112",
    propose(
        &[sop_class::VERIFICATION, sop_class::CT_IMAGE],
        &[TransferSyntax::ExplicitVrLittleEndian],
    ),
)
.await
.unwrap();

assert_eq!(assoc.echo(sop_class::VERIFICATION).await.unwrap(), status::SUCCESS);

let st = assoc
    .store(&ds, class_uid, inst_uid, priority::MEDIUM)
    .await
    .unwrap();
assert_eq!(st, status::SUCCESS);
assoc.release().await.unwrap();
```

C-FIND with return keys:

```rust
use vyna_dcm::{DataSet, Vr, tags};

let mut q = DataSet::new();
q.put_text(tags::PATIENT_ID, Vr::Lo, "E123");
q.put_null(tags::PATIENT_NAME, Vr::Pn);
let (st, matches) = assoc
    .find_all(sop_class::STUDY_ROOT_FIND, &q, priority::MEDIUM)
    .await
    .unwrap();
```

C-MOVE / C-GET return `MoveResult { status, remaining, completed,
failed, warning }`; `get_with` takes an `on_store(class, inst,
dataset) -> u16` callback for incoming objects. N-services
(`n_create`, `n_set`, `n_get`, `n_action`, `n_delete`,
`n_event_report`) return `(status, Option<DataSet>)`.

## 5. SCP: serve DIMSE

Implement `Handlers` (every method has a default; echo succeeds, the
rest answer “not supported” / failure):

```rust
use std::sync::Arc;
use tokio::net::TcpListener;
use vyna_dcm::net::AeTitle;
use vyna_dcm::net::service::{
    AcceptorOptions, Handlers, RequestContext, ScpOptions, accept,
};
use vyna_dcm::net::status;
use vyna_dcm::{DataSet, FileMeta, write_part10};

struct StoreScp;

impl Handlers for StoreScp {
    async fn on_store(&self, ctx: &RequestContext<'_>, dataset: DataSet) -> u16 {
        let class = ctx.command.affected_sop_class_uid.clone().unwrap_or_default();
        let inst = ctx.command.affected_sop_instance_uid.clone().unwrap_or_default();
        let meta = FileMeta::new(&class, &inst, ctx.context.transfer_syntax);
        match write_part10(&meta, &dataset) {
            Ok(bytes) => match std::fs::write(format!("{inst}.dcm"), bytes) {
                Ok(()) => status::SUCCESS,
                Err(_) => 0xA700, // out of resources
            },
            Err(_) => 0xA900, // dataset does not match SOP class
        }
    }
}

let listener = TcpListener::bind("127.0.0.1:11112").await.unwrap();
let acceptor = AcceptorOptions {
    ae_title: AeTitle::new("VYNA_SCP").unwrap(),
    ..Default::default()
};
let handlers = Arc::new(StoreScp);
loop {
    let Ok((tcp, _)) = listener.accept().await else { continue };
    let (a, h) = (acceptor.clone(), handlers.clone());
    tokio::spawn(async move {
        accept(tcp, &a, &*h, &ScpOptions::default()).await.ok();
    });
}
```

Tighten negotiation via `AcceptorOptions`: `check_called_ae`,
`accepted_transfer_syntaxes`, `accepted_sop_classes`,
`user_identity_auth` (a callback returning `Some(response)` to accept).

C-MOVE delivery needs `ScpOptions { move_config }` with an
`AeDirectory` (e.g. `HashMapDirectory { map }`), outbound association
options, and a calling AE.

## 6. TLS (PS3.15)

SCU side — handshake first, then run the association over the TLS stream:

```rust
use vyna_dcm::net::tls::TlsConnector;

let connector = TlsConnector::new("pacs.example", None, None).unwrap();
let tls = connector.connect_to("10.0.0.5:11112").await.unwrap();
let mut assoc =
    Association::request_over(&opts, &calling, &called, tls, proposed).await.unwrap();
```

`TlsConnector::new(server_name, extra_ca_pem, client_identity)`:
pass a PEM CA to trust private PKI, or `(cert, key)` PEMs for mutual
TLS. Trust otherwise comes from WebPKI roots.

SCP side — wrap each TCP stream before `accept`:

```rust
use vyna_dcm::net::tls::TlsAcceptor;

let tls = TlsAcceptor::new(&chain_pem, &key_pem, None).unwrap(); // + client CA for mTLS
let stream = tls.accept(tcp).await.unwrap();
accept(stream, &acceptor, &handlers, &scp_opts).await.unwrap();
```

## 7. MWL, MPPS, Storage Commitment

```rust
use vyna_dcm::core::sop_class;
use vyna_dcm::net::worklist::{
    MppsData, mwl_query_for_station, parse_commitment_result, storage_commitment_request,
};

// Modality Worklist C-FIND
let q = mwl_query_for_station("ECG-1");
let (st, items) = assoc.find_all(sop_class::MODALITY_WORKLIST_FIND, &q, priority::MEDIUM).await.unwrap();

// MPPS N-CREATE (IN PROGRESS) → N-SET (COMPLETED)
let mpps = MppsData { status: "IN PROGRESS".into(), ..Default::default() };
let (st, _) = assoc.n_create(sop_class::MODALITY_PERFORMED_PROCEDURE_STEP, &pps_uid, Some(&mpps.to_create_dataset())).await.unwrap();

// Storage Commitment N-ACTION (action type 1)
let (req_ds, txn_uid) = storage_commitment_request(&[(class_uid, inst_uid)]);
let (st, _) = assoc.n_action(sop_class::STORAGE_COMMITMENT_PUSH, &commit_uid, 1, Some(&req_ds)).await.unwrap();
// …later, when the N-EVENT-REPORT arrives:
let (txn, succeeded, failed) = parse_commitment_result(&event_ds);
```

`MppsData` derives `Default`; fill the performed-procedure fields you
need and use `to_create_dataset()` / `to_set_dataset()`.

## 8. DICOMweb client

```rust
use vyna_dcm::web::DicomWebClient;

let client = DicomWebClient::new("https://pacs.example/dicom-web").unwrap();
let studies = client.search_studies(&[("PatientID", "E123")]).await.unwrap();
let (_meta, ds) = client.wado_dataset(study, series, instance).await.unwrap();
let resp = client.stow(Some(study), &[part10_bytes]).await.unwrap();
assert!(resp.status == 200 || resp.status == 202);
```

Auth: `.with_bearer(token)` / `.with_basic(user, pass)` builders.
HTTPS uses the OS certificate store; the shared ring provider is
installed automatically. Frames: `wado_frames(uids, &[1, 2], accept)`;
metadata: `wado_metadata(...)`; bulk: `wado_bulkdata(uri)`.
Legacy PACS: `wado_uri(&WadoUriRequest::new(study, series, object))`
for raw bytes, or `wado_uri_dataset(...)` parsed; set `content_type`,
`transfer_syntax`, `anonymize`, and the rendering knobs as needed.

## 9. DICOMweb server

Serve a `DicomWebStore` (use `MemoryStore`, or implement the six-method
trait over your own archive) behind the hyper front end:

```rust
use std::sync::Arc;
use tokio::net::TcpListener;
use vyna_dcm::web::hyper_server::serve_connection;
use vyna_dcm::web::server::MemoryStore;

let store = Arc::new(MemoryStore::new());
let listener = TcpListener::bind("127.0.0.1:8080").await.unwrap();
let base = "http://127.0.0.1:8080/dicom-web".to_string();
loop {
    let Ok((tcp, _)) = listener.accept().await else { break };
    let (s, b) = (store.clone(), base.clone());
    tokio::spawn(async move {
        serve_connection(s, b, tcp).await.ok();
    });
}
```

To embed in another HTTP framework, call
`dispatch(&store, public_base, &HttpRequest) -> HttpResponse` directly.

## 10. ECG end-to-end (VynaECG recipe)

```rust
use vyna_dcm::ecg::{EcgInstanceMeta, EcgLead, EcgRecording, recording_to_dataset};
use vyna_dcm::PersonName;

// 1. Acquire → model (sensitivity in mV/LSB here: 1 µV/LSB)
let leads = vec![EcgLead::new("II", vec![0, 5, -3], 0.001)];
let rec = EcgRecording::new(leads, 500.0).unwrap();

// 2. Wrap in a SOP instance (UIDs generated when left empty)
let meta = EcgInstanceMeta::new(PersonName::parse("Doe^Jane"), "E123");
let ds = recording_to_dataset(&rec, &meta).unwrap();

// 3. Persist and/or C-STORE it like any other dataset (§2, §4)
```

Read back with `recording_from_waveform_item(&waveform_item)` and
convert with `lead.physical_samples()` (mV). Standard 12-lead labels
and MDC codes: `STANDARD_12_LEAD`, `mdc_code_for_lead("II")`.

## 11. JPEG pixel codecs (bundled + overrides)

Six JPEG transfer syntaxes work out of the box — `.50` baseline,
`.70` lossless, `.80`/`.81` JPEG-LS, `.90`/`.91` JPEG 2000 — with no
plug-in. Native, RLE, and deflated syntaxes need no codec either:

```rust
use vyna_dcm::codec::{decode_pixels, encode_pixels};
use vyna_dcm::TransferSyntax;

// Fragments → raw native-order samples (all frames concatenated)…
let raw = decode_pixels(&ds, TransferSyntax::JpegLsLossless).unwrap();
// …and back to one fragment per frame.
let frags = encode_pixels(&ds, TransferSyntax::JpegLsLossless, &raw, None).unwrap();
```

Tune the lossy knobs through `BundledCodec` (`jpeg.quality`,
`jpeg_ls.near`, `jpeg_2000.lossy_ratio`) and pass it explicitly:

```rust
use vyna_dcm::codec::{BundledCodec, decode_pixels_with};

let mut codec = BundledCodec::default();
codec.jpeg.quality = 95;
let raw = decode_pixels_with(&ds, ts, Some(&codec)).unwrap();
```

To replace a backend (e.g. with a hardware codec), implement
`PixelCodec` and pass it instead:

```rust
use vyna_dcm::codec::{PixelCodec, PixelCodecError, decode_pixels_with};
use vyna_dcm::core::FragmentSequence;
use vyna_dcm::TransferSyntax;

struct MyJpeg;
impl PixelCodec for MyJpeg {
    fn syntaxes(&self) -> &[TransferSyntax] {
        &[TransferSyntax::JpegBaseline8Bit]
    }
    fn decode_frame(
        &self,
        ts: TransferSyntax,
        fragments: &FragmentSequence,
        rows: u16,
        columns: u16,
        samples_per_pixel: u16,
        bits_allocated: u16,
    ) -> Result<Vec<u8>, PixelCodecError> {
        // …decode via your JPEG crate…
        Err(PixelCodecError::Codec("not implemented".into()))
    }
    fn encode_frame(
        &self,
        ts: TransferSyntax,
        raw: &[u8],
        rows: u16,
        columns: u16,
        samples_per_pixel: u16,
        bits_allocated: u16,
    ) -> Result<FragmentSequence, PixelCodecError> {
        Err(PixelCodecError::Codec("not implemented".into()))
    }
}

let raw = decode_pixels_with(&ds, ts, Some(&MyJpeg)).unwrap();
```

With `None` instead of a codec, `decode_pixels_with` rejects
JPEG-family syntaxes with `UnsupportedTransferSyntax`.

## 12. Cross-cutting notes

- **Errors**: each crate exposes `Error`/`Result`; match on variants
  (e.g. `net::Error::NoPresentationContext`) for control flow, render
  with `{e}` for logs. Malformed input always yields `Err`, never panics.
- **Logging**: the crates emit `tracing` events (association lifecycle,
  dispatch failures, C-MOVE routing); bring your own subscriber.
- **Threading**: `Association` is `!Clone`, drive with `&mut` from one
  task; `Handlers`/`DicomWebStore` are `Send + Sync` and shared by
  reference; `core`/`codec`/`ecg` are sync and executor-agnostic.
- **Limits**: tune `AssociationOptions::{max_pdu_len, read_timeout,
  max_incoming_pdu}`; STOW bodies cap at 1 GiB.
- **Snippet verification**: every snippet in §§2–11 is mirrored in a
  temporary example and `cargo check`ed on each doc release; the
  runnable `ecg_write`, `wado_fetch`, and `link_route` examples cover
  the same paths end to end.
