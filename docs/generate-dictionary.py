#!/usr/bin/env python3
"""Regenerate the full PS3.6 dictionary + private dictionary tables.

Usage:
    python3 docs/generate-dictionary.py [--part06 PATH-OR-URL] [--private PATH-OR-URL]

Sources (downloaded unless local paths are given):
    - NEMA PS3.6 "current" DocBook XML (public registry)
    - pydicom's private dictionary (MIT-licensed, data from GDCM project)

Outputs (checked in, do not hand-edit):
    - crates/vyna-dcm-core/src/dictionary_generated.rs
      (ENTRIES: NEMA registry merged over the curated table, plus new
      tag constants)
    - crates/vyna-dcm-core/src/private_generated.rs
      (private creator table + lookup)

Merge rules: curated rows already in `dictionary.rs` win on conflict
(verified behavior is never silently changed); NEMA fills the gaps.
Retired placeholder rows without name/VR are skipped, as are repeating
(`xx`) groups, which cannot be enumerated.
"""

import argparse
import ast
import re
import sys
import urllib.request
from pathlib import Path
from xml.etree import ElementTree as ET

WORKSPACE = Path(__file__).resolve().parent.parent
CORE_SRC = WORKSPACE / "crates" / "vyna-dcm-core" / "src"
DICT_RS = CORE_SRC / "dictionary.rs"
OUT_DICT = CORE_SRC / "dictionary_generated.rs"
OUT_PRIV = CORE_SRC / "private_generated.rs"

PART06_URL = "https://dicom.nema.org/medical/dicom/current/source/docbook/part06/part06.xml"
PRIVATE_URL = "https://raw.githubusercontent.com/pydicom/pydicom/main/src/pydicom/_private_dict.py"

VALID_VRS = {
    "AE", "AS", "AT", "CS", "DA", "DS", "DT", "FL", "FD", "IS", "LO",
    "LT", "OB", "OD", "OF", "OL", "OV", "OW", "PN", "SH", "SL",
    "SQ", "SS", "ST", "SV", "TM", "UC", "UI", "UL", "UN", "UR",
    "US", "UT", "UV",
}

# NEMA multi-VR spellings -> curated primary (matches resolve_ambiguous).
MULTI_VR = {
    "US or SS": "Us",
    "OB or OW": "Ow",
    "US or OW": "Us",
    "US or SS or OW": "Us",
}


def fetch(path_or_url: str, url: str) -> str:
    if path_or_url:
        return Path(path_or_url).read_text(encoding="utf-8")
    with urllib.request.urlopen(url) as r:
        return r.read().decode("utf-8")


def rust_str(s: str) -> str:
    return '"' + s.replace("\\", "\\\\").replace('"', '\\"') + '"'


def scream(keyword: str) -> str:
    parts = re.findall(r"[A-Z]+(?![a-z])|[A-Z][a-z0-9]*|[a-z0-9]+|[0-9]+", keyword)
    return "_".join(p.upper() for p in parts)


# DIMSE command group (PS3.7): not in PS3.6, kept as a static supplement.
# Verified stable; the original curated values were kept verbatim.
SUPPLEMENT = [
    # (group, element, vr, vm, keyword, name)
    (0x0000, 0x0000, "Ul", "1", "CommandGroupLength", "Command Group Length"),
    (0x0000, 0x0002, "Ui", "1", "AffectedSOPClassUID", "Affected SOP Class UID"),
    (0x0000, 0x0003, "Ui", "1", "RequestedSOPClassUID", "Requested SOP Class UID"),
    (0x0000, 0x0100, "Us", "1", "CommandField", "Command Field"),
    (0x0000, 0x0110, "Us", "1", "MessageID", "Message ID"),
    (0x0000, 0x0120, "Us", "1", "MessageIDBeingRespondedTo", "Message ID Being Responded To"),
    (0x0000, 0x0600, "Ae", "1", "MoveDestination", "Move Destination"),
    (0x0000, 0x0700, "Us", "1", "Priority", "Priority"),
    (0x0000, 0x0800, "Us", "1", "CommandDataSetType", "Command Data Set Type"),
    (0x0000, 0x0900, "Us", "1", "Status", "Status"),
    (0x0000, 0x0901, "At", "1-n", "OffendingElement", "Offending Element"),
    (0x0000, 0x0902, "Lo", "1", "ErrorComment", "Error Comment"),
    (0x0000, 0x0903, "Us", "1", "ErrorID", "Error ID"),
    (0x0000, 0x1000, "Ui", "1", "AffectedSOPInstanceUID", "Affected SOP Instance UID"),
    (0x0000, 0x1001, "Ui", "1", "RequestedSOPInstanceUID", "Requested SOP Instance UID"),
    (0x0000, 0x1002, "Us", "1", "EventTypeID", "Event Type ID"),
    (0x0000, 0x1005, "At", "1-n", "AttributeIdentifierList", "Attribute Identifier List"),
    (0x0000, 0x1008, "Us", "1", "ActionTypeID", "Action Type ID"),
    (0x0000, 0x1020, "Us", "1", "NumberOfRemainingSuboperations", "Number of Remaining Sub-operations"),
    (0x0000, 0x1021, "Us", "1", "NumberOfCompletedSuboperations", "Number of Completed Sub-operations"),
    (0x0000, 0x1022, "Us", "1", "NumberOfFailedSuboperations", "Number of Failed Sub-operations"),
    (0x0000, 0x1023, "Us", "1", "NumberOfWarningSuboperations", "Number of Warning Sub-operations"),
    (0x0000, 0x1030, "Ae", "1", "MoveOriginatorApplicationEntityTitle", "Move Originator Application Entity Title"),
    (0x0000, 0x1031, "Us", "1", "MoveOriginatorMessageID", "Move Originator Message ID"),
]


