//! Syslog delivery (RFC 5424/5426/6587, TLS per RFC 5425).
//!
//! Targets are `udp://host:port`, `tcp://host:port`, or `tls://host:port`
//! URLs (ports default to 514, or 6514 for TLS). TCP and TLS use
//! octet-counting framing; UDP sends one datagram per message.

use std::io::Write as _;
use std::net::{TcpStream, ToSocketAddrs, UdpSocket};
use std::str::FromStr;
use std::time::Duration;

use crate::error::{Error, Result};
use crate::message::{AuditMessage, Outcome};

/// Syslog transport.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Transport {
    /// UDP, one datagram per message (RFC 5426).
    Udp,
    /// TCP with octet-counting framing (RFC 6587).
    Tcp,
    /// TLS over TCP with octet-counting framing (RFC 5425).
    Tls,
}

/// Parsed audit-server URL.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SyslogTarget {
    /// Transport.
    pub transport: Transport,
    /// Host name or IP (brackets stripped).
    pub host: String,
    /// Port (514 by default, 6514 for TLS).
    pub port: u16,
}

impl FromStr for SyslogTarget {
    type Err = Error;

    fn from_str(url: &str) -> Result<Self> {
        // Schemes are case-insensitive (RFC 3986 §3.1).
        let (transport, rest, default_port) = match url.split_once("://") {
            Some((scheme, rest)) if scheme.eq_ignore_ascii_case("udp") => {
                (Transport::Udp, rest, 514)
            }
            Some((scheme, rest)) if scheme.eq_ignore_ascii_case("tcp") => {
                (Transport::Tcp, rest, 514)
            }
            Some((scheme, rest)) if scheme.eq_ignore_ascii_case("tls") => {
                (Transport::Tls, rest, 6514)
            }
            _ => return Err(Error::BadUrl(url.to_string())),
        };
        if rest.is_empty() || rest.contains('/') || rest.contains('?') {
            return Err(Error::BadUrl(url.to_string()));
        }
        let (host, port) = if let Some(bracketed) = rest.strip_prefix('[') {
            let (host, port) = bracketed
                .split_once(']')
                .ok_or_else(|| Error::BadUrl(url.to_string()))?;
            let port = match port.strip_prefix(':') {
                Some(p) => p.parse().map_err(|_| Error::BadUrl(url.to_string()))?,
                None if port.is_empty() => default_port,
                None => return Err(Error::BadUrl(url.to_string())),
            };
            (host.to_string(), port)
        } else {
            match rest.rsplit_once(':') {
                // `host:port`, but a bare IPv6 literal without port.
                Some((host, port))
                    if !host.is_empty() && !port.contains(':') && !host.contains(':') =>
                {
                    let port = port.parse().map_err(|_| Error::BadUrl(url.to_string()))?;
                    (host.to_string(), port)
                }
                _ if !rest.contains(':') => (rest.to_string(), default_port),
                _ => return Err(Error::BadUrl(url.to_string())),
            }
        };
        if host.is_empty() {
            return Err(Error::BadUrl(url.to_string()));
        }
        Ok(Self {
            transport,
            host,
            port,
        })
    }
}

/// Syslog severity for an audit outcome.
#[must_use]
pub const fn severity_for(outcome: Outcome) -> u8 {
    match outcome {
        Outcome::Success => 5,      // notice
        Outcome::MinorFailure => 4, // warning
        Outcome::SeriousFailure => 3,
        Outcome::MajorFailure => 2, // critical
    }
}

/// Facility 13 (audit) plus severity.
#[must_use]
pub const fn pri_value(severity: u8) -> u32 {
    13 * 8 + severity as u32
}

/// Render an RFC 5424 frame (without transport framing).
///
/// Every header field is sanitized: spaces and control characters
/// would corrupt the framing (log injection via `--hostname` or a
/// hostile direct caller).
#[must_use]
pub fn syslog_frame(
    severity: u8,
    timestamp: &str,
    hostname: &str,
    app_name: &str,
    msgid: &str,
    message: &str,
) -> String {
    format!(
        "<{}>1 {timestamp} {hostname} {app_name} - {msgid} - {message}",
        pri_value(severity),
        timestamp = sanitize_field(timestamp),
        hostname = sanitize_field(hostname),
        app_name = sanitize_field(app_name),
        msgid = sanitize_field(msgid),
    )
}

