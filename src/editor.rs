//! # Editor Buffer Model & Application State Engine
//!
//! This module forms the central operational core of `subject0`:
//!
//! 1. **Text Storage & Mutability ([`ropey::Rope`])**:
//!    Buffer contents are stored as a chunked, reference-counted B-tree rope with $O(\log N)$
//!    mutations and $O(1)$ copy-on-write structural sharing for undo/redo snapshots.
//!
//! 2. **Modal Editing State Machine ([`Mode`])**:
//!    Implements modal key semantics across `Normal`, `Insert`, `Command`, and `Visual` states.
//!
//! 3. **Top-Tier Language Server Protocol (LSP) State Integration**:
//!    - Multi-file workspace edit dispatcher applying atomic updates across disk and memory.
//!    - Additional text edits on completion acceptance (auto-imports).
//!    - Jump history tracking across document definition/reference navigations.
//!    - Bidirectional diagnostic traversal (`next_diagnostic`, `prev_diagnostic`).
//!    - Request cancellation and server reboot triggers (`:lsp-restart`).
//!
//! 4. **Theming, Diagnostics Sync & Data Protection**:
//!    Maintains AST highlighting trees, shifts diagnostic positions on row mutations,
//!    tracks a bidirectional undo/redo ring, and protects unsaved buffers.

use std::{
    collections::{HashMap, HashSet},
    env,
    fs::{self, File},
    io::{BufWriter, Write},
    path::{Path, PathBuf},
};

use crate::lsp::{
    CodeActionItem, DiagnosticItem, HoverInfo, InlayHintItem, LocationItem, LspInbound,
    LspOutbound, LspStatus, SignatureHelpInfo, SuggestionItem, SymbolItem, TextEditItem,
    char_to_utf16_col, run_lsp_actor, subject0_config_dir, utf16_to_char_col,
};
use crate::syntax::SyntaxEngine;

use crate::nerdfonts::{
    ARROW_LEFT, ARROW_RIGHT, CMD_CODE_ACTIONS, CMD_DEFINITION, CMD_FORMAT, CMD_HOVER,
    CMD_INSERT_ABOVE, CMD_INSERT_BELOW, CMD_JOIN, CMD_JUMP_BOTTOM, CMD_JUMP_TOP, CMD_PASTE,
    CMD_QUIT, CMD_REDO, CMD_REFERENCES, CMD_RENAME, CMD_RESTART, CMD_SAVE, CMD_SAVE_QUIT,
    CMD_SYMBOLS, CMD_TOGGLE_CASE, CMD_UNDO, CMD_VISUAL, CMD_YANK, DIAG_ERROR, DIAG_WARN,
    FILE_DOCUMENT, FOLDER, GEAR_CONFIG, HELP, HINTS_ON, LIGHTBULB, SETTINGS_COGS, THEME, WRAP_ON,
};
use crate::theme::Theme;

use anyhow::{Result, anyhow};
use ropey::Rope;
use serde_json::{Value, json};
use tokio::sync::mpsc;

/// Persistent editor configuration stored in `.subject0`.
#[derive(Clone, Debug)]
pub struct AppConfig {
    pub preferred_lsps: HashMap<String, String>,
    pub line_wrap: bool,
    pub theme: String,
    pub show_inlay_hints: bool,
    /// Resolved absolute path to the `.subject0` configuration file.
    pub source_path: PathBuf,
}

impl AppConfig {
    fn resolve_path(project_root: &Path) -> PathBuf {
        let project_cfg = project_root.join(".subject0");
        if project_cfg.exists() {
            return project_cfg;
        }

        let global_cfg = subject0_config_dir().join(".subject0");
        if global_cfg.exists() {
            return global_cfg;
        }

        if let Some(home) = dirs::home_dir() {
            let home_cfg = home.join(".subject0");
            if home_cfg.exists() {
                return home_cfg;
            }
        }

        project_cfg
    }

    pub fn load_from(project_root: &Path) -> Self {
        let path = Self::resolve_path(project_root);
        let mut preferred_lsps = HashMap::new();
        let mut line_wrap = true;
        let mut theme = "gruber-darker".to_string();
        let mut show_inlay_hints = true;

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
            if let Some(t) = val.get("theme").and_then(Value::as_str) {
                theme = t.to_string();
            }
            if let Some(ih) = val.get("show_inlay_hints").and_then(Value::as_bool) {
                show_inlay_hints = ih;
            }
        }

        Self {
            preferred_lsps,
            line_wrap,
            theme,
            show_inlay_hints,
            source_path: path,
        }
    }

    pub fn save(&self) -> Result<()> {
        let mut val = if let Ok(content) = fs::read_to_string(&self.source_path) {
            if let Ok(existing) = serde_json::from_str::<Value>(&content) {
                existing
            } else {
                json!({})
            }
        } else {
            json!({})
        };

        if let Some(obj) = val.as_object_mut() {
            obj.insert("preferred_lsps".to_string(), json!(self.preferred_lsps));
            obj.insert("line_wrap".to_string(), json!(self.line_wrap));
            obj.insert("theme".to_string(), json!(self.theme));
            obj.insert("show_inlay_hints".to_string(), json!(self.show_inlay_hints));
        } else {
            val = json!({
                "preferred_lsps": self.preferred_lsps,
                "line_wrap": self.line_wrap,
                "theme": self.theme,
                "show_inlay_hints": self.show_inlay_hints,
            });
        }

        if let Some(parent) = self.source_path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        fs::write(&self.source_path, serde_json::to_string_pretty(&val)?)?;
        Ok(())
    }
}

/// Interactive modal state when choosing from available color themes.
pub struct ThemePicker {
    pub selected_idx: usize,
}

/// Interactive modal state when multiple language servers are detected for a language.
pub struct LspPicker {
    pub language_id: String,
    pub candidates: Vec<String>,
    pub selected_idx: usize,
}

/// Interactive modal state when choosing a Code Action / Quickfix.
pub struct CodeActionPicker {
    pub actions: Vec<CodeActionItem>,
    pub selected_idx: usize,
}

/// Interactive modal state for fuzzy symbol search across document outlines.
pub struct SymbolPicker {
    pub symbols: Vec<SymbolItem>,
    pub query: String,
    pub selected_idx: usize,
    pub scroll: usize,
}

impl SymbolPicker {
    pub fn filtered_symbols(&self) -> Vec<&SymbolItem> {
        let q = self.query.to_lowercase();
        self.symbols
            .iter()
            .filter(|s| {
                q.is_empty()
                    || s.name.to_lowercase().contains(&q)
                    || s.container_name
                        .as_deref()
                        .is_some_and(|c| c.to_lowercase().contains(&q))
            })
            .collect()
    }
}

/// Interactive modal state when choosing from multiple definition or reference locations.
pub struct LocationPicker {
    pub title: &'static str,
    pub locations: Vec<LocationItem>,
    pub selected_idx: usize,
    pub scroll: usize,
}

