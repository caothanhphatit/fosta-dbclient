//! Dependency rules between backend layers, checked on the source text.
//!
//! Commands, the statement gateway and the assistant reach engines only through the registry (`runtime`) and the contract
//! (`contract`): they never name an engine module (`engines::<id>`). `engines::common` is allowed: it is the shared SQL
//! toolkit (statement parser and classifier), which the gateway uses to check grid filters, not an engine.

use std::path::{Path, PathBuf};

/// Layers that must stay engine-neutral.
const NEUTRAL_LAYERS: [&str; 4] = ["commands", "gateway", "ai", "mcp"];
/// Shared engine code these layers may use.
const ALLOWED_ENGINE_MODULES: [&str; 1] = ["common"];

fn src() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

/// Every engine module: the folders of `src/engines` except the shared ones.
fn engine_modules() -> Vec<String> {
    std::fs::read_dir(src().join("engines"))
        .expect("src/engines")
        .filter_map(Result::ok)
        .filter(|entry| entry.path().is_dir())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| !ALLOWED_ENGINE_MODULES.contains(&name.as_str()))
        .collect()
}

fn rust_files(dir: &Path, files: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("readable source folder").filter_map(Result::ok) {
        let path = entry.path();
        if path.is_dir() {
            rust_files(&path, files);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            files.push(path);
        }
    }
}

/// The engine modules named by `engines::…` paths in `text`, including grouped imports (`engines::{mysql, common}`).
fn engine_references(text: &str, engines: &[String]) -> Vec<String> {
    let identifier = |rest: &str| -> String { rest.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '_').collect() };
    let mut found = Vec::new();
    for (index, _) in text.match_indices("engines::") {
        let rest = &text[index + "engines::".len()..];
        let names: Vec<String> = match rest.strip_prefix('{') {
            Some(group) => group
                .split('}')
                .next()
                .unwrap_or_default()
                .split(',')
                .map(|item| identifier(item.trim()))
                .collect(),
            None => vec![identifier(rest)],
        };
        found.extend(names.into_iter().filter(|name| engines.contains(name)));
    }
    found
}

/// Removed modules that must not come back.
fn removed_references(text: &str) -> Vec<&'static str> {
    ["drivers::", "runtime::legacy", "DbManager"]
        .into_iter()
        .filter(|pattern| text.contains(pattern))
        .collect()
}

#[test]
fn commands_gateway_and_ai_do_not_reach_engine_modules() {
    let engines = engine_modules();
    assert!(engines.iter().any(|name| name == "mysql"), "{engines:?}");
    let mut violations = Vec::new();
    for layer in NEUTRAL_LAYERS {
        let mut files = Vec::new();
        rust_files(&src().join(layer), &mut files);
        assert!(!files.is_empty(), "no sources in src/{layer}");
        for file in files {
            let text = std::fs::read_to_string(&file).expect("readable source");
            let relative = file.strip_prefix(src()).unwrap_or(&file).display().to_string();
            for name in engine_references(&text, &engines) {
                violations.push(format!("{relative}: engines::{name}"));
            }
            for pattern in removed_references(&text) {
                violations.push(format!("{relative}: {pattern}"));
            }
        }
    }
    assert!(
        violations.is_empty(),
        "commands, gateway and ai must use the registry and the contract, not engine modules:\n{}",
        violations.join("\n")
    );
}

#[test]
fn the_matcher_finds_direct_and_grouped_engine_paths() {
    let engines = vec!["mysql".to_owned(), "mongo".to_owned()];
    assert_eq!(
        engine_references("use crate::engines::mysql::vector::KeyCell;", &engines),
        ["mysql"]
    );
    assert_eq!(
        engine_references("use crate::engines::{common::split, mongo::classify};", &engines),
        ["mongo"]
    );
    assert!(engine_references("use crate::engines::common::sqlclassify::parse;", &engines).is_empty());
    assert!(engine_references("crate::engines::mysqlish", &engines).is_empty());
    assert_eq!(removed_references("use crate::drivers::Driver;"), ["drivers::"]);
}

/// Driver crates: only engine modules may talk to a database library.
const DRIVER_CRATES: [&str; 8] = [
    "mysql_async::",
    "mongodb::",
    "tokio_postgres::",
    "rusqlite::",
    "oracle::",
    "redis::",
    "tiberius::",
    "elasticsearch::",
];

