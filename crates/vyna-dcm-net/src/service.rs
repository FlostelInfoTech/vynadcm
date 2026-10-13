//! SCP side: association negotiation, service handlers, dispatch.
//!
//! - [`AcceptorOptions`] + [`negotiate`] decide A-ASSOCIATE-AC/RJ.
//! - [`Handlers`] implements the DIMSE services; override what you need,
//!   the defaults answer C-ECHO and fail the rest with standard statuses.
//! - [`accept`] runs one inbound connection to release/abort.
//! - [`run_scp`] runs the dispatch loop on an established [`Association`].
//!
//! SCU operations are `impl Association` methods in [`crate::scu`].

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::time::timeout;
use vyna_dcm_core::DataSet;

use crate::{
    AeTitle, Error, Result,
    association::{Association, AssociationInfo, DimseEvent, NegotiatedContext},
    dimse::{Command, DimseMessage, command_field as CF, priority, status},
    pdu::{AcceptedContext, AssociateAc, AssociateRj, Pdu, ProposedContext},
};

/// User-identity check: `Some(response)` accepts (response echoed back when
/// requested), `None` rejects the association.
pub type UserIdentityAuth = Arc<dyn Fn(&crate::pdu::UserIdentity) -> Option<Vec<u8>> + Send + Sync>;

/// What the acceptor is willing to negotiate.
///
/// The defaults accept any Called-AE title, any proposed SOP class,
/// and no user identity: convenient for loopback tests and trusted
/// networks, but an Internet- or hospital-facing SCP should set
/// [`AcceptorOptions::check_called_ae`], restrict
/// [`AcceptorOptions::accepted_sop_classes`], and install
/// [`AcceptorOptions::user_identity_auth`] (see
/// [`AcceptorOptions::strict`] for the Called-AE half).
#[derive(Clone)]
pub struct AcceptorOptions {
    /// Our AE title.
    pub ae_title: AeTitle,
    /// Association-level options.
    pub association: crate::association::AssociationOptions,
    /// Transfer syntaxes we accept (checked in SCU-offered order).
    pub accepted_transfer_syntaxes: Vec<String>,
    /// Abstract syntaxes we accept; `None` = accept anything
    /// proposed (restrict this on exposed SCPs).
    pub accepted_sop_classes: Option<Vec<String>>,
    /// Reject when Called-AE != ours. Off by default (many peers
    /// misconfigure it); enable on exposed SCPs.
    pub check_called_ae: bool,
    /// User-identity gate; `None` = no authentication.
    pub user_identity_auth: Option<UserIdentityAuth>,
}

impl AcceptorOptions {
    /// Defaults with Called-AE checking enabled.
    #[must_use]
    pub fn strict() -> Self {
        Self {
            check_called_ae: true,
            ..Self::default()
        }
    }
}

impl Default for AcceptorOptions {
    fn default() -> Self {
        Self {
            ae_title: AeTitle::new("VYNA_SCP").unwrap(),
            association: crate::association::AssociationOptions::default(),
            accepted_transfer_syntaxes: vyna_dcm_core::TransferSyntax::ALL
                .iter()
                .map(|ts| ts.uid().to_string())
                .collect(),
            accepted_sop_classes: None,
            check_called_ae: false,
            user_identity_auth: None,
        }
    }
}

/// Build odd-ID presentation contexts for SCU proposals.
///
/// At most 128 SOP classes fit (odd IDs `1..=255`); proposing more
/// is an error rather than wrapped-around duplicate IDs.
pub fn propose(
    sop_classes: &[&str],
    transfer_syntaxes: &[vyna_dcm_core::TransferSyntax],
) -> Result<Vec<ProposedContext>> {
    if sop_classes.len() > 128 {
        return Err(Error::BadPdu(format!(
            "cannot propose {} SOP classes (odd context IDs 1..=255 hold 128)",
            sop_classes.len()
        )));
    }
    Ok(sop_classes
        .iter()
        .enumerate()
        .map(|(i, sop)| ProposedContext {
            id: (i * 2 + 1) as u8,
            abstract_syntax: sop.to_string(),
            transfer_syntaxes: transfer_syntaxes
                .iter()
                .map(|ts| ts.uid().to_string())
                .collect(),
        })
        .collect())
}

