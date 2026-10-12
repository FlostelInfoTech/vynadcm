//! DICOMDIR media directory read/create (NEMA PS3.10/11/12).
//!
//! [`read_dicomdir`] parses a DICOMDIR file into a patient → study →
//! series → instance tree by following the offset links exactly (loops
//! and dangling offsets are errors; misplaced or unusable records are
//! collected in [`DicomDir::skipped`], never silently dropped).
//! [`build_dicomdir`] writes a DICOMDIR for referenced files, copying
//! the standard record keys from their datasets and computing exact
//! byte offsets in two measured passes.
//!
//! Multi-referenced records (`MRDR`) are reported as skipped rather
//! than followed.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use vyna_dcm_core::{DataElement, DataSet, Tag, TransferSyntax, Vr, sop_class, tags};

use crate::{
    Error, FileMeta, Result, decode_dataset_capture_items, decode_up_to,
    encode_dataset_capture_items, meta_section_len, write_part10,
};

/// Default File-set ID when the caller supplies none.
pub const DEFAULT_FILE_SET_ID: &str = "VYNADCM";

/// In-use flag value for live records.
const IN_USE: u16 = 0xFFFF;

// Short aliases for the marathon tag names.
use tags::{
    DIRECTORY_RECORD_SEQUENCE as SEQ, DIRECTORY_RECORD_TYPE as REC_TYPE,
    FILE_SET_CONSISTENCY_FLAG as CONSISTENCY, FILE_SET_ID as FILESET,
    OFFSET_OF_REFERENCED_LOWER_LEVEL_DIRECTORY_ENTITY as LOWER,
    OFFSET_OF_THE_FIRST_DIRECTORY_RECORD_OF_THE_ROOT_DIRECTORY_ENTITY as FIRST,
    OFFSET_OF_THE_LAST_DIRECTORY_RECORD_OF_THE_ROOT_DIRECTORY_ENTITY as LAST,
    OFFSET_OF_THE_NEXT_DIRECTORY_RECORD as NEXT, PRIVATE_RECORD_UID as PRIV_UID,
    RECORD_IN_USE_FLAG as IN_USE_FLAG, REFERENCED_FILE_ID as FILE_ID,
    REFERENCED_SOP_CLASS_UID_IN_FILE as REF_CLASS, REFERENCED_SOP_INSTANCE_UID_IN_FILE as REF_INST,
    REFERENCED_TRANSFER_SYNTAX_UID_IN_FILE as REF_TS,
};

/// One skipped record: present in the file but unusable or unreachable.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SkippedRecord {
    /// Absolute file offset of the record's item tag.
    pub offset: usize,
    /// Directory Record Type (empty when unreadable).
    pub record_type: String,
    /// Why it was skipped.
    pub reason: String,
}

/// One referenced instance (leaf record with a file).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirInstance {
    /// Directory Record Type (`IMAGE`, `RT DOSE`, …).
    pub record_type: String,
    /// Referenced SOP Class UID.
    pub sop_class_uid: String,
    /// Referenced SOP Instance UID.
    pub sop_uid: String,
    /// Transfer Syntax UID of the referenced file.
    pub ts_uid: String,
    /// Instance Number, when the record carries one.
    pub instance_number: Option<String>,
    /// Referenced File ID components (path segments below the media root).
    pub file_id: Vec<String>,
}

impl DirInstance {
    /// File path relative to the media root (`DICOMDIR` directory).
    #[must_use]
    pub fn relative_path(&self) -> PathBuf {
        self.file_id.iter().collect()
    }
}

/// One series node with its instances.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DirSeries {
    /// Series Instance UID.
    pub uid: Option<String>,
    /// Series Number.
    pub number: Option<String>,
    /// Modality.
    pub modality: Option<String>,
    /// Series Description.
    pub description: Option<String>,
    /// Instance records.
    pub instances: Vec<DirInstance>,
}

/// One study node with its series.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DirStudy {
    /// Study Instance UID.
    pub uid: Option<String>,
    /// Study ID.
    pub id: Option<String>,
    /// Study Date.
    pub date: Option<String>,
    /// Study Time.
    pub time: Option<String>,
    /// Accession Number.
    pub accession: Option<String>,
    /// Study Description.
    pub description: Option<String>,
    /// Series records.
    pub series: Vec<DirSeries>,
}

/// One patient node with its studies.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DirPatient {
    /// Patient's Name.
    pub name: Option<String>,
    /// Patient ID.
    pub id: Option<String>,
    /// Patient's Birth Date.
    pub birth_date: Option<String>,
    /// Patient's Sex.
    pub sex: Option<String>,
    /// Study records.
    pub studies: Vec<DirStudy>,
}

/// A parsed DICOMDIR file-set.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DicomDir {
    /// File-set ID.
    pub file_set_id: Option<String>,
    /// Patient records in file order.
    pub patients: Vec<DirPatient>,
    /// Records present but unusable or unreachable.
    pub skipped: Vec<SkippedRecord>,
}

/// One flattened file reference with its parent nodes.
#[derive(Clone, Debug)]
pub struct DirFile<'a> {
    /// Parent patient node.
    pub patient: &'a DirPatient,
    /// Parent study node.
    pub study: &'a DirStudy,
    /// Parent series node.
    pub series: &'a DirSeries,
    /// The instance itself.
    pub instance: &'a DirInstance,
}

