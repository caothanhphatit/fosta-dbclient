//! The tree: schemas (users that own objects), then one folder per object type with objects, then the objects. Oracle's
//! schemas are what the shared UI calls databases (the query tab's database picker, the ER diagram, completion), so a
//! schema node has the kind `database` and every object carries its owner as `ObjectRef::database` (`schema` stays
//! empty). Each level is one catalog query, run when it is expanded.
//!
//! Node ids are `schema:<owner>`, `schema:<owner>/folder:<OBJECT TYPE>` and `schema:<owner>/folder:<TYPE>/<kind>:<name>`,
//! so they stay stable and say which schema and folder a request is about.

use oracle::Connection;

use super::{map_err, strings};
use crate::{
    contract::{BuiltinAction, NodeAction, NodeKey, NodeKind, ObjectNode, ObjectRef, TreeChildren, TreeRequest},
    engines::common::{self, sqlbuild::escape_like},
    error::{AppError, AppResult},
};

/// The view role of PL/SQL source, sequences and synonyms (`src/engines/oracle/definitionView.ts`).
pub const DEFINITION_VIEW: &str = "definition";

/// Object folders in tree order: label, `ALL_OBJECTS.OBJECT_TYPE`, kind.
pub const FOLDERS: [(&str, &str, common::NodeKind); 8] = [
    ("Tables", "TABLE", common::NodeKind::Table),
    ("Views", "VIEW", common::NodeKind::View),
    ("Materialized views", "MATERIALIZED VIEW", common::NodeKind::MaterializedView),
    ("Sequences", "SEQUENCE", common::NodeKind::Sequence),
    ("Synonyms", "SYNONYM", common::NodeKind::Synonym),
    ("Packages", "PACKAGE", common::NodeKind::Package),
    ("Procedures", "PROCEDURE", common::NodeKind::Procedure),
    ("Functions", "FUNCTION", common::NodeKind::Function),
];

/// Objects the folders list: no recycle bin, no generated objects, no materialized views' container tables.
const VISIBLE: &str = "o.object_name NOT LIKE 'BIN$%' AND o.generated = 'N' \
     AND NOT (o.object_type = 'TABLE' AND EXISTS (SELECT 1 FROM all_mviews m WHERE m.owner = o.owner AND m.mview_name = o.object_name))";

fn encode(name: &str) -> String {
    name.replace('%', "%25").replace('/', "%2F")
}

fn decode(segment: &str) -> String {
    segment.replace("%2F", "/").replace("%25", "%")
}

/// A contract reference to an object of `owner` (the owner is the "database"; `schema` stays empty).
pub fn object_ref(owner: &str, name: &str, kind: NodeKind) -> ObjectRef {
    ObjectRef::new(Some(owner), None, name, kind)
}

/// The owner of a contract reference: `schema` when a caller sets it, else `database`, else the login schema.
pub fn owner_of(object: &ObjectRef, login: &str) -> String {
    object
        .schema
        .as_deref()
        .or(object.database.as_deref())
        .filter(|name| !name.is_empty())
        .unwrap_or(login)
        .to_owned()
}

/// A contract reference as the core's: the owner becomes the schema.
pub fn to_core(object: &ObjectRef, login: &str) -> AppResult<common::ObjectRef> {
    Ok(common::ObjectRef::new(
        None,
        Some(&owner_of(object, login)),
        &object.name,
        common::NodeKind::try_from(&object.kind)?,
    ))
}

/// A core reference as the contract's.
pub fn to_contract(object: &common::ObjectRef, login: &str) -> ObjectRef {
    object_ref(object.schema.as_deref().unwrap_or(login), &object.name, object.kind.into())
}

/// Where a node id points.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Level {
    Schema(String),
    Folder { owner: String, object_type: &'static str },
}

pub fn level_of(key: &NodeKey) -> AppResult<Level> {
    let gone = || AppError::new("This item no longer exists. Refresh the tree.");
    let mut parts = key.id.split('/');
    let owner = parts
        .next()
        .and_then(|part| part.strip_prefix("schema:"))
        .map(decode)
        .ok_or_else(gone)?;
    match parts.next() {
        None => Ok(Level::Schema(owner)),
        Some(folder) => {
            let wanted = folder.strip_prefix("folder:").ok_or_else(gone)?;
            let (_, object_type, _) = FOLDERS.iter().find(|(_, t, _)| *t == wanted).ok_or_else(gone)?;
            Ok(Level::Folder { owner, object_type })
        }
    }
}

