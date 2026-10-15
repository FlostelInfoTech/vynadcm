//! SCP side: association negotiation, service handlers, dispatch.
//!
//! - [`AcceptorOptions`] + [`negotiate`] decide A-ASSOCIATE-AC/RJ.
//! - [`Handlers`] implements the DIMSE services; override what you need,
//!   the defaults answer C-ECHO and fail the rest with standard statuses.
//! - [`accept`] runs one inbound connection to release/abort.
//! - [`run_scp`] runs the dispatch loop on an established [`Association`].
//!
//! SCU operations are [`Association`] methods in `scu`.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::time::timeout;
use vyna_dcm_core::DataSet;

use crate::{
    AeTitle, Error, Result,
    association::{
        Association, AssociationInfo, DimseEvent, MAX_STASHED_BYTES, MAX_STASHED_EVENTS,
        NegotiatedContext,
    },
    dimse::{Command, DimseMessage, command_field as CF, priority, status},
    pdu::{
        AcceptedContext, AssociateAc, AssociateRj, Pdu, ProposedContext, relational_acceptance,
        relational_requested,
    },
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
#[non_exhaustive]
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
    /// How long to wait for the initial A-ASSOCIATE-RQ on a new
    /// connection (ARTIM-style: PS3.8 §9.1.5, 30 s). Separate from
    /// [`AssociationOptions::read_timeout`](crate::AssociationOptions::read_timeout):
    /// a peer that connects and goes silent must not hold an
    /// association slot for the full inactivity budget.
    pub associate_timeout: Duration,
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
            associate_timeout: Duration::from_secs(30),
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

    // Presentation-context IDs are the initiator's responsibility
    // (PS3.8 §9.3.2.2): odd and nonzero, each used once. A peer that
    // violates this is protocol-broken, so reject the association
    // rather than accepting contexts we cannot address unambiguously.
    let mut seen = [false; 256];
    for prop in &rq.presentation_contexts {
        if prop.id == 0 || prop.id % 2 == 0 {
            return Err(reject(1, 1, 1)); // malformed presentation-context id
        }
        if seen[prop.id as usize] {
            return Err(reject(1, 1, 1)); // duplicate presentation-context id
        }
        seen[prop.id as usize] = true;
    }

    let mut contexts = Vec::new();
    let mut pcs = Vec::new();
    let mut extended = Vec::new();
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
                    // Relational queries are accepted per QR context
                    // when proposed (bit 0); anything else (non-QR
                    // SOP, unset bit, absent entry) stays hierarchical
                    // and gets no echo.
                    let relational = is_qr_retrieve_sop(&prop.abstract_syntax)
                        && relational_requested(&rq.extended_negotiation, &prop.abstract_syntax);
                    if relational {
                        extended.push(relational_acceptance(&prop.abstract_syntax));
                    }
                    contexts.push(NegotiatedContext {
                        id: prop.id,
                        abstract_syntax: prop.abstract_syntax.clone(),
                        transfer_syntax: ts,
                        transfer_syntax_uid: ts_uid.clone(),
                        relational,
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
            extended_negotiation: extended,
        },
        contexts,
    ))
}

