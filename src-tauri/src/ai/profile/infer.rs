//! Relations inferred from names where no foreign key is declared: a column `customer_id` (`customerId`, `customerID`,
//! `fk_customer`) points at an object `customer` or `customers` (or `categories` for `category_id`) and its key. Only
//! described objects take part; a column that already has a declared key is left alone.

use std::collections::HashSet;

use super::model::{ObjectProfile, RelationProfile, RelationSource};
use crate::contract::ValueKind;

/// The target has a one-column key named like the column expects and the types agree.
const CONFIDENT: f32 = 0.8;
/// The names match, the key types do not (or are unknown).
const LIKELY: f32 = 0.5;

/// The object name a referencing column names, lower case: `customer_id` → `customer`.
fn referenced_stem(column: &str) -> Option<String> {
    let lower = column.to_ascii_lowercase();
    let stem = if let Some(stem) = lower.strip_prefix("fk_") {
        stem.strip_suffix("_id").unwrap_or(stem)
    } else if let Some(stem) = lower.strip_suffix("_id") {
        stem
    } else if column.ends_with("Id") || column.ends_with("ID") {
        &lower[..lower.len() - 2]
    } else {
        return None;
    };
    let stem = stem.trim_end_matches('_');
    (!stem.is_empty() && stem != "parent").then(|| stem.to_owned())
}

/// The object names a stem may stand for: itself and its plurals.
fn candidates(stem: &str) -> Vec<String> {
    let mut names = vec![stem.to_owned(), format!("{stem}s"), format!("{stem}es")];
    if let Some(base) = stem.strip_suffix('y') {
        names.push(format!("{base}ies"));
    }
    names
}

/// The key column of `target` a reference to it uses: its one-column primary key, else `id` / `_id`.
fn key_of(target: &ObjectProfile) -> Option<&str> {
    match target.primary_key.as_slice() {
        [key] => Some(key.as_str()),
        [] => target
            .columns
            .iter()
            .find(|column| matches!(column.name.as_str(), "id" | "_id" | "ID" | "Id"))
            .map(|column| column.name.as_str()),
        _ => None,
    }
}

fn compatible(a: &ValueKind, b: &ValueKind) -> bool {
    a == b || (a.is_numeric() && b.is_numeric()) || matches!((a, b), (ValueKind::Other, _) | (_, ValueKind::Other))
}

/// Relations implied by column names, beside the `declared` ones.
pub fn infer_relations(objects: &[ObjectProfile], declared: &[RelationProfile]) -> Vec<RelationProfile> {
    let taken: HashSet<(String, String)> = declared
        .iter()
        .flat_map(|relation| relation.columns.iter().map(|column| (relation.from.clone(), column.clone())))
        .collect();
    let detailed: Vec<&ObjectProfile> = objects.iter().filter(|object| object.detailed).collect();
    let mut inferred = Vec::new();
    for object in &detailed {
        let from = object.qualified();
        for column in &object.columns {
            if column.primary || taken.contains(&(from.clone(), column.name.clone())) {
                continue;
            }
            let Some(stem) = referenced_stem(&column.name) else { continue };
            let names = candidates(&stem);
            // Prefer a target in the same schema, then anywhere.
            let target = detailed
                .iter()
                .filter(|target| names.iter().any(|name| target.name.eq_ignore_ascii_case(name)))
                .min_by_key(|target| target.schema != object.schema);
            let Some(target) = target else { continue };
            if target.qualified() == from {
                continue;
            }
            let Some(key) = key_of(target) else { continue };
            let confidence = match target.column(key) {
                Some(key_column) if compatible(&key_column.kind, &column.kind) => CONFIDENT,
                _ => LIKELY,
            };
            inferred.push(RelationProfile {
                name: None,
                from: from.clone(),
                columns: vec![column.name.clone()],
                to: target.qualified(),
                to_columns: vec![key.to_owned()],
                to_database: None,
                source: RelationSource::InferredName,
                confidence,
            });
        }
    }
    inferred
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stems_follow_common_naming() {
        assert_eq!(referenced_stem("customer_id").as_deref(), Some("customer"));
        assert_eq!(referenced_stem("customerId").as_deref(), Some("customer"));
        assert_eq!(referenced_stem("customerID").as_deref(), Some("customer"));
        assert_eq!(referenced_stem("fk_customer").as_deref(), Some("customer"));
        assert_eq!(referenced_stem("id"), None);
        assert_eq!(referenced_stem("_id"), None);
        assert_eq!(referenced_stem("parent_id"), None);
        assert_eq!(referenced_stem("valid"), None);
        assert!(candidates("category").contains(&"categories".to_owned()));
    }
}
