//! Shared TLS helpers: one rustls `ClientConfig` builder for every rustls-based engine, plus a mapping to the
//! few knobs a native-tls engine (tiberius on Windows) has.
//!
//! Trust decision, first match wins:
//! 1. `pinned_sha256`: the server's chain must contain a certificate with that SHA-256 fingerprint; chain,
//!    expiry and name are not checked (the pin is the trust anchor).
//! 2. `accept_invalid_certs`: any certificate.
//! 3. otherwise the chain must lead to `ca_file` (when given) or to the system / Mozilla roots, and for every
//!    mode but `verifyCa` the certificate must be valid for `server_name` (or the connect host).
//!
//! Handshake signatures are verified in all cases, so a pinned or accepted certificate still proves
//! possession of its key.

use std::sync::{Arc, OnceLock};

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::client::WebPkiServerVerifier;
use rustls::crypto::CryptoProvider;
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime};
use rustls::{CertificateError, ClientConfig, DigitallySignedStruct, RootCertStore, SignatureScheme};

use super::spec::{TlsMode, TlsSpec};
use crate::error::{AppError, AppResult};

/// Error code of TLS configuration problems (unreadable files, bad fingerprint, TLS disabled).
pub const TLS_CONFIG: &str = "TLS_CONFIG";

fn config_error(message: impl Into<String>) -> AppError {
    let message = message.into();
    AppError::structured(message.clone(), serde_json::json!({ "code": TLS_CONFIG, "message": message }))
}

/// Builds the rustls client config for `tls`. Fails for `mode: disable` (check [`TlsSpec::is_enabled`]
/// first), unreadable or empty PEM files, an encrypted client key, or a malformed fingerprint.
///
/// The config does not choose the SNI name: pass [`server_name`] to the connector so `tls.server_name`
/// overrides the connect host there too. Verification already uses the override.
pub fn rustls_client_config(tls: &TlsSpec) -> AppResult<ClientConfig> {
    if !tls.is_enabled() {
        return Err(config_error(
            "TLS is disabled for this connection (mode \"disable\"); no TLS configuration to build.",
        ));
    }
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let verifier = build_verifier(tls, provider.clone())?;
    let builder = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|error| config_error(format!("Could not set up TLS: {error}")))?
        .dangerous()
        .with_custom_certificate_verifier(verifier);
    let config = match (non_empty(&tls.client_cert_file), non_empty(&tls.client_key_file)) {
        (None, None) => builder.with_no_client_auth(),
        (Some(cert_file), Some(key_file)) => {
            let certs = read_certificates(cert_file, "client certificate")?;
            let key = read_private_key(key_file)?;
            builder
                .with_client_auth_cert(certs, key)
                .map_err(|error| config_error(format!("The client certificate and key do not fit together: {error}")))?
        }
        (Some(_), None) => return Err(config_error("A client certificate needs its private key file too.")),
        (None, Some(_)) => return Err(config_error("A client key needs its certificate file too.")),
    };
    Ok(config)
}

/// The name to send as SNI and verify against: `tls.server_name` when set, else `connect_host`. Engines that
/// connect through a tunnel pass the original database host here (see `PreparedEndpoint::tls_server_name`).
pub fn server_name(tls: &TlsSpec, connect_host: &str) -> AppResult<ServerName<'static>> {
    let name = tls
        .server_name
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .unwrap_or(connect_host);
    ServerName::try_from(name.to_string())
        .map_err(|_| config_error(format!("\"{name}\" is not a valid TLS server name (a DNS name or an IP address).")))
}

