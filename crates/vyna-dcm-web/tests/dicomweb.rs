//! Loopback tests: real HTTP between `hyper_server` and `DicomWebClient`
//! over a store seeded from `tests/data`.

#![cfg(feature = "server")]

use std::path::PathBuf;
use std::sync::Arc;
use vyna_dcm_core::tags;
use vyna_dcm_web::Bytes;
use vyna_dcm_web::DicomWebClient;
use vyna_dcm_web::Error;
use vyna_dcm_web::hyper_server::{ServerAuth, serve_connection_with_auth};
use vyna_dcm_web::server::{DicomWebStore, MemoryStore};

fn corpus(name: &str) -> Vec<u8> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/data")
        .join(name);
    std::fs::read(path).unwrap()
}

async fn serve(store: Arc<MemoryStore>) -> (String, tokio::task::JoinHandle<()>) {
    serve_inner(store, ServerAuth::default()).await
}

async fn serve_inner(
    store: Arc<MemoryStore>,
    auth: ServerAuth,
) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let public = format!("{base}/");
    let handle = tokio::spawn(async move {
        loop {
            let Ok((tcp, _)) = listener.accept().await else {
                break;
            };
            // One task per connection (like `serve_forever`): HTTP
            // clients pool keep-alive connections, so awaiting each
            // connection inline would stall every later client until
            // the pool idles out.
            let (store, public, auth) = (store.clone(), public.clone(), auth.clone());
            tokio::spawn(async move {
                let _ = serve_connection_with_auth(store, public, tcp, auth).await;
            });
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

#[tokio::test]
async fn authenticated_client_round_trip() {
    let store = Arc::new(MemoryStore::new());
    store
        .store_instance(Bytes::from(corpus("emri_small.dcm")), None)
        .unwrap();
    let (base, server) = serve_inner(
        store,
        ServerAuth {
            bearer_token: Some("test-bearer-token".to_string()),
        },
    )
    .await;
    // Bearer client: QIDO + WADO work end to end.
    let authed = DicomWebClient::new(&base)
        .unwrap()
        .with_bearer("test-bearer-token");
    let matches = authed
        .search_studies(&[("StudyInstanceUID", STUDY)])
        .await
        .unwrap();
    assert_eq!(matches.len(), 1);
    let file = authed.wado_file(STUDY, SERIES, INST).await.unwrap();
    assert_eq!(&file[128..132], b"DICM");
    // Bare client: 401 surfaces as a typed status, not a hang or 200.
    let bare = DicomWebClient::new(&base).unwrap();
    match bare.search_studies(&[]).await {
        Err(Error::Status { status, .. }) => assert_eq!(status, 401),
        other => panic!("expected 401 status, got {other:?}"),
    }
    // Wrong token: 401 too.
    let wrong = DicomWebClient::new(&base)
        .unwrap()
        .with_bearer("wrong-token");
    match wrong.search_studies(&[]).await {
        Err(Error::Status { status, .. }) => assert_eq!(status, 401),
        other => panic!("expected 401 status, got {other:?}"),
    }
    server.abort();
}

#[tokio::test]
async fn wado_study_and_series_retrieve_over_wire() {
    let store = Arc::new(MemoryStore::new());
    let file = corpus("emri_small.dcm");
    store
        .store_instance(Bytes::from(file.clone()), None)
        .unwrap();
    let (base, server) = serve(store).await;
    let client = DicomWebClient::new(&base).unwrap();
    // Whole-study retrieve: one application/dicom part per instance.
    let study_parts = client.wado_study(STUDY).await.unwrap();
    assert_eq!(study_parts.len(), 1);
    assert!(
        study_parts[0].content_type.contains("application/dicom"),
        "{}",
        study_parts[0].content_type
    );
    assert_eq!(study_parts[0].body.as_ref(), file.as_slice());
    // Whole-series retrieve narrows to the series.
    let series_parts = client.wado_series(STUDY, SERIES).await.unwrap();
    assert_eq!(series_parts.len(), 1);
    assert_eq!(series_parts[0].body.as_ref(), file.as_slice());
    // Unknown study stays a typed 404.
    match client.wado_study("9.9.9").await {
        Err(Error::Status { status, .. }) => assert_eq!(status, 404),
        other => panic!("expected 404 status, got {other:?}"),
    }
    server.abort();
}

#[tokio::test]
async fn stow_partial_failure_surfaces_409_to_client() {
    let store = Arc::new(MemoryStore::new());
    let (base, server) = serve(store).await;
    let client = DicomWebClient::new(&base).unwrap();
    // One good instance plus one unparseable part: the client sees
    // the 409 with its FailedSOPSequence detail intact.
    let err = client
        .stow(None, &[corpus("SC_rgb.dcm"), b"not a dicom file".to_vec()])
        .await
        .unwrap_err();
    match err {
        Error::Status { status, body } => {
            assert_eq!(status, 409);
            assert!(body.contains("00081198"), "{body}");
        }
        other => panic!("expected 409 status, got {other:?}"),
    }
    // The good instance still landed.
    assert_eq!(client.search_studies(&[]).await.unwrap().len(), 1);
    server.abort();
}

#[tokio::test]
async fn cross_origin_bulkdata_strips_auth() {
    use std::sync::Mutex;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let store = Arc::new(MemoryStore::new());
    store
        .store_instance(Bytes::from(corpus("emri_small.dcm")), None)
        .unwrap();
    let (base, server) = serve_inner(
        store,
        ServerAuth {
            bearer_token: Some("bulk-bearer-token".to_string()),
        },
    )
    .await;
    let client = DicomWebClient::new(&base)
        .unwrap()
        .with_bearer("bulk-bearer-token");

    // Same-origin bulk fetch keeps auth (the gated server answers).
    let raw = client.wado_metadata_raw(STUDY, SERIES, INST).await.unwrap();
    let uri = raw["7FE00010"]["BulkDataURI"].as_str().unwrap().to_string();
    assert!(uri.starts_with(&base), "{uri}");
    let (ct, bulk) = client.wado_bulkdata(&uri).await.unwrap();
    assert_eq!(ct, "application/octet-stream");
    // All frames joined (emri_small: ten 64x64 16-bit frames).
    assert_eq!(bulk.len(), 64 * 64 * 2 * 10);

    // Cross-origin stub: records any Authorization it sees.
    let seen: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let evil_base = format!("http://{}", listener.local_addr().unwrap());
    let seen2 = seen.clone();
    tokio::spawn(async move {
        let (mut tcp, _) = listener.accept().await.unwrap();
        let mut head = vec![0u8; 65536];
        let mut have = 0usize;
        loop {
            let n = tcp.read(&mut head[have..]).await.unwrap();
            have += n;
            if String::from_utf8_lossy(&head[..have]).contains("\r\n\r\n") {
                break;
            }
        }
        let auth = String::from_utf8_lossy(&head[..have])
            .split("\r\n")
            .find_map(|l| {
                l.split_once(':').and_then(|(k, v)| {
                    k.trim()
                        .eq_ignore_ascii_case("authorization")
                        .then(|| v.trim().to_string())
                })
            });
        *seen2.lock().unwrap() = auth;
        let body = b"CROSS-ORIGIN-BYTES";
        let resp = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/octet-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
            body.len()
        );
        tcp.write_all(resp.as_bytes()).await.unwrap();
        tcp.write_all(body).await.unwrap();
    });
    let (ct, body) = client
        .wado_bulkdata(&format!("{evil_base}/bulk/x"))
        .await
        .unwrap();
    assert_eq!(ct, "application/octet-stream");
    assert_eq!(body, b"CROSS-ORIGIN-BYTES");
    // The foreign origin never saw our credentials.
    assert_eq!(*seen.lock().unwrap(), None);
    server.abort();
}
