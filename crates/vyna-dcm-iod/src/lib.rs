#![warn(missing_docs)]
//! IOD validation: Type 1/2/3 presence plus VR and VM checking.
//!
//! [`validate`] checks a dataset against one of the supported Information
//! Object Definitions (see [`IODS`], generated from NEMA PS3.3 by
//! `docs/generate-iod.py`):
//!
//! - **Type 1** attributes must be present and non-empty.
//! - **Type 2** attributes must be present (they may be empty).
//! - **Type 3** attributes are checked only when present.
//! - Present attributes must use the dictionary VR (or a listed alternate)
//!   and satisfy the dictionary Value Multiplicity.
//!
//! Scope notes (see the generator for detail):
//!
//! - Top-level attributes only; sequence *items* are not descended into.
//! - Only modules with IOD Usage M enforce presence; the rest are
//!   VR/VM-checked when present.
//! - Type 1C/2C conditions are evaluated only for the curated
//!   [`CondRule`] set; the rest behave like Type 3.
//! - VM is not checked for bulk (`OB`/`OW`/…) values, fragments, or
//!   sequences, whose counts don't map to dictionary VM.
//! - File-context assumption: the STOW-RS metadata exception does not
//!   apply, so e.g. Pixel Data is required.

use std::fmt;

use vyna_dcm_core::{DataSet, Tag, TransferSyntax, Value, Vr, lookup, tags};

mod iod_generated;

pub use iod_generated::IODS;

/// Presence requirement of one IOD attribute.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Req {
    /// Must be present and non-empty.
    T1,
    /// Must be present; may be empty.
    T2,
    /// Checked only when present.
    IfPresent,
    /// Required (present and non-empty) when the rule holds.
    Cond(CondRule),
}

/// Evaluable Type-1C conditions (see `docs/generate-iod.py`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum CondRule {
    /// Pixel Data: always required in file/C-STORE context.
    PixelData,
    /// Frame Increment Pointer: required for multi-frame images.
    FrameIncrementPointer,
    /// Planar Configuration: required for color images.
    PlanarConfiguration,
    /// Lossy Image Compression: required for lossy transfer syntaxes.
    LossyCompression,
    /// Lossy Image Compression Ratio: required when compression flag is `01`.
    LossyRatio,
    /// Specific Character Set: required when text uses non-ASCII characters.
    SpecificCharacterSet,
    /// Extended Offset Table Lengths: required with Extended Offset Table.
    ExtOffsetTableLengths,
}

impl CondRule {
    /// Whether the guarded attribute is required for this dataset.
    #[must_use]
    pub fn required(&self, ds: &DataSet, ts: Option<TransferSyntax>) -> bool {
        match self {
            CondRule::PixelData => true,
            CondRule::FrameIncrementPointer => ds
                .string(tags::NUMBER_OF_FRAMES)
                .and_then(|s| s.trim().parse::<u32>().ok())
                .is_some_and(|n| n > 1),
            CondRule::PlanarConfiguration => {
                ds.uint16(tags::SAMPLES_PER_PIXEL).is_some_and(|n| n > 1)
            }
            CondRule::LossyCompression => ts.is_some_and(|ts| ts.is_lossy()),
            CondRule::LossyRatio => ds
                .string(tags::LOSSY_IMAGE_COMPRESSION)
                .is_some_and(|s| s.trim().trim_end_matches('\0') == "01"),
            CondRule::SpecificCharacterSet => has_non_ascii_text(ds),
            CondRule::ExtOffsetTableLengths => ds.get(tags::EXTENDED_OFFSET_TABLE).is_some(),
        }
    }

    /// Human-readable condition for finding messages.
    #[must_use]
    pub fn describe(&self) -> &'static str {
        match self {
            CondRule::PixelData => "pixel data is required in file/C-STORE instances",
            CondRule::FrameIncrementPointer => "required for multi-frame images",
            CondRule::PlanarConfiguration => "required for color images",
            CondRule::LossyCompression => "required for lossy transfer syntaxes",
            CondRule::LossyRatio => "required when Lossy Image Compression is 01",
            CondRule::SpecificCharacterSet => "required when text uses non-ASCII characters",
            CondRule::ExtOffsetTableLengths => "required with Extended Offset Table",
        }
    }
}

