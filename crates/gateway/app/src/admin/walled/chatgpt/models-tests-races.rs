//! Routing-before-disk atomicity (R7) and session/setup fence concurrency
//! (R3) tests for `/admin/chatgpt/models/*`.

use gateway_config::Config;
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};

use super::super::super::error_code;
use super::super::ModelRig;

#[cfg(feature = "local")]
fn running_local(name: &str) -> crate::local::LocalRuntime {
    let config = Config::from_toml_str(&format!(
        "config-version = 0\n\
         [server]\nbind = \"127.0.0.1:0\"\napi_key = \"test-token\"\n\
         [[endpoint]]\nid = \"local\"\nprotocol = \"openai\"\n\
         base_url = \"http://127.0.0.1:9\"\napi_key = \"\"\n\
         [[model]]\nname = \"{name}\"\ndescription = \"running child\"\n\
         context = 4096\nupstream = \"{name}\"\nendpoints = [\"local\"]\n"
    ))
    .expect("local fixture config parses");
    let routing =
        crate::routing::Routing::from_config(&config).expect("local fixture routing builds");
    crate::local::LocalRuntime::from_test_models(routing.models().to_vec())
}

#[cfg(feature = "local")]
#[tokio::test]
async fn routing_merge_failure_leaves_disk_untouched_and_retry_reconciles_live() {
    let env = ModelRig::with_toml("").await;
    env.sign_in().await;
    let disk_before = std::fs::read_to_string(&env.config_path).expect("read gateway.toml");
    let prev_path = crate::config_shadow::rollback_path(&env.config_path);

    // A local child named `gpt-5` is still loaded in memory while its
    // declaration was removed from gateway.toml on disk.
    env.state.live.write().await.local = running_local("gpt-5");

    let failed = env.rig.owner(Method::POST, "models/enable").await;
    assert_eq!(failed.status(), StatusCode::BAD_REQUEST);
    assert_eq!(error_code(failed).await, "switch_failed");

    let disk_after_fail = std::fs::read_to_string(&env.config_path).expect("read gateway.toml");
    assert_eq!(
        disk_after_fail, disk_before,
        "routing merge failure must not commit gateway.toml before live routing builds"
    );
    assert!(
        !prev_path.exists(),
        "failed enable must not leave a rollback file behind"
    );

    // Once the conflicting local model unloads, retrying enable succeeds and
    // publishes both disk and live routing together.
    env.state.live.write().await.local = crate::local::LocalRuntime::empty();
    let retried = env.rig.owner(Method::POST, "models/enable").await;
    assert_eq!(retried.status(), StatusCode::OK);
    let body: Value = retried.json().await.expect("json");
    assert_eq!(body["added"], json!(["gpt-5", "gpt-5-mini"]));

    let status: Value = env
        .rig
        .owner(Method::GET, "status")
        .await
        .json()
        .await
        .expect("status");
    assert_eq!(status["plan_models"], json!(["gpt-5", "gpt-5-mini"]));

    // And if gateway.toml on disk already contains plan rows that live routing
    // had not picked up yet, an idempotent enable (`added: []`) still
    // reconciles live routing from the validated config.
    let baseline_config = Config::from_toml_str(&disk_before).expect("baseline config");
    let baseline_routing =
        crate::routing::Routing::from_config(&baseline_config).expect("baseline routing");
    env.state.live.write().await.routing = std::sync::Arc::new(baseline_routing);

    let reconcile = env.rig.owner(Method::POST, "models/enable").await;
    assert_eq!(reconcile.status(), StatusCode::OK);
    let reconcile_body: Value = reconcile.json().await.expect("json");
    assert_eq!(reconcile_body["added"], json!([]));

    let status_after: Value = env
        .rig
        .owner(Method::GET, "status")
        .await
        .json()
        .await
        .expect("status");
    assert_eq!(
        status_after["plan_models"],
        json!(["gpt-5", "gpt-5-mini"]),
        "idempotent enable reconciles live routing when disk already had the plan rows"
    );
}

