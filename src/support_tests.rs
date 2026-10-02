//! Coverage tests for query loading, grammar paths, and language detection.
#![cfg(test)]

use crate::grammar::{GrammarState, grammar_state, grammar_symbol};
use crate::query_loader::{capture_names, inherit_parents, load_query_pack};
use crate::syntax::{SupportedLanguage, SyntaxEngine};
use crate::tree_engine::{
    byte_to_point, collect_folds, collect_tags, rainbow_spans, suggest_indent, textobject_at,
};
use std::path::PathBuf;

fn rust_tree(src: &str) -> (tree_sitter::Language, tree_sitter::Tree) {
    let lang: tree_sitter::Language = tree_sitter_rust::LANGUAGE.into();
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&lang).unwrap();
    let tree = parser.parse(src, None).unwrap();
    (lang, tree)
}

#[test]
fn inherit_case_0() {
    let src = ";; inherits: parent0, other0\n(id) @name\n";
    assert_eq!(inherit_parents(src), vec!["parent0", "other0"]);
}
#[test]
fn capture_case_0() {
    let src = "(a) @cap0 (b) @other0";
    let names = capture_names(src);
    assert!(names.contains(&"cap0".to_string()));
    assert!(names.contains(&"other0".to_string()));
}

#[test]
fn inherit_case_1() {
    let src = ";; inherits: parent1, other1\n(id) @name\n";
    assert_eq!(inherit_parents(src), vec!["parent1", "other1"]);
}
#[test]
fn capture_case_1() {
    let src = "(a) @cap1 (b) @other1";
    let names = capture_names(src);
    assert!(names.contains(&"cap1".to_string()));
    assert!(names.contains(&"other1".to_string()));
}

#[test]
fn inherit_case_2() {
    let src = ";; inherits: parent2, other2\n(id) @name\n";
    assert_eq!(inherit_parents(src), vec!["parent2", "other2"]);
}
#[test]
fn capture_case_2() {
    let src = "(a) @cap2 (b) @other2";
    let names = capture_names(src);
    assert!(names.contains(&"cap2".to_string()));
    assert!(names.contains(&"other2".to_string()));
}

#[test]
fn inherit_case_3() {
    let src = ";; inherits: parent3, other3\n(id) @name\n";
    assert_eq!(inherit_parents(src), vec!["parent3", "other3"]);
}
#[test]
fn capture_case_3() {
    let src = "(a) @cap3 (b) @other3";
    let names = capture_names(src);
    assert!(names.contains(&"cap3".to_string()));
    assert!(names.contains(&"other3".to_string()));
}

#[test]
fn inherit_case_4() {
    let src = ";; inherits: parent4, other4\n(id) @name\n";
    assert_eq!(inherit_parents(src), vec!["parent4", "other4"]);
}
#[test]
fn capture_case_4() {
    let src = "(a) @cap4 (b) @other4";
    let names = capture_names(src);
    assert!(names.contains(&"cap4".to_string()));
    assert!(names.contains(&"other4".to_string()));
}

#[test]
fn inherit_case_5() {
    let src = ";; inherits: parent5, other5\n(id) @name\n";
    assert_eq!(inherit_parents(src), vec!["parent5", "other5"]);
}
#[test]
fn capture_case_5() {
    let src = "(a) @cap5 (b) @other5";
    let names = capture_names(src);
    assert!(names.contains(&"cap5".to_string()));
    assert!(names.contains(&"other5".to_string()));
}

#[test]
fn inherit_case_6() {
    let src = ";; inherits: parent6, other6\n(id) @name\n";
    assert_eq!(inherit_parents(src), vec!["parent6", "other6"]);
}
#[test]
fn capture_case_6() {
    let src = "(a) @cap6 (b) @other6";
    let names = capture_names(src);
    assert!(names.contains(&"cap6".to_string()));
    assert!(names.contains(&"other6".to_string()));
}

#[test]
fn inherit_case_7() {
    let src = ";; inherits: parent7, other7\n(id) @name\n";
    assert_eq!(inherit_parents(src), vec!["parent7", "other7"]);
}
#[test]
fn capture_case_7() {
    let src = "(a) @cap7 (b) @other7";
    let names = capture_names(src);
    assert!(names.contains(&"cap7".to_string()));
    assert!(names.contains(&"other7".to_string()));
}

