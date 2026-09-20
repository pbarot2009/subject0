//! # Terminal UI Render Pipeline & Floating Modals Subsystem
//!
//! Handles layout structuring, double-buffering frames via Ratatui, syntax token
//! coloring, inlay hints styling, gutter diagnostic indicators, floating autocomplete,
//! and modals (hover cards, quickfixes, symbol outlines, pickers, and command palette).

use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Position, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Paragraph},
};

use crate::editor::{Editor, Focus, Mode};
use crate::lsp::{InlayHintType, LspStatus, utf16_to_char_col};
use crate::syntax::{completion_kind_icon, file_icon_and_color, symbol_kind_icon};

use crate::nerdfonts::{
    BOX_HORIZONTAL, BOX_VERTICAL, CHECK, CHEVRON_RIGHT, CMD_RENAME, CMD_SYMBOLS, CURSOR_BLOCK,
    DIAG_ERROR, DIAG_ERROR_PAD, DIAG_HINT, DIAG_HINT_PAD, DIAG_INFO, DIAG_INFO_PAD, DIAG_WARN,
    DIAG_WARN_PAD, ELLIPSIS, FOLDER, FOLDER_CLOSED, FOLDER_OPEN, FOLDER_OUTLINE, GUTTER_EMPTY,
    HELP, HINTS_OFF, HINTS_ON, INFO_DOC, KIND_FUNCTION, LIGHTBULB, LINE_WRAP, LOCATION, LSP_ERROR,
    LSP_NOT_FOUND, LSP_READY, MODIFIED_DOT, PALETTE, POWERLINE_LEFT, POWERLINE_RIGHT,
    PROMPT_CHEVRON, PROMPT_COLON, SELECTION_BLANK, SETTINGS_COGS, SPINNER, THEME, TILDE, TOOL_LINK,
    WRAP_OFF, WRAP_ON,
};

/// Safely truncates a string by Unicode scalar count without slicing mid-codepoint.
pub fn safe_truncate(s: &str, max_chars: usize) -> String {
    if s.chars().count() > max_chars {
        let mut result: String = s.chars().take(max_chars.saturating_sub(1)).collect();
        result.push_str(ELLIPSIS);
        result
    } else {
        s.to_string()
    }
}

