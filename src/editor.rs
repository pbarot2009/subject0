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
//!    Maintains a ring-bounded history of historical [`Rope`] states (capped at 64 entries).
//!    Because `Rope` clones share immutable internal B-tree nodes, pushing snapshots avoids
//!    deep string allocations.
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

use anyhow::Result;
use ropey::Rope;
use tokio::sync::mpsc;

use crate::lsp::{DiagnosticItem, LspInbound, LspStatus, SuggestionItem, SyntaxEngine};

/// Active input mode governing key event interpretation and cursor boundary constraints.
#[derive(PartialEq, Eq, Clone, Copy)]
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
#[derive(PartialEq, Eq, Clone, Copy)]
pub enum Focus {
    /// Main code buffer viewport.
    Editor,
    /// Side-panel file tree explorer viewport.
    Explorer,
}

// -----------------------------------------------------------------------------
// Command Palette Definitions
// -----------------------------------------------------------------------------

/// Enumeration of all discrete operations dispatchable via the interactive command palette.
#[derive(Clone, Copy, PartialEq, Eq)]
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
    /// Pastes text from the internal clipboard at the current cursor offset.
    ToggleCase,
    /// Swaps character casing (uppercase <-> lowercase) over selection or character.
    Paste,
    /// Relocates the cursor to the first character of the first line.
    JumpTop,
    /// Relocates the cursor to the first character of the final line.
    JumpBottom,
    /// Dispatches an asynchronous LSP completion request at the current cursor position.
    TriggerCompletion,
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
    /// Creates a closed, empty command palette instance.
    pub fn new() -> Self {
        Self {
            visible: false,
            query: String::new(),
            selected_idx: 0,
            scroll: 0,
        }
    }

    /// Evaluates `self.query` against [`PALETTE_COMMANDS`] and returns matching candidates.
    ///
    /// Matches case-insensitively against both command titles and shortcut strings.
    /// Returns the complete registry if the query buffer is empty.
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

// -----------------------------------------------------------------------------
// File Explorer
// -----------------------------------------------------------------------------

/// Represents a single node (file or directory) within the hierarchical file tree.
#[derive(Clone, Debug)]
pub struct FileEntry {
    /// Full filesystem path to the target node.
    pub path: PathBuf,
    /// Terminal display name (filename or directory name).
    pub name: String,
    /// True if the entry represents a directory; false for regular files and symlinks.
    pub is_dir: bool,
    /// Indentation depth relative to the root directory (0 = root level children).
    pub depth: usize,
    /// If `is_dir` is true, tracks whether sub-entries are currently materialized.
    pub expanded: bool,
}

/// Sidebar file tree model supporting dynamic directory expansion and navigation.
pub struct FileExplorer {
    /// Base directory path serving as the hierarchy root.
    pub root: PathBuf,
    /// Flattened display list containing all visible nodes, including expanded subtrees.
    pub entries: Vec<FileEntry>,
    /// Index within `entries` representing the currently focused row.
    pub selected_idx: usize,
    /// Vertical viewport scroll offset for the explorer sidebar.
    pub scroll: usize,
    /// Controls whether the file explorer sidebar is visible.
    pub visible: bool,
}

impl FileExplorer {
    /// Constructs a file explorer rooted at the provided filesystem path and reads top-level entries.
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

    /// Rescans the root directory, collapsing expanded subtrees and resetting `entries`.
    pub fn refresh(&mut self) {
        self.entries = Self::read_directory(&self.root, 0);
    }

    /// Reads immediate children of `dir`, sorting directories before files, and ignores noise.
    ///
    /// Filters out:
    /// - Hidden files and directories starting with `.` (e.g., `.git`)
    /// - Build artifact directories (`target`, `node_modules`)
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

            // Sort directories before files, then sort alphabetically by filename.
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

