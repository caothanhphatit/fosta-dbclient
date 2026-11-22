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
//! back); what the planner cannot satisfy becomes a warning, never a silent omission.

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
                if self.dialect.needs_rebuild(item) {
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
        let bare = TableDef {
            indexes: Vec::new(),
            foreign_keys: Vec::new(),
            comment: None,
            columns: table
                .columns
                .iter()
                .map(|column| crate::contract::ColumnDef {
                    comment: None,
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
        if self.comments() {
            if let Some(comment) = table.comment.clone().filter(|c| !c.is_empty()) {
                self.set_comment(id, "table", &table.name, None, Some(comment));
            }
            for column in &table.columns {
                if let Some(comment) = column.comment.clone().filter(|c| !c.is_empty()) {
                    self.set_comment(id, "table", &table.name, Some(&column.name), Some(comment));
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

    fn set_comment(&mut self, id: &str, kind: &str, name: &str, column: Option<&str>, comment: Option<String>) {
        self.push(
            Phase::Comments,
            id,
            MigrationStep::SetComment {
                target: CommentTarget {
                    kind: kind.to_owned(),
                    name: name.to_owned(),
                    column: column.map(str::to_owned),
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
                let comment = def.comment.take().filter(|c| !c.is_empty() && self.comments());
                self.push(
                    Phase::AddColumns,
                    id,
                    MigrationStep::AddColumn {
                        table: table.clone(),
                        column: def,
                    },
                );
                if let Some(comment) = comment {
                    self.set_comment(id, "table", &table, Some(&change.name), Some(comment));
                }
            }
            ("column", ChangeKind::Removed) => {
                let Some(def) = column(target, &change.name) else { return };
                self.altered_tables.insert(self.fold(&table));
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
                    self.set_comment(id, "table", &table, Some(&from.name), comment);
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
                    self.view_creates.push((id.clone(), source.clone(), None));
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
        loop {
            let dropped: HashSet<String> = self.view_drops.iter().map(|(_, v, _)| self.fold(&v.name)).collect();
            let mut found = Vec::new();
            for view in &target.views {
                if dropped.contains(&self.fold(&view.name)) {
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
                let note = format!("Recreated because it depends on {cause}, which changes");
                self.view_drops.push((id.clone(), view.clone(), Some(note.clone())));
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

/// What a comment on a view is on (`COMMENT ON MATERIALIZED VIEW` differs).
fn view_kind(view: &ViewDef) -> &'static str {
    if view.materialized {
        "materializedView"
    } else {
        "view"
    }
}
