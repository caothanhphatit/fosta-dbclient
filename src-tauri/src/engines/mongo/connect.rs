//! Client options from a `ConnectRequest`, and what the server is.
//!
//! - `spec.url` set: the saved connection string (without its password) is parsed by the driver (SRV lookup, replica
//!   sets, options); the password from the secret store is added to the credential, never to the string or a log.
//! - Otherwise: the endpoint, the user and `spec.database` as the auth database.
//! - `spec.tls`: disable turns TLS off; require / verify turn it on with the CA file, a client certificate (a PEM with the
//!   key) and `accept_invalid_certs` as "allow invalid certificates"; prefer leaves the string's setting. verify-ca checks
//!   the host name like verify-full: the driver's rustls backend cannot skip only that check.

use std::path::PathBuf;

use mongodb::{
    bson::{doc, Document},
    options::{ClientOptions, Credential, ServerAddress, Tls, TlsOptions},
    Client,
};

use crate::{
    contract::{ConnectRequest, ServerInfo, TlsConfig, TlsMode},
    error::AppResult,
};

/// How the deployment is laid out; transactions need a replica set or a sharded cluster.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Topology {
    Standalone,
    ReplicaSet,
    Sharded,
}

impl Topology {
    pub fn supports_transactions(self) -> bool {
        self != Topology::Standalone
    }

    fn flavor(self) -> Option<String> {
        match self {
            Topology::Standalone => None,
            Topology::ReplicaSet => Some("replicaSet".to_owned()),
            Topology::Sharded => Some("sharded".to_owned()),
        }
    }
}

pub async fn build_options(request: &ConnectRequest) -> AppResult<ClientOptions> {
    let spec = &request.spec;
    let password = request.secrets.password_or_empty();
    let url = spec.url.as_deref().map(str::trim).filter(|url| !url.is_empty());
    let mut options = match url {
        Some(url) => {
            let mut options = ClientOptions::parse(url).await?;
            match options.credential.as_mut() {
                Some(credential) => {
                    if credential.password.is_none() && !password.is_empty() {
                        credential.password = Some(password.to_owned());
                    }
                }
                None if !spec.user.is_empty() => options.credential = Some(credential(&spec.user, password, spec.database.as_deref())),
                None => {}
            }
            options
        }
        None => {
            let mut options = ClientOptions::builder().hosts(vec![address(request)]).build();
            if !spec.user.is_empty() {
                options.credential = Some(credential(&spec.user, password, spec.database.as_deref()));
            }
            options
        }
    };
    // Through an SSH tunnel only the tunnel's end is reachable (not the string's hosts, not the replica set members the
    // server reports): connect to it directly.
    if spec.ssh.as_ref().is_some_and(|ssh| ssh.enabled) {
        options.hosts = vec![address(request)];
        options.direct_connection = Some(true);
    }
    if let Some(tls) = &spec.tls {
        apply_tls(&mut options, tls);
    }
    options.app_name = Some("Fosta Studio".to_owned());
    options.connect_timeout = Some(request.connect_timeout);
    options.server_selection_timeout = Some(request.connect_timeout);
    Ok(options)
}

fn address(request: &ConnectRequest) -> ServerAddress {
    ServerAddress::Tcp {
        host: request.endpoint.host.clone(),
        port: Some(request.endpoint.port),
    }
}

fn credential(user: &str, password: &str, auth_database: Option<&str>) -> Credential {
    Credential::builder()
        .username(user.to_owned())
        .password(password.to_owned())
        .source(auth_database.filter(|name| !name.is_empty()).map(str::to_owned))
        .build()
}

pub fn apply_tls(options: &mut ClientOptions, tls: &TlsConfig) {
    match tls.mode {
        TlsMode::Disable => options.tls = Some(Tls::Disabled),
        // MongoDB has no opportunistic TLS: keep what the string says (on for SRV).
        TlsMode::Prefer => {}
        TlsMode::Require | TlsMode::VerifyCa | TlsMode::VerifyFull => {
            let mut settings = TlsOptions::default();
            settings.ca_file_path = tls.ca_file.as_deref().filter(|path| !path.is_empty()).map(PathBuf::from);
            settings.cert_key_file_path = tls.client_cert_file.as_deref().filter(|path| !path.is_empty()).map(PathBuf::from);
            settings.allow_invalid_certificates = Some(tls.accept_invalid_certs);
            options.tls = Some(Tls::Enabled(settings));
        }
    }
}

/// `hello` (or `isMaster` on old servers): a replica set reports `setName`, a mongos `msg: "isdbgrid"`.
pub fn topology_of(hello: &Document) -> Topology {
    if hello.get_str("msg").is_ok_and(|msg| msg == "isdbgrid") {
        Topology::Sharded
    } else if hello.get_str("setName").is_ok() {
        Topology::ReplicaSet
    } else {
        Topology::Standalone
    }
}

pub fn server_info(version: &str, topology: Topology) -> ServerInfo {
    ServerInfo {
        product: "MongoDB".to_owned(),
        version: version.to_owned(),
        label: format!("MongoDB {version}").trim().to_owned(),
        flavor: topology.flavor(),
    }
}

