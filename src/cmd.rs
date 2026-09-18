//! # Command-Line Argument Parser & Diagnostics Subsystem
//!
//! Handles command-line arguments, flag decoding (`--help`, `--version`, `--clean`),
//! jump-to-line specifiers (`+<line>`, `file:line:col`), path resolution across
//! Termux, Linux, macOS, and Windows, grammar inspection, and language health diagnostics.

use std::{
    env,
    path::{Path, PathBuf},
    process,
};

use crate::lsp::{resolve_binary_path, DynamicGrammar, SupportedLanguage};

/// Parsed command-line arguments.
#[derive(Debug, Default, Clone)]
pub struct CliArgs {
    /// Path to target file or directory.
    pub path: Option<PathBuf>,
    /// Optional target line number to jump to on launch (1-based).
    pub jump_line: Option<usize>,
    /// Optional target column number to jump to on launch (1-based).
    pub jump_col: Option<usize>,
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

        let force_grammar = raw_args
            .iter()
            .any(|a| a == "--force" || a == "--reinstall" || a == "-f");

        let mut idx = 0;
        let mut treat_as_positional = false;

        while idx < raw_args.len() {
            let arg = &raw_args[idx];

            if treat_as_positional {
                Self::assign_target_path(&mut cli, arg);
                idx += 1;
                continue;
            }

            match arg.as_str() {
                "--" => {
                    treat_as_positional = true;
                }
                "-h" | "--help" => {
                    Self::print_help();
                    process::exit(0);
                }
                "-v" | "-V" | "--version" => {
                    Self::print_version();
                    process::exit(0);
                }
                "-g" | "--install-grammar" => {
                    if idx + 1 < raw_args.len() {
                        let lang = raw_args[idx + 1].clone();
                        let clean_lang = Self::clean_lang_arg(&lang);
                        Self::run_grammar_installer(&clean_lang, force_grammar);
                        process::exit(0);
                    } else {
                        eprintln!("Error: Missing language argument for --install-grammar <lang>");
                        process::exit(1);
                    }
                }
                s if s.starts_with("--install-grammar=") => {
                    let raw_lang = s.trim_start_matches("--install-grammar=");
                    let clean_lang = Self::clean_lang_arg(raw_lang);
                    Self::run_grammar_installer(&clean_lang, force_grammar);
                    process::exit(0);
                }
                "-H" | "--health" | "--doctor" => {
                    Self::run_health_check();
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
                s if !s.starts_with('-') => {
                    Self::assign_target_path(&mut cli, s);
                }
                _ => {}
            }
            idx += 1;
        }

        cli
    }

    /// Strips enclosing quotes from argument flags.
    fn clean_lang_arg(raw: &str) -> String {
        raw.trim().trim_matches('\'').trim_matches('"').to_string()
    }

