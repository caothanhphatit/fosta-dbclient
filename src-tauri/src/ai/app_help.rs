//! The app knowledge pack: what the assistant knows about Fosta Studio itself.
//!
//! Sources are embedded at build time, so nothing is fetched at run time:
//!
//! - `docs/app-help.yaml`: curated how-to entries (id, group, title, keywords, body, optional UI action id), read by
//!   [`parse_entries`], a parser for the strict YAML subset the file documents in its header;
//! - the user-facing markdown docs (`docs/features.md`, `docs/mcp.md`, `docs/security.md`, `docs/web-edition.md`,
//!   `docs/engines/*.md` except the developer-only `language.md`), split on `##` headings by [`split_markdown`];
//! - the `[Unreleased]` and latest release sections of `CHANGELOG.md`.
//!
//! On first use they are merged into one list of sections with a small BM25 index ([`pack`]). The assistant gets
//! [`capability_summary`] (feature titles by group, about 400 tokens) in the trusted part of its prompt and calls
//! [`search_app_help`] for the steps: the top three sections, at most [`MAX_RESULT_CHARS`] characters in all.

use std::{collections::HashMap, sync::OnceLock};

/// The whole answer of [`search_app_help`].
pub(crate) const MAX_RESULT_CHARS: usize = 3000;
/// Sections returned by [`search_app_help`].
const TOP_SECTIONS: usize = 3;
/// A markdown section longer than this is cut into chunks at paragraph and list-item boundaries.
const MAX_CHUNK_CHARS: usize = 1400;
/// The capability summary stays under this many characters (about 450 tokens).
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) const MAX_SUMMARY_CHARS: usize = 1800;

/// Curated entries rank above doc sections with the same score: they are short and name the UI labels.
const CURATED_BOOST: f64 = 1.3;
/// Title and keyword tokens count this many times as body tokens.
const FIELD_WEIGHT: usize = 3;
const BM25_K1: f64 = 1.2;
const BM25_B: f64 = 0.75;

const APP_HELP_YAML: &str = include_str!("../../../docs/app-help.yaml");
const CHANGELOG: &str = include_str!("../../../CHANGELOG.md");
const MARKDOWN_DOCS: &[(&str, &str)] = &[
    ("docs/features.md", include_str!("../../../docs/features.md")),
    ("docs/mcp.md", include_str!("../../../docs/mcp.md")),
    ("docs/security.md", include_str!("../../../docs/security.md")),
    ("docs/web-edition.md", include_str!("../../../docs/web-edition.md")),
    ("docs/engines/clickhouse.md", include_str!("../../../docs/engines/clickhouse.md")),
    (
        "docs/engines/elasticsearch.md",
        include_str!("../../../docs/engines/elasticsearch.md"),
    ),
    ("docs/engines/opensearch.md", include_str!("../../../docs/engines/opensearch.md")),
    ("docs/engines/oracle.md", include_str!("../../../docs/engines/oracle.md")),
    ("docs/engines/postgres.md", include_str!("../../../docs/engines/postgres.md")),
    ("docs/engines/redis.md", include_str!("../../../docs/engines/redis.md")),
    ("docs/engines/sqlite.md", include_str!("../../../docs/engines/sqlite.md")),
    ("docs/engines/ssh-tls.md", include_str!("../../../docs/engines/ssh-tls.md")),
];

/// One curated entry of `docs/app-help.yaml`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct HelpEntry {
    pub(crate) id: String,
    pub(crate) group: String,
    pub(crate) title: String,
    pub(crate) keywords: Vec<String>,
    pub(crate) body: String,
    /// A UI action id such as `open_settings:mcp`; the frontend action registry maps it.
    pub(crate) action: Option<String>,
}

/// A searchable section: a curated entry or a part of a markdown doc.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct HelpSection {
    /// The entry id, or `<doc path>#<n>` for a doc section.
    pub(crate) id: String,
    pub(crate) title: String,
    /// `app-help` or the doc's path.
    pub(crate) source: String,
    pub(crate) keywords: Vec<String>,
    pub(crate) body: String,
    pub(crate) action: Option<String>,
    curated: bool,
}

