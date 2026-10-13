//! `dcmodify`: edit tags in a Part-10 file (modify/insert/erase).
//!
//! Exit status: 0 on success, 1 on failure. Edits apply in phases —
//! all erases, then all modifies, then all inserts — in CLI order
//! within each phase. Without `--output` the input file is replaced
//! (via a temporary file + rename, so a failed run keeps the original).
//! File-meta SOP UIDs are synced from the edited dataset; erasing or
//! blanking them refuses to write.

#[path = "iocommon.rs"]
mod iocommon;

use std::path::{Path, PathBuf};

use clap::Parser;
use vyna_dcm::{
    DataElement, DataSet, Tag, TransferSyntax, Value, Vr,
    core::{dictionary_vr, is_valid_uid, lookup_keyword},
    read_part10, write_part10,
};

/// DICOM tag editor.
#[derive(Parser, Debug)]
#[command(
    name = "dcmodify",
    about = "Modify, insert, or erase tags in a Part-10 file"
)]
struct Args {
    /// Part-10 file to edit.
    input: PathBuf,
    /// Output file (default: replace the input).
    #[arg(short, long)]
    output: Option<PathBuf>,
    /// Modify an existing tag: `PATH=VALUE` (keeps the element's
    /// VR). Repeatable. Multi-valued: separate values with `\`.
    /// Paths descend into sequences: `SEQ[0].TAG` (0-based).
    #[arg(short = 'm', long = "modify")]
    modifies: Vec<String>,
    /// Erase a tag (warns and continues when absent). Repeatable.
    /// Accepts `SEQ[0].TAG` paths.
    #[arg(short = 'e', long = "erase")]
    erases: Vec<String>,
    /// Insert a tag: `PATH=VALUE` or `PATH:VR=VALUE` (`:VR` required
    /// when the tag has no dictionary entry). Repeatable.
    #[arg(short = 'i', long = "insert")]
    inserts: Vec<String>,
    /// Transfer syntax UID for the output (default: keep the input's).
    #[arg(long)]
    transfer_syntax: Option<String>,
}

/// Tags look like `(0010,0010)`, `0010,0010`, `00100010`, or a
/// keyword such as `PatientName`.
fn parse_tag_spec(spec: &str) -> Result<Tag, String> {
    let mut s = spec.trim();
    if s.starts_with('(') {
        s = s
            .strip_prefix('(')
            .and_then(|s| s.strip_suffix(')'))
            .ok_or_else(|| format!("bad tag {spec:?}: unbalanced parentheses"))?;
    }
    if let Some((g, e)) = s.split_once(',') {
        let hex = |part: &str, what: &str| {
            u16::from_str_radix(part.trim(), 16)
                .map_err(|_| format!("bad tag {spec:?}: {what} is not hex"))
        };
        if g.trim().is_empty() || e.trim().is_empty() {
            return Err(format!("bad tag {spec:?}: want (GGGG,EEEE)"));
        }
        return Ok(Tag::new(hex(g, "group")?, hex(e, "element")?));
    }
    if s.len() == 8 && s.bytes().all(|b| b.is_ascii_hexdigit()) {
        let g = u16::from_str_radix(&s[..4], 16).expect("checked hex");
        let e = u16::from_str_radix(&s[4..], 16).expect("checked hex");
        return Ok(Tag::new(g, e));
    }
    lookup_keyword(s)
        .map(|e| e.tag)
        .ok_or_else(|| format!("unknown tag {spec:?}: want (GGGG,EEEE), GGGGEEEE, or a keyword"))
}

/// An edit path: sequence hops (`TAG[index]`, 0-based) leading to a
/// final tag, e.g. `ReferencedImageSequence[0].SeriesDescription`.
/// A bare `TAG` addresses the top-level dataset.
#[derive(Debug, PartialEq, Eq)]
struct EditPath {
    hops: Vec<(Tag, usize)>,
    tag: Tag,
}

