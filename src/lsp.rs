//! # Language Server Protocol (LSP) Client & Themed Syntax Highlighting Engine
//!
//! This module implements language intelligence and syntax styling for `subject0`:
//!
//! 1. **Crash-Proof LSP Background Actor (`run_lsp_actor`)**:
//!    - Asynchronous, non-blocking Tokio actor communicating via JSON-RPC 2.0 with HTTP-style headers.
//!    - **Process Supervisor & Reboot Mechanics**: Automatically restarts crashed server processes with
//!      exponential backoff, re-establishing initialization handshakes and document sync without lost edits.
//!    - **Session Epoch Tracking**: Discards delayed messages from dead or superseded server processes.
//!    - **Request Cancellation**: Transmits `$/cancelRequest` when operations are invalidated.
//!    - **Full `lsp-types` Integration**: Standardized protocol types for handshakes, capabilities,
//!      completions, hover documentation, signature assistance, definitions, references, workspace edits,
//!      code actions, symbols, inlay hints, and push diagnostics.
//!
//! 2. **Compile-Time & Dynamic Syntax Engine (`SyntaxEngine`)**:
//!    - Statically compiled and dynamic highlighting powered by **`tree-sitter-highlight`**.
//!    - Dual-tier highlighting: Statically linked Tree-sitter AST queries with fallback to LSP Semantic Tokens.
//!    - Zero-copy line-rendering mapped against the active [`Theme`].

use anyhow::{Result, anyhow};
use lsp_types::{
    ClientCapabilities, CodeActionClientCapabilities, CodeActionKind, CodeActionKindLiteralSupport,
    CodeActionLiteralSupport, CodeActionOrCommand, CodeActionParams, CodeActionResponse,
    CompletionClientCapabilities, CompletionItemCapability, CompletionItemCapabilityResolveSupport,
    CompletionParams, CompletionResponse, CompletionTextEdit, Diagnostic, DiagnosticSeverity,
    DiagnosticTag, DidChangeTextDocumentParams, DidCloseTextDocumentParams,
    DidOpenTextDocumentParams, DidSaveTextDocumentParams, DocumentChanges,
    DocumentFormattingParams, DocumentSymbol, DocumentSymbolClientCapabilities,
    DocumentSymbolParams, DocumentSymbolResponse, Documentation,
    DynamicRegistrationClientCapabilities, FailureHandlingKind, GeneralClientCapabilities,
    GotoCapability, GotoDefinitionParams, GotoDefinitionResponse, Hover, HoverClientCapabilities,
    HoverContents, HoverParams, InitializeParams, InitializeResult, InitializedParams, InlayHint,
    InlayHintClientCapabilities, InlayHintKind, InlayHintLabel, InlayHintParams,
    InlayHintResolveClientCapabilities, InsertTextFormat, MarkedString, MarkupKind,
    ParameterInformationSettings, ParameterLabel, PartialResultParams, Position,
    PositionEncodingKind, PublishDiagnosticsClientCapabilities, PublishDiagnosticsParams, Range,
    ReferenceContext, ReferenceParams, RenameClientCapabilities, RenameParams,
    ResourceOperationKind, SemanticTokenModifier, SemanticTokenType,
    SemanticTokensClientCapabilities, SemanticTokensClientCapabilitiesRequests,
    SemanticTokensFullOptions, SemanticTokensParams, SemanticTokensResult, SignatureHelp,
    SignatureHelpClientCapabilities, SignatureHelpParams, SignatureInformationSettings,
    TextDocumentClientCapabilities, TextDocumentContentChangeEvent, TextDocumentIdentifier,
    TextDocumentItem, TextDocumentPositionParams, TextDocumentSyncClientCapabilities, TextEdit,
    TokenFormat, Uri, VersionedTextDocumentIdentifier, WindowClientCapabilities,
    WorkDoneProgressParams, WorkspaceClientCapabilities, WorkspaceEdit,
    WorkspaceEditClientCapabilities, WorkspaceFolder,
};
use ratatui::{
    style::{Color, Modifier, Style},
    text::Span,
};
use serde_json::Value;
use std::{
    collections::HashMap,
    env, fs,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, Command as TokioCommand},
    sync::mpsc,
    time::sleep,
};
use tree_sitter_highlight::{Highlight, HighlightConfiguration, HighlightEvent, Highlighter};

use crate::nerdfonts::{
    FILE_C, FILE_CPP, FILE_CSHARP, FILE_CSS, FILE_DART, FILE_GENERIC, FILE_GO, FILE_HTML,
    FILE_JAVA, FILE_JAVASCRIPT, FILE_JSON, FILE_KOTLIN, FILE_LUA, FILE_MARKDOWN, FILE_ODIN,
    FILE_PHP, FILE_PYTHON, FILE_RUBY, FILE_RUST, FILE_SCALA, FILE_SHELL, FILE_SQL, FILE_SWIFT,
    FILE_TOML, FILE_TYPESCRIPT, FILE_YAML, FILE_ZIG, KIND_BOOLEAN, KIND_CLASS, KIND_CONSTANT,
    KIND_DEFAULT, KIND_FILE, KIND_FUNCTION, KIND_KEYWORD, KIND_MODULE, KIND_NAMESPACE, KIND_NUMBER,
    KIND_OPERATOR, KIND_PROPERTY, KIND_STRING, KIND_TYPE_PARAM, KIND_VARIABLE,
};
use crate::theme::Theme;

// === Standardized LSP Client Types ===

/// Represents a single diagnostic entry emitted by an LSP server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiagnosticItem {
    pub line: usize,
    pub col: usize,
    pub end_line: usize,
    pub end_col: usize,
    pub message: String,
    pub severity: u8,
    pub is_unnecessary: bool,
    pub is_deprecated: bool,
}

/// An individual code completion candidate returned by the LSP server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SuggestionItem {
    pub label: String,
    pub insert_text: String,
    pub detail: Option<String>,
    pub documentation: Option<String>,
    pub kind: u64,
    pub additional_text_edits: Vec<TextEditItem>,
}

/// Semantic kind of an inlay hint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InlayHintType {
    Type,
    Parameter,
    Other,
}

/// Inferred type or parameter name inlay hint displayed inline within code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InlayHintItem {
    pub line: usize,
    pub col: usize,
    pub label: String,
    pub kind: InlayHintType,
    pub padding_left: bool,
    pub padding_right: bool,
}

/// Hover documentation payload containing documentation lines.
#[derive(Debug, Clone)]
pub struct HoverInfo {
    pub lines: Vec<String>,
}

/// Active function or method signature help tooltip information.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct SignatureHelpInfo {
    pub signature_label: String,
    pub active_parameter: Option<usize>,
    pub parameter_label: Option<String>,
    pub doc: Option<String>,
}

/// Source code location returned by definition and reference queries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocationItem {
    pub path: PathBuf,
    pub line: usize,
    pub col: usize,
}

/// Atomic text replacement range for formatting, renaming, and code actions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextEditItem {
    pub start_line: usize,
    pub start_col: usize,
    pub end_line: usize,
    pub end_col: usize,
    pub new_text: String,
}

/// Code action or quickfix candidate provided by the LSP server.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct CodeActionItem {
    pub title: String,
    pub kind: Option<String>,
    pub is_preferred: bool,
    pub edits: HashMap<PathBuf, Vec<TextEditItem>>,
}

/// Document outline symbol (function, struct, method, variable, enum, etc.).
#[derive(Debug, Clone)]
pub struct SymbolItem {
    pub name: String,
    pub kind: u64,
    pub line: usize,
    pub col: usize,
    pub container_name: Option<String>,
}

/// Operational lifecycle state of the LSP server process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LspStatus {
    Disabled,
    NotFound(String),
    Starting(String),
    Ready(String),
    Error(String),
}

/// Canonical semantic classification mapping server legends and queries into unified theme tokens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CanonicalTokenType {
    Keyword,
    Type,
    Function,
    Variable,
    Parameter,
    Property,
    String,
    Number,
    Comment,
    Operator,
    Macro,
    Namespace,
    Tag,
    Other,
}

impl CanonicalTokenType {
    pub fn from_name(name: &str) -> Self {
        match name {
            "keyword" | "boolean" | "conditional" | "repeat" | "modifier" => Self::Keyword,
            "type" | "class" | "struct" | "enum" | "union" | "interface" | "typeParameter"
            | "builtinType" => Self::Type,
            "function" | "method" | "constructor" => Self::Function,
            "variable" => Self::Variable,
            "parameter" => Self::Parameter,
            "property" | "field" | "enumMember" => Self::Property,
            "string" | "character" | "regexp" => Self::String,
            "number" | "float" => Self::Number,
            "comment" | "documentation" => Self::Comment,
            "operator" => Self::Operator,
            "macro" | "attribute" | "annotation" => Self::Macro,
            "namespace" | "module" | "package" => Self::Namespace,
            "tag" => Self::Tag,
            _ => Self::Other,
        }
    }

