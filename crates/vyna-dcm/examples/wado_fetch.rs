//! DICOMweb pattern: serve a study from `MemoryStore`, then STOW / QIDO /
//! WADO it with the client — fully self-contained.
//!
//! Run: `cargo run -p vyna-dcm --example wado_fetch`

use std::sync::Arc;

use tokio::net::TcpListener;
use vyna_dcm::web::client::DicomWebClient;
use vyna_dcm::web::hyper_server::serve_connection;
use vyna_dcm::web::server::{MemoryStore, test_file};

#[tokio::main]
async fn main() -> Result<(), String> {
    let store = Arc::new(MemoryStore::new());
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|e| e.to_string())?;
    let base = format!(
        "http://{}",
        listener.local_addr().map_err(|e| e.to_string())?
    );
    let (s2, b2) = (store.clone(), base.clone());
    tokio::spawn(async move {
        loop {
            let Ok((tcp, _)) = listener.accept().await else {
                break;
            };
            let (s, b) = (s2.clone(), b2.clone());
            tokio::spawn(async move {
                serve_connection(s, b, tcp).await.ok();
            });
        }
    });

    let client = DicomWebClient::new(&base).map_err(|e| e.to_string())?;
    // STOW one 8x8 CT slice.
    let file = test_file("1.9.9", "1.9.9.1", "1.9.9.1.1", 8, 8);
    let stow = client
        .stow(None, &[file])
        .await
        .map_err(|e| e.to_string())?;
    println!("STOW: {}", stow.retrieve_url.unwrap_or_default());
    // QIDO it back.
    let studies = client
        .search_studies(&[("PatientID", "W123")])
        .await
        .map_err(|e| e.to_string())?;
    println!("QIDO: {} studie(s)", studies.len());
    // WADO metadata + bulk pixels.
    let raw = client
        .wado_metadata_raw("1.9.9", "1.9.9.1", "1.9.9.1.1")
        .await
        .map_err(|e| e.to_string())?;
    let uri = raw["7FE00010"]["BulkDataURI"].as_str().unwrap().to_string();
    let (_, bulk) = client
        .wado_bulkdata(&uri)
        .await
        .map_err(|e| e.to_string())?;
    println!("WADO bulk pixels: {} bytes", bulk.len());
    assert_eq!(bulk.len(), 8 * 8 * 2);
    println!("DICOMweb round-trip OK");
    Ok(())
}
