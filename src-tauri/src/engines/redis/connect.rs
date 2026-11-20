//! Connection options from a [`ConnectRequest`]: the endpoint (already rewritten by the SSH manager), the password from the
//! secret store, the form's `options.*` and the TLS files. Redis's client has its own rustls glue, so the profile's
//! certificate files are read into PEM here instead of going through `net::tls::rustls_client_config`.

use std::fs;

use super::{
    connection::{HostPort, RedisConnectOptions, RedisTls, RedisTopology, TlsVerify, DEFAULT_PORT},
    sentinel::{master_name_required, DEFAULT_SENTINEL_PORT},
};
use crate::{
    contract::{ConnectRequest, SecretSlot, TlsConfig, TlsMode},
    error::{AppError, AppResult},
};

/// The default `CLIENT SETNAME`.
pub const CLIENT_NAME: &str = "Fosta Studio";

/// Cluster and Sentinel connections reach several nodes, which one SSH tunnel cannot.
pub const SSH_REFUSAL: &str = "Redis Cluster/Sentinel through an SSH tunnel is not supported yet: the nodes announce their own \
                               addresses, which a single tunnel cannot reach. Connect to one node as Standalone, or forward \
                               each node yourself and fill Address map.";

fn read_pem(path: &Option<String>, what: &str) -> AppResult<Option<String>> {
    let Some(path) = path.as_deref().map(str::trim).filter(|path| !path.is_empty()) else {
        return Ok(None);
    };
    fs::read_to_string(path).map(Some).map_err(|error| {
        AppError::coded(
            crate::net::tls::TLS_CONFIG,
            format!("Could not read the {what} file {path}: {error}"),
        )
    })
}

/// TLS settings, or `None` for a plain connection. Redis cannot negotiate TLS, so `prefer` (the profile default, not
/// offered by this engine's form) means plain; `require` encrypts without checking the certificate, `verifyCa` checks the
/// chain but not the host name, `verifyFull` checks both (so a tunnel needs `verifyCa`, the endpoint is 127.0.0.1).
pub fn tls_options(tls: &TlsConfig) -> AppResult<Option<RedisTls>> {
    let verify = match tls.mode {
        TlsMode::Disable | TlsMode::Prefer => return Ok(None),
        _ if tls.accept_invalid_certs => TlsVerify::None,
        TlsMode::Require => TlsVerify::None,
        TlsMode::VerifyCa => TlsVerify::SkipHostname,
        TlsMode::VerifyFull => TlsVerify::Full,
    };
    let client_cert_pem = read_pem(&tls.client_cert_file, "client certificate")?;
    let client_key_pem = read_pem(&tls.client_key_file, "client key")?;
    if client_cert_pem.is_some() != client_key_pem.is_some() {
        return Err(AppError::coded(
            crate::net::tls::TLS_CONFIG,
            "Mutual TLS needs both the client certificate and its key file.",
        ));
    }
    Ok(Some(RedisTls {
        verify,
        ca_pem: read_pem(&tls.ca_file, "CA certificate")?,
        client_cert_pem,
        client_key_pem,
    }))
}

