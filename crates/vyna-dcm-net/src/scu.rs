//! SCU operations as [`Association`] methods: echo, store, find, move,
//! get and the N-services (MPPS, Storage Commitment, ...).
//!
//! Every method returns the DIMSE status (transport/protocol problems are
//! `Err`); use [`status::is_success`] et al. to classify statuses.

use vyna_dcm_core::{DataSet, Tag, TransferSyntax};

use crate::{
    Error, Result,
    ae::AeTitle,
    association::{Association, DimseEvent},
    dimse::{Command, DimseMessage, command_field as CF, status},
    worklist::{CommitmentResult, parse_commitment_result},
};

/// C-MOVE / C-GET final outcome with sub-operation counts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MoveResult {
    /// Final DIMSE status.
    pub status: u16,
    /// Sub-operations remaining (final: 0).
    pub remaining: u16,
    /// Completed sub-operations.
    pub completed: u16,
    /// Failed sub-operations.
    pub failed: u16,
    /// Warning sub-operations.
    pub warning: u16,
}

impl MoveResult {
    fn from_command(cmd: &Command) -> Self {
        Self {
            status: cmd.status.unwrap_or(status::PROCESSING_FAILURE),
            remaining: cmd.remaining.unwrap_or(0),
            completed: cmd.completed.unwrap_or(0),
            failed: cmd.failed.unwrap_or(0),
            warning: cmd.warning.unwrap_or(0),
        }
    }
}

/// Maximum pending responses [`Association::find_all`] collects: past
/// this the SCU drains to the final response and fails with
/// [`Error::TooManyMatches`](crate::Error::TooManyMatches) instead of
/// letting a rogue peer OOM it. Mirrors the SCP-side 10,000-match cap.
pub const MAX_FIND_MATCHES: usize = 10_000;

/// Maximum aggregate encoded bytes [`Association::find_all`] collects
/// (past it: [`Error::MatchesTooLarge`](crate::Error::MatchesTooLarge)).
/// C-FIND identifiers are small key sets, so 256 MiB is generous headroom.
pub const MAX_FIND_BYTES: u64 = 256 * 1024 * 1024;

impl Association {
    /// C-ECHO against the Verification (or any echoable) SOP class.
    pub async fn echo(&mut self, sop_class_uid: &str) -> Result<u16> {
        let ctx = self
            .context_for(sop_class_uid)
            .ok_or_else(|| Error::NoPresentationContext(sop_class_uid.into()))?
            .clone();
        let id = self.next_message_id();
        self.send(ctx.id, &Command::echo_rq(id), None).await?;
        let msg = self.expect_message(CF::C_ECHO_RSP, id).await?;
        Ok(msg.command.status.unwrap_or(status::PROCESSING_FAILURE))
    }

    /// C-STORE a dataset.
    pub async fn store(
        &mut self,
        dataset: &DataSet,
        sop_class_uid: &str,
        sop_instance_uid: &str,
        priority: u16,
    ) -> Result<u16> {
        let ctx = self
            .context_for(sop_class_uid)
            .ok_or_else(|| Error::NoPresentationContext(sop_class_uid.into()))?
            .clone();
        let id = self.next_message_id();
        let cmd = Command::store_rq(id, priority, sop_class_uid, sop_instance_uid);
        self.send(ctx.id, &cmd, Some(dataset)).await?;
        let msg = self.expect_message(CF::C_STORE_RSP, id).await?;
        Ok(msg.command.status.unwrap_or(status::PROCESSING_FAILURE))
    }

    /// C-FIND with a per-match callback; returns the final status.
    pub async fn find_with(
        &mut self,
        sop_class_uid: &str,
        query: &DataSet,
        priority: u16,
        mut on_match: impl FnMut(DataSet),
    ) -> Result<u16> {
        let ctx = self
            .context_for(sop_class_uid)
            .ok_or_else(|| Error::NoPresentationContext(sop_class_uid.into()))?
            .clone();
        let id = self.next_message_id();
        self.send(
            ctx.id,
            &Command::find_rq(id, priority, sop_class_uid),
            Some(query),
        )
        .await?;
        loop {
            let msg = self.expect_message(CF::C_FIND_RSP, id).await?;
            let s = msg.command.status.unwrap_or(status::PROCESSING_FAILURE);
            if status::is_pending(s) {
                if let Some(ds) = msg.dataset {
                    on_match(ds);
                }
            } else {
                return Ok(s);
            }
        }
    }

    /// C-FIND collecting all matches (capped: [`MAX_FIND_MATCHES`]
    /// responses / [`MAX_FIND_BYTES`] bytes; past either cap the SCU
    /// drains to the final response and errors, keeping the
    /// association usable). For unbounded streaming use [`Association::find_with`].
    pub async fn find_all(
        &mut self,
        sop_class_uid: &str,
        query: &DataSet,
        priority: u16,
    ) -> Result<(u16, Vec<DataSet>)> {
        let ctx = self
            .context_for(sop_class_uid)
            .ok_or_else(|| Error::NoPresentationContext(sop_class_uid.into()))?
            .clone();
        let id = self.next_message_id();
        self.send(
            ctx.id,
            &Command::find_rq(id, priority, sop_class_uid),
            Some(query),
        )
        .await?;
        let mut matches = Vec::new();
        let mut bytes: u64 = 0;
        let mut overflow: Option<Error> = None;
        loop {
            let msg = self.expect_message(CF::C_FIND_RSP, id).await?;
            let s = msg.command.status.unwrap_or(status::PROCESSING_FAILURE);
            if status::is_pending(s) {
                // Past the cap: drop matches but keep consuming so the
                // association stays in sync through the final response.
                if let Some(ds) = msg.dataset.filter(|_| overflow.is_none()) {
                    if matches.len() >= MAX_FIND_MATCHES {
                        overflow = Some(Error::TooManyMatches {
                            limit: MAX_FIND_MATCHES,
                        });
                    } else {
                        let n = vyna_dcm_codec::encode_dataset(
                            &ds,
                            TransferSyntax::ImplicitVrLittleEndian,
                        )?
                        .len() as u64;
                        if bytes.saturating_add(n) > MAX_FIND_BYTES {
                            overflow = Some(Error::MatchesTooLarge {
                                limit: MAX_FIND_BYTES,
                            });
                        } else {
                            bytes += n;
                            matches.push(ds);
                        }
                    }
                }
            } else if let Some(e) = overflow.take() {
                return Err(e);
            } else {
                return Ok((s, matches));
            }
        }
    }

