//! Associations: the connected DIMSE conversation (NEMA PS3.8 §9, PS3.7).
//!
//! [`Association`] owns a framed byte stream (plain TCP or TLS) plus the
//! negotiated presentation contexts, and sends/receives [`DimseMessage`]s
//! with P-DATA-TF fragmentation. SCU operations live in the private
//! `scu` module as `impl Association` methods; the SCP dispatch loop is
//! [`crate::service::run_scp`].

use std::collections::VecDeque;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite};
use tokio::net::TcpStream;
use tokio::time::{Instant, timeout, timeout_at};
use vyna_dcm_codec::{decode_dataset, encode_dataset};
use vyna_dcm_core::{APPLICATION_CONTEXT_NAME, TransferSyntax};

use crate::{
    AeTitle, Command, Error, Result,
    dimse::{DimseMessage, decode_command, encode_command},
    pdu::{
        AssociateAc, AssociateRq, ExtendedNegotiation, Pdu, Pdv, ProposedContext,
        relational_requested,
    },
};

/// Trait alias for a connected byte stream.
pub trait AsyncReadWrite: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> AsyncReadWrite for T {}

/// Maximum events the pushback queue holds: a conformant peer sends at
/// most one stray message mid-operation (a C-CANCEL or release), so 16
/// is generous headroom and anything past it is a flood. Past the cap
/// [`Association::stash`] fails with
/// [`Error::TooManyStashed`](crate::Error::TooManyStashed) and the
/// operation aborts the association instead of buffering peer-driven
/// datasets without bound.
pub const MAX_STASHED_EVENTS: usize = 16;

/// Maximum aggregate dataset bytes the pushback queue holds (see
/// [`MAX_STASHED_EVENTS`]: the count cap alone still allows 16 x 1 GiB
/// stashed datasets). Past it [`Association::stash`] fails with
/// [`Error::StashedTooLarge`](crate::Error::StashedTooLarge). Legit
/// strays are a C-CANCEL or release (bytes, not megabytes), so 64 MiB
/// is generous headroom. (The C-GET hold-aside carries its own twin
/// budget, so the combined worst case is twice this.)
pub const MAX_STASHED_BYTES: u64 = 64 * 1024 * 1024;

/// Maximum reassembled DIMSE *command set* bytes across all its PDVs.
/// Command sets are ~10^2 bytes (a group-length-prefixed group
/// `0x0000`); sharing the 1 GiB dataset budget let a peer burn a
/// gigabyte of command reassembly before decode failed. Past this
/// [`Association::recv`] fails with [`Error::MessageTooLarge`].
pub const MAX_COMMAND_BYTES: u64 = 64 * 1024;

/// Options for requesting (SCU) or accepting (SCP) associations.
#[derive(Clone, Debug)]
#[non_exhaustive]
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
    /// TCP connect timeout (a hanging SYN must not stall the caller —
    /// for C-MOVE that caller is an inbound association).
    pub connect_timeout: Duration,
    /// Pending-response cap for SCU multi-response loops
    /// (`find_with`, `find_cancelable`, `get_with`, MOVE awaits): past
    /// this the association aborts with [`Error::TooManyResponses`],
    /// so a rogue peer PENDING forever cannot hold the call forever.
    pub max_pending_responses: u32,
    /// Per-PDU write timeout: a zero-window (or otherwise stalled)
    /// peer must not hold a send path forever.
    pub write_timeout: Duration,
    /// Cumulative bound for reassembling one DIMSE message across all
    /// its PDVs: the per-PDU read timeout resets on every PDU, so
    /// without this a slow-drip peer holds [`Association::recv`]
    /// forever. Past it `recv` fails with [`Error::Timeout`].
    pub max_message_duration: Duration,
    /// SOP-class extended negotiations (`0x56`) to propose, e.g. a
    /// relational-queries request for a QR SOP class.
    pub extended_negotiation: Vec<ExtendedNegotiation>,
}

impl Association {
    /// Count one more non-final response toward
    /// `max_pending_responses`; past the cap the association aborts
    /// (draining would let a rogue peer hold the call forever).
    pub(crate) async fn count_pending(&mut self, pending: &mut u32) -> Result<()> {
        *pending = pending.saturating_add(1);
        if *pending > self.max_pending_responses {
            // Best-effort: the error below is the outcome; a failed
            // abort just means the socket is already dead.
            self.abort(0, 0).await.ok();
            return Err(Error::TooManyResponses {
                limit: self.max_pending_responses,
            });
        }
        Ok(())
    }
}

