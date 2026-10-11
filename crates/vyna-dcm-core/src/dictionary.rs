//! Public data dictionary (NEMA PS3.6 subset).
//!
//! Curated author transcription of the attributes VynaECG / VynaLink need:
//! file meta, DIMSE commands, SOP common, patient, study/series/instance,
//! general image, pixel, waveform, worklist/MPPS, SR content, documents.
//! Tag numbers, default VRs, VMs and keywords were cross-checked against
//! the PS3.6 registry (via `dicom-rs/dicom-dictionary-std`, `pydicom` and
//! `dcm4che` conventions); each entry keeps the primary VR for attributes
//! whose VR depends on context:
//!
//! - `ox` (`OB`/`OW`): Pixel Data, Waveform Data, Channel Min/Max, Waveform
//!   Padding — `OB` when (Waveform) Bits Allocated ≤ 8, else `OW`.
//! - `xs` (`US`/`SS`): Smallest/Largest Image Pixel Value, Pixel Padding
//!   Value — `US` when Pixel Representation is 0, else `SS`.
//!
//! The implicit-VR codec in `vyna-dcm-codec` applies these rules; see
//! [`is_context_dependent_vr`].

use crate::{Tag, Vr};

/// One public dictionary entry.
#[derive(Clone, Copy, Debug)]
pub struct DictEntry {
    /// Attribute tag.
    pub tag: Tag,
    /// Default (primary) value representation.
    pub vr: Vr,
    /// Value multiplicity descriptor, e.g. `"1"`, `"1-n"`, `"2"`.
    pub vm: &'static str,
    /// CamelCase keyword, e.g. `"PatientName"`.
    pub keyword: &'static str,
    /// Human-readable name, e.g. `"Patient's Name"`.
    pub name: &'static str,
}

const fn e(
    group: u16,
    element: u16,
    vr: Vr,
    vm: &'static str,
    keyword: &'static str,
    name: &'static str,
) -> DictEntry {
    DictEntry {
        tag: Tag::new(group, element),
        vr,
        vm,
        keyword,
        name,
    }
}