/// Replace header-hostile characters (whitespace, controls, DEL)
/// with `_` (RFC 5424 fields admit no spaces).
fn sanitize_field(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_whitespace() || c.is_control() {
                '_'
            } else {
                c
            }
        })
        .collect()
}

/// Octet-counting frame for TCP/TLS (RFC 6587).
#[must_use]
pub fn octet_frame(frame: &str) -> String {
    format!("{} {frame}", frame.len())
}

/// Default connect/write timeout.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);

/// Syslog sender for audit messages (blocking; safe to share).
#[derive(Clone, Debug)]
pub struct SyslogSender {
    target: SyslogTarget,
    app_name: String,
    hostname: String,
    ca_pem: Option<Vec<u8>>,
    server_name: Option<String>,
    timeout: Duration,
}

impl SyslogSender {
    /// Sender with default identity (`vyna-dcm` / audit source host).
    #[must_use]
    pub fn new(target: SyslogTarget, hostname: impl Into<String>) -> Self {
        Self {
            target,
            app_name: "vyna-dcm".to_string(),
            hostname: hostname.into(),
            ca_pem: None,
            server_name: None,
            timeout: DEFAULT_TIMEOUT,
        }
    }

    /// Override the RFC 5424 `APP-NAME`.
    #[must_use]
    pub fn with_app_name(mut self, app: impl Into<String>) -> Self {
        self.app_name = app.into();
        self
    }

    /// Extra CA certificates (PEM) for `tls://` verification.
    #[must_use]
    pub fn with_ca_pem(mut self, pem: Vec<u8>) -> Self {
        self.ca_pem = Some(pem);
        self
    }

    /// TLS server name (defaults to the target host).
    #[must_use]
    pub fn with_server_name(mut self, name: impl Into<String>) -> Self {
        self.server_name = Some(name.into());
        self
    }

    /// Connect/write timeout.
    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Send one audit message (severity follows its outcome).
    ///
    /// Connect/read/write run under the 10 s timeout, but DNS
    /// resolution does not (`std` offers no resolution timeout): a
    /// broken resolver can stall past it. Prefer IP literals for the
    /// audit server when that matters.
    pub fn emit(&self, message: &AuditMessage) -> Result<()> {
        let frame = syslog_frame(
            severity_for(message.outcome),
            &vyna_dcm_core::utc_now_rfc3339(),
            &self.hostname,
            &self.app_name,
            "ATNA",
            &message.to_xml(),
        );
        self.send_frame(&frame)
    }

    fn send_frame(&self, frame: &str) -> Result<()> {
        match self.target.transport {
            Transport::Udp => self.send_udp(frame),
            Transport::Tcp => self.send_tcp(frame),
            Transport::Tls => self.send_tls(frame),
        }
    }

    fn send_udp(&self, frame: &str) -> Result<()> {
        // Resolve first and bind a matching family: an IPv4 socket
        // cannot send to an IPv6 server.
        let mut last_err = None;
        for addr in (self.target.host.as_str(), self.target.port)
            .to_socket_addrs()
            .map_err(|e| Error::Transport(e.to_string()))?
        {
            let bind = if addr.is_ipv6() {
                "[::]:0"
            } else {
                "0.0.0.0:0"
            };
            match UdpSocket::bind(bind).and_then(|s| s.send_to(frame.as_bytes(), addr).map(|_| ()))
            {
                Ok(()) => return Ok(()),
                Err(e) => last_err = Some(e),
            }
        }
        Err(Error::Transport(
            last_err
                .map(|e| e.to_string())
                .unwrap_or_else(|| format!("{}: no addresses", self.target.host)),
        ))
    }

    fn send_tcp(&self, frame: &str) -> Result<()> {
        let mut last_err = None;
        for addr in (self.target.host.as_str(), self.target.port)
            .to_socket_addrs()
            .map_err(|e| Error::Transport(e.to_string()))?
        {
            match TcpStream::connect_timeout(&addr, self.timeout) {
                Ok(stream) => {
                    return Self::write_tcp(stream, &octet_frame(frame), self.timeout);
                }
                Err(e) => last_err = Some(e),
            }
        }
        Err(Error::Transport(
            last_err
                .map(|e| e.to_string())
                .unwrap_or_else(|| format!("{}: no addresses", self.target.host)),
        ))
    }

