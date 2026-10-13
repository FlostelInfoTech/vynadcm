//! DICOMweb pattern: serve a study from `MemoryStore`, then STOW / QIDO /
//! WADO it with the client — fully self-contained.
//!
//! Run: `cargo run -p vyna-dcm --example wado_fetch`

use std::sync::Arc;

use vyna_dcm::web::client::DicomWebClient;
use vyna_dcm::web::hyper_server::serve_forever;
use vyna_dcm::web::server::{MemoryStore, test_file};

#[tokio::main]
async fn main() -> Result<(), String> {
    let store = Arc::new(MemoryStore::new());
    // Grab a free loopback port, then let `serve_forever` own the
    // listener (its accept loop, connection cap, and error backoff).
    let probe = std::net::TcpListener::bind("127.0.0.1:0").map_err(|e| e.to_string())?;
    let bind = probe.local_addr().map_err(|e| e.to_string())?;
    drop(probe);
    let base = format!("http://{bind}");
    let (s, b) = (store.clone(), base.clone());
    tokio::spawn(async move {
        serve_forever(s, b, bind).await.ok();
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