/// Curated public dictionary in ascending tag order.
pub const ENTRIES: &[DictEntry] = &[
    // -- DIMSE command group (0000) --------------------------------------
    e(
        0x0000,
        0x0000,
        Vr::Ul,
        "1",
        "CommandGroupLength",
        "Command Group Length",
    ),
    e(
        0x0000,
        0x0002,
        Vr::Ui,
        "1",
        "AffectedSOPClassUID",
        "Affected SOP Class UID",
    ),
    e(
        0x0000,
        0x0003,
        Vr::Ui,
        "1",
        "RequestedSOPClassUID",
        "Requested SOP Class UID",
    ),
    e(0x0000, 0x0100, Vr::Us, "1", "CommandField", "Command Field"),
    e(0x0000, 0x0110, Vr::Us, "1", "MessageID", "Message ID"),
    e(
        0x0000,
        0x0120,
        Vr::Us,
        "1",
        "MessageIDBeingRespondedTo",
        "Message ID Being Responded To",
    ),
    e(
        0x0000,
        0x0600,
        Vr::Ae,
        "1",
        "MoveDestination",
        "Move Destination",
    ),
    e(0x0000, 0x0700, Vr::Us, "1", "Priority", "Priority"),
    e(
        0x0000,
        0x0800,
        Vr::Us,
        "1",
        "CommandDataSetType",
        "Command Data Set Type",
    ),
    e(0x0000, 0x0900, Vr::Us, "1", "Status", "Status"),
    e(
        0x0000,
        0x0901,
        Vr::At,
        "1-n",
        "OffendingElement",
        "Offending Element",
    ),
    e(0x0000, 0x0902, Vr::Lo, "1", "ErrorComment", "Error Comment"),
    e(0x0000, 0x0903, Vr::Us, "1", "ErrorID", "Error ID"),
    e(
        0x0000,
        0x1000,
        Vr::Ui,
        "1",
        "AffectedSOPInstanceUID",
        "Affected SOP Instance UID",
    ),
    e(
        0x0000,
        0x1001,
        Vr::Ui,
        "1",
        "RequestedSOPInstanceUID",
        "Requested SOP Instance UID",
    ),
    e(0x0000, 0x1002, Vr::Us, "1", "EventTypeID", "Event Type ID"),
    e(
        0x0000,
        0x1005,
        Vr::At,
        "1-n",
        "AttributeIdentifierList",
        "Attribute Identifier List",
    ),
    e(
        0x0000,
        0x1008,
        Vr::Us,
        "1",
        "ActionTypeID",
        "Action Type ID",
    ),
    e(
        0x0000,
        0x1020,
        Vr::Us,
        "1",
        "NumberOfRemainingSuboperations",
        "Number of Remaining Sub-operations",
    ),
    e(
        0x0000,
        0x1021,
        Vr::Us,
        "1",
        "NumberOfCompletedSuboperations",
        "Number of Completed Sub-operations",
    ),
    e(
        0x0000,
        0x1022,
        Vr::Us,
        "1",
        "NumberOfFailedSuboperations",
        "Number of Failed Sub-operations",
    ),
    e(
        0x0000,
        0x1023,
        Vr::Us,
        "1",
        "NumberOfWarningSuboperations",
        "Number of Warning Sub-operations",
    ),
    e(
        0x0000,
        0x1030,
        Vr::Ae,
        "1",
        "MoveOriginatorApplicationEntityTitle",
        "Move Originator Application Entity Title",
    ),
    e(
        0x0000,
        0x1031,
        Vr::Us,
        "1",
        "MoveOriginatorMessageID",
        "Move Originator Message ID",
    ),
    // -- File meta group (0002) ------------------------------------------
    e(
        0x0002,
        0x0000,
        Vr::Ul,
        "1",
        "FileMetaInformationGroupLength",
        "File Meta Information Group Length",
    ),
    e(
        0x0002,
        0x0001,
        Vr::Ob,
        "1",
        "FileMetaInformationVersion",
        "File Meta Information Version",
    ),
    e(
        0x0002,
        0x0002,
        Vr::Ui,
        "1",
        "MediaStorageSOPClassUID",
        "Media Storage SOP Class UID",
    ),
    e(
        0x0002,
        0x0003,
        Vr::Ui,
        "1",
        "MediaStorageSOPInstanceUID",
        "Media Storage SOP Instance UID",
    ),
    e(
        0x0002,
        0x0010,
        Vr::Ui,
        "1",
        "TransferSyntaxUID",
        "Transfer Syntax UID",
    ),
    e(
        0x0002,
        0x0012,
        Vr::Ui,
        "1",
        "ImplementationClassUID",
        "Implementation Class UID",
    ),
    e(
        0x0002,
        0x0013,
        Vr::Sh,
        "1",
        "ImplementationVersionName",
        "Implementation Version Name",
    ),
    e(
        0x0002,
        0x0016,
        Vr::Ae,
        "1",
        "SourceApplicationEntityTitle",
        "Source Application Entity Title",
    ),
    e(
        0x0002,
        0x0017,
        Vr::Ae,
        "1",
        "SendingApplicationEntityTitle",
        "Sending Application Entity Title",
    ),
    e(
        0x0002,
        0x0018,
        Vr::Ae,
        "1",
        "ReceivingApplicationEntityTitle",
        "Receiving Application Entity Title",
    ),
    e(
        0x0002,
        0x0026,
        Vr::Ur,
        "1",
        "SourcePresentationAddress",
        "Source Presentation Address",
    ),
    e(
        0x0002,
        0x0027,
        Vr::Ur,
        "1",
        "SendingPresentationAddress",
        "Sending Presentation Address",
    ),
    e(
        0x0002,
        0x0028,
        Vr::Ur,
        "1",
        "ReceivingPresentationAddress",
        "Receiving Presentation Address",
    ),
    e(
        0x0002,
        0x0100,
        Vr::Ui,
        "1",
        "PrivateInformationCreatorUID",
        "Private Information Creator UID",
    ),
    e(
        0x0002,
        0x0102,
        Vr::Ob,
        "1",
        "PrivateInformation",
        "Private Information",
    ),
    // -- SOP common / identifying (0008) ---------------------------------
    e(
        0x0008,
        0x0005,
        Vr::Cs,
        "1-n",
        "SpecificCharacterSet",
        "Specific Character Set",
    ),
    e(
        0x0008,
        0x0006,
        Vr::Sq,
        "1",
        "LanguageCodeSequence",
        "Language Code Sequence",
    ),
    e(0x0008, 0x0008, Vr::Cs, "2-n", "ImageType", "Image Type"),
    e(
        0x0008,
        0x0012,
        Vr::Da,
        "1",
        "InstanceCreationDate",
        "Instance Creation Date",
    ),
    e(
        0x0008,
        0x0013,
        Vr::Tm,
        "1",
        "InstanceCreationTime",
        "Instance Creation Time",
    ),
    e(
        0x0008,
        0x0014,
        Vr::Ui,
        "1",
        "InstanceCreatorUID",
        "Instance Creator UID",
    ),
    e(0x0008, 0x0016, Vr::Ui, "1", "SOPClassUID", "SOP Class UID"),
    e(
        0x0008,
        0x0018,
        Vr::Ui,
        "1",
        "SOPInstanceUID",
        "SOP Instance UID",
    ),
    e(0x0008, 0x0020, Vr::Da, "1", "StudyDate", "Study Date"),
    e(0x0008, 0x0021, Vr::Da, "1", "SeriesDate", "Series Date"),
    e(
        0x0008,
        0x0022,
        Vr::Da,
        "1",
        "AcquisitionDate",
        "Acquisition Date",
    ),
    e(0x0008, 0x0023, Vr::Da, "1", "ContentDate", "Content Date"),
    e(
        0x0008,
        0x002A,
        Vr::Dt,
        "1",
        "AcquisitionDateTime",
        "Acquisition DateTime",
    ),
    e(0x0008, 0x0030, Vr::Tm, "1", "StudyTime", "Study Time"),
    e(0x0008, 0x0031, Vr::Tm, "1", "SeriesTime", "Series Time"),
    e(
        0x0008,
        0x0032,
        Vr::Tm,
        "1",
        "AcquisitionTime",
        "Acquisition Time",
    ),
    e(0x0008, 0x0033, Vr::Tm, "1", "ContentTime", "Content Time"),
    e(
        0x0008,
        0x0050,
        Vr::Sh,
        "1",
        "AccessionNumber",
        "Accession Number",
    ),
    e(
        0x0008,
        0x0051,
        Vr::Sq,
        "1",
        "IssuerOfAccessionNumberSequence",
        "Issuer of Accession Number Sequence",
    ),
    e(
        0x0008,
        0x0052,
        Vr::Cs,
        "1",
        "QueryRetrieveLevel",
        "Query/Retrieve Level",
    ),
    e(
        0x0008,
        0x0054,
        Vr::Ae,
        "1-n",
        "RetrieveAETitle",
        "Retrieve AE Title",
    ),
    e(
        0x0008,
        0x0055,
        Vr::Ae,
        "1",
        "StationAETitle",
        "Station AE Title",
    ),
    e(
        0x0008,
        0x0056,
        Vr::Cs,
        "1",
        "InstanceAvailability",
        "Instance Availability",
    ),
    e(
        0x0008,
        0x0058,
        Vr::Ui,
        "1-n",
        "FailedSOPInstanceUIDList",
        "Failed SOP Instance UID List",
    ),
    e(0x0008, 0x0060, Vr::Cs, "1", "Modality", "Modality"),
    e(
        0x0008,
        0x0061,
        Vr::Cs,
        "1-n",
        "ModalitiesInStudy",
        "Modalities in Study",
    ),
    e(
        0x0008,
        0x0062,
        Vr::Ui,
        "1-n",
        "SOPClassesInStudy",
        "SOP Classes in Study",
    ),
    e(
        0x0008,
        0x0064,
        Vr::Cs,
        "1",
        "ConversionType",
        "Conversion Type",
    ),
    e(0x0008, 0x0070, Vr::Lo, "1", "Manufacturer", "Manufacturer"),
    e(
        0x0008,
        0x0080,
        Vr::Lo,
        "1",
        "InstitutionName",
        "Institution Name",
    ),
    e(
        0x0008,
        0x0081,
        Vr::St,
        "1",
        "InstitutionAddress",
        "Institution Address",
    ),
    e(
        0x0008,
        0x0082,
        Vr::Sq,
        "1",
        "InstitutionCodeSequence",
        "Institution Code Sequence",
    ),
    e(
        0x0008,
        0x0090,
        Vr::Pn,
        "1",
        "ReferringPhysicianName",
        "Referring Physician's Name",
    ),
    e(
        0x0008,
        0x0092,
        Vr::St,
        "1",
        "ReferringPhysicianAddress",
        "Referring Physician's Address",
    ),
    e(
        0x0008,
        0x0094,
        Vr::Sh,
        "1-n",
        "ReferringPhysicianTelephoneNumbers",
        "Referring Physician's Telephone Numbers",
    ),
    e(
        0x0008,
        0x0096,
        Vr::Sq,
        "1",
        "ReferringPhysicianIdentificationSequence",
        "Referring Physician Identification Sequence",
    ),
    e(0x0008, 0x0100, Vr::Sh, "1", "CodeValue", "Code Value"),
    e(
        0x0008,
        0x0102,
        Vr::Sh,
        "1",
        "CodingSchemeDesignator",
        "Coding Scheme Designator",
    ),
    e(
        0x0008,
        0x0103,
        Vr::Sh,
        "1",
        "CodingSchemeVersion",
        "Coding Scheme Version",
    ),
    e(0x0008, 0x0104, Vr::Lo, "1", "CodeMeaning", "Code Meaning"),
    e(
        0x0008,
        0x0105,
        Vr::Cs,
        "1",
        "MappingResource",
        "Mapping Resource",
    ),
    e(
        0x0008,
        0x0106,
        Vr::Dt,
        "1",
        "ContextGroupVersion",
        "Context Group Version",
    ),
    e(
        0x0008,
        0x0110,
        Vr::Sq,
        "1",
        "CodingSchemeIdentificationSequence",
        "Coding Scheme Identification Sequence",
    ),
    e(
        0x0008,
        0x0201,
        Vr::Sh,
        "1",
        "TimezoneOffsetFromUTC",
        "Timezone Offset From UTC",
    ),
    e(
        0x0008,
        0x1030,
        Vr::Lo,
        "1",
        "StudyDescription",
        "Study Description",
    ),
    e(
        0x0008,
        0x103E,
        Vr::Lo,
        "1",
        "SeriesDescription",
        "Series Description",
    ),
    e(
        0x0008,
        0x1040,
        Vr::Lo,
        "1",
        "InstitutionalDepartmentName",
        "Institutional Department Name",
    ),
    e(
        0x0008,
        0x1048,
        Vr::Pn,
        "1-n",
        "PhysiciansOfRecord",
        "Physician(s) of Record",
    ),
    e(
        0x0008,
        0x1050,
        Vr::Pn,
        "1-n",
        "PerformingPhysicianName",
        "Performing Physician's Name",
    ),
    e(
        0x0008,
        0x1060,
        Vr::Pn,
        "1-n",
        "NameOfPhysiciansReadingStudy",
        "Name of Physician(s) Reading Study",
    ),
    e(
        0x0008,
        0x1080,
        Vr::Lo,
        "1-n",
        "AdmittingDiagnosesDescription",
        "Admitting Diagnoses Description",
    ),
    e(
        0x0008,
        0x1090,
        Vr::Lo,
        "1",
        "ManufacturerModelName",
        "Manufacturer's Model Name",
    ),
    e(
        0x0008,
        0x1110,
        Vr::Sq,
        "1",
        "ReferencedStudySequence",
        "Referenced Study Sequence",
    ),
    e(
        0x0008,
        0x1111,
        Vr::Sq,
        "1",
        "ReferencedPerformedProcedureStepSequence",
        "Referenced Performed Procedure Step Sequence",
    ),
    e(
        0x0008,
        0x1120,
        Vr::Sq,
        "1",
        "ReferencedPatientSequence",
        "Referenced Patient Sequence",
    ),
    e(
        0x0008,
        0x1140,
        Vr::Sq,
        "1",
        "ReferencedImageSequence",
        "Referenced Image Sequence",
    ),
    e(
        0x0008,
        0x1150,
        Vr::Ui,
        "1",
        "ReferencedSOPClassUID",
        "Referenced SOP Class UID",
    ),
    e(
        0x0008,
        0x1155,
        Vr::Ui,
        "1",
        "ReferencedSOPInstanceUID",
        "Referenced SOP Instance UID",
    ),
    e(0x0008, 0x1190, Vr::Ur, "1", "RetrieveURL", "Retrieve URL"),
    e(
        0x0008,
        0x1195,
        Vr::Ui,
        "1",
        "TransactionUID",
        "Transaction UID",
    ),
    e(
        0x0008,
        0x1196,
        Vr::Us,
        "1",
        "WarningReason",
        "Warning Reason",
    ),
    e(
        0x0008,
        0x1197,
        Vr::Us,
        "1",
        "FailureReason",
        "Failure Reason",
    ),
    e(
        0x0008,
        0x1198,
        Vr::Sq,
        "1",
        "FailedSOPSequence",
        "Failed SOP Sequence",
    ),
    e(
        0x0008,
        0x1199,
        Vr::Sq,
        "1",
        "ReferencedSOPSequence",
        "Referenced SOP Sequence",
    ),
    e(
        0x0008,
        0x2111,
        Vr::St,
        "1",
        "DerivationDescription",
        "Derivation Description",
    ),
    e(
        0x0008,
        0x2112,
        Vr::Sq,
        "1",
        "SourceImageSequence",
        "Source Image Sequence",
    ),
    e(
        0x0008,
        0x2218,
        Vr::Sq,
        "1",
        "AnatomicRegionSequence",
        "Anatomic Region Sequence",
    ),
    // -- Patient (0010) --------------------------------------------------
    e(0x0010, 0x0010, Vr::Pn, "1", "PatientName", "Patient's Name"),
    e(0x0010, 0x0020, Vr::Lo, "1", "PatientID", "Patient ID"),
    e(
        0x0010,
        0x0021,
        Vr::Lo,
        "1",
        "IssuerOfPatientID",
        "Issuer of Patient ID",
    ),
    e(
        0x0010,
        0x0030,
        Vr::Da,
        "1",
        "PatientBirthDate",
        "Patient's Birth Date",
    ),
    e(
        0x0010,
        0x0032,
        Vr::Tm,
        "1",
        "PatientBirthTime",
        "Patient's Birth Time",
    ),
    e(0x0010, 0x0040, Vr::Cs, "1", "PatientSex", "Patient's Sex"),
    e(0x0010, 0x1010, Vr::As, "1", "PatientAge", "Patient's Age"),
    e(0x0010, 0x1020, Vr::Ds, "1", "PatientSize", "Patient's Size"),
    e(
        0x0010,
        0x1030,
        Vr::Ds,
        "1",
        "PatientWeight",
        "Patient's Weight",
    ),
    e(
        0x0010,
        0x1040,
        Vr::Lo,
        "1",
        "PatientAddress",
        "Patient's Address",
    ),
    e(
        0x0010,
        0x2000,
        Vr::Lo,
        "1-n",
        "MedicalAlerts",
        "Medical Alerts",
    ),
    e(0x0010, 0x2110, Vr::Lo, "1-n", "Allergies", "Allergies"),
    e(
        0x0010,
        0x2150,
        Vr::Lo,
        "1",
        "CountryOfResidence",
        "Country of Residence",
    ),
    e(
        0x0010,
        0x21B0,
        Vr::Lt,
        "1",
        "AdditionalPatientHistory",
        "Additional Patient History",
    ),
    e(
        0x0010,
        0x4000,
        Vr::Lt,
        "1",
        "PatientComments",
        "Patient Comments",
    ),
    // -- Acquisition (0018, ECG/PACS subset) -----------------------------
    e(
        0x0018,
        0x0015,
        Vr::Cs,
        "1",
        "BodyPartExamined",
        "Body Part Examined",
    ),
    e(
        0x0018,
        0x1068,
        Vr::Ds,
        "1",
        "MultiplexGroupTimeOffset",
        "Multiplex Group Time Offset",
    ),
    e(
        0x0018,
        0x5100,
        Vr::Cs,
        "1",
        "PatientPosition",
        "Patient Position",
    ),
    // -- Study / series / instance (0020) --------------------------------
    e(
        0x0020,
        0x000D,
        Vr::Ui,
        "1",
        "StudyInstanceUID",
        "Study Instance UID",
    ),
    e(
        0x0020,
        0x000E,
        Vr::Ui,
        "1",
        "SeriesInstanceUID",
        "Series Instance UID",
    ),
    e(0x0020, 0x0010, Vr::Sh, "1", "StudyID", "Study ID"),
    e(0x0020, 0x0011, Vr::Is, "1", "SeriesNumber", "Series Number"),
    e(
        0x0020,
        0x0012,
        Vr::Is,
        "1",
        "AcquisitionNumber",
        "Acquisition Number",
    ),
    e(
        0x0020,
        0x0013,
        Vr::Is,
        "1",
        "InstanceNumber",
        "Instance Number",
    ),
    e(
        0x0020,
        0x0020,
        Vr::Cs,
        "2",
        "PatientOrientation",
        "Patient Orientation",
    ),
    e(
        0x0020,
        0x0032,
        Vr::Ds,
        "3",
        "ImagePositionPatient",
        "Image Position (Patient)",
    ),
    e(
        0x0020,
        0x0037,
        Vr::Ds,
        "6",
        "ImageOrientationPatient",
        "Image Orientation (Patient)",
    ),
    e(
        0x0020,
        0x0052,
        Vr::Ui,
        "1",
        "FrameOfReferenceUID",
        "Frame of Reference UID",
    ),
    e(0x0020, 0x0060, Vr::Cs, "1", "Laterality", "Laterality"),
    e(
        0x0020,
        0x1002,
        Vr::Is,
        "1",
        "ImagesInAcquisition",
        "Images in Acquisition",
    ),
    e(
        0x0020,
        0x1206,
        Vr::Is,
        "1",
        "NumberOfStudyRelatedSeries",
        "Number of Study Related Series",
    ),
    e(
        0x0020,
        0x1208,
        Vr::Is,
        "1",
        "NumberOfStudyRelatedInstances",
        "Number of Study Related Instances",
    ),
    e(
        0x0020,
        0x1209,
        Vr::Is,
        "1",
        "NumberOfSeriesRelatedInstances",
        "Number of Series Related Instances",
    ),
    // -- Image / pixel description (0028) --------------------------------
    e(
        0x0028,
        0x0002,
        Vr::Us,
        "1",
        "SamplesPerPixel",
        "Samples per Pixel",
    ),
    e(
        0x0028,
        0x0004,
        Vr::Cs,
        "1",
        "PhotometricInterpretation",
        "Photometric Interpretation",
    ),
    e(
        0x0028,
        0x0006,
        Vr::Us,
        "1",
        "PlanarConfiguration",
        "Planar Configuration",
    ),
    e(
        0x0028,
        0x0008,
        Vr::Is,
        "1",
        "NumberOfFrames",
        "Number of Frames",
    ),
    e(
        0x0028,
        0x0009,
        Vr::At,
        "1-n",
        "FrameIncrementPointer",
        "Frame Increment Pointer",
    ),
    e(0x0028, 0x0010, Vr::Us, "1", "Rows", "Rows"),
    e(0x0028, 0x0011, Vr::Us, "1", "Columns", "Columns"),
    e(
        0x0028,
        0x0034,
        Vr::Is,
        "2",
        "PixelAspectRatio",
        "Pixel Aspect Ratio",
    ),
    e(
        0x0028,
        0x0100,
        Vr::Us,
        "1",
        "BitsAllocated",
        "Bits Allocated",
    ),
    e(0x0028, 0x0101, Vr::Us, "1", "BitsStored", "Bits Stored"),
    e(0x0028, 0x0102, Vr::Us, "1", "HighBit", "High Bit"),
    e(
        0x0028,
        0x0103,
        Vr::Us,
        "1",
        "PixelRepresentation",
        "Pixel Representation",
    ),
    e(
        0x0028,
        0x0106,
        Vr::Us,
        "1",
        "SmallestImagePixelValue",
        "Smallest Image Pixel Value",
    ),
    e(
        0x0028,
        0x0107,
        Vr::Us,
        "1",
        "LargestImagePixelValue",
        "Largest Image Pixel Value",
    ),
    e(
        0x0028,
        0x0120,
        Vr::Us,
        "1",
        "PixelPaddingValue",
        "Pixel Padding Value",
    ),
    e(
        0x0028,
        0x0301,
        Vr::Cs,
        "1",
        "BurnedInAnnotation",
        "Burned In Annotation",
    ),
    e(
        0x0028,
        0x1050,
        Vr::Ds,
        "1-n",
        "WindowCenter",
        "Window Center",
    ),
    e(0x0028, 0x1051, Vr::Ds, "1-n", "WindowWidth", "Window Width"),
    e(
        0x0028,
        0x1052,
        Vr::Ds,
        "1",
        "RescaleIntercept",
        "Rescale Intercept",
    ),
    e(0x0028, 0x1053, Vr::Ds, "1", "RescaleSlope", "Rescale Slope"),
    e(0x0028, 0x1054, Vr::Lo, "1", "RescaleType", "Rescale Type"),
    e(
        0x0028,
        0x2110,
        Vr::Cs,
        "1",
        "LossyImageCompression",
        "Lossy Image Compression",
    ),
    e(
        0x0028,
        0x2112,
        Vr::Ds,
        "1-n",
        "LossyImageCompressionRatio",
        "Lossy Image Compression Ratio",
    ),
    e(
        0x0028,
        0x2114,
        Vr::Cs,
        "1-n",
        "LossyImageCompressionMethod",
        "Lossy Image Compression Method",
    ),
    // -- Request / visit (0032, 0038) ------------------------------------
    e(
        0x0032,
        0x1032,
        Vr::Pn,
        "1",
        "RequestingPhysician",
        "Requesting Physician",
    ),
    e(
        0x0032,
        0x1033,
        Vr::Lo,
        "1",
        "RequestingService",
        "Requesting Service",
    ),
    e(
        0x0032,
        0x1064,
        Vr::Sq,
        "1",
        "RequestedProcedureCodeSequence",
        "Requested Procedure Code Sequence",
    ),
    e(
        0x0032,
        0x1070,
        Vr::Lo,
        "1",
        "RequestedContrastAgent",
        "Requested Contrast Agent",
    ),
    e(0x0038, 0x0010, Vr::Lo, "1", "AdmissionID", "Admission ID"),
    e(
        0x0038,
        0x0300,
        Vr::Lo,
        "1",
        "CurrentPatientLocation",
        "Current Patient Location",
    ),
    e(
        0x0038,
        0x0400,
        Vr::Lo,
        "1",
        "PatientInstitutionResidence",
        "Patient's Institution Residence",
    ),
    // -- Waveform (003A) -------------------------------------------------
    e(
        0x003A,
        0x0004,
        Vr::Cs,
        "1",
        "WaveformOriginality",
        "Waveform Originality",
    ),
    e(
        0x003A,
        0x0005,
        Vr::Us,
        "1",
        "NumberOfWaveformChannels",
        "Number of Waveform Channels",
    ),
    e(
        0x003A,
        0x0010,
        Vr::Ul,
        "1",
        "NumberOfWaveformSamples",
        "Number of Waveform Samples",
    ),
    e(
        0x003A,
        0x001A,
        Vr::Ds,
        "1",
        "SamplingFrequency",
        "Sampling Frequency",
    ),
    e(
        0x003A,
        0x0020,
        Vr::Sh,
        "1",
        "MultiplexGroupLabel",
        "Multiplex Group Label",
    ),
    e(
        0x003A,
        0x0200,
        Vr::Sq,
        "1",
        "ChannelDefinitionSequence",
        "Channel Definition Sequence",
    ),
    e(
        0x003A,
        0x0202,
        Vr::Is,
        "1",
        "WaveformChannelNumber",
        "Waveform Channel Number",
    ),
    e(0x003A, 0x0203, Vr::Sh, "1", "ChannelLabel", "Channel Label"),
    e(
        0x003A,
        0x0205,
        Vr::Cs,
        "1-n",
        "ChannelStatus",
        "Channel Status",
    ),
    e(
        0x003A,
        0x0208,
        Vr::Sq,
        "1",
        "ChannelSourceSequence",
        "Channel Source Sequence",
    ),
    e(
        0x003A,
        0x0209,
        Vr::Sq,
        "1",
        "ChannelSourceModifiersSequence",
        "Channel Source Modifiers Sequence",
    ),
    e(
        0x003A,
        0x020A,
        Vr::Sq,
        "1",
        "SourceWaveformSequence",
        "Source Waveform Sequence",
    ),
    e(
        0x003A,
        0x020C,
        Vr::Lo,
        "1",
        "ChannelDerivationDescription",
        "Channel Derivation Description",
    ),
    e(
        0x003A,
        0x0210,
        Vr::Ds,
        "1",
        "ChannelSensitivity",
        "Channel Sensitivity",
    ),
    e(
        0x003A,
        0x0211,
        Vr::Sq,
        "1",
        "ChannelSensitivityUnitsSequence",
        "Channel Sensitivity Units Sequence",
    ),
    e(
        0x003A,
        0x0212,
        Vr::Ds,
        "1",
        "ChannelSensitivityCorrectionFactor",
        "Channel Sensitivity Correction Factor",
    ),
    e(
        0x003A,
        0x0213,
        Vr::Ds,
        "1",
        "ChannelBaseline",
        "Channel Baseline",
    ),
    e(
        0x003A,
        0x0214,
        Vr::Ds,
        "1",
        "ChannelTimeSkew",
        "Channel Time Skew",
    ),
    e(
        0x003A,
        0x0215,
        Vr::Ds,
        "1",
        "ChannelSampleSkew",
        "Channel Sample Skew",
    ),
    e(
        0x003A,
        0x0218,
        Vr::Ds,
        "1",
        "ChannelOffset",
        "Channel Offset",
    ),
    e(
        0x003A,
        0x021A,
        Vr::Us,
        "1",
        "WaveformBitsStored",
        "Waveform Bits Stored",
    ),
    e(
        0x003A,
        0x0220,
        Vr::Ds,
        "1",
        "FilterLowFrequency",
        "Filter Low Frequency",
    ),
    e(
        0x003A,
        0x0221,
        Vr::Ds,
        "1",
        "FilterHighFrequency",
        "Filter High Frequency",
    ),
    e(
        0x003A,
        0x0222,
        Vr::Ds,
        "1",
        "NotchFilterFrequency",
        "Notch Filter Frequency",
    ),
    e(
        0x003A,
        0x0223,
        Vr::Ds,
        "1",
        "NotchFilterBandwidth",
        "Notch Filter Bandwidth",
    ),
    e(
        0x003A,
        0x0230,
        Vr::Fl,
        "1",
        "WaveformDataDisplayScale",
        "Waveform Data Display Scale",
    ),
    e(
        0x003A,
        0x0310,
        Vr::Ui,
        "1",
        "MultiplexGroupUID",
        "Multiplex Group UID",
    ),
    e(
        0x003A,
        0x0311,
        Vr::Ds,
        "1",
        "PowerlineFrequency",
        "Powerline Frequency",
    ),
    e(
        0x003A,
        0x0317,
        Vr::Cs,
        "1",
        "WaveformAmplifierType",
        "Waveform Amplifier Type",
    ),
    // -- Procedure steps / SR (0040) -------------------------------------
    e(
        0x0040,
        0x0001,
        Vr::Ae,
        "1-n",
        "ScheduledStationAETitle",
        "Scheduled Station AE Title",
    ),
    e(
        0x0040,
        0x0002,
        Vr::Da,
        "1",
        "ScheduledProcedureStepStartDate",
        "Scheduled Procedure Step Start Date",
    ),
    e(
        0x0040,
        0x0003,
        Vr::Tm,
        "1",
        "ScheduledProcedureStepStartTime",
        "Scheduled Procedure Step Start Time",
    ),
    e(
        0x0040,
        0x0006,
        Vr::Pn,
        "1",
        "ScheduledPerformingPhysicianName",
        "Scheduled Performing Physician's Name",
    ),
    e(
        0x0040,
        0x0007,
        Vr::Lo,
        "1",
        "ScheduledProcedureStepDescription",
        "Scheduled Procedure Step Description",
    ),
    e(
        0x0040,
        0x0008,
        Vr::Sq,
        "1",
        "ScheduledProtocolCodeSequence",
        "Scheduled Protocol Code Sequence",
    ),
    e(
        0x0040,
        0x0009,
        Vr::Sh,
        "1",
        "ScheduledProcedureStepID",
        "Scheduled Procedure Step ID",
    ),
    e(
        0x0040,
        0x0010,
        Vr::Sh,
        "1-n",
        "ScheduledStationName",
        "Scheduled Station Name",
    ),
    e(
        0x0040,
        0x0011,
        Vr::Sh,
        "1",
        "ScheduledProcedureStepLocation",
        "Scheduled Procedure Step Location",
    ),
    e(
        0x0040,
        0x0012,
        Vr::Lo,
        "1",
        "PreMedication",
        "Pre-Medication",
    ),
    e(
        0x0040,
        0x0020,
        Vr::Cs,
        "1",
        "ScheduledProcedureStepStatus",
        "Scheduled Procedure Step Status",
    ),
    e(
        0x0040,
        0x0100,
        Vr::Sq,
        "1",
        "ScheduledProcedureStepSequence",
        "Scheduled Procedure Step Sequence",
    ),
    e(
        0x0040,
        0x0241,
        Vr::Ae,
        "1",
        "PerformedStationAETitle",
        "Performed Station AE Title",
    ),
    e(
        0x0040,
        0x0242,
        Vr::Sh,
        "1",
        "PerformedStationName",
        "Performed Station Name",
    ),
    e(
        0x0040,
        0x0243,
        Vr::Sh,
        "1",
        "PerformedLocation",
        "Performed Location",
    ),
    e(
        0x0040,
        0x0244,
        Vr::Da,
        "1",
        "PerformedProcedureStepStartDate",
        "Performed Procedure Step Start Date",
    ),
    e(
        0x0040,
        0x0245,
        Vr::Tm,
        "1",
        "PerformedProcedureStepStartTime",
        "Performed Procedure Step Start Time",
    ),
    e(
        0x0040,
        0x0250,
        Vr::Da,
        "1",
        "PerformedProcedureStepEndDate",
        "Performed Procedure Step End Date",
    ),
    e(
        0x0040,
        0x0251,
        Vr::Tm,
        "1",
        "PerformedProcedureStepEndTime",
        "Performed Procedure Step End Time",
    ),
    e(
        0x0040,
        0x0252,
        Vr::Cs,
        "1",
        "PerformedProcedureStepStatus",
        "Performed Procedure Step Status",
    ),
    e(
        0x0040,
        0x0253,
        Vr::Sh,
        "1",
        "PerformedProcedureStepID",
        "Performed Procedure Step ID",
    ),
    e(
        0x0040,
        0x0254,
        Vr::Lo,
        "1",
        "PerformedProcedureStepDescription",
        "Performed Procedure Step Description",
    ),
    e(
        0x0040,
        0x0255,
        Vr::Lo,
        "1",
        "PerformedProcedureTypeDescription",
        "Performed Procedure Type Description",
    ),
    e(
        0x0040,
        0x0260,
        Vr::Sq,
        "1",
        "PerformedProtocolCodeSequence",
        "Performed Protocol Code Sequence",
    ),
    e(
        0x0040,
        0x0270,
        Vr::Sq,
        "1",
        "ScheduledStepAttributesSequence",
        "Scheduled Step Attributes Sequence",
    ),
    e(
        0x0040,
        0x0275,
        Vr::Sq,
        "1",
        "RequestAttributesSequence",
        "Request Attributes Sequence",
    ),
    e(
        0x0040,
        0x0280,
        Vr::St,
        "1",
        "CommentsOnThePerformedProcedureStep",
        "Comments on the Performed Procedure Step",
    ),
    e(
        0x0040,
        0x0340,
        Vr::Sq,
        "1",
        "PerformedSeriesSequence",
        "Performed Series Sequence",
    ),
    e(
        0x0040,
        0x0400,
        Vr::Lt,
        "1",
        "CommentsOnTheScheduledProcedureStep",
        "Comments on the Scheduled Procedure Step",
    ),
    e(
        0x0040,
        0x1001,
        Vr::Sh,
        "1",
        "RequestedProcedureID",
        "Requested Procedure ID",
    ),
    e(
        0x0040,
        0x1002,
        Vr::Lo,
        "1",
        "ReasonForTheRequestedProcedure",
        "Reason for the Requested Procedure",
    ),
    e(
        0x0040,
        0x1003,
        Vr::Sh,
        "1",
        "RequestedProcedurePriority",
        "Requested Procedure Priority",
    ),
    e(
        0x0040,
        0x1004,
        Vr::Lo,
        "1",
        "PatientTransportArrangements",
        "Patient Transport Arrangements",
    ),
    e(
        0x0040,
        0x1400,
        Vr::Lt,
        "1",
        "RequestedProcedureComments",
        "Requested Procedure Comments",
    ),
    e(
        0x0040,
        0x2004,
        Vr::Da,
        "1",
        "IssueDateOfImagingServiceRequest",
        "Issue Date of Imaging Service Request",
    ),
    e(
        0x0040,
        0x2005,
        Vr::Tm,
        "1",
        "IssueTimeOfImagingServiceRequest",
        "Issue Time of Imaging Service Request",
    ),
    e(
        0x0040,
        0x2016,
        Vr::Lo,
        "1",
        "PlacerOrderNumberImagingServiceRequest",
        "Placer Order Number / Imaging Service Request",
    ),
    e(
        0x0040,
        0x2017,
        Vr::Lo,
        "1",
        "FillerOrderNumberImagingServiceRequest",
        "Filler Order Number / Imaging Service Request",
    ),
    e(
        0x0040,
        0xA010,
        Vr::Cs,
        "1",
        "RelationshipType",
        "Relationship Type",
    ),
    e(0x0040, 0xA040, Vr::Cs, "1", "ValueType", "Value Type"),
    e(
        0x0040,
        0xA043,
        Vr::Sq,
        "1",
        "ConceptNameCodeSequence",
        "Concept Name Code Sequence",
    ),
    e(0x0040, 0xA120, Vr::Dt, "1", "DateTime", "DateTime"),
    e(0x0040, 0xA121, Vr::Da, "1", "Date", "Date"),
    e(0x0040, 0xA122, Vr::Tm, "1", "Time", "Time"),
    e(0x0040, 0xA123, Vr::Pn, "1", "PersonName", "Person Name"),
    e(0x0040, 0xA124, Vr::Ui, "1", "UID", "UID"),
    e(0x0040, 0xA160, Vr::Ut, "1", "TextValue", "Text Value"),
    e(
        0x0040,
        0xA168,
        Vr::Sq,
        "1",
        "ConceptCodeSequence",
        "Concept Code Sequence",
    ),
    e(
        0x0040,
        0xA300,
        Vr::Sq,
        "1",
        "MeasuredValueSequence",
        "Measured Value Sequence",
    ),
    e(
        0x0040,
        0xA30A,
        Vr::Ds,
        "1-n",
        "NumericValue",
        "Numeric Value",
    ),
    e(
        0x0040,
        0xA730,
        Vr::Sq,
        "1",
        "ContentSequence",
        "Content Sequence",
    ),
    // -- Encapsulated documents (0042) -----------------------------------
    e(
        0x0042,
        0x0010,
        Vr::St,
        "1",
        "DocumentTitle",
        "Document Title",
    ),
    e(
        0x0042,
        0x0011,
        Vr::Ob,
        "1",
        "EncapsulatedDocument",
        "Encapsulated Document",
    ),
    e(
        0x0042,
        0x0012,
        Vr::Lo,
        "1",
        "MIMETypeOfEncapsulatedDocument",
        "MIME Type of Encapsulated Document",
    ),
    // -- Waveform data (5400) --------------------------------------------
    e(
        0x5400,
        0x0100,
        Vr::Sq,
        "1",
        "WaveformSequence",
        "Waveform Sequence",
    ),
    e(
        0x5400,
        0x0110,
        Vr::Ow,
        "1",
        "ChannelMinimumValue",
        "Channel Minimum Value",
    ),
    e(
        0x5400,
        0x0112,
        Vr::Ow,
        "1",
        "ChannelMaximumValue",
        "Channel Maximum Value",
    ),
    e(
        0x5400,
        0x1004,
        Vr::Us,
        "1",
        "WaveformBitsAllocated",
        "Waveform Bits Allocated",
    ),
    e(
        0x5400,
        0x1006,
        Vr::Cs,
        "1",
        "WaveformSampleInterpretation",
        "Waveform Sample Interpretation",
    ),
    e(
        0x5400,
        0x100A,
        Vr::Ow,
        "1",
        "WaveformPaddingValue",
        "Waveform Padding Value",
    ),
    e(0x5400, 0x1010, Vr::Ow, "1", "WaveformData", "Waveform Data"),
    // -- Pixel data (7FE0) -----------------------------------------------
    e(
        0x7FE0,
        0x0001,
        Vr::Ov,
        "1",
        "ExtendedOffsetTable",
        "Extended Offset Table",
    ),
    e(
        0x7FE0,
        0x0002,
        Vr::Ov,
        "1",
        "ExtendedOffsetTableLengths",
        "Extended Offset Table Lengths",
    ),
    e(
        0x7FE0,
        0x0003,
        Vr::Uv,
        "1",
        "EncapsulatedPixelDataValueTotalLength",
        "Encapsulated Pixel Data Value Total Length",
    ),
    e(
        0x7FE0,
        0x0008,
        Vr::Of,
        "1",
        "FloatPixelData",
        "Float Pixel Data",
    ),
    e(
        0x7FE0,
        0x0009,
        Vr::Od,
        "1",
        "DoubleFloatPixelData",
        "Double Float Pixel Data",
    ),
    e(0x7FE0, 0x0010, Vr::Ow, "1", "PixelData", "Pixel Data"),
];

