//! MariaDB vector tooling (VECTOR columns, MariaDB 11.7+): the [`VectorStore`] the shared `vector.*` operations
//! (`crate::contract::vector`) run on. Result sets are typed like query results (contract `ColumnInfo` and `Value`), with
//! vector cells summarised; key cells are typed values as the grid holds them.

use std::{collections::HashMap, time::Instant};

use async_trait::async_trait;
use mysql_async::{prelude::*, Column, Conn, QueryResult, Row, Value};

use super::{
    catalog::{create_statement, qualified, quote_ident, text_rows},
    values::{decode_vector, guess_vector_dims, plain_text, result_column, sql_type_name, to_param, to_value, BINARY_CHARSET},
};
use crate::{
    contract::{
        vector::{
            self as shared, summarize, vector_text as vector_literal, KeyCell, Metric, NearestRequest, Neighbour, RowSet, SampleOrder,
            SearchRequest, TableRef, VectorColumn, VectorIndex, VectorProfile, VectorSearchResult, VectorStore, KEY_SEPARATOR,
        },
        ColumnInfo, Value as Typed, ValueKind,
    },
    error::{AppError, AppResult},
};

pub use shared::OPERATIONS;

mod catalog;
mod rows;
mod sampling;
mod search;

use catalog::*;
use rows::*;
use search::*;

static PROFILE: VectorProfile = VectorProfile {
    metrics: &[Metric::Cosine, Metric::Euclidean],
    schemas: false,
    index_usage: "Searches must ORDER BY the matching distance function and use LIMIT to use this index.",
    add_index: "Add one with: ALTER TABLE … ADD VECTOR INDEX (column) DISTANCE=cosine",
    euclidean_advice: "Normalize them or use DISTANCE=cosine.",
};

/// `0x…` with upper-case digits, as the vector view has always shown binary cells.
fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut text = String::with_capacity(2 + bytes.len() * 2);
    text.push_str("0x");
    for byte in bytes {
        let _ = write!(text, "{byte:02X}");
    }
    text
}

/// The vector columns of one MariaDB connection.
pub struct MariaVectors<'a> {
    conn: &'a mut Conn,
}

/// Runs one vector operation on `conn`.
pub async fn invoke(conn: &mut Conn, name: &str, args: serde_json::Value) -> AppResult<serde_json::Value> {
    shared::invoke(&mut MariaVectors { conn }, name, args).await
}

#[async_trait]
impl VectorStore for MariaVectors<'_> {
    fn profile(&self) -> &'static VectorProfile {
        &PROFILE
    }

    async fn supported(&mut self, _table: &TableRef) -> AppResult<bool> {
        Ok(self.conn.query_drop("SELECT VEC_FromText('[1]')").await.is_ok())
    }

    async fn columns(&mut self, table: &TableRef) -> AppResult<Vec<VectorColumn>> {
        Ok(table_columns(self.conn, &table.database, &table.table)
            .await?
            .into_iter()
            .filter_map(|column| {
                let dims = column.vector_dims?;
                Some(VectorColumn {
                    name: column.name,
                    dims,
                    type_name: format!("VECTOR({dims})"),
                })
            })
            .collect())
    }

    async fn indexes(&mut self, table: &TableRef) -> AppResult<Vec<VectorIndex>> {
        vector_indexes(self.conn, &table.database, &table.table).await
    }

    async fn primary_key(&mut self, table: &TableRef) -> AppResult<Vec<String>> {
        primary_key(self.conn, &table.database, &table.table).await
    }

    async fn fetch(&mut self, table: &TableRef, column: &str, key: &[KeyCell]) -> AppResult<Vec<f32>> {
        fetch_vector(self.conn, &table.database, &table.table, column, key).await
    }

    async fn count(&mut self, table: &TableRef, column: &VectorColumn) -> AppResult<(u64, u64)> {
        sampling::count(self.conn, table, &column.name).await
    }

    async fn sample(
        &mut self,
        table: &TableRef,
        column: &VectorColumn,
        limit: u32,
        order: SampleOrder<'_>,
    ) -> AppResult<Vec<Option<Vec<f32>>>> {
        sampling::sample(self.conn, table, &column.name, limit, order).await
    }

    async fn search(&mut self, table: &TableRef, request: &SearchRequest<'_>) -> AppResult<VectorSearchResult> {
        search(self.conn, table, request).await
    }

    async fn nearest(&mut self, table: &TableRef, request: &NearestRequest<'_>) -> AppResult<Vec<Neighbour>> {
        sampling::nearest(self.conn, table, request).await
    }
}

