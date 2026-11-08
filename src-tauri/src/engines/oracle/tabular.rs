//! Browsing and editing rows for the contract. Browse SQL comes from `engines::common::sqlbuild` with every value bound.
//!
//! Keys: the primary key, else (for heap tables) the ROWID, browsed as the pseudo column `_rowid_`. Views and materialized
//! views have no key and are read-only.
//!
//! Optimistic concurrency compares the original values, NULL-safe (`(col = :a OR (col IS NULL AND :b IS NULL))`), on every
//! comparable column the grid sends; LOB, LONG, JSON and XMLTYPE columns cannot be compared with `=` and are left out of the
//! check. `ORA_ROWSCN` is not used: without `ROWDEPENDENCIES` (off by default and fixed at CREATE TABLE) it is tracked per
//! block, so a change to any other row in the same block would look like a conflict. Edits run in one transaction; a change
//! that matches no row rolls everything back with a `CONFLICT` carrying the row as it is now.

use std::time::Instant;

use oracle::Connection;

use super::{binds, describe_relation, descriptor, estimate, map_err, nodes, one, refs, types, OraDialect, ROWID};
use crate::{
    contract::{
        tabular::{conflict_error, RowConflict},
        ApplyResult, BrowseRequest, ColumnInfo, EngineError, Field, Filter, Page, RowChange, RowChangeSet, Total, Value,
    },
    engines::common::{
        self, adapt,
        sqlbuild::{self, SqlDialect},
        ObjectDescription, ValueKind,
    },
    error::{AppError, AppResult},
};

/// Pages are capped so one request cannot pull a whole table.
const MAX_PAGE_SIZE: u64 = 5000;

pub(super) fn app(error: EngineError) -> AppError {
    AppError::from(error).with_engine(descriptor::ID)
}

fn ora(error: oracle::Error) -> AppError {
    app(map_err(error))
}

/// A table or view as browsed and edited: its description (without DDL), the columns (plus `_rowid_` for a heap table
/// without a primary key) and the key columns.
pub(super) struct Shape {
    pub description: ObjectDescription,
    pub columns: Vec<ColumnInfo>,
    pub key: Vec<String>,
}

pub(super) fn shape(c: &Connection, object: &common::ObjectRef, login: &str) -> AppResult<Shape> {
    let owner = object.schema.clone().unwrap_or_else(|| login.to_owned());
    let found = one(
        c,
        "SELECT o.object_type, (SELECT t.iot_type || '/' || t.temporary || '/' || \
                (SELECT 'X' FROM all_external_tables x WHERE x.owner = t.owner AND x.table_name = t.table_name) \
                FROM all_tables t WHERE t.owner = o.owner AND t.table_name = o.object_name) \
         FROM all_objects o WHERE o.owner = :1 AND o.object_name = :2 AND o.object_type IN ('TABLE', 'VIEW', 'MATERIALIZED VIEW') \
         ORDER BY CASE o.object_type WHEN 'MATERIALIZED VIEW' THEN 0 ELSE 1 END",
        &[&owner, &object.name],
    )
    .map_err(app)?
    .ok_or_else(|| AppError::new(format!("There is no table or view named '{}' in schema '{owner}'.", object.name)))?;
    let kind = match found[0].as_deref() {
        Some("VIEW") => common::NodeKind::View,
        Some("MATERIALIZED VIEW") => common::NodeKind::MaterializedView,
        _ => common::NodeKind::Table,
    };
    let mut description = ObjectDescription::new(common::ObjectRef::new(None, Some(&owner), &object.name, kind));
    describe_relation(c, &mut description, &owner).map_err(app)?;
    let mut columns = description.columns.clone();
    for column in &mut columns {
        column.hints.read_only |= !column.is_writable();
    }
    // Index-organized (no physical ROWID) and external tables (no ROWID at all) have no ROWID to fall back on.
    let heap = found[1]
        .as_deref()
        .is_some_and(|flags| flags.starts_with('/') && !flags.ends_with('X'));
    let key = match &description.primary_key {
        Some(pk) => pk.columns.clone(),
        None if kind == common::NodeKind::Table && heap => {
            let mut rowid = ColumnInfo::result(ROWID, "ROWID", ValueKind::Text);
            rowid.nullable = false;
            rowid.identity = Some("ROWID".into());
            rowid.hints.read_only = true;
            rowid.hints.primary = true;
            columns.insert(0, rowid);
            vec![ROWID.to_owned()]
        }
        None => Vec::new(),
    };
    for column in &mut columns {
        column.hints.primary |= key.contains(&column.name);
    }
    Ok(Shape { description, columns, key })
}

