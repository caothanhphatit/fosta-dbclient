//! [`CompareOptions`] resolved against the two snapshots: whether names fold case, which names are excluded and which
//! kinds are compared.

use crate::contract::{CompareOptions, SchemaSnapshot};

/// The options in force for one compare.
#[derive(Clone, Debug)]
pub struct ResolvedOptions<'a> {
    pub options: &'a CompareOptions,
    /// Names match case-insensitively (the option, or either snapshot folding case).
    pub case_insensitive: bool,
}

impl<'a> ResolvedOptions<'a> {
    pub fn new(options: &'a CompareOptions, source: &SchemaSnapshot, target: &SchemaSnapshot) -> Self {
        let case_insensitive = options
            .case_insensitive_names
            .unwrap_or(source.case_insensitive_names || target.case_insensitive_names);
        Self { options, case_insensitive }
    }

    /// The key a name is matched by.
    pub fn fold(&self, name: &str) -> String {
        if self.case_insensitive {
            name.to_lowercase()
        } else {
            name.to_owned()
        }
    }

    /// Whether an object named `name` is left out by an `exclude` pattern.
    pub fn excluded(&self, name: &str) -> bool {
        self.options.exclude.iter().any(|pattern| glob_match(pattern, name))
    }

    /// Whether objects of `kind` (`table`, `view`) are compared.
    pub fn includes(&self, kind: &str) -> bool {
        self.options.include_kinds.iter().any(|k| k.eq_ignore_ascii_case(kind))
    }
}

/// Case-insensitive glob match: `*` is any run of characters, `?` one character.
pub fn glob_match(pattern: &str, text: &str) -> bool {
    let pattern: Vec<char> = pattern.to_lowercase().chars().collect();
    let text: Vec<char> = text.to_lowercase().chars().collect();
    let (mut p, mut t) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while t < text.len() {
        if p < pattern.len() && (pattern[p] == '?' || pattern[p] == text[t]) {
            p += 1;
            t += 1;
        } else if p < pattern.len() && pattern[p] == '*' {
            star = Some((p, t));
            p += 1;
        } else if let Some((star_p, star_t)) = star {
            p = star_p + 1;
            t = star_t + 1;
            star = Some((star_p, star_t + 1));
        } else {
            return false;
        }
    }
    pattern[p..].iter().all(|c| *c == '*')
}
