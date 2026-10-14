//! Part-10 files: 128-byte preamble + `DICM` + file meta + dataset.
//!
//! File meta (group `0x0002`) is always Explicit VR Little Endian and
//! carries the dataset's transfer syntax (NEMA PS3.10).

use vyna_dcm_core::{
    DataSet, TransferSyntax, VYNA_IMPLEMENTATION_CLASS_UID, VYNA_IMPLEMENTATION_VERSION_NAME, Vr,
    tags,
};

use crate::{Error, Result, decode_dataset, decode_up_to, encode_dataset};

/// Preamble (128) + `DICM` (4) prefix length.
pub const PREFIX_LEN: usize = 132;
/// File meta information version (`00 01`).
pub const META_VERSION: [u8; 2] = [0x00, 0x01];

/// File Meta Information (group `0x0002`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileMeta {
    /// Media Storage SOP Class UID.
    pub media_storage_sop_class_uid: String,
    /// Media Storage SOP Instance UID.
    pub media_storage_sop_instance_uid: String,
    /// Transfer Syntax UID of the dataset that follows.
    pub transfer_syntax_uid: String,
    /// Implementation Class UID of the writer.
    pub implementation_class_uid: String,
    /// Optional implementation version name (≤ 16 chars).
    pub implementation_version_name: Option<String>,
    /// Optional source/sending/receiving AE titles.
    pub source_ae_title: Option<String>,
    /// Optional sending AE title.
    pub sending_ae_title: Option<String>,
    /// Optional receiving AE title.
    pub receiving_ae_title: Option<String>,
}

impl FileMeta {
    /// Minimal meta for new files written by this library.
    pub fn new(
        sop_class_uid: impl Into<String>,
        sop_instance_uid: impl Into<String>,
        ts: TransferSyntax,
    ) -> Self {
        Self {
            media_storage_sop_class_uid: sop_class_uid.into(),
            media_storage_sop_instance_uid: sop_instance_uid.into(),
            transfer_syntax_uid: ts.uid().to_string(),
            implementation_class_uid: VYNA_IMPLEMENTATION_CLASS_UID.to_string(),
            implementation_version_name: Some(VYNA_IMPLEMENTATION_VERSION_NAME.to_string()),
            source_ae_title: None,
            sending_ae_title: None,
            receiving_ae_title: None,
        }
    }

    /// Parse the dataset transfer syntax.
    pub fn transfer_syntax(&self) -> Result<TransferSyntax> {
        TransferSyntax::from_uid(&self.transfer_syntax_uid)
            .ok_or_else(|| Error::UnsupportedTransferSyntax(self.transfer_syntax_uid.clone()))
    }

    /// Build from a decoded group-`0x0002` dataset.
    pub fn from_dataset(ds: &DataSet) -> Result<Self> {
        let req = |tag| {
            ds.string(tag)
                .filter(|s| !s.is_empty())
                .map(|s| s.trim_end_matches('\0').to_string())
                .ok_or_else(|| Error::BadFileMeta(format!("missing {tag}")))
        };
        Ok(Self {
            media_storage_sop_class_uid: req(tags::MEDIA_STORAGE_SOP_CLASS_UID)?,
            media_storage_sop_instance_uid: req(tags::MEDIA_STORAGE_SOP_INSTANCE_UID)?,
            transfer_syntax_uid: req(tags::TRANSFER_SYNTAX_UID)?,
            implementation_class_uid: req(tags::IMPLEMENTATION_CLASS_UID)?,
            implementation_version_name: ds
                .string(tags::IMPLEMENTATION_VERSION_NAME)
                .map(str::to_string),
            source_ae_title: ds
                .string(tags::SOURCE_APPLICATION_ENTITY_TITLE)
                .map(str::trim)
                .map(str::to_string),
            sending_ae_title: ds
                .string(tags::SENDING_APPLICATION_ENTITY_TITLE)
                .map(str::trim)
                .map(str::to_string),
            receiving_ae_title: ds
                .string(tags::RECEIVING_APPLICATION_ENTITY_TITLE)
                .map(str::trim)
                .map(str::to_string),
        })
    }

