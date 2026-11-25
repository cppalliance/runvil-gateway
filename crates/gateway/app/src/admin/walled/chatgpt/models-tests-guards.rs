//! Refusal, draft-preservation, hand-edit preservation, generation/account
//! fence, and projection/log-canary tests for `/admin/chatgpt/models/*`.

use std::io::Write;
use std::sync::{Arc, Mutex};

use gateway_chatgpt_plan::fake_issuer::Fake;
use reqwest::{Method, StatusCode};
use serde_json::json;

use super::{ModelRig, ModelsApi, call, error_code};
use crate::chatgpt::ChatGpt;
use crate::test_support::{loopback_state, serve_state};

#[tokio::test]
async fn pending_drafts_refuse_model_writes_and_leave_files_untouched() {
    let env = ModelRig::with_toml("").await;
    env.sign_in().await;

    let original_toml = std::fs::read_to_string(&env.config_path).expect("read toml");
    let toml_next = env.config_path.with_file_name("gateway.toml.next");
    let staged = format!("{original_toml}\n# staged config edit\n");
    std::fs::write(&toml_next, &staged).expect("write toml.next");

    for route in ["models/enable", "models/forget"] {
        let resp = env.rig.owner(Method::POST, route).await;
        assert_eq!(resp.status(), StatusCode::CONFLICT, "{route}");
        assert_eq!(error_code(resp).await, "config_has_pending_changes");
        assert_eq!(
            std::fs::read_to_string(&env.config_path).expect("toml"),
            original_toml
        );
        assert_eq!(std::fs::read_to_string(&toml_next).expect("next"), staged);
    }

    // Credential retirement (`/admin/chatgpt/forget`) stays separate so a
    // dirty config draft never blocks revoking OAuth tokens.
    let cred_forget = env.rig.owner(Method::POST, "forget").await;
    assert_eq!(cred_forget.status(), StatusCode::OK);
    env.sign_in().await;

    std::fs::remove_file(&toml_next).expect("rm toml.next");

    // An unapplied `gateway.env.next` draft also blocks config writes.
    let env_next = env.config_path.with_file_name("gateway.env.next");
    std::fs::write(&env_next, "OPENAI_KEY=staged\n").expect("write env.next");
    let resp = env.rig.owner(Method::POST, "models/enable").await;
    assert_eq!(resp.status(), StatusCode::CONFLICT);
    assert_eq!(error_code(resp).await, "config_has_pending_changes");
    assert_eq!(
        std::fs::read_to_string(&env_next).expect("env.next"),
        "OPENAI_KEY=staged\n"
    );
    assert_eq!(
        std::fs::read_to_string(&env.config_path).expect("toml"),
        original_toml
    );
}

#[tokio::test]
async fn enable_preserves_hand_edited_disk_config_and_uninterpolated_env_vars() {
    let env = ModelRig::with_toml("").await;
    env.sign_in().await;

    let env_file = env.config_path.with_extension("env");
    std::fs::write(&env_file, "HAND_API_KEY=sk-hand-secret-value\n").expect("write env");

    let base = std::fs::read_to_string(&env.config_path).expect("read base");
    let hand_edited = format!(
        "# operator note at top\n{base}\n\
         [[endpoint]]\n\
         id = \"hand-ep\"\n\
         protocol = \"openai\"\n\
         base_url = \"http://127.0.0.1:9/v1\"\n\
         api_key = \"${{HAND_API_KEY}}\"\n\n\
         [[model]]\n\
         name = \"hand-model\"\n\
         description = \"Hand-edited model\"\n\
         upstream = \"hand-model\"\n\
         endpoints = [\"hand-ep\"]\n\
         context = 64000\n"
    );
    std::fs::write(&env.config_path, &hand_edited).expect("write hand-edited");

    let resp = env.rig.owner(Method::POST, "models/enable").await;
    assert_eq!(resp.status(), StatusCode::OK);

    // Original file (including `#` comment) is preserved in `.prev`.
    let prev = std::fs::read_to_string(env.config_path.with_file_name("gateway.toml.prev"))
        .expect("read prev");
    assert_eq!(prev, hand_edited);

    // Disk `gateway.toml` keeps `hand-ep`, `hand-model`, AND raw `${HAND_API_KEY}`
    // (never expanding the secret into `gateway.toml`), plus the new plan models.
    let disk = std::fs::read_to_string(&env.config_path).expect("read disk");
    assert!(disk.contains("${HAND_API_KEY}"), "{disk}");
    assert!(!disk.contains("sk-hand-secret-value"), "{disk}");
    assert!(disk.contains("name = \"hand-model\""), "{disk}");
    assert!(disk.contains("name = \"gpt-5\""), "{disk}");

    // Live routing picked up both the hand-added model and the plan models.
    let listed = env.listed_models(true).await;
    assert!(listed.contains(&"hand-model".to_owned()), "{listed:?}");
    assert!(listed.contains(&"gpt-5".to_owned()), "{listed:?}");
}