/// The code lines of a source file: comments (`//`, `///`, `//!`) dropped, so docs may name what code must not use.
fn code_lines(text: &str) -> impl Iterator<Item = &str> {
    text.lines().filter(|line| !line.trim_start().starts_with("//"))
}

/// Files under `src/<layer>` that satisfy `keep`, as (relative path, text).
fn layer_sources(layer: &str, keep: impl Fn(&str) -> bool) -> Vec<(String, String)> {
    let mut files = Vec::new();
    rust_files(&src().join(layer), &mut files);
    assert!(!files.is_empty(), "no sources in src/{layer}");
    files
        .into_iter()
        .map(|file| {
            let relative = file
                .strip_prefix(src())
                .unwrap_or(&file)
                .display()
                .to_string()
                .replace(std::path::MAIN_SEPARATOR, "/");
            let text = std::fs::read_to_string(&file).expect("readable source");
            (relative, text)
        })
        .filter(|(relative, _)| keep(relative))
        .collect()
}

fn is_test_source(relative: &str) -> bool {
    relative.contains("/testing/") || relative.ends_with("_tests.rs") || relative.ends_with("/tests.rs")
}

/// Lines of `text` that contain one of `patterns`, as `path: pattern`.
fn pattern_violations(relative: &str, text: &str, patterns: &[&str]) -> Vec<String> {
    code_lines(text)
        .flat_map(|line| patterns.iter().filter(move |pattern| line.contains(**pattern)))
        .map(|pattern| format!("{relative}: {pattern}"))
        .collect()
}

#[test]
fn commands_gateway_and_ai_do_not_use_driver_crates() {
    let mut violations = Vec::new();
    for layer in NEUTRAL_LAYERS {
        for (relative, text) in layer_sources(layer, |_| true) {
            violations.extend(pattern_violations(&relative, &text, &DRIVER_CRATES));
        }
    }
    assert!(
        violations.is_empty(),
        "database libraries belong to engine modules:\n{}",
        violations.join("\n")
    );
}

#[test]
fn the_contract_depends_on_no_other_layer() {
    let mut violations = Vec::new();
    for (relative, text) in layer_sources("contract", |relative| !is_test_source(relative)) {
        violations.extend(pattern_violations(
            &relative,
            &text,
            &[
                "crate::engines",
                "crate::commands",
                "crate::gateway",
                "crate::ai",
                "crate::runtime",
                "crate::server",
            ],
        ));
        violations.extend(pattern_violations(&relative, &text, &DRIVER_CRATES));
    }
    assert!(
        violations.is_empty(),
        "contract types and traits are the layer everything else depends on:\n{}",
        violations.join("\n")
    );
}

#[test]
fn engines_do_not_reach_back_into_commands_gateway_or_the_assistant() {
    let mut violations = Vec::new();
    for (relative, text) in layer_sources("engines", |relative| !is_test_source(relative)) {
        violations.extend(pattern_violations(
            &relative,
            &text,
            &["crate::commands", "crate::gateway", "crate::ai"],
        ));
    }
    assert!(
        violations.is_empty(),
        "engines implement the contract; callers reach them through the registry:\n{}",
        violations.join("\n")
    );
}

#[test]
fn engine_modules_do_not_use_each_others_driver_code() {
    // Only the shared toolkit (`engines::common`) and MariaDB's MySQL base are shared; any other engine path is a leak.
    let engines = engine_modules();
    let mut violations = Vec::new();
    for (relative, text) in layer_sources("engines", |relative| !is_test_source(relative)) {
        let own = relative.split('/').nth(1).unwrap_or_default().to_owned();
        let text: String = code_lines(&text).collect::<Vec<_>>().join("\n");
        for name in engine_references(&text, &engines) {
            if name != own && !(own == "mariadb" && name == "mysql") {
                violations.push(format!("{relative}: engines::{name}"));
            }
        }
    }
    assert!(
        violations.is_empty(),
        "engines must not name each other:\n{}",
        violations.join("\n")
    );
}

#[test]
fn the_helpers_ignore_comments_and_name_driver_crates() {
    let text = "//! uses crate::engines::mysql\nlet pool = mysql_async::Pool::new();\n";
    assert_eq!(code_lines(text).count(), 1);
    assert_eq!(pattern_violations("a.rs", text, &DRIVER_CRATES), ["a.rs: mysql_async::"]);
    assert!(is_test_source("contract/testing/suites.rs"));
    assert!(is_test_source("engines/mssql/contract_tests.rs"));
    assert!(!is_test_source("engines/mssql/adapter.rs"));
}
