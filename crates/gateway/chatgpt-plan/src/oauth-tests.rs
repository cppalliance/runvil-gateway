//! Tests for the OAuth calls, against the fake sign-in service.

use std::collections::HashMap;

use super::*;
use crate::fake_issuer::Fake;

fn query(url: &str) -> HashMap<String, String> {
    Url::parse(url)
        .expect("a url")
        .query_pairs()
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect()
}

fn request(client_id: Option<&str>) -> AuthorizeRequest<'_> {
    AuthorizeRequest {
        client_id,
        agent_host_id: "urn:uuid:11111111-1111-4111-8111-111111111111",
        redirect_uri: "http://127.0.0.1:4567/auth/callback",
        state: "state-1",
        nonce: "nonce-1",
        verifier: "verifier-1",
        login_hint: Some("a@example.test"),
    }
}

#[tokio::test]
async fn a_first_sign_in_registers_and_names_the_agent() {
    let fake = Fake::start().await;
    let client = OAuthClient::new(fake.config());
    let url = client.authorize_url(&request(None)).await.expect("url");
    assert!(url.starts_with(&format!("{}/api/accounts/authorize?", fake.base)));
    let query = query(&url);
    assert_eq!(query["client_id"], "dynamic_agent_client");
    assert_eq!(query["agent_name_hint"], "Runvil");
    assert_eq!(
        query["ext_agent_host_id"],
        "urn:uuid:11111111-1111-4111-8111-111111111111"
    );
    assert_eq!(query["response_type"], "code");
    assert_eq!(query["redirect_uri"], "http://127.0.0.1:4567/auth/callback");
    assert_eq!(query["scope"], SCOPE);
    assert_eq!(query["resource"], "https://api.example.test/v1");
    assert_eq!(query["code_challenge_method"], "S256");
    // S256 of "verifier-1", base64url without padding.
    assert_eq!(
        query["code_challenge"],
        URL_SAFE_NO_PAD.encode(Sha256::digest(b"verifier-1"))
    );
    assert!(
        !query.contains_key("login_hint"),
        "a registration has no account yet"
    );
    assert!(
        !url.contains("verifier-1"),
        "the verifier never appears in the URL"
    );
}

#[tokio::test]
async fn a_later_sign_in_uses_the_issued_client_and_hints_the_account() {
    let fake = Fake::start().await;
    let client = OAuthClient::new(fake.config());
    let url = client
        .authorize_url(&request(Some("oaiapp_1")))
        .await
        .expect("url");
    let query = query(&url);
    assert_eq!(query["client_id"], "oaiapp_1");
    assert_eq!(query["login_hint"], "a@example.test");
    assert!(!query.contains_key("agent_name_hint"));
}

#[tokio::test]
async fn exchange_and_refresh_send_the_documented_fields() {
    let fake = Fake::start().await;
    let client = OAuthClient::new(fake.config());
    let tokens = client
        .exchange(
            "oaiapp_1",
            "code-1",
            "verifier-1",
            "http://127.0.0.1:4567/auth/callback",
        )
        .await
        .expect("exchanges");
    assert_eq!(tokens.access_token.expose(), "at-1");
    assert_eq!(tokens.expires_in, 3600);
    assert!(
        tokens
            .scopes
            .as_ref()
            .expect("the reply names its scopes")
            .iter()
            .any(|scope| scope == PLAN_SCOPE)
    );
    assert!(tokens.id_token.is_some());

    let refreshed = client
        .refresh(
            "oaiapp_1",
            tokens.refresh_token.as_ref().expect("a refresh token"),
        )
        .await
        .expect("refreshes");
    assert_eq!(refreshed.access_token.expose(), "at-2");
    assert_eq!(refreshed.refresh_token.expect("rotated").expose(), "rt-2");

    fake.seen(|seen| {
        let exchange = &seen.token_requests[0];
        assert_eq!(exchange["grant_type"], "authorization_code");
        assert_eq!(exchange["client_id"], "oaiapp_1");
        assert_eq!(exchange["code"], "code-1");
        assert_eq!(exchange["code_verifier"], "verifier-1");
        assert_eq!(
            exchange["redirect_uri"],
            "http://127.0.0.1:4567/auth/callback"
        );
        assert_eq!(exchange["resource"], "https://api.example.test/v1");
        let refresh = &seen.token_requests[1];
        assert_eq!(refresh["grant_type"], "refresh_token");
        assert_eq!(refresh["client_id"], "oaiapp_1");
        assert_eq!(refresh["refresh_token"], "rt-1");
        assert_eq!(refresh["resource"], "https://api.example.test/v1");
    });
}

