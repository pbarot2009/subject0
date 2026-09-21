//! # Git Version Control & Real-Time Diff Subsystem
//!
//! Provides Git repository tracking, branch status, and line-level diffing
//! against the `HEAD` commit using pure Rust [`gix`] and [`imara_diff`]:
//!
//! 1. **Zero-Lag Worker Model ([`run_git_actor`])**:
//!    Executes repository discovery, object database lookups, and diffing on a
//!    background worker task without blocking the Ratatui render loop.
//!
//! 2. **In-Memory Myers Diffing ([`imara_diff`])**:
//!    Compares live editor buffer text against cached `HEAD` blobs in sub-millisecond
//!    time, producing exact gutter markers and hunk navigation targets.
//!
//! 3. **Dual-Tier Engine Resilience**:
//!    Prioritizes pure-Rust `gix` for in-memory speed, and gracefully falls back
//!    to system `git` CLI on platforms with complex sandbox or symlink layouts (e.g. Android Termux).

use std::{
    collections::HashMap,
    env,
    path::{Path, PathBuf},
    process::Command,
};

use imara_diff::intern::InternedInput;
use imara_diff::{Algorithm, Sink, diff};
use tokio::sync::mpsc;

use crate::lsp::resolve_binary_path;

/// Category of line-level difference between the live buffer and `HEAD`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GutterChange {
    /// Newly inserted lines not present in `HEAD`.
    Added,
    /// Existing lines modified from `HEAD`.
    Modified,
    /// Lines removed from `HEAD` at this line boundary.
    Deleted,
}

/// A unified line diff hunk representing a contiguous set of changes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitHunk {
    /// 0-based starting line index in the `HEAD` baseline blob.
    pub before_start: usize,
    /// Number of lines in the `HEAD` baseline.
    pub before_len: usize,
    /// 0-based starting line index in the live editor buffer.
    pub after_start: usize,
    /// Number of lines in the live editor buffer.
    pub after_len: usize,
    /// Classification of change.
    pub kind: GutterChange,
    /// Original lines from `HEAD` used for hunk reversion.
    pub head_text: String,
}

/// Complete Git diff state and repository metadata for an active buffer.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GitDiffSummary {
    /// Active branch name or short commit SHA if detached.
    pub branch: Option<String>,
    /// Sorted list of contiguous diff hunks.
    pub hunks: Vec<GitHunk>,
    /// Total lines added in the current buffer.
    pub added_lines: usize,
    /// Total lines modified in the current buffer.
    pub modified_lines: usize,
    /// Total lines deleted in the current buffer.
    pub deleted_lines: usize,
}

impl GitDiffSummary {
    /// Returns the gutter marker for a given 0-based buffer line, if modified.
    pub fn change_for_line(&self, line: usize) -> Option<GutterChange> {
        let mut deletion_marker = false;

        for hunk in &self.hunks {
            match hunk.kind {
                GutterChange::Added | GutterChange::Modified => {
                    if line >= hunk.after_start && line < hunk.after_start + hunk.after_len {
                        return Some(hunk.kind);
                    }
                }
                GutterChange::Deleted => {
                    if line == hunk.after_start {
                        deletion_marker = true;
                    }
                }
            }
        }

        if deletion_marker {
            Some(GutterChange::Deleted)
        } else {
            None
        }
    }

    /// Finds the diff hunk encompassing or adjacent to the specified line number.
    pub fn hunk_at_line(&self, line: usize) -> Option<&GitHunk> {
        self.hunks.iter().find(|h| {
            if h.after_len == 0 {
                line == h.after_start
            } else {
                line >= h.after_start && line < h.after_start + h.after_len
            }
        })
    }

    /// Locates the next hunk starting line strictly after `current_line` (wraps around).
    pub fn next_hunk_line(&self, current_line: usize) -> Option<usize> {
        if self.hunks.is_empty() {
            return None;
        }

        self.hunks
            .iter()
            .map(|h| h.after_start)
            .find(|&start| start > current_line)
            .or_else(|| self.hunks.first().map(|h| h.after_start))
    }

    /// Locates the previous hunk starting line strictly before `current_line` (wraps around).
    pub fn prev_hunk_line(&self, current_line: usize) -> Option<usize> {
        if self.hunks.is_empty() {
            return None;
        }

        self.hunks
            .iter()
            .rev()
            .map(|h| h.after_start)
            .find(|&start| start < current_line)
            .or_else(|| self.hunks.last().map(|h| h.after_start))
    }
}

