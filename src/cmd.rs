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
        let r = "\x1b[0m";
        let b = "\x1b[1m";
        let blue = "\x1b[38;2;100;180;255m";
        let green = "\x1b[38;2;100;200;140m";
        let yellow = "\x1b[38;2;240;200;90m";
        let gray = "\x1b[38;2;140;145;160m";
        let white = "\x1b[38;2;225;230;240m";
        let ver = env!("CARGO_PKG_VERSION");

        let lines = vec![
            format!(" {blue}󰈙{r} {b}{white}subject0{r} {gray}(s0){r}  {green}v{ver}{r}"),
            format!(" {gray}Modal terminal code editor with LSP intelligence{r}"),
            String::new(),
            format!(" {yellow}Author:{r}   {white}Prathmesh S. Barot{r}"),
            format!(" {yellow}License:{r}  {white}MIT OR Apache-2.0{r}"),
            format!(" {yellow}Source:{r}   {blue}https://github.com/pbarot2009/subject0{r}"),
        ];

        Self::print_boxed_card(&lines, 56);
    }

    fn print_help() {
        let r = "\x1b[0m";
        let b = "\x1b[1m";
        let blue = "\x1b[38;2;100;180;255m";
        let green = "\x1b[38;2;100;200;140m";
        let yellow = "\x1b[38;2;240;200;90m";
        let magenta = "\x1b[38;2;220;110;240m";
        let gray = "\x1b[38;2;140;145;160m";
        let white = "\x1b[38;2;225;230;240m";
        let ver = env!("CARGO_PKG_VERSION");

        let header = vec![format!(
            " {blue}󰈙{r} {b}{white}subject0{r} {gray}(s0){r} {green}v{ver}{r} {gray}— Terminal Modal Code Editor{r}"
        )];

        Self::print_boxed_card(&header, 62);

        println!(
            "
  {b}{blue}󰅂 USAGE:{r}
      {white}s0{r} {yellow}[OPTIONS]{r} {green}[PATH]{r} {magenta}[+LINE]{r}

  {b}{blue}󰅂 ARGUMENTS:{r}
      {green}[PATH]{r}             File or folder path {gray}(opens scratch buffer if empty){r}
      {magenta}[+LINE]{r}            Jump directly to line number {gray}(e.g. +42){r}

  {b}{blue}󰅂 OPTIONS:{r}
      {yellow}-h, --help{r}        Show this formatted help menu and exit
      {yellow}-v, --version{r}     Print version information and metadata
      {yellow}-w, --wrap{r}        Force soft line wrapping on
      {yellow}-nw, --no-wrap{r}    Force line wrapping off (horizontal scroll)
      {yellow}--clean{r}           Bypass workspace and user {gray}.subject0{r} configs

  {b}{blue}󰅂 EXAMPLES:{r}
      {gray}# Open a file at line 50:{r}
      {white}s0 src/main.rs +50{r}

      {gray}# Open project directory in the sidebar explorer:{r}
      {white}s0 .{r}

      {gray}# Launch scratch buffer with no configuration:{r}
      {white}s0 --clean{r}"
        );
    }

    /// Renders a bordered card with dynamic padding, guaranteeing 100% border alignment.
    fn print_boxed_card(lines: &[String], min_width: usize) {
        let r = "\x1b[0m";
        let border = "\x1b[38;2;70;75;95m";

        let max_content_width = lines
            .iter()
            .map(|l| Self::visible_width(l))
            .max()
            .unwrap_or(0);
        let inner_width = max_content_width.max(min_width);

        // Top border
        println!("{border}╭{}╮{r}", "─".repeat(inner_width + 2));

        // Content rows with calculated padding
        for line in lines {
            let line_w = Self::visible_width(line);
            let pad = inner_width.saturating_sub(line_w);
            println!("{border}│{r} {line}{}{border} │{r}", " ".repeat(pad));
        }

        // Bottom border
        println!("{border}╰{}╯{r}", "─".repeat(inner_width + 2));
    }

    /// Computes printable character width by ignoring non-printing ANSI SGR escape codes.
    fn visible_width(s: &str) -> usize {
        let mut width = 0;
        let mut in_escape = false;
        for c in s.chars() {
            if c == '\x1b' {
                in_escape = true;
            } else if in_escape {
                if c.is_ascii_alphabetic() {
                    in_escape = false;
                }
            } else {
                width += 1;
            }
        }
        width
    }
}
