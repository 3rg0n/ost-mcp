//! Finding the mailbox file when the caller does not name one.
//!
//! Outlook records the full path of every store it has open in the profile
//! registry, so that is what this reads. The filename is not derivable: it is
//! usually the account UPN, but it is whatever the profile says, and a directory
//! scan cannot tell a primary mailbox from a leftover copy or from the `.nst`
//! Groups store sitting beside it. Scanning the directory is kept only as a
//! fallback for a file no profile mentions.
//!
//! The keys, as measured on Outlook 2021 (Office 16.0):
//!
//! ```text
//! HKCU\Software\Microsoft\Office\<ver>\Outlook
//!   DefaultProfile                 = REG_SZ, the profile Outlook opens
//!   Profiles\<profile>\<service>
//!     001f6610                     = REG_BINARY, UTF-16 store path
//!     001f3001                     = REG_BINARY, UTF-16 account display name
//! ```
//!
//! `<service>` is an opaque key name, not the `9375CFF0…\0000000x` layout MAPI
//! documents — both appear, so the whole profile subtree is walked rather than
//! assuming either shape.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// A store this machine says exists.
#[derive(Clone, Debug)]
pub struct Found {
    pub path: PathBuf,
    pub bytes: u64,
    /// The account the profile files this store under, when it came from one.
    pub account: Option<String>,
    /// `profile "Outlook"`, or `directory` for a file found by scanning.
    pub source: String,
}

/// Every store worth opening, best first: the default profile's stores, then the
/// other profiles', then anything left in the Outlook directory.
pub fn stores() -> Vec<Found> {
    let mut seen = HashSet::new();
    from_profiles()
        .into_iter()
        .chain(from_directory())
        .filter(|f| seen.insert(f.path.to_string_lossy().to_lowercase()))
        .collect()
}

/// The store to open when none was given on the command line.
pub fn primary() -> Option<PathBuf> {
    stores().into_iter().next().map(|f| f.path)
}

/// `%LOCALAPPDATA%\Microsoft\Outlook`, where Outlook 2013+ puts OST files.
pub fn default_dir() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA").map(|d| Path::new(&d).join("Microsoft").join("Outlook"))
}

/// A path only becomes a candidate once it resolves to a file, which is also how
/// a profile entry left behind by a removed account gets dropped.
fn record(path: PathBuf, account: Option<String>, source: String) -> Option<Found> {
    Some(Found {
        bytes: std::fs::metadata(&path).ok()?.len(),
        path,
        account,
        source,
    })
}

fn is_store(path: &str) -> bool {
    let lower = path.to_lowercase();
    lower.ends_with(".ost") || lower.ends_with(".pst")
}

/// Every `.ost` and `.pst` in the default Outlook directory, largest first: a
/// secondary store is generally much smaller than the primary one.
fn from_directory() -> Vec<Found> {
    let Some(dir) = default_dir() else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut found: Vec<Found> = entries
        .flatten()
        .filter(|e| is_store(&e.file_name().to_string_lossy()))
        .filter_map(|e| record(e.path(), None, "directory".to_string()))
        .collect();
    found.sort_by_key(|f| std::cmp::Reverse(f.bytes));
    found
}

#[cfg(windows)]
fn from_profiles() -> Vec<Found> {
    use windows_registry::CURRENT_USER;

    let mut out = Vec::new();
    let Ok(office) = CURRENT_USER.open(r"Software\Microsoft\Office") else {
        return out;
    };
    let Ok(versions) = office.keys() else {
        return out;
    };
    // Several Office versions can be registered at once, and only some of them
    // have an Outlook profile; a version without one is skipped by the open.
    for version in versions {
        let Ok(outlook) = office.open(format!(r"{version}\Outlook")) else {
            continue;
        };
        let Ok(profiles) = outlook.open("Profiles") else {
            continue;
        };
        let Ok(names) = profiles.keys() else {
            continue;
        };
        let default = outlook.get_string("DefaultProfile").ok();
        let mut names: Vec<String> = names.collect();
        // The default profile is the one Outlook actually opened, so it leads.
        names.sort_by_key(|n| {
            !default
                .as_deref()
                .is_some_and(|d| d.eq_ignore_ascii_case(n))
        });
        for name in names {
            let Ok(profile) = profiles.open(&name) else {
                continue;
            };
            let mut paths = Vec::new();
            walk(&profile, 3, &mut paths);
            let source = format!("profile {name:?}");
            out.extend(
                paths
                    .into_iter()
                    .filter_map(|(p, account)| record(p.into(), account, source.clone())),
            );
        }
    }
    out
}

/// Collect the store paths in one profile subtree.
///
/// Any value whose text names a `.ost` or `.pst` file is a store path.
/// `001f6610` (`PR_PROFILE_OFFLINE_STORE_PATH`) is the tag for a cached Exchange
/// mailbox, but an added PST is filed under other tags, and matching on the
/// extension picks up every one of them without guessing at tag numbers. It also
/// leaves the `.nst` Groups store alone, which this reader cannot parse.
#[cfg(windows)]
fn walk(key: &windows_registry::Key, depth: u32, out: &mut Vec<(String, Option<String>)>) {
    // `PR_DISPLAY_NAME` of the service, which is the account the store belongs to.
    let account = value_text(key, "001f3001");
    if let Ok(values) = key.values() {
        for (_, value) in values {
            match decode(&value) {
                Some(text) if is_store(&text) => out.push((text, account.clone())),
                _ => {}
            }
        }
    }
    if depth == 0 {
        return;
    }
    if let Ok(subs) = key.keys() {
        for sub in subs {
            if let Ok(child) = key.open(&sub) {
                walk(&child, depth - 1, out);
            }
        }
    }
}

#[cfg(windows)]
fn value_text(key: &windows_registry::Key, name: &str) -> Option<String> {
    decode(&key.get_value(name).ok()?).filter(|s| !s.is_empty())
}

/// A profile stores its strings as `REG_BINARY` holding NUL-terminated UTF-16,
/// not as `REG_SZ`. Both decode the same way; anything numeric is not a path.
#[cfg(windows)]
fn decode(value: &windows_registry::Value) -> Option<String> {
    use windows_registry::Type;
    match value.ty() {
        Type::String | Type::ExpandString | Type::Bytes => {
            let wide = value.as_wide();
            let end = wide.iter().position(|&c| c == 0).unwrap_or(wide.len());
            Some(String::from_utf16_lossy(&wide[..end]))
        }
        _ => None,
    }
}

#[cfg(not(windows))]
fn from_profiles() -> Vec<Found> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognises_store_extensions() {
        assert!(is_store(r"C:\x\user@example.com.ost"));
        assert!(is_store("Archive.PST"));
        // The Groups store sits next to the OST and is not one.
        assert!(!is_store(r"C:\x\user@example.com.nst"));
        assert!(!is_store("notes.txt"));
    }
}
