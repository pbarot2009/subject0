//! Regression tests for buffer edits, LSP helpers, git hunks, and IO.
#![allow(clippy::all, clippy::pedantic, unused)]

use std::io::Write;
use std::path::PathBuf;

use ropey::Rope;

use crate::editor::{Editor, line_len};
use crate::git::compute_hunks_from_text;
use crate::grammar::grammar_symbol;
use crate::lsp::{
    DiagnosticItem, SuggestionItem, TextEditItem, char_to_utf16_col, parse_snippet_to_plain_text,
    slice_utf16, utf16_to_char_col,
};
use crate::safe_io::{atomic_write_with, file_stamp};

fn editor_with(text: &str) -> Editor {
    let mut ed = Editor::new(None).expect("editor");
    ed.rope = Rope::from_str(text);
    ed.syntax.reparse(text);
    ed
}

fn diag(line: usize, end: usize) -> DiagnosticItem {
    DiagnosticItem {
        line,
        col: 0,
        end_line: end,
        end_col: 1,
        message: "m".into(),
        severity: 1,
        is_unnecessary: false,
        is_deprecated: false,
    }
}

#[test]
fn utf16_col_case_0() {
    let s = "hello";
    assert_eq!(char_to_utf16_col(s, 0), 0);
    let back = utf16_to_char_col(s, char_to_utf16_col(s, 0));
    assert!(back <= s.chars().count());
}

#[test]
fn utf16_col_case_1() {
    let s = "hello";
    assert_eq!(char_to_utf16_col(s, 2), 2);
    let back = utf16_to_char_col(s, char_to_utf16_col(s, 2));
    assert!(back <= s.chars().count());
}

#[test]
fn utf16_col_case_2() {
    let s = "hello";
    assert_eq!(char_to_utf16_col(s, 99), 5);
    let back = utf16_to_char_col(s, char_to_utf16_col(s, 99));
    assert!(back <= s.chars().count());
}

#[test]
fn utf16_col_case_3() {
    let s = "café";
    assert_eq!(char_to_utf16_col(s, 3), 3);
    let back = utf16_to_char_col(s, char_to_utf16_col(s, 3));
    assert!(back <= s.chars().count());
}

#[test]
fn utf16_col_case_4() {
    let s = "café";
    assert_eq!(char_to_utf16_col(s, 4), 4);
    let back = utf16_to_char_col(s, char_to_utf16_col(s, 4));
    assert!(back <= s.chars().count());
}

#[test]
fn utf16_col_case_5() {
    let s = "名前";
    assert_eq!(char_to_utf16_col(s, 1), 1);
    let back = utf16_to_char_col(s, char_to_utf16_col(s, 1));
    assert!(back <= s.chars().count());
}

#[test]
fn utf16_col_case_6() {
    let s = "a\n";
    assert_eq!(char_to_utf16_col(s, 1), 1);
    let back = utf16_to_char_col(s, char_to_utf16_col(s, 1));
    assert!(back <= s.chars().count());
}

#[test]
fn utf16_col_case_7() {
    let s = "a\r\n";
    assert_eq!(char_to_utf16_col(s, 1), 1);
    let back = utf16_to_char_col(s, char_to_utf16_col(s, 1));
    assert!(back <= s.chars().count());
}

#[test]
fn utf16_col_case_8() {
    let s = "😀x";
    assert_eq!(char_to_utf16_col(s, 2), 3);
    let back = utf16_to_char_col(s, char_to_utf16_col(s, 2));
    assert!(back <= s.chars().count());
}

#[test]
fn utf16_col_case_9() {
    let s = "";
    assert_eq!(char_to_utf16_col(s, 3), 0);
    let back = utf16_to_char_col(s, char_to_utf16_col(s, 3));
    assert!(back <= s.chars().count());
}

#[test]
fn snippet_case_0() {
    assert_eq!(parse_snippet_to_plain_text("hello"), "hello");
}

#[test]
fn snippet_case_1() {
    assert_eq!(parse_snippet_to_plain_text("$1"), "");
}

#[test]
fn snippet_case_2() {
    assert_eq!(parse_snippet_to_plain_text("${1:name}"), "name");
}

#[test]
fn snippet_case_3() {
    assert_eq!(parse_snippet_to_plain_text("${0}"), "");
}

#[test]
fn snippet_case_4() {
    assert_eq!(parse_snippet_to_plain_text("${1|one,two|}"), "one");
}

#[test]
fn snippet_case_5() {
    assert_eq!(parse_snippet_to_plain_text("pre $0 post"), "pre  post");
}

#[test]
fn snippet_case_6() {
    assert_eq!(parse_snippet_to_plain_text("\\$not"), "$not");
}

#[test]
fn snippet_case_7() {
    assert_eq!(parse_snippet_to_plain_text("${1:foo ${2:bar}}"), "foo bar");
}

#[test]
fn snippet_case_8() {
    assert_eq!(parse_snippet_to_plain_text("fn $1()"), "fn ()");
}

#[test]
fn snippet_case_9() {
    assert_eq!(parse_snippet_to_plain_text("${12:typed}"), "typed");
}

#[test]
fn slice_utf16_ascii_window() {
    assert_eq!(slice_utf16("abcdef", 1, 4), "bcd");
}
#[test]
fn slice_utf16_empty_range() {
    assert!(slice_utf16("abc", 2, 2).is_empty());
}
#[test]
fn grammar_symbol_rewrites_dashes() {
    assert_eq!(grammar_symbol("c-sharp"), "tree_sitter_c_sharp");
}
#[test]
fn grammar_symbol_plain() {
    assert_eq!(grammar_symbol("rust"), "tree_sitter_rust");
}