// === Diff Sink Implementation ===

struct HunkSink<'a> {
    before_lines: &'a [&'a str],
    hunks: Vec<GitHunk>,
}

impl<'a> Sink for HunkSink<'a> {
    type Out = Vec<GitHunk>;

    fn process_change(&mut self, before: std::ops::Range<u32>, after: std::ops::Range<u32>) {
        let (b_start, b_end) = (before.start as usize, before.end as usize);
        let (a_start, a_end) = (after.start as usize, after.end as usize);

        let b_len = b_end.saturating_sub(b_start);
        let a_len = a_end.saturating_sub(a_start);

        let kind = if b_len == 0 && a_len > 0 {
            GutterChange::Added
        } else if b_len > 0 && a_len == 0 {
            GutterChange::Deleted
        } else {
            GutterChange::Modified
        };

        let head_text = if b_len > 0 && b_start < self.before_lines.len() {
            let slice_end = b_end.min(self.before_lines.len());
            let mut text = self.before_lines[b_start..slice_end].join("\n");
            text.push('\n');
            text
        } else {
            String::new()
        };

        self.hunks.push(GitHunk {
            before_start: b_start,
            before_len: b_len,
            after_start: a_start,
            after_len: a_len,
            kind,
            head_text,
        });
    }

    fn finish(self) -> Self::Out {
        self.hunks
    }
}

// === Path & Repository Resolution Helpers ===

/// Resolves the starting directory for Git discovery, ensuring files are never passed directly.
fn resolve_search_directory(path: &Path) -> PathBuf {
    let cwd = env::current_dir().unwrap_or_else(|_| PathBuf::from("."));

    if path.is_dir() {
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            cwd.join(path)
        }
    } else if let Some(parent) = path.parent() {
        if parent.as_os_str().is_empty() {
            cwd
        } else if parent.is_absolute() {
            parent.to_path_buf()
        } else {
            cwd.join(parent)
        }
    } else {
        cwd
    }
}

/// Normalizes relative paths between worktrees and files across Linux, macOS, Windows, and Termux.
fn find_relative_path(workdir: &Path, file_path: &Path) -> Option<PathBuf> {
    if let Ok(rel) = file_path.strip_prefix(workdir) {
        return Some(rel.to_path_buf());
    }

    let cwd = env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let abs_file = if file_path.is_absolute() {
        file_path.to_path_buf()
    } else {
        cwd.join(file_path)
    };
    let abs_workdir = if workdir.is_absolute() {
        workdir.to_path_buf()
    } else {
        cwd.join(workdir)
    };

    if let Ok(rel) = abs_file.strip_prefix(&abs_workdir) {
        return Some(rel.to_path_buf());
    }

    if let (Ok(c_file), Ok(c_work)) = (abs_file.canonicalize(), abs_workdir.canonicalize()) {
        if let Ok(rel) = c_file.strip_prefix(&c_work) {
            return Some(rel.to_path_buf());
        }
    }

    // Handle Termux Android symlink aliases (/data/data vs /data/user/0)
    let s_file = abs_file
        .to_string_lossy()
        .replace("/data/user/0/", "/data/data/");
    let s_work = abs_workdir
        .to_string_lossy()
        .replace("/data/user/0/", "/data/data/");
    if let Some(rel) = s_file.strip_prefix(&s_work) {
        let clean = rel.trim_start_matches('/');
        return Some(PathBuf::from(clean));
    }

    None
}

/// Resolves the current branch or short commit SHA from a `gix::Repository`.
fn resolve_branch_name(repo: &gix::Repository) -> Option<String> {
    let head = repo.head().ok()?;
    match head.referent_name() {
        Some(ref_name) => Some(ref_name.shorten().to_string()),
        None => head.id().map(|id| id.to_hex_with_len(7).to_string()),
    }
}

/// Resolves and reads raw file bytes from the `HEAD` commit tree in `gix`.
fn read_head_blob(repo: &gix::Repository, unix_path: &str) -> Option<Vec<u8>> {
    let head_commit = repo.head_commit().ok()?;
    let tree = head_commit.tree().ok()?;
    let clean_path = unix_path.trim_start_matches('/');
    let entry = tree.lookup_entry_by_path(clean_path).ok()??;
    let object = entry.object().ok()?;
    Some(object.data.to_vec())
}

