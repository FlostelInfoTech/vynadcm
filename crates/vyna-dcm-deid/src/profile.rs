//! De-identification actions and the curated basic profile.
//!
//! Action letters follow PS3.15 Annex E semantics; the tag list is a
//! curated subset (see [`BASIC_PROFILE`}), not a transcription of E.1.

use vyna_dcm_core::{Tag, tags};

/// What to do with one data element.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// Replace with a dummy: the configured patient name for `PN`
    /// values, the replacement ID otherwise. Always non-empty, hence
    /// safe for Type 1 attributes.
    Dummy,
    /// Empty the value but keep the element (valid for Type 2/3,
    /// including sequences, which encode as zero items).
    Zero,
    /// Remove the element. Used only for Type 3 attributes, whole
    /// module removals (clinical trial), stale provenance, and
    /// signatures (which de-identification invalidates).
    Remove,
    /// Replace each UID component with a consistent remap: the same
    /// input UID always maps to the same fresh UID within one
    /// [`Deidentifier`](crate::Deidentifier).
    RemapUid,
    /// Shift `DA`/`DT` calendar days by the configured offset
    /// (keeping longitudinal intervals); write the [`DUMMY_DATE`] /
    /// [`DUMMY_DATETIME`] when no offset is set. Never empty, hence
    /// safe for Type 1 dates (e.g. waveform Content Date).
    ShiftDate,
}

/// Dummy `DA` written when no date offset is configured.
pub const DUMMY_DATE: &str = "19000101";

/// Dummy `DT` written when no date offset is configured.
pub const DUMMY_DATETIME: &str = "19000101000000";

