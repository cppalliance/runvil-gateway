use gateway_config::{Config, ProfileSelection, profile_state_path, shadow_path};

use crate::test_support::{AdminPaths, app_state, serve_state, serve_with_paths};

const CONFIG: &str = r#"
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

[[model]]
name = "beta-model"
description = "beta"
context = 1024
upstream = "beta"
endpoints = ["fake"]

[[profile]]
name = "alpha"
models = []

[[profile]]
name = "beta"
models = []
"#;

fn fixture() -> (tempfile::TempDir, Config, AdminPaths) {
    let temp = tempfile::TempDir::new().expect("temp dir");
    let config_path = temp.path().join("gateway.toml");
    std::fs::write(&config_path, CONFIG).expect("write config");
    std::fs::write(
        profile_state_path(&config_path),
        "active_profile = \"alpha\"\n",
    )
    .expect("write state");
    let config = Config::load(&config_path, &ProfileSelection::default()).expect("load config");
    let paths = AdminPaths {
        fixture_dir: temp.path().to_path_buf(),
        active: "alpha".to_owned(),
        config_path,
    };
    (temp, config, paths)
}

/// The live config as a save body, carrying its `x-config-revision`
/// precondition and omitting `active_profile`.
async fn save_body(addr: std::net::SocketAddr) -> serde_json::Value {
    let response = reqwest::Client::new()
        .get(format!("http://{addr}/admin/config"))
        .bearer_auth("test-token")
        .send()
        .await
        .expect("get sends");
    let revision = response
        .headers()
        .get("x-config-revision")
        .and_then(|v| v.to_str().ok())
        .expect("GET /admin/config exposes x-config-revision")
        .to_owned();
    let mut body: serde_json::Value = response.json().await.expect("config json");
    let map = body.as_object_mut().expect("the config is an object");
    map.remove("active_profile");
    map.insert("revision".to_owned(), serde_json::Value::String(revision));
    body
}

async fn put_config(addr: std::net::SocketAddr, body: &serde_json::Value) -> reqwest::Response {
    reqwest::Client::new()
        .put(format!("http://{addr}/admin/config"))
        .bearer_auth("test-token")
        .json(body)
        .send()
        .await
        .expect("put sends")
}

/// Add a call to the Add Model endpoint to a request body
fn push_cloud_endpoint(body: &mut serde_json::Value, var: &str) {
    body["endpoint"]
        .as_array_mut()
        .expect("endpoints are an array")
        .push(serde_json::json!({
            "id": "staged",
            "protocol": "openai",
            "base_url": "http://127.0.0.1:9",
            "api_key": format!("${{{var}}}"),
        }));
}

#[tokio::test]
async fn a_save_setting_active_profile_is_rejected_and_stages_nothing() {
    let (_temp, config, paths) = fixture();
    let config_path = paths.config_path.clone();
    let addr = serve_with_paths(config, paths).await;
    let mut body = save_body(addr).await;
    body["active_profile"] = serde_json::json!("beta");

    let response = put_config(addr, &body).await;

    assert_eq!(response.status(), reqwest::StatusCode::UNPROCESSABLE_ENTITY);
    let error: serde_json::Value = response.json().await.expect("error envelope");
    assert_eq!(error["error"]["code"], "config_write_rejected");
    assert!(
        error["error"]["message"].as_str().is_some_and(|message| {
            message.contains("active_profile is not a configuration key")
                && message.contains("POST /admin/switch-profile")
        }),
        "the message names the switch route: {error}"
    );
    assert!(!shadow_path(&config_path).exists());
    assert!(!shadow_path(&profile_state_path(&config_path)).exists());
}

#[tokio::test]
async fn a_save_replies_with_the_config_shadow_alone() {
    let (_temp, config, paths) = fixture();
    let config_path = paths.config_path.clone();
    let addr = serve_with_paths(config, paths).await;
    let mut body = save_body(addr).await;
    body["model"][0]["description"] = serde_json::json!("edited");

    let response = put_config(addr, &body).await;

    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let reply: serde_json::Value = response.json().await.expect("save reply");
    assert_eq!(
        reply,
        serde_json::json!({ "shadow": shadow_path(&config_path).display().to_string() }),
        "the reply names only the config shadow"
    );
    assert!(shadow_path(&config_path).is_file());
    assert!(!shadow_path(&profile_state_path(&config_path)).exists());
}