#[tokio::test]
async fn a_refusal_keeps_only_a_plain_error_code() {
    let fake = Fake::start().await;
    let client = OAuthClient::new(fake.config());
    fake.set(|behavior| behavior.token_error = Some((400, "invalid_grant")));
    let refused = client
        .refresh("oaiapp_1", &Secret::new("rt-0".to_owned()))
        .await
        .expect_err("refused");
    assert_eq!(
        refused,
        OAuthError::Refused {
            status: 400,
            code: "invalid_grant".to_owned()
        }
    );
    assert!(refused.ends_the_session());
    assert!(!refused.to_string().contains("secret detail"));

    fake.set(|behavior| behavior.token_error = Some((400, "Not A Code; drop table")));
    let odd = client
        .refresh("oaiapp_1", &Secret::new("rt-0".to_owned()))
        .await
        .expect_err("refused");
    assert_eq!(
        odd,
        OAuthError::Refused {
            status: 400,
            code: "unrecognized_error".to_owned()
        }
    );
    assert!(!odd.ends_the_session());

    fake.set(|behavior| behavior.token_error = Some((503, "server_error")));
    let busy = client
        .refresh("oaiapp_1", &Secret::new("rt-0".to_owned()))
        .await
        .expect_err("refused");
    assert!(!busy.ends_the_session(), "a 503 does not end the session");
}

#[tokio::test]
async fn the_fake_refuses_what_the_real_service_would() {
    let fake = Fake::start().await;
    let client = OAuthClient::new(fake.config());
    let invalid_grant = OAuthError::Refused {
        status: 400,
        code: "invalid_grant".to_owned(),
    };

    // A refresh token the service never issued.
    let unknown = client
        .refresh("oaiapp_1", &Secret::new("rt-unknown".to_owned()))
        .await;
    assert_eq!(unknown.expect_err("refused"), invalid_grant);

    // A code exchange whose verifier does not match the challenge, or whose
    // redirect differs from the one the authorize URL carried.
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(b"verifier-1"));
    fake.expect_exchange(&challenge, "http://127.0.0.1:4567/auth/callback");
    let wrong_verifier = client
        .exchange(
            "oaiapp_1",
            "code-1",
            "verifier-2",
            "http://127.0.0.1:4567/auth/callback",
        )
        .await;
    assert_eq!(wrong_verifier.expect_err("refused"), invalid_grant);
    let wrong_redirect = client
        .exchange(
            "oaiapp_1",
            "code-1",
            "verifier-1",
            "http://127.0.0.1:9999/auth/callback",
        )
        .await;
    assert_eq!(wrong_redirect.expect_err("refused"), invalid_grant);

    // The matching exchange works, its refresh token works once, and a
    // revoked one stops working.
    let tokens = client
        .exchange(
            "oaiapp_1",
            "code-1",
            "verifier-1",
            "http://127.0.0.1:4567/auth/callback",
        )
        .await
        .expect("exchanges");
    let first = tokens.refresh_token.expect("a refresh token");
    let second = client
        .refresh("oaiapp_1", &first)
        .await
        .expect("refreshes")
        .refresh_token
        .expect("rotated");
    assert_eq!(
        client.refresh("oaiapp_1", &first).await.expect_err("spent"),
        invalid_grant
    );
    assert!(client.revoke("oaiapp_1", &second).await);
    assert_eq!(
        client
            .refresh("oaiapp_1", &second)
            .await
            .expect_err("revoked"),
        invalid_grant
    );
}

#[tokio::test]
async fn a_token_redirect_is_not_followed() {
    // A followed 307 would resend the refresh token to the new server.
    let target = Fake::start().await;
    let fake = Fake::start().await;
    fake.set(|behavior| {
        behavior.token_redirect = Some(format!("{}/api/accounts/oauth/token", target.base));
    });
    let client = OAuthClient::new(fake.config());
    let outcome = client
        .refresh("oaiapp_1", &Secret::new("rt-0".to_owned()))
        .await;
    assert!(
        matches!(outcome, Err(OAuthError::Refused { status: 307, .. })),
        "{outcome:?}"
    );
    assert!(
        target.grants().is_empty(),
        "the redirect target saw no request"
    );
}

#[tokio::test]
async fn an_unreachable_issuer_is_a_connect_failure_not_an_unknown_outcome() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let issuer = format!("http://{}", listener.local_addr().expect("addr"));
    drop(listener);
    let client = OAuthClient::new(OAuthConfig {
        issuer,
        resource: "https://api.example.test/v1".to_owned(),
        agent_name: "Runvil".to_owned(),
    });
    let outcome = client
        .refresh("oaiapp_1", &Secret::new("rt-0".to_owned()))
        .await;
    assert_eq!(outcome.expect_err("fails"), OAuthError::Connect);
}

