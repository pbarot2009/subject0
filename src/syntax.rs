//! # Tree-Sitter & Semantic Syntax Highlighting Engine
//!
//! Provides AST-based syntax highlighting, dynamic query evaluation, and icon/color
//! resolution for languages supported by `subject0`.

use std::{collections::HashMap, fs, path::PathBuf};

use anyhow::{Result, anyhow};
use ratatui::{
    style::{Color, Modifier, Style},
    text::Span,
};
use tree_sitter_highlight::{Highlight, HighlightConfiguration, HighlightEvent, Highlighter};

use crate::lsp::{
    CanonicalTokenType, SemanticTokenSpan, resolve_binary_path, subject0_config_dir,
    subject0_data_dir,
};
use crate::nerdfonts::*;
use crate::theme::Theme;

// === Custom Highlight Query Paths & Dynamic Grammar Stubs ===

pub fn query_file_path(lang_name: &str) -> Option<PathBuf> {
    if lang_name.is_empty() {
        return None;
    }
    let dirs = [
        subject0_config_dir().join("queries").join(lang_name),
        subject0_data_dir().join("queries").join(lang_name),
        PathBuf::from("./queries").join(lang_name),
        PathBuf::from("./runtime/queries").join(lang_name),
    ];

    for d in &dirs {
        let p = d.join("highlights.scm");
        if p.is_file() {
            return Some(p);
        }
    }
    None
}

pub struct DynamicGrammar;

impl DynamicGrammar {
    pub fn grammar_file_path(lang_name: &str) -> Option<PathBuf> {
        query_file_path(lang_name)
    }

    #[allow(dead_code)]
    pub fn download_wasm_grammar(lang_name: &str) -> Result<PathBuf> {
        let sl = SupportedLanguage::all()
            .iter()
            .copied()
            .find(|l| l.grammar_name() == lang_name || l.lsp_id() == lang_name);

        if let Some(lang) = sl
            && lang.static_language().is_some()
        {
            return Ok(PathBuf::from("built-in"));
        }

        Err(anyhow!(
            "Language '{lang_name}' uses LSP semantic highlighting tier."
        ))
    }
}

// === Supported Languages Specification ===

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SupportedLanguage {
    Rust,
    Go,
    Python,
    C,
    Cpp,
    Zig,
    JavaScript,
    TypeScript,
    Tsx,
    Html,
    Css,
    Json,
    Toml,
    Yaml,
    Bash,
    Lua,
    Markdown,
    Java,
    CSharp,
    Php,
    Ruby,
    Kotlin,
    Swift,
    Dart,
    Sql,
    Scala,
    Odin,
    Haskell,
    Elixir,
    Erlang,
    OCaml,
    FSharp,
    Elm,
    Julia,
    Nim,
    Crystal,
    Clojure,
    Nix,
    Gleam,
    Terraform,
    Vue,
    Svelte,
    Astro,
    Perl,
    R,
    Racket,
    Scheme,
    CommonLisp,
    PureScript,
    Fortran,
    D,
    V,
    Zsh,
    Fish,
    PowerShell,
    Groovy,
    Gradle,
    ObjectiveC,
    ObjectiveCpp,
    Cuda,
    Glsl,
    Hlsl,
    Wgsl,
    Solidity,
    Move,
    Cairo,
    Haxe,
    Pascal,
    Ada,
    Cobol,
    Prolog,
    Tcl,
    Awk,
    Makefile,
    Cmake,
    Dockerfile,
    GraphQL,
    Proto,
    Thrift,
    Xml,
    Ini,
    Csv,
    Properties,
    EnvFile,
    Hcl,
    Jsonnet,
    Dhall,
    Nginx,
    Diff,
    Regex,
    Vim,
    EmacsLisp,
    Latex,
    Bibtex,
    Typst,
    Rst,
    Org,
    Assembly,
    Verilog,
    Vhdl,
    Zephyr,
    Janet,
    Wren,
    Vala,
    Gd,
    Plain,
}

#[allow(non_upper_case_globals, dead_code)]
pub const VHDL: SupportedLanguage = SupportedLanguage::Vhdl;

impl SupportedLanguage {
    pub fn from_path(path: Option<&PathBuf>) -> Self {
        let file_name = path
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
            .unwrap_or("");
        let ext = path
            .and_then(|p| p.extension())
            .and_then(|e| e.to_str())
            .unwrap_or("");

        match file_name {
            "Dockerfile" | "Containerfile" => return SupportedLanguage::Dockerfile,
            "Makefile" | "makefile" | "GNUmakefile" => return SupportedLanguage::Makefile,
            "CMakeLists.txt" => return SupportedLanguage::Cmake,
            ".gitignore" | ".dockerignore" | ".npmignore" => return SupportedLanguage::Plain,
            ".bashrc" | ".bash_profile" | ".zshrc" => return SupportedLanguage::Bash,
            ".env" => return SupportedLanguage::EnvFile,
            "nginx.conf" => return SupportedLanguage::Nginx,
            _ => {}
        }

        match ext {
            "rs" => SupportedLanguage::Rust,
            "go" => SupportedLanguage::Go,
            "py" | "pyi" | "pyw" => SupportedLanguage::Python,
            "c" | "h" => SupportedLanguage::C,
            "cpp" | "hpp" | "cc" | "cxx" | "hh" | "hxx" | "c++" => SupportedLanguage::Cpp,
            "zig" | "zon" => SupportedLanguage::Zig,
            "js" | "jsx" | "mjs" | "cjs" => SupportedLanguage::JavaScript,
            "ts" | "mts" | "cts" => SupportedLanguage::TypeScript,
            "tsx" => SupportedLanguage::Tsx,
            "html" | "htm" | "xhtml" => SupportedLanguage::Html,
            "css" | "scss" | "less" => SupportedLanguage::Css,
            "json" | "jsonc" | "json5" => SupportedLanguage::Json,
            "toml" => SupportedLanguage::Toml,
            "yaml" | "yml" => SupportedLanguage::Yaml,
            "sh" | "bash" => SupportedLanguage::Bash,
            "lua" => SupportedLanguage::Lua,
            "md" | "markdown" | "mdx" => SupportedLanguage::Markdown,
            "java" => SupportedLanguage::Java,
            "cs" | "csx" => SupportedLanguage::CSharp,
            "php" | "phtml" => SupportedLanguage::Php,
            "rb" | "rake" | "gemspec" => SupportedLanguage::Ruby,
            "kt" | "kts" => SupportedLanguage::Kotlin,
            "swift" => SupportedLanguage::Swift,
            "dart" => SupportedLanguage::Dart,
            "sql" => SupportedLanguage::Sql,
            "scala" | "sc" => SupportedLanguage::Scala,
            "odin" => SupportedLanguage::Odin,
            "hs" | "lhs" => SupportedLanguage::Haskell,
            "ex" | "exs" => SupportedLanguage::Elixir,
            "erl" | "hrl" => SupportedLanguage::Erlang,
            "ml" | "mli" => SupportedLanguage::OCaml,
            "fs" | "fsi" | "fsx" => SupportedLanguage::FSharp,
            "elm" => SupportedLanguage::Elm,
            "jl" => SupportedLanguage::Julia,
            "nim" | "nims" => SupportedLanguage::Nim,
            "cr" => SupportedLanguage::Crystal,
            "clj" | "cljs" | "cljc" | "edn" => SupportedLanguage::Clojure,
            "nix" => SupportedLanguage::Nix,
            "gleam" => SupportedLanguage::Gleam,
            "tf" | "tfvars" => SupportedLanguage::Terraform,
            "vue" => SupportedLanguage::Vue,
            "svelte" => SupportedLanguage::Svelte,
            "astro" => SupportedLanguage::Astro,
            "pl" | "pm" => SupportedLanguage::Perl,
            "r" | "rmd" => SupportedLanguage::R,
            "rkt" => SupportedLanguage::Racket,
            "scm" | "ss" => SupportedLanguage::Scheme,
            "lisp" | "lsp" | "cl" => SupportedLanguage::CommonLisp,
            "purs" => SupportedLanguage::PureScript,
            "f90" | "f95" | "f03" | "f08" | "for" | "f" => SupportedLanguage::Fortran,
            "d" | "di" => SupportedLanguage::D,
            "v" => SupportedLanguage::V,
            "zsh" => SupportedLanguage::Zsh,
            "fish" => SupportedLanguage::Fish,
            "ps1" | "psm1" | "psd1" => SupportedLanguage::PowerShell,
            "groovy" | "gvy" => SupportedLanguage::Groovy,
            "gradle" => SupportedLanguage::Gradle,
            "m" => SupportedLanguage::ObjectiveC,
            "mm" => SupportedLanguage::ObjectiveCpp,
            "cu" | "cuh" => SupportedLanguage::Cuda,
            "glsl" | "vert" | "frag" | "geom" => SupportedLanguage::Glsl,
            "hlsl" => SupportedLanguage::Hlsl,
            "wgsl" => SupportedLanguage::Wgsl,
            "sol" => SupportedLanguage::Solidity,
            "move" => SupportedLanguage::Move,
            "cairo" => SupportedLanguage::Cairo,
            "hx" => SupportedLanguage::Haxe,
            "pas" | "pp" => SupportedLanguage::Pascal,
            "ada" | "adb" | "ads" => SupportedLanguage::Ada,
            "cob" | "cbl" => SupportedLanguage::Cobol,
            "prolog" | "pro" => SupportedLanguage::Prolog,
            "tcl" => SupportedLanguage::Tcl,
            "awk" => SupportedLanguage::Awk,
            "mk" | "mak" => SupportedLanguage::Makefile,
            "cmake" => SupportedLanguage::Cmake,
            "dockerfile" => SupportedLanguage::Dockerfile,
            "graphql" | "gql" => SupportedLanguage::GraphQL,
            "proto" => SupportedLanguage::Proto,
            "thrift" => SupportedLanguage::Thrift,
            "xml" | "xsd" | "xsl" | "plist" => SupportedLanguage::Xml,
            "ini" | "cfg" | "conf" => SupportedLanguage::Ini,
            "csv" | "tsv" => SupportedLanguage::Csv,
            "properties" => SupportedLanguage::Properties,
            "env" => SupportedLanguage::EnvFile,
            "hcl" => SupportedLanguage::Hcl,
            "jsonnet" | "libsonnet" => SupportedLanguage::Jsonnet,
            "dhall" => SupportedLanguage::Dhall,
            "diff" | "patch" => SupportedLanguage::Diff,
            "vim" => SupportedLanguage::Vim,
            "el" => SupportedLanguage::EmacsLisp,
            "tex" | "latex" | "sty" | "cls" => SupportedLanguage::Latex,
            "bib" => SupportedLanguage::Bibtex,
            "typ" => SupportedLanguage::Typst,
            "rst" => SupportedLanguage::Rst,
            "org" => SupportedLanguage::Org,
            "asm" | "s" => SupportedLanguage::Assembly,
            "v_verilog" | "sv" | "svh" => SupportedLanguage::Verilog,
            "vhd" | "vhdl" => SupportedLanguage::Vhdl,
            "janet" => SupportedLanguage::Janet,
            "wren" => SupportedLanguage::Wren,
            "vala" => SupportedLanguage::Vala,
            "gd" => SupportedLanguage::Gd,
            _ => SupportedLanguage::Plain,
        }
    }

