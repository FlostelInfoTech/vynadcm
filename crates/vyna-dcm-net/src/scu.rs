//! SCU operations as [`Association`] methods: echo, store, find, move,
//! get and the N-services (MPPS, Storage Commitment, ...).
//!
//! Every method returns the DIMSE status (transport/protocol problems are
//! `Err`); use [`status::is_success`] et al. to classify statuses.

use vyna_dcm_core::{DataSet, Tag};

use crate::{
    Error, Result,
    association::{Association, DimseEvent},
    dimse::{Command, DimseMessage, command_field as CF, status},
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

    /// C-FIND collecting all matches.
    pub async fn find_all(
        &mut self,
        sop_class_uid: &str,
        query: &DataSet,
        priority: u16,
    ) -> Result<(u16, Vec<DataSet>)> {
        let mut matches = Vec::new();
        let s = self
            .find_with(sop_class_uid, query, priority, |ds| matches.push(ds))
            .await?;
        Ok((s, matches))
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
        let ctx = self
            .context_for(sop_class_uid)
            .ok_or_else(|| Error::NoPresentationContext(sop_class_uid.into()))?
            .clone();
        let id = self.next_message_id();
        let cmd = Command::move_rq(id, priority, sop_class_uid, destination_ae);
        self.send(ctx.id, &cmd, Some(query)).await?;
        self.await_move_get(CF::C_MOVE_RSP, id).await
    }

    /// C-GET: receive C-STORE sub-operations over this same association.
    /// `on_store` gets `(sop_class_uid, sop_instance_uid, dataset)` and
    /// returns the C-STORE-RSP status for each instance.
    pub async fn get_with(
        &mut self,
        sop_class_uid: &str,
        query: &DataSet,
        priority: u16,
        mut on_store: impl FnMut(String, String, DataSet) -> u16,
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
                    let s = match m.dataset {
                        Some(ds) => on_store(class.clone(), inst.clone(), ds),
                        None => status::PROCESSING_FAILURE,
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
    }

    impl Handlers for TestScp {
        async fn on_store(&self, _ctx: &RequestContext<'_>, dataset: DataSet) -> u16 {
            self.stored.lock().unwrap().push(dataset);
            status::SUCCESS
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
        });
        let h2 = handlers.clone();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
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
            ),
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
        // Cancelable FIND stopping after the first match.
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
        assert_eq!((seen, s), (1, status::SUCCESS));
        assoc.release().await.unwrap();
        assert_eq!(handlers.stored.lock().unwrap().len(), 1);
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
        });
        let d2 = dest_handlers.clone();
        tokio::spawn(async move {
            let (stream, _) = dest_listener.accept().await.unwrap();
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
        });
        let h2 = handlers.clone();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
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
            ),
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
                |c, i, ds| {
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

    use std::collections::HashMap;
    use std::sync::Arc;

    /// `Arc<TestScp>` derefs to `TestScp: Handlers`.
    type ArcTestScp = Arc<TestScp>;
}
