//! Associations: the connected DIMSE conversation (NEMA PS3.8 §9, PS3.7).
//!
//! [`Association`] owns a framed byte stream (plain TCP or TLS) plus the
//! negotiated presentation contexts, and sends/receives [`DimseMessage`]s
//! with P-DATA-TF fragmentation. SCU operations live in [`crate::service`]
//! as `impl Association` methods; the SCP dispatch loop is
//! [`crate::service::run_scp`].

use std::collections::VecDeque;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;
use tokio::time::timeout;
use vyna_dcm_codec::{decode_dataset, encode_dataset};
use vyna_dcm_core::{APPLICATION_CONTEXT_NAME, TransferSyntax};

use crate::{
    AeTitle, Command, Error, Result,
    dimse::{DimseMessage, decode_command, encode_command},
    pdu::{AssociateAc, AssociateRq, Pdu, Pdv, ProposedContext},
};

/// Trait alias for a connected byte stream.
pub trait AsyncReadWrite: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> AsyncReadWrite for T {}

/// Options for requesting (SCU) or accepting (SCP) associations.
#[derive(Clone, Debug)]
pub struct AssociationOptions {
    /// Our max receive PDU length, declared to the peer.
    pub max_pdu_len: u32,
    /// Per-read timeout.
    pub read_timeout: Duration,
    /// Our implementation class UID.
    pub implementation_class_uid: String,
    /// Our implementation version name.
    pub implementation_version: Option<String>,
    /// Application context (almost always the DICOM one).
    pub application_context: String,
    /// Hard cap for any incoming PDU (DoS guard).
    pub max_incoming_pdu: u32,
    /// Hard cap for one reassembled DIMSE command or dataset across
    /// all its PDVs (DoS guard; the per-PDU cap alone does not bound
    /// a many-PDU message).
    pub max_message_bytes: u64,
}

impl Default for AssociationOptions {
    fn default() -> Self {
        Self {
            max_pdu_len: 256 * 1024,
            read_timeout: Duration::from_secs(300),
            implementation_class_uid: vyna_dcm_core::VYNA_IMPLEMENTATION_CLASS_UID.to_string(),
            implementation_version: Some(
                vyna_dcm_core::VYNA_IMPLEMENTATION_VERSION_NAME.to_string(),
            ),
            application_context: APPLICATION_CONTEXT_NAME.to_string(),
            max_incoming_pdu: 64 * 1024 * 1024,
            max_message_bytes: 1024 * 1024 * 1024,
        }
    }
}

/// One negotiated presentation context.
#[derive(Clone, Debug)]
pub struct NegotiatedContext {
    /// Context ID.
    pub id: u8,
    /// Agreed abstract syntax (SOP class UID).
    pub abstract_syntax: String,
    /// Agreed transfer syntax.
    pub transfer_syntax: TransferSyntax,
    /// Agreed transfer syntax UID (verbatim).
    pub transfer_syntax_uid: String,
}

/// Snapshot of association identity for SCP handlers.
#[derive(Clone, Debug)]
pub struct AssociationInfo {
    /// Our AE title.
    pub local_ae: AeTitle,
    /// Peer's AE title.
    pub peer_ae: AeTitle,
    /// Peer's max receive PDU (0 = unlimited, we cap sends at 1 MiB).
    pub peer_max_pdu: u32,
}

/// What [`Association::recv`] can return.
#[derive(Debug)]
pub enum DimseEvent {
    /// A complete DIMSE message (boxed: the message is ~280 bytes).
    Message(Box<DimseMessage>),
    /// Peer asked to release (RELEASE-RSP already sent).
    Released,
}

/// A live association: stream + negotiated contexts + message IDs.
pub struct Association {
    stream: Box<dyn AsyncReadWrite>,
    /// Negotiated contexts.
    contexts: Vec<NegotiatedContext>,
    info: AssociationInfo,
    our_max_pdu: u32,
    max_incoming_pdu: u32,
    max_message_bytes: u64,
    read_timeout: Duration,
    next_id: u16,
    /// Pushback queue: events seen by a cancel poll that belong to the
    /// dispatch loop (e.g. RELEASE-RQ mid-FIND). [`Association::recv`]
    /// drains this before touching the wire.
    pending: VecDeque<DimseEvent>,
}

