//! # Command-Line Argument Parser
//!
//! Handles command-line arguments, flag decoding (`--help`, `--version`, `--clean`),
//! jump-to-line specifiers (`+<line>`), and directory/file path resolution.

use std::{env, path::PathBuf, process};

/// Parsed command-line arguments.
#[derive(Debug, Default, Clone)]
pub struct CliArgs {
    /// Path to target file or directory.
    pub path: Option<PathBuf>,
    /// Optional target line number to jump to on launch (1-based).
    pub jump_line: Option<usize>,
    /// Optional override for soft line wrapping.
    pub line_wrap: Option<bool>,
    /// When true, ignore `.subject0` configuration file.
    pub ignore_config: bool,
}

impl CliArgs {
    /// Parses CLI arguments from standard environment args.
    ///
    /// Intercepts `--help` and `--version` to print directly to stdout and exit
    /// before terminal raw mode or alternate screen buffers are initialized.
    pub fn parse() -> Self {
        let raw_args: Vec<String> = env::args().skip(1).collect();
        let mut cli = Self::default();

        for arg in raw_args {
            match arg.as_str() {
                "-h" | "--help" => {
                    Self::print_help();
                    process::exit(0);
                }
                "-v" | "-V" | "--version" => {
                    Self::print_version();
                    process::exit(0);
                }
                "--clean" | "--no-config" => {
                    cli.ignore_config = true;
                }
                "-w" | "--wrap" => {
                    cli.line_wrap = Some(true);
                }
                "-nw" | "--no-wrap" => {
                    cli.line_wrap = Some(false);
                }
                // Handle editor line-jump syntax (e.g., +42 or +100)
                s if s.starts_with('+') => {
                    if let Ok(line_num) = s[1..].parse::<usize>() {
                        cli.jump_line = Some(line_num);
                    }
                }
                // Positional file or directory target
                s if !s.starts_with('-') && cli.path.is_none() => {
                    cli.path = Some(PathBuf::from(s));
                }
                _ => {}
            }
        }

        cli
    }

    fn print_version() {
        println!("subject0 (s0) v{}", env!("CARGO_PKG_VERSION"));
    }

    fn print_help() {
        println!(
            "subject0 (s0) v{} - Modal terminal code editor with LSP and Tree-sitter

USAGE:
    s0 [OPTIONS] [PATH] [+LINE]

ARGUMENTS:
    [PATH]         File or directory to open (scratch buffer if omitted)
    [+LINE]        Jump directly to line number (e.g., +25)

OPTIONS:
    -h, --help        Print this help message and exit
    -v, --version     Print version information and exit
    -w, --wrap        Force enable viewport line wrapping
    -nw, --no-wrap    Force disable viewport line wrapping
    --clean           Ignore local and user .subject0 configuration files

EXAMPLES:
    s0 src/main.rs          Open src/main.rs
    s0 src/main.rs +45      Open src/main.rs and jump to line 45
    s0 .                    Open current directory in file explorer sidebar
    s0 --clean              Open scratch buffer bypassing saved preferences",
            env!("CARGO_PKG_VERSION")
        );
    }
}
