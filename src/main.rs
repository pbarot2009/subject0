use std::{
    env,
    fs::File,
    io::{stdout, BufWriter, Write},
    path::PathBuf,
};

use anyhow::Result;
use crossterm::{
    event::{self, Event, KeyCode, KeyEvent, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Position, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Paragraph},
    Frame, Terminal,
};
use ropey::Rope;

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
            should_quit: false,
        })
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
    }

    pub fn insert_newline(&mut self) {
        let idx = self.char_index();
        self.rope.insert_char(idx, '\n');
        self.cursor_y += 1;
        self.cursor_x = 0;
        self.modified = true;
    }

    pub fn backspace(&mut self) {
        if self.cursor_x > 0 {
            let idx = self.char_index();
            self.rope.remove(idx - 1..idx);
            self.cursor_x -= 1;
            self.modified = true;
        } else if self.cursor_y > 0 {
            let prev_len = line_len(&self.rope, self.cursor_y - 1);
            let current_line_idx = self.rope.line_to_char(self.cursor_y);

            // Remove the preceding newline character
            if current_line_idx > 0 {
                self.rope.remove(current_line_idx - 1..current_line_idx);
            }

            self.cursor_y -= 1;
            self.cursor_x = prev_len;
            self.modified = true;
        }
    }

    pub fn delete_under_cursor(&mut self) {
        let line_len = self.current_line_len();
        if self.cursor_x < line_len {
            let idx = self.char_index();
            self.rope.remove(idx..idx + 1);
            self.modified = true;
        }
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
        } else {
            self.status_msg = "Error: No file name (use :w <filename>)".to_string();
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
        } else if cmd.starts_with("w ") {
            let name = cmd[2..].trim();
            self.path = Some(PathBuf::from(name));
            let _ = self.save();
        } else if cmd == "wq" {
            if self.save().is_ok() {
                self.should_quit = true;
            }
        } else if !cmd.is_empty() {
            self.status_msg = format!("Not an editor command: :{}", cmd);
        }
    }

    // Modal cursor boundary enforcement
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

// Adjusts hardware terminal caret style: block for normal, beam for insert
fn set_terminal_cursor_style(mode: Mode) {
    let mut stdout = stdout();
    match mode {
        Mode::Normal | Mode::Command => {
            let _ = stdout.write_all(b"\x1b[2 q"); // Steady block
        }
        Mode::Insert => {
            let _ = stdout.write_all(b"\x1b[6 q"); // Steady bar
        }
    }
    let _ = stdout.flush();
}

fn setup_panic_hook() {
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let mut out = stdout();
        let _ = out.write_all(b"\x1b[0 q"); // Reset cursor style
        let _ = out.flush();
        let _ = disable_raw_mode();
        let _ = execute!(out, LeaveAlternateScreen);
        hook(info);
    }));
}

fn main() -> Result<()> {
    setup_panic_hook();
    enable_raw_mode()?;
    let mut stdout = stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let target_path = env::args().nth(1).map(PathBuf::from);
    let mut editor = Editor::new(target_path)?;

    set_terminal_cursor_style(editor.mode);

    while !editor.should_quit {
        terminal.draw(|f| render_ui(f, &mut editor))?;

        if let Event::Key(key) = event::read()? {
            handle_key_event(&mut editor, key);
        }
    }

    let mut out = std::io::stdout();
    let _ = out.write_all(b"\x1b[0 q"); // Reset cursor style
    let _ = out.flush();
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    Ok(())
}