    /// C-FIND with cancellation support: the callback returns `true` to
    /// keep receiving, `false` to C-CANCEL and stop.
    ///
    /// After CANCEL, in-flight pending responses are discarded until the
    /// final response; the final status is `CANCEL` on cancel-aware peers
    /// or `SUCCESS` when the peer ran to completion first.
    pub async fn find_cancelable(
        &mut self,
        sop_class_uid: &str,
        query: &DataSet,
        priority: u16,
        mut on_match: impl FnMut(DataSet) -> bool,
    ) -> Result<u16> {
        let ctx = self
            .context_for(sop_class_uid)
            .ok_or_else(|| Error::NoPresentationContext(sop_class_uid.into()))?
            .clone();
        let id = self.next_message_id();
        self.send(
            ctx.id,
            &Command::find_rq(id, priority, sop_class_uid),
            Some(query),
        )
        .await?;
        loop {
            let msg = self.expect_message(CF::C_FIND_RSP, id).await?;
            let s = msg.command.status.unwrap_or(status::PROCESSING_FAILURE);
            if status::is_pending(s) {
                let keep = match msg.dataset {
                    Some(ds) => on_match(ds),
                    None => true,
                };
                if !keep {
                    self.send(ctx.id, &Command::cancel_rq(id), None).await?;
                    // Drain in-flight pendings until the final response.
                    loop {
                        let fin = self.expect_message(CF::C_FIND_RSP, id).await?;
                        let fs = fin.command.status.unwrap_or(status::CANCEL);
                        if !status::is_pending(fs) {
                            return Ok(fs);
                        }
                    }
                }
            } else {
                return Ok(s);
            }
        }
    }

    /// C-MOVE: ask the SCP to C-STORE matches to `destination_ae`.
    pub async fn move_to(
        &mut self,
        sop_class_uid: &str,
        query: &DataSet,
        destination_ae: &str,
        priority: u16,
    ) -> Result<MoveResult> {
        // Reject empty/overlong destinations before touching the wire:
        // a blank (0000,0600) would fail opaquely at the peer.
        let dest = AeTitle::new(destination_ae)?;
        let ctx = self
            .context_for(sop_class_uid)
            .ok_or_else(|| Error::NoPresentationContext(sop_class_uid.into()))?
            .clone();
        let id = self.next_message_id();
        let cmd = Command::move_rq(id, priority, sop_class_uid, dest.as_str());
        self.send(ctx.id, &cmd, Some(query)).await?;
        self.await_move_get(CF::C_MOVE_RSP, id).await
    }

    /// C-GET: receive C-STORE sub-operations over this same association.
    /// `on_store` gets `(sop_class_uid, sop_instance_uid, dataset,
    /// transfer_syntax)` and returns the C-STORE-RSP status for each
    /// instance. Encapsulated datasets arrive as-is (fragments); the
    /// callback owns any conversion.
    pub async fn get_with(
        &mut self,
        sop_class_uid: &str,
        query: &DataSet,
        priority: u16,
        mut on_store: impl FnMut(String, String, DataSet, TransferSyntax) -> u16,
    ) -> Result<MoveResult> {
        let ctx = self
            .context_for(sop_class_uid)
            .ok_or_else(|| Error::NoPresentationContext(sop_class_uid.into()))?
            .clone();
        let id = self.next_message_id();
        self.send(
            ctx.id,
            &Command::get_rq(id, priority, sop_class_uid),
            Some(query),
        )
        .await?;
        loop {
            match self.recv().await? {
                DimseEvent::Released => return Err(Error::Released),
                DimseEvent::Message(m) if m.command.field == CF::C_STORE_RQ => {
                    let store_id = m.command.message_id.unwrap_or(0);
                    let class = m.command.affected_sop_class_uid.clone().unwrap_or_default();
                    let inst = m
                        .command
                        .affected_sop_instance_uid
                        .clone()
                        .unwrap_or_default();
                    let s = match (m.dataset, self.context_by_id(m.context_id)) {
                        (Some(ds), Some(st)) => {
                            on_store(class.clone(), inst.clone(), ds, st.transfer_syntax)
                        }
                        _ => status::PROCESSING_FAILURE,
                    };
                    let rsp = Command::store_rsp(store_id, Some(class), Some(inst), s);
                    self.send(m.context_id, &rsp, None).await?;
                }
                DimseEvent::Message(m) if m.command.field == CF::C_GET_RSP => {
                    check_id(&m, id)?;
                    let s = m.command.status.unwrap_or(status::PROCESSING_FAILURE);
                    if !status::is_pending(s) {
                        return Ok(MoveResult::from_command(&m.command));
                    }
                }
                DimseEvent::Message(m) => {
                    return Err(Error::UnexpectedPdu {
                        expected: "C-STORE-RQ or C-GET-RSP",
                        got: m.command.name().to_string(),
                    });
                }
            }
        }
    }

    /// N-CREATE (e.g. MPPS SOP instance creation).
    pub async fn n_create(
        &mut self,
        sop_class_uid: &str,
        sop_instance_uid: &str,
        dataset: Option<&DataSet>,
    ) -> Result<(u16, Option<DataSet>)> {
        let ctx = self
            .context_for(sop_class_uid)
            .ok_or_else(|| Error::NoPresentationContext(sop_class_uid.into()))?
            .clone();
        let id = self.next_message_id();
        let cmd = Command::n_create_rq(id, sop_class_uid, sop_instance_uid, dataset.is_some());
        self.send(ctx.id, &cmd, dataset).await?;
        let msg = self.expect_message(CF::N_CREATE_RSP, id).await?;
        Ok((
            msg.command.status.unwrap_or(status::PROCESSING_FAILURE),
            msg.dataset,
        ))
    }

