//! UIDs: SOP classes, well-known instances, validation and generation.
//!
//! UID roots owned by standards bodies live below; generated instance UIDs
//! use the UUID-based `2.25.<decimal>` form (PS3.5 §B.12), which needs no
//! registered organisation root.

use uuid::Uuid;

use crate::{Error, Result};

/// Validate UID syntax: `0-9` and `.`, ≤ 64 chars, no empty components,
/// no leading zeroes (except a literal `0` component).
#[must_use]
pub fn is_valid_uid(uid: &str) -> bool {
    if uid.is_empty() || uid.len() > 64 {
        return false;
    }
    if !uid.bytes().all(|b| b.is_ascii_digit() || b == b'.') {
        return false;
    }
    if uid.starts_with('.') || uid.ends_with('.') || uid.contains("..") {
        return false;
    }
    uid.split('.').all(|c| c == "0" || !c.starts_with('0'))
}

/// Validate or return a typed error.
pub fn check_uid(uid: &str) -> Result<()> {
    if is_valid_uid(uid) {
        Ok(())
    } else {
        Err(Error::InvalidUid {
            value: uid.to_string(),
            reason: "bad UID syntax",
        })
    }
}

/// Generate a fresh instance UID as `2.25.<uuid-as-decimal>`.
#[must_use]
pub fn new_uid() -> String {
    format!("2.25.{}", Uuid::new_v4().as_u128())
}

/// VynaDCM implementation class UID (stable per release line).
pub const VYNA_IMPLEMENTATION_CLASS_UID: &str = "2.25.46417651854989365804951432447169401540";

/// VynaDCM implementation version name (file meta `SH`, ≤ 16 chars).
pub const VYNA_IMPLEMENTATION_VERSION_NAME: &str = "VYNA_DCM_010";

/// DICOM application context name (`1.2.840.10008.3.1.1.1`).
pub const APPLICATION_CONTEXT_NAME: &str = "1.2.840.10008.3.1.1.1";

