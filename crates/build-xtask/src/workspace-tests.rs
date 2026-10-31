//! Fixtures for the shared `crates/` walk: containers nest, crates stop the
//! descent, and a manifest that cannot be read is reported rather than
//! skipped.

use std::fs;
use std::path::Path;

use super::*;

fn root() -> tempfile::TempDir {
    let root = tempfile::TempDir::new().expect("tempdir");
    fs::create_dir_all(root.path().join("crates")).expect("the crates directory creates");
    root
}

fn write_crate(root: &Path, dir: &str, manifest: &str) {
    let dir = root.join("crates").join(dir);
    fs::create_dir_all(&dir).expect("the crate directory creates");
    fs::write(dir.join("Cargo.toml"), manifest).expect("the manifest writes");
}

#[test]
fn crates_under_a_manifestless_container_are_found() {
    let root = root();
    write_crate(
        root.path(),
        "shared-loopback",
        "[package]\nname = \"shared-loopback\"\n",
    );
    write_crate(
        root.path(),
        "gateway/stt/engine",
        "[package]\nname = \"gateway-stt-engine\"\n",
    );
    let walk = crates(root.path());
    assert!(walk.violations.is_empty(), "{:?}", walk.violations);
    let found: Vec<String> = walk
        .crates
        .iter()
        .map(|dir| dir.to_string_lossy().replace('\\', "/"))
        .collect();
    assert_eq!(
        found,
        ["crates/gateway/stt/engine", "crates/shared-loopback"]
    );
}

#[test]
fn a_directory_with_a_manifest_is_not_descended_into() {
    let root = root();
    write_crate(
        root.path(),
        "gateway/local",
        "[package]\nname = \"gateway-local\"\n",
    );
    fs::create_dir_all(
        root.path()
            .join("crates")
            .join("gateway")
            .join("local")
            .join("nested"),
    )
    .expect("the nested directory creates");
    write_crate(
        root.path(),
        "gateway/local/nested",
        "[package]\nname = \"not-a-member\"\n",
    );
    let walk = crates(root.path());
    assert_eq!(walk.crates.len(), 1, "{:?}", walk.crates);
}

#[test]
fn an_unparseable_manifest_is_reported_and_listed_as_unread() {
    let root = root();
    write_crate(root.path(), "broken", "not [valid toml");
    let walk = crates(root.path());
    assert_eq!(walk.violations.len(), 1, "{:?}", walk.violations);
    assert!(
        walk.violations[0].contains("unparseable manifest"),
        "the failure mode is named: {:?}",
        walk.violations
    );
    assert_eq!(walk.unread.len(), 1, "{:?}", walk.unread);
}

#[test]
fn a_manifest_with_no_package_name_is_reported() {
    let root = root();
    write_crate(root.path(), "nameless", "[workspace]\n");
    let walk = crates(root.path());
    assert_eq!(walk.violations.len(), 1, "{:?}", walk.violations);
    assert!(
        walk.violations[0].contains("no package name"),
        "{:?}",
        walk.violations
    );
}

#[test]
fn a_missing_crates_directory_is_reported_not_silent() {
    let root = tempfile::TempDir::new().expect("tempdir");
    let walk = crates(root.path());
    assert_eq!(walk.violations.len(), 1, "{:?}", walk.violations);
    assert!(
        walk.violations[0].contains("unreadable crates directory"),
        "{:?}",
        walk.violations
    );
}
