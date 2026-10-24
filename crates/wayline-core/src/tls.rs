//! Platform identity conversion and the upstream TLS configuration.
use std::{io, sync::Arc};
pub type TlsError = io::Error;
pub(crate) fn install_provider() {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
}
pub(crate) fn upstream_identity(identity: &crate::Identity) -> owly_airdrop::driver::tls::Identity {
    owly_airdrop::driver::tls::Identity::from_der(
        identity.certificate_der.clone(),
        identity.private_key_der.clone(),
    )
}
pub fn server_config(identity: &crate::Identity) -> io::Result<Arc<rustls::ServerConfig>> {
    install_provider();
    owly_airdrop::driver::tls::server_config(&upstream_identity(identity))
}
