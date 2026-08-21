//! Finding a profile's `Data` directory without hardcoding a name.
//!
//! Measured on a live machine (`docs/mac-outlook-format.md` §1): the identity
//! directory is `Main Identity`, not the `Main Profile` name every forum
//! source and vendor reference claims, and the `Outlook 15 Profiles` name
//! above it is itself a version-era label that a later build could move. So
//! this globs both levels instead of assuming either name, the Mac
//! counterpart of the Windows registry resolver in
//! `crates/ost-mcp/src/discover.rs`.
//!
//! Only this module is `#[cfg(target_os = "macos")]`; the reader in
//! [`crate::schema`] and [`crate::olk15`] is portable, so a copied profile
//! can be opened and tested on any OS.

use std::path::PathBuf;

/// Every profile's `Data` directory found under the Outlook group container,
/// largest `Outlook.sqlite` first as a proxy for "most likely the one in
/// use" — there is no registry-equivalent default-profile marker measured
/// yet (see `docs/mac-outlook-format.md` U8).
#[cfg(target_os = "macos")]
pub fn data_dirs() -> Vec<PathBuf> {
    let Some(home) = std::env::var_os("HOME") else {
        return Vec::new();
    };
    let container = PathBuf::from(home)
        .join("Library/Group Containers/UBF8T346G9.Office/Outlook");
    let Ok(profile_roots) = std::fs::read_dir(&container) else {
        return Vec::new();
    };

    let mut found: Vec<(u64, PathBuf)> = Vec::new();
    for root in profile_roots.flatten() {
        let Ok(identities) = std::fs::read_dir(root.path()) else {
            continue;
        };
        for identity in identities.flatten() {
            let data = identity.path().join("Data");
            let db = data.join("Outlook.sqlite");
            if let Ok(meta) = std::fs::metadata(&db) {
                found.push((meta.len(), data));
            }
        }
    }
    found.sort_by_key(|(len, _)| std::cmp::Reverse(*len));
    found.into_iter().map(|(_, dir)| dir).collect()
}

#[cfg(not(target_os = "macos"))]
pub fn data_dirs() -> Vec<PathBuf> {
    Vec::new()
}

/// The profile to open when none was named explicitly.
#[cfg(target_os = "macos")]
pub fn primary() -> Option<PathBuf> {
    data_dirs().into_iter().next()
}

#[cfg(not(target_os = "macos"))]
pub fn primary() -> Option<PathBuf> {
    None
}
