//! # Command-Line Argument Parser & Diagnostics Subsystem
//!
//! Handles command-line arguments, flag decoding (`--help`, `--version`, `--clean`),
//! jump-to-line specifiers (`+<line>`, `file:line:col`), path resolution across
//! Termux, Linux, macOS, and Windows, grammar inspection, and language health diagnostics.

use std::{
    env, fs,
    path::{Path, PathBuf},
    process::{self, Command},
};

use crate::lsp::{resolve_binary_path, subject0_config_dir};
use crate::syntax::{DynamicGrammar, SupportedLanguage};

use crate::nerdfonts::{
    BOX_HORIZONTAL, BOX_ROUND_BOTTOM_LEFT, BOX_ROUND_BOTTOM_RIGHT, BOX_ROUND_TOP_LEFT,
    BOX_ROUND_TOP_RIGHT, BOX_VERTICAL, CHECK, CHEVRON_RIGHT, DOT_MIDDLE, FILE_DOCUMENT, GIT_BRANCH,
    HEALTH, LIGHTBULB, MISSING,
};

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

#[derive(Debug, Clone, PartialEq, Eq)]
enum GrammarRequest {
    Help,
    List,
    Fetch(String),
    Build(String),
    Install(String),
    Remove(String),
    Status(String),
}