    /// Convert to a group-`0x0002` dataset (without group length; the
    /// writer computes it).
    pub fn to_dataset(&self) -> DataSet {
        use vyna_dcm_core::{DataElement, Vr};
        let mut ds = DataSet::new();
        ds.insert(DataElement::bytes(
            tags::FILE_META_INFORMATION_VERSION,
            Vr::Ob,
            META_VERSION.to_vec(),
        ));
        ds.put_uid(
            tags::MEDIA_STORAGE_SOP_CLASS_UID,
            self.media_storage_sop_class_uid.clone(),
        );
        ds.put_uid(
            tags::MEDIA_STORAGE_SOP_INSTANCE_UID,
            self.media_storage_sop_instance_uid.clone(),
        );
        ds.put_uid(tags::TRANSFER_SYNTAX_UID, self.transfer_syntax_uid.clone());
        ds.put_uid(
            tags::IMPLEMENTATION_CLASS_UID,
            self.implementation_class_uid.clone(),
        );
        if let Some(v) = &self.implementation_version_name {
            ds.put_text(tags::IMPLEMENTATION_VERSION_NAME, Vr::Sh, v.clone());
        }
        if let Some(v) = &self.source_ae_title {
            ds.put_text(tags::SOURCE_APPLICATION_ENTITY_TITLE, Vr::Ae, v.clone());
        }
        if let Some(v) = &self.sending_ae_title {
            ds.put_text(tags::SENDING_APPLICATION_ENTITY_TITLE, Vr::Ae, v.clone());
        }
        if let Some(v) = &self.receiving_ae_title {
            ds.put_text(tags::RECEIVING_APPLICATION_ENTITY_TITLE, Vr::Ae, v.clone());
        }
        ds
    }
}

/// Read a Part-10 file into `(meta, dataset)`.
pub fn read_part10(bytes: &[u8]) -> Result<(FileMeta, DataSet)> {
    let (meta, body, meta_len) = read_meta(bytes)?;
    let ts = meta.transfer_syntax()?;
    let dataset = decode_dataset(&body[meta_len..], ts)?;
    Ok((meta, dataset))
}

/// Read file meta plus the dataset header, stopping before Pixel Data
/// (which — and anything after it — is never decoded): for indexes and
/// tools that only need header keys. Standard query keys all sort
/// before group `0x7FE0`, so nothing a header scan needs is lost.
pub fn read_part10_header(bytes: &[u8]) -> Result<(FileMeta, DataSet)> {
    read_part10_header_complete(bytes).map(|(meta, ds, _)| (meta, ds))
}

/// Like [`read_part10_header`], also reporting whether the Pixel Data
/// stop was reached: `false` means `bytes` ran out first, so the
/// headers may be partial (callers feeding a bounded prefix must grow
/// it and retry rather than index a key-poor record).
///
/// Exception: a fully-present file with no Pixel Data element at all
/// also reports `false` — the flag cannot distinguish "pixel-less" from
/// "cut exactly at the pixel tag". Callers must treat EOF as complete:
///
/// ```ignore
/// let (meta, ds, complete) = read_part10_header_complete(&prefix)?;
/// let done = complete || eof_reached; // qrdb's read_header_capped does this
/// ```
pub fn read_part10_header_complete(bytes: &[u8]) -> Result<(FileMeta, DataSet, bool)> {
    let (meta, body, meta_len) = read_meta(bytes)?;
    let ts = meta.transfer_syntax()?;
    let slice = &body[meta_len..];
    let (dataset, used) = decode_up_to(slice, ts, Some(tags::PIXEL_DATA))?;
    // The stop rewinds without consuming, so anything unconsumed
    // means the Pixel Data tag was reached (headers complete).
    Ok((meta, dataset, used < slice.len()))
}

/// Prefix check plus file-meta walk shared by both readers.
fn read_meta(bytes: &[u8]) -> Result<(FileMeta, &[u8], usize)> {
    if bytes.len() < PREFIX_LEN {
        return Err(Error::NotPart10("file shorter than 132-byte prefix"));
    }
    if &bytes[128..132] != b"DICM" {
        return Err(Error::NotPart10("missing `DICM` magic at offset 128"));
    }
    let body = &bytes[PREFIX_LEN..];
    let stop = Tag::new(0x0003, 0x0000); // end of group 0x0002
    let (meta_ds, meta_len) =
        decode_up_to(body, TransferSyntax::ExplicitVrLittleEndian, Some(stop))?;
    let meta = FileMeta::from_dataset(&meta_ds)?;
    Ok((meta, body, meta_len))
}

/// Encode the file-meta section: group length element + meta elements.
fn encode_meta_section(meta: &FileMeta) -> Result<Vec<u8>> {
    let meta_ds = meta.to_dataset();
    // Encode once to measure, then prepend the group length.
    let mut meta_body = encode_dataset(&meta_ds, TransferSyntax::ExplicitVrLittleEndian)?;
    let group_len = u32::try_from(meta_body.len()).map_err(|_| Error::Unencodable {
        tag: Tag::new(0x0002, 0x0000),
        vr: Vr::Ul,
        message: format!(
            "file meta ({} bytes) exceeds the u32 group length",
            meta_body.len()
        ),
    })?;
    let mut meta_full = Vec::with_capacity(meta_body.len() + 12);
    meta_full.extend_from_slice(&[0x02, 0x00, 0x00, 0x00]); // (0002,0000)
    meta_full.extend_from_slice(b"UL");
    meta_full.extend_from_slice(&[0x04, 0x00]);
    meta_full.extend_from_slice(&group_len.to_le_bytes());
    meta_full.append(&mut meta_body);
    Ok(meta_full)
}

