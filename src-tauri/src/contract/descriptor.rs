//! Engine descriptors: built in Rust, served to the UI by `engine_descriptors`, so the connection dialog, the engine
//! picker, the filter box and the query editor have one source of truth.

use serde::{Deserialize, Serialize};

use super::{capability::CapabilitySet, spec::SecretSlot};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum EngineFamily {
    Sql,
    Document,
    KeyValue,
    Search,
}

/// `planned` engines are registered (descriptor, classifier, URL parser) but cannot connect yet; the UI hides them unless
/// `localStorage["fosta.dev.showPlannedEngines"] === "1"`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum EngineStatus {
    Ready,
    Preview,
    Planned,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SqlDialectId {
    MariaDb,
    MySql,
    MsSql,
    Postgres,
    Sqlite,
    ClickHouse,
    Oracle,
    Generic,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AuthMethodId {
    Password,
    None,
    ApiKey,
    Bearer,
    AclUser,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TlsModeId {
    Disable,
    Prefer,
    Require,
    VerifyCa,
    VerifyFull,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UrlSpec {
    pub schemes: Vec<String>,
    pub example: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TlsSupport {
    pub modes: Vec<TlsModeId>,
    pub ca: bool,
    pub client_cert: bool,
    pub server_name: bool,
    pub pin: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FormSection {
    #[default]
    General,
    Auth,
    Advanced,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum FieldKind {
    Text,
    Number,
    Password { slot: SecretSlot },
    Checkbox,
    Select { options: Vec<(String, String)> },
    File { filters: Vec<String>, save: bool },
    Textarea,
}

/// One extra field of the connection form. `key` is `"database"`, `"file"`, `"url"` or `"options.<name>"`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FieldSpec {
    pub key: String,
    pub kind: FieldKind,
    pub label: String,
    #[serde(default)]
    pub placeholder: Option<String>,
    #[serde(default)]
    pub hint: Option<String>,
    #[serde(default)]
    pub section: FormSection,
    #[serde(default)]
    pub default: Option<String>,
    #[serde(default)]
    pub required: bool,
}

impl FieldSpec {
    pub fn new(key: &str, kind: FieldKind, label: &str) -> Self {
        Self {
            key: key.to_owned(),
            kind,
            label: label.to_owned(),
            placeholder: None,
            hint: None,
            section: FormSection::General,
            default: None,
            required: false,
        }
    }

    pub fn hint(mut self, hint: &str) -> Self {
        self.hint = Some(hint.to_owned());
        self
    }

    pub fn placeholder(mut self, placeholder: &str) -> Self {
        self.placeholder = Some(placeholder.to_owned());
        self
    }

    pub fn default_value(mut self, value: &str) -> Self {
        self.default = Some(value.to_owned());
        self
    }

    pub fn section(mut self, section: FormSection) -> Self {
        self.section = section;
        self
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionForm {
    pub uses_host: bool,
    pub uses_file: bool,
    pub default_port: Option<u16>,
    pub default_user: String,
    pub database_label: String,
    pub database_hint: String,
    pub url: Option<UrlSpec>,
    pub auth: Vec<AuthMethodId>,
    pub fields: Vec<FieldSpec>,
    pub ssh: bool,
    pub tls: Option<TlsSupport>,
}

/// A query language a session can run, and the editor that edits it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LanguageSpec {
    /// `"sql"`, `"mongo-command"`, `"redis"`, `"es-console"`, `"esql"`, `"es-sql"`.
    pub id: String,
    pub label: String,
    /// The editor the UI module provides (`"sql"`, `"json"`, `"redis"`, `"es-console"`).
    pub editor: String,
    #[serde(default)]
    pub dialect: Option<SqlDialectId>,
    #[serde(default)]
    pub default: bool,
    /// One line under the editor that says what to type and how to run it.
    #[serde(default)]
    pub hint: String,
    /// Code-fence tags of this language in assistant replies: the assistant writes the first (```sql), and a block
    /// tagged with any of them runs in this language from the chat.
    #[serde(default)]
    pub fences: Vec<String>,
}

impl LanguageSpec {
    /// `fences` from string literals.
    pub fn fence_tags(tags: &[&str]) -> Vec<String> {
        tags.iter().map(|tag| (*tag).to_owned()).collect()
    }

    /// The fence tag the assistant writes for this language (empty when it has none).
    pub fn fence(&self) -> &str {
        self.fences.first().map(String::as_str).unwrap_or_default()
    }
}

/// A prompt the assistant panel offers as a quick action for this engine (prefilled in the composer).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssistantPrompt {
    pub label: String,
    /// An `IconName` from `src/core/dom.ts`.
    pub icon: String,
    pub prompt: String,
}

/// What the assistant panel says and offers for an engine, so it never compares engine ids.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssistantHints {
    /// How test data for one object is written: "INSERT statements", "an insertMany command".
    pub test_data: String,
    /// Extra quick actions in query tabs and on the connection ("Aggregation pipeline").
    #[serde(default)]
    pub prompts: Vec<AssistantPrompt>,
    /// Query tabs of this engine offer "Translate to…".
    #[serde(default)]
    pub translate_from: bool,
    /// The engine as a translation target, in "Translate this to …"; `None` leaves it out of the menu.
    #[serde(default)]
    pub translate_to: Option<String>,
    /// The menu label of the target; the engine label when `None`.
    #[serde(default)]
    pub translate_label: Option<String>,
}

impl AssistantHints {
    /// A SQL engine: INSERT statements, translatable from and to (as `label`).
    pub fn sql(label: &str) -> Self {
        Self {
            test_data: "INSERT statements".into(),
            prompts: Vec::new(),
            translate_from: true,
            translate_to: Some(label.to_owned()),
            translate_label: None,
        }
    }

    /// No longer a translation target (another engine stands for the same dialect).
    pub fn not_a_target(mut self) -> Self {
        self.translate_to = None;
        self
    }
}

impl Default for AssistantHints {
    fn default() -> Self {
        Self {
            test_data: "statements".into(),
            prompts: Vec::new(),
            translate_from: false,
            translate_to: None,
            translate_label: None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RawFilterLang {
    Sql,
    MongoJson,
    EsQueryDsl,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FilterSupport {
    /// Column filters as structured conditions (bound by the engine).
    pub structured: bool,
    /// The language of the raw filter box, when there is one.
    pub raw: Option<RawFilterLang>,
    pub placeholder: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EngineDescriptor {
    pub id: String,
    pub label: String,
    pub family: EngineFamily,
    /// An `IconName` from `src/core/dom.ts`.
    pub icon: String,
    pub status: EngineStatus,
    pub sql_dialect: Option<SqlDialectId>,
    /// The static upper bound; a connection reports its actual set in `ConnectInfo`.
    pub capabilities: CapabilitySet,
    /// The name of the second tree level ("tables", "collections", "keys", "indices").
    pub objects_label: String,
    pub connection: ConnectionForm,
    pub languages: Vec<LanguageSpec>,
    pub filter: FilterSupport,
    /// What the assistant panel offers for this engine.
    #[serde(default)]
    pub assistant: AssistantHints,
}

impl EngineDescriptor {
    pub fn default_language(&self) -> Option<&LanguageSpec> {
        self.languages
            .iter()
            .find(|language| language.default)
            .or_else(|| self.languages.first())
    }
}