    #[allow(dead_code)]
    pub fn from_query_capture(capture_name: &str) -> Self {
        let name = capture_name.trim_start_matches('@');
        if name.starts_with("keyword")
            || name.starts_with("repeat")
            || name.starts_with("conditional")
            || name.starts_with("include")
            || name.starts_with("storage")
            || name == "exception"
        {
            Self::Keyword
        } else if name.starts_with("type")
            || name.starts_with("structure")
            || name.starts_with("class")
            || name.starts_with("storageclass")
            || name.starts_with("interface")
            || name.starts_with("enum")
            || name.starts_with("union")
        {
            Self::Type
        } else if name.starts_with("function")
            || name.starts_with("method")
            || name.starts_with("constructor")
        {
            Self::Function
        } else if name.starts_with("variable.parameter") || name == "parameter" {
            Self::Parameter
        } else if name.starts_with("variable") {
            Self::Variable
        } else if name.starts_with("property")
            || name.starts_with("field")
            || name.starts_with("member")
        {
            Self::Property
        } else if name.starts_with("string") || name.starts_with("character") {
            Self::String
        } else if name.starts_with("number")
            || name.starts_with("float")
            || name.starts_with("boolean")
        {
            Self::Number
        } else if name.starts_with("comment") || name.starts_with("doc") {
            Self::Comment
        } else if name.starts_with("operator") {
            Self::Operator
        } else if name.starts_with("macro")
            || name.starts_with("attribute")
            || name.starts_with("annotation")
        {
            Self::Macro
        } else if name.starts_with("module")
            || name.starts_with("namespace")
            || name.starts_with("package")
        {
            Self::Namespace
        } else if name.starts_with("tag") {
            Self::Tag
        } else {
            Self::Other
        }
    }
}

/// A single decoded semantic token span positioned in buffer coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SemanticTokenSpan {
    pub line: usize,
    pub start_col: usize,
    pub length: usize,
    pub token_type: CanonicalTokenType,
}

/// Inbound messages routed to the background LSP actor.
#[allow(dead_code)]
pub enum LspInbound {
    Change {
        text: String,
        version: i32,
    },
    Save,
    Completion {
        line: usize,
        col: usize,
        req_id: i64,
    },
    SemanticTokens {
        req_id: i64,
    },
    InlayHints {
        req_id: i64,
        max_lines: usize,
    },
    Hover {
        line: usize,
        col: usize,
        req_id: i64,
    },
    SignatureHelp {
        line: usize,
        col: usize,
        req_id: i64,
    },
    Definition {
        line: usize,
        col: usize,
        req_id: i64,
    },
    References {
        line: usize,
        col: usize,
        req_id: i64,
    },
    Formatting {
        req_id: i64,
    },
    CodeAction {
        line: usize,
        col: usize,
        diagnostics: Vec<DiagnosticItem>,
        req_id: i64,
    },
    Rename {
        line: usize,
        col: usize,
        new_name: String,
        req_id: i64,
    },
    DocumentSymbol {
        req_id: i64,
    },
    OpenFile {
        path: PathBuf,
        text: String,
        lang_id: String,
    },
    CancelRequest {
        req_id: i64,
    },
    Restart,
}

/// Outbound messages received from the background LSP actor.
#[allow(dead_code)]
pub enum LspOutbound {
    Status(LspStatus),
    Diagnostics(Vec<DiagnosticItem>),
    Completions {
        req_id: i64,
        items: Vec<SuggestionItem>,
    },
    SemanticTokens {
        tokens: Vec<SemanticTokenSpan>,
    },
    InlayHints {
        req_id: i64,
        hints: Vec<InlayHintItem>,
    },
    Hover {
        req_id: i64,
        hover: Option<HoverInfo>,
    },
    SignatureHelp {
        req_id: i64,
        help: Option<SignatureHelpInfo>,
    },
    Definition {
        req_id: i64,
        locations: Vec<LocationItem>,
    },
    References {
        req_id: i64,
        locations: Vec<LocationItem>,
    },
    Formatting {
        req_id: i64,
        edits: Vec<TextEditItem>,
    },
    CodeActions {
        req_id: i64,
        actions: Vec<CodeActionItem>,
    },
    Rename {
        req_id: i64,
        changes: HashMap<PathBuf, Vec<TextEditItem>>,
    },
    DocumentSymbols {
        req_id: i64,
        symbols: Vec<SymbolItem>,
    },
}

// === Cross-Platform Path & Directory Resolution ===

/// Locates standard data directory honoring Termux, XDG, macOS, and Windows conventions.
pub fn subject0_data_dir() -> PathBuf {
    if let Ok(custom) = env::var("SUBJECT0_DATA_DIR") {
        return PathBuf::from(custom);
    }
    if let Some(data) = dirs::data_local_dir() {
        return data.join("subject0");
    }
    if let Ok(prefix) = env::var("PREFIX") {
        return PathBuf::from(prefix).join("share/subject0");
    }
    dirs::home_dir().map_or_else(
        || PathBuf::from("./.subject0_data"),
        |h| h.join(".local/share/subject0"),
    )
}

/// Locates standard config directory honoring Termux, XDG, macOS, and Windows conventions.
pub fn subject0_config_dir() -> PathBuf {
    if let Ok(custom) = env::var("SUBJECT0_CONFIG_DIR") {
        return PathBuf::from(custom);
    }
    if let Some(cfg) = dirs::config_dir() {
        return cfg.join("subject0");
    }
    if let Ok(prefix) = env::var("PREFIX") {
        return PathBuf::from(prefix).join("etc/subject0");
    }
    dirs::home_dir().map_or_else(
        || PathBuf::from("./.subject0_cfg"),
        |h| h.join(".config/subject0"),
    )
}

/// Resolves an executable binary path across Android Termux, Linux, macOS, and Windows.
pub fn resolve_binary_path(cmd: &str) -> Option<PathBuf> {
    if let Ok(path) = which::which(cmd) {
        return Some(path);
    }

    let mut search_dirs = Vec::new();

    if let Some(home) = dirs::home_dir() {
        search_dirs.push(home.join(".cargo/bin"));
        search_dirs.push(home.join(".local/bin"));
        search_dirs.push(home.join("bin"));
        search_dirs.push(home.join(".npm-global/bin"));
        search_dirs.push(home.join("go/bin"));
    }

    if let Ok(prefix) = env::var("PREFIX") {
        search_dirs.push(PathBuf::from(prefix).join("bin"));
    }

    #[cfg(target_os = "windows")]
    {
        if let Ok(local_app) = env::var("LOCALAPPDATA") {
            let base = PathBuf::from(local_app);
            search_dirs.push(base.join("Programs"));
            search_dirs.push(base.join("Microsoft/WindowsApps"));
        }
        if let Some(home) = dirs::home_dir() {
            search_dirs.push(home.join("scoop/shims"));
        }
        search_dirs.push(PathBuf::from("C:\\ProgramData\\chocolatey\\bin"));
    }

    #[cfg(unix)]
    {
        search_dirs.push(PathBuf::from("/usr/local/bin"));
        search_dirs.push(PathBuf::from("/opt/homebrew/bin"));
        search_dirs.push(PathBuf::from("/usr/bin"));
    }

    for dir in search_dirs {
        let candidate = dir.join(cmd);
        if candidate.is_file() {
            return Some(candidate);
        }
        #[cfg(target_os = "windows")]
        {
            for ext in &["exe", "cmd", "bat", "ps1"] {
                let c = dir.join(format!("{cmd}.{ext}"));
                if c.is_file() {
                    return Some(c);
                }
            }
        }
    }

    None
}

/// Converts a local filesystem path to an RFC-compliant `file://` URI string.
pub fn file_to_uri(path: &Path) -> String {
    let abs = if path.is_absolute() {
        path.to_path_buf()
    } else if let Ok(cwd) = env::current_dir() {
        cwd.join(path)
    } else {
        path.to_path_buf()
    };

    if let Ok(url) = url::Url::from_file_path(&abs) {
        return url.to_string();
    }

    let s = abs.to_string_lossy();
    if cfg!(target_os = "windows") {
        let clean = s.replace('\\', "/");
        format!("file:///{}", clean.trim_start_matches('/'))
    } else {
        format!("file://{s}")
    }
}

/// Parses an RFC 3986 `file://` URI into a local `PathBuf`.
pub fn uri_to_path(uri_str: &str) -> Option<PathBuf> {
    if let Ok(url) = url::Url::parse(uri_str)
        && let Ok(path) = url.to_file_path()
    {
        return Some(path);
    }

    let raw = uri_str.strip_prefix("file://")?;
    #[cfg(target_os = "windows")]
    {
        let clean = raw.trim_start_matches('/');
        Some(PathBuf::from(clean.replace('/', "\\")))
    }
    #[cfg(not(target_os = "windows"))]
    {
        Some(PathBuf::from(raw))
    }
}

pub fn uris_match(a: &str, b: &str) -> bool {
    if let (Some(pa), Some(pb)) = (uri_to_path(a), uri_to_path(b)) {
        return pa == pb;
    }
    a.trim_end_matches('/')
        .eq_ignore_ascii_case(b.trim_end_matches('/'))
}

pub fn utf16_to_char_col(line: &str, utf16_col: usize) -> usize {
    let mut current_utf16 = 0usize;
    let mut char_count = 0usize;
    for c in line.chars() {
        if current_utf16 >= utf16_col {
            break;
        }
        current_utf16 += c.len_utf16();
        char_count += 1;
    }
    char_count
}

pub fn char_to_utf16_col(line: &str, char_col: usize) -> usize {
    let mut utf16_count = 0usize;
    for (idx, ch) in line.chars().enumerate() {
        if idx >= char_col {
            break;
        }
        utf16_count += ch.len_utf16();
    }
    utf16_count
}

