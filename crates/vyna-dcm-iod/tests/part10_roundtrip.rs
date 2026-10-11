//! Part-10 round-trip: fixtures survive real wire encode/decode and validate.
//!
//! This exercises the interaction between wire decoding (IS text values,
//! UI padding, implicit/explicit VR resolution) and the IOD engine, using
//! only public API.

use vyna_dcm_codec::{FileMeta, read_part10, write_part10};
use vyna_dcm_core::{
    DataElement, DataSet, Tag, TransferSyntax, Value, Vr, lookup, sop_class, tags,
};
use vyna_dcm_iod::{IODS, validate};

fn dummy(vr: Vr, n: usize) -> Value {
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
        | Vr::Ur
        | Vr::Ut => Value::Text(vec!["1".to_string(); n]),
        Vr::Ui => Value::Text(vec!["1.2.3.4".to_string(); n]),
        Vr::Pn => Value::PersonName(vec![vyna_dcm_core::PersonName::parse("TEST^PAT"); n]),
        Vr::At => Value::Tags(vec![Tag::new(0x0008, 0x0016); n]),
        Vr::Us => Value::U16(vec![1; n]),
        Vr::Ss => Value::I16(vec![1; n]),
        Vr::Ul => Value::U32(vec![1; n]),
        Vr::Uv => Value::U64(vec![1; n]),
        Vr::Sl => Value::I32(vec![1; n]),
        Vr::Sv => Value::I64(vec![1; n]),
        Vr::Fl => Value::F32(vec![1.0; n]),
        Vr::Fd => Value::F64(vec![1.0; n]),
        Vr::Ob | Vr::Ow | Vr::Od | Vr::Of | Vr::Ol | Vr::Ov | Vr::Un | Vr::Si => {
            Value::Bytes(vec![0, 0])
        }
        Vr::Sq => Value::Sequence(vec![DataSet::new()]),
    }
}

/// Minimum multiplicity of a dictionary VM string (test-side only; the
/// engine owns the real parser).
fn vm_min(vm: &str) -> usize {
    vm.split(" or ")
        .next()
        .and_then(|s| s.split('-').next())
        .and_then(|s| s.parse().ok())
        .unwrap_or(1)
}

fn ct_fixture() -> DataSet {
    let iod = IODS.iter().find(|i| i.uid == sop_class::CT_IMAGE).unwrap();
    let mut ds = DataSet::new();
    for attr in iod.attrs {
        // Enforced requirements only; single-frame mono native shape.
        let enforce = matches!(attr.req, vyna_dcm_iod::Req::T1 | vyna_dcm_iod::Req::T2)
            || matches!(
                attr.req,
                vyna_dcm_iod::Req::Cond(vyna_dcm_iod::CondRule::PixelData)
            );
        if !enforce {
            continue;
        }
        let entry = lookup(attr.tag).unwrap();
        ds.insert(DataElement::new(
            attr.tag,
            entry.vr,
            dummy(entry.vr, vm_min(entry.vm)),
        ));
    }
    // Realistic geometry (IS text, US) for the condition rules.
    ds.insert(DataElement::text(tags::NUMBER_OF_FRAMES, Vr::Is, "1"));
    ds.insert(DataElement::u16_single(tags::SAMPLES_PER_PIXEL, 1));
    ds.insert(DataElement::new(
        tags::SOP_CLASS_UID,
        Vr::Ui,
        Value::Text(vec![sop_class::CT_IMAGE.to_string()]),
    ));
    ds.insert(DataElement::new(
        tags::SOP_INSTANCE_UID,
        Vr::Ui,
        Value::Text(vec!["1.2.3.4.5.6".to_string()]),
    ));
    ds
}

#[test]
fn part10_ct_roundtrip_validates_clean() {
    let ds = ct_fixture();
    let meta = FileMeta::new(
        sop_class::CT_IMAGE,
        "1.2.3.4.5.6",
        TransferSyntax::ExplicitVrLittleEndian,
    );
    let bytes = write_part10(&meta, &ds).unwrap();
    let (meta2, ds2) = read_part10(&bytes).unwrap();
    assert_eq!(meta2.transfer_syntax_uid, "1.2.840.10008.1.2.1");
    let ts = TransferSyntax::from_uid(&meta2.transfer_syntax_uid);
    let report = validate(&ds2, sop_class::CT_IMAGE, ts).unwrap();
    assert!(
        report.is_clean(),
        "{:?}",
        report
            .findings
            .iter()
            .map(|f| f.to_string())
            .collect::<Vec<_>>()
    );
}

#[test]
fn part10_broken_ct_reports_errors() {
    let mut ds = ct_fixture();
    ds.remove(tags::SERIES_INSTANCE_UID); // missing Type 1
    ds.insert(DataElement::text(tags::MODALITY, Vr::Cs, "")); // empty Type 1
    let meta = FileMeta::new(
        sop_class::CT_IMAGE,
        "1.2.3.4.5.6",
        TransferSyntax::ExplicitVrLittleEndian,
    );
    let bytes = write_part10(&meta, &ds).unwrap();
    let (_, ds2) = read_part10(&bytes).unwrap();
    let report = validate(
        &ds2,
        sop_class::CT_IMAGE,
        Some(TransferSyntax::ExplicitVrLittleEndian),
    )
    .unwrap();
    assert!(report.has_errors());
    let tags_found: Vec<Tag> = report.findings.iter().map(|f| f.tag).collect();
    assert!(tags_found.contains(&tags::SERIES_INSTANCE_UID));
    assert!(tags_found.contains(&tags::MODALITY));
}