/// Negotiate an A-ASSOCIATE-RQ into AC (+ contexts) or RJ.
pub fn negotiate(
    rq: &crate::pdu::AssociateRq,
    opts: &AcceptorOptions,
) -> std::result::Result<(AssociateAc, Vec<NegotiatedContext>), AssociateRj> {
    let reject = |result, source, reason| AssociateRj {
        result,
        source,
        reason,
    };
    if rq.application_context != opts.association.application_context {
        return Err(reject(1, 1, 2)); // application context not supported
    }
    if opts.check_called_ae && rq.called_ae != opts.ae_title {
        return Err(reject(1, 1, 7)); // called-AE-title not recognised
    }
    let mut identity_response = None;
    if let Some(auth) = &opts.user_identity_auth {
        match &rq.user_identity {
            Some(id) => match auth(id) {
                Some(resp) if id.positive_response_requested => identity_response = Some(resp),
                Some(_) => {}
                None => return Err(reject(1, 1, 1)),
            },
            None => return Err(reject(1, 1, 1)),
        }
    }

    let mut contexts = Vec::new();
    let mut pcs = Vec::new();
    for prop in &rq.presentation_contexts {
        if let Some(only) = &opts.accepted_sop_classes
            && !only.contains(&prop.abstract_syntax)
        {
            pcs.push(AcceptedContext {
                id: prop.id,
                result: 3,
                transfer_syntax: None,
            });
            continue;
        }
        let pick = prop
            .transfer_syntaxes
            .iter()
            .find(|ts| opts.accepted_transfer_syntaxes.iter().any(|a| a == *ts))
            .cloned();
        match pick {
            Some(ts_uid) => {
                // Parse defensively; our accepted list is known-good.
                if let Some(ts) = vyna_dcm_core::TransferSyntax::from_uid(&ts_uid) {
                    contexts.push(NegotiatedContext {
                        id: prop.id,
                        abstract_syntax: prop.abstract_syntax.clone(),
                        transfer_syntax: ts,
                        transfer_syntax_uid: ts_uid.clone(),
                    });
                    pcs.push(AcceptedContext {
                        id: prop.id,
                        result: 0,
                        transfer_syntax: Some(ts_uid),
                    });
                } else {
                    pcs.push(AcceptedContext {
                        id: prop.id,
                        result: 4,
                        transfer_syntax: None,
                    });
                }
            }
            None => pcs.push(AcceptedContext {
                id: prop.id,
                result: 4,
                transfer_syntax: None,
            }),
        }
    }
    if contexts.is_empty() {
        return Err(reject(1, 1, 1)); // nothing acceptable
    }

    Ok((
        AssociateAc {
            application_context: rq.application_context.clone(),
            presentation_contexts: pcs,
            max_pdu_len: opts.association.max_pdu_len,
            implementation_class_uid: opts.association.implementation_class_uid.clone(),
            implementation_version: opts.association.implementation_version.clone(),
            async_ops: None, // synchronous: one outstanding operation per association
            role_selection: rq.role_selection.clone(), // accept proposed roles
            user_identity_response: identity_response,
        },
        contexts,
    ))
}

/// Request context handed to [`Handlers`].
#[derive(Debug)]
pub struct RequestContext<'a> {
    /// Association identity snapshot.
    pub association: &'a AssociationInfo,
    /// Context the request arrived on.
    pub context: &'a NegotiatedContext,
    /// Parsed command.
    pub command: &'a Command,
}

/// One instance a C-MOVE/C-GET handler wants delivered.
#[derive(Clone, Debug)]
pub struct MoveInstance {
    /// Storage SOP class UID.
    pub sop_class_uid: String,
    /// Storage SOP instance UID.
    pub sop_instance_uid: String,
    /// Full dataset to C-STORE.
    pub dataset: DataSet,
}

/// DIMSE service handlers (SCP side).
///
/// All methods have defaults: C-ECHO succeeds, everything else fails with a
/// standard status. Override what your application serves.
///
/// Native `async fn` is used deliberately (static dispatch only): handler
/// futures are awaited inline by [`run_scp`], never spawned, so no `Send`
/// bound on the futures is required.
#[allow(async_fn_in_trait)]
pub trait Handlers: Send + Sync {
    /// C-ECHO-RQ → status (default: success).
    async fn on_echo(&self, _ctx: &RequestContext<'_>) -> u16 {
        status::SUCCESS
    }

    /// C-STORE-RQ + dataset → status.
    async fn on_store(&self, _ctx: &RequestContext<'_>, _dataset: DataSet) -> u16 {
        status::PROCESSING_FAILURE
    }

    /// C-FIND-RQ + query → final status plus pending matches.
    /// Return a failure status with no matches to fail the whole FIND.
    async fn on_find(&self, _ctx: &RequestContext<'_>, _query: &DataSet) -> (u16, Vec<DataSet>) {
        (status::SOP_CLASS_NOT_SUPPORTED, Vec::new())
    }

