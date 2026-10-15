//! Protocol Data Units (NEMA PS3.8 §9): association, data, release, abort.
//!
//! [`Pdu`] encodes/decodes the seven PDU types; [`read_pdu`] / [`write_pdu`]
//! frame them over any async byte stream.

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::{AeTitle, Error, Result};

/// Proposed presentation context (A-ASSOCIATE-RQ, item `0x20`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProposedContext {
    /// Odd context ID (`1..=255`).
    pub id: u8,
    /// Abstract syntax UID (SOP class).
    pub abstract_syntax: String,
    /// Offered transfer syntax UIDs in preference order.
    pub transfer_syntaxes: Vec<String>,
}

/// Accepted presentation context (A-ASSOCIATE-AC, item `0x21`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AcceptedContext {
    /// Context ID echoed from the request.
    pub id: u8,
    /// `0` = accepted, `1..=4` = rejected (reason).
    pub result: u8,
    /// Agreed transfer syntax (present when accepted).
    pub transfer_syntax: Option<String>,
}

/// SCP/SCU role selection sub-item (`0x54`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RoleSelection {
    /// SOP Class UID the roles apply to.
    pub sop_class_uid: String,
    /// Proposer acts as SCU.
    pub scu_role: bool,
    /// Proposer acts as SCP.
    pub scp_role: bool,
}

/// User Identity Negotiation (RQ item `0x58`).
#[derive(Clone, PartialEq, Eq)]
pub struct UserIdentity {
    /// Identity type: 1 username, 2 username+password, 3 Kerberos, 4 SAML.
    pub identity_type: u8,
    /// Request a positive server response.
    pub positive_response_requested: bool,
    /// Primary field (username / ticket / assertion).
    pub primary: Vec<u8>,
    /// Secondary field (password).
    pub secondary: Vec<u8>,
}

// Manual `Debug`: identity fields carry passwords, tickets, and SAML
// assertions, none of which may reach logs.
impl std::fmt::Debug for UserIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UserIdentity")
            .field("identity_type", &self.identity_type)
            .field(
                "positive_response_requested",
                &self.positive_response_requested,
            )
            .field("primary", &"<redacted>")
            .field("secondary", &"<redacted>")
            .finish()
    }
}

/// A-ASSOCIATE-RQ PDU (`0x01`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AssociateRq {
    /// Called (destination) AE title.
    pub called_ae: AeTitle,
    /// Calling (source) AE title.
    pub calling_ae: AeTitle,
    /// Application context UID.
    pub application_context: String,
    /// Proposed presentation contexts.
    pub presentation_contexts: Vec<ProposedContext>,
    /// Our max receive PDU length (`0` = no explicit limit).
    pub max_pdu_len: u32,
    /// Our implementation class UID.
    pub implementation_class_uid: String,
    /// Optional implementation version name.
    pub implementation_version: Option<String>,
    /// Proposed async window (None = synchronous only).
    pub async_ops: Option<(u16, u16)>,
    /// Proposed role selections.
    pub role_selection: Vec<RoleSelection>,
    /// Proposed user identity.
    pub user_identity: Option<UserIdentity>,
}

/// A-ASSOCIATE-AC PDU (`0x02`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AssociateAc {
    /// Echoed application context UID.
    pub application_context: String,
    /// Accepted/rejected presentation contexts.
    pub presentation_contexts: Vec<AcceptedContext>,
    /// Acceptor max receive PDU length (`0` = no explicit limit).
    pub max_pdu_len: u32,
    /// Acceptor implementation class UID.
    pub implementation_class_uid: String,
    /// Optional implementation version name.
    pub implementation_version: Option<String>,
    /// Agreed async window.
    pub async_ops: Option<(u16, u16)>,
    /// Agreed role selections.
    pub role_selection: Vec<RoleSelection>,
    /// Server user-identity response bytes.
    pub user_identity_response: Option<Vec<u8>>,
}

/// A-ASSOCIATE-RJ PDU (`0x03`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AssociateRj {
    /// `1` = rejected-permanent, `2` = rejected-transient.
    pub result: u8,
    /// `1` = user, `2` = provider (ACSE), `3` = provider (presentation).
    pub source: u8,
    /// Reason code (meaning depends on source).
    pub reason: u8,
}