pub fn parse_snippet_to_plain_text(snippet: &str) -> String {
    let mut result = String::with_capacity(snippet.len());
    let mut chars = snippet.chars().peekable();

    while let Some(c) = chars.next() {
        if c == '$' {
            if let Some(&'{') = chars.peek() {
                chars.next();
                let mut placeholder = String::new();
                let mut has_colon = false;
                for ch in chars.by_ref() {
                    if ch == '}' {
                        break;
                    }
                    if ch == ':' && !has_colon {
                        has_colon = true;
                        continue;
                    }
                    if has_colon {
                        placeholder.push(ch);
                    }
                }
                result.push_str(&placeholder);
            } else if let Some(&next_c) = chars.peek() {
                if next_c.is_ascii_digit() {
                    chars.next();
                } else {
                    result.push(c);
                }
            } else {
                result.push(c);
            }
        } else if c == '\\' {
            if let Some(&next_c) = chars.peek() {
                if next_c == '$' || next_c == '}' || next_c == '\\' {
                    result.push(next_c);
                    chars.next();
                } else {
                    result.push(c);
                }
            } else {
                result.push(c);
            }
        } else {
            result.push(c);
        }
    }
    result
}

// === JSON-RPC LSP Framing ===

pub async fn read_lsp_message<R: AsyncBufReadExt + Unpin>(reader: &mut R) -> Result<Value> {
    let mut content_length = 0usize;
    let mut line = String::new();

    loop {
        line.clear();
        let bytes_read = reader.read_line(&mut line).await?;
        if bytes_read == 0 {
            return Err(anyhow!("LSP stream closed"));
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            break;
        }
        let trimmed_lower = trimmed.to_ascii_lowercase();
        if let Some(val) = trimmed_lower.strip_prefix("content-length:") {
            content_length = val.trim().parse::<usize>()?;
        }
    }

    if content_length == 0 {
        return Err(anyhow!("Missing Content-Length header"));
    }
    if content_length > 100 * 1024 * 1024 {
        return Err(anyhow!("LSP payload exceeds 100MB safety limit"));
    }

    let mut body = vec![0u8; content_length];
    reader.read_exact(&mut body).await?;
    let val: Value = serde_json::from_slice(&body)?;
    Ok(val)
}

pub async fn send_lsp_message<W: AsyncWriteExt + Unpin>(
    writer: &mut W,
    value: &Value,
) -> Result<()> {
    let body = serde_json::to_string(value)?;
    let header = format!("Content-Length: {}\r\n\r\n", body.len());
    writer.write_all(header.as_bytes()).await?;
    writer.write_all(body.as_bytes()).await?;
    writer.flush().await?;
    Ok(())
}

pub async fn send_cancel_request<W: AsyncWriteExt + Unpin>(
    writer: &mut W,
    req_id: i64,
) -> Result<()> {
    let cancel = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "$/cancelRequest",
        "params": { "id": req_id }
    });
    send_lsp_message(writer, &cancel).await
}

pub fn server_cmd_and_args(cmd: &str) -> (String, Vec<&'static str>) {
    match cmd {
        "pyright" => {
            if resolve_binary_path("pyright-langserver").is_some() {
                ("pyright-langserver".to_string(), vec!["--stdio"])
            } else {
                ("pyright".to_string(), vec!["--stdio"])
            }
        }
        "pyright-langserver"
        | "typescript-language-server"
        | "vscode-html-language-server"
        | "vscode-css-language-server"
        | "vscode-json-language-server"
        | "yaml-language-server"
        | "intelephense"
        | "vtsls"
        | "docker-langserver" => (cmd.to_string(), vec!["--stdio"]),
        "bash-language-server" => ("bash-language-server".to_string(), vec!["start"]),
        "taplo" => ("taplo".to_string(), vec!["lsp", "stdio"]),
        "dart" => ("dart".to_string(), vec!["language-server"]),
        "omnisharp" => ("omnisharp".to_string(), vec!["-lsp"]),
        _ => (cmd.to_string(), vec![]),
    }
}

// === Type Adapters & Decoders using lsp-types ===

fn text_edit_to_item(edit: TextEdit) -> TextEditItem {
    TextEditItem {
        start_line: edit.range.start.line as usize,
        start_col: edit.range.start.character as usize,
        end_line: edit.range.end.line as usize,
        end_col: edit.range.end.character as usize,
        new_text: edit.new_text,
    }
}

fn completion_kind_to_u64(kind: Option<lsp_types::CompletionItemKind>) -> u64 {
    kind.and_then(|k| serde_json::to_value(k).ok())
        .and_then(|v| v.as_u64())
        .unwrap_or(0)
}

fn symbol_kind_to_u64(kind: lsp_types::SymbolKind) -> u64 {
    serde_json::to_value(kind)
        .ok()
        .and_then(|v| v.as_u64())
        .unwrap_or(0)
}

fn parse_lsp_diagnostics(params: Value) -> Result<(String, Vec<DiagnosticItem>)> {
    let parsed: PublishDiagnosticsParams = serde_json::from_value(params)?;
    let uri_str = parsed.uri.to_string();
    let mut items = Vec::with_capacity(parsed.diagnostics.len());

    for d in parsed.diagnostics {
        let is_unnecessary = d
            .tags
            .as_ref()
            .is_some_and(|tags| tags.contains(&DiagnosticTag::UNNECESSARY));
        let is_deprecated = d
            .tags
            .as_ref()
            .is_some_and(|tags| tags.contains(&DiagnosticTag::DEPRECATED));

        let severity = match d.severity {
            Some(DiagnosticSeverity::WARNING) => 2,
            Some(DiagnosticSeverity::INFORMATION) => 3,
            Some(DiagnosticSeverity::HINT) => 4,
            _ => 1,
        };

        items.push(DiagnosticItem {
            line: d.range.start.line as usize,
            col: d.range.start.character as usize,
            end_line: d.range.end.line as usize,
            end_col: d.range.end.character as usize,
            message: d.message,
            severity,
            is_unnecessary,
            is_deprecated,
        });
    }

    Ok((uri_str, items))
}

fn parse_lsp_completions(val: Value) -> Vec<SuggestionItem> {
    let response: CompletionResponse = match serde_json::from_value(val) {
        Ok(res) => res,
        Err(_) => return Vec::new(),
    };

    let items = match response {
        CompletionResponse::Array(items) => items,
        CompletionResponse::List(list) => list.items,
    };

    let mut suggestions = Vec::with_capacity(items.len());
    for item in items {
        let insert_text = if let Some(it) = item.insert_text {
            if item.insert_text_format == Some(InsertTextFormat::SNIPPET) {
                parse_snippet_to_plain_text(&it)
            } else {
                it
            }
        } else if let Some(edit) = item.text_edit {
            match edit {
                CompletionTextEdit::Edit(te) => {
                    if item.insert_text_format == Some(InsertTextFormat::SNIPPET) {
                        parse_snippet_to_plain_text(&te.new_text)
                    } else {
                        te.new_text
                    }
                }
                CompletionTextEdit::InsertAndReplace(ir) => {
                    if item.insert_text_format == Some(InsertTextFormat::SNIPPET) {
                        parse_snippet_to_plain_text(&ir.new_text)
                    } else {
                        ir.new_text
                    }
                }
            }
        } else {
            item.label.clone()
        };

        let documentation = item.documentation.map(|doc| match doc {
            Documentation::String(s) => s,
            Documentation::MarkupContent(mc) => mc.value,
        });

        let additional_text_edits = item
            .additional_text_edits
            .unwrap_or_default()
            .into_iter()
            .map(text_edit_to_item)
            .collect();

        let kind = completion_kind_to_u64(item.kind);

        suggestions.push(SuggestionItem {
            label: item.label,
            insert_text,
            detail: item.detail,
            documentation,
            kind,
            additional_text_edits,
        });
    }
    suggestions
}

fn parse_lsp_hover(val: Value) -> Option<HoverInfo> {
    let hover: Hover = serde_json::from_value(val).ok()?;
    let mut lines = Vec::new();

    match hover.contents {
        HoverContents::Scalar(marked) => match marked {
            MarkedString::String(s) => {
                for l in s.lines() {
                    lines.push(l.to_string());
                }
            }
            MarkedString::LanguageString(ls) => {
                for l in ls.value.lines() {
                    lines.push(l.to_string());
                }
            }
        },
        HoverContents::Array(arr) => {
            for marked in arr {
                match marked {
                    MarkedString::String(s) => {
                        for l in s.lines() {
                            lines.push(l.to_string());
                        }
                    }
                    MarkedString::LanguageString(ls) => {
                        for l in ls.value.lines() {
                            lines.push(l.to_string());
                        }
                    }
                }
            }
        }
        HoverContents::Markup(mc) => {
            for l in mc.value.lines() {
                lines.push(l.to_string());
            }
        }
    }

    if lines.is_empty() {
        None
    } else {
        Some(HoverInfo { lines })
    }
}

fn parse_lsp_signature_help(val: Value) -> Option<SignatureHelpInfo> {
    let help: SignatureHelp = serde_json::from_value(val).ok()?;
    if help.signatures.is_empty() {
        return None;
    }

    let active_sig_idx = help.active_signature.unwrap_or(0) as usize;
    let sig = help
        .signatures
        .get(active_sig_idx)
        .or_else(|| help.signatures.first())?;

    let signature_label = sig.label.clone();
    let active_param_idx = help
        .active_parameter
        .or(sig.active_parameter)
        .map(|v| v as usize);

    let mut parameter_label = None;
    if let (Some(params), Some(p_idx)) = (&sig.parameters, active_param_idx)
        && let Some(p_info) = params.get(p_idx)
    {
        match &p_info.label {
            ParameterLabel::Simple(s) => parameter_label = Some(s.clone()),
            ParameterLabel::LabelOffsets([start, end]) => {
                let s_idx = *start as usize;
                let e_idx = *end as usize;
                if s_idx <= e_idx && e_idx <= signature_label.len() {
                    parameter_label = Some(signature_label[s_idx..e_idx].to_string());
                }
            }
        }
    }

    let doc = sig.documentation.as_ref().map(|d| match d {
        Documentation::String(s) => s.clone(),
        Documentation::MarkupContent(mc) => mc.value.clone(),
    });

    Some(SignatureHelpInfo {
        signature_label,
        active_parameter: active_param_idx,
        parameter_label,
        doc,
    })
}

