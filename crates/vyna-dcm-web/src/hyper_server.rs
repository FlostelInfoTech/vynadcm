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

/// Default aggregate cap for in-flight request *and* response
/// bodies across one [`serve_forever`] listener (4 GiB): the
/// per-connection [`MAX_BODY_LEN`] alone still allows 1024 x 1 GiB
/// of buffered uploads — and unbounded concurrent RetrieveStudy
/// responses on the way out. Past it, new bytes fail fast with `503`
/// instead of OOMing the server; embedders serving from tight boxes
/// can share a smaller [`InflightBudget`] via
/// [`serve_connection_with_budget`].
pub const MAX_INFLIGHT_BODY_BYTES: u64 = 4 * 1024 * 1024 * 1024;

/// Shared budget for in-flight request and response bodies: one of
/// these per listener (see [`MAX_INFLIGHT_BODY_BYTES`]), cloned into
/// every connection task. Request reservations release when the
/// upload finishes; response reservations release when the body
/// leaves the socket (see [`CappedBody`).
#[derive(Debug)]
pub struct InflightBudget {
    used: std::sync::Mutex<u64>,
    cap: u64,
}

impl InflightBudget {
    /// Budget with an explicit byte cap.
    #[must_use]
    pub fn new(cap: u64) -> Self {
        Self {
            used: std::sync::Mutex::new(0),
            cap,
        }
    }

    /// Reserve `n` more bytes; false (nothing reserved) past the cap.
    fn try_reserve(&self, n: u64) -> bool {
        let mut used = self.used.lock().unwrap_or_else(|e| e.into_inner());
        if used.saturating_add(n) > self.cap {
            return false;
        }
        *used = used.saturating_add(n);
        true
    }

    /// Release a previous reservation.
    fn release(&self, n: u64) {
        let mut used = self.used.lock().unwrap_or_else(|e| e.into_inner());
        *used = used.saturating_sub(n);
    }
}

impl Default for InflightBudget {
    /// [`MAX_INFLIGHT_BODY_BYTES`] shared budget.
    fn default() -> Self {
        Self::new(MAX_INFLIGHT_BODY_BYTES)
    }
}

/// Reserved in-flight body bytes; released back to the budget on drop.
struct InflightGuard {
    budget: Arc<InflightBudget>,
    bytes: u64,
}

impl InflightGuard {
    /// Reserve `n` more bytes against the shared budget.
    fn reserve(&mut self, n: u64) -> bool {
        if self.budget.try_reserve(n) {
            self.bytes = self.bytes.saturating_add(n);
            true
        } else {
            false
        }
    }
}

impl Drop for InflightGuard {
    fn drop(&mut self) {
        self.budget.release(self.bytes);
    }
}

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
    // Auth schemes are case-insensitive (RFC 9110 §11.4); the
    // token itself still compares in constant time.
    let Some((scheme, token)) = got.split_once(' ') else {
        return false;
    };
    if !scheme.eq_ignore_ascii_case("bearer") {
        return false;
    }
    constant_time_eq(token, expected)
}

/// Fixed-time string equality: every byte pair is always compared
/// (length still leaks, which is fine for fixed-shape tokens).
fn constant_time_eq(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.bytes()
        .zip(b.bytes())
        .fold(0u8, |acc, (x, y)| acc | (x ^ y))
        == 0
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
/// Uses a fresh default [`InflightBudget`]: right for a lone
/// connection (the per-connection cap already bounds it); listeners
/// sharing one budget across connections want
/// [`serve_connection_with_budget`].
pub async fn serve_connection_with_auth<S: DicomWebStore + 'static>(
    store: Arc<S>,
    public_base: String,
    tcp: TcpStream,
    auth: ServerAuth,
) -> Result<()> {
    serve_connection_with_budget(
        store,
        public_base,
        tcp,
        auth,
        Arc::new(InflightBudget::default()),
    )
    .await
}

