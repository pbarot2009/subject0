use std::{
    env,
    path::{Path, PathBuf},
    process::Stdio,
};

use anyhow::{anyhow, Result};
use ratatui::{
    style::{Color, Modifier, Style},
    text::Span,
};
use serde_json::Value;
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    process::{ChildStdin, Command as TokioCommand},
    sync::mpsc,
};
use tree_sitter::{Parser, Tree};

// -----------------------------------------------------------------------------
// LSP Types & Actor Protocol
// -----------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct DiagnosticItem {
    pub line: usize,
    #[allow(dead_code)]
    pub col: usize,
    pub message: String,
    pub severity: u8, // 1: Error, 2: Warning, 3: Info, 4: Hint
}

#[derive(Debug, Clone)]
pub struct SuggestionItem {
    pub label: String,
    pub insert_text: String,
    pub detail: Option<String>,
    pub kind: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LspStatus {
    Disabled,
    NotFound(String),
    Starting(String),
    Ready(String),
    Error(String),
}

pub enum LspInbound {
    Change { text: String, version: i64 },
    Save,
    Completion { line: usize, col: usize, req_id: i64 },
    OpenFile { path: PathBuf, text: String, lang_id: String },
}

pub enum LspOutbound {
    Status(LspStatus),
    Diagnostics(Vec<DiagnosticItem>),
    Completions { req_id: i64, items: Vec<SuggestionItem> },
}

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

pub async fn send_lsp_message<W: AsyncWriteExt + Unpin>(writer: &mut W, value: &Value) -> Result<()> {
    let body = serde_json::to_string(value)?;
    let header = format!("Content-Length: {}\r\n\r\n", body.len());
    writer.write_all(header.as_bytes()).await?;
    writer.write_all(body.as_bytes()).await?;
    writer.flush().await?;
    Ok(())
}

pub async fn run_lsp_actor(
    initial_file: PathBuf,
    initial_lang: String,
    server_cmd: String,
    mut rx: mpsc::UnboundedReceiver<LspInbound>,
    tx: mpsc::UnboundedSender<LspOutbound>,
    initial_text: String,
) {
    let bin_path = match resolve_binary_path(&server_cmd) {
        Some(p) => p,
        None => {
            let _ = tx.send(LspOutbound::Status(LspStatus::NotFound(server_cmd)));
            return;
        }
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
                        "change": 1,
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
        let _ = tx.send(LspOutbound::Status(LspStatus::Error("Init request failed".into())));
        return;
    }

    loop {
        match read_lsp_message(&mut stdout).await {
            Ok(msg) => {
                if msg.get("id").and_then(|v| v.as_i64()) == Some(1) {
                    break;
                }
            }
            Err(_) => {
                let _ = tx.send(LspOutbound::Status(LspStatus::Error("Init rejected".into())));
                return;
            }
        }
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
                    None => break,
                }
            }
            msg = read_lsp_message(&mut stdout) => {
                match msg {
                    Ok(json) => {
                        if json.get("method").and_then(|m| m.as_str()) == Some("textDocument/publishDiagnostics") {
                            if let Some(params) = json.get("params") {
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
                        } else if let Some(resp_id) = json.get("id").and_then(|id| id.as_i64()) {
                            let mut results = Vec::new();
                            let result_val = json.get("result");

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
                                            .map(|s| s.to_string());
                                        let kind = item.get("kind").and_then(|k| k.as_u64()).unwrap_or(0);

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
                    Err(_) => {
                        let _ = tx.send(LspOutbound::Status(LspStatus::Error("Terminated".into())));
                        break;
                    }
                }
            }
        }
    }
}

// -----------------------------------------------------------------------------
// Syntax Highlighting Engine
// -----------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SupportedLanguage {
    Rust,
    Python,
    Markdown,
    Plain,
}

pub struct SyntaxEngine {
    pub language: SupportedLanguage,
    parser: Option<Parser>,
    #[allow(dead_code)]
    tree: Option<Tree>,
}

impl SyntaxEngine {
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

    pub fn reparse(&mut self, text: &str) {
        if let Some(parser) = &mut self.parser {
            self.tree = parser.parse(text, None);
        }
    }

    pub fn highlight_line(&self, line_text: &str, _line_idx: usize) -> Vec<Span<'static>> {
        if line_text.is_empty() {
            return vec![Span::raw("")];
        }

