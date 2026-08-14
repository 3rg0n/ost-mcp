//! Finding the mailbox file when the caller does not name one.
//!
//! Outlook keeps the cached-Exchange OST for every profile in one directory, so
//! there is usually exactly one candidate. When there are several, the largest
//! is returned first: a secondary store (a shared mailbox, an archive) is
//! generally much smaller than the primary one.

use std::path::{Path, PathBuf};

/// `%LOCALAPPDATA%\Microsoft\Outlook`, where Outlook 2013+ puts OST files.
pub fn default_dir() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA").map(|d| Path::new(&d).join("Microsoft").join("Outlook"))
}

/// Every `.ost` and `.pst` in `dir`, largest first.
pub fn candidates_in(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut found: Vec<(u64, PathBuf)> = entries
        .flatten()
        .filter_map(|e| {
            let path = e.path();
            let ext = path.extension()?.to_str()?.to_ascii_lowercase();
            if ext != "ost" && ext != "pst" {
                return None;
            }
            Some((e.metadata().map(|m| m.len()).unwrap_or(0), path))
        })
        .collect();
    found.sort_by_key(|(bytes, _)| std::cmp::Reverse(*bytes));
    found.into_iter().map(|(_, p)| p).collect()
}

/// The store to open when none was given on the command line.
pub fn primary() -> Option<PathBuf> {
    candidates_in(&default_dir()?).into_iter().next()
}