impl From<HelpEntry> for HelpSection {
    fn from(entry: HelpEntry) -> Self {
        HelpSection {
            id: entry.id,
            title: entry.title,
            source: String::from("app-help"),
            keywords: entry.keywords,
            body: entry.body,
            action: entry.action,
            curated: true,
        }
    }
}

/// The merged sections, their index and the summary, built once.
pub(crate) struct HelpPack {
    pub(crate) entries: Vec<HelpEntry>,
    pub(crate) sections: Vec<HelpSection>,
    index: Bm25,
    summary: String,
}

/// The pack, built at first use. The embedded YAML is checked by the tests, so a parse error here is a build mistake.
pub(crate) fn pack() -> &'static HelpPack {
    static PACK: OnceLock<HelpPack> = OnceLock::new();
    PACK.get_or_init(|| build_pack().expect("docs/app-help.yaml parses (checked by the app_help tests)"))
}

fn build_pack() -> Result<HelpPack, String> {
    let entries = parse_entries(APP_HELP_YAML)?;
    let mut sections: Vec<HelpSection> = entries.iter().cloned().map(HelpSection::from).collect();
    for (path, text) in MARKDOWN_DOCS {
        sections.extend(split_markdown(path, text));
    }
    sections.extend(split_markdown("CHANGELOG.md", &current_changelog(CHANGELOG)));
    let index = Bm25::new(&sections);
    let summary = build_summary(&entries);
    Ok(HelpPack {
        entries,
        sections,
        index,
        summary,
    })
}

/// Fosta Studio's features as titles grouped by area, for the trusted part of the system prompt.
// Wired into the system prompt in a later phase (prompt.rs).
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn capability_summary() -> &'static str {
    &pack().summary
}

/// The best sections for `query`, best first (at most `limit`).
pub(crate) fn search(query: &str, limit: usize) -> Vec<&'static HelpSection> {
    let pack = pack();
    pack.index
        .rank(&pack.sections, query)
        .into_iter()
        .take(limit)
        .map(|index| &pack.sections[index])
        .collect()
}

/// The UI action ids the pack names (`verb:target`, e.g. `open_settings:mcp`), each with the title of the first entry
/// that names it, sorted by id. This is the allowlist of `suggest_app_action`; the frontend registry
/// (`src/app/appActions.ts`) maps exactly these ids, which the tests on both sides check against `docs/app-help.yaml`.
pub(crate) fn actions() -> Vec<(&'static str, &'static str)> {
    let mut actions: Vec<(&str, &str)> = Vec::new();
    for entry in &pack().entries {
        if let Some(action) = entry.action.as_deref() {
            if !actions.iter().any(|(id, _)| *id == action) {
                actions.push((action, &entry.title));
            }
        }
    }
    actions.sort_unstable();
    actions
}

/// The title of the first entry naming the UI action `id`, or `None` when the pack names no such action.
pub(crate) fn action_title(id: &str) -> Option<&'static str> {
    actions().into_iter().find(|(action, _)| *action == id).map(|(_, title)| title)
}

/// The `search_app_help` tool: the top three sections for `query`, at most [`MAX_RESULT_CHARS`] characters.
pub(crate) fn search_app_help(query: &str) -> String {
    let query = query.trim();
    if query.is_empty() {
        return String::from("Pass `query`: a few words about the feature, e.g. \"port forward\" or \"export csv\".");
    }
    let hits = search(query, TOP_SECTIONS);
    if hits.is_empty() {
        return format!(
            "No help section matches \"{query}\". Try other words (a feature name or a UI label), or answer from the \
             capability summary and say you are not sure of the exact steps."
        );
    }
    let mut out = String::new();
    for (position, section) in hits.iter().enumerate() {
        let budget = (MAX_RESULT_CHARS - out.len()) / (hits.len() - position);
        out.push_str(&format_section(section, budget));
        if position + 1 < hits.len() {
            out.push_str("\n\n");
        }
    }
    out
}