/// Computes hunks between baseline text and buffer text using `imara-diff`.
pub fn compute_hunks_from_text(head_text: &str, buffer_text: &str) -> Vec<GitHunk> {
    let clean_head = head_text.replace("\r\n", "\n");
    let clean_buf = buffer_text.replace("\r\n", "\n");

    let head_lines: Vec<&str> = if clean_head.is_empty() {
        Vec::new()
    } else {
        clean_head.split('\n').collect()
    };

    let input = InternedInput::new(clean_head.as_str(), clean_buf.as_str());
    let sink = HunkSink {
        before_lines: &head_lines,
        hunks: Vec::new(),
    };

    diff(Algorithm::Histogram, &input, sink)
}

// === CLI Fallback Engine for Complex Platform Layouts ===

fn diff_via_cli(path: &Path, buffer_text: &str, search_dir: &Path) -> Option<GitDiffSummary> {
    let git_bin = resolve_binary_path("git")?;

    let toplevel_out = Command::new(&git_bin)
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(search_dir)
        .output()
        .ok()?;

    if !toplevel_out.status.success() {
        return None;
    }

    let workdir_str = String::from_utf8_lossy(&toplevel_out.stdout)
        .trim()
        .to_string();
    let workdir = PathBuf::from(&workdir_str);

    let branch_out = Command::new(&git_bin)
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .current_dir(search_dir)
        .output()
        .ok();

    let branch = branch_out.and_then(|o| {
        if o.status.success() {
            let b = String::from_utf8_lossy(&o.stdout).trim().to_string();
            if b.is_empty() || b == "HEAD" {
                None
            } else {
                Some(b)
            }
        } else {
            None
        }
    });

    let rel_path = find_relative_path(&workdir, path)?;
    let unix_path = rel_path.to_str()?.replace('\\', "/");
    let clean_unix_path = unix_path.trim_start_matches('/');

    let show_out = Command::new(&git_bin)
        .args(["show", &format!("HEAD:{clean_unix_path}")])
        .current_dir(&workdir)
        .output()
        .ok();

    let head_text = if let Some(o) = show_out {
        if o.status.success() {
            String::from_utf8_lossy(&o.stdout).to_string()
        } else {
            String::new()
        }
    } else {
        String::new()
    };

    let hunks = compute_hunks_from_text(&head_text, buffer_text);
    let mut added_lines = 0;
    let mut modified_lines = 0;
    let mut deleted_lines = 0;

    for hunk in &hunks {
        match hunk.kind {
            GutterChange::Added => added_lines += hunk.after_len,
            GutterChange::Modified => modified_lines += hunk.after_len,
            GutterChange::Deleted => deleted_lines += hunk.before_len,
        }
    }

    Some(GitDiffSummary {
        branch,
        hunks,
        added_lines,
        modified_lines,
        deleted_lines,
    })
}

// === Unified Diff Computation Entry Point ===

/// Inspects a repository on disk, resolves `HEAD` contents, and builds a [`GitDiffSummary`].
pub fn compute_diff(path: &Path, buffer_text: &str) -> GitDiffSummary {
    let search_dir = resolve_search_directory(path);
    let cwd = env::current_dir().unwrap_or_else(|_| PathBuf::from("."));

    // 1. Attempt Pure-Rust Gitoxide discovery starting from the directory
    let gix_repo = gix::discover(&search_dir).or_else(|_| gix::discover(&cwd));

    if let Ok(repo) = gix_repo {
        let branch = resolve_branch_name(&repo);

        if let Some(workdir) = repo.work_dir() {
            if let Some(rel_path) = find_relative_path(workdir, path) {
                if let Some(unix_path) = rel_path.to_str() {
                    let head_text = match read_head_blob(&repo, unix_path) {
                        Some(bytes) => String::from_utf8_lossy(&bytes).to_string(),
                        None => String::new(),
                    };

                    let hunks = compute_hunks_from_text(&head_text, buffer_text);
                    let mut added_lines = 0;
                    let mut modified_lines = 0;
                    let mut deleted_lines = 0;

                    for hunk in &hunks {
                        match hunk.kind {
                            GutterChange::Added => added_lines += hunk.after_len,
                            GutterChange::Modified => modified_lines += hunk.after_len,
                            GutterChange::Deleted => deleted_lines += hunk.before_len,
                        }
                    }

                    return GitDiffSummary {
                        branch,
                        hunks,
                        added_lines,
                        modified_lines,
                        deleted_lines,
                    };
                }
            }
        }

        return GitDiffSummary {
            branch,
            ..Default::default()
        };
    }

    // 2. Fallback to CLI Git if gix encounters environment/sandbox permission limits
    if let Some(summary) = diff_via_cli(path, buffer_text, &search_dir) {
        return summary;
    }

    GitDiffSummary::default()
}