fn handle_key_event(editor: &mut Editor, key: KeyEvent) {
    let prev_mode = editor.mode;

    match editor.mode {
        Mode::Normal => match key.code {
            KeyCode::Char('i') => {
                editor.mode = Mode::Insert;
            }
            KeyCode::Char('a') => {
                let line_len = editor.current_line_len();
                if editor.cursor_x < line_len {
                    editor.cursor_x += 1;
                }
                editor.mode = Mode::Insert;
            }
            KeyCode::Char('o') => {
                editor.cursor_x = editor.current_line_len();
                editor.insert_newline();
                editor.mode = Mode::Insert;
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
            KeyCode::Char('0') => editor.cursor_x = 0,
            KeyCode::Char('$') => editor.cursor_x = editor.current_line_len().saturating_sub(1),
            _ => {}
        },
        Mode::Insert => match key.code {
            KeyCode::Esc => {
                editor.mode = Mode::Normal;
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
            KeyCode::Char(c) => {
                editor.command_buffer.push(c);
            }
            _ => {}
        },
    }

    if prev_mode != editor.mode {
        set_terminal_cursor_style(editor.mode);
    }

    editor.clamp_cursor();
}

fn render_ui(frame: &mut Frame, editor: &mut Editor) {
    let size = frame.area();

    // 3-tier layout: Viewport, Status line, Command/Message line
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),    // Text area
            Constraint::Length(1), // Unified status bar
            Constraint::Length(1), // Command prompt / notification
        ])
        .split(size);

    let viewport = chunks[0];

    // Dynamic gutter sizing based on total line count
    let total_lines = editor.rope.len_lines().max(1);
    let line_digits = total_lines.to_string().len().max(2);
    let gutter_width = line_digits + 3; // digits + " │ "
    let text_area_width = (viewport.width as usize).saturating_sub(gutter_width);

    editor.update_scroll(text_area_width, viewport.height as usize);

    // 1. Text Area Rendering
    let mut visible_lines = Vec::new();
    let start_line = editor.scroll_y;
    let end_line = (start_line + viewport.height as usize).min(editor.rope.len_lines());

    for y in start_line..end_line {
        let is_current = y == editor.cursor_y;
        let line_num_str = format!("{:>width$} │ ", y + 1, width = line_digits);

        let gutter_style = if is_current {
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::DarkGray)
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

        visible_lines.push(Line::from(vec![
            Span::styled(line_num_str, gutter_style),
            Span::raw(scrolled_line),
        ]));
    }

    // Fill viewport height with empty line markers (~) if buffer is short
    let rendered_lines = visible_lines.len();
    for _ in rendered_lines..viewport.height as usize {
        visible_lines.push(Line::from(vec![Span::styled(
            format!("{:>width$} │ ", "~", width = line_digits),
            Style::default().fg(Color::DarkGray),
        )]));
    }

    frame.render_widget(Paragraph::new(visible_lines), viewport);

    // 2. Status Bar Rendering (Seamless dark bar with rounded powerline badges)
    let (badge_text, badge_color) = match editor.mode {
        Mode::Normal => (" NORMAL ", Color::Rgb(80, 140, 255)),
        Mode::Insert => (" INSERT ", Color::Rgb(70, 200, 120)),
        Mode::Command => (" COMMAND ", Color::Rgb(220, 100, 240)),
    };

    let bar_bg = Color::Rgb(30, 32, 40);
    let bar_fg = Color::Rgb(200, 205, 215);

    let file_name = editor
        .path
        .as_ref()
        .map(|p| {
            p.file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string()
        })
        .unwrap_or_else(|| "[No Name]".into());

    let mod_indicator = if editor.modified { " ●" } else { "" };
    let position_str = format!("Ln {}, Col {} ", editor.cursor_y + 1, editor.cursor_x + 1);

    // Left badges + rounded separators
    let status_left = Line::from(vec![
        Span::styled(
            badge_text,
            Style::default()
                .bg(badge_color)
                .fg(Color::Black)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" ", Style::default().bg(bar_bg).fg(badge_color)),
        Span::styled(
            format!(" {}{}", file_name, mod_indicator),
            Style::default().bg(bar_bg).fg(bar_fg),
        ),
    ]);

    let status_right = Line::from(vec![
        Span::styled("", Style::default().bg(bar_bg).fg(Color::Rgb(45, 48, 60))),
        Span::styled(
            format!(" {}", position_str),
            Style::default()
                .bg(Color::Rgb(45, 48, 60))
                .fg(bar_fg)
                .add_modifier(Modifier::BOLD),
        ),
    ]);

    // Render full background fill
    frame.render_widget(
        Block::default().style(Style::default().bg(bar_bg)),
        chunks[1],
    );
    frame.render_widget(Paragraph::new(status_left), chunks[1]);
    frame.render_widget(
        Paragraph::new(status_right).alignment(ratatui::layout::Alignment::Right),
        chunks[1],
    );

    // 3. Command / Notification Bar
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
        frame.render_widget(Paragraph::new(prompt_line), chunks[2]);
        frame.set_cursor_position(Position::new(
            (2 + editor.command_buffer.len()) as u16,
            chunks[2].y,
        ));
    } else {
        let msg_line = Line::from(vec![
            Span::styled(" 󰅂 ", Style::default().fg(Color::DarkGray)),
            Span::styled(&editor.status_msg, Style::default().fg(Color::Gray)),
        ]);
        frame.render_widget(Paragraph::new(msg_line), chunks[2]);

        // Place terminal cursor in viewport
        let screen_x = viewport.x
            + gutter_width as u16
            + (editor.cursor_x.saturating_sub(editor.scroll_x)) as u16;
        let screen_y = viewport.y + (editor.cursor_y.saturating_sub(editor.scroll_y)) as u16;

        if screen_x < viewport.right() && screen_y < viewport.bottom() {
            frame.set_cursor_position(Position::new(screen_x, screen_y));
        }
    }
}
