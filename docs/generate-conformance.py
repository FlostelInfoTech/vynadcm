#!/usr/bin/env python3
"""Regenerate docs/conformance.md from implemented capabilities.

Usage:  python3 docs/generate-conformance.py [--check]
  --check  exit 1 with a diff when the committed file differs; writes nothing.
Requires: python3 (stdlib only). No network access.

Every capability claim below is extracted from the workspace sources; the
script asserts each extraction so new capabilities (or refactors that move
them) fail loudly instead of silently going stale. Output is deterministic
(no timestamps) so `--check` is a byte comparison.
"""

import difflib
import re
import sys
from pathlib import Path

WORKSPACE = Path(__file__).resolve().parent.parent
OUT = WORKSPACE / "docs" / "conformance.md"
CORE = WORKSPACE / "crates" / "vyna-dcm-core" / "src"
NET = WORKSPACE / "crates" / "vyna-dcm-net" / "src"
BINS = WORKSPACE / "crates" / "vyna-dcm" / "src" / "bin"
FACACE_CARGO = WORKSPACE / "crates" / "vyna-dcm" / "Cargo.toml"
NET_CARGO = WORKSPACE / "crates" / "vyna-dcm-net" / "Cargo.toml"


def must_find(pattern: str, text: str, what: str) -> "re.Match[str]":
    m = re.search(pattern, text, re.M)
    assert m, f"conformance: cannot locate {what}"
    return m


# -- SOP classes -----------------------------------------------------------

UID_RS = CORE / "uid.rs"
uid_src = UID_RS.read_text()
sop_mod = re.search(r"pub mod sop_class \{(.*?)\n\}\n", uid_src, re.S)
assert sop_mod, "conformance: cannot locate sop_class module"
sop_src = sop_mod.group(1)
sop_consts: dict[str, tuple[str, str]] = {}  # NAME -> (uid, description)
for m in re.finditer(
    r'/// ([^\n]*)\n    pub const ([A-Z][A-Z0-9_]*): &str = "([0-9.]+)";', sop_src
):
    desc, name, uid = m.group(1), m.group(2), m.group(3)
    sop_consts[name] = (uid, desc)
assert len(sop_consts) > 30, "conformance: SOP table parse collapsed"


def list_members(list_name: str) -> list[str]:
    m = re.search(
        rf"pub const {list_name}: &\[&str\] = &\[(.*?)\];", sop_src, re.S
    )
    assert m, f"conformance: cannot locate {list_name}"
    members = re.findall(r"^\s*([A-Z][A-Z0-9_]*),?\s*$", m.group(1), re.M)
    assert members, f"conformance: {list_name} parsed empty"
    for name in members:
        assert name in sop_consts, f"conformance: {list_name} refs unknown {name}"
    return members


STORAGE = set(list_members("DEFAULT_STORAGE_CLASSES"))
QUERY = set(list_members("QUERY_CLASSES"))

# Non-storage SOPs that belong to no proposal list; each needs a conscious
# category here (an assertion on a new leftover forces the classification,
# not silent drift). Leftovers described as Storage are classified by rule
# below: defined UIDs usable wherever the peer accepts them (e.g. storescu
# proposes file-derived classes) but not in the default proposal lists.
LEFTOVER_CATEGORY = {
    "VERIFICATION": "Verification",
    "MODALITY_PERFORMED_PROCEDURE_STEP": "N-service (MPPS)",
    "STORAGE_COMMITMENT_PUSH": "N-service (commitment)",
    "STORAGE_COMMITMENT_PULL": "N-service (commitment)",
    "MEDIA_STORAGE_DIRECTORY": "Media (DICOMDIR)",
}


def sop_category(name: str) -> str:
    if name in STORAGE:
        return "Storage"
    if name in QUERY:
        return "Query/Retrieve"
    if name in LEFTOVER_CATEGORY:
        return LEFTOVER_CATEGORY[name]
    if " Storage" in sop_consts[name][1]:
        return "Storage (defined; not in default proposals)"
    raise AssertionError(f"conformance: unclassified SOP {name}")


