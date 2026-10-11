//! Hyper HTTP/1.1 front end for the DICOMweb [`dispatch`] core.
//!
//! [`dispatch`]: crate::server::dispatch

use std::net::SocketAddr;
use std::sync::Arc;

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response};
use hyper_util::rt::TokioIo;
use tokio::net::{TcpListener, TcpStream};

use crate::server::{DicomWebStore, HttpRequest, dispatch};

/// Cap for STOW request bodies (1 GiB).
pub const MAX_BODY_LEN: usize = 1024 * 1024 * 1024;

/// Serve one TCP connection (HTTP/1.1, keep-alive) to completion.
pub async fn serve_connection<S: DicomWebStore + 'static>(
    store: Arc<S>,
    public_base: String,
    tcp: TcpStream,
) -> Result<(), String> {
    let io = TokioIo::new(tcp);
    http1::Builder::new()
        .serve_connection(
            io,
            service_fn(move |req: Request<hyper::body::Incoming>| {
                let store = store.clone();
                let public_base = public_base.clone();
                async move { handle_one(store.as_ref(), &public_base, req).await }
            }),
        )
        .await
        .map_err(|e| e.to_string())
}

/// Bind and serve forever (each connection in its own task).
pub async fn serve_forever<S: DicomWebStore + 'static>(
    store: Arc<S>,
    public_base: String,
    bind: SocketAddr,
) -> Result<(), String> {
    let listener = TcpListener::bind(bind).await.map_err(|e| e.to_string())?;
    loop {
        let Ok((tcp, _)) = listener.accept().await else {
            continue;
        };
        let (store, public_base) = (store.clone(), public_base.clone());
        tokio::spawn(async move {
            if let Err(e) = serve_connection(store, public_base, tcp).await {
                tracing::warn!("DICOMweb connection error: {e}");
            }
        });
    }
}

async fn handle_one<S: DicomWebStore>(
    store: &S,
    public_base: &str,
    req: Request<hyper::body::Incoming>,
) -> Result<Response<Full<Bytes>>, hyper::Error> {
    let (parts, body) = req.into_parts();
    let raw_body = body.collect().await?.to_bytes();
    if raw_body.len() > MAX_BODY_LEN {
        let mut resp = Response::new(Full::new(Bytes::from_static(b"request body too large")));
        *resp.status_mut() = hyper::StatusCode::PAYLOAD_TOO_LARGE;
        return Ok(resp);
    }
    let path_query = parts
        .uri
        .path_and_query()
        .map(|pq| pq.as_str())
        .unwrap_or("/");
    let (path, query) = match path_query.split_once('?') {
        Some((p, q)) => (p.to_string(), q.to_string()),
        None => (path_query.to_string(), String::new()),
    };
    let mut headers = std::collections::HashMap::new();
    for (name, value) in parts.headers.iter() {
        if let Ok(v) = value.to_str() {
            headers.insert(name.as_str().to_ascii_lowercase(), v.to_string());
        }
    }
    let hreq = HttpRequest {
        method: parts.method.to_string(),
        path,
        query,
        headers,
        body: raw_body.to_vec(),
    };
    let hresp = dispatch(store, public_base, &hreq);
    let mut builder = Response::builder().status(hresp.status);
    for (k, v) in &hresp.headers {
        builder = builder.header(k, v);
    }
    match builder.body(Full::new(Bytes::from(hresp.body))) {
        Ok(resp) => Ok(resp),
        Err(_) => {
            let mut resp = Response::new(Full::new(Bytes::from_static(b"internal error")));
            *resp.status_mut() = hyper::StatusCode::INTERNAL_SERVER_ERROR;
            Ok(resp)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::DicomWebClient;
    use crate::server::{MemoryStore, test_file};

    #[tokio::test]
    async fn end_to_end_client_against_hyper() {
        let store = Arc::new(MemoryStore::new());
        store
            .store_instance(&test_file("1.2.9", "1.2.9.1", "1.2.9.1.1", 4, 4), None)
            .unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let base = format!("http://{addr}");
        let s2 = store.clone();
        let b2 = base.clone();
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

        let client = DicomWebClient::new(&base).unwrap();
        // QIDO.
        let studies = client
            .search_studies(&[("PatientID", "W123")])
            .await
            .unwrap();
        assert_eq!(studies.len(), 1);
        // WADO file + metadata + bulk.
        let file = client
            .wado_file("1.2.9", "1.2.9.1", "1.2.9.1.1")
            .await
            .unwrap();
        assert_eq!(&file[128..132], b"DICM");
        let meta = client
            .wado_metadata("1.2.9", "1.2.9.1", "1.2.9.1.1")
            .await
            .unwrap();
        assert_eq!(
            meta.string(vyna_dcm_core::tags::PATIENT_ID),
            Some("W123"),
            "metadata parses with bulk elements empty"
        );
        assert!(
            meta.get(vyna_dcm_core::tags::PIXEL_DATA)
                .unwrap()
                .value
                .is_empty()
        );
        // The BulkDataURI resolves through the raw metadata form.
        let raw = client
            .wado_metadata_raw("1.2.9", "1.2.9.1", "1.2.9.1.1")
            .await
            .unwrap();
        let uri = raw["7FE00010"]["BulkDataURI"].as_str().unwrap().to_string();
        let (ct, bulk) = client.wado_bulkdata(&uri).await.unwrap();
        assert_eq!(ct, "application/octet-stream");
        assert_eq!(bulk.len(), 4 * 4 * 2);
        // STOW a second instance, then find both.
        let file2 = test_file("1.2.9", "1.2.9.2", "1.2.9.2.1", 2, 2);
        let stow = client.stow(None, &[file2]).await.unwrap();
        assert!(stow.retrieve_url.unwrap().contains("/studies/1.2.9"));
        let all = client
            .search_instances("1.2.9", "1.2.9.2", &[])
            .await
            .unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(store.len(), 2);
    }
}
