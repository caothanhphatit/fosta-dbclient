//! MariaDB/MySQL connection strings: `mysql://`, `mariadb://` and `jdbc:mysql://` / `jdbc:mariadb://` URLs. The password
//! is returned on its own so it goes to the secret store, never into the profile.

use super::descriptor::Flavor;
use crate::{
    contract::{value::percent_decode, ConnectionSpec, ParsedConnection, TlsConfig, TlsMode},
    error::{AppError, AppResult},
};

const DEFAULT_PORT: u16 = 3306;

fn shape_error() -> AppError {
    AppError::new("Expected mysql://user:password@host:port/database")
}

/// `host:port` (the first of a host list), bracketed IPv6 allowed.
fn host_port(text: &str) -> AppResult<(String, u16)> {
    let first = text.split(',').next().unwrap_or_default().trim();
    if let Some(rest) = first.strip_prefix('[') {
        let (host, after) = rest.split_once(']').ok_or_else(shape_error)?;
        let port = match after.strip_prefix(':') {
            Some(port) if !port.is_empty() => port.parse().map_err(|_| AppError::new(format!("'{port}' is not a valid port")))?,
            _ => DEFAULT_PORT,
        };
        return Ok((host.to_owned(), port));
    }
    match first.rsplit_once(':') {
        Some((host, port)) if !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()) => Ok((
            host.to_owned(),
            port.parse().map_err(|_| AppError::new(format!("'{port}' is not a valid port")))?,
        )),
        _ => Ok((first.to_owned(), DEFAULT_PORT)),
    }
}

/// `ssl-mode` (MySQL clients), `sslMode` (JDBC) and `useSSL` / `ssl` flags.
fn tls_mode(value: &str) -> AppResult<TlsMode> {
    Ok(match value.to_ascii_lowercase().replace(['-', '_'], "").as_str() {
        "disabled" | "disable" | "false" | "0" | "no" => TlsMode::Disable,
        "preferred" | "prefer" | "trust" => TlsMode::Prefer,
        "required" | "require" | "true" | "1" | "yes" => TlsMode::Require,
        "verifyca" => TlsMode::VerifyCa,
        "verifyidentity" | "verifyfull" => TlsMode::VerifyFull,
        _ => return Err(AppError::new(format!("Unknown TLS mode '{value}'"))),
    })
}

pub fn parse(flavor: Flavor, text: &str) -> AppResult<ParsedConnection> {
    let text = text.trim();
    if text.is_empty() {
        return Err(AppError::new("Paste a connection string first"));
    }
    let text = text.strip_prefix("jdbc:").or_else(|| text.strip_prefix("JDBC:")).unwrap_or(text);
    let (scheme, rest) = text.split_once("://").ok_or_else(shape_error)?;
    if !scheme.eq_ignore_ascii_case("mysql") && !scheme.eq_ignore_ascii_case("mariadb") {
        return Err(shape_error());
    }
    let (main, query) = rest.split_once('?').unwrap_or((rest, ""));
    let main = main.split('#').next().unwrap_or_default();
    let (authority, path) = main.split_once('/').unwrap_or((main, ""));
    let (credentials, hosts) = match authority.rsplit_once('@') {
        Some((credentials, hosts)) => (Some(credentials), hosts),
        None => (None, authority),
    };
    if hosts.trim().is_empty() {
        return Err(shape_error());
    }
    let (mut user, mut password) = match credentials {
        Some(credentials) => match credentials.split_once(':') {
            Some((user, password)) => (percent_decode(user), Some(percent_decode(password))),
            None => (percent_decode(credentials), None),
        },
        None => (String::new(), None),
    };
    let (host, port) = host_port(hosts)?;
    let mut database = Some(percent_decode(path.trim_end_matches('/'))).filter(|name| !name.is_empty());
    let mut tls: Option<TlsConfig> = None;
    fn tls_config(tls: &mut Option<TlsConfig>) -> &mut TlsConfig {
        tls.get_or_insert_with(TlsConfig::default)
    }
    for pair in query.split('&').filter(|pair| !pair.is_empty()) {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        let value = percent_decode(value);
        match key.to_ascii_lowercase().replace(['-', '_'], "").as_str() {
            "database" | "dbname" if database.is_none() && !value.is_empty() => database = Some(value),
            "user" | "username" if user.is_empty() => user = value,
            "password" if password.is_none() => password = Some(value),
            "sslmode" | "usessl" | "ssl" | "tls" | "requiressl" => tls_config(&mut tls).mode = tls_mode(&value)?,
            "sslca" | "sslrootcert" | "serversslcert" | "trustcertificatekeystoreurl" => {
                tls_config(&mut tls).ca_file = Some(value.trim_start_matches("file:").to_owned());
            }
            "sslcert" => tls_config(&mut tls).client_cert_file = Some(value),
            "sslkey" => tls_config(&mut tls).client_key_file = Some(value),
            // Driver tuning (charset, time zone, pool sizes) does not change where to connect.
            _ => {}
        }
    }
    Ok(ParsedConnection {
        spec: ConnectionSpec {
            engine: flavor.id().to_owned(),
            host,
            port,
            user,
            database,
            tls,
            ..ConnectionSpec::default()
        },
        password,
    })
}

