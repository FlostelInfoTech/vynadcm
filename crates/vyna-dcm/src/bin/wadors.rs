//! `wadors`: WADO-RS retrieve (instance, metadata, frames, bulk data).

#[path = "webcommon.rs"]
mod webcommon;

use std::path::PathBuf;

use clap::Parser;
use vyna_dcm::dataset_to_json;
use webcommon::WebArgs;

/// WADO-RS retrieve tool.
#[derive(Parser, Debug)]
#[command(name = "wadors", about = "WADO-RS retrieve to files")]
struct Args {
    #[command(flatten)]
    web: WebArgs,
    /// Study Instance UID (ignored with --bulkdata).
    study: Option<String>,
    /// Series Instance UID (needs STUDY for retrieve/metadata/frames).
    #[arg(long)]
    series: Option<String>,
    /// SOP Instance UID.
    #[arg(long)]
    instance: Option<String>,
    /// Fetch metadata (`.../metadata`) as JSON to stdout.
    #[arg(long, default_value_t = false)]
    metadata: bool,
    /// Fetch frames (comma list, e.g. `1,2,3`).
    #[arg(long)]
    frames: Option<String>,
    /// Accept type for frames (e.g. `image/jpeg`).
    #[arg(long, default_value = "application/octet-stream")]
    accept: String,
    /// Fetch one BulkData URI to the output file.
    #[arg(long)]
    bulkdata: Option<String>,
    /// Output file (`-` is unsupported; metadata goes to stdout).
    #[arg(short, long)]
    output: Option<PathBuf>,
}

fn main() {
    let args = Args::parse();
    match run(&args) {
        Ok(()) => {}
        Err(e) => {
            eprintln!("wadors: {e}");
            std::process::exit(1);
        }
    }
}

/// Parse a comma-separated 1-based frame list.
fn parse_frames(s: &str) -> Result<Vec<u32>, String> {
    s.split(',')
        .map(|n| {
            n.trim()
                .parse::<u32>()
                .map_err(|_| format!("bad frame number {n:?}"))
                .and_then(|n| {
                    if n == 0 {
                        Err("frame numbers are 1-based".to_string())
                    } else {
                        Ok(n)
                    }
                })
        })
        .collect()
}

fn run(args: &Args) -> Result<(), String> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    rt.block_on(run_async(args))
}

async fn run_async(args: &Args) -> Result<(), String> {
    let client = args.web.client()?;
    if let Some(uri) = &args.bulkdata {
        let output = args
            .output
            .as_ref()
            .ok_or("--output is required with --bulkdata")?;
        let (content_type, body) = client.wado_bulkdata(uri).await.map_err(|e| e.to_string())?;
        std::fs::write(output, body).map_err(|e| e.to_string())?;
        println!("{} ({} bytes)", output.display(), content_type);
        return Ok(());
    }
    let (Some(study), Some(series), Some(instance)) = (
        args.study.as_ref(),
        args.series.as_ref(),
        args.instance.as_ref(),
    ) else {
        return Err("need STUDY with --series and --instance (or --bulkdata URI)".to_string());
    };
    if args.metadata {
        let meta = client
            .wado_metadata(study, series, instance)
            .await
            .map_err(|e| e.to_string())?;
        println!(
            "{}",
            serde_json::to_string_pretty(&dataset_to_json(&meta)).unwrap()
        );
        return Ok(());
    }
    if let Some(frames) = &args.frames {
        let numbers = parse_frames(frames)?;
        let output = args
            .output
            .as_ref()
            .ok_or("--output is required with --frames")?;
        let parts = client
            .wado_frames(study, series, instance, &numbers, &args.accept)
            .await
            .map_err(|e| e.to_string())?;
        write_parts(
            output,
            &parts.iter().map(|p| p.body.clone()).collect::<Vec<_>>(),
        )?;
        return Ok(());
    }
    let output = args.output.as_ref().ok_or("--output is required")?;
    let bytes = client
        .wado_file(study, series, instance)
        .await
        .map_err(|e| e.to_string())?;
    std::fs::write(output, &bytes).map_err(|e| e.to_string())?;
    println!("{} ({} bytes)", output.display(), bytes.len());
    Ok(())
}

/// One part lands exactly at `output`; several fan out to
/// `STEM_N.EXT`.
fn write_parts(output: &PathBuf, parts: &[Vec<u8>]) -> Result<(), String> {
    if parts.len() == 1 {
        std::fs::write(output, &parts[0]).map_err(|e| e.to_string())?;
        println!("{} ({} bytes)", output.display(), parts[0].len());
        return Ok(());
    }
    let stem = output
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    for (i, part) in parts.iter().enumerate() {
        let mut named = output.clone();
        let ext = output
            .extension()
            .map(|e| format!(".{}", e.to_string_lossy()))
            .unwrap_or_default();
        named.set_file_name(format!("{stem}_{}{ext}", i + 1));
        std::fs::write(&named, part).map_err(|e| e.to_string())?;
        println!("{} ({} bytes)", named.display(), part.len());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_parse() {
        assert_eq!(parse_frames("1,2,3").unwrap(), vec![1, 2, 3]);
        assert_eq!(parse_frames(" 7 ").unwrap(), vec![7]);
        assert!(parse_frames("0").is_err());
        assert!(parse_frames("1,x").is_err());
        assert!(parse_frames("").is_err());
    }
}
