# VynaDCM DICOM Conformance Statement

Product: VynaDCM `0.2.0` (workspace crates `vyna-dcm-*`).
Generated from the implemented capabilities by
`docs/generate-conformance.py` — do not hand-edit; rerun the script.

## 1. Introduction

This statement describes the DICOM networking, media, and DICOMweb
behaviour of the VynaDCM command-line Application Entities and the
library defaults behind them, following the structure of PS3.2.
Implementation Class UID
`2.25.46417651854989365804951432447169401540`, Implementation Version Name `VYNA_DCM_020`.

## 2. Implementation model

| Application Entity | Default AE title | Role | Services |
|--------------------|------------------|------|----------|
| `echoscu` | `VYNA_SCU` | SCU | C-ECHO |
| `storescu` | `VYNA_SCU` | SCU | C-STORE |
| `findscu` | `VYNA_SCU` | SCU | C-FIND |
| `movescu` | `VYNA_SCU` | SCU | C-MOVE |
| `getscu` | `VYNA_SCU` | SCU | C-GET |
| `mppsscu` | `VYNA_SCU` | SCU | N-CREATE, N-SET |
| `stgcmtscu` | `VYNA_SCU` | SCU | N-ACTION + N-EVENT-REPORT (commitment) |
| `echoscp` | `VYNA_SCP` | SCP | C-ECHO (all other services use rejecting defaults) |
| `storescp` | `VYNA_SCP` | SCP | C-STORE |
| `findscp` | `VYNA_SCP` | SCP | C-FIND, C-MOVE, C-GET |
| `mwlscp` | `VYNA_SCP` | SCP | C-FIND |

Application data flow is tool-to-peer over one TCP association per
invocation: SCU tools connect, negotiate, perform their service, and
release; SCP tools listen, serve each inbound association to
release/abort, and loop.

## 3. AE specifications

### 3.1 SCU tools

#### 3.1.1 `echoscu` — Send C-ECHO and print the status

- Services: C-ECHO.
- SOP Classes proposed (SCU role):
  - Verification (C-ECHO) (`1.2.840.10008.1.1`).
- Transfer syntaxes proposed: Explicit VR Little Endian and
  Implicit VR Little Endian.

#### 3.1.2 `storescu` — C-STORE Part-10 files to a peer

- Services: C-STORE.
- SOP Classes: taken from the input files' SOP Class UIDs
  (any storage SOP Class the peer accepts).
- Transfer syntaxes proposed: Explicit VR Little Endian and
  Implicit VR Little Endian.
- Note: also proposes each input file's own transfer syntax.

#### 3.1.3 `findscu` — C-FIND query and print matches

- Services: C-FIND.
- SOP Classes proposed (SCU role):
  - Study Root Q/R — FIND (`1.2.840.10008.5.1.4.1.2.2.1`).
  - Patient Root Q/R — FIND (`1.2.840.10008.5.1.4.1.2.1.1`).
  - Patient/Study Only Q/R — FIND (`1.2.840.10008.5.1.4.1.2.3.1`).
  - Modality Worklist — FIND (`1.2.840.10008.5.1.4.31`).
- Transfer syntaxes proposed: Explicit VR Little Endian and
  Implicit VR Little Endian.

#### 3.1.4 `movescu` — C-MOVE retrieve: the peer sends matches to --dest (run storescp there)

- Services: C-MOVE.
- SOP Classes proposed (SCU role):
  - Study Root Q/R — MOVE (`1.2.840.10008.5.1.4.1.2.2.2`).
  - Patient Root Q/R — MOVE (`1.2.840.10008.5.1.4.1.2.1.2`).
  - Patient/Study Only Q/R — MOVE (`1.2.840.10008.5.1.4.1.2.3.2`).
- Transfer syntaxes proposed: Explicit VR Little Endian and
  Implicit VR Little Endian.

#### 3.1.5 `getscu` — C-GET retrieve into a directory (instances arrive on this association)

