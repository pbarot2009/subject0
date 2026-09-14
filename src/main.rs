mod editor;
mod lsp;

use std::{
    cmp::Ordering,
    env,
    io::{stdout, Write},
    path::PathBuf,
    time::Duration,
};

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

use editor::{line_len, Editor, Focus, Mode};
use lsp::{
    completion_kind_icon, file_icon_and_color, run_lsp_actor, LspInbound, LspOutbound, LspStatus,
    SuggestionItem,
};

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

                        if !filtered.is_empty() {
                            editor.completions = filtered;
                            editor.completion_idx = 0;
                            editor.completion_scroll = 0;
                            editor.completion_visible = true;
                        } else {
                            editor.completion_visible = false;
                        }
                    }
                }
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
// Touch & Mouse Handling
// -----------------------------------------------------------------------------

fn handle_mouse_event(editor: &mut Editor, mouse: MouseEvent, size: Size) {
    let status_row = size.height.saturating_sub(2);
    let cmd_row = size.height.saturating_sub(1);
    let viewport_top = 1u16;
    let viewport_bottom = size.height.saturating_sub(3);

    // 1. Command Palette Touch Events
    if editor.palette.visible {
        if let MouseEventKind::Down(MouseButton::Left) = mouse.kind {
            let width = 46u16.min(size.width.saturating_sub(2));
            let height = 12u16.min(size.height.saturating_sub(2));
            let x = (size.width.saturating_sub(width)) / 2;
            let y = 1u16;

            if mouse.column >= x && mouse.column < x + width && mouse.row >= y + 2 && mouse.row < y + height - 1 {
                let clicked_row = (mouse.row - (y + 2)) as usize;
                let cmds = editor.palette.filtered_commands();
                let actual_idx = editor.palette.scroll + clicked_row;
                if actual_idx < cmds.len() {
                    let cmd_id = cmds[actual_idx].id;
                    editor.execute_palette_command(cmd_id);
                }
                return;
            } else if mouse.column < x || mouse.column >= x + width || mouse.row < y || mouse.row >= y + height {
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
                editor.mode = match editor.mode {
                    Mode::Normal => Mode::Insert,
                    Mode::Insert => Mode::Normal,
                    Mode::Command | Mode::Visual { .. } => Mode::Normal,
                };
                set_terminal_cursor_style(editor.mode);
                editor.completion_visible = false;
            } else if mouse.column <= 18 {
                // Sidebar Toggle
                editor.explorer.visible = !editor.explorer.visible;
                if editor.explorer.visible {
                    editor.explorer.refresh();
                    editor.focus = Focus::Explorer;
                } else {
                    editor.focus = Focus::Editor;
                }
            } else if mouse.column <= 27 {
                // Line Wrap Toggle
                editor.line_wrap = !editor.line_wrap;
                editor.status_msg = format!("Line Wrap: {}", if editor.line_wrap { "ON" } else { "OFF" });
            } else if mouse.column <= 36 {
                // Command Palette
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
            MouseEventKind::ScrollUp => {
                if editor.explorer.selected_idx > 0 {
                    editor.explorer.selected_idx -= 1;
                    editor.explorer.update_scroll(max_visible);
                }
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
    let text_area_width = (size.width as usize).saturating_sub(content_left as usize).max(1);

    match mouse.kind {
        MouseEventKind::Down(MouseButton::Left) | MouseEventKind::Drag(MouseButton::Left) => {
            if mouse.row >= viewport_top && mouse.row < viewport_bottom {
                let clicked_screen_row = (mouse.row - viewport_top) as usize;

                if !editor.line_wrap {
                    let target_line = (editor.scroll_y + clicked_screen_row).min(editor.rope.len_lines().saturating_sub(1));
                    editor.cursor_y = target_line;
                    if mouse.column >= content_left {
                        editor.cursor_x = editor.scroll_x + (mouse.column - content_left) as usize;
                    } else {
                        editor.cursor_x = 0;
                    }
                } else {
                    // Walk wrapped rows to find exact line and column
                    let mut accumulated_rows = 0;
                    let mut found_line = editor.rope.len_lines().saturating_sub(1);
                    let mut found_col = 0;

                    for y in editor.scroll_y..editor.rope.len_lines() {
                        let l_len = line_len(&editor.rope, y);
                        let sub_rows = if l_len == 0 { 1 } else { (l_len + text_area_width - 1) / text_area_width };

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
    // Filter release events to prevent double-firing in terminals reporting key releases
    if key.kind == KeyEventKind::Release {
        return;
    }

    let prev_mode = editor.mode;
    let max_visible = 6usize;

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
            if let Some(pending) = editor.pending_key.take() {
                match (pending, key.code) {
                    ('d', KeyCode::Char('d')) => editor.delete_current_line(),
                    ('g', KeyCode::Char('g')) => {
                        editor.cursor_y = 0;
                        editor.cursor_x = 0;
                    }
                    ('g', KeyCode::Char('h')) => editor.cursor_x = 0,
                    ('g', KeyCode::Char('l')) => editor.cursor_x = editor.current_line_len().saturating_sub(1),
                    ('g', KeyCode::Char('e')) => {
                        editor.cursor_y = editor.rope.len_lines().saturating_sub(1);
                        editor.cursor_x = 0;
                    }
                    _ => {}
                }
                editor.clamp_cursor();
                return;
            }

            match key.code {
                KeyCode::Char(' ') => {
                    // Open Helix Space Menu / Command Palette
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
                _ => {}
            }
        }
        Mode::Visual { .. } => {
            match key.code {
                KeyCode::Esc => {
                    editor.mode = Mode::Normal;
                }
                KeyCode::Char('d') | KeyCode::Char('x') => {
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
                KeyCode::Char('j') | KeyCode::Down => {
                    if editor.cursor_y + 1 < editor.rope.len_lines() {
                        editor.cursor_y += 1;
                    }
                }
                _ => {}
            }
        }
        Mode::Insert => {
            if editor.completion_visible && !editor.completions.is_empty() {
                match key.code {
                    KeyCode::Down => {
                        editor.completion_idx = (editor.completion_idx + 1) % editor.completions.len();
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
                    if !editor.completions.is_empty() {
                        editor.completion_visible = true;
                        editor.completion_idx = 0;
                        editor.completion_scroll = 0;
                    } else {
                        for _ in 0..4 {
                            editor.insert_char(' ');
                        }
                    }
                }
                KeyCode::Char(' ') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    editor.request_completions();
                }
                KeyCode::Char('(') => {
                    editor.insert_pair('(', ')');
                    editor.request_completions();
                }
                KeyCode::Char('[') => {
                    editor.insert_pair('[', ']');
                    editor.request_completions();
                }
                KeyCode::Char('{') => {
                    editor.insert_pair('{', '}');
                    editor.request_completions();
                }
                KeyCode::Char('"') => {
                    if editor.char_under_cursor() == Some('"') {
                        editor.cursor_x += 1;
                    } else {
                        editor.insert_pair('"', '"');
                    }
                }
                KeyCode::Char('\'') => {
                    if editor.char_under_cursor() == Some('\'') {
                        editor.cursor_x += 1;
                    } else {
                        editor.insert_pair('\'', '\'');
                    }
                }
                KeyCode::Char(')') if editor.char_under_cursor() == Some(')') => {
                    editor.cursor_x += 1;
                }
                KeyCode::Char(']') if editor.char_under_cursor() == Some(']') => {
                    editor.cursor_x += 1;
                }
                KeyCode::Char('}') if editor.char_under_cursor() == Some('}') => {
                    editor.cursor_x += 1;
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
                    if c.is_alphanumeric() || c == '_' || c == '.' || c == ':' {
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

// -----------------------------------------------------------------------------
// UI Render Pipeline
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

    let (explorer_area, editor_area) = if editor.explorer.visible {
        let is_mobile = size.width < 70;
        let sidebar_width = if is_mobile {
            (size.width * 7 / 10).max(26).min(size.width)
        } else {
            26u16
        };

        let h_chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Length(sidebar_width),
                Constraint::Min(10),
            ])
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
            .title(Line::from(vec![
                Span::styled(" 󰉓 Files ", Style::default().fg(Color::Rgb(220, 225, 235)).add_modifier(Modifier::BOLD)),
            ]));

        let inner_exp = exp_block.inner(exp_rect);
        frame.render_widget(exp_block, exp_rect);

        editor.explorer.update_scroll(inner_exp.height as usize);

        let mut tree_lines = Vec::new();
        let scroll_start = editor.explorer.scroll;
        let scroll_end = (scroll_start + inner_exp.height as usize).min(editor.explorer.entries.len());

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

    let editor_block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(if editor.focus == Focus::Editor { Color::Rgb(65, 70, 85) } else { Color::Rgb(45, 50, 60) }))
        .title(window_title);

    let inner_area = editor_block.inner(editor_area);
    frame.render_widget(editor_block, editor_area);

    let total_lines = editor.rope.len_lines().max(1);
    let line_digits = total_lines.to_string().len().max(2);
    let gutter_width = line_digits + 4;
    let text_area_width = (inner_area.width as usize).saturating_sub(gutter_width).max(1);

    editor.update_scroll(text_area_width, inner_area.height as usize);

    // Build visible lines: Syntax-highlighted character arrays
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

        // Tokenize through SyntaxEngine preserving Tree-sitter colors
        let syntax_spans = editor.syntax.highlight_line(&line_str, y);
        let mut char_styles: Vec<(char, Style)> = Vec::with_capacity(line_str.len());
        for span in syntax_spans {
            let st = span.style;
            for ch in span.content.chars() {
                char_styles.push((ch, st));
            }
        }

        if editor.line_wrap && char_styles.len() > text_area_width {
            let total_chars = char_styles.len();
            let mut chunk_start = 0;
            let mut is_first_sub = true;

            while chunk_start < total_chars && (current_row as usize) < inner_area.height as usize {
                let chunk_end = (chunk_start + text_area_width).min(total_chars);
                let (marker, gutter_str, g_style) = if is_first_sub {
                    (diag_marker, format!("{:>width$} │ ", y + 1, width = line_digits), gutter_style)
                } else {
                    (" ", format!("{:>width$} ↳ ", "", width = line_digits), Style::default().fg(Color::Rgb(65, 70, 85)))
                };

                let mut sub_spans = vec![
                    Span::styled(marker, if is_first_sub { diag_style } else { Style::default() }),
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
                    editor.cursor_x >= chunk_start && editor.cursor_x <= chunk_end
                } else {
                    editor.cursor_x >= chunk_start && editor.cursor_x < chunk_end
                };

                if is_cursor_line && in_chunk && cursor_screen_pos.is_none() {
                    let cx = inner_area.x + gutter_width as u16 + (editor.cursor_x - chunk_start) as u16;
                    let cy = inner_area.y + current_row;
                    cursor_screen_pos = Some((cx, cy));
                }

                visible_lines.push(Line::from(sub_spans));
                current_row += 1;
                chunk_start = chunk_end;
                is_first_sub = false;
            }
        } else {
            let (marker, gutter_str, g_style) = (diag_marker, format!("{:>width$} │ ", y + 1, width = line_digits), gutter_style);
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

                for (col_idx, (ch, mut st)) in char_styles.into_iter().enumerate().skip(skip_count).take(take_count) {
                    if editor.is_char_selected(y, col_idx) {
                        st = st.bg(Color::Rgb(55, 80, 145)).fg(Color::White);
                    }
                    row_spans.push(Span::styled(ch.to_string(), st));
                }

                if is_cursor_line && cursor_screen_pos.is_none() {
                    let visible_x = editor.cursor_x.saturating_sub(skip_count);
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
    let bar_fg = Color::Rgb(200, 205, 220);

    let error_count = editor.diagnostics.iter().filter(|d| d.severity == 1).count();
    let warn_count = editor.diagnostics.iter().filter(|d| d.severity == 2).count();

    let sidebar_toggle_badge = if editor.explorer.visible { " 󰉓 Files " } else { " 󰉒 Files " };
    let wrap_badge = if editor.line_wrap { " 󰖶 Wrap " } else { " 󰖵 NoWrap " };

    let status_left = Line::from(vec![
        Span::styled(
            badge_text,
            Style::default().bg(badge_color).fg(Color::Rgb(15, 17, 22)).add_modifier(Modifier::BOLD),
        ),
        Span::styled("", Style::default().bg(pill_bg).fg(badge_color)),
        Span::styled(
            sidebar_toggle_badge,
            Style::default().bg(pill_bg).fg(if editor.explorer.visible { Color::Rgb(100, 180, 255) } else { bar_fg }),
        ),
        Span::styled(
            wrap_badge,
            Style::default().bg(pill_bg).fg(if editor.line_wrap { Color::Rgb(100, 200, 140) } else { Color::Rgb(140, 145, 160) }),
        ),
        Span::styled(
            " 󰍉 Cmd ",
            Style::default().bg(pill_bg).fg(Color::Rgb(240, 180, 80)),
        ),
        Span::styled("", Style::default().bg(bar_bg).fg(pill_bg)),
    ]);

    let mut diag_indicators = Vec::new();
    if error_count > 0 {
        diag_indicators.push(Span::styled(format!("  {} ", error_count), Style::default().bg(bar_bg).fg(Color::Rgb(240, 90, 90))));
    }
    if warn_count > 0 {
        diag_indicators.push(Span::styled(format!(" {} ", warn_count), Style::default().bg(bar_bg).fg(Color::Rgb(245, 185, 60))));
    }
    if error_count == 0 && warn_count == 0 {
        if let LspStatus::Ready(name) = &editor.lsp_status {
            diag_indicators.push(Span::styled(format!(" 󰄬 {} ", name), Style::default().bg(bar_bg).fg(Color::Rgb(100, 180, 120))));
        }
    }

    let mut status_right_spans = diag_indicators;
    status_right_spans.extend(vec![
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
        Paragraph::new(Line::from(status_right_spans)).alignment(ratatui::layout::Alignment::Right),
        main_chunks[1],
    );

    // 4. Diagnostics / Bottom Notification Area
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
            let (d_icon, icon_style) = match diag.severity {
                1 => ("  ", Style::default().fg(Color::Rgb(240, 90, 90))),
                2 => ("  ", Style::default().fg(Color::Rgb(245, 185, 60))),
                _ => (" 󰌵 ", Style::default().fg(Color::Rgb(100, 180, 255))),
            };
            Line::from(vec![
                Span::styled(d_icon, icon_style),
                Span::styled(&diag.message, Style::default().fg(Color::Rgb(230, 235, 245)).add_modifier(Modifier::ITALIC)),
            ])
        } else {
            Line::from(vec![
                Span::styled(" 󰅂 ", Style::default().fg(Color::DarkGray)),
                Span::styled(&editor.status_msg, Style::default().fg(Color::Rgb(170, 175, 190))),
            ])
        };

        frame.render_widget(Paragraph::new(msg_line), main_chunks[2]);

        // Place terminal cursor
        let screen_x = inner_area.x + gutter_width as u16 + (editor.cursor_x.saturating_sub(editor.scroll_x)) as u16;
        let screen_y = inner_area.y + (editor.cursor_y.saturating_sub(editor.scroll_y)) as u16;

        // Floating Auto-Complete Dropdown
        if editor.mode == Mode::Insert && editor.completion_visible && !editor.completions.is_empty() {
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
                let item_bg = if is_sel { Color::Rgb(40, 75, 145) } else { Color::Rgb(25, 27, 34) };
                let text_style = if is_sel {
                    Style::default().bg(item_bg).fg(Color::White).add_modifier(Modifier::BOLD)
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
                .title(Line::from(Span::styled(title_info, Style::default().fg(Color::Rgb(140, 160, 200)))));

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
            Span::styled(" 󰍉 > ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::styled(&editor.palette.query, Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
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

            let row_bg = if is_sel { Color::Rgb(40, 75, 145) } else { Color::Rgb(25, 27, 34) };
            let title_style = if is_sel {
                Style::default().bg(row_bg).fg(Color::White).add_modifier(Modifier::BOLD)
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
                Span::styled(cmd.icon, Style::default().bg(row_bg).fg(Color::Rgb(100, 180, 255))),
                Span::styled(" ", Style::default().bg(row_bg)),
                Span::styled(display_title, title_style),
                Span::styled(" ".repeat(padding), Style::default().bg(row_bg)),
                Span::styled(format!(" {} ", cmd.shortcut), Style::default().bg(row_bg).fg(Color::Rgb(140, 145, 160))),
            ]));
        }

        let p_block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(Color::Rgb(80, 140, 255)))
            .style(Style::default().bg(Color::Rgb(25, 27, 34)))
            .title(Line::from(Span::styled(" 󰍉 Command Palette (Esc to close) ", Style::default().fg(Color::Rgb(180, 200, 240)).add_modifier(Modifier::BOLD))));

        frame.render_widget(Paragraph::new(palette_lines).block(p_block), palette_rect);
    }
}