    /// Resolves compiled, statically linked grammar for top-tier languages.
    pub fn static_language(self) -> Option<tree_sitter::Language> {
        match self {
            SupportedLanguage::Rust => Some(tree_sitter_rust::LANGUAGE.into()),
            SupportedLanguage::C => Some(tree_sitter_c::LANGUAGE.into()),
            SupportedLanguage::Cpp => Some(tree_sitter_cpp::LANGUAGE.into()),
            SupportedLanguage::Zig => Some(tree_sitter_zig::LANGUAGE.into()),
            SupportedLanguage::Python => Some(tree_sitter_python::LANGUAGE.into()),
            SupportedLanguage::JavaScript => Some(tree_sitter_javascript::LANGUAGE.into()),
            // Plain `.ts`/`.mts`/`.cts` use the TypeScript dialect grammar (no JSX syntax).
            SupportedLanguage::TypeScript => {
                Some(tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into())
            }
            // `.tsx` is a genuinely different grammar (TSX dialect) that additionally
            // understands JSX syntax; reusing LANGUAGE_TYPESCRIPT for `.tsx` files is
            // what previously caused parse/query errors on JSX-containing TypeScript.
            SupportedLanguage::Tsx => Some(tree_sitter_typescript::LANGUAGE_TSX.into()),
            SupportedLanguage::Go => Some(tree_sitter_go::LANGUAGE.into()),
            SupportedLanguage::Json => Some(tree_sitter_json::LANGUAGE.into()),
            SupportedLanguage::Toml => Some(tree_sitter_toml_ng::LANGUAGE.into()),
            SupportedLanguage::Yaml => Some(tree_sitter_yaml::LANGUAGE.into()),
            SupportedLanguage::Bash | SupportedLanguage::Zsh => {
                Some(tree_sitter_bash::LANGUAGE.into())
            }
            SupportedLanguage::Html => Some(tree_sitter_html::LANGUAGE.into()),
            SupportedLanguage::Css => Some(tree_sitter_css::LANGUAGE.into()),
            SupportedLanguage::Markdown => Some(tree_sitter_md::LANGUAGE.into()),
            SupportedLanguage::Java => Some(tree_sitter_java::LANGUAGE.into()),
            SupportedLanguage::CSharp => Some(tree_sitter_c_sharp::LANGUAGE.into()),
            SupportedLanguage::Ruby => Some(tree_sitter_ruby::LANGUAGE.into()),
            SupportedLanguage::Lua => Some(tree_sitter_lua::LANGUAGE.into()),
            _ => None,
        }
    }