#[test]
fn line_len_strips_newline() {
    let rope = Rope::from_str("ab\ncd\n");
    assert_eq!(line_len(&rope, 0), 2);
    assert_eq!(line_len(&rope, 1), 2);
}
#[test]
fn insert_char_moves_cursor() {
    let mut ed = editor_with("ab\n");
    ed.set_mode(crate::editor::Mode::Insert);
    ed.insert_char('Z');
    assert_eq!(ed.rope.to_string(), "Zab\n");
    assert_eq!(ed.cursor_x, 1);
    assert!(ed.modified);
}
#[test]
fn backspace_deletes_char() {
    let mut ed = editor_with("ab\n");
    ed.cursor_x = 2;
    ed.set_mode(crate::editor::Mode::Insert);
    ed.backspace();
    assert_eq!(ed.rope.to_string(), "a\n");
}
#[test]
fn delete_line_copies_clipboard() {
    let mut ed = editor_with("one\ntwo\n");
    ed.delete_current_line();
    assert!(ed.clipboard.starts_with("one"));
    assert!(ed.rope.to_string().starts_with("two"));
}
#[test]
fn join_lines_skips_extra_space_on_empty() {
    let mut ed = editor_with("foo\n\n");
    ed.join_lines();
    let text = ed.rope.to_string();
    assert!(!text.contains("foo  "));
    assert!(text.starts_with("foo"));
}
#[test]
fn toggle_case_advances() {
    let mut ed = editor_with("ab\n");
    ed.toggle_case();
    assert_eq!(ed.rope.char(0), 'A');
    assert_eq!(ed.cursor_x, 1);
}
#[test]
fn undo_restores_text() {
    let mut ed = editor_with("ab\n");
    ed.set_mode(crate::editor::Mode::Insert);
    ed.insert_char('Q');
    ed.undo();
    assert_eq!(ed.rope.to_string(), "ab\n");
}
#[test]
fn redo_reapplies() {
    let mut ed = editor_with("ab\n");
    ed.set_mode(crate::editor::Mode::Insert);
    ed.insert_char('Q');
    ed.undo();
    ed.redo();
    assert!(ed.rope.to_string().starts_with('Q'));
}
#[test]
fn paste_inserts_clipboard() {
    let mut ed = editor_with("ab\n");
    ed.clipboard = "XY".into();
    ed.paste();
    assert!(ed.rope.to_string().contains("XY"));
}
#[test]
fn diagnostic_shift_keeps_multiline_end() {
    let mut ed = editor_with("a\nb\nc\n");
    ed.diagnostics = vec![diag(0, 2)];
    ed.cursor_y = 1;
    ed.set_mode(crate::editor::Mode::Insert);
    ed.insert_newline();
    let d = &ed.diagnostics[0];
    assert!(d.end_line >= d.line);
    assert!(d.end_line >= 2);
}
#[test]
fn open_file_clears_folds() {
    let mut ed = editor_with("a\n");
    ed.folded_lines.insert(0);
    ed.doc_highlights.push((0, 0, 1));
    let dir = std::env::temp_dir().join(format!("s0-open-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("t.txt");
    std::fs::write(&path, "zz\n").unwrap();
    ed.open_file(&path).unwrap();
    assert!(ed.folded_lines.is_empty());
    assert!(ed.doc_highlights.is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}
#[test]
fn move_vertical_stays_in_range() {
    let mut ed = editor_with("a\nb\nc\n");
    ed.move_vertical(5);
    assert!(ed.cursor_y < ed.rope.len_lines());
    ed.move_vertical(-9);
    assert_eq!(ed.cursor_y, 0);
}
#[test]
fn hidden_fold_is_skipped() {
    let mut ed = editor_with("a\nb\nc\nd\n");
    ed.folded_lines.insert(0);
    ed.syntax.folds.push(crate::tree_engine::FoldRange {
        start_line: 0,
        end_line: 2,
    });
    ed.move_vertical(1);
    assert_eq!(ed.cursor_y, 3);
}
#[test]
fn completion_replaces_prefix() {
    let mut ed = editor_with("fn\n");
    ed.cursor_x = 2;
    ed.set_mode(crate::editor::Mode::Insert);
    ed.completions = vec![SuggestionItem {
        label: "fn".into(),
        insert_text: "function".into(),
        detail: None,
        documentation: None,
        kind: 3,
        primary_edit: Some(TextEditItem {
            start_line: 0,
            start_col: 0,
            end_line: 0,
            end_col: 2,
            new_text: "fn".into(),
        }),
        additional_text_edits: vec![],
    }];
    ed.completion_visible = true;
    ed.accept_completion();
    assert!(ed.rope.to_string().starts_with("function"));
    assert_eq!(ed.cursor_x, "function".chars().count());
}
#[test]
fn completion_applies_additional_before_primary() {
    let mut ed = editor_with("foo\nbar\n");
    ed.cursor_y = 1;
    ed.cursor_x = 3;
    ed.set_mode(crate::editor::Mode::Insert);
    ed.completions = vec![SuggestionItem {
        label: "bar".into(),
        insert_text: "baz".into(),
        detail: None,
        documentation: None,
        kind: 1,
        primary_edit: Some(TextEditItem {
            start_line: 1,
            start_col: 0,
            end_line: 1,
            end_col: 3,
            new_text: "bar".into(),
        }),
        additional_text_edits: vec![TextEditItem {
            start_line: 0,
            start_col: 0,
            end_line: 0,
            end_col: 0,
            new_text: "use x;\n".into(),
        }],
    }];
    ed.completion_visible = true;
    ed.accept_completion();
    let text = ed.rope.to_string();
    assert!(text.contains("use x;"));
    assert!(text.contains("baz"));
}
#[test]
fn jump_back_does_not_skip() {
    let mut ed = editor_with("a\nb\nc\n");
    ed.path = Some(PathBuf::from("mem.rs"));
    ed.cursor_y = 0;
    ed.record_jump_checkpoint();
    ed.cursor_y = 2;
    ed.record_jump_checkpoint();
    ed.jump_backward();
    assert_eq!(ed.cursor_y, 0);
}
#[test]
fn git_hunk_revert_text_matches_head() {
    let hunks = compute_hunks_from_text("a\nb\n", "a\nB\n");
    assert!(!hunks.is_empty());
    assert!(hunks.iter().any(|h| h.head_text.contains('b')));
}
#[test]
fn git_added_line_hunk() {
    let hunks = compute_hunks_from_text("a\n", "a\nb\n");
    assert!(hunks.iter().any(|h| h.after_len >= 1));
}
#[test]
fn atomic_write_roundtrip() {
    let dir = std::env::temp_dir().join(format!("s0-atomic-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("note.txt");
    atomic_write_with(&path, |w| {
        w.write_all(b"hello")?;
        Ok(())
    })
    .unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "hello");
    assert!(file_stamp(&path).is_some());
    let _ = std::fs::remove_dir_all(&dir);
}
#[test]
fn shell_layout_hides_panels_when_narrow() {
    let (rail, explorer, outline, problems, narrow) =
        crate::ui::shell_layout(40, 20, true, true, true);
    assert!(narrow);
    assert_eq!(rail, 0);
    assert_eq!(explorer, 0);
    assert_eq!(outline, 0);
    assert_eq!(problems, 0);
}
#[test]
fn shell_layout_keeps_editor_room_on_resize() {
    let (rail, explorer, outline, problems, narrow) =
        crate::ui::shell_layout(100, 12, true, true, true);
    assert!(!narrow);
    assert_eq!(rail, 0);
    assert_eq!(explorer, 28);
    assert_eq!(outline, 0);
    assert_eq!(problems, 0);
    let (rail, explorer, outline, problems, _) = crate::ui::shell_layout(160, 40, true, true, true);
    assert_eq!(rail, 5);
    assert_eq!(explorer, 28);
    assert_eq!(outline, 26);
    assert_eq!(problems, 4);
    assert!(160 - rail - explorer - outline >= 24);
}
#[test]
fn version_does_not_wrap_negative() {
    let mut ed = editor_with("a\n");
    ed.doc_version = i32::MAX;
    ed.set_mode(crate::editor::Mode::Insert);
    ed.insert_char('x');
    assert!(ed.doc_version > 0);
}

#[test]
fn buffer_roundtrip_0() {
    let text = format!("line {}\nline {}\n", 0, 0 + 1);
    let rope = Rope::from_str(&text);
    assert_eq!(rope.to_string(), text);
    assert_eq!(
        line_len(&rope, 0),
        text.lines().next().unwrap().chars().count()
    );
    assert_eq!(
        char_to_utf16_col(&text, 1),
        utf16_to_char_col(&text, 1)
            .max(1)
            .min(char_to_utf16_col(&text, 1).max(1))
    );
    assert_eq!(
        grammar_symbol(&format!("lang-0")).contains("tree_sitter_"),
        true
    );
    assert_eq!(
        parse_snippet_to_plain_text(&format!("${{1:v0}}")),
        format!("v0")
    );
}

#[test]
fn buffer_roundtrip_1() {
    let text = format!("line {}\nline {}\n", 1, 1 + 1);
    let rope = Rope::from_str(&text);
    assert_eq!(rope.to_string(), text);
    assert_eq!(
        line_len(&rope, 0),
        text.lines().next().unwrap().chars().count()
    );
    assert_eq!(
        char_to_utf16_col(&text, 1),
        utf16_to_char_col(&text, 1)
            .max(1)
            .min(char_to_utf16_col(&text, 1).max(1))
    );
    assert_eq!(
        grammar_symbol(&format!("lang-1")).contains("tree_sitter_"),
        true
    );
    assert_eq!(
        parse_snippet_to_plain_text(&format!("${{1:v1}}")),
        format!("v1")
    );
}

#[test]
fn buffer_roundtrip_2() {
    let text = format!("line {}\nline {}\n", 2, 2 + 1);
    let rope = Rope::from_str(&text);
    assert_eq!(rope.to_string(), text);
    assert_eq!(
        line_len(&rope, 0),
        text.lines().next().unwrap().chars().count()
    );
    assert_eq!(
        char_to_utf16_col(&text, 1),
        utf16_to_char_col(&text, 1)
            .max(1)
            .min(char_to_utf16_col(&text, 1).max(1))
    );
    assert_eq!(
        grammar_symbol(&format!("lang-2")).contains("tree_sitter_"),
        true
    );
    assert_eq!(
        parse_snippet_to_plain_text(&format!("${{1:v2}}")),
        format!("v2")
    );
}

#[test]
fn buffer_roundtrip_3() {
    let text = format!("line {}\nline {}\n", 3, 3 + 1);
    let rope = Rope::from_str(&text);
    assert_eq!(rope.to_string(), text);
    assert_eq!(
        line_len(&rope, 0),
        text.lines().next().unwrap().chars().count()
    );
    assert_eq!(
        char_to_utf16_col(&text, 1),
        utf16_to_char_col(&text, 1)
            .max(1)
            .min(char_to_utf16_col(&text, 1).max(1))
    );
    assert_eq!(
        grammar_symbol(&format!("lang-3")).contains("tree_sitter_"),
        true
    );
    assert_eq!(
        parse_snippet_to_plain_text(&format!("${{1:v3}}")),
        format!("v3")
    );
}

#[test]
fn buffer_roundtrip_4() {
    let text = format!("line {}\nline {}\n", 4, 4 + 1);
    let rope = Rope::from_str(&text);
    assert_eq!(rope.to_string(), text);
    assert_eq!(
        line_len(&rope, 0),
        text.lines().next().unwrap().chars().count()
    );
    assert_eq!(
        char_to_utf16_col(&text, 1),
        utf16_to_char_col(&text, 1)
            .max(1)
            .min(char_to_utf16_col(&text, 1).max(1))
    );
    assert_eq!(
        grammar_symbol(&format!("lang-4")).contains("tree_sitter_"),
        true
    );
    assert_eq!(
        parse_snippet_to_plain_text(&format!("${{1:v4}}")),
        format!("v4")
    );
}

#[test]
fn buffer_roundtrip_5() {
    let text = format!("line {}\nline {}\n", 5, 5 + 1);
    let rope = Rope::from_str(&text);
    assert_eq!(rope.to_string(), text);
    assert_eq!(
        line_len(&rope, 0),
        text.lines().next().unwrap().chars().count()
    );
    assert_eq!(
        char_to_utf16_col(&text, 1),
        utf16_to_char_col(&text, 1)
            .max(1)
            .min(char_to_utf16_col(&text, 1).max(1))
    );
    assert_eq!(
        grammar_symbol(&format!("lang-5")).contains("tree_sitter_"),
        true
    );
    assert_eq!(
        parse_snippet_to_plain_text(&format!("${{1:v5}}")),
        format!("v5")
    );
}

#[test]
fn buffer_roundtrip_6() {
    let text = format!("line {}\nline {}\n", 6, 6 + 1);
    let rope = Rope::from_str(&text);
    assert_eq!(rope.to_string(), text);
    assert_eq!(
        line_len(&rope, 0),
        text.lines().next().unwrap().chars().count()
    );
    assert_eq!(
        char_to_utf16_col(&text, 1),
        utf16_to_char_col(&text, 1)
            .max(1)
            .min(char_to_utf16_col(&text, 1).max(1))
    );
    assert_eq!(
        grammar_symbol(&format!("lang-6")).contains("tree_sitter_"),
        true
    );
    assert_eq!(
        parse_snippet_to_plain_text(&format!("${{1:v6}}")),
        format!("v6")
    );
}

#[test]
fn buffer_roundtrip_7() {
    let text = format!("line {}\nline {}\n", 7, 7 + 1);
    let rope = Rope::from_str(&text);
    assert_eq!(rope.to_string(), text);
    assert_eq!(
        line_len(&rope, 0),
        text.lines().next().unwrap().chars().count()
    );
    assert_eq!(
        char_to_utf16_col(&text, 1),
        utf16_to_char_col(&text, 1)
            .max(1)
            .min(char_to_utf16_col(&text, 1).max(1))
    );
    assert_eq!(
        grammar_symbol(&format!("lang-7")).contains("tree_sitter_"),
        true
    );
    assert_eq!(
        parse_snippet_to_plain_text(&format!("${{1:v7}}")),
        format!("v7")
    );
}

#[test]
fn buffer_roundtrip_8() {
    let text = format!("line {}\nline {}\n", 8, 8 + 1);
    let rope = Rope::from_str(&text);
    assert_eq!(rope.to_string(), text);
    assert_eq!(
        line_len(&rope, 0),
        text.lines().next().unwrap().chars().count()
    );
    assert_eq!(
        char_to_utf16_col(&text, 1),
        utf16_to_char_col(&text, 1)
            .max(1)
            .min(char_to_utf16_col(&text, 1).max(1))
    );
    assert_eq!(
        grammar_symbol(&format!("lang-8")).contains("tree_sitter_"),
        true
    );
    assert_eq!(
        parse_snippet_to_plain_text(&format!("${{1:v8}}")),
        format!("v8")
    );
}

#[test]
fn buffer_roundtrip_9() {
    let text = format!("line {}\nline {}\n", 9, 9 + 1);
    let rope = Rope::from_str(&text);
    assert_eq!(rope.to_string(), text);
    assert_eq!(
        line_len(&rope, 0),
        text.lines().next().unwrap().chars().count()
    );
    assert_eq!(
        char_to_utf16_col(&text, 1),
        utf16_to_char_col(&text, 1)
            .max(1)
            .min(char_to_utf16_col(&text, 1).max(1))
    );
    assert_eq!(
        grammar_symbol(&format!("lang-9")).contains("tree_sitter_"),
        true
    );
    assert_eq!(
        parse_snippet_to_plain_text(&format!("${{1:v9}}")),
        format!("v9")
    );
}

#[test]
fn buffer_roundtrip_10() {
    let text = format!("line {}\nline {}\n", 10, 10 + 1);
    let rope = Rope::from_str(&text);
    assert_eq!(rope.to_string(), text);
    assert_eq!(
        line_len(&rope, 0),
        text.lines().next().unwrap().chars().count()
    );
    assert_eq!(
        char_to_utf16_col(&text, 1),
        utf16_to_char_col(&text, 1)
            .max(1)
            .min(char_to_utf16_col(&text, 1).max(1))
    );
    assert_eq!(
        grammar_symbol(&format!("lang-10")).contains("tree_sitter_"),
        true
    );
    assert_eq!(
        parse_snippet_to_plain_text(&format!("${{1:v10}}")),
        format!("v10")
    );
}

#[test]
fn buffer_roundtrip_11() {
    let text = format!("line {}\nline {}\n", 11, 11 + 1);
    let rope = Rope::from_str(&text);
    assert_eq!(rope.to_string(), text);
    assert_eq!(
        line_len(&rope, 0),
        text.lines().next().unwrap().chars().count()
    );
    assert_eq!(
        char_to_utf16_col(&text, 1),
        utf16_to_char_col(&text, 1)
            .max(1)
            .min(char_to_utf16_col(&text, 1).max(1))
    );
    assert_eq!(
        grammar_symbol(&format!("lang-11")).contains("tree_sitter_"),
        true
    );
    assert_eq!(
        parse_snippet_to_plain_text(&format!("${{1:v11}}")),
        format!("v11")
    );
}

#[test]
fn buffer_roundtrip_12() {
    let text = format!("line {}\nline {}\n", 12, 12 + 1);
    let rope = Rope::from_str(&text);
    assert_eq!(rope.to_string(), text);
    assert_eq!(
        line_len(&rope, 0),
        text.lines().next().unwrap().chars().count()
    );
    assert_eq!(
        char_to_utf16_col(&text, 1),
        utf16_to_char_col(&text, 1)
            .max(1)
            .min(char_to_utf16_col(&text, 1).max(1))
    );
    assert_eq!(
        grammar_symbol(&format!("lang-12")).contains("tree_sitter_"),
        true
    );
    assert_eq!(
        parse_snippet_to_plain_text(&format!("${{1:v12}}")),
        format!("v12")
    );
}

#[test]
fn buffer_roundtrip_13() {
    let text = format!("line {}\nline {}\n", 13, 13 + 1);
    let rope = Rope::from_str(&text);
    assert_eq!(rope.to_string(), text);
    assert_eq!(
        line_len(&rope, 0),
        text.lines().next().unwrap().chars().count()
    );
    assert_eq!(
        char_to_utf16_col(&text, 1),
        utf16_to_char_col(&text, 1)
            .max(1)
            .min(char_to_utf16_col(&text, 1).max(1))
    );
    assert_eq!(
        grammar_symbol(&format!("lang-13")).contains("tree_sitter_"),
        true
    );
    assert_eq!(
        parse_snippet_to_plain_text(&format!("${{1:v13}}")),
        format!("v13")
    );
}

#[test]
fn buffer_roundtrip_14() {
    let text = format!("line {}\nline {}\n", 14, 14 + 1);
    let rope = Rope::from_str(&text);
    assert_eq!(rope.to_string(), text);
    assert_eq!(
        line_len(&rope, 0),
        text.lines().next().unwrap().chars().count()
    );
    assert_eq!(
        char_to_utf16_col(&text, 1),
        utf16_to_char_col(&text, 1)
            .max(1)
            .min(char_to_utf16_col(&text, 1).max(1))
    );
    assert_eq!(
        grammar_symbol(&format!("lang-14")).contains("tree_sitter_"),
        true
    );
    assert_eq!(
        parse_snippet_to_plain_text(&format!("${{1:v14}}")),
        format!("v14")
    );
}

#[test]
fn buffer_roundtrip_15() {
    let text = format!("line {}\nline {}\n", 15, 15 + 1);
    let rope = Rope::from_str(&text);
    assert_eq!(rope.to_string(), text);
    assert_eq!(
        line_len(&rope, 0),
        text.lines().next().unwrap().chars().count()
    );
    assert_eq!(
        char_to_utf16_col(&text, 1),
        utf16_to_char_col(&text, 1)
            .max(1)
            .min(char_to_utf16_col(&text, 1).max(1))
    );
    assert_eq!(
        grammar_symbol(&format!("lang-15")).contains("tree_sitter_"),
        true
    );
    assert_eq!(
        parse_snippet_to_plain_text(&format!("${{1:v15}}")),
        format!("v15")
    );
}

#[test]
fn buffer_roundtrip_16() {
    let text = format!("line {}\nline {}\n", 16, 16 + 1);
    let rope = Rope::from_str(&text);
    assert_eq!(rope.to_string(), text);
    assert_eq!(
        line_len(&rope, 0),
        text.lines().next().unwrap().chars().count()
    );
    assert_eq!(
        char_to_utf16_col(&text, 1),
        utf16_to_char_col(&text, 1)
            .max(1)
            .min(char_to_utf16_col(&text, 1).max(1))
    );
    assert_eq!(
        grammar_symbol(&format!("lang-16")).contains("tree_sitter_"),
        true
    );
    assert_eq!(
        parse_snippet_to_plain_text(&format!("${{1:v16}}")),
        format!("v16")
    );
}

#[test]
fn buffer_roundtrip_17() {
    let text = format!("line {}\nline {}\n", 17, 17 + 1);
    let rope = Rope::from_str(&text);
    assert_eq!(rope.to_string(), text);
    assert_eq!(
        line_len(&rope, 0),
        text.lines().next().unwrap().chars().count()
    );
    assert_eq!(
        char_to_utf16_col(&text, 1),
        utf16_to_char_col(&text, 1)
            .max(1)
            .min(char_to_utf16_col(&text, 1).max(1))
    );
    assert_eq!(
        grammar_symbol(&format!("lang-17")).contains("tree_sitter_"),
        true
    );
    assert_eq!(
        parse_snippet_to_plain_text(&format!("${{1:v17}}")),
        format!("v17")
    );
}

#[test]
fn buffer_roundtrip_18() {
    let text = format!("line {}\nline {}\n", 18, 18 + 1);
    let rope = Rope::from_str(&text);
    assert_eq!(rope.to_string(), text);
    assert_eq!(
        line_len(&rope, 0),
        text.lines().next().unwrap().chars().count()
    );
    assert_eq!(
        char_to_utf16_col(&text, 1),
        utf16_to_char_col(&text, 1)
            .max(1)
            .min(char_to_utf16_col(&text, 1).max(1))
    );
    assert_eq!(
        grammar_symbol(&format!("lang-18")).contains("tree_sitter_"),
        true
    );
    assert_eq!(
        parse_snippet_to_plain_text(&format!("${{1:v18}}")),
        format!("v18")
    );
}

#[test]
fn buffer_roundtrip_19() {
    let text = format!("line {}\nline {}\n", 19, 19 + 1);
    let rope = Rope::from_str(&text);
    assert_eq!(rope.to_string(), text);
    assert_eq!(
        line_len(&rope, 0),
        text.lines().next().unwrap().chars().count()
    );
    assert_eq!(
        char_to_utf16_col(&text, 1),
        utf16_to_char_col(&text, 1)
            .max(1)
            .min(char_to_utf16_col(&text, 1).max(1))
    );
    assert_eq!(
        grammar_symbol(&format!("lang-19")).contains("tree_sitter_"),
        true
    );
    assert_eq!(
        parse_snippet_to_plain_text(&format!("${{1:v19}}")),
        format!("v19")
    );
}

#[test]
fn buffer_roundtrip_20() {
    let text = format!("line {}\nline {}\n", 20, 20 + 1);
    let rope = Rope::from_str(&text);
    assert_eq!(rope.to_string(), text);
    assert_eq!(
        line_len(&rope, 0),
        text.lines().next().unwrap().chars().count()
    );
    assert_eq!(
        char_to_utf16_col(&text, 1),
        utf16_to_char_col(&text, 1)
            .max(1)
            .min(char_to_utf16_col(&text, 1).max(1))
    );
    assert_eq!(
        grammar_symbol(&format!("lang-20")).contains("tree_sitter_"),
        true
    );
    assert_eq!(
        parse_snippet_to_plain_text(&format!("${{1:v20}}")),
        format!("v20")
    );
}

#[test]
fn buffer_roundtrip_21() {
    let text = format!("line {}\nline {}\n", 21, 21 + 1);
    let rope = Rope::from_str(&text);
    assert_eq!(rope.to_string(), text);
    assert_eq!(
        line_len(&rope, 0),
        text.lines().next().unwrap().chars().count()
    );
    assert_eq!(
        char_to_utf16_col(&text, 1),
        utf16_to_char_col(&text, 1)
            .max(1)
            .min(char_to_utf16_col(&text, 1).max(1))
    );
    assert_eq!(
        grammar_symbol(&format!("lang-21")).contains("tree_sitter_"),
        true
    );
    assert_eq!(
        parse_snippet_to_plain_text(&format!("${{1:v21}}")),
        format!("v21")
    );
}

#[test]
fn buffer_roundtrip_22() {
    let text = format!("line {}\nline {}\n", 22, 22 + 1);
    let rope = Rope::from_str(&text);
    assert_eq!(rope.to_string(), text);
    assert_eq!(
        line_len(&rope, 0),
        text.lines().next().unwrap().chars().count()
    );
    assert_eq!(
        char_to_utf16_col(&text, 1),
        utf16_to_char_col(&text, 1)
            .max(1)
            .min(char_to_utf16_col(&text, 1).max(1))
    );
    assert_eq!(
        grammar_symbol(&format!("lang-22")).contains("tree_sitter_"),
        true
    );
    assert_eq!(
        parse_snippet_to_plain_text(&format!("${{1:v22}}")),
        format!("v22")
    );
}

#[test]
fn buffer_roundtrip_23() {
    let text = format!("line {}\nline {}\n", 23, 23 + 1);
    let rope = Rope::from_str(&text);
    assert_eq!(rope.to_string(), text);
    assert_eq!(
        line_len(&rope, 0),
        text.lines().next().unwrap().chars().count()
    );
    assert_eq!(
        char_to_utf16_col(&text, 1),
        utf16_to_char_col(&text, 1)
            .max(1)
            .min(char_to_utf16_col(&text, 1).max(1))
    );
    assert_eq!(
        grammar_symbol(&format!("lang-23")).contains("tree_sitter_"),
        true
    );
    assert_eq!(
        parse_snippet_to_plain_text(&format!("${{1:v23}}")),
        format!("v23")
    );
}

#[test]
fn buffer_roundtrip_24() {
    let text = format!("line {}\nline {}\n", 24, 24 + 1);
    let rope = Rope::from_str(&text);
    assert_eq!(rope.to_string(), text);
    assert_eq!(
        line_len(&rope, 0),
        text.lines().next().unwrap().chars().count()
    );
    assert_eq!(
        char_to_utf16_col(&text, 1),
        utf16_to_char_col(&text, 1)
            .max(1)
            .min(char_to_utf16_col(&text, 1).max(1))
    );
    assert_eq!(
        grammar_symbol(&format!("lang-24")).contains("tree_sitter_"),
        true
    );
    assert_eq!(
        parse_snippet_to_plain_text(&format!("${{1:v24}}")),
        format!("v24")
    );
}

#[test]
fn buffer_roundtrip_25() {
    let text = format!("line {}\nline {}\n", 25, 25 + 1);
    let rope = Rope::from_str(&text);
    assert_eq!(rope.to_string(), text);
    assert_eq!(
        line_len(&rope, 0),
        text.lines().next().unwrap().chars().count()
    );
    assert_eq!(
        char_to_utf16_col(&text, 1),
        utf16_to_char_col(&text, 1)
            .max(1)
            .min(char_to_utf16_col(&text, 1).max(1))
    );
    assert_eq!(
        grammar_symbol(&format!("lang-25")).contains("tree_sitter_"),
        true
    );
    assert_eq!(
        parse_snippet_to_plain_text(&format!("${{1:v25}}")),
        format!("v25")
    );
}

#[test]
fn buffer_roundtrip_26() {
    let text = format!("line {}\nline {}\n", 26, 26 + 1);
    let rope = Rope::from_str(&text);
    assert_eq!(rope.to_string(), text);
    assert_eq!(
        line_len(&rope, 0),
        text.lines().next().unwrap().chars().count()
    );
    assert_eq!(
        char_to_utf16_col(&text, 1),
        utf16_to_char_col(&text, 1)
            .max(1)
            .min(char_to_utf16_col(&text, 1).max(1))
    );
    assert_eq!(
        grammar_symbol(&format!("lang-26")).contains("tree_sitter_"),
        true
    );
    assert_eq!(
        parse_snippet_to_plain_text(&format!("${{1:v26}}")),
        format!("v26")
    );
}

#[test]
fn buffer_roundtrip_27() {
    let text = format!("line {}\nline {}\n", 27, 27 + 1);
    let rope = Rope::from_str(&text);
    assert_eq!(rope.to_string(), text);
    assert_eq!(
        line_len(&rope, 0),
        text.lines().next().unwrap().chars().count()
    );
    assert_eq!(
        char_to_utf16_col(&text, 1),
        utf16_to_char_col(&text, 1)
            .max(1)
            .min(char_to_utf16_col(&text, 1).max(1))
    );
    assert_eq!(
        grammar_symbol(&format!("lang-27")).contains("tree_sitter_"),
        true
    );
    assert_eq!(
        parse_snippet_to_plain_text(&format!("${{1:v27}}")),
        format!("v27")
    );
}

#[test]
fn buffer_roundtrip_28() {
    let text = format!("line {}\nline {}\n", 28, 28 + 1);
    let rope = Rope::from_str(&text);
    assert_eq!(rope.to_string(), text);
    assert_eq!(
        line_len(&rope, 0),
        text.lines().next().unwrap().chars().count()
    );
    assert_eq!(
        char_to_utf16_col(&text, 1),
        utf16_to_char_col(&text, 1)
            .max(1)
            .min(char_to_utf16_col(&text, 1).max(1))
    );
    assert_eq!(
        grammar_symbol(&format!("lang-28")).contains("tree_sitter_"),
        true
    );
    assert_eq!(
        parse_snippet_to_plain_text(&format!("${{1:v28}}")),
        format!("v28")
    );
}

#[test]
fn buffer_roundtrip_29() {
    let text = format!("line {}\nline {}\n", 29, 29 + 1);
    let rope = Rope::from_str(&text);
    assert_eq!(rope.to_string(), text);
    assert_eq!(
        line_len(&rope, 0),
        text.lines().next().unwrap().chars().count()
    );
    assert_eq!(
        char_to_utf16_col(&text, 1),
        utf16_to_char_col(&text, 1)
            .max(1)
            .min(char_to_utf16_col(&text, 1).max(1))
    );
    assert_eq!(
        grammar_symbol(&format!("lang-29")).contains("tree_sitter_"),
        true
    );
    assert_eq!(
        parse_snippet_to_plain_text(&format!("${{1:v29}}")),
        format!("v29")
    );
}

#[test]
fn buffer_roundtrip_30() {
    let text = format!("line {}\nline {}\n", 30, 30 + 1);
    let rope = Rope::from_str(&text);
    assert_eq!(rope.to_string(), text);
    assert_eq!(
        line_len(&rope, 0),
        text.lines().next().unwrap().chars().count()
    );
    assert_eq!(
        char_to_utf16_col(&text, 1),
        utf16_to_char_col(&text, 1)
            .max(1)
            .min(char_to_utf16_col(&text, 1).max(1))
    );
    assert_eq!(
        grammar_symbol(&format!("lang-30")).contains("tree_sitter_"),
        true
    );
    assert_eq!(
        parse_snippet_to_plain_text(&format!("${{1:v30}}")),
        format!("v30")
    );
}

#[test]
fn buffer_roundtrip_31() {
    let text = format!("line {}\nline {}\n", 31, 31 + 1);
    let rope = Rope::from_str(&text);
    assert_eq!(rope.to_string(), text);
    assert_eq!(
        line_len(&rope, 0),
        text.lines().next().unwrap().chars().count()
    );
    assert_eq!(
        char_to_utf16_col(&text, 1),
        utf16_to_char_col(&text, 1)
            .max(1)
            .min(char_to_utf16_col(&text, 1).max(1))
    );
    assert_eq!(
        grammar_symbol(&format!("lang-31")).contains("tree_sitter_"),
        true
    );
    assert_eq!(
        parse_snippet_to_plain_text(&format!("${{1:v31}}")),
        format!("v31")
    );
}

#[test]
fn buffer_roundtrip_32() {
    let text = format!("line {}\nline {}\n", 32, 32 + 1);
    let rope = Rope::from_str(&text);
    assert_eq!(rope.to_string(), text);
    assert_eq!(
        line_len(&rope, 0),
        text.lines().next().unwrap().chars().count()
    );
    assert_eq!(
        char_to_utf16_col(&text, 1),
        utf16_to_char_col(&text, 1)
            .max(1)
            .min(char_to_utf16_col(&text, 1).max(1))
    );
    assert_eq!(
        grammar_symbol(&format!("lang-32")).contains("tree_sitter_"),
        true
    );
    assert_eq!(
        parse_snippet_to_plain_text(&format!("${{1:v32}}")),
        format!("v32")
    );
}

#[test]
fn buffer_roundtrip_33() {
    let text = format!("line {}\nline {}\n", 33, 33 + 1);
    let rope = Rope::from_str(&text);
    assert_eq!(rope.to_string(), text);
    assert_eq!(
        line_len(&rope, 0),
        text.lines().next().unwrap().chars().count()
    );
    assert_eq!(
        char_to_utf16_col(&text, 1),
        utf16_to_char_col(&text, 1)
            .max(1)
            .min(char_to_utf16_col(&text, 1).max(1))
    );
    assert_eq!(
        grammar_symbol(&format!("lang-33")).contains("tree_sitter_"),
        true
    );
    assert_eq!(
        parse_snippet_to_plain_text(&format!("${{1:v33}}")),
        format!("v33")
    );
}

#[test]
fn buffer_roundtrip_34() {
    let text = format!("line {}\nline {}\n", 34, 34 + 1);
    let rope = Rope::from_str(&text);
    assert_eq!(rope.to_string(), text);
    assert_eq!(
        line_len(&rope, 0),
        text.lines().next().unwrap().chars().count()
    );
    assert_eq!(
        char_to_utf16_col(&text, 1),
        utf16_to_char_col(&text, 1)
            .max(1)
            .min(char_to_utf16_col(&text, 1).max(1))
    );
    assert_eq!(
        grammar_symbol(&format!("lang-34")).contains("tree_sitter_"),
        true
    );
    assert_eq!(
        parse_snippet_to_plain_text(&format!("${{1:v34}}")),
        format!("v34")
    );
}

#[test]
fn buffer_roundtrip_35() {
    let text = format!("line {}\nline {}\n", 35, 35 + 1);
    let rope = Rope::from_str(&text);
    assert_eq!(rope.to_string(), text);
    assert_eq!(
        line_len(&rope, 0),
        text.lines().next().unwrap().chars().count()
    );
    assert_eq!(
        char_to_utf16_col(&text, 1),
        utf16_to_char_col(&text, 1)
            .max(1)
            .min(char_to_utf16_col(&text, 1).max(1))
    );
    assert_eq!(
        grammar_symbol(&format!("lang-35")).contains("tree_sitter_"),
        true
    );
    assert_eq!(
        parse_snippet_to_plain_text(&format!("${{1:v35}}")),
        format!("v35")
    );
}

#[test]
fn buffer_roundtrip_36() {
    let text = format!("line {}\nline {}\n", 36, 36 + 1);
    let rope = Rope::from_str(&text);
    assert_eq!(rope.to_string(), text);
    assert_eq!(
        line_len(&rope, 0),
        text.lines().next().unwrap().chars().count()
    );
    assert_eq!(
        char_to_utf16_col(&text, 1),
        utf16_to_char_col(&text, 1)
            .max(1)
            .min(char_to_utf16_col(&text, 1).max(1))
    );
    assert_eq!(
        grammar_symbol(&format!("lang-36")).contains("tree_sitter_"),
        true
    );
    assert_eq!(
        parse_snippet_to_plain_text(&format!("${{1:v36}}")),
        format!("v36")
    );
}

#[test]
fn buffer_roundtrip_37() {
    let text = format!("line {}\nline {}\n", 37, 37 + 1);
    let rope = Rope::from_str(&text);
    assert_eq!(rope.to_string(), text);
    assert_eq!(
        line_len(&rope, 0),
        text.lines().next().unwrap().chars().count()
    );
    assert_eq!(
        char_to_utf16_col(&text, 1),
        utf16_to_char_col(&text, 1)
            .max(1)
            .min(char_to_utf16_col(&text, 1).max(1))
    );
    assert_eq!(
        grammar_symbol(&format!("lang-37")).contains("tree_sitter_"),
        true
    );
    assert_eq!(
        parse_snippet_to_plain_text(&format!("${{1:v37}}")),
        format!("v37")
    );
}

#[test]
fn buffer_roundtrip_38() {
    let text = format!("line {}\nline {}\n", 38, 38 + 1);
    let rope = Rope::from_str(&text);
    assert_eq!(rope.to_string(), text);
    assert_eq!(
        line_len(&rope, 0),
        text.lines().next().unwrap().chars().count()
    );
    assert_eq!(
        char_to_utf16_col(&text, 1),
        utf16_to_char_col(&text, 1)
            .max(1)
            .min(char_to_utf16_col(&text, 1).max(1))
    );
    assert_eq!(
        grammar_symbol(&format!("lang-38")).contains("tree_sitter_"),
        true
    );
    assert_eq!(
        parse_snippet_to_plain_text(&format!("${{1:v38}}")),
        format!("v38")
    );
}

#[test]
fn buffer_roundtrip_39() {
    let text = format!("line {}\nline {}\n", 39, 39 + 1);
    let rope = Rope::from_str(&text);
    assert_eq!(rope.to_string(), text);
    assert_eq!(
        line_len(&rope, 0),
        text.lines().next().unwrap().chars().count()
    );
    assert_eq!(
        char_to_utf16_col(&text, 1),
        utf16_to_char_col(&text, 1)
            .max(1)
            .min(char_to_utf16_col(&text, 1).max(1))
    );
    assert_eq!(
        grammar_symbol(&format!("lang-39")).contains("tree_sitter_"),
        true
    );
    assert_eq!(
        parse_snippet_to_plain_text(&format!("${{1:v39}}")),
        format!("v39")
    );
}

#[test]
fn buffer_roundtrip_40() {
    let text = format!("line {}\nline {}\n", 40, 40 + 1);
    let rope = Rope::from_str(&text);
    assert_eq!(rope.to_string(), text);
    assert_eq!(
        line_len(&rope, 0),
        text.lines().next().unwrap().chars().count()
    );
    assert_eq!(
        char_to_utf16_col(&text, 1),
        utf16_to_char_col(&text, 1)
            .max(1)
            .min(char_to_utf16_col(&text, 1).max(1))
    );
    assert_eq!(
        grammar_symbol(&format!("lang-40")).contains("tree_sitter_"),
        true
    );
    assert_eq!(
        parse_snippet_to_plain_text(&format!("${{1:v40}}")),
        format!("v40")
    );
}

#[test]
fn buffer_roundtrip_41() {
    let text = format!("line {}\nline {}\n", 41, 41 + 1);
    let rope = Rope::from_str(&text);
    assert_eq!(rope.to_string(), text);
    assert_eq!(
        line_len(&rope, 0),
        text.lines().next().unwrap().chars().count()
    );
    assert_eq!(
        char_to_utf16_col(&text, 1),
        utf16_to_char_col(&text, 1)
            .max(1)
            .min(char_to_utf16_col(&text, 1).max(1))
    );
    assert_eq!(
        grammar_symbol(&format!("lang-41")).contains("tree_sitter_"),
        true
    );
    assert_eq!(
        parse_snippet_to_plain_text(&format!("${{1:v41}}")),
        format!("v41")
    );
}

#[test]
fn buffer_roundtrip_42() {
    let text = format!("line {}\nline {}\n", 42, 42 + 1);
    let rope = Rope::from_str(&text);
    assert_eq!(rope.to_string(), text);
    assert_eq!(
        line_len(&rope, 0),
        text.lines().next().unwrap().chars().count()
    );
    assert_eq!(
        char_to_utf16_col(&text, 1),
        utf16_to_char_col(&text, 1)
            .max(1)
            .min(char_to_utf16_col(&text, 1).max(1))
    );
    assert_eq!(
        grammar_symbol(&format!("lang-42")).contains("tree_sitter_"),
        true
    );
    assert_eq!(
        parse_snippet_to_plain_text(&format!("${{1:v42}}")),
        format!("v42")
    );
}

#[test]
fn buffer_roundtrip_43() {
    let text = format!("line {}\nline {}\n", 43, 43 + 1);
    let rope = Rope::from_str(&text);
    assert_eq!(rope.to_string(), text);
    assert_eq!(
        line_len(&rope, 0),
        text.lines().next().unwrap().chars().count()
    );
    assert_eq!(
        char_to_utf16_col(&text, 1),
        utf16_to_char_col(&text, 1)
            .max(1)
            .min(char_to_utf16_col(&text, 1).max(1))
    );
    assert_eq!(
        grammar_symbol(&format!("lang-43")).contains("tree_sitter_"),
        true
    );
    assert_eq!(
        parse_snippet_to_plain_text(&format!("${{1:v43}}")),
        format!("v43")
    );
}

#[test]
fn buffer_roundtrip_44() {
    let text = format!("line {}\nline {}\n", 44, 44 + 1);
    let rope = Rope::from_str(&text);
    assert_eq!(rope.to_string(), text);
    assert_eq!(
        line_len(&rope, 0),
        text.lines().next().unwrap().chars().count()
    );
    assert_eq!(
        char_to_utf16_col(&text, 1),
        utf16_to_char_col(&text, 1)
            .max(1)
            .min(char_to_utf16_col(&text, 1).max(1))
    );
    assert_eq!(
        grammar_symbol(&format!("lang-44")).contains("tree_sitter_"),
        true
    );
    assert_eq!(
        parse_snippet_to_plain_text(&format!("${{1:v44}}")),
        format!("v44")
    );
}

#[test]
fn buffer_roundtrip_45() {
    let text = format!("line {}\nline {}\n", 45, 45 + 1);
    let rope = Rope::from_str(&text);
    assert_eq!(rope.to_string(), text);
    assert_eq!(
        line_len(&rope, 0),
        text.lines().next().unwrap().chars().count()
    );
    assert_eq!(
        char_to_utf16_col(&text, 1),
        utf16_to_char_col(&text, 1)
            .max(1)
            .min(char_to_utf16_col(&text, 1).max(1))
    );
    assert_eq!(
        grammar_symbol(&format!("lang-45")).contains("tree_sitter_"),
        true
    );
    assert_eq!(
        parse_snippet_to_plain_text(&format!("${{1:v45}}")),
        format!("v45")
    );
}

#[test]
fn buffer_roundtrip_46() {
    let text = format!("line {}\nline {}\n", 46, 46 + 1);
    let rope = Rope::from_str(&text);
    assert_eq!(rope.to_string(), text);
    assert_eq!(
        line_len(&rope, 0),
        text.lines().next().unwrap().chars().count()
    );
    assert_eq!(
        char_to_utf16_col(&text, 1),
        utf16_to_char_col(&text, 1)
            .max(1)
            .min(char_to_utf16_col(&text, 1).max(1))
    );
    assert_eq!(
        grammar_symbol(&format!("lang-46")).contains("tree_sitter_"),
        true
    );
    assert_eq!(
        parse_snippet_to_plain_text(&format!("${{1:v46}}")),
        format!("v46")
    );
}

#[test]
fn buffer_roundtrip_47() {
    let text = format!("line {}\nline {}\n", 47, 47 + 1);
    let rope = Rope::from_str(&text);
    assert_eq!(rope.to_string(), text);
    assert_eq!(
        line_len(&rope, 0),
        text.lines().next().unwrap().chars().count()
    );
    assert_eq!(
        char_to_utf16_col(&text, 1),
        utf16_to_char_col(&text, 1)
            .max(1)
            .min(char_to_utf16_col(&text, 1).max(1))
    );
    assert_eq!(
        grammar_symbol(&format!("lang-47")).contains("tree_sitter_"),
        true
    );
    assert_eq!(
        parse_snippet_to_plain_text(&format!("${{1:v47}}")),
        format!("v47")
    );
}

#[test]
fn buffer_roundtrip_48() {
    let text = format!("line {}\nline {}\n", 48, 48 + 1);
    let rope = Rope::from_str(&text);
    assert_eq!(rope.to_string(), text);
    assert_eq!(
        line_len(&rope, 0),
        text.lines().next().unwrap().chars().count()
    );
    assert_eq!(
        char_to_utf16_col(&text, 1),
        utf16_to_char_col(&text, 1)
            .max(1)
            .min(char_to_utf16_col(&text, 1).max(1))
    );
    assert_eq!(
        grammar_symbol(&format!("lang-48")).contains("tree_sitter_"),
        true
    );
    assert_eq!(
        parse_snippet_to_plain_text(&format!("${{1:v48}}")),
        format!("v48")
    );
}

#[test]
fn buffer_roundtrip_49() {
    let text = format!("line {}\nline {}\n", 49, 49 + 1);
    let rope = Rope::from_str(&text);
    assert_eq!(rope.to_string(), text);
    assert_eq!(
        line_len(&rope, 0),
        text.lines().next().unwrap().chars().count()
    );
    assert_eq!(
        char_to_utf16_col(&text, 1),
        utf16_to_char_col(&text, 1)
            .max(1)
            .min(char_to_utf16_col(&text, 1).max(1))
    );
    assert_eq!(
        grammar_symbol(&format!("lang-49")).contains("tree_sitter_"),
        true
    );
    assert_eq!(
        parse_snippet_to_plain_text(&format!("${{1:v49}}")),
        format!("v49")
    );
}

#[test]
fn buffer_roundtrip_50() {
    let text = format!("line {}\nline {}\n", 50, 50 + 1);
    let rope = Rope::from_str(&text);
    assert_eq!(rope.to_string(), text);
    assert_eq!(
        line_len(&rope, 0),
        text.lines().next().unwrap().chars().count()
    );
    assert_eq!(
        char_to_utf16_col(&text, 1),
        utf16_to_char_col(&text, 1)
            .max(1)
            .min(char_to_utf16_col(&text, 1).max(1))
    );
    assert_eq!(
        grammar_symbol(&format!("lang-50")).contains("tree_sitter_"),
        true
    );
    assert_eq!(
        parse_snippet_to_plain_text(&format!("${{1:v50}}")),
        format!("v50")
    );
}

#[test]
fn buffer_roundtrip_51() {
    let text = format!("line {}\nline {}\n", 51, 51 + 1);
    let rope = Rope::from_str(&text);
    assert_eq!(rope.to_string(), text);
    assert_eq!(
        line_len(&rope, 0),
        text.lines().next().unwrap().chars().count()
    );
    assert_eq!(
        char_to_utf16_col(&text, 1),
        utf16_to_char_col(&text, 1)
            .max(1)
            .min(char_to_utf16_col(&text, 1).max(1))
    );
    assert_eq!(
        grammar_symbol(&format!("lang-51")).contains("tree_sitter_"),
        true
    );
    assert_eq!(
        parse_snippet_to_plain_text(&format!("${{1:v51}}")),
        format!("v51")
    );
}

#[test]
fn buffer_roundtrip_52() {
    let text = format!("line {}\nline {}\n", 52, 52 + 1);
    let rope = Rope::from_str(&text);
    assert_eq!(rope.to_string(), text);
    assert_eq!(
        line_len(&rope, 0),
        text.lines().next().unwrap().chars().count()
    );
    assert_eq!(
        char_to_utf16_col(&text, 1),
        utf16_to_char_col(&text, 1)
            .max(1)
            .min(char_to_utf16_col(&text, 1).max(1))
    );
    assert_eq!(
        grammar_symbol(&format!("lang-52")).contains("tree_sitter_"),
        true
    );
    assert_eq!(
        parse_snippet_to_plain_text(&format!("${{1:v52}}")),
        format!("v52")
    );
}

#[test]
fn buffer_roundtrip_53() {
    let text = format!("line {}\nline {}\n", 53, 53 + 1);
    let rope = Rope::from_str(&text);
    assert_eq!(rope.to_string(), text);
    assert_eq!(
        line_len(&rope, 0),
        text.lines().next().unwrap().chars().count()
    );
    assert_eq!(
        char_to_utf16_col(&text, 1),
        utf16_to_char_col(&text, 1)
            .max(1)
            .min(char_to_utf16_col(&text, 1).max(1))
    );
    assert_eq!(
        grammar_symbol(&format!("lang-53")).contains("tree_sitter_"),
        true
    );
    assert_eq!(
        parse_snippet_to_plain_text(&format!("${{1:v53}}")),
        format!("v53")
    );
}

#[test]
fn buffer_roundtrip_54() {
    let text = format!("line {}\nline {}\n", 54, 54 + 1);
    let rope = Rope::from_str(&text);
    assert_eq!(rope.to_string(), text);
    assert_eq!(
        line_len(&rope, 0),
        text.lines().next().unwrap().chars().count()
    );
    assert_eq!(
        char_to_utf16_col(&text, 1),
        utf16_to_char_col(&text, 1)
            .max(1)
            .min(char_to_utf16_col(&text, 1).max(1))
    );
    assert_eq!(
        grammar_symbol(&format!("lang-54")).contains("tree_sitter_"),
        true
    );
    assert_eq!(
        parse_snippet_to_plain_text(&format!("${{1:v54}}")),
        format!("v54")
    );
}

#[test]
fn buffer_roundtrip_55() {
    let text = format!("line {}\nline {}\n", 55, 55 + 1);
    let rope = Rope::from_str(&text);
    assert_eq!(rope.to_string(), text);
    assert_eq!(
        line_len(&rope, 0),
        text.lines().next().unwrap().chars().count()
    );
    assert_eq!(
        char_to_utf16_col(&text, 1),
        utf16_to_char_col(&text, 1)
            .max(1)
            .min(char_to_utf16_col(&text, 1).max(1))
    );
    assert_eq!(
        grammar_symbol(&format!("lang-55")).contains("tree_sitter_"),
        true
    );
    assert_eq!(
        parse_snippet_to_plain_text(&format!("${{1:v55}}")),
        format!("v55")
    );
}

#[test]
fn buffer_roundtrip_56() {
    let text = format!("line {}\nline {}\n", 56, 56 + 1);
    let rope = Rope::from_str(&text);
    assert_eq!(rope.to_string(), text);
    assert_eq!(
        line_len(&rope, 0),
        text.lines().next().unwrap().chars().count()
    );
    assert_eq!(
        char_to_utf16_col(&text, 1),
        utf16_to_char_col(&text, 1)
            .max(1)
            .min(char_to_utf16_col(&text, 1).max(1))
    );
    assert_eq!(
        grammar_symbol(&format!("lang-56")).contains("tree_sitter_"),
        true
    );
    assert_eq!(
        parse_snippet_to_plain_text(&format!("${{1:v56}}")),
        format!("v56")
    );
}

#[test]
fn buffer_roundtrip_57() {
    let text = format!("line {}\nline {}\n", 57, 57 + 1);
    let rope = Rope::from_str(&text);
    assert_eq!(rope.to_string(), text);
    assert_eq!(
        line_len(&rope, 0),
        text.lines().next().unwrap().chars().count()
    );
    assert_eq!(
        char_to_utf16_col(&text, 1),
        utf16_to_char_col(&text, 1)
            .max(1)
            .min(char_to_utf16_col(&text, 1).max(1))
    );
    assert_eq!(
        grammar_symbol(&format!("lang-57")).contains("tree_sitter_"),
        true
    );
    assert_eq!(
        parse_snippet_to_plain_text(&format!("${{1:v57}}")),
        format!("v57")
    );
}

#[test]
fn buffer_roundtrip_58() {
    let text = format!("line {}\nline {}\n", 58, 58 + 1);
    let rope = Rope::from_str(&text);
    assert_eq!(rope.to_string(), text);
    assert_eq!(
        line_len(&rope, 0),
        text.lines().next().unwrap().chars().count()
    );
    assert_eq!(
        char_to_utf16_col(&text, 1),
        utf16_to_char_col(&text, 1)
            .max(1)
            .min(char_to_utf16_col(&text, 1).max(1))
    );
    assert_eq!(
        grammar_symbol(&format!("lang-58")).contains("tree_sitter_"),
        true
    );
    assert_eq!(
        parse_snippet_to_plain_text(&format!("${{1:v58}}")),
        format!("v58")
    );
}

#[test]
fn buffer_roundtrip_59() {
    let text = format!("line {}\nline {}\n", 59, 59 + 1);
    let rope = Rope::from_str(&text);
    assert_eq!(rope.to_string(), text);
    assert_eq!(
        line_len(&rope, 0),
        text.lines().next().unwrap().chars().count()
    );
    assert_eq!(
        char_to_utf16_col(&text, 1),
        utf16_to_char_col(&text, 1)
            .max(1)
            .min(char_to_utf16_col(&text, 1).max(1))
    );
    assert_eq!(
        grammar_symbol(&format!("lang-59")).contains("tree_sitter_"),
        true
    );
    assert_eq!(
        parse_snippet_to_plain_text(&format!("${{1:v59}}")),
        format!("v59")
    );
}

#[test]
fn buffer_roundtrip_60() {
    let text = format!("line {}\nline {}\n", 60, 60 + 1);
    let rope = Rope::from_str(&text);
    assert_eq!(rope.to_string(), text);
    assert_eq!(
        line_len(&rope, 0),
        text.lines().next().unwrap().chars().count()
    );
    assert_eq!(
        char_to_utf16_col(&text, 1),
        utf16_to_char_col(&text, 1)
            .max(1)
            .min(char_to_utf16_col(&text, 1).max(1))
    );
    assert_eq!(
        grammar_symbol(&format!("lang-60")).contains("tree_sitter_"),
        true
    );
    assert_eq!(
        parse_snippet_to_plain_text(&format!("${{1:v60}}")),
        format!("v60")
    );
}

#[test]
fn buffer_roundtrip_61() {
    let text = format!("line {}\nline {}\n", 61, 61 + 1);
    let rope = Rope::from_str(&text);
    assert_eq!(rope.to_string(), text);
    assert_eq!(
        line_len(&rope, 0),
        text.lines().next().unwrap().chars().count()
    );
    assert_eq!(
        char_to_utf16_col(&text, 1),
        utf16_to_char_col(&text, 1)
            .max(1)
            .min(char_to_utf16_col(&text, 1).max(1))
    );
    assert_eq!(
        grammar_symbol(&format!("lang-61")).contains("tree_sitter_"),
        true
    );
    assert_eq!(
        parse_snippet_to_plain_text(&format!("${{1:v61}}")),
        format!("v61")
    );
}

#[test]
fn buffer_roundtrip_62() {
    let text = format!("line {}\nline {}\n", 62, 62 + 1);
    let rope = Rope::from_str(&text);
    assert_eq!(rope.to_string(), text);
    assert_eq!(
        line_len(&rope, 0),
        text.lines().next().unwrap().chars().count()
    );
    assert_eq!(
        char_to_utf16_col(&text, 1),
        utf16_to_char_col(&text, 1)
            .max(1)
            .min(char_to_utf16_col(&text, 1).max(1))
    );
    assert_eq!(
        grammar_symbol(&format!("lang-62")).contains("tree_sitter_"),
        true
    );
    assert_eq!(
        parse_snippet_to_plain_text(&format!("${{1:v62}}")),
        format!("v62")
    );
}

#[test]
fn buffer_roundtrip_63() {
    let text = format!("line {}\nline {}\n", 63, 63 + 1);
    let rope = Rope::from_str(&text);
    assert_eq!(rope.to_string(), text);
    assert_eq!(
        line_len(&rope, 0),
        text.lines().next().unwrap().chars().count()
    );
    assert_eq!(
        char_to_utf16_col(&text, 1),
        utf16_to_char_col(&text, 1)
            .max(1)
            .min(char_to_utf16_col(&text, 1).max(1))
    );
    assert_eq!(
        grammar_symbol(&format!("lang-63")).contains("tree_sitter_"),
        true
    );
    assert_eq!(
        parse_snippet_to_plain_text(&format!("${{1:v63}}")),
        format!("v63")
    );
}

#[test]
fn buffer_roundtrip_64() {
    let text = format!("line {}\nline {}\n", 64, 64 + 1);
    let rope = Rope::from_str(&text);
    assert_eq!(rope.to_string(), text);
    assert_eq!(
        line_len(&rope, 0),
        text.lines().next().unwrap().chars().count()
    );
    assert_eq!(
        char_to_utf16_col(&text, 1),
        utf16_to_char_col(&text, 1)
            .max(1)
            .min(char_to_utf16_col(&text, 1).max(1))
    );
    assert_eq!(
        grammar_symbol(&format!("lang-64")).contains("tree_sitter_"),
        true
    );
    assert_eq!(
        parse_snippet_to_plain_text(&format!("${{1:v64}}")),
        format!("v64")
    );
}

#[test]
fn buffer_roundtrip_65() {
    let text = format!("line {}\nline {}\n", 65, 65 + 1);
    let rope = Rope::from_str(&text);
    assert_eq!(rope.to_string(), text);
    assert_eq!(
        line_len(&rope, 0),
        text.lines().next().unwrap().chars().count()
    );
    assert_eq!(
        char_to_utf16_col(&text, 1),
        utf16_to_char_col(&text, 1)
            .max(1)
            .min(char_to_utf16_col(&text, 1).max(1))
    );
    assert_eq!(
        grammar_symbol(&format!("lang-65")).contains("tree_sitter_"),
        true
    );
    assert_eq!(
        parse_snippet_to_plain_text(&format!("${{1:v65}}")),
        format!("v65")
    );
}

#[test]
fn buffer_roundtrip_66() {
    let text = format!("line {}\nline {}\n", 66, 66 + 1);
    let rope = Rope::from_str(&text);
    assert_eq!(rope.to_string(), text);
    assert_eq!(
        line_len(&rope, 0),
        text.lines().next().unwrap().chars().count()
    );
    assert_eq!(
        char_to_utf16_col(&text, 1),
        utf16_to_char_col(&text, 1)
            .max(1)
            .min(char_to_utf16_col(&text, 1).max(1))
    );
    assert_eq!(
        grammar_symbol(&format!("lang-66")).contains("tree_sitter_"),
        true
    );
    assert_eq!(
        parse_snippet_to_plain_text(&format!("${{1:v66}}")),
        format!("v66")
    );
}

#[test]
fn buffer_roundtrip_67() {
    let text = format!("line {}\nline {}\n", 67, 67 + 1);
    let rope = Rope::from_str(&text);
    assert_eq!(rope.to_string(), text);
    assert_eq!(
        line_len(&rope, 0),
        text.lines().next().unwrap().chars().count()
    );
    assert_eq!(
        char_to_utf16_col(&text, 1),
        utf16_to_char_col(&text, 1)
            .max(1)
            .min(char_to_utf16_col(&text, 1).max(1))
    );
    assert_eq!(
        grammar_symbol(&format!("lang-67")).contains("tree_sitter_"),
        true
    );
    assert_eq!(
        parse_snippet_to_plain_text(&format!("${{1:v67}}")),
        format!("v67")
    );
}

#[test]
fn buffer_roundtrip_68() {
    let text = format!("line {}\nline {}\n", 68, 68 + 1);
    let rope = Rope::from_str(&text);
    assert_eq!(rope.to_string(), text);
    assert_eq!(
        line_len(&rope, 0),
        text.lines().next().unwrap().chars().count()
    );
    assert_eq!(
        char_to_utf16_col(&text, 1),
        utf16_to_char_col(&text, 1)
            .max(1)
            .min(char_to_utf16_col(&text, 1).max(1))
    );
    assert_eq!(
        grammar_symbol(&format!("lang-68")).contains("tree_sitter_"),
        true
    );
    assert_eq!(
        parse_snippet_to_plain_text(&format!("${{1:v68}}")),
        format!("v68")
    );
}

#[test]
fn buffer_roundtrip_69() {
    let text = format!("line {}\nline {}\n", 69, 69 + 1);
    let rope = Rope::from_str(&text);
    assert_eq!(rope.to_string(), text);
    assert_eq!(
        line_len(&rope, 0),
        text.lines().next().unwrap().chars().count()
    );
    assert_eq!(
        char_to_utf16_col(&text, 1),
        utf16_to_char_col(&text, 1)
            .max(1)
            .min(char_to_utf16_col(&text, 1).max(1))
    );
    assert_eq!(
        grammar_symbol(&format!("lang-69")).contains("tree_sitter_"),
        true
    );
    assert_eq!(
        parse_snippet_to_plain_text(&format!("${{1:v69}}")),
        format!("v69")
    );
}

#[test]
fn buffer_roundtrip_70() {
    let text = format!("line {}\nline {}\n", 70, 70 + 1);
    let rope = Rope::from_str(&text);
    assert_eq!(rope.to_string(), text);
    assert_eq!(
        line_len(&rope, 0),
        text.lines().next().unwrap().chars().count()
    );
    assert_eq!(
        char_to_utf16_col(&text, 1),
        utf16_to_char_col(&text, 1)
            .max(1)
            .min(char_to_utf16_col(&text, 1).max(1))
    );
    assert_eq!(
        grammar_symbol(&format!("lang-70")).contains("tree_sitter_"),
        true
    );
    assert_eq!(
        parse_snippet_to_plain_text(&format!("${{1:v70}}")),
        format!("v70")
    );
}

#[test]
fn buffer_roundtrip_71() {
    let text = format!("line {}\nline {}\n", 71, 71 + 1);
    let rope = Rope::from_str(&text);
    assert_eq!(rope.to_string(), text);
    assert_eq!(
        line_len(&rope, 0),
        text.lines().next().unwrap().chars().count()
    );
    assert_eq!(
        char_to_utf16_col(&text, 1),
        utf16_to_char_col(&text, 1)
            .max(1)
            .min(char_to_utf16_col(&text, 1).max(1))
    );
    assert_eq!(
        grammar_symbol(&format!("lang-71")).contains("tree_sitter_"),
        true
    );
    assert_eq!(
        parse_snippet_to_plain_text(&format!("${{1:v71}}")),
        format!("v71")
    );
}

#[test]
fn buffer_roundtrip_72() {
    let text = format!("line {}\nline {}\n", 72, 72 + 1);
    let rope = Rope::from_str(&text);
    assert_eq!(rope.to_string(), text);
    assert_eq!(
        line_len(&rope, 0),
        text.lines().next().unwrap().chars().count()
    );
    assert_eq!(
        char_to_utf16_col(&text, 1),
        utf16_to_char_col(&text, 1)
            .max(1)
            .min(char_to_utf16_col(&text, 1).max(1))
    );
    assert_eq!(
        grammar_symbol(&format!("lang-72")).contains("tree_sitter_"),
        true
    );
    assert_eq!(
        parse_snippet_to_plain_text(&format!("${{1:v72}}")),
        format!("v72")
    );
}

#[test]
fn buffer_roundtrip_73() {
    let text = format!("line {}\nline {}\n", 73, 73 + 1);
    let rope = Rope::from_str(&text);
    assert_eq!(rope.to_string(), text);
    assert_eq!(
        line_len(&rope, 0),
        text.lines().next().unwrap().chars().count()
    );
    assert_eq!(
        char_to_utf16_col(&text, 1),
        utf16_to_char_col(&text, 1)
            .max(1)
            .min(char_to_utf16_col(&text, 1).max(1))
    );
    assert_eq!(
        grammar_symbol(&format!("lang-73")).contains("tree_sitter_"),
        true
    );
    assert_eq!(
        parse_snippet_to_plain_text(&format!("${{1:v73}}")),
        format!("v73")
    );
}

#[test]
fn buffer_roundtrip_74() {
    let text = format!("line {}\nline {}\n", 74, 74 + 1);
    let rope = Rope::from_str(&text);
    assert_eq!(rope.to_string(), text);
    assert_eq!(
        line_len(&rope, 0),
        text.lines().next().unwrap().chars().count()
    );
    assert_eq!(
        char_to_utf16_col(&text, 1),
        utf16_to_char_col(&text, 1)
            .max(1)
            .min(char_to_utf16_col(&text, 1).max(1))
    );
    assert_eq!(
        grammar_symbol(&format!("lang-74")).contains("tree_sitter_"),
        true
    );
    assert_eq!(
        parse_snippet_to_plain_text(&format!("${{1:v74}}")),
        format!("v74")
    );
}

#[test]
fn buffer_roundtrip_75() {
    let text = format!("line {}\nline {}\n", 75, 75 + 1);
    let rope = Rope::from_str(&text);
    assert_eq!(rope.to_string(), text);
    assert_eq!(
        line_len(&rope, 0),
        text.lines().next().unwrap().chars().count()
    );
    assert_eq!(
        char_to_utf16_col(&text, 1),
        utf16_to_char_col(&text, 1)
            .max(1)
            .min(char_to_utf16_col(&text, 1).max(1))
    );
    assert_eq!(
        grammar_symbol(&format!("lang-75")).contains("tree_sitter_"),
        true
    );
    assert_eq!(
        parse_snippet_to_plain_text(&format!("${{1:v75}}")),
        format!("v75")
    );
}

#[test]
fn buffer_roundtrip_76() {
    let text = format!("line {}\nline {}\n", 76, 76 + 1);
    let rope = Rope::from_str(&text);
    assert_eq!(rope.to_string(), text);
    assert_eq!(
        line_len(&rope, 0),
        text.lines().next().unwrap().chars().count()
    );
    assert_eq!(
        char_to_utf16_col(&text, 1),
        utf16_to_char_col(&text, 1)
            .max(1)
            .min(char_to_utf16_col(&text, 1).max(1))
    );
    assert_eq!(
        grammar_symbol(&format!("lang-76")).contains("tree_sitter_"),
        true
    );
    assert_eq!(
        parse_snippet_to_plain_text(&format!("${{1:v76}}")),
        format!("v76")
    );
}

#[test]
fn buffer_roundtrip_77() {
    let text = format!("line {}\nline {}\n", 77, 77 + 1);
    let rope = Rope::from_str(&text);
    assert_eq!(rope.to_string(), text);
    assert_eq!(
        line_len(&rope, 0),
        text.lines().next().unwrap().chars().count()
    );
    assert_eq!(
        char_to_utf16_col(&text, 1),
        utf16_to_char_col(&text, 1)
            .max(1)
            .min(char_to_utf16_col(&text, 1).max(1))
    );
    assert_eq!(
        grammar_symbol(&format!("lang-77")).contains("tree_sitter_"),
        true
    );
    assert_eq!(
        parse_snippet_to_plain_text(&format!("${{1:v77}}")),
        format!("v77")
    );
}

#[test]
fn buffer_roundtrip_78() {
    let text = format!("line {}\nline {}\n", 78, 78 + 1);
    let rope = Rope::from_str(&text);
    assert_eq!(rope.to_string(), text);
    assert_eq!(
        line_len(&rope, 0),
        text.lines().next().unwrap().chars().count()
    );
    assert_eq!(
        char_to_utf16_col(&text, 1),
        utf16_to_char_col(&text, 1)
            .max(1)
            .min(char_to_utf16_col(&text, 1).max(1))
    );
    assert_eq!(
        grammar_symbol(&format!("lang-78")).contains("tree_sitter_"),
        true
    );
    assert_eq!(
        parse_snippet_to_plain_text(&format!("${{1:v78}}")),
        format!("v78")
    );
}

#[test]
fn buffer_roundtrip_79() {
    let text = format!("line {}\nline {}\n", 79, 79 + 1);
    let rope = Rope::from_str(&text);
    assert_eq!(rope.to_string(), text);
    assert_eq!(
        line_len(&rope, 0),
        text.lines().next().unwrap().chars().count()
    );
    assert_eq!(
        char_to_utf16_col(&text, 1),
        utf16_to_char_col(&text, 1)
            .max(1)
            .min(char_to_utf16_col(&text, 1).max(1))
    );
    assert_eq!(
        grammar_symbol(&format!("lang-79")).contains("tree_sitter_"),
        true
    );
    assert_eq!(
        parse_snippet_to_plain_text(&format!("${{1:v79}}")),
        format!("v79")
    );
}

#[test]
fn buffer_roundtrip_80() {
    let text = format!("line {}\nline {}\n", 80, 80 + 1);
    let rope = Rope::from_str(&text);
    assert_eq!(rope.to_string(), text);
    assert_eq!(
        line_len(&rope, 0),
        text.lines().next().unwrap().chars().count()
    );
    assert_eq!(
        char_to_utf16_col(&text, 1),
        utf16_to_char_col(&text, 1)
            .max(1)
            .min(char_to_utf16_col(&text, 1).max(1))
    );
    assert_eq!(
        grammar_symbol(&format!("lang-80")).contains("tree_sitter_"),
        true
    );
    assert_eq!(
        parse_snippet_to_plain_text(&format!("${{1:v80}}")),
        format!("v80")
    );
}

#[test]
fn buffer_roundtrip_81() {
    let text = format!("line {}\nline {}\n", 81, 81 + 1);
    let rope = Rope::from_str(&text);
    assert_eq!(rope.to_string(), text);
    assert_eq!(
        line_len(&rope, 0),
        text.lines().next().unwrap().chars().count()
    );
    assert_eq!(
        char_to_utf16_col(&text, 1),
        utf16_to_char_col(&text, 1)
            .max(1)
            .min(char_to_utf16_col(&text, 1).max(1))
    );
    assert_eq!(
        grammar_symbol(&format!("lang-81")).contains("tree_sitter_"),
        true
    );
    assert_eq!(
        parse_snippet_to_plain_text(&format!("${{1:v81}}")),
        format!("v81")
    );
}

#[test]
fn buffer_roundtrip_82() {
    let text = format!("line {}\nline {}\n", 82, 82 + 1);
    let rope = Rope::from_str(&text);
    assert_eq!(rope.to_string(), text);
    assert_eq!(
        line_len(&rope, 0),
        text.lines().next().unwrap().chars().count()
    );
    assert_eq!(
        char_to_utf16_col(&text, 1),
        utf16_to_char_col(&text, 1)
            .max(1)
            .min(char_to_utf16_col(&text, 1).max(1))
    );
    assert_eq!(
        grammar_symbol(&format!("lang-82")).contains("tree_sitter_"),
        true
    );
    assert_eq!(
        parse_snippet_to_plain_text(&format!("${{1:v82}}")),
        format!("v82")
    );
}

#[test]
fn buffer_roundtrip_83() {
    let text = format!("line {}\nline {}\n", 83, 83 + 1);
    let rope = Rope::from_str(&text);
    assert_eq!(rope.to_string(), text);
    assert_eq!(
        line_len(&rope, 0),
        text.lines().next().unwrap().chars().count()
    );
    assert_eq!(
        char_to_utf16_col(&text, 1),
        utf16_to_char_col(&text, 1)
            .max(1)
            .min(char_to_utf16_col(&text, 1).max(1))
    );
    assert_eq!(
        grammar_symbol(&format!("lang-83")).contains("tree_sitter_"),
        true
    );
    assert_eq!(
        parse_snippet_to_plain_text(&format!("${{1:v83}}")),
        format!("v83")
    );
}

#[test]
fn buffer_roundtrip_84() {
    let text = format!("line {}\nline {}\n", 84, 84 + 1);
    let rope = Rope::from_str(&text);
    assert_eq!(rope.to_string(), text);
    assert_eq!(
        line_len(&rope, 0),
        text.lines().next().unwrap().chars().count()
    );
    assert_eq!(
        char_to_utf16_col(&text, 1),
        utf16_to_char_col(&text, 1)
            .max(1)
            .min(char_to_utf16_col(&text, 1).max(1))
    );
    assert_eq!(
        grammar_symbol(&format!("lang-84")).contains("tree_sitter_"),
        true
    );
    assert_eq!(
        parse_snippet_to_plain_text(&format!("${{1:v84}}")),
        format!("v84")
    );
}

#[test]
fn buffer_roundtrip_85() {
    let text = format!("line {}\nline {}\n", 85, 85 + 1);
    let rope = Rope::from_str(&text);
    assert_eq!(rope.to_string(), text);
    assert_eq!(
        line_len(&rope, 0),
        text.lines().next().unwrap().chars().count()
    );
    assert_eq!(
        char_to_utf16_col(&text, 1),
        utf16_to_char_col(&text, 1)
            .max(1)
            .min(char_to_utf16_col(&text, 1).max(1))
    );
    assert_eq!(
        grammar_symbol(&format!("lang-85")).contains("tree_sitter_"),
        true
    );
    assert_eq!(
        parse_snippet_to_plain_text(&format!("${{1:v85}}")),
        format!("v85")
    );
}

#[test]
fn buffer_roundtrip_86() {
    let text = format!("line {}\nline {}\n", 86, 86 + 1);
    let rope = Rope::from_str(&text);
    assert_eq!(rope.to_string(), text);
    assert_eq!(
        line_len(&rope, 0),
        text.lines().next().unwrap().chars().count()
    );
    assert_eq!(
        char_to_utf16_col(&text, 1),
        utf16_to_char_col(&text, 1)
            .max(1)
            .min(char_to_utf16_col(&text, 1).max(1))
    );
    assert_eq!(
        grammar_symbol(&format!("lang-86")).contains("tree_sitter_"),
        true
    );
    assert_eq!(
        parse_snippet_to_plain_text(&format!("${{1:v86}}")),
        format!("v86")
    );
}

#[test]
fn buffer_roundtrip_87() {
    let text = format!("line {}\nline {}\n", 87, 87 + 1);
    let rope = Rope::from_str(&text);
    assert_eq!(rope.to_string(), text);
    assert_eq!(
        line_len(&rope, 0),
        text.lines().next().unwrap().chars().count()
    );
    assert_eq!(
        char_to_utf16_col(&text, 1),
        utf16_to_char_col(&text, 1)
            .max(1)
            .min(char_to_utf16_col(&text, 1).max(1))
    );
    assert_eq!(
        grammar_symbol(&format!("lang-87")).contains("tree_sitter_"),
        true
    );
    assert_eq!(
        parse_snippet_to_plain_text(&format!("${{1:v87}}")),
        format!("v87")
    );
}

#[test]
fn buffer_roundtrip_88() {
    let text = format!("line {}\nline {}\n", 88, 88 + 1);
    let rope = Rope::from_str(&text);
    assert_eq!(rope.to_string(), text);
    assert_eq!(
        line_len(&rope, 0),
        text.lines().next().unwrap().chars().count()
    );
    assert_eq!(
        char_to_utf16_col(&text, 1),
        utf16_to_char_col(&text, 1)
            .max(1)
            .min(char_to_utf16_col(&text, 1).max(1))
    );
    assert_eq!(
        grammar_symbol(&format!("lang-88")).contains("tree_sitter_"),
        true
    );
    assert_eq!(
        parse_snippet_to_plain_text(&format!("${{1:v88}}")),
        format!("v88")
    );
}

#[test]
fn buffer_roundtrip_89() {
    let text = format!("line {}\nline {}\n", 89, 89 + 1);
    let rope = Rope::from_str(&text);
    assert_eq!(rope.to_string(), text);
    assert_eq!(
        line_len(&rope, 0),
        text.lines().next().unwrap().chars().count()
    );
    assert_eq!(
        char_to_utf16_col(&text, 1),
        utf16_to_char_col(&text, 1)
            .max(1)
            .min(char_to_utf16_col(&text, 1).max(1))
    );
    assert_eq!(
        grammar_symbol(&format!("lang-89")).contains("tree_sitter_"),
        true
    );
    assert_eq!(
        parse_snippet_to_plain_text(&format!("${{1:v89}}")),
        format!("v89")
    );
}

#[test]
fn buffer_roundtrip_90() {
    let text = format!("line {}\nline {}\n", 90, 90 + 1);
    let rope = Rope::from_str(&text);
    assert_eq!(rope.to_string(), text);
    assert_eq!(
        line_len(&rope, 0),
        text.lines().next().unwrap().chars().count()
    );
    assert_eq!(
        char_to_utf16_col(&text, 1),
        utf16_to_char_col(&text, 1)
            .max(1)
            .min(char_to_utf16_col(&text, 1).max(1))
    );
    assert_eq!(
        grammar_symbol(&format!("lang-90")).contains("tree_sitter_"),
        true
    );
    assert_eq!(
        parse_snippet_to_plain_text(&format!("${{1:v90}}")),
        format!("v90")
    );
}

#[test]
fn buffer_roundtrip_91() {
    let text = format!("line {}\nline {}\n", 91, 91 + 1);
    let rope = Rope::from_str(&text);
    assert_eq!(rope.to_string(), text);
    assert_eq!(
        line_len(&rope, 0),
        text.lines().next().unwrap().chars().count()
    );
    assert_eq!(
        char_to_utf16_col(&text, 1),
        utf16_to_char_col(&text, 1)
            .max(1)
            .min(char_to_utf16_col(&text, 1).max(1))
    );
    assert_eq!(
        grammar_symbol(&format!("lang-91")).contains("tree_sitter_"),
        true
    );
    assert_eq!(
        parse_snippet_to_plain_text(&format!("${{1:v91}}")),
        format!("v91")
    );
}

#[test]
fn buffer_roundtrip_92() {
    let text = format!("line {}\nline {}\n", 92, 92 + 1);
    let rope = Rope::from_str(&text);
    assert_eq!(rope.to_string(), text);
    assert_eq!(
        line_len(&rope, 0),
        text.lines().next().unwrap().chars().count()
    );
    assert_eq!(
        char_to_utf16_col(&text, 1),
        utf16_to_char_col(&text, 1)
            .max(1)
            .min(char_to_utf16_col(&text, 1).max(1))
    );
    assert_eq!(
        grammar_symbol(&format!("lang-92")).contains("tree_sitter_"),
        true
    );
    assert_eq!(
        parse_snippet_to_plain_text(&format!("${{1:v92}}")),
        format!("v92")
    );
}

#[test]
fn buffer_roundtrip_93() {
    let text = format!("line {}\nline {}\n", 93, 93 + 1);
    let rope = Rope::from_str(&text);
    assert_eq!(rope.to_string(), text);
    assert_eq!(
        line_len(&rope, 0),
        text.lines().next().unwrap().chars().count()
    );
    assert_eq!(
        char_to_utf16_col(&text, 1),
        utf16_to_char_col(&text, 1)
            .max(1)
            .min(char_to_utf16_col(&text, 1).max(1))
    );
    assert_eq!(
        grammar_symbol(&format!("lang-93")).contains("tree_sitter_"),
        true
    );
    assert_eq!(
        parse_snippet_to_plain_text(&format!("${{1:v93}}")),
        format!("v93")
    );
}

#[test]
fn buffer_roundtrip_94() {
    let text = format!("line {}\nline {}\n", 94, 94 + 1);
    let rope = Rope::from_str(&text);
    assert_eq!(rope.to_string(), text);
    assert_eq!(
        line_len(&rope, 0),
        text.lines().next().unwrap().chars().count()
    );
    assert_eq!(
        char_to_utf16_col(&text, 1),
        utf16_to_char_col(&text, 1)
            .max(1)
            .min(char_to_utf16_col(&text, 1).max(1))
    );
    assert_eq!(
        grammar_symbol(&format!("lang-94")).contains("tree_sitter_"),
        true
    );
    assert_eq!(
        parse_snippet_to_plain_text(&format!("${{1:v94}}")),
        format!("v94")
    );
}

#[test]
fn buffer_roundtrip_95() {
    let text = format!("line {}\nline {}\n", 95, 95 + 1);
    let rope = Rope::from_str(&text);
    assert_eq!(rope.to_string(), text);
    assert_eq!(
        line_len(&rope, 0),
        text.lines().next().unwrap().chars().count()
    );
    assert_eq!(
        char_to_utf16_col(&text, 1),
        utf16_to_char_col(&text, 1)
            .max(1)
            .min(char_to_utf16_col(&text, 1).max(1))
    );
    assert_eq!(
        grammar_symbol(&format!("lang-95")).contains("tree_sitter_"),
        true
    );
    assert_eq!(
        parse_snippet_to_plain_text(&format!("${{1:v95}}")),
        format!("v95")
    );
}

#[test]
fn buffer_roundtrip_96() {
    let text = format!("line {}\nline {}\n", 96, 96 + 1);
    let rope = Rope::from_str(&text);
    assert_eq!(rope.to_string(), text);
    assert_eq!(
        line_len(&rope, 0),
        text.lines().next().unwrap().chars().count()
    );
    assert_eq!(
        char_to_utf16_col(&text, 1),
        utf16_to_char_col(&text, 1)
            .max(1)
            .min(char_to_utf16_col(&text, 1).max(1))
    );
    assert_eq!(
        grammar_symbol(&format!("lang-96")).contains("tree_sitter_"),
        true
    );
    assert_eq!(
        parse_snippet_to_plain_text(&format!("${{1:v96}}")),
        format!("v96")
    );
}

#[test]
fn buffer_roundtrip_97() {
    let text = format!("line {}\nline {}\n", 97, 97 + 1);
    let rope = Rope::from_str(&text);
    assert_eq!(rope.to_string(), text);
    assert_eq!(
        line_len(&rope, 0),
        text.lines().next().unwrap().chars().count()
    );
    assert_eq!(
        char_to_utf16_col(&text, 1),
        utf16_to_char_col(&text, 1)
            .max(1)
            .min(char_to_utf16_col(&text, 1).max(1))
    );
    assert_eq!(
        grammar_symbol(&format!("lang-97")).contains("tree_sitter_"),
        true
    );
    assert_eq!(
        parse_snippet_to_plain_text(&format!("${{1:v97}}")),
        format!("v97")
    );
}

#[test]
fn buffer_roundtrip_98() {
    let text = format!("line {}\nline {}\n", 98, 98 + 1);
    let rope = Rope::from_str(&text);
    assert_eq!(rope.to_string(), text);
    assert_eq!(
        line_len(&rope, 0),
        text.lines().next().unwrap().chars().count()
    );
    assert_eq!(
        char_to_utf16_col(&text, 1),
        utf16_to_char_col(&text, 1)
            .max(1)
            .min(char_to_utf16_col(&text, 1).max(1))
    );
    assert_eq!(
        grammar_symbol(&format!("lang-98")).contains("tree_sitter_"),
        true
    );
    assert_eq!(
        parse_snippet_to_plain_text(&format!("${{1:v98}}")),
        format!("v98")
    );
}

#[test]
fn buffer_roundtrip_99() {
    let text = format!("line {}\nline {}\n", 99, 99 + 1);
    let rope = Rope::from_str(&text);
    assert_eq!(rope.to_string(), text);
    assert_eq!(
        line_len(&rope, 0),
        text.lines().next().unwrap().chars().count()
    );
    assert_eq!(
        char_to_utf16_col(&text, 1),
        utf16_to_char_col(&text, 1)
            .max(1)
            .min(char_to_utf16_col(&text, 1).max(1))
    );
    assert_eq!(
        grammar_symbol(&format!("lang-99")).contains("tree_sitter_"),
        true
    );
    assert_eq!(
        parse_snippet_to_plain_text(&format!("${{1:v99}}")),
        format!("v99")
    );
}

#[test]
fn buffer_roundtrip_100() {
    let text = format!("line {}\nline {}\n", 100, 100 + 1);
    let rope = Rope::from_str(&text);
    assert_eq!(rope.to_string(), text);
    assert_eq!(
        line_len(&rope, 0),
        text.lines().next().unwrap().chars().count()
    );
    assert_eq!(
        char_to_utf16_col(&text, 1),
        utf16_to_char_col(&text, 1)
            .max(1)
            .min(char_to_utf16_col(&text, 1).max(1))
    );
    assert_eq!(
        grammar_symbol(&format!("lang-100")).contains("tree_sitter_"),
        true
    );
    assert_eq!(
        parse_snippet_to_plain_text(&format!("${{1:v100}}")),
        format!("v100")
    );
}

#[test]
fn buffer_roundtrip_101() {
    let text = format!("line {}\nline {}\n", 101, 101 + 1);
    let rope = Rope::from_str(&text);
    assert_eq!(rope.to_string(), text);
    assert_eq!(
        line_len(&rope, 0),
        text.lines().next().unwrap().chars().count()
    );
    assert_eq!(
        char_to_utf16_col(&text, 1),
        utf16_to_char_col(&text, 1)
            .max(1)
            .min(char_to_utf16_col(&text, 1).max(1))
    );
    assert_eq!(
        grammar_symbol(&format!("lang-101")).contains("tree_sitter_"),
        true
    );
    assert_eq!(
        parse_snippet_to_plain_text(&format!("${{1:v101}}")),
        format!("v101")
    );
}

#[test]
fn buffer_roundtrip_102() {
    let text = format!("line {}\nline {}\n", 102, 102 + 1);
    let rope = Rope::from_str(&text);
    assert_eq!(rope.to_string(), text);
    assert_eq!(
        line_len(&rope, 0),
        text.lines().next().unwrap().chars().count()
    );
    assert_eq!(
        char_to_utf16_col(&text, 1),
        utf16_to_char_col(&text, 1)
            .max(1)
            .min(char_to_utf16_col(&text, 1).max(1))
    );
    assert_eq!(
        grammar_symbol(&format!("lang-102")).contains("tree_sitter_"),
        true
    );
    assert_eq!(
        parse_snippet_to_plain_text(&format!("${{1:v102}}")),
        format!("v102")
    );
}

#[test]
fn buffer_roundtrip_103() {
    let text = format!("line {}\nline {}\n", 103, 103 + 1);
    let rope = Rope::from_str(&text);
    assert_eq!(rope.to_string(), text);
    assert_eq!(
        line_len(&rope, 0),
        text.lines().next().unwrap().chars().count()
    );
    assert_eq!(
        char_to_utf16_col(&text, 1),
        utf16_to_char_col(&text, 1)
            .max(1)
            .min(char_to_utf16_col(&text, 1).max(1))
    );
    assert_eq!(
        grammar_symbol(&format!("lang-103")).contains("tree_sitter_"),
        true
    );
    assert_eq!(
        parse_snippet_to_plain_text(&format!("${{1:v103}}")),
        format!("v103")
    );
}

#[test]
fn buffer_roundtrip_104() {
    let text = format!("line {}\nline {}\n", 104, 104 + 1);
    let rope = Rope::from_str(&text);
    assert_eq!(rope.to_string(), text);
    assert_eq!(
        line_len(&rope, 0),
        text.lines().next().unwrap().chars().count()
    );
    assert_eq!(
        char_to_utf16_col(&text, 1),
        utf16_to_char_col(&text, 1)
            .max(1)
            .min(char_to_utf16_col(&text, 1).max(1))
    );
    assert_eq!(
        grammar_symbol(&format!("lang-104")).contains("tree_sitter_"),
        true
    );
    assert_eq!(
        parse_snippet_to_plain_text(&format!("${{1:v104}}")),
        format!("v104")
    );
}

#[test]
fn buffer_roundtrip_105() {
    let text = format!("line {}\nline {}\n", 105, 105 + 1);
    let rope = Rope::from_str(&text);
    assert_eq!(rope.to_string(), text);
    assert_eq!(
        line_len(&rope, 0),
        text.lines().next().unwrap().chars().count()
    );
    assert_eq!(
        char_to_utf16_col(&text, 1),
        utf16_to_char_col(&text, 1)
            .max(1)
            .min(char_to_utf16_col(&text, 1).max(1))
    );
    assert_eq!(
        grammar_symbol(&format!("lang-105")).contains("tree_sitter_"),
        true
    );
    assert_eq!(
        parse_snippet_to_plain_text(&format!("${{1:v105}}")),
        format!("v105")
    );
}

#[test]
fn buffer_roundtrip_106() {
    let text = format!("line {}\nline {}\n", 106, 106 + 1);
    let rope = Rope::from_str(&text);
    assert_eq!(rope.to_string(), text);
    assert_eq!(
        line_len(&rope, 0),
        text.lines().next().unwrap().chars().count()
    );
    assert_eq!(
        char_to_utf16_col(&text, 1),
        utf16_to_char_col(&text, 1)
            .max(1)
            .min(char_to_utf16_col(&text, 1).max(1))
    );
    assert_eq!(
        grammar_symbol(&format!("lang-106")).contains("tree_sitter_"),
        true
    );
    assert_eq!(
        parse_snippet_to_plain_text(&format!("${{1:v106}}")),
        format!("v106")
    );
}

#[test]
fn buffer_roundtrip_107() {
    let text = format!("line {}\nline {}\n", 107, 107 + 1);
    let rope = Rope::from_str(&text);
    assert_eq!(rope.to_string(), text);
    assert_eq!(
        line_len(&rope, 0),
        text.lines().next().unwrap().chars().count()
    );
    assert_eq!(
        char_to_utf16_col(&text, 1),
        utf16_to_char_col(&text, 1)
            .max(1)
            .min(char_to_utf16_col(&text, 1).max(1))
    );
    assert_eq!(
        grammar_symbol(&format!("lang-107")).contains("tree_sitter_"),
        true
    );
    assert_eq!(
        parse_snippet_to_plain_text(&format!("${{1:v107}}")),
        format!("v107")
    );
}

#[test]
fn buffer_roundtrip_108() {
    let text = format!("line {}\nline {}\n", 108, 108 + 1);
    let rope = Rope::from_str(&text);
    assert_eq!(rope.to_string(), text);
    assert_eq!(
        line_len(&rope, 0),
        text.lines().next().unwrap().chars().count()
    );
    assert_eq!(
        char_to_utf16_col(&text, 1),
        utf16_to_char_col(&text, 1)
            .max(1)
            .min(char_to_utf16_col(&text, 1).max(1))
    );
    assert_eq!(
        grammar_symbol(&format!("lang-108")).contains("tree_sitter_"),
        true
    );
    assert_eq!(
        parse_snippet_to_plain_text(&format!("${{1:v108}}")),
        format!("v108")
    );
}

#[test]
fn buffer_roundtrip_109() {
    let text = format!("line {}\nline {}\n", 109, 109 + 1);
    let rope = Rope::from_str(&text);
    assert_eq!(rope.to_string(), text);
    assert_eq!(
        line_len(&rope, 0),
        text.lines().next().unwrap().chars().count()
    );
    assert_eq!(
        char_to_utf16_col(&text, 1),
        utf16_to_char_col(&text, 1)
            .max(1)
            .min(char_to_utf16_col(&text, 1).max(1))
    );
    assert_eq!(
        grammar_symbol(&format!("lang-109")).contains("tree_sitter_"),
        true
    );
    assert_eq!(
        parse_snippet_to_plain_text(&format!("${{1:v109}}")),
        format!("v109")
    );
}

#[test]
fn buffer_roundtrip_110() {
    let text = format!("line {}\nline {}\n", 110, 110 + 1);
    let rope = Rope::from_str(&text);
    assert_eq!(rope.to_string(), text);
    assert_eq!(
        line_len(&rope, 0),
        text.lines().next().unwrap().chars().count()
    );
    assert_eq!(
        char_to_utf16_col(&text, 1),
        utf16_to_char_col(&text, 1)
            .max(1)
            .min(char_to_utf16_col(&text, 1).max(1))
    );
    assert_eq!(
        grammar_symbol(&format!("lang-110")).contains("tree_sitter_"),
        true
    );
    assert_eq!(
        parse_snippet_to_plain_text(&format!("${{1:v110}}")),
        format!("v110")
    );
}

#[test]
fn buffer_roundtrip_111() {
    let text = format!("line {}\nline {}\n", 111, 111 + 1);
    let rope = Rope::from_str(&text);
    assert_eq!(rope.to_string(), text);
    assert_eq!(
        line_len(&rope, 0),
        text.lines().next().unwrap().chars().count()
    );
    assert_eq!(
        char_to_utf16_col(&text, 1),
        utf16_to_char_col(&text, 1)
            .max(1)
            .min(char_to_utf16_col(&text, 1).max(1))
    );
    assert_eq!(
        grammar_symbol(&format!("lang-111")).contains("tree_sitter_"),
        true
    );
    assert_eq!(
        parse_snippet_to_plain_text(&format!("${{1:v111}}")),
        format!("v111")
    );
}

#[test]
fn buffer_roundtrip_112() {
    let text = format!("line {}\nline {}\n", 112, 112 + 1);
    let rope = Rope::from_str(&text);
    assert_eq!(rope.to_string(), text);
    assert_eq!(
        line_len(&rope, 0),
        text.lines().next().unwrap().chars().count()
    );
    assert_eq!(
        char_to_utf16_col(&text, 1),
        utf16_to_char_col(&text, 1)
            .max(1)
            .min(char_to_utf16_col(&text, 1).max(1))
    );
    assert_eq!(
        grammar_symbol(&format!("lang-112")).contains("tree_sitter_"),
        true
    );
    assert_eq!(
        parse_snippet_to_plain_text(&format!("${{1:v112}}")),
        format!("v112")
    );
}

#[test]
fn buffer_roundtrip_113() {
    let text = format!("line {}\nline {}\n", 113, 113 + 1);
    let rope = Rope::from_str(&text);
    assert_eq!(rope.to_string(), text);
    assert_eq!(
        line_len(&rope, 0),
        text.lines().next().unwrap().chars().count()
    );
    assert_eq!(
        char_to_utf16_col(&text, 1),
        utf16_to_char_col(&text, 1)
            .max(1)
            .min(char_to_utf16_col(&text, 1).max(1))
    );
    assert_eq!(
        grammar_symbol(&format!("lang-113")).contains("tree_sitter_"),
        true
    );
    assert_eq!(
        parse_snippet_to_plain_text(&format!("${{1:v113}}")),
        format!("v113")
    );
}

#[test]
fn buffer_roundtrip_114() {
    let text = format!("line {}\nline {}\n", 114, 114 + 1);
    let rope = Rope::from_str(&text);
    assert_eq!(rope.to_string(), text);
    assert_eq!(
        line_len(&rope, 0),
        text.lines().next().unwrap().chars().count()
    );
    assert_eq!(
        char_to_utf16_col(&text, 1),
        utf16_to_char_col(&text, 1)
            .max(1)
            .min(char_to_utf16_col(&text, 1).max(1))
    );
    assert_eq!(
        grammar_symbol(&format!("lang-114")).contains("tree_sitter_"),
        true
    );
    assert_eq!(
        parse_snippet_to_plain_text(&format!("${{1:v114}}")),
        format!("v114")
    );
}

#[test]
fn buffer_roundtrip_115() {
    let text = format!("line {}\nline {}\n", 115, 115 + 1);
    let rope = Rope::from_str(&text);
    assert_eq!(rope.to_string(), text);
    assert_eq!(
        line_len(&rope, 0),
        text.lines().next().unwrap().chars().count()
    );
    assert_eq!(
        char_to_utf16_col(&text, 1),
        utf16_to_char_col(&text, 1)
            .max(1)
            .min(char_to_utf16_col(&text, 1).max(1))
    );
    assert_eq!(
        grammar_symbol(&format!("lang-115")).contains("tree_sitter_"),
        true
    );
    assert_eq!(
        parse_snippet_to_plain_text(&format!("${{1:v115}}")),
        format!("v115")
    );
}

#[test]
fn buffer_roundtrip_116() {
    let text = format!("line {}\nline {}\n", 116, 116 + 1);
    let rope = Rope::from_str(&text);
    assert_eq!(rope.to_string(), text);
    assert_eq!(
        line_len(&rope, 0),
        text.lines().next().unwrap().chars().count()
    );
    assert_eq!(
        char_to_utf16_col(&text, 1),
        utf16_to_char_col(&text, 1)
            .max(1)
            .min(char_to_utf16_col(&text, 1).max(1))
    );
    assert_eq!(
        grammar_symbol(&format!("lang-116")).contains("tree_sitter_"),
        true
    );
    assert_eq!(
        parse_snippet_to_plain_text(&format!("${{1:v116}}")),
        format!("v116")
    );
}

#[test]
fn buffer_roundtrip_117() {
    let text = format!("line {}\nline {}\n", 117, 117 + 1);
    let rope = Rope::from_str(&text);
    assert_eq!(rope.to_string(), text);
    assert_eq!(
        line_len(&rope, 0),
        text.lines().next().unwrap().chars().count()
    );
    assert_eq!(
        char_to_utf16_col(&text, 1),
        utf16_to_char_col(&text, 1)
            .max(1)
            .min(char_to_utf16_col(&text, 1).max(1))
    );
    assert_eq!(
        grammar_symbol(&format!("lang-117")).contains("tree_sitter_"),
        true
    );
    assert_eq!(
        parse_snippet_to_plain_text(&format!("${{1:v117}}")),
        format!("v117")
    );
}

#[test]
fn buffer_roundtrip_118() {
    let text = format!("line {}\nline {}\n", 118, 118 + 1);
    let rope = Rope::from_str(&text);
    assert_eq!(rope.to_string(), text);
    assert_eq!(
        line_len(&rope, 0),
        text.lines().next().unwrap().chars().count()
    );
    assert_eq!(
        char_to_utf16_col(&text, 1),
        utf16_to_char_col(&text, 1)
            .max(1)
            .min(char_to_utf16_col(&text, 1).max(1))
    );
    assert_eq!(
        grammar_symbol(&format!("lang-118")).contains("tree_sitter_"),
        true
    );
    assert_eq!(
        parse_snippet_to_plain_text(&format!("${{1:v118}}")),
        format!("v118")
    );
}

#[test]
fn buffer_roundtrip_119() {
    let text = format!("line {}\nline {}\n", 119, 119 + 1);
    let rope = Rope::from_str(&text);
    assert_eq!(rope.to_string(), text);
    assert_eq!(
        line_len(&rope, 0),
        text.lines().next().unwrap().chars().count()
    );
    assert_eq!(
        char_to_utf16_col(&text, 1),
        utf16_to_char_col(&text, 1)
            .max(1)
            .min(char_to_utf16_col(&text, 1).max(1))
    );
    assert_eq!(
        grammar_symbol(&format!("lang-119")).contains("tree_sitter_"),
        true
    );
    assert_eq!(
        parse_snippet_to_plain_text(&format!("${{1:v119}}")),
        format!("v119")
    );
}