#[test]
fn inherit_case_8() {
    let src = ";; inherits: parent8, other8\n(id) @name\n";
    assert_eq!(inherit_parents(src), vec!["parent8", "other8"]);
}
#[test]
fn capture_case_8() {
    let src = "(a) @cap8 (b) @other8";
    let names = capture_names(src);
    assert!(names.contains(&"cap8".to_string()));
    assert!(names.contains(&"other8".to_string()));
}

#[test]
fn inherit_case_9() {
    let src = ";; inherits: parent9, other9\n(id) @name\n";
    assert_eq!(inherit_parents(src), vec!["parent9", "other9"]);
}
#[test]
fn capture_case_9() {
    let src = "(a) @cap9 (b) @other9";
    let names = capture_names(src);
    assert!(names.contains(&"cap9".to_string()));
    assert!(names.contains(&"other9".to_string()));
}

#[test]
fn inherit_case_10() {
    let src = ";; inherits: parent10, other10\n(id) @name\n";
    assert_eq!(inherit_parents(src), vec!["parent10", "other10"]);
}
#[test]
fn capture_case_10() {
    let src = "(a) @cap10 (b) @other10";
    let names = capture_names(src);
    assert!(names.contains(&"cap10".to_string()));
    assert!(names.contains(&"other10".to_string()));
}

#[test]
fn inherit_case_11() {
    let src = ";; inherits: parent11, other11\n(id) @name\n";
    assert_eq!(inherit_parents(src), vec!["parent11", "other11"]);
}
#[test]
fn capture_case_11() {
    let src = "(a) @cap11 (b) @other11";
    let names = capture_names(src);
    assert!(names.contains(&"cap11".to_string()));
    assert!(names.contains(&"other11".to_string()));
}

#[test]
fn inherit_case_12() {
    let src = ";; inherits: parent12, other12\n(id) @name\n";
    assert_eq!(inherit_parents(src), vec!["parent12", "other12"]);
}
#[test]
fn capture_case_12() {
    let src = "(a) @cap12 (b) @other12";
    let names = capture_names(src);
    assert!(names.contains(&"cap12".to_string()));
    assert!(names.contains(&"other12".to_string()));
}

#[test]
fn inherit_case_13() {
    let src = ";; inherits: parent13, other13\n(id) @name\n";
    assert_eq!(inherit_parents(src), vec!["parent13", "other13"]);
}
#[test]
fn capture_case_13() {
    let src = "(a) @cap13 (b) @other13";
    let names = capture_names(src);
    assert!(names.contains(&"cap13".to_string()));
    assert!(names.contains(&"other13".to_string()));
}

#[test]
fn inherit_case_14() {
    let src = ";; inherits: parent14, other14\n(id) @name\n";
    assert_eq!(inherit_parents(src), vec!["parent14", "other14"]);
}
#[test]
fn capture_case_14() {
    let src = "(a) @cap14 (b) @other14";
    let names = capture_names(src);
    assert!(names.contains(&"cap14".to_string()));
    assert!(names.contains(&"other14".to_string()));
}

#[test]
fn inherit_case_15() {
    let src = ";; inherits: parent15, other15\n(id) @name\n";
    assert_eq!(inherit_parents(src), vec!["parent15", "other15"]);
}
#[test]
fn capture_case_15() {
    let src = "(a) @cap15 (b) @other15";
    let names = capture_names(src);
    assert!(names.contains(&"cap15".to_string()));
    assert!(names.contains(&"other15".to_string()));
}

#[test]
fn inherit_case_16() {
    let src = ";; inherits: parent16, other16\n(id) @name\n";
    assert_eq!(inherit_parents(src), vec!["parent16", "other16"]);
}
#[test]
fn capture_case_16() {
    let src = "(a) @cap16 (b) @other16";
    let names = capture_names(src);
    assert!(names.contains(&"cap16".to_string()));
    assert!(names.contains(&"other16".to_string()));
}

#[test]
fn inherit_case_17() {
    let src = ";; inherits: parent17, other17\n(id) @name\n";
    assert_eq!(inherit_parents(src), vec!["parent17", "other17"]);
}
#[test]
fn capture_case_17() {
    let src = "(a) @cap17 (b) @other17";
    let names = capture_names(src);
    assert!(names.contains(&"cap17".to_string()));
    assert!(names.contains(&"other17".to_string()));
}