impl DicomDir {
    /// All referenced files in tree order.
    #[must_use]
    pub fn files(&self) -> Vec<DirFile<'_>> {
        let mut out = Vec::new();
        for patient in &self.patients {
            for study in &patient.studies {
                for series in &study.series {
                    for instance in &series.instances {
                        out.push(DirFile {
                            patient,
                            study,
                            series,
                            instance,
                        });
                    }
                }
            }
        }
        out
    }
}

/// Read a DICOMDIR file into a [`DicomDir`] tree.
pub fn read_dicomdir(bytes: &[u8]) -> Result<DicomDir> {
    if bytes.len() < crate::PREFIX_LEN {
        return Err(Error::NotPart10("file shorter than 132-byte prefix"));
    }
    if &bytes[128..132] != b"DICM" {
        return Err(Error::NotPart10("missing `DICM` magic at offset 128"));
    }
    let body = &bytes[crate::PREFIX_LEN..];
    let stop = Tag::new(0x0003, 0x0000);
    let (meta_ds, meta_len) =
        decode_up_to(body, TransferSyntax::ExplicitVrLittleEndian, Some(stop))?;
    let meta = FileMeta::from_dataset(&meta_ds)?;
    if meta
        .media_storage_sop_class_uid
        .trim_end_matches(['\0', ' '])
        != sop_class::MEDIA_STORAGE_DIRECTORY
    {
        return Err(Error::BadFileMeta(format!(
            "not a DICOMDIR (SOP Class {})",
            meta.media_storage_sop_class_uid
        )));
    }
    let ts = meta.transfer_syntax()?;
    let dataset_bytes = &body[meta_len..];
    let dataset_start = crate::PREFIX_LEN + meta_len;
    let (ds, rel_offsets) = decode_dataset_capture_items(dataset_bytes, ts, SEQ)?;
    let items = ds.sequence(SEQ).unwrap_or(&[]);
    if rel_offsets.len() != items.len() {
        return Err(Error::BadValue {
            tag: SEQ,
            vr: Vr::Sq,
            message: "directory records are nested or duplicated; links are ambiguous".into(),
        });
    }
    let abs: Vec<usize> = rel_offsets.iter().map(|r| dataset_start + r).collect();
    let by_offset: HashMap<usize, usize> = abs.iter().copied().zip(0..).collect();
    if by_offset.len() != abs.len() {
        return Err(Error::BadValue {
            tag: SEQ,
            vr: Vr::Sq,
            message: "duplicate record offsets".into(),
        });
    }

    let mut dir = DicomDir {
        file_set_id: ds.string(FILESET).map(str::to_string),
        ..DicomDir::default()
    };
    let first = ds.uint32(FIRST).unwrap_or(0);
    if first == 0 {
        if !items.is_empty() {
            return Err(Error::BadValue {
                tag: FIRST,
                vr: Vr::Ul,
                message: "first-record offset is zero but records exist".into(),
            });
        }
        return Ok(dir);
    }
    let mut walker = Walker {
        items,
        by_offset,
        abs: &abs,
        visited: HashSet::new(),
        skipped: Vec::new(),
    };
    walker.walk_chain(first as usize, Level::Root, &mut dir)?;
    // Anything never reached (orphans under skipped records, ignored
    // leaf links) is reported, never silently dropped.
    for (i, item) in items.iter().enumerate() {
        if !walker.visited.contains(&i) {
            walker.skipped.push(SkippedRecord {
                offset: abs[i],
                record_type: record_type_of(item).unwrap_or_default().to_string(),
                reason: "unreachable record".into(),
            });
        }
    }
    dir.skipped = walker.skipped;
    Ok(dir)
}

/// Tree level: which record type a chain position expects.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Level {
    Root,
    Patient,
    Study,
    Series,
}

struct Walker<'a> {
    items: &'a [DataSet],
    by_offset: HashMap<usize, usize>,
    abs: &'a [usize],
    visited: HashSet<usize>,
    skipped: Vec<SkippedRecord>,
}

