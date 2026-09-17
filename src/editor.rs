//! # Editor Buffer Model & Application State Engine
//!
//! This module forms the central operational core of `subject0`. It manages:
//!
//! 1. **Text Storage & Mutability ([`ropey::Rope`])**:
//!    Buffer contents are stored as a chunked, reference-counted B-tree rope. Modifications,
//!    insertions, and deletions execute in $O(\log N)$ time, and document snapshots for undo
//!    history leverage copy-on-write structural sharing, making state capture $O(1)$ in time
//!    and minimal in memory overhead.
//!
//! 2. **Modal Editing State Machine ([`Mode`])**:
//!    Implements modal key semantics across four distinct states:
//!    - [`Mode::Normal`]: Motion, operational commands, and dispatch. Cursor clamps to `line_len - 1`.
//!    - [`Mode::Insert`]: Direct character streaming, pair-matching, and auto-indentation. Cursor clamps to `line_len`.
//!    - [`Mode::Command`]: Ex-style command-line input buffer (e.g., `:w`, `:q`, `:explore`).
//!    - [`Mode::Visual`]: 2D anchor-to-cursor selection highlighting and bulk manipulation.
//!
//! 3. **Undo History Stack**:
//!    Maintains a ring-bounded history of historical snapshots paired with cursor coordinates
//!    (capped at 64 entries).
//!
//! 4. **Document & LSP Synchronization**:
//!    Every buffer mutation increments `doc_version`, synchronizes AST highlights via
//!    [`SyntaxEngine::reparse`], and dispatches full-text LSP change notifications
//!    (`textDocument/didChange`) through an unbounded Tokio channel.
//!
//! 5. **Subsystem Overlays**:
//!    Coordinates the built-in tree-structured file explorer ([`FileExplorer`]) and fuzzy-filtered
//!    command palette ([`CommandPalette`]).

use std::{
    env,
    fs::{self, File},
    io::{BufWriter, Write},
    path::{Path, PathBuf},
};

use crate::lsp::{
    DiagnosticItem, LspInbound, LspOutbound, LspStatus, SuggestionItem, SyntaxEngine, run_lsp_actor,
};

use anyhow::{Result, anyhow};
use ropey::Rope;
use tokio::sync::mpsc;

use serde_json::{Value, json};
use std::collections::HashMap;

/// Persistent editor configuration stored in `.subject0`.
#[derive(Clone, Debug)]
pub struct AppConfig {
    pub preferred_lsps: HashMap<String, String>,
    pub line_wrap: bool,
    /// Resolved absolute path to the `.subject0` file this config was loaded
    /// from (or will be written to). Fixed at load time so that `save()` always
    /// targets the same file as `load()` did, regardless of the process's
    /// current working directory at save time.
    pub source_path: PathBuf,
}

impl AppConfig {
    /// Resolves the `.subject0` config path anchored to a specific project
    /// root, rather than the ambient process working directory.
    fn resolve_path(project_root: &Path) -> PathBuf {
        let project_cfg = project_root.join(".subject0");
        if project_cfg.exists() {
            return project_cfg;
        }
        let home = env::var_os("HOME").or_else(|| env::var_os("USERPROFILE"));
        if let Some(h) = home {
            let home_cfg = PathBuf::from(h).join(".subject0");
            if home_cfg.exists() {
                return home_cfg;
            }
        }
        project_cfg
    }

    /// Loads configuration anchored to `project_root`.
    pub fn load_from(project_root: &Path) -> Self {
        let path = Self::resolve_path(project_root);
        let mut preferred_lsps = HashMap::new();
        let mut line_wrap = true;

        if let Ok(content) = fs::read_to_string(&path)
            && let Ok(val) = serde_json::from_str::<Value>(&content)
        {
            if let Some(obj) = val.get("preferred_lsps").and_then(Value::as_object) {
                for (k, v) in obj {
                    if let Some(s) = v.as_str() {
                        preferred_lsps.insert(k.clone(), s.to_string());
                    }
                }
            }
            if let Some(w) = val.get("line_wrap").and_then(Value::as_bool) {
                line_wrap = w;
            }
        }

        Self {
            preferred_lsps,
            line_wrap,
            source_path: path,
        }
    }

    /// Saves configuration while preserving any external or unrecognized fields in `.subject0`.
    pub fn save(&self) -> Result<()> {
        let mut val = if let Ok(content) = fs::read_to_string(&self.source_path)
            && let Ok(existing) = serde_json::from_str::<Value>(&content)
        {
            existing
        } else {
            json!({})
        };

        if let Some(obj) = val.as_object_mut() {
            obj.insert("preferred_lsps".to_string(), json!(self.preferred_lsps));
            obj.insert("line_wrap".to_string(), json!(self.line_wrap));
        } else {
            val = json!({
                "preferred_lsps": self.preferred_lsps,
                "line_wrap": self.line_wrap,
            });
        }

        if let Some(parent) = self.source_path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        fs::write(&self.source_path, serde_json::to_string_pretty(&val)?)?;
        Ok(())
    }
}

/// Interactive modal state when multiple LSPs are detected.
pub struct LspPicker {
    pub language_id: String,
    pub candidates: Vec<String>,
    pub selected_idx: usize,
}

/// Active input mode governing key event interpretation and cursor boundary constraints.
#[derive(PartialEq, Eq, Clone, Copy, Debug)]
pub enum Mode {
    /// Navigation and command execution mode. Cursor rests on existing glyphs (`x <= len - 1`).
    Normal,
    /// Direct text insertion mode. Cursor may advance past the final glyph (`x <= len`).
    Insert,
    /// Ex-style command input mode active on the bottom status line.
    Command,
    /// Text selection mode active between a fixed anchor coordinate and the moving cursor coordinate.
    Visual {
        /// Zero-based character/column offset where the selection originated.
        anchor_x: usize,
        /// Zero-based line index where the selection originated.
        anchor_y: usize,
    },
}

/// Identifies which viewport element currently holds keyboard input focus.
#[derive(PartialEq, Eq, Clone, Copy, Debug)]
pub enum Focus {
    /// Main code buffer viewport.
    Editor,
    /// Side-panel file tree explorer viewport.
    Explorer,
}

// === Command Palette Definitions ===

/// Enumeration of all discrete operations dispatchable via the interactive command palette.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CommandId {
    /// Selects the entire text document buffer into a visual range.
    SelectAll,
    /// Toggles visibility of the sidebar file browser.
    ToggleExplorer,
    /// Toggles viewport soft line-wrapping on or off.
    ToggleWrap,
    /// Writes modified buffer state to the underlying filesystem path.
    Save,
    /// Writes modified buffer state and flags the editor to exit.
    SaveQuit,
    /// Terminates the editor process immediately, discarding unstaged modifications.
    QuitForce,
    /// Switches the buffer mode to visual selection at the current cursor coordinate.
    EnterVisual,
    /// Inserts a newline beneath the current cursor line and switches to insert mode.
    InsertBelow,
    /// Inserts a newline above the current cursor line and switches to insert mode.
    InsertAbove,
    /// Joins the subsequent line onto the current line, collapsing whitespace.
    JoinLines,
    /// Reverts the document rope to the most recent undo snapshot.
    Undo,
    /// Copies the active visual selection or current line to the internal clipboard.
    Yank,
    /// Swaps character casing (uppercase <-> lowercase) over selection or character.
    ToggleCase,
    /// Pastes text from the internal clipboard at the current cursor offset.
    Paste,
    /// Relocates the cursor to the first character of the first line.
    JumpTop,
    /// Relocates the cursor to the first character of the final line.
    JumpBottom,
    /// Dispatches an asynchronous LSP completion request at the current cursor position.
    TriggerCompletion,
    /// Shows picker to select active LSP server for current file.
    SelectLsp,
    /// Save Config to .subject0
    SaveConfig,
    /// Displays keybindings and user guide modal.
    ShowHelp,
}

