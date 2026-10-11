//! Modality Worklist, MPPS and Storage Commitment dataset builders.
//!
//! - MWL: C-FIND with SOP class [`MODALITY_WORKLIST`]; queries combine
//!   matching keys with empty (universal-match) return keys.
//! - MPPS: N-CREATE then N-SET on [`MODALITY_PPS`].
//! - Storage Commitment: N-ACTION (action type 1) on
//!   [`STORAGE_COMMITMENT_PUSH`], result via N-EVENT-REPORT.

use vyna_dcm_core::{DataSet, Vr, tags};

pub use vyna_dcm_core::sop_class::{
    MODALITY_PERFORMED_PROCEDURE_STEP as MODALITY_PPS, MODALITY_WORKLIST_FIND as MODALITY_WORKLIST,
    STORAGE_COMMITMENT_PUSH,
};

/// Storage Commitment N-ACTION action type: request commitment.
pub const COMMITMENT_ACTION_REQUEST: u16 = 1;
/// Storage Commitment N-EVENT-REPORT event type: storage commitment.
pub const COMMITMENT_EVENT_TYPE: u16 = 1;

/// Empty MWL identifier with the standard return keys.
#[must_use]
pub fn mwl_base_query() -> DataSet {
    let mut ds = DataSet::new();
    // Patient / visit return keys.
    ds.put_null(tags::PATIENT_NAME, Vr::Pn);
    ds.put_null(tags::PATIENT_ID, Vr::Lo);
    ds.put_null(tags::PATIENT_BIRTH_DATE, Vr::Da);
    ds.put_null(tags::PATIENT_SEX, Vr::Cs);
    ds.put_null(tags::ACCESSION_NUMBER, Vr::Sh);
    ds.put_null(tags::REQUESTING_PHYSICIAN, Vr::Pn);
    // Scheduled step return keys inside one sequence item.
    let mut item = DataSet::new();
    item.put_null(tags::SCHEDULED_STATION_AE_TITLE, Vr::Ae);
    item.put_null(tags::SCHEDULED_PROCEDURE_STEP_START_DATE, Vr::Da);
    item.put_null(tags::SCHEDULED_PROCEDURE_STEP_START_TIME, Vr::Tm);
    item.put_null(tags::MODALITY, Vr::Cs);
    item.put_null(tags::SCHEDULED_PERFORMING_PHYSICIAN_NAME, Vr::Pn);
    item.put_null(tags::SCHEDULED_PROCEDURE_STEP_DESCRIPTION, Vr::Lo);
    item.put_null(tags::SCHEDULED_PROCEDURE_STEP_ID, Vr::Sh);
    item.put_null(tags::SCHEDULED_STATION_NAME, Vr::Sh);
    item.put_null(tags::REQUESTED_PROCEDURE_ID, Vr::Sh);
    ds.put_sequence(tags::SCHEDULED_PROCEDURE_STEP_SEQUENCE, vec![item]);
    ds
}

/// MWL query constrained to a scheduled station AE title.
#[must_use]
pub fn mwl_query_for_station(station_ae: &str) -> DataSet {
    let mut ds = mwl_base_query();
    if let Some(item) = first_sps_mut(&mut ds) {
        item.put_text(tags::SCHEDULED_STATION_AE_TITLE, Vr::Ae, station_ae);
    }
    ds
}

/// MWL query constrained to one patient ID.
#[must_use]
pub fn mwl_query_for_patient(patient_id: &str) -> DataSet {
    let mut ds = mwl_base_query();
    ds.put_text(tags::PATIENT_ID, Vr::Lo, patient_id);
    ds
}

/// MWL query constrained to a start date (`YYYYMMDD`, ranges like
/// `20260101-20261231` allowed).
#[must_use]
pub fn mwl_query_for_date(date_or_range: &str) -> DataSet {
    let mut ds = mwl_base_query();
    if let Some(item) = first_sps_mut(&mut ds) {
        item.put_text(
            tags::SCHEDULED_PROCEDURE_STEP_START_DATE,
            Vr::Da,
            date_or_range,
        );
    }
    ds
}

