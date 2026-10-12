# Vendored DICOM regression corpus

Real third-party-encoded files (not fixtures we wrote ourselves), so the
test suite proves interop with DCMTK/GDCM writers — the same corpus family
`dicom-rs` tests against via `dicom-test-files`.

Source: <https://github.com/pydicom/pydicom-data> (`data_store/data/`,
MIT licensed, Copyright (c) 2020 Dicom in Python), fetched 2026-10-12:

| File | Bytes | SHA-256 | Notes |
|---|---|---|---|
| `emri_small.dcm` | 84256 | `151233ec…bc9a56` | MR, Explicit LE, DCMTK writer |
| `emri_small_RLE.dcm` | 49022 | `93c19bca…ee45266` | same pixels, RLE |
| `emri_small_jpeg_ls_lossless.dcm` | 42834 | `24de03c9…9ec9f87b` | same pixels, JPEG-LS, GDCM writer |
| `emri_small_jpeg_2k_lossless.dcm` | 40324 | `b2b40633…a30da9dc` | same pixels, JPEG 2000, GDCM writer |
| `SC_rgb.dcm` | 31300 | `b0f868d6…321138` | SC RGB 8-bit, Explicit LE |

All four `emri_small*` files share SOP Instance UID
`1.2.826.0.1.3680043.2.1143.6455556726214900995651753669640998622`;
the pixel decoders must agree byte-exactly on them
(`corpus.rs::lossless_family_agrees`).

Full hashes: `sha256sum *.dcm`. Re-fetching is not part of any gate;
these bytes are the fixture.