impl Association {
    /// Connect over TCP and request an association.
    pub async fn request(
        opts: &AssociationOptions,
        calling_ae: &AeTitle,
        called_ae: &AeTitle,
        addr: &str,
        proposed: Vec<ProposedContext>,
    ) -> Result<Self> {
        let stream = TcpStream::connect(addr).await?;
        // DIMSE is small-write chatty; Nagle would stall each PDU on the
        // peer's delayed ACK (~40ms), as DCMTK/dcm4che also avoid.
        stream.set_nodelay(true)?;
        Self::request_over(opts, calling_ae, called_ae, stream, proposed).await
    }

    /// Request an association over an already-connected stream
    /// (e.g. a TLS session from [`crate::tls`]).
    pub async fn request_over<S>(
        opts: &AssociationOptions,
        calling_ae: &AeTitle,
        called_ae: &AeTitle,
        stream: S,
        proposed: Vec<ProposedContext>,
    ) -> Result<Self>
    where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        for pc in &proposed {
            if pc.id == 0 || pc.id % 2 == 0 {
                return Err(Error::BadPdu(format!(
                    "presentation context id {} must be odd and nonzero",
                    pc.id
                )));
            }
            if pc.transfer_syntaxes.is_empty() {
                return Err(Error::BadPdu(
                    "presentation context without transfer syntaxes".into(),
                ));
            }
        }
        let mut this = Self {
            stream: Box::new(stream),
            contexts: Vec::new(),
            info: AssociationInfo {
                local_ae: calling_ae.clone(),
                peer_ae: called_ae.clone(),
                peer_max_pdu: 0,
            },
            our_max_pdu: opts.max_pdu_len,
            max_incoming_pdu: opts.max_incoming_pdu,
            max_message_bytes: opts.max_message_bytes,
            read_timeout: opts.read_timeout,
            next_id: 1,
            pending: VecDeque::new(),
        };
        let rq = AssociateRq {
            called_ae: called_ae.clone(),
            calling_ae: calling_ae.clone(),
            application_context: opts.application_context.clone(),
            presentation_contexts: proposed.clone(),
            max_pdu_len: opts.max_pdu_len,
            implementation_class_uid: opts.implementation_class_uid.clone(),
            implementation_version: opts.implementation_version.clone(),
            async_ops: None,
            role_selection: Vec::new(),
            user_identity: None,
        };
        crate::pdu::write_pdu(&mut this.stream, &Pdu::AssociateRq(rq)).await?;
        match this.read_pdu().await? {
            Pdu::AssociateAc(ac) => this.adopt_ac(ac, &proposed)?,
            Pdu::AssociateRj(rj) => {
                return Err(Error::Rejected {
                    result: rj.result,
                    origin: rj.source,
                    reason: rj.reason,
                });
            }
            Pdu::Abort { source, reason } => {
                return Err(Error::Aborted {
                    origin: source,
                    reason,
                });
            }
            other => {
                return Err(Error::UnexpectedPdu {
                    expected: "A-ASSOCIATE-AC",
                    got: other.name().to_string(),
                });
            }
        }
        Ok(this)
    }

    /// Build the SCU side from an accepted AC (also used by tests).
    fn adopt_ac(&mut self, ac: AssociateAc, proposed: &[ProposedContext]) -> Result<()> {
        self.info.peer_max_pdu = ac.max_pdu_len;
        for acc in &ac.presentation_contexts {
            if acc.result != 0 {
                continue;
            }
            let Some(ts_uid) = &acc.transfer_syntax else {
                continue;
            };
            let Some(ts) = TransferSyntax::from_uid(ts_uid) else {
                continue;
            };
            let Some(prop) = proposed.iter().find(|p| p.id == acc.id) else {
                continue;
            };
            self.contexts.push(NegotiatedContext {
                id: acc.id,
                abstract_syntax: prop.abstract_syntax.clone(),
                transfer_syntax: ts,
                transfer_syntax_uid: ts_uid.clone(),
            });
        }
        if self.contexts.is_empty() {
            return Err(Error::BadPdu(
                "association accepted without usable contexts".into(),
            ));
        }
        Ok(())
    }

    /// Build the SCP side after the acceptor negotiated contexts.
    pub fn from_accepted(
        stream: Box<dyn AsyncReadWrite>,
        info: AssociationInfo,
        contexts: Vec<NegotiatedContext>,
        opts: &AssociationOptions,
    ) -> Self {
        Self {
            stream,
            contexts,
            info,
            our_max_pdu: opts.max_pdu_len,
            max_incoming_pdu: opts.max_incoming_pdu,
            max_message_bytes: opts.max_message_bytes,
            read_timeout: opts.read_timeout,
            next_id: 1,
            pending: VecDeque::new(),
        }
    }

    /// Negotiated contexts.
    #[must_use]
    pub fn contexts(&self) -> &[NegotiatedContext] {
        &self.contexts
    }

    /// Our max receive PDU length, as declared to the peer.
    #[must_use]
    pub fn our_max_pdu(&self) -> u32 {
        self.our_max_pdu
    }

    /// Association identity snapshot.
    #[must_use]
    pub fn info(&self) -> &AssociationInfo {
        &self.info
    }

    /// First context whose abstract syntax matches `sop_class_uid`.
    #[must_use]
    pub fn context_for(&self, sop_class_uid: &str) -> Option<&NegotiatedContext> {
        self.contexts
            .iter()
            .find(|c| c.abstract_syntax == sop_class_uid)
    }

    /// Context by ID.
    #[must_use]
    pub fn context_by_id(&self, id: u8) -> Option<&NegotiatedContext> {
        self.contexts.iter().find(|c| c.id == id)
    }

    /// Next outgoing message ID (wraps `1..=65535`, never 0).
    pub fn next_message_id(&mut self) -> u16 {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1).max(1);
        if self.next_id == 0 {
            self.next_id = 1;
        }
        id
    }

    /// Send one DIMSE message (command Implicit VR LE, dataset in the
    /// context's transfer syntax), fragmented to the peer's max PDU.
    pub async fn send(
        &mut self,
        context_id: u8,
        command: &Command,
        dataset: Option<&vyna_dcm_core::DataSet>,
    ) -> Result<()> {
        let ctx = self
            .context_by_id(context_id)
            .ok_or(Error::UnknownContext(context_id))?
            .clone();
        let cmd_bytes = encode_command(command)?;
        let data_bytes = match dataset {
            Some(ds) => Some(encode_dataset(ds, ctx.transfer_syntax)?),
            None => None,
        };
        // One PDV per P-DATA-TF keeps framing trivially correct.
        let send_max = self.send_max();
        let mut frags: Vec<(bool, Vec<u8>)> = Vec::new(); // (is_command, bytes)
        frags.push((true, cmd_bytes));
        if let Some(b) = data_bytes {
            frags.push((false, b));
        }
        for (is_command, bytes) in frags {
            let chunks = chunk_bytes(&bytes, send_max);
            let n = chunks.len();
            for (i, chunk) in chunks.into_iter().enumerate() {
                let pdv = Pdv {
                    context_id,
                    is_command,
                    is_last: i + 1 == n,
                    data: chunk,
                };
                crate::pdu::write_pdu(&mut self.stream, &Pdu::Data(vec![pdv])).await?;
            }
        }
        Ok(())
    }

    /// Largest P-DATA-TF payload we may emit (peer limit or 1 MiB).
    fn send_max(&self) -> usize {
        let cap = if self.info.peer_max_pdu == 0 {
            1024 * 1024
        } else {
            self.info.peer_max_pdu
        };
        (cap as usize).saturating_sub(12).max(64)
    }

    /// Receive one complete DIMSE message (reassembling PDVs), or a release.
    pub async fn recv(&mut self) -> Result<DimseEvent> {
        if let Some(ev) = self.pending.pop_front() {
            return Ok(ev);
        }
        let mut cmd_buf: Vec<u8> = Vec::new();
        let mut cmd_ctx: Option<u8> = None;
        let mut command: Option<Command> = None;
        let mut data_buf: Vec<u8> = Vec::new();
        loop {
            match self.read_pdu().await? {
                Pdu::Data(pdvs) => {
                    for pdv in pdvs {
                        let ctx = self
                            .context_by_id(pdv.context_id)
                            .ok_or(Error::UnknownContext(pdv.context_id))?
                            .clone();
                        if pdv.is_command {
                            if command.is_some() {
                                return Err(Error::BadPdu(
                                    "command PDV after command complete".into(),
                                ));
                            }
                            match cmd_ctx {
                                Some(id) if id != pdv.context_id => {
                                    return Err(Error::BadPdu("command PDVs span contexts".into()));
                                }
                                _ => cmd_ctx = Some(pdv.context_id),
                            }
                            if cmd_buf.len() as u64 + pdv.data.len() as u64 > self.max_message_bytes
                            {
                                return Err(Error::MessageTooLarge {
                                    limit: self.max_message_bytes,
                                });
                            }
                            cmd_buf.extend_from_slice(&pdv.data);
                            if pdv.is_last {
                                let cmd = decode_command(&cmd_buf)?;
                                if !cmd.has_dataset() {
                                    return Ok(DimseEvent::Message(Box::new(DimseMessage {
                                        context_id: pdv.context_id,
                                        command: cmd,
                                        dataset: None,
                                    })));
                                }
                                command = Some(cmd);
                            }
                        } else {
                            let Some(cmd) = &command else {
                                return Err(Error::BadPdu("dataset PDV before command".into()));
                            };
                            if Some(pdv.context_id) != cmd_ctx {
                                return Err(Error::BadPdu("dataset PDV on wrong context".into()));
                            }
                            if data_buf.len() as u64 + pdv.data.len() as u64
                                > self.max_message_bytes
                            {
                                return Err(Error::MessageTooLarge {
                                    limit: self.max_message_bytes,
                                });
                            }
                            data_buf.extend_from_slice(&pdv.data);
                            if pdv.is_last {
                                let ds = decode_dataset(&data_buf, ctx.transfer_syntax)?;
                                return Ok(DimseEvent::Message(Box::new(DimseMessage {
                                    context_id: pdv.context_id,
                                    command: command.take().unwrap(),
                                    dataset: Some(ds),
                                })));
                            }
                            let _ = cmd;
                        }
                    }
                }
                Pdu::ReleaseRq => {
                    crate::pdu::write_pdu(&mut self.stream, &Pdu::ReleaseRsp).await?;
                    return Ok(DimseEvent::Released);
                }
                Pdu::Abort { source, reason } => {
                    return Err(Error::Aborted {
                        origin: source,
                        reason,
                    });
                }
                other => {
                    return Err(Error::UnexpectedPdu {
                        expected: "P-DATA-TF",
                        got: other.name().to_string(),
                    });
                }
            }
        }
    }

    /// Receive one event, waiting at most `wait`; `Ok(None)` on timeout.
    ///
    /// Used to poll for C-CANCEL between pending responses. Timeout
    /// soundness: a timeout abandons a partial read, so callers must
    /// only poll while the peer can solely send small single-PDU
    /// messages (C-CANCEL is ~30 bytes); anything larger in flight
    /// risks association desync, which then fails loudly.
    pub async fn poll_event(&mut self, wait: Duration) -> Result<Option<DimseEvent>> {
        if let Some(ev) = self.pending.pop_front() {
            return Ok(Some(ev));
        }
        match timeout(wait, self.recv()).await {
            Ok(ev) => Ok(Some(ev?)),
            Err(_) => Ok(None),
        }
    }

    /// Return an event to the pushback queue (see [`Association::recv`]).
    pub fn stash(&mut self, ev: DimseEvent) {
        self.pending.push_back(ev);
    }

    /// Clean release: RELEASE-RQ then await RELEASE-RSP.
    pub async fn release(&mut self) -> Result<()> {
        crate::pdu::write_pdu(&mut self.stream, &Pdu::ReleaseRq).await?;
        match self.read_pdu().await? {
            Pdu::ReleaseRsp => Ok(()),
            Pdu::Abort { source, reason } => Err(Error::Aborted {
                origin: source,
                reason,
            }),
            other => Err(Error::UnexpectedPdu {
                expected: "A-RELEASE-RSP",
                got: other.name().to_string(),
            }),
        }
    }

    /// Send A-ABORT (best effort; errors swallowed except I/O).
    pub async fn abort(&mut self, source: u8, reason: u8) -> Result<()> {
        crate::pdu::write_pdu(&mut self.stream, &Pdu::Abort { source, reason }).await
    }

    async fn read_pdu(&mut self) -> Result<Pdu> {
        timeout(
            self.read_timeout,
            crate::pdu::read_pdu(&mut self.stream, self.max_incoming_pdu),
        )
        .await
        .map_err(|_| Error::Timeout("PDU read"))?
    }
}

