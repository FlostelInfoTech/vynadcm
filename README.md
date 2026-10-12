# VynaDCM

From-scratch DICOM library in Rust, built for **VynaECG** (ECG management)
and **VynaLink** (interface engine) to replace their current open-source
DICOM dependencies.

No DICOM code is borrowed: the data model, codecs, DIMSE networking,
DICOMweb, and ECG mapping are all implemented in this workspace
(~17k lines). External crates are used only for generic infrastructure
(async runtime, HTTP, TLS, crypto, CLI parsing).

| Crate | Purpose |
|---|---|
| `vyna-dcm-core` | Tags, 35 VRs, `DataSet`, full PS3.6 + private dictionaries, UIDs, 16 transfer syntaxes |
| `vyna-dcm-codec` | Dataset codec, Part-10 files, from-scratch RLE, DICOM JSON, pixel helpers |
| `vyna-dcm-net` | PDU/association, DIMSE SCU + SCP, MWL/MPPS/Storage-Commitment builders, TLS |
| `vyna-dcm-web` | DICOMweb QIDO/WADO/STOW client + server, `multipart/related` codec |
| `vyna-dcm-ecg` | 12-lead ECG model and Waveform Sequence mapping for VynaECG |
| `vyna-dcm-iod` | IOD validation: Type 1/2/3 presence, VR/VM checks, 18 SOP Classes |
| `vyna-dcm-pdf` | Encapsulated PDF SOP-instance builders and extractors |
| `vyna-dcm-deid` | Basic-profile de-identification for research exports |
| `vyna-dcm-atna` | ATNA audit-trail emission over syslog |
| `vyna-dcm` | Facade re-exporting the above, plus 32 CLI tools and 3 examples |

## Documentation

| Document | Description |
|---|---|
| [ARCHITECTURE.md](ARCHITECTURE.md) | One-page layer design and module map |
| [docs/requirements.md](docs/requirements.md) | Requirements specification (SRS) |
| [docs/design.md](docs/design.md) | Design specification (SDS) |
| [docs/integration.md](docs/integration.md) | Integration guide for VynaECG / VynaLink |
| [docs/user-guide.md](docs/user-guide.md) | CLI reference and operator walkthroughs |
| [docs/sbom.md](docs/sbom.md) | Software bill of materials (CycloneDX) |
| [docs/conformance.md](docs/conformance.md) | DICOM Conformance Statement (generated) |
| [docs/roadmap.md](docs/roadmap.md) | Ecosystem comparison and roadmap |
| [CHANGELOG.md](CHANGELOG.md) | Release notes |

## Quick start

Requires Rust 1.98+ (edition 2024).

```sh
cargo build --workspace
cargo test --workspace        # 167 unit + 2 integration + 7 doc tests
cargo clippy --workspace --all-targets
cargo fmt --all -- --check
```

Loopback C-ECHO with the bundled tools:

```sh
cargo run -p vyna-dcm --bin echoscp -- --bind 127.0.0.1:11116 &
cargo run -p vyna-dcm --bin echoscu -- --connect 127.0.0.1:11116
kill %1  # stop the background server
```

Convert a Part-10 file to DICOM JSON and back:

```sh
cargo run -p vyna-dcm --bin dcm2json -- file.dcm > file.json
cargo run -p vyna-dcm --bin json2dcm -- --output roundtrip.dcm file.json
```

Run the worked examples:

```sh
cargo run -p vyna-dcm --example ecg_write   # VynaECG pattern: 12-lead -> Part-10 -> verify
cargo run -p vyna-dcm --example wado_fetch  # DICOMweb: in-process server + client round-trip
# VynaLink pattern: SCP on :11115 forwarding to a PACS on :11114
cargo run -p vyna-dcm --example link_route -- 127.0.0.1:11115 127.0.0.1:11114 PACS
```

## CLI tools (`vyna-dcm`, feature `cli`)

| Tool | Description |
|---|---|
| `echoscu` / `echoscp` | C-ECHO SCU / SCP |
| `storescu` / `storescp` | C-STORE sender / receiver into a directory |
| `findscu` | C-FIND query, prints matches |
| `findscp` | QR C-FIND/C-MOVE/C-GET server over a directory |
| `mwlscp` | Modality Worklist C-FIND server over a directory |
| `movescu` | C-MOVE retrieve to a destination AE |
| `getscu` | C-GET retrieve into a directory |
| `mppsscu` | MPPS N-CREATE + N-SET |
| `mppsscp` | MPPS N-CREATE/N-SET receiver into a directory |
| `stgcmtscu` | Storage Commitment request + report wait |
| `stgcmtscp` | Storage Commitment verdicts from a directory |
| `qidors` | QIDO-RS search, prints matches |
| `wadors` | WADO-RS retrieve (instance/metadata/frames/bulkdata) |
| `stowrs` | STOW-RS store files |
| `wadorsd` | DICOMweb stub server over a directory |
| `dcmdump` | Part-10 file text dump |
| `dcm2json` / `json2dcm` | Part-10 <-> DICOM JSON conversion |
| `dcm2xml` / `xml2dcm` | Part-10 <-> Native DICOM Model XML conversion |
| `dcm2img` / `img2dcm` | Frame <-> PNG export/import |
| `dcmvalidate` | IOD conformance check (exit 0 clean, 1 errors, 2 unsupported SOP) |
| `pdf2dcm` | Wrap a PDF in an Encapsulated PDF file (with IOD self-check) |
| `dcm2pdf` | Extract the PDF from an Encapsulated PDF file |
| `dcmodify` | Modify/insert/erase tags in a Part-10 file |
| `dcm2dcm` | Transcode pixel data to another transfer syntax |
| `dcmdir` | Create or list a DICOMDIR media directory |
| `deidentify` | De-identify Part-10 files (basic profile) |
| `auditsend` | Send one ATNA audit message |

DIMSE tools need features `cli,net`; DICOMweb tools need `cli,web`;
file tools need only `cli`.

## Features

| Crate | Feature (default) | Effect |
|---|---|---|
| `vyna-dcm-net` | `tls` (on) | TLS associations via tokio-rustls; off = plain TCP only |
| `vyna-dcm-web` | `server` (on) | hyper HTTP/1.1 front end; off = client + dispatch core only |
| `vyna-dcm` | `net`, `web`, `ecg`, `cli` (all on) | Facade re-exports and CLI availability |

Every combination builds; without defaults the gated tests drop out
(net: 18 tests without `tls`; web: 10 tests without `server`).

```toml
# Minimal embed: model + codec only
vyna-dcm = { git = "https://gitlab.flostel.com/alexshao/VynaDCM", version = "0.1", default-features = false }
# VynaECG: features = ["ecg", "net"]. VynaLink: features = ["net", "web"].
```

## Scope notes

- Pixel data: native (uncompressed), RLE Lossless, Deflated, and the JPEG
  families (`.50`, `.70`, `.80`/`.81`, `.90`/`.91`, via the bundled
  `BundledCodec`) are decoded and encoded in-tree; all four uncompressed
  syntaxes (including retired Big Endian) round-trip. JPEG Extended
  12-bit, J2K Part 2, and HTJ2K remain unsupported.
- `vyna-dcm-ecg` covers resting 12-lead ECG (General ECG / 12-Lead SOP
  instances); it is a mapping helper, not an acquisition driver.
- Part-10 → JSON → Part-10 normalizes DS float formatting (`500` →
  `500.0`); values are numerically identical, as the DICOM JSON Model
  stores DS as JSON numbers.

## License

Proprietary — all rights reserved. See your Flostel license agreement.
