//! Tidy-style architecture checks for the gateway workspace: the lint
//! inheritance check, the ceiling wiring, the unsafe allowlist, the doctest
//! ban, the `Debug` spelling rule, and the walled admin tier.
//!
//! Each check returns a list of human-readable violations. The `#[test]`
//! wrappers assert the lists are empty, so `cargo test -p build-xtask`
//! enforces the architecture; `cargo xtask tidy` prints the same report on
//! demand.
//!
//! Two of these bind every workspace crate: the lint inheritance check and
//! the ceiling wiring, which requires every crate's build script to run
//! `build_ceiling::check()`, the compile-time 500-line file limit. The
//! shared walk's read failures are reported here, in one place.

use std::fs;
use std::path::{Path, PathBuf};

#[path = "tidy-wiring.rs"]
mod wiring;

/// Runs every check and returns all violations.
#[must_use]
pub(crate) fn all_violations(root: &Path) -> Vec<String> {
    let mut violations = manifest_violations(root);
    violations.extend(lint_inheritance_violations(root));
    violations.extend(wiring::ceiling_wiring_violations(root));
    violations.extend(crate::unsafe_allowlist::unsafe_allowlist_violations(root));
    violations.extend(walled_tier_violations(root));
    violations.extend(crate::no_doctests::no_doctests_violations(root));
    violations.extend(crate::debug_spelling::debug_spelling_violations(root));
    violations
}

/// The manifests the shared walk could not read, parse, or name. Every other
/// check shares the walk and leaves these here, so each failure is named
/// once in [`all_violations`].
#[must_use]
fn manifest_violations(root: &Path) -> Vec<String> {
    crate::workspace::crates(root).violations
}

/// Checks that every workspace crate, unread ones included, inherits
/// `[lints] workspace = true` (which includes `unreachable_pub`) and that
/// the workspace root sets it. A walk that found no crate fails: a crate
/// that was never read cannot be shown to inherit.
#[must_use]
fn lint_inheritance_violations(root: &Path) -> Vec<String> {
    let mut violations = Vec::new();
    let root_manifest = root.join("Cargo.toml");
    match fs::read_to_string(&root_manifest)
        .ok()
        .and_then(|text| toml::from_str::<toml::Value>(&text).ok())
    {
        Some(manifest) => {
            let set = manifest
                .get("workspace")
                .and_then(|workspace| workspace.get("lints"))
                .and_then(|lints| lints.get("rust"))
                .and_then(|rust| rust.get("unreachable_pub"));
            if set.is_none() {
                violations.push(
                    "workspace root does not set `unreachable_pub` in [workspace.lints.rust]"
                        .to_owned(),
                );
            }
        }
        None => violations.push(format!("{}: unparseable manifest", root_manifest.display())),
    }
    let walk = crate::workspace::crates(root);
    let dirs: Vec<&PathBuf> = walk.crates.iter().chain(&walk.unread).collect();
    if dirs.is_empty() {
        violations.push(format!(
            "{}: the lint inheritance check scanned nothing: no workspace crate was found, \
             so none can be shown to inherit `[lints] workspace = true`",
            root.join("crates").display()
        ));
    }
    for dir in dirs {
        let manifest_path = root.join(dir).join("Cargo.toml");
        let inherits = fs::read_to_string(&manifest_path)
            .ok()
            .and_then(|text| toml::from_str::<toml::Value>(&text).ok())
            .and_then(|manifest| {
                manifest
                    .get("lints")
                    .and_then(|lints| lints.get("workspace"))
                    .and_then(toml::Value::as_bool)
            })
            .unwrap_or(false);
        if !inherits {
            violations.push(format!(
                "{} does not inherit `[lints] workspace = true`",
                manifest_path.display()
            ));
        }
    }
    violations
}

/// The crate-relative path the tier's modules are spelled by.
const WALLED_PATH: &str = "crate::admin::walled::";
/// The files outside the tier that may name a tier module, keyed on path
/// because a line number moves with any edit: `lib.rs` for the `AppState`
/// field types and the router merge, `registry.rs` for the route
/// enumeration, and `test_support.rs` for the fixture that assembles the
/// same state. `test_support.rs` is `#[cfg(test)]`-gated, but the rule
/// reads source text and never sees a cfg, so it is allowlisted by name
/// like the other two.
const WALLED_ALLOWLIST: [&str; 3] = [
    "crates/gateway/app/src/lib.rs",
    "crates/gateway/app/src/registry.rs",
    "crates/gateway/app/src/test_support.rs",
];

/// Checks that only the walled admin tier's own modules and the three
/// assembly sites in [`WALLED_ALLOWLIST`] name a `crate::admin::walled::`
/// path, so a module that no walled route needs does not sit in a
/// directory defined as routes that read secrets, write files, or launch
/// processes.
///
/// The rule protects that directory's meaning, not the wall: wall
/// enforcement is structural, at the router merge in the gateway app's
/// `lib.rs`. Its reach is one textual match, so it catches a spelled path
/// and misses an alias, a `super::` path, a re-export, and any path a
/// macro generates. It is a tripwire for the common case.
///
/// A source tree with no `.rs` file is reported, and so is a file the check
/// cannot read: a scan that read nothing cannot be shown clean.
#[must_use]
fn walled_tier_violations(root: &Path) -> Vec<String> {
    let app_src = root.join("crates").join("gateway").join("app").join("src");
    let tier = app_src.join("admin").join("walled");
    let files = rust_files(&app_src);
    if files.is_empty() {
        return vec![format!(
            "{}: the walled tier check scanned nothing: no `.rs` file was found under the \
             gateway app's source tree, so nothing can be shown to respect the tier",
            app_src.display()
        )];
    }
    let mut violations = Vec::new();
    for file in files {
        if file.starts_with(&tier) {
            continue;
        }
        let relative = slash_path(root, &file);
        if WALLED_ALLOWLIST.contains(&relative.as_str()) {
            continue;
        }
        let text = match fs::read_to_string(&file) {
            Ok(text) => text,
            Err(error) => {
                violations.push(format!("{relative}: unreadable source file: {error}"));
                continue;
            }
        };
        for (index, line) in text.lines().enumerate() {
            // A doc or line comment naming a route module in prose states
            // where the other half of a feature lives; it is not a
            // dependency on it, and failing the build on documentation
            // would only teach authors to stop writing it.
            if line.trim_start().starts_with("//") || !line.contains(WALLED_PATH) {
                continue;
            }
            violations.push(format!(
                "{relative}:{} names {WALLED_PATH}, which only the tier's own modules \
                 and the assembly sites ({}) may",
                index + 1,
                WALLED_ALLOWLIST.join(", ")
            ));
        }
    }
    violations
}

/// A file's path relative to the workspace root with `/` separators, so
/// the allowlist and the violations read the same on every platform.
///
/// A component that is not valid UTF-8 is rendered lossily rather than
/// dropped: dropping it would shorten the path and could make it match an
/// allowlist entry, exempting a file the rule should report.
pub(crate) fn slash_path(root: &Path, file: &Path) -> String {
    file.strip_prefix(root)
        .unwrap_or(file)
        .components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

/// Every `.rs` file under `dir`, recursively, in path order.
pub(crate) fn rust_files(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    collect_rust_files(dir, &mut files);
    files.sort();
    files
}

fn collect_rust_files(dir: &Path, files: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if entry.file_name() != "target" {
                collect_rust_files(&path, files);
            }
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            files.push(path);
        }
    }
}

#[cfg(test)]
#[path = "tidy-tests.rs"]
mod tests;
