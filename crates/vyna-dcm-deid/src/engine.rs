//! The de-identification engine: [`Deidentifier`] plus [`DeidReport`].

use std::collections::HashMap;

use vyna_dcm_core::{DataElement, DataSet, PersonName, Tag, Value, Vr, new_uid, tags};

use crate::dates::{shift_da, shift_dt};
use crate::error::{Error, Result};
use crate::profile::{Action, BASIC_PROFILE};

/// Default replacement patient ID / name.
pub const DEFAULT_REPLACEMENT: &str = "ANONYMIZED";

/// One de-identification run over a dataset.
pub struct Deidentifier {
    profile: HashMap<Tag, Action>,
    replacement_id: String,
    replacement_name: PersonName,
    date_offset: Option<i64>,
    retain_private: bool,
    custom_profile: bool,
    allow_burned_in: bool,
    uid_map: HashMap<String, String>,
}

// Manual `Debug`: `uid_map` keys are ORIGINAL UIDs (linkable PHI)
// and must never reach logs; only the map size shows.
impl std::fmt::Debug for Deidentifier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Deidentifier")
            .field("replacement_id", &self.replacement_id)
            .field("replacement_name", &self.replacement_name)
            .field("date_offset", &self.date_offset)
            .field("retain_private", &self.retain_private)
            .field("custom_profile", &self.custom_profile)
            .field("allow_burned_in", &self.allow_burned_in)
            .field("uid_map_len", &self.uid_map.len())
            .finish_non_exhaustive()
    }
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
            custom_profile: false,
            allow_burned_in: false,
            uid_map: HashMap::new(),
        }
    }

    /// Use a custom `(tag, action)` table instead of [`BASIC_PROFILE`].
    ///
    /// Custom profiles withhold the `YES` / `113100` conformance markers
    /// (unknown coverage must not claim the Basic Profile); stamp your
    /// own `Deidentification Method Code Sequence` when yours holds.
    #[must_use]
    pub fn with_profile(mut self, profile: &[(Tag, Action)]) -> Self {
        self.profile = profile.iter().copied().collect();
        self.custom_profile = true;
        self
    }

    /// Replacement value for [`Action::Dummy`] text attributes.
    ///
    /// Fails instead of truncating: over 16 bytes (the value flows
    /// into `SH`/`AE`/`CS` rows capped at 16 characters), backslash
    /// (the encoder joins text components with `\`, so an embedded
    /// one decodes back as two values — silent VM corruption), or
    /// control characters (not valid in text VRs) are rejected.
    pub fn with_replacement_id(mut self, id: impl Into<String>) -> Result<Self> {
        let id = id.into();
        if id.len() > 16 {
            return Err(Error::InvalidReplacementId(format!(
                "exceeds the 16-byte SH/AE/CS limit: {id:?}"
            )));
        }
        if id.contains('\\') {
            return Err(Error::InvalidReplacementId(format!(
                "backslash would corrupt multi-valued elements: {id:?}"
            )));
        }
        if id.chars().any(|c| c.is_control()) {
            return Err(Error::InvalidReplacementId(format!(
                "control characters are not valid in text VRs: {id:?}"
            )));
        }
        self.replacement_id = id;
        Ok(self)
    }

    /// Replacement value for [`Action::Dummy`] person names.
    ///
    /// # Panics
    ///
    /// Panics when any `=`-separated component group exceeds 64 bytes
    /// (the `PN` per-group limit).
    /// Replacement person name stamped into `Dummy`-action `PN`
    /// elements. Fails instead of truncating: overlong groups and
    /// over-count components/groups are rejected.
    pub fn with_replacement_name(mut self, name: &str) -> Result<Self> {
        for group in name.split('=') {
            if group.len() > 64 {
                return Err(Error::InvalidReplacementName(format!(
                    "group exceeds the 64-byte PN limit: {group:?}"
                )));
            }
        }
        self.replacement_name = PersonName::try_parse(name)
            .map_err(|e| Error::InvalidReplacementName(e.to_string()))?;
        Ok(self)
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

    /// Proceed when Burned In Annotation is `YES` instead of refusing
    /// (default: refuse).
    ///
    /// The engine still never alters pixels: set this only when the
    /// caller redacts them separately first (e.g.
    /// `vyna_dcm_codec::redact_regions`). Overridden runs record a
    /// warning, which withholds the `YES` / `113100` conformance
    /// markers — an override the engine cannot verify must not claim
    /// them.
    #[must_use]
    pub fn with_allow_burned_in(mut self, allow: bool) -> Self {
        self.allow_burned_in = allow;
        self
    }

    /// De-identify `ds` in place, returning what changed.
    ///
    /// Refuses with [`Error::BurnedInAnnotation`] when Burned In
    /// Annotation is `YES` (pixels are never altered), unless
    /// [`with_allow_burned_in`](Self::with_allow_burned_in) was set —
    /// in which case the run proceeds with a recorded warning (and
    /// withheld conformance markers). UID remaps stay
    /// consistent across every `apply` call on this `Deidentifier`.
    ///
    /// Only the dataset is touched: callers that persist file meta
    /// must remap Media Storage SOP Instance UID through
    /// [`uid_map`](Self::uid_map), replace Implementation Class UID
    /// and Version Name with their own, and clear the file-meta AE
    /// titles — or the original UIDs, the source writer's identity,
    /// and the source site's network endpoints leak into the output
    /// meta header (see the `deidentify` CLI for the full pattern).
    ///
    /// The `YES` / `113100` conformance markers are stamped only for a
    /// clean Basic-profile run: a custom profile, retained privates,
    /// or any kept-value warning withholds them (see the report).
    pub fn apply(&mut self, ds: &mut DataSet) -> Result<DeidReport> {
        let mut report = DeidReport::default();
        match burned_in_status(ds) {
            BurnedIn::Yes if !self.allow_burned_in => return Err(Error::BurnedInAnnotation),
            BurnedIn::Yes => report.warnings.push(
                "Burned In Annotation is YES: caller attested external pixel redaction".to_string(),
            ),
            BurnedIn::Unknown => report
                .warnings
                .push("Burned In Annotation present but unparseable: kept as-is".to_string()),
            BurnedIn::Clean => {}
        }
        self.apply_to(ds, &mut report);
        if report.dates_touched > 0 || report.dates_lost > 0 {
            // Shifted dates keep intervals (MODIFIED) — but only when
            // no date lost its values: a run that both shifts and
            // empties dates must not claim maintained relationships.
            // Untouched-by-loss kept dates are originals (UNMODIFIED).
            let flag = if report.dates_lost > 0 {
                "REMOVED"
            } else if report.dates_shifted > 0 {
                "MODIFIED"
            } else {
                "UNMODIFIED"
            };
            ds.put_text(
                tags::LONGITUDINAL_TEMPORAL_INFORMATION_MODIFIED,
                Vr::Cs,
                flag,
            );
        }
        // PS3.15 E.1 conformance markers (the profile strips any
        // pre-existing values; the de-identified instance declares
        // fresh ones) — unless this run cannot honestly claim them.
        let markers_safe =
            !self.custom_profile && !self.retain_private && report.warnings.is_empty();
        if markers_safe {
            ds.put_text(tags::PATIENT_IDENTITY_REMOVED, Vr::Cs, "YES");
            let mut basic = DataSet::new();
            basic.put_text(tags::CODE_VALUE, Vr::Sh, "113100");
            basic.put_text(tags::CODING_SCHEME_DESIGNATOR, Vr::Sh, "DCM");
            basic.put_text(
                tags::CODE_MEANING,
                Vr::Lo,
                "Basic Application Confidentiality Profile",
            );
            let mut methods = vec![basic];
            if report.dates_shifted > 0 {
                let mut modified = DataSet::new();
                modified.put_text(tags::CODE_VALUE, Vr::Sh, "113105");
                modified.put_text(tags::CODING_SCHEME_DESIGNATOR, Vr::Sh, "DCM");
                modified.put_text(
                    tags::CODE_MEANING,
                    Vr::Lo,
                    "Retain Longitudinal Temporal Information With Modified Dates Option",
                );
                methods.push(modified);
            }
            ds.put_sequence(tags::DEIDENTIFICATION_METHOD_CODE_SEQUENCE, methods);
        } else {
            let mut why = Vec::new();
            if self.custom_profile {
                why.push("custom profile");
            }
            if self.retain_private {
                why.push("retained private elements");
            }
            if !report.warnings.is_empty() {
                why.push("kept values (see warnings)");
            }
            report
                .warnings
                .push(format!("conformance markers withheld: {}", why.join(", ")));
        }
        // A proven top-level NO is re-declared instead of left as a
        // dirty-writer spelling; anything else (YES refused above,
        // unparseable warned above) is kept as-is.
        let top = ds.get(tags::BURNED_IN_ANNOTATION);
        if top.is_some() && matches!(burned_in_element(top), BurnedIn::Clean) {
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
            if is_overlay(tag) || is_curve_data(tag) {
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
                    if is_longitudinal_date(ds, tag) {
                        report.dates_lost += 1;
                    }
                    ds.remove(tag);
                    report.removed += 1;
                }
                Some(Action::Zero) => {
                    if let Some(el) = ds.get_mut(tag) {
                        if matches!(el.vr, Vr::Da | Vr::Dt) {
                            report.dates_lost += 1;
                        }
                        el.value = Value::Empty;
                        report.zeroed += 1;
                    }
                }
                Some(Action::Dummy) => self.apply_dummy(ds, tag, report),
                Some(Action::RemapUid) => self.apply_remap(ds, tag, report),
                Some(Action::ShiftDate) => self.apply_shift(ds, tag, report, false),
                Some(Action::ShiftDateOrZero) => self.apply_shift(ds, tag, report, true),
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
        let Some(vr) = ds.get(tag).map(|el| el.vr) else {
            return;
        };
        // Every branch writes a VR-valid, non-empty value: dummies must
        // stay encodable and Type 1-safe. `UI` reuses the consistent
        // remap (a fixed dummy UID would collide across instances).
        match vr {
            Vr::Pn => ds.put_person_name(tag, vec![self.replacement_name.clone()]),
            Vr::Tm => ds.put_text(tag, vr, crate::profile::DUMMY_TIME),
            Vr::As => ds.put_text(tag, vr, crate::profile::DUMMY_AGE),
            Vr::Da => {
                ds.put_text(tag, vr, crate::profile::DUMMY_DATE);
                report.dates_lost += 1;
            }
            Vr::Dt => {
                ds.put_text(tag, vr, crate::profile::DUMMY_DATETIME);
                report.dates_lost += 1;
            }
            Vr::Sq => ds.put_sequence(tag, vec![DataSet::new()]),
            Vr::Ui => {
                self.apply_remap(ds, tag, report);
                return;
            }
            Vr::Ob | Vr::Ow | Vr::Of | Vr::Od | Vr::Ol | Vr::Ov | Vr::Un => {
                // `OF`/`OD`/`OL`/`OV` need 4/8-byte multiples (the
                // encoder's even-padding does not guarantee that):
                // pad with zeros, which decode as valid 0/0.0 values.
                let unit = match vr {
                    Vr::Of | Vr::Ol => 4,
                    Vr::Od | Vr::Ov => 8,
                    Vr::Ow => 2,
                    _ => 1,
                };
                let mut bytes = self.replacement_id.clone().into_bytes();
                while !bytes.len().is_multiple_of(unit) {
                    bytes.push(0);
                }
                ds.put_bytes(tag, vr, bytes);
            }
            // Plain-text VRs take the replacement id verbatim.
            Vr::Ae | Vr::Cs | Vr::Sh | Vr::Lo | Vr::St | Vr::Lt | Vr::Ut | Vr::Uc | Vr::Ur => {
                ds.put_text(tag, vr, self.replacement_id.clone());
            }
            // Numeric-string VRs need a numeric dummy: the replacement
            // id ("ANONYMIZED") is not a valid DS/IS value.
            Vr::Ds | Vr::Is => {
                ds.put_text(tag, vr, "0");
            }
            // Binary numerics take a zero of their own width. Writing
            // text under them would emit unencodable DICOM (reachable
            // via mistyped elements in dirty files or custom profiles
            // pointing Dummy at numeric tags).
            Vr::Us => ds.put_u16_single(tag, 0),
            Vr::Ss => ds.insert(DataElement::new(tag, vr, Value::I16(vec![0]))),
            Vr::Ul => ds.put_u32_single(tag, 0),
            Vr::Sl => ds.insert(DataElement::new(tag, vr, Value::I32(vec![0]))),
            Vr::Uv => ds.insert(DataElement::new(tag, vr, Value::U64(vec![0]))),
            Vr::Sv => ds.insert(DataElement::new(tag, vr, Value::I64(vec![0]))),
            Vr::Fl => ds.insert(DataElement::new(tag, vr, Value::F32(vec![0.0]))),
            Vr::Fd => ds.insert(DataElement::new(tag, vr, Value::F64(vec![0.0]))),
            Vr::At => ds.insert(DataElement::tags(tag, vec![])),
            // Retired SI: unknown wire width, so keep + warn rather
            // than emit bytes that misdeclare the value. The warning
            // withholds the conformance markers.
            Vr::Si => {
                report
                    .warnings
                    .push(format!("SI dummy kept (retired VR): {tag:?}"));
                return;
            }
            // Future VRs: same keep-and-warn policy — never emit
            // bytes that misdeclare the value.
            _ => {
                report
                    .warnings
                    .push(format!("dummy kept (unsupported VR {vr:?}): {tag:?}"));
                return;
            }
        }
        report.dummied += 1;
    }

    fn apply_remap(&mut self, ds: &mut DataSet, tag: Tag, report: &mut DeidReport) {
        let Some(el) = ds.get_mut(tag) else {
            return;
        };
        // Zero-length elements hold no PHI: leave them silently (a
        // warning here would withhold the Basic Profile markers over
        // nothing — empty Type 2 values are common in real files).
        if el.value.is_empty() {
            return;
        }
        let Value::Text(values) = &mut el.value else {
            report
                .warnings
                .push(format!("UID element kept (not text): {tag:?}"));
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

    fn apply_shift(
        &mut self,
        ds: &mut DataSet,
        tag: Tag,
        report: &mut DeidReport,
        zero_when_no_offset: bool,
    ) {
        let Some(el) = ds.get_mut(tag) else {
            return;
        };
        // Zero-length elements hold no PHI: leave them silently (see
        // `apply_remap` — a warning would withhold markers over an
        // empty Type 2 date).
        if el.value.is_empty() {
            return;
        }
        let vr = el.vr;
        let Value::Text(values) = &mut el.value else {
            report
                .warnings
                .push(format!("date element kept (not text): {tag:?}"));
            return;
        };
        // A custom profile can point a shift action at any VR, but a
        // date dummy/shift is only meaningful for DA/DT: writing
        // "19000101" into a TM (or UI/PN/numeric) element emits
        // VR-invalid output, so warn-and-keep instead (both branches).
        if !matches!(vr, Vr::Da | Vr::Dt) {
            report
                .warnings
                .push(format!("shift action kept (not a date VR {vr:?}): {tag:?}"));
            return;
        }
        let is_dt = vr == Vr::Dt;
        report.dates_touched += 1;
        let Some(offset) = self.date_offset else {
            // No offset: the shift degrades to empty/dummy, which
            // destroys intervals.
            if zero_when_no_offset {
                el.value = Value::Empty;
                report.zeroed += 1;
            } else {
                let dummy = if is_dt {
                    crate::profile::DUMMY_DATETIME
                } else {
                    crate::profile::DUMMY_DATE
                };
                // Empty components hold no PHI: skip them like the
                // with-offset branch does.
                for value in values.iter_mut().filter(|s| !s.is_empty()) {
                    *value = dummy.to_string();
                }
                report.dummied += 1;
            }
            report.dates_lost += 1;
            return;
        };
        // Empty components hold no PHI: skip them like apply_remap
        // does instead of warning (a warning would withhold the
        // Basic Profile markers over nothing). Enumerate before
        // filtering so the VM position stays truthful.
        for (i, value) in values.iter_mut().enumerate().filter(|(_, s)| !s.is_empty()) {
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
                // Tag + 1-based VM position only: the kept VALUE is a
                // quasi-identifier and warnings are typically logged.
                None => report.warnings.push(format!(
                    "unshiftable date value kept: {tag:?} value {}",
                    i + 1
                )),
            }
        }
    }
}

/// What one [`Deidentifier::apply`] call changed.
#[derive(Clone, Debug, Default)]
pub struct DeidReport {
    /// Elements removed: [`Action::Remove`] rows, dropped privates,
    /// overlay planes, and curve-data groups.
    pub removed: usize,
    /// Elements emptied but kept: [`Action::Zero`] rows plus
    /// [`Action::ShiftDateOrZero`] dates with no offset configured.
    pub zeroed: usize,
    /// Elements replaced with dummy values: [`Action::Dummy`] rows
    /// plus [`Action::ShiftDate`] dates with no offset configured.
    pub dummied: usize,
    /// UID components remapped ([`Action::RemapUid`]).
    pub uids_remapped: usize,
    /// Date components shifted ([`Action::ShiftDate`] /
    /// [`Action::ShiftDateOrZero`] with an offset set).
    pub dates_shifted: usize,
    /// Date (`DA`/`DT`) elements with text values seen by either
    /// shift action — shifted, dummied, zeroed, warned-and-kept, or
    /// skipped-empty-component alike (per-value outcomes land in
    /// `dates_shifted`/`dates_lost`, kept values warn). Wholly empty
    /// elements return before counting (they hold no PHI).
    pub dates_touched: usize,
    /// Date (`DA`/`DT`) elements whose values were emptied, dummied,
    /// or removed, destroying longitudinal intervals (`TM` excluded:
    /// times are not longitudinal). Feeds the
    /// `LONGITUDINAL_TEMPORAL_INFORMATION_MODIFIED` flag.
    pub dates_lost: usize,
    /// Non-fatal problems, always values kept as-is (any entry
    /// withholds the conformance markers).
    pub warnings: Vec<String>,
}

/// Overlay plane groups (`0x6000..=0x60FF`).
fn is_overlay(tag: Tag) -> bool {
    (0x6000..=0x60FF).contains(&tag.group())
}

/// Elements whose values carry longitudinal temporal information
/// (`DA`/`DT`; `TM` times are not longitudinal).
fn is_longitudinal_date(ds: &DataSet, tag: Tag) -> bool {
    ds.get(tag)
        .is_some_and(|el| matches!(el.vr, Vr::Da | Vr::Dt))
}

/// Curve-data groups: even groups `0x5000..=0x50FE` (E.1 `X`).
fn is_curve_data(tag: Tag) -> bool {
    let group = tag.group();
    (0x5000..0x5100).contains(&group) && group.is_multiple_of(2)
}

/// Burned-in-annotation verdict for one dataset (top level and
/// sequence items alike: the tag is top-level-only in practice, but
/// a nested `YES` must refuse too).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BurnedIn {
    /// Absent, or every value proven `NO`.
    Clean,
    /// Present but neither proven `YES` nor proven `NO` (empty,
    /// mistyped, or an unrecognized spelling): kept, warned, and the
    /// conformance markers are withheld.
    Unknown,
    /// Any value proven `YES`: the run refuses.
    Yes,
}

fn burned_in_status(ds: &DataSet) -> BurnedIn {
    let mut status = burned_in_element(ds.get(tags::BURNED_IN_ANNOTATION));
    if matches!(status, BurnedIn::Yes) {
        return status;
    }
    for (_, el) in ds.iter() {
        if let Value::Sequence(items) = &el.value {
            for item in items {
                status = worst_burned_in(status, burned_in_status(item));
                if matches!(status, BurnedIn::Yes) {
                    return status;
                }
            }
        }
    }
    status
}

fn worst_burned_in(a: BurnedIn, b: BurnedIn) -> BurnedIn {
    match (a, b) {
        (BurnedIn::Yes, _) | (_, BurnedIn::Yes) => BurnedIn::Yes,
        (BurnedIn::Unknown, _) | (_, BurnedIn::Unknown) => BurnedIn::Unknown,
        _ => BurnedIn::Clean,
    }
}

/// Verdict for one Burned In Annotation element (`None` = absent).
fn burned_in_element(el: Option<&vyna_dcm_core::DataElement>) -> BurnedIn {
    let Some(el) = el else {
        return BurnedIn::Clean;
    };
    match &el.value {
        Value::Text(values) => {
            let mut saw_no = false;
            for v in values {
                if is_burned_in_word(v, "YES") {
                    return BurnedIn::Yes;
                }
                saw_no |= is_burned_in_word(v, "NO");
            }
            // Every value must prove NO; an empty list or any other
            // spelling stays Unknown.
            if saw_no && values.iter().all(|v| is_burned_in_word(v, "NO")) {
                BurnedIn::Clean
            } else {
                BurnedIn::Unknown
            }
        }
        Value::Bytes(bytes) => {
            let text = String::from_utf8_lossy(bytes);
            if is_burned_in_word(&text, "YES") {
                BurnedIn::Yes
            } else if is_burned_in_word(&text, "NO") {
                BurnedIn::Clean
            } else {
                BurnedIn::Unknown
            }
        }
        // Present-but-empty and mistyped VRs prove nothing.
        _ => BurnedIn::Unknown,
    }
}

/// Trim NUL padding as well as whitespace (`YES\0` from dirty writers
/// must refuse, not slip through) and compare case-insensitively:
/// this is a safety gate, not a parser.
fn is_burned_in_word(value: &str, word: &str) -> bool {
    value
        .trim_matches(|c: char| c.is_whitespace() || c == '\0')
        .eq_ignore_ascii_case(word)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> DataSet {
        let mut ds = DataSet::new();
        ds.put_text(tags::CLINICAL_TRIAL_SUBJECT_ID, Vr::Lo, "SUBJ-7");
        ds.put_person_name(tags::PATIENT_NAME, vec![PersonName::parse("Doe^Jane")]);
        ds.put_text(tags::PATIENT_ID, Vr::Lo, "E123");
        ds.put_text(tags::STUDY_DATE, Vr::Da, "20240115");
        ds.put_uid(tags::STUDY_INSTANCE_UID, "1.2.3");
        ds.put_text(tags::INSTITUTION_NAME, Vr::Lo, "City Hospital");
        ds.put_text(tags::MODALITY, Vr::Cs, "ECG");
        ds.put_text(Tag::new(0x0009, 0x0010), Vr::Lo, "PRIVATE CREATOR");
        ds.put_text(Tag::new(0x0009, 0x1001), Vr::Lo, "secret");
        ds.put_bytes(Tag::new(0x6000, 0x3000), Vr::Ow, vec![1, 2]);
        ds.put_bytes(Tag::new(0x5000, 0x3000), Vr::Ow, vec![3, 4]);
        // U* reference sequence: kept, children remapped in place.
        let mut item = DataSet::new();
        item.put_uid(tags::REFERENCED_SOP_INSTANCE_UID, "1.2.3");
        item.put_text(tags::PATIENT_ID, Vr::Lo, "E999");
        ds.put_sequence(tags::REFERENCED_IMAGE_SEQUENCE, vec![item]);
        ds
    }

    #[test]
    fn every_action_fires() {
        let mut ds = fixture();
        let report = Deidentifier::new().apply(&mut ds).unwrap();
        assert_eq!(
            ds.string(tags::CLINICAL_TRIAL_SUBJECT_ID),
            Some("ANONYMIZED")
        );
        assert!(ds.get(tags::PATIENT_NAME).unwrap().value.is_empty());
        assert!(ds.get(tags::PATIENT_ID).unwrap().value.is_empty());
        // E.1 Z date with no offset: emptied, not dummied.
        assert!(ds.get(tags::STUDY_DATE).unwrap().value.is_empty());
        let uid = ds.string(tags::STUDY_INSTANCE_UID).unwrap();
        assert_ne!(uid, "1.2.3");
        assert!(vyna_dcm_core::is_valid_uid(uid));
        assert!(!ds.contains(tags::INSTITUTION_NAME));
        assert_eq!(ds.string(tags::MODALITY), Some("ECG")); // kept
        assert!(!ds.contains(Tag::new(0x0009, 0x0010)));
        assert!(!ds.contains(Tag::new(0x0009, 0x1001)));
        assert!(!ds.contains(Tag::new(0x6000, 0x3000)));
        assert!(!ds.contains(Tag::new(0x5000, 0x3000)));
        // Sequence recursion: same input UID maps to the same fresh UID.
        let item = &ds.sequence(tags::REFERENCED_IMAGE_SEQUENCE).unwrap()[0];
        assert_eq!(item.string(tags::REFERENCED_SOP_INSTANCE_UID), Some(uid));
        assert!(item.get(tags::PATIENT_ID).unwrap().value.is_empty());
        assert_eq!(
            (
                report.removed,
                report.zeroed,
                report.dummied,
                report.uids_remapped
            ),
            (5, 4, 1, 2)
        );
        assert_eq!(
            ds.string(tags::LONGITUDINAL_TEMPORAL_INFORMATION_MODIFIED),
            Some("REMOVED")
        );
        // Clean run: markers stamped, no 113105 without shifting.
        assert_eq!(ds.string(tags::PATIENT_IDENTITY_REMOVED), Some("YES"));
        let methods = ds
            .sequence(tags::DEIDENTIFICATION_METHOD_CODE_SEQUENCE)
            .unwrap();
        assert_eq!(methods.len(), 1);
        assert!(report.warnings.is_empty());
    }

    #[test]
    fn empty_date_components_skipped_not_warned() {
        let mut ds = DataSet::new();
        ds.insert(DataElement::new(
            tags::STUDY_DATE,
            Vr::Da,
            Value::Text(vec!["".into(), "20240101".into()]),
        ));
        let report = Deidentifier::new()
            .with_date_offset(30)
            .apply(&mut ds)
            .unwrap();
        // The empty component holds no PHI: skipped silently, left
        // in place, and the markers are still stamped.
        assert!(report.warnings.is_empty(), "{:?}", report.warnings);
        assert_eq!(report.dates_shifted, 1);
        assert_eq!(
            ds.strings(tags::STUDY_DATE).unwrap(),
            &["".to_string(), "20240131".to_string()]
        );
        assert_eq!(ds.string(tags::PATIENT_IDENTITY_REMOVED), Some("YES"));
    }

    #[test]
    fn kept_date_warning_hides_value_shows_vm_position() {
        let mut ds = DataSet::new();
        ds.insert(DataElement::new(
            tags::STUDY_DATE,
            Vr::Da,
            Value::Text(vec!["".into(), "NOTADATE".into()]),
        ));
        let report = Deidentifier::new()
            .with_date_offset(30)
            .apply(&mut ds)
            .unwrap();
        assert_eq!(report.warnings.len(), 2); // kept value + withheld markers
        assert!(
            !report.warnings[0].contains("NOTADATE"),
            "{:?}",
            report.warnings
        );
        assert!(
            report.warnings[0].contains("value 2"),
            "VM position must count the skipped empty: {:?}",
            report.warnings
        );
        // The kept value itself is untouched.
        assert_eq!(
            ds.strings(tags::STUDY_DATE).unwrap(),
            &["".to_string(), "NOTADATE".to_string()]
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
        // Kept value + markers-withheld notice; no conformance claim.
        assert_eq!(report.warnings.len(), 2);
        assert!(ds.get(tags::PATIENT_IDENTITY_REMOVED).is_none());
        assert!(
            ds.get(tags::DEIDENTIFICATION_METHOD_CODE_SEQUENCE)
                .is_none()
        );
    }

    #[test]
    fn e1_stragglers_are_handled() {
        // The seven E.1 identifying attributes once absent from the
        // profile: none may survive while the output stamps Basic
        // Profile conformance.
        let mut ds = DataSet::new();
        ds.put_text(tags::PATIENT_SEX, Vr::Cs, "F");
        ds.put_text(tags::MEDICAL_ALERTS, Vr::Lo, "penicillin");
        ds.put_text(tags::ALLERGIES, Vr::Lo, "latex");
        ds.put_text(tags::ADDITIONAL_PATIENT_HISTORY, Vr::Lt, "smoker");
        ds.put_text(
            tags::REASON_FOR_THE_REQUESTED_PROCEDURE,
            Vr::Sh,
            "rule out MI",
        );
        ds.put_sequence(tags::REFERENCED_PATIENT_SEQUENCE, vec![DataSet::new()]);
        ds.put_text(
            tags::ISSUE_DATE_OF_IMAGING_SERVICE_REQUEST,
            Vr::Da,
            "20240115",
        );
        Deidentifier::new().apply(&mut ds).unwrap();
        // E.1 Z: Type 2 stays present but emptied.
        let el = ds.get(tags::PATIENT_SEX).expect("Type 2 stays present");
        assert!(el.value.is_empty(), "Patient's Sex not emptied");
        // E.1 X: removed outright.
        for tag in [
            tags::MEDICAL_ALERTS,
            tags::ALLERGIES,
            tags::ADDITIONAL_PATIENT_HISTORY,
            tags::REASON_FOR_THE_REQUESTED_PROCEDURE,
            tags::REFERENCED_PATIENT_SEQUENCE,
            tags::ISSUE_DATE_OF_IMAGING_SERVICE_REQUEST,
        ] {
            assert!(ds.get(tag).is_none(), "{tag} not removed");
        }
        assert_eq!(ds.string(tags::PATIENT_IDENTITY_REMOVED), Some("YES"));
    }

    #[test]
    fn mistyped_uid_and_date_warn_and_keep() {
        let mut ds = DataSet::new();
        ds.put_bytes(tags::STUDY_INSTANCE_UID, Vr::Ob, vec![1, 2, 3]);
        ds.put_bytes(tags::STUDY_DATE, Vr::Ob, vec![4, 5, 6]);
        let report = Deidentifier::new()
            .with_date_offset(7)
            .apply(&mut ds)
            .unwrap();
        // Two kept values + the markers-withheld notice.
        assert_eq!(report.warnings.len(), 3);
        assert!(matches!(
            ds.get(tags::STUDY_INSTANCE_UID).map(|e| &e.value),
            Some(Value::Bytes(_))
        ));
        assert!(ds.get(tags::PATIENT_IDENTITY_REMOVED).is_none());
    }

    #[test]
    fn burned_in_annotation_refuses_before_mutation() {
        let mut ds = DataSet::new();
        ds.put_text(tags::BURNED_IN_ANNOTATION, Vr::Cs, "YES");
        ds.put_text(tags::PATIENT_ID, Vr::Lo, "E123");
        let err = Deidentifier::new().apply(&mut ds).unwrap_err();
        assert!(matches!(err, Error::BurnedInAnnotation));
        assert_eq!(ds.string(tags::PATIENT_ID), Some("E123"));
        // A YES nested in a sequence item refuses just the same.
        let mut ds = DataSet::new();
        let mut item = DataSet::new();
        item.put_text(tags::BURNED_IN_ANNOTATION, Vr::Cs, "YES");
        ds.put_sequence(tags::REQUEST_ATTRIBUTES_SEQUENCE, vec![item]);
        let err = Deidentifier::new().apply(&mut ds).unwrap_err();
        assert!(matches!(err, Error::BurnedInAnnotation));
    }

    #[test]
    fn burned_in_override_proceeds_warned_and_unmarked() {
        let mut ds = DataSet::new();
        ds.put_text(tags::BURNED_IN_ANNOTATION, Vr::Cs, "YES");
        ds.put_text(tags::PATIENT_ID, Vr::Lo, "E123");
        let report = Deidentifier::new()
            .with_allow_burned_in(true)
            .apply(&mut ds)
            .unwrap();
        assert_eq!(report.warnings.len(), 2, "{:?}", report.warnings);
        assert!(report.warnings[0].contains("attested external pixel redaction"));
        assert!(report.warnings[1].contains("markers withheld"));
        // Metadata still processed…
        assert_ne!(ds.string(tags::PATIENT_ID), Some("E123"));
        // …but the unverifiable override withholds the markers.
        assert_ne!(
            ds.string(tags::PATIENT_IDENTITY_REMOVED),
            Some("YES"),
            "override must not claim the Basic Profile markers"
        );
        assert!(
            ds.sequence(tags::DEIDENTIFICATION_METHOD_CODE_SEQUENCE)
                .is_none()
        );
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
    fn debug_hides_original_uids() {
        let mut deid = Deidentifier::new();
        let mut ds = DataSet::new();
        ds.put_uid(tags::STUDY_INSTANCE_UID, "1.2.3.ORIGINAL");
        deid.apply(&mut ds).unwrap();
        assert!(deid.uid_map().contains_key("1.2.3.ORIGINAL"));
        let shown = format!("{deid:?}");
        assert!(!shown.contains("1.2.3.ORIGINAL"), "{shown}");
        assert!(shown.contains("uid_map_len"), "{shown}");
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
        // Retained privates may hide PHI: markers withheld, said so.
        assert!(ds.get(tags::PATIENT_IDENTITY_REMOVED).is_none());
        assert!(
            report
                .warnings
                .iter()
                .any(|w| w.contains("retained private"))
        );
    }

    #[test]
    fn custom_dummies_apply() {
        let mut ds = DataSet::new();
        ds.put_person_name(
            tags::VERIFYING_OBSERVER_NAME,
            vec![PersonName::parse("A^B")],
        );
        ds.put_text(tags::CLINICAL_TRIAL_SUBJECT_ID, Vr::Lo, "SUBJ-0");
        Deidentifier::new()
            .with_replacement_id("SUBJ-7")
            .unwrap()
            .with_replacement_name("Subject^Seven")
            .unwrap()
            .apply(&mut ds)
            .unwrap();
        assert_eq!(ds.string(tags::CLINICAL_TRIAL_SUBJECT_ID), Some("SUBJ-7"));
        assert_eq!(
            ds.person_names(tags::VERIFYING_OBSERVER_NAME).unwrap()[0].to_dicom_string(),
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

    #[test]
    fn type1_date_dummies_without_offset() {
        // E.1 X/D Type 1 dates stay non-empty without an offset.
        let mut ds = DataSet::new();
        ds.put_text(tags::SERIES_DATE, Vr::Da, "20240115");
        let report = Deidentifier::new().apply(&mut ds).unwrap();
        assert_eq!(ds.string(tags::SERIES_DATE), Some("19000101"));
        assert_eq!(report.dummied, 1);
        assert_eq!(
            ds.string(tags::LONGITUDINAL_TEMPORAL_INFORMATION_MODIFIED),
            Some("REMOVED")
        );
    }

    #[test]
    fn dummy_shapes_non_text_vrs() {
        // Dummy under numeric/AT VRs (dirty-file mistypes or custom
        // profiles) must stay encodable: zeros of the right width,
        // never text under a binary VR.
        let mut ds = DataSet::new();
        ds.put_u16_single(tags::ROWS, 7);
        ds.put_text(tags::PATIENT_WEIGHT, Vr::Ds, "80");
        ds.insert(DataElement::tags(
            tags::FRAME_INCREMENT_POINTER,
            vec![tags::ROWS],
        ));
        ds.insert(DataElement::new(
            tags::COLUMNS,
            Vr::Si,
            Value::Bytes(vec![1, 2]),
        ));
        let report = Deidentifier::new()
            .with_profile(&[
                (tags::ROWS, Action::Dummy),
                (tags::PATIENT_WEIGHT, Action::Dummy),
                (tags::FRAME_INCREMENT_POINTER, Action::Dummy),
                (tags::COLUMNS, Action::Dummy),
            ])
            .apply(&mut ds)
            .unwrap();
        assert!(matches!(
            ds.get(tags::ROWS).map(|e| &e.value),
            Some(Value::U16(v)) if v == &vec![0u16]
        ));
        assert_eq!(ds.string(tags::PATIENT_WEIGHT), Some("0"));
        assert!(matches!(
            ds.get(tags::FRAME_INCREMENT_POINTER).map(|e| &e.value),
            Some(Value::Tags(v)) if v.is_empty()
        ));
        // Retired SI kept as-is with a warning, not dummied.
        assert!(matches!(
            ds.get(tags::COLUMNS).map(|e| &e.value),
            Some(Value::Bytes(v)) if v == &vec![1u8, 2]
        ));
        assert_eq!(report.dummied, 3);
        assert!(report.warnings.iter().any(|w| w.contains("SI dummy kept")));
    }

    #[test]
    fn dummy_byte_vrs_pad_to_unit_multiples() {
        // "ANONYMIZED" is 10 bytes: fine for OB/OW, but OF/OD/OL/OV
        // need 4/8-byte multiples — pad with zeros, not even-padding.
        let mut ds = DataSet::new();
        ds.put_bytes(tags::PIXEL_DATA, Vr::Ow, vec![1, 2, 3, 4]);
        ds.put_bytes(tags::FLOAT_PIXEL_DATA, Vr::Of, vec![1, 2, 3, 4]);
        ds.put_bytes(tags::DOUBLE_FLOAT_PIXEL_DATA, Vr::Od, vec![1, 2, 3, 4]);
        ds.put_bytes(tags::SELECTOR_OL_VALUE, Vr::Ol, vec![1, 2, 3, 4]);
        ds.put_bytes(tags::SELECTOR_OV_VALUE, Vr::Ov, vec![1, 2, 3, 4]);
        Deidentifier::new()
            .with_profile(&[
                (tags::PIXEL_DATA, Action::Dummy),
                (tags::FLOAT_PIXEL_DATA, Action::Dummy),
                (tags::DOUBLE_FLOAT_PIXEL_DATA, Action::Dummy),
                (tags::SELECTOR_OL_VALUE, Action::Dummy),
                (tags::SELECTOR_OV_VALUE, Action::Dummy),
            ])
            .apply(&mut ds)
            .unwrap();
        for (tag, unit) in [
            (tags::PIXEL_DATA, 2),
            (tags::FLOAT_PIXEL_DATA, 4),
            (tags::DOUBLE_FLOAT_PIXEL_DATA, 8),
            (tags::SELECTOR_OL_VALUE, 4),
            (tags::SELECTOR_OV_VALUE, 8),
        ] {
            let bytes = ds.bytes(tag).unwrap();
            assert_eq!(bytes.len() % unit, 0, "{tag:?}: {}", bytes.len());
            assert!(bytes.starts_with(b"ANONYMIZED"), "{tag:?}");
            assert!(
                bytes[b"ANONYMIZED".len()..].iter().all(|b| *b == 0),
                "{tag:?}"
            );
        }
    }

    #[test]
    fn empty_uid_and_date_elements_stay_silent() {
        // Zero-length Type 2 values hold no PHI: no warnings, no
        // marker withholding, no remap/shift counts.
        let mut ds = DataSet::new();
        ds.put_null(tags::STUDY_INSTANCE_UID, Vr::Ui);
        ds.put_null(tags::STUDY_DATE, Vr::Da);
        ds.put_uid(tags::SERIES_INSTANCE_UID, "1.2.3.4");
        ds.put_text(tags::SERIES_DATE, Vr::Da, "20200101");
        let report = Deidentifier::new()
            .with_date_offset(30)
            .apply(&mut ds)
            .unwrap();
        assert!(report.warnings.is_empty(), "{:?}", report.warnings);
        assert_eq!(report.uids_remapped, 1);
        assert_eq!(report.dates_shifted, 1);
        assert_eq!(ds.string(tags::SERIES_DATE), Some("20200131"));
        assert!(
            ds.get(tags::PATIENT_IDENTITY_REMOVED)
                .is_some_and(|e| e.value == Value::Text(vec!["YES".to_string()]))
        );
    }

    #[test]
    fn shift_action_on_non_date_vr_warns_and_keeps() {
        // Custom profile pointing shift actions at non-DA/DT VRs:
        // warn-and-keep (both branches), never VR-invalid dummies.
        for offset in [None, Some(30)] {
            let mut ds = DataSet::new();
            ds.put_text(tags::STUDY_TIME, Vr::Tm, "120000");
            let mut deid =
                Deidentifier::new().with_profile(&[(tags::STUDY_TIME, Action::ShiftDate)]);
            if let Some(days) = offset {
                deid = deid.with_date_offset(days);
            }
            let report = deid.apply(&mut ds).unwrap();
            assert_eq!(ds.string(tags::STUDY_TIME), Some("120000"));
            assert_eq!(report.dates_touched, 0);
            assert_eq!(report.dates_lost, 0);
            assert!(
                report.warnings.iter().any(|w| w.contains("not a date VR")),
                "{offset:?}: {:?}",
                report.warnings
            );
        }
    }

    #[test]
    fn mixed_shift_and_loss_reports_removed() {
        // One date shifted, one emptied: claiming MODIFIED would lie
        // about maintained intervals.
        let mut ds = DataSet::new();
        ds.put_text(tags::STUDY_DATE, Vr::Da, "20240101");
        ds.put_text(tags::SERIES_DATE, Vr::Da, "20240115");
        let report = Deidentifier::new()
            .with_profile(&[
                (tags::STUDY_DATE, Action::ShiftDate),
                (tags::SERIES_DATE, Action::Zero),
            ])
            .with_date_offset(30)
            .apply(&mut ds)
            .unwrap();
        assert_eq!(ds.string(tags::STUDY_DATE), Some("20240131"));
        assert_eq!(ds.string(tags::SERIES_DATE), None);
        assert_eq!((report.dates_shifted, report.dates_lost), (1, 1));
        assert_eq!(
            ds.string(tags::LONGITUDINAL_TEMPORAL_INFORMATION_MODIFIED),
            Some("REMOVED")
        );
    }

    #[test]
    fn lost_only_date_reports_removed() {
        // No shift action ran at all, but a date was emptied: the
        // flag must appear (previously only shift-seen dates gated
        // it, so this run stamped nothing).
        let mut ds = DataSet::new();
        ds.put_text(tags::SERIES_DATE, Vr::Da, "20240115");
        let report = Deidentifier::new()
            .with_profile(&[(tags::SERIES_DATE, Action::Zero)])
            .apply(&mut ds)
            .unwrap();
        assert_eq!(report.dates_lost, 1);
        assert_eq!(
            ds.string(tags::LONGITUDINAL_TEMPORAL_INFORMATION_MODIFIED),
            Some("REMOVED")
        );
    }

    #[test]
    fn kept_only_dates_report_unmodified() {
        // Unshiftable values are kept as originals: intervals intact,
        // so neither MODIFIED (nothing shifted) nor REMOVED (nothing
        // lost) is honest. Markers are still withheld (warning).
        let mut ds = DataSet::new();
        ds.put_text(tags::STUDY_DATE, Vr::Da, "baddate");
        let report = Deidentifier::new()
            .with_date_offset(30)
            .apply(&mut ds)
            .unwrap();
        assert_eq!(ds.string(tags::STUDY_DATE), Some("baddate"));
        assert_eq!((report.dates_shifted, report.dates_lost), (0, 0));
        assert!(!report.warnings.is_empty());
        assert_eq!(
            ds.string(tags::LONGITUDINAL_TEMPORAL_INFORMATION_MODIFIED),
            Some("UNMODIFIED")
        );
    }

    #[test]
    fn shifted_dates_stamp_113105() {
        let mut ds = DataSet::new();
        ds.put_text(tags::STUDY_DATE, Vr::Da, "20240101");
        ds.put_text(tags::SERIES_DATE, Vr::Da, "20240115");
        Deidentifier::new()
            .with_date_offset(30)
            .apply(&mut ds)
            .unwrap();
        let methods = ds
            .sequence(tags::DEIDENTIFICATION_METHOD_CODE_SEQUENCE)
            .unwrap();
        assert_eq!(methods.len(), 2);
        assert_eq!(methods[0].string(tags::CODE_VALUE), Some("113100"));
        assert_eq!(methods[1].string(tags::CODE_VALUE), Some("113105"));
        assert_eq!(
            methods[1].string(tags::CODING_SCHEME_DESIGNATOR),
            Some("DCM")
        );
    }

    #[test]
    fn tm_and_age_dummies_are_vr_valid() {
        let mut ds = DataSet::new();
        ds.put_text(tags::SERIES_TIME, Vr::Tm, "120000");
        ds.put_text(tags::CONTENT_TIME, Vr::Tm, "120000");
        ds.put_text(tags::SELECTOR_AS_VALUE, Vr::As, "042Y");
        Deidentifier::new().apply(&mut ds).unwrap();
        assert_eq!(ds.string(tags::SERIES_TIME), Some("000000"));
        assert_eq!(ds.string(tags::CONTENT_TIME), Some("000000"));
        assert_eq!(ds.string(tags::SELECTOR_AS_VALUE), Some("000Y"));
    }

    #[test]
    fn tm_without_dummy_action_zeroes_or_removes() {
        // Study Time (E.1 Z, Type 2) empties; Acquisition Time (E.1 X/Z,
        // Type 2) empties; a T3 X time is removed.
        let mut ds = DataSet::new();
        ds.put_text(tags::STUDY_TIME, Vr::Tm, "120000");
        ds.put_text(tags::ACQUISITION_TIME, Vr::Tm, "120000");
        ds.put_text(tags::TREATMENT_TIME, Vr::Tm, "120000");
        Deidentifier::new().apply(&mut ds).unwrap();
        assert!(ds.get(tags::STUDY_TIME).unwrap().value.is_empty());
        assert!(ds.get(tags::ACQUISITION_TIME).unwrap().value.is_empty());
        assert!(!ds.contains(tags::TREATMENT_TIME));
    }

    #[test]
    fn encapsulated_document_is_replaced_with_dummy_bytes() {
        let mut ds = DataSet::new();
        ds.put_bytes(
            tags::ENCAPSULATED_DOCUMENT,
            Vr::Ob,
            b"%PDF-1.4 patient payload".to_vec(),
        );
        let report = Deidentifier::new().apply(&mut ds).unwrap();
        assert_eq!(report.dummied, 1);
        match ds.get(tags::ENCAPSULATED_DOCUMENT).map(|e| &e.value) {
            Some(Value::Bytes(b)) => assert_eq!(b, b"ANONYMIZED"),
            other => panic!("expected dummy bytes, got {other:?}"),
        }
    }

    #[test]
    fn sequence_dummy_writes_a_single_empty_item() {
        // E.1 D sequences (e.g. Person ID Code Seq) keep one empty item:
        // non-empty for conditional presence, content-free.
        let mut ds = DataSet::new();
        let mut item = DataSet::new();
        item.put_text(tags::PATIENT_ID, Vr::Lo, "E123");
        ds.put_sequence(tags::PERSON_IDENTIFICATION_CODE_SEQUENCE, vec![item]);
        Deidentifier::new().apply(&mut ds).unwrap();
        let items = ds
            .sequence(tags::PERSON_IDENTIFICATION_CODE_SEQUENCE)
            .unwrap();
        assert_eq!(items.len(), 1);
        assert!(items[0].iter().next().is_none());
    }

    #[test]
    fn trial_rows_follow_e1_letters() {
        let mut ds = DataSet::new();
        ds.put_text(tags::CLINICAL_TRIAL_SPONSOR_NAME, Vr::Lo, "Acme");
        ds.put_text(tags::CLINICAL_TRIAL_PROTOCOL_NAME, Vr::Lo, "P-1");
        ds.put_text(tags::CLINICAL_TRIAL_SITE_ID, Vr::Lo, "S-1");
        ds.put_text(tags::CLINICAL_TRIAL_SUBJECT_ID, Vr::Lo, "SUBJ-1");
        Deidentifier::new().apply(&mut ds).unwrap();
        // E.1 D rows stay non-empty; E.1 Z rows empty.
        assert_eq!(
            ds.string(tags::CLINICAL_TRIAL_SPONSOR_NAME),
            Some("ANONYMIZED")
        );
        assert_eq!(
            ds.string(tags::CLINICAL_TRIAL_SUBJECT_ID),
            Some("ANONYMIZED")
        );
        assert!(
            ds.get(tags::CLINICAL_TRIAL_PROTOCOL_NAME)
                .unwrap()
                .value
                .is_empty()
        );
        assert!(
            ds.get(tags::CLINICAL_TRIAL_SITE_ID)
                .unwrap()
                .value
                .is_empty()
        );
    }

    #[test]
    fn conditional_rows_stay_present() {
        // Responsible Person/Organization are E.1 X but Type 2
        // conditional: emptied, never removed.
        let mut ds = DataSet::new();
        ds.put_text(tags::RESPONSIBLE_PERSON, Vr::Pn, "Doe^Jane");
        ds.put_text(tags::RESPONSIBLE_ORGANIZATION, Vr::Lo, "Acme");
        Deidentifier::new().apply(&mut ds).unwrap();
        assert!(ds.get(tags::RESPONSIBLE_PERSON).unwrap().value.is_empty());
        assert!(
            ds.get(tags::RESPONSIBLE_ORGANIZATION)
                .unwrap()
                .value
                .is_empty()
        );
    }

    #[test]
    fn unlisted_clinical_content_is_kept() {
        // E.1-unlisted species/breed (conditional validity) and SR Text
        // Value (clinical content) have no rows: kept verbatim.
        let mut ds = DataSet::new();
        ds.put_text(tags::PATIENT_SPECIES_DESCRIPTION, Vr::Lo, "Felis catus");
        ds.put_text(tags::TEXT_VALUE, Vr::St, "finding text");
        let report = Deidentifier::new().apply(&mut ds).unwrap();
        assert_eq!(
            ds.string(tags::PATIENT_SPECIES_DESCRIPTION),
            Some("Felis catus")
        );
        assert_eq!(ds.string(tags::TEXT_VALUE), Some("finding text"));
        assert_eq!(ds.string(tags::PATIENT_IDENTITY_REMOVED), Some("YES"));
        assert!(report.warnings.is_empty());
    }

    #[test]
    fn custom_profile_withholds_markers() {
        let mut ds = DataSet::new();
        ds.put_text(tags::PATIENT_ID, Vr::Lo, "E123");
        let profile = [(tags::PATIENT_ID, Action::Zero)];
        let report = Deidentifier::new()
            .with_profile(&profile)
            .apply(&mut ds)
            .unwrap();
        assert!(ds.get(tags::PATIENT_IDENTITY_REMOVED).is_none());
        assert!(
            ds.get(tags::DEIDENTIFICATION_METHOD_CODE_SEQUENCE)
                .is_none()
        );
        assert!(report.warnings.iter().any(|w| w.contains("custom profile")));
    }

    #[test]
    fn curve_group_rule_bounds() {
        assert!(is_curve_data(Tag::new(0x5000, 0x3000)));
        assert!(is_curve_data(Tag::new(0x50FE, 0x0001)));
        assert!(!is_curve_data(Tag::new(0x4FFE, 0x3000)));
        assert!(!is_curve_data(Tag::new(0x5100, 0x3000)));
        // Odd groups are private (removed by the private rule instead).
        assert!(!is_curve_data(Tag::new(0x5001, 0x3000)));
        assert!(!is_curve_data(Tag::new(0x6000, 0x3000)));
    }

    #[test]
    fn burned_in_gate_checks_every_value_and_vr() {
        // YES hiding behind a first NO value must still refuse.
        let mut ds = DataSet::new();
        ds.put_text(tags::BURNED_IN_ANNOTATION, Vr::Cs, "NO");
        if let Some(el) = ds.get_mut(tags::BURNED_IN_ANNOTATION) {
            el.value = Value::Text(vec!["NO".to_string(), "YES".to_string()]);
        }
        let err = Deidentifier::new().apply(&mut ds).unwrap_err();
        assert!(matches!(err, Error::BurnedInAnnotation));

        // YES smuggled in a mistyped byte value must refuse too.
        let mut ds = DataSet::new();
        ds.put_bytes(tags::BURNED_IN_ANNOTATION, Vr::Ob, b"YES".to_vec());
        let err = Deidentifier::new().apply(&mut ds).unwrap_err();
        assert!(matches!(err, Error::BurnedInAnnotation));

        // Bytes NO passes and is restamped through the text path.
        let mut ds = DataSet::new();
        ds.put_bytes(tags::BURNED_IN_ANNOTATION, Vr::Ob, b"NO".to_vec());
        Deidentifier::new().apply(&mut ds).unwrap();
        assert_eq!(ds.string(tags::BURNED_IN_ANNOTATION), Some("NO"));
    }

    #[test]
    fn burned_in_unparseable_warns_and_withholds() {
        for setup in ["MAYBE", ""] {
            let mut ds = DataSet::new();
            ds.put_text(tags::BURNED_IN_ANNOTATION, Vr::Cs, setup);
            let report = Deidentifier::new().apply(&mut ds).unwrap();
            // Kept as-is, warned, markers withheld.
            assert_eq!(ds.string(tags::BURNED_IN_ANNOTATION), Some(setup));
            assert!(report.warnings.iter().any(|w| w.contains("unparseable")));
            assert!(ds.get(tags::PATIENT_IDENTITY_REMOVED).is_none());
        }
    }

    #[test]
    fn bad_replacement_id_is_an_error_not_a_panic() {
        // Overlong, backslash (VM corruption on encode), controls.
        for bad in ["12345678901234567", "A\\B", "A\tB", "A\n", "\u{7f}"] {
            let err = Deidentifier::new().with_replacement_id(bad).unwrap_err();
            assert!(
                matches!(err, Error::InvalidReplacementId(_)),
                "{bad:?}: {err:?}"
            );
        }
        assert!(
            Deidentifier::new()
                .with_replacement_id("1234567890123456")
                .is_ok()
        );
    }

    #[test]
    fn bad_replacement_name_is_an_error_not_a_panic() {
        for bad in [&"A".repeat(65) as &str, "A^B^C^D^E^F", "a=b=c=d", "A\\B"] {
            let err = Deidentifier::new()
                .with_replacement_name(bad)
                .expect_err("bad name must fail");
            assert!(matches!(err, Error::InvalidReplacementName(_)), "{err:?}");
        }
        assert!(
            Deidentifier::new()
                .with_replacement_name("Subject^Seven")
                .is_ok()
        );
    }

    #[test]
    fn dummy_targets_are_dummy_shaped_vrs() {
        // The engine's Dummy arm only shapes text-ish, PN, SQ, TM, AS,
        // DA, DT, UI, and byte VRs: every Dummy row must be one of those.
        use vyna_dcm_core::dictionary_vr;
        for (tag, action) in BASIC_PROFILE {
            if !matches!(action, Action::Dummy) {
                continue;
            }
            let vr = dictionary_vr(*tag).unwrap_or_else(|| panic!("{tag} not in dict"));
            assert!(
                matches!(
                    vr,
                    Vr::Ae
                        | Vr::As
                        | Vr::Cs
                        | Vr::Da
                        | Vr::Ds
                        | Vr::Dt
                        | Vr::Is
                        | Vr::Lo
                        | Vr::Lt
                        | Vr::Pn
                        | Vr::Sh
                        | Vr::St
                        | Vr::Tm
                        | Vr::Uc
                        | Vr::Ui
                        | Vr::Ur
                        | Vr::Ut
                        | Vr::Sq
                        | Vr::Ob
                        | Vr::Ow
                        | Vr::Of
                        | Vr::Od
                        | Vr::Ol
                        | Vr::Ov
                        | Vr::Un
                ),
                "{tag} Dummy over {vr}"
            );
        }
    }
}