    pub fn builtin_highlight_query(self) -> &'static str {
        match self {
            SupportedLanguage::Rust => {
                r#"
                (identifier) @variable
                (type_identifier) @type
                (primitive_type) @type
                (field_identifier) @property
                (call_expression function: (identifier) @function)
                (call_expression function: (field_expression field: (field_identifier) @function))
                (function_item name: (identifier) @function)
                (macro_invocation macro: (identifier) @macro)
                [
                  "fn" "let" "mut" "pub" "struct" "enum" "impl" "trait" "use" "mod" "crate"
                  "match" "if" "else" "while" "for" "in" "loop" "return" "break" "continue"
                  "as" "const" "static" "type" "unsafe" "async" "await" "where" "ref" "move"
                ] @keyword
                (line_comment) @comment
                (block_comment) @comment
                (string_literal) @string
                (raw_string_literal) @string
                (char_literal) @string
                (integer_literal) @number
                (float_literal) @number
                (boolean_literal) @number
                "#
            }
            SupportedLanguage::Python => {
                r#"
                (identifier) @variable
                (call function: (identifier) @function)
                (call function: (attribute attribute: (identifier) @function))
                (function_definition name: (identifier) @function)
                (class_definition name: (identifier) @type)
                [
                  "def" "class" "return" "if" "elif" "else" "for" "while" "break"
                  "continue" "import" "from" "as" "try" "except" "finally" "raise"
                  "with" "pass" "lambda" "yield" "global" "nonlocal" "assert" "async" "await"
                ] @keyword
                (comment) @comment
                (string) @string
                (integer) @number
                (float) @number
                (true) @number
                (false) @number
                (none) @keyword
                "#
            }
            SupportedLanguage::C => {
                r#"
                (identifier) @variable
                (type_identifier) @type
                (primitive_type) @type
                (field_identifier) @property
                (call_expression function: (identifier) @function)
                (call_expression function: (field_expression field: (field_identifier) @function))
                (function_declarator declarator: (identifier) @function)
                [
                  "if" "else" "switch" "case" "default" "while" "do" "for" "break"
                  "continue" "return" "goto" "struct" "union" "enum" "typedef"
                  "sizeof" "static" "extern" "auto" "register" "const" "volatile"
                ] @keyword
                (comment) @comment
                (string_literal) @string
                (char_literal) @string
                (number_literal) @number
                (preproc_include) @macro
                (preproc_def) @macro
                (preproc_directive) @macro
                "#
            }
            SupportedLanguage::Cpp => {
                r#"
                (identifier) @variable
                (type_identifier) @type
                (primitive_type) @type
                (field_identifier) @property
                (call_expression function: (identifier) @function)
                (call_expression function: (field_expression field: (field_identifier) @function))
                (function_declarator declarator: (identifier) @function)
                [
                  "if" "else" "switch" "case" "default" "while" "do" "for" "break"
                  "continue" "return" "goto" "struct" "union" "enum" "typedef"
                  "sizeof" "static" "extern" "auto" "register" "const" "volatile"
                  "class" "public" "private" "protected" "virtual" "template" "typename"
                  "namespace" "using" "new" "delete" "this" "try" "catch" "throw"
                ] @keyword
                (comment) @comment
                (string_literal) @string
                (char_literal) @string
                (number_literal) @number
                (preproc_include) @macro
                (preproc_def) @macro
                (preproc_directive) @macro
                "#
            }
            SupportedLanguage::Zig => {
                r#"
                (identifier) @variable
                (type_identifier) @type
                (field_identifier) @property
                (call_expression function: (identifier) @function)
                (call_expression function: (field_expression field: (field_identifier) @function))
                [
                  "const" "var" "fn" "pub" "return" "if" "else" "switch" "while" "for"
                  "break" "continue" "defer" "errdefer" "try" "catch" "unreachable"
                  "test" "usingnamespace" "opaque" "enum" "struct" "union" "error"
                  "and" "or" "orelse"
                ] @keyword
                (line_comment) @comment
                (string_literal) @string
                (char_literal) @string
                (integer_literal) @number
                (float_literal) @number
                "#
            }
            SupportedLanguage::Go => {
                r#"
                (identifier) @variable
                (type_identifier) @type
                (field_identifier) @property
                (package_identifier) @namespace
                (call_expression function: (identifier) @function)
                (call_expression function: (selector_expression field: (field_identifier) @function))
                (function_declaration name: (identifier) @function)
                (method_declaration name: (field_identifier) @function)
                [
                  "func" "return" "var" "const" "type" "struct" "interface" "package"
                  "import" "for" "range" "if" "else" "switch" "case" "default" "select"
                  "go" "defer" "chan" "map" "break" "continue" "fallthrough"
                ] @keyword
                (comment) @comment
                (raw_string_literal) @string
                (interpreted_string_literal) @string
                (int_literal) @number
                (float_literal) @number
                "#
            }
            SupportedLanguage::JavaScript => {
                r#"
                (identifier) @variable
                (property_identifier) @property
                (shorthand_property_identifier) @property
                (call_expression function: (identifier) @function)
                (call_expression function: (member_expression property: (property_identifier) @function))
                (function_declaration name: (identifier) @function)
                (function_expression name: (identifier) @function)
                (method_definition name: (property_identifier) @function)
                (arrow_function) @function
                (formal_parameters (identifier) @parameter)
                [
                  "function" "const" "let" "var" "return" "if" "else" "switch" "case"
                  "default" "for" "while" "do" "break" "continue" "try" "catch" "finally"
                  "throw" "class" "extends" "import" "export" "from" "new" "static" "get" "set"
                  "this" "super" "async" "await" "yield" "typeof" "instanceof" "void" "delete" "in" "of"
                ] @keyword
                (comment) @comment
                (string) @string
                (template_string) @string
                (regex) @string
                (number) @number
                [ (true) (false) ] @number
                (null) @keyword
                (undefined) @keyword
                ["=" "==" "===" "!=" "!==" "<" ">" "<=" ">=" "+" "-" "*" "/" "%" "&&" "||" "??" "=>" "..."] @operator
                (jsx_opening_element (identifier) @tag)
                (jsx_opening_element (member_expression) @tag)
                (jsx_closing_element (identifier) @tag)
                (jsx_closing_element (member_expression) @tag)
                (jsx_self_closing_element (identifier) @tag)
                (jsx_self_closing_element (member_expression) @tag)
                (jsx_attribute (property_identifier) @property)
                (jsx_text) @string
                "#
            }
            SupportedLanguage::TypeScript => {
                r#"
                (identifier) @variable
                (type_identifier) @type
                (predefined_type) @type
                (property_identifier) @property
                (shorthand_property_identifier) @property
                (call_expression function: (identifier) @function)
                (call_expression function: (member_expression property: (property_identifier) @function))
                (function_declaration name: (identifier) @function)
                (function_expression name: (identifier) @function)
                (method_definition name: (property_identifier) @function)
                (method_signature name: (property_identifier) @function)
                (arrow_function) @function
                (formal_parameters (required_parameter pattern: (identifier) @parameter))
                (formal_parameters (identifier) @parameter)
                [
                  "function" "const" "let" "var" "return" "if" "else" "switch" "case"
                  "default" "for" "while" "do" "break" "continue" "try" "catch" "finally"
                  "throw" "class" "extends" "import" "export" "from" "new" "static" "get" "set"
                  "this" "super" "async" "await" "yield" "typeof" "instanceof" "void" "delete" "in" "of"
                  "type" "interface" "enum" "namespace" "declare" "abstract" "implements"
                  "readonly" "as" "keyof" "is" "satisfies" "infer" "asserts" "override"
                ] @keyword
                (comment) @comment
                (string) @string
                (template_string) @string
                (regex) @string
                (number) @number
                [ (true) (false) ] @number
                (null) @keyword
                (undefined) @keyword
                ["=" "==" "===" "!=" "!==" "<" ">" "<=" ">=" "+" "-" "*" "/" "%" "&&" "||" "??" "=>" "..." ":"] @operator
                "#
            }
            // `.tsx` files parse with the dedicated TSX dialect grammar, which is a
            // superset of TypeScript that additionally understands JSX element syntax.
            SupportedLanguage::Tsx => {
                r#"
                (identifier) @variable
                (type_identifier) @type
                (predefined_type) @type
                (property_identifier) @property
                (shorthand_property_identifier) @property
                (call_expression function: (identifier) @function)
                (call_expression function: (member_expression property: (property_identifier) @function))
                (function_declaration name: (identifier) @function)
                (function_expression name: (identifier) @function)
                (method_definition name: (property_identifier) @function)
                (method_signature name: (property_identifier) @function)
                (arrow_function) @function
                (formal_parameters (required_parameter pattern: (identifier) @parameter))
                (formal_parameters (identifier) @parameter)
                [
                  "function" "const" "let" "var" "return" "if" "else" "switch" "case"
                  "default" "for" "while" "do" "break" "continue" "try" "catch" "finally"
                  "throw" "class" "extends" "import" "export" "from" "new" "static" "get" "set"
                  "this" "super" "async" "await" "yield" "typeof" "instanceof" "void" "delete" "in" "of"
                  "type" "interface" "enum" "namespace" "declare" "abstract" "implements"
                  "readonly" "as" "keyof" "is" "satisfies" "infer" "asserts" "override"
                ] @keyword
                (comment) @comment
                (string) @string
                (template_string) @string
                (regex) @string
                (number) @number
                [ (true) (false) ] @number
                (null) @keyword
                (undefined) @keyword
                ["=" "==" "===" "!=" "!==" "<" ">" "<=" ">=" "+" "-" "*" "/" "%" "&&" "||" "??" "=>" "..." ":"] @operator
                (jsx_opening_element (identifier) @tag)
                (jsx_opening_element (member_expression) @tag)
                (jsx_closing_element (identifier) @tag)
                (jsx_closing_element (member_expression) @tag)
                (jsx_self_closing_element (identifier) @tag)
                (jsx_self_closing_element (member_expression) @tag)
                (jsx_attribute (property_identifier) @property)
                (jsx_text) @string
                "#
            }
            SupportedLanguage::Bash | SupportedLanguage::Zsh => {
                r#"
                (variable_name) @variable
                (command_name) @function
                [
                  "if" "then" "else" "elif" "fi" "case" "esac" "for" "while" "until"
                  "do" "done" "in" "function" "select" "time"
                ] @keyword
                (comment) @comment
                (string) @string
                (raw_string) @string
                "#
            }
            SupportedLanguage::Json => {
                r#"
                (pair key: (_) @property)
                (string) @string
                (number) @number
                [ (true) (false) ] @number
                (null) @keyword
                (comment) @comment
                "#
            }
            SupportedLanguage::Toml => {
                r#"
                (table (bare_key) @type)
                (pair [ (bare_key) (quoted_key) ] @property)
                (string) @string
                (integer) @number
                (float) @number
                (boolean) @number
                (comment) @comment
                "#
            }
            SupportedLanguage::Yaml => {
                r#"
                (block_mapping_pair key: (_) @property)
                (flow_pair key: (_) @property)
                (string_scalar) @string
                (integer_scalar) @number
                (float_scalar) @number
                (boolean_scalar) @number
                (null_scalar) @keyword
                (comment) @comment
                "#
            }
            SupportedLanguage::Html => {
                r#"
                (tag_name) @tag
                (attribute_name) @property
                (attribute_value) @string
                (comment) @comment
                "#
            }
            SupportedLanguage::Css => {
                r#"
                (tag_name) @tag
                (class_name) @type
                (id_name) @type
                (property_name) @property
                (color_value) @number
                (integer_value) @number
                (float_value) @number
                (string_value) @string
                (comment) @comment
                "#
            }
            SupportedLanguage::Markdown => {
                r#"
                (atx_heading) @type
                (setext_heading) @type
                (fenced_code_block) @string
                (indented_code_block) @string
                (block_quote) @comment
                (thematic_break) @keyword
                "#
            }
            SupportedLanguage::Java => {
                r#"
                (identifier) @variable
                (type_identifier) @type
                (field_access field: (identifier) @property)
                (method_invocation name: (identifier) @function)
                (method_declaration name: (identifier) @function)
                [
                  "public" "private" "protected" "class" "interface" "enum" "extends"
                  "implements" "return" "if" "else" "for" "while" "do" "break" "continue"
                  "switch" "case" "default" "new" "this" "super" "try" "catch" "finally"
                  "throw" "throws" "static" "final" "void" "package" "import"
                ] @keyword
                (line_comment) @comment
                (block_comment) @comment
                (string_literal) @string
                (decimal_integer_literal) @number
                (hex_integer_literal) @number
                (octal_integer_literal) @number
                (binary_integer_literal) @number
                (decimal_floating_point_literal) @number
                (hex_floating_point_literal) @number
                (true) @number
                (false) @number
                (null_literal) @keyword
                "#
            }
            SupportedLanguage::CSharp => {
                r#"
                (identifier) @variable
                (predefined_type) @type
                (invocation_expression function: (identifier) @function)
                (invocation_expression function: (member_access_expression name: (identifier) @function))
                (method_declaration name: (identifier) @function)
                (local_function_statement name: (identifier) @function)
                (class_declaration name: (identifier) @type)
                (interface_declaration name: (identifier) @type)
                (struct_declaration name: (identifier) @type)
                (enum_declaration name: (identifier) @type)
                (namespace_declaration name: (identifier) @namespace)
                [
                  "class" "interface" "struct" "enum" "namespace" "using" "public" "private"
                  "protected" "internal" "static" "readonly" "const" "return" "if" "else"
                  "switch" "case" "default" "for" "foreach" "while" "do" "break" "continue"
                  "try" "catch" "finally" "throw" "new" "this" "base" "async" "await" "var"
                  "void" "override" "virtual" "abstract" "sealed" "partial" "in" "out" "ref"
                  "get" "set" "yield" "is" "as"
                ] @keyword
                (comment) @comment
                (string_literal) @string
                (verbatim_string_literal) @string
                (raw_string_literal) @string
                (character_literal) @string
                (integer_literal) @number
                (real_literal) @number
                (boolean_literal) @number
                (null_literal) @keyword
                "#
            }
            SupportedLanguage::Ruby => {
                r#"
                (identifier) @variable
                (constant) @type
                (call method: [(identifier) (constant)] @function)
                (method name: (identifier) @function)
                (method_parameters (identifier) @parameter)
                (block_parameters (identifier) @parameter)
                (instance_variable) @property
                (class_variable) @property
                [
                  "alias" "and" "begin" "break" "case" "class" "def" "do" "else" "elsif"
                  "end" "ensure" "for" "if" "in" "module" "next" "or" "rescue" "retry"
                  "return" "then" "unless" "until" "when" "while" "yield" "not" "self" "super"
                ] @keyword
                (comment) @comment
                [ (string) (bare_string) (heredoc_body) (heredoc_beginning) (subshell) ] @string
                [ (simple_symbol) (delimited_symbol) (hash_key_symbol) (bare_symbol) ] @string
                (regex) @string
                [ (integer) (float) ] @number
                [ (nil) (true) (false) ] @keyword
                "#
            }
            SupportedLanguage::Lua => {
                r#"
                (identifier) @variable
                (function_call name: (identifier) @function)
                (function_call name: (dot_index_expression field: (identifier) @function))
                (function_call name: (method_index_expression method: (identifier) @function))
                (function_declaration name: (identifier) @function)
                (function_declaration name: (dot_index_expression field: (identifier) @function))
                (function_declaration name: (method_index_expression method: (identifier) @function))
                (parameters (identifier) @parameter)
                [
                  "function" "local" "end" "if" "then" "else" "elseif" "for" "while"
                  "repeat" "until" "do" "break" "return" "in" "and" "or" "not" "goto"
                ] @keyword
                (comment) @comment
                (string) @string
                (number) @number
                [ (true) (false) ] @number
                (nil) @keyword
                "#
            }
            _ => "",
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            SupportedLanguage::Rust => "Rust",
            SupportedLanguage::Go => "Go",
            SupportedLanguage::Python => "Python",
            SupportedLanguage::C => "C",
            SupportedLanguage::Cpp => "C++",
            SupportedLanguage::Zig => "Zig",
            SupportedLanguage::JavaScript => "JavaScript",
            SupportedLanguage::TypeScript => "TypeScript",
            SupportedLanguage::Tsx => "TSX",
            SupportedLanguage::Html => "HTML",
            SupportedLanguage::Css => "CSS",
            SupportedLanguage::Json => "JSON",
            SupportedLanguage::Toml => "TOML",
            SupportedLanguage::Yaml => "YAML",
            SupportedLanguage::Bash => "Bash",
            SupportedLanguage::Lua => "Lua",
            SupportedLanguage::Markdown => "Markdown",
            SupportedLanguage::Java => "Java",
            SupportedLanguage::CSharp => "C#",
            SupportedLanguage::Php => "PHP",
            SupportedLanguage::Ruby => "Ruby",
            SupportedLanguage::Kotlin => "Kotlin",
            SupportedLanguage::Swift => "Swift",
            SupportedLanguage::Dart => "Dart",
            SupportedLanguage::Sql => "SQL",
            SupportedLanguage::Scala => "Scala",
            SupportedLanguage::Odin => "Odin",
            SupportedLanguage::Haskell => "Haskell",
            SupportedLanguage::Elixir => "Elixir",
            SupportedLanguage::Erlang => "Erlang",
            SupportedLanguage::OCaml => "OCaml",
            SupportedLanguage::FSharp => "F#",
            SupportedLanguage::Elm => "Elm",
            SupportedLanguage::Julia => "Julia",
            SupportedLanguage::Nim => "Nim",
            SupportedLanguage::Crystal => "Crystal",
            SupportedLanguage::Clojure => "Clojure",
            SupportedLanguage::Nix => "Nix",
            SupportedLanguage::Gleam => "Gleam",
            SupportedLanguage::Terraform => "Terraform",
            SupportedLanguage::Vue => "Vue",
            SupportedLanguage::Svelte => "Svelte",
            SupportedLanguage::Astro => "Astro",
            SupportedLanguage::Perl => "Perl",
            SupportedLanguage::R => "R",
            SupportedLanguage::Racket => "Racket",
            SupportedLanguage::Scheme => "Scheme",
            SupportedLanguage::CommonLisp => "Common Lisp",
            SupportedLanguage::PureScript => "PureScript",
            SupportedLanguage::Fortran => "Fortran",
            SupportedLanguage::D => "D",
            SupportedLanguage::V => "V",
            SupportedLanguage::Zsh => "Zsh",
            SupportedLanguage::Fish => "Fish",
            SupportedLanguage::PowerShell => "PowerShell",
            SupportedLanguage::Groovy => "Groovy",
            SupportedLanguage::Gradle => "Gradle",
            SupportedLanguage::ObjectiveC => "Objective-C",
            SupportedLanguage::ObjectiveCpp => "Objective-C++",
            SupportedLanguage::Cuda => "CUDA",
            SupportedLanguage::Glsl => "GLSL",
            SupportedLanguage::Hlsl => "HLSL",
            SupportedLanguage::Wgsl => "WGSL",
            SupportedLanguage::Solidity => "Solidity",
            SupportedLanguage::Move => "Move",
            SupportedLanguage::Cairo => "Cairo",
            SupportedLanguage::Haxe => "Haxe",
            SupportedLanguage::Pascal => "Pascal",
            SupportedLanguage::Ada => "Ada",
            SupportedLanguage::Cobol => "COBOL",
            SupportedLanguage::Prolog => "Prolog",
            SupportedLanguage::Tcl => "Tcl",
            SupportedLanguage::Awk => "AWK",
            SupportedLanguage::Makefile => "Makefile",
            SupportedLanguage::Cmake => "CMake",
            SupportedLanguage::Dockerfile => "Dockerfile",
            SupportedLanguage::GraphQL => "GraphQL",
            SupportedLanguage::Proto => "Protocol Buffers",
            SupportedLanguage::Thrift => "Thrift",
            SupportedLanguage::Xml => "XML",
            SupportedLanguage::Ini => "INI",
            SupportedLanguage::Csv => "CSV",
            SupportedLanguage::Properties => "Properties",
            SupportedLanguage::EnvFile => "Dotenv",
            SupportedLanguage::Hcl => "HCL",
            SupportedLanguage::Jsonnet => "Jsonnet",
            SupportedLanguage::Dhall => "Dhall",
            SupportedLanguage::Nginx => "Nginx",
            SupportedLanguage::Diff => "Diff",
            SupportedLanguage::Regex => "Regex",
            SupportedLanguage::Vim => "Vimscript",
            SupportedLanguage::EmacsLisp => "Emacs Lisp",
            SupportedLanguage::Latex => "LaTeX",
            SupportedLanguage::Bibtex => "BibTeX",
            SupportedLanguage::Typst => "Typst",
            SupportedLanguage::Rst => "reStructuredText",
            SupportedLanguage::Org => "Org Mode",
            SupportedLanguage::Assembly => "Assembly",
            SupportedLanguage::Verilog => "Verilog",
            SupportedLanguage::Vhdl => "VHDL",
            SupportedLanguage::Zephyr => "Zephyr",
            SupportedLanguage::Janet => "Janet",
            SupportedLanguage::Wren => "Wren",
            SupportedLanguage::Vala => "Vala",
            SupportedLanguage::Gd => "GDScript",
            SupportedLanguage::Plain => "Plain Text",
        }
    }

    pub fn grammar_name(self) -> &'static str {
        match self {
            SupportedLanguage::Rust => "rust",
            SupportedLanguage::Go => "go",
            SupportedLanguage::Python => "python",
            SupportedLanguage::C => "c",
            SupportedLanguage::Cpp => "cpp",
            SupportedLanguage::Zig => "zig",
            SupportedLanguage::JavaScript => "javascript",
            SupportedLanguage::TypeScript => "typescript",
            SupportedLanguage::Tsx => "tsx",
            SupportedLanguage::Html => "html",
            SupportedLanguage::Css => "css",
            SupportedLanguage::Json => "json",
            SupportedLanguage::Toml => "toml",
            SupportedLanguage::Yaml => "yaml",
            SupportedLanguage::Bash | SupportedLanguage::Zsh => "bash",
            SupportedLanguage::Lua => "lua",
            SupportedLanguage::Markdown => "markdown",
            SupportedLanguage::Java => "java",
            SupportedLanguage::CSharp => "c_sharp",
            SupportedLanguage::Php => "php",
            SupportedLanguage::Ruby => "ruby",
            SupportedLanguage::Kotlin => "kotlin",
            SupportedLanguage::Swift => "swift",
            SupportedLanguage::Dart => "dart",
            SupportedLanguage::Sql => "sql",
            SupportedLanguage::Scala => "scala",
            SupportedLanguage::Odin => "odin",
            SupportedLanguage::Haskell => "haskell",
            SupportedLanguage::Elixir => "elixir",
            SupportedLanguage::Erlang => "erlang",
            SupportedLanguage::OCaml => "ocaml",
            SupportedLanguage::FSharp => "fsharp",
            SupportedLanguage::Elm => "elm",
            SupportedLanguage::Julia => "julia",
            SupportedLanguage::Nim => "nim",
            SupportedLanguage::Crystal => "crystal",
            SupportedLanguage::Clojure => "clojure",
            SupportedLanguage::Nix => "nix",
            SupportedLanguage::Gleam => "gleam",
            SupportedLanguage::Terraform | SupportedLanguage::Hcl => "hcl",
            SupportedLanguage::Vue => "vue",
            SupportedLanguage::Svelte => "svelte",
            SupportedLanguage::Astro => "astro",
            SupportedLanguage::Perl => "perl",
            SupportedLanguage::R => "r",
            SupportedLanguage::Racket => "racket",
            SupportedLanguage::Scheme => "scheme",
            SupportedLanguage::CommonLisp => "commonlisp",
            SupportedLanguage::PureScript => "purescript",
            SupportedLanguage::Fortran => "fortran",
            SupportedLanguage::D => "d",
            SupportedLanguage::V => "v",
            SupportedLanguage::Fish => "fish",
            SupportedLanguage::PowerShell => "powershell",
            SupportedLanguage::Groovy | SupportedLanguage::Gradle => "groovy",
            SupportedLanguage::ObjectiveC | SupportedLanguage::ObjectiveCpp => "objc",
            SupportedLanguage::Cuda => "cuda",
            SupportedLanguage::Glsl => "glsl",
            SupportedLanguage::Hlsl => "hlsl",
            SupportedLanguage::Wgsl => "wgsl",
            SupportedLanguage::Solidity => "solidity",
            SupportedLanguage::Move => "move",
            SupportedLanguage::Cairo => "cairo",
            SupportedLanguage::Haxe => "haxe",
            SupportedLanguage::Pascal => "pascal",
            SupportedLanguage::Ada => "ada",
            SupportedLanguage::Cobol => "cobol",
            SupportedLanguage::Prolog => "prolog",
            SupportedLanguage::Tcl => "tcl",
            SupportedLanguage::Awk => "awk",
            SupportedLanguage::Makefile => "make",
            SupportedLanguage::Cmake => "cmake",
            SupportedLanguage::Dockerfile => "dockerfile",
            SupportedLanguage::GraphQL => "graphql",
            SupportedLanguage::Proto => "proto",
            SupportedLanguage::Thrift => "thrift",
            SupportedLanguage::Xml => "xml",
            SupportedLanguage::Ini => "ini",
            SupportedLanguage::Csv => "csv",
            SupportedLanguage::Properties => "properties",
            SupportedLanguage::EnvFile => "dotenv",
            SupportedLanguage::Jsonnet => "jsonnet",
            SupportedLanguage::Dhall => "dhall",
            SupportedLanguage::Nginx => "nginx",
            SupportedLanguage::Diff => "diff",
            SupportedLanguage::Regex => "regex",
            SupportedLanguage::Vim => "vim",
            SupportedLanguage::EmacsLisp => "elisp",
            SupportedLanguage::Latex => "latex",
            SupportedLanguage::Bibtex => "bibtex",
            SupportedLanguage::Typst => "typst",
            SupportedLanguage::Rst => "rst",
            SupportedLanguage::Org => "org",
            SupportedLanguage::Assembly => "asm",
            SupportedLanguage::Verilog => "verilog",
            SupportedLanguage::Vhdl => "vhdl",
            SupportedLanguage::Zephyr => "devicetree",
            SupportedLanguage::Janet => "janet_simple",
            SupportedLanguage::Wren => "wren",
            SupportedLanguage::Vala => "vala",
            SupportedLanguage::Gd => "gdscript",
            SupportedLanguage::Plain => "",
        }
    }

    pub fn lsp_id(self) -> &'static str {
        match self {
            SupportedLanguage::Rust => "rust",
            SupportedLanguage::Go => "go",
            SupportedLanguage::Python => "python",
            SupportedLanguage::C => "c",
            SupportedLanguage::Cpp => "cpp",
            SupportedLanguage::Zig => "zig",
            SupportedLanguage::JavaScript => "javascript",
            SupportedLanguage::TypeScript => "typescript",
            // typescript-language-server / vtsls require the exact "typescriptreact"
            // languageId for .tsx — sending "typescript" causes JSX-aware features
            // (and in some server versions, the whole didOpen) to misbehave.
            SupportedLanguage::Tsx => "typescriptreact",
            SupportedLanguage::Html => "html",
            SupportedLanguage::Css => "css",
            SupportedLanguage::Json => "json",
            SupportedLanguage::Toml => "toml",
            SupportedLanguage::Yaml => "yaml",
            SupportedLanguage::Bash | SupportedLanguage::Zsh => "shellscript",
            SupportedLanguage::Lua => "lua",
            SupportedLanguage::Markdown => "markdown",
            SupportedLanguage::Java => "java",
            SupportedLanguage::CSharp => "csharp",
            SupportedLanguage::Php => "php",
            SupportedLanguage::Ruby => "ruby",
            SupportedLanguage::Kotlin => "kotlin",
            SupportedLanguage::Swift => "swift",
            SupportedLanguage::Dart => "dart",
            SupportedLanguage::Sql => "sql",
            SupportedLanguage::Scala => "scala",
            SupportedLanguage::Odin => "odin",
            SupportedLanguage::Haskell => "haskell",
            SupportedLanguage::Elixir => "elixir",
            SupportedLanguage::Erlang => "erlang",
            SupportedLanguage::OCaml => "ocaml",
            SupportedLanguage::FSharp => "fsharp",
            SupportedLanguage::Elm => "elm",
            SupportedLanguage::Julia => "julia",
            SupportedLanguage::Nim => "nim",
            SupportedLanguage::Crystal => "crystal",
            SupportedLanguage::Clojure => "clojure",
            SupportedLanguage::Nix => "nix",
            SupportedLanguage::Gleam => "gleam",
            SupportedLanguage::Terraform | SupportedLanguage::Hcl => "terraform",
            SupportedLanguage::Vue => "vue",
            SupportedLanguage::Svelte => "svelte",
            SupportedLanguage::Astro => "astro",
            SupportedLanguage::Perl => "perl",
            SupportedLanguage::R => "r",
            SupportedLanguage::Racket => "racket",
            SupportedLanguage::Scheme => "scheme",
            SupportedLanguage::CommonLisp => "lisp",
            SupportedLanguage::PureScript => "purescript",
            SupportedLanguage::Fortran => "fortran",
            SupportedLanguage::D => "d",
            SupportedLanguage::V => "vlang",
            SupportedLanguage::Fish => "fish",
            SupportedLanguage::PowerShell => "powershell",
            SupportedLanguage::Groovy | SupportedLanguage::Gradle => "groovy",
            SupportedLanguage::ObjectiveC => "objective-c",
            SupportedLanguage::ObjectiveCpp => "objective-cpp",
            SupportedLanguage::Cuda => "cuda",
            SupportedLanguage::Glsl => "glsl",
            SupportedLanguage::Hlsl => "hlsl",
            SupportedLanguage::Wgsl => "wgsl",
            SupportedLanguage::Solidity => "solidity",
            SupportedLanguage::Move => "move",
            SupportedLanguage::Cairo => "cairo",
            SupportedLanguage::Haxe => "haxe",
            SupportedLanguage::Pascal => "pascal",
            SupportedLanguage::Ada => "ada",
            SupportedLanguage::Cobol => "cobol",
            SupportedLanguage::Prolog => "prolog",
            SupportedLanguage::Tcl => "tcl",
            SupportedLanguage::Awk => "awk",
            SupportedLanguage::Makefile => "makefile",
            SupportedLanguage::Cmake => "cmake",
            SupportedLanguage::Dockerfile => "dockerfile",
            SupportedLanguage::GraphQL => "graphql",
            SupportedLanguage::Proto => "proto",
            SupportedLanguage::Thrift => "thrift",
            SupportedLanguage::Xml => "xml",
            SupportedLanguage::Ini => "ini",
            SupportedLanguage::Csv => "csv",
            SupportedLanguage::Properties => "properties",
            SupportedLanguage::EnvFile => "dotenv",
            SupportedLanguage::Jsonnet => "jsonnet",
            SupportedLanguage::Dhall => "dhall",
            SupportedLanguage::Nginx => "nginx",
            SupportedLanguage::Diff => "diff",
            SupportedLanguage::Regex => "regex",
            SupportedLanguage::Vim => "vim",
            SupportedLanguage::EmacsLisp => "emacs-lisp",
            SupportedLanguage::Latex => "latex",
            SupportedLanguage::Bibtex => "bibtex",
            SupportedLanguage::Typst => "typst",
            SupportedLanguage::Rst => "restructuredtext",
            SupportedLanguage::Org => "org",
            SupportedLanguage::Assembly => "asm",
            SupportedLanguage::Verilog => "verilog",
            SupportedLanguage::Vhdl => "vhdl",
            SupportedLanguage::Zephyr => "dts",
            SupportedLanguage::Janet => "janet",
            SupportedLanguage::Wren => "wren",
            SupportedLanguage::Vala => "vala",
            SupportedLanguage::Gd => "gdscript",
            SupportedLanguage::Plain => "plaintext",
        }
    }

    pub fn candidate_servers(self) -> &'static [&'static str] {
        match self {
            SupportedLanguage::Rust => &["rust-analyzer"],
            SupportedLanguage::Go => &["gopls"],
            SupportedLanguage::Python => &[
                "pyright-langserver",
                "pyright",
                "pylsp",
                "jedi-language-server",
            ],
            SupportedLanguage::C | SupportedLanguage::Cpp => &["clangd", "ccls"],
            SupportedLanguage::Zig => &["zls"],
            SupportedLanguage::JavaScript
            | SupportedLanguage::TypeScript
            | SupportedLanguage::Tsx => &[
                "typescript-language-server",
                "vtsls",
                "quick-lint-js",
                "biome",
            ],
            SupportedLanguage::Html => &["vscode-html-language-server", "html-languageserver"],
            SupportedLanguage::Css => &["vscode-css-language-server", "css-languageserver"],
            SupportedLanguage::Json => &["vscode-json-language-server"],
            SupportedLanguage::Toml => &["taplo"],
            SupportedLanguage::Yaml => &["yaml-language-server"],
            SupportedLanguage::Bash | SupportedLanguage::Zsh => &["bash-language-server"],
            SupportedLanguage::Lua => &["lua-language-server"],
            SupportedLanguage::Markdown => &["marksman"],
            SupportedLanguage::Java => &["jdtls"],
            SupportedLanguage::CSharp => &["omnisharp", "csharp-ls"],
            SupportedLanguage::Php => &["intelephense", "phpactor"],
            SupportedLanguage::Ruby => &["solargraph", "ruby-lsp"],
            SupportedLanguage::Kotlin => &["kotlin-language-server"],
            SupportedLanguage::Swift => &["sourcekit-lsp"],
            SupportedLanguage::Dart => &["dart"],
            SupportedLanguage::Sql => &["sqls", "sql-language-server"],
            SupportedLanguage::Scala => &["metals"],
            SupportedLanguage::Odin => &["ols"],
            SupportedLanguage::Haskell => {
                &["haskell-language-server-wrapper", "haskell-language-server"]
            }
            SupportedLanguage::Elixir => &["elixir-ls", "lexical"],
            SupportedLanguage::Erlang => &["erlang_ls"],
            SupportedLanguage::OCaml => &["ocamllsp"],
            SupportedLanguage::FSharp => &["fsautocomplete"],
            SupportedLanguage::Elm => &["elm-language-server"],
            SupportedLanguage::Julia => &["julia"],
            SupportedLanguage::Nim => &["nimlsp", "nimlangserver"],
            SupportedLanguage::Crystal => &["crystalline"],
            SupportedLanguage::Clojure => &["clojure-lsp"],
            SupportedLanguage::Nix => &["nil", "nixd"],
            SupportedLanguage::Gleam => &["gleam"],
            SupportedLanguage::Terraform | SupportedLanguage::Hcl => &["terraform-ls"],
            SupportedLanguage::Vue => &["vue-language-server"],
            SupportedLanguage::Svelte => &["svelteserver", "svelte-language-server"],
            SupportedLanguage::Astro => &["astro-ls"],
            SupportedLanguage::Perl => &["perlnavigator", "pls"],
            SupportedLanguage::R => &["r-languageserver"],
            SupportedLanguage::Racket => &["racket-langserver"],
            SupportedLanguage::PureScript => &["purescript-language-server"],
            SupportedLanguage::Fortran => &["fortls"],
            SupportedLanguage::D => &["serve-d"],
            SupportedLanguage::V => &["v-analyzer"],
            SupportedLanguage::PowerShell => &["powershell-editor-services"],
            SupportedLanguage::Groovy | SupportedLanguage::Gradle => &["groovy-language-server"],
            SupportedLanguage::ObjectiveC | SupportedLanguage::ObjectiveCpp => &["clangd"],
            SupportedLanguage::Solidity => &["nomicfoundation-solidity-language-server"],
            SupportedLanguage::Haxe => &["haxe-language-server"],
            SupportedLanguage::Pascal => &["pasls"],
            SupportedLanguage::Ada => &["ada_language_server"],
            SupportedLanguage::Cmake => &["neocmakelsp", "cmake-language-server"],
            SupportedLanguage::Dockerfile => &["docker-langserver"],
            SupportedLanguage::GraphQL => &["graphql-lsp"],
            SupportedLanguage::Proto => &["buf-language-server", "pls-proto"],
            SupportedLanguage::Xml => &["lemminx"],
            SupportedLanguage::Vim => &["vim-language-server"],
            SupportedLanguage::EmacsLisp => &["elisp-language-server"],
            SupportedLanguage::Latex | SupportedLanguage::Bibtex => &["texlab", "digestif"],
            SupportedLanguage::Typst => &["tinymist", "typst-lsp"],
            SupportedLanguage::Verilog | SupportedLanguage::Vhdl => &["svlangserver", "vhdl_ls"],
            SupportedLanguage::Zephyr => &["dts-lsp"],
            SupportedLanguage::Vala => &["vala-language-server"],
            _ => &[],
        }
    }

    pub fn installed_servers(self) -> Vec<String> {
        self.candidate_servers()
            .iter()
            .filter(|cmd| resolve_binary_path(cmd).is_some())
            .map(|s| (*s).to_string())
            .collect()
    }

    pub fn all() -> &'static [SupportedLanguage] {
        &[
            SupportedLanguage::Rust,
            SupportedLanguage::Go,
            SupportedLanguage::Python,
            SupportedLanguage::C,
            SupportedLanguage::Cpp,
            SupportedLanguage::Zig,
            SupportedLanguage::JavaScript,
            SupportedLanguage::TypeScript,
            SupportedLanguage::Tsx,
            SupportedLanguage::Html,
            SupportedLanguage::Css,
            SupportedLanguage::Json,
            SupportedLanguage::Toml,
            SupportedLanguage::Yaml,
            SupportedLanguage::Bash,
            SupportedLanguage::Lua,
            SupportedLanguage::Markdown,
            SupportedLanguage::Java,
            SupportedLanguage::CSharp,
            SupportedLanguage::Php,
            SupportedLanguage::Ruby,
            SupportedLanguage::Kotlin,
            SupportedLanguage::Swift,
            SupportedLanguage::Dart,
            SupportedLanguage::Sql,
            SupportedLanguage::Scala,
            SupportedLanguage::Odin,
            SupportedLanguage::Haskell,
            SupportedLanguage::Elixir,
            SupportedLanguage::Erlang,
            SupportedLanguage::OCaml,
            SupportedLanguage::FSharp,
            SupportedLanguage::Elm,
            SupportedLanguage::Julia,
            SupportedLanguage::Nim,
            SupportedLanguage::Crystal,
            SupportedLanguage::Clojure,
            SupportedLanguage::Nix,
            SupportedLanguage::Gleam,
            SupportedLanguage::Terraform,
            SupportedLanguage::Vue,
            SupportedLanguage::Svelte,
            SupportedLanguage::Astro,
            SupportedLanguage::Perl,
            SupportedLanguage::R,
            SupportedLanguage::Racket,
            SupportedLanguage::Scheme,
            SupportedLanguage::CommonLisp,
            SupportedLanguage::PureScript,
            SupportedLanguage::Fortran,
            SupportedLanguage::D,
            SupportedLanguage::V,
            SupportedLanguage::Zsh,
            SupportedLanguage::Fish,
            SupportedLanguage::PowerShell,
            SupportedLanguage::Groovy,
            SupportedLanguage::Gradle,
            SupportedLanguage::ObjectiveC,
            SupportedLanguage::ObjectiveCpp,
            SupportedLanguage::Cuda,
            SupportedLanguage::Glsl,
            SupportedLanguage::Hlsl,
            SupportedLanguage::Wgsl,
            SupportedLanguage::Solidity,
            SupportedLanguage::Move,
            SupportedLanguage::Cairo,
            SupportedLanguage::Haxe,
            SupportedLanguage::Pascal,
            SupportedLanguage::Ada,
            SupportedLanguage::Cobol,
            SupportedLanguage::Prolog,
            SupportedLanguage::Tcl,
            SupportedLanguage::Awk,
            SupportedLanguage::Makefile,
            SupportedLanguage::Cmake,
            SupportedLanguage::Dockerfile,
            SupportedLanguage::GraphQL,
            SupportedLanguage::Proto,
            SupportedLanguage::Thrift,
            SupportedLanguage::Xml,
            SupportedLanguage::Ini,
            SupportedLanguage::Csv,
            SupportedLanguage::Properties,
            SupportedLanguage::EnvFile,
            SupportedLanguage::Hcl,
            SupportedLanguage::Jsonnet,
            SupportedLanguage::Dhall,
            SupportedLanguage::Nginx,
            SupportedLanguage::Diff,
            SupportedLanguage::Regex,
            SupportedLanguage::Vim,
            SupportedLanguage::EmacsLisp,
            SupportedLanguage::Latex,
            SupportedLanguage::Bibtex,
            SupportedLanguage::Typst,
            SupportedLanguage::Rst,
            SupportedLanguage::Org,
            SupportedLanguage::Assembly,
            SupportedLanguage::Verilog,
            SupportedLanguage::Vhdl,
            SupportedLanguage::Zephyr,
            SupportedLanguage::Janet,
            SupportedLanguage::Wren,
            SupportedLanguage::Vala,
            SupportedLanguage::Gd,
        ]
    }
}

