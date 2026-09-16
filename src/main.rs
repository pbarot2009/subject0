//! # Application Entry Point, Event Loop, & Terminal UI Subsystem
//!
//! This module serves as the runtime orchestrator for `subject0`. It integrates the
//! terminal lifecycle, asynchronous event multiplexing, input decoding, and the
//! frame rendering pipeline.
//!
//! ## Key Subsystems
//!
//! 1. **Terminal Control & Panic Hygiene**:
//!    - Initializes Crossterm raw mode, alternate screen buffers, and mouse event capture.
//!    - Installs a custom panic hook via [`setup_panic_hook`] to guarantee the terminal
//!      restores standard screen buffers, cursor styles, and raw mode flags if a panic occurs,
//!      preventing host terminal corruption.
//!    - Controls hardware cursor rendering modes dynamically via DEC Private Mode Set/Reset
//!      cursor shape escape sequences ([`set_terminal_cursor_style`]).
//!
//! 2. **Asynchronous Actor Multiplexing**:
//!    - Spawns background language server tasks ([`run_lsp_actor`]) using Tokio channels.
//!    - Intercepts and merges incoming LSP outbound traffic (diagnostics, completion responses,
//!      status updates) into the editor buffer model on each event loop tick.
//!    - Implements an autocompletion ranker that filters candidates based on exact match,
//!      case-insensitive prefix match, and label length.
//!
//! 3. **Input Handling & Spatial Coordinate Translation**:
//!    - **Keyboard Controller ([`handle_key_event`])**: Decodes raw key events into modal
//!      state transitions (`Normal`, `Insert`, `Command`, `Visual`), manages multi-key
//!      chords (e.g., `gg`, `dd`), and drives dialog inputs.
//!    - **Pointer / Touch Controller ([`handle_mouse_event`])**: Maps mouse clicks and scroll
//!      wheel events into exact 2D buffer coordinates. Accurately handles soft line-wrapping
//!      by decomposing wrapped lines into sub-rows to calculate the clicked column and line index.
//!
//! 4. **Immediate-Mode UI Rendering Pipeline ([`render_ui`])**:
//!    - Constructs an immediate-mode layout hierarchy through Ratatui widgets.
//!    - Renders the file tree sidebar with collapsible directory icons.
//!    - Renders the main document viewport with dynamic gutter widths, diagnostic severity
//!      markers, syntax-highlighted code spans, and visual selections.
//!    - Renders a Powerline-styled status bar and command/diagnostic notification panel.
//!    - Computes absolute screen coordinates for floating popup overlays (autocomplete dropdown
//!      and command palette).

mod cmd;
mod editor;
mod lsp;

use std::{
    cmp::Ordering,
    io::{Write, stdout},
    path::Path,
    time::Duration,
};

const SPINNER: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

use anyhow::Result;
use crossterm::{
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind,
        KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
    },
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Position, Rect, Size},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Paragraph},
};
use tokio::sync::mpsc;

use editor::{Editor, Focus, Mode, line_len};
use lsp::{
    LspInbound, LspOutbound, LspStatus, SuggestionItem, completion_kind_icon, file_icon_and_color,
    run_lsp_actor,
};

/// Configures the terminal hardware cursor geometry based on the active modal editing state.
///
/// Sends standard DECSCUSR (DEC Set Cursor Style) ANSI control sequences:
/// - `\x1b[2 q`: Steady Block cursor (used in [`Mode::Normal`], [`Mode::Command`], and [`Mode::Visual`]).
/// - `\x1b[6 q`: Steady Bar / I-Beam cursor (used in [`Mode::Insert`]).
fn set_terminal_cursor_style(mode: Mode) {
    let mut stdout = stdout();
    match mode {
        Mode::Normal | Mode::Command | Mode::Visual { .. } => {
            let _ = stdout.write_all(b"\x1b[2 q"); // Block
        }
        Mode::Insert => {
            let _ = stdout.write_all(b"\x1b[6 q"); // Thin Bar
        }
    }
    let _ = stdout.flush();
}

/// Registers a panic hook to clean up the terminal before the process aborts.
///
/// In raw mode, terminal standard I/O handles do not process line feeds or echo characters
/// conventionally. If an unhandled panic occurs while raw mode or alternate screen buffers
/// are active, the user's shell becomes unresponsive. This hook resets:
/// 1. Hardware cursor style to default (`\x1b[0 q`).
/// 2. Raw mode (restoring canonical line input and echoing).
/// 3. Alternate screen buffer (reverting to primary shell buffer).
/// 4. Mouse reporting protocols.
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

