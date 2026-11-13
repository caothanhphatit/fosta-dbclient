//! Profile builds against in-memory engines: the contract fake (`shop.items`) and a catalog with schemas, keys,
//! sensitive columns, system objects and routines that a test can change between builds.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use super::{
    build::{build, Step},
    fingerprint,
    model::{DescriptionSource, ProfileNote, RelationSource, ValueHint},
    store::{merge, ProfileStore, CORRUPT_SUFFIX, MAX_PROFILE_BYTES},
    *,
};
use crate::{
    ai::AiRuns,
    contract::{
        testing::FakeEngine, unsupported_capability, CallContext, Capability, CapabilitySet, ColumnInfo, Connection, ForeignKeyInfo,
        KeyInfo, NodeKey, NodeKind, ObjectDescription, ObjectNode, ObjectRef, OpenedSession, ServerInfo, SessionOptions, TreeChildren,
        TreeRequest, ValueKind, CANCELLED,
    },
    error::AppResult,
    runtime::{ConnectionManager, Handle},
};

/// One table of the catalog: schema, name, columns `(name, type, kind)`, primary key, foreign keys `(column, table)`.
#[derive(Clone)]
struct Table {
    schema: &'static str,
    name: &'static str,
    kind: NodeKind,
    columns: Vec<(&'static str, &'static str, ValueKind)>,
    primary: Option<&'static str>,
    foreign: Vec<(&'static str, &'static str)>,
    comment: Option<&'static str>,
}

fn table(schema: &'static str, name: &'static str, columns: &[(&'static str, &'static str)]) -> Table {
    Table {
        schema,
        name,
        kind: NodeKind::TABLE,
        columns: columns
            .iter()
            .map(|(name, type_name)| {
                let kind = if type_name.starts_with("int") {
                    ValueKind::Int
                } else {
                    ValueKind::Text
                };
                (*name, *type_name, kind)
            })
            .collect(),
        primary: columns.iter().any(|(name, _)| *name == "id").then_some("id"),
        foreign: Vec::new(),
        comment: None,
    }
}

fn shop_tables() -> Vec<Table> {
    let mut customers = table(
        "public",
        "customers",
        &[("id", "int"), ("name", "text"), ("password_hash", "text"), ("api_token", "text")],
    );
    customers.comment = Some("People who buy things; key sk-abcdefghijklmnopqrstuvwx1234");
    let mut orders = table("public", "orders", &[("id", "int"), ("customer_id", "int"), ("status", "text")]);
    orders.foreign = vec![("customer_id", "customers")];
    vec![
        customers,
        orders,
        table(
            "public",
            "order_items",
            &[("order_id", "int"), ("product_id", "int"), ("qty", "int")],
        ),
        table("public", "products", &[("id", "int"), ("name", "text")]),
        table("public", "pg_authid", &[("rolname", "text"), ("rolpassword", "text")]),
        table("information_schema", "columns", &[("column_name", "text")]),
    ]
}

struct Catalog {
    tables: Mutex<Vec<Table>>,
    described: Mutex<Vec<String>>,
    info: ServerInfo,
}

impl Catalog {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            tables: Mutex::new(shop_tables()),
            described: Mutex::new(Vec::new()),
            info: ServerInfo::default(),
        })
    }
}

fn node(id: String, label: &str, kind: NodeKind, object: Option<ObjectRef>, detail: Option<&str>) -> ObjectNode {
    ObjectNode {
        key: NodeKey {
            id,
            kind: kind.clone(),
            object: object.clone(),
        },
        label: label.into(),
        expandable: !(kind.is_relation() || kind == NodeKind::FUNCTION),
        kind,
        icon: "table".into(),
        object,
        detail: detail.map(str::to_owned),
        badges: Vec::new(),
        default_view: None,
        actions: Vec::new(),
    }
}

/// The catalog's connection (shared, so a test can change the tables between builds).
struct CatalogConnection(Arc<Catalog>);