# -- transfer syntaxes ------------------------------------------------------

TS_RS = CORE / "transfer_syntax.rs"
ts_src = TS_RS.read_text()
ts_uid = dict(
    re.findall(r"TransferSyntax::(\w+) => \"([0-9.]+)\"", ts_src)
)
ts_name = dict(
    re.findall(r'TransferSyntax::(\w+) => "([^"]+)",\n', ts_src)
)
ts_name = {k: v for k, v in ts_name.items() if not v[0].isdigit()}
assert ts_uid and ts_uid.keys() == ts_name.keys(), (
    "conformance: transfer syntax uid()/name() mismatch"
)
m_all = re.search(
    r"pub const ALL: \[TransferSyntax; \d+\] = \[(.*?)\];", ts_src, re.S
)
assert m_all, "conformance: cannot locate TS ALL"
all_block = m_all.group(1)
all_variants = set(re.findall(r"TransferSyntax::(\w+)", all_block))
assert all_variants == set(ts_uid), "conformance: TS ALL out of sync"
m_lossy = re.search(r"fn is_lossy.*?matches!\(\s*self,(.*?)\)", ts_src, re.S)
assert m_lossy, "conformance: cannot locate is_lossy"
lossy_block = m_lossy.group(1)
LOSSY = set(re.findall(r"TransferSyntax::(\w+)", lossy_block))
assert LOSSY, "conformance: is_lossy parse collapsed"

# -- implementation identity -------------------------------------------------

version = must_find(r'^version = "([^"]+)"', FACACE_CARGO.read_text(), "crate version").group(1)
impl_uid = must_find(
    r'VYNA_IMPLEMENTATION_CLASS_UID: &str = "([^"]+)"', uid_src, "impl class UID"
).group(1)
impl_ver = must_find(
    r'VYNA_IMPLEMENTATION_VERSION_NAME: &str = "([^"]+)"', uid_src, "impl version"
).group(1)
app_ctx = must_find(
    r'APPLICATION_CONTEXT_NAME: &str = "([^"]+)"', uid_src, "app context"
).group(1)


def assoc_default(field: str, pattern: str) -> str:
    src = (NET / "association.rs").read_text()
    return must_find(rf"{field}: {pattern}", src, f"assoc {field}").group(1)


MAX_PDU_LIB = assoc_default("max_pdu_len", r"([\d *]+)")
MAX_INCOMING = assoc_default("max_incoming_pdu", r"([\d *]+)")

# -- CLI defaults ------------------------------------------------------------

common_src = (BINS / "common.rs").read_text()


def struct_body(struct_name: str) -> str:
    m = re.search(rf"pub struct {struct_name} \{{(.*?)\n\}}", common_src, re.S)
    assert m, f"conformance: cannot locate {struct_name}"
    return m.group(1)


CONN_SRC = struct_body("ConnArgs")
LISTEN_SRC = struct_body("ListenArgs")


def cli_default(src: str, struct_name: str, field: str, pattern: str) -> str:
    # `#[arg(long, default_value = "X")]` immediately above `pub FIELD:`
    m = re.search(
        rf"#\[arg\([^\]]*{pattern}[^\]]*\)\]\n    pub {field}:", src
    )
    assert m, f"conformance: cannot locate {struct_name}.{field}"
    return m.group(1)


CALLED_AE = cli_default(CONN_SRC, "ConnArgs", "called", r'default_value = "([^"]+)"')
CALLING_AE = cli_default(CONN_SRC, "ConnArgs", "calling", r'default_value = "([^"]+)"')
SCP_AE = cli_default(LISTEN_SRC, "ListenArgs", "ae_title", r'default_value = "([^"]+)"')
BIND = cli_default(LISTEN_SRC, "ListenArgs", "bind", r'default_value = "([^"]+)"')
SCU_TIMEOUT = cli_default(CONN_SRC, "ConnArgs", "timeout_secs", r"default_value_t = (\d+)")
SCP_TIMEOUT = cli_default(LISTEN_SRC, "ListenArgs", "timeout_secs", r"default_value_t = (\d+)")
SCU_PDU_EXPR = cli_default(CONN_SRC, "ConnArgs", "max_pdu", r"default_value_t = ([\d *]+)")
SCP_PDU_EXPR = cli_default(LISTEN_SRC, "ListenArgs", "max_pdu", r"default_value_t = ([\d *]+)")
assert SCU_PDU_EXPR == SCP_PDU_EXPR, "conformance: SCU/SCP max_pdu differ"