/// Split an insert's `PATH[:VR]` left-hand side (`:` cannot appear in
/// a path, so the first colon is the VR separator).
fn parse_path_vr(left: &str) -> Result<(EditPath, Option<Vr>), String> {
    match left.split_once(':') {
        None => Ok((parse_edit_path(left)?, None)),
        Some((path, vr)) => {
            let vr = vr
                .to_ascii_uppercase()
                .parse::<Vr>()
                .map_err(|_| format!("unknown VR {vr:?} in {left:?}"))?;
            Ok((parse_edit_path(path)?, Some(vr)))
        }
    }
}

fn parse_edit_path(left: &str) -> Result<EditPath, String> {
    // Tag specs (keywords, hex, `(GGGG,EEEE)`) never contain `.`.
    let segs: Vec<&str> = left.split('.').collect();
    if segs.iter().any(|s| s.trim().is_empty()) {
        return Err(format!("bad path {left:?}: empty segment"));
    }
    let mut hops = Vec::with_capacity(segs.len().saturating_sub(1));
    for seg in &segs[..segs.len() - 1] {
        let (tag_part, idx) = split_hop(seg, left)?;
        hops.push((parse_tag_spec(tag_part)?, idx));
    }
    let last = segs[segs.len() - 1];
    if last.contains('[') {
        return Err(format!("bad path {left:?}: the final tag takes no [index]"));
    }
    Ok(EditPath {
        hops,
        tag: parse_tag_spec(last)?,
    })
}

/// Split a `TAG[index]` hop; a bare tag is rejected (descending needs
/// an explicit 0-based item index).
fn split_hop<'a>(seg: &'a str, whole: &str) -> Result<(&'a str, usize), String> {
    let (tag_part, rest) = seg
        .split_once('[')
        .ok_or_else(|| format!("bad path {whole:?}: {seg:?} needs [index] to descend"))?;
    let idx = rest
        .strip_suffix(']')
        .ok_or_else(|| format!("bad path {whole:?}: {seg:?} wants TAG[index]"))?;
    let idx: usize = idx
        .parse()
        .map_err(|_| format!("bad path {whole:?}: {idx:?} is not an item index"))?;
    if tag_part.trim().is_empty() {
        return Err(format!("bad path {whole:?}: empty tag in {seg:?}"));
    }
    Ok((tag_part, idx))
}

/// Follow `hops` from `ds`; every hop must name an existing sequence
/// with an in-range item (edits never create items).
fn descend<'a>(ds: &'a mut DataSet, hops: &[(Tag, usize)]) -> Result<&'a mut DataSet, String> {
    let mut cur = ds;
    for (tag, idx) in hops {
        let el = cur
            .get_mut(*tag)
            .ok_or_else(|| format!("({tag}): not present; path hops must exist"))?;
        match &mut el.value {
            Value::Sequence(items) => {
                let len = items.len();
                cur = items.get_mut(*idx).ok_or_else(|| {
                    format!("({tag})[{idx}]: item index out of range (len {len})")
                })?;
            }
            _ => {
                return Err(format!("({tag}): not a sequence (VR {})", el.vr));
            }
        }
    }
    Ok(cur)
}