/// Spawns an LSP background actor using the unified outbound channel.
fn start_lsp_for_file(editor: &mut Editor, path: &Path, lang_id: &str, cmd: &str) {
    if let Some(out_tx) = &editor.lsp_out_tx {
        let (in_tx, in_rx) = mpsc::unbounded_channel::<LspInbound>();
        editor.lsp_tx = Some(in_tx);
        let p = path.to_path_buf();
        let initial_text = editor.rope.to_string();
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

// === Application Lifecycle & Event Loop ===

/// Application entry point initializing terminal subsystems and running the event loop.
///
/// # Control Flow
/// 1. Configures terminal panic safety hooks and enables raw mode + alternate screen buffer.
/// 2. Parses command-line arguments to load an initial file path into [`Editor`].
/// 3. Spawns the LSP actor task if the target file extension has an associated server.
/// 4. Enters the main event loop:
///    - Drains non-blocking messages from the LSP outbound channel.
///    - Filters, sorts, and limits completion suggestions.
///    - Draws the current frame using [`render_ui`].
///    - Polls crossterm for key and mouse input events with a 20ms timeout.
/// 5. Restores original terminal state on exit.
#[tokio::main]
async fn main() -> Result<()> {
    // 1. Parse command-line flags before initializing terminal screen
    let cli_args = cmd::CliArgs::parse();

    setup_panic_hook();
    enable_raw_mode()?;
    let mut stdout = stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let target_path = cli_args.path.clone();
    let mut editor = Editor::new(target_path.as_ref())?;

    // Apply CLI flag overrides
    if cli_args.ignore_config {
        // Reset in-memory preferences, but keep the already-resolved
        // `source_path` so that if the user later saves config mid-session
        // (e.g. via the SaveConfig palette command), it still writes to the
        // correct project-anchored `.subject0` location rather than losing
        // track of where it should go.
        editor.config = editor::AppConfig {
            preferred_lsps: std::collections::HashMap::new(),
            line_wrap: true,
            source_path: editor.config.source_path.clone(),
        };
    }
    if let Some(wrap) = cli_args.line_wrap {
        editor.line_wrap = wrap;
    }
    if let Some(line) = cli_args.jump_line {
        editor.cursor_y = line.saturating_sub(1);
        editor.clamp_cursor();
    }

    let (lsp_out_tx, mut lsp_out_rx) = mpsc::unbounded_channel::<LspOutbound>();
    editor.lsp_out_tx = Some(lsp_out_tx.clone());

    // Dynamic Multi-LSP Resolution (only spawn for files, not directories)
    if let Some(path) = &target_path
        && path.is_file()
    {
        let lang = editor.syntax.language;
        let lang_id = lang.lsp_id();
        let installed = lang.installed_servers();

        if !installed.is_empty() {
            let preferred = editor.config.preferred_lsps.get(lang_id).cloned();
            let chosen_server = if let Some(pref) = preferred.filter(|p| installed.contains(p)) {
                Some(pref)
            } else if installed.len() == 1 {
                Some(installed[0].clone())
            } else {
                // Multiple servers installed and none configured: prompt user
                editor.lsp_picker = Some(editor::LspPicker {
                    language_id: lang_id.to_string(),
                    candidates: installed.clone(),
                    selected_idx: 0,
                });
                None
            };

            if let Some(cmd) = chosen_server {
                start_lsp_for_file(&mut editor, path, lang_id, &cmd);
            }
        }
    }

    set_terminal_cursor_style(editor.mode);

    while !editor.should_quit {
        // Drain incoming messages emitted by the background LSP actor.
        while let Ok(msg) = lsp_out_rx.try_recv() {
            match msg {
                LspOutbound::Status(s) => {
                    let was_ready = matches!(s, LspStatus::Ready(_));
                    editor.lsp_status = s;
                    if was_ready {
                        editor.request_semantic_tokens();
                    }
                }
                LspOutbound::SemanticTokens { tokens } => {
                    editor.syntax.set_semantic_tokens(tokens);
                }
                LspOutbound::Diagnostics(d) => editor.diagnostics = d,
                LspOutbound::Completions { req_id, items } => {
                    // Only process completions corresponding to the latest request sequence.
                    if req_id == editor.lsp_req_id && !items.is_empty() {
                        let prefix = editor.current_word_prefix();
                        let prefix_lower = prefix.to_lowercase();

                        // 1. Filter candidates containing the current prefix substring.
                        let mut filtered: Vec<SuggestionItem> = items
                            .into_iter()
                            .filter(|it| {
                                prefix_lower.is_empty()
                                    || it.label.to_lowercase().contains(&prefix_lower)
                            })
                            .collect();

                        // 2. Rank candidates: Exact match > Prefix match > Shortest length.
                        filtered.sort_by(|a, b| {
                            let a_lbl = &a.label;
                            let b_lbl = &b.label;
                            let a_exact = a_lbl == &prefix;
                            let b_exact = b_lbl == &prefix;
                            if a_exact && !b_exact {
                                return Ordering::Less;
                            }
                            if !a_exact && b_exact {
                                return Ordering::Greater;
                            }

                            let a_starts = a_lbl.to_lowercase().starts_with(&prefix_lower);
                            let b_starts = b_lbl.to_lowercase().starts_with(&prefix_lower);
                            if a_starts && !b_starts {
                                return Ordering::Less;
                            }
                            if !a_starts && b_starts {
                                return Ordering::Greater;
                            }

                            a_lbl.len().cmp(&b_lbl.len())
                        });

                        // 3. Limit to the top 100 entries to prevent frame-rendering latency.
                        filtered.truncate(100);

                        if filtered.is_empty() {
                            editor.completion_visible = false;
                        } else {
                            editor.completions = filtered;
                            editor.completion_idx = 0;
                            editor.completion_scroll = 0;
                            editor.completion_visible = true;
                        }
                    }
                }
            }
        }

        editor.spinner_tick = editor.spinner_tick.wrapping_add(1);

        // Draw current state onto terminal frame.
        terminal.draw(|f| render_ui(f, &mut editor))?;

        // Poll for interactive user input events with a 20ms timeout.
        if event::poll(Duration::from_millis(20))? {
            match event::read()? {
                Event::Key(key) => handle_key_event(&mut editor, key),
                Event::Mouse(mouse) => handle_mouse_event(&mut editor, mouse, terminal.size()?),
                _ => {}
            }
        }
    }

    // Reset terminal configuration upon exit.
    let mut out = std::io::stdout();
    let _ = out.write_all(b"\x1b[0 q");
    let _ = disable_raw_mode();
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    Ok(())
}

// === Touch & Mouse Handling ===

/// Translates raw mouse and touchscreen interactions into editor actions.
///
/// Handles interaction zones in order of spatial precedence:
/// 1. **Command Palette**: Intercepts clicks on palette items or backdrop clicks to dismiss.
/// 2. **Statusline Taps**: Toggles mode, file explorer, line wrap, or opens the palette.
/// 3. **Sidebar File Explorer**: Selects entries, expands directories, or opens files.
/// 4. **Completion Dropdown**: Handles scroll-wheel selection and item clicks.
/// 5. **Document Viewport**: Maps clicks and drags to `(cursor_x, cursor_y)`, properly
///    handling line wrap sub-row calculation, gutter offsets, and viewport bounds.
fn handle_mouse_event(editor: &mut Editor, mouse: MouseEvent, size: Size) {
    let status_row = size.height.saturating_sub(2);
    let cmd_row = size.height.saturating_sub(1);
    let viewport_top = 1u16;
    let viewport_bottom = size.height.saturating_sub(3);

    // 1. Intercept Command Palette Key Events
    if editor.palette.visible {
        if let MouseEventKind::Down(MouseButton::Left) = mouse.kind {
            let width = 46u16.min(size.width.saturating_sub(2));
            let height = 12u16.min(size.height.saturating_sub(2));
            let x = (size.width.saturating_sub(width)) / 2;
            let y = 1u16;

            // Check if click occurred within the list region of the palette window.
            if mouse.column >= x
                && mouse.column < x + width
                && mouse.row >= y + 2
                && mouse.row < y + height - 1
            {
                let clicked_row = (mouse.row - (y + 2)) as usize;
                let cmds = editor.palette.filtered_commands();
                let actual_idx = editor.palette.scroll + clicked_row;
                if actual_idx < cmds.len() {
                    let cmd_id = cmds[actual_idx].id;
                    editor.execute_palette_command(cmd_id);
                }
                return;
            } else if mouse.column < x
                || mouse.column >= x + width
                || mouse.row < y
                || mouse.row >= y + height
            {
                // Click occurred outside modal boundary; dismiss palette.
                editor.palette.visible = false;
                return;
            }
        }
        return;
    }

    // 2. Statusline Taps
    if mouse.row == status_row {
        if let MouseEventKind::Down(MouseButton::Left) = mouse.kind {
            if mouse.column <= 9 {
                // Mode Badge Tap: Toggle between Normal and Insert modes.
                editor.mode = match editor.mode {
                    Mode::Normal => Mode::Insert,
                    Mode::Insert | Mode::Command | Mode::Visual { .. } => Mode::Normal,
                };
                set_terminal_cursor_style(editor.mode);
                editor.completion_visible = false;
            } else if mouse.column <= 18 {
                // Sidebar Toggle Button Tap
                editor.explorer.visible = !editor.explorer.visible;
                if editor.explorer.visible {
                    editor.explorer.refresh();
                    editor.focus = Focus::Explorer;
                } else {
                    editor.focus = Focus::Editor;
                }
            } else if mouse.column <= 27 {
                // Line Wrap Toggle Button Tap
                editor.line_wrap = !editor.line_wrap;
                editor.status_msg =
                    format!("Line Wrap: {}", if editor.line_wrap { "ON" } else { "OFF" });
            } else if mouse.column <= 36 {
                // Command Palette Shortcut Tap
                editor.palette.visible = true;
                editor.palette.query.clear();
                editor.palette.selected_idx = 0;
                editor.palette.scroll = 0;
            }
        }
        return;
    }

    if mouse.row == cmd_row {
        return;
    }

    // Determine current sidebar width allocation.
    let explorer_width = if editor.explorer.visible {
        if size.width < 70 {
            (size.width * 7 / 10).max(26).min(size.width)
        } else {
            26u16
        }
    } else {
        0u16
    };

    // 3. File Explorer Sidebar Interactions
    if editor.explorer.visible && mouse.column < explorer_width {
        let max_visible = viewport_bottom.saturating_sub(viewport_top) as usize;
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if mouse.row >= viewport_top && mouse.row < viewport_bottom {
                    let clicked_idx = editor.explorer.scroll + (mouse.row - viewport_top) as usize;
                    if clicked_idx < editor.explorer.entries.len() {
                        editor.explorer.selected_idx = clicked_idx;
                        if editor.explorer.entries[clicked_idx].is_dir {
                            editor.explorer.toggle_expand(clicked_idx);
                        } else {
                            let path = editor.explorer.entries[clicked_idx].path.clone();
                            let _ = editor.open_file(path);
                            editor.focus = Focus::Editor;
                            if size.width < 70 {
                                editor.explorer.visible = false;
                            }
                        }
                    }
                }
            }
            MouseEventKind::ScrollDown => {
                if editor.explorer.selected_idx + 1 < editor.explorer.entries.len() {
                    editor.explorer.selected_idx += 1;
                    editor.explorer.update_scroll(max_visible);
                }
            }
            MouseEventKind::ScrollUp if editor.explorer.selected_idx > 0 => {
                editor.explorer.selected_idx -= 1;
                editor.explorer.update_scroll(max_visible);
            }
            _ => {}
        }
        return;
    }

    // 4. Completion Dropdown Interactions
    if editor.completion_visible && !editor.completions.is_empty() {
        let max_visible = 6usize;
        match mouse.kind {
            MouseEventKind::ScrollDown => {
                if editor.completion_idx + 1 < editor.completions.len() {
                    editor.completion_idx += 1;
                    editor.update_completion_scroll(max_visible);
                    return;
                }
            }
            MouseEventKind::ScrollUp => {
                if editor.completion_idx > 0 {
                    editor.completion_idx -= 1;
                    editor.update_completion_scroll(max_visible);
                    return;
                }
            }
            MouseEventKind::Down(MouseButton::Left) => {
                editor.accept_completion();
                return;
            }
            _ => {}
        }
    }

    // 5. Document Viewport Buffer Interactions (Reliable line & sub-row mapping)
    let gutter_digits = editor.rope.len_lines().max(1).to_string().len().max(2);
    let gutter_width = gutter_digits + 4;
    let content_left = explorer_width + 1u16 + gutter_width as u16;
    let text_area_width = (size.width as usize)
        .saturating_sub(content_left as usize)
        .max(1);

    match mouse.kind {
        MouseEventKind::Down(MouseButton::Left) | MouseEventKind::Drag(MouseButton::Left) => {
            if mouse.row >= viewport_top && mouse.row < viewport_bottom {
                let clicked_screen_row = (mouse.row - viewport_top) as usize;

                if editor.line_wrap {
                    // When wrapping is enabled, iterate lines and calculate visual sub-rows.
                    let mut accumulated_rows = 0;
                    let mut found_line = editor.rope.len_lines().saturating_sub(1);
                    let mut found_col = 0;

                    for y in editor.scroll_y..editor.rope.len_lines() {
                        let l_len = line_len(&editor.rope, y);
                        let sub_rows = if l_len == 0 {
                            1
                        } else {
                            l_len.div_ceil(text_area_width)
                        };

                        if clicked_screen_row < accumulated_rows + sub_rows {
                            found_line = y;
                            let sub_idx = clicked_screen_row - accumulated_rows;
                            let sub_col = if mouse.column >= content_left {
                                (mouse.column - content_left) as usize
                            } else {
                                0
                            };
                            found_col = (sub_idx * text_area_width + sub_col).min(l_len);
                            break;
                        }
                        accumulated_rows += sub_rows;
                    }

                    editor.cursor_y = found_line;
                    editor.cursor_x = found_col;
                } else {
                    // When wrapping is disabled, 1 terminal row == 1 buffer line.
                    let target_line = (editor.scroll_y + clicked_screen_row)
                        .min(editor.rope.len_lines().saturating_sub(1));
                    editor.cursor_y = target_line;
                    if mouse.column >= content_left {
                        editor.cursor_x = editor.scroll_x + (mouse.column - content_left) as usize;
                    } else {
                        editor.cursor_x = 0;
                    }
                }

                editor.clamp_cursor();
                editor.completion_visible = false;
                editor.focus = Focus::Editor;

                if size.width < 70 && editor.explorer.visible {
                    editor.explorer.visible = false;
                }
            }
        }
        MouseEventKind::ScrollUp => {
            editor.scroll_y = editor.scroll_y.saturating_sub(3);
            editor.cursor_y = editor.cursor_y.saturating_sub(3);
            editor.clamp_cursor();
        }
        MouseEventKind::ScrollDown if editor.scroll_y + 3 < editor.rope.len_lines() => {
            editor.scroll_y += 3;
            editor.cursor_y = (editor.cursor_y + 3).min(editor.rope.len_lines().saturating_sub(1));
            editor.clamp_cursor();
        }
        _ => {}
    }
}

