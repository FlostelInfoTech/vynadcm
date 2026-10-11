#!/usr/bin/env python3
"""Generate IOD validation tables from NEMA PS3.3 DocBook XML.

Reads the IOD Module Tables (Annex A) for the target SOP Classes plus the
Module Attribute Tables (Annex C) they reference, and emits
`crates/vyna-dcm-iod/src/iod_generated.rs`.

Scope (matches the P0.3 validator): top-level attributes only. Sequence
*items* (rows starting with `>`) and repeating-group rows (`(60xx,...)`)
are skipped and reported — never silently. Macro `Include` rows are
expanded recursively (file/C-STORE context: the single STOW-RS-gated
include is treated as required). Conditional types (1C/2C) fold to "if
present" except a curated set of evaluable conditions (see TAG_RULES);
modules with Usage C/U never enforce presence.

Usage:
    python3 docs/generate-iod.py [--part03 PATH] [--out PATH]
With no --part03, downloads NEMA "current" PS3.3 (p instability: entry
counts follow whatever edition "current" resolves to; the stamped edition
is recorded in the generated header).
"""

import argparse
import re
import urllib.request
import xml.etree.ElementTree as ET
from pathlib import Path

NS = {"d": "http://docbook.org/ns/docbook"}
XMLID = "{http://www.w3.org/XML/1998/namespace}id"

# SR value-type macros (NUM/CODE/IMAGE/...) describe content items whose
# applicability depends on Value Type, which P0.3 does not evaluate. Their
# rows fold to IfPresent — except the Container macro (C.18.8-1): the only
# shipped IOD using SR content is KO, whose root item is always a
# CONTAINER, so its rows (e.g. Continuity of Content, Type 1) apply.
DEMOTE_MACROS = {
    "table_C.18.1-1",  # NUM
    "table_C.18.2-1",  # CODE
    "table_C.18.3-1",  # COMPOSITE
    "table_C.18.4-1",  # IMAGE
    "table_C.18.5-1",  # WAVEFORM
    "table_C.18.6-1",  # SCOORD
    "table_C.18.9-1",  # SCOORD3D
    "table_C.18.7-1",  # TCOORD
    "table_C.18.10-1",  # TABLE
}

# 1C tags whose condition the validator evaluates (tag -> CondRule variant).
# Every other 1C/2C folds to Req::IfPresent.
TAG_RULES = {
    (0x7FE0, 0x0010): "PixelData",  # file/C-STORE context: always required
    (0x0028, 0x0009): "FrameIncrementPointer",  # multi-frame only
    (0x0028, 0x0006): "PlanarConfiguration",  # color (SamplesPerPixel > 1) only
    (0x0028, 0x2110): "LossyCompression",  # lossy transfer syntax only
    (0x0028, 0x2112): "LossyRatio",  # only if Lossy Image Compression is "01"
    (0x0008, 0x0005): "SpecificCharacterSet",  # only if non-ASCII text present
    (0x7FE0, 0x0002): "ExtOffsetTableLengths",  # only if Extended Offset Table present
}
PART03_URL = "https://dicom.nema.org/medical/dicom/current/source/docbook/part03/part03.xml"
DEFAULT_OUT = (
    Path(__file__).resolve().parent.parent
    / "crates/vyna-dcm-iod/src/iod_generated.rs"
)