    /// C-MOVE-RQ + query + destination AE → final status plus instances
    /// the framework C-STOREs to the destination.
    async fn on_move(
        &self,
        _ctx: &RequestContext<'_>,
        _query: &DataSet,
        _destination: &str,
    ) -> (u16, Vec<MoveInstance>) {
        (status::SOP_CLASS_NOT_SUPPORTED, Vec::new())
    }

    /// C-GET-RQ + query → final status plus instances the framework
    /// C-STOREs back over this same association.
    async fn on_get(
        &self,
        _ctx: &RequestContext<'_>,
        _query: &DataSet,
    ) -> (u16, Vec<MoveInstance>) {
        (status::SOP_CLASS_NOT_SUPPORTED, Vec::new())
    }

    /// N-CREATE-RQ → status + optional response dataset.
    async fn on_n_create(
        &self,
        _ctx: &RequestContext<'_>,
        _dataset: Option<DataSet>,
    ) -> (u16, Option<DataSet>) {
        (status::SOP_CLASS_NOT_SUPPORTED, None)
    }

    /// N-SET-RQ → status + optional response dataset.
    async fn on_n_set(
        &self,
        _ctx: &RequestContext<'_>,
        _dataset: Option<DataSet>,
    ) -> (u16, Option<DataSet>) {
        (status::SOP_CLASS_NOT_SUPPORTED, None)
    }

    /// N-GET-RQ → status + optional response dataset.
    async fn on_n_get(&self, _ctx: &RequestContext<'_>) -> (u16, Option<DataSet>) {
        (status::SOP_CLASS_NOT_SUPPORTED, None)
    }

    /// N-ACTION-RQ → status + optional response dataset + follow-up
    /// SCP-initiated requests, emitted after the RSP on success with
    /// their RSPs awaited (Storage Commitment reports).
    async fn on_n_action(
        &self,
        _ctx: &RequestContext<'_>,
        _dataset: Option<DataSet>,
    ) -> (u16, Option<DataSet>, Vec<ScpFollowUp>) {
        (status::SOP_CLASS_NOT_SUPPORTED, None, Vec::new())
    }

    /// N-DELETE-RQ → status.
    async fn on_n_delete(&self, _ctx: &RequestContext<'_>) -> u16 {
        status::SOP_CLASS_NOT_SUPPORTED
    }

    /// N-EVENT-REPORT-RQ → status.
    async fn on_n_event_report(&self, _ctx: &RequestContext<'_>, _dataset: Option<DataSet>) -> u16 {
        status::SOP_CLASS_NOT_SUPPORTED
    }
}

/// Maps a Move destination AE title to a host/port.
pub trait AeDirectory: Send + Sync {
    /// Resolve `ae` (unpadded) to `(host, port)`.
    fn resolve(&self, ae: &str) -> Option<(String, u16)>;
}

/// Static [`AeDirectory`] backed by a map.
#[derive(Clone, Default)]
pub struct HashMapDirectory {
    /// AE title (unpadded) → `(host, port)`.
    pub map: HashMap<String, (String, u16)>,
}

impl AeDirectory for HashMapDirectory {
    fn resolve(&self, ae: &str) -> Option<(String, u16)> {
        self.map.get(ae.trim()).cloned()
    }
}

/// How long the SCP waits for a C-CANCEL between pending responses.
/// Local peers answer in microseconds; the wait also yields to the
/// executor so the peer task runs between responses.
const CANCEL_POLL_WAIT: Duration = Duration::from_millis(1);

/// Outcome of polling for C-CANCEL during a multi-response operation.
enum CancelPoll {
    /// No C-CANCEL for `message_id`; streaming continues.
    KeepGoing,
    /// C-CANCEL for `message_id` arrived; stop with status CANCEL.
    Cancelled,
    /// The peer released mid-operation; stop without a final response
    /// (the stashed release reaches the dispatch loop next).
    Released,
}

/// Poll once for a C-CANCEL-RQ targeting `message_id`. Anything else
/// the peer sends mid-operation (e.g. RELEASE-RQ) is stashed back for
/// the dispatch loop — never swallowed.
async fn poll_cancel(assoc: &mut Association, message_id: u16) -> Result<CancelPoll> {
    match assoc.poll_event(CANCEL_POLL_WAIT).await? {
        None => Ok(CancelPoll::KeepGoing),
        Some(DimseEvent::Released) => {
            assoc.stash(DimseEvent::Released);
            Ok(CancelPoll::Released)
        }
        Some(DimseEvent::Message(m))
            if m.command.field == CF::C_CANCEL_RQ
                && m.command.message_id_rsp == Some(message_id) =>
        {
            Ok(CancelPoll::Cancelled)
        }
        Some(other) => {
            if let DimseEvent::Message(m) = &other {
                tracing::debug!(
                    "stashing {} received mid-operation for message {message_id}",
                    m.command.name()
                );
            }
            assoc.stash(other);
            Ok(CancelPoll::KeepGoing)
        }
    }
}