// === Themed Tree-Sitter & Semantic Syntax Highlighting Engine ===

pub const HIGHLIGHT_NAMES: &[&str] = &[
    "keyword",
    "type",
    "function",
    "variable",
    "parameter",
    "property",
    "string",
    "number",
    "comment",
    "operator",
    "macro",
    "namespace",
    "tag",
];

fn highlight_idx_to_token_type(idx: usize) -> CanonicalTokenType {
    match idx {
        0 => CanonicalTokenType::Keyword,
        1 => CanonicalTokenType::Type,
        2 => CanonicalTokenType::Function,
        3 => CanonicalTokenType::Variable,
        4 => CanonicalTokenType::Parameter,
        5 => CanonicalTokenType::Property,
        6 => CanonicalTokenType::String,
        7 => CanonicalTokenType::Number,
        8 => CanonicalTokenType::Comment,
        9 => CanonicalTokenType::Operator,
        10 => CanonicalTokenType::Macro,
        11 => CanonicalTokenType::Namespace,
        12 => CanonicalTokenType::Tag,
        _ => CanonicalTokenType::Other,
    }
}

pub struct SyntaxEngine {
    pub language: SupportedLanguage,
    #[allow(dead_code)]
    pub parser: tree_sitter::Parser,
    pub highlight_config: Option<HighlightConfiguration>,
    pub highlighter: Highlighter,
    pub ts_tokens: HashMap<usize, Vec<SemanticTokenSpan>>,
    pub semantic_tokens: HashMap<usize, Vec<SemanticTokenSpan>>,
    pub has_grammar: bool,
}