# (SOP Class UID, Part-3 IOD section id, SOP name). Section ids come from the
# Part-4 "Standard SOP Classes" olinks (verified 2026-10-11).
TARGETS = [
    ("1.2.840.10008.5.1.4.1.1.1", "sect_A.2", "Computed Radiography Image Storage"),
    ("1.2.840.10008.5.1.4.1.1.1.1", "sect_A.26", "Digital X-Ray Image Storage - For Presentation"),
    ("1.2.840.10008.5.1.4.1.1.1.1.1", "sect_A.26", "Digital X-Ray Image Storage - For Processing"),
    ("1.2.840.10008.5.1.4.1.1.1.2", "sect_A.27", "Digital Mammography X-Ray Image Storage - For Presentation"),
    ("1.2.840.10008.5.1.4.1.1.1.2.1", "sect_A.27", "Digital Mammography X-Ray Image Storage - For Processing"),
    ("1.2.840.10008.5.1.4.1.1.2", "sect_A.3", "CT Image Storage"),
    ("1.2.840.10008.5.1.4.1.1.3.1", "sect_A.7", "Ultrasound Multi-frame Image Storage"),
    ("1.2.840.10008.5.1.4.1.1.4", "sect_A.4", "MR Image Storage"),
    ("1.2.840.10008.5.1.4.1.1.6.1", "sect_A.6", "Ultrasound Image Storage"),
    ("1.2.840.10008.5.1.4.1.1.7", "sect_A.8.1", "Secondary Capture Image Storage"),
    ("1.2.840.10008.5.1.4.1.1.9.1.1", "sect_A.34.3", "12-lead ECG Waveform Storage"),
    ("1.2.840.10008.5.1.4.1.1.9.1.2", "sect_A.34.4", "General ECG Waveform Storage"),
    ("1.2.840.10008.5.1.4.1.1.12.1", "sect_A.14", "X-Ray Angiographic Image Storage"),
    ("1.2.840.10008.5.1.4.1.1.20", "sect_A.5", "Nuclear Medicine Image Storage"),
    ("1.2.840.10008.5.1.4.1.1.77.1.4", "sect_A.32.4", "VL Photographic Image Storage"),
    ("1.2.840.10008.5.1.4.1.1.88.59", "sect_A.35.4", "Key Object Selection Document Storage"),
    ("1.2.840.10008.5.1.4.1.1.104.1", "sect_A.45.1", "Encapsulated PDF Storage"),
    ("1.2.840.10008.5.1.4.1.1.128", "sect_A.21", "Positron Emission Tomography Image Storage"),
]


def tx(el) -> str:
    return " ".join("".join(el.itertext()).split()).replace("\u200b", "")


def rust_str(s: str) -> str:
    return '"' + s.replace("\\", "\\\\").replace('"', '\\"') + '"'


