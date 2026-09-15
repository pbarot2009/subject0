# Changelog

All notable changes to `subject0` will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.1.0-dev] - 2026-09-15

### Added
- **Multi-Language LSP Discovery**: Dynamic background discovery and launching for Rust (`rust-analyzer`), Go (`gopls`), Python (`pyright`, `pyright-langserver`, `pylsp`), HTML, CSS, and JavaScript/TypeScript language servers.
- **Interactive LSP Selection Modal**: Automatically detects installed candidate servers and prompts the user to select their preference when multiple options exist.
- **Persistent Configuration (`.subject0`)**: Local workspace and home directory configuration storing preferred language servers and line-wrap preferences.
- **Live LSP Lifecycle Indicators**: Real-time status bar telemetry with animated Braille spinner (`Starting`), verification check (`Ready`), missing binary warnings, and error indicators.
- **Extended Command Palette Actions**: Integrated `:lsp` for changing servers on the fly and `:cfg` for writing editor settings.

### Fixed
- **Delayed Viewport Scrolling**: Resolved issue where soft line-wrapping desynchronized logical buffer line indices from terminal screen rows, delaying scroll updates.
- **Pyright Integration**: Added mandatory `--stdio` flags for `pyright-langserver`, unified background outbound Tokio channels, and implemented LSP `textEdit` parsing alongside standard `insertText`.
- **Autocomplete Triggers**: Prevented unwanted completion popups on structural delimiters (`{`, `(`, `[`) and restored soft 4-space tab behavior on `Tab`.
- **Popup Anchor Coordinate Desync**: Rewrote screen-space coordinate translation so floating completion popups stay pinned to the cursor on wrapped lines.
- **Buffer Data Loss on Save/Quit**: Prevented `:wq` and `SaveQuit` from exiting without saving when buffers are unnamed, and enabled saving named paths via `:w <filename>`.
- **Clipboard & Motion Inconsistencies**:
  - Configured `dd` and `x` to populate the internal clipboard for subsequent `p` operations.
  - Corrected 2D coordinate calculations after multi-line paste operations.
  - Eliminated swallowed keystrokes when canceling or breaking pending key chords (`d`, `g`).
  - Added leading indentation preservation to `insert_line_above` (`O`) and fixed delimiter splicing in `insert_line_below` (`o`).
  - Handled multi-byte CRLF (`\r\n`) endings during line joins (`J`) and backspaces.
- **File Explorer Collapse Panic**: Fixed index-out-of-bounds panics when collapsing active subdirectories.
- **Cross-File Diagnostic Leaks**: Filtered incoming LSP `textDocument/publishDiagnostics` messages strictly against the active buffer URI.
- **Compiler & Clippy Compliance**: Resolved all pedantic clippy warnings, doc formatting requirements, and pattern-match warnings.