#[async_trait]
impl Connection for CatalogConnection {
    fn engine_id(&self) -> &str {
        "fake"
    }

    fn info(&self) -> &ServerInfo {
        &self.0.info
    }

    fn capabilities(&self) -> CapabilitySet {
        CapabilitySet::of(&[Capability::Tree, Capability::Describe])
    }

    async fn children(&self, request: &TreeRequest, _ctx: &CallContext) -> AppResult<TreeChildren> {
        let parent = request.parent.as_ref().map(|key| key.id.as_str());
        let nodes = match parent {
            None => vec![
                node("db:shop".into(), "shop", NodeKind::DATABASE, None, None),
                node("db:mysql".into(), "mysql", NodeKind::DATABASE, None, Some("system")),
            ],
            Some("db:shop") => vec![
                node("db:shop/public".into(), "public", NodeKind::SCHEMA, None, None),
                node(
                    "db:shop/information_schema".into(),
                    "information_schema",
                    NodeKind::SCHEMA,
                    None,
                    None,
                ),
            ],
            Some(schema) => {
                let schema = schema.trim_start_matches("db:shop/");
                let mut nodes: Vec<ObjectNode> = self
                    .0
                    .tables
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|table| table.schema == schema)
                    .map(|table| {
                        let object = ObjectRef::new(Some("shop"), Some(table.schema), table.name, table.kind.clone());
                        node(
                            format!("t:{schema}.{}", table.name),
                            table.name,
                            table.kind.clone(),
                            Some(object),
                            None,
                        )
                    })
                    .collect();
                if schema == "public" {
                    let function = ObjectRef::new(Some("shop"), Some("public"), "order_total", NodeKind::FUNCTION);
                    nodes.push(node(
                        "f:order_total".into(),
                        "order_total",
                        NodeKind::FUNCTION,
                        Some(function),
                        Some("(order_id int) → numeric"),
                    ));
                }
                nodes
            }
        };
        Ok(TreeChildren {
            nodes,
            next_cursor: None,
            total_hint: None,
        })
    }

    async fn describe(&self, object: &ObjectRef, _ctx: &CallContext) -> AppResult<ObjectDescription> {
        self.0.described.lock().unwrap().push(object.name.clone());
        let table = self
            .0
            .tables
            .lock()
            .unwrap()
            .iter()
            .find(|table| table.name == object.name)
            .cloned()
            .expect("a listed table");
        let mut description = ObjectDescription::new(object.clone());
        description.comment = table.comment.map(str::to_owned);
        description.columns = table
            .columns
            .iter()
            .map(|(name, type_name, kind)| {
                let mut column = ColumnInfo::result(name, type_name, kind.clone());
                column.nullable = Some(*name) != table.primary;
                if name.contains("password") {
                    column.comment = Some("bcrypt of the password".into());
                    column.default = Some("'$2b$10$abcdefghijklmnopqrstuv'".into());
                }
                column
            })
            .collect();
        description.primary_key = table.primary.map(|key| KeyInfo {
            name: None,
            columns: vec![key.to_owned()],
        });
        description.foreign_keys = table
            .foreign
            .iter()
            .map(|(column, target)| ForeignKeyInfo {
                name: Some(format!("fk_{}_{column}", table.name)),
                columns: vec![(*column).to_owned()],
                ref_database: None,
                ref_schema: Some(table.schema.to_owned()),
                ref_table: (*target).to_owned(),
                ref_columns: vec!["id".into()],
                on_update: None,
                on_delete: None,
            })
            .collect();
        description.row_estimate = Some(42);
        description.ddl = Some("CREATE TABLE … DEFAULT 'sk-abcdefghijklmnopqrstuvwx1234'".into());
        Ok(description)
    }

    async fn open_session(&self, _options: &SessionOptions, _ctx: &CallContext) -> AppResult<OpenedSession> {
        Err(unsupported_capability("sessions"))
    }

    async fn close(&self) {}
}

