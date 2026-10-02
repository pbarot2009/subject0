//! Crash-safe file writes and project-root discovery.
//!
//! Saves follow the usual editor pattern: write a temp file in the same directory,
//! fsync it, preserve the destination mode, then rename. The parent directory is
//! fsynced so a power loss cannot leave the new name pointing at a partial file.

use std::{
    env,
    fs::{self, File, OpenOptions},
    io::{BufWriter, Write},
    path::{Path, PathBuf},
};

use anyhow::Result;

const PROJECT_MARKERS: &[&str] = &[
    ".git",
    "Cargo.toml",
    "go.mod",
    "package.json",
    "pyproject.toml",
    "pom.xml",
    "composer.json",
    "Gemfile",
];

/// Walks upward from `path` until a project marker is found.
pub fn find_project_root(path: &Path) -> PathBuf {
    let start = if path.is_dir() {
        path.to_path_buf()
    } else {
        path.parent()
            .filter(|p| !p.as_os_str().is_empty())
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."))
    };

    let mut cur = if start.is_absolute() {
        start.clone()
    } else {
        env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(&start)
    };

    loop {
        for marker in PROJECT_MARKERS {
            if cur.join(marker).exists() {
                return cur;
            }
        }
        if !cur.pop() {
            break;
        }
    }

    if start.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        start
    }
}

/// Writes `write` to `path` atomically and keeps the existing permission bits.
pub fn atomic_write_with(path: &Path, write: impl FnOnce(&mut dyn Write) -> Result<()>) -> Result<()> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)?;
    }

    let perms = fs::metadata(path).ok().map(|meta| meta.permissions());
    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("s0_buffer");
    let tmp_path = path.with_file_name(format!(".{file_name}.s0tmp"));

    {
        let file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&tmp_path)?;
        let mut writer = BufWriter::new(file);
        write(&mut writer)?;
        writer.flush()?;
        writer.get_ref().sync_all()?;
    }

    if let Some(perms) = perms {
        let _ = fs::set_permissions(&tmp_path, perms);
    }

    if fs::rename(&tmp_path, path).is_err() {
        fs::copy(&tmp_path, path)?;
        let _ = fs::remove_file(&tmp_path);
        if let Ok(file) = File::open(path) {
            let _ = file.sync_all();
        }
    }

    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
        && let Ok(dir) = File::open(parent)
    {
        let _ = dir.sync_all();
    }

    Ok(())
}

/// Modified time and length used to detect external changes before overwrite.
pub fn file_stamp(path: &Path) -> Option<(std::time::SystemTime, u64)> {
    let meta = fs::metadata(path).ok()?;
    Some((meta.modified().ok()?, meta.len()))
}
