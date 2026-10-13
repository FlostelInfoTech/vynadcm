//! Encapsulated PDF Storage SOP instances (NEMA PS3.3 A.45).
//!
//! [`pdf_to_dataset`] builds a complete, validatable SOP instance from
//! PDF bytes plus [`PdfInstanceMeta`]; [`pdf_from_dataset`] extracts the
//! bytes back out. Every Type 2 attribute of the IOD is always present
//! (empty when unknown), so built instances validate clean with
//! `vyna-dcm-iod` and are directly C-STORE-able.

use vyna_dcm_core::{DataSet, PersonName, Value, Vr, sop_class, tags};

use crate::{Error, Result};

/// MIME type stored in every instance built here.
pub const MIME_TYPE_PDF: &str = "application/pdf";
/// `Specific Character Set` value used when text needs non-ASCII (UTF-8).
pub const CHARSET_UTF8: &str = "ISO_IR 192";

/// Coded concept for Concept Name Code Sequence (0040,A043).
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct ConceptName {
    /// Code Value (e.g. `"18748-4"`).
    pub value: String,
    /// Coding Scheme Designator (e.g. `"LN"`).
    pub scheme: String,
    /// Code Meaning (e.g. `"Diagnostic Imaging Report"`).
    pub meaning: String,
}

impl ConceptName {
    /// New coded concept.
    pub fn new(
        value: impl Into<String>,
        scheme: impl Into<String>,
        meaning: impl Into<String>,
    ) -> Self {
        Self {
            value: value.into(),
            scheme: scheme.into(),
            meaning: meaning.into(),
        }
    }
}

/// Identifying metadata for a complete Encapsulated PDF SOP instance.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct PdfInstanceMeta {
    /// SOP Instance UID (generated when empty).
    pub sop_instance_uid: String,
    /// Study Instance UID (generated when empty).
    pub study_uid: String,
    /// Series Instance UID (generated when empty).
    pub series_uid: String,
    /// Patient name.
    pub patient_name: PersonName,
    /// Patient ID (Type 2: kept even when empty).
    pub patient_id: String,
    /// Patient birth date `YYYYMMDD`.
    pub birth_date: Option<String>,
    /// Patient sex (`M`/`F`/`O`).
    pub sex: Option<String>,
    /// Study ID.
    pub study_id: Option<String>,
    /// Accession number.
    pub accession: Option<String>,
    /// Study date `YYYYMMDD`.
    pub study_date: Option<String>,
    /// Study time `HHMMSS`.
    pub study_time: Option<String>,
    /// Content (document creation) date `YYYYMMDD`.
    pub content_date: Option<String>,
    /// Content time `HHMMSS`.
    pub content_time: Option<String>,
    /// Acquisition date/time `YYYYMMDDHHMMSS`.
    pub acquisition_datetime: Option<String>,
    /// Referring physician's name.
    pub referring_physician: Option<PersonName>,
    /// Document title.
    pub title: Option<String>,
    /// Document concept name (empty sequence when absent).
    pub concept_name: Option<ConceptName>,
    /// Modality; default `OT`.
    pub modality: String,
    /// Conversion Type; default `WSD` (workstation).
    pub conversion_type: String,
    /// Burned In Annotation; default `YES` (reports carry patient text).
    pub burned_in_annotation: String,
    /// Institution name.
    pub institution: Option<String>,
    /// Manufacturer; default `Vyna`.
    pub manufacturer: String,
    /// Manufacturer model; default `VynaDCM`.
    pub model: String,
    /// Series description.
    pub series_description: Option<String>,
    /// Series number.
    pub series_number: String,
    /// Instance number.
    pub instance_number: String,
}