- Services: C-GET.
- SOP Classes proposed (SCU role):
  - Study Root Q/R — GET (`1.2.840.10008.5.1.4.1.2.2.3`).
  - Patient Root Q/R — GET (`1.2.840.10008.5.1.4.1.2.1.3`).
  - Computed Radiography Image Storage (`1.2.840.10008.5.1.4.1.1.1`).
  - Digital X-Ray Image Storage — For Presentation (`1.2.840.10008.5.1.4.1.1.1.1`).
  - Digital X-Ray Image Storage — For Processing (`1.2.840.10008.5.1.4.1.1.1.1.1`).
  - Digital Mammography X-Ray Image Storage — For Presentation (`1.2.840.10008.5.1.4.1.1.1.2`).
  - Digital Mammography X-Ray Image Storage — For Processing (`1.2.840.10008.5.1.4.1.1.1.2.1`).
  - CT Image Storage (`1.2.840.10008.5.1.4.1.1.2`).
  - Enhanced CT Image Storage (`1.2.840.10008.5.1.4.1.1.2.1`).
  - Ultrasound Multi-frame Image Storage (`1.2.840.10008.5.1.4.1.1.3.1`).
  - MR Image Storage (`1.2.840.10008.5.1.4.1.1.4`).
  - Enhanced MR Image Storage (`1.2.840.10008.5.1.4.1.1.4.1`).
  - Ultrasound Image Storage (single-frame) (`1.2.840.10008.5.1.4.1.1.6.1`).
  - Secondary Capture Image Storage (`1.2.840.10008.5.1.4.1.1.7`).
  - 12-Lead ECG Waveform Storage (`1.2.840.10008.5.1.4.1.1.9.1.1`).
  - General ECG Waveform Storage (`1.2.840.10008.5.1.4.1.1.9.1.2`).
  - Ambulatory ECG Waveform Storage (`1.2.840.10008.5.1.4.1.1.9.1.3`).
  - Hemodynamic Waveform Storage (`1.2.840.10008.5.1.4.1.1.9.2.1`).
  - X-Ray Angiographic Image Storage (`1.2.840.10008.5.1.4.1.1.12.1`).
  - Nuclear Medicine Image Storage (`1.2.840.10008.5.1.4.1.1.20`).
  - VL Photographic Image Storage (`1.2.840.10008.5.1.4.1.1.77.1.4`).
  - Basic Text SR Storage (`1.2.840.10008.5.1.4.1.1.88.11`).
  - Enhanced SR Storage (`1.2.840.10008.5.1.4.1.1.88.22`).
  - Comprehensive SR Storage (`1.2.840.10008.5.1.4.1.1.88.33`).
  - Key Object Selection Document Storage (`1.2.840.10008.5.1.4.1.1.88.59`).
  - Encapsulated PDF Storage (`1.2.840.10008.5.1.4.1.1.104.1`).
  - Encapsulated CDA Storage (`1.2.840.10008.5.1.4.1.1.104.2`).
  - Positron Emission Tomography Image Storage (`1.2.840.10008.5.1.4.1.1.128`).
- Transfer syntaxes proposed: Explicit VR Little Endian and
  Implicit VR Little Endian.

#### 3.1.6 `mppsscu` — Send an MPPS N-CREATE followed by N-SET

- Services: N-CREATE, N-SET.
- SOP Classes proposed (SCU role):
  - Modality Performed Procedure Step (`1.2.840.10008.3.1.2.3.3`).
- Transfer syntaxes proposed: Explicit VR Little Endian and
  Implicit VR Little Endian.

#### 3.1.7 `stgcmtscu` — Request storage commitment, then wait for the N-EVENT-REPORT (--timeout-secs bounds the wait)

- Services: N-ACTION + N-EVENT-REPORT (commitment).
- SOP Classes proposed (SCU role):
  - Storage Commitment Push Model (`1.2.840.10008.1.20.1`).
