//! `vyna-dcm-deid`: de-identification for research exports.
//!
//! - [`Action`] — per-element actions (PS3.15 Annex E semantics)
//! - [`BASIC_PROFILE`] — curated tag table (subset of E.1, documented)
//! - [`Deidentifier`] — the engine: dummies, UID remaps, date shifts
//! - [`DeidReport`] — what one run changed
//! - [`shift_da`] — longitudinal `DA` shifting
//!
//! Unlisted tags are kept, sequences are recursed into, and private
//! groups plus overlay planes are removed by rule. Pixels are never
//! altered: `YES` Burned In Annotation refuses the run.
//!
//! # Example
//!
//! ```
//! use vyna_dcm_core::{DataSet, Vr, tags};
//! use vyna_dcm_deid::Deidentifier;
//!
//! let mut ds = DataSet::new();
//! ds.put_text(tags::PATIENT_ID, Vr::Lo, "E123");
//! ds.put_text(tags::STUDY_DATE, Vr::Da, "20240115");
//! let report = Deidentifier::new()
//!     .with_date_offset(30)
//!     .apply(&mut ds)
//!     .unwrap();
//! assert_eq!(ds.string(tags::PATIENT_ID), Some("ANONYMIZED"));
//! assert_eq!(ds.string(tags::STUDY_DATE), Some("20240214"));
//! assert_eq!(report.dates_shifted, 1);
//! ```

#![warn(missing_docs)]

mod dates;
mod engine;
mod error;
mod profile;

pub use dates::{civil_from_days, days_from_civil, shift_da, shift_dt};
pub use engine::{DEFAULT_REPLACEMENT, DeidReport, Deidentifier};
pub use error::{Error, Result};
pub use profile::{Action, BASIC_PROFILE, DUMMY_DATE, DUMMY_DATETIME};