fn parse_lsp_locations(val: Value) -> Vec<LocationItem> {
    let resp: GotoDefinitionResponse = match serde_json::from_value(val) {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };

    let mut locations = Vec::new();
    match resp {
        GotoDefinitionResponse::Scalar(loc) => {
            if let Some(p) = uri_to_path(loc.uri.as_str()) {
                locations.push(LocationItem {
                    path: p,
                    line: loc.range.start.line as usize,
                    col: loc.range.start.character as usize,
                });
            }
        }
        GotoDefinitionResponse::Array(arr) => {
            for loc in arr {
                if let Some(p) = uri_to_path(loc.uri.as_str()) {
                    locations.push(LocationItem {
                        path: p,
                        line: loc.range.start.line as usize,
                        col: loc.range.start.character as usize,
                    });
                }
            }
        }
        GotoDefinitionResponse::Link(links) => {
            for link in links {
                if let Some(p) = uri_to_path(link.target_uri.as_str()) {
                    let range = link.target_selection_range;
                    locations.push(LocationItem {
                        path: p,
                        line: range.start.line as usize,
                        col: range.start.character as usize,
                    });
                }
            }
        }
    }
    locations
}

fn parse_lsp_formatting(val: Value) -> Vec<TextEditItem> {
    let edits: Vec<TextEdit> = serde_json::from_value(val).unwrap_or_default();
    edits.into_iter().map(text_edit_to_item).collect()
}

fn parse_lsp_workspace_edit(edit: WorkspaceEdit) -> HashMap<PathBuf, Vec<TextEditItem>> {
    let mut result: HashMap<PathBuf, Vec<TextEditItem>> = HashMap::new();

    if let Some(changes) = edit.changes {
        for (uri, edits) in changes {
            if let Some(path) = uri_to_path(uri.as_str()) {
                result.insert(path, edits.into_iter().map(text_edit_to_item).collect());
            }
        }
    }

    if let Some(doc_changes) = edit.document_changes {
        match doc_changes {
            DocumentChanges::Edits(edits_list) => {
                for doc_edit in edits_list {
                    if let Some(path) = uri_to_path(doc_edit.text_document.uri.as_str()) {
                        let converted: Vec<TextEditItem> = doc_edit
                            .edits
                            .into_iter()
                            .map(|oe| match oe {
                                lsp_types::OneOf::Left(te) => text_edit_to_item(te),
                                lsp_types::OneOf::Right(ae) => text_edit_to_item(ae.text_edit),
                            })
                            .collect();
                        result.entry(path).or_default().extend(converted);
                    }
                }
            }
            DocumentChanges::Operations(ops) => {
                for op in ops {
                    if let lsp_types::DocumentChangeOperation::Edit(edit_op) = op
                        && let Some(path) = uri_to_path(edit_op.text_document.uri.as_str())
                    {
                        let converted: Vec<TextEditItem> = edit_op
                            .edits
                            .into_iter()
                            .map(|oe| match oe {
                                lsp_types::OneOf::Left(te) => text_edit_to_item(te),
                                lsp_types::OneOf::Right(ae) => text_edit_to_item(ae.text_edit),
                            })
                            .collect();
                        result.entry(path).or_default().extend(converted);
                    }
                }
            }
        }
    }

    result
}

fn parse_lsp_code_actions(val: Value) -> Vec<CodeActionItem> {
    let resp: CodeActionResponse = match serde_json::from_value(val) {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };

    let mut actions = Vec::new();
    for item in resp {
        match item {
            CodeActionOrCommand::CodeAction(ca) => {
                let edits = ca.edit.map(parse_lsp_workspace_edit).unwrap_or_default();
                actions.push(CodeActionItem {
                    title: ca.title,
                    kind: ca.kind.map(|k| k.as_str().to_string()),
                    is_preferred: ca.is_preferred.unwrap_or(false),
                    edits,
                });
            }
            CodeActionOrCommand::Command(cmd) => {
                actions.push(CodeActionItem {
                    title: cmd.title,
                    kind: None,
                    is_preferred: false,
                    edits: HashMap::new(),
                });
            }
        }
    }
    actions
}

fn parse_lsp_document_symbols_recursive(
    symbols: Vec<DocumentSymbol>,
    container: Option<&str>,
    out: &mut Vec<SymbolItem>,
) {
    for sym in symbols {
        #[allow(deprecated)]
        out.push(SymbolItem {
            name: sym.name.clone(),
            kind: symbol_kind_to_u64(sym.kind),
            line: sym.selection_range.start.line as usize,
            col: sym.selection_range.start.character as usize,
            container_name: container.map(ToString::to_string),
        });

        if let Some(children) = sym.children {
            parse_lsp_document_symbols_recursive(children, Some(&sym.name), out);
        }
    }
}

fn parse_lsp_document_symbols(val: Value) -> Vec<SymbolItem> {
    let resp: DocumentSymbolResponse = match serde_json::from_value(val) {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };

    let mut result = Vec::new();
    match resp {
        DocumentSymbolResponse::Flat(flat) => {
            for sym in flat {
                #[allow(deprecated)]
                result.push(SymbolItem {
                    name: sym.name,
                    kind: symbol_kind_to_u64(sym.kind),
                    line: sym.location.range.start.line as usize,
                    col: sym.location.range.start.character as usize,
                    container_name: sym.container_name,
                });
            }
        }
        DocumentSymbolResponse::Nested(nested) => {
            parse_lsp_document_symbols_recursive(nested, None, &mut result);
        }
    }
    result
}

fn parse_lsp_inlay_hints(val: Value) -> Vec<InlayHintItem> {
    let hints: Vec<InlayHint> = match serde_json::from_value(val) {
        Ok(h) => h,
        Err(_) => return Vec::new(),
    };

    hints
        .into_iter()
        .map(|h| {
            let label = match h.label {
                InlayHintLabel::String(s) => s,
                InlayHintLabel::LabelParts(parts) => parts.into_iter().map(|p| p.value).collect(),
            };

            let kind = match h.kind {
                Some(InlayHintKind::TYPE) => InlayHintType::Type,
                Some(InlayHintKind::PARAMETER) => InlayHintType::Parameter,
                _ => InlayHintType::Other,
            };

            InlayHintItem {
                line: h.position.line as usize,
                col: h.position.character as usize,
                label,
                kind,
                padding_left: h.padding_left.unwrap_or(false),
                padding_right: h.padding_right.unwrap_or(false),
            }
        })
        .collect()
}

// === Process Supervisor, Auto-Restart & Background Actor ===

fn spawn_lsp_child(
    bin_path: &Path,
    args: &[&str],
) -> Result<(Child, ChildStdin, BufReader<tokio::process::ChildStdout>)> {
    let mut cmd_builder = TokioCommand::new(bin_path);
    cmd_builder.args(args);
    cmd_builder.kill_on_drop(true);

    let mut child = cmd_builder
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;

    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| anyhow!("Child stdin acquired"))?;
    let stdout = BufReader::new(
        child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("Child stdout acquired"))?,
    );

    Ok((child, stdin, stdout))
}