/// Look up a public dictionary entry by tag.
#[must_use]
pub fn lookup(tag: Tag) -> Option<&'static DictEntry> {
    ENTRIES
        .binary_search_by_key(&tag.to_u32(), |e| e.tag.to_u32())
        .ok()
        .map(|i| &ENTRIES[i])
}

/// Default VR for implicit-VR decoding, when the tag is known.
#[must_use]
pub fn dictionary_vr(tag: Tag) -> Option<Vr> {
    lookup(tag).map(|e| e.vr)
}

/// Look up a dictionary entry by keyword (exact case, e.g. `"PatientName"`).
#[must_use]
pub fn lookup_keyword(keyword: &str) -> Option<&'static DictEntry> {
    ENTRIES.iter().find(|e| e.keyword == keyword)
}

/// True for attributes whose real VR depends on sibling values
/// (`ox`/`px`/`xs` in PS3.6: pixel, waveform and min/max/padding values).
/// Well-known tags as constants.
#[allow(missing_docs)]
pub mod tags {
    use crate::Tag;

    macro_rules! tags {
        ($($name:ident = ($g:expr, $e:expr);)*) => {
            $(pub const $name: Tag = Tag::new($g, $e);)*
        };
    }

    tags! {
        // 0000: DIMSE commands
        COMMAND_GROUP_LENGTH = (0x0000, 0x0000);
        AFFECTED_SOP_CLASS_UID = (0x0000, 0x0002);
        REQUESTED_SOP_CLASS_UID = (0x0000, 0x0003);
        COMMAND_FIELD = (0x0000, 0x0100);
        MESSAGE_ID = (0x0000, 0x0110);
        MESSAGE_ID_BEING_RESPONDED_TO = (0x0000, 0x0120);
        MOVE_DESTINATION = (0x0000, 0x0600);
        PRIORITY = (0x0000, 0x0700);
        COMMAND_DATA_SET_TYPE = (0x0000, 0x0800);
        STATUS = (0x0000, 0x0900);
        OFFENDING_ELEMENT = (0x0000, 0x0901);
        ERROR_COMMENT = (0x0000, 0x0902);
        ERROR_ID = (0x0000, 0x0903);
        AFFECTED_SOP_INSTANCE_UID = (0x0000, 0x1000);
        REQUESTED_SOP_INSTANCE_UID = (0x0000, 0x1001);
        EVENT_TYPE_ID = (0x0000, 0x1002);
        ATTRIBUTE_IDENTIFIER_LIST = (0x0000, 0x1005);
        ACTION_TYPE_ID = (0x0000, 0x1008);
        NUMBER_OF_REMAINING_SUBOPERATIONS = (0x0000, 0x1020);
        NUMBER_OF_COMPLETED_SUBOPERATIONS = (0x0000, 0x1021);
        NUMBER_OF_FAILED_SUBOPERATIONS = (0x0000, 0x1022);
        NUMBER_OF_WARNING_SUBOPERATIONS = (0x0000, 0x1023);
        MOVE_ORIGINATOR_APPLICATION_ENTITY_TITLE = (0x0000, 0x1030);
        MOVE_ORIGINATOR_MESSAGE_ID = (0x0000, 0x1031);
        // 0002: file meta
        FILE_META_INFORMATION_GROUP_LENGTH = (0x0002, 0x0000);
        FILE_META_INFORMATION_VERSION = (0x0002, 0x0001);
        MEDIA_STORAGE_SOP_CLASS_UID = (0x0002, 0x0002);
        MEDIA_STORAGE_SOP_INSTANCE_UID = (0x0002, 0x0003);
        TRANSFER_SYNTAX_UID = (0x0002, 0x0010);
        IMPLEMENTATION_CLASS_UID = (0x0002, 0x0012);
        IMPLEMENTATION_VERSION_NAME = (0x0002, 0x0013);
        SOURCE_APPLICATION_ENTITY_TITLE = (0x0002, 0x0016);
        SENDING_APPLICATION_ENTITY_TITLE = (0x0002, 0x0017);
        RECEIVING_APPLICATION_ENTITY_TITLE = (0x0002, 0x0018);
        SOURCE_PRESENTATION_ADDRESS = (0x0002, 0x0026);
        SENDING_PRESENTATION_ADDRESS = (0x0002, 0x0027);
        RECEIVING_PRESENTATION_ADDRESS = (0x0002, 0x0028);
        PRIVATE_INFORMATION_CREATOR_UID = (0x0002, 0x0100);
        PRIVATE_INFORMATION = (0x0002, 0x0102);
        // 0008: SOP common / identifying
        SPECIFIC_CHARACTER_SET = (0x0008, 0x0005);
        LANGUAGE_CODE_SEQUENCE = (0x0008, 0x0006);
        IMAGE_TYPE = (0x0008, 0x0008);
        INSTANCE_CREATION_DATE = (0x0008, 0x0012);
        INSTANCE_CREATION_TIME = (0x0008, 0x0013);
        INSTANCE_CREATOR_UID = (0x0008, 0x0014);
        SOP_CLASS_UID = (0x0008, 0x0016);
        SOP_INSTANCE_UID = (0x0008, 0x0018);
        STUDY_DATE = (0x0008, 0x0020);
        SERIES_DATE = (0x0008, 0x0021);
        ACQUISITION_DATE = (0x0008, 0x0022);
        CONTENT_DATE = (0x0008, 0x0023);
        ACQUISITION_DATE_TIME = (0x0008, 0x002A);
        STUDY_TIME = (0x0008, 0x0030);
        SERIES_TIME = (0x0008, 0x0031);
        ACQUISITION_TIME = (0x0008, 0x0032);
        CONTENT_TIME = (0x0008, 0x0033);
        ACCESSION_NUMBER = (0x0008, 0x0050);
        ISSUER_OF_ACCESSION_NUMBER_SEQUENCE = (0x0008, 0x0051);
        QUERY_RETRIEVE_LEVEL = (0x0008, 0x0052);
        RETRIEVE_AE_TITLE = (0x0008, 0x0054);
        STATION_AE_TITLE = (0x0008, 0x0055);
        INSTANCE_AVAILABILITY = (0x0008, 0x0056);
        FAILED_SOP_INSTANCE_UID_LIST = (0x0008, 0x0058);
        MODALITY = (0x0008, 0x0060);
        MODALITIES_IN_STUDY = (0x0008, 0x0061);
        SOP_CLASSES_IN_STUDY = (0x0008, 0x0062);
        CONVERSION_TYPE = (0x0008, 0x0064);
        MANUFACTURER = (0x0008, 0x0070);
        INSTITUTION_NAME = (0x0008, 0x0080);
        INSTITUTION_ADDRESS = (0x0008, 0x0081);
        INSTITUTION_CODE_SEQUENCE = (0x0008, 0x0082);
        REFERRING_PHYSICIAN_NAME = (0x0008, 0x0090);
        REFERRING_PHYSICIAN_ADDRESS = (0x0008, 0x0092);
        REFERRING_PHYSICIAN_TELEPHONE_NUMBERS = (0x0008, 0x0094);
        REFERRING_PHYSICIAN_IDENTIFICATION_SEQUENCE = (0x0008, 0x0096);
        CODE_VALUE = (0x0008, 0x0100);
        CODING_SCHEME_DESIGNATOR = (0x0008, 0x0102);
        CODING_SCHEME_VERSION = (0x0008, 0x0103);
        CODE_MEANING = (0x0008, 0x0104);
        MAPPING_RESOURCE = (0x0008, 0x0105);
        CONTEXT_GROUP_VERSION = (0x0008, 0x0106);
        CODING_SCHEME_IDENTIFICATION_SEQUENCE = (0x0008, 0x0110);
        TIMEZONE_OFFSET_FROM_UTC = (0x0008, 0x0201);
        STUDY_DESCRIPTION = (0x0008, 0x1030);
        SERIES_DESCRIPTION = (0x0008, 0x103E);
        INSTITUTIONAL_DEPARTMENT_NAME = (0x0008, 0x1040);
        PHYSICIANS_OF_RECORD = (0x0008, 0x1048);
        PERFORMING_PHYSICIAN_NAME = (0x0008, 0x1050);
        NAME_OF_PHYSICIANS_READING_STUDY = (0x0008, 0x1060);
        ADMITTING_DIAGNOSES_DESCRIPTION = (0x0008, 0x1080);
        MANUFACTURER_MODEL_NAME = (0x0008, 0x1090);
        REFERENCED_STUDY_SEQUENCE = (0x0008, 0x1110);
        REFERENCED_PERFORMED_PROCEDURE_STEP_SEQUENCE = (0x0008, 0x1111);
        REFERENCED_PATIENT_SEQUENCE = (0x0008, 0x1120);
        REFERENCED_IMAGE_SEQUENCE = (0x0008, 0x1140);
        REFERENCED_SOP_CLASS_UID = (0x0008, 0x1150);
        REFERENCED_SOP_INSTANCE_UID = (0x0008, 0x1155);
        RETRIEVE_URL = (0x0008, 0x1190);
        TRANSACTION_UID = (0x0008, 0x1195);
        WARNING_REASON = (0x0008, 0x1196);
        FAILURE_REASON = (0x0008, 0x1197);
        FAILED_SOP_SEQUENCE = (0x0008, 0x1198);
        REFERENCED_SOP_SEQUENCE = (0x0008, 0x1199);
        DERIVATION_DESCRIPTION = (0x0008, 0x2111);
        SOURCE_IMAGE_SEQUENCE = (0x0008, 0x2112);
        ANATOMIC_REGION_SEQUENCE = (0x0008, 0x2218);
        // 0010: patient
        PATIENT_NAME = (0x0010, 0x0010);
        PATIENT_ID = (0x0010, 0x0020);
        ISSUER_OF_PATIENT_ID = (0x0010, 0x0021);
        PATIENT_BIRTH_DATE = (0x0010, 0x0030);
        PATIENT_BIRTH_TIME = (0x0010, 0x0032);
        PATIENT_SEX = (0x0010, 0x0040);
        PATIENT_AGE = (0x0010, 0x1010);
        PATIENT_SIZE = (0x0010, 0x1020);
        PATIENT_WEIGHT = (0x0010, 0x1030);
        PATIENT_ADDRESS = (0x0010, 0x1040);
        MEDICAL_ALERTS = (0x0010, 0x2000);
        ALLERGIES = (0x0010, 0x2110);
        COUNTRY_OF_RESIDENCE = (0x0010, 0x2150);
        ADDITIONAL_PATIENT_HISTORY = (0x0010, 0x21B0);
        PATIENT_COMMENTS = (0x0010, 0x4000);
        // 0018: acquisition subset
        BODY_PART_EXAMINED = (0x0018, 0x0015);
        MULTIPLEX_GROUP_TIME_OFFSET = (0x0018, 0x1068);
        PATIENT_POSITION = (0x0018, 0x5100);
        // 0020: study / series / instance
        STUDY_INSTANCE_UID = (0x0020, 0x000D);
        SERIES_INSTANCE_UID = (0x0020, 0x000E);
        STUDY_ID = (0x0020, 0x0010);
        SERIES_NUMBER = (0x0020, 0x0011);
        ACQUISITION_NUMBER = (0x0020, 0x0012);
        INSTANCE_NUMBER = (0x0020, 0x0013);
        PATIENT_ORIENTATION = (0x0020, 0x0020);
        IMAGE_POSITION_PATIENT = (0x0020, 0x0032);
        IMAGE_ORIENTATION_PATIENT = (0x0020, 0x0037);
        FRAME_OF_REFERENCE_UID = (0x0020, 0x0052);
        LATERALITY = (0x0020, 0x0060);
        IMAGES_IN_ACQUISITION = (0x0020, 0x1002);
        NUMBER_OF_STUDY_RELATED_SERIES = (0x0020, 0x1206);
        NUMBER_OF_STUDY_RELATED_INSTANCES = (0x0020, 0x1208);
        NUMBER_OF_SERIES_RELATED_INSTANCES = (0x0020, 0x1209);
        // 0028: image / pixel description
        SAMPLES_PER_PIXEL = (0x0028, 0x0002);
        PHOTOMETRIC_INTERPRETATION = (0x0028, 0x0004);
        PLANAR_CONFIGURATION = (0x0028, 0x0006);
        NUMBER_OF_FRAMES = (0x0028, 0x0008);
        FRAME_INCREMENT_POINTER = (0x0028, 0x0009);
        ROWS = (0x0028, 0x0010);
        COLUMNS = (0x0028, 0x0011);
        PIXEL_ASPECT_RATIO = (0x0028, 0x0034);
        BITS_ALLOCATED = (0x0028, 0x0100);
        BITS_STORED = (0x0028, 0x0101);
        HIGH_BIT = (0x0028, 0x0102);
        PIXEL_REPRESENTATION = (0x0028, 0x0103);
        SMALLEST_IMAGE_PIXEL_VALUE = (0x0028, 0x0106);
        LARGEST_IMAGE_PIXEL_VALUE = (0x0028, 0x0107);
        PIXEL_PADDING_VALUE = (0x0028, 0x0120);
        BURNED_IN_ANNOTATION = (0x0028, 0x0301);
        WINDOW_CENTER = (0x0028, 0x1050);
        WINDOW_WIDTH = (0x0028, 0x1051);
        RESCALE_INTERCEPT = (0x0028, 0x1052);
        RESCALE_SLOPE = (0x0028, 0x1053);
        RESCALE_TYPE = (0x0028, 0x1054);
        LOSSY_IMAGE_COMPRESSION = (0x0028, 0x2110);
        LOSSY_IMAGE_COMPRESSION_RATIO = (0x0028, 0x2112);
        LOSSY_IMAGE_COMPRESSION_METHOD = (0x0028, 0x2114);
        // 0032/0038: request / visit
        REQUESTING_PHYSICIAN = (0x0032, 0x1032);
        REQUESTING_SERVICE = (0x0032, 0x1033);
        REQUESTED_PROCEDURE_CODE_SEQUENCE = (0x0032, 0x1064);
        REQUESTED_CONTRAST_AGENT = (0x0032, 0x1070);
        ADMISSION_ID = (0x0038, 0x0010);
        CURRENT_PATIENT_LOCATION = (0x0038, 0x0300);
        PATIENT_INSTITUTION_RESIDENCE = (0x0038, 0x0400);
        // 003A: waveform
        WAVEFORM_ORIGINALITY = (0x003A, 0x0004);
        NUMBER_OF_WAVEFORM_CHANNELS = (0x003A, 0x0005);
        NUMBER_OF_WAVEFORM_SAMPLES = (0x003A, 0x0010);
        SAMPLING_FREQUENCY = (0x003A, 0x001A);
        MULTIPLEX_GROUP_LABEL = (0x003A, 0x0020);
        CHANNEL_DEFINITION_SEQUENCE = (0x003A, 0x0200);
        WAVEFORM_CHANNEL_NUMBER = (0x003A, 0x0202);
        CHANNEL_LABEL = (0x003A, 0x0203);
        CHANNEL_STATUS = (0x003A, 0x0205);
        CHANNEL_SOURCE_SEQUENCE = (0x003A, 0x0208);
        CHANNEL_SOURCE_MODIFIERS_SEQUENCE = (0x003A, 0x0209);
        SOURCE_WAVEFORM_SEQUENCE = (0x003A, 0x020A);
        CHANNEL_DERIVATION_DESCRIPTION = (0x003A, 0x020C);
        CHANNEL_SENSITIVITY = (0x003A, 0x0210);
        CHANNEL_SENSITIVITY_UNITS_SEQUENCE = (0x003A, 0x0211);
        CHANNEL_SENSITIVITY_CORRECTION_FACTOR = (0x003A, 0x0212);
        CHANNEL_BASELINE = (0x003A, 0x0213);
        CHANNEL_TIME_SKEW = (0x003A, 0x0214);
        CHANNEL_SAMPLE_SKEW = (0x003A, 0x0215);
        CHANNEL_OFFSET = (0x003A, 0x0218);
        WAVEFORM_BITS_STORED = (0x003A, 0x021A);
        FILTER_LOW_FREQUENCY = (0x003A, 0x0220);
        FILTER_HIGH_FREQUENCY = (0x003A, 0x0221);
        NOTCH_FILTER_FREQUENCY = (0x003A, 0x0222);
        NOTCH_FILTER_BANDWIDTH = (0x003A, 0x0223);
        WAVEFORM_DATA_DISPLAY_SCALE = (0x003A, 0x0230);
        MULTIPLEX_GROUP_UID = (0x003A, 0x0310);
        POWERLINE_FREQUENCY = (0x003A, 0x0311);
        WAVEFORM_AMPLIFIER_TYPE = (0x003A, 0x0317);
        // 0040: procedure steps / SR
        SCHEDULED_STATION_AE_TITLE = (0x0040, 0x0001);
        SCHEDULED_PROCEDURE_STEP_START_DATE = (0x0040, 0x0002);
        SCHEDULED_PROCEDURE_STEP_START_TIME = (0x0040, 0x0003);
        SCHEDULED_PERFORMING_PHYSICIAN_NAME = (0x0040, 0x0006);
        SCHEDULED_PROCEDURE_STEP_DESCRIPTION = (0x0040, 0x0007);
        SCHEDULED_PROTOCOL_CODE_SEQUENCE = (0x0040, 0x0008);
        SCHEDULED_PROCEDURE_STEP_ID = (0x0040, 0x0009);
        SCHEDULED_STATION_NAME = (0x0040, 0x0010);
        SCHEDULED_PROCEDURE_STEP_LOCATION = (0x0040, 0x0011);
        PRE_MEDICATION = (0x0040, 0x0012);
        SCHEDULED_PROCEDURE_STEP_STATUS = (0x0040, 0x0020);
        SCHEDULED_PROCEDURE_STEP_SEQUENCE = (0x0040, 0x0100);
        PERFORMED_STATION_AE_TITLE = (0x0040, 0x0241);
        PERFORMED_STATION_NAME = (0x0040, 0x0242);
        PERFORMED_LOCATION = (0x0040, 0x0243);
        PERFORMED_PROCEDURE_STEP_START_DATE = (0x0040, 0x0244);
        PERFORMED_PROCEDURE_STEP_START_TIME = (0x0040, 0x0245);
        PERFORMED_PROCEDURE_STEP_END_DATE = (0x0040, 0x0250);
        PERFORMED_PROCEDURE_STEP_END_TIME = (0x0040, 0x0251);
        PERFORMED_PROCEDURE_STEP_STATUS = (0x0040, 0x0252);
        PERFORMED_PROCEDURE_STEP_ID = (0x0040, 0x0253);
        PERFORMED_PROCEDURE_STEP_DESCRIPTION = (0x0040, 0x0254);
        PERFORMED_PROCEDURE_TYPE_DESCRIPTION = (0x0040, 0x0255);
        PERFORMED_PROTOCOL_CODE_SEQUENCE = (0x0040, 0x0260);
        SCHEDULED_STEP_ATTRIBUTES_SEQUENCE = (0x0040, 0x0270);
        REQUEST_ATTRIBUTES_SEQUENCE = (0x0040, 0x0275);
        COMMENTS_ON_THE_PERFORMED_PROCEDURE_STEP = (0x0040, 0x0280);
        PERFORMED_SERIES_SEQUENCE = (0x0040, 0x0340);
        COMMENTS_ON_THE_SCHEDULED_PROCEDURE_STEP = (0x0040, 0x0400);
        REQUESTED_PROCEDURE_ID = (0x0040, 0x1001);
        REASON_FOR_THE_REQUESTED_PROCEDURE = (0x0040, 0x1002);
        REQUESTED_PROCEDURE_PRIORITY = (0x0040, 0x1003);
        PATIENT_TRANSPORT_ARRANGEMENTS = (0x0040, 0x1004);
        REQUESTED_PROCEDURE_COMMENTS = (0x0040, 0x1400);
        ISSUE_DATE_OF_IMAGING_SERVICE_REQUEST = (0x0040, 0x2004);
        ISSUE_TIME_OF_IMAGING_SERVICE_REQUEST = (0x0040, 0x2005);
        PLACER_ORDER_NUMBER_IMAGING_SERVICE_REQUEST = (0x0040, 0x2016);
        FILLER_ORDER_NUMBER_IMAGING_SERVICE_REQUEST = (0x0040, 0x2017);
        RELATIONSHIP_TYPE = (0x0040, 0xA010);
        VALUE_TYPE = (0x0040, 0xA040);
        CONCEPT_NAME_CODE_SEQUENCE = (0x0040, 0xA043);
        DATE_TIME = (0x0040, 0xA120);
        DATE = (0x0040, 0xA121);
        TIME = (0x0040, 0xA122);
        PERSON_NAME = (0x0040, 0xA123);
        UID = (0x0040, 0xA124);
        TEXT_VALUE = (0x0040, 0xA160);
        CONCEPT_CODE_SEQUENCE = (0x0040, 0xA168);
        MEASURED_VALUE_SEQUENCE = (0x0040, 0xA300);
        NUMERIC_VALUE = (0x0040, 0xA30A);
        CONTENT_SEQUENCE = (0x0040, 0xA730);
        // 0042: encapsulated documents
        DOCUMENT_TITLE = (0x0042, 0x0010);
        ENCAPSULATED_DOCUMENT = (0x0042, 0x0011);
        MIME_TYPE_OF_ENCAPSULATED_DOCUMENT = (0x0042, 0x0012);
        // 5400: waveform data
        WAVEFORM_SEQUENCE = (0x5400, 0x0100);
        CHANNEL_MINIMUM_VALUE = (0x5400, 0x0110);
        CHANNEL_MAXIMUM_VALUE = (0x5400, 0x0112);
        WAVEFORM_BITS_ALLOCATED = (0x5400, 0x1004);
        WAVEFORM_SAMPLE_INTERPRETATION = (0x5400, 0x1006);
        WAVEFORM_PADDING_VALUE = (0x5400, 0x100A);
        WAVEFORM_DATA = (0x5400, 0x1010);
        // 7FE0: pixel data
        EXTENDED_OFFSET_TABLE = (0x7FE0, 0x0001);
        EXTENDED_OFFSET_TABLE_LENGTHS = (0x7FE0, 0x0002);
        ENCAPSULATED_PIXEL_DATA_VALUE_TOTAL_LENGTH = (0x7FE0, 0x0003);
        FLOAT_PIXEL_DATA = (0x7FE0, 0x0008);
        DOUBLE_FLOAT_PIXEL_DATA = (0x7FE0, 0x0009);
        PIXEL_DATA = (0x7FE0, 0x0010);
    }
}

