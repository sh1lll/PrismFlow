//! The companion's per-install self-signed certificate. Devices pin its
//! SHA-256 fingerprint at pairing, so no public CA is involved.

use sha2::{Digest, Sha256};
use std::path::Path;

pub struct Identity {
    pub cert_der: Vec<u8>,
    pub key_der: Vec<u8>,
    /// Lowercase hex SHA-256 of the certificate (what devices pin).
    pub fingerprint: String,
}

pub fn fingerprint(cert_der: &[u8]) -> String {
    Sha256::digest(cert_der).iter().map(|b| format!("{b:02x}")).collect()
}

/// Load the identity for `sans`, generating a new one when none exists or
/// the names changed (a new address means devices must re-pair).
pub fn load_or_create(dir: &Path, sans: &[String]) -> Result<Identity, String> {
    let cert_p = dir.join("companion-cert.der");
    let key_p = dir.join("companion-key.der");
    let sans_p = dir.join("companion-sans.json");
    let want = serde_json::to_string(sans).map_err(|e| e.to_string())?;
    if let (Ok(cert), Ok(key), Ok(have)) = (std::fs::read(&cert_p), std::fs::read(&key_p), std::fs::read_to_string(&sans_p)) {
        if have == want {
            return Ok(Identity { fingerprint: fingerprint(&cert), cert_der: cert, key_der: key });
        }
    }
    let ck = rcgen::generate_simple_self_signed(sans.to_vec()).map_err(|e| format!("certificate generation failed: {e}"))?;
    let cert = ck.cert.der().to_vec();
    let key = ck.key_pair.serialize_der();
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    crate::config_engine::atomic_write(&sans_p, &want).map_err(|e| e.to_string())?;
    std::fs::write(&cert_p, &cert).map_err(|e| e.to_string())?;
    write_private(&key_p, &key)?;
    Ok(Identity { fingerprint: fingerprint(&cert), cert_der: cert, key_der: key })
}

fn write_private(path: &Path, bytes: &[u8]) -> Result<(), String> {
    std::fs::write(path, bytes).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).map_err(|e| e.to_string())?;
    }
    Ok(())
}

pub fn server_config(id: &Identity) -> Result<rustls::ServerConfig, String> {
    let provider = std::sync::Arc::new(rustls::crypto::ring::default_provider());
    rustls::ServerConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(|e| e.to_string())?
        .with_no_client_auth()
        .with_single_cert(
            vec![rustls::pki_types::CertificateDer::from(id.cert_der.clone())],
            rustls::pki_types::PrivateKeyDer::Pkcs8(id.key_der.clone().into()),
        )
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_is_stable_until_the_names_change() {
        let dir = tempfile::tempdir().unwrap();
        let a = load_or_create(dir.path(), &["127.0.0.1".into()]).unwrap();
        let b = load_or_create(dir.path(), &["127.0.0.1".into()]).unwrap();
        assert_eq!(a.fingerprint, b.fingerprint);
        assert_eq!(a.fingerprint.len(), 64);
        let c = load_or_create(dir.path(), &["10.0.0.9".into()]).unwrap();
        assert_ne!(a.fingerprint, c.fingerprint);
        server_config(&c).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.path().join("companion-key.der")).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }
}