/// One attribute row of an IOD table.
#[derive(Clone, Copy, Debug)]
pub struct IodAttr {
    /// Attribute tag.
    pub tag: Tag,
    /// Presence requirement.
    pub req: Req,
    /// Module that contributes the requirement (for messages).
    pub module: &'static str,
}

/// One supported Information Object Definition.
#[derive(Clone, Copy, Debug)]
pub struct Iod {
    /// SOP Class UID.
    pub uid: &'static str,
    /// SOP Class name.
    pub name: &'static str,
    /// Attribute rows in ascending tag order.
    pub attrs: &'static [IodAttr],
}

/// Find the IOD for a SOP Class UID (trailing padding tolerated).
#[must_use]
pub fn find_iod(sop_class_uid: &str) -> Option<&'static Iod> {
    let uid = sop_class_uid.trim_end_matches(['\0', ' ']);
    IODS.iter().find(|iod| iod.uid == uid)
}

/// Finding severity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    /// Report only; the instance may still be usable.
    Warning,
    /// Conformance violation.
    Error,
}

impl fmt::Display for Severity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Severity::Warning => write!(f, "warning"),
            Severity::Error => write!(f, "error"),
        }
    }
}

/// Machine-readable finding category.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Kind {
    /// Type 1 attribute absent.
    MissingType1,
    /// Type 1 attribute present but empty.
    EmptyType1,
    /// Type 2 attribute absent.
    MissingType2,
    /// Conditionally-required attribute absent or empty.
    CondRequired(CondRule),
    /// Element VR is not the dictionary VR or a listed alternate.
    VrMismatch {
        /// Accepted VRs, primary first.
        expected: Vec<Vr>,
        /// VR on the element.
        found: Vr,
    },
    /// Value multiplicity outside the dictionary VM.
    VmMismatch {
        /// Dictionary VM string (e.g. `1-n`).
        expected: String,
        /// Observed multiplicity.
        found: usize,
    },
    /// Explicit `UN` for a dictionary-known tag (legal but sloppy).
    UnknownVr,
}

/// One validation finding.
#[derive(Clone, Debug)]
pub struct Finding {
    /// Severity.
    pub severity: Severity,
    /// Attribute tag.
    pub tag: Tag,
    /// Dictionary keyword (empty when unknown).
    pub keyword: String,
    /// Contributing module (empty when not from the IOD table).
    pub module: &'static str,
    /// Category.
    pub kind: Kind,
    /// Human-readable detail.
    pub message: String,
}

impl fmt::Display for Finding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "[{}] ({:04X},{:04X}) {} [{}]: {}",
            self.severity, self.tag.0, self.tag.1, self.keyword, self.module, self.message
        )
    }
}

/// Validation outcome for one dataset.
#[derive(Clone, Debug)]
pub struct Report {
    /// IOD that was checked.
    pub iod: &'static Iod,
    /// Findings in ascending tag order.
    pub findings: Vec<Finding>,
}

impl Report {
    /// True when there are no findings at all.
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.findings.is_empty()
    }

    /// True when there are no errors (warnings allowed).
    #[must_use]
    pub fn has_errors(&self) -> bool {
        self.findings.iter().any(|f| f.severity == Severity::Error)
    }
}

/// Validate a dataset against the IOD for `sop_class_uid`.
///
/// `ts` is the transfer syntax the dataset is (or will be) encoded with;
/// it drives the lossy-compression condition — pass `None` when unknown.
/// Returns `None` for unsupported SOP Classes.
#[must_use]
pub fn validate(ds: &DataSet, sop_class_uid: &str, ts: Option<TransferSyntax>) -> Option<Report> {
    let iod = find_iod(sop_class_uid)?;
    let mut findings = Vec::new();
    for attr in iod.attrs {
        check_attr(ds, attr, ts, &mut findings);
    }
    Some(Report { iod, findings })
}