fn record_type_of(item: &DataSet) -> Option<&str> {
    item.string(REC_TYPE)
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

impl Walker<'_> {
    fn resolve(&self, offset: usize, link_tag: Tag) -> Result<usize> {
        self.by_offset
            .get(&offset)
            .copied()
            .ok_or_else(|| Error::BadValue {
                tag: link_tag,
                vr: Vr::Ul,
                message: format!("offset {offset} matches no directory record"),
            })
    }

    /// Follow one sibling chain, attaching well-placed records.
    fn walk_chain(&mut self, mut offset: usize, level: Level, dir: &mut DicomDir) -> Result<()> {
        loop {
            if offset == 0 {
                return Ok(());
            }
            let idx = self.resolve(offset, NEXT)?;
            if !self.visited.insert(idx) {
                return Err(Error::BadValue {
                    tag: NEXT,
                    vr: Vr::Ul,
                    message: format!("directory offset loop at {offset}"),
                });
            }
            let item = &self.items[idx];
            offset = item.uint32(NEXT).unwrap_or(0) as usize;
            self.visit(idx, item, level, dir)?;
        }
    }

    fn visit(
        &mut self,
        idx: usize,
        item: &DataSet,
        level: Level,
        dir: &mut DicomDir,
    ) -> Result<()> {
        // Only an explicit zero means deleted (the flag is US, but tolerate U32).
        let deleted = item.uint16(IN_USE_FLAG).is_some_and(|f| f == 0)
            || item.uint32(IN_USE_FLAG).is_some_and(|f| f == 0);
        if deleted {
            self.skip(idx, item, "record not in use");
            return Ok(());
        }
        let Some(rec_type) = record_type_of(item) else {
            self.skip(idx, item, "missing Directory Record Type");
            return Ok(());
        };
        let lower = item.uint32(LOWER).unwrap_or(0) as usize;
        // Children attach to the last node pushed (traversal order).
        match (level, rec_type) {
            (Level::Root, "PATIENT") => {
                dir.patients.push(patient_of(item));
                self.walk_chain(lower, Level::Patient, dir)?;
            }
            (Level::Patient, "STUDY") => {
                let Some(patient) = dir.patients.last_mut() else {
                    self.skip(idx, item, "STUDY record outside PATIENT");
                    return Ok(());
                };
                patient.studies.push(study_of(item));
                self.walk_chain(lower, Level::Study, dir)?;
            }
            (Level::Study, "SERIES") => {
                let Some(series) = last_series_mut(dir) else {
                    self.skip(idx, item, "SERIES record outside STUDY");
                    return Ok(());
                };
                series.push(series_of(item));
                self.walk_chain(lower, Level::Series, dir)?;
            }
            (Level::Series, t) if t != "PATIENT" && t != "STUDY" && t != "SERIES" => {
                match (
                    instance_of(item),
                    last_series_mut(dir).and_then(|s| s.last_mut()),
                ) {
                    (Some(inst), Some(series)) => series.instances.push(inst),
                    (None, _) => self.skip(idx, item, "leaf without file reference"),
                    (Some(_), None) => self.skip(idx, item, "leaf record outside SERIES"),
                }
            }
            _ => self.skip(idx, item, &format!("{rec_type} record at the wrong level")),
        }
        Ok(())
    }

    fn skip(&mut self, idx: usize, item: &DataSet, reason: &str) {
        self.skipped.push(SkippedRecord {
            offset: self.abs[idx],
            record_type: record_type_of(item).unwrap_or_default().to_string(),
            reason: reason.to_string(),
        });
    }
}

/// Currently open series list (series of the last study of the last patient).
fn last_series_mut(dir: &mut DicomDir) -> Option<&mut Vec<DirSeries>> {
    dir.patients
        .last_mut()?
        .studies
        .last_mut()
        .map(|s| &mut s.series)
}

fn text(item: &DataSet, tag: Tag) -> Option<String> {
    item.string(tag).map(str::to_string)
}

fn patient_of(item: &DataSet) -> DirPatient {
    // Person names are not text values; format the first one.
    let name = item
        .person_names(tags::PATIENT_NAME)
        .and_then(|v| v.first())
        .map(|pn| pn.to_dicom_string());
    DirPatient {
        name,
        id: text(item, tags::PATIENT_ID),
        birth_date: text(item, tags::PATIENT_BIRTH_DATE),
        sex: text(item, tags::PATIENT_SEX),
        studies: Vec::new(),
    }
}

fn study_of(item: &DataSet) -> DirStudy {
    DirStudy {
        uid: text(item, tags::STUDY_INSTANCE_UID),
        id: text(item, tags::STUDY_ID),
        date: text(item, tags::STUDY_DATE),
        time: text(item, tags::STUDY_TIME),
        accession: text(item, tags::ACCESSION_NUMBER),
        description: text(item, tags::STUDY_DESCRIPTION),
        series: Vec::new(),
    }
}

fn series_of(item: &DataSet) -> DirSeries {
    DirSeries {
        uid: text(item, tags::SERIES_INSTANCE_UID),
        number: text(item, tags::SERIES_NUMBER),
        modality: text(item, tags::MODALITY),
        description: text(item, tags::SERIES_DESCRIPTION),
        instances: Vec::new(),
    }
}

/// Leaf record → instance; `None` when it has no usable file reference.
fn instance_of(item: &DataSet) -> Option<DirInstance> {
    let file_id = item.strings(FILE_ID)?.to_vec();
    if file_id.is_empty() {
        return None;
    }
    Some(DirInstance {
        record_type: record_type_of(item)?.to_string(),
        sop_class_uid: text(item, REF_CLASS)?,
        sop_uid: text(item, REF_INST)?,
        ts_uid: text(item, REF_TS)?,
        instance_number: text(item, tags::INSTANCE_NUMBER),
        file_id,
    })
}

/// Map a SOP Class UID to its Directory Record Type (`IMAGE` default).
#[must_use]
pub fn record_type_for_sop_class(sop_class_uid: &str) -> &'static str {
    match sop_class_uid.trim_end_matches(['\0', ' ']) {
        sop_class::RT_DOSE => "RT DOSE",
        sop_class::RT_STRUCTURE_SET => "RT STRUCTURE SET",
        sop_class::RT_PLAN => "RT PLAN",
        sop_class::BASIC_TEXT_SR | sop_class::ENHANCED_SR | sop_class::COMPREHENSIVE_SR => {
            "SR DOCUMENT"
        }
        sop_class::KEY_OBJECT_SELECTION => "KEY OBJECT DOC",
        _ => "IMAGE",
    }
}

/// One file to index in a DICOMDIR.
pub struct DirEntry<'a> {
    /// The referenced file's dataset (record keys are copied from it).
    pub dataset: &'a DataSet,
    /// File ID: path components below the media root (1–8 of ≤16 chars).
    pub file_id: Vec<String>,
    /// Transfer Syntax UID of the file as stored on media.
    pub transfer_syntax_uid: String,
    /// Directory Record Type override (default: mapped from SOP Class).
    pub record_type: Option<String>,
}

