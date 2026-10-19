//! TLS transport helpers for the AirDrop listener.

use std::io;
use std::sync::Arc;

use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};

use crate::Identity;

#[derive(Debug)]
pub enum TlsError {
    Io(io::Error),
    Rustls(rustls::Error),
}

impl From<io::Error> for TlsError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<rustls::Error> for TlsError {
    fn from(error: rustls::Error) -> Self {
        Self::Rustls(error)
    }
}

impl std::fmt::Display for TlsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(f, "I/O error: {error}"),
            Self::Rustls(error) => write!(f, "TLS: {error}"),
        }
    }
}

impl std::error::Error for TlsError {}

pub fn server_config(identity: &Identity) -> Result<Arc<rustls::ServerConfig>, TlsError> {
    let certificates = vec![CertificateDer::from(identity.certificate_der.clone())];
    let private_key =
        PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(identity.private_key_der.clone()));
    // GPUI's HTTP dependency also enables ring. Select our provider explicitly
    // so feature unification cannot make listener initialization panic.
    let config = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::aws_lc_rs::default_provider(),
    ))
    .with_safe_default_protocol_versions()?
    .with_no_client_auth()
    .with_single_cert(certificates, private_key)?;
    Ok(Arc::new(config))
}
