//! Tests for the private proofs file.

use super::*;

fn valid() -> GatewayPrivateFile {
    GatewayPrivateFile {
        pid: 4242,
        epoch: 1_763_000_000,
        plan_proof: "plan-proof-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_owned(),
        owner_proof: "owner-proof-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_owned(),
    }
}

fn discovery(pid: u32, epoch: u64) -> GatewayDiscoveryFile {
    GatewayDiscoveryFile {
        port: 8081,
        api_key: "key".to_owned(),
        pid,
        epoch,
        version: "0.3.1".to_owned(),
        started_at: "2026-11-13T12:00:00Z".to_owned(),
    }
}

#[test]
fn it_round_trips_and_is_absent_until_written() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    assert_eq!(GatewayPrivateFile::read(dir.path()).expect("reads"), None);
    valid().write_to(dir.path()).expect("writes");
    assert_eq!(
        GatewayPrivateFile::read(dir.path()).expect("reads"),
        Some(valid())
    );
}

#[cfg(unix)]
#[test]
fn it_lands_owner_only() {
    use std::os::unix::fs::PermissionsExt as _;

    let dir = tempfile::TempDir::new().expect("tempdir");
    valid().write_to(dir.path()).expect("writes");
    let mode = std::fs::metadata(gateway_private_file_path(dir.path()))
        .expect("meta")
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600);
}

#[test]
fn proofs_pair_only_with_the_boot_that_minted_them() {
    let file = valid();
    assert!(file.names_boot_of(&discovery(4242, 1_763_000_000)));
    assert!(
        !file.names_boot_of(&discovery(4243, 1_763_000_000)),
        "another process"
    );
    assert!(
        !file.names_boot_of(&discovery(4242, 1_763_000_001)),
        "a later boot"
    );
}

#[test]
fn an_invalid_file_is_neither_written_nor_read() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let cases = [
        GatewayPrivateFile { pid: 0, ..valid() },
        GatewayPrivateFile {
            plan_proof: String::new(),
            ..valid()
        },
        GatewayPrivateFile {
            owner_proof: "has a space".to_owned(),
            ..valid()
        },
        GatewayPrivateFile {
            owner_proof: valid().plan_proof,
            ..valid()
        },
    ];
    for case in cases {
        assert!(case.validation_error().is_some(), "{case:?}");
        assert!(matches!(
            case.write_to(dir.path()),
            Err(SidecarError::Invalid { .. })
        ));
    }
    assert_eq!(GatewayPrivateFile::read(dir.path()).expect("reads"), None);

    std::fs::write(gateway_private_file_path(dir.path()), b"{ not json").expect("write");
    assert!(matches!(
        GatewayPrivateFile::read(dir.path()),
        Err(SidecarError::Parse { .. })
    ));
}

#[test]
fn only_the_owning_process_removes_it() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    remove_private_if_mine(dir.path(), 4242).expect("a missing file is fine");
    valid().write_to(dir.path()).expect("writes");
    remove_private_if_mine(dir.path(), 9999).expect("a foreign pid leaves it");
    assert!(
        GatewayPrivateFile::read(dir.path())
            .expect("reads")
            .is_some()
    );
    remove_private_if_mine(dir.path(), 4242).expect("removes its own");
    assert_eq!(GatewayPrivateFile::read(dir.path()).expect("reads"), None);
}

#[test]
fn debug_output_never_shows_a_proof() {
    let shown = format!("{:?}", valid());
    assert!(
        !shown.contains("plan-proof-a") && !shown.contains("owner-proof-b"),
        "{shown}"
    );
}
