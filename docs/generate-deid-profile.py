#!/usr/bin/env python3
"""Generate the de-identification Basic profile from PS3.15 Table E.1-1.

Reads the pinned E.1 snapshot (`docs/e1-basic-profile.json`: tag id, name,
Basic Profile action letter), resolves each tag's VR from
`dictionary_generated.rs` and its strictest IOD type from
`iod_generated.rs`, and emits
`crates/vyna-dcm-deid/src/profile_generated.rs`.

Letter-to-action mapping (strictest type across shipped IODs; absent or
IfPresent-only counts as Type 3):
  DA/DT: D anywhere -> ShiftDate; Z (no D) -> ShiftDateOrZero (T1: ShiftDate);
         pure X -> Remove (T3) / Zero (T2) / ShiftDate (T1, none exist).
  TM:    never shifted (a day offset is identity mod 24h): T1/D -> Dummy
         ("000000"), T2 -> Zero, T3 X -> Remove, T3 D -> Dummy, else Zero.
  UI:    U/D -> RemapUid; T1 -> RemapUid; T2 -> Zero; T3 X -> Remove else Zero.
  else (incl. SQ): T1 -> Dummy; T2 pure-D -> Dummy else Zero;
         T3 X -> Remove, T3 Z -> Zero, else Dummy.
Rationale notes: DA X/D keeps ShiftDate (shifting is E.1-conformant and
preserves longitudinal utility) while TM X/D prefers Remove (shifting is
meaningless); T3 slashes otherwise prefer X, then Z, then D/U.

Deliberate deviations from a literal transcription (all asserted below):
  U*_KEEP: Referenced/Source Image Sequence are E.1 X/Z/U*: the U* branch
         (keep the sequence, remap the UIDs inside via recursion) preserves
         referential integrity, so no row is emitted for either.
  RESPONSIBLE_ZERO: Responsible Person/Organization are E.1 X but Type 2
         conditional, so they are Zeroed (kept empty) instead of Removed.
  Curve Data (50xx-xxxx), Overlay Data/Comments (60xx-...), and private
         attributes are pseudo-rows covered by engine rules, not table rows.
  E.1-unlisted extras (markers pre-strip, order/filler numbers, unlisted
         identities) stay with their documented reasons; E.1-unlisted
         Patient Species/Breed rows and SR Text Value (0040,A160) are
         dropped (conditional-type validity / clinical content).

Usage:
    python3 docs/generate-deid-profile.py [--e1 PATH] [--out PATH]
The default input is the pinned snapshot above; to re-pin it, fetch Table
E.1-1 from the NEMA PS3.15 "current" chtml and store {id, name,
basicProfile} rows (see the snapshot header for the source URL).
"""

import argparse
import json
import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

# E.1 X/Z/U* sequences kept for referential integrity (UIDs inside are
# remapped by engine recursion); no row is emitted for these.
U_STAR_KEEP = {"00081140", "00082112"}

# E.1 X but Type 2 conditional: Zero instead of Remove.
RESPONSIBLE_ZERO = {"00102297", "00102299"}

# Pseudo-rows covered by engine rules (curve-group removal, overlay-plane
# removal, private-group removal), never by table rows.
PSEUDO_ROWS = {
    "50XXXXXX",
    "60XX3000",
    "60XX4000",
    "GGGGEEEE-WHERE-GGGG-IS-ODD",
}