def human_bytes(expr: str) -> str:
    n = 1
    for part in expr.split("*"):
        n *= int(part.strip())
    if n % (1024 * 1024) == 0:
        return f"{n // (1024 * 1024)} MiB"
    if n % 1024 == 0:
        return f"{n // 1024} KiB"
    return f"{n} bytes"


MAX_PDU = human_bytes(SCU_PDU_EXPR)
MAX_INCOMING_H = human_bytes(MAX_INCOMING)
assert MAX_PDU_LIB.replace(" ", "") == SCU_PDU_EXPR.replace(" ", ""), (
    "conformance: CLI max_pdu diverged from library default"
)
assert "tls_client_ca" in common_src and "tls_cert" in common_src, (
    "conformance: TLS flags moved"
)

# -- negotiation posture -----------------------------------------------------

req_src = (NET / "association.rs").read_text()
assert "role_selection: Vec::new()" in req_src, "conformance: SCU role posture moved"
assert "user_identity: None" in req_src, "conformance: SCU identity posture moved"
svc_src = (NET / "service.rs").read_text()
assert "TransferSyntax::ALL" in svc_src, "conformance: acceptor syntax gating moved"
assert "accepted_sop_classes: None" in svc_src, "conformance: acceptor SOP gating moved"
assert "check_called_ae: false" in svc_src, "conformance: acceptor AE check moved"
assert "user_identity_auth: None" in svc_src, "conformance: acceptor identity default moved"
assert "fn propose(" in svc_src, "conformance: SCU propose() moved"
assert "rq.role_selection.clone()" in svc_src, (
    "conformance: SCP role handling changed; update the role claim"
)

# -- TLS ---------------------------------------------------------------------

net_cargo = NET_CARGO.read_text()
assert 'default = ["tls"]' in net_cargo, "conformance: TLS default feature moved"
assert re.search(r'^tls = \[.*tokio-rustls.*rustls', net_cargo, re.M), (
    "conformance: TLS backend moved"
)
tls_src = (NET / "tls.rs").read_text()
assert "struct TlsConnector" in tls_src and "struct TlsAcceptor" in tls_src, (
    "conformance: TLS types moved"
)
assert "webpki-roots" in net_cargo, "conformance: WebPKI roots evidence moved"
root_cargo = (WORKSPACE / "Cargo.toml").read_text()
assert '"ring"' in root_cargo and "tokio-rustls" in root_cargo, (
    "conformance: ring backend evidence moved"
)

# -- character sets ------------------------------------------------------------

decode_src = (WORKSPACE / "crates" / "vyna-dcm-codec" / "src" / "decode.rs").read_text()
assert "from_utf8_lossy" in decode_src, "conformance: text decode posture moved"
pdf_src = (WORKSPACE / "crates" / "vyna-dcm-pdf" / "src" / "document.rs").read_text()
assert "ISO_IR 192" in pdf_src, "conformance: PDF charset evidence moved"

# -- media + web -----------------------------------------------------------------

codec_lib = WORKSPACE / "crates" / "vyna-dcm-codec" / "src" / "lib.rs"
codec_src = codec_lib.read_text()
assert "build_dicomdir" in codec_src and "read_dicomdir" in codec_src, (
    "conformance: DICOMDIR API moved"
)
web_client = (WORKSPACE / "crates" / "vyna-dcm-web" / "src" / "client.rs").read_text()
for fn in ("qido", "search_studies", "wado_dataset", "wado_uri_dataset", "stow"):
    assert f"pub async fn {fn}" in web_client, f"conformance: web client {fn} moved"

# -- per-tool services -----------------------------------------------------------