fn check_attr(
    ds: &DataSet,
    attr: &IodAttr,
    ts: Option<TransferSyntax>,
    findings: &mut Vec<Finding>,
) {
    let entry = lookup(attr.tag);
    let keyword = entry.map_or(String::new(), |e| e.keyword.to_string());
    let el = ds.get(attr.tag);
    // Presence checks.
    match (el, attr.req) {
        (None, Req::T1) => {
            findings.push(Finding {
                severity: Severity::Error,
                tag: attr.tag,
                keyword,
                module: attr.module,
                kind: Kind::MissingType1,
                message: "missing Type 1 attribute".into(),
            });
            return;
        }
        (None, Req::T2) => {
            findings.push(Finding {
                severity: Severity::Error,
                tag: attr.tag,
                keyword,
                module: attr.module,
                kind: Kind::MissingType2,
                message: "missing Type 2 attribute".into(),
            });
            return;
        }
        (None, Req::Cond(rule)) if rule.required(ds, ts) => {
            findings.push(Finding {
                severity: Severity::Error,
                tag: attr.tag,
                keyword,
                module: attr.module,
                kind: Kind::CondRequired(rule),
                message: format!(
                    "missing conditionally-required attribute ({})",
                    rule.describe()
                ),
            });
            return;
        }
        (None, _) => return,
        (Some(_), _) => {}
    }
    // Present from here on.
    let el = ds.get(attr.tag).expect("checked present");
    if el.value.is_empty() {
        match attr.req {
            Req::T1 => findings.push(Finding {
                severity: Severity::Error,
                tag: attr.tag,
                keyword: keyword.clone(),
                module: attr.module,
                kind: Kind::EmptyType1,
                message: "empty Type 1 attribute".into(),
            }),
            Req::Cond(rule) if rule.required(ds, ts) => findings.push(Finding {
                severity: Severity::Error,
                tag: attr.tag,
                keyword: keyword.clone(),
                module: attr.module,
                kind: Kind::CondRequired(rule),
                message: format!(
                    "empty conditionally-required attribute ({})",
                    rule.describe()
                ),
            }),
            _ => {}
        }
    }
    let Some(entry) = entry else { return };
    // VR check.
    if el.vr != entry.vr && !entry.alts.contains(&el.vr) {
        if el.vr == Vr::Un {
            findings.push(Finding {
                severity: Severity::Warning,
                tag: attr.tag,
                keyword: keyword.clone(),
                module: attr.module,
                kind: Kind::UnknownVr,
                message: format!("explicit UN for known tag (dictionary VR is {})", entry.vr),
            });
        } else {
            let mut expected = vec![entry.vr];
            expected.extend_from_slice(entry.alts);
            findings.push(Finding {
                severity: Severity::Error,
                tag: attr.tag,
                keyword: keyword.clone(),
                module: attr.module,
                kind: Kind::VrMismatch {
                    expected,
                    found: el.vr,
                },
                message: format!("VR {} does not match dictionary VR {}", el.vr, entry.vr),
            });
        }
    }
    // VM check (countable values of non-empty elements only).
    if el.value.is_empty() {
        return;
    }
    let vm = match &el.value {
        Value::Bytes(_) | Value::Fragments(_) | Value::Sequence(_) => return,
        v => v.multiplicity(),
    };
    if let Some(range) = parse_vm(entry.vm)
        && !range.contains(vm as u32)
    {
        findings.push(Finding {
            severity: Severity::Error,
            tag: attr.tag,
            keyword,
            module: attr.module,
            kind: Kind::VmMismatch {
                expected: entry.vm.to_string(),
                found: vm,
            },
            message: format!("VM {vm} outside dictionary VM {}", entry.vm),
        });
    }
}

/// True when any text or person-name value holds non-ASCII characters,
/// recursing into sequences (Specific Character Set governs the whole
/// dataset, not just top-level elements).
fn has_non_ascii_text(ds: &DataSet) -> bool {
    ds.iter().any(|(_, el)| match &el.value {
        Value::Text(v) => v.iter().any(|s| !s.is_ascii()),
        Value::PersonName(v) => v.iter().any(|pn| {
            [&pn.alphabetic]
                .into_iter()
                .chain(pn.ideographic.as_ref())
                .chain(pn.phonetic.as_ref())
                .flat_map(|g| [&g.family, &g.given, &g.middle, &g.prefix, &g.suffix])
                .any(|s| !s.is_ascii())
        }),
        Value::Sequence(items) => items.iter().any(has_non_ascii_text),
        _ => false,
    })
}

/// Parsed dictionary VM string (e.g. `1`, `1-n`, `2-2n`, `1-8`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct VmRange {
    min: u32,
    max: Option<u32>,
    multiple: u32,
}