/// True for attributes whose real VR depends on sibling values
/// (`ox`/`px`/`xs` in PS3.6: pixel, waveform and min/max/padding values).
#[must_use]
pub const fn is_context_dependent_vr(tag: Tag) -> bool {
    matches!(
        (tag.0, tag.1),
        (0x0028, 0x0106)
            | (0x0028, 0x0107)
            | (0x0028, 0x0120)
            | (0x5400, 0x0110)
            | (0x5400, 0x0112)
            | (0x5400, 0x100A)
            | (0x5400, 0x1010)
            | (0x7FE0, 0x0010)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tags;

    #[test]
    fn entries_are_sorted_unique_and_complete() {
        assert!(ENTRIES.len() > 200, "dictionary shrank unexpectedly");
        let mut prev: Option<u32> = None;
        for (i, e) in ENTRIES.iter().enumerate() {
            let cur = e.tag.to_u32();
            if let Some(p) = prev {
                assert!(cur > p, "entry {i} ({}) out of order", e.tag);
            }
            prev = Some(cur);
        }
    }

    #[test]
    fn lookup_finds_verified_entries() {
        let e = lookup(tags::PATIENT_NAME).unwrap();
        assert_eq!(
            (e.vr, e.keyword, e.name),
            (Vr::Pn, "PatientName", "Patient's Name")
        );
        let e = lookup(tags::WAVEFORM_BITS_ALLOCATED).unwrap();
        assert_eq!((e.vr, e.vm), (Vr::Us, "1"));
        let e = lookup(tags::PIXEL_DATA).unwrap();
        assert_eq!(e.keyword, "PixelData");
        assert!(lookup(Tag::new(0x0019, 0x1001)).is_none());
        assert!(lookup(Tag::new(0x0009, 0x0010)).is_none());
    }

    #[test]
    fn every_tag_constant_resolves() {
        // Spot-check one constant per group; full coverage would be noise.
        for tag in [
            tags::COMMAND_FIELD,
            tags::TRANSFER_SYNTAX_UID,
            tags::SOP_INSTANCE_UID,
            tags::PATIENT_ID,
            tags::PATIENT_POSITION,
            tags::STUDY_INSTANCE_UID,
            tags::BITS_ALLOCATED,
            tags::REQUESTING_PHYSICIAN,
            tags::ADMISSION_ID,
            tags::SAMPLING_FREQUENCY,
            tags::SCHEDULED_PROCEDURE_STEP_SEQUENCE,
            tags::ENCAPSULATED_DOCUMENT,
            tags::WAVEFORM_DATA,
            tags::PIXEL_DATA,
        ] {
            assert!(lookup(tag).is_some(), "{tag}");
        }
    }
}
