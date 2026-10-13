//! DICOM mapping for ECG recordings (12-Lead / General ECG Waveform IODs,
//! PS3.3 A.34.3 / A.34.5 + Waveform Module C.10.9).
//!
//! [`recording_to_waveform_item`] / [`recording_from_waveform_item`] convert
//! one multiplex group; [`recording_to_dataset`] builds a complete SOP
//! instance for C-STORE / Part-10 / STOW-RS.

use vyna_dcm_core::{DataSet, PersonName, Value, Vr, tags};

use crate::{
    Error, Result,
    model::{EcgRecording, SampleInterpretation, mdc_code_for_lead},
};

/// 12-Lead ECG Waveform Storage SOP Class.
pub const SOP_12_LEAD_ECG: &str = "1.2.840.10008.5.1.4.1.1.9.1.1";
/// General ECG Waveform Storage SOP Class.
pub const SOP_GENERAL_ECG: &str = "1.2.840.10008.5.1.4.1.1.9.1.2";

/// Identifying metadata for a complete ECG SOP instance.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct EcgInstanceMeta {
    /// SOP Class UID (12-lead or General ECG).
    pub sop_class_uid: String,
    /// SOP Instance UID (generated when empty).
    pub sop_instance_uid: String,
    /// Study Instance UID (generated when empty).
    pub study_uid: String,
    /// Series Instance UID (generated when empty).
    pub series_uid: String,
    /// Patient name.
    pub patient_name: PersonName,
    /// Patient ID.
    pub patient_id: String,
    /// Patient birth date `YYYYMMDD`.
    pub birth_date: Option<String>,
    /// Patient sex (`M`/`F`/`O`).
    pub sex: Option<String>,
    /// Study ID.
    pub study_id: Option<String>,
    /// Accession number.
    pub accession: Option<String>,
    /// Study date `YYYYMMDD` (Type 2: empty when unknown).
    pub study_date: Option<String>,
    /// Study time `HHMMSS` (Type 2: empty when unknown).
    pub study_time: Option<String>,
    /// Content date `YYYYMMDD` (Type 1: required).
    pub content_date: Option<String>,
    /// Content time `HHMMSS` (Type 1: required).
    pub content_time: Option<String>,
    /// Acquisition date/time `YYYYMMDDHHMMSS[.ffffff][±HHMM]`
    /// (Type 1: required).
    pub acquisition_datetime: Option<String>,
    /// Referring physician's name (Type 2: empty when unknown).
    pub referring_physician: Option<PersonName>,
    /// Acquisition Context Sequence items (Type 2: empty when none).
    pub acquisition_context: Vec<DataSet>,
    /// Institution name.
    pub institution: Option<String>,
    /// Manufacturer; default `Vyna`.
    pub manufacturer: String,
    /// Manufacturer model; default `VynaECG`.
    pub model: String,
    /// Series description.
    pub series_description: Option<String>,
    /// Instance number.
    pub instance_number: String,
}

impl EcgInstanceMeta {
    /// Minimal metadata; UIDs are generated when left empty.
    /// [`recording_to_dataset`] still needs the Type 1 datetimes
    /// ([`EcgInstanceMeta::content_date`], `content_time`,
    /// `acquisition_datetime`): there is no honest default for them.
    pub fn new(patient_name: PersonName, patient_id: impl Into<String>) -> Self {
        Self {
            sop_class_uid: SOP_12_LEAD_ECG.to_string(),
            sop_instance_uid: String::new(),
            study_uid: String::new(),
            series_uid: String::new(),
            patient_name,
            patient_id: patient_id.into(),
            birth_date: None,
            sex: None,
            study_id: None,
            accession: None,
            study_date: None,
            study_time: None,
            content_date: None,
            content_time: None,
            acquisition_datetime: None,
            referring_physician: None,
            acquisition_context: Vec::new(),
            institution: None,
            manufacturer: "Vyna".into(),
            model: "VynaECG".into(),
            series_description: None,
            instance_number: "1".into(),
        }
    }

    /// True when any stored text needs non-ASCII (UTF-8) encoding.
    fn needs_utf8(&self) -> bool {
        fn pn_non_ascii(pn: &PersonName) -> bool {
            [&pn.alphabetic]
                .into_iter()
                .chain(pn.ideographic.as_ref())
                .chain(pn.phonetic.as_ref())
                .flat_map(|g| [&g.family, &g.given, &g.middle, &g.prefix, &g.suffix])
                .any(|s| !s.is_ascii())
        }
        pn_non_ascii(&self.patient_name)
            || self.referring_physician.as_ref().is_some_and(pn_non_ascii)
            || !self.patient_id.is_ascii()
            || [
                &self.birth_date,
                &self.sex,
                &self.study_id,
                &self.accession,
                &self.study_date,
                &self.study_time,
                &self.content_date,
                &self.content_time,
                &self.acquisition_datetime,
                &self.institution,
                &self.series_description,
            ]
            .into_iter()
            .flatten()
            .any(|s| !s.is_ascii())
            || !self.manufacturer.is_ascii()
            || !self.model.is_ascii()
            || !self.instance_number.is_ascii()
    }

    fn fill_uids(&mut self) {
        for uid in [
            &mut self.sop_instance_uid,
            &mut self.study_uid,
            &mut self.series_uid,
        ] {
            if uid.is_empty() {
                *uid = vyna_dcm_core::new_uid();
            }
        }
    }
}

/// True when any recording text (lead labels/units/codes, multiplex
/// label, amplifier type) needs non-ASCII encoding. Checked alongside
/// the header scan so a fully ASCII header with non-ASCII lead text
/// still declares UTF-8.
fn recording_needs_utf8(rec: &EcgRecording) -> bool {
    rec.multiplex_label.as_ref().is_some_and(|s| !s.is_ascii())
        || rec.amplifier_type.as_ref().is_some_and(|s| !s.is_ascii())
        || rec.leads.iter().any(|l| {
            !l.label.is_ascii()
                || !l.units.is_ascii()
                || l.mdc_code.as_ref().is_some_and(|s| !s.is_ascii())
        })
}

