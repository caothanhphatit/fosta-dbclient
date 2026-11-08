//! rustls client configs from a profile's TLS settings. Engines with a rustls-based client use
//! [`rustls_client_config`]; SQL Server on Windows keeps SChannel and maps the settings itself.

use std::sync::Arc;

use rustls::{
    client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
    pki_types::{pem::PemObject, CertificateDer, PrivateKeyDer, ServerName, UnixTime},
    DigitallySignedStruct, SignatureScheme,
};

use crate::{
    contract::TlsConfig,
    error::{AppError, AppResult},
};

/// Verifies against the bundled web roots plus `ca_file`, presents the client certificate when given, and skips
/// verification when `accept_invalid_certs` is set (self-signed test servers; traffic is encrypted, the server is not
/// authenticated). Mode `disable` is the caller's business (no TLS at all).
pub fn rustls_client_config(tls: &TlsConfig) -> AppResult<rustls::ClientConfig> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let builder = rustls::ClientConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .map_err(|error| AppError::new(format!("TLS setup failed: {error}")))?;
    let builder = if tls.accept_invalid_certs {
        builder.dangerous().with_custom_certificate_verifier(Arc::new(AcceptAny(provider)))
    } else {
        let mut roots = rustls::RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        if let Some(path) = tls.ca_file.as_deref().filter(|path| !path.is_empty()) {
            let certs = CertificateDer::pem_file_iter(path)
                .map_err(|error| AppError::new(format!("Could not read the CA file '{path}': {error}.")))?;
            for cert in certs {
                let cert = cert.map_err(|error| AppError::new(format!("The CA file '{path}' is not valid PEM: {error}.")))?;
                roots
                    .add(cert)
                    .map_err(|error| AppError::new(format!("The CA file '{path}' has an unusable certificate: {error}.")))?;
            }
        }
        builder.with_root_certificates(roots)
    };
    match (tls.client_cert_file.as_deref(), tls.client_key_file.as_deref()) {
        (Some(cert_path), Some(key_path)) if !cert_path.is_empty() && !key_path.is_empty() => {
            let certs = CertificateDer::pem_file_iter(cert_path)
                .and_then(|certs| certs.collect::<Result<Vec<_>, _>>())
                .map_err(|error| AppError::new(format!("Could not read the client certificate '{cert_path}': {error}.")))?;
            let key = PrivateKeyDer::from_pem_file(key_path)
                .map_err(|error| AppError::new(format!("Could not read the client key '{key_path}': {error}.")))?;
            builder
                .with_client_auth_cert(certs, key)
                .map_err(|error| AppError::new(format!("The client certificate cannot be used: {error}.")))
        }
        _ => Ok(builder.with_no_client_auth()),
    }
}

#[derive(Debug)]
struct AcceptAny(Arc<rustls::crypto::CryptoProvider>);

impl ServerCertVerifier for AcceptAny {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(message, cert, dss, &self.0.signature_verification_algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.0.signature_verification_algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configs_build_for_verified_and_insecure_modes_and_bad_files_are_explained() {
        assert!(rustls_client_config(&TlsConfig::default()).is_ok());
        let insecure = TlsConfig {
            accept_invalid_certs: true,
            ..TlsConfig::default()
        };
        assert!(rustls_client_config(&insecure).is_ok());
        let missing = TlsConfig {
            ca_file: Some("/no/such/ca.pem".into()),
            ..TlsConfig::default()
        };
        assert!(rustls_client_config(&missing).unwrap_err().to_string().contains("CA file"));
    }
}