    /// N-SET (e.g. MPPS update).
    pub async fn n_set(
        &mut self,
        sop_class_uid: &str,
        sop_instance_uid: &str,
        dataset: &DataSet,
    ) -> Result<(u16, Option<DataSet>)> {
        let ctx = self
            .context_for(sop_class_uid)
            .ok_or_else(|| Error::NoPresentationContext(sop_class_uid.into()))?
            .clone();
        let id = self.next_message_id();
        let cmd = Command::n_set_rq(id, sop_class_uid, sop_instance_uid);
        self.send(ctx.id, &cmd, Some(dataset)).await?;
        let msg = self.expect_message(CF::N_SET_RSP, id).await?;
        Ok((
            msg.command.status.unwrap_or(status::PROCESSING_FAILURE),
            msg.dataset,
        ))
    }

    /// N-GET with attribute identifiers.
    pub async fn n_get(
        &mut self,
        sop_class_uid: &str,
        sop_instance_uid: &str,
        attributes: Vec<Tag>,
    ) -> Result<(u16, Option<DataSet>)> {
        let ctx = self
            .context_for(sop_class_uid)
            .ok_or_else(|| Error::NoPresentationContext(sop_class_uid.into()))?
            .clone();
        let id = self.next_message_id();
        let cmd = Command::n_get_rq(id, sop_class_uid, sop_instance_uid, attributes);
        self.send(ctx.id, &cmd, None).await?;
        let msg = self.expect_message(CF::N_GET_RSP, id).await?;
        Ok((
            msg.command.status.unwrap_or(status::PROCESSING_FAILURE),
            msg.dataset,
        ))
    }

    /// N-ACTION (e.g. Storage Commitment request, action type 1).
    pub async fn n_action(
        &mut self,
        sop_class_uid: &str,
        sop_instance_uid: &str,
        action_type_id: u16,
        dataset: Option<&DataSet>,
    ) -> Result<(u16, Option<DataSet>)> {
        let ctx = self
            .context_for(sop_class_uid)
            .ok_or_else(|| Error::NoPresentationContext(sop_class_uid.into()))?
            .clone();
        let id = self.next_message_id();
        let cmd = Command::n_action_rq(
            id,
            sop_class_uid,
            sop_instance_uid,
            action_type_id,
            dataset.is_some(),
        );
        self.send(ctx.id, &cmd, dataset).await?;
        let msg = self.expect_message(CF::N_ACTION_RSP, id).await?;
        Ok((
            msg.command.status.unwrap_or(status::PROCESSING_FAILURE),
            msg.dataset,
        ))
    }

    /// N-DELETE.
    pub async fn n_delete(&mut self, sop_class_uid: &str, sop_instance_uid: &str) -> Result<u16> {
        let ctx = self
            .context_for(sop_class_uid)
            .ok_or_else(|| Error::NoPresentationContext(sop_class_uid.into()))?
            .clone();
        let id = self.next_message_id();
        self.send(
            ctx.id,
            &Command::n_delete_rq(id, sop_class_uid, sop_instance_uid),
            None,
        )
        .await?;
        let msg = self.expect_message(CF::N_DELETE_RSP, id).await?;
        Ok(msg.command.status.unwrap_or(status::PROCESSING_FAILURE))
    }

    /// N-EVENT-REPORT (e.g. Storage Commitment result on the reverse
    /// association, or MPPS-linked event reports).
    pub async fn n_event_report(
        &mut self,
        sop_class_uid: &str,
        sop_instance_uid: &str,
        event_type_id: u16,
        dataset: Option<&DataSet>,
    ) -> Result<u16> {
        let ctx = self
            .context_for(sop_class_uid)
            .ok_or_else(|| Error::NoPresentationContext(sop_class_uid.into()))?
            .clone();
        let id = self.next_message_id();
        let cmd = Command::n_event_report_rq(
            id,
            sop_class_uid,
            sop_instance_uid,
            event_type_id,
            dataset.is_some(),
        );
        self.send(ctx.id, &cmd, dataset).await?;
        let msg = self.expect_message(CF::N_EVENT_REPORT_RSP, id).await?;
        Ok(msg.command.status.unwrap_or(status::PROCESSING_FAILURE))
    }

    /// Storage Commitment: N-ACTION request (action type 1), then wait for
    /// the N-EVENT-REPORT on this same association.
    ///
    /// The report is acknowledged with SUCCESS automatically and parsed.
    /// Returns the N-ACTION status plus the report, or `None` when the
    /// N-ACTION itself failed (no report follows a failed request). The
    /// association read timeout bounds the wait.
    pub async fn storage_commit(
        &mut self,
        sop_class_uid: &str,
        request_instance_uid: &str,
        dataset: &DataSet,
    ) -> Result<(u16, Option<CommitmentResult>)> {
        let (action_status, _) = self
            .n_action(sop_class_uid, request_instance_uid, 1, Some(dataset))
            .await?;
        if !status::is_success(action_status) {
            return Ok((action_status, None));
        }
        match self.recv().await? {
            DimseEvent::Released => Err(Error::Released),
            DimseEvent::Message(m) if m.command.field == CF::N_EVENT_REPORT_RQ => {
                let report_id = m.command.message_id.unwrap_or(0);
                if m.command.event_type_id != Some(1) {
                    // Nack the report before failing: the peer awaits
                    // this RSP and would otherwise hang to timeout.
                    let rsp = Command::n_event_report_rsp(
                        report_id,
                        m.command.affected_sop_class_uid.clone(),
                        m.command.affected_sop_instance_uid.clone(),
                        status::PROCESSING_FAILURE,
                    );
                    self.send(m.context_id, &rsp, None).await?;
                    return Err(Error::BadCommand(format!(
                        "unexpected storage commitment event type {:?}, want 1",
                        m.command.event_type_id
                    )));
                }
                let result = m.dataset.as_ref().map(parse_commitment_result).unwrap_or((
                    None,
                    Vec::new(),
                    Vec::new(),
                ));
                let rsp = Command::n_event_report_rsp(
                    report_id,
                    m.command.affected_sop_class_uid.clone(),
                    m.command.affected_sop_instance_uid.clone(),
                    status::SUCCESS,
                );
                self.send(m.context_id, &rsp, None).await?;
                Ok((action_status, Some(result)))
            }
            DimseEvent::Message(m) => Err(Error::UnexpectedPdu {
                expected: "N-EVENT-REPORT-RQ",
                got: m.command.name().to_string(),
            }),
        }
    }

