//! The engine extras hook against an in-memory document engine: metadata (a stable basis per collection, database
//! knowledge), reads through the gateway (`doc.sample` per collection, `doc.keys` for the database) and what their
//! answers add (dotted fields with presence, a reference, sampled values, a key pattern).

use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc, Mutex, OnceLock,
};

use async_trait::async_trait;
use serde_json::{json, Map, Value as Json};

use super::{
    extras::{enrich, ExtrasReads},
    learn_with,
    model::RelationSource,
    store::ProfileStore,
    BuildReads, DatabaseProfile, Progress, Sampler,
};
use crate::{
    ai::AiRuns,
    contract::{
        testing::fake, CallContext, CancellationToken, Capability, CapabilitySet, Category, Classification, ClassifyInput, ColumnInfo,
        ConnectRequest, Connection, Engine, EngineDescriptor, EngineFamily, ExtrasRead, FieldReference, FieldSamples, FieldShape, KeyInfo,
        NodeKey, NodeKind, ObjectDescription, ObjectExtras, ObjectNode, ObjectRef, OpenedSession, ProfileEnrichment, ProfileExtras,
        ServerInfo, SessionOptions, StatementInfo, TreeChildren, TreeRequest, ValueKind, KEY_PATTERN,
    },
    error::AppResult,
    gateway::{
        rules::{Preset, SafetySettings},
        Context,
    },
    runtime::{ConnectionManager, Handle},
};

struct DocEngine;

#[async_trait]
impl Engine for DocEngine {
    fn descriptor(&self) -> &EngineDescriptor {
        static DESCRIPTOR: OnceLock<EngineDescriptor> = OnceLock::new();
        DESCRIPTOR.get_or_init(|| {
            let mut descriptor = fake::descriptor();
            descriptor.id = "fakedoc".into();
            descriptor.family = EngineFamily::Document;
            descriptor.capabilities = CapabilitySet::of(&[Capability::Tree, Capability::Describe]);
            descriptor
        })
    }

    fn classify(&self, input: &ClassifyInput<'_>) -> Classification {
        match input {
            ClassifyInput::Capability { name, .. } if *name == "doc.sample" || *name == "doc.keys" => {
                Classification::single(StatementInfo::new(Category::Read, name, name))
            }
            _ => Classification::single(StatementInfo::new(Category::Write, "WRITE", "write")),
        }
    }

    async fn connect(&self, _request: ConnectRequest) -> AppResult<Arc<dyn Connection>> {
        Err(crate::error::AppError::new("not used"))
    }
}

#[derive(Default)]
pub(crate) struct State {
    pub(crate) invoked: Mutex<Vec<String>>,
    /// Bumped per build: the sampled fields differ from one build to the next, like a random sample.
    builds: AtomicUsize,
    /// Another index spec: the metadata basis changes.
    reindexed: std::sync::atomic::AtomicBool,
    /// `doc.sample` fails.
    failing: std::sync::atomic::AtomicBool,
}

struct DocConn {
    state: Arc<State>,
    info: ServerInfo,
}

fn node(label: &str, kind: NodeKind, object: Option<ObjectRef>) -> ObjectNode {
    ObjectNode {
        key: NodeKey {
            id: label.into(),
            kind: kind.clone(),
            object: object.clone(),
        },
        label: label.into(),
        expandable: !kind.is_relation(),
        kind,
        icon: "table".into(),
        object,
        detail: None,
        badges: Vec::new(),
        default_view: None,
        actions: Vec::new(),
    }
}

const COLLECTIONS: [&str; 2] = ["orders", "customers"];

#[async_trait]
impl Connection for DocConn {
    fn engine_id(&self) -> &str {
        "fakedoc"
    }

    fn info(&self) -> &ServerInfo {
        &self.info
    }

    fn capabilities(&self) -> CapabilitySet {
        CapabilitySet::of(&[Capability::Tree, Capability::Describe])
    }

    async fn children(&self, request: &TreeRequest, _ctx: &CallContext) -> AppResult<TreeChildren> {
        let nodes = match request.parent {
            None => vec![node(
                "shop",
                NodeKind::DATABASE,
                Some(ObjectRef::new(Some("shop"), None, "shop", NodeKind::DATABASE)),
            )],
            Some(_) => COLLECTIONS
                .iter()
                .map(|name| {
                    node(
                        name,
                        NodeKind::COLLECTION,
                        Some(ObjectRef::new(Some("shop"), None, name, NodeKind::COLLECTION)),
                    )
                })
                .collect(),
        };
        Ok(TreeChildren {
            nodes,
            next_cursor: None,
            total_hint: None,
        })
    }