/// MWL query constrained to an accession number.
#[must_use]
pub fn mwl_query_for_accession(accession: &str) -> DataSet {
    let mut ds = mwl_base_query();
    ds.put_text(tags::ACCESSION_NUMBER, Vr::Sh, accession);
    ds
}

fn first_sps_mut(ds: &mut DataSet) -> Option<&mut DataSet> {
    let el = ds.get_mut(tags::SCHEDULED_PROCEDURE_STEP_SEQUENCE)?;
    match &mut el.value {
        vyna_dcm_core::Value::Sequence(items) => items.first_mut(),
        _ => None,
    }
}

/// MPPS data shared by N-CREATE and N-SET.
#[derive(Clone, Debug, Default)]
pub struct MppsData {
    /// Performed Procedure Step ID (N-CREATE).
    pub performed_procedure_step_id: String,
    /// Performed Station AE title.
    pub performed_station_ae: String,
    /// Performed Station Name.
    pub performed_station_name: Option<String>,
    /// Modality of the performed step.
    pub modality: String,
    /// Start date `YYYYMMDD` / time `HHMMSS`.
    pub start_date: String,
    /// Start time.
    pub start_time: String,
    /// End date/time (N-SET complete/discontinue).
    pub end_date: Option<String>,
    /// End time.
    pub end_time: Option<String>,
    /// `IN PROGRESS`, `COMPLETED` or `DISCONTINUED`.
    pub status: String,
    /// Study Instance UID of the scheduled step.
    pub study_instance_uid: Option<String>,
    /// Accession number of the scheduled step.
    pub accession_number: Option<String>,
    /// Requested Procedure ID of the scheduled step.
    pub requested_procedure_id: Option<String>,
    /// Scheduled Procedure Step ID.
    pub scheduled_procedure_step_id: Option<String>,
    /// Performed series: `(series_instance_uid, referenced_sop_class_uid,
    /// referenced_sop_instance_uid)` triples for N-SET.
    pub performed_series: Vec<(String, String, String)>,
}

impl MppsData {
    /// N-CREATE dataset (`IN PROGRESS`, no series yet).
    #[must_use]
    pub fn to_create_dataset(&self) -> DataSet {
        let mut ds = DataSet::new();
        ds.put_text(
            tags::PERFORMED_PROCEDURE_STEP_ID,
            Vr::Sh,
            self.performed_procedure_step_id.clone(),
        );
        ds.put_text(
            tags::PERFORMED_STATION_AE_TITLE,
            Vr::Ae,
            self.performed_station_ae.clone(),
        );
        if let Some(n) = &self.performed_station_name {
            ds.put_text(tags::PERFORMED_STATION_NAME, Vr::Sh, n.clone());
        }
        ds.put_text(tags::MODALITY, Vr::Cs, self.modality.clone());
        ds.put_text(
            tags::PERFORMED_PROCEDURE_STEP_START_DATE,
            Vr::Da,
            self.start_date.clone(),
        );
        ds.put_text(
            tags::PERFORMED_PROCEDURE_STEP_START_TIME,
            Vr::Tm,
            self.start_time.clone(),
        );
        ds.put_text(
            tags::PERFORMED_PROCEDURE_STEP_STATUS,
            Vr::Cs,
            self.status.clone(),
        );

        let mut sched = DataSet::new();
        if let Some(u) = &self.study_instance_uid {
            sched.put_uid(tags::STUDY_INSTANCE_UID, u.clone());
        }
        if let Some(a) = &self.accession_number {
            sched.put_text(tags::ACCESSION_NUMBER, Vr::Sh, a.clone());
        }
        ds.put_sequence(tags::SCHEDULED_STEP_ATTRIBUTES_SEQUENCE, vec![sched]);

        let mut req_attrs = DataSet::new();
        if let Some(r) = &self.requested_procedure_id {
            req_attrs.put_text(tags::REQUESTED_PROCEDURE_ID, Vr::Sh, r.clone());
        }
        if let Some(s) = &self.scheduled_procedure_step_id {
            req_attrs.put_text(tags::SCHEDULED_PROCEDURE_STEP_ID, Vr::Sh, s.clone());
        }
        ds.put_sequence(tags::REQUEST_ATTRIBUTES_SEQUENCE, vec![req_attrs]);
        ds.put_sequence(tags::PERFORMED_SERIES_SEQUENCE, vec![]);
        ds
    }