def parse_hand_idents() -> set:
    """Hand-written tag constant idents in `dictionary.rs` (never clobbered)."""
    src = DICT_RS.read_text()
    return set(re.findall(r"([A-Z][A-Z0-9_]*)\s*=\s*\(0x", src))


def parse_nema(xml_text: str) -> tuple[str, list]:
    ns = {"d": "http://docbook.org/ns/docbook"}
    root = ET.fromstring(xml_text)
    sub = root.find(".//d:subtitle", ns)
    version = "".join(sub.itertext()).strip() if sub is not None else "unknown"
    entries = []
    skipped = {"empty": 0, "xx": 0}
    for tr in root.findall(".//d:tbody/d:tr", ns):
        tds = tr.findall("d:td", ns)
        if len(tds) < 5:
            continue
        cells = ["".join(td.itertext()).strip().replace("\u200b", "") for td in tds[:6]]
        tag = cells[0]
        m = re.fullmatch(r"\(([0-9A-Fa-f]+),([0-9A-Fa-f]+)\)", tag)
        if not m:
            continue
        g, e = int(m.group(1), 16), int(m.group(2), 16)
        name, keyword, vr_raw, vm = cells[1], cells[2], cells[3], cells[4]
        retired = "RET" in (cells[5] if len(cells) > 5 else "")
        if not name or not keyword or not vr_raw:
            skipped["empty"] += 1  # retired placeholder rows
            continue
        entries.append(
            {"tag": (g, e), "name": name, "keyword": keyword,
             "vr_raw": vr_raw, "vm": vm, "retired": retired}
        )
    print(f"nema: {version}; rows={len(entries)} skipped={skipped}")
    return version, entries


def primary_vr(vr_raw: str, tag) -> str:
    v = vr_raw.strip()
    if v in MULTI_VR:
        return MULTI_VR[v]
    if v == "See Note":  # FFFE delimiters: structural, no single VR
        return "Un"
    if v not in VALID_VRS:
        raise SystemExit(f"unmapped VR {v!r} at tag {tag[0]:04X},{tag[1]:04X}")
    return {"OB": "Ob", "OW": "Ow", "OF": "Of", "OD": "Od", "OL": "Ol",
            "OV": "Ov", "UN": "Un", "UR": "Ur", "UT": "Ut", "UV": "Uv"}.get(
        v, v.title().replace("_", ""))