/// The string for a spec, without a password.
pub fn format(flavor: Flavor, spec: &ConnectionSpec) -> String {
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
    let database = spec
        .database
        .as_deref()
        .filter(|name| !name.is_empty())
        .map(|name| format!("/{}", encode(name)))
        .unwrap_or_default();
    format!("{}://{user}{host}:{}{database}", flavor.id(), spec.port)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_ok(text: &str) -> ParsedConnection {
        parse(Flavor::MariaDb, text).unwrap_or_else(|error| panic!("{text}: {error}"))
    }

    #[test]
    fn urls_give_user_password_host_port_and_database() {
        let parsed = parse_ok("mysql://root:p%40ss@db.local:3307/shop");
        assert_eq!(parsed.password.as_deref(), Some("p@ss"));
        let spec = parsed.spec;
        assert_eq!(spec.engine, "mariadb");
        assert_eq!((spec.host.as_str(), spec.port, spec.user.as_str()), ("db.local", 3307, "root"));
        assert_eq!(spec.database.as_deref(), Some("shop"));
        assert!(spec.options.is_empty() && spec.tls.is_none());
    }

    #[test]
    fn jdbc_and_mariadb_prefixes_and_a_database_parameter() {
        let parsed = parse(Flavor::MySql, "jdbc:mariadb://10.0.0.5/?database=crm").unwrap();
        assert_eq!(parsed.spec.engine, "mysql");
        assert_eq!((parsed.spec.host.as_str(), parsed.spec.port), ("10.0.0.5", 3306));
        assert_eq!(parsed.spec.user, "");
        assert_eq!(parsed.password, None);
        assert_eq!(parsed.spec.database.as_deref(), Some("crm"));
        let jdbc = parse_ok("jdbc:mysql://h:3310/app?user=svc&password=s%26cret&useUnicode=true&serverTimezone=UTC");
        assert_eq!((jdbc.spec.user.as_str(), jdbc.password.as_deref()), ("svc", Some("s&cret")));
    }

    #[test]
    fn a_string_without_a_password_keeps_the_saved_one() {
        assert_eq!(parse_ok("mysql://app@127.0.0.1").password, None);
        assert_eq!(parse_ok("mysql://app:@127.0.0.1").password.as_deref(), Some(""));
    }

    #[test]
    fn host_lists_and_bracketed_ipv6() {
        let list = parse_ok("mysql://u@a:3310,b:3311/x");
        assert_eq!((list.spec.host.as_str(), list.spec.port), ("a", 3310));
        let v6 = parse_ok("mysql://u@[::1]:3308/x");
        assert_eq!((v6.spec.host.as_str(), v6.spec.port), ("::1", 3308));
        assert_eq!(parse_ok("mysql://u@[::1]/x").spec.port, 3306);
    }

    #[test]
    fn tls_options_map_to_the_tls_settings() {
        let parsed = parse_ok("mysql://u@h/db?ssl-mode=VERIFY_IDENTITY&ssl-ca=C:/ca.pem&ssl-cert=c.pem&ssl-key=k.pem");
        let tls = parsed.spec.tls.unwrap();
        assert_eq!(tls.mode, TlsMode::VerifyFull);
        assert_eq!(tls.ca_file.as_deref(), Some("C:/ca.pem"));
        assert_eq!(tls.client_cert_file.as_deref(), Some("c.pem"));
        assert_eq!(tls.client_key_file.as_deref(), Some("k.pem"));
        assert_eq!(
            parse_ok("jdbc:mysql://h/db?sslMode=REQUIRED").spec.tls.unwrap().mode,
            TlsMode::Require
        );
        assert_eq!(parse_ok("jdbc:mysql://h/db?useSSL=false").spec.tls.unwrap().mode, TlsMode::Disable);
        assert!(parse(Flavor::MySql, "mysql://h/db?ssl-mode=sometimes").is_err());
    }

    #[test]
    fn other_schemes_and_empty_input_are_refused() {
        let error = parse(Flavor::MariaDb, "postgres://u@h/db").unwrap_err();
        assert!(error.to_string().starts_with("Expected mysql://"), "{error}");
        let empty = parse(Flavor::MariaDb, "   ").unwrap_err();
        assert_eq!(empty.to_string(), "Paste a connection string first");
        assert!(parse(Flavor::MariaDb, "mysql://u@/db").is_err());
    }

    #[test]
    fn specs_format_back_without_the_password() {
        let parsed = parse_ok("mariadb://r%20t:secret@db.local:3307/my%20db");
        let text = format(Flavor::MariaDb, &parsed.spec);
        assert_eq!(text, "mariadb://r%20t@db.local:3307/my%20db");
        assert!(!text.contains("secret"));
        assert_eq!(parse_ok(&text).spec, parsed.spec);
    }
}
