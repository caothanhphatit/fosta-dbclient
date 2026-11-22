//! The planner: the selected items of a [`SchemaDiff`] as engine-neutral [`MigrationStep`]s, in an order a server can
//! run them:
//!
//! 1. drop foreign keys (removed or changed ones, and the ones in the way of a dropped table, a dropped key or a
//!    column type change; those are added back in phase 11),
//! 2. drop views (removed or changed ones, and the ones reading an altered table or a dropped view; dependents first),
//! 3. drop indexes, unique and check constraints and primary keys,
//! 4. drop tables (only-in-target items the user selected),
//! 5. create tables (columns, primary key, uniques and checks),
//! 6. add columns, 7. alter columns (or rebuild the table), 8. drop columns,
//! 9. create indexes and constraints,
//! 10. create views (dependencies first),
//! 11. add foreign keys,
//! 12. comments and table options.
//!
//! Selecting an item pulls in what it needs (dependent views are recreated even when identical, foreign keys are added
//! back); what the planner cannot satisfy becomes a warning, never a silent omission. Where foreign keys live in the
//! table (SQLite), they go with `CREATE TABLE` or the rebuild instead of phases 1 and 11.

use std::collections::{HashMap, HashSet};

use super::{diff::Labels, options::ResolvedOptions};
use crate::contract::{
    Change, ChangeKind, CheckDef, ColumnChanges, CommentTarget, CompareOptions, ConstraintDef, ConstraintKind, DiffItem, DiffStatus,
    ForeignKeyDef, IndexDef, KeyDef, MigrationStep, Risk, SchemaDialect, SchemaDiff, SchemaSnapshot, ScriptWarning, TableDef, ViewDef,
};

