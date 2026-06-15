//! TLS with Trust-On-First-Use (TOFU) fingerprints.
//!
//! Like the reference protocol, Self-KVM layers TLS *under* the message
//! protocol: the TLS handshake completes (and the server fingerprint is
//! verified) before any [`crate::FramedConn`] traffic flows. There is no CA
//! chain — the server presents a self-signed certificate and the client pins
//! its SHA-256 fingerprint on first sight, rejecting any later change.

use std::fmt;
use std::sync::Arc;

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{ring as ring_provider, CryptoProvider};
use rustls::pki_types::{
    CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName, UnixTime,
};
use rustls::{DigitallySignedStruct, SignatureScheme};
use sha2::{Digest, Sha256};
use tokio_rustls::{TlsAcceptor, TlsConnector};

#[derive(Debug, thiserror::Error)]
pub enum TlsError {
    #[error("certificate generation failed: {0}")]
    Cert(String),
    #[error("tls configuration failed: {0}")]
    Config(String),
}

/// Compute the `SHA256:aa:bb:..` fingerprint of a DER certificate.
pub fn fingerprint(cert: &CertificateDer<'_>) -> String {
    let digest = Sha256::digest(cert.as_ref());
    let hex: Vec<String> = digest.iter().map(|b| format!("{b:02x}")).collect();
    format!("SHA256:{}", hex.join(":"))
}

/// Decides whether a presented server fingerprint is trusted, recording it on
/// first sight (trust-on-first-use).
pub trait TrustStore: fmt::Debug + Send + Sync {
    /// `Ok(())` to accept; `Err(reason)` to reject. Unknown identities should be
    /// recorded and accepted; a changed fingerprint must be rejected.
    fn check(&self, identity: &str, fingerprint: &str) -> Result<(), String>;
}

/// In-memory TOFU store (used by tests and ephemeral runs).
#[derive(Debug, Default)]
pub struct MemoryTrustStore {
    map: std::sync::Mutex<std::collections::HashMap<String, String>>,
}

impl MemoryTrustStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Pre-seed a trusted fingerprint (e.g. loaded from disk).
    pub fn insert(&self, identity: &str, fingerprint: &str) {
        self.map
            .lock()
            .unwrap()
            .insert(identity.to_string(), fingerprint.to_string());
    }

    pub fn get(&self, identity: &str) -> Option<String> {
        self.map.lock().unwrap().get(identity).cloned()
    }
}

impl TrustStore for MemoryTrustStore {
    fn check(&self, identity: &str, fingerprint: &str) -> Result<(), String> {
        let mut map = self.map.lock().unwrap();
        match map.get(identity) {
            Some(known) if known == fingerprint => Ok(()),
            Some(known) => Err(format!(
                "fingerprint changed for {identity}: trusted {known}, got {fingerprint}"
            )),
            None => {
                map.insert(identity.to_string(), fingerprint.to_string());
                Ok(())
            }
        }
    }
}

/// TOFU store persisted to a file, one `identity fingerprint` pair per line.
#[derive(Debug)]
pub struct FileTrustStore {
    path: std::path::PathBuf,
    mem: std::sync::Mutex<std::collections::HashMap<String, String>>,
}

impl FileTrustStore {
    /// Load trusted fingerprints from `path` (missing file => empty store).
    pub fn load(path: impl Into<std::path::PathBuf>) -> Self {
        let path = path.into();
        let mut map = std::collections::HashMap::new();
        if let Ok(text) = std::fs::read_to_string(&path) {
            for line in text.lines() {
                if let Some((id, fp)) = line.split_once(char::is_whitespace) {
                    map.insert(id.trim().to_string(), fp.trim().to_string());
                }
            }
        }
        Self {
            path,
            mem: std::sync::Mutex::new(map),
        }
    }

    fn persist(&self, map: &std::collections::HashMap<String, String>) {
        let body: String = map.iter().map(|(k, v)| format!("{k} {v}\n")).collect();
        if let Some(dir) = self.path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(&self.path, body);
    }
}

impl TrustStore for FileTrustStore {
    fn check(&self, identity: &str, fingerprint: &str) -> Result<(), String> {
        let mut map = self.mem.lock().unwrap();
        match map.get(identity) {
            Some(known) if known == fingerprint => Ok(()),
            Some(known) => Err(format!(
                "fingerprint changed for {identity}: trusted {known}, got {fingerprint}"
            )),
            None => {
                map.insert(identity.to_string(), fingerprint.to_string());
                self.persist(&map);
                Ok(())
            }
        }
    }
}