- Transfer syntaxes proposed: Explicit VR Little Endian and
  Implicit VR Little Endian.

### 3.2 SCP tools

#### 3.2.1 `echoscp` — Answer C-ECHO requests until stopped

- Services handled: C-ECHO (all other services use rejecting defaults).

#### 3.2.2 `storescp` — Receive C-STORE into a directory

- Services handled: C-STORE.

#### 3.2.3 `findscp` — Serve C-FIND/C-MOVE/C-GET from a directory

- Services handled: C-FIND, C-MOVE, C-GET.

#### 3.2.4 `mwlscp` — Serve MWL C-FIND from a directory

- Services handled: C-FIND.

Acceptance policy (library defaults, shared by all SCP tools): any
proposed SOP Class is accepted (`accepted_sop_classes: None`); all
16 transfer syntaxes of Annex B are accepted; Called-AE
is not enforced unless `--strict-ae` is passed; no User Identity
authentication unless the embedding application installs the
`user_identity_auth` callback.

### 3.3 Association policies

- Application Context Name: `1.2.840.10008.3.1.1.1`.
- Implementation Class UID `2.25.46417651854989365804951432447169401540`, Version Name `VYNA_DCM_020`.
- Presentation contexts: the SCU proposes one context per SOP Class,
  each with the offered transfer syntaxes; the SCP accepts at most
  one syntax per context from its accepted list.
- Maximum PDU: tools offer 256 KiB (`max_pdu`); the largest
  accepted incoming PDU is 64 MiB (`max_incoming_pdu`).
- Timeouts: SCU tools default to a 30 s per-read timeout, SCP tools
  to 300 s (`--timeout-secs`).
- The SCU proposes no Role Selection and no User Identity; the
  SCP accepts (echoes) any roles the peer proposes. No other
  extended negotiation (asynchronous operations, SOP Class
  extended/common negotiation) is offered by either side.
- SCU tools request release after their service; SCP tools answer
  release and abort cleanly. Protocol violations abort the
  association (A-ABORT source 2).

## 4. Communication profiles

### 4.1 TCP/IP and DICOM TLS

- DIMSE runs over TCP/IP. DICOM TLS (PS3.15) is compiled in by
  default (`net` feature `tls`, tokio-rustls/ring backend).
- SCU tools: `--tls` with optional `--tls-ca` (private CA),
  `--tls-cert`/`--tls-key` (mutual TLS client identity),
  `--tls-server-name` (defaults to the peer host).
- SCP tools: TLS is enabled by `--tls-cert`/`--tls-key`, with
  optional `--tls-client-ca` for mutual TLS.

### 4.2 DICOMweb (informative; outside PS3.2)

- Client: QIDO-RS search (`qido`, `search_studies/series/instances`),
  WADO-RS retrieve (`wado_parts/file/dataset/metadata`), legacy
  WADO-URI (`wado_uri`, `wado_uri_dataset`), STOW-RS store (`stow`).
- Server (default `server` feature): framework-agnostic dispatch
  over an in-memory store, with a hyper front end.
- CLIs: `qidors` (search), `wadors` (retrieve), `stowrs` (store),
  `wadorsd` (stub server over a directory).

### 4.3 Media (DICOMDIR)

- `read_dicomdir` (File Set Reader) and `build_dicomdir` (File Set
  Creator) plus the `dcmdir` CLI; directory records are encoded
  Explicit VR Little Endian. No physical-media handling: file sets
  live in the filesystem.

## 5. Character sets

- Text values (PN/LO/SH/ST/LT/UT) decode per the top-level
  Specific Character Set, inherited by nested sequences:
  `ISO_IR 6/100/101/109/110/126/127/138/144/148/166/192`,
  `ISO_IR 13`, Japanese `ISO 2022 IR 13/87/159` pairs, Korean
  `ISO 2022 IR 149`, `GBK`/`GB18030`; UC/UR stay UTF-8, and
  undeclared/unknown sets fall back to Latin-1.
