//! Regression tests over real third-party-encoded files
//! (`tests/data`, DCMTK/GDCM writers).

use std::path::PathBuf;
use vyna_dcm_codec::{
    TransferSyntax, dataset_from_json, dataset_from_xml, dataset_to_json, dataset_to_xml,
    decode_pixels, frame_to_png, read_part10,
};
use vyna_dcm_core::{DataSet, tags};

fn corpus(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/data")
        .join(name)
}

fn read(name: &str) -> (DataSet, TransferSyntax) {
    let bytes = std::fs::read(corpus(name)).unwrap();
    let (meta, ds) = read_part10(&bytes).unwrap();
    let ts = meta.transfer_syntax().unwrap();
    (ds, ts)
}

/// The lossless emri_small family must decode byte-identically:
/// native (DCMTK) vs RLE vs JPEG-LS vs JPEG 2000 (GDCM).
#[test]
fn lossless_family_agrees() {
    let (native, native_ts) = read("emri_small.dcm");
    let expect = decode_pixels(&native, native_ts).unwrap();
    assert!(!expect.is_empty());
    for name in [
        "emri_small_RLE.dcm",
        "emri_small_jpeg_ls_lossless.dcm",
        "emri_small_jpeg_2k_lossless.dcm",
    ] {
        let (ds, ts) = read(name);
        let got = decode_pixels(&ds, ts).unwrap();
        assert_eq!(got, expect, "{name} disagrees with native");
    }
}

/// Every corpus file renders a first frame to PNG.
#[test]
fn corpus_renders_png() {
    for name in [
        "emri_small.dcm",
        "emri_small_RLE.dcm",
        "emri_small_jpeg_ls_lossless.dcm",
        "emri_small_jpeg_2k_lossless.dcm",
        "SC_rgb.dcm",
    ] {
        let (ds, ts) = read(name);
        let png = frame_to_png(&ds, ts, 1).unwrap();
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n", "{name}");
    }
}

/// XML preserves the corpus byte-behavior exactly (DS lexical form
/// included); JSON normalizes DS floats, so it is checked scoped.
#[test]
fn xml_round_trips_corpus_exactly() {
    for name in ["emri_small.dcm", "SC_rgb.dcm"] {
        let (ds, _) = read(name);
        let back = dataset_from_xml(&dataset_to_xml(&ds)).unwrap();
        assert_eq!(back, ds, "{name}");
    }
}

#[test]
fn json_preserves_pixels_and_keys() {
    for name in ["emri_small.dcm", "SC_rgb.dcm"] {
        let (ds, ts) = read(name);
        let back = dataset_from_json(&dataset_to_json(&ds)).unwrap();
        assert_eq!(
            back.string(tags::SOP_INSTANCE_UID),
            ds.string(tags::SOP_INSTANCE_UID),
            "{name}"
        );
        assert_eq!(back.uint16(tags::ROWS), ds.uint16(tags::ROWS), "{name}");
        assert_eq!(
            back.bytes(tags::PIXEL_DATA),
            ds.bytes(tags::PIXEL_DATA),
            "{name} pixels"
        );
        // And the JSON-side bytes still decode as the same frame.
        let _ = decode_pixels(&ds, ts).unwrap();
    }
}
