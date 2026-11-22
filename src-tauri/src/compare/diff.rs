//! The diff of two snapshots: "what would make the target match the source".
//!
//! 1. Tables and views match by name (case-folded when names are case-insensitive); excluded names are left out and
//!    listed as skipped.
//! 2. Inside a matched table, columns match by name and the primary key is one item. Uniques, checks, indexes and
//!    foreign keys match by name when both sides are user-named (and names are not ignored), otherwise by their
//!    normalized definition, so server-chosen names (`PK__orders__3213E83F`) never show as differences.
//! 3. Attributes compare after the dialect's normalizers, with each side's scope schema dropped from references, so
//!    `compare_a` and `compare_b` compare like for like.
//! 4. A lone dropped column next to a lone added one of the same type and nullability is flagged as a possible rename
//!    (a hint only: the script still drops and adds).

use std::collections::HashSet;

use super::options::ResolvedOptions;
use crate::contract::{
    Change, ChangeKind, ColumnDef, CompareOptions, DialectFeatures, DiffItem, DiffStatus, FieldChange, ForeignKeyDef, IdentityKind,
    IndexDef, KeyDef, Risk, SchemaDialect, SchemaDiff, SchemaSnapshot, ScopeSummary, SkippedObject, TableDef, ViewDef,
};

/// Diffs `source` against `target`. The dialect is the target's.
pub fn diff(source: &SchemaSnapshot, target: &SchemaSnapshot, options: &CompareOptions, dialect: &dyn SchemaDialect) -> SchemaDiff {
    let cx = Cx::new(source, target, options, dialect);
    let mut skipped = Skipped::default();
    for object in source.skipped.iter().chain(&target.skipped) {
        skipped.push(object.clone());
    }
    let mut items = Vec::new();
    if cx.options.includes("table") {
        let pairs = cx.pair_objects("table", &source.tables, &target.tables, |t| &t.name, &mut skipped);
        items.extend(pairs.into_iter().map(|(s, t)| cx.table_item(s, t)));
    }
    if cx.options.includes("view") {
        let pairs = cx.pair_objects("view", &source.views, &target.views, |v| &v.name, &mut skipped);
        items.extend(pairs.into_iter().map(|(s, t)| cx.view_item(s, t)));
    }
    items.sort_by_cached_key(|item| (u8::from(item.kind != "table"), cx.options.fold(&item.name), item.name.clone()));
    SchemaDiff {
        source: summary(source),
        target: summary(target),
        options: options.clone(),
        items,
        skipped: skipped.list,
    }
}

fn summary(snapshot: &SchemaSnapshot) -> ScopeSummary {
    ScopeSummary {
        engine: snapshot.engine.clone(),
        server_version: snapshot.server_version.clone(),
        database: snapshot.scope.database.clone(),
        schema: snapshot.scope.schema.clone(),
    }
}

/// Skipped objects without duplicates, in first-seen order.
#[derive(Default)]
struct Skipped {
    seen: HashSet<SkippedObject>,
    list: Vec<SkippedObject>,
}

impl Skipped {
    fn push(&mut self, object: SkippedObject) {
        if self.seen.insert(object.clone()) {
            self.list.push(object);
        }
    }
}

#[derive(Clone, Copy)]
enum Side {
    Source,
    Target,
}

/// A unique, check, index, foreign key or primary key reduced to comparable fields.
struct Sub {
    name: Option<String>,
    /// Named by the user, so its name may matter.
    user_named: bool,
    /// (field, normalized value); equal fields = same definition.
    fields: Vec<(&'static str, String)>,
}

impl Sub {
    fn label(&self) -> String {
        self.name
            .clone()
            .unwrap_or_else(|| self.fields.first().map(|(_, value)| format!("({value})")).unwrap_or_default())
    }
}

/// How risky adding, removing and modifying a kind of sub-object is.
#[derive(Clone, Copy)]
struct SubRisks {
    added: Risk,
    removed: Risk,
    modified: Risk,
    note: Option<&'static str>,
}

const CONSTRAINT_RISKS: SubRisks = SubRisks {
    added: Risk::Review,
    removed: Risk::None,
    modified: Risk::Review,
    note: Some("May fail on existing data"),
};
const INDEX_RISKS: SubRisks = SubRisks {
    added: Risk::None,
    removed: Risk::None,
    modified: Risk::None,
    note: None,
};
const PRIMARY_KEY_RISKS: SubRisks = SubRisks {
    added: Risk::Review,
    removed: Risk::Review,
    modified: Risk::Review,
    note: Some("Changing the primary key may fail on existing data and affects foreign keys"),
};

struct Cx<'a> {
    options: ResolvedOptions<'a>,
    dialect: &'a dyn SchemaDialect,
    features: DialectFeatures,
    source_schema: Option<&'a str>,
    target_schema: Option<&'a str>,
}

