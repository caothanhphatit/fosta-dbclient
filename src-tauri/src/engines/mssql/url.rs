//! SQL Server connection strings: ADO.NET `Server=…;Database=…` pairs, JDBC `jdbc:sqlserver://host:port;key=value`
//! and `sqlserver://` / `mssql://user@host:port/database` URLs. The password is returned on its own so it goes to the
//! secret store, never into the profile.

use super::descriptor::ID;
use crate::{
    contract::{engine::split_host_port, value::percent_decode, ConnectionSpec, ParsedConnection, TlsConfig, TlsMode},
    error::{AppError, AppResult},
};

const DEFAULT_PORT: u16 = 1433;

fn shape_error() -> AppError {
    AppError::new("Expected Server=host,port;Database=…;User Id=…;Password=…")
}

/// The field a connection-string key sets (case-insensitive, as ADO.NET and JDBC spell them).
fn field_of(key: &str) -> Option<&'static str> {
    Some(match key.trim().to_ascii_lowercase().as_str() {
        "server" | "data source" | "address" | "addr" | "network address" | "servername" => "server",
        "database" | "initial catalog" | "databasename" => "database",
        "user id" | "uid" | "user" | "username" => "user",
        "password" | "pwd" => "password",
        "encrypt" => "encrypt",
        "trustservercertificate" | "trust server certificate" => "trust",
        "port" | "portnumber" => "port",
        _ => return None,
    })
}

/// `key=value;` pairs where a value may be wrapped in {braces} (with `}}` as an escaped brace) to hold `;`.
fn key_value_pairs(text: &str) -> Vec<(String, String)> {
    let chars: Vec<char> = text.chars().collect();
    let mut pairs = Vec::new();
    let mut index = 0;
    while index < chars.len() {
        let Some(equals) = chars[index..].iter().position(|c| *c == '=').map(|offset| index + offset) else {
            break;
        };
        let name: String = chars[index..equals].iter().collect();
        let name = name.trim_start_matches([' ', '\t', ';']).trim().to_owned();
        let mut cursor = equals + 1;
        while chars.get(cursor) == Some(&' ') {
            cursor += 1;
        }
        let mut value = String::new();
        if chars.get(cursor) == Some(&'{') {
            cursor += 1;
            while cursor < chars.len() {
                match (chars[cursor], chars.get(cursor + 1)) {
                    ('}', Some('}')) => {
                        value.push('}');
                        cursor += 2;
                    }
                    ('}', _) => {
                        cursor += 1;
                        break;
                    }
                    (c, _) => {
                        value.push(c);
                        cursor += 1;
                    }
                }
            }
            index = chars[cursor..]
                .iter()
                .position(|c| *c == ';')
                .map_or(chars.len(), |offset| cursor + offset + 1);
        } else {
            let end = chars[cursor..]
                .iter()
                .position(|c| *c == ';')
                .map_or(chars.len(), |offset| cursor + offset);
            value = chars[cursor..end].iter().collect::<String>().trim().to_owned();
            index = end + 1;
        }
        if !name.is_empty() {
            pairs.push((name, value));
        }
    }
    pairs
}

#[derive(Default)]
struct Fields {
    server: Option<String>,
    database: Option<String>,
    user: Option<String>,
    password: Option<String>,
    encrypt: Option<String>,
    trust: Option<String>,
    port: Option<String>,
}

impl Fields {
    fn set(&mut self, key: &str, value: String) {
        let slot = match field_of(key) {
            Some("server") => &mut self.server,
            Some("database") => &mut self.database,
            Some("user") => &mut self.user,
            Some("password") => &mut self.password,
            Some("encrypt") => &mut self.encrypt,
            Some("trust") => &mut self.trust,
            Some("port") => &mut self.port,
            _ => return,
        };
        *slot = Some(value);
    }
}

/// `sqlserver://host:port;…` (JDBC) or `mssql://user:password@host:port/database?encrypt=false` (URL).
fn read_url(rest: &str, fields: &mut Fields) {
    let (main, pairs) = rest.split_once(';').unwrap_or((rest, ""));
    let (main, query) = main.split_once('?').unwrap_or((main, ""));
    let (authority, path) = main.split_once('/').unwrap_or((main, ""));
    let host = match authority.rsplit_once('@') {
        Some((credentials, host)) => {
            match credentials.split_once(':') {
                Some((user, password)) => {
                    fields.user = Some(percent_decode(user));
                    fields.password = Some(percent_decode(password));
                }
                None => fields.user = Some(percent_decode(credentials)),
            }
            host
        }
        None => authority,
    };
    fields.server = Some(host.to_owned());
    let database = percent_decode(path.trim_end_matches('/'));
    if !database.is_empty() {
        fields.database = Some(database);
    }
    for pair in query.split('&').filter(|pair| !pair.is_empty()) {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        fields.set(key, percent_decode(value));
    }
    for (key, value) in key_value_pairs(pairs) {
        fields.set(&key, value);
    }
}

