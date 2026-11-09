//! The MongoDB engine port. One driver `Client` (its own connection pool) serves the tree, browsing, edits and the
//! query tabs; a query tab is a database name and the client, each run gets its own server session.

use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use mongodb::{bson::doc, Client};

use super::{
    catalog,
    classify::classify_mongo,
    connect::{self, Topology},
    descriptor,
    session::{MongoSession, LANGUAGE},
    tabular, url,
};
use crate::{
    contract::{
        classify_common, ApplyResult, BrowseRequest, CallContext, CapabilitySet, Category, Classification, ClassifyInput, ConnectRequest,
        Connection, ConnectionSpec, DatabaseAdmin, Engine, EngineDescriptor, NodeKey, NodeKind, ObjectDescription, ObjectRef,
        OpenedSession, Page, ParsedConnection, RowChangeSet, SchemaGraph, SchemaGraphSource, ServerInfo, SessionOptions, StatementInfo,
        TabularData, TreeChildren, TreeRequest,
    },
    error::{AppError, AppResult},
};

pub struct MongoAdapter;

pub static MONGODB: MongoAdapter = MongoAdapter;

#[async_trait]
impl Engine for MongoAdapter {
    fn descriptor(&self) -> &EngineDescriptor {
        static DESCRIPTOR: OnceLock<EngineDescriptor> = OnceLock::new();
        DESCRIPTOR.get_or_init(descriptor::descriptor)
    }

    fn classify(&self, input: &ClassifyInput<'_>) -> Classification {
        if let Some(classification) = classify_common(input) {
            return classification;
        }
        match input {
            ClassifyInput::Statement { language: LANGUAGE, text } => classify_mongo(text),
            ClassifyInput::Statement { language, text } => Classification::single(
                StatementInfo::new(Category::Unknown, language, text)
                    .with_reason(&format!("MongoDB sessions do not run the '{language}' language.")),
            ),
            // MongoDB has no engine operations.
            ClassifyInput::Capability { name, .. } => Classification::single(StatementInfo::refused(name, name, "unknown operation")),
            _ => Classification::default(),
        }
    }

    fn parse_connection_string(&self, text: &str) -> AppResult<ParsedConnection> {
        url::parse(text)
    }

    fn format_connection_string(&self, spec: &ConnectionSpec) -> Option<String> {
        Some(url::format(spec))
    }

    async fn connect(&self, request: ConnectRequest) -> AppResult<Arc<dyn Connection>> {
        let options = connect::build_options(&request).await?;
        let client = Client::with_options(options)?;
        let (info, topology) = match connect::probe(&client).await {
            Ok(probed) => probed,
            Err(error) => {
                client.shutdown().await;
                return Err(error);
            }
        };
        Ok(Arc::new(MongoConnection {
            client,
            info,
            topology,
            capabilities: self.descriptor().capabilities,
        }))
    }
}

/// An open MongoDB deployment.
pub struct MongoConnection {
    client: Client,
    info: ServerInfo,
    topology: Topology,
    capabilities: CapabilitySet,
}

fn database_of_node(parent: &NodeKey) -> AppResult<String> {
    parent
        .object
        .as_ref()
        .map(|object| object.name.clone())
        .or_else(|| parent.id.strip_prefix("db:").map(str::to_owned))
        .ok_or_else(|| AppError::new("This item no longer exists. Refresh the tree."))
}

#[async_trait]
impl Connection for MongoConnection {
    fn engine_id(&self) -> &str {
        descriptor::ID
    }

    fn info(&self) -> &ServerInfo {
        &self.info
    }

    fn capabilities(&self) -> CapabilitySet {
        self.capabilities
    }

    async fn children(&self, request: &TreeRequest, _ctx: &CallContext) -> AppResult<TreeChildren> {
        let nodes: Vec<_> = match &request.parent {
            None => catalog::databases(&self.client)
                .await?
                .iter()
                .map(|name| catalog::database_node(name))
                .collect(),
            Some(parent) if parent.kind == NodeKind::DATABASE => {
                let database = database_of_node(parent)?;
                catalog::collections(&self.client, &database)
                    .await?
                    .iter()
                    .map(|entry| catalog::collection_node(&database, entry))
                    .collect()
            }
            Some(_) => Vec::new(),
        };
        let nodes: Vec<_> = match request.filter.as_deref().map(str::to_lowercase).filter(|text| !text.is_empty()) {
            Some(filter) => nodes
                .into_iter()
                .filter(|node| node.label.to_lowercase().contains(&filter))
                .collect(),
            None => nodes,
        };
        Ok(TreeChildren {
            total_hint: Some(nodes.len() as u64),
            nodes,
            next_cursor: None,
        })
    }

