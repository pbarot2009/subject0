//! Per-language Tree-sitter grammar fetch and build.
//!
//! Unlike Helix's `hx --grammar fetch` (which pulls every grammar), subject0
//! installs one language at a time. Static grammars compiled into the binary
//! always win. Built libraries live under the data dir and are loaded with
//! `libloading`.
//!
//! Grammar source list is `languages.toml` (Helix grammar table, MPL-2.0).

use std::{
    fs,
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
};

use anyhow::{Result, anyhow};
use libloading::Library;
use serde::Deserialize;

use crate::lsp::subject0_data_dir;

#[derive(Debug, Clone, Deserialize)]
pub struct GrammarSource {
    pub git: String,
    pub rev: String,
    #[serde(default)]
    pub subpath: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GrammarEntry {
    pub name: String,
    pub source: GrammarSource,
}

#[derive(Debug, Clone, Deserialize)]
struct GrammarFile {
    #[serde(default)]
    grammar: Vec<GrammarEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GrammarState {
    Static,
    Built(PathBuf),
    Fetched(PathBuf),
    Missing,
}

pub fn runtime_dir() -> PathBuf {
    subject0_data_dir().join("runtime")
}

pub fn grammar_src_dir(name: &str) -> PathBuf {
    runtime_dir().join("grammars").join("sources").join(name)
}

pub fn grammar_lib_path(name: &str) -> PathBuf {
    let file = format!(
        "{}{name}{}",
        std::env::consts::DLL_PREFIX,
        std::env::consts::DLL_SUFFIX
    );
    runtime_dir().join("grammars").join(file)
}

pub fn languages_toml_path() -> Option<PathBuf> {
    if let Ok(env) = std::env::var("SUBJECT0_LANGUAGES_TOML") {
        let p = PathBuf::from(env);
        if p.is_file() {
            return Some(p);
        }
    }
    let candidates = [
        PathBuf::from("languages.toml"),
        PathBuf::from("./languages.toml"),
    ];
    for c in candidates {
        if c.is_file() {
            return Some(c);
        }
    }
    if let Ok(mut exe) = std::env::current_exe() {
        exe.pop();
        let next = exe.join("languages.toml");
        if next.is_file() {
            return Some(next);
        }
        let parent = exe.join("../languages.toml");
        if parent.is_file() {
            return Some(parent);
        }
    }
    let data = subject0_data_dir().join("languages.toml");
    if data.is_file() {
        return Some(data);
    }
    None
}

pub fn load_grammar_table() -> Result<Vec<GrammarEntry>> {
    let path = languages_toml_path().ok_or_else(|| anyhow!("languages.toml not found"))?;
    let text = fs::read_to_string(&path)?;
    let parsed: GrammarFile = toml::from_str(&text)?;
    Ok(parsed.grammar)
}

pub fn find_grammar(name: &str) -> Result<GrammarEntry> {
    let name = name.trim().to_ascii_lowercase();
    load_grammar_table()?
        .into_iter()
        .find(|g| g.name.eq_ignore_ascii_case(&name))
        .ok_or_else(|| anyhow!("no grammar source named '{name}' in languages.toml"))
}

pub fn list_grammar_names() -> Result<Vec<String>> {
    let mut names: Vec<String> = load_grammar_table()?.into_iter().map(|g| g.name).collect();
    names.sort();
    names.dedup();
    Ok(names)
}

pub fn grammar_state(name: &str, has_static: bool) -> GrammarState {
    if has_static {
        return GrammarState::Static;
    }
    let lib = grammar_lib_path(name);
    if lib.is_file() {
        return GrammarState::Built(lib);
    }
    let src = grammar_src_dir(name);
    if src.join(".git").exists() || src.join("src").is_dir() {
        return GrammarState::Fetched(src);
    }
    GrammarState::Missing
}

/// Clone one grammar repository at the pinned revision. Does not build.
pub fn fetch_grammar(name: &str) -> Result<PathBuf> {
    fetch_grammar_with(name, &mut |_| {})
}

/// Same as [`fetch_grammar`], reporting each step and git stderr line.
pub fn fetch_grammar_with(name: &str, report: &mut dyn FnMut(GrammarEvent)) -> Result<PathBuf> {
    let entry = find_grammar(name)?;
    report(GrammarEvent::Info(format!("source  {}", entry.source.git)));
    report(GrammarEvent::Info(format!("rev     {}", entry.source.rev)));
    let dir = grammar_src_dir(&entry.name);
    report(GrammarEvent::Step("Prepare directory".into()));
    if dir.exists() {
        fs::remove_dir_all(&dir)?;
    }
    if let Some(parent) = dir.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::create_dir_all(&dir)?;
    report(GrammarEvent::Ok);

    report(GrammarEvent::Step("Initialize repository".into()));
    git(&dir, &["init"], report)?;
    git(
        &dir,
        &["remote", "add", "origin", &entry.source.git],
        report,
    )?;
    report(GrammarEvent::Ok);

    report(GrammarEvent::Step("Clone pinned revision".into()));
    let fetch = git(
        &dir,
        &[
            "fetch",
            "--depth",
            "1",
            "--progress",
            "origin",
            &entry.source.rev,
        ],
        report,
    );
    if fetch.is_err() {
        git(
            &dir,
            &["fetch", "--progress", "origin", &entry.source.rev],
            report,
        )?;
    }
    report(GrammarEvent::Ok);

    report(GrammarEvent::Step("Checkout revision".into()));
    git(&dir, &["checkout", &entry.source.rev], report)?;
    report(GrammarEvent::Ok);
    Ok(dir)
}

/// Compile a previously fetched grammar into a shared library.
pub fn build_grammar(name: &str) -> Result<PathBuf> {
    build_grammar_with(name, &mut |_| {})
}

/// Same as [`build_grammar`], reporting compile steps.
pub fn build_grammar_with(name: &str, report: &mut dyn FnMut(GrammarEvent)) -> Result<PathBuf> {
    let entry = find_grammar(name)?;
    let src_root = grammar_src_dir(&entry.name);
    if !src_root.exists() {
        return Err(anyhow!(
            "grammar '{}' is not fetched. Run: s0 --grammar fetch {}",
            entry.name,
            entry.name
        ));
    }
    let src_dir = match &entry.source.subpath {
        Some(sub) => src_root.join(sub),
        None => src_root.clone(),
    };
    report(GrammarEvent::Step("Locate parser.c".into()));
    let parser = src_dir.join("src").join("parser.c");
    if !parser.is_file() {
        return Err(anyhow!("missing parser.c in {}", src_dir.display()));
    }
    report(GrammarEvent::Ok);
    let out = grammar_lib_path(&entry.name);
    if let Some(parent) = out.parent() {
        fs::create_dir_all(parent)?;
    }
    report(GrammarEvent::Step("Compile shared library".into()));
    compile_parser(&src_dir, &parser, &out, report)?;
    report(GrammarEvent::Ok);
    report(GrammarEvent::Info(format!("library {}", out.display())));
    Ok(out)
}

/// Fetch then build a single language.
pub fn install_grammar(name: &str) -> Result<PathBuf> {
    install_grammar_with(name, &mut |_| {})
}

/// Fetch then build, reporting both phases.
pub fn install_grammar_with(name: &str, report: &mut dyn FnMut(GrammarEvent)) -> Result<PathBuf> {
    fetch_grammar_with(name, report)?;
    build_grammar_with(name, report)
}

/// Progress events for the themed CLI card.
#[derive(Debug, Clone)]
pub enum GrammarEvent {
    Info(String),
    Step(String),
    Detail(String),
    Ok,
}

pub fn remove_grammar(name: &str) -> Result<()> {
    let entry = find_grammar(name).unwrap_or_else(|_| GrammarEntry {
        name: name.to_string(),
        source: GrammarSource {
            git: String::new(),
            rev: String::new(),
            subpath: None,
        },
    });
    let src = grammar_src_dir(&entry.name);
    let lib = grammar_lib_path(&entry.name);
    if src.exists() {
        fs::remove_dir_all(&src)?;
    }
    if lib.exists() {
        fs::remove_file(&lib)?;
    }
    Ok(())
}

/// Load a built grammar. The library is leaked for the process lifetime so
/// the `Language` pointer stays valid.
pub fn load_dynamic_language(name: &str) -> Result<tree_sitter::Language> {
    let path = grammar_lib_path(name);
    if !path.is_file() {
        return Err(anyhow!("grammar library not built: {}", path.display()));
    }
    unsafe {
        let lib = Library::new(&path)?;
        let symbol = grammar_symbol(name);
        let func: libloading::Symbol<unsafe extern "C" fn() -> *const ()> =
            match lib.get(symbol.as_bytes()) {
                Ok(func) => func,
                Err(err) => {
                    drop(lib);
                    return Err(anyhow!("grammar '{name}' missing symbol {symbol}: {err}"));
                }
            };
        let raw = func();
        if raw.is_null() {
            drop(lib);
            return Err(anyhow!("grammar '{name}' exported a null language"));
        }
        let lang = tree_sitter::Language::from_raw(raw.cast());
        std::mem::forget(lib);
        Ok(lang)
    }
}

pub fn grammar_symbol(name: &str) -> String {
    let mut sym = String::from("tree_sitter_");
    for c in name.chars() {
        sym.push(if c == '-' { '_' } else { c });
    }
    sym
}

fn compile_parser(
    src_dir: &Path,
    parser: &Path,
    out: &Path,
    report: &mut dyn FnMut(GrammarEvent),
) -> Result<()> {
    let compiler = std::env::var("CC").unwrap_or_else(|_| "cc".to_string());
    let header = src_dir.join("src");
    let mut cmd = Command::new(&compiler);
    cmd.current_dir(src_dir);
    if cfg!(windows) {
        cmd.args(["/nologo", "/LD", "/O2", "/utf-8"]);
        cmd.arg(format!("/I{}", header.display()));
        cmd.arg(parser);
        if let Some(scanner) = scanner_path(src_dir) {
            cmd.arg(scanner);
        }
        cmd.arg(format!("/Fe:{}", out.display()));
    } else {
        let shared = if cfg!(target_os = "macos") {
            "-dynamiclib"
        } else {
            "-shared"
        };
        cmd.args([shared, "-fPIC", "-fvisibility=hidden", "-O3", "-I"])
            .arg(&header)
            .arg(parser);
        if let Some(scanner) = scanner_path(src_dir) {
            cmd.arg(scanner);
        }
        cmd.arg("-o").arg(out);
    }
    report(GrammarEvent::Detail(format!("compiler {compiler}")));
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd
        .spawn()
        .map_err(|e| anyhow!("failed to run {compiler}: {e}"))?;
    let stderr = child.stderr.take();
    let handle = thread::spawn(move || {
        let mut lines = Vec::new();
        if let Some(stderr) = stderr {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                lines.push(line);
            }
        }
        lines
    });
    let status = child
        .wait()
        .map_err(|e| anyhow!("compiler wait failed: {e}"))?;
    let lines = handle.join().unwrap_or_default();
    for line in &lines {
        report(GrammarEvent::Detail(line.clone()));
    }
    if !status.success() {
        return Err(anyhow!(
            "grammar compile failed ({compiler}): {}",
            lines.join(" ")
        ));
    }
    Ok(())
}

fn scanner_path(src_dir: &Path) -> Option<PathBuf> {
    for name in ["scanner.c", "scanner.cc", "scanner.cpp"] {
        let p = src_dir.join("src").join(name);
        if p.is_file() {
            return Some(p);
        }
    }
    None
}

fn git(dir: &Path, args: &[&str], report: &mut dyn FnMut(GrammarEvent)) -> Result<String> {
    let mut child = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| anyhow!("git is not available: {e}"))?;
    let stderr = child.stderr.take();
    let handle = thread::spawn(move || {
        let mut lines = Vec::new();
        if let Some(stderr) = stderr {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                lines.push(line);
            }
        }
        lines
    });
    let status = child.wait().map_err(|e| anyhow!("git wait failed: {e}"))?;
    let lines = handle.join().unwrap_or_default();
    for line in &lines {
        report(GrammarEvent::Detail(line.clone()));
    }
    if !status.success() {
        return Err(anyhow!(
            "git {} failed: {}",
            args.join(" "),
            lines.join(" ")
        ));
    }
    Ok(String::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn symbol_replaces_hyphens() {
        assert_eq!(grammar_symbol("c-sharp"), "tree_sitter_c_sharp");
        assert_eq!(grammar_symbol("rust"), "tree_sitter_rust");
    }

    #[test]
    fn lib_name_uses_platform_prefix() {
        let path = grammar_lib_path("rust");
        let file = path.file_name().unwrap().to_string_lossy();
        assert!(file.contains("rust"));
        assert!(file.starts_with(std::env::consts::DLL_PREFIX) || cfg!(windows));
    }

    #[test]
    fn missing_state_without_files() {
        let state = grammar_state("not-a-real-grammar-xyz", false);
        assert_eq!(state, GrammarState::Missing);
    }

    #[test]
    fn static_state_wins() {
        assert_eq!(grammar_state("rust", true), GrammarState::Static);
    }
}
