//! The schema snapshot the editor's completion source works from (the result of the `completion_schema` command).
//!
//! JSON shape (camelCase), mirrored by `CompletionSchema` in `src/lang/types.ts`:
//!
//! ```json
//! { "databases": ["shop"], "schemas": ["public"], "defaultSchema": "public",
//!   "tables": [ { "name": "orders", "schema": "public", "kind": "table",
//!                 "columns": [ { "name": "customer_id", "type": "int", "pk": false,
//!                                "fk": { "table": "customers", "column": "id" } } ] } ] }
//! ```

use serde::{Deserialize, Serialize};

/// Completion stops listing tables past this many (the editor still completes keywords and functions).
pub const MAX_TABLES: usize = 5_000;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompletionSchema {
    #[serde(default)]
    pub databases: Vec<String>,
    #[serde(default)]
    pub schemas: Vec<String>,
    /// The schema unqualified names resolve to (`public`, `dbo`, the current database for MySQL).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_schema: Option<String>,
    #[serde(default)]
    pub tables: Vec<CompletionTable>,
    /// True when tables were left out because of [`MAX_TABLES`].
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub truncated: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompletionTable {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<String>,
    /// `table`, `view`, `materializedView`, `foreignTable`, `dictionary`, `collection`...
    pub kind: String,
    #[serde(default)]
    pub columns: Vec<CompletionColumn>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompletionColumn {
    pub name: String,
    #[serde(rename = "type", default)]
    pub type_name: String,
    #[serde(default)]
    pub pk: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fk: Option<ForeignRef>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ForeignRef {
    pub table: String,
    pub column: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<String>,
}

/// A single-column foreign key as the builder takes it: `column` references `ref_table.ref_column`.
#[derive(Debug, Clone)]
pub struct ForeignKeyPart<'a> {
    pub columns: &'a [String],
    pub ref_schema: Option<&'a str>,
    pub ref_table: &'a str,
    pub ref_columns: &'a [String],
}

impl CompletionTable {
    /// Builds a table from catalog parts: `(name, type)` columns, the primary-key column names and the foreign keys.
    /// Composite foreign keys map column by column.
    pub fn from_parts<'a>(
        schema: Option<&str>,
        name: &str,
        kind: &str,
        columns: impl IntoIterator<Item = (&'a str, &'a str)>,
        primary_key: &[String],
        foreign_keys: &[ForeignKeyPart<'_>],
    ) -> Self {
        let columns = columns
            .into_iter()
            .map(|(col, ty)| {
                let fk = foreign_keys.iter().find_map(|fk| {
                    let pos = fk.columns.iter().position(|c| c == col)?;
                    Some(ForeignRef {
                        table: fk.ref_table.to_owned(),
                        column: fk.ref_columns.get(pos)?.clone(),
                        schema: fk.ref_schema.map(str::to_owned),
                    })
                });
                CompletionColumn {
                    name: col.to_owned(),
                    type_name: ty.to_owned(),
                    pk: primary_key.iter().any(|p| p == col),
                    fk,
                }
            })
            .collect();
        Self {
            name: name.to_owned(),
            schema: schema.map(str::to_owned),
            kind: kind.to_owned(),
            columns,
        }
    }
}

impl CompletionSchema {
    /// Adds a table unless [`MAX_TABLES`] is reached (then marks the schema truncated).
    pub fn push_table(&mut self, table: CompletionTable) {
        if self.tables.len() >= MAX_TABLES {
            self.truncated = true;
            return;
        }
        if let Some(schema) = &table.schema {
            if !self.schemas.contains(schema) {
                self.schemas.push(schema.clone());
            }
        }
        self.tables.push(table);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_tables_and_serializes_camel_case() {
        let cols = [("id".to_owned(), "int".to_owned()), ("customer_id".to_owned(), "int".to_owned())];
        let fk_cols = ["customer_id".to_owned()];
        let ref_cols = ["id".to_owned()];
        let table = CompletionTable::from_parts(
            Some("public"),
            "orders",
            "table",
            cols.iter().map(|(n, t)| (n.as_str(), t.as_str())),
            &["id".to_owned()],
            &[ForeignKeyPart {
                columns: &fk_cols,
                ref_schema: None,
                ref_table: "customers",
                ref_columns: &ref_cols,
            }],
        );
        let mut schema = CompletionSchema {
            databases: vec!["shop".into()],
            default_schema: Some("public".into()),
            ..Default::default()
        };
        schema.push_table(table);
        let json = serde_json::to_value(&schema).unwrap();
        assert_eq!(json["defaultSchema"], "public");
        assert_eq!(json["schemas"][0], "public");
        assert_eq!(json["tables"][0]["columns"][0]["pk"], true);
        assert_eq!(json["tables"][0]["columns"][0]["type"], "int");
        assert_eq!(json["tables"][0]["columns"][1]["fk"]["table"], "customers");
        assert!(json.get("truncated").is_none());
        let back: CompletionSchema = serde_json::from_value(json).unwrap();
        assert_eq!(back, schema);
    }

    #[test]
    fn caps_the_table_count() {
        let mut schema = CompletionSchema::default();
        for i in 0..MAX_TABLES + 3 {
            schema.push_table(CompletionTable {
                name: format!("t{i}"),
                kind: "table".into(),
                ..Default::default()
            });
        }
        assert_eq!(schema.tables.len(), MAX_TABLES);
        assert!(schema.truncated);
    }
}
