//! Row edits. ClickHouse has no transactions and no row-level updates: an update is an `ALTER TABLE … UPDATE` mutation and
//! a delete a lightweight `DELETE FROM` (or `ALTER TABLE … DELETE` on servers older than 23.3). Both only exist on
//! MergeTree tables, so every other engine is read-only here.
//!
//! The concurrency check is honest about what is possible:
//!
//! 1. Before anything is written, every update and delete is checked with `SELECT count() … WHERE <key> AND <original
//!    values>` (the same guarded WHERE the mutation uses). A row that is gone or changed is a `CONFLICT` listing the row as
//!    it is now, and nothing at all of the set is applied (inserts included).
//! 2. The mutation then runs with that same guarded WHERE and `mutations_sync=2`, so if another writer changes the row
//!    between the check and the mutation, the mutation matches nothing instead of overwriting.
//!
//! What cannot be undone is the second step across a set: the changes run one after the other, so a server error halfway
//! leaves the earlier ones applied. The error says how many were.

use super::{params_of, ChDialect, ChEngine};
use crate::{
    contract::{
        tabular::{conflict_error, RowConflict},
        ApplyResult, Field, RowChange, RowChangeSet, Value,
    },
    engines::common::{
        adapt,
        sqlbuild::{self, Binder, Param, SqlDialect},
        ColumnInfo, NodeKind, ObjectDescription, ObjectRef,
    },
    error::{AppError, AppResult},
};

/// Whether a table engine supports mutations (`ALTER … UPDATE`, `DELETE FROM`): the MergeTree family, replicated and
/// shared variants included.
pub fn supports_mutations(engine: &str) -> bool {
    engine.ends_with("MergeTree")
}

/// The table engine from a description's facts.
pub fn engine_of(description: &ObjectDescription) -> Option<&str> {
    description
        .extra
        .iter()
        .find(|(name, _)| name == "engine")
        .map(|(_, engine)| engine.as_str())
}

/// Why this object cannot be edited, or `None` when it can.
pub fn read_only_reason(description: &ObjectDescription) -> Option<String> {
    if description.object.kind != NodeKind::Table {
        return Some("Only tables can be edited; views and dictionaries are read-only.".to_owned());
    }
    match engine_of(description) {
        Some(engine) if supports_mutations(engine) => None,
        Some(engine) => Some(format!(
            "{} uses the {engine} engine. ClickHouse can change rows (as mutations) only in MergeTree tables, so this table is read-only here. Use an INSERT statement in a query tab to add rows.",
            description.object.name
        )),
        None => Some("The table engine is unknown, so the table is read-only here.".to_owned()),
    }
}

fn pairs(fields: &[Field]) -> Vec<(String, Value)> {
    fields.iter().map(|field| (field.column.clone(), field.value.clone())).collect()
}

/// A guarded change as statements: the check (`SELECT count()`) and the mutation, with their parameters.
struct Prepared {
    check: Option<(String, Vec<(String, String)>)>,
    run: (String, Vec<(String, String)>),
}

impl ChEngine {
    /// Applies a change set; see the module documentation for what is and is not atomic.
    pub async fn apply(&self, set: &RowChangeSet) -> AppResult<ApplyResult> {
        let mut object = adapt::object_to_common(&set.object)?;
        if object.database.is_none() {
            object.database = Some(self.default_database().to_owned());
        }
        let description = self.describe(&object).await?;
        if let Some(reason) = read_only_reason(&description) {
            return Err(AppError::new(reason).with("code", "READ_ONLY"));
        }
        let table = ChDialect.table_sql(&description.object);
        let lightweight_delete = self.version_numbers() >= (23, 3);
        let prepared = set
            .changes
            .iter()
            .map(|change| self.prepare(&table, &description, change, lightweight_delete))
            .collect::<AppResult<Vec<_>>>()?;

        // 1. Check every update and delete before writing anything.
        let mut conflicts = Vec::new();
        for (index, (change, prepared)) in set.changes.iter().zip(&prepared).enumerate() {
            let Some((sql, params)) = &prepared.check else { continue };
            let (_, rows) = self.http().query(sql, params).await?;
            let matched = rows
                .first()
                .and_then(|row| row.first())
                .and_then(Value::display)
                .and_then(|text| text.parse::<u64>().ok())
                .unwrap_or(0);
            match matched {
                1 => {}
                0 => {
                    let current = self.reread(&description, &object, change.key()).await?;
                    conflicts.push(RowConflict::new(index, change, current));
                }
                n => {
                    return Err(AppError::new(format!(
                        "A change matched {n} rows instead of one, so nothing was applied. ClickHouse keys are not unique; change these rows with a statement in a query tab."
                    )));
                }
            }
        }
        if !conflicts.is_empty() {
            return Err(conflict_error(&set.object, conflicts));
        }

        // 2. Apply them one after the other, waiting for each mutation.
        let mut result = ApplyResult::default();
        for (index, (change, prepared)) in set.changes.iter().zip(prepared).enumerate() {
            let (sql, params) = prepared.run;
            if let Err(error) = self.http().execute(&sql, &params).await {
                let error = AppError::from(error);
                return Err(if index == 0 {
                    error
                } else {
                    let message = format!(
                        "{error} {index} of {} changes had already been applied: ClickHouse has no transactions to roll them back.",
                        set.changes.len()
                    );
                    AppError::new(message).with("applied", index)
                });
            }
            match change {
                RowChange::Insert { .. } => result.inserted += 1,
                RowChange::Update { .. } => result.updated += 1,
                RowChange::Delete { .. } => result.deleted += 1,
            }
        }
        Ok(result)
    }