/// An SCP-initiated request emitted after the RSP, with its RSP
/// awaited before dispatch continues. The framework assigns the
/// message ID at send time.
#[derive(Clone, Debug)]
pub enum ScpFollowUp {
    /// N-EVENT-REPORT-RQ (Storage Commitment today; UPS later).
    EventReport {
        /// Affected SOP Class UID.
        sop_class_uid: String,
        /// Affected SOP Instance UID.
        affected_sop_instance_uid: String,
        /// Event Type ID.
        event_type_id: u16,
        /// Report dataset, if any.
        dataset: Option<DataSet>,
    },
}

/// How the SCP performs C-MOVE sub-operations.
#[derive(Clone)]
pub struct ScpMoveConfig {
    /// Destination directory.
    pub directory: Arc<dyn AeDirectory>,
    /// Options for the outbound (SCU) association.
    pub assoc_options: crate::association::AssociationOptions,
    /// Calling AE used on the outbound association.
    pub calling_ae: AeTitle,
}

/// SCP run options.
#[derive(Clone, Default)]
pub struct ScpOptions {
    /// C-MOVE delivery config; without it, C-MOVE fails cleanly.
    pub move_config: Option<ScpMoveConfig>,
}

/// Accept one inbound connection: negotiate, then run to release/abort.
pub async fn accept<S, H>(
    stream: S,
    acceptor: &AcceptorOptions,
    handlers: &H,
    scp_opts: &ScpOptions,
) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    H: Handlers,
{
    let mut stream: Box<dyn crate::association::AsyncReadWrite> = Box::new(stream);
    let first = timeout(
        acceptor.association.read_timeout,
        crate::pdu::read_pdu(&mut stream, acceptor.association.max_incoming_pdu),
    )
    .await
    .map_err(|_| Error::Timeout("association request"))??;
    let Pdu::AssociateRq(rq) = first else {
        crate::pdu::write_pdu(
            &mut stream,
            &Pdu::Abort {
                source: 2,
                reason: 2,
            },
        )
        .await
        .ok();
        return Err(Error::UnexpectedPdu {
            expected: "A-ASSOCIATE-RQ",
            got: first.name().to_string(),
        });
    };
    let (ac, contexts) = match negotiate(&rq, acceptor) {
        Ok(ok) => ok,
        Err(rj) => {
            let err = Error::Rejected {
                result: rj.result,
                origin: rj.source,
                reason: rj.reason,
            };
            crate::pdu::write_pdu(&mut stream, &Pdu::AssociateRj(rj)).await?;
            return Err(err);
        }
    };
    crate::pdu::write_pdu(&mut stream, &Pdu::AssociateAc(ac)).await?;
    let info = AssociationInfo {
        local_ae: acceptor.ae_title.clone(),
        peer_ae: rq.calling_ae.clone(),
        peer_max_pdu: rq.max_pdu_len,
    };
    let mut assoc = Association::from_accepted(stream, info, contexts, &acceptor.association);
    run_scp(&mut assoc, handlers, scp_opts).await
}

/// Run the SCP dispatch loop until release, abort or transport error.
pub async fn run_scp<H: Handlers>(
    assoc: &mut Association,
    handlers: &H,
    opts: &ScpOptions,
) -> Result<()> {
    loop {
        match assoc.recv().await? {
            DimseEvent::Released => return Ok(()),
            DimseEvent::Message(msg) => {
                if let Err(e) = dispatch(assoc, handlers, opts, *msg).await {
                    tracing::warn!("SCP dispatch failed: {e}");
                    assoc.abort(2, 0).await.ok();
                    return Err(e);
                }
            }
        }
    }
}

