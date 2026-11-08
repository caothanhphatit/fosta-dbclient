//! MongoDB connection strings: `mongodb://` (host lists, replica sets) and `mongodb+srv://`. The profile keeps the
//! whole string without its password in `spec.url` (replica sets, SRV records and driver options live in it); the
//! password is returned on its own so it goes to the secret store.

use crate::{
    contract::{engine::split_host_port, value::percent_decode, ConnectionSpec, ParsedConnection, TlsConfig, TlsMode},
    error::{AppError, AppResult},
};

const DEFAULT_PORT: u16 = 27017;

fn shape_error() -> AppError {
    AppError::new("Expected mongodb://… or mongodb+srv://…")
}

fn flag(value: &str) -> bool {
    matches!(value.to_ascii_lowercase().as_str(), "true" | "1" | "yes")
}

pub fn parse(text: &str) -> AppResult<ParsedConnection> {
    let text = text.trim();
    if text.is_empty() {
        return Err(AppError::new("Paste a connection string first"));
    }
    let (scheme, rest) = text.split_once("://").ok_or_else(shape_error)?;
    let srv = scheme.eq_ignore_ascii_case("mongodb+srv");
    if !srv && !scheme.eq_ignore_ascii_case("mongodb") {
        return Err(shape_error());
    }
    let rest = rest.split('#').next().unwrap_or_default();
    let (main, query) = match rest.split_once('?') {
        Some((main, query)) => (main, Some(query)),
        None => (rest, None),
    };
    let (authority, path) = match main.split_once('/') {
        Some((authority, path)) => (authority, Some(path)),
        None => (main, None),
    };
    let (credentials, hosts) = match authority.rsplit_once('@') {
        Some((credentials, hosts)) => (Some(credentials), hosts),
        None => (None, authority),
    };
    if hosts.trim().is_empty() {
        return Err(shape_error());
    }
    let (raw_user, password) = match credentials {
        Some(credentials) => match credentials.split_once(':') {
            Some((user, password)) => (user, Some(percent_decode(password))),
            None => (credentials, None),
        },
        None => ("", None),
    };
    let (host, port) = if srv {
        (hosts.to_owned(), DEFAULT_PORT)
    } else {
        split_host_port(hosts.split(',').next().unwrap_or_default(), DEFAULT_PORT)?
    };
    let mut auth_source = None;
    let mut tls: Option<TlsConfig> = None;
    for pair in query.unwrap_or_default().split('&').filter(|pair| !pair.is_empty()) {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        let value = percent_decode(value);
        let required = || TlsConfig {
            mode: TlsMode::Require,
            ..TlsConfig::default()
        };
        match key.to_ascii_lowercase().as_str() {
            "authsource" if !value.is_empty() => auth_source = Some(value),
            "tls" | "ssl" => {
                tls.get_or_insert_with(required).mode = if flag(&value) { TlsMode::Require } else { TlsMode::Disable };
            }
            "tlscafile" => tls.get_or_insert_with(required).ca_file = Some(value),
            "tlscertificatekeyfile" => tls.get_or_insert_with(required).client_cert_file = Some(value),
            "tlsallowinvalidcertificates" | "tlsinsecure" => tls.get_or_insert_with(required).accept_invalid_certs = flag(&value),
            // Replica set names, read preferences, timeouts and the like stay in the string.
            _ => {}
        }
    }
    let path_database = path.map(percent_decode).filter(|name| !name.is_empty());
    // Keep everything but the password: hosts, replica set, TLS and SRV options all live in the string.
    let user_part = if raw_user.is_empty() {
        String::new()
    } else {
        format!("{raw_user}@")
    };
    let url = format!(
        "{scheme}://{user_part}{hosts}{}{}",
        path.map(|path| format!("/{path}")).unwrap_or_default(),
        query.map(|query| format!("?{query}")).unwrap_or_default()
    );
    Ok(ParsedConnection {
        spec: ConnectionSpec {
            engine: super::descriptor::ID.to_owned(),
            host,
            port,
            user: percent_decode(raw_user),
            database: auth_source.or(path_database),
            url: Some(url),
            tls,
            ..ConnectionSpec::default()
        },
        password,
    })
}

