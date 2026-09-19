//! # Theme Engine & Visual Styles
//!
//! Provides 10 complete, curated themes with unified color schemes across the
//! text buffer, syntax tokens, file explorer, statusline, modal popups,
//! and full Language Server Protocol (LSP) intelligence elements (inlay hints,
//! hover documentation cards, signature helpers, and diagnostics).

use ratatui::style::Color;

/// Complete styling specification for all UI components, syntax tokens, and LSP features.
#[derive(Clone, Copy, Debug)]
#[allow(dead_code)]
pub struct Theme {
    pub name: &'static str,
    pub display_name: &'static str,

    // Base UI
    pub bg: Color,
    pub fg: Color,
    pub border: Color,
    pub border_focused: Color,
    pub cursor_line_bg: Color,
    pub selection_bg: Color,
    pub selection_fg: Color,
    pub line_number: Color,
    pub line_number_active: Color,

    // Statusline
    pub status_bg: Color,
    pub status_fg: Color,
    pub status_pill_bg: Color,
    pub mode_normal: Color,
    pub mode_insert: Color,
    pub mode_visual: Color,
    pub mode_command: Color,

    // Sidebar Explorer
    pub explorer_bg: Color,
    pub explorer_fg: Color,
    pub explorer_sel_bg: Color,
    pub explorer_sel_fg: Color,
    pub explorer_dir: Color,
    pub explorer_dir_expanded: Color,

    // Popups, Command Palette & Autocomplete
    pub popup_bg: Color,
    pub popup_border: Color,
    pub popup_sel_bg: Color,
    pub popup_sel_fg: Color,
    pub popup_text: Color,

    // Syntax Highlighting Tokens
    pub syn_keyword: Color,
    pub syn_type: Color,
    pub syn_function: Color,
    pub syn_string: Color,
    pub syn_number: Color,
    pub syn_comment: Color,
    pub syn_macro: Color,
    pub syn_operator: Color,
    pub syn_namespace: Color,
    pub syn_tag: Color,
    pub syn_variable: Color,
    pub syn_parameter: Color,
    pub syn_property: Color,

    // LSP Intelligence: Inlay Hints, Hover Documentation, Signature Help & Diagnostics
    pub inlay_hint_fg: Color,
    pub inlay_hint_bg: Color,
    pub inlay_hint_param_fg: Color,
    pub hover_bg: Color,
    pub hover_border: Color,
    pub hover_fg: Color,
    pub hover_code_bg: Color,
    pub signature_active_param: Color,
    pub diag_error: Color,
    pub diag_warn: Color,
    pub diag_info: Color,
    pub diag_hint: Color,
}

impl Theme {
    /// Retrieves a theme by its canonical identifier, falling back to `gruber-darker`.
    pub fn from_name(name: &str) -> Self {
        Self::all()
            .iter()
            .copied()
            .find(|t| t.name.eq_ignore_ascii_case(name))
            .unwrap_or_else(Self::gruber_darker)
    }