async fn dispatch<H: Handlers>(
    assoc: &mut Association,
    handlers: &H,
    opts: &ScpOptions,
    msg: DimseMessage,
) -> Result<()> {
    let ctx_id = msg.context_id;
    let context = assoc
        .context_by_id(ctx_id)
        .ok_or(Error::UnknownContext(ctx_id))?
        .clone();
    let info = assoc.info().clone();
    let rctx = RequestContext {
        association: &info,
        context: &context,
        command: &msg.command,
    };
    let cmd = &msg.command;
    let rsp_id = cmd.message_id.unwrap_or(0);

    match cmd.field {
        CF::C_ECHO_RQ => {
            let s = handlers.on_echo(&rctx).await;
            assoc
                .send(ctx_id, &Command::echo_rsp(rsp_id, s), None)
                .await?;
        }
        CF::C_STORE_RQ => {
            let Some(ds) = msg.dataset else {
                let rsp = Command::store_rsp(
                    rsp_id,
                    cmd.affected_sop_class_uid.clone(),
                    None,
                    status::PROCESSING_FAILURE,
                );
                assoc.send(ctx_id, &rsp, None).await?;
                return Ok(());
            };
            let s = handlers.on_store(&rctx, ds).await;
            let rsp = Command::store_rsp(
                rsp_id,
                cmd.affected_sop_class_uid.clone(),
                cmd.affected_sop_instance_uid.clone(),
                s,
            );
            assoc.send(ctx_id, &rsp, None).await?;
        }
        CF::C_FIND_RQ => {
            let query = msg.dataset.unwrap_or_default();
            let (final_status, matches) = handlers.on_find(&rctx, &query).await;
            if status::is_failure(final_status) || status::is_cancel(final_status) {
                let rsp = Command::find_rsp(
                    rsp_id,
                    cmd.affected_sop_class_uid.clone(),
                    final_status,
                    false,
                );
                assoc.send(ctx_id, &rsp, None).await?;
                return Ok(());
            }
            for m in &matches {
                let rsp = Command::find_rsp(
                    rsp_id,
                    cmd.affected_sop_class_uid.clone(),
                    status::PENDING,
                    true,
                );
                assoc.send(ctx_id, &rsp, Some(m)).await?;
                match poll_cancel(assoc, rsp_id).await? {
                    CancelPoll::KeepGoing => {}
                    CancelPoll::Released => return Ok(()),
                    CancelPoll::Cancelled => {
                        let rsp = Command::find_rsp(
                            rsp_id,
                            cmd.affected_sop_class_uid.clone(),
                            status::CANCEL,
                            false,
                        );
                        assoc.send(ctx_id, &rsp, None).await?;
                        return Ok(());
                    }
                }
            }
            let rsp = Command::find_rsp(
                rsp_id,
                cmd.affected_sop_class_uid.clone(),
                final_status,
                false,
            );
            assoc.send(ctx_id, &rsp, None).await?;
        }
        CF::C_CANCEL_RQ => {
            // In-flight operations consume their own CANCEL via
            // poll_cancel; one arriving here targets a completed (or
            // unknown) operation and is correctly ignored.
            tracing::debug!("C-CANCEL-RQ for a completed operation ignored");
        }
        CF::C_MOVE_RQ => {
            handle_move(assoc, handlers, opts, &rctx, &msg, ctx_id).await?;
        }
        CF::C_GET_RQ => {
            handle_get(assoc, handlers, &rctx, &msg, ctx_id).await?;
        }
        CF::N_CREATE_RQ => {
            let (s, ds) = handlers.on_n_create(&rctx, msg.dataset).await;
            let rsp = Command::n_create_rsp(
                rsp_id,
                cmd.affected_sop_class_uid.clone(),
                cmd.affected_sop_instance_uid.clone(),
                s,
            );
            send_optional(assoc, ctx_id, rsp, ds).await?;
        }
        CF::N_SET_RQ => {
            let (s, ds) = handlers.on_n_set(&rctx, msg.dataset).await;
            let rsp = Command::n_set_rsp(
                rsp_id,
                cmd.affected_sop_class_uid.clone(),
                cmd.affected_sop_instance_uid.clone(),
                s,
            );
            send_optional(assoc, ctx_id, rsp, ds).await?;
        }
        CF::N_GET_RQ => {
            let (s, ds) = handlers.on_n_get(&rctx).await;
            let rsp = Command::n_get_rsp(
                rsp_id,
                cmd.affected_sop_class_uid.clone(),
                cmd.affected_sop_instance_uid.clone(),
                s,
                ds.is_some(),
            );
            assoc.send(ctx_id, &rsp, ds.as_ref()).await?;
        }
        CF::N_ACTION_RQ => {
            let (s, ds, follow_ups) = handlers.on_n_action(&rctx, msg.dataset).await;
            let rsp = Command::n_action_rsp(rsp_id, s);
            send_optional(assoc, ctx_id, rsp, ds).await?;
            if status::is_success(s) {
                for f in follow_ups {
                    emit_follow_up(assoc, ctx_id, f).await?;
                }
            }
        }
        CF::N_DELETE_RQ => {
            let s = handlers.on_n_delete(&rctx).await;
            assoc
                .send(ctx_id, &Command::n_delete_rsp(rsp_id, s), None)
                .await?;
        }
        CF::N_EVENT_REPORT_RQ => {
            let s = handlers.on_n_event_report(&rctx, msg.dataset).await;
            assoc
                .send(ctx_id, &Command::n_event_report_rsp(rsp_id, s), None)
                .await?;
        }
        _ => {
            return Err(Error::BadCommand(format!(
                "unrecognised command field {:#06X}",
                cmd.field
            )));
        }
    }
    Ok(())
}