/// Active modal editing state.
#[derive(PartialEq, Eq, Clone, Copy, Debug)]
pub enum Mode {
    Normal,
    Insert,
    Command,
    Visual { anchor_x: usize, anchor_y: usize },
}

/// Identifies which viewport element currently holds keyboard input focus.
#[derive(PartialEq, Eq, Clone, Copy, Debug)]
pub enum Focus {
    Editor,
    Explorer,
}

/// Navigation jump checkpoint for jump-list history.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JumpCheckpoint {
    pub path: PathBuf,
    pub line: usize,
    pub col: usize,
}

// === Command Palette Definitions ===

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CommandId {
    SelectAll,
    ToggleExplorer,
    ToggleWrap,
    Save,
    SaveQuit,
    Quit,
    QuitForce,
    EnterVisual,
    InsertBelow,
    InsertAbove,
    JoinLines,
    Undo,
    Redo,
    Yank,
    ToggleCase,
    Paste,
    JumpTop,
    JumpBottom,
    TriggerCompletion,
    SelectLsp,
    SelectTheme,
    SaveConfig,
    ShowHelp,
    // LSP-Powered Capabilities
    FormatDocument,
    RenameSymbol,
    ShowHover,
    CodeActions,
    DocumentSymbols,
    GoToDefinition,
    FindReferences,
    ToggleInlayHints,
    RestartLsp,
    NextDiagnostic,
    PrevDiagnostic,
    JumpBackward,
    JumpForward,
}

#[derive(Clone)]
pub struct PaletteCommand {
    pub title: &'static str,
    pub shortcut: &'static str,
    pub icon: &'static str,
    pub id: CommandId,
}

pub static PALETTE_COMMANDS: &[PaletteCommand] = &[
    PaletteCommand {
        title: "Format Document (LSP)",
        shortcut: ":fmt / Alt-F",
        icon: CMD_FORMAT,
        id: CommandId::FormatDocument,
    },
    PaletteCommand {
        title: "Show Documentation / Type Hover",
        shortcut: "K / :hover",
        icon: CMD_HOVER,
        id: CommandId::ShowHover,
    },
    PaletteCommand {
        title: "Code Actions & Quickfixes",
        shortcut: "ga / :ca",
        icon: CMD_CODE_ACTIONS,
        id: CommandId::CodeActions,
    },
    PaletteCommand {
        title: "Go to Definition",
        shortcut: "gd",
        icon: CMD_DEFINITION,
        id: CommandId::GoToDefinition,
    },
    PaletteCommand {
        title: "Find References",
        shortcut: "gr",
        icon: CMD_REFERENCES,
        id: CommandId::FindReferences,
    },
    PaletteCommand {
        title: "Rename Symbol (Project-Wide)",
        shortcut: ":rn / F2",
        icon: CMD_RENAME,
        id: CommandId::RenameSymbol,
    },
    PaletteCommand {
        title: "Document Symbol Outline",
        shortcut: ":symbols / :sym",
        icon: CMD_SYMBOLS,
        id: CommandId::DocumentSymbols,
    },
    PaletteCommand {
        title: "Next Compiler Diagnostic",
        shortcut: ":nd / ]d",
        icon: DIAG_ERROR,
        id: CommandId::NextDiagnostic,
    },
    PaletteCommand {
        title: "Previous Compiler Diagnostic",
        shortcut: ":pd / [d",
        icon: DIAG_WARN,
        id: CommandId::PrevDiagnostic,
    },
    PaletteCommand {
        title: "Jump Backward (Location History)",
        shortcut: "Ctrl-O",
        icon: ARROW_LEFT,
        id: CommandId::JumpBackward,
    },
    PaletteCommand {
        title: "Jump Forward (Location History)",
        shortcut: "Ctrl-I",
        icon: ARROW_RIGHT,
        id: CommandId::JumpForward,
    },
    PaletteCommand {
        title: "Restart Active LSP Server",
        shortcut: ":lsp-restart",
        icon: CMD_RESTART,
        id: CommandId::RestartLsp,
    },
    PaletteCommand {
        title: "Toggle Inferred Type & Param Inlay Hints",
        shortcut: ":hints",
        icon: HINTS_ON,
        id: CommandId::ToggleInlayHints,
    },
    PaletteCommand {
        title: "Select All Buffer",
        shortcut: "%",
        icon: FILE_DOCUMENT,
        id: CommandId::SelectAll,
    },
    PaletteCommand {
        title: "Toggle File Explorer",
        shortcut: "Ctrl-E / :e",
        icon: FOLDER,
        id: CommandId::ToggleExplorer,
    },
    PaletteCommand {
        title: "Toggle Line Wrap",
        shortcut: ":wrap",
        icon: WRAP_ON,
        id: CommandId::ToggleWrap,
    },
    PaletteCommand {
        title: "Save Buffer",
        shortcut: ":w",
        icon: CMD_SAVE,
        id: CommandId::Save,
    },
    PaletteCommand {
        title: "Save and Quit",
        shortcut: ":wq",
        icon: CMD_SAVE_QUIT,
        id: CommandId::SaveQuit,
    },
    PaletteCommand {
        title: "Quit Editor",
        shortcut: ":q",
        icon: CMD_QUIT,
        id: CommandId::Quit,
    },
    PaletteCommand {
        title: "Force Quit",
        shortcut: ":q!",
        icon: CMD_QUIT,
        id: CommandId::QuitForce,
    },
    PaletteCommand {
        title: "Enter Visual Selection",
        shortcut: "v",
        icon: CMD_VISUAL,
        id: CommandId::EnterVisual,
    },
    PaletteCommand {
        title: "Insert Line Below",
        shortcut: "o",
        icon: CMD_INSERT_BELOW,
        id: CommandId::InsertBelow,
    },
    PaletteCommand {
        title: "Insert Line Above",
        shortcut: "O",
        icon: CMD_INSERT_ABOVE,
        id: CommandId::InsertAbove,
    },
    PaletteCommand {
        title: "Join Lines",
        shortcut: "J",
        icon: CMD_JOIN,
        id: CommandId::JoinLines,
    },
    PaletteCommand {
        title: "Undo Change",
        shortcut: "u",
        icon: CMD_UNDO,
        id: CommandId::Undo,
    },
    PaletteCommand {
        title: "Redo Change",
        shortcut: "Ctrl-R",
        icon: CMD_REDO,
        id: CommandId::Redo,
    },
    PaletteCommand {
        title: "Yank Selection / Line",
        shortcut: "y",
        icon: CMD_YANK,
        id: CommandId::Yank,
    },
    PaletteCommand {
        title: "Paste from Clipboard",
        shortcut: "p",
        icon: CMD_PASTE,
        id: CommandId::Paste,
    },
    PaletteCommand {
        title: "Toggle Case (Upper/Lower)",
        shortcut: "~",
        icon: CMD_TOGGLE_CASE,
        id: CommandId::ToggleCase,
    },
    PaletteCommand {
        title: "Jump to Top of File",
        shortcut: "gg",
        icon: CMD_JUMP_TOP,
        id: CommandId::JumpTop,
    },
    PaletteCommand {
        title: "Jump to Bottom of File",
        shortcut: "G / ge",
        icon: CMD_JUMP_BOTTOM,
        id: CommandId::JumpBottom,
    },
    PaletteCommand {
        title: "Trigger AI / LSP Completions",
        shortcut: "Ctrl-Space",
        icon: LIGHTBULB,
        id: CommandId::TriggerCompletion,
    },
    PaletteCommand {
        title: "Select Active LSP Server",
        shortcut: ":lsp",
        icon: SETTINGS_COGS,
        id: CommandId::SelectLsp,
    },
    PaletteCommand {
        title: "Select Color Theme",
        shortcut: ":theme",
        icon: THEME,
        id: CommandId::SelectTheme,
    },
    PaletteCommand {
        title: "Save Config to .subject0",
        shortcut: ":cfg",
        icon: GEAR_CONFIG,
        id: CommandId::SaveConfig,
    },
    PaletteCommand {
        title: "Show Keybindings & Help",
        shortcut: "? / :help",
        icon: HELP,
        id: CommandId::ShowHelp,
    },
];

