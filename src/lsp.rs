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

use anyhow::{anyhow, Result};
use ratatui::{
    style::{Color, Modifier, Style},
    text::Span,
};
use serde_json::Value;
use std::{
    env,
    path::{Path, PathBuf},
    process::Stdio,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    process::{ChildStdin, Command as TokioCommand},
    sync::mpsc,
};
use tree_sitter::{Parser, Tree};

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
    let Some(bin_path) = resolve_binary_path(&server_cmd) else {
        let _ = tx.send(LspOutbound::Status(LspStatus::NotFound(server_cmd)));
        return;
    };

    let _ = tx.send(LspOutbound::Status(LspStatus::Starting(server_cmd.clone())));

    let mut child = match TokioCommand::new(bin_path)
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

    // Construct the standard LSP initialization payload announcing client capabilities.
    let init_req = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "processId": std::process::id(),
            "rootUri": root_uri,
            "capabilities": {
                "textDocument": {
                    "synchronization": {
                        "openClose": true,
                        "change": 1, // 1 = Full text synchronization
                        "save": { "includeText": false }
                    },
                    "completion": {
                        "completionItem": {
                            "snippetSupport": false,
                            "documentationFormat": ["plaintext"]
                        }
                    },
                    "publishDiagnostics": {
                        "relatedInformation": true
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

    // Await response to initialization request (id == 1) before proceeding.
    loop {
        if let Ok(msg) = read_lsp_message(&mut stdout).await {
            if msg.get("id").and_then(Value::as_i64) == Some(1) {
                break;
            }
        } else {
            let _ = tx.send(LspOutbound::Status(LspStatus::Error(
                "Init rejected".into(),
            )));
            return;
        }
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
                    // Handle completion responses matching a previously sent request ID.

                    } else if let Some(resp_id) = json.get("id").and_then(Value::as_i64) {
                        let mut results = Vec::new();
                        let result_val = json.get("result");

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
                                    let insert_text = item
                                        .get("insertText")
                                        .and_then(|it| it.as_str())
                                        .unwrap_or(label)
                                        .to_string();
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
                } else {
                    // Stream closed or unreadable; notify UI and terminate actor.
                    let _ = tx.send(LspOutbound::Status(LspStatus::Error("Terminated".into())));
                    break;
                }
            }
        }
    }
}

// === Syntax Highlighting Engine ===

/// Identifies the source programming language or document format for syntax styling.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SupportedLanguage {
    /// Rust source file (`.rs`).
    Rust,
    /// Python source file (`.py`).
    Python,
    /// Markdown documentation file (`.md`).
    Markdown,
    /// Unrecognized file extension or raw plain text.
    Plain,
}

/// Core syntax engine coordinating tree-sitter AST parsing and lexical highlighting.
pub struct SyntaxEngine {
    /// Active language detected for the current buffer.
    pub language: SupportedLanguage,
    /// Tree-sitter parser instance for languages supporting concrete grammar compilation.
    parser: Option<Parser>,
    /// Most recently computed Tree-sitter concrete syntax tree.
    #[allow(dead_code)]
    tree: Option<Tree>,
}

impl SyntaxEngine {
    /// Initializes a syntax engine configured for the file type indicated by `path`.
    ///
    /// Configures the internal Tree-sitter parser with the appropriate language grammar
    /// (`tree-sitter-rust` or `tree-sitter-python`). Falls back to non-parser highlighting
    /// for Markdown and plain text.
    pub fn new(path: Option<&PathBuf>) -> Self {
        let ext = path
            .and_then(|p| p.extension())
            .and_then(|e| e.to_str())
            .unwrap_or("");

        let (language, parser) = match ext {
            "rs" => {
                let mut p = Parser::new();
                let _ = p.set_language(&tree_sitter_rust::language());
                (SupportedLanguage::Rust, Some(p))
            }
            "py" => {
                let mut p = Parser::new();
                let _ = p.set_language(&tree_sitter_python::language());
                (SupportedLanguage::Python, Some(p))
            }
            "md" => (SupportedLanguage::Markdown, None),
            _ => (SupportedLanguage::Plain, None),
        };

        Self {
            language,
            parser,
            tree: None,
        }
    }

    /// Reparses the document text to update the cached Tree-sitter syntax tree.
    pub fn reparse(&mut self, text: &str) {
        if let Some(parser) = &mut self.parser {
            self.tree = parser.parse(text, None);
        }
    }

    /// Renders a single row of text into a sequence of styled Ratatui [`Span`] elements.
    ///
    /// Routes the input to the appropriate language highlighter based on [`Self::language`].
    pub fn highlight_line(&self, line_text: &str, _line_idx: usize) -> Vec<Span<'static>> {
        if line_text.is_empty() {
            return vec![Span::raw("")];
        }