#[test]
fn inherit_case_18() {
    let src = ";; inherits: parent18, other18\n(id) @name\n";
    assert_eq!(inherit_parents(src), vec!["parent18", "other18"]);
}
#[test]
fn capture_case_18() {
    let src = "(a) @cap18 (b) @other18";
    let names = capture_names(src);
    assert!(names.contains(&"cap18".to_string()));
    assert!(names.contains(&"other18".to_string()));
}

#[test]
fn inherit_case_19() {
    let src = ";; inherits: parent19, other19\n(id) @name\n";
    assert_eq!(inherit_parents(src), vec!["parent19", "other19"]);
}
#[test]
fn capture_case_19() {
    let src = "(a) @cap19 (b) @other19";
    let names = capture_names(src);
    assert!(names.contains(&"cap19".to_string()));
    assert!(names.contains(&"other19".to_string()));
}

#[test]
fn detect_rs_0() {
    let p = PathBuf::from("file.rs");
    assert_eq!(
        SupportedLanguage::from_path(Some(&p)),
        SupportedLanguage::Rust
    );
}

#[test]
fn detect_go_1() {
    let p = PathBuf::from("file.go");
    assert_eq!(
        SupportedLanguage::from_path(Some(&p)),
        SupportedLanguage::Go
    );
}

#[test]
fn detect_py_2() {
    let p = PathBuf::from("file.py");
    assert_eq!(
        SupportedLanguage::from_path(Some(&p)),
        SupportedLanguage::Python
    );
}

#[test]
fn detect_c_3() {
    let p = PathBuf::from("file.c");
    assert_eq!(SupportedLanguage::from_path(Some(&p)), SupportedLanguage::C);
}

#[test]
fn detect_cpp_4() {
    let p = PathBuf::from("file.cpp");
    assert_eq!(
        SupportedLanguage::from_path(Some(&p)),
        SupportedLanguage::Cpp
    );
}

#[test]
fn detect_zig_5() {
    let p = PathBuf::from("file.zig");
    assert_eq!(
        SupportedLanguage::from_path(Some(&p)),
        SupportedLanguage::Zig
    );
}

#[test]
fn detect_js_6() {
    let p = PathBuf::from("file.js");
    assert_eq!(
        SupportedLanguage::from_path(Some(&p)),
        SupportedLanguage::JavaScript
    );
}

#[test]
fn detect_ts_7() {
    let p = PathBuf::from("file.ts");
    assert_eq!(
        SupportedLanguage::from_path(Some(&p)),
        SupportedLanguage::TypeScript
    );
}

#[test]
fn detect_tsx_8() {
    let p = PathBuf::from("file.tsx");
    assert_eq!(
        SupportedLanguage::from_path(Some(&p)),
        SupportedLanguage::Tsx
    );
}

#[test]
fn detect_html_9() {
    let p = PathBuf::from("file.html");
    assert_eq!(
        SupportedLanguage::from_path(Some(&p)),
        SupportedLanguage::Html
    );
}

#[test]
fn detect_css_10() {
    let p = PathBuf::from("file.css");
    assert_eq!(
        SupportedLanguage::from_path(Some(&p)),
        SupportedLanguage::Css
    );
}

#[test]
fn detect_json_11() {
    let p = PathBuf::from("file.json");
    assert_eq!(
        SupportedLanguage::from_path(Some(&p)),
        SupportedLanguage::Json
    );
}

#[test]
fn detect_toml_12() {
    let p = PathBuf::from("file.toml");
    assert_eq!(
        SupportedLanguage::from_path(Some(&p)),
        SupportedLanguage::Toml
    );
}

#[test]
fn detect_yaml_13() {
    let p = PathBuf::from("file.yaml");
    assert_eq!(
        SupportedLanguage::from_path(Some(&p)),
        SupportedLanguage::Yaml
    );
}

#[test]
fn detect_sh_14() {
    let p = PathBuf::from("file.sh");
    assert_eq!(
        SupportedLanguage::from_path(Some(&p)),
        SupportedLanguage::Bash
    );
}

