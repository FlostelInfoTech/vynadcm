# VynaDCM Ecosystem Comparison & Roadmap

| Field | Value |
|---|---|
| Document | Roadmap (living) |
| Version | 0.1.0 |
| Date | 2026-10-11 |
| Status | Living — reprioritized as VynaECG/VynaLink needs evolve |
| Applies to | VynaDCM workspace v0.1.0 |

Revision history: 0.1.0 (2026-10-11) — initial issue, from comparison
with dcm4che, DCMTK, pydicom (+pynetdicom), and fo-dicom.

## Contents

- [1. Method](#1-method)
- [2. Where VynaDCM leads](#2-where-vynadcm-leads)
- [3. Priority 0: interoperability foundations](#3-priority-0-interoperability-foundations)
- [4. Priority 1: interface-engine depth](#4-priority-1-interface-engine-depth)
- [5. Priority 2: deployment and robustness](#5-priority-2-deployment-and-robustness)
- [6. Deliberate non-goals](#6-deliberate-non-goals)

## 1. Method

Compared against the reference toolkits' shipped capabilities
([dcm4che modules & tools](https://github.com/dcm4che/dcm4che),
[DCMTK modules](https://github.com/DCMTK/dcmtk),
[pydicom user guide](https://pydicom.github.io/pydicom/stable/guides/user/index.html),
fo-dicom DIMSE/DICOMweb coverage). Each gap below is ranked by value
to the two consumers (VynaECG, VynaLink) and given acceptance criteria.
Items move to the SRS (`docs/requirements.md`) when scheduled.

## 2. Where VynaDCM leads

- DICOMweb client **and** server in one dependency-light library
  (DCMTK: none; fo-dicom: weak; pydicom: separate packages).
- ECG waveform IOD modeling (recording model, MDC codes, SOP
  builder) — no rival ships ECG-specific mapping.
- HTJ2K transfer syntaxes modeled; single-`ring` TLS stack shared by
  DIMSE and HTTPS; fully async networking.

## 3. Priority 0: interoperability foundations

### P0.1 Full PS3.6 dictionary + private creators — DONE

Replaced the 266-entry curated table with the complete public registry
(5,239 entries, PS3.6 2026d) plus private-creator mechanics (424
creators / 8,860 entries) and the missing `SV` VR (35 variants).
Accepted: registry/private spot tests, 96/96 green, clippy clean.

### P0.2 Bundled JPEG / JPEG-LS / JPEG 2000 backend — DONE

Shipped `BundledCodec` (`.50` via `jpeg-decoder`/`jpeg-encoder`,
`.70` via `jpegli`, `.80`/`.81` via `jpegls`, `.90`/`.91` via
vendored `openjp2` with a null-`dealloc` patch) plus `encode_pixels`
and multi-frame decode; `decode_pixels` works without an explicit
codec. Accepted: exact lossless round-trips, NEAR-bound and
lossy-error tests, independent-encoder cross-checks, truncated-input
robustness, 117/117 green, clippy clean. `.51`, J2K Part 2, and
HTJ2K stay out of scope.

### P0.3 IOD validator (`dciodvfy` / `dcmvalidate`-like)

Type 1/2/3 presence and VR/VM checking for common SOP classes, as a
library API plus a `dcmvalidate` CLI. Critical for an interface
engine that must reject bad inbound data with reasons. Acceptance:
validator flags missing Type-1 and mistyped attributes on fixtures.

### P0.4 Encapsulated PDF helpers (`pdf2dcm` / `dcm2pdf`-like)

ECG reports are standardly encapsulated PDFs. Provide SOP-instance
builders and extractors for Encapsulated PDF storage. Acceptance:
round-trip a PDF through a C-STORE-able instance.

## 4. Priority 1: interface-engine depth

- **Tag-editor CLI** (`dcmodify`-like): modify/insert/remove tags in
  files; the dataset API already supports it.
- **Transcode CLI/API** (`dcmconv`/`dcm2dcm`-like): decode + re-encode
  across transfer syntaxes (needs P0.2 for compressed targets).
- **DICOMDIR read/create** (`dcmdir`/`dcmgpdir`, pydicom filesets):
  media import for VynaLink.
- **WADO-URI client**: retired but still widely deployed on legacy
  PACS; complements the RS client.
- **Missing SCU tools** (`movescu`, `getscu`, `mppsscu`,
  `stgcmtscu`): the APIs exist; dcm4che ships all four tools.
- **DICOM Conformance Statement**: hospitals routinely require one
  for procurement; generate from implemented capabilities.

## 5. Priority 2: deployment and robustness

- **De-identification profiles** (PS3.15 E, dcm4che `deidentify`):
  research/AI exports of ECGs.
- **ATNA audit-trail emission** (dcm4che `audit`/syslog): hospital
  compliance logging.
- **Specific Character Set conversion** (DCMTK `oficonv`-like):
  correct text decoding for CJK patient names.
- **XML presentation** (`dcm2xml`/`xml2dcm`): debugging and transforms.
- **UPS / IAN**: adopt if customers demand; niche otherwise.

## 6. Deliberate non-goals

Image display/windowing, RT/SR specialty modules, print management,
HL7 (covered by the RS7 library), LDAP configuration management,
media burning beyond DICOMDIR, FIPS mode.
