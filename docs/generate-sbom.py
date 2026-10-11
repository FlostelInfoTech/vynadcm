#!/usr/bin/env python3
"""Regenerate docs/sbom.cyclonedx.json from the locked dependency graph.

Usage:  python3 docs/generate-sbom.py
Requires: cargo, python3 (stdlib only). No network access.

Reads `cargo metadata` (which reflects Cargo.toml + Cargo.lock) and emits
a CycloneDX 1.5 software bill of materials covering the workspace members
and every locked third-party crate.
"""

import datetime
import json
import subprocess
import sys
from pathlib import Path

WORKSPACE = Path(__file__).resolve().parent.parent
OUT = WORKSPACE / "docs" / "sbom.cyclonedx.json"
SPEC_VERSION = "1.5"


def licenses_for(spdx: str | None) -> list:
    """Map a Cargo license string to CycloneDX license entries."""
    if not spdx:
        return []
    spdx = spdx.strip().strip("()")
    # Old dual-license shorthand ("MIT/Apache-2.0") is not valid SPDX
    # expression syntax; normalize to OR.
    if "/" in spdx and " OR " not in spdx and " AND " not in spdx:
        spdx = spdx.replace("/", " OR ")
    # SPDX expression (OR/AND/WITH) vs single identifier.
    if any(op in spdx for op in (" OR ", " AND ", " WITH ")):
        return [{"expression": spdx}]
    return [{"license": {"id": spdx}}]


def lock_checksums() -> dict:
    """Map (name, version) -> sha256 from Cargo.lock (cargo metadata omits it)."""
    out, name, version = {}, None, None
    for line in (WORKSPACE / "Cargo.lock").read_text().splitlines():
        line = line.strip()
        if line == "[[package]]":
            name, version = None, None
        elif line.startswith("name = ") and '"' in line:
            name = line.split('"')[1]
        elif line.startswith("version = ") and '"' in line:
            version = line.split('"')[1]
        elif line.startswith("checksum = ") and name and version:
            out[(name, version)] = line.split('"')[1]
    return out


def main() -> int:
    meta = json.loads(
        subprocess.run(
            ["cargo", "metadata", "--format-version", "1"],
            cwd=WORKSPACE,
            capture_output=True,
            text=True,
            check=True,
        ).stdout
    )
    members = set(meta["workspace_members"])
    checksums = lock_checksums()

    components = []
    for pkg in sorted(meta["packages"], key=lambda p: p["name"].lower()):
        is_member = pkg["id"] in members
        comp = {
            "bom-ref": f"pkg:cargo/{pkg['name']}@{pkg['version']}",
            "type": "library",
            "name": pkg["name"],
            "version": pkg["version"],
            "scope": "required",
            "purl": f"pkg:cargo/{pkg['name']}@{pkg['version']}",
        }
        lic = licenses_for(pkg.get("license"))
        if lic:
            comp["licenses"] = lic
        digest = checksums.get((pkg["name"], pkg["version"]))
        if not is_member and digest:
            comp["hashes"] = [{"alg": "SHA-256", "content": digest}]
        if is_member:
            comp["description"] = "VynaDCM workspace member (proprietary)"
        components.append(comp)

    root = next(p for p in meta["packages"] if p["name"] == "vyna-dcm")
    bom = {
        "bomFormat": "CycloneDX",
        "specVersion": SPEC_VERSION,
        "version": 1,
        "metadata": {
            "timestamp": datetime.datetime.now(datetime.timezone.utc)
            .isoformat(timespec="seconds")
            .replace("+00:00", "Z"),
            "tools": {
                "components": [
                    {
                        "type": "application",
                        "name": "VynaDCM generate-sbom.py",
                        "version": root["version"],
                    }
                ]
            },
            "component": {
                "bom-ref": f"pkg:cargo/vyna-dcm@{root['version']}",
                "type": "application",
                "name": "vyna-dcm",
                "version": root["version"],
            },
        },
        "components": components,
    }
    OUT.write_text(json.dumps(bom, indent=2) + "\n")
    third_party = sum(1 for p in meta["packages"] if p["id"] not in members)
    print(f"wrote {OUT} ({len(members)} workspace, {third_party} third-party)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