# E.1-unlisted rows kept with (const, action, reason). Deliberately NOT here:
# Patient Species/Breed (1C/2C validity) and SR Text Value (0040,A160)
# (clinical content) are dropped entirely.
EXTRAS = [
    ("COMMENTS_ON_THE_SCHEDULED_PROCEDURE_STEP", "Zero",
     "unlisted operator free text"),
    ("DEIDENTIFICATION_METHOD", "Remove",
     "pre-existing marker stripped; engine re-stamps"),
    ("DEIDENTIFICATION_METHOD_CODE_SEQUENCE", "Remove",
     "pre-existing marker stripped; engine re-stamps"),
    ("DEVICE_ID", "Zero", "unlisted device identity"),
    ("FILLER_ORDER_NUMBER_PROCEDURE", "Zero", "unlisted order identity"),
    ("HL_7_INSTANCE_IDENTIFIER", "Zero", "unlisted HL7 linkage"),
    ("HUMAN_PERFORMER_CODE_SEQUENCE", "Zero", "unlisted performer identity"),
    ("ISSUER_OF_ACCESSION_NUMBER_SEQUENCE", "Zero", "unlisted issuer identity"),
    ("LOCATION", "Zero", "unlisted location identity"),
    ("PATIENT_BIRTH_DATE_IN_ALTERNATIVE_CALENDAR", "Zero",
     "unlisted birth-date equivalent"),
    ("PATIENT_BODY_MASS_INDEX", "Zero", "unlisted patient characteristic"),
    ("PATIENT_CLINICAL_TRIAL_PARTICIPATION_SEQUENCE", "Remove",
     "unlisted trial linkage"),
    ("PATIENT_IDENTITY_REMOVED", "Remove",
     "pre-existing marker stripped; engine re-stamps"),
    ("PERTINENT_DOCUMENTS_SEQUENCE", "Zero", "unlisted linkage"),
    ("PERTINENT_RESOURCES_SEQUENCE", "Zero", "unlisted linkage"),
    ("PLACER_ORDER_NUMBER_PROCEDURE", "Zero", "unlisted order identity"),
    ("ROUTE_OF_ADMISSIONS", "Zero", "unlisted admission data"),
    ("SECONDARY_CAPTURE_DEVICE_ID", "Remove", "unlisted device identity"),
    ("TRACKING_ID", "Dummy", "unlisted tracking identity"),
    ("UNFORMATTED_TEXT_VALUE", "Zero", "unlisted presentation text"),
    ("VISIT_STATUS_ID", "Zero", "unlisted visit identity"),
]


def load_dict():
    """tag id -> (VR, const name). Hand-written consts win on conflict."""
    gen = (ROOT / "crates/vyna-dcm-core/src/dictionary_generated.rs").read_text()
    vr = {}
    for g, e, v in re.findall(
        r"e\(\s*0x([0-9A-Fa-f]{4}),\s*0x([0-9A-Fa-f]{4}),\s*Vr::(\w+),", gen
    ):
        vr[f"{g.upper()}{e.upper()}"] = v
    const = {}
    for g, e, name in re.findall(
        r"/// Tag \(([0-9A-F]{4}),([0-9A-F]{4})\):.*?\n    pub const (\w+): Tag", gen
    ):
        const[f"{g}{e}"] = name
    hand = (ROOT / "crates/vyna-dcm-core/src/dictionary.rs").read_text()
    for name, g, e in re.findall(
        r"([A-Z0-9_]+) *= *\(0x([0-9A-Fa-f]{4}), *0x([0-9A-Fa-f]{4})\)", hand
    ):
        const[f"{g.upper()}{e.upper()}"] = name
    return vr, const


def load_types():
    """tag id -> strictest Req name across shipped IOD tables."""
    gen = (ROOT / "crates/vyna-dcm-iod/src/iod_generated.rs").read_text()
    rows = re.findall(
        r"tag: Tag::new\((0x[0-9A-Fa-f]+), *(0x[0-9A-Fa-f]+)\),\s*req: Req::(\w+)",
        gen,
    )
    rank = {"T1": 3, "T2": 2, "Cond": 1, "IfPresent": 0}
    tmap = {}
    for g, e, r in rows:
        tid = f"{int(g, 16):04X}{int(e, 16):04X}"
        if tid not in tmap or rank[r] > rank[tmap[tid]]:
            tmap[tid] = r
    return tmap


