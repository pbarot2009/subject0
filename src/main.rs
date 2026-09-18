//! # Application Entry Point, Event Loop, & Terminal UI Subsystem
//!
//! This module serves as the runtime orchestrator for `subject0`. It integrates the
//! terminal lifecycle, asynchronous event multiplexing, input decoding, themed frame
//! rendering, and modal subsystem coordination[span_0](start_span)[span_0](end_span).

mod cmd;
mod editor;
mod lsp;
mod theme;

use std::{
    cmp::Ordering,
    io::{stdout, Write},
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
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Position, Rect, Size},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Paragraph},
    Frame, Terminal,
};
use tokio::sync::mpsc;

use editor::{Editor, Focus, Mode};
use lsp::{completion_kind_icon, file_icon_and_color, LspOutbound, LspStatus, SuggestionItem};
use theme::Theme;

/// RAII Terminal Guard ensuring the host terminal is reliably restored
/// to canonical mode regardless of exit status[span_1](start_span)[span_1](end_span).
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

/// Configures the terminal hardware cursor geometry based on the active modal editing state[span_2](start_span)[span_2](end_span).
fn set_terminal_cursor_style(mode: Mode) {
    let mut stdout = stdout();
    match mode {
        Mode::Normal | Mode::Command | Mode::Visual { .. } => {
            let _ = stdout.write_all(b"\x1b[2 q"); // Steady Block[span_3](start_span)[span_3](end_span)
        }
        Mode::Insert => {
            let _ = stdout.write_all(b"\x1b[6 q"); // Steady Bar / I-Beam[span_4](start_span)[span_4](end_span)
        }
    }
    let _ = stdout.flush();
}

/// Registers a secondary panic hook ensuring screen recovery during thread unwinding[span_5](start_span)[span_5](end_span).
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

/// Safely truncates a string by Unicode scalar count without slicing mid-codepoint[span_6](start_span)[span_6](end_span).
fn safe_truncate(s: &str, max_chars: usize) -> String {
    if s.chars().count() > max_chars {
        let mut result: String = s.chars().take(max_chars.saturating_sub(1)).collect();
        result.push('…');
        result
    } else {
        s.to_string()
    }
}

