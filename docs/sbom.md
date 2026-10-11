# VynaDCM SBOM

| Field | Value |
|---|---|
| Document | Software bill of materials (living) |
| Version | 0.1.0 |
| Date | 2026-10-11 |
| Status | Living — regenerated on every dependency change |
| Applies to | VynaDCM workspace v0.1.0 |

Revision history: 0.1.0 (2026-10-11) — initial issue (201
components); Unreleased — bundled JPEG codecs: 210 components (204
third-party), incl. vendored `openjp2` (BSD-2-Clause, patched in-tree).

## 1. Artifact

`docs/sbom.cyclonedx.json` is the authoritative bill of materials in
[CycloneDX](https://cyclonedx.org/) 1.5 JSON format. It currently lists
**6 workspace members** (proprietary) and **204 third-party crates**,
each with name, version, Package URL, SPDX license, and (for registry
crates) the SHA-256 checksum from `Cargo.lock`. One third-party
component, `openjp2` 0.6.1, is vendored under `vendor/` with a VynaDCM
patch (see `vendor/openjp2/README.VYNA.md`) instead of a lock checksum.

License mix is permissive throughout: predominantly
`MIT OR Apache-2.0` / `MIT`, plus `BSD-2-Clause` (openjp2),
`Unicode-3.0` (unicode tables), `0BSD`, `ISC`, and `Unlicense`
duals. No copyleft licenses are present. Per-component detail lives
in the JSON, not here, so this page cannot rot.

## 2. Regenerating

```sh
python3 docs/generate-sbom.py
```

The generator uses only `cargo metadata`, `Cargo.lock`, and the Python
standard library — no network, no plugins. Re-run it whenever
dependencies change and commit the refreshed JSON alongside the
`Cargo.toml` / `Cargo.lock` edit.

## 3. Scope notes

- The SBOM reflects the **locked** graph (`Cargo.lock`), i.e. exactly
  what CI and releases build.
- Workspace members are marked `Proprietary` with
  `"description": "VynaDCM workspace member (proprietary)"`.
- Build-time-only and test-only crates (e.g. `rcgen`, used solely by
  the TLS loopback test) are included with `scope: required`, since
  CycloneDX scope here means “required to reproduce the build”.
- Known single-provider constraint (auditable from this SBOM): the
  graph must contain `ring` and must **not** contain `aws-lc-rs` /
  `aws-lc-sys`, so rustls has exactly one `CryptoProvider`. Verify
  with `cargo tree -i aws-lc-rs` (expect no match).