/// SOP Class UIDs (NEMA PS3.6 Annex A subset for ECG/PACS/Q&R/MWL).
pub mod sop_class {
    /// Verification (C-ECHO).
    pub const VERIFICATION: &str = "1.2.840.10008.1.1";
    /// Computed Radiography Image Storage.
    pub const CR_IMAGE: &str = "1.2.840.10008.5.1.4.1.1.1";
    /// CT Image Storage.
    pub const CT_IMAGE: &str = "1.2.840.10008.5.1.4.1.1.2";
    /// MR Image Storage.
    pub const MR_IMAGE: &str = "1.2.840.10008.5.1.4.1.1.4";
    /// Enhanced CT Image Storage.
    pub const ENHANCED_CT_IMAGE: &str = "1.2.840.10008.5.1.4.1.1.2.1";
    /// Enhanced MR Image Storage.
    pub const ENHANCED_MR_IMAGE: &str = "1.2.840.10008.5.1.4.1.1.4.1";
    /// Digital X-Ray Image Storage — For Presentation.
    pub const DX_PRESENTATION: &str = "1.2.840.10008.5.1.4.1.1.1.1";
    /// Digital X-Ray Image Storage — For Processing.
    pub const DX_PROCESSING: &str = "1.2.840.10008.5.1.4.1.1.1.1.1";
    /// Digital Mammography X-Ray Image Storage — For Presentation.
    pub const MG_PRESENTATION: &str = "1.2.840.10008.5.1.4.1.1.1.2";
    /// Digital Mammography X-Ray Image Storage — For Processing.
    pub const MG_PROCESSING: &str = "1.2.840.10008.5.1.4.1.1.1.2.1";
    /// Ultrasound Image Storage (single-frame).
    pub const US_IMAGE: &str = "1.2.840.10008.5.1.4.1.1.6.1";
    /// Ultrasound Multi-frame Image Storage.
    pub const US_MULTIFRAME: &str = "1.2.840.10008.5.1.4.1.1.3.1";
    /// X-Ray Angiographic Image Storage.
    pub const XA_IMAGE: &str = "1.2.840.10008.5.1.4.1.1.12.1";
    /// Nuclear Medicine Image Storage.
    pub const NM_IMAGE: &str = "1.2.840.10008.5.1.4.1.1.20";
    /// Positron Emission Tomography Image Storage.
    pub const PT_IMAGE: &str = "1.2.840.10008.5.1.4.1.1.128";
    /// VL Photographic Image Storage.
    pub const VL_PHOTOGRAPHIC: &str = "1.2.840.10008.5.1.4.1.1.77.1.4";
    /// Key Object Selection Document Storage.
    pub const KEY_OBJECT_SELECTION: &str = "1.2.840.10008.5.1.4.1.1.88.59";
    /// Secondary Capture Image Storage.
    pub const SECONDARY_CAPTURE_IMAGE: &str = "1.2.840.10008.5.1.4.1.1.7";
    /// 12-Lead ECG Waveform Storage.
    pub const TWELVE_LEAD_ECG_WAVEFORM: &str = "1.2.840.10008.5.1.4.1.1.9.1.1";
    /// General ECG Waveform Storage.
    pub const GENERAL_ECG_WAVEFORM: &str = "1.2.840.10008.5.1.4.1.1.9.1.2";
    /// Ambulatory ECG Waveform Storage.
    pub const AMBULATORY_ECG_WAVEFORM: &str = "1.2.840.10008.5.1.4.1.1.9.1.3";
    /// Hemodynamic Waveform Storage.
    pub const HEMODYNAMIC_WAVEFORM: &str = "1.2.840.10008.5.1.4.1.1.9.2.1";
    /// Basic Text SR Storage.
    pub const BASIC_TEXT_SR: &str = "1.2.840.10008.5.1.4.1.1.88.11";
    /// Enhanced SR Storage.
    pub const ENHANCED_SR: &str = "1.2.840.10008.5.1.4.1.1.88.22";
    /// Comprehensive SR Storage.
    pub const COMPREHENSIVE_SR: &str = "1.2.840.10008.5.1.4.1.1.88.33";
    /// Encapsulated PDF Storage.
    pub const ENCAPSULATED_PDF: &str = "1.2.840.10008.5.1.4.1.1.104.1";
    /// Encapsulated CDA Storage.
    pub const ENCAPSULATED_CDA: &str = "1.2.840.10008.5.1.4.1.1.104.2";
    /// Patient Root Q/R — FIND.
    pub const PATIENT_ROOT_FIND: &str = "1.2.840.10008.5.1.4.1.2.1.1";
    /// Patient Root Q/R — MOVE.
    pub const PATIENT_ROOT_MOVE: &str = "1.2.840.10008.5.1.4.1.2.1.2";
    /// Patient Root Q/R — GET.
    pub const PATIENT_ROOT_GET: &str = "1.2.840.10008.5.1.4.1.2.1.3";
    /// Study Root Q/R — FIND.
    pub const STUDY_ROOT_FIND: &str = "1.2.840.10008.5.1.4.1.2.2.1";
    /// Study Root Q/R — MOVE.
    pub const STUDY_ROOT_MOVE: &str = "1.2.840.10008.5.1.4.1.2.2.2";
    /// Study Root Q/R — GET.
    pub const STUDY_ROOT_GET: &str = "1.2.840.10008.5.1.4.1.2.2.3";
    /// Patient/Study Only Q/R — FIND.
    pub const PATIENT_STUDY_ONLY_FIND: &str = "1.2.840.10008.5.1.4.1.2.3.1";
    /// Patient/Study Only Q/R — MOVE.
    pub const PATIENT_STUDY_ONLY_MOVE: &str = "1.2.840.10008.5.1.4.1.2.3.2";
    /// Modality Worklist — FIND.
    pub const MODALITY_WORKLIST_FIND: &str = "1.2.840.10008.5.1.4.31";
    /// Modality Performed Procedure Step.
    pub const MODALITY_PERFORMED_PROCEDURE_STEP: &str = "1.2.840.10008.3.1.2.3.3";
    /// Storage Commitment Push Model.
    pub const STORAGE_COMMITMENT_PUSH: &str = "1.2.840.10008.1.20.1";
    /// Storage Commitment Pull Model.
    pub const STORAGE_COMMITMENT_PULL: &str = "1.2.840.10008.1.20.2";
    /// Media Storage Directory Storage (DICOMDIR).
    pub const MEDIA_STORAGE_DIRECTORY: &str = "1.2.840.10008.1.3.10";
    /// RT Dose Storage.
    pub const RT_DOSE: &str = "1.2.840.10008.5.1.4.1.1.481.2";
    /// RT Structure Set Storage.
    pub const RT_STRUCTURE_SET: &str = "1.2.840.10008.5.1.4.1.1.481.3";
    /// RT Plan Storage.
    pub const RT_PLAN: &str = "1.2.840.10008.5.1.4.1.1.481.5";