        match self.language {
            SupportedLanguage::Markdown => Self::highlight_markdown(line_text),
            SupportedLanguage::Rust | SupportedLanguage::Python => {
                Self::highlight_code(line_text, self.language)
            }
            SupportedLanguage::Plain => vec![Span::styled(line_text.to_string(), Style::default().fg(Color::Rgb(215, 220, 230)))],
        }
    }

    fn highlight_markdown(line_text: &str) -> Vec<Span<'static>> {
        let trimmed = line_text.trim_start();
        if trimmed.starts_with("# ") {
            vec![Span::styled(
                line_text.to_string(),
                Style::default().fg(Color::Rgb(80, 200, 240)).add_modifier(Modifier::BOLD),
            )]
        } else if trimmed.starts_with("## ") {
            vec![Span::styled(
                line_text.to_string(),
                Style::default().fg(Color::Rgb(120, 180, 255)).add_modifier(Modifier::BOLD),
            )]
        } else if trimmed.starts_with("### ") {
            vec![Span::styled(
                line_text.to_string(),
                Style::default().fg(Color::Rgb(180, 160, 240)).add_modifier(Modifier::BOLD),
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

    fn highlight_code(line_text: &str, lang: SupportedLanguage) -> Vec<Span<'static>> {
        let mut spans = Vec::new();
        let mut idx = 0;
        let chars: Vec<char> = line_text.chars().collect();
        let len = chars.len();

        while idx < len {
            // Comments
            if (lang == SupportedLanguage::Rust && idx + 1 < len && chars[idx] == '/' && chars[idx + 1] == '/')
                || (lang == SupportedLanguage::Python && chars[idx] == '#')
            {
                let rest: String = chars[idx..].iter().collect();
                spans.push(Span::styled(
                    rest,
                    Style::default().fg(Color::Rgb(115, 125, 140)).add_modifier(Modifier::ITALIC),
                ));
                break;
            }

            // String literals
            if chars[idx] == '"' || chars[idx] == '\'' {
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
                spans.push(Span::styled(token, Style::default().fg(Color::Rgb(150, 215, 120))));
                idx = end;
                continue;
            }

            // Numeric literals
            if chars[idx].is_ascii_digit() {
                let mut end = idx;
                while end < len && (chars[end].is_ascii_alphanumeric() || chars[end] == '.') {
                    end += 1;
                }
                let num: String = chars[idx..end].iter().collect();
                spans.push(Span::styled(num, Style::default().fg(Color::Rgb(250, 175, 95))));
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

                // Rust macro check: println!
                if lang == SupportedLanguage::Rust && end < len && chars[end] == '!' {
                    end += 1;
                    let macro_word: String = chars[idx..end].iter().collect();
                    spans.push(Span::styled(
                        macro_word,
                        Style::default().fg(Color::Rgb(80, 210, 240)).add_modifier(Modifier::BOLD),
                    ));
                    idx = end;
                    continue;
                }

                // Function call lookahead
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
                            Style::default().fg(Color::Rgb(220, 110, 240)).add_modifier(Modifier::BOLD)
                        }
                        "i8" | "i16" | "i32" | "i64" | "i128" | "u8" | "u16" | "u32" | "u64" | "u128"
                        | "usize" | "isize" | "f32" | "f64" | "bool" | "char" | "str" | "String"
                        | "Option" | "Result" | "Some" | "None" | "Ok" | "Err" | "Self" | "self"
                        | "Vec" | "Box" | "Rc" | "Arc" => {
                            Style::default().fg(Color::Rgb(240, 200, 90))
                        }
                        _ => {
                            if is_func {
                                Style::default().fg(Color::Rgb(100, 175, 255))
                            } else if word.chars().next().map(|c| c.is_uppercase()).unwrap_or(false) {
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
                            Style::default().fg(Color::Rgb(220, 110, 240)).add_modifier(Modifier::BOLD)
                        }
                        "True" | "False" | "None" | "self" | "int" | "str" | "list" | "dict"
                        | "set" | "tuple" | "bool" | "float" => {
                            Style::default().fg(Color::Rgb(240, 200, 90))
                        }
                        _ => {
                            if is_func {
                                Style::default().fg(Color::Rgb(100, 175, 255))
                            } else if word.chars().next().map(|c| c.is_uppercase()).unwrap_or(false) {
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
            spans.push(Span::styled(chars[idx].to_string(), Style::default().fg(Color::Rgb(140, 150, 170))));
            idx += 1;
        }

        spans
    }
}

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

pub fn completion_kind_icon(kind: u64) -> (&'static str, Color) {
    match kind {
        2 | 3 => ("󰊕", Color::Rgb(80, 200, 240)),  // Method / Function
        4 => ("󰌗", Color::Rgb(240, 180, 70)),       // Constructor
        5 | 6 => ("󰫧", Color::Rgb(250, 210, 90)),   // Field / Variable
        7 | 8 => ("󱡠", Color::Rgb(120, 160, 255)),  // Class / Struct
        9 => ("󰏗", Color::Rgb(140, 220, 120)),      // Module
        14 => ("󰌆", Color::Rgb(220, 110, 240)),     // Keyword
        _ => ("󰈚", Color::Rgb(170, 175, 190)),       // Text
    }
}