impl VmRange {
    fn contains(&self, vm: u32) -> bool {
        if vm < self.min {
            return false;
        }
        if let Some(max) = self.max
            && vm > max
        {
            return false;
        }
        vm.is_multiple_of(self.multiple)
    }
}

/// Parse a PS3.6 VM string; `None` when unrecognized (check skipped).
fn parse_vm(vm: &str) -> Option<VmRange> {
    fn num(s: &str) -> Option<u32> {
        s.strip_suffix('n').unwrap_or(s).parse().ok()
    }
    // "1-n or 1": union — the unbounded alternative governs.
    let vm = vm.split(" or ").next()?;
    let (lo, hi) = match vm.split_once('-') {
        None => (vm, None),
        Some((a, b)) => (a, Some(b)),
    };
    let min: u32 = lo.parse().ok()?;
    let (max, multiple) = match hi {
        None => (Some(min), 1),
        Some(h) if h.ends_with('n') => (None, num(h).unwrap_or(1).max(1)),
        Some(h) => (Some(h.parse().ok()?), 1),
    };
    Some(VmRange { min, max, multiple })
}

#[cfg(test)]
mod tests {
    use super::*;
    use vyna_dcm_core::DataElement;

    #[test]
    fn tables_are_dictionary_consistent() {
        assert_eq!(IODS.len(), 18);
        for iod in IODS {
            assert!(!iod.attrs.is_empty(), "{}", iod.uid);
            let mut tags: Vec<Tag> = iod.attrs.iter().map(|a| a.tag).collect();
            tags.sort();
            tags.dedup();
            assert_eq!(tags.len(), iod.attrs.len(), "dup tags in {}", iod.uid);
            assert!(
                iod.attrs.iter().any(|a| matches!(a.req, Req::T1)),
                "no T1 in {}",
                iod.uid
            );
            for attr in iod.attrs {
                let entry = lookup(attr.tag);
                assert!(entry.is_some(), "{:?} not in dictionary", attr.tag);
                assert!(parse_vm(entry.unwrap().vm).is_some(), "{:?}", attr.tag);
            }
        }
    }

    #[test]
    fn vm_parser_covers_dictionary_vocabulary() {
        for (vm, ok, bad) in [
            ("1", &[1][..], &[0, 2][..]),
            ("1-n", &[1, 7], &[0]),
            ("1-n or 1", &[1, 99], &[0]),
            ("2", &[2], &[1, 3]),
            ("2-2n", &[2, 4], &[1, 3]),
            ("2-4", &[2, 3, 4], &[1, 5]),
            ("3-3n", &[3, 6], &[4, 5]),
            ("1-8", &[1, 8], &[9]),
            ("4-5", &[4, 5], &[3, 6]),
            ("6-n", &[6, 60], &[5]),
        ] {
            let range = parse_vm(vm).unwrap();
            for v in ok {
                assert!(range.contains(*v), "{vm} should contain {v}");
            }
            for v in bad {
                assert!(!range.contains(*v), "{vm} should reject {v}");
            }
        }
        assert!(parse_vm("bogus").is_none());
    }

    /// Dummy value for a VR with at least `n` values.
    fn dummy_value(vr: Vr, n: usize) -> Value {
        let n = n.max(1);
        match vr {
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
            | Vr::Ui
            | Vr::Ur
            | Vr::Ut => Value::Text(vec!["1".to_string(); n]),
            Vr::Pn => Value::PersonName(
                std::iter::repeat_with(|| vyna_dcm_core::PersonName::parse("TEST^PATIENT"))
                    .take(n)
                    .collect(),
            ),
            Vr::At => Value::Tags(vec![Tag::new(0x0008, 0x0016); n]),
            Vr::Us => Value::U16(vec![1; n]),
            Vr::Ss => Value::I16(vec![1; n]),
            Vr::Ul => Value::U32(vec![1; n]),
            Vr::Uv => Value::U64(vec![1; n]),
            Vr::Sl => Value::I32(vec![1; n]),
            Vr::Sv => Value::I64(vec![1; n]),
            Vr::Fl => Value::F32(vec![1.0; n]),
            Vr::Fd => Value::F64(vec![1.0; n]),
            Vr::Sq => Value::Sequence(vec![DataSet::new(); n]),
            Vr::Ob | Vr::Ow | Vr::Od | Vr::Of | Vr::Ol | Vr::Ov | Vr::Un | Vr::Si | _ => {
                Value::Bytes(vec![0; n])
            }
        }
    }

