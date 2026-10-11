//! `vyna-dcm`: from-scratch DICOM for VynaECG and VynaLink.
//!
//! Facade over the workspace crates (each usable standalone):
//!
//! - [`core`] — tags, VRs, datasets, dictionary, UIDs
//! - [`codec`] — dataset codec, Part-10 files, RLE, DICOM JSON
//! - [`net`] — DIMSE, associations, SCU/SCP services, TLS (feature `net`)
//! - [`web`] — DICOMweb QIDO/WADO/STOW client + server (feature `web`)
//! - [`ecg`] — ECG waveform model for VynaECG (feature `ecg`)
//!
//! Common types are re-exported at the root for convenience.

#![warn(missing_docs)]

pub use vyna_dcm_codec as codec;
pub use vyna_dcm_core as core;
#[cfg(feature = "ecg")]
pub use vyna_dcm_ecg as ecg;
#[cfg(feature = "net")]
pub use vyna_dcm_net as net;
#[cfg(feature = "web")]
pub use vyna_dcm_web as web;

/// Crate version.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

pub use vyna_dcm_codec::{
    FileMeta, dataset_from_json, dataset_to_json, decode_dataset, decode_pixels, dump_dataset,
    encode_dataset, read_part10, write_part10,
};
pub use vyna_dcm_core::{
    DataElement, DataSet, PersonName, Tag, TransferSyntax, Value, Vr, lookup_keyword, new_uid, tags,
};