impl PdfInstanceMeta {
    /// Minimal metadata; UIDs are generated when left empty.
    pub fn new(patient_name: PersonName, patient_id: impl Into<String>) -> Self {
        Self {
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
            title: None,
            concept_name: None,
            modality: "OT".into(),
            conversion_type: "WSD".into(),
            burned_in_annotation: "YES".into(),
            institution: None,
            manufacturer: "Vyna".into(),
            model: "VynaDCM".into(),
            series_description: None,
            series_number: "1".into(),
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
                &self.title,
                &self.institution,
                &self.series_description,
            ]
            .into_iter()
            .flatten()
            .any(|s| !s.is_ascii())
            || [
                &self.modality,
                &self.conversion_type,
                &self.burned_in_annotation,
                &self.manufacturer,
                &self.model,
                &self.series_number,
                &self.instance_number,
            ]
            .into_iter()
            .any(|s| !s.is_ascii())
            || self.concept_name.as_ref().is_some_and(|c| {
                [&c.value, &c.scheme, &c.meaning]
                    .into_iter()
                    .any(|s| !s.is_ascii())
            })
    }
}

/// Build a complete Encapsulated PDF SOP instance dataset.
///
/// All Type 1 attributes get real values; all Type 2 attributes are
/// present (empty when the metadata omits them); Specific Character Set
/// is added only when needed. The exact byte length is recorded in
/// Encapsulated Document Length (0042,0015) so [`pdf_from_dataset`]
/// can strip wire padding exactly.
///
/// Validated here: UID syntax for the three UID fields, Patient Sex
/// (`M`/`F`/`O` when non-empty), Burned In Annotation (`YES`/`NO`),
/// and the non-empty Type 1 modality/conversion-type/series/instance
/// numbers. Other caller strings (dates, titles) are written verbatim:
/// garbage in, non-conformant out.
pub fn pdf_to_dataset(pdf: &[u8], meta: &PdfInstanceMeta) -> Result<DataSet> {
    if pdf.is_empty() {
        return Err(Error::EmptyDocument);
    }
    if !pdf.starts_with(b"%PDF-") {
        return Err(Error::NotPdf);
    }
    let len_u32 =
        u32::try_from(pdf.len()).map_err(|_| Error::DocumentTooLarge(pdf.len() as u64))?;
    let mut meta = meta.clone();
    meta.fill_uids();
    for (what, uid) in [
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
    if !matches!(meta.burned_in_annotation.as_str(), "YES" | "NO") {
        return Err(Error::InvalidMeta(format!(
            "Burned In Annotation {:?} (want YES or NO)",
            meta.burned_in_annotation
        )));
    }
    // Type 1 caller strings: required with real values (defaults are
    // non-empty but a caller can empty them).
    for (tag, v) in [
        (tags::MODALITY, &meta.modality),
        (tags::CONVERSION_TYPE, &meta.conversion_type),
        (tags::SERIES_NUMBER, &meta.series_number),
        (tags::INSTANCE_NUMBER, &meta.instance_number),
    ] {
        if v.is_empty() {
            return Err(Error::Missing(tag.to_string()));
        }
    }
    let mut ds = DataSet::new();
    if meta.needs_utf8() {
        ds.put_text(tags::SPECIFIC_CHARACTER_SET, Vr::Cs, CHARSET_UTF8);
    }
    ds.put_uid(tags::SOP_CLASS_UID, sop_class::ENCAPSULATED_PDF);
    ds.put_uid(tags::SOP_INSTANCE_UID, meta.sop_instance_uid.clone());
    ds.put_uid(tags::STUDY_INSTANCE_UID, meta.study_uid.clone());
    ds.put_uid(tags::SERIES_INSTANCE_UID, meta.series_uid.clone());
    ds.put_text(tags::MODALITY, Vr::Cs, meta.modality.clone());
    ds.put_text(tags::CONVERSION_TYPE, Vr::Cs, meta.conversion_type.clone());
    ds.put_text(
        tags::BURNED_IN_ANNOTATION,
        Vr::Cs,
        meta.burned_in_annotation.clone(),
    );
    ds.put_text(
        tags::MIME_TYPE_OF_ENCAPSULATED_DOCUMENT,
        Vr::Lo,
        MIME_TYPE_PDF,
    );
    ds.put_bytes(tags::ENCAPSULATED_DOCUMENT, Vr::Ob, pdf.to_vec());
    ds.put_u32_single(tags::ENCAPSULATED_DOCUMENT_LENGTH, len_u32);
    ds.put_text(tags::SERIES_NUMBER, Vr::Is, meta.series_number.clone());
    ds.put_text(tags::INSTANCE_NUMBER, Vr::Is, meta.instance_number.clone());
    ds.put_person_name(tags::PATIENT_NAME, vec![meta.patient_name.clone()]);
    ds.put_text(tags::PATIENT_ID, Vr::Lo, meta.patient_id.clone());
    ds.put_text(tags::MANUFACTURER, Vr::Lo, meta.manufacturer.clone());
    ds.put_text(tags::MANUFACTURER_MODEL_NAME, Vr::Lo, meta.model.clone());
    // Type 2: present with a value, else present-but-empty.
    put_optional_text(&mut ds, tags::STUDY_DATE, Vr::Da, &meta.study_date);
    put_optional_text(&mut ds, tags::STUDY_TIME, Vr::Tm, &meta.study_time);
    put_optional_text(&mut ds, tags::ACCESSION_NUMBER, Vr::Sh, &meta.accession);
    put_optional_text(&mut ds, tags::STUDY_ID, Vr::Sh, &meta.study_id);
    put_optional_text(&mut ds, tags::CONTENT_DATE, Vr::Da, &meta.content_date);
    put_optional_text(&mut ds, tags::CONTENT_TIME, Vr::Tm, &meta.content_time);
    put_optional_text(
        &mut ds,
        tags::ACQUISITION_DATE_TIME,
        Vr::Dt,
        &meta.acquisition_datetime,
    );
    put_optional_text(&mut ds, tags::PATIENT_BIRTH_DATE, Vr::Da, &meta.birth_date);
    put_optional_text(&mut ds, tags::PATIENT_SEX, Vr::Cs, &meta.sex);
    put_optional_text(&mut ds, tags::DOCUMENT_TITLE, Vr::St, &meta.title);
    match &meta.referring_physician {
        Some(pn) => ds.put_person_name(tags::REFERRING_PHYSICIAN_NAME, vec![pn.clone()]),
        None => ds.put_null(tags::REFERRING_PHYSICIAN_NAME, Vr::Pn),
    }
    match &meta.concept_name {
        Some(c) => {
            let mut item = DataSet::new();
            item.put_text(tags::CODE_VALUE, Vr::Sh, c.value.clone());
            item.put_text(tags::CODING_SCHEME_DESIGNATOR, Vr::Sh, c.scheme.clone());
            item.put_text(tags::CODE_MEANING, Vr::Lo, c.meaning.clone());
            ds.put_sequence(tags::CONCEPT_NAME_CODE_SEQUENCE, vec![item]);
        }
        None => ds.put_sequence(tags::CONCEPT_NAME_CODE_SEQUENCE, Vec::new()),
    }
    // Type 3 (kept when supplied).
    if let Some(i) = &meta.institution {
        ds.put_text(tags::INSTITUTION_NAME, Vr::Lo, i.clone());
    }
    if let Some(d) = &meta.series_description {
        ds.put_text(tags::SERIES_DESCRIPTION, Vr::Lo, d.clone());
    }
    Ok(ds)
}

fn put_optional_text(ds: &mut DataSet, tag: vyna_dcm_core::Tag, vr: Vr, v: &Option<String>) {
    match v {
        Some(s) => ds.put_text(tag, vr, s.clone()),
        None => ds.put_null(tag, vr),
    }
}

/// A PDF extracted from a dataset, with its document metadata.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct ExtractedPdf {
    /// The PDF bytes, exactly (wire padding stripped).
    pub bytes: Vec<u8>,
    /// MIME Type of Encapsulated Document, when present.
    pub mime_type: Option<String>,
    /// Document Title, when present.
    pub title: Option<String>,
    /// SOP Instance UID, when present.
    pub sop_instance_uid: Option<String>,
}