pub struct CommandPalette {
    pub visible: bool,
    pub query: String,
    pub selected_idx: usize,
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

// === File Explorer ===

#[derive(Clone, Debug)]
pub struct FileEntry {
    pub path: PathBuf,
    pub name: String,
    pub is_dir: bool,
    pub depth: usize,
    pub expanded: bool,
}

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

    /// Refreshes the explorer tree while preserving expanded folders across refreshes.
    pub fn refresh(&mut self) {
        let expanded_paths: HashSet<PathBuf> = self
            .entries
            .iter()
            .filter(|e| e.is_dir && e.expanded)
            .map(|e| e.path.clone())
            .collect();

        self.entries = Self::read_directory_recursive(&self.root, 0, &expanded_paths);

        if self.selected_idx >= self.entries.len() && !self.entries.is_empty() {
            self.selected_idx = self.entries.len() - 1;
        }
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

    fn read_directory_recursive(
        dir: &Path,
        depth: usize,
        expanded: &HashSet<PathBuf>,
    ) -> Vec<FileEntry> {
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
                let is_expanded = is_dir && expanded.contains(&p);

                entries.push(FileEntry {
                    path: p.clone(),
                    name,
                    is_dir,
                    depth,
                    expanded: is_expanded,
                });

                if is_expanded {
                    let mut children = Self::read_directory_recursive(&p, depth + 1, expanded);
                    entries.append(&mut children);
                }
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

// === Undo / Redo Snapshot State ===

#[derive(Clone, Debug)]
pub struct UndoSnapshot {
    pub rope: Rope,
    pub cursor_x: usize,
    pub cursor_y: usize,
}

// === Editor Buffer Model ===

#[allow(clippy::struct_excessive_bools)]
pub struct Editor {
    pub rope: Rope,
    pub path: Option<PathBuf>,
    pub mode: Mode,
    pub focus: Focus,
    pub cursor_x: usize,
    pub cursor_y: usize,
    pub scroll_x: usize,
    pub scroll_y: usize,
    pub line_wrap: bool,
    pub clipboard: String,
    pub modified: bool,
    pub saved_undo_len: usize,
    pub status_msg: String,
    pub command_buffer: String,
    pub pending_key: Option<char>,
    pub undo_stack: Vec<UndoSnapshot>,
    pub redo_stack: Vec<UndoSnapshot>,
    pub insert_snapshot_taken: bool,
    pub syntax: SyntaxEngine,
    pub diagnostics: Vec<DiagnosticItem>,
    pub lsp_status: LspStatus,
    pub lsp_tx: Option<mpsc::UnboundedSender<LspInbound>>,
    pub lsp_out_tx: Option<mpsc::UnboundedSender<LspOutbound>>,
    pub spinner_tick: usize,
    pub lsp_req_id: i64,
    pub doc_version: i32,

    // Completions State
    pub completions: Vec<SuggestionItem>,
    pub completion_idx: usize,
    pub completion_scroll: usize,
    pub completion_visible: bool,
    pub completion_rect: Option<(u16, u16, u16, u16)>,
    pub active_lsp_lang: Option<String>,

    // LSP Extended Intelligence State
    pub inlay_hints: Vec<InlayHintItem>,
    pub show_inlay_hints: bool,
    pub hover_info: Option<HoverInfo>,
    pub hover_scroll: usize,
    pub signature_help: Option<SignatureHelpInfo>,
    pub code_action_picker: Option<CodeActionPicker>,
    pub symbol_picker: Option<SymbolPicker>,
    pub location_picker: Option<LocationPicker>,
    pub rename_prompt: Option<String>,

    // Location Navigation History (Jump List)
    pub jump_list: Vec<JumpCheckpoint>,
    pub jump_idx: usize,

    // Subsystems
    pub explorer: FileExplorer,
    pub palette: CommandPalette,
    pub show_help: bool,
    pub help_scroll: usize,
    pub should_quit: bool,

    pub config: AppConfig,
    pub theme: Theme,
    pub theme_picker: Option<ThemePicker>,
    pub lsp_picker: Option<LspPicker>,
}

impl Editor {
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
        let initial_hints = config.show_inlay_hints;
        let theme = Theme::from_name(&config.theme);

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
            theme,
            theme_picker: None,
            lsp_picker: None,

            clipboard: String::new(),
            modified: false,
            saved_undo_len: 0,
            status_msg,
            command_buffer: String::new(),
            pending_key: None,
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
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

            inlay_hints: Vec::new(),
            show_inlay_hints: initial_hints,
            hover_info: None,
            hover_scroll: 0,
            signature_help: None,
            code_action_picker: None,
            symbol_picker: None,
            location_picker: None,
            rename_prompt: None,

            jump_list: Vec::new(),
            jump_idx: 0,

            explorer,
            palette: CommandPalette::new(),
            show_help: false,
            help_scroll: 0,
            should_quit: false,
        })
    }

    /// Switches the active color theme and writes it to `.subject0`.
    pub fn set_theme(&mut self, theme_name: &str) {
        self.theme = Theme::from_name(theme_name);
        self.config.theme = self.theme.name.to_string();
        let _ = self.config.save();
        self.status_msg = format!("Theme: {}", self.theme.display_name);
    }

    pub fn set_mode(&mut self, mode: Mode) {
        if self.mode != mode {
            if matches!(mode, Mode::Insert) {
                self.insert_snapshot_taken = false;
            }
            self.mode = mode;
            self.clamp_cursor();
        }
    }

    fn check_insert_snapshot(&mut self) {
        if !self.insert_snapshot_taken {
            self.snapshot();
            self.insert_snapshot_taken = true;
        }
    }

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

