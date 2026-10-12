//! `deidentify`: de-identify Part-10 files with the basic profile.

use std::path::{Path, PathBuf};

use clap::Parser;
use vyna_dcm::{Deidentifier, read_part10, write_part10};

/// De-identification CLI (basic profile, PS3.15 E semantics).
#[derive(Parser, Debug)]
#[command(
    name = "deidentify",
    about = "De-identify Part-10 files for research exports"
)]
struct Args {
    /// Input Part-10 files (UID maps stay consistent across all of them).
    #[arg(required = true)]
    inputs: Vec<PathBuf>,
    /// Output directory (required for multiple inputs).
    #[arg(long)]
    output_dir: Option<PathBuf>,
    /// Output file (single input only).
    #[arg(short, long)]
    output: Option<PathBuf>,
    /// Rewrite the inputs in place (via temp files).
    #[arg(long, default_value_t = false)]
    in_place: bool,
    /// Shift dates by DAYS instead of zeroing them (negative shifts back).
    #[arg(long)]
    date_offset: Option<i64>,
    /// Replacement patient ID (default: ANONYMIZED).
    #[arg(long)]
    patient_id: Option<String>,
    /// Replacement patient name (default: ANONYMIZED).
    #[arg(long)]
    patient_name: Option<String>,
    /// Keep private data elements (default: removed).
    #[arg(long, default_value_t = false)]
    retain_private_tags: bool,
}

/// Resolve `(input, destination)` pairs from the output flags.
fn plan_outputs(
    inputs: &[PathBuf],
    output: Option<&PathBuf>,
    output_dir: Option<&PathBuf>,
    in_place: bool,
) -> Result<Vec<(PathBuf, PathBuf)>, String> {
    if inputs.is_empty() {
        return Err("need at least one input file".to_string());
    }
    if output.is_some() && output_dir.is_some() {
        return Err("--output and --output-dir conflict".to_string());
    }
    if in_place && (output.is_some() || output_dir.is_some()) {
        return Err("--in-place conflicts with --output/--output-dir".to_string());
    }
    if !in_place && output.is_none() && output_dir.is_none() {
        return Err("need --output, --output-dir, or --in-place".to_string());
    }
    if output.is_some() && inputs.len() > 1 {
        return Err("--output takes a single input; use --output-dir".to_string());
    }
    inputs
        .iter()
        .map(|input| {
            let dest = if in_place {
                input.clone()
            } else if let Some(o) = output {
                o.clone()
            } else if let Some(name) = input.file_name() {
                output_dir.unwrap().join(name)
            } else {
                return Err(format!(
                    "{}: no file name for --output-dir",
                    input.display()
                ));
            };
            Ok((input.clone(), dest))
        })
        .collect()
}

fn write_output(dest: &Path, input: &Path, bytes: &[u8]) -> Result<(), String> {
    if dest != input {
        return std::fs::write(dest, bytes).map_err(|e| format!("{}: {e}", dest.display()));
    }
    let tmp = input.with_extension("vyna-tmp");
    if let Err(e) = std::fs::write(&tmp, bytes) {
        return Err(format!("{}: {e}", tmp.display()));
    }
    if let Err(e) = std::fs::rename(&tmp, input) {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("{}: {e}", input.display()));
    }
    Ok(())
}

fn process_file(deid: &mut Deidentifier, input: &Path, dest: &Path) -> Result<String, String> {
    let bytes = std::fs::read(input).map_err(|e| format!("{}: {e}", input.display()))?;
    let (mut meta, mut ds) =
        read_part10(&bytes).map_err(|e| format!("{}: {e}", input.display()))?;
    let report = deid
        .apply(&mut ds)
        .map_err(|e| format!("{}: {e}", input.display()))?;
    for w in &report.warnings {
        eprintln!("deidentify: warning: {w}");
    }
    // File-meta SOP UIDs follow the remapped dataset.
    let class = ds
        .string(vyna_dcm::tags::SOP_CLASS_UID)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("{}: missing SOP Class UID", input.display()))?;
    let inst = ds
        .string(vyna_dcm::tags::SOP_INSTANCE_UID)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("{}: missing SOP Instance UID", input.display()))?;
    meta.media_storage_sop_class_uid = class.to_string();
    meta.media_storage_sop_instance_uid = inst.to_string();
    let out = write_part10(&meta, &ds).map_err(|e| format!("{}: {e}", input.display()))?;
    write_output(dest, input, &out)?;
    Ok(format!(
        "{} -> {}: {} removed, {} zeroed, {} dummied, {} UIDs remapped, {} dates shifted",
        input.display(),
        dest.display(),
        report.removed,
        report.zeroed,
        report.dummied,
        report.uids_remapped,
        report.dates_shifted,
    ))
}