/// Parses a SHA-256 certificate fingerprint: 64 hex digits (colons, spaces and a `sha256:` prefix allowed,
/// any case) or the base64 form OpenSSL and browsers show.
pub fn parse_sha256_fingerprint(text: &str) -> AppResult<[u8; 32]> {
    let trimmed = text.trim();
    let body = trimmed
        .strip_prefix("sha256:")
        .or_else(|| trimmed.strip_prefix("SHA256:"))
        .or_else(|| trimmed.strip_prefix("sha256/"))
        .unwrap_or(trimmed);
    let hex: String = body.chars().filter(|c| !matches!(c, ':' | ' ' | '-')).collect();
    if hex.len() == 64 && hex.chars().all(|c| c.is_ascii_hexdigit()) {
        let mut out = [0u8; 32];
        for (index, byte) in out.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&hex[index * 2..index * 2 + 2], 16).expect("checked hex");
        }
        return Ok(out);
    }
    use base64::Engine as _;
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(body)
        .or_else(|_| base64::engine::general_purpose::STANDARD_NO_PAD.decode(body));
    match decoded {
        Ok(bytes) if bytes.len() == 32 => Ok(bytes.try_into().expect("checked length")),
        _ => Err(config_error(format!(
            "\"{trimmed}\" is not a SHA-256 fingerprint: expected 64 hex digits (colons allowed) or 44 base64 characters."
        ))),
    }
}

/// The SHA-256 fingerprint of a DER certificate as colon-separated upper-case hex, the format
/// [`parse_sha256_fingerprint`] reads and certificate viewers show.
pub fn certificate_sha256(der: &[u8]) -> String {
    let digest = ring::digest::digest(&ring::digest::SHA256, der);
    digest
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02X}"))
        .collect::<Vec<_>>()
        .join(":")
}

// ---- native-tls ----

/// What a native-tls engine can express. tiberius (SQL Server on Windows, SChannel) takes exactly these.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeTlsOptions {
    /// Encrypt the connection (`EncryptionLevel::Required`); `false` means `NotSupported`/off.
    pub encrypt: bool,
    /// `Config::trust_cert()`: accept any certificate.
    pub trust_server_certificate: bool,
    /// `Config::trust_cert_ca(path)`: trust this CA file (host names are still checked).
    pub ca_file: Option<String>,
}

/// Maps `tls` to [`NativeTlsOptions`]. native-tls cannot pin a fingerprint or present a client certificate
/// through tiberius, so those fail with a clear message instead of being ignored. `verifyCa` is mapped to the
/// stricter full verification. `prefer` maps to encryption on, like SQL Server's own "Optional" default.
pub fn native_tls_options(tls: &TlsSpec) -> AppResult<NativeTlsOptions> {
    if !tls.is_enabled() {
        return Ok(NativeTlsOptions {
            encrypt: false,
            trust_server_certificate: false,
            ca_file: None,
        });
    }
    if non_empty(&tls.pinned_sha256).is_some() {
        return Err(config_error(
            "Certificate pinning is not available with the system TLS stack this engine uses on Windows; use a CA file or \"accept invalid certificates\".",
        ));
    }
    if non_empty(&tls.client_cert_file).is_some() || non_empty(&tls.client_key_file).is_some() {
        return Err(config_error(
            "Client certificates are not available with the system TLS stack this engine uses on Windows.",
        ));
    }
    Ok(NativeTlsOptions {
        encrypt: true,
        trust_server_certificate: tls.accept_invalid_certs,
        ca_file: non_empty(&tls.ca_file).map(str::to_string),
    })
}

// ---- Internals ----

fn non_empty(value: &Option<String>) -> Option<&str> {
    value.as_deref().map(str::trim).filter(|text| !text.is_empty())
}

fn read_certificates(path: &str, what: &str) -> AppResult<Vec<CertificateDer<'static>>> {
    let certs = CertificateDer::pem_file_iter(path)
        .and_then(|iter| iter.collect::<Result<Vec<_>, _>>())
        .map_err(|error| config_error(format!("Could not read the {what} file {path}: {error}")))?;
    if certs.is_empty() {
        return Err(config_error(format!("The {what} file {path} holds no PEM certificate.")));
    }
    Ok(certs)
}

