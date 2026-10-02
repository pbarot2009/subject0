//! # Terminal UI Render Pipeline & Floating Modals Subsystem
//!
//! Handles layout structuring, double-buffering frames via Ratatui, syntax token
//! coloring, inlay hints styling, gutter diagnostic and Git diff indicators,
//! floating autocomplete, and modals (hover cards, quickfixes, symbol outlines,
//! pickers, and command palette).

use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Position, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Paragraph},
};

use crate::editor::{Editor, Focus, HitAction, HitRegion, Mode};
use crate::git::GutterChange;
use crate::lsp::{InlayHintType, LspStatus, utf16_to_char_col};
use crate::syntax::{completion_kind_icon, file_icon_and_color, symbol_kind_icon};

use crate::nerdfonts::{
    BOX_HORIZONTAL, BOX_VERTICAL, CHECK, CHEVRON_RIGHT, CLOSE, CMD_RENAME, CMD_SYMBOLS,
    CURSOR_BLOCK, DIAG_ERROR, DIAG_ERROR_PAD, DIAG_HINT, DIAG_HINT_PAD, DIAG_INFO, DIAG_INFO_PAD,
    DIAG_WARN, DIAG_WARN_PAD, ELLIPSIS, FOLDER, FOLDER_CLOSED, FOLDER_OPEN, FOLDER_OUTLINE,
    GIT_BRANCH, GUTTER_EMPTY, HELP, HINTS_OFF, HINTS_ON, INFO_DOC, KIND_FUNCTION, LIGHTBULB,
    LINE_WRAP, LOCATION, LSP_ERROR, LSP_NOT_FOUND, LSP_READY, MODIFIED_DOT, PALETTE,
    POWERLINE_LEFT, POWERLINE_RIGHT, PROMPT_CHEVRON, PROMPT_COLON, SELECTION_BLANK, SETTINGS_COGS,
    SPINNER, THEME, TILDE, TOOL_LINK, WRAP_OFF, WRAP_ON,
};

/// Column budget for the shell. Panels shrink or hide before they overlap the deck.
pub fn shell_layout(
    width: u16,
    height: u16,
    files: bool,
    problems: bool,
    outline: bool,
) -> (u16, u16, u16, u16, bool) {
    let narrow = width < 72;
    let rail = if width >= 100 { 3 } else { 0 };
    let problems_h = if problems && !narrow && height >= 16 {
        4.min(height.saturating_sub(8))
    } else {
        0
    };
    let mut explorer = if files && !narrow {
        if width < 100 { 22 } else { 28 }
    } else {
        0
    };
    let mut outline_w = if outline && width >= 130 && height >= 16 {
        26
    } else {
        0
    };
    let mut reserved = rail + explorer + outline_w;
    if width.saturating_sub(reserved) < 24 {
        outline_w = 0;
        reserved = rail + explorer;
    }
    if width.saturating_sub(reserved) < 20 {
        explorer = 0;
    }
    (rail, explorer, outline_w, problems_h, narrow)
}

fn cols(s: &str) -> usize {
    s.chars().map(char_cols).sum()
}

fn char_cols(ch: char) -> usize {
    let cp = ch as u32;
    if ch == '\u{fe0f}' || ch == '\u{fe0e}' || ch == '\u{200d}' {
        0
    } else if cp >= 0x1100
        || (0xE000..=0xF8FF).contains(&cp)
        || (0xF0000..=0xFFFFD).contains(&cp)
        || (0x100000..=0x10FFFD).contains(&cp)
    {
        2
    } else {
        1
    }
}

fn fit_cols(s: &str, max_cols: usize) -> String {
    if max_cols == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut used = 0;
    for ch in s.chars() {
        let w = char_cols(ch);
        if used + w > max_cols {
            break;
        }
        out.push(ch);
        used += w;
    }
    while used < max_cols {
        out.push(' ');
        used += 1;
    }
    out
}

/// Clips `s` to a terminal column budget, keeping the ellipsis inside the budget.
pub fn safe_truncate(s: &str, max_cols: usize) -> String {
    clip_cols(s, max_cols)
}

fn clip_cols(s: &str, max_cols: usize) -> String {
    if max_cols == 0 || cols(s) <= max_cols {
        return if max_cols == 0 {
            String::new()
        } else {
            s.to_string()
        };
    }
    let ell = ELLIPSIS;
    let budget = max_cols.saturating_sub(cols(ell));
    let mut out = String::new();
    let mut used = 0;
    for ch in s.chars() {
        let w = char_cols(ch);
        if used + w > budget {
            break;
        }
        out.push(ch);
        used += w;
    }
    out.push_str(ell);
    out
}

fn pad_line(mut spans: Vec<Span<'static>>, width: usize, style: Style) -> Line<'static> {
    let used: usize = spans.iter().map(|span| cols(span.content.as_ref())).sum();
    if used < width {
        spans.push(Span::styled(" ".repeat(width - used), style));
    }
    Line::from(spans)
}

fn explorer_line(
    depth: usize,
    icon: &str,
    icon_color: Color,
    name: &str,
    selected: bool,
    width: usize,
    theme: crate::theme::Theme,
) -> Line<'static> {
    let row_bg = if selected {
        theme.explorer_sel_bg
    } else {
        theme.explorer_bg
    };
    let name_style = if selected {
        Style::default()
            .bg(row_bg)
            .fg(theme.explorer_sel_fg)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().bg(row_bg).fg(theme.explorer_fg)
    };
    let indent = "  ".repeat(depth);
    let prefix = format!(" {indent}");
    let icon_s = format!("{icon} ");
    let used = cols(&prefix) + cols(&icon_s);
    let label = clip_cols(name, width.saturating_sub(used));
    pad_line(
        vec![
            Span::styled(prefix, name_style),
            Span::styled(icon_s, Style::default().fg(icon_color).bg(row_bg)),
            Span::styled(label, name_style),
        ],
        width,
        name_style,
    )
}

