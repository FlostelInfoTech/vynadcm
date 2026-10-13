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
    /// Content (acquisition) date/time; omit when unknown.
    pub content_date: Option<String>,
    /// Content time `HHMMSS`.
    pub content_time: Option<String>,
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
            content_date: None,
            content_time: None,
            institution: None,
            manufacturer: "Vyna".into(),
            model: "VynaECG".into(),
            series_description: None,
            instance_number: "1".into(),
        }
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
    if !rec.sampling_frequency_hz.is_finite() || rec.sampling_frequency_hz <= 0.0 {
        return Err(Error::Waveform(format!(
            "bad sampling frequency {}",
            rec.sampling_frequency_hz
        )));
    }
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
        ds_format(rec.sampling_frequency_hz),
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
            ds_format(lead.sensitivity),
        );
        ch.put_sequence(
            tags::CHANNEL_SENSITIVITY_UNITS_SEQUENCE,
            vec![units_item(&lead.units)],
        );
        ch.put_text(
            tags::CHANNEL_SENSITIVITY_CORRECTION_FACTOR,
            Vr::Ds,
            ds_format(lead.correction_factor),
        );
        ch.put_text(tags::CHANNEL_BASELINE, Vr::Ds, ds_format(lead.baseline));
        ch.put_text(tags::CHANNEL_TIME_SKEW, Vr::Ds, "0");
        ch.put_text(tags::CHANNEL_SAMPLE_SKEW, Vr::Ds, "0");
        ch.put_text(tags::CHANNEL_OFFSET, Vr::Ds, "0");
        ch.put_u16_single(tags::WAVEFORM_BITS_STORED, rec.bits_allocated);
        if let Some(f) = lead.filter_low_hz {
            ch.put_text(tags::FILTER_LOW_FREQUENCY, Vr::Ds, ds_format(f));
        }
        if let Some(f) = lead.filter_high_hz {
            ch.put_text(tags::FILTER_HIGH_FREQUENCY, Vr::Ds, ds_format(f));
        }
        if let Some(f) = lead.notch_hz {
            ch.put_text(tags::NOTCH_FILTER_FREQUENCY, Vr::Ds, ds_format(f));
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
    item.put_bytes(tags::WAVEFORM_DATA, Vr::Ow, interleave(rec)?);
    if let Some(f) = rec.powerline_hz {
        item.put_text(tags::POWERLINE_FREQUENCY, Vr::Ds, ds_format(f));
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

/// Format an `f64` as a `DS` value (≤ 16 chars, no exponent).
fn ds_format(v: f64) -> String {
    if v == 0.0 {
        return "0".into();
    }
    let mut s = format!("{v:.9}");
    while s.contains('.') && s.ends_with('0') {
        s.pop();
    }
    if s.ends_with('.') {
        s.pop();
    }
    if s.len() > 16 {
        s.truncate(16);
    }
    s
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
    if channels == 0 || samples == 0 || freq <= 0.0 {
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
    if raw.len() != channels * samples * bytes_per_sample {
        return Err(Error::Waveform(format!(
            "waveform data {} bytes, want {}",
            raw.len(),
            channels * samples * bytes_per_sample
        )));
    }

    let mut leads = Vec::with_capacity(channels);
    for (c, def) in defs.iter().enumerate() {
        let label = def.string(tags::CHANNEL_LABEL).unwrap_or("?").to_string();
        let num = |tag, default: f64| {
            def.string(tag)
                .and_then(|s| s.trim().parse::<f64>().ok())
                .unwrap_or(default)
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
            filter_low_hz: def
                .string(tags::FILTER_LOW_FREQUENCY)
                .and_then(|s| s.parse().ok()),
            filter_high_hz: def
                .string(tags::FILTER_HIGH_FREQUENCY)
                .and_then(|s| s.parse().ok()),
            notch_hz: def
                .string(tags::NOTCH_FILTER_FREQUENCY)
                .and_then(|s| s.parse().ok()),
        });
    }

    let mut rec = EcgRecording::new(leads, freq).map_err(Error::Waveform)?;
    rec.originality = item
        .string(tags::WAVEFORM_ORIGINALITY)
        .unwrap_or("ORIGINAL")
        .to_string();
    rec.multiplex_label = item.string(tags::MULTIPLEX_GROUP_LABEL).map(str::to_string);
    rec.powerline_hz = item
        .string(tags::POWERLINE_FREQUENCY)
        .and_then(|s| s.parse().ok());
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
pub fn recording_to_dataset(rec: &EcgRecording, meta: &EcgInstanceMeta) -> Result<DataSet> {
    let mut meta = meta.clone();
    meta.fill_uids();
    let mut ds = DataSet::new();
    ds.put_uid(tags::SOP_CLASS_UID, meta.sop_class_uid.clone());
    ds.put_uid(tags::SOP_INSTANCE_UID, meta.sop_instance_uid.clone());
    ds.put_uid(tags::STUDY_INSTANCE_UID, meta.study_uid.clone());
    ds.put_uid(tags::SERIES_INSTANCE_UID, meta.series_uid.clone());
    ds.put_text(tags::MODALITY, Vr::Cs, "ECG");
    ds.put_text(tags::MANUFACTURER, Vr::Lo, meta.manufacturer.clone());
    ds.put_text(tags::MANUFACTURER_MODEL_NAME, Vr::Lo, meta.model.clone());
    ds.put_person_name(tags::PATIENT_NAME, vec![meta.patient_name.clone()]);
    ds.put_text(tags::PATIENT_ID, Vr::Lo, meta.patient_id.clone());
    if let Some(d) = &meta.birth_date {
        ds.put_text(tags::PATIENT_BIRTH_DATE, Vr::Da, d.clone());
    }
    if let Some(s) = &meta.sex {
        ds.put_text(tags::PATIENT_SEX, Vr::Cs, s.clone());
    }
    if let Some(s) = &meta.study_id {
        ds.put_text(tags::STUDY_ID, Vr::Sh, s.clone());
    }
    if let Some(a) = &meta.accession {
        ds.put_text(tags::ACCESSION_NUMBER, Vr::Sh, a.clone());
    }
    if let Some(d) = &meta.content_date {
        ds.put_text(tags::CONTENT_DATE, Vr::Da, d.clone());
    }
    if let Some(t) = &meta.content_time {
        ds.put_text(tags::CONTENT_TIME, Vr::Tm, t.clone());
    }
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
        let ds = recording_to_dataset(&rec, &meta).unwrap();
        assert_eq!(ds.string(tags::MODALITY), Some("ECG"));
        assert_eq!(ds.string(tags::SOP_CLASS_UID), Some(SOP_12_LEAD_ECG));
        assert_eq!(ds.sequence(tags::WAVEFORM_SEQUENCE).unwrap().len(), 1);
        // UIDs were generated.
        assert!(vyna_dcm_core::is_valid_uid(
            ds.string(tags::SOP_INSTANCE_UID).unwrap()
        ));
        // File meta derives cleanly.
        let fm = file_meta_for_dataset(&ds, vyna_dcm_core::TransferSyntax::ExplicitVrLittleEndian)
            .unwrap();
        assert_eq!(fm.media_storage_sop_class_uid, SOP_12_LEAD_ECG);
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
}