    async fn describe(&self, object: &ObjectRef, _ctx: &CallContext) -> AppResult<ObjectDescription> {
        // Top-level fields from a sample that moves between builds, like MongoDB's describe.
        let mut description = ObjectDescription::new(object.clone());
        let build = self.state.builds.load(Ordering::Relaxed);
        let mut id = ColumnInfo::result("_id", "objectId", ValueKind::ObjectId);
        id.nullable = false;
        description.columns = vec![id, ColumnInfo::result("status", "string", ValueKind::Text)];
        if build % 2 == 1 {
            description.columns.push(ColumnInfo::result("rare", "string", ValueKind::Text));
        }
        description.primary_key = Some(KeyInfo {
            name: None,
            columns: vec!["_id".into()],
        });
        Ok(description)
    }

    async fn open_session(&self, _options: &SessionOptions, _ctx: &CallContext) -> AppResult<OpenedSession> {
        Err(crate::error::AppError::new("not used"))
    }

    async fn invoke(&self, name: &str, args: Json, _ctx: &CallContext) -> AppResult<Json> {
        self.state.invoked.lock().unwrap().push(name.to_owned());
        if name == "doc.sample" && self.state.failing.load(Ordering::Relaxed) {
            return Err(crate::error::AppError::new("the sample failed"));
        }
        Ok(json!({ "name": name, "args": args, "build": self.state.builds.load(Ordering::Relaxed) }))
    }

    fn profile_extras(&self) -> Option<&dyn ProfileExtras> {
        Some(self)
    }

    async fn close(&self) {}
}

fn field(path: &str, type_name: &str, kind: ValueKind, presence: f32) -> FieldShape {
    FieldShape {
        path: path.into(),
        type_name: type_name.into(),
        kind,
        nullable: presence < 100.0,
        primary: path == "_id",
        presence: Some(presence),
    }
}

fn samples(path: &str, values: &[&str], more: bool) -> FieldSamples {
    FieldSamples {
        path: path.into(),
        values: values.iter().map(|value| (*value).to_owned()).collect(),
        more,
    }
}

#[async_trait]
impl ProfileExtras for DocConn {
    async fn metadata(&self, _database: &str, objects: &[ObjectRef], _ctx: &CallContext) -> AppResult<ProfileEnrichment> {
        let reindexed = self.state.reindexed.load(Ordering::Relaxed);
        let mut enrichment = ProfileEnrichment::default();
        for object in objects {
            let mut extras = ObjectExtras::new(object.clone());
            extras.fingerprint_basis = Some(format!(
                "{}\nindexes {}",
                object.name,
                if reindexed { "_id, status" } else { "_id" }
            ));
            enrichment.objects.push(extras);
        }
        let mut knowledge = Map::new();
        knowledge.insert(
            "views".into(),
            json!({ "recent": "orders", "token": "sk-abcdefghijklmnopqrstuvwx1234" }),
        );
        enrichment.extras = knowledge;
        Ok(enrichment)
    }

    fn reads(&self, database: &str, objects: &[ObjectRef]) -> Vec<ExtrasRead> {
        let mut reads: Vec<ExtrasRead> = objects
            .iter()
            .map(|object| ExtrasRead {
                object: Some(object.clone()),
                name: "doc.sample".into(),
                args: json!({ "database": database, "collection": object.name }),
            })
            .collect();
        reads.push(ExtrasRead {
            object: None,
            name: "doc.keys".into(),
            args: json!({ "database": database }),
        });
        reads
    }