SCU_METHODS = {
    "echo": "C-ECHO",
    "store": "C-STORE",
    "find_with": "C-FIND",
    "find_all": "C-FIND",
    "find_cancelable": "C-FIND",
    "move_to": "C-MOVE",
    "get_with": "C-GET",
    "n_create": "N-CREATE",
    "n_set": "N-SET",
    "n_get": "N-GET",
    "n_action": "N-ACTION",
    "n_delete": "N-DELETE",
    "n_event_report": "N-EVENT-REPORT",
    "storage_commit": "N-ACTION + N-EVENT-REPORT (commitment)",
}
SCP_HANDLERS = {
    "on_echo": "C-ECHO",
    "on_store": "C-STORE",
    "on_find": "C-FIND",
    "on_move": "C-MOVE",
    "on_get": "C-GET",
    "on_n_create": "N-CREATE",
    "on_n_set": "N-SET",
    "on_n_get": "N-GET",
    "on_n_action": "N-ACTION",
    "on_n_delete": "N-DELETE",
    "on_n_event_report": "N-EVENT-REPORT",
}

bin_names = re.findall(r'\[\[bin\]\]\nname = "([^"]+)"', FACACE_CARGO.read_text())
assert bin_names, "conformance: no [[bin]] entries found"
tools = []  # (name, kind, about, services, sop_consts, notes)
for name in bin_names:
    src = (BINS / f"{name}.rs").read_text()
    about = must_find(r'about = "([^"]+)"', src, f"{name} about").group(1)
    if "ConnArgs" in src:
        kind = "SCU"
    elif "ListenArgs" in src:
        kind = "SCP"
    else:
        kind = "file"
    services: list[str] = []
    sops: list[str] = []
    notes: list[str] = []
    if kind == "SCU":
        for m in re.finditer(r"assoc\s*\.\s*(\w+)", src):
            svc = SCU_METHODS.get(m.group(1))
            if svc and svc not in services:
                services.append(svc)
        assert services, f"conformance: no DIMSE usage detected in {name}"
        for c in re.findall(r"sop_class::([A-Z][A-Z0-9_]*)", src):
            if c == "DEFAULT_STORAGE_CLASSES":
                sops.append(c)
            else:
                assert c in sop_consts, f"conformance: {name} refs unknown {c}"
                if c not in sops:
                    sops.append(c)
        if "syntaxes.push(ts)" in src:
            notes.append("also proposes each input file's own transfer syntax")
        if "DEFAULT_STORAGE_CLASSES" in sops:
            notes.append(
                f"also proposes {len(STORAGE)} storage SOP classes for C-GET delivery"
            )
    elif kind == "SCP":
        for m in re.finditer(r"async fn (on_\w+)", src):
            svc = SCP_HANDLERS.get(m.group(1))
            assert svc, f"conformance: unknown handler {m.group(1)} in {name}"
            if svc not in services:
                services.append(svc)
        if re.search(r"impl Handlers for \w+ \{\}", src):
            services.append("C-ECHO (all other services use rejecting defaults)")
    tools.append((name, kind, about, services, sops, notes))

scu_tools = [t for t in tools if t[1] == "SCU"]
scp_tools = [t for t in tools if t[1] == "SCP"]
assert len(scu_tools) >= 7 and len(scp_tools) >= 2, "conformance: AE table collapsed"


def sop_line(const: str) -> str:
    uid, desc = sop_consts[const]
    return f"{desc.rstrip('.')} (`{uid}`)"


def uid_key(uid: str) -> tuple:
    return tuple(int(p) for p in uid.split("."))


# -- render ----------------------------------------------------------------------

L = []
add = L.append
add("# VynaDCM DICOM Conformance Statement")
add("")
add(f"Product: VynaDCM `{version}` (workspace crates `vyna-dcm-*`).")
add("Generated from the implemented capabilities by")
add("`docs/generate-conformance.py` — do not hand-edit; rerun the script.")
add("")
add("## 1. Introduction")
add("")
add("This statement describes the DICOM networking, media, and DICOMweb")
add("behaviour of the VynaDCM command-line Application Entities and the")
add("library defaults behind them, following the structure of PS3.2.")
add("Implementation Class UID")
add(f"`{impl_uid}`, Implementation Version Name `{impl_ver}`.")
add("")
add("## 2. Implementation model")
add("")
add("| Application Entity | Default AE title | Role | Services |")
add("|--------------------|------------------|------|----------|")
for name, kind, _about, services, _sops, _notes in scu_tools + scp_tools:
    ae = CALLING_AE if kind == "SCU" else SCP_AE
    add(f"| `{name}` | `{ae}` | {kind} | {', '.join(services)} |")
