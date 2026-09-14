# subject0 (s0)

A modal terminal code editor built in Rust with native Language Server Protocol (LSP) integration and Tree-sitter syntax highlighting.

## Architecture

`subject0` uses an asynchronous actor-based architecture to isolate buffer editing from language server operations:

- **Text Storage**: Powered by `ropey::Rope`, a B-tree data structure providing O(log N) insertions, deletions, and non-blocking snapshotting for the undo history.
- **LSP Subsystem**: Runs as an isolated Tokio background actor communicating with the editor via unbounded MPSC channels. Implements JSON-RPC 2.0 framing over standard I/O for `textDocument/didOpen`, `didChange` (full sync), `didSave`, and `completion`.
- **Syntax Engine**: Combines Tree-sitter parsing with a single-pass lexical scanner to produce styled `ratatui::text::Span` lines without frame drops during rapid scrolling.
- **Terminal Control**: Driven by `crossterm` and `ratatui`. Hardware cursor shapes (steady block vs. thin bar) switch dynamically using DECSCUSR ANSI escape codes.

## Prerequisites

- **Rust**: 1.74 or later (2021 edition).
- **Nerd Font**: Required to render file icons and completion glyphs properly.
- **Language Servers** (optional, discovered in `$PATH` or `~/.cargo/bin`):
  - Rust: `rust-analyzer`
  - Python: `pylsp`

## Installation

Clone the repository and build the release binary:

```bash
git clone https://github.com/pbarot2009/subject0.git
cd subject0
cargo build --release
```

The compiled binary will be located at `target/release/s0`. Copy it to a directory in your `$PATH`:

```bash
cp target/release/s0 ~/.local/bin/
```

## Usage

```bash
s0 [FILE_PATH]
```

If no path is provided, an empty scratch buffer opens.

## Keybindings

### Normal Mode

| Key | Action |
| --- | --- |
| `i` / `I` | Enter Insert mode (at cursor / at line start) |
| `a` / `A` | Append (after cursor / at line end) |
| `o` / `O` | Insert line below / above |
| `v` | Enter Visual mode |
| `h`, `j`, `k`, `l` | Move cursor left, down, up, right |
| `0` / `$` | Move cursor to line start / line end |
| `gg` / `G` | Jump to top / bottom of document |
| `gh` / `gl` | Jump to line start / line end |
| `d d` | Delete current line |
| `x` | Delete character under cursor |
| `u` | Undo last modification |
| `y` | Yank current line |
| `p` | Paste clipboard contents |
| `~` | Toggle character case |
| `J` | Join current line with next line |
| `%` | Select entire buffer |
| `Space` | Open Command Palette |
| `:` | Enter Command mode |
| `Ctrl-E` | Toggle File Explorer sidebar |

### Insert Mode

| Key | Action |
| --- | --- |
| `Esc` | Return to Normal mode |
| `Tab` | Open completions if available, or insert 4 spaces |
| `Ctrl-Space` | Trigger LSP completions manually |
| `Down` / `Up` | Navigate autocomplete suggestions |
| `Enter` / `Tab` | Accept highlighted completion candidate |
| `(`, `[`, `{`, `"`, `'` | Automatically insert matching delimiter pairs |
| `Backspace` | Delete character or delete matching delimiter pair |

### Visual Mode

| Key | Action |
| --- | --- |
| `Esc` | Cancel selection, return to Normal mode |
| `d` / `x` | Delete selected text |
| `c` | Delete selected text and enter Insert mode |
| `y` | Yank selected text |
| `~` | Toggle case over selected range |
| `%` | Expand selection to entire buffer |

### Command Mode (`:`)

| Command | Action |
| --- | --- |
| `:w` | Write buffer to disk |
| `:q` | Quit (aborts if there are unsaved changes) |
| `:q!` | Force quit, discarding unsaved changes |
| `:wq` | Write buffer to disk and quit |
| `:wrap` | Toggle viewport soft line wrapping |
| `:e`, `:explore` | Toggle File Explorer sidebar |
| `:p`, `:menu` | Open Command Palette |

## Development

Check code formatting and run linter checks:

```bash
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
```

Verify documentation builds cleanly:

```bash
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps
```

## License

Dual-licensed under either the MIT License or Apache License, Version 2.0 at your option.