/// [`serve_connection_with_auth`] over a caller-shared [`InflightBudget`]
/// (used by [`serve_forever`] to cap aggregate in-flight bodies).
pub async fn serve_connection_with_budget<S: DicomWebStore + 'static>(
    store: Arc<S>,
    public_base: String,
    tcp: TcpStream,
    auth: ServerAuth,
    budget: Arc<InflightBudget>,
) -> Result<()> {
    serve_with_deadline(
        store,
        public_base,
        tcp,
        auth,
        budget,
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
    budget: Arc<InflightBudget>,
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
                let budget = budget.clone();
                async move { handle_one(store, public_base, req, auth, budget).await }
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
    let budget = Arc::new(InflightBudget::default());
    loop {
        let Ok((tcp, _)) = listener.accept().await else {
            // Back off: a persistent accept failure (e.g. EMFILE)
            // must not spin the loop at full speed.
            tokio::time::sleep(Duration::from_millis(100)).await;
            continue;
        };
        let Ok(permit) = slots.clone().try_acquire_owned() else {
            tracing::warn!("DICOMweb connection dropped: over {MAX_CONNECTIONS} concurrent");
            continue;
        };
        let (store, public_base, auth, budget) = (
            store.clone(),
            public_base.clone(),
            auth.clone(),
            budget.clone(),
        );
        tokio::spawn(async move {
            let _permit = permit;
            if let Err(e) =
                serve_connection_with_budget(store, public_base, tcp, auth, budget).await
            {
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

/// Whether this method+path is a body-accepting route (STOW): the
/// front end buffers the up-to-1 GiB body only for these. Every other
/// route answers without reading the body, so a huge upload to an
/// unknown route (or a GET with a junk body) never buffers. Uses the
/// same prefix strip as [`dispatch`](crate::server::dispatch) so the
/// pre-check can never disagree with the router about STOW paths.
fn route_expects_body(method: &hyper::Method, path: &str, public_base: &str) -> bool {
    if *method != hyper::Method::POST {
        return false;
    }
    let stripped = crate::server::strip_base_prefix(public_base, path);
    let mut segs = stripped.split('/').filter(|s| !s.is_empty());
    matches!(
        (segs.next(), segs.next(), segs.next()),
        (Some("studies"), None, _) | (Some("studies"), Some(_), None)
    )
}

/// Percent-decode one path segment, symmetric with
/// [`DicomWebClient::path_segment`](crate::DicomWebClient::path_segment):
/// `%XX` unescapes, `+` stays a plus (plus-as-space is query-only),
/// malformed `%` stays literal.
fn decode_path_segment(seg: &str) -> String {
    let mut out = Vec::with_capacity(seg.len());
    let b = seg.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len()
            && let (Some(h), Some(l)) = (hex_val(b[i + 1]), hex_val(b[i + 2]))
        {
            out.push(h << 4 | l);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Decode a request path segment by segment (never whole-string: a
/// decoded `%2F` must fail closed as a separator at routing, not
/// smuggle into a segment).
fn decode_request_path(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    let mut first = true;
    for seg in path.split('/') {
        if !first {
            out.push('/');
        }
        first = false;
        out.push_str(&decode_path_segment(seg));
    }
    out
}

/// Response body holding its [`InflightBudget`] reservation until the
/// bytes leave the socket: the guard drops (releasing the budget)
/// only when hyper finishes sending and drops the body. This is the
/// response-side analog of the request-body budget — same budget,
/// both directions.
struct CappedBody {
    inner: Full<Bytes>,
    _guard: Option<InflightGuard>,
}

impl http_body::Body for CappedBody {
    type Data = Bytes;
    type Error = std::convert::Infallible;

    fn poll_frame(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<std::result::Result<http_body::Frame<Self::Data>, Self::Error>>>
    {
        std::pin::Pin::new(&mut self.get_mut().inner).poll_frame(cx)
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> http_body::SizeHint {
        self.inner.size_hint()
    }
}

/// Build the wire response, reserving `body` against the shared
/// budget first: past the cap the request fails fast with 503
/// instead of buffering unbounded aggregate response bytes.
fn capped_response(
    budget: &Arc<InflightBudget>,
    status: u16,
    headers: &[(String, String)],
    body: Bytes,
) -> std::result::Result<Response<CappedBody>, hyper::Error> {
    let plain = |status: hyper::StatusCode, body: &'static [u8]| {
        let mut resp = Response::new(CappedBody {
            inner: Full::new(Bytes::from_static(body)),
            _guard: None,
        });
        *resp.status_mut() = status;
        resp
    };
    let mut guard = InflightGuard {
        budget: budget.clone(),
        bytes: 0,
    };
    if !guard.reserve(body.len() as u64) {
        return Ok(plain(
            hyper::StatusCode::SERVICE_UNAVAILABLE,
            b"server busy: too many large responses in flight",
        ));
    }
    let mut builder = Response::builder().status(status);
    for (k, v) in headers {
        builder = builder.header(k, v);
    }
    match builder.body(CappedBody {
        inner: Full::new(body),
        _guard: Some(guard),
    }) {
        Ok(resp) => Ok(resp),
        Err(_) => Ok(plain(
            hyper::StatusCode::INTERNAL_SERVER_ERROR,
            b"internal error",
        )),
    }
}

async fn handle_one<S: DicomWebStore + 'static>(
    store: Arc<S>,
    public_base: String,
    req: Request<hyper::body::Incoming>,
    auth: ServerAuth,
    budget: Arc<InflightBudget>,
) -> std::result::Result<Response<CappedBody>, hyper::Error> {
    let (parts, body) = req.into_parts();
    let mut headers = std::collections::HashMap::new();
    for (name, value) in parts.headers.iter() {
        if let Ok(v) = value.to_str() {
            headers.insert(name.as_str().to_ascii_lowercase(), v.to_string());
        }
    }
    if !authorized(&headers, &auth) {
        return capped_response(
            &budget,
            401,
            &[("www-authenticate".to_string(), "Bearer".to_string())],
            Bytes::from_static(b"unauthorized"),
        );
    }
    let path_query = parts
        .uri
        .path_and_query()
        .map(|pq| pq.as_str())
        .unwrap_or("/");
    let (raw_path, query) = match path_query.split_once('?') {
        Some((p, q)) => (p, q.to_string()),
        None => (path_query, String::new()),
    };
    let path = decode_request_path(raw_path);
    // Route before buffering: only STOW accepts a body, so only STOW
    // routes pay for one. Anything else answers without reading the
    // body at all (hyper closes the connection after the response
    // when a junk body goes unread — correct, just no keep-alive).
    let mut raw_body = BytesMut::new();
    let mut guard = InflightGuard {
        budget: budget.clone(),
        bytes: 0,
    };
    if route_expects_body(&parts.method, &path, &public_base) {
        // Stream the body with an early cap: collecting first and
        // checking after would let a huge STOW eat memory before the
        // limit ever applied. BytesMut freezes into the request body
        // without a further copy. Each chunk also reserves against
        // the listener-wide in-flight budget; past it the upload
        // fails fast with 503.
        let mut body = body;
        while let Some(frame) = body.frame().await {
            let frame = frame?;
            if let Some(data) = frame.data_ref() {
                if body_over_cap(raw_body.len(), data.len()) {
                    return capped_response(
                        &budget,
                        413,
                        &[("content-type".to_string(), "text/plain".to_string())],
                        Bytes::from_static(b"request body too large"),
                    );
                }
                if !guard.reserve(data.len() as u64) {
                    return capped_response(
                        &budget,
                        503,
                        &[("content-type".to_string(), "text/plain".to_string())],
                        Bytes::from_static(b"server busy: too many large uploads in flight"),
                    );
                }
                raw_body.extend_from_slice(data);
            }
        }
    }
    let raw_body = raw_body.freeze();
    let hreq = HttpRequest {
        method: parts.method.to_string(),
        path,
        query,
        headers,
        body: raw_body,
    };
    // Dispatch off the tokio worker: STOW multipart scans over up to
    // 1 GiB are CPU-bound and must not stall the event loop (the
    // request body moves without copying — `Bytes`, not `Vec<u8>`).
    let hresp = tokio::task::spawn_blocking(move || dispatch(store.as_ref(), &public_base, &hreq))
        .await
        .unwrap_or_else(|_| crate::server::HttpResponse::text(500, "dispatch panicked"));
    // Release the request bytes before reserving the response: the
    // two never double-count against the shared budget.
    drop(guard);
    capped_response(&budget, hresp.status, &hresp.headers, hresp.body)
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
        // Fixed-time comparison semantics.
        assert!(constant_time_eq("abc123", "abc123"));
        assert!(!constant_time_eq("abc123", "abc124"));
        assert!(!constant_time_eq("abc123", "abc123!"));
        assert!(!constant_time_eq("abc123", "short"));
        let mut good = HashMap::new();
        good.insert("authorization".to_string(), "Bearer s3cret".to_string());
        assert!(authorized(&good, &gated));
        // Scheme is case-insensitive (RFC 9110 §11.4); a bare scheme
        // with no token still fails.
        for scheme in ["bearer", "BEARER", "BeArEr"] {
            let mut mixed = HashMap::new();
            mixed.insert("authorization".to_string(), format!("{scheme} s3cret"));
            assert!(authorized(&mixed, &gated), "{scheme}");
        }
        let mut bare = HashMap::new();
        bare.insert("authorization".to_string(), "Bearer".to_string());
        assert!(!authorized(&bare, &gated));
    }

    #[tokio::test]
    async fn end_to_end_client_against_hyper() {
        let store = Arc::new(MemoryStore::new());
        store
            .store_instance(
                Bytes::from(test_file("1.2.9", "1.2.9.1", "1.2.9.1.1", 4, 4)),
                None,
            )
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
            Arc::new(InflightBudget::default()),
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
    fn inflight_budget_accounts_and_releases() {
        let budget = Arc::new(InflightBudget::new(100));
        assert!(budget.try_reserve(60));
        assert!(budget.try_reserve(40));
        assert!(!budget.try_reserve(1), "past cap: nothing reserved");
        budget.release(60);
        assert!(budget.try_reserve(1));
        // Guards release on drop.
        let used = *budget.used.lock().unwrap();
        {
            let mut g = InflightGuard {
                budget: budget.clone(),
                bytes: 0,
            };
            assert!(g.reserve(10));
            assert_eq!(*budget.used.lock().unwrap(), used + 10);
        }
        assert_eq!(*budget.used.lock().unwrap(), used);
    }

    #[test]
    fn body_cap_boundary() {
        assert!(!body_over_cap(0, 0));
        assert!(!body_over_cap(MAX_BODY_LEN - 1, 1));
        assert!(!body_over_cap(MAX_BODY_LEN, 0));
        assert!(body_over_cap(MAX_BODY_LEN, 1));
        assert!(body_over_cap(usize::MAX, usize::MAX));
    }

    /// Send a raw HTTP request, return the status line's code.
    async fn raw_status(port: u16, head: &[u8], body: &[u8]) -> u16 {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut tcp = tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .unwrap();
        tcp.write_all(head).await.unwrap();
        tcp.write_all(body).await.unwrap();
        let mut buf = vec![0u8; 1024];
        let n = tcp.read(&mut buf).await.unwrap();
        let line = String::from_utf8_lossy(&buf[..n]);
        line.split_whitespace()
            .nth(1)
            .unwrap_or("0")
            .parse()
            .unwrap_or(0)
    }

    #[tokio::test]
    async fn unauthorized_over_wire_is_401() {
        let store = Arc::new(MemoryStore::new());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            loop {
                let Ok((tcp, _)) = listener.accept().await else {
                    break;
                };
                let s = store.clone();
                tokio::spawn(async move {
                    serve_connection_with_auth(
                        s,
                        format!("http://127.0.0.1:{port}"),
                        tcp,
                        ServerAuth {
                            bearer_token: Some("test-bearer-token".to_string()),
                        },
                    )
                    .await
                    .ok();
                });
            }
        });
        let authz = |t: &str| format!("authorization: Bearer {t}\r\n");
        let req = |auth: &str| {
            format!("GET /studies HTTP/1.1\r\nhost: x\r\n{auth}connection: close\r\n\r\n")
                .into_bytes()
        };
        assert_eq!(raw_status(port, &req(""), &[]).await, 401);
        assert_eq!(
            raw_status(port, &req(&authz("wrong-token")), &[]).await,
            401
        );
        assert_eq!(
            raw_status(port, &req(&authz("test-bearer-token")), &[]).await,
            200
        );
    }

    #[tokio::test]
    async fn oversize_body_over_wire_is_413() {
        let store = Arc::new(MemoryStore::new());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let Ok((tcp, _)) = listener.accept().await else {
                return;
            };
            serve_connection(store, format!("http://127.0.0.1:{port}"), tcp)
                .await
                .ok();
        });
        // One byte past the cap, streamed from a small reusable chunk
        // (the server must 413 while accumulating, not after).
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut tcp = tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .unwrap();
        tcp.write_all(
            b"POST /studies HTTP/1.1\r\nhost: x\r\ncontent-type: multipart/related; boundary=b\r\ntransfer-encoding: chunked\r\n\r\n",
        )
        .await
        .unwrap();
        let chunk = vec![b'x'; 1 << 20];
        let mut sent = 0usize;
        // Writes may fail once the server 413s and hangs up; the
        // status below is the assertion either way.
        while sent <= MAX_BODY_LEN {
            let head = format!("{:X}\r\n", chunk.len());
            if tcp.write_all(head.as_bytes()).await.is_err()
                || tcp.write_all(&chunk).await.is_err()
                || tcp.write_all(b"\r\n").await.is_err()
            {
                break;
            }
            sent += chunk.len();
        }
        let _ = tcp.write_all(b"0\r\n\r\n").await;
        let mut buf = vec![0u8; 1024];
        let n = tcp.read(&mut buf).await.unwrap();
        let line = String::from_utf8_lossy(&buf[..n]);
        let status: u16 = line
            .split_whitespace()
            .nth(1)
            .unwrap_or("0")
            .parse()
            .unwrap_or(0);
        assert_eq!(status, 413, "{line}");
    }

    #[test]
    fn path_segments_decode_symmetrically() {
        // Client encode → server decode round-trips (the server side
        // of `DicomWebClient::path_segment`).
        for raw in ["1.2.9", "a?b#c d", "100%", "a+b", "Ü", "ünïcodé"] {
            let enc = DicomWebClient::path_segment(raw);
            assert_eq!(decode_path_segment(&enc), raw, "{raw}");
        }
        // `+` stays a plus in paths (plus-as-space is query-only).
        assert_eq!(decode_path_segment("a+b"), "a+b");
        // Malformed `%` stays literal, never panics or drops bytes.
        assert_eq!(decode_path_segment("a%2"), "a%2");
        assert_eq!(decode_path_segment("a%zz"), "a%zz");
        assert_eq!(decode_path_segment("%"), "%");
        assert_eq!(decode_path_segment("100%%"), "100%%");
        // Whole-path decode preserves separators.
        assert_eq!(
            decode_request_path("/studies/1.2.9%41/series"),
            "/studies/1.2.9A/series"
        );
        assert_eq!(decode_request_path("/"), "/");
    }

    #[tokio::test]
    async fn encoded_uid_routes_over_wire() {
        let store = Arc::new(MemoryStore::new());
        store
            .store_instance(
                Bytes::from(test_file("1.2.9", "1.2.9.1", "1.2.9.1.1", 4, 4)),
                None,
            )
            .unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            loop {
                let Ok((tcp, _)) = listener.accept().await else {
                    break;
                };
                let s = store.clone();
                tokio::spawn(async move {
                    serve_connection(s, format!("http://127.0.0.1:{port}"), tcp)
                        .await
                        .ok();
                });
            }
        });
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        async fn fetch(port: u16, head: &[u8]) -> String {
            let mut tcp = tokio::net::TcpStream::connect(("127.0.0.1", port))
                .await
                .unwrap();
            tcp.write_all(head).await.unwrap();
            let mut body = Vec::new();
            tcp.read_to_end(&mut body).await.unwrap();
            String::from_utf8_lossy(&body).into_owned()
        }
        // Strict encoders escape even dots: %2E-encoded UIDs reach
        // the store decoded (was: no match).
        let text = fetch(
            port,
            b"GET /studies/1%2E2%2E9/series HTTP/1.1\r\nhost: x\r\nconnection: close\r\n\r\n",
        )
        .await;
        assert!(text.starts_with("HTTP/1.1 200"), "{text}");
        assert!(text.contains("1.2.9.1"), "{text}");
        // %2F decodes to a separator and fails closed (404 — never a
        // segment smuggle into another route).
        let text = fetch(
            port,
            b"GET /studies/1.2.9%2FA/series HTTP/1.1\r\nhost: x\r\nconnection: close\r\n\r\n",
        )
        .await;
        assert!(text.starts_with("HTTP/1.1 404"), "{text}");
    }

    #[test]
    fn stow_routes_expect_bodies_only() {
        use hyper::Method;
        assert!(route_expects_body(&Method::POST, "/studies", "http://x"));
        assert!(route_expects_body(
            &Method::POST,
            "/studies/1.2.3",
            "http://x"
        ));
        assert!(!route_expects_body(
            &Method::POST,
            "/studies/1.2.3/series",
            "http://x"
        ));
        assert!(!route_expects_body(&Method::POST, "/nope", "http://x"));
        assert!(!route_expects_body(&Method::GET, "/studies", "http://x"));
        assert!(!route_expects_body(&Method::DELETE, "/studies", "http://x"));
        // Prefixed deployments agree with dispatch (segment-boundary
        // strip, never a prefix-collapse).
        assert!(route_expects_body(
            &Method::POST,
            "/dicomweb/studies",
            "http://x/dicomweb"
        ));
        assert!(route_expects_body(
            &Method::POST,
            "/dicomweb/studies/1.2.3",
            "http://x/dicomweb"
        ));
        assert!(!route_expects_body(
            &Method::POST,
            "/dicomweb2/studies",
            "http://x/dicomweb"
        ));
    }

    #[tokio::test]
    async fn unknown_post_answers_without_body() {
        // Route-before-buffering: a POST to an unknown route declares
        // a 100 MB body but sends none — the 405 must arrive without
        // the server waiting on the missing bytes (the old code
        // buffered first and hung here).
        let store = Arc::new(MemoryStore::new());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let Ok((tcp, _)) = listener.accept().await else {
                return;
            };
            serve_connection(store, format!("http://127.0.0.1:{port}"), tcp)
                .await
                .ok();
        });
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut tcp = tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .unwrap();
        tcp.write_all(
            b"POST /nope HTTP/1.1\r\nhost: x\r\ncontent-length: 104857600\r\nconnection: close\r\n\r\n",
        )
        .await
        .unwrap();
        // No body follows: the server must still answer promptly.
        let status = tokio::time::timeout(Duration::from_secs(10), async {
            let mut buf = vec![0u8; 1024];
            let n = tcp.read(&mut buf).await.unwrap();
            String::from_utf8_lossy(&buf[..n])
                .split_whitespace()
                .nth(1)
                .unwrap_or("0")
                .parse::<u16>()
                .unwrap_or(0)
        })
        .await
        .expect("server waited on the un-sent body");
        assert_eq!(status, 405);
    }

    #[tokio::test]
    async fn saturated_response_budget_is_503() {
        // An 8-byte shared budget cannot hold any real response: the
        // QIDO answer fails fast with 503 (was: unbounded aggregate
        // response buffering).
        let store = Arc::new(MemoryStore::new());
        store
            .store_instance(
                Bytes::from(test_file("1.2.9", "1.2.9.1", "1.2.9.1.1", 4, 4)),
                None,
            )
            .unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let budget = Arc::new(InflightBudget::new(8));
        tokio::spawn(async move {
            loop {
                let Ok((tcp, _)) = listener.accept().await else {
                    break;
                };
                let (s, b) = (store.clone(), budget.clone());
                tokio::spawn(async move {
                    serve_connection_with_budget(
                        s,
                        format!("http://127.0.0.1:{port}"),
                        tcp,
                        ServerAuth::default(),
                        b,
                    )
                    .await
                    .ok();
                });
            }
        });
        let head = b"GET /studies HTTP/1.1\r\nhost: x\r\nconnection: close\r\n\r\n";
        assert_eq!(raw_status(port, head, &[]).await, 503);
        // A generous budget still serves.
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let store = Arc::new(MemoryStore::new());
        store
            .store_instance(
                Bytes::from(test_file("1.2.9", "1.2.9.1", "1.2.9.1.1", 4, 4)),
                None,
            )
            .unwrap();
        tokio::spawn(async move {
            let Ok((tcp, _)) = listener.accept().await else {
                return;
            };
            serve_connection(store, format!("http://127.0.0.1:{port}"), tcp)
                .await
                .ok();
        });
        assert_eq!(raw_status(port, head, &[]).await, 200);
    }
}
