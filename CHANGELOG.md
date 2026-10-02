## Unreleased

- Fixed completion acceptance so auto-imports apply against the original buffer and the cursor lands at the end of the inserted text.
- LSP `didOpen` always closes the previous document first, including the same URI, and keeps the editor document version.
- Stale definition, reference, highlight, and semantic-token responses are ignored unless the request id matches.
- Diagnostic ranges keep both ends when lines are inserted or deleted. Folds are skipped by `j`/`k` and cleared on file open.
- Git hunk text no longer treats a trailing newline as an extra line. Config and buffer saves use unique atomic temp files.
- Tree-sitter indent, fold, tag, and textobject queries are compiled once per process.
- Fixture files under `tests/` are no longer compiled as Cargo integration tests. Added regression coverage for edits, snippets, and diffs.

# Changelog
- Static grammars reduced to Rust, C, Python, JavaScript, TypeScript, TSX, Go, JSON, HTML, Markdown, and Bash. C++ uses C until `s0 --grammar install cpp`.
- Execute all Tree-sitter query files (highlights, locals, injections, indents, textobjects, rainbows, tags, folds) with `;; inherits:` resolution.
- Per-language grammar install (`s0 --grammar install <lang>`), not a fetch-all. Static grammars stay built in.
- LSP: declaration, type definition, implementation, document highlight, range formatting, workspace symbols, pull diagnostics.

# Changelog

All notable changes to `subject0` will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.1.0-alpha.1] - 2026-09-15

### Added
- **Command-Line Interface (`src/cmd.rs`)**:
  - Full CLI flag parser supporting `-h`/`--help`, `-v`/`--version`, `-w`/`--wrap`, `-nw`/`--no-wrap`, and `--clean`.
  - Line jump support using editor syntax (`+<line>`, e.g., `s0 src/main.rs +42`).
  - Directory target support (`s0 .` or `s0 path/to/dir`) that automatically mounts and focuses the File Explorer sidebar.
- **Themed Terminal Cards**:
  - Truecolor, ANSI-aware boxed cards for `--help` and `--version` output with dynamic character width calculation and padding for exact border alignment.
- **In-Editor Interactive Help Modal**:
  - Modal keybinding reference popup toggled via `?` in Normal mode, `:h`, `:help`, or the Command Palette, featuring smooth vertical scrolling (`j`/`k`, `PageDown`/`PageUp`).
- **Multi-Language LSP Intelligence**:
  - Dynamic discovery and execution for Rust (`rust-analyzer`), Go (`gopls`), Python (`pyright-langserver`, `pyright`, `pylsp`), HTML, CSS, and JavaScript/TypeScript language servers.
  - Interactive selection modal when multiple candidate servers are discovered on the host system.
  - Telemetry status bar indicators with animated Braille spinner (`Starting`), ready checks (`Ready`), and error reports.
- **Persistent Configuration**:
  - Automated configuration management writing to `.subject0` in workspace roots and falling back to `~/.subject0`.

### Fixed
- **Soft-Wrap Delayed Viewport Scroll**: Corrected visual sub-row calculations so vertical scrolling triggers immediately when the cursor touches the bottom screen row.
- **Pyright Integration**: Added required `--stdio` execution flags and implemented LSP `textEdit` parsing.
- **Autocomplete Trigger Hygiene**: Removed autocomplete invocations from bracket pairs (`{`, `(`, `[`) and restored standard 4-space indentation on `Tab`.
- **Completion Dropdown Alignment**: Anchored autocomplete popup screen coordinates to match actual cursor screen positions across wrapped lines.
- **Clipboard & Edit Operations**:
  - Cut operations (`dd`, `x`) now populate the internal clipboard for `p` paste actions.
  - Corrected cursor coordinate translation following multi-line clipboard pastes.
  - Fixed orphaned carriage return (`\r`) issues when joining lines or deleting line breaks in CRLF files.
  - Added indentation replication for `O` (`insert_line_above`) and preserved line boundaries in `o` (`insert_line_below`).
- **Explorer Tree Traversal**: Resolved index-out-of-bounds panics when collapsing expanded subtrees.
- **Diagnostic Scoping**: Restricted diagnostic annotations strictly to the active document URI.
