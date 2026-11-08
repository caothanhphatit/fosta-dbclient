//! Editor support: the schema the completion needs, and SQL formatting (`crate::lang`).

use std::collections::HashMap;

use crate::{
    ai::catalog,
    contract::{Capability, ObjectDescription, ObjectNode, SchemaGraph, SqlDialectId},
    error::AppResult,
    lang::{self, schema::ForeignKeyPart, CompletionSchema, CompletionTable, Dialect, FormatOptions},
    runtime::Handle,
    AppState,
};

use super::objects::metadata_context;

/// Objects described one by one for their columns when the engine has no schema graph; the rest complete by name only.
const MAX_DESCRIBED: usize = 200;

/// The language service's dialect for a contract dialect (`None`: generic SQL).
fn dialect_of(dialect: Option<SqlDialectId>) -> Dialect {
    match dialect {
        Some(SqlDialectId::MariaDb) => Dialect::MariaDb,
        Some(SqlDialectId::MySql) => Dialect::MySql,
        Some(SqlDialectId::MsSql) => Dialect::MsSql,
        Some(SqlDialectId::Postgres) => Dialect::Postgres,
        Some(SqlDialectId::Sqlite) => Dialect::Sqlite,
        Some(SqlDialectId::ClickHouse) => Dialect::ClickHouse,
        Some(SqlDialectId::Oracle) => Dialect::Oracle,
        Some(SqlDialectId::Generic) | None => Dialect::Generic,
    }
}

/// The completion kind of a tree node (`table`, `view`, `collection`, ...).
fn kind_of(node: &ObjectNode) -> String {
    node.object
        .as_ref()
        .map_or_else(|| node.kind.as_str().to_owned(), |object| object.kind.as_str().to_owned())
}

/// A table with its name only (no columns known).
fn bare_table(node: &ObjectNode, kind: &str) -> Option<CompletionTable> {
    let object = node.object.as_ref()?;
    Some(CompletionTable::from_parts(
        object.schema.as_deref(),
        &object.name,
        kind,
        std::iter::empty(),
        &[],
        &[],
    ))
}

/// A table from its description: columns, primary key and foreign keys.
fn table_from_description(kind: &str, description: &ObjectDescription) -> CompletionTable {
    let primary: Vec<String> = description
        .columns
        .iter()
        .filter(|column| column.hints.primary)
        .map(|column| column.name.clone())
        .chain(description.primary_key.iter().flat_map(|key| key.columns.iter().cloned()))
        .collect();
    let foreign: Vec<ForeignKeyPart<'_>> = description
        .foreign_keys
        .iter()
        .map(|key| ForeignKeyPart {
            columns: &key.columns,
            ref_schema: key.ref_schema.as_deref(),
            ref_table: &key.ref_table,
            ref_columns: &key.ref_columns,
        })
        .collect();
    CompletionTable::from_parts(
        description.object.schema.as_deref(),
        &description.object.name,
        kind,
        description
            .columns
            .iter()
            .map(|column| (column.name.as_str(), column.type_name.as_str())),
        &primary,
        &foreign,
    )
}

/// A table from the engine's schema graph (matched by name); `None` when the graph does not have it.
fn table_from_graph(node: &ObjectNode, kind: &str, graph: &SchemaGraph) -> Option<CompletionTable> {
    let object = node.object.as_ref()?;
    let table = graph.tables.iter().find(|table| table.name == object.name)?;
    let primary: Vec<String> = table
        .columns
        .iter()
        .filter(|column| column.primary)
        .map(|column| column.name.clone())
        .collect();
    let foreign: Vec<ForeignKeyPart<'_>> = graph
        .relations
        .iter()
        .filter(|relation| relation.table == object.name)
        .map(|relation| ForeignKeyPart {
            columns: &relation.columns,
            ref_schema: None,
            ref_table: &relation.referenced_table,
            ref_columns: &relation.referenced_columns,
        })
        .collect();
    Some(CompletionTable::from_parts(
        object.schema.as_deref(),
        &object.name,
        kind,
        table
            .columns
            .iter()
            .map(|column| (column.name.as_str(), column.column_type.as_str())),
        &primary,
        &foreign,
    ))
}

/// The tables, views and collections of `database` with their columns, read through the contract: the object tree
/// (`children`), then the schema graph when the engine has one, else `describe` per object (capped).
async fn build_schema(handle: &Handle, state: &AppState, connection_id: &str, database: Option<String>) -> AppResult<CompletionSchema> {
    let ctx = metadata_context(state, connection_id);
    let databases = catalog::databases(handle, &ctx).await.unwrap_or_default();
    let mut schema = CompletionSchema {
        databases,
        ..CompletionSchema::default()
    };
    let Some(database) = database.filter(|name| !name.is_empty()) else {
        return Ok(schema);
    };
    let nodes: Vec<ObjectNode> = catalog::objects(handle, &ctx, &database)
        .await?
        .into_iter()
        .filter(|node| node.kind.is_relation())
        .collect();
    let graph = if handle.conn.capabilities().contains(Capability::SchemaGraph) {
        catalog::schema_overview(handle, &ctx, &database).await.ok()
    } else {
        None
    };
    let mut schemas: HashMap<String, usize> = HashMap::new();
    for (index, node) in nodes.iter().enumerate() {
        let kind = kind_of(node);
        let Some(object) = node.object.clone() else { continue };
        if let Some(name) = &object.schema {
            *schemas.entry(name.clone()).or_default() += 1;
        }
        let table = match graph.as_ref() {
            Some(graph) => table_from_graph(node, &kind, graph).or_else(|| bare_table(node, &kind)),
            None if index < MAX_DESCRIBED => match catalog::describe(handle, &ctx, object).await {
                Ok(description) => Some(table_from_description(&kind, &description)),
                Err(_) => bare_table(node, &kind),
            },
            None => bare_table(node, &kind),
        };
        if let Some(table) = table {
            schema.push_table(table);
        }
    }
    // Unqualified names resolve to the schema most objects are in (dbo, public); MySQL-like engines have none.
    schema.default_schema = schemas.into_iter().max_by_key(|(_, count)| *count).map(|(name, _)| name);
    Ok(schema)
}

/// `completion_schema`: what the editor completes for one database of an open connection. Without a database, only the
/// database names.
pub async fn completion_schema(state: &AppState, connection_id: String, database: Option<String>) -> AppResult<CompletionSchema> {
    let handle = state.engines.handle(&connection_id)?;
    build_schema(&handle, state, &connection_id, database).await
}

/// `format_sql`: whitespace and keyword case only; text the formatter cannot verify comes back unchanged.
pub fn format_sql(_state: &AppState, dialect: Option<SqlDialectId>, text: String, options: Option<FormatOptions>) -> AppResult<String> {
    Ok(lang::format_sql(dialect_of(dialect), &text, &options.unwrap_or_default()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_contract_dialect_maps_to_the_same_language_dialect() {
        for dialect in [
            SqlDialectId::MariaDb,
            SqlDialectId::MySql,
            SqlDialectId::MsSql,
            SqlDialectId::Postgres,
            SqlDialectId::Sqlite,
            SqlDialectId::ClickHouse,
            SqlDialectId::Oracle,
            SqlDialectId::Generic,
        ] {
            let contract = serde_json::to_value(dialect).unwrap();
            assert_eq!(contract, serde_json::to_value(dialect_of(Some(dialect))).unwrap());
        }
        assert_eq!(dialect_of(None), Dialect::Generic);
    }
}