/// Convert a recording into one Waveform Sequence item.
pub fn recording_to_waveform_item(rec: &EcgRecording) -> Result<DataSet> {
    if !matches!(rec.bits_allocated, 8 | 16) {
        return Err(Error::Samples(format!(
            "bits_allocated {} (want 8 or 16)",
            rec.bits_allocated
        )));
    }
    // Fields are public, so `EcgRecording::new`'s checks are
    // bypassable: re-validate shape here, since unequal lead
    // lengths would panic the interleaver below.
    if rec.leads.is_empty() {
        return Err(Error::Waveform("recording without leads".into()));
    }
    let samples = rec.samples_per_lead();
    if samples == 0 {
        return Err(Error::Waveform("recording without samples".into()));
    }
    if rec.leads.iter().any(|l| l.len() != samples) {
        return Err(Error::Waveform("leads have unequal sample counts".into()));
    }
    if rec.sampling_frequency_hz <= 0.0 {
        return Err(Error::Waveform(format!(
            "bad sampling frequency {}",
            rec.sampling_frequency_hz
        )));
    }
    // Waveform Originality is a closed enumerated pair (C.10.9.1.4):
    // anything else would emit a non-conformant item.
    if !matches!(rec.originality.as_str(), "ORIGINAL" | "DERIVED") {
        return Err(Error::InvalidMeta(format!(
            "Waveform Originality {:?} (want ORIGINAL or DERIVED)",
            rec.originality
        )));
    }
    // Format a DS field, refusing non-finite floats (NaN/inf are not
    // valid DS values).
    let ds_field = |what: &str, v: f64| {
        ds_format(v).ok_or_else(|| Error::Waveform(format!("non-finite {what}")))
    };
    let channels =
        u16::try_from(rec.leads.len()).map_err(|_| Error::Waveform("too many leads".into()))?;
    let sample_count =
        u32::try_from(samples).map_err(|_| Error::Waveform("too many samples".into()))?;
    let mut item = DataSet::new();
    item.put_text(tags::WAVEFORM_ORIGINALITY, Vr::Cs, rec.originality.clone());
    item.put_u16_single(tags::NUMBER_OF_WAVEFORM_CHANNELS, channels);
    item.put_u32_single(tags::NUMBER_OF_WAVEFORM_SAMPLES, sample_count);
    item.put_text(
        tags::SAMPLING_FREQUENCY,
        Vr::Ds,
        ds_field("sampling frequency", rec.sampling_frequency_hz)?,
    );
    if let Some(label) = &rec.multiplex_label {
        item.put_text(tags::MULTIPLEX_GROUP_LABEL, Vr::Sh, label.clone());
    }

    let mut channels = Vec::with_capacity(rec.leads.len());
    for (i, lead) in rec.leads.iter().enumerate() {
        let mut ch = DataSet::new();
        ch.put_text(tags::WAVEFORM_CHANNEL_NUMBER, Vr::Is, (i + 1).to_string());
        ch.put_text(tags::CHANNEL_LABEL, Vr::Sh, lead.label.clone());
        ch.put_text(tags::CHANNEL_STATUS, Vr::Cs, "OK");
        ch.put_sequence(tags::CHANNEL_SOURCE_SEQUENCE, vec![lead_source_item(lead)]);
        ch.put_text(
            tags::CHANNEL_SENSITIVITY,
            Vr::Ds,
            ds_field("channel sensitivity", lead.sensitivity)?,
        );
        ch.put_sequence(
            tags::CHANNEL_SENSITIVITY_UNITS_SEQUENCE,
            vec![units_item(&lead.units)],
        );
        ch.put_text(
            tags::CHANNEL_SENSITIVITY_CORRECTION_FACTOR,
            Vr::Ds,
            ds_field("sensitivity correction factor", lead.correction_factor)?,
        );
        ch.put_text(
            tags::CHANNEL_BASELINE,
            Vr::Ds,
            ds_field("channel baseline", lead.baseline)?,
        );
        ch.put_text(tags::CHANNEL_TIME_SKEW, Vr::Ds, "0");
        ch.put_text(tags::CHANNEL_SAMPLE_SKEW, Vr::Ds, "0");
        ch.put_text(tags::CHANNEL_OFFSET, Vr::Ds, "0");
        ch.put_u16_single(tags::WAVEFORM_BITS_STORED, rec.bits_allocated);
        if let Some(f) = lead.filter_low_hz {
            ch.put_text(
                tags::FILTER_LOW_FREQUENCY,
                Vr::Ds,
                ds_field("filter frequency", f)?,
            );
        }
        if let Some(f) = lead.filter_high_hz {
            ch.put_text(
                tags::FILTER_HIGH_FREQUENCY,
                Vr::Ds,
                ds_field("filter frequency", f)?,
            );
        }
        if let Some(f) = lead.notch_hz {
            ch.put_text(
                tags::NOTCH_FILTER_FREQUENCY,
                Vr::Ds,
                ds_field("filter frequency", f)?,
            );
        }
        channels.push(ch);
    }
    item.put_sequence(tags::CHANNEL_DEFINITION_SEQUENCE, channels);

    item.put_u16_single(tags::WAVEFORM_BITS_ALLOCATED, rec.bits_allocated);
    let interp = match (rec.sample_interpretation, rec.bits_allocated) {
        (SampleInterpretation::Signed, 16) => "SS",
        (SampleInterpretation::Unsigned, 16) => "US",
        (SampleInterpretation::Signed, 8) => "SB",
        (SampleInterpretation::Unsigned, 8) => "UB",
        _ => unreachable!(),
    };
    item.put_text(tags::WAVEFORM_SAMPLE_INTERPRETATION, Vr::Cs, interp);
    // PS3.5 §8.3: 8-bit samples ride OB, 16-bit samples OW.
    let data_vr = if rec.bits_allocated == 8 {
        Vr::Ob
    } else {
        Vr::Ow
    };
    item.put_bytes(tags::WAVEFORM_DATA, data_vr, interleave(rec)?);
    if let Some(f) = rec.powerline_hz {
        item.put_text(
            tags::POWERLINE_FREQUENCY,
            Vr::Ds,
            ds_field("powerline frequency", f)?,
        );
    }
    if let Some(a) = &rec.amplifier_type {
        item.put_text(tags::WAVEFORM_AMPLIFIER_TYPE, Vr::Cs, a.clone());
    }
    Ok(item)
}