    // -- internals ---------------------------------------------------------

    async fn expect_message(&mut self, field: u16, id: u16) -> Result<DimseMessage> {
        match self.recv().await? {
            DimseEvent::Released => Err(Error::Released),
            DimseEvent::Message(m) if m.command.field == field => {
                check_id(&m, id)?;
                Ok(*m)
            }
            DimseEvent::Message(m) => Err(Error::UnexpectedPdu {
                expected: match field {
                    CF::C_ECHO_RSP => "C-ECHO-RSP",
                    CF::C_STORE_RSP => "C-STORE-RSP",
                    CF::C_FIND_RSP => "C-FIND-RSP",
                    CF::C_MOVE_RSP => "C-MOVE-RSP",
                    CF::C_GET_RSP => "C-GET-RSP",
                    _ => "N-*-RSP",
                },
                got: m.command.name().to_string(),
            }),
        }
    }

    async fn await_move_get(&mut self, field: u16, id: u16) -> Result<MoveResult> {
        loop {
            let msg = self.expect_message(field, id).await?;
            let s = msg.command.status.unwrap_or(status::PROCESSING_FAILURE);
            if !status::is_pending(s) {
                return Ok(MoveResult::from_command(&msg.command));
            }
        }
    }
}

fn check_id(msg: &DimseMessage, id: u16) -> Result<()> {
    if msg.command.message_id_rsp != Some(id) {
        return Err(Error::BadCommand(format!(
            "message id mismatch for {}: want {id}, got {:?}",
            msg.command.name(),
            msg.command.message_id_rsp
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AeTitle;
    use crate::dimse::priority;
    use crate::service::{
        AcceptorOptions, Handlers, MoveInstance, RequestContext, ScpOptions, accept, propose,
    };
    use std::sync::Mutex;
    use tokio::net::TcpListener;
    use vyna_dcm_core::{Vr, tags};

    struct TestScp {
        stored: Mutex<Vec<DataSet>>,
        find_matches: Vec<DataSet>,
        move_instances: Vec<MoveInstance>,
        fail_store: bool,
    }

    impl Handlers for TestScp {
        async fn on_store(&self, _ctx: &RequestContext<'_>, dataset: DataSet) -> u16 {
            self.stored.lock().unwrap().push(dataset);
            if self.fail_store {
                status::PROCESSING_FAILURE
            } else {
                status::SUCCESS
            }
        }

        async fn on_find(
            &self,
            _ctx: &RequestContext<'_>,
            _query: &DataSet,
        ) -> (u16, Vec<DataSet>) {
            (status::SUCCESS, self.find_matches.clone())
        }

        async fn on_move(
            &self,
            _ctx: &RequestContext<'_>,
            _query: &DataSet,
            _dest: &str,
        ) -> (u16, Vec<MoveInstance>) {
            (status::SUCCESS, self.move_instances.clone())
        }

        async fn on_get(
            &self,
            _ctx: &RequestContext<'_>,
            _query: &DataSet,
        ) -> (u16, Vec<MoveInstance>) {
            (status::SUCCESS, self.move_instances.clone())
        }
    }

    fn test_ds(id: &str) -> DataSet {
        let mut ds = DataSet::new();
        ds.put_uid(tags::SOP_CLASS_UID, vyna_dcm_core::sop_class::CT_IMAGE);
        ds.put_uid(tags::SOP_INSTANCE_UID, id);
        ds.put_text(tags::PATIENT_NAME, Vr::Pn, "Loop^Back");
        ds.put_text(tags::MODALITY, Vr::Cs, "CT");
        ds
    }

    fn short_opts() -> crate::association::AssociationOptions {
        crate::association::AssociationOptions {
            read_timeout: std::time::Duration::from_secs(10),
            max_pdu_len: 16384,
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn loopback_echo_store_find() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let handlers = ArcTestScp::new(TestScp {
            stored: Mutex::new(vec![]),
            find_matches: vec![test_ds("1.2.3.1"), test_ds("1.2.3.2")],
            move_instances: vec![],
            fail_store: false,
        });
        let h2 = handlers.clone();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            stream.set_nodelay(true).unwrap();
            let acc = AcceptorOptions {
                association: short_opts(),
                ..Default::default()
            };
            accept(stream, &acc, &*h2, &ScpOptions::default())
                .await
                .unwrap();
        });

        let mut assoc = Association::request(
            &short_opts(),
            &AeTitle::new("SCU").unwrap(),
            &AeTitle::new("SCP").unwrap(),
            &addr.to_string(),
            propose(
                &[
                    vyna_dcm_core::sop_class::VERIFICATION,
                    vyna_dcm_core::sop_class::CT_IMAGE,
                    vyna_dcm_core::sop_class::STUDY_ROOT_FIND,
                ],
                &[
                    vyna_dcm_core::TransferSyntax::ExplicitVrLittleEndian,
                    vyna_dcm_core::TransferSyntax::ImplicitVrLittleEndian,
                ],
            )
            .unwrap(),
        )
        .await
        .unwrap();

        assert_eq!(
            assoc
                .echo(vyna_dcm_core::sop_class::VERIFICATION)
                .await
                .unwrap(),
            status::SUCCESS
        );
        assert_eq!(
            assoc
                .store(
                    &test_ds("1.2.3.9"),
                    vyna_dcm_core::sop_class::CT_IMAGE,
                    "1.2.3.9",
                    priority::MEDIUM
                )
                .await
                .unwrap(),
            status::SUCCESS
        );
        let (s, matches) = assoc
            .find_all(
                vyna_dcm_core::sop_class::STUDY_ROOT_FIND,
                &DataSet::new(),
                priority::MEDIUM,
            )
            .await
            .unwrap();
        assert_eq!(s, status::SUCCESS);
        assert_eq!(matches.len(), 2);
        // Cancelable FIND stopping after the first match: the SCP
        // honors C-CANCEL mid-stream, so the final status is CANCEL.
        let mut seen = 0;
        let s = assoc
            .find_cancelable(
                vyna_dcm_core::sop_class::STUDY_ROOT_FIND,
                &DataSet::new(),
                priority::MEDIUM,
                |_| {
                    seen += 1;
                    false
                },
            )
            .await
            .unwrap();
        assert_eq!((seen, s), (1, status::CANCEL));
        assoc.release().await.unwrap();
        assert_eq!(handlers.stored.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn loopback_find_cancel_truncates() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let matches: Vec<DataSet> = (0..30).map(|i| test_ds(&format!("1.2.3.{i}"))).collect();
        let handlers = ArcTestScp::new(TestScp {
            stored: Mutex::new(vec![]),
            find_matches: matches,
            move_instances: vec![],
            fail_store: false,
        });
        let h2 = handlers.clone();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            stream.set_nodelay(true).unwrap();
            let acc = AcceptorOptions {
                association: short_opts(),
                ..Default::default()
            };
            accept(stream, &acc, &*h2, &ScpOptions::default())
                .await
                .unwrap();
        });

        let mut assoc = Association::request(
            &short_opts(),
            &AeTitle::new("SCU").unwrap(),
            &AeTitle::new("SCP").unwrap(),
            &addr.to_string(),
            propose(
                &[
                    vyna_dcm_core::sop_class::VERIFICATION,
                    vyna_dcm_core::sop_class::STUDY_ROOT_FIND,
                ],
                &[vyna_dcm_core::TransferSyntax::ExplicitVrLittleEndian],
            )
            .unwrap(),
        )
        .await
        .unwrap();

        // Cancel after the 5th match: the drain discards in-flight
        // pendings without counting, so `seen` is exactly 5 and the
        // server stops early with CANCEL instead of running to 30.
        let mut seen = 0;
        let s = assoc
            .find_cancelable(
                vyna_dcm_core::sop_class::STUDY_ROOT_FIND,
                &DataSet::new(),
                priority::MEDIUM,
                |_| {
                    seen += 1;
                    seen < 5
                },
            )
            .await
            .unwrap();
        assert_eq!((seen, s), (5, status::CANCEL));
        // The association stays in sync after a cancelled FIND.
        assert_eq!(
            assoc
                .echo(vyna_dcm_core::sop_class::VERIFICATION)
                .await
                .unwrap(),
            status::SUCCESS
        );
        assoc.release().await.unwrap();
    }

    #[tokio::test]
    async fn loopback_find_all_caps_rogue_peer() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        // One more pending than the SCU collects.
        let matches: Vec<DataSet> = (0..=MAX_FIND_MATCHES)
            .map(|i| test_ds(&format!("1.2.3.{i}")))
            .collect();
        let handlers = ArcTestScp::new(TestScp {
            stored: Mutex::new(vec![]),
            find_matches: matches,
            move_instances: vec![],
            fail_store: false,
        });
        let h2 = handlers.clone();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            stream.set_nodelay(true).unwrap();
            let acc = AcceptorOptions {
                association: short_opts(),
                ..Default::default()
            };
            accept(stream, &acc, &*h2, &ScpOptions::default())
                .await
                .unwrap();
        });

        let mut assoc = Association::request(
            &short_opts(),
            &AeTitle::new("SCU").unwrap(),
            &AeTitle::new("SCP").unwrap(),
            &addr.to_string(),
            propose(
                &[
                    vyna_dcm_core::sop_class::VERIFICATION,
                    vyna_dcm_core::sop_class::STUDY_ROOT_FIND,
                ],
                &[vyna_dcm_core::TransferSyntax::ExplicitVrLittleEndian],
            )
            .unwrap(),
        )
        .await
        .unwrap();

        let err = assoc
            .find_all(
                vyna_dcm_core::sop_class::STUDY_ROOT_FIND,
                &DataSet::new(),
                priority::MEDIUM,
            )
            .await
            .unwrap_err();
        assert!(
            matches!(err, Error::TooManyMatches { .. }),
            "expected TooManyMatches, got {err:?}"
        );
        // Drained to the final response: the association stays usable.
        assert_eq!(
            assoc
                .echo(vyna_dcm_core::sop_class::VERIFICATION)
                .await
                .unwrap(),
            status::SUCCESS
        );
        assoc.release().await.unwrap();
    }

    #[tokio::test]
    async fn loopback_mpps_create_set() {
        use crate::worklist::{MppsData, MppsScp};
        let dir = std::env::temp_dir().join(format!("mpps-loopback-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let handlers = Arc::new(MppsScp::new(dir.clone()));
        let h2 = handlers.clone();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            stream.set_nodelay(true).unwrap();
            let acc = AcceptorOptions {
                association: short_opts(),
                ..Default::default()
            };
            accept(stream, &acc, &*h2, &ScpOptions::default())
                .await
                .unwrap();
        });

        let sop = vyna_dcm_core::sop_class::MODALITY_PERFORMED_PROCEDURE_STEP;
        let mut assoc = Association::request(
            &short_opts(),
            &AeTitle::new("SCU").unwrap(),
            &AeTitle::new("SCP").unwrap(),
            &addr.to_string(),
            propose(
                &[sop],
                &[vyna_dcm_core::TransferSyntax::ExplicitVrLittleEndian],
            )
            .unwrap(),
        )
        .await
        .unwrap();

        let create = MppsData {
            performed_procedure_step_id: "PPS1".into(),
            performed_station_ae: "SCU".into(),
            modality: "ECG".into(),
            start_date: "20261012".into(),
            start_time: "120000".into(),
            status: "IN PROGRESS".into(),
            ..MppsData::default()
        }
        .to_create_dataset();
        let (cs, _) = assoc.n_create(sop, "1.2.3.1", Some(&create)).await.unwrap();
        assert_eq!(cs, status::SUCCESS);
        // Duplicate N-CREATE fails; the step survives.
        let (dup, _) = assoc.n_create(sop, "1.2.3.1", Some(&create)).await.unwrap();
        assert_eq!(dup, status::DUPLICATE_SOP_INSTANCE);
        let set = MppsData {
            status: "COMPLETED".into(),
            end_date: Some("20261012".into()),
            ..MppsData::default()
        }
        .to_set_dataset();
        let (ss, _) = assoc.n_set(sop, "1.2.3.1", &set).await.unwrap();
        assert_eq!(ss, status::SUCCESS);
        // Unknown step and post-final updates fail.
        let (ns, _) = assoc.n_set(sop, "9.9.9", &set).await.unwrap();
        assert_eq!(ns, status::NO_SUCH_SOP_INSTANCE);
        let (fin, _) = assoc.n_set(sop, "1.2.3.1", &set).await.unwrap();
        assert_eq!(fin, status::PROCESSING_FAILURE);
        assoc.release().await.unwrap();

        let stored = handlers.get("1.2.3.1").unwrap();
        assert_eq!(
            stored.string(tags::PERFORMED_PROCEDURE_STEP_STATUS),
            Some("COMPLETED")
        );
        assert_eq!(
            stored.string(tags::PERFORMED_PROCEDURE_STEP_ID),
            Some("PPS1")
        );
        assert!(dir.join("1.2.3.1.dcm").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn loopback_storage_commit_push() {
        use crate::qrdb::StgCommitScp;
        // Two files on disk; only one gets requested + one ghost.
        let dir = std::env::temp_dir().join(format!("stgcommit-loopback-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let ct = vyna_dcm_core::sop_class::CT_IMAGE;
        for inst in ["1.2.3.1", "1.2.3.2"] {
            let meta = vyna_dcm_codec::FileMeta::new(
                ct,
                inst,
                vyna_dcm_core::TransferSyntax::ExplicitVrLittleEndian,
            );
            let bytes = vyna_dcm_codec::write_part10(&meta, &test_ds(inst)).unwrap();
            std::fs::write(dir.join(format!("{inst}.dcm")), bytes).unwrap();
        }
        let index = crate::qrdb::QrIndex::scan(&dir).unwrap();
        assert_eq!(index.records().len(), 2);

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let handlers = Arc::new(StgCommitScp::new(index));
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            stream.set_nodelay(true).unwrap();
            let acc = AcceptorOptions {
                association: short_opts(),
                ..Default::default()
            };
            accept(stream, &acc, &*handlers, &ScpOptions::default())
                .await
                .unwrap();
        });

        let sop = vyna_dcm_core::sop_class::STORAGE_COMMITMENT_PUSH;
        let mut assoc = Association::request(
            &short_opts(),
            &AeTitle::new("SCU").unwrap(),
            &AeTitle::new("SCP").unwrap(),
            &addr.to_string(),
            propose(
                &[sop],
                &[vyna_dcm_core::TransferSyntax::ExplicitVrLittleEndian],
            )
            .unwrap(),
        )
        .await
        .unwrap();

        let (req, txn) =
            crate::worklist::storage_commitment_request(&[(ct, "1.2.3.1"), (ct, "9.9.9")]);
        let (s, res) = assoc.storage_commit(sop, "1.9.1", &req).await.unwrap();
        assert_eq!(s, status::SUCCESS);
        let (got_txn, succ, failed) = res.unwrap();
        assert_eq!(got_txn.as_deref(), Some(txn.as_str()));
        assert_eq!(succ, vec![(ct.to_string(), "1.2.3.1".to_string())]);
        assert_eq!(failed, vec![(ct.to_string(), "9.9.9".to_string())]);
        assoc.release().await.unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn loopback_move_and_get() {
        // Move destination SCP (plain storage).
        let dest_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let dest_addr = dest_listener.local_addr().unwrap();
        let dest_handlers = ArcTestScp::new(TestScp {
            stored: Mutex::new(vec![]),
            find_matches: vec![],
            move_instances: vec![],
            fail_store: false,
        });
        let d2 = dest_handlers.clone();
        tokio::spawn(async move {
            let (stream, _) = dest_listener.accept().await.unwrap();
            stream.set_nodelay(true).unwrap();
            let acc = AcceptorOptions {
                association: short_opts(),
                ..Default::default()
            };
            accept(stream, &acc, &*d2, &ScpOptions::default())
                .await
                .unwrap();
        });

        // Main SCP serving MOVE + GET from canned instances.
        let instances = vec![
            MoveInstance {
                sop_class_uid: vyna_dcm_core::sop_class::CT_IMAGE.into(),
                sop_instance_uid: "1.2.3.11".into(),
                dataset: test_ds("1.2.3.11"),
            },
            MoveInstance {
                sop_class_uid: vyna_dcm_core::sop_class::CT_IMAGE.into(),
                sop_instance_uid: "1.2.3.12".into(),
                dataset: test_ds("1.2.3.12"),
            },
        ];
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let handlers = ArcTestScp::new(TestScp {
            stored: Mutex::new(vec![]),
            find_matches: vec![],
            move_instances: instances,
            fail_store: false,
        });
        let h2 = handlers.clone();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            stream.set_nodelay(true).unwrap();
            let acc = AcceptorOptions {
                association: short_opts(),
                ..Default::default()
            };
            let mut dir = HashMap::new();
            dir.insert(
                "MOVEDEST".to_string(),
                ("127.0.0.1".to_string(), dest_addr.port()),
            );
            let scp_opts = ScpOptions {
                move_config: Some(crate::service::ScpMoveConfig {
                    directory: Arc::new(crate::service::HashMapDirectory { map: dir }),
                    assoc_options: short_opts(),
                    calling_ae: AeTitle::new("MOVESCP").unwrap(),
                }),
            };
            accept(stream, &acc, &*h2, &scp_opts).await.unwrap();
        });

        let ts = [vyna_dcm_core::TransferSyntax::ExplicitVrLittleEndian];
        let mut assoc = Association::request(
            &short_opts(),
            &AeTitle::new("SCU").unwrap(),
            &AeTitle::new("SCP").unwrap(),
            &addr.to_string(),
            propose(
                &[
                    vyna_dcm_core::sop_class::STUDY_ROOT_MOVE,
                    vyna_dcm_core::sop_class::STUDY_ROOT_GET,
                    vyna_dcm_core::sop_class::CT_IMAGE,
                ],
                &ts,
            )
            .unwrap(),
        )
        .await
        .unwrap();

        let r = assoc
            .move_to(
                vyna_dcm_core::sop_class::STUDY_ROOT_MOVE,
                &DataSet::new(),
                "MOVEDEST",
                priority::MEDIUM,
            )
            .await
            .unwrap();
        assert_eq!((r.status, r.completed, r.failed), (status::SUCCESS, 2, 0));

        let mut got = Vec::new();
        let r = assoc
            .get_with(
                vyna_dcm_core::sop_class::STUDY_ROOT_GET,
                &DataSet::new(),
                priority::MEDIUM,
                |c, i, ds, _ts| {
                    got.push((c, i, ds));
                    status::SUCCESS
                },
            )
            .await
            .unwrap();
        assert_eq!((r.status, r.completed), (status::SUCCESS, 2));
        assert_eq!(got.len(), 2);
        assoc.release().await.unwrap();

        // Destination received both MOVE sub-operations.
        assert_eq!(dest_handlers.stored.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn loopback_move_and_get_failed_subop_reports_b000() {
        // Move destination SCP that fails every C-STORE.
        let dest_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let dest_addr = dest_listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (stream, _) = dest_listener.accept().await.unwrap();
            stream.set_nodelay(true).unwrap();
            let acc = AcceptorOptions {
                association: short_opts(),
                ..Default::default()
            };
            let dest = TestScp {
                stored: Mutex::new(vec![]),
                find_matches: vec![],
                move_instances: vec![],
                fail_store: true,
            };
            accept(stream, &acc, &dest, &ScpOptions::default())
                .await
                .unwrap();
        });

        // Main SCP serving one instance for MOVE + GET.
        let instances = vec![MoveInstance {
            sop_class_uid: vyna_dcm_core::sop_class::CT_IMAGE.into(),
            sop_instance_uid: "1.2.3.11".into(),
            dataset: test_ds("1.2.3.11"),
        }];
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let handlers = ArcTestScp::new(TestScp {
            stored: Mutex::new(vec![]),
            find_matches: vec![],
            move_instances: instances,
            fail_store: false,
        });
        let h2 = handlers.clone();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            stream.set_nodelay(true).unwrap();
            let acc = AcceptorOptions {
                association: short_opts(),
                ..Default::default()
            };
            let mut dir = HashMap::new();
            dir.insert(
                "MOVEDEST".to_string(),
                ("127.0.0.1".to_string(), dest_addr.port()),
            );
            let scp_opts = ScpOptions {
                move_config: Some(crate::service::ScpMoveConfig {
                    directory: Arc::new(crate::service::HashMapDirectory { map: dir }),
                    assoc_options: short_opts(),
                    calling_ae: AeTitle::new("MOVESCP").unwrap(),
                }),
            };
            accept(stream, &acc, &*h2, &scp_opts).await.unwrap();
        });

        let ts = [vyna_dcm_core::TransferSyntax::ExplicitVrLittleEndian];
        let mut assoc = Association::request(
            &short_opts(),
            &AeTitle::new("SCU").unwrap(),
            &AeTitle::new("SCP").unwrap(),
            &addr.to_string(),
            propose(
                &[
                    vyna_dcm_core::sop_class::STUDY_ROOT_MOVE,
                    vyna_dcm_core::sop_class::STUDY_ROOT_GET,
                    vyna_dcm_core::sop_class::CT_IMAGE,
                ],
                &ts,
            )
            .unwrap(),
        )
        .await
        .unwrap();

        // Failed sub-operation: final is B000, not SUCCESS.
        let r = assoc
            .move_to(
                vyna_dcm_core::sop_class::STUDY_ROOT_MOVE,
                &DataSet::new(),
                "MOVEDEST",
                priority::MEDIUM,
            )
            .await
            .unwrap();
        assert_eq!(
            (r.status, r.completed, r.failed),
            (status::SUBOPERATIONS_COMPLETE_WITH_FAILURES, 0, 1)
        );

        // Same for C-GET when the receiver rejects the instance.
        let r = assoc
            .get_with(
                vyna_dcm_core::sop_class::STUDY_ROOT_GET,
                &DataSet::new(),
                priority::MEDIUM,
                |_, _, _, _| status::PROCESSING_FAILURE,
            )
            .await
            .unwrap();
        assert_eq!(
            (r.status, r.completed, r.failed),
            (status::SUBOPERATIONS_COMPLETE_WITH_FAILURES, 0, 1)
        );
        assoc.release().await.unwrap();
    }

    use std::collections::HashMap;
    use std::sync::Arc;

    /// `Arc<TestScp>` derefs to `TestScp: Handlers`.
    type ArcTestScp = Arc<TestScp>;

    /// DIMSE-level loopback pair (no A-ASSOCIATE): client + scripted server.
    fn dimse_pair() -> (Association, Association) {
        let opts = short_opts();
        let (a, b) = tokio::io::duplex(65536);
        let ctx = || crate::association::NegotiatedContext {
            id: 1,
            abstract_syntax: vyna_dcm_core::sop_class::STORAGE_COMMITMENT_PUSH.to_string(),
            transfer_syntax: vyna_dcm_core::TransferSyntax::ExplicitVrLittleEndian,
            transfer_syntax_uid: vyna_dcm_core::TransferSyntax::ExplicitVrLittleEndian
                .uid()
                .to_string(),
        };
        let info_a = crate::association::AssociationInfo {
            local_ae: AeTitle::new("SCU").unwrap(),
            peer_ae: AeTitle::new("SCP").unwrap(),
            peer_max_pdu: 0,
        };
        let info_b = crate::association::AssociationInfo {
            local_ae: AeTitle::new("SCP").unwrap(),
            peer_ae: AeTitle::new("SCU").unwrap(),
            peer_max_pdu: 0,
        };
        (
            Association::from_accepted(Box::new(a), info_a, vec![ctx()], &opts),
            Association::from_accepted(Box::new(b), info_b, vec![ctx()], &opts),
        )
    }

    async fn recv_message(assoc: &mut Association) -> DimseMessage {
        match assoc.recv().await.unwrap() {
            DimseEvent::Message(m) => *m,
            DimseEvent::Released => panic!("peer released"),
        }
    }

    #[tokio::test]
    async fn move_to_rejects_bad_destination_before_wire() {
        let (mut scu, _scp) = dimse_pair();
        for bad in ["", "   ", "THIS_AE_TITLE_IS_WAY_TOO_LONG"] {
            let err = scu
                .move_to("1.2.3", &DataSet::new(), bad, priority::MEDIUM)
                .await
                .expect_err("bad dest must fail");
            assert!(matches!(err, Error::BadAeTitle(_, _)), "{err:?}");
        }
    }

    #[tokio::test]
    async fn loopback_storage_commit() {
        let (mut scu, mut scp) = dimse_pair();
        let server = tokio::spawn(async move {
            let rq = recv_message(&mut scp).await;
            assert_eq!(rq.command.field, CF::N_ACTION_RQ);
            let rq_id = rq.command.message_id.unwrap_or(0);
            scp.send(
                rq.context_id,
                &Command::n_action_rsp(
                    rq_id,
                    rq.command.requested_sop_class_uid.clone(),
                    rq.command.requested_sop_instance_uid.clone(),
                    status::SUCCESS,
                ),
                None,
            )
            .await
            .unwrap();
            // Commitment report: one success, one failure.
            let mut ok = DataSet::new();
            ok.put_uid(
                tags::REFERENCED_SOP_CLASS_UID,
                vyna_dcm_core::sop_class::CT_IMAGE,
            );
            ok.put_uid(tags::REFERENCED_SOP_INSTANCE_UID, "1.2.3.1");
            let mut ng = DataSet::new();
            ng.put_uid(
                tags::REFERENCED_SOP_CLASS_UID,
                vyna_dcm_core::sop_class::MR_IMAGE,
            );
            ng.put_uid(tags::REFERENCED_SOP_INSTANCE_UID, "1.2.3.2");
            let mut report = DataSet::new();
            report.put_uid(tags::TRANSACTION_UID, "1.9.9");
            report.put_sequence(tags::REFERENCED_SOP_SEQUENCE, vec![ok]);
            report.put_sequence(tags::FAILED_SOP_SEQUENCE, vec![ng]);
            let ev = Command::n_event_report_rq(
                7,
                vyna_dcm_core::sop_class::STORAGE_COMMITMENT_PUSH,
                "1.9.1",
                1,
                true,
            );
            scp.send(1, &ev, Some(&report)).await.unwrap();
            // SCU must acknowledge the report.
            let ack = recv_message(&mut scp).await;
            assert_eq!(ack.command.field, CF::N_EVENT_REPORT_RSP);
            assert_eq!(ack.command.status, Some(status::SUCCESS));
        });

        let (req, _txn) = crate::worklist::storage_commitment_request(&[
            (vyna_dcm_core::sop_class::CT_IMAGE, "1.2.3.1"),
            (vyna_dcm_core::sop_class::MR_IMAGE, "1.2.3.2"),
        ]);
        let (s, res) = scu
            .storage_commit(
                vyna_dcm_core::sop_class::STORAGE_COMMITMENT_PUSH,
                "1.9.1",
                &req,
            )
            .await
            .unwrap();
        assert_eq!(s, status::SUCCESS);
        let (txn, succ, failed) = res.unwrap();
        assert_eq!(txn.as_deref(), Some("1.9.9"));
        assert_eq!(
            succ,
            vec![(
                vyna_dcm_core::sop_class::CT_IMAGE.to_string(),
                "1.2.3.1".to_string()
            )]
        );
        assert_eq!(failed.len(), 1);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn storage_commit_skips_wait_on_action_failure() {
        let (mut scu, mut scp) = dimse_pair();
        let server = tokio::spawn(async move {
            let rq = recv_message(&mut scp).await;
            assert_eq!(rq.command.field, CF::N_ACTION_RQ);
            let rq_id = rq.command.message_id.unwrap_or(0);
            scp.send(
                rq.context_id,
                &Command::n_action_rsp(
                    rq_id,
                    rq.command.requested_sop_class_uid.clone(),
                    rq.command.requested_sop_instance_uid.clone(),
                    status::PROCESSING_FAILURE,
                ),
                None,
            )
            .await
            .unwrap();
            // No report follows; SCU must not wait for one.
        });

        let (req, _txn) = crate::worklist::storage_commitment_request(&[(
            vyna_dcm_core::sop_class::CT_IMAGE,
            "1.2.3.1",
        )]);
        let (s, res) = scu
            .storage_commit(
                vyna_dcm_core::sop_class::STORAGE_COMMITMENT_PUSH,
                "1.9.1",
                &req,
            )
            .await
            .unwrap();
        assert_eq!((s, res.is_none()), (status::PROCESSING_FAILURE, true));
        server.await.unwrap();
    }
}