    fn interpret(&self, _database: &str, answers: &[(ExtrasRead, Json)]) -> ProfileEnrichment {
        let mut enrichment = ProfileEnrichment::default();
        for (read, answer) in answers {
            let build = answer["build"].as_u64().unwrap_or(0);
            match (read.name.as_str(), &read.object) {
                ("doc.sample", Some(object)) if object.name == "orders" => {
                    let mut extras = ObjectExtras::new(object.clone());
                    let mut fields = vec![
                        field("_id", "objectId", ValueKind::ObjectId, 100.0),
                        field("status", "string", ValueKind::Text, 100.0),
                        field("customerId", "objectId", ValueKind::ObjectId, 98.0),
                        field("contact.email", "string", ValueKind::Text, 50.0),
                        field("contact.password", "string", ValueKind::Text, 50.0),
                        field("items.sku", "string | int", ValueKind::Text, 75.5),
                    ];
                    if build % 2 == 1 {
                        fields.push(field("rare", "string", ValueKind::Text, 0.2));
                    }
                    extras.fields = Some(fields);
                    extras.samples = vec![
                        samples("status", &["paid", "new"], false),
                        samples("contact.email", &["a@example.com", "b@example.org"], false),
                        samples("contact.password", &["hunter2"], false),
                        samples("_id", &["1", "2"], false),
                    ];
                    enrichment.objects.push(extras);
                    enrichment.references.push(FieldReference {
                        from: "orders".into(),
                        path: "customerId".into(),
                        to: "customers".into(),
                        to_field: "_id".into(),
                        confidence: 0.75,
                    });
                }
                ("doc.keys", None) => {
                    let mut pattern = ObjectExtras::new(ObjectRef::new(Some("shop"), None, "session:{id}", KEY_PATTERN));
                    pattern.facts = vec![("type".into(), "hash".into()), ("keys sampled".into(), "12".into())];
                    enrichment.objects.push(pattern);
                }
                _ => {}
            }
        }
        enrichment
    }
}

/// Opens the document engine as `id` on `manager`; the state records what was read.
pub(crate) fn insert_doc(manager: &ConnectionManager, id: &str) -> Arc<State> {
    static ENGINE: DocEngine = DocEngine;
    let state = Arc::new(State::default());
    manager.insert(
        id,
        &ENGINE,
        Arc::new(DocConn {
            state: Arc::clone(&state),
            info: ServerInfo::default(),
        }),
    );
    state
}

fn setup() -> (ConnectionManager, Handle, Arc<State>) {
    let manager = ConnectionManager::default();
    let state = insert_doc(&manager, "c");
    let handle = manager.handle("c").unwrap();
    (manager, handle, state)
}

fn context(safety: SafetySettings) -> Context {
    Context {
        workspace_id: "w".into(),
        workspace_name: "Team".into(),
        connection_id: "c".into(),
        connection_name: "Main".into(),
        safety,
    }
}

fn open() -> Context {
    context(SafetySettings::preset(Preset::Open))
}