/// Curated basic profile: `(tag, action)` pairs.
///
/// Coverage: patient demographics/contacts/IDs, visit/admission data,
/// institution/staff/device identity, order/filler numbers, clinical-trial
/// module, icon/annotation free text, all instance UIDs, and clinical
/// dates. Unlisted tags are kept (notably clinical content, anatomy,
/// acquisition technique, and `TM` times), sequences are recursed into,
/// private groups and overlay planes are removed by rule (see the
/// engine), and `Specific Character Set` conversion is out of scope.
///
/// Type discipline: [`Action::Dummy`], [`Action::RemapUid`], and
/// [`Action::ShiftDate`] (all non-empty) are the only actions touching
/// attributes that can be Type 1; every [`Action::Zero`] target is
/// Type 2/3 and every [`Action::Remove`] target is Type 3,
/// module-removal, provenance, or a signature.
pub const BASIC_PROFILE: &[(Tag, Action)] = &[
    // -- dummies (identifiers that must stay non-empty) --
    (tags::PATIENT_NAME, Action::Dummy),
    (tags::PATIENT_ID, Action::Dummy),
    (tags::PERSON_NAME, Action::Dummy),
    (tags::HUMAN_PERFORMER_NAME, Action::Dummy),
    (tags::CONTENT_CREATOR_NAME, Action::Dummy),
    (tags::TRACKING_ID, Action::Dummy),
    // -- shiftable dates --
    (tags::STUDY_DATE, Action::ShiftDate),
    (tags::SERIES_DATE, Action::ShiftDate),
    (tags::ACQUISITION_DATE, Action::ShiftDate),
    (tags::CONTENT_DATE, Action::ShiftDate),
    (tags::INSTANCE_CREATION_DATE, Action::ShiftDate),
    (tags::ADMITTING_DATE, Action::ShiftDate),
    (tags::DISCHARGE_DATE, Action::ShiftDate),
    (tags::SCHEDULED_PROCEDURE_STEP_START_DATE, Action::ShiftDate),
    (tags::SCHEDULED_PROCEDURE_STEP_END_DATE, Action::ShiftDate),
    (tags::PRESENTATION_CREATION_DATE, Action::ShiftDate),
    (tags::DATE, Action::ShiftDate),
    (tags::STRUCTURE_SET_DATE, Action::ShiftDate),
    // -- UID remaps --
    (tags::STUDY_INSTANCE_UID, Action::RemapUid),
    (tags::SERIES_INSTANCE_UID, Action::RemapUid),
    (tags::SOP_INSTANCE_UID, Action::RemapUid),
    (tags::FRAME_OF_REFERENCE_UID, Action::RemapUid),
    (tags::REFERENCED_SOP_INSTANCE_UID, Action::RemapUid),
    (tags::REFERENCED_FRAME_OF_REFERENCE_UID, Action::RemapUid),
    (tags::RELATED_FRAME_OF_REFERENCE_UID, Action::RemapUid),
    (
        tags::SYNCHRONIZATION_FRAME_OF_REFERENCE_UID,
        Action::RemapUid,
    ),
    (tags::CONCATENATION_UID, Action::RemapUid),
    (tags::DIMENSION_ORGANIZATION_UID, Action::RemapUid),
    (tags::DEVICE_UID, Action::RemapUid),
    (tags::SPECIMEN_UID, Action::RemapUid),
    (tags::FIDUCIAL_UID, Action::RemapUid),
    (tags::ANNOTATION_GROUP_UID, Action::RemapUid),
    (tags::TRACKING_UID, Action::RemapUid),
    (tags::PRESENTATION_DISPLAY_COLLECTION_UID, Action::RemapUid),
    (tags::TRANSACTION_UID, Action::RemapUid),
    (tags::IRRADIATION_EVENT_UID, Action::RemapUid),
    (tags::OBSERVATION_UID, Action::RemapUid),
    (tags::UID, Action::RemapUid),
    // -- removals: Type 3 / module / provenance / signatures --
    (tags::ISSUER_OF_PATIENT_ID, Action::Remove),
    (tags::PATIENT_BIRTH_TIME, Action::Remove),
    (tags::PATIENT_BIRTH_NAME, Action::Remove),
    (tags::PATIENT_MOTHER_BIRTH_NAME, Action::Remove),
    (tags::PATIENT_ADDRESS, Action::Remove),
    (tags::PATIENT_TELEPHONE_NUMBERS, Action::Remove),
    (tags::PATIENT_RELIGIOUS_PREFERENCE, Action::Remove),
    (tags::MILITARY_RANK, Action::Remove),
    (tags::BRANCH_OF_SERVICE, Action::Remove),
    (tags::MEDICAL_RECORD_LOCATOR, Action::Remove),
    (tags::PATIENT_INSURANCE_PLAN_CODE_SEQUENCE, Action::Remove),
    (tags::PATIENT_PRIMARY_LANGUAGE_CODE_SEQUENCE, Action::Remove),
    (
        tags::PATIENT_PRIMARY_LANGUAGE_MODIFIER_CODE_SEQUENCE,
        Action::Remove,
    ),
    (tags::OTHER_PATIENT_NAMES, Action::Remove),
    (tags::OTHER_PATIENT_I_DS, Action::Remove),
    (tags::OTHER_PATIENT_I_DS_SEQUENCE, Action::Remove),
    (tags::RESPONSIBLE_PERSON, Action::Remove),
    (tags::RESPONSIBLE_ORGANIZATION, Action::Remove),
    (tags::PATIENT_SPECIES_DESCRIPTION, Action::Remove),
    (tags::PATIENT_SPECIES_CODE_SEQUENCE, Action::Remove),
    (tags::PATIENT_BREED_DESCRIPTION, Action::Remove),
    (tags::PATIENT_BREED_CODE_SEQUENCE, Action::Remove),
    (tags::COUNTRY_OF_RESIDENCE, Action::Remove),
    (tags::REGION_OF_RESIDENCE, Action::Remove),
    (tags::INSTITUTION_NAME, Action::Remove),
    (tags::INSTITUTION_ADDRESS, Action::Remove),
    (tags::INSTITUTION_CODE_SEQUENCE, Action::Remove),
    (tags::INSTITUTIONAL_DEPARTMENT_NAME, Action::Remove),
    (tags::STATION_NAME, Action::Remove),
    (tags::DEVICE_SERIAL_NUMBER, Action::Remove),
    (tags::GANTRY_ID, Action::Remove),
    (tags::GENERATOR_ID, Action::Remove),
    (tags::CASSETTE_ID, Action::Remove),
    (tags::PLATE_ID, Action::Remove),
    (tags::SECONDARY_CAPTURE_DEVICE_ID, Action::Remove),
    (tags::DATE_OF_LAST_CALIBRATION, Action::Remove),
    (tags::ICON_IMAGE_SEQUENCE, Action::Remove),
    (tags::ORIGINAL_ATTRIBUTES_SEQUENCE, Action::Remove),
    (tags::MODIFIED_ATTRIBUTES_SEQUENCE, Action::Remove),
    (tags::DEIDENTIFICATION_METHOD, Action::Remove),
    (tags::DEIDENTIFICATION_METHOD_CODE_SEQUENCE, Action::Remove),
    (tags::PATIENT_IDENTITY_REMOVED, Action::Remove),
    (tags::DIGITAL_SIGNATURES_SEQUENCE, Action::Remove),
    (tags::CLINICAL_TRIAL_SPONSOR_NAME, Action::Remove),
    (tags::CLINICAL_TRIAL_PROTOCOL_ID, Action::Remove),
    (tags::CLINICAL_TRIAL_PROTOCOL_NAME, Action::Remove),
    (tags::CLINICAL_TRIAL_SITE_ID, Action::Remove),
    (tags::CLINICAL_TRIAL_SITE_NAME, Action::Remove),
    (tags::CLINICAL_TRIAL_SUBJECT_ID, Action::Remove),
    (tags::CLINICAL_TRIAL_SUBJECT_READING_ID, Action::Remove),
    (
        tags::CLINICAL_TRIAL_PROTOCOL_ETHICS_COMMITTEE_NAME,
        Action::Remove,
    ),
    (
        tags::CLINICAL_TRIAL_PROTOCOL_ETHICS_COMMITTEE_APPROVAL_NUMBER,
        Action::Remove,
    ),
    (
        tags::CLINICAL_TRIAL_COORDINATING_CENTER_NAME,
        Action::Remove,
    ),
    (
        tags::PATIENT_CLINICAL_TRIAL_PARTICIPATION_SEQUENCE,
        Action::Remove,
    ),
    // -- zeroed: names, IDs, contacts, descriptions, quasi-identifiers --
    (tags::PATIENT_BIRTH_DATE, Action::Zero),
    (tags::LAST_MENSTRUAL_DATE, Action::Zero),
    (
        tags::PATIENT_BIRTH_DATE_IN_ALTERNATIVE_CALENDAR,
        Action::Zero,
    ),
    (tags::STUDY_ID, Action::Zero),
    (tags::ACCESSION_NUMBER, Action::Zero),
    (tags::ISSUER_OF_ACCESSION_NUMBER_SEQUENCE, Action::Zero),
    (tags::ADMISSION_ID, Action::Zero),
    (tags::ISSUER_OF_ADMISSION_ID_SEQUENCE, Action::Zero),
    (tags::REFERRING_PHYSICIAN_NAME, Action::Zero),
    (tags::REFERRING_PHYSICIAN_TELEPHONE_NUMBERS, Action::Zero),
    (
        tags::REFERRING_PHYSICIAN_IDENTIFICATION_SEQUENCE,
        Action::Zero,
    ),
    (tags::CONSULTING_PHYSICIAN_NAME, Action::Zero),
    (
        tags::CONSULTING_PHYSICIAN_IDENTIFICATION_SEQUENCE,
        Action::Zero,
    ),
    (tags::PHYSICIANS_OF_RECORD, Action::Zero),
    (
        tags::PHYSICIANS_OF_RECORD_IDENTIFICATION_SEQUENCE,
        Action::Zero,
    ),
    (tags::NAME_OF_PHYSICIANS_READING_STUDY, Action::Zero),
    (tags::PERFORMING_PHYSICIAN_NAME, Action::Zero),
    (
        tags::PERFORMING_PHYSICIAN_IDENTIFICATION_SEQUENCE,
        Action::Zero,
    ),
    (tags::OPERATORS_NAME, Action::Zero),
    (tags::OPERATOR_IDENTIFICATION_SEQUENCE, Action::Zero),
    (tags::REQUESTING_PHYSICIAN, Action::Zero),
    (tags::REQUESTING_SERVICE, Action::Zero),
    (tags::VERIFYING_OBSERVER_NAME, Action::Zero),
    (
        tags::VERIFYING_OBSERVER_IDENTIFICATION_CODE_SEQUENCE,
        Action::Zero,
    ),
    (tags::VERIFYING_ORGANIZATION, Action::Zero),
    (tags::AUTHOR_OBSERVER_SEQUENCE, Action::Zero),
    (tags::CUSTODIAL_ORGANIZATION_SEQUENCE, Action::Zero),
    (tags::HUMAN_PERFORMER_CODE_SEQUENCE, Action::Zero),
    (tags::HUMAN_PERFORMER_ORGANIZATION, Action::Zero),
    (tags::REVIEWER_NAME, Action::Zero),
    (
        tags::CONTENT_CREATOR_IDENTIFICATION_CODE_SEQUENCE,
        Action::Zero,
    ),
    (tags::STUDY_DESCRIPTION, Action::Zero),
    (tags::STUDY_COMMENTS, Action::Zero),
    (tags::SERIES_DESCRIPTION, Action::Zero),
    (tags::IMAGE_COMMENTS, Action::Zero),
    (tags::PATIENT_COMMENTS, Action::Zero),
    (tags::UNFORMATTED_TEXT_VALUE, Action::Zero),
    (tags::TEXT_VALUE, Action::Zero),
    (tags::REQUESTED_PROCEDURE_ID, Action::Zero),
    (tags::REQUESTED_PROCEDURE_DESCRIPTION, Action::Zero),
    (tags::REQUESTED_PROCEDURE_COMMENTS, Action::Zero),
    (tags::SCHEDULED_PROCEDURE_STEP_ID, Action::Zero),
    (tags::SCHEDULED_PROCEDURE_STEP_DESCRIPTION, Action::Zero),
    (tags::SCHEDULED_PROCEDURE_STEP_LOCATION, Action::Zero),
    (tags::SCHEDULED_HUMAN_PERFORMERS_SEQUENCE, Action::Zero),
    (tags::COMMENTS_ON_THE_SCHEDULED_PROCEDURE_STEP, Action::Zero),
    (tags::CURRENT_PATIENT_LOCATION, Action::Zero),
    (tags::PATIENT_INSTITUTION_RESIDENCE, Action::Zero),
    (tags::SCHEDULED_PATIENT_INSTITUTION_RESIDENCE, Action::Zero),
    (tags::LOCATION, Action::Zero),
    (tags::ROUTE_OF_ADMISSIONS, Action::Zero),
    (tags::VISIT_COMMENTS, Action::Zero),
    (tags::VISIT_STATUS_ID, Action::Zero),
    (tags::SERVICE_EPISODE_ID, Action::Zero),
    (tags::ISSUER_OF_SERVICE_EPISODE_ID, Action::Zero),
    (tags::SERVICE_EPISODE_DESCRIPTION, Action::Zero),
    (tags::SPECIAL_NEEDS, Action::Zero),
    (tags::PATIENT_STATE, Action::Zero),
    (tags::PERTINENT_RESOURCES_SEQUENCE, Action::Zero),
    (tags::PERTINENT_DOCUMENTS_SEQUENCE, Action::Zero),
    (tags::PATIENT_TRANSPORT_ARRANGEMENTS, Action::Zero),
    (tags::IMAGING_SERVICE_REQUEST_COMMENTS, Action::Zero),
    (tags::NAMES_OF_INTENDED_RECIPIENTS_OF_RESULTS, Action::Zero),
    (
        tags::INTENDED_RECIPIENTS_OF_RESULTS_IDENTIFICATION_SEQUENCE,
        Action::Zero,
    ),
    (tags::PERSON_IDENTIFICATION_CODE_SEQUENCE, Action::Zero),
    (tags::PERSON_ADDRESS, Action::Zero),
    (tags::PERSON_TELEPHONE_NUMBERS, Action::Zero),
    (
        tags::PLACER_ORDER_NUMBER_IMAGING_SERVICE_REQUEST,
        Action::Zero,
    ),
    (tags::PLACER_ORDER_NUMBER_PROCEDURE, Action::Zero),
    (
        tags::FILLER_ORDER_NUMBER_IMAGING_SERVICE_REQUEST,
        Action::Zero,
    ),
    (tags::FILLER_ORDER_NUMBER_PROCEDURE, Action::Zero),
    (tags::ORDER_ENTERED_BY, Action::Zero),
    (tags::ORDER_ENTERER_LOCATION, Action::Zero),
    (tags::ORDER_CALLBACK_PHONE_NUMBER, Action::Zero),
    (tags::BURNED_IN_ANNOTATION, Action::Zero),
    (tags::TIMEZONE_OFFSET_FROM_UTC, Action::Zero),
    (tags::SMOKING_STATUS, Action::Zero),
    (tags::PREGNANCY_STATUS, Action::Zero),
    (tags::PATIENT_AGE, Action::Zero),
    (tags::PATIENT_SIZE, Action::Zero),
    (tags::PATIENT_WEIGHT, Action::Zero),
    (tags::PATIENT_BODY_MASS_INDEX, Action::Zero),
    (tags::OCCUPATION, Action::Zero),
    (tags::ETHNIC_GROUP, Action::Zero),
    (tags::DEVICE_ID, Action::Zero),
    (tags::SPECIMEN_ACCESSION_NUMBER, Action::Zero),
    (tags::CONTAINER_IDENTIFIER, Action::Zero),
    (
        tags::ISSUER_OF_THE_CONTAINER_IDENTIFIER_SEQUENCE,
        Action::Zero,
    ),
    (tags::SLIDE_IDENTIFIER, Action::Zero),
    (tags::CONTRIBUTION_DESCRIPTION, Action::Zero),
    (tags::HL_7_INSTANCE_IDENTIFIER, Action::Zero),
    // -- shiftable date-times --
    (tags::ACQUISITION_DATE_TIME, Action::ShiftDate),
    (tags::HL_7_DOCUMENT_EFFECTIVE_TIME, Action::ShiftDate),
    (tags::VERIFICATION_DATE_TIME, Action::ShiftDate),
    (tags::OBSERVATION_DATE_TIME, Action::ShiftDate),
    (tags::DATE_TIME, Action::ShiftDate),
    (tags::EXPECTED_COMPLETION_DATE_TIME, Action::ShiftDate),
    (tags::CONTRIBUTION_DATE_TIME, Action::ShiftDate),
    (tags::DATE_TIME_OF_LAST_CALIBRATION, Action::ShiftDate),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_has_no_duplicate_tags() {
        let mut seen = std::collections::HashSet::new();
        for (tag, _) in BASIC_PROFILE {
            assert!(seen.insert(tag), "duplicate profile tag {tag:?}");
        }
    }

    #[test]
    fn profile_tags_are_all_in_the_dictionary() {
        for (tag, _) in BASIC_PROFILE {
            assert!(
                vyna_dcm_core::ENTRIES.iter().any(|e| e.tag == *tag),
                "profile tag {tag:?} missing from the dictionary"
            );
        }
    }
}
