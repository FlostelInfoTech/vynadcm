//! Hyper HTTP/1.1 front end for the DICOMweb [`dispatch`] core.
//!
//! [`dispatch`]: crate::server::dispatch

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use bytes::{Bytes, BytesMut};
use http_body_util::{BodyExt, Full};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response};
use hyper_util::rt::TokioIo;
use tokio::net::{TcpListener, TcpStream};

use crate::server::{DicomWebStore, HttpRequest, dispatch};
use crate::{Error, Result};

/// Cap for STOW request bodies (1 GiB).
pub const MAX_BODY_LEN: usize = 1024 * 1024 * 1024;

/// Maximum lifetime of one HTTP connection (slowloris guard): a peer
/// that trickles bytes forever is cut off instead of hoarding a task.
pub const MAX_CONN_SECS: u64 = 300;

/// Maximum concurrent connections per [`serve_forever`] listener;
/// connections past this are dropped at accept.
pub const MAX_CONNECTIONS: usize = 1024;

/// Optional access control for the plain-HTTP front end.
///
/// WARNING: this server speaks unencrypted HTTP and, with default
/// settings, accepts every request without authentication. DICOM
/// payloads routinely contain PHI: bind to loopback only, or put a
/// TLS-terminating reverse proxy (with its own auth) in front, and
/// consider setting [`ServerAuth::bearer_token`].
#[derive(Clone, Default)]
pub struct ServerAuth {
    /// When set, every request must carry
    /// `Authorization: Bearer <token>` or gets `401 Unauthorized`.
    pub bearer_token: Option<String>,
}

// Manual `Debug`: the token must never reach logs.
impl std::fmt::Debug for ServerAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServerAuth")
            .field(
                "bearer_token",
                &self.bearer_token.as_ref().map(|_| "<redacted>"),
            )
            .finish()
    }
}

/// Whether `headers` (lowercased names) satisfy `auth`.
fn authorized(headers: &std::collections::HashMap<String, String>, auth: &ServerAuth) -> bool {
    let Some(expected) = auth.bearer_token.as_deref() else {
        return true;
    };
    let Some(got) = headers.get("authorization") else {
        return false;
    };
    let Some(token) = got.strip_prefix("Bearer ") else {
        return false;
    };
    token == expected
}

/// Serve one TCP connection (HTTP/1.1, keep-alive) to completion.
///
/// See [`ServerAuth`] for the plaintext/no-auth warning; use
/// [`serve_connection_with_auth`] to require a bearer token.
pub async fn serve_connection<S: DicomWebStore + 'static>(
    store: Arc<S>,
    public_base: String,
    tcp: TcpStream,
) -> Result<()> {
    serve_connection_with_auth(store, public_base, tcp, ServerAuth::default()).await
}

/// [`serve_connection`] with access control (see [`ServerAuth`]).
pub async fn serve_connection_with_auth<S: DicomWebStore + 'static>(
    store: Arc<S>,
    public_base: String,
    tcp: TcpStream,
    auth: ServerAuth,
) -> Result<()> {
    serve_with_deadline(
        store,
        public_base,
        tcp,
        auth,
        Duration::from_secs(MAX_CONN_SECS),
    )
    .await
}

/// [`serve_connection_with_auth`] with an explicit deadline (the
/// public entry points use [`MAX_CONN_SECS`]).
async fn serve_with_deadline<S: DicomWebStore + 'static>(
    store: Arc<S>,
    public_base: String,
    tcp: TcpStream,
    auth: ServerAuth,
    deadline: Duration,
) -> Result<()> {
    let io = TokioIo::new(tcp);
    tokio::time::timeout(
        deadline,
        http1::Builder::new().serve_connection(
            io,
            service_fn(move |req: Request<hyper::body::Incoming>| {
                let store = store.clone();
                let public_base = public_base.clone();
                let auth = auth.clone();
                async move { handle_one(store.as_ref(), &public_base, req, &auth).await }
            }),
        ),
    )
    .await
    .map_err(|_| Error::Http("connection timed out".into()))?
    .map_err(|e| Error::Http(e.to_string()))
}