/// Build an element from CLI text for `vr` (`\` separates values;
/// empty text means present-but-empty; `@path` reads file bytes for
/// binary VRs).
fn parse_values(tag: Tag, vr: Vr, text: &str) -> Result<DataElement, String> {
    if text.is_empty() {
        return Ok(DataElement::empty(tag, vr));
    }
    let bad = |what: &str| format!("({tag}): bad {vr} value {what:?}");
    let parts: Vec<&str> = text.split('\\').collect();
    let value = match vr {
        Vr::Ae
        | Vr::As
        | Vr::Cs
        | Vr::Da
        | Vr::Ds
        | Vr::Dt
        | Vr::Is
        | Vr::Lo
        | Vr::Lt
        | Vr::Sh
        | Vr::St
        | Vr::Tm
        | Vr::Uc
        | Vr::Ur
        | Vr::Ut => Value::Text(parts.into_iter().map(str::to_string).collect()),
        Vr::Ui => {
            for p in &parts {
                if !is_valid_uid(p) {
                    return Err(bad(p));
                }
            }
            Value::Text(parts.into_iter().map(str::to_string).collect())
        }
        Vr::Pn => parts
            .into_iter()
            .map(vyna_dcm::PersonName::try_parse)
            .collect::<Result<Vec<_>, _>>()
            .map(Value::PersonName)
            .map_err(|e| bad(&e.to_string()))?,
        Vr::At => parts
            .into_iter()
            .map(parse_tag_spec)
            .collect::<Result<Vec<_>, _>>()
            .map(Value::Tags)?,
        Vr::Us => parse_numbers(&parts, &bad, str::parse::<u16>).map(Value::U16)?,
        Vr::Ss => parse_numbers(&parts, &bad, str::parse::<i16>).map(Value::I16)?,
        Vr::Ul => parse_numbers(&parts, &bad, str::parse::<u32>).map(Value::U32)?,
        Vr::Sl => parse_numbers(&parts, &bad, str::parse::<i32>).map(Value::I32)?,
        Vr::Uv => parse_numbers(&parts, &bad, str::parse::<u64>).map(Value::U64)?,
        Vr::Sv => parse_numbers(&parts, &bad, str::parse::<i64>).map(Value::I64)?,
        Vr::Fl => parse_numbers(&parts, &bad, str::parse::<f32>).map(Value::F32)?,
        Vr::Fd => parse_numbers(&parts, &bad, str::parse::<f64>).map(Value::F64)?,
        Vr::Ob | Vr::Ow | Vr::Od | Vr::Of | Vr::Ol | Vr::Ov => {
            let Some(path) = text.strip_prefix('@') else {
                return Err(format!(
                    "({tag}): binary VR {vr} needs @path (e.g. -m '{tag}=@bytes.bin')"
                ));
            };
            Value::Bytes(read_aux_file(path)?)
        }
        Vr::Un => {
            if let Some(path) = text.strip_prefix('@') {
                Value::Bytes(read_aux_file(path)?)
            } else {
                Value::Bytes(text.as_bytes().to_vec())
            }
        }
        Vr::Sq => {
            return Err(format!(
                "({tag}): sequences can't be set wholesale; edit items via SEQ[index].TAG paths"
            ));
        }
        Vr::Si => {
            // Retired (no standard-defined width); the codec treats
            // it as bytes.
            let Some(path) = text.strip_prefix('@') else {
                return Err(format!("({tag}): binary VR {vr} needs @path"));
            };
            Value::Bytes(read_aux_file(path)?)
        }
        _ => {
            return Err(format!("({tag}): unsupported VR {vr}"));
        }
    };
    Ok(DataElement::new(tag, vr, value))
}

fn parse_numbers<T, E, F>(
    parts: &[&str],
    bad: &dyn Fn(&str) -> String,
    f: F,
) -> Result<Vec<T>, String>
where
    F: Fn(&str) -> Result<T, E> + Copy,
{
    parts.iter().map(|p| f(p).map_err(|_| bad(p))).collect()
}

fn read_aux_file(path: &str) -> Result<Vec<u8>, String> {
    if path.is_empty() {
        return Err("empty @path for binary value".to_string());
    }
    std::fs::read(path).map_err(|e| format!("@{path}: {e}"))
}

/// How many edits of each kind were applied.
#[derive(Debug, Default, PartialEq, Eq)]
struct EditCounts {
    modified: usize,
    inserted: usize,
    erased: usize,
    warnings: Vec<String>,
}