/// A step with the diff item it serves and what the user should know about it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlannedStep {
    pub item_id: String,
    pub step: MigrationStep,
    /// Why a step the user did not ask for directly is there (a dependency).
    pub note: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Plan {
    pub steps: Vec<PlannedStep>,
    /// Dependencies the selection leaves unsatisfied, and anything else the script cannot do for the user.
    pub warnings: Vec<ScriptWarning>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Phase {
    DropForeignKeys,
    DropViews,
    DropConstraints,
    DropTables,
    CreateTables,
    AddColumns,
    AlterColumns,
    DropColumns,
    CreateConstraints,
    CreateViews,
    AddForeignKeys,
    Comments,
}

const PHASES: usize = Phase::Comments as usize + 1;

/// Plans the items of `diff` whose ids are in `include` (identical items need nothing). The dialect is the target's.
pub fn plan(
    diff: &SchemaDiff,
    include: &[String],
    source: &SchemaSnapshot,
    target: &SchemaSnapshot,
    options: &CompareOptions,
    dialect: &dyn SchemaDialect,
) -> Plan {
    let mut planner = Planner {
        names: ResolvedOptions::new(options, source, target),
        labels: Labels::new(source, target, options, dialect),
        dialect,
        source,
        target,
        options,
        phases: vec![Vec::new(); PHASES],
        warnings: Vec::new(),
        selected: include.iter().map(String::as_str).collect(),
        items: diff.items.iter().map(|item| (item.id.as_str(), item)).collect(),
        dropped_tables: HashSet::new(),
        created_tables: HashSet::new(),
        altered_tables: HashSet::new(),
        type_changed: HashSet::new(),
        key_dropped: HashSet::new(),
        dropped_fks: HashSet::new(),
        dropped_subs: HashSet::new(),
        held: HashMap::new(),
        view_drops: Vec::new(),
        view_creates: Vec::new(),
    };
    for item in &diff.items {
        if item.status != DiffStatus::Identical && planner.selected.contains(item.id.as_str()) {
            match item.kind.as_str() {
                "table" => planner.table(item),
                "view" => planner.view(item),
                _ => {}
            }
        }
    }
    planner.held_dependents();
    planner.foreign_key_dependencies();
    planner.view_dependencies();
    planner.order_views();
    Plan {
        steps: planner.phases.into_iter().flatten().collect(),
        warnings: planner.warnings,
    }
}

struct Planner<'a> {
    names: ResolvedOptions<'a>,
    labels: Labels<'a>,
    dialect: &'a dyn SchemaDialect,
    source: &'a SchemaSnapshot,
    target: &'a SchemaSnapshot,
    options: &'a CompareOptions,
    phases: Vec<Vec<PlannedStep>>,
    warnings: Vec<ScriptWarning>,
    selected: HashSet<&'a str>,
    items: HashMap<&'a str, &'a DiffItem>,
    /// Folded table names.
    dropped_tables: HashSet<String>,
    created_tables: HashSet<String>,
    /// Tables whose views must be dropped and recreated (a column type change or drop, a rebuild).
    altered_tables: HashSet<String>,
    /// (table, column), folded.
    type_changed: HashSet<(String, String)>,
    /// Tables losing a primary key or unique constraint that foreign keys may reference.
    key_dropped: HashSet<String>,
    /// (table, foreign key label), folded.
    dropped_fks: HashSet<(String, String)>,
    /// (table, part, name) of the keys, checks and indexes the plan drops, folded.
    dropped_subs: HashSet<(String, &'static str, String)>,
    /// Where columns are held by their dependents (SQL Server): per folded table, the folded columns the plan alters
    /// and the ones it drops.
    held: HashMap<String, (HashSet<String>, HashSet<String>)>,
    view_drops: Vec<(String, ViewDef, Option<String>)>,
    view_creates: Vec<(String, ViewDef, Option<String>)>,
}

impl<'a> Planner<'a> {
    fn fold(&self, name: &str) -> String {
        self.names.fold(name)
    }

    fn push(&mut self, phase: Phase, item_id: &str, step: MigrationStep) {
        self.push_noted(phase, item_id, step, None);
    }

    fn push_noted(&mut self, phase: Phase, item_id: &str, step: MigrationStep, note: Option<String>) {
        let dropped = match &step {
            MigrationStep::DropConstraint {
                table,
                kind,
                name: Some(name),
            } => Some((table, constraint_part(*kind), name)),
            MigrationStep::DropIndex { table, name } => Some((table, "index", name)),
            _ => None,
        };
        if let Some((table, part, name)) = dropped {
            self.dropped_subs.insert((self.fold(table), part, self.fold(name)));
        }
        self.phases[phase as usize].push(PlannedStep {
            item_id: item_id.to_owned(),
            step,
            note,
        });
    }

    fn warn(&mut self, item_id: &str, risk: Risk, message: String) {
        self.warnings.push(ScriptWarning {
            item_id: item_id.to_owned(),
            risk,
            message,
        });
    }

    fn manual(&mut self, phase: Phase, item_id: &str, reason: String) {
        self.push(phase, item_id, MigrationStep::Manual { reason, ddl_hint: None });
    }

    fn source_table(&self, name: &str) -> Option<&'a TableDef> {
        self.source.tables.iter().find(|t| self.names.fold(&t.name) == self.fold(name))
    }

    fn target_table(&self, name: &str) -> Option<&'a TableDef> {
        self.target.tables.iter().find(|t| self.names.fold(&t.name) == self.fold(name))
    }

    fn source_view(&self, name: &str) -> Option<&'a ViewDef> {
        self.source.views.iter().find(|v| self.names.fold(&v.name) == self.fold(name))
    }

    fn target_view(&self, name: &str) -> Option<&'a ViewDef> {
        self.target.views.iter().find(|v| self.names.fold(&v.name) == self.fold(name))
    }

    /// Whether a foreign key points into the scope (`None`, or the side's own schema).
    fn in_scope(&self, key: &ForeignKeyDef, schema: Option<&str>) -> bool {
        match (&key.ref_schema, schema) {
            (None, _) => true,
            (Some(reference), Some(scope)) => self.fold(reference) == self.fold(scope),
            (Some(_), None) => false,
        }
    }

    fn comments(&self) -> bool {
        !self.options.ignore_comments
    }

    // --- Tables -------------------------------------------------------------------------------------------------------

    fn table(&mut self, item: &'a DiffItem) {
        let id = item.id.as_str();
        match item.status {
            DiffStatus::OnlyInSource => {
                let Some(table) = self.source_table(&item.name) else { return };
                self.create_table(id, table);
            }
            DiffStatus::OnlyInTarget => {
                let Some(table) = self.target_table(&item.name) else { return };
                self.dropped_tables.insert(self.fold(&table.name));
                self.push(Phase::DropTables, id, MigrationStep::DropTable { name: table.name.clone() });
            }
            DiffStatus::Different => {
                let (Some(source), Some(target)) = (self.source_table(&item.name), self.target_table(&item.name)) else {
                    return;
                };
                if self.dialect.needs_rebuild(item, source, target) {
                    self.rebuild(item, source, target);
                } else {
                    for change in &item.changes {
                        self.table_change(id, change, source, target);
                    }
                }
            }
            DiffStatus::Identical => {}
        }
    }

    fn create_table(&mut self, id: &str, table: &TableDef) {
        self.created_tables.insert(self.fold(&table.name));
        let features = self.dialect.features();
        let inline = features.inline_comments;
        let keep = |comment: &Option<String>| comment.clone().filter(|_| inline && self.comments());
        let bare = TableDef {
            indexes: Vec::new(),
            foreign_keys: if features.inline_foreign_keys {
                table.foreign_keys.clone()
            } else {
                Vec::new()
            },
            comment: keep(&table.comment),
            columns: table
                .columns
                .iter()
                .map(|column| crate::contract::ColumnDef {
                    comment: keep(&column.comment),
                    ..column.clone()
                })
                .collect(),
            ..table.clone()
        };
        self.push(Phase::CreateTables, id, MigrationStep::CreateTable { table: bare });
        for index in &table.indexes {
            self.push(
                Phase::CreateConstraints,
                id,
                MigrationStep::CreateIndex {
                    table: table.name.clone(),
                    index: index.clone(),
                },
            );
        }
        for key in &table.foreign_keys {
            self.add_foreign_key(id, &table.name, key);
        }
        if self.comments() && !inline {
            if let Some(comment) = table.comment.clone().filter(|c| !c.is_empty()) {
                self.set_comment(id, "table", &table.name, None, Some(comment));
            }
            for column in &table.columns {
                if let Some(comment) = column.comment.clone().filter(|c| !c.is_empty()) {
                    self.set_comment(id, "table", &table.name, Some(column), Some(comment));
                }
            }
        }
    }

    fn rebuild(&mut self, item: &DiffItem, source: &TableDef, target: &TableDef) {
        let column_map = source
            .columns
            .iter()
            .filter_map(|s| {
                target
                    .columns
                    .iter()
                    .find(|t| self.fold(&t.name) == self.fold(&s.name))
                    .map(|t| (s.name.clone(), t.name.clone()))
            })
            .collect();
        self.altered_tables.insert(self.fold(&target.name));
        self.key_dropped.insert(self.fold(&target.name));
        let mut source_table = source.clone();
        source_table.name = target.name.clone();
        self.push(
            Phase::AlterColumns,
            &item.id,
            MigrationStep::RebuildTable {
                source_table,
                target_table: target.clone(),
                column_map,
            },
        );
        // Foreign keys of the rebuilt table come back with it; the ones of other tables pointing at it are handled by
        // the dependency pass.
        for key in &target.foreign_keys {
            if let Some(name) = &key.name {
                self.dropped_fks.insert((self.fold(&target.name), self.fold(name)));
            }
        }
    }

    /// `column` is the column as it stands when the comment is set (after the script's earlier steps).
    fn set_comment(&mut self, id: &str, kind: &str, name: &str, column: Option<&crate::contract::ColumnDef>, comment: Option<String>) {
        self.push(
            Phase::Comments,
            id,
            MigrationStep::SetComment {
                target: CommentTarget {
                    kind: kind.to_owned(),
                    name: name.to_owned(),
                    column: column.map(|c| c.name.clone()),
                    definition: column.map(|c| crate::contract::ColumnDef {
                        comment: comment.clone(),
                        ..c.clone()
                    }),
                },
                comment,
            },
        );
    }

    fn table_change(&mut self, id: &str, change: &Change, source: &'a TableDef, target: &'a TableDef) {
        let table = target.name.clone();
        let column = |t: &'a TableDef, name: &str| t.columns.iter().find(|c| self.fold(&c.name) == self.fold(name));
        match (change.part.as_str(), change.what) {
            ("column", ChangeKind::Added) => {
                let Some(def) = column(source, &change.name) else { return };
                let mut def = def.clone();
                let mut comment = def.comment.take().filter(|c| !c.is_empty() && self.comments());
                if self.dialect.features().inline_comments {
                    def.comment = comment.take();
                }
                self.push(
                    Phase::AddColumns,
                    id,
                    MigrationStep::AddColumn {
                        table: table.clone(),
                        column: def.clone(),
                    },
                );
                if let Some(comment) = comment {
                    self.set_comment(id, "table", &table, Some(&def), Some(comment));
                }
            }
            ("column", ChangeKind::Removed) => {
                let Some(def) = column(target, &change.name) else { return };
                self.altered_tables.insert(self.fold(&table));
                if self.dialect.features().columns_held_by_dependents {
                    self.hold(&table, &def.name, true);
                    if def.default.is_some() && def.default_name.is_some() {
                        // The column's default constraint goes first.
                        let bare = crate::contract::ColumnDef {
                            default: None,
                            default_name: None,
                            ..def.clone()
                        };
                        self.push(
                            Phase::AlterColumns,
                            id,
                            MigrationStep::AlterColumn {
                                table: table.clone(),
                                from: def.clone(),
                                to: bare,
                                changes: ColumnChanges {
                                    default: true,
                                    ..ColumnChanges::default()
                                },
                            },
                        );
                    }
                }
                self.push(
                    Phase::DropColumns,
                    id,
                    MigrationStep::DropColumn {
                        table: table.clone(),
                        column: def.name.clone(),
                    },
                );
            }
            ("column", ChangeKind::Modified) => {
                let (Some(to), Some(from)) = (column(source, &change.name), column(target, &change.name)) else {
                    return;
                };
                let has = |field: &str| change.fields.iter().any(|f| f.field == field);
                let changes = ColumnChanges {
                    type_name: has("type"),
                    nullability: has("nullable"),
                    default: has("default"),
                    identity: has("identity"),
                    generated: has("generated"),
                    collation: has("collation"),
                };
                if changes.type_name || changes.collation || changes.generated {
                    self.altered_tables.insert(self.fold(&table));
                    self.type_changed.insert((self.fold(&table), self.fold(&from.name)));
                }
                let held = changes.type_name || changes.collation || changes.generated || changes.nullability;
                if held && self.dialect.features().columns_held_by_dependents {
                    self.type_changed.insert((self.fold(&table), self.fold(&from.name)));
                    self.hold(&table, &from.name, false);
                }
                if changes != ColumnChanges::default() {
                    self.push(
                        Phase::AlterColumns,
                        id,
                        MigrationStep::AlterColumn {
                            table: table.clone(),
                            from: from.clone(),
                            to: to.clone(),
                            changes,
                        },
                    );
                }
                if has("comment") {
                    let comment = to.comment.clone().filter(|c| !c.is_empty());
                    let current = changes.apply(from, to);
                    self.set_comment(id, "table", &table, Some(&current), comment);
                }
            }
            ("columnOrder", _) => self.manual(
                Phase::AlterColumns,
                id,
                format!("The column order of {table} differs; reordering columns needs a table rebuild, which this script does not do"),
            ),
            ("primaryKey", what) => {
                if matches!(what, ChangeKind::Removed | ChangeKind::Modified) {
                    if let Some(key) = &target.primary_key {
                        self.key_dropped.insert(self.fold(&table));
                        self.push(
                            Phase::DropConstraints,
                            id,
                            MigrationStep::DropConstraint {
                                table: table.clone(),
                                kind: ConstraintKind::PrimaryKey,
                                name: key.name.clone(),
                            },
                        );
                    }
                }
                if matches!(what, ChangeKind::Added | ChangeKind::Modified) {
                    if let Some(key) = &source.primary_key {
                        self.push(
                            Phase::CreateConstraints,
                            id,
                            MigrationStep::AddPrimaryKey {
                                table: table.clone(),
                                key: key.clone(),
                            },
                        );
                    }
                }
            }
            ("unique", what) => {
                let found = self.locate(change, "unique", source, target, |t| &t.uniques);
                self.sub_steps(id, change, what, found, |planner, key: &KeyDef, drop| {
                    if drop {
                        planner.key_dropped.insert(planner.fold(&table));
                        MigrationStep::DropConstraint {
                            table: table.clone(),
                            kind: ConstraintKind::Unique,
                            name: key.name.clone(),
                        }
                    } else {
                        MigrationStep::AddConstraint {
                            table: table.clone(),
                            constraint: ConstraintDef::Unique { key: key.clone() },
                        }
                    }
                });
            }
            ("check", what) => {
                let found = self.locate(change, "check", source, target, |t| &t.checks);
                self.sub_steps(id, change, what, found, |_, check: &CheckDef, drop| {
                    if drop {
                        MigrationStep::DropConstraint {
                            table: table.clone(),
                            kind: ConstraintKind::Check,
                            name: check.name.clone(),
                        }
                    } else {
                        MigrationStep::AddConstraint {
                            table: table.clone(),
                            constraint: ConstraintDef::Check { check: check.clone() },
                        }
                    }
                });
            }
            ("index", what) => {
                let found = self.locate(change, "index", source, target, |t| &t.indexes);
                self.sub_steps(id, change, what, found, |_, index: &IndexDef, drop| {
                    if drop {
                        MigrationStep::DropIndex {
                            table: table.clone(),
                            name: index.name.clone(),
                        }
                    } else {
                        MigrationStep::CreateIndex {
                            table: table.clone(),
                            index: index.clone(),
                        }
                    }
                });
            }
            ("foreignKey", what) => {
                let (added, removed) = self.locate(change, "foreignKey", source, target, |t| &t.foreign_keys);
                if matches!(what, ChangeKind::Removed | ChangeKind::Modified) {
                    match removed {
                        Some(key) => self.drop_foreign_key(id, &table, key, None),
                        None => self.not_found(id, change),
                    }
                }
                if matches!(what, ChangeKind::Added | ChangeKind::Modified) {
                    match added {
                        Some(key) => self.add_foreign_key(id, &table, key),
                        None => self.not_found(id, change),
                    }
                }
            }
            ("comment", _) => {
                let comment = source.comment.clone().filter(|c| !c.is_empty());
                self.set_comment(id, "table", &table, None, comment);
            }
            ("option", _) => {
                let value = source
                    .options
                    .iter()
                    .find(|(key, _)| key.eq_ignore_ascii_case(&change.name))
                    .map(|(_, value)| value.clone());
                match value {
                    Some(value) => self.push(
                        Phase::Comments,
                        id,
                        MigrationStep::SetTableOption {
                            table: table.clone(),
                            name: change.name.clone(),
                            value,
                        },
                    ),
                    None => self.manual(
                        Phase::Comments,
                        id,
                        format!("Option {} of {table} is set on the target only; reset it by hand", change.name),
                    ),
                }
            }
            _ => self.not_found(id, change),
        }
    }

    /// The source and target definitions a sub-object change is about (by the labels the diff gave them).
    fn locate<T>(
        &self,
        change: &Change,
        part: &str,
        source: &'a TableDef,
        target: &'a TableDef,
        defs: impl Fn(&'a TableDef) -> &'a Vec<T>,
    ) -> (Option<&'a T>, Option<&'a T>) {
        let find = |table: &'a TableDef, is_source: bool, label: &str| -> Option<&'a T> {
            let labels = self.labels.of(part, table, is_source);
            labels
                .iter()
                .position(|l| self.fold(l) == self.fold(label))
                .map(|at| &defs(table)[at])
        };
        let target_label = change
            .fields
            .iter()
            .find(|f| f.field == "name")
            .and_then(|f| f.target.clone())
            .unwrap_or_else(|| change.name.clone());
        let added = match change.what {
            ChangeKind::Removed => None,
            _ => find(source, true, &change.name),
        };
        let removed = match change.what {
            ChangeKind::Added => None,
            ChangeKind::Removed => find(target, false, &change.name),
            ChangeKind::Modified => find(target, false, &target_label),
        };
        (added, removed)
    }

    /// Drop (`removed`) and create (`added`) steps of a sub-object change.
    fn sub_steps<T>(
        &mut self,
        id: &str,
        change: &Change,
        what: ChangeKind,
        (added, removed): (Option<&T>, Option<&T>),
        mut step: impl FnMut(&mut Self, &T, bool) -> MigrationStep,
    ) {
        if matches!(what, ChangeKind::Removed | ChangeKind::Modified) {
            match removed {
                Some(def) => {
                    let drop = step(self, def, true);
                    let phase = Phase::DropConstraints;
                    self.push(phase, id, drop);
                }
                None => self.not_found(id, change),
            }
        }
        if matches!(what, ChangeKind::Added | ChangeKind::Modified) {
            match added {
                Some(def) => {
                    let create = step(self, def, false);
                    self.push(Phase::CreateConstraints, id, create);
                }
                None => self.not_found(id, change),
            }
        }
    }

    fn not_found(&mut self, id: &str, change: &Change) {
        self.manual(
            Phase::Comments,
            id,
            format!("Could not script the {} change of {}; apply it by hand", change.part, change.name),
        );
    }

    fn drop_foreign_key(&mut self, id: &str, table: &str, key: &ForeignKeyDef, note: Option<String>) {
        let Some(name) = &key.name else {
            self.manual(
                Phase::DropForeignKeys,
                id,
                format!("A foreign key of {table} has no name; drop it by hand"),
            );
            return;
        };
        if !self.dropped_fks.insert((self.fold(table), self.fold(name))) {
            return;
        }
        self.push_noted(
            Phase::DropForeignKeys,
            id,
            MigrationStep::DropForeignKey {
                table: table.to_owned(),
                name: name.clone(),
            },
            note,
        );
    }

    fn add_foreign_key(&mut self, id: &str, table: &str, key: &ForeignKeyDef) {
        if self.in_scope(key, self.source.scope.schema.as_deref()) {
            let referenced = self.fold(&key.ref_table);
            let exists = (self.target_table(&key.ref_table).is_some() && !self.dropped_tables.contains(&referenced))
                || self.created_tables.contains(&referenced)
                || self.fold(table) == referenced;
            let will_exist = exists || {
                let ref_id = format!(
                    "table:{}",
                    self.source_table(&key.ref_table)
                        .map_or(key.ref_table.as_str(), |t| t.name.as_str())
                );
                self.selected.contains(ref_id.as_str()) && self.items.contains_key(ref_id.as_str())
            };
            if !will_exist {
                self.warn(
                    id,
                    Risk::Review,
                    format!(
                        "Foreign key {} on {table} needs table {}, which is not selected",
                        key.name.as_deref().unwrap_or("(unnamed)"),
                        key.ref_table
                    ),
                );
            }
        }
        if self.dialect.features().inline_foreign_keys {
            // Part of the table's CREATE TABLE (or rebuild).
            return;
        }
        self.push(
            Phase::AddForeignKeys,
            id,
            MigrationStep::AddForeignKey {
                table: table.to_owned(),
                key: key.clone(),
            },
        );
    }

    /// Foreign keys of the target in the way of the planned drops and type changes: dropped first, added back after.
    fn foreign_key_dependencies(&mut self) {
        if self.dialect.features().inline_foreign_keys {
            // Keys refer to tables by name, so they survive a rebuild; a dropped parent leaves them dangling, as the
            // source has them.
            return;
        }
        let target = self.target;
        let target_schema = target.scope.schema.clone();
        for table in &target.tables {
            let table_key = self.fold(&table.name);
            let table_dropped = self.dropped_tables.contains(&table_key);
            let id = self.item_id("table", &table.name);
            for key in &table.foreign_keys {
                if !self.in_scope(key, target_schema.as_deref()) {
                    continue;
                }
                let referenced = self.fold(&key.ref_table);
                let already = key
                    .name
                    .as_ref()
                    .is_some_and(|name| self.dropped_fks.contains(&(table_key.clone(), self.fold(name))));
                if already {
                    continue;
                }
                if self.dropped_tables.contains(&referenced) {
                    if referenced != table_key {
                        let note = format!("It references {}, which is dropped", key.ref_table);
                        self.drop_foreign_key(&id, &table.name, key, Some(note));
                    }
                    continue;
                }
                if table_dropped {
                    continue;
                }
                let columns_changed = key
                    .columns
                    .iter()
                    .any(|c| self.type_changed.contains(&(table_key.clone(), self.fold(c))));
                let references_changed = key
                    .ref_columns
                    .iter()
                    .any(|c| self.type_changed.contains(&(referenced.clone(), self.fold(c))));
                let reason = if columns_changed || references_changed {
                    Some("a column it uses changes type".to_owned())
                } else if self.key_dropped.contains(&referenced) {
                    Some(format!("a key of {} it may reference is recreated", key.ref_table))
                } else {
                    None
                };
                if let Some(reason) = reason {
                    let note = format!("Dropped and added back because {reason}");
                    self.drop_foreign_key(&id, &table.name, key, Some(note.clone()));
                    self.push_noted(
                        Phase::AddForeignKeys,
                        &id,
                        MigrationStep::AddForeignKey {
                            table: table.name.clone(),
                            key: key.clone(),
                        },
                        Some(note),
                    );
                }
            }
        }
    }

    /// Notes a column the plan alters (or drops) where dependents hold columns.
    fn hold(&mut self, table: &str, column: &str, dropped: bool) {
        let (table, column) = (self.fold(table), self.fold(column));
        let (altered, gone) = self.held.entry(table).or_default();
        altered.insert(column.clone());
        if dropped {
            gone.insert(column);
        }
    }

    /// Where dependents hold columns (SQL Server): the target's keys, checks and indexes over a column the plan alters
    /// or drops are dropped first, and the ones over altered columns are created again afterwards (unless a change
    /// already drops them). Dropping a key also drops and re-adds the foreign keys that may reference it.
    fn held_dependents(&mut self) {
        let held = std::mem::take(&mut self.held);
        let mut tables: Vec<_> = held.into_iter().collect();
        tables.sort_by(|a, b| a.0.cmp(&b.0));
        for (folded, (mut altered, gone)) in tables {
            let Some(table) = self.target_table(&folded) else { continue };
            let id = self.item_id("table", &table.name);
            let name = table.name.clone();
            let note = Some(format!("Dropped and created again because a column it uses changes ({name})"));
            let computed_drops = self.held_computed_columns(&id, table, &mut altered, &gone);
            // Schema-bound views of the scope over the table hold its columns too: they are dropped and recreated.
            let schema_bound = self
                .target
                .views
                .iter()
                .any(|view| view.option("schemabinding").is_some() && view.depends_on.iter().any(|dep| self.fold(dep) == folded));
            if schema_bound {
                self.altered_tables.insert(folded.clone());
            }
            self.warn(
                &id,
                Risk::Review,
                format!(
                    "Objects WITH SCHEMABINDING outside the compared schema (views, functions) that use {name} block \
                     changing its columns, and the compare does not see them; check sys.sql_expression_dependencies \
                     before running"
                ),
            );
            let fold_all = |planner: &Self, columns: &[String]| columns.iter().map(|c| planner.fold(c)).collect::<Vec<_>>();
            let uses = |columns: &[String]| columns.iter().any(|c| altered.contains(c));
            let keeps = |columns: &[String]| !columns.iter().any(|c| gone.contains(c));
            let free = |planner: &Self, part: &'static str, sub: &Option<String>| {
                sub.as_ref()
                    .is_none_or(|n| !planner.dropped_subs.contains(&(folded.clone(), part, planner.fold(n))))
            };
            if let Some(key) = &table.primary_key {
                let columns = fold_all(self, &key.columns);
                if uses(&columns) && key.name.is_some() && free(self, "primaryKey", &key.name) {
                    self.key_dropped.insert(folded.clone());
                    let drop = MigrationStep::DropConstraint {
                        table: name.clone(),
                        kind: ConstraintKind::PrimaryKey,
                        name: key.name.clone(),
                    };
                    self.push_noted(Phase::DropConstraints, &id, drop, note.clone());
                    if keeps(&columns) {
                        let add = MigrationStep::AddPrimaryKey {
                            table: name.clone(),
                            key: key.clone(),
                        };
                        self.push_noted(Phase::CreateConstraints, &id, add, note.clone());
                    }
                }
            }
            for key in &table.uniques {
                let columns = fold_all(self, &key.columns);
                if uses(&columns) && key.name.is_some() && free(self, "unique", &key.name) {
                    self.key_dropped.insert(folded.clone());
                    let drop = MigrationStep::DropConstraint {
                        table: name.clone(),
                        kind: ConstraintKind::Unique,
                        name: key.name.clone(),
                    };
                    self.push_noted(Phase::DropConstraints, &id, drop, note.clone());
                    if keeps(&columns) {
                        let add = MigrationStep::AddConstraint {
                            table: name.clone(),
                            constraint: ConstraintDef::Unique { key: key.clone() },
                        };
                        self.push_noted(Phase::CreateConstraints, &id, add, note.clone());
                    }
                }
            }
            for check in &table.checks {
                let columns: Vec<String> = mentioned(&check.expression).into_iter().map(|c| self.fold(&c)).collect();
                if uses(&columns) && check.name.is_some() && free(self, "check", &check.name) {
                    let drop = MigrationStep::DropConstraint {
                        table: name.clone(),
                        kind: ConstraintKind::Check,
                        name: check.name.clone(),
                    };
                    self.push_noted(Phase::DropConstraints, &id, drop, note.clone());
                    if keeps(&columns) {
                        let add = MigrationStep::AddConstraint {
                            table: name.clone(),
                            constraint: ConstraintDef::Check { check: check.clone() },
                        };
                        self.push_noted(Phase::CreateConstraints, &id, add, note.clone());
                    }
                }
            }
            for index in &table.indexes {
                let mut columns: Vec<String> = index.include.clone();
                for part in &index.parts {
                    columns.extend(part.column.clone());
                    columns.extend(part.expression.as_deref().map(mentioned).unwrap_or_default());
                }
                columns.extend(index.predicate.as_deref().map(mentioned).unwrap_or_default());
                let columns = fold_all(self, &columns);
                if uses(&columns) && free(self, "index", &Some(index.name.clone())) {
                    let drop = MigrationStep::DropIndex {
                        table: name.clone(),
                        name: index.name.clone(),
                    };
                    self.push_noted(Phase::DropConstraints, &id, drop, note.clone());
                    if keeps(&columns) {
                        let add = MigrationStep::CreateIndex {
                            table: name.clone(),
                            index: index.clone(),
                        };
                        self.push_noted(Phase::CreateConstraints, &id, add, note.clone());
                    }
                }
            }
            // After the keys, checks and indexes over them.
            for (drop, note) in computed_drops {
                self.push_noted(Phase::DropConstraints, &id, drop, note);
            }
        }
    }

    /// Computed columns of `table` over a column the plan alters or drops hold it (SQL Server): one the plan leaves alone
    /// is dropped (the returned steps, after its indexes) and added again afterwards (at the end of the column order), and counts as altered so its own
    /// keys and indexes are held too; one the plan changes itself, or that reads a dropped column, is a manual step.
    fn held_computed_columns(
        &mut self,
        id: &str,
        table: &TableDef,
        altered: &mut HashSet<String>,
        gone: &HashSet<String>,
    ) -> Vec<(MigrationStep, Option<String>)> {
        let name = table.name.clone();
        let mut found = Vec::new();
        let mut drops = Vec::new();
        for column in &table.columns {
            let Some(generated) = &column.generated else { continue };
            let folded = self.fold(&column.name);
            let reads: Vec<String> = mentioned(&generated.expression)
                .into_iter()
                .map(|c| self.fold(&c))
                .filter(|c| *c != folded && altered.contains(c))
                .collect();
            if !reads.is_empty() {
                found.push((column, folded, reads));
            }
        }
        for (column, folded, reads) in found {
            let touched = altered.contains(&folded);
            if touched || reads.iter().any(|c| gone.contains(c)) {
                self.manual(
                    Phase::DropConstraints,
                    id,
                    format!(
                        "Computed column {name}.{} uses a column this script changes or drops; drop it before running the \
                         script and add it again afterwards as the source defines it",
                        column.name
                    ),
                );
                continue;
            }
            altered.insert(folded);
            let note = Some(format!(
                "Dropped and added again because a column it uses changes ({name}); it moves to the end of the column order"
            ));
            let drop = MigrationStep::DropColumn {
                table: name.clone(),
                column: column.name.clone(),
            };
            drops.push((drop, note.clone()));
            let add = MigrationStep::AddColumn {
                table: name.clone(),
                column: column.clone(),
            };
            self.push_noted(Phase::CreateConstraints, id, add, note);
        }
        drops
    }

    // --- Views --------------------------------------------------------------------------------------------------------

    fn item_id(&self, kind: &str, name: &str) -> String {
        let folded = self.fold(name);
        self.items
            .values()
            .find(|item| item.kind == kind && self.fold(&item.name) == folded)
            .map_or_else(|| format!("{kind}:{name}"), |item| item.id.clone())
    }

    fn view(&mut self, item: &'a DiffItem) {
        let id = item.id.clone();
        match item.status {
            DiffStatus::OnlyInSource => {
                let Some(view) = self.source_view(&item.name) else { return };
                self.view_creates.push((id.clone(), view.clone(), None));
                if self.comments() {
                    if let Some(comment) = view.comment.clone().filter(|c| !c.is_empty()) {
                        self.set_comment(&id, view_kind(view), &view.name, None, Some(comment));
                    }
                }
            }
            DiffStatus::OnlyInTarget => {
                let Some(view) = self.target_view(&item.name) else { return };
                self.view_drops.push((id, view.clone(), None));
            }
            DiffStatus::Different => {
                let (Some(source), Some(target)) = (self.source_view(&item.name), self.target_view(&item.name)) else {
                    return;
                };
                let redefined = item.changes.iter().any(|c| c.part == "definition");
                if redefined {
                    self.view_drops.push((id.clone(), target.clone(), None));
                    self.view_creates
                        .push((id.clone(), source.clone(), Some(RECREATED_VIEW_LOSES.to_owned())));
                }
                let commented = item.changes.iter().any(|c| c.part == "comment");
                if self.comments() && (commented || (redefined && source.comment.as_deref().is_some_and(|c| !c.is_empty()))) {
                    let comment = source.comment.clone().filter(|c| !c.is_empty());
                    self.set_comment(&id, view_kind(source), &target.name, None, comment);
                }
            }
            DiffStatus::Identical => {}
        }
    }

    /// Target views reading an altered table or a dropped view are dropped and recreated (transitively); views reading
    /// a dropped table are reported.
    fn view_dependencies(&mut self) {
        let target = self.target;
        // Views whose definition the target does not show are never dropped (the script could not recreate them).
        let mut unreadable: HashSet<String> = HashSet::new();
        loop {
            let dropped: HashSet<String> = self.view_drops.iter().map(|(_, v, _)| self.fold(&v.name)).collect();
            let mut found = Vec::new();
            for view in &target.views {
                let folded = self.fold(&view.name);
                if dropped.contains(&folded) || unreadable.contains(&folded) {
                    continue;
                }
                let cause = view.depends_on.iter().find(|dep| {
                    let dep = self.fold(dep);
                    self.altered_tables.contains(&dep) || dropped.contains(&dep)
                });
                if let Some(cause) = cause {
                    found.push((view, cause.clone()));
                }
            }
            if found.is_empty() {
                break;
            }
            for (view, cause) in found {
                let id = self.item_id("view", &view.name);
                if view.unreadable {
                    unreadable.insert(self.fold(&view.name));
                    self.manual(
                        Phase::DropViews,
                        &id,
                        format!(
                            "View {} depends on {cause}, which changes, but its definition is not readable, so the script \
                             neither drops nor recreates it; drop it before this script and recreate it afterwards by hand",
                            view.name
                        ),
                    );
                    continue;
                }
                let note = format!("Recreated because it depends on {cause}, which changes");
                self.view_drops.push((id.clone(), view.clone(), Some(note.clone())));
                let note = format!("{note}. {RECREATED_VIEW_LOSES}");
                let created = self
                    .view_creates
                    .iter()
                    .any(|(_, v, _)| self.fold(&v.name) == self.fold(&view.name));
                if created {
                    continue;
                }
                // The source's definition when the user wants this view synced (or it is identical), else the target's own.
                let wanted = self.items.get(id.as_str()).is_some_and(|item| {
                    item.status == DiffStatus::Identical || (self.selected.contains(id.as_str()) && item.status == DiffStatus::Different)
                });
                let definition = match (wanted, self.source_view(&view.name)) {
                    (true, Some(source)) => ViewDef {
                        name: view.name.clone(),
                        ..source.clone()
                    },
                    _ => view.clone(),
                };
                self.view_creates.push((id, definition, Some(note)));
            }
        }
        let dropped_views: HashSet<String> = self.view_drops.iter().map(|(_, v, _)| self.fold(&v.name)).collect();
        let mut messages = Vec::new();
        if !self.altered_tables.is_empty() || !self.dropped_tables.is_empty() {
            for view in target.views.iter().filter(|v| v.unreadable && v.depends_on.is_empty()) {
                messages.push((
                    self.item_id("view", &view.name),
                    format!(
                        "The definition of view {} is not readable, so whether it reads a table this script changes is \
                         unknown; check it by hand",
                        view.name
                    ),
                ));
            }
        }
        for view in &target.views {
            if dropped_views.contains(&self.fold(&view.name)) {
                continue;
            }
            if let Some(table) = view.depends_on.iter().find(|dep| self.dropped_tables.contains(&self.fold(dep))) {
                messages.push((
                    self.item_id("view", &view.name),
                    format!("View {} reads table {table}, which is dropped; select the view too", view.name),
                ));
            }
        }
        for (id, message) in messages {
            self.warn(&id, Risk::Review, message);
        }
    }

    /// Drops dependents first, creates dependencies first; a cycle is reported and broken.
    fn order_views(&mut self) {
        let drops = std::mem::take(&mut self.view_drops);
        let creates = std::mem::take(&mut self.view_creates);
        let mut drop_order = self.topological(&drops);
        drop_order.reverse();
        for at in drop_order {
            let (id, view, note) = &drops[at];
            let step = MigrationStep::DropView {
                name: view.name.clone(),
                materialized: view.materialized,
            };
            self.push_noted(Phase::DropViews, id, step, note.clone());
        }
        for at in self.topological(&creates) {
            let (id, view, note) = &creates[at];
            let step = MigrationStep::CreateView {
                view: view.clone(),
                replace: false,
            };
            self.push_noted(Phase::CreateViews, id, step, note.clone());
        }
    }

    /// Indexes of `views` with every view after the views of the list it depends on.
    fn topological(&mut self, views: &[(String, ViewDef, Option<String>)]) -> Vec<usize> {
        let position: HashMap<String, usize> = views.iter().enumerate().map(|(i, (_, v, _))| (self.fold(&v.name), i)).collect();
        let mut order = Vec::with_capacity(views.len());
        let mut state = vec![0u8; views.len()]; // 0 new, 1 visiting, 2 done
        let mut cycles = Vec::new();
        for start in 0..views.len() {
            self.visit(start, views, &position, &mut state, &mut order, &mut cycles);
        }
        for at in cycles {
            let (id, view, _) = &views[at];
            self.warn(
                id,
                Risk::Review,
                format!(
                    "Views around {} depend on each other; check the order of their statements",
                    view.name
                ),
            );
        }
        order
    }

    fn visit(
        &self,
        at: usize,
        views: &[(String, ViewDef, Option<String>)],
        position: &HashMap<String, usize>,
        state: &mut [u8],
        order: &mut Vec<usize>,
        cycles: &mut Vec<usize>,
    ) {
        match state[at] {
            2 => return,
            1 => {
                cycles.push(at);
                return;
            }
            _ => {}
        }
        state[at] = 1;
        for dep in &views[at].1.depends_on {
            if let Some(&next) = position.get(&self.fold(dep)) {
                if next != at {
                    self.visit(next, views, position, state, order, cycles);
                }
            }
        }
        state[at] = 2;
        order.push(at);
    }
}

