//! The de-identification engine: [`Deidentifier`] plus [`DeidReport`].

use std::collections::HashMap;

use vyna_dcm_core::{DataSet, PersonName, Tag, Value, Vr, new_uid, tags};

use crate::dates::{shift_da, shift_dt};
use crate::error::{Error, Result};
use crate::profile::{Action, BASIC_PROFILE};

/// Default replacement patient ID / name.
pub const DEFAULT_REPLACEMENT: &str = "ANONYMIZED";

/// One de-identification run over a dataset.
#[derive(Debug)]
pub struct Deidentifier {
    profile: HashMap<Tag, Action>,
    replacement_id: String,
    replacement_name: PersonName,
    date_offset: Option<i64>,
    retain_private: bool,
    uid_map: HashMap<String, String>,
}

impl Default for Deidentifier {
    fn default() -> Self {
        Self::new()
    }
}

impl Deidentifier {
    /// Basic profile, `ANONYMIZED` dummies, dates zeroed, private removed.
    #[must_use]
    pub fn new() -> Self {
        Self {
            profile: BASIC_PROFILE.iter().copied().collect(),
            replacement_id: DEFAULT_REPLACEMENT.to_string(),
            replacement_name: PersonName::parse(DEFAULT_REPLACEMENT),
            date_offset: None,
            retain_private: false,
            uid_map: HashMap::new(),
        }
    }

    /// Use a custom `(tag, action)` table instead of [`BASIC_PROFILE`].
    #[must_use]
    pub fn with_profile(mut self, profile: &'static [(Tag, Action)]) -> Self {
        self.profile = profile.iter().copied().collect();
        self
    }

    /// Replacement value for [`Action::Dummy`] text attributes (Patient ID).
    #[must_use]
    pub fn with_replacement_id(mut self, id: impl Into<String>) -> Self {
        self.replacement_id = id.into();
        self
    }

    /// Replacement value for [`Action::Dummy`] person names.
    #[must_use]
    pub fn with_replacement_name(mut self, name: &str) -> Self {
        self.replacement_name = PersonName::parse(name);
        self
    }

    /// Shift `DA`/`DT` values by `days` (negative shifts back); without
    /// an offset, shiftable dates are replaced with a fixed dummy
    /// instead (longitudinal intervals are lost but Type 1 dates stay
    /// valid). One offset per `Deidentifier` keeps intervals across
    /// every dataset it processes.
    #[must_use]
    pub fn with_date_offset(mut self, days: i64) -> Self {
        self.date_offset = Some(days);
        self
    }

    /// Keep private data elements (default: removed).
    #[must_use]
    pub fn with_retain_private(mut self, retain: bool) -> Self {
        self.retain_private = retain;
        self
    }

