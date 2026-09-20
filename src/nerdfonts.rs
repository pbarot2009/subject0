//! # Nerd Font Glyphs & Icon Registry
//!
//! Centralized repository of all Nerd Font icons, symbols, and UI glyphs used
//! across `subject0`. Encapsulates icons for file extensions, LSP statuses, diagnostics,
//! document symbols, autocompletion kinds, command palette actions, powerline segment
//! dividers, and future extensible developer tooling (Git, terminal, debug, etc.).

#![allow(dead_code)]

// === Powerline Dividers & UI Shapes ===

/// Solid right-facing Powerline arrow separator (`\u{e0b0}`).
pub const POWERLINE_RIGHT: &str = "";
/// Solid left-facing Powerline arrow separator (`\u{e0b2}`).
pub const POWERLINE_LEFT: &str = "";
/// Rounded right-facing Powerline separator (`\u{e0b4}`).
pub const POWERLINE_RIGHT_ROUND: &str = "";
/// Rounded left-facing Powerline separator (`\u{e0b6}`).
pub const POWERLINE_LEFT_ROUND: &str = "";

/// Standard right-pointing chevron indicator.
pub const CHEVRON_RIGHT: &str = "󰅂";
/// Left-pointing navigation arrow.
pub const ARROW_LEFT: &str = "󰁍";
/// Right-pointing navigation arrow.
pub const ARROW_RIGHT: &str = "󰁔";
/// Upward navigation arrow.
pub const ARROW_UP: &str = "󰗈";
/// Downward navigation arrow.
pub const ARROW_DOWN: &str = "󰗈";
/// Soft line wrap return gutter arrow.
pub const LINE_WRAP: &str = "↳";
/// Unsaved buffer modification marker.
pub const MODIFIED_DOT: &str = "●";

// === Status, Diagnostics & Feedback ===

/// Checkmark indicating successful compilation, check, or active toggle.
pub const CHECK: &str = "󰄬";
/// Close cross / cancel indicator.
pub const CLOSE: &str = "󰅖";
/// Generic error circle indicator.
pub const ERROR: &str = "󰅚";
/// Warning triangle indicator.
pub const WARNING: &str = "";
/// Informational indicator.
pub const INFO: &str = "󰌵";
/// Compiler hint indicator.
pub const HINT: &str = "󰌵";
/// Lightbulb representing quickfixes, code actions, and inferred hints.
pub const LIGHTBULB: &str = "󰌵";
/// Question / help symbol.
pub const HELP: &str = "󰋖";
/// Documentation / hover card info icon.
pub const INFO_DOC: &str = "󰋽";
/// Octagon indicating an uninstalled or missing binary.
pub const MISSING: &str = "󰚌";
/// Medical stethoscope representing system/LSP health doctor diagnostics.
pub const HEALTH: &str = "󰆉";

// Compiler diagnostic badges
/// Diagnostic severity error badge.
pub const DIAG_ERROR: &str = "";
/// Diagnostic severity warning badge.
pub const DIAG_WARN: &str = "";
/// Diagnostic severity info badge.
pub const DIAG_INFO: &str = "󰌵";
/// Diagnostic severity hint badge.
pub const DIAG_HINT: &str = "󰌵";

/// Padded diagnostic error badge for gutters and status rows.
pub const DIAG_ERROR_PAD: &str = " ";
/// Padded diagnostic warning badge for gutters and status rows.
pub const DIAG_WARN_PAD: &str = " ";
/// Padded diagnostic info badge for gutters and status rows.
pub const DIAG_INFO_PAD: &str = "󰌵 ";
/// Padded diagnostic hint badge for gutters and status rows.
pub const DIAG_HINT_PAD: &str = "󰌵 ";

// LSP Process state badges
/// Server ready badge.
pub const LSP_READY: &str = "󰄬";
/// Server error badge.
pub const LSP_ERROR: &str = "󰅚";
/// Server not found badge.
pub const LSP_NOT_FOUND: &str = "󰄰";

/// Braille loading animation sequence used for async server handshakes.
pub const SPINNER: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

// === Explorer & Filesystem ===

/// Folder icon (standard).
pub const FOLDER: &str = "󰉓";
/// Folder icon (outline).
pub const FOLDER_OUTLINE: &str = "󰉒";
/// Collapsed directory node icon.
pub const FOLDER_CLOSED: &str = "";
/// Expanded directory node icon.
pub const FOLDER_OPEN: &str = "";

/// Generic document / file icon.
pub const FILE_DOCUMENT: &str = "󰈙";
/// Generic document outline icon.
pub const FILE_OUTLINE: &str = "󰈚";
/// Default fallback file icon.
pub const FILE_GENERIC: &str = "󰈔";
/// Generic source code file icon.
pub const FILE_CODE: &str = "󰈙";
/// Compiled binary file icon.
pub const FILE_BINARY: &str = "󰡯";

// === Editor Modes & Control Badges ===

/// Palette / symbol search icon.
pub const PALETTE: &str = "󰍉";
/// Color theme switcher icon.
pub const THEME: &str = "󰔎";
/// Active cursor position crosshairs.
pub const LOCATION: &str = "󰆤";
/// Settings / LSP configuration icon.
pub const SETTINGS_COGS: &str = "";
/// General configuration file icon.
pub const GEAR_CONFIG: &str = "󰄛";

/// Line wrap enabled icon.
pub const WRAP_ON: &str = "󰖶";
/// Line wrap disabled icon.
pub const WRAP_OFF: &str = "󰖵";
/// Inlay hints enabled icon.
pub const HINTS_ON: &str = "󰌵";
/// Inlay hints disabled icon.
pub const HINTS_OFF: &str = "󰌶";

// === Command Palette Actions ===

