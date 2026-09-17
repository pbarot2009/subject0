//! # Command-Line Argument Parser
//!
//! Handles command-line arguments, flag decoding (`--help`, `--version`, `--clean`),
//! jump-to-line specifiers (`+<line>`, `file:line:col`), and directory/file path resolution.

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

        // If the path literally exists on disk, use it directly
        let literal_path = PathBuf::from(arg);
        if literal_path.exists() {
            cli.path = Some(literal_path);
            return;
        }

        // Check for file:line:col or file:line syntax
        let parts: Vec<&str> = arg.rsplitn(3, ':').collect();
        if parts.len() == 3 {
            // [col, line, file]
            if let (Ok(line), Ok(col)) = (parts[1].parse::<usize>(), parts[0].parse::<usize>()) {
                let candidate = PathBuf::from(parts[2]);
                cli.path = Some(candidate);
                cli.jump_line = Some(line);
                cli.jump_col = Some(col);
                return;
            }
        } else if parts.len() == 2 {
            // [line, file]
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
      {magenta}[+LINE]{r}            Jump directly to line number {gray}(e.g. +42 or path:42){r}

  {b}{blue}󰅂 OPTIONS:{r}
      {yellow}-h, --help{r}                Show this formatted help menu and exit
      {yellow}-v, --version{r}             Print version information and metadata
      {yellow}-g, --install-grammar <L>{r}  Download & compile Tree-sitter grammar (.so/.dll & queries)
      {yellow}--force, --reinstall{r}      Re-download and recompile existing grammar
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

      {gray}# Launch scratch buffer with no configuration:{r}
      {white}s0 --clean{r}

      {gray}# Check which languages have an LSP server and grammar installed:{r}
      {white}s0 --health{r}"
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
                // Wide characters and Nerd Font icons occupy 2 visual columns
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
                    || (0xE000..=0xF8FF).contains(&u); // Private Use Area (Nerd Font icons)

                width += if is_wide { 2 } else { 1 };
            }
        }
        width
    }

    /// Cross-platform helper locating standard user home directory.
    fn get_home_dir() -> Option<PathBuf> {
        env::var_os("HOME")
            .or_else(|| env::var_os("USERPROFILE"))
            .map(PathBuf::from)
    }

    fn shorten_home(path: &Path) -> String {
        if let Some(home) = Self::get_home_dir() {
            let s_path = path.to_string_lossy();
            let s_home = home.to_string_lossy();
            if s_path.starts_with(&*s_home) {
                return s_path.replacen(&*s_home, "~", 1);
            }
        }
        path.to_string_lossy().to_string()
    }

    /// Validates grammar identifier against path traversal attacks.
    fn is_valid_grammar_name(name: &str) -> bool {
        !name.is_empty()
            && name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    }

    /// Maps user input grammar aliases to official repository URLs and subdirectory targets.
    fn resolve_grammar_target(lang: &str) -> (&'static str, Option<&'static str>) {
        match lang {
            "c_sharp" | "csharp" | "cs" => (
                "https://github.com/tree-sitter/tree-sitter-c-sharp.git",
                None,
            ),
            "typescript" | "ts" => (
                "https://github.com/tree-sitter/tree-sitter-typescript.git",
                Some("typescript"),
            ),
            "tsx" => (
                "https://github.com/tree-sitter/tree-sitter-typescript.git",
                Some("tsx"),
            ),
            "javascript" | "js" => (
                "https://github.com/tree-sitter/tree-sitter-javascript.git",
                None,
            ),
            "bash" | "sh" | "zsh" => ("https://github.com/tree-sitter/tree-sitter-bash.git", None),
            "rust" | "rs" => ("https://github.com/tree-sitter/tree-sitter-rust.git", None),
            "python" | "py" => (
                "https://github.com/tree-sitter/tree-sitter-python.git",
                None,
            ),
            "c" => ("https://github.com/tree-sitter/tree-sitter-c.git", None),
            "cpp" | "c++" => ("https://github.com/tree-sitter/tree-sitter-cpp.git", None),
            "go" => ("https://github.com/tree-sitter/tree-sitter-go.git", None),
            "zig" => ("https://github.com/ziglibs/tree-sitter-zig.git", None),
            "lua" => ("https://github.com/MunifTanjim/tree-sitter-lua.git", None),
            "toml" => (
                "https://github.com/tree-sitter-grammars/tree-sitter-toml.git",
                None,
            ),
            "json" => ("https://github.com/tree-sitter/tree-sitter-json.git", None),
            "yaml" | "yml" => (
                "https://github.com/tree-sitter-grammars/tree-sitter-yaml.git",
                None,
            ),
            "html" => ("https://github.com/tree-sitter/tree-sitter-html.git", None),
            "css" => ("https://github.com/tree-sitter/tree-sitter-css.git", None),
            "markdown" | "md" => (
                "https://github.com/tree-sitter-grammars/tree-sitter-markdown.git",
                Some("tree-sitter-markdown"),
            ),
            "hcl" | "terraform" => (
                "https://github.com/tree-sitter-grammars/tree-sitter-hcl.git",
                None,
            ),
            "janet_simple" | "janet" => (
                "https://github.com/janet-lang/tree-sitter-janet-simple.git",
                None,
            ),
            _ => ("", None),
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

        if !Self::is_valid_grammar_name(lang) {
            eprintln!(
                "{red}Error: Invalid grammar name '{lang}'. Only alphanumeric, '-' and '_' characters are permitted.{r}"
            );
            process::exit(1);
        }

        let home = if let Some(h) = Self::get_home_dir() {
            h
        } else {
            eprintln!("{red}Error: Unable to locate home directory (HOME or USERPROFILE).{r}");
            process::exit(1);
        };

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

        let (explicit_repo, subpath) = Self::resolve_grammar_target(canon_lang);
        let dynamic_url = format!("https://github.com/tree-sitter/tree-sitter-{canon_lang}.git");
        let repo_url = if !explicit_repo.is_empty() {
            explicit_repo
        } else {
            &dynamic_url
        };

        let grammars_dir = home.join(".local/share/subject0/grammars");
        let queries_dir = home.join(".local/share/subject0/queries").join(canon_lang);
        let target_scm = queries_dir.join("highlights.scm");

        let ext = if cfg!(target_os = "windows") {
            "dll"
        } else if cfg!(target_os = "macos") {
            "dylib"
        } else {
            "so"
        };

        let target_so = grammars_dir.join(format!("{canon_lang}.{ext}"));

        if target_so.is_file() && !force {
            let display_so = Self::shorten_home(&target_so);
            let query_status = if target_scm.is_file() {
                format!("{green}Installed (highlights.scm){r}")
            } else {
                format!("{yellow}Missing (Run with --force to refetch queries){r}")
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

        let temp_dir = env::temp_dir().join(format!("s0-grammar-{canon_lang}"));
        if temp_dir.exists() {
            let _ = fs::remove_dir_all(&temp_dir);
        }

        println!("{blue}󰄬 Cloning Tree-sitter grammar for {b}{canon_lang}{r}{blue}...{r}");

        let clone_status = Command::new("git")
            .arg("clone")
            .arg("--depth=1")
            .arg(repo_url)
            .arg(&temp_dir)
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

        let base_repo_dir = if let Some(sub) = subpath {
            temp_dir.join(sub)
        } else {
            temp_dir.clone()
        };

        let src_dir = if base_repo_dir.join("src").exists() {
            base_repo_dir.join("src")
        } else if temp_dir.join(canon_lang).join("src").exists() {
            temp_dir.join(canon_lang).join("src")
        } else {
            base_repo_dir.clone()
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

        let c_compiler = if Command::new("cc").arg("--version").output().is_ok() {
            "cc"
        } else if Command::new("clang").arg("--version").output().is_ok() {
            "clang"
        } else {
            "gcc"
        };

        let cpp_compiler = if Command::new("c++").arg("--version").output().is_ok() {
            "c++"
        } else if Command::new("clang++").arg("--version").output().is_ok() {
            "clang++"
        } else {
            "g++"
        };

        let parser_obj = temp_dir.join("parser.o");
        let scanner_obj = temp_dir.join("scanner.o");

        println!("{yellow}󰑮 Compiling C parser for {b}{canon_lang}{r}{yellow}...{r}");

        // 1. Compile parser.c strictly with C compiler to prevent name mangling
        let mut cmd_c = Command::new(c_compiler);
        cmd_c
            .arg("-O3")
            .arg("-fPIC")
            .arg("-I")
            .arg(&src_dir)
            .arg("-I")
            .arg(&base_repo_dir)
            .arg("-c")
            .arg(&parser_c)
            .arg("-o")
            .arg(&parser_obj);

        if !cmd_c.status().map_or(false, |s| s.success()) {
            eprintln!("{red}Failed to compile parser.c. Verify a C compiler is installed.{r}");
            let _ = fs::remove_dir_all(&temp_dir);
            process::exit(1);
        }

        // 2. Compile scanner if present
        let mut has_scanner = false;
        let mut scanner_is_cpp = false;

        if scanner_c.exists() {
            has_scanner = true;
            let mut cmd_sc = Command::new(c_compiler);
            cmd_sc
                .arg("-O3")
                .arg("-fPIC")
                .arg("-I")
                .arg(&src_dir)
                .arg("-I")
                .arg(&base_repo_dir)
                .arg("-c")
                .arg(&scanner_c)
                .arg("-o")
                .arg(&scanner_obj);
            if !cmd_sc.status().map_or(false, |s| s.success()) {
                eprintln!("{red}Failed to compile C scanner.{r}");
                let _ = fs::remove_dir_all(&temp_dir);
                process::exit(1);
            }
        } else if scanner_cc.exists() || scanner_cpp.exists() {
            has_scanner = true;
            scanner_is_cpp = true;
            let active_scanner = if scanner_cc.exists() {
                scanner_cc
            } else {
                scanner_cpp
            };
            let mut cmd_sc = Command::new(cpp_compiler);
            cmd_sc
                .arg("-O3")
                .arg("-fPIC")
                .arg("-I")
                .arg(&src_dir)
                .arg("-I")
                .arg(&base_repo_dir)
                .arg("-c")
                .arg(&active_scanner)
                .arg("-o")
                .arg(&scanner_obj);
            if !cmd_sc.status().map_or(false, |s| s.success()) {
                eprintln!("{red}Failed to compile C++ scanner.{r}");
                let _ = fs::remove_dir_all(&temp_dir);
                process::exit(1);
            }
        }

        // 3. Link objects into final shared library
        println!("{yellow}󰑮 Linking {b}{canon_lang}.{ext}{r}{yellow}...{r}");
        let linker = if scanner_is_cpp {
            cpp_compiler
        } else {
            c_compiler
        };
        let mut link_cmd = Command::new(linker);

        if cfg!(target_os = "macos") {
            link_cmd.args(["-dynamiclib", "-undefined", "dynamic_lookup"]);
        } else if cfg!(target_os = "windows") {
            link_cmd.args(["-shared", "-Wl,--export-all-symbols"]);
        } else {
            link_cmd.arg("-shared");
        }

        link_cmd.arg(&parser_obj);
        if has_scanner {
            link_cmd.arg(&scanner_obj);
        }
        link_cmd.arg("-o").arg(&target_so);

        if !link_cmd.status().map_or(false, |s| s.success()) {
            eprintln!("{red}Linking failed for {canon_lang}.{ext}.{r}");
            let _ = fs::remove_dir_all(&temp_dir);
            process::exit(1);
        }

        // Copy highlights.scm queries if present
        let query_candidates = [
            base_repo_dir.join("queries").join("highlights.scm"),
            base_repo_dir
                .join("queries")
                .join(canon_lang)
                .join("highlights.scm"),
            temp_dir.join("queries").join("highlights.scm"),
            temp_dir
                .join("queries")
                .join(canon_lang)
                .join("highlights.scm"),
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
            format!(" Language:  {b}{canon_lang}{r}"),
            format!(" Library:   {blue}{display_so}{r}"),
            format!(
                " Queries:   {}",
                if query_copied {
                    format!("{green}Installed (highlights.scm){r}")
                } else {
                    format!(
                        "{yellow}Missing from repository (Highlighting will fall back to Tier 2/LSP){r}"
                    )
                }
            ),
        ];

        Self::print_boxed_card(&lines, 56);
    }

    /// Prints a health report showing language grammar and LSP connectivity.
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

        // Account for "(none available)" text width (16 chars) so table columns align
        let server_w = candidate_max
            .max("LSP SERVER".len())
            .max("(none available)".len());

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
