//! DICOMweb client: QIDO-RS, WADO-RS, STOW-RS (NEMA PS3.18 §8).

use vyna_dcm_codec::{FileMeta, dataset_from_json, read_part10};
use vyna_dcm_core::DataSet;

use crate::{
    Error, Result,
    multipart::{Part, decode_multipart, encode_multipart, new_boundary, parse_content_type},
};

/// Client authentication.
#[derive(Clone, Debug)]
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
    pub fn new(base_url: impl Into<String>) -> Result<Self> {
        // reqwest 0.13 `rustls-no-provider` needs the app to install a
        // provider; ignore failure (another ring install already done).
        let _ = rustls::crypto::ring::default_provider().install_default();
        let http = reqwest::Client::builder()
            .build()
            .map_err(|e| Error::Http(e.to_string()))?;
        Ok(Self {
            http,
            base_url: base_url.into().trim_end_matches('/').to_string(),
            auth: None,
        })
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
        parts
            .into_iter()
            .find(|p| p.content_type.eq_ignore_ascii_case("application/dicom"))
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
    pub async fn wado_bulkdata(&self, uri: &str) -> Result<(String, Vec<u8>)> {
        let url = if uri.starts_with("http://") || uri.starts_with("https://") {
            uri.to_string()
        } else if uri.starts_with('/') {
            format!("{}{uri}", self.base_url)
        } else {
            format!("{}/{uri}", self.base_url)
        };
        let resp = self
            .get(url)
            .send()
            .await
            .map_err(|e| Error::Http(e.to_string()))?;
        Self::expect_ok(resp).await
    }

    // -- STOW-RS ---------------------------------------------------------

    /// Store instances (`POST /studies[/{study_uid}]`).
    ///
    /// Each file must be a Part-10 byte buffer. Returns the parsed
    /// RetrieveURL plus the raw response.
    pub async fn stow(&self, study_uid: Option<&str>, files: &[Vec<u8>]) -> Result<StowResponse> {
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
        let status = resp.status().as_u16();
        let ct = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        let raw = resp
            .bytes()
            .await
            .map_err(|e| Error::Http(e.to_string()))?
            .to_vec();
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
                // XML form: <RetrieveURL>...</RetrieveURL>
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

    async fn expect_ok(resp: reqwest::Response) -> Result<(String, Vec<u8>)> {
        let status = resp.status().as_u16();
        let ct = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        let body = resp
            .bytes()
            .await
            .map_err(|e| Error::Http(e.to_string()))?
            .to_vec();
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
        let status = resp.status().as_u16();
        let ct = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        let body = resp
            .bytes()
            .await
            .map_err(|e| Error::Http(e.to_string()))?
            .to_vec();
        if !(200..300).contains(&status) {
            return Err(Error::Status {
                status,
                body: String::from_utf8_lossy(&body).chars().take(300).collect(),
            });
        }
        // Single-part responses (e.g. one frame as image/jpeg) have no
        // multipart framing; wrap them as one part.
        let (mime, params) = parse_content_type(&ct);
        if mime == "multipart/related" {
            let boundary = params
                .get("boundary")
                .ok_or_else(|| Error::Multipart("missing boundary".into()))?;
            Ok(decode_multipart(&body, boundary)?)
        } else {
            Ok(vec![Part::new(mime, body)])
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