/// Apply CLI edits to a dataset: erases, then modifies, then inserts.
fn apply_edits(
    ds: &mut DataSet,
    modifies: &[String],
    erases: &[String],
    inserts: &[String],
) -> Result<EditCounts, String> {
    let mut counts = EditCounts::default();
    for spec in erases {
        if spec.contains('=') || spec.contains(':') {
            return Err(format!("bad --erase {spec:?}: want PATH without = or :VR"));
        }
        let path = parse_edit_path(spec)?;
        let target = descend(ds, &path.hops)?;
        if target.remove(path.tag).is_none() {
            counts
                .warnings
                .push(format!("({}): not present, nothing erased", path.tag));
        } else {
            counts.erased += 1;
        }
    }
    for spec in modifies {
        let (left, text) = spec
            .split_once('=')
            .ok_or_else(|| format!("bad --modify {spec:?}: want PATH=VALUE"))?;
        if left.contains(':') {
            return Err(format!(
                "bad --modify {spec:?}: modifies keep the existing VR (no :VR)"
            ));
        }
        let path = parse_edit_path(left)?;
        let target = descend(ds, &path.hops)?;
        let vr = target
            .get(path.tag)
            .map(|el| el.vr)
            .ok_or_else(|| format!("({}): not present; use --insert to add it", path.tag))?;
        target.insert(parse_values(path.tag, vr, text)?);
        counts.modified += 1;
    }
    for spec in inserts {
        let (left, text) = spec
            .split_once('=')
            .ok_or_else(|| format!("bad --insert {spec:?}: want PATH[:VR]=VALUE"))?;
        let (path, explicit_vr) = parse_path_vr(left)?;
        let target = descend(ds, &path.hops)?;
        let tag = path.tag;
        if target.contains(tag) {
            return Err(format!(
                "({tag}): already present; use --modify to change it"
            ));
        }
        match (tag.0, tag.1) {
            (0x0000, _) | (0x0002, _) | (0xFFFE, _) => {
                return Err(format!(
                    "({tag}): command, file-meta, and delimiter groups can't be inserted"
                ));
            }
            (_, 0x0000) => {
                return Err(format!("({tag}): group lengths are managed by the writer"));
            }
            _ => {}
        }
        let vr = match explicit_vr {
            Some(vr) => vr,
            None => dictionary_vr(tag)
                .ok_or_else(|| format!("({tag}): no dictionary entry; use TAG:VR=VALUE"))?,
        };
        target.insert(parse_values(tag, vr, text)?);
        counts.inserted += 1;
    }
    Ok(counts)
}

fn main() {
    std::process::exit(run(&Args::parse()));
}

fn run(args: &Args) -> i32 {
    let bytes = match std::fs::read(&args.input) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("dcmodify: {}: {e}", args.input.display());
            return 1;
        }
    };
    let (mut meta, mut ds) = match read_part10(&bytes) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("dcmodify: {}: {e}", args.input.display());
            return 1;
        }
    };
    let ts = match &args.transfer_syntax {
        Some(uid) => match TransferSyntax::from_uid(uid) {
            Some(ts) => ts,
            None => {
                eprintln!("dcmodify: unknown transfer syntax {uid}");
                return 1;
            }
        },
        None => match meta.transfer_syntax() {
            Ok(ts) => ts,
            Err(e) => {
                eprintln!("dcmodify: {}: {e}", args.input.display());
                return 1;
            }
        },
    };
    let counts = match apply_edits(&mut ds, &args.modifies, &args.erases, &args.inserts) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("dcmodify: {e}");
            return 1;
        }
    };
    for w in &counts.warnings {
        eprintln!("dcmodify: warning: {w}");
    }
    // File-meta SOP UIDs follow the edited dataset.
    let class = match ds
        .string(vyna_dcm::tags::SOP_CLASS_UID)
        .filter(|s| !s.is_empty())
    {
        Some(s) => s.to_string(),
        None => {
            eprintln!("dcmodify: edits removed SOP Class UID; refusing to write");
            return 1;
        }
    };
    let inst = match ds
        .string(vyna_dcm::tags::SOP_INSTANCE_UID)
        .filter(|s| !s.is_empty())
    {
        Some(s) => s.to_string(),
        None => {
            eprintln!("dcmodify: edits removed SOP Instance UID; refusing to write");
            return 1;
        }
    };
    let meta_synced =
        meta.media_storage_sop_class_uid != class || meta.media_storage_sop_instance_uid != inst;
    meta.media_storage_sop_class_uid = class;
    meta.media_storage_sop_instance_uid = inst;
    meta.transfer_syntax_uid = ts.uid().to_string();
    let out = match write_part10(&meta, &ds) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("dcmodify: {e}");
            return 1;
        }
    };
    let dest = args.output.as_deref().unwrap_or(&args.input);
    if let Err(e) = write_output(dest, &args.input, &out) {
        eprintln!("dcmodify: {e}");
        return 1;
    }
    println!(
        "{} -> {}: {} modified, {} inserted, {} erased{}",
        args.input.display(),
        dest.display(),
        counts.modified,
        counts.inserted,
        counts.erased,
        if meta_synced {
            " (file meta UIDs synced)"
        } else {
            ""
        }
    );
    0
}

