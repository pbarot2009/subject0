//! # Command-Line Argument Parser
//!
//! Handles command-line arguments, flag decoding (`--help`, `--version`, `--clean`),
//! jump-to-line specifiers (`+<line>`), and directory/file path resolution.

use std::{
    env, fs,
    path::{Path, PathBuf},
    process::{self, Command},
};

use crate::lsp::{DynamicGrammar, SupportedLanguage, resolve_binary_path};

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

        let force_grammar = raw_args
            .iter()
            .any(|a| a == "--force" || a == "--reinstall" || a == "-f");

        let mut idx = 0;
        while idx < raw_args.len() {
            let arg = &raw_args[idx];
            match arg.as_str() {
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
                        Self::run_grammar_installer(&lang, force_grammar);
                        process::exit(0);
                    } else {
                        eprintln!("Error: Missing language argument for --install-grammar <lang>");
                        process::exit(1);
                    }
                }
                s if s.starts_with("--install-grammar=") => {
                    let lang = s.trim_start_matches("--install-grammar=");
                    Self::run_grammar_installer(lang, force_grammar);
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
                s if !s.starts_with('-') && cli.path.is_none() => {
                    cli.path = Some(PathBuf::from(s));
                }
                _ => {}
            }
            idx += 1;
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
      {yellow}-h, --help{r}                Show this formatted help menu and exit
      {yellow}-v, --version{r}             Print version information and metadata
      {yellow}-g, --install-grammar <L>{r}  Download & compile Tree-sitter grammar (.so & queries)
      {yellow}--force, --reinstall{r}      Re-download and recompile existing grammar
      {yellow}-H, --health, --doctor{r}    Show install status of every supported language
      {yellow}-w, --wrap{r}                Force soft line wrapping on
      {yellow}-nw, --no-wrap{r}            Force line wrapping off (horizontal scroll)
      {yellow}--clean{r}                   Bypass workspace and user {gray}.subject0{r} configs

  {b}{blue}󰅂 EXAMPLES:{r}
      {gray}# Open a file at line 50:{r}
      {white}s0 src/main.rs +50{r}

      {gray}# Open project directory in the sidebar explorer:{r}
      {white}s0 .{r}

      {gray}# Launch scratch buffer with no configuration:{r}
      {white}s0 --clean{r}

      {gray}# Check which languages have an LSP server and grammar installed:{r}
      {white}s0 --health{r}"
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

    fn shorten_home(path: &Path) -> String {
        if let Ok(home) = env::var("HOME") {
            path.to_string_lossy().replacen(&home, "~", 1)
        } else {
            path.to_string_lossy().to_string()
        }
    }

    /// Downloads, compiles, and installs a Tree-sitter grammar shared library and queries.
    fn run_grammar_installer(lang: &str, force: bool) {
        let r = "\x1b[0m";
        let b = "\x1b[1m";
        let green = "\x1b[38;2;100;200;140m";
        let yellow = "\x1b[38;2;240;200;90m";
        let red = "\x1b[38;2;240;90;90m";
        let blue = "\x1b[38;2;100;180;255m";
        let gray = "\x1b[38;2;140;145;160m";

        let home = if let Ok(h) = env::var("HOME") {
            PathBuf::from(h)
        } else {
            eprintln!("{red}Error: Unable to locate $HOME environment variable.{r}");
            process::exit(1);
        };

        let grammars_dir = home.join(".local/share/subject0/grammars");
        let queries_dir = home.join(".local/share/subject0/queries").join(lang);
        let target_scm = queries_dir.join("highlights.scm");

        let ext = if cfg!(target_os = "windows") {
            "dll"
        } else if cfg!(target_os = "macos") {
            "dylib"
        } else {
            "so"
        };

        let target_so = grammars_dir.join(format!("{lang}.{ext}"));

        // Cache detection: skip cloning/compilation if already installed
        if target_so.is_file() && !force {
            let display_so = Self::shorten_home(&target_so);
            let query_status = if target_scm.is_file() {
                format!("{green}Installed (highlights.scm){r}")
            } else {
                format!("{yellow}None{r}")
            };

            let lines = vec![
                format!(" {green}󰄬 Grammar already installed{r}"),
                format!(" Language:  {b}{lang}{r}"),
                format!(" Library:   {blue}{display_so}{r}"),
                format!(" Queries:   {query_status}"),
                String::new(),
                format!(
                    " {gray}Pass {yellow}--force{gray} or {yellow}--reinstall{gray} to recompile{r}"
                ),
            ];

            Self::print_boxed_card(&lines, 56);
            process::exit(0);
        }

        if let Err(e) = fs::create_dir_all(&grammars_dir) {
            eprintln!("{red}Error creating grammars directory: {e}{r}");
            process::exit(1);
        }
        if let Err(e) = fs::create_dir_all(&queries_dir) {
            eprintln!("{red}Error creating queries directory: {e}{r}");
            process::exit(1);
        }

        let repo_url = match lang {
            "rust" => "https://github.com/tree-sitter/tree-sitter-rust.git",
            "python" => "https://github.com/tree-sitter/tree-sitter-python.git",
            "c" => "https://github.com/tree-sitter/tree-sitter-c.git",
            "cpp" => "https://github.com/tree-sitter/tree-sitter-cpp.git",
            "go" => "https://github.com/tree-sitter/tree-sitter-go.git",
            "zig" => "https://github.com/ziglibs/tree-sitter-zig.git",
            "javascript" | "js" => "https://github.com/tree-sitter/tree-sitter-javascript.git",
            "typescript" | "ts" => "https://github.com/tree-sitter/tree-sitter-typescript.git",
            "bash" | "sh" => "https://github.com/tree-sitter/tree-sitter-bash.git",
            "lua" => "https://github.com/MunifTanjim/tree-sitter-lua.git",
            "toml" => "https://github.com/tree-sitter-grammars/tree-sitter-toml.git",
            "json" => "https://github.com/tree-sitter/tree-sitter-json.git",
            _ => &format!("https://github.com/tree-sitter/tree-sitter-{lang}.git"),
        };

        let temp_dir = env::temp_dir().join(format!("s0-grammar-{lang}"));
        if temp_dir.exists() {
            let _ = fs::remove_dir_all(&temp_dir);
        }

        println!("{blue}󰄬 Cloning Tree-sitter grammar for {b}{lang}{r}{blue}...{r}");

        let clone_status = Command::new("git")
            .args(["clone", "--depth=1", repo_url, temp_dir.to_str().unwrap()])
            .status();

        match clone_status {
            Ok(s) if s.success() => {}
            _ => {
                eprintln!(
                    "{red}Failed to clone repository from {repo_url}. Verify git is installed.{r}"
                );
                process::exit(1);
            }
        }

        // Locate source files
        let src_dir = if temp_dir.join(lang).join("src").exists() {
            temp_dir.join(lang).join("src")
        } else {
            temp_dir.join("src")
        };

        let parser_c = src_dir.join("parser.c");
        if !parser_c.exists() {
            eprintln!("{red}Invalid grammar repository: missing src/parser.c{r}");
            let _ = fs::remove_dir_all(&temp_dir);
            process::exit(1);
        }

        let scanner_c = src_dir.join("scanner.c");
        let scanner_cc = src_dir.join("scanner.cc");
        let scanner_cpp = src_dir.join("scanner.cpp");

        let has_cpp_scanner = scanner_cc.exists() || scanner_cpp.exists();
        let compiler = if has_cpp_scanner {
            if Command::new("c++").arg("--version").output().is_ok() {
                "c++"
            } else if Command::new("clang++").arg("--version").output().is_ok() {
                "clang++"
            } else {
                "g++"
            }
        } else if Command::new("cc").arg("--version").output().is_ok() {
            "cc"
        } else if Command::new("clang").arg("--version").output().is_ok() {
            "clang"
        } else {
            "gcc"
        };

        println!("{yellow}󰑮 Compiling {b}{lang}.{ext}{r}{yellow} with {compiler}...{r}");

        let mut compile_cmd = Command::new(compiler);
        compile_cmd
            .arg("-O3")
            .arg("-fPIC")
            .arg("-shared")
            .arg(format!("-I{}", src_dir.display()))
            .arg(parser_c);

        if scanner_c.exists() {
            compile_cmd.arg(scanner_c);
        } else if scanner_cc.exists() {
            compile_cmd.arg(scanner_cc);
        } else if scanner_cpp.exists() {
            compile_cmd.arg(scanner_cpp);
        }

        compile_cmd.arg("-o").arg(&target_so);

        let compile_status = compile_cmd.status();
        match compile_status {
            Ok(s) if s.success() => {}
            _ => {
                eprintln!(
                    "{red}Compilation failed. Ensure a C/C++ compiler is installed on your host.{r}"
                );
                let _ = fs::remove_dir_all(&temp_dir);
                process::exit(1);
            }
        }

        // Copy highlights.scm queries if present
        let query_candidates = [
            temp_dir.join("queries").join("highlights.scm"),
            temp_dir.join("queries").join(lang).join("highlights.scm"),
            temp_dir.join(lang).join("queries").join("highlights.scm"),
            src_dir.join("highlights.scm"),
        ];

        let mut query_copied = false;
        for q in &query_candidates {
            if q.is_file() {
                let target_scm = queries_dir.join("highlights.scm");
                if fs::copy(q, target_scm).is_ok() {
                    query_copied = true;
                    break;
                }
            }
        }

        let _ = fs::remove_dir_all(&temp_dir);

        let display_so = Self::shorten_home(&target_so);
        let lines = vec![
            format!(" {green}󰄬 Grammar Installed Successfully!{r}"),
            format!(" Language:  {b}{lang}{r}"),
            format!(" Library:   {blue}{display_so}{r}"),
            format!(
                " Queries:   {}",
                if query_copied {
                    format!("{green}Installed (highlights.scm){r}")
                } else {
                    format!("{yellow}None found in repo (AST fallback active){r}")
                }
            ),
        ];

        Self::print_boxed_card(&lines, 56);
    }

    /// Prints a health report showing, for every supported language: whether
    /// a Tree-sitter grammar is installed, and whether at least one candidate
    /// LSP server binary is reachable on `$PATH` (or `~/.cargo/bin`).
    ///
    /// This purely inspects the local filesystem/`$PATH` — it never spawns or
    /// initializes any language server, so it's always fast and side-effect free.
    fn run_health_check() {
        let r = "\x1b[0m";
        let b = "\x1b[1m";
        let green = "\x1b[38;2;100;200;140m";
        let yellow = "\x1b[38;2;240;200;90m";
        let blue = "\x1b[38;2;100;180;255m";
        let gray = "\x1b[38;2;140;145;160m";
        let white = "\x1b[38;2;225;230;240m";
        let ver = env!("CARGO_PKG_VERSION");

        let ok = format!("{green}󰄬{r}");
        let missing = format!("{gray}󰚌{r}");

        let header = vec![format!(
            " {blue}󰆉{r} {b}{white}subject0{r} {gray}(s0){r} {green}v{ver}{r} {gray}— Language Support Health{r}"
        )];
        Self::print_boxed_card(&header, 62);
        println!();

        let langs = SupportedLanguage::all();

        // Column widths sized off the longest actual value, so the table
        // stays aligned no matter the terminal width or language name length.
        let name_w = langs
            .iter()
            .map(|l| l.display_name().chars().count())
            .max()
            .unwrap_or(8)
            .max("LANGUAGE".len());
        let server_w = langs
            .iter()
            .flat_map(|l| l.candidate_servers().iter())
            .map(|s| s.chars().count())
            .max()
            .unwrap_or(6)
            .max("LSP SERVER".len());

        println!(
            "  {gray}{:<name_w$}  {:<4}  {:<server_w$}  {:<4}{r}",
            "LANGUAGE",
            "AST",
            "LSP SERVER",
            "LSP",
            name_w = name_w,
            server_w = server_w
        );
        println!(
            "  {gray}{}  {}  {}  {}{r}",
            "─".repeat(name_w),
            "─".repeat(4),
            "─".repeat(server_w),
            "─".repeat(4)
        );

        let mut grammar_count = 0usize;
        let mut lsp_count = 0usize;

        for lang in langs {
            let grammar_installed =
                DynamicGrammar::grammar_file_path(lang.grammar_name()).is_some();
            if grammar_installed {
                grammar_count += 1;
            }
            let ast_cell = if lang.grammar_name().is_empty() {
                format!("{gray}  ·{r} ")
            } else if grammar_installed {
                format!("{ok} ")
            } else {
                format!("{missing} ")
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
            // Pad on the plain (ANSI-free) text width, then append the
            // colorized label, so the escape codes never throw off alignment.
            let server_pad = " ".repeat(server_w.saturating_sub(server_text.chars().count()));

            println!(
                "  {white}{:<name_w$}{r}  {}  {}{}  {}",
                lang.display_name(),
                ast_cell,
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
                " {b}{white}Grammars:{r}  {green}{grammar_count}{r}{gray}/{total} installed{r}"
            ),
            format!(
                " {b}{white}LSP servers:{r} {green}{lsp_count}{r}{gray}/{total} found on $PATH{r}"
            ),
            String::new(),
            format!(
                " {yellow}󰄬{r} = installed   {gray}󰚌{r} = not found   {gray}·{r} = not applicable"
            ),
            format!(" {gray}Install a grammar with:{r} {blue}s0 -g <language>{r}"),
        ];
        Self::print_boxed_card(&summary, 56);
    }
}