    async fn describe(&self, object: &ObjectRef, _ctx: &CallContext) -> AppResult<ObjectDescription> {
        catalog::describe(&self.client, object).await
    }

    async fn open_session(&self, options: &SessionOptions, _ctx: &CallContext) -> AppResult<OpenedSession> {
        if let Some(language) = options.language.as_deref().filter(|language| *language != LANGUAGE) {
            return Err(AppError::new(format!("MongoDB sessions do not run the '{language}' language.")));
        }
        Ok(OpenedSession {
            session: Arc::new(MongoSession::new(self.client.clone(), options.database.clone())),
            canceller: None,
        })
    }

    fn tabular(&self) -> Option<&dyn TabularData> {
        Some(self)
    }

    fn schema_graph(&self) -> Option<&dyn SchemaGraphSource> {
        Some(self)
    }

    fn admin(&self) -> Option<&dyn DatabaseAdmin> {
        Some(self)
    }

    async fn ping(&self) -> AppResult<()> {
        self.client.database("admin").run_command(doc! { "ping": 1 }).await?;
        Ok(())
    }

    async fn close(&self) {
        self.client.clone().shutdown().await;
    }
}

#[async_trait]
impl TabularData for MongoConnection {
    async fn browse(&self, request: &BrowseRequest, ctx: &CallContext) -> AppResult<Page> {
        if request.reverse {
            return Err(crate::contract::unsupported_capability("reading a collection from its end"));
        }
        tabular::browse(&self.client, request, ctx).await
    }

    async fn apply(&self, changes: &RowChangeSet, ctx: &CallContext) -> AppResult<ApplyResult> {
        tabular::apply(&self.client, self.topology.supports_transactions(), changes, ctx).await
    }
}

#[async_trait]
impl SchemaGraphSource for MongoConnection {
    async fn schema_graph(&self, database: &str, _ctx: &CallContext) -> AppResult<SchemaGraph> {
        catalog::schema_graph(&self.client, database).await
    }
}

#[async_trait]
impl DatabaseAdmin for MongoConnection {
    /// MongoDB creates a database together with its first collection.
    async fn create_database(&self, name: &str, first_collection: Option<&str>, _ctx: &CallContext) -> AppResult<()> {
        let collection = first_collection
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| AppError::new("MongoDB creates a database together with its first collection; enter a collection name"))?;
        self.client.database(name).create_collection(collection).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::Capability;

    fn statement(text: &str) -> Option<Category> {
        MONGODB.classify(&ClassifyInput::Statement { language: LANGUAGE, text }).worst()
    }

    #[test]
    fn the_descriptor_is_cached_without_transactions_or_stop() {
        assert_eq!(MONGODB.id(), "mongodb");
        assert!(std::ptr::eq(MONGODB.descriptor(), MONGODB.descriptor()));
        let capabilities = MONGODB.descriptor().capabilities;
        assert!(capabilities.contains(Capability::KeysetPaging) && capabilities.contains(Capability::RawFilter));
        assert!(!capabilities.contains(Capability::Transactions) && !capabilities.contains(Capability::Cancel));
    }

    #[test]
    fn commands_and_operations_are_classified() {
        assert_eq!(statement(r#"{ "find": "t" }"#), Some(Category::Read));
        assert_eq!(statement(r#"{ "insert": "t", "documents": [] }"#), Some(Category::Write));
        assert_eq!(statement(r#"{ "drop": "t" }"#), Some(Category::Destructive));
        let sql = MONGODB.classify(&ClassifyInput::Statement {
            language: "sql",
            text: "SELECT 1",
        });
        assert_eq!(sql.worst(), Some(Category::Unknown));
        let unknown = MONGODB.classify(&ClassifyInput::Capability {
            name: "vector.search",
            args: &serde_json::Value::Null,
        });
        assert_eq!(unknown.refusal(), Some("unknown operation"));
    }

    #[test]
    fn connection_strings_parse_and_format() {
        let parsed = MONGODB
            .parse_connection_string("mongodb://root:pw@127.0.0.1:27017/?authSource=admin")
            .unwrap();
        assert_eq!(parsed.spec.engine, "mongodb");
        assert_eq!(parsed.password.as_deref(), Some("pw"));
        assert_eq!(
            MONGODB.format_connection_string(&parsed.spec).as_deref(),
            Some("mongodb://root@127.0.0.1:27017/?authSource=admin")
        );
    }
}