fn read_private_key(path: &str) -> AppResult<PrivateKeyDer<'static>> {
    let text =
        std::fs::read_to_string(path).map_err(|error| config_error(format!("Could not read the client key file {path}: {error}")))?;
    if text.contains("ENCRYPTED PRIVATE KEY") || text.contains("Proc-Type: 4,ENCRYPTED") {
        return Err(config_error(format!(
            "The client key {path} is encrypted, which is not supported yet. Decrypt it with \
             `openssl pkcs8 -in key.pem -out key-plain.pem` and protect the file instead."
        )));
    }
    PrivateKeyDer::from_pem_slice(text.as_bytes())
        .map_err(|error| config_error(format!("The client key file {path} holds no usable private key: {error}")))
}

fn system_roots() -> Arc<RootCertStore> {
    static ROOTS: OnceLock<Arc<RootCertStore>> = OnceLock::new();
    ROOTS
        .get_or_init(|| {
            let mut store = RootCertStore::empty();
            store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
            // A broken system store must not break TLS: the Mozilla roots above still apply.
            let native = rustls_native_certs::load_native_certs();
            let (_added, _ignored) = store.add_parsable_certificates(native.certs);
            Arc::new(store)
        })
        .clone()
}

fn build_verifier(tls: &TlsSpec, provider: Arc<CryptoProvider>) -> AppResult<Arc<dyn ServerCertVerifier>> {
    if let Some(pin) = non_empty(&tls.pinned_sha256) {
        return Ok(Arc::new(PinnedVerifier {
            fingerprint: parse_sha256_fingerprint(pin)?,
            provider,
        }));
    }
    if tls.accept_invalid_certs {
        return Ok(Arc::new(AcceptAnyVerifier { provider }));
    }
    let roots = match non_empty(&tls.ca_file) {
        Some(path) => {
            let mut store = RootCertStore::empty();
            for cert in read_certificates(path, "CA certificate")? {
                store
                    .add(cert)
                    .map_err(|error| config_error(format!("The CA file {path} holds an unusable certificate: {error}")))?;
            }
            Arc::new(store)
        }
        None => system_roots(),
    };
    let inner = WebPkiServerVerifier::builder_with_provider(roots, provider)
        .build()
        .map_err(|error| config_error(format!("Could not set up certificate verification: {error}")))?;
    let name_override = match non_empty(&tls.server_name) {
        Some(name) => {
            Some(ServerName::try_from(name.to_string()).map_err(|_| config_error(format!("\"{name}\" is not a valid TLS server name.")))?)
        }
        None => None,
    };
    Ok(Arc::new(ChainVerifier {
        inner,
        check_name: tls.mode != TlsMode::VerifyCa,
        name_override,
    }))
}

/// WebPKI chain verification, optionally without the name check and with a fixed name.
#[derive(Debug)]
struct ChainVerifier {
    inner: Arc<WebPkiServerVerifier>,
    check_name: bool,
    name_override: Option<ServerName<'static>>,
}

impl ServerCertVerifier for ChainVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        ocsp_response: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let name = self.name_override.as_ref().unwrap_or(server_name);
        match self.inner.verify_server_cert(end_entity, intermediates, name, ocsp_response, now) {
            Err(rustls::Error::InvalidCertificate(CertificateError::NotValidForName | CertificateError::NotValidForNameContext { .. }))
                if !self.check_name =>
            {
                Ok(ServerCertVerified::assertion())
            }
            other => other,
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.inner.verify_tls12_signature(message, cert, dss)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.inner.verify_tls13_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.inner.supported_verify_schemes()
    }
}

/// Signature checks shared by the verifiers that do not use WebPKI.
macro_rules! signature_checks {
    () => {
        fn verify_tls12_signature(
            &self,
            message: &[u8],
            cert: &CertificateDer<'_>,
            dss: &DigitallySignedStruct,
        ) -> Result<HandshakeSignatureValid, rustls::Error> {
            rustls::crypto::verify_tls12_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
        }

        fn verify_tls13_signature(
            &self,
            message: &[u8],
            cert: &CertificateDer<'_>,
            dss: &DigitallySignedStruct,
        ) -> Result<HandshakeSignatureValid, rustls::Error> {
            rustls::crypto::verify_tls13_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
        }

        fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
            self.provider.signature_verification_algorithms.supported_schemes()
        }
    };
}

