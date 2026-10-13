//! `dcmdir`: create a DICOMDIR for referenced files, or list one.
//!
//! Exit status: 0 on success, 1 on failure.

#[path = "iocommon.rs"]
mod iocommon;

use std::path::{Component, Path, PathBuf};

use clap::Parser;
use vyna_dcm::{
    codec::dicomdir::{DicomDir, DirEntry, build_dicomdir, read_dicomdir},
    read_part10,
};

/// DICOMDIR media-directory tool.
#[derive(Parser, Debug)]
#[command(name = "dcmdir", about = "Create or list a DICOMDIR media directory")]
struct Args {
    /// List this DICOMDIR file instead of creating one.
    #[arg(long)]
    list: Option<PathBuf>,
    /// DICOMDIR file to write (create mode).
    #[arg(long)]
    output: Option<PathBuf>,
    /// File-set ID (create mode; default `VYNADCM`).
    #[arg(long)]
    file_set_id: Option<String>,
    /// Files or directories to index (create mode; directories are
    /// scanned recursively). Every file must sit under the output
    /// directory so its File ID stays relative.
    inputs: Vec<PathBuf>,
}

fn main() {
    std::process::exit(run(&Args::parse()));
}

fn run(args: &Args) -> i32 {
    if let Some(list) = &args.list {
        if args.output.is_some() || !args.inputs.is_empty() {
            eprintln!("dcmdir: --list takes no --output or inputs");
            return 1;
        }
        return run_list(list);
    }
    let Some(output) = &args.output else {
        eprintln!("dcmdir: create mode needs --output (or use --list)");
        return 1;
    };
    if args.inputs.is_empty() {
        eprintln!("dcmdir: create mode needs at least one input");
        return 1;
    }
    run_create(output, args.file_set_id.as_deref(), &args.inputs)
}

fn run_list(path: &Path) -> i32 {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("dcmdir: {}: {e}", path.display());
            return 1;
        }
    };
    let dir = match read_dicomdir(&bytes) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("dcmdir: {}: {e}", path.display());
            return 1;
        }
    };
    print_tree(&dir);
    0
}

fn print_tree(dir: &DicomDir) {
    let files = dir.files().len();
    println!(
        "File-set {}: {} patient(s), {} file(s)",
        dir.file_set_id.as_deref().unwrap_or("-"),
        dir.patients.len(),
        files
    );
    for patient in &dir.patients {
        println!(
            "{} [{}]",
            patient.id.as_deref().unwrap_or("-"),
            patient.name.as_deref().unwrap_or("-")
        );
        for study in &patient.studies {
            println!(
                "  {} ({})",
                study.uid.as_deref().unwrap_or("-"),
                study.id.as_deref().unwrap_or("-")
            );
            for series in &study.series {
                println!(
                    "    {} {}: {}",
                    series.modality.as_deref().unwrap_or("-"),
                    series.number.as_deref().unwrap_or("-"),
                    series.uid.as_deref().unwrap_or("-")
                );
                for inst in &series.instances {
                    println!(
                        "      {} {} [{}] ({})",
                        inst.record_type,
                        inst.sop_uid,
                        inst.relative_path().display(),
                        inst.ts_uid
                    );
                }
            }
        }
    }
    if !dir.skipped.is_empty() {
        println!("skipped: {}", dir.skipped.len());
        for skip in &dir.skipped {
            println!("  @{} {}: {}", skip.offset, skip.record_type, skip.reason);
        }
    }
}

/// Collect indexable files: explicit files must exist; directories are
/// scanned recursively (symlinks skipped); output is sorted.
fn collect_files(inputs: &[PathBuf], output: &Path) -> Result<Vec<(PathBuf, bool)>, String> {
    let mut out = Vec::new();
    for input in inputs {
        if input.is_dir() {
            scan_dir(input, &mut out).map_err(|e| format!("{}: {e}", input.display()))?;
        } else if input.is_file() {
            out.push((input.clone(), true));
        } else {
            return Err(format!("{}: no such file or directory", input.display()));
        }
    }
    out.sort();
    out.dedup();
    out.retain(|(p, _)| p != output);
    Ok(out)
}