fn run() -> i32 {
    let args = Args::parse();
    let jobs = match plan_outputs(
        &args.inputs,
        args.output.as_ref(),
        args.output_dir.as_ref(),
        args.in_place,
    ) {
        Ok(j) => j,
        Err(e) => {
            eprintln!("deidentify: {e}");
            return 1;
        }
    };
    if let Some(dir) = &args.output_dir
        && let Err(e) = std::fs::create_dir_all(dir)
    {
        eprintln!("deidentify: {}: {e}", dir.display());
        return 1;
    }
    let mut deid = Deidentifier::new().with_retain_private(args.retain_private_tags);
    if let Some(days) = args.date_offset {
        deid = deid.with_date_offset(days);
    }
    if let Some(id) = &args.patient_id {
        deid = deid.with_replacement_id(id.clone());
    }
    if let Some(name) = &args.patient_name {
        deid = deid.with_replacement_name(name);
    }
    for (input, dest) in &jobs {
        match process_file(&mut deid, input, dest) {
            Ok(summary) => println!("{summary}"),
            Err(e) => {
                eprintln!("deidentify: {e}");
                return 1;
            }
        }
    }
    0
}

fn main() {
    std::process::exit(run());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan(
        inputs: &[&str],
        output: Option<&str>,
        dir: Option<&str>,
        in_place: bool,
    ) -> Result<Vec<(String, String)>, String> {
        let ins: Vec<PathBuf> = inputs.iter().map(PathBuf::from).collect();
        let o = output.map(PathBuf::from);
        let d = dir.map(PathBuf::from);
        plan_outputs(&ins, o.as_ref(), d.as_ref(), in_place).map(|v| {
            v.into_iter()
                .map(|(a, b)| {
                    (
                        a.to_string_lossy().into_owned(),
                        b.to_string_lossy().into_owned(),
                    )
                })
                .collect()
        })
    }

    #[test]
    fn output_plans_resolve() {
        assert_eq!(
            plan(&["a.dcm"], Some("b.dcm"), None, false).unwrap(),
            vec![("a.dcm".to_string(), "b.dcm".to_string())]
        );
        assert_eq!(
            plan(&["a.dcm", "b.dcm"], None, Some("out"), false).unwrap(),
            vec![
                ("a.dcm".to_string(), "out/a.dcm".to_string()),
                ("b.dcm".to_string(), "out/b.dcm".to_string()),
            ]
        );
        assert_eq!(
            plan(&["a.dcm"], None, None, true).unwrap(),
            vec![("a.dcm".to_string(), "a.dcm".to_string())]
        );
    }

    #[test]
    fn output_plans_reject_conflicts() {
        assert!(plan(&[], Some("b.dcm"), None, false).is_err());
        assert!(plan(&["a.dcm"], Some("b.dcm"), Some("o"), false).is_err());
        assert!(plan(&["a.dcm", "b.dcm"], Some("c.dcm"), None, false).is_err());
        assert!(plan(&["a.dcm"], Some("b.dcm"), None, true).is_err());
        assert!(plan(&["a.dcm"], None, None, false).is_err());
        assert!(plan(&["/"], None, Some("o"), false).is_err());
    }

    #[test]
    fn process_file_round_trip() {
        use vyna_dcm::{DataSet, FileMeta, TransferSyntax, Vr, tags};
        let dir = std::env::temp_dir().join(format!("deid-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let input = dir.join("in.dcm");
        let output = dir.join("out.dcm");
        let mut ds = DataSet::new();
        ds.put_uid(tags::SOP_CLASS_UID, "1.2.840.10008.5.1.4.1.1.7");
        ds.put_uid(tags::SOP_INSTANCE_UID, "1.9.9.9");
        ds.put_text(tags::PATIENT_ID, Vr::Lo, "E123");
        let meta = FileMeta::new(
            "1.2.840.10008.5.1.4.1.1.7",
            "1.9.9.9",
            TransferSyntax::ExplicitVrLittleEndian,
        );
        std::fs::write(&input, vyna_dcm::write_part10(&meta, &ds).unwrap()).unwrap();
        let summary = process_file(&mut Deidentifier::new(), &input, &output).unwrap();
        assert!(summary.contains("1 dummied"), "{summary}");
        let (meta2, ds2) = read_part10(&std::fs::read(&output).unwrap()).unwrap();
        assert_eq!(ds2.string(tags::PATIENT_ID), Some("ANONYMIZED"));
        assert_eq!(
            meta2.media_storage_sop_instance_uid,
            ds2.string(tags::SOP_INSTANCE_UID).unwrap()
        );
        assert_ne!(meta2.media_storage_sop_instance_uid, "1.9.9.9");
        std::fs::remove_file(&input).unwrap();
        std::fs::remove_file(&output).unwrap();
    }
}
