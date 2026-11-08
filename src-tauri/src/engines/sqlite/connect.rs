//! Connection options from a `ConnectRequest`: the file from the form's file field (or a connection URL), the switches
//! and the optional attached databases.

use std::{
    sync::atomic::{AtomicU32, Ordering},
    time::Duration,
};

use super::SqliteOptions;
use crate::{
    contract::{ConnectRequest, ServerInfo},
    error::{AppError, AppResult},
};

static MEMORY: AtomicU32 = AtomicU32::new(0);

fn flag(request: &ConnectRequest, key: &str) -> bool {
    matches!(request.spec.option(key), Some("true" | "1" | "yes"))
}

/// `alias=path` pairs separated by new lines or `;`. A line with only a path takes the file name as the alias.
pub fn parse_attach(text: &str) -> AppResult<Vec<(String, String)>> {
    let mut out: Vec<(String, String)> = Vec::new();
    for entry in text.split(['\n', ';']).map(str::trim).filter(|entry| !entry.is_empty()) {
        let (alias, path) = match entry.split_once('=') {
            Some((alias, path)) => (alias.trim().to_owned(), path.trim().to_owned()),
            None => {
                let stem = std::path::Path::new(entry)
                    .file_stem()
                    .and_then(|stem| stem.to_str())
                    .unwrap_or("attached");
                (stem.to_owned(), entry.to_owned())
            }
        };
        if alias.is_empty() || path.is_empty() {
            return Err(AppError::new(format!(
                "'{entry}' is not an attached database. Write it as alias=path."
            )));
        }
        if matches!(alias.to_ascii_lowercase().as_str(), "main" | "temp") || out.iter().any(|(taken, _)| taken.eq_ignore_ascii_case(&alias))
        {
            return Err(AppError::new(format!("The attached database name '{alias}' is already taken.")));
        }
        out.push((alias, path));
    }
    Ok(out)
}

pub fn build_options(request: &ConnectRequest) -> AppResult<SqliteOptions> {
    let spec = &request.spec;
    let file = spec
        .file
        .as_deref()
        .map(str::trim)
        .filter(|file| !file.is_empty())
        .ok_or_else(|| AppError::new("Choose the SQLite database file."))?;
    let mut options = SqliteOptions::file(file);
    // A private in-memory database would be a different one for every connection; a named shared one is the same for
    // the tree, the grid and every query tab of this connection.
    if file == ":memory:" {
        options.path = format!(
            "file:fosta-memory-{}?mode=memory&cache=shared",
            MEMORY.fetch_add(1, Ordering::Relaxed)
        );
    }
    options.read_only = flag(request, "readOnly");
    options.create = flag(request, "create");
    if let Some(text) = spec.option("busyTimeout") {
        let millis: u64 = text
            .trim()
            .parse()
            .map_err(|_| AppError::new(format!("The busy timeout '{text}' is not a number of milliseconds.")))?;
        options.busy_timeout = Duration::from_millis(millis);
    }
    if let Some(text) = spec.option("attach") {
        options.attach = parse_attach(text)?;
    }
    Ok(options)
}

pub fn server_info(version: &str, options: &SqliteOptions) -> ServerInfo {
    ServerInfo {
        product: "SQLite".to_owned(),
        version: version.to_owned(),
        label: format!("SQLite {version}"),
        flavor: options.read_only.then(|| "read-only".to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::{ConnectSecrets, ConnectionSpec, Endpoint};

    fn request(file: Option<&str>, options: &[(&str, &str)]) -> ConnectRequest {
        let mut spec = ConnectionSpec {
            engine: "sqlite".into(),
            file: file.map(str::to_owned),
            ..Default::default()
        };
        for (key, value) in options {
            spec.options.insert((*key).to_owned(), (*value).to_owned());
        }
        ConnectRequest {
            spec,
            secrets: ConnectSecrets::default(),
            endpoint: Endpoint {
                host: String::new(),
                port: 0,
            },
            connect_timeout: Duration::from_secs(5),
        }
    }

    #[test]
    fn the_file_field_and_switches_become_options() {
        let options = build_options(&request(
            Some("C:/data/shop.db"),
            &[
                ("readOnly", "true"),
                ("busyTimeout", "250"),
                ("attach", "audit=C:/data/audit.db\nD:/x/archive.sqlite"),
            ],
        ))
        .unwrap();
        assert_eq!(options.path, "C:/data/shop.db");
        assert!(options.read_only && !options.create);
        assert_eq!(options.busy_timeout, Duration::from_millis(250));
        assert_eq!(
            options.attach,
            vec![
                ("audit".into(), "C:/data/audit.db".into()),
                ("archive".into(), "D:/x/archive.sqlite".into())
            ]
        );
        assert!(build_options(&request(None, &[])).is_err(), "a file is required");
        assert!(build_options(&request(Some("a.db"), &[("busyTimeout", "soon")])).is_err());
        assert!(build_options(&request(Some("a.db"), &[("attach", "main=b.db")])).is_err());
        assert!(build_options(&request(Some("a.db"), &[("attach", "x=b.db;x=c.db")])).is_err());
    }

    #[test]
    fn memory_databases_are_shared_between_connections_of_one_profile() {
        let first = build_options(&request(Some(":memory:"), &[])).unwrap();
        let second = build_options(&request(Some(":memory:"), &[])).unwrap();
        assert!(first.path.starts_with("file:fosta-memory-") && first.path.contains("cache=shared"));
        assert_ne!(first.path, second.path, "two profiles must not see each other's data");
    }
}