/// Static descriptor defining metadata for a searchable command palette entry.
#[derive(Clone)]
pub struct PaletteCommand {
    /// Human-readable title displayed in the palette candidate list.
    pub title: &'static str,
    /// Suggested keyboard shortcut or ex-command displayed as secondary UI hint.
    pub shortcut: &'static str,
    /// Nerd Font icon glyph prefixed to the title.
    pub icon: &'static str,
    /// Target command ID executed when this palette entry is activated.
    pub id: CommandId,
}

/// Static registry of available editor commands exposed through the command palette.
pub static PALETTE_COMMANDS: &[PaletteCommand] = &[
    PaletteCommand {
        title: "Select All Buffer",
        shortcut: "%",
        icon: "󰈙",
        id: CommandId::SelectAll,
    },
    PaletteCommand {
        title: "Toggle File Explorer",
        shortcut: "Ctrl-E / :e",
        icon: "󰉓",
        id: CommandId::ToggleExplorer,
    },
    PaletteCommand {
        title: "Toggle Line Wrap",
        shortcut: ":wrap",
        icon: "󰖶",
        id: CommandId::ToggleWrap,
    },
    PaletteCommand {
        title: "Save Buffer",
        shortcut: ":w",
        icon: "󰆓",
        id: CommandId::Save,
    },
    PaletteCommand {
        title: "Save and Quit",
        shortcut: ":wq",
        icon: "󰆘",
        id: CommandId::SaveQuit,
    },
    PaletteCommand {
        title: "Force Quit",
        shortcut: ":q!",
        icon: "󰈆",
        id: CommandId::QuitForce,
    },
    PaletteCommand {
        title: "Enter Visual Selection",
        shortcut: "v",
        icon: "󰒅",
        id: CommandId::EnterVisual,
    },
    PaletteCommand {
        title: "Insert Line Below",
        shortcut: "o",
        icon: "󰙍",
        id: CommandId::InsertBelow,
    },
    PaletteCommand {
        title: "Insert Line Above",
        shortcut: "O",
        icon: "󰙌",
        id: CommandId::InsertAbove,
    },
    PaletteCommand {
        title: "Join Lines",
        shortcut: "J",
        icon: "󰘥",
        id: CommandId::JoinLines,
    },
    PaletteCommand {
        title: "Undo Change",
        shortcut: "u",
        icon: "󰑌",
        id: CommandId::Undo,
    },
    PaletteCommand {
        title: "Yank Selection / Line",
        shortcut: "y",
        icon: "󰅍",
        id: CommandId::Yank,
    },
    PaletteCommand {
        title: "Paste from Clipboard",
        shortcut: "p",
        icon: "󰅒",
        id: CommandId::Paste,
    },
    PaletteCommand {
        title: "Toggle Case (Upper/Lower)",
        shortcut: "~",
        icon: "󰬴",
        id: CommandId::ToggleCase,
    },
    PaletteCommand {
        title: "Jump to Top of File",
        shortcut: "gg",
        icon: "󰗈",
        id: CommandId::JumpTop,
    },
    PaletteCommand {
        title: "Jump to Bottom of File",
        shortcut: "G / ge",
        icon: "󰗈",
        id: CommandId::JumpBottom,
    },
    PaletteCommand {
        title: "Trigger AI / LSP Completions",
        shortcut: "Ctrl-Space",
        icon: "󰌵",
        id: CommandId::TriggerCompletion,
    },
    PaletteCommand {
        title: "Select Active LSP Server",
        shortcut: ":lsp",
        icon: "",
        id: CommandId::SelectLsp,
    },
    PaletteCommand {
        title: "Save Config to .subject0",
        shortcut: ":cfg",
        icon: "󰄛",
        id: CommandId::SaveConfig,
    },
    PaletteCommand {
        title: "Show Keybindings & Help",
        shortcut: "? / :help",
        icon: "󰋖",
        id: CommandId::ShowHelp,
    },
];

/// Transient UI state for the fuzzy-filtered command palette overlay.
pub struct CommandPalette {
    /// Indicates whether the palette popup overlay is currently rendered.
    pub visible: bool,
    /// User query string used to filter commands.
    pub query: String,
    /// Index of the currently highlighted candidate within the filtered results.
    pub selected_idx: usize,
    /// Vertical viewport scroll offset for the rendered candidate list.
    pub scroll: usize,
}

impl CommandPalette {
    pub fn new() -> Self {
        Self {
            visible: false,
            query: String::new(),
            selected_idx: 0,
            scroll: 0,
        }
    }

    pub fn filtered_commands(&self) -> Vec<&'static PaletteCommand> {
        let q = self.query.to_lowercase();
        PALETTE_COMMANDS
            .iter()
            .filter(|cmd| {
                q.is_empty()
                    || cmd.title.to_lowercase().contains(&q)
                    || cmd.shortcut.to_lowercase().contains(&q)
            })
            .collect()
    }
}

// File Explorer

/// Represents a single node (file or directory) within the hierarchical file tree.
#[derive(Clone, Debug)]
pub struct FileEntry {
    pub path: PathBuf,
    pub name: String,
    pub is_dir: bool,
    pub depth: usize,
    pub expanded: bool,
}

/// Sidebar file tree model supporting dynamic directory expansion and navigation.
pub struct FileExplorer {
    pub root: PathBuf,
    pub entries: Vec<FileEntry>,
    pub selected_idx: usize,
    pub scroll: usize,
    pub visible: bool,
}

impl FileExplorer {
    pub fn new<P: AsRef<Path>>(root: P) -> Self {
        let root_buf = root.as_ref().to_path_buf();
        let mut explorer = Self {
            root: root_buf.clone(),
            entries: Vec::new(),
            selected_idx: 0,
            scroll: 0,
            visible: false,
        };
        explorer.refresh();
        explorer
    }

    pub fn refresh(&mut self) {
        self.entries = Self::read_directory(&self.root, 0);
    }

    fn read_directory(dir: &Path, depth: usize) -> Vec<FileEntry> {
        let mut entries = Vec::new();
        if let Ok(read_dir) = fs::read_dir(dir) {
            let mut paths: Vec<PathBuf> = read_dir
                .filter_map(|res| res.ok().map(|e| e.path()))
                .filter(|p| {
                    let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
                    !name.starts_with('.') && name != "target" && name != "node_modules"
                })
                .collect();

            paths.sort_by(|a, b| {
                let a_is_dir = a.is_dir();
                let b_is_dir = b.is_dir();
                if a_is_dir && !b_is_dir {
                    std::cmp::Ordering::Less
                } else if !a_is_dir && b_is_dir {
                    std::cmp::Ordering::Greater
                } else {
                    a.file_name().cmp(&b.file_name())
                }
            });

            for p in paths {
                let is_dir = p.is_dir();
                let name = p
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("?")
                    .to_string();

                entries.push(FileEntry {
                    path: p,
                    name,
                    is_dir,
                    depth,
                    expanded: false,
                });
            }
        }
        entries
    }