/// SOP classes whose C-FIND/C-MOVE/C-GET accept relational-queries
/// extended negotiation (Study/Patient Root + Patient/Study Only).
fn is_qr_retrieve_sop(sop: &str) -> bool {
    use vyna_dcm_core::sop_class as sc;
    matches!(
        sop,
        sc::PATIENT_ROOT_FIND
            | sc::PATIENT_ROOT_MOVE
            | sc::PATIENT_ROOT_GET
            | sc::STUDY_ROOT_FIND
            | sc::STUDY_ROOT_MOVE
            | sc::STUDY_ROOT_GET
            | sc::PATIENT_STUDY_ONLY_FIND
            | sc::PATIENT_STUDY_ONLY_MOVE
    )
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

/// Outcome of loading one planned retrieve instance.
#[derive(Clone, Debug)]
pub enum RetrieveLoad {
    /// Instance ready to C-STORE.
    Ready(MoveInstance),
    /// This instance failed to load: counted as a failed
    /// sub-operation while the retrieve continues with the rest.
    Failed,
}

/// Loads one planned retrieve instance. The framework calls [`RetrieveLoader::load`]
/// with sequential indices (`0..plan.total`), sending each instance
/// before loading the next, so a large retrieve holds ~one dataset
/// in RAM instead of the whole result set.
pub trait RetrieveLoader: Send + Sync {
    /// Load instance `index` (out of range → [`RetrieveLoad::Failed`]).
    fn load(&self, index: usize) -> Pin<Box<dyn Future<Output = RetrieveLoad> + Send + '_>>;
}

/// Lazy C-MOVE/C-GET match list: cheap upfront metadata plus a
/// per-instance loader. Handlers serving large archives override
/// [`Handlers::on_move_plan`]/[`Handlers::on_get_plan`] to build one
/// without loading any instance data; the framework streams
/// load-and-send through [`RetrieveLoader::load`].
pub struct RetrievePlan {
    /// Total instances (drives the remaining counts).
    pub total: usize,
    /// Distinct storage classes (sizes the MOVE outbound proposal).
    pub storage_classes: Vec<String>,
    loader: Box<dyn RetrieveLoader>,
}

impl RetrievePlan {
    /// Build a plan from parts.
    pub fn new(
        total: usize,
        storage_classes: Vec<String>,
        loader: impl RetrieveLoader + 'static,
    ) -> Self {
        Self {
            total,
            storage_classes,
            loader: Box::new(loader),
        }
    }

    /// Buffer an eager instance list (what the default
    /// [`Handlers::on_move_plan`]/[`Handlers::on_get_plan`] use to
    /// bridge legacy [`Handlers::on_move`]/[`Handlers::on_get`]).
    pub fn buffered(instances: Vec<MoveInstance>) -> Self {
        let mut storage_classes = Vec::new();
        for inst in &instances {
            if !storage_classes.contains(&inst.sop_class_uid) {
                storage_classes.push(inst.sop_class_uid.clone());
            }
        }
        Self {
            total: instances.len(),
            storage_classes,
            loader: Box::new(BufferedLoader { instances }),
        }
    }

    /// Load instance `index` for sending.
    pub async fn load(&self, index: usize) -> RetrieveLoad {
        self.loader.load(index).await
    }
}

/// [`RetrieveLoader`] over an eager [`Vec`].
struct BufferedLoader {
    instances: Vec<MoveInstance>,
}

impl RetrieveLoader for BufferedLoader {
    fn load(&self, index: usize) -> Pin<Box<dyn Future<Output = RetrieveLoad> + Send + '_>> {
        let out = self
            .instances
            .get(index)
            .cloned()
            .map(RetrieveLoad::Ready)
            .unwrap_or(RetrieveLoad::Failed);
        Box::pin(async move { out })
    }
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
    /// Dispatch fails dataset-less requests before this runs, so
    /// `query` always came from a real Identifier.
    /// Return a failure status with no matches to fail the whole FIND.
    /// The status must be final (success/warning/failure/cancel):
    /// `PENDING` is coerced to `SUCCESS` (with a warning logged), since
    /// a PENDING final would hang the peer awaiting more matches.
    async fn on_find(&self, _ctx: &RequestContext<'_>, _query: &DataSet) -> (u16, Vec<DataSet>) {
        (status::SOP_CLASS_NOT_SUPPORTED, Vec::new())
    }

    /// C-MOVE-RQ + query + destination AE → final status plus instances
    /// the framework C-STOREs to the destination. Dispatch fails
    /// dataset-less requests first, so `query` is always real.
    ///
    /// Prefer [`Handlers::on_move_plan`] for large result sets: this
    /// eager form holds every instance in RAM before the first send.
    async fn on_move(
        &self,
        _ctx: &RequestContext<'_>,
        _query: &DataSet,
        _destination: &str,
    ) -> (u16, Vec<MoveInstance>) {
        (status::SOP_CLASS_NOT_SUPPORTED, Vec::new())
    }

    /// C-MOVE-RQ → final status plus a lazy [`RetrievePlan`] the
    /// framework streams load-and-send. The default bridges
    /// [`Handlers::on_move`]; override for large archives.
    async fn on_move_plan(
        &self,
        ctx: &RequestContext<'_>,
        query: &DataSet,
        destination: &str,
    ) -> (u16, RetrievePlan) {
        let (s, instances) = self.on_move(ctx, query, destination).await;
        (s, RetrievePlan::buffered(instances))
    }

    /// C-GET-RQ + query → final status plus instances the framework
    /// C-STOREs back over this same association. Dispatch fails
    /// dataset-less requests first, so `query` is always real.
    ///
    /// Prefer [`Handlers::on_get_plan`] for large result sets: this
    /// eager form holds every instance in RAM before the first send.
    async fn on_get(
        &self,
        _ctx: &RequestContext<'_>,
        _query: &DataSet,
    ) -> (u16, Vec<MoveInstance>) {
        (status::SOP_CLASS_NOT_SUPPORTED, Vec::new())
    }

    /// C-GET-RQ → final status plus a lazy [`RetrievePlan`] the
    /// framework streams load-and-send. The default bridges
    /// [`Handlers::on_get`]; override for large archives.
    async fn on_get_plan(&self, ctx: &RequestContext<'_>, query: &DataSet) -> (u16, RetrievePlan) {
        let (s, instances) = self.on_get(ctx, query).await;
        (s, RetrievePlan::buffered(instances))
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

/// C-CANCEL poll between pending responses is a non-blocking check:
/// a 1 ms sleep here would make a 10k-match FIND take 10+ s. The
/// explicit yield lets the peer task run between responses instead.
const CANCEL_POLL_WAIT: Duration = Duration::ZERO;

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
/// the dispatch loop — never swallowed (past the stash cap the
/// operation fails and the association aborts instead).
async fn poll_cancel(assoc: &mut Association, message_id: u16) -> Result<CancelPoll> {
    // Let the peer task run first so an in-flight C-CANCEL lands in
    // the socket buffer before the non-blocking check below.
    tokio::task::yield_now().await;
    match assoc.poll_event(CANCEL_POLL_WAIT).await? {
        None => Ok(CancelPoll::KeepGoing),
        Some(DimseEvent::Released) => {
            assoc.stash(DimseEvent::Released)?;
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
            assoc.stash(other)?;
            Ok(CancelPoll::KeepGoing)
        }
    }
}

/// An SCP-initiated request emitted after the RSP, with its RSP
/// awaited before dispatch continues. The framework assigns the
/// message ID at send time.
#[derive(Clone, Debug)]
#[non_exhaustive]
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
#[non_exhaustive]
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
        acceptor.associate_timeout,
        crate::pdu::read_pdu(&mut stream, acceptor.association.max_incoming_pdu),
    )
    .await
    .map_err(|_| Error::Timeout("association request"))??;
    let Pdu::AssociateRq(rq) = first else {
        crate::pdu::write_pdu_timeout(
            &mut stream,
            &Pdu::Abort {
                source: 2,
                reason: 2,
            },
            acceptor.association.write_timeout,
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
            crate::pdu::write_pdu_timeout(
                &mut stream,
                &Pdu::AssociateRj(rj),
                acceptor.association.write_timeout,
            )
            .await?;
            return Err(err);
        }
    };
    crate::pdu::write_pdu_timeout(
        &mut stream,
        &Pdu::AssociateAc(ac),
        acceptor.association.write_timeout,
    )
    .await?;
    let info = AssociationInfo {
        local_ae: acceptor.ae_title.clone(),
        peer_ae: rq.calling_ae.clone(),
        peer_max_pdu: rq.max_pdu_len,
    };
    let mut assoc = Association::from_accepted(stream, info, contexts, &acceptor.association);
    run_scp(&mut assoc, handlers, scp_opts).await
}