- Encoding mirrors decoding (one code-extension run per value,
  `?` for unmappable characters); creation sets `ISO_IR 192`
  only where the builder knows the text needs it (Encapsulated
  PDF instances with non-ASCII text).

## 6. Security

- Association security relies on DICOM TLS (see §4.1); the library
  defaults to WebPKI roots with optional private CA.
- Not implemented: the ATNA ARR (server) side, full E.1
  transcription beyond the basic de-identification profile,
  and User Identity on the SCU side.

## 7. Configuration

| Item | Default | Flag |
|------|---------|------|
| SCU calling AE | `VYNA_SCU` | `--calling` |
| SCU called AE | `VYNA_SCP` | `--called` |
| SCP AE title | `VYNA_SCP` | `--ae-title` |
| SCP bind | `127.0.0.1:11112` | `--bind` |
| Max PDU | 256 KiB | `--max-pdu` |
| SCU read timeout | 30 s | `--timeout-secs` |
| SCP read timeout | 300 s | `--timeout-secs` |

## Annex A — SOP Classes

| SOP Class | UID | Category |
|-----------|-----|----------|
| Verification (C-ECHO) | `1.2.840.10008.1.1` | Verification |
| Media Storage Directory Storage (DICOMDIR) | `1.2.840.10008.1.3.10` | Media (DICOMDIR) |
| Storage Commitment Push Model | `1.2.840.10008.1.20.1` | N-service (commitment) |
| Storage Commitment Pull Model | `1.2.840.10008.1.20.2` | N-service (commitment) |
| Modality Performed Procedure Step | `1.2.840.10008.3.1.2.3.3` | N-service (MPPS) |
| Computed Radiography Image Storage | `1.2.840.10008.5.1.4.1.1.1` | Storage |
| Digital X-Ray Image Storage — For Presentation | `1.2.840.10008.5.1.4.1.1.1.1` | Storage |
| Digital X-Ray Image Storage — For Processing | `1.2.840.10008.5.1.4.1.1.1.1.1` | Storage |
| Digital Mammography X-Ray Image Storage — For Presentation | `1.2.840.10008.5.1.4.1.1.1.2` | Storage |
| Digital Mammography X-Ray Image Storage — For Processing | `1.2.840.10008.5.1.4.1.1.1.2.1` | Storage |
| CT Image Storage | `1.2.840.10008.5.1.4.1.1.2` | Storage |
| Enhanced CT Image Storage | `1.2.840.10008.5.1.4.1.1.2.1` | Storage |
| Ultrasound Multi-frame Image Storage | `1.2.840.10008.5.1.4.1.1.3.1` | Storage |
| MR Image Storage | `1.2.840.10008.5.1.4.1.1.4` | Storage |
| Enhanced MR Image Storage | `1.2.840.10008.5.1.4.1.1.4.1` | Storage |
| Ultrasound Image Storage (single-frame) | `1.2.840.10008.5.1.4.1.1.6.1` | Storage |
| Secondary Capture Image Storage | `1.2.840.10008.5.1.4.1.1.7` | Storage |
| 12-Lead ECG Waveform Storage | `1.2.840.10008.5.1.4.1.1.9.1.1` | Storage |
| General ECG Waveform Storage | `1.2.840.10008.5.1.4.1.1.9.1.2` | Storage |
| Ambulatory ECG Waveform Storage | `1.2.840.10008.5.1.4.1.1.9.1.3` | Storage |
| Hemodynamic Waveform Storage | `1.2.840.10008.5.1.4.1.1.9.2.1` | Storage |
| X-Ray Angiographic Image Storage | `1.2.840.10008.5.1.4.1.1.12.1` | Storage |
| Nuclear Medicine Image Storage | `1.2.840.10008.5.1.4.1.1.20` | Storage |
| VL Photographic Image Storage | `1.2.840.10008.5.1.4.1.1.77.1.4` | Storage |
| Basic Text SR Storage | `1.2.840.10008.5.1.4.1.1.88.11` | Storage |
| Enhanced SR Storage | `1.2.840.10008.5.1.4.1.1.88.22` | Storage |
| Comprehensive SR Storage | `1.2.840.10008.5.1.4.1.1.88.33` | Storage |
| Key Object Selection Document Storage | `1.2.840.10008.5.1.4.1.1.88.59` | Storage |
| Encapsulated PDF Storage | `1.2.840.10008.5.1.4.1.1.104.1` | Storage |
| Encapsulated CDA Storage | `1.2.840.10008.5.1.4.1.1.104.2` | Storage |
| Positron Emission Tomography Image Storage | `1.2.840.10008.5.1.4.1.1.128` | Storage |
| RT Dose Storage | `1.2.840.10008.5.1.4.1.1.481.2` | Storage (defined; not in default proposals) |
| RT Structure Set Storage | `1.2.840.10008.5.1.4.1.1.481.3` | Storage (defined; not in default proposals) |
| RT Plan Storage | `1.2.840.10008.5.1.4.1.1.481.5` | Storage (defined; not in default proposals) |
| Patient Root Q/R — FIND | `1.2.840.10008.5.1.4.1.2.1.1` | Query/Retrieve |
| Patient Root Q/R — MOVE | `1.2.840.10008.5.1.4.1.2.1.2` | Query/Retrieve |
| Patient Root Q/R — GET | `1.2.840.10008.5.1.4.1.2.1.3` | Query/Retrieve |
| Study Root Q/R — FIND | `1.2.840.10008.5.1.4.1.2.2.1` | Query/Retrieve |
| Study Root Q/R — MOVE | `1.2.840.10008.5.1.4.1.2.2.2` | Query/Retrieve |
| Study Root Q/R — GET | `1.2.840.10008.5.1.4.1.2.2.3` | Query/Retrieve |
| Patient/Study Only Q/R — FIND | `1.2.840.10008.5.1.4.1.2.3.1` | Query/Retrieve |
| Patient/Study Only Q/R — MOVE | `1.2.840.10008.5.1.4.1.2.3.2` | Query/Retrieve |
| Modality Worklist — FIND | `1.2.840.10008.5.1.4.31` | Query/Retrieve |