/// Reads the version and the topology; the first command also checks the credentials.
pub async fn probe(client: &Client) -> AppResult<(ServerInfo, Topology)> {
    let admin = client.database("admin");
    let build = admin.run_command(doc! { "buildInfo": 1 }).await?;
    let version = build.get_str("version").unwrap_or_default().to_owned();
    let hello = match admin.run_command(doc! { "hello": 1 }).await {
        Ok(reply) => reply,
        Err(_) => admin.run_command(doc! { "isMaster": 1 }).await?,
    };
    let topology = topology_of(&hello);
    Ok((server_info(&version, topology), topology))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::contract::{ConnectSecrets, ConnectionSpec, Endpoint};

    fn request(spec: ConnectionSpec) -> ConnectRequest {
        ConnectRequest {
            endpoint: Endpoint {
                host: spec.host.clone(),
                port: spec.port,
            },
            spec,
            secrets: ConnectSecrets::password("s3cret"),
            connect_timeout: Duration::from_secs(3),
        }
    }

    fn spec() -> ConnectionSpec {
        ConnectionSpec {
            engine: "mongodb".into(),
            host: "db.local".into(),
            port: 27018,
            user: "app".into(),
            database: Some("admin".into()),
            ..ConnectionSpec::default()
        }
    }

    #[tokio::test]
    async fn host_fields_give_the_endpoint_and_the_auth_database() {
        let options = build_options(&request(spec())).await.unwrap();
        assert_eq!(
            options.hosts,
            vec![ServerAddress::Tcp {
                host: "db.local".into(),
                port: Some(27018)
            }]
        );
        let credential = options.credential.unwrap();
        assert_eq!(credential.username.as_deref(), Some("app"));
        assert_eq!(credential.password.as_deref(), Some("s3cret"));
        assert_eq!(credential.source.as_deref(), Some("admin"));
        assert_eq!(options.connect_timeout, Some(Duration::from_secs(3)));
        assert!(options.tls.is_none());
    }

    #[tokio::test]
    async fn the_password_is_added_to_a_url_without_one() {
        let mut with_url = spec();
        with_url.url = Some("mongodb://root@h1:27017,h2:27017/?authSource=admin&replicaSet=rs0".into());
        let options = build_options(&request(with_url)).await.unwrap();
        assert_eq!(options.hosts.len(), 2);
        assert_eq!(options.repl_set_name.as_deref(), Some("rs0"));
        let credential = options.credential.unwrap();
        assert_eq!(credential.username.as_deref(), Some("root"));
        assert_eq!(credential.password.as_deref(), Some("s3cret"));
    }

    #[tokio::test]
    async fn through_a_tunnel_only_the_tunnel_end_is_used_directly() {
        let mut tunneled = spec();
        tunneled.ssh = Some(crate::contract::SshConfig {
            enabled: true,
            host: "bastion".into(),
            port: 22,
            user: "me".into(),
            auth: crate::contract::spec::SshAuth::Agent,
            jump_hosts: Vec::new(),
            host_key: Default::default(),
            keepalive_secs: None,
        });
        let mut request = request(tunneled);
        request.endpoint = Endpoint {
            host: "127.0.0.1".into(),
            port: 50123,
        };
        let options = build_options(&request).await.unwrap();
        assert_eq!(
            options.hosts,
            vec![ServerAddress::Tcp {
                host: "127.0.0.1".into(),
                port: Some(50123)
            }]
        );
        assert_eq!(options.direct_connection, Some(true));
    }

    #[tokio::test]
    async fn tls_modes_map_to_driver_settings() {
        let mut secure = spec();
        secure.tls = Some(TlsConfig {
            mode: TlsMode::Require,
            ca_file: Some("C:/ca.pem".into()),
            accept_invalid_certs: true,
            ..TlsConfig::default()
        });
        let options = build_options(&request(secure)).await.unwrap();
        let Some(Tls::Enabled(settings)) = options.tls else {
            panic!("tls on")
        };
        assert_eq!(settings.ca_file_path, Some(PathBuf::from("C:/ca.pem")));
        assert_eq!(settings.allow_invalid_certificates, Some(true));
        let mut off = spec();
        off.url = Some("mongodb://h/?tls=true".into());
        off.tls = Some(TlsConfig {
            mode: TlsMode::Disable,
            ..TlsConfig::default()
        });
        assert!(matches!(build_options(&request(off)).await.unwrap().tls, Some(Tls::Disabled)));
    }

    #[test]
    fn topology_comes_from_hello() {
        assert_eq!(topology_of(&doc! { "isWritablePrimary": true }), Topology::Standalone);
        assert_eq!(topology_of(&doc! { "setName": "rs0" }), Topology::ReplicaSet);
        assert_eq!(topology_of(&doc! { "msg": "isdbgrid" }), Topology::Sharded);
        let info = server_info("8.0.4", Topology::ReplicaSet);
        assert_eq!((info.label.as_str(), info.flavor.as_deref()), ("MongoDB 8.0.4", Some("replicaSet")));
        assert!(Topology::Sharded.supports_transactions() && !Topology::Standalone.supports_transactions());
    }
}