/// Emit one SCP-initiated follow-up and await its RSP. Anything
/// but the matching RSP (e.g. a release) is stashed for the
/// dispatch loop — never swallowed.
async fn emit_follow_up(assoc: &mut Association, ctx_id: u8, f: ScpFollowUp) -> Result<()> {
    let ScpFollowUp::EventReport {
        sop_class_uid,
        affected_sop_instance_uid,
        event_type_id,
        dataset,
    } = f;
    let id = assoc.next_message_id();
    let rq = Command::n_event_report_rq(
        id,
        sop_class_uid,
        affected_sop_instance_uid,
        event_type_id,
        dataset.is_some(),
    );
    assoc.send(ctx_id, &rq, dataset.as_ref()).await?;
    match assoc.recv().await? {
        DimseEvent::Message(m)
            if m.command.field == CF::N_EVENT_REPORT_RSP
                && m.command.message_id_rsp == Some(id) =>
        {
            tracing::debug!(
                "follow-up N-EVENT-REPORT acked: {:#06X}",
                m.command.status.unwrap_or(0)
            );
        }
        DimseEvent::Released => {
            assoc.stash(DimseEvent::Released);
        }
        DimseEvent::Message(other) => {
            tracing::warn!(
                "stashing {} instead of a follow-up RSP",
                other.command.name()
            );
            assoc.stash(DimseEvent::Message(other));
        }
    }
    Ok(())
}

async fn send_optional(
    assoc: &mut Association,
    ctx_id: u8,
    mut cmd: Command,
    ds: Option<DataSet>,
) -> Result<()> {
    cmd.data_set_type = if ds.is_some() {
        crate::dimse::DATA_SET_PRESENT
    } else {
        crate::dimse::DATA_SET_NULL
    };
    assoc.send(ctx_id, &cmd, ds.as_ref()).await
}