## Annex B — Transfer Syntaxes

| Transfer Syntax | UID | Lossy |
|---|---|---|
| Implicit VR Little Endian | `1.2.840.10008.1.2` | no |
| Explicit VR Little Endian | `1.2.840.10008.1.2.1` | no |
| Deflated Explicit VR Little Endian | `1.2.840.10008.1.2.1.99` | no |
| Explicit VR Big Endian (retired) | `1.2.840.10008.1.2.2` | no |
| JPEG Baseline (8-bit) | `1.2.840.10008.1.2.4.50` | yes |
| JPEG Extended (12-bit) | `1.2.840.10008.1.2.4.51` | yes |
| JPEG Lossless | `1.2.840.10008.1.2.4.70` | no |
| JPEG-LS Lossless | `1.2.840.10008.1.2.4.80` | no |
| JPEG-LS Near-lossless | `1.2.840.10008.1.2.4.81` | yes |
| JPEG 2000 Lossless | `1.2.840.10008.1.2.4.90` | no |
| JPEG 2000 | `1.2.840.10008.1.2.4.91` | yes |
| JPEG 2000 Part 2 Lossless | `1.2.840.10008.1.2.4.92` | no |
| JPEG 2000 Part 2 | `1.2.840.10008.1.2.4.93` | yes |
| HT JPEG 2000 Lossless | `1.2.840.10008.1.2.4.201` | no |
| HT JPEG 2000 | `1.2.840.10008.1.2.4.202` | yes |
| RLE Lossless | `1.2.840.10008.1.2.5` | no |

## Annex C — Implementation identifying attributes

- Implementation Class UID: `2.25.46417651854989365804951432447169401540`.
- Implementation Version Name: `VYNA_DCM_020`.