impl SyntaxEngine {
    pub fn new(path: Option<&PathBuf>) -> Self {
        let language = SupportedLanguage::from_path(path);
        let mut parser = tree_sitter::Parser::new();
        let mut highlight_config = None;
        let mut has_grammar = false;

        if let Some(static_lang) = language.static_language() {
            let _ = parser.set_language(&static_lang);
            let query_src = if let Some(qp) = query_file_path(language.grammar_name()) {
                fs::read_to_string(qp)
                    .unwrap_or_else(|_| language.builtin_highlight_query().to_string())
            } else {
                language.builtin_highlight_query().to_string()
            };

            match HighlightConfiguration::new(
                static_lang,
                language.grammar_name(),
                &query_src,
                "",
                "",
            ) {
                Ok(mut config) => {
                    config.configure(HIGHLIGHT_NAMES);
                    highlight_config = Some(config);
                    has_grammar = true;
                }
                Err(err) => {
                    eprintln!(
                        "[subject0] Highlight config compilation error for {:?}: {err}",
                        language
                    );
                }
            }
        }

        Self {
            language,
            parser,
            highlight_config,
            highlighter: Highlighter::new(),
            ts_tokens: HashMap::new(),
            semantic_tokens: HashMap::new(),
            has_grammar,
        }
    }

