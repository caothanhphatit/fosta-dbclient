//! Connecting: distribution and version detection from `/`, feature flags derived from them, cluster health.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{client::EsClient, config::EsConnectOptions};
use crate::error::{AppError, AppResult};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Distribution {
    Elasticsearch,
    /// Elastic Cloud Serverless (`build_flavor: serverless`): no node, shard or ILM APIs.
    ElasticsearchServerless,
    OpenSearch,
    Unknown,
}

/// What `/` says about the cluster.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClusterInfo {
    pub distribution: Distribution,
    /// `8.19.3`, `2.19.1`...
    pub version: String,
    pub major: u32,
    pub minor: u32,
    pub cluster_name: String,
    pub cluster_uuid: Option<String>,
    pub node_name: Option<String>,
    pub lucene_version: Option<String>,
    pub build_flavor: Option<String>,
}

/// Which APIs the cluster has, so the UI shows only what works.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EsFeatures {
    /// ES|QL (`POST /_query`, Elasticsearch 8.11+).
    pub esql: bool,
    /// SQL: `/_sql` (Elasticsearch) or `/_plugins/_sql` (OpenSearch).
    pub sql: bool,
    /// Point in time: `/<index>/_pit` (ES 7.10+) or `/<index>/_search/point_in_time` (OpenSearch 2.4+).
    pub point_in_time: bool,
    /// Lifecycle policies: ILM (`/_ilm`) on Elasticsearch, ISM (`/_plugins/_ism`) on OpenSearch.
    pub lifecycle: LifecycleApi,
    pub data_streams: bool,
    /// Composable index templates and component templates (ES 7.8+, OpenSearch 1.0+).
    pub composable_templates: bool,
    /// Node, shard and `_cat` APIs (absent on serverless).
    pub node_apis: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum LifecycleApi {
    Ilm,
    Ism,
    None,
}

impl ClusterInfo {
    pub fn from_root(root: &Value) -> AppResult<Self> {
        let version = &root["version"];
        let number = version["number"]
            .as_str()
            .ok_or_else(|| AppError::new("This URL does not look like Elasticsearch or OpenSearch (no version in the root response)."))?;
        let build_flavor = version["build_flavor"].as_str().map(str::to_string);
        let distribution = match (version["distribution"].as_str(), build_flavor.as_deref()) {
            (Some("opensearch"), _) => Distribution::OpenSearch,
            (_, Some("serverless")) => Distribution::ElasticsearchServerless,
            (None, _) if root["tagline"].as_str().is_some() => Distribution::Elasticsearch,
            _ => Distribution::Unknown,
        };
        let mut parts = number.split(['.', '-']).map(|part| part.parse::<u32>().unwrap_or(0));
        Ok(Self {
            distribution,
            version: number.to_string(),
            major: parts.next().unwrap_or(0),
            minor: parts.next().unwrap_or(0),
            cluster_name: root["cluster_name"].as_str().unwrap_or_default().to_string(),
            cluster_uuid: root["cluster_uuid"].as_str().map(str::to_string),
            node_name: root["name"].as_str().map(str::to_string),
            lucene_version: version["lucene_version"].as_str().map(str::to_string),
            build_flavor,
        })
    }

    pub fn is_opensearch(&self) -> bool {
        self.distribution == Distribution::OpenSearch
    }

    fn at_least(&self, major: u32, minor: u32) -> bool {
        (self.major, self.minor) >= (major, minor)
    }