    pub fn cursor_utf16_col(&self) -> usize {
        if self.cursor_y >= self.rope.len_lines() {
            return 0;
        }
        let line = self.rope.line(self.cursor_y).to_string();
        char_to_utf16_col(&line, self.cursor_x)
    }

    /// Records current file position in the jump history.
    pub fn record_jump_checkpoint(&mut self) {
        if let Some(p) = &self.path {
            let cp = JumpCheckpoint {
                path: p.clone(),
                line: self.cursor_y,
                col: self.cursor_x,
            };
            if self.jump_list.last() != Some(&cp) {
                if self.jump_list.len() >= 100 {
                    self.jump_list.remove(0);
                }
                self.jump_list.push(cp);
                self.jump_idx = self.jump_list.len();
            }
        }
    }

    /// Jumps backward through the location jump-list history.
    pub fn jump_backward(&mut self) {
        if self.jump_idx > 0 && !self.jump_list.is_empty() {
            if self.jump_idx == self.jump_list.len() {
                self.record_jump_checkpoint();
                self.jump_idx = self.jump_idx.saturating_sub(1);
            }
            self.jump_idx = self.jump_idx.saturating_sub(1);
            if let Some(cp) = self.jump_list.get(self.jump_idx).cloned() {
                self.jump_to_location(LocationItem {
                    path: cp.path,
                    line: cp.line,
                    col: cp.col,
                });
                self.status_msg = format!(
                    "Jumped back ({}/{})",
                    self.jump_idx + 1,
                    self.jump_list.len()
                );
            }
        } else {
            self.status_msg = "Already at oldest jump position".to_string();
        }
    }

    /// Jumps forward through the location jump-list history.
    pub fn jump_forward(&mut self) {
        if self.jump_idx + 1 < self.jump_list.len() {
            self.jump_idx += 1;
            if let Some(cp) = self.jump_list.get(self.jump_idx).cloned() {
                self.jump_to_location(LocationItem {
                    path: cp.path,
                    line: cp.line,
                    col: cp.col,
                });
                self.status_msg = format!(
                    "Jumped forward ({}/{})",
                    self.jump_idx + 1,
                    self.jump_list.len()
                );
            }
        } else {
            self.status_msg = "Already at newest jump position".to_string();
        }
    }