/// Build a DICOMDIR file for `entries` (grouped patient → study →
/// series in first-appearance order). `file_set_id` defaults to
/// [`DEFAULT_FILE_SET_ID`].
pub fn build_dicomdir(entries: &[DirEntry], file_set_id: Option<&str>) -> Result<Vec<u8>> {
    for entry in entries {
        validate_entry(entry)?;
    }
    // Group stably: patient ID → study UID → series UID.
    let mut patients: Vec<PatientGroup> = Vec::new();
    let mut by_patient: HashMap<&str, usize> = HashMap::new();
    for (n, entry) in entries.iter().enumerate() {
        let ds = entry.dataset;
        let pid = ds.string(tags::PATIENT_ID).unwrap_or_default();
        let pidx = *by_patient.entry(pid).or_insert_with(|| {
            patients.push(PatientGroup::new());
            patients.len() - 1
        });
        patients[pidx].add(n, ds);
    }

    // Records in traversal order with parent links.
    let mut nodes: Vec<Node> = Vec::new();
    let mut roots: Vec<usize> = Vec::new();
    for patient in &patients {
        let pidx = push_node(&mut nodes, None, patient.item(entries));
        roots.push(pidx);
        for study in &patient.studies {
            let sidx = push_node(&mut nodes, Some(pidx), study.item(entries));
            for series in &study.series {
                let ridx = push_node(&mut nodes, Some(sidx), series.item(entries));
                for n in &series.entries {
                    push_node(&mut nodes, Some(ridx), instance_item(&entries[*n]));
                }
            }
        }
    }
    // Sibling chains → (next, lower) index links.
    let mut links = vec![(None, None); nodes.len()];
    link_children(&mut links, None, &roots);
    for (i, node) in nodes.iter().enumerate() {
        link_children(&mut links, Some(i), &node.children);
    }

    let mut items: Vec<DataSet> = nodes.into_iter().map(|n| n.item).collect();
    let meta = FileMeta::new(
        sop_class::MEDIA_STORAGE_DIRECTORY,
        vyna_dcm_core::new_uid(),
        TransferSyntax::ExplicitVrLittleEndian,
    );
    if items.is_empty() {
        let mut ds = DataSet::new();
        ds.put_text(FILESET, Vr::Cs, file_set_id.unwrap_or(DEFAULT_FILE_SET_ID));
        ds.put_u32_single(FIRST, 0);
        ds.put_u32_single(LAST, 0);
        ds.put_u16_single(CONSISTENCY, 0);
        return write_part10(&meta, &ds);
    }
    // First pass: measure item positions, then stamp the offsets (UL
    // values are fixed-size, so the layout cannot shift).
    let mut measure = DataSet::new();
    measure.put_text(FILESET, Vr::Cs, file_set_id.unwrap_or(DEFAULT_FILE_SET_ID));
    measure.put_u32_single(FIRST, 0);
    measure.put_u32_single(LAST, 0);
    measure.put_u16_single(CONSISTENCY, 0);
    measure.put_sequence(SEQ, items.clone());
    let ts = TransferSyntax::ExplicitVrLittleEndian;
    let (bytes, rels) = encode_dataset_capture_items(&measure, ts, SEQ)?;
    if rels.len() != items.len() {
        return Err(Error::BadValue {
            tag: SEQ,
            vr: Vr::Sq,
            message: "offset capture mismatch".into(),
        });
    }
    let start = crate::PREFIX_LEN + meta_section_len(&meta)?;
    let abs: Vec<u32> = rels
        .iter()
        .map(|r| {
            u32::try_from(start + r).map_err(|_| Error::BadValue {
                tag: SEQ,
                vr: Vr::Sq,
                message: "file too large for 32-bit directory offsets".into(),
            })
        })
        .collect::<Result<_>>()?;
    for (i, item) in items.iter_mut().enumerate() {
        let (next, lower) = links[i];
        item.put_u32_single(NEXT, next.map(|n| abs[n]).unwrap_or(0));
        item.put_u32_single(LOWER, lower.map(|n| abs[n]).unwrap_or(0));
    }
    let mut ds = DataSet::new();
    ds.put_text(FILESET, Vr::Cs, file_set_id.unwrap_or(DEFAULT_FILE_SET_ID));
    ds.put_u32_single(FIRST, abs[0]);
    ds.put_u32_single(LAST, abs[items.len() - 1]);
    ds.put_u16_single(CONSISTENCY, 0);
    ds.put_sequence(SEQ, items);
    let bytes2 = crate::encode_dataset(&ds, ts)?;
    debug_assert_eq!(bytes.len(), bytes2.len(), "offset stamping shifted layout");
    write_part10(&meta, &ds)
}

fn link_children(
    links: &mut [(Option<usize>, Option<usize>)],
    parent: Option<usize>,
    children: &[usize],
) {
    if let Some(p) = parent {
        links[p].1 = children.first().copied();
    }
    for w in children.windows(2) {
        links[w[0]].0 = Some(w[1]);
    }
}

fn push_node(nodes: &mut Vec<Node>, parent: Option<usize>, item: DataSet) -> usize {
    let idx = nodes.len();
    if let Some(p) = parent {
        nodes[p].children.push(idx);
    }
    nodes.push(Node {
        item,
        children: Vec::new(),
    });
    idx
}