#[tokio::test]
async fn models_forget_supersedes_in_flight_enable_while_keeping_sign_in_active() {
    let env = ModelRig::with_toml("").await;
    env.sign_in().await;

    // Seed an initial enabled catalog, then hold a second `models/enable`
    // while `models/forget` removes all plan models.
    let seeded = env.rig.owner(Method::POST, "models/enable").await;
    assert_eq!(seeded.status(), StatusCode::OK);

    env.api.set(|b| b.hold = true);
    let addr = env.rig.addr;
    let owner = env.rig.owner_proof.clone();
    let in_flight = tokio::spawn(async move {
        super::super::super::call(
            addr,
            Method::POST,
            "models/enable",
            Some("test-token"),
            &[&owner],
        )
        .await
    });
    env.api.entered.notified().await;

    let forgot = env.rig.owner(Method::POST, "models/forget").await;
    assert_eq!(forgot.status(), StatusCode::OK);

    let mid_status: Value = env
        .rig
        .owner(Method::GET, "status")
        .await
        .json()
        .await
        .expect("status");
    assert_eq!(
        mid_status["status"], "active",
        "models/forget leaves OAuth credentials active"
    );
    assert_eq!(mid_status["plan_models"], json!([]));
    let disk_after_forget =
        std::fs::read_to_string(&env.config_path).expect("read gateway.toml after forget");
    assert!(!disk_after_forget.contains("chatgpt-plan"));

    env.api.release.notify_one();
    let enable_resp = in_flight.await.expect("in-flight enable joins");
    assert_eq!(
        enable_resp.status(),
        StatusCode::CONFLICT,
        "an enable started before models/forget must be superseded instead of reinstalling models"
    );
    assert_eq!(error_code(enable_resp).await, "chatgpt_sign_in_changed");

    let disk_final = std::fs::read_to_string(&env.config_path).expect("read gateway.toml");
    assert_eq!(
        disk_final, disk_after_forget,
        "superseded enable must not write gateway.toml"
    );
    let final_status: Value = env
        .rig
        .owner(Method::GET, "status")
        .await
        .json()
        .await
        .expect("status");
    assert_eq!(final_status["status"], "active");
    assert_eq!(final_status["plan_models"], json!([]));
}

#[tokio::test]
async fn sign_out_at_commit_boundary_aborts_enable_without_writing_disk_or_live() {
    use std::sync::Arc;

    use gateway_chatgpt_plan::fake_issuer::Fake;

    use super::super::super::Rig;
    use super::super::ModelsApi;
    use crate::chatgpt::ChatGpt;
    use crate::park::{Phase, PhasePark};
    use crate::test_support::{AdminPaths, app_state};

    let fake = Fake::start().await;
    let (api, api_addr) = ModelsApi::start().await;
    let profile = tempfile::TempDir::new().expect("tempdir");
    let cache = profile.path().join("cache");
    std::fs::create_dir_all(cache.join("models")).expect("mkdir cache");
    let config_path = profile.path().join("gateway.toml");
    let base_toml = format!(
        "config-version = 0\n\n\
         [server]\n\
         bind = \"127.0.0.1:0\"\n\
         api_key = \"test-token\"\n\n\
         [local]\n\
         cache_dir = '{}'\n",
        cache.display()
    );
    std::fs::write(&config_path, &base_toml).expect("write gateway.toml");
    let config = Config::from_toml_str(&base_toml).expect("valid base config");
    let mut state = app_state(
        config,
        Some(AdminPaths {
            fixture_dir: profile.path().to_path_buf(),
            active: "main".to_owned(),
            config_path: config_path.clone(),
        }),
    );
    let park = Arc::new(PhasePark::at(Phase::ChatGptEnableCommit));
    state.park = Some(Arc::clone(&park));
    state.chatgpt = ChatGpt::with_api_base(&format!("http://{api_addr}/v1"));
    state.chatgpt.open_on(profile.path(), fake.config());
    let broker = state.chatgpt.broker().expect("broker").clone();
    let rig = Rig::serve(fake, profile, state).await;

    let callback = rig.begin().await;
    assert_eq!(
        reqwest::get(&callback).await.expect("callback").status(),
        StatusCode::OK
    );

    let addr = rig.addr;
    let owner = rig.owner_proof.clone();
    let in_flight = tokio::spawn(async move {
        super::super::super::call(
            addr,
            Method::POST,
            "models/enable",
            Some("test-token"),
            &[&owner],
        )
        .await
    });
    park.entered().await;

    // Credential `forget` runs in the commit-boundary window (after pre-checks,
    // TOML preparation, and routing construction, as an already-admitted
    // `/admin/chatgpt/forget` handler or OAuth callback completion can).
    assert!(broker.forget().await.expect("forgets").revoked);

    park.release();
    let enable_resp = in_flight.await.expect("parked enable joins");
    assert_eq!(
        enable_resp.status(),
        StatusCode::CONFLICT,
        "session retirement at the commit boundary must abort enable before writing"
    );
    assert_eq!(error_code(enable_resp).await, "chatgpt_sign_in_changed");

    let disk_after = std::fs::read_to_string(&config_path).expect("read gateway.toml");
    assert_eq!(disk_after, base_toml, "gateway.toml must remain untouched");
    assert!(
        !crate::config_shadow::rollback_path(&config_path).exists(),
        "rollback file must not be created"
    );
    let status: Value = rig
        .owner(Method::GET, "status")
        .await
        .json()
        .await
        .expect("status");
    assert_eq!(status["plan_models"], json!([]));
    assert_eq!(api.hits(), 1);
}