/// Primary UI rendering entry point dispatched on every event loop redraw tick.
pub fn render_ui(frame: &mut Frame, editor: &mut Editor) {
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
            let is_sel = i == editor.explorer.selected_idx;
            let indent = "  ".repeat(entry.depth);

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

            let item_style = if is_sel {
                Style::default()
                    .bg(theme.explorer_sel_bg)
                    .fg(theme.explorer_sel_fg)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme.explorer_fg)
            };

            tree_lines.push(Line::from(vec![
                Span::raw(" "),
                Span::raw(indent),
                Span::styled(format!("{raw_icon} "), Style::default().fg(icon_color)),
                Span::styled(entry.name.clone(), item_style),
            ]));
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
    let gutter_width = line_digits + 5;
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
            Some(1) => (DIAG_ERROR_PAD, Style::default().fg(theme.diag_error)),
            Some(2) => (DIAG_WARN_PAD, Style::default().fg(theme.diag_warn)),
            Some(3) => (DIAG_INFO_PAD, Style::default().fg(theme.diag_info)),
            Some(_) => (DIAG_HINT_PAD, Style::default().fg(theme.diag_hint)),
            None => (GUTTER_EMPTY, Style::default()),
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
                let (marker, gutter_str, g_style) = if is_first_sub {
                    (
                        diag_marker,
                        format!("{:>width$} {BOX_VERTICAL} ", y + 1, width = line_digits),
                        gutter_style,
                    )
                } else {
                    (
                        GUTTER_EMPTY,
                        format!("{:>width$} {LINE_WRAP} ", "", width = line_digits),
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

                visible_lines.push(Line::from(sub_spans));
                current_row += 1;
                chunk_start = chunk_end;
                is_first_sub = false;
            }
        } else {
            let (marker, gutter_str, g_style) = (
                diag_marker,
                format!("{:>width$} {BOX_VERTICAL} ", y + 1, width = line_digits),
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

            visible_lines.push(Line::from(row_spans));
            current_row += 1;
        }
    }

    for _ in (current_row as usize)..inner_area.height as usize {
        visible_lines.push(Line::from(vec![
            Span::raw(GUTTER_EMPTY),
            Span::styled(
                format!("{:>width$} {BOX_VERTICAL} ", TILDE, width = line_digits),
                Style::default().fg(theme.line_number),
            ),
        ]));
    }

    frame.render_widget(Paragraph::new(visible_lines), inner_area);

    // 3. Render Statusline
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
        format!(" {FOLDER} Files ")
    } else {
        format!(" {FOLDER_OUTLINE} Files ")
    };
    let wrap_badge = if editor.line_wrap {
        format!(" {WRAP_ON} Wrap ")
    } else {
        format!(" {WRAP_OFF} NoWrap ")
    };
    let hints_badge = if editor.show_inlay_hints {
        format!(" {HINTS_ON} Hints ")
    } else {
        format!(" {HINTS_OFF} Hints ")
    };

    let status_left = Line::from(vec![
        Span::styled(
            badge_text,
            Style::default()
                .bg(badge_color)
                .fg(Color::Rgb(15, 17, 22))
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            POWERLINE_RIGHT,
            Style::default().bg(pill_bg).fg(badge_color),
        ),
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
            hints_badge,
            Style::default().bg(pill_bg).fg(if editor.show_inlay_hints {
                theme.syn_function
            } else {
                theme.line_number
            }),
        ),
        Span::styled(
            format!(" {PALETTE} Cmd "),
            Style::default().bg(pill_bg).fg(theme.mode_command),
        ),
        Span::styled(POWERLINE_RIGHT, Style::default().bg(bar_bg).fg(pill_bg)),
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
                format!(" {LSP_READY} {name} "),
                Style::default().bg(bar_bg).fg(theme.mode_insert),
            ));
        }
        LspStatus::Error(err) => {
            status_right_spans.push(Span::styled(
                format!(" {LSP_ERROR} LSP: {err} "),
                Style::default()
                    .bg(bar_bg)
                    .fg(theme.diag_error)
                    .add_modifier(Modifier::BOLD),
            ));
        }
        LspStatus::NotFound(name) => {
            status_right_spans.push(Span::styled(
                format!(" {LSP_NOT_FOUND} {name} missing "),
                Style::default().bg(bar_bg).fg(theme.mode_command),
            ));
        }
        LspStatus::Disabled => {}
    }

    if error_count > 0 {
        status_right_spans.push(Span::styled(
            format!(" {DIAG_ERROR} {error_count} "),
            Style::default().bg(bar_bg).fg(theme.diag_error),
        ));
    }
    if warn_count > 0 {
        status_right_spans.push(Span::styled(
            format!(" {DIAG_WARN} {warn_count} "),
            Style::default().bg(bar_bg).fg(theme.diag_warn),
        ));
    }

    status_right_spans.extend(vec![
        Span::styled(POWERLINE_LEFT, Style::default().bg(bar_bg).fg(pill_bg)),
        Span::styled(
            format!(" {THEME} {} ", theme.display_name),
            Style::default().bg(pill_bg).fg(bar_foreground),
        ),
        Span::styled(POWERLINE_LEFT, Style::default().bg(pill_bg).fg(badge_color)),
        Span::styled(
            format!(
                " {LOCATION} {}:{} ",
                editor.cursor_y + 1,
                editor.cursor_x + 1
            ),
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

    // 4. Command Bar & Status Messages
    let (screen_x, screen_y) =
        cursor_screen_pos.unwrap_or((inner_area.x + gutter_width as u16, inner_area.y));

    if editor.mode == Mode::Command {
        let prompt_line = Line::from(vec![
            Span::styled(
                format!(" {PROMPT_COLON}"),
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
                Span::styled(d_icon, icon_style),
                Span::styled(
                    &diag.message,
                    Style::default().fg(theme.fg).add_modifier(Modifier::ITALIC),
                ),
            ])
        } else {
            Line::from(vec![
                Span::styled(
                    format!(" {CHEVRON_RIGHT} "),
                    Style::default().fg(theme.line_number),
                ),
                Span::styled(&editor.status_msg, Style::default().fg(theme.status_fg)),
            ])
        };

        frame.render_widget(Paragraph::new(msg_line), main_chunks[2]);

        if editor.focus == Focus::Editor
            && let Some((cx, cy)) = cursor_screen_pos
            && cx < inner_area.right()
            && cy < inner_area.bottom()
        {
            frame.set_cursor_position(Position::new(cx, cy));
        }
    }

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
    }

    // 9. Symbol Outline Picker Modal
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
    }

    // 10. Location Picker Modal (Definitions / References)
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
    }

    // 11. Rename Symbol Prompt Modal
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
    }

    // 15. Render In-Editor Help Modal
    if editor.show_help {
        let width = 66u16.min(size.width.saturating_sub(4));
        let height = 24u16.min(size.height.saturating_sub(2));
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
