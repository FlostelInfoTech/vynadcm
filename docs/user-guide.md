# VynaDCM User Guide

| Field | Value |
|---|---|
| Document | User guide (living) |
| Version | 0.1.0 |
| Date | 2026-10-11 |
| Status | Living draft — updated with every release |
| Applies to | VynaDCM workspace v0.1.0, `vyna-dcm` CLI tools |

Revision history: 0.1.0 (2026-10-11) — initial issue. Flag
reference below is transcribed from `--help` output.

Audience: operators and support engineers running connectivity checks,
storage, queries, and file conversions. Developers embedding the
library should read `docs/integration.md`.

## Contents

- [1. Installation](#1-installation)
- [2. Common connection options](#2-common-connection-options)
- [3. Connectivity: echoscu / echoscp](#3-connectivity-echoscu--echoscp)
- [4. Storage: storescu / storescp](#4-storage-storescu--storescp)
- [5. Query: findscu](#5-query-findscu)
- [6. Files: dcmdump, dcm2json, json2dcm, dcmvalidate, pdf2dcm, dcm2pdf, dcmodify](#6-files-dcmdump-dcm2json-json2dcm-dcmvalidate-pdf2dcm-dcm2pdf-dcmodify)
- [7. TLS walkthrough](#7-tls-walkthrough)
- [8. Worked examples](#8-worked-examples)
- [9. Troubleshooting](#9-troubleshooting)
- [10. ATNA audit trail](#10-atna-audit-trail)

## 1. Installation

```sh
git clone https://gitlab.flostel.com/alexshao/VynaDCM
cd VynaDCM
cargo build --release -p vyna-dcm --bins
# binaries land in target/release/
```

Requires Rust 1.98+. The eight tools are also runnable in place with
`cargo run -p vyna-dcm --bin <tool> -- <args>`.

## 2. Common connection options

`echoscu`, `storescu`, `findscu`, `movescu`, `getscu`, `mppsscu`,
and `stgcmtscu` share these flags:

| Flag | Default | Meaning |
|---|---|---|
| `--connect <host:port>` | (required) | Peer address |
| `--called <AE>` | `VYNA_SCP` | Called (destination) AE title |
| `--calling <AE>` | `VYNA_SCU` | Calling (our) AE title |
| `--max-pdu <bytes>` | `262144` | Our max receive PDU length |
| `--timeout-secs <s>` | `30` | Per-read timeout |
| `--tls` | off | Use DICOM TLS (PS3.15) |
| `--tls-ca <pem>` | — | Extra CA for server verification (private PKI) |
| `--tls-cert/--tls-key <pem>` | — | Client certificate + key (mutual TLS) |
| `--tls-server-name <name>` | connect host | TLS server name indication |

SCP tools (`echoscp`, `storescp`) share the listener flags:
`--bind` (default `127.0.0.1:11112`), `--ae-title` (default
`VYNA_SCP`), `--max-pdu` (default `262144`), `--timeout-secs`
(default `300`), `--strict-ae` (reject mismatched Called-AE), and
`--tls-cert` / `--tls-key` / `--tls-client-ca` (TLS is enabled when
cert+key are present; client CA switches on mutual TLS).
`storescp` adds `--directory` (default `.`) for received files.

Exit codes: `0` on success; nonzero with an error message on
connection, negotiation, or DIMSE failures.

## 3. Connectivity: echoscu / echoscp

```sh
# Terminal 1: start a responder
echoscp --bind 127.0.0.1:11116

# Terminal 2: verify (prints C-ECHO-RSP status, expect 0x0000)
echoscu --connect 127.0.0.1:11116
```

Use `echoscu` against any PACS to check reachability, AE titles, and
negotiation before storing or querying.

## 4. Storage: storescu / storescp

```sh
# Terminal 1: receive into a directory
storescp --bind 127.0.0.1:11113 --directory /data/incoming

# Terminal 2: send one or more Part-10 files
storescu --connect 127.0.0.1:11113 study1.dcm study2.dcm
```

- `storescp` writes each instance as `<SOPInstanceUID>.dcm`
  (re-encoded with the negotiated transfer syntax) and prints the
  patient name per file.
- `storescu` accepts `--priority <0|1|2>` (0 = medium, 1 = high,
  2 = low; default 0).
- Statuses: `0x0000` stored; `0xA700` out of resources (disk write
  failed); `0xA900` encode failure.
- Both take `--audit-server` for ATNA emission (see §10).

## 5. Query: findscu

```sh
# Study Root query with matching keys and JSON output
findscu --connect 127.0.0.1:11112 \
  -k PatientID=E123 -k Modality=ECG -k StudyDate=20260101-20261231 \
  --json

# Modality Worklist for a station
findscu --connect 127.0.0.1:11112 --model mwl \
  -k ScheduledStationAETitle=ECG-1
```

- `--model`: `studyroot` (default), `patientroot`, `patientstudy`,
  or `mwl`.
- `-k/--key KEY=value` (repeatable): keyword (`PatientID`) or
  8-hex-digit tag (`00100020`); empty value (`-k PatientName=`)
  requests a return key.
- `--json` prints matches as DICOM JSON; otherwise a text dump.

### Retrieve: movescu / getscu

```sh
# C-MOVE: the peer sends matches to the MOVEDEST AE (run storescp there)
movescu --connect 127.0.0.1:11112 --dest MOVEDEST -k StudyInstanceUID=1.2.3

# C-GET: instances arrive back on this same association
getscu --connect 127.0.0.1:11112 --output-dir ./got -k StudyInstanceUID=1.2.3
```

- `--model`: `studyroot` (default), `patientroot`; `movescu` also
  accepts `patientstudy` (C-GET has no Patient/Study-only form).
- `-k/--key` works exactly as in `findscu`, including the derived
  Query/Retrieve Level.
- The destination AE must be known to the *peer*, not to `movescu`.

### MPPS and Storage Commitment: mppsscu / stgcmtscu

```sh
# MPPS: N-CREATE (IN PROGRESS) then N-SET (COMPLETED)
mppsscu --connect 127.0.0.1:11112 --pps-id PPS1 --modality ECG \
  --study-uid 1.2.3 --series 1.2.3.4:1.2.840.10008.5.1.4.1.1.2:1.2.3.101

# Storage Commitment: N-ACTION then wait for the N-EVENT-REPORT
stgcmtscu --connect 127.0.0.1:11112 --file received.dcm --timeout-secs 60
```

- `mppsscu` generates the MPPS UID and UTC start/end dates unless
  `--mpps-uid` / `--start-date` / `--end-date` (+ `--start-time` /
  `--end-time`) are given; `--discontinue` ends DISCONTINUED.
- `stgcmtscu` takes `--file` (SOP UIDs read from Part-10 meta) and/or
  `--instance CLASS_UID:INSTANCE_UID`; `--timeout-secs` bounds the
  report wait, and any failed instance exits nonzero.

## 6. Files: dcmdump, dcm2json, json2dcm, dcmvalidate, pdf2dcm, dcm2pdf, dcmodify

```sh
dcmdump file.dcm                        # text dump to stdout
dcm2json file.dcm > file.json           # Part-10 → DICOM JSON
dcm2json --with-meta file.dcm > full.json  # include group 0002 meta
json2dcm --output roundtrip.dcm file.json  # JSON → Part-10 (explicit LE)
json2dcm --output implicit.dcm --transfer-syntax 1.2.840.10008.1.2 file.json
dcmvalidate file.dcm                    # IOD conformance check
dcmvalidate --sop-class 1.2.840.10008.5.1.4.1.1.2 file.dcm  # override SOP Class
pdf2dcm report.pdf report.dcm --patient-name "Doe^Jane" --patient-id P123
dcm2pdf report.dcm report.out.pdf       # extract (byte-exact)
```

`dcmvalidate` checks 18 SOP Classes (CR/CT/MR/MG/DX/US/SC/PT/NM/XA/
VL/KO/PDF/ECG) for Type 1/2 presence, VR/VM conformance, and
conditional rules (multi-frame, color, lossy syntax, charset); it
also verifies file-meta SOP UIDs match the dataset. Exit 0 means
no errors (warnings allowed), 1 means errors, 2 means the SOP
Class is not supported.

`pdf2dcm` wraps a PDF (e.g. an ECG report) in an Encapsulated PDF
instance: Study/Series/SOP UIDs are generated unless `--study-uid` /
`--series-uid` / `--sop-uid` are given, `--title` defaults to the
input file name, and `--transfer-syntax explicit|implicit|deflated`
(default `explicit`) picks the dataset encoding. The instance is
validated against its IOD before writing, so a zero exit means the
file is C-STORE-able; `dcm2pdf` extracts the bytes back exactly.

`dcmodify` edits tags in place (or to `--output`): `-m TAG=VALUE`
changes an existing element keeping its VR, `-i TAG[:VR]=VALUE`
adds one (`:VR` needed only when the tag has no dictionary entry),
`-e TAG` removes one. Tags accept `(0010,0010)`, `0010,0010`,
`00100010`, or keywords; `\` separates multi-values, empty values
blank the element, and `@path` feeds file bytes to binary VRs:

```sh
dcmodify file.dcm -o anon.dcm -m PatientName="Smith^John" -m PatientID= -e AccessionNumber
dcmodify file.dcm -i SeriesDescription=Edited -i "(0009,1001):LO=private"
dcmvalidate anon.dcm   # check the edited file still conforms
```

Edits apply as erases, then modifies, then inserts; file-meta SOP
UIDs follow the dataset (erasing them refuses to write), and
`--transfer-syntax UID` overrides the preserved encoding.
Sequences can be erased but not set from the CLI.

`dcm2dcm` transcodes pixel data between syntaxes (names or UIDs):

```sh
dcm2dcm ct.dcm ct.rle.dcm --transfer-syntax rle
dcm2dcm sc.dcm sc.jpg.dcm --transfer-syntax jpeg-baseline --quality 90
dcm2dcm us.dcm us.j2k.dcm --transfer-syntax jpeg2000 --lossy-ratio 8
```

Lossy targets (`.50`/`.81`/`.91`) get a fresh SOP Instance UID and
lossy history tags; lossless targets keep the UID. Quality knobs
(`--quality`, `--near`, `--lossy-ratio`) only pair with their own
target and are rejected otherwise. Palette images and `.51`/J2K
Part 2/HTJ2K are not supported.

`dcmdir` builds a media index from files or directories (scanned
recursively; unreadable finds are skipped with a warning while
explicit files must parse), and `--list` prints one back:

```sh
dcmdir --output /media/USB/DICOMDIR --file-set-id CARDIO /media/USB
dcmdir --list /media/USB/DICOMDIR
```

Every indexed file must sit under the output directory so its File
ID stays relative; record types map from SOP Class (`IMAGE`, `RT
DOSE`, `SR DOCUMENT`, …). Skipped records print with their reason
rather than failing the import.

`deidentify` applies the basic profile for research exports (names
and IDs dummied, dates shifted or dummied, UIDs remapped
consistently, private groups and overlays removed; times, clinical
content, and pixels kept):

```sh
deidentify study/*.dcm --output-dir anon/ --date-offset 30
deidentify one.dcm --output one.anon.dcm --patient-id SUBJ-7
deidentify old.dcm --in-place   # rewrite via a temp file
```

- `--date-offset DAYS` keeps longitudinal intervals across the whole
  batch (one UID map per run); without it, dates become `19000101`.
- `--patient-id` / `--patient-name` override the `ANONYMIZED`
  dummies; `--retain-private-tags` keeps private groups.
- Files with Burned In Annotation `YES` are refused (pixels are
  never altered); file-meta UIDs follow the remapped dataset.

Note: JSON round-trips normalize `DS` float formatting (`500` →
`500.0`); values stay numerically identical.

## 7. TLS walkthrough

SCU against a TLS PACS with a private CA:

```sh
echoscu --connect pacs.example:11112 --tls --tls-ca hospital-ca.pem
```

Mutual TLS (both sides authenticate):

```sh
# SCP requires client certificates
storescp --bind 0.0.0.0:11112 --tls-cert scp-chain.pem \
  --tls-key scp-key.pem --tls-client-ca hospital-ca.pem --directory /data/incoming

# SCU presents its identity
storescu --connect 10.0.0.5:11112 --tls --tls-ca hospital-ca.pem \
  --tls-cert scu-cert.pem --tls-key scu-key.pem file.dcm
```

## 8. Worked examples

Three runnable scenarios ship with the workspace:

```sh
cargo run -p vyna-dcm --example ecg_write   # 12-lead ECG → Part-10 → verify
cargo run -p vyna-dcm --example wado_fetch  # DICOMweb server + client round-trip
cargo run -p vyna-dcm --example link_route -- 127.0.0.1:11115 127.0.0.1:11114 PACS
```

`link_route` demonstrates the VynaLink pattern: an SCP that forwards
every received instance to a downstream PACS.

## 9. Troubleshooting

| Symptom | Likely cause | Action |
|---|---|---|
| Association rejected (result 1) | AE title / application context | Check `--called`/`--calling`; SCP `--strict-ae` |
| `NoPresentationContext` | SOP class or transfer syntax missing from the agreed set | Bundled SCPs accept all by default — the peer likely didn't propose it; integrators tune `AcceptorOptions` |
| C-MOVE fails Move Destination Unknown | No route for destination AE | Configure the move directory (integrators: `ScpMoveConfig`) |
| TLS handshake failure | CA / hostname mismatch | `--tls-ca`, `--tls-server-name`; check PEM validity |
| `json2dcm` missing UID errors | JSON lacks SOP Class/Instance UIDs | Source dataset must carry both UIDs |
| Slow/blocked reads | Peer idle, tight timeout | Raise `--timeout-secs` (SCP default 300 s) |

## 10. ATNA audit trail

`storescp` (actor-start + per-store), `storescu` (per-store), and
`findscu` (per-query) emit DICOM audit messages when given an audit
server; outcomes follow the DIMSE status, and emission failures warn
without breaking the transfer:

```sh
storescp --bind 127.0.0.1:11112 --directory /data/incoming \
  --audit-server udp://arr.example:514
storescu --connect 127.0.0.1:11112 \
  --audit-server tls://arr.example --audit-ca hospital-ca.pem file.dcm
```

`auditsend` emits any single event (useful for ARR connectivity
checks and scripts); `--dry-run` prints the XML instead:

```sh
auditsend --server udp://127.0.0.1:514 transferred --study 1.2.3 \
  --source-ae SRC --dest-ae DST --sop 1.2.840.10008.5.1.4.1.1.2:1.2.3.1
auditsend --server tls://arr.example --ca hospital-ca.pem --dry-run \
  query --source-ae QRY --dest-ae PACS --patient E123
```

- `--audit-source` overrides the Audit Source ID (default: our AE);
  TLS verifies hostnames against WebPKI plus `--audit-ca`.
- Events: `start`, `stop`, `transferred`, `begin-transfer`,
  `query`, `deleted`, `export`, `import`, `custom`;
  `--outcome success|minor|serious|major` (default success).
