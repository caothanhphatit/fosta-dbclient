//! Script assembly: the planned steps rendered by the target's dialect, with a header that says what was compared,
//! with which options and how risky the script is, `-- WARNING:` lines above the statements that need a look, and the
//! dialect's transaction wrapper. Text only: nothing here runs it.

use super::{
    diff::diff,
    plan::{plan, Plan},
};
use crate::contract::{
    CompareOptions, RenderContext, RenderedStatement, Risk, SchemaDialect, SchemaSnapshot, ScopeSummary, ScriptWarning, SyncScript,
};

/// The sync script that makes `target` match `source` for the items in `include`. `generated_at` goes in the header.
pub fn sync_script(
    source: &SchemaSnapshot,
    target: &SchemaSnapshot,
    options: &CompareOptions,
    include: &[String],
    dialect: &dyn SchemaDialect,
    generated_at: &str,
) -> SyncScript {
    let diff = diff(source, target, options, dialect);
    let Plan {
        steps,
        warnings: plan_warnings,
    } = plan(&diff, include, source, target, options, dialect);
    let ctx = RenderContext {
        source_schema: source.scope.schema.clone(),
        target_schema: target.scope.schema.clone(),
    };
    let mut warnings = plan_warnings.clone();
    let mut body: Vec<RenderedStatement> = Vec::new();
    for planned in &steps {
        let mut rendered = dialect.render(&planned.step, &ctx);
        if let (Some(note), Some(first)) = (&planned.note, rendered.first_mut()) {
            first.warnings.insert(0, note.clone());
            first.risk = first.risk.max(Risk::Review);
        }
        for statement in &rendered {
            warnings.extend(statement.warnings.iter().map(|message| {
                ScriptWarning {
                    item_id: planned.item_id.clone(),
                    risk: statement
                        .risk
                        .max(if statement.warnings.is_empty() { Risk::None } else { Risk::Review }),
                    message: message.clone(),
                }
            }));
        }
        body.extend(rendered);
    }
    let features = dialect.features();
    // Wrapping DDL that commits implicitly (MySQL) in a transaction would only pretend to be atomic.
    let transactional = !body.is_empty() && features.transactional_ddl && options.transactional.unwrap_or(true);
    let statements = body.iter().filter(|s| !is_comment(&s.sql)).count();
    let wrapped = if body.is_empty() { body } else { dialect.wrap(body, transactional) };

    let mut sql = header(source, target, options, &warnings, &plan_warnings, generated_at);
    if !features.transactional_ddl && !wrapped.is_empty() {
        // After the header's last line ("Review before running ..."), still part of the leading comment block.
        sql.push_str(&format!(
            "-- WARNING: Not atomic: {} commits every DDL statement implicitly, so a failure leaves the target partly migrated.\n\
             -- The statements are ordered so that an early failure leaves less to undo; try the script on a backup or a \
             staging copy first.\n",
            target.engine
        ));
    }
    if wrapped.is_empty() {
        sql.push_str("\n-- Nothing to do: the selected objects of the target already match the source.\n");
    }
    for statement in &wrapped {
        sql.push('\n');
        for warning in &statement.warnings {
            sql.push_str(&format!("-- WARNING: {}\n", one_line(warning)));
        }
        let text = statement.sql.trim_end();
        sql.push_str(text);
        let separator = features
            .batch_separator
            .is_some_and(|separator| text.eq_ignore_ascii_case(separator));
        if !is_comment(text) && !separator && !text.ends_with(';') {
            sql.push(';');
        }
        sql.push('\n');
    }
    SyncScript {
        sql,
        warnings,
        statements,
        transactional,
        dialect: target.engine.clone(),
    }
}

/// Whether a rendered statement is only comments (a manual step).
fn is_comment(sql: &str) -> bool {
    sql.lines().all(|line| {
        let line = line.trim();
        line.is_empty() || line.starts_with("--")
    })
}

fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn scope(summary: &ScopeSummary) -> String {
    let name = match &summary.schema {
        // MySQL scopes are databases: the schema is the database itself.
        Some(schema) if *schema != summary.database => format!("{}.{schema}", summary.database),
        _ => summary.database.clone(),
    };
    format!("{name} ({} {})", summary.engine, summary.server_version)
}

fn summary(snapshot: &SchemaSnapshot) -> ScopeSummary {
    ScopeSummary {
        engine: snapshot.engine.clone(),
        server_version: snapshot.server_version.clone(),
        database: snapshot.scope.database.clone(),
        schema: snapshot.scope.schema.clone(),
    }
}

fn header(
    source: &SchemaSnapshot,
    target: &SchemaSnapshot,
    options: &CompareOptions,
    warnings: &[ScriptWarning],
    plan_warnings: &[ScriptWarning],
    generated_at: &str,
) -> String {
    let mut ignored = Vec::new();
    for (on, label) in [
        (options.ignore_column_order, "column order"),
        (options.ignore_comments, "comments"),
        (options.ignore_collation, "collations"),
        (options.ignore_defaults, "defaults"),
        (options.ignore_constraint_names, "constraint names"),
        (options.ignore_index_names, "index names"),
    ] {
        if on {
            ignored.push(label);
        }
    }
    let count = |risk: Risk| warnings.iter().filter(|w| w.risk == risk).count();
    let mut lines = vec![
        format!(
            "-- Schema sync script generated by Fosta Studio {} at {generated_at}.",
            env!("CARGO_PKG_VERSION")
        ),
        format!("-- Source: {}", scope(&summary(source))),
        format!("-- Target: {}", scope(&summary(target))),
        format!(
            "-- Ignored: {}.",
            if ignored.is_empty() {
                "nothing".to_owned()
            } else {
                ignored.join(", ")
            }
        ),
    ];
    if !options.exclude.is_empty() {
        lines.push(format!("-- Excluded names: {}.", options.exclude.join(", ")));
    }
    lines.push(format!(
        "-- Warnings: {} data loss, {} to review.",
        count(Risk::DataLoss),
        count(Risk::Review)
    ));
    for warning in plan_warnings {
        lines.push(format!("-- WARNING: {}", one_line(&warning.message)));
    }
    lines.push("-- Review before running. Nothing has been run.".to_owned());
    let mut text = lines.join("\n");
    text.push('\n');
    text
}
