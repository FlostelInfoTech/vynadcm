//! `wadorsd`: serve a DICOMweb store (QIDO/WADO/STOW) from a directory.
//!
//! Seeds an in-memory store from `*.dcm` files; STOW uploads land in
//! memory too (a stub archive, not durable storage) unless `--persist`
//! backs the store with the directory itself.

#[path = "webcommon.rs"]
mod webcommon;

use std::path::PathBuf;
use std::sync::Arc;

use clap::Parser;
use vyna_dcm::web::filestore::FileStore;
use vyna_dcm::web::hyper_server::{ServerAuth, serve_forever_with_auth};
use vyna_dcm::web::server::{DicomWebStore, MemoryStore};

/// DICOMweb stub server.
///
/// Plain HTTP without auth by default: keep the loopback bind or put
/// a TLS-terminating proxy in front (PHI on the wire otherwise).
#[derive(Parser, Debug)]
#[command(name = "wadorsd", about = "Serve QIDO/WADO/STOW from a directory")]
struct Args {
    /// Bind address.
    #[arg(long, default_value = "127.0.0.1:8080")]
    bind: String,
    /// Directory of Part-10 files to serve (recursive).
    #[arg(long, default_value = ".")]
    dir: PathBuf,
    /// Public base URL for BulkDataURIs (defaults to `http://{bind}/`).
    #[arg(long)]
    public_base: Option<String>,
    /// Require `Authorization: Bearer <token>` on every request.
    #[arg(long)]
    bearer_token: Option<String>,
    /// Read the Bearer `[REDACTED]` from a file instead of argv (tokens passed
    /// via `--bearer-token` are visible in `ps` output). The file
    /// content is whitespace-trimmed; conflicts with `--bearer-token`.
    #[arg(long, conflicts_with = "bearer_token")]
    bearer_token_file: Option<PathBuf>,
    /// Persist the store in `--dir` (`{study}/{series}/{instance}.dcm`,
    /// rescanned on startup) instead of seeding memory: STOW uploads
    /// survive restarts.
    #[arg(long)]
    persist: bool,
}

#[tokio::main]
async fn main() {
    let args = Args::parse();
    match run(&args).await {
        Ok(()) => {}
        Err(e) => {
            eprintln!("wadorsd: {e}");
            std::process::exit(1);
        }
    }
}

/// True when the bind needs the loud PHI warning: reachable beyond
/// loopback with no bearer token configured (and plain HTTP either way).
fn needs_phi_warning(addr: &std::net::SocketAddr, bearer_token: &Option<String>) -> bool {
    bearer_token.is_none() && !addr.ip().is_loopback()
}

/// Resolve the effective Bearer `[REDACTED]` `--bearer-token-file`
/// when given (clap forbids passing both flags at once),
/// `--bearer-token` otherwise.
fn resolve_token(args: &Args) -> Result<Option<String>, String> {
    if let Some(path) = &args.bearer_token_file {
        let raw = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let token = raw.trim().to_string();
        if token.is_empty() {
            return Err(format!(
                "{}: token file is empty or whitespace-only",
                path.display()
            ));
        }
        return Ok(Some(token));
    }
    Ok(args.bearer_token.clone())
}

async fn run(args: &Args) -> Result<(), String> {
    let addr: std::net::SocketAddr = args
        .bind
        .parse()
        .map_err(|_| format!("bad --bind {:?}", args.bind))?;
    let token = resolve_token(args)?;
    // An empty token suppresses the PHI warning while 401ing every
    // request (no `Authorization` header can ever match it): reject
    // it loudly instead of running a "protected" dead server.
    if token.as_deref().is_some_and(str::is_empty) {
        return Err(
            "empty --bearer-token authenticates nothing; omit it or pass a real token".into(),
        );
    }
    if needs_phi_warning(&addr, &token) {
        eprintln!(
            "wadorsd WARNING: serving PHI over unauthenticated plain HTTP on {addr} \
             (non-loopback bind without --bearer-token/--bearer-token-file); keep the loopback bind or put \
             a TLS-terminating proxy in front"
        );
    }
    let (store, seeded, skipped) = open_store(args)?;
    println!(
        "wadorsd serving {} instance(s) ({} skipped) from {} on http://{}/",
        seeded,
        skipped,
        args.dir.display(),
        addr
    );
    let base = args
        .public_base
        .clone()
        .unwrap_or_else(|| format!("http://{addr}/"));
    let auth = ServerAuth {
        bearer_token: token,
    };
    // `serve_forever_with_auth` is generic over a sized store, so each
    // variant serves through the shared helper below.
    match store {
        Store::Memory(store) => serve(store, base, addr, auth).await,
        Store::File(store) => serve(store, base, addr, auth).await,
    }
}

async fn serve<S: DicomWebStore + 'static>(
    store: Arc<S>,
    base: String,
    addr: std::net::SocketAddr,
    auth: ServerAuth,
) -> Result<(), String> {
    serve_forever_with_auth(store, base, addr, auth)
        .await
        .map_err(|e| e.to_string())
}

/// Either backing store `wadorsd` can serve.
enum Store {
    Memory(Arc<MemoryStore>),
    File(Arc<FileStore>),
}