fn lead_source_item(lead: &crate::model::EcgLead) -> DataSet {
    let mut item = DataSet::new();
    match lead
        .mdc_code
        .clone()
        .or_else(|| mdc_code_for_lead(&lead.label).map(str::to_string))
    {
        Some(code) => {
            item.put_text(tags::CODE_VALUE, Vr::Sh, code);
            item.put_text(tags::CODING_SCHEME_DESIGNATOR, Vr::Sh, "MDC");
            item.put_text(tags::CODE_MEANING, Vr::Lo, lead.label.clone());
        }
        None => {
            item.put_text(tags::CODE_VALUE, Vr::Sh, lead.label.clone());
            item.put_text(tags::CODING_SCHEME_DESIGNATOR, Vr::Sh, "99VYNA");
            item.put_text(tags::CODE_MEANING, Vr::Lo, format!("Lead {}", lead.label));
        }
    }
    item
}

fn units_item(units: &str) -> DataSet {
    let mut item = DataSet::new();
    item.put_text(tags::CODE_VALUE, Vr::Sh, units);
    item.put_text(tags::CODING_SCHEME_DESIGNATOR, Vr::Sh, "UCUM");
    item.put_text(tags::CODE_MEANING, Vr::Lo, units);
    item
}

/// Format an `f64` as a `DS` value (≤ 16 chars);
/// `None` for non-finite inputs (never written: NaN/inf are not DS).
fn ds_format(v: f64) -> Option<String> {
    if !v.is_finite() {
        return None;
    }
    if v == 0.0 {
        return Some("0".into());
    }
    // Full precision first, then fewer decimals until the 16-char DS
    // limit holds. A blind truncate would corrupt the value (or leave
    // a trailing dot). A fixed-point spelling that collapses a nonzero
    // value to zero is skipped (not returned), so tiny values fall
    // through to scientific notation, which DS permits.
    for prec in (0..=15).rev() {
        let mut s = format!("{v:.prec$}");
        if s.contains('.') {
            while s.ends_with('0') {
                s.pop();
            }
            if s.ends_with('.') {
                s.pop();
            }
        }
        if s.len() <= 16 && s.parse::<f64>().is_ok_and(|back| back != 0.0) {
            return Some(s);
        }
    }
    let s = format!("{v:.6e}");
    if s.len() <= 16 { Some(s) } else { None }
}

/// Interleave channel samples (sample-major, little-endian).
fn interleave(rec: &EcgRecording) -> Result<Vec<u8>> {
    let (channels, samples) = (rec.leads.len(), rec.samples_per_lead());
    let mut out = Vec::with_capacity(channels * samples * (rec.bits_allocated as usize / 8));
    for t in 0..samples {
        for lead in &rec.leads {
            let v = lead.samples[t];
            match (rec.sample_interpretation, rec.bits_allocated) {
                (SampleInterpretation::Signed, 16) => {
                    let w = i16::try_from(v)
                        .map_err(|_| Error::Samples(format!("sample {v} out of SS range")))?;
                    out.extend_from_slice(&w.to_le_bytes());
                }
                (SampleInterpretation::Unsigned, 16) => {
                    let w = u16::try_from(v)
                        .map_err(|_| Error::Samples(format!("sample {v} out of US range")))?;
                    out.extend_from_slice(&w.to_le_bytes());
                }
                (SampleInterpretation::Signed, 8) => {
                    let w = i8::try_from(v)
                        .map_err(|_| Error::Samples(format!("sample {v} out of SB range")))?;
                    out.push(w as u8);
                }
                (SampleInterpretation::Unsigned, 8) => {
                    let w = u8::try_from(v)
                        .map_err(|_| Error::Samples(format!("sample {v} out of UB range")))?;
                    out.push(w);
                }
                _ => unreachable!(),
            }
        }
    }
    Ok(out)
}