    pub fn has_treesitter(&self) -> bool {
        self.has_grammar && self.highlight_config.is_some()
    }

    /// Reparses text into line-addressed syntax spans using `tree-sitter-highlight`.
    pub fn reparse(&mut self, text: &str) {
        if !self.has_treesitter() {
            return;
        }

        let Some(config) = &self.highlight_config else {
            return;
        };

        self.ts_tokens.clear();
        let bytes = text.as_bytes();

        let mut line_starts = vec![0usize];
        for (i, &b) in bytes.iter().enumerate() {
            if b == b'\n' {
                line_starts.push(i + 1);
            }
        }

        let byte_col_to_char_col = |line_str: &str, byte_col: usize| -> usize {
            let clean = line_str.strip_suffix('\r').unwrap_or(line_str);
            let mut boundary = byte_col.min(clean.len());
            while boundary > 0 && !clean.is_char_boundary(boundary) {
                boundary -= 1;
            }
            clean[..boundary].chars().count()
        };

        let Ok(events) = self.highlighter.highlight(config, bytes, None, |_| None) else {
            return;
        };

        let mut highlight_stack: Vec<CanonicalTokenType> = Vec::new();
        let mut current_token: Option<CanonicalTokenType> = None;
        let lines: Vec<&str> = text.split('\n').collect();

        for event in events {
            match event {
                Ok(HighlightEvent::HighlightStart(Highlight(idx))) => {
                    let token_type = highlight_idx_to_token_type(idx);
                    highlight_stack.push(token_type);
                    current_token = Some(token_type);
                }
                Ok(HighlightEvent::HighlightEnd) => {
                    highlight_stack.pop();
                    current_token = highlight_stack.last().copied();
                }
                Ok(HighlightEvent::Source { start, end }) => {
                    if let Some(token_type) = current_token {
                        if token_type == CanonicalTokenType::Other || start >= end {
                            continue;
                        }

                        let start_line = match line_starts.binary_search(&start) {
                            Ok(idx) => idx,
                            Err(idx) => idx.saturating_sub(1),
                        };
                        let end_target = end.saturating_sub(1).max(start);
                        let end_line = match line_starts.binary_search(&end_target) {
                            Ok(idx) => idx,
                            Err(idx) => idx.saturating_sub(1),
                        };

                        for row in start_line..=end_line {
                            if let Some(&line_str) = lines.get(row) {
                                let line_start_b = line_starts[row];
                                let line_end_b = if row + 1 < line_starts.len() {
                                    line_starts[row + 1].saturating_sub(1)
                                } else {
                                    bytes.len()
                                };

                                let b_start = start.max(line_start_b).saturating_sub(line_start_b);
                                let b_end = end.min(line_end_b).saturating_sub(line_start_b);

                                let c_start = byte_col_to_char_col(line_str, b_start);
                                let c_end = byte_col_to_char_col(line_str, b_end);
                                let len = c_end.saturating_sub(c_start);

                                if len > 0 {
                                    self.ts_tokens.entry(row).or_default().push(
                                        SemanticTokenSpan {
                                            line: row,
                                            start_col: c_start,
                                            length: len,
                                            token_type,
                                        },
                                    );
                                }
                            }
                        }
                    }
                }
                Err(_) => break,
            }
        }
    }