/// Open the backing store plus (indexed, skipped) counts for the
/// startup line: a durable [`FileStore`] rescan under `--persist`,
/// otherwise a [`MemoryStore`] seeded from `--dir`.
fn open_store(args: &Args) -> Result<(Store, usize, usize), String> {
    if args.persist {
        let store = FileStore::open(&args.dir)?;
        let (len, skipped) = (store.len(), store.skipped());
        return Ok((Store::File(Arc::new(store)), len, skipped));
    }
    let store = Arc::new(MemoryStore::new());
    let mut read = 0usize;
    let mut skipped = 0usize;
    for file in webcommon::collect_dcm_files(&args.dir) {
        read += 1;
        let bytes = std::fs::read(&file).map_err(|e| format!("{}: {e}", file.display()))?;
        match store.store_instance(vyna_dcm::web::Bytes::from(bytes), None) {
            Ok(_) => {}
            Err(e) => {
                eprintln!("wadorsd: skipping {}: {e}", file.display());
                skipped += 1;
            }
        }
    }
    // The store caps at 10k instances / 1 GiB with oldest-first
    // eviction: when seeding overflows the caps the startup line
    // would otherwise under-report with no hint anything was lost.
    if store.len() + skipped != read {
        eprintln!(
            "wadorsd WARNING: seeded {read} file(s) but the store holds {} \
             (store caps evicted the oldest)",
            store.len()
        );
    }
    let len = store.len();
    Ok((Store::Memory(store), len, skipped))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phi_warning_only_for_exposed_binds() {
        let token = Some("t".to_string());
        for (bind, tok, warn) in [
            ("127.0.0.1:8080", None, false),
            ("[::1]:8080", None, false),
            ("0.0.0.0:8080", None, true),
            ("[::]:8080", None, true),
            ("192.168.1.5:8080", None, true),
            ("0.0.0.0:8080", token.clone(), false),
            ("192.168.1.5:8080", token.clone(), false),
        ] {
            let addr: std::net::SocketAddr = bind.parse().unwrap();
            assert_eq!(needs_phi_warning(&addr, &tok), warn, "{bind}");
        }
    }

    #[tokio::test]
    async fn empty_bearer_token_rejected() {
        let args = Args {
            bind: "127.0.0.1:1".into(),
            dir: PathBuf::from("."),
            public_base: None,
            bearer_token: Some(String::new()),
            bearer_token_file: None,
            persist: false,
        };
        let err = run(&args).await.unwrap_err();
        assert!(err.contains("empty --bearer-token"), "{err}");
    }

    fn token_file_fixture(name: &str, contents: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "wadorsd-tok-{}-{}-{}",
            std::process::id(),
            name,
            contents.len()
        ));
        std::fs::write(&path, contents).unwrap();
        path
    }

    fn args_with_token_file(path: PathBuf) -> Args {
        Args {
            bind: "127.0.0.1:1".into(),
            dir: PathBuf::from("."),
            public_base: None,
            bearer_token: None,
            bearer_token_file: Some(path),
            persist: false,
        }
    }

    #[test]
    fn token_file_resolves_trimmed_token() {
        let path = token_file_fixture("ok", "s3cret\n");
        let args = args_with_token_file(path.clone());
        assert_eq!(resolve_token(&args).unwrap(), Some("s3cret".to_string()));
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn token_file_missing_and_empty_fail_loudly() {
        let missing = args_with_token_file(PathBuf::from("definitely-not-a-token-file-xyz"));
        let err = resolve_token(&missing).unwrap_err();
        assert!(err.contains("definitely-not-a-token-file-xyz"), "{err}");

        let path = token_file_fixture("empty", "  \n\t\n");
        let empty = args_with_token_file(path.clone());
        let err = resolve_token(&empty).unwrap_err();
        assert!(err.contains("empty or whitespace-only"), "{err}");
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn token_and_token_file_conflict() {
        let err =
            Args::try_parse_from(["wadorsd", "--bearer-token", "a", "--bearer-token-file", "b"])
                .unwrap_err();
        assert!(err.to_string().contains("cannot be used with"), "{err}");
    }

    #[test]
    fn open_store_memory_and_persist_modes() {
        use vyna_dcm::web::server::test_file;
        let dir = std::env::temp_dir().join(format!("wadorsd-store-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("a.dcm"),
            test_file("1.2.910.1", "1.2.910.2", "1.2.910.3", 8, 8),
        )
        .unwrap();
        for persist in [false, true] {
            let args = Args {
                bind: "127.0.0.1:1".into(),
                dir: dir.clone(),
                public_base: None,
                bearer_token: None,
                bearer_token_file: None,
                persist,
            };
            let (store, len, skipped) = open_store(&args).unwrap();
            assert_eq!((len, skipped), (1, 0), "persist={persist}");
            let file = match &store {
                Store::Memory(store) => {
                    assert!(!persist);
                    store.instance_file("1.2.910.1", "1.2.910.2", "1.2.910.3")
                }
                Store::File(store) => {
                    assert!(persist);
                    store.instance_file("1.2.910.1", "1.2.910.2", "1.2.910.3")
                }
            };
            assert!(file.is_some(), "persist={persist}");
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
