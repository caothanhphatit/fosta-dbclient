//! Nearest-neighbour search statements and their plan.

use super::*;

pub(super) async fn search(conn: &mut Conn, table: &TableRef, request: &SearchRequest<'_>) -> AppResult<VectorSearchResult> {
    let columns = table_columns(conn, &table.database, &table.table).await?;
    let vectors: HashMap<String, u32> = columns
        .iter()
        .filter_map(|column| Some((column.name.clone(), column.vector_dims?)))
        .collect();
    let select_list = columns
        .iter()
        .filter(|column| column.vector_dims.is_none())
        .map(|column| quote_ident(&column.name))
        .collect::<Vec<_>>();
    let statement = SearchStatement {
        target: qualified(&table.database, &table.table),
        column: quote_ident(&request.column.name),
        function: distance_function(request.metric),
        select_list: select_list.join(", "),
        filter: shared::where_clause(request.filter),
        k: request.k,
    };
    let runnable_sql = statement.render(&format!("VEC_FromText('{}')", vector_literal(request.query)));
    let plan = first_set(read_result_sets(conn.query_iter(format!("EXPLAIN {runnable_sql}")).await?, &VectorHint::Guess).await?);
    let started = Instant::now();
    let result = first_set(read_result_sets(conn.query_iter(runnable_sql.clone()).await?, &VectorHint::Known(&vectors)).await?);
    let elapsed_ms = elapsed_millis(started);
    let primary_key = primary_key(conn, &table.database, &table.table).await?;
    Ok(VectorSearchResult {
        index_used: index_in_plan(&plan),
        plan,
        result,
        sql: statement.render("VEC_FromText(?)"),
        runnable_sql,
        elapsed_ms,
        query_dims: request.query.len(),
        primary_key,
    })
}

pub(super) fn index_in_plan(plan: &RowSet) -> Option<String> {
    let key_column = plan.columns.iter().position(|column| column.name.eq_ignore_ascii_case("key"))?;
    plan.rows.iter().find_map(|row| row.get(key_column).and_then(Typed::display))
}

pub(super) struct SearchStatement {
    pub(super) target: String,
    pub(super) column: String,
    pub(super) function: &'static str,
    pub(super) select_list: String,
    pub(super) filter: String,
    pub(super) k: u32,
}

impl SearchStatement {
    pub(super) fn render(&self, query_vector: &str) -> String {
        let distance = format!("{}({}, {query_vector})", self.function, self.column);
        let separator = if self.select_list.is_empty() { "" } else { ", " };
        format!(
            "SELECT {distance} AS `vec_distance`{separator}{} FROM {}{} ORDER BY {distance} LIMIT {}",
            self.select_list, self.target, self.filter, self.k
        )
    }
}