/// One Presentation Data Value inside P-DATA-TF.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pdv {
    /// Presentation context ID.
    pub context_id: u8,
    /// True for DIMSE command PDVs, false for dataset PDVs.
    pub is_command: bool,
    /// True for the last fragment of a command/dataset.
    pub is_last: bool,
    /// Fragment bytes.
    pub data: Vec<u8>,
}

/// One decoded PDU.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Pdu {
    /// A-ASSOCIATE-RQ.
    AssociateRq(AssociateRq),
    /// A-ASSOCIATE-AC.
    AssociateAc(AssociateAc),
    /// A-ASSOCIATE-RJ.
    AssociateRj(AssociateRj),
    /// P-DATA-TF with one or more PDVs.
    Data(Vec<Pdv>),
    /// A-RELEASE-RQ.
    ReleaseRq,
    /// A-RELEASE-RSP.
    ReleaseRsp,
    /// A-ABORT.
    Abort {
        /// Abort source.
        source: u8,
        /// Abort reason.
        reason: u8,
    },
}

impl Pdu {
    /// Short PDU type name for diagnostics.
    #[must_use]
    pub fn name(&self) -> &'static str {
        match self {
            Pdu::AssociateRq(_) => "A-ASSOCIATE-RQ",
            Pdu::AssociateAc(_) => "A-ASSOCIATE-AC",
            Pdu::AssociateRj(_) => "A-ASSOCIATE-RJ",
            Pdu::Data(_) => "P-DATA-TF",
            Pdu::ReleaseRq => "A-RELEASE-RQ",
            Pdu::ReleaseRsp => "A-RELEASE-RSP",
            Pdu::Abort { .. } => "A-ABORT",
        }
    }
}

// -- framing ---------------------------------------------------------------

/// Read one PDU; `max_len` caps the payload (DoS guard).
pub async fn read_pdu<R: AsyncRead + Unpin>(r: &mut R, max_len: u32) -> Result<Pdu> {
    let mut hdr = [0u8; 6];
    r.read_exact(&mut hdr).await.map_err(map_eof)?;
    let (typ, len) = (hdr[0], u32::from_be_bytes([hdr[2], hdr[3], hdr[4], hdr[5]]));
    if len > max_len {
        return Err(Error::BadPdu(format!(
            "PDU length {len} exceeds cap {max_len}"
        )));
    }
    let mut payload = vec![0u8; len as usize];
    r.read_exact(&mut payload).await.map_err(map_eof)?;
    decode_pdu(typ, &payload)
}

fn map_eof(e: std::io::Error) -> Error {
    if e.kind() == std::io::ErrorKind::UnexpectedEof {
        Error::BadPdu("connection closed mid-PDU".into())
    } else {
        Error::Io(e)
    }
}

/// Write one PDU.
pub async fn write_pdu<W: AsyncWrite + Unpin>(w: &mut W, pdu: &Pdu) -> Result<()> {
    let (typ, payload) = encode_pdu(pdu)?;
    let len = u32::try_from(payload.len())
        .map_err(|_| Error::BadPdu(format!("PDU payload ({} bytes) exceeds u32", payload.len())))?;
    w.write_u8(typ).await?;
    w.write_u8(0).await?;
    w.write_u32(len).await?;
    w.write_all(&payload).await?;
    w.flush().await?;
    Ok(())
}

/// Write one PDU, failing with [`Error::Timeout`] past `timeout`: a
/// zero-window (or otherwise stalled) peer must not hold a send path
/// forever. All association send paths use this; bare [`write_pdu`]
/// remains for tests and single-shot peers.
pub async fn write_pdu_timeout<W: AsyncWrite + Unpin>(
    w: &mut W,
    pdu: &Pdu,
    timeout: std::time::Duration,
) -> Result<()> {
    tokio::time::timeout(timeout, write_pdu(w, pdu))
        .await
        .map_err(|_| Error::Timeout("PDU write"))?
}

// -- PDU encode/decode --------------------------------------------------------

