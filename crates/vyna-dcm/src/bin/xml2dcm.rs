//! `xml2dcm`: convert Native DICOM Model XML back to a Part-10 file.

#[path = "iocommon.rs"]
mod iocommon;

use std::path::PathBuf;

use clap::Parser;
use vyna_dcm::{
    FileMeta, TransferSyntax, dataset_from_xml, dataset_from_xml_with_resolver, tags, write_part10,
};

/// XML-to-DICOM tool.
#[derive(Parser, Debug)]
#[command(name = "xml2dcm", about = "Convert DICOM XML to a Part-10 file")]
struct Args {
    /// DICOM XML file to convert.
    file: PathBuf,
    /// Output Part-10 file.
    #[arg(short, long)]
    output: PathBuf,
    /// Transfer syntax UID for the dataset.
    #[arg(long, default_value = "1.2.840.10008.1.2.1")]
    transfer_syntax: String,
    /// Resolve `<BulkData uri="..."/>` from files under this directory
    /// (bare relative paths, or `file:` URIs re-anchored under it;
    /// absolute bare paths, `..` segments, and network schemes are
    /// rejected).
    #[arg(long)]
    bulk_data_dir: Option<PathBuf>,
}

fn main() {
    let args = Args::parse();
    match run(&args) {
        Ok(()) => {}
        Err(e) => {
            eprintln!("xml2dcm: {e}");
            std::process::exit(1);
        }
    }
}

/// Resolve one bulk URI to bytes under `dir`: `file:` URIs
/// re-anchor under `dir` (scheme, `//host`, and leading slashes
/// dropped); bare URIs must already be relative. Network schemes,
/// absolute bare paths, and `..` segments are rejected — stage the
/// files under `dir` instead.
fn resolve_bulk_file(dir: &std::path::Path, uri: &str) -> Result<Vec<u8>, vyna_dcm::codec::Error> {
    let err = |m: String| vyna_dcm::codec::Error::Xml(m);
    let rel = match uri.strip_prefix("file:") {
        Some(p) => {
            let p = p
                .strip_prefix("//")
                .and_then(|a| a.split_once('/').map(|(_, q)| q))
                .unwrap_or(p);
            p.trim_start_matches('/')
        }
        None => {
            if uri.contains("://") {
                return Err(err(format!(
                    "bulk uri {uri:?} is not a local file reference (stage it under {})",
                    dir.display()
                )));
            }
            if uri.starts_with('/') {
                return Err(err(format!(
                    "bulk uri {uri:?} is absolute (stage it under {})",
                    dir.display()
                )));
            }
            uri
        }
    };
    let mut out = dir.to_path_buf();
    for seg in rel.split('/') {
        if seg.is_empty() || seg == "." {
            continue;
        }
        if seg == ".." {
            return Err(err(format!("bulk uri {uri:?} escapes {}", dir.display())));
        }
        out.push(seg);
    }
    std::fs::read(&out).map_err(|e| err(format!("bulk uri {uri:?} ({}): {e}", out.display())))
}

fn run(args: &Args) -> Result<(), String> {
    let text = std::fs::read_to_string(&args.file).map_err(|e| e.to_string())?;
    let mut ds = match &args.bulk_data_dir {
        Some(dir) => dataset_from_xml_with_resolver(&text, |uri| resolve_bulk_file(dir, uri))
            .map_err(|e| e.to_string())?,
        None => dataset_from_xml(&text).map_err(|e| e.to_string())?,
    };
    let ts = TransferSyntax::from_uid(&args.transfer_syntax)
        .ok_or_else(|| format!("unknown transfer syntax {}", args.transfer_syntax))?;
    // Encapsulated syntaxes need fragments, not bytes: re-fragment Pixel
    // Data when the XML carried InlineBinary bytes.
    if ts.is_encapsulated()
        && let Some(el) = ds.get(tags::PIXEL_DATA).cloned()
        && let vyna_dcm::Value::Bytes(b) = &el.value
    {
        let b = b.clone();
        ds.remove(tags::PIXEL_DATA);
        ds.insert(vyna_dcm::DataElement::new(
            tags::PIXEL_DATA,
            el.vr,
            vyna_dcm::Value::Fragments(vyna_dcm::codec::fragment_payload(&b, 65536)),
        ));
    }
    let class = ds
        .string(tags::SOP_CLASS_UID)
        .ok_or("dataset without SOP Class UID")?
        .to_string();
    let inst = ds
        .string(tags::SOP_INSTANCE_UID)
        .ok_or("dataset without SOP Instance UID")?
        .to_string();
    let meta = FileMeta::new(class, inst, ts);
    let bytes = write_part10(&meta, &ds).map_err(|e| e.to_string())?;
    iocommon::write_atomic(&args.output, &bytes).map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bulk_file_policy_accepts_relative_and_rejects_escapes() {
        let dir = std::env::temp_dir().join(format!("xml2dcm-bulk-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("px.bin"), b"\x01\x02").unwrap();
        std::fs::write(dir.join("sub").join("f.bin"), b"\x03").unwrap();
        // Accepted forms.
        assert_eq!(resolve_bulk_file(&dir, "px.bin").unwrap(), b"\x01\x02");
        assert_eq!(resolve_bulk_file(&dir, "sub/f.bin").unwrap(), b"\x03");
        assert_eq!(resolve_bulk_file(&dir, "file:px.bin").unwrap(), b"\x01\x02");
        assert_eq!(
            resolve_bulk_file(&dir, "file://host/sub/f.bin").unwrap(),
            b"\x03"
        );
        // Rejected forms.
        for bad in [
            "http://x/px.bin",
            "https://x/px.bin",
            "/etc/passwd",
            "../px.bin",
            "sub/../../px.bin",
            "missing.bin",
        ] {
            assert!(resolve_bulk_file(&dir, bad).is_err(), "{bad:?}");
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn bulk_data_dir_end_to_end() {
        let dir = std::env::temp_dir().join(format!("xml2dcm-e2e-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("px.bin"), vec![0xABu8; 8]).unwrap();
        let xml = concat!(
            "<NativeDicomModel>",
            r#"<DicomAttribute tag="00080016" vr="UI"><Value number="1">1.2.840.10008.5.1.4.1.1.7</Value></DicomAttribute>"#,
            r#"<DicomAttribute tag="00080018" vr="UI"><Value number="1">1.9.9.11</Value></DicomAttribute>"#,
            r#"<DicomAttribute tag="7FE00010" vr="OW"><BulkData uri="px.bin"/></DicomAttribute>"#,
            "</NativeDicomModel>",
        );
        std::fs::write(dir.join("in.xml"), xml).unwrap();
        let args = Args {
            file: dir.join("in.xml"),
            output: dir.join("out.dcm"),
            transfer_syntax: "1.2.840.10008.1.2.1".into(),
            bulk_data_dir: Some(dir.clone()),
        };
        run(&args).unwrap();
        let (_, ds) = vyna_dcm::read_part10(&std::fs::read(dir.join("out.dcm")).unwrap()).unwrap();
        let vyna_dcm::Value::Bytes(px) = &ds.get(vyna_dcm::tags::PIXEL_DATA).unwrap().value else {
            panic!("bulk bytes lost");
        };
        assert_eq!(px, &vec![0xABu8; 8]);
        // Without the flag the same file still refuses.
        let args = Args {
            bulk_data_dir: None,
            ..args
        };
        assert!(run(&args).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