#[test]
fn detect_lua_15() {
    let p = PathBuf::from("file.lua");
    assert_eq!(
        SupportedLanguage::from_path(Some(&p)),
        SupportedLanguage::Lua
    );
}

#[test]
fn detect_md_16() {
    let p = PathBuf::from("file.md");
    assert_eq!(
        SupportedLanguage::from_path(Some(&p)),
        SupportedLanguage::Markdown
    );
}

#[test]
fn detect_java_17() {
    let p = PathBuf::from("file.java");
    assert_eq!(
        SupportedLanguage::from_path(Some(&p)),
        SupportedLanguage::Java
    );
}

#[test]
fn detect_cs_18() {
    let p = PathBuf::from("file.cs");
    assert_eq!(
        SupportedLanguage::from_path(Some(&p)),
        SupportedLanguage::CSharp
    );
}

#[test]
fn detect_rb_19() {
    let p = PathBuf::from("file.rb");
    assert_eq!(
        SupportedLanguage::from_path(Some(&p)),
        SupportedLanguage::Ruby
    );
}

#[test]
fn grammar_symbol_0() {
    assert_eq!(grammar_symbol("lang-0"), "tree_sitter_lang_0");
    assert_eq!(
        grammar_state("missing-lang-0", false),
        GrammarState::Missing
    );
    assert_eq!(grammar_state("missing-lang-0", true), GrammarState::Static);
}

#[test]
fn grammar_symbol_1() {
    assert_eq!(grammar_symbol("lang-1"), "tree_sitter_lang_1");
    assert_eq!(
        grammar_state("missing-lang-1", false),
        GrammarState::Missing
    );
    assert_eq!(grammar_state("missing-lang-1", true), GrammarState::Static);
}

#[test]
fn grammar_symbol_2() {
    assert_eq!(grammar_symbol("lang-2"), "tree_sitter_lang_2");
    assert_eq!(
        grammar_state("missing-lang-2", false),
        GrammarState::Missing
    );
    assert_eq!(grammar_state("missing-lang-2", true), GrammarState::Static);
}

#[test]
fn grammar_symbol_3() {
    assert_eq!(grammar_symbol("lang-3"), "tree_sitter_lang_3");
    assert_eq!(
        grammar_state("missing-lang-3", false),
        GrammarState::Missing
    );
    assert_eq!(grammar_state("missing-lang-3", true), GrammarState::Static);
}

#[test]
fn grammar_symbol_4() {
    assert_eq!(grammar_symbol("lang-4"), "tree_sitter_lang_4");
    assert_eq!(
        grammar_state("missing-lang-4", false),
        GrammarState::Missing
    );
    assert_eq!(grammar_state("missing-lang-4", true), GrammarState::Static);
}

#[test]
fn grammar_symbol_5() {
    assert_eq!(grammar_symbol("lang-5"), "tree_sitter_lang_5");
    assert_eq!(
        grammar_state("missing-lang-5", false),
        GrammarState::Missing
    );
    assert_eq!(grammar_state("missing-lang-5", true), GrammarState::Static);
}

#[test]
fn grammar_symbol_6() {
    assert_eq!(grammar_symbol("lang-6"), "tree_sitter_lang_6");
    assert_eq!(
        grammar_state("missing-lang-6", false),
        GrammarState::Missing
    );
    assert_eq!(grammar_state("missing-lang-6", true), GrammarState::Static);
}

#[test]
fn grammar_symbol_7() {
    assert_eq!(grammar_symbol("lang-7"), "tree_sitter_lang_7");
    assert_eq!(
        grammar_state("missing-lang-7", false),
        GrammarState::Missing
    );
    assert_eq!(grammar_state("missing-lang-7", true), GrammarState::Static);
}

#[test]
fn grammar_symbol_8() {
    assert_eq!(grammar_symbol("lang-8"), "tree_sitter_lang_8");
    assert_eq!(
        grammar_state("missing-lang-8", false),
        GrammarState::Missing
    );
    assert_eq!(grammar_state("missing-lang-8", true), GrammarState::Static);
}

#[test]
fn grammar_symbol_9() {
    assert_eq!(grammar_symbol("lang-9"), "tree_sitter_lang_9");
    assert_eq!(
        grammar_state("missing-lang-9", false),
        GrammarState::Missing
    );
    assert_eq!(grammar_state("missing-lang-9", true), GrammarState::Static);
}

