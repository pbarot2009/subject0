//! # Application Entry Point, Event Loop, & Terminal UI Subsystem
//!
//! This module serves as the runtime orchestrator for `subject0`. It integrates the
//! terminal lifecycle, asynchronous event multiplexing, input decoding, themed frame
//! rendering, and modal subsystem coordination for full Language Server Protocol (LSP)
//! intelligence and real-time Git diff tracking.

mod cmd;
mod editor;
mod git;
mod lsp;
mod nerdfonts;
mod syntax;
mod theme;
mod ui;

use std::{
    cmp::Ordering,
    io::{Write, stdout},
    time::Duration,
};

use anyhow::Result;
use crossterm::{
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind,
        KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
    },
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{Terminal, backend::CrosstermBackend, layout::Size};
use tokio::sync::mpsc;

use editor::{CodeActionPicker, Editor, Focus, LocationPicker, Mode, SymbolPicker};
use git::{GitInbound, GitOutbound, run_git_actor};
use lsp::{LspOutbound, LspStatus, SuggestionItem, utf16_to_char_col};
use theme::Theme;
use ui::render_ui;

/// RAII Terminal Guard ensuring the host terminal is reliably restored
/// to canonical mode regardless of exit status[span_8](start_span)[span_8](end_span).
struct TerminalGuard;

impl TerminalGuard {
    fn enter() -> Result<Self> {
        enable_raw_mode()?;
        let mut out = stdout();
        execute!(out, EnterAlternateScreen, EnableMouseCapture)?;
        Ok(Self)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let mut out = stdout();
        let _ = out.write_all(b"\x1b[0 q");
        let _ = disable_raw_mode();
        let _ = execute!(out, LeaveAlternateScreen, DisableMouseCapture);
        let _ = out.flush();
    }
}

/// Configures the terminal hardware cursor geometry based on the active modal editing state[span_9](start_span)[span_9](end_span).
fn set_terminal_cursor_style(mode: Mode) {
    let mut stdout = stdout();
    match mode {
        Mode::Normal | Mode::Command | Mode::Visual { .. } => {
            let _ = stdout.write_all(b"\x1b[2 q"); // Steady Block
        }
        Mode::Insert => {
            let _ = stdout.write_all(b"\x1b[6 q"); // Steady Bar / I-Beam
        }
    }
    let _ = stdout.flush();
}

/// Registers a secondary panic hook ensuring screen recovery during thread unwinding[span_10](start_span)[span_10](end_span).
fn setup_panic_hook() {
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let mut out = stdout();
        let _ = out.write_all(b"\x1b[0 q");
        let _ = disable_raw_mode();
        let _ = execute!(out, LeaveAlternateScreen, DisableMouseCapture);
        let _ = out.flush();
        hook(info);
    }));
}

// === Application Lifecycle & Event Loop ===