def fetch(path_or_empty: str) -> str:
    if path_or_empty:
        return Path(path_or_empty).read_text(encoding="utf-8")
    with urllib.request.urlopen(PART03_URL) as r:
        return r.read().decode("utf-8")


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--part03", default="")
    ap.add_argument("--out", default=str(DEFAULT_OUT))
    args = ap.parse_args()

    root = ET.fromstring(fetch(args.part03))
    sub = root.find(".//d:subtitle", NS)
    version = tx(sub) if sub is not None else "unknown"

    sections = {}
    for s in root.findall(".//d:section", NS):
        sid = s.get(XMLID)
        t = s.find("d:title", NS)
        if sid and t is not None:
            sections[sid] = s
    by_title: dict[str, list] = {}
    for s in root.findall(".//d:section", NS):
        t = s.find("d:title", NS)
        if t is not None:
            by_title.setdefault(tx(t), []).append(s)

    def module_table(iod_sec) -> ET.Element:
        for tbl in iod_sec.findall(".//d:table", NS):
            heads = [tx(h) for h in tbl.findall("d:thead/d:tr/d:th", NS)]
            if heads[:4] == ["IE", "Module", "Reference", "Usage"]:
                return tbl
        raise SystemExit("no IOD module table found")

    def attr_table(mod_sec):
        for tbl in mod_sec.findall(".//d:table", NS):
            heads = [tx(h).lower() for h in tbl.findall("d:thead/d:tr/d:th", NS)]
            if len(heads) >= 3 and heads[:3] == ["attribute name", "tag", "type"]:
                return tbl
        return None

    # IOD section id -> [(module, usage)]
    iod_modules: dict[str, list] = {}
    for _uid, sid, _name in TARGETS:
        if sid in iod_modules:
            continue
        sec = sections.get(sid)
        if sec is None:
            raise SystemExit(f"missing IOD section {sid}")
        tbl = module_table(sec)
        rows, ie = [], ""
        for tr in tbl.findall("d:tbody/d:tr", NS):
            cells = [tx(c) for c in tr.findall("d:td", NS)]
            # Rowspans drop the IE cell: 4 cells, else 3 with inherited IE.
            if len(cells) == 4:
                ie = cells[0] or ie
                module, usage = cells[1], cells[3].strip()[:1]
            elif len(cells) == 3:
                module, usage = cells[0], cells[2].strip()[:1]
            else:
                continue
            if not module:
                continue
            if not usage:
                raise SystemExit(f"{sid}: module {module!r} has no M/C/U usage")
            rows.append((ie, module, usage))
        iod_modules[sid] = rows
    n_mod_refs = sum(len(v) for v in iod_modules.values())
    print(f"IODs: {len(iod_modules)} sections, {n_mod_refs} module refs")

    tables_by_id = {}
    for tbl in root.findall(".//d:table", NS):
        tid = tbl.get(XMLID)
        if tid:
            tables_by_id[tid] = tbl

    skipped = {"nested": 0, "tag": [], "type": []}
    expanded: dict[str, list] = {}  # module -> [(table id, condition)]

    def expand_table(tbl, chain: tuple, demoted: bool = False) -> list:
        """Top-level (tag, type) rows with recursive macro expansion."""
        out = []
        for tr in tbl.findall("d:tbody/d:tr", NS):
            tds = tr.findall("d:td", NS)
            cells = [tx(c) for c in tds]
            if not cells:
                continue
            if cells[0].startswith("Include"):
                xr = tr.find(".//d:xref", NS)
                ref = xr.get("linkend", "") if xr is not None else ""
                if not ref:
                    raise SystemExit(f"include without table ref in {chain}")
                if ref in chain:
                    raise SystemExit(f"macro cycle: {chain} -> {ref}")
                tgt = tables_by_id.get(ref)
                if tgt is None:
                    # Some includes point at a section, not a table.
                    sec = sections.get(ref)
                    tgt = attr_table(sec) if sec is not None else None
                    if tgt is None:
                        raise SystemExit(f"unresolvable macro ref: {ref}")
                cond = cells[-1] if len(cells) > 1 else ""
                expanded.setdefault(chain[0], []).append((ref, cond))
                out.extend(
                    expand_table(tgt, chain + (ref,), demoted or ref in DEMOTE_MACROS)
                )
                continue
            if len(cells) < 3:
                continue
            name, tag, typ = cells[0], cells[1], cells[2].split()[0] if cells[2] else ""
            if name.startswith(">"):
                skipped["nested"] += 1
                continue
            m = re.fullmatch(r"\(([0-9A-Fa-f]+),([0-9A-Fa-f]+)\)", tag)
            if not m:
                skipped["tag"].append((chain[0], name, tag))
                continue
            if not re.fullmatch(r"[123]C?", typ):
                skipped["type"].append((chain[0], name, tag, typ))
                continue
            if demoted:
                typ = "3"
            out.append((int(m.group(1), 16), int(m.group(2), 16), typ))
        return out

    # module name -> [(tag, type)] top-level attributes
    mod_attrs: dict[str, list] = {}
    for _uid, sid, _name in TARGETS:
        for _ie, module, _usage in iod_modules[sid]:
            if module in mod_attrs:
                continue
            cands = by_title.get(f"{module} Module", [])
            if not cands:
                raise SystemExit(f"missing module section: {module!r} Module")
            # Same-titled Annex A subsections (IOD-specific variants) sort
            # before Annex C; the canonical module lives in Annex C.
            cands = sorted(
                cands, key=lambda s: not (s.get(XMLID) or "").startswith("sect_C.")
            )
            tbl = None
            for sec in cands:
                tbl = attr_table(sec)
                if tbl is not None:
                    break
            if tbl is None:
                raise SystemExit(f"no attribute table in: {module} Module")
            mod_attrs[module] = expand_table(tbl, (module,))
    n_attrs = sum(len(v) for v in mod_attrs.values())
    print(f"modules: {len(mod_attrs)}, top-level attrs (macro-expanded): {n_attrs}, "
          f"nested skipped: {skipped['nested']}")
    n_inc = sum(len(v) for v in expanded.values())
    print(f"macro includes expanded: {n_inc} in {len(expanded)} modules")
    for module, refs in sorted(expanded.items()):
        for ref, cond in refs:
            flag = f" :: [{cond[:70]}]" if cond else ""
            print(f"  {module[:30]:32s} <- {ref}{flag}")
    if skipped["tag"]:
        print(f"unparsed tag rows: {len(skipped['tag'])}")
        for row in skipped["tag"][:12]:
            print("  ", row)
    if skipped["type"]:
        print(f"unparsed type rows: {len(skipped['type'])}")
        for row in skipped["type"][:12]:
            print("  ", row)

    # Merge per IOD: tag -> (strictest presence class, module).
    # Presence rank: T1 > T2 > rule-backed 1C > (T3, plain 1C, 2C,
    # non-M). Non-M modules never enforce presence; 1C/2C fold to
    # IfPresent except TAG_RULES tags.
    def rank_of(typ: str, tag: tuple[int, int]) -> float:
        base = {"1": 3, "2": 2, "3": 1, "1C": 1, "2C": 1}[typ]
        if typ == "1C" and tag in TAG_RULES:
            return 1.5
        return base

    iods = []
    for uid, sid, name in TARGETS:
        merged: dict[tuple[int, int], tuple[str, str]] = {}
        for _ie, module, usage in iod_modules[sid]:
            for g, e, typ in mod_attrs[module]:
                eff = typ if usage == "M" else "3"
                cur = merged.get((g, e))
                if cur is None or rank_of(eff, (g, e)) > rank_of(cur[0], (g, e)):
                    merged[(g, e)] = (eff, module)
        iods.append((uid, name, merged))
    total = sum(len(m) for _, _, m in iods)
    print(f"emit: {len(iods)} SOPs, {total} attr rows")

    lines = [
        "//! GENERATED by `docs/generate-iod.py` — do not hand-edit.",
        f"//! IOD tables: NEMA {version}.",
        "//! Top-level attributes only; sequence items, macro includes and",
        "//! conditional-type evaluation are out of scope (see generator).",
        "",
        "use super::{CondRule, Iod, IodAttr, Req};",
        "use vyna_dcm_core::Tag;",
        "",
        "/// Supported IODs, sorted by SOP Class UID.",
        "pub const IODS: &[Iod] = &[",
    ]
    for uid, name, merged in sorted(iods):
        lines.append(
            f"    Iod {{ uid: {rust_str(uid)}, name: {rust_str(name)}, attrs: &["
        )
        for (g, e), (typ, module) in sorted(merged.items()):
            if typ == "1C" and (g, e) in TAG_RULES:
                req = f"Req::Cond(CondRule::{TAG_RULES[(g, e)]})"
            else:
                req = {"1": "Req::T1", "2": "Req::T2"}.get(typ, "Req::IfPresent")
            lines.append(
                f"        IodAttr {{ tag: Tag::new(0x{g:04X}, 0x{e:04X}), "
                f"req: {req}, module: {rust_str(module)} }},"
            )
        lines.append("    ] },")
    lines += ["];", ""]
    out = Path(args.out)
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text("\n".join(lines))
    print(f"wrote {out}")


if __name__ == "__main__":
    main()
