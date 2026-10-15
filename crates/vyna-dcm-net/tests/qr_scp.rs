//! Loopback tests: real SCU↔SCP associations against `QrScp`/`MwlScp`
//! over temp Part-10 directories (C-FIND/MOVE/GET + worklist).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use vyna_dcm_codec::{FileMeta, write_part10};
use vyna_dcm_core::sop_class;
use vyna_dcm_core::{DataElement, DataSet, TransferSyntax, Value, Vr, tags};
use vyna_dcm_net::dimse::DATA_SET_NULL;
use vyna_dcm_net::qrdb::{MwlScp, QrIndex, QrScp};
use vyna_dcm_net::service::{
    AcceptorOptions, Handlers, HashMapDirectory, RequestContext, ScpMoveConfig, ScpOptions, accept,
    propose,
};
use vyna_dcm_net::{AeTitle, Association, AssociationOptions, Command, DimseEvent};

fn opts() -> AssociationOptions {
    let mut o = AssociationOptions::default();
    o.read_timeout = Duration::from_secs(10);
    o
}

fn acceptor() -> AcceptorOptions {
    let mut o = AcceptorOptions::default();
    o.association = opts();
    o
}

fn tmpdir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("vyna-qr-{}-{}", name, std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn instance_ds(patient: &str, study: &str, series: &str, inst: &str) -> DataSet {
    let mut ds = DataSet::new();
    ds.put_text(tags::SOP_CLASS_UID, Vr::Ui, sop_class::CT_IMAGE);
    ds.put_text(tags::SOP_INSTANCE_UID, Vr::Ui, inst);
    ds.put_text(tags::PATIENT_ID, Vr::Lo, patient);
    ds.put_text(tags::PATIENT_NAME, Vr::Pn, "Doe^John");
    ds.put_text(tags::STUDY_INSTANCE_UID, Vr::Ui, study);
    ds.put_text(tags::SERIES_INSTANCE_UID, Vr::Ui, series);
    ds.put_text(tags::MODALITY, Vr::Cs, "CT");
    ds.insert(DataElement::new(
        tags::PIXEL_DATA,
        Vr::Ob,
        Value::Bytes(vec![1, 2, 3, 4]),
    ));
    ds
}

fn write_instance(dir: &std::path::Path, name: &str, ds: &DataSet) {
    let meta = FileMeta::new(
        sop_class::CT_IMAGE,
        ds.string(tags::SOP_INSTANCE_UID).unwrap(),
        TransferSyntax::ExplicitVrLittleEndian,
    );
    std::fs::write(dir.join(name), write_part10(&meta, ds).unwrap()).unwrap();
}

fn qr_dir() -> PathBuf {
    // Unique per call: tests run in parallel.
    static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let dir = tmpdir(&format!("qr-{n}"));
    write_instance(
        &dir,
        "a1.dcm",
        &instance_ds("P1", "1.2.1", "1.2.1.1", "1.2.1.1.1"),
    );
    write_instance(
        &dir,
        "a2.dcm",
        &instance_ds("P1", "1.2.1", "1.2.1.1", "1.2.1.1.2"),
    );
    write_instance(
        &dir,
        "b1.dcm",
        &instance_ds("P2", "1.2.2", "1.2.2.1", "1.2.2.1.1"),
    );
    // One junk file: skipped, never fatal.
    std::fs::write(dir.join("note.txt"), b"not dicom").unwrap();
    dir
}

fn study_query(patient: &str) -> DataSet {
    let mut q = DataSet::new();
    q.put_text(tags::QUERY_RETRIEVE_LEVEL, Vr::Cs, "STUDY");
    q.put_text(tags::PATIENT_ID, Vr::Lo, patient);
    q.put_null(tags::STUDY_INSTANCE_UID, Vr::Ui);
    q.put_null(tags::PATIENT_NAME, Vr::Pn);
    q
}

// Concrete serve fns (the `Handlers` trait's native async fns are not
// Send-bounded, so each spawn site names its handler type).
macro_rules! serve_with {
    ($h:expr, $o:expr) => {{
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let handlers = $h;
        let scp_opts = $o;
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            stream.set_nodelay(true).unwrap();
            accept(stream, &acceptor(), &handlers, &scp_opts)
                .await
                .unwrap();
        });
        addr
    }};
}

