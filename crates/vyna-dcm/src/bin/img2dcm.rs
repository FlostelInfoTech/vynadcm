//! `img2dcm`: replace the Pixel Data of a Part-10 file from a PNG.
//!
//! Geometry tags are rewritten from the PNG (8/16-bit gray, 8-bit
//! RGB). Without `--transfer-syntax` the output keeps a native
//! template syntax; with one, injected pixels are encoded to the
//! target (RLE/JPEG/JPEG-LS/JPEG 2000 included). UIDs are kept except
//! on lossy targets, which get a fresh SOP Instance UID (reported).

use std::path::PathBuf;

use clap::Parser;
use vyna_dcm::{
    FileMeta, TranscodeOptions, TransferSyntax, inject_png, read_part10, tags, transcode_dataset,
    write_part10,
};

/// PNG-to-DICOM tool.
#[derive(Parser, Debug)]
#[command(name = "img2dcm", about = "Inject PNG pixels into a Part-10 file")]
struct Args {
    /// PNG image to inject.
    image: PathBuf,
    /// Template Part-10 file.
    template: PathBuf,
    /// Output Part-10 file.
    #[arg(short, long)]
    output: PathBuf,
    /// Output transfer syntax: a UID or one of `explicit`, `implicit`,
    /// `bigendian`, `deflated`, `rle`, `jpeg-baseline`, `jpeg-lossless`,
    /// `jpeg-ls`, `jpeg-ls-near`, `jpeg2000-lossless`, `jpeg2000`
    /// (default: keep the template's native syntax).
    #[arg(long)]
    transfer_syntax: Option<String>,
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

/// Codec options from the quality knobs; each knob requires its
/// matching `--transfer-syntax` target.
fn codec_options(args: &Args) -> Result<TranscodeOptions, String> {
    use TransferSyntax as Ts;
    let target = args.transfer_syntax.as_deref().and_then(target_syntax);
    let mut opts = TranscodeOptions::default();
    if let Some(q) = args.quality {
        if target != Some(Ts::JpegBaseline8Bit) {
            return Err("--quality applies to jpeg-baseline targets only".to_string());
        }
        if !(1..=100).contains(&q) {
            return Err(format!("--quality {q} out of range 1–100"));
        }
        opts.codec.jpeg.quality = q;
    }
    if let Some(near) = args.near {
        if target != Some(Ts::JpegLsNearLossless) {
            return Err("--near applies to jpeg-ls-near targets only".to_string());
        }
        opts.codec.jpeg_ls.near = near;
    }
    if let Some(ratio) = args.lossy_ratio {
        if target != Some(Ts::Jpeg2000) {
            return Err("--lossy-ratio applies to jpeg2000 targets only".to_string());
        }
        if !ratio.is_finite() || ratio < 1.0 {
            return Err("--lossy-ratio must be finite and >= 1.0".to_string());
        }
        opts.codec.jpeg_2000.lossy_ratio = ratio;
    }
    Ok(opts)
}

fn main() {
    let args = Args::parse();
    match run(&args) {
        Ok(()) => {}
        Err(e) => {
            eprintln!("img2dcm: {e}");
            std::process::exit(1);
        }
    }
}

fn run(args: &Args) -> Result<(), String> {
    let png = std::fs::read(&args.image).map_err(|e| e.to_string())?;
    let bytes = std::fs::read(&args.template).map_err(|e| e.to_string())?;
    let (meta, mut ds) = read_part10(&bytes).map_err(|e| e.to_string())?;
    let template_ts = meta.transfer_syntax().map_err(|e| e.to_string())?;
    // Injected pixels are native little-endian; the output syntax
    // defaults to a native template and any request is honored via
    // transcode (this also fixes big-endian templates, whose bytes
    // used to be written unswapped).
    let out_ts = match &args.transfer_syntax {
        Some(spec) => {
            target_syntax(spec.trim()).ok_or_else(|| format!("unknown transfer syntax {spec}"))?
        }
        None if template_ts.is_uncompressed() => template_ts,
        None => {
            return Err(format!(
                "template uses {template_ts:?}; pass --transfer-syntax to encode"
            ));
        }
    };
    let opts = codec_options(args)?;
    inject_png(&mut ds, &png).map_err(|e| e.to_string())?;
    let out_ds = transcode_dataset(&ds, TransferSyntax::ExplicitVrLittleEndian, out_ts, &opts)
        .map_err(|e| e.to_string())?;
    let class = out_ds
        .string(tags::SOP_CLASS_UID)
        .ok_or("dataset without SOP Class UID")?
        .to_string();
    let inst = out_ds
        .string(tags::SOP_INSTANCE_UID)
        .ok_or("dataset without SOP Instance UID")?
        .to_string();
    let out = write_part10(&FileMeta::new(class, inst.clone(), out_ts), &out_ds)
        .map_err(|e| e.to_string())?;
    std::fs::write(&args.output, out).map_err(|e| e.to_string())?;
    if ds.string(tags::SOP_INSTANCE_UID) != out_ds.string(tags::SOP_INSTANCE_UID) {
        println!("img2dcm: lossy target; new SOP Instance UID {inst}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transfer_syntax_names_parse() {
        use vyna_dcm::TransferSyntax as Ts;
        assert_eq!(target_syntax("explicit"), Some(Ts::ExplicitVrLittleEndian));
        assert_eq!(target_syntax("rle"), Some(Ts::RleLossless));
        assert_eq!(target_syntax("jpeg-baseline"), Some(Ts::JpegBaseline8Bit));
        assert_eq!(target_syntax("jpeg-lossless"), Some(Ts::JpegLossless));
        assert_eq!(target_syntax("jpeg-ls"), Some(Ts::JpegLsLossless));
        assert_eq!(target_syntax("jpeg-ls-near"), Some(Ts::JpegLsNearLossless));
        assert_eq!(
            target_syntax("jpeg2000-lossless"),
            Some(Ts::Jpeg2000Lossless)
        );
        assert_eq!(target_syntax("jpeg2000"), Some(Ts::Jpeg2000));
        assert_eq!(
            target_syntax("1.2.840.10008.1.2.4.70"),
            Some(Ts::JpegLossless)
        );
        assert_eq!(target_syntax("nope"), None);
    }

    #[test]
    fn quality_knobs_validate() {
        let args = Args::try_parse_from([
            "img2dcm",
            "a.png",
            "t.dcm",
            "-o",
            "o.dcm",
            "--transfer-syntax",
            "jpeg-baseline",
            "--quality",
            "75",
        ])
        .unwrap();
        assert_eq!(codec_options(&args).unwrap().codec.jpeg.quality, 75);
        let args = Args::try_parse_from([
            "img2dcm",
            "a.png",
            "t.dcm",
            "-o",
            "o.dcm",
            "--quality",
            "75",
        ])
        .unwrap();
        assert!(codec_options(&args).is_err()); // no syntax, knob rejected
        let args = Args::try_parse_from([
            "img2dcm",
            "a.png",
            "t.dcm",
            "-o",
            "o.dcm",
            "--transfer-syntax",
            "jpeg-ls",
            "--quality",
            "75",
        ])
        .unwrap();
        assert!(codec_options(&args).is_err()); // wrong target
    }
}