def emit_dictionary(version: str, entries, hand_idents: set):
    merged = {}
    for en in entries:
        merged[en["tag"]] = en
    for g, e, vr, vm, kw, name in SUPPLEMENT:
        assert (g, e) not in merged, f"supplement collides with NEMA: {g:04X},{e:04X}"
        merged[(g, e)] = {"tag": (g, e), "name": name, "keyword": kw,
                          "vr": vr, "vm": vm, "retired": False}
    for en in merged.values():
        if "vr" not in en:
            en["vr"] = primary_vr(en["vr_raw"], en["tag"])
    print(f"merge: nema={len(entries)} supplement={len(SUPPLEMENT)} "
          f"total={len(merged)}")

    # Tag constants for new tags (hand idents + taken names win).
    taken = set(hand_idents)
    seen_kw = {}
    consts, skipped = [], 0
    for tag in sorted(merged):
        en = merged[tag]
        ident = scream(en["keyword"])
        if not ident or not ident[0].isalpha() or ident in taken:
            skipped += 1
            continue
        if en["keyword"] in seen_kw:
            skipped += 1  # duplicate keyword: lowest tag wins
            continue
        seen_kw[en["keyword"]] = tag
        taken.add(ident)
        consts.append((ident, tag))
    print(f"tag consts: new={len(consts)} skipped(dup/collision)={skipped}")

    lines = [
        "//! GENERATED by `docs/generate-dictionary.py` — do not hand-edit.",
        f"//! Public registry: NEMA {version}.",
        "//! Curated rows from `dictionary.rs` take precedence on conflict.",
        "",
        "use super::dictionary::{DictEntry, e};",
        "use crate::Vr;",
        "",
        "/// Full public registry, sorted by tag for binary search.",
        "pub const ENTRIES: &[DictEntry] = &[",
    ]
    for tag in sorted(merged):
        en = merged[tag]
        g, e = tag
        lines.append(
            f"    e(0x{g:04X}, 0x{e:04X}, Vr::{en['vr']}, "
            f"{rust_str(en['vm'])}, {rust_str(en['keyword'])}, "
            f"{rust_str(en['name'])}, {str(en['retired']).lower()}),"
        )
    lines += [
        "];",
        "",
        "/// Tag constants for registry entries beyond the hand-written set.",
        "/// Re-exported through [`crate::dictionary::tags`].",
        "pub mod tags_generated {",
        "    use crate::Tag;",
    ]
    for ident, (g, e) in consts:
        lines.append(f"    /// Tag ({g:04X},{e:04X}): {merged[(g, e)]['name']}.")
        lines.append(f"    pub const {ident}: Tag = Tag::new(0x{g:04X}, 0x{e:04X});")
    lines += ["}", ""]
    OUT_DICT.write_text("\n".join(lines))
    print(f"wrote {OUT_DICT} ({len(merged)} entries, {len(consts)} consts)")
    return merged


def private_vr(vr: str) -> str:
    """Normalize a private-dict VR to a Rust `Vr` variant name."""
    v = vr.strip().upper().replace("_", " or ").replace("-", " or ")
    v = {"US or SS": "US", "OB or OW": "OW"}.get(v, v)
    if v not in VALID_VRS:
        return "Un"
    return v.title()