/// Extract the encapsulated PDF from a dataset.
///
/// Fragmented storage is joined; Encapsulated Document Length (0042,0015)
/// truncates wire padding when present and sane. The result must carry
/// the `%PDF-` magic (the MIME type tag is reported, not enforced).
pub fn pdf_from_dataset(ds: &DataSet) -> Result<ExtractedPdf> {
    let el = ds
        .get(tags::ENCAPSULATED_DOCUMENT)
        .ok_or_else(|| Error::Missing(tags::ENCAPSULATED_DOCUMENT.to_string()))?;
    let mut bytes = match &el.value {
        Value::Bytes(b) => b.clone(),
        Value::Fragments(f) => f.joined(),
        _ => return Err(Error::NoDocumentBytes),
    };
    // A zero length with stored bytes is contradictory, not an
    // empty document: only truncate to a positive sane length.
    if let Some(len) = ds.uint32(tags::ENCAPSULATED_DOCUMENT_LENGTH)
        && len > 0
        && (len as usize) <= bytes.len()
    {
        bytes.truncate(len as usize);
    }
    // Serving arbitrary stored bytes as `application/pdf` would
    // mislabel them; refuse instead (mirrors the builder's check).
    if !bytes.starts_with(b"%PDF-") {
        return Err(Error::NotPdf);
    }
    Ok(ExtractedPdf {
        bytes,
        mime_type: ds
            .string(tags::MIME_TYPE_OF_ENCAPSULATED_DOCUMENT)
            .map(str::to_string),
        title: ds.string(tags::DOCUMENT_TITLE).map(str::to_string),
        sop_instance_uid: ds.string(tags::SOP_INSTANCE_UID).map(str::to_string),
    })
}