    pub fn set_semantic_tokens(&mut self, tokens: Vec<SemanticTokenSpan>) {
        if self.has_treesitter() {
            return;
        }
        self.semantic_tokens.clear();
        for tok in tokens {
            self.semantic_tokens.entry(tok.line).or_default().push(tok);
        }
    }

    /// Renders styled terminal spans for a single line using the active [`Theme`].
    pub fn highlight_line(
        &self,
        line_text: &str,
        line_idx: usize,
        theme: &Theme,
    ) -> Vec<Span<'static>> {
        if line_text.is_empty() {
            return vec![Span::raw("")];
        }

        if self.has_treesitter() {
            return self.render_treesitter_line(line_text, line_idx, theme);
        }

        if !self.semantic_tokens.is_empty() {
            return self.render_semantic_line(line_text, line_idx, theme);
        }

        vec![Span::styled(
            line_text.to_string(),
            Style::default().fg(theme.fg),
        )]
    }

    fn render_treesitter_line(
        &self,
        line_text: &str,
        line_idx: usize,
        theme: &Theme,
    ) -> Vec<Span<'static>> {
        let chars: Vec<char> = line_text.chars().collect();
        if chars.is_empty() {
            return vec![Span::raw("")];
        }

        let default_style = Style::default().fg(theme.fg);
        let mut styles = vec![default_style; chars.len()];

        if let Some(tokens) = self.ts_tokens.get(&line_idx) {
            let mut sorted = tokens.clone();

            let specificity = |t: CanonicalTokenType| -> u8 {
                match t {
                    CanonicalTokenType::Other => 0,
                    CanonicalTokenType::Variable => 1,
                    CanonicalTokenType::Operator => 2,
                    CanonicalTokenType::Property => 3,
                    CanonicalTokenType::Parameter => 4,
                    CanonicalTokenType::Namespace => 5,
                    CanonicalTokenType::Tag => 6,
                    CanonicalTokenType::Macro => 7,
                    CanonicalTokenType::Number => 8,
                    CanonicalTokenType::String => 9,
                    CanonicalTokenType::Comment => 10,
                    CanonicalTokenType::Type => 11,
                    CanonicalTokenType::Function => 12,
                    CanonicalTokenType::Keyword => 13,
                }
            };

            sorted.sort_by(|a, b| {
                b.length
                    .cmp(&a.length)
                    .then_with(|| specificity(a.token_type).cmp(&specificity(b.token_type)))
            });

            for tok in sorted {
                if tok.token_type == CanonicalTokenType::Other {
                    continue;
                }
                let start = tok.start_col;
                let end = (start + tok.length).min(chars.len());
                if start < chars.len() && end > start {
                    let style = Self::style_for_token_type(tok.token_type, theme);
                    for cell in &mut styles[start..end] {
                        *cell = style;
                    }
                }
            }
        }

        let mut spans = Vec::new();
        let mut chunk = String::new();
        let mut current_style = styles[0];

        for (i, &ch) in chars.iter().enumerate() {
            if styles[i] == current_style {
                chunk.push(ch);
            } else {
                spans.push(Span::styled(chunk.clone(), current_style));
                chunk.clear();
                chunk.push(ch);
                current_style = styles[i];
            }
        }
        if !chunk.is_empty() {
            spans.push(Span::styled(chunk, current_style));
        }

        spans
    }

    fn render_semantic_line(
        &self,
        line_text: &str,
        line_idx: usize,
        theme: &Theme,
    ) -> Vec<Span<'static>> {
        let chars: Vec<char> = line_text.chars().collect();
        if chars.is_empty() {
            return vec![Span::raw("")];
        }

        let default_style = Style::default().fg(theme.fg);
        let mut styles = vec![default_style; chars.len()];

        if let Some(tokens) = self.semantic_tokens.get(&line_idx) {
            for tok in tokens {
                if tok.token_type == CanonicalTokenType::Other {
                    continue;
                }
                let char_start = crate::lsp::utf16_to_char_col(line_text, tok.start_col);
                let char_end = crate::lsp::utf16_to_char_col(line_text, tok.start_col + tok.length);
                let start = char_start.min(chars.len());
                let end = char_end.min(chars.len());
                if end > start {
                    let style = Self::style_for_token_type(tok.token_type, theme);
                    for cell in &mut styles[start..end] {
                        *cell = style;
                    }
                }
            }
        }

        let mut spans = Vec::new();
        let mut chunk = String::new();
        let mut current_style = styles[0];

        for (i, &ch) in chars.iter().enumerate() {
            if styles[i] == current_style {
                chunk.push(ch);
            } else {
                spans.push(Span::styled(chunk.clone(), current_style));
                chunk.clear();
                chunk.push(ch);
                current_style = styles[i];
            }
        }
        if !chunk.is_empty() {
            spans.push(Span::styled(chunk, current_style));
        }

        spans
    }

    fn style_for_token_type(token_type: CanonicalTokenType, theme: &Theme) -> Style {
        match token_type {
            CanonicalTokenType::Keyword => Style::default()
                .fg(theme.syn_keyword)
                .add_modifier(Modifier::BOLD),
            CanonicalTokenType::Type => Style::default().fg(theme.syn_type),
            CanonicalTokenType::Function => Style::default().fg(theme.syn_function),
            CanonicalTokenType::String => Style::default().fg(theme.syn_string),
            CanonicalTokenType::Number => Style::default().fg(theme.syn_number),
            CanonicalTokenType::Comment => Style::default()
                .fg(theme.syn_comment)
                .add_modifier(Modifier::ITALIC),
            CanonicalTokenType::Macro => Style::default()
                .fg(theme.syn_macro)
                .add_modifier(Modifier::BOLD),
            CanonicalTokenType::Operator => Style::default().fg(theme.syn_operator),
            CanonicalTokenType::Namespace => Style::default().fg(theme.syn_namespace),
            CanonicalTokenType::Tag => Style::default()
                .fg(theme.syn_tag)
                .add_modifier(Modifier::BOLD),
            CanonicalTokenType::Variable => Style::default().fg(theme.syn_variable),
            CanonicalTokenType::Parameter => Style::default().fg(theme.syn_parameter),
            CanonicalTokenType::Property => Style::default().fg(theme.syn_property),
            CanonicalTokenType::Other => Style::default().fg(theme.fg),
        }
    }
}

// === UI Display Helpers ===

