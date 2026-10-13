//! DICOMweb client: QIDO-RS, WADO-RS, STOW-RS (NEMA PS3.18 §8).

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

/// Lowercased `scheme://authority` of an absolute HTTP(S) URL, or
/// `None` when the URL is relative or not HTTP(S).
fn origin_of(url: &str) -> Option<String> {
    let (scheme, rest) = url
        .strip_prefix("https://")
        .map(|r| ("https", r))
        .or_else(|| url.strip_prefix("http://").map(|r| ("http", r)))?;
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    if authority.is_empty() {
        return None;
    }
    // Lowercasing the whole authority is safe (host is
    // case-insensitive, port/userinfo are matched verbatim anyway).
    Some(format!("{}://{}", scheme, authority.to_ascii_lowercase()))
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

/// DICOMweb client over a service base URL like
/// `https://pacs.example/dicom-web`.
#[derive(Clone, Debug)]
pub struct DicomWebClient {
    http: reqwest::Client,
    base_url: String,
    auth: Option<Auth>,
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
    /// Basic-auth headers).
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
        // reqwest 0.13 `rustls-no-provider` needs the app to install a
        // provider; ignore failure (another ring install already done).
        let _ = rustls::crypto::ring::default_provider().install_default();
        // A hung server must not hang us forever; bulk transfers
        // can legitimately take minutes, hence the budget.
        let http = Self::build_http(std::time::Duration::from_secs(300))?;
        Ok(Self {
            http,
            base_url: base,
            auth: None,
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
            .map_err(|e| Error::Http(format!("platform TLS verifier: {e}")))
            .map(|b| b.with_no_client_auth())
    }

    /// Shared HTTP engine (also used by [`Self::with_timeout`], which
    /// rebuilds the immutable reqwest client).
    fn build_http(timeout: std::time::Duration) -> Result<reqwest::Client> {
        reqwest::Client::builder()
            .timeout(timeout)
            .use_preconfigured_tls(Self::platform_tls_config()?)
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
    #[must_use]
    pub fn with_timeout(mut self, timeout: std::time::Duration) -> Self {
        // `reqwest` clients are immutable once built; rebuild with
        // the same settings plus the new timeout.
        match Self::build_http(timeout) {
            Ok(http) => self.http = http,
            Err(e) => tracing::warn!("client rebuild for timeout failed ({e}); keeping old client"),
        }
        self
    }

    fn get(&self, url: String) -> reqwest::RequestBuilder {
        let mut req = self.http.get(url);
        req = self.apply_auth(req);
        req
    }

    fn post(&self, url: String) -> reqwest::RequestBuilder {
        self.apply_auth(self.http.post(url))
    }

    fn apply_auth(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match &self.auth {
            Some(Auth::Bearer(t)) => req.bearer_auth(t),
            Some(Auth::Basic { username, password }) => req.basic_auth(username, Some(password)),
            None => req,
        }
    }

    // -- QIDO-RS ---------------------------------------------------------

    /// Generic QIDO-RS search over `resource` (`studies`,
    /// `studies/{uid}/series`, `studies/{uid}/series/{uid}/instances`, ...).
    pub async fn qido(&self, resource: &str, query: &[(&str, &str)]) -> Result<Vec<DataSet>> {
        let url = format!("{}/{}", self.base_url, resource.trim_start_matches('/'));
        let resp = self
            .get(url)
            .header("Accept", "application/dicom+json")
            .query(query)
            .send()
            .await
            .map_err(|e| Error::Http(e.to_string()))?;
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
        self.qido(&format!("studies/{study_uid}/series"), query)
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
            &format!("studies/{study_uid}/series/{series_uid}/instances"),
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
            "{}/studies/{study_uid}/series/{series_uid}/instances/{instance_uid}",
            self.base_url
        );
        let resp = self
            .get(url)
            .header(
                "Accept",
                accept.unwrap_or("multipart/related; type=\"application/dicom\""),
            )
            .send()
            .await
            .map_err(|e| Error::Http(e.to_string()))?;
        Self::expect_multipart(resp).await
    }

    /// Retrieve an instance's first `application/dicom` part as raw bytes.
    pub async fn wado_file(
        &self,
        study_uid: &str,
        series_uid: &str,
        instance_uid: &str,
    ) -> Result<Vec<u8>> {
        let parts = self
            .wado_parts(study_uid, series_uid, instance_uid, None)
            .await?;
        Self::dicom_part_body(parts)
    }

    /// First `application/dicom` part body. Media-type match, not
    /// string equality: compliant servers add parameters such as
    /// `transfer-syntax` (PS3.18 §8.6).
    fn dicom_part_body(parts: Vec<Part>) -> Result<Vec<u8>> {
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
            "{}/studies/{study_uid}/series/{series_uid}/instances/{instance_uid}/metadata",
            self.base_url
        );
        let resp = self
            .get(url)
            .header("Accept", "application/dicom+json")
            .send()
            .await
            .map_err(|e| Error::Http(e.to_string()))?;
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
            "{}/studies/{study_uid}/series/{series_uid}/instances/{instance_uid}/frames/{list}",
            self.base_url
        );
        let resp = self
            .get(url)
            .header("Accept", accept)
            .send()
            .await
            .map_err(|e| Error::Http(e.to_string()))?;
        Self::expect_multipart(resp).await
    }

    /// Fetch a BulkDataURI (absolute URL or server-relative path).
    ///
    /// Absolute URIs on another origin are fetched *without* our
    /// credentials: BulkDataURIs are server-supplied and may point
    /// anywhere, so forwarding `Authorization` would leak it (SSRF).
    pub async fn wado_bulkdata(&self, uri: &str) -> Result<(String, Vec<u8>)> {
        let url = if uri.starts_with("http://") || uri.starts_with("https://") {
            uri.to_string()
        } else if uri.starts_with('/') {
            format!("{}{uri}", self.base_url)
        } else {
            format!("{}/{uri}", self.base_url)
        };
        // Same-origin (including server-relative URIs resolved above)
        // keeps auth; cross-origin absolute URIs must not carry it.
        let same = same_origin(&url, &self.base_url);
        let req = self.http.get(url);
        let req = if same { self.apply_auth(req) } else { req };
        let resp = req.send().await.map_err(|e| Error::Http(e.to_string()))?;
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
            .map_err(|e| Error::Http(e.to_string()))?;
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
            Some(u) => format!("{}/studies/{u}", self.base_url),
            None => format!("{}/studies", self.base_url),
        };
        let parts: Vec<Part> = files
            .iter()
            .map(|f| Part::new("application/dicom", f.clone()))
            .collect();
        let boundary = new_boundary();
        let (content_type, body) = encode_multipart(&parts, &boundary, "application/dicom");
        let resp = self
            .post(url)
            .header("Content-Type", content_type)
            .header("Accept", "application/dicom+json")
            .body(body)
            .send()
            .await
            .map_err(|e| Error::Http(e.to_string()))?;
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
        while let Some(chunk) = resp.chunk().await.map_err(|e| Error::Http(e.to_string()))? {
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
            vec![1, 2, 3]
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
        ] {
            let err = DicomWebClient::new(bad).unwrap_err();
            assert!(matches!(err, Error::BadRequest(_)), "{bad}: {err:?}");
        }
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
        assert_eq!(parts[0].body, b"PAYLOAD");
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
}
