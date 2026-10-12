//! `pdf2dcm`: wrap a PDF in an Encapsulated PDF Part-10 file.
//!
//! Exit status: 0 on success, 1 on failure. The built instance is
//! validated against its IOD before writing; validation errors abort.

use std::path::PathBuf;

use clap::{Parser, ValueEnum};
use vyna_dcm::{
    PersonName, TransferSyntax,
    pdf::{ConceptName, PdfInstanceMeta, file_meta_for_dataset, pdf_to_dataset},
    validate, write_part10,
};

/// PDF-to-DICOM encapsulation tool.
#[derive(Parser, Debug)]
#[command(name = "pdf2dcm", about = "Wrap a PDF in an Encapsulated PDF file")]
struct Args {
    /// PDF file to encapsulate.
    input: PathBuf,
    /// Part-10 file to write.
    output: PathBuf,
    /// Patient name (`Family^Given`).
    #[arg(long, default_value = "ANONYMOUS")]
    patient_name: String,
    /// Patient ID.
    #[arg(long, default_value = "")]
    patient_id: String,
    /// Study Instance UID (generated when absent).
    #[arg(long)]
    study_uid: Option<String>,
    /// Series Instance UID (generated when absent).
    #[arg(long)]
    series_uid: Option<String>,
    /// SOP Instance UID (generated when absent).
    #[arg(long)]
    sop_uid: Option<String>,
    /// Patient birth date `YYYYMMDD`.
    #[arg(long)]
    birth_date: Option<String>,
    /// Patient sex (`M`/`F`/`O`).
    #[arg(long)]
    sex: Option<String>,
    /// Study ID.
    #[arg(long)]
    study_id: Option<String>,
    /// Accession number.
    #[arg(long)]
    accession: Option<String>,
    /// Study date `YYYYMMDD`.
    #[arg(long)]
    study_date: Option<String>,
    /// Study time `HHMMSS`.
    #[arg(long)]
    study_time: Option<String>,
    /// Content date `YYYYMMDD`.
    #[arg(long)]
    content_date: Option<String>,
    /// Content time `HHMMSS`.
    #[arg(long)]
    content_time: Option<String>,
    /// Acquisition date/time `YYYYMMDDHHMMSS`.
    #[arg(long)]
    acquisition_datetime: Option<String>,
    /// Referring physician's name.
    #[arg(long)]
    referring_physician: Option<String>,
    /// Document title (default: input file name).
    #[arg(long)]
    title: Option<String>,
    /// Concept Name code value (include the sequence when given).
    #[arg(long)]
    concept_code: Option<String>,
    /// Concept Name coding scheme.
    #[arg(long, default_value = "99VYNA")]
    concept_scheme: String,
    /// Concept Name meaning (default: the code value).
    #[arg(long)]
    concept_meaning: Option<String>,
    /// Modality.
    #[arg(long, default_value = "OT")]
    modality: String,
    /// Conversion Type.
    #[arg(long, default_value = "WSD")]
    conversion_type: String,
    /// Burned In Annotation.
    #[arg(long, default_value = "YES")]
    burned_in: String,
    /// Institution name.
    #[arg(long)]
    institution: Option<String>,
    /// Manufacturer.
    #[arg(long, default_value = "Vyna")]
    manufacturer: String,
    /// Manufacturer model name.
    #[arg(long, default_value = "VynaDCM")]
    model: String,
    /// Series description.
    #[arg(long)]
    series_description: Option<String>,
    /// Series number.
    #[arg(long, default_value = "1")]
    series_number: String,
    /// Instance number.
    #[arg(long, default_value = "1")]
    instance_number: String,
    /// Dataset transfer syntax.
    #[arg(long, value_enum, default_value = "explicit")]
    transfer_syntax: Syntax,
}

/// Writable dataset transfer syntaxes.
#[derive(Clone, Copy, Debug, ValueEnum)]
enum Syntax {
    /// Explicit VR Little Endian.
    Explicit,
    /// Implicit VR Little Endian.
    Implicit,
    /// Deflated Explicit VR Little Endian.
    Deflated,
}