fn like_filter(filter: Option<&str>) -> Option<String> {
    filter
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(|text| format!("%{}%", escape_like(text)))
}

/// Paging over a listing: the cursor is the offset of the next page.
fn page(cursor: Option<&str>, limit: Option<u32>) -> (u64, Option<u64>) {
    let offset = cursor.and_then(|text| text.parse().ok()).unwrap_or(0);
    (offset, limit.filter(|limit| *limit > 0).map(u64::from))
}

fn finish(mut nodes: Vec<ObjectNode>, offset: u64, limit: Option<u64>) -> TreeChildren {
    let next_cursor = match limit {
        Some(limit) if nodes.len() as u64 > limit => {
            nodes.truncate(limit as usize);
            Some((offset + limit).to_string())
        }
        _ => None,
    };
    TreeChildren {
        nodes,
        next_cursor,
        total_hint: None,
    }
}

fn paging_sql(offset: u64, limit: Option<u64>) -> String {
    match limit {
        Some(limit) => format!(" OFFSET {offset} ROWS FETCH NEXT {} ROWS ONLY", limit + 1),
        None if offset > 0 => format!(" OFFSET {offset} ROWS"),
        None => String::new(),
    }
}

/// Children of a node, one catalog query per level.
pub fn children(c: &Connection, request: &TreeRequest, login: &str) -> AppResult<TreeChildren> {
    let filter = like_filter(request.filter.as_deref());
    let (offset, limit) = page(request.cursor.as_deref(), request.limit);
    match &request.parent {
        None => Ok(finish(schemas(c, filter.as_deref(), offset, limit, login)?, offset, limit)),
        Some(key) => match level_of(key)? {
            Level::Schema(owner) => folders(c, &owner, key),
            Level::Folder { owner, object_type } => Ok(finish(
                objects(c, &owner, object_type, key, filter.as_deref(), offset, limit)?,
                offset,
                limit,
            )),
        },
    }
}

/// Schemas that are not Oracle-maintained and own at least one object, the login schema first (even when empty).
fn schemas(c: &Connection, filter: Option<&str>, offset: u64, limit: Option<u64>, login: &str) -> AppResult<Vec<ObjectNode>> {
    let mut sql = String::from(
        "SELECT u.username FROM all_users u WHERE (u.username = USER OR (u.oracle_maintained = 'N' \
         AND EXISTS (SELECT 1 FROM all_objects o WHERE o.owner = u.username)))",
    );
    if filter.is_some() {
        sql.push_str(" AND UPPER(u.username) LIKE UPPER(:1) ESCAPE '\\'");
    }
    sql.push_str(" ORDER BY CASE WHEN u.username = USER THEN 0 ELSE 1 END, u.username");
    sql.push_str(&paging_sql(offset, limit));
    let rows = match filter {
        Some(pattern) => strings(c, &sql, &[&pattern])?,
        None => strings(c, &sql, &[])?,
    };
    Ok(rows
        .into_iter()
        .filter_map(|row| row.into_iter().next().flatten())
        .map(|owner| schema_node(&owner, owner == login))
        .collect())
}

pub fn schema_node(owner: &str, login: bool) -> ObjectNode {
    let object = object_ref(owner, owner, NodeKind::DATABASE);
    ObjectNode {
        key: NodeKey {
            id: format!("schema:{}", encode(owner)),
            kind: NodeKind::DATABASE,
            object: Some(object.clone()),
        },
        label: owner.to_owned(),
        kind: NodeKind::DATABASE,
        icon: "engine:oracle/schema".into(),
        object: Some(object),
        detail: login.then(|| "login schema".to_owned()),
        badges: Vec::new(),
        expandable: true,
        default_view: None,
        actions: vec![
            NodeAction::builtin("newQuery", "New query", BuiltinAction::NewQuery),
            NodeAction::builtin("diagram", "ER diagram", BuiltinAction::Diagram),
            NodeAction::builtin("copyName", "Copy name", BuiltinAction::CopyName),
            NodeAction::builtin("refresh", "Refresh", BuiltinAction::Refresh).separated(),
        ],
    }
}