/// The names [`Change`]s give the sub-objects of a table (`Change::name`), so the planner can find the definitions a
/// change is about: the user name, or the normalized definition in parentheses for unnamed ones.
pub(super) struct Labels<'a>(Cx<'a>);

impl<'a> Labels<'a> {
    pub(super) fn new(
        source: &'a SchemaSnapshot,
        target: &'a SchemaSnapshot,
        options: &'a CompareOptions,
        dialect: &'a dyn SchemaDialect,
    ) -> Self {
        Self(Cx::new(source, target, options, dialect))
    }

    /// The label of each sub-object of `part` in `table` (of the source side when `source`), in the table's order.
    pub(super) fn of(&self, part: &str, table: &TableDef, source: bool) -> Vec<String> {
        let side = if source { Side::Source } else { Side::Target };
        self.0.subs(part, table, side).iter().map(Sub::label).collect()
    }
}

impl<'a> Cx<'a> {
    fn new(source: &'a SchemaSnapshot, target: &'a SchemaSnapshot, options: &'a CompareOptions, dialect: &'a dyn SchemaDialect) -> Self {
        Cx {
            options: ResolvedOptions::new(options, source, target),
            dialect,
            features: dialect.features(),
            source_schema: source.scope.schema.as_deref(),
            target_schema: target.scope.schema.as_deref(),
        }
    }
}