fn scan_dir(dir: &Path, out: &mut Vec<(PathBuf, bool)>) -> std::io::Result<()> {
    let mut entries: Vec<_> = std::fs::read_dir(dir)?.collect::<Result<_, _>>()?;
    entries.sort_by_key(|e| e.path());
    for entry in entries {
        if entry.file_type()?.is_symlink() {
            continue;
        }
        let path = entry.path();
        if path.is_dir() {
            scan_dir(&path, out)?;
        } else if path.is_file() {
            out.push((path, false));
        }
    }
    Ok(())
}

fn run_create(output: &Path, file_set_id: Option<&str>, inputs: &[PathBuf]) -> i32 {
    // Containment root: the output directory, resolved. A bare
    // `--output DICOMDIR` resolves against the current directory —
    // stripping an empty prefix (always `Ok`) would skip containment
    // and bake absolute File IDs.
    let root_dir = output
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    let root = match root_dir.canonicalize() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("dcmdir: output directory {}: {e}", root_dir.display());
            return 1;
        }
    };
    let files = match collect_files(inputs, output) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("dcmdir: {e}");
            return 1;
        }
    };
    // Read everything first so one failure aborts before writing.
    let mut held = Vec::with_capacity(files.len());
    let mut entries = Vec::with_capacity(files.len());
    for (path, explicit) in &files {
        let bytes = match std::fs::read(path) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("dcmdir: {}: {e}", path.display());
                if *explicit {
                    return 1;
                }
                continue;
            }
        };
        let (meta, ds) = match read_part10(&bytes) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("dcmdir: {}: {e} (skipped)", path.display());
                if *explicit {
                    return 1;
                }
                continue;
            }
        };
        if meta
            .media_storage_sop_class_uid
            .trim_end_matches(['\0', ' '])
            == vyna_dcm::core::sop_class::MEDIA_STORAGE_DIRECTORY
        {
            eprintln!("dcmdir: {}: is itself a DICOMDIR (skipped)", path.display());
            continue;
        }
        let ts = match meta.transfer_syntax() {
            Ok(ts) => ts,
            Err(e) => {
                eprintln!("dcmdir: {}: {e}", path.display());
                if *explicit {
                    return 1;
                }
                continue;
            }
        };
        // Canonicalize both sides so `..` segments, absolute inputs,
        // and symlinks that escape the root all fail the strip below
        // (a contained absolute input still yields a relative ID).
        let canon = match path.canonicalize() {
            Ok(c) => c,
            Err(e) => {
                eprintln!("dcmdir: {}: {e}", path.display());
                if *explicit {
                    return 1;
                }
                continue;
            }
        };
        let rel = match canon.strip_prefix(&root) {
            Ok(r) => r,
            Err(_) => {
                eprintln!("dcmdir: {}: outside the DICOMDIR directory", path.display());
                return 1;
            }
        };
        held.push((ds, ts.uid().to_string()));
        let mut file_id = Vec::new();
        for c in rel.components() {
            match c {
                Component::Normal(s) => file_id.push(s.to_string_lossy().into_owned()),
                _ => {
                    eprintln!("dcmdir: {}: unrepresentable File ID", path.display());
                    return 1;
                }
            }
        }
        entries.push((file_id, held.len() - 1));
    }
    let refs: Vec<DirEntry> = entries
        .iter()
        .map(|(file_id, i)| DirEntry {
            dataset: &held[*i].0,
            file_id: file_id.clone(),
            transfer_syntax_uid: held[*i].1.clone(),
            record_type: None,
        })
        .collect();
    let bytes = match build_dicomdir(&refs, file_set_id) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("dcmdir: {e}");
            return 1;
        }
    };
    if let Err(e) = iocommon::write_atomic(output, &bytes) {
        eprintln!("dcmdir: {}: {e}", output.display());
        return 1;
    }
    println!(
        "{}: {} file(s) indexed ({} bytes)",
        output.display(),
        refs.len(),
        bytes.len()
    );
    0
}