/// Split bytes into PDV payloads of at most `max` bytes (at least one).
fn chunk_bytes(bytes: &[u8], max: usize) -> Vec<Vec<u8>> {
    if bytes.is_empty() {
        return vec![Vec::new()];
    }
    bytes.chunks(max.max(1)).map(|c| c.to_vec()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_ids_skip_zero_on_wrap() {
        let opts = AssociationOptions::default();
        let (a, _b) = tokio::io::duplex(64);
        let mut assoc = Association {
            stream: Box::new(a),
            contexts: vec![],
            info: AssociationInfo {
                local_ae: AeTitle::new("A").unwrap(),
                peer_ae: AeTitle::new("B").unwrap(),
                peer_max_pdu: 0,
            },
            our_max_pdu: opts.max_pdu_len,
            max_incoming_pdu: opts.max_incoming_pdu,
            max_message_bytes: opts.max_message_bytes,
            read_timeout: opts.read_timeout,
            next_id: u16::MAX,
            pending: VecDeque::new(),
        };
        assert_eq!(assoc.next_message_id(), u16::MAX);
        assert_eq!(assoc.next_message_id(), 1);
    }

    #[test]
    fn chunking_rules() {
        assert_eq!(chunk_bytes(&[], 10), vec![Vec::<u8>::new()]);
        assert_eq!(chunk_bytes(&[1, 2, 3], 10), vec![vec![1, 2, 3]]);
        assert_eq!(chunk_bytes(&[1, 2, 3, 4], 2), vec![vec![1, 2], vec![3, 4]]);
    }

    #[tokio::test]
    async fn recv_enforces_total_message_cap() {
        use crate::dimse::priority;
        // Loopback pair: the sender fragments into ~52-byte PDVs
        // (small peer max PDU) while the receiver allows 256 bytes
        // total, so a 308-byte dataset must fail even though every
        // single PDU is far below the per-PDU cap.
        let sender_opts = AssociationOptions::default();
        let receiver_opts = AssociationOptions {
            max_message_bytes: 256,
            ..AssociationOptions::default()
        };
        let (a, b) = tokio::io::duplex(65536);
        let ctx = || NegotiatedContext {
            id: 1,
            abstract_syntax: vyna_dcm_core::sop_class::CT_IMAGE.to_string(),
            transfer_syntax: TransferSyntax::ExplicitVrLittleEndian,
            transfer_syntax_uid: TransferSyntax::ExplicitVrLittleEndian.uid().to_string(),
        };
        let info = |local: &str, peer: &str| AssociationInfo {
            local_ae: AeTitle::new(local).unwrap(),
            peer_ae: AeTitle::new(peer).unwrap(),
            peer_max_pdu: 0,
        };
        let mut sender =
            Association::from_accepted(Box::new(a), info("SCU", "SCP"), vec![ctx()], &sender_opts);
        sender.info.peer_max_pdu = 64;
        let mut receiver = Association::from_accepted(
            Box::new(b),
            info("SCP", "SCU"),
            vec![ctx()],
            &receiver_opts,
        );
        let mut ds = vyna_dcm_core::DataSet::new();
        ds.put_bytes(
            vyna_dcm_core::tags::PIXEL_DATA,
            vyna_dcm_core::Vr::Ow,
            vec![0xABu8; 300],
        );
        let cmd = Command::store_rq(
            1,
            priority::MEDIUM,
            vyna_dcm_core::sop_class::CT_IMAGE,
            "1.2.3.4",
        );
        let sent = tokio::spawn(async move {
            let mut sender = sender;
            sender.send(1, &cmd, Some(&ds)).await.unwrap();
        });
        let err = receiver.recv().await.unwrap_err();
        assert!(
            matches!(err, Error::MessageTooLarge { limit: 256 }),
            "{err:?}"
        );
        sent.await.unwrap();
    }
}
