//! The contract between the gateway's writer and the readers of
//! `gateway.json` that live in other repositories. `fixtures/gateway.json`
//! is the file those readers pin, so the writer's output must keep matching
//! it field for field.

use gateway_api_discovery::GatewayDiscoveryFile;

const FIXTURE: &str = include_str!("../fixtures/gateway.json");

/// What the writer puts on disk equals the fixture that readers pin.
#[test]
fn the_writer_output_matches_the_contract_fixture() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let file = GatewayDiscoveryFile {
        port: 52814,
        api_key: "fixture-key".to_owned(),
        pid: 4242,
        epoch: 1_700_000_000,
        version: "0.1.0".to_owned(),
        started_at: "2023-11-14T22:13:20Z".to_owned(),
    };
    file.write_to(dir.path()).expect("the writer writes");
    let written =
        std::fs::read_to_string(dir.path().join("gateway.json")).expect("the file exists");
    let written: serde_json::Value = serde_json::from_str(&written).expect("the file is JSON");
    let fixture: serde_json::Value = serde_json::from_str(FIXTURE).expect("the fixture is JSON");
    assert_eq!(written, fixture);
}

/// The fixture reads back as a valid discovery file with the values it holds.
#[test]
fn the_contract_fixture_reads_back_as_a_valid_discovery_file() {
    let dir = tempfile::tempdir().expect("a temp dir");
    std::fs::write(dir.path().join("gateway.json"), FIXTURE).expect("the fixture is written");
    let file = GatewayDiscoveryFile::read(dir.path())
        .expect("the fixture reads")
        .expect("the fixture is a file");
    assert_eq!(file.port, 52814);
    assert_eq!(file.api_key, "fixture-key");
    assert_eq!(file.pid, 4242);
    assert_eq!(file.epoch, 1_700_000_000);
    assert_eq!(file.version, "0.1.0");
    assert_eq!(file.started_at, "2023-11-14T22:13:20Z");
    assert_eq!(file.validation_error(), None);
}