pub fn parse(text: &str) -> AppResult<ParsedConnection> {
    let text = text.trim();
    if text.is_empty() {
        return Err(AppError::new("Paste a connection string first"));
    }
    let text = if text.len() >= 5 && text[..5].eq_ignore_ascii_case("jdbc:") {
        &text[5..]
    } else {
        text
    };
    let mut fields = Fields::default();
    let lower = text.to_ascii_lowercase();
    match ["sqlserver://", "mssql://"].iter().find(|scheme| lower.starts_with(*scheme)) {
        Some(scheme) => read_url(&text[scheme.len()..], &mut fields),
        None => {
            for (key, value) in key_value_pairs(text) {
                fields.set(&key, value);
            }
        }
    }
    let server = fields.server.filter(|server| !server.trim().is_empty()).ok_or_else(shape_error)?;
    // "tcp:host,1433"; "host\INSTANCE" (named instances need SQL Browser; the host is used as-is).
    let server = server.trim();
    let server = if server.len() >= 4 && server[..4].eq_ignore_ascii_case("tcp:") {
        &server[4..]
    } else {
        server
    };
    let server = server.split('\\').next().unwrap_or_default();
    let (host, parsed_port) = match server.split_once(',') {
        Some((host, port)) => (
            host.trim().to_owned(),
            port.trim()
                .parse()
                .map_err(|_| AppError::new(format!("'{port}' is not a valid port")))?,
        ),
        None => split_host_port(server, DEFAULT_PORT)?,
    };
    let port = fields.port.and_then(|port| port.trim().parse().ok()).unwrap_or(parsed_port);
    let matches = |value: &Option<String>, words: &[&str]| {
        value
            .as_deref()
            .is_some_and(|value| words.iter().any(|word| value.trim().eq_ignore_ascii_case(word)))
    };
    let encrypt = !matches(&fields.encrypt, &["false", "no", "optional", "0"]);
    let trust = matches(&fields.trust, &["true", "yes", "1"]);
    Ok(ParsedConnection {
        spec: ConnectionSpec {
            engine: ID.to_owned(),
            host,
            port,
            user: fields.user.unwrap_or_default(),
            database: fields.database.filter(|name| !name.is_empty()),
            tls: Some(TlsConfig {
                mode: if encrypt { TlsMode::Require } else { TlsMode::Disable },
                accept_invalid_certs: trust,
                ..TlsConfig::default()
            }),
            ..ConnectionSpec::default()
        },
        password: fields.password,
    })
}

/// A value as ADO.NET writes it: in braces when it holds `;`, braces or edge spaces.
fn quote(value: &str) -> String {
    if value.contains([';', '{', '}']) || value.trim() != value {
        format!("{{{}}}", value.replace('}', "}}"))
    } else {
        value.to_owned()
    }
}

