//! TLS for DICOM associations (PS3.15), via rustls.
//!
//! - SCU: [`TlsConnector`] wraps a TCP stream after connecting.
//! - SCP: [`TlsAcceptor`] wraps each inbound TCP stream before [`accept`].
//!
//! [`accept`]: crate::service::accept

use std::io::BufReader;
use std::sync::Arc;
use tokio::net::TcpStream;
use tokio_rustls::rustls::{self, ClientConfig, RootCertStore, ServerConfig};
use tokio_rustls::{TlsAcceptor as TokioAcceptor, TlsConnector as TokioConnector};

use crate::{Error, Result};

/// TLS client connector (SCU side).
#[derive(Clone)]
pub struct TlsConnector {
    inner: TokioConnector,
    /// Server name (DNS or IP literal) for certificate verification.
    server_name: String,
}

impl TlsConnector {
    /// Build with WebPKI roots plus optional extra CA PEM, and optional
    /// mutual-TLS client identity (`cert_chain_pem` + `key_pem`).
    pub fn new(
        server_name: impl Into<String>,
        extra_ca_pem: Option<&[u8]>,
        client_identity: Option<(&[u8], &[u8])>,
    ) -> Result<Self> {
        let mut roots = RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        if let Some(pem) = extra_ca_pem {
            for cert in rustls_pemfile::certs(&mut BufReader::new(pem)) {
                let cert = cert.map_err(|e| Error::Tls(e.to_string()))?;
                roots.add(cert).map_err(|e| Error::Tls(e.to_string()))?;
            }
        }
        let builder = ClientConfig::builder().with_root_certificates(roots);
        let config = match client_identity {
            None => builder.with_no_client_auth(),
            Some((cert_pem, key_pem)) => {
                let certs: Vec<_> = rustls_pemfile::certs(&mut BufReader::new(cert_pem))
                    .collect::<std::result::Result<_, _>>()
                    .map_err(|e| Error::Tls(e.to_string()))?;
                let key = private_key(key_pem)?;
                builder
                    .with_client_auth_cert(certs, key)
                    .map_err(|e| Error::Tls(e.to_string()))?
            }
        };
        Ok(Self {
            inner: TokioConnector::from(Arc::new(config)),
            server_name: server_name.into(),
        })
    }

    /// TLS handshake over an open TCP stream.
    pub async fn connect(
        &self,
        stream: TcpStream,
    ) -> Result<tokio_rustls::client::TlsStream<TcpStream>> {
        let name = self
            .server_name
            .clone()
            .try_into()
            .map_err(|_| Error::Tls(format!("bad server name {:?}", self.server_name)))?;
        self.inner
            .connect(name, stream)
            .await
            .map_err(|e| Error::Tls(e.to_string()))
    }

    /// Connect + handshake to `addr` in one step.
    pub async fn connect_to(
        &self,
        addr: &str,
    ) -> Result<tokio_rustls::client::TlsStream<TcpStream>> {
        let stream = TcpStream::connect(addr).await?;
        self.connect(stream).await
    }
}

/// TLS server acceptor (SCP side).
#[derive(Clone)]
pub struct TlsAcceptor {
    inner: TokioAcceptor,
}

impl TlsAcceptor {
    /// Build from a PEM certificate chain + private key, with optional
    /// mutual-TLS client CA (PEM). Without a client CA, any client may
    /// connect (server authentication only).
    pub fn new(
        cert_chain_pem: &[u8],
        key_pem: &[u8],
        client_ca_pem: Option<&[u8]>,
    ) -> Result<Self> {
        let certs: Vec<_> = rustls_pemfile::certs(&mut BufReader::new(cert_chain_pem))
            .collect::<std::result::Result<_, _>>()
            .map_err(|e| Error::Tls(e.to_string()))?;
        if certs.is_empty() {
            return Err(Error::Tls("empty certificate chain".into()));
        }
        let key = private_key(key_pem)?;
        let builder = if let Some(ca_pem) = client_ca_pem {
            let mut roots = RootCertStore::empty();
            for cert in rustls_pemfile::certs(&mut BufReader::new(ca_pem)) {
                let cert = cert.map_err(|e| Error::Tls(e.to_string()))?;
                roots.add(cert).map_err(|e| Error::Tls(e.to_string()))?;
            }
            let verifier = rustls::server::WebPkiClientVerifier::builder(Arc::new(roots))
                .build()
                .map_err(|e| Error::Tls(e.to_string()))?;
            ServerConfig::builder().with_client_cert_verifier(verifier)
        } else {
            ServerConfig::builder().with_no_client_auth()
        };
        let config = builder
            .with_single_cert(certs, key)
            .map_err(|e| Error::Tls(e.to_string()))?;
        Ok(Self {
            inner: TokioAcceptor::from(Arc::new(config)),
        })
    }

    /// TLS handshake over an inbound TCP stream.
    pub async fn accept(
        &self,
        stream: TcpStream,
    ) -> Result<tokio_rustls::server::TlsStream<TcpStream>> {
        self.inner
            .accept(stream)
            .await
            .map_err(|e| Error::Tls(e.to_string()))
    }
}

fn private_key(key_pem: &[u8]) -> Result<rustls::pki_types::PrivateKeyDer<'static>> {
    rustls_pemfile::private_key(&mut BufReader::new(key_pem))
        .map_err(|e| Error::Tls(e.to_string()))?
        .ok_or_else(|| Error::Tls("no private key found in PEM".into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::service::{AcceptorOptions, Handlers, ScpOptions, accept, propose};
    use crate::{AeTitle, Association, AssociationOptions};

    #[test]
    fn rejects_empty_chain_and_key() {
        assert!(TlsAcceptor::new(b"", b"", None).is_err());
        assert!(
            TlsAcceptor::new(
                b"-----BEGIN CERTIFICATE-----\nZm9v\n-----END CERTIFICATE-----\n",
                b"not a key",
                None
            )
            .is_err()
        );
    }

    struct EchoOnly;
    impl Handlers for EchoOnly {}

    #[tokio::test]
    async fn tls_loopback_echo() {
        let certified =
            rcgen::generate_simple_self_signed(vec!["localhost".into(), "127.0.0.1".into()])
                .unwrap();
        let cert_pem = certified.cert.pem().into_bytes();
        let key_pem = certified.signing_key.serialize_pem().into_bytes();

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let acceptor = TlsAcceptor::new(&cert_pem, &key_pem, None).unwrap();
        tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            let tls = acceptor.accept(tcp).await.unwrap();
            let mut acc = AcceptorOptions::default();
            acc.association.read_timeout = std::time::Duration::from_secs(10);
            accept(tls, &acc, &EchoOnly, &ScpOptions::default())
                .await
                .unwrap();
        });

        let connector = TlsConnector::new("localhost", Some(&cert_pem), None).unwrap();
        let tls = connector.connect_to(&addr.to_string()).await.unwrap();
        let opts = AssociationOptions {
            read_timeout: std::time::Duration::from_secs(10),
            ..Default::default()
        };
        let mut assoc = Association::request_over(
            &opts,
            &AeTitle::new("SCU").unwrap(),
            &AeTitle::new("SCP").unwrap(),
            tls,
            propose(
                &[vyna_dcm_core::sop_class::VERIFICATION],
                &[vyna_dcm_core::TransferSyntax::ExplicitVrLittleEndian],
            ),
        )
        .await
        .unwrap();
        assert_eq!(
            assoc
                .echo(vyna_dcm_core::sop_class::VERIFICATION)
                .await
                .unwrap(),
            crate::status::SUCCESS
        );
        assoc.release().await.unwrap();
    }
}