pub(crate) fn decode_pdu(typ: u8, p: &[u8]) -> Result<Pdu> {
    match typ {
        0x01 => Ok(Pdu::AssociateRq(decode_assoc_rq(p)?)),
        0x02 => Ok(Pdu::AssociateAc(decode_assoc_ac(p)?)),
        0x03 => {
            if p.len() != 4 {
                return Err(Error::BadPdu("A-ASSOCIATE-RJ must be 4 bytes".into()));
            }
            Ok(Pdu::AssociateRj(AssociateRj {
                result: p[1],
                source: p[2],
                reason: p[3],
            }))
        }
        0x04 => Ok(Pdu::Data(decode_data(p)?)),
        0x05 => {
            check_len(p, 4, "A-RELEASE-RQ")?;
            Ok(Pdu::ReleaseRq)
        }
        0x06 => {
            check_len(p, 4, "A-RELEASE-RSP")?;
            Ok(Pdu::ReleaseRsp)
        }
        0x07 => {
            if p.len() != 4 {
                return Err(Error::BadPdu("A-ABORT must be 4 bytes".into()));
            }
            Ok(Pdu::Abort {
                source: p[2],
                reason: p[3],
            })
        }
        _ => Err(Error::BadPdu(format!("unknown PDU type {typ:#04X}"))),
    }
}

fn check_len(p: &[u8], want: usize, what: &str) -> Result<()> {
    if p.len() != want {
        return Err(Error::BadPdu(format!("{what} must be {want} bytes")));
    }
    Ok(())
}

fn encode_pdu(pdu: &Pdu) -> Result<(u8, Vec<u8>)> {
    match pdu {
        Pdu::AssociateRq(rq) => Ok((0x01, encode_assoc_rq(rq)?)),
        Pdu::AssociateAc(ac) => Ok((0x02, encode_assoc_ac(ac)?)),
        Pdu::AssociateRj(rj) => Ok((0x03, vec![0, rj.result, rj.source, rj.reason])),
        Pdu::Data(pdvs) => {
            let mut out = Vec::new();
            for pdv in pdvs {
                let len = pdv.data.len() + 2;
                let len = u32::try_from(len).map_err(|_| {
                    Error::BadPdu(format!("PDV ({} bytes) exceeds u32", pdv.data.len()))
                })?;
                out.extend_from_slice(&len.to_be_bytes());
                out.push(pdv.context_id);
                out.push((pdv.is_command as u8) | ((pdv.is_last as u8) << 1));
                out.extend_from_slice(&pdv.data);
            }
            Ok((0x04, out))
        }
        Pdu::ReleaseRq => Ok((0x05, vec![0, 0, 0, 0])),
        Pdu::ReleaseRsp => Ok((0x06, vec![0, 0, 0, 0])),
        Pdu::Abort { source, reason } => Ok((0x07, vec![0, 0, *source, *reason])),
    }
}

/// Maximum PDVs in one P-DATA-TF: the byte cap alone still allows
/// millions of minimum-size PDVs (each a `Vec` + struct, ~5x wire
/// bytes in heap). Conformant senders emit a handful per PDU.
pub const MAX_PDVS_PER_PDU: usize = 4096;

/// Maximum sub-items decoded at one PDU item level (association RQ/AC
/// items, presentation-context or user-info sub-items): same
/// count-amplification guard as [`MAX_PDVS_PER_PDU`]. Legitimate
/// associations carry ~10^2 items per level at most.
pub const MAX_PDU_ITEMS: usize = 1024;

fn decode_data(p: &[u8]) -> Result<Vec<Pdv>> {
    let mut pdvs = Vec::new();
    let mut pos = 0;
    while pos < p.len() {
        if pdvs.len() >= MAX_PDVS_PER_PDU {
            return Err(Error::BadPdu(format!(
                "P-DATA-TF exceeds {MAX_PDVS_PER_PDU} PDVs"
            )));
        }
        if pos + 6 > p.len() {
            return Err(Error::BadPdu("truncated PDV header".into()));
        }
        let len = u32::from_be_bytes([p[pos], p[pos + 1], p[pos + 2], p[pos + 3]]) as usize;
        let (ctx, ctrl) = (p[pos + 4], p[pos + 5]);
        // Checked: a u32 wire length can wrap the end offset on 32-bit.
        let end = pos
            .checked_add(4)
            .and_then(|e| e.checked_add(len))
            .filter(|&e| e <= p.len());
        let Some(end) = end else {
            return Err(Error::BadPdu("PDV length out of range".into()));
        };
        if len < 2 {
            return Err(Error::BadPdu("PDV length out of range".into()));
        }
        pdvs.push(Pdv {
            context_id: ctx,
            is_command: ctrl & 0x01 == 0x01,
            is_last: ctrl & 0x02 == 0x02,
            data: p[pos + 6..end].to_vec(),
        });
        pos = end;
    }
    if pdvs.is_empty() {
        return Err(Error::BadPdu("P-DATA-TF without PDVs".into()));
    }
    Ok(pdvs)
}