/// Build a TLS acceptor from a freshly generated self-signed certificate.
/// Returns the acceptor and the certificate's fingerprint (to display/share).
pub fn server_acceptor() -> Result<(TlsAcceptor, String), TlsError> {
    let key = rcgen::generate_simple_self_signed(vec!["self-kvm".to_string()])
        .map_err(|e| TlsError::Cert(e.to_string()))?;
    let cert_der = CertificateDer::from(key.cert.der().to_vec());
    let fp = fingerprint(&cert_der);
    let key_der = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.key_pair.serialize_der()));

    let config = rustls::ServerConfig::builder_with_provider(Arc::new(ring_provider::default_provider()))
        .with_safe_default_protocol_versions()
        .map_err(|e| TlsError::Config(e.to_string()))?
        .with_no_client_auth()
        .with_single_cert(vec![cert_der], key_der)
        .map_err(|e| TlsError::Config(e.to_string()))?;

    Ok((TlsAcceptor::from(Arc::new(config)), fp))
}

/// Build a TLS connector that pins the server fingerprint against `store` under
/// the key `identity`.
pub fn client_connector(identity: impl Into<String>, store: Arc<dyn TrustStore>) -> TlsConnector {
    let provider = Arc::new(ring_provider::default_provider());
    let verifier = Arc::new(TofuVerifier {
        provider: provider.clone(),
        identity: identity.into(),
        store,
    });
    let config = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .expect("ring provider supports default versions")
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_no_client_auth();
    TlsConnector::from(Arc::new(config))
}

/// The dummy SNI used for connections (hostname is not validated under TOFU).
pub fn server_name() -> ServerName<'static> {
    ServerName::try_from("self-kvm").expect("valid static dns name")
}

/// A certificate verifier that trusts on first use and pins by SHA-256
/// fingerprint, while still cryptographically verifying the handshake signature
/// (so a peer must actually hold the pinned key).
#[derive(Debug)]
struct TofuVerifier {
    provider: Arc<CryptoProvider>,
    identity: String,
    store: Arc<dyn TrustStore>,
}

impl ServerCertVerifier for TofuVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let fp = fingerprint(end_entity);
        self.store
            .check(&self.identity, &fp)
            .map_err(|reason| rustls::Error::General(reason))?;
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::FramedConn;
    use kvm_proto::Message;
    use tokio::net::{TcpListener, TcpStream};

    #[tokio::test]
    async fn tls_framed_roundtrip_and_tofu_record() {
        let (acceptor, server_fp) = server_acceptor().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            let tls = acceptor.accept(tcp).await.unwrap();
            let mut conn = FramedConn::new(tls);
            let m = conn.recv().await.unwrap();
            conn.send(&m).await.unwrap();
        });

        let store = Arc::new(MemoryTrustStore::new());
        let connector = client_connector("studio", store.clone());
        let tcp = TcpStream::connect(addr).await.unwrap();
        let tls = connector.connect(server_name(), tcp).await.unwrap();
        let mut conn = FramedConn::new(tls);

        let msg = Message::Enter {
            x: 1,
            y: 2,
            seq: 3,
            modifiers: 4,
        };
        conn.send(&msg).await.unwrap();
        assert_eq!(conn.recv().await.unwrap(), msg);
        server.await.unwrap();

        // TOFU recorded the server's fingerprint under the identity.
        assert_eq!(store.get("studio").as_deref(), Some(server_fp.as_str()));
    }

    #[tokio::test]
    async fn tls_rejects_changed_fingerprint() {
        let (acceptor, _fp) = server_acceptor().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        tokio::spawn(async move {
            if let Ok((tcp, _)) = listener.accept().await {
                let _ = acceptor.accept(tcp).await; // may fail when client aborts
            }
        });

        let store = Arc::new(MemoryTrustStore::new());
        store.insert("studio", "SHA256:de:ad:be:ef"); // pinned to the wrong cert
        let connector = client_connector("studio", store);
        let tcp = TcpStream::connect(addr).await.unwrap();
        let result = connector.connect(server_name(), tcp).await;
        assert!(result.is_err(), "handshake must fail on fingerprint mismatch");
    }
}