// === Keyboard Controller ===

/// Dispatches raw key events according to the active mode and focus context.
///
/// Filters out key release events to prevent duplicate executions on platforms
/// with the kitty keyboard protocol or enhanced event reporting.
///
/// # Routing Priority
/// 1. Command Palette Navigation (when visible).
/// 2. Global Shortcuts (`Ctrl-E` for Explorer toggle).
/// 3. File Explorer Sidebar Navigation (when focused).
/// 4. Modal Editing Handler:
///    - [`Mode::Normal`]: Motion commands, multi-key prefixes (`gg`, `dd`), mode transitions.
///    - [`Mode::Visual`]: Selection manipulation, deletion, yanking.
///    - [`Mode::Insert`]: Text typing, autocomplete popup control, auto-pairing brackets.
///    - [`Mode::Command`]: Ex-command input buffering and execution on Enter.
fn handle_key_event(editor: &mut Editor, key: KeyEvent) {
    // Filter release events to prevent double-firing in terminals reporting key releases.
    if key.kind == KeyEventKind::Release {
        return;
    }

    // Intercept In-Editor Help Modal Navigation
    if editor.show_help {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q' | '?') => {
                editor.show_help = false;
            }
            KeyCode::Down | KeyCode::Char('j') => {
                editor.help_scroll = editor.help_scroll.saturating_add(1);
            }
            KeyCode::Up | KeyCode::Char('k') => {
                editor.help_scroll = editor.help_scroll.saturating_sub(1);
            }
            KeyCode::PageDown => {
                editor.help_scroll = editor.help_scroll.saturating_add(8);
            }
            KeyCode::PageUp => {
                editor.help_scroll = editor.help_scroll.saturating_sub(8);
            }
            _ => {}
        }
        return;
    }

    let prev_mode = editor.mode;
    let max_visible = 6usize;

    // Intercept LSP Server Selection Modal
    if let Some(mut picker) = editor.lsp_picker.take() {
        match key.code {
            KeyCode::Esc => {
                // Dismiss modal
                editor.status_msg = "LSP selection canceled".to_string();
            }
            KeyCode::Down | KeyCode::Char('j') => {
                picker.selected_idx = (picker.selected_idx + 1) % picker.candidates.len();
                editor.lsp_picker = Some(picker);
            }
            KeyCode::Up | KeyCode::Char('k') => {
                picker.selected_idx = if picker.selected_idx == 0 {
                    picker.candidates.len() - 1
                } else {
                    picker.selected_idx - 1
                };
                editor.lsp_picker = Some(picker);
            }
            KeyCode::Enter => {
                let chosen = picker.candidates[picker.selected_idx].clone();
                editor
                    .config
                    .preferred_lsps
                    .insert(picker.language_id.clone(), chosen.clone());
                let _ = editor.config.save();
                editor.status_msg = format!("Selected LSP: {chosen} (saved to .subject0)");

                if let Some(path) = editor.path.clone() {
                    start_lsp_for_file(editor, &path, &picker.language_id, &chosen);
                }
            }

            _ => {
                editor.lsp_picker = Some(picker);
            }
        }
        return;
    }

    // 1. Intercept Command Palette Key Events
    if editor.palette.visible {
        let cmds = editor.palette.filtered_commands();
        match key.code {
            KeyCode::Esc => {
                editor.palette.visible = false;
            }
            KeyCode::Down => {
                if !cmds.is_empty() {
                    editor.palette.selected_idx = (editor.palette.selected_idx + 1) % cmds.len();
                }
            }
            KeyCode::Up => {
                if !cmds.is_empty() {
                    editor.palette.selected_idx = if editor.palette.selected_idx == 0 {
                        cmds.len() - 1
                    } else {
                        editor.palette.selected_idx - 1
                    };
                }
            }
            KeyCode::Enter => {
                if !cmds.is_empty() && editor.palette.selected_idx < cmds.len() {
                    let cmd_id = cmds[editor.palette.selected_idx].id;
                    editor.execute_palette_command(cmd_id);
                }
            }
            KeyCode::Backspace => {
                editor.palette.query.pop();
                editor.palette.selected_idx = 0;
            }
            KeyCode::Char(c) => {
                editor.palette.query.push(c);
                editor.palette.selected_idx = 0;
            }
            _ => {}
        }
        return;
    }

    // Toggle File Explorer: Ctrl-E
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('e') {
        editor.explorer.visible = !editor.explorer.visible;
        if editor.explorer.visible {
            editor.explorer.refresh();
            editor.focus = Focus::Explorer;
        } else {
            editor.focus = Focus::Editor;
        }
        return;
    }

    // Explorer navigation when active
    if editor.focus == Focus::Explorer && editor.explorer.visible {
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => {
                if editor.explorer.selected_idx + 1 < editor.explorer.entries.len() {
                    editor.explorer.selected_idx += 1;
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                if editor.explorer.selected_idx > 0 {
                    editor.explorer.selected_idx -= 1;
                }
            }
            KeyCode::Enter | KeyCode::Char(' ') => {
                let idx = editor.explorer.selected_idx;
                if idx < editor.explorer.entries.len() {
                    if editor.explorer.entries[idx].is_dir {
                        editor.explorer.toggle_expand(idx);
                    } else {
                        let path = editor.explorer.entries[idx].path.clone();
                        let _ = editor.open_file(path);
                        editor.focus = Focus::Editor;
                    }
                }
            }
            KeyCode::Esc | KeyCode::Char('q') => {
                editor.focus = Focus::Editor;
            }
            _ => {}
        }
        return;
    }

    // Editor Buffer Handling
    match editor.mode {
        Mode::Normal => {
            editor.completion_visible = false;
            // Evaluate pending multi-character chords (e.g., 'dd', 'gg').
            if let Some(pending) = editor.pending_key.take() {
                let handled = match (pending, key.code) {
                    ('d', KeyCode::Char('d')) => {
                        editor.delete_current_line();
                        true
                    }
                    ('g', KeyCode::Char('g')) => {
                        editor.cursor_y = 0;
                        editor.cursor_x = 0;
                        true
                    }
                    ('g', KeyCode::Char('h')) => {
                        editor.cursor_x = 0;
                        true
                    }
                    ('g', KeyCode::Char('l')) => {
                        editor.cursor_x = editor.current_line_len().saturating_sub(1);
                        true
                    }
                    ('g', KeyCode::Char('e')) => {
                        editor.cursor_y = editor.rope.len_lines().saturating_sub(1);
                        editor.cursor_x = 0;
                        true
                    }
                    _ => false,
                };
                if handled || key.code == KeyCode::Esc {
                    editor.clamp_cursor();
                    return;
                }
                // Unmatched chords fall through to process the incoming key in Normal mode
            }

            match key.code {
                KeyCode::Char(' ') => {
                    // Open Helix-style Space Menu / Command Palette.
                    editor.palette.visible = true;
                    editor.palette.query.clear();
                    editor.palette.selected_idx = 0;
                    editor.palette.scroll = 0;
                }
                KeyCode::Char('v') => {
                    editor.mode = Mode::Visual {
                        anchor_x: editor.cursor_x,
                        anchor_y: editor.cursor_y,
                    };
                }
                KeyCode::Char('%') => editor.select_all(),
                KeyCode::Char('y') => editor.yank_selection(),
                KeyCode::Char('p') => editor.paste(),
                KeyCode::Char('~') => editor.toggle_case(),
                KeyCode::Char('J') => editor.join_lines(),
                KeyCode::Char('o') => editor.insert_line_below(),
                KeyCode::Char('O') => editor.insert_line_above(),
                KeyCode::Char('i') => {
                    editor.snapshot();
                    editor.mode = Mode::Insert;
                }
                KeyCode::Char('I') => {
                    editor.snapshot();
                    editor.cursor_x = 0;
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
                KeyCode::Char('A') => {
                    editor.snapshot();
                    editor.cursor_x = editor.current_line_len();
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
                KeyCode::Char('0') => editor.cursor_x = 0,
                KeyCode::Char('$') => editor.cursor_x = editor.current_line_len().saturating_sub(1),
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
                KeyCode::Char('?') => {
                    editor.show_help = true;
                    editor.help_scroll = 0;
                }
                _ => {}
            }
        }
        Mode::Visual { .. } => match key.code {
            KeyCode::Esc => {
                editor.mode = Mode::Normal;
            }
            KeyCode::Char('d' | 'x') => {
                editor.delete_selection();
            }
            KeyCode::Char('c') => {
                editor.delete_selection();
                editor.mode = Mode::Insert;
            }
            KeyCode::Char('y') => {
                editor.yank_selection();
            }
            KeyCode::Char('~') => {
                editor.toggle_case();
            }
            KeyCode::Char('%') => {
                editor.select_all();
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
            KeyCode::Char('j') | KeyCode::Down if editor.cursor_y + 1 < editor.rope.len_lines() => {
                editor.cursor_y += 1;
            }
            _ => {}
        },
        Mode::Insert => {
            // Priority handling for active autocomplete menu navigation.
            if editor.completion_visible && !editor.completions.is_empty() {
                match key.code {
                    KeyCode::Down => {
                        editor.completion_idx =
                            (editor.completion_idx + 1) % editor.completions.len();
                        editor.update_completion_scroll(max_visible);
                        return;
                    }
                    KeyCode::Up => {
                        editor.completion_idx = if editor.completion_idx == 0 {
                            editor.completions.len() - 1
                        } else {
                            editor.completion_idx - 1
                        };
                        editor.update_completion_scroll(max_visible);
                        return;
                    }
                    KeyCode::Tab | KeyCode::Enter => {
                        editor.accept_completion();
                        return;
                    }
                    KeyCode::Esc => {
                        editor.completion_visible = false;
                        return;
                    }
                    _ => {}
                }
            }

            match key.code {
                KeyCode::Esc => {
                    editor.mode = Mode::Normal;
                    editor.completion_visible = false;
                    if editor.cursor_x > 0 && editor.cursor_x >= editor.current_line_len() {
                        editor.cursor_x = editor.cursor_x.saturating_sub(1);
                    }
                    editor.clamp_cursor();
                }
                KeyCode::Enter => editor.insert_newline(),
                KeyCode::Backspace => editor.backspace(),
                KeyCode::Tab => {
                    // Soft tabs: 4 spaces without reviving stale completion popups
                    for _ in 0..4 {
                        editor.insert_char(' ');
                    }
                    editor.completion_visible = false;
                }
                KeyCode::Char(' ') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    editor.request_completions();
                }
                // Automatic closing bracket and delimiter pairs without firing autocomplete
                KeyCode::Char('(') => {
                    editor.insert_pair('(', ')');
                    editor.completion_visible = false;
                }
                KeyCode::Char('[') => {
                    editor.insert_pair('[', ']');
                    editor.completion_visible = false;
                }
                KeyCode::Char('{') => {
                    editor.insert_pair('{', '}');
                    editor.completion_visible = false;
                }
                KeyCode::Char('"') => {
                    if editor.char_under_cursor() == Some('"') {
                        editor.cursor_x += 1;
                    } else {
                        editor.insert_pair('"', '"');
                    }
                    editor.completion_visible = false;
                }
                KeyCode::Char('\'') => {
                    if editor.char_under_cursor() == Some('\'') {
                        editor.cursor_x += 1;
                    } else {
                        editor.insert_pair('\'', '\'');
                    }
                    editor.completion_visible = false;
                }
                // Step over closing delimiter if already present
                KeyCode::Char(')') if editor.char_under_cursor() == Some(')') => {
                    editor.cursor_x += 1;
                    editor.completion_visible = false;
                }
                KeyCode::Char(']') if editor.char_under_cursor() == Some(']') => {
                    editor.cursor_x += 1;
                    editor.completion_visible = false;
                }
                KeyCode::Char('}') if editor.char_under_cursor() == Some('}') => {
                    editor.cursor_x += 1;
                    editor.completion_visible = false;
                }
                KeyCode::Left => {
                    editor.cursor_x = editor.cursor_x.saturating_sub(1);
                    editor.completion_visible = false;
                }
                KeyCode::Right => {
                    if editor.cursor_x < editor.current_line_len() {
                        editor.cursor_x += 1;
                    }
                    editor.completion_visible = false;
                }
                KeyCode::Up => {
                    editor.cursor_y = editor.cursor_y.saturating_sub(1);
                    editor.completion_visible = false;
                }
                KeyCode::Down => {
                    if editor.cursor_y + 1 < editor.rope.len_lines() {
                        editor.cursor_y += 1;
                    }
                    editor.completion_visible = false;
                }
                KeyCode::Char(c) => {
                    editor.insert_char(c);
                    // Only request completions on valid identifiers or trigger characters
                    let prefix = editor.current_word_prefix();
                    if c == '.'
                        || c == ':'
                        || (!prefix.is_empty() && (c.is_alphanumeric() || c == '_'))
                    {
                        editor.request_completions();
                    } else {
                        editor.completion_visible = false;
                    }
                }

                _ => {}
            }
        }
        Mode::Command => {
            editor.completion_visible = false;
            match key.code {
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
            }
        }
    }

    if prev_mode != editor.mode {
        set_terminal_cursor_style(editor.mode);
    }

    editor.clamp_cursor();
}

// === UI Render Pipeline ===

/// Renders the complete editor user interface into the Ratatui frame buffer.
///
/// # Rendering Layers
/// 1. **Layout Slicing**: Splits the terminal screen vertically into main workspace,
///    statusline, and bottom notification/command bar.
/// 2. **File Explorer (Sidebar)**: Materializes tree entries with Nerd Font glyphs,
///    indentation lines, and directory expansion indicators.
/// 3. **Document Viewport**:
///    - Renders line number gutters with dynamic width based on total line count.
///    - Highlights syntax using the [`crate::lsp::SyntaxEngine`].
///    - Handles visual selection backgrounds.
///    - Emits line wrapping sub-rows with continuation glyphs (`↳`).
/// 4. **Powerline Status Bar**: Renders mode indicator pill, file and wrap toggles,
///    diagnostic counts (errors/warnings), LSP status, and cursor coordinates.
/// 5. **Bottom Area**: Renders `:command` prompts or active-line LSP diagnostics.
/// 6. **Popup Overlays**:
///    - Floating autocomplete dropdown anchored beside the editing cursor.
///    - Centered command palette modal.
#[allow(clippy::needless_range_loop)]
fn render_ui(frame: &mut Frame, editor: &mut Editor) {
    let size = frame.area();

    // Divide screen vertically: Workspace | Statusline (1) | Bottom Line (1)
    let main_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(3),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .split(size);

    // Split workspace horizontally if file explorer sidebar is visible.
    let (explorer_area, editor_area) = if editor.explorer.visible {
        let is_mobile = size.width < 70;
        let sidebar_width = if is_mobile {
            (size.width * 7 / 10).max(26).min(size.width)
        } else {
            26u16
        };

        let h_chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(sidebar_width), Constraint::Min(10)])
            .split(main_chunks[0]);

        (Some(h_chunks[0]), h_chunks[1])
    } else {
        (None, main_chunks[0])
    };

    // 1. Render File Explorer (Sidebar)
    if let Some(exp_rect) = explorer_area {
        let is_focused = editor.focus == Focus::Explorer;
        let border_color = if is_focused {
            Color::Rgb(80, 140, 255)
        } else {
            Color::Rgb(55, 60, 75)
        };

        let exp_block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(border_color))
            .title(Line::from(vec![Span::styled(
                " 󰉓 Files ",
                Style::default()
                    .fg(Color::Rgb(220, 225, 235))
                    .add_modifier(Modifier::BOLD),
            )]));

        let inner_exp = exp_block.inner(exp_rect);
        frame.render_widget(exp_block, exp_rect);

        editor.explorer.update_scroll(inner_exp.height as usize);

        let mut tree_lines = Vec::new();
        let scroll_start = editor.explorer.scroll;
        let scroll_end =
            (scroll_start + inner_exp.height as usize).min(editor.explorer.entries.len());

        for i in scroll_start..scroll_end {
            let entry = &editor.explorer.entries[i];
            let is_sel = i == editor.explorer.selected_idx;
            let indent = "  ".repeat(entry.depth);

            let (icon, icon_color) = if entry.is_dir {
                if entry.expanded {
                    (" ", Color::Rgb(240, 200, 90))
                } else {
                    (" ", Color::Rgb(220, 180, 70))
                }
            } else {
                file_icon_and_color(Some(&entry.path))
            };

            let item_style = if is_sel {
                Style::default()
                    .bg(Color::Rgb(40, 75, 145))
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::Rgb(200, 205, 220))
            };

            tree_lines.push(Line::from(vec![
                Span::raw(indent),
                Span::styled(icon, Style::default().fg(icon_color)),
                Span::styled(format!(" {}", entry.name), item_style),
            ]));
        }

        frame.render_widget(Paragraph::new(tree_lines), inner_exp);
    }

    // 2. Render Document Editor Viewport
    let (icon, icon_color) = file_icon_and_color(editor.path.as_ref());
    let file_title = editor.path.as_ref().map_or_else(
        || "unnamed".into(),
        |p| {
            p.file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string()
        },
    );

    let window_title = Line::from(vec![
        Span::raw(" "),
        Span::styled(format!("{icon} "), Style::default().fg(icon_color)),
        Span::styled(
            file_title,
            Style::default()
                .fg(Color::Rgb(220, 225, 235))
                .add_modifier(Modifier::BOLD),
        ),
        if editor.modified {
            Span::styled(" ●", Style::default().fg(Color::Rgb(240, 100, 100)))
        } else {
            Span::raw("")
        },
        Span::raw(" "),
    ]);

    let editor_block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(if editor.focus == Focus::Editor {
            Color::Rgb(65, 70, 85)
        } else {
            Color::Rgb(45, 50, 60)
        }))
        .title(window_title);

    let inner_area = editor_block.inner(editor_area);
    frame.render_widget(editor_block, editor_area);

    let total_lines = editor.rope.len_lines().max(1);
    let line_digits = total_lines.to_string().len().max(2);
    let gutter_width = line_digits + 4;
    let text_area_width = (inner_area.width as usize)
        .saturating_sub(gutter_width)
        .max(1);

    editor.update_scroll(text_area_width, inner_area.height as usize);

    // Build visible lines: Syntax-highlighted character arrays.
    let mut visible_lines = Vec::new();
    let mut cursor_screen_pos: Option<(u16, u16)> = None;

    let start_line = editor.scroll_y;
    let end_line = (start_line + inner_area.height as usize).min(editor.rope.len_lines());

    let mut current_row = 0u16;

    for y in start_line..end_line {
        if (current_row as usize) >= inner_area.height as usize {
            break;
        }

        let is_cursor_line = y == editor.cursor_y;

        let line_diag = editor.diagnostics.iter().find(|d| d.line == y);
        let (diag_marker, diag_style) = match line_diag.map(|d| d.severity) {
            Some(1) => ("", Style::default().fg(Color::Rgb(240, 90, 90))),
            Some(2) => ("", Style::default().fg(Color::Rgb(245, 185, 60))),
            Some(_) => ("󰌵", Style::default().fg(Color::Rgb(100, 180, 255))),
            None => (" ", Style::default()),
        };

        let gutter_style = if is_cursor_line {
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD)
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

        // Tokenize through SyntaxEngine preserving Tree-sitter colors.
        let syntax_spans = editor.syntax.highlight_line(&line_str, y);
        let mut char_styles: Vec<(char, Style)> = Vec::with_capacity(line_str.len() * 2);
        for span in syntax_spans {
            let st = span.style;
            for ch in span.content.chars() {
                if ch == '\t' {
                    for _ in 0..4 {
                        char_styles.push((' ', st));
                    }
                } else {
                    char_styles.push((ch, st));
                }
            }
        }

        // Map buffer cursor_x to visual screen column considering expanded tabs
        let visual_cursor_x = {
            let mut vx = 0;
            for (idx, ch) in line_str.chars().enumerate() {
                if idx >= editor.cursor_x {
                    break;
                }
                if ch == '\t' {
                    vx += 4;
                } else {
                    vx += 1;
                }
            }
            vx
        };

        if editor.line_wrap && char_styles.len() > text_area_width {
            let total_chars = char_styles.len();
            let mut chunk_start = 0;
            let mut is_first_sub = true;

            while chunk_start < total_chars && (current_row as usize) < inner_area.height as usize {
                let chunk_end = (chunk_start + text_area_width).min(total_chars);
                let (marker, gutter_str, g_style) = if is_first_sub {
                    (
                        diag_marker,
                        format!("{:>width$} │ ", y + 1, width = line_digits),
                        gutter_style,
                    )
                } else {
                    (
                        " ",
                        format!("{:>width$} ↳ ", "", width = line_digits),
                        Style::default().fg(Color::Rgb(65, 70, 85)),
                    )
                };

                let mut sub_spans = vec![
                    Span::styled(
                        marker,
                        if is_first_sub {
                            diag_style
                        } else {
                            Style::default()
                        },
                    ),
                    Span::styled(gutter_str, g_style),
                ];

                for col_idx in chunk_start..chunk_end {
                    let (ch, mut st) = char_styles[col_idx];
                    if editor.is_char_selected(y, col_idx) {
                        st = st.bg(Color::Rgb(55, 80, 145)).fg(Color::White);
                    }
                    sub_spans.push(Span::styled(ch.to_string(), st));
                }

                let is_last_chunk = chunk_end == total_chars;
                let in_chunk = if is_last_chunk {
                    visual_cursor_x >= chunk_start && visual_cursor_x <= chunk_end
                } else {
                    visual_cursor_x >= chunk_start && visual_cursor_x < chunk_end
                };

                if is_cursor_line && in_chunk && cursor_screen_pos.is_none() {
                    let cx =
                        inner_area.x + gutter_width as u16 + (visual_cursor_x - chunk_start) as u16;
                    let cy = inner_area.y + current_row;
                    cursor_screen_pos = Some((cx, cy));
                }

                visible_lines.push(Line::from(sub_spans));
                current_row += 1;
                chunk_start = chunk_end;
                is_first_sub = false;
            }
        } else {
            let (marker, gutter_str, g_style) = (
                diag_marker,
                format!("{:>width$} │ ", y + 1, width = line_digits),
                gutter_style,
            );
            let mut row_spans = vec![
                Span::styled(marker, diag_style),
                Span::styled(gutter_str, g_style),
            ];

            if char_styles.is_empty() {
                if is_cursor_line && cursor_screen_pos.is_none() {
                    let cx = inner_area.x + gutter_width as u16;
                    let cy = inner_area.y + current_row;
                    cursor_screen_pos = Some((cx, cy));
                }
            } else {
                let skip_count = if editor.line_wrap { 0 } else { editor.scroll_x };
                let take_count = text_area_width;

                for (col_idx, (ch, mut st)) in char_styles
                    .into_iter()
                    .enumerate()
                    .skip(skip_count)
                    .take(take_count)
                {
                    if editor.is_char_selected(y, col_idx) {
                        st = st.bg(Color::Rgb(55, 80, 145)).fg(Color::White);
                    }
                    row_spans.push(Span::styled(ch.to_string(), st));
                }

                if is_cursor_line && cursor_screen_pos.is_none() {
                    let visible_x = visual_cursor_x.saturating_sub(skip_count);
                    if visible_x < text_area_width {
                        let cx = inner_area.x + gutter_width as u16 + visible_x as u16;
                        let cy = inner_area.y + current_row;
                        cursor_screen_pos = Some((cx, cy));
                    }
                }
            }

            visible_lines.push(Line::from(row_spans));
            current_row += 1;
        }
    }

    // Fill remaining viewport space with '~' tildes.
    for _ in (current_row as usize)..inner_area.height as usize {
        visible_lines.push(Line::from(vec![
            Span::raw(" "),
            Span::styled(
                format!("{:>width$} │ ", "~", width = line_digits),
                Style::default().fg(Color::Rgb(50, 55, 68)),
            ),
        ]));
    }

    frame.render_widget(Paragraph::new(visible_lines), inner_area);

    // 3. Render Statusline
    let (badge_text, badge_color) = match editor.mode {
        Mode::Normal => (" NORMAL ", Color::Rgb(80, 140, 255)),
        Mode::Insert => (" INSERT ", Color::Rgb(70, 200, 120)),
        Mode::Command => (" COMMAND ", Color::Rgb(220, 100, 240)),
        Mode::Visual { .. } => (" VISUAL ", Color::Rgb(240, 160, 60)),
    };

    let bar_bg = Color::Rgb(20, 22, 28);
    let pill_bg = Color::Rgb(35, 38, 48);
    let bar_foreground = Color::Rgb(200, 205, 220);

    let error_count = editor
        .diagnostics
        .iter()
        .filter(|d| d.severity == 1)
        .count();
    let warn_count = editor
        .diagnostics
        .iter()
        .filter(|d| d.severity == 2)
        .count();

    let sidebar_toggle_badge = if editor.explorer.visible {
        " 󰉓 Files "
    } else {
        " 󰉒 Files "
    };
    let wrap_badge = if editor.line_wrap {
        " 󰖶 Wrap "
    } else {
        " 󰖵 NoWrap "
    };

    let status_left = Line::from(vec![
        Span::styled(
            badge_text,
            Style::default()
                .bg(badge_color)
                .fg(Color::Rgb(15, 17, 22))
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled("", Style::default().bg(pill_bg).fg(badge_color)),
        Span::styled(
            sidebar_toggle_badge,
            Style::default().bg(pill_bg).fg(if editor.explorer.visible {
                Color::Rgb(100, 180, 255)
            } else {
                bar_foreground
            }),
        ),
        Span::styled(
            wrap_badge,
            Style::default().bg(pill_bg).fg(if editor.line_wrap {
                Color::Rgb(100, 200, 140)
            } else {
                Color::Rgb(140, 145, 160)
            }),
        ),
        Span::styled(
            " 󰍉 Cmd ",
            Style::default().bg(pill_bg).fg(Color::Rgb(240, 180, 80)),
        ),
        Span::styled("", Style::default().bg(bar_bg).fg(pill_bg)),
    ]);

    let spinner_icon = SPINNER[(editor.spinner_tick / 3) % SPINNER.len()];

    let mut status_right_spans = Vec::new();

    // LSP Lifecycle Status Indicator
    match &editor.lsp_status {
        LspStatus::Starting(name) => {
            status_right_spans.push(Span::styled(
                format!(" {spinner_icon} {name} "),
                Style::default()
                    .bg(bar_bg)
                    .fg(Color::Rgb(245, 185, 60))
                    .add_modifier(Modifier::BOLD),
            ));
        }
        LspStatus::Ready(name) => {
            status_right_spans.push(Span::styled(
                format!(" 󰄬 {name} "),
                Style::default().bg(bar_bg).fg(Color::Rgb(100, 200, 140)),
            ));
        }
        LspStatus::Error(err) => {
            status_right_spans.push(Span::styled(
                format!(" 󰅚 LSP: {err} "),
                Style::default()
                    .bg(bar_bg)
                    .fg(Color::Rgb(240, 90, 90))
                    .add_modifier(Modifier::BOLD),
            ));
        }
        LspStatus::NotFound(name) => {
            status_right_spans.push(Span::styled(
                format!(" 󰄰 {name} missing "),
                Style::default().bg(bar_bg).fg(Color::Rgb(240, 140, 70)),
            ));
        }
        LspStatus::Disabled => {}
    }

    // Diagnostics Indicators
    if error_count > 0 {
        status_right_spans.push(Span::styled(
            format!("  {error_count} "),
            Style::default().bg(bar_bg).fg(Color::Rgb(240, 90, 90)),
        ));
    }
    if warn_count > 0 {
        status_right_spans.push(Span::styled(
            format!(" {warn_count} "),
            Style::default().bg(bar_bg).fg(Color::Rgb(245, 185, 60)),
        ));
    }

    status_right_spans.extend(vec![
        Span::styled("", Style::default().bg(bar_bg).fg(pill_bg)),
        Span::styled(
            format!("  {total_lines}L "),
            Style::default().bg(pill_bg).fg(Color::Rgb(160, 165, 180)),
        ),
        Span::styled("", Style::default().bg(pill_bg).fg(badge_color)),
        Span::styled(
            format!(" 󰆤 {}:{} ", editor.cursor_y + 1, editor.cursor_x + 1),
            Style::default()
                .bg(badge_color)
                .fg(Color::Rgb(15, 17, 22))
                .add_modifier(Modifier::BOLD),
        ),
    ]);

    frame.render_widget(
        Block::default().style(Style::default().bg(bar_bg)),
        main_chunks[1],
    );
    frame.render_widget(Paragraph::new(status_left), main_chunks[1]);
    frame.render_widget(
        Paragraph::new(Line::from(status_right_spans)).alignment(ratatui::layout::Alignment::Right),
        main_chunks[1],
    );

    // 4. Diagnostics / Bottom Notification Area
    if editor.mode == Mode::Command {
        let prompt_line = Line::from(vec![
            Span::styled(
                " :",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(&editor.command_buffer),
        ]);
        frame.render_widget(Paragraph::new(prompt_line), main_chunks[2]);
        frame.set_cursor_position(Position::new(
            (2 + editor.command_buffer.len()) as u16,
            main_chunks[2].y,
        ));
    } else {
        let active_diag = editor
            .diagnostics
            .iter()
            .find(|d| d.line == editor.cursor_y);
        let msg_line = if let Some(diag) = active_diag {
            let (d_icon, icon_style) = match diag.severity {
                1 => ("  ", Style::default().fg(Color::Rgb(240, 90, 90))),
                2 => ("  ", Style::default().fg(Color::Rgb(245, 185, 60))),
                _ => (" 󰌵 ", Style::default().fg(Color::Rgb(100, 180, 255))),
            };
            Line::from(vec![
                Span::styled(d_icon, icon_style),
                Span::styled(
                    &diag.message,
                    Style::default()
                        .fg(Color::Rgb(230, 235, 245))
                        .add_modifier(Modifier::ITALIC),
                ),
            ])
        } else {
            Line::from(vec![
                Span::styled(" 󰅂 ", Style::default().fg(Color::DarkGray)),
                Span::styled(
                    &editor.status_msg,
                    Style::default().fg(Color::Rgb(170, 175, 190)),
                ),
            ])
        };

        frame.render_widget(Paragraph::new(msg_line), main_chunks[2]);

        // Use exact screen coordinates computed during line rendering
        let (screen_x, screen_y) =
            cursor_screen_pos.unwrap_or((inner_area.x + gutter_width as u16, inner_area.y));

        // Floating Auto-Complete Dropdown
        if editor.mode == Mode::Insert
            && editor.completion_visible
            && !editor.completions.is_empty()
        {
            let max_visible_items = 6usize;
            let total_items = editor.completions.len();
            let count = total_items.min(max_visible_items);
            let popup_height = (count as u16) + 2;
            let popup_width = 38u16.min(frame.area().width.saturating_sub(screen_x).max(22));

            let mut popup_x = screen_x;
            if popup_x + popup_width > frame.area().width {
                popup_x = frame.area().width.saturating_sub(popup_width);
            }

            let popup_y = if screen_y + 1 + popup_height < frame.area().bottom() {
                screen_y + 1
            } else {
                screen_y.saturating_sub(popup_height)
            };

            let popup_rect = Rect::new(popup_x, popup_y, popup_width, popup_height);
            frame.render_widget(Clear, popup_rect);

            let scroll_start = editor.completion_scroll;
            let scroll_end = (scroll_start + max_visible_items).min(total_items);

            let mut list_lines = Vec::new();
            for i in scroll_start..scroll_end {
                let item = &editor.completions[i];
                let is_sel = i == editor.completion_idx;

                let (kind_icon, kind_color) = completion_kind_icon(item.kind);
                let item_bg = if is_sel {
                    Color::Rgb(40, 75, 145)
                } else {
                    Color::Rgb(25, 27, 34)
                };
                let text_style = if is_sel {
                    Style::default()
                        .bg(item_bg)
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().bg(item_bg).fg(Color::Rgb(215, 220, 230))
                };

                let avail_width = (popup_width as usize).saturating_sub(6);
                let label_text = if item.label.len() > avail_width {
                    format!("{}…", &item.label[..avail_width.saturating_sub(1)])
                } else {
                    item.label.clone()
                };

                let padding = avail_width.saturating_sub(label_text.chars().count());

                list_lines.push(Line::from(vec![
                    Span::styled(" ", Style::default().bg(item_bg)),
                    Span::styled(kind_icon, Style::default().bg(item_bg).fg(kind_color)),
                    Span::styled(" ", Style::default().bg(item_bg)),
                    Span::styled(label_text, text_style),
                    Span::styled(" ".repeat(padding), Style::default().bg(item_bg)),
                ]));
            }

            let title_info = format!(" {}/{} ", editor.completion_idx + 1, total_items);
            let comp_block = Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(Color::Rgb(80, 140, 255)))
                .style(Style::default().bg(Color::Rgb(25, 27, 34)))
                .title(Line::from(Span::styled(
                    title_info,
                    Style::default().fg(Color::Rgb(140, 160, 200)),
                )));

            frame.render_widget(Paragraph::new(list_lines).block(comp_block), popup_rect);
        }

        if editor.focus == Focus::Editor
            && let Some((cx, cy)) = cursor_screen_pos
            && cx < inner_area.right()
            && cy < inner_area.bottom()
        {
            frame.set_cursor_position(Position::new(cx, cy));
        }
    }

    // 5. Render Command Palette Modal
    if editor.palette.visible {
        let width = 46u16.min(size.width.saturating_sub(2));
        let height = 12u16.min(size.height.saturating_sub(2));
        let x = (size.width.saturating_sub(width)) / 2;
        let y = 1u16;

        let palette_rect = Rect::new(x, y, width, height);
        frame.render_widget(Clear, palette_rect);

        let filtered = editor.palette.filtered_commands();
        let max_visible = (height.saturating_sub(4)) as usize;

        if editor.palette.selected_idx < editor.palette.scroll {
            editor.palette.scroll = editor.palette.selected_idx;
        } else if editor.palette.selected_idx >= editor.palette.scroll + max_visible {
            editor.palette.scroll = editor.palette.selected_idx + 1 - max_visible;
        }

        let mut palette_lines = Vec::new();
        palette_lines.push(Line::from(vec![
            Span::styled(
                " 󰍉 > ",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                &editor.palette.query,
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled("█", Style::default().fg(Color::Rgb(100, 180, 255))),
        ]));
        palette_lines.push(Line::from(Span::styled(
            "─".repeat((width as usize).saturating_sub(2)),
            Style::default().fg(Color::Rgb(60, 65, 80)),
        )));

        let scroll_start = editor.palette.scroll;
        let scroll_end = (scroll_start + max_visible).min(filtered.len());

        for i in scroll_start..scroll_end {
            let cmd = filtered[i];
            let is_sel = i == editor.palette.selected_idx;

            let row_bg = if is_sel {
                Color::Rgb(40, 75, 145)
            } else {
                Color::Rgb(25, 27, 34)
            };
            let title_style = if is_sel {
                Style::default()
                    .bg(row_bg)
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().bg(row_bg).fg(Color::Rgb(215, 220, 230))
            };

            let avail_title_width = (width as usize).saturating_sub(cmd.shortcut.len() + 8);
            let display_title = if cmd.title.len() > avail_title_width {
                format!("{}…", &cmd.title[..avail_title_width.saturating_sub(1)])
            } else {
                cmd.title.to_string()
            };

            let padding = avail_title_width.saturating_sub(display_title.chars().count());

            palette_lines.push(Line::from(vec![
                Span::styled(" ", Style::default().bg(row_bg)),
                Span::styled(
                    cmd.icon,
                    Style::default().bg(row_bg).fg(Color::Rgb(100, 180, 255)),
                ),
                Span::styled(" ", Style::default().bg(row_bg)),
                Span::styled(display_title, title_style),
                Span::styled(" ".repeat(padding), Style::default().bg(row_bg)),
                Span::styled(
                    format!(" {} ", cmd.shortcut),
                    Style::default().bg(row_bg).fg(Color::Rgb(140, 145, 160)),
                ),
            ]));
        }

        let p_block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(Color::Rgb(80, 140, 255)))
            .style(Style::default().bg(Color::Rgb(25, 27, 34)))
            .title(Line::from(Span::styled(
                " 󰍉 Command Palette (Esc to close) ",
                Style::default()
                    .fg(Color::Rgb(180, 200, 240))
                    .add_modifier(Modifier::BOLD),
            )));

        frame.render_widget(Paragraph::new(palette_lines).block(p_block), palette_rect);
    }
    // 6. Render LSP Picker Modal Overlay
    if let Some(picker) = &editor.lsp_picker {
        let width = 48u16.min(size.width.saturating_sub(2));
        let height = ((picker.candidates.len() as u16) + 4).min(size.height.saturating_sub(2));
        let x = (size.width.saturating_sub(width)) / 2;
        let y = (size.height.saturating_sub(height)) / 2;

        let picker_rect = Rect::new(x, y, width, height);
        frame.render_widget(Clear, picker_rect);

        let mut lines = Vec::new();
        lines.push(Line::from(vec![
            Span::styled(
                " Detected LSPs for ",
                Style::default().fg(Color::Rgb(160, 170, 185)),
            ),
            Span::styled(
                &picker.language_id,
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                " (Select with Enter):",
                Style::default().fg(Color::Rgb(160, 170, 185)),
            ),
        ]));
        lines.push(Line::from(Span::styled(
            "─".repeat((width as usize).saturating_sub(2)),
            Style::default().fg(Color::Rgb(60, 65, 80)),
        )));

        for (idx, candidate) in picker.candidates.iter().enumerate() {
            let is_sel = idx == picker.selected_idx;
            let bg = if is_sel {
                Color::Rgb(40, 75, 145)
            } else {
                Color::Rgb(25, 27, 34)
            };
            let style = if is_sel {
                Style::default()
                    .bg(bg)
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().bg(bg).fg(Color::Rgb(215, 220, 230))
            };

            lines.push(Line::from(vec![
                Span::styled(
                    if is_sel { " 󰄬 " } else { "   " },
                    Style::default().bg(bg).fg(Color::Green),
                ),
                Span::styled(format!("{candidate:<38}"), style),
            ]));
        }

        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(Color::Rgb(100, 180, 255)))
            .style(Style::default().bg(Color::Rgb(25, 27, 34)))
            .title(Line::from(Span::styled(
                "  Choose Language Server ",
                Style::default()
                    .fg(Color::Rgb(180, 200, 240))
                    .add_modifier(Modifier::BOLD),
            )));

        frame.render_widget(Paragraph::new(lines).block(block), picker_rect);
    }

    // 7. Render In-Editor Help Modal
    if editor.show_help {
        let width = 64u16.min(size.width.saturating_sub(4));
        let height = 22u16.min(size.height.saturating_sub(2));
        let x = (size.width.saturating_sub(width)) / 2;
        let y = (size.height.saturating_sub(height)) / 2;

        let help_rect = Rect::new(x, y, width, height);
        frame.render_widget(Clear, help_rect);

        let help_block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(Color::Rgb(100, 180, 255)))
            .style(Style::default().bg(Color::Rgb(22, 25, 32)))
            .title(Line::from(vec![
                Span::styled(
                    " 󰋖 subject0 Keybindings & Help ",
                    Style::default()
                        .fg(Color::Rgb(240, 200, 90))
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    "(Esc to close) ",
                    Style::default().fg(Color::Rgb(140, 145, 160)),
                ),
            ]));

        let inner_rect = help_block.inner(help_rect);
        frame.render_widget(help_block, help_rect);

        let c_sec = Style::default()
            .fg(Color::Rgb(80, 210, 240))
            .add_modifier(Modifier::BOLD);
        let c_key = Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD);
        let c_desc = Style::default().fg(Color::Rgb(215, 220, 230));

        let content = vec![
            Line::from(Span::styled("NORMAL MODE MOTIONS", c_sec)),
            Line::from(vec![
                Span::styled("  h, j, k, l     ", c_key),
                Span::styled("Move cursor left, down, up, right", c_desc),
            ]),
            Line::from(vec![
                Span::styled("  0, $           ", c_key),
                Span::styled("Move to start / end of line", c_desc),
            ]),
            Line::from(vec![
                Span::styled("  gg, G          ", c_key),
                Span::styled("Jump to top / bottom of document", c_desc),
            ]),
            Line::from(vec![
                Span::styled("  i, I, a, A     ", c_key),
                Span::styled("Enter Insert mode (cursor/start/after/end)", c_desc),
            ]),
            Line::from(vec![
                Span::styled("  o, O           ", c_key),
                Span::styled("Insert new line below / above", c_desc),
            ]),
            Line::from(vec![
                Span::styled("  dd, x          ", c_key),
                Span::styled("Cut current line / character into clipboard", c_desc),
            ]),
            Line::from(vec![
                Span::styled("  y, p, u        ", c_key),
                Span::styled("Yank line, Paste clipboard, Undo edit", c_desc),
            ]),
            Line::from(vec![
                Span::styled("  ~, J           ", c_key),
                Span::styled("Toggle character case, Join lines", c_desc),
            ]),
            Line::from(vec![
                Span::styled("  v, %           ", c_key),
                Span::styled("Enter Visual mode, Select entire buffer", c_desc),
            ]),
            Line::from(vec![
                Span::styled("  Space          ", c_key),
                Span::styled("Open Command Palette", c_desc),
            ]),
            Line::from(vec![
                Span::styled("  ?              ", c_key),
                Span::styled("Open this Keybindings & Help modal", c_desc),
            ]),
            Line::from(Span::raw("")),
            Line::from(Span::styled("INSERT MODE", c_sec)),
            Line::from(vec![
                Span::styled("  Esc            ", c_key),
                Span::styled("Return to Normal mode", c_desc),
            ]),
            Line::from(vec![
                Span::styled("  Tab            ", c_key),
                Span::styled("Insert 4 soft spaces (or accept completion)", c_desc),
            ]),
            Line::from(vec![
                Span::styled("  Ctrl-Space     ", c_key),
                Span::styled("Trigger LSP completion popup manually", c_desc),
            ]),
            Line::from(vec![
                Span::styled("  (, [, {, \", '  ", c_key),
                Span::styled("Auto-closing delimiter pairs", c_desc),
            ]),
            Line::from(Span::raw("")),
            Line::from(Span::styled("COMMAND MODE & SHORTCUTS", c_sec)),
            Line::from(vec![
                Span::styled("  :w [file]      ", c_key),
                Span::styled("Save buffer to disk", c_desc),
            ]),
            Line::from(vec![
                Span::styled("  :q, :q!        ", c_key),
                Span::styled("Quit editor (force quit without saving)", c_desc),
            ]),
            Line::from(vec![
                Span::styled("  :wq            ", c_key),
                Span::styled("Save and exit", c_desc),
            ]),
            Line::from(vec![
                Span::styled("  :lsp           ", c_key),
                Span::styled("Open Language Server picker", c_desc),
            ]),
            Line::from(vec![
                Span::styled("  :cfg           ", c_key),
                Span::styled("Save configuration to .subject0", c_desc),
            ]),
            Line::from(vec![
                Span::styled("  :wrap          ", c_key),
                Span::styled("Toggle soft line-wrapping", c_desc),
            ]),
            Line::from(vec![
                Span::styled("  Ctrl-E / :e    ", c_key),
                Span::styled("Toggle File Explorer sidebar", c_desc),
            ]),
        ];

        let total_lines = content.len();
        let visible_lines = inner_rect.height as usize;
        let max_scroll = total_lines.saturating_sub(visible_lines);
        if editor.help_scroll > max_scroll {
            editor.help_scroll = max_scroll;
        }

        let slice: Vec<Line> = content
            .into_iter()
            .skip(editor.help_scroll)
            .take(visible_lines)
            .collect();
        frame.render_widget(Paragraph::new(slice), inner_rect);
    }
}