pub fn file_icon_and_color(path: Option<&PathBuf>) -> (&'static str, Color) {
    let file_name = path
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
        .unwrap_or("");
    let ext = path
        .and_then(|p| p.extension())
        .and_then(|e| e.to_str())
        .unwrap_or("");

    match file_name {
        "Dockerfile" | "Containerfile" => return (FILE_DOCKER, Color::Rgb(56, 157, 246)),
        "Makefile" | "GNUmakefile" | "makefile" => return (FILE_MAKE, Color::Rgb(235, 102, 60)),
        "CMakeLists.txt" => return (FILE_CMAKE, Color::Rgb(65, 172, 126)),
        ".gitignore" | ".gitmodules" | ".gitattributes" => {
            return (FILE_GIT, Color::Rgb(240, 80, 50));
        }
        ".env" => return (FILE_CONFIG, Color::Rgb(250, 210, 90)),
        _ => {}
    }

    match ext {
        "rs" => (FILE_RUST, Color::Rgb(235, 102, 60)),
        "py" | "pyi" | "pyw" => (FILE_PYTHON, Color::Rgb(255, 212, 59)),
        "go" => (FILE_GO, Color::Rgb(80, 200, 240)),
        "zig" | "zon" => (FILE_ZIG, Color::Rgb(245, 160, 60)),
        "c" | "h" => (FILE_C, Color::Rgb(80, 140, 255)),
        "cpp" | "hpp" | "cc" | "cxx" | "hh" | "hxx" | "c++" => (FILE_CPP, Color::Rgb(80, 140, 255)),
        "js" | "jsx" | "mjs" | "cjs" => (FILE_JAVASCRIPT, Color::Rgb(245, 215, 75)),
        "ts" | "tsx" | "mts" | "cts" => (FILE_TYPESCRIPT, Color::Rgb(80, 160, 240)),
        "html" | "htm" | "xhtml" => (FILE_HTML, Color::Rgb(240, 100, 60)),
        "css" | "scss" | "less" => (FILE_CSS, Color::Rgb(80, 160, 240)),
        "json" | "jsonc" | "json5" => (FILE_JSON, Color::Rgb(240, 200, 80)),
        "toml" => (FILE_TOML, Color::Rgb(160, 80, 50)),
        "yaml" | "yml" => (FILE_YAML, Color::Rgb(220, 100, 100)),
        "sh" | "bash" | "zsh" => (FILE_SHELL, Color::Rgb(100, 200, 140)),
        "lua" => (FILE_LUA, Color::Rgb(80, 140, 240)),
        "md" | "markdown" | "mdx" => (FILE_MARKDOWN, Color::Rgb(120, 180, 255)),
        "java" => (FILE_JAVA, Color::Rgb(240, 80, 80)),
        "cs" | "csx" => (FILE_CSHARP, Color::Rgb(180, 120, 240)),
        "php" | "phtml" => (FILE_PHP, Color::Rgb(130, 140, 220)),
        "rb" | "rake" | "gemspec" => (FILE_RUBY, Color::Rgb(220, 60, 60)),
        "kt" | "kts" => (FILE_KOTLIN, Color::Rgb(160, 100, 240)),
        "swift" => (FILE_SWIFT, Color::Rgb(240, 120, 60)),
        "dart" => (FILE_DART, Color::Rgb(80, 180, 240)),
        "sql" => (FILE_SQL, Color::Rgb(220, 140, 80)),
        "scala" | "sc" => (FILE_SCALA, Color::Rgb(220, 60, 60)),
        "odin" => (FILE_ODIN, Color::Rgb(80, 180, 220)),
        "hs" | "lhs" => (FILE_HASKELL, Color::Rgb(145, 110, 200)),
        "ex" | "exs" => (FILE_ELIXIR, Color::Rgb(170, 120, 220)),
        "erl" | "hrl" => (FILE_ERLANG, Color::Rgb(180, 60, 90)),
        "ml" | "mli" => (FILE_OCAML, Color::Rgb(240, 150, 60)),
        "fs" | "fsi" | "fsx" => (FILE_FSHARP, Color::Rgb(80, 160, 220)),
        "elm" => (FILE_ELM, Color::Rgb(110, 180, 150)),
        "jl" => (FILE_JULIA, Color::Rgb(160, 90, 200)),
        "nim" | "nims" => (FILE_NIM, Color::Rgb(245, 215, 75)),
        "cr" => (FILE_CRYSTAL, Color::Rgb(220, 220, 220)),
        "clj" | "cljs" | "cljc" | "edn" => (FILE_CLOJURE, Color::Rgb(110, 170, 240)),
        "nix" => (FILE_NIX, Color::Rgb(120, 160, 255)),
        "gleam" => (FILE_GLEAM, Color::Rgb(255, 170, 210)),
        "tf" | "tfvars" | "hcl" => (FILE_TERRAFORM, Color::Rgb(130, 90, 220)),
        "vue" => (FILE_VUE, Color::Rgb(70, 180, 140)),
        "svelte" => (FILE_SVELTE, Color::Rgb(240, 80, 40)),
        "astro" => (FILE_ASTRO, Color::Rgb(245, 120, 60)),
        "pl" | "pm" => (FILE_PERL, Color::Rgb(80, 140, 200)),
        "r" | "rmd" => (FILE_R, Color::Rgb(60, 120, 200)),
        "rkt" => (FILE_RACKET, Color::Rgb(180, 80, 80)),
        "lisp" | "lsp" | "cl" | "scm" | "ss" => (FILE_LISP, Color::Rgb(180, 180, 180)),
        "purs" => (FILE_PURESCRIPT, Color::Rgb(220, 220, 220)),
        "f90" | "f95" | "f03" | "f08" | "for" | "f" => (FILE_FORTRAN, Color::Rgb(130, 90, 180)),
        "d" | "di" => (FILE_D, Color::Rgb(180, 60, 60)),
        "v" => (FILE_V, Color::Rgb(100, 150, 200)),
        "ps1" | "psm1" | "psd1" => (FILE_POWERSHELL, Color::Rgb(80, 160, 240)),
        "fish" => (FILE_FISH, Color::Rgb(180, 70, 60)),
        "gradle" | "groovy" | "gvy" => (FILE_GRADLE, Color::Rgb(60, 140, 180)),
        "graphql" | "gql" => (FILE_GRAPHQL, Color::Rgb(230, 70, 160)),
        "proto" => (FILE_PROTO, Color::Rgb(80, 140, 200)),
        "xml" | "xsd" | "xsl" | "plist" => (FILE_XML, Color::Rgb(230, 100, 50)),
        "vim" => (FILE_VIM, Color::Rgb(50, 160, 70)),
        "tex" | "latex" | "sty" | "cls" | "bib" => (FILE_LATEX, Color::Rgb(60, 140, 120)),
        "typ" => (FILE_TYPST, Color::Rgb(60, 160, 190)),
        "asm" | "s" => (FILE_ASM, Color::Rgb(150, 150, 150)),
        _ => (FILE_GENERIC, Color::Rgb(160, 165, 175)),
    }
}

pub fn completion_kind_icon(kind: u64) -> (&'static str, Color) {
    match kind {
        1 => (KIND_TEXT, Color::Rgb(180, 185, 195)),
        2 => (KIND_METHOD, Color::Rgb(80, 200, 240)),
        3 => (KIND_FUNCTION, Color::Rgb(80, 200, 240)),
        4 => (KIND_CONSTRUCTOR, Color::Rgb(240, 180, 70)),
        5 => (KIND_FIELD, Color::Rgb(250, 210, 90)),
        6 => (KIND_VARIABLE, Color::Rgb(120, 160, 255)),
        7 => (KIND_CLASS, Color::Rgb(240, 180, 70)),
        8 => (KIND_INTERFACE, Color::Rgb(150, 166, 200)),
        9 => (KIND_MODULE, Color::Rgb(140, 220, 120)),
        10 => (KIND_PROPERTY, Color::Rgb(250, 210, 90)),
        11 => (KIND_UNIT, Color::Rgb(149, 169, 159)),
        12 => (KIND_VALUE, Color::Rgb(149, 169, 159)),
        13 => (KIND_ENUM, Color::Rgb(240, 180, 70)),
        14 => (KIND_KEYWORD, Color::Rgb(220, 110, 240)),
        15 => (KIND_SNIPPET, Color::Rgb(235, 140, 80)),
        16 => (KIND_COLOR, Color::Rgb(235, 100, 150)),
        17 => (KIND_FILE, Color::Rgb(120, 160, 255)),
        18 => (KIND_REFERENCE, Color::Rgb(120, 180, 240)),
        19 => (KIND_FOLDER, Color::Rgb(250, 190, 80)),
        20 => (KIND_ENUM_MEMBER, Color::Rgb(250, 210, 90)),
        21 => (KIND_CONSTANT, Color::Rgb(255, 221, 51)),
        22 => (KIND_STRUCT, Color::Rgb(240, 180, 70)),
        23 => (KIND_EVENT, Color::Rgb(255, 180, 50)),
        24 => (KIND_OPERATOR, Color::Rgb(220, 110, 240)),
        25 => (KIND_TYPE_PARAM, Color::Rgb(149, 169, 159)),
        _ => (KIND_DEFAULT, Color::Rgb(170, 175, 190)),
    }
}

pub fn symbol_kind_icon(kind: u64) -> (&'static str, Color) {
    match kind {
        1 => (KIND_FILE, Color::Rgb(120, 160, 255)),
        2 => (KIND_MODULE, Color::Rgb(220, 140, 80)),
        3 => (KIND_NAMESPACE, Color::Rgb(150, 166, 200)),
        4 => (KIND_PACKAGE, Color::Rgb(220, 140, 80)),
        5 => (KIND_CLASS, Color::Rgb(240, 180, 70)),
        6 => (KIND_METHOD, Color::Rgb(80, 200, 240)),
        7 => (KIND_PROPERTY, Color::Rgb(250, 210, 90)),
        8 => (KIND_FIELD, Color::Rgb(250, 210, 90)),
        9 => (KIND_CONSTRUCTOR, Color::Rgb(80, 200, 240)),
        10 => (KIND_ENUM, Color::Rgb(240, 180, 70)),
        11 => (KIND_INTERFACE, Color::Rgb(150, 166, 200)),
        12 => (KIND_FUNCTION, Color::Rgb(80, 200, 240)),
        13 => (KIND_VARIABLE, Color::Rgb(228, 228, 228)),
        14 => (KIND_CONSTANT, Color::Rgb(255, 221, 51)),
        15 => (KIND_STRING, Color::Rgb(115, 201, 54)),
        16 => (KIND_NUMBER, Color::Rgb(149, 169, 159)),
        17 => (KIND_BOOLEAN, Color::Rgb(255, 221, 51)),
        18 => (KIND_ARRAY, Color::Rgb(150, 166, 200)),
        19 => (KIND_OBJECT, Color::Rgb(240, 180, 70)),
        20 => (KIND_KEY, Color::Rgb(220, 110, 240)),
        21 => (KIND_NULL, Color::Rgb(149, 169, 159)),
        22 => (KIND_ENUM_MEMBER, Color::Rgb(250, 210, 90)),
        23 => (KIND_STRUCT, Color::Rgb(240, 180, 70)),
        24 => (KIND_EVENT, Color::Rgb(255, 180, 50)),
        25 => (KIND_OPERATOR, Color::Rgb(220, 110, 240)),
        26 => (KIND_TYPE_PARAM, Color::Rgb(149, 169, 159)),
        _ => (KIND_DEFAULT, Color::Rgb(170, 175, 190)),
    }
}