/// Write the edited file; in-place replacement goes through a
/// temporary file + rename so a failed run keeps the original.
///
/// The temp name carries the pid plus an attempt counter and is
/// created with create-new semantics: concurrent runs on the same
/// file must not share one fixed temp name (and a planted symlink
/// must not redirect the write elsewhere).
fn write_output(dest: &Path, input: &Path, bytes: &[u8]) -> Result<(), String> {
    if dest != input {
        return iocommon::write_atomic(dest, bytes).map_err(|e| format!("{}: {e}", dest.display()));
    }
    for attempt in 0..100u32 {
        let tmp = input.with_extension(format!("vyna-tmp-{}-{attempt}", std::process::id()));
        let mut opts = std::fs::OpenOptions::new();
        match opts.write(true).create_new(true).open(&tmp) {
            Ok(mut f) => {
                use std::io::Write;
                if let Err(e) = f.write_all(bytes) {
                    let _ = std::fs::remove_file(&tmp);
                    return Err(format!("{}: {e}", tmp.display()));
                }
                drop(f);
                if let Err(e) = std::fs::rename(&tmp, input) {
                    let _ = std::fs::remove_file(&tmp);
                    return Err(format!("{}: {e}", input.display()));
                }
                return Ok(());
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(format!("{}: {e}", tmp.display())),
        }
    }
    Err(format!("{}: cannot pick a temporary name", input.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use vyna_dcm::core::tags;

    #[test]
    fn tag_specs_parse() {
        let want = Tag::new(0x0010, 0x0010);
        for spec in [
            "(0010,0010)",
            "(0010, 0010)",
            "0010,0010",
            "00100010",
            "PatientName",
        ] {
            assert_eq!(parse_tag_spec(spec).unwrap(), want, "{spec}");
        }
        assert_eq!(
            parse_tag_spec("(0009,1001)").unwrap(),
            Tag::new(0x0009, 0x1001)
        );
        assert!(parse_tag_spec("(0010,0010").is_err());
        assert!(parse_tag_spec("0010,").is_err());
        assert!(parse_tag_spec("NoSuchKeyword").is_err());
        assert!(parse_tag_spec("0010,ZZZZ").is_err());
    }

    #[test]
    fn insert_vr_suffix_parses() {
        let (path, vr) = parse_path_vr("PatientName:PN").unwrap();
        assert!(path.hops.is_empty());
        assert_eq!((path.tag, vr), (tags::PATIENT_NAME, Some(Vr::Pn)));
        let (path, vr) = parse_path_vr("(0009,1001):lo").unwrap();
        assert!(path.hops.is_empty());
        assert_eq!((path.tag, vr), (Tag::new(0x0009, 0x1001), Some(Vr::Lo)));
        let (path, vr) = parse_path_vr("ReferencedImageSequence[0].(0009,1001):LO").unwrap();
        assert_eq!(path.hops, vec![(REF_IMG_SEQ, 0)]);
        assert_eq!((path.tag, vr), (Tag::new(0x0009, 0x1001), Some(Vr::Lo)));
        assert!(parse_path_vr("00100010:XX").is_err());
        assert_eq!(parse_path_vr("00100010").unwrap().1, None);
    }

    #[test]
    fn values_parse_per_vr() {
        let tag = tags::PATIENT_NAME;
        let el = parse_values(tag, Vr::Pn, "Doe^Jane\\Smith^John").unwrap();
        assert_eq!(el.value.multiplicity(), 2);
        let el = parse_values(tag, Vr::Ui, "1.2.3.4").unwrap();
        assert!(matches!(el.value, Value::Text(_)));
        assert!(parse_values(tag, Vr::Ui, "not a uid!").is_err());
        let el = parse_values(tags::INSTANCE_NUMBER, Vr::Is, "7").unwrap();
        assert!(matches!(el.value, Value::Text(_)));
        let el = parse_values(Tag::new(0x0028, 0x0011), Vr::Us, "512\\512").unwrap();
        assert_eq!(el.value.multiplicity(), 2);
        assert!(parse_values(Tag::new(0x0028, 0x0011), Vr::Us, "512x").is_err());
        let el = parse_values(Tag::new(0x0018, 0x0081), Vr::Ds, "120").unwrap();
        assert!(matches!(el.value, Value::Text(_)));
        let el = parse_values(Tag::new(0x0008, 0x1140), Vr::At, "PatientName\\00100020").unwrap();
        assert_eq!(el.value.multiplicity(), 2);
        assert!(parse_values(tag, Vr::Sq, "x").is_err());
        // Empty text blanks the element, keeping its VR.
        let el = parse_values(tag, Vr::Lo, "").unwrap();
        assert_eq!(el.vr, Vr::Lo);
        assert!(el.value.is_empty());
        // UN takes literal bytes; other binary VRs need @path.
        let el = parse_values(tag, Vr::Un, "abc").unwrap();
        assert_eq!(el.value, Value::Bytes(b"abc".to_vec()));
        assert!(parse_values(tag, Vr::Ob, "abc").is_err());
        assert!(parse_values(tag, Vr::Ob, "@/no/such/file").is_err());
    }

    #[test]
    fn binary_values_read_from_file() {
        let dir = std::env::temp_dir().join(format!("dcmodify-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("blob.bin");
        std::fs::write(&path, [1u8, 2, 3]).unwrap();
        let el = parse_values(tags::PIXEL_DATA, Vr::Ow, &format!("@{}", path.display())).unwrap();
        assert_eq!(el.value, Value::Bytes(vec![1, 2, 3]));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    const REF_IMG_SEQ: Tag = Tag::new(0x0008, 0x1140);

    fn sample() -> DataSet {
        let mut ds = DataSet::new();
        ds.put_uid(tags::SOP_CLASS_UID, "1.2.3");
        ds.put_uid(tags::SOP_INSTANCE_UID, "1.2.3.4");
        ds.put_person_name(
            tags::PATIENT_NAME,
            vec![vyna_dcm::PersonName::parse("Doe^Jane")],
        );
        ds.put_text(tags::PATIENT_ID, Vr::Lo, "P123");
        ds
    }

    fn sample_with_sequence() -> DataSet {
        let mut ds = sample();
        let mut item0 = DataSet::new();
        item0.put_text(tags::SERIES_DESCRIPTION, Vr::Sh, "Old");
        let mut item1 = DataSet::new();
        item1.put_text(tags::SERIES_DESCRIPTION, Vr::Sh, "Second");
        ds.insert(DataElement::new(
            REF_IMG_SEQ,
            Vr::Sq,
            Value::Sequence(vec![item0, item1]),
        ));
        ds
    }

    #[test]
    fn sequence_paths_parse() {
        let p = parse_edit_path("PatientName").unwrap();
        assert!(p.hops.is_empty());
        assert_eq!(p.tag, tags::PATIENT_NAME);
        let p = parse_edit_path("ReferencedImageSequence[0].SeriesDescription").unwrap();
        assert_eq!(p.hops, vec![(REF_IMG_SEQ, 0)]);
        assert_eq!(p.tag, tags::SERIES_DESCRIPTION);
        let p = parse_edit_path("(0008,1140)[1].(0008,103E)").unwrap();
        assert_eq!(p.hops, vec![(REF_IMG_SEQ, 1)]);
        assert_eq!(p.tag, tags::SERIES_DESCRIPTION);
        // Nested hops.
        let p =
            parse_edit_path("ReferencedImageSequence[0].ReferencedImageSequence[1].SOPInstanceUID")
                .unwrap();
        assert_eq!(p.hops, vec![(REF_IMG_SEQ, 0), (REF_IMG_SEQ, 1)]);
        // Violations.
        assert!(parse_edit_path("ReferencedImageSequence.SeriesDescription").is_err());
        assert!(parse_edit_path("ReferencedImageSequence[0]").is_err());
        assert!(parse_edit_path("ReferencedImageSequence[x].SeriesDescription").is_err());
        assert!(parse_edit_path("ReferencedImageSequence[0].SeriesDescription[0]").is_err());
        assert!(parse_edit_path("").is_err());
    }

    #[test]
    fn sequence_edits_apply_per_item() {
        let mut ds = sample_with_sequence();
        let counts = apply_edits(
            &mut ds,
            &["ReferencedImageSequence[1].SeriesDescription=New".to_string()],
            &["ReferencedImageSequence[0].SeriesDescription".to_string()],
            &["ReferencedImageSequence[0].InstitutionName=St Elsewhere".to_string()],
        )
        .unwrap();
        assert_eq!((counts.modified, counts.inserted, counts.erased), (1, 1, 1));
        let items = ds.sequence(REF_IMG_SEQ).unwrap();
        assert!(!items[0].contains(tags::SERIES_DESCRIPTION));
        assert_eq!(
            items[0].string(tags::INSTITUTION_NAME),
            Some("St Elsewhere")
        );
        assert_eq!(items[1].string(tags::SERIES_DESCRIPTION), Some("New"));
    }

    #[test]
    fn sequence_edit_violations_error() {
        let mut ds = sample_with_sequence();
        // Item index out of range.
        assert!(
            apply_edits(
                &mut ds,
                &["ReferencedImageSequence[7].SeriesDescription=X".to_string()],
                &[],
                &[],
            )
            .is_err()
        );
        // Hop through a non-sequence.
        assert!(
            apply_edits(
                &mut ds,
                &["PatientID[0].SeriesDescription=X".to_string()],
                &[],
                &[]
            )
            .is_err()
        );
        // Modify a tag missing from the item.
        assert!(
            apply_edits(
                &mut ds,
                &["ReferencedImageSequence[0].Modality=CT".to_string()],
                &[],
                &[]
            )
            .is_err()
        );
        // Insert a tag already in the item.
        assert!(
            apply_edits(
                &mut ds,
                &[],
                &[],
                &["ReferencedImageSequence[0].SeriesDescription=X".to_string()],
            )
            .is_err()
        );
        // Erasing a missing item tag warns instead of failing.
        let counts = apply_edits(
            &mut ds,
            &[],
            &["ReferencedImageSequence[0].Modality".to_string()],
            &[],
        )
        .unwrap();
        assert_eq!(counts.erased, 0);
        assert_eq!(counts.warnings.len(), 1);
    }

    #[test]
    fn edits_apply_in_phases() {
        let mut ds = sample();
        let counts = apply_edits(
            &mut ds,
            &["PatientName=Smith^John".to_string()],
            &["AccessionNumber".to_string(), "PatientID".to_string()],
            &["InstitutionName=St Elsewhere".to_string()],
        )
        .unwrap();
        assert_eq!(counts.modified, 1);
        assert_eq!(counts.inserted, 1);
        assert_eq!(counts.erased, 1); // PatientID; AccessionNumber only warned
        assert_eq!(counts.warnings.len(), 1);
        assert_eq!(
            ds.person_names(tags::PATIENT_NAME).unwrap()[0]
                .alphabetic
                .family,
            "Smith"
        );
        assert!(!ds.contains(tags::PATIENT_ID));
        assert_eq!(ds.string(tags::INSTITUTION_NAME), Some("St Elsewhere"));
    }

    #[test]
    fn edit_violations_error() {
        let mut ds = sample();
        // Modify missing.
        assert!(apply_edits(&mut ds, &["Modality=CT".to_string()], &[], &[]).is_err());
        // Modify with :VR.
        assert!(apply_edits(&mut ds, &["PatientID:LO=X".to_string()], &[], &[]).is_err());
        // Insert present.
        assert!(apply_edits(&mut ds, &[], &[], &["PatientID=X".to_string()]).is_err());
        // Insert unknown tag without VR.
        assert!(apply_edits(&mut ds, &[], &[], &["(0009,1001)=X".to_string()]).is_err());
        // Insert guarded groups.
        assert!(apply_edits(&mut ds, &[], &[], &["(0002,0010)=X".to_string()]).is_err());
        assert!(apply_edits(&mut ds, &[], &[], &["(0010,0000)=4".to_string()]).is_err());
        // Insert unknown tag with VR works.
        let counts = apply_edits(&mut ds, &[], &[], &["(0009,1001):LO=X".to_string()]).unwrap();
        assert_eq!(counts.inserted, 1);
        assert_eq!(ds.string(Tag::new(0x0009, 0x1001)), Some("X"));
        // Erase with =.
        assert!(apply_edits(&mut ds, &[], &["PatientID=X".to_string()], &[]).is_err());
    }

    #[test]
    fn full_edit_cycle_through_part10() {
        use vyna_dcm::TransferSyntax as Ts;
        use vyna_dcm::pdf::{PdfInstanceMeta, file_meta_for_dataset, pdf_to_dataset};
        let pdf = b"%PDF-1.4\n%%EOF\n";
        let ds = pdf_to_dataset(
            pdf,
            &PdfInstanceMeta::new(vyna_dcm::PersonName::parse("Doe^Jane"), "P123"),
        )
        .unwrap();
        let meta = file_meta_for_dataset(&ds, Ts::ExplicitVrLittleEndian).unwrap();
        let file = write_part10(&meta, &ds).unwrap();
        let (meta2, mut ds2) = read_part10(&file).unwrap();
        assert_eq!(meta2.transfer_syntax_uid, Ts::ExplicitVrLittleEndian.uid());
        let counts = apply_edits(
            &mut ds2,
            &["PatientName=Smith^John".to_string()],
            &[],
            &["SeriesDescription=Edited".to_string()],
        )
        .unwrap();
        assert_eq!((counts.modified, counts.inserted), (1, 1));
        let meta3 = vyna_dcm::FileMeta::new(
            ds2.string(tags::SOP_CLASS_UID).unwrap(),
            ds2.string(tags::SOP_INSTANCE_UID).unwrap(),
            Ts::ExplicitVrLittleEndian,
        );
        let file3 = write_part10(&meta3, &ds2).unwrap();
        let (_, ds3) = read_part10(&file3).unwrap();
        assert_eq!(
            ds3.person_names(tags::PATIENT_NAME).unwrap()[0]
                .alphabetic
                .family,
            "Smith"
        );
        assert_eq!(ds3.string(tags::SERIES_DESCRIPTION), Some("Edited"));
        // Edited instance still validates clean.
        let report = vyna_dcm::validate(
            &ds3,
            vyna_dcm::core::sop_class::ENCAPSULATED_PDF,
            Some(Ts::ExplicitVrLittleEndian),
        )
        .unwrap();
        assert!(report.is_clean());
    }

    #[test]
    fn in_place_write_replaces_and_cleans_up() {
        let dir = std::env::temp_dir().join(format!("dcmodify-out-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("edit.dcm");
        std::fs::write(&file, b"original").unwrap();
        write_output(&file, &file, b"edited").unwrap();
        assert_eq!(std::fs::read(&file).unwrap(), b"edited");
        // No temp litter left behind.
        let litter: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.file_name()
                    .to_str()
                    .is_some_and(|n| n.contains("vyna-tmp"))
            })
            .collect();
        assert!(litter.is_empty(), "{litter:?}");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
