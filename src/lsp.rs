//! # Language Server Protocol (LSP) Client & Themed Syntax Engine
//!
//! This module implements language intelligence and code styling for `subject0`:
//!
//! 1. **LSP Background Actor (`run_lsp_actor`)**:
//!    Asynchronous, non-blocking Tokio task managing the language server child process
//!    over JSON-RPC 2.0 with HTTP-style `Content-Length` framing.
//!
//! 2. **Compile-Time Static Syntax Engine ([`SyntaxEngine`])**:
//!    Statically linked Tree-sitter parsers and built-in queries for primary languages
//!    (Rust, C, C++, Python, Go, JS, TS, Bash, JSON, TOML, YAML, HTML, CSS, Markdown, Java).
//!    All tokens are styled dynamically against the active [`Theme`].
//!
//! 3. **LSP Semantic Token Tier**:
//!    Compiler-accurate semantic token styling for languages without static AST parsers.

use anyhow::{Result, anyhow};
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
};

use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    process::{ChildStdin, Command as TokioCommand},
    sync::mpsc,
};
use tree_sitter::StreamingIterator;

use crate::theme::Theme;

// === LSP Types & Actor Protocol ===

/// Represents a single diagnostic entry emitted by an LSP server.
#[derive(Debug, Clone)]
pub struct DiagnosticItem {
    pub line: usize,
    pub col: usize,
    pub message: String,
    pub severity: u8,
}