    /// De-identify `ds` in place, returning what changed.
    ///
    /// Refuses with [`Error::BurnedInAnnotation`] when Burned In
    /// Annotation is `YES` (pixels are never altered). UID remaps stay
    /// consistent across every `apply` call on this `Deidentifier`.
    ///
    /// Only the dataset is touched: callers that persist file meta
    /// must rewrite it through [`uid_map`](Self::uid_map), or the
    /// original UIDs leak into the output meta header.
    pub fn apply(&mut self, ds: &mut DataSet) -> Result<DeidReport> {
        // Trim NUL padding as well as whitespace ("YES\0" from dirty
        // writers must refuse, not slip through), and compare
        // case-insensitively: this is a safety gate, not a parser.
        let burned_in = ds.string(tags::BURNED_IN_ANNOTATION).is_some_and(|s| {
            s.trim_matches(|c: char| c.is_whitespace() || c == '\0')
                .eq_ignore_ascii_case("YES")
        });
        if burned_in {
            return Err(Error::BurnedInAnnotation);
        }
        let mut report = DeidReport::default();
        self.apply_to(ds, &mut report);
        if report.dates_touched > 0 {
            let flag = if self.date_offset.is_some() {
                "MODIFIED"
            } else {
                "REMOVED"
            };
            ds.put_text(
                tags::LONGITUDINAL_TEMPORAL_INFORMATION_MODIFIED,
                Vr::Cs,
                flag,
            );
        }
        // PS3.15 E.1 conformance markers (the profile strips any
        // pre-existing values; the de-identified instance declares
        // fresh ones).
        ds.put_text(tags::PATIENT_IDENTITY_REMOVED, Vr::Cs, "YES");
        let mut method = DataSet::new();
        method.put_text(tags::CODE_VALUE, Vr::Sh, "113100");
        method.put_text(tags::CODING_SCHEME_DESIGNATOR, Vr::Sh, "DCM");
        method.put_text(
            tags::CODE_MEANING,
            Vr::Lo,
            "Basic Application Confidentiality Profile",
        );
        ds.put_sequence(tags::DEIDENTIFICATION_METHOD_CODE_SEQUENCE, vec![method]);
        // The gate above passed, so no burned-in PHI remains: declare
        // NO instead of leaving the marker empty.
        if ds.get(tags::BURNED_IN_ANNOTATION).is_some() {
            ds.put_text(tags::BURNED_IN_ANNOTATION, Vr::Cs, "NO");
        }
        Ok(report)
    }

    /// The consistent original→replacement UID map built by [`apply`](Self::apply).
    #[must_use]
    pub fn uid_map(&self) -> &HashMap<String, String> {
        &self.uid_map
    }

    fn apply_to(&mut self, ds: &mut DataSet, report: &mut DeidReport) {
        let order: Vec<Tag> = ds.iter().map(|(tag, _)| tag).collect();
        for tag in order {
            if is_overlay(tag) {
                ds.remove(tag);
                report.removed += 1;
                continue;
            }
            if tag.is_private() && !self.retain_private {
                ds.remove(tag);
                report.removed += 1;
                continue;
            }
            match self.profile.get(&tag).copied() {
                Some(Action::Remove) => {
                    ds.remove(tag);
                    report.removed += 1;
                }
                Some(Action::Zero) => {
                    if let Some(el) = ds.get_mut(tag) {
                        el.value = Value::Empty;
                        report.zeroed += 1;
                    }
                }
                Some(Action::Dummy) => self.apply_dummy(ds, tag, report),
                Some(Action::RemapUid) => self.apply_remap(ds, tag, report),
                Some(Action::ShiftDate) => self.apply_shift(ds, tag, report),
                None => {}
            }
            if let Some(el) = ds.get_mut(tag)
                && let Value::Sequence(items) = &mut el.value
            {
                for item in items.iter_mut() {
                    self.apply_to(item, report);
                }
            }
        }
    }

    fn apply_dummy(&mut self, ds: &mut DataSet, tag: Tag, report: &mut DeidReport) {
        let Some(vr) = ds.get_mut(tag).map(|el| el.vr) else {
            return;
        };
        if vr == Vr::Pn {
            ds.put_person_name(tag, vec![self.replacement_name.clone()]);
        } else {
            ds.put_text(tag, vr, self.replacement_id.clone());
        }
        report.dummied += 1;
    }

    fn apply_remap(&mut self, ds: &mut DataSet, tag: Tag, report: &mut DeidReport) {
        let Some(el) = ds.get_mut(tag) else {
            return;
        };
        let Value::Text(values) = &mut el.value else {
            return;
        };
        for value in values.iter_mut().filter(|s| !s.is_empty()) {
            let mapped = self
                .uid_map
                .entry(value.clone())
                .or_insert_with(new_uid)
                .clone();
            if mapped != *value {
                *value = mapped;
                report.uids_remapped += 1;
            }
        }
    }

