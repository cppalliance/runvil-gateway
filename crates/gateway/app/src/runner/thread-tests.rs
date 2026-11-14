//! Tests for what the discovery file guard removes and what it leaves.

use std::path::Path;

use gateway_api_discovery::{
    GatewayDiscoveryFile, GatewayPrivateFile, gateway_discovery_file_path,
    gateway_private_file_path,
};

use super::GatewayDiscoveryFileGuard;

const MINE: u32 = 4242;
const THEIRS: u32 = 4343;
const EPOCH: u64 = 1_763_000_000;

/// Publishes both files in `run_dir` as process `pid` would.
fn publish(run_dir: &Path, pid: u32) {
    GatewayDiscoveryFile {
        port: 8081,
        api_key: "key".to_owned(),
        pid,
        epoch: EPOCH,
        version: "0.3.1".to_owned(),
        started_at: "2026-11-13T12:00:00Z".to_owned(),
    }
    .write_to(run_dir)
    .expect("the discovery file writes");
    GatewayPrivateFile {
        pid,
        epoch: EPOCH,
        plan_proof: "plan-proof-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_owned(),
        owner_proof: "owner-proof-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_owned(),
    }
    .write_to(run_dir)
    .expect("the private file writes");
}

/// Whether the discovery file and the private file are present, in that order.
fn present(run_dir: &Path) -> (bool, bool) {
    (
        gateway_discovery_file_path(run_dir).exists(),
        gateway_private_file_path(run_dir).exists(),
    )
}

#[test]
fn a_dropped_guard_removes_both_of_its_files() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    publish(dir.path(), MINE);

    drop(GatewayDiscoveryFileGuard::new(dir.path().to_owned(), MINE));

    assert_eq!(present(dir.path()), (false, false));
}

#[test]
fn a_kept_guard_leaves_the_discovery_file_and_removes_its_proofs() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    publish(dir.path(), MINE);

    GatewayDiscoveryFileGuard::new(dir.path().to_owned(), MINE).keep();

    assert_eq!(present(dir.path()), (true, false));
}

#[test]
fn a_kept_guard_spares_another_processes_proofs() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    publish(dir.path(), THEIRS);

    GatewayDiscoveryFileGuard::new(dir.path().to_owned(), MINE).keep();

    assert_eq!(present(dir.path()), (true, true));
}

#[test]
fn a_dropped_guard_spares_another_processes_files() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    publish(dir.path(), THEIRS);

    drop(GatewayDiscoveryFileGuard::new(dir.path().to_owned(), MINE));

    assert_eq!(present(dir.path()), (true, true));
}