    pub fn toggle_expand(&mut self, idx: usize) {
        if idx >= self.entries.len() || !self.entries[idx].is_dir {
            return;
        }

        if self.entries[idx].expanded {
            self.entries[idx].expanded = false;
            let current_depth = self.entries[idx].depth;
            let mut remove_count = 0;
            for entry in self.entries.iter().skip(idx + 1) {
                if entry.depth > current_depth {
                    remove_count += 1;
                } else {
                    break;
                }
            }
            self.entries.drain(idx + 1..idx + 1 + remove_count);

            if self.selected_idx > idx && self.selected_idx <= idx + remove_count {
                self.selected_idx = idx;
            } else if self.selected_idx > idx + remove_count {
                self.selected_idx = self.selected_idx.saturating_sub(remove_count);
            }

            if !self.entries.is_empty() && self.selected_idx >= self.entries.len() {
                self.selected_idx = self.entries.len().saturating_sub(1);
            }
            if self.scroll >= self.entries.len() {
                self.scroll = self.entries.len().saturating_sub(1);
            }
        } else {
            self.entries[idx].expanded = true;
            let current_depth = self.entries[idx].depth;
            let dir_path = self.entries[idx].path.clone();
            let children = Self::read_directory(&dir_path, current_depth + 1);

            let insert_pos = idx + 1;
            self.entries.splice(insert_pos..insert_pos, children);
        }
    }

    pub fn update_scroll(&mut self, viewport_height: usize) {
        if self.selected_idx < self.scroll {
            self.scroll = self.selected_idx;
        } else if self.selected_idx >= self.scroll + viewport_height {
            self.scroll = self.selected_idx + 1 - viewport_height;
        }
    }
}

// === Undo Snapshot State ===

/// Historic checkpoint capturing buffer text alongside cursor coordinates.
#[derive(Clone, Debug)]
pub struct UndoSnapshot {
    pub rope: Rope,
    pub cursor_x: usize,
    pub cursor_y: usize,
}

// === Editor Buffer Model ===

/// Primary application state model encapsulating buffer data, UI state, and subsystems.
#[allow(clippy::struct_excessive_bools)]
pub struct Editor {
    /// B-tree rope storing the buffer text contents.
    pub rope: Rope,
    /// Absolute or relative path to the currently open file on disk (`None` for scratch buffers).
    pub path: Option<PathBuf>,
    /// Active modal editing mode.
    pub mode: Mode,
    /// Current input focus target (main editor viewport or sidebar explorer).
    pub focus: Focus,
    /// Zero-based character/column offset of the cursor on the current line.
    pub cursor_x: usize,
    /// Zero-based line index of the cursor within the rope.
    pub cursor_y: usize,
    /// Horizontal scroll offset (used when line wrapping is disabled).
    pub scroll_x: usize,
    /// Vertical scroll offset representing the top visible line index.
    pub scroll_y: usize,
    /// Determines whether lines wider than the viewport wrap or scroll horizontally.
    pub line_wrap: bool,
    /// Internal yank/cut buffer used for clipboard operations.
    pub clipboard: String,
    /// Dirty flag tracking whether the rope contents diverge from disk storage.
    pub modified: bool,
    /// Informational string rendered on the bottom status line.
    pub status_msg: String,
    /// Accumulator for commands typed while in [`Mode::Command`].
    pub command_buffer: String,
    /// Buffered multi-key sequence prefix (e.g., initial `'g'` in a `"gg"` motion).
    pub pending_key: Option<char>,
    /// Undo history ring storing previous state snapshots (bounded to 64 snapshots).
    pub undo_stack: Vec<UndoSnapshot>,
    /// Tracks if an undo checkpoint has been recorded for the ongoing continuous insert run.
    pub insert_snapshot_taken: bool,
    /// Syntax engine instance driving AST parsing and lexical syntax highlighting.
    pub syntax: SyntaxEngine,
    /// Diagnostics received from the background LSP server mapped to the active document.
    pub diagnostics: Vec<DiagnosticItem>,
    /// Process lifecycle status of the associated LSP server.
    pub lsp_status: LspStatus,
    /// Transmission channel dispatching requests to the background LSP actor.
    pub lsp_tx: Option<mpsc::UnboundedSender<LspInbound>>,
    /// Receiver channel forwarder for LSP outbound events.
    pub lsp_out_tx: Option<mpsc::UnboundedSender<LspOutbound>>,
    /// Animation tick counter for UI spinners.
    pub spinner_tick: usize,
    /// Monotonically increasing request ID counter for correlating LSP responses.
    pub lsp_req_id: i64,

    /// Monotonically increasing document version counter sent in LSP change events.
    pub doc_version: i64,

    // Completions State
    /// Active list of completion suggestions returned by the LSP.
    pub completions: Vec<SuggestionItem>,
    /// Index of the highlighted completion item in [`Self::completions`].
    pub completion_idx: usize,
    /// Scroll offset of the completion popup list.
    pub completion_scroll: usize,
    /// Controls whether the completion popup window is visible.
    pub completion_visible: bool,
    /// Absolute rendered screen boundary of the completion popup (x, y, width, height).
    pub completion_rect: Option<(u16, u16, u16, u16)>,
    /// Tracks the language identifier for the currently spawned LSP session.
    pub active_lsp_lang: Option<String>,

    // File Tree & Command Palette
    /// File tree explorer state machine.
    pub explorer: FileExplorer,
    /// Interactive command palette state machine.
    pub palette: CommandPalette,
    /// Whether the in-editor keybinding help modal is open.
    pub show_help: bool,
    /// Viewport vertical scroll for the help modal.
    pub help_scroll: usize,
    /// Flag signaling the main application event loop to shut down.
    pub should_quit: bool,

    /// Persistent configuration loaded from `.subject0`.
    pub config: AppConfig,
    /// LSP server selection modal state.
    pub lsp_picker: Option<LspPicker>,
}