/// Accept one inbound connection under a concurrency cap: holds a
/// `slots` permit for the association lifetime, refusing with
/// [`Error::TooManyAssociations`] (stream dropped, like the CLI SCPs'
/// over-cap path) when no permit is immediately available.
///
/// Library embedders spawning one task per connection should route
/// every connection through this with a shared semaphore instead of
/// bare [`accept`], which cannot bound concurrency by itself.
pub async fn accept_limited<S, H>(
    stream: S,
    acceptor: &AcceptorOptions,
    handlers: &H,
    scp_opts: &ScpOptions,
    slots: &tokio::sync::Semaphore,
) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    H: Handlers,
{
    let Ok(_permit) = slots.try_acquire() else {
        return Err(Error::TooManyAssociations);
    };
    accept(stream, acceptor, handlers, scp_opts).await
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
                    // Best-effort: the dispatch error is the outcome.
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
    // Message ID is mandatory in requests (PS3.7): a response with ID
    // 0 matches nothing on the peer, so fail closed (abort) instead of
    // emitting an unroutable response. Exception: C-CANCEL-RQ carries
    // only Message ID Being Responded To by design — but that must be
    // present (the cancel arm never responds, so the 0 is unused).
    let rsp_id = match cmd.message_id {
        Some(id) => id,
        None if cmd.field == CF::C_CANCEL_RQ => {
            if cmd.message_id_rsp.is_none() {
                return Err(Error::BadCommand(
                    "C-CANCEL-RQ without Message ID Being Responded To".into(),
                ));
            }
            0
        }
        None => {
            return Err(Error::BadCommand(format!(
                "request without Message ID ({})",
                cmd.name()
            )));
        }
    };
    // The request must name the SOP Class its presentation context was
    // negotiated for: anything else is cross-context service (handlers
    // would serve Y while responses echo X). A missing UID fails too —
    // only C-CANCEL-RQ legitimately carries none.
    if cmd.field != CF::C_CANCEL_RQ {
        let request_uid = match cmd.field {
            CF::N_SET_RQ | CF::N_GET_RQ | CF::N_ACTION_RQ | CF::N_DELETE_RQ => {
                &cmd.requested_sop_class_uid
            }
            _ => &cmd.affected_sop_class_uid,
        };
        match request_uid {
            Some(uid) if *uid == context.abstract_syntax => {}
            Some(uid) => {
                return Err(Error::BadCommand(format!(
                    "SOP Class {uid} requested on context negotiated for {}",
                    context.abstract_syntax
                )));
            }
            None => {
                return Err(Error::BadCommand(format!(
                    "request without SOP Class UID ({})",
                    cmd.name()
                )));
            }
        }
    }

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
            // C-FIND-RQ always carries an Identifier (PS3.4 C.4.1.1):
            // a missing dataset is a protocol violation, not an
            // empty query (which would MATCH-ALL a worklist). It fails
            // as an unusable identifier, like a level-less one.
            let Some(query) = msg.dataset else {
                let rsp = Command::find_rsp(
                    rsp_id,
                    cmd.affected_sop_class_uid.clone(),
                    status::DATASET_MISMATCH,
                    false,
                );
                assoc.send(ctx_id, &rsp, None).await?;
                return Ok(());
            };
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
            // PENDING is not a final status: a handler returning it
            // would hang the peer awaiting more matches, so coerce
            // (loudly) to SUCCESS — the matches already sent stand.
            let final_status = if status::is_pending(final_status) {
                tracing::warn!("C-FIND handler returned PENDING as final; coercing to SUCCESS");
                status::SUCCESS
            } else {
                final_status
            };
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
            // Conformant requests carry Requested UIDs; echo them back as
            // the response's Affected UIDs.
            let rsp = Command::n_set_rsp(
                rsp_id,
                cmd.requested_sop_class_uid.clone(),
                cmd.requested_sop_instance_uid.clone(),
                s,
            );
            send_optional(assoc, ctx_id, rsp, ds).await?;
        }
        CF::N_GET_RQ => {
            // The Requested SOP Instance UID is mandatory: without it
            // the framework must not run the handler and echo a
            // UID-less SUCCESS.
            let (class, inst) = match (
                cmd.requested_sop_class_uid.clone(),
                cmd.requested_sop_instance_uid.clone(),
            ) {
                (Some(c), Some(i)) => (c, i),
                _ => {
                    let rsp = Command::n_get_rsp(
                        rsp_id,
                        cmd.requested_sop_class_uid.clone(),
                        cmd.requested_sop_instance_uid.clone(),
                        status::MISSING_ATTRIBUTE_VALUE,
                        false,
                    );
                    assoc.send(ctx_id, &rsp, None).await?;
                    return Ok(());
                }
            };
            let (s, ds) = handlers.on_n_get(&rctx).await;
            // Conformant requests carry Requested UIDs; the response
            // echoes them as Affected UIDs.
            let rsp = Command::n_get_rsp(rsp_id, Some(class), Some(inst), s, ds.is_some());
            assoc.send(ctx_id, &rsp, ds.as_ref()).await?;
        }
        CF::N_ACTION_RQ => {
            let (s, ds, follow_ups) = handlers.on_n_action(&rctx, msg.dataset).await;
            let rsp = Command::n_action_rsp(
                rsp_id,
                cmd.requested_sop_class_uid.clone(),
                cmd.requested_sop_instance_uid.clone(),
                s,
            );
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
                .send(
                    ctx_id,
                    &Command::n_delete_rsp(
                        rsp_id,
                        cmd.requested_sop_class_uid.clone(),
                        cmd.requested_sop_instance_uid.clone(),
                        s,
                    ),
                    None,
                )
                .await?;
        }
        CF::N_EVENT_REPORT_RQ => {
            // Validate identity BEFORE the handler runs: committing a
            // report the framework cannot route a response to lets the
            // peer observe handler timing for instances it only guesses.
            let Some(sop_class) = cmd.affected_sop_class_uid.clone() else {
                return Err(Error::BadCommand(
                    "N-EVENT-REPORT without Affected SOP Class UID".into(),
                ));
            };
            let Some(sop_instance) = cmd.affected_sop_instance_uid.clone() else {
                return Err(Error::BadCommand(
                    "N-EVENT-REPORT without Affected SOP Instance UID".into(),
                ));
            };
            let s = handlers.on_n_event_report(&rctx, msg.dataset).await;
            // Event reports (unlike the other N-services) carry
            // Affected UIDs in the request itself.
            assoc
                .send(
                    ctx_id,
                    &Command::n_event_report_rsp(rsp_id, Some(sop_class), Some(sop_instance), s),
                    None,
                )
                .await?;
        }
        _ => {
            // No response is addressable for an unknown command, so
            // fail closed: `run_scp` turns this into an A-ABORT.
            return Err(Error::BadCommand(format!(
                "unrecognised command field {:#06X}",
                cmd.field
            )));
        }
    }
    Ok(())
}

