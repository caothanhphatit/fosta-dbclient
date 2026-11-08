//! Safety rules per workspace: a preset (Open, Guarded, Read-only) or a Custom matrix of
//! category → action, plus a few options. Stored as the `safety` object of a workspace in
//! `fosta-studio.json`; the shapes mirror `SafetySettings` in `src/core/types.ts`.

use serde::{Deserialize, Serialize};

use super::classify::Category;

/// Workspace icon that marks a production environment.
pub const PRODUCTION_ICON: &str = "shield";
const DEFAULT_MAX_ROWS: u32 = 1000;
/// Upper bound for the row limit of query results, whatever the rules say.
pub const MAX_ROWS_LIMIT: u32 = 100_000;
const GUARDED_TIMEOUT_SECS: u32 = 60;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Preset {
    /// Everything runs; destructive statements ask once (the dialog can stop asking).
    #[default]
    Open,
    /// Writes and DDL need a confirmation; destructive and administrative statements a typed one.
    Guarded,
    /// Reads only, backed by read-only sessions where the engine has them.
    ReadOnly,
    /// The `rules` matrix and options as stored.
    Custom,
}

/// What happens to a statement of one category. Ordered from least to most strict.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Action {
    Allow,
    Confirm,
    /// Confirm by typing the workspace or table name.
    Typed,
    Block,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CategoryRules {
    pub read: Action,
    pub write: Action,
    pub ddl: Action,
    pub destructive: Action,
    pub admin: Action,
}

impl CategoryRules {
    const OPEN: Self = Self {
        read: Action::Allow,
        write: Action::Allow,
        ddl: Action::Allow,
        destructive: Action::Confirm,
        admin: Action::Allow,
    };
    const GUARDED: Self = Self {
        read: Action::Allow,
        write: Action::Confirm,
        ddl: Action::Confirm,
        destructive: Action::Typed,
        admin: Action::Typed,
    };
    const READ_ONLY: Self = Self {
        read: Action::Allow,
        write: Action::Block,
        ddl: Action::Block,
        destructive: Action::Block,
        admin: Action::Block,
    };

    /// The action for `category`. Transaction control is always allowed (Commit and Rollback
    /// must work even in a Read-only workspace); unparsed SQL is treated like a write.
    pub fn action(&self, category: Category) -> Action {
        match category {
            Category::Transaction => Action::Allow,
            Category::Read => self.read,
            Category::Write | Category::Unknown => self.write,
            Category::Ddl => self.ddl,
            Category::Destructive => self.destructive,
            Category::Admin => self.admin,
        }
    }

    fn entries(&self) -> [(Category, Action); 5] {
        [
            (Category::Read, self.read),
            (Category::Write, self.write),
            (Category::Ddl, self.ddl),
            (Category::Destructive, self.destructive),
            (Category::Admin, self.admin),
        ]
    }
}

impl Default for CategoryRules {
    fn default() -> Self {
        Self::GUARDED
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SafetySettings {
    pub preset: Preset,
    /// The matrix used by the Custom preset (kept when switching presets, so it is not lost).
    #[serde(default)]
    pub rules: CategoryRules,
    /// Custom only: several statements in one query-tab run.
    #[serde(default = "enabled")]
    pub allow_multi_statement: bool,
    /// Custom only: statement timeout in seconds, 0 = none.
    #[serde(default)]
    pub statement_timeout_secs: u32,
    /// Custom only: rows kept per result set in query tabs.
    #[serde(default = "default_max_rows")]
    pub max_rows: u32,
    /// The setting this workspace started from; anything looser counts as relaxed and shows
    /// a warning badge. Production workspaces always compare against Read-only.
    #[serde(default)]
    pub baseline: Option<Preset>,
}

fn enabled() -> bool {
    true
}

fn default_max_rows() -> u32 {
    DEFAULT_MAX_ROWS
}

/// The rules in force, after the preset is applied.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Effective {
    pub rules: CategoryRules,
    pub allow_multi_statement: bool,
    pub statement_timeout_secs: u32,
    pub max_rows: u32,
}

impl Effective {
    /// Every non-read category is blocked: sessions can be put in read-only mode.
    pub fn read_only(&self) -> bool {
        [self.rules.write, self.rules.ddl, self.rules.destructive, self.rules.admin]
            .iter()
            .all(|action| *action == Action::Block)
    }
}

impl SafetySettings {
    pub fn preset(preset: Preset) -> Self {
        Self {
            preset,
            rules: CategoryRules::default(),
            allow_multi_statement: true,
            statement_timeout_secs: 0,
            max_rows: DEFAULT_MAX_ROWS,
            baseline: Some(preset),
        }
    }

    /// The default for a workspace that has no `safety` yet: Production (shield icon)
    /// is Read-only, every other existing workspace keeps the old, open behaviour.
    pub fn default_for_icon(icon: &str) -> Self {
        let mut settings = Self::preset(if icon == PRODUCTION_ICON { Preset::ReadOnly } else { Preset::Open });
        settings.baseline = Some(settings.preset);
        settings
    }