fn catalog_handle(catalog: &Arc<Catalog>) -> (ConnectionManager, Handle) {
    let manager = ConnectionManager::default();
    manager.insert("c", FakeEngine::leak(), Arc::new(CatalogConnection(Arc::clone(catalog))));
    let handle = manager.handle("c").unwrap();
    (manager, handle)
}

fn quiet(_: Progress) {}

fn temporary_store(name: &str) -> (Arc<ProfileStore>, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("fosta-profiles-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    (Arc::new(ProfileStore::new(dir.clone())), dir)
}

#[tokio::test]
async fn the_fake_engine_gets_a_profile_of_its_table() {
    let manager = ConnectionManager::default();
    let engine = FakeEngine::leak();
    manager.insert("f", engine, engine.connection());
    let handle = manager.handle("f").unwrap();
    let profile = build(&handle, "f", "shop", &CancellationToken::new(), &quiet).await.unwrap();
    assert_eq!((profile.engine.as_str(), profile.database.as_str()), ("fake", "shop"));
    assert_eq!(profile.objects.len(), 1);
    let items = profile.object("items").unwrap();
    assert!(items.detailed);
    assert_eq!(items.columns.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), ["id", "name"]);
    assert!(!profile.data_read);
    // Metadata only: no rows were browsed and no session was opened.
    let calls = engine.calls.lock().unwrap().clone();
    assert!(calls.iter().all(|call| call == "children" || call == "describe"), "{calls:?}");
    let missing = build(&handle, "f", "nope", &CancellationToken::new(), &quiet).await.unwrap_err();
    assert!(missing.to_string().contains("no database nope"), "{missing}");
}

#[tokio::test]
async fn a_build_learns_columns_keys_relations_and_routines() {
    let catalog = Catalog::new();
    let (_manager, handle) = catalog_handle(&catalog);
    let steps = Mutex::new(Vec::new());
    let record = |progress: Progress| steps.lock().unwrap().push(progress);
    let profile = build(&handle, "c", "shop", &CancellationToken::new(), &record).await.unwrap();
    let names: Vec<String> = profile.objects.iter().map(|object| object.qualified()).collect();
    assert_eq!(
        names,
        ["public.customers", "public.orders", "public.order_items", "public.products"]
    );
    assert_eq!(profile.detailed_objects, 4);
    assert_eq!(profile.total_objects, 4);
    assert!(!profile.truncated);
    let orders = profile.object("orders").unwrap();
    assert_eq!(orders.primary_key, ["id"]);
    assert!(orders.column("id").unwrap().primary);
    assert_eq!(orders.row_estimate, Some(42));
    // The declared key, and the keys inferred from names.
    let edges: Vec<(String, String, RelationSource)> = profile
        .relations
        .iter()
        .map(|relation| {
            (
                format!("{}.{}", relation.from, relation.columns.join(",")),
                relation.to.clone(),
                relation.source,
            )
        })
        .collect();
    assert!(edges.contains(&("public.orders.customer_id".into(), "public.customers".into(), RelationSource::Fk)));
    assert!(edges.contains(&(
        "public.order_items.order_id".into(),
        "public.orders".into(),
        RelationSource::InferredName
    )));
    assert!(edges.contains(&(
        "public.order_items.product_id".into(),
        "public.products".into(),
        RelationSource::InferredName
    )));
    assert_eq!(
        edges.iter().filter(|(from, ..)| from == "public.orders.customer_id").count(),
        1,
        "a declared key is not inferred again: {edges:?}"
    );
    let inferred = profile
        .relations
        .iter()
        .find(|relation| relation.source == RelationSource::InferredName)
        .unwrap();
    assert!(inferred.confidence < 1.0 && inferred.to_columns == ["id"]);
    assert_eq!(profile.routines.len(), 1);
    assert_eq!(profile.routines[0].signature.as_deref(), Some("(order_id int) → numeric"));
    let steps = steps.lock().unwrap();
    assert_eq!(steps.first().unwrap().step, Step::Listing);
    assert!(steps
        .iter()
        .any(|progress| progress.step == Step::Describing && progress.object.as_deref() == Some("public.orders")));
    assert_eq!(steps.last().unwrap().step, Step::Relations);
}

