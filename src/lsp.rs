//! # Language Server Protocol (LSP) Client & Syntax Highlighting Subsystem
//!
//! This module implements the language intelligence and code appearance layers for the
//! `subject0` editor. It is split into two primary components:
//!
//! 1. **LSP Background Actor (`run_lsp_actor`)**:
//!    An asynchronous, non-blocking Tokio task that manages the lifecycle of a language server
//!    process (e.g., `rust-analyzer`, `pyright`). It communicates with the server over standard
//!    I/O using JSON-RPC 2.0 framed with HTTP-style `Content-Length` headers, conforming to the
//!    Language Server Protocol specification. Communication between the editor UI event loop
//!    and this background actor occurs over unbounded Tokio MPSC channels via [`LspInbound`]
//!    and [`LspOutbound`] messages.
//!
//! 2. **Syntax Highlighting Engine ([`SyntaxEngine`])**:
//!    A token-level lexical analyzer and tree-sitter parser wrapper that transforms raw buffer
//!    slices into styled Ratatui [`Span`] sequences for terminal rendering. It provides
//!    per-language keyword, literal, comment, and identifier highlighting for Rust, Python,
//!    and Markdown, as well as glyph and color resolution for file trees and completion menus.

use anyhow::{Result, anyhow};
use ratatui::{
    style::{Color, Modifier, Style},
    text::Span,
};
use serde_json::Value;
use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Stdio,
};

use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    process::{ChildStdin, Command as TokioCommand},
    sync::mpsc,
};

// === LSP Types & Actor Protocol ===

/// Represents a single diagnostic entry emitted by an LSP server.
///
/// Diagnostics report errors, warnings, lints, and hints detected by background
/// language servers (e.g., compiler diagnostic output from `rust-analyzer`).
#[derive(Debug, Clone)]
pub struct DiagnosticItem {
    /// Zero-based line number within the buffer where the diagnostic begins.
    pub line: usize,
    /// Zero-based character/column offset within the line where the diagnostic begins.
    #[allow(dead_code)]
    pub col: usize,
    /// Human-readable diagnostic description or compiler error message.
    pub message: String,
    /// LSP severity indicator:
    /// - `1`: Error
    /// - `2`: Warning
    /// - `3`: Information
    /// - `4`: Hint
    pub severity: u8,
}

/// An individual code completion candidate returned by the LSP server.
///
/// Corresponds to the LSP `CompletionItem` interface, carrying display labels,
/// insertion text, and categorization metadata used to render popup completion menus.
#[derive(Debug, Clone)]
pub struct SuggestionItem {
    /// The primary label displayed in the completion popup menu (e.g., method name).
    pub label: String,
    /// The exact text that should be placed into the buffer if this item is accepted.
    /// Defaults to [`Self::label`] when the server does not specify an explicit `insertText`.
    pub insert_text: String,
    /// Optional auxiliary information (such as function signature or containing module).
    #[allow(dead_code)]
    pub detail: Option<String>,
    /// Numeric LSP `CompletionItemKind` discriminant (e.g., `2` for Method, `3` for Function).
    /// Used by [`completion_kind_icon`] to render appropriate UI glyphs.
    pub kind: u64,
}

/// Represents the current operational lifecycle state of the LSP server process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LspStatus {
    /// The LSP feature is disabled or not configured for the active buffer.
    Disabled,
    /// The specified language server binary executable could not be resolved on the host.
    NotFound(String),
    /// The server binary was located and spawned; the initialization handshake is underway.
    Starting(String),
    /// The initialization handshake is complete, and the server is ready to handle requests.
    Ready(String),
    /// An unrecoverable I/O or protocol error occurred, or the process terminated abnormally.
    Error(String),
}