/// The string for a spec, without a password: the saved string as-is, otherwise one built from host, user and the
/// auth database.
pub fn format(spec: &ConnectionSpec) -> String {
    if let Some(url) = spec.url.as_deref().map(str::trim).filter(|url| !url.is_empty()) {
        return url.to_owned();
    }
    let encode = |text: &str| {
        text.bytes()
            .map(|b| match b {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => (b as char).to_string(),
                other => format!("%{other:02X}"),
            })
            .collect::<String>()
    };
    let user = if spec.user.is_empty() {
        String::new()
    } else {
        format!("{}@", encode(&spec.user))
    };
    let host = if spec.host.contains(':') {
        format!("[{}]", spec.host)
    } else {
        spec.host.clone()
    };
    let auth = spec
        .database
        .as_deref()
        .filter(|name| !name.is_empty())
        .map(|name| format!("?authSource={}", encode(name)))
        .unwrap_or_default();
    format!("mongodb://{user}{host}:{}/{auth}", spec.port)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_ok(text: &str) -> ParsedConnection {
        parse(text).unwrap_or_else(|error| panic!("{text}: {error}"))
    }

    #[test]
    fn srv_strings_keep_the_url_without_the_password_and_read_auth_source() {
        let parsed = parse_ok("mongodb+srv://user:secret@cluster0.example.net/?authSource=admin&retryWrites=true");
        assert_eq!(parsed.spec.engine, "mongodb");
        assert_eq!((parsed.spec.host.as_str(), parsed.spec.port), ("cluster0.example.net", 27017));
        assert_eq!(parsed.spec.user, "user");
        assert_eq!(parsed.password.as_deref(), Some("secret"));
        assert_eq!(parsed.spec.database.as_deref(), Some("admin"));
        assert_eq!(
            parsed.spec.url.as_deref(),
            Some("mongodb+srv://user@cluster0.example.net/?authSource=admin&retryWrites=true")
        );
        assert!(parsed.spec.tls.is_none(), "SRV turns TLS on inside the string");
    }

    #[test]
    fn replica_sets_use_the_first_host_and_the_path_as_the_database() {
        let parsed = parse_ok("mongodb://a:27018,b:27019/events?replicaSet=rs0");
        assert_eq!((parsed.spec.host.as_str(), parsed.spec.port), ("a", 27018));
        assert_eq!(parsed.spec.user, "");
        assert_eq!(parsed.password, None);
        assert_eq!(parsed.spec.database.as_deref(), Some("events"));
        assert_eq!(parsed.spec.url.as_deref(), Some("mongodb://a:27018,b:27019/events?replicaSet=rs0"));
    }

    #[test]
    fn auth_source_wins_over_the_path_and_credentials_are_decoded() {
        let parsed = parse_ok("mongodb://app%40corp:p%40ss%3Aw@db.local/shop?authSource=admin&readPreference=secondary");
        assert_eq!(parsed.spec.user, "app@corp");
        assert_eq!(parsed.password.as_deref(), Some("p@ss:w"));
        assert_eq!(parsed.spec.database.as_deref(), Some("admin"));
        assert_eq!((parsed.spec.host.as_str(), parsed.spec.port), ("db.local", 27017));
        assert_eq!(
            parsed.spec.url.as_deref(),
            Some("mongodb://app%40corp@db.local/shop?authSource=admin&readPreference=secondary")
        );
        let bare = parse_ok("mongodb://[::1]:27020");
        assert_eq!((bare.spec.host.as_str(), bare.spec.port), ("::1", 27020));
        assert_eq!(bare.spec.database, None);
    }

    #[test]
    fn tls_options_map_to_the_tls_config() {
        let parsed = parse_ok("mongodb://h/?tls=true&tlsCAFile=%2Fetc%2Fca.pem&tlsAllowInvalidCertificates=true");
        let tls = parsed.spec.tls.expect("tls");
        assert_eq!(tls.mode, TlsMode::Require);
        assert_eq!(tls.ca_file.as_deref(), Some("/etc/ca.pem"));
        assert!(tls.accept_invalid_certs);
        assert_eq!(parse_ok("mongodb://h/?ssl=false").spec.tls.unwrap().mode, TlsMode::Disable);
    }

    #[test]
    fn other_schemes_are_refused() {
        let error = parse("mysql://u@h/db").unwrap_err();
        assert!(error.to_string().contains("Expected mongodb://"), "{error}");
        assert!(parse("mongodb://").is_err());
        assert!(parse("  ").is_err());
    }

    #[test]
    fn format_returns_the_saved_string_or_builds_one() {
        let spec = |user: &str, database: Option<&str>, url: Option<&str>| ConnectionSpec {
            engine: "mongodb".into(),
            host: "127.0.0.1".into(),
            port: 27017,
            user: user.into(),
            database: database.map(str::to_owned),
            url: url.map(str::to_owned),
            ..ConnectionSpec::default()
        };
        assert_eq!(
            format(&spec("root", None, Some("mongodb+srv://u@x.net/"))),
            "mongodb+srv://u@x.net/"
        );
        assert_eq!(
            format(&spec("root", Some("admin"), None)),
            "mongodb://root@127.0.0.1:27017/?authSource=admin"
        );
        assert_eq!(format(&spec("", None, None)), "mongodb://127.0.0.1:27017/");
        let round = parse_ok(&format(&spec("root", Some("admin"), None)));
        assert_eq!(
            (round.spec.user.as_str(), round.spec.database.as_deref(), round.password),
            ("root", Some("admin"), None)
        );
    }
}