    /// Toggles the expansion state of a directory entry at `idx`.
    ///
    /// - If currently expanded: Traverses forward, counting all descendants with
    ///   `depth > current_depth`, and removes them from the flattened vector via [`Vec::drain`].
    /// - If collapsed: Reads the directory contents from disk at `current_depth + 1` and
    ///   splices them directly following `idx`.
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
        } else {
            self.entries[idx].expanded = true;
            let current_depth = self.entries[idx].depth;
            let dir_path = self.entries[idx].path.clone();
            let children = Self::read_directory(&dir_path, current_depth + 1);

            let insert_pos = idx + 1; self.entries.splice(insert_pos..insert_pos, children);
        }
    }

    /// Adjusts `self.scroll` to keep `self.selected_idx` within the visible vertical window.
    pub fn update_scroll(&mut self, viewport_height: usize) {
        if self.selected_idx < self.scroll {
            self.scroll = self.selected_idx;
        } else if self.selected_idx >= self.scroll + viewport_height {
            self.scroll = self.selected_idx + 1 - viewport_height;
        }
    }
}

// -----------------------------------------------------------------------------
// Editor Buffer Model
// -----------------------------------------------------------------------------

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
    /// Undo history ring storing previous [`Rope`] states (bounded to 64 snapshots).
    pub undo_stack: Vec<Rope>,
    /// Syntax engine instance driving AST parsing and lexical syntax highlighting.
    pub syntax: SyntaxEngine,
    /// Diagnostics received from the background LSP server mapped to the active document.
    pub diagnostics: Vec<DiagnosticItem>,
    /// Process lifecycle status of the associated LSP server.
    pub lsp_status: LspStatus,
    /// Transmission channel dispatching requests to the background LSP actor.
    pub lsp_tx: Option<mpsc::UnboundedSender<LspInbound>>,
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

    // File Tree & Command Palette
    /// File tree explorer state machine.
    pub explorer: FileExplorer,
    /// Interactive command palette state machine.
    pub palette: CommandPalette,
    /// Flag signaling the main application event loop to shut down.
    pub should_quit: bool,
}

impl Editor {
    /// Instantiates a new editor buffer.
    ///
    /// If `path` is provided and points to an existing file, its contents are ingested into
    /// the [`Rope`] via buffered I/O. Otherwise, an empty rope is initialized. Also sets up
    /// the syntax engine, initializes the file explorer from the current working directory,
    /// and resets cursor coordinates.
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

        let root_dir = env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let explorer = FileExplorer::new(root_dir);