def map_action(vr, letter, typ):
    vr = vr.upper()
    t1 = typ == "T1"
    t2 = typ == "T2"
    t3 = not t1 and not t2
    ls = set(letter.split("/"))
    has_u = "U" in ls or "U*" in ls
    if vr in ("DA", "DT"):
        if "D" in ls:
            return "ShiftDate"
        if "Z" in ls:
            return "ShiftDate" if t1 else "ShiftDateOrZero"
        return "Remove" if t3 else ("Zero" if t2 else "ShiftDate")
    if vr == "TM":
        if t1 or (t3 and "D" in ls and "X" not in ls):
            return "Dummy"
        if t2:
            return "Zero"
        if t3:
            return "Remove" if "X" in ls else ("Dummy" if "D" in ls else "Zero")
        return "Dummy"
    if vr == "UI":
        if has_u or "D" in ls:
            return "RemapUid"
        if t1:
            return "RemapUid"
        if t2:
            return "Zero"
        return "Remove" if "X" in ls else "Zero"
    if t1:
        return "Dummy"
    if t2:
        if "D" in ls and "X" not in ls and "Z" not in ls:
            return "Dummy"
        return "Zero"
    if "X" in ls:
        return "Remove"
    if "Z" in ls:
        return "Zero"
    return "Dummy"


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--e1", default=str(ROOT / "docs/e1-basic-profile.json"))
    ap.add_argument(
        "--out",
        default=str(ROOT / "crates/vyna-dcm-deid/src/profile_generated.rs"),
    )
    args = ap.parse_args()

    snap = json.loads(Path(args.e1).read_text())
    e1rows = snap["rows"]
    vr_of, const_of = load_dict()
    types = load_types()

    emitted = {}  # tid -> (const, action, comment)
    omitted_u_star = []
    rule_covered = []
    for e in e1rows:
        tid = e["id"].upper()
        letter = e["basicProfile"]
        if tid not in vr_of or tid not in const_of:
            if tid in PSEUDO_ROWS:
                rule_covered.append(tid)
                continue
            sys.exit(f"error: E.1 row {tid} ({e['name']}) has no dictionary entry")
        if tid in U_STAR_KEEP:
            assert "U*" in letter, f"U* row {tid} lost its U* branch: {letter}"
            omitted_u_star.append(tid)
            continue
        typ = types.get(tid, "absent")
        action = map_action(vr_of[tid], letter, typ)
        shown = {"T1": "T1", "T2": "T2"}.get(typ, "T3")
        comment = f"E.1:{letter} {shown}"
        if tid in RESPONSIBLE_ZERO:
            assert letter == "X", f"responsible row {tid} no longer E.1 X: {letter}"
            action = "Zero"
            comment += " (Zero: Type 2 conditional)"
        emitted[tid] = (const_of[tid], action, comment)
    assert sorted(omitted_u_star) == sorted(U_STAR_KEEP), "U* set drifted"
    assert sorted(rule_covered) == sorted(PSEUDO_ROWS), (
        f"pseudo-row set drifted: {sorted(rule_covered)}"
    )

    for const, action, reason in EXTRAS:
        tid = next((t for t, c in const_of.items() if c == const), None)
        if tid is None:
            sys.exit(f"error: extra const tags::{const} not found")
        if tid in emitted:
            sys.exit(f"error: extra tags::{const} is now E.1-listed; drop it")
        typ = types.get(tid, "absent")
        shown = {"T1": "T1", "T2": "T2"}.get(typ, "T3")
        emitted[tid] = (const, action, f"unlisted ({shown}): {reason}")

    # Type discipline: Type 1 targets must keep a non-empty value.
    for tid, (const, action, _) in emitted.items():
        if types.get(tid) == "T1" and action not in (
            "ShiftDate",
            "ShiftDateOrZero",
            "Dummy",
            "RemapUid",
        ):
            sys.exit(f"error: T1 row {tid} tags::{const} maps to {action}")
        if action == "ShiftDateOrZero" and types.get(tid) == "T1":
            sys.exit(f"error: T1 row {tid} tags::{const} may empty")

    from collections import Counter

    counts = Counter(a for _, a, _ in emitted.values())
    # The pinned source carries a trailing "(Table E.1-1 ...)" note;
    # angle-wrap only the URL so rustdoc sees a valid autolink.
    src_url, _, src_note = snap["source"].partition(" ")
    lines = [
        "//! GENERATED by `docs/generate-deid-profile.py` — do not hand-edit.",
        f"//! E.1 source: <{src_url}>{(' ' + src_note) if src_note else ''}",
        f"//! E.1 edition: {snap['edition']} ({len(e1rows)} rows).",
        "//!",
        "//! Transcription of the PS3.15 Table E.1-1 Basic Profile column:",
        "//! every listed tag with a dictionary entry has a row, except the",
        "//! two X/Z/U* image-reference sequences (kept with remapped UIDs",
        "//! for referential integrity) and the four pseudo-rows covered by",
        "//! engine rules (curve groups, overlay planes, private groups).",
        "//! Slash letters resolve by strictest shipped-IOD type (see the",
        "//! generator); per-row comments cite the E.1 letter and type.",
        "",
        "use vyna_dcm_core::{Tag, tags};",
        "",
        "use crate::profile::Action;",
        "",
        "/// Basic profile: `(tag, action)` pairs transcribed from PS3.15 E.1.",
        "///",
        "/// Unlisted tags are kept (notably clinical content, anatomy, and",
        "/// acquisition technique), sequences are recursed into, private",
        "/// groups, overlay planes, and curve-data groups are removed by",
        "/// rule (see the engine), and `Specific Character Set` conversion",
        "/// is out of scope.",
        "///",
        "/// Type discipline: [`Action::Dummy`], [`Action::RemapUid`],",
        "/// [`Action::ShiftDate`], and [`Action::ShiftDateOrZero`] with an",
        "/// offset are the only non-empty outcomes touching attributes",
        "/// that can be Type 1; every [`Action::Zero`] target is Type 2/3",
        "/// and every [`Action::Remove`] target is Type 3, a module",
        "/// removal, provenance, or a signature. The generator asserts this",
        "/// on every run.",
        "pub const BASIC_PROFILE: &[(Tag, Action)] = &[",
    ]
    for tid in sorted(emitted):
        const, action, comment = emitted[tid]
        lines.append(f"    (tags::{const}, Action::{action}), // {comment}")
    lines.append("];")
    lines.append("")
    out = Path(args.out)
    out.write_text("\n".join(lines))
    # rustfmt aligns the trailing E.1 citations; run it so regen output
    # is format-stable without a manual pass.
    try:
        subprocess.run(
            ["rustfmt", "--edition", "2024", str(out)], check=True, capture_output=True
        )
    except (OSError, subprocess.CalledProcessError):
        print("warning: rustfmt unavailable; run `cargo fmt` manually", file=sys.stderr)
    total = len(emitted)
    by_action = ", ".join(f"{counts[a]} {a}" for a in sorted(counts))
    print(f"wrote {args.out}: {total} rows ({by_action})")
    # Self-contained E.1 fixture for the coverage test (same source, so
    # the table and the fixture cannot drift; lives in-crate so sdist
    # tests never reach outside the package).
    fixture = ROOT / "crates/vyna-dcm-deid/tests/fixtures/e1-basic-profile.json"
    fixture.parent.mkdir(parents=True, exist_ok=True)
    fixture.write_text(
        json.dumps(
            {"source": snap["source"], "edition": snap["edition"], "rows": e1rows},
            indent=1,
        )
        + "\n"
    )
    print(f"wrote {fixture}: {len(e1rows)} E.1 rows")


if __name__ == "__main__":
    main()
