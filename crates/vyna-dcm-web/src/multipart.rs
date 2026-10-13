//! Minimal `multipart/related` codec (RFC 2046, PS3.18 §8.7), from scratch.
//!
//! Boundaries are matched only at line starts so binary bodies containing
//! boundary-like bytes do not confuse the parser.

use std::collections::HashMap;

use crate::{Error, Result};

/// One body part.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Part {
    /// Part `Content-Type` (default `application/octet-stream`).
    pub content_type: String,
    /// Optional `Content-Location`.
    pub content_location: Option<String>,
    /// All part headers in order.
    pub headers: Vec<(String, String)>,
    /// Raw body bytes.
    pub body: Vec<u8>,
}

impl Part {
    /// Build a part with a content type and body.
    pub fn new(content_type: impl Into<String>, body: Vec<u8>) -> Self {
        let content_type = content_type.into();
        Self {
            headers: vec![("Content-Type".into(), content_type.clone())],
            content_type,
            content_location: None,
            body,
        }
    }

    /// Attach a `Content-Location` header.
    #[must_use]
    pub fn with_location(mut self, location: impl Into<String>) -> Self {
        let location = location.into();
        self.headers
            .push(("Content-Location".into(), location.clone()));
        self.content_location = Some(location);
        self
    }
}

/// Generate a random boundary.
#[must_use]
pub fn new_boundary() -> String {
    format!("vynadcm-{}", uuid::Uuid::new_v4().as_simple())
}