async fn perform_handshake(
    stdin: &mut ChildStdin,
    stdout: &mut BufReader<tokio::process::ChildStdout>,
    root_dir: &Path,
    root_uri: &Uri,
) -> Result<Vec<String>> {
    #[allow(deprecated)]
    let init_params = InitializeParams {
        process_id: Some(std::process::id()),
        root_path: Some(root_dir.to_string_lossy().to_string()),
        root_uri: Some(root_uri.clone()),
        initialization_options: Some(serde_json::json!({
            "checkOnSave": true,
            "cargo": { "allFeatures": true },
            "procMacro": { "enable": true }
        })),
        capabilities: ClientCapabilities {
            workspace: Some(WorkspaceClientCapabilities {
                apply_edit: Some(true),
                workspace_edit: Some(WorkspaceEditClientCapabilities {
                    document_changes: Some(true),
                    resource_operations: Some(vec![
                        ResourceOperationKind::Create,
                        ResourceOperationKind::Rename,
                        ResourceOperationKind::Delete,
                    ]),
                    failure_handling: Some(FailureHandlingKind::Undo),
                    normalizes_line_endings: Some(true),
                    change_annotation_support: None,
                }),
                did_change_configuration: Some(DynamicRegistrationClientCapabilities {
                    dynamic_registration: Some(false),
                }),
                workspace_folders: Some(true),
                configuration: Some(true),
                ..Default::default()
            }),
            text_document: Some(TextDocumentClientCapabilities {
                synchronization: Some(TextDocumentSyncClientCapabilities {
                    dynamic_registration: Some(false),
                    will_save: Some(false),
                    will_save_wait_until: Some(false),
                    did_save: Some(true),
                }),
                completion: Some(CompletionClientCapabilities {
                    dynamic_registration: Some(false),
                    completion_item: Some(CompletionItemCapability {
                        snippet_support: Some(true),
                        commit_characters_support: Some(true),
                        documentation_format: Some(vec![
                            MarkupKind::Markdown,
                            MarkupKind::PlainText,
                        ]),
                        deprecated_support: Some(true),
                        preselect_support: Some(true),
                        insert_replace_support: Some(true),
                        resolve_support: Some(CompletionItemCapabilityResolveSupport {
                            properties: vec![
                                "documentation".to_string(),
                                "detail".to_string(),
                                "additionalTextEdits".to_string(),
                            ],
                        }),
                        ..Default::default()
                    }),
                    context_support: Some(true),
                    ..Default::default()
                }),
                hover: Some(HoverClientCapabilities {
                    dynamic_registration: Some(false),
                    content_format: Some(vec![MarkupKind::Markdown, MarkupKind::PlainText]),
                }),
                signature_help: Some(SignatureHelpClientCapabilities {
                    dynamic_registration: Some(false),
                    signature_information: Some(SignatureInformationSettings {
                        documentation_format: Some(vec![
                            MarkupKind::Markdown,
                            MarkupKind::PlainText,
                        ]),
                        parameter_information: Some(ParameterInformationSettings {
                            label_offset_support: Some(true),
                        }),
                        active_parameter_support: Some(true),
                    }),
                    context_support: Some(true),
                }),
                definition: Some(GotoCapability {
                    dynamic_registration: Some(false),
                    link_support: Some(true),
                }),
                references: Some(DynamicRegistrationClientCapabilities {
                    dynamic_registration: Some(false),
                }),
                document_symbol: Some(DocumentSymbolClientCapabilities {
                    dynamic_registration: Some(false),
                    hierarchical_document_symbol_support: Some(true),
                    ..Default::default()
                }),
                formatting: Some(DynamicRegistrationClientCapabilities {
                    dynamic_registration: Some(false),
                }),
                code_action: Some(CodeActionClientCapabilities {
                    dynamic_registration: Some(false),
                    code_action_literal_support: Some(CodeActionLiteralSupport {
                        code_action_kind: CodeActionKindLiteralSupport {
                            value_set: vec![
                                CodeActionKind::QUICKFIX.as_str().to_string(),
                                CodeActionKind::REFACTOR.as_str().to_string(),
                                CodeActionKind::SOURCE.as_str().to_string(),
                            ],
                        },
                    }),
                    is_preferred_support: Some(true),
                    ..Default::default()
                }),
                rename: Some(RenameClientCapabilities {
                    dynamic_registration: Some(false),
                    prepare_support: Some(false),
                    ..Default::default()
                }),
                inlay_hint: Some(InlayHintClientCapabilities {
                    dynamic_registration: Some(false),
                    resolve_support: Some(InlayHintResolveClientCapabilities {
                        properties: vec![],
                    }),
                }),
                publish_diagnostics: Some(PublishDiagnosticsClientCapabilities {
                    related_information: Some(true),
                    tag_support: Some(lsp_types::TagSupport {
                        value_set: vec![DiagnosticTag::UNNECESSARY, DiagnosticTag::DEPRECATED],
                    }),
                    version_support: Some(true),
                    ..Default::default()
                }),

                semantic_tokens: Some(SemanticTokensClientCapabilities {
                    dynamic_registration: Some(false),
                    requests: SemanticTokensClientCapabilitiesRequests {
                        range: Some(false),
                        full: Some(SemanticTokensFullOptions::Bool(true)),
                    },
                    token_types: vec![
                        SemanticTokenType::NAMESPACE,
                        SemanticTokenType::TYPE,
                        SemanticTokenType::CLASS,
                        SemanticTokenType::ENUM,
                        SemanticTokenType::INTERFACE,
                        SemanticTokenType::STRUCT,
                        SemanticTokenType::TYPE_PARAMETER,
                        SemanticTokenType::PARAMETER,
                        SemanticTokenType::VARIABLE,
                        SemanticTokenType::PROPERTY,
                        SemanticTokenType::ENUM_MEMBER,
                        SemanticTokenType::FUNCTION,
                        SemanticTokenType::METHOD,
                        SemanticTokenType::MACRO,
                        SemanticTokenType::KEYWORD,
                        SemanticTokenType::COMMENT,
                        SemanticTokenType::STRING,
                        SemanticTokenType::NUMBER,
                        SemanticTokenType::OPERATOR,
                    ],
                    token_modifiers: vec![
                        SemanticTokenModifier::DECLARATION,
                        SemanticTokenModifier::DEFINITION,
                        SemanticTokenModifier::READONLY,
                        SemanticTokenModifier::STATIC,
                        SemanticTokenModifier::DEFAULT_LIBRARY,
                    ],
                    formats: vec![TokenFormat::RELATIVE],
                    ..Default::default()
                }),
                ..Default::default()
            }),
            window: Some(WindowClientCapabilities {
                work_done_progress: Some(true),
                ..Default::default()
            }),
            general: Some(GeneralClientCapabilities {
                position_encodings: Some(vec![
                    PositionEncodingKind::UTF16,
                    PositionEncodingKind::UTF8,
                ]),
                ..Default::default()
            }),
            ..Default::default()
        },
        workspace_folders: Some(vec![WorkspaceFolder {
            uri: root_uri.clone(),
            name: "root".to_string(),
        }]),
        ..Default::default()
    };

    let init_msg = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": init_params
    });

    send_lsp_message(stdin, &init_msg).await?;

    let mut server_legend = Vec::new();
    let init_deadline = tokio::time::Instant::now() + Duration::from_secs(30);

    while tokio::time::Instant::now() < init_deadline {
        let msg = tokio::select! {
            res = read_lsp_message(stdout) => res?,
            () = sleep(Duration::from_millis(50)) => continue,
        };

        if let Some(method) = msg.get("method").and_then(Value::as_str) {
            if let Some(req_id) = msg.get("id") {
                let resp = serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": req_id,
                    "result": if method == "workspace/configuration" {
                        serde_json::json!([{}])
                    } else if method == "workspace/workspaceFolders" {
                        serde_json::json!([{ "uri": root_uri.as_str(), "name": "root" }])
                    } else {
                        Value::Null
                    }
                });
                let _ = send_lsp_message(stdin, &resp).await;
            }
        } else if msg.get("id").and_then(Value::as_i64) == Some(1) {
            if let Ok(init_res) = serde_json::from_value::<InitializeResult>(
                msg.get("result").cloned().unwrap_or(Value::Null),
            ) && let Some(lsp_types::ServerCapabilities {
                semantic_tokens_provider:
                    Some(lsp_types::SemanticTokensServerCapabilities::SemanticTokensOptions(opt)),
                ..
            }) = init_res.capabilities.into()
            {
                server_legend = opt
                    .legend
                    .token_types
                    .into_iter()
                    .map(|t| t.as_str().to_string())
                    .collect();
            }
            break;
        }
    }

    if server_legend.is_empty() {
        server_legend = vec![
            "namespace".into(),
            "type".into(),
            "class".into(),
            "enum".into(),
            "interface".into(),
            "struct".into(),
            "typeParameter".into(),
            "parameter".into(),
            "variable".into(),
            "property".into(),
            "enumMember".into(),
            "function".into(),
            "method".into(),
            "macro".into(),
            "keyword".into(),
            "comment".into(),
            "string".into(),
            "number".into(),
            "operator".into(),
        ];
    }

    let initialized = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "initialized",
        "params": InitializedParams {}
    });
    send_lsp_message(stdin, &initialized).await?;

    Ok(server_legend)
}

async fn graceful_shutdown(
    stdin: &mut ChildStdin,
    stdout: &mut BufReader<tokio::process::ChildStdout>,
) {
    let shutdown_req = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 999_999,
        "method": "shutdown",
        "params": Value::Null
    });

    if send_lsp_message(stdin, &shutdown_req).await.is_ok() {
        let shutdown_timeout = sleep(Duration::from_millis(400));
        tokio::pin!(shutdown_timeout);
        loop {
            tokio::select! {
                () = &mut shutdown_timeout => break,
                msg = read_lsp_message(stdout) => {
                    if let Ok(v) = msg {
                        if v.get("id").and_then(Value::as_i64) == Some(999_999) {
                            break;
                        }
                    } else {
                        break;
                    }
                }
            }
        }
        let exit_notif = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "exit",
            "params": Value::Null
        });
        let _ = send_lsp_message(stdin, &exit_notif).await;
    }
}