    /// N-SET dataset (status + end date/time + performed series).
    #[must_use]
    pub fn to_set_dataset(&self) -> DataSet {
        let mut ds = DataSet::new();
        ds.put_text(
            tags::PERFORMED_PROCEDURE_STEP_STATUS,
            Vr::Cs,
            self.status.clone(),
        );
        if let Some(d) = &self.end_date {
            ds.put_text(tags::PERFORMED_PROCEDURE_STEP_END_DATE, Vr::Da, d.clone());
        }
        if let Some(t) = &self.end_time {
            ds.put_text(tags::PERFORMED_PROCEDURE_STEP_END_TIME, Vr::Tm, t.clone());
        }
        let mut series_items = Vec::new();
        for (series_uid, class_uid, inst_uid) in &self.performed_series {
            let mut item = DataSet::new();
            item.put_uid(tags::SERIES_INSTANCE_UID, series_uid.clone());
            let mut ref_img = DataSet::new();
            ref_img.put_uid(tags::REFERENCED_SOP_CLASS_UID, class_uid.clone());
            ref_img.put_uid(tags::REFERENCED_SOP_INSTANCE_UID, inst_uid.clone());
            item.put_sequence(tags::REFERENCED_IMAGE_SEQUENCE, vec![ref_img]);
            series_items.push(item);
        }
        if !series_items.is_empty() {
            ds.put_sequence(tags::PERFORMED_SERIES_SEQUENCE, series_items);
        }
        ds
    }
}

/// Build a Storage Commitment N-ACTION dataset.
///
/// `instances`: `(sop_class_uid, sop_instance_uid)` pairs the archive
/// should take custody of. Returns the dataset plus the transaction UID.
#[must_use]
pub fn storage_commitment_request(instances: &[(&str, &str)]) -> (DataSet, String) {
    let transaction_uid = vyna_dcm_core::new_uid();
    let mut ds = DataSet::new();
    ds.put_uid(tags::TRANSACTION_UID, transaction_uid.clone());
    let items = instances
        .iter()
        .map(|(c, i)| {
            let mut item = DataSet::new();
            item.put_uid(tags::REFERENCED_SOP_CLASS_UID, c.to_string());
            item.put_uid(tags::REFERENCED_SOP_INSTANCE_UID, i.to_string());
            item
        })
        .collect();
    ds.put_sequence(tags::REFERENCED_SOP_SEQUENCE, items);
    (ds, transaction_uid)
}

/// A parsed Storage Commitment N-EVENT-REPORT result:
/// `(transaction_uid, succeeded, failed)`, each entry a
/// `(sop_class_uid, sop_instance_uid)` pair.
pub type CommitmentResult = (Option<String>, Vec<(String, String)>, Vec<(String, String)>);