impl Cx<'_> {
    fn schema(&self, side: Side) -> Option<&str> {
        match side {
            Side::Source => self.source_schema,
            Side::Target => self.target_schema,
        }
    }

    fn ty(&self, raw: &str, side: Side) -> String {
        self.dialect.normalize_type(raw, self.schema(side))
    }

    fn expr(&self, raw: &str, side: Side) -> String {
        self.dialect.normalize_expression(raw, self.schema(side))
    }

    fn names_equal(&self, a: &str, b: &str) -> bool {
        self.options.fold(a) == self.options.fold(b)
    }

    fn columns_text(&self, columns: &[String]) -> String {
        columns.iter().map(|c| self.options.fold(c)).collect::<Vec<_>>().join(", ")
    }

    /// Objects of one kind paired by name: source order, then the target-only ones in target order. Excluded names are
    /// recorded as skipped.
    fn pair_objects<'t, T>(
        &self,
        kind: &str,
        source: &'t [T],
        target: &'t [T],
        name: impl Fn(&T) -> &String,
        skipped: &mut Skipped,
    ) -> Vec<(Option<&'t T>, Option<&'t T>)> {
        let mut keep = |object: &T| {
            let name = name(object);
            if self.options.excluded(name) {
                skipped.push(SkippedObject {
                    kind: kind.to_owned(),
                    name: name.clone(),
                    reason: "Excluded by the compare options".into(),
                });
                false
            } else {
                true
            }
        };
        let source: Vec<&T> = source.iter().filter(|o| keep(o)).collect();
        let target: Vec<&T> = target.iter().filter(|o| keep(o)).collect();
        self.pair_by_name(&source, &target, |o| name(o))
    }

    fn pair_by_name<'t, T>(&self, source: &[&'t T], target: &[&'t T], name: impl Fn(&T) -> &String) -> Vec<(Option<&'t T>, Option<&'t T>)> {
        let mut used = vec![false; target.len()];
        let mut pairs = Vec::new();
        for s in source {
            let key = self.options.fold(name(s));
            let found = target
                .iter()
                .enumerate()
                .position(|(i, t)| !used[i] && self.options.fold(name(t)) == key);
            if let Some(i) = found {
                used[i] = true;
            }
            pairs.push((Some(*s), found.map(|i| target[i])));
        }
        pairs.extend(target.iter().enumerate().filter(|(i, _)| !used[*i]).map(|(_, t)| (None, Some(*t))));
        pairs
    }

    // --- Tables ---------------------------------------------------------------------------------------------------------

    fn table_item(&self, source: Option<&TableDef>, target: Option<&TableDef>) -> DiffItem {
        let name = source.or(target).map(|t| t.name.clone()).unwrap_or_default();
        let (status, changes, risk) = match (source, target) {
            (Some(s), Some(t)) => {
                let changes = self.table_changes(s, t);
                let risk = max_risk(&changes);
                let status = if changes.is_empty() {
                    DiffStatus::Identical
                } else {
                    DiffStatus::Different
                };
                (status, changes, risk)
            }
            (Some(_), None) => (DiffStatus::OnlyInSource, Vec::new(), Risk::None),
            _ => (DiffStatus::OnlyInTarget, Vec::new(), Risk::DataLoss),
        };
        let ddl = |table: &TableDef, like: Option<&TableDef>| self.dialect.table_ddl(self.target_schema, &self.display_table(table, like));
        DiffItem {
            id: format!("table:{name}"),
            kind: "table".into(),
            name,
            status,
            changes,
            risk,
            source_ddl: source.map(|s| ddl(s, None)),
            target_ddl: target.map(|t| ddl(t, source)),
        }
    }

    /// The table as the side-by-side DDL shows it: ignored attributes left out, and the columns in the order of `like`
    /// (the source) when column order is ignored.
    fn display_table(&self, table: &TableDef, like: Option<&TableDef>) -> TableDef {
        let options = self.options.options;
        let mut table = table.clone();
        if options.ignore_comments {
            table.comment = None;
        }
        for column in &mut table.columns {
            if options.ignore_comments {
                column.comment = None;
            }
            if options.ignore_collation {
                column.collation = None;
            }
            if options.ignore_defaults {
                column.default = None;
                column.default_name = None;
            }
        }
        if let (true, Some(like)) = (options.ignore_column_order, like) {
            table.columns.sort_by_cached_key(|column| {
                like.columns
                    .iter()
                    .position(|other| self.names_equal(&other.name, &column.name))
                    .unwrap_or(usize::MAX)
            });
        }
        table
    }

    fn table_changes(&self, source: &TableDef, target: &TableDef) -> Vec<Change> {
        let options = self.options.options;
        let mut changes = self.column_changes(source, target);
        if !options.ignore_column_order {
            changes.extend(self.column_order_change(source, target));
        }
        let ignore_names = options.ignore_constraint_names;
        let subs = |part: &str| (self.subs(part, source, Side::Source), self.subs(part, target, Side::Target));
        let (pk_source, pk_target) = subs("primaryKey");
        changes.extend(self.primary_key_change(pk_source, pk_target, ignore_names));
        for (part, ignore, risks) in [
            ("unique", ignore_names, CONSTRAINT_RISKS),
            ("check", ignore_names, CONSTRAINT_RISKS),
            ("index", options.ignore_index_names, INDEX_RISKS),
            ("foreignKey", ignore_names, CONSTRAINT_RISKS),
        ] {
            let (s, t) = subs(part);
            changes.extend(self.match_subs(part, ignore, s, t, risks));
        }
        if !options.ignore_comments {
            changes.extend(comment_change(&source.name, source.comment.as_deref(), target.comment.as_deref()));
        }
        changes.extend(self.option_changes(source, target));
        changes
    }

    fn column_fields(&self, column: &ColumnDef, side: Side) -> Vec<(&'static str, Option<String>)> {
        let options = self.options.options;
        let serial = column
            .identity
            .as_ref()
            .is_some_and(|identity| identity.kind == IdentityKind::Serial);
        let mut fields = vec![
            ("type", Some(self.ty(&column.type_name, side))),
            ("nullable", Some(if column.nullable { "NULL" } else { "NOT NULL" }.to_owned())),
        ];
        if !options.ignore_defaults && !serial {
            let default = column
                .default
                .as_deref()
                .map(|raw| self.dialect.normalize_default(raw, self.schema(side)));
            fields.push(("default", default.filter(|d| !d.is_empty())));
        }
        fields.push((
            "identity",
            column.identity.as_ref().map(|identity| identity_text(identity.kind).to_owned()),
        ));
        fields.push((
            "generated",
            column.generated.as_ref().map(|generated| {
                let kind = if generated.stored { "stored" } else { "virtual" };
                format!("{} ({kind})", self.expr(&generated.expression, side))
            }),
        ));
        if !options.ignore_collation {
            fields.push(("collation", column.collation.clone()));
        }
        if !options.ignore_comments {
            fields.push(("comment", column.comment.clone().filter(|c| !c.is_empty())));
        }
        fields
    }

    fn column_changes(&self, source: &TableDef, target: &TableDef) -> Vec<Change> {
        let source_columns: Vec<&ColumnDef> = source.columns.iter().collect();
        let target_columns: Vec<&ColumnDef> = target.columns.iter().collect();
        let mut changes = Vec::new();
        for pair in self.pair_by_name(&source_columns, &target_columns, |c| &c.name) {
            match pair {
                (Some(s), None) => {
                    let required = !s.nullable && s.default.is_none() && s.identity.is_none() && s.generated.is_none();
                    let (risk, note) = if required {
                        (
                            Risk::Review,
                            Some("NOT NULL without a default: fails if the table has rows".to_owned()),
                        )
                    } else {
                        (Risk::None, None)
                    };
                    changes.push(change("column", &s.name, ChangeKind::Added, Vec::new(), risk, note));
                }
                (None, Some(t)) => changes.push(change(
                    "column",
                    &t.name,
                    ChangeKind::Removed,
                    Vec::new(),
                    Risk::DataLoss,
                    Some("Dropping the column loses its data".into()),
                )),
                (Some(s), Some(t)) => changes.extend(self.column_change(s, t)),
                (None, None) => {}
            }
        }
        if self.options.options.detect_renames {
            self.mark_possible_rename(source, target, &mut changes);
        }
        changes
    }

    fn column_change(&self, source: &ColumnDef, target: &ColumnDef) -> Option<Change> {
        let fields = field_changes(self.column_fields(source, Side::Source), self.column_fields(target, Side::Target));
        if fields.is_empty() {
            return None;
        }
        let mut risk = Risk::None;
        let mut notes = Vec::new();
        for field in &fields {
            match field.field.as_str() {
                "type" => {
                    let type_risk = self.dialect.type_change_risk(target, source);
                    if type_risk == Risk::DataLoss {
                        notes.push("The type change may lose data");
                    }
                    risk = risk.max(type_risk);
                }
                "nullable" if target.nullable && !source.nullable => {
                    risk = risk.max(Risk::Review);
                    notes.push("Fails if the target has NULLs in this column");
                }
                "identity" | "generated" => risk = risk.max(Risk::Review),
                _ => {}
            }
        }
        let note = (!notes.is_empty()).then(|| notes.join("; "));
        Some(change("column", &source.name, ChangeKind::Modified, fields, risk, note))
    }

    /// One added and one removed column with the same normalized type and nullability: probably a rename.
    fn mark_possible_rename(&self, source: &TableDef, target: &TableDef, changes: &mut [Change]) {
        let of = |what: ChangeKind| -> Vec<usize> {
            changes
                .iter()
                .enumerate()
                .filter(|(_, c)| c.part == "column" && c.what == what)
                .map(|(i, _)| i)
                .collect()
        };
        let (added, removed) = (of(ChangeKind::Added), of(ChangeKind::Removed));
        let ([added], [removed]) = (added.as_slice(), removed.as_slice()) else {
            return;
        };
        let (added, removed) = (*added, *removed);
        let column = |table: &TableDef, name: &str| table.columns.iter().find(|c| self.names_equal(&c.name, name)).cloned();
        let (Some(new), Some(old)) = (column(source, &changes[added].name), column(target, &changes[removed].name)) else {
            return;
        };
        let same = self.ty(&new.type_name, Side::Source) == self.ty(&old.type_name, Side::Target) && new.nullable == old.nullable;
        if !same {
            return;
        }
        let note = format!(
            "Possible rename of {} to {}; the script drops and adds, so the data is lost",
            old.name, new.name
        );
        changes[added].possible_rename = Some(old.name.clone());
        changes[added].risk = changes[added].risk.max(Risk::Review);
        changes[added].note = Some(note.clone());
        changes[removed].possible_rename = Some(new.name.clone());
        changes[removed].note = Some(note);
    }

    fn column_order_change(&self, source: &TableDef, target: &TableDef) -> Option<Change> {
        let common = |table: &TableDef, other: &TableDef| -> Vec<String> {
            table
                .columns
                .iter()
                .filter(|c| other.columns.iter().any(|o| self.names_equal(&o.name, &c.name)))
                .map(|c| self.options.fold(&c.name))
                .collect()
        };
        let (source_order, target_order) = (common(source, target), common(target, source));
        (source_order != target_order).then(|| {
            change(
                "columnOrder",
                &source.name,
                ChangeKind::Modified,
                vec![FieldChange {
                    field: "order".into(),
                    source: Some(source_order.join(", ")),
                    target: Some(target_order.join(", ")),
                }],
                Risk::Review,
                Some("Reordering columns needs a table rebuild".into()),
            )
        })
    }

    fn primary_key_change(&self, source: Vec<Sub>, target: Vec<Sub>, ignore_names: bool) -> Option<Change> {
        match (source.first(), target.first()) {
            (Some(s), Some(t)) => self.sub_change("primaryKey", ignore_names, s, t, PRIMARY_KEY_RISKS),
            _ => self
                .match_subs("primaryKey", ignore_names, source, target, PRIMARY_KEY_RISKS)
                .into_iter()
                .next(),
        }
    }

    /// The sub-objects of one `part` (`primaryKey`, `unique`, `check`, `index`, `foreignKey`) of a table, in its order.
    fn subs(&self, part: &str, table: &TableDef, side: Side) -> Vec<Sub> {
        let key_sub = |key: &KeyDef| Sub {
            name: key.name.clone(),
            user_named: key.name.is_some() && !key.system_named,
            fields: vec![("columns", self.columns_text(&key.columns))],
        };
        match part {
            "primaryKey" => table.primary_key.iter().map(key_sub).collect(),
            "unique" => table.uniques.iter().map(key_sub).collect(),
            "check" => table
                .checks
                .iter()
                .map(|check| Sub {
                    name: check.name.clone(),
                    user_named: check.name.is_some() && !check.system_named,
                    fields: vec![("expression", self.expr(&check.expression, side))],
                })
                .collect(),
            "index" => table.indexes.iter().map(|index| self.index_sub(index, side)).collect(),
            "foreignKey" => table.foreign_keys.iter().map(|key| self.foreign_key_sub(key, side)).collect(),
            _ => Vec::new(),
        }
    }

    fn index_sub(&self, index: &IndexDef, side: Side) -> Sub {
        let parts = index
            .parts
            .iter()
            .map(|part| {
                let key = match (&part.column, &part.expression) {
                    (Some(column), _) => self.options.fold(column),
                    (None, Some(expression)) => self.expr(expression, side),
                    (None, None) => String::new(),
                };
                if part.descending {
                    format!("{key} DESC")
                } else {
                    key
                }
            })
            .collect::<Vec<_>>()
            .join(", ");
        Sub {
            name: Some(index.name.clone()),
            user_named: !index.system_named,
            fields: vec![
                ("columns", parts),
                ("unique", yes_no(index.unique)),
                ("method", index.method.as_deref().unwrap_or_default().to_lowercase()),
                (
                    "predicate",
                    index.predicate.as_deref().map(|p| self.expr(p, side)).unwrap_or_default(),
                ),
                ("include", self.columns_text(&index.include)),
            ],
        }
    }

    fn foreign_key_sub(&self, key: &ForeignKeyDef, side: Side) -> Sub {
        // A reference to the side's own scope schema compares equal to an unqualified one (schema mapping).
        let schema = match (&key.ref_schema, self.schema(side)) {
            (Some(schema), Some(scope)) if self.names_equal(schema, scope) => None,
            (schema, _) => schema.as_deref().map(|s| self.options.fold(s)),
        };
        let table = self.options.fold(&key.ref_table);
        let qualified = schema.map_or(table.clone(), |schema| format!("{schema}.{table}"));
        Sub {
            name: key.name.clone(),
            user_named: key.name.is_some() && !key.system_named,
            fields: vec![
                ("columns", self.columns_text(&key.columns)),
                ("references", format!("{qualified}({})", self.columns_text(&key.ref_columns))),
                ("onUpdate", self.fk_action(key.on_update.as_deref())),
                ("onDelete", self.fk_action(key.on_delete.as_deref())),
            ],
        }
    }

    /// `NULL` and `NO ACTION` are the same; so is `RESTRICT` where the dialect says so.
    fn fk_action(&self, action: Option<&str>) -> String {
        let action = action
            .map(|a| a.split_whitespace().collect::<Vec<_>>().join(" ").to_uppercase())
            .unwrap_or_default();
        match action.as_str() {
            "" | "NO ACTION" => "NO ACTION".into(),
            "RESTRICT" if self.features.restrict_equals_no_action => "NO ACTION".into(),
            _ => action,
        }
    }

    /// Pairs sub-objects (by user name where names matter, then by definition) and reports the differences.
    fn match_subs(&self, part: &str, ignore_names: bool, source: Vec<Sub>, target: Vec<Sub>, risks: SubRisks) -> Vec<Change> {
        let mut target_used = vec![false; target.len()];
        let mut pairs: Vec<Option<usize>> = vec![None; source.len()];
        if !ignore_names {
            for (i, s) in source.iter().enumerate() {
                let Some(name) = s.name.as_deref().filter(|_| s.user_named) else {
                    continue;
                };
                let found = target.iter().enumerate().position(|(j, t)| {
                    !target_used[j] && t.user_named && t.name.as_deref().is_some_and(|other| self.names_equal(name, other))
                });
                if let Some(j) = found {
                    target_used[j] = true;
                    pairs[i] = Some(j);
                }
            }
        }
        for (i, s) in source.iter().enumerate() {
            if pairs[i].is_some() {
                continue;
            }
            let found = target.iter().enumerate().position(|(j, t)| !target_used[j] && t.fields == s.fields);
            if let Some(j) = found {
                target_used[j] = true;
                pairs[i] = Some(j);
            }
        }
        let mut changes = Vec::new();
        for (s, pair) in source.iter().zip(&pairs) {
            match pair {
                Some(j) => changes.extend(self.sub_change(part, ignore_names, s, &target[*j], risks)),
                None => changes.push(change(
                    part,
                    &s.label(),
                    ChangeKind::Added,
                    Vec::new(),
                    risks.added,
                    risk_note(risks.added, risks.note),
                )),
            }
        }
        for (t, used) in target.iter().zip(&target_used) {
            if !used {
                changes.push(change(
                    part,
                    &t.label(),
                    ChangeKind::Removed,
                    Vec::new(),
                    risks.removed,
                    risk_note(risks.removed, risks.note),
                ));
            }
        }
        changes
    }

    fn sub_change(&self, part: &str, ignore_names: bool, source: &Sub, target: &Sub, risks: SubRisks) -> Option<Change> {
        let as_fields = |sub: &Sub| -> Vec<(&'static str, Option<String>)> {
            sub.fields
                .iter()
                .map(|(field, value)| (*field, Some(value.clone()).filter(|v| !v.is_empty())))
                .collect()
        };
        let mut fields = field_changes(as_fields(source), as_fields(target));
        let both_named = source.user_named && target.user_named;
        if !ignore_names
            && both_named
            && !self.names_equal(
                source.name.as_deref().unwrap_or_default(),
                target.name.as_deref().unwrap_or_default(),
            )
        {
            fields.insert(
                0,
                FieldChange {
                    field: "name".into(),
                    source: source.name.clone(),
                    target: target.name.clone(),
                },
            );
        }
        if fields.is_empty() {
            return None;
        }
        let only_name = fields.len() == 1 && fields[0].field == "name";
        let risk = if only_name { Risk::None } else { risks.modified };
        Some(change(
            part,
            &source.label(),
            ChangeKind::Modified,
            fields,
            risk,
            risk_note(risk, risks.note),
        ))
    }

    fn option_changes(&self, source: &TableDef, target: &TableDef) -> Vec<Change> {
        let ignored = |key: &str| self.options.options.ignore_collation && key.eq_ignore_ascii_case("collation");
        let find = |table: &TableDef, key: &str| -> Option<String> {
            table
                .options
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(key))
                .map(|(_, v)| v.clone())
        };
        let mut keys: Vec<&String> = Vec::new();
        for (key, _) in source.options.iter().chain(&target.options) {
            if !ignored(key) && !keys.iter().any(|k| k.eq_ignore_ascii_case(key)) {
                keys.push(key);
            }
        }
        keys.into_iter()
            .filter_map(|key| {
                let (s, t) = (find(source, key), find(target, key));
                let same = match (&s, &t) {
                    (Some(a), Some(b)) => a.eq_ignore_ascii_case(b),
                    _ => s == t,
                };
                (!same).then(|| {
                    let what = match (&s, &t) {
                        (Some(_), None) => ChangeKind::Added,
                        (None, Some(_)) => ChangeKind::Removed,
                        _ => ChangeKind::Modified,
                    };
                    change(
                        "option",
                        key,
                        what,
                        vec![FieldChange {
                            field: key.clone(),
                            source: s,
                            target: t,
                        }],
                        Risk::Review,
                        Some("Engine options are shown for review".into()),
                    )
                })
            })
            .collect()
    }

    // --- Views ----------------------------------------------------------------------------------------------------------

    fn view_item(&self, source: Option<&ViewDef>, target: Option<&ViewDef>) -> DiffItem {
        let name = source.or(target).map(|v| v.name.clone()).unwrap_or_default();
        let (status, changes) = match (source, target) {
            (Some(s), Some(t)) => {
                let changes = self.view_changes(s, t);
                let status = if changes.is_empty() {
                    DiffStatus::Identical
                } else {
                    DiffStatus::Different
                };
                (status, changes)
            }
            (Some(_), None) => (DiffStatus::OnlyInSource, Vec::new()),
            _ => (DiffStatus::OnlyInTarget, Vec::new()),
        };
        let ddl = |view: &ViewDef| {
            let mut view = view.clone();
            if self.options.options.ignore_comments {
                view.comment = None;
            }
            self.dialect.view_ddl(self.target_schema, &view)
        };
        DiffItem {
            id: format!("view:{name}"),
            kind: "view".into(),
            name,
            status,
            risk: max_risk(&changes),
            changes,
            source_ddl: source.map(ddl),
            target_ddl: target.map(ddl),
        }
    }

    fn view_changes(&self, source: &ViewDef, target: &ViewDef) -> Vec<Change> {
        let fields = |view: &ViewDef, side: Side| {
            vec![
                ("definition", Some(self.expr(&view.definition, side))),
                ("materialized", Some(yes_no(view.materialized))),
            ]
        };
        let mut changes = Vec::new();
        let definition = field_changes(fields(source, Side::Source), fields(target, Side::Target));
        if !definition.is_empty() {
            let materialized = source.materialized || target.materialized;
            let (risk, note) = if materialized {
                (
                    Risk::Review,
                    Some("The materialized view is recreated; its data must be refreshed".to_owned()),
                )
            } else {
                (Risk::None, None)
            };
            changes.push(change("definition", &source.name, ChangeKind::Modified, definition, risk, note));
        }
        if !self.options.options.ignore_comments {
            changes.extend(comment_change(&source.name, source.comment.as_deref(), target.comment.as_deref()));
        }
        changes
    }
}