impl CliArgs {
    /// Parses CLI arguments from standard environment args.
    ///
    /// Intercepts `--help`, `--version`, and `setup` to print directly to stdout and exit
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
                "setup" | "--setup" => {
                    Self::run_setup(force_grammar);
                    process::exit(0);
                }
                "grammar" | "--grammar" | "grammer" | "--grammer" => {
                    let tail = &raw_args[idx + 1..];
                    match Self::parse_grammar_tail(tail) {
                        Ok(req) => {
                            if Self::dispatch_grammar(req, force_grammar).is_err() {
                                process::exit(1);
                            }
                            process::exit(0);
                        }
                        Err(err) => {
                            eprintln!("Error: {err}");
                            eprintln!("{}", Self::grammar_usage());
                            process::exit(1);
                        }
                    }
                }
                s if s.starts_with("--grammar=") || s.starts_with("--grammer=") => {
                    let rest = s.split_once('=').map(|(_, v)| v).unwrap_or("");
                    let mut tail: Vec<String> = Vec::new();
                    if !rest.is_empty() {
                        tail.push(rest.to_string());
                    }
                    tail.extend(raw_args[idx + 1..].iter().cloned());
                    match Self::parse_grammar_tail(&tail) {
                        Ok(req) => {
                            if Self::dispatch_grammar(req, force_grammar).is_err() {
                                process::exit(1);
                            }
                            process::exit(0);
                        }
                        Err(err) => {
                            eprintln!("Error: {err}");
                            eprintln!("{}", Self::grammar_usage());
                            process::exit(1);
                        }
                    }
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
                other => {
                    let hint = Self::suggest_flag(other);
                    eprintln!("Error: unknown option '{other}'{hint}");
                    eprintln!(
                        "Run 's0 --help' for usage. Use '--' before a path that starts with '-'."
                    );
                    process::exit(1);
                }
            }
            idx += 1;
        }

        cli
    }

    /// Strips enclosing quotes from argument flags.
    fn clean_lang_arg(raw: &str) -> String {
        raw.trim().trim_matches('\'').trim_matches('"').to_string()
    }

    /// Copies or clones the queries directory into the user configuration directory.
    fn run_setup(force: bool) {
        let r = "\x1b[0m";
        let b = "\x1b[1m";
        let green = "\x1b[38;2;100;200;140m";
        let blue = "\x1b[38;2;100;180;255m";
        let yellow = "\x1b[38;2;240;200;90m";
        let red = "\x1b[38;2;240;90;90m";
        let gray = "\x1b[38;2;140;145;160m";
        let white = "\x1b[38;2;225;230;240m";

        let target_dir = subject0_config_dir().join("queries");

        if target_dir.exists() && !force {
            let files_count = Self::count_subdirs(&target_dir);
            let target_disp = Self::shorten_home(&target_dir);
            let lines = vec![
                format!(" {green}{CHECK} Queries Already Configured{r}"),
                format!(" Destination: {b}{target_disp}{r}"),
                format!(" Languages:   {green}{files_count} installed{r}"),
                String::new(),
                format!(" {gray}Run {white}s0 setup --force{gray} to overwrite or resync.{r}"),
            ];
            Self::print_boxed_card(&lines, 64);
            return;
        }

        // Search for existing local queries in source/dev trees
        let local_candidates = [
            PathBuf::from("./src/queries"),
            PathBuf::from("./queries"),
            PathBuf::from("../queries"),
            PathBuf::from("../../src/queries"),
        ];

        let local_source = local_candidates.iter().find(|p| p.is_dir());

        if let Some(src) = local_source {
            println!(
                "  {blue}{CHEVRON_RIGHT}{r} Copying queries from {white}{}{r}...",
                src.display()
            );
            match Self::copy_dir_recursive(src, &target_dir) {
                Ok(count) => {
                    let target_disp = Self::shorten_home(&target_dir);
                    let langs_count = Self::count_subdirs(&target_dir);
                    let lines = vec![
                        format!(" {green}{CHECK} Query Synchronization Successful{r}"),
                        format!(" Source:      {white}{}{r}", src.display()),
                        format!(" Destination: {b}{target_disp}{r}"),
                        format!(" Synced:      {green}{langs_count} languages ({count} files){r}"),
                        String::new(),
                        format!(" {gray}Tree-sitter queries are now active system-wide.{r}"),
                    ];
                    Self::print_boxed_card(&lines, 64);
                }
                Err(err) => {
                    eprintln!("{red}Error copying queries: {err}{r}");
                    process::exit(1);
                }
            }
        } else {
            // If running standalone without local repository files, clone via git
            println!(
                "  {yellow}{LIGHTBULB}{r} No local queries found. Fetching from subject0 repository..."
            );

            if resolve_binary_path("git").is_none() {
                eprintln!("{red}Error: 'git' is not installed or not in PATH.{r}");
                eprintln!(
                    "{gray}Please install git or run 's0 setup' inside the subject0 repository.{r}"
                );
                process::exit(1);
            }

            let temp_dir = env::temp_dir().join(format!("s0_queries_{}", process::id()));
            println!("  {yellow}{CHEVRON_RIGHT}{r} Cloning subject0 queries");
            let status = Command::new("git")
                .args([
                    "clone",
                    "--depth",
                    "1",
                    "--quiet",
                    "https://github.com/pbarot2009/subject0.git",
                ])
                .arg(&temp_dir)
                .status();

            match status {
                Ok(s) if s.success() => {
                    let candidates = [
                        temp_dir.join("src").join("queries"),
                        temp_dir.join("queries"),
                    ];
                    let repo_queries = candidates.into_iter().find(|p| p.is_dir());

                    if let Some(src_queries) = repo_queries {
                        let _ = Self::copy_dir_recursive(&src_queries, &target_dir);
                        let _ = fs::remove_dir_all(&temp_dir);

                        let target_disp = Self::shorten_home(&target_dir);
                        let langs_count = Self::count_subdirs(&target_dir);
                        let lines = vec![
                            format!(" {green}{CHECK} Subject0 Queries Cloned & Configured{r}"),
                            format!(
                                " Source:      {blue}https://github.com/pbarot2009/subject0{r}"
                            ),
                            format!(" Destination: {b}{target_disp}{r}"),
                            format!(" Languages:   {green}{langs_count} installed{r}"),
                            String::new(),
                            format!(" {gray}Tree-sitter queries are now active system-wide.{r}"),
                        ];
                        Self::print_boxed_card(&lines, 64);
                    } else {
                        let _ = fs::remove_dir_all(&temp_dir);
                        eprintln!(
                            "{red}Error: Failed to locate 'src/queries' in cloned repository.{r}"
                        );
                        process::exit(1);
                    }
                }
                _ => {
                    let _ = fs::remove_dir_all(&temp_dir);
                    eprintln!("{red}Error: Failed to clone subject0 repository via git.{r}");
                    process::exit(1);
                }
            }
        }
    }

    /// Recursively copies directories and files.
    fn copy_dir_recursive(src: &Path, dst: &Path) -> std::io::Result<usize> {
        fs::create_dir_all(dst)?;
        let mut count = 0;
        for entry in fs::read_dir(src)? {
            let entry = entry?;
            let ty = entry.file_type()?;
            let src_path = entry.path();
            let dst_path = dst.join(entry.file_name());

            if ty.is_dir() {
                count += Self::copy_dir_recursive(&src_path, &dst_path)?;
            } else {
                fs::copy(&src_path, &dst_path)?;
                count += 1;
            }
        }
        Ok(count)
    }

    /// Counts subdirectories inside a given path.
    fn count_subdirs(p: &Path) -> usize {
        fs::read_dir(p)
            .map(|entries| {
                entries
                    .filter_map(Result::ok)
                    .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
                    .count()
            })
            .unwrap_or(0)
    }

    /// Assigns file path and checks for `path:line:col` or `path:line` format across Unix and Windows.
    fn assign_target_path(cli: &mut Self, arg: &str) {
        if cli.path.is_some() {
            return;
        }

        let literal_path = PathBuf::from(arg);
        if literal_path.exists() {
            cli.path = Some(literal_path);
            return;
        }

        let tokens: Vec<&str> = arg.rsplit(':').collect();

        if tokens.len() >= 3
            && let (Ok(col), Ok(line)) = (tokens[0].parse::<usize>(), tokens[1].parse::<usize>())
        {
            let path_str: String = tokens[2..]
                .iter()
                .rev()
                .copied()
                .collect::<Vec<_>>()
                .join(":");
            cli.path = Some(PathBuf::from(path_str));
            cli.jump_line = Some(line);
            cli.jump_col = Some(col);
            return;
        }

        if tokens.len() >= 2
            && let Ok(line) = tokens[0].parse::<usize>()
        {
            let path_str: String = tokens[1..]
                .iter()
                .rev()
                .copied()
                .collect::<Vec<_>>()
                .join(":");
            cli.path = Some(PathBuf::from(path_str));
            cli.jump_line = Some(line);
            return;
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
            format!(
                " {blue}{FILE_DOCUMENT}{r} {b}{white}subject0{r} {gray}(s0){r}  {green}v{ver}{r}"
            ),
            format!(
                " {gray}Modal terminal code editor with Tree-sitter, Full LSP & pure-Rust Git engine{r}"
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
            " {blue}{FILE_DOCUMENT}{r} {b}{white}subject0{r} {gray}(s0){r} {green}v{ver}{r} {gray}— Terminal Modal Code Editor with Full LSP & Git{r}"
        )];

        Self::print_boxed_card(&header, 66);

        println!(
            "
  {b}{blue}{CHEVRON_RIGHT} USAGE:{r}
      {white}s0{r} {yellow}[OPTIONS]{r} {green}[PATH]{r} {magenta}[+LINE]{r}

  {b}{blue}{CHEVRON_RIGHT} ARGUMENTS:{r}
      {green}[PATH]{r}             File or folder path {gray}(opens scratch buffer if empty){r}
      {magenta}[+LINE]{r}            Jump directly to line number {gray}(e.g. +42 or path:42:10){r}

  {b}{blue}{CHEVRON_RIGHT} COMMANDS & OPTIONS:{r}
      {yellow}setup, --setup{r}            Synchronize runtime queries to ~/.config/subject0/queries
      {yellow}-h, --help{r}                Show this formatted help menu and exit
      {yellow}-v, --version{r}             Print version information and metadata
      {yellow}-g, --install-grammar <L>{r}  Inspect one grammar (static or installed)
      {yellow}grammar / --grammar <cmd>{r}  fetch, build, install, remove, status, list
      {yellow}--grammar=install <L>{r}     Same command, equals form accepted
      {yellow}-H, --health, --doctor{r}    Show install status of every supported language & LSP
      {yellow}-w, --wrap{r}                Force soft line wrapping on
      {yellow}-nw, --no-wrap{r}            Force line wrapping off (horizontal scroll)
      {yellow}--clean{r}                   Bypass workspace and user {gray}.subject0{r} configs
      {yellow}--{r}                        Treat all subsequent arguments as positional paths

  {b}{blue}{CHEVRON_RIGHT} GIT VERSION CONTROL & MOTIONS:{r}
      {magenta}]c{r}                        Jump to next Git diff hunk in buffer
      {magenta}[c{r}                        Jump to previous Git diff hunk in buffer
      {magenta}:revert-hunk{r} / {magenta}:rh{r}       Revert Git diff hunk under cursor to HEAD
      {magenta}:git{r}                     Display current branch and diff summary

  {b}{blue}{CHEVRON_RIGHT} LSP KEYBINDINGS (IN-EDITOR):{r}
      {magenta}K{r}                         Hover documentation and inferred type inspector
      {magenta}gd{r}                        Jump directly to definition under cursor
      {magenta}gr{r}                        Find all references across project
      {magenta}ga{r}                        Trigger available quickfixes & code actions
      {magenta}:fmt{r} / {magenta}Alt-F{r}             Format current buffer via LSP server
      {magenta}:rn{r}  / {magenta}F2{r}                Rename symbol across project (multi-file)
      {magenta}:sym{r} / {magenta}:symbols{r}          Search document symbols / function outline
      {magenta}]d{r}   / {magenta}[d{r}                Jump to next / previous compiler diagnostic
      {magenta}Ctrl-O{r} / {magenta}Ctrl-I{r}           Jump backward / forward in navigation history
      {magenta}:lsp-restart{r}             Reboot crashed or frozen Language Server
      {magenta}:hints{r}                   Toggle inline inferred type & parameter hints

  {b}{blue}{CHEVRON_RIGHT} EXAMPLES:{r}
      {gray}# Open a project directory in the sidebar explorer:{r}
      {white}s0 .{r}

      {gray}# Open a file at line 50, column 10:{r}
      {white}s0 src/main.rs:50:10{r}

      {gray}# Check system grammars, Git, and LSP servers:{r}
      {white}s0 --health\n  Editor: :colors :links :calls :incoming :outgoing use the Helix-style LSP methods{r}"
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

        println!(
            "{border}{BOX_ROUND_TOP_LEFT}{}{BOX_ROUND_TOP_RIGHT}{r}",
            BOX_HORIZONTAL.repeat(inner_width + 2)
        );

        for line in lines {
            let line_w = Self::visible_width(line);
            let pad = inner_width.saturating_sub(line_w);
            println!(
                "{border}{BOX_VERTICAL}{r} {line}{}{border} {BOX_VERTICAL}{r}",
                " ".repeat(pad)
            );
        }

        println!(
            "{border}{BOX_ROUND_BOTTOM_LEFT}{}{BOX_ROUND_BOTTOM_RIGHT}{r}",
            BOX_HORIZONTAL.repeat(inner_width + 2)
        );
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

    fn suggest_flag(given: &str) -> String {
        let known = [
            "--grammar",
            "--help",
            "--version",
            "--wrap",
            "--no-wrap",
            "--clean",
            "--install-grammar",
            "--health",
            "--doctor",
            "--setup",
            "grammar",
            "setup",
        ];
        let mut best = None;
        let mut best_dist = 3;
        for flag in known {
            let dist = edit_distance(given, flag);
            if dist < best_dist {
                best = Some(flag);
                best_dist = dist;
            }
        }
        match best {
            Some(flag) if best_dist > 0 => format!(". Did you mean '{flag}'?"),
            _ => String::new(),
        }
    }

    fn grammar_usage() -> &'static str {
        "Usage: s0 --grammar <fetch|build|install|remove|status|list|help> [language]\n\
         --grammer is accepted as a spelling of --grammar.\n\
         Forms: s0 --grammar install kotlin\n\
                s0 --grammar=install kotlin\n\
                s0 --grammar install=kotlin\n\
                s0 grammar install kotlin\n\
         Installs one language. Built in: rust, python, javascript, typescript, tsx, go, c, json, html, markdown, bash."
    }

    /// Normalize user language names to `languages.toml` grammar ids.
    pub fn canonicalize_grammar_name(raw: &str) -> String {
        let cleaned = Self::clean_lang_arg(raw).to_ascii_lowercase();
        match cleaned.as_str() {
            "rs" | "rust" => "rust".into(),
            "py" | "python" => "python".into(),
            "js" | "javascript" | "jsx" => "javascript".into(),
            "ts" | "typescript" => "typescript".into(),
            "tsx" => "tsx".into(),
            "golang" | "go" => "go".into(),
            "c" => "c".into(),
            "c++" | "cpp" | "cplusplus" | "cxx" => "cpp".into(),
            "json" => "json".into(),
            "htm" | "html" => "html".into(),
            "md" | "markdown" => "markdown".into(),
            "sh" | "shell" | "bash" | "zsh" => "bash".into(),
            "cs" | "csharp" | "c#" => "c-sharp".into(),
            "rb" | "ruby" => "ruby".into(),
            "yml" | "yaml" => "yaml".into(),
            "toml" => "toml".into(),
            "css" => "css".into(),
            "zig" => "zig".into(),
            "java" => "java".into(),
            "lua" => "lua".into(),
            other => other.to_string(),
        }
    }

    fn parse_grammar_tail(tail: &[String]) -> Result<GrammarRequest, String> {
        let tail: Vec<String> = tail
            .iter()
            .filter(|s| !matches!(s.as_str(), "--force" | "--reinstall" | "-f"))
            .cloned()
            .collect();
        if tail.is_empty() {
            return Err("missing grammar command".into());
        }
        let head = Self::clean_lang_arg(&tail[0]);
        if head == "--" {
            return Err("'--' is not a grammar command".into());
        }
        let (action, inline_lang) = if let Some((cmd, lang)) = head.split_once('=') {
            (cmd.to_string(), Some(lang.to_string()))
        } else {
            (head, None)
        };
        let action = action.to_ascii_lowercase();
        if matches!(action.as_str(), "help" | "--help" | "-h") {
            if tail.len() > 1 || inline_lang.is_some() {
                return Err("grammar help takes no arguments".into());
            }
            return Ok(GrammarRequest::Help);
        }
        if action == "list" {
            if tail.len() > 1 || inline_lang.is_some() {
                return Err("grammar list takes no language".into());
            }
            return Ok(GrammarRequest::List);
        }
        if !matches!(
            action.as_str(),
            "fetch" | "build" | "install" | "remove" | "status"
        ) {
            return Err(format!("unknown grammar command '{action}'"));
        }
        let lang = if let Some(lang) = inline_lang {
            if tail.len() > 1 {
                return Err("unexpected extra arguments after install=<language>".into());
            }
            lang
        } else if tail.len() < 2 {
            return Err(format!("missing language for '{action}'"));
        } else if tail.len() > 2 {
            return Err(format!("unexpected extra argument '{}'", tail[2]));
        } else {
            tail[1].clone()
        };
        if lang == "--" || lang.starts_with('-') {
            return Err(format!("invalid language '{lang}'"));
        }
        let canon = Self::canonicalize_grammar_name(&lang);
        if !Self::is_valid_grammar_name(&canon) {
            return Err(format!(
                "invalid grammar name '{lang}'. Use letters, numbers, '-' and '_'."
            ));
        }
        Ok(match action.as_str() {
            "fetch" => GrammarRequest::Fetch(canon),
            "build" => GrammarRequest::Build(canon),
            "install" => GrammarRequest::Install(canon),
            "remove" => GrammarRequest::Remove(canon),
            "status" => GrammarRequest::Status(canon),
            _ => unreachable!(),
        })
    }

    fn dispatch_grammar(req: GrammarRequest, force: bool) -> Result<(), ()> {
        match req {
            GrammarRequest::Help => {
                println!("{}", Self::grammar_usage());
                Ok(())
            }
            GrammarRequest::List => Self::run_grammar_command("list", None),
            GrammarRequest::Fetch(lang) => Self::run_grammar_progress("fetch", &lang, force),
            GrammarRequest::Build(lang) => Self::run_grammar_progress("build", &lang, force),
            GrammarRequest::Install(lang) => Self::run_grammar_progress("install", &lang, force),
            GrammarRequest::Remove(lang) => Self::run_grammar_progress("remove", &lang, force),
            GrammarRequest::Status(lang) => Self::run_grammar_command("status", Some(&lang)),
        }
    }

    fn builtin_grammar(lang: &str) -> bool {
        matches!(
            lang,
            "rust"
                | "c"
                | "python"
                | "javascript"
                | "typescript"
                | "tsx"
                | "go"
                | "json"
                | "html"
                | "markdown"
                | "bash"
        )
    }

    fn finish_card(title: &str, kind: &str, result: &str, note: &str) {
        let r = "\x1b[0m";
        let b = "\x1b[1m";
        let green = "\x1b[38;2;100;200;140m";
        let yellow = "\x1b[38;2;240;200;90m";
        let blue = "\x1b[38;2;100;180;255m";
        let gray = "\x1b[38;2;140;145;160m";
        let white = "\x1b[38;2;225;230;240m";
        let lines = vec![
            format!(" {green}{CHECK}{r} {b}{white}{title}{r}"),
            format!(" {yellow}Type:{r}    {white}{kind}{r}"),
            format!(" {yellow}Result:{r}  {blue}{result}{r}"),
            format!(" {gray}{note}{r}"),
        ];
        Self::print_boxed_card(&lines, 64);
    }

    fn step_line(label: &str) {
        let yellow = "\x1b[38;2;240;200;90m";
        let white = "\x1b[38;2;225;230;240m";
        let r = "\x1b[0m";
        println!("  {yellow}{CHEVRON_RIGHT}{r} {white}{label}{r}");
    }

    fn detail_line(label: &str) {
        let gray = "\x1b[38;2;140;145;160m";
        let r = "\x1b[0m";
        println!("    {gray}{label}{r}");
    }

    fn run_grammar_progress(action: &str, lang: &str, force: bool) -> Result<(), ()> {
        let red = "\x1b[38;2;240;90;90m";
        let r = "\x1b[0m";
        let state = crate::grammar::grammar_state(lang, Self::builtin_grammar(lang));
        if !force {
            match (action, &state) {
                ("install" | "build", crate::grammar::GrammarState::Static) => {
                    Self::finish_card(
                        &format!("{lang} already built in"),
                        "built-in grammar",
                        "skipped",
                        "Pass --force to clone and build a library anyway.",
                    );
                    return Ok(());
                }
                ("install" | "build", crate::grammar::GrammarState::Built(path)) => {
                    Self::finish_card(
                        &format!("{lang} already installed"),
                        "installed library",
                        &path.display().to_string(),
                        "Pass --force to reinstall.",
                    );
                    return Ok(());
                }
                (
                    "fetch",
                    crate::grammar::GrammarState::Fetched(_)
                    | crate::grammar::GrammarState::Built(_),
                ) => {
                    let where_at = match &state {
                        crate::grammar::GrammarState::Fetched(p)
                        | crate::grammar::GrammarState::Built(p) => p.display().to_string(),
                        _ => String::new(),
                    };
                    Self::finish_card(
                        &format!("{lang} already fetched"),
                        "cached sources",
                        &where_at,
                        "Pass --force to clone again.",
                    );
                    return Ok(());
                }
                _ => {}
            }
        }

        let mut report = |ev: crate::grammar::GrammarEvent| match ev {
            crate::grammar::GrammarEvent::Step(name) => Self::step_line(&name),
            crate::grammar::GrammarEvent::Detail(line) => {
                let clean = line.replace('\r', " ").trim().to_string();
                if clean.is_empty()
                    || clean.contains('%')
                    || clean.starts_with("remote:")
                    || clean.starts_with("hint:")
                {
                    return;
                }
                Self::detail_line(&clean);
            }
            crate::grammar::GrammarEvent::Info(line) => Self::detail_line(&line),
            crate::grammar::GrammarEvent::Ok => {}
        };
        let result = match action {
            "fetch" => crate::grammar::fetch_grammar_with(lang, &mut report)
                .map(|p| p.display().to_string()),
            "build" => crate::grammar::build_grammar_with(lang, &mut report)
                .map(|p| p.display().to_string()),
            "install" => crate::grammar::install_grammar_with(lang, &mut report)
                .map(|p| p.display().to_string()),
            "remove" => {
                Self::step_line("Removing sources and library");
                crate::grammar::remove_grammar(lang).map(|()| "removed".into())
            }
            _ => unreachable!(),
        };
        match result {
            Ok(path) => {
                let kind = if Self::builtin_grammar(lang) {
                    "built-in + library"
                } else if action == "remove" {
                    "removed"
                } else {
                    "installed library"
                };
                println!();
                Self::finish_card(
                    &format!("{action} {lang}"),
                    kind,
                    &path,
                    "Open a file of this language to use the grammar.",
                );
                Ok(())
            }
            Err(err) => {
                eprintln!("{red}Error:{r} {err}");
                Err(())
            }
        }
    }

    fn run_grammar_command(action: &str, lang: Option<&str>) -> Result<(), ()> {
        match action {
            "list" => match crate::grammar::list_grammar_names() {
                Ok(names) => {
                    println!("Available grammar sources: {}", names.len());
                    for name in names {
                        let state = crate::grammar::grammar_state(&name, false);
                        println!("  {name:24} {state:?}");
                    }
                    Ok(())
                }
                Err(e) => {
                    eprintln!("Error: {e}");
                    Err(())
                }
            },
            "fetch" | "build" | "install" | "remove" | "status" => {
                let Some(lang) = lang else {
                    eprintln!("Usage: s0 --grammar {action} <language>");
                    return Err(());
                };
                if !Self::is_valid_grammar_name(lang) {
                    eprintln!("Error: invalid grammar name '{lang}'");
                    return Err(());
                }
                let result = match action {
                    "fetch" => crate::grammar::fetch_grammar(lang)
                        .map(|p| format!("Fetched {lang} into {}", p.display())),
                    "build" => crate::grammar::build_grammar(lang)
                        .map(|p| format!("Built {lang} -> {}", p.display())),
                    "install" => crate::grammar::install_grammar(lang)
                        .map(|p| format!("Installed {lang} -> {}", p.display())),
                    "remove" => {
                        crate::grammar::remove_grammar(lang).map(|()| format!("Removed {lang}"))
                    }
                    "status" => {
                        let builtin = matches!(
                            lang,
                            "rust"
                                | "c"
                                | "python"
                                | "javascript"
                                | "typescript"
                                | "tsx"
                                | "go"
                                | "json"
                                | "html"
                                | "markdown"
                                | "bash"
                        );
                        let state = crate::grammar::grammar_state(lang, builtin);
                        if lang == "cpp" && !matches!(state, crate::grammar::GrammarState::Built(_))
                        {
                            Ok("cpp: not installed; .cpp files use the built-in C grammar until `s0 --grammar install cpp`".into())
                        } else {
                            Ok(format!("{lang}: {state:?}"))
                        }
                    }
                    _ => unreachable!(),
                };
                match result {
                    Ok(msg) => {
                        println!("{msg}");
                        Ok(())
                    }
                    Err(e) => {
                        eprintln!("Error: {e}");
                        Err(())
                    }
                }
            }
            _ => {
                eprintln!(
                    "Usage: s0 --grammar <fetch|build|install|remove|status|list> [language]"
                );
                eprintln!("Installs one language at a time. Example: s0 --grammar install python");
                Err(())
            }
        }
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

        if let Some(sl) = matched_lang
            && sl.static_language().is_some()
        {
            let custom_query = DynamicGrammar::grammar_file_path(canon_lang);
            let query_status = if let Some(qp) = custom_query {
                let disp = Self::shorten_home(&qp);
                format!("{green}Custom override ({disp}){r}")
            } else {
                format!("{green}Built-in standard queries{r}")
            };

            let lines = vec![
                format!(" {green}{CHECK} Built-in Static Grammar{r}"),
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

        let lines = vec![
            format!(" {yellow}{LIGHTBULB} LSP Semantic Highlighting & Intelligence Tier{r}"),
            format!(" Language:  {b}{canon_lang}{r}"),
            format!(" Status:    {gray}No built-in Tree-sitter grammar{r}"),
            String::new(),
            format!(" {white}subject0 statically links grammars for top-tier languages:{r}"),
            format!(" {gray}Built in: Rust, C, Python, JavaScript, TypeScript, TSX,{r}"),
            format!(" {gray}Go, JSON, HTML, Markdown, Bash. Install others with{r}"),
            format!(" {gray}s0 --grammar install <lang>{r}"),
            String::new(),
            format!(
                " {blue}Files for '{canon_lang}' receive full syntax and semantic intelligence via LSP.{r}"
            ),
        ];
        Self::print_boxed_card(&lines, 66);
    }

    /// Prints a comprehensive health report showing static grammars, LSP servers, and Git integration.
    fn run_health_check() {
        let r = "\x1b[0m";
        let b = "\x1b[1m";
        let green = "\x1b[38;2;100;200;140m";
        let blue = "\x1b[38;2;100;180;255m";
        let gray = "\x1b[38;2;140;145;160m";
        let white = "\x1b[38;2;225;230;240m";
        let ver = env!("CARGO_PKG_VERSION");

        let ok = format!("{green}{CHECK}{r}");
        let missing = format!("{gray}{MISSING}{r}");

        let header = vec![format!(
            " {blue}{HEALTH}{r} {b}{white}subject0{r} {gray}(s0){r} {green}v{ver}{r} {gray}— Language Support & Health Diagnostics{r}"
        )];
        Self::print_boxed_card(&header, 66);
        println!();

        // Check Git Subsystem status
        let git_installed = resolve_binary_path("git").is_some();
        let git_status_badge = if git_installed {
            format!("{ok} Available in PATH")
        } else {
            format!("{missing} (In-editor engine active via gix)")
        };

        let git_card = vec![
            format!(" {blue}{GIT_BRANCH} Git Engine Status{r}"),
            format!(" In-Editor Engine: {green}Pure Rust (gix + imara-diff){r}"),
            format!(" CLI Binary:       {git_status_badge}"),
        ];
        Self::print_boxed_card(&git_card, 66);
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
            BOX_HORIZONTAL.repeat(name_w),
            BOX_HORIZONTAL.repeat(ast_hdr_w),
            BOX_HORIZONTAL.repeat(server_w),
            BOX_HORIZONTAL.repeat(4)
        );

        let mut static_count = 0usize;
        let mut lsp_count = 0usize;

        for lang in langs {
            let (ast_badge, ast_pad) = if lang.static_language().is_some() {
                static_count += 1;
                (format!("{green}{CHECK} Static{r}"), 4)
            } else if lang.grammar_name().is_empty() {
                (format!("{gray}  {DOT_MIDDLE}{r}     "), 5)
            } else {
                (format!("{blue}{LIGHTBULB} LSP{r}   "), 5)
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
                    format!("{gray}  {DOT_MIDDLE}{r} "),
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
                " {green}Static{r} = Built-in AST grammar   {blue}LSP{r} = Inlay hints, symbols, semantic tokens   {gray}{MISSING}{r} = Not found in PATH"
            ),
        ];
        Self::print_boxed_card(&summary, 64);
    }
}

#[cfg(test)]
mod cli_tests {
    use super::CliArgs;

    fn tail(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn grammar_install_separate_tokens() {
        let req = CliArgs::parse_grammar_tail(&tail(&["install", "kotlin"])).unwrap();
        assert_eq!(req, super::GrammarRequest::Install("kotlin".into()));
    }

    #[test]
    fn grammar_install_equals_form() {
        let req = CliArgs::parse_grammar_tail(&tail(&["install=kotlin"])).unwrap();
        assert_eq!(req, super::GrammarRequest::Install("kotlin".into()));
    }

    #[test]
    fn grammar_alias_cpp() {
        let req = CliArgs::parse_grammar_tail(&tail(&["install", "c++"])).unwrap();
        assert_eq!(req, super::GrammarRequest::Install("cpp".into()));
    }

    #[test]
    fn grammar_alias_ts_and_csharp() {
        let req = CliArgs::parse_grammar_tail(&tail(&["status", "ts"])).unwrap();
        assert_eq!(req, super::GrammarRequest::Status("typescript".into()));
        let req = CliArgs::parse_grammar_tail(&tail(&["fetch", "c#"])).unwrap();
        assert_eq!(req, super::GrammarRequest::Fetch("c-sharp".into()));
    }

    #[test]
    fn grammar_list_and_help_reject_extras() {
        assert!(CliArgs::parse_grammar_tail(&tail(&["list"])).is_ok());
        assert!(CliArgs::parse_grammar_tail(&tail(&["list", "rust"])).is_err());
        assert!(CliArgs::parse_grammar_tail(&tail(&["help"])).is_ok());
        assert!(CliArgs::parse_grammar_tail(&tail(&["--help", "x"])).is_err());
    }

    #[test]
    fn grammar_missing_language_and_unknown_action() {
        assert!(CliArgs::parse_grammar_tail(&tail(&["install"])).is_err());
        assert!(CliArgs::parse_grammar_tail(&tail(&["explode", "rust"])).is_err());
        assert!(CliArgs::parse_grammar_tail(&tail(&[])).is_err());
    }

    #[test]
    fn grammar_rejects_extra_junk_and_dashes() {
        assert!(CliArgs::parse_grammar_tail(&tail(&["install", "kotlin", "extra"])).is_err());
        assert!(CliArgs::parse_grammar_tail(&tail(&["install", "--"])).is_err());
        assert!(CliArgs::parse_grammar_tail(&tail(&["--"])).is_err());
    }

    #[test]
    fn grammar_force_flag_is_not_an_extra_argument() {
        let req = CliArgs::parse_grammar_tail(&tail(&["install", "kotlin", "--force"])).unwrap();
        assert_eq!(req, super::GrammarRequest::Install("kotlin".into()));
    }

    #[test]
    fn grammar_quotes_are_stripped() {
        let req = CliArgs::parse_grammar_tail(&tail(&["build", "\"python\""])).unwrap();
        assert_eq!(req, super::GrammarRequest::Build("python".into()));
    }
}

fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0; b.len() + 1];
    for (i, ca) in a.iter().enumerate() {
        cur[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            cur[j + 1] = (prev[j + 1] + 1).min(cur[j] + 1).min(prev[j] + cost);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

#[cfg(test)]
mod flag_tests {
    use super::edit_distance;

    #[test]
    fn grammer_is_one_edit_from_grammar() {
        assert_eq!(edit_distance("--grammer", "--grammar"), 1);
    }
}