    pub fn features(&self) -> EsFeatures {
        match self.distribution {
            Distribution::OpenSearch => EsFeatures {
                esql: false,
                sql: true,
                point_in_time: self.at_least(2, 4),
                lifecycle: LifecycleApi::Ism,
                data_streams: true,
                composable_templates: true,
                node_apis: true,
            },
            Distribution::ElasticsearchServerless => EsFeatures {
                esql: true,
                sql: true,
                point_in_time: true,
                lifecycle: LifecycleApi::None,
                data_streams: true,
                composable_templates: true,
                node_apis: false,
            },
            Distribution::Elasticsearch | Distribution::Unknown => EsFeatures {
                esql: self.at_least(8, 11),
                sql: self.at_least(6, 3),
                point_in_time: self.at_least(7, 10),
                lifecycle: if self.at_least(6, 6) {
                    LifecycleApi::Ilm
                } else {
                    LifecycleApi::None
                },
                data_streams: self.at_least(7, 9),
                composable_templates: self.at_least(7, 8),
                node_apis: true,
            },
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClusterHealth {
    /// green, yellow or red.
    pub status: String,
    pub number_of_nodes: u64,
    pub number_of_data_nodes: u64,
    pub active_primary_shards: u64,
    pub active_shards: u64,
    pub relocating_shards: u64,
    pub initializing_shards: u64,
    pub unassigned_shards: u64,
    pub active_shards_percent: Option<f64>,
}

/// An open cluster connection. Cheap to clone; every call is a stateless HTTP request.
#[derive(Clone)]
pub struct EsEngine {
    pub(crate) client: EsClient,
    pub(crate) info: ClusterInfo,
}

impl EsEngine {
    /// Calls `/` (checks the URL, credentials and TLS) and detects the distribution and version.
    pub async fn connect(options: &EsConnectOptions) -> AppResult<Self> {
        Self::connect_with(options, None).await
    }

    /// [`Self::connect`] with a rustls configuration from the connection's TLS settings.
    pub async fn connect_with(options: &EsConnectOptions, tls: Option<rustls::ClientConfig>) -> AppResult<Self> {
        let client = EsClient::with_tls(options, tls)?;
        let root = client.get("/").await?;
        let info = ClusterInfo::from_root(&root)?;
        Ok(Self { client, info })
    }

    pub fn info(&self) -> &ClusterInfo {
        &self.info
    }

    pub fn features(&self) -> EsFeatures {
        self.info.features()
    }

    pub fn client(&self) -> &EsClient {
        &self.client
    }

    pub async fn health(&self) -> AppResult<ClusterHealth> {
        let body = self.client.get("/_cluster/health").await?;
        let number = |key: &str| body[key].as_u64().unwrap_or(0);
        Ok(ClusterHealth {
            status: body["status"].as_str().unwrap_or("unknown").to_string(),
            number_of_nodes: number("number_of_nodes"),
            number_of_data_nodes: number("number_of_data_nodes"),
            active_primary_shards: number("active_primary_shards"),
            active_shards: number("active_shards"),
            relocating_shards: number("relocating_shards"),
            initializing_shards: number("initializing_shards"),
            unassigned_shards: number("unassigned_shards"),
            active_shards_percent: body["active_shards_percent_as_number"].as_f64(),
        })
    }
}

/// Percent-encodes one path segment (index names, ids, template names).
pub fn segment(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~' | b'*' | b',' | b'+' | b':' | b'@') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn detects_distributions() {
        let es = ClusterInfo::from_root(&json!({
            "name": "n1", "cluster_name": "c", "version": { "number": "8.19.3", "build_flavor": "default" }, "tagline": "You Know, for Search"
        }))
        .unwrap();
        assert_eq!((es.distribution, es.major, es.minor), (Distribution::Elasticsearch, 8, 19));
        assert!(es.features().esql);
        assert_eq!(es.features().lifecycle, LifecycleApi::Ilm);

        let os = ClusterInfo::from_root(&json!({
            "cluster_name": "c", "version": { "distribution": "opensearch", "number": "2.19.1" }, "tagline": "The OpenSearch Project"
        }))
        .unwrap();
        assert_eq!(os.distribution, Distribution::OpenSearch);
        assert!(!os.features().esql);
        assert_eq!(os.features().lifecycle, LifecycleApi::Ism);

        let old = ClusterInfo::from_root(&json!({ "version": { "number": "7.9.0" }, "tagline": "x" })).unwrap();
        assert!(!old.features().point_in_time);
        let serverless = ClusterInfo::from_root(&json!({ "version": { "number": "8.11.0", "build_flavor": "serverless" } })).unwrap();
        assert_eq!(serverless.distribution, Distribution::ElasticsearchServerless);
        assert!(ClusterInfo::from_root(&json!({ "hello": 1 })).is_err());
    }

    #[test]
    fn encodes_segments() {
        assert_eq!(segment("logs-app.2026/11"), "logs-app.2026%2F11");
        assert_eq!(segment("doc id#1"), "doc%20id%231");
    }
}