impl Editor {
    /// Instantiates a new editor buffer.
    pub fn new(path: Option<&PathBuf>) -> Result<Self> {
        let is_dir_target = path.is_some_and(|p| p.is_dir());

        let (rope, buffer_path, status_msg) = match path {
            Some(p) if p.is_dir() => (Rope::new(), None, format!("Opened folder {}", p.display())),
            Some(p) if p.exists() => {
                let file = File::open(p)?;
                (
                    Rope::from_reader(file)?,
                    Some(p.clone()),
                    format!("Loaded {}", p.display()),
                )
            }
            Some(p) => (
                Rope::new(),
                Some(p.clone()),
                format!("New: {}", p.display()),
            ),
            None => (Rope::new(), None, "Ready".to_string()),
        };

        let mut syntax = SyntaxEngine::new(buffer_path.as_ref());
        let text = rope.to_string();
        syntax.reparse(&text);

        let root_dir = if is_dir_target {
            path.cloned().unwrap()
        } else {
            env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
        };

        let mut explorer = FileExplorer::new(root_dir.clone());
        if is_dir_target {
            explorer.visible = true;
        }

        let config = AppConfig::load_from(&root_dir);
        let initial_wrap = config.line_wrap;

        Ok(Self {
            rope,
            path: buffer_path,
            mode: Mode::Normal,
            focus: if is_dir_target {
                Focus::Explorer
            } else {
                Focus::Editor
            },
            cursor_x: 0,
            cursor_y: 0,
            scroll_x: 0,
            scroll_y: 0,
            line_wrap: initial_wrap,
            config,
            lsp_picker: None,

            clipboard: String::new(),
            modified: false,
            status_msg,
            command_buffer: String::new(),
            pending_key: None,
            undo_stack: Vec::new(),
            insert_snapshot_taken: false,
            syntax,
            diagnostics: Vec::new(),
            lsp_status: LspStatus::Disabled,
            lsp_tx: None,
            lsp_out_tx: None,
            spinner_tick: 0,
            lsp_req_id: 10,

            doc_version: 1,
            completions: Vec::new(),
            completion_idx: 0,
            completion_scroll: 0,
            completion_visible: false,
            completion_rect: None,
            active_lsp_lang: None,
            explorer,
            palette: CommandPalette::new(),
            show_help: false,
            help_scroll: 0,
            should_quit: false,
        })
    }

    /// Sets the active mode and manages transactional insert checkpoints.
    pub fn set_mode(&mut self, mode: Mode) {
        if self.mode != mode {
            if matches!(mode, Mode::Insert) {
                self.insert_snapshot_taken = false;
            }
            self.mode = mode;
            self.clamp_cursor();
        }
    }

    /// Ensures an undo snapshot is taken on the first edit of an insertion sequence.
    fn check_insert_snapshot(&mut self) {
        if !self.insert_snapshot_taken {
            self.snapshot();
            self.insert_snapshot_taken = true;
        }
    }

