//! DICOMweb client: QIDO-RS, WADO-RS, STOW-RS (NEMA PS3.18 §8).

use bytes::Bytes;
use vyna_dcm_codec::{FileMeta, dataset_from_json, read_part10};
use vyna_dcm_core::DataSet;

use crate::{
    Error, Result,
    multipart::{
        Part, decode_multipart, encode_multipart, is_dicom_media_type, new_boundary,
        parse_content_type,
    },
};

/// Client authentication.
#[derive(Clone)]
#[non_exhaustive]
pub enum Auth {
    /// `Authorization: Bearer <token>`.
    Bearer(String),
    /// HTTP Basic auth.
    Basic {
        /// Username.
        username: String,
        /// Password.
        password: String,
    },
}

// Manual `Debug`: tokens and passwords must never reach logs.
impl std::fmt::Debug for Auth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Auth::Bearer(_) => write!(f, "Bearer(<redacted>)"),
            Auth::Basic { username, .. } => f
                .debug_struct("Basic")
                .field("username", username)
                .field("password", &"<redacted>")
                .finish(),
        }
    }
}

/// Lowercased `scheme://host[:port]` of an absolute HTTP(S) URL, or
/// `None` when the URL is relative or not HTTP(S).
///
/// Default ports (`:443`/`:80`) normalize away, matching reqwest's
/// own redirect logic (`port_or_known_default`): spelling the base
/// `https://h/x` and a BulkDataURI `https://h:443/y` must not count
/// as cross-origin (auth stripped → 401).
fn origin_of(url: &str) -> Option<String> {
    let (scheme, rest) = url
        .strip_prefix("https://")
        .map(|r| ("https", r))
        .or_else(|| url.strip_prefix("http://").map(|r| ("http", r)))?;
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    if authority.is_empty() {
        return None;
    }
    // Split off a port only to drop exact default spellings; the
    // rejoin below reconstructs anything else verbatim (including
    // IPv6 literals, where the "port" half never equals a default).
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) => (h, Some(p)),
        None => (authority, None),
    };
    let default = match scheme {
        "https" => "443",
        _ => "80",
    };
    let normalized = match port {
        Some(p) if p != default => format!("{host}:{p}"),
        _ => host.to_string(),
    };
    // Lowercasing the whole authority is safe (host is
    // case-insensitive, port/userinfo are matched verbatim anyway).
    Some(format!("{}://{}", scheme, normalized.to_ascii_lowercase()))
}

/// Whether `url` may carry this client's credentials.
///
/// True for relative URLs (resolved against our own base) and for
/// absolute URLs on the same origin as `base`. When either side is
/// unparseable we keep the legacy behavior (send) so exotic but
/// same-server setups do not break; only a proven mismatch strips.
fn same_origin(url: &str, base: &str) -> bool {
    match (origin_of(url), origin_of(base)) {
        (Some(a), Some(b)) => a == b,
        _ => true,
    }
}

/// Maximum response body the client buffers (1 GiB, matching the
/// server-side request cap): a hostile QIDO/metadata JSON cannot
/// OOM the client.
const MAX_RESPONSE_BYTES: usize = 1024 * 1024 * 1024;

/// Default total request timeout (bulk transfers can legitimately
/// take minutes).
const DEFAULT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);

/// Custom TLS trust/identity (see [`DicomWebClient::with_extra_ca_pem`]).
/// Stored as PEM so every builder composes through one rebuild path.
#[derive(Clone, Default)]
struct ClientTls {
    /// Extra CA PEM: appended to Mozilla roots, or the only trust
    /// when `private_ca_only`.
    ca_pem: Option<Vec<u8>>,
    /// Trust only `ca_pem` (no WebPKI roots).
    private_ca_only: bool,
    /// Mutual-TLS client identity: cert chain + private key PEM.
    client_identity: Option<(Vec<u8>, Vec<u8>)>,
}

// Manual `Debug`: key/CA PEM bytes must never reach logs.
impl std::fmt::Debug for ClientTls {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClientTls")
            .field("ca_pem", &self.ca_pem.as_ref().map(|_| "<redacted>"))
            .field("private_ca_only", &self.private_ca_only)
            .field(
                "client_identity",
                &self.client_identity.as_ref().map(|_| "<redacted>"),
            )
            .finish()
    }
}

/// Classify a reqwest transport failure into the typed variants
/// ([`Error::Timeout`] is retryable for idempotent reads;
/// [`Error::Connect`] and [`Error::Tls`] are not transient
/// round-trips to retry blindly).
fn transport_error(e: reqwest::Error) -> Error {
    if e.is_timeout() {
        return Error::Timeout(e.to_string());
    }
    if has_tls_source(&e) {
        return Error::Tls(e.to_string());
    }
    if e.is_connect() {
        return Error::Connect(e.to_string());
    }
    Error::Http(e.to_string())
}

/// Whether the error's source chain carries a TLS failure. A failed
/// handshake also reports `is_connect`, so this check runs first.
/// Matching stays below the top message, which embeds the request
/// URL — a host named `certs.example` must not misclassify a refused
/// connection as a TLS failure.
fn has_tls_source(e: &reqwest::Error) -> bool {
    use std::error::Error as _;
    // Deterministic pass: any `rustls::Error` in the chain, through
    // `io::Error` payload layers too (handshake failures arrive
    // double-wrapped, and `source()` does not descend into `io`
    // payloads — only `get_ref` does).
    let mut next = e.source();
    while let Some(s) = next {
        if tls_payload(s) {
            return true;
        }
        next = s.source();
    }
    // Fallback pass: rustls failure names in chain `Debug` (pinned by
    // `transport_tls_failure_is_typed`, whose `InvalidMessage` names
    // no crate).
    let mut next = e.source();
    while let Some(s) = next {
        let d = format!("{s:?}").to_ascii_lowercase();
        if d.contains("rustls")
            || d.contains("invalidmessage")
            || d.contains("certificate")
            || d.contains("handshake")
            || d.contains("webpki")
        {
            return true;
        }
        next = s.source();
    }
    false
}