/// Parse a Storage Commitment N-EVENT-REPORT dataset into a [`CommitmentResult`].
#[must_use]
pub fn parse_commitment_result(ds: &DataSet) -> CommitmentResult {
    let txn = ds.string(tags::TRANSACTION_UID).map(str::to_string);
    let collect = |tag| {
        ds.sequence(tag)
            .map(|items| {
                items
                    .iter()
                    .filter_map(|it| {
                        Some((
                            it.string(tags::REFERENCED_SOP_CLASS_UID)?.to_string(),
                            it.string(tags::REFERENCED_SOP_INSTANCE_UID)?.to_string(),
                        ))
                    })
                    .collect()
            })
            .unwrap_or_default()
    };
    (
        txn,
        collect(tags::REFERENCED_SOP_SEQUENCE),
        collect(tags::FAILED_SOP_SEQUENCE),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mwl_queries_carry_keys() {
        let q = mwl_query_for_station("ECG01");
        let item = &q.sequence(tags::SCHEDULED_PROCEDURE_STEP_SEQUENCE).unwrap()[0];
        assert_eq!(item.string(tags::SCHEDULED_STATION_AE_TITLE), Some("ECG01"));
        assert!(item.contains(tags::MODALITY)); // empty return key
        assert_eq!(
            mwl_query_for_patient("P1").string(tags::PATIENT_ID),
            Some("P1")
        );
        let q = mwl_query_for_date("20260101-20261231");
        let item = &q.sequence(tags::SCHEDULED_PROCEDURE_STEP_SEQUENCE).unwrap()[0];
        assert_eq!(
            item.string(tags::SCHEDULED_PROCEDURE_STEP_START_DATE),
            Some("20260101-20261231")
        );
    }

    #[test]
    fn mpps_datasets_have_required_keys() {
        let mpps = MppsData {
            performed_procedure_step_id: "PPS1".into(),
            performed_station_ae: "ECG01".into(),
            modality: "ECG".into(),
            start_date: "20261011".into(),
            start_time: "120000".into(),
            status: "IN PROGRESS".into(),
            study_instance_uid: Some("1.2.3".into()),
            requested_procedure_id: Some("RP1".into()),
            scheduled_procedure_step_id: Some("SPS1".into()),
            ..MppsData::default()
        };
        let create = mpps.to_create_dataset();
        assert_eq!(
            create.string(tags::PERFORMED_PROCEDURE_STEP_STATUS),
            Some("IN PROGRESS")
        );
        assert!(
            create
                .sequence(tags::SCHEDULED_STEP_ATTRIBUTES_SEQUENCE)
                .is_some()
        );

        let done = MppsData {
            status: "COMPLETED".into(),
            end_date: Some("20261011".into()),
            end_time: Some("121500".into()),
            performed_series: vec![(
                "1.2.3.4".into(),
                "1.2.840.10008.5.1.4.1.1.9.1.1".into(),
                "1.2.3.4.5".into(),
            )],
            ..mpps
        };
        let set = done.to_set_dataset();
        assert_eq!(
            set.string(tags::PERFORMED_PROCEDURE_STEP_STATUS),
            Some("COMPLETED")
        );
        assert_eq!(
            set.sequence(tags::PERFORMED_SERIES_SEQUENCE).unwrap().len(),
            1
        );
    }

    #[test]
    fn commitment_round_trip() {
        let (req, txn) = storage_commitment_request(&[("1.2.3", "1.2.3.4")]);
        assert_eq!(req.string(tags::TRANSACTION_UID), Some(txn.as_str()));
        // Fake N-EVENT-REPORT: same instance committed.
        let mut evt = DataSet::new();
        evt.put_uid(tags::TRANSACTION_UID, txn.clone());
        let mut item = DataSet::new();
        item.put_uid(tags::REFERENCED_SOP_CLASS_UID, "1.2.3");
        item.put_uid(tags::REFERENCED_SOP_INSTANCE_UID, "1.2.3.4");
        evt.put_sequence(tags::REFERENCED_SOP_SEQUENCE, vec![item]);
        let (t, ok, failed) = parse_commitment_result(&evt);
        assert_eq!(t, Some(txn));
        assert_eq!(ok.len(), 1);
        assert!(failed.is_empty());
    }
}