/// Canonical semantic classification mapping server legends into unified editor colors.
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
            _ => Self::Other,
        }
    }

    /// Maps standard Tree-sitter query capture names into unified theme tokens.
    pub fn from_query_capture(capture_name: &str) -> Self {
        if capture_name.starts_with("keyword")
            || capture_name.starts_with("repeat")
            || capture_name.starts_with("conditional")
            || capture_name.starts_with("include")
        {
            Self::Keyword
        } else if capture_name.starts_with("type")
            || capture_name.starts_with("structure")
            || capture_name.starts_with("class")
            || capture_name.starts_with("storageclass")
        {
            Self::Type
        } else if capture_name.starts_with("function")
            || capture_name.starts_with("method")
            || capture_name.starts_with("constructor")
        {
            Self::Function
        } else if capture_name.starts_with("variable.parameter") || capture_name == "parameter" {
            Self::Parameter
        } else if capture_name.starts_with("variable") {
            Self::Variable
        } else if capture_name.starts_with("property") || capture_name.starts_with("field") {
            Self::Property
        } else if capture_name.starts_with("string") || capture_name.starts_with("character") {
            Self::String
        } else if capture_name.starts_with("number")
            || capture_name.starts_with("float")
            || capture_name.starts_with("boolean")
        {
            Self::Number
        } else if capture_name.starts_with("comment") {
            Self::Comment
        } else if capture_name.starts_with("operator") {
            Self::Operator
        } else if capture_name.starts_with("macro") || capture_name.starts_with("attribute") {
            Self::Macro
        } else if capture_name.starts_with("module") || capture_name.starts_with("namespace") {
            Self::Namespace
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

/// Commands and notifications routed from the editor frontend into the background LSP actor.
pub enum LspInbound {
    /// Broadcasts an edit notification (`textDocument/didChange`) to synchronize document state.
    /// Uses full-document synchronization (`TextDocumentSyncKind::Full = 1`).
    Change {
        /// Full snapshot of the updated document buffer.
        text: String,
        /// Monotonically increasing document version identifier.
        version: i64,
    },
    /// Informs the language server that the active file has been persisted to disk (`textDocument/didSave`).
    Save,
    /// Requests autocomplete items at a given buffer position (`textDocument/completion`).
    Completion {
        /// Zero-based line number of the cursor.
        line: usize,
        /// Zero-based UTF-16 character/column index of the cursor.
        col: usize,
        /// Correlation identifier used to pair the asynchronous response with this request.
        req_id: i64,
    },
    /// Requests semantic highlighting tokens (`textDocument/semanticTokens/full`).
    SemanticTokens {
        /// Correlation identifier used to pair the asynchronous response with this request.
        req_id: i64,
    },
    /// Switches the active document context or notifies the server of an opened file (`textDocument/didOpen`).
    OpenFile {
        /// Absolute or relative path to the newly opened file.
        path: PathBuf,
        /// Complete textual contents of the file at the moment of opening.
        text: String,
        /// LSP language identifier string (e.g., `"rust"`, `"python"`).
        lang_id: String,
    },
}

/// Notifications and response payloads routed from the background LSP actor to the editor frontend.
pub enum LspOutbound {
    /// An update regarding the child process status or lifecycle transition.
    Status(LspStatus),
    /// Set of diagnostics published by the server (`textDocument/publishDiagnostics`).
    Diagnostics(Vec<DiagnosticItem>),
    /// Completion suggestions matching a prior [`LspInbound::Completion`] request.
    Completions {
        /// Request correlation ID matching the ID provided during the request dispatch.
        req_id: i64,
        /// List of completion candidates parsed from the server's response.
        items: Vec<SuggestionItem>,
    },
    /// Semantic tokens decoded from `textDocument/semanticTokens/full`.
    SemanticTokens { tokens: Vec<SemanticTokenSpan> },
}

/// Searches the host system to resolve the absolute path to an executable binary.
///
/// # Search Order
/// 1. Each directory entry listed in the system `$PATH` environment variable.
/// 2. `$HOME/.cargo/bin/<cmd>` (standard location for Rust toolchain binaries like `rust-analyzer`).
///
/// Returns `Some(PathBuf)` if an existing file matches `cmd`, or `None` if resolution fails.
pub fn resolve_binary_path(cmd: &str) -> Option<PathBuf> {
    if let Ok(path_var) = env::var("PATH") {
        for dir in env::split_paths(&path_var) {
            let candidate = dir.join(cmd);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    if let Ok(home) = env::var("HOME") {
        let cargo_bin = PathBuf::from(home).join(".cargo/bin").join(cmd);
        if cargo_bin.is_file() {
            return Some(cargo_bin);
        }
    }
    None
}

/// Converts a local filesystem path into an RFC 3986 compliant `file://` URI string.
///
/// If `path` is relative, it is resolved against the current working directory before
/// constructing the URI scheme to ensure language servers receive canonical paths.
pub fn file_to_uri(path: &Path) -> String {
    let abs = if path.is_absolute() {
        path.to_path_buf()
    } else if let Ok(cwd) = env::current_dir() {
        cwd.join(path)
    } else {
        path.to_path_buf()
    };
    format!("file://{}", abs.to_string_lossy())
}

/// Reads a single framed JSON-RPC message from an asynchronous buffered LSP stream.
///
/// # Wire Framing Protocol
/// Conforms to the LSP Base Protocol framing:
/// ```text
/// Content-Length: <byte_count>\r\n
/// \r\n
/// <raw_json_payload>
/// ```
///
/// Continues parsing header fields until an empty line (`\r\n`) is encountered,
/// reads exactly `Content-Length` bytes from the stream, and parses the slice as a
/// [`serde_json::Value`].
///
/// # Errors
/// Returns an error if the underlying stream closes (EOF), if the `Content-Length`
/// header is absent, or if the payload contains invalid JSON.
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
        if let Some(val) = trimmed.strip_prefix("Content-Length:") {
            content_length = val.trim().parse::<usize>()?;
        }
    }

    if content_length == 0 {
        return Err(anyhow!("Missing Content-Length header"));
    }

    let mut body = vec![0u8; content_length];
    reader.read_exact(&mut body).await?;
    let val: Value = serde_json::from_slice(&body)?;
    Ok(val)
}

/// Serializes a JSON payload and transmits it over an asynchronous stream with LSP framing headers.
///
/// Generates the standard `Content-Length: <len>\r\n\r\n` prefix, writes the payload bytes,
/// and flushes the destination writer immediately.
///
/// # Errors
/// Returns an error if JSON serialization fails or an I/O write error occurs on `writer`.
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

/// Resolves the actual binary name and standard arguments for an LSP command.
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
        | "intelephense" => (cmd.to_string(), vec!["--stdio"]),
        "taplo" => ("taplo".to_string(), vec!["lsp", "stdio"]),
        "dart" => ("dart".to_string(), vec!["language-server"]),
        "omnisharp" => ("omnisharp".to_string(), vec!["-lsp"]),
        _ => (cmd.to_string(), vec![]),
    }
}

