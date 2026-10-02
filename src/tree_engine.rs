//! Tree-sitter query runners for every `.scm` kind subject0 ships.
//!
//! Highlight injection/locals are applied by `SyntaxEngine` through
//! `tree-sitter-highlight`. This module runs the other query files against
//! the concrete syntax tree:
//!
//! - `indents.scm` — indent width for a new line
//! - `textobjects.scm` — around/inside selections
//! - `rainbows.scm` — bracket depth
//! - `tags.scm` — fallback document symbols
//! - `folds.scm` — fold ranges

use std::collections::HashMap;
use std::sync::Mutex;

use tree_sitter::{Language, Node, Query, QueryCursor, StreamingIterator, Tree};

fn cached_query(language: &Language, src: &str) -> Option<&'static Query> {
    static CACHE: Mutex<Option<HashMap<String, &'static Query>>> = Mutex::new(None);
    let mut guard = CACHE.lock().ok()?;
    if guard.is_none() {
        *guard = Some(HashMap::new());
    }
    let cache = guard.as_mut()?;
    let key = src.to_string();
    if let Some(query) = cache.get(&key) {
        return Some(*query);
    }
    let query = Query::new(language, src).ok()?;
    let leaked: &'static Query = Box::leak(Box::new(query));
    cache.insert(key, leaked);
    Some(leaked)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ByteRange {
    pub start: usize,
    pub end: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagSymbol {
    pub name: String,
    pub kind: String,
    pub start_line: usize,
    pub start_col: usize,
    pub end_line: usize,
    pub end_col: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FoldRange {
    pub start_line: usize,
    pub end_line: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RainbowSpan {
    pub start: usize,
    pub end: usize,
    pub depth: u8,
}

/// Suggested indent for `line` (0-based), in columns of `indent_width` spaces.
pub fn suggest_indent(
    language: &Language,
    source: &str,
    tree: &Tree,
    query_src: &str,
    line: usize,
    indent_width: usize,
) -> Option<String> {
    let line_start = line_byte(source, line)?;
    suggest_indent_at_byte(language, source, tree, query_src, line_start, indent_width)
}

/// Suggested indent for a byte offset that will become the new line start.
pub fn suggest_indent_at_byte(
    language: &Language,
    source: &str,
    tree: &Tree,
    query_src: &str,
    byte_pos: usize,
    indent_width: usize,
) -> Option<String> {
    if query_src.trim().is_empty() {
        return None;
    }
    let query = cached_query(language, query_src)?;
    indent_from_query(query, source, tree, byte_pos, indent_width)
}

fn indent_from_query(
    query: &Query,
    source: &str,
    tree: &Tree,
    byte_pos: usize,
    indent_width: usize,
) -> Option<String> {
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(query, tree.root_node(), source.as_bytes());
    let mut level: i32 = 0;
    while let Some(m) = matches.next() {
        for cap in m.captures {
            let name = query.capture_names()[cap.index as usize];
            let node = cap.node;
            if name == "indent" || name == "indent.always" {
                if node.start_byte() < byte_pos && node.end_byte() > byte_pos {
                    level += 1;
                }
            } else if name == "outdent" || name == "outdent.always" {
                if node.start_byte() >= byte_pos && node.start_position().row == byte_line(source, byte_pos) {
                    level -= 1;
                }
            }
        }
    }
    let level = level.max(0) as usize;
    Some(" ".repeat(level.saturating_mul(indent_width.max(1))))
}

fn byte_line(source: &str, byte: usize) -> usize {
    source.bytes().take(byte).filter(|b| *b == b'\n').count()
}

/// Smallest textobject of `object` (for example `function`) covering `byte_pos`.
pub fn textobject_at(
    language: &Language,
    source: &str,
    tree: &Tree,
    query_src: &str,
    byte_pos: usize,
    object: &str,
    inside: bool,
) -> Option<ByteRange> {
    if query_src.trim().is_empty() {
        return None;
    }
    let query = cached_query(language, query_src)?;
    let want = if inside {
        format!("{object}.inside")
    } else {
        format!("{object}.around")
    };
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(query, tree.root_node(), source.as_bytes());
    let mut best: Option<ByteRange> = None;
    while let Some(m) = matches.next() {
        for cap in m.captures {
            let name = query.capture_names()[cap.index as usize];
            if name != want {
                continue;
            }
            let node = cap.node;
            if byte_pos < node.start_byte() || byte_pos > node.end_byte() {
                continue;
            }
            let range = ByteRange {
                start: node.start_byte(),
                end: node.end_byte(),
            };
            best = Some(match best {
                Some(prev) if range_len(prev) <= range_len(range) => prev,
                _ => range,
            });
        }
    }
    best
}

pub fn rainbow_spans(
    language: &Language,
    source: &str,
    tree: &Tree,
    query_src: &str,
) -> Vec<RainbowSpan> {
    if query_src.trim().is_empty() {
        return Vec::new();
    }
    let Some(query) = cached_query(language, query_src) else {
        return Vec::new();
    };
    let mut cursor = QueryCursor::new();
    let mut scopes: Vec<(usize, usize)> = Vec::new();
    let mut matches = cursor.matches(query, tree.root_node(), source.as_bytes());
    while let Some(m) = matches.next() {
        for cap in m.captures {
            let name = query.capture_names()[cap.index as usize];
            if name.starts_with("rainbow") {
                scopes.push((cap.node.start_byte(), cap.node.end_byte()));
            }
        }
    }
    scopes.sort_by_key(|s| (s.0, s.1));
    let mut spans = Vec::new();
    let bytes = source.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if is_bracket(b) {
            let depth = scopes.iter().filter(|s| i >= s.0 && i < s.1).count();
            if depth > 0 {
                spans.push(RainbowSpan {
                    start: i,
                    end: i + 1,
                    depth: depth.min(6) as u8,
                });
            }
        }
        i += 1;
    }
    spans
}

pub fn collect_tags(
    language: &Language,
    source: &str,
    tree: &Tree,
    query_src: &str,
) -> Vec<TagSymbol> {
    if query_src.trim().is_empty() {
        return Vec::new();
    }
    let Some(query) = cached_query(language, query_src) else {
        return Vec::new();
    };
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(query, tree.root_node(), source.as_bytes());
    let mut tags = Vec::new();
    while let Some(m) = matches.next() {
        let mut name = None;
        let mut def_node: Option<Node> = None;
        let mut kind = String::new();
        for cap in m.captures {
            let cap_name = query.capture_names()[cap.index as usize];
            if cap_name == "name" {
                name = source
                    .get(cap.node.start_byte()..cap.node.end_byte())
                    .map(str::to_string);
            } else if cap_name.starts_with("definition.") {
                kind = cap_name.trim_start_matches("definition.").to_string();
                def_node = Some(cap.node);
            }
        }
        if let (Some(name), Some(node)) = (name, def_node) {
            tags.push(TagSymbol {
                name,
                kind,
                start_line: node.start_position().row as usize,
                start_col: node.start_position().column as usize,
                end_line: node.end_position().row as usize,
                end_col: node.end_position().column as usize,
            });
        }
    }
    tags.sort_by_key(|t| (t.start_line, t.start_col));
    tags
}

pub fn collect_folds(
    language: &Language,
    source: &str,
    tree: &Tree,
    query_src: &str,
) -> Vec<FoldRange> {
    if query_src.trim().is_empty() {
        return Vec::new();
    }
    let Some(query) = cached_query(language, query_src) else {
        return Vec::new();
    };
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(query, tree.root_node(), source.as_bytes());
    let mut folds = Vec::new();
    while let Some(m) = matches.next() {
        for cap in m.captures {
            let name = query.capture_names()[cap.index as usize];
            if name != "fold" && !name.starts_with("fold.") {
                continue;
            }
            let start = cap.node.start_position().row as usize;
            let end = cap.node.end_position().row as usize;
            if end > start {
                folds.push(FoldRange {
                    start_line: start,
                    end_line: end,
                });
            }
        }
    }
    folds.sort_by_key(|f| (f.start_line, f.end_line));
    folds.dedup();
    folds
}

pub fn byte_to_point(source: &str, byte: usize) -> (usize, usize) {
    let byte = byte.min(source.len());
    let mut line = 0usize;
    let mut col = 0usize;
    for (i, ch) in source.char_indices() {
        if i >= byte {
            break;
        }
        if ch == '\n' {
            line += 1;
            col = 0;
        } else {
            col += 1;
        }
    }
    (line, col)
}

fn line_byte(source: &str, line: usize) -> Option<usize> {
    if line == 0 {
        return Some(0);
    }
    let mut seen = 0usize;
    for (i, b) in source.bytes().enumerate() {
        if b == b'\n' {
            seen += 1;
            if seen == line {
                return Some(i + 1);
            }
        }
    }
    None
}

fn range_len(r: ByteRange) -> usize {
    r.end.saturating_sub(r.start)
}

fn is_bracket(b: u8) -> bool {
    matches!(b, b'(' | b')' | b'[' | b']' | b'{' | b'}' | b'<' | b'>')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rust_lang() -> Language {
        tree_sitter_rust::LANGUAGE.into()
    }

    fn parse(src: &str) -> Tree {
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&rust_lang()).unwrap();
        parser.parse(src, None).unwrap()
    }

    #[test]
    fn indent_query_increases_inside_block() {
        let src = "fn main() {\nlet x = 1;\n}\n";
        let tree = parse(src);
        let q = r#"
            (block) @indent
            (function_item) @indent
        "#;
        let indent = suggest_indent(&rust_lang(), src, &tree, q, 1, 4).unwrap();
        assert_eq!(indent, "        ");
    }

    #[test]
    fn empty_indent_query_returns_none() {
        let src = "fn main() {}\n";
        let tree = parse(src);
        assert!(suggest_indent(&rust_lang(), src, &tree, "  ", 0, 4).is_none());
    }

    #[test]
    fn textobject_selects_function_around() {
        let src = "fn main() {\n  let x = 1;\n}\n";
        let tree = parse(src);
        let q = "(function_item) @function.around";
        let range = textobject_at(&rust_lang(), src, &tree, q, 4, "function", false).unwrap();
        assert_eq!(&src[range.start..range.end], src.trim_end());
    }

    #[test]
    fn tags_collect_function_name() {
        let src = "fn answer() -> i32 { 42 }\n";
        let tree = parse(src);
        let q = "(function_item name: (identifier) @name) @definition.function";
        let tags = collect_tags(&rust_lang(), src, &tree, q);
        assert_eq!(tags.len(), 1);
        assert_eq!(tags[0].name, "answer");
        assert_eq!(tags[0].kind, "function");
    }

    #[test]
    fn folds_span_multiple_lines() {
        let src = "fn main() {\n  let x = 1;\n}\n";
        let tree = parse(src);
        let q = "(function_item) @fold";
        let folds = collect_folds(&rust_lang(), src, &tree, q);
        assert_eq!(folds.len(), 1);
        assert_eq!(folds[0].start_line, 0);
        assert!(folds[0].end_line >= 2);
    }

    #[test]
    fn rainbow_marks_nested_braces() {
        let src = "fn main() { { } }\n";
        let tree = parse(src);
        let q = "(block) @rainbow.scope";
        let spans = rainbow_spans(&rust_lang(), src, &tree, q);
        assert!(spans.len() >= 2);
        assert!(spans.iter().any(|s| s.depth >= 2));
    }

    #[test]
    fn byte_to_point_counts_lines() {
        assert_eq!(byte_to_point("ab\ncd", 4), (1, 1));
        assert_eq!(byte_to_point("ab\ncd", 0), (0, 0));
    }
}