// -- association PDUs --------------------------------------------------------

struct Items<'a> {
    buf: &'a [u8],
    pos: usize,
    count: usize,
}

impl<'a> Items<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Self {
            buf,
            pos: 0,
            count: 0,
        }
    }

    fn next(&mut self) -> Result<Option<(u8, &'a [u8])>> {
        if self.pos == self.buf.len() {
            return Ok(None);
        }
        if self.count >= MAX_PDU_ITEMS {
            return Err(Error::BadPdu(format!(
                "PDU item level exceeds {MAX_PDU_ITEMS} items"
            )));
        }
        self.count += 1;
        if self.pos + 4 > self.buf.len() {
            return Err(Error::BadPdu("truncated item header".into()));
        }
        let (typ, len) = (
            self.buf[self.pos],
            u16::from_be_bytes([self.buf[self.pos + 2], self.buf[self.pos + 3]]) as usize,
        );
        if self.pos + 4 + len > self.buf.len() {
            return Err(Error::BadPdu("item length out of range".into()));
        }
        let v = &self.buf[self.pos + 4..self.pos + 4 + len];
        self.pos += 4 + len;
        Ok(Some((typ, v)))
    }
}

fn uid_str(bytes: &[u8]) -> Result<String> {
    let s = std::str::from_utf8(bytes).map_err(|_| Error::BadPdu("UID not ASCII".into()))?;
    Ok(s.trim().trim_end_matches('\0').to_string())
}

fn decode_assoc_rq(p: &[u8]) -> Result<AssociateRq> {
    if p.len() < 68 {
        return Err(Error::BadPdu("A-ASSOCIATE-RQ too short".into()));
    }
    let version = u16::from_be_bytes([p[0], p[1]]);
    if version != 1 {
        return Err(Error::BadPdu(format!("protocol version {version}, want 1")));
    }
    let called_ae = AeTitle::from_wire(p[4..20].try_into().unwrap())?;
    let calling_ae = AeTitle::from_wire(p[20..36].try_into().unwrap())?;

    let mut rq = AssociateRq {
        called_ae,
        calling_ae,
        application_context: String::new(),
        presentation_contexts: Vec::new(),
        max_pdu_len: 0,
        implementation_class_uid: String::new(),
        implementation_version: None,
        async_ops: None,
        role_selection: Vec::new(),
        user_identity: None,
    };
    let mut items = Items::new(&p[68..]);
    while let Some((typ, v)) = items.next()? {
        match typ {
            0x10 => rq.application_context = uid_str(v)?,
            0x20 => rq.presentation_contexts.push(decode_pc_rq(v)?),
            0x50 => decode_user_info_rq(v, &mut rq)?,
            _ => {} // forward-compatible: skip unknown items
        }
    }
    if rq.application_context.is_empty() {
        return Err(Error::BadPdu(
            "A-ASSOCIATE-RQ without application context".into(),
        ));
    }
    Ok(rq)
}

fn decode_assoc_ac(p: &[u8]) -> Result<AssociateAc> {
    if p.len() < 68 {
        return Err(Error::BadPdu("A-ASSOCIATE-AC too short".into()));
    }
    let version = u16::from_be_bytes([p[0], p[1]]);
    if version != 1 {
        return Err(Error::BadPdu(format!("protocol version {version}, want 1")));
    }
    let mut ac = AssociateAc {
        application_context: String::new(),
        presentation_contexts: Vec::new(),
        max_pdu_len: 0,
        implementation_class_uid: String::new(),
        implementation_version: None,
        async_ops: None,
        role_selection: Vec::new(),
        user_identity_response: None,
    };
    let mut items = Items::new(&p[68..]);
    while let Some((typ, v)) = items.next()? {
        match typ {
            0x10 => ac.application_context = uid_str(v)?,
            0x21 => ac.presentation_contexts.push(decode_pc_ac(v)?),
            0x50 => decode_user_info_ac(v, &mut ac)?,
            _ => {}
        }
    }
    if ac.application_context.is_empty() {
        return Err(Error::BadPdu(
            "A-ASSOCIATE-AC without application context".into(),
        ));
    }
    Ok(ac)
}