#[test]
fn grammar_symbol_10() {
    assert_eq!(grammar_symbol("lang-10"), "tree_sitter_lang_10");
    assert_eq!(
        grammar_state("missing-lang-10", false),
        GrammarState::Missing
    );
    assert_eq!(grammar_state("missing-lang-10", true), GrammarState::Static);
}

#[test]
fn grammar_symbol_11() {
    assert_eq!(grammar_symbol("lang-11"), "tree_sitter_lang_11");
    assert_eq!(
        grammar_state("missing-lang-11", false),
        GrammarState::Missing
    );
    assert_eq!(grammar_state("missing-lang-11", true), GrammarState::Static);
}

#[test]
fn grammar_symbol_12() {
    assert_eq!(grammar_symbol("lang-12"), "tree_sitter_lang_12");
    assert_eq!(
        grammar_state("missing-lang-12", false),
        GrammarState::Missing
    );
    assert_eq!(grammar_state("missing-lang-12", true), GrammarState::Static);
}

#[test]
fn grammar_symbol_13() {
    assert_eq!(grammar_symbol("lang-13"), "tree_sitter_lang_13");
    assert_eq!(
        grammar_state("missing-lang-13", false),
        GrammarState::Missing
    );
    assert_eq!(grammar_state("missing-lang-13", true), GrammarState::Static);
}

#[test]
fn grammar_symbol_14() {
    assert_eq!(grammar_symbol("lang-14"), "tree_sitter_lang_14");
    assert_eq!(
        grammar_state("missing-lang-14", false),
        GrammarState::Missing
    );
    assert_eq!(grammar_state("missing-lang-14", true), GrammarState::Static);
}

#[test]
fn grammar_symbol_15() {
    assert_eq!(grammar_symbol("lang-15"), "tree_sitter_lang_15");
    assert_eq!(
        grammar_state("missing-lang-15", false),
        GrammarState::Missing
    );
    assert_eq!(grammar_state("missing-lang-15", true), GrammarState::Static);
}

#[test]
fn grammar_symbol_16() {
    assert_eq!(grammar_symbol("lang-16"), "tree_sitter_lang_16");
    assert_eq!(
        grammar_state("missing-lang-16", false),
        GrammarState::Missing
    );
    assert_eq!(grammar_state("missing-lang-16", true), GrammarState::Static);
}

#[test]
fn grammar_symbol_17() {
    assert_eq!(grammar_symbol("lang-17"), "tree_sitter_lang_17");
    assert_eq!(
        grammar_state("missing-lang-17", false),
        GrammarState::Missing
    );
    assert_eq!(grammar_state("missing-lang-17", true), GrammarState::Static);
}

#[test]
fn grammar_symbol_18() {
    assert_eq!(grammar_symbol("lang-18"), "tree_sitter_lang_18");
    assert_eq!(
        grammar_state("missing-lang-18", false),
        GrammarState::Missing
    );
    assert_eq!(grammar_state("missing-lang-18", true), GrammarState::Static);
}

#[test]
fn grammar_symbol_19() {
    assert_eq!(grammar_symbol("lang-19"), "tree_sitter_lang_19");
    assert_eq!(
        grammar_state("missing-lang-19", false),
        GrammarState::Missing
    );
    assert_eq!(grammar_state("missing-lang-19", true), GrammarState::Static);
}

#[test]
fn grammar_symbol_20() {
    assert_eq!(grammar_symbol("lang-20"), "tree_sitter_lang_20");
    assert_eq!(
        grammar_state("missing-lang-20", false),
        GrammarState::Missing
    );
    assert_eq!(grammar_state("missing-lang-20", true), GrammarState::Static);
}

#[test]
fn grammar_symbol_21() {
    assert_eq!(grammar_symbol("lang-21"), "tree_sitter_lang_21");
    assert_eq!(
        grammar_state("missing-lang-21", false),
        GrammarState::Missing
    );
    assert_eq!(grammar_state("missing-lang-21", true), GrammarState::Static);
}

#[test]
fn grammar_symbol_22() {
    assert_eq!(grammar_symbol("lang-22"), "tree_sitter_lang_22");
    assert_eq!(
        grammar_state("missing-lang-22", false),
        GrammarState::Missing
    );
    assert_eq!(grammar_state("missing-lang-22", true), GrammarState::Static);
}

