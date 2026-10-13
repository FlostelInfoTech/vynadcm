//! CLI coverage for the `vyna-dcm` binaries.
//!
//! - `all_bins_show_help`: every binary built for the active feature
//!   set must accept `--help` (exercises all clap definitions).
//! - JSON/XML converter round-trips through real files, plus a
//!   `dcmdump` functional check.

#![cfg(feature = "cli")]

use std::path::{Path, PathBuf};
use std::process::Command;

use vyna_dcm::{DataSet, FileMeta, TransferSyntax, Vr, read_part10, tags, write_part10};

/// `(binary, needs_net, needs_web)`; every binary also needs `cli`.
const ALL_BINS: &[(&str, bool, bool)] = &[
    ("echoscu", true, false),
    ("echoscp", true, false),
    ("storescu", true, false),
    ("storescp", true, false),
    ("findscu", true, false),
    ("findscp", true, false),
    ("mwlscp", true, false),
    ("mppsscp", true, false),
    ("movescu", true, false),
    ("getscu", true, false),
    ("mppsscu", true, false),
    ("stgcmtscu", true, false),
    ("stgcmtscp", true, false),
    ("qidors", false, true),
    ("wadors", false, true),
    ("stowrs", false, true),
    ("wadorsd", false, true),
    ("dcmdump", false, false),
    ("dcm2json", false, false),
    ("json2dcm", false, false),
    ("dcm2xml", false, false),
    ("xml2dcm", false, false),
    ("dcm2img", false, false),
    ("img2dcm", false, false),
    ("dcmvalidate", false, false),
    ("pdf2dcm", false, false),
    ("dcm2pdf", false, false),
    ("dcmodify", false, false),
    ("dcm2dcm", false, false),
    ("dcmdir", false, false),
    ("deidentify", false, false),
    ("auditsend", false, false),
];

/// Path of a binary that must exist for the active feature set.
fn require_bin(name: &str) -> &'static str {
    let path = match name {
        "echoscu" => option_env!("CARGO_BIN_EXE_echoscu"),
        "echoscp" => option_env!("CARGO_BIN_EXE_echoscp"),
        "storescu" => option_env!("CARGO_BIN_EXE_storescu"),
        "storescp" => option_env!("CARGO_BIN_EXE_storescp"),
        "findscu" => option_env!("CARGO_BIN_EXE_findscu"),
        "findscp" => option_env!("CARGO_BIN_EXE_findscp"),
        "mwlscp" => option_env!("CARGO_BIN_EXE_mwlscp"),
        "mppsscp" => option_env!("CARGO_BIN_EXE_mppsscp"),
        "movescu" => option_env!("CARGO_BIN_EXE_movescu"),
        "getscu" => option_env!("CARGO_BIN_EXE_getscu"),
        "mppsscu" => option_env!("CARGO_BIN_EXE_mppsscu"),
        "stgcmtscu" => option_env!("CARGO_BIN_EXE_stgcmtscu"),
        "stgcmtscp" => option_env!("CARGO_BIN_EXE_stgcmtscp"),
        "qidors" => option_env!("CARGO_BIN_EXE_qidors"),
        "wadors" => option_env!("CARGO_BIN_EXE_wadors"),
        "stowrs" => option_env!("CARGO_BIN_EXE_stowrs"),
        "wadorsd" => option_env!("CARGO_BIN_EXE_wadorsd"),
        "dcmdump" => option_env!("CARGO_BIN_EXE_dcmdump"),
        "dcm2json" => option_env!("CARGO_BIN_EXE_dcm2json"),
        "json2dcm" => option_env!("CARGO_BIN_EXE_json2dcm"),
        "dcm2xml" => option_env!("CARGO_BIN_EXE_dcm2xml"),
        "xml2dcm" => option_env!("CARGO_BIN_EXE_xml2dcm"),
        "dcm2img" => option_env!("CARGO_BIN_EXE_dcm2img"),
        "img2dcm" => option_env!("CARGO_BIN_EXE_img2dcm"),
        "dcmvalidate" => option_env!("CARGO_BIN_EXE_dcmvalidate"),
        "pdf2dcm" => option_env!("CARGO_BIN_EXE_pdf2dcm"),
        "dcm2pdf" => option_env!("CARGO_BIN_EXE_dcm2pdf"),
        "dcmodify" => option_env!("CARGO_BIN_EXE_dcmodify"),
        "dcm2dcm" => option_env!("CARGO_BIN_EXE_dcm2dcm"),
        "dcmdir" => option_env!("CARGO_BIN_EXE_dcmdir"),
        "deidentify" => option_env!("CARGO_BIN_EXE_deidentify"),
        "auditsend" => option_env!("CARGO_BIN_EXE_auditsend"),
        other => panic!("unknown binary {other}"),
    };
    path.unwrap_or_else(|| panic!("binary {name} was not built for these features"))
}