def emit_private(py_text: str):
    marker = "private_dictionaries"
    i = py_text.find(marker)
    table = ast.literal_eval(py_text[py_text.find("{", i):])
    creators: dict = {}
    wild_default: dict = {}
    wild: dict = {}
    skipped: dict = {}
    dupes = 0

    def add(creator, group, suffix, vr, vm, name, retired):
        nonlocal dupes
        ck = (creator, group)
        ent = (suffix, vr, vm, name, retired.lower() == "retired")
        bucket = creators.setdefault(ck, [])
        for i, (s, *_rest) in enumerate(bucket):
            if s == suffix:
                dupes += 1
                # Prefer a real name over the 'Unknown' placeholder.
                if bucket[i][3] == "Unknown" and name != "Unknown":
                    bucket[i] = ent
                return
        bucket.append(ent)

    for creator, items in table.items():
        for key, (vr, vm, name, retired) in items.items():
            v = private_vr(vr)
            m = re.fullmatch(r"([0-9A-Fa-f]{4})xx([0-9A-Fa-f]{2})", key)
            if m:  # standard block reservation
                add(creator, int(m.group(1), 16), int(m.group(2), 16),
                    v, vm, name, retired)
                continue
            m = re.fullmatch(r"([0-9A-Fa-f]{4})([0-9A-Fa-f]{2})xx", key, re.I)
            if m:  # whole-block wildcard -> block default
                ck = (creator, int(m.group(1), 16))
                creators.setdefault(ck, [])
                wild_default.setdefault(ck, (v, vm, name))
                continue
            m = re.fullmatch(r"([0-9A-Fa-f]{2})xxxx([0-9A-Fa-f]{2})", key, re.I)
            if m:  # group wildcard -> (creator, suffix) fallback
                suf = int(m.group(2), 16)
                wild.setdefault((creator, suf), (v, vm, name))
                continue
            m = re.fullmatch(r"([0-9A-Fa-f]{8})", key)
            if m:  # fully explicit: keep standard data elements only
                group, el = int(key[:4], 16), int(key[4:], 16)
                if group % 2 == 1 and el >= 0x1000:
                    add(creator, group, el & 0xFF, v, vm, name, retired)
                else:
                    skipped["explicit-low/even"] = skipped.get("explicit-low/even", 0) + 1
                continue
            skipped["other"] = skipped.get("other", 0) + 1
    print(f"private: creators={len({c for c, _ in creators})} "
          f"groups={len(creators)} entries={sum(map(len, creators.values()))} "
          f"wild-defaults={len(wild_default)} group-wild={len(wild)} "
          f"dupes={dupes} skipped={skipped}")

    lines = [
        "//! GENERATED by `docs/generate-dictionary.py` — do not hand-edit.",
        "//!",
        "//! Private dictionary data derived from pydicom's private dictionary",
        '//! (MIT license, (c) pydicom contributors; data from the GDCM project).',
        "//! Only the tag/VR/VM/name facts are used; see pydicom for the",
        "//! full license text.",
        "",
        "use crate::Vr;",
        "",
        "/// One private data element description.",
        "#[derive(Clone, Copy, Debug, PartialEq, Eq)]",
        "pub struct PrivateEntry {",
        "    /// Advisory VR (private VRs are not authoritative).",
        "    pub vr: Vr,",
        "    /// Value multiplicity (`\"1\"`, `\"1-n\"`, …).",
        "    pub vm: &'static str,",
        "    /// Element name.",
        "    pub name: &'static str,",
        "    /// True when the source marks this entry retired.",
        "    pub retired: bool,",
        "}",
        "",
        "/// One private creator's reservation block in one (odd) group.",
        "pub struct PrivateCreator {",
        "    /// Private Creator ID string (LO, e.g. `\"ELSCINT1\"`).",
        "    pub creator: &'static str,",
        "    /// Odd group number this block was defined for.",
        "    pub group: u16,",
        "    /// `(suffix, entry)` pairs sorted by suffix; suffix is the low",
        "    /// byte of the data element tag.",
        "    pub entries: &'static [(u8, PrivateEntry)],",
        "    /// Whole-block wildcard entry, if the source defines one.",
        "    pub block_default: Option<PrivateEntry>,",
        "}",
        "",
        "/// Private creator blocks sorted by `(creator, group)`.",
        "pub const PRIVATE: &[PrivateCreator] = &[",
    ]

    def entry_lit(v, vm, name, retired):
        return (
            f"PrivateEntry {{ vr: Vr::{v}, vm: {rust_str(vm)}, "
            f"name: {rust_str(name)}, retired: {str(retired).lower()} }}"
        )

    for (creator, group) in sorted(creators):
        lines.append(
            f"    PrivateCreator {{ creator: {rust_str(creator)}, "
            f"group: 0x{group:04X}, entries: &["
        )
        for suffix, v, vm, name, retired in sorted(creators[(creator, group)]):
            lines.append(f"        (0x{suffix:02X}, {entry_lit(v, vm, name, retired)}),")
        lines.append("    ],")
        if (creator, group) in wild_default:
            v, vm, name = wild_default[(creator, group)]
            lines.append(f"    block_default: Some({entry_lit(v, vm, name, False)}),")
        else:
            lines.append("    block_default: None,")
        lines.append("    },")
    lines += [
        "];",
        "",
        "/// Group-wildcard entries `(creator, suffix, entry)` sorted by key.",
        "/// Consulted only when no group-specific block matches.",
        "pub const PRIVATE_WILD: &[(&str, u8, PrivateEntry)] = &[",
    ]
    for (creator, suf), (v, vm, name) in sorted(wild.items()):
        lines.append(
            f"    ({rust_str(creator)}, 0x{suf:02X}, {entry_lit(v, vm, name, False)}),"
        )
    lines += [
        "];",
        "",
        "/// Look up a private element by creator, group, and suffix byte.",
        "///",
        "/// The suffix is the low byte of the data element tag; see",
        "/// [`crate::Tag::private_suffix`]. Falls back to the block",
        "/// wildcard, then to group-agnostic entries. Returns `None` for",
        "/// unknown creators or unlisted suffixes.",
        "#[must_use]",
        "pub fn lookup_private(creator: &str, group: u16, suffix: u8) -> Option<&'static PrivateEntry>",
        "{",
        "    if let Ok(i) = PRIVATE",
        "        .binary_search_by(|c| (c.creator, c.group).cmp(&(creator, group)))",
        "    {",
        "        let block = &PRIVATE[i];",
        "        if let Ok(j) = block",
        "            .entries",
        "            .binary_search_by_key(&suffix, |(s, _)| *s)",
        "        {",
        "            return Some(&block.entries[j].1);",
        "        }",
        "        if let Some(d) = &block.block_default {",
        "            return Some(d);",
        "        }",
        "    }",
        "    PRIVATE_WILD",
        "        .binary_search_by(|(c, s, _)| (*c, *s).cmp(&(creator, suffix)))",
        "        .ok()",
        "        .map(|i| &PRIVATE_WILD[i].2)",
        "}",
        "",
    ]
    OUT_PRIV.write_text("\n".join(lines))
    print(f"wrote {OUT_PRIV}")


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--part06", default="")
    ap.add_argument("--private", default="")
    args = ap.parse_args()
    hand_idents = parse_hand_idents()
    print(f"hand-idents: {len(hand_idents)}")
    version, entries = parse_nema(fetch(args.part06, PART06_URL))
    emit_dictionary(version, entries, hand_idents)
    emit_private(fetch(args.private, PRIVATE_URL))
    return 0


if __name__ == "__main__":
    sys.exit(main())