fn read_rows(c: &Connection, sql: &str, params: &[sqlbuild::Param], columns: &[ColumnInfo]) -> AppResult<Vec<Vec<Value>>> {
    let b = binds(params);
    let rows = c.query(sql, &refs(&b)).map_err(ora)?;
    let mut out = Vec::new();
    for row in rows {
        out.push(types::read_row(columns, row.map_err(ora)?.sql_values()));
    }
    Ok(out)
}

pub(super) fn browse_sync(c: &Connection, request: &BrowseRequest, login: &str) -> AppResult<Page> {
    let started = Instant::now();
    let object = nodes::to_core(&request.object, login)?;
    let Shape { description, columns, key } = shape(c, &object, login)?;
    let shown: Vec<ColumnInfo> = match &request.columns {
        Some(wanted) => columns
            .into_iter()
            .filter(|column| wanted.contains(&column.name) || key.contains(&column.name))
            .collect(),
        None => columns,
    };
    let raw = match &request.filter {
        Filter::Raw { text, .. } => Some(text.clone()),
        _ => None,
    };
    let mut core = adapt::browse_to_common(&BrowseRequest {
        filter: if raw.is_some() { Filter::None } else { request.filter.clone() },
        ..request.clone()
    })?;
    core.object = description.object.clone();
    core.limit = request.limit.clamp(1, MAX_PAGE_SIZE);
    let built = sqlbuild::build_browse_with_raw(&OraDialect, &core, &shown, &key, raw.as_deref()).map_err(app)?;
    let rows = read_rows(c, &built.sql, &built.params, &shown)?;
    let total = if request.exact_total {
        let where_clause = if built.where_sql.is_empty() {
            String::new()
        } else {
            format!(" WHERE {}", built.where_sql)
        };
        // The keyset parameters come after the filter's.
        let keyset = request.after.as_ref().map_or(0, |after| after.len() * (after.len() + 1) / 2);
        let filter_params = &built.params[..built.params.len() - keyset];
        let sql = format!("SELECT COUNT(*) FROM {}{where_clause}", OraDialect.table_sql(&description.object));
        let b = binds(filter_params);
        let count: i64 = c.query_row_as(&sql, &refs(&b)).map_err(ora)?;
        Some(Total {
            value: count.max(0) as u64,
            exact: true,
        })
    } else if request.estimate_total {
        let estimated = if built.where_sql.is_empty() {
            description.row_estimate
        } else {
            estimate(c, &description.object, &built)
        };
        estimated.map(|rows| Total {
            value: rows.max(0) as u64,
            exact: false,
        })
    } else {
        None
    };
    let elapsed_ms = started.elapsed().as_millis() as u64;
    let mut page = adapt::page_to_contract(sqlbuild::finish_page(shown, rows, core.limit, &built, key, None), elapsed_ms);
    page.total = total;
    Ok(page)
}

/// The change with only the original values that can be compared (see the module comment).
fn comparable_change(change: &RowChange, columns: &[ColumnInfo]) -> RowChange {
    let keep = |fields: &[Field]| -> Vec<Field> {
        fields
            .iter()
            .filter(|field| columns.iter().any(|c| c.name == field.column && super::comparable(c)))
            .cloned()
            .collect()
    };
    match change {
        RowChange::Update {
            key,
            values,
            original,
            version,
            force,
        } => RowChange::Update {
            key: key.clone(),
            values: values.clone(),
            original: keep(original),
            version: version.clone(),
            force: *force,
        },
        RowChange::Delete {
            key,
            original,
            version,
            force,
        } => RowChange::Delete {
            key: key.clone(),
            original: keep(original),
            version: version.clone(),
            force: *force,
        },
        insert => insert.clone(),
    }
}