/// Parse one Waveform Sequence item back into a recording.
pub fn recording_from_waveform_item(item: &DataSet) -> Result<EcgRecording> {
    let req = |tag| {
        item.require_string(tag)
            .map_err(|_| Error::Missing(tag.to_string()))
    };
    let channels = item
        .uint16(tags::NUMBER_OF_WAVEFORM_CHANNELS)
        .ok_or_else(|| Error::Missing(tags::NUMBER_OF_WAVEFORM_CHANNELS.to_string()))?
        as usize;
    let samples = item
        .uint32(tags::NUMBER_OF_WAVEFORM_SAMPLES)
        .ok_or_else(|| Error::Missing(tags::NUMBER_OF_WAVEFORM_SAMPLES.to_string()))?
        as usize;
    let freq: f64 = req(tags::SAMPLING_FREQUENCY)?
        .trim()
        .parse()
        .map_err(|_| Error::Waveform("bad Sampling Frequency".into()))?;
    // `!is_finite` first: NaN compares false against `<= 0.0` and
    // would otherwise slip through.
    if channels == 0 || samples == 0 || !freq.is_finite() || freq <= 0.0 {
        return Err(Error::Waveform("empty waveform dimensions".into()));
    }
    let bits = item.uint16(tags::WAVEFORM_BITS_ALLOCATED).unwrap_or(16);
    if !matches!(bits, 8 | 16) {
        return Err(Error::Samples(format!("bits_allocated {bits}")));
    }
    let interp = match req(tags::WAVEFORM_SAMPLE_INTERPRETATION)?.trim() {
        "SS" | "SB" => SampleInterpretation::Signed,
        "US" | "UB" => SampleInterpretation::Unsigned,
        other => return Err(Error::Samples(format!("interpretation {other:?}"))),
    };
    let defs = item
        .sequence(tags::CHANNEL_DEFINITION_SEQUENCE)
        .ok_or_else(|| Error::Missing(tags::CHANNEL_DEFINITION_SEQUENCE.to_string()))?;
    if defs.len() != channels {
        return Err(Error::Waveform(format!(
            "channel definitions {} != channels {channels}",
            defs.len()
        )));
    }
    let raw = waveform_bytes(item)?;
    let bytes_per_sample = bits as usize / 8;
    // u64 math: channels/samples come from the wire and their product
    // overflows 32-bit usize.
    let want = channels as u64 * samples as u64 * bytes_per_sample as u64;
    // Accept one trailing pad byte: odd-sized 8-bit waveforms carry a
    // wire pad that must not fail the round trip (its value is
    // unchecked — encoders must write 0x00 — and it is never indexed).
    let len = raw.len() as u64;
    if len != want && len != want + 1 {
        return Err(Error::Waveform(format!(
            "waveform data {} bytes, want {want}",
            raw.len(),
        )));
    }

    let mut leads = Vec::with_capacity(channels);
    for (c, def) in defs.iter().enumerate() {
        let label = def.string(tags::CHANNEL_LABEL).unwrap_or("?").to_string();
        // `f64::from_str` accepts "NaN"/"inf", which the encoder
        // refuses to write: treat non-finite like unparsable (fall
        // back to the default) so parse/encode agree.
        let num = |tag, default: f64| {
            def.string(tag)
                .and_then(|s| s.trim().parse::<f64>().ok())
                .filter(|v| v.is_finite())
                .unwrap_or(default)
        };
        let opt_num = |tag| {
            def.string(tag)
                .and_then(|s| s.trim().parse::<f64>().ok())
                .filter(|v| v.is_finite())
        };
        let units = def
            .sequence(tags::CHANNEL_SENSITIVITY_UNITS_SEQUENCE)
            .and_then(|s| s.first()?.string(tags::CODE_VALUE))
            .unwrap_or("mV")
            .to_string();
        let mdc_code = def
            .sequence(tags::CHANNEL_SOURCE_SEQUENCE)
            .and_then(|s| s.first())
            .and_then(|s| {
                (s.string(tags::CODING_SCHEME_DESIGNATOR) == Some("MDC"))
                    .then(|| s.string(tags::CODE_VALUE).map(str::to_string))
                    .flatten()
            });
        let mut lead_samples = Vec::with_capacity(samples);
        for t in 0..samples {
            let off = (t * channels + c) * bytes_per_sample;
            let v = match (interp, bits) {
                (SampleInterpretation::Signed, 16) => {
                    i16::from_le_bytes([raw[off], raw[off + 1]]) as i32
                }
                (SampleInterpretation::Unsigned, 16) => {
                    u16::from_le_bytes([raw[off], raw[off + 1]]) as i32
                }
                (SampleInterpretation::Signed, 8) => (raw[off] as i8) as i32,
                (SampleInterpretation::Unsigned, 8) => raw[off] as i32,
                _ => unreachable!(),
            };
            lead_samples.push(v);
        }
        leads.push(crate::model::EcgLead {
            label,
            samples: lead_samples,
            sensitivity: num(tags::CHANNEL_SENSITIVITY, 1.0),
            units,
            correction_factor: num(tags::CHANNEL_SENSITIVITY_CORRECTION_FACTOR, 1.0),
            baseline: num(tags::CHANNEL_BASELINE, 0.0),
            mdc_code,
            filter_low_hz: opt_num(tags::FILTER_LOW_FREQUENCY),
            filter_high_hz: opt_num(tags::FILTER_HIGH_FREQUENCY),
            notch_hz: opt_num(tags::NOTCH_FILTER_FREQUENCY),
        });
    }

    let mut rec = EcgRecording::new(leads, freq)?;
    rec.originality = item
        .string(tags::WAVEFORM_ORIGINALITY)
        .unwrap_or("ORIGINAL")
        .to_string();
    rec.multiplex_label = item.string(tags::MULTIPLEX_GROUP_LABEL).map(str::to_string);
    rec.powerline_hz = item
        .string(tags::POWERLINE_FREQUENCY)
        .and_then(|s| s.trim().parse::<f64>().ok())
        .filter(|v| v.is_finite());
    rec.amplifier_type = item
        .string(tags::WAVEFORM_AMPLIFIER_TYPE)
        .map(str::to_string);
    rec.sample_interpretation = interp;
    rec.bits_allocated = bits;
    Ok(rec)
}

fn waveform_bytes(item: &DataSet) -> Result<Vec<u8>> {
    let el = item
        .get(tags::WAVEFORM_DATA)
        .ok_or_else(|| Error::Missing(tags::WAVEFORM_DATA.to_string()))?;
    match &el.value {
        Value::Bytes(b) => Ok(b.clone()),
        Value::Fragments(f) => Ok(f.joined()),
        _ => Err(Error::Waveform("Waveform Data has no bytes".into())),
    }
}