/// Encode parts; returns the `Content-Type` header value and the body.
#[must_use]
pub fn encode_multipart(parts: &[Part], boundary: &str, root_type: &str) -> (String, Vec<u8>) {
    let content_type = format!("multipart/related; type=\"{root_type}\"; boundary=\"{boundary}\"");
    let mut body = Vec::new();
    for part in parts {
        body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
        for (name, value) in &part.headers {
            body.extend_from_slice(format!("{name}: {value}\r\n").as_bytes());
        }
        // Ensure a Content-Type header exists even if the caller built the
        // part headers manually.
        if !part
            .headers
            .iter()
            .any(|(n, _)| n.eq_ignore_ascii_case("content-type"))
        {
            body.extend_from_slice(format!("Content-Type: {}\r\n", part.content_type).as_bytes());
        }
        body.extend_from_slice(b"\r\n");
        body.extend_from_slice(&part.body);
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    (content_type, body)
}

/// Maximum parts accepted by [`decode_multipart`]: delimiter marks
/// and part headers are attacker-controlled, so an unbounded scan
/// turns a 1 GiB body into gigabytes of bookkeeping.
const MAX_PARTS: usize = 100_000;

/// Decode a multipart body framed by `boundary` (without quotes/dashes).
pub fn decode_multipart(body: &[u8], boundary: &str) -> Result<Vec<Part>> {
    let needle = format!("--{boundary}").into_bytes();
    // Delimiter positions accepted only at a line start.
    let mut marks: Vec<usize> = Vec::new();
    let mut i = 0;
    while i + needle.len() <= body.len() {
        if body[i..].starts_with(&needle)
            && (i == 0 || body[..i].ends_with(b"\r\n") || body[..i].ends_with(b"\n"))
        {
            marks.push(i);
            // +1: the closing delimiter is a mark but not a part.
            if marks.len() > MAX_PARTS + 1 {
                return Err(Error::Multipart(format!("too many parts (>{MAX_PARTS})")));
            }
            i += needle.len();
        } else {
            i += 1;
        }
    }
    if marks.is_empty() {
        return Err(Error::Multipart("no boundary delimiters found".into()));
    }
    // The body must end with the closing delimiter: without it the
    // payload is truncated, and returning partial (or, for a single
    // mark, empty) parts would silently corrupt STOW/WADO receives.
    // Anything past the first closing mark is epilogue, ignored.
    let Some(close_idx) = marks
        .iter()
        .position(|m| body[m + needle.len()..].starts_with(b"--"))
    else {
        return Err(Error::Multipart("missing closing delimiter".into()));
    };
    let marks = &marks[..=close_idx];
    let mut parts = Vec::new();
    for w in marks.windows(2) {
        let (start, end) = (w[0], w[1]);
        let mut chunk = &body[start + needle.len()..end];
        if chunk.starts_with(b"--") {
            break; // closing delimiter reached early
        }
        chunk = trim_crlf(chunk);
        // Split headers from body at the blank line.
        let (head, payload) = match find_double_crlf(chunk) {
            Some(p) => {
                let rest = &chunk[p..];
                let start = if rest.starts_with(b"\r\n\r\n") {
                    p + 4
                } else {
                    p + 2
                };
                (&chunk[..p], trim_trailing_crlf(&chunk[start..]))
            }
            None => (&[][..], trim_trailing_crlf(chunk)),
        };
        let headers = parse_headers(head)?;
        parts.push(part_from_headers(headers, payload.to_vec()));
    }
    Ok(parts)
}

fn trim_crlf(mut b: &[u8]) -> &[u8] {
    if let Some(s) = b.strip_prefix(b"\r\n") {
        b = s;
    } else if let Some(s) = b.strip_prefix(b"\n") {
        b = s;
    }
    b
}

fn trim_trailing_crlf(mut b: &[u8]) -> &[u8] {
    if let Some(s) = b.strip_suffix(b"\r\n") {
        b = s;
    } else if let Some(s) = b.strip_suffix(b"\n") {
        b = s;
    }
    b
}

fn find_double_crlf(b: &[u8]) -> Option<usize> {
    if let Some(p) = find_sub(b, b"\r\n\r\n") {
        return Some(p);
    }
    find_sub(b, b"\n\n")
}

fn find_sub(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// Maximum headers per part: real MIME parts carry a handful; an
/// unbounded count lets a header-dense body inflate past its wire
/// size by multiples in `Vec<String>` overhead.
const MAX_PART_HEADERS: usize = 100;

fn parse_headers(head: &[u8]) -> Result<Vec<(String, String)>> {
    if head.is_empty() {
        return Ok(Vec::new());
    }
    let text =
        std::str::from_utf8(head).map_err(|_| Error::Multipart("part headers not ASCII".into()))?;
    let mut out = Vec::new();
    for line in text.split("\r\n").flat_map(|l| l.split('\n')) {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if out.len() >= MAX_PART_HEADERS {
            return Err(Error::Multipart(format!(
                "too many part headers (>{MAX_PART_HEADERS})"
            )));
        }
        let (name, value) = line
            .split_once(':')
            .ok_or_else(|| Error::Multipart(format!("bad header {line:?}")))?;
        out.push((name.trim().to_string(), value.trim().to_string()));
    }
    Ok(out)
}

fn part_from_headers(headers: Vec<(String, String)>, body: Vec<u8>) -> Part {
    let content_type = headers
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case("content-type"))
        .map(|(_, v)| v.clone())
        .unwrap_or_else(|| "application/octet-stream".to_string());
    let content_location = headers
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case("content-location"))
        .map(|(_, v)| v.clone());
    Part {
        content_type,
        content_location,
        headers,
        body,
    }
}

/// Split a `Content-Type` header into mime type + lowercase parameters
/// (quotes stripped).
#[must_use]
pub fn parse_content_type(header: &str) -> (String, HashMap<String, String>) {
    let mut parts = header.split(';');
    let mime = parts.next().unwrap_or("").trim().to_ascii_lowercase();
    let mut params = HashMap::new();
    for p in parts {
        if let Some((k, v)) = p.split_once('=') {
            let v = v.trim().trim_matches('"').to_string();
            params.insert(k.trim().to_ascii_lowercase(), v);
        }
    }
    (mime, params)
}