/// Length of the encoded file-meta section (for DICOMDIR offsets).
pub(crate) fn meta_section_len(meta: &FileMeta) -> Result<usize> {
    encode_meta_section(meta).map(|v| v.len())
}

/// Write a Part-10 file: preamble + `DICM` + meta + dataset.
pub fn write_part10(meta: &FileMeta, dataset: &DataSet) -> Result<Vec<u8>> {
    let ts = meta.transfer_syntax()?;
    let meta_full = encode_meta_section(meta)?;

    let data = encode_dataset(dataset, ts)?;
    let mut out = Vec::with_capacity(PREFIX_LEN + meta_full.len() + data.len());
    out.extend_from_slice(&[0u8; 128]);
    out.extend_from_slice(b"DICM");
    out.extend_from_slice(&meta_full);
    out.extend_from_slice(&data);
    Ok(out)
}

use vyna_dcm_core::Tag;

#[cfg(test)]
mod tests {
    use super::*;
    use vyna_dcm_core::{DataElement, Value, Vr, tags};

    fn sample() -> (FileMeta, DataSet) {
        let meta = FileMeta::new(
            "1.2.840.10008.5.1.4.1.1.9.1.1",
            "1.2.3.4",
            TransferSyntax::ExplicitVrLittleEndian,
        );
        let mut ds = DataSet::new();
        ds.put_uid(tags::SOP_CLASS_UID, "1.2.840.10008.5.1.4.1.1.9.1.1");
        ds.put_uid(tags::SOP_INSTANCE_UID, "1.2.3.4");
        ds.put_person_name(
            tags::PATIENT_NAME,
            vec![vyna_dcm_core::PersonName::parse("Doe^Jane")],
        );
        ds.put_text(tags::MODALITY, Vr::Cs, "ECG");
        (meta, ds)
    }

    #[test]
    fn round_trips_part10() {
        let (meta, ds) = sample();
        let bytes = write_part10(&meta, &ds).unwrap();
        assert_eq!(&bytes[128..132], b"DICM");
        let (meta2, ds2) = read_part10(&bytes).unwrap();
        assert_eq!(meta2, meta);
        assert_eq!(ds2, ds);
    }

    #[test]
    fn round_trips_implicit_and_deflated() {
        for ts in [
            TransferSyntax::ImplicitVrLittleEndian,
            TransferSyntax::DeflatedExplicitVrLittleEndian,
        ] {
            let (mut meta, ds) = sample();
            meta.transfer_syntax_uid = ts.uid().to_string();
            let bytes = write_part10(&meta, &ds).unwrap();
            let (meta2, ds2) = read_part10(&bytes).unwrap();
            assert_eq!(meta2.transfer_syntax_uid, ts.uid());
            assert_eq!(ds2, ds);
        }
    }

    #[test]
    fn rejects_non_part10() {
        assert!(read_part10(&[0u8; 10]).is_err());
        let mut bad = vec![0u8; 200];
        bad[128..132].copy_from_slice(b"XXXX");
        assert!(read_part10(&bad).is_err());
    }

    #[test]
    fn header_completeness_tracks_pixel_data_stop() {
        let (meta, mut ds) = sample();
        ds.insert(DataElement::new(
            tags::PIXEL_DATA,
            Vr::Ow,
            Value::Bytes(vec![1, 2, 3, 4]),
        ));
        let bytes = write_part10(&meta, &ds).unwrap();
        // Whole file: the stop is reached.
        let (_, back, complete) = read_part10_header_complete(&bytes).unwrap();
        assert!(complete);
        assert!(back.get(tags::PIXEL_DATA).is_none());
        assert_eq!(back.string(tags::MODALITY), Some("ECG"));
        // Explicit VR: tag(4) + VR(2) + reserved(2) + len(4) + value(4).
        let at_tag = bytes.len() - 16;
        // Prefix cut one byte into the headers: truncation errors,
        // never partial success. (Cuts inside the pixel VALUE stay
        // invisible by design — the header reader stops at its tag.)
        assert!(read_part10_header_complete(&bytes[..at_tag - 1]).is_err());
        // Prefix cut exactly at the pixel tag: headers decode but the
        // stop is unreached, so callers must grow, not index partial.
        let (_, partial, complete) = read_part10_header_complete(&bytes[..at_tag]).unwrap();
        assert!(!complete);
        assert_eq!(partial.string(tags::MODALITY), Some("ECG"));
    }
}
