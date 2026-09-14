use std::{
    env,
    fs::File,
    io::{stdout, BufWriter, Write},
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

use anyhow::{anyhow, Result};
use crossterm::{
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, MouseButton,
        MouseEvent, MouseEventKind,
    },
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Position, Size},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Paragraph},
    Frame, Terminal,
};
use ropey::Rope;
use serde_json::Value;
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    process::{ChildStdin, Command as TokioCommand},
    sync::mpsc,
};
use tree_sitter::{Parser, Tree};

// -----------------------------------------------------------------------------
// LSP Types & Background Actor Loop
// -----------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct DiagnosticItem {
    pub line: usize,
    pub col: usize,
    pub message: String,
    pub severity: u8,
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
}

pub enum LspOutbound {
    Status(LspStatus),
    Diagnostics(Vec<DiagnosticItem>),
}

fn resolve_binary_path(cmd: &str) -> Option<PathBuf> {
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

fn file_to_uri(path: &Path) -> String {
    let abs = if path.is_absolute() {
        path.to_path_buf()
    } else if let Ok(cwd) = env::current_dir() {
        cwd.join(path)
    } else {
        path.to_path_buf()
    };
    format!("file://{}", abs.to_string_lossy())
}

async fn read_lsp_message<R: AsyncBufReadExt + Unpin>(reader: &mut R) -> Result<Value> {
    let mut content_length = 0usize;
    let mut line = String::new();

    loop {
        line.clear();
        let bytes_read = reader.read_line(&mut line).await?;
        if bytes_read == 0 {
            return Err(anyhow!("LSP stream terminated"));
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

async fn send_lsp_message<W: AsyncWriteExt + Unpin>(writer: &mut W, value: &Value) -> Result<()> {
    let body = serde_json::to_string(value)?;
    let header = format!("Content-Length: {}\r\n\r\n", body.len());
    writer.write_all(header.as_bytes()).await?;
    writer.write_all(body.as_bytes()).await?;
    writer.flush().await?;
    Ok(())
}

async fn run_lsp_actor(
    file_path: PathBuf,
    lang_id: String,
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
    let file_uri = file_to_uri(&file_path);

    // Step 1: Initialize Request
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

    // Step 2: Await Initialize Response
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

    // Step 3: Initialized Notification
    let initialized = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "initialized",
        "params": {}
    });
    let _ = send_lsp_message(&mut stdin, &initialized).await;

    // Step 4: Open Document
    let did_open = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "textDocument/didOpen",
        "params": {
            "textDocument": {
                "uri": file_uri,
                "languageId": lang_id,
                "version": 1,
                "text": initial_text
            }
        }
    });
    let _ = send_lsp_message(&mut stdin, &did_open).await;
    let _ = tx.send(LspOutbound::Status(LspStatus::Ready(server_cmd)));

    // Step 5: Event Loop
    loop {
        tokio::select! {
            cmd = rx.recv() => {
                match cmd {
                    Some(LspInbound::Change { text, version }) => {
                        let did_change = serde_json::json!({
                            "jsonrpc": "2.0",
                            "method": "textDocument/didChange",
                            "params": {
                                "textDocument": { "uri": file_uri, "version": version },
                                "contentChanges": [{ "text": text }]
                            }
                        });
                        let _ = send_lsp_message(&mut stdin, &did_change).await;
                    }
                    Some(LspInbound::Save) => {
                        let did_save = serde_json::json!({
                            "jsonrpc": "2.0",
                            "method": "textDocument/didSave",
                            "params": { "textDocument": { "uri": file_uri } }
                        });
                        let _ = send_lsp_message(&mut stdin, &did_save).await;
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
            SupportedLanguage::Plain => vec![Span::raw(line_text.to_string())],
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
                    Style::default().fg(Color::Rgb(100, 110, 125)).add_modifier(Modifier::ITALIC),
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
                spans.push(Span::styled(token, Style::default().fg(Color::Rgb(150, 210, 120))));
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
                spans.push(Span::styled(num, Style::default().fg(Color::Rgb(250, 170, 90))));
                idx = end;
                continue;
            }

            // Identifiers & Keywords
            if chars[idx].is_alphabetic() || chars[idx] == '_' {
                let mut end = idx;
                while end < len && (chars[end].is_alphanumeric() || chars[end] == '_') {
                    end += 1;
                }
                let word: String = chars[idx..end].iter().collect();

                let style = match lang {
                    SupportedLanguage::Rust => match word.as_str() {
                        "fn" | "let" | "mut" | "pub" | "struct" | "enum" | "match" | "if"
                        | "else" | "impl" | "for" | "in" | "while" | "return" | "use" | "mod"
                        | "async" | "await" | "trait" | "type" | "where" | "loop" => {
                            Style::default().fg(Color::Rgb(220, 110, 240)).add_modifier(Modifier::BOLD)
                        }
                        "i8" | "i16" | "i32" | "i64" | "u8" | "u16" | "u32" | "u64" | "usize"
                        | "isize" | "f32" | "f64" | "bool" | "char" | "String" | "Option"
                        | "Result" | "Some" | "None" | "Ok" | "Err" | "Self" | "self" => {
                            Style::default().fg(Color::Rgb(240, 200, 90))
                        }
                        _ => {
                            if end < len && chars[end] == '(' {
                                Style::default().fg(Color::Rgb(100, 170, 255))
                            } else {
                                Style::default().fg(Color::Rgb(220, 225, 235))
                            }
                        }
                    },
                    SupportedLanguage::Python => match word.as_str() {
                        "def" | "class" | "if" | "elif" | "else" | "for" | "while" | "return"
                        | "import" | "from" | "as" | "with" | "try" | "except" | "finally"
                        | "lambda" | "yield" | "pass" | "break" | "continue" | "in" | "is"
                        | "not" | "and" | "or" => {
                            Style::default().fg(Color::Rgb(220, 110, 240)).add_modifier(Modifier::BOLD)
                        }
                        "True" | "False" | "None" | "self" | "int" | "str" | "list" | "dict" => {
                            Style::default().fg(Color::Rgb(240, 200, 90))
                        }
                        _ => {
                            if end < len && chars[end] == '(' {
                                Style::default().fg(Color::Rgb(100, 170, 255))
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

            // Punctuation
            spans.push(Span::styled(
                chars[idx].to_string(),
                Style::default().fg(Color::Rgb(140, 150, 170)),
            ));
            idx += 1;
        }

        spans
    }
}

// -----------------------------------------------------------------------------
// Core Editor Model
// -----------------------------------------------------------------------------

#[derive(PartialEq, Eq, Clone, Copy)]
pub enum Mode {
    Normal,
    Insert,
    Command,
}

pub struct Editor {
    pub rope: Rope,
    pub path: Option<PathBuf>,
    pub mode: Mode,
    pub cursor_x: usize,
    pub cursor_y: usize,
    pub scroll_x: usize,
    pub scroll_y: usize,
    pub modified: bool,
    pub status_msg: String,
    pub command_buffer: String,
    pub pending_key: Option<char>,
    pub undo_stack: Vec<Rope>,
    pub syntax: SyntaxEngine,
    pub diagnostics: Vec<DiagnosticItem>,
    pub lsp_status: LspStatus,
    pub lsp_tx: Option<mpsc::UnboundedSender<LspInbound>>,
    pub doc_version: i64,
    pub should_quit: bool,
}

impl Editor {
    pub fn new(path: Option<PathBuf>) -> Result<Self> {
        let (rope, status_msg) = match &path {
            Some(p) if p.exists() => {
                let file = File::open(p)?;
                (Rope::from_reader(file)?, format!("Loaded {}", p.display()))
            }
            Some(p) => (Rope::new(), format!("New: {}", p.display())),
            None => (Rope::new(), "Ready".to_string()),
        };

        let mut syntax = SyntaxEngine::new(path.as_ref());
        let text = rope.to_string();
        syntax.reparse(&text);

        Ok(Self {
            rope,
            path,
            mode: Mode::Normal,
            cursor_x: 0,
            cursor_y: 0,
            scroll_x: 0,
            scroll_y: 0,
            modified: false,
            status_msg,
            command_buffer: String::new(),
            pending_key: None,
            undo_stack: Vec::new(),
            syntax,
            diagnostics: Vec::new(),
            lsp_status: LspStatus::Disabled,
            lsp_tx: None,
            doc_version: 1,
            should_quit: false,
        })
    }

    pub fn snapshot(&mut self) {
        if self.undo_stack.len() >= 64 {
            self.undo_stack.remove(0);
        }
        self.undo_stack.push(self.rope.clone());
    }

    pub fn undo(&mut self) {
        if let Some(prev) = self.undo_stack.pop() {
            self.rope = prev;
            self.modified = true;
            self.status_msg = "Reverted change".to_string();
            self.clamp_cursor();
            self.on_buffer_modified();
        }
    }

    pub fn on_buffer_modified(&mut self) {
        let text = self.rope.to_string();
        self.syntax.reparse(&text);
        self.doc_version += 1;

        if let Some(tx) = &self.lsp_tx {
            let _ = tx.send(LspInbound::Change {
                text,
                version: self.doc_version,
            });
        }
    }

    pub fn current_line_len(&self) -> usize {
        line_len(&self.rope, self.cursor_y)
    }

    pub fn char_index(&self) -> usize {
        let line_start = self.rope.line_to_char(self.cursor_y);
        line_start + self.cursor_x
    }

    pub fn insert_char(&mut self, c: char) {
        let idx = self.char_index();
        self.rope.insert_char(idx, c);
        self.cursor_x += 1;
        self.modified = true;
        self.on_buffer_modified();
    }

    pub fn insert_newline(&mut self) {
        let idx = self.char_index();
        self.rope.insert_char(idx, '\n');
        self.cursor_y += 1;
        self.cursor_x = 0;
        self.modified = true;
        self.on_buffer_modified();
    }

    pub fn backspace(&mut self) {
        if self.cursor_x > 0 {
            let idx = self.char_index();
            self.rope.remove(idx - 1..idx);
            self.cursor_x -= 1;
            self.modified = true;
            self.on_buffer_modified();
        } else if self.cursor_y > 0 {
            let prev_len = line_len(&self.rope, self.cursor_y - 1);
            let current_line_idx = self.rope.line_to_char(self.cursor_y);

            if current_line_idx > 0 {
                self.rope.remove(current_line_idx - 1..current_line_idx);
            }

            self.cursor_y -= 1;
            self.cursor_x = prev_len;
            self.modified = true;
            self.on_buffer_modified();
        }
    }

    pub fn delete_under_cursor(&mut self) {
        let line_len = self.current_line_len();
        if self.cursor_x < line_len {
            self.snapshot();
            let idx = self.char_index();
            self.rope.remove(idx..idx + 1);
            self.modified = true;
            self.on_buffer_modified();
        }
    }

    pub fn delete_current_line(&mut self) {
        if self.rope.len_lines() == 0 {
            return;
        }
        self.snapshot();
        let start = self.rope.line_to_char(self.cursor_y);
        let end = if self.cursor_y + 1 < self.rope.len_lines() {
            self.rope.line_to_char(self.cursor_y + 1)
        } else {
            self.rope.len_chars()
        };

        if start < end {
            self.rope.remove(start..end);
            self.modified = true;
            self.on_buffer_modified();
        }

        if self.cursor_y >= self.rope.len_lines() && self.cursor_y > 0 {
            self.cursor_y -= 1;
        }
        self.clamp_cursor();
    }

    pub fn save(&mut self) -> Result<()> {
        if let Some(path) = &self.path {
            let file = File::create(path)?;
            let mut writer = BufWriter::new(file);
            for chunk in self.rope.chunks() {
                writer.write_all(chunk.as_bytes())?;
            }
            writer.flush()?;
            self.modified = false;
            self.status_msg = format!("Saved {}", path.display());

            if let Some(tx) = &self.lsp_tx {
                let _ = tx.send(LspInbound::Save);
            }
        } else {
            self.status_msg = "No file name (use :w <name>)".to_string();
        }
        Ok(())
    }

    pub fn execute_command(&mut self) {
        let cmd = self.command_buffer.trim().to_string();
        self.command_buffer.clear();

        if cmd == "q" {
            if self.modified {
                self.status_msg = "Unsaved changes! Use :q! to force quit".to_string();
            } else {
                self.should_quit = true;
            }
        } else if cmd == "q!" {
            self.should_quit = true;
        } else if cmd == "w" {
            let _ = self.save();
        } else if cmd == "wq" {
            if self.save().is_ok() {
                self.should_quit = true;
            }
        } else if !cmd.is_empty() {
            self.status_msg = format!("Unknown command: :{}", cmd);
        }
    }

    pub fn clamp_cursor(&mut self) {
        let max_lines = self.rope.len_lines().max(1);
        if self.cursor_y >= max_lines {
            self.cursor_y = max_lines - 1;
        }

        let line_len = self.current_line_len();
        let max_x = match self.mode {
            Mode::Insert => line_len,
            Mode::Normal | Mode::Command => line_len.saturating_sub(1),
        };

        if self.cursor_x > max_x {
            self.cursor_x = max_x;
        }
    }

    pub fn update_scroll(&mut self, width: usize, height: usize) {
        if self.cursor_y < self.scroll_y {
            self.scroll_y = self.cursor_y;
        } else if self.cursor_y >= self.scroll_y + height {
            self.scroll_y = self.cursor_y - height + 1;
        }

        if self.cursor_x < self.scroll_x {
            self.scroll_x = self.cursor_x;
        } else if self.cursor_x >= self.scroll_x + width {
            self.scroll_x = self.cursor_x - width + 1;
        }
    }
}

fn line_len(rope: &Rope, line_idx: usize) -> usize {
    if line_idx >= rope.len_lines() {
        return 0;
    }
    let line = rope.line(line_idx);
    let mut len = line.len_chars();
    if len > 0 && line.char(len - 1) == '\n' {
        len -= 1;
        if len > 0 && line.char(len - 1) == '\r' {
            len -= 1;
        }
    }
    len
}

fn file_icon_and_color(path: Option<&PathBuf>) -> (&'static str, Color) {
    let ext = path
        .and_then(|p| p.extension())
        .and_then(|e| e.to_str())
        .unwrap_or("");

    match ext {
        "rs" => ("", Color::Rgb(235, 102, 60)),
        "py" => ("", Color::Rgb(255, 212, 59)),
        "md" => ("", Color::Rgb(120, 180, 255)),
        _ => ("󰈔", Color::Rgb(160, 165, 175)),
    }
}

fn set_terminal_cursor_style(mode: Mode) {
    let mut stdout = stdout();
    match mode {
        Mode::Normal | Mode::Command => {
            let _ = stdout.write_all(b"\x1b[2 q");
        }
        Mode::Insert => {
            let _ = stdout.write_all(b"\x1b[6 q");
        }
    }
    let _ = stdout.flush();
}

fn setup_panic_hook() {
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let mut out = stdout();
        let _ = out.write_all(b"\x1b[0 q");
        let _ = disable_raw_mode();
        let _ = execute!(out, LeaveAlternateScreen, DisableMouseCapture);
        hook(info);
    }));
}

// -----------------------------------------------------------------------------
// Application Lifecycle & Event Loop
// -----------------------------------------------------------------------------

#[tokio::main]
async fn main() -> Result<()> {
    setup_panic_hook();
    enable_raw_mode()?;
    let mut stdout = stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let target_path = env::args().nth(1).map(PathBuf::from);
    let mut editor = Editor::new(target_path.clone())?;

    let (lsp_out_tx, mut lsp_out_rx) = mpsc::unbounded_channel::<LspOutbound>();
    let (lsp_in_tx, lsp_in_rx) = mpsc::unbounded_channel::<LspInbound>();

    if let Some(path) = &target_path {
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        let (server_cmd, lang_id) = match ext {
            "rs" => (Some("rust-analyzer"), "rust"),
            "py" => (Some("pylsp"), "python"),
            _ => (None, ""),
        };

        if let Some(cmd) = server_cmd {
            editor.lsp_tx = Some(lsp_in_tx);
            let p = path.clone();
            let initial_text = editor.rope.to_string();
            tokio::spawn(run_lsp_actor(
                p,
                lang_id.to_string(),
                cmd.to_string(),
                lsp_in_rx,
                lsp_out_tx,
                initial_text,
            ));
        }
    }

    set_terminal_cursor_style(editor.mode);

    while !editor.should_quit {
        while let Ok(msg) = lsp_out_rx.try_recv() {
            match msg {
                LspOutbound::Status(s) => editor.lsp_status = s,
                LspOutbound::Diagnostics(d) => editor.diagnostics = d,
            }
        }

        terminal.draw(|f| render_ui(f, &mut editor))?;

        if event::poll(Duration::from_millis(20))? {
            match event::read()? {
                Event::Key(key) => handle_key_event(&mut editor, key),
                Event::Mouse(mouse) => handle_mouse_event(&mut editor, mouse, terminal.size()?),
                _ => {}
            }
        }
    }

    let mut out = std::io::stdout();
    let _ = out.write_all(b"\x1b[0 q");
    let _ = disable_raw_mode();
    execute!(terminal.backend_mut(), LeaveAlternateScreen, DisableMouseCapture)?;
    Ok(())
}

// -----------------------------------------------------------------------------
// Touch & Mouse Handling (Pixel-Aligned)
// -----------------------------------------------------------------------------

fn handle_mouse_event(editor: &mut Editor, mouse: MouseEvent, size: Size) {
    let gutter_digits = editor.rope.len_lines().max(1).to_string().len().max(2);
    let gutter_width = gutter_digits + 4; // Exactly matches renderer width

    let viewport_top = 1u16;
    let viewport_bottom = size.height.saturating_sub(3);
    let content_left = 1u16 + gutter_width as u16;

    match mouse.kind {
        MouseEventKind::Down(MouseButton::Left) | MouseEventKind::Drag(MouseButton::Left) => {
            if mouse.row == size.height.saturating_sub(2) && mouse.column <= 12 {
                editor.mode = match editor.mode {
                    Mode::Normal => Mode::Insert,
                    Mode::Insert => Mode::Normal,
                    Mode::Command => Mode::Normal,
                };
                set_terminal_cursor_style(editor.mode);
                return;
            }

            if mouse.row >= viewport_top && mouse.row < viewport_bottom {
                let target_line = editor.scroll_y + (mouse.row - viewport_top) as usize;
                if target_line < editor.rope.len_lines() {
                    editor.cursor_y = target_line;
                    if mouse.column >= content_left {
                        editor.cursor_x = editor.scroll_x + (mouse.column - content_left) as usize;
                    } else {
                        editor.cursor_x = 0;
                    }
                    editor.clamp_cursor();
                }
            }
        }
        MouseEventKind::ScrollUp => {
            editor.scroll_y = editor.scroll_y.saturating_sub(3);
            editor.cursor_y = editor.cursor_y.saturating_sub(3);
            editor.clamp_cursor();
        }
        MouseEventKind::ScrollDown => {
            if editor.scroll_y + 3 < editor.rope.len_lines() {
                editor.scroll_y += 3;
                editor.cursor_y = (editor.cursor_y + 3).min(editor.rope.len_lines().saturating_sub(1));
                editor.clamp_cursor();
            }
        }
        _ => {}
    }
}

// -----------------------------------------------------------------------------
// Keyboard Controller
// -----------------------------------------------------------------------------

fn handle_key_event(editor: &mut Editor, key: KeyEvent) {
    let prev_mode = editor.mode;

    match editor.mode {
        Mode::Normal => {
            if let Some(pending) = editor.pending_key.take() {
                match (pending, key.code) {
                    ('d', KeyCode::Char('d')) => editor.delete_current_line(),
                    ('g', KeyCode::Char('g')) => {
                        editor.cursor_y = 0;
                        editor.cursor_x = 0;
                    }
                    _ => {}
                }
                editor.clamp_cursor();
                return;
            }

            match key.code {
                KeyCode::Char('i') => {
                    editor.snapshot();
                    editor.mode = Mode::Insert;
                }
                KeyCode::Char('a') => {
                    editor.snapshot();
                    let line_len = editor.current_line_len();
                    if editor.cursor_x < line_len {
                        editor.cursor_x += 1;
                    }
                    editor.mode = Mode::Insert;
                }
                KeyCode::Char('o') => {
                    editor.snapshot();
                    editor.cursor_x = editor.current_line_len();
                    editor.insert_newline();
                    editor.mode = Mode::Insert;
                }
                KeyCode::Char('u') => editor.undo(),
                KeyCode::Char('d') => editor.pending_key = Some('d'),
                KeyCode::Char('g') => editor.pending_key = Some('g'),
                KeyCode::Char('G') => {
                    editor.cursor_y = editor.rope.len_lines().saturating_sub(1);
                    editor.cursor_x = 0;
                }
                KeyCode::Char(':') => {
                    editor.mode = Mode::Command;
                    editor.command_buffer.clear();
                }
                KeyCode::Char('h') | KeyCode::Left => {
                    editor.cursor_x = editor.cursor_x.saturating_sub(1);
                }
                KeyCode::Char('l') | KeyCode::Right => {
                    let max = editor.current_line_len().saturating_sub(1);
                    if editor.cursor_x < max {
                        editor.cursor_x += 1;
                    }
                }
                KeyCode::Char('k') | KeyCode::Up => {
                    editor.cursor_y = editor.cursor_y.saturating_sub(1);
                }
                KeyCode::Char('j') | KeyCode::Down => {
                    if editor.cursor_y + 1 < editor.rope.len_lines() {
                        editor.cursor_y += 1;
                    }
                }
                KeyCode::Char('x') => editor.delete_under_cursor(),
                _ => {}
            }
        }
        Mode::Insert => match key.code {
            KeyCode::Esc => {
                editor.mode = Mode::Normal;
                if editor.cursor_x > 0 && editor.cursor_x >= editor.current_line_len() {
                    editor.cursor_x = editor.cursor_x.saturating_sub(1);
                }
                editor.clamp_cursor();
            }
            KeyCode::Enter => editor.insert_newline(),
            KeyCode::Backspace => editor.backspace(),
            KeyCode::Tab => {
                for _ in 0..4 {
                    editor.insert_char(' ');
                }
            }
            KeyCode::Left => editor.cursor_x = editor.cursor_x.saturating_sub(1),
            KeyCode::Right => {
                if editor.cursor_x < editor.current_line_len() {
                    editor.cursor_x += 1;
                }
            }
            KeyCode::Up => editor.cursor_y = editor.cursor_y.saturating_sub(1),
            KeyCode::Down => {
                if editor.cursor_y + 1 < editor.rope.len_lines() {
                    editor.cursor_y += 1;
                }
            }
            KeyCode::Char(c) => editor.insert_char(c),
            _ => {}
        },
        Mode::Command => match key.code {
            KeyCode::Esc => {
                editor.mode = Mode::Normal;
                editor.command_buffer.clear();
            }
            KeyCode::Enter => {
                editor.execute_command();
                if editor.mode == Mode::Command {
                    editor.mode = Mode::Normal;
                }
            }
            KeyCode::Backspace => {
                if editor.command_buffer.pop().is_none() {
                    editor.mode = Mode::Normal;
                }
            }
            KeyCode::Char(c) => editor.command_buffer.push(c),
            _ => {}
        },
    }

    if prev_mode != editor.mode {
        set_terminal_cursor_style(editor.mode);
    }

    editor.clamp_cursor();
}

// -----------------------------------------------------------------------------
// Viewport & UI Render Pipeline
// -----------------------------------------------------------------------------

fn render_ui(frame: &mut Frame, editor: &mut Editor) {
    let size = frame.area();

    let main_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(3),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .split(size);

    let (icon, icon_color) = file_icon_and_color(editor.path.as_ref());
    let file_title = editor
        .path
        .as_ref()
        .map(|p| p.file_name().unwrap_or_default().to_string_lossy().to_string())
        .unwrap_or_else(|| "unnamed".into());

    let window_title = Line::from(vec![
        Span::raw(" "),
        Span::styled(format!("{} ", icon), Style::default().fg(icon_color)),
        Span::styled(file_title, Style::default().fg(Color::Rgb(220, 225, 235)).add_modifier(Modifier::BOLD)),
        if editor.modified {
            Span::styled(" ●", Style::default().fg(Color::Rgb(240, 100, 100)))
        } else {
            Span::raw("")
        },
        Span::raw(" "),
    ]);

    let rounded_block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::Rgb(55, 60, 75)))
        .title(window_title);

    let inner_area = rounded_block.inner(main_chunks[0]);
    frame.render_widget(rounded_block, main_chunks[0]);

    // Gutter Geometry: 1 (indicator) + line_digits + 3 (" │ ") = line_digits + 4
    let total_lines = editor.rope.len_lines().max(1);
    let line_digits = total_lines.to_string().len().max(2);
    let gutter_width = line_digits + 4;
    let text_area_width = (inner_area.width as usize).saturating_sub(gutter_width);

    editor.update_scroll(text_area_width, inner_area.height as usize);

    let mut visible_lines = Vec::new();
    let start_line = editor.scroll_y;
    let end_line = (start_line + inner_area.height as usize).min(editor.rope.len_lines());

    for y in start_line..end_line {
        let is_current = y == editor.cursor_y;

        let line_diag = editor.diagnostics.iter().find(|d| d.line == y);
        let (diag_marker, diag_style) = match line_diag.map(|d| d.severity) {
            Some(1) => ("", Style::default().fg(Color::Rgb(240, 90, 90))),
            Some(2) => ("", Style::default().fg(Color::Rgb(240, 180, 70))),
            Some(_) => ("󰌵", Style::default().fg(Color::Rgb(100, 180, 255))),
            None => (" ", Style::default()),
        };

        let gutter_style = if is_current {
            Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::Rgb(80, 85, 100))
        };

        let line = editor.rope.line(y);
        let mut line_str = line.to_string();
        if line_str.ends_with('\n') {
            line_str.pop();
            if line_str.ends_with('\r') {
                line_str.pop();
            }
        }

        let scrolled_line: String = line_str.chars().skip(editor.scroll_x).collect();

        let mut spans = vec![
            Span::styled(diag_marker, diag_style),
            Span::styled(format!("{:>width$} │ ", y + 1, width = line_digits), gutter_style),
        ];
        spans.extend(editor.syntax.highlight_line(&scrolled_line, y));

        visible_lines.push(Line::from(spans));
    }

    for _ in visible_lines.len()..inner_area.height as usize {
        visible_lines.push(Line::from(vec![
            Span::raw(" "),
            Span::styled(
                format!("{:>width$} │ ", "~", width = line_digits),
                Style::default().fg(Color::Rgb(50, 55, 68)),
            ),
        ]));
    }

    frame.render_widget(Paragraph::new(visible_lines), inner_area);

    // Powerline Statusline
    let (badge_text, badge_color) = match editor.mode {
        Mode::Normal => (" NORMAL ", Color::Rgb(80, 140, 255)),
        Mode::Insert => (" INSERT ", Color::Rgb(70, 200, 120)),
        Mode::Command => (" COMMAND ", Color::Rgb(220, 100, 240)),
    };

    let bar_bg = Color::Rgb(20, 22, 28);
    let pill_bg = Color::Rgb(35, 38, 48);
    let bar_fg = Color::Rgb(200, 205, 220);

    let error_count = editor.diagnostics.iter().filter(|d| d.severity == 1).count();
    let warn_count = editor.diagnostics.iter().filter(|d| d.severity == 2).count();

    let status_left = Line::from(vec![
        Span::styled(
            badge_text,
            Style::default().bg(badge_color).fg(Color::Rgb(15, 17, 22)).add_modifier(Modifier::BOLD),
        ),
        Span::styled("", Style::default().bg(pill_bg).fg(badge_color)),
        Span::styled(
            format!("  {} ", match editor.syntax.language {
                SupportedLanguage::Rust => "Rust",
                SupportedLanguage::Python => "Python",
                SupportedLanguage::Markdown => "Markdown",
                SupportedLanguage::Plain => "Text",
            }),
            Style::default().bg(pill_bg).fg(bar_fg),
        ),
        Span::styled("", Style::default().bg(bar_bg).fg(pill_bg)),
    ]);

    let lsp_badge = match &editor.lsp_status {
        LspStatus::Ready(name) => {
            if error_count > 0 || warn_count > 0 {
                Span::styled(
                    format!("  {}  {} ", error_count, warn_count),
                    Style::default().bg(bar_bg).fg(Color::Rgb(240, 90, 90)),
                )
            } else {
                Span::styled(format!(" 󰄬 {} ", name), Style::default().bg(bar_bg).fg(Color::Rgb(100, 180, 120)))
            }
        }
        LspStatus::Starting(name) => Span::styled(
            format!(" 󰑮 {} init... ", name),
            Style::default().bg(bar_bg).fg(Color::Rgb(100, 180, 240)),
        ),
        LspStatus::NotFound(name) => Span::styled(
            format!(" 󰅚 {} not found ", name),
            Style::default().bg(bar_bg).fg(Color::Rgb(240, 140, 60)),
        ),
        LspStatus::Error(_) => Span::styled(" 󰅚 LSP err ", Style::default().bg(bar_bg).fg(Color::Rgb(240, 90, 90))),
        LspStatus::Disabled => Span::styled(" 󰌵 plain ", Style::default().bg(bar_bg).fg(Color::Rgb(100, 105, 120))),
    };

    let status_right = Line::from(vec![
        lsp_badge,
        Span::styled("", Style::default().bg(bar_bg).fg(pill_bg)),
        Span::styled(
            format!("  {}L ", total_lines),
            Style::default().bg(pill_bg).fg(Color::Rgb(160, 165, 180)),
        ),
        Span::styled("", Style::default().bg(pill_bg).fg(badge_color)),
        Span::styled(
            format!(" 󰆤 {}:{} ", editor.cursor_y + 1, editor.cursor_x + 1),
            Style::default().bg(badge_color).fg(Color::Rgb(15, 17, 22)).add_modifier(Modifier::BOLD),
        ),
    ]);

    frame.render_widget(Block::default().style(Style::default().bg(bar_bg)), main_chunks[1]);
    frame.render_widget(Paragraph::new(status_left), main_chunks[1]);
    frame.render_widget(
        Paragraph::new(status_right).alignment(ratatui::layout::Alignment::Right),
        main_chunks[1],
    );

    // Diagnostics / Notifications Bar
    if editor.mode == Mode::Command {
        let prompt_line = Line::from(vec![
            Span::styled(" :", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::raw(&editor.command_buffer),
        ]);
        frame.render_widget(Paragraph::new(prompt_line), main_chunks[2]);
        frame.set_cursor_position(Position::new(
            (2 + editor.command_buffer.len()) as u16,
            main_chunks[2].y,
        ));
    } else {
        let active_diag = editor.diagnostics.iter().find(|d| d.line == editor.cursor_y);
        let msg_line = if let Some(diag) = active_diag {
            Line::from(vec![
                Span::styled("  ", Style::default().fg(Color::Rgb(240, 90, 90))),
                Span::styled(&diag.message, Style::default().fg(Color::Rgb(230, 235, 245)).add_modifier(Modifier::ITALIC)),
            ])
        } else {
            Line::from(vec![
                Span::styled(" 󰅂 ", Style::default().fg(Color::DarkGray)),
                Span::styled(&editor.status_msg, Style::default().fg(Color::Rgb(170, 175, 190))),
            ])
        };

        frame.render_widget(Paragraph::new(msg_line), main_chunks[2]);

        // Place terminal cursor matching the rendered text area
        let screen_x = inner_area.x + gutter_width as u16 + (editor.cursor_x.saturating_sub(editor.scroll_x)) as u16;
        let screen_y = inner_area.y + (editor.cursor_y.saturating_sub(editor.scroll_y)) as u16;

        if screen_x < inner_area.right() && screen_y < inner_area.bottom() {
            frame.set_cursor_position(Position::new(screen_x, screen_y));
        }
    }
}