/// Background actor responsible for orchestrating an LSP server process session.
///
/// # Lifecycle Stages
/// 1. **Binary Discovery**: Resolves the executable path using [`resolve_binary_path`].
/// 2. **Process Spawning**: Spawns the child process with piped standard I/O handles.
/// 3. **Handshake**:
///    - Dispatches the `initialize` request (request ID `1`) configured with client capabilities
///      (full sync, completion, diagnostics) and current root URI.
///    - Awaits the response with matching ID `1`.
///    - Sends the `initialized` notification.
/// 4. **Document Ingestion**: Sends an initial `textDocument/didOpen` notification with `initial_text`.
/// 5. **Event Multiplexing**: Enters a bi-directional `tokio::select!` event loop:
///    - **Inbound (`rx`)**: Processes changes, saves, completions, and file open events,
///      serializing them to server stdin.
///    - **Outbound (`stdout`)**: Consumes incoming JSON-RPC notifications and responses,
///      extracting diagnostics and completion responses and routing them over `tx`.
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

    let root_path =
        env::current_dir().map_or_else(|_| ".".to_string(), |p| p.to_string_lossy().to_string());

    // Construct the standard LSP initialization payload announcing client capabilities.
    let init_req = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "processId": std::process::id(),
            "rootPath": root_path,
            "rootUri": root_uri,
            "workspaceFolders": [
                {
                    "uri": root_uri,
                    "name": "root"
                }
            ],
            "capabilities": {
                "workspace": {
                    "workspaceFolders": true
                },
                "textDocument": {
                    "synchronization": {
                        "openClose": true,
                        "change": 1,
                        "save": { "includeText": false }
                    },
                                        "completion": {
                        "completionItem": {
                            "snippetSupport": false,
                            "commitCharactersSupport": true,
                            "documentationFormat": ["plaintext", "markdown"]
                        }
                    },
                    "publishDiagnostics": {
                        "relatedInformation": true
                    },
                    "semanticTokens": {
                        "requests": { "full": true },
                        "tokenTypes": [
                            "namespace", "type", "class", "enum", "interface",
                            "struct", "typeParameter", "parameter", "variable",
                            "property", "enumMember", "function", "method",
                            "macro", "keyword", "comment", "string", "number", "operator"
                        ],
                        "tokenModifiers": [
                            "declaration", "definition", "readonly", "static", "defaultLibrary"
                        ],
                        "formats": ["relative"]
                    }
                }
            },
            "initializationOptions": {
                "checkOnSave": true
            }
        }
    });

    if send_lsp_message(&mut stdin, &init_req).await.is_err() {
        let _ = tx.send(LspOutbound::Status(LspStatus::Error(
            "Init request failed".into(),
        )));
        return;
    }

    let mut server_legend: Vec<String> = Vec::new();

    // Await response to initialization request (id == 1) and extract server legend.
    loop {
        if let Ok(msg) = read_lsp_message(&mut stdout).await {
            if msg.get("id").and_then(Value::as_i64) == Some(1) {
                if let Some(types_arr) = msg
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
        } else {
            let _ = tx.send(LspOutbound::Status(LspStatus::Error(
                "Init rejected".into(),
            )));
            return;
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

    // Confirm initialization to server.
    let initialized = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "initialized",
        "params": {}
    });
    let _ = send_lsp_message(&mut stdin, &initialized).await;

    // Ingest the initial document contents via didOpen.
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

    // Main event loop multiplexing frontend commands and server responses.
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
                        let st_req = serde_json::json!({
                            "jsonrpc": "2.0",
                            "id": req_id,
                            "method": "textDocument/semanticTokens/full",
                            "params": {
                                "textDocument": { "uri": current_file_uri }
                            }
                        });
                        let _ = send_lsp_message(&mut stdin, &st_req).await;
                    }
                    Some(LspInbound::OpenFile { path, text, lang_id }) => {

                        current_file_uri = file_to_uri(&path);
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
                    None => break, // Frontend sender dropped; terminate actor.
                }
            }
            msg = read_lsp_message(&mut stdout) => {
                if let Ok(json) = msg {
                                        // Handle diagnostics notifications published by the server.
                    if json.get("method").and_then(|m| m.as_str()) == Some("textDocument/publishDiagnostics") {
                        if let Some(params) = json.get("params") {
                            let diag_uri = params.get("uri").and_then(|u| u.as_str()).unwrap_or("");
                            if diag_uri == current_file_uri
                                || diag_uri.trim_end_matches('/') == current_file_uri.trim_end_matches('/')
                            {
                                let mut items = Vec::new();
                                if let Some(diag_array) = params.get("diagnostics").and_then(|d| d.as_array()) {
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
                                        // Handle completion and semantic tokens responses matching a previously sent request ID.
                    } else if let Some(resp_id) = json.get("id").and_then(Value::as_i64) {
                        let result_val = json.get("result");

                        if let Some(data) = result_val.and_then(|r| r.get("data")).and_then(|d| d.as_array()) {
                            let ints: Vec<usize> = data.iter().filter_map(Value::as_u64).map(|v| v as usize).collect();
                            let mut tokens = Vec::new();
                            let mut cur_line = 0usize;
                            let mut cur_char = 0usize;

                            // chunks_exact is intentional here: the LSP semantic-tokens data
                            // array is a flat, non-array-backed Vec<usize> of arbitrary
                            // runtime length, so `as_chunks::<5>()` (which needs a fixed-size
                            // array as input) does not apply.
                            #[allow(clippy::chunks_exact_to_as_chunks)]
                            for chunk in ints.chunks_exact(5) {
                                let delta_line = chunk[0];
                                let delta_start = chunk[1];
                                let length = chunk[2];
                                let token_type_idx = chunk[3];

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


                            let _ = tx.send(LspOutbound::SemanticTokens { tokens });
                        } else {
                            let mut results = Vec::new();

                            // LSP completion responses may return either `CompletionItem[]` or `CompletionList { items: [...] }`.
                            let items_array = result_val.and_then(|r| {
                                if r.is_array() {
                                    Some(r.as_array().unwrap())
                                } else {
                                    r.get("items").and_then(|it| it.as_array())
                                }
                            });

                            if let Some(arr) = items_array {

                            for item in arr {
                                if let Some(label) = item.get("label").and_then(|l| l.as_str()) {
                                    let insert_text = if let Some(it) = item.get("insertText").and_then(|it| it.as_str()) {
                                        it.to_string()
                                    } else if let Some(te) = item.get("textEdit") {
                                        te.get("newText")
                                            .and_then(|nt| nt.as_str())
                                            .unwrap_or(label)
                                            .to_string()
                                    } else {
                                        label.to_string()
                                    };
                                    let detail = item
                                        .get("detail")
                                        .and_then(|d| d.as_str())
                                        .map(ToString::to_string);
                                    let kind = item.get("kind").and_then(Value::as_u64).unwrap_or(0);

                                    results.push(SuggestionItem {
                                        label: label.to_string(),
                                        insert_text,
                                        detail,
                                        kind,
                                    });
                                }
                            }
                        }

                                                    let _ = tx.send(LspOutbound::Completions { req_id: resp_id, items: results });
                        }
                    }
                } else {

                    // Stream closed or unreadable; notify UI and terminate actor.
                    let _ = tx.send(LspOutbound::Status(LspStatus::Error("Terminated".into())));
                    break;
                }
            }
        }
    }
}

// === Dynamic Tree-Sitter Loader via libloading ===

/// Encapsulates a dynamically loaded Tree-sitter parser and its compiled highlight query.
pub struct DynamicGrammar {
    pub parser: tree_sitter::Parser,
    pub query: Option<tree_sitter::Query>,
    _lib: libloading::Library,
}

impl DynamicGrammar {
    /// Attempts to locate and load a shared grammar library (`.so`, `.dylib`, or `.dll`)
    /// and its corresponding `highlights.scm` query from standard user paths.
    pub fn load(lang_name: &str) -> Option<Self> {
        if lang_name.is_empty() {
            return None;
        }

        let ext = if cfg!(target_os = "windows") {
            "dll"
        } else if cfg!(target_os = "macos") {
            "dylib"
        } else {
            "so"
        };

        let file_candidates = [
            format!("{lang_name}.{ext}"),
            format!("libtree-sitter-{lang_name}.{ext}"),
            format!("tree-sitter-{lang_name}.{ext}"),
        ];

        let mut search_dirs = Vec::new();
        if let Ok(home) = env::var("HOME") {
            search_dirs.push(PathBuf::from(home.clone()).join(".local/share/subject0/grammars"));
            search_dirs.push(PathBuf::from(home).join(".config/subject0/grammars"));
        }
        search_dirs.push(PathBuf::from("./grammars"));

        for dir in search_dirs {
            for name in &file_candidates {
                let p = dir.join(name);
                if p.is_file()
                    && let Ok(lib) = unsafe { libloading::Library::new(&p) }
                {
                    let symbol_name = format!("tree_sitter_{lang_name}");
                    let constructor: Result<
                        libloading::Symbol<unsafe extern "C" fn() -> tree_sitter::Language>,
                        _,
                    > = unsafe { lib.get(symbol_name.as_bytes()) };
                    if let Ok(lang_fn) = constructor {
                        let language = unsafe { lang_fn() };
                        let mut parser = tree_sitter::Parser::new();
                        if parser.set_language(&language).is_ok() {
                            // Load highlights.scm query if present
                            let mut query = None;
                            let mut query_paths = Vec::new();
                            if let Ok(home) = env::var("HOME") {
                                query_paths.push(
                                    PathBuf::from(home.clone())
                                        .join(".local/share/subject0/queries")
                                        .join(lang_name)
                                        .join("highlights.scm"),
                                );
                                query_paths.push(
                                    PathBuf::from(home)
                                        .join(".config/subject0/queries")
                                        .join(lang_name)
                                        .join("highlights.scm"),
                                );
                            }
                            query_paths.push(
                                PathBuf::from("./queries")
                                    .join(lang_name)
                                    .join("highlights.scm"),
                            );

                            for qp in query_paths {
                                if qp.is_file()
                                    && let Ok(content) = fs::read_to_string(&qp)
                                    && let Ok(q) = tree_sitter::Query::new(&language, &content)
                                {
                                    query = Some(q);
                                    break;
                                }
                            }

                            return Some(Self {
                                parser,
                                query,
                                _lib: lib,
                            });
                        }
                    }
                }
            }
        }
        None
    }

    /// Checks whether a compiled grammar shared library for `lang_name` exists
    /// on disk in any of the standard search locations, without loading it
    /// (no `dlopen`). Used by the `--health` report so a broken/incompatible
    /// library can't crash the check — it only confirms presence.
    pub fn grammar_file_path(lang_name: &str) -> Option<PathBuf> {
        if lang_name.is_empty() {
            return None;
        }

        let ext = if cfg!(target_os = "windows") {
            "dll"
        } else if cfg!(target_os = "macos") {
            "dylib"
        } else {
            "so"
        };

        let file_candidates = [
            format!("{lang_name}.{ext}"),
            format!("libtree-sitter-{lang_name}.{ext}"),
            format!("tree-sitter-{lang_name}.{ext}"),
        ];

        let mut search_dirs = Vec::new();
        if let Ok(home) = env::var("HOME") {
            search_dirs.push(PathBuf::from(home.clone()).join(".local/share/subject0/grammars"));
            search_dirs.push(PathBuf::from(home).join(".config/subject0/grammars"));
        }
        search_dirs.push(PathBuf::from("./grammars"));

        for dir in search_dirs {
            for name in &file_candidates {
                let p = dir.join(name);
                if p.is_file() {
                    return Some(p);
                }
            }
        }
        None
    }
}

// === Syntax Highlighting Engine ===

/// Identifies the source programming/markup language across 100+ languages.
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
    // --- Extended language set ---
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

        // A handful of build/config files are recognized by exact filename
        // rather than extension.
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

    /// Human-readable display name, used in the `:health` report and status line.
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
            SupportedLanguage::Groovy => "groovy",
            SupportedLanguage::Gradle => "groovy",
            SupportedLanguage::ObjectiveC => "objc",
            SupportedLanguage::ObjectiveCpp => "objc",
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
            SupportedLanguage::Bash => "shellscript",
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
            SupportedLanguage::Terraform => "terraform",
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
            SupportedLanguage::Zsh => "shellscript",
            SupportedLanguage::Fish => "fish",
            SupportedLanguage::PowerShell => "powershell",
            SupportedLanguage::Groovy => "groovy",
            SupportedLanguage::Gradle => "groovy",
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
            SupportedLanguage::Hcl => "hcl",
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

    /// Returns candidate language server binary names in priority order.
    ///
    /// Entries are the real, verified binary names shipped by each server's
    /// official distribution as of this writing. Languages with no widely
    /// adopted standalone LSP implementation return an empty slice, so the
    /// editor correctly falls back to Tier 1/2 highlighting alone rather than
    /// claiming a server exists.
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
            SupportedLanguage::Solidity => &["solc", "nomicfoundation-solidity-language-server"],
            SupportedLanguage::Haxe => &["haxe-language-server"],
            SupportedLanguage::Pascal => &["pasls"],
            SupportedLanguage::Ada => &["ada_language_server"],
            SupportedLanguage::Tcl => &["tclsh"],
            SupportedLanguage::Makefile => &["cmake-language-server"],
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
            SupportedLanguage::Gd => &[],
            _ => &[],
        }
    }

    /// Scans the system for installed candidate servers.
    pub fn installed_servers(self) -> Vec<String> {
        self.candidate_servers()
            .iter()
            .filter(|cmd| resolve_binary_path(cmd).is_some())
            .map(|s| (*s).to_string())
            .collect()
    }

    /// All supported languages, in a stable order, for use by the `:health`
    /// report and similar full-listing UI.
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