/// The ADO.NET string for a spec, without a password.
pub fn format(spec: &ConnectionSpec) -> String {
    let mut parts = vec![format!("Server={},{}", quote(&spec.host), spec.port)];
    if let Some(database) = spec.database.as_deref().filter(|name| !name.is_empty()) {
        parts.push(format!("Database={}", quote(database)));
    }
    if !spec.user.is_empty() {
        parts.push(format!("User Id={}", quote(&spec.user)));
    }
    let tls = spec.tls.as_ref();
    let encrypt = tls.is_none_or(|tls| tls.mode != TlsMode::Disable);
    parts.push(format!("Encrypt={}", if encrypt { "True" } else { "False" }));
    if tls.is_some_and(|tls| tls.accept_invalid_certs) {
        parts.push("TrustServerCertificate=True".into());
    }
    parts.join(";")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_ok(text: &str) -> ParsedConnection {
        parse(text).unwrap_or_else(|error| panic!("{text}: {error}"))
    }

    fn tls_of(parsed: &ParsedConnection) -> (TlsMode, bool) {
        let tls = parsed.spec.tls.as_ref().expect("tls");
        (tls.mode, tls.accept_invalid_certs)
    }

    #[test]
    fn ado_keys_are_case_insensitive_with_tcp_and_a_port_after_a_comma() {
        let parsed = parse_ok(
            "Server=tcp:sql.example.com,1444;Initial Catalog=ERP;User ID=sa;Password=secret;Encrypt=False;TrustServerCertificate=yes",
        );
        assert_eq!(parsed.spec.engine, "mssql");
        assert_eq!((parsed.spec.host.as_str(), parsed.spec.port), ("sql.example.com", 1444));
        assert_eq!(parsed.spec.user, "sa");
        assert_eq!(parsed.password.as_deref(), Some("secret"));
        assert_eq!(parsed.spec.database.as_deref(), Some("ERP"));
        assert_eq!(tls_of(&parsed), (TlsMode::Disable, true));
        assert!(parsed.spec.options.is_empty());
    }

    #[test]
    fn braced_values_hold_semicolons_and_escaped_braces() {
        let parsed = parse_ok("Data Source=db;Database={my;db};Pwd={p;a}}ss};User Id=app");
        assert_eq!(parsed.spec.host, "db");
        assert_eq!(parsed.spec.database.as_deref(), Some("my;db"));
        assert_eq!(parsed.password.as_deref(), Some("p;a}ss"));
        assert_eq!(parsed.spec.user, "app");
    }

    #[test]
    fn the_jdbc_form() {
        let parsed = parse_ok("jdbc:sqlserver://sql01:1435;databaseName=Sales;user=reporter;password=pw;encrypt=true");
        assert_eq!((parsed.spec.host.as_str(), parsed.spec.port), ("sql01", 1435));
        assert_eq!((parsed.spec.user.as_str(), parsed.password.as_deref()), ("reporter", Some("pw")));
        assert_eq!(parsed.spec.database.as_deref(), Some("Sales"));
        assert_eq!(tls_of(&parsed), (TlsMode::Require, false));
    }

    #[test]
    fn urls_with_credentials_and_a_database() {
        let parsed = parse_ok("mssql://app:p%40ss@db.local:1500/shop?encrypt=false&trustServerCertificate=true");
        assert_eq!((parsed.spec.host.as_str(), parsed.spec.port), ("db.local", 1500));
        assert_eq!((parsed.spec.user.as_str(), parsed.password.as_deref()), ("app", Some("p@ss")));
        assert_eq!(parsed.spec.database.as_deref(), Some("shop"));
        assert_eq!(tls_of(&parsed), (TlsMode::Disable, true));
    }

    #[test]
    fn a_named_instance_is_dropped_port_is_honoured_and_tls_defaults_on() {
        let parsed = parse_ok(r"Server=db\SQLEXPRESS;Port=1500");
        assert_eq!((parsed.spec.host.as_str(), parsed.spec.port), ("db", 1500));
        assert_eq!(parsed.spec.user, "");
        assert_eq!(parsed.password, None);
        assert_eq!(parsed.spec.database, None);
        assert_eq!(tls_of(&parsed), (TlsMode::Require, false));
        assert_eq!(parse_ok("Server=db").spec.port, 1433);
    }

    #[test]
    fn a_server_is_required() {
        let error = parse("Database=ERP;User Id=sa").unwrap_err();
        assert!(error.to_string().starts_with("Expected Server="), "{error}");
        assert_eq!(parse("  ").unwrap_err().to_string(), "Paste a connection string first");
    }

    #[test]
    fn specs_format_as_ado_pairs_with_the_tls_flags() {
        let mut spec = ConnectionSpec {
            engine: "mssql".into(),
            host: "127.0.0.1".into(),
            port: 1433,
            user: "sa".into(),
            database: Some("ERP".into()),
            tls: Some(TlsConfig {
                mode: TlsMode::Disable,
                accept_invalid_certs: true,
                ..TlsConfig::default()
            }),
            ..ConnectionSpec::default()
        };
        assert_eq!(
            format(&spec),
            "Server=127.0.0.1,1433;Database=ERP;User Id=sa;Encrypt=False;TrustServerCertificate=True"
        );
        let bare = ConnectionSpec {
            user: String::new(),
            database: None,
            tls: None,
            ..spec.clone()
        };
        assert_eq!(format(&bare), "Server=127.0.0.1,1433;Encrypt=True");
        // Round trip through parse, without a password.
        spec.database = Some("my;db".into());
        let parsed = parse_ok(&format(&spec));
        assert_eq!(parsed.spec, spec);
        assert_eq!(parsed.password, None);
    }
}