pub fn options(request: &ConnectRequest) -> AppResult<RedisConnectOptions> {
    let spec = &request.spec;
    let number = |name: &str| -> AppResult<Option<u64>> {
        spec.option(name)
            .map(|text| {
                text.trim()
                    .parse()
                    .map_err(|_| AppError::new(format!("`{text}` is not a number (option {name}).")))
            })
            .transpose()
    };
    let mut db = match spec.option("db").or(spec.database.as_deref().filter(|name| !name.is_empty())) {
        Some(text) => text
            .trim()
            .trim_start_matches("db")
            .parse::<i64>()
            .ok()
            .filter(|db| *db >= 0)
            .ok_or_else(|| AppError::new(format!("The Redis database is a number such as 0 or 3, not `{text}`.")))?,
        None => 0,
    };
    let topology = match spec.option("topology").map(str::trim).unwrap_or("standalone") {
        "standalone" => RedisTopology::Standalone,
        kind @ ("cluster" | "sentinel") => {
            if spec.ssh.as_ref().is_some_and(|ssh| ssh.enabled) {
                return Err(AppError::new(SSH_REFUSAL));
            }
            let mut nodes = vec![HostPort::new(request.endpoint.host.clone(), request.endpoint.port)];
            let default_port = if kind == "sentinel" { DEFAULT_SENTINEL_PORT } else { DEFAULT_PORT };
            for node in HostPort::parse_list(spec.option("nodes").unwrap_or_default(), default_port)? {
                if !nodes.contains(&node) {
                    nodes.push(node);
                }
            }
            if kind == "sentinel" {
                let master_name = spec.option("masterName").map(str::trim).filter(|name| !name.is_empty());
                RedisTopology::Sentinel {
                    master_name: master_name.ok_or_else(master_name_required)?.to_owned(),
                    sentinels: nodes,
                    sentinel_username: spec
                        .option("sentinelUser")
                        .map(|user| user.trim().to_owned())
                        .filter(|user| !user.is_empty()),
                    sentinel_password: request
                        .secrets
                        .get(SecretSlot::SecondaryPassword)
                        .map(str::to_owned)
                        .filter(|password| !password.is_empty()),
                    read_replica: false,
                }
            } else {
                // The form hides Database index on a cluster but keeps its saved value: database 0 regardless.
                db = 0;
                RedisTopology::Cluster {
                    seeds: nodes,
                    read_from_replicas: matches!(spec.option("readFromReplicas"), Some("true" | "1")),
                }
            }
        }
        other => {
            return Err(AppError::new(format!(
                "Unknown Redis topology `{other}`: use standalone, cluster or sentinel."
            )))
        }
    };
    let address_map = HostPort::parse_map(spec.option("addressMap").unwrap_or_default())?;
    Ok(RedisConnectOptions {
        host: request.endpoint.host.clone(),
        port: Some(request.endpoint.port),
        username: Some(spec.user.trim().to_owned()).filter(|user| !user.is_empty()),
        password: request
            .secrets
            .get(SecretSlot::Password)
            .map(str::to_owned)
            .filter(|password| !password.is_empty()),
        db,
        tls: spec.tls.as_ref().map(tls_options).transpose()?.flatten(),
        resp3: matches!(spec.option("resp3"), Some("true" | "1")),
        connect_timeout_ms: Some(request.connect_timeout.as_millis().min(u128::from(u64::MAX)) as u64).filter(|ms| *ms > 0),
        command_timeout_ms: number("commandTimeoutMs")?,
        client_name: Some(spec.option("clientName").unwrap_or(CLIENT_NAME).to_owned()),
        topology,
        address_map,
    })
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::time::Duration;

    use super::*;
    use crate::contract::{ConnectSecrets, ConnectionSpec, Endpoint};

    fn request(spec: ConnectionSpec, password: &str) -> ConnectRequest {
        ConnectRequest {
            spec,
            secrets: ConnectSecrets::password(password),
            endpoint: Endpoint {
                host: "127.0.0.1".into(),
                port: 16379,
            },
            connect_timeout: Duration::from_secs(3),
        }
    }

    #[test]
    fn the_endpoint_wins_over_the_spec_host_and_options_are_read() {
        let mut spec = ConnectionSpec {
            engine: "redis".into(),
            host: "cache.internal".into(),
            port: 6379,
            user: "app".into(),
            options: HashMap::from([("db".to_owned(), "3".to_owned()), ("resp3".to_owned(), "true".to_owned())]),
            ..Default::default()
        };
        spec.options.insert("commandTimeoutMs".into(), "5000".into());
        let options = options(&request(spec, "s3cret")).unwrap();
        assert_eq!((options.host.as_str(), options.port), ("127.0.0.1", Some(16379)));
        assert_eq!(
            (options.username.as_deref(), options.password.as_deref()),
            (Some("app"), Some("s3cret"))
        );
        assert_eq!((options.db, options.resp3, options.command_timeout_ms), (3, true, Some(5000)));
        assert_eq!(options.connect_timeout_ms, Some(3000));
        assert_eq!(options.client_name.as_deref(), Some(CLIENT_NAME));
        assert!(options.tls.is_none());
        let bad = ConnectionSpec {
            options: HashMap::from([("db".to_owned(), "x".to_owned())]),
            ..Default::default()
        };
        assert!(super::options(&request(bad, "")).is_err());
    }

    fn cluster_spec(options: &[(&str, &str)]) -> ConnectionSpec {
        ConnectionSpec {
            engine: "redis".into(),
            host: "127.0.0.1".into(),
            port: 7001,
            options: options
                .iter()
                .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
                .collect(),
            ..Default::default()
        }
    }

    #[test]
    fn the_cluster_topology_collects_seeds_replicas_and_the_address_map() {
        let spec = cluster_spec(&[
            ("topology", "cluster"),
            ("nodes", "127.0.0.1:7002, 10.0.0.3\n127.0.0.1:16379 [::1]:7004"),
            ("readFromReplicas", "true"),
            ("addressMap", "10.0.1.5:6379=127.0.0.1:17001, 10.0.1.6:6379=127.0.0.1:17002"),
        ]);
        let options = options(&request(spec, "pw")).unwrap();
        let RedisTopology::Cluster { seeds, read_from_replicas } = &options.topology else {
            panic!("cluster topology");
        };
        // The endpoint first, then the other nodes (default port 6379), without repeating the endpoint.
        assert_eq!(
            seeds,
            &vec![
                HostPort::new("127.0.0.1", 16379),
                HostPort::new("127.0.0.1", 7002),
                HostPort::new("10.0.0.3", 6379),
                HostPort::new("::1", 7004),
            ]
        );
        assert!(*read_from_replicas);
        assert_eq!(
            options.address_map[1],
            (HostPort::new("10.0.1.6", 6379), HostPort::new("127.0.0.1", 17002))
        );
        // Saved profiles without the option stay standalone.
        let plain = options_of(cluster_spec(&[])).unwrap();
        assert!(matches!(plain.topology, RedisTopology::Standalone) && plain.address_map.is_empty());
    }

    fn options_of(spec: ConnectionSpec) -> AppResult<RedisConnectOptions> {
        options(&request(spec, ""))
    }

    #[test]
    fn cluster_ignores_the_hidden_database_and_refuses_ssh_and_bad_addresses() {
        // Database index is hidden on a cluster but keeps the value saved under Standalone.
        let options = options_of(cluster_spec(&[("topology", "cluster"), ("db", "3")])).unwrap();
        assert_eq!(options.db, 0);
        let mut spec = cluster_spec(&[("topology", "cluster")]);
        spec.ssh = Some(
            serde_json::from_value(serde_json::json!({ "enabled": true, "host": "bastion", "user": "me", "auth": { "method": "agent" } }))
                .unwrap(),
        );
        assert_eq!(options_of(spec.clone()).unwrap_err().to_string(), SSH_REFUSAL);
        spec.options.insert("topology".into(), "sentinel".into());
        assert_eq!(options_of(spec.clone()).unwrap_err().to_string(), SSH_REFUSAL);
        // SSH stays fine for a standalone server.
        spec.options.insert("topology".into(), "standalone".into());
        assert!(options_of(spec).is_ok());
        assert!(options_of(cluster_spec(&[("topology", "sentinel")]))
            .unwrap_err()
            .to_string()
            .contains("master name"));
        assert!(options_of(cluster_spec(&[("topology", "mesh")])).is_err());
        assert!(options_of(cluster_spec(&[("topology", "cluster"), ("nodes", "a:x")])).is_err());
        assert!(options_of(cluster_spec(&[("addressMap", "10.0.1.5:6379")])).is_err());
    }

    #[test]
    fn the_sentinel_topology_reads_the_master_sentinels_and_the_secondary_password() {
        let spec = cluster_spec(&[
            ("topology", "sentinel"),
            ("masterName", " fosta "),
            ("nodes", "127.0.0.1:26391, sentinel-3"),
            ("sentinelUser", "watcher"),
            ("db", "3"),
            ("addressMap", "10.0.1.5:6379=127.0.0.1:16390"),
        ]);
        let mut secrets = ConnectSecrets::password("data-pw");
        secrets.values.insert(SecretSlot::SecondaryPassword, "sentinel-pw".into());
        let options = options(&ConnectRequest {
            secrets,
            ..request(spec.clone(), "")
        })
        .unwrap();
        let RedisTopology::Sentinel {
            master_name,
            sentinels,
            sentinel_username,
            sentinel_password,
            read_replica,
        } = &options.topology
        else {
            panic!("sentinel topology");
        };
        assert_eq!(master_name, "fosta");
        // The endpoint first, then the other sentinels (default port 26379).
        assert_eq!(
            sentinels,
            &vec![
                HostPort::new("127.0.0.1", 16379),
                HostPort::new("127.0.0.1", 26391),
                HostPort::new("sentinel-3", 26379),
            ]
        );
        assert_eq!(
            (sentinel_username.as_deref(), sentinel_password.as_deref(), *read_replica),
            (Some("watcher"), Some("sentinel-pw"), false)
        );
        // The data side keeps its own password and database; the address map applies to the master.
        assert_eq!((options.password.as_deref(), options.db), (Some("data-pw"), 3));
        assert_eq!(options.address_map.len(), 1);
        // No sentinel password: none is sent (the resolver falls back to the data password on NOAUTH).
        let options = options_of(spec).unwrap();
        let RedisTopology::Sentinel { sentinel_password, .. } = &options.topology else {
            panic!("sentinel topology");
        };
        assert!(sentinel_password.is_none());
        let blank = cluster_spec(&[("topology", "sentinel"), ("masterName", "  ")]);
        assert!(options_of(blank).unwrap_err().to_string().contains("master name"));
    }

    #[test]
    fn tls_modes_map_to_verification() {
        let mode = |mode| {
            tls_options(&TlsConfig {
                mode,
                ..Default::default()
            })
            .unwrap()
        };
        assert!(mode(TlsMode::Disable).is_none() && mode(TlsMode::Prefer).is_none());
        assert_eq!(mode(TlsMode::Require).unwrap().verify, TlsVerify::None);
        assert_eq!(mode(TlsMode::VerifyCa).unwrap().verify, TlsVerify::SkipHostname);
        assert_eq!(mode(TlsMode::VerifyFull).unwrap().verify, TlsVerify::Full);
        let lax = TlsConfig {
            mode: TlsMode::VerifyFull,
            accept_invalid_certs: true,
            ..Default::default()
        };
        assert_eq!(tls_options(&lax).unwrap().unwrap().verify, TlsVerify::None);
        let missing = TlsConfig {
            mode: TlsMode::VerifyCa,
            ca_file: Some("/definitely/not/here.pem".into()),
            ..Default::default()
        };
        assert_eq!(tls_options(&missing).unwrap_err().code(), Some(crate::net::tls::TLS_CONFIG));
        let half = TlsConfig {
            mode: TlsMode::VerifyCa,
            client_cert_file: Some("cert.pem".into()),
            ..Default::default()
        };
        assert!(tls_options(&half).is_err());
    }
}