/// Whether this chain link is (or wraps, through any depth of
/// `io::Error` payload) a `rustls::Error`.
fn tls_payload(s: &(dyn std::error::Error + 'static)) -> bool {
    if s.downcast_ref::<rustls::Error>().is_some() {
        return true;
    }
    if let Some(io) = s.downcast_ref::<std::io::Error>()
        && let Some(inner) = io.get_ref()
    {
        return tls_payload(inner);
    }
    false
}

/// DICOMweb client over a service base URL like
/// `https://pacs.example/dicom-web`.
#[derive(Clone, Debug)]
pub struct DicomWebClient {
    http: reqwest::Client,
    base_url: String,
    auth: Option<Auth>,
    /// Per-request timeout, applied to every request builder (see
    /// [`Self::with_timeout`]).
    timeout: std::time::Duration,
    tls: ClientTls,
}

impl DicomWebClient {
    /// Build a client for `base_url` (trailing slashes trimmed).
    ///
    /// HTTPS uses the OS certificate store; the ring crypto provider
    /// (shared with `vyna-dcm-net`) is installed as the rustls default.
    /// Empty or unparseable base URLs are [`Error::BadRequest`] now,
    /// not confusing transport errors at first use. URLs with userinfo
    /// (`https://user:pass@host`) are rejected: credentials belong in
    /// [`Auth`], not in a string that reaches `Debug` and
    /// error messages (and that reqwest would turn into surprise
    /// Basic-auth headers). Query strings and fragments are rejected
    /// too: every call appends path segments, which a `?`/`#` would
    /// corrupt into query/fragment.
    pub fn new(base_url: impl Into<String>) -> Result<Self> {
        let base = base_url.into();
        let base = base.trim_end_matches('/').to_string();
        if base.is_empty() {
            return Err(Error::BadRequest("empty base URL".into()));
        }
        let parsed = reqwest::Url::parse(&base)
            .map_err(|e| Error::BadRequest(format!("bad base URL {base:?}: {e}")))?;
        if !matches!(parsed.scheme(), "http" | "https") {
            return Err(Error::BadRequest(format!(
                "bad base URL {base:?}: want http(s)"
            )));
        }
        if !parsed.username().is_empty() || parsed.password().is_some() {
            return Err(Error::BadRequest(format!(
                "bad base URL {base:?}: userinfo credentials are rejected, use Auth"
            )));
        }
        if parsed.query().is_some() || parsed.fragment().is_some() {
            return Err(Error::BadRequest(format!(
                "bad base URL {base:?}: query/fragment would corrupt appended paths"
            )));
        }
        // reqwest 0.13 `rustls-no-provider` needs the app to install a
        // provider; ignore failure (another ring install already done).
        let _ = rustls::crypto::ring::default_provider().install_default();
        // A hung server must not hang us forever; bulk transfers
        // can legitimately take minutes, hence the budget.
        let http = Self::build_http(DEFAULT_TIMEOUT, &ClientTls::default())?;
        Ok(Self {
            http,
            base_url: base,
            auth: None,
            timeout: DEFAULT_TIMEOUT,
            tls: ClientTls::default(),
        })
    }

    /// rustls client config verifying against the OS (native) trust
    /// store via `rustls-platform-verifier` — without this, reqwest's
    /// `rustls-no-provider` build ships no roots and every HTTPS
    /// request fails verification.
    fn platform_tls_config() -> Result<rustls::ClientConfig> {
        use rustls_platform_verifier::BuilderVerifierExt;
        rustls::ClientConfig::builder()
            .with_platform_verifier()
            .map_err(|e| Error::Tls(format!("platform TLS verifier: {e}")))
            .map(|b| b.with_no_client_auth())
    }

    /// rustls client config for custom trust/identity (parity with
    /// `vyna-dcm-net`'s `TlsConnector`): Mozilla roots plus the CA
    /// PEM, or the CA PEM alone in private-CA mode, with an optional
    /// mutual-TLS client identity. Custom-CA modes verify against
    /// Mozilla roots rather than the OS store (the platform verifier
    /// exposes no extra-roots hook).
    fn custom_tls_config(tls: &ClientTls) -> Result<rustls::ClientConfig> {
        use std::io::BufReader;
        let ca_pem = tls
            .ca_pem
            .as_deref()
            .ok_or_else(|| Error::Tls("no CA configured".into()))?;
        let mut roots = rustls::RootCertStore::empty();
        if !tls.private_ca_only {
            roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        }
        let mut added = 0u32;
        for cert in rustls_pemfile::certs(&mut BufReader::new(ca_pem)) {
            let cert = cert.map_err(|e| Error::Tls(format!("CA PEM: {e}")))?;
            roots
                .add(cert)
                .map_err(|e| Error::Tls(format!("CA PEM: {e}")))?;
            added += 1;
        }
        if added == 0 {
            return Err(Error::Tls("CA PEM holds no certificates".into()));
        }
        let builder = rustls::ClientConfig::builder().with_root_certificates(roots);
        match tls.client_identity.as_ref() {
            None => Ok(builder.with_no_client_auth()),
            Some((cert_pem, key_pem)) => {
                let (certs, key) = Self::parse_identity(cert_pem, key_pem)?;
                builder
                    .with_client_auth_cert(certs, key)
                    .map_err(|e| Error::Tls(format!("client identity: {e}")))
            }
        }
    }

    /// Parse a mutual-TLS client identity (cert chain + private key
    /// PEM), requiring at least one certificate and a key.
    fn parse_identity(
        cert_pem: &[u8],
        key_pem: &[u8],
    ) -> Result<(
        Vec<rustls::pki_types::CertificateDer<'static>>,
        rustls::pki_types::PrivateKeyDer<'static>,
    )> {
        use std::io::BufReader;
        let certs: Vec<_> = rustls_pemfile::certs(&mut BufReader::new(cert_pem))
            .collect::<std::result::Result<_, _>>()
            .map_err(|e| Error::Tls(format!("client identity certs: {e}")))?;
        if certs.is_empty() {
            return Err(Error::Tls("client identity holds no certificates".into()));
        }
        let key = rustls_pemfile::private_key(&mut BufReader::new(key_pem))
            .map_err(|e| Error::Tls(format!("client identity key: {e}")))?
            .ok_or_else(|| Error::Tls("client identity holds no private key".into()))?;
        Ok((certs, key))
    }

    /// TLS config for the current options: platform trust by default
    /// (plus a client identity when set), custom trust once a CA PEM
    /// is configured.
    fn tls_config_for(tls: &ClientTls) -> Result<rustls::ClientConfig> {
        if tls.ca_pem.is_some() {
            return Self::custom_tls_config(tls);
        }
        match tls.client_identity.as_ref() {
            None => Self::platform_tls_config(),
            Some((cert_pem, key_pem)) => {
                use rustls_platform_verifier::BuilderVerifierExt;
                let (certs, key) = Self::parse_identity(cert_pem, key_pem)?;
                rustls::ClientConfig::builder()
                    .with_platform_verifier()
                    .map_err(|e| Error::Tls(format!("platform TLS verifier: {e}")))?
                    .with_client_auth_cert(certs, key)
                    .map_err(|e| Error::Tls(format!("client identity: {e}")))
            }
        }
    }

    /// Shared HTTP engine (also used by the TLS builders, which
    /// rebuild the immutable reqwest client — timeouts apply
    /// per-request instead, so [`Self::with_timeout`] never rebuilds).
    fn build_http(timeout: std::time::Duration, tls: &ClientTls) -> Result<reqwest::Client> {
        reqwest::Client::builder()
            .timeout(timeout)
            .use_preconfigured_tls(Self::tls_config_for(tls)?)
            .build()
            .map_err(|e| Error::Http(e.to_string()))
    }

    /// Use bearer-token auth.
    #[must_use]
    pub fn with_bearer(mut self, token: impl Into<String>) -> Self {
        self.auth = Some(Auth::Bearer(token.into()));
        self
    }

    /// Use HTTP Basic auth.
    #[must_use]
    pub fn with_basic(mut self, username: impl Into<String>, password: impl Into<String>) -> Self {
        self.auth = Some(Auth::Basic {
            username: username.into(),
            password: password.into(),
        });
        self
    }

    /// Override the default 300 s total request timeout.
    ///
    /// Infallible by construction: the timeout is stored and applied
    /// to every request builder, so — unlike the TLS builders below —
    /// this never rebuilds the HTTP engine and never fails.
    #[must_use]
    pub fn with_timeout(mut self, timeout: std::time::Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Rebuild the HTTP engine after a TLS option change (the only
    /// builders that rebuild: `reqwest` clients are immutable).
    fn rebuild_http(&mut self) -> Result<()> {
        self.http = Self::build_http(self.timeout, &self.tls)?;
        Ok(())
    }

    /// Trust an additional private CA (PEM) alongside the default
    /// roots — hospital PACS fleets behind an internal CA (parity
    /// with `vyna-dcm-net`'s `TlsConnector::new`). Empty or
    /// unparseable PEM is [`Error::Tls`]; last CA call wins.
    pub fn with_extra_ca_pem(mut self, ca_pem: &[u8]) -> Result<Self> {
        self.tls.ca_pem = Some(ca_pem.to_vec());
        self.tls.private_ca_only = false;
        self.rebuild_http()?;
        Ok(self)
    }

    /// Trust *only* the given private CA PEM (no WebPKI roots) —
    /// prefer this over [`Self::with_extra_ca_pem`] when the fleet
    /// has an internal CA, so no public-CA-issued certificate can
    /// impersonate the peer (parity with `TlsConnector::new_private_ca`).
    pub fn with_private_ca_pem(mut self, ca_pem: &[u8]) -> Result<Self> {
        self.tls.ca_pem = Some(ca_pem.to_vec());
        self.tls.private_ca_only = true;
        self.rebuild_http()?;
        Ok(self)
    }

    /// Present a mutual-TLS client identity (certificate chain +
    /// private key PEM) on every HTTPS connection (parity with
    /// `TlsConnector`'s client-identity parameter). Composes with the
    /// default platform trust or with either CA builder above.
    pub fn with_client_identity_pem(
        mut self,
        cert_chain_pem: &[u8],
        key_pem: &[u8],
    ) -> Result<Self> {
        self.tls.client_identity = Some((cert_chain_pem.to_vec(), key_pem.to_vec()));
        self.rebuild_http()?;
        Ok(self)
    }

    fn get(&self, url: String) -> reqwest::RequestBuilder {
        let req = self.http.get(url).timeout(self.timeout);
        self.apply_auth(req)
    }

    fn post(&self, url: String) -> reqwest::RequestBuilder {
        self.apply_auth(self.http.post(url).timeout(self.timeout))
    }

    fn apply_auth(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match &self.auth {
            Some(Auth::Bearer(t)) => req.bearer_auth(t),
            Some(Auth::Basic { username, password }) => req.basic_auth(username, Some(password)),
            None => req,
        }
    }

    // -- QIDO-RS ---------------------------------------------------------

    /// Percent-encode one URL path segment. UIDs interpolate into
    /// request paths, and a `?` would inject a query while a `#`
    /// would silently retarget the request — so every UID goes
    /// through here (`[A-Za-z0-9.-]` pass, the rest `%XX`).
    #[must_use]
    pub fn path_segment(s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        for b in s.bytes() {
            if b.is_ascii_alphanumeric() || b == b'.' || b == b'-' {
                out.push(b as char);
            } else {
                out.push_str(&format!("%{b:02X}"));
            }
        }
        out
    }

    /// Generic QIDO-RS search over `resource` (`studies`,
    /// `studies/{uid}/series`, `studies/{uid}/series/{uid}/instances`, ...).
    ///
    /// `resource` interpolates verbatim: callers composing it from UIDs
    /// must encode each segment with [`path_segment`](Self::path_segment)
    /// (the typed `search_*` wrappers already do).
    pub async fn qido(&self, resource: &str, query: &[(&str, &str)]) -> Result<Vec<DataSet>> {
        let url = format!("{}/{}", self.base_url, resource.trim_start_matches('/'));
        let resp = self
            .get(url)
            .header("Accept", "application/dicom+json")
            .query(query)
            .send()
            .await
            .map_err(transport_error)?;
        Self::expect_json_array(resp).await
    }

    /// Search studies (`GET /studies?...`).
    pub async fn search_studies(&self, query: &[(&str, &str)]) -> Result<Vec<DataSet>> {
        self.qido("studies", query).await
    }

    /// Search series of a study.
    pub async fn search_series(
        &self,
        study_uid: &str,
        query: &[(&str, &str)],
    ) -> Result<Vec<DataSet>> {
        self.qido(
            &format!("studies/{}/series", Self::path_segment(study_uid)),
            query,
        )
        .await
    }

    /// Search instances of a series.
    pub async fn search_instances(
        &self,
        study_uid: &str,
        series_uid: &str,
        query: &[(&str, &str)],
    ) -> Result<Vec<DataSet>> {
        self.qido(
            &format!(
                "studies/{}/series/{}/instances",
                Self::path_segment(study_uid),
                Self::path_segment(series_uid)
            ),
            query,
        )
        .await
    }

    // -- WADO-RS ---------------------------------------------------------

    /// Retrieve an instance as raw multipart parts.
    ///
    /// `accept` defaults to `multipart/related; type="application/dicom"`;
    /// pass e.g. `multipart/related; type="application/dicom";
    /// transfer-syntax=1.2.840.10008.1.2.1` to request a transfer syntax.
    pub async fn wado_parts(
        &self,
        study_uid: &str,
        series_uid: &str,
        instance_uid: &str,
        accept: Option<&str>,
    ) -> Result<Vec<Part>> {
        let url = format!(
            "{}/studies/{}/series/{}/instances/{}",
            self.base_url,
            Self::path_segment(study_uid),
            Self::path_segment(series_uid),
            Self::path_segment(instance_uid)
        );
        let resp = self
            .get(url)
            .header(
                "Accept",
                accept.unwrap_or("multipart/related; type=\"application/dicom\""),
            )
            .send()
            .await
            .map_err(transport_error)?;
        Self::expect_multipart(resp).await
    }

    /// Retrieve a whole study as one `application/dicom` part per
    /// instance (`GET /studies/{uid}`, PS3.18 §6.5.3).
    pub async fn wado_study(&self, study_uid: &str) -> Result<Vec<Part>> {
        let url = format!(
            "{}/studies/{}",
            self.base_url,
            Self::path_segment(study_uid)
        );
        let resp = self
            .get(url)
            .header("Accept", "multipart/related; type=\"application/dicom\"")
            .send()
            .await
            .map_err(transport_error)?;
        Self::expect_multipart(resp).await
    }

    /// Retrieve a whole series as one `application/dicom` part per
    /// instance (`GET /studies/{uid}/series/{uid}`, PS3.18 §6.5.4).
    pub async fn wado_series(&self, study_uid: &str, series_uid: &str) -> Result<Vec<Part>> {
        let url = format!(
            "{}/studies/{}/series/{}",
            self.base_url,
            Self::path_segment(study_uid),
            Self::path_segment(series_uid)
        );
        let resp = self
            .get(url)
            .header("Accept", "multipart/related; type=\"application/dicom\"")
            .send()
            .await
            .map_err(transport_error)?;
        Self::expect_multipart(resp).await
    }

    /// Retrieve an instance's first `application/dicom` part as raw bytes.
    ///
    /// Shared [`Bytes`]: no copy between the response buffer and the caller.
    pub async fn wado_file(
        &self,
        study_uid: &str,
        series_uid: &str,
        instance_uid: &str,
    ) -> Result<Bytes> {
        let parts = self
            .wado_parts(study_uid, series_uid, instance_uid, None)
            .await?;
        Self::dicom_part_body(parts)
    }

    /// First `application/dicom` part body. Media-type match, not
    /// string equality: compliant servers add parameters such as
    /// `transfer-syntax` (PS3.18 §8.6).
    fn dicom_part_body(parts: Vec<Part>) -> Result<Bytes> {
        parts
            .into_iter()
            .find(|p| is_dicom_media_type(&p.content_type))
            .map(|p| p.body)
            .ok_or_else(|| Error::Multipart("no application/dicom part".into()))
    }

    /// Retrieve and parse an instance into `(meta, dataset)`.
    pub async fn wado_dataset(
        &self,
        study_uid: &str,
        series_uid: &str,
        instance_uid: &str,
    ) -> Result<(FileMeta, DataSet)> {
        let bytes = self.wado_file(study_uid, series_uid, instance_uid).await?;
        Ok(read_part10(&bytes)?)
    }

    /// Retrieve instance metadata (`.../instances/{uid}/metadata`).
    ///
    /// Bulk elements arrive as `BulkDataURI` links and parse as empty
    /// values; use [`wado_metadata_raw`](Self::wado_metadata_raw) to read
    /// the URIs, then [`wado_bulkdata`](Self::wado_bulkdata) to fetch them.
    pub async fn wado_metadata(
        &self,
        study_uid: &str,
        series_uid: &str,
        instance_uid: &str,
    ) -> Result<DataSet> {
        let raw = self
            .wado_metadata_raw(study_uid, series_uid, instance_uid)
            .await?;
        vyna_dcm_codec::dataset_from_json(&raw).map_err(Error::Codec)
    }

    /// Retrieve instance metadata as raw DICOM JSON (preserves
    /// `BulkDataURI` links).
    pub async fn wado_metadata_raw(
        &self,
        study_uid: &str,
        series_uid: &str,
        instance_uid: &str,
    ) -> Result<serde_json::Value> {
        let url = format!(
            "{}/studies/{}/series/{}/instances/{}/metadata",
            self.base_url,
            Self::path_segment(study_uid),
            Self::path_segment(series_uid),
            Self::path_segment(instance_uid)
        );
        let resp = self
            .get(url)
            .header("Accept", "application/dicom+json")
            .send()
            .await
            .map_err(transport_error)?;
        let (_ct, body) = Self::expect_ok(resp).await?;
        let v: serde_json::Value =
            serde_json::from_slice(&body).map_err(|e| Error::Json(e.to_string()))?;
        v.as_array()
            .and_then(|a| a.first().cloned())
            .ok_or_else(|| Error::Json("empty metadata array".into()))
    }

    /// Retrieve rendered frames (`.../instances/{uid}/frames/{1,2,...}`).
    pub async fn wado_frames(
        &self,
        study_uid: &str,
        series_uid: &str,
        instance_uid: &str,
        frames: &[u32],
        accept: &str,
    ) -> Result<Vec<Part>> {
        if frames.is_empty() {
            return Err(Error::BadRequest("no frames requested".into()));
        }
        let list = frames
            .iter()
            .map(u32::to_string)
            .collect::<Vec<_>>()
            .join(",");
        let url = format!(
            "{}/studies/{}/series/{}/instances/{}/frames/{list}",
            self.base_url,
            Self::path_segment(study_uid),
            Self::path_segment(series_uid),
            Self::path_segment(instance_uid)
        );
        let resp = self
            .get(url)
            .header("Accept", accept)
            .send()
            .await
            .map_err(transport_error)?;
        Self::expect_multipart(resp).await
    }

    /// Fetch a BulkDataURI (absolute URL or server-relative path).
    ///
    /// Absolute URIs on another origin are fetched *without* our
    /// credentials: BulkDataURIs are server-supplied and may point
    /// anywhere, so forwarding `Authorization` would leak it (SSRF).
    /// Absolute URIs carrying their own userinfo (`http://u:p@host`)
    /// are rejected: reqwest would turn it into a surprise Basic-auth
    /// header, the exact class base-URL userinfo was rejected for.
    pub async fn wado_bulkdata(&self, uri: &str) -> Result<(String, Vec<u8>)> {
        let url = if uri.starts_with("http://") || uri.starts_with("https://") {
            let parsed = reqwest::Url::parse(uri)
                .map_err(|e| Error::BadRequest(format!("bad BulkDataURI: {e}")))?;
            if !parsed.username().is_empty() || parsed.password().is_some() {
                return Err(Error::BadRequest(
                    "BulkDataURI carries its own userinfo credentials".into(),
                ));
            }
            uri.to_string()
        } else if uri.starts_with('/') {
            format!("{}{uri}", self.base_url)
        } else {
            format!("{}/{uri}", self.base_url)
        };
        // Same-origin (including server-relative URIs resolved above)
        // keeps auth; cross-origin absolute URIs must not carry it.
        let same = same_origin(&url, &self.base_url);
        let req = self.http.get(url).timeout(self.timeout);
        let req = if same { self.apply_auth(req) } else { req };
        let resp = req.send().await.map_err(transport_error)?;
        Self::expect_ok(resp).await
    }

    // -- WADO-URI ----------------------------------------------------------

    /// WADO-URI retrieve (`GET {base}?requestType=WADO&...`, PS3.18 §6,
    /// retired but still widely deployed on legacy PACS).
    ///
    /// Returns the raw response content type and bytes.
    pub async fn wado_uri(&self, req: &WadoUriRequest) -> Result<(String, Vec<u8>)> {
        let params = Self::wado_uri_params(req)?;
        let resp = self
            .get(self.base_url.clone())
            .query(&params)
            .send()
            .await
            .map_err(transport_error)?;
        Self::expect_ok(resp).await
    }

    /// WADO-URI retrieve parsed as Part-10.
    ///
    /// Fails unless the server answers `application/dicom` (an optional
    /// `transfer-syntax` parameter is allowed).
    pub async fn wado_uri_dataset(&self, req: &WadoUriRequest) -> Result<(FileMeta, DataSet)> {
        let (ct, bytes) = self.wado_uri(req).await?;
        let mime = ct.split(';').next().unwrap_or("").trim();
        if !mime.eq_ignore_ascii_case("application/dicom") {
            return Err(Error::BadRequest(format!(
                "wado-uri returned {mime}, not application/dicom"
            )));
        }
        Ok(read_part10(&bytes)?)
    }

    /// Validated WADO-URI query parameters (`requestType` first).
    fn wado_uri_params(req: &WadoUriRequest) -> Result<Vec<(&'static str, String)>> {
        if req.study_uid.is_empty() || req.series_uid.is_empty() || req.object_uid.is_empty() {
            return Err(Error::BadRequest(
                "wado-uri needs studyUID, seriesUID, and objectUID".into(),
            ));
        }
        if let Some(q) = req.image_quality
            && !(1..=100).contains(&q)
        {
            return Err(Error::BadRequest(format!(
                "imageQuality {q} out of range 1–100"
            )));
        }
        for (name, v) in [
            ("rows", req.rows),
            ("columns", req.columns),
            ("frameNumber", req.frame_number),
        ] {
            if v.is_some_and(|n| n == 0) {
                return Err(Error::BadRequest(format!("{name} must be nonzero")));
            }
        }
        let mut params = vec![
            ("requestType", "WADO".to_string()),
            ("studyUID", req.study_uid.clone()),
            ("seriesUID", req.series_uid.clone()),
            ("objectUID", req.object_uid.clone()),
        ];
        for (name, v) in [
            ("contentType", &req.content_type),
            ("transferSyntax", &req.transfer_syntax),
            ("annotation", &req.annotation),
            ("region", &req.region),
            ("windowCenter", &req.window_center),
            ("windowWidth", &req.window_width),
            ("presentationUID", &req.presentation_uid),
            ("presentationSeriesUID", &req.presentation_series_uid),
            ("charset", &req.charset),
        ] {
            if let Some(s) = v {
                params.push((name, s.clone()));
            }
        }
        if req.anonymize {
            params.push(("anonymize", "yes".to_string()));
        }
        for (name, v) in [
            ("rows", req.rows),
            ("columns", req.columns),
            ("frameNumber", req.frame_number),
        ] {
            if let Some(n) = v {
                params.push((name, n.to_string()));
            }
        }
        if let Some(q) = req.image_quality {
            params.push(("imageQuality", q.to_string()));
        }
        Ok(params)
    }

    // -- STOW-RS ---------------------------------------------------------

    /// Store instances (`POST /studies[/{study_uid}]`).
    ///
    /// Each file must be a Part-10 byte buffer. Returns the parsed
    /// RetrieveURL plus the raw response.
    pub async fn stow(&self, study_uid: Option<&str>, files: &[Vec<u8>]) -> Result<StowResponse> {
        if files.is_empty() {
            return Err(Error::BadRequest("nothing to store".into()));
        }
        let url = match study_uid {
            Some(u) => format!("{}/studies/{}", self.base_url, Self::path_segment(u)),
            None => format!("{}/studies", self.base_url),
        };
        let parts: Vec<Part> = files
            .iter()
            .map(|f| Part::new("application/dicom", f.clone()))
            .collect();
        let boundary = new_boundary();
        let (content_type, body) = encode_multipart(&parts, &boundary, "application/dicom")?;
        let resp = self
            .post(url)
            .header("Content-Type", content_type)
            .header("Accept", "application/dicom+json")
            .body(body)
            .send()
            .await
            .map_err(transport_error)?;
        let (status, ct, raw) = Self::read_body_capped(resp).await?;
        if !(200..300).contains(&status) {
            return Err(Error::Status {
                status,
                body: String::from_utf8_lossy(&raw).chars().take(300).collect(),
            });
        }
        // Best-effort RetrieveURL extraction from the JSON store response.
        let retrieve_url = serde_json::from_slice::<serde_json::Value>(&raw)
            .ok()
            .and_then(|v| {
                v.get("00081190")?
                    .get("Value")?
                    .as_array()?
                    .first()?
                    .as_str()
                    .map(str::to_string)
            })
            .or_else(|| {
                // XML form: the literal `<RetrieveURL>` element text.
                // Deliberately not a general XML parser (no namespaces,
                // entities, or CDATA handling per W3C XML 1.0
                // <https://www.w3.org/TR/xml/>): the Native DICOM Model
                // (PS3.19) response carries the URL as plain element
                // text, and anything fancier falls through to `None`
                // while `raw_body` keeps the full response.
                let text = String::from_utf8_lossy(&raw);
                let start = text.find("<RetrieveURL>")? + "<RetrieveURL>".len();
                let end = text[start..].find("</RetrieveURL>")? + start;
                Some(text[start..end].trim().to_string())
            });
        Ok(StowResponse {
            status,
            retrieve_url,
            content_type: ct,
            raw_body: raw,
        })
    }

    // -- response helpers --------------------------------------------------

    /// Status + content type + body, with the body streamed under a
    /// cap (a lying `Content-Length` cannot bypass the streaming
    /// budget below it).
    async fn read_body_capped(mut resp: reqwest::Response) -> Result<(u16, String, Vec<u8>)> {
        let status = resp.status().as_u16();
        let ct = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        if resp
            .content_length()
            .is_some_and(|n| n > MAX_RESPONSE_BYTES as u64)
        {
            return Err(Error::Http(format!(
                "response exceeds {MAX_RESPONSE_BYTES} bytes"
            )));
        }
        let mut body = Vec::new();
        while let Some(chunk) = resp.chunk().await.map_err(transport_error)? {
            if body.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
                return Err(Error::Http(format!(
                    "response exceeds {MAX_RESPONSE_BYTES} bytes"
                )));
            }
            body.extend_from_slice(&chunk);
        }
        Ok((status, ct, body))
    }

    async fn expect_ok(resp: reqwest::Response) -> Result<(String, Vec<u8>)> {
        let (status, ct, body) = Self::read_body_capped(resp).await?;
        if !(200..300).contains(&status) {
            return Err(Error::Status {
                status,
                body: String::from_utf8_lossy(&body).chars().take(300).collect(),
            });
        }
        Ok((ct, body))
    }

    async fn expect_json_array(resp: reqwest::Response) -> Result<Vec<DataSet>> {
        let (_ct, body) = Self::expect_ok(resp).await?;
        if body.is_empty() {
            return Ok(Vec::new()); // 204 / empty match set
        }
        let v: serde_json::Value =
            serde_json::from_slice(&body).map_err(|e| Error::Json(e.to_string()))?;
        let arr = v
            .as_array()
            .ok_or_else(|| Error::Json("QIDO response not an array".into()))?;
        arr.iter()
            .map(dataset_from_json)
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Error::Codec)
    }

    async fn expect_multipart(resp: reqwest::Response) -> Result<Vec<Part>> {
        let (status, ct, body) = Self::read_body_capped(resp).await?;
        if !(200..300).contains(&status) {
            return Err(Error::Status {
                status,
                body: String::from_utf8_lossy(&body).chars().take(300).collect(),
            });
        }
        // Single-part responses (e.g. one frame as image/jpeg) have no
        // multipart framing; wrap them as one part, keeping the full
        // Content-Type (parameters like transfer-syntax belong to the
        // caller, as with real multipart parts).
        let (mime, params) = parse_content_type(&ct);
        if mime == "multipart/related" {
            let boundary = params
                .get("boundary")
                .ok_or_else(|| Error::Multipart("missing boundary".into()))?;
            let body = Bytes::from(body);
            Ok(decode_multipart(&body, boundary)?)
        } else {
            Ok(vec![Part::new(ct, body)])
        }
    }
}

/// WADO-URI retrieve parameters (PS3.18 §6, retired).
#[derive(Clone, Debug, Default)]
pub struct WadoUriRequest {
    /// Study Instance UID (required).
    pub study_uid: String,
    /// Series Instance UID (required).
    pub series_uid: String,
    /// SOP Instance UID (required).
    pub object_uid: String,
    /// Response MIME type (e.g. `application/dicom`, `image/jpeg`).
    pub content_type: Option<String>,
    /// Transfer Syntax UID (only with `application/dicom`).
    pub transfer_syntax: Option<String>,
    /// Request de-identification (`anonymize=yes`).
    pub anonymize: bool,
    /// Burned-in annotation (`patient`, `technique`, or both).
    pub annotation: Option<String>,
    /// Rendered rows.
    pub rows: Option<u32>,
    /// Rendered columns.
    pub columns: Option<u32>,
    /// Rendered region as `x1,y1,x2,y2` fractions of the image.
    pub region: Option<String>,
    /// VOI window center (DS decimal string).
    pub window_center: Option<String>,
    /// VOI window width (DS decimal string).
    pub window_width: Option<String>,
    /// Rendered frame (1-based).
    pub frame_number: Option<u32>,
    /// Rendered image quality 1–100.
    pub image_quality: Option<u8>,
    /// Presentation State SOP Instance UID to apply.
    pub presentation_uid: Option<String>,
    /// Presentation State Series UID to apply.
    pub presentation_series_uid: Option<String>,
    /// Character set for text annotation (e.g. `ISO_IR 192`).
    pub charset: Option<String>,
}

impl WadoUriRequest {
    /// Request for one object; the rest stays optional.
    pub fn new(
        study_uid: impl Into<String>,
        series_uid: impl Into<String>,
        object_uid: impl Into<String>,
    ) -> Self {
        Self {
            study_uid: study_uid.into(),
            series_uid: series_uid.into(),
            object_uid: object_uid.into(),
            ..Self::default()
        }
    }
}

/// STOW-RS store response.
#[derive(Clone, Debug)]
pub struct StowResponse {
    /// HTTP status (200/202).
    pub status: u16,
    /// RetrieveURL when the server returned one.
    pub retrieve_url: Option<String>,
    /// Response content type.
    pub content_type: String,
    /// Raw response body.
    pub raw_body: Vec<u8>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::sync::Arc;

    /// The platform TLS config must verify against the OS store: a
    /// loopback server trusted only via `SSL_CERT_FILE` handshakes,
    /// and the same server without it is refused. (The verifier
    /// probes the store per config, so the two directions share one
    /// server thread but build separate configs.)
    #[test]
    fn https_verifies_against_platform_store() {
        let certified = rcgen::generate_simple_self_signed(vec!["127.0.0.1".into()]).unwrap();
        let ca_path = std::env::temp_dir().join(format!("vyna-web-tls-{}.pem", std::process::id()));
        std::fs::write(&ca_path, certified.cert.pem().as_bytes()).unwrap();

        let server_config = Arc::new(
            rustls::ServerConfig::builder()
                .with_no_client_auth()
                .with_single_cert(
                    vec![certified.cert.der().clone()],
                    rustls::pki_types::PrivateKeyDer::Pkcs8(
                        rustls::pki_types::PrivatePkcs8KeyDer::from(
                            certified.signing_key.serialize_der(),
                        ),
                    ),
                )
                .unwrap(),
        );
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            for _ in 0..2 {
                let Ok((tcp, _)) = listener.accept() else {
                    break;
                };
                tcp.set_read_timeout(Some(std::time::Duration::from_secs(10)))
                    .ok();
                let mut conn = rustls::ServerConnection::new(server_config.clone()).unwrap();
                let mut sock = tcp;
                let mut tls = rustls::Stream::new(&mut conn, &mut sock);
                let mut buf = [0u8; 5];
                // The untrusted attempt dies in the handshake; that
                // error is the expected negative, not a failure.
                if tls.read_exact(&mut buf).is_ok() {
                    let _ = tls.write_all(&buf);
                }
            }
        });

        let _ = rustls::crypto::ring::default_provider().install_default();
        let name: rustls::pki_types::ServerName<'static> = "127.0.0.1".try_into().unwrap();
        let round_trip = |config: Arc<rustls::ClientConfig>| {
            let mut conn = rustls::ClientConnection::new(config, name.clone()).unwrap();
            let sock = std::net::TcpStream::connect(addr).unwrap();
            sock.set_read_timeout(Some(std::time::Duration::from_secs(10)))
                .ok();
            let mut sock = sock;
            let mut tls = rustls::Stream::new(&mut conn, &mut sock);
            tls.write_all(b"hello")?;
            let mut buf = [0u8; 5];
            tls.read_exact(&mut buf)?;
            std::io::Result::Ok(buf)
        };

        // Trusted via the platform store hook: echo works.
        unsafe { std::env::set_var("SSL_CERT_FILE", &ca_path) };
        let trusted = DicomWebClient::platform_tls_config().unwrap();
        let echo = round_trip(Arc::new(trusted)).expect("trusted handshake failed");
        assert_eq!(&echo, b"hello");
        // Same server, no trust anchor: verification must refuse.
        unsafe { std::env::remove_var("SSL_CERT_FILE") };
        let untrusted = DicomWebClient::platform_tls_config().unwrap();
        assert!(
            round_trip(Arc::new(untrusted)).is_err(),
            "untrusted server must fail verification"
        );
        // The public constructor wires the same config.
        let client = DicomWebClient::new(format!("https://{addr}/x"))
            .unwrap()
            .with_timeout(std::time::Duration::from_secs(5));
        assert!(client.base_url.starts_with("https://"));

        server.join().unwrap();
        let _ = std::fs::remove_file(&ca_path);
    }

    fn params(req: &WadoUriRequest) -> Vec<(String, String)> {
        DicomWebClient::wado_uri_params(req)
            .unwrap()
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect()
    }

    #[test]
    fn wado_uri_minimal_params() {
        let p = params(&WadoUriRequest::new("1.2.1", "1.2.2", "1.2.3"));
        assert_eq!(
            p,
            vec![
                ("requestType".to_string(), "WADO".to_string()),
                ("studyUID".to_string(), "1.2.1".to_string()),
                ("seriesUID".to_string(), "1.2.2".to_string()),
                ("objectUID".to_string(), "1.2.3".to_string()),
            ]
        );
    }

    #[test]
    fn wado_uri_full_params() {
        let req = WadoUriRequest {
            content_type: Some("image/jpeg".into()),
            transfer_syntax: Some("1.2.840.10008.1.2.4.50".into()),
            anonymize: true,
            annotation: Some("patient,technique".into()),
            rows: Some(512),
            columns: Some(512),
            region: Some("0,0,1,1".into()),
            window_center: Some("40".into()),
            window_width: Some("400".into()),
            frame_number: Some(2),
            image_quality: Some(90),
            presentation_uid: Some("1.2.9".into()),
            presentation_series_uid: Some("1.2.10".into()),
            charset: Some("ISO_IR 192".into()),
            ..WadoUriRequest::new("1.2.1", "1.2.2", "1.2.3")
        };
        let p = params(&req);
        let get = |k: &str| p.iter().find(|(kk, _)| kk == k).map(|(_, v)| v.as_str());
        assert_eq!(get("requestType"), Some("WADO"));
        assert_eq!(get("contentType"), Some("image/jpeg"));
        assert_eq!(get("transferSyntax"), Some("1.2.840.10008.1.2.4.50"));
        assert_eq!(get("anonymize"), Some("yes"));
        assert_eq!(get("annotation"), Some("patient,technique"));
        assert_eq!(get("rows"), Some("512"));
        assert_eq!(get("region"), Some("0,0,1,1"));
        assert_eq!(get("windowCenter"), Some("40"));
        assert_eq!(get("frameNumber"), Some("2"));
        assert_eq!(get("imageQuality"), Some("90"));
        assert_eq!(get("presentationUID"), Some("1.2.9"));
        assert_eq!(get("presentationSeriesUID"), Some("1.2.10"));
        assert_eq!(get("charset"), Some("ISO_IR 192"));
    }

    #[test]
    fn bulkdata_auth_same_origin_only() {
        let base = "https://pacs.example/dicom-web";
        // Server-relative and same-origin absolute URIs keep auth.
        assert!(same_origin(
            "https://pacs.example/dicom-web/studies/1/bulk/2",
            base
        ));
        assert!(same_origin(
            "https://PACS.example:443/dicom-web/studies/1",
            "https://pacs.example:443/other"
        ));
        // Cross-origin absolute URIs must not carry credentials.
        assert!(!same_origin("https://evil.example/bulk", base));
        assert!(!same_origin(
            "http://pacs.example/dicom-web/studies/1",
            base
        ));
        assert!(!same_origin(
            "https://pacs.example:8443/dicom-web/studies/1",
            base
        ));
        assert!(!same_origin("https://pacs.example.evil.example/bulk", base));
        // Unparseable sides keep legacy behavior (send).
        assert!(same_origin("/studies/1/bulk/2", base));
        assert!(same_origin("studies/1", base));
    }

    #[test]
    fn wado_uri_anonymize_omitted_when_false() {
        let p = params(&WadoUriRequest::new("1.2.1", "1.2.2", "1.2.3"));
        assert!(!p.iter().any(|(k, _)| k == "anonymize"));
    }

    #[test]
    fn wado_uri_validation() {
        let bad_uids = WadoUriRequest::new("", "1.2.2", "1.2.3");
        assert!(DicomWebClient::wado_uri_params(&bad_uids).is_err());
        for quality in [0, 101] {
            let req = WadoUriRequest {
                image_quality: Some(quality),
                ..WadoUriRequest::new("1.2.1", "1.2.2", "1.2.3")
            };
            assert!(DicomWebClient::wado_uri_params(&req).is_err(), "{quality}");
        }
        for (rows, columns, frame) in [
            (Some(0), None, None),
            (None, Some(0), None),
            (None, None, Some(0)),
        ] {
            let req = WadoUriRequest {
                rows,
                columns,
                frame_number: frame,
                ..WadoUriRequest::new("1.2.1", "1.2.2", "1.2.3")
            };
            assert!(DicomWebClient::wado_uri_params(&req).is_err());
        }
    }

    #[test]
    fn dicom_part_body_accepts_parameters() {
        // A compliant server's transfer-syntax suffix must not hide
        // the part from wado_file.
        let parts = vec![
            Part::new("application/octet-stream", vec![9]),
            Part::new(
                "application/dicom; transfer-syntax=1.2.840.10008.1.2.1",
                vec![1, 2, 3],
            ),
        ];
        assert_eq!(
            DicomWebClient::dicom_part_body(parts).unwrap(),
            Bytes::from_static(&[1, 2, 3])
        );
        let none = vec![Part::new("application/octet-stream", vec![9])];
        assert!(DicomWebClient::dicom_part_body(none).is_err());
    }

    #[test]
    fn new_rejects_bad_base_urls() {
        assert!(DicomWebClient::new("http://localhost:8080").is_ok());
        for bad in [
            "",
            "   ",
            "not a url",
            "ftp://x/y",
            "gopher://x",
            "https://user:pass@pacs.example/dw",
            "https://user@pacs.example/dw",
            "http://h/x?y=1",
            "http://h/x#frag",
            "http://h/?limit=5",
        ] {
            let err = DicomWebClient::new(bad).unwrap_err();
            assert!(matches!(err, Error::BadRequest(_)), "{bad}: {err:?}");
        }
    }

    #[test]
    fn path_segment_encodes_reserved_chars() {
        assert_eq!(DicomWebClient::path_segment("1.2.3-4"), "1.2.3-4");
        assert_eq!(
            DicomWebClient::path_segment("1.2.3?limit=5"),
            "1.2.3%3Flimit%3D5"
        );
        assert_eq!(DicomWebClient::path_segment("a#b/c d"), "a%23b%2Fc%20d");
    }

    #[test]
    fn same_origin_normalizes_default_ports() {
        assert!(same_origin("https://h:443/y", "https://h/x"));
        assert!(same_origin("http://h/y", "http://h:80/x"));
        assert!(same_origin("https://h/y", "https://h/x"));
        assert!(!same_origin("https://h:8443/y", "https://h/x"));
        assert!(!same_origin("http://h/y", "https://h/x"));
        assert!(!same_origin("https://evil/y", "https://h/x"));
    }

    #[tokio::test]
    async fn bulkdata_rejects_own_userinfo() {
        let client = DicomWebClient::new("http://127.0.0.1:1").unwrap();
        // Rejected client-side before any fetch (no server needed).
        let err = client
            .wado_bulkdata("http://spy:s3cret@127.0.0.1:1/bulk/x")
            .await
            .unwrap_err();
        assert!(matches!(err, Error::BadRequest(_)), "{err:?}");
    }

    #[tokio::test]
    async fn empty_retrieves_fail_client_side() {
        let client = DicomWebClient::new("http://127.0.0.1:1").unwrap();
        let err = client
            .wado_frames("1", "1", "1", &[], "application/octet-stream")
            .await
            .unwrap_err();
        assert!(matches!(err, Error::BadRequest(_)), "{err:?}");
        let err = client.stow(None, &[]).await.unwrap_err();
        assert!(matches!(err, Error::BadRequest(_)), "{err:?}");
    }

    #[test]
    fn debug_redacts_credentials() {
        let bearer = format!("{:?}", Auth::Bearer("s3cret-token".into()));
        assert!(!bearer.contains("s3cret"), "{bearer}");
        let basic = format!(
            "{:?}",
            Auth::Basic {
                username: "ann".into(),
                password: "hunter2".into(),
            }
        );
        assert!(basic.contains("ann"), "{basic}");
        assert!(!basic.contains("hunter2"), "{basic}");
    }

    #[tokio::test]
    async fn stow_xml_fallback_extracts_retrieve_url() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        // Raw-socket stub (no hyper needed): answers the STOW POST with
        // the legacy XML store response instead of dicom+json.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (mut tcp, _) = listener.accept().await.unwrap();
            let mut head = vec![0u8; 65536];
            let mut have = 0usize;
            let body_len = loop {
                let n = tcp.read(&mut head[have..]).await.unwrap();
                have += n;
                let text = String::from_utf8_lossy(&head[..have]);
                if let Some(end) = text.find("\r\n\r\n") {
                    let len = text[..end]
                        .split("\r\n")
                        .find_map(|l| {
                            l.split_once(':').and_then(|(k, v)| {
                                (k.trim().eq_ignore_ascii_case("content-length"))
                                    .then(|| v.trim().parse::<usize>().unwrap_or(0))
                            })
                        })
                        .unwrap_or(0);
                    let mut left = len + end + 4 - have;
                    while left > 0 {
                        let mut sink = vec![0u8; left.min(65536)];
                        let n = tcp.read(&mut sink).await.unwrap();
                        if n == 0 {
                            break;
                        }
                        left -= n;
                    }
                    break len;
                }
            };
            assert!(body_len > 0, "stub got an empty STOW body");
            let xml = "<NativeDicomModel><RetrieveURL>http://stub/studies/1.2.9</RetrieveURL></NativeDicomModel>";
            let resp = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/dicom+xml\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{xml}",
                xml.len()
            );
            tcp.write_all(resp.as_bytes()).await.unwrap();
        });
        let client = DicomWebClient::new(format!("http://127.0.0.1:{port}")).unwrap();
        let stow = client.stow(None, &[vec![0u8; 8]]).await.unwrap();
        assert_eq!(stow.status, 200);
        assert_eq!(
            stow.retrieve_url.as_deref(),
            Some("http://stub/studies/1.2.9")
        );
    }

    #[tokio::test]
    async fn metadata_raw_preserves_bulk_links_parsed_drops_them() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        // Raw-socket stub: one metadata GET answered with a pixel
        // element that carries only a BulkDataURI (no inline Value).
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            for _ in 0..2 {
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
                let json = r#"[{"00100010":{"vr":"PN","Value":[{"Alphabetic":"Doe^Jane"}]},"7FE00010":{"vr":"OW","BulkDataURI":"http://127.0.0.1/bulk/abc"}}]"#;
                let resp = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/dicom+json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{json}",
                    json.len()
                );
                tcp.write_all(resp.as_bytes()).await.unwrap();
            }
        });
        let client = DicomWebClient::new(format!("http://127.0.0.1:{port}")).unwrap();
        let raw = client.wado_metadata_raw("1", "1", "1").await.unwrap();
        assert_eq!(
            raw["7FE00010"]["BulkDataURI"].as_str(),
            Some("http://127.0.0.1/bulk/abc")
        );
        // The parsed form drops the link (bulk arrives empty) — which
        // is why `--metadata` prints the raw JSON, not this.
        let parsed = client.wado_metadata("1", "1", "1").await.unwrap();
        assert!(parsed.bytes(vyna_dcm_core::tags::PIXEL_DATA).is_none());
    }

    #[tokio::test]
    async fn single_part_wrap_keeps_content_type_params() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        // Raw-socket stub: answers the WADO GET with a single-part
        // body carrying a transfer-syntax parameter.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (mut tcp, _) = listener.accept().await.unwrap();
            let mut head = vec![0u8; 4096];
            let mut have = 0usize;
            loop {
                let n = tcp.read(&mut head[have..]).await.unwrap();
                have += n;
                if String::from_utf8_lossy(&head[..have]).contains("\r\n\r\n") {
                    break;
                }
            }
            let body = b"PAYLOAD";
            let resp = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/dicom; transfer-syntax=1.2.840.10008.1.2.1\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                body.len()
            );
            tcp.write_all(resp.as_bytes()).await.unwrap();
            tcp.write_all(body).await.unwrap();
        });
        let client = DicomWebClient::new(format!("http://127.0.0.1:{port}")).unwrap();
        let parts = client
            .wado_parts("1.2.1", "1.2.2", "1.2.3", None)
            .await
            .unwrap();
        assert_eq!(parts.len(), 1);
        assert_eq!(
            parts[0].content_type,
            "application/dicom; transfer-syntax=1.2.840.10008.1.2.1"
        );
        assert!(is_dicom_media_type(&parts[0].content_type));
        assert_eq!(parts[0].body, Bytes::from_static(b"PAYLOAD"));
    }

    #[tokio::test]
    async fn wado_uri_dataset_wrong_mime_is_bad_request() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        // Raw-socket stub: answers the WADO-URI GET with an HTML
        // error page instead of application/dicom.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (mut tcp, _) = listener.accept().await.unwrap();
            let mut head = vec![0u8; 4096];
            let mut have = 0usize;
            loop {
                let n = tcp.read(&mut head[have..]).await.unwrap();
                have += n;
                if String::from_utf8_lossy(&head[..have]).contains("\r\n\r\n") {
                    break;
                }
            }
            let body = b"<html>gone</html>";
            let resp = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: text/html\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                body.len()
            );
            tcp.write_all(resp.as_bytes()).await.unwrap();
            tcp.write_all(body).await.unwrap();
        });
        let client = DicomWebClient::new(format!("http://127.0.0.1:{port}")).unwrap();
        let req = WadoUriRequest::new("1.2.1", "1.2.2", "1.2.3");
        let err = client.wado_uri_dataset(&req).await.unwrap_err();
        assert!(matches!(err, Error::BadRequest(_)), "{err:?}");
    }

    #[tokio::test]
    async fn transport_timeout_is_typed() {
        // Stub accepts and never answers: the request must fail with
        // the retryable Timeout variant — proving `with_timeout`
        // applies end to end (it stores the budget; builders use it).
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let Ok((_tcp, _)) = listener.accept().await else {
                return;
            };
            tokio::time::sleep(std::time::Duration::from_secs(30)).await;
        });
        let client = DicomWebClient::new(format!("http://127.0.0.1:{port}"))
            .unwrap()
            .with_timeout(std::time::Duration::from_millis(200));
        let t0 = std::time::Instant::now();
        let err = client.search_studies(&[]).await.unwrap_err();
        assert!(
            t0.elapsed() < std::time::Duration::from_secs(10),
            "timeout did not apply: {:?}",
            t0.elapsed()
        );
        assert!(matches!(err, Error::Timeout(_)), "{err:?}");
    }

    #[tokio::test]
    async fn transport_refused_is_connect() {
        // Nothing listens: a connect-level failure, not stringly Http.
        let probe = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = probe.local_addr().unwrap().port();
        drop(probe);
        let client = DicomWebClient::new(format!("http://127.0.0.1:{port}")).unwrap();
        let err = client.search_studies(&[]).await.unwrap_err();
        assert!(matches!(err, Error::Connect(_)), "{err:?}");
    }

    #[tokio::test]
    async fn transport_tls_failure_is_typed() {
        // Plain-HTTP stub hit over https://: the handshake dies and
        // must surface as Tls — not Connect (a failed handshake also
        // reports `is_connect`) and not stringly Http.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let Ok((mut tcp, _)) = listener.accept().await else {
                return;
            };
            let mut buf = vec![0u8; 4096];
            if tcp.read(&mut buf).await.unwrap_or(0) > 0 {
                let _ = tcp
                    .write_all(
                        b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\nconnection: close\r\n\r\nno",
                    )
                    .await;
            }
        });
        let client = DicomWebClient::new(format!("https://127.0.0.1:{port}")).unwrap();
        let err = client.search_studies(&[]).await.unwrap_err();
        assert!(matches!(err, Error::Tls(_)), "{err:?}");
    }

    /// Minimal TLS+HTTP stub: handshakes with `config`, reads one
    /// request head per connection, answers `body`. Serves `n`
    /// connections, then stops.
    fn https_stub(
        config: Arc<rustls::ServerConfig>,
        n: usize,
        body: Vec<u8>,
    ) -> (std::net::SocketAddr, std::thread::JoinHandle<()>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = std::thread::spawn(move || {
            for _ in 0..n {
                let Ok((tcp, _)) = listener.accept() else {
                    break;
                };
                tcp.set_read_timeout(Some(std::time::Duration::from_secs(10)))
                    .ok();
                let mut conn = match rustls::ServerConnection::new(config.clone()) {
                    Ok(c) => c,
                    Err(_) => break,
                };
                let mut sock = tcp;
                let mut tls = rustls::Stream::new(&mut conn, &mut sock);
                // A client without the requested identity dies in the
                // handshake; that error is the expected negative.
                let mut head = Vec::new();
                let mut byte = [0u8; 1];
                let mut ok = false;
                for _ in 0..65536 {
                    match tls.read_exact(&mut byte) {
                        Ok(()) => {
                            head.push(byte[0]);
                            if head.ends_with(b"\r\n\r\n") {
                                ok = true;
                                break;
                            }
                        }
                        Err(_) => break,
                    }
                }
                if !ok {
                    continue;
                }
                let resp = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/dicom+json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                    body.len()
                );
                let _ = tls.write_all(resp.as_bytes());
                let _ = tls.write_all(&body);
            }
        });
        (addr, handle)
    }

    fn server_config(
        cert_pem: &[u8],
        key_pem: &[u8],
        client_ca_pem: Option<&[u8]>,
    ) -> Arc<rustls::ServerConfig> {
        use std::io::BufReader;
        let _ = rustls::crypto::ring::default_provider().install_default();
        let certs: Vec<_> = rustls_pemfile::certs(&mut BufReader::new(cert_pem))
            .collect::<std::result::Result<_, _>>()
            .unwrap();
        let key = rustls_pemfile::private_key(&mut BufReader::new(key_pem))
            .unwrap()
            .unwrap();
        let config = match client_ca_pem {
            None => rustls::ServerConfig::builder()
                .with_no_client_auth()
                .with_single_cert(certs, key)
                .unwrap(),
            Some(ca) => {
                let mut roots = rustls::RootCertStore::empty();
                for cert in rustls_pemfile::certs(&mut BufReader::new(ca)) {
                    roots.add(cert.unwrap()).unwrap();
                }
                let verifier = rustls::server::WebPkiClientVerifier::builder(Arc::new(roots))
                    .build()
                    .unwrap();
                rustls::ServerConfig::builder()
                    .with_client_cert_verifier(verifier)
                    .with_single_cert(certs, key)
                    .unwrap()
            }
        };
        Arc::new(config)
    }

    #[tokio::test]
    async fn tls_private_ca_verifies_over_wire() {
        let certified = rcgen::generate_simple_self_signed(vec!["127.0.0.1".into()]).unwrap();
        let ca_pem = certified.cert.pem().into_bytes();
        let key_pem = certified.signing_key.serialize_pem().into_bytes();
        let (addr, server) = https_stub(server_config(&ca_pem, &key_pem, None), 1, b"[]".to_vec());
        let client = DicomWebClient::new(format!("https://{addr}"))
            .unwrap()
            .with_private_ca_pem(&ca_pem)
            .unwrap();
        let studies = client.search_studies(&[]).await.unwrap();
        assert!(studies.is_empty());
        server.join().unwrap();
    }

    #[tokio::test]
    async fn tls_extra_ca_verifies_over_wire() {
        let certified = rcgen::generate_simple_self_signed(vec!["127.0.0.1".into()]).unwrap();
        let ca_pem = certified.cert.pem().into_bytes();
        let key_pem = certified.signing_key.serialize_pem().into_bytes();
        let (addr, server) = https_stub(server_config(&ca_pem, &key_pem, None), 1, b"[]".to_vec());
        let client = DicomWebClient::new(format!("https://{addr}"))
            .unwrap()
            .with_extra_ca_pem(&ca_pem)
            .unwrap();
        let studies = client.search_studies(&[]).await.unwrap();
        assert!(studies.is_empty());
        server.join().unwrap();
    }

    #[tokio::test]
    async fn tls_untrusted_server_is_typed_error() {
        let certified = rcgen::generate_simple_self_signed(vec!["127.0.0.1".into()]).unwrap();
        let ca_pem = certified.cert.pem().into_bytes();
        let key_pem = certified.signing_key.serialize_pem().into_bytes();
        // Two handshakes: default trust, then a *wrong* private CA.
        let (addr, server) = https_stub(server_config(&ca_pem, &key_pem, None), 2, b"[]".to_vec());
        let client = DicomWebClient::new(format!("https://{addr}")).unwrap();
        let err = client.search_studies(&[]).await.unwrap_err();
        assert!(matches!(err, Error::Tls(_)), "{err:?}");
        let other = rcgen::generate_simple_self_signed(vec!["127.0.0.1".into()]).unwrap();
        let client = DicomWebClient::new(format!("https://{addr}"))
            .unwrap()
            .with_private_ca_pem(other.cert.pem().as_bytes())
            .unwrap();
        let err = client.search_studies(&[]).await.unwrap_err();
        assert!(matches!(err, Error::Tls(_)), "{err:?}");
        server.join().unwrap();
    }

    #[tokio::test]
    async fn tls_client_identity_mtls_round_trip() {
        let server_cert = rcgen::generate_simple_self_signed(vec!["127.0.0.1".into()]).unwrap();
        let server_pem = server_cert.cert.pem().into_bytes();
        let server_key = server_cert.signing_key.serialize_pem().into_bytes();
        let client_cert = rcgen::generate_simple_self_signed(vec!["client".into()]).unwrap();
        let client_pem = client_cert.cert.pem().into_bytes();
        let client_key = client_cert.signing_key.serialize_pem().into_bytes();
        // Two handshakes: without, then with, the client identity.
        let (addr, server) = https_stub(
            server_config(&server_pem, &server_key, Some(&client_pem)),
            2,
            b"[]".to_vec(),
        );
        // No identity: the server-mandated handshake fails.
        let bare = DicomWebClient::new(format!("https://{addr}"))
            .unwrap()
            .with_private_ca_pem(&server_pem)
            .unwrap();
        let err = bare.search_studies(&[]).await.unwrap_err();
        assert!(matches!(err, Error::Tls(_)), "{err:?}");
        // With identity: the round trip succeeds.
        let mtls = DicomWebClient::new(format!("https://{addr}"))
            .unwrap()
            .with_private_ca_pem(&server_pem)
            .unwrap()
            .with_client_identity_pem(&client_pem, &client_key)
            .unwrap();
        assert!(mtls.search_studies(&[]).await.unwrap().is_empty());
        server.join().unwrap();
    }

    #[test]
    fn tls_builders_reject_garbage_pem() {
        for bad in [b"".as_slice(), b"not a pem at all"] {
            let err = DicomWebClient::new("https://127.0.0.1:1")
                .unwrap()
                .with_private_ca_pem(bad)
                .unwrap_err();
            assert!(matches!(err, Error::Tls(_)), "{err:?}");
            let err = DicomWebClient::new("https://127.0.0.1:1")
                .unwrap()
                .with_extra_ca_pem(bad)
                .unwrap_err();
            assert!(matches!(err, Error::Tls(_)), "{err:?}");
        }
        let good = rcgen::generate_simple_self_signed(vec!["client".into()]).unwrap();
        let cert_pem = good.cert.pem().into_bytes();
        let key_pem = good.signing_key.serialize_pem().into_bytes();
        // Identity missing its key, or its certs, is refused.
        for (cert, key) in [
            (cert_pem.as_slice(), b"".as_slice()),
            (b"".as_slice(), key_pem.as_slice()),
        ] {
            let err = DicomWebClient::new("https://127.0.0.1:1")
                .unwrap()
                .with_client_identity_pem(cert, key)
                .unwrap_err();
            assert!(matches!(err, Error::Tls(_)), "{err:?}");
        }
        // The pair together builds.
        assert!(
            DicomWebClient::new("https://127.0.0.1:1")
                .unwrap()
                .with_client_identity_pem(&cert_pem, &key_pem)
                .is_ok()
        );
        // Debug never prints CA/key material.
        let shown = format!(
            "{:?}",
            DicomWebClient::new("https://127.0.0.1:1")
                .unwrap()
                .with_extra_ca_pem(&cert_pem)
                .unwrap()
                .with_client_identity_pem(&cert_pem, &key_pem)
                .unwrap()
        );
        assert!(!shown.contains("BEGIN"), "{shown}");
        assert!(shown.contains("<redacted>"), "{shown}");
    }
}
