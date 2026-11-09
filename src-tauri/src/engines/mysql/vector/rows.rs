//! Typed result sets: columns and cells of the vector tool's statements, WHERE and key helpers.

use super::*;

/// A result column with the vector dimensions from `hint` (a known VECTOR column, or none for a byte string that only
/// looked like one).
pub(super) fn typed_column(column: &Column, hint: &VectorHint<'_>) -> ColumnInfo {
    let mut info = result_column(column);
    let dims = hint.dims(column);
    if dims == info.hints.vector_dims {
        return info;
    }
    if dims.is_some() {
        info.kind = ValueKind::Other;
        info.type_name = "vector".to_owned();
        info.hints.binary = false;
    } else {
        info.kind = ValueKind::Bytes;
        info.type_name = sql_type_name(column.column_type(), column.character_set() == BINARY_CHARSET, column.flags()).to_owned();
        info.hints.binary = true;
    }
    info.hints.vector_dims = dims;
    info
}

/// A typed cell; a vector is shown as its summary.
pub(super) fn typed_cell(value: Value, column: &ColumnInfo) -> Typed {
    match value {
        Value::Bytes(bytes) if column.hints.vector_dims.is_some() => match decode_vector(&bytes) {
            Some(values) => Typed::Other {
                type_name: "vector".to_owned(),
                text: summarize(&values),
            },
            None => Typed::Bytes(bytes),
        },
        other => to_value(other, column),
    }
}

/// Every result set of a query, typed.
pub(super) async fn read_result_sets<P: Protocol>(
    mut result: QueryResult<'_, 'static, P>,
    hint: &VectorHint<'_>,
) -> AppResult<Vec<RowSet>> {
    let mut sets = Vec::new();
    while let Some(columns) = result.columns() {
        let columns: Vec<ColumnInfo> = columns.iter().map(|column| typed_column(column, hint)).collect();
        let mut rows = Vec::new();
        while let Some(row) = result.next().await? {
            rows.push(
                row.unwrap()
                    .into_iter()
                    .zip(&columns)
                    .map(|(value, column)| typed_cell(value, column))
                    .collect(),
            );
        }
        sets.push(RowSet { columns, rows });
    }
    // A failed later statement is held as a pending error that `columns()` does not report.
    result.next().await?;
    Ok(sets)
}

pub(super) async fn primary_key(conn: &mut Conn, database: &str, table: &str) -> AppResult<Vec<String>> {
    let rows = text_rows(
        conn,
        "SELECT COLUMN_NAME FROM information_schema.KEY_COLUMN_USAGE \
         WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ? AND CONSTRAINT_NAME = 'PRIMARY' ORDER BY ORDINAL_POSITION",
        (database.to_owned(), table.to_owned()),
    )
    .await?;
    Ok(rows.iter().map(|row| row.text(0)).collect())
}

pub(super) fn key_condition(key: &[KeyCell]) -> AppResult<String> {
    if key.is_empty() {
        return Err(AppError::new("Cannot identify the row"));
    }
    Ok(key
        .iter()
        .map(|cell| format!("{} <=> ?", quote_ident(&cell.column)))
        .collect::<Vec<_>>()
        .join(" AND "))
}

pub(super) fn bind_values(key: &[KeyCell]) -> Vec<Value> {
    key.iter().map(|cell| to_param(&cell.value)).collect()
}

pub(super) fn first_set(sets: Vec<RowSet>) -> RowSet {
    sets.into_iter().next().unwrap_or_default()
}

/// A key value as text, for comparing result keys.
pub(super) fn key_text(value: Value) -> Option<String> {
    match value {
        Value::Bytes(bytes) => Some(String::from_utf8(bytes).unwrap_or_else(|error| hex(error.as_bytes()))),
        other => plain_text(other),
    }
}