/// Core syntax engine coordinating instant lexical highlighting, dynamic tree-sitter AST,
/// and compiler-grade LSP semantic token overlays.
pub struct SyntaxEngine {
    pub language: SupportedLanguage,
    pub dynamic_grammar: Option<DynamicGrammar>,
    pub tree: Option<tree_sitter::Tree>,
    /// Spatial lookup of Tree-sitter query tokens: line index -> tokens
    pub ts_tokens: std::collections::HashMap<usize, Vec<SemanticTokenSpan>>,
    /// Spatial lookup of LSP semantic tokens: line index -> tokens
    pub semantic_tokens: std::collections::HashMap<usize, Vec<SemanticTokenSpan>>,
}

impl SyntaxEngine {
    pub fn new(path: Option<&PathBuf>) -> Self {
        let language = SupportedLanguage::from_path(path);
        let dynamic_grammar = DynamicGrammar::load(language.grammar_name());

        Self {
            language,
            dynamic_grammar,
            tree: None,
            ts_tokens: std::collections::HashMap::new(),
            semantic_tokens: std::collections::HashMap::new(),
        }
    }

    /// Reparses buffer text using dynamic Tree-sitter and evaluates highlight queries.
    pub fn reparse(&mut self, text: &str) {
        if let Some(dg) = &mut self.dynamic_grammar {
            self.tree = dg.parser.parse(text, None);
            self.ts_tokens.clear();

            if let (Some(tree), Some(query)) = (&self.tree, &dg.query) {
                let mut cursor = tree_sitter::QueryCursor::new();
                let text_bytes = text.as_bytes();
                let lines: Vec<&str> = text.lines().collect();

                for m in cursor.matches(query, tree.root_node(), text_bytes) {
                    for capture in m.captures {
                        let node = capture.node;
                        let start = node.start_position();
                        let end = node.end_position();
                        let capture_name = query.capture_names()[capture.index as usize];
                        let token_type = CanonicalTokenType::from_query_capture(capture_name);

                        if token_type != CanonicalTokenType::Other {
                            for row in start.row..=end.row {
                                let col_start = if row == start.row { start.column } else { 0 };
                                let col_end = if row == end.row {
                                    end.column
                                } else {
                                    lines.get(row).map_or(0, |l| l.len())
                                };

                                if col_end > col_start {
                                    self.ts_tokens.entry(row).or_default().push(
                                        SemanticTokenSpan {
                                            line: row,
                                            start_col: col_start,
                                            length: col_end - col_start,
                                            token_type,
                                        },
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    /// Ingests and indexes decoded LSP semantic tokens by line row.
    pub fn set_semantic_tokens(&mut self, tokens: Vec<SemanticTokenSpan>) {
        self.semantic_tokens.clear();
        for tok in tokens {
            self.semantic_tokens.entry(tok.line).or_default().push(tok);
        }
    }

    /// Renders a single row of text into styled Ratatui Spans.
    ///
    /// Layered composition (each tier only overrides the characters it actually
    /// classifies; anything a higher tier leaves untagged keeps the styling from
    /// the tier below it, so highlighting never "falls through" to a flat color):
    /// 1. Instant Universal Lexer (Tier 1) — always computed first as the base layer.
    /// 2. Dynamic Tree-sitter Query tokens via `.scm` (Tier 2) — overlaid on top.
    /// 3. Compiler-accurate LSP semantic tokens (Tier 3) — overlaid last, highest priority.
    pub fn highlight_line(&self, line_text: &str, line_idx: usize) -> Vec<Span<'static>> {
        if line_text.is_empty() {
            return vec![Span::raw("")];
        }

        // Tier 1: Universal Lexer forms the base layer for every line.
        let base_spans = match self.language {
            SupportedLanguage::Markdown => Self::highlight_markdown(line_text),
            _ => self.highlight_code_universal(line_text),
        };

        let ts_tokens = self.ts_tokens.get(&line_idx).map_or(&[][..], Vec::as_slice);
        let semantic_tokens = self
            .semantic_tokens
            .get(&line_idx)
            .map_or(&[][..], Vec::as_slice);

        if ts_tokens.is_empty() && semantic_tokens.is_empty() {
            return base_spans;
        }

        Self::render_layered_line(line_text, &base_spans, ts_tokens, semantic_tokens)
    }

    /// Merges Tier 1 base spans with Tier 2 and Tier 3 token overlays on a
    /// per-character basis, so gaps left by a higher tier fall back to the next
    /// tier down instead of a flat default color.
    fn render_layered_line(
        text: &str,
        base_spans: &[Span<'static>],
        ts_tokens: &[SemanticTokenSpan],
        semantic_tokens: &[SemanticTokenSpan],
    ) -> Vec<Span<'static>> {
        let chars: Vec<char> = text.chars().collect();
        if chars.is_empty() {
            return vec![Span::raw("")];
        }

        // Expand the Tier 1 base spans into a per-character style array.
        let mut styles = Vec::with_capacity(chars.len());
        for span in base_spans {
            let span_len = span.content.chars().count();
            for _ in 0..span_len {
                styles.push(span.style);
            }
        }
        // Defensive: if base span char count ever drifts from `chars.len()`
        // (should not happen), pad with the default style rather than panic.
        let default_style = Style::default().fg(Color::Rgb(215, 220, 230));
        while styles.len() < chars.len() {
            styles.push(default_style);
        }
        styles.truncate(chars.len());

        // Overlay Tier 2 (tree-sitter) on top of the Tier 1 base.
        for tok in ts_tokens {
            let start = tok.start_col;
            let end = (start + tok.length).min(chars.len());
            if start < chars.len() && end > start {
                let style = Self::style_for_token_type(tok.token_type);
                for cell in &mut styles[start..end] {
                    *cell = style;
                }
            }
        }

        // Overlay Tier 3 (LSP semantic tokens) last, taking final priority.
        for tok in semantic_tokens {
            let start = tok.start_col;
            let end = (start + tok.length).min(chars.len());
            if start < chars.len() && end > start {
                let style = Self::style_for_token_type(tok.token_type);
                for cell in &mut styles[start..end] {
                    *cell = style;
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

    fn style_for_token_type(token_type: CanonicalTokenType) -> Style {
        match token_type {
            CanonicalTokenType::Keyword => Style::default()
                .fg(Color::Rgb(220, 110, 240))
                .add_modifier(Modifier::BOLD),
            CanonicalTokenType::Type => Style::default().fg(Color::Rgb(240, 200, 90)),
            CanonicalTokenType::Function => Style::default().fg(Color::Rgb(100, 175, 255)),
            CanonicalTokenType::String => Style::default().fg(Color::Rgb(150, 215, 120)),
            CanonicalTokenType::Number => Style::default().fg(Color::Rgb(250, 175, 95)),
            CanonicalTokenType::Comment => Style::default()
                .fg(Color::Rgb(115, 125, 140))
                .add_modifier(Modifier::ITALIC),
            CanonicalTokenType::Macro => Style::default()
                .fg(Color::Rgb(80, 210, 240))
                .add_modifier(Modifier::BOLD),
            CanonicalTokenType::Operator => Style::default().fg(Color::Rgb(140, 150, 170)),
            CanonicalTokenType::Namespace => Style::default().fg(Color::Rgb(140, 180, 240)),
            CanonicalTokenType::Variable
            | CanonicalTokenType::Parameter
            | CanonicalTokenType::Property
            | CanonicalTokenType::Other => Style::default().fg(Color::Rgb(220, 225, 235)),
        }
    }

    fn highlight_markdown(line_text: &str) -> Vec<Span<'static>> {
        let trimmed = line_text.trim_start();
        if trimmed.starts_with("# ") {
            vec![Span::styled(
                line_text.to_string(),
                Style::default()
                    .fg(Color::Rgb(80, 200, 240))
                    .add_modifier(Modifier::BOLD),
            )]
        } else if trimmed.starts_with("## ") {
            vec![Span::styled(
                line_text.to_string(),
                Style::default()
                    .fg(Color::Rgb(120, 180, 255))
                    .add_modifier(Modifier::BOLD),
            )]
        } else if trimmed.starts_with("### ") {
            vec![Span::styled(
                line_text.to_string(),
                Style::default()
                    .fg(Color::Rgb(180, 160, 240))
                    .add_modifier(Modifier::BOLD),
            )]
        } else if trimmed.starts_with("```") {
            vec![Span::styled(
                line_text.to_string(),
                Style::default().fg(Color::Rgb(240, 180, 90)),
            )]
        } else {
            vec![Span::styled(
                line_text.to_string(),
                Style::default().fg(Color::Rgb(210, 215, 225)),
            )]
        }
    }

    fn highlight_code_universal(&self, line_text: &str) -> Vec<Span<'static>> {
        let mut spans = Vec::new();
        let chars: Vec<char> = line_text.chars().collect();
        let len = chars.len();
        let mut idx = 0;

        let comment_prefix = match self.language {
            SupportedLanguage::Python
            | SupportedLanguage::Bash
            | SupportedLanguage::Zsh
            | SupportedLanguage::Fish
            | SupportedLanguage::Yaml
            | SupportedLanguage::Toml
            | SupportedLanguage::Ruby
            | SupportedLanguage::Perl
            | SupportedLanguage::Nim
            | SupportedLanguage::Julia
            | SupportedLanguage::R
            | SupportedLanguage::Elixir
            | SupportedLanguage::Crystal
            | SupportedLanguage::PowerShell
            | SupportedLanguage::Dockerfile
            | SupportedLanguage::Makefile
            | SupportedLanguage::Awk
            | SupportedLanguage::Tcl
            | SupportedLanguage::Gd
            | SupportedLanguage::EnvFile
            | SupportedLanguage::Properties
            | SupportedLanguage::Cmake
            | SupportedLanguage::Nginx
            | SupportedLanguage::Terraform
            | SupportedLanguage::Hcl
            | SupportedLanguage::GraphQL
            | SupportedLanguage::Ini => Some("#"),
            SupportedLanguage::Lua
            | SupportedLanguage::Sql
            | SupportedLanguage::Haskell
            | SupportedLanguage::Elm
            | SupportedLanguage::Ada
            | SupportedLanguage::VHDL => Some("--"),
            SupportedLanguage::Clojure
            | SupportedLanguage::CommonLisp
            | SupportedLanguage::Scheme
            | SupportedLanguage::Racket
            | SupportedLanguage::EmacsLisp
            | SupportedLanguage::Assembly => Some(";"),
            SupportedLanguage::Erlang
            | SupportedLanguage::Prolog
            | SupportedLanguage::Latex
            | SupportedLanguage::Bibtex => Some("%"),
            SupportedLanguage::Vim => Some("\""),
            _ => Some("//"),
        };

        while idx < len {
            if let Some(prefix) = comment_prefix {
                let rest: String = chars[idx..].iter().collect();
                if rest.starts_with(prefix) {
                    spans.push(Span::styled(
                        rest,
                        Style::default()
                            .fg(Color::Rgb(115, 125, 140))
                            .add_modifier(Modifier::ITALIC),
                    ));
                    break;
                }
            }

            // Builtin compiler intrinsics (Zig @import, @as, @intCast, etc.)
            if chars[idx] == '@' && idx + 1 < len && chars[idx + 1].is_alphabetic() {
                let start = idx;
                idx += 1;
                while idx < len && (chars[idx].is_alphanumeric() || chars[idx] == '_') {
                    idx += 1;
                }
                let builtin: String = chars[start..idx].iter().collect();
                spans.push(Span::styled(
                    builtin,
                    Style::default()
                        .fg(Color::Rgb(80, 210, 240))
                        .add_modifier(Modifier::BOLD),
                ));
                continue;
            }

            // String literals
            if chars[idx] == '"' || chars[idx] == '\'' || chars[idx] == '`' {
                let quote = chars[idx];
                let mut end = idx + 1;
                while end < len {
                    if chars[end] == '\\' && end + 1 < len {
                        end += 2;
                        continue;
                    }
                    if chars[end] == quote {
                        end += 1;
                        break;
                    }
                    end += 1;
                }
                let token: String = chars[idx..end].iter().collect();
                spans.push(Span::styled(
                    token,
                    Style::default().fg(Color::Rgb(150, 215, 120)),
                ));
                idx = end;
                continue;
            }

            // Numbers
            if chars[idx].is_ascii_digit() {
                let mut end = idx;
                while end < len && (chars[end].is_ascii_alphanumeric() || chars[end] == '.') {
                    end += 1;
                }
                let num: String = chars[idx..end].iter().collect();
                spans.push(Span::styled(
                    num,
                    Style::default().fg(Color::Rgb(250, 175, 95)),
                ));
                idx = end;
                continue;
            }

            // Keywords, Builtins, and Types
            if chars[idx].is_alphabetic() || chars[idx] == '_' {
                let mut end = idx;
                while end < len && (chars[end].is_alphanumeric() || chars[end] == '_') {
                    end += 1;
                }
                let word: String = chars[idx..end].iter().collect();

                let mut lookahead = end;
                while lookahead < len && chars[lookahead].is_whitespace() {
                    lookahead += 1;
                }
                let is_func = lookahead < len && chars[lookahead] == '(';

                let style = match word.as_str() {
                    // Control flow & definitions
                    "fn" | "func" | "def" | "function" | "proc" | "let" | "mut" | "const"
                    | "var" | "val" | "if" | "else" | "match" | "switch" | "case" | "for"
                    | "while" | "loop" | "do" | "end" | "return" | "break" | "continue"
                    | "import" | "from" | "pub" | "export" | "extern" | "struct" | "enum"
                    | "union" | "class" | "interface" | "trait" | "impl" | "type" | "package"
                    | "comptime" | "inline" | "defer" | "errdefer" | "catch" | "try" | "throw"
                    | "throws" | "usingnamespace" | "threadlocal" | "unreachable" | "test"
                    | "async" | "await" | "suspend" | "resume" | "chan" | "select" | "go"
                    | "range" | "map" | "yield" | "package_clause" | "public" | "private"
                    | "protected" | "static" | "final" | "override" | "guard" => Style::default()
                        .fg(Color::Rgb(220, 110, 240))
                        .add_modifier(Modifier::BOLD),
                    // Booleans and primitives
                    "true" | "false" | "None" | "null" | "nil" | "undefined" | "iota" | "Some"
                    | "Ok" | "Err" => Style::default().fg(Color::Rgb(240, 200, 90)),
                    // Types
                    "u8" | "u16" | "u32" | "u64" | "u128" | "usize" | "i8" | "i16" | "i32"
                    | "i64" | "i128" | "isize" | "f16" | "f32" | "f64" | "f128" | "bool"
                    | "char" | "str" | "String" | "Vec" | "int" | "int8" | "int16" | "int32"
                    | "int64" | "uint" | "uint8" | "uint16" | "uint32" | "uint64" | "uintptr"
                    | "float" | "float32" | "float64" | "byte" | "rune" | "error" | "anyerror"
                    | "anyopaque" | "noreturn" | "void" | "boolean" => {
                        Style::default().fg(Color::Rgb(240, 200, 100))
                    }
                    // Common built-in utilities
                    "make" | "new" | "len" | "cap" | "append" | "copy" | "delete" | "close"
                    | "panic" | "recover" | "print" | "println" => {
                        Style::default().fg(Color::Rgb(100, 175, 255))
                    }
                    _ => {
                        if is_func {
                            Style::default().fg(Color::Rgb(100, 175, 255))
                        } else if word.chars().next().is_some_and(char::is_uppercase) {
                            Style::default().fg(Color::Rgb(240, 200, 90))
                        } else {
                            Style::default().fg(Color::Rgb(220, 225, 235))
                        }
                    }
                };

                spans.push(Span::styled(word, style));
                idx = end;
                continue;
            }

            spans.push(Span::styled(
                chars[idx].to_string(),
                Style::default().fg(Color::Rgb(140, 150, 170)),
            ));
            idx += 1;
        }

        spans
    }
}

/// Returns the Nerd Font glyph icon and corresponding theme color for a file path.
///
/// Inspects the file extension to select appropriate iconography for file trees,
/// tab headers, and status bars. Defaults to a generic document icon (`"󰈔"`) for
/// unrecognized extensions.
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

/// Returns the Nerd Font glyph icon and theme color for an LSP `CompletionItemKind`.
///
/// Maps numeric completion kinds defined by the Language Server Protocol specification
/// to visual indicators in the editor's autocomplete popup:
/// - `2`, `3`: Method / Function (`"󰊕"`)
/// - `4`: Constructor (`"󰌗"`)
/// - `5`, `6`: Field / Variable (`"󰫧"`)
/// - `7`, `8`: Class / Struct (`"󱡠"`)
/// - `9`: Module / Namespace (`"󰏗"`)
/// - `14`: Keyword (`"󰌆"`)
/// - Other: Text / Default (`"󰈚"`)
pub fn completion_kind_icon(kind: u64) -> (&'static str, Color) {
    match kind {
        2 | 3 => ("󰊕", Color::Rgb(80, 200, 240)), // Method / Function
        4 => ("󰌗", Color::Rgb(240, 180, 70)),     // Constructor
        5 | 6 => ("󰫧", Color::Rgb(250, 210, 90)), // Field / Variable
        7 | 8 => ("󱡠", Color::Rgb(120, 160, 255)), // Class / Struct
        9 => ("󰏗", Color::Rgb(140, 220, 120)),    // Module
        14 => ("󰌆", Color::Rgb(220, 110, 240)),   // Keyword
        _ => ("󰈚", Color::Rgb(170, 175, 190)),    // Text
    }
}