    fn apply_shift(&mut self, ds: &mut DataSet, tag: Tag, report: &mut DeidReport) {
        let Some(el) = ds.get_mut(tag) else {
            return;
        };
        let Value::Text(values) = &mut el.value else {
            return;
        };
        let is_dt = el.vr == Vr::Dt;
        report.dates_touched += 1;
        let Some(offset) = self.date_offset else {
            let dummy = if is_dt {
                crate::profile::DUMMY_DATETIME
            } else {
                crate::profile::DUMMY_DATE
            };
            for value in values.iter_mut() {
                *value = dummy.to_string();
            }
            report.dummied += 1;
            return;
        };
        for value in values.iter_mut() {
            let shifted = if is_dt {
                shift_dt(value, offset)
            } else {
                shift_da(value, offset)
            };
            match shifted {
                Some(shifted) => {
                    *value = shifted;
                    report.dates_shifted += 1;
                }
                None => report
                    .warnings
                    .push(format!("unshiftable date value kept: {tag:?} = {value:?}")),
            }
        }
    }
}

/// What one [`Deidentifier::apply`] call changed.
#[derive(Clone, Debug, Default)]
pub struct DeidReport {
    /// Elements removed ([`Action::Remove`], private, overlays).
    pub removed: usize,
    /// Elements emptied ([`Action::Zero`], incl. zeroed dates).
    pub zeroed: usize,
    /// Elements replaced with dummies ([`Action::Dummy`]).
    pub dummied: usize,
    /// UID components remapped ([`Action::RemapUid`]).
    pub uids_remapped: usize,
    /// Date components shifted ([`Action::ShiftDate`] with an offset).
    pub dates_shifted: usize,
    /// Shiftable-date elements seen (shifted or zeroed).
    pub dates_touched: usize,
    /// Non-fatal problems (values kept as-is).
    pub warnings: Vec<String>,
}