#[allow(clippy::too_many_arguments)]
async fn handle_move<H: Handlers>(
    assoc: &mut Association,
    handlers: &H,
    opts: &ScpOptions,
    rctx: &RequestContext<'_>,
    msg: &DimseMessage,
    ctx_id: u8,
) -> Result<()> {
    let cmd = msg.command.clone();
    let rsp_id = cmd.message_id.unwrap_or(0);
    let dest = cmd.move_destination.clone().unwrap_or_default();
    let query = msg.dataset.clone().unwrap_or_default();
    let (handler_status, instances) = handlers.on_move(rctx, &query, dest.trim()).await;
    let sop = cmd.affected_sop_class_uid.clone();

    let fail_final = async |assoc: &mut Association, s: u16| -> Result<()> {
        let rsp = Command::move_get_rsp(true, rsp_id, sop.clone(), s, 0, 0, 0, 0);
        assoc.send(ctx_id, &rsp, None).await
    };
    if status::is_failure(handler_status) || status::is_cancel(handler_status) {
        return fail_final(assoc, handler_status).await;
    }
    let Some(move_cfg) = &opts.move_config else {
        tracing::warn!("C-MOVE without move_config; failing");
        return fail_final(assoc, status::MOVE_DESTINATION_UNKNOWN).await;
    };
    let Some((host, port)) = move_cfg.directory.resolve(dest.trim()) else {
        return fail_final(assoc, status::MOVE_DESTINATION_UNKNOWN).await;
    };

    // One outbound association carrying every needed storage class.
    let mut classes: Vec<&str> = Vec::new();
    for inst in &instances {
        if !classes.contains(&inst.sop_class_uid.as_str()) {
            classes.push(inst.sop_class_uid.as_str());
        }
    }
    let ts = [
        vyna_dcm_core::TransferSyntax::ExplicitVrLittleEndian,
        vyna_dcm_core::TransferSyntax::ImplicitVrLittleEndian,
    ];
    let dest_ae = AeTitle::new(dest.trim()).unwrap_or_else(|_| AeTitle::new("MOVEDEST").unwrap());
    let Ok(proposed) = propose(&classes, &ts) else {
        tracing::warn!("C-MOVE proposes too many storage classes; failing");
        return fail_final(assoc, status::PROCESSING_FAILURE).await;
    };
    let mut outbound = match Association::request(
        &move_cfg.assoc_options,
        &move_cfg.calling_ae,
        &dest_ae,
        &format!("{host}:{port}"),
        proposed,
    )
    .await
    {
        Ok(a) => a,
        Err(e) => {
            tracing::warn!("C-MOVE outbound association failed: {e}");
            return fail_final(assoc, status::PROCESSING_FAILURE).await;
        }
    };

    let (mut done_ok, mut done_fail, mut done_warn) = (0u16, 0u16, 0u16);
    for (i, inst) in instances.iter().enumerate() {
        let remaining = (instances.len() - i - 1) as u16;
        let s = outbound
            .store(
                &inst.dataset,
                &inst.sop_class_uid,
                &inst.sop_instance_uid,
                priority::MEDIUM,
            )
            .await
            .unwrap_or(status::PROCESSING_FAILURE);
        if status::is_success(s) {
            done_ok += 1;
        } else if status::is_warning(s) {
            done_warn += 1;
        } else {
            done_fail += 1;
        }
        let rsp = Command::move_get_rsp(
            true,
            rsp_id,
            sop.clone(),
            status::PENDING,
            remaining,
            done_ok,
            done_fail,
            done_warn,
        );
        assoc.send(ctx_id, &rsp, None).await?;
        match poll_cancel(assoc, rsp_id).await? {
            CancelPoll::KeepGoing => {}
            CancelPoll::Released => {
                outbound.release().await.ok();
                return Ok(());
            }
            CancelPoll::Cancelled => {
                outbound.release().await.ok();
                let rsp = Command::move_get_rsp(
                    true,
                    rsp_id,
                    sop,
                    status::CANCEL,
                    remaining,
                    done_ok,
                    done_fail,
                    done_warn,
                );
                assoc.send(ctx_id, &rsp, None).await?;
                return Ok(());
            }
        }
    }
    outbound.release().await.ok();
    let rsp = Command::move_get_rsp(
        true,
        rsp_id,
        sop,
        status::SUCCESS,
        0,
        done_ok,
        done_fail,
        done_warn,
    );
    assoc.send(ctx_id, &rsp, None).await?;
    Ok(())
}

async fn handle_get<H: Handlers>(
    assoc: &mut Association,
    handlers: &H,
    rctx: &RequestContext<'_>,
    msg: &DimseMessage,
    ctx_id: u8,
) -> Result<()> {
    let cmd = msg.command.clone();
    let rsp_id = cmd.message_id.unwrap_or(0);
    let query = msg.dataset.clone().unwrap_or_default();
    let (handler_status, instances) = handlers.on_get(rctx, &query).await;
    let sop = cmd.affected_sop_class_uid.clone();

    if status::is_failure(handler_status) || status::is_cancel(handler_status) {
        let rsp = Command::move_get_rsp(false, rsp_id, sop, handler_status, 0, 0, 0, 0);
        assoc.send(ctx_id, &rsp, None).await?;
        return Ok(());
    }
    let (mut done_ok, mut done_fail, mut done_warn) = (0u16, 0u16, 0u16);
    for (i, inst) in instances.iter().enumerate() {
        let remaining = (instances.len() - i - 1) as u16;
        let Some(st_ctx) = assoc.context_for(&inst.sop_class_uid).cloned() else {
            done_fail += 1;
            let rsp = Command::move_get_rsp(
                false,
                rsp_id,
                sop.clone(),
                status::PENDING,
                remaining,
                done_ok,
                done_fail,
                done_warn,
            );
            assoc.send(ctx_id, &rsp, None).await?;
            continue;
        };
        let store_id = assoc.next_message_id();
        let store_cmd = Command::store_rq(
            store_id,
            priority::MEDIUM,
            &inst.sop_class_uid,
            &inst.sop_instance_uid,
        );
        assoc
            .send(st_ctx.id, &store_cmd, Some(&inst.dataset))
            .await?;
        // Await the C-STORE-RSP (a C-CANCEL for the GET aborts delivery).
        // Single receive: anything else aborts the GET, so no retry loop.
        let store_status = match assoc.recv().await? {
            DimseEvent::Message(m)
                if m.command.field == CF::C_STORE_RSP
                    && m.command.message_id_rsp == Some(store_id) =>
            {
                m.command.status.unwrap_or(status::PROCESSING_FAILURE)
            }
            DimseEvent::Message(m)
                if m.command.field == CF::C_CANCEL_RQ
                    && m.command.message_id_rsp == Some(rsp_id) =>
            {
                let rsp = Command::move_get_rsp(
                    false,
                    rsp_id,
                    sop,
                    status::CANCEL,
                    remaining,
                    done_ok,
                    done_fail,
                    done_warn,
                );
                assoc.send(ctx_id, &rsp, None).await?;
                return Ok(());
            }
            DimseEvent::Message(m) => {
                return Err(Error::UnexpectedPdu {
                    expected: "C-STORE-RSP",
                    got: m.command.name().to_string(),
                });
            }
            DimseEvent::Released => return Err(Error::Released),
        };
        if status::is_success(store_status) {
            done_ok += 1;
        } else if status::is_warning(store_status) {
            done_warn += 1;
        } else {
            done_fail += 1;
        }
        let rsp = Command::move_get_rsp(
            false,
            rsp_id,
            sop.clone(),
            status::PENDING,
            remaining,
            done_ok,
            done_fail,
            done_warn,
        );
        assoc.send(ctx_id, &rsp, None).await?;
    }
    let rsp = Command::move_get_rsp(
        false,
        rsp_id,
        sop,
        status::SUCCESS,
        0,
        done_ok,
        done_fail,
        done_warn,
    );
    assoc.send(ctx_id, &rsp, None).await?;
    Ok(())
}