    /// Detects whether the active buffer uses CRLF (`\r\n`) or LF (`\n`) line terminators.
    pub fn detect_line_ending(&self) -> &'static str {
        let sample_lines = self.rope.len_lines().min(64);
        for i in 0..sample_lines {
            let line = self.rope.line(i);
            let len = line.len_chars();
            if len >= 2 && line.char(len - 1) == '\n' && line.char(len - 2) == '\r' {
                return "\r\n";
            }
        }
        "\n"
    }

    /// Translates `cursor_x` into a UTF-16 code unit offset for standard LSP compatibility.
    pub fn cursor_utf16_col(&self) -> usize {
        if self.cursor_y >= self.rope.len_lines() {
            return 0;
        }
        let line = self.rope.line(self.cursor_y);
        let mut utf16_count = 0;
        for (char_idx, ch) in line.chars().enumerate() {
            if char_idx >= self.cursor_x {
                break;
            }
            utf16_count += ch.len_utf16();
        }
        utf16_count
    }

    /// Loads a new file from disk into the current editor buffer.
    pub fn open_file<P: AsRef<Path>>(&mut self, path: P) -> Result<()> {
        let path_buf = path.as_ref().to_path_buf();
        let file = File::open(&path_buf)?;
        self.rope = Rope::from_reader(file)?;
        self.path = Some(path_buf.clone());
        self.cursor_x = 0;
        self.cursor_y = 0;
        self.scroll_x = 0;
        self.scroll_y = 0;
        self.modified = false;
        self.undo_stack.clear();
        self.insert_snapshot_taken = false;
        self.diagnostics.clear();
        self.completion_visible = false;

        self.syntax = SyntaxEngine::new(Some(&path_buf));
        let text = self.rope.to_string();
        self.syntax.reparse(&text);
        self.doc_version = 1;

        let lang_id = self.syntax.language.lsp_id();
        if !lang_id.is_empty() && lang_id != "plaintext" {
            if self.lsp_tx.is_some() && self.active_lsp_lang.as_deref() == Some(lang_id) {
                if let Some(tx) = &self.lsp_tx {
                    let _ = tx.send(LspInbound::OpenFile {
                        path: path_buf.clone(),
                        text,
                        lang_id: lang_id.to_string(),
                    });
                    self.request_semantic_tokens();
                }
            } else {
                self.ensure_lsp_for_file(&path_buf);
            }
        } else {
            self.lsp_tx = None;
            self.active_lsp_lang = None;
            self.lsp_status = LspStatus::Disabled;
        }

        self.status_msg = format!("Opened {}", path_buf.display());
        Ok(())
    }

    /// Updates internal path mapping and notifies or spawns LSP instances accordingly.
    pub fn switch_file_target(&mut self, new_path: PathBuf) {
        let abs_path = if new_path.is_absolute() {
            new_path
        } else if let Ok(cwd) = env::current_dir() {
            cwd.join(new_path)
        } else {
            new_path
        };

        self.path = Some(abs_path.clone());
        self.syntax = SyntaxEngine::new(Some(&abs_path));
        let text = self.rope.to_string();
        self.syntax.reparse(&text);

        let lang_id = self.syntax.language.lsp_id();
        if !lang_id.is_empty() && lang_id != "plaintext" {
            if self.lsp_tx.is_some() && self.active_lsp_lang.as_deref() == Some(lang_id) {
                if let Some(tx) = &self.lsp_tx {
                    let _ = tx.send(LspInbound::OpenFile {
                        path: abs_path,
                        text,
                        lang_id: lang_id.to_string(),
                    });
                    self.request_semantic_tokens();
                }
            } else {
                self.ensure_lsp_for_file(&abs_path);
            }
        }
    }

    /// Spawns or configures an LSP server instance matching the target file.
    pub fn ensure_lsp_for_file(&mut self, path: &Path) {
        let lang = self.syntax.language;
        let lang_id = lang.lsp_id();
        if lang_id.is_empty() || lang_id == "plaintext" {
            self.lsp_tx = None;
            self.active_lsp_lang = None;
            self.lsp_status = LspStatus::Disabled;
            return;
        }

        let installed = lang.installed_servers();
        if installed.is_empty() {
            self.lsp_tx = None;
            self.active_lsp_lang = None;
            let candidates = lang.candidate_servers();
            if let Some(first) = candidates.first() {
                self.lsp_status = LspStatus::NotFound((*first).to_string());
            } else {
                self.lsp_status = LspStatus::Disabled;
            }
            return;
        }

        let preferred = self.config.preferred_lsps.get(lang_id).cloned();
        let chosen_server = if let Some(pref) = preferred.filter(|p| installed.contains(p)) {
            Some(pref)
        } else if installed.len() == 1 {
            Some(installed[0].clone())
        } else {
            self.lsp_picker = Some(LspPicker {
                language_id: lang_id.to_string(),
                candidates: installed.clone(),
                selected_idx: 0,
            });
            None
        };

        if let Some(cmd) = chosen_server {
            self.start_lsp_server(path, lang_id, &cmd);
        }
    }

    /// Spawns the background LSP actor task for a specific language and executable.
    pub fn start_lsp_server(&mut self, path: &Path, lang_id: &str, cmd: &str) {
        if let Some(out_tx) = &self.lsp_out_tx {
            let (in_tx, in_rx) = mpsc::unbounded_channel::<LspInbound>();
            self.lsp_tx = Some(in_tx);
            self.active_lsp_lang = Some(lang_id.to_string());
            let p = path.to_path_buf();
            let initial_text = self.rope.to_string();
            tokio::spawn(run_lsp_actor(
                p,
                lang_id.to_string(),
                cmd.to_string(),
                in_rx,
                out_tx.clone(),
                initial_text,
            ));
        }
    }

    /// Dispatches an asynchronous `textDocument/semanticTokens/full` request to the LSP actor.
    pub fn request_semantic_tokens(&mut self) {
        if self.syntax.has_treesitter() {
            return;
        }
        if let Some(tx) = &self.lsp_tx {
            self.lsp_req_id += 1;
            let _ = tx.send(LspInbound::SemanticTokens {
                req_id: self.lsp_req_id,
            });
        }
    }

    /// Pushes a snapshot of the current [`Rope`] and cursor coordinates onto `undo_stack`.
    pub fn snapshot(&mut self) {
        if self.undo_stack.len() >= 64 {
            self.undo_stack.remove(0);
        }
        self.undo_stack.push(UndoSnapshot {
            rope: self.rope.clone(),
            cursor_x: self.cursor_x,
            cursor_y: self.cursor_y,
        });
    }

    /// Reverts the document rope to the most recent checkpoint on `undo_stack`.
    pub fn undo(&mut self) {
        if let Some(prev) = self.undo_stack.pop() {
            self.rope = prev.rope;
            self.cursor_x = prev.cursor_x;
            self.cursor_y = prev.cursor_y;
            self.modified = true;
            self.status_msg = "Reverted change".to_string();
            self.insert_snapshot_taken = false;
            self.clamp_cursor();
            self.on_buffer_modified();
        }
    }

    /// Synchronizes external subsystems and invalidates stale diagnostics following a mutation.
    pub fn on_buffer_modified(&mut self) {
        let text = self.rope.to_string();
        self.syntax.reparse(&text);
        self.doc_version += 1;

        let max_lines = self.rope.len_lines().max(1);
        self.diagnostics.retain(|d| d.line < max_lines);

        if let Some(tx) = &self.lsp_tx {
            let _ = tx.send(LspInbound::Change {
                text,
                version: self.doc_version,
            });
            self.request_semantic_tokens();
        }
    }

    /// Dispatches an asynchronous `textDocument/completion` request using UTF-16 coordinates.
    pub fn request_completions(&mut self) {
        if let Some(tx) = &self.lsp_tx {
            self.lsp_req_id += 1;
            let col = self.cursor_utf16_col();
            let _ = tx.send(LspInbound::Completion {
                line: self.cursor_y,
                col,
                req_id: self.lsp_req_id,
            });
        }
    }

    /// Returns the character count of the line at [`Self::cursor_y`], excluding newline delimiters.
    pub fn current_line_len(&self) -> usize {
        line_len(&self.rope, self.cursor_y)
    }

    /// Translates 2D cursor coordinates `(cursor_x, cursor_y)` into a linear 1D character offset.
    pub fn char_index(&self) -> usize {
        let line_start = self.rope.line_to_char(self.cursor_y);
        line_start + self.cursor_x
    }

    /// Returns the unicode `char` currently situated directly under the cursor.
    pub fn char_under_cursor(&self) -> Option<char> {
        let idx = self.char_index();
        if idx < self.rope.len_chars() {
            Some(self.rope.char(idx))
        } else {
            None
        }
    }

    /// Scans backward from `cursor_x` on the current line to extract the active identifier prefix.
    pub fn current_word_prefix(&self) -> String {
        if self.cursor_y >= self.rope.len_lines() {
            return String::new();
        }
        let line = self.rope.line(self.cursor_y);
        let chars: Vec<char> = line.chars().collect();
        let mut start = self.cursor_x;
        while start > 0 {
            let c = chars.get(start - 1).copied().unwrap_or(' ');
            if c.is_alphanumeric() || c == '_' {
                start -= 1;
            } else {
                break;
            }
        }
        if start < self.cursor_x && self.cursor_x <= chars.len() {
            chars[start..self.cursor_x].iter().collect()
        } else {
            String::new()
        }
    }

    /// Calculates the half-open linear character range `[start, end)` representing the active visual selection.
    #[allow(clippy::comparison_chain)]
    pub fn selection_range(&self) -> Option<(usize, usize)> {
        if let Mode::Visual { anchor_x, anchor_y } = self.mode {
            let (start_idx, end_idx) = if anchor_y < self.cursor_y {
                let start = self.rope.line_to_char(anchor_y) + anchor_x;
                let end = self.rope.line_to_char(self.cursor_y) + self.cursor_x;
                (start, end)
            } else if anchor_y > self.cursor_y {
                let start = self.rope.line_to_char(self.cursor_y) + self.cursor_x;
                let end = self.rope.line_to_char(anchor_y) + anchor_x;
                (start, end)
            } else {
                let sx = anchor_x.min(self.cursor_x);
                let ex = anchor_x.max(self.cursor_x);
                let base = self.rope.line_to_char(anchor_y);
                (base + sx, base + ex)
            };

            Some((start_idx, (end_idx + 1).min(self.rope.len_chars())))
        } else {
            None
        }
    }

    /// Queries whether the character at buffer coordinate `(line, col)` falls within the active visual selection.
    pub fn is_char_selected(&self, line: usize, col: usize) -> bool {
        if let Mode::Visual { anchor_x, anchor_y } = self.mode {
            let (a_l, a_c) = (anchor_y, anchor_x);
            let (c_l, c_c) = (self.cursor_y, self.cursor_x);
            let (min_l, min_c, max_l, max_c) = if a_l < c_l || (a_l == c_l && a_c <= c_c) {
                (a_l, a_c, c_l, c_c)
            } else {
                (c_l, c_c, a_l, a_c)
            };

            if line < min_l || line > max_l {
                return false;
            }
            if line == min_l && line == max_l {
                col >= min_c && col <= max_c
            } else if line == min_l {
                col >= min_c
            } else if line == max_l {
                col <= max_c
            } else {
                true
            }
        } else {
            false
        }
    }

    /// Enters [`Mode::Visual`] spanning the entirety of the buffer.
    pub fn select_all(&mut self) {
        if self.rope.len_chars() == 0 {
            return;
        }
        self.set_mode(Mode::Visual {
            anchor_x: 0,
            anchor_y: 0,
        });
        self.cursor_y = self.rope.len_lines().saturating_sub(1);
        self.cursor_x = self.current_line_len();
        self.status_msg = "Selected all".to_string();
    }

    /// Copies selected text (or the entire current line if no selection is active) into [`Self::clipboard`].
    pub fn yank_selection(&mut self) {
        if let Some((start, end)) = self.selection_range() {
            if start < end {
                let slice = self.rope.slice(start..end);
                self.clipboard = slice.to_string();
                self.status_msg = format!("Yanked {} chars", self.clipboard.chars().count());
            }
        } else if self.cursor_y < self.rope.len_lines() {
            let line = self.rope.line(self.cursor_y);
            self.clipboard = line.to_string();
            self.status_msg = "Yanked line".to_string();
        }
        self.set_mode(Mode::Normal);
    }

    /// Deletes characters encompassed by the active visual selection, storing them in [`Self::clipboard`].
    pub fn delete_selection(&mut self) {
        if let Some((start, end)) = self.selection_range()
            && start < end
        {
            self.snapshot();
            let slice = self.rope.slice(start..end);
            self.clipboard = slice.to_string();
            self.rope.remove(start..end);
            self.modified = true;
            self.status_msg = format!("Deleted {} chars", self.clipboard.chars().count());

            let new_cursor_line = self.rope.char_to_line(start);
            let line_start = self.rope.line_to_char(new_cursor_line);
            self.cursor_y = new_cursor_line;
            self.cursor_x = start.saturating_sub(line_start);
            self.set_mode(Mode::Normal);
            self.clamp_cursor();
            self.on_buffer_modified();
        }
    }

    /// Inserts the contents of [`Self::clipboard`] into the buffer at the current cursor index.
    pub fn paste(&mut self) {
        if self.clipboard.is_empty() {
            self.status_msg = "Clipboard is empty".to_string();
            return;
        }

        self.snapshot();
        let idx = self.char_index().min(self.rope.len_chars());
        self.rope.insert(idx, &self.clipboard);
        let end_idx = idx + self.clipboard.chars().count();
        let new_line = self.rope.char_to_line(end_idx);
        let line_start = self.rope.line_to_char(new_line);
        self.cursor_y = new_line;
        self.cursor_x = end_idx.saturating_sub(line_start);
        self.modified = true;
        self.clamp_cursor();
        self.on_buffer_modified();
        self.status_msg = "Pasted from clipboard".to_string();
    }

    /// Toggles the case of selected characters or the character directly beneath the cursor.
    pub fn toggle_case(&mut self) {
        if let Some((start, end)) = self.selection_range() {
            if start < end {
                self.snapshot();
                let orig = self.rope.slice(start..end).to_string();
                let toggled: String = orig
                    .chars()
                    .map(|c| {
                        if c.is_uppercase() {
                            c.to_lowercase().next().unwrap_or(c)
                        } else {
                            c.to_uppercase().next().unwrap_or(c)
                        }
                    })
                    .collect();

                self.rope.remove(start..end);
                self.rope.insert(start, &toggled);
                self.modified = true;
                self.set_mode(Mode::Normal);
                self.on_buffer_modified();
            }
        } else {
            let line_len = self.current_line_len();
            if self.cursor_x < line_len {
                let idx = self.char_index();
                if idx < self.rope.len_chars() {
                    self.snapshot();
                    let c = self.rope.char(idx);
                    let toggled = if c.is_uppercase() {
                        c.to_lowercase().next().unwrap_or(c)
                    } else {
                        c.to_uppercase().next().unwrap_or(c)
                    };
                    self.rope.remove(idx..=idx);
                    self.rope.insert_char(idx, toggled);
                    self.cursor_x += 1;
                    self.modified = true;
                    self.clamp_cursor();
                    self.on_buffer_modified();
                }
            }
        }
    }

    /// Merges the line below the current cursor line onto the current line.
    pub fn join_lines(&mut self) {
        if self.cursor_y + 1 >= self.rope.len_lines() {
            return;
        }

        self.snapshot();
        let next_line_start = self.rope.line_to_char(self.cursor_y + 1);
        let del_range = if next_line_start >= 2
            && self.rope.char(next_line_start - 1) == '\n'
            && self.rope.char(next_line_start - 2) == '\r'
        {
            (next_line_start - 2)..next_line_start
        } else if next_line_start > 0 {
            (next_line_start - 1)..next_line_start
        } else {
            0..0
        };

        let insert_pos = del_range.start;
        self.rope.remove(del_range);

        let mut ws_len = 0;
        while insert_pos + ws_len < self.rope.len_chars()
            && (self.rope.char(insert_pos + ws_len) == ' '
                || self.rope.char(insert_pos + ws_len) == '\t')
        {
            ws_len += 1;
        }

        if ws_len > 0 {
            self.rope.remove(insert_pos..insert_pos + ws_len);
        }
        self.rope.insert_char(insert_pos, ' ');

        self.cursor_y = self.rope.char_to_line(insert_pos);
        let line_start = self.rope.line_to_char(self.cursor_y);
        self.cursor_x = insert_pos.saturating_sub(line_start);

        self.modified = true;
        self.clamp_cursor();
        self.on_buffer_modified();
        self.status_msg = "Joined lines".to_string();
    }

    /// Creates an empty line beneath the current line and switches into [`Mode::Insert`].
    pub fn insert_line_below(&mut self) {
        self.snapshot();
        let le = self.detect_line_ending();
        let indent: String = if self.cursor_y < self.rope.len_lines() {
            self.rope
                .line(self.cursor_y)
                .chars()
                .take_while(|c| *c == ' ' || *c == '\t')
                .collect()
        } else {
            String::new()
        };

        let line_len_with_nl = if self.cursor_y < self.rope.len_lines() {
            self.rope.line(self.cursor_y).len_chars()
        } else {
            0
        };
        let line_start = if self.cursor_y < self.rope.len_lines() {
            self.rope.line_to_char(self.cursor_y)
        } else {
            self.rope.len_chars()
        };

        let has_newline = if self.cursor_y < self.rope.len_lines() {
            let l = self.rope.line(self.cursor_y);
            let len = l.len_chars();
            len > 0 && l.char(len - 1) == '\n'
        } else {
            false
        };

        let insert_idx = line_start + line_len_with_nl;
        let to_insert = if has_newline {
            format!("{indent}{le}")
        } else {
            format!("{le}{indent}")
        };

        self.rope.insert(insert_idx, &to_insert);
        self.cursor_y += 1;
        self.cursor_x = indent.chars().count();
        self.modified = true;
        self.set_mode(Mode::Insert);
        self.insert_snapshot_taken = true;
        self.completion_visible = false;
        self.on_buffer_modified();
    }

    /// Creates an empty line above the current line and switches into [`Mode::Insert`].
    pub fn insert_line_above(&mut self) {
        self.snapshot();
        let le = self.detect_line_ending();
        let indent: String = if self.cursor_y < self.rope.len_lines() {
            self.rope
                .line(self.cursor_y)
                .chars()
                .take_while(|c| *c == ' ' || *c == '\t')
                .collect()
        } else {
            String::new()
        };

        let idx = if self.cursor_y < self.rope.len_lines() {
            self.rope.line_to_char(self.cursor_y)
        } else {
            0
        };

        let to_insert = format!("{indent}{le}");
        self.rope.insert(idx, &to_insert);
        self.cursor_x = indent.chars().count();
        self.modified = true;
        self.set_mode(Mode::Insert);
        self.insert_snapshot_taken = true;
        self.completion_visible = false;
        self.on_buffer_modified();
    }

    /// Inserts a single unicode character at the current cursor offset and advances the cursor.
    pub fn insert_char(&mut self, c: char) {
        self.check_insert_snapshot();
        let idx = self.char_index();
        self.rope.insert_char(idx, c);
        self.cursor_x += 1;
        self.modified = true;
        self.on_buffer_modified();
    }

    /// Inserts an opening and closing delimiter pair, placing the cursor between them.
    pub fn insert_pair(&mut self, open: char, close: char) {
        self.check_insert_snapshot();
        let idx = self.char_index();
        self.rope.insert_char(idx, open);
        self.rope.insert_char(idx + 1, close);
        self.cursor_x += 1;
        self.modified = true;
        self.on_buffer_modified();
    }

    /// Inserts a newline character with automatic indentation preservation.
    pub fn insert_newline(&mut self) {
        self.check_insert_snapshot();
        let le = self.detect_line_ending();
        let idx = self.char_index();
        let current_line = if self.cursor_y < self.rope.len_lines() {
            self.rope.line(self.cursor_y).to_string()
        } else {
            String::new()
        };

        let indent: String = current_line
            .chars()
            .take_while(|c| *c == ' ' || *c == '\t')
            .collect();

        let char_before = if idx > 0 {
            Some(self.rope.char(idx - 1))
        } else {
            None
        };
        let char_after = if idx < self.rope.len_chars() {
            Some(self.rope.char(idx))
        } else {
            None
        };

        if char_before == Some('{') && char_after == Some('}') {
            let inner_indent = format!("{indent}    ");
            let to_insert = format!("{le}{inner_indent}{le}{indent}");
            self.rope.insert(idx, &to_insert);
            self.cursor_y += 1;
            self.cursor_x = inner_indent.chars().count();
        } else {
            let to_insert = format!("{le}{indent}");
            self.rope.insert(idx, &to_insert);
            self.cursor_y += 1;
            self.cursor_x = inner_indent_len(&indent);
        }

        self.modified = true;
        self.completion_visible = false;
        self.on_buffer_modified();
    }

    /// Deletes the character before the cursor or handles multi-character boundary conditions.
    pub fn backspace(&mut self) {
        self.check_insert_snapshot();
        if self.cursor_x > 0 {
            let idx = self.char_index();
            let char_before = self.rope.char(idx - 1);
            let char_after = if idx < self.rope.len_chars() {
                Some(self.rope.char(idx))
            } else {
                None
            };

            #[allow(clippy::match_like_matches_macro)]
            let is_pair = match (char_before, char_after) {
                ('(', Some(')')) => true,
                ('[', Some(']')) => true,
                ('{', Some('}')) => true,
                ('"', Some('"')) => true,
                ('\'', Some('\'')) => true,
                _ => false,
            };

            if is_pair {
                self.rope.remove((idx - 1)..=idx);
            } else {
                self.rope.remove(idx - 1..idx);
            }

            self.cursor_x -= 1;
            self.modified = true;
            self.on_buffer_modified();
        } else if self.cursor_y > 0 {
            let prev_len = line_len(&self.rope, self.cursor_y - 1);
            let current_line_idx = self.rope.line_to_char(self.cursor_y);

            if current_line_idx >= 2
                && self.rope.char(current_line_idx - 1) == '\n'
                && self.rope.char(current_line_idx - 2) == '\r'
            {
                self.rope.remove(current_line_idx - 2..current_line_idx);
            } else if current_line_idx > 0 {
                self.rope.remove(current_line_idx - 1..current_line_idx);
            }

            self.cursor_y -= 1;
            self.cursor_x = prev_len;
            self.modified = true;
            self.on_buffer_modified();
        }

        self.completion_visible = false;
    }

    /// Updates `completion_scroll` to keep `completion_idx` inside the visible completion popup.
    pub fn update_completion_scroll(&mut self, max_visible: usize) {
        if self.completion_idx < self.completion_scroll {
            self.completion_scroll = self.completion_idx;
        } else if self.completion_idx >= self.completion_scroll + max_visible {
            self.completion_scroll = self.completion_idx + 1 - max_visible;
        }
    }

    /// Applies the selected completion candidate into the buffer.
    pub fn accept_completion(&mut self) {
        if !self.completion_visible || self.completions.is_empty() {
            return;
        }

        self.snapshot();
        let item = &self.completions[self.completion_idx];
        let replacement = item.insert_text.clone();
        let prefix = self.current_word_prefix();
        let prefix_len = prefix.chars().count();

        let idx = self.char_index();
        let start = idx.saturating_sub(prefix_len);

        self.rope.remove(start..idx);
        self.rope.insert(start, &replacement);

        let end_idx = start + replacement.chars().count();
        let new_line = self.rope.char_to_line(end_idx);
        let line_start = self.rope.line_to_char(new_line);
        self.cursor_y = new_line;
        self.cursor_x = end_idx.saturating_sub(line_start);

        self.modified = true;
        self.completion_visible = false;
        self.clamp_cursor();
        self.on_buffer_modified();
    }

    /// Deletes a single character situated directly under the cursor and stores it in [`Self::clipboard`].
    pub fn delete_under_cursor(&mut self) {
        let line_len = self.current_line_len();
        if self.cursor_x < line_len {
            self.snapshot();
            let idx = self.char_index();
            self.clipboard = self.rope.char(idx).to_string();
            self.rope.remove(idx..=idx);
            self.modified = true;
            self.clamp_cursor();
            self.on_buffer_modified();
        }
    }

    /// Deletes the entire active line and stores it in [`Self::clipboard`].
    pub fn delete_current_line(&mut self) {
        if self.rope.len_lines() == 0 {
            return;
        }
        self.snapshot();
        let num_lines = self.rope.len_lines();
        let start = self.rope.line_to_char(self.cursor_y);
        let end = if self.cursor_y + 1 < num_lines {
            self.rope.line_to_char(self.cursor_y + 1)
        } else {
            self.rope.len_chars()
        };

        if start < end {
            self.clipboard = self.rope.slice(start..end).to_string();
            self.rope.remove(start..end);
        } else if start == end && self.cursor_y > 0 {
            let prev_line_start = self.rope.line_to_char(self.cursor_y - 1);
            let line_del = prev_line_start + line_len(&self.rope, self.cursor_y - 1);
            if line_del < start {
                self.clipboard = self.rope.slice(line_del..start).to_string();
                self.rope.remove(line_del..start);
            }
        }
        self.modified = true;
        self.status_msg = "Cut line".to_string();
        self.on_buffer_modified();

        if self.cursor_y >= self.rope.len_lines() && self.cursor_y > 0 {
            self.cursor_y -= 1;
        }
        self.clamp_cursor();
    }

    /// Safely writes the rope buffer to disk using atomic filesystem renaming.
    pub fn save(&mut self) -> Result<()> {
        if let Some(path) = &self.path {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }

            let file_name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("s0_buffer");
            let tmp_path = path.with_file_name(format!(".{file_name}.s0_tmp"));

            {
                let file = File::create(&tmp_path)?;
                let mut writer = BufWriter::new(file);
                for chunk in self.rope.chunks() {
                    writer.write_all(chunk.as_bytes())?;
                }
                writer.flush()?;
            }

            fs::rename(&tmp_path, path)?;

            self.modified = false;
            self.status_msg = format!("Saved {}", path.display());

            if let Some(tx) = &self.lsp_tx {
                let _ = tx.send(LspInbound::Save);
                self.request_semantic_tokens();
            }
            Ok(())
        } else {
            self.status_msg = "No file name (use :w <name>)".to_string();
            Err(anyhow!("No file name specified"))
        }
    }

    /// Executes an action selected from the command palette overlay.
    pub fn execute_palette_command(&mut self, id: CommandId) {
        self.palette.visible = false;
        match id {
            CommandId::SelectAll => self.select_all(),
            CommandId::ToggleExplorer => {
                self.explorer.visible = !self.explorer.visible;
                if self.explorer.visible {
                    self.explorer.refresh();
                    self.focus = Focus::Explorer;
                } else {
                    self.focus = Focus::Editor;
                }
            }
            CommandId::ToggleWrap => {
                self.line_wrap = !self.line_wrap;
                self.status_msg =
                    format!("Line Wrap: {}", if self.line_wrap { "ON" } else { "OFF" });
            }
            CommandId::Save => {
                let _ = self.save();
            }
            CommandId::SaveQuit => {
                if self.save().is_ok() {
                    self.should_quit = true;
                }
            }
            CommandId::QuitForce => self.should_quit = true,
            CommandId::EnterVisual => {
                self.set_mode(Mode::Visual {
                    anchor_x: self.cursor_x,
                    anchor_y: self.cursor_y,
                });
            }
            CommandId::InsertBelow => self.insert_line_below(),
            CommandId::InsertAbove => self.insert_line_above(),
            CommandId::JoinLines => self.join_lines(),
            CommandId::Undo => self.undo(),
            CommandId::Yank => self.yank_selection(),
            CommandId::Paste => self.paste(),
            CommandId::ToggleCase => self.toggle_case(),
            CommandId::JumpTop => {
                self.cursor_y = 0;
                self.cursor_x = 0;
            }
            CommandId::JumpBottom => {
                self.cursor_y = self.rope.len_lines().saturating_sub(1);
                self.cursor_x = 0;
            }
            CommandId::TriggerCompletion => self.request_completions(),
            CommandId::SelectLsp => {
                let installed = self.syntax.language.installed_servers();
                if installed.is_empty() {
                    self.status_msg = "No installed LSP found for this file".to_string();
                } else {
                    self.lsp_picker = Some(LspPicker {
                        language_id: self.syntax.language.lsp_id().to_string(),
                        candidates: installed,
                        selected_idx: 0,
                    });
                }
            }
            CommandId::SaveConfig => {
                self.config.line_wrap = self.line_wrap;
                if self.config.save().is_ok() {
                    self.status_msg = "Config saved to .subject0".to_string();
                } else {
                    self.status_msg = "Failed to write .subject0".to_string();
                }
            }
            CommandId::ShowHelp => {
                self.show_help = true;
                self.help_scroll = 0;
            }
        }
    }

    /// Evaluates and executes the ex-command string buffered in `command_buffer`.
    pub fn execute_command(&mut self) {
        let cmd = self.command_buffer.trim().to_string();
        self.command_buffer.clear();

        let mut parts = cmd.split_whitespace();
        let action = parts.next().unwrap_or("");
        let arg = parts.next();

        match action {
            "q" => {
                if self.modified {
                    self.status_msg = "Unsaved changes! Use :q! to force quit".to_string();
                } else {
                    self.should_quit = true;
                }
            }
            "q!" => {
                self.should_quit = true;
            }
            "w" => {
                if let Some(filename) = arg {
                    self.switch_file_target(PathBuf::from(filename));
                }
                let _ = self.save();
            }
            "wq" => {
                if let Some(filename) = arg {
                    self.switch_file_target(PathBuf::from(filename));
                }
                if self.save().is_ok() {
                    self.should_quit = true;
                }
            }
            "wrap" => {
                self.line_wrap = !self.line_wrap;
                self.status_msg =
                    format!("Line Wrap: {}", if self.line_wrap { "ON" } else { "OFF" });
            }
            "e" | "explore" => {
                self.explorer.visible = !self.explorer.visible;
                if self.explorer.visible {
                    self.explorer.refresh();
                    self.focus = Focus::Explorer;
                } else {
                    self.focus = Focus::Editor;
                }
            }
            "p" | "menu" | "commands" | "pal" => {
                self.palette.visible = true;
                self.palette.query.clear();
                self.palette.selected_idx = 0;
                self.palette.scroll = 0;
            }
            "h" | "help" => {
                self.show_help = true;
                self.help_scroll = 0;
            }

            _ if !cmd.is_empty() => {
                self.status_msg = format!("Unknown command: :{cmd}");
            }
            _ => {}
        }
    }

    /// Enforces modal cursor constraints against buffer boundaries.
    pub fn clamp_cursor(&mut self) {
        let max_lines = self.rope.len_lines().max(1);
        if self.cursor_y >= max_lines {
            self.cursor_y = max_lines - 1;
        }

        let line_len = self.current_line_len();
        let max_x = match self.mode {
            Mode::Insert => line_len,
            Mode::Normal | Mode::Command | Mode::Visual { .. } => line_len.saturating_sub(1),
        };

        if self.cursor_x > max_x {
            self.cursor_x = max_x;
        }
    }

    /// Recalculates horizontal and vertical scroll offsets so the cursor remains visible.
    pub fn update_scroll(&mut self, width: usize, height: usize) {
        if height == 0 || width == 0 {
            return;
        }

        if self.cursor_y < self.scroll_y {
            self.scroll_y = self.cursor_y;
        }

        if self.line_wrap {
            self.scroll_x = 0;
            let effective_width = width.max(1);

            let calc_cursor_visual_row =
                |start_y: usize, cursor_y: usize, cursor_x: usize, rope: &Rope| -> usize {
                    let mut rows = 0;
                    for y in start_y..cursor_y {
                        let len = line_len(rope, y);
                        let line_rows = if len == 0 {
                            1
                        } else {
                            len.div_ceil(effective_width)
                        };
                        rows += line_rows;
                    }
                    let cur_len = line_len(rope, cursor_y);
                    let cur_rows = if cur_len == 0 {
                        1
                    } else {
                        cur_len.div_ceil(effective_width)
                    };
                    let cursor_sub_row =
                        (cursor_x / effective_width).min(cur_rows.saturating_sub(1));
                    rows + cursor_sub_row
                };

            while self.scroll_y < self.cursor_y
                && calc_cursor_visual_row(self.scroll_y, self.cursor_y, self.cursor_x, &self.rope)
                    >= height
            {
                self.scroll_y += 1;
            }
        } else {
            if self.cursor_y >= self.scroll_y + height {
                self.scroll_y = self.cursor_y - height + 1;
            }

            if self.cursor_x < self.scroll_x {
                self.scroll_x = self.cursor_x;
            } else if self.cursor_x >= self.scroll_x + width {
                self.scroll_x = self.cursor_x - width + 1;
            }
        }
    }
}

/// Helper function to compute character length for indentation strings.
fn inner_indent_len(indent: &str) -> usize {
    indent.chars().count()
}

/// Calculates the printable character count of a rope line, stripping trailing `\r` and `\n`.
pub fn line_len(rope: &Rope, line_idx: usize) -> usize {
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