#[tokio::test]
async fn enable_refuses_bad_sessions_missing_paths_and_bad_upstreams_and_retries_401_once() {
    // 1. Unsigned -> 409 chatgpt_sign_in_required, 0 `/models` hits.
    let env = ModelRig::with_toml("").await;
    let resp = env.rig.owner(Method::POST, "models/enable").await;
    assert_eq!(resp.status(), StatusCode::CONFLICT);
    assert_eq!(error_code(resp).await, "chatgpt_sign_in_required");
    assert_eq!(env.api.hits(), 0);

    // 2. Signed out -> 409 chatgpt_sign_in_required, 0 `/models` hits.
    env.sign_in().await;
    assert_eq!(
        env.rig.owner(Method::POST, "sign-out").await.status(),
        StatusCode::OK
    );
    let resp = env.rig.owner(Method::POST, "models/enable").await;
    assert_eq!(resp.status(), StatusCode::CONFLICT);
    assert_eq!(error_code(resp).await, "chatgpt_sign_in_required");
    assert_eq!(env.api.hits(), 0);

    // 3. Active without plan scope -> 409 chatgpt_plan_not_granted, 0 hits.
    let no_grant = ModelRig::with_toml("").await;
    no_grant.rig.fake.set(|b| {
        b.scope = "openid profile email offline_access".to_owned();
    });
    no_grant.sign_in().await;
    let resp = no_grant.rig.owner(Method::POST, "models/enable").await;
    assert_eq!(resp.status(), StatusCode::CONFLICT);
    assert_eq!(error_code(resp).await, "chatgpt_plan_not_granted");
    assert_eq!(no_grant.api.hits(), 0);

    // 4. Missing config_path -> 400 config_path_unavailable.
    let fake = Fake::start().await;
    let (_api, api_addr) = ModelsApi::start().await;
    let profile = tempfile::TempDir::new().expect("tempdir");
    let mut pathless = loopback_state(Some(false));
    pathless.chatgpt = ChatGpt::with_api_base(&format!("http://{api_addr}/v1"));
    pathless.chatgpt.open_on(profile.path(), fake.config());
    let owner_proof = pathless.chatgpt.owner_proof().to_owned();
    let addr = serve_state(pathless).await;
    let resp = call(
        addr,
        Method::POST,
        "models/enable",
        Some("test-token"),
        &[&owner_proof],
    )
    .await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    assert_eq!(error_code(resp).await, "config_path_unavailable");

    // 5. Redirect, 500, malformed JSON, and oversized body -> 502 Bad Gateway.
    let bad = ModelRig::with_toml("").await;
    bad.sign_in().await;
    let (trap, trap_addr) = ModelsApi::start().await;
    bad.api.set(|b| {
        b.redirect_to = Some(format!("http://{trap_addr}/v1/models"));
    });
    assert_eq!(
        bad.rig.owner(Method::POST, "models/enable").await.status(),
        StatusCode::BAD_GATEWAY
    );
    assert_eq!(trap.hits(), 0, "redirects must never be followed");

    bad.api.set(|b| {
        b.redirect_to = None;
        b.status = 500;
    });
    assert_eq!(
        bad.rig.owner(Method::POST, "models/enable").await.status(),
        StatusCode::BAD_GATEWAY
    );

    bad.api.set(|b| {
        b.status = 200;
        b.body = "{not-json".to_owned();
    });
    assert_eq!(
        bad.rig.owner(Method::POST, "models/enable").await.status(),
        StatusCode::BAD_GATEWAY
    );

    bad.api.set(|b| {
        b.body = " ".repeat(2 * 1024 * 1024);
    });
    assert_eq!(
        bad.rig.owner(Method::POST, "models/enable").await.status(),
        StatusCode::BAD_GATEWAY
    );

    // 6. Cached token rejected with 401 -> refreshes once and succeeds.
    let retry_env = ModelRig::with_toml("").await;
    retry_env.sign_in().await;
    retry_env.api.set(|b| b.fail_first_401 = true);
    let ok = retry_env.rig.owner(Method::POST, "models/enable").await;
    assert_eq!(ok.status(), StatusCode::OK);
    let seen_auth = retry_env.api.auth_headers();
    assert_eq!(seen_auth.len(), 2);
    assert_ne!(seen_auth[0], seen_auth[1], "retry must use refreshed token");
}

