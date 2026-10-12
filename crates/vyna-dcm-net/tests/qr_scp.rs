//! Loopback tests: real SCU↔SCP associations against `QrScp`/`MwlScp`
//! over temp Part-10 directories (C-FIND/MOVE/GET + worklist).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use vyna_dcm_codec::{FileMeta, write_part10};
use vyna_dcm_core::sop_class;
use vyna_dcm_core::{DataElement, DataSet, TransferSyntax, Value, Vr, tags};
use vyna_dcm_net::qrdb::{MwlScp, QrIndex, QrScp};
use vyna_dcm_net::service::{
    AcceptorOptions, Handlers, HashMapDirectory, RequestContext, ScpMoveConfig, ScpOptions, accept,
    propose,
};
use vyna_dcm_net::{AeTitle, Association, AssociationOptions};

fn opts() -> AssociationOptions {
    AssociationOptions {
        read_timeout: Duration::from_secs(10),
        ..Default::default()
    }
}

fn acceptor() -> AcceptorOptions {
    AcceptorOptions {
        association: opts(),
        ..Default::default()
    }
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
        ),
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
    let scp_opts = ScpOptions {
        move_config: Some(ScpMoveConfig {
            directory: Arc::new(HashMapDirectory { map }),
            assoc_options: opts(),
            calling_ae: AeTitle::new("FINDSCP").unwrap(),
        }),
    };
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