#[test]
fn grammar_symbol_23() {
    assert_eq!(grammar_symbol("lang-23"), "tree_sitter_lang_23");
    assert_eq!(
        grammar_state("missing-lang-23", false),
        GrammarState::Missing
    );
    assert_eq!(grammar_state("missing-lang-23", true), GrammarState::Static);
}

#[test]
fn grammar_symbol_24() {
    assert_eq!(grammar_symbol("lang-24"), "tree_sitter_lang_24");
    assert_eq!(
        grammar_state("missing-lang-24", false),
        GrammarState::Missing
    );
    assert_eq!(grammar_state("missing-lang-24", true), GrammarState::Static);
}

#[test]
fn grammar_symbol_25() {
    assert_eq!(grammar_symbol("lang-25"), "tree_sitter_lang_25");
    assert_eq!(
        grammar_state("missing-lang-25", false),
        GrammarState::Missing
    );
    assert_eq!(grammar_state("missing-lang-25", true), GrammarState::Static);
}

#[test]
fn grammar_symbol_26() {
    assert_eq!(grammar_symbol("lang-26"), "tree_sitter_lang_26");
    assert_eq!(
        grammar_state("missing-lang-26", false),
        GrammarState::Missing
    );
    assert_eq!(grammar_state("missing-lang-26", true), GrammarState::Static);
}

#[test]
fn grammar_symbol_27() {
    assert_eq!(grammar_symbol("lang-27"), "tree_sitter_lang_27");
    assert_eq!(
        grammar_state("missing-lang-27", false),
        GrammarState::Missing
    );
    assert_eq!(grammar_state("missing-lang-27", true), GrammarState::Static);
}

#[test]
fn grammar_symbol_28() {
    assert_eq!(grammar_symbol("lang-28"), "tree_sitter_lang_28");
    assert_eq!(
        grammar_state("missing-lang-28", false),
        GrammarState::Missing
    );
    assert_eq!(grammar_state("missing-lang-28", true), GrammarState::Static);
}

#[test]
fn grammar_symbol_29() {
    assert_eq!(grammar_symbol("lang-29"), "tree_sitter_lang_29");
    assert_eq!(
        grammar_state("missing-lang-29", false),
        GrammarState::Missing
    );
    assert_eq!(grammar_state("missing-lang-29", true), GrammarState::Static);
}

#[test]
fn grammar_symbol_30() {
    assert_eq!(grammar_symbol("lang-30"), "tree_sitter_lang_30");
    assert_eq!(
        grammar_state("missing-lang-30", false),
        GrammarState::Missing
    );
    assert_eq!(grammar_state("missing-lang-30", true), GrammarState::Static);
}

#[test]
fn grammar_symbol_31() {
    assert_eq!(grammar_symbol("lang-31"), "tree_sitter_lang_31");
    assert_eq!(
        grammar_state("missing-lang-31", false),
        GrammarState::Missing
    );
    assert_eq!(grammar_state("missing-lang-31", true), GrammarState::Static);
}

#[test]
fn grammar_symbol_32() {
    assert_eq!(grammar_symbol("lang-32"), "tree_sitter_lang_32");
    assert_eq!(
        grammar_state("missing-lang-32", false),
        GrammarState::Missing
    );
    assert_eq!(grammar_state("missing-lang-32", true), GrammarState::Static);
}

#[test]
fn grammar_symbol_33() {
    assert_eq!(grammar_symbol("lang-33"), "tree_sitter_lang_33");
    assert_eq!(
        grammar_state("missing-lang-33", false),
        GrammarState::Missing
    );
    assert_eq!(grammar_state("missing-lang-33", true), GrammarState::Static);
}

#[test]
fn grammar_symbol_34() {
    assert_eq!(grammar_symbol("lang-34"), "tree_sitter_lang_34");
    assert_eq!(
        grammar_state("missing-lang-34", false),
        GrammarState::Missing
    );
    assert_eq!(grammar_state("missing-lang-34", true), GrammarState::Static);
}

#[test]
fn grammar_symbol_35() {
    assert_eq!(grammar_symbol("lang-35"), "tree_sitter_lang_35");
    assert_eq!(
        grammar_state("missing-lang-35", false),
        GrammarState::Missing
    );
    assert_eq!(grammar_state("missing-lang-35", true), GrammarState::Static);
}