fn store(name: &str) -> (Arc<ProfileStore>, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("fosta-extras-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    (Arc::new(ProfileStore::new(dir.clone())), dir)
}

fn quiet(_: Progress) {}

async fn learn(
    manager: &ConnectionManager,
    handle: &Handle,
    store: &Arc<ProfileStore>,
    reads: Option<(Context, bool)>,
) -> (super::LearnOutcome, DatabaseProfile) {
    let reads = reads.map(|(context, values)| BuildReads {
        sampler: Sampler { manager, context },
        values,
    });
    let outcome = learn_with(&AiRuns::default(), store, handle, "shop", "r", reads, true, &quiet)
        .await
        .unwrap();
    (outcome, store.load("c", "shop").unwrap().unwrap())
}

#[tokio::test]
async fn the_hook_adds_fields_references_hints_and_patterns() {
    let (manager, handle, state) = setup();
    let (store, dir) = store("all");
    let (outcome, profile) = learn(&manager, &handle, &store, Some((open(), true))).await;
    let orders = profile.object("orders").unwrap();
    let names: Vec<&str> = orders.columns.iter().map(|column| column.name.as_str()).collect();
    assert_eq!(
        names,
        ["_id", "status", "customerId", "contact.email", "contact.password", "items.sku"]
    );
    let sku = orders.column("items.sku").unwrap();
    assert_eq!((sku.type_name.as_str(), sku.presence), ("string | int", Some(75.5)));
    assert!(orders.column("contact.password").unwrap().sensitive);
    assert_eq!(orders.primary_key, ["_id"]);
    // Hints follow the value rules: values for a short list, the format only for emails, nothing for sensitive
    // fields or keys.
    let hint = |column: &str| orders.value_hints.iter().find(|hint| hint.column == column).cloned();
    assert_eq!(hint("status").unwrap().values, ["new", "paid"]);
    let email = hint("contact.email").unwrap();
    assert_eq!((email.format.as_deref(), email.values.is_empty()), (Some("email"), true));
    assert!(hint("contact.password").is_none() && hint("_id").is_none());
    assert!(profile.data_read);
    // The matched reference replaces the guess from the name.
    let to_customers: Vec<_> = profile
        .relations
        .iter()
        .filter(|relation| relation.from == "orders" && relation.columns == ["customerId"])
        .collect();
    assert_eq!(to_customers.len(), 1, "{:?}", profile.relations);
    assert_eq!(
        (to_customers[0].source, to_customers[0].to.as_str(), to_customers[0].confidence),
        (RelationSource::MongoRef, "customers", 0.75)
    );
    // The key pattern is a new object, marked as a pattern.
    let pattern = profile.object("session:{id}").unwrap();
    assert_eq!(pattern.kind, KEY_PATTERN);
    assert!(pattern.extra.contains(&("type".to_owned(), "hash".to_owned())));
    assert_eq!(profile.total_objects, 3);
    // Engine knowledge is scrubbed.
    assert_eq!(profile.extras["views"]["recent"], "orders");
    assert_ne!(profile.extras["views"]["token"], "sk-abcdefghijklmnopqrstuvwx1234");
    let extras = outcome.extras.unwrap();
    assert_eq!((extras.reads, extras.failed, extras.references), (3, 0, 1), "{extras:?}");
    assert_eq!(state.invoked.lock().unwrap().len(), 3);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn sampled_values_need_the_values_gate() {
    let (manager, handle, _) = setup();
    let (store, dir) = store("gate");
    let (outcome, profile) = learn(&manager, &handle, &store, Some((open(), false))).await;
    let orders = profile.object("orders").unwrap();
    assert!(orders.column("contact.email").is_some(), "fields come without the gate");
    assert!(profile.objects.iter().all(|object| object.value_hints.is_empty()));
    assert!(!profile.data_read && outcome.values.is_none());
    assert_eq!(outcome.extras.unwrap().hinted, 0);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn without_reads_only_metadata_is_used() {
    let (manager, handle, state) = setup();
    let (store, dir) = store("metadata");
    let (outcome, profile) = learn(&manager, &handle, &store, None).await;
    assert!(state.invoked.lock().unwrap().is_empty(), "nothing read");
    let orders = profile.object("orders").unwrap();
    assert_eq!(orders.columns.len(), 2, "the described columns stay");
    assert!(profile.object("session:{id}").is_none());
    assert_eq!(profile.extras["views"]["recent"], "orders");
    let extras = outcome.extras.unwrap();
    assert_eq!(extras.reads, 0);
    assert_eq!(extras.skipped.as_deref(), Some(super::extras::NO_READS));
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn fingerprints_rest_on_the_metadata_basis() {
    let (manager, handle, state) = setup();
    let (store, dir) = store("fingerprint");
    let (_, first) = learn(&manager, &handle, &store, Some((open(), true))).await;
    // The next sample has another field; the structure did not change.
    state.builds.fetch_add(1, Ordering::Relaxed);
    let (again, second) = learn(&manager, &handle, &store, Some((open(), true))).await;
    assert!(second.object("orders").unwrap().column("rare").is_some());
    assert_eq!(
        first.object("orders").unwrap().fingerprint,
        second.object("orders").unwrap().fingerprint
    );
    assert_eq!(first.fingerprint, second.fingerprint);
    assert!(again.diff.is_empty(), "{:?}", again.diff);
    // Without reads the basis is the same, too (the key pattern, which needs a read, is gone).
    let (quiet_build, _) = learn(&manager, &handle, &store, None).await;
    assert!(quiet_build.diff.changed.is_empty(), "{:?}", quiet_build.diff);
    assert_eq!(quiet_build.diff.removed, ["session:{id}"]);
    // A changed index spec is a change.
    state.reindexed.store(true, Ordering::Relaxed);
    let (reindexed, _) = learn(&manager, &handle, &store, Some((open(), true))).await;
    assert_eq!(reindexed.diff.changed.len(), 2, "{:?}", reindexed.diff);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn refused_reads_stop_and_keep_the_structure() {
    let (manager, handle, state) = setup();
    let mut confirm = SafetySettings::preset(Preset::Custom);
    confirm.rules.read = crate::gateway::rules::Action::Confirm;
    let token = CancellationToken::new();
    let mut profile = super::build::build(&handle, "c", "shop", &token, &quiet).await.unwrap();
    let sampler = Sampler {
        manager: &manager,
        context: context(confirm),
    };
    let reads = ExtrasReads {
        sampler: &sampler,
        values: true,
    };
    let (outcome, sampled) = enrich(&handle, Some(reads), &mut profile, &token, &quiet).await.unwrap().unwrap();
    assert!(outcome.skipped.unwrap().contains("refused"));
    assert_eq!(outcome.reads, 1);
    assert!(sampled.is_empty() && state.invoked.lock().unwrap().is_empty());
    assert_eq!(profile.object("orders").unwrap().columns.len(), 2);
}

#[tokio::test]
async fn engines_without_extras_are_left_alone() {
    let manager = ConnectionManager::default();
    let engine = fake::FakeEngine::leak();
    manager.insert("c", engine, engine.connection());
    let handle = manager.handle("c").unwrap();
    let token = CancellationToken::new();
    let mut profile = super::build::build(&handle, "c", "shop", &token, &quiet).await.unwrap();
    let before = profile.clone();
    let sampler = Sampler {
        manager: &manager,
        context: open(),
    };
    let reads = ExtrasReads {
        sampler: &sampler,
        values: true,
    };
    assert!(enrich(&handle, Some(reads), &mut profile, &token, &quiet).await.unwrap().is_none());
    assert_eq!(profile, before);
    assert!(!engine.calls.lock().unwrap().iter().any(|call| call == "invoke"));
}

/// Against the MongoDB container (`db-studio-dev-mongo-1`, root / studio-root on 127.0.0.1:27017): nested paths,
/// references, gated hints and fingerprints that stay put across builds.
#[tokio::test]
#[ignore = "needs the MongoDB container (dev/compose); run with --ignored --test-threads=1"]
async fn live_mongo_extras() {
    use crate::contract::{CollectSink, ConnectSecrets, ConnectionSpec, RunOptions, RunRequest};
    let env = |name: &str, default: &str| std::env::var(format!("FOSTA_CONTRACT_MONGO_{name}")).unwrap_or_else(|_| default.into());
    let spec = ConnectionSpec {
        engine: "mongodb".into(),
        host: env("HOST", "127.0.0.1"),
        port: env("PORT", "27017").parse().unwrap_or(27017),
        user: env("USER", "root"),
        database: Some("admin".into()),
        ..ConnectionSpec::default()
    };
    let manager = ConnectionManager::default();
    manager
        .connect("c", spec, ConnectSecrets::password(&env("PASSWORD", "studio-root")))
        .await
        .expect("connect");
    let handle = manager.handle("c").unwrap();
    let oid = |index: usize| json!({ "$oid": format!("{index:024x}") });
    let customers: Vec<Json> = (0..10usize)
        .map(|index| json!({ "_id": oid(index + 1), "name": format!("c{index}") }))
        .collect();
    const STATUSES: [&str; 3] = ["new", "paid", "shipped"];
    let orders: Vec<Json> = (0..60usize)
        .map(|index| {
            json!({
                "_id": index,
                "status": STATUSES[index % 3],
                "customerId": oid(index % 10 + 1),
                "code": if index % 2 == 0 { json!(index) } else { json!(format!("C{index}")) },
                "contact": { "email": format!("user{index}@example.com"), "password": format!("secret-{index}") },
                "items": [{ "sku": format!("p{}", index % 4), "qty": 1 }],
            })
        })
        .collect();
    let session = handle
        .call("opened a session", |conn| async move {
            conn.open_session(&SessionOptions::default(), &CallContext::default()).await
        })
        .await
        .unwrap()
        .session;
    for command in [
        json!({ "drop": "orders" }),
        json!({ "drop": "customers" }),
        json!({ "insert": "customers", "documents": customers }),
        json!({ "insert": "orders", "documents": orders }),
        json!({ "createIndexes": "orders", "indexes": [{ "key": { "status": 1 }, "name": "status_1" }] }),
    ] {
        let request = RunRequest {
            text: command.to_string(),
            database: Some("fosta_extras".into()),
            offset: 0,
        };
        let mut sink = CollectSink::default();
        // Dropping a collection that is not there fails; that is fine.
        let _ = handle.run(Arc::clone(&session), request, RunOptions::default(), &mut sink).await;
    }
    let (store, dir) = store("live-mongo");
    let runs = AiRuns::default();
    let build = |values: bool| {
        let reads = BuildReads {
            sampler: Sampler {
                manager: &manager,
                context: open(),
            },
            values,
        };
        learn_with(&runs, &store, &handle, "fosta_extras", "r", Some(reads), values, &quiet)
    };
    let first = build(true).await.unwrap();
    let profile = store.load("c", "fosta_extras").unwrap().unwrap();
    let orders = profile.object("orders").unwrap();
    let column = |name: &str| orders.column(name).unwrap_or_else(|| panic!("{name}: {:?}", orders.columns));
    assert_eq!(column("contact.email").presence, Some(100.0));
    assert_eq!(column("items.sku").type_name, "string");
    assert!(
        column("items").type_name.starts_with("array<object"),
        "{}",
        column("items").type_name
    );
    let code = &column("code").type_name;
    assert!(code.contains("int") && code.contains("string"), "{code}");
    assert!(column("contact.password").sensitive);
    let reference = profile
        .relations
        .iter()
        .find(|relation| relation.from == "orders" && relation.columns == ["customerId"])
        .unwrap();
    assert_eq!((reference.source, reference.to.as_str()), (RelationSource::MongoRef, "customers"));
    assert!(reference.confidence > 0.9, "{reference:?}");
    let hint = |column: &str| orders.value_hints.iter().find(|hint| hint.column == column).cloned();
    assert_eq!(hint("status").unwrap().values, STATUSES);
    assert_eq!(hint("contact.email").unwrap().format.as_deref(), Some("email"));
    assert!(hint("contact.email").unwrap().values.is_empty());
    assert!(hint("contact.password").is_none() && hint("_id").is_none());
    let extras = first.extras.unwrap();
    assert_eq!(extras.failed, 0, "{extras:?}");
    assert!(profile.data_read);
    // A second build samples other documents; the fingerprints stay.
    let again = build(true).await.unwrap();
    assert!(again.diff.is_empty(), "{:?}", again.diff);
    let second = store.load("c", "fosta_extras").unwrap().unwrap();
    assert_eq!(profile.fingerprint, second.fingerprint);
    // Without the values gate (an explicit opt-out) the fields stay and the hints go.
    build(false).await.unwrap();
    let gated = store.load("c", "fosta_extras").unwrap().unwrap();
    let orders = gated.object("orders").unwrap();
    assert!(orders.column("contact.email").is_some() && orders.value_hints.is_empty());
    assert!(!gated.data_read);
    manager.disconnect("c").await;
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn a_build_writes_one_audit_entry_for_its_reads_and_one_per_failure() {
    let (mut manager, _, state) = setup();
    let path = std::env::temp_dir().join(format!("fosta-extras-audit-{}.jsonl", std::process::id()));
    let _ = std::fs::remove_file(&path);
    manager.gateway = crate::gateway::Gateway::with_audit(crate::audit::AuditLog::new(path.clone(), 1000));
    let handle = manager.handle("c").unwrap();
    let (store, dir) = store("audit");
    let (outcome, _) = learn(&manager, &handle, &store, Some((open(), true))).await;
    assert_eq!(outcome.extras.unwrap().reads, 3);
    let entries = crate::audit::AuditLog::new(path.clone(), 1000).read_all().unwrap();
    assert_eq!(entries.len(), 1, "{entries:?}");
    let summary = &entries[0];
    assert_eq!((summary.caller.as_str(), summary.category.as_str()), ("assistant", "read"));
    assert!(
        summary.statement.contains("doc.sample × 2") && summary.statement.contains("doc.keys × 1"),
        "{}",
        summary.statement
    );
    assert!(summary.detail.starts_with("3 reads, 0 failed"), "{}", summary.detail);
    // Failed reads are audited one by one, with the summary.
    state.failing.store(true, Ordering::Relaxed);
    let _ = std::fs::remove_file(&path);
    let (outcome, _) = learn(&manager, &handle, &store, Some((open(), true))).await;
    assert_eq!(outcome.extras.unwrap().failed, 2);
    let entries = crate::audit::AuditLog::new(path.clone(), 1000).read_all().unwrap();
    let failed = entries.iter().filter(|entry| entry.detail.starts_with("failed:")).count();
    assert_eq!((entries.len(), failed), (3, 2), "{entries:?}");
    assert!(entries.iter().any(|entry| entry.detail.starts_with("3 reads, 2 failed")));
    let _ = std::fs::remove_file(path);
    let _ = std::fs::remove_dir_all(dir);
}