    fn write_tcp(mut stream: TcpStream, frame: &str, timeout: Duration) -> Result<()> {
        stream
            .set_write_timeout(Some(timeout))
            .map_err(|e| Error::Transport(e.to_string()))?;
        stream
            .write_all(frame.as_bytes())
            .map_err(|e| Error::Transport(e.to_string()))?;
        stream.flush().map_err(|e| Error::Transport(e.to_string()))
    }

    #[cfg(feature = "tls")]
    fn send_tls(&self, frame: &str) -> Result<()> {
        use std::io::BufReader;
        use std::sync::Arc;

        let _ = rustls::crypto::ring::default_provider().install_default();
        let mut roots = rustls::RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        if let Some(pem) = &self.ca_pem {
            for cert in rustls_pemfile::certs(&mut BufReader::new(&pem[..])) {
                let cert = cert.map_err(|e| Error::Tls(e.to_string()))?;
                roots.add(cert).map_err(|e| Error::Tls(e.to_string()))?;
            }
        }
        let config = rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth();
        let name = self
            .server_name
            .clone()
            .unwrap_or_else(|| self.target.host.clone());
        let server_name =
            rustls::pki_types::ServerName::try_from(name).map_err(|e| Error::Tls(e.to_string()))?;
        let mut conn = rustls::ClientConnection::new(Arc::new(config), server_name)
            .map_err(|e| Error::Tls(e.to_string()))?;
        let mut last_err = None;
        for addr in (self.target.host.as_str(), self.target.port)
            .to_socket_addrs()
            .map_err(|e| Error::Transport(e.to_string()))?
        {
            match TcpStream::connect_timeout(&addr, self.timeout) {
                Ok(mut sock) => {
                    sock.set_write_timeout(Some(self.timeout))
                        .map_err(|e| Error::Transport(e.to_string()))?;
                    // The TLS handshake reads: without a read timeout a
                    // silent peer hangs emit() forever.
                    sock.set_read_timeout(Some(self.timeout))
                        .map_err(|e| Error::Transport(e.to_string()))?;
                    let mut tls = rustls::Stream::new(&mut conn, &mut sock);
                    tls.write_all(octet_frame(frame).as_bytes())
                        .map_err(|e| Error::Tls(e.to_string()))?;
                    tls.flush().map_err(|e| Error::Tls(e.to_string()))?;
                    return Ok(());
                }
                Err(e) => last_err = Some(e),
            }
        }
        Err(Error::Transport(
            last_err
                .map(|e| e.to_string())
                .unwrap_or_else(|| format!("{}: no addresses", self.target.host)),
        ))
    }