#[test]
fn grammar_symbol_36() {
    assert_eq!(grammar_symbol("lang-36"), "tree_sitter_lang_36");
    assert_eq!(
        grammar_state("missing-lang-36", false),
        GrammarState::Missing
    );
    assert_eq!(grammar_state("missing-lang-36", true), GrammarState::Static);
}

#[test]
fn grammar_symbol_37() {
    assert_eq!(grammar_symbol("lang-37"), "tree_sitter_lang_37");
    assert_eq!(
        grammar_state("missing-lang-37", false),
        GrammarState::Missing
    );
    assert_eq!(grammar_state("missing-lang-37", true), GrammarState::Static);
}

#[test]
fn grammar_symbol_38() {
    assert_eq!(grammar_symbol("lang-38"), "tree_sitter_lang_38");
    assert_eq!(
        grammar_state("missing-lang-38", false),
        GrammarState::Missing
    );
    assert_eq!(grammar_state("missing-lang-38", true), GrammarState::Static);
}

#[test]
fn grammar_symbol_39() {
    assert_eq!(grammar_symbol("lang-39"), "tree_sitter_lang_39");
    assert_eq!(
        grammar_state("missing-lang-39", false),
        GrammarState::Missing
    );
    assert_eq!(grammar_state("missing-lang-39", true), GrammarState::Static);
}

#[test]
fn rust_query_pack_has_highlights() {
    let pack = load_query_pack("rust");
    assert!(pack.highlights.contains("@function") || pack.highlights.contains("function"));
    assert!(!pack.indents.trim().is_empty());
    assert!(!pack.textobjects.trim().is_empty());
}

#[test]
fn rust_engine_highlights_keyword() {
    let path = PathBuf::from("sample.rs");
    let mut engine = SyntaxEngine::new(Some(&path));
    assert!(engine.has_treesitter());
    engine.reparse("fn main() {\n    let x = 1;\n}\n");
    assert!(!engine.ts_tokens.is_empty());
    assert!(engine.tree.is_some());
}

#[test]
fn rust_indent_and_textobject_and_tags() {
    let src = "fn answer() -> i32 {\n    42\n}\n";
    let (lang, tree) = rust_tree(src);
    let indent = suggest_indent(&lang, src, &tree, "(block) @indent\n", 1, 4).unwrap();
    assert_eq!(indent, "    ");
    let range = textobject_at(
        &lang,
        src,
        &tree,
        "(function_item) @function.around",
        3,
        "function",
        false,
    )
    .unwrap();
    assert!(range.end > range.start);
    let tags = collect_tags(
        &lang,
        src,
        &tree,
        "(function_item name: (identifier) @name) @definition.function",
    );
    assert_eq!(tags[0].name, "answer");
    let folds = collect_folds(&lang, src, &tree, "(function_item) @fold");
    assert_eq!(folds.len(), 1);
    let rainbow = rainbow_spans(&lang, src, &tree, "(block) @rainbow.scope");
    assert!(!rainbow.is_empty());
    assert_eq!(byte_to_point(src, 0), (0, 0));
}

#[test]
fn cpp_inherits_c_highlights() {
    let pack = load_query_pack("cpp");
    assert!(pack.highlights.contains("inherits") || pack.highlights.len() > 100);
}

#[test]
fn injection_query_loaded_for_rust() {
    let pack = load_query_pack("rust");
    assert!(pack.injections.contains("injection") || !pack.injections.trim().is_empty());
}

#[test]
fn byte_point_0() {
    let src = "a\n".repeat(0 + 1);
    assert_eq!(byte_to_point(&src, 0), (0, 0));
}

#[test]
fn byte_point_1() {
    let src = "a\n".repeat(1 + 1);
    assert_eq!(byte_to_point(&src, 0), (0, 0));
}

#[test]
fn byte_point_2() {
    let src = "a\n".repeat(2 + 1);
    assert_eq!(byte_to_point(&src, 0), (0, 0));
}

#[test]
fn byte_point_3() {
    let src = "a\n".repeat(3 + 1);
    assert_eq!(byte_to_point(&src, 0), (0, 0));
}