    /// All storage SOP classes this library proposes by default.
    pub const DEFAULT_STORAGE_CLASSES: &[&str] = &[
        CR_IMAGE,
        CT_IMAGE,
        MR_IMAGE,
        ENHANCED_CT_IMAGE,
        ENHANCED_MR_IMAGE,
        DX_PRESENTATION,
        DX_PROCESSING,
        MG_PRESENTATION,
        MG_PROCESSING,
        US_IMAGE,
        US_MULTIFRAME,
        XA_IMAGE,
        NM_IMAGE,
        PT_IMAGE,
        VL_PHOTOGRAPHIC,
        KEY_OBJECT_SELECTION,
        SECONDARY_CAPTURE_IMAGE,
        TWELVE_LEAD_ECG_WAVEFORM,
        GENERAL_ECG_WAVEFORM,
        AMBULATORY_ECG_WAVEFORM,
        HEMODYNAMIC_WAVEFORM,
        BASIC_TEXT_SR,
        ENHANCED_SR,
        COMPREHENSIVE_SR,
        ENCAPSULATED_PDF,
        ENCAPSULATED_CDA,
    ];

    /// All query/retrieve + worklist SOP classes.
    pub const QUERY_CLASSES: &[&str] = &[
        PATIENT_ROOT_FIND,
        PATIENT_ROOT_MOVE,
        PATIENT_ROOT_GET,
        STUDY_ROOT_FIND,
        STUDY_ROOT_MOVE,
        STUDY_ROOT_GET,
        PATIENT_STUDY_ONLY_FIND,
        PATIENT_STUDY_ONLY_MOVE,
        MODALITY_WORKLIST_FIND,
    ];
}

/// Well-known framing UIDs.
pub mod well_known {
    /// Default application context (same as [`crate::APPLICATION_CONTEXT_NAME`]).
    pub const APPLICATION_CONTEXT: &str = "1.2.840.10008.3.1.1.1";
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uid_validation_rules() {
        assert!(is_valid_uid("1.2.840.10008.1.1"));
        assert!(is_valid_uid("2.25.46417651854989365804951432447169401540"));
        assert!(is_valid_uid("1.0"));
        assert!(!is_valid_uid(""));
        assert!(!is_valid_uid("1.2.840.10008.1.1."));
        assert!(!is_valid_uid(".1.2"));
        assert!(!is_valid_uid("1..2"));
        assert!(!is_valid_uid("1.02.3"));
        assert!(!is_valid_uid("1.2a.3"));
        assert!(!is_valid_uid(&"1.".repeat(40)));
        assert!(check_uid("1.2.3").is_ok());
        assert!(check_uid("xx").is_err());
    }

    #[test]
    fn generated_uids_are_valid_and_unique() {
        let a = new_uid();
        let b = new_uid();
        assert_ne!(a, b);
        assert!(is_valid_uid(&a) && is_valid_uid(&b));
        assert!(a.starts_with("2.25."));
        assert!(is_valid_uid(VYNA_IMPLEMENTATION_CLASS_UID));
        assert!(is_valid_uid(sop_class::MODALITY_WORKLIST_FIND));
        for uid in [
            sop_class::DX_PRESENTATION,
            sop_class::DX_PROCESSING,
            sop_class::MG_PRESENTATION,
            sop_class::MG_PROCESSING,
            sop_class::US_IMAGE,
            sop_class::US_MULTIFRAME,
            sop_class::XA_IMAGE,
            sop_class::NM_IMAGE,
            sop_class::PT_IMAGE,
            sop_class::VL_PHOTOGRAPHIC,
            sop_class::KEY_OBJECT_SELECTION,
        ] {
            assert!(is_valid_uid(uid), "{uid}");
        }
        for uid in sop_class::DEFAULT_STORAGE_CLASSES
            .iter()
            .chain(sop_class::QUERY_CLASSES)
        {
            assert!(is_valid_uid(uid), "{uid}");
        }
    }
}