/// File meta matching a PDF instance dataset.
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
    use vyna_dcm_core::{DataElement, Vr, is_valid_uid};

    /// Minimal PDF-ish bytes (even length).
    fn sample_pdf() -> Vec<u8> {
        b"%PDF-1.4\n1 0 obj<</>>endobj\ntrailer<</Root 1 0 R>>\n%%EOF\n".to_vec()
    }

    fn sample_meta() -> PdfInstanceMeta {
        let mut meta = PdfInstanceMeta::new(PersonName::parse("Doe^Jane"), "P123");
        meta.title = Some("ECG Report".into());
        meta.content_date = Some("20261011".into());
        meta.content_time = Some("120000".into());
        meta
    }

    #[test]
    fn empty_type1_caller_strings_rejected() {
        let pdf = sample_pdf();
        for field in [
            "modality",
            "conversion_type",
            "series_number",
            "instance_number",
        ] {
            let mut meta = sample_meta();
            match field {
                "modality" => meta.modality.clear(),
                "conversion_type" => meta.conversion_type.clear(),
                "series_number" => meta.series_number.clear(),
                _ => meta.instance_number.clear(),
            }
            let err = pdf_to_dataset(&pdf, &meta).unwrap_err();
            assert!(matches!(err, Error::Missing(_)), "{field}: {err:?}");
        }
    }

    #[test]
    fn build_extract_round_trip() {
        let pdf = sample_pdf();
        let ds = pdf_to_dataset(&pdf, &sample_meta()).unwrap();
        assert_eq!(
            ds.string(tags::SOP_CLASS_UID),
            Some(sop_class::ENCAPSULATED_PDF)
        );
        assert_eq!(ds.string(tags::MODALITY), Some("OT"));
        assert_eq!(ds.string(tags::CONVERSION_TYPE), Some("WSD"));
        assert_eq!(ds.string(tags::BURNED_IN_ANNOTATION), Some("YES"));
        assert_eq!(
            ds.string(tags::MIME_TYPE_OF_ENCAPSULATED_DOCUMENT),
            Some(MIME_TYPE_PDF)
        );
        for tag in [
            tags::SOP_INSTANCE_UID,
            tags::STUDY_INSTANCE_UID,
            tags::SERIES_INSTANCE_UID,
        ] {
            let uid = ds.require_string(tag).unwrap();
            assert!(is_valid_uid(uid), "{tag} not a valid UID: {uid}");
        }
        // Type 2 present-but-empty when unknown.
        assert!(ds.contains(tags::STUDY_DATE));
        assert!(ds.string(tags::STUDY_DATE).is_none_or(|s| s.is_empty()));
        assert!(
            ds.sequence(tags::CONCEPT_NAME_CODE_SEQUENCE)
                .is_some_and(|s| s.is_empty())
        );
        // No charset needed for ASCII-only content.
        assert!(!ds.contains(tags::SPECIFIC_CHARACTER_SET));

        let back = pdf_from_dataset(&ds).unwrap();
        assert_eq!(back.bytes, pdf);
        assert_eq!(back.mime_type.as_deref(), Some(MIME_TYPE_PDF));
        assert_eq!(back.title.as_deref(), Some("ECG Report"));
        assert_eq!(
            back.sop_instance_uid.as_deref(),
            ds.string(tags::SOP_INSTANCE_UID)
        );
    }

    #[test]
    fn odd_length_pdf_survives_part10() {
        let mut pdf = sample_pdf();
        if pdf.len().is_multiple_of(2) {
            pdf.push(b'%'); // force odd length -> wire padding applies
        }
        assert!(!pdf.len().is_multiple_of(2));
        let ds = pdf_to_dataset(&pdf, &sample_meta()).unwrap();
        let meta =
            file_meta_for_dataset(&ds, vyna_dcm_core::TransferSyntax::ExplicitVrLittleEndian)
                .unwrap();
        let bytes = vyna_dcm_codec::write_part10(&meta, &ds).unwrap();
        let (_, ds2) = vyna_dcm_codec::read_part10(&bytes).unwrap();
        // Stored value carries the pad byte; extraction strips it exactly.
        assert_eq!(
            ds2.bytes(tags::ENCAPSULATED_DOCUMENT).unwrap().len(),
            pdf.len() + 1
        );
        assert_eq!(pdf_from_dataset(&ds2).unwrap().bytes, pdf);
    }

    #[test]
    fn round_trips_all_native_syntaxes() {
        use vyna_dcm_core::TransferSyntax as Ts;
        let pdf = sample_pdf();
        let ds = pdf_to_dataset(&pdf, &sample_meta()).unwrap();
        for ts in [
            Ts::ExplicitVrLittleEndian,
            Ts::ImplicitVrLittleEndian,
            Ts::DeflatedExplicitVrLittleEndian,
        ] {
            let meta = file_meta_for_dataset(&ds, ts).unwrap();
            let bytes = vyna_dcm_codec::write_part10(&meta, &ds).unwrap();
            let (_, ds2) = vyna_dcm_codec::read_part10(&bytes).unwrap();
            assert_eq!(pdf_from_dataset(&ds2).unwrap().bytes, pdf, "ts {ts:?}");
            let report =
                vyna_dcm_iod::validate(&ds2, sop_class::ENCAPSULATED_PDF, Some(ts)).unwrap();
            assert!(
                report.is_clean(),
                "ts {ts:?}: {:?}",
                report
                    .findings
                    .iter()
                    .map(|f| f.to_string())
                    .collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn built_instance_validates_clean() {
        use vyna_dcm_core::TransferSyntax as Ts;
        let pdf = sample_pdf();
        let ds = pdf_to_dataset(&pdf, &sample_meta()).unwrap();
        let report = vyna_dcm_iod::validate(
            &ds,
            sop_class::ENCAPSULATED_PDF,
            Some(Ts::ExplicitVrLittleEndian),
        )
        .unwrap();
        assert!(
            report.is_clean(),
            "{:?}",
            report
                .findings
                .iter()
                .map(|f| f.to_string())
                .collect::<Vec<_>>()
        );
        // And after a real Part-10 round-trip (wire VR resolution, padding).
        let meta = file_meta_for_dataset(&ds, Ts::ExplicitVrLittleEndian).unwrap();
        let bytes = vyna_dcm_codec::write_part10(&meta, &ds).unwrap();
        let (_, ds2) = vyna_dcm_codec::read_part10(&bytes).unwrap();
        let report = vyna_dcm_iod::validate(
            &ds2,
            sop_class::ENCAPSULATED_PDF,
            Some(Ts::ExplicitVrLittleEndian),
        )
        .unwrap();
        assert!(
            report.is_clean(),
            "{:?}",
            report
                .findings
                .iter()
                .map(|f| f.to_string())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn non_ascii_patient_adds_utf8_charset() {
        let pdf = sample_pdf();
        let mut meta = sample_meta();
        meta.patient_name = PersonName::parse("山田^太郎");
        let ds = pdf_to_dataset(&pdf, &meta).unwrap();
        assert_eq!(ds.string(tags::SPECIFIC_CHARACTER_SET), Some(CHARSET_UTF8));
        let file = vyna_dcm_codec::write_part10(
            &file_meta_for_dataset(&ds, vyna_dcm_core::TransferSyntax::ExplicitVrLittleEndian)
                .unwrap(),
            &ds,
        )
        .unwrap();
        let (_, ds2) = vyna_dcm_codec::read_part10(&file).unwrap();
        let report = vyna_dcm_iod::validate(
            &ds2,
            sop_class::ENCAPSULATED_PDF,
            Some(vyna_dcm_core::TransferSyntax::ExplicitVrLittleEndian),
        )
        .unwrap();
        assert!(
            report.is_clean(),
            "{:?}",
            report
                .findings
                .iter()
                .map(|f| f.to_string())
                .collect::<Vec<_>>()
        );
        assert_eq!(pdf_from_dataset(&ds2).unwrap().bytes, pdf);
    }

    #[test]
    fn concept_name_item() {
        let pdf = sample_pdf();
        let mut meta = sample_meta();
        meta.concept_name = Some(ConceptName::new(
            "18748-4",
            "LN",
            "Diagnostic Imaging Report",
        ));
        let ds = pdf_to_dataset(&pdf, &meta).unwrap();
        let items = ds.sequence(tags::CONCEPT_NAME_CODE_SEQUENCE).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].string(tags::CODE_VALUE), Some("18748-4"));
        assert_eq!(items[0].string(tags::CODING_SCHEME_DESIGNATOR), Some("LN"));
        assert_eq!(
            items[0].string(tags::CODE_MEANING),
            Some("Diagnostic Imaging Report")
        );
    }

    #[test]
    fn supplied_uids_are_kept() {
        let pdf = sample_pdf();
        let mut meta = sample_meta();
        meta.sop_instance_uid = "1.2.3.4.5".into();
        meta.study_uid = "1.2.3.4".into();
        meta.series_uid = "1.2.3.4.1".into();
        let ds = pdf_to_dataset(&pdf, &meta).unwrap();
        assert_eq!(ds.string(tags::SOP_INSTANCE_UID), Some("1.2.3.4.5"));
        assert_eq!(ds.string(tags::STUDY_INSTANCE_UID), Some("1.2.3.4"));
        assert_eq!(ds.string(tags::SERIES_INSTANCE_UID), Some("1.2.3.4.1"));
    }

    #[test]
    fn rejects_empty_pdf() {
        assert!(matches!(
            pdf_to_dataset(&[], &sample_meta()),
            Err(Error::EmptyDocument)
        ));
    }

    #[test]
    fn rejects_non_pdf_bytes() {
        assert!(matches!(
            pdf_to_dataset(b"\x89PNG\r\n\x1a\nxxxx", &sample_meta()),
            Err(Error::NotPdf)
        ));
    }

    #[test]
    fn extract_joins_fragments() {
        let pdf = sample_pdf();
        let mut ds = DataSet::new();
        ds.insert(DataElement::new(
            tags::ENCAPSULATED_DOCUMENT,
            Vr::Ob,
            Value::Fragments(vyna_dcm_core::FragmentSequence {
                basic_offset_table: Vec::new(),
                fragments: vec![pdf[..10].to_vec(), pdf[10..].to_vec()],
            }),
        ));
        assert_eq!(pdf_from_dataset(&ds).unwrap().bytes, pdf);
    }

    #[test]
    fn extract_without_length_returns_stored_bytes() {
        // Foreign file, no (0042,0015): never guess, never strip.
        let mut stored = sample_pdf();
        stored.push(0x00);
        let mut ds = DataSet::new();
        ds.put_bytes(tags::ENCAPSULATED_DOCUMENT, Vr::Ob, stored.clone());
        assert_eq!(pdf_from_dataset(&ds).unwrap().bytes, stored);
        // Insane length (longer than the data): ignore it the same way.
        ds.put_u32_single(
            tags::ENCAPSULATED_DOCUMENT_LENGTH,
            stored.len() as u32 + 100,
        );
        assert_eq!(pdf_from_dataset(&ds).unwrap().bytes, stored);
        // Zero length with stored bytes is contradictory, not empty:
        // keep the stored bytes instead of truncating to nothing.
        ds.put_u32_single(tags::ENCAPSULATED_DOCUMENT_LENGTH, 0);
        assert_eq!(pdf_from_dataset(&ds).unwrap().bytes, stored);
    }

    #[test]
    fn extract_errors() {
        let ds = DataSet::new();
        assert!(matches!(pdf_from_dataset(&ds), Err(Error::Missing(_))));
        let mut ds = DataSet::new();
        ds.put_text(tags::ENCAPSULATED_DOCUMENT, Vr::Ob, "not bytes");
        assert!(matches!(pdf_from_dataset(&ds), Err(Error::NoDocumentBytes)));
        assert!(matches!(
            file_meta_for_dataset(
                &DataSet::new(),
                vyna_dcm_core::TransferSyntax::ExplicitVrLittleEndian
            ),
            Err(Error::Missing(_))
        ));
    }

    #[test]
    fn extract_rejects_non_pdf_bytes() {
        let mut ds = DataSet::new();
        ds.put_uid(tags::SOP_CLASS_UID, sop_class::ENCAPSULATED_PDF);
        ds.put_bytes(tags::ENCAPSULATED_DOCUMENT, Vr::Ob, b"plain text".to_vec());
        assert!(matches!(pdf_from_dataset(&ds), Err(Error::NotPdf)));
    }

    #[test]
    fn builder_validates_uids_and_enums() {
        let pdf = sample_pdf();
        let mut meta = sample_meta();
        meta.study_uid = "bad uid".into();
        assert!(matches!(
            pdf_to_dataset(&pdf, &meta),
            Err(Error::InvalidMeta(_))
        ));
        let mut meta = sample_meta();
        meta.sex = Some("Q".into());
        assert!(matches!(
            pdf_to_dataset(&pdf, &meta),
            Err(Error::InvalidMeta(_))
        ));
        let mut meta = sample_meta();
        meta.burned_in_annotation = "MAYBE".into();
        assert!(matches!(
            pdf_to_dataset(&pdf, &meta),
            Err(Error::InvalidMeta(_))
        ));
        // Valid values still build.
        assert!(pdf_to_dataset(&pdf, &sample_meta()).is_ok());
    }
}