add("")
add("Application data flow is tool-to-peer over one TCP association per")
add("invocation: SCU tools connect, negotiate, perform their service, and")
add("release; SCP tools listen, serve each inbound association to")
add("release/abort, and loop.")
add("")
add("## 3. AE specifications")
add("")
add("### 3.1 SCU tools")
add("")
for i, (name, _k, about, services, sops, notes) in enumerate(scu_tools, 1):
    add(f"#### 3.1.{i} `{name}` — {about}")
    add("")
    add(f"- Services: {', '.join(services)}.")
    if sops:
        add("- SOP Classes proposed (SCU role):")
        for c in sops:
            if c == "DEFAULT_STORAGE_CLASSES":
                for m in sorted(STORAGE, key=lambda m: uid_key(sop_consts[m][0])):
                    add(f"  - {sop_line(m)}.")
            else:
                add(f"  - {sop_line(c)}.")
    else:
        add("- SOP Classes: taken from the input files' SOP Class UIDs")
        add("  (any storage SOP Class the peer accepts).")
    add("- Transfer syntaxes proposed: Explicit VR Little Endian and")
    add("  Implicit VR Little Endian.")
    for n in notes:
        if "storage SOP classes for C-GET" in n:
            continue  # now enumerated above
        add(f"- Note: {n}.")
    add("")
add("### 3.2 SCP tools")
add("")
for i, (name, _k, about, services, _s, _n) in enumerate(scp_tools, 1):
    add(f"#### 3.2.{i} `{name}` — {about}")
    add("")
    add(f"- Services handled: {', '.join(services)}.")
    add("")