/// Asynchronous LSP Process Supervisor with Auto-Restart, Crash Proofing, & Reboot.
pub async fn run_lsp_actor(
    initial_file: PathBuf,
    initial_lang: String,
    server_cmd: String,
    mut rx: mpsc::UnboundedReceiver<LspInbound>,
    tx: mpsc::UnboundedSender<LspOutbound>,
    initial_text: String,
) {
    let (resolved_cmd, args) = server_cmd_and_args(&server_cmd);
    let Some(bin_path) = resolve_binary_path(&resolved_cmd) else {
        let _ = tx.send(LspOutbound::Status(LspStatus::NotFound(server_cmd)));
        return;
    };

    let root_dir = env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let root_uri_str = file_to_uri(&root_dir);
    let root_uri = root_uri_str
        .parse::<Uri>()
        .unwrap_or_else(|_| "file:///".parse().unwrap());

    let mut current_file = initial_file;
    let mut current_lang = initial_lang;
    let mut current_text = initial_text;
    let mut current_version = 1i32;
    let mut restart_attempts = 0usize;
    let mut session_id = 0u64;

    'supervisor: loop {
        session_id += 1;
        let active_session = session_id;
        let _ = tx.send(LspOutbound::Status(LspStatus::Starting(server_cmd.clone())));

        let (mut child, mut stdin, mut stdout) = match spawn_lsp_child(&bin_path, &args) {
            Ok(triplet) => triplet,
            Err(e) => {
                let _ = tx.send(LspOutbound::Status(LspStatus::Error(format!(
                    "Failed to start: {e}"
                ))));
                return;
            }
        };

        let server_legend =
            match perform_handshake(&mut stdin, &mut stdout, &root_dir, &root_uri).await {
                Ok(legend) => legend,
                Err(e) => {
                    let _ = tx.send(LspOutbound::Status(LspStatus::Error(format!(
                        "Handshake failed: {e}"
                    ))));
                    let _ = child.kill().await;
                    return;
                }
            };

        // Open active document
        let current_file_uri = file_to_uri(&current_file);
        if let Ok(uri) = current_file_uri.parse::<Uri>() {
            let did_open = serde_json::json!({
                "jsonrpc": "2.0",
                "method": "textDocument/didOpen",
                "params": DidOpenTextDocumentParams {
                    text_document: TextDocumentItem {
                        uri,
                        language_id: current_lang.clone(),
                        version: current_version,
                        text: current_text.clone(),
                    }
                }
            });
            let _ = send_lsp_message(&mut stdin, &did_open).await;
        }

        let _ = tx.send(LspOutbound::Status(LspStatus::Ready(server_cmd.clone())));
        if restart_attempts > 0 {
            restart_attempts = 0;
        }

        let mut pending_requests: HashMap<i64, &'static str> = HashMap::new();
        let mut last_completion_req: Option<i64> = None;
        let mut child_crashed = false;
        let mut manual_reboot = false;

        'event_loop: loop {
            tokio::select! {
                inbound = rx.recv() => {
                    match inbound {
                        Some(LspInbound::Restart) => {
                            manual_reboot = true;
                            break 'event_loop;
                        }
                        Some(LspInbound::OpenFile { path, text, lang_id }) => {
                            let old_uri_str = file_to_uri(&current_file);
                            let new_uri_str = file_to_uri(&path);

                            if old_uri_str != new_uri_str
                                && let Ok(old_uri) = old_uri_str.parse::<Uri>() {
                                    let did_close = serde_json::json!({
                                        "jsonrpc": "2.0",
                                        "method": "textDocument/didClose",
                                        "params": DidCloseTextDocumentParams {
                                            text_document: TextDocumentIdentifier { uri: old_uri }
                                        }
                                    });
                                    let _ = send_lsp_message(&mut stdin, &did_close).await;
                                }

                            current_file = path;
                            current_text = text;
                            current_lang = lang_id;
                            current_version = 1;

                            if let Ok(new_uri) = new_uri_str.parse::<Uri>() {
                                let did_open = serde_json::json!({
                                    "jsonrpc": "2.0",
                                    "method": "textDocument/didOpen",
                                    "params": DidOpenTextDocumentParams {
                                        text_document: TextDocumentItem {
                                            uri: new_uri,
                                            language_id: current_lang.clone(),
                                            version: current_version,
                                            text: current_text.clone(),
                                        }
                                    }
                                });
                                let _ = send_lsp_message(&mut stdin, &did_open).await;
                            }
                        }
                        Some(LspInbound::Change { text, version }) => {
                            current_text = text.clone();
                            current_version = version;
                            let uri_str = file_to_uri(&current_file);
                            if let Ok(uri) = uri_str.parse::<Uri>() {
                                let did_change = serde_json::json!({
                                    "jsonrpc": "2.0",
                                    "method": "textDocument/didChange",
                                    "params": DidChangeTextDocumentParams {
                                        text_document: VersionedTextDocumentIdentifier { uri, version },
                                        content_changes: vec![TextDocumentContentChangeEvent {
                                            range: None,
                                            range_length: None,
                                            text,
                                        }],
                                    }
                                });
                                let _ = send_lsp_message(&mut stdin, &did_change).await;
                            }
                        }
                        Some(LspInbound::Save) => {
                            let uri_str = file_to_uri(&current_file);
                            if let Ok(uri) = uri_str.parse::<Uri>() {
                                let did_save = serde_json::json!({
                                    "jsonrpc": "2.0",
                                    "method": "textDocument/didSave",
                                    "params": DidSaveTextDocumentParams {
                                        text_document: TextDocumentIdentifier { uri },
                                        text: None,
                                    }
                                });
                                let _ = send_lsp_message(&mut stdin, &did_save).await;
                            }
                        }
                        Some(LspInbound::Completion { line, col, req_id }) => {
                            if let Some(prev) = last_completion_req.take() {
                                let _ = send_cancel_request(&mut stdin, prev).await;
                            }
                            last_completion_req = Some(req_id);
                            pending_requests.insert(req_id, "completion");

                            let uri_str = file_to_uri(&current_file);
                            if let Ok(uri) = uri_str.parse::<Uri>() {
                                let comp_req = serde_json::json!({
                                    "jsonrpc": "2.0",
                                    "id": req_id,
                                    "method": "textDocument/completion",
                                    "params": CompletionParams {
                                        text_document_position: TextDocumentPositionParams {
                                            text_document: TextDocumentIdentifier { uri },
                                            position: Position { line: line as u32, character: col as u32 },
                                        },
                                        work_done_progress_params: WorkDoneProgressParams::default(),
                                        partial_result_params: PartialResultParams::default(),
                                        context: None,
                                    }
                                });
                                let _ = send_lsp_message(&mut stdin, &comp_req).await;
                            }
                        }
                        Some(LspInbound::CancelRequest { req_id }) => {
                            pending_requests.remove(&req_id);
                            let _ = send_cancel_request(&mut stdin, req_id).await;
                        }
                        Some(LspInbound::SemanticTokens { req_id }) => {
                            pending_requests.insert(req_id, "semanticTokens");
                            let uri_str = file_to_uri(&current_file);
                            if let Ok(uri) = uri_str.parse::<Uri>() {
                                let st_req = serde_json::json!({
                                    "jsonrpc": "2.0",
                                    "id": req_id,
                                    "method": "textDocument/semanticTokens/full",
                                    "params": SemanticTokensParams {
                                        text_document: TextDocumentIdentifier { uri },
                                        work_done_progress_params: WorkDoneProgressParams::default(),
                                        partial_result_params: PartialResultParams::default(),
                                    }
                                });
                                let _ = send_lsp_message(&mut stdin, &st_req).await;
                            }
                        }
                        Some(LspInbound::InlayHints { req_id, max_lines }) => {
                            pending_requests.insert(req_id, "inlayHints");
                            let uri_str = file_to_uri(&current_file);
                            if let Ok(uri) = uri_str.parse::<Uri>() {
                                let ih_req = serde_json::json!({
                                    "jsonrpc": "2.0",
                                    "id": req_id,
                                    "method": "textDocument/inlayHint",
                                    "params": InlayHintParams {
                                        text_document: TextDocumentIdentifier { uri },
                                        range: Range {
                                            start: Position { line: 0, character: 0 },
                                            end: Position { line: max_lines.max(100) as u32, character: 0 },
                                        },
                                        work_done_progress_params: WorkDoneProgressParams::default(),
                                    }
                                });
                                let _ = send_lsp_message(&mut stdin, &ih_req).await;
                            }
                        }
                        Some(LspInbound::Hover { line, col, req_id }) => {
                            pending_requests.insert(req_id, "hover");
                            let uri_str = file_to_uri(&current_file);
                            if let Ok(uri) = uri_str.parse::<Uri>() {
                                let h_req = serde_json::json!({
                                    "jsonrpc": "2.0",
                                    "id": req_id,
                                    "method": "textDocument/hover",
                                    "params": HoverParams {
                                        text_document_position_params: TextDocumentPositionParams {
                                            text_document: TextDocumentIdentifier { uri },
                                            position: Position { line: line as u32, character: col as u32 },
                                        },
                                        work_done_progress_params: WorkDoneProgressParams::default(),
                                    }
                                });
                                let _ = send_lsp_message(&mut stdin, &h_req).await;
                            }
                        }
                        Some(LspInbound::SignatureHelp { line, col, req_id }) => {
                            pending_requests.insert(req_id, "signatureHelp");
                            let uri_str = file_to_uri(&current_file);
                            if let Ok(uri) = uri_str.parse::<Uri>() {
                                let sh_req = serde_json::json!({
                                    "jsonrpc": "2.0",
                                    "id": req_id,
                                    "method": "textDocument/signatureHelp",
                                    "params": SignatureHelpParams {
                                        text_document_position_params: TextDocumentPositionParams {
                                            text_document: TextDocumentIdentifier { uri },
                                            position: Position { line: line as u32, character: col as u32 },
                                        },
                                        work_done_progress_params: WorkDoneProgressParams::default(),
                                        context: None,
                                    }
                                });
                                let _ = send_lsp_message(&mut stdin, &sh_req).await;
                            }
                        }
                        Some(LspInbound::Definition { line, col, req_id }) => {
                            pending_requests.insert(req_id, "definition");
                            let uri_str = file_to_uri(&current_file);
                            if let Ok(uri) = uri_str.parse::<Uri>() {
                                let def_req = serde_json::json!({
                                    "jsonrpc": "2.0",
                                    "id": req_id,
                                    "method": "textDocument/definition",
                                    "params": GotoDefinitionParams {
                                        text_document_position_params: TextDocumentPositionParams {
                                            text_document: TextDocumentIdentifier { uri },
                                            position: Position { line: line as u32, character: col as u32 },
                                        },
                                        work_done_progress_params: WorkDoneProgressParams::default(),
                                        partial_result_params: PartialResultParams::default(),
                                    }
                                });
                                let _ = send_lsp_message(&mut stdin, &def_req).await;
                            }
                        }
                        Some(LspInbound::References { line, col, req_id }) => {
                            pending_requests.insert(req_id, "references");
                            let uri_str = file_to_uri(&current_file);
                            if let Ok(uri) = uri_str.parse::<Uri>() {
                                let ref_req = serde_json::json!({
                                    "jsonrpc": "2.0",
                                    "id": req_id,
                                    "method": "textDocument/references",
                                    "params": ReferenceParams {
                                        text_document_position: TextDocumentPositionParams {
                                            text_document: TextDocumentIdentifier { uri },
                                            position: Position { line: line as u32, character: col as u32 },
                                        },
                                        work_done_progress_params: WorkDoneProgressParams::default(),
                                        partial_result_params: PartialResultParams::default(),
                                        context: ReferenceContext { include_declaration: true },
                                    }
                                });
                                let _ = send_lsp_message(&mut stdin, &ref_req).await;
                            }
                        }
                        Some(LspInbound::Formatting { req_id }) => {
                            pending_requests.insert(req_id, "formatting");
                            let uri_str = file_to_uri(&current_file);
                            if let Ok(uri) = uri_str.parse::<Uri>() {
                                let fmt_req = serde_json::json!({
                                    "jsonrpc": "2.0",
                                    "id": req_id,
                                    "method": "textDocument/formatting",
                                    "params": DocumentFormattingParams {
                                        text_document: TextDocumentIdentifier { uri },
                                        options: lsp_types::FormattingOptions {
                                            tab_size: 4,
                                            insert_spaces: true,
                                            ..Default::default()
                                        },
                                        work_done_progress_params: WorkDoneProgressParams::default(),
                                    }
                                });
                                let _ = send_lsp_message(&mut stdin, &fmt_req).await;
                            }
                        }
                        Some(LspInbound::CodeAction { line, col, diagnostics, req_id }) => {
                            pending_requests.insert(req_id, "codeAction");
                            let uri_str = file_to_uri(&current_file);
                            if let Ok(uri) = uri_str.parse::<Uri>() {
                                let diag_list: Vec<Diagnostic> = diagnostics
                                    .into_iter()
                                    .map(|d| Diagnostic {
                                        range: Range {
                                            start: Position { line: d.line as u32, character: d.col as u32 },
                                            end: Position { line: d.end_line.max(d.line) as u32, character: d.end_col.max(d.col + 1) as u32 },
                                        },
                                        severity: Some(match d.severity {
                                            1 => DiagnosticSeverity::ERROR,
                                            2 => DiagnosticSeverity::WARNING,
                                            3 => DiagnosticSeverity::INFORMATION,
                                            _ => DiagnosticSeverity::HINT,
                                        }),
                                        message: d.message,
                                        ..Default::default()
                                    })
                                    .collect();

                                let ca_req = serde_json::json!({
                                    "jsonrpc": "2.0",
                                    "id": req_id,
                                    "method": "textDocument/codeAction",
                                    "params": CodeActionParams {
                                        text_document: TextDocumentIdentifier { uri },
                                        range: Range {
                                            start: Position { line: line as u32, character: col as u32 },
                                            end: Position { line: line as u32, character: col as u32 },
                                        },
                                        context: lsp_types::CodeActionContext {
                                            diagnostics: diag_list,
                                            only: None,
                                            trigger_kind: None,
                                        },
                                        work_done_progress_params: WorkDoneProgressParams::default(),
                                        partial_result_params: PartialResultParams::default(),
                                    }
                                });
                                let _ = send_lsp_message(&mut stdin, &ca_req).await;
                            }
                        }
                        Some(LspInbound::Rename { line, col, new_name, req_id }) => {
                            pending_requests.insert(req_id, "rename");
                            let uri_str = file_to_uri(&current_file);
                            if let Ok(uri) = uri_str.parse::<Uri>() {
                                let rn_req = serde_json::json!({
                                    "jsonrpc": "2.0",
                                    "id": req_id,
                                    "method": "textDocument/rename",
                                    "params": RenameParams {
                                        text_document_position: TextDocumentPositionParams {
                                            text_document: TextDocumentIdentifier { uri },
                                            position: Position { line: line as u32, character: col as u32 },
                                        },
                                        new_name,
                                        work_done_progress_params: WorkDoneProgressParams::default(),
                                    }
                                });
                                let _ = send_lsp_message(&mut stdin, &rn_req).await;
                            }
                        }
                        Some(LspInbound::DocumentSymbol { req_id }) => {
                            pending_requests.insert(req_id, "documentSymbol");
                            let uri_str = file_to_uri(&current_file);
                            if let Ok(uri) = uri_str.parse::<Uri>() {
                                let ds_req = serde_json::json!({
                                    "jsonrpc": "2.0",
                                    "id": req_id,
                                    "method": "textDocument/documentSymbol",
                                    "params": DocumentSymbolParams {
                                        text_document: TextDocumentIdentifier { uri },
                                        work_done_progress_params: WorkDoneProgressParams::default(),
                                        partial_result_params: PartialResultParams::default(),
                                    }
                                });
                                let _ = send_lsp_message(&mut stdin, &ds_req).await;
                            }
                        }
                        None => {
                            graceful_shutdown(&mut stdin, &mut stdout).await;
                            return;
                        }
                    }
                }
                msg = read_lsp_message(&mut stdout) => {
                    if let Ok(json) = msg {
                        if active_session != session_id {
                            continue;
                        }
                        if let Some(method) = json.get("method").and_then(Value::as_str) {
                            if method == "textDocument/publishDiagnostics" {
                                if let Some(params) = json.get("params").cloned()
                                    && let Ok((diag_uri, diags)) = parse_lsp_diagnostics(params) {
                                        let current_uri = file_to_uri(&current_file);
                                        if uris_match(&diag_uri, &current_uri) {
                                            let _ = tx.send(LspOutbound::Diagnostics(diags));
                                        }
                                    }
                            } else if let Some(req_id) = json.get("id") {
                                let resp = serde_json::json!({
                                    "jsonrpc": "2.0",
                                    "id": req_id,
                                    "result": if method == "workspace/configuration" {
                                        serde_json::json!([{}])
                                    } else if method == "workspace/workspaceFolders" {
                                        serde_json::json!([{ "uri": root_uri.as_str(), "name": "root" }])
                                    } else if method == "workspace/applyEdit" {
                                        serde_json::json!({ "applied": true })
                                    } else {
                                        Value::Null
                                    }
                                });
                                let _ = send_lsp_message(&mut stdin, &resp).await;
                            }
                        } else if let Some(resp_id) = json.get("id").and_then(Value::as_i64) {
                            let result_val = json.get("result").cloned().unwrap_or(Value::Null);

                            if let Some(req_type) = pending_requests.remove(&resp_id) {
                                match req_type {
                                    "completion" => {
                                        last_completion_req = None;
                                        let items = parse_lsp_completions(result_val);
                                        let _ = tx.send(LspOutbound::Completions { req_id: resp_id, items });
                                    }
                                    "hover" => {
                                        let hover = parse_lsp_hover(result_val);
                                        let _ = tx.send(LspOutbound::Hover { req_id: resp_id, hover });
                                    }
                                    "signatureHelp" => {
                                        let help = parse_lsp_signature_help(result_val);
                                        let _ = tx.send(LspOutbound::SignatureHelp { req_id: resp_id, help });
                                    }
                                    "definition" => {
                                        let locations = parse_lsp_locations(result_val);
                                        let _ = tx.send(LspOutbound::Definition { req_id: resp_id, locations });
                                    }
                                    "references" => {
                                        let locations = parse_lsp_locations(result_val);
                                        let _ = tx.send(LspOutbound::References { req_id: resp_id, locations });
                                    }
                                    "formatting" => {
                                        let edits = parse_lsp_formatting(result_val);
                                        let _ = tx.send(LspOutbound::Formatting { req_id: resp_id, edits });
                                    }
                                    "codeAction" => {
                                        let actions = parse_lsp_code_actions(result_val);
                                        let _ = tx.send(LspOutbound::CodeActions { req_id: resp_id, actions });
                                    }
                                    "rename" => {
                                        let edit: WorkspaceEdit = serde_json::from_value(result_val).unwrap_or_default();
                                        let changes = parse_lsp_workspace_edit(edit);
                                        let _ = tx.send(LspOutbound::Rename { req_id: resp_id, changes });
                                    }
                                    "documentSymbol" => {
                                        let symbols = parse_lsp_document_symbols(result_val);
                                        let _ = tx.send(LspOutbound::DocumentSymbols { req_id: resp_id, symbols });
                                    }
                                    "inlayHints" => {
                                        let hints = parse_lsp_inlay_hints(result_val);
                                        let _ = tx.send(LspOutbound::InlayHints { req_id: resp_id, hints });
                                    }
                                    "semanticTokens" => {
                                        let mut tokens = Vec::new();
                                        if let Ok(st_res) = serde_json::from_value::<SemanticTokensResult>(result_val)
                                            && let SemanticTokensResult::Tokens(st) = st_res {
                                                let mut cur_line = 0usize;
                                                let mut cur_char = 0usize;

                                                for token in &st.data {
                                                    let delta_line = token.delta_line as usize;
                                                    let delta_start = token.delta_start as usize;
                                                    let length = token.length as usize;
                                                    let token_type_idx = token.token_type as usize;

                                                    let token_name = server_legend.get(token_type_idx).map_or("", String::as_str);
                                                    let token_type = CanonicalTokenType::from_name(token_name);

                                                    if delta_line > 0 {
                                                        cur_line = cur_line.saturating_add(delta_line);
                                                        cur_char = delta_start;
                                                    } else {
                                                        cur_char = cur_char.saturating_add(delta_start);
                                                    }

                                                    tokens.push(SemanticTokenSpan {
                                                        line: cur_line,
                                                        start_col: cur_char,
                                                        length,
                                                        token_type,
                                                    });
                                                }
                                            }
                                        let _ = tx.send(LspOutbound::SemanticTokens { tokens });
                                    }
                                    _ => {}
                                }
                            }
                        }
                    } else {
                        child_crashed = true;
                        break 'event_loop;
                    }
                }
            }
        }

        if manual_reboot {
            graceful_shutdown(&mut stdin, &mut stdout).await;
            sleep(Duration::from_millis(150)).await;
            continue 'supervisor;
        }

        if child_crashed {
            restart_attempts += 1;
            if restart_attempts > 5 {
                let _ = tx.send(LspOutbound::Status(LspStatus::Error(format!(
                    "Server '{server_cmd}' crashed repeatedly. Reboot halted."
                ))));
                while let Some(inbound) = rx.recv().await {
                    if matches!(inbound, LspInbound::Restart) {
                        break;
                    }
                }
                restart_attempts = 0;
                continue 'supervisor;
            }

            let backoff = Duration::from_millis(400 * (1 << (restart_attempts - 1)));
            let _ = tx.send(LspOutbound::Status(LspStatus::Starting(format!(
                "{server_cmd} (restarting in {backoff:?}...)"
            ))));
            sleep(backoff).await;
        }
    }
}

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