    /// Build a dataset satisfying every enforced requirement of `iod`.
    fn valid_fixture(iod: &Iod) -> DataSet {
        let mut ds = DataSet::new();
        // Single-frame monochrome native instance: kills the multi-frame,
        // color, and lossy conditions; ASCII-only kills the charset one.
        for attr in iod.attrs {
            let entry = lookup(attr.tag).unwrap();
            let enforce = match attr.req {
                Req::T1 | Req::T2 => true,
                Req::IfPresent => false,
                Req::Cond(rule) => {
                    // Mirror the engine's conditions for this fixture shape.
                    !matches!(
                        rule,
                        CondRule::FrameIncrementPointer
                            | CondRule::PlanarConfiguration
                            | CondRule::LossyCompression
                            | CondRule::LossyRatio
                            | CondRule::SpecificCharacterSet
                            | CondRule::ExtOffsetTableLengths
                    )
                }
            };
            if !enforce {
                continue;
            }
            let count = parse_vm(entry.vm).map_or(1, |r| r.min.max(1) as usize);
            // Sequences take one empty item (items are out of scope).
            let value = dummy_value(entry.vr, count);
            ds.insert(DataElement::new(attr.tag, entry.vr, value));
        }
        ds
    }

    #[test]
    fn every_iod_accepts_a_valid_fixture() {
        for iod in IODS {
            let ds = valid_fixture(iod);
            let report = validate(&ds, iod.uid, Some(TransferSyntax::ExplicitVrLittleEndian))
                .unwrap_or_else(|| panic!("{} not found", iod.uid));
            assert!(
                report.is_clean(),
                "{}: {:?}",
                iod.uid,
                report
                    .findings
                    .iter()
                    .map(|f| f.to_string())
                    .collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn ct_negative_paths() {
        let uid = vyna_dcm_core::sop_class::CT_IMAGE;
        let iod = find_iod(uid).unwrap();
        let base = valid_fixture(iod);
        let check =
            |ds: &DataSet| validate(ds, uid, Some(TransferSyntax::ExplicitVrLittleEndian)).unwrap();
        // Missing Type 1.
        let mut ds = base.clone();
        ds.remove(tags::SOP_INSTANCE_UID);
        let report = check(&ds);
        let kinds: Vec<_> = report.findings.iter().map(|f| &f.kind).collect();
        assert!(kinds.contains(&&Kind::MissingType1), "{kinds:?}");
        // Empty Type 1.
        let mut ds = base.clone();
        ds.insert(DataElement::text(tags::SOP_INSTANCE_UID, Vr::Ui, ""));
        let report = check(&ds);
        let kinds: Vec<_> = report.findings.iter().map(|f| &f.kind).collect();
        assert!(kinds.contains(&&Kind::EmptyType1), "{kinds:?}");
        // Missing Type 2.
        let mut ds = base.clone();
        ds.remove(tags::PATIENT_NAME);
        let report = check(&ds);
        let kinds: Vec<_> = report.findings.iter().map(|f| &f.kind).collect();
        assert!(kinds.contains(&&Kind::MissingType2), "{kinds:?}");
        // Wrong VR.
        let mut ds = base.clone();
        ds.insert(DataElement::new(tags::ROWS, Vr::Ul, Value::U32(vec![64])));
        let report = check(&ds);
        let kinds: Vec<_> = report.findings.iter().map(|f| &f.kind).collect();
        assert!(
            kinds.iter().any(|k| matches!(k, Kind::VrMismatch { .. })),
            "{kinds:?}"
        );
        // Bad VM (Modality is VM 1).
        let mut ds = base.clone();
        ds.insert(DataElement::new(
            tags::MODALITY,
            Vr::Cs,
            Value::Text(vec!["CT".into(), "MR".into()]),
        ));
        let report = check(&ds);
        let kinds: Vec<_> = report.findings.iter().map(|f| &f.kind).collect();
        assert!(
            kinds.iter().any(|k| matches!(k, Kind::VmMismatch { .. })),
            "{kinds:?}"
        );
        // Explicit UN warns, not errors.
        let mut ds = base.clone();
        ds.insert(DataElement::new(
            tags::ROWS,
            Vr::Un,
            Value::Bytes(vec![64, 0]),
        ));
        let report = check(&ds);
        assert!(!report.has_errors());
        assert!(
            report.findings.iter().any(|f| f.kind == Kind::UnknownVr),
            "{:?}",
            report.findings
        );
    }

    #[test]
    fn ecg_and_pdf_negative_paths() {
        use vyna_dcm_core::sop_class;
        let native = Some(TransferSyntax::ExplicitVrLittleEndian);
        let kinds_for = |uid: &str, ds: &DataSet| {
            validate(ds, uid, native)
                .unwrap()
                .findings
                .iter()
                .map(|f| f.kind.clone())
                .collect::<Vec<_>>()
        };
        // Waveform Sequence is Type 1 in both ECG IODs.
        for uid in [
            sop_class::TWELVE_LEAD_ECG_WAVEFORM,
            sop_class::GENERAL_ECG_WAVEFORM,
        ] {
            let base = valid_fixture(find_iod(uid).unwrap());
            let mut ds = base.clone();
            ds.remove(tags::WAVEFORM_SEQUENCE);
            assert!(
                kinds_for(uid, &ds).contains(&Kind::MissingType1),
                "{uid} missing Waveform Sequence"
            );
            let mut ds = base.clone();
            ds.put_sequence(tags::WAVEFORM_SEQUENCE, Vec::new());
            assert!(
                kinds_for(uid, &ds).contains(&Kind::EmptyType1),
                "{uid} empty Waveform Sequence"
            );
        }
        // ECG Type 1 datetimes (Content Date/Time, Acquisition DateTime).
        let uid = sop_class::TWELVE_LEAD_ECG_WAVEFORM;
        let base = valid_fixture(find_iod(uid).unwrap());
        for (tag, vr) in [
            (tags::CONTENT_DATE, Vr::Da),
            (tags::CONTENT_TIME, Vr::Tm),
            (tags::ACQUISITION_DATE_TIME, Vr::Dt),
        ] {
            let mut ds = base.clone();
            ds.remove(tag);
            assert!(
                kinds_for(uid, &ds).contains(&Kind::MissingType1),
                "{tag} missing"
            );
            let mut ds = base.clone();
            ds.insert(DataElement::text(tag, vr, ""));
            assert!(
                kinds_for(uid, &ds).contains(&Kind::EmptyType1),
                "{tag} empty"
            );
        }
        // Encapsulated Document + MIME Type are Type 1 in the PDF IOD.
        let uid = sop_class::ENCAPSULATED_PDF;
        let base = valid_fixture(find_iod(uid).unwrap());
        let mut ds = base.clone();
        ds.remove(tags::ENCAPSULATED_DOCUMENT);
        assert!(kinds_for(uid, &ds).contains(&Kind::MissingType1));
        let mut ds = base.clone();
        ds.put_bytes(tags::ENCAPSULATED_DOCUMENT, Vr::Ob, Vec::new());
        assert!(kinds_for(uid, &ds).contains(&Kind::EmptyType1));
        let mut ds = base.clone();
        ds.remove(tags::MIME_TYPE_OF_ENCAPSULATED_DOCUMENT);
        assert!(kinds_for(uid, &ds).contains(&Kind::MissingType1));
        let mut ds = base.clone();
        ds.insert(DataElement::text(
            tags::MIME_TYPE_OF_ENCAPSULATED_DOCUMENT,
            Vr::Lo,
            "",
        ));
        assert!(kinds_for(uid, &ds).contains(&Kind::EmptyType1));
    }

    #[test]
    fn conditional_rules_fire() {
        use vyna_dcm_core::sop_class;
        let native = Some(TransferSyntax::ExplicitVrLittleEndian);
        let has = |report: &Report, rule: CondRule| {
            report
                .findings
                .iter()
                .any(|f| matches!(f.kind, Kind::CondRequired(r) if r == rule))
        };
        // Multi-frame US without Frame Increment Pointer.
        let iod = find_iod(sop_class::US_IMAGE).unwrap();
        let mut ds = valid_fixture(iod);
        ds.insert(DataElement::text(tags::NUMBER_OF_FRAMES, Vr::Is, "3"));
        let report = validate(&ds, sop_class::US_IMAGE, native).unwrap();
        assert!(
            has(&report, CondRule::FrameIncrementPointer),
            "{:?}",
            report.findings
        );
        // Color CT without Planar Configuration.
        let iod = find_iod(sop_class::CT_IMAGE).unwrap();
        let mut ds = valid_fixture(iod);
        ds.insert(DataElement::u16_single(tags::SAMPLES_PER_PIXEL, 3));
        let report = validate(&ds, sop_class::CT_IMAGE, native).unwrap();
        assert!(
            has(&report, CondRule::PlanarConfiguration),
            "{:?}",
            report.findings
        );
        // Lossy transfer syntax without the flag (US).
        let iod = find_iod(sop_class::US_IMAGE).unwrap();
        let mut ds = valid_fixture(iod);
        ds.remove(tags::LOSSY_IMAGE_COMPRESSION);
        let report = validate(
            &ds,
            sop_class::US_IMAGE,
            Some(TransferSyntax::JpegBaseline8Bit),
        )
        .unwrap();
        assert!(
            has(&report, CondRule::LossyCompression),
            "{:?}",
            report.findings
        );
        // Lossy flag "01" without the ratio (DX).
        let iod = find_iod(sop_class::DX_PRESENTATION).unwrap();
        let mut ds = valid_fixture(iod);
        ds.insert(DataElement::text(
            tags::LOSSY_IMAGE_COMPRESSION,
            Vr::Cs,
            "01",
        ));
        ds.remove(tags::LOSSY_IMAGE_COMPRESSION_RATIO);
        let report = validate(&ds, sop_class::DX_PRESENTATION, native).unwrap();
        assert!(has(&report, CondRule::LossyRatio), "{:?}", report.findings);
        // Non-ASCII text without Specific Character Set.
        let iod = find_iod(sop_class::CT_IMAGE).unwrap();
        let mut ds = valid_fixture(iod);
        ds.insert(DataElement::text(tags::PATIENT_NAME, Vr::Pn, "Müller"));
        let report = validate(&ds, sop_class::CT_IMAGE, native).unwrap();
        assert!(
            has(&report, CondRule::SpecificCharacterSet),
            "{:?}",
            report.findings
        );
        // Non-ASCII nested inside a sequence counts too.
        let iod = find_iod(sop_class::CT_IMAGE).unwrap();
        let mut ds = valid_fixture(iod);
        let mut item = DataSet::new();
        item.put_text(tags::SERIES_DESCRIPTION, Vr::Lo, "Tête");
        ds.put_sequence(tags::REFERENCED_SERIES_SEQUENCE, vec![item]);
        let report = validate(&ds, sop_class::CT_IMAGE, native).unwrap();
        assert!(
            has(&report, CondRule::SpecificCharacterSet),
            "{:?}",
            report.findings
        );
        // Extended Offset Table without its lengths.
        let iod = find_iod(sop_class::CT_IMAGE).unwrap();
        let mut ds = valid_fixture(iod);
        ds.insert(DataElement::new(
            tags::EXTENDED_OFFSET_TABLE,
            Vr::Ob,
            Value::Bytes(vec![0; 8]),
        ));
        let report = validate(&ds, sop_class::CT_IMAGE, native).unwrap();
        assert!(
            has(&report, CondRule::ExtOffsetTableLengths),
            "{:?}",
            report.findings
        );
        // Missing Pixel Data always fires.
        let iod = find_iod(sop_class::CT_IMAGE).unwrap();
        let mut ds = valid_fixture(iod);
        ds.remove(tags::PIXEL_DATA);
        let report = validate(&ds, sop_class::CT_IMAGE, native).unwrap();
        assert!(has(&report, CondRule::PixelData), "{:?}", report.findings);
    }

    #[test]
    fn unknown_sop_returns_none() {
        let ds = DataSet::new();
        assert!(validate(&ds, "1.2.3.4.5", None).is_none());
        // Trailing padding is tolerated.
        assert!(find_iod("1.2.840.10008.5.1.4.1.1.2\0").is_some());
    }
}