/// Calculates visual column width of a line taking expanded tabs into account[span_7](start_span)[span_7](end_span).
fn visual_line_len(rope: &ropey::Rope, line_idx: usize) -> usize {
    if line_idx >= rope.len_lines() {
        return 0;
    }
    let line = rope.line(line_idx);
    let mut len = 0;
    for ch in line.chars() {
        if ch == '\n' || ch == '\r' {
            continue;
        }
        if ch == '\t' {
            len += 4;
        } else {
            len += 1;
        }
    }
    len
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

    let (lsp_out_tx, mut lsp_out_rx) = mpsc::unbounded_channel::<LspOutbound>();
    editor.lsp_out_tx = Some(lsp_out_tx.clone());

    if let Some(path) = &target_path {
        if path.is_file() {
            editor.ensure_lsp_for_file(path);
        }
    }

    set_terminal_cursor_style(editor.mode);

    let mut needs_redraw = true;

    while !editor.should_quit {
        let mut received_lsp_msg = false;

        while let Ok(msg) = lsp_out_rx.try_recv() {
            received_lsp_msg = true;
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

        if received_lsp_msg {
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

    // 1. Intercept Help Modal[span_8](start_span)[span_8](end_span)
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

    // 2. Intercept Theme Picker Modal
    if let Some(picker) = &editor.theme_picker {
        if let MouseEventKind::Down(MouseButton::Left) = mouse.kind {
            let themes = Theme::all();
            let width = 48u16.min(size.width.saturating_sub(2));
            let height = ((themes.len() as u16) + 4).min(size.height.saturating_sub(2));
            let x = (size.width.saturating_sub(width)) / 2;
            let y = (size.height.saturating_sub(height)) / 2;

            if mouse.column >= x + 1
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

    // 3. Intercept LSP Server Picker Modal[span_9](start_span)[span_9](end_span)
    if let Some(picker) = &editor.lsp_picker {
        if let MouseEventKind::Down(MouseButton::Left) = mouse.kind {
            let width = 48u16.min(size.width.saturating_sub(2));
            let height = ((picker.candidates.len() as u16) + 4).min(size.height.saturating_sub(2));
            let x = (size.width.saturating_sub(width)) / 2;
            let y = (size.height.saturating_sub(height)) / 2;

            if mouse.column >= x + 1
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

    // 4. Intercept Command Palette Interactions[span_10](start_span)[span_10](end_span)
    if editor.palette.visible {
        if let MouseEventKind::Down(MouseButton::Left) = mouse.kind {
            let width = 46u16.min(size.width.saturating_sub(2));
            let height = 12u16.min(size.height.saturating_sub(2));
            let x = (size.width.saturating_sub(width)) / 2;
            let y = 1u16;

            if mouse.column >= x + 1
                && mouse.column < x + width - 1
                && mouse.row >= y + 3
                && mouse.row < y + height - 1
            {
                let clicked_row = (mouse.row - (y + 3)) as usize;
                let cmds = editor.palette.filtered_commands();
                let actual_idx = editor.palette.scroll + clicked_row;
                if actual_idx < cmds.len() {
                    let cmd_id = cmds[actual_idx].id;
                    editor.execute_palette_command(cmd_id);
                    set_terminal_cursor_style(editor.mode);
                }
                return;
            } else if mouse.column < x
                || mouse.column >= x + width
                || mouse.row < y
                || mouse.row >= y + height
            {
                editor.palette.visible = false;
                return;
            }
        }
        return;
    }

    // 5. Statusline Interactions[span_11](start_span)[span_11](end_span)
    if mouse.row == status_row {
        if let MouseEventKind::Down(MouseButton::Left) = mouse.kind {
            let badge_len = match editor.mode {
                Mode::Normal => 8,
                Mode::Insert => 8,
                Mode::Command => 9,
                Mode::Visual { .. } => 8,
            } + 1;

            let files_end = badge_len + 10;
            let wrap_end = files_end + 10;
            let cmd_end = wrap_end + 9;

            let col = mouse.column as usize;
            if col <= badge_len {
                editor.set_mode(match editor.mode {
                    Mode::Normal => Mode::Insert,
                    Mode::Insert | Mode::Command | Mode::Visual { .. } => Mode::Normal,
                });
                set_terminal_cursor_style(editor.mode);
                editor.completion_visible = false;
            } else if col <= files_end {
                editor.explorer.visible = !editor.explorer.visible;
                if editor.explorer.visible {
                    editor.explorer.refresh();
                    editor.focus = Focus::Explorer;
                } else {
                    editor.focus = Focus::Editor;
                }
            } else if col <= wrap_end {
                editor.line_wrap = !editor.line_wrap;
                editor.status_msg =
                    format!("Line Wrap: {}", if editor.line_wrap { "ON" } else { "OFF" });
            } else if col <= cmd_end {
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

    let explorer_width = if editor.explorer.visible {
        if size.width < 70 {
            (size.width * 7 / 10).max(26).min(size.width)
        } else {
            26u16
        }
    } else {
        0u16
    };

    // 6. File Explorer Sidebar Interactions[span_12](start_span)[span_12](end_span)
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

    // 7. Completion Dropdown Interactions[span_13](start_span)[span_13](end_span)
    if editor.completion_visible && !editor.completions.is_empty() {
        if let Some((px, py, pw, ph)) = editor.completion_rect {
            let in_popup = mouse.column >= px
                && mouse.column < px + pw
                && mouse.row >= py
                && mouse.row < py + ph;

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
                        if mouse.row >= py + 1 && mouse.row < py + ph - 1 {
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
    }

    // 8. Document Viewport Buffer Interactions[span_14](start_span)[span_14](end_span)
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
                    let mut accumulated_rows = 0;
                    let mut found_line = editor.rope.len_lines().saturating_sub(1);
                    let mut found_col = 0;

                    for y in editor.scroll_y..editor.rope.len_lines() {
                        let l_len = visual_line_len(&editor.rope, y);
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
                            let raw_line = editor.rope.line(y);
                            let target_visual_x = sub_idx * text_area_width + sub_col;
                            let mut current_vx = 0;
                            let mut resolved_char_idx = 0;
                            for (c_idx, ch) in raw_line.chars().enumerate() {
                                if current_vx >= target_visual_x || ch == '\n' || ch == '\r' {
                                    break;
                                }
                                current_vx += if ch == '\t' { 4 } else { 1 };
                                resolved_char_idx = c_idx + 1;
                            }
                            found_col = resolved_char_idx;
                            break;
                        }
                        accumulated_rows += sub_rows;
                    }

                    editor.cursor_y = found_line;
                    editor.cursor_x = found_col;
                } else {
                    let target_line = (editor.scroll_y + clicked_screen_row)
                        .min(editor.rope.len_lines().saturating_sub(1));
                    editor.cursor_y = target_line;
                    if mouse.column >= content_left {
                        let target_visual_x =
                            editor.scroll_x + (mouse.column - content_left) as usize;
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
            editor.completion_visible = false;
        }
        MouseEventKind::ScrollDown if editor.scroll_y + 3 < editor.rope.len_lines() => {
            editor.scroll_y += 3;
            editor.cursor_y = (editor.cursor_y + 3).min(editor.rope.len_lines().saturating_sub(1));
            editor.clamp_cursor();
            editor.completion_visible = false;
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

    // 1. Intercept In-Editor Help Modal Navigation[span_15](start_span)[span_15](end_span)
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

    // 2. Intercept Theme Picker Modal
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

    // 3. Intercept LSP Server Selection Modal[span_16](start_span)[span_16](end_span)
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

    // 4. Intercept Command Palette Key Events[span_17](start_span)[span_17](end_span)
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
            KeyCode::Char(c) => {
                editor.palette.query.push(c);
                editor.palette.selected_idx = 0;
            }
            _ => {}
        }
        return;
    }

    // 5. Global Shortcuts: Ctrl-E for File Explorer[span_18](start_span)[span_18](end_span)
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

    // 6. File Explorer Navigation Focus[span_19](start_span)[span_19](end_span)
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

    // 7. Modal Editing Handler[span_20](start_span)[span_20](end_span)
    match editor.mode {
        Mode::Normal => {
            editor.completion_visible = false;

            // Global Normal Mode Hotkeys
            if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('r') {
                editor.redo();
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
            }

            match key.code {
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
                KeyCode::Char('G') => {
                    editor.cursor_y = editor.rope.len_lines().saturating_sub(1);
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
        Mode::Visual { .. } => match key.code {
            KeyCode::Esc => {
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

// === UI Render Pipeline ===

fn render_ui(frame: &mut Frame, editor: &mut Editor) {
    let size = frame.area();
    let theme = editor.theme;

    let main_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(3),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .split(size);

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

    // 1. Render File Explorer (Sidebar)[span_21](start_span)[span_21](end_span)
    if let Some(exp_rect) = explorer_area {
        let is_focused = editor.focus == Focus::Explorer;
        let border_color = if is_focused {
            theme.border_focused
        } else {
            theme.border
        };

        let exp_block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(border_color))
            .style(Style::default().bg(theme.explorer_bg))
            .title(Line::from(vec![Span::styled(
                " 󰉓 Files ",
                Style::default()
                    .fg(theme.explorer_fg)
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
                    (" ", theme.explorer_dir_expanded)
                } else {
                    (" ", theme.explorer_dir)
                }
            } else {
                file_icon_and_color(Some(&entry.path))
            };

            let item_style = if is_sel {
                Style::default()
                    .bg(theme.explorer_sel_bg)
                    .fg(theme.explorer_sel_fg)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme.explorer_fg)
            };

            tree_lines.push(Line::from(vec![
                Span::raw(indent),
                Span::styled(icon, Style::default().fg(icon_color)),
                Span::styled(format!(" {}", entry.name), item_style),
            ]));
        }

        frame.render_widget(Paragraph::new(tree_lines), inner_exp);
    }

    // 2. Render Document Editor Viewport[span_22](start_span)[span_22](end_span)
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
            Style::default().fg(theme.fg).add_modifier(Modifier::BOLD),
        ),
        if editor.modified {
            Span::styled(" ●", Style::default().fg(Color::Rgb(244, 56, 65)))
        } else {
            Span::raw("")
        },
        Span::raw(" "),
    ]);

    let editor_block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(if editor.focus == Focus::Editor {
            theme.border_focused
        } else {
            theme.border
        }))
        .style(Style::default().bg(theme.bg))
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
            Some(1) => ("", Style::default().fg(Color::Rgb(244, 56, 65))),
            Some(2) => ("", Style::default().fg(Color::Rgb(245, 185, 60))),
            Some(_) => ("󰌵", Style::default().fg(Color::Rgb(100, 180, 255))),
            None => (" ", Style::default()),
        };

        let gutter_style = if is_cursor_line {
            Style::default()
                .fg(theme.line_number_active)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.line_number)
        };

        let line = editor.rope.line(y);
        let mut line_str = line.to_string();
        if line_str.ends_with('\n') {
            line_str.pop();
            if line_str.ends_with('\r') {
                line_str.pop();
            }
        }

        let syntax_spans = editor.syntax.highlight_line(&line_str, y, &theme);
        let mut char_cells: Vec<(char, Style, usize)> = Vec::with_capacity(line_str.len() * 2);
        let mut char_idx = 0;
        for span in syntax_spans {
            let st = span.style;
            for ch in span.content.chars() {
                if ch == '\t' {
                    for _ in 0..4 {
                        char_cells.push((' ', st, char_idx));
                    }
                } else {
                    char_cells.push((ch, st, char_idx));
                }
                char_idx += 1;
            }
        }

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

        if editor.line_wrap && char_cells.len() > text_area_width {
            let total_chars = char_cells.len();
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
                        Style::default().fg(theme.line_number),
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
                    let (ch, mut st, orig_char_idx) = char_cells[col_idx];
                    if editor.is_char_selected(y, orig_char_idx) {
                        st = st.bg(theme.selection_bg).fg(theme.selection_fg);
                    } else if is_cursor_line {
                        st = st.bg(theme.cursor_line_bg);
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

            if char_cells.is_empty() {
                if is_cursor_line && cursor_screen_pos.is_none() {
                    let cx = inner_area.x + gutter_width as u16;
                    let cy = inner_area.y + current_row;
                    cursor_screen_pos = Some((cx, cy));
                }
            } else {
                let skip_count = if editor.line_wrap { 0 } else { editor.scroll_x };
                let take_count = text_area_width;

                for (_col_idx, (ch, mut st, orig_char_idx)) in char_cells
                    .into_iter()
                    .enumerate()
                    .skip(skip_count)
                    .take(take_count)
                {
                    if editor.is_char_selected(y, orig_char_idx) {
                        st = st.bg(theme.selection_bg).fg(theme.selection_fg);
                    } else if is_cursor_line {
                        st = st.bg(theme.cursor_line_bg);
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

    for _ in (current_row as usize)..inner_area.height as usize {
        visible_lines.push(Line::from(vec![
            Span::raw(" "),
            Span::styled(
                format!("{:>width$} │ ", "~", width = line_digits),
                Style::default().fg(theme.line_number),
            ),
        ]));
    }

    frame.render_widget(Paragraph::new(visible_lines), inner_area);

    // 3. Render Statusline[span_23](start_span)[span_23](end_span)
    let (badge_text, badge_color) = match editor.mode {
        Mode::Normal => (" NORMAL ", theme.mode_normal),
        Mode::Insert => (" INSERT ", theme.mode_insert),
        Mode::Command => (" COMMAND ", theme.mode_command),
        Mode::Visual { .. } => (" VISUAL ", theme.mode_visual),
    };

    let bar_bg = theme.status_bg;
    let pill_bg = theme.status_pill_bg;
    let bar_foreground = theme.status_fg;

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
                theme.mode_normal
            } else {
                bar_foreground
            }),
        ),
        Span::styled(
            wrap_badge,
            Style::default().bg(pill_bg).fg(if editor.line_wrap {
                theme.mode_insert
            } else {
                theme.line_number
            }),
        ),
        Span::styled(
            " 󰍉 Cmd ",
            Style::default().bg(pill_bg).fg(theme.mode_command),
        ),
        Span::styled("", Style::default().bg(bar_bg).fg(pill_bg)),
    ]);

    let spinner_icon = SPINNER[(editor.spinner_tick / 3) % SPINNER.len()];

    let mut status_right_spans = Vec::new();

    match &editor.lsp_status {
        LspStatus::Starting(name) => {
            status_right_spans.push(Span::styled(
                format!(" {spinner_icon} {name} "),
                Style::default()
                    .bg(bar_bg)
                    .fg(theme.mode_command)
                    .add_modifier(Modifier::BOLD),
            ));
        }
        LspStatus::Ready(name) => {
            status_right_spans.push(Span::styled(
                format!(" 󰄬 {name} "),
                Style::default().bg(bar_bg).fg(theme.mode_insert),
            ));
        }
        LspStatus::Error(err) => {
            status_right_spans.push(Span::styled(
                format!(" 󰅚 LSP: {err} "),
                Style::default()
                    .bg(bar_bg)
                    .fg(Color::Rgb(244, 56, 65))
                    .add_modifier(Modifier::BOLD),
            ));
        }
        LspStatus::NotFound(name) => {
            status_right_spans.push(Span::styled(
                format!(" 󰄰 {name} missing "),
                Style::default().bg(bar_bg).fg(theme.mode_command),
            ));
        }
        LspStatus::Disabled => {}
    }

    if error_count > 0 {
        status_right_spans.push(Span::styled(
            format!("  {error_count} "),
            Style::default().bg(bar_bg).fg(Color::Rgb(244, 56, 65)),
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
            format!(" 󰔎 {} ", theme.display_name),
            Style::default().bg(pill_bg).fg(bar_foreground),
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

    // 4. Diagnostics / Command Bar Area[span_24](start_span)[span_24](end_span)
    if editor.mode == Mode::Command {
        let prompt_line = Line::from(vec![
            Span::styled(
                " :",
                Style::default()
                    .fg(theme.mode_command)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(&editor.command_buffer),
        ]);
        frame.render_widget(Paragraph::new(prompt_line), main_chunks[2]);
        let cmd_chars = editor.command_buffer.chars().count();
        let target_x = (2 + cmd_chars).min((frame.area().width.saturating_sub(1)) as usize) as u16;
        frame.set_cursor_position(Position::new(target_x, main_chunks[2].y));
    } else {
        let active_diag = editor
            .diagnostics
            .iter()
            .find(|d| d.line == editor.cursor_y);
        let msg_line = if let Some(diag) = active_diag {
            let (d_icon, icon_style) = match diag.severity {
                1 => ("  ", Style::default().fg(Color::Rgb(244, 56, 65))),
                2 => ("  ", Style::default().fg(Color::Rgb(245, 185, 60))),
                _ => (" 󰌵 ", Style::default().fg(Color::Rgb(100, 180, 255))),
            };
            Line::from(vec![
                Span::styled(d_icon, icon_style),
                Span::styled(
                    &diag.message,
                    Style::default().fg(theme.fg).add_modifier(Modifier::ITALIC),
                ),
            ])
        } else {
            Line::from(vec![
                Span::styled(" 󰅂 ", Style::default().fg(theme.line_number)),
                Span::styled(&editor.status_msg, Style::default().fg(theme.status_fg)),
            ])
        };

        frame.render_widget(Paragraph::new(msg_line), main_chunks[2]);

        let (screen_x, screen_y) =
            cursor_screen_pos.unwrap_or((inner_area.x + gutter_width as u16, inner_area.y));

        if !(editor.mode == Mode::Insert
            && editor.completion_visible
            && !editor.completions.is_empty())
        {
            editor.completion_rect = None;
        }

        // Floating Auto-Complete Dropdown[span_25](start_span)[span_25](end_span)
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
            editor.completion_rect = Some((popup_x, popup_y, popup_width, popup_height));
            frame.render_widget(Clear, popup_rect);

            let scroll_start = editor.completion_scroll;
            let scroll_end = (scroll_start + max_visible_items).min(total_items);

            let mut list_lines = Vec::new();
            for i in scroll_start..scroll_end {
                let item = &editor.completions[i];
                let is_sel = i == editor.completion_idx;

                let (kind_icon, kind_color) = completion_kind_icon(item.kind);
                let item_bg = if is_sel {
                    theme.popup_sel_bg
                } else {
                    theme.popup_bg
                };
                let text_style = if is_sel {
                    Style::default()
                        .bg(item_bg)
                        .fg(theme.popup_sel_fg)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().bg(item_bg).fg(theme.popup_text)
                };

                let avail_width = (popup_width as usize).saturating_sub(6);
                let label_text = safe_truncate(&item.label, avail_width);
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
                .border_style(Style::default().fg(theme.popup_border))
                .style(Style::default().bg(theme.popup_bg))
                .title(Line::from(Span::styled(
                    title_info,
                    Style::default().fg(theme.status_fg),
                )));

            frame.render_widget(Paragraph::new(list_lines).block(comp_block), popup_rect);
        }

        if editor.focus == Focus::Editor {
            if let Some((cx, cy)) = cursor_screen_pos {
                if cx < inner_area.right() && cy < inner_area.bottom() {
                    frame.set_cursor_position(Position::new(cx, cy));
                }
            }
        }
    }

    // 5. Render Command Palette Modal[span_26](start_span)[span_26](end_span)
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
                    .fg(theme.mode_command)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                &editor.palette.query,
                Style::default()
                    .fg(theme.popup_sel_fg)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled("█", Style::default().fg(theme.border_focused)),
        ]));
        palette_lines.push(Line::from(Span::styled(
            "─".repeat((width as usize).saturating_sub(2)),
            Style::default().fg(theme.border),
        )));

        let scroll_start = editor.palette.scroll;
        let scroll_end = (scroll_start + max_visible).min(filtered.len());

        for i in scroll_start..scroll_end {
            let cmd = filtered[i];
            let is_sel = i == editor.palette.selected_idx;

            let row_bg = if is_sel {
                theme.popup_sel_bg
            } else {
                theme.popup_bg
            };
            let title_style = if is_sel {
                Style::default()
                    .bg(row_bg)
                    .fg(theme.popup_sel_fg)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().bg(row_bg).fg(theme.popup_text)
            };

            let avail_title_width = (width as usize).saturating_sub(cmd.shortcut.len() + 8);
            let display_title = safe_truncate(cmd.title, avail_title_width);
            let padding = avail_title_width.saturating_sub(display_title.chars().count());

            palette_lines.push(Line::from(vec![
                Span::styled(" ", Style::default().bg(row_bg)),
                Span::styled(cmd.icon, Style::default().bg(row_bg).fg(theme.mode_normal)),
                Span::styled(" ", Style::default().bg(row_bg)),
                Span::styled(display_title, title_style),
                Span::styled(" ".repeat(padding), Style::default().bg(row_bg)),
                Span::styled(
                    format!(" {} ", cmd.shortcut),
                    Style::default().bg(row_bg).fg(theme.line_number),
                ),
            ]));
        }

        let p_block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(theme.popup_border))
            .style(Style::default().bg(theme.popup_bg))
            .title(Line::from(Span::styled(
                " 󰍉 Command Palette (Esc to close) ",
                Style::default()
                    .fg(theme.popup_text)
                    .add_modifier(Modifier::BOLD),
            )));

        frame.render_widget(Paragraph::new(palette_lines).block(p_block), palette_rect);
    }

    // 6. Render Theme Picker Modal Overlay
    if let Some(picker) = &editor.theme_picker {
        let themes = Theme::all();
        let width = 48u16.min(size.width.saturating_sub(2));
        let height = ((themes.len() as u16) + 4).min(size.height.saturating_sub(2));
        let x = (size.width.saturating_sub(width)) / 2;
        let y = (size.height.saturating_sub(height)) / 2;

        let picker_rect = Rect::new(x, y, width, height);
        frame.render_widget(Clear, picker_rect);

        let mut lines = Vec::new();
        lines.push(Line::from(vec![
            Span::styled(" Select Theme ", Style::default().fg(theme.status_fg)),
            Span::styled("(Enter to apply):", Style::default().fg(theme.line_number)),
        ]));
        lines.push(Line::from(Span::styled(
            "─".repeat((width as usize).saturating_sub(2)),
            Style::default().fg(theme.border),
        )));

        for (idx, t) in themes.iter().enumerate() {
            let is_sel = idx == picker.selected_idx;
            let is_active = t.name == editor.theme.name;
            let bg = if is_sel {
                theme.popup_sel_bg
            } else {
                theme.popup_bg
            };
            let style = if is_sel {
                Style::default()
                    .bg(bg)
                    .fg(theme.popup_sel_fg)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().bg(bg).fg(theme.popup_text)
            };

            let mark = if is_active {
                " 󰄬 "
            } else if is_sel {
                " 󰅂 "
            } else {
                "   "
            };
            lines.push(Line::from(vec![
                Span::styled(mark, Style::default().bg(bg).fg(theme.mode_normal)),
                Span::styled(format!("{:<38}", t.display_name), style),
            ]));
        }

        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(theme.popup_border))
            .style(Style::default().bg(theme.popup_bg))
            .title(Line::from(Span::styled(
                " 󰔎 Color Themes ",
                Style::default()
                    .fg(theme.popup_text)
                    .add_modifier(Modifier::BOLD),
            )));

        frame.render_widget(Paragraph::new(lines).block(block), picker_rect);
    }

    // 7. Render LSP Picker Modal Overlay[span_27](start_span)[span_27](end_span)
    if let Some(picker) = &editor.lsp_picker {
        let width = 48u16.min(size.width.saturating_sub(2));
        let height = ((picker.candidates.len() as u16) + 4).min(size.height.saturating_sub(2));
        let x = (size.width.saturating_sub(width)) / 2;
        let y = (size.height.saturating_sub(height)) / 2;

        let picker_rect = Rect::new(x, y, width, height);
        frame.render_widget(Clear, picker_rect);

        let mut lines = Vec::new();
        lines.push(Line::from(vec![
            Span::styled(" Detected LSPs for ", Style::default().fg(theme.status_fg)),
            Span::styled(
                &picker.language_id,
                Style::default()
                    .fg(theme.mode_command)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                " (Select with Enter):",
                Style::default().fg(theme.status_fg),
            ),
        ]));
        lines.push(Line::from(Span::styled(
            "─".repeat((width as usize).saturating_sub(2)),
            Style::default().fg(theme.border),
        )));

        for (idx, candidate) in picker.candidates.iter().enumerate() {
            let is_sel = idx == picker.selected_idx;
            let bg = if is_sel {
                theme.popup_sel_bg
            } else {
                theme.popup_bg
            };
            let style = if is_sel {
                Style::default()
                    .bg(bg)
                    .fg(theme.popup_sel_fg)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().bg(bg).fg(theme.popup_text)
            };

            lines.push(Line::from(vec![
                Span::styled(
                    if is_sel { " 󰄬 " } else { "   " },
                    Style::default().bg(bg).fg(theme.mode_insert),
                ),
                Span::styled(format!("{candidate:<38}"), style),
            ]));
        }

        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(theme.popup_border))
            .style(Style::default().bg(theme.popup_bg))
            .title(Line::from(Span::styled(
                "  Choose Language Server ",
                Style::default()
                    .fg(theme.popup_text)
                    .add_modifier(Modifier::BOLD),
            )));

        frame.render_widget(Paragraph::new(lines).block(block), picker_rect);
    }

    // 8. Render In-Editor Help Modal[span_28](start_span)[span_28](end_span)
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
            .border_style(Style::default().fg(theme.popup_border))
            .style(Style::default().bg(theme.popup_bg))
            .title(Line::from(vec![
                Span::styled(
                    " 󰋖 subject0 Keybindings & Help ",
                    Style::default()
                        .fg(theme.mode_command)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled("(Esc to close) ", Style::default().fg(theme.line_number)),
            ]));

        let inner_rect = help_block.inner(help_rect);
        frame.render_widget(help_block, help_rect);

        let c_sec = Style::default()
            .fg(theme.mode_normal)
            .add_modifier(Modifier::BOLD);
        let c_key = Style::default()
            .fg(theme.mode_command)
            .add_modifier(Modifier::BOLD);
        let c_desc = Style::default().fg(theme.popup_text);

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
                Span::styled("  y, p           ", c_key),
                Span::styled("Yank line, Paste from clipboard", c_desc),
            ]),
            Line::from(vec![
                Span::styled("  u, Ctrl-R      ", c_key),
                Span::styled("Undo edit, Redo edit", c_desc),
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
                Span::styled("  Tab / Shift-Tab", c_key),
                Span::styled("Indent 4 spaces / Dedent line", c_desc),
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
                Span::styled("  :theme [name]  ", c_key),
                Span::styled("Open theme picker or switch theme", c_desc),
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