async fn serve_qr(index: QrIndex, scp_opts: ScpOptions) -> String {
    serve_with!(QrScp::new(index), scp_opts)
}

async fn serve_mwl(index: QrIndex) -> String {
    serve_with!(MwlScp::new(index), ScpOptions::default())
}

async fn connect(addr: &str, sops: &[&str]) -> Association {
    Association::request(
        &opts(),
        &AeTitle::new("SCU").unwrap(),
        &AeTitle::new("SCP").unwrap(),
        addr,
        propose(
            sops,
            &[
                TransferSyntax::ExplicitVrLittleEndian,
                TransferSyntax::ImplicitVrLittleEndian,
            ],
        )
        .unwrap(),
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn find_study_level_groups() {
    let dir = qr_dir();
    let index = QrIndex::scan(&dir).unwrap();
    assert_eq!(index.records().len(), 3);
    assert_eq!(index.skipped, 1);
    let addr = serve_qr(index, ScpOptions::default()).await;
    let mut assoc = connect(&addr, &[sop_class::STUDY_ROOT_FIND]).await;
    let (status, matches) = assoc
        .find_all(sop_class::STUDY_ROOT_FIND, &study_query("P*"), 0)
        .await
        .unwrap();
    assert_eq!(status, 0x0000);
    assert_eq!(matches.len(), 2);
    assert_eq!(
        matches[0].person_names(tags::PATIENT_NAME).unwrap()[0].to_dicom_string(),
        "Doe^John",
        "requested keys echo with record values"
    );
    assoc.release().await.unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn find_missing_level_fails() {
    let dir = qr_dir();
    let addr = serve_qr(QrIndex::scan(&dir).unwrap(), ScpOptions::default()).await;
    let mut assoc = connect(&addr, &[sop_class::STUDY_ROOT_FIND]).await;
    let (status, matches) = assoc
        .find_all(sop_class::STUDY_ROOT_FIND, &DataSet::new(), 0)
        .await
        .unwrap();
    assert_eq!(status, 0xA900);
    assert!(matches.is_empty());
    assoc.release().await.unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

/// Raw C-FIND-RQ with Data Set Type NULL (no Identifier): the SCP
/// must fail it, never treat it as an empty (MATCH-ALL) query.
async fn find_without_dataset(assoc: &mut Association, sop: &str) -> (u16, bool) {
    let ctx = assoc.context_for(sop).unwrap().clone();
    let id = assoc.next_message_id();
    let mut rq = Command::find_rq(id, 0, sop).unwrap();
    rq.data_set_type = DATA_SET_NULL;
    assoc.send(ctx.id, &rq, None).await.unwrap();
    let mut saw_pending = false;
    loop {
        let DimseEvent::Message(msg) = assoc.recv().await.unwrap() else {
            panic!("association released mid-FIND");
        };
        let s = msg.command.status.unwrap();
        if s == 0xFF00 {
            saw_pending = true;
            continue;
        }
        return (s, saw_pending);
    }
}

#[tokio::test]
async fn find_without_dataset_fails() {
    let dir = qr_dir();
    let addr = serve_qr(QrIndex::scan(&dir).unwrap(), ScpOptions::default()).await;
    let mut assoc = connect(&addr, &[sop_class::STUDY_ROOT_FIND]).await;
    let (status, saw_pending) = find_without_dataset(&mut assoc, sop_class::STUDY_ROOT_FIND).await;
    assert_eq!(status, 0xA900);
    assert!(!saw_pending);
    assoc.release().await.unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn mwl_find_without_dataset_fails_instead_of_match_all() {
    let dir = tmpdir("mwl-nods");
    let mut ds = DataSet::new();
    ds.put_text(tags::SOP_CLASS_UID, Vr::Ui, sop_class::CT_IMAGE);
    ds.put_text(tags::SOP_INSTANCE_UID, Vr::Ui, "1.2.9.7");
    ds.put_text(tags::PATIENT_ID, Vr::Lo, "W7");
    write_instance(&dir, "w7.dcm", &ds);
    let addr = serve_mwl(QrIndex::scan(&dir).unwrap()).await;
    let mut assoc = connect(&addr, &[sop_class::MODALITY_WORKLIST_FIND]).await;
    let (status, saw_pending) =
        find_without_dataset(&mut assoc, sop_class::MODALITY_WORKLIST_FIND).await;
    assert_eq!(status, 0xA900);
    assert!(!saw_pending, "dataset-less MWL FIND must not emit matches");
    assoc.release().await.unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

#[derive(Default)]
struct Collector {
    stored: Mutex<Vec<DataSet>>,
}

impl Handlers for Collector {
    async fn on_store(&self, _ctx: &RequestContext<'_>, dataset: DataSet) -> u16 {
        self.stored.lock().unwrap().push(dataset);
        0x0000
    }
}

#[tokio::test]
async fn move_delivers_to_destination() {
    let dir = qr_dir();
    // Destination storage SCP.
    let dest: Arc<Collector> = Arc::new(Collector::default());
    let d2 = dest.clone();
    let dest_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let dest_addr = dest_listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (stream, _) = dest_listener.accept().await.unwrap();
        stream.set_nodelay(true).unwrap();
        accept(stream, &acceptor(), &*d2, &ScpOptions::default())
            .await
            .unwrap();
    });
    // QR SCP with a route to it.
    let mut map = HashMap::new();
    map.insert(
        "DEST".to_string(),
        ("127.0.0.1".to_string(), dest_addr.port()),
    );
    let mut scp_opts = ScpOptions::default();
    scp_opts.move_config = Some(ScpMoveConfig {
        directory: Arc::new(HashMapDirectory { map }),
        assoc_options: opts(),
        calling_ae: AeTitle::new("FINDSCP").unwrap(),
    });
    let addr = serve_qr(QrIndex::scan(&dir).unwrap(), scp_opts).await;
    let mut assoc = connect(&addr, &[sop_class::STUDY_ROOT_MOVE]).await;
    let result = assoc
        .move_to(sop_class::STUDY_ROOT_MOVE, &study_query("P1"), "DEST", 0)
        .await
        .unwrap();
    assert_eq!(result.status, 0x0000);
    assert_eq!((result.completed, result.failed), (2, 0));
    assoc.release().await.unwrap();
    let stored = dest.stored.lock().unwrap();
    assert_eq!(stored.len(), 2);
    assert!(
        stored
            .iter()
            .all(|ds| ds.string(tags::PATIENT_ID) == Some("P1"))
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn empty_move_plan_succeeds_without_dialing() {
    let dir = qr_dir();
    // Route DEST at a closed port: if the SCP dialed, this fails.
    let dead = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let dead_port = dead.local_addr().unwrap().port();
    drop(dead);
    let mut map = HashMap::new();
    map.insert("DEST".to_string(), ("127.0.0.1".to_string(), dead_port));
    let mut scp_opts = ScpOptions::default();
    scp_opts.move_config = Some(ScpMoveConfig {
        directory: Arc::new(HashMapDirectory { map }),
        assoc_options: opts(),
        calling_ae: AeTitle::new("FINDSCP").unwrap(),
    });
    let addr = serve_qr(QrIndex::scan(&dir).unwrap(), scp_opts).await;
    let mut assoc = connect(&addr, &[sop_class::STUDY_ROOT_MOVE]).await;
    // Query matches nothing: SUCCESS with zero counts, no outbound dial.
    let result = assoc
        .move_to(
            sop_class::STUDY_ROOT_MOVE,
            &study_query("NOBODY"),
            "DEST",
            0,
        )
        .await
        .unwrap();
    assert_eq!(result.status, 0x0000);
    assert_eq!((result.completed, result.failed, result.warning), (0, 0, 0));
    assoc.release().await.unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn move_outbound_death_fails_fast_with_processing_failure() {
    use vyna_dcm_net::pdu::{AcceptedContext, AssociateAc, Pdu, read_pdu, write_pdu};
    let dir = qr_dir();
    // Destination: completes the association handshake, then A-ABORTs
    // on the first P-DATA (simulates transport death mid-MOVE).
    let dest_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let dest_addr = dest_listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut tcp, _) = dest_listener.accept().await.unwrap();
        let rq = match read_pdu(&mut tcp, 64 * 1024 * 1024).await.unwrap() {
            Pdu::AssociateRq(rq) => rq,
            other => panic!("expected RQ, got {other:?}"),
        };
        let ac = AssociateAc {
            application_context: rq.application_context.clone(),
            presentation_contexts: rq
                .presentation_contexts
                .iter()
                .map(|p| AcceptedContext {
                    id: p.id,
                    result: 0,
                    transfer_syntax: p.transfer_syntaxes.first().cloned(),
                })
                .collect(),
            max_pdu_len: 16384,
            implementation_class_uid: "1.2.9".into(),
            implementation_version: None,
            async_ops: None,
            role_selection: vec![],
            user_identity_response: None,
        };
        write_pdu(&mut tcp, &Pdu::AssociateAc(ac)).await.unwrap();
        // First P-DATA (a C-STORE-RQ): abort instead of answering.
        let first = read_pdu(&mut tcp, 64 * 1024 * 1024).await.unwrap();
        assert!(matches!(first, Pdu::Data(_)), "{first:?}");
        write_pdu(
            &mut tcp,
            &Pdu::Abort {
                source: 2,
                reason: 0,
            },
        )
        .await
        .unwrap();
    });
    let mut map = HashMap::new();
    map.insert(
        "DEST".to_string(),
        ("127.0.0.1".to_string(), dest_addr.port()),
    );
    let mut scp_opts = ScpOptions::default();
    scp_opts.move_config = Some(ScpMoveConfig {
        directory: Arc::new(HashMapDirectory { map }),
        assoc_options: opts(),
        calling_ae: AeTitle::new("FINDSCP").unwrap(),
    });
    let addr = serve_qr(QrIndex::scan(&dir).unwrap(), scp_opts).await;
    let mut assoc = connect(&addr, &[sop_class::STUDY_ROOT_MOVE]).await;
    // Two instances would move; the dead transport must fail the whole
    // MOVE with PROCESSING_FAILURE (not per-instance B000), promptly:
    // well under one 10 s read_timeout, let alone two.
    let t0 = std::time::Instant::now();
    let result = assoc
        .move_to(sop_class::STUDY_ROOT_MOVE, &study_query("P1"), "DEST", 0)
        .await
        .unwrap();
    assert_eq!(result.status, 0x0110);
    assert!(
        t0.elapsed() < Duration::from_secs(10),
        "MOVE hung on dead socket"
    );
    assoc.release().await.unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn send_flag_mismatch_is_rejected() {
    use vyna_dcm_net::Error;
    let dir = qr_dir();
    let addr = serve_qr(QrIndex::scan(&dir).unwrap(), ScpOptions::default()).await;
    let mut assoc = connect(&addr, &[sop_class::STUDY_ROOT_FIND]).await;
    let ctx = assoc
        .context_for(sop_class::STUDY_ROOT_FIND)
        .unwrap()
        .clone();
    let id = assoc.next_message_id();
    // Flag says dataset, none given.
    let rq = Command::find_rq(id, 0, sop_class::STUDY_ROOT_FIND).unwrap();
    let err = assoc.send(ctx.id, &rq, None).await.unwrap_err();
    assert!(matches!(err, Error::BadCommand(_)), "{err:?}");
    // Dataset given, flag says none.
    let mut rq = Command::find_rq(id, 0, sop_class::STUDY_ROOT_FIND).unwrap();
    rq.data_set_type = DATA_SET_NULL;
    let err = assoc
        .send(ctx.id, &rq, Some(&DataSet::new()))
        .await
        .unwrap_err();
    assert!(matches!(err, Error::BadCommand(_)), "{err:?}");
    assoc.release().await.unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn ps_only_move_rejects_series_level() {
    let dir = qr_dir();
    // No move_config: STUDY passes the guard (then fails unknown
    // destination), SERIES must fail identifier-mismatch first.
    let addr = serve_qr(QrIndex::scan(&dir).unwrap(), ScpOptions::default()).await;
    let mut assoc = connect(&addr, &[sop_class::PATIENT_STUDY_ONLY_MOVE]).await;
    let mut q = DataSet::new();
    q.put_text(tags::QUERY_RETRIEVE_LEVEL, Vr::Cs, "SERIES");
    q.put_text(tags::SERIES_INSTANCE_UID, Vr::Ui, "1.2.1.1");
    let result = assoc
        .move_to(sop_class::PATIENT_STUDY_ONLY_MOVE, &q, "DEST", 0)
        .await
        .unwrap();
    assert_eq!(result.status, 0xA900);
    let mut q = DataSet::new();
    q.put_text(tags::QUERY_RETRIEVE_LEVEL, Vr::Cs, "STUDY");
    q.put_text(tags::STUDY_INSTANCE_UID, Vr::Ui, "1.2.1");
    let result = assoc
        .move_to(sop_class::PATIENT_STUDY_ONLY_MOVE, &q, "DEST", 0)
        .await
        .unwrap();
    assert_eq!(result.status, 0xA801);
    assoc.release().await.unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn get_survives_stray_cancel_for_other_op() {
    use vyna_dcm_net::command_field as CF;
    let dir = qr_dir();
    let addr = serve_qr(QrIndex::scan(&dir).unwrap(), ScpOptions::default()).await;
    let mut assoc = connect(&addr, &[sop_class::STUDY_ROOT_GET, sop_class::CT_IMAGE]).await;
    let ctx = assoc
        .context_for(sop_class::STUDY_ROOT_GET)
        .unwrap()
        .clone();
    let id = assoc.next_message_id();
    let mut q = DataSet::new();
    q.put_text(tags::QUERY_RETRIEVE_LEVEL, Vr::Cs, "IMAGE");
    q.put_text(tags::SOP_INSTANCE_UID, Vr::Ui, "1.2.2.1.1");
    assoc
        .send(
            ctx.id,
            &Command::get_rq(id, 0, sop_class::STUDY_ROOT_GET).unwrap(),
            Some(&q),
        )
        .await
        .unwrap();
    // First: the C-STORE-RQ for the single match.
    let store = match assoc.recv().await.unwrap() {
        DimseEvent::Message(m) => *m,
        DimseEvent::Released => panic!("released mid-GET"),
    };
    assert_eq!(store.command.field, CF::C_STORE_RQ);
    let store_id = store.command.message_id.unwrap();
    // Stray: C-CANCEL for an unknown operation. This used to abort
    // the whole association; now it is stashed and ignored.
    assoc
        .send(ctx.id, &Command::cancel_rq(id.wrapping_add(100)), None)
        .await
        .unwrap();
    // Answer the store; the GET must continue to SUCCESS.
    let rsp = Command::store_rsp(
        store_id,
        store.command.affected_sop_class_uid.clone(),
        store.command.affected_sop_instance_uid.clone(),
        0x0000,
    );
    assoc.send(store.context_id, &rsp, None).await.unwrap();
    loop {
        match assoc.recv().await.unwrap() {
            DimseEvent::Released => panic!("released mid-GET"),
            DimseEvent::Message(m) => {
                assert_eq!(m.command.field, CF::C_GET_RSP);
                let s = m.command.status.unwrap();
                if s == 0xFF00 {
                    continue;
                }
                assert_eq!(s, 0x0000);
                break;
            }
        }
    }
    assoc.release().await.unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

/// Raw C-GET for the single `1.2.2.1.1` match, stopped after the
/// C-STORE-RQ arrives: returns `(dir, assoc, get_ctx, store_ctx,
/// get_id, store_id)` for stray-injection tests.
async fn stray_get_setup() -> (PathBuf, Association, u8, u8, u16, u16) {
    let dir = qr_dir();
    let addr = serve_qr(QrIndex::scan(&dir).unwrap(), ScpOptions::default()).await;
    let mut assoc = connect(&addr, &[sop_class::STUDY_ROOT_GET, sop_class::CT_IMAGE]).await;
    let ctx = assoc.context_for(sop_class::STUDY_ROOT_GET).unwrap().id;
    let st_ctx = assoc.context_for(sop_class::CT_IMAGE).unwrap().id;
    let id = assoc.next_message_id();
    let mut q = DataSet::new();
    q.put_text(tags::QUERY_RETRIEVE_LEVEL, Vr::Cs, "IMAGE");
    q.put_text(tags::SOP_INSTANCE_UID, Vr::Ui, "1.2.2.1.1");
    assoc
        .send(
            ctx,
            &Command::get_rq(id, 0, sop_class::STUDY_ROOT_GET).unwrap(),
            Some(&q),
        )
        .await
        .unwrap();
    let store = match assoc.recv().await.unwrap() {
        DimseEvent::Message(m) => *m,
        DimseEvent::Released => panic!("released mid-GET"),
    };
    assert_eq!(store.command.field, vyna_dcm_net::command_field::C_STORE_RQ);
    let store_id = store.command.message_id.unwrap();
    (dir, assoc, ctx, st_ctx, id, store_id)
}

#[tokio::test]
async fn get_stray_count_flood_aborts() {
    let (dir, mut assoc, ctx, _st_ctx, id, _) = stray_get_setup().await;
    // 17 dataset-less strays: the 17th exceeds the 16-event hold
    // budget and the GET fails (association aborts).
    for i in 0..17u16 {
        assoc
            .send(ctx, &Command::cancel_rq(id.wrapping_add(100 + i)), None)
            .await
            .unwrap();
    }
    assert!(assoc.recv().await.is_err(), "flooded GET must abort");
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn get_stray_byte_flood_aborts() {
    let (dir, mut assoc, _ctx, st_ctx, _, _) = stray_get_setup().await;
    // 13 x 5 MiB stray datasets = 65 MiB past the 64 MiB hold budget.
    let mut ds = DataSet::new();
    ds.insert(DataElement::new(
        tags::PIXEL_DATA,
        Vr::Ob,
        Value::Bytes(vec![7u8; 5 * 1024 * 1024]),
    ));
    for i in 0..13u16 {
        let rq = Command::store_rq(500 + i, 0, sop_class::CT_IMAGE, "1.2.9.99").unwrap();
        assoc.send(st_ctx, &rq, Some(&ds)).await.unwrap();
    }
    assert!(assoc.recv().await.is_err(), "flooded GET must abort");
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn get_serves_over_same_association() {
    let dir = qr_dir();
    let addr = serve_qr(QrIndex::scan(&dir).unwrap(), ScpOptions::default()).await;
    let mut assoc = connect(&addr, &[sop_class::STUDY_ROOT_GET, sop_class::CT_IMAGE]).await;
    let mut got = Vec::new();
    let mut q = DataSet::new();
    q.put_text(tags::QUERY_RETRIEVE_LEVEL, Vr::Cs, "IMAGE");
    q.put_text(tags::SOP_INSTANCE_UID, Vr::Ui, "1.2.2.1.1");
    let result = assoc
        .get_with(sop_class::STUDY_ROOT_GET, &q, 0, |class, inst, ds, _ts| {
            got.push((class, inst, ds));
            0x0000
        })
        .await
        .unwrap();
    assert_eq!(result.status, 0x0000);
    assert_eq!(result.completed, 1);
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].0, sop_class::CT_IMAGE);
    assert_eq!(got[0].2.string(tags::PATIENT_ID), Some("P2"));
    assoc.release().await.unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn mwl_matches_station() {
    let dir = tmpdir("mwl");
    for (name, patient, station, uid) in [
        ("w1.dcm", "W1", "CT01", "1.2.9.1"),
        ("w2.dcm", "W2", "MR01", "1.2.9.2"),
    ] {
        let mut ds = DataSet::new();
        ds.put_text(tags::SOP_CLASS_UID, Vr::Ui, sop_class::CT_IMAGE);
        ds.put_text(tags::SOP_INSTANCE_UID, Vr::Ui, uid);
        ds.put_text(tags::PATIENT_ID, Vr::Lo, patient);
        ds.put_text(tags::ACCESSION_NUMBER, Vr::Sh, "A1");
        let mut sps = DataSet::new();
        sps.put_text(tags::SCHEDULED_STATION_AE_TITLE, Vr::Ae, station);
        sps.put_text(tags::MODALITY, Vr::Cs, "CT");
        ds.put_sequence(tags::SCHEDULED_PROCEDURE_STEP_SEQUENCE, vec![sps]);
        write_instance(&dir, name, &ds);
    }
    let addr = serve_mwl(QrIndex::scan(&dir).unwrap()).await;
    let mut assoc = connect(&addr, &[sop_class::MODALITY_WORKLIST_FIND]).await;
    let q = vyna_dcm_net::worklist::mwl_query_for_station("CT01");
    let (status, matches) = assoc
        .find_all(sop_class::MODALITY_WORKLIST_FIND, &q, 0)
        .await
        .unwrap();
    assert_eq!(status, 0x0000);
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].string(tags::PATIENT_ID), Some("W1"));
    assoc.release().await.unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn user_identity_gate_rejects_anonymous() {
    use vyna_dcm_net::Error;
    use vyna_dcm_net::pdu::UserIdentity;

    let dir = qr_dir();
    let index = QrIndex::scan(&dir).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut acc = acceptor();
        // Reject everything: only the gate firing matters here.
        acc.user_identity_auth = Some(std::sync::Arc::new(|_: &UserIdentity| None));
        let _ = accept(stream, &acc, &QrScp::new(index), &ScpOptions::default()).await;
    });
    // The SCU sends no user identity, so the gate rejects.
    let err = vyna_dcm_net::Association::request(
        &opts(),
        &AeTitle::new("SCU").unwrap(),
        &AeTitle::new("SCP").unwrap(),
        &addr,
        propose(
            &[sop_class::STUDY_ROOT_FIND],
            &[
                TransferSyntax::ExplicitVrLittleEndian,
                TransferSyntax::ImplicitVrLittleEndian,
            ],
        )
        .unwrap(),
    )
    .await
    .err()
    .expect("anonymous association was accepted");
    assert!(matches!(err, Error::Rejected { .. }), "{err:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

struct NHandlers;

impl Handlers for NHandlers {
    async fn on_n_get(&self, _ctx: &RequestContext<'_>) -> (u16, Option<DataSet>) {
        let mut ds = DataSet::new();
        ds.put_text(tags::PATIENT_ID, Vr::Lo, "N123");
        (0x0000, Some(ds))
    }

    async fn on_n_delete(&self, _ctx: &RequestContext<'_>) -> u16 {
        0x0000
    }

    async fn on_n_event_report(&self, _ctx: &RequestContext<'_>, _dataset: Option<DataSet>) -> u16 {
        0x0000
    }
}

#[tokio::test]
async fn n_services_dispatch_loopback() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        stream.set_nodelay(true).unwrap();
        accept(stream, &acceptor(), &NHandlers, &ScpOptions::default())
            .await
            .unwrap();
    });
    let sop = sop_class::MODALITY_PERFORMED_PROCEDURE_STEP;
    let mut assoc = connect(&addr, &[sop]).await;
    let (s, ds) = assoc.n_get(sop, "1.2.9.9", vec![]).await.unwrap();
    assert_eq!(s, 0x0000);
    assert_eq!(ds.unwrap().string(tags::PATIENT_ID), Some("N123"));
    assert_eq!(assoc.n_delete(sop, "1.2.9.9").await.unwrap(), 0x0000);
    assert_eq!(
        assoc.n_event_report(sop, "1.2.9.9", 1, None).await.unwrap(),
        0x0000
    );
    assoc.release().await.unwrap();
}

#[tokio::test]
async fn release_during_get_is_clean() {
    use vyna_dcm_net::dimse::command_field as cf;
    use vyna_dcm_net::{Command, DimseEvent};

    let dir = qr_dir();
    let index = QrIndex::scan(&dir).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        stream.set_nodelay(true).unwrap();
        accept(
            stream,
            &acceptor(),
            &QrScp::new(index),
            &ScpOptions::default(),
        )
        .await
    });
    let mut assoc = connect(&addr, &[sop_class::STUDY_ROOT_GET, sop_class::CT_IMAGE]).await;
    // Manual C-GET: read the first C-STORE-RQ, then release without
    // acking it, so the SCP sits in its store-wait when RELEASE arrives.
    let ctx = assoc
        .context_for(sop_class::STUDY_ROOT_GET)
        .unwrap()
        .clone();
    let id = assoc.next_message_id();
    let mut q = DataSet::new();
    q.put_text(tags::QUERY_RETRIEVE_LEVEL, Vr::Cs, "IMAGE");
    q.put_text(tags::SOP_INSTANCE_UID, Vr::Ui, "1.2.2.1.1");
    assoc
        .send(
            ctx.id,
            &Command::get_rq(id, 0, sop_class::STUDY_ROOT_GET).unwrap(),
            Some(&q),
        )
        .await
        .unwrap();
    let first = assoc.recv().await.unwrap();
    assert!(
        matches!(&first, DimseEvent::Message(m) if m.command.field == cf::C_STORE_RQ),
        "expected C-STORE-RQ, got {first:?}"
    );
    assoc.release().await.unwrap();
    // SCP side: quiet Ok — erroring here used to send A-ABORT after the
    // release completed.
    server.await.unwrap().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}