    #[cfg(not(feature = "tls"))]
    fn send_tls(&self, _frame: &str) -> Result<()> {
        Err(Error::TlsDisabled)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_parse_with_scheme_defaults() {
        let t: SyslogTarget = "udp://10.0.0.1:514".parse().unwrap();
        assert_eq!(
            (t.transport, t.host.as_str(), t.port),
            (Transport::Udp, "10.0.0.1", 514)
        );
        let t: SyslogTarget = "tcp://arr.example".parse().unwrap();
        assert_eq!((t.transport, t.port), (Transport::Tcp, 514));
        let t: SyslogTarget = "tls://arr.example".parse().unwrap();
        assert_eq!((t.transport, t.port), (Transport::Tls, 6514));
        let t: SyslogTarget = "udp://[::1]:10514".parse().unwrap();
        assert_eq!((t.host.as_str(), t.port), ("::1", 10514));
        assert!("http://x:1".parse::<SyslogTarget>().is_err());
        assert!("udp://".parse::<SyslogTarget>().is_err());
        assert!("udp://h:1/path".parse::<SyslogTarget>().is_err());
        assert!("udp://h:notaport".parse::<SyslogTarget>().is_err());
        assert!("udp://[::1".parse::<SyslogTarget>().is_err());
        // Schemes are case-insensitive.
        let t: SyslogTarget = "UDP://10.0.0.1:514".parse().unwrap();
        assert_eq!(t.transport, Transport::Udp);
        let t: SyslogTarget = "TLS://arr.example".parse().unwrap();
        assert_eq!((t.transport, t.port), (Transport::Tls, 6514));
    }

    #[test]
    fn frames_carry_pri_and_octet_counts() {
        assert_eq!(pri_value(5), 109);
        assert_eq!(severity_for(Outcome::Success), 5);
        assert_eq!(severity_for(Outcome::MajorFailure), 2);
        let frame = syslog_frame(5, "2026-10-12T00:00:00.000Z", "h", "a", "ATNA", "<m/>");
        assert!(frame.starts_with("<109>1 2026-10-12T00:00:00.000Z h a - ATNA - <m/>"));
        let octets = octet_frame(&frame);
        assert!(octets.starts_with(&format!("{} ", frame.len())));
        // Every header field is injection-proofed, not just host/app.
        let evil = syslog_frame(5, "t\n20", "h h", "a\ta", "M\nX", "<m/>");
        assert!(!evil.contains(['\n', '\t']), "{evil}");
        assert!(evil.contains("h_h") && evil.contains("M_X"), "{evil}");
    }

    #[test]
    fn udp_loopback_delivers() {
        let rx = UdpSocket::bind("127.0.0.1:0").unwrap();
        let port = rx.local_addr().unwrap().port();
        rx.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let target: SyslogTarget = format!("udp://127.0.0.1:{port}").parse().unwrap();
        let sender = SyslogSender::new(target, "testhost");
        let msg = AuditMessage::actor_activity("APP", None, "SRC", true);
        sender.emit(&msg).unwrap();
        let mut buf = [0u8; 65535];
        let (len, _) = rx.recv_from(&mut buf).unwrap();
        let got = String::from_utf8(buf[..len].to_vec()).unwrap();
        assert!(got.starts_with("<109>1 "), "{got}");
        assert!(
            got.contains(" testhost vyna-dcm - ATNA - <AuditMessage"),
            "{got}"
        );
        assert!(got.contains("code=\"110100\""), "{got}");
    }

    #[test]
    fn tcp_loopback_delivers_octet_frame() {
        use std::io::Read as _;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut sock, _) = listener.accept().unwrap();
            sock.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            let mut buf = Vec::new();
            sock.read_to_end(&mut buf).unwrap();
            String::from_utf8(buf).unwrap()
        });
        let target: SyslogTarget = format!("tcp://127.0.0.1:{port}").parse().unwrap();
        let sender = SyslogSender::new(target, "testhost");
        let msg = AuditMessage::actor_activity("APP", None, "SRC", true);
        sender.emit(&msg).unwrap();
        let got = server.join().unwrap();
        let (len, rest) = got.split_once(' ').unwrap();
        assert_eq!(rest.len(), len.parse::<usize>().unwrap());
        assert!(rest.contains("code=\"110100\""), "{got}");
    }

    #[test]
    fn tcp_refused_is_an_error() {
        // Port 1 is (almost) never listening; connect refuses fast.
        let target: SyslogTarget = "tcp://127.0.0.1:1".parse().unwrap();
        let sender = SyslogSender::new(target, "h").with_timeout(Duration::from_millis(500));
        let msg = AuditMessage::actor_activity("APP", None, "SRC", true);
        assert!(matches!(sender.emit(&msg), Err(Error::Transport(_))));
    }

    #[cfg(feature = "tls")]
    #[test]
    fn tls_loopback_delivers_with_private_ca() {
        use std::io::Read as _;
        use std::sync::Arc;

        let certified =
            rcgen::generate_simple_self_signed(vec!["localhost".into(), "127.0.0.1".into()])
                .unwrap();
        let cert_der = certified.cert.der().to_vec();
        let key_der = certified.signing_key.serialize_der();
        let cert_pem = certified.cert.pem().into_bytes();

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let _ = rustls::crypto::ring::default_provider().install_default();
            let config = rustls::ServerConfig::builder()
                .with_no_client_auth()
                .with_single_cert(
                    vec![rustls::pki_types::CertificateDer::from(cert_der)],
                    rustls::pki_types::PrivateKeyDer::Pkcs8(
                        rustls::pki_types::PrivatePkcs8KeyDer::from(key_der),
                    ),
                )
                .unwrap();
            let (mut sock, _) = listener.accept().unwrap();
            sock.set_read_timeout(Some(Duration::from_secs(10)))
                .unwrap();
            let mut conn = rustls::ServerConnection::new(Arc::new(config)).unwrap();
            let mut tls = rustls::Stream::new(&mut conn, &mut sock);
            let mut buf = Vec::new();
            let mut chunk = [0u8; 4096];
            loop {
                match tls.read(&mut chunk) {
                    Ok(0) => break,
                    Ok(n) => buf.extend_from_slice(&chunk[..n]),
                    // The sender drops without close_notify by design.
                    Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
                    Err(e) => panic!("tls read: {e}"),
                }
            }
            String::from_utf8(buf).unwrap()
        });

        // Trust the private CA explicitly; the default WebPKI roots
        // must not validate this self-signed loopback cert.
        let target: SyslogTarget = format!("tls://127.0.0.1:{port}").parse().unwrap();
        let sender = SyslogSender::new(target, "testhost")
            .with_ca_pem(cert_pem)
            .with_timeout(Duration::from_secs(10));
        let msg = AuditMessage::actor_activity("APP", None, "SRC", true);
        sender.emit(&msg).unwrap();
        let got = server.join().unwrap();
        assert!(got.contains("code=\"110100\""), "{got}");
    }

    #[cfg(feature = "tls")]
    #[test]
    fn tls_rejects_untrusted_servers() {
        let certified = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
        let cert_der = certified.cert.der().to_vec();
        let key_der = certified.signing_key.serialize_der();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            use std::io::Read as _;
            use std::sync::Arc;
            let _ = rustls::crypto::ring::default_provider().install_default();
            let config = rustls::ServerConfig::builder()
                .with_no_client_auth()
                .with_single_cert(
                    vec![rustls::pki_types::CertificateDer::from(cert_der)],
                    rustls::pki_types::PrivateKeyDer::Pkcs8(
                        rustls::pki_types::PrivatePkcs8KeyDer::from(key_der),
                    ),
                )
                .unwrap();
            let Ok((mut sock, _)) = listener.accept() else {
                return;
            };
            sock.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            let mut conn = rustls::ServerConnection::new(Arc::new(config)).unwrap();
            let mut tls = rustls::Stream::new(&mut conn, &mut sock);
            let mut buf = Vec::new();
            let _ = tls.read_to_end(&mut buf);
        });
        let target: SyslogTarget = format!("tls://127.0.0.1:{port}").parse().unwrap();
        let sender = SyslogSender::new(target, "h").with_timeout(Duration::from_secs(5));
        let msg = AuditMessage::actor_activity("APP", None, "SRC", true);
        // No private CA: WebPKI roots cannot validate the loopback cert.
        assert!(matches!(sender.emit(&msg), Err(Error::Tls(_))));
        drop(server);
    }

    #[test]
    fn frame_sanitizes_header_fields() {
        let f = syslog_frame(5, "t", "evil\nhost x", "a\tb", "ATNA", "<m/>");
        assert!(f.contains("evil_host_x"), "{f}");
        assert!(f.contains("a_b"), "{f}");
        assert!(!f.contains('\n'), "{f}");
    }

    #[test]
    fn udp_reaches_ipv6_loopback() {
        let listener = match UdpSocket::bind("[::1]:0") {
            Ok(s) => s,
            Err(e) => {
                eprintln!("skipping IPv6 test: no v6 stack ({e})");
                return;
            }
        };
        let port = listener.local_addr().unwrap().port();
        listener
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let target: SyslogTarget = format!("udp://[::1]:{port}").parse().unwrap();
        let sender = SyslogSender::new(target, "h");
        let msg = crate::AuditMessage::actor_activity("APP", None, "SRC", true);
        sender.emit(&msg).unwrap();
        let mut buf = [0u8; 4096];
        let (n, _) = listener.recv_from(&mut buf).unwrap();
        assert!(
            String::from_utf8_lossy(&buf[..n]).contains("ATNA"),
            "no datagram arrived"
        );
    }

    #[cfg(feature = "tls")]
    #[test]
    fn tls_emit_times_out_on_silent_peer() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let (sock, _) = listener.accept().unwrap();
            std::thread::sleep(Duration::from_secs(30));
            drop(sock);
        });
        let target: SyslogTarget = format!("tls://127.0.0.1:{port}").parse().unwrap();
        let sender = SyslogSender::new(target, "h").with_timeout(Duration::from_millis(200));
        let msg = crate::AuditMessage::actor_activity("APP", None, "SRC", true);
        let start = std::time::Instant::now();
        assert!(sender.emit(&msg).is_err());
        assert!(start.elapsed() < Duration::from_secs(10), "emit hung");
    }
}