/// The folders of a schema that have objects, with their counts.
fn folders(c: &Connection, owner: &str, parent: &NodeKey) -> AppResult<TreeChildren> {
    let counts = strings(
        c,
        &format!(
            "SELECT o.object_type, TO_CHAR(COUNT(*)) FROM all_objects o WHERE o.owner = :1 AND o.object_type IN \
             ('TABLE', 'VIEW', 'MATERIALIZED VIEW', 'SEQUENCE', 'SYNONYM', 'PACKAGE', 'PROCEDURE', 'FUNCTION') AND {VISIBLE} \
             GROUP BY o.object_type"
        ),
        &[&owner],
    )?;
    let nodes = FOLDERS
        .iter()
        .filter_map(|(label, object_type, _)| {
            let count = counts.iter().find(|row| row[0].as_deref() == Some(*object_type))?[1].clone()?;
            Some(ObjectNode {
                key: NodeKey {
                    id: format!("{}/folder:{object_type}", parent.id),
                    kind: NodeKind::FOLDER,
                    object: None,
                },
                label: (*label).to_owned(),
                kind: NodeKind::FOLDER,
                icon: "layers".into(),
                object: None,
                detail: Some(count),
                badges: Vec::new(),
                expandable: true,
                default_view: None,
                actions: vec![NodeAction::builtin("refresh", "Refresh", BuiltinAction::Refresh)],
            })
        })
        .collect();
    Ok(TreeChildren {
        nodes,
        next_cursor: None,
        total_hint: None,
    })
}

/// The objects of one type in a schema: tables with their row estimates, synonyms with their targets, invalid PL/SQL and
/// views marked.
fn objects(
    c: &Connection,
    owner: &str,
    object_type: &'static str,
    parent: &NodeKey,
    filter: Option<&str>,
    offset: u64,
    limit: Option<u64>,
) -> AppResult<Vec<ObjectNode>> {
    let (_, _, core_kind) = FOLDERS.iter().find(|(_, t, _)| *t == object_type).copied().unwrap_or(FOLDERS[0]);
    let detail = match core_kind {
        common::NodeKind::Table => {
            "(SELECT TO_CHAR(t.num_rows) FROM all_tables t WHERE t.owner = o.owner AND t.table_name = o.object_name)"
        }
        common::NodeKind::Synonym => {
            "(SELECT s.table_owner || '.' || s.table_name || NVL2(s.db_link, '@' || s.db_link, '') FROM all_synonyms s \
              WHERE s.owner = o.owner AND s.synonym_name = o.object_name)"
        }
        _ => "NULL",
    };
    let mut sql =
        format!("SELECT o.object_name, o.status, {detail} FROM all_objects o WHERE o.owner = :1 AND o.object_type = :2 AND {VISIBLE}");
    if filter.is_some() {
        sql.push_str(" AND UPPER(o.object_name) LIKE UPPER(:3) ESCAPE '\\'");
    }
    sql.push_str(" ORDER BY o.object_name");
    sql.push_str(&paging_sql(offset, limit));
    let rows = match filter {
        Some(pattern) => strings(c, &sql, &[&owner, &object_type, &pattern])?,
        None => strings(c, &sql, &[&owner, &object_type])?,
    };
    let kind: NodeKind = core_kind.into();
    Ok(rows
        .into_iter()
        .filter_map(|row| {
            let name = row[0].clone()?;
            let object = object_ref(owner, &name, kind.clone());
            let detail = match core_kind {
                common::NodeKind::Table => row[2].as_ref().map(|rows| format!("~{rows} rows")),
                _ => row[2].clone(),
            };
            let badges = if row[1].as_deref() == Some("INVALID") {
                vec![crate::contract::object::Badge {
                    text: "invalid".into(),
                    tone: crate::contract::object::Tone::Danger,
                }]
            } else {
                Vec::new()
            };
            Some(ObjectNode {
                key: NodeKey {
                    id: format!("{}/{}:{}", parent.id, kind.as_str(), encode(&name)),
                    kind: kind.clone(),
                    object: Some(object.clone()),
                },
                label: name,
                kind: kind.clone(),
                icon: icon_of(core_kind).into(),
                object: Some(object),
                detail,
                badges,
                expandable: false,
                default_view: Some(if core_kind.is_relation() { "data" } else { DEFINITION_VIEW }.to_owned()),
                actions: actions(core_kind),
            })
        })
        .collect())
}

fn icon_of(kind: common::NodeKind) -> &'static str {
    match kind {
        common::NodeKind::Table => "table",
        common::NodeKind::View | common::NodeKind::MaterializedView => "view",
        common::NodeKind::Sequence => "engine:oracle/sequence",
        common::NodeKind::Synonym => "engine:oracle/synonym",
        common::NodeKind::Package => "engine:oracle/package",
        _ => "terminal",
    }
}

const EXPLAIN: &str = "EXPLAIN PLAN FOR SELECT * FROM {qualified};\nSELECT plan_table_output FROM TABLE(DBMS_XPLAN.DISPLAY());";