struct Node {
    item: DataSet,
    children: Vec<usize>,
}

struct SeriesGroup {
    entries: Vec<usize>,
}

struct StudyGroup {
    series: Vec<SeriesGroup>,
    by_series: HashMap<String, usize>,
}

struct PatientGroup {
    studies: Vec<StudyGroup>,
    by_study: HashMap<String, usize>,
}

impl PatientGroup {
    fn new() -> Self {
        Self {
            studies: Vec::new(),
            by_study: HashMap::new(),
        }
    }

    fn add(&mut self, n: usize, ds: &DataSet) {
        let study_uid = ds.string(tags::STUDY_INSTANCE_UID).unwrap_or_default();
        let sidx = *self
            .by_study
            .entry(study_uid.to_string())
            .or_insert_with(|| {
                self.studies.push(StudyGroup {
                    series: Vec::new(),
                    by_series: HashMap::new(),
                });
                self.studies.len() - 1
            });
        let study = &mut self.studies[sidx];
        let series_uid = ds.string(tags::SERIES_INSTANCE_UID).unwrap_or_default();
        let ridx = *study
            .by_series
            .entry(series_uid.to_string())
            .or_insert_with(|| {
                study.series.push(SeriesGroup {
                    entries: Vec::new(),
                });
                study.series.len() - 1
            });
        study.series[ridx].entries.push(n);
    }

    fn item(&self, entries: &[DirEntry]) -> DataSet {
        let ds = entries[self.first_entry()].dataset;
        let mut item = record_item("PATIENT");
        for tag in [
            tags::PATIENT_NAME,
            tags::PATIENT_ID,
            tags::PATIENT_BIRTH_DATE,
            tags::PATIENT_SEX,
        ] {
            copy_key(&mut item, ds, tag);
        }
        item
    }

    fn first_entry(&self) -> usize {
        self.studies[0].series[0].entries[0]
    }
}

impl StudyGroup {
    fn item(&self, entries: &[DirEntry]) -> DataSet {
        let ds = entries[self.series[0].entries[0]].dataset;
        let mut item = record_item("STUDY");
        for tag in [
            tags::STUDY_DATE,
            tags::STUDY_TIME,
            tags::ACCESSION_NUMBER,
            tags::REFERRING_PHYSICIAN_NAME,
            tags::STUDY_DESCRIPTION,
            tags::STUDY_INSTANCE_UID,
            tags::STUDY_ID,
        ] {
            copy_key(&mut item, ds, tag);
        }
        item
    }
}

impl SeriesGroup {
    fn item(&self, entries: &[DirEntry]) -> DataSet {
        let ds = entries[self.entries[0]].dataset;
        let mut item = record_item("SERIES");
        for tag in [
            tags::MODALITY,
            tags::SERIES_DATE,
            tags::SERIES_TIME,
            tags::SERIES_DESCRIPTION,
            tags::SERIES_INSTANCE_UID,
            tags::SERIES_NUMBER,
        ] {
            copy_key(&mut item, ds, tag);
        }
        item
    }
}

/// Record framing with zeroed links plus the type.
fn record_item(rec_type: &str) -> DataSet {
    let mut item = DataSet::new();
    item.put_u32_single(NEXT, 0);
    item.put_u16_single(IN_USE_FLAG, IN_USE);
    item.put_u32_single(LOWER, 0);
    item.put_text(REC_TYPE, Vr::Cs, rec_type);
    item
}

/// Copy a key element verbatim when the source dataset has it.
fn copy_key(item: &mut DataSet, ds: &DataSet, tag: Tag) {
    if let Some(el) = ds.get(tag) {
        item.insert(el.clone());
    }
}

fn instance_item(entry: &DirEntry) -> DataSet {
    let ds = entry.dataset;
    let rec_type = entry
        .record_type
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| {
            record_type_for_sop_class(ds.string(tags::SOP_CLASS_UID).unwrap_or_default())
                .to_string()
        });
    let mut item = record_item(&rec_type);
    if rec_type == "PRIVATE" {
        item.put_uid(
            PRIV_UID,
            ds.string(tags::SOP_INSTANCE_UID).unwrap_or_default(),
        );
    }
    copy_key(&mut item, ds, tags::INSTANCE_NUMBER);
    copy_key(&mut item, ds, tags::IMAGE_TYPE);
    item.insert(DataElement::texts(FILE_ID, Vr::Cs, entry.file_id.clone()));
    item.put_uid(
        REF_CLASS,
        ds.string(tags::SOP_CLASS_UID).unwrap_or_default(),
    );
    item.put_uid(
        REF_INST,
        ds.string(tags::SOP_INSTANCE_UID).unwrap_or_default(),
    );
    item.put_uid(REF_TS, entry.transfer_syntax_uid.clone());
    item
}

