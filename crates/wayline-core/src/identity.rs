//! Stable self-signed identity used by the AirDrop TLS endpoint.

use sha2::Digest;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug)]
pub enum IdentityError {
    Io(std::io::Error),
    Certificate(rcgen::Error),
    Invalid(&'static str),
}

impl From<std::io::Error> for IdentityError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<rcgen::Error> for IdentityError {
    fn from(error: rcgen::Error) -> Self {
        Self::Certificate(error)
    }
}

impl std::fmt::Display for IdentityError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(f, "I/O error: {error}"),
            Self::Certificate(error) => write!(f, "certificate: {error}"),
            Self::Invalid(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for IdentityError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    pub certificate_der: Vec<u8>,
    pub private_key_der: Vec<u8>,
}

impl Identity {
    pub fn load_or_create(
        directory: impl AsRef<Path>,
        computer_name: &str,
    ) -> Result<Self, IdentityError> {
        let directory = directory.as_ref();
        let certificate_path = directory.join("receiver.cert.der");
        let key_path = directory.join("receiver.key.der");
        if certificate_path.is_file() && key_path.is_file() {
            let certificate_der = fs::read(&certificate_path)?;
            let private_key_der = fs::read(&key_path)?;
            if !certificate_der.is_empty() && !private_key_der.is_empty() {
                return Ok(Self {
                    certificate_der,
                    private_key_der,
                });
            }
        }

        let mut names = vec!["airdrop".to_owned()];
        if valid_dns_name(computer_name) {
            names.push(computer_name.to_owned());
        }
        let rcgen::CertifiedKey {
            cert: certificate,
            signing_key,
        } = rcgen::generate_simple_self_signed(names)?;
        let identity = Self {
            certificate_der: certificate.der().as_ref().to_vec(),
            private_key_der: signing_key.serialize_der(),
        };
        fs::create_dir_all(directory)?;
        write_private(&certificate_path, &identity.certificate_der)?;
        write_private(&key_path, &identity.private_key_der)?;
        Ok(identity)
    }

    /// Stable 48-bit service id, matching the existing receiver's cache-stable behavior.
    pub fn service_id(&self) -> String {
        let digest = sha2::Sha256::digest(&self.certificate_der);
        hex::encode(&digest[..6])
    }

    /// A rename must be a new discovery instance so peers repeat /Discover
    /// instead of reusing a cached ReceiverComputerName. Keep the TLS keys stable.
    pub fn named_service_id(&self, computer_name: &str) -> String {
        let mut digest = sha2::Sha256::new();
        digest.update(&self.certificate_der);
        digest.update(b"\0wayline-name\0");
        digest.update(computer_name.as_bytes());
        hex::encode(&digest.finalize()[..6])
    }

    pub fn paths(directory: impl AsRef<Path>) -> (PathBuf, PathBuf) {
        let directory = directory.as_ref();
        (
            directory.join("receiver.cert.der"),
            directory.join("receiver.key.der"),
        )
    }
}

fn valid_dns_name(name: &str) -> bool {
    if name.is_empty() || name.len() > 253 || !name.is_ascii() {
        return false;
    }
    name.split('.').all(|label| {
        !label.is_empty()
            && label.len() <= 63
            && !label.starts_with('-')
            && !label.ends_with('-')
            && label
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    })
}

fn write_private(path: &Path, bytes: &[u8]) -> Result<(), IdentityError> {
    let temporary = path.with_extension("tmp");
    fs::write(&temporary, bytes)?;
    fs::rename(&temporary, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_is_stable_across_loads() {
        let directory = tempfile::tempdir().unwrap();
        let first = Identity::load_or_create(directory.path(), "test-host").unwrap();
        let second = Identity::load_or_create(directory.path(), "test-host").unwrap();
        assert_eq!(first, second);
        assert_eq!(first.service_id().len(), 12);
        crate::tls::server_config(&first).unwrap();
    }

    #[test]
    fn non_ascii_computer_name_still_gets_a_valid_airdrop_identity() {
        let directory = tempfile::tempdir().unwrap();
        let identity = Identity::load_or_create(directory.path(), "家庭PC").unwrap();
        assert!(!identity.certificate_der.is_empty());
        crate::tls::server_config(&identity).unwrap();
    }

    #[test]
    fn rename_refreshes_discovery_identity_but_preserves_tls_identity() {
        let directory = tempfile::tempdir().unwrap();
        let before = Identity::load_or_create(directory.path(), "家庭PC").unwrap();
        let after = Identity::load_or_create(directory.path(), "XiaoMi").unwrap();
        assert_eq!(before, after);
        let old = before.named_service_id("家庭PC");
        let new = after.named_service_id("XiaoMi");
        assert_ne!(old, new);
        assert_ne!(new, after.service_id());
        assert_ne!(new, after.named_service_id("xiaomi"));
        assert_eq!(new, before.named_service_id("XiaoMi"));
        assert_eq!(new.len(), 12);
        assert!(new.bytes().all(|c| c.is_ascii_hexdigit()));
    }
}
