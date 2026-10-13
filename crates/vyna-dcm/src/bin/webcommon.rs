//! Shared CLI plumbing for the `vyna-dcm` DICOMweb tools.
#![allow(dead_code)] // each binary uses a subset

use std::path::{Path, PathBuf};

use vyna_dcm::web::DicomWebClient;

/// Server URL + auth arguments shared by the client tools.
#[derive(clap::Args, Debug)]
pub struct WebArgs {
    /// DICOMweb base URL (e.g. `http://host:8080/dicomweb`).
    #[arg(long)]
    pub url: String,
    /// Bearer token auth.
    #[arg(long)]
    pub bearer: Option<String>,
    /// HTTP Basic auth as `user:password`.
    #[arg(long)]
    pub basic: Option<String>,
}

impl WebArgs {
    /// Build the client with the requested auth.
    pub fn client(&self) -> Result<DicomWebClient, String> {
        if self.bearer.is_some() && self.basic.is_some() {
            return Err("--bearer and --basic are exclusive".to_string());
        }
        let client = DicomWebClient::new(&self.url).map_err(|e| e.to_string())?;
        if let Some(token) = &self.bearer {
            return Ok(client.with_bearer(token));
        }
        if let Some(basic) = &self.basic {
            let Some((user, pass)) = basic.split_once(':') else {
                return Err("--basic wants user:password".to_string());
            };
            return Ok(client.with_basic(user, pass));
        }
        Ok(client)
    }
}

/// Collect `*.dcm` files under `dir`, recursively.
pub fn collect_dcm_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    collect_dir(dir, &mut out);
    out.sort();
    out
}

fn collect_dir(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        // Never follow symlinks: a looped link would recurse forever
        // (and any link escapes the served root).
        let Ok(ft) = entry.file_type() else {
            continue;
        };
        if ft.is_symlink() {
            continue;
        }
        let path = entry.path();
        if ft.is_dir() {
            collect_dir(&path, out);
        } else if path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("dcm"))
        {
            out.push(path);
        }
    }
}

/// Split `KEY=value` pairs (empty value = return key).
pub fn split_keys(keys: &[String]) -> Result<Vec<(String, String)>, String> {
    keys.iter()
        .map(|k| {
            k.split_once('=')
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .ok_or_else(|| format!("key {k:?} must be KEY=value"))
        })
        .collect()
}
