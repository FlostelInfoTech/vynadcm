//! A de-identified 12-lead ECG instance stays IOD-valid with identical samples.

use vyna_dcm::core::{PersonName, tags};
use vyna_dcm::ecg::{
    EcgInstanceMeta, EcgLead, EcgRecording, STANDARD_12_LEAD, recording_from_waveform_item,
    recording_to_dataset,
};
use vyna_dcm::{Deidentifier, validate};

fn recording() -> EcgRecording {
    let leads = STANDARD_12_LEAD
        .iter()
        .map(|(label, code)| {
            let mut lead = EcgLead::new(*label, (0..200).map(|t| t % 37 - 18).collect(), 0.001);
            lead.mdc_code = Some((*code).to_string());
            lead
        })
        .collect();
    EcgRecording::new(leads, 500.0).unwrap()
}

#[test]
fn deidentified_ecg_validates_with_identical_samples() {
    let mut meta = EcgInstanceMeta::new(PersonName::parse("Doe^Jane"), "E123456");
    meta.accession = Some("A789".into());
    meta.birth_date = Some("19800304".into());
    meta.sex = Some("M".into());
    meta.study_id = Some("S1".into());
    meta.content_date = Some("20261011".into());
    meta.institution = Some("City Hospital".into());
    meta.series_description = Some("resting".into());
    let mut ds = recording_to_dataset(&recording(), &meta).unwrap();
    // The builder emits a sparse instance; complete the Type 1/2 shape.
    use vyna_dcm::core::Vr;
    ds.put_text(tags::STUDY_DATE, Vr::Da, "20261011");
    ds.put_text(tags::STUDY_TIME, Vr::Tm, "120000");
    ds.put_text(tags::CONTENT_TIME, Vr::Tm, "120000");
    ds.put_text(tags::ACQUISITION_DATE_TIME, Vr::Dt, "20261011120000");
    ds.put_person_name(
        tags::REFERRING_PHYSICIAN_NAME,
        vec![PersonName::parse("Ref^Doc")],
    );
    ds.put_sequence(tags::ACQUISITION_CONTEXT_SEQUENCE, vec![]);
    let class = ds.string(tags::SOP_CLASS_UID).unwrap().to_string();

    let pre = validate(&ds, &class, None).unwrap();
    assert!(!pre.has_errors(), "fixture must validate: {pre:?}");

    let sop_before = ds.string(tags::SOP_INSTANCE_UID).unwrap().to_string();
    let mut deid = ds.clone();
    let report = Deidentifier::new()
        .with_date_offset(30)
        .apply(&mut deid)
        .unwrap();
    assert!(report.uids_remapped > 0);

    let post = validate(&deid, &class, None).unwrap();
    assert!(!post.has_errors(), "de-identified must validate: {post:?}");

    assert_eq!(deid.string(tags::PATIENT_ID), Some("ANONYMIZED"));
    assert_eq!(deid.string(tags::ACCESSION_NUMBER), None);
    assert_eq!(deid.string(tags::PATIENT_BIRTH_DATE), None);
    assert_eq!(deid.string(tags::CONTENT_DATE), Some("20261110"));
    assert_eq!(deid.string(tags::STUDY_DATE), Some("20261110"));
    assert_eq!(
        deid.string(tags::ACQUISITION_DATE_TIME),
        Some("20261110120000")
    );
    assert_eq!(deid.string(tags::CONTENT_TIME), Some("120000")); // TM kept
    assert_eq!(deid.string(tags::STUDY_TIME), Some("120000")); // TM kept
    assert!(!deid.contains(tags::INSTITUTION_NAME));
    assert_eq!(deid.string(tags::SERIES_DESCRIPTION), None);
    assert_eq!(deid.string(tags::STUDY_ID), None);
    assert_ne!(deid.string(tags::SOP_INSTANCE_UID).unwrap(), sop_before);
    assert_eq!(
        deid.string(tags::LONGITUDINAL_TEMPORAL_INFORMATION_MODIFIED),
        Some("MODIFIED")
    );

    // Waveform items carry no PHI and must be byte-identical.
    let items_before = ds.sequence(tags::WAVEFORM_SEQUENCE).unwrap();
    let items_after = deid.sequence(tags::WAVEFORM_SEQUENCE).unwrap();
    assert_eq!(items_before, items_after);
    let rec_before = recording_from_waveform_item(&items_before[0]).unwrap();
    let rec_after = recording_from_waveform_item(&items_after[0]).unwrap();
    assert_eq!(rec_before.leads.len(), rec_after.leads.len());
    for (a, b) in rec_before.leads.iter().zip(rec_after.leads.iter()) {
        assert_eq!(a.samples, b.samples);
    }
}