fn change(part: &str, name: &str, what: ChangeKind, fields: Vec<FieldChange>, risk: Risk, note: Option<String>) -> Change {
    Change {
        part: part.to_owned(),
        name: name.to_owned(),
        what,
        fields,
        risk,
        note,
        possible_rename: None,
    }
}

fn risk_note(risk: Risk, note: Option<&str>) -> Option<String> {
    (risk != Risk::None).then(|| note.map(str::to_owned)).flatten()
}

fn comment_change(name: &str, source: Option<&str>, target: Option<&str>) -> Option<Change> {
    let (source, target) = (source.filter(|c| !c.is_empty()), target.filter(|c| !c.is_empty()));
    (source != target).then(|| {
        change(
            "comment",
            name,
            ChangeKind::Modified,
            vec![FieldChange {
                field: "comment".into(),
                source: source.map(str::to_owned),
                target: target.map(str::to_owned),
            }],
            Risk::None,
            None,
        )
    })
}

/// The fields whose values differ, in `source`'s order (both lists have the same fields).
fn field_changes(source: Vec<(&'static str, Option<String>)>, target: Vec<(&'static str, Option<String>)>) -> Vec<FieldChange> {
    source
        .into_iter()
        .zip(target)
        .filter(|((_, s), (_, t))| s != t)
        .map(|((field, s), (_, t))| FieldChange {
            field: field.to_owned(),
            source: s,
            target: t,
        })
        .collect()
}

fn max_risk(changes: &[Change]) -> Risk {
    changes.iter().map(|c| c.risk).max().unwrap_or_default()
}

fn yes_no(value: bool) -> String {
    if value { "yes" } else { "no" }.to_owned()
}

fn identity_text(kind: IdentityKind) -> &'static str {
    match kind {
        IdentityKind::Always => "always",
        IdentityKind::ByDefault => "by default",
        IdentityKind::Serial => "serial",
        IdentityKind::AutoIncrement => "auto increment",
    }
}