/// Bind and serve forever (each connection in its own task).
///
/// See [`ServerAuth`] for the plaintext/no-auth warning; use
/// [`serve_forever_with_auth`] to require a bearer token.
pub async fn serve_forever<S: DicomWebStore + 'static>(
    store: Arc<S>,
    public_base: String,
    bind: SocketAddr,
) -> Result<()> {
    serve_forever_with_auth(store, public_base, bind, ServerAuth::default()).await
}

/// [`serve_forever`] with access control (see [`ServerAuth`]).
pub async fn serve_forever_with_auth<S: DicomWebStore + 'static>(
    store: Arc<S>,
    public_base: String,
    bind: SocketAddr,
    auth: ServerAuth,
) -> Result<()> {
    let listener = TcpListener::bind(bind)
        .await
        .map_err(|e| Error::Http(e.to_string()))?;
    let slots = Arc::new(tokio::sync::Semaphore::new(MAX_CONNECTIONS));
    loop {
        let Ok((tcp, _)) = listener.accept().await else {
            continue;
        };
        let Ok(permit) = slots.clone().try_acquire_owned() else {
            tracing::warn!("DICOMweb connection dropped: over {MAX_CONNECTIONS} concurrent");
            continue;
        };
        let (store, public_base, auth) = (store.clone(), public_base.clone(), auth.clone());
        tokio::spawn(async move {
            let _permit = permit;
            if let Err(e) = serve_connection_with_auth(store, public_base, tcp, auth).await {
                tracing::warn!("DICOMweb connection error: {e}");
            }
        });
    }
}

/// Whether appending `next` bytes to `accumulated` would breach
/// [`MAX_BODY_LEN`]; saturating so adversarial lengths can't wrap.
fn body_over_cap(accumulated: usize, next: usize) -> bool {
    accumulated.saturating_add(next) > MAX_BODY_LEN
}