/// What a script that drops and recreates a view must tell the user.
const RECREATED_VIEW_LOSES: &str = "Dropping the view also drops its grants, its indexes (materialized or indexed views) and its \
                                    INSTEAD OF triggers; re-apply them after the script";

/// The diff's part name of a constraint kind.
fn constraint_part(kind: ConstraintKind) -> &'static str {
    match kind {
        ConstraintKind::PrimaryKey => "primaryKey",
        ConstraintKind::Unique => "unique",
        ConstraintKind::Check => "check",
    }
}

/// The identifiers an expression mentions (bare, `[bracketed]`, `"quoted"` or `` `quoted` ``), outside string
/// literals: the columns a check or an index expression may use.
pub(super) fn mentioned(expression: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut chars = expression.char_indices().peekable();
    while let Some((at, c)) = chars.next() {
        let close = match c {
            '\'' => {
                // A literal; a doubled quote is an escape.
                while let Some((_, c)) = chars.next() {
                    if c == '\'' && chars.next_if(|(_, next)| *next == '\'').is_none() {
                        break;
                    }
                }
                continue;
            }
            '[' => ']',
            '"' | '`' => c,
            c if c.is_alphabetic() || c == '_' => {
                let mut end = at + c.len_utf8();
                while let Some((next_at, next)) = chars.next_if(|(_, n)| n.is_alphanumeric() || matches!(n, '_' | '$' | '#' | '@')) {
                    end = next_at + next.len_utf8();
                }
                names.push(expression[at..end].to_owned());
                continue;
            }
            _ => continue,
        };
        let mut name = String::new();
        while let Some((_, c)) = chars.next() {
            // A doubled closing quote is an escaped one.
            if c == close && chars.next_if(|(_, next)| *next == close).is_none() {
                break;
            }
            name.push(c);
        }
        names.push(name);
    }
    names
}

/// What a comment on a view is on (`COMMENT ON MATERIALIZED VIEW` differs).
fn view_kind(view: &ViewDef) -> &'static str {
    if view.materialized {
        "materializedView"
    } else {
        "view"
    }
}