fn decode_pc_rq(v: &[u8]) -> Result<ProposedContext> {
    if v.len() < 4 {
        return Err(Error::BadPdu("presentation context RQ too short".into()));
    }
    let id = v[0];
    let mut abstract_syntax = String::new();
    let mut transfer_syntaxes = Vec::new();
    let mut sub = Items::new(&v[4..]);
    while let Some((typ, sv)) = sub.next()? {
        match typ {
            0x30 => abstract_syntax = uid_str(sv)?,
            0x40 => transfer_syntaxes.push(uid_str(sv)?),
            _ => {}
        }
    }
    if abstract_syntax.is_empty() || transfer_syntaxes.is_empty() {
        return Err(Error::BadPdu(
            "presentation context without syntaxes".into(),
        ));
    }
    Ok(ProposedContext {
        id,
        abstract_syntax,
        transfer_syntaxes,
    })
}

fn decode_pc_ac(v: &[u8]) -> Result<AcceptedContext> {
    if v.len() < 4 {
        return Err(Error::BadPdu("presentation context AC too short".into()));
    }
    let (id, result) = (v[0], v[1]);
    let mut transfer_syntax = None;
    let mut sub = Items::new(&v[4..]);
    while let Some((typ, sv)) = sub.next()? {
        if typ == 0x40 {
            transfer_syntax = Some(uid_str(sv)?);
        }
    }
    Ok(AcceptedContext {
        id,
        result,
        transfer_syntax,
    })
}

fn decode_user_info_rq(v: &[u8], rq: &mut AssociateRq) -> Result<()> {
    let mut sub = Items::new(v);
    while let Some((typ, sv)) = sub.next()? {
        match typ {
            0x51 => {
                if sv.len() != 4 {
                    return Err(Error::BadPdu("max PDU length item must be 4 bytes".into()));
                }
                rq.max_pdu_len = u32::from_be_bytes([sv[0], sv[1], sv[2], sv[3]]);
            }
            0x52 => rq.implementation_class_uid = uid_str(sv)?,
            0x53 => {
                if sv.len() != 4 {
                    return Err(Error::BadPdu("async ops item must be 4 bytes".into()));
                }
                rq.async_ops = Some((
                    u16::from_be_bytes([sv[0], sv[1]]),
                    u16::from_be_bytes([sv[2], sv[3]]),
                ));
            }
            0x54 => rq.role_selection.push(decode_role(sv)?),
            0x55 => {
                rq.implementation_version =
                    Some(std::str::from_utf8(sv).unwrap_or("").trim().to_string())
            }
            0x58 => rq.user_identity = Some(decode_user_identity(sv)?),
            _ => {}
        }
    }
    Ok(())
}

fn decode_user_info_ac(v: &[u8], ac: &mut AssociateAc) -> Result<()> {
    let mut sub = Items::new(v);
    while let Some((typ, sv)) = sub.next()? {
        match typ {
            0x51 => {
                if sv.len() != 4 {
                    return Err(Error::BadPdu("max PDU length item must be 4 bytes".into()));
                }
                ac.max_pdu_len = u32::from_be_bytes([sv[0], sv[1], sv[2], sv[3]]);
            }
            0x52 => ac.implementation_class_uid = uid_str(sv)?,
            0x53 => {
                if sv.len() != 4 {
                    return Err(Error::BadPdu("async ops item must be 4 bytes".into()));
                }
                ac.async_ops = Some((
                    u16::from_be_bytes([sv[0], sv[1]]),
                    u16::from_be_bytes([sv[2], sv[3]]),
                ));
            }
            0x54 => ac.role_selection.push(decode_role(sv)?),
            0x55 => {
                ac.implementation_version =
                    Some(std::str::from_utf8(sv).unwrap_or("").trim().to_string())
            }
            0x58 => {
                if sv.len() < 4 {
                    return Err(Error::BadPdu("user identity AC too short".into()));
                }
                let len = u16::from_be_bytes([sv[0], sv[1]]) as usize;
                if 2 + len > sv.len() {
                    return Err(Error::BadPdu("user identity response out of range".into()));
                }
                ac.user_identity_response = Some(sv[2..2 + len].to_vec());
            }
            _ => {}
        }
    }
    Ok(())
}