async fn handle_one<S: DicomWebStore>(
    store: &S,
    public_base: &str,
    req: Request<hyper::body::Incoming>,
    auth: &ServerAuth,
) -> std::result::Result<Response<Full<Bytes>>, hyper::Error> {
    let (parts, body) = req.into_parts();
    let mut headers = std::collections::HashMap::new();
    for (name, value) in parts.headers.iter() {
        if let Ok(v) = value.to_str() {
            headers.insert(name.as_str().to_ascii_lowercase(), v.to_string());
        }
    }
    if !authorized(&headers, auth) {
        let mut resp = Response::new(Full::new(Bytes::from_static(b"unauthorized")));
        *resp.status_mut() = hyper::StatusCode::UNAUTHORIZED;
        resp.headers_mut().insert(
            hyper::header::WWW_AUTHENTICATE,
            hyper::header::HeaderValue::from_static("Bearer"),
        );
        return Ok(resp);
    }
    // Stream the body with an early cap: collecting first and
    // checking after would let a huge STOW eat memory before the
    // limit ever applied. BytesMut freezes into the request body
    // without a further copy.
    let mut raw_body = BytesMut::new();
    let mut body = body;
    while let Some(frame) = body.frame().await {
        let frame = frame?;
        if let Some(data) = frame.data_ref() {
            if body_over_cap(raw_body.len(), data.len()) {
                let mut resp =
                    Response::new(Full::new(Bytes::from_static(b"request body too large")));
                *resp.status_mut() = hyper::StatusCode::PAYLOAD_TOO_LARGE;
                return Ok(resp);
            }
            raw_body.extend_from_slice(data);
        }
    }
    let raw_body = raw_body.freeze();
    let path_query = parts
        .uri
        .path_and_query()
        .map(|pq| pq.as_str())
        .unwrap_or("/");
    let (path, query) = match path_query.split_once('?') {
        Some((p, q)) => (p.to_string(), q.to_string()),
        None => (path_query.to_string(), String::new()),
    };
    let hreq = HttpRequest {
        method: parts.method.to_string(),
        path,
        query,
        headers,
        body: raw_body,
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

    #[test]
    fn bearer_token_gates_requests() {
        use std::collections::HashMap;
        let open = ServerAuth::default();
        assert!(authorized(&HashMap::new(), &open));
        let gated = ServerAuth {
            bearer_token: Some("s3cret".to_string()),
        };
        assert!(!authorized(&HashMap::new(), &gated));
        let mut wrong = HashMap::new();
        wrong.insert("authorization".to_string(), "Bearer nope".to_string());
        assert!(!authorized(&wrong, &gated));
        let mut basic = HashMap::new();
        basic.insert("authorization".to_string(), "Basic c2NyZXQ=".to_string());
        assert!(!authorized(&basic, &gated));
        let mut good = HashMap::new();
        good.insert("authorization".to_string(), "Bearer s3cret".to_string());
        assert!(authorized(&good, &gated));
    }

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

    /// Minimal percent-decoder for asserting query values.
    fn pct_decode(s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        let mut chars = s.chars();
        while let Some(c) = chars.next() {
            if c == '%' {
                let hi = chars.next().unwrap_or('0');
                let lo = chars.next().unwrap_or('0');
                let b = u8::from_str_radix(&format!("{hi}{lo}"), 16).unwrap_or(b'?');
                out.push(b as char);
            } else if c == '+' {
                out.push(' ');
            } else {
                out.push(c);
            }
        }
        out
    }

    #[tokio::test]
    async fn wado_uri_against_stub() {
        use crate::client::WadoUriRequest;
        use std::sync::Mutex;

        let dicom = test_file("1.2.9", "1.2.9.1", "1.2.9.1.1", 4, 4);
        let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let seen2 = seen.clone();
        tokio::spawn(async move {
            loop {
                let Ok((tcp, _)) = listener.accept().await else {
                    break;
                };
                let (seen, dicom) = (seen2.clone(), dicom.clone());
                tokio::spawn(async move {
                    let io = TokioIo::new(tcp);
                    let svc = service_fn(move |req: Request<hyper::body::Incoming>| {
                        let (seen, dicom) = (seen.clone(), dicom.clone());
                        async move {
                            let query = req.uri().query().map(pct_decode).unwrap_or_default();
                            seen.lock().unwrap().push(query.clone());
                            let get = |k: &str| {
                                query.split('&').find_map(|p| {
                                    p.split_once('=')
                                        .filter(|(kk, _)| *kk == k)
                                        .map(|(_, v)| v.to_string())
                                })
                            };
                            let (status, ct, body): (u16, &str, Vec<u8>) =
                                if get("requestType").as_deref() != Some("WADO") {
                                    (400, "text/plain", b"want requestType=WADO".to_vec())
                                } else if get("objectUID").as_deref() == Some("1.9.9.9") {
                                    (404, "text/plain", b"no such object".to_vec())
                                } else if get("contentType").as_deref() == Some("image/jpeg") {
                                    (200, "image/jpeg", b"fake-jpeg-bytes".to_vec())
                                } else {
                                    (200, "application/dicom", dicom)
                                };
                            let mut resp = Response::new(Full::new(Bytes::from(body)));
                            *resp.status_mut() = hyper::StatusCode::from_u16(status).unwrap();
                            resp.headers_mut().insert(
                                hyper::header::CONTENT_TYPE,
                                hyper::header::HeaderValue::from_str(ct).unwrap(),
                            );
                            Ok::<_, hyper::Error>(resp)
                        }
                    });
                    http1::Builder::new().serve_connection(io, svc).await.ok();
                });
            }
        });

        let client = DicomWebClient::new(format!("http://{addr}/wado")).unwrap();
        // DICOM retrieve parses into a dataset.
        let req = WadoUriRequest {
            content_type: Some("application/dicom".into()),
            transfer_syntax: Some("1.2.840.10008.1.2.1".into()),
            anonymize: true,
            ..WadoUriRequest::new("1.2.9", "1.2.9.1", "1.2.9.1.1")
        };
        let (meta, ds) = client.wado_uri_dataset(&req).await.unwrap();
        assert_eq!(ds.string(vyna_dcm_core::tags::PATIENT_ID), Some("W123"));
        assert_eq!(meta.transfer_syntax_uid, "1.2.840.10008.1.2.1");
        // The stub saw the full query.
        {
            let queries = seen.lock().unwrap();
            assert_eq!(queries.len(), 1);
            for want in [
                "requestType=WADO",
                "studyUID=1.2.9",
                "seriesUID=1.2.9.1",
                "objectUID=1.2.9.1.1",
                "contentType=application/dicom",
                "transferSyntax=1.2.840.10008.1.2.1",
                "anonymize=yes",
            ] {
                assert!(queries[0].contains(want), "{}", queries[0]);
            }
        }
        // Rendered retrieve passes content through untouched.
        let req = WadoUriRequest {
            content_type: Some("image/jpeg".into()),
            rows: Some(64),
            frame_number: Some(1),
            ..WadoUriRequest::new("1.2.9", "1.2.9.1", "1.2.9.1.1")
        };
        let (ct, body) = client.wado_uri(&req).await.unwrap();
        assert_eq!(ct, "image/jpeg");
        assert_eq!(body, b"fake-jpeg-bytes");
        assert!(
            seen.lock().unwrap()[1].contains("frameNumber=1"),
            "frame param sent"
        );
        // Rendered bytes are not a dataset.
        assert!(client.wado_uri_dataset(&req).await.is_err());
        // Unknown object surfaces the status.
        let missing = WadoUriRequest::new("1.2.9", "1.2.9.1", "1.9.9.9");
        match client.wado_uri(&missing).await {
            Err(crate::Error::Status { status, .. }) => assert_eq!(status, 404),
            other => panic!("expected 404 status, got {other:?}"),
        }
    }

    #[test]
    fn debug_redacts_bearer_token() {
        let auth = ServerAuth {
            bearer_token: Some("s3cret-token".into()),
        };
        let shown = format!("{auth:?}");
        assert!(!shown.contains("s3cret"), "{shown}");
    }

    #[tokio::test]
    async fn idle_connection_times_out() {
        use tokio::io::AsyncWriteExt;
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let mut client = tokio::net::TcpStream::connect(addr).await.unwrap();
        // Trickle one line, then go silent (slowloris): the server
        // must cut the connection instead of waiting forever.
        client
            .write_all(b"GET /studies HTTP/1.1\r\n")
            .await
            .unwrap();
        let (tcp, _) = listener.accept().await.unwrap();
        let store = Arc::new(MemoryStore::new());
        let err = serve_with_deadline(
            store,
            "http://x".to_string(),
            tcp,
            ServerAuth::default(),
            Duration::from_millis(50),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, Error::Http(_)), "{err}");
        assert!(err.to_string().contains("timed out"), "{err}");
    }

    #[tokio::test]
    async fn bind_conflict_returns_http_error() {
        let held = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = held.local_addr().unwrap();
        let store = Arc::new(MemoryStore::new());
        let err = serve_forever(store, format!("http://{addr}"), addr)
            .await
            .unwrap_err();
        assert!(matches!(err, Error::Http(_)), "{err}");
    }

    #[test]
    fn body_cap_boundary() {
        assert!(!body_over_cap(0, 0));
        assert!(!body_over_cap(MAX_BODY_LEN - 1, 1));
        assert!(!body_over_cap(MAX_BODY_LEN, 0));
        assert!(body_over_cap(MAX_BODY_LEN, 1));
        assert!(body_over_cap(usize::MAX, usize::MAX));
    }
}
