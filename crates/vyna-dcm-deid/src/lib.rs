//! `vyna-dcm-deid`: de-identification for research exports.
//!
//! - [`Action`] — per-element actions (PS3.15 Annex E semantics)
//! - [`BASIC_PROFILE`] — tag table transcribed from E.1 (generated)
//! - [`Deidentifier`] — the engine: dummies, UID remaps, date shifts
//! - [`DeidReport`] — what one run changed
//! - [`shift_da`] — longitudinal `DA` shifting
//!
//! Unlisted tags are kept, sequences are recursed into, and private
//! groups, overlay planes, plus curve-data groups are removed by rule.
//! Pixels are never altered: `YES` Burned In Annotation refuses the run.
//!
//! # Example
//!
//! ```
//! use vyna_dcm_core::{DataSet, Vr, tags};
//! use vyna_dcm_deid::Deidentifier;
//!
//! let mut ds = DataSet::new();
//! ds.put_text(tags::CLINICAL_TRIAL_SUBJECT_ID, Vr::Lo, "SUBJ-7");
//! ds.put_text(tags::STUDY_DATE, Vr::Da, "20240115");
//! let report = Deidentifier::new()
//!     .with_date_offset(30)
//!     .apply(&mut ds)
//!     .unwrap();
//! assert_eq!(
//!     ds.string(tags::CLINICAL_TRIAL_SUBJECT_ID),
//!     Some("ANONYMIZED")
//! );
//! assert_eq!(ds.string(tags::STUDY_DATE), Some("20240214"));
//! assert_eq!(report.dates_shifted, 1);
//! ```

#![warn(missing_docs)]

mod dates;
mod engine;
mod error;
mod profile;
mod profile_generated;

pub use dates::{civil_from_days, days_from_civil, shift_da, shift_dt};
pub use engine::{DEFAULT_REPLACEMENT, DeidReport, Deidentifier};
pub use error::{Error, Result};
pub use profile::{Action, BASIC_PROFILE, DUMMY_AGE, DUMMY_DATE, DUMMY_DATETIME, DUMMY_TIME};