    /// New workspaces start Guarded (Read-only when created as Production).
    pub fn for_new_workspace(icon: &str) -> Self {
        Self::preset(if icon == PRODUCTION_ICON {
            Preset::ReadOnly
        } else {
            Preset::Guarded
        })
    }

    /// The confirmation dialog's "Don't ask again": these rules as a Custom setting with every
    /// category in `categories` that asks for a plain confirmation allowed. Typed confirmations
    /// and blocks stay. None when nothing would change.
    pub fn without_confirmation(&self, categories: &[Category]) -> Option<Self> {
        let effective = self.effective();
        let mut rules = effective.rules.clone();
        let mut changed = false;
        for category in categories {
            let slot = match category {
                Category::Transaction => continue,
                Category::Read => &mut rules.read,
                Category::Write | Category::Unknown => &mut rules.write,
                Category::Ddl => &mut rules.ddl,
                Category::Destructive => &mut rules.destructive,
                Category::Admin => &mut rules.admin,
            };
            if *slot == Action::Confirm {
                *slot = Action::Allow;
                changed = true;
            }
        }
        if !changed {
            return None;
        }
        Some(Self {
            preset: Preset::Custom,
            rules,
            allow_multi_statement: effective.allow_multi_statement,
            statement_timeout_secs: effective.statement_timeout_secs,
            max_rows: effective.max_rows,
            baseline: self.baseline,
        })
    }

    pub fn effective(&self) -> Effective {
        let (rules, timeout) = match self.preset {
            Preset::Open => (CategoryRules::OPEN, 0),
            Preset::Guarded => (CategoryRules::GUARDED, GUARDED_TIMEOUT_SECS),
            Preset::ReadOnly => (CategoryRules::READ_ONLY, GUARDED_TIMEOUT_SECS),
            Preset::Custom => {
                return Effective {
                    rules: self.rules.clone(),
                    allow_multi_statement: self.allow_multi_statement,
                    statement_timeout_secs: self.statement_timeout_secs,
                    max_rows: self.max_rows.clamp(1, MAX_ROWS_LIMIT),
                };
            }
        };
        Effective {
            rules,
            allow_multi_statement: true,
            statement_timeout_secs: timeout,
            max_rows: DEFAULT_MAX_ROWS,
        }
    }

    /// What this workspace is compared against for the "relaxed" warning.
    pub fn baseline_for(&self, icon: &str) -> Preset {
        if icon == PRODUCTION_ICON {
            Preset::ReadOnly
        } else {
            self.baseline.unwrap_or(Preset::Open)
        }
    }

    /// The ways this setting is looser than its baseline (empty when it is not relaxed).
    pub fn relaxations_from_baseline(&self, icon: &str) -> Vec<String> {
        relaxations(&Self::preset(self.baseline_for(icon)).effective(), &self.effective())
    }
}

/// Every way `next` is less safe than `previous`, as short sentences (empty when it is not).
pub fn relaxations(previous: &Effective, next: &Effective) -> Vec<String> {
    let mut changes = Vec::new();
    for ((category, before), (_, after)) in previous.rules.entries().into_iter().zip(next.rules.entries()) {
        if after < before {
            changes.push(format!(
                "{} statements: {} → {}",
                category.label(),
                action_label(before),
                action_label(after)
            ));
        }
    }
    if next.allow_multi_statement && !previous.allow_multi_statement {
        changes.push("several statements per run allowed".to_owned());
    }
    let looser_timeout = previous.statement_timeout_secs > 0
        && (next.statement_timeout_secs == 0 || next.statement_timeout_secs > previous.statement_timeout_secs);
    if looser_timeout {
        changes.push(format!(
            "statement timeout: {} → {}",
            timeout_label(previous.statement_timeout_secs),
            timeout_label(next.statement_timeout_secs)
        ));
    }
    if next.max_rows > previous.max_rows {
        changes.push(format!("max rows: {} → {}", previous.max_rows, next.max_rows));
    }
    changes
}

fn action_label(action: Action) -> &'static str {
    match action {
        Action::Allow => "allowed",
        Action::Confirm => "confirm",
        Action::Typed => "typed confirmation",
        Action::Block => "blocked",
    }
}