#[tokio::test]
async fn system_and_credential_objects_are_left_out_and_sensitive_columns_redacted() {
    let catalog = Catalog::new();
    let (_manager, handle) = catalog_handle(&catalog);
    let profile = build(&handle, "c", "shop", &CancellationToken::new(), &quiet).await.unwrap();
    assert!(profile.object("pg_authid").is_none());
    assert!(profile.object("columns").is_none(), "information_schema is not walked");
    assert_eq!(profile.excluded_objects, 2);
    let described = catalog.described.lock().unwrap().clone();
    assert!(
        !described.iter().any(|name| name == "pg_authid" || name == "columns"),
        "{described:?}"
    );
    let customers = profile.object("customers").unwrap();
    let password = customers.column("password_hash").unwrap();
    assert!(password.sensitive && password.comment.is_none() && password.default.is_none());
    assert!(customers.column("api_token").unwrap().sensitive);
    assert!(!customers.column("name").unwrap().sensitive);
    assert_eq!(customers.comment.as_deref(), Some("People who buy things; key [redacted]"));
    let json = serde_json::to_string(&profile).unwrap();
    assert!(
        !json.contains("sk-abc") && !json.contains("$2b$") && !json.contains("CREATE TABLE"),
        "{json}"
    );
    // A system database is refused outright.
    let refused = build(&handle, "c", "mysql", &CancellationToken::new(), &quiet).await.unwrap_err();
    assert!(refused.to_string().contains("system database"), "{refused}");
}

#[tokio::test]
async fn fingerprints_are_stable_and_change_per_object() {
    let catalog = Catalog::new();
    let (_manager, handle) = catalog_handle(&catalog);
    let first = build(&handle, "c", "shop", &CancellationToken::new(), &quiet).await.unwrap();
    let again = build(&handle, "c", "shop", &CancellationToken::new(), &quiet).await.unwrap();
    assert_eq!(first.fingerprint, again.fingerprint);
    assert_eq!(first.checks, again.checks);
    assert!(fingerprint::diff(&first, &again).is_empty());
    // Reordering columns is no change; a new column, a dropped table and a new table are.
    {
        let mut tables = catalog.tables.lock().unwrap();
        tables[3].columns.reverse();
        tables[1].columns.push(("shipped_at", "timestamp", ValueKind::Timestamp));
        tables.retain(|table| table.name != "order_items");
        tables.push(table("public", "invoices", &[("id", "int"), ("order_id", "int")]));
    }
    let changed = build(&handle, "c", "shop", &CancellationToken::new(), &quiet).await.unwrap();
    assert_ne!(first.fingerprint, changed.fingerprint);
    assert_ne!(first.checks.listing, changed.checks.listing);
    let diff = fingerprint::diff(&first, &changed);
    assert_eq!(diff.added, ["public.invoices"]);
    assert_eq!(diff.changed, ["public.orders"]);
    assert_eq!(diff.removed, ["public.order_items"]);
    assert_eq!(diff.unchanged, 2);
    assert_eq!(
        first.object("products").unwrap().fingerprint,
        changed.object("products").unwrap().fingerprint
    );
}