    fn prepare(&self, table: &str, description: &ObjectDescription, change: &RowChange, lightweight_delete: bool) -> AppResult<Prepared> {
        let columns: &[ColumnInfo] = &description.columns;
        let find = |name: &str| {
            description
                .column(name)
                .ok_or_else(|| AppError::new(format!("The table has no column named '{name}'.")))
        };
        let settings = |lightweight: bool| {
            let mut settings = vec![("mutations_sync".to_owned(), "2".to_owned())];
            if lightweight {
                settings.push(("lightweight_deletes_sync".to_owned(), "2".to_owned()));
            }
            settings
        };
        match change {
            RowChange::Insert { values } => {
                if values.is_empty() {
                    return Err(AppError::new("The new row has no values."));
                }
                let mut binder = Binder::new(&ChDialect);
                let mut names = Vec::new();
                let mut expressions = Vec::new();
                for field in values {
                    let column = find(&field.column)?;
                    names.push(ChDialect.quote_ident(&field.column));
                    expressions.push(if field.value.is_null() {
                        "NULL".to_owned()
                    } else {
                        binder.bind_column(field.value.clone(), column)
                    });
                }
                // A SELECT, not VALUES: query parameters are not accepted inside VALUES.
                let sql = format!("INSERT INTO {table} ({}) SELECT {}", names.join(", "), expressions.join(", "));
                Ok(Prepared {
                    check: None,
                    run: (sql, params_of(&binder.params)),
                })
            }
            RowChange::Update { key, values, .. } => {
                if values.is_empty() {
                    return Err(AppError::new("The update changes no columns."));
                }
                let mut binder = Binder::new(&ChDialect);
                let mut assignments = Vec::new();
                for field in values {
                    let column = find(&field.column)?;
                    let placeholder = if field.value.is_null() {
                        "NULL".to_owned()
                    } else {
                        binder.bind_column(field.value.clone(), column)
                    };
                    assignments.push(format!("{} = {placeholder}", ChDialect.quote_ident(&field.column)));
                }
                let where_sql = sqlbuild::guarded_where(&mut binder, key, change.check(), columns, None)?;
                let mut params = params_of(&binder.params);
                params.extend(settings(false));
                let sql = format!("ALTER TABLE {table} UPDATE {} WHERE {where_sql}", assignments.join(", "));
                Ok(Prepared {
                    check: Some(self.count_statement(table, &where_sql, &binder.params, values.len())),
                    run: (sql, params),
                })
            }
            RowChange::Delete { key, .. } => {
                let mut binder = Binder::new(&ChDialect);
                let where_sql = sqlbuild::guarded_where(&mut binder, key, change.check(), columns, None)?;
                let mut params = params_of(&binder.params);
                params.extend(settings(lightweight_delete));
                let sql = if lightweight_delete {
                    format!("DELETE FROM {table} WHERE {where_sql}")
                } else {
                    format!("ALTER TABLE {table} DELETE WHERE {where_sql}")
                };
                Ok(Prepared {
                    check: Some(self.count_statement(table, &where_sql, &binder.params, 0)),
                    run: (sql, params),
                })
            }
        }
    }