#[test]
fn all_bins_show_help() {
    assert_eq!(ALL_BINS.len(), 32, "keep the inventory in sync");
    let (has_net, has_web) = (cfg!(feature = "net"), cfg!(feature = "web"));
    let mut covered = 0;
    for (name, need_net, need_web) in ALL_BINS {
        if (*need_net && !has_net) || (*need_web && !has_web) {
            continue;
        }
        covered += 1;
        let out = Command::new(require_bin(name))
            .arg("--help")
            .output()
            .unwrap_or_else(|e| panic!("{name} --help failed to spawn: {e}"));
        assert!(out.status.success(), "{name} --help: {out:?}");
        assert!(!out.stdout.is_empty(), "{name} --help printed nothing");
    }
    assert!(covered > 0, "no binaries covered");
}

fn scratch_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("vynadcm-cli-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Minimal single-frame CT Part-10 fixture.
fn fixture_part10(path: &Path) {
    let mut ds = DataSet::new();
    ds.put_uid(tags::SOP_CLASS_UID, vyna_dcm::core::sop_class::CT_IMAGE);
    ds.put_uid(tags::SOP_INSTANCE_UID, "1.2.3.4.5");
    ds.put_uid(tags::STUDY_INSTANCE_UID, "1.2.3.4");
    ds.put_uid(tags::SERIES_INSTANCE_UID, "1.2.3.4.1");
    ds.put_text(tags::PATIENT_NAME, Vr::Pn, "Smoke^Test");
    ds.put_text(tags::PATIENT_ID, Vr::Lo, "CLI123");
    ds.put_text(tags::MODALITY, Vr::Cs, "CT");
    ds.put_u16_single(tags::ROWS, 2);
    ds.put_u16_single(tags::COLUMNS, 2);
    ds.put_u16_single(tags::SAMPLES_PER_PIXEL, 1);
    ds.put_u16_single(tags::BITS_ALLOCATED, 16);
    ds.put_u16_single(tags::BITS_STORED, 12);
    ds.put_u16_single(tags::HIGH_BIT, 11);
    ds.put_u16_single(tags::PIXEL_REPRESENTATION, 0);
    ds.put_text(tags::PHOTOMETRIC_INTERPRETATION, Vr::Cs, "MONOCHROME2");
    ds.put_bytes(tags::PIXEL_DATA, Vr::Ow, vec![1, 2, 3, 4, 5, 6, 7, 8]);
    let meta = FileMeta::new(
        vyna_dcm::core::sop_class::CT_IMAGE,
        "1.2.3.4.5",
        TransferSyntax::ExplicitVrLittleEndian,
    );
    std::fs::write(path, write_part10(&meta, &ds).unwrap()).unwrap();
}

fn run_ok(bin: &str, args: &[&str]) -> std::process::Output {
    let out = Command::new(require_bin(bin))
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("{bin} failed to spawn: {e}"));
    assert!(
        out.status.success(),
        "{bin} {args:?} exited {}: {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    out
}

#[test]
fn json_converters_round_trip_through_files() {
    let dir = scratch_dir("json");
    let dcm = dir.join("in.dcm");
    fixture_part10(&dcm);
    let out = run_ok("dcm2json", &[dcm.to_str().unwrap()]);
    let json_path = dir.join("mid.json");
    std::fs::write(&json_path, &out.stdout).unwrap();
    let back = dir.join("out.dcm");
    run_ok(
        "json2dcm",
        &[json_path.to_str().unwrap(), "-o", back.to_str().unwrap()],
    );
    let (_, ds) = read_part10(&std::fs::read(&back).unwrap()).unwrap();
    assert_eq!(ds.string(tags::PATIENT_ID), Some("CLI123"));
    assert_eq!(ds.string(tags::SOP_INSTANCE_UID), Some("1.2.3.4.5"));
    assert_eq!(
        ds.bytes(tags::PIXEL_DATA).unwrap(),
        &[1, 2, 3, 4, 5, 6, 7, 8]
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn xml_converters_round_trip_through_files() {
    let dir = scratch_dir("xml");
    let dcm = dir.join("in.dcm");
    fixture_part10(&dcm);
    let out = run_ok("dcm2xml", &[dcm.to_str().unwrap()]);
    assert!(String::from_utf8_lossy(&out.stdout).contains("NativeDicomModel"));
    let xml_path = dir.join("mid.xml");
    std::fs::write(&xml_path, &out.stdout).unwrap();
    let back = dir.join("out.dcm");
    run_ok(
        "xml2dcm",
        &[xml_path.to_str().unwrap(), "-o", back.to_str().unwrap()],
    );
    let (_, ds) = read_part10(&std::fs::read(&back).unwrap()).unwrap();
    assert_eq!(ds.string(tags::PATIENT_ID), Some("CLI123"));
    assert_eq!(
        ds.bytes(tags::PIXEL_DATA).unwrap(),
        &[1, 2, 3, 4, 5, 6, 7, 8]
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn dcmdump_prints_tags() {
    let dir = scratch_dir("dump");
    let dcm = dir.join("in.dcm");
    fixture_part10(&dcm);
    let out = run_ok("dcmdump", &[dcm.to_str().unwrap()]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("(0010,0010)"), "{text}");
    assert!(text.contains("PatientName"), "{text}");
    std::fs::remove_dir_all(&dir).unwrap();
}