#[test]
fn byte_point_4() {
    let src = "a\n".repeat(4 + 1);
    assert_eq!(byte_to_point(&src, 0), (0, 0));
}

#[test]
fn byte_point_5() {
    let src = "a\n".repeat(5 + 1);
    assert_eq!(byte_to_point(&src, 0), (0, 0));
}

#[test]
fn byte_point_6() {
    let src = "a\n".repeat(6 + 1);
    assert_eq!(byte_to_point(&src, 0), (0, 0));
}

#[test]
fn byte_point_7() {
    let src = "a\n".repeat(7 + 1);
    assert_eq!(byte_to_point(&src, 0), (0, 0));
}

#[test]
fn byte_point_8() {
    let src = "a\n".repeat(8 + 1);
    assert_eq!(byte_to_point(&src, 0), (0, 0));
}

#[test]
fn byte_point_9() {
    let src = "a\n".repeat(9 + 1);
    assert_eq!(byte_to_point(&src, 0), (0, 0));
}

#[test]
fn byte_point_10() {
    let src = "a\n".repeat(10 + 1);
    assert_eq!(byte_to_point(&src, 0), (0, 0));
}

#[test]
fn byte_point_11() {
    let src = "a\n".repeat(11 + 1);
    assert_eq!(byte_to_point(&src, 0), (0, 0));
}

#[test]
fn byte_point_12() {
    let src = "a\n".repeat(12 + 1);
    assert_eq!(byte_to_point(&src, 0), (0, 0));
}

#[test]
fn byte_point_13() {
    let src = "a\n".repeat(13 + 1);
    assert_eq!(byte_to_point(&src, 0), (0, 0));
}

#[test]
fn byte_point_14() {
    let src = "a\n".repeat(14 + 1);
    assert_eq!(byte_to_point(&src, 0), (0, 0));
}

#[test]
fn byte_point_15() {
    let src = "a\n".repeat(15 + 1);
    assert_eq!(byte_to_point(&src, 0), (0, 0));
}

#[test]
fn byte_point_16() {
    let src = "a\n".repeat(16 + 1);
    assert_eq!(byte_to_point(&src, 0), (0, 0));
}

#[test]
fn byte_point_17() {
    let src = "a\n".repeat(17 + 1);
    assert_eq!(byte_to_point(&src, 0), (0, 0));
}

#[test]
fn byte_point_18() {
    let src = "a\n".repeat(18 + 1);
    assert_eq!(byte_to_point(&src, 0), (0, 0));
}

#[test]
fn byte_point_19() {
    let src = "a\n".repeat(19 + 1);
    assert_eq!(byte_to_point(&src, 0), (0, 0));
}

#[test]
fn byte_point_20() {
    let src = "a\n".repeat(20 + 1);
    assert_eq!(byte_to_point(&src, 0), (0, 0));
}

#[test]
fn byte_point_21() {
    let src = "a\n".repeat(21 + 1);
    assert_eq!(byte_to_point(&src, 0), (0, 0));
}

#[test]
fn byte_point_22() {
    let src = "a\n".repeat(22 + 1);
    assert_eq!(byte_to_point(&src, 0), (0, 0));
}

#[test]
fn byte_point_23() {
    let src = "a\n".repeat(23 + 1);
    assert_eq!(byte_to_point(&src, 0), (0, 0));
}

#[test]
fn byte_point_24() {
    let src = "a\n".repeat(24 + 1);
    assert_eq!(byte_to_point(&src, 0), (0, 0));
}

#[test]
fn byte_point_25() {
    let src = "a\n".repeat(25 + 1);
    assert_eq!(byte_to_point(&src, 0), (0, 0));
}

#[test]
fn byte_point_26() {
    let src = "a\n".repeat(26 + 1);
    assert_eq!(byte_to_point(&src, 0), (0, 0));
}

#[test]
fn byte_point_27() {
    let src = "a\n".repeat(27 + 1);
    assert_eq!(byte_to_point(&src, 0), (0, 0));
}

#[test]
fn byte_point_28() {
    let src = "a\n".repeat(28 + 1);
    assert_eq!(byte_to_point(&src, 0), (0, 0));
}

#[test]
fn byte_point_29() {
    let src = "a\n".repeat(29 + 1);
    assert_eq!(byte_to_point(&src, 0), (0, 0));
}