impl Default for AssociationOptions {
    fn default() -> Self {
        Self {
            max_pdu_len: 256 * 1024,
            read_timeout: Duration::from_secs(300),
            connect_timeout: Duration::from_secs(10),
            implementation_class_uid: vyna_dcm_core::VYNA_IMPLEMENTATION_CLASS_UID.to_string(),
            implementation_version: Some(
                vyna_dcm_core::VYNA_IMPLEMENTATION_VERSION_NAME.to_string(),
            ),
            application_context: APPLICATION_CONTEXT_NAME.to_string(),
            max_incoming_pdu: 64 * 1024 * 1024,
            max_message_bytes: 1024 * 1024 * 1024,
            max_pending_responses: crate::MAX_PENDING_RESPONSES,
            write_timeout: Duration::from_secs(30),
            max_message_duration: Duration::from_secs(3600),
            extended_negotiation: Vec::new(),
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
    /// Relational queries negotiated for this context (QR SOP classes:
    /// extended negotiation accepted with bit 0 set).
    pub relational: bool,
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
    max_pending_responses: u32,
    next_id: u16,
    write_timeout: Duration,
    max_message_duration: Duration,
    /// In-progress PDU read, preserved across `poll_event` timeouts:
    /// bytes already consumed stay here so the next read continues
    /// where the abandoned one stopped instead of desyncing.
    partial_hdr: [u8; 6],
    partial_hdr_len: usize,
    partial_payload: Vec<u8>,
    /// Pushback queue: events seen by a cancel poll that belong to the
    /// dispatch loop (e.g. RELEASE-RQ mid-FIND), each with its dataset
    /// bytes for the [`MAX_STASHED_BYTES`] budget. [`Association::recv`]
    /// drains this before touching the wire.
    pending: VecDeque<(DimseEvent, u64)>,
    /// PDVs read past a message boundary: one P-DATA-TF may carry
    /// several messages, and [`Association::recv`] keeps the trailing
    /// PDVs here for the next call instead of dropping them.
    pdu_queue: VecDeque<Pdv>,
}

impl Association {
    /// Connect over TCP and request an association. Any
    /// [`ToSocketAddrs`](tokio::net::ToSocketAddrs) form works — prefer
    /// `(host, port)` tuples over pre-joined `"host:port"` strings so
    /// IPv6 literals resolve correctly.
    pub async fn request<A: tokio::net::ToSocketAddrs>(
        opts: &AssociationOptions,
        calling_ae: &AeTitle,
        called_ae: &AeTitle,
        addr: A,
        proposed: Vec<ProposedContext>,
    ) -> Result<Self> {
        // A hanging SYN must time out: without this, a C-MOVE to a
        // blackholed destination stalls the inbound association forever.
        let stream = timeout(opts.connect_timeout, TcpStream::connect(addr))
            .await
            .map_err(|_| Error::Timeout("TCP connect"))??;
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
        if proposed.is_empty() {
            return Err(Error::BadPdu("no presentation contexts proposed".into()));
        }
        // Same ID rules the SCP enforces (PS3.8 §9.3.2.2), checked
        // locally so a buggy caller fails fast instead of emitting a
        // malformed A-ASSOCIATE-RQ the peer rejects opaquely.
        let mut seen = [false; 256];
        for pc in &proposed {
            if pc.id == 0 || pc.id % 2 == 0 {
                return Err(Error::BadPdu(format!(
                    "presentation context id {} must be odd and nonzero",
                    pc.id
                )));
            }
            if seen[pc.id as usize] {
                return Err(Error::BadPdu(format!(
                    "duplicate presentation context id {}",
                    pc.id
                )));
            }
            seen[pc.id as usize] = true;
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
            max_pending_responses: opts.max_pending_responses,
            write_timeout: opts.write_timeout,
            max_message_duration: opts.max_message_duration,
            partial_hdr: [0; 6],
            partial_hdr_len: 0,
            partial_payload: Vec::new(),
            next_id: 1,
            pending: VecDeque::new(),
            pdu_queue: VecDeque::new(),
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
            extended_negotiation: opts.extended_negotiation.clone(),
        };
        this.write_pdu(&Pdu::AssociateRq(rq)).await?;
        match this.read_pdu().await? {
            Pdu::AssociateAc(ac) => this.adopt_ac(
                ac,
                &proposed,
                &opts.application_context,
                &opts.extended_negotiation,
            )?,
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
    ///
    /// Validated like the SCP validates the RQ: a mismatched
    /// application context, an unknown or duplicate context ID, or a
    /// transfer syntax we never proposed fails the association
    /// instead of silently narrowing it to whatever is left.
    fn adopt_ac(
        &mut self,
        ac: AssociateAc,
        proposed: &[ProposedContext],
        application_context: &str,
        proposed_extended: &[ExtendedNegotiation],
    ) -> Result<()> {
        if ac.application_context != application_context {
            return Err(Error::BadPdu(format!(
                "A-ASSOCIATE-AC application context {} differs from proposed {application_context}",
                ac.application_context
            )));
        }
        self.info.peer_max_pdu = ac.max_pdu_len;
        let mut seen = [false; 256];
        for acc in &ac.presentation_contexts {
            if acc.result != 0 {
                continue;
            }
            if seen[acc.id as usize] {
                return Err(Error::BadPdu(format!(
                    "duplicate accepted presentation context id {}",
                    acc.id
                )));
            }
            seen[acc.id as usize] = true;
            let Some(prop) = proposed.iter().find(|p| p.id == acc.id) else {
                return Err(Error::BadPdu(format!(
                    "accepted unproposed presentation context id {}",
                    acc.id
                )));
            };
            let Some(ts_uid) = &acc.transfer_syntax else {
                return Err(Error::BadPdu(format!(
                    "accepted context {} without transfer syntax",
                    acc.id
                )));
            };
            if !prop.transfer_syntaxes.iter().any(|t| t == ts_uid) {
                return Err(Error::BadPdu(format!(
                    "accepted context {} with unproposed transfer syntax {ts_uid}",
                    acc.id
                )));
            }
            let Some(ts) = TransferSyntax::from_uid(ts_uid) else {
                return Err(Error::BadPdu(format!(
                    "accepted context {} with unknown transfer syntax {ts_uid}",
                    acc.id
                )));
            };
            // Relational sticks only when we proposed it and the
            // acceptor echoed it back: a gratuitous AC entry we never
            // asked for must not flip query semantics.
            let proposed_relational =
                relational_requested(proposed_extended, &prop.abstract_syntax);
            let relational = proposed_relational
                && relational_requested(&ac.extended_negotiation, &prop.abstract_syntax);
            self.contexts.push(NegotiatedContext {
                id: acc.id,
                abstract_syntax: prop.abstract_syntax.clone(),
                transfer_syntax: ts,
                transfer_syntax_uid: ts_uid.clone(),
                relational,
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
            max_pending_responses: opts.max_pending_responses,
            write_timeout: opts.write_timeout,
            max_message_duration: opts.max_message_duration,
            partial_hdr: [0; 6],
            partial_hdr_len: 0,
            partial_payload: Vec::new(),
            next_id: 1,
            pending: VecDeque::new(),
            pdu_queue: VecDeque::new(),
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
        // 65535 wraps to 0, clamped back to 1.
        self.next_id = self.next_id.wrapping_add(1).max(1);
        id
    }

    /// Send one DIMSE message (command Implicit VR LE, dataset in the
    /// context's transfer syntax), fragmented to the peer's max PDU.
    ///
    /// The caller must keep `command.data_set_type` consistent with
    /// `dataset` (`None` ⟺ `DATA_SET_NULL`): a mismatch puts a
    /// dataset-less command on the wire with data (or vice versa).
    pub async fn send(
        &mut self,
        context_id: u8,
        command: &Command,
        dataset: Option<&vyna_dcm_core::DataSet>,
    ) -> Result<()> {
        // Hard error, not debug-only: a mismatch would emit corrupt
        // DIMSE (flag says dataset, none follows, or vice versa).
        if command.has_dataset() != dataset.is_some() {
            return Err(Error::BadCommand(
                "command data_set_type disagrees with dataset presence".into(),
            ));
        }
        // Priority is validated at the RQ constructors, but a
        // hand-built command must not slip an invalid one past them.
        use crate::dimse::command_field as CF;
        if matches!(
            command.field,
            CF::C_STORE_RQ | CF::C_FIND_RQ | CF::C_MOVE_RQ | CF::C_GET_RQ
        ) && command.priority > crate::priority::LOW
        {
            return Err(Error::BadCommand(format!(
                "priority {} out of range 0..=2",
                command.priority
            )));
        }
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
                self.write_pdu(&Pdu::Data(vec![pdv])).await?;
            }
        }
        Ok(())
    }

    /// Largest P-DATA-TF payload we may emit (peer limit or 1 MiB).
    ///
    /// PS3.8: never emit a PDU larger than the peer declared — even a
    /// tiny declared maximum is honored (fragmenting down to 1-byte
    /// PDVs), since oversize PDUs are a protocol violation.
    fn send_max(&self) -> usize {
        let cap = if self.info.peer_max_pdu == 0 {
            1024 * 1024
        } else {
            self.info.peer_max_pdu
        };
        // 12 bytes of PDU + PDV header precede every payload.
        (cap as usize).saturating_sub(12).max(1)
    }

    /// Receive one complete DIMSE message (reassembling PDVs), or a release.
    ///
    /// One message gets one cumulative [`AssociationOptions::max_message_duration`]
    /// across all its PDVs on top of the per-PDU read timeout.
    pub async fn recv(&mut self) -> Result<DimseEvent> {
        if let Some((ev, _)) = self.pending.pop_front() {
            return Ok(ev);
        }
        let deadline = Instant::now() + self.max_message_duration;
        let mut cmd_buf: Vec<u8> = Vec::new();
        let mut cmd_ctx: Option<u8> = None;
        let mut command: Option<Command> = None;
        let mut data_buf: Vec<u8> = Vec::new();
        loop {
            let pdv = match self.pdu_queue.pop_front() {
                Some(p) => p,
                None => match self.read_pdu_capped(deadline).await? {
                    Pdu::Data(pdvs) => {
                        // decode_data rejects empty P-DATA-TFs, so the
                        // queue is non-empty and the loop makes progress.
                        self.pdu_queue.extend(pdvs);
                        continue;
                    }
                    Pdu::ReleaseRq => {
                        self.write_pdu(&Pdu::ReleaseRsp).await?;
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
                },
            };
            let ctx = self
                .context_by_id(pdv.context_id)
                .ok_or(Error::UnknownContext(pdv.context_id))?
                .clone();
            if pdv.is_command {
                if command.is_some() {
                    return Err(Error::BadPdu("command PDV after command complete".into()));
                }
                match cmd_ctx {
                    Some(id) if id != pdv.context_id => {
                        return Err(Error::BadPdu("command PDVs span contexts".into()));
                    }
                    _ => cmd_ctx = Some(pdv.context_id),
                }
                if cmd_buf.len() as u64 + pdv.data.len() as u64 > MAX_COMMAND_BYTES {
                    return Err(Error::MessageTooLarge {
                        limit: MAX_COMMAND_BYTES,
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
                if data_buf.len() as u64 + pdv.data.len() as u64 > self.max_message_bytes {
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

    /// Receive one event, waiting at most `wait`; `Ok(None)` on timeout.
    ///
    /// Used to poll for C-CANCEL between pending responses. A timeout
    /// abandons no data: the in-progress PDU read is preserved in
    /// `partial_*`, so a message arriving in segments (e.g. a C-CANCEL
    /// split across polls) resumes where it stopped.
    pub async fn poll_event(&mut self, wait: Duration) -> Result<Option<DimseEvent>> {
        if let Some((ev, _)) = self.pending.pop_front() {
            return Ok(Some(ev));
        }
        match timeout(wait, self.recv()).await {
            Ok(ev) => Ok(Some(ev?)),
            Err(_) => Ok(None),
        }
    }

    /// Return an event to the pushback queue (see [`Association::recv`]).
    /// Fails with [`Error::TooManyStashed`](crate::Error::TooManyStashed)
    /// past [`MAX_STASHED_EVENTS`] (or
    /// [`Error::StashedTooLarge`](crate::Error::StashedTooLarge) past
    /// [`MAX_STASHED_BYTES`]) instead of buffering without bound.
    pub fn stash(&mut self, ev: DimseEvent) -> Result<()> {
        if self.pending.len() >= MAX_STASHED_EVENTS {
            return Err(Error::TooManyStashed {
                limit: MAX_STASHED_EVENTS,
            });
        }
        let size = event_bytes(&ev);
        let held: u64 = self.pending.iter().map(|(_, b)| b).sum();
        if held.saturating_add(size) > MAX_STASHED_BYTES {
            return Err(Error::StashedTooLarge {
                limit: MAX_STASHED_BYTES,
            });
        }
        self.pending.push_back((ev, size));
        Ok(())
    }

    /// Clean release: RELEASE-RQ then await RELEASE-RSP.
    ///
    /// A simultaneous peer RELEASE-RQ (PS3.8 §9.3.8 release collision)
    /// is answered with RELEASE-RSP and counts as clean completion,
    /// not an error.
    pub async fn release(&mut self) -> Result<()> {
        self.write_pdu(&Pdu::ReleaseRq).await?;
        match self.read_pdu().await? {
            Pdu::ReleaseRsp => Ok(()),
            Pdu::ReleaseRq => {
                self.write_pdu(&Pdu::ReleaseRsp).await?;
                Ok(())
            }
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
        self.write_pdu(&Pdu::Abort { source, reason }).await
    }

    /// One PDU write bounded by [`AssociationOptions::write_timeout`].
    async fn write_pdu(&mut self, pdu: &Pdu) -> Result<()> {
        crate::pdu::write_pdu_timeout(&mut self.stream, pdu, self.write_timeout).await
    }

    async fn read_pdu(&mut self) -> Result<Pdu> {
        timeout(self.read_timeout, self.read_pdu_inner())
            .await
            .map_err(|_| Error::Timeout("PDU read"))?
    }

    /// One PDU read bounded by the per-PDU read timeout *and* the
    /// message reassembly `deadline`.
    async fn read_pdu_capped(&mut self, deadline: Instant) -> Result<Pdu> {
        timeout_at(deadline, self.read_pdu())
            .await
            .map_err(|_| Error::Timeout("DIMSE message"))?
    }

    /// Resumable PDU read: bytes already consumed are kept in
    /// `partial_*`, so a `poll_event` timeout abandons no data and the
    /// next call continues where this one stopped.
    async fn read_pdu_inner(&mut self) -> Result<Pdu> {
        while self.partial_hdr_len < 6 {
            let n = self
                .stream
                .read(&mut self.partial_hdr[self.partial_hdr_len..])
                .await?;
            if n == 0 {
                return Err(Error::BadPdu("connection closed mid-PDU".into()));
            }
            self.partial_hdr_len += n;
        }
        let (typ, len) = (
            self.partial_hdr[0],
            u32::from_be_bytes([
                self.partial_hdr[2],
                self.partial_hdr[3],
                self.partial_hdr[4],
                self.partial_hdr[5],
            ]),
        );
        if len > self.max_incoming_pdu {
            self.reset_partial();
            return Err(Error::BadPdu(format!(
                "PDU length {len} exceeds cap {}",
                self.max_incoming_pdu
            )));
        }
        let total = len as usize;
        self.partial_payload
            .reserve(total.saturating_sub(self.partial_payload.len()));
        while self.partial_payload.len() < total {
            let mut chunk = [0u8; 8192];
            let want = (total - self.partial_payload.len()).min(chunk.len());
            let n = self.stream.read(&mut chunk[..want]).await?;
            if n == 0 {
                return Err(Error::BadPdu("connection closed mid-PDU".into()));
            }
            self.partial_payload.extend_from_slice(&chunk[..n]);
        }
        // Take the payload out before decoding: a malformed PDU
        // still consumes its bytes, so the next read starts fresh.
        let payload = std::mem::take(&mut self.partial_payload);
        self.partial_hdr_len = 0;
        crate::pdu::decode_pdu(typ, &payload)
    }

    fn reset_partial(&mut self) {
        self.partial_hdr_len = 0;
        self.partial_payload.clear();
    }
}

/// Dataset bytes an event holds for the stash budget (commands are
/// ~10^2 bytes and ignored; releases hold nothing).
fn event_bytes(ev: &DimseEvent) -> u64 {
    match ev {
        DimseEvent::Released => 0,
        DimseEvent::Message(m) => m.dataset.as_ref().map(|ds| ds.approx_bytes()).unwrap_or(0),
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
            max_pending_responses: opts.max_pending_responses,
            write_timeout: opts.write_timeout,
            max_message_duration: opts.max_message_duration,
            partial_hdr: [0; 6],
            partial_hdr_len: 0,
            partial_payload: Vec::new(),
            next_id: u16::MAX,
            pending: VecDeque::new(),
            pdu_queue: VecDeque::new(),
        };
        assert_eq!(assoc.next_message_id(), u16::MAX);
        assert_eq!(assoc.next_message_id(), 1);
    }

    #[test]
    fn stash_fails_past_cap() {
        let opts = AssociationOptions::default();
        let (a, _b) = tokio::io::duplex(64);
        let info = AssociationInfo {
            local_ae: AeTitle::new("A").unwrap(),
            peer_ae: AeTitle::new("B").unwrap(),
            peer_max_pdu: 0,
        };
        let mut assoc = Association::from_accepted(Box::new(a), info, vec![], &opts);
        for _ in 0..MAX_STASHED_EVENTS {
            assoc.stash(DimseEvent::Released).unwrap();
        }
        let err = assoc.stash(DimseEvent::Released).unwrap_err();
        assert!(
            matches!(err, Error::TooManyStashed { .. }),
            "unexpected: {err:?}"
        );
    }

    #[test]
    fn stash_fails_past_byte_budget() {
        use vyna_dcm_core::{DataElement, DataSet, Value, Vr, tags};
        let opts = AssociationOptions::default();
        let (a, _b) = tokio::io::duplex(64);
        let info = AssociationInfo {
            local_ae: AeTitle::new("A").unwrap(),
            peer_ae: AeTitle::new("B").unwrap(),
            peer_max_pdu: 0,
        };
        let mut assoc = Association::from_accepted(Box::new(a), info, vec![], &opts);
        let message = |bytes: usize| {
            let mut ds = DataSet::new();
            ds.insert(DataElement::new(
                tags::PIXEL_DATA,
                Vr::Ow,
                Value::Bytes(vec![0u8; bytes]),
            ));
            DimseEvent::Message(Box::new(crate::dimse::DimseMessage {
                context_id: 1,
                command: Command::echo_rq(1),
                dataset: Some(ds),
            }))
        };
        // Small stashes fit.
        assoc.stash(message(1024)).unwrap();
        // One 64 MiB + 1 byte stash trips the byte budget on its own.
        let err = assoc
            .stash(message(MAX_STASHED_BYTES as usize + 1))
            .unwrap_err();
        assert!(
            matches!(err, Error::StashedTooLarge { .. }),
            "unexpected: {err:?}"
        );
    }

    #[tokio::test]
    async fn connect_timeout_bounds_blackhole() {
        // Probe first: TEST-NET-1 must hang here for the strict
        // assertion to mean anything (some sandboxes refuse it fast
        // instead, in which case there is nothing to bound).
        let probe = tokio::time::timeout(
            Duration::from_secs(2),
            tokio::net::TcpStream::connect("192.0.2.1:42"),
        )
        .await;
        if probe.is_ok() {
            eprintln!("blackhole probe did not hang; skipping strict connect-timeout check");
            return;
        }
        let opts = AssociationOptions {
            connect_timeout: Duration::from_millis(200),
            ..Default::default()
        };
        let t0 = std::time::Instant::now();
        let Err(err) = Association::request(
            &opts,
            &AeTitle::new("SCU").unwrap(),
            &AeTitle::new("SCP").unwrap(),
            "192.0.2.1:42",
            Vec::new(),
        )
        .await
        else {
            panic!("blackhole connect unexpectedly succeeded");
        };
        assert!(matches!(err, Error::Timeout(_)), "{err:?}");
        assert!(t0.elapsed() < Duration::from_secs(30), "connect hung");
    }

    #[tokio::test]
    async fn refused_connect_still_fails_fast() {
        // The timeout must not break normal failures: port 1 refuses
        // immediately with an I/O error, not a timeout.
        let opts = AssociationOptions {
            connect_timeout: Duration::from_secs(10),
            ..Default::default()
        };
        let Err(err) = Association::request(
            &opts,
            &AeTitle::new("SCU").unwrap(),
            &AeTitle::new("SCP").unwrap(),
            "127.0.0.1:1",
            Vec::new(),
        )
        .await
        else {
            panic!("refused connect unexpectedly succeeded");
        };
        assert!(matches!(err, Error::Io(_)), "{err:?}");
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
            relational: false,
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
        )
        .unwrap();
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

    #[tokio::test]
    async fn recv_keeps_trailing_pdvs() {
        // One P-DATA-TF carrying two C-ECHO-RQ commands: both must
        // surface; the second must not be dropped with the first.
        let (a, b) = tokio::io::duplex(65536);
        let mut w = a;
        let pdvs: Vec<Pdv> = [1u16, 2]
            .map(|id| Pdv {
                context_id: 1,
                is_command: true,
                is_last: true,
                data: encode_command(&Command::echo_rq(id)).unwrap(),
            })
            .to_vec();
        crate::pdu::write_pdu(&mut w, &Pdu::Data(pdvs))
            .await
            .unwrap();
        drop(w);
        let mut receiver = Association::from_accepted(
            Box::new(b),
            AssociationInfo {
                local_ae: AeTitle::new("SCP").unwrap(),
                peer_ae: AeTitle::new("SCU").unwrap(),
                peer_max_pdu: 0,
            },
            vec![NegotiatedContext {
                id: 1,
                abstract_syntax: vyna_dcm_core::sop_class::CT_IMAGE.to_string(),
                transfer_syntax: TransferSyntax::ExplicitVrLittleEndian,
                transfer_syntax_uid: TransferSyntax::ExplicitVrLittleEndian.uid().to_string(),
                relational: false,
            }],
            &AssociationOptions::default(),
        );
        let mut ids = Vec::new();
        for _ in 0..2 {
            match receiver.recv().await.unwrap() {
                DimseEvent::Message(m) => ids.push(m.command.message_id),
                DimseEvent::Released => panic!("unexpected release"),
            }
        }
        assert_eq!(ids, vec![Some(1), Some(2)]);
    }

    fn loopback_ctx(id: u8) -> NegotiatedContext {
        NegotiatedContext {
            id,
            abstract_syntax: vyna_dcm_core::sop_class::CT_IMAGE.to_string(),
            transfer_syntax: TransferSyntax::ExplicitVrLittleEndian,
            transfer_syntax_uid: TransferSyntax::ExplicitVrLittleEndian.uid().to_string(),
            relational: false,
        }
    }

    fn loopback_info(local: &str, peer: &str) -> AssociationInfo {
        AssociationInfo {
            local_ae: AeTitle::new(local).unwrap(),
            peer_ae: AeTitle::new(peer).unwrap(),
            peer_max_pdu: 0,
        }
    }

    #[tokio::test]
    async fn poll_event_preserves_partial_pdu() {
        // A C-CANCEL split across two polls: the ZERO-wait poll must
        // return None without dropping the bytes it already consumed.
        use tokio::io::AsyncWriteExt;
        let (a, b) = tokio::io::duplex(65536);
        let mut peer = a;
        let mut assoc = Association::from_accepted(
            Box::new(b),
            loopback_info("SCP", "SCU"),
            vec![loopback_ctx(1)],
            &AssociationOptions::default(),
        );
        let cmd_bytes = encode_command(&Command::cancel_rq(7)).unwrap();
        let mut frame = vec![0x04u8, 0x00];
        frame.extend_from_slice(&((cmd_bytes.len() + 6) as u32).to_be_bytes());
        frame.extend_from_slice(&((cmd_bytes.len() + 2) as u32).to_be_bytes());
        frame.extend([1u8, 0x03]); // context 1, command + last
        frame.extend_from_slice(&cmd_bytes);
        peer.write_all(&frame[..10]).await.unwrap();
        assert!(assoc.poll_event(Duration::ZERO).await.unwrap().is_none());
        peer.write_all(&frame[10..]).await.unwrap();
        match assoc.poll_event(Duration::from_secs(5)).await.unwrap() {
            Some(DimseEvent::Message(m)) => {
                assert_eq!(m.command.field, crate::dimse::command_field::C_CANCEL_RQ);
                assert_eq!(m.command.message_id_rsp, Some(7));
            }
            other => panic!("expected C-CANCEL, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn write_timeout_bounds_stalled_peer() {
        // Nobody reads: the send must fail, not hang on a full buffer.
        let (a, _b) = tokio::io::duplex(1024);
        let opts = AssociationOptions {
            write_timeout: Duration::from_millis(100),
            ..AssociationOptions::default()
        };
        let mut assoc = Association::from_accepted(
            Box::new(a),
            loopback_info("SCU", "SCP"),
            vec![loopback_ctx(1)],
            &opts,
        );
        let mut ds = vyna_dcm_core::DataSet::new();
        ds.put_bytes(
            vyna_dcm_core::tags::PIXEL_DATA,
            vyna_dcm_core::Vr::Ow,
            vec![0xABu8; 256 * 1024],
        );
        let cmd = Command::store_rq(1, crate::dimse::priority::MEDIUM, "1.2.3", "1.2.3.4").unwrap();
        let err = assoc.send(1, &cmd, Some(&ds)).await.unwrap_err();
        assert!(matches!(err, Error::Timeout("PDU write")), "{err:?}");
    }

    #[tokio::test]
    async fn release_handles_simultaneous_peer_release() {
        // PS3.8 §9.3.8 collision: both sides send RELEASE-RQ at once;
        // both must complete cleanly.
        let (a, b) = tokio::io::duplex(65536);
        let opts = AssociationOptions::default();
        let mut left =
            Association::from_accepted(Box::new(a), loopback_info("A", "B"), vec![], &opts);
        let mut right =
            Association::from_accepted(Box::new(b), loopback_info("B", "A"), vec![], &opts);
        let (r1, r2) = tokio::join!(left.release(), right.release());
        r1.unwrap();
        r2.unwrap();
    }

    #[tokio::test]
    async fn recv_enforces_cumulative_message_deadline() {
        // A peer that never finishes the PDU must hit the cumulative
        // message bound, not the (much longer) per-PDU read timeout.
        use tokio::io::AsyncWriteExt;
        let (a, b) = tokio::io::duplex(65536);
        let mut peer = a;
        let opts = AssociationOptions {
            read_timeout: Duration::from_secs(30),
            max_message_duration: Duration::from_millis(100),
            ..AssociationOptions::default()
        };
        let mut assoc = Association::from_accepted(
            Box::new(b),
            loopback_info("SCP", "SCU"),
            vec![loopback_ctx(1)],
            &opts,
        );
        peer.write_all(&[0x04u8, 0x00, 0x00]).await.unwrap();
        let t0 = std::time::Instant::now();
        let err = assoc.recv().await.unwrap_err();
        assert!(matches!(err, Error::Timeout("DIMSE message")), "{err:?}");
        assert!(t0.elapsed() < Duration::from_secs(30));
    }

    #[test]
    fn tiny_peer_max_pdu_is_respected() {
        // PS3.8: emitted PDUs must fit the peer's declared maximum,
        // however small (12 bytes of PDU + PDV header overhead).
        let opts = AssociationOptions::default();
        let (a, _b) = tokio::io::duplex(64);
        let mut assoc =
            Association::from_accepted(Box::new(a), loopback_info("SCU", "SCP"), vec![], &opts);
        assoc.info.peer_max_pdu = 20;
        assert_eq!(assoc.send_max(), 8);
        assoc.info.peer_max_pdu = 13;
        assert_eq!(assoc.send_max(), 1);
        assoc.info.peer_max_pdu = 0;
        assert_eq!(assoc.send_max(), 1024 * 1024 - 12);
    }

    #[tokio::test]
    async fn tiny_peer_max_pdu_still_round_trips() {
        // 8-byte PDV payloads must reassemble into the intact message.
        let (a, b) = tokio::io::duplex(65536);
        let opts = AssociationOptions::default();
        let mut sender = Association::from_accepted(
            Box::new(a),
            loopback_info("SCU", "SCP"),
            vec![loopback_ctx(1)],
            &opts,
        );
        sender.info.peer_max_pdu = 20;
        let mut receiver = Association::from_accepted(
            Box::new(b),
            loopback_info("SCP", "SCU"),
            vec![loopback_ctx(1)],
            &opts,
        );
        let mut ds = vyna_dcm_core::DataSet::new();
        ds.put_bytes(
            vyna_dcm_core::tags::PIXEL_DATA,
            vyna_dcm_core::Vr::Ow,
            vec![0xCDu8; 300],
        );
        let cmd = Command::store_rq(1, crate::dimse::priority::MEDIUM, "1.2.3", "1.2.3.4").unwrap();
        sender.send(1, &cmd, Some(&ds)).await.unwrap();
        drop(sender);
        match receiver.recv().await.unwrap() {
            DimseEvent::Message(m) => assert_eq!(m.dataset.unwrap(), ds),
            DimseEvent::Released => panic!("unexpected release"),
        }
    }

    #[tokio::test]
    async fn command_reassembly_has_dedicated_budget() {
        // A 70 KiB command stream must trip the 64 KiB command budget,
        // not accumulate toward the 1 GiB dataset budget.
        let (a, b) = tokio::io::duplex(1024 * 1024);
        let mut w = a;
        let chunk = vec![0x55u8; 8192];
        let mut pdvs = Vec::new();
        for i in 0..9 {
            pdvs.push(Pdv {
                context_id: 1,
                is_command: true,
                is_last: i == 8,
                data: chunk.clone(),
            });
        }
        crate::pdu::write_pdu(&mut w, &Pdu::Data(pdvs))
            .await
            .unwrap();
        drop(w);
        let mut receiver = Association::from_accepted(
            Box::new(b),
            loopback_info("SCP", "SCU"),
            vec![loopback_ctx(1)],
            &AssociationOptions::default(),
        );
        let err = receiver.recv().await.unwrap_err();
        assert!(
            matches!(
                err,
                Error::MessageTooLarge {
                    limit: MAX_COMMAND_BYTES
                }
            ),
            "{err:?}"
        );
    }

    #[test]
    fn adopt_ac_validates_like_negotiate() {
        use crate::pdu::AcceptedContext;
        let opts = AssociationOptions::default();
        let (a, _b) = tokio::io::duplex(64);
        let mut assoc =
            Association::from_accepted(Box::new(a), loopback_info("SCU", "SCP"), vec![], &opts);
        let proposed = vec![crate::pdu::ProposedContext {
            id: 1,
            abstract_syntax: "1.2.3".into(),
            transfer_syntaxes: vec![TransferSyntax::ExplicitVrLittleEndian.uid().to_string()],
        }];
        let app = opts.application_context.clone();
        let ac = |ctxs: Vec<AcceptedContext>| crate::pdu::AssociateAc {
            application_context: app.clone(),
            presentation_contexts: ctxs,
            max_pdu_len: 16384,
            implementation_class_uid: "1.2.9".into(),
            implementation_version: None,
            async_ops: None,
            role_selection: vec![],
            user_identity_response: None,
            extended_negotiation: vec![],
        };
        let accepted = |id: u8, ts: Option<&str>| AcceptedContext {
            id,
            result: 0,
            transfer_syntax: ts.map(str::to_string),
        };
        // Happy path + rejected entries skipped.
        assoc.contexts.clear();
        assoc
            .adopt_ac(
                ac(vec![
                    accepted(1, Some(TransferSyntax::ExplicitVrLittleEndian.uid())),
                    AcceptedContext {
                        id: 3,
                        result: 3,
                        transfer_syntax: None,
                    },
                ]),
                &proposed,
                &app,
                &[],
            )
            .unwrap();
        assert_eq!(assoc.contexts.len(), 1);
        // Wrong application context.
        let mut bad = ac(vec![accepted(
            1,
            Some(TransferSyntax::ExplicitVrLittleEndian.uid()),
        )]);
        bad.application_context = "9.9.9".into();
        assert!(assoc.adopt_ac(bad, &proposed, &app, &[]).is_err());
        // Duplicate accepted ID.
        assert!(
            assoc
                .adopt_ac(
                    ac(vec![
                        accepted(1, Some(TransferSyntax::ExplicitVrLittleEndian.uid())),
                        accepted(1, Some(TransferSyntax::ExplicitVrLittleEndian.uid())),
                    ]),
                    &proposed,
                    &app,
                    &[],
                )
                .is_err()
        );
        // Unproposed ID, missing TS, unproposed TS.
        assert!(
            assoc
                .adopt_ac(
                    ac(vec![accepted(
                        3,
                        Some(TransferSyntax::ExplicitVrLittleEndian.uid())
                    )]),
                    &proposed,
                    &app,
                    &[],
                )
                .is_err()
        );
        assert!(
            assoc
                .adopt_ac(ac(vec![accepted(1, None)]), &proposed, &app, &[])
                .is_err()
        );
        assert!(
            assoc
                .adopt_ac(ac(vec![accepted(1, Some("9.9.9.9"))]), &proposed, &app, &[])
                .is_err()
        );
    }

    #[test]
    fn adopt_ac_sets_relational_only_when_proposed_and_echoed() {
        use crate::pdu::AcceptedContext;
        use vyna_dcm_core::sop_class as sc;
        let opts = AssociationOptions::default();
        let (a, _b) = tokio::io::duplex(64);
        let mut assoc =
            Association::from_accepted(Box::new(a), loopback_info("SCU", "SCP"), vec![], &opts);
        let proposed = vec![crate::pdu::ProposedContext {
            id: 1,
            abstract_syntax: sc::STUDY_ROOT_FIND.to_string(),
            transfer_syntaxes: vec![TransferSyntax::ExplicitVrLittleEndian.uid().to_string()],
        }];
        let app = opts.application_context.clone();
        let accepted = AcceptedContext {
            id: 1,
            result: 0,
            transfer_syntax: Some(TransferSyntax::ExplicitVrLittleEndian.uid().to_string()),
        };
        let ac_with = |ext: Vec<crate::pdu::ExtendedNegotiation>| crate::pdu::AssociateAc {
            application_context: app.clone(),
            presentation_contexts: vec![accepted.clone()],
            max_pdu_len: 16384,
            implementation_class_uid: "1.2.9".into(),
            implementation_version: None,
            async_ops: None,
            role_selection: vec![],
            user_identity_response: None,
            extended_negotiation: ext,
        };
        let rel = crate::pdu::ExtendedNegotiation {
            sop_class_uid: sc::STUDY_ROOT_FIND.to_string(),
            app_info: vec![1],
        };
        // Proposed + echoed: relational sticks.
        assoc.contexts.clear();
        assoc
            .adopt_ac(
                ac_with(vec![rel.clone()]),
                &proposed,
                &app,
                std::slice::from_ref(&rel),
            )
            .unwrap();
        assert!(assoc.contexts[0].relational);
        // Echoed but never proposed: gratuitous entry ignored.
        assoc.contexts.clear();
        assoc
            .adopt_ac(ac_with(vec![rel.clone()]), &proposed, &app, &[])
            .unwrap();
        assert!(!assoc.contexts[0].relational);
        // Proposed but not echoed: stays hierarchical.
        assoc.contexts.clear();
        assoc
            .adopt_ac(ac_with(vec![]), &proposed, &app, std::slice::from_ref(&rel))
            .unwrap();
        assert!(!assoc.contexts[0].relational);
    }

    #[tokio::test]
    async fn request_over_rejects_bad_proposals_locally() {
        // Duplicate IDs and empty proposals fail before any wire I/O
        // (the peer half is never even read here).
        let opts = AssociationOptions {
            read_timeout: Duration::from_millis(100),
            ..AssociationOptions::default()
        };
        let pc = |id: u8| crate::pdu::ProposedContext {
            id,
            abstract_syntax: "1.2.3".into(),
            transfer_syntaxes: vec![TransferSyntax::ExplicitVrLittleEndian.uid().to_string()],
        };
        let (a, _b) = tokio::io::duplex(64);
        let err = match Association::request_over(
            &opts,
            &AeTitle::new("SCU").unwrap(),
            &AeTitle::new("SCP").unwrap(),
            a,
            vec![pc(1), pc(1)],
        )
        .await
        {
            Ok(_) => panic!("duplicate IDs accepted"),
            Err(e) => e,
        };
        assert!(matches!(err, Error::BadPdu(_)), "{err:?}");
        let (a, _b) = tokio::io::duplex(64);
        let err = match Association::request_over(
            &opts,
            &AeTitle::new("SCU").unwrap(),
            &AeTitle::new("SCP").unwrap(),
            a,
            Vec::new(),
        )
        .await
        {
            Ok(_) => panic!("empty proposal accepted"),
            Err(e) => e,
        };
        assert!(matches!(err, Error::BadPdu(_)), "{err:?}");
    }
}