fn format_section(section: &HelpSection, budget: usize) -> String {
    let mut head = format!("## {} ({})\n", section.title, section.source);
    if let Some(action) = &section.action {
        head.push_str(&format!("UI action: {action}\n"));
    }
    let room = budget.saturating_sub(head.len());
    head.push_str(&cut(&section.body, room));
    cut(&head, budget)
}

/// `text` cut to at most `max` bytes at a char boundary, with an ellipsis when cut.
fn cut(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_owned();
    }
    let mut end = max.saturating_sub('…'.len_utf8());
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", text[..end].trim_end())
}

fn build_summary(entries: &[HelpEntry]) -> String {
    let mut groups: Vec<(&str, Vec<String>)> = Vec::new();
    for entry in entries {
        let title = short_title(&entry.title, &entry.group);
        match groups.iter_mut().find(|(group, _)| *group == entry.group) {
            Some((_, titles)) => titles.push(title),
            None => groups.push((&entry.group, vec![title])),
        }
    }
    let mut out = String::from("Fosta Studio features (call search_app_help for the steps and exact UI labels):\n");
    for (group, titles) in groups {
        out.push_str(&format!("- {group}: {}\n", titles.join("; ")));
    }
    out.trim_end().to_owned()
}

/// A title without its parenthesis and without the leading engine name its group already says.
fn short_title(title: &str, group: &str) -> String {
    let mut title = match title.find(" (") {
        Some(at) => &title[..at],
        None => title,
    };
    // Engine groups: "MongoDB indexes" reads "Indexes" under "- MongoDB:".
    for engine in [
        "Elasticsearch and OpenSearch",
        "Elasticsearch",
        "MongoDB",
        "Redis",
        "ClickHouse",
        "Oracle",
    ] {
        if group.starts_with(engine) {
            if let Some(rest) = title.strip_prefix(engine).and_then(|rest| rest.strip_prefix(' ')) {
                title = rest;
                break;
            }
        }
    }
    let mut chars = title.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

// --- docs/app-help.yaml ---------------------------------------------------------------------------------------------

/// Parses the YAML subset of `docs/app-help.yaml`: a top-level list of mappings; `key: value` plain scalars (quotes
/// around a whole value are dropped), `keywords: [a, b]` flow lists and a `body: |` literal block indented by four
/// spaces; `#` comment lines and blank lines between entries. Anything else is an error naming the line, and so is an
/// entry without id, group, title, keywords or body, an unknown key, or a repeated id.
pub(crate) fn parse_entries(text: &str) -> Result<Vec<HelpEntry>, String> {
    const BODY_INDENT: &str = "    ";
    let mut entries: Vec<HelpEntry> = Vec::new();
    let mut lines = text.lines().enumerate().peekable();
    while let Some((number, raw)) = lines.next() {
        let line_no = number + 1;
        let line = raw.trim_end();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let field = if let Some(rest) = line.strip_prefix("- ") {
            entries.push(HelpEntry::default());
            rest
        } else if let Some(rest) = line.strip_prefix("  ") {
            if rest.starts_with(' ') {
                return Err(format!("line {line_no}: unexpected indentation"));
            }
            rest
        } else {
            return Err(format!("line {line_no}: expected `- key: value` or `  key: value`"));
        };
        let entry = entries
            .last_mut()
            .ok_or_else(|| format!("line {line_no}: a field before the first `- ` entry"))?;
        let (key, value) = field
            .split_once(':')
            .map(|(key, value)| (key.trim(), value.trim()))
            .ok_or_else(|| format!("line {line_no}: expected `key: value`"))?;
        match key {
            "body" => {
                if value != "|" {
                    return Err(format!("line {line_no}: body must be a `|` block"));
                }
                let mut body: Vec<&str> = Vec::new();
                while let Some((_, next)) = lines.peek() {
                    let next = next.trim_end();
                    if next.is_empty() {
                        body.push("");
                    } else if let Some(content) = next.strip_prefix(BODY_INDENT) {
                        body.push(content);
                    } else {
                        break;
                    }
                    lines.next();
                }
                while body.last().is_some_and(|last| last.is_empty()) {
                    body.pop();
                }
                entry.body = body.join("\n");
            }
            "keywords" => {
                let inner = value
                    .strip_prefix('[')
                    .and_then(|rest| rest.strip_suffix(']'))
                    .ok_or_else(|| format!("line {line_no}: keywords must be a `[a, b]` list"))?;
                entry.keywords = inner
                    .split(',')
                    .map(|keyword| unquote(keyword.trim()).to_owned())
                    .filter(|keyword| !keyword.is_empty())
                    .collect();
            }
            "id" | "group" | "title" | "action" => {
                if value.contains(" #") || value.contains(": ") {
                    return Err(format!("line {line_no}: `{key}` must not contain \": \" or \" #\""));
                }
                let value = unquote(value).to_owned();
                match key {
                    "id" => entry.id = value,
                    "group" => entry.group = value,
                    "title" => entry.title = value,
                    _ => entry.action = Some(value).filter(|action| !action.is_empty()),
                }
            }
            other => return Err(format!("line {line_no}: unknown key `{other}`")),
        }
    }
    let mut seen = std::collections::HashSet::new();
    for entry in &entries {
        let label = if entry.id.is_empty() {
            entry.title.as_str()
        } else {
            entry.id.as_str()
        };
        for (name, empty) in [
            ("id", entry.id.is_empty()),
            ("group", entry.group.is_empty()),
            ("title", entry.title.is_empty()),
            ("keywords", entry.keywords.is_empty()),
            ("body", entry.body.trim().is_empty()),
        ] {
            if empty {
                return Err(format!("entry `{label}` has no {name}"));
            }
        }
        if !seen.insert(entry.id.as_str()) {
            return Err(format!("the id `{}` is used twice", entry.id));
        }
    }
    Ok(entries)
}

fn unquote(value: &str) -> &str {
    for quote in ['"', '\''] {
        if let Some(inner) = value.strip_prefix(quote).and_then(|rest| rest.strip_suffix(quote)) {
            return inner;
        }
    }
    value
}

// --- Markdown ---------------------------------------------------------------------------------------------------------

/// Splits a markdown doc into sections at its `## ` headings (outside code fences); deeper headings stay inside their
/// section, and the text before the first `## ` is a section named after the `# ` title. Sections longer than
/// [`MAX_CHUNK_CHARS`] are cut into chunks at blank lines and top-level list items.
pub(crate) fn split_markdown(path: &str, text: &str) -> Vec<HelpSection> {
    let mut doc_title = path.to_owned();
    let mut raw: Vec<(String, Vec<&str>)> = vec![(String::new(), Vec::new())];
    let mut in_fence = false;
    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            in_fence = !in_fence;
        }
        if !in_fence {
            if let Some(title) = line.strip_prefix("## ") {
                raw.push((title.trim().to_owned(), Vec::new()));
                continue;
            }
            if let Some(title) = line.strip_prefix("# ") {
                doc_title = title.trim().to_owned();
                continue;
            }
        }
        if let Some((_, lines)) = raw.last_mut() {
            lines.push(line);
        }
    }
    let mut sections = Vec::new();
    for (heading, lines) in raw {
        let body = lines.join("\n").trim().to_owned();
        if body.is_empty() {
            continue;
        }
        let title = if heading.is_empty() {
            doc_title.clone()
        } else {
            format!("{doc_title}: {heading}")
        };
        let chunks = chunk(&body, MAX_CHUNK_CHARS);
        let several = chunks.len() > 1;
        for (part, body) in chunks.into_iter().enumerate() {
            let title = match (several, lead_name(&body)) {
                (true, Some(name)) => format!("{title}: {name}"),
                (true, None) => format!("{title} ({})", part + 1),
                (false, _) => title.clone(),
            };
            sections.push(HelpSection {
                id: format!("{path}#{}", sections.len() + 1),
                title,
                source: path.to_owned(),
                keywords: Vec::new(),
                body,
                action: None,
                curated: false,
            });
        }
    }
    sections
}