impl Syntax {
    fn transfer_syntax(self) -> TransferSyntax {
        match self {
            Syntax::Explicit => TransferSyntax::ExplicitVrLittleEndian,
            Syntax::Implicit => TransferSyntax::ImplicitVrLittleEndian,
            Syntax::Deflated => TransferSyntax::DeflatedExplicitVrLittleEndian,
        }
    }
}

fn main() {
    std::process::exit(run(&Args::parse()));
}

fn run(args: &Args) -> i32 {
    let pdf = match std::fs::read(&args.input) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("pdf2dcm: {}: {e}", args.input.display());
            return 1;
        }
    };
    if !pdf.starts_with(b"%PDF-") {
        eprintln!(
            "pdf2dcm: warning: {} does not start with %PDF- magic",
            args.input.display()
        );
    }
    let mut meta = PdfInstanceMeta::new(
        PersonName::parse(&args.patient_name),
        args.patient_id.clone(),
    );
    meta.sop_instance_uid = args.sop_uid.clone().unwrap_or_default();
    meta.study_uid = args.study_uid.clone().unwrap_or_default();
    meta.series_uid = args.series_uid.clone().unwrap_or_default();
    meta.birth_date = args.birth_date.clone();
    meta.sex = args.sex.clone();
    meta.study_id = args.study_id.clone();
    meta.accession = args.accession.clone();
    meta.study_date = args.study_date.clone();
    meta.study_time = args.study_time.clone();
    meta.content_date = args.content_date.clone();
    meta.content_time = args.content_time.clone();
    meta.acquisition_datetime = args.acquisition_datetime.clone();
    meta.referring_physician = args.referring_physician.as_deref().map(PersonName::parse);
    meta.title = args.title.clone().or_else(|| {
        args.input
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
    });
    meta.concept_name = args.concept_code.clone().map(|code| {
        let meaning = args.concept_meaning.clone().unwrap_or_else(|| code.clone());
        ConceptName::new(code, args.concept_scheme.clone(), meaning)
    });
    meta.modality = args.modality.clone();
    meta.conversion_type = args.conversion_type.clone();
    meta.burned_in_annotation = args.burned_in.clone();
    meta.institution = args.institution.clone();
    meta.manufacturer = args.manufacturer.clone();
    meta.model = args.model.clone();
    meta.series_description = args.series_description.clone();
    meta.series_number = args.series_number.clone();
    meta.instance_number = args.instance_number.clone();

    let ts = args.transfer_syntax.transfer_syntax();
    let ds = match pdf_to_dataset(&pdf, &meta) {
        Ok(ds) => ds,
        Err(e) => {
            eprintln!("pdf2dcm: {e}");
            return 1;
        }
    };
    let sop_class = ds
        .string(vyna_dcm::tags::SOP_CLASS_UID)
        .unwrap_or_default()
        .to_string();
    match validate(&ds, &sop_class, Some(ts)) {
        Some(report) if report.has_errors() => {
            for f in &report.findings {
                eprintln!("pdf2dcm: validation: {f}");
            }
            return 1;
        }
        None => {
            eprintln!("pdf2dcm: validation: unsupported SOP Class {sop_class}");
            return 1;
        }
        _ => {}
    }
    let file = match file_meta_for_dataset(&ds, ts)
        .map_err(|e| e.to_string())
        .and_then(|m| write_part10(&m, &ds).map_err(|e| e.to_string()))
    {
        Ok(b) => b,
        Err(e) => {
            eprintln!("pdf2dcm: {e}");
            return 1;
        }
    };
    if let Err(e) = std::fs::write(&args.output, &file) {
        eprintln!("pdf2dcm: {}: {e}", args.output.display());
        return 1;
    }
    println!(
        "{}: {} PDF bytes -> {} (SOP Instance {})",
        args.output.display(),
        pdf.len(),
        ts.uid(),
        ds.string(vyna_dcm::tags::SOP_INSTANCE_UID)
            .unwrap_or_default()
    );
    0
}