/// Trusts a chain that contains a certificate with the pinned SHA-256 fingerprint.
#[derive(Debug)]
struct PinnedVerifier {
    fingerprint: [u8; 32],
    provider: Arc<CryptoProvider>,
}

impl ServerCertVerifier for PinnedVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let matches = std::iter::once(end_entity)
            .chain(intermediates)
            .any(|cert| ring::digest::digest(&ring::digest::SHA256, cert.as_ref()).as_ref() == self.fingerprint);
        if matches {
            Ok(ServerCertVerified::assertion())
        } else {
            Err(rustls::Error::General(format!(
                "no certificate in the server's chain matches the pinned SHA-256 fingerprint (the server sent {})",
                certificate_sha256(end_entity.as_ref())
            )))
        }
    }

    signature_checks!();
}

/// Accepts any certificate (`accept_invalid_certs`).
#[derive(Debug)]
struct AcceptAnyVerifier {
    provider: Arc<CryptoProvider>,
}

impl ServerCertVerifier for AcceptAnyVerifier {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    signature_checks!();
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use rcgen::{BasicConstraints, CertificateParams, CertifiedIssuer, IsCa, KeyPair, KeyUsagePurpose};
    use rustls::server::WebPkiClientVerifier;
    use rustls::{ClientConnection, ServerConfig, ServerConnection};

    use super::*;

    /// A CA, a server certificate for `db.test` (+ 127.0.0.1) signed by it, a client CA and a client
    /// certificate, all written as PEM files into a fresh directory.
    struct Pki {
        dir: PathBuf,
        server_chain: Vec<CertificateDer<'static>>,
        server_key: PrivateKeyDer<'static>,
        client_ca: CertificateDer<'static>,
        ca_der: CertificateDer<'static>,
    }

