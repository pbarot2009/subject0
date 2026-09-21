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

/// Parsed command-line arguments[span_5](start_span)[span_5](end_span).
#[derive(Debug, Default, Clone)]
pub struct CliArgs {
    /// Path to target file or directory[span_6](start_span)[span_6](end_span).
    pub path: Option<PathBuf>,
    /// Optional target line number to jump to on launch (1-based)[span_7](start_span)[span_7](end_span).
    pub jump_line: Option<usize>,
    /// Optional target column number to jump to on launch (1-based)[span_8](start_span)[span_8](end_span).
    pub jump_col: Option<usize>,
    /// Optional override for soft line wrapping[span_9](start_span)[span_9](end_span).
    pub line_wrap: Option<bool>,
    /// When true, ignore `.subject0` configuration file[span_10](start_span)[span_10](end_span).
    pub ignore_config: bool,
}

impl CliArgs {
    /// Parses CLI arguments from standard environment args[span_11](start_span)[span_11](end_span).
    ///
    /// Intercepts `--help`, `--version`, and `setup` to print directly to stdout and exit
    /// before terminal raw mode or alternate screen buffers are initialized[span_12](start_span)[span_12](end_span).
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
                // Handle editor line-jump syntax (e.g., +42 or +100)[span_13](start_span)[span_13](end_span)
                s if s.starts_with('+') => {
                    if let Ok(line_num) = s[1..].parse::<usize>() {
                        cli.jump_line = Some(line_num);
                    }
                }
                // Positional file or directory target[span_14](start_span)[span_14](end_span)
                s if !s.starts_with('-') => {
                    Self::assign_target_path(&mut cli, s);
                }
                _ => {}
            }
            idx += 1;
        }

        cli
    }

    /// Strips enclosing quotes from argument flags[span_15](start_span)[span_15](end_span).
    fn clean_lang_arg(raw: &str) -> String {
        raw.trim().trim_matches('\'').trim_matches('"').to_string()
    }

    /// Copies or clones the queries directory into the user configuration directory[span_16](start_span)[span_16](end_span).
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

        // Search for existing local queries in source/dev trees[span_17](start_span)[span_17](end_span)
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
            // If running standalone without local repository files, clone via git[span_18](start_span)[span_18](end_span)
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
            let status = Command::new("git")
                .args([
                    "clone",
                    "--depth",
                    "1",
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

    /// Recursively copies directories and files[span_19](start_span)[span_19](end_span).
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

    /// Counts subdirectories inside a given path[span_20](start_span)[span_20](end_span).
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

    /// Assigns file path and checks for `path:line:col` or `path:line` format across Unix and Windows[span_21](start_span)[span_21](end_span).
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
      {yellow}-g, --install-grammar <L>{r}  Inspect status of built-in Tree-sitter grammar
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
      {white}s0 --health{r}"
        );
    }

    /// Renders a bordered card with dynamic padding, guaranteeing border alignment[span_22](start_span)[span_22](end_span).
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

    /// Computes terminal display character width handling escape codes and double-width glyphs[span_23](start_span)[span_23](end_span).
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

    /// Inspects status of a Tree-sitter grammar[span_24](start_span)[span_24](end_span).
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
            format!(" {gray}Rust, C, C++, Zig, Python, JavaScript, TypeScript, Go, JSON,{r}"),
            format!(" {gray}TOML, YAML, Bash, HTML, CSS, Markdown, Java, C#, Ruby, Lua.{r}"),
            String::new(),
            format!(
                " {blue}Files for '{canon_lang}' receive full syntax and semantic intelligence via LSP.{r}"
            ),
        ];
        Self::print_boxed_card(&lines, 66);
    }

    /// Prints a comprehensive health report showing static grammars, LSP servers, and Git integration[span_25](start_span)[span_25](end_span).
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