/// Cuts `body` into chunks of at most `max` bytes: blocks end at blank lines and before top-level list items; a
/// block longer than `max` is cut at line ends, and a line longer than `max` at a char boundary.
fn chunk(body: &str, max: usize) -> Vec<String> {
    if body.len() <= max {
        return vec![body.to_owned()];
    }
    let mut blocks: Vec<String> = Vec::new();
    let mut current = String::new();
    for line in body.lines() {
        let starts_block = line.trim().is_empty() || line.starts_with("- ") || line.starts_with("* ");
        if starts_block && !current.trim().is_empty() {
            blocks.push(std::mem::take(&mut current));
        }
        if line.trim().is_empty() {
            continue;
        }
        current.push_str(line);
        current.push('\n');
    }
    if !current.trim().is_empty() {
        blocks.push(current);
    }
    let mut pieces: Vec<String> = Vec::new();
    for block in blocks {
        if block.len() <= max {
            pieces.push(block);
            continue;
        }
        let mut piece = String::new();
        for line in block.lines() {
            if !piece.is_empty() && piece.len() + line.len() + 1 > max {
                pieces.push(std::mem::take(&mut piece));
            }
            let mut rest = line;
            while rest.len() > max {
                let mut end = max;
                while !rest.is_char_boundary(end) {
                    end -= 1;
                }
                pieces.push(rest[..end].to_owned());
                rest = &rest[end..];
            }
            piece.push_str(rest);
            piece.push('\n');
        }
        if !piece.trim().is_empty() {
            pieces.push(piece);
        }
    }
    let mut chunks: Vec<String> = Vec::new();
    let mut current = String::new();
    for piece in pieces {
        if !current.is_empty() && current.len() + piece.len() + 1 > max {
            chunks.push(current.trim().to_owned());
            current.clear();
        }
        if !current.is_empty() {
            current.push('\n');
        }
        current.push_str(&piece);
    }
    if !current.trim().is_empty() {
        chunks.push(current.trim().to_owned());
    }
    chunks
}