/// Build a complete ECG SOP instance dataset from a recording + metadata.
///
/// Every Type 1 attribute is written with a value (missing Type 1
/// datetimes are [`Error::Missing`], never fabricated) and every
/// Type 2 attribute is at least present, so the result validates
/// against the ECG IODs.
///
/// Validated here: UID syntax for all four UID fields, Patient Sex
/// (`M`/`F`/`O` when non-empty), Waveform Originality (in the item
/// builder), and the per-SOP-class channel caps (12-lead ≤ 13,
/// General ECG ≤ 12). Other free-text fields (dates, names, labels,
/// descriptions) are the caller's responsibility and are written
/// verbatim: garbage in, non-conformant out.
pub fn recording_to_dataset(rec: &EcgRecording, meta: &EcgInstanceMeta) -> Result<DataSet> {
    let mut meta = meta.clone();
    meta.fill_uids();
    for (what, uid) in [
        ("SOP Class UID", &meta.sop_class_uid),
        ("SOP Instance UID", &meta.sop_instance_uid),
        ("Study Instance UID", &meta.study_uid),
        ("Series Instance UID", &meta.series_uid),
    ] {
        if !vyna_dcm_core::is_valid_uid(uid) {
            return Err(Error::InvalidMeta(format!("bad {what} {uid:?}")));
        }
    }
    if let Some(sex) = &meta.sex
        && !sex.is_empty()
        && !matches!(sex.as_str(), "M" | "F" | "O")
    {
        return Err(Error::InvalidMeta(format!(
            "Patient Sex {sex:?} (want M, F, or O)"
        )));
    }
    // Per-SOP-class channel caps (A.34.3.4.4 / A.34.5.4.3); other
    // waveform SOP classes pass through uncapped.
    let cap = match meta.sop_class_uid.as_str() {
        SOP_12_LEAD_ECG => Some(13),
        SOP_GENERAL_ECG => Some(12),
        _ => None,
    };
    if let Some(max) = cap
        && rec.leads.len() > max
    {
        return Err(Error::InvalidMeta(format!(
            "{} leads exceed the {max}-channel cap for {}",
            rec.leads.len(),
            meta.sop_class_uid
        )));
    }
    // Type 1 datetimes: required with real values.
    for (tag, v) in [
        (tags::CONTENT_DATE, &meta.content_date),
        (tags::CONTENT_TIME, &meta.content_time),
        (tags::ACQUISITION_DATE_TIME, &meta.acquisition_datetime),
    ] {
        if v.as_ref().is_none_or(|s| s.is_empty()) {
            return Err(Error::Missing(tag.to_string()));
        }
    }
    let mut ds = DataSet::new();
    if meta.needs_utf8() || recording_needs_utf8(rec) {
        ds.put_text(tags::SPECIFIC_CHARACTER_SET, Vr::Cs, "ISO_IR 192");
    }
    ds.put_uid(tags::SOP_CLASS_UID, meta.sop_class_uid.clone());
    ds.put_uid(tags::SOP_INSTANCE_UID, meta.sop_instance_uid.clone());
    ds.put_uid(tags::STUDY_INSTANCE_UID, meta.study_uid.clone());
    ds.put_uid(tags::SERIES_INSTANCE_UID, meta.series_uid.clone());
    ds.put_text(tags::MODALITY, Vr::Cs, "ECG");
    ds.put_text(tags::MANUFACTURER, Vr::Lo, meta.manufacturer.clone());
    ds.put_text(tags::MANUFACTURER_MODEL_NAME, Vr::Lo, meta.model.clone());
    ds.put_person_name(tags::PATIENT_NAME, vec![meta.patient_name.clone()]);
    ds.put_text(tags::PATIENT_ID, Vr::Lo, meta.patient_id.clone());
    // Type 2: present with a value, else present-but-empty.
    put_optional_text(&mut ds, tags::PATIENT_BIRTH_DATE, Vr::Da, &meta.birth_date);
    put_optional_text(&mut ds, tags::PATIENT_SEX, Vr::Cs, &meta.sex);
    put_optional_text(&mut ds, tags::STUDY_ID, Vr::Sh, &meta.study_id);
    put_optional_text(&mut ds, tags::ACCESSION_NUMBER, Vr::Sh, &meta.accession);
    put_optional_text(&mut ds, tags::STUDY_DATE, Vr::Da, &meta.study_date);
    put_optional_text(&mut ds, tags::STUDY_TIME, Vr::Tm, &meta.study_time);
    ds.put_text(
        tags::CONTENT_DATE,
        Vr::Da,
        meta.content_date.clone().unwrap_or_default(),
    );
    ds.put_text(
        tags::CONTENT_TIME,
        Vr::Tm,
        meta.content_time.clone().unwrap_or_default(),
    );
    ds.put_text(
        tags::ACQUISITION_DATE_TIME,
        Vr::Dt,
        meta.acquisition_datetime.clone().unwrap_or_default(),
    );
    match &meta.referring_physician {
        Some(pn) => ds.put_person_name(tags::REFERRING_PHYSICIAN_NAME, vec![pn.clone()]),
        None => ds.put_null(tags::REFERRING_PHYSICIAN_NAME, Vr::Pn),
    }
    ds.put_sequence(
        tags::ACQUISITION_CONTEXT_SEQUENCE,
        meta.acquisition_context.clone(),
    );
    if let Some(i) = &meta.institution {
        ds.put_text(tags::INSTITUTION_NAME, Vr::Lo, i.clone());
    }
    if let Some(d) = &meta.series_description {
        ds.put_text(tags::SERIES_DESCRIPTION, Vr::Lo, d.clone());
    }
    ds.put_text(tags::SERIES_NUMBER, Vr::Is, "1");
    ds.put_text(tags::INSTANCE_NUMBER, Vr::Is, meta.instance_number.clone());
    ds.put_sequence(
        tags::WAVEFORM_SEQUENCE,
        vec![recording_to_waveform_item(rec)?],
    );
    Ok(ds)
}

fn put_optional_text(ds: &mut DataSet, tag: vyna_dcm_core::Tag, vr: Vr, v: &Option<String>) {
    match v {
        Some(s) => ds.put_text(tag, vr, s.clone()),
        None => ds.put_null(tag, vr),
    }
}