    /// `SELECT count() … WHERE <guarded>`; the first `skip` parameters belong to the SET list and are not sent (their
    /// names `{p1:…}` stay unique, so the WHERE clause keeps working with the rest).
    fn count_statement(&self, table: &str, where_sql: &str, params: &[Param], skip: usize) -> (String, Vec<(String, String)>) {
        let sent = params_of(params).into_iter().skip(skip).collect();
        (format!("SELECT count() FROM {table} WHERE {where_sql}"), sent)
    }

    /// The row as it is now (all columns), or `None` when it does not exist.
    async fn reread(&self, description: &ObjectDescription, object: &ObjectRef, key: &[Field]) -> AppResult<Option<Vec<Field>>> {
        let built = sqlbuild::build_reread(&ChDialect, &description.object, key, &description.columns)?;
        let _ = object;
        let (columns, rows) = self.http().query(&built.sql, &params_of(&built.params)).await?;
        Ok(rows.into_iter().next().map(|row| {
            columns
                .iter()
                .zip(row)
                .map(|(column, value)| Field::new(&column.name, value))
                .collect()
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(engine: &str, kind: NodeKind) -> ObjectDescription {
        let mut description = ObjectDescription::new(ObjectRef::new(Some("shop"), None, "orders", kind));
        description.extra.push(("engine".into(), engine.into()));
        description
    }

    #[test]
    fn only_mergetree_tables_are_editable() {
        for engine in ["MergeTree", "ReplacingMergeTree", "ReplicatedMergeTree", "SharedSummingMergeTree"] {
            assert!(read_only_reason(&table(engine, NodeKind::Table)).is_none(), "{engine}");
        }
        for engine in ["Log", "Memory", "Distributed", "MergeTreeX", "Kafka"] {
            assert!(read_only_reason(&table(engine, NodeKind::Table)).is_some(), "{engine}");
        }
        let reason = read_only_reason(&table("Log", NodeKind::Table)).unwrap();
        assert!(reason.contains("Log engine") && reason.contains("read-only"), "{reason}");
        assert!(read_only_reason(&table("View", NodeKind::View)).is_some());
        assert!(read_only_reason(&ObjectDescription::new(ObjectRef::table(None, "t"))).is_some());
    }

    #[test]
    fn guarded_statements_check_and_mutate_with_the_same_where() {
        use crate::contract::{ColumnHints, ValueKind};
        let mut description = table("MergeTree", NodeKind::Table);
        let column = |name: &str, type_name: &str, kind: ValueKind| ColumnInfo {
            name: name.into(),
            type_name: type_name.into(),
            kind,
            nullable: false,
            default: None,
            comment: None,
            identity: None,
            generated: None,
            hints: ColumnHints::default(),
        };
        description.columns = vec![column("id", "UInt64", ValueKind::Int), column("name", "String", ValueKind::Text)];
        let change = RowChange::Update {
            key: vec![Field::new("id", Value::Int(5))],
            values: vec![Field::new("name", Value::Text("b".into()))],
            original: vec![Field::new("name", Value::Text("a".into()))],
            version: None,
            force: false,
        };
        let engine_less = prepare_only(&description, &change);
        let (check_sql, check_params) = engine_less.check.unwrap();
        let (run_sql, run_params) = engine_less.run;
        assert!(
            run_sql.starts_with("ALTER TABLE `shop`.`orders` UPDATE `name` = {p1:String} WHERE "),
            "{run_sql}"
        );
        let where_sql = run_sql.split_once(" WHERE ").unwrap().1;
        assert!(
            where_sql.contains("`id` = {p2:UInt64}") && where_sql.contains("{p3:String}"),
            "{where_sql}"
        );
        assert_eq!(check_sql, format!("SELECT count() FROM `shop`.`orders` WHERE {where_sql}"));
        // The check does not send the SET value; the mutation sends everything and waits.
        assert!(check_params.iter().all(|(name, _)| name != "param_p1"), "{check_params:?}");
        assert!(run_params.contains(&("param_p1".to_owned(), "b".to_owned())));
        assert!(run_params.contains(&("mutations_sync".to_owned(), "2".to_owned())));
    }

    /// `prepare` needs no connection state, only the version for lightweight deletes.
    fn prepare_only(description: &ObjectDescription, change: &RowChange) -> Prepared {
        let engine = ChEngine::for_tests();
        engine
            .prepare(&ChDialect.table_sql(&description.object), description, change, true)
            .unwrap()
    }
}
