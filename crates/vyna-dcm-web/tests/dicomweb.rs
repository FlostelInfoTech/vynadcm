//! Loopback tests: real HTTP between `hyper_server` and `DicomWebClient`
//! over a store seeded from `tests/data`.

#![cfg(feature = "server")]

use std::path::PathBuf;
use std::sync::Arc;
use vyna_dcm_core::tags;
use vyna_dcm_web::Bytes;
use vyna_dcm_web::DicomWebClient;
use vyna_dcm_web::hyper_server::serve_connection;
use vyna_dcm_web::server::{DicomWebStore, MemoryStore};

fn corpus(name: &str) -> Vec<u8> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/data")
        .join(name);
    std::fs::read(path).unwrap()
}

async fn serve(store: Arc<MemoryStore>) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let public = format!("{base}/");
    let handle = tokio::spawn(async move {
        loop {
            let Ok((tcp, _)) = listener.accept().await else {
                break;
            };
            let _ = serve_connection(store.clone(), public.clone(), tcp).await;
        }
    });
    (base, handle)
}

const STUDY: &str = "1.2.826.0.1.3680043.2.1143.3365540476747857567072393009509418480";
const SERIES: &str = "1.2.826.0.1.3680043.2.1143.3712364435022872412969836992152438492";
const INST: &str = "1.2.826.0.1.3680043.2.1143.6455556726214900995651753669640998622";

#[tokio::test]
async fn qido_finds_seeded_instance() {
    let store = Arc::new(MemoryStore::new());
    store
        .store_instance(Bytes::from(corpus("emri_small.dcm")), None)
        .unwrap();
    let (base, server) = serve(store).await;
    let client = DicomWebClient::new(&base).unwrap();
    let matches = client
        .search_studies(&[("StudyInstanceUID", STUDY)])
        .await
        .unwrap();
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].string(tags::STUDY_INSTANCE_UID), Some(STUDY));
    // Empty value = return key: modality echoes.
    let matches = client
        .search_series(STUDY, &[("Modality", "")])
        .await
        .unwrap();
    assert_eq!(matches.len(), 1);
    server.abort();
}

#[tokio::test]
async fn wado_round_trips_file_metadata_frames() {
    let store = Arc::new(MemoryStore::new());
    let file = corpus("emri_small.dcm");
    store
        .store_instance(Bytes::from(file.clone()), None)
        .unwrap();
    let (base, server) = serve(store).await;
    let client = DicomWebClient::new(&base).unwrap();
    // Instance bytes are exact.
    let back = client.wado_file(STUDY, SERIES, INST).await.unwrap();
    assert_eq!(back.as_ref(), file.as_slice());
    // Metadata strips bulk elements (BulkDataURI links parse as empty).
    let meta = client.wado_metadata(STUDY, SERIES, INST).await.unwrap();
    assert!(matches!(
        meta.get(tags::PIXEL_DATA).map(|e| &e.value),
        Some(vyna_dcm_core::Value::Empty)
    ));
    assert_eq!(meta.string(tags::SOP_INSTANCE_UID), Some(INST));
    // Frames decode to rows*columns*bytes-per-sample (64x64 16-bit).
    let frames = client
        .wado_frames(STUDY, SERIES, INST, &[1, 2], "application/octet-stream")
        .await
        .unwrap();
    assert_eq!(frames.len(), 2);
    assert!(frames.iter().all(|f| f.body.len() == 64 * 64 * 2));
    server.abort();
}

#[tokio::test]
async fn stow_lands_and_qido_sees_it() {
    let store = Arc::new(MemoryStore::new());
    let (base, server) = serve(store).await;
    let client = DicomWebClient::new(&base).unwrap();
    let resp = client.stow(None, &[corpus("SC_rgb.dcm")]).await.unwrap();
    assert!(resp.status == 200 || resp.status == 202);
    let matches = client.search_studies(&[]).await.unwrap();
    assert_eq!(matches.len(), 1);
    server.abort();
}
