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
//!
//! # Features
//!
//! Default: `net`, `web`, `ecg`, `cli` (everything, including the
//! binaries). Library embeds should start from
//! `default-features = false` and add only the stacks they need
//! (`net` pulls tokio-rustls, `web` pulls reqwest/hyper); `cli`
//! exists only to build the binaries and changes nothing in the
//! library.

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
    DicomDir, DirEntry, FileMeta, RedactFrames, RedactRegion, TranscodeOptions, build_dicomdir,
    dataset_from_json, dataset_from_xml, dataset_from_xml_with_resolver, dataset_to_json,
    dataset_to_xml, decode_dataset, decode_pixels, dump_dataset, encode_dataset, frame_to_png,
    inject_png, read_dicomdir, read_part10, redact_regions, transcode_dataset, write_part10,
};
pub use vyna_dcm_core::{
    DataElement, DataSet, PersonName, Tag, TransferSyntax, Value, Vr, lookup_keyword, new_uid, tags,
};
pub use vyna_dcm_deid::{Action, DeidReport, Deidentifier};
pub use vyna_dcm_iod::{Finding, Iod, Report, Severity, find_iod, validate};
pub use vyna_dcm_pdf::{ExtractedPdf, PdfInstanceMeta, pdf_from_dataset, pdf_to_dataset};
