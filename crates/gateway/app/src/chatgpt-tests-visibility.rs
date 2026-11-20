//! Broker status gating for `plan_visible` and `authorize_plan`: signed-out,
//! non-granted, expired, and unreadable sessions hide plan models from
//! `GET /v1/models` and refuse inference before touching the network.

use reqwest::StatusCode;

use super::{OWNER_PROOF_HEADER, Rig, chat, every_route, listed, post, refusal};

#[tokio::test]
async fn signing_out_hides_plan_models_and_refuses_every_route_without_network() {
    let rig = Rig::new().await;
    let addr = rig.serve().await;
    let proof = rig.plan_proof();

    // Never signed in: every inference route refuses before kind checks or
    // backend/issuer calls, and listing omits the plan model.
    assert_eq!(listed(addr, &[&proof]).await, ["own-sol"]);
    for (path, body) in every_route("sol") {
        let outcome = refusal(post(addr, path, &body, &[&proof]).await).await;
        assert_eq!(
            outcome,
            (StatusCode::FORBIDDEN, "chatgpt_sign_in_required".to_owned()),
            "{path}"
        );
    }
    assert!(rig.fake.grants().is_empty());
    assert!(rig.sent().is_empty());

    // Signed in with plan granted: listed again.
    rig.sign_in(addr).await;
    assert_eq!(listed(addr, &[&proof]).await, ["sol", "own-sol"]);

    // Sign out: plan model disappears without editing config, and requests
    // get the sign-in-required code with zero backend traffic.
    let sign_out = reqwest::Client::new()
        .post(format!("http://{addr}/admin/chatgpt/sign-out"))
        .bearer_auth("test-token")
        .header(OWNER_PROOF_HEADER, rig.state.chatgpt.owner_proof())
        .send()
        .await
        .expect("the gateway answers");
    assert_eq!(sign_out.status(), StatusCode::OK);

    let grants_after_sign_out = rig.fake.grants();
    assert_eq!(listed(addr, &[&proof]).await, ["own-sol"]);
    for (path, body) in every_route("sol") {
        let outcome = refusal(post(addr, path, &body, &[&proof]).await).await;
        assert_eq!(
            outcome,
            (StatusCode::FORBIDDEN, "chatgpt_sign_in_required".to_owned()),
            "{path}"
        );
    }
    assert_eq!(rig.fake.grants(), grants_after_sign_out);
    assert!(rig.sent().is_empty());
}

#[tokio::test]
async fn a_sign_in_without_plan_grant_neither_lists_nor_serves_plan_models() {
    let rig = Rig::new().await;
    rig.fake.set(|behavior| {
        behavior.scope = "openid profile email offline_access resource.invoke".to_owned();
    });
    let addr = rig.serve().await;
    rig.sign_in(addr).await;
    let proof = rig.plan_proof();

    let grants_before = rig.fake.grants();
    assert_eq!(listed(addr, &[&proof]).await, ["own-sol"]);
    for (path, body) in every_route("sol") {
        let outcome = refusal(post(addr, path, &body, &[&proof]).await).await;
        assert_eq!(
            outcome,
            (StatusCode::FORBIDDEN, "chatgpt_plan_not_granted".to_owned()),
            "{path}"
        );
    }
    assert_eq!(rig.fake.grants(), grants_before);
    assert!(rig.sent().is_empty());
}

#[tokio::test]
async fn expired_and_unreadable_records_hide_plan_models_and_report_expired() {
    let rig = Rig::new().await;
    // Within the 120s refresh margin so the ID token verifies at sign-in while
    // the next `access_token()` call must refresh immediately.
    rig.fake.set(|behavior| behavior.expires_in = 60);
    let addr = rig.serve().await;
    rig.sign_in(addr).await;
    let proof = rig.plan_proof();

    // Force a refresh failure so the broker settles in NeedsAuth.
    rig.fake
        .set(|behavior| behavior.token_error = Some((400, "invalid_grant")));
    let _ = rig
        .state
        .chatgpt
        .broker()
        .expect("open")
        .access_token()
        .await;

    let grants_in_needs_auth = rig.fake.grants();
    assert_eq!(listed(addr, &[&proof]).await, ["own-sol"]);
    let outcome = refusal(post(addr, "/v1/chat/completions", &chat("sol"), &[&proof]).await).await;
    assert_eq!(
        outcome,
        (StatusCode::FORBIDDEN, "chatgpt_sign_in_expired".to_owned())
    );
    assert_eq!(
        rig.fake.grants(),
        grants_in_needs_auth,
        "authorize_plan rejects from in-memory status without refreshing"
    );
    assert!(rig.sent().is_empty());

    // An unreadable store file on a fresh start behaves the same way.
    let corrupt_rig = Rig::new().await;
    let store_dir = corrupt_rig.profile.path().join("chatgpt-plan");
    std::fs::create_dir_all(&store_dir).expect("mkdir");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&store_dir, std::fs::Permissions::from_mode(0o700))
            .expect("chmod dir");
    }
    let record_file = store_dir.join("chatgpt-plan.json");
    std::fs::write(&record_file, b"{corrupt").expect("write corrupt");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&record_file, std::fs::Permissions::from_mode(0o600))
            .expect("chmod file");
    }
    let corrupt_addr = corrupt_rig.serve().await;
    let corrupt_proof = corrupt_rig.plan_proof();
    assert_eq!(listed(corrupt_addr, &[&corrupt_proof]).await, ["own-sol"]);
    let outcome = refusal(
        post(
            corrupt_addr,
            "/v1/chat/completions",
            &chat("sol"),
            &[&corrupt_proof],
        )
        .await,
    )
    .await;
    assert_eq!(
        outcome,
        (StatusCode::FORBIDDEN, "chatgpt_sign_in_expired".to_owned())
    );
    assert!(corrupt_rig.fake.grants().is_empty());
}