// === Background Worker Supervisor ===

/// Inbound messages routed to the background Git actor.
#[derive(Debug, Clone)]
pub enum GitInbound {
    /// Dispatched on buffer typing or modifications.
    UpdateBuffer { path: PathBuf, text: String },
    /// Dispatched on file open, save, or manual refresh.
    Refresh { path: PathBuf, text: String },
}

/// Outbound messages emitted by the background Git actor.
#[derive(Debug, Clone)]
pub enum GitOutbound {
    /// Fresh diff calculations and repository branch metadata.
    DiffSummary {
        path: PathBuf,
        summary: GitDiffSummary,
    },
}

/// Asynchronous background worker task supervising Git queries and diff operations.
pub async fn run_git_actor(
    mut rx: mpsc::UnboundedReceiver<GitInbound>,
    tx: mpsc::UnboundedSender<GitOutbound>,
) {
    while let Some(first_msg) = rx.recv().await {
        let mut pending_batches = HashMap::new();

        match first_msg {
            GitInbound::UpdateBuffer { path, text } | GitInbound::Refresh { path, text } => {
                pending_batches.insert(path, text);
            }
        }

        // Drain pending queue entries to coalesce rapid keystrokes
        while let Ok(newer_msg) = rx.try_recv() {
            match newer_msg {
                GitInbound::UpdateBuffer { path, text } | GitInbound::Refresh { path, text } => {
                    pending_batches.insert(path, text);
                }
            }
        }

        for (path, text) in pending_batches {
            let sender = tx.clone();
            tokio::task::spawn_blocking(move || {
                let summary = compute_diff(&path, &text);
                let _ = sender.send(GitOutbound::DiffSummary { path, summary });
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_diff_additions_and_deletions() {
        let head = "line 1\nline 2\nline 3\n";
        let buf = "line 1\nline 2 modified\nline 3\nline 4\n";

        let hunks = compute_hunks_from_text(head, buf);
        assert_eq!(hunks.len(), 2);

        assert_eq!(hunks[0].kind, GutterChange::Modified);
        assert_eq!(hunks[0].after_start, 1);
        assert_eq!(hunks[0].after_len, 1);

        assert_eq!(hunks[1].kind, GutterChange::Added);
        assert_eq!(hunks[1].after_start, 3);
        assert_eq!(hunks[1].after_len, 1);
    }

    #[test]
    fn test_hunk_navigation() {
        let summary = GitDiffSummary {
            branch: Some("main".into()),
            hunks: vec![
                GitHunk {
                    before_start: 5,
                    before_len: 2,
                    after_start: 5,
                    after_len: 2,
                    kind: GutterChange::Modified,
                    head_text: "old\n".into(),
                },
                GitHunk {
                    before_start: 15,
                    before_len: 0,
                    after_start: 15,
                    after_len: 4,
                    kind: GutterChange::Added,
                    head_text: String::new(),
                },
            ],
            added_lines: 4,
            modified_lines: 2,
            deleted_lines: 0,
        };

        assert_eq!(summary.change_for_line(5), Some(GutterChange::Modified));
        assert_eq!(summary.change_for_line(6), Some(GutterChange::Modified));
        assert_eq!(summary.change_for_line(7), None);
        assert_eq!(summary.change_for_line(15), Some(GutterChange::Added));

        assert_eq!(summary.next_hunk_line(0), Some(5));
        assert_eq!(summary.next_hunk_line(5), Some(15));
        assert_eq!(summary.next_hunk_line(15), Some(5));

        assert_eq!(summary.prev_hunk_line(20), Some(15));
        assert_eq!(summary.prev_hunk_line(15), Some(5));
        assert_eq!(summary.prev_hunk_line(5), Some(15));
    }
}