/// An individual code completion candidate returned by the LSP server.
#[derive(Debug, Clone)]
pub struct SuggestionItem {
    pub label: String,
    pub insert_text: String,
    pub detail: Option<String>,
    pub kind: u64,
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
            "keyword" | "boolean" | "conditional" | "repeat" => Self::Keyword,
            "type" | "class" | "struct" | "enum" | "union" | "interface" | "typeParameter"
            | "builtinType" => Self::Type,
            "function" | "method" => Self::Function,
            "variable" => Self::Variable,
            "parameter" => Self::Parameter,
            "property" | "field" | "enumMember" => Self::Property,
            "string" | "character" => Self::String,
            "number" | "float" => Self::Number,
            "comment" | "documentation" => Self::Comment,
            "operator" => Self::Operator,
            "macro" | "attribute" => Self::Macro,
            "namespace" | "module" | "package" => Self::Namespace,
            "tag" => Self::Tag,
            _ => Self::Other,
        }
    }

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
pub enum LspInbound {
    Change {
        text: String,
        version: i64,
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
    OpenFile {
        path: PathBuf,
        text: String,
        lang_id: String,
    },
}

/// Outbound messages received from the background LSP actor.
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
    dirs::home_dir()
        .map(|h| h.join(".local/share/subject0"))
        .unwrap_or_else(|| PathBuf::from("./.subject0_data"))
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
    dirs::home_dir()
        .map(|h| h.join(".config/subject0"))
        .unwrap_or_else(|| PathBuf::from("./.subject0_cfg"))
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

/// Converts local path to an RFC 3986 `file://` URI string.
pub fn file_to_uri(path: &Path) -> String {
    let abs = if path.is_absolute() {
        path.to_path_buf()
    } else if let Ok(cwd) = env::current_dir() {
        cwd.join(path)
    } else {
        path.to_path_buf()
    };
    let s = abs.to_string_lossy();
    if cfg!(target_os = "windows") {
        let clean = s.replace('\\', "/");
        format!("file:///{}", clean.trim_start_matches('/'))
    } else {
        format!("file://{s}")
    }
}

fn uris_match(a: &str, b: &str) -> bool {
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

// === Asynchronous LSP Background Actor ===

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

    let _ = tx.send(LspOutbound::Status(LspStatus::Starting(server_cmd.clone())));

    let mut cmd_builder = TokioCommand::new(bin_path);
    cmd_builder.args(&args);
    cmd_builder.kill_on_drop(true);

    let mut child = match cmd_builder
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            let _ = tx.send(LspOutbound::Status(LspStatus::Error(e.to_string())));
            return;
        }
    };

    let mut stdin: ChildStdin = child.stdin.take().expect("Child stdin acquired");
    let mut stdout = BufReader::new(child.stdout.take().expect("Child stdout acquired"));

    let root_uri = match env::current_dir() {
        Ok(d) => file_to_uri(&d),
        Err(_) => "file://.".to_string(),
    };
    let mut current_file_uri = file_to_uri(&initial_file);
    let mut pending_requests: HashMap<i64, &'static str> = HashMap::new();

    let root_path =
        env::current_dir().map_or_else(|_| ".".to_string(), |p| p.to_string_lossy().to_string());

    let init_req = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "processId": std::process::id(),
            "rootPath": root_path,
            "rootUri": root_uri,
            "workspaceFolders": [{ "uri": root_uri, "name": "root" }],
            "capabilities": {
                "workspace": { "workspaceFolders": true, "configuration": true },
                "textDocument": {
                    "synchronization": {
                        "openClose": true,
                        "change": 1,
                        "save": { "includeText": false }
                    },
                    "completion": {
                        "completionItem": {
                            "snippetSupport": true,
                            "commitCharactersSupport": true,
                            "documentationFormat": ["plaintext", "markdown"]
                        }
                    },
                    "publishDiagnostics": { "relatedInformation": true },
                    "semanticTokens": {
                        "requests": { "full": true },
                        "tokenTypes": [
                            "namespace", "type", "class", "enum", "interface",
                            "struct", "typeParameter", "parameter", "variable",
                            "property", "enumMember", "function", "method",
                            "macro", "keyword", "comment", "string", "number", "operator"
                        ],
                        "tokenModifiers": ["declaration", "definition", "readonly", "static", "defaultLibrary"],
                        "formats": ["relative"]
                    }
                }
            },
            "initializationOptions": { "checkOnSave": true }
        }
    });

    if send_lsp_message(&mut stdin, &init_req).await.is_err() {
        let _ = tx.send(LspOutbound::Status(LspStatus::Error(
            "Init handshake write failed".into(),
        )));
        return;
    }

    let mut server_legend: Vec<String> = Vec::new();
    let init_timeout = tokio::time::sleep(tokio::time::Duration::from_secs(30));
    tokio::pin!(init_timeout);

    loop {
        tokio::select! {
            _ = &mut init_timeout => {
                let _ = tx.send(LspOutbound::Status(LspStatus::Error(
                    "LSP initialization timed out after 30 seconds".into(),
                )));
                return;
            }
            inbound = rx.recv() => {
                if inbound.is_none() {
                    return;
                }
            }
            msg = read_lsp_message(&mut stdout) => {
                match msg {
                    Ok(val) => {
                        if let Some(method) = val.get("method").and_then(Value::as_str) {
                            if let Some(req_id) = val.get("id") {
                                let resp = serde_json::json!({
                                    "jsonrpc": "2.0",
                                    "id": req_id,
                                    "result": if method == "workspace/configuration" {
                                        serde_json::json!([])
                                    } else {
                                        Value::Null
                                    }
                                });
                                let _ = send_lsp_message(&mut stdin, &resp).await;
                            }
                        } else if val.get("id").and_then(Value::as_i64) == Some(1) {
                            if let Some(types_arr) = val
                                .get("result")
                                .and_then(|r| r.get("capabilities"))
                                .and_then(|c| c.get("semanticTokensProvider"))
                                .and_then(|p| p.get("legend"))
                                .and_then(|l| l.get("tokenTypes"))
                                .and_then(Value::as_array)
                            {
                                server_legend = types_arr
                                    .iter()
                                    .filter_map(Value::as_str)
                                    .map(ToString::to_string)
                                    .collect();
                            }
                            break;
                        }
                    }
                    Err(e) => {
                        let _ = tx.send(LspOutbound::Status(LspStatus::Error(
                            format!("Init failed: {e}"),
                        )));
                        return;
                    }
                }
            }
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
        "params": {}
    });
    let _ = send_lsp_message(&mut stdin, &initialized).await;

    let did_open = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "textDocument/didOpen",
        "params": {
            "textDocument": {
                "uri": current_file_uri,
                "languageId": initial_lang,
                "version": 1,
                "text": initial_text
            }
        }
    });
    let _ = send_lsp_message(&mut stdin, &did_open).await;
    let _ = tx.send(LspOutbound::Status(LspStatus::Ready(server_cmd)));

    loop {
        tokio::select! {
            cmd = rx.recv() => {
                match cmd {
                    Some(LspInbound::Change { text, version }) => {
                        let did_change = serde_json::json!({
                            "jsonrpc": "2.0",
                            "method": "textDocument/didChange",
                            "params": {
                                "textDocument": { "uri": current_file_uri, "version": version },
                                "contentChanges": [{ "text": text }]
                            }
                        });
                        let _ = send_lsp_message(&mut stdin, &did_change).await;
                    }
                    Some(LspInbound::Save) => {
                        let did_save = serde_json::json!({
                            "jsonrpc": "2.0",
                            "method": "textDocument/didSave",
                            "params": { "textDocument": { "uri": current_file_uri } }
                        });
                        let _ = send_lsp_message(&mut stdin, &did_save).await;
                    }
                    Some(LspInbound::Completion { line, col, req_id }) => {
                        pending_requests.insert(req_id, "completion");
                        let comp_req = serde_json::json!({
                            "jsonrpc": "2.0",
                            "id": req_id,
                            "method": "textDocument/completion",
                            "params": {
                                "textDocument": { "uri": current_file_uri },
                                "position": { "line": line, "character": col }
                            }
                        });
                        let _ = send_lsp_message(&mut stdin, &comp_req).await;
                    }
                    Some(LspInbound::SemanticTokens { req_id }) => {
                        pending_requests.insert(req_id, "semanticTokens");
                        let st_req = serde_json::json!({
                            "jsonrpc": "2.0",
                            "id": req_id,
                            "method": "textDocument/semanticTokens/full",
                            "params": { "textDocument": { "uri": current_file_uri } }
                        });
                        let _ = send_lsp_message(&mut stdin, &st_req).await;
                    }
                    Some(LspInbound::OpenFile { path, text, lang_id }) => {
                        let new_uri = file_to_uri(&path);
                        if new_uri != current_file_uri {
                            let did_close = serde_json::json!({
                                "jsonrpc": "2.0",
                                "method": "textDocument/didClose",
                                "params": { "textDocument": { "uri": current_file_uri } }
                            });
                            let _ = send_lsp_message(&mut stdin, &did_close).await;
                            current_file_uri = new_uri;
                        }
                        let open_req = serde_json::json!({
                            "jsonrpc": "2.0",
                            "method": "textDocument/didOpen",
                            "params": {
                                "textDocument": {
                                    "uri": current_file_uri,
                                    "languageId": lang_id,
                                    "version": 1,
                                    "text": text
                                }
                            }
                        });
                        let _ = send_lsp_message(&mut stdin, &open_req).await;
                    }
                    None => {
                        let shutdown_req = serde_json::json!({
                            "jsonrpc": "2.0",
                            "id": 999_999,
                            "method": "shutdown",
                            "params": null
                        });
                        if send_lsp_message(&mut stdin, &shutdown_req).await.is_ok() {
                            let shutdown_timeout = tokio::time::sleep(tokio::time::Duration::from_millis(500));
                            tokio::pin!(shutdown_timeout);
                            loop {
                                tokio::select! {
                                    _ = &mut shutdown_timeout => break,
                                    msg = read_lsp_message(&mut stdout) => {
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
                                "params": null
                            });
                            let _ = send_lsp_message(&mut stdin, &exit_notif).await;
                        }
                        break;
                    }
                }
            }
            msg = read_lsp_message(&mut stdout) => {
                if let Ok(json) = msg {
                    if let Some(method) = json.get("method").and_then(Value::as_str) {
                        if method == "textDocument/publishDiagnostics" {
                            if let Some(params) = json.get("params") {
                                let diag_uri = params.get("uri").and_then(|u| u.as_str()).unwrap_or("");
                                if uris_match(diag_uri, &current_file_uri) {
                                    let mut items = Vec::new();
                                    if let Some(diag_array) = params.get("diagnostics").and_then(Value::as_array) {
                                        for d in diag_array {
                                            let line = d["range"]["start"]["line"].as_u64().unwrap_or(0) as usize;
                                            let col = d["range"]["start"]["character"].as_u64().unwrap_or(0) as usize;
                                            let message = d["message"].as_str().unwrap_or("").to_string();
                                            let severity = d["severity"].as_u64().unwrap_or(1) as u8;
                                            items.push(DiagnosticItem { line, col, message, severity });
                                        }
                                    }
                                    let _ = tx.send(LspOutbound::Diagnostics(items));
                                }
                            }
                        } else if let Some(req_id) = json.get("id") {
                            let resp = serde_json::json!({
                                "jsonrpc": "2.0",
                                "id": req_id,
                                "result": if method == "workspace/configuration" {
                                    serde_json::json!([])
                                } else if method == "workspace/workspaceFolders" {
                                    serde_json::json!([{ "uri": root_uri, "name": "root" }])
                                } else {
                                    Value::Null
                                }
                            });
                            let _ = send_lsp_message(&mut stdin, &resp).await;
                        }
                    } else if let Some(resp_id) = json.get("id").and_then(Value::as_i64) {
                        let result_val = json.get("result");

                        if let Some(req_type) = pending_requests.remove(&resp_id) {
                            match req_type {
                                "semanticTokens" => {
                                    let mut tokens = Vec::new();
                                    if let Some(data) = result_val
                                        .and_then(|r| r.get("data"))
                                        .and_then(Value::as_array)
                                    {
                                        let ints: Vec<usize> = data
                                            .iter()
                                            .filter_map(Value::as_u64)
                                            .map(|v| v as usize)
                                            .collect();
                                        let mut cur_line = 0usize;
                                        let mut cur_char = 0usize;

                                        for chunk in ints.chunks_exact(5) {
                                            let delta_line = chunk[0];
                                            let delta_start = chunk[1];
                                            let length = chunk[2];
                                            let token_type_idx = chunk[3];

                                            let token_name = server_legend
                                                .get(token_type_idx)
                                                .map_or("", String::as_str);
                                            let token_type =
                                                CanonicalTokenType::from_name(token_name);

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
                                "completion" => {
                                    let mut results = Vec::new();
                                    let items_array = result_val.and_then(|r| {
                                        if r.is_array() {
                                            r.as_array()
                                        } else {
                                            r.get("items").and_then(Value::as_array)
                                        }
                                    });

                                    if let Some(arr) = items_array {
                                        for item in arr {
                                            if let Some(label) =
                                                item.get("label").and_then(Value::as_str)
                                            {
                                                let raw_insert_text = if let Some(it) = item
                                                    .get("insertText")
                                                    .and_then(Value::as_str)
                                                {
                                                    it.to_string()
                                                } else if let Some(te) = item.get("textEdit") {
                                                    te.get("newText")
                                                        .and_then(Value::as_str)
                                                        .unwrap_or(label)
                                                        .to_string()
                                                } else {
                                                    label.to_string()
                                                };

                                                let insert_format = item
                                                    .get("insertTextFormat")
                                                    .and_then(Value::as_u64)
                                                    .unwrap_or(1);

                                                let insert_text = if insert_format == 2 {
                                                    parse_snippet_to_plain_text(&raw_insert_text)
                                                } else {
                                                    raw_insert_text
                                                };

                                                let detail = item
                                                    .get("detail")
                                                    .and_then(Value::as_str)
                                                    .map(ToString::to_string);
                                                let kind = item
                                                    .get("kind")
                                                    .and_then(Value::as_u64)
                                                    .unwrap_or(0);

                                                results.push(SuggestionItem {
                                                    label: label.to_string(),
                                                    insert_text,
                                                    detail,
                                                    kind,
                                                });
                                            }
                                        }
                                    }
                                    let _ = tx.send(LspOutbound::Completions {
                                        req_id: resp_id,
                                        items: results,
                                    });
                                }
                                _ => {}
                            }
                        }
                    }
                } else {
                    let _ = tx.send(LspOutbound::Status(LspStatus::Error("Terminated".into())));
                    break;
                }
            }
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

        if let Some(lang) = sl {
            if lang.static_language().is_some() {
                return Ok(PathBuf::from("built-in"));
            }
        }

        Err(anyhow!(
            "Language '{lang_name}' is not in the compiled-in static grammar set.\n\
             All primary languages (Rust, C, C++, Python, Go, JS, TS, Bash, JSON, TOML, YAML, HTML, CSS, Markdown, Java)\n\
             are built-in with zero runtime overhead. LSP semantic highlighting is used for all other files."
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
    VHDL,
    Zephyr,
    Janet,
    Wren,
    Vala,
    Gd,
    Plain,
}

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
            "vhd" | "vhdl" => SupportedLanguage::VHDL,
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

    /// Built-in highlight queries guaranteeing instant syntax highlighting out of the box.
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
                r#"
                (table (bare_key) @type)
                (pair (bare_key) @property)
                (string) @string
                (integer) @number
                (float) @number
                (boolean) @number
                (comment) @comment
                "#
            }
            SupportedLanguage::Yaml => {
                r#"
                (block_mapping_pair key: (flow_node) @property)
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
                (fenced_code_block) @string
                (link) @property
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
            SupportedLanguage::VHDL => "VHDL",
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
            SupportedLanguage::Bash => "bash",
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
            SupportedLanguage::Terraform => "hcl",
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
            SupportedLanguage::Zsh => "bash",
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
            SupportedLanguage::Hcl => "hcl",
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
            SupportedLanguage::VHDL => "vhdl",
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
            SupportedLanguage::VHDL => "vhdl",
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
            SupportedLanguage::Verilog | SupportedLanguage::VHDL => &["svlangserver", "vhdl_ls"],
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
            SupportedLanguage::VHDL,
            SupportedLanguage::Zephyr,
            SupportedLanguage::Janet,
            SupportedLanguage::Wren,
            SupportedLanguage::Vala,
            SupportedLanguage::Gd,
        ]
    }
}

// === Themed Syntax Highlighting Engine ===

/// High-speed syntax highlighting engine utilizing compiled AST grammars,
/// dynamic query resolution, and LSP semantic overlays styled against the active theme.
pub struct SyntaxEngine {
    pub language: SupportedLanguage,
    pub parser: tree_sitter::Parser,
    pub tree: Option<tree_sitter::Tree>,
    pub query: Option<tree_sitter::Query>,
    pub ts_tokens: HashMap<usize, Vec<SemanticTokenSpan>>,
    pub semantic_tokens: HashMap<usize, Vec<SemanticTokenSpan>>,
    pub has_grammar: bool,
}

impl SyntaxEngine {
    pub fn new(path: Option<&PathBuf>) -> Self {
        let language = SupportedLanguage::from_path(path);
        let mut parser = tree_sitter::Parser::new();
        let mut query = None;
        let mut has_grammar = false;

        if let Some(static_lang) = language.static_language() {
            if parser.set_language(&static_lang).is_ok() {
                let query_src = if let Some(qp) = query_file_path(language.grammar_name()) {
                    fs::read_to_string(qp)
                        .unwrap_or_else(|_| language.builtin_highlight_query().to_string())
                } else {
                    language.builtin_highlight_query().to_string()
                };

                if let Ok(q) = tree_sitter::Query::new(&static_lang, &query_src) {
                    query = Some(q);
                    has_grammar = true;
                }
            }
        }

        Self {
            language,
            parser,
            tree: None,
            query,
            ts_tokens: HashMap::new(),
            semantic_tokens: HashMap::new(),
            has_grammar,
        }
    }

    pub fn has_treesitter(&self) -> bool {
        self.has_grammar && self.query.is_some()
    }

    /// Reparses the document without stale node reuse to eliminate tree misalignment issues.
    pub fn reparse(&mut self, text: &str) {
        if !self.has_grammar {
            return;
        }

        let Some(query) = &self.query else {
            return;
        };

        self.tree = self.parser.parse(text, None);
        self.ts_tokens.clear();

        let Some(tree) = &self.tree else {
            return;
        };

        let mut cursor = tree_sitter::QueryCursor::new();
        let text_bytes = text.as_bytes();
        let lines: Vec<&str> = text.split('\n').collect();

        let byte_col_to_char_col = |line_str: &str, byte_col: usize| -> usize {
            let clean = line_str.strip_suffix('\r').unwrap_or(line_str);
            let mut boundary = byte_col.min(clean.len());
            while boundary > 0 && !clean.is_char_boundary(boundary) {
                boundary -= 1;
            }
            clean[..boundary].chars().count()
        };

        let mut matches = cursor.matches(query, tree.root_node(), text_bytes);
        while let Some(m) = matches.next() {
            for capture in m.captures {
                let node = capture.node;
                let capture_names = query.capture_names();
                let capture_idx = capture.index as usize;
                if capture_idx >= capture_names.len() {
                    continue;
                }
                let capture_name = capture_names[capture_idx];
                let token_type = CanonicalTokenType::from_query_capture(capture_name);

                if token_type == CanonicalTokenType::Other {
                    continue;
                }

                let start_pos = node.start_position();
                let end_pos = node.end_position();

                if start_pos.row == end_pos.row {
                    let row = start_pos.row;
                    if let Some(line_str) = lines.get(row) {
                        let c_start = byte_col_to_char_col(line_str, start_pos.column);
                        let c_end = byte_col_to_char_col(line_str, end_pos.column);
                        let len = c_end.saturating_sub(c_start);
                        if len > 0 {
                            self.ts_tokens
                                .entry(row)
                                .or_default()
                                .push(SemanticTokenSpan {
                                    line: row,
                                    start_col: c_start,
                                    length: len,
                                    token_type,
                                });
                        }
                    }
                } else if matches!(
                    token_type,
                    CanonicalTokenType::Comment | CanonicalTokenType::String
                ) {
                    for row in start_pos.row..=end_pos.row {
                        if let Some(line_str) = lines.get(row) {
                            let clean = line_str.strip_suffix('\r').unwrap_or(line_str);
                            let byte_start = if row == start_pos.row {
                                start_pos.column
                            } else {
                                0
                            };
                            let byte_end = if row == end_pos.row {
                                end_pos.column
                            } else {
                                clean.len()
                            };

                            let c_start = byte_col_to_char_col(clean, byte_start);
                            let c_end = byte_col_to_char_col(clean, byte_end);
                            let len = c_end.saturating_sub(c_start);
                            if len > 0 {
                                self.ts_tokens
                                    .entry(row)
                                    .or_default()
                                    .push(SemanticTokenSpan {
                                        line: row,
                                        start_col: c_start,
                                        length: len,
                                        token_type,
                                    });
                            }
                        }
                    }
                }
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

// === UI Helpers ===

pub fn file_icon_and_color(path: Option<&PathBuf>) -> (&'static str, Color) {
    let ext = path
        .and_then(|p| p.extension())
        .and_then(|e| e.to_str())
        .unwrap_or("");

    match ext {
        "rs" => ("", Color::Rgb(235, 102, 60)),
        "py" | "pyi" => ("", Color::Rgb(255, 212, 59)),
        "go" => ("", Color::Rgb(80, 200, 240)),
        "zig" | "zon" => ("", Color::Rgb(245, 160, 60)),
        "c" | "h" => ("", Color::Rgb(80, 140, 255)),
        "cpp" | "hpp" | "cc" | "cxx" => ("", Color::Rgb(80, 140, 255)),
        "js" | "jsx" | "mjs" | "cjs" => ("", Color::Rgb(245, 215, 75)),
        "ts" | "tsx" | "mts" | "cts" => ("", Color::Rgb(80, 160, 240)),
        "html" | "htm" => ("", Color::Rgb(240, 100, 60)),
        "css" | "scss" | "less" => ("", Color::Rgb(80, 160, 240)),
        "json" => ("", Color::Rgb(240, 200, 80)),
        "toml" => ("", Color::Rgb(160, 80, 50)),
        "yaml" | "yml" => ("", Color::Rgb(220, 100, 100)),
        "sh" | "bash" | "zsh" => ("", Color::Rgb(100, 200, 140)),
        "lua" => ("", Color::Rgb(80, 140, 240)),
        "md" | "markdown" => ("", Color::Rgb(120, 180, 255)),
        "java" => ("", Color::Rgb(240, 80, 80)),
        "cs" => ("󰌛", Color::Rgb(180, 120, 240)),
        "php" => ("", Color::Rgb(130, 140, 220)),
        "rb" | "rake" => ("", Color::Rgb(220, 60, 60)),
        "kt" | "kts" => ("", Color::Rgb(160, 100, 240)),
        "swift" => ("", Color::Rgb(240, 120, 60)),
        "dart" => ("", Color::Rgb(80, 180, 240)),
        "sql" => ("", Color::Rgb(220, 140, 80)),
        "scala" | "sc" => ("", Color::Rgb(220, 60, 60)),
        "odin" => ("", Color::Rgb(80, 180, 220)),
        _ => ("󰈔", Color::Rgb(160, 165, 175)),
    }
}

pub fn completion_kind_icon(kind: u64) -> (&'static str, Color) {
    match kind {
        2 | 3 => ("󰊕", Color::Rgb(80, 200, 240)),
        4 => ("󰌗", Color::Rgb(240, 180, 70)),
        5 | 6 => ("󰫧", Color::Rgb(250, 210, 90)),
        7 | 8 => ("󱡠", Color::Rgb(120, 160, 255)),
        9 => ("󰏗", Color::Rgb(140, 220, 120)),
        14 => ("󰌆", Color::Rgb(220, 110, 240)),
        _ => ("󰈚", Color::Rgb(170, 175, 190)),
    }
}