    /// Registry of all 10 available themes.
    pub fn all() -> &'static [Theme] {
        const THEMES: &[Theme] = &[
            Theme::gruber_darker(),
            Theme::tokyo_night(),
            Theme::catppuccin_mocha(),
            Theme::gruvbox_dark(),
            Theme::nord(),
            Theme::one_dark(),
            Theme::dracula(),
            Theme::rose_pine(),
            Theme::kanagawa(),
            Theme::monokai_pro(),
        ];
        THEMES
    }

    // 1. Gruber Darker (Iconic Steve Losh / Alexander Gruber palette)
    pub const fn gruber_darker() -> Self {
        Self {
            name: "gruber-darker",
            display_name: "Gruber Darker",
            bg: Color::Rgb(24, 24, 24),
            fg: Color::Rgb(228, 228, 228),
            border: Color::Rgb(40, 40, 40),
            border_focused: Color::Rgb(255, 221, 51),
            cursor_line_bg: Color::Rgb(30, 30, 30),
            selection_bg: Color::Rgb(72, 72, 72),
            selection_fg: Color::Rgb(255, 255, 255),
            line_number: Color::Rgb(82, 82, 82),
            line_number_active: Color::Rgb(255, 221, 51),

            status_bg: Color::Rgb(18, 18, 18),
            status_fg: Color::Rgb(228, 228, 228),
            status_pill_bg: Color::Rgb(40, 40, 40),
            mode_normal: Color::Rgb(255, 221, 51),   // Yellow
            mode_insert: Color::Rgb(115, 201, 54),   // Green
            mode_visual: Color::Rgb(150, 166, 200),  // Niagara Blue
            mode_command: Color::Rgb(158, 149, 199), // Wisteria Purple

            explorer_bg: Color::Rgb(20, 20, 20),
            explorer_fg: Color::Rgb(180, 180, 180),
            explorer_sel_bg: Color::Rgb(48, 48, 48),
            explorer_sel_fg: Color::Rgb(255, 221, 51),
            explorer_dir: Color::Rgb(255, 221, 51),
            explorer_dir_expanded: Color::Rgb(244, 180, 26),

            popup_bg: Color::Rgb(20, 20, 20),
            popup_border: Color::Rgb(255, 221, 51),
            popup_sel_bg: Color::Rgb(52, 52, 52),
            popup_sel_fg: Color::Rgb(255, 255, 255),
            popup_text: Color::Rgb(228, 228, 228),

            syn_keyword: Color::Rgb(255, 221, 51), // Amber/Yellow
            syn_type: Color::Rgb(149, 169, 159),   // Quartz Gray/Green
            syn_function: Color::Rgb(150, 166, 200), // Niagara Blue
            syn_string: Color::Rgb(115, 201, 54),  // Green
            syn_number: Color::Rgb(149, 169, 159), // Quartz
            syn_comment: Color::Rgb(150, 80, 75),  // Brown/Muted Red
            syn_macro: Color::Rgb(255, 221, 51),   // Yellow
            syn_operator: Color::Rgb(228, 228, 228), // White
            syn_namespace: Color::Rgb(150, 166, 200),
            syn_tag: Color::Rgb(244, 56, 65), // Red
            syn_variable: Color::Rgb(228, 228, 228),
            syn_parameter: Color::Rgb(149, 169, 159),
            syn_property: Color::Rgb(158, 149, 199), // Wisteria Purple

            inlay_hint_fg: Color::Rgb(115, 115, 115),
            inlay_hint_bg: Color::Rgb(32, 32, 32),
            inlay_hint_param_fg: Color::Rgb(149, 169, 159),
            hover_bg: Color::Rgb(20, 20, 20),
            hover_border: Color::Rgb(255, 221, 51),
            hover_fg: Color::Rgb(228, 228, 228),
            hover_code_bg: Color::Rgb(30, 30, 30),
            signature_active_param: Color::Rgb(255, 221, 51),
            diag_error: Color::Rgb(244, 56, 65),
            diag_warn: Color::Rgb(255, 221, 51),
            diag_info: Color::Rgb(150, 166, 200),
            diag_hint: Color::Rgb(149, 169, 159),
        }
    }

    // 2. Tokyo Night
    pub const fn tokyo_night() -> Self {
        Self {
            name: "tokyo-night",
            display_name: "Tokyo Night",
            bg: Color::Rgb(26, 27, 38),
            fg: Color::Rgb(192, 202, 245),
            border: Color::Rgb(41, 46, 66),
            border_focused: Color::Rgb(122, 162, 247),
            cursor_line_bg: Color::Rgb(31, 35, 53),
            selection_bg: Color::Rgb(54, 76, 126),
            selection_fg: Color::Rgb(255, 255, 255),
            line_number: Color::Rgb(75, 82, 114),
            line_number_active: Color::Rgb(224, 175, 104),

            status_bg: Color::Rgb(22, 22, 30),
            status_fg: Color::Rgb(169, 177, 214),
            status_pill_bg: Color::Rgb(36, 40, 59),
            mode_normal: Color::Rgb(122, 162, 247),
            mode_insert: Color::Rgb(158, 206, 106),
            mode_visual: Color::Rgb(187, 154, 247),
            mode_command: Color::Rgb(224, 175, 104),

            explorer_bg: Color::Rgb(22, 22, 30),
            explorer_fg: Color::Rgb(169, 177, 214),
            explorer_sel_bg: Color::Rgb(41, 46, 66),
            explorer_sel_fg: Color::Rgb(122, 162, 247),
            explorer_dir: Color::Rgb(122, 162, 247),
            explorer_dir_expanded: Color::Rgb(187, 154, 247),

            popup_bg: Color::Rgb(22, 22, 30),
            popup_border: Color::Rgb(122, 162, 247),
            popup_sel_bg: Color::Rgb(41, 46, 66),
            popup_sel_fg: Color::Rgb(255, 255, 255),
            popup_text: Color::Rgb(192, 202, 245),

            syn_keyword: Color::Rgb(187, 154, 247),
            syn_type: Color::Rgb(42, 195, 222),
            syn_function: Color::Rgb(122, 162, 247),
            syn_string: Color::Rgb(158, 206, 106),
            syn_number: Color::Rgb(255, 158, 100),
            syn_comment: Color::Rgb(86, 95, 137),
            syn_macro: Color::Rgb(125, 207, 255),
            syn_operator: Color::Rgb(137, 221, 255),
            syn_namespace: Color::Rgb(125, 207, 255),
            syn_tag: Color::Rgb(247, 118, 142),
            syn_variable: Color::Rgb(192, 202, 245),
            syn_parameter: Color::Rgb(224, 175, 104),
            syn_property: Color::Rgb(115, 218, 202),

            inlay_hint_fg: Color::Rgb(86, 95, 137),
            inlay_hint_bg: Color::Rgb(31, 35, 53),
            inlay_hint_param_fg: Color::Rgb(115, 218, 202),
            hover_bg: Color::Rgb(22, 22, 30),
            hover_border: Color::Rgb(122, 162, 247),
            hover_fg: Color::Rgb(192, 202, 245),
            hover_code_bg: Color::Rgb(31, 35, 53),
            signature_active_param: Color::Rgb(224, 175, 104),
            diag_error: Color::Rgb(247, 118, 142),
            diag_warn: Color::Rgb(224, 175, 104),
            diag_info: Color::Rgb(122, 162, 247),
            diag_hint: Color::Rgb(115, 218, 202),
        }
    }

    // 3. Catppuccin Mocha
    pub const fn catppuccin_mocha() -> Self {
        Self {
            name: "catppuccin-mocha",
            display_name: "Catppuccin Mocha",
            bg: Color::Rgb(30, 30, 46),
            fg: Color::Rgb(205, 214, 244),
            border: Color::Rgb(49, 50, 68),
            border_focused: Color::Rgb(137, 180, 250),
            cursor_line_bg: Color::Rgb(49, 50, 68),
            selection_bg: Color::Rgb(88, 91, 112),
            selection_fg: Color::Rgb(205, 214, 244),
            line_number: Color::Rgb(108, 112, 134),
            line_number_active: Color::Rgb(249, 226, 175),

            status_bg: Color::Rgb(24, 24, 37),
            status_fg: Color::Rgb(166, 173, 200),
            status_pill_bg: Color::Rgb(49, 50, 68),
            mode_normal: Color::Rgb(137, 180, 250),
            mode_insert: Color::Rgb(166, 227, 161),
            mode_visual: Color::Rgb(203, 166, 247),
            mode_command: Color::Rgb(249, 226, 175),

            explorer_bg: Color::Rgb(24, 24, 37),
            explorer_fg: Color::Rgb(186, 194, 222),
            explorer_sel_bg: Color::Rgb(69, 71, 90),
            explorer_sel_fg: Color::Rgb(137, 180, 250),
            explorer_dir: Color::Rgb(137, 180, 250),
            explorer_dir_expanded: Color::Rgb(203, 166, 247),

            popup_bg: Color::Rgb(24, 24, 37),
            popup_border: Color::Rgb(137, 180, 250),
            popup_sel_bg: Color::Rgb(69, 71, 90),
            popup_sel_fg: Color::Rgb(205, 214, 244),
            popup_text: Color::Rgb(205, 214, 244),

            syn_keyword: Color::Rgb(203, 166, 247),
            syn_type: Color::Rgb(249, 226, 175),
            syn_function: Color::Rgb(137, 180, 250),
            syn_string: Color::Rgb(166, 227, 161),
            syn_number: Color::Rgb(250, 179, 135),
            syn_comment: Color::Rgb(108, 112, 134),
            syn_macro: Color::Rgb(245, 194, 231),
            syn_operator: Color::Rgb(148, 226, 213),
            syn_namespace: Color::Rgb(180, 190, 254),
            syn_tag: Color::Rgb(243, 139, 168),
            syn_variable: Color::Rgb(205, 214, 244),
            syn_parameter: Color::Rgb(235, 160, 172),
            syn_property: Color::Rgb(137, 220, 235),

            inlay_hint_fg: Color::Rgb(108, 112, 134),
            inlay_hint_bg: Color::Rgb(38, 38, 56),
            inlay_hint_param_fg: Color::Rgb(148, 226, 213),
            hover_bg: Color::Rgb(24, 24, 37),
            hover_border: Color::Rgb(137, 180, 250),
            hover_fg: Color::Rgb(205, 214, 244),
            hover_code_bg: Color::Rgb(49, 50, 68),
            signature_active_param: Color::Rgb(249, 226, 175),
            diag_error: Color::Rgb(243, 139, 168),
            diag_warn: Color::Rgb(249, 226, 175),
            diag_info: Color::Rgb(137, 180, 250),
            diag_hint: Color::Rgb(148, 226, 213),
        }
    }

    // 4. Gruvbox Dark
    pub const fn gruvbox_dark() -> Self {
        Self {
            name: "gruvbox-dark",
            display_name: "Gruvbox Dark",
            bg: Color::Rgb(40, 40, 40),
            fg: Color::Rgb(235, 219, 178),
            border: Color::Rgb(60, 56, 54),
            border_focused: Color::Rgb(250, 189, 47),
            cursor_line_bg: Color::Rgb(50, 48, 47),
            selection_bg: Color::Rgb(80, 73, 69),
            selection_fg: Color::Rgb(235, 219, 178),
            line_number: Color::Rgb(124, 111, 100),
            line_number_active: Color::Rgb(250, 189, 47),

            status_bg: Color::Rgb(29, 32, 33),
            status_fg: Color::Rgb(213, 196, 161),
            status_pill_bg: Color::Rgb(60, 56, 54),
            mode_normal: Color::Rgb(131, 165, 152),
            mode_insert: Color::Rgb(184, 187, 38),
            mode_visual: Color::Rgb(211, 134, 155),
            mode_command: Color::Rgb(250, 189, 47),

            explorer_bg: Color::Rgb(29, 32, 33),
            explorer_fg: Color::Rgb(213, 196, 161),
            explorer_sel_bg: Color::Rgb(80, 73, 69),
            explorer_sel_fg: Color::Rgb(250, 189, 47),
            explorer_dir: Color::Rgb(250, 189, 47),
            explorer_dir_expanded: Color::Rgb(254, 128, 25),

            popup_bg: Color::Rgb(29, 32, 33),
            popup_border: Color::Rgb(250, 189, 47),
            popup_sel_bg: Color::Rgb(80, 73, 69),
            popup_sel_fg: Color::Rgb(251, 241, 199),
            popup_text: Color::Rgb(235, 219, 178),

            syn_keyword: Color::Rgb(251, 73, 52),
            syn_type: Color::Rgb(250, 189, 47),
            syn_function: Color::Rgb(184, 187, 38),
            syn_string: Color::Rgb(184, 187, 38),
            syn_number: Color::Rgb(211, 134, 155),
            syn_comment: Color::Rgb(146, 131, 116),
            syn_macro: Color::Rgb(142, 192, 124),
            syn_operator: Color::Rgb(254, 128, 25),
            syn_namespace: Color::Rgb(131, 165, 152),
            syn_tag: Color::Rgb(251, 73, 52),
            syn_variable: Color::Rgb(235, 219, 178),
            syn_parameter: Color::Rgb(131, 165, 152),
            syn_property: Color::Rgb(142, 192, 124),

            inlay_hint_fg: Color::Rgb(146, 131, 116),
            inlay_hint_bg: Color::Rgb(50, 48, 47),
            inlay_hint_param_fg: Color::Rgb(142, 192, 124),
            hover_bg: Color::Rgb(29, 32, 33),
            hover_border: Color::Rgb(250, 189, 47),
            hover_fg: Color::Rgb(235, 219, 178),
            hover_code_bg: Color::Rgb(50, 48, 47),
            signature_active_param: Color::Rgb(250, 189, 47),
            diag_error: Color::Rgb(251, 73, 52),
            diag_warn: Color::Rgb(250, 189, 47),
            diag_info: Color::Rgb(131, 165, 152),
            diag_hint: Color::Rgb(142, 192, 124),
        }
    }

    // 5. Nord
    pub const fn nord() -> Self {
        Self {
            name: "nord",
            display_name: "Nord",
            bg: Color::Rgb(46, 52, 64),
            fg: Color::Rgb(236, 239, 244),
            border: Color::Rgb(59, 66, 82),
            border_focused: Color::Rgb(136, 192, 208),
            cursor_line_bg: Color::Rgb(59, 66, 82),
            selection_bg: Color::Rgb(67, 76, 94),
            selection_fg: Color::Rgb(236, 239, 244),
            line_number: Color::Rgb(76, 86, 106),
            line_number_active: Color::Rgb(235, 203, 139),

            status_bg: Color::Rgb(36, 41, 51),
            status_fg: Color::Rgb(216, 222, 233),
            status_pill_bg: Color::Rgb(59, 66, 82),
            mode_normal: Color::Rgb(129, 161, 193),
            mode_insert: Color::Rgb(163, 190, 140),
            mode_visual: Color::Rgb(180, 142, 173),
            mode_command: Color::Rgb(235, 203, 139),

            explorer_bg: Color::Rgb(41, 46, 57),
            explorer_fg: Color::Rgb(216, 222, 233),
            explorer_sel_bg: Color::Rgb(67, 76, 94),
            explorer_sel_fg: Color::Rgb(136, 192, 208),
            explorer_dir: Color::Rgb(129, 161, 193),
            explorer_dir_expanded: Color::Rgb(136, 192, 208),

            popup_bg: Color::Rgb(36, 41, 51),
            popup_border: Color::Rgb(136, 192, 208),
            popup_sel_bg: Color::Rgb(67, 76, 94),
            popup_sel_fg: Color::Rgb(236, 239, 244),
            popup_text: Color::Rgb(236, 239, 244),

            syn_keyword: Color::Rgb(129, 161, 193),
            syn_type: Color::Rgb(143, 188, 187),
            syn_function: Color::Rgb(136, 192, 208),
            syn_string: Color::Rgb(163, 190, 140),
            syn_number: Color::Rgb(180, 142, 173),
            syn_comment: Color::Rgb(76, 86, 106),
            syn_macro: Color::Rgb(94, 129, 172),
            syn_operator: Color::Rgb(129, 161, 193),
            syn_namespace: Color::Rgb(143, 188, 187),
            syn_tag: Color::Rgb(191, 97, 106),
            syn_variable: Color::Rgb(216, 222, 233),
            syn_parameter: Color::Rgb(229, 233, 240),
            syn_property: Color::Rgb(236, 239, 244),

            inlay_hint_fg: Color::Rgb(94, 106, 130),
            inlay_hint_bg: Color::Rgb(59, 66, 82),
            inlay_hint_param_fg: Color::Rgb(143, 188, 187),
            hover_bg: Color::Rgb(36, 41, 51),
            hover_border: Color::Rgb(136, 192, 208),
            hover_fg: Color::Rgb(236, 239, 244),
            hover_code_bg: Color::Rgb(46, 52, 64),
            signature_active_param: Color::Rgb(235, 203, 139),
            diag_error: Color::Rgb(191, 97, 106),
            diag_warn: Color::Rgb(235, 203, 139),
            diag_info: Color::Rgb(136, 192, 208),
            diag_hint: Color::Rgb(143, 188, 187),
        }
    }

    // 6. One Dark
    pub const fn one_dark() -> Self {
        Self {
            name: "one-dark",
            display_name: "One Dark",
            bg: Color::Rgb(40, 44, 52),
            fg: Color::Rgb(171, 178, 191),
            border: Color::Rgb(62, 68, 81),
            border_focused: Color::Rgb(97, 175, 239),
            cursor_line_bg: Color::Rgb(44, 49, 58),
            selection_bg: Color::Rgb(62, 68, 81),
            selection_fg: Color::Rgb(255, 255, 255),
            line_number: Color::Rgb(92, 99, 112),
            line_number_active: Color::Rgb(229, 192, 123),

            status_bg: Color::Rgb(33, 37, 43),
            status_fg: Color::Rgb(171, 178, 191),
            status_pill_bg: Color::Rgb(49, 54, 63),
            mode_normal: Color::Rgb(97, 175, 239),
            mode_insert: Color::Rgb(152, 195, 121),
            mode_visual: Color::Rgb(198, 120, 221),
            mode_command: Color::Rgb(229, 192, 123),

            explorer_bg: Color::Rgb(33, 37, 43),
            explorer_fg: Color::Rgb(171, 178, 191),
            explorer_sel_bg: Color::Rgb(44, 49, 58),
            explorer_sel_fg: Color::Rgb(97, 175, 239),
            explorer_dir: Color::Rgb(97, 175, 239),
            explorer_dir_expanded: Color::Rgb(198, 120, 221),

            popup_bg: Color::Rgb(33, 37, 43),
            popup_border: Color::Rgb(97, 175, 239),
            popup_sel_bg: Color::Rgb(44, 49, 58),
            popup_sel_fg: Color::Rgb(255, 255, 255),
            popup_text: Color::Rgb(171, 178, 191),

            syn_keyword: Color::Rgb(198, 120, 221),
            syn_type: Color::Rgb(229, 192, 123),
            syn_function: Color::Rgb(97, 175, 239),
            syn_string: Color::Rgb(152, 195, 121),
            syn_number: Color::Rgb(209, 154, 102),
            syn_comment: Color::Rgb(92, 99, 112),
            syn_macro: Color::Rgb(86, 182, 194),
            syn_operator: Color::Rgb(86, 182, 194),
            syn_namespace: Color::Rgb(229, 192, 123),
            syn_tag: Color::Rgb(224, 108, 117),
            syn_variable: Color::Rgb(224, 108, 117),
            syn_parameter: Color::Rgb(171, 178, 191),
            syn_property: Color::Rgb(171, 178, 191),

            inlay_hint_fg: Color::Rgb(92, 99, 112),
            inlay_hint_bg: Color::Rgb(44, 49, 58),
            inlay_hint_param_fg: Color::Rgb(86, 182, 194),
            hover_bg: Color::Rgb(33, 37, 43),
            hover_border: Color::Rgb(97, 175, 239),
            hover_fg: Color::Rgb(171, 178, 191),
            hover_code_bg: Color::Rgb(40, 44, 52),
            signature_active_param: Color::Rgb(229, 192, 123),
            diag_error: Color::Rgb(224, 108, 117),
            diag_warn: Color::Rgb(229, 192, 123),
            diag_info: Color::Rgb(97, 175, 239),
            diag_hint: Color::Rgb(86, 182, 194),
        }
    }

    // 7. Dracula
    pub const fn dracula() -> Self {
        Self {
            name: "dracula",
            display_name: "Dracula",
            bg: Color::Rgb(40, 42, 54),
            fg: Color::Rgb(248, 248, 242),
            border: Color::Rgb(68, 71, 90),
            border_focused: Color::Rgb(189, 147, 249),
            cursor_line_bg: Color::Rgb(68, 71, 90),
            selection_bg: Color::Rgb(68, 71, 90),
            selection_fg: Color::Rgb(248, 248, 242),
            line_number: Color::Rgb(98, 114, 164),
            line_number_active: Color::Rgb(241, 250, 140),

            status_bg: Color::Rgb(33, 34, 44),
            status_fg: Color::Rgb(248, 248, 242),
            status_pill_bg: Color::Rgb(68, 71, 90),
            mode_normal: Color::Rgb(189, 147, 249),
            mode_insert: Color::Rgb(80, 250, 123),
            mode_visual: Color::Rgb(255, 121, 198),
            mode_command: Color::Rgb(241, 250, 140),

            explorer_bg: Color::Rgb(33, 34, 44),
            explorer_fg: Color::Rgb(248, 248, 242),
            explorer_sel_bg: Color::Rgb(68, 71, 90),
            explorer_sel_fg: Color::Rgb(189, 147, 249),
            explorer_dir: Color::Rgb(189, 147, 249),
            explorer_dir_expanded: Color::Rgb(255, 121, 198),

            popup_bg: Color::Rgb(33, 34, 44),
            popup_border: Color::Rgb(189, 147, 249),
            popup_sel_bg: Color::Rgb(68, 71, 90),
            popup_sel_fg: Color::Rgb(255, 255, 255),
            popup_text: Color::Rgb(248, 248, 242),

            syn_keyword: Color::Rgb(255, 121, 198),
            syn_type: Color::Rgb(139, 233, 253),
            syn_function: Color::Rgb(80, 250, 123),
            syn_string: Color::Rgb(241, 250, 140),
            syn_number: Color::Rgb(189, 147, 249),
            syn_comment: Color::Rgb(98, 114, 164),
            syn_macro: Color::Rgb(255, 184, 108),
            syn_operator: Color::Rgb(255, 121, 198),
            syn_namespace: Color::Rgb(189, 147, 249),
            syn_tag: Color::Rgb(255, 121, 198),
            syn_variable: Color::Rgb(248, 248, 242),
            syn_parameter: Color::Rgb(255, 184, 108),
            syn_property: Color::Rgb(102, 217, 239),

            inlay_hint_fg: Color::Rgb(98, 114, 164),
            inlay_hint_bg: Color::Rgb(50, 52, 66),
            inlay_hint_param_fg: Color::Rgb(139, 233, 253),
            hover_bg: Color::Rgb(33, 34, 44),
            hover_border: Color::Rgb(189, 147, 249),
            hover_fg: Color::Rgb(248, 248, 242),
            hover_code_bg: Color::Rgb(40, 42, 54),
            signature_active_param: Color::Rgb(241, 250, 140),
            diag_error: Color::Rgb(255, 85, 85),
            diag_warn: Color::Rgb(241, 250, 140),
            diag_info: Color::Rgb(139, 233, 253),
            diag_hint: Color::Rgb(80, 250, 123),
        }
    }

    // 8. Rose Pine
    pub const fn rose_pine() -> Self {
        Self {
            name: "rose-pine",
            display_name: "Rosé Pine",
            bg: Color::Rgb(25, 23, 36),
            fg: Color::Rgb(224, 222, 244),
            border: Color::Rgb(38, 35, 58),
            border_focused: Color::Rgb(196, 167, 231),
            cursor_line_bg: Color::Rgb(31, 29, 46),
            selection_bg: Color::Rgb(68, 65, 90),
            selection_fg: Color::Rgb(224, 222, 244),
            line_number: Color::Rgb(110, 106, 134),
            line_number_active: Color::Rgb(246, 193, 119),

            status_bg: Color::Rgb(20, 18, 28),
            status_fg: Color::Rgb(224, 222, 244),
            status_pill_bg: Color::Rgb(38, 35, 58),
            mode_normal: Color::Rgb(196, 167, 231),
            mode_insert: Color::Rgb(156, 207, 216),
            mode_visual: Color::Rgb(235, 188, 186),
            mode_command: Color::Rgb(246, 193, 119),

            explorer_bg: Color::Rgb(20, 18, 28),
            explorer_fg: Color::Rgb(224, 222, 244),
            explorer_sel_bg: Color::Rgb(42, 39, 63),
            explorer_sel_fg: Color::Rgb(196, 167, 231),
            explorer_dir: Color::Rgb(196, 167, 231),
            explorer_dir_expanded: Color::Rgb(235, 188, 186),

            popup_bg: Color::Rgb(20, 18, 28),
            popup_border: Color::Rgb(196, 167, 231),
            popup_sel_bg: Color::Rgb(42, 39, 63),
            popup_sel_fg: Color::Rgb(255, 255, 255),
            popup_text: Color::Rgb(224, 222, 244),

            syn_keyword: Color::Rgb(49, 116, 143),
            syn_type: Color::Rgb(156, 207, 216),
            syn_function: Color::Rgb(235, 188, 186),
            syn_string: Color::Rgb(246, 193, 119),
            syn_number: Color::Rgb(235, 111, 146),
            syn_comment: Color::Rgb(110, 106, 134),
            syn_macro: Color::Rgb(196, 167, 231),
            syn_operator: Color::Rgb(196, 167, 231),
            syn_namespace: Color::Rgb(196, 167, 231),
            syn_tag: Color::Rgb(235, 111, 146),
            syn_variable: Color::Rgb(224, 222, 244),
            syn_parameter: Color::Rgb(196, 167, 231),
            syn_property: Color::Rgb(156, 207, 216),

            inlay_hint_fg: Color::Rgb(110, 106, 134),
            inlay_hint_bg: Color::Rgb(35, 33, 48),
            inlay_hint_param_fg: Color::Rgb(156, 207, 216),
            hover_bg: Color::Rgb(20, 18, 28),
            hover_border: Color::Rgb(196, 167, 231),
            hover_fg: Color::Rgb(224, 222, 244),
            hover_code_bg: Color::Rgb(31, 29, 46),
            signature_active_param: Color::Rgb(246, 193, 119),
            diag_error: Color::Rgb(235, 111, 146),
            diag_warn: Color::Rgb(246, 193, 119),
            diag_info: Color::Rgb(156, 207, 216),
            diag_hint: Color::Rgb(196, 167, 231),
        }
    }

    // 9. Kanagawa
    pub const fn kanagawa() -> Self {
        Self {
            name: "kanagawa",
            display_name: "Kanagawa",
            bg: Color::Rgb(31, 31, 40),
            fg: Color::Rgb(220, 215, 186),
            border: Color::Rgb(42, 42, 55),
            border_focused: Color::Rgb(126, 156, 216),
            cursor_line_bg: Color::Rgb(42, 42, 55),
            selection_bg: Color::Rgb(45, 79, 103),
            selection_fg: Color::Rgb(220, 215, 186),
            line_number: Color::Rgb(84, 84, 109),
            line_number_active: Color::Rgb(255, 158, 100),

            status_bg: Color::Rgb(22, 22, 29),
            status_fg: Color::Rgb(197, 194, 171),
            status_pill_bg: Color::Rgb(42, 42, 55),
            mode_normal: Color::Rgb(126, 156, 216),
            mode_insert: Color::Rgb(152, 187, 108),
            mode_visual: Color::Rgb(149, 127, 184),
            mode_command: Color::Rgb(224, 159, 84),

            explorer_bg: Color::Rgb(22, 22, 29),
            explorer_fg: Color::Rgb(197, 194, 171),
            explorer_sel_bg: Color::Rgb(45, 79, 103),
            explorer_sel_fg: Color::Rgb(220, 215, 186),
            explorer_dir: Color::Rgb(126, 156, 216),
            explorer_dir_expanded: Color::Rgb(149, 127, 184),

            popup_bg: Color::Rgb(22, 22, 29),
            popup_border: Color::Rgb(126, 156, 216),
            popup_sel_bg: Color::Rgb(45, 79, 103),
            popup_sel_fg: Color::Rgb(255, 255, 255),
            popup_text: Color::Rgb(220, 215, 186),

            syn_keyword: Color::Rgb(149, 127, 184),
            syn_type: Color::Rgb(126, 156, 216),
            syn_function: Color::Rgb(126, 156, 216),
            syn_string: Color::Rgb(152, 187, 108),
            syn_number: Color::Rgb(210, 126, 90),
            syn_comment: Color::Rgb(114, 113, 105),
            syn_macro: Color::Rgb(228, 104, 114),
            syn_operator: Color::Rgb(192, 163, 110),
            syn_namespace: Color::Rgb(126, 156, 216),
            syn_tag: Color::Rgb(228, 104, 114),
            syn_variable: Color::Rgb(220, 215, 186),
            syn_parameter: Color::Rgb(184, 180, 160),
            syn_property: Color::Rgb(230, 195, 134),

            inlay_hint_fg: Color::Rgb(114, 113, 105),
            inlay_hint_bg: Color::Rgb(42, 42, 55),
            inlay_hint_param_fg: Color::Rgb(126, 156, 216),
            hover_bg: Color::Rgb(22, 22, 29),
            hover_border: Color::Rgb(126, 156, 216),
            hover_fg: Color::Rgb(220, 215, 186),
            hover_code_bg: Color::Rgb(31, 31, 40),
            signature_active_param: Color::Rgb(224, 159, 84),
            diag_error: Color::Rgb(228, 104, 114),
            diag_warn: Color::Rgb(224, 159, 84),
            diag_info: Color::Rgb(126, 156, 216),
            diag_hint: Color::Rgb(152, 187, 108),
        }
    }

    // 10. Monokai Pro
    pub const fn monokai_pro() -> Self {
        Self {
            name: "monokai-pro",
            display_name: "Monokai Pro",
            bg: Color::Rgb(45, 42, 46),
            fg: Color::Rgb(252, 252, 250),
            border: Color::Rgb(64, 61, 65),
            border_focused: Color::Rgb(255, 216, 102),
            cursor_line_bg: Color::Rgb(64, 61, 65),
            selection_bg: Color::Rgb(87, 83, 88),
            selection_fg: Color::Rgb(252, 252, 250),
            line_number: Color::Rgb(114, 112, 114),
            line_number_active: Color::Rgb(255, 216, 102),

            status_bg: Color::Rgb(34, 31, 34),
            status_fg: Color::Rgb(252, 252, 250),
            status_pill_bg: Color::Rgb(64, 61, 65),
            mode_normal: Color::Rgb(120, 220, 232),
            mode_insert: Color::Rgb(169, 220, 105),
            mode_visual: Color::Rgb(171, 157, 242),
            mode_command: Color::Rgb(255, 216, 102),

            explorer_bg: Color::Rgb(34, 31, 34),
            explorer_fg: Color::Rgb(193, 192, 192),
            explorer_sel_bg: Color::Rgb(64, 61, 65),
            explorer_sel_fg: Color::Rgb(255, 216, 102),
            explorer_dir: Color::Rgb(255, 216, 102),
            explorer_dir_expanded: Color::Rgb(255, 97, 136),

            popup_bg: Color::Rgb(34, 31, 34),
            popup_border: Color::Rgb(255, 216, 102),
            popup_sel_bg: Color::Rgb(64, 61, 65),
            popup_sel_fg: Color::Rgb(255, 255, 255),
            popup_text: Color::Rgb(252, 252, 250),

            syn_keyword: Color::Rgb(255, 97, 136),
            syn_type: Color::Rgb(120, 220, 232),
            syn_function: Color::Rgb(169, 220, 105),
            syn_string: Color::Rgb(255, 216, 102),
            syn_number: Color::Rgb(171, 157, 242),
            syn_comment: Color::Rgb(114, 112, 114),
            syn_macro: Color::Rgb(255, 97, 136),
            syn_operator: Color::Rgb(255, 97, 136),
            syn_namespace: Color::Rgb(120, 220, 232),
            syn_tag: Color::Rgb(255, 97, 136),
            syn_variable: Color::Rgb(252, 252, 250),
            syn_parameter: Color::Rgb(255, 157, 0),
            syn_property: Color::Rgb(120, 220, 232),

            inlay_hint_fg: Color::Rgb(114, 112, 114),
            inlay_hint_bg: Color::Rgb(55, 52, 56),
            inlay_hint_param_fg: Color::Rgb(120, 220, 232),
            hover_bg: Color::Rgb(34, 31, 34),
            hover_border: Color::Rgb(255, 216, 102),
            hover_fg: Color::Rgb(252, 252, 250),
            hover_code_bg: Color::Rgb(45, 42, 46),
            signature_active_param: Color::Rgb(255, 216, 102),
            diag_error: Color::Rgb(255, 97, 136),
            diag_warn: Color::Rgb(255, 216, 102),
            diag_info: Color::Rgb(120, 220, 232),
            diag_hint: Color::Rgb(169, 220, 105),
        }
    }
}