/// Overlay plane groups (`0x6000..=0x60FF`).
fn is_overlay(tag: Tag) -> bool {
    (0x6000..=0x60FF).contains(&tag.group())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> DataSet {
        let mut ds = DataSet::new();
        ds.put_person_name(tags::PATIENT_NAME, vec![PersonName::parse("Doe^Jane")]);
        ds.put_text(tags::PATIENT_ID, Vr::Lo, "E123");
        ds.put_text(tags::STUDY_DATE, Vr::Da, "20240115");
        ds.put_uid(tags::STUDY_INSTANCE_UID, "1.2.3");
        ds.put_text(tags::INSTITUTION_NAME, Vr::Lo, "City Hospital");
        ds.put_text(tags::MODALITY, Vr::Cs, "ECG");
        ds.put_text(Tag::new(0x0009, 0x0010), Vr::Lo, "PRIVATE CREATOR");
        ds.put_text(Tag::new(0x0009, 0x1001), Vr::Lo, "secret");
        ds.put_bytes(Tag::new(0x6000, 0x3000), Vr::Ow, vec![1, 2]);
        let mut item = DataSet::new();
        item.put_text(tags::REQUESTED_PROCEDURE_ID, Vr::Sh, "RP1");
        item.put_uid(tags::REFERENCED_SOP_INSTANCE_UID, "1.2.3");
        ds.put_sequence(tags::REQUEST_ATTRIBUTES_SEQUENCE, vec![item]);
        ds
    }

    #[test]
    fn every_action_fires() {
        let mut ds = fixture();
        let report = Deidentifier::new().apply(&mut ds).unwrap();
        assert_eq!(
            ds.person_names(tags::PATIENT_NAME).unwrap()[0].to_dicom_string(),
            "ANONYMIZED"
        );
        assert_eq!(ds.string(tags::PATIENT_ID), Some("ANONYMIZED"));
        assert_eq!(ds.string(tags::STUDY_DATE), Some("19000101")); // dummy date
        let uid = ds.string(tags::STUDY_INSTANCE_UID).unwrap();
        assert_ne!(uid, "1.2.3");
        assert!(vyna_dcm_core::is_valid_uid(uid));
        assert!(!ds.contains(tags::INSTITUTION_NAME));
        assert_eq!(ds.string(tags::MODALITY), Some("ECG")); // kept
        assert!(!ds.contains(Tag::new(0x0009, 0x0010)));
        assert!(!ds.contains(Tag::new(0x0009, 0x1001)));
        assert!(!ds.contains(Tag::new(0x6000, 0x3000)));
        // Sequence recursion: same input UID maps to the same fresh UID.
        let item = &ds.sequence(tags::REQUEST_ATTRIBUTES_SEQUENCE).unwrap()[0];
        assert_eq!(item.string(tags::REQUESTED_PROCEDURE_ID), None);
        assert_eq!(item.string(tags::REFERENCED_SOP_INSTANCE_UID), Some(uid));
        assert_eq!(
            (
                report.removed,
                report.zeroed,
                report.dummied,
                report.uids_remapped
            ),
            (4, 1, 3, 2)
        );
        assert_eq!(
            ds.string(tags::LONGITUDINAL_TEMPORAL_INFORMATION_MODIFIED),
            Some("REMOVED")
        );
    }

    #[test]
    fn shift_keeps_longitudinal_intervals() {
        let mut ds = DataSet::new();
        ds.put_text(tags::STUDY_DATE, Vr::Da, "20240101");
        ds.put_text(tags::SERIES_DATE, Vr::Da, "20240115");
        let report = Deidentifier::new()
            .with_date_offset(30)
            .apply(&mut ds)
            .unwrap();
        assert_eq!(ds.string(tags::STUDY_DATE), Some("20240131"));
        assert_eq!(ds.string(tags::SERIES_DATE), Some("20240214"));
        assert_eq!(report.dates_shifted, 2);
        assert_eq!(
            ds.string(tags::LONGITUDINAL_TEMPORAL_INFORMATION_MODIFIED),
            Some("MODIFIED")
        );
    }

    #[test]
    fn datetimes_shift_or_dummy_by_vr() {
        let mut ds = DataSet::new();
        ds.put_text(tags::ACQUISITION_DATE_TIME, Vr::Dt, "20240115120000");
        Deidentifier::new()
            .with_date_offset(30)
            .apply(&mut ds)
            .unwrap();
        assert_eq!(
            ds.string(tags::ACQUISITION_DATE_TIME),
            Some("20240214120000")
        );
        let mut ds = DataSet::new();
        ds.put_text(tags::ACQUISITION_DATE_TIME, Vr::Dt, "20240115120000");
        Deidentifier::new().apply(&mut ds).unwrap();
        assert_eq!(
            ds.string(tags::ACQUISITION_DATE_TIME),
            Some("19000101000000")
        );
    }

    #[test]
    fn invalid_date_warns_and_keeps() {
        let mut ds = DataSet::new();
        ds.put_text(tags::STUDY_DATE, Vr::Da, "baddate");
        let report = Deidentifier::new()
            .with_date_offset(7)
            .apply(&mut ds)
            .unwrap();
        assert_eq!(ds.string(tags::STUDY_DATE), Some("baddate"));
        assert_eq!(report.warnings.len(), 1);
    }

    #[test]
    fn burned_in_annotation_refuses_before_mutation() {
        let mut ds = DataSet::new();
        ds.put_text(tags::BURNED_IN_ANNOTATION, Vr::Cs, "YES");
        ds.put_text(tags::PATIENT_ID, Vr::Lo, "E123");
        let err = Deidentifier::new().apply(&mut ds).unwrap_err();
        assert!(matches!(err, Error::BurnedInAnnotation));
        assert_eq!(ds.string(tags::PATIENT_ID), Some("E123"));
    }

    #[test]
    fn burned_in_gate_ignores_padding_and_case() {
        // NUL-padded or lowercase YES from dirty writers must refuse,
        // not slip through and get the marker zeroed.
        for yes in ["YES\0", "YES ", " yes", "Yes", "yes"] {
            let mut ds = DataSet::new();
            ds.put_text(tags::BURNED_IN_ANNOTATION, Vr::Cs, yes);
            let err = Deidentifier::new().apply(&mut ds).unwrap_err();
            assert!(matches!(err, Error::BurnedInAnnotation), "{yes:?}");
        }
        // NO passes and is restamped (never emptied).
        let mut ds = DataSet::new();
        ds.put_text(tags::BURNED_IN_ANNOTATION, Vr::Cs, "NO");
        Deidentifier::new().apply(&mut ds).unwrap();
        assert_eq!(ds.string(tags::BURNED_IN_ANNOTATION), Some("NO"));
    }

    #[test]
    fn conformance_markers_are_declared() {
        let mut ds = DataSet::new();
        ds.put_text(tags::PATIENT_IDENTITY_REMOVED, Vr::Cs, "NO");
        Deidentifier::new().apply(&mut ds).unwrap();
        assert_eq!(ds.string(tags::PATIENT_IDENTITY_REMOVED), Some("YES"));
        let seq = ds
            .sequence(tags::DEIDENTIFICATION_METHOD_CODE_SEQUENCE)
            .unwrap();
        assert_eq!(seq.len(), 1);
        assert_eq!(seq[0].string(tags::CODE_VALUE), Some("113100"));
        assert_eq!(seq[0].string(tags::CODING_SCHEME_DESIGNATOR), Some("DCM"));
    }

    #[test]
    fn uid_map_is_visible_to_callers() {
        let mut deid = Deidentifier::new();
        let mut ds = DataSet::new();
        ds.put_uid(tags::STUDY_INSTANCE_UID, "1.2.3");
        deid.apply(&mut ds).unwrap();
        let mapped = deid.uid_map().get("1.2.3").unwrap().clone();
        assert_eq!(ds.string(tags::STUDY_INSTANCE_UID), Some(mapped.as_str()));
        assert_ne!(mapped, "1.2.3");
    }

    #[test]
    fn private_tags_survive_when_retained() {
        let mut ds = DataSet::new();
        ds.put_text(Tag::new(0x0009, 0x1001), Vr::Lo, "secret");
        let report = Deidentifier::new()
            .with_retain_private(true)
            .apply(&mut ds)
            .unwrap();
        assert_eq!(ds.string(Tag::new(0x0009, 0x1001)), Some("secret"));
        assert_eq!(report.removed, 0);
    }

    #[test]
    fn custom_dummies_apply() {
        let mut ds = DataSet::new();
        ds.put_person_name(tags::PATIENT_NAME, vec![PersonName::parse("A^B")]);
        ds.put_text(tags::PATIENT_ID, Vr::Lo, "E123");
        Deidentifier::new()
            .with_replacement_id("SUBJ-7")
            .with_replacement_name("Subject^Seven")
            .apply(&mut ds)
            .unwrap();
        assert_eq!(ds.string(tags::PATIENT_ID), Some("SUBJ-7"));
        assert_eq!(
            ds.person_names(tags::PATIENT_NAME).unwrap()[0].to_dicom_string(),
            "Subject^Seven"
        );
    }

    #[test]
    fn uid_maps_stay_consistent_across_datasets() {
        let mut deid = Deidentifier::new();
        let mut a = DataSet::new();
        a.put_uid(tags::STUDY_INSTANCE_UID, "1.2.3");
        let mut b = DataSet::new();
        b.put_uid(tags::STUDY_INSTANCE_UID, "1.2.3");
        b.put_uid(tags::SERIES_INSTANCE_UID, "1.2.4");
        deid.apply(&mut a).unwrap();
        deid.apply(&mut b).unwrap();
        assert_eq!(
            a.string(tags::STUDY_INSTANCE_UID),
            b.string(tags::STUDY_INSTANCE_UID)
        );
        assert_ne!(
            b.string(tags::STUDY_INSTANCE_UID),
            b.string(tags::SERIES_INSTANCE_UID)
        );
    }
}