#[tokio::test]
async fn same_account_sign_out_and_re_sign_in_mid_fetch_aborts_enable() {
    let env = ModelRig::with_toml("").await;
    env.sign_in().await;
    let disk_before = std::fs::read_to_string(&env.config_path).expect("read gateway.toml");

    env.api.set(|b| b.hold = true);
    let addr = env.rig.addr;
    let owner = env.rig.owner_proof.clone();
    let in_flight = tokio::spawn(async move {
        super::super::super::call(
            addr,
            Method::POST,
            "models/enable",
            Some("test-token"),
            &[&owner],
        )
        .await
    });
    env.api.entered.notified().await;

    // The same account (`user-1`, active with plan grant) signs out and signs
    // back in while `/models` is in flight: only `state.generation` changed.
    let signed_out = env.rig.owner(Method::POST, "sign-out").await;
    assert_eq!(signed_out.status(), StatusCode::OK);
    env.sign_in().await;
    assert_eq!(env.rig.status().await["status"], "active");

    env.api.release.notify_one();
    let enable_resp = in_flight.await.expect("in-flight enable joins");
    assert_eq!(
        enable_resp.status(),
        StatusCode::CONFLICT,
        "same-account re-sign-in mid-fetch must invalidate the earlier generation's fence"
    );
    assert_eq!(error_code(enable_resp).await, "chatgpt_sign_in_changed");

    let disk_after = std::fs::read_to_string(&env.config_path).expect("read gateway.toml");
    assert_eq!(disk_after, disk_before);
    assert_eq!(env.rig.status().await["plan_models"], json!([]));
}

#[tokio::test]
async fn missing_no_redirect_models_client_fails_closed() {
    use gateway_chatgpt_plan::fake_issuer::Fake;

    use super::super::super::Rig;
    use super::super::ModelsApi;
    use crate::chatgpt::ChatGpt;
    use crate::test_support::{AdminPaths, app_state};

    let fake = Fake::start().await;
    let (api, api_addr) = ModelsApi::start().await;
    let profile = tempfile::TempDir::new().expect("tempdir");
    let cache = profile.path().join("cache");
    std::fs::create_dir_all(cache.join("models")).expect("mkdir cache");
    let config_path = profile.path().join("gateway.toml");
    let base_toml = format!(
        "config-version = 0\n\n\
         [server]\n\
         bind = \"127.0.0.1:0\"\n\
         api_key = \"test-token\"\n\n\
         [local]\n\
         cache_dir = '{}'\n",
        cache.display()
    );
    std::fs::write(&config_path, &base_toml).expect("write gateway.toml");
    let config = Config::from_toml_str(&base_toml).expect("valid base config");
    let mut state = app_state(
        config,
        Some(AdminPaths {
            fixture_dir: profile.path().to_path_buf(),
            active: "main".to_owned(),
            config_path,
        }),
    );
    state.chatgpt = ChatGpt::without_models_client(&format!("http://{api_addr}/v1"));
    state.chatgpt.open_on(profile.path(), fake.config());
    let rig = Rig::serve(fake, profile, state).await;

    let callback = rig.begin().await;
    assert_eq!(
        reqwest::get(&callback).await.expect("callback").status(),
        StatusCode::OK
    );

    let resp = rig.owner(Method::POST, "models/enable").await;
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(error_code(resp).await, "chatgpt_unavailable");
    assert_eq!(
        api.hits(),
        0,
        "without the no-redirect client, no request is ever sent"
    );
}
