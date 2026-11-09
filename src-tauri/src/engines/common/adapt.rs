//! Conversions between the SQL cores' shapes and the contract, used by each core's `adapter.rs`.
//!
//! The cores keep their closed [`NodeKind`] enum and core-level [`ObjectRef`]; the contract has open string kinds and
//! object references with extra qualifiers. Catalog trees built by a core's `catalog()` are served lazily as tree
//! children by [`catalog_children`], keyed by a stable path id.

use super::{
    classify::ClassifiedStatement, BrowseRequest, CatalogNode, Classification, NodeKind, ObjectDescription, ObjectRef, Page, RowChange,
    StatementKind, TlsMode,
};
use crate::{
    contract::{
        self,
        classify::{Category, StatementInfo},
        object::{NodeAction, NodeKey, ObjectNode, TreeChildren},
        tabular::Filter,
    },
    error::{AppError, AppResult},
};

impl From<NodeKind> for contract::NodeKind {
    fn from(kind: NodeKind) -> Self {
        match kind {
            NodeKind::Database => Self::DATABASE,
            NodeKind::Schema => Self::SCHEMA,
            NodeKind::Folder => Self::FOLDER,
            NodeKind::Table => Self::TABLE,
            NodeKind::View => Self::VIEW,
            NodeKind::MaterializedView => Self::MATERIALIZED_VIEW,
            NodeKind::ForeignTable => Self::FOREIGN_TABLE,
            NodeKind::Dictionary => Self::DICTIONARY,
            NodeKind::Function => Self::FUNCTION,
            NodeKind::Procedure => Self::PROCEDURE,
            NodeKind::Package => Self::PACKAGE,
            NodeKind::Sequence => Self::SEQUENCE,
            NodeKind::Type => Self::TYPE,
            NodeKind::Extension => Self::EXTENSION,
            NodeKind::Index => Self::INDEX,
            NodeKind::Trigger => Self::TRIGGER,
            NodeKind::Synonym => Self::SYNONYM,
        }
    }
}

impl TryFrom<&contract::NodeKind> for NodeKind {
    type Error = AppError;

    fn try_from(kind: &contract::NodeKind) -> AppResult<Self> {
        Ok(match kind.as_str() {
            "database" => NodeKind::Database,
            "schema" => NodeKind::Schema,
            "folder" => NodeKind::Folder,
            "table" => NodeKind::Table,
            "view" => NodeKind::View,
            "materializedView" => NodeKind::MaterializedView,
            "foreignTable" => NodeKind::ForeignTable,
            "dictionary" => NodeKind::Dictionary,
            "function" => NodeKind::Function,
            "procedure" => NodeKind::Procedure,
            "package" => NodeKind::Package,
            "sequence" => NodeKind::Sequence,
            "type" => NodeKind::Type,
            "extension" => NodeKind::Extension,
            "index" => NodeKind::Index,
            "trigger" => NodeKind::Trigger,
            "synonym" => NodeKind::Synonym,
            other => return Err(AppError::new(format!("This engine has no objects of kind '{other}'."))),
        })
    }
}

/// A core reference as a contract reference; `database` fills in the connection's database when the core leaves it out.
pub fn object_to_contract(object: &ObjectRef, database: Option<&str>) -> contract::ObjectRef {
    contract::ObjectRef {
        database: object.database.clone().or_else(|| database.map(str::to_owned)),
        schema: object.schema.clone(),
        name: object.name.clone(),
        kind: object.kind.into(),
        path: Vec::new(),
        binary: false,
    }
}

pub fn object_to_common(object: &contract::ObjectRef) -> AppResult<ObjectRef> {
    Ok(ObjectRef {
        database: object.database.clone(),
        schema: object.schema.clone(),
        name: object.name.clone(),
        kind: NodeKind::try_from(&object.kind)?,
    })
}

pub fn description_to_contract(description: ObjectDescription, database: Option<&str>) -> contract::ObjectDescription {
    let primary: Vec<String> = description.primary_key.as_ref().map(|key| key.columns.clone()).unwrap_or_default();
    let mut columns = description.columns;
    for column in &mut columns {
        column.hints.primary |= primary.contains(&column.name);
        column.hints.binary |= matches!(column.kind, contract::ValueKind::Bytes);
        column.hints.read_only |= !column.is_writable();
    }
    contract::ObjectDescription {
        object: object_to_contract(&description.object, database),
        comment: description.comment,
        columns,
        primary_key: description.primary_key,
        unique_keys: description.unique_keys,
        foreign_keys: description.foreign_keys,
        referenced_by: Vec::new(),
        indexes: description.indexes,
        row_estimate: description.row_estimate,
        ddl: description.ddl,
        extra: description.extra,
    }
}