        Ok(Self {
            rope,
            path,
            mode: Mode::Normal,
            focus: Focus::Editor,
            cursor_x: 0,
            cursor_y: 0,
            scroll_x: 0,
            scroll_y: 0,
            line_wrap: true,
            clipboard: String::new(),
            modified: false,
            status_msg,
            command_buffer: String::new(),
            pending_key: None,
            undo_stack: Vec::new(),
            syntax,
            diagnostics: Vec::new(),
            lsp_status: LspStatus::Disabled,
            lsp_tx: None,
            lsp_req_id: 10,
            doc_version: 1,
            completions: Vec::new(),
            completion_idx: 0,
            completion_scroll: 0,
            completion_visible: false,
            explorer,
            palette: CommandPalette::new(),
            should_quit: false,
        })
    }

    /// Loads a new file from disk into the current editor buffer.
    ///
    /// Replaces the underlying [`Rope`], resets cursor and scroll coordinates, clears the
    /// undo stack and diagnostics, reconfigures the [`SyntaxEngine`], and sends a
    /// `textDocument/didOpen` notification over `lsp_tx` if a language server is connected.
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
        self.diagnostics.clear();
        self.completion_visible = false;

        self.syntax = SyntaxEngine::new(Some(&path_buf));
        let text = self.rope.to_string();
        self.syntax.reparse(&text);
        self.doc_version = 1;

        if let Some(tx) = &self.lsp_tx {
            let ext = path_buf.extension().and_then(|e| e.to_str()).unwrap_or("");
            let lang_id = match ext {
                "rs" => "rust",
                "py" => "python",
                _ => "",
            };
            if !lang_id.is_empty() {
                let _ = tx.send(LspInbound::OpenFile {
                    path: path_buf.clone(),
                    text,
                    lang_id: lang_id.to_string(),
                });
            }
        }

        self.status_msg = format!("Opened {}", path_buf.display());
        Ok(())
    }

    /// Pushes a snapshot of the current [`Rope`] onto `undo_stack`.
    ///
    /// History depth is bounded to 64 entries. When capacity is exceeded, the oldest
    /// snapshot is dropped. Clones of [`Rope`] are cheap $O(1)$ operations due to internal
    /// structural sharing.
    pub fn snapshot(&mut self) {
        if self.undo_stack.len() >= 64 {
            self.undo_stack.remove(0);
        }
        self.undo_stack.push(self.rope.clone());
    }

    /// Reverts the document rope to the most recent checkpoint on `undo_stack`.
    ///
    /// Clamps cursor coordinates to the reverted buffer boundaries and triggers
    /// re-parsing and LSP change notifications via [`Self::on_buffer_modified`].
    pub fn undo(&mut self) {
        if let Some(prev) = self.undo_stack.pop() {
            self.rope = prev;
            self.modified = true;
            self.status_msg = "Reverted change".to_string();
            self.clamp_cursor();
            self.on_buffer_modified();
        }
    }

    /// Synchronizes external subsystems following a buffer mutation.
    ///
    /// 1. Regenerates AST highlighting caches via [`SyntaxEngine::reparse`].
    /// 2. Increments [`Self::doc_version`].
    /// 3. Emits an [`LspInbound::Change`] event to the background LSP actor.
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

    /// Dispatches an asynchronous `textDocument/completion` request to the LSP actor
    /// corresponding to the current cursor position.
    pub fn request_completions(&mut self) {
        if let Some(tx) = &self.lsp_tx {
            self.lsp_req_id += 1;
            let _ = tx.send(LspInbound::Completion {
                line: self.cursor_y,
                col: self.cursor_x,
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
    ///
    /// Used to compute the text slice to be replaced when accepting an autocomplete candidate.
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
    ///
    /// Returns `None` if the editor is not in [`Mode::Visual`].
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

    /// Enters [`Mode::Visual`] spanning the entirety of the buffer from `(0, 0)` to the final character.
    pub fn select_all(&mut self) {
        if self.rope.len_chars() == 0 {
            return;
        }
        self.mode = Mode::Visual {
            anchor_x: 0,
            anchor_y: 0,
        };
        self.cursor_y = self.rope.len_lines().saturating_sub(1);
        self.cursor_x = self.current_line_len();
        self.status_msg = "Selected all".to_string();
    }

    /// Copies selected text (or the entire current line if no selection is active) into [`Self::clipboard`].
    ///
    /// Reverts mode to [`Mode::Normal`].
    pub fn yank_selection(&mut self) {
        if let Some((start, end)) = self.selection_range() {
            if start < end {
                let slice = self.rope.slice(start..end);
                self.clipboard = slice.to_string();
                self.status_msg = format!("Yanked {} chars", self.clipboard.len());
            }
        } else {
            let line = self.rope.line(self.cursor_y);
            self.clipboard = line.to_string();
            self.status_msg = "Yanked line".to_string();
        }
        self.mode = Mode::Normal;
    }

    /// Deletes characters encompassed by the active visual selection, storing them in [`Self::clipboard`].
    ///
    /// Records an undo snapshot, repositions the cursor to the deletion origin, and returns to [`Mode::Normal`].
    pub fn delete_selection(&mut self) {
        if let Some((start, end)) = self.selection_range() {
            if start < end {
                self.snapshot();
                let slice = self.rope.slice(start..end);
                self.clipboard = slice.to_string();
                self.rope.remove(start..end);
                self.modified = true;
                self.status_msg = format!("Deleted {} chars", self.clipboard.len());

                let new_cursor_line = self.rope.char_to_line(start);
                let line_start = self.rope.line_to_char(new_cursor_line);
                self.cursor_y = new_cursor_line;
                self.cursor_x = start.saturating_sub(line_start);
                self.mode = Mode::Normal;
                self.clamp_cursor();
                self.on_buffer_modified();
            }
        }
    }

    /// Inserts the contents of [`Self::clipboard`] into the buffer at the current cursor index.
    pub fn paste(&mut self) {
        if self.clipboard.is_empty() {
            self.status_msg = "Clipboard is empty".to_string();
            return;
        }

        self.snapshot();
        let idx = self.char_index();
        self.rope.insert(idx, &self.clipboard);
        self.cursor_x += self.clipboard.chars().count();
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
                self.mode = Mode::Normal;
                self.on_buffer_modified();
            }
        } else {
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

    /// Merges the line below the current cursor line onto the current line.
    ///
    /// Replaces the newline delimiter and any leading indentation on the next line
    /// with a single whitespace character.
    pub fn join_lines(&mut self) {
        if self.cursor_y + 1 >= self.rope.len_lines() {
            return;
        }

        self.snapshot();
        let line_end = self.rope.line_to_char(self.cursor_y + 1) - 1;
        self.rope.remove(line_end..=line_end);

        let next_line_start = line_end;
        let mut ws_len = 0;
        while next_line_start + ws_len < self.rope.len_chars()
            && self.rope.char(next_line_start + ws_len).is_whitespace()
        {
            ws_len += 1;
        }

        if ws_len > 0 {
            self.rope.remove(next_line_start..next_line_start + ws_len);
        }
        self.rope.insert_char(next_line_start, ' ');

        self.modified = true;
        self.on_buffer_modified();
        self.status_msg = "Joined lines".to_string();
    }

    /// Creates an empty line beneath the current line and switches into [`Mode::Insert`].
    pub fn insert_line_below(&mut self) {
        self.snapshot();
        self.cursor_x = self.current_line_len();
        self.insert_newline();
        self.mode = Mode::Insert;
    }

    /// Creates an empty line above the current line and switches into [`Mode::Insert`].
    pub fn insert_line_above(&mut self) {
        self.snapshot();
        self.cursor_x = 0;
        let idx = self.rope.line_to_char(self.cursor_y);
        self.rope.insert_char(idx, '\n');
        self.modified = true;
        self.mode = Mode::Insert;
        self.on_buffer_modified();
    }

    /// Inserts a single unicode character at the current cursor offset and advances the cursor.
    pub fn insert_char(&mut self, c: char) {
        let idx = self.char_index();
        self.rope.insert_char(idx, c);
        self.cursor_x += 1;
        self.modified = true;
        self.on_buffer_modified();
    }

    /// Inserts an opening and closing delimiter pair, placing the cursor between them.
    pub fn insert_pair(&mut self, open: char, close: char) {
        let idx = self.char_index();
        self.rope.insert_char(idx, open);
        self.rope.insert_char(idx + 1, close);
        self.cursor_x += 1;
        self.modified = true;
        self.on_buffer_modified();
    }

    /// Inserts a newline character with automatic indentation preservation.
    ///
    /// Detects leading whitespace on the active line and replicates it onto the new line.
    /// If the cursor is positioned directly between `{` and `}`, it expands the block
    /// across three lines with an additional four spaces of nested indentation.
    pub fn insert_newline(&mut self) {
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
            let to_insert = format!("\n{inner_indent}\n{indent}");
            self.rope.insert(idx, &to_insert);
            self.cursor_y += 1;
            self.cursor_x = inner_indent.chars().count();
        } else {
            let to_insert = format!("\n{indent}");
            self.rope.insert(idx, &to_insert);
            self.cursor_y += 1;
            self.cursor_x = indent.chars().count();
        }

        self.modified = true;
        self.completion_visible = false;
        self.on_buffer_modified();
    }

    /// Deletes the character before the cursor or handles multi-character boundary conditions.
    ///
    /// - If the cursor is between an auto-closed pair (e.g., `()` or `{}`), both are removed.
    /// - If at the start of a line (`cursor_x == 0`), joins the current line with the previous line.
    pub fn backspace(&mut self) {
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

            if current_line_idx > 0 {
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
    ///
    /// Replaces the current typed identifier prefix with the candidate's `insert_text`,
    /// repositions the cursor at the end of the insertion, and dismisses the popup.
    pub fn accept_completion(&mut self) {
        if !self.completion_visible || self.completions.is_empty() {
            return;
        }

        let item = &self.completions[self.completion_idx];
        let replacement = item.insert_text.clone();
        let prefix = self.current_word_prefix();
        let prefix_len = prefix.chars().count();

        let idx = self.char_index();
        let start = idx.saturating_sub(prefix_len);

        self.rope.remove(start..idx);
        self.rope.insert(start, &replacement);

        self.cursor_x = self.cursor_x - prefix_len + replacement.chars().count();
        self.modified = true;
        self.completion_visible = false;
        self.on_buffer_modified();
    }

    /// Deletes a single character situated directly under the cursor without modifying the clipboard.
    pub fn delete_under_cursor(&mut self) {
        let line_len = self.current_line_len();
        if self.cursor_x < line_len {
            self.snapshot();
            let idx = self.char_index();
            self.rope.remove(idx..=idx);
            self.modified = true;
            self.on_buffer_modified();
        }
    }

    /// Deletes the entire active line including its trailing newline delimiter.
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

    /// Flushes the rope buffer out to the underlying file path using buffered I/O.
    ///
    /// Clears `modified`, updates `status_msg`, and notifies the LSP actor via [`LspInbound::Save`].
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
                self.mode = Mode::Visual {
                    anchor_x: self.cursor_x,
                    anchor_y: self.cursor_y,
                };
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
        }
    }

    /// Evaluates and executes the ex-command string buffered in `command_buffer`.
    ///
    /// Supported commands:
    /// - `:q` - Quit (fails if unsaved changes exist)
    /// - `:q!` - Force quit discarding changes
    /// - `:w` - Save buffer
    /// - `:wq` - Save buffer and quit
    /// - `:wrap` - Toggle line wrapping
    /// - `:e`, `:explore` - Toggle sidebar file explorer
    /// - `:p`, `:menu`, `:commands`, `:pal` - Open interactive command palette
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
        } else if cmd == "wrap" {
            self.line_wrap = !self.line_wrap;
            self.status_msg = format!("Line Wrap: {}", if self.line_wrap { "ON" } else { "OFF" });
        } else if cmd == "e" || cmd == "explore" {
            self.explorer.visible = !self.explorer.visible;
            if self.explorer.visible {
                self.explorer.refresh();
                self.focus = Focus::Explorer;
            } else {
                self.focus = Focus::Editor;
            }
        } else if cmd == "p" || cmd == "menu" || cmd == "commands" || cmd == "pal" {
            self.palette.visible = true;
            self.palette.query.clear();
            self.palette.selected_idx = 0;
            self.palette.scroll = 0;
        } else if !cmd.is_empty() {
            self.status_msg = format!("Unknown command: :{cmd}");
        }
    }

    /// Enforces modal cursor constraints against buffer boundaries.
    ///
    /// - Restricts `cursor_y` to `[0, len_lines - 1]`.
    /// - In [`Mode::Insert`], restricts `cursor_x` to `[0, line_len]`.
    /// - In non-insert modes, restricts `cursor_x` to `[0, line_len - 1]`.
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
        if self.cursor_y < self.scroll_y {
            self.scroll_y = self.cursor_y;
        } else if self.cursor_y >= self.scroll_y + height {
            self.scroll_y = self.cursor_y - height + 1;
        }

        if self.line_wrap {
            self.scroll_x = 0;
        } else if self.cursor_x < self.scroll_x {
            self.scroll_x = self.cursor_x;
        } else if self.cursor_x >= self.scroll_x + width {
            self.scroll_x = self.cursor_x - width + 1;
        }
    }
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
