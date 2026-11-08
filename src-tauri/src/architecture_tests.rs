//! Dependency rules between backend layers, checked on the source text.
//!
//! Commands, the statement gateway and the assistant reach engines only through the registry (`runtime`) and the contract
//! (`contract`): they never name an engine module (`engines::<id>`). `engines::common` is allowed: it is the shared SQL
//! toolkit (statement parser and classifier), which the gateway uses to check grid filters, not an engine.

use std::path::{Path, PathBuf};

/// Layers that must stay engine-neutral.
const NEUTRAL_LAYERS: [&str; 3] = ["commands", "gateway", "ai"];
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