#[tokio::test]
async fn adding_a_cloud_provider_validates_against_a_key_staged_only_in_the_env_shadow() {
    // After a 'Save' operation in the UI the saved key is stored in the shadow
    // env file and is not yet available in the process's environment.
    //
    // When a Cloud Model is added the handler should validate against the running
    // environment and  pending keys in the shadow env.

    let (_temp, config, paths) = fixture();
    let config_path = paths.config_path.clone();

    std::fs::write(
        shadow_path(&config_path.with_extension("env")),
        "RV_TEST_STAGED_KEY=sk-staged\n",
    )
    .expect("write env shadow");

    let addr = serve_with_paths(config, paths).await;

    let mut body = save_body(addr).await;
    push_cloud_endpoint(&mut body, "RV_TEST_STAGED_KEY");

    let response = put_config(addr, &body).await;

    let expected = reqwest::StatusCode::OK;
    let actual = response.status();
    assert_eq!(
        actual,
        expected,
        "Got `{actual}`, expected `{expected}`. Response: {}",
        response.text().await.unwrap_or_default()
    );
    assert!(shadow_path(&config_path).is_file());
}

#[tokio::test]
async fn adding_a_cloud_provider_without_its_key_is_rejected_and_stages_nothing() {
    let (_temp, config, paths) = fixture();
    let config_path = paths.config_path.clone();
    let addr = serve_with_paths(config, paths).await;

    let mut body = save_body(addr).await;
    push_cloud_endpoint(&mut body, "RV_TEST_MISSING_KEY");

    let response = put_config(addr, &body).await;

    assert_eq!(response.status(), reqwest::StatusCode::UNPROCESSABLE_ENTITY);

    let error: serde_json::Value = response.json().await.expect("error envelope");

    assert_eq!(error["error"]["code"], "config_write_rejected");

    assert!(
        error["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("RV_TEST_MISSING_KEY")),
        "the error message does not name the missing variable: {error}"
    );

    assert!(!shadow_path(&config_path).exists());
}

async fn pending_revision(addr: std::net::SocketAddr) -> String {
    let body: serde_json::Value = reqwest::Client::new()
        .get(format!("http://{addr}/admin/config-pending"))
        .bearer_auth("test-token")
        .send()
        .await
        .expect("pending get sends")
        .json()
        .await
        .expect("pending json");
    body["revision"]
        .as_str()
        .expect("pending reply carries revision")
        .to_owned()
}

#[tokio::test]
async fn stale_revision_refuses_save_with_409_without_clobbering_newer_disk_config() {
    let (_temp, config, paths) = fixture();
    let config_path = paths.config_path.clone();
    let addr = serve_with_paths(config, paths).await;

    let get_resp = reqwest::Client::new()
        .get(format!("http://{addr}/admin/config"))
        .bearer_auth("test-token")
        .send()
        .await
        .expect("get config");
    let header_rev = get_resp
        .headers()
        .get("x-config-revision")
        .and_then(|v| v.to_str().ok())
        .expect("GET /admin/config exposes x-config-revision")
        .to_owned();
    let etag = get_resp
        .headers()
        .get(reqwest::header::ETAG)
        .and_then(|v| v.to_str().ok())
        .expect("GET /admin/config exposes ETag")
        .to_owned();
    assert_eq!(header_rev.len(), 64, "SHA-256 hex revision");
    assert_eq!(etag, format!("\"{header_rev}\""));

    let loaded_rev = pending_revision(addr).await;
    assert_eq!(loaded_rev, header_rev);

    let mut stale_body = save_body(addr).await;
    stale_body["model"][0]["description"] = serde_json::json!("edited from stale tab");
    stale_body["revision"] = serde_json::json!(loaded_rev);

    // Out-of-band update on disk (standing in for ChatGPT models/enable).
    let updated_disk = format!(
        "{CONFIG}\n[[endpoint]]\nid = \"chatgpt\"\nprotocol = \"openai-responses\"\nauth = \"chatgpt-plan\"\n\n\
         [[model]]\nname = \"gpt-5\"\ndescription = \"GPT-5\"\ncontext = 400000\nupstream = \"gpt-5\"\nendpoints = [\"chatgpt\"]\n"
    );
    std::fs::write(&config_path, &updated_disk).expect("write updated config");

    let response = put_config(addr, &stale_body).await;
    assert_eq!(response.status(), reqwest::StatusCode::CONFLICT);
    let error: serde_json::Value = response.json().await.expect("conflict json");
    assert_eq!(error["error"]["code"], "config_conflict");
    assert!(
        error["error"]["message"]
            .as_str()
            .is_some_and(|msg| msg.contains("reload")),
        "conflict message tells caller to reload: {error}"
    );
    assert!(
        !shadow_path(&config_path).exists(),
        "stale save stages no shadow"
    );
    assert!(
        std::fs::read_to_string(&config_path)
            .expect("read config")
            .contains("name = \"gpt-5\""),
        "plan model on disk survives untouched"
    );
}

#[tokio::test]
async fn revision_tracks_pending_drafts_apply_revert_headers_and_invalid_inputs() {
    let (_temp, config, paths) = fixture();
    let config_path = paths.config_path.clone();
    let state = app_state(config, Some(paths));
    let _worker = state.commands.spawn_worker(&state).expect("worker spawns");
    let addr = serve_state(state).await;
    let client = reqwest::Client::new();

    let rev0 = pending_revision(addr).await;
    let mut first = save_body(addr).await;
    first["model"][0]["description"] = serde_json::json!("draft one");
    first["revision"] = serde_json::json!(rev0);

    let ok1 = put_config(addr, &first).await;
    assert_eq!(ok1.status(), reqwest::StatusCode::OK);
    let shadow_toml = std::fs::read_to_string(shadow_path(&config_path)).expect("read shadow");
    assert!(
        !shadow_toml.contains("revision"),
        "wire revision precondition never enters gateway.toml.next"
    );

    let rev1 = pending_revision(addr).await;
    assert_ne!(rev1, rev0, "staging a shadow advances pending revision");

    // Re-sending rev0 now fails 409 because the pending draft moved on.
    let stale_second = put_config(addr, &first).await;
    assert_eq!(stale_second.status(), reqwest::StatusCode::CONFLICT);

    // Sending rev1 via If-Match header succeeds and advances to rev2.
    let mut second = save_body(addr).await;
    second.as_object_mut().expect("object").remove("revision");
    second["model"][0]["description"] = serde_json::json!("draft two");
    let ok2 = client
        .put(format!("http://{addr}/admin/config"))
        .bearer_auth("test-token")
        .header(reqwest::header::IF_MATCH, format!("\"{rev1}\""))
        .json(&second)
        .send()
        .await
        .expect("put with If-Match");
    assert_eq!(ok2.status(), reqwest::StatusCode::OK);
    let rev2 = pending_revision(addr).await;
    assert_ne!(rev2, rev1);

    // Revert removes the shadow and restores pending revision back to rev0.
    let revert = client
        .post(format!("http://{addr}/admin/config-revert"))
        .bearer_auth("test-token")
        .send()
        .await
        .expect("revert");
    assert_eq!(revert.status(), reqwest::StatusCode::OK);
    assert_eq!(pending_revision(addr).await, rev0);

    // Stage again and apply: committed GET /admin/config revision advances to match.
    second["revision"] = serde_json::json!(rev0);
    assert_eq!(
        put_config(addr, &second).await.status(),
        reqwest::StatusCode::OK
    );
    let staged_rev = pending_revision(addr).await;
    let apply = client
        .post(format!("http://{addr}/admin/config-apply"))
        .bearer_auth("test-token")
        .send()
        .await
        .expect("apply");
    assert_eq!(apply.status(), reqwest::StatusCode::OK);
    assert_eq!(pending_revision(addr).await, staged_rev);
    let get_after = client
        .get(format!("http://{addr}/admin/config"))
        .bearer_auth("test-token")
        .send()
        .await
        .expect("get after apply");
    assert_eq!(
        get_after.headers()["x-config-revision"].to_str().unwrap(),
        staged_rev
    );

    // Malformed non-string or empty revision is rejected 422 without staging.
    for bad in [serde_json::json!(123), serde_json::json!("")] {
        let mut invalid = save_body(addr).await;
        invalid["revision"] = bad;
        let resp = put_config(addr, &invalid).await;
        assert_eq!(resp.status(), reqwest::StatusCode::UNPROCESSABLE_ENTITY);
        assert!(!shadow_path(&config_path).exists());
    }
}

#[tokio::test]
async fn a_save_without_a_revision_precondition_is_refused_and_stages_nothing() {
    let (_temp, config, paths) = fixture();
    let config_path = paths.config_path.clone();
    let addr = serve_with_paths(config, paths).await;

    let mut absent = save_body(addr).await;
    absent.as_object_mut().expect("object").remove("revision");
    let mut explicit_null = absent.clone();
    explicit_null["revision"] = serde_json::Value::Null;

    for candidate in [&absent, &explicit_null] {
        let resp = put_config(addr, candidate).await;
        assert_eq!(resp.status(), reqwest::StatusCode::PRECONDITION_REQUIRED);
        let err: serde_json::Value = resp.json().await.expect("error json");
        assert_eq!(err["error"]["code"], "config_revision_required");
        assert!(!shadow_path(&config_path).exists());
    }
}

#[tokio::test]
async fn running_config_cannot_be_saved_over_an_out_of_band_disk_edit() {
    let (_temp, config, paths) = fixture();
    let config_path = paths.config_path.clone();
    let addr = serve_with_paths(config, paths).await;

    let disk_b = CONFIG.replace("description = \"alpha\"", "description = \"gpt-5-on-disk\"");
    std::fs::write(&config_path, &disk_b).expect("write out-of-band edit");

    let client = reqwest::Client::new();
    let get_resp = client
        .get(format!("http://{addr}/admin/config"))
        .bearer_auth("test-token")
        .send()
        .await
        .expect("get running config");
    let rev_a = crate::config_shadow::bytes_revision(CONFIG.as_bytes());
    let etag = get_resp.headers()[reqwest::header::ETAG]
        .to_str()
        .unwrap()
        .to_owned();
    let x_rev = get_resp.headers()["x-config-revision"]
        .to_str()
        .unwrap()
        .to_owned();
    assert_eq!(etag, format!("\"{rev_a}\""));
    assert_eq!(x_rev, rev_a);

    let mut running_body: serde_json::Value = get_resp.json().await.expect("running json");
    assert_eq!(running_body["model"][0]["description"], "alpha");
    running_body
        .as_object_mut()
        .expect("object")
        .remove("active_profile");

    let put_if_match = client
        .put(format!("http://{addr}/admin/config"))
        .bearer_auth("test-token")
        .header(reqwest::header::IF_MATCH, &etag)
        .json(&running_body)
        .send()
        .await
        .expect("put with If-Match from GET /admin/config");
    assert_eq!(put_if_match.status(), reqwest::StatusCode::CONFLICT);
    let err: serde_json::Value = put_if_match.json().await.expect("conflict json");
    assert_eq!(err["error"]["code"], "config_conflict");

    running_body["revision"] = serde_json::Value::String(x_rev);
    assert_eq!(
        put_config(addr, &running_body).await.status(),
        reqwest::StatusCode::CONFLICT
    );
    assert!(!shadow_path(&config_path).exists());
    let disk_after = std::fs::read_to_string(&config_path).expect("read disk");
    assert!(disk_after.contains("gpt-5-on-disk"));
}