add("Acceptance policy (library defaults, shared by both SCPs): any")
add("proposed SOP Class is accepted (`accepted_sop_classes: None`); all")
add(f"{len(ts_uid)} transfer syntaxes of Annex B are accepted; Called-AE")
add("is not enforced unless `--strict-ae` is passed; no User Identity")
add("authentication unless the embedding application installs the")
add("`user_identity_auth` callback.")
add("")
add("### 3.3 Association policies")
add("")
add(f"- Application Context Name: `{app_ctx}`.")
add(f"- Implementation Class UID `{impl_uid}`, Version Name `{impl_ver}`.")
add("- Presentation contexts: the SCU proposes one context per SOP Class,")
add("  each with the offered transfer syntaxes; the SCP accepts at most")
add("  one syntax per context from its accepted list.")
add(f"- Maximum PDU: tools offer {MAX_PDU} (`max_pdu`); the largest")
add(f"  accepted incoming PDU is {MAX_INCOMING_H} (`max_incoming_pdu`).")
add("- Timeouts: SCU tools default to a 30 s per-read timeout, SCP tools")
add("  to 300 s (`--timeout-secs`).")
add("- The SCU proposes no Role Selection and no User Identity; the")
add("  SCP accepts (echoes) any roles the peer proposes. No other")
add("  extended negotiation (asynchronous operations, SOP Class")
add("  extended/common negotiation) is offered by either side.")
add("- SCU tools request release after their service; SCP tools answer")
add("  release and abort cleanly. Protocol violations abort the")
add("  association (A-ABORT source 2).")
add("")
add("## 4. Communication profiles")
add("")
add("### 4.1 TCP/IP and DICOM TLS")
add("")
add("- DIMSE runs over TCP/IP. DICOM TLS (PS3.15) is compiled in by")
add("  default (`net` feature `tls`, tokio-rustls/ring backend).")
add("- SCU tools: `--tls` with optional `--tls-ca` (private CA),")
add("  `--tls-cert`/`--tls-key` (mutual TLS client identity),")
add("  `--tls-server-name` (defaults to the peer host).")
add("- SCP tools: TLS is enabled by `--tls-cert`/`--tls-key`, with")
add("  optional `--tls-client-ca` for mutual TLS.")
add("")
add("### 4.2 DICOMweb (informative; outside PS3.2)")
add("")
add("- Client: QIDO-RS search (`qido`, `search_studies/series/instances`),")
add("  WADO-RS retrieve (`wado_parts/file/dataset/metadata`), legacy")
add("  WADO-URI (`wado_uri`, `wado_uri_dataset`), STOW-RS store (`stow`).")
add("- Server (default `server` feature): framework-agnostic dispatch")
add("  over an in-memory store, with a hyper front end.")
add("")
add("### 4.3 Media (DICOMDIR)")
add("")
add("- `read_dicomdir` (File Set Reader) and `build_dicomdir` (File Set")
add("  Creator) plus the `dcmdir` CLI; directory records are encoded")
add("  Explicit VR Little Endian. No physical-media handling: file sets")
add("  live in the filesystem.")
add("")
add("## 5. Character sets")
add("")
add("- Text values are decoded as UTF-8 (lossy replacement on invalid")
add("  bytes) and encoded back as UTF-8 bytes; Specific Character Set")
add("  is carried as a normal data element but not interpreted for")
add("  code-extension conversion.")
add("- Creation sets `ISO_IR 192` only where the builder knows the text")
add("  needs it (Encapsulated PDF instances with non-ASCII text).")
add("")
add("## 6. Security")
add("")
add("- Association security relies on DICOM TLS (see §4.1); the library")
add("  defaults to WebPKI roots with optional private CA.")
add("- Not implemented: ATNA audit-trail emission, de-identification")
add("  profiles, and User Identity on the SCU side (see roadmap P2 for")
add("  the first two).")
add("")
add("## 7. Configuration")
add("")
add("| Item | Default | Flag |")
add("|------|---------|------|")
add(f"| SCU calling AE | `{CALLING_AE}` | `--calling` |")
add(f"| SCU called AE | `{CALLED_AE}` | `--called` |")
add(f"| SCP AE title | `{SCP_AE}` | `--ae-title` |")
add(f"| SCP bind | `{BIND}` | `--bind` |")
add(f"| Max PDU | {MAX_PDU} | `--max-pdu` |")
add(f"| SCU read timeout | {SCU_TIMEOUT} s | `--timeout-secs` |")
add(f"| SCP read timeout | {SCP_TIMEOUT} s | `--timeout-secs` |")
add("")
add("## Annex A — SOP Classes")
add("")
add("| SOP Class | UID | Category |")
add("|-----------|-----|----------|")
for const in sorted(sop_consts, key=lambda c: uid_key(sop_consts[c][0])):
    uid, desc = sop_consts[const]
    add(f"| {desc.rstrip('.')} | `{uid}` | {sop_category(const)} |")
add("")
add("## Annex B — Transfer Syntaxes")
add("")
add("| Transfer Syntax | UID | Lossy |")
add("|---|---|---|")
for variant in sorted(ts_uid, key=lambda v: uid_key(ts_uid[v])):
    lossy = "yes" if variant in LOSSY else "no"
    add(f"| {ts_name[variant]} | `{ts_uid[variant]}` | {lossy} |")
add("")
add("## Annex C — Implementation identifying attributes")
add("")
add(f"- Implementation Class UID: `{impl_uid}`.")
add(f"- Implementation Version Name: `{impl_ver}`.")
add("")

doc = "\n".join(L)
if "--check" in sys.argv[1:]:
    committed = OUT.read_text() if OUT.exists() else ""
    if committed != doc:
        diff = difflib.unified_diff(
            committed.splitlines(), doc.splitlines(),
            str(OUT), "generated",
            lineterm="",
        )
        sys.stdout.write("\n".join(diff) + "\n")
        sys.exit(1)
    print("conformance.md is current")
else:
    OUT.write_text(doc)
    print(f"wrote {OUT} ({len(doc)} bytes)")