/// The bold name a chunk starts with (`- **Workspaces**: …`), used to title chunks of a long list.
fn lead_name(body: &str) -> Option<String> {
    let rest = body.trim_start();
    let rest = rest
        .strip_prefix("- ")
        .or_else(|| rest.strip_prefix("* "))
        .unwrap_or(rest)
        .trim_start();
    let rest = rest.strip_prefix("**")?;
    let end = rest.find("**")?;
    let name = rest[..end].trim().trim_end_matches(':');
    (!name.is_empty() && name.len() <= 60).then(|| name.to_owned())
}

/// The `[Unreleased]` section and the latest release of the changelog, with their `## ` headings.
fn current_changelog(text: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    let mut releases = 0;
    for line in text.lines() {
        if line.starts_with("## [") {
            releases += 1;
            if releases > 2 {
                break;
            }
        }
        if releases > 0 {
            out.push(line);
        }
    }
    format!("# Changelog\n{}", out.join("\n"))
}

// --- Search -----------------------------------------------------------------------------------------------------------

const STOP_WORDS: &[&str] = &[
    "a", "an", "and", "are", "as", "at", "be", "by", "can", "do", "does", "for", "from", "how", "i", "in", "is", "it", "me", "my", "of",
    "on", "or", "the", "this", "to", "what", "where", "which", "with", "you", "your", "we", "use", "using", "want", "there", "way", "get",
    "set", "make", "should", "would", "could", "please", "into", "about",
];

/// Lower-case word tokens without stop words, lightly stemmed (`forwarding` → `forward`, `keys` → `key`).
fn tokens(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_lowercase)
        .filter(|word| !STOP_WORDS.contains(&word.as_str()))
        .map(|word| stem(&word))
        .collect()
}

fn stem(word: &str) -> String {
    let len = word.chars().count();
    for (suffix, min) in [("ing", 6), ("ed", 5)] {
        if len >= min {
            if let Some(base) = word.strip_suffix(suffix) {
                return base.to_owned();
            }
        }
    }
    if len >= 4 && !word.ends_with("ss") && !word.ends_with("us") && !word.ends_with("is") {
        if let Some(base) = word.strip_suffix("ies") {
            return format!("{base}y");
        }
        if let Some(base) = word.strip_suffix('s') {
            return base.to_owned();
        }
    }
    word.to_owned()
}