fn elapsed_millis(started: Instant) -> u64 {
    started.elapsed().as_millis() as u64
}

/// The distance function of a metric (`VEC_DISTANCE` follows the index's).
fn distance_function(metric: Metric) -> &'static str {
    match metric {
        Metric::Cosine => "VEC_DISTANCE_COSINE",
        Metric::Euclidean => "VEC_DISTANCE_EUCLIDEAN",
        // `dot` is not in the profile, so the shared checks refuse it before.
        Metric::Auto | Metric::Dot => "VEC_DISTANCE",
    }
}

pub struct TableColumn {
    pub name: String,
    pub vector_dims: Option<u32>,
}

/// Columns of a table whose vectors are known (`DATA_TYPE = 'vector'`), or a guess from result metadata.
pub enum VectorHint<'a> {
    Known(&'a HashMap<String, u32>),
    Guess,
}

impl VectorHint<'_> {
    fn dims(&self, column: &Column) -> Option<u32> {
        match self {
            Self::Known(columns) => columns.get(column.name_str().as_ref()).copied(),
            Self::Guess => guess_vector_dims(column),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn statements_and_cells_render_like_the_vector_view_expects() {
        let statement = SearchStatement {
            target: qualified("shop", "products"),
            column: quote_ident("embedding"),
            function: distance_function(Metric::Cosine),
            select_list: "`id`".into(),
            filter: shared::where_clause(Some(" stock > 0 ")),
            k: 3,
        };
        assert_eq!(
            statement.render("VEC_FromText(?)"),
            "SELECT VEC_DISTANCE_COSINE(`embedding`, VEC_FromText(?)) AS `vec_distance`, `id` FROM `shop`.`products` \
             WHERE stock > 0 ORDER BY VEC_DISTANCE_COSINE(`embedding`, VEC_FromText(?)) LIMIT 3"
        );
        assert_eq!(distance_function(Metric::Auto), "VEC_DISTANCE");
        assert_eq!(vector_literal(&[0.5, 1.0]), "[0.5,1]");
        assert_eq!(key_text(Value::Bytes(vec![0xab, 0xff])).as_deref(), Some("0xABFF"));
        assert_eq!(
            bind_values(&[KeyCell {
                column: "id".into(),
                value: Typed::Int(7)
            }]),
            [Value::Int(7)]
        );
        let line = "VECTOR KEY idx_embedding (embedding) M='6' DISTANCE='cosine',";
        assert_eq!(option_value(line, "DISTANCE="), Some("cosine"));
        assert_eq!(option_value(line, " M="), Some("6"));
        assert_eq!(option_value("KEY i (e) M=8 DISTANCE=euclidean", " M="), Some("8"));
        assert_eq!(key_condition(&[]).unwrap_err().to_string(), "Cannot identify the row");
        assert_eq!(
            sampling::nearest_sql("`t`", "`v`", "`id`", "VEC_DISTANCE_COSINE", "[1]", 5, Some("`idx`")),
            "SELECT VEC_DISTANCE_COSINE(`v`, VEC_FromText('[1]')), `id` FROM `t` IGNORE INDEX (`idx`) \
             ORDER BY VEC_DISTANCE_COSINE(`v`, VEC_FromText('[1]')), `id` LIMIT 5"
        );
        assert_eq!(
            sampling::nearest_sql("`t`", "`v`", "`id`", "VEC_DISTANCE", "[1]", 5, None),
            "SELECT VEC_DISTANCE(`v`, VEC_FromText('[1]')), `id` FROM `t` ORDER BY VEC_DISTANCE(`v`, VEC_FromText('[1]')) LIMIT 5"
        );
    }
}