fn timeout_label(seconds: u32) -> String {
    if seconds == 0 {
        "none".to_owned()
    } else {
        format!("{seconds} s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presets_map_categories_to_actions() {
        let guarded = SafetySettings::preset(Preset::Guarded).effective();
        assert_eq!(guarded.rules.action(Category::Read), Action::Allow);
        assert_eq!(guarded.rules.action(Category::Write), Action::Confirm);
        assert_eq!(
            guarded.rules.action(Category::Unknown),
            Action::Confirm,
            "unparsed SQL is treated like a write"
        );
        assert_eq!(guarded.rules.action(Category::Ddl), Action::Confirm);
        assert_eq!(guarded.rules.action(Category::Destructive), Action::Typed);
        assert_eq!(guarded.rules.action(Category::Admin), Action::Typed);
        assert_eq!(guarded.statement_timeout_secs, 60);
        let read_only = SafetySettings::preset(Preset::ReadOnly).effective();
        assert!(read_only.read_only());
        assert_eq!(read_only.rules.action(Category::Unknown), Action::Block);
        assert_eq!(
            read_only.rules.action(Category::Transaction),
            Action::Allow,
            "commit/rollback always work"
        );
        let open = SafetySettings::preset(Preset::Open).effective();
        assert!(!open.read_only());
        assert_eq!(open.statement_timeout_secs, 0);
        assert_eq!(
            open.rules.action(Category::Destructive),
            Action::Confirm,
            "Open asks once before DROP or DELETE without WHERE"
        );
        assert!(Category::ALL
            .iter()
            .filter(|category| **category != Category::Destructive)
            .all(|category| open.rules.action(*category) == Action::Allow));
    }

    #[test]
    fn dont_ask_again_allows_only_plain_confirmations() {
        let open = SafetySettings::preset(Preset::Open);
        let skipped = open.without_confirmation(&[Category::Destructive]).expect("destructive asked");
        assert_eq!(skipped.preset, Preset::Custom);
        assert_eq!(skipped.baseline, Some(Preset::Open), "the relaxed badge still compares with Open");
        assert!(Category::ALL
            .iter()
            .all(|category| skipped.effective().rules.action(*category) == Action::Allow));
        assert_eq!(skipped.without_confirmation(&[Category::Destructive]), None, "nothing left to skip");

        let guarded = SafetySettings::preset(Preset::Guarded);
        let skipped = guarded
            .without_confirmation(&[Category::Write, Category::Destructive, Category::Transaction])
            .expect("writes asked");
        let rules = skipped.effective().rules;
        assert_eq!(rules.write, Action::Allow);
        assert_eq!(rules.ddl, Action::Confirm, "only the categories that asked");
        assert_eq!(rules.destructive, Action::Typed, "typed confirmations keep asking");
        assert_eq!(skipped.effective().statement_timeout_secs, 60, "the preset's timeout is kept");
        assert_eq!(
            SafetySettings::preset(Preset::ReadOnly).without_confirmation(&[Category::Write]),
            None
        );
    }

    #[test]
    fn custom_uses_the_stored_matrix_and_options() {
        let settings = SafetySettings {
            preset: Preset::Custom,
            rules: CategoryRules {
                read: Action::Allow,
                write: Action::Allow,
                ddl: Action::Block,
                destructive: Action::Block,
                admin: Action::Block,
            },
            allow_multi_statement: false,
            statement_timeout_secs: 5,
            max_rows: 10_000_000,
            baseline: None,
        };
        let effective = settings.effective();
        assert_eq!(effective.rules.action(Category::Write), Action::Allow);
        assert_eq!(effective.rules.action(Category::Ddl), Action::Block);
        assert!(!effective.allow_multi_statement);
        assert_eq!(effective.statement_timeout_secs, 5);
        assert_eq!(effective.max_rows, MAX_ROWS_LIMIT);
    }

    #[test]
    fn defaults_keep_existing_workspaces_open_and_production_read_only() {
        assert_eq!(SafetySettings::default_for_icon("laptop").preset, Preset::Open);
        assert_eq!(SafetySettings::default_for_icon("shield").preset, Preset::ReadOnly);
        assert_eq!(SafetySettings::for_new_workspace("cloud").preset, Preset::Guarded);
        assert_eq!(SafetySettings::for_new_workspace("shield").preset, Preset::ReadOnly);
        let parsed: SafetySettings = serde_json::from_str(r#"{ "preset": "readOnly" }"#).unwrap();
        assert_eq!(parsed.max_rows, 1000);
        assert!(parsed.allow_multi_statement);
    }

    #[test]
    fn relaxations_are_listed_and_baselines_respected() {
        let guarded = SafetySettings::preset(Preset::Guarded);
        let open = SafetySettings::preset(Preset::Open);
        let changes = relaxations(&guarded.effective(), &open.effective());
        assert!(
            changes
                .iter()
                .any(|change| change.starts_with("Write statements: confirm → allowed")),
            "{changes:?}"
        );
        assert!(changes.iter().any(|change| change.starts_with("statement timeout")), "{changes:?}");
        assert!(
            relaxations(&open.effective(), &guarded.effective()).is_empty(),
            "tightening is not a relaxation"
        );
        // An existing workspace that defaulted to Open is not "relaxed"...
        assert!(SafetySettings::default_for_icon("laptop")
            .relaxations_from_baseline("laptop")
            .is_empty());
        // ...a Guarded workspace switched to Open is, and so is a Production workspace that is not Read-only.
        let mut relaxed = SafetySettings::for_new_workspace("cloud");
        relaxed.preset = Preset::Open;
        assert!(!relaxed.relaxations_from_baseline("cloud").is_empty());
        assert!(!guarded.relaxations_from_baseline(PRODUCTION_ICON).is_empty());
        assert!(SafetySettings::preset(Preset::ReadOnly)
            .relaxations_from_baseline(PRODUCTION_ICON)
            .is_empty());
    }
}
