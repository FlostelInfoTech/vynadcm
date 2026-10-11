//! `vyna-dcm-ecg`: ECG waveform helpers for VynaECG.
//!
//! - [`EcgRecording`] / [`EcgLead`] — the lead/sample model with
//!   physical-unit conversion
//! - [`recording_to_waveform_item`] / [`recording_from_waveform_item`] —
//!   Waveform Sequence (multiplex group) mapping
//! - [`recording_to_dataset`] — complete 12-Lead / General ECG SOP
//!   instances ready for C-STORE, Part-10 or STOW-RS
//!
//! # Example
//!
//! ```
//! use vyna_dcm_ecg::{EcgLead, EcgRecording, recording_to_waveform_item};
//!
//! let leads = vec![EcgLead::new("II", vec![0; 500], 0.001)];
//! let rec = EcgRecording::new(leads, 500.0).unwrap();
//! let item = recording_to_waveform_item(&rec).unwrap();
//! assert_eq!(item.string(vyna_dcm_core::tags::SAMPLING_FREQUENCY), Some("500"));
//! ```

#![warn(missing_docs)]

mod error;
pub mod model;
pub mod waveform;

pub use error::{Error, Result};
pub use model::{EcgLead, EcgRecording, STANDARD_12_LEAD, SampleInterpretation, mdc_code_for_lead};
pub use waveform::{
    EcgInstanceMeta, SOP_12_LEAD_ECG, SOP_GENERAL_ECG, file_meta_for_dataset,
    recording_from_waveform_item, recording_to_dataset, recording_to_waveform_item,
};