/// Dynamic grammar facade maintaining clean compatibility with `cmd.rs`.
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
            "ts" | "tsx" | "mts" | "cts" => SupportedLanguage::TypeScript,
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
            SupportedLanguage::TypeScript => {
                Some(tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into())
            }
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
                (type (identifier) @type)
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
            SupportedLanguage::C | SupportedLanguage::Cpp => {
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
            SupportedLanguage::JavaScript | SupportedLanguage::TypeScript => {
                r#"
                (identifier) @variable
                (type_identifier) @type
                (property_identifier) @property
                (call_expression function: (identifier) @function)
                (call_expression function: (member_expression property: (property_identifier) @function))
                (function_declaration name: (identifier) @function)
                (method_definition name: (property_identifier) @function)
                [
                  "function" "const" "let" "var" "return" "if" "else" "switch" "case"
                  "default" "for" "while" "do" "break" "continue" "try" "catch" "finally"
                  "throw" "class" "extends" "import" "export" "from" "new"
                  "this" "super" "async" "await" "yield" "typeof" "instanceof" "void"
                  "type" "interface" "enum" "namespace" "declare" "abstract" "implements"
                ] @keyword
                (comment) @comment
                (string) @string
                (template_string) @string
                (number) @number
                (true) @number
                (false) @number
                (null) @keyword
                (undefined) @keyword
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
                (number) @number
                "#
            }
            SupportedLanguage::Json => {
                r#"
                (pair key: (string) @property)
                (string) @string
                (number) @number
                [ "true" "false" ] @number
                "null" @keyword
                "#
            }
            SupportedLanguage::Toml => {
                r"
                (table (bare_key) @type)
                (pair (bare_key) @property)
                (string) @string
                (integer) @number
                (float) @number
                (boolean) @number
                (comment) @comment
                "
            }
            SupportedLanguage::Yaml => {
                r"
                (block_mapping_pair key: (flow_node) @property)
                (string_scalar) @string
                (integer_scalar) @number
                (float_scalar) @number
                (boolean_scalar) @number
                (null_scalar) @keyword
                (comment) @comment
                "
            }
            SupportedLanguage::Html => {
                r"
                (tag_name) @tag
                (attribute_name) @property
                (attribute_value) @string
                (comment) @comment
                "
            }
            SupportedLanguage::Css => {
                r"
                (tag_name) @tag
                (class_name) @type
                (id_name) @type
                (property_name) @property
                (color_value) @number
                (integer_value) @number
                (float_value) @number
                (string_value) @string
                (comment) @comment
                "
            }
            SupportedLanguage::Markdown => {
                r"
                (atx_heading) @type
                (fenced_code_block) @string
                (link) @property
                "
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
                (floating_point_literal) @number
                (true) @number
                (false) @number
                (null_literal) @keyword
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
            SupportedLanguage::JavaScript | SupportedLanguage::TypeScript => &[
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

/// Themed Syntax Highlighting Engine using compiled AST queries via `tree-sitter-highlight`.
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

            if let Ok(mut config) = HighlightConfiguration::new(
                static_lang,
                language.grammar_name(),
                &query_src,
                "",
                "",
            ) {
                config.configure(HIGHLIGHT_NAMES);
                highlight_config = Some(config);
                has_grammar = true;
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
                        let end_line = match line_starts.binary_search(&end) {
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
                let char_start = utf16_to_char_col(line_text, tok.start_col);
                let char_end = utf16_to_char_col(line_text, tok.start_col + tok.length);
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
    let ext = path
        .and_then(|p| p.extension())
        .and_then(|e| e.to_str())
        .unwrap_or("");

    match ext {
        "rs" => (FILE_RUST, Color::Rgb(235, 102, 60)),
        "py" | "pyi" => (FILE_PYTHON, Color::Rgb(255, 212, 59)),
        "go" => (FILE_GO, Color::Rgb(80, 200, 240)),
        "zig" | "zon" => (FILE_ZIG, Color::Rgb(245, 160, 60)),
        "c" | "h" => (FILE_C, Color::Rgb(80, 140, 255)),
        "cpp" | "hpp" | "cc" | "cxx" => (FILE_CPP, Color::Rgb(80, 140, 255)),
        "js" | "jsx" | "mjs" | "cjs" => (FILE_JAVASCRIPT, Color::Rgb(245, 215, 75)),
        "ts" | "tsx" | "mts" | "cts" => (FILE_TYPESCRIPT, Color::Rgb(80, 160, 240)),
        "html" | "htm" => (FILE_HTML, Color::Rgb(240, 100, 60)),
        "css" | "scss" | "less" => (FILE_CSS, Color::Rgb(80, 160, 240)),
        "json" => (FILE_JSON, Color::Rgb(240, 200, 80)),
        "toml" => (FILE_TOML, Color::Rgb(160, 80, 50)),
        "yaml" | "yml" => (FILE_YAML, Color::Rgb(220, 100, 100)),
        "sh" | "bash" | "zsh" => (FILE_SHELL, Color::Rgb(100, 200, 140)),
        "lua" => (FILE_LUA, Color::Rgb(80, 140, 240)),
        "md" | "markdown" => (FILE_MARKDOWN, Color::Rgb(120, 180, 255)),
        "java" => (FILE_JAVA, Color::Rgb(240, 80, 80)),
        "cs" => (FILE_CSHARP, Color::Rgb(180, 120, 240)),
        "php" => (FILE_PHP, Color::Rgb(130, 140, 220)),
        "rb" | "rake" => (FILE_RUBY, Color::Rgb(220, 60, 60)),
        "kt" | "kts" => (FILE_KOTLIN, Color::Rgb(160, 100, 240)),
        "swift" => (FILE_SWIFT, Color::Rgb(240, 120, 60)),
        "dart" => (FILE_DART, Color::Rgb(80, 180, 240)),
        "sql" => (FILE_SQL, Color::Rgb(220, 140, 80)),
        "scala" | "sc" => (FILE_SCALA, Color::Rgb(220, 60, 60)),
        "odin" => (FILE_ODIN, Color::Rgb(80, 180, 220)),
        _ => (FILE_GENERIC, Color::Rgb(160, 165, 175)),
    }
}

pub fn completion_kind_icon(kind: u64) -> (&'static str, Color) {
    match kind {
        2 | 3 => (KIND_FUNCTION, Color::Rgb(80, 200, 240)),
        4 => (KIND_CLASS, Color::Rgb(240, 180, 70)),
        5 | 6 => (KIND_PROPERTY, Color::Rgb(250, 210, 90)),
        7 | 8 => (KIND_VARIABLE, Color::Rgb(120, 160, 255)),
        9 => (KIND_MODULE, Color::Rgb(140, 220, 120)),
        14 => (KIND_KEYWORD, Color::Rgb(220, 110, 240)),
        _ => (KIND_DEFAULT, Color::Rgb(170, 175, 190)),
    }
}

pub fn symbol_kind_icon(kind: u64) -> (&'static str, Color) {
    match kind {
        1 => (KIND_FILE, Color::Rgb(120, 160, 255)),      // File
        2 | 4 => (KIND_MODULE, Color::Rgb(220, 140, 80)), // Module, Package
        3 | 18 => (KIND_NAMESPACE, Color::Rgb(150, 166, 200)), // Namespace, Array
        5 | 10 | 23 => (KIND_CLASS, Color::Rgb(240, 180, 70)), // Class, Enum, Struct
        6 | 9 | 12 => (KIND_FUNCTION, Color::Rgb(80, 200, 240)), // Method, Constructor, Function
        7 | 8 => (KIND_PROPERTY, Color::Rgb(250, 210, 90)), // Property, Field
        11 => (KIND_CLASS, Color::Rgb(150, 166, 200)),    // Interface
        13 => (KIND_VARIABLE, Color::Rgb(228, 228, 228)), // Variable
        14 => (KIND_CONSTANT, Color::Rgb(255, 221, 51)),  // Constant
        15 => (KIND_STRING, Color::Rgb(115, 201, 54)),    // String
        16 => (KIND_NUMBER, Color::Rgb(149, 169, 159)),   // Number
        17 => (KIND_BOOLEAN, Color::Rgb(255, 221, 51)),   // Boolean
        25 => (KIND_OPERATOR, Color::Rgb(220, 110, 240)), // Operator
        26 => (KIND_TYPE_PARAM, Color::Rgb(149, 169, 159)), // TypeParameter
        _ => (KIND_DEFAULT, Color::Rgb(170, 175, 190)),
    }
}