fn validate_entry(entry: &DirEntry) -> Result<()> {
    let ds = entry.dataset;
    for tag in [tags::SOP_CLASS_UID, tags::SOP_INSTANCE_UID] {
        if ds.string(tag).is_none_or(|s| s.is_empty()) {
            return Err(Error::BadValue {
                tag,
                vr: Vr::Ui,
                message: format!("entry for {:?} needs {tag}", entry.file_id),
            });
        }
    }
    for tag in [
        tags::PATIENT_ID,
        tags::STUDY_INSTANCE_UID,
        tags::SERIES_INSTANCE_UID,
    ] {
        if ds.string(tag).is_none_or(|s| s.is_empty()) {
            return Err(Error::BadValue {
                tag,
                vr: Vr::Ui,
                message: format!("entry for {:?} needs {tag} for grouping", entry.file_id),
            });
        }
    }
    if entry.file_id.is_empty() || entry.file_id.len() > 8 {
        return Err(Error::BadValue {
            tag: FILE_ID,
            vr: Vr::Cs,
            message: format!("file ID needs 1–8 components, got {}", entry.file_id.len()),
        });
    }
    for comp in &entry.file_id {
        if comp.is_empty() || comp.len() > 16 {
            return Err(Error::BadValue {
                tag: FILE_ID,
                vr: Vr::Cs,
                message: format!("file ID component {comp:?} must be 1–16 chars"),
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::read_part10;

    fn img_ds(
        sop_class: &str,
        sop_uid: &str,
        patient_id: &str,
        study_uid: &str,
        series_uid: &str,
        modality: &str,
        number: &str,
    ) -> DataSet {
        let mut ds = DataSet::new();
        ds.put_uid(tags::SOP_CLASS_UID, sop_class);
        ds.put_uid(tags::SOP_INSTANCE_UID, sop_uid);
        ds.put_person_name(
            tags::PATIENT_NAME,
            vec![vyna_dcm_core::PersonName::parse("Doe^Jane")],
        );
        ds.put_text(tags::PATIENT_ID, Vr::Lo, patient_id);
        ds.put_uid(tags::STUDY_INSTANCE_UID, study_uid);
        ds.put_text(tags::STUDY_ID, Vr::Sh, "S1");
        ds.put_uid(tags::SERIES_INSTANCE_UID, series_uid);
        ds.put_text(tags::MODALITY, Vr::Cs, modality);
        ds.put_text(tags::SERIES_NUMBER, Vr::Is, "3");
        ds.put_text(tags::INSTANCE_NUMBER, Vr::Is, number);
        ds
    }

    fn entry<'a>(
        ds: &'a DataSet,
        file_id: &[&str],
        ts: &str,
        record_type: Option<&str>,
    ) -> DirEntry<'a> {
        DirEntry {
            dataset: ds,
            file_id: file_id.iter().map(|s| s.to_string()).collect(),
            transfer_syntax_uid: ts.to_string(),
            record_type: record_type.map(str::to_string),
        }
    }

    fn ct_sets() -> Vec<DataSet> {
        vec![
            img_ds(
                "1.2.840.10008.5.1.4.1.1.2",
                "1.2.3.1",
                "P1",
                "1.2.3",
                "1.2.3.1",
                "CT",
                "1",
            ),
            img_ds(
                "1.2.840.10008.5.1.4.1.1.2",
                "1.2.3.2",
                "P1",
                "1.2.3",
                "1.2.3.1",
                "CT",
                "2",
            ),
            img_ds(
                sop_class::RT_DOSE,
                "1.2.3.3",
                "P1",
                "1.2.3",
                "1.2.3.9",
                "RTDOSE",
                "1",
            ),
            img_ds(
                sop_class::COMPREHENSIVE_SR,
                "1.2.4.1",
                "P2",
                "1.2.4",
                "1.2.4.1",
                "SR",
                "1",
            ),
        ]
    }

    /// Every nonzero link in the file must point at an item tag.
    fn assert_links_point_at_items(bytes: &[u8]) {
        let (_, ds) = read_part10(bytes).unwrap();
        let items = ds.sequence(SEQ).unwrap();
        let mut offs = vec![ds.uint32(FIRST).unwrap(), ds.uint32(LAST).unwrap()];
        for item in items {
            offs.push(item.uint32(NEXT).unwrap_or(0));
            offs.push(item.uint32(LOWER).unwrap_or(0));
        }
        assert!(offs.iter().any(|o| *o != 0));
        for off in offs.into_iter().filter(|o| *o != 0) {
            let o = off as usize;
            assert_eq!(
                &bytes[o..o + 4],
                &[0xFE, 0xFF, 0x00, 0xE0],
                "offset {off} is not an item tag"
            );
        }
    }

    #[test]
    fn roundtrip_tree() {
        let dss = ct_sets();
        let entries = vec![
            entry(&dss[0], &["DICOM", "IM1"], "1.2.840.10008.1.2.1", None),
            entry(&dss[1], &["DICOM", "IM2"], "1.2.840.10008.1.2.1", None),
            entry(&dss[2], &["DICOM", "RD1"], "1.2.840.10008.1.2", None),
            entry(&dss[3], &["DICOM", "SR1"], "1.2.840.10008.1.2.1", None),
        ];
        let bytes = build_dicomdir(&entries, Some("TESTSET")).unwrap();
        assert_links_point_at_items(&bytes);
        let dir = read_dicomdir(&bytes).unwrap();
        assert_eq!(dir.file_set_id.as_deref(), Some("TESTSET"));
        assert!(dir.skipped.is_empty());
        assert_eq!(dir.patients.len(), 2);
        let p1 = &dir.patients[0];
        assert_eq!(p1.id.as_deref(), Some("P1"));
        assert_eq!(p1.name.as_deref(), Some("Doe^Jane"));
        assert_eq!(p1.studies.len(), 1);
        assert_eq!(p1.studies[0].series.len(), 2);
        let ct = &p1.studies[0].series[0];
        assert_eq!(ct.modality.as_deref(), Some("CT"));
        assert_eq!(ct.instances.len(), 2);
        assert_eq!(ct.instances[0].record_type, "IMAGE");
        assert_eq!(ct.instances[0].sop_uid, "1.2.3.1");
        assert_eq!(ct.instances[0].instance_number.as_deref(), Some("1"));
        assert_eq!(
            ct.instances[0].file_id,
            vec!["DICOM".to_string(), "IM1".to_string()]
        );
        assert_eq!(ct.instances[0].relative_path(), PathBuf::from("DICOM/IM1"));
        let rd = &p1.studies[0].series[1];
        assert_eq!(rd.instances[0].record_type, "RT DOSE");
        assert_eq!(rd.instances[0].ts_uid, "1.2.840.10008.1.2");
        let p2 = &dir.patients[1];
        assert_eq!(
            p2.studies[0].series[0].instances[0].record_type,
            "SR DOCUMENT"
        );
        assert_eq!(dir.files().len(), 4);
        // Default file-set ID.
        let bytes = build_dicomdir(&entries, None).unwrap();
        let dir = read_dicomdir(&bytes).unwrap();
        assert_eq!(dir.file_set_id.as_deref(), Some(DEFAULT_FILE_SET_ID));
    }

    #[test]
    fn empty_dir_roundtrip() {
        let bytes = build_dicomdir(&[], Some("EMPTY")).unwrap();
        let dir = read_dicomdir(&bytes).unwrap();
        assert_eq!(dir.file_set_id.as_deref(), Some("EMPTY"));
        assert!(dir.patients.is_empty());
        assert!(dir.skipped.is_empty());
    }

    #[test]
    fn record_type_mapping() {
        assert_eq!(record_type_for_sop_class(sop_class::RT_DOSE), "RT DOSE");
        assert_eq!(
            record_type_for_sop_class(sop_class::RT_STRUCTURE_SET),
            "RT STRUCTURE SET"
        );
        assert_eq!(record_type_for_sop_class(sop_class::RT_PLAN), "RT PLAN");
        assert_eq!(
            record_type_for_sop_class(sop_class::BASIC_TEXT_SR),
            "SR DOCUMENT"
        );
        assert_eq!(
            record_type_for_sop_class(sop_class::KEY_OBJECT_SELECTION),
            "KEY OBJECT DOC"
        );
        assert_eq!(
            record_type_for_sop_class("1.2.840.10008.5.1.4.1.1.2"),
            "IMAGE"
        );
        assert_eq!(record_type_for_sop_class("9.9.9.9"), "IMAGE");
        // Explicit override wins, including PRIVATE.
        let dss = ct_sets();
        let entries = vec![entry(
            &dss[0],
            &["F1"],
            "1.2.840.10008.1.2.1",
            Some("PRIVATE"),
        )];
        let bytes = build_dicomdir(&entries, None).unwrap();
        let dir = read_dicomdir(&bytes).unwrap();
        assert_eq!(
            dir.patients[0].studies[0].series[0].instances[0].record_type,
            "PRIVATE"
        );
    }

    #[test]
    fn entry_validation() {
        let dss = ct_sets();
        // Missing grouping key.
        let mut bad = dss[0].clone();
        bad.remove(tags::PATIENT_ID);
        let entries = vec![entry(&bad, &["F1"], "1.2.840.10008.1.2.1", None)];
        assert!(build_dicomdir(&entries, None).is_err());
        // Bad file IDs.
        for file_id in [
            Vec::new(),
            vec!["a", "b", "c", "d", "e", "f", "g", "h", "i"],
            vec![""],
            vec!["0123456789ABCDEFG"],
        ] {
            let entries = vec![entry(&dss[0], &file_id, "1.2.840.10008.1.2.1", None)];
            assert!(build_dicomdir(&entries, None).is_err(), "{file_id:?}");
        }
    }

    #[test]
    fn rejects_non_directory() {
        let mut ds = DataSet::new();
        ds.put_uid(tags::SOP_CLASS_UID, "1.2.840.10008.5.1.4.1.1.2");
        ds.put_uid(tags::SOP_INSTANCE_UID, "1.2.3.9");
        let meta = FileMeta::new(
            "1.2.840.10008.5.1.4.1.1.2",
            "1.2.3.9",
            TransferSyntax::ExplicitVrLittleEndian,
        );
        let bytes = write_part10(&meta, &ds).unwrap();
        assert!(read_dicomdir(&bytes).is_err());
    }

    #[test]
    fn first_zero_with_records_errors() {
        let mut ds = DataSet::new();
        ds.put_text(FILESET, Vr::Cs, "X");
        ds.put_u32_single(FIRST, 0);
        ds.put_u32_single(LAST, 0);
        ds.put_u16_single(CONSISTENCY, 0);
        ds.put_sequence(SEQ, vec![record_item("PATIENT")]);
        let meta = FileMeta::new(
            sop_class::MEDIA_STORAGE_DIRECTORY,
            "1.2.3.9",
            TransferSyntax::ExplicitVrLittleEndian,
        );
        let bytes = write_part10(&meta, &ds).unwrap();
        assert!(read_dicomdir(&bytes).is_err());
    }

    #[test]
    fn capture_points_at_item_tags() {
        let mut ds = DataSet::new();
        ds.put_text(tags::PATIENT_ID, Vr::Lo, "P1");
        ds.put_sequence(SEQ, vec![record_item("PATIENT"), record_item("STUDY")]);
        let ts = TransferSyntax::ExplicitVrLittleEndian;
        let (bytes, offsets) = encode_dataset_capture_items(&ds, ts, SEQ).unwrap();
        assert_eq!(offsets.len(), 2);
        for off in &offsets {
            assert_eq!(&bytes[*off..*off + 4], &[0xFE, 0xFF, 0x00, 0xE0]);
        }
        let (back, offsets2) = decode_dataset_capture_items(&bytes, ts, SEQ).unwrap();
        assert_eq!(offsets, offsets2);
        assert_eq!(back.sequence(SEQ).unwrap().len(), 2);
    }

    // --- Walker unit tests (hand-linked items, fake offsets) ---

    fn wrec(rec_type: &str, next: u32, lower: u32) -> DataSet {
        let mut item = record_item(rec_type);
        item.put_u32_single(NEXT, next);
        item.put_u32_single(LOWER, lower);
        item
    }

    fn wleaf(next: u32) -> DataSet {
        let mut item = wrec("IMAGE", next, 0);
        item.insert(DataElement::texts(FILE_ID, Vr::Cs, vec!["F1".to_string()]));
        item.put_uid(REF_CLASS, "1.2.840.10008.5.1.4.1.1.2");
        item.put_uid(REF_INST, "1.2.3.1");
        item.put_uid(REF_TS, "1.2.840.10008.1.2.1");
        item
    }

    fn walker_at<'a>(items: &'a [DataSet], abs: &'a [usize]) -> Walker<'a> {
        Walker {
            items,
            by_offset: abs.iter().copied().zip(0..).collect(),
            abs,
            visited: HashSet::new(),
            skipped: Vec::new(),
        }
    }

    #[test]
    fn walker_skips_and_sweeps() {
        // PATIENT ->lower STUDY ->next SERIES-at-the-wrong-level
        // (skipped, not descended); the leaf is an orphan (swept).
        // (Offset 0 ends chains, so fakes start at a nonzero base.)
        let items = vec![
            wrec("PATIENT", 0, 200),
            wrec("STUDY", 300, 0),
            wrec("SERIES", 0, 400),
            wleaf(0),
        ];
        let abs = vec![100usize, 200, 300, 400];
        let mut walker = walker_at(&items, &abs);
        let mut dir = DicomDir::default();
        walker.walk_chain(100, Level::Root, &mut dir).unwrap();
        for (i, item) in items.iter().enumerate() {
            if !walker.visited.contains(&i) {
                walker.skipped.push(SkippedRecord {
                    offset: abs[i],
                    record_type: record_type_of(item).unwrap_or_default().to_string(),
                    reason: "unreachable record".into(),
                });
            }
        }
        assert_eq!(dir.patients.len(), 1);
        assert_eq!(dir.patients[0].studies.len(), 1);
        assert!(dir.patients[0].studies[0].series.is_empty());
        let reasons: Vec<&str> = walker.skipped.iter().map(|s| s.reason.as_str()).collect();
        assert!(
            reasons.iter().any(|r| r.contains("wrong level")),
            "{reasons:?}"
        );
        assert!(
            reasons.iter().any(|r| r.contains("unreachable")),
            "{reasons:?}"
        );
    }

    #[test]
    fn walker_leaf_without_file_is_skipped() {
        let items = vec![
            wrec("PATIENT", 0, 200),
            wrec("STUDY", 0, 300),
            wrec("SERIES", 0, 400),
            wrec("IMAGE", 0, 0), // no file reference
        ];
        let abs: Vec<usize> = vec![100, 200, 300, 400];
        let mut walker = walker_at(&items, &abs);
        let mut dir = DicomDir::default();
        walker.walk_chain(100, Level::Root, &mut dir).unwrap();
        assert!(dir.patients[0].studies[0].series[0].instances.is_empty());
        assert_eq!(walker.skipped.len(), 1);
        assert!(walker.skipped[0].reason.contains("file reference"));
    }

    #[test]
    fn walker_loops_and_dangling_error() {
        // Two records looping back to each other (offset 0 ends chains,
        // so the loop lives at a nonzero base).
        let items = vec![wrec("STUDY", 200, 0), wrec("STUDY", 100, 0)];
        let abs = vec![100usize, 200usize];
        let mut walker = walker_at(&items, &abs);
        let mut dir = DicomDir::default();
        assert!(walker.walk_chain(100, Level::Patient, &mut dir).is_err());
        // Dangling offset.
        let items = vec![wrec("PATIENT", 999, 0)];
        let abs = vec![100usize];
        let mut walker = walker_at(&items, &abs);
        assert!(walker.walk_chain(100, Level::Root, &mut dir).is_err());
        // Deleted record: skipped, subtree orphaned.
        let mut items = vec![wrec("PATIENT", 0, 200), wleaf(0)];
        items[0].put_u16_single(IN_USE_FLAG, 0);
        let abs = vec![100usize, 200];
        let mut walker = walker_at(&items, &abs);
        let mut dir = DicomDir::default();
        walker.walk_chain(100, Level::Root, &mut dir).unwrap();
        assert!(dir.patients.is_empty());
        assert_eq!(walker.skipped.len(), 1);
        assert!(walker.skipped[0].reason.contains("not in use"));
    }
}