/// Emit one SCP-initiated follow-up and await its RSP, looping past
/// strays (a single `recv` would orphan a late RSP to the dispatch
/// loop, which A-ABORTs it). Strays hold aside under the twin budget
/// and stash once the RSP lands — stashing inside the loop would
/// replay them through `recv`'s pending-first queue forever.
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
    let mut strays: Vec<DimseEvent> = Vec::new();
    let mut strays_bytes: u64 = 0;
    loop {
        match assoc.recv().await? {
            DimseEvent::Message(m)
                if m.command.field == CF::N_EVENT_REPORT_RSP
                    && m.command.message_id_rsp == Some(id) =>
            {
                for s in strays {
                    assoc.stash(s)?;
                }
                tracing::debug!(
                    "follow-up N-EVENT-REPORT acked: {:#06X}",
                    m.command.status.unwrap_or(0)
                );
                return Ok(());
            }
            DimseEvent::Message(m) => {
                if strays.len() >= MAX_STASHED_EVENTS {
                    return Err(Error::TooManyStashed {
                        limit: MAX_STASHED_EVENTS,
                    });
                }
                let size = m.dataset.as_ref().map(|ds| ds.approx_bytes()).unwrap_or(0);
                if strays_bytes.saturating_add(size) > MAX_STASHED_BYTES {
                    return Err(Error::StashedTooLarge {
                        limit: MAX_STASHED_BYTES,
                    });
                }
                strays_bytes += size;
                tracing::debug!("holding {} instead of a follow-up RSP", m.command.name());
                strays.push(DimseEvent::Message(m));
            }
            DimseEvent::Released => {
                for s in strays {
                    assoc.stash(s)?;
                }
                assoc.stash(DimseEvent::Released)?;
                return Ok(());
            }
        }
    }
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
    let sop = cmd.affected_sop_class_uid.clone();

    let fail_final = async |assoc: &mut Association, s: u16| -> Result<()> {
        let rsp = Command::move_get_rsp(true, rsp_id, sop.clone(), s, 0, 0, 0, 0);
        assoc.send(ctx_id, &rsp, None).await
    };
    // C-MOVE-RQ always carries an Identifier (PS3.4 C.4.2.1); a
    // missing one fails as an unusable identifier, like retrieve_plan
    // treats a level-less one.
    let Some(query) = msg.dataset.clone() else {
        return fail_final(assoc, status::DATASET_MISMATCH).await;
    };
    let (handler_status, plan) = handlers.on_move_plan(rctx, &query, dest.trim()).await;
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
    let classes: Vec<&str> = plan.storage_classes.iter().map(String::as_str).collect();
    let ts = [
        vyna_dcm_core::TransferSyntax::ExplicitVrLittleEndian,
        vyna_dcm_core::TransferSyntax::ImplicitVrLittleEndian,
    ];
    // Never substitute a fallback AE: associating to the wrong
    // destination would deliver PHI to a stranger.
    let Ok(dest_ae) = AeTitle::new(dest.trim()) else {
        tracing::warn!("C-MOVE destination is not a valid AE title; failing");
        return fail_final(assoc, status::MOVE_DESTINATION_UNKNOWN).await;
    };
    // Empty plan: handler status (a warning survives) with zero
    // counts and no outbound association (a flaky-but-valid
    // destination must not fail a zero-match retrieve).
    if plan.total == 0 {
        let s = move_get_final(handler_status, 0);
        let rsp = Command::move_get_rsp(true, rsp_id, sop, s, 0, 0, 0, 0);
        assoc.send(ctx_id, &rsp, None).await?;
        return Ok(());
    }
    let Ok(proposed) = propose(&classes, &ts) else {
        tracing::warn!("C-MOVE proposes too many storage classes; failing");
        return fail_final(assoc, status::PROCESSING_FAILURE).await;
    };
    let mut outbound = match Association::request(
        &move_cfg.assoc_options,
        &move_cfg.calling_ae,
        &dest_ae,
        // Tuple form: pre-joined "host:port" mis-parses IPv6 literals.
        (host.as_str(), port),
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
    // Stream load-and-send: each instance is loaded, stored, and
    // dropped before the next is touched. Counters saturate: a custom
    // handler planning >65535 instances must not panic or wrap.
    for i in 0..plan.total {
        let remaining = u16::try_from(plan.total.saturating_sub(i + 1)).unwrap_or(u16::MAX);
        let outcome = match plan.load(i).await {
            RetrieveLoad::Ready(inst) => Some(
                match outbound
                    .store(
                        &inst.dataset,
                        &inst.sop_class_uid,
                        &inst.sop_instance_uid,
                        priority::MEDIUM,
                    )
                    .await
                {
                    Ok(s) => s,
                    Err(e) => {
                        // Transport death, not a store failure: release and
                        // fail the whole MOVE. Looping on would block a full
                        // read_timeout per remaining instance on a dead socket.
                        tracing::warn!("C-MOVE outbound transport failed: {e}");
                        // Best-effort: the transport is known-dead already.
                        outbound.release().await.ok();
                        let rsp = Command::move_get_rsp(
                            true,
                            rsp_id,
                            sop,
                            status::PROCESSING_FAILURE,
                            remaining,
                            done_ok,
                            done_fail,
                            done_warn,
                        );
                        assoc.send(ctx_id, &rsp, None).await?;
                        return Ok(());
                    }
                },
            ),
            RetrieveLoad::Failed => {
                done_fail = done_fail.saturating_add(1);
                None
            }
        };
        if let Some(s) = outcome {
            if status::is_success(s) {
                done_ok = done_ok.saturating_add(1);
            } else if status::is_warning(s) {
                done_warn = done_warn.saturating_add(1);
            } else {
                done_fail = done_fail.saturating_add(1);
            }
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
                // Best-effort: the inbound side is gone; outcome fixed.
                outbound.release().await.ok();
                return Ok(());
            }
            CancelPoll::Cancelled => {
                // Best-effort: the CANCEL final below is the outcome.
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
    // Best-effort: all sub-operations concluded; the final below stands
    // regardless of whether the goodbye handshake completes.
    outbound.release().await.ok();
    let final_status = move_get_final(handler_status, done_fail);
    let rsp = Command::move_get_rsp(
        true,
        rsp_id,
        sop,
        final_status,
        0,
        done_ok,
        done_fail,
        done_warn,
    );
    assoc.send(ctx_id, &rsp, None).await?;
    Ok(())
}

/// C-MOVE/C-GET final status: failures surface as B000 (a SUCCESS
/// final with failed sub-operations lies to the caller; PS3.4
/// C.4.2/C.4.3), otherwise a handler-returned warning survives
/// instead of being coerced to SUCCESS.
fn move_get_final(handler_status: u16, done_fail: u16) -> u16 {
    if done_fail > 0 {
        status::SUBOPERATIONS_COMPLETE_WITH_FAILURES
    } else if status::is_warning(handler_status) {
        handler_status
    } else {
        status::SUCCESS
    }
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
    let sop = cmd.affected_sop_class_uid.clone();
    // C-GET-RQ always carries an Identifier (PS3.4 C.4.3.1); a
    // missing one fails as an unusable identifier (see handle_move).
    let Some(query) = msg.dataset.clone() else {
        let rsp = Command::move_get_rsp(false, rsp_id, sop, status::DATASET_MISMATCH, 0, 0, 0, 0);
        assoc.send(ctx_id, &rsp, None).await?;
        return Ok(());
    };
    let (handler_status, plan) = handlers.on_get_plan(rctx, &query).await;

    if status::is_failure(handler_status) || status::is_cancel(handler_status) {
        let rsp = Command::move_get_rsp(false, rsp_id, sop, handler_status, 0, 0, 0, 0);
        assoc.send(ctx_id, &rsp, None).await?;
        return Ok(());
    }
    let (mut done_ok, mut done_fail, mut done_warn) = (0u16, 0u16, 0u16);
    // Stream load-and-send (counters saturate past 65535; see handle_move).
    for i in 0..plan.total {
        let remaining = u16::try_from(plan.total.saturating_sub(i + 1)).unwrap_or(u16::MAX);
        let inst = match plan.load(i).await {
            RetrieveLoad::Ready(inst) => inst,
            RetrieveLoad::Failed => {
                done_fail = done_fail.saturating_add(1);
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
            }
        };
        let Some(st_ctx) = assoc.context_for(&inst.sop_class_uid).cloned() else {
            done_fail = done_fail.saturating_add(1);
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
        )?;
        assoc
            .send(st_ctx.id, &store_cmd, Some(&inst.dataset))
            .await?;
        // Await the C-STORE-RSP (a C-CANCEL for the GET aborts delivery).
        // Stray messages (a C-CANCEL for another op, etc.) are held
        // aside and stashed for the dispatch loop once the RSP lands —
        // stashing inside the loop would replay them through
        // `recv`'s pending-first queue forever. The hold-aside carries
        // the stash caps as a twin budget: past either, the GET fails
        // instead of buffering a flood.
        let mut strays: Vec<DimseEvent> = Vec::new();
        let mut strays_bytes: u64 = 0;
        let store_status = loop {
            match assoc.recv().await? {
                DimseEvent::Message(m)
                    if m.command.field == CF::C_STORE_RSP
                        && m.command.message_id_rsp == Some(store_id) =>
                {
                    for s in strays {
                        assoc.stash(s)?;
                    }
                    break m.command.status.unwrap_or(status::PROCESSING_FAILURE);
                }
                DimseEvent::Message(m)
                    if m.command.field == CF::C_CANCEL_RQ
                        && m.command.message_id_rsp == Some(rsp_id) =>
                {
                    for s in strays {
                        assoc.stash(s)?;
                    }
                    let rsp = Command::move_get_rsp(
                        false,
                        rsp_id,
                        sop.clone(),
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
                    if strays.len() >= MAX_STASHED_EVENTS {
                        return Err(Error::TooManyStashed {
                            limit: MAX_STASHED_EVENTS,
                        });
                    }
                    let size = m.dataset.as_ref().map(|ds| ds.approx_bytes()).unwrap_or(0);
                    if strays_bytes.saturating_add(size) > MAX_STASHED_BYTES {
                        return Err(Error::StashedTooLarge {
                            limit: MAX_STASHED_BYTES,
                        });
                    }
                    strays_bytes += size;
                    tracing::debug!(
                        "holding {} received mid-GET for message {rsp_id}",
                        m.command.name()
                    );
                    strays.push(DimseEvent::Message(m));
                }
                // Peer released mid-GET: stash for the dispatch loop and
                // stop quietly like FIND/MOVE — erroring here would make
                // run_scp send A-ABORT after the release completed.
                DimseEvent::Released => {
                    for s in strays {
                        assoc.stash(s)?;
                    }
                    assoc.stash(DimseEvent::Released)?;
                    return Ok(());
                }
            }
        };
        if status::is_success(store_status) {
            done_ok = done_ok.saturating_add(1);
        } else if status::is_warning(store_status) {
            done_warn = done_warn.saturating_add(1);
        } else {
            done_fail = done_fail.saturating_add(1);
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
        // Like MOVE: poll after every PENDING so a cancel arriving
        // after the final sub-op is honored (CANCEL, not SUCCESS).
        match poll_cancel(assoc, rsp_id).await? {
            CancelPoll::KeepGoing => {}
            CancelPoll::Released => return Ok(()),
            CancelPoll::Cancelled => {
                let rsp = Command::move_get_rsp(
                    false,
                    rsp_id,
                    sop.clone(),
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
    let final_status = move_get_final(handler_status, done_fail);
    let rsp = Command::move_get_rsp(
        false,
        rsp_id,
        sop,
        final_status,
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
    use crate::association::{AssociationOptions, AsyncReadWrite};
    use crate::pdu::{AssociateRq, ProposedContext};
    use std::sync::Mutex;
    use vyna_dcm_core::TransferSyntax;

    /// SCP-side association over a duplex half, with one negotiated
    /// context for `sop`. Returns the association plus the peer half
    /// for observing responses.
    fn duplex_scp(sop: &str) -> (Association, tokio::io::DuplexStream, NegotiatedContext) {
        let (a, b) = tokio::io::duplex(65536);
        let ctx = NegotiatedContext {
            id: 1,
            abstract_syntax: sop.to_string(),
            transfer_syntax: TransferSyntax::ExplicitVrLittleEndian,
            transfer_syntax_uid: TransferSyntax::ExplicitVrLittleEndian.uid().to_string(),
            relational: false,
        };
        let info = AssociationInfo {
            local_ae: AeTitle::new("SCP").unwrap(),
            peer_ae: AeTitle::new("SCU").unwrap(),
            peer_max_pdu: 0,
        };
        let opts = AssociationOptions::default();
        (
            Association::from_accepted(
                Box::new(a) as Box<dyn AsyncReadWrite>,
                info,
                vec![ctx.clone()],
                &opts,
            ),
            b,
            ctx,
        )
    }

    struct ProbeHandlers {
        find_final: u16,
        find_matches: Vec<DataSet>,
        n_get_called: Mutex<bool>,
        event_called: Mutex<bool>,
    }

    impl Handlers for ProbeHandlers {
        async fn on_find(
            &self,
            _ctx: &RequestContext<'_>,
            _query: &DataSet,
        ) -> (u16, Vec<DataSet>) {
            (self.find_final, self.find_matches.clone())
        }

        async fn on_n_get(&self, _ctx: &RequestContext<'_>) -> (u16, Option<DataSet>) {
            *self.n_get_called.lock().unwrap() = true;
            (status::SUCCESS, None)
        }

        async fn on_n_event_report(
            &self,
            _ctx: &RequestContext<'_>,
            _dataset: Option<DataSet>,
        ) -> u16 {
            *self.event_called.lock().unwrap() = true;
            status::SUCCESS
        }
    }

    /// Read response statuses from the peer half until a final.
    async fn rsp_statuses(peer: &mut Association, field: u16, id: u16) -> Vec<u16> {
        let mut out = Vec::new();
        loop {
            let m = peer.recv().await.unwrap();
            let DimseEvent::Message(m) = m else {
                panic!("released")
            };
            assert_eq!(m.command.field, field);
            assert_eq!(m.command.message_id_rsp, Some(id));
            let s = m.command.status.unwrap();
            out.push(s);
            if !status::is_pending(s) {
                return out;
            }
        }
    }

    #[tokio::test]
    async fn dispatch_rejects_idless_and_cross_context_requests() {
        let handlers = ProbeHandlers {
            find_final: status::SUCCESS,
            find_matches: vec![],
            n_get_called: Mutex::new(false),
            event_called: Mutex::new(false),
        };
        let opts = ScpOptions::default();
        // No Message ID: fail closed, no unroutable response.
        let (mut assoc, _b, ctx) = duplex_scp("1.2.840.10008.1.1");
        let mut cmd = Command::echo_rq(1);
        cmd.message_id = None;
        let msg = DimseMessage {
            context_id: ctx.id,
            command: cmd,
            dataset: None,
        };
        let err = dispatch(&mut assoc, &handlers, &opts, msg)
            .await
            .unwrap_err();
        assert!(matches!(err, Error::BadCommand(_)), "{err:?}");
        // SOP Class outside the negotiated context: fail closed.
        let (mut assoc, _b, ctx) = duplex_scp("1.2.840.10008.5.1.4.1.1.2");
        let msg = DimseMessage {
            context_id: ctx.id,
            command: Command::find_rq(7, priority::MEDIUM, "1.2.840.10008.5.1.4.1.1.4").unwrap(),
            dataset: Some(DataSet::new()),
        };
        let err = dispatch(&mut assoc, &handlers, &opts, msg)
            .await
            .unwrap_err();
        assert!(matches!(err, Error::BadCommand(_)), "{err:?}");
    }

    #[tokio::test]
    async fn find_pending_final_coerces_to_success() {
        let mut one = DataSet::new();
        one.put_text(
            vyna_dcm_core::tags::PATIENT_NAME,
            vyna_dcm_core::Vr::Pn,
            "Doe^Jane",
        );
        let handlers = ProbeHandlers {
            find_final: status::PENDING,
            find_matches: vec![one],
            n_get_called: Mutex::new(false),
            event_called: Mutex::new(false),
        };
        let opts = ScpOptions::default();
        let sop = "1.2.840.10008.5.1.4.1.2.2.1";
        let (mut assoc, b, ctx) = duplex_scp(sop);
        let msg = DimseMessage {
            context_id: ctx.id,
            command: Command::find_rq(3, priority::MEDIUM, sop).unwrap(),
            dataset: Some(DataSet::new()),
        };
        dispatch(&mut assoc, &handlers, &opts, msg).await.unwrap();
        drop(assoc);
        let info = AssociationInfo {
            local_ae: AeTitle::new("SCU").unwrap(),
            peer_ae: AeTitle::new("SCP").unwrap(),
            peer_max_pdu: 0,
        };
        let mut peer = Association::from_accepted(
            Box::new(b) as Box<dyn AsyncReadWrite>,
            info,
            vec![ctx],
            &AssociationOptions::default(),
        );
        // One PENDING-with-dataset, then a SUCCESS final (not PENDING).
        assert_eq!(
            rsp_statuses(&mut peer, CF::C_FIND_RSP, 3).await,
            vec![status::PENDING, status::SUCCESS]
        );
    }

    #[tokio::test]
    async fn n_services_validate_identity_before_handlers() {
        let handlers = ProbeHandlers {
            find_final: status::SUCCESS,
            find_matches: vec![],
            n_get_called: Mutex::new(false),
            event_called: Mutex::new(false),
        };
        let opts = ScpOptions::default();
        // N-GET without Requested SOP Instance UID: 0121, no handler run.
        let sop = "1.2.840.10008.3.1.2.3.1";
        let (mut assoc, b, ctx) = duplex_scp(sop);
        let mut cmd = Command::n_get_rq(5, sop, "1.2.9", vec![]);
        cmd.requested_sop_instance_uid = None;
        let msg = DimseMessage {
            context_id: ctx.id,
            command: cmd,
            dataset: None,
        };
        dispatch(&mut assoc, &handlers, &opts, msg).await.unwrap();
        assert!(!*handlers.n_get_called.lock().unwrap());
        drop(assoc);
        let info = AssociationInfo {
            local_ae: AeTitle::new("SCU").unwrap(),
            peer_ae: AeTitle::new("SCP").unwrap(),
            peer_max_pdu: 0,
        };
        let mut peer = Association::from_accepted(
            Box::new(b) as Box<dyn AsyncReadWrite>,
            info,
            vec![ctx],
            &AssociationOptions::default(),
        );
        assert_eq!(
            rsp_statuses(&mut peer, CF::N_GET_RSP, 5).await,
            vec![status::MISSING_ATTRIBUTE_VALUE]
        );
        // N-EVENT-REPORT without Affected UIDs: abort, no handler run.
        let sop = "1.2.840.10008.1.20.1";
        let (mut assoc, _b, ctx) = duplex_scp(sop);
        let mut cmd = Command::n_event_report_rq(6, sop, "1.2.8", 1, false);
        cmd.affected_sop_instance_uid = None;
        let msg = DimseMessage {
            context_id: ctx.id,
            command: cmd,
            dataset: None,
        };
        let err = dispatch(&mut assoc, &handlers, &opts, msg)
            .await
            .unwrap_err();
        assert!(matches!(err, Error::BadCommand(_)), "{err:?}");
        assert!(!*handlers.event_called.lock().unwrap());
    }

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
            extended_negotiation: vec![],
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
    fn negotiates_relational_for_qr_contexts() {
        use vyna_dcm_core::sop_class as sc;
        let opts = AcceptorOptions::default();
        // QR + bit set: echoed + flagged.
        let mut rq = sample_rq();
        rq.presentation_contexts[0].abstract_syntax = sc::STUDY_ROOT_FIND.to_string();
        rq.extended_negotiation = vec![crate::pdu::ExtendedNegotiation {
            sop_class_uid: sc::STUDY_ROOT_FIND.to_string(),
            app_info: vec![1],
        }];
        let (ac, ctxs) = negotiate(&rq, &opts).unwrap();
        assert!(ctxs[0].relational);
        assert_eq!(ac.extended_negotiation.len(), 1);
        assert_eq!(
            ac.extended_negotiation[0].sop_class_uid,
            sc::STUDY_ROOT_FIND
        );
        assert_eq!(ac.extended_negotiation[0].app_info, vec![1]);
        // Bit unset: no echo, stays hierarchical.
        rq.extended_negotiation[0].app_info = vec![0];
        let (ac, ctxs) = negotiate(&rq, &opts).unwrap();
        assert!(!ctxs[0].relational);
        assert!(ac.extended_negotiation.is_empty());
        // Non-QR SOP with bit set: ignored entirely.
        rq.presentation_contexts[0].abstract_syntax = sc::CT_IMAGE.to_string();
        rq.extended_negotiation = vec![crate::pdu::ExtendedNegotiation {
            sop_class_uid: sc::CT_IMAGE.to_string(),
            app_info: vec![1],
        }];
        let (ac, ctxs) = negotiate(&rq, &opts).unwrap();
        assert!(!ctxs[0].relational);
        assert!(ac.extended_negotiation.is_empty());
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
    fn rejects_malformed_context_ids() {
        let opts = AcceptorOptions::default();
        for bad in [0u8, 2, 4, 254] {
            let mut rq = sample_rq();
            rq.presentation_contexts[0].id = bad;
            let err = negotiate(&rq, &opts).unwrap_err();
            assert_eq!((err.result, err.source, err.reason), (1, 1, 1), "id {bad}");
        }
        // Duplicates: same odd id twice.
        let mut rq = sample_rq();
        let dup = rq.presentation_contexts[0].clone();
        rq.presentation_contexts.push(dup);
        let err = negotiate(&rq, &opts).unwrap_err();
        assert_eq!((err.result, err.source, err.reason), (1, 1, 1));
        // Distinct odd ids still negotiate.
        let mut rq = sample_rq();
        let mut second = rq.presentation_contexts[0].clone();
        second.id = 3;
        rq.presentation_contexts.push(second);
        let (_, ctxs) = negotiate(&rq, &opts).unwrap();
        assert_eq!(ctxs.len(), 2);
    }

    #[tokio::test]
    async fn dispatch_rejects_missing_sop_class_uid() {
        let handlers = ProbeHandlers {
            find_final: status::SUCCESS,
            find_matches: vec![],
            n_get_called: Mutex::new(false),
            event_called: Mutex::new(false),
        };
        let opts = ScpOptions::default();
        // UID-less requests fail closed (only C-CANCEL may omit UIDs).
        let sop = "1.2.840.10008.5.1.4.1.2.2.1";
        let (mut assoc, _b, ctx) = duplex_scp(sop);
        let mut cmd = Command::find_rq(7, priority::MEDIUM, sop).unwrap();
        cmd.affected_sop_class_uid = None;
        let msg = DimseMessage {
            context_id: ctx.id,
            command: cmd,
            dataset: Some(DataSet::new()),
        };
        let err = dispatch(&mut assoc, &handlers, &opts, msg)
            .await
            .unwrap_err();
        assert!(matches!(err, Error::BadCommand(_)), "{err:?}");
        // C-CANCEL-RQ without UIDs is still accepted-and-ignored.
        let (mut assoc, _b, ctx) = duplex_scp(sop);
        let msg = DimseMessage {
            context_id: ctx.id,
            command: Command::cancel_rq(7),
            dataset: None,
        };
        dispatch(&mut assoc, &handlers, &opts, msg).await.unwrap();
    }

    /// C-MOVE/C-GET handlers answering with a warning and (for MOVE)
    /// an empty plan.
    struct WarnPlan;

    impl Handlers for WarnPlan {
        async fn on_move(
            &self,
            _ctx: &RequestContext<'_>,
            _query: &DataSet,
            _destination: &str,
        ) -> (u16, Vec<MoveInstance>) {
            (status::ELEMENTS_DISCARDED, Vec::new())
        }

        async fn on_get(
            &self,
            _ctx: &RequestContext<'_>,
            _query: &DataSet,
        ) -> (u16, Vec<MoveInstance>) {
            (status::ELEMENTS_DISCARDED, Vec::new())
        }
    }

    /// Read the single final C-MOVE/C-GET-RSP status off the peer half.
    async fn final_move_get_status(
        b: tokio::io::DuplexStream,
        ctx: NegotiatedContext,
        field: u16,
        id: u16,
    ) -> u16 {
        let info = AssociationInfo {
            local_ae: AeTitle::new("SCU").unwrap(),
            peer_ae: AeTitle::new("SCP").unwrap(),
            peer_max_pdu: 0,
        };
        let mut peer = Association::from_accepted(
            Box::new(b) as Box<dyn AsyncReadWrite>,
            info,
            vec![ctx],
            &AssociationOptions::default(),
        );
        *rsp_statuses(&mut peer, field, id).await.last().unwrap()
    }

    #[tokio::test]
    async fn move_empty_plan_preserves_handler_warning() {
        // Empty plan, warning handler status: the final must be the
        // warning, not SUCCESS (no outbound dial happens).
        let sop = "1.2.840.10008.5.1.4.1.2.2.2";
        let (mut assoc, b, ctx) = duplex_scp(sop);
        let mut map = HashMap::new();
        map.insert("DEST".to_string(), ("127.0.0.1".to_string(), 11112));
        let scp_opts = ScpOptions {
            move_config: Some(ScpMoveConfig {
                directory: Arc::new(HashMapDirectory { map }),
                assoc_options: AssociationOptions::default(),
                calling_ae: AeTitle::new("SCP").unwrap(),
            }),
            ..Default::default()
        };
        let msg = DimseMessage {
            context_id: ctx.id,
            command: Command::move_rq(11, priority::MEDIUM, sop, "DEST").unwrap(),
            dataset: Some(DataSet::new()),
        };
        dispatch(&mut assoc, &WarnPlan, &scp_opts, msg)
            .await
            .unwrap();
        drop(assoc);
        assert_eq!(
            final_move_get_status(b, ctx, CF::C_MOVE_RSP, 11).await,
            status::ELEMENTS_DISCARDED
        );
    }

    #[tokio::test]
    async fn get_empty_plan_preserves_handler_warning() {
        let sop = "1.2.840.10008.5.1.4.1.2.2.3";
        let (mut assoc, b, ctx) = duplex_scp(sop);
        let msg = DimseMessage {
            context_id: ctx.id,
            command: Command::get_rq(12, priority::MEDIUM, sop).unwrap(),
            dataset: Some(DataSet::new()),
        };
        dispatch(&mut assoc, &WarnPlan, &ScpOptions::default(), msg)
            .await
            .unwrap();
        drop(assoc);
        assert_eq!(
            final_move_get_status(b, ctx, CF::C_GET_RSP, 12).await,
            status::ELEMENTS_DISCARDED
        );
    }

    #[tokio::test]
    async fn get_final_preserves_handler_warning_with_sub_ops() {
        // One successful sub-operation plus a handler warning: the
        // final must still be the warning, not SUCCESS.
        struct WarnGet;
        impl Handlers for WarnGet {
            async fn on_get(
                &self,
                _ctx: &RequestContext<'_>,
                _query: &DataSet,
            ) -> (u16, Vec<MoveInstance>) {
                let mut ds = DataSet::new();
                ds.put_text(
                    vyna_dcm_core::tags::PATIENT_NAME,
                    vyna_dcm_core::Vr::Pn,
                    "Doe^Jane",
                );
                (
                    status::ELEMENTS_DISCARDED,
                    vec![MoveInstance {
                        sop_class_uid: vyna_dcm_core::sop_class::CT_IMAGE.to_string(),
                        sop_instance_uid: "1.2.9.1".to_string(),
                        dataset: ds,
                    }],
                )
            }
        }
        let sop = vyna_dcm_core::sop_class::CT_IMAGE;
        let (mut assoc, b, ctx) = duplex_scp(sop);
        let peer_ctx = ctx.clone();
        let peer_task = tokio::spawn(async move {
            let info = AssociationInfo {
                local_ae: AeTitle::new("SCU").unwrap(),
                peer_ae: AeTitle::new("SCP").unwrap(),
                peer_max_pdu: 0,
            };
            let mut peer = Association::from_accepted(
                Box::new(b) as Box<dyn AsyncReadWrite>,
                info,
                vec![peer_ctx],
                &AssociationOptions::default(),
            );
            // Answer the single C-STORE-RQ, then collect C-GET-RSPs.
            let DimseEvent::Message(m) = peer.recv().await.unwrap() else {
                panic!("released");
            };
            assert_eq!(m.command.field, CF::C_STORE_RQ);
            let store_id = m.command.message_id.unwrap();
            let rsp = Command::store_rsp(
                store_id,
                m.command.affected_sop_class_uid.clone(),
                m.command.affected_sop_instance_uid.clone(),
                status::SUCCESS,
            );
            peer.send(m.context_id, &rsp, None).await.unwrap();
            rsp_statuses(&mut peer, CF::C_GET_RSP, 13).await
        });
        let msg = DimseMessage {
            context_id: ctx.id,
            command: Command::get_rq(13, priority::MEDIUM, sop).unwrap(),
            dataset: Some(DataSet::new()),
        };
        dispatch(&mut assoc, &WarnGet, &ScpOptions::default(), msg)
            .await
            .unwrap();
        let statuses = peer_task.await.unwrap();
        assert_eq!(statuses, vec![status::PENDING, status::ELEMENTS_DISCARDED]);
    }

    #[tokio::test]
    async fn accept_uses_short_associate_timer() {
        // A connected-but-silent peer must hit the 30 s-style ARTIM
        // timer, not the 300 s inactivity budget.
        struct EchoOnly;
        impl Handlers for EchoOnly {}
        let (peer, b) = tokio::io::duplex(65536);
        let _peer = peer; // held open and silent (a drop would EOF instead)
        let acc = AcceptorOptions {
            associate_timeout: Duration::from_millis(100),
            ..Default::default()
        };
        let t0 = std::time::Instant::now();
        let err = accept(b, &acc, &EchoOnly, &ScpOptions::default())
            .await
            .unwrap_err();
        assert!(
            matches!(err, Error::Timeout("association request")),
            "{err:?}"
        );
        assert!(t0.elapsed() < Duration::from_secs(30));
    }

    #[tokio::test]
    async fn accept_limited_enforces_concurrency_cap() {
        struct EchoOnly;
        impl Handlers for EchoOnly {}
        // No permits: refused without touching the stream.
        let slots = tokio::sync::Semaphore::new(0);
        let (_a, b) = tokio::io::duplex(65536);
        let err = accept_limited(
            b,
            &AcceptorOptions::default(),
            &EchoOnly,
            &ScpOptions::default(),
            &slots,
        )
        .await
        .unwrap_err();
        assert!(matches!(err, Error::TooManyAssociations), "{err:?}");
        // One permit: a full associate-then-release loopback succeeds.
        let slots = tokio::sync::Semaphore::new(1);
        let (a, b) = tokio::io::duplex(65536);
        let peer_task = tokio::spawn(async move {
            use crate::pdu::{Pdu, read_pdu, write_pdu};
            let mut peer = a;
            write_pdu(&mut peer, &Pdu::AssociateRq(sample_rq()))
                .await
                .unwrap();
            let ac = read_pdu(&mut peer, 64 * 1024 * 1024).await.unwrap();
            assert!(matches!(ac, Pdu::AssociateAc(_)), "{ac:?}");
            write_pdu(&mut peer, &Pdu::ReleaseRq).await.unwrap();
            let rsp = read_pdu(&mut peer, 64 * 1024 * 1024).await.unwrap();
            assert!(matches!(rsp, Pdu::ReleaseRsp), "{rsp:?}");
        });
        accept_limited(
            b,
            &AcceptorOptions::default(),
            &EchoOnly,
            &ScpOptions::default(),
            &slots,
        )
        .await
        .unwrap();
        peer_task.await.unwrap();
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