#[tokio::test]
async fn revoke_is_confirmed_only_by_a_200() {
    let fake = Fake::start().await;
    let client = OAuthClient::new(fake.config());
    assert!(
        client
            .revoke("oaiapp_1", &Secret::new("rt-1".to_owned()))
            .await
    );
    fake.set(|behavior| behavior.revoke_status = 204);
    assert!(
        !client
            .revoke("oaiapp_1", &Secret::new("rt-2".to_owned()))
            .await
    );
    fake.set(|behavior| behavior.revoke_status = 500);
    assert!(
        !client
            .revoke("oaiapp_1", &Secret::new("rt-3".to_owned()))
            .await
    );
    assert_eq!(
        fake.seen(|seen| seen.revoked.clone()),
        ["rt-1", "rt-2", "rt-3"]
    );
}

#[tokio::test]
async fn discovery_naming_another_issuer_or_origin_is_ignored() {
    for wrong_issuer in [true, false] {
        let evil = Fake::start().await;
        let fake = Fake::start().await;
        fake.set(|behavior| {
            if wrong_issuer {
                behavior.discovery_issuer = Some(evil.base.clone());
            } else {
                behavior.discovery_token_endpoint =
                    Some(format!("{}/api/accounts/oauth/token", evil.base));
            }
        });
        fake.accept_refresh_token("rt-0");
        let client = OAuthClient::new(fake.config());
        assert_eq!(client.endpoints().await, Endpoints::documented(&fake.base));
        client
            .refresh("oaiapp_1", &Secret::new("rt-0".to_owned()))
            .await
            .expect("refreshes at the pinned issuer");
        assert!(
            evil.grants().is_empty(),
            "the other origin saw no token request"
        );
    }
}

#[tokio::test]
async fn a_discovery_that_could_not_be_fetched_is_tried_again() {
    let fake = Fake::start().await;
    fake.set(|behavior| behavior.discovery_status = 503);
    let client = OAuthClient::new(fake.config());
    // The issuer's discovery is failing: the documented endpoints serve, and
    // the failure is not remembered.
    assert_eq!(client.endpoints().await, Endpoints::documented(&fake.base));
    assert_eq!(client.endpoints().await, Endpoints::documented(&fake.base));
    assert_eq!(fake.seen(|seen| seen.discovery_hits), 2);

    // Once it answers, the answer is kept.
    fake.set(|behavior| behavior.discovery_status = 200);
    let _ = client.endpoints().await;
    let _ = client.endpoints().await;
    assert_eq!(fake.seen(|seen| seen.discovery_hits), 3);
}

#[tokio::test]
async fn the_jwks_verifies_the_fake_issuers_tokens() {
    let fake = Fake::start().await;
    fake.set(|behavior| behavior.nonce = "nonce-1".to_owned());
    let client = OAuthClient::new(fake.config());
    let tokens = client
        .exchange(
            "oaiapp_1",
            "code-1",
            "verifier-1",
            "http://127.0.0.1:1/auth/callback",
        )
        .await
        .expect("exchanges");
    let jwks = client.jwks().await.expect("jwks");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_secs();
    let claims = crate::id_token::verify(
        tokens.id_token.expect("an id token").expose(),
        &jwks,
        &crate::id_token::Expected {
            issuer: &fake.base,
            client_id: "oaiapp_1",
            nonce: Some("nonce-1"),
            access_token: tokens.access_token.expose(),
            now,
        },
    )
    .expect("verifies");
    assert_eq!(claims.subject, "user-1");
}

#[test]
fn random_tokens_are_long_distinct_and_url_safe() {
    let first = random_token(32).expect("randomness");
    let second = random_token(32).expect("randomness");
    assert_ne!(first, second);
    assert_eq!(first.len(), 43);
    assert!(
        first
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
    );
}

#[tokio::test]
async fn missing_no_redirect_oauth_client_fails_closed_as_never_left() {
    let fake = Fake::start().await;
    let client = OAuthClient::without_http_client(fake.config());

    let exchange = client
        .exchange("oaiapp_1", "code-1", "verifier-1", "http://127.0.0.1:1/cb")
        .await;
    assert_eq!(exchange.unwrap_err(), OAuthError::Connect);

    let refresh = client
        .refresh("oaiapp_1", &Secret::new("rt-seed".to_owned()))
        .await;
    assert_eq!(refresh.unwrap_err(), OAuthError::Connect);

    assert_eq!(client.jwks().await.unwrap_err(), OAuthError::Connect);
    assert!(
        !client
            .revoke("oaiapp_1", &Secret::new("rt-seed".to_owned()))
            .await
    );

    // Zero network requests left the process.
    assert_eq!(fake.seen(|seen| seen.discovery_hits), 0);
    assert!(fake.seen(|seen| seen.token_requests.is_empty()));
    assert_eq!(fake.seen(|seen| seen.jwks_hits), 0);
    assert!(fake.seen(|seen| seen.revoked.is_empty()));
}