pub(super) fn apply_sync(c: &Connection, set: &RowChangeSet, login: &str) -> AppResult<ApplyResult> {
    let object = nodes::to_core(&set.object, login)?;
    let Shape { description, columns, key } = shape(c, &object, login)?;
    if description.object.kind != common::NodeKind::Table {
        return Err(AppError::new(format!(
            "{} is not a table, so its rows are read-only.",
            description.object.name
        )));
    }
    if key.is_empty() {
        return Err(AppError::new(format!(
            "{} has no primary key and no ROWID, so its rows cannot be changed safely.",
            description.object.name
        )));
    }
    let statements = set
        .changes
        .iter()
        .map(|change| {
            sqlbuild::build_guarded(
                &OraDialect,
                &description.object,
                &comparable_change(change, &columns),
                &columns,
                None,
            )
            .map_err(app)
        })
        .collect::<AppResult<Vec<_>>>()?;
    let outcome = (|| {
        let mut result = ApplyResult::default();
        let mut missed = Vec::new();
        for (index, (change, built)) in set.changes.iter().zip(&statements).enumerate() {
            let b = binds(&built.params);
            let affected = c.execute(&built.sql, &refs(&b)).map_err(ora)?.row_count().map_err(ora)?;
            match change {
                RowChange::Insert { .. } => result.inserted += affected,
                RowChange::Update { .. } if affected == 0 => missed.push(index),
                RowChange::Update { .. } => result.updated += affected,
                RowChange::Delete { .. } if affected == 0 => missed.push(index),
                RowChange::Delete { .. } => result.deleted += affected,
            }
        }
        Ok::<_, AppError>((result, missed))
    })();
    let (result, missed) = match outcome {
        Ok(done) => done,
        Err(error) => {
            let _ = c.rollback();
            return Err(error);
        }
    };
    if missed.is_empty() {
        c.commit().map_err(ora)?;
        return Ok(result);
    }
    c.rollback().map_err(ora)?;
    let mut conflicts = Vec::with_capacity(missed.len());
    for index in missed {
        let change = &set.changes[index];
        let current = reread(c, &description.object, change.key(), &columns)?;
        conflicts.push(RowConflict::new(index, change, current));
    }
    Err(conflict_error(&set.object, conflicts))
}

/// The row with this key as it is now (all columns), or `None` when it no longer exists.
fn reread(c: &Connection, object: &common::ObjectRef, key: &[Field], columns: &[ColumnInfo]) -> AppResult<Option<Vec<Field>>> {
    let built = sqlbuild::build_reread(&OraDialect, object, key, columns).map_err(app)?;
    Ok(read_rows(c, &built.sql, &built.params, columns)?.into_iter().next().map(|values| {
        columns
            .iter()
            .zip(values)
            .map(|(column, value)| Field::new(&column.name, value))
            .collect()
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn column(name: &str, type_name: &str, kind: ValueKind) -> ColumnInfo {
        ColumnInfo::result(name, type_name, kind)
    }

    #[test]
    fn lob_and_json_originals_are_left_out_of_the_check() {
        let columns = vec![
            column("id", "NUMBER(10)", ValueKind::Int),
            column("name", "VARCHAR2(100)", ValueKind::Text),
            column("doc", "CLOB", ValueKind::Text),
            column("meta", "JSON", ValueKind::Json),
        ];
        let change = RowChange::Update {
            key: vec![Field::new("id", Value::Int(1))],
            values: vec![Field::new("name", Value::Text("b".into()))],
            original: vec![
                Field::new("name", Value::Text("a".into())),
                Field::new("doc", Value::Text("long".into())),
                Field::new("meta", Value::Json("{}".into())),
            ],
            version: None,
            force: false,
        };
        let RowChange::Update { original, .. } = comparable_change(&change, &columns) else {
            unreachable!()
        };
        assert_eq!(original, vec![Field::new("name", Value::Text("a".into()))]);
        let object = common::ObjectRef::new(None, Some("SHOP"), "T", common::NodeKind::Table);
        let built = sqlbuild::build_guarded(&OraDialect, &object, &comparable_change(&change, &columns), &columns, None).unwrap();
        assert_eq!(
            built.sql,
            "UPDATE \"SHOP\".\"T\" SET \"name\" = :1 WHERE \"id\" = :2 AND (\"name\" = :3 OR (\"name\" IS NULL AND :4 IS NULL))"
        );
    }

    #[test]
    fn rowid_keys_select_and_compare_through_conversions() {
        let mut rowid = column(ROWID, "ROWID", ValueKind::Text);
        rowid.nullable = false;
        let columns = vec![rowid, column("qty", "NUMBER", ValueKind::Decimal)];
        let object = common::ObjectRef::new(None, Some("SHOP"), "LOOSE", common::NodeKind::Table);
        let change = RowChange::Delete {
            key: vec![Field::new(ROWID, Value::Text("AAAR3sAAEAAAACXAAA".into()))],
            original: vec![Field::new("qty", Value::Decimal("2".into()))],
            version: None,
            force: false,
        };
        let built = sqlbuild::build_guarded(&OraDialect, &object, &change, &columns, None).unwrap();
        assert!(
            built
                .sql
                .starts_with("DELETE FROM \"SHOP\".\"LOOSE\" WHERE ROWID = CHARTOROWID(:1) AND"),
            "{}",
            built.sql
        );
        let reread = sqlbuild::build_reread(&OraDialect, &object, change.key(), &columns).unwrap();
        assert!(
            reread.sql.starts_with("SELECT ROWIDTOCHAR(ROWID) AS \"_rowid_\", \"qty\" FROM"),
            "{}",
            reread.sql
        );
    }
}