    /// Assigns file path and checks for `path:line:col` or `path:line` format.
    fn assign_target_path(cli: &mut Self, arg: &str) {
        if cli.path.is_some() {
            return;
        }

        let literal_path = PathBuf::from(arg);
        if literal_path.exists() {
            cli.path = Some(literal_path);
            return;
        }

        let parts: Vec<&str> = arg.rsplitn(3, ':').collect();
        if parts.len() == 3 {
            if let (Ok(line), Ok(col)) = (parts[1].parse::<usize>(), parts[0].parse::<usize>()) {
                let candidate = PathBuf::from(parts[2]);
                cli.path = Some(candidate);
                cli.jump_line = Some(line);
                cli.jump_col = Some(col);
                return;
            }
        } else if parts.len() == 2 {
            if let Ok(line) = parts[0].parse::<usize>() {
                let candidate = PathBuf::from(parts[1]);
                cli.path = Some(candidate);
                cli.jump_line = Some(line);
                return;
            }
        }

        cli.path = Some(literal_path);
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
            format!(
                " {gray}Modal terminal code editor with compile-time Tree-sitter & LSP engine{r}"
            ),
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
      {magenta}[+LINE]{r}            Jump directly to line number {gray}(e.g. +42 or path:42){r}

  {b}{blue}󰅂 OPTIONS:{r}
      {yellow}-h, --help{r}                Show this formatted help menu and exit
      {yellow}-v, --version{r}             Print version information and metadata
      {yellow}-g, --install-grammar <L>{r}  Inspect status of built-in Tree-sitter grammar
      {yellow}-H, --health, --doctor{r}    Show install status of every supported language
      {yellow}-w, --wrap{r}                Force soft line wrapping on
      {yellow}-nw, --no-wrap{r}            Force line wrapping off (horizontal scroll)
      {yellow}--clean{r}                   Bypass workspace and user {gray}.subject0{r} configs
      {yellow}--{r}                        Treat all subsequent arguments as positional paths

  {b}{blue}󰅂 EXAMPLES:{r}
      {gray}# Open a file at line 50:{r}
      {white}s0 src/main.rs +50{r}
      {white}s0 src/main.rs:50{r}

      {gray}# Open project directory in the sidebar explorer:{r}
      {white}s0 .{r}

      {gray}# Check syntax grammars and LSP servers across your system:{r}
      {white}s0 --health{r}

      {gray}# Check status of a built-in grammar:{r}
      {white}s0 -g rust{r}"
        );
    }

    /// Renders a bordered card with dynamic padding, guaranteeing border alignment.
    fn print_boxed_card(lines: &[String], min_width: usize) {
        let r = "\x1b[0m";
        let border = "\x1b[38;2;70;75;95m";

        let max_content_width = lines
            .iter()
            .map(|l| Self::visible_width(l))
            .max()
            .unwrap_or(0);
        let inner_width = max_content_width.max(min_width);

        println!("{border}╭{}╮{r}", "─".repeat(inner_width + 2));

        for line in lines {
            let line_w = Self::visible_width(line);
            let pad = inner_width.saturating_sub(line_w);
            println!("{border}│{r} {line}{}{border} │{r}", " ".repeat(pad));
        }

        println!("{border}╰{}╯{r}", "─".repeat(inner_width + 2));
    }

    /// Computes terminal display character width handling escape codes and double-width glyphs.
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
                let u = c as u32;
                let is_wide = (0x1100..=0x115F).contains(&u)
                    || (0x2E80..=0xA4CF).contains(&u)
                    || (0xAC00..=0xD7A3).contains(&u)
                    || (0xF900..=0xFAFF).contains(&u)
                    || (0xFE30..=0xFE6F).contains(&u)
                    || (0xFF00..=0xFF60).contains(&u)
                    || (0xFFE0..=0xFFE6).contains(&u)
                    || (0x1F300..=0x1F64F).contains(&u)
                    || (0x1F680..=0x1F6FF).contains(&u)
                    || (0xE000..=0xF8FF).contains(&u);

                width += if is_wide { 2 } else { 1 };
            }
        }
        width
    }

    fn shorten_home(path: &Path) -> String {
        if let Some(home) = dirs::home_dir() {
            let s_path = path.to_string_lossy();
            let s_home = home.to_string_lossy();
            if s_path.starts_with(&*s_home) {
                return s_path.replacen(&*s_home, "~", 1);
            }
        }
        path.to_string_lossy().to_string()
    }

    fn is_valid_grammar_name(name: &str) -> bool {
        !name.is_empty()
            && name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    }

    /// Inspects status of a Tree-sitter grammar.
    fn run_grammar_installer(lang: &str, _force: bool) {
        let r = "\x1b[0m";
        let b = "\x1b[1m";
        let green = "\x1b[38;2;100;200;140m";
        let yellow = "\x1b[38;2;240;200;90m";
        let red = "\x1b[38;2;240;90;90m";
        let blue = "\x1b[38;2;100;180;255m";
        let gray = "\x1b[38;2;140;145;160m";
        let white = "\x1b[38;2;225;230;240m";

        if !Self::is_valid_grammar_name(lang) {
            eprintln!(
                "{red}Error: Invalid grammar name '{lang}'. Only alphanumeric, '-' and '_' characters are permitted.{r}"
            );
            process::exit(1);
        }

        let canon_lang = match lang {
            "ts" => "typescript",
            "js" => "javascript",
            "sh" | "zsh" => "bash",
            "csharp" | "cs" => "c_sharp",
            "py" => "python",
            "rs" => "rust",
            "md" => "markdown",
            _ => lang,
        };

        let matched_lang = SupportedLanguage::all()
            .iter()
            .copied()
            .find(|l| l.grammar_name() == canon_lang || l.lsp_id() == canon_lang);

        if let Some(sl) = matched_lang {
            if sl.static_language().is_some() {
                let custom_query = DynamicGrammar::grammar_file_path(canon_lang);
                let query_status = if let Some(qp) = custom_query {
                    let disp = Self::shorten_home(&qp);
                    format!("{green}Custom override ({disp}){r}")
                } else {
                    format!("{green}Built-in standard queries{r}")
                };

                let lines = vec![
                    format!(" {green}󰄬 Built-in Static Grammar{r}"),
                    format!(" Language:  {b}{canon_lang}{r}"),
                    format!(" Tier:      {green}Statically linked (zero runtime overhead){r}"),
                    format!(" Queries:   {query_status}"),
                    format!(" Latency:   {blue}0 ms (Instantly ready){r}"),
                    String::new(),
                    format!(" {gray}No external compiler or runtime downloading is needed.{r}"),
                ];
                Self::print_boxed_card(&lines, 60);
                process::exit(0);
            }
        }

        let lines = vec![
            format!(" {yellow}󰌵 LSP Semantic Highlighting Tier{r}"),
            format!(" Language:  {b}{canon_lang}{r}"),
            format!(" Status:    {gray}No built-in Tree-sitter grammar{r}"),
            String::new(),
            format!(" {white}subject0 statically links grammars for top-tier languages:{r}"),
            format!(" {gray}Rust, C, C++, Python, JavaScript, TypeScript, Go, JSON,{r}"),
            format!(" {gray}TOML, YAML, Bash, HTML, CSS, Markdown, Java.{r}"),
            String::new(),
            format!(" {blue}Files for '{canon_lang}' receive full syntax intelligence via LSP.{r}"),
        ];
        Self::print_boxed_card(&lines, 62);
    }

    /// Prints a comprehensive health report showing static grammars and LSP servers.
    fn run_health_check() {
        let r = "\x1b[0m";
        let b = "\x1b[1m";
        let green = "\x1b[38;2;100;200;140m";
        let blue = "\x1b[38;2;100;180;255m";
        let gray = "\x1b[38;2;140;145;160m";
        let white = "\x1b[38;2;225;230;240m";
        let ver = env!("CARGO_PKG_VERSION");

        let ok = format!("{green}󰄬{r}");
        let missing = format!("{gray}󰚌{r}");

        let header = vec![format!(
            " {blue}󰆉{r} {b}{white}subject0{r} {gray}(s0){r} {green}v{ver}{r} {gray}— Language Support & Health{r}"
        )];
        Self::print_boxed_card(&header, 62);
        println!();

        let langs = SupportedLanguage::all();

        let name_w = langs
            .iter()
            .map(|l| l.display_name().chars().count())
            .max()
            .unwrap_or(8)
            .max("LANGUAGE".len());

        let candidate_max = langs
            .iter()
            .flat_map(|l| l.candidate_servers().iter())
            .map(|s| s.chars().count())
            .max()
            .unwrap_or(6);

        let server_w = candidate_max
            .max("LSP SERVER".len())
            .max("(none available)".len());

        let ast_hdr_w = 12;

        println!(
            "  {gray}{:<name_w$}  {:<ast_hdr_w$}  {:<server_w$}  {:<4}{r}",
            "LANGUAGE",
            "SYNTAX TIER",
            "LSP SERVER",
            "LSP",
            name_w = name_w,
            ast_hdr_w = ast_hdr_w,
            server_w = server_w
        );
        println!(
            "  {gray}{}  {}  {}  {}{r}",
            "─".repeat(name_w),
            "─".repeat(ast_hdr_w),
            "─".repeat(server_w),
            "─".repeat(4)
        );

        let mut static_count = 0usize;
        let mut lsp_count = 0usize;

        for lang in langs {
            let (ast_badge, ast_pad) = if lang.static_language().is_some() {
                static_count += 1;
                (format!("{green}󰄬 Static{r}"), 4)
            } else if lang.grammar_name().is_empty() {
                (format!("{gray}  ·{r}     "), 5)
            } else {
                (format!("{blue}󰌵 LSP{r}   "), 5)
            };

            let candidates = lang.candidate_servers();
            let installed_server = candidates
                .iter()
                .find(|cmd| resolve_binary_path(cmd).is_some());

            if installed_server.is_some() {
                lsp_count += 1;
            }

            let (server_text, server_label, lsp_cell) = if candidates.is_empty() {
                (
                    "(none available)".to_string(),
                    format!("{gray}(none available){r}"),
                    format!("{gray}  ·{r} "),
                )
            } else if let Some(found) = installed_server {
                (
                    (*found).to_string(),
                    format!("{white}{found}{r}"),
                    format!("{ok} "),
                )
            } else {
                (
                    candidates[0].to_string(),
                    format!("{gray}{}{r}", candidates[0]),
                    format!("{missing} "),
                )
            };

            let server_pad = " ".repeat(server_w.saturating_sub(server_text.chars().count()));
            let ast_spacing = " ".repeat(ast_hdr_w.saturating_sub(ast_pad + 2));

            println!(
                "  {white}{:<name_w$}{r}  {}{ast_spacing}  {}{}  {}",
                lang.display_name(),
                ast_badge,
                server_label,
                server_pad,
                lsp_cell,
                name_w = name_w,
            );
        }

        println!();
        let total = langs.len();
        let summary = vec![
            format!(
                " {b}{white}Grammars:{r}    {green}{static_count}{r} static (built-in) {gray}(total: {total}){r}"
            ),
            format!(
                " {b}{white}LSP servers:{r} {green}{lsp_count}{r}{gray}/{total} detected in PATH{r}"
            ),
            String::new(),
            format!(
                " {green}Static{r} = Built-in AST grammar   {blue}LSP{r} = Semantic tokens   {gray}󰚌{r} = Server not found"
            ),
        ];
        Self::print_boxed_card(&summary, 60);
    }
}