/// Whether a part Content-Type names `application/dicom`, ignoring
/// parameters (PS3.18 permits e.g. `transfer-syntax=...`) and case.
#[must_use]
pub fn is_dicom_media_type(header: &str) -> bool {
    parse_content_type(header).0 == "application/dicom"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_binary_parts() {
        let parts = vec![
            Part::new("application/dicom", vec![0, 1, 2, 3, 255, 254]),
            Part::new("application/pdf", b"%PDF-1.4".to_vec()).with_location("/bulk/1"),
        ];
        let (ct, body) = encode_multipart(&parts, "BOUNDARY42", "application/dicom");
        assert!(ct.contains("multipart/related"), "{ct}");
        let back = decode_multipart(&body, "BOUNDARY42").unwrap();
        assert_eq!(back, parts);
    }

    #[test]
    fn tolerates_boundary_like_bytes_inside_body() {
        // Boundary text inside the body but NOT at a line start.
        let body = b"--B\r\nContent-Type: application/octet-stream\r\n\r\nxx--Byy\r\n--B--\r\n";
        let parts = decode_multipart(body, "B").unwrap();
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0].body, b"xx--Byy");
    }

    #[test]
    fn parses_content_type_params() {
        let (mime, p) =
            parse_content_type("multipart/related; type=\"application/dicom\"; boundary=abc");
        assert_eq!(mime, "multipart/related");
        assert_eq!(p["type"], "application/dicom");
        assert_eq!(p["boundary"], "abc");
    }

    #[test]
    fn rejects_missing_boundary() {
        assert!(decode_multipart(b"no delimiters here", "B").is_err());
    }

    #[test]
    fn rejects_unclosed_body() {
        // Truncated after the first boundary: no closing delimiter.
        let cut = b"--B\r\nContent-Type: application/octet-stream\r\n\r\nxx";
        assert!(decode_multipart(cut, "B").is_err());
        // Even a complete-looking part without the close is refused.
        let cut = b"--B\r\nContent-Type: text/plain\r\n\r\nxx\r\n--B\r\nContent-Type: text/plain\r\n\r\nyy";
        assert!(decode_multipart(cut, "B").is_err());
        // Epilogue past the closing delimiter stays ignored.
        let ok = b"--B\r\nContent-Type: text/plain\r\n\r\nxx\r\n--B--\r\ntrailing junk";
        assert_eq!(decode_multipart(ok, "B").unwrap().len(), 1);
    }

    #[test]
    fn rejects_part_flood() {
        let mut body = Vec::new();
        for _ in 0..=MAX_PARTS + 1 {
            body.extend_from_slice(b"--B\r\n\r\nx\r\n");
        }
        body.extend_from_slice(b"--B--\r\n");
        let err = decode_multipart(&body, "B").unwrap_err();
        assert!(format!("{err:?}").contains("too many parts"), "{err:?}");
    }

    #[test]
    fn rejects_header_flood() {
        let mut body = b"--B\r\n".to_vec();
        for i in 0..=MAX_PART_HEADERS {
            body.extend_from_slice(format!("X-H{i}: v\r\n").as_bytes());
        }
        body.extend_from_slice(b"\r\nx\r\n--B--\r\n");
        let err = decode_multipart(&body, "B").unwrap_err();
        assert!(
            format!("{err:?}").contains("too many part headers"),
            "{err:?}"
        );
    }

    #[test]
    fn dicom_media_type_ignores_params_and_case() {
        assert!(is_dicom_media_type("application/dicom"));
        assert!(is_dicom_media_type(
            "application/dicom; transfer-syntax=1.2.840.10008.1.2.1"
        ));
        assert!(is_dicom_media_type(
            "Application/DICOM; TRANSFER-SYNTAX=\"1.2.840.10008.1.2.4.50\""
        ));
        assert!(!is_dicom_media_type("application/octet-stream"));
        assert!(!is_dicom_media_type("application/dicom+json"));
        assert!(!is_dicom_media_type(""));
    }
}
