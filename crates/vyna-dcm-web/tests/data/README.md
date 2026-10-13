# Test fixtures: real third-party-encoded DICOM files

Seeds the loopback DICOMweb store. Files live next to the tests so
they travel with the published crate.

Source: <https://github.com/pydicom/pydicom-data> (`data_store/data/`,
MIT licensed, Copyright (c) 2020 Dicom in Python):

| File | Notes |
|---|---|
| `emri_small.dcm` | MR, Explicit LE, DCMTK writer |
| `SC_rgb.dcm` | SC RGB 8-bit, Explicit LE |