#[tokio::test]
async fn fence_aborts_write_when_sign_in_changes_mid_fetch() {
    // Case A: sign-out while `/models` is in flight.
    let env = ModelRig::with_toml("").await;
    env.sign_in().await;
    let before = std::fs::read_to_string(&env.config_path).expect("read before");
    env.api.set(|b| b.hold = true);

    let entered = env.api.entered.notified();
    let addr = env.rig.addr;
    let proof = env.rig.owner_proof.clone();
    let enable_task = tokio::spawn(async move {
        call(
            addr,
            Method::POST,
            "models/enable",
            Some("test-token"),
            &[&proof],
        )
        .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(5), entered)
        .await
        .expect("enable reaches /models");

    assert_eq!(
        env.rig.owner(Method::POST, "sign-out").await.status(),
        StatusCode::OK
    );
    env.api.release.notify_waiters();

    let resp = enable_task.await.expect("enable joined");
    assert_eq!(resp.status(), StatusCode::CONFLICT);
    assert_eq!(error_code(resp).await, "chatgpt_sign_in_changed");
    assert_eq!(
        std::fs::read_to_string(&env.config_path).expect("read after"),
        before
    );
    assert_eq!(env.rig.status().await["plan_models"], json!([]));

    // Case B: forget + sign-in as another account (`user-2`) mid-fetch.
    let env2 = ModelRig::with_toml("").await;
    env2.sign_in().await;
    let before2 = std::fs::read_to_string(&env2.config_path).expect("read before2");
    env2.api.set(|b| b.hold = true);

    let entered2 = env2.api.entered.notified();
    let addr2 = env2.rig.addr;
    let proof2 = env2.rig.owner_proof.clone();
    let enable_task2 = tokio::spawn(async move {
        call(
            addr2,
            Method::POST,
            "models/enable",
            Some("test-token"),
            &[&proof2],
        )
        .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(5), entered2)
        .await
        .expect("enable2 reaches /models");

    assert_eq!(
        env2.rig.owner(Method::POST, "forget").await.status(),
        StatusCode::OK
    );
    env2.rig.fake.set(|b| b.subject = "user-2".to_owned());
    env2.sign_in().await;
    env2.api.release.notify_waiters();

    let resp2 = enable_task2.await.expect("enable2 joined");
    assert_eq!(resp2.status(), StatusCode::CONFLICT);
    assert_eq!(error_code(resp2).await, "chatgpt_sign_in_changed");
    assert_eq!(
        std::fs::read_to_string(&env2.config_path).expect("read after2"),
        before2
    );
    assert_eq!(env2.rig.status().await["plan_models"], json!([]));
}

#[derive(Clone, Default)]
struct BufWriter(Arc<Mutex<Vec<u8>>>);

struct GuardWriter(Arc<Mutex<Vec<u8>>>);

impl Write for GuardWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().expect("log buf").extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for BufWriter {
    type Writer = GuardWriter;

    fn make_writer(&'a self) -> Self::Writer {
        GuardWriter(Arc::clone(&self.0))
    }
}

#[tokio::test]
async fn nothing_beyond_the_projection_reaches_the_response_config_or_logs() {
    let writer = BufWriter::default();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_writer(writer.clone())
        .without_time()
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);

    let env = ModelRig::with_toml("").await;
    env.sign_in().await;
    tokio::spawn(async { tracing::info!("canary_listener_alive") })
        .await
        .expect("probe log");

    env.api.set(|b| {
        b.extra_header = Some(("x-openai-debug".to_owned(), "CANARY_HDR_99".to_owned()));
        b.body = json!({
            "models": [
                {
                    "slug": "gpt-5",
                    "display_name": "GPT-5",
                    "visibility": "list",
                    "context_window": 400_000,
                    "base_instructions": "CANARY_PROMPT_99",
                    "organization": "CANARY_ORG_99"
                },
                {
                    "slug": "CANARY_HIDDEN_SLUG",
                    "display_name": "CANARY_HIDDEN_NAME",
                    "visibility": "hide",
                    "context_window": 128_000
                }
            ],
            "account_tier": "CANARY_TIER_99"
        })
        .to_string();
    });

    let ok_resp = env.rig.owner(Method::POST, "models/enable").await;
    assert_eq!(ok_resp.status(), StatusCode::OK);
    let ok_text = ok_resp.text().await.expect("ok text");

    // Also exercise upstream error and parse-failure paths with sensitive bodies.
    env.api.set(|b| {
        b.status = 500;
        b.body = "CANARY_ERR_BODY_99".to_owned();
    });
    let err_resp = env.rig.owner(Method::POST, "models/enable").await;
    assert_eq!(err_resp.status(), StatusCode::BAD_GATEWAY);
    let err_text = err_resp.text().await.expect("err text");

    env.api.set(|b| {
        b.status = 200;
        b.body = "{\"secret\":\"CANARY_PARSE_TOKEN_99\"".to_owned();
    });
    let bad_json = env.rig.owner(Method::POST, "models/enable").await;
    assert_eq!(bad_json.status(), StatusCode::BAD_GATEWAY);
    let bad_json_text = bad_json.text().await.expect("bad json text");

    let disk = std::fs::read_to_string(&env.config_path).expect("disk");
    let logs = String::from_utf8_lossy(&writer.0.lock().expect("log buf")).into_owned();
    assert!(
        logs.contains("canary_listener_alive"),
        "log sink must capture spawned tasks"
    );

    let mut forbidden = vec![
        "CANARY_HDR_99".to_owned(),
        "CANARY_PROMPT_99".to_owned(),
        "CANARY_ORG_99".to_owned(),
        "CANARY_HIDDEN_SLUG".to_owned(),
        "CANARY_HIDDEN_NAME".to_owned(),
        "CANARY_TIER_99".to_owned(),
        "CANARY_ERR_BODY_99".to_owned(),
        "CANARY_PARSE_TOKEN_99".to_owned(),
        env.rig.owner_proof.clone(),
        env.rig.plan_proof.clone(),
    ];
    forbidden.extend(env.rig.fake.seen(|s| s.valid_refresh_tokens.clone()));
    for auth in env.api.auth_headers() {
        if let Some(tok) = auth.strip_prefix("Bearer ") {
            forbidden.push(tok.to_owned());
        }
    }

    for secret in &forbidden {
        assert!(!ok_text.contains(secret), "ok response leaked {secret}");
        assert!(!err_text.contains(secret), "err response leaked {secret}");
        assert!(!bad_json_text.contains(secret), "parse err leaked {secret}");
        assert!(!disk.contains(secret), "gateway.toml leaked {secret}");
        assert!(!logs.contains(secret), "tracing logs leaked {secret}");
    }
}

#[path = "models-tests-races.rs"]
mod races;
