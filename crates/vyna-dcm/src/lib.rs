//! `vyna-dcm`: from-scratch DICOM for VynaECG and VynaLink.
//!
//! Facade over the workspace crates (each usable standalone):
//!
//! - [`core`] — tags, VRs, datasets, dictionary, UIDs
//! - [`codec`] — dataset codec, Part-10 files, RLE, DICOM JSON
//! - [`net`] — DIMSE, associations, SCU/SCP services, TLS (feature `net`)
//! - [`web`] — DICOMweb QIDO/WADO/STOW client + server (feature `web`)
//! - [`ecg`] — ECG waveform model for VynaECG (feature `ecg`)
//! - [`iod`] — IOD conformance validation
//! - [`pdf`] — Encapsulated PDF builders and extractors
//! - [`deid`] — de-identification profiles for research exports
//! - [`atna`] — ATNA audit-trail emission over syslog
//!
//! Common types are re-exported at the root for convenience.

#![warn(missing_docs)]

pub use vyna_dcm_atna as atna;
pub use vyna_dcm_codec as codec;
pub use vyna_dcm_core as core;
pub use vyna_dcm_deid as deid;
#[cfg(feature = "ecg")]
pub use vyna_dcm_ecg as ecg;
pub use vyna_dcm_iod as iod;
#[cfg(feature = "net")]
pub use vyna_dcm_net as net;
pub use vyna_dcm_pdf as pdf;
#[cfg(feature = "web")]
pub use vyna_dcm_web as web;

/// Crate version.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

pub use vyna_dcm_atna::{
    ActionCode, AuditMessage, Code, Outcome, Participant, ParticipantObject, SyslogSender,
    SyslogTarget,
};
pub use vyna_dcm_codec::{
    DicomDir, DirEntry, FileMeta, TranscodeOptions, build_dicomdir, dataset_from_json,
    dataset_to_json, decode_dataset, decode_pixels, dump_dataset, encode_dataset, read_dicomdir,
    read_part10, transcode_dataset, write_part10,
};
pub use vyna_dcm_core::{
    DataElement, DataSet, PersonName, Tag, TransferSyntax, Value, Vr, lookup_keyword, new_uid, tags,
};
pub use vyna_dcm_deid::{Action, DeidReport, Deidentifier};
pub use vyna_dcm_iod::{Finding, Iod, Report, Severity, find_iod, validate};
pub use vyna_dcm_pdf::{ExtractedPdf, PdfInstanceMeta, pdf_from_dataset, pdf_to_dataset};
