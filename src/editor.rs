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

#[derive(PartialEq, Eq, Clone, Copy)]
pub enum Mode {
    Normal,
    Insert,
    Command,
    Visual { anchor_x: usize, anchor_y: usize },
}

#[derive(PartialEq, Eq, Clone, Copy)]
pub enum Focus {
    Editor,
    Explorer,
}

// -----------------------------------------------------------------------------
// Command Palette Definitions
// -----------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum CommandId {
    SelectAll,
    ToggleExplorer,
    ToggleWrap,
    Save,
    SaveQuit,
    QuitForce,
    EnterVisual,
    InsertBelow,
    InsertAbove,
    JoinLines,
    Undo,
    Yank,
    Paste,
    ToggleCase,
    JumpTop,
    JumpBottom,
    TriggerCompletion,
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

// -----------------------------------------------------------------------------
// File Explorer
// -----------------------------------------------------------------------------

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
        } else {
            self.entries[idx].expanded = true;
            let current_depth = self.entries[idx].depth;
            let dir_path = self.entries[idx].path.clone();
            let children = Self::read_directory(&dir_path, current_depth + 1);

            let mut insert_pos = idx + 1;
            for child in children {
                self.entries.insert(insert_pos, child);
                insert_pos += 1;
            }
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

// -----------------------------------------------------------------------------
// Editor Buffer Model
// -----------------------------------------------------------------------------

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
    pub status_msg: String,
    pub command_buffer: String,
    pub pending_key: Option<char>,
    pub undo_stack: Vec<Rope>,
    pub syntax: SyntaxEngine,
    pub diagnostics: Vec<DiagnosticItem>,
    pub lsp_status: LspStatus,
    pub lsp_tx: Option<mpsc::UnboundedSender<LspInbound>>,
    pub lsp_req_id: i64,
    pub doc_version: i64,

    // Completions State
    pub completions: Vec<SuggestionItem>,
    pub completion_idx: usize,
    pub completion_scroll: usize,
    pub completion_visible: bool,

    // File Tree & Command Palette
    pub explorer: FileExplorer,
    pub palette: CommandPalette,
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
        self.mode = Mode::Visual {
            anchor_x: 0,
            anchor_y: 0,
        };
        self.cursor_y = self.rope.len_lines().saturating_sub(1);
        self.cursor_x = self.current_line_len();
        self.status_msg = "Selected all".to_string();
    }

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
                self.rope.remove(idx..idx + 1);
                self.rope.insert_char(idx, toggled);
                self.cursor_x += 1;
                self.modified = true;
                self.clamp_cursor();
                self.on_buffer_modified();
            }
        }
    }

    pub fn join_lines(&mut self) {
        if self.cursor_y + 1 >= self.rope.len_lines() {
            return;
        }

        self.snapshot();
        let line_end = self.rope.line_to_char(self.cursor_y + 1) - 1;
        self.rope.remove(line_end..line_end + 1);

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

    pub fn insert_line_below(&mut self) {
        self.snapshot();
        self.cursor_x = self.current_line_len();
        self.insert_newline();
        self.mode = Mode::Insert;
    }

    pub fn insert_line_above(&mut self) {
        self.snapshot();
        self.cursor_x = 0;
        let idx = self.rope.line_to_char(self.cursor_y);
        self.rope.insert_char(idx, '\n');
        self.modified = true;
        self.mode = Mode::Insert;
        self.on_buffer_modified();
    }

    pub fn insert_char(&mut self, c: char) {
        let idx = self.char_index();
        self.rope.insert_char(idx, c);
        self.cursor_x += 1;
        self.modified = true;
        self.on_buffer_modified();
    }

    pub fn insert_pair(&mut self, open: char, close: char) {
        let idx = self.char_index();
        self.rope.insert_char(idx, open);
        self.rope.insert_char(idx + 1, close);
        self.cursor_x += 1;
        self.modified = true;
        self.on_buffer_modified();
    }

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
            let inner_indent = format!("{}    ", indent);
            let to_insert = format!("\n{}\n{}", inner_indent, indent);
            self.rope.insert(idx, &to_insert);
            self.cursor_y += 1;
            self.cursor_x = inner_indent.chars().count();
        } else {
            let to_insert = format!("\n{}", indent);
            self.rope.insert(idx, &to_insert);
            self.cursor_y += 1;
            self.cursor_x = indent.chars().count();
        }

        self.modified = true;
        self.completion_visible = false;
        self.on_buffer_modified();
    }

    pub fn backspace(&mut self) {
        if self.cursor_x > 0 {
            let idx = self.char_index();
            let char_before = self.rope.char(idx - 1);
            let char_after = if idx < self.rope.len_chars() {
                Some(self.rope.char(idx))
            } else {
                None
            };

            let is_pair = match (char_before, char_after) {
                ('(', Some(')')) => true,
                ('[', Some(']')) => true,
                ('{', Some('}')) => true,
                ('"', Some('"')) => true,
                ('\'', Some('\'')) => true,
                _ => false,
            };

            if is_pair {
                self.rope.remove(idx - 1..idx + 1);
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
            Mode::Normal | Mode::Command | Mode::Visual { .. } => line_len.saturating_sub(1),
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

        if !self.line_wrap {
            if self.cursor_x < self.scroll_x {
                self.scroll_x = self.cursor_x;
            } else if self.cursor_x >= self.scroll_x + width {
                self.scroll_x = self.cursor_x - width + 1;
            }
        } else {
            self.scroll_x = 0;
        }
    }
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