fn decode_role(sv: &[u8]) -> Result<RoleSelection> {
    if sv.len() < 4 {
        return Err(Error::BadPdu("role selection too short".into()));
    }
    let len = u16::from_be_bytes([sv[0], sv[1]]) as usize;
    if 2 + len + 2 > sv.len() {
        return Err(Error::BadPdu("role selection UID out of range".into()));
    }
    Ok(RoleSelection {
        sop_class_uid: uid_str(&sv[2..2 + len])?,
        scu_role: sv[2 + len] != 0,
        scp_role: sv[3 + len] != 0,
    })
}

fn decode_user_identity(sv: &[u8]) -> Result<UserIdentity> {
    if sv.len() < 6 {
        return Err(Error::BadPdu("user identity RQ too short".into()));
    }
    let (identity_type, positive) = (sv[0], sv[1] != 0);
    let l1 = u16::from_be_bytes([sv[2], sv[3]]) as usize;
    if 4 + l1 + 2 > sv.len() {
        return Err(Error::BadPdu("user identity primary out of range".into()));
    }
    let l2 = u16::from_be_bytes([sv[4 + l1], sv[5 + l1]]) as usize;
    if 6 + l1 + l2 > sv.len() {
        return Err(Error::BadPdu("user identity secondary out of range".into()));
    }
    Ok(UserIdentity {
        identity_type,
        positive_response_requested: positive,
        primary: sv[4..4 + l1].to_vec(),
        secondary: sv[6 + l1..6 + l1 + l2].to_vec(),
    })
}

// -- encode ---------------------------------------------------------------

/// A PDU item length field: oversized values error out rather than
/// being silently truncated onto the wire by `as u16`.
fn item_len(len: usize, what: &'static str) -> Result<u16> {
    u16::try_from(len).map_err(|_| Error::BadPdu(format!("{what} exceeds 65535 bytes")))
}

fn put_item(out: &mut Vec<u8>, typ: u8, value: &[u8]) -> Result<()> {
    out.push(typ);
    out.push(0);
    out.extend_from_slice(&item_len(value.len(), "PDU item")?.to_be_bytes());
    out.extend_from_slice(value);
    Ok(())
}

fn encode_assoc_rq(rq: &AssociateRq) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    out.extend_from_slice(&1u16.to_be_bytes());
    out.extend_from_slice(&[0, 0]);
    out.extend_from_slice(&rq.called_ae.to_wire());
    out.extend_from_slice(&rq.calling_ae.to_wire());
    out.extend_from_slice(&[0u8; 32]);
    put_item(&mut out, 0x10, rq.application_context.as_bytes())?;
    for pc in &rq.presentation_contexts {
        let mut v = vec![pc.id, 0, 0, 0];
        put_item(&mut v, 0x30, pc.abstract_syntax.as_bytes())?;
        for ts in &pc.transfer_syntaxes {
            put_item(&mut v, 0x40, ts.as_bytes())?;
        }
        put_item(&mut out, 0x20, &v)?;
    }
    put_item(&mut out, 0x50, &encode_user_info_rq(rq)?)?;
    Ok(out)
}

fn encode_user_info_rq(rq: &AssociateRq) -> Result<Vec<u8>> {
    let mut u = Vec::new();
    put_item(&mut u, 0x51, &rq.max_pdu_len.to_be_bytes())?;
    put_item(&mut u, 0x52, rq.implementation_class_uid.as_bytes())?;
    if let Some(v) = &rq.implementation_version {
        put_item(&mut u, 0x55, v.as_bytes())?;
    }
    if let Some((inv, perf)) = rq.async_ops {
        let mut v = Vec::new();
        v.extend_from_slice(&inv.to_be_bytes());
        v.extend_from_slice(&perf.to_be_bytes());
        put_item(&mut u, 0x53, &v)?;
    }
    for r in &rq.role_selection {
        put_item(&mut u, 0x54, &encode_role(r)?)?;
    }
    if let Some(id) = &rq.user_identity {
        let mut v = vec![id.identity_type, id.positive_response_requested as u8];
        v.extend_from_slice(&item_len(id.primary.len(), "user identity primary")?.to_be_bytes());
        v.extend_from_slice(&id.primary);
        v.extend_from_slice(
            &item_len(id.secondary.len(), "user identity secondary")?.to_be_bytes(),
        );
        v.extend_from_slice(&id.secondary);
        put_item(&mut u, 0x58, &v)?;
    }
    Ok(u)
}