#[tokio::main]
async fn main() -> Result<()> {
    let cli_args = cmd::CliArgs::parse();

    setup_panic_hook();
    let _guard = TerminalGuard::enter()?;
    let backend = CrosstermBackend::new(stdout());
    let mut terminal = Terminal::new(backend)?;

    let target_path = cli_args.path.clone();
    let mut editor = Editor::new(target_path.as_ref())?;

    if cli_args.ignore_config {
        editor.config = editor::AppConfig {
            preferred_lsps: std::collections::HashMap::new(),
            line_wrap: true,
            theme: "gruber-darker".to_string(),
            show_inlay_hints: true,
            source_path: editor.config.source_path.clone(),
        };
        editor.theme = Theme::gruber_darker();
    }
    if let Some(wrap) = cli_args.line_wrap {
        editor.line_wrap = wrap;
    }
    if let Some(line) = cli_args.jump_line {
        editor.cursor_y = line.saturating_sub(1);
        editor.clamp_cursor();
    }

    // Initialize LSP messaging pipeline[span_11](start_span)[span_11](end_span)
    let (lsp_out_tx, mut lsp_out_rx) = mpsc::unbounded_channel::<LspOutbound>();
    editor.lsp_out_tx = Some(lsp_out_tx.clone());

    // Initialize background Git actor pipeline
    let (git_in_tx, git_in_rx) = mpsc::unbounded_channel::<GitInbound>();
    let (git_out_tx, mut git_out_rx) = mpsc::unbounded_channel::<GitOutbound>();
    editor.git_tx = Some(git_in_tx);
    tokio::spawn(run_git_actor(git_in_rx, git_out_tx));

    if let Some(path) = &target_path
        && path.is_file()
    {
        editor.ensure_lsp_for_file(path);
        editor.request_git_diff();
    }

    set_terminal_cursor_style(editor.mode);

    let mut needs_redraw = true;

    while !editor.should_quit {
        let mut received_bg_msg = false;

        // Drain LSP background messages[span_12](start_span)[span_12](end_span)
        while let Ok(msg) = lsp_out_rx.try_recv() {
            received_bg_msg = true;
            match msg {
                LspOutbound::Status(s) => {
                    let was_ready = matches!(s, LspStatus::Ready(_));
                    editor.lsp_status = s;
                    if was_ready {
                        editor.request_semantic_tokens();
                        editor.request_inlay_hints();
                    }
                }
                LspOutbound::SemanticTokens { tokens } => {
                    editor.syntax.set_semantic_tokens(tokens);
                }
                LspOutbound::Diagnostics(d) => editor.diagnostics = d,
                LspOutbound::InlayHints { req_id: _, hints } => {
                    editor.inlay_hints = hints;
                }
                LspOutbound::Hover { req_id: _, hover } => {
                    if let Some(info) = hover {
                        editor.hover_info = Some(info);
                        editor.hover_scroll = 0;
                        editor.status_msg = "Hover documentation loaded".to_string();
                    } else {
                        editor.status_msg = "No hover documentation available".to_string();
                    }
                }
                LspOutbound::SignatureHelp { req_id: _, help } => {
                    editor.signature_help = help;
                }
                LspOutbound::Definition {
                    req_id: _,
                    locations,
                } => {
                    if locations.is_empty() {
                        editor.status_msg = "No definition found".to_string();
                    } else if locations.len() == 1 {
                        let loc = locations.into_iter().next().unwrap();
                        editor.jump_to_location(loc);
                    } else {
                        editor.location_picker = Some(LocationPicker {
                            title: "Go to Definition",
                            locations,
                            selected_idx: 0,
                            scroll: 0,
                        });
                    }
                }
                LspOutbound::References {
                    req_id: _,
                    locations,
                } => {
                    if locations.is_empty() {
                        editor.status_msg = "No references found".to_string();
                    } else if locations.len() == 1 {
                        let loc = locations.into_iter().next().unwrap();
                        editor.jump_to_location(loc);
                    } else {
                        editor.location_picker = Some(LocationPicker {
                            title: "Find References",
                            locations,
                            selected_idx: 0,
                            scroll: 0,
                        });
                    }
                }
                LspOutbound::Formatting { req_id: _, edits } => {
                    if edits.is_empty() {
                        editor.status_msg = "Buffer already formatted".to_string();
                    } else {
                        editor.apply_text_edits(&edits);
                        editor.status_msg = "Formatted document with LSP".to_string();
                    }
                }
                LspOutbound::CodeActions { req_id: _, actions } => {
                    if actions.is_empty() {
                        editor.status_msg = "No code actions available at cursor".to_string();
                    } else {
                        editor.code_action_picker = Some(CodeActionPicker {
                            actions,
                            selected_idx: 0,
                        });
                    }
                }
                LspOutbound::Rename { req_id: _, changes } => {
                    match editor.apply_workspace_edits(&changes) {
                        Ok(count) => {
                            editor.status_msg =
                                format!("Renamed symbol ({count} file edits applied)");
                        }
                        Err(e) => {
                            editor.status_msg = format!("Rename failed: {e}");
                        }
                    }
                }
                LspOutbound::DocumentSymbols { req_id: _, symbols } => {
                    if symbols.is_empty() {
                        editor.status_msg = "No document symbols found".to_string();
                    } else {
                        editor.symbol_picker = Some(SymbolPicker {
                            symbols,
                            query: String::new(),
                            selected_idx: 0,
                            scroll: 0,
                        });
                    }
                }
                LspOutbound::Completions { req_id, items } => {
                    if req_id == editor.lsp_req_id && !items.is_empty() {
                        let prefix = editor.current_word_prefix();
                        let prefix_lower = prefix.to_lowercase();

                        let mut filtered: Vec<SuggestionItem> = items
                            .into_iter()
                            .filter(|it| {
                                prefix_lower.is_empty()
                                    || it.label.to_lowercase().contains(&prefix_lower)
                            })
                            .collect();

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

        // Drain Git background messages
        while let Ok(msg) = git_out_rx.try_recv() {
            received_bg_msg = true;
            match msg {
                GitOutbound::DiffSummary { path, summary } => {
                    let is_current = editor
                        .path
                        .as_ref()
                        .is_some_and(|p| p.canonicalize().ok() == path.canonicalize().ok())
                        || editor.path.as_ref() == Some(&path);

                    if is_current {
                        editor.git_diff = summary;
                    }
                }
            }
        }

        if received_bg_msg {
            needs_redraw = true;
        }

        let is_loading = matches!(editor.lsp_status, LspStatus::Starting(_));
        if is_loading {
            editor.spinner_tick = editor.spinner_tick.wrapping_add(1);
            needs_redraw = true;
        }

        if needs_redraw {
            terminal.draw(|f| render_ui(f, &mut editor))?;
            needs_redraw = false;
        }

        let poll_duration = if is_loading {
            Duration::from_millis(60)
        } else {
            Duration::from_millis(20)
        };

        if event::poll(poll_duration)? {
            match event::read()? {
                Event::Key(key) => {
                    handle_key_event(&mut editor, key);
                    needs_redraw = true;
                }
                Event::Mouse(mouse) => {
                    handle_mouse_event(&mut editor, mouse, terminal.size()?);
                    needs_redraw = true;
                }
                Event::Resize(_, _) => {
                    needs_redraw = true;
                }
                _ => {}
            }
        }
    }

    Ok(())
}

// === Touch & Mouse Handling ===

fn handle_mouse_event(editor: &mut Editor, mouse: MouseEvent, size: Size) {
    let status_row = size.height.saturating_sub(2);
    let cmd_row = size.height.saturating_sub(1);
    let viewport_top = 1u16;
    let viewport_bottom = size.height.saturating_sub(3);

    // Dismiss active hover card on outside click[span_13](start_span)[span_13](end_span)
    if editor.hover_info.is_some() {
        if let MouseEventKind::Down(MouseButton::Left) = mouse.kind {
            editor.hover_info = None;
        }
        return;
    }

    // Intercept In-Editor Help Modal[span_14](start_span)[span_14](end_span)
    if editor.show_help {
        if let MouseEventKind::Down(MouseButton::Left) = mouse.kind {
            let width = 64u16.min(size.width.saturating_sub(4));
            let height = 22u16.min(size.height.saturating_sub(2));
            let x = (size.width.saturating_sub(width)) / 2;
            let y = (size.height.saturating_sub(height)) / 2;

            if mouse.column < x
                || mouse.column >= x + width
                || mouse.row < y
                || mouse.row >= y + height
            {
                editor.show_help = false;
            }
        }
        return;
    }

    // Intercept Theme Picker Modal[span_15](start_span)[span_15](end_span)
    if editor.theme_picker.is_some() {
        if let MouseEventKind::Down(MouseButton::Left) = mouse.kind {
            let themes = Theme::all();
            let width = 48u16.min(size.width.saturating_sub(2));
            let height = ((themes.len() as u16) + 4).min(size.height.saturating_sub(2));
            let x = (size.width.saturating_sub(width)) / 2;
            let y = (size.height.saturating_sub(height)) / 2;

            if mouse.column > x
                && mouse.column < x + width - 1
                && mouse.row >= y + 2
                && mouse.row < y + 2 + themes.len() as u16
            {
                let clicked_idx = (mouse.row - (y + 2)) as usize;
                if clicked_idx < themes.len() {
                    editor.set_theme(themes[clicked_idx].name);
                    editor.theme_picker = None;
                }
            } else if mouse.column < x
                || mouse.column >= x + width
                || mouse.row < y
                || mouse.row >= y + height
            {
                editor.theme_picker = None;
            }
        }
        return;
    }

    // Intercept LSP Server Picker Modal[span_16](start_span)[span_16](end_span)
    if let Some(picker) = &editor.lsp_picker {
        if let MouseEventKind::Down(MouseButton::Left) = mouse.kind {
            let width = 48u16.min(size.width.saturating_sub(2));
            let height = ((picker.candidates.len() as u16) + 4).min(size.height.saturating_sub(2));
            let x = (size.width.saturating_sub(width)) / 2;
            let y = (size.height.saturating_sub(height)) / 2;

            if mouse.column > x
                && mouse.column < x + width - 1
                && mouse.row >= y + 2
                && mouse.row < y + 2 + picker.candidates.len() as u16
            {
                let clicked_idx = (mouse.row - (y + 2)) as usize;
                if clicked_idx < picker.candidates.len() {
                    let chosen = picker.candidates[clicked_idx].clone();
                    let lang_id = picker.language_id.clone();
                    editor
                        .config
                        .preferred_lsps
                        .insert(lang_id.clone(), chosen.clone());
                    let _ = editor.config.save();
                    editor.status_msg = format!("Selected LSP: {chosen} (saved to .subject0)");
                    if let Some(path) = editor.path.clone() {
                        editor.start_lsp_server(&path, &lang_id, &chosen);
                    }
                    editor.lsp_picker = None;
                }
            } else if mouse.column < x
                || mouse.column >= x + width
                || mouse.row < y
                || mouse.row >= y + height
            {
                editor.lsp_picker = None;
            }
        }
        return;
    }

    // Intercept Command Palette Interactions[span_17](start_span)[span_17](end_span)
    if editor.palette.visible {
        let width = 46u16.min(size.width.saturating_sub(2));
        let height = 12u16.min(size.height.saturating_sub(2));
        let x = (size.width.saturating_sub(width)) / 2;
        let y = 1u16;

        let in_palette = mouse.column >= x
            && mouse.column < x + width
            && mouse.row >= y
            && mouse.row < y + height;

        let cmds = editor.palette.filtered_commands();
        match mouse.kind {
            MouseEventKind::ScrollDown if in_palette => {
                if !cmds.is_empty() {
                    editor.palette.selected_idx = (editor.palette.selected_idx + 1) % cmds.len();
                }
                return;
            }
            MouseEventKind::ScrollUp if in_palette => {
                if !cmds.is_empty() {
                    editor.palette.selected_idx = if editor.palette.selected_idx == 0 {
                        cmds.len().saturating_sub(1)
                    } else {
                        editor.palette.selected_idx - 1
                    };
                }
                return;
            }
            MouseEventKind::Down(MouseButton::Left) => {
                if mouse.column > x
                    && mouse.column < x + width - 1
                    && mouse.row >= y + 3
                    && mouse.row < y + height - 1
                {
                    let clicked_row = (mouse.row - (y + 3)) as usize;
                    let actual_idx = editor.palette.scroll + clicked_row;
                    if actual_idx < cmds.len() {
                        let cmd_id = cmds[actual_idx].id;
                        editor.execute_palette_command(cmd_id);
                        set_terminal_cursor_style(editor.mode);
                    }
                    return;
                } else if !in_palette {
                    editor.palette.visible = false;
                    return;
                }
            }
            _ => {}
        }
        return;
    }

    // Statusline Interactions[span_18](start_span)[span_18](end_span)
    if mouse.row == status_row {
        if let MouseEventKind::Down(MouseButton::Left) = mouse.kind {
            let badge_len = match editor.mode {
                Mode::Normal | Mode::Insert | Mode::Visual { .. } => 8,
                Mode::Command => 9,
            } + 1;

            let col = mouse.column as usize;
            if col <= badge_len {
                editor.set_mode(match editor.mode {
                    Mode::Normal => Mode::Insert,
                    Mode::Insert | Mode::Command | Mode::Visual { .. } => Mode::Normal,
                });
                set_terminal_cursor_style(editor.mode);
                editor.completion_visible = false;
            }
        }
        return;
    }

    if mouse.row == cmd_row {
        return;
    }

    let explorer_width = if editor.explorer.visible {
        if size.width < 70 {
            (size.width * 7 / 10).max(26).min(size.width)
        } else {
            26u16
        }
    } else {
        0u16
    };

    // File Explorer Sidebar Interactions[span_19](start_span)[span_19](end_span)
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
                            if let Err(e) = editor.open_file(path) {
                                editor.status_msg = e.to_string();
                            } else {
                                editor.focus = Focus::Editor;
                                if size.width < 70 {
                                    editor.explorer.visible = false;
                                }
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

    // Completion Dropdown Interactions[span_20](start_span)[span_20](end_span)
    if editor.completion_visible
        && !editor.completions.is_empty()
        && let Some((px, py, pw, ph)) = editor.completion_rect
    {
        let in_popup =
            mouse.column >= px && mouse.column < px + pw && mouse.row >= py && mouse.row < py + ph;

        if in_popup {
            let max_visible = 6usize;
            match mouse.kind {
                MouseEventKind::ScrollDown => {
                    if editor.completion_idx + 1 < editor.completions.len() {
                        editor.completion_idx += 1;
                        editor.update_completion_scroll(max_visible);
                    }
                    return;
                }
                MouseEventKind::ScrollUp => {
                    if editor.completion_idx > 0 {
                        editor.completion_idx -= 1;
                        editor.update_completion_scroll(max_visible);
                    }
                    return;
                }
                MouseEventKind::Down(MouseButton::Left) => {
                    if mouse.row > py && mouse.row < py + ph - 1 {
                        let clicked_row = (mouse.row - (py + 1)) as usize;
                        let target_idx = editor.completion_scroll + clicked_row;
                        if target_idx < editor.completions.len() {
                            editor.completion_idx = target_idx;
                        }
                    }
                    editor.accept_completion();
                    return;
                }
                _ => return,
            }
        } else if let MouseEventKind::Down(MouseButton::Left) = mouse.kind {
            editor.completion_visible = false;
            editor.completion_rect = None;
        }
    }

    // Document Viewport Buffer Interactions[span_21](start_span)[span_21](end_span)
    let gutter_digits = editor.rope.len_lines().max(1).to_string().len().max(2);
    let gutter_width = gutter_digits + 7;
    let content_left = explorer_width + 1u16 + gutter_width as u16;

    match mouse.kind {
        MouseEventKind::Down(MouseButton::Left) | MouseEventKind::Drag(MouseButton::Left) => {
            if mouse.row >= viewport_top && mouse.row < viewport_bottom {
                let clicked_screen_row = (mouse.row - viewport_top) as usize;
                let target_line = (editor.scroll_y + clicked_screen_row)
                    .min(editor.rope.len_lines().saturating_sub(1));
                editor.cursor_y = target_line;

                if mouse.column >= content_left {
                    let target_visual_x = editor.scroll_x + (mouse.column - content_left) as usize;
                    let raw_line = editor.rope.line(target_line);
                    let mut current_vx = 0;
                    let mut resolved_char_idx = 0;
                    for (c_idx, ch) in raw_line.chars().enumerate() {
                        if current_vx >= target_visual_x || ch == '\n' || ch == '\r' {
                            break;
                        }
                        current_vx += if ch == '\t' { 4 } else { 1 };
                        resolved_char_idx = c_idx + 1;
                    }
                    editor.cursor_x = resolved_char_idx;
                } else {
                    editor.cursor_x = 0;
                }

                editor.clamp_cursor();
                editor.completion_visible = false;
                editor.hover_info = None;
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
            editor.completion_visible = false;
            editor.hover_info = None;
        }
        MouseEventKind::ScrollDown if editor.scroll_y + 3 < editor.rope.len_lines() => {
            editor.scroll_y += 3;
            editor.cursor_y = (editor.cursor_y + 3).min(editor.rope.len_lines().saturating_sub(1));
            editor.clamp_cursor();
            editor.completion_visible = false;
            editor.hover_info = None;
        }
        _ => {}
    }
}

// === Keyboard Controller ===

fn handle_key_event(editor: &mut Editor, key: KeyEvent) {
    if key.kind == KeyEventKind::Release {
        return;
    }

    let prev_mode = editor.mode;
    let max_visible = 6usize;

    // 1. Rename Symbol Prompt Input[span_22](start_span)[span_22](end_span)
    if let Some(mut name) = editor.rename_prompt.take() {
        match key.code {
            KeyCode::Esc => {
                editor.status_msg = "Rename canceled".to_string();
            }
            KeyCode::Enter => {
                editor.request_rename(&name);
            }
            KeyCode::Backspace => {
                name.pop();
                editor.rename_prompt = Some(name);
            }
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                name.push(c);
                editor.rename_prompt = Some(name);
            }
            _ => {
                editor.rename_prompt = Some(name);
            }
        }
        return;
    }

    // 2. Hover Card Viewer[span_23](start_span)[span_23](end_span)
    if editor.hover_info.is_some() {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                editor.hover_info = None;
                return;
            }
            KeyCode::Down | KeyCode::Char('j') => {
                editor.hover_scroll = editor.hover_scroll.saturating_add(1);
                return;
            }
            KeyCode::Up | KeyCode::Char('k') => {
                editor.hover_scroll = editor.hover_scroll.saturating_sub(1);
                return;
            }
            _ => {
                editor.hover_info = None;
            }
        }
    }

    // 3. Code Actions Picker Modal[span_24](start_span)[span_24](end_span)
    if let Some(mut picker) = editor.code_action_picker.take() {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                editor.status_msg = "Code actions canceled".to_string();
            }
            KeyCode::Down | KeyCode::Char('j') => {
                picker.selected_idx = (picker.selected_idx + 1) % picker.actions.len();
                editor.code_action_picker = Some(picker);
            }
            KeyCode::Up | KeyCode::Char('k') => {
                picker.selected_idx = if picker.selected_idx == 0 {
                    picker.actions.len().saturating_sub(1)
                } else {
                    picker.selected_idx - 1
                };
                editor.code_action_picker = Some(picker);
            }
            KeyCode::Enter => {
                if let Some(action) = picker.actions.get(picker.selected_idx) {
                    match editor.apply_workspace_edits(&action.edits) {
                        Ok(_) => {
                            editor.status_msg = format!("Applied: {}", action.title);
                        }
                        Err(e) => {
                            editor.status_msg = format!("Action application error: {e}");
                        }
                    }
                }
            }
            _ => {
                editor.code_action_picker = Some(picker);
            }
        }
        return;
    }

    // 4. Symbol Outline Picker Modal[span_25](start_span)[span_25](end_span)
    if let Some(mut picker) = editor.symbol_picker.take() {
        let filtered_count = picker.filtered_symbols().len();
        match key.code {
            KeyCode::Esc => {
                editor.status_msg = "Symbol search closed".to_string();
            }
            KeyCode::Down => {
                if filtered_count > 0 {
                    picker.selected_idx = (picker.selected_idx + 1) % filtered_count;
                }
                editor.symbol_picker = Some(picker);
            }
            KeyCode::Up => {
                if filtered_count > 0 {
                    picker.selected_idx = if picker.selected_idx == 0 {
                        filtered_count.saturating_sub(1)
                    } else {
                        picker.selected_idx - 1
                    };
                }
                editor.symbol_picker = Some(picker);
            }
            KeyCode::Enter => {
                let filtered = picker.filtered_symbols();
                if let Some(sym) = filtered.get(picker.selected_idx) {
                    editor.record_jump_checkpoint();
                    editor.cursor_y = sym.line.min(editor.rope.len_lines().saturating_sub(1));
                    let line_str = editor.rope.line(editor.cursor_y).to_string();
                    editor.cursor_x = utf16_to_char_col(&line_str, sym.col);
                    editor.clamp_cursor();
                    editor.status_msg = format!("Jumped to symbol {}", sym.name);
                }
            }
            KeyCode::Backspace => {
                picker.query.pop();
                picker.selected_idx = 0;
                editor.symbol_picker = Some(picker);
            }
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                picker.query.push(c);
                picker.selected_idx = 0;
                editor.symbol_picker = Some(picker);
            }
            _ => {
                editor.symbol_picker = Some(picker);
            }
        }
        return;
    }

    // 5. Locations Picker (Definition / References)[span_26](start_span)[span_26](end_span)
    if let Some(mut picker) = editor.location_picker.take() {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                editor.status_msg = "Location picker closed".to_string();
            }
            KeyCode::Down | KeyCode::Char('j') => {
                picker.selected_idx = (picker.selected_idx + 1) % picker.locations.len();
                editor.location_picker = Some(picker);
            }
            KeyCode::Up | KeyCode::Char('k') => {
                picker.selected_idx = if picker.selected_idx == 0 {
                    picker.locations.len().saturating_sub(1)
                } else {
                    picker.selected_idx - 1
                };
                editor.location_picker = Some(picker);
            }
            KeyCode::Enter => {
                if let Some(loc) = picker.locations.get(picker.selected_idx).cloned() {
                    editor.jump_to_location(loc);
                }
            }
            _ => {
                editor.location_picker = Some(picker);
            }
        }
        return;
    }

    // 6. Help Modal Navigation[span_27](start_span)[span_27](end_span)
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

    // 7. Theme Picker Modal[span_28](start_span)[span_28](end_span)
    if let Some(mut picker) = editor.theme_picker.take() {
        let themes = Theme::all();
        match key.code {
            KeyCode::Esc => {
                editor.status_msg = "Theme selection canceled".to_string();
            }
            KeyCode::Down | KeyCode::Char('j') => {
                picker.selected_idx = (picker.selected_idx + 1) % themes.len();
                editor.theme_picker = Some(picker);
            }
            KeyCode::Up | KeyCode::Char('k') => {
                picker.selected_idx = if picker.selected_idx == 0 {
                    themes.len().saturating_sub(1)
                } else {
                    picker.selected_idx - 1
                };
                editor.theme_picker = Some(picker);
            }
            KeyCode::Enter => {
                let chosen = themes[picker.selected_idx].name;
                editor.set_theme(chosen);
            }
            _ => {
                editor.theme_picker = Some(picker);
            }
        }
        return;
    }

    // 8. LSP Server Picker Modal[span_29](start_span)[span_29](end_span)
    if let Some(mut picker) = editor.lsp_picker.take() {
        if picker.candidates.is_empty() {
            editor.status_msg = "No LSP candidates available".to_string();
            return;
        }

        match key.code {
            KeyCode::Esc => {
                editor.status_msg = "LSP selection canceled".to_string();
            }
            KeyCode::Down | KeyCode::Char('j') => {
                picker.selected_idx = (picker.selected_idx + 1) % picker.candidates.len();
                editor.lsp_picker = Some(picker);
            }
            KeyCode::Up | KeyCode::Char('k') => {
                picker.selected_idx = if picker.selected_idx == 0 {
                    picker.candidates.len().saturating_sub(1)
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
                    editor.start_lsp_server(&path, &picker.language_id, &chosen);
                }
            }
            _ => {
                editor.lsp_picker = Some(picker);
            }
        }
        return;
    }

    // 9. Command Palette Key Events[span_30](start_span)[span_30](end_span)
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
                        cmds.len().saturating_sub(1)
                    } else {
                        editor.palette.selected_idx - 1
                    };
                }
            }
            KeyCode::Enter => {
                if !cmds.is_empty() && editor.palette.selected_idx < cmds.len() {
                    let cmd_id = cmds[editor.palette.selected_idx].id;
                    editor.execute_palette_command(cmd_id);
                    set_terminal_cursor_style(editor.mode);
                }
            }
            KeyCode::Backspace => {
                editor.palette.query.pop();
                editor.palette.selected_idx = 0;
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                editor.palette.query.clear();
                editor.palette.selected_idx = 0;
            }
            KeyCode::Char('w') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                let trimmed = editor.palette.query.trim_end();
                if let Some(pos) = trimmed.rfind(' ') {
                    editor.palette.query.truncate(pos + 1);
                } else {
                    editor.palette.query.clear();
                }
                editor.palette.selected_idx = 0;
            }
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                editor.palette.query.push(c);
                editor.palette.selected_idx = 0;
            }
            _ => {}
        }
        return;
    }

    // 10. Global Shortcuts: Ctrl-E for File Explorer[span_31](start_span)[span_31](end_span)
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

    // 11. File Explorer Navigation Focus[span_32](start_span)[span_32](end_span)
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
                        if let Err(e) = editor.open_file(path) {
                            editor.status_msg = e.to_string();
                        } else {
                            editor.focus = Focus::Editor;
                        }
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

    // 12. Modal Editing Handler[span_33](start_span)[span_33](end_span)
    match editor.mode {
        Mode::Normal => {
            editor.completion_visible = false;
            editor.signature_help = None;

            if key.modifiers.contains(KeyModifiers::CONTROL) {
                match key.code {
                    KeyCode::Char('r') => {
                        editor.redo();
                        return;
                    }
                    KeyCode::Char('o') => {
                        editor.jump_backward();
                        return;
                    }
                    KeyCode::Char('i') => {
                        editor.jump_forward();
                        return;
                    }
                    KeyCode::Char(' ') => {
                        editor.set_mode(Mode::Insert);
                        editor.request_completions();
                        return;
                    }
                    _ => {}
                }
            }

            if key.modifiers.contains(KeyModifiers::ALT) && key.code == KeyCode::Char('f') {
                editor.request_formatting();
                return;
            }

            if let Some(pending) = editor.pending_key.take() {
                let handled = match (pending, key.code) {
                    ('d', KeyCode::Char('d')) => {
                        editor.delete_current_line();
                        true
                    }
                    ('g', KeyCode::Char('g')) => {
                        editor.cursor_y = 0;
                        editor.cursor_x = 0;
                        editor.scroll_y = 0;
                        editor.scroll_x = 0;
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
                    ('g', KeyCode::Char('d')) => {
                        editor.request_definition();
                        true
                    }
                    ('g', KeyCode::Char('r')) => {
                        editor.request_references();
                        true
                    }
                    ('g', KeyCode::Char('a')) => {
                        editor.request_code_actions();
                        true
                    }
                    (']', KeyCode::Char('d')) => {
                        editor.next_diagnostic();
                        true
                    }
                    ('[', KeyCode::Char('d')) => {
                        editor.prev_diagnostic();
                        true
                    }
                    (']', KeyCode::Char('c')) => {
                        editor.jump_next_hunk();
                        true
                    }
                    ('[', KeyCode::Char('c')) => {
                        editor.jump_prev_hunk();
                        true
                    }
                    _ => false,
                };
                if handled {
                    editor.clamp_cursor();
                    return;
                }
            }

            match key.code {
                KeyCode::F(2) => {
                    editor.rename_prompt = Some(editor.current_word_prefix());
                }
                KeyCode::Char('K') => {
                    editor.request_hover();
                }
                KeyCode::Char(' ') => {
                    editor.palette.visible = true;
                    editor.palette.query.clear();
                    editor.palette.selected_idx = 0;
                    editor.palette.scroll = 0;
                }
                KeyCode::Char('v') => {
                    editor.set_mode(Mode::Visual {
                        anchor_x: editor.cursor_x,
                        anchor_y: editor.cursor_y,
                    });
                }
                KeyCode::Char('%') => editor.select_all(),
                KeyCode::Char('y') => editor.yank_selection(),
                KeyCode::Char('p') => editor.paste(),
                KeyCode::Char('~') => editor.toggle_case(),
                KeyCode::Char('J') => editor.join_lines(),
                KeyCode::Char('o') => editor.insert_line_below(),
                KeyCode::Char('O') => editor.insert_line_above(),
                KeyCode::Char('i') => {
                    editor.set_mode(Mode::Insert);
                }
                KeyCode::Char('I') => {
                    editor.cursor_x = 0;
                    editor.set_mode(Mode::Insert);
                }
                KeyCode::Char('a') => {
                    let line_len = editor.current_line_len();
                    if editor.cursor_x < line_len {
                        editor.cursor_x += 1;
                    }
                    editor.set_mode(Mode::Insert);
                }
                KeyCode::Char('A') => {
                    editor.cursor_x = editor.current_line_len();
                    editor.set_mode(Mode::Insert);
                }
                KeyCode::Char('u') => editor.undo(),
                KeyCode::Char('d') => editor.pending_key = Some('d'),
                KeyCode::Char('g') => editor.pending_key = Some('g'),
                KeyCode::Char(']') => editor.pending_key = Some(']'),
                KeyCode::Char('[') => editor.pending_key = Some('['),
                KeyCode::Char('G') => {
                    let max_lines = editor.rope.len_lines().max(1);
                    editor.cursor_y = max_lines - 1;
                    editor.cursor_x = 0;
                }
                KeyCode::Char(':') => {
                    editor.set_mode(Mode::Command);
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
        Mode::Visual { .. } => {
            if let Some(pending) = editor.pending_key.take() {
                let handled = match (pending, key.code) {
                    ('g', KeyCode::Char('g')) => {
                        editor.cursor_y = 0;
                        editor.cursor_x = 0;
                        editor.scroll_y = 0;
                        editor.scroll_x = 0;
                        true
                    }
                    ('g', KeyCode::Char('e')) => {
                        editor.cursor_y = editor.rope.len_lines().saturating_sub(1);
                        editor.cursor_x = editor.current_line_len();
                        true
                    }
                    _ => false,
                };
                if handled {
                    editor.clamp_cursor();
                    return;
                }
            }

            match key.code {
                KeyCode::Esc | KeyCode::Char('v') => {
                    editor.set_mode(Mode::Normal);
                }
                KeyCode::Char('d' | 'x') => {
                    editor.delete_selection();
                }
                KeyCode::Char('c') => {
                    editor.delete_selection();
                    editor.set_mode(Mode::Insert);
                }
                KeyCode::Char('y') => {
                    editor.yank_selection();
                }
                KeyCode::Char('p') => {
                    editor.paste();
                }
                KeyCode::Char('~') => {
                    editor.toggle_case();
                }
                KeyCode::Char('%') => {
                    editor.select_all();
                }
                KeyCode::Char('0') => {
                    editor.cursor_x = 0;
                }
                KeyCode::Char('$') => {
                    editor.cursor_x = editor.current_line_len();
                }
                KeyCode::Char('g') => {
                    editor.pending_key = Some('g');
                }
                KeyCode::Char('G') => {
                    let max_lines = editor.rope.len_lines().max(1);
                    editor.cursor_y = max_lines - 1;
                    editor.cursor_x = editor.current_line_len();
                }
                KeyCode::Char(' ') => {
                    editor.palette.visible = true;
                    editor.palette.query.clear();
                    editor.palette.selected_idx = 0;
                    editor.palette.scroll = 0;
                }
                KeyCode::Char(':') => {
                    editor.set_mode(Mode::Command);
                    editor.command_buffer.clear();
                }
                KeyCode::Char('h') | KeyCode::Left => {
                    editor.cursor_x = editor.cursor_x.saturating_sub(1);
                }
                KeyCode::Char('l') | KeyCode::Right => {
                    let max = editor.current_line_len();
                    if editor.cursor_x < max {
                        editor.cursor_x += 1;
                    }
                }
                KeyCode::Char('k') | KeyCode::Up => {
                    editor.cursor_y = editor.cursor_y.saturating_sub(1);
                }
                KeyCode::Char('j') | KeyCode::Down
                    if editor.cursor_y + 1 < editor.rope.len_lines() =>
                {
                    editor.cursor_y += 1;
                }
                _ => {}
            }
        }
        Mode::Insert => {
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
                            editor.completions.len().saturating_sub(1)
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
                    editor.set_mode(Mode::Normal);
                    editor.completion_visible = false;
                    editor.signature_help = None;
                    if editor.cursor_x > 0 && editor.cursor_x >= editor.current_line_len() {
                        editor.cursor_x = editor.cursor_x.saturating_sub(1);
                    }
                    editor.clamp_cursor();
                }
                KeyCode::Enter => editor.insert_newline(),
                KeyCode::Backspace => editor.backspace(),
                KeyCode::BackTab => {
                    editor.dedent_current_line();
                    editor.completion_visible = false;
                }
                KeyCode::Tab => {
                    for _ in 0..4 {
                        editor.insert_char(' ');
                    }
                    editor.completion_visible = false;
                }
                KeyCode::Char(' ') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    editor.request_completions();
                }
                KeyCode::Char('(') => {
                    editor.insert_pair('(', ')');
                    editor.completion_visible = false;
                    editor.request_signature_help();
                }
                KeyCode::Char(',') => {
                    editor.insert_char(',');
                    editor.request_signature_help();
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
                KeyCode::Char(')') if editor.char_under_cursor() == Some(')') => {
                    editor.cursor_x += 1;
                    editor.completion_visible = false;
                    editor.signature_help = None;
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
                    editor.set_mode(Mode::Normal);
                    editor.command_buffer.clear();
                }
                KeyCode::Enter => {
                    editor.execute_command();
                    if editor.mode == Mode::Command {
                        editor.set_mode(Mode::Normal);
                    }
                }
                KeyCode::Backspace => {
                    if editor.command_buffer.pop().is_none() {
                        editor.set_mode(Mode::Normal);
                    }
                }
                KeyCode::Char('w') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    let trimmed = editor.command_buffer.trim_end();
                    if let Some(pos) = trimmed.rfind(' ') {
                        editor.command_buffer.truncate(pos + 1);
                    } else {
                        editor.command_buffer.clear();
                    }
                }
                KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    editor.command_buffer.clear();
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