/// A contract browse request for a core. Raw filters are handled by the caller (cores take structured conditions only).
pub fn browse_to_common(request: &contract::BrowseRequest) -> AppResult<BrowseRequest> {
    let (filter, match_any) = match &request.filter {
        Filter::None => (Vec::new(), false),
        Filter::Conditions { conditions, match_any } => (conditions.clone(), *match_any),
        Filter::Raw { .. } => return Err(contract::unsupported_capability("raw filters")),
    };
    Ok(BrowseRequest {
        object: object_to_common(&request.object)?,
        filter,
        match_any,
        sort: request.sort.clone(),
        offset: request.offset,
        limit: request.limit,
        after: request.after.clone(),
        estimate_total: request.estimate_total,
        reverse: request.reverse,
    })
}

pub fn page_to_contract(page: Page, elapsed_ms: u64) -> contract::Page {
    contract::Page {
        columns: page.columns,
        rows: page.rows,
        has_more: page.has_more,
        key_columns: page.key_columns,
        next_keyset: page.next_keyset,
        total: page.estimated_total.map(|value| contract::Total {
            value: value.max(0) as u64,
            exact: false,
        }),
        statement: Some(page.sql),
        elapsed_ms,
        row_versions: None,
    }
}

fn pairs(fields: &[contract::Field]) -> Vec<(String, contract::Value)> {
    fields.iter().map(|field| (field.column.clone(), field.value.clone())).collect()
}

/// A contract change as a core change. The core statement does not carry the concurrency check; adapters that use
/// this must add it (see `RowChange::check`).
pub fn change_to_common(change: &contract::RowChange) -> RowChange {
    match change {
        contract::RowChange::Insert { values } => RowChange::Insert { values: pairs(values) },
        contract::RowChange::Update { key, values, .. } => RowChange::Update {
            key: pairs(key),
            values: pairs(values),
        },
        contract::RowChange::Delete { key, .. } => RowChange::Delete { key: pairs(key) },
    }
}

impl From<StatementKind> for Category {
    /// Session settings (SET, USE) only change the session, so the gateway treats them as reads.
    fn from(kind: StatementKind) -> Self {
        match kind {
            StatementKind::Read | StatementKind::Session => Category::Read,
            StatementKind::Transaction => Category::Transaction,
            StatementKind::Write => Category::Write,
            StatementKind::Unknown => Category::Unknown,
            StatementKind::Ddl => Category::Ddl,
            StatementKind::Admin => Category::Admin,
            StatementKind::Destructive => Category::Destructive,
        }
    }
}

impl From<ClassifiedStatement> for StatementInfo {
    fn from(statement: ClassifiedStatement) -> Self {
        let kind = if statement.keyword.is_empty() {
            "statement"
        } else {
            statement.keyword.as_str()
        };
        let mut info = StatementInfo::new(statement.kind.into(), kind, &statement.text);
        info.reason = statement.reason;
        info
    }
}

/// A core classification (one entry per statement of the script) as the gateway's classification.
impl From<Classification> for contract::Classification {
    fn from(classification: Classification) -> Self {
        Self {
            statements: classification.statements.into_iter().map(StatementInfo::from).collect(),
        }
    }
}

/// A core TLS mode as the contract's TLS settings; `None` for a plain connection (the form's default).
/// `SkipVerify` is libpq's `require`: encrypted, certificate not checked.
pub fn tls_to_contract(mode: &TlsMode) -> Option<contract::TlsConfig> {
    match mode {
        TlsMode::Disable => None,
        TlsMode::Verify { ca_file } => Some(contract::TlsConfig {
            mode: contract::TlsMode::VerifyFull,
            ca_file: ca_file.clone(),
            ..Default::default()
        }),
        TlsMode::SkipVerify => Some(contract::TlsConfig {
            mode: contract::TlsMode::Require,
            accept_invalid_certs: true,
            ..Default::default()
        }),
    }
}

/// Icon names (`src/core/dom.ts`) per kind.
pub fn icon_for(kind: NodeKind) -> &'static str {
    match kind {
        NodeKind::Database | NodeKind::Schema => "database",
        NodeKind::Folder => "layers",
        NodeKind::Table | NodeKind::ForeignTable | NodeKind::Dictionary => "table",
        NodeKind::View | NodeKind::MaterializedView => "view",
        NodeKind::Function | NodeKind::Procedure | NodeKind::Package => "terminal",
        NodeKind::Index => "key",
        _ => "layers",
    }
}

