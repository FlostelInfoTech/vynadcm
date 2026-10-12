//! `qidors`: QIDO-RS search and print matches.

#[path = "webcommon.rs"]
mod webcommon;

use clap::Parser;
use vyna_dcm::{dataset_to_json, dump_dataset};
use webcommon::WebArgs;

/// QIDO-RS search tool.
#[derive(Parser, Debug)]
#[command(name = "qidors", about = "QIDO-RS search and print matches")]
struct Args {
    #[command(flatten)]
    web: WebArgs,
    /// Series scope: search series of this Study Instance UID.
    #[arg(long, conflicts_with = "instances")]
    series: Option<String>,
    /// Instance scope: STUDY SERIES (two UIDs).
    #[arg(long, num_args = 2, value_names = ["STUDY", "SERIES"])]
    instances: Option<Vec<String>>,
    /// Query keys as KEY=value (keyword or 8-hex tag). Repeatable;
    /// empty value means return key.
    #[arg(short, long = "key")]
    keys: Vec<String>,
    /// Page size (`limit`).
    #[arg(long)]
    limit: Option<usize>,
    /// Page start (`offset`).
    #[arg(long)]
    offset: Option<usize>,
    /// Print matches as DICOM JSON instead of text dump.
    #[arg(long, default_value_t = false)]
    json: bool,
}

#[tokio::main]
async fn main() {
    let args = Args::parse();
    match run(&args).await {
        Ok(()) => {}
        Err(e) => {
            eprintln!("qidors: {e}");
            std::process::exit(1);
        }
    }
}

async fn run(args: &Args) -> Result<(), String> {
    let client = args.web.client()?;
    let mut owned = webcommon::split_keys(&args.keys)?;
    if let Some(n) = args.limit {
        owned.push(("limit".to_string(), n.to_string()));
    }
    if let Some(n) = args.offset {
        owned.push(("offset".to_string(), n.to_string()));
    }
    let query: Vec<(&str, &str)> = owned
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    let matches = if let Some(pair) = &args.instances {
        client
            .search_instances(&pair[0], &pair[1], &query)
            .await
            .map_err(|e| e.to_string())?
    } else if let Some(study) = &args.series {
        client
            .search_series(study, &query)
            .await
            .map_err(|e| e.to_string())?
    } else {
        client
            .search_studies(&query)
            .await
            .map_err(|e| e.to_string())?
    };
    for (i, ds) in matches.iter().enumerate() {
        println!("--- match {} ---", i + 1);
        if args.json {
            println!(
                "{}",
                serde_json::to_string_pretty(&dataset_to_json(ds)).unwrap()
            );
        } else {
            print!("{}", dump_dataset(ds));
        }
    }
    println!("QIDO complete: {} match(es)", matches.len());
    Ok(())
}