/// File meta matching an ECG instance dataset.
pub fn file_meta_for_dataset(
    ds: &DataSet,
    ts: vyna_dcm_core::TransferSyntax,
) -> Result<vyna_dcm_codec::FileMeta> {
    let sop_class = ds
        .require_string(tags::SOP_CLASS_UID)
        .map_err(|_| Error::Missing(tags::SOP_CLASS_UID.to_string()))?;
    let sop_inst = ds
        .require_string(tags::SOP_INSTANCE_UID)
        .map_err(|_| Error::Missing(tags::SOP_INSTANCE_UID.to_string()))?;
    Ok(vyna_dcm_codec::FileMeta::new(sop_class, sop_inst, ts))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{EcgLead, STANDARD_12_LEAD};

    fn twelve_lead() -> EcgRecording {
        let leads = STANDARD_12_LEAD
            .iter()
            .enumerate()
            .map(|(i, (label, code))| {
                let mut lead = EcgLead::new(
                    *label,
                    (0..500)
                        .map(|t| ((t + i as i32 * 7) % 2000) - 1000)
                        .collect(),
                    0.001,
                );
                lead.mdc_code = Some((*code).to_string());
                lead.filter_low_hz = Some(0.05);
                lead.filter_high_hz = Some(150.0);
                lead
            })
            .collect();
        let mut rec = EcgRecording::new(leads, 500.0).unwrap();
        rec.multiplex_label = Some("RESTING 12-LEAD".into());
        rec.powerline_hz = Some(50.0);
        rec
    }

    #[test]
    fn waveform_item_round_trip() {
        let rec = twelve_lead();
        let item = recording_to_waveform_item(&rec).unwrap();
        assert_eq!(item.uint16(tags::NUMBER_OF_WAVEFORM_CHANNELS), Some(12));
        assert_eq!(item.uint32(tags::NUMBER_OF_WAVEFORM_SAMPLES), Some(500));
        let back = recording_from_waveform_item(&item).unwrap();
        assert_eq!(back.leads.len(), 12);
        assert_eq!(back.sampling_frequency_hz, 500.0);
        for (a, b) in rec.leads.iter().zip(back.leads.iter()) {
            assert_eq!(a.label, b.label);
            assert_eq!(a.samples, b.samples);
            assert_eq!(a.sensitivity, b.sensitivity);
            assert_eq!(a.mdc_code, b.mdc_code);
        }
        assert_eq!(back.powerline_hz, Some(50.0));
    }

    #[test]
    fn unsigned_and_8bit_round_trip() {
        for (interp, bits) in [
            (SampleInterpretation::Unsigned, 16),
            (SampleInterpretation::Signed, 8),
            (SampleInterpretation::Unsigned, 8),
        ] {
            let max = match (interp, bits) {
                (SampleInterpretation::Signed, 16) => 30000,
                (SampleInterpretation::Unsigned, 16) => 60000,
                (SampleInterpretation::Signed, 8) => 100,
                _ => 200,
            };
            let leads = vec![EcgLead::new("II", vec![0, max, max / 2, 1], 0.001)];
            let mut rec = EcgRecording::new(leads, 250.0).unwrap();
            rec.sample_interpretation = interp;
            rec.bits_allocated = bits;
            let item = recording_to_waveform_item(&rec).unwrap();
            let back = recording_from_waveform_item(&item).unwrap();
            assert_eq!(back.sample_interpretation, interp);
            assert_eq!(back.bits_allocated, bits);
            assert_eq!(back.leads[0].samples, vec![0, max, max / 2, 1]);
        }
    }

    #[test]
    fn full_instance_dataset() {
        let rec = twelve_lead();
        let mut meta = EcgInstanceMeta::new(PersonName::parse("Doe^Jane"), "E123");
        meta.accession = Some("A456".into());
        meta.content_date = Some("20261011".into());
        meta.content_time = Some("120000".into());
        meta.acquisition_datetime = Some("20261011120000".into());
        let ds = recording_to_dataset(&rec, &meta).unwrap();
        assert_eq!(ds.string(tags::MODALITY), Some("ECG"));
        assert_eq!(ds.string(tags::SOP_CLASS_UID), Some(SOP_12_LEAD_ECG));
        assert_eq!(ds.sequence(tags::WAVEFORM_SEQUENCE).unwrap().len(), 1);
        // UIDs were generated.
        assert!(vyna_dcm_core::is_valid_uid(
            ds.string(tags::SOP_INSTANCE_UID).unwrap()
        ));
        // Type 2 shape is complete: unknown values are present-but-empty.
        for tag in [
            tags::STUDY_DATE,
            tags::STUDY_TIME,
            tags::PATIENT_BIRTH_DATE,
            tags::PATIENT_SEX,
            tags::STUDY_ID,
            tags::REFERRING_PHYSICIAN_NAME,
            tags::ACQUISITION_CONTEXT_SEQUENCE,
        ] {
            assert!(ds.get(tag).is_some(), "missing {tag}");
        }
        assert_eq!(
            ds.sequence(tags::ACQUISITION_CONTEXT_SEQUENCE)
                .unwrap()
                .len(),
            0
        );
        // File meta derives cleanly.
        let fm = file_meta_for_dataset(&ds, vyna_dcm_core::TransferSyntax::ExplicitVrLittleEndian)
            .unwrap();
        assert_eq!(fm.media_storage_sop_class_uid, SOP_12_LEAD_ECG);
    }

    #[test]
    fn instance_dataset_validates_against_its_iod() {
        let rec = twelve_lead();
        let mut meta = EcgInstanceMeta::new(PersonName::parse("Doe^Jane"), "E123");
        meta.content_date = Some("20261011".into());
        meta.content_time = Some("120000".into());
        meta.acquisition_datetime = Some("20261011120000".into());
        let ds = recording_to_dataset(&rec, &meta).unwrap();
        let report = vyna_dcm_iod::validate(&ds, SOP_12_LEAD_ECG, None).unwrap();
        assert!(!report.has_errors(), "{report:?}");
    }

    #[test]
    fn missing_type1_datetimes_are_an_error() {
        let rec = twelve_lead();
        let meta = EcgInstanceMeta::new(PersonName::parse("Doe^Jane"), "E123");
        assert!(matches!(
            recording_to_dataset(&rec, &meta),
            Err(Error::Missing(_))
        ));
    }

    #[test]
    fn rejects_bad_waveforms() {
        let empty = DataSet::new();
        assert!(recording_from_waveform_item(&empty).is_err());
        let mut bad = recording_to_waveform_item(&twelve_lead()).unwrap();
        bad.put_bytes(tags::WAVEFORM_DATA, Vr::Ow, vec![1, 2, 3]); // truncated
        assert!(recording_from_waveform_item(&bad).is_err());
    }

    #[test]
    fn rejects_misshapen_recordings() {
        // Unequal lead lengths must error, not panic the interleaver.
        let mut rec = twelve_lead();
        rec.leads[1].samples.pop();
        assert!(recording_to_waveform_item(&rec).is_err());
        // Empty leads, empty samples, and bad rates are refused too.
        let mut rec = twelve_lead();
        rec.leads.clear();
        assert!(recording_to_waveform_item(&rec).is_err());
        let mut rec = twelve_lead();
        for lead in &mut rec.leads {
            lead.samples.clear();
        }
        assert!(recording_to_waveform_item(&rec).is_err());
        for rate in [0.0, -500.0, f64::NAN, f64::INFINITY] {
            let mut rec = twelve_lead();
            rec.sampling_frequency_hz = rate;
            assert!(recording_to_waveform_item(&rec).is_err(), "{rate}");
        }
    }

    #[test]
    fn non_finite_fields_are_rejected() {
        // NaN/inf are not valid DS: never written, always an error.
        let mut rec = twelve_lead();
        rec.leads[0].sensitivity = f64::NAN;
        assert!(recording_to_waveform_item(&rec).is_err());
        let mut rec = twelve_lead();
        rec.leads[0].baseline = f64::INFINITY;
        assert!(recording_to_waveform_item(&rec).is_err());
        let mut rec = twelve_lead();
        rec.powerline_hz = Some(f64::NAN);
        assert!(recording_to_waveform_item(&rec).is_err());
        assert_eq!(ds_format(f64::NAN), None);
        assert_eq!(ds_format(f64::INFINITY), None);
        assert_eq!(ds_format(0.0).as_deref(), Some("0"));
        assert_eq!(ds_format(1.5).as_deref(), Some("1.5"));
        // Long values shrink decimals, never truncate mid-number or
        // leave a trailing dot; absurd magnitudes go scientific.
        let s = ds_format(123456789012345.6).unwrap();
        assert!(s.len() <= 16 && !s.ends_with('.'), "{s}");
        // Rounds to whole units rather than corrupting the magnitude.
        assert!((s.parse::<f64>().unwrap() - 123456789012345.6).abs() < 1.0);
        let s = ds_format(1e16).unwrap();
        assert!(s.len() <= 16, "{s}");
        assert_eq!(s.parse::<f64>().unwrap(), 1e16);
    }

    #[test]
    fn non_finite_wire_floats_parse_as_absent() {
        let mut item = recording_to_waveform_item(&twelve_lead()).unwrap();
        let mut defs = item
            .sequence(tags::CHANNEL_DEFINITION_SEQUENCE)
            .unwrap()
            .to_vec();
        defs[0].put_text(tags::CHANNEL_SENSITIVITY, Vr::Ds, "NaN");
        defs[0].put_text(tags::FILTER_LOW_FREQUENCY, Vr::Ds, "inf");
        item.put_sequence(tags::CHANNEL_DEFINITION_SEQUENCE, defs);
        item.put_text(tags::POWERLINE_FREQUENCY, Vr::Ds, "NaN");
        let back = recording_from_waveform_item(&item).unwrap();
        // Defaults/absent, mirroring unparsable strings (never NaN).
        assert_eq!(back.leads[0].sensitivity, 1.0);
        assert_eq!(back.leads[0].filter_low_hz, None);
        assert_eq!(back.powerline_hz, None);
    }

    #[test]
    fn wire_item_with_bad_frequency_is_rejected() {
        let item = recording_to_waveform_item(&twelve_lead()).unwrap();
        for bad in ["NaN", "inf", "-inf", "-500", "0", "abc"] {
            let mut item = item.clone();
            item.put_text(tags::SAMPLING_FREQUENCY, Vr::Ds, bad);
            assert!(
                recording_from_waveform_item(&item).is_err(),
                "{bad} must not parse"
            );
        }
    }

    #[test]
    fn ds_format_tiny_values_go_scientific() {
        // 1e-10 collapses to "0" in fixed point: must fall through to
        // scientific, never return a zero that loses the value.
        let s = ds_format(1e-10).unwrap();
        assert_ne!(s, "0");
        assert!((s.parse::<f64>().unwrap() - 1e-10).abs() < 1e-16, "{s}");
        assert!(s.len() <= 16, "{s}");
        // Values that fit in fixed point stay there.
        assert_eq!(ds_format(1e-7).as_deref(), Some("0.0000001"));
        // Sub-unit precision uses the full 16-char budget (past the
        // old 9-decimal cap).
        let s = ds_format(0.123456789012345).unwrap();
        assert!(s.len() <= 16, "{s}");
        assert_ne!(s, "0.123456789", "precision capped at 9 decimals: {s}");
        assert!((s.parse::<f64>().unwrap() - 0.123456789012345).abs() < 1e-12);
        // Negative tiny values behave the same.
        let s = ds_format(-2.5e-12).unwrap();
        assert!(s.parse::<f64>().unwrap() < 0.0);
        assert!((s.parse::<f64>().unwrap() - -2.5e-12).abs() < 1e-18, "{s}");
    }

    fn dated_meta() -> EcgInstanceMeta {
        let mut meta = EcgInstanceMeta::new(PersonName::parse("Doe^Jane"), "E123");
        meta.content_date = Some("20261011".into());
        meta.content_time = Some("120000".into());
        meta.acquisition_datetime = Some("20261011120000".into());
        meta
    }

    #[test]
    fn eight_bit_waveform_uses_ob_and_survives_part10() {
        // Odd total bytes (1 lead x 3 samples): the wire pad byte must
        // not fail the decode.
        let leads = vec![EcgLead::new("II", vec![0, 100, -100], 0.001)];
        let mut rec = EcgRecording::new(leads, 250.0).unwrap();
        rec.sample_interpretation = SampleInterpretation::Signed;
        rec.bits_allocated = 8;
        let item = recording_to_waveform_item(&rec).unwrap();
        assert_eq!(
            item.get(tags::WAVEFORM_DATA).unwrap().vr,
            vyna_dcm_core::Vr::Ob
        );
        let mut meta = dated_meta();
        meta.sop_class_uid = SOP_GENERAL_ECG.to_string();
        let ds = recording_to_dataset(&rec, &meta).unwrap();
        let ts = vyna_dcm_core::TransferSyntax::ExplicitVrLittleEndian;
        let bytes =
            vyna_dcm_codec::write_part10(&file_meta_for_dataset(&ds, ts).unwrap(), &ds).unwrap();
        let (_, back) = vyna_dcm_codec::read_part10(&bytes).unwrap();
        let wave = &back.sequence(tags::WAVEFORM_SEQUENCE).unwrap()[0];
        let rec2 = recording_from_waveform_item(wave).unwrap();
        assert_eq!(rec2.bits_allocated, 8);
        assert_eq!(rec2.leads[0].samples, vec![0, 100, -100]);
    }

    #[test]
    fn general_ecg_validates() {
        let leads: Vec<EcgLead> = ["I", "II", "V1", "V2"]
            .iter()
            .map(|l| EcgLead::new(*l, vec![0, 10, -10, 5], 0.001))
            .collect();
        let rec = EcgRecording::new(leads, 500.0).unwrap();
        let mut meta = dated_meta();
        meta.sop_class_uid = SOP_GENERAL_ECG.to_string();
        let ds = recording_to_dataset(&rec, &meta).unwrap();
        let report = vyna_dcm_iod::validate(&ds, SOP_GENERAL_ECG, None).unwrap();
        assert!(!report.has_errors(), "{report:?}");
    }

    #[test]
    fn non_ascii_recording_declares_utf8() {
        let mut rec = twelve_lead();
        rec.leads[0].label = "II-\u{5BFC}\u{8054}".to_string();
        rec.leads[0].units = "mV-\u{6BEB}\u{4F0F}".to_string();
        let meta = dated_meta();
        let ds = recording_to_dataset(&rec, &meta).unwrap();
        assert_eq!(ds.string(tags::SPECIFIC_CHARACTER_SET), Some("ISO_IR 192"));
        let report = vyna_dcm_iod::validate(&ds, SOP_12_LEAD_ECG, None).unwrap();
        assert!(!report.has_errors(), "{report:?}");
        // ASCII-only recording: no charset stamped.
        let ds = recording_to_dataset(&twelve_lead(), &meta).unwrap();
        assert!(ds.get(tags::SPECIFIC_CHARACTER_SET).is_none());
    }

    #[test]
    fn metadata_validation_rejects_garbage() {
        let rec = twelve_lead();
        // Bad UID syntax.
        let mut meta = dated_meta();
        meta.sop_instance_uid = "not a uid!".into();
        assert!(matches!(
            recording_to_dataset(&rec, &meta),
            Err(Error::InvalidMeta(_))
        ));
        // Bad Patient Sex.
        let mut meta = dated_meta();
        meta.sex = Some("X".into());
        assert!(matches!(
            recording_to_dataset(&rec, &meta),
            Err(Error::InvalidMeta(_))
        ));
        // Empty sex (Type 2 unknown) still passes.
        let mut meta = dated_meta();
        meta.sex = Some(String::new());
        assert!(recording_to_dataset(&rec, &meta).is_ok());
        // Bad originality.
        let mut rec = twelve_lead();
        rec.originality = "MAYBE".into();
        assert!(matches!(
            recording_to_waveform_item(&rec),
            Err(Error::InvalidMeta(_))
        ));
        // Channel caps: 12-lead <= 13, General <= 12.
        let many: Vec<EcgLead> = (0..14)
            .map(|i| EcgLead::new(format!("C{i}"), vec![0, 1], 0.001))
            .collect();
        let rec14 = EcgRecording::new(many, 500.0).unwrap();
        let meta12 = dated_meta();
        assert!(recording_to_dataset(&rec14, &meta12).is_err());
        let mut meta_gen = dated_meta();
        meta_gen.sop_class_uid = SOP_GENERAL_ECG.to_string();
        assert!(recording_to_dataset(&rec14, &meta_gen).is_err());
        // Boundaries hold: 13 on 12-lead, 12 on General.
        let thirteen: Vec<EcgLead> = (0..13)
            .map(|i| EcgLead::new(format!("C{i}"), vec![0, 1], 0.001))
            .collect();
        let rec13 = EcgRecording::new(thirteen, 500.0).unwrap();
        assert!(recording_to_dataset(&rec13, &dated_meta()).is_ok());
        assert!(recording_to_dataset(&rec13, &meta_gen).is_err());
        let twelve: Vec<EcgLead> = (0..12)
            .map(|i| EcgLead::new(format!("C{i}"), vec![0, 1], 0.001))
            .collect();
        let rec12 = EcgRecording::new(twelve, 500.0).unwrap();
        assert!(recording_to_dataset(&rec12, &meta_gen).is_ok());
        // Unknown waveform SOPs pass through uncapped.
        let mut meta_other = dated_meta();
        meta_other.sop_class_uid = "1.2.840.10008.5.1.4.1.1.9.2.2".into();
        assert!(recording_to_dataset(&rec14, &meta_other).is_ok());
    }
}
