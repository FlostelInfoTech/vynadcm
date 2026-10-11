# VynaDCM

From-scratch DICOM library in Rust, built for **VynaECG** (ECG management)
and **VynaLink** (interface engine) to replace their current open-source
DICOM dependencies.

No DICOM code is borrowed: the data model, codecs, DIMSE networking,
DICOMweb, and ECG mapping are all implemented in this workspace
(~15k lines). External crates are used only for generic infrastructure
(async runtime, HTTP, TLS, crypto, CLI parsing).

| Crate | Purpose |
|---|---|
| `vyna-dcm-core` | Tags, 34 VRs, `DataSet`, 266-entry PS3.6 dictionary, UIDs, 16 transfer syntaxes |
| `vyna-dcm-codec` | Dataset codec, Part-10 files, from-scratch RLE, DICOM JSON, pixel helpers |
| `vyna-dcm-net` | PDU/association, DIMSE SCU + SCP, MWL/MPPS/Storage-Commitment builders, TLS |
| `vyna-dcm-web` | DICOMweb QIDO/WADO/STOW client + server, `multipart/related` codec |
| `vyna-dcm-ecg` | 12-lead ECG model and Waveform Sequence mapping for VynaECG |
| `vyna-dcm` | Facade re-exporting the above, plus 8 CLI tools and 3 examples |

See [ARCHITECTURE.md](ARCHITECTURE.md) for the layer design and module map.

## Quick start

Requires Rust 1.98+ (edition 2024).

```sh
cargo build --workspace
cargo test --workspace        # 82 unit + 5 doc tests
cargo clippy --workspace --all-targets
cargo fmt --all -- --check
```

Loopback C-ECHO with the bundled tools:

```sh
cargo run -p vyna-dcm --bin echoscp -- --bind 127.0.0.1:11116 &
cargo run -p vyna-dcm --bin echoscu -- --connect 127.0.0.1:11116
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
| `dcmdump` | Part-10 file text dump |
| `dcm2json` / `json2dcm` | Part-10 <-> DICOM JSON conversion |

Network tools need features `cli,net`; file tools need only `cli`.

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
vyna-dcm = { version = "0.1", default-features = false }
# VynaECG: + ECG mapping. VynaLink: + net (+ web for DICOMweb endpoints).
```

## Scope notes

- Pixel data: native (uncompressed), RLE Lossless, and Deflated are decoded
  in-tree; all four uncompressed syntaxes (including retired Big Endian)
  round-trip. JPEG / JPEG 2000 families are negotiated end-to-end but pixel
  decoding goes through the `PixelCodec` plug-in trait — bring your own
  decoder.
- `vyna-dcm-ecg` covers resting 12-lead ECG (General ECG / 12-Lead SOP
  instances); it is a mapping helper, not an acquisition driver.
- Part-10 → JSON → Part-10 normalizes DS float formatting (`500` →
  `500.0`); values are numerically identical, as the DICOM JSON Model
  stores DS as JSON numbers.

## License

Proprietary — all rights reserved. See your Flostel license agreement.