/// Primary UI rendering entry point dispatched on every event loop redraw tick.
pub fn render_ui(frame: &mut Frame, editor: &mut Editor) {
    let size = frame.area();
    let theme = editor.theme;
    editor.hit_regions.clear();
    editor.term_cols = size.width;
    let narrow = size.width < 72;
    let (rail_w, explorer_w, outline_w, problems_h, _) = shell_layout(
        size.width,
        size.height,
        editor.explorer.visible,
        editor.problems_open,
        editor.outline_open,
    );
    let show_rail = rail_w > 0;
    let show_problems = problems_h > 0;
    let show_outline = outline_w > 0;
    let dock_explorer = explorer_w > 0;

    let deck_h = 1u16;
    let cmd_h = 1u16;
    let main_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(3),
            Constraint::Length(problems_h),
            Constraint::Length(deck_h),
            Constraint::Length(cmd_h),
        ])
        .split(size);
    let body = main_chunks[0];
    let problems_area = if show_problems {
        Some(main_chunks[1])
    } else {
        None
    };
    let deck_area = main_chunks[2];
    let command_area = main_chunks[3];

    let h_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(rail_w),
            Constraint::Length(explorer_w),
            Constraint::Min(8),
            Constraint::Length(outline_w),
        ])
        .split(body);
    let rail_area = if show_rail { Some(h_chunks[0]) } else { None };
    let explorer_area = if dock_explorer {
        Some(h_chunks[1])
    } else {
        None
    };
    let editor_area = h_chunks[2];
    let outline_area = if show_outline {
        Some(h_chunks[3])
    } else {
        None
    };

    // 1. Render File Explorer (Sidebar with padded 2-cell icons)
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
            .title(Line::from(vec![
                Span::raw(" "),
                Span::styled(format!("{FOLDER} "), Style::default().fg(theme.mode_normal)),
                Span::styled(
                    "Files ",
                    Style::default()
                        .fg(theme.explorer_fg)
                        .add_modifier(Modifier::BOLD),
                ),
            ]));

        let inner_exp = exp_block.inner(exp_rect);
        frame.render_widget(exp_block, exp_rect);

        editor.explorer.update_scroll(inner_exp.height as usize);

        let mut tree_lines = Vec::new();
        let scroll_start = editor.explorer.scroll;
        let scroll_end =
            (scroll_start + inner_exp.height as usize).min(editor.explorer.entries.len());

        for i in scroll_start..scroll_end {
            let entry = &editor.explorer.entries[i];
            let (raw_icon, icon_color) = if entry.is_dir {
                if entry.expanded {
                    (FOLDER_OPEN, theme.explorer_dir_expanded)
                } else {
                    (FOLDER_CLOSED, theme.explorer_dir)
                }
            } else {
                let (i_str, c) = file_icon_and_color(Some(&entry.path));
                (i_str.trim(), c)
            };
            tree_lines.push(explorer_line(
                entry.depth,
                raw_icon,
                icon_color,
                &entry.name,
                i == editor.explorer.selected_idx,
                inner_exp.width as usize,
                theme,
            ));
            let row = inner_exp.y + (i - scroll_start) as u16;
            editor.hit_regions.push(HitRegion {
                x: inner_exp.x,
                y: row,
                w: inner_exp.width,
                h: 1,
                action: HitAction::ClickFile(i),
            });
        }

        frame.render_widget(Paragraph::new(tree_lines), inner_exp);
    }

    // 2. Render Document Editor Viewport
    let (icon, icon_color) = file_icon_and_color(editor.path.as_ref());
    let clean_icon = icon.trim();
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
        Span::styled(format!("{clean_icon} "), Style::default().fg(icon_color)),
        Span::styled(
            file_title,
            Style::default().fg(theme.fg).add_modifier(Modifier::BOLD),
        ),
        if editor.modified {
            Span::styled(
                format!(" {MODIFIED_DOT}"),
                Style::default().fg(Color::Rgb(235, 235, 235)),
            )
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
    // Gutter layout: Diag (2 cells) + Git Diff (2 cells) + Line Numbers (line_digits + 3 cells)
    let tight = narrow || size.width < 96;
    let gutter_width = line_digits + if tight { 3 } else { 7 };
    let num_tail = if tight {
        " ".to_string()
    } else {
        format!(" {BOX_VERTICAL} ")
    };
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
        if editor.folded_lines.iter().any(|start| {
            y > *start
                && editor
                    .syntax
                    .fold_at(*start)
                    .is_some_and(|f| y <= f.end_line && y != f.start_line)
        }) {
            continue;
        }
        if (current_row as usize) >= inner_area.height as usize {
            break;
        }

        let is_cursor_line = y == editor.cursor_y;
        let text_origin = inner_area.x + gutter_width as u16;

        // Compiler Diagnostic Indicator (2 cells)
        let line_diag = editor.diagnostics.iter().find(|d| d.line == y);
        let (diag_marker, diag_style) = if tight {
            match line_diag.map(|d| d.severity) {
                Some(1) => ("!", Style::default().fg(theme.diag_error)),
                Some(2) => ("!", Style::default().fg(theme.diag_warn)),
                Some(3) => ("i", Style::default().fg(theme.diag_info)),
                Some(_) => (".", Style::default().fg(theme.diag_hint)),
                None => (" ", Style::default()),
            }
        } else {
            match line_diag.map(|d| d.severity) {
                Some(1) => (DIAG_ERROR_PAD, Style::default().fg(theme.diag_error)),
                Some(2) => (DIAG_WARN_PAD, Style::default().fg(theme.diag_warn)),
                Some(3) => (DIAG_INFO_PAD, Style::default().fg(theme.diag_info)),
                Some(_) => (DIAG_HINT_PAD, Style::default().fg(theme.diag_hint)),
                None => (GUTTER_EMPTY, Style::default()),
            }
        };

        let git_change = editor.git_diff.change_for_line(y);
        let (git_marker, git_style) = if tight {
            match git_change {
                Some(GutterChange::Added) => ("+", Style::default().fg(theme.git_added)),
                Some(GutterChange::Modified) => ("~", Style::default().fg(theme.git_modified)),
                Some(GutterChange::Deleted) => ("-", Style::default().fg(theme.git_deleted)),
                None => (" ", Style::default()),
            }
        } else {
            match git_change {
                Some(GutterChange::Added) => ("▎ ", Style::default().fg(theme.git_added)),
                Some(GutterChange::Modified) => ("▎ ", Style::default().fg(theme.git_modified)),
                Some(GutterChange::Deleted) => ("▔ ", Style::default().fg(theme.git_deleted)),
                None => ("  ", Style::default()),
            }
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
        let mut base_cells: Vec<(char, Style, usize)> = Vec::with_capacity(line_str.len() * 2);
        let mut char_idx = 0;
        for span in syntax_spans {
            let st = span.style;
            for ch in span.content.chars() {
                if ch == '\t' {
                    for _ in 0..4 {
                        base_cells.push((' ', st, char_idx));
                    }
                } else {
                    base_cells.push((ch, st, char_idx));
                }
                char_idx += 1;
            }
        }

        // Weave Inlay Hints (Inferred Types & Parameters)
        let mut char_cells: Vec<(char, Style, Option<usize>)> = Vec::new();
        let line_hints: Vec<&crate::lsp::InlayHintItem> = if editor.show_inlay_hints {
            editor.inlay_hints.iter().filter(|h| h.line == y).collect()
        } else {
            Vec::new()
        };

        let mut hint_map: std::collections::HashMap<usize, Vec<&crate::lsp::InlayHintItem>> =
            std::collections::HashMap::new();
        for h in line_hints {
            let col = utf16_to_char_col(&line_str, h.col);
            hint_map.entry(col).or_default().push(h);
        }

        for (ch, st, idx) in base_cells {
            if let Some(hints) = hint_map.remove(&idx) {
                for h in hints {
                    let h_style = match h.kind {
                        InlayHintType::Parameter => Style::default()
                            .fg(theme.inlay_hint_param_fg)
                            .bg(theme.inlay_hint_bg),
                        _ => Style::default()
                            .fg(theme.inlay_hint_fg)
                            .bg(theme.inlay_hint_bg),
                    };

                    if h.padding_left {
                        char_cells.push((' ', h_style, None));
                    }
                    for hc in h.label.chars() {
                        char_cells.push((hc, h_style, None));
                    }
                    if h.padding_right {
                        char_cells.push((' ', h_style, None));
                    }
                }
            }
            char_cells.push((ch, st, Some(idx)));
        }

        // Tail hints (e.g. end-of-line return types)
        if let Some(hints) = hint_map.remove(&line_str.chars().count()) {
            for h in hints {
                let h_style = match h.kind {
                    InlayHintType::Parameter => Style::default()
                        .fg(theme.inlay_hint_param_fg)
                        .bg(theme.inlay_hint_bg),
                    _ => Style::default()
                        .fg(theme.inlay_hint_fg)
                        .bg(theme.inlay_hint_bg),
                };
                if h.padding_left {
                    char_cells.push((' ', h_style, None));
                }
                for hc in h.label.chars() {
                    char_cells.push((hc, h_style, None));
                }
                if h.padding_right {
                    char_cells.push((' ', h_style, None));
                }
            }
        }

        let visual_cursor_x = {
            let mut resolved = None;
            for (cell_idx, &(_, _, opt_idx)) in char_cells.iter().enumerate() {
                if opt_idx == Some(editor.cursor_x) {
                    resolved = Some(cell_idx);
                    break;
                }
            }
            resolved.unwrap_or(char_cells.len())
        };

        if editor.line_wrap && char_cells.len() > text_area_width {
            let total_chars = char_cells.len();
            let mut chunk_start = 0;
            let mut is_first_sub = true;

            while chunk_start < total_chars && (current_row as usize) < inner_area.height as usize {
                let chunk_end = (chunk_start + text_area_width).min(total_chars);
                let (marker, git_m, gutter_str, g_style) = if is_first_sub {
                    (
                        diag_marker,
                        git_marker,
                        format!("{:>width$}{num_tail}", y + 1, width = line_digits),
                        gutter_style,
                    )
                } else {
                    (
                        GUTTER_EMPTY,
                        "  ",
                        format!(
                            "{:>width$}{num_tail}",
                            if tight { "↳" } else { LINE_WRAP },
                            width = line_digits
                        ),
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
                    Span::styled(
                        git_m,
                        if is_first_sub {
                            git_style
                        } else {
                            Style::default()
                        },
                    ),
                    Span::styled(gutter_str, g_style),
                ];

                for col_idx in chunk_start..chunk_end {
                    let (ch, mut st, opt_orig_idx) = char_cells[col_idx];
                    if let Some(orig_char_idx) = opt_orig_idx {
                        if editor.is_char_selected(y, orig_char_idx) {
                            st = st.bg(theme.selection_bg).fg(theme.selection_fg);
                        } else if is_cursor_line {
                            st = st.bg(theme.cursor_line_bg);
                        }
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

                let row_style = if is_cursor_line {
                    Style::default().bg(theme.cursor_line_bg)
                } else {
                    Style::default().bg(theme.bg)
                };
                editor.hit_regions.push(HitRegion {
                    x: inner_area.x,
                    y: inner_area.y + current_row,
                    w: inner_area.width,
                    h: 1,
                    action: HitAction::ClickEditorLine {
                        line: y,
                        origin_x: text_origin,
                        visual_base: chunk_start,
                    },
                });
                visible_lines.push(pad_line(sub_spans, inner_area.width as usize, row_style));
                current_row += 1;
                chunk_start = chunk_end;
                is_first_sub = false;
            }
        } else {
            let (marker, git_m, gutter_str, g_style) = (
                diag_marker,
                git_marker,
                format!("{:>width$}{num_tail}", y + 1, width = line_digits),
                gutter_style,
            );
            let mut row_spans = vec![
                Span::styled(marker, diag_style),
                Span::styled(git_m, git_style),
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

                for (_col_idx, (ch, mut st, opt_orig_idx)) in char_cells
                    .into_iter()
                    .enumerate()
                    .skip(skip_count)
                    .take(take_count)
                {
                    if let Some(orig_char_idx) = opt_orig_idx {
                        if editor.is_char_selected(y, orig_char_idx) {
                            st = st.bg(theme.selection_bg).fg(theme.selection_fg);
                        } else if is_cursor_line {
                            st = st.bg(theme.cursor_line_bg);
                        }
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

            let row_style = if is_cursor_line {
                Style::default().bg(theme.cursor_line_bg)
            } else {
                Style::default().bg(theme.bg)
            };
            let visual_base = if editor.line_wrap { 0 } else { editor.scroll_x };
            editor.hit_regions.push(HitRegion {
                x: inner_area.x,
                y: inner_area.y + current_row,
                w: inner_area.width,
                h: 1,
                action: HitAction::ClickEditorLine {
                    line: y,
                    origin_x: text_origin,
                    visual_base,
                },
            });
            visible_lines.push(pad_line(row_spans, inner_area.width as usize, row_style));
            current_row += 1;
        }
    }

    for _ in (current_row as usize)..inner_area.height as usize {
        visible_lines.push(Line::from(vec![
            Span::raw(GUTTER_EMPTY),
            Span::raw("  "),
            Span::styled(
                format!("{:>width$}{num_tail}", TILDE, width = line_digits),
                Style::default().fg(theme.line_number),
            ),
        ]));
    }

    frame.render_widget(Paragraph::new(visible_lines), inner_area);

    // 3. Render Statusline
    // 4. Command Bar & Status Messages
    let (screen_x, screen_y) =
        cursor_screen_pos.unwrap_or((inner_area.x + gutter_width as u16, inner_area.y));

    if let Some(prompt) = &editor.confirm {
        let yes = " Yes ";
        let no = " No ";
        let msg = clip_cols(
            &prompt.message,
            (command_area.width as usize).saturating_sub(cols(yes) + cols(no) + 4),
        );
        let line = Line::from(vec![
            Span::styled(
                format!(" {msg} "),
                Style::default().bg(theme.status_bg).fg(theme.status_fg),
            ),
            Span::styled(
                yes,
                Style::default()
                    .bg(theme.mode_insert)
                    .fg(Color::Rgb(15, 17, 22))
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" ", Style::default().bg(theme.status_bg)),
            Span::styled(
                no,
                Style::default()
                    .bg(theme.diag_error)
                    .fg(Color::Rgb(15, 17, 22))
                    .add_modifier(Modifier::BOLD),
            ),
        ]);
        let yes_x = command_area.x + 1 + cols(&msg) as u16 + 1;
        editor.hit_regions.push(HitRegion {
            x: yes_x,
            y: command_area.y,
            w: cols(yes) as u16,
            h: 1,
            action: HitAction::ConfirmYes,
        });
        editor.hit_regions.push(HitRegion {
            x: yes_x + cols(yes) as u16 + 1,
            y: command_area.y,
            w: cols(no) as u16,
            h: 1,
            action: HitAction::ConfirmNo,
        });
        frame.render_widget(
            Paragraph::new(line).style(Style::default().bg(theme.status_bg)),
            command_area,
        );
    } else if editor.mode == Mode::Command {
        let prompt_line = Line::from(vec![
            Span::styled(
                format!(" {PROMPT_COLON}"),
                Style::default()
                    .fg(theme.mode_command)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(&editor.command_buffer),
        ]);
        frame.render_widget(
            Paragraph::new(prompt_line)
                .style(Style::default().bg(theme.status_bg).fg(theme.status_fg)),
            command_area,
        );
        let cmd_chars = editor.command_buffer.chars().count();
        let target_x = (2 + cmd_chars).min((frame.area().width.saturating_sub(1)) as usize) as u16;
        frame.set_cursor_position(Position::new(target_x, command_area.y));
    } else {
        let active_diag = editor
            .diagnostics
            .iter()
            .find(|d| d.line == editor.cursor_y);
        let msg_line = if let Some(diag) = active_diag {
            let (d_icon, icon_style) = match diag.severity {
                1 => (
                    format!(" {DIAG_ERROR} "),
                    Style::default().fg(theme.diag_error),
                ),
                2 => (
                    format!(" {DIAG_WARN} "),
                    Style::default().fg(theme.diag_warn),
                ),
                3 => (
                    format!(" {DIAG_INFO} "),
                    Style::default().fg(theme.diag_info),
                ),
                _ => (
                    format!(" {DIAG_HINT} "),
                    Style::default().fg(theme.diag_hint),
                ),
            };
            Line::from(vec![
                Span::styled(d_icon, icon_style.bg(theme.status_bg)),
                Span::styled(
                    &diag.message,
                    Style::default()
                        .fg(theme.fg)
                        .bg(theme.status_bg)
                        .add_modifier(Modifier::ITALIC),
                ),
            ])
        } else {
            Line::from(vec![
                Span::styled(
                    format!(" {CHEVRON_RIGHT} "),
                    Style::default().fg(theme.line_number).bg(theme.status_bg),
                ),
                Span::styled(
                    &editor.status_msg,
                    Style::default().fg(theme.status_fg).bg(theme.status_bg),
                ),
            ])
        };

        frame.render_widget(
            Paragraph::new(msg_line)
                .style(Style::default().bg(theme.status_bg).fg(theme.status_fg)),
            command_area,
        );

        if editor.focus == Focus::Editor
            && let Some((cx, cy)) = cursor_screen_pos
            && cx < inner_area.right()
            && cy < inner_area.bottom()
        {
            frame.set_cursor_position(Position::new(cx, cy));
        }
    }

    render_shell_chrome(
        frame,
        editor,
        rail_area,
        outline_area,
        problems_area,
        deck_area,
        narrow,
    );

    // 5. Floating Autocomplete Dropdown
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
                Span::styled(
                    format!("{kind_icon} "),
                    Style::default().bg(item_bg).fg(kind_color),
                ),
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

    // 6. Floating Signature Help Tooltip
    if editor.mode == Mode::Insert
        && let Some(help) = &editor.signature_help
    {
        let width = (help.signature_label.len() as u16 + 4)
            .min(frame.area().width.saturating_sub(4))
            .max(30);
        let height = 3u16;
        let x = screen_x.min(frame.area().width.saturating_sub(width));
        let y = if screen_y > height {
            screen_y.saturating_sub(height)
        } else {
            (screen_y + 1).min(frame.area().height.saturating_sub(height))
        };

        let tooltip_rect = Rect::new(x, y, width, height);
        frame.render_widget(Clear, tooltip_rect);

        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(theme.syn_function))
            .style(Style::default().bg(theme.hover_bg))
            .title(Line::from(Span::styled(
                format!(" {KIND_FUNCTION} Signature "),
                Style::default()
                    .fg(theme.syn_function)
                    .add_modifier(Modifier::BOLD),
            )));

        let line = Line::from(vec![
            Span::raw(" "),
            Span::styled(
                &help.signature_label,
                Style::default().fg(theme.fg).add_modifier(Modifier::BOLD),
            ),
        ]);

        frame.render_widget(Paragraph::new(line).block(block), tooltip_rect);
    }

    // 7. Floating Hover Documentation Card
    if let Some(hover) = &editor.hover_info {
        let max_content_len = hover.lines.iter().map(String::len).max().unwrap_or(30);
        let width = (max_content_len as u16 + 4)
            .min(frame.area().width.saturating_sub(4))
            .max(40);
        let height = (hover.lines.len() as u16 + 4)
            .min(16)
            .min(frame.area().height.saturating_sub(4));

        let x = (frame.area().width.saturating_sub(width)) / 2;
        let y = (frame.area().height.saturating_sub(height)) / 2;

        let card_rect = Rect::new(x, y, width, height);
        frame.render_widget(Clear, card_rect);

        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(theme.hover_border))
            .style(Style::default().bg(theme.hover_bg))
            .title(Line::from(vec![
                Span::styled(
                    format!(" {INFO_DOC} Documentation & Types "),
                    Style::default()
                        .fg(theme.hover_border)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    "(Esc/q to dismiss) ",
                    Style::default().fg(theme.line_number),
                ),
            ]));

        let inner_card = block.inner(card_rect);
        frame.render_widget(block, card_rect);

        let mut doc_lines = Vec::new();
        let scroll_start = editor.hover_scroll;
        let scroll_end = (scroll_start + inner_card.height as usize).min(hover.lines.len());

        for i in scroll_start..scroll_end {
            let l = &hover.lines[i];
            let is_code = l.starts_with("```") || l.starts_with("    ");
            let st = if is_code {
                Style::default().fg(theme.syn_function)
            } else {
                Style::default().fg(theme.hover_fg)
            };
            doc_lines.push(Line::from(vec![Span::raw(" "), Span::styled(l, st)]));
        }

        frame.render_widget(Paragraph::new(doc_lines), inner_card);
    }

    // 8. Code Action Picker Modal
    if let Some(picker) = &editor.code_action_picker {
        let width = 56u16.min(size.width.saturating_sub(2));
        let height = ((picker.actions.len() as u16) + 4).min(size.height.saturating_sub(2));
        let x = (size.width.saturating_sub(width)) / 2;
        let y = (size.height.saturating_sub(height)) / 2;

        let rect = Rect::new(x, y, width, height);
        frame.render_widget(Clear, rect);

        let mut lines = Vec::new();
        lines.push(Line::from(vec![
            Span::styled(
                " Quickfixes & Actions ",
                Style::default().fg(theme.status_fg),
            ),
            Span::styled("(Enter to apply):", Style::default().fg(theme.line_number)),
        ]));
        lines.push(Line::from(Span::styled(
            BOX_HORIZONTAL.repeat((width as usize).saturating_sub(2)),
            Style::default().fg(theme.border),
        )));

        for (idx, action) in picker.actions.iter().enumerate() {
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

            let mark = if is_sel {
                format!(" {CHEVRON_RIGHT} ")
            } else {
                SELECTION_BLANK.to_string()
            };
            let pref = if action.is_preferred {
                format!(" {CHECK}")
            } else {
                String::new()
            };
            lines.push(Line::from(vec![
                Span::styled(mark, Style::default().bg(bg).fg(theme.mode_insert)),
                Span::styled(format!("{}{}", action.title, pref), style),
            ]));
        }

        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(theme.popup_border))
            .style(Style::default().bg(theme.popup_bg))
            .title(Line::from(Span::styled(
                format!(" {LIGHTBULB} Available Code Actions "),
                Style::default()
                    .fg(theme.popup_text)
                    .add_modifier(Modifier::BOLD),
            )));

        frame.render_widget(Paragraph::new(lines).block(block), rect);
        for idx in 0..picker.actions.len() {
            editor.hit_regions.push(HitRegion {
                x: rect.x + 1,
                y: rect.y + 3 + idx as u16,
                w: rect.width.saturating_sub(2),
                h: 1,
                action: HitAction::ClickAction(idx),
            });
        }
    }
    if let Some(picker) = &editor.symbol_picker {
        let width = 58u16.min(size.width.saturating_sub(2));
        let height = 14u16.min(size.height.saturating_sub(2));
        let x = (size.width.saturating_sub(width)) / 2;
        let y = 1u16;

        let rect = Rect::new(x, y, width, height);
        frame.render_widget(Clear, rect);

        let filtered = picker.filtered_symbols();
        let max_visible = (height.saturating_sub(4)) as usize;

        let mut lines = Vec::new();
        lines.push(Line::from(vec![
            Span::styled(
                format!(" {CMD_SYMBOLS} {PROMPT_CHEVRON} "),
                Style::default()
                    .fg(theme.mode_command)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                &picker.query,
                Style::default()
                    .fg(theme.popup_sel_fg)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(CURSOR_BLOCK, Style::default().fg(theme.border_focused)),
        ]));
        lines.push(Line::from(Span::styled(
            BOX_HORIZONTAL.repeat((width as usize).saturating_sub(2)),
            Style::default().fg(theme.border),
        )));

        let scroll_start = picker.scroll;
        let scroll_end = (scroll_start + max_visible).min(filtered.len());

        for i in scroll_start..scroll_end {
            let sym = filtered[i];
            let is_sel = i == picker.selected_idx;
            let row_bg = if is_sel {
                theme.popup_sel_bg
            } else {
                theme.popup_bg
            };
            let (icon, icon_c) = symbol_kind_icon(sym.kind);

            let style = if is_sel {
                Style::default()
                    .bg(row_bg)
                    .fg(theme.popup_sel_fg)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().bg(row_bg).fg(theme.popup_text)
            };

            lines.push(Line::from(vec![
                Span::styled(" ", Style::default().bg(row_bg)),
                Span::styled(format!("{icon} "), Style::default().bg(row_bg).fg(icon_c)),
                Span::styled(sym.name.clone(), style),
                Span::styled(
                    format!(" :{}", sym.line + 1),
                    Style::default().bg(row_bg).fg(theme.line_number),
                ),
            ]));
        }

        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(theme.popup_border))
            .style(Style::default().bg(theme.popup_bg))
            .title(Line::from(Span::styled(
                format!(" {CMD_SYMBOLS} Document Symbols (Esc to close) "),
                Style::default()
                    .fg(theme.popup_text)
                    .add_modifier(Modifier::BOLD),
            )));

        frame.render_widget(Paragraph::new(lines).block(block), rect);
        for i in scroll_start..scroll_end {
            editor.hit_regions.push(HitRegion {
                x: rect.x + 1,
                y: rect.y + 3 + (i - scroll_start) as u16,
                w: rect.width.saturating_sub(2),
                h: 1,
                action: HitAction::ClickSymbol(i),
            });
        }
    }
    if let Some(picker) = &editor.location_picker {
        let width = 64u16.min(size.width.saturating_sub(2));
        let height = ((picker.locations.len() as u16) + 4)
            .min(14)
            .min(size.height.saturating_sub(2));
        let x = (size.width.saturating_sub(width)) / 2;
        let y = (size.height.saturating_sub(height)) / 2;

        let rect = Rect::new(x, y, width, height);
        frame.render_widget(Clear, rect);

        let mut lines = Vec::new();
        lines.push(Line::from(vec![
            Span::styled(
                format!(" {} ", picker.title),
                Style::default().fg(theme.status_fg),
            ),
            Span::styled("(Enter to jump):", Style::default().fg(theme.line_number)),
        ]));
        lines.push(Line::from(Span::styled(
            BOX_HORIZONTAL.repeat((width as usize).saturating_sub(2)),
            Style::default().fg(theme.border),
        )));

        let max_vis = height.saturating_sub(4) as usize;
        let start = picker.scroll;
        let end = (start + max_vis).min(picker.locations.len());

        for i in start..end {
            let loc = &picker.locations[i];
            let is_sel = i == picker.selected_idx;
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

            let mark = if is_sel {
                format!(" {CHEVRON_RIGHT} ")
            } else {
                SELECTION_BLANK.to_string()
            };
            let file_str = loc
                .path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();

            lines.push(Line::from(vec![
                Span::styled(mark, Style::default().bg(bg).fg(theme.mode_insert)),
                Span::styled(format!("{}:{}", file_str, loc.line + 1), style),
            ]));
        }

        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(theme.popup_border))
            .style(Style::default().bg(theme.popup_bg))
            .title(Line::from(Span::styled(
                format!(" {TOOL_LINK} {} ", picker.title),
                Style::default()
                    .fg(theme.popup_text)
                    .add_modifier(Modifier::BOLD),
            )));

        frame.render_widget(Paragraph::new(lines).block(block), rect);
        for i in start..end {
            editor.hit_regions.push(HitRegion {
                x: rect.x + 1,
                y: rect.y + 3 + (i - start) as u16,
                w: rect.width.saturating_sub(2),
                h: 1,
                action: HitAction::ClickLocation(i),
            });
        }
    }
    if let Some(name) = &editor.rename_prompt {
        let width = 46u16.min(size.width.saturating_sub(2));
        let height = 3u16;
        let x = (size.width.saturating_sub(width)) / 2;
        let y = (size.height.saturating_sub(height)) / 2;

        let rect = Rect::new(x, y, width, height);
        frame.render_widget(Clear, rect);

        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(theme.mode_command))
            .style(Style::default().bg(theme.popup_bg))
            .title(Line::from(Span::styled(
                format!(" {CMD_RENAME} Rename Symbol (Enter to commit) "),
                Style::default()
                    .fg(theme.mode_command)
                    .add_modifier(Modifier::BOLD),
            )));

        let line = Line::from(vec![
            Span::styled(" New Name: ", Style::default().fg(theme.status_fg)),
            Span::styled(
                name,
                Style::default()
                    .fg(theme.popup_sel_fg)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(CURSOR_BLOCK, Style::default().fg(theme.mode_command)),
        ]);

        frame.render_widget(Paragraph::new(line).block(block), rect);
    }

    // 12. Render Command Palette Modal
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
                format!(" {PALETTE} {PROMPT_CHEVRON} "),
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
            Span::styled(CURSOR_BLOCK, Style::default().fg(theme.border_focused)),
        ]));
        palette_lines.push(Line::from(Span::styled(
            BOX_HORIZONTAL.repeat((width as usize).saturating_sub(2)),
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
                Span::styled(
                    format!("{} ", cmd.icon),
                    Style::default().bg(row_bg).fg(theme.mode_normal),
                ),
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
                format!(" {PALETTE} Command Palette (Esc to close) "),
                Style::default()
                    .fg(theme.popup_text)
                    .add_modifier(Modifier::BOLD),
            )));

        frame.render_widget(Paragraph::new(palette_lines).block(p_block), palette_rect);
    }

    // 13. Render Theme Picker Modal Overlay
    if let Some(picker) = &editor.theme_picker {
        let themes = crate::theme::Theme::all();
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
            BOX_HORIZONTAL.repeat((width as usize).saturating_sub(2)),
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
                format!(" {CHECK} ")
            } else if is_sel {
                format!(" {CHEVRON_RIGHT} ")
            } else {
                SELECTION_BLANK.to_string()
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
                format!(" {THEME} Color Themes "),
                Style::default()
                    .fg(theme.popup_text)
                    .add_modifier(Modifier::BOLD),
            )));

        frame.render_widget(Paragraph::new(lines).block(block), picker_rect);
        for idx in 0..themes.len() {
            editor.hit_regions.push(HitRegion {
                x: picker_rect.x + 1,
                y: picker_rect.y + 3 + idx as u16,
                w: picker_rect.width.saturating_sub(2),
                h: 1,
                action: HitAction::ClickTheme(idx),
            });
        }
    }

    // 14. Render LSP Picker Modal Overlay
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
            BOX_HORIZONTAL.repeat((width as usize).saturating_sub(2)),
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
                    if is_sel {
                        format!(" {CHECK} ")
                    } else {
                        SELECTION_BLANK.to_string()
                    },
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
                format!(" {SETTINGS_COGS} Choose Language Server "),
                Style::default()
                    .fg(theme.popup_text)
                    .add_modifier(Modifier::BOLD),
            )));

        frame.render_widget(Paragraph::new(lines).block(block), picker_rect);
        for idx in 0..picker.candidates.len() {
            editor.hit_regions.push(HitRegion {
                x: picker_rect.x + 1,
                y: picker_rect.y + 3 + idx as u16,
                w: picker_rect.width.saturating_sub(2),
                h: 1,
                action: HitAction::ClickLsp(idx),
            });
        }
    }

    // 15. Render In-Editor Help Modal
    if editor.show_help {
        let width = 66u16.min(size.width.saturating_sub(4));
        let height = 24u16.min(size.height.saturating_sub(2));
        let x = (size.width.saturating_sub(width)) / 2;
        let y = (size.height.saturating_sub(height)) / 2;

        let help_rect = Rect::new(x, y, width, height);
        frame.render_widget(Clear, help_rect);
        editor.hit_regions.push(HitRegion {
            x: help_rect.x,
            y: help_rect.y,
            w: help_rect.width,
            h: 1,
            action: HitAction::ClosePopup,
        });

        let help_block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(theme.popup_border))
            .style(Style::default().bg(theme.popup_bg))
            .title(Line::from(vec![
                Span::styled(
                    format!(" {HELP} subject0 Keybindings & Help "),
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
            Line::from(Span::styled("GIT VERSION CONTROL & DIFFS", c_sec)),
            Line::from(vec![
                Span::styled("  ]c             ", c_key),
                Span::styled("Jump to next Git diff hunk", c_desc),
            ]),
            Line::from(vec![
                Span::styled("  [c             ", c_key),
                Span::styled("Jump to previous Git diff hunk", c_desc),
            ]),
            Line::from(vec![
                Span::styled("  :rh / :revert  ", c_key),
                Span::styled("Revert Git diff hunk under cursor", c_desc),
            ]),
            Line::from(vec![
                Span::styled("  :git           ", c_key),
                Span::styled("Show current branch and diff summary", c_desc),
            ]),
            Line::from(Span::raw("")),
            Line::from(Span::styled("LSP INTELLIGENCE & CODE ACTIONS", c_sec)),
            Line::from(vec![
                Span::styled("  K              ", c_key),
                Span::styled("Show Type & Documentation Hover card", c_desc),
            ]),
            Line::from(vec![
                Span::styled("  gd             ", c_key),
                Span::styled("Go to Definition", c_desc),
            ]),
            Line::from(vec![
                Span::styled("  gr             ", c_key),
                Span::styled("Find all References", c_desc),
            ]),
            Line::from(vec![
                Span::styled("  ga             ", c_key),
                Span::styled("Trigger Quickfixes & Code Actions", c_desc),
            ]),
            Line::from(vec![
                Span::styled("  :fmt / Alt-F   ", c_key),
                Span::styled("Format document with LSP server", c_desc),
            ]),
            Line::from(vec![
                Span::styled("  :rn / F2       ", c_key),
                Span::styled("Rename symbol (multi-file project-wide)", c_desc),
            ]),
            Line::from(vec![
                Span::styled("  :sym / :symbols", c_key),
                Span::styled("Fuzzy search document symbol outline", c_desc),
            ]),
            Line::from(vec![
                Span::styled("  ]d / [d        ", c_key),
                Span::styled("Jump to next / previous compiler diagnostic", c_desc),
            ]),
            Line::from(vec![
                Span::styled("  Ctrl-O / Ctrl-I", c_key),
                Span::styled("Jump backward / forward in navigation history", c_desc),
            ]),
            Line::from(vec![
                Span::styled("  :lsp-restart   ", c_key),
                Span::styled("Reboot crashed/frozen Language Server", c_desc),
            ]),
            Line::from(vec![
                Span::styled("  :hints         ", c_key),
                Span::styled("Toggle inferred type & param inlay hints", c_desc),
            ]),
            Line::from(Span::raw("")),
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
                Span::styled("  :wq, :wq!      ", c_key),
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

fn push_hit(editor: &mut Editor, area: Rect, action: HitAction) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    editor.hit_regions.push(HitRegion {
        x: area.x,
        y: area.y,
        w: area.width,
        h: area.height,
        action,
    });
}

fn short_lsp(name: &str) -> String {
    let base = name
        .rsplit(|c| c == '/' || c == '\\')
        .next()
        .unwrap_or(name);
    let base = base
        .strip_suffix("-language-server")
        .or_else(|| base.strip_suffix("-lsp"))
        .unwrap_or(base);
    base.trim().to_string()
}

fn render_shell_chrome(
    frame: &mut Frame,
    editor: &mut Editor,
    rail: Option<Rect>,
    outline: Option<Rect>,
    problems: Option<Rect>,
    deck: Rect,
    narrow: bool,
) {
    let theme = editor.theme;
    if let Some(area) = rail {
        frame.render_widget(
            Block::default().style(Style::default().bg(theme.explorer_bg)),
            area,
        );
        // Single-cell marks. Wide nerd icons overflow a 3-column rail and paint into the editor.
        let items = [
            (
                "F",
                HitAction::ToggleExplorer,
                editor.explorer.visible,
                theme.mode_normal,
            ),
            (
                "!",
                HitAction::ToggleProblems,
                editor.problems_open,
                theme.diag_error,
            ),
            (
                "O",
                HitAction::ToggleOutline,
                editor.outline_open,
                theme.syn_function,
            ),
            (
                "/",
                HitAction::OpenPalette,
                editor.palette.visible,
                theme.mode_command,
            ),
            (
                "T",
                HitAction::OpenTheme,
                editor.theme_picker.is_some(),
                theme.mode_visual,
            ),
        ];
        for (i, (mark, action, on, color)) in items.iter().enumerate() {
            let y = area.y + i as u16;
            if y >= area.bottom() {
                break;
            }
            let cell = Rect::new(area.x, y, area.width, 1);
            let bg = if *on {
                theme.explorer_sel_bg
            } else {
                theme.explorer_bg
            };
            let fg = if *on { *color } else { theme.line_number };
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    fit_cols(&format!(" {mark} "), area.width as usize),
                    Style::default().fg(fg).bg(bg).add_modifier(if *on {
                        Modifier::BOLD
                    } else {
                        Modifier::empty()
                    }),
                ))),
                cell,
            );
            push_hit(editor, cell, action.clone());
        }
    }

    if let Some(area) = outline {
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(theme.border))
            .style(Style::default().bg(theme.explorer_bg))
            .title(Line::from(Span::styled(
                format!(" {CMD_SYMBOLS} Outline "),
                Style::default()
                    .fg(theme.syn_function)
                    .add_modifier(Modifier::BOLD),
            )));
        let inner = block.inner(area);
        frame.render_widget(block, area);
        let tags: Vec<(usize, String)> = editor
            .syntax
            .tags
            .iter()
            .take(inner.height as usize)
            .enumerate()
            .map(|(i, tag)| (i, tag.name.clone()))
            .collect();
        let mut lines = Vec::new();
        for (i, name) in &tags {
            let icon_style = Style::default()
                .fg(theme.syn_function)
                .bg(theme.explorer_bg);
            let name_style = Style::default().fg(theme.explorer_fg).bg(theme.explorer_bg);
            let label = clip_cols(
                name,
                (inner.width as usize).saturating_sub(cols(KIND_FUNCTION) + 2),
            );
            lines.push(pad_line(
                vec![
                    Span::styled(format!(" {KIND_FUNCTION} "), icon_style),
                    Span::styled(label, name_style),
                ],
                inner.width as usize,
                name_style,
            ));
            push_hit(
                editor,
                Rect::new(inner.x, inner.y + *i as u16, inner.width, 1),
                HitAction::ClickOutline(*i),
            );
        }
        if lines.is_empty() {
            lines.push(Line::from(Span::styled(
                " no symbols",
                Style::default().fg(theme.line_number),
            )));
        }
        frame.render_widget(
            Paragraph::new(lines).style(Style::default().bg(theme.explorer_bg)),
            inner,
        );
    }

    if let Some(area) = problems {
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(theme.diag_warn))
            .style(Style::default().bg(theme.explorer_bg))
            .title(Line::from(Span::styled(
                format!(" {DIAG_ERROR} Problems {} ", editor.diagnostics.len()),
                Style::default()
                    .fg(theme.diag_warn)
                    .add_modifier(Modifier::BOLD),
            )));
        let inner = block.inner(area);
        frame.render_widget(block, area);
        let diags: Vec<(usize, String, Color, &'static str)> = editor
            .diagnostics
            .iter()
            .take(inner.height as usize)
            .enumerate()
            .map(|(i, d)| {
                let (color, icon) = match d.severity {
                    1 => (theme.diag_error, DIAG_ERROR),
                    2 => (theme.diag_warn, DIAG_WARN),
                    3 => (theme.diag_info, DIAG_INFO),
                    _ => (theme.diag_hint, DIAG_HINT),
                };
                (
                    i,
                    format!("{}:{} {}", d.line + 1, d.col + 1, d.message),
                    color,
                    icon,
                )
            })
            .collect();
        let mut lines = Vec::new();
        for (i, text, color, icon) in &diags {
            let icon_style = Style::default().fg(*color).bg(theme.explorer_bg);
            let name_style = Style::default().fg(theme.explorer_fg).bg(theme.explorer_bg);
            let label = clip_cols(text, (inner.width as usize).saturating_sub(cols(icon) + 2));
            lines.push(pad_line(
                vec![
                    Span::styled(format!(" {icon} "), icon_style),
                    Span::styled(label, name_style),
                ],
                inner.width as usize,
                name_style,
            ));
            push_hit(
                editor,
                Rect::new(inner.x, inner.y + *i as u16, inner.width, 1),
                HitAction::ClickProblem(*i),
            );
        }
        if lines.is_empty() {
            lines.push(Line::from(Span::styled(
                " no problems",
                Style::default().fg(theme.mode_insert),
            )));
        }
        frame.render_widget(
            Paragraph::new(lines).style(Style::default().bg(theme.explorer_bg)),
            inner,
        );
    }

    frame.render_widget(
        Block::default().style(Style::default().bg(theme.status_bg)),
        deck,
    );
    let (mode, mode_color) = match editor.mode {
        Mode::Normal => ("N", theme.mode_normal),
        Mode::Insert => ("I", theme.mode_insert),
        Mode::Command => ("C", theme.mode_command),
        Mode::Visual { .. } => ("V", theme.mode_visual),
    };
    let pos = format!("{}:{}", editor.cursor_y + 1, editor.cursor_x + 1);
    let lsp_label = match &editor.lsp_status {
        crate::lsp::LspStatus::Starting(name) => Some((short_lsp(name), theme.mode_command)),
        crate::lsp::LspStatus::Ready(name) => Some((short_lsp(name), theme.mode_insert)),
        crate::lsp::LspStatus::Error(_) => Some(("LSP!".to_string(), theme.diag_error)),
        crate::lsp::LspStatus::NotFound(_) => Some(("no-lsp".to_string(), theme.mode_command)),
        crate::lsp::LspStatus::Disabled => None,
    };
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
    let chips: Vec<(String, HitAction, bool, Color)> = if narrow {
        vec![
            (
                format!("{FOLDER} Files"),
                HitAction::ToggleExplorer,
                editor.explorer.visible,
                theme.mode_normal,
            ),
            (
                format!("{DIAG_ERROR} Err"),
                HitAction::ToggleProblems,
                editor.problems_open,
                theme.diag_error,
            ),
            (
                format!("{CMD_SYMBOLS} Sym"),
                HitAction::ToggleOutline,
                editor.outline_open,
                theme.syn_function,
            ),
            (
                format!("{PALETTE} Cmd"),
                HitAction::OpenPalette,
                false,
                theme.mode_command,
            ),
        ]
    } else {
        vec![
            (
                format!("{FOLDER} Files"),
                HitAction::ToggleExplorer,
                editor.explorer.visible,
                theme.mode_normal,
            ),
            (
                format!("{DIAG_ERROR} Problems"),
                HitAction::ToggleProblems,
                editor.problems_open,
                theme.diag_warn,
            ),
            (
                format!("{CMD_SYMBOLS} Outline"),
                HitAction::ToggleOutline,
                editor.outline_open,
                theme.syn_function,
            ),
            (
                format!("{LSP_READY} LSP"),
                HitAction::OpenLsp,
                false,
                theme.mode_insert,
            ),
            (
                format!("{THEME} Theme"),
                HitAction::OpenTheme,
                false,
                theme.mode_visual,
            ),
            (
                format!("{LINE_WRAP} Wrap"),
                HitAction::ToggleWrap,
                editor.line_wrap,
                theme.mode_insert,
            ),
            (
                format!("{HINTS_ON} Hints"),
                HitAction::ToggleHints,
                editor.show_inlay_hints,
                theme.syn_function,
            ),
            (
                format!("{PALETTE} Cmd"),
                HitAction::OpenPalette,
                false,
                theme.mode_command,
            ),
        ]
    };
    let mode_label = format!(" {mode} ");
    let pos_label = format!(" {pos} ");
    let mut right_w = cols(&mode_label) + cols(&pos_label);
    let lsp_text = lsp_label
        .as_ref()
        .map(|(s, _)| clip_cols(s, if narrow { 8 } else { 14 }));
    if let Some(s) = &lsp_text {
        right_w += cols(s) + 1;
    }
    if error_count > 0 {
        right_w += cols(&format!("{error_count}")) + 3;
    }
    if warn_count > 0 {
        right_w += cols(&format!("{warn_count}")) + 3;
    }
    let right_w = (right_w as u16).min(deck.width.saturating_sub(8));
    let left_w = deck.width.saturating_sub(right_w);
    let mut x = deck.x;
    let left_end = deck.x + left_w;
    for (label, action, on, color) in &chips {
        let w = (cols(label) + 1).max(5) as u16;
        if x + w > left_end {
            break;
        }
        let cell = Rect::new(x, deck.y, w, 1);
        let style = if *on {
            Style::default()
                .fg(*color)
                .bg(theme.explorer_sel_bg)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.status_fg).bg(theme.status_bg)
        };
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(fit_cols(label, w as usize), style))),
            cell,
        );
        push_hit(editor, cell, action.clone());
        x += w;
    }
    if let Some(branch) = &editor.git_diff.branch {
        if x < left_end {
            let added = editor.git_diff.added_lines;
            let modified = editor.git_diff.modified_lines;
            let deleted = editor.git_diff.deleted_lines;
            let extra = if added + modified + deleted > 0 {
                format!(" +{added} ~{modified} -{deleted}")
            } else {
                String::new()
            };
            let text = format!(" {GIT_BRANCH} {branch}{extra}");
            let w = cols(&text).min((left_end - x) as usize) as u16;
            if w > 2 {
                frame.render_widget(
                    Paragraph::new(Line::from(Span::styled(
                        fit_cols(&text, w as usize),
                        Style::default()
                            .fg(theme.git_branch)
                            .bg(theme.status_bg)
                            .add_modifier(Modifier::BOLD),
                    ))),
                    Rect::new(x, deck.y, w, 1),
                );
            }
        }
    }
    let mut rx = deck.x + left_w;
    if error_count > 0 && rx < deck.right() {
        let text = format!(" {DIAG_ERROR}{error_count}");
        let w = cols(&text).min((deck.right() - rx) as usize) as u16;
        let cell = Rect::new(rx, deck.y, w, 1);
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                fit_cols(&text, w as usize),
                Style::default().fg(theme.diag_error).bg(theme.status_bg),
            ))),
            cell,
        );
        push_hit(editor, cell, HitAction::NextDiagnostic);
        rx += w;
    }
    if warn_count > 0 && rx < deck.right() {
        let text = format!(" {DIAG_WARN}{warn_count}");
        let w = cols(&text).min((deck.right() - rx) as usize) as u16;
        let cell = Rect::new(rx, deck.y, w, 1);
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                fit_cols(&text, w as usize),
                Style::default().fg(theme.diag_warn).bg(theme.status_bg),
            ))),
            cell,
        );
        push_hit(editor, cell, HitAction::NextDiagnostic);
        rx += w;
    }
    if let Some((label, color)) = lsp_text.zip(lsp_label.map(|(_, c)| c)) {
        if rx < deck.right() {
            let w = (cols(&label) + 1).min((deck.right() - rx) as usize) as u16;
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    fit_cols(&format!(" {label}"), w as usize),
                    Style::default().fg(color).bg(theme.status_bg),
                ))),
                Rect::new(rx, deck.y, w, 1),
            );
            rx += w;
        }
    }
    if rx < deck.right() {
        let rest = deck.right() - rx;
        let mode_w = cols(&mode_label).min(rest as usize) as u16;
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                fit_cols(&mode_label, mode_w as usize),
                Style::default()
                    .fg(Color::Rgb(15, 17, 22))
                    .bg(mode_color)
                    .add_modifier(Modifier::BOLD),
            ))),
            Rect::new(rx, deck.y, mode_w, 1),
        );
        rx += mode_w;
        if rx < deck.right() {
            let pos_w = deck.right() - rx;
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    fit_cols(&pos_label, pos_w as usize),
                    Style::default()
                        .fg(theme.mode_insert)
                        .bg(theme.status_bg)
                        .add_modifier(Modifier::BOLD),
                ))),
                Rect::new(rx, deck.y, pos_w, 1),
            );
        }
    }

    if narrow && (editor.explorer.visible || editor.problems_open || editor.outline_open) {
        let body = frame.area();
        let sheet = Rect::new(0, 0, body.width, deck.y.max(4));
        frame.render_widget(Clear, sheet);
        let (title, title_color) = if editor.explorer.visible {
            (format!(" {FOLDER} Files "), theme.mode_normal)
        } else if editor.problems_open {
            (format!(" {DIAG_ERROR} Problems "), theme.diag_warn)
        } else {
            (format!(" {CMD_SYMBOLS} Outline "), theme.syn_function)
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(theme.border_focused))
            .style(Style::default().bg(theme.explorer_bg))
            .title(Line::from(Span::styled(
                title,
                Style::default()
                    .fg(title_color)
                    .add_modifier(Modifier::BOLD),
            )));
        let inner = block.inner(sheet);
        frame.render_widget(block, sheet);
        let close = Rect::new(sheet.right().saturating_sub(6), sheet.y, 5, 2);
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "  x ",
                Style::default()
                    .fg(theme.diag_error)
                    .bg(theme.explorer_bg)
                    .add_modifier(Modifier::BOLD),
            ))),
            Rect::new(sheet.right().saturating_sub(5), sheet.y, 4, 1),
        );
        push_hit(editor, close, HitAction::CloseSheet);
        if editor.explorer.visible {
            editor.explorer.update_scroll(inner.height as usize);
            let start = editor.explorer.scroll;
            let end = (start + inner.height as usize).min(editor.explorer.entries.len());
            let rows: Vec<(usize, usize, &'static str, Color, String, bool)> = editor
                .explorer
                .entries
                .iter()
                .enumerate()
                .skip(start)
                .take(end.saturating_sub(start))
                .map(|(i, entry)| {
                    let (icon, color) = if entry.is_dir {
                        if entry.expanded {
                            (FOLDER_OPEN, theme.explorer_dir_expanded)
                        } else {
                            (FOLDER_CLOSED, theme.explorer_dir)
                        }
                    } else {
                        let (icon, color) = file_icon_and_color(Some(&entry.path));
                        (icon.trim(), color)
                    };
                    (
                        i,
                        entry.depth,
                        icon,
                        color,
                        entry.name.clone(),
                        i == editor.explorer.selected_idx,
                    )
                })
                .collect();
            let mut lines = Vec::new();
            for (offset, (i, depth, icon, color, name, selected)) in rows.iter().enumerate() {
                lines.push(explorer_line(
                    *depth,
                    icon,
                    *color,
                    name,
                    *selected,
                    inner.width as usize,
                    theme,
                ));
                push_hit(
                    editor,
                    Rect::new(inner.x, inner.y + offset as u16, inner.width, 1),
                    HitAction::ClickFile(*i),
                );
            }
            frame.render_widget(
                Paragraph::new(lines).style(Style::default().bg(theme.explorer_bg)),
                inner,
            );
        } else if editor.problems_open {
            let diags: Vec<(usize, String, Color, &'static str)> = editor
                .diagnostics
                .iter()
                .take(inner.height as usize)
                .enumerate()
                .map(|(i, d)| {
                    let (color, icon) = match d.severity {
                        1 => (theme.diag_error, DIAG_ERROR),
                        2 => (theme.diag_warn, DIAG_WARN),
                        3 => (theme.diag_info, DIAG_INFO),
                        _ => (theme.diag_hint, DIAG_HINT),
                    };
                    (
                        i,
                        format!("{}:{} {}", d.line + 1, d.col + 1, d.message),
                        color,
                        icon,
                    )
                })
                .collect();
            let mut lines = Vec::new();
            for (i, text, color, icon) in &diags {
                let icon_style = Style::default().fg(*color).bg(theme.explorer_bg);
                let name_style = Style::default().fg(theme.explorer_fg).bg(theme.explorer_bg);
                let label = clip_cols(text, (inner.width as usize).saturating_sub(cols(icon) + 2));
                lines.push(pad_line(
                    vec![
                        Span::styled(format!(" {icon} "), icon_style),
                        Span::styled(label, name_style),
                    ],
                    inner.width as usize,
                    name_style,
                ));
                push_hit(
                    editor,
                    Rect::new(inner.x, inner.y + *i as u16, inner.width, 1),
                    HitAction::ClickProblem(*i),
                );
            }
            if lines.is_empty() {
                lines.push(Line::from(Span::styled(
                    " no problems",
                    Style::default().fg(theme.mode_insert),
                )));
            }
            frame.render_widget(
                Paragraph::new(lines).style(Style::default().bg(theme.explorer_bg)),
                inner,
            );
        } else {
            let tags: Vec<(usize, String)> = editor
                .syntax
                .tags
                .iter()
                .take(inner.height as usize)
                .enumerate()
                .map(|(i, tag)| (i, tag.name.clone()))
                .collect();
            let mut lines = Vec::new();
            for (i, name) in &tags {
                let icon_style = Style::default()
                    .fg(theme.syn_function)
                    .bg(theme.explorer_bg);
                let name_style = Style::default().fg(theme.explorer_fg).bg(theme.explorer_bg);
                let label = clip_cols(
                    name,
                    (inner.width as usize).saturating_sub(cols(KIND_FUNCTION) + 2),
                );
                lines.push(pad_line(
                    vec![
                        Span::styled(format!(" {KIND_FUNCTION} "), icon_style),
                        Span::styled(label, name_style),
                    ],
                    inner.width as usize,
                    name_style,
                ));
                push_hit(
                    editor,
                    Rect::new(inner.x, inner.y + *i as u16, inner.width, 1),
                    HitAction::ClickOutline(*i),
                );
            }
            if lines.is_empty() {
                lines.push(Line::from(Span::styled(
                    " no symbols",
                    Style::default().fg(theme.line_number),
                )));
            }
            frame.render_widget(
                Paragraph::new(lines).style(Style::default().bg(theme.explorer_bg)),
                inner,
            );
        }
    }
}
