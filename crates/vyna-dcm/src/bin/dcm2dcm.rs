//! `dcm2dcm`: transcode a Part-10 file to another transfer syntax.
//!
//! Exit status: 0 on success, 1 on failure. Lossy targets get a fresh
//! SOP Instance UID; lossless targets keep it.

use std::path::PathBuf;

use clap::Parser;
use vyna_dcm::{
    FileMeta, TranscodeOptions, TransferSyntax, read_part10, tags, transcode_dataset, write_part10,
};

/// DICOM pixel-data transcoder.
#[derive(Parser, Debug)]
#[command(
    name = "dcm2dcm",
    about = "Transcode pixel data to another transfer syntax"
)]
struct Args {
    /// Part-10 file to transcode.
    input: PathBuf,
    /// Transcoded Part-10 file to write.
    output: PathBuf,
    /// Target transfer syntax: a UID or one of `explicit`, `implicit`,
    /// `bigendian`, `deflated`, `rle`, `jpeg-baseline`, `jpeg-lossless`,
    /// `jpeg-ls`, `jpeg-ls-near`, `jpeg2000-lossless`, `jpeg2000`.
    #[arg(long)]
    transfer_syntax: String,
    /// JPEG Baseline quality 1–100 (only with `jpeg-baseline`).
    #[arg(long)]
    quality: Option<u8>,
    /// JPEG-LS NEAR bound (only with `jpeg-ls-near`).
    #[arg(long)]
    near: Option<u8>,
    /// JPEG 2000 lossy target ratio ≥ 1.0 (only with `jpeg2000`).
    #[arg(long)]
    lossy_ratio: Option<f32>,
}

fn main() {
    std::process::exit(run(&Args::parse()));
}

fn target_syntax(spec: &str) -> Option<TransferSyntax> {
    use TransferSyntax as Ts;
    match spec {
        "explicit" => Some(Ts::ExplicitVrLittleEndian),
        "implicit" => Some(Ts::ImplicitVrLittleEndian),
        "bigendian" => Some(Ts::ExplicitVrBigEndian),
        "deflated" => Some(Ts::DeflatedExplicitVrLittleEndian),
        "rle" => Some(Ts::RleLossless),
        "jpeg-baseline" => Some(Ts::JpegBaseline8Bit),
        "jpeg-lossless" => Some(Ts::JpegLossless),
        "jpeg-ls" => Some(Ts::JpegLsLossless),
        "jpeg-ls-near" => Some(Ts::JpegLsNearLossless),
        "jpeg2000-lossless" => Some(Ts::Jpeg2000Lossless),
        "jpeg2000" => Some(Ts::Jpeg2000),
        uid => TransferSyntax::from_uid(uid),
    }
}

fn run(args: &Args) -> i32 {
    let bytes = match std::fs::read(&args.input) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("dcm2dcm: {}: {e}", args.input.display());
            return 1;
        }
    };
    let (meta, ds) = match read_part10(&bytes) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("dcm2dcm: {}: {e}", args.input.display());
            return 1;
        }
    };
    let source_ts = match meta.transfer_syntax() {
        Ok(ts) => ts,
        Err(e) => {
            eprintln!("dcm2dcm: {}: {e}", args.input.display());
            return 1;
        }
    };
    let target_ts = match target_syntax(args.transfer_syntax.trim()) {
        Some(ts) => ts,
        None => {
            eprintln!("dcm2dcm: unknown transfer syntax {}", args.transfer_syntax);
            return 1;
        }
    };
    let mut opts = TranscodeOptions::default();
    if let Some(q) = args.quality {
        if target_ts != TransferSyntax::JpegBaseline8Bit {
            eprintln!("dcm2dcm: --quality applies to jpeg-baseline targets only");
            return 1;
        }
        if !(1..=100).contains(&q) {
            eprintln!("dcm2dcm: --quality {q} out of range 1–100");
            return 1;
        }
        opts.codec.jpeg.quality = q;
    }
    if let Some(near) = args.near {
        if target_ts != TransferSyntax::JpegLsNearLossless {
            eprintln!("dcm2dcm: --near applies to jpeg-ls-near targets only");
            return 1;
        }
        opts.codec.jpeg_ls.near = near;
    }
    if let Some(ratio) = args.lossy_ratio {
        if target_ts != TransferSyntax::Jpeg2000 {
            eprintln!("dcm2dcm: --lossy-ratio applies to jpeg2000 targets only");
            return 1;
        }
        if !ratio.is_finite() || ratio < 1.0 {
            eprintln!("dcm2dcm: --lossy-ratio must be finite and >= 1.0");
            return 1;
        }
        opts.codec.jpeg_2000.lossy_ratio = ratio;
    }
    let out_ds = match transcode_dataset(&ds, source_ts, target_ts, &opts) {
        Ok(ds) => ds,
        Err(e) => {
            eprintln!("dcm2dcm: {e}");
            return 1;
        }
    };
    let class = out_ds
        .string(tags::SOP_CLASS_UID)
        .unwrap_or_default()
        .to_string();
    let inst = out_ds
        .string(tags::SOP_INSTANCE_UID)
        .unwrap_or_default()
        .to_string();
    let uid_changed = ds.string(tags::SOP_INSTANCE_UID) != out_ds.string(tags::SOP_INSTANCE_UID);
    let file = match write_part10(&FileMeta::new(class, inst.clone(), target_ts), &out_ds) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("dcm2dcm: {e}");
            return 1;
        }
    };
    if let Err(e) = std::fs::write(&args.output, &file) {
        eprintln!("dcm2dcm: {}: {e}", args.output.display());
        return 1;
    }
    println!(
        "{}: {} -> {} ({} -> {} bytes{})",
        args.output.display(),
        source_ts.uid(),
        target_ts.uid(),
        bytes.len(),
        file.len(),
        if uid_changed {
            format!(", new SOP Instance UID {inst}")
        } else {
            String::new()
        }
    );
    0
}