    /// Loads a new file from disk into the current editor buffer, protecting against unsaved modifications.
    pub fn open_file<P: AsRef<Path>>(&mut self, path: P) -> Result<()> {
        if self.modified {
            return Err(anyhow!(
                "Unsaved changes! Save (:w) or force quit (:q!) first"
            ));
        }

        let path_buf = path.as_ref().to_path_buf();
        let file = File::open(&path_buf)?;
        self.rope = Rope::from_reader(file)?;
        self.path = Some(path_buf.clone());
        self.cursor_x = 0;
        self.cursor_y = 0;
        self.scroll_x = 0;
        self.scroll_y = 0;
        self.modified = false;
        self.saved_undo_len = 0;
        self.undo_stack.clear();
        self.redo_stack.clear();
        self.insert_snapshot_taken = false;
        self.diagnostics.clear();
        self.completion_visible = false;
        self.inlay_hints.clear();
        self.hover_info = None;
        self.signature_help = None;

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
                    self.request_inlay_hints();
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
                    self.request_inlay_hints();
                }
            } else {
                self.ensure_lsp_for_file(&abs_path);
            }
        }
    }

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

    /// Dispatches a reboot request to the active LSP background process.
    pub fn restart_lsp(&mut self) {
        if let Some(tx) = &self.lsp_tx {
            let _ = tx.send(LspInbound::Restart);
            self.status_msg = "Restarting Language Server...".to_string();
        } else if let Some(path) = self.path.clone() {
            self.ensure_lsp_for_file(&path);
        }
    }

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

    pub fn request_inlay_hints(&mut self) {
        if !self.show_inlay_hints {
            return;
        }
        if let Some(tx) = &self.lsp_tx {
            self.lsp_req_id += 1;
            let _ = tx.send(LspInbound::InlayHints {
                req_id: self.lsp_req_id,
                max_lines: self.rope.len_lines(),
            });
        }
    }

    pub fn request_hover(&mut self) {
        if let Some(tx) = &self.lsp_tx {
            self.lsp_req_id += 1;
            let col = self.cursor_utf16_col();
            let _ = tx.send(LspInbound::Hover {
                line: self.cursor_y,
                col,
                req_id: self.lsp_req_id,
            });
            self.status_msg = "Fetching hover docs…".to_string();
        }
    }

    pub fn request_signature_help(&mut self) {
        if let Some(tx) = &self.lsp_tx {
            self.lsp_req_id += 1;
            let col = self.cursor_utf16_col();
            let _ = tx.send(LspInbound::SignatureHelp {
                line: self.cursor_y,
                col,
                req_id: self.lsp_req_id,
            });
        }
    }

    pub fn request_definition(&mut self) {
        self.record_jump_checkpoint();
        if let Some(tx) = &self.lsp_tx {
            self.lsp_req_id += 1;
            let col = self.cursor_utf16_col();
            let _ = tx.send(LspInbound::Definition {
                line: self.cursor_y,
                col,
                req_id: self.lsp_req_id,
            });
            self.status_msg = "Finding definition…".to_string();
        }
    }

    pub fn request_references(&mut self) {
        self.record_jump_checkpoint();
        if let Some(tx) = &self.lsp_tx {
            self.lsp_req_id += 1;
            let col = self.cursor_utf16_col();
            let _ = tx.send(LspInbound::References {
                line: self.cursor_y,
                col,
                req_id: self.lsp_req_id,
            });
            self.status_msg = "Searching references…".to_string();
        }
    }

    pub fn request_formatting(&mut self) {
        if let Some(tx) = &self.lsp_tx {
            self.lsp_req_id += 1;
            let _ = tx.send(LspInbound::Formatting {
                req_id: self.lsp_req_id,
            });
            self.status_msg = "Formatting buffer with LSP…".to_string();
        }
    }

    pub fn request_code_actions(&mut self) {
        if let Some(tx) = &self.lsp_tx {
            self.lsp_req_id += 1;
            let col = self.cursor_utf16_col();
            let cur_y = self.cursor_y;
            let diags: Vec<DiagnosticItem> = self
                .diagnostics
                .iter()
                .filter(|d| d.line == cur_y)
                .cloned()
                .collect();

            let _ = tx.send(LspInbound::CodeAction {
                line: cur_y,
                col,
                diagnostics: diags,
                req_id: self.lsp_req_id,
            });
            self.status_msg = "Querying code actions…".to_string();
        }
    }

    pub fn request_rename(&mut self, new_name: &str) {
        if new_name.trim().is_empty() {
            self.status_msg = "Rename aborted: empty identifier name".to_string();
            return;
        }
        if let Some(tx) = &self.lsp_tx {
            self.lsp_req_id += 1;
            let col = self.cursor_utf16_col();
            let _ = tx.send(LspInbound::Rename {
                line: self.cursor_y,
                col,
                new_name: new_name.to_string(),
                req_id: self.lsp_req_id,
            });
            self.status_msg = format!("Renaming to '{new_name}'…");
        }
    }

    pub fn request_document_symbols(&mut self) {
        if let Some(tx) = &self.lsp_tx {
            self.lsp_req_id += 1;
            let _ = tx.send(LspInbound::DocumentSymbol {
                req_id: self.lsp_req_id,
            });
            self.status_msg = "Loading document symbols…".to_string();
        }
    }

    /// Navigates to the next diagnostic in the document.
    pub fn next_diagnostic(&mut self) {
        if self.diagnostics.is_empty() {
            self.status_msg = "No diagnostics in buffer".to_string();
            return;
        }
        let cur_y = self.cursor_y;
        let target = self
            .diagnostics
            .iter()
            .find(|d| d.line > cur_y)
            .or_else(|| self.diagnostics.first())
            .cloned();

        if let Some(d) = target {
            self.cursor_y = d.line.min(self.rope.len_lines().saturating_sub(1));
            let line_str = self.rope.line(self.cursor_y).to_string();
            self.cursor_x = utf16_to_char_col(&line_str, d.col);
            self.clamp_cursor();
            self.status_msg = format!("Diagnostic: {}", d.message);
        }
    }

    /// Navigates to the previous diagnostic in the document.
    pub fn prev_diagnostic(&mut self) {
        if self.diagnostics.is_empty() {
            self.status_msg = "No diagnostics in buffer".to_string();
            return;
        }
        let cur_y = self.cursor_y;
        let target = self
            .diagnostics
            .iter()
            .rev()
            .find(|d| d.line < cur_y)
            .or_else(|| self.diagnostics.last())
            .cloned();

        if let Some(d) = target {
            self.cursor_y = d.line.min(self.rope.len_lines().saturating_sub(1));
            let line_str = self.rope.line(self.cursor_y).to_string();
            self.cursor_x = utf16_to_char_col(&line_str, d.col);
            self.clamp_cursor();
            self.status_msg = format!("Diagnostic: {}", d.message);
        }
    }

    /// Converts an LSP position (0-based line, UTF-16 character column) into a character index.
    fn lsp_pos_to_char_index(rope: &Rope, line: usize, utf16_col: usize) -> usize {
        let total_lines = rope.len_lines();
        if line >= total_lines {
            return rope.len_chars();
        }

        let line_start_char = rope.line_to_char(line);
        let line_slice = rope.line(line);
        let line_str = line_slice.to_string();
        let char_offset = utf16_to_char_col(&line_str, utf16_col);

        (line_start_char + char_offset).min(rope.len_chars())
    }

    /// Applies a collection of text edits to an arbitrary Rope buffer in descending order.
    fn apply_edits_to_rope(rope: &mut Rope, edits: &[TextEditItem]) {
        let mut indexed_edits: Vec<(usize, usize, &str)> = edits
            .iter()
            .map(|e| {
                let start_idx = Self::lsp_pos_to_char_index(rope, e.start_line, e.start_col);
                let end_idx = Self::lsp_pos_to_char_index(rope, e.end_line, e.end_col);
                (start_idx, end_idx, e.new_text.as_str())
            })
            .collect();

        // Descending order guarantees offset invariance for earlier edits
        indexed_edits.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.cmp(&a.1)));

        for (start_idx, end_idx, new_text) in indexed_edits {
            let safe_start = start_idx.min(rope.len_chars());
            let safe_end = end_idx.max(safe_start).min(rope.len_chars());

            if safe_start < safe_end {
                rope.remove(safe_start..safe_end);
            }
            if !new_text.is_empty() {
                rope.insert(safe_start, new_text);
            }
        }
    }

    /// Applies a collection of LSP text edits cleanly to the current editor buffer.
    pub fn apply_text_edits(&mut self, edits: &[TextEditItem]) {
        if edits.is_empty() {
            return;
        }

        self.snapshot();
        Self::apply_edits_to_rope(&mut self.rope, edits);

        self.modified = true;
        self.clamp_cursor();
        self.on_buffer_modified();
    }

    /// Dispatches a project-wide `WorkspaceEdit` map cleanly across disk and memory.
    pub fn apply_workspace_edits(
        &mut self,
        changes: &HashMap<PathBuf, Vec<TextEditItem>>,
    ) -> Result<usize> {
        let mut total_applied = 0;
        let current_canon = self.path.as_ref().and_then(|p| p.canonicalize().ok());

        for (path, edits) in changes {
            if edits.is_empty() {
                continue;
            }

            let is_current = current_canon
                .as_ref()
                .is_some_and(|cur| path.canonicalize().ok().as_ref() == Some(cur))
                || self.path.as_ref() == Some(path);

            if is_current {
                self.apply_text_edits(edits);
                total_applied += edits.len();
            } else if path.exists() {
                let file = File::open(path)?;
                let mut ext_rope = Rope::from_reader(file)?;
                Self::apply_edits_to_rope(&mut ext_rope, edits);

                let tmp_path = path.with_extension("s0_wedit_tmp");
                {
                    let out_file = File::create(&tmp_path)?;
                    let mut writer = BufWriter::new(out_file);
                    for chunk in ext_rope.chunks() {
                        writer.write_all(chunk.as_bytes())?;
                    }
                    writer.flush()?;
                }
                if fs::rename(&tmp_path, path).is_err() {
                    fs::copy(&tmp_path, path)?;
                    let _ = fs::remove_file(&tmp_path);
                }
                total_applied += edits.len();
            }
        }

        Ok(total_applied)
    }

    /// Jumps directly to a target location (file, line, col), opening the target if external.
    #[allow(clippy::needless_pass_by_value)]
    pub fn jump_to_location(&mut self, loc: LocationItem) {
        let is_current = self
            .path
            .as_ref()
            .is_some_and(|p| p.canonicalize().ok() == loc.path.canonicalize().ok());

        if is_current {
            self.cursor_y = loc.line.min(self.rope.len_lines().saturating_sub(1));
            let line_str = self.rope.line(self.cursor_y).to_string();
            self.cursor_x = utf16_to_char_col(&line_str, loc.col);
            self.clamp_cursor();
            self.status_msg = format!("Jumped to line {}", self.cursor_y + 1);
        } else {
            match self.open_file(&loc.path) {
                Ok(()) => {
                    self.cursor_y = loc.line.min(self.rope.len_lines().saturating_sub(1));
                    let line_str = self.rope.line(self.cursor_y).to_string();
                    self.cursor_x = utf16_to_char_col(&line_str, loc.col);
                    self.clamp_cursor();
                    self.status_msg = format!(
                        "Opened {} at line {}",
                        loc.path.file_name().unwrap_or_default().to_string_lossy(),
                        self.cursor_y + 1
                    );
                }
                Err(e) => {
                    self.status_msg = format!("Failed to jump to {}: {e}", loc.path.display());
                }
            }
        }
    }

    /// Pushes current buffer state into the undo stack and clears redo history.
    pub fn snapshot(&mut self) {
        if self.undo_stack.len() >= 64 {
            self.undo_stack.remove(0);
            self.saved_undo_len = self.saved_undo_len.saturating_sub(1);
        }
        self.undo_stack.push(UndoSnapshot {
            rope: self.rope.clone(),
            cursor_x: self.cursor_x,
            cursor_y: self.cursor_y,
        });
        self.redo_stack.clear();
    }

    /// Reverts the document rope to the most recent checkpoint on `undo_stack`.
    pub fn undo(&mut self) {
        if let Some(prev) = self.undo_stack.pop() {
            if self.redo_stack.len() >= 64 {
                self.redo_stack.remove(0);
            }
            self.redo_stack.push(UndoSnapshot {
                rope: self.rope.clone(),
                cursor_x: self.cursor_x,
                cursor_y: self.cursor_y,
            });

            self.rope = prev.rope;
            self.cursor_x = prev.cursor_x;
            self.cursor_y = prev.cursor_y;
            self.modified = self.undo_stack.len() != self.saved_undo_len;
            self.status_msg = "Reverted change".to_string();
            self.insert_snapshot_taken = false;
            self.clamp_cursor();
            self.on_buffer_modified();
        } else {
            self.status_msg = "Already at oldest change".to_string();
        }
    }

    /// Steps forward through historical edits using `redo_stack`.
    pub fn redo(&mut self) {
        if let Some(next) = self.redo_stack.pop() {
            if self.undo_stack.len() >= 64 {
                self.undo_stack.remove(0);
                self.saved_undo_len = self.saved_undo_len.saturating_sub(1);
            }
            self.undo_stack.push(UndoSnapshot {
                rope: self.rope.clone(),
                cursor_x: self.cursor_x,
                cursor_y: self.cursor_y,
            });

            self.rope = next.rope;
            self.cursor_x = next.cursor_x;
            self.cursor_y = next.cursor_y;
            self.modified = self.undo_stack.len() != self.saved_undo_len;
            self.status_msg = "Redone change".to_string();
            self.insert_snapshot_taken = false;
            self.clamp_cursor();
            self.on_buffer_modified();
        } else {
            self.status_msg = "Already at newest change".to_string();
        }
    }

    /// Shifts diagnostic coordinates down when lines are added strictly below them.
    fn shift_diagnostics_down(&mut self, after_line: usize, count: usize) {
        for d in &mut self.diagnostics {
            if d.line > after_line {
                d.line += count;
                d.end_line += count;
            }
        }
    }

    /// Shifts diagnostic coordinates down when lines are inserted at or above them.
    fn shift_diagnostics_down_from(&mut self, from_line: usize, count: usize) {
        for d in &mut self.diagnostics {
            if d.line >= from_line {
                d.line += count;
                d.end_line += count;
            }
        }
    }

    /// Shifts diagnostic coordinates up when a line above them is removed.
    fn shift_diagnostics_up(&mut self, removed_line: usize) {
        self.diagnostics
            .retain(|d| d.line != removed_line && d.end_line != removed_line);
        for d in &mut self.diagnostics {
            if d.line > removed_line {
                d.line = d.line.saturating_sub(1);
            }
            if d.end_line > removed_line {
                d.end_line = d.end_line.saturating_sub(1);
            }
        }
    }

    pub fn on_buffer_modified(&mut self) {
        let text = self.rope.to_string();
        self.syntax.reparse(&text);
        self.doc_version = self.doc_version.wrapping_add(1);

        let max_lines = self.rope.len_lines().max(1);
        self.diagnostics.retain(|d| d.line < max_lines);

        if let Some(tx) = &self.lsp_tx {
            let _ = tx.send(LspInbound::Change {
                text,
                version: self.doc_version,
            });
            self.request_semantic_tokens();
            self.request_inlay_hints();
        }
    }

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

    pub fn current_line_len(&self) -> usize {
        line_len(&self.rope, self.cursor_y)
    }

    pub fn char_index(&self) -> usize {
        let line_start = self.rope.line_to_char(self.cursor_y);
        line_start + self.cursor_x
    }

    pub fn char_under_cursor(&self) -> Option<char> {
        let idx = self.char_index();
        if idx < self.rope.len_chars() {
            Some(self.rope.char(idx))
        } else {
            None
        }
    }

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

    pub fn paste(&mut self) {
        if self.clipboard.is_empty() {
            self.status_msg = "Clipboard is empty".to_string();
            return;
        }

        self.snapshot();

        if let Some((start, end)) = self.selection_range()
            && start < end
        {
            self.rope.remove(start..end);
            self.rope.insert(start, &self.clipboard);
            let end_idx = start + self.clipboard.chars().count();
            let new_line = self.rope.char_to_line(end_idx);
            let line_start = self.rope.line_to_char(new_line);
            self.cursor_y = new_line;
            self.cursor_x = end_idx.saturating_sub(line_start);
            self.set_mode(Mode::Normal);
            self.modified = true;
            self.clamp_cursor();
            self.on_buffer_modified();
            self.status_msg = "Pasted from clipboard".to_string();
            return;
        }

        if self.clipboard.ends_with('\n') {
            let insert_line = self.cursor_y + 1;
            let num_lines = self.rope.len_lines();
            let insert_idx = if insert_line < num_lines {
                self.rope.line_to_char(insert_line)
            } else {
                self.rope.len_chars()
            };

            let text = if insert_idx == self.rope.len_chars()
                && self.rope.len_chars() > 0
                && self.rope.char(self.rope.len_chars() - 1) != '\n'
            {
                let le = self.detect_line_ending();
                format!("{le}{}", self.clipboard.trim_end_matches(['\r', '\n']))
            } else {
                self.clipboard.clone()
            };

            self.rope.insert(insert_idx, &text);
            self.shift_diagnostics_down(self.cursor_y, text.lines().count().max(1));
            self.cursor_y = (self.cursor_y + 1).min(self.rope.len_lines().saturating_sub(1));
            self.cursor_x = 0;
        } else {
            let idx = self.char_index().min(self.rope.len_chars());
            self.rope.insert(idx, &self.clipboard);
            let end_idx = idx + self.clipboard.chars().count();
            let new_line = self.rope.char_to_line(end_idx);
            let line_start = self.rope.line_to_char(new_line);
            self.cursor_y = new_line;
            self.cursor_x = end_idx.saturating_sub(line_start);
        }

        self.modified = true;
        self.clamp_cursor();
        self.on_buffer_modified();
        self.status_msg = "Pasted from clipboard".to_string();
    }

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

        self.shift_diagnostics_up(self.cursor_y + 1);

        self.cursor_y = self.rope.char_to_line(insert_pos);
        let line_start = self.rope.line_to_char(self.cursor_y);
        self.cursor_x = insert_pos.saturating_sub(line_start);

        self.modified = true;
        self.clamp_cursor();
        self.on_buffer_modified();
        self.status_msg = "Joined lines".to_string();
    }

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
        self.shift_diagnostics_down(self.cursor_y, 1);
        self.cursor_y += 1;
        self.cursor_x = indent.chars().count();
        self.modified = true;
        self.set_mode(Mode::Insert);
        self.insert_snapshot_taken = true;
        self.completion_visible = false;
        self.on_buffer_modified();
    }

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
        self.shift_diagnostics_down_from(self.cursor_y, 1);
        self.cursor_x = indent.chars().count();
        self.modified = true;
        self.set_mode(Mode::Insert);
        self.insert_snapshot_taken = true;
        self.completion_visible = false;
        self.on_buffer_modified();
    }

    pub fn insert_char(&mut self, c: char) {
        self.check_insert_snapshot();
        let idx = self.char_index();
        self.rope.insert_char(idx, c);
        self.cursor_x += 1;
        self.modified = true;
        self.on_buffer_modified();
    }

    pub fn insert_pair(&mut self, open: char, close: char) {
        self.check_insert_snapshot();
        let idx = self.char_index();
        self.rope.insert_char(idx, open);
        self.rope.insert_char(idx + 1, close);
        self.cursor_x += 1;
        self.modified = true;
        self.on_buffer_modified();
    }

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
            self.shift_diagnostics_down(self.cursor_y, 2);
            self.cursor_y += 1;
            self.cursor_x = inner_indent.chars().count();
        } else {
            let to_insert = format!("{le}{indent}");
            self.rope.insert(idx, &to_insert);
            self.shift_diagnostics_down(self.cursor_y, 1);
            self.cursor_y += 1;
            self.cursor_x = inner_indent_len(&indent);
        }

        self.modified = true;
        self.completion_visible = false;
        self.on_buffer_modified();
    }

    /// Dedents current line by up to 4 spaces or 1 tab.
    pub fn dedent_current_line(&mut self) {
        if self.cursor_y >= self.rope.len_lines() {
            return;
        }
        self.check_insert_snapshot();
        let line_start = self.rope.line_to_char(self.cursor_y);
        let line = self.rope.line(self.cursor_y);
        let mut spaces_to_remove = 0;
        for ch in line.chars() {
            if ch == '\t' {
                spaces_to_remove = 1;
                break;
            } else if ch == ' ' {
                spaces_to_remove += 1;
                if spaces_to_remove == 4 {
                    break;
                }
            } else {
                break;
            }
        }
        if spaces_to_remove > 0 {
            self.rope.remove(line_start..line_start + spaces_to_remove);
            self.cursor_x = self.cursor_x.saturating_sub(spaces_to_remove);
            self.modified = true;
            self.on_buffer_modified();
        }
    }

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

            self.shift_diagnostics_up(self.cursor_y);

            self.cursor_y -= 1;
            self.cursor_x = prev_len;
            self.modified = true;
            self.on_buffer_modified();
        }

        self.completion_visible = false;
    }

    pub fn update_completion_scroll(&mut self, max_visible: usize) {
        if self.completion_idx < self.completion_scroll {
            self.completion_scroll = self.completion_idx;
        } else if self.completion_idx >= self.completion_scroll + max_visible {
            self.completion_scroll = self.completion_idx + 1 - max_visible;
        }
    }

    pub fn accept_completion(&mut self) {
        if !self.completion_visible || self.completions.is_empty() {
            return;
        }

        self.snapshot();
        let item = self.completions[self.completion_idx].clone();
        let replacement = item.insert_text;
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

        // Apply any auto-import edits provided by the language server
        if !item.additional_text_edits.is_empty() {
            self.apply_text_edits(&item.additional_text_edits);
        }

        self.modified = true;
        self.completion_visible = false;
        self.clamp_cursor();
        self.on_buffer_modified();
    }

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

        let current_y = self.cursor_y;
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

        self.shift_diagnostics_up(current_y);
        self.modified = true;
        self.status_msg = "Cut line".to_string();
        self.on_buffer_modified();

        if self.cursor_y >= self.rope.len_lines() && self.cursor_y > 0 {
            self.cursor_y -= 1;
        }
        self.clamp_cursor();
    }

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

            if let Err(_e) = fs::rename(&tmp_path, path) {
                fs::copy(&tmp_path, path)?;
                let _ = fs::remove_file(&tmp_path);
            }

            self.modified = false;
            self.saved_undo_len = self.undo_stack.len();
            self.status_msg = format!("Saved {}", path.display());

            if let Some(tx) = &self.lsp_tx {
                let _ = tx.send(LspInbound::Save);
                self.request_semantic_tokens();
                self.request_inlay_hints();
            }
            Ok(())
        } else {
            self.status_msg = "No file name (use :w <name>)".to_string();
            Err(anyhow!("No file name specified"))
        }
    }

    pub fn execute_palette_command(&mut self, id: CommandId) {
        self.palette.visible = false;
        match id {
            CommandId::FormatDocument => self.request_formatting(),
            CommandId::ShowHover => self.request_hover(),
            CommandId::CodeActions => self.request_code_actions(),
            CommandId::GoToDefinition => self.request_definition(),
            CommandId::FindReferences => self.request_references(),
            CommandId::DocumentSymbols => self.request_document_symbols(),
            CommandId::RenameSymbol => {
                self.rename_prompt = Some(self.current_word_prefix());
            }
            CommandId::NextDiagnostic => self.next_diagnostic(),
            CommandId::PrevDiagnostic => self.prev_diagnostic(),
            CommandId::JumpBackward => self.jump_backward(),
            CommandId::JumpForward => self.jump_forward(),
            CommandId::RestartLsp => self.restart_lsp(),
            CommandId::ToggleInlayHints => {
                self.show_inlay_hints = !self.show_inlay_hints;
                self.config.show_inlay_hints = self.show_inlay_hints;
                let _ = self.config.save();
                if self.show_inlay_hints {
                    self.request_inlay_hints();
                    self.status_msg = "Inlay Hints: ON".to_string();
                } else {
                    self.inlay_hints.clear();
                    self.status_msg = "Inlay Hints: OFF".to_string();
                }
            }
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
                if let Err(e) = self.save() {
                    self.status_msg = format!("Error saving: {e}");
                }
            }
            CommandId::SaveQuit => match self.save() {
                Ok(()) => self.should_quit = true,
                Err(e) => self.status_msg = format!("Error saving: {e}"),
            },
            CommandId::Quit => {
                if self.modified {
                    self.status_msg = "Unsaved changes! Use :q! to force quit".to_string();
                } else {
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
            CommandId::Redo => self.redo(),
            CommandId::Yank => self.yank_selection(),
            CommandId::Paste => self.paste(),
            CommandId::ToggleCase => self.toggle_case(),
            CommandId::JumpTop => {
                self.cursor_y = 0;
                self.cursor_x = 0;
                self.scroll_y = 0;
                self.scroll_x = 0;
            }
            CommandId::JumpBottom => {
                let max_lines = self.rope.len_lines().max(1);
                self.cursor_y = max_lines - 1;
                self.cursor_x = 0;
            }
            CommandId::TriggerCompletion => {
                self.set_mode(Mode::Insert);
                self.request_completions();
            }
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
            CommandId::SelectTheme => {
                let cur_idx = Theme::all()
                    .iter()
                    .position(|t| t.name == self.theme.name)
                    .unwrap_or(0);
                self.theme_picker = Some(ThemePicker {
                    selected_idx: cur_idx,
                });
            }
            CommandId::SaveConfig => {
                self.config.line_wrap = self.line_wrap;
                self.config.theme = self.theme.name.to_string();
                self.config.show_inlay_hints = self.show_inlay_hints;
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

    pub fn execute_command(&mut self) {
        let cmd = self.command_buffer.trim().to_string();
        self.command_buffer.clear();

        let (action, arg) = match cmd.find(char::is_whitespace) {
            Some(idx) => (&cmd[..idx], Some(cmd[idx..].trim())),
            None => (cmd.as_str(), None),
        };

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
                if let Some(filename) = arg.filter(|s| !s.is_empty()) {
                    self.switch_file_target(PathBuf::from(filename));
                }
                if let Err(e) = self.save() {
                    self.status_msg = format!("Error saving: {e}");
                }
            }
            "wq" | "wq!" => {
                if let Some(filename) = arg.filter(|s| !s.is_empty()) {
                    self.switch_file_target(PathBuf::from(filename));
                }
                match self.save() {
                    Ok(()) => self.should_quit = true,
                    Err(e) => self.status_msg = format!("Error saving: {e}"),
                }
            }
            "wrap" => {
                self.line_wrap = !self.line_wrap;
                self.status_msg =
                    format!("Line Wrap: {}", if self.line_wrap { "ON" } else { "OFF" });
            }
            "fmt" | "format" => {
                self.request_formatting();
            }
            "hover" | "doc" => {
                self.request_hover();
            }
            "ca" | "action" | "codeaction" => {
                self.request_code_actions();
            }
            "sym" | "symbols" => {
                self.request_document_symbols();
            }
            "def" | "definition" => {
                self.request_definition();
            }
            "ref" | "references" => {
                self.request_references();
            }
            "nd" | "next-diag" => {
                self.next_diagnostic();
            }
            "pd" | "prev-diag" => {
                self.prev_diagnostic();
            }
            "lsp-restart" | "restart" => {
                self.restart_lsp();
            }
            "hints" | "inlay" => {
                self.show_inlay_hints = !self.show_inlay_hints;
                self.config.show_inlay_hints = self.show_inlay_hints;
                let _ = self.config.save();
                if self.show_inlay_hints {
                    self.request_inlay_hints();
                    self.status_msg = "Inlay Hints: ON".to_string();
                } else {
                    self.inlay_hints.clear();
                    self.status_msg = "Inlay Hints: OFF".to_string();
                }
            }
            "rn" | "rename" => {
                if let Some(name) = arg.filter(|s| !s.is_empty()) {
                    self.request_rename(name);
                } else {
                    self.rename_prompt = Some(self.current_word_prefix());
                }
            }
            "theme" | "colorscheme" => {
                if let Some(name) = arg.filter(|s| !s.is_empty()) {
                    self.set_theme(name);
                } else {
                    let cur_idx = Theme::all()
                        .iter()
                        .position(|t| t.name == self.theme.name)
                        .unwrap_or(0);
                    self.theme_picker = Some(ThemePicker {
                        selected_idx: cur_idx,
                    });
                }
            }
            "u" | "undo" => self.undo(),
            "redo" => self.redo(),
            "lsp" => {
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
            "cfg" => {
                self.config.line_wrap = self.line_wrap;
                self.config.theme = self.theme.name.to_string();
                self.config.show_inlay_hints = self.show_inlay_hints;
                if self.config.save().is_ok() {
                    self.status_msg = "Config saved to .subject0".to_string();
                } else {
                    self.status_msg = "Failed to write .subject0".to_string();
                }
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

    pub fn clamp_cursor(&mut self) {
        let max_lines = self.rope.len_lines().max(1);
        if self.cursor_y >= max_lines {
            self.cursor_y = max_lines - 1;
        }

        let line_len = self.current_line_len();
        let max_x = match self.mode {
            Mode::Insert | Mode::Visual { .. } => line_len,
            Mode::Normal | Mode::Command => line_len.saturating_sub(1),
        };

        if self.cursor_x > max_x {
            self.cursor_x = max_x;
        }
    }

    /// Viewport updater that guarantees instant response on large files.
    pub fn update_scroll(&mut self, width: usize, height: usize) {
        if height == 0 || width == 0 {
            return;
        }

        let total_lines = self.rope.len_lines().max(1);
        if self.cursor_y >= total_lines {
            self.cursor_y = total_lines - 1;
        }

        if self.cursor_y < self.scroll_y {
            self.scroll_y = self.cursor_y;
        }

        if self.line_wrap {
            self.scroll_x = 0;
            let effective_width = width.max(1);

            let line_visual_rows = |y: usize, rope: &Rope| -> usize {
                if y >= rope.len_lines() {
                    return 1;
                }
                let len = line_len(rope, y);
                if len == 0 {
                    1
                } else {
                    len.div_ceil(effective_width)
                }
            };

            let cur_line_rows = line_visual_rows(self.cursor_y, &self.rope);
            let cur_sub_row =
                (self.cursor_x / effective_width).min(cur_line_rows.saturating_sub(1));

            let mut visual_rows_down = 0;
            for y in self.scroll_y..self.cursor_y {
                visual_rows_down += line_visual_rows(y, &self.rope);
                if visual_rows_down >= height {
                    break;
                }
            }
            visual_rows_down += cur_sub_row;

            if visual_rows_down >= height {
                let mut accumulated = cur_sub_row + 1;
                let mut new_top = self.cursor_y;

                while new_top > 0 && accumulated < height {
                    let prev_rows = line_visual_rows(new_top - 1, &self.rope);
                    if accumulated + prev_rows > height {
                        break;
                    }
                    accumulated += prev_rows;
                    new_top -= 1;
                }
                self.scroll_y = new_top;
            }
        } else {
            if self.cursor_y >= self.scroll_y + height {
                self.scroll_y = self.cursor_y.saturating_sub(height - 1);
            }

            if self.cursor_x < self.scroll_x {
                self.scroll_x = self.cursor_x;
            } else if self.cursor_x >= self.scroll_x + width {
                self.scroll_x = self.cursor_x.saturating_sub(width - 1);
            }
        }
    }
}

fn inner_indent_len(indent: &str) -> usize {
    indent.chars().count()
}

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
