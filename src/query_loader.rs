//! Runtime Tree-sitter query loader.
//!
//! Loads every Helix-style query file (`highlights`, `locals`, `injections`,
//! `indents`, `textobjects`, `rainbows`, `tags`, `folds`) and resolves
//! `;; inherits: parent` headers the same way Helix does. Child rules are
//! appended after parents so more specific patterns win.

use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
};

/// Query filenames subject0 executes. `folds` is included even though older
/// checkouts only shipped the other seven.
pub const QUERY_FILES: &[&str] = &[
    "highlights.scm",
    "locals.scm",
    "injections.scm",
    "indents.scm",
    "textobjects.scm",
    "rainbows.scm",
    "tags.scm",
    "folds.scm",
];

/// One language's full query pack. Empty strings mean "no query of that kind".
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct QueryPack {
    pub highlights: String,
    pub locals: String,
    pub injections: String,
    pub indents: String,
    pub textobjects: String,
    pub rainbows: String,
    pub tags: String,
    pub folds: String,
}

impl QueryPack {
    pub fn get(&self, filename: &str) -> &str {
        match filename {
            "highlights.scm" => &self.highlights,
            "locals.scm" => &self.locals,
            "injections.scm" => &self.injections,
            "indents.scm" => &self.indents,
            "textobjects.scm" => &self.textobjects,
            "rainbows.scm" => &self.rainbows,
            "tags.scm" => &self.tags,
            "folds.scm" => &self.folds,
            _ => "",
        }
    }

    pub fn is_empty(&self) -> bool {
        self.highlights.trim().is_empty()
            && self.locals.trim().is_empty()
            && self.injections.trim().is_empty()
            && self.indents.trim().is_empty()
            && self.textobjects.trim().is_empty()
            && self.rainbows.trim().is_empty()
            && self.tags.trim().is_empty()
            && self.folds.trim().is_empty()
    }
}

/// Locate the queries root. `SUBJECT0_QUERIES_DIR` wins, then paths next to
/// the executable, then the working tree, then the user config/data dirs.
pub fn find_queries_root() -> Option<PathBuf> {
    if let Ok(env_path) = std::env::var("SUBJECT0_QUERIES_DIR") {
        let p = PathBuf::from(env_path);
        if p.is_dir() {
            return Some(p);
        }
    }

    if let Ok(mut exe) = std::env::current_exe() {
        exe.pop();
        let candidates = [
            exe.join("queries"),
            exe.join("../queries"),
            exe.join("../../queries"),
            exe.join("../share/subject0/queries"),
        ];
        for c in candidates {
            if c.is_dir() {
                return Some(c);
            }
        }
    }

    let config = crate::lsp::subject0_config_dir().join("queries");
    let data = crate::lsp::subject0_data_dir().join("queries");
    let dev_and_sys = [
        PathBuf::from("./queries"),
        PathBuf::from("./src/queries"),
        PathBuf::from("./runtime/queries"),
        config,
        data,
        PathBuf::from("/usr/share/subject0/queries"),
    ];
    for dir in dev_and_sys {
        if dir.is_dir() {
            return Some(dir);
        }
    }
    None
}

/// Load one query file, resolving `;; inherits:` recursively.
pub fn load_query_file(lang_name: &str, filename: &str) -> String {
    let Some(root) = find_queries_root() else {
        return String::new();
    };
    let mut visited = HashSet::new();
    let mut acc = String::new();
    load_recursive(&root, lang_name, filename, &mut visited, &mut acc);
    acc
}

/// Load every query kind for a language.
pub fn load_query_pack(lang_name: &str) -> QueryPack {
    QueryPack {
        highlights: load_query_file(lang_name, "highlights.scm"),
        locals: load_query_file(lang_name, "locals.scm"),
        injections: load_query_file(lang_name, "injections.scm"),
        indents: load_query_file(lang_name, "indents.scm"),
        textobjects: load_query_file(lang_name, "textobjects.scm"),
        rainbows: load_query_file(lang_name, "rainbows.scm"),
        tags: load_query_file(lang_name, "tags.scm"),
        folds: load_query_file(lang_name, "folds.scm"),
    }
}

fn load_recursive(
    root: &Path,
    lang_name: &str,
    filename: &str,
    visited: &mut HashSet<String>,
    acc: &mut String,
) {
    let key = format!("{lang_name}/{filename}");
    if !visited.insert(key) {
        return;
    }
    let path = root.join(lang_name).join(filename);
    let Ok(content) = fs::read_to_string(&path) else {
        return;
    };
    for parent in inherit_parents(&content) {
        load_recursive(root, parent, filename, visited, acc);
    }
    acc.push('\n');
    acc.push_str(&content);
    acc.push('\n');
}

/// Parse `;; inherits: a, b` from the header (first 12 lines).
pub fn inherit_parents(content: &str) -> Vec<&str> {
    let mut parents = Vec::new();
    for line in content.lines().take(12) {
        let trimmed = line.trim();
        let Some(rest) = trimmed.strip_prefix(';') else {
            if !trimmed.is_empty() && !trimmed.starts_with(';') {
                break;
            }
            continue;
        };
        let rest = rest.trim_start_matches(';').trim();
        if let Some(parent_str) = rest.strip_prefix("inherits:") {
            for parent in parent_str.split(',') {
                let parent = parent.trim();
                if !parent.is_empty() {
                    parents.push(parent);
                }
            }
        }
    }
    parents
}

/// Capture names (`@function.method`) in source order, without the `@`.
pub fn capture_names(query_src: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut seen = HashSet::new();
    let bytes = query_src.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'"' {
            i += 1;
            while i < bytes.len() && bytes[i] != b'"' {
                if bytes[i] == b'\\' {
                    i += 1;
                }
                i += 1;
            }
            i += 1;
            continue;
        }
        if bytes[i] == b';' {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if bytes[i] == b'@' {
            let start = i + 1;
            let mut end = start;
            while end < bytes.len() && is_capture_char(bytes[end]) {
                end += 1;
            }
            if end > start {
                let name = &query_src[start..end];
                if seen.insert(name.to_string()) {
                    names.push(name.to_string());
                }
            }
            i = end;
            continue;
        }
        i += 1;
    }
    names
}

fn is_capture_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'-' || b == b'.' || b == b'#'
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inherits_header_parses_multiple_parents() {
        let src = ";; inherits: ecma, jsx\n;; comment\n(identifier) @variable\n";
        assert_eq!(inherit_parents(src), vec!["ecma", "jsx"]);
    }

    #[test]
    fn inherits_ignores_body_comments() {
        let src = "(identifier) @variable\n;; inherits: should-not\n";
        assert!(inherit_parents(src).is_empty());
    }

    #[test]
    fn capture_names_skip_strings_and_comments() {
        let src = r#"
            ; @not_a_capture
            (string) @string
            (#eq? @string "foo@bar")
            (call function: (identifier) @function.method)
        "#;
        let names = capture_names(src);
        assert_eq!(names, vec!["string".to_string(), "function.method".to_string()]);
    }

    #[test]
    fn capture_names_are_unique() {
        let src = "(a) @keyword\n(b) @keyword\n(c) @type\n";
        assert_eq!(capture_names(src), vec!["keyword".to_string(), "type".to_string()]);
    }
}