#[tokio::test]
async fn a_cancelled_build_stops_and_stores_nothing() {
    let catalog = Catalog::new();
    let (_manager, handle) = catalog_handle(&catalog);
    let (store, dir) = temporary_store("cancel");
    let runs = AiRuns::default();
    let stop = |progress: Progress| {
        if progress.step == Step::Describing && progress.done == 1 {
            runs.cancel("run-1");
        }
    };
    let error = learn(&runs, &store, &handle, "shop", "run-1", None, &stop).await.unwrap_err();
    assert_eq!(error.code(), Some(CANCELLED), "{error}");
    assert!(catalog.described.lock().unwrap().len() <= 2);
    assert!(store.load("c", "shop").unwrap().is_none());
    // The slot is free again and a later build succeeds.
    let outcome = learn(&runs, &store, &handle, "shop", "run-2", None, &quiet).await.unwrap();
    assert!(!outcome.refreshed && outcome.status.exists);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn refreshing_keeps_notes_and_descriptions_and_reports_staleness() {
    let catalog = Catalog::new();
    let (_manager, handle) = catalog_handle(&catalog);
    let (store, dir) = temporary_store("refresh");
    let runs = AiRuns::default();
    let first = learn(&runs, &store, &handle, "shop", "r", None, &quiet).await.unwrap();
    assert_eq!(first.diff.added.len(), 4);
    let note = ProfileNote {
        id: "n1".into(),
        text: "An order is paid once status = 'paid'".into(),
        object: Some("orders".into()),
        created_at: 0,
        updated_at: 0,
    };
    store.update_notes("c", "shop", vec![note]).unwrap();
    store
        .update_description("c", "shop", "public.orders", Some("Customer orders".into()))
        .unwrap();
    store
        .update_description("c", "shop", "products", Some("What we sell".into()))
        .unwrap();
    let status = status(&store, Some(&handle), "c", "shop", 0, false).await.unwrap();
    assert_eq!((status.exists, status.stale), (true, Some(false)));
    catalog.tables.lock().unwrap()[1]
        .columns
        .push(("shipped_at", "timestamp", ValueKind::Timestamp));
    catalog.tables.lock().unwrap().push(table("public", "invoices", &[("id", "int")]));
    // The cached answer holds for ten minutes unless forced.
    assert_eq!(
        super::status(&store, Some(&handle), "c", "shop", 0, false).await.unwrap().stale,
        Some(false)
    );
    // ... but not once a statement that may change the schema ran through the app (the gateway's epoch moved).
    assert_eq!(
        super::status(&store, Some(&handle), "c", "shop", 1, false).await.unwrap().stale,
        Some(true)
    );
    assert_eq!(
        super::status(&store, Some(&handle), "c", "shop", 1, false).await.unwrap().stale,
        Some(true),
        "the new answer is cached at the new epoch"
    );
    assert_eq!(
        super::status(&store, Some(&handle), "c", "shop", 0, true).await.unwrap().stale,
        Some(true)
    );
    assert_eq!(
        super::status(&store, None, "c", "other", 0, false).await.unwrap(),
        ProfileStatus::default()
    );
    let refreshed = learn(&runs, &store, &handle, "shop", "r", None, &quiet).await.unwrap();
    assert!(refreshed.refreshed);
    assert_eq!(
        (refreshed.diff.added.clone(), refreshed.diff.changed.clone()),
        (vec!["public.invoices".to_owned()], vec!["public.orders".to_owned()])
    );
    assert_eq!(refreshed.status.stale, Some(false));
    let profile = store.load("c", "shop").unwrap().unwrap();
    assert_eq!(profile.notes.len(), 1);
    assert_eq!(profile.first_learned_at, store_first_learned(&first));
    let orders = profile.object("orders").unwrap();
    assert_eq!(
        orders.description.as_ref().map(|d| (d.text.as_str(), d.source)),
        Some(("Customer orders", DescriptionSource::User))
    );
    assert!(profile.object("invoices").unwrap().description.is_none());
    let _ = std::fs::remove_dir_all(dir);
}

fn store_first_learned(outcome: &LearnOutcome) -> i64 {
    outcome.status.learned_at.unwrap()
}

#[tokio::test]
async fn merge_keeps_model_knowledge_only_for_unchanged_objects() {
    let catalog = Catalog::new();
    let (_manager, handle) = catalog_handle(&catalog);
    let mut before = build(&handle, "c", "shop", &CancellationToken::new(), &quiet).await.unwrap();
    for object in &mut before.objects {
        object.description = Some(model::Description {
            text: format!("about {}", object.name),
            source: DescriptionSource::Llm,
            updated_at: 1,
            fingerprint: None,
        });
        object.value_hints = vec![ValueHint {
            column: "status".into(),
            values: vec!["paid".into()],
            format: None,
        }];
    }
    catalog.tables.lock().unwrap()[1]
        .columns
        .push(("shipped_at", "timestamp", ValueKind::Timestamp));
    let after = build(&handle, "c", "shop", &CancellationToken::new(), &quiet).await.unwrap();
    let (merged, diff) = merge(Some(before), after);
    assert_eq!(diff.changed, ["public.orders"]);
    let orders = merged.object("orders").unwrap();
    assert!(orders.description.is_none() && orders.value_hints.is_empty());
    let products = merged.object("products").unwrap();
    assert!(products.description.is_some() && !products.value_hints.is_empty());
    assert!(merged.data_read);
}

#[tokio::test]
async fn the_store_caps_sets_aside_damaged_files_and_forgets_deleted_connections() {
    let catalog = Catalog::new();
    let (_manager, handle) = catalog_handle(&catalog);
    let (store, dir) = temporary_store("store");
    let profile = build(&handle, "c", "shop", &CancellationToken::new(), &quiet).await.unwrap();
    store.refresh(profile.clone()).unwrap();
    let mut other = profile.clone();
    other.database = "crm".into();
    store.refresh(other).unwrap();
    let mut elsewhere = profile.clone();
    elsewhere.connection_id = "d".into();
    store.refresh(elsewhere).unwrap();
    let path = store.path("c", "shop");
    assert_eq!(path.file_name().unwrap().to_string_lossy().len(), 64 + ".json".len());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
    }
    // A damaged file is moved aside and reads as none.
    std::fs::write(&path, b"{ not json").unwrap();
    assert!(store.load("c", "shop").unwrap().is_none());
    let mut corrupt = path.as_os_str().to_owned();
    corrupt.push(CORRUPT_SUFFIX);
    assert!(std::path::Path::new(&corrupt).exists());
    assert!(!path.exists());
    assert!(store.update_notes("c", "shop", Vec::new()).is_err());
    // Deleting a connection deletes its profiles only.
    assert_eq!(store.remove_connection("c").unwrap(), 1);
    assert!(store.load("c", "crm").unwrap().is_none());
    assert!(store.load("d", "shop").unwrap().is_some());
    assert!(store.delete("d", "shop").unwrap());
    assert!(!store.delete("d", "shop").unwrap());
    // A build too large for the file keeps its objects by name past the cap.
    let mut huge = profile;
    let template = huge.objects[0].clone();
    huge.objects = (0..3000)
        .map(|index| {
            let mut object = template.clone();
            object.name = format!("t{index}");
            object.comment = Some("x".repeat(400));
            for column in 0..12 {
                let mut extra = object.columns[0].clone();
                extra.name = format!("column_{column}_with_a_long_name");
                extra.comment = Some("y".repeat(200));
                object.columns.push(extra);
            }
            object
        })
        .collect();
    let (stored, _) = store.refresh(huge).unwrap();
    assert!(std::fs::metadata(store.path("c", "shop")).unwrap().len() <= MAX_PROFILE_BYTES as u64);
    assert!(stored.truncated);
    assert!(stored.detailed_objects < 3000);
    assert_eq!(stored.objects.len(), 3000);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn deletions_remove_leftover_files_and_win_over_a_running_build() {
    let catalog = Catalog::new();
    let (_manager, handle) = catalog_handle(&catalog);
    let (store, dir) = temporary_store("leftovers");
    let profile = build(&handle, "c", "shop", &CancellationToken::new(), &quiet).await.unwrap();
    let leftovers = |path: &std::path::Path| {
        let mut corrupt = path.as_os_str().to_owned();
        corrupt.push(CORRUPT_SUFFIX);
        [std::path::PathBuf::from(corrupt), path.with_extension("tmp")]
    };
    // A file moved aside for its version (it still names its connection) and a half-written file.
    store.refresh(profile.clone()).unwrap();
    let path = store.path("c", "shop");
    let mut old = serde_json::to_value(&profile).unwrap();
    old["version"] = serde_json::json!(99);
    std::fs::write(&path, serde_json::to_vec(&old).unwrap()).unwrap();
    assert!(store.load("c", "shop").unwrap().is_none());
    std::fs::write(path.with_extension("tmp"), b"{ half").unwrap();
    assert!(leftovers(&path).iter().all(|file| file.exists()));
    assert_eq!(store.remove_connection("c").unwrap(), 0, "no profile left, only leftovers");
    assert!(leftovers(&path).iter().all(|file| !file.exists()));
    // Delete removes them too.
    store.refresh(profile.clone()).unwrap();
    for file in leftovers(&path) {
        std::fs::write(file, b"{ not json").unwrap();
    }
    assert!(store.delete("c", "shop").unwrap());
    assert!(!path.exists() && leftovers(&path).iter().all(|file| !file.exists()));
    // A deletion during a build wins: the build's write is refused, a later build saves again.
    for delete_connection in [false, true] {
        let slot = store.begin_learning("c", "shop").unwrap();
        if delete_connection {
            store.remove_connection("c").unwrap();
        } else {
            store.delete("c", "shop").unwrap();
        }
        let refused = store.refresh(profile.clone()).unwrap_err();
        assert!(refused.to_string().contains("deleted"), "{refused}");
        assert!(store.load("c", "shop").unwrap().is_none());
        drop(slot);
        store.refresh(profile.clone()).unwrap();
        assert!(store.delete("c", "shop").unwrap());
    }
    // Another database's build is not affected.
    let slot = store.begin_learning("c", "crm").unwrap();
    store.delete("c", "shop").unwrap();
    let mut crm = profile.clone();
    crm.database = "crm".into();
    store.refresh(crm).unwrap();
    drop(slot);
    // Through a whole build: deleted while describing, nothing is stored.
    let runs = AiRuns::default();
    let deleting = |progress: Progress| {
        if progress.step == Step::Describing && progress.done == 1 {
            store.delete("c", "shop").unwrap();
        }
    };
    let error = learn(&runs, &store, &handle, "shop", "run", None, &deleting).await.unwrap_err();
    assert!(error.to_string().contains("deleted"), "{error}");
    assert!(store.load("c", "shop").unwrap().is_none());
    learn(&runs, &store, &handle, "shop", "run", None, &quiet).await.unwrap();
    assert!(store.load("c", "shop").unwrap().is_some());
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn notes_are_validated() {
    let catalog = Catalog::new();
    let (_manager, handle) = catalog_handle(&catalog);
    let (store, dir) = temporary_store("notes");
    store
        .refresh(build(&handle, "c", "shop", &CancellationToken::new(), &quiet).await.unwrap())
        .unwrap();
    let note = |id: &str, text: &str| ProfileNote {
        id: id.into(),
        text: text.into(),
        object: None,
        created_at: 0,
        updated_at: 0,
    };
    assert!(store.update_notes("c", "shop", vec![note("a", "x"), note("a", "y")]).is_err());
    assert!(store.update_notes("c", "shop", vec![note("a", &"z".repeat(5000))]).is_err());
    let saved = store.update_notes("c", "shop", vec![note("a", " keep "), note("b", "  ")]).unwrap();
    assert_eq!(saved.notes.len(), 1);
    assert_eq!(saved.notes[0].text, "keep");
    assert!(saved.notes[0].created_at > 0);
    assert!(store.update_description("c", "shop", "nope", Some("x".into())).is_err());
    let cleared = store.update_description("c", "shop", "orders", None).unwrap();
    assert!(cleared.object("orders").unwrap().description.is_none());
    let _ = std::fs::remove_dir_all(dir);
}
