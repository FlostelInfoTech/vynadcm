//! E.1 transcription coverage: every pinned Table E.1-1 row with a
//! dictionary entry must have a [`BASIC_PROFILE`] row, except the
//! deliberate omissions below (which are asserted, not silently
//! skipped). The fixture is emitted by `docs/generate-deid-profile.py`
//! from the same snapshot as the table, so the two cannot drift.

use std::collections::HashSet;

use vyna_dcm_core::{Tag, lookup};
use vyna_dcm_deid::BASIC_PROFILE;

/// E.1 pseudo-rows covered by engine rules, not table rows: Curve Data
/// (curve-group rule), Overlay Data/Comments (overlay-plane rule), and
/// private attributes (private-group rule).
const PSEUDO_IDS: &[&str] = &[
    "50XXXXXX",
    "60XX3000",
    "60XX4000",
    "GGGGEEEE-WHERE-GGGG-IS-ODD",
];

/// E.1 X/Z/U* image-reference sequences: the U* branch (keep the
/// sequence, remap the UIDs inside) preserves referential integrity.
const U_STAR_IDS: &[&str] = &["00081140", "00082112"];

/// E.1-unlisted extras documented in the generator.
const EXTRAS: usize = 21;

const FIXTURE: &str = include_str!("fixtures/e1-basic-profile.json");

#[test]
fn basic_profile_covers_e1() {
    let fixture: serde_json::Value = serde_json::from_str(FIXTURE).unwrap();
    let rows = fixture["rows"].as_array().unwrap();
    assert!(!rows.is_empty(), "empty E.1 fixture");

    let table: HashSet<Tag> = BASIC_PROFILE.iter().map(|(tag, _)| *tag).collect();
    // No duplicate rows.
    assert_eq!(table.len(), BASIC_PROFILE.len(), "duplicate table rows");

    let mut covered = 0;
    for row in rows {
        let id = row["id"].as_str().unwrap().to_uppercase();
        if PSEUDO_IDS.contains(&id.as_str()) {
            continue;
        }
        if U_STAR_IDS.contains(&id.as_str()) {
            let tag = tag_of(&id);
            assert!(
                !table.contains(&tag),
                "{id} must stay unlisted (U* keep-and-remap)"
            );
            assert!(lookup(tag).is_some(), "{id} lost its dictionary entry");
            continue;
        }
        let tag = tag_of(&id);
        assert!(lookup(tag).is_some(), "{id} has no dictionary entry");
        assert!(table.contains(&tag), "E.1 row {id} has no profile row");
        covered += 1;
    }

    // Table = covered E.1 rows + documented extras, nothing else.
    assert_eq!(
        BASIC_PROFILE.len(),
        covered + EXTRAS,
        "table/extras count drifted"
    );
}

fn tag_of(id: &str) -> Tag {
    assert_eq!(id.len(), 8, "non-tag E.1 id: {id}");
    let group = u16::from_str_radix(&id[0..4], 16).unwrap();
    let elem = u16::from_str_radix(&id[4..8], 16).unwrap();
    Tag::new(group, elem)
}