        match self.language {
            SupportedLanguage::Markdown => Self::highlight_markdown(line_text),
            SupportedLanguage::Rust | SupportedLanguage::Python => {
                Self::highlight_code(line_text, self.language)
            }
            SupportedLanguage::Plain => vec![Span::styled(
                line_text.to_string(),
                Style::default().fg(Color::Rgb(215, 220, 230)),
            )],
        }
    }

    /// Highlights a single line of Markdown text based on structural block prefixes.
    ///
    /// Applies distinct colors and bold formatting to headers (`#`, `##`, `###`),
    /// accent colors to code fences (```` ``` ````), and neutral foreground tones
    /// to standard text.
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

    /// Performs lexical analysis on code lines for languages with keyword-based tokenization.
    ///
    /// Scans characters sequentially to tokenize:
    /// - Line comments (`//` for Rust, `#` for Python)
    /// - Single and double-quoted string literals with escape sequence handling
    /// - Numeric constants (integers and floats)
    /// - Rust macro calls (detecting identifier followed by `!`)
    /// - Function calls (detecting identifier followed by whitespace and `(`)
    /// - Language-specific reserved keywords and built-in type identifiers
    /// - Punctuation and operators
    fn highlight_code(line_text: &str, lang: SupportedLanguage) -> Vec<Span<'static>> {
        let mut spans = Vec::new();
        let mut idx = 0;
        let chars: Vec<char> = line_text.chars().collect();
        let len = chars.len();

        while idx < len {
            // Line Comments
            if (lang == SupportedLanguage::Rust
                && idx + 1 < len
                && chars[idx] == '/'
                && chars[idx + 1] == '/')
                || (lang == SupportedLanguage::Python && chars[idx] == '#')
            {
                let rest: String = chars[idx..].iter().collect();
                spans.push(Span::styled(
                    rest,
                    Style::default()
                        .fg(Color::Rgb(115, 125, 140))
                        .add_modifier(Modifier::ITALIC),
                ));
                break;
            }

            // String Literals
            if chars[idx] == '"' || chars[idx] == '\'' {
                let quote = chars[idx];
                let mut end = idx + 1;
                while end < len {
                    if chars[end] == '\\' && end + 1 < len {
                        end += 2; // Skip escaped character
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

            // Numeric Literals
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

            // Identifiers, Keywords, Macros & Functions
            if chars[idx].is_alphabetic() || chars[idx] == '_' {
                let mut end = idx;
                while end < len && (chars[end].is_alphanumeric() || chars[end] == '_') {
                    end += 1;
                }
                let word: String = chars[idx..end].iter().collect();

                // Rust macro check: println!, format!, vec!
                if lang == SupportedLanguage::Rust && end < len && chars[end] == '!' {
                    end += 1;
                    let macro_word: String = chars[idx..end].iter().collect();
                    spans.push(Span::styled(
                        macro_word,
                        Style::default()
                            .fg(Color::Rgb(80, 210, 240))
                            .add_modifier(Modifier::BOLD),
                    ));
                    idx = end;
                    continue;
                }

                // Function call lookahead: identifier followed by optional whitespace and '('
                let mut lookahead = end;
                while lookahead < len && chars[lookahead].is_whitespace() {
                    lookahead += 1;
                }
                let is_func = lookahead < len && chars[lookahead] == '(';

                let style = match lang {
                    SupportedLanguage::Rust => match word.as_str() {
                        "fn" | "let" | "mut" | "pub" | "struct" | "enum" | "match" | "if"
                        | "else" | "impl" | "for" | "in" | "while" | "return" | "use" | "mod"
                        | "async" | "await" | "trait" | "type" | "where" | "loop" | "as"
                        | "break" | "continue" | "const" | "static" | "ref" | "move" => {
                            Style::default()
                                .fg(Color::Rgb(220, 110, 240))
                                .add_modifier(Modifier::BOLD)
                        }
                        "i8" | "i16" | "i32" | "i64" | "i128" | "u8" | "u16" | "u32" | "u64"
                        | "u128" | "usize" | "isize" | "f32" | "f64" | "bool" | "char" | "str"
                        | "String" | "Option" | "Result" | "Some" | "None" | "Ok" | "Err"
                        | "Self" | "self" | "Vec" | "Box" | "Rc" | "Arc" => {
                            Style::default().fg(Color::Rgb(240, 200, 90))
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
                    },
                    SupportedLanguage::Python => match word.as_str() {
                        "def" | "class" | "if" | "elif" | "else" | "for" | "while" | "return"
                        | "import" | "from" | "as" | "with" | "try" | "except" | "finally"
                        | "lambda" | "yield" | "pass" | "break" | "continue" | "in" | "is"
                        | "not" | "and" | "or" | "global" | "nonlocal" | "assert" => {
                            Style::default()
                                .fg(Color::Rgb(220, 110, 240))
                                .add_modifier(Modifier::BOLD)
                        }
                        "True" | "False" | "None" | "self" | "int" | "str" | "list" | "dict"
                        | "set" | "tuple" | "bool" | "float" => {
                            Style::default().fg(Color::Rgb(240, 200, 90))
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
                    },
                    _ => Style::default().fg(Color::Rgb(220, 225, 235)),
                };

                spans.push(Span::styled(word, style));
                idx = end;
                continue;
            }

            // Operators & Punctuation
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
        "py" => ("", Color::Rgb(255, 212, 59)),
        "md" => ("", Color::Rgb(120, 180, 255)),
        "toml" => ("", Color::Rgb(160, 80, 50)),
        "json" => ("", Color::Rgb(240, 200, 80)),
        "c" | "h" => ("", Color::Rgb(80, 140, 255)),
        "cpp" | "hpp" => ("", Color::Rgb(80, 140, 255)),
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
