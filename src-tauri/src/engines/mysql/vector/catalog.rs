//! Which columns and indexes of a table are vectors, and fetching one row's vector.

use super::*;

pub async fn table_columns(conn: &mut Conn, database: &str, table: &str) -> AppResult<Vec<TableColumn>> {
    let rows = text_rows(
        conn,
        "SELECT COLUMN_NAME, DATA_TYPE, COLUMN_TYPE FROM information_schema.COLUMNS \
         WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ? ORDER BY ORDINAL_POSITION",
        (database.to_owned(), table.to_owned()),
    )
    .await?;
    Ok(rows
        .iter()
        .map(|row| TableColumn {
            name: row.text(0),
            vector_dims: row
                .text(1)
                .eq_ignore_ascii_case("vector")
                .then(|| parse_dims(&row.text(2)))
                .flatten(),
        })
        .collect())
}

pub(super) async fn vector_indexes(conn: &mut Conn, database: &str, table: &str) -> AppResult<Vec<VectorIndex>> {
    let rows = text_rows(
        conn,
        "SELECT INDEX_NAME, COLUMN_NAME FROM information_schema.STATISTICS \
         WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ? AND INDEX_TYPE = 'VECTOR'",
        (database.to_owned(), table.to_owned()),
    )
    .await?;
    if rows.is_empty() {
        return Ok(Vec::new());
    }
    let ddl = create_statement(conn, database, table).await?.replace('`', "");
    Ok(rows
        .iter()
        .map(|row| {
            let name = row.text(0);
            let line = ddl.lines().find(|line| line.contains(&format!("KEY {name} ("))).unwrap_or_default();
            let m: Option<u32> = option_value(line, " M=").and_then(|value| value.parse().ok());
            VectorIndex {
                metric: option_value(line, "DISTANCE=").unwrap_or("euclidean").to_lowercase(),
                kind: "mhnsw".to_owned(),
                params: format!("M={}", m.map_or_else(|| "default".to_owned(), |m| m.to_string())),
                m,
                column: row.text(1),
                name,
            }
        })
        .collect())
}

pub async fn fetch_vector(conn: &mut Conn, database: &str, table: &str, column: &str, key: &[KeyCell]) -> AppResult<Vec<f32>> {
    let sql = format!(
        "SELECT {} FROM {} WHERE {} LIMIT 1",
        quote_ident(column),
        qualified(database, table),
        key_condition(key)?
    );
    let row: Option<Row> = conn.exec_first(sql, bind_values(key)).await?;
    match row.and_then(|row| row.unwrap().into_iter().next()) {
        Some(Value::Bytes(bytes)) => decode_vector(&bytes).ok_or_else(|| AppError::new("The value is not a valid vector")),
        Some(Value::NULL) => Err(AppError::new("This row has no vector (NULL)")),
        Some(_) => Err(AppError::new("The value is not a vector")),
        None => Err(AppError::new("Row not found")),
    }
}

pub(super) fn parse_dims(column_type: &str) -> Option<u32> {
    column_type.split_once('(')?.1.trim_end_matches(')').trim().parse().ok()
}

/// `DISTANCE=cosine` or, as MariaDB 11.8 writes it, `DISTANCE='cosine'` (backticks already removed).
pub(super) fn option_value<'a>(line: &'a str, name: &str) -> Option<&'a str> {
    let start = line.find(name)? + name.len();
    line[start..]
        .trim_start_matches(['\'', '"'])
        .split(|character: char| !character.is_ascii_alphanumeric())
        .next()
        .filter(|value| !value.is_empty())
}