/// Short alias for tests and tools.
#[allow(dead_code)]
pub(crate) fn default_read_timeout() -> Duration {
    Duration::from_secs(300)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pdu::{AssociateRq, ProposedContext};

    fn sample_rq() -> AssociateRq {
        AssociateRq {
            called_ae: AeTitle::new("VYNA_SCP").unwrap(),
            calling_ae: AeTitle::new("TEST_SCU").unwrap(),
            application_context: "1.2.840.10008.3.1.1.1".into(),
            presentation_contexts: vec![ProposedContext {
                id: 1,
                abstract_syntax: "1.2.840.10008.1.1".into(),
                transfer_syntaxes: vec!["1.2.840.10008.1.2.1".into()],
            }],
            max_pdu_len: 16384,
            implementation_class_uid: "1.2.3".into(),
            implementation_version: None,
            async_ops: None,
            role_selection: vec![],
            user_identity: None,
        }
    }

    #[test]
    fn negotiates_happy_path() {
        let opts = AcceptorOptions::default();
        let (ac, ctxs) = negotiate(&sample_rq(), &opts).unwrap();
        assert_eq!(ac.presentation_contexts[0].result, 0);
        assert_eq!(ctxs.len(), 1);
        assert_eq!(ctxs[0].transfer_syntax_uid, "1.2.840.10008.1.2.1");
    }

    #[test]
    fn rejects_bad_context_and_ae() {
        let opts = AcceptorOptions::default();
        let mut rq = sample_rq();
        rq.application_context = "1.2.3".into();
        assert_eq!(negotiate(&rq, &opts).unwrap_err().reason, 2);

        let strict = AcceptorOptions {
            check_called_ae: true,
            ..Default::default()
        };
        let mut rq = sample_rq();
        rq.called_ae = AeTitle::new("OTHER").unwrap();
        assert_eq!(negotiate(&rq, &strict).unwrap_err().reason, 7);

        let limited = AcceptorOptions {
            accepted_transfer_syntaxes: vec!["1.2.840.10008.1.2".into()],
            ..Default::default()
        };
        let err = negotiate(&sample_rq(), &limited).unwrap_err();
        assert_eq!((err.result, err.source), (1, 1)); // nothing acceptable
    }

    #[test]
    fn propose_builds_odd_ids() {
        let pcs = propose(
            &["1.1", "1.2", "1.3"],
            &[vyna_dcm_core::TransferSyntax::ExplicitVrLittleEndian],
        )
        .unwrap();
        assert_eq!(pcs.iter().map(|p| p.id).collect::<Vec<_>>(), vec![1, 3, 5]);
        // 128 classes use IDs 1..=255 exactly; the 129th errors
        // instead of wrapping to a duplicate ID.
        let many: Vec<String> = (0..129).map(|i| format!("1.2.{i}")).collect();
        let many_ref: Vec<&str> = many.iter().map(String::as_str).collect();
        let pcs = propose(
            &many_ref[..128],
            &[vyna_dcm_core::TransferSyntax::ExplicitVrLittleEndian],
        )
        .unwrap();
        assert_eq!(pcs.len(), 128);
        assert_eq!(pcs[127].id, 255);
        assert!(
            propose(
                &many_ref,
                &[vyna_dcm_core::TransferSyntax::ExplicitVrLittleEndian]
            )
            .is_err()
        );
    }
}
