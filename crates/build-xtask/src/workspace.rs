//! The shared walk over `crates/`: the one enumeration the architecture
//! checks use.
//!
//! A directory holding a `Cargo.toml` is a crate and is not descended into;
//! any other directory is a container and the walk descends, so the
//! manifestless `crates/gateway/` and `crates/gateway/stt/` are walked
//! through to the crates they hold.
//!
//! A manifest the walk cannot read, parse, or find a package name in is
//! reported rather than skipped: a crate that cannot be read cannot be
//! shown to satisfy anything. Exactly one check owns those failures, so
//! they are named once in `tidy::all_violations`.

use std::fs;
use std::path::{Path, PathBuf};

/// Every crate under `crates/`, plus the directories whose manifest could
/// not be read.
pub(crate) struct Walk {
    /// The crates whose manifest named a package, as directories relative
    /// to the workspace root, in path order.
    pub(crate) crates: Vec<PathBuf>,
    /// Crate directories, relative to the workspace root, whose manifest
    /// could not be read, parsed, or named.
    pub(crate) unread: Vec<PathBuf>,
    /// Every read failure, in walk order.
    pub(crate) violations: Vec<String>,
}

/// Every crate directory under `crates/`, in path order.
pub(crate) fn crates(root: &Path) -> Walk {
    let mut walk = Walk {
        crates: Vec::new(),
        unread: Vec::new(),
        violations: Vec::new(),
    };
    walk_dir(root, &root.join("crates"), &mut walk);
    walk.crates.sort();
    walk.unread.sort();
    walk
}

/// Walks one directory level: crates are read, manifestless containers are
/// descended into.
fn walk_dir(root: &Path, dir: &Path, walk: &mut Walk) {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) => {
            walk.violations.push(format!(
                "{}: unreadable crates directory: {error}",
                dir.display()
            ));
            return;
        }
    };
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                walk.violations.push(format!(
                    "{}: unreadable directory entry: {error}",
                    dir.display()
                ));
                continue;
            }
        };
        if !entry.path().is_dir() {
            continue;
        }
        let sub = entry.path();
        if sub.join("Cargo.toml").exists() {
            read_crate(root, &sub, walk);
        } else {
            walk_dir(root, &sub, walk);
        }
    }
}

/// Reads one crate's manifest into the walk; a failure names the crate in
/// `unread` and the failure mode in `violations`.
fn read_crate(root: &Path, dir: &Path, walk: &mut Walk) {
    let relative = dir.strip_prefix(root).unwrap_or(dir).to_path_buf();
    let manifest_path = dir.join("Cargo.toml");
    let text = match fs::read_to_string(&manifest_path) {
        Ok(text) => text,
        Err(error) => {
            walk.unread.push(relative);
            walk.violations.push(format!(
                "{}: unreadable manifest: {error}",
                manifest_path.display()
            ));
            return;
        }
    };
    let manifest = match toml::from_str::<toml::Value>(&text) {
        Ok(manifest) => manifest,
        Err(error) => {
            walk.unread.push(relative);
            walk.violations.push(format!(
                "{}: unparseable manifest: {error}",
                manifest_path.display()
            ));
            return;
        }
    };
    let named = manifest
        .get("package")
        .and_then(|package| package.get("name"))
        .and_then(toml::Value::as_str)
        .is_some();
    if named {
        walk.crates.push(relative);
    } else {
        walk.unread.push(relative);
        walk.violations.push(format!(
            "{}: manifest has no package name",
            manifest_path.display()
        ));
    }
}

/// The workspace root, found from this crate's own location, for the checks
/// that run over the real tree.
#[cfg(test)]
pub(crate) fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("build-xtask lives at <root>/crates/build-xtask")
        .to_path_buf()
}

#[cfg(test)]
#[path = "workspace-tests.rs"]
mod tests;