fn encode_role(r: &RoleSelection) -> Result<Vec<u8>> {
    let mut v = Vec::new();
    v.extend_from_slice(&item_len(r.sop_class_uid.len(), "role selection UID")?.to_be_bytes());
    v.extend_from_slice(r.sop_class_uid.as_bytes());
    v.push(r.scu_role as u8);
    v.push(r.scp_role as u8);
    Ok(v)
}

fn encode_assoc_ac(ac: &AssociateAc) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    out.extend_from_slice(&1u16.to_be_bytes());
    out.extend_from_slice(&[0, 0]);
    out.extend_from_slice(&[0u8; 64]); // AE-sized + trailing fields are reserved in AC
    put_item(&mut out, 0x10, ac.application_context.as_bytes())?;
    for pc in &ac.presentation_contexts {
        let mut v = vec![pc.id, pc.result, 0, 0];
        if let Some(ts) = &pc.transfer_syntax {
            put_item(&mut v, 0x40, ts.as_bytes())?;
        }
        put_item(&mut out, 0x21, &v)?;
    }
    let mut u = Vec::new();
    put_item(&mut u, 0x51, &ac.max_pdu_len.to_be_bytes())?;
    put_item(&mut u, 0x52, ac.implementation_class_uid.as_bytes())?;
    if let Some(v) = &ac.implementation_version {
        put_item(&mut u, 0x55, v.as_bytes())?;
    }
    if let Some((inv, perf)) = ac.async_ops {
        let mut v = Vec::new();
        v.extend_from_slice(&inv.to_be_bytes());
        v.extend_from_slice(&perf.to_be_bytes());
        put_item(&mut u, 0x53, &v)?;
    }
    for r in &ac.role_selection {
        put_item(&mut u, 0x54, &encode_role(r)?)?;
    }
    if let Some(resp) = &ac.user_identity_response {
        let mut v = Vec::new();
        v.extend_from_slice(&item_len(resp.len(), "user identity response")?.to_be_bytes());
        v.extend_from_slice(resp);
        put_item(&mut u, 0x58, &v)?;
    }
    put_item(&mut out, 0x50, &u)?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_rq() -> AssociateRq {
        AssociateRq {
            called_ae: AeTitle::new("CALLED").unwrap(),
            calling_ae: AeTitle::new("CALLING").unwrap(),
            application_context: "1.2.840.10008.3.1.1.1".into(),
            presentation_contexts: vec![ProposedContext {
                id: 1,
                abstract_syntax: "1.2.840.10008.1.1".into(),
                transfer_syntaxes: vec!["1.2.840.10008.1.2.1".into(), "1.2.840.10008.1.2".into()],
            }],
            max_pdu_len: 16384,
            implementation_class_uid: "1.2.3".into(),
            implementation_version: Some("V1".into()),
            async_ops: None,
            role_selection: vec![RoleSelection {
                sop_class_uid: "1.2.840.10008.1.1".into(),
                scu_role: true,
                scp_role: false,
            }],
            user_identity: Some(UserIdentity {
                identity_type: 1,
                positive_response_requested: true,
                primary: b"user".to_vec(),
                secondary: vec![],
            }),
        }
    }

    #[test]
    fn assoc_rq_round_trip() {
        let rq = sample_rq();
        let (_, bytes) = encode_pdu(&Pdu::AssociateRq(rq.clone())).unwrap();
        let back = decode_pdu(0x01, &bytes).unwrap();
        assert_eq!(back, Pdu::AssociateRq(rq));
    }

    #[test]
    fn oversized_items_error_instead_of_truncating() {
        assert!(item_len(65535, "test").is_ok());
        assert!(item_len(65536, "test").is_err());
        let mut rq = sample_rq();
        rq.presentation_contexts[0].abstract_syntax = "1.".repeat(40000);
        assert!(encode_pdu(&Pdu::AssociateRq(rq)).is_err());
    }

    #[test]
    fn assoc_ac_round_trip() {
        let ac = AssociateAc {
            application_context: "1.2.840.10008.3.1.1.1".into(),
            presentation_contexts: vec![
                AcceptedContext {
                    id: 1,
                    result: 0,
                    transfer_syntax: Some("1.2.840.10008.1.2.1".into()),
                },
                AcceptedContext {
                    id: 3,
                    result: 3,
                    transfer_syntax: None,
                },
            ],
            max_pdu_len: 32768,
            implementation_class_uid: "1.2.3".into(),
            implementation_version: None,
            async_ops: Some((1, 1)),
            role_selection: vec![],
            user_identity_response: Some(b"ok".to_vec()),
        };
        let (_, bytes) = encode_pdu(&Pdu::AssociateAc(ac.clone())).unwrap();
        assert_eq!(decode_pdu(0x02, &bytes).unwrap(), Pdu::AssociateAc(ac));
    }

    #[test]
    fn data_and_control_pdus() {
        let pdvs = vec![
            Pdv {
                context_id: 1,
                is_command: true,
                is_last: true,
                data: vec![1, 2],
            },
            Pdv {
                context_id: 1,
                is_command: false,
                is_last: false,
                data: vec![3],
            },
        ];
        let (_, bytes) = encode_pdu(&Pdu::Data(pdvs.clone())).unwrap();
        // Control bytes: command+last = 0x03, data+more = 0x00.
        assert_eq!(bytes[5], 0x03);
        assert_eq!(decode_pdu(0x04, &bytes).unwrap(), Pdu::Data(pdvs));

        for (typ, pdu) in [
            (
                0x03,
                Pdu::AssociateRj(AssociateRj {
                    result: 1,
                    source: 1,
                    reason: 2,
                }),
            ),
            (0x05, Pdu::ReleaseRq),
            (0x06, Pdu::ReleaseRsp),
            (
                0x07,
                Pdu::Abort {
                    source: 2,
                    reason: 0,
                },
            ),
        ] {
            let (t, bytes) = encode_pdu(&pdu).unwrap();
            assert_eq!(t, typ);
            assert_eq!(decode_pdu(typ, &bytes).unwrap(), pdu);
        }
    }

    #[test]
    fn rejects_malformed() {
        assert!(decode_pdu(0x09, &[]).is_err());
        assert!(decode_pdu(0x04, &[0, 0, 0]).is_err());
        assert!(decode_pdu(0x01, &[0, 1]).is_err());
        // Unknown *items* are skipped, not fatal.
        let rq = sample_rq();
        let (_, mut bytes) = encode_pdu(&Pdu::AssociateRq(rq.clone())).unwrap();
        bytes.extend([0x99, 0x00, 0x00, 0x02, 0xAA, 0xBB]);
        assert_eq!(decode_pdu(0x01, &bytes).unwrap(), Pdu::AssociateRq(rq));
    }

    #[test]
    fn debug_redacts_user_identity() {
        let id = UserIdentity {
            identity_type: 2,
            positive_response_requested: true,
            primary: b"ann".to_vec(),
            secondary: b"hunter2".to_vec(),
        };
        let shown = format!("{id:?}");
        assert!(!shown.contains("hunter2"), "{shown}");
        assert!(!shown.contains("ann"), "{shown}");
    }

    #[tokio::test]
    async fn adversarial_pdu_length_rejected_before_alloc() {
        // A header claiming u32::MAX payload bytes with (almost) no
        // bytes behind it must fail on the cap, not on a 4 GiB
        // allocation: this test would OOM or hang otherwise.
        let wire = [0x04u8, 0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0x00];
        let mut r = &wire[..];
        let err = read_pdu(&mut r, 64 * 1024 * 1024).await.unwrap_err();
        assert!(err.to_string().contains("exceeds cap"), "{err}");
    }

    #[test]
    fn item_counts_are_capped() {
        // 6-byte minimum-size PDVs: the byte cap alone would admit
        // millions (~5x wire bytes in heap Vecs).
        let mut payload = Vec::new();
        for _ in 0..=MAX_PDVS_PER_PDU {
            payload.extend_from_slice(&2u32.to_be_bytes());
            payload.extend([1u8, 0x01]);
        }
        let err = decode_pdu(0x04, &payload).unwrap_err();
        assert!(err.to_string().contains("PDVs"), "{err}");
        // Same for association item levels: 4-byte empty items past
        // the fixed 68-byte header (unknown types are skipped, but
        // still counted).
        let mut rq = vec![0, 1, 0, 0];
        rq.extend(b"CALLED          ");
        rq.extend(b"CALLING         ");
        rq.extend([0u8; 32]);
        for _ in 0..=MAX_PDU_ITEMS {
            rq.extend([0x09, 0x00, 0x00, 0x00]);
        }
        let err = decode_pdu(0x01, &rq).unwrap_err();
        assert!(err.to_string().contains("items"), "{err}");
    }
}