pub const CMD_FORMAT: &str = "󰉠";
pub const CMD_HOVER: &str = "󰋽";
pub const CMD_CODE_ACTIONS: &str = "󰌵";
pub const CMD_DEFINITION: &str = "󰌹";
pub const CMD_REFERENCES: &str = "󰌷";
pub const CMD_RENAME: &str = "󰑕";
pub const CMD_SYMBOLS: &str = "󰅩";
pub const CMD_RESTART: &str = "󰑐";
pub const CMD_SAVE: &str = "󰆓";
pub const CMD_SAVE_QUIT: &str = "󰆘";
pub const CMD_QUIT: &str = "󰈆";
pub const CMD_VISUAL: &str = "󰒅";
pub const CMD_INSERT_BELOW: &str = "󰙍";
pub const CMD_INSERT_ABOVE: &str = "󰙌";
pub const CMD_JOIN: &str = "󰘥";
pub const CMD_UNDO: &str = "󰑌";
pub const CMD_REDO: &str = "󰑎";
pub const CMD_YANK: &str = "󰅍";
pub const CMD_PASTE: &str = "󰅒";
pub const CMD_TOGGLE_CASE: &str = "󰬴";
pub const CMD_JUMP_TOP: &str = "󰗈";
pub const CMD_JUMP_BOTTOM: &str = "󰗈";

// === LSP Autocompletion & Symbol Kinds ===

pub const KIND_FUNCTION: &str = "󰊕";
pub const KIND_METHOD: &str = "󰊕";
pub const KIND_CONSTRUCTOR: &str = "󰊕";
pub const KIND_CLASS: &str = "󰌗";
pub const KIND_STRUCT: &str = "󰌗";
pub const KIND_ENUM: &str = "󰌗";
pub const KIND_INTERFACE: &str = "󱡠";
pub const KIND_VARIABLE: &str = "󱡠";
pub const KIND_FIELD: &str = "󰫧";
pub const KIND_PROPERTY: &str = "󰫧";
pub const KIND_PACKAGE: &str = "󰏗";
pub const KIND_MODULE: &str = "󰏗";
pub const KIND_KEYWORD: &str = "󰌆";
pub const KIND_CONSTANT: &str = "󰌆";
pub const KIND_STRING: &str = "󰈙";
pub const KIND_NUMBER: &str = "󰎠";
pub const KIND_BOOLEAN: &str = "󰨚";
pub const KIND_OPERATOR: &str = "󰊕";
pub const KIND_TYPE_PARAM: &str = "󰌗";
pub const KIND_FILE: &str = "󰅩";
pub const KIND_NAMESPACE: &str = "󰅲";
pub const KIND_ARRAY: &str = "󰅲";
pub const KIND_DEFAULT: &str = "󰈚";

// === Programming Language File Extension Glyphs ===

pub const FILE_RUST: &str = "";
pub const FILE_PYTHON: &str = "";
pub const FILE_GO: &str = "";
pub const FILE_ZIG: &str = "";
pub const FILE_C: &str = "";
pub const FILE_CPP: &str = "";
pub const FILE_JAVASCRIPT: &str = "";
pub const FILE_TYPESCRIPT: &str = "";
pub const FILE_HTML: &str = "";
pub const FILE_CSS: &str = "";
pub const FILE_JSON: &str = "";
pub const FILE_TOML: &str = "";
pub const FILE_YAML: &str = "";
pub const FILE_SHELL: &str = "";
pub const FILE_LUA: &str = "";
pub const FILE_MARKDOWN: &str = "";
pub const FILE_JAVA: &str = "";
pub const FILE_CSHARP: &str = "󰌛";
pub const FILE_PHP: &str = "";
pub const FILE_RUBY: &str = "";
pub const FILE_KOTLIN: &str = "";
pub const FILE_SWIFT: &str = "";
pub const FILE_DART: &str = "";
pub const FILE_SQL: &str = "";
pub const FILE_SCALA: &str = "";
pub const FILE_ODIN: &str = "";

// === Extra Developer Tooling Glyphs ===

pub const GIT_BRANCH: &str = "";
pub const GIT_COMMIT: &str = "󰜘";
pub const GIT_DIFF_ADDED: &str = "󰐕";
pub const GIT_DIFF_REMOVED: &str = "󰍴";
pub const GIT_DIFF_MODIFIED: &str = "󰝤";
pub const GIT_REPO: &str = "";

pub const TOOL_DOCKER: &str = "";
pub const TOOL_DATABASE: &str = "󰆼";
pub const TOOL_TERMINAL: &str = "";
pub const TOOL_DEBUG: &str = "󰃤";
pub const TOOL_LOCK: &str = "󰌾";
pub const TOOL_PLAY: &str = "󰐊";
pub const TOOL_STOP: &str = "󰓛";
pub const TOOL_SEARCH: &str = "󰍉";
pub const TOOL_LINK: &str = "󰌷";

pub const LANG_HASKELL: &str = "";
pub const LANG_ELIXIR: &str = "";
pub const LANG_ERLANG: &str = "";
pub const LANG_OCAML: &str = "";
pub const LANG_CLOJURE: &str = "";
pub const LANG_JULIA: &str = "";
pub const LANG_NIM: &str = "󰆥";
pub const LANG_NIX: &str = "";
pub const LANG_VUE: &str = "󰡄";
pub const LANG_SVELTE: &str = "";
pub const LANG_R: &str = "󰟔";
pub const LANG_PERL: &str = "";
pub const LANG_TERRAFORM: &str = "󱁢";
pub const LANG_GRAPHQL: &str = "󰡋";
pub const LANG_XML: &str = "󰗀";
pub const LANG_VIM: &str = "";
pub const LANG_LATEX: &str = "";
pub const LANG_TYPST: &str = "";