/// Object actions per kind; query templates go through the gateway like anything typed in a query tab.
fn actions(kind: common::NodeKind) -> Vec<NodeAction> {
    let copy = NodeAction::builtin("copyName", "Copy name", BuiltinAction::CopyName);
    let source = NodeAction::view("source", "Source", DEFINITION_VIEW);
    let compile = |what: &str| NodeAction::query("compile", "Compile", &format!("ALTER {what} {{qualified}} COMPILE")).separated();
    match kind {
        common::NodeKind::Table => vec![
            NodeAction::view("design", "Design", "design"),
            copy,
            NodeAction::query("count", "Count rows", "SELECT COUNT(*) FROM {qualified}").separated(),
            NodeAction::query("explain", "Explain a select", EXPLAIN),
            NodeAction::query("truncate", "Truncate…", "TRUNCATE TABLE {qualified}")
                .danger()
                .separated(),
        ],
        common::NodeKind::View | common::NodeKind::MaterializedView => vec![
            NodeAction::view("design", "Design", "design"),
            copy,
            NodeAction::query("explain", "Explain a select", EXPLAIN).separated(),
        ],
        common::NodeKind::Sequence => vec![
            NodeAction::view("definition", "Definition", DEFINITION_VIEW),
            copy,
            NodeAction::query("next", "Next value", "SELECT {qualified}.NEXTVAL FROM dual").separated(),
        ],
        common::NodeKind::Synonym => vec![NodeAction::view("definition", "Definition", DEFINITION_VIEW), copy],
        common::NodeKind::Package => vec![source, copy, compile("PACKAGE")],
        common::NodeKind::Procedure => vec![source, copy, compile("PROCEDURE")],
        common::NodeKind::Function => vec![source, copy, compile("FUNCTION")],
        _ => vec![copy],
    }
}

/// Whether `owner` is a user of this database (a query tab's database can be the service name instead).
pub fn schema_exists(c: &Connection, owner: &str) -> AppResult<bool> {
    let rows = c
        .query_row_as::<i64>("SELECT COUNT(*) FROM all_users WHERE username = :1", &[&owner])
        .map_err(|error| AppError::from(map_err(error)))?;
    Ok(rows > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_ids_name_the_schema_and_folder() {
        let schema = schema_node("SHOP", true);
        assert_eq!(schema.key.id, "schema:SHOP");
        assert_eq!(schema.kind, NodeKind::DATABASE);
        assert_eq!(schema.object.as_ref().and_then(|o| o.database.as_deref()), Some("SHOP"));
        assert_eq!(level_of(&schema.key).unwrap(), Level::Schema("SHOP".into()));
        let folder = NodeKey {
            id: "schema:A%2FB/folder:MATERIALIZED VIEW".into(),
            kind: NodeKind::FOLDER,
            object: None,
        };
        assert_eq!(
            level_of(&folder).unwrap(),
            Level::Folder {
                owner: "A/B".into(),
                object_type: "MATERIALIZED VIEW"
            }
        );
        let bogus = NodeKey {
            id: "schema:SHOP/folder:INDEX".into(),
            kind: NodeKind::FOLDER,
            object: None,
        };
        assert!(level_of(&bogus).is_err());
    }

    #[test]
    fn references_map_the_owner_to_the_database() {
        let open = object_ref("SHOP", "ORDERS", NodeKind::TABLE);
        let core = to_core(&open, "OTHER").unwrap();
        assert_eq!((core.schema.as_deref(), core.name.as_str()), (Some("SHOP"), "ORDERS"));
        assert_eq!(to_contract(&core, "OTHER"), open);
        let bare = ObjectRef::new(None, None, "T", NodeKind::TABLE);
        assert_eq!(owner_of(&bare, "ME"), "ME");
        assert_eq!(
            finish(vec![schema_node("A", false), schema_node("B", false)], 0, Some(1))
                .next_cursor
                .as_deref(),
            Some("1")
        );
        assert_eq!(paging_sql(10, Some(5)), " OFFSET 10 ROWS FETCH NEXT 6 ROWS ONLY");
        assert_eq!(like_filter(Some(" ord_ ")).as_deref(), Some("%ord\\_%"));
    }

    #[test]
    fn plsql_objects_open_their_source_and_tables_their_rows() {
        let compile = actions(common::NodeKind::Package);
        assert!(compile.iter().any(|a| a.id == "compile"));
        assert!(actions(common::NodeKind::Table).iter().any(|a| a.id == "truncate" && a.danger));
    }
}
