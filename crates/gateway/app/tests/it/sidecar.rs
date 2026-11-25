//! The gateway discovery file lifecycle: [`spawn`] writes `gateway.json` after
//! the bind and [`GatewayHandle::shutdown`] removes it.

use std::time::Duration;

use gateway::{ProfileName, ServeOptions, spawn};

/// A minimal boot config: one unreachable fake backend, one profile. The
/// backend is never contacted at boot.
const CATALOG: &str = r#"
config-version = 0

[server]
bind = "127.0.0.1:0"
api_key = "test-token"

[[endpoint]]
id = "fake"
protocol = "openai"
base_url = "http://127.0.0.1:9"
api_key = ""

[[model]]
name = "alpha-model"
description = "alpha"
context = 1024
upstream = "alpha"
endpoints = ["fake"]

[[profile]]
name = "alpha"
models = []
"#;

#[test]
fn spawn_writes_the_gateway_discovery_file_and_shutdown_removes_it() {
    let temp = tempfile::TempDir::new().unwrap();
    let config_path = temp.path().join("gateway.toml");
    std::fs::write(&config_path, CATALOG).unwrap();
    let run_dir = temp.path().join("run");
    let options = ServeOptions::new(
        Some(config_path),
        ProfileName::parse("alpha").expect("profile name"),
    )
    .with_run_dir(run_dir.clone());

    let gateway = spawn(&options).expect("the gateway boots");

    // The file exists the moment spawn returns, holding the real bind.
    let file = gateway_api_discovery::GatewayDiscoveryFile::read(&run_dir)
        .expect("the gateway discovery file reads")
        .expect("the gateway discovery file exists after the bind");
    assert_eq!(file.pid, std::process::id());
    let port: u16 = gateway
        .url()
        .strip_prefix("http://127.0.0.1:")
        .expect("the gateway bound loopback")
        .parse()
        .expect("the url includes a port");
    assert_eq!(file.port, port, "the file records the bound port");
    assert_eq!(file.api_key, "test-token");
    assert_eq!(file.version, env!("CARGO_PKG_VERSION"));
    assert!(file.epoch > 0);
    // The file's parameters reach the live gateway.
    gateway_api_discovery::wait_for_health(gateway.url(), Duration::from_secs(5))
        .expect("the health endpoint answers through the file's port");

    gateway.shutdown().expect("clean shutdown");
    assert_eq!(
        gateway_api_discovery::GatewayDiscoveryFile::read(&run_dir).expect("read after shutdown"),
        None,
        "a clean shutdown removes the gateway discovery file"
    );
}

#[test]
fn post_shutdown_stops_the_gateway_and_removes_the_gateway_discovery_file() {
    let temp = tempfile::TempDir::new().unwrap();
    let config_path = temp.path().join("gateway.toml");
    std::fs::write(&config_path, CATALOG).unwrap();
    let run_dir = temp.path().join("run");
    let options = ServeOptions::new(
        Some(config_path),
        ProfileName::parse("alpha").expect("profile name"),
    )
    .with_run_dir(run_dir.clone());

    let gateway = spawn(&options).expect("the gateway boots");
    let url = gateway.url().to_owned();

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let status = runtime
        .block_on(async {
            reqwest::Client::new()
                .post(format!("{url}/shutdown"))
                .bearer_auth("test-token")
                .send()
                .await
        })
        .expect("the shutdown POST answers");
    assert_eq!(status.status(), reqwest::StatusCode::ACCEPTED);
    drop(runtime);

    // `join` sends no signal of its own: only the route's signal can end
    // the gateway thread, so the rendezvous pins that the route drove the
    // shutdown.
    let (done, done_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = done.send(gateway.join());
    });
    done_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("the route's signal stopped the gateway thread")
        .expect("clean shutdown");
    assert_eq!(
        gateway_api_discovery::GatewayDiscoveryFile::read(&run_dir).expect("read after shutdown"),
        None,
        "the route-driven shutdown removes the gateway discovery file"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn spawn_writes_the_private_proofs_file_and_its_owner_proof_opens_sign_in() {
    let temp = tempfile::TempDir::new().unwrap();
    let config_path = temp.path().join("gateway.toml");
    std::fs::write(&config_path, CATALOG).unwrap();
    let run_dir = temp.path().join("run");
    let options = ServeOptions::new(
        Some(config_path),
        ProfileName::parse("alpha").expect("profile name"),
    )
    .with_run_dir(run_dir.clone());

    let gateway = spawn(&options).expect("the gateway boots");

    let discovery = gateway_api_discovery::GatewayDiscoveryFile::read(&run_dir)
        .expect("the gateway discovery file reads")
        .expect("the gateway discovery file exists");
    let private = gateway_api_discovery::GatewayPrivateFile::read(&run_dir)
        .expect("the private file reads")
        .expect("the private file exists the moment spawn returns");
    assert!(private.names_boot_of(&discovery));
    for proof in [&private.plan_proof, &private.owner_proof] {
        assert_eq!(proof.len(), 64);
        assert!(proof.bytes().all(|byte| byte.is_ascii_hexdigit()));
        assert_ne!(proof, &discovery.api_key);
    }
    assert_ne!(private.plan_proof, private.owner_proof);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = |path: std::path::PathBuf| {
            std::fs::metadata(path)
                .expect("metadata")
                .permissions()
                .mode()
                & 0o777
        };
        assert_eq!(mode(run_dir.join("gateway-private.json")), 0o600);
        // The broker opened its store in the profile directory.
        assert_eq!(mode(temp.path().join("chatgpt-plan")), 0o700);
    }

    // The file's owner proof, on top of the key, opens the sign-in routes.
    let status = |owner_proof: Option<String>| {
        let mut request = reqwest::Client::new()
            .get(format!("{}/admin/chatgpt/status", gateway.url()))
            .bearer_auth(&discovery.api_key);
        if let Some(owner_proof) = owner_proof {
            request = request.header("x-runvil-owner-proof", owner_proof);
        }
        request.send()
    };
    let refused = status(None).await.expect("the gateway answers");
    assert_eq!(refused.status(), reqwest::StatusCode::FORBIDDEN);
    let wrong = status(Some(private.plan_proof.clone()))
        .await
        .expect("the gateway answers");
    assert_eq!(wrong.status(), reqwest::StatusCode::FORBIDDEN);
    let admitted = status(Some(private.owner_proof.clone()))
        .await
        .expect("the gateway answers");
    assert_eq!(admitted.status(), reqwest::StatusCode::OK);
    let body: serde_json::Value = admitted.json().await.expect("a status object");
    assert_eq!(body["available"], true, "{body}");
    assert_eq!(body["status"], serde_json::Value::Null, "{body}");

    gateway.shutdown().expect("clean shutdown");
    assert_eq!(
        gateway_api_discovery::GatewayPrivateFile::read(&run_dir).expect("read after shutdown"),
        None,
        "a clean shutdown withdraws the proofs"
    );
}
