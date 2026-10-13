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
    /// Dummy text for ID-like `Dummy`-action tags (default:
    /// ANONYMIZED). Patient ID itself is emptied per E.1 `Z`.
    #[arg(long)]
    patient_id: Option<String>,
    /// Dummy text for name-like `Dummy`-action tags (default:
    /// ANONYMIZED). Patient's Name itself is removed per E.1 `X`.
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

/// Every output goes through a temporary file + rename so a failed
/// run never leaves a truncated destination (in-place or not).
///
/// The temp name carries the pid plus an attempt counter and is
/// created with create-new semantics: concurrent runs on the same
/// file must not share one fixed temp name (and a planted symlink
/// must not redirect the write elsewhere).
fn write_output(dest: &Path, bytes: &[u8]) -> Result<(), String> {
    for attempt in 0..100u32 {
        let tmp = dest.with_extension(format!("vyna-tmp-{}-{attempt}", std::process::id()));
        let mut opts = std::fs::OpenOptions::new();
        match opts.write(true).create_new(true).open(&tmp) {
            Ok(mut f) => {
                use std::io::Write;
                if let Err(e) = f.write_all(bytes) {
                    let _ = std::fs::remove_file(&tmp);
                    return Err(format!("{}: {e}", tmp.display()));
                }
                drop(f);
                if let Err(e) = std::fs::rename(&tmp, dest) {
                    let _ = std::fs::remove_file(&tmp);
                    return Err(format!("{}: {e}", dest.display()));
                }
                return Ok(());
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(format!("{}: {e}", tmp.display())),
        }
    }
    Err(format!("{}: cannot pick a temporary name", dest.display()))
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
    // The source writer's version must not leak into de-identified output.
    meta.implementation_version_name =
        Some(vyna_dcm_core::VYNA_IMPLEMENTATION_VERSION_NAME.to_string());
    let out = write_part10(&meta, &ds).map_err(|e| format!("{}: {e}", input.display()))?;
    write_output(dest, &out)?;
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
        match deid.with_replacement_name(name) {
            Ok(d) => deid = d,
            Err(e) => {
                eprintln!("deidentify: bad --patient-name: {e}");
                return 1;
            }
        }
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
        // Patient ID is E.1 `Z`: zeroed (kept but emptied), not dummied.
        assert!(summary.contains("1 zeroed"), "{summary}");
        let (meta2, ds2) = read_part10(&std::fs::read(&output).unwrap()).unwrap();
        assert_eq!(ds2.string(tags::PATIENT_ID), None);
        assert!(ds2.contains(tags::PATIENT_ID));
        assert_eq!(
            meta2.media_storage_sop_instance_uid,
            ds2.string(tags::SOP_INSTANCE_UID).unwrap()
        );
        assert_ne!(meta2.media_storage_sop_instance_uid, "1.9.9.9");
        std::fs::remove_file(&input).unwrap();
        std::fs::remove_file(&output).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn in_place_write_skips_planted_symlink() {
        let dir = std::env::temp_dir().join(format!("deid-out-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("anon.dcm");
        let victim = dir.join("victim.txt");
        std::fs::write(&file, b"original").unwrap();
        std::fs::write(&victim, b"precious").unwrap();
        // Plant a symlink at the first temp candidate: create-new must
        // refuse it (not follow it into the victim).
        let tmp0 = file.with_extension(format!("vyna-tmp-{}-0", std::process::id()));
        std::os::unix::fs::symlink(&victim, &tmp0).unwrap();
        write_output(&file, b"edited").unwrap();
        assert_eq!(std::fs::read(&file).unwrap(), b"edited");
        assert_eq!(std::fs::read(&victim).unwrap(), b"precious");
        // Non-in-place writes are atomic too, with no temp leftovers.
        let dest = dir.join("copy.dcm");
        write_output(&dest, b"copy").unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"copy");
        let mut names: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        let planted = tmp0.file_name().unwrap().to_string_lossy().into_owned();
        let mut expected = vec![
            "anon.dcm".to_string(),
            "copy.dcm".to_string(),
            "victim.txt".to_string(),
            planted,
        ];
        expected.sort();
        assert_eq!(names, expected);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