/// A BM25 index over the sections: title and keywords weigh [`FIELD_WEIGHT`] times the body.
struct Bm25 {
    /// Term frequencies per section.
    terms: Vec<HashMap<String, usize>>,
    lengths: Vec<usize>,
    average_length: f64,
    /// Sections containing each term.
    document_frequency: HashMap<String, usize>,
}

impl Bm25 {
    fn new(sections: &[HelpSection]) -> Self {
        let mut terms = Vec::with_capacity(sections.len());
        let mut lengths = Vec::with_capacity(sections.len());
        let mut document_frequency: HashMap<String, usize> = HashMap::new();
        for section in sections {
            let mut counts: HashMap<String, usize> = HashMap::new();
            let heading = format!("{} {}", section.title, section.keywords.join(" "));
            for token in tokens(&heading) {
                *counts.entry(token).or_default() += FIELD_WEIGHT;
            }
            for token in tokens(&section.body) {
                *counts.entry(token).or_default() += 1;
            }
            for term in counts.keys() {
                *document_frequency.entry(term.clone()).or_default() += 1;
            }
            lengths.push(counts.values().sum());
            terms.push(counts);
        }
        let average_length = lengths.iter().sum::<usize>() as f64 / lengths.len().max(1) as f64;
        Bm25 {
            terms,
            lengths,
            average_length,
            document_frequency,
        }
    }

    /// Indexes of the sections matching `query`, best first.
    fn rank(&self, sections: &[HelpSection], query: &str) -> Vec<usize> {
        let mut query_terms = tokens(query);
        query_terms.sort();
        query_terms.dedup();
        let total = self.terms.len() as f64;
        let mut scored: Vec<(usize, f64)> = Vec::new();
        for (index, counts) in self.terms.iter().enumerate() {
            let mut score = 0.0;
            for term in &query_terms {
                let Some(&frequency) = counts.get(term) else { continue };
                let documents = self.document_frequency.get(term).copied().unwrap_or(0) as f64;
                let idf = ((total - documents + 0.5) / (documents + 0.5) + 1.0).ln();
                let frequency = frequency as f64;
                let norm = 1.0 - BM25_B + BM25_B * self.lengths[index] as f64 / self.average_length;
                score += idf * frequency * (BM25_K1 + 1.0) / (frequency + BM25_K1 * norm);
            }
            if score > 0.0 {
                if sections[index].curated {
                    score *= CURATED_BOOST;
                }
                scored.push((index, score));
            }
        }
        scored.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
        scored.into_iter().map(|(index, _)| index).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn top_ids(query: &str, n: usize) -> Vec<String> {
        search(query, n).into_iter().map(|section| section.id.clone()).collect()
    }

    #[test]
    fn the_embedded_help_parses_with_complete_unique_entries() {
        let entries = parse_entries(APP_HELP_YAML).expect("docs/app-help.yaml parses");
        assert!(entries.len() >= 50, "{} entries", entries.len());
        let mut ids = std::collections::HashSet::new();
        for entry in &entries {
            assert!(
                !entry.id.is_empty() && !entry.group.is_empty() && !entry.title.is_empty(),
                "{entry:?}"
            );
            assert!(!entry.body.trim().is_empty(), "{}", entry.id);
            assert!(
                !entry.keywords.is_empty() && entry.keywords.iter().all(|k| !k.trim().is_empty()),
                "{}",
                entry.id
            );
            assert!(ids.insert(entry.id.clone()), "duplicate id {}", entry.id);
            assert!(
                entry.id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'),
                "id {} is kebab-case",
                entry.id
            );
            if let Some(action) = &entry.action {
                let (verb, target) = action.split_once(':').unwrap_or_default();
                assert!(
                    !verb.is_empty() && !target.is_empty(),
                    "{}: action {action} is verb:target",
                    entry.id
                );
            }
            assert!(
                entry.body.len() <= MAX_RESULT_CHARS / 2,
                "{}: body is {} chars",
                entry.id,
                entry.body.len()
            );
        }
        assert_eq!(pack().entries.len(), entries.len());
    }

    #[test]
    fn the_parser_reads_blocks_lists_and_refuses_bad_input() {
        let text = "# comment\n\n- id: one\n  group: G\n  title: \"First\"\n  keywords: [a, b c, 'd']\n  action: open_settings:mcp\n  body: |\n    Line one.\n\n    - item\n\n- id: two\n  group: G\n  title: Second\n  keywords: [x]\n  body: |\n    Only.\n";
        let entries = parse_entries(text).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].title, "First");
        assert_eq!(entries[0].keywords, ["a", "b c", "d"]);
        assert_eq!(entries[0].action.as_deref(), Some("open_settings:mcp"));
        assert_eq!(entries[0].body, "Line one.\n\n- item");
        assert_eq!(entries[1].action, None);
        assert_eq!(entries[1].body, "Only.");

