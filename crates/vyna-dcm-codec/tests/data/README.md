# Test fixtures: real third-party-encoded DICOM files

Proves interop with DCMTK/GDCM writers (not fixtures we wrote
ourselves). Files live next to the tests so they travel with the
published crate.

Source: <https://github.com/pydicom/pydicom-data> (`data_store/data/`,
MIT licensed, Copyright (c) 2020 Dicom in Python):

| File | Notes |
|---|---|
| `emri_small.dcm` | MR, Explicit LE, DCMTK writer |
| `emri_small_RLE.dcm` | same pixels, RLE |
| `emri_small_jpeg_ls_lossless.dcm` | same pixels, JPEG-LS, GDCM writer |
| `emri_small_jpeg_2k_lossless.dcm` | same pixels, JPEG 2000, GDCM writer |
| `SC_rgb.dcm` | SC RGB 8-bit, Explicit LE |