fn segment(node: &CatalogNode) -> String {
    let kind: contract::NodeKind = node.kind.into();
    format!("{}:{}", kind.as_str(), node.name.replace('/', "%2F"))
}

/// Children of `parent` in a catalog tree built by a core. Node ids are the `/`-joined `kind:name` path from the root,
/// so they stay stable across calls. `actions` adds node actions per node.
pub fn catalog_children(
    roots: &[CatalogNode],
    parent: Option<&NodeKey>,
    database: Option<&str>,
    actions: &dyn Fn(&CatalogNode) -> Vec<NodeAction>,
) -> AppResult<TreeChildren> {
    let (prefix, children) = match parent {
        None => (String::new(), roots),
        Some(key) => {
            let mut level = roots;
            let mut found: Option<&CatalogNode> = None;
            for part in key.id.split('/').filter(|part| !part.is_empty()) {
                found = level.iter().find(|node| segment(node) == part);
                match found {
                    Some(node) => level = &node.children,
                    None => return Err(AppError::new("This item no longer exists. Refresh the tree.")),
                }
            }
            (format!("{}/", key.id), found.map(|node| node.children.as_slice()).unwrap_or(roots))
        }
    };
    let nodes = children
        .iter()
        .map(|node| {
            let kind: contract::NodeKind = node.kind.into();
            let object = node.object.as_ref().map(|object| object_to_contract(object, database));
            let id = format!("{prefix}{}", segment(node));
            ObjectNode {
                key: NodeKey {
                    id,
                    kind: kind.clone(),
                    object: object.clone(),
                },
                label: node.name.clone(),
                icon: icon_for(node.kind).to_owned(),
                object,
                detail: node.detail.clone(),
                badges: Vec::new(),
                expandable: !node.children.is_empty(),
                default_view: node.kind.is_relation().then(|| "data".to_owned()),
                actions: actions(node),
                kind,
            }
        })
        .collect();
    Ok(TreeChildren {
        nodes,
        next_cursor: None,
        total_hint: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_and_objects_convert_both_ways() {
        for kind in [NodeKind::Table, NodeKind::MaterializedView, NodeKind::Synonym] {
            let open: contract::NodeKind = kind.into();
            assert_eq!(NodeKind::try_from(&open).unwrap(), kind);
        }
        assert!(NodeKind::try_from(&contract::NodeKind::COLLECTION).is_err());
        let object = ObjectRef::table(Some("public"), "orders");
        let open = object_to_contract(&object, Some("shop"));
        assert_eq!(open.database.as_deref(), Some("shop"));
        assert_eq!(object_to_common(&open).unwrap().name, "orders");
    }

    #[test]
    fn core_classifications_map_onto_gateway_categories() {
        use crate::engines::common::{classify, split::Dialect};
        let classification: contract::Classification = classify(
            Dialect::Postgres,
            "SELECT 1; SET search_path = x; BEGIN; DELETE FROM t; DROP TABLE t",
        )
        .into();
        let categories: Vec<Category> = classification.statements.iter().map(|s| s.category).collect();
        assert_eq!(
            categories,
            vec![
                Category::Read,
                Category::Read,
                Category::Transaction,
                Category::Destructive,
                Category::Destructive
            ]
        );
        assert!(classification.statements[3].reason.is_some());
        assert_eq!(classification.statements[4].kind, "DROP TABLE");
        assert!(tls_to_contract(&TlsMode::Disable).is_none());
        let skip = tls_to_contract(&TlsMode::SkipVerify).unwrap();
        assert!(skip.accept_invalid_certs && skip.mode == contract::TlsMode::Require);
    }

    #[test]
    fn catalog_children_are_addressed_by_stable_paths() {
        let table = CatalogNode::object(ObjectRef::table(Some("public"), "orders"), Some("~12 rows".into()));
        let roots = vec![CatalogNode::container(
            NodeKind::Schema,
            "public",
            vec![CatalogNode::container(NodeKind::Folder, "Tables", vec![table])],
        )];
        let none = |_: &CatalogNode| Vec::new();
        let top = catalog_children(&roots, None, Some("shop"), &none).unwrap();
        assert_eq!(top.nodes[0].key.id, "schema:public");
        let folder = catalog_children(&roots, Some(&top.nodes[0].key), Some("shop"), &none).unwrap();
        let tables = catalog_children(&roots, Some(&folder.nodes[0].key), Some("shop"), &none).unwrap();
        assert_eq!(tables.nodes[0].key.id, "schema:public/folder:Tables/table:orders");
        assert_eq!(tables.nodes[0].default_view.as_deref(), Some("data"));
        assert!(!tables.nodes[0].expandable);
    }
}