        let missing = "- id: one\n  group: G\n  title: T\n  keywords: [a]\n";
        assert!(parse_entries(missing).unwrap_err().contains("no body"));
        let twice = format!("{}{}", &text[10..], &text[10..]);
        assert!(parse_entries(&twice).unwrap_err().contains("used twice"));
        assert!(parse_entries("- id: x\n  colour: red\n").unwrap_err().contains("unknown key"));
        assert!(parse_entries("- id: x\n  keywords: a, b\n").unwrap_err().contains("list"));
        assert!(parse_entries("- id: x\n  body: plain\n").unwrap_err().contains("block"));
        assert!(parse_entries("id: x\n").unwrap_err().contains("line 1"));
    }

    #[test]
    fn markdown_splits_on_level_two_headings_outside_fences() {
        let text = "# Guide\nIntro text.\n## First\nAlpha.\n### Deeper\nStill first.\n```sh\n## not a heading\n```\n## Second\nBeta.\n";
        let sections = split_markdown("docs/guide.md", text);
        let titles: Vec<&str> = sections.iter().map(|section| section.title.as_str()).collect();
        assert_eq!(titles, ["Guide", "Guide: First", "Guide: Second"]);
        assert!(sections[1].body.contains("### Deeper") && sections[1].body.contains("## not a heading"));
        assert_eq!(sections[2].body, "Beta.");
        assert!(sections.iter().all(|section| section.source == "docs/guide.md"));
    }

    #[test]
    fn long_sections_are_chunked_and_titled_by_their_lead_item() {
        let item = |name: &str| format!("- **{name}**: {}\n", "word ".repeat(120));
        let text = format!("# Features\n{}{}{}", item("Workspaces"), item("Commands"), item("Connections"));
        let sections = split_markdown("docs/features.md", &text);
        assert!(sections.len() >= 2, "{}", sections.len());
        assert!(sections.iter().all(|section| section.body.len() <= MAX_CHUNK_CHARS));
        assert_eq!(sections[0].title, "Features: Workspaces");
        // Every doc section of the real pack respects the bound too.
        assert!(pack()
            .sections
            .iter()
            .filter(|section| !section.curated)
            .all(|section| section.body.len() <= MAX_CHUNK_CHARS));
    }

    #[test]
    fn the_pack_merges_entries_docs_and_the_current_changelog() {
        let pack = pack();
        for source in [
            "app-help",
            "docs/features.md",
            "docs/mcp.md",
            "docs/engines/redis.md",
            "CHANGELOG.md",
        ] {
            assert!(pack.sections.iter().any(|section| section.source == source), "{source}");
        }
        let changelog: Vec<&HelpSection> = pack.sections.iter().filter(|section| section.source == "CHANGELOG.md").collect();
        // [Unreleased] is empty right after a release, so only the latest release is certain to be there.
        assert!(!changelog.is_empty());
        let releases: std::collections::HashSet<&str> = changelog
            .iter()
            .filter_map(|section| section.title.split(": ").nth(1))
            .map(|heading| heading.split(" (").next().unwrap_or(heading))
            .collect();
        assert!(releases.len() <= 2, "{releases:?}");
    }

    #[test]
    fn search_finds_the_expected_entries() {
        for (query, expected) in [
            ("how do I port forward", "commands"),
            ("ssh key", "ssh-tunnel"),
            ("export csv", "import-export"),
            ("aggregation", "mongo-aggregations"),
            ("mcp claude", "mcp-server"),
        ] {
            let ids = top_ids(query, 3);
            assert!(ids.iter().any(|id| id == expected), "{query}: {ids:?}");
            assert_eq!(ids.first().map(String::as_str), Some(expected), "{query}: {ids:?}");
        }
        assert!(top_ids("how do I port forward", 3).contains(&String::from("connection-command")));
        assert!(search("zzzz qqqq", 3).is_empty());
        assert!(search("how do I", 3).is_empty(), "stop words alone match nothing");
    }

    #[test]
    fn the_tool_answer_is_bounded_and_names_the_action() {
        let answer = search_app_help("mcp claude");
        assert!(answer.len() <= MAX_RESULT_CHARS, "{}", answer.len());
        assert!(answer.starts_with("## MCP server"), "{answer}");
        assert!(answer.contains("UI action: open_settings:mcp"));
        for query in ["safety rules", "mongodb", "redis keys ttl", "tls certificate", "the"] {
            assert!(search_app_help(query).len() <= MAX_RESULT_CHARS, "{query}");
        }
        assert!(search_app_help("  ").contains("Pass `query`"));
        assert!(search_app_help("zzzz").starts_with("No help section matches"));
    }

    /// The ids of `APP_ACTION_IDS` in the frontend registry (`src/app/appActions.ts`).
    fn registry_ids() -> Vec<String> {
        let source = include_str!("../../../src/app/appActions.ts");
        let start = source.find("APP_ACTION_IDS = [").expect("appActions.ts declares APP_ACTION_IDS");
        let list = &source[start..];
        let list = &list[..list.find(']').expect("APP_ACTION_IDS is closed")];
        list.split('"').skip(1).step_by(2).map(str::to_owned).collect()
    }

    #[test]
    fn the_pack_actions_match_the_frontend_registry() {
        let actions = actions();
        assert!(actions.len() >= 10, "{actions:?}");
        for (id, title) in &actions {
            let (verb, target) = id.split_once(':').unwrap_or_default();
            assert!(
                matches!(verb, "open_dialog" | "open_view" | "open_settings" | "run"),
                "{id}: unknown verb"
            );
            assert!(target.chars().all(|c| c.is_ascii_lowercase() || c == '_'), "{id}");
            assert_eq!(action_title(id), Some(*title));
        }
        assert_eq!(action_title("run:drop_database"), None);
        let mut registry = registry_ids();
        registry.sort_unstable();
        let pack: Vec<String> = actions.iter().map(|(id, _)| (*id).to_owned()).collect();
        assert_eq!(registry, pack, "src/app/appActions.ts and docs/app-help.yaml name the same actions");
    }

    #[test]
    fn the_capability_summary_lists_every_group_within_budget() {
        let summary = capability_summary();
        assert!(summary.len() <= MAX_SUMMARY_CHARS, "{} chars:\n{summary}", summary.len());
        assert!(summary.len() / 4 <= 450, "about {} tokens", summary.len() / 4);
        for entry in &pack().entries {
            assert!(summary.contains(&format!("- {}: ", entry.group)), "{}", entry.group);
        }
        assert!(summary.contains("search_app_help"));
        assert!(summary.contains("- Workspaces and safety: Workspaces and the rail; "), "{summary}");
        assert!(summary.contains("- MongoDB: Collection workspace; "), "{summary}");
        assert!(summary.contains("- Vector search: Vector search; "), "{summary}");
    }

    #[test]
    fn tokens_drop_stop_words_and_stem() {
        assert_eq!(tokens("How do I port-forward?"), ["port", "forward"]);
        assert_eq!(tokens("Forwarding SSH keys"), ["forward", "ssh", "key"]);
        assert_eq!(stem("queries"), "query");
        assert_eq!(stem("class"), "class");
    }
}