    fn ca(name: &str) -> CertifiedIssuer<'static, KeyPair> {
        let mut params = CertificateParams::new(Vec::<String>::new()).unwrap();
        params.distinguished_name.push(rcgen::DnType::CommonName, name);
        params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
        CertifiedIssuer::self_signed(params, KeyPair::generate().unwrap()).unwrap()
    }

    fn pki(tag: &str) -> Pki {
        let dir = std::env::temp_dir().join(format!("fosta-net-tls-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let server_ca = ca("Fosta Test CA");
        let server_key = KeyPair::generate().unwrap();
        let server_cert = CertificateParams::new(vec!["db.test".to_string(), "127.0.0.1".to_string()])
            .unwrap()
            .signed_by(&server_key, &server_ca)
            .unwrap();
        let client_ca = ca("Fosta Client CA");
        let client_key = KeyPair::generate().unwrap();
        let client_cert = CertificateParams::new(vec!["client".to_string()])
            .unwrap()
            .signed_by(&client_key, &client_ca)
            .unwrap();
        std::fs::write(dir.join("ca.pem"), server_ca.pem()).unwrap();
        std::fs::write(dir.join("client.pem"), client_cert.pem()).unwrap();
        std::fs::write(dir.join("client-key.pem"), client_key.serialize_pem()).unwrap();
        Pki {
            server_chain: vec![server_cert.der().clone()],
            server_key: PrivateKeyDer::try_from(server_key.serialize_der()).unwrap(),
            client_ca: client_ca.der().clone(),
            ca_der: server_ca.der().clone(),
            dir,
        }
    }

    impl Pki {
        fn path(&self, name: &str) -> Option<String> {
            Some(self.dir.join(name).to_string_lossy().into_owned())
        }

        fn server(&self, require_client_cert: bool) -> Arc<ServerConfig> {
            let provider = Arc::new(rustls::crypto::ring::default_provider());
            let builder = ServerConfig::builder_with_provider(provider.clone())
                .with_safe_default_protocol_versions()
                .unwrap();
            let builder = if require_client_cert {
                let mut roots = RootCertStore::empty();
                roots.add(self.client_ca.clone()).unwrap();
                let verifier = WebPkiClientVerifier::builder_with_provider(Arc::new(roots), provider)
                    .build()
                    .unwrap();
                builder.with_client_cert_verifier(verifier)
            } else {
                builder.with_no_client_auth()
            };
            Arc::new(
                builder
                    .with_single_cert(self.server_chain.clone(), self.server_key.clone_key())
                    .unwrap(),
            )
        }

        /// A server that sends its CA certificate after the leaf.
        fn server_with_ca(&self) -> Arc<ServerConfig> {
            let provider = Arc::new(rustls::crypto::ring::default_provider());
            let chain = vec![self.server_chain[0].clone(), self.ca_der.clone()];
            Arc::new(
                ServerConfig::builder_with_provider(provider)
                    .with_safe_default_protocol_versions()
                    .unwrap()
                    .with_no_client_auth()
                    .with_single_cert(chain, self.server_key.clone_key())
                    .unwrap(),
            )
        }
    }

    impl Drop for Pki {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    /// Runs a full handshake in memory and returns the first error either side reports.
    fn handshake(client: ClientConfig, name: &str, server: Arc<ServerConfig>) -> Result<(), String> {
        let mut client = ClientConnection::new(Arc::new(client), ServerName::try_from(name.to_string()).unwrap()).unwrap();
        let mut server = ServerConnection::new(server).unwrap();
        for _ in 0..20 {
            let mut buffer = Vec::new();
            client.write_tls(&mut buffer).unwrap();
            server.read_tls(&mut buffer.as_slice()).unwrap();
            server.process_new_packets().map_err(|error| format!("server: {error}"))?;
            let mut buffer = Vec::new();
            server.write_tls(&mut buffer).unwrap();
            client.read_tls(&mut buffer.as_slice()).unwrap();
            client.process_new_packets().map_err(|error| format!("client: {error}"))?;
            if !client.is_handshaking() && !server.is_handshaking() {
                // Flush the client's last flight so the server sees the client certificate verdict.
                let mut buffer = Vec::new();
                client.write_tls(&mut buffer).unwrap();
                server.read_tls(&mut buffer.as_slice()).unwrap();
                server.process_new_packets().map_err(|error| format!("server: {error}"))?;
                return Ok(());
            }
        }
        Err("handshake did not finish".into())
    }

    fn spec(mode: TlsMode) -> TlsSpec {
        TlsSpec {
            mode,
            ..TlsSpec::default()
        }
    }

    #[test]
    fn verify_full_with_a_custom_ca_checks_chain_and_name() {
        let pki = pki("full");
        let tls = TlsSpec {
            ca_file: pki.path("ca.pem"),
            ..spec(TlsMode::VerifyFull)
        };
        handshake(rustls_client_config(&tls).unwrap(), "db.test", pki.server(false)).unwrap();
        let error = handshake(rustls_client_config(&tls).unwrap(), "other.test", pki.server(false)).unwrap_err();
        assert!(error.contains("not valid for name") || error.contains("NotValidForName"), "{error}");
        // require uses the same checks.
        let require = TlsSpec {
            mode: TlsMode::Require,
            ..tls
        };
        assert!(handshake(rustls_client_config(&require).unwrap(), "other.test", pki.server(false)).is_err());
    }

    #[test]
    fn verify_ca_checks_the_chain_but_not_the_name() {
        let pki = pki("ca");
        let tls = TlsSpec {
            ca_file: pki.path("ca.pem"),
            ..spec(TlsMode::VerifyCa)
        };
        handshake(rustls_client_config(&tls).unwrap(), "other.test", pki.server(false)).unwrap();
        // The chain still has to lead to the CA.
        let error = handshake(
            rustls_client_config(&spec(TlsMode::VerifyCa)).unwrap(),
            "db.test",
            pki.server(false),
        )
        .unwrap_err();
        assert!(error.to_lowercase().contains("unknown"), "{error}");
    }

    #[test]
    fn system_roots_reject_a_private_ca() {
        let pki = pki("system");
        let error = handshake(rustls_client_config(&spec(TlsMode::Require)).unwrap(), "db.test", pki.server(false)).unwrap_err();
        assert!(
            error.contains("UnknownIssuer") || error.to_lowercase().contains("unknown issuer"),
            "{error}"
        );
    }

    #[test]
    fn server_name_overrides_the_connect_host_for_verification_and_sni() {
        let pki = pki("sni");
        let plain = TlsSpec {
            ca_file: pki.path("ca.pem"),
            ..spec(TlsMode::VerifyFull)
        };
        // A tunnel connects to localhost, which the certificate does not name.
        assert!(handshake(rustls_client_config(&plain).unwrap(), "localhost", pki.server(false)).is_err());
        let tls = TlsSpec {
            server_name: Some("db.test".into()),
            ..plain
        };
        handshake(rustls_client_config(&tls).unwrap(), "localhost", pki.server(false)).unwrap();
        assert_eq!(server_name(&tls, "127.0.0.1").unwrap(), ServerName::try_from("db.test").unwrap());
        assert_eq!(
            server_name(&spec(TlsMode::Require), "127.0.0.1").unwrap(),
            ServerName::try_from("127.0.0.1").unwrap()
        );
        assert!(server_name(&spec(TlsMode::Require), "not a host").is_err());
    }

    #[test]
    fn accept_invalid_trusts_an_unknown_certificate() {
        let pki = pki("insecure");
        let tls = TlsSpec {
            accept_invalid_certs: true,
            ..spec(TlsMode::Require)
        };
        handshake(rustls_client_config(&tls).unwrap(), "anything.test", pki.server(false)).unwrap();
    }

    #[test]
    fn pinning_accepts_the_leaf_or_ca_fingerprint_only() {
        let pki = pki("pin");
        let leaf = certificate_sha256(pki.server_chain[0].as_ref());
        let pinned = |fingerprint: String| TlsSpec {
            pinned_sha256: Some(fingerprint),
            ..spec(TlsMode::Require)
        };
        handshake(rustls_client_config(&pinned(leaf.clone())).unwrap(), "x.test", pki.server(false)).unwrap();
        // Lower case without colons works too.
        let compact = leaf.replace(':', "").to_lowercase();
        handshake(rustls_client_config(&pinned(compact)).unwrap(), "x.test", pki.server(false)).unwrap();
        let error = handshake(
            rustls_client_config(&pinned("00".repeat(32))).unwrap(),
            "db.test",
            pki.server(false),
        )
        .unwrap_err();
        assert!(error.contains("pinned SHA-256"), "{error}");
        assert!(error.contains(&leaf), "{error}");
        // A CA pin matches only when the server sends the CA certificate in its chain.
        let ca_pin = certificate_sha256(pki.ca_der.as_ref());
        assert!(handshake(rustls_client_config(&pinned(ca_pin.clone())).unwrap(), "db.test", pki.server(false)).is_err());
        handshake(rustls_client_config(&pinned(ca_pin)).unwrap(), "db.test", pki.server_with_ca()).unwrap();
    }

    #[test]
    fn client_certificates_enable_mutual_tls() {
        let pki = pki("mtls");
        let without = TlsSpec {
            ca_file: pki.path("ca.pem"),
            ..spec(TlsMode::VerifyFull)
        };
        assert!(handshake(rustls_client_config(&without).unwrap(), "db.test", pki.server(true)).is_err());
        let with = TlsSpec {
            client_cert_file: pki.path("client.pem"),
            client_key_file: pki.path("client-key.pem"),
            ..without
        };
        handshake(rustls_client_config(&with).unwrap(), "db.test", pki.server(true)).unwrap();
    }

    #[test]
    fn configuration_mistakes_are_reported() {
        let pki = pki("mistakes");
        let code = |tls: &TlsSpec| rustls_client_config(tls).unwrap_err().code().map(str::to_string);
        assert_eq!(code(&spec(TlsMode::Disable)).as_deref(), Some(TLS_CONFIG));
        let half = TlsSpec {
            client_cert_file: pki.path("client.pem"),
            ..spec(TlsMode::Require)
        };
        assert!(rustls_client_config(&half).unwrap_err().to_string().contains("private key"));
        let missing = TlsSpec {
            ca_file: Some(Path::new("does-not-exist.pem").to_string_lossy().into_owned()),
            ..spec(TlsMode::Require)
        };
        assert!(rustls_client_config(&missing)
            .unwrap_err()
            .to_string()
            .contains("does-not-exist.pem"));
        let encrypted = pki.dir.join("encrypted.pem");
        std::fs::write(
            &encrypted,
            "-----BEGIN ENCRYPTED PRIVATE KEY-----\nAAAA\n-----END ENCRYPTED PRIVATE KEY-----\n",
        )
        .unwrap();
        let with_encrypted_key = TlsSpec {
            client_cert_file: pki.path("client.pem"),
            client_key_file: Some(encrypted.to_string_lossy().into_owned()),
            ..spec(TlsMode::Require)
        };
        assert!(rustls_client_config(&with_encrypted_key)
            .unwrap_err()
            .to_string()
            .contains("encrypted"));
        let bad_pin = TlsSpec {
            pinned_sha256: Some("zz".into()),
            ..spec(TlsMode::Require)
        };
        assert!(rustls_client_config(&bad_pin).unwrap_err().to_string().contains("SHA-256"));
    }

    #[test]
    fn fingerprints_parse_in_hex_and_base64() {
        let bytes: [u8; 32] = std::array::from_fn(|index| index as u8);
        let hex = bytes.iter().map(|b| format!("{b:02x}")).collect::<String>();
        assert_eq!(parse_sha256_fingerprint(&hex).unwrap(), bytes);
        let colons = bytes.iter().map(|b| format!("{b:02X}")).collect::<Vec<_>>().join(":");
        assert_eq!(parse_sha256_fingerprint(&format!("sha256:{colons}")).unwrap(), bytes);
        use base64::Engine as _;
        let b64 = base64::engine::general_purpose::STANDARD.encode(bytes);
        assert_eq!(parse_sha256_fingerprint(&b64).unwrap(), bytes);
        assert_eq!(parse_sha256_fingerprint(b64.trim_end_matches('=')).unwrap(), bytes);
        assert!(parse_sha256_fingerprint("abcd").is_err());
        assert_eq!(certificate_sha256(b"x").len(), 32 * 3 - 1);
    }

    #[test]
    fn native_tls_options_map_what_tiberius_supports() {
        assert!(!native_tls_options(&spec(TlsMode::Disable)).unwrap().encrypt);
        let options = native_tls_options(&TlsSpec {
            accept_invalid_certs: true,
            ca_file: Some("ca.pem".into()),
            ..spec(TlsMode::VerifyCa)
        })
        .unwrap();
        assert_eq!(
            options,
            NativeTlsOptions {
                encrypt: true,
                trust_server_certificate: true,
                ca_file: Some("ca.pem".into())
            }
        );
        assert!(native_tls_options(&TlsSpec {
            pinned_sha256: Some("00".repeat(32)),
            ..spec(TlsMode::Require)
        })
        .is_err());
        assert!(native_tls_options(&TlsSpec {
            client_cert_file: Some("c.pem".into()),
            ..spec(TlsMode::Require)
        })
        .is_err());
    }
}
