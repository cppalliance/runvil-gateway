//! Tests for ID-token verification: each check refuses, in order.

use serde_json::{Value, json};

use super::*;
use crate::testing::{ISSUER, at_hash, jwks_document, sign};

const NOW: u64 = 1_763_000_000;

fn header() -> Value {
    json!({ "alg": "RS256", "kid": "k1", "typ": "JWT" })
}

fn claims() -> Value {
    json!({
        "iss": ISSUER, "aud": ["oaiapp_1"], "sub": "user-1", "email": "a@example.test",
        "exp": NOW + 3600, "nonce": "n-1", "at_hash": at_hash("access-1"),
    })
}

fn expected() -> Expected<'static> {
    Expected {
        issuer: ISSUER,
        client_id: "oaiapp_1",
        nonce: Some("n-1"),
        access_token: "access-1",
        now: NOW,
    }
}

fn check(
    header: &Value,
    claims: &Value,
    expected: &Expected<'_>,
) -> Result<IdClaims, IdTokenError> {
    verify(
        &sign(header, claims),
        &Jwks::from_document(&jwks_document("k1")),
        expected,
    )
}

fn with(mut value: Value, key: &str, replacement: Value) -> Value {
    value[key] = replacement;
    value
}

#[test]
fn a_good_token_yields_its_subject_and_email() {
    let verified = check(&header(), &claims(), &expected()).expect("verifies");
    assert_eq!(
        verified,
        IdClaims {
            subject: "user-1".to_owned(),
            email: Some("a@example.test".to_owned())
        }
    );
}

#[test]
fn a_string_audience_a_refresh_without_nonce_and_a_missing_at_hash_verify() {
    let claims = with(claims(), "aud", json!("oaiapp_1"));
    let expected = Expected {
        nonce: None,
        ..expected()
    };
    assert!(check(&header(), &claims, &expected).is_ok());
    // `at_hash` is optional: a token without one still verifies.
    let mut bare = claims;
    bare.as_object_mut().expect("object").remove("at_hash");
    assert!(check(&header(), &bare, &expected).is_ok());
}

#[test]
fn each_check_refuses_with_its_own_error() {
    let cases: Vec<(Value, Value, IdTokenError)> = vec![
        (
            with(header(), "alg", json!("HS256")),
            claims(),
            IdTokenError::Algorithm,
        ),
        (
            with(header(), "alg", json!("none")),
            claims(),
            IdTokenError::Algorithm,
        ),
        (
            with(header(), "kid", json!("k2")),
            claims(),
            IdTokenError::UnknownKey,
        ),
        (
            header(),
            with(claims(), "iss", json!("https://evil.test")),
            IdTokenError::Issuer,
        ),
        (
            header(),
            with(claims(), "aud", json!(["oaiapp_2"])),
            IdTokenError::Audience,
        ),
        (
            header(),
            with(claims(), "aud", json!(["oaiapp_1", "someone-else"])),
            IdTokenError::Audience,
        ),
        (
            header(),
            with(claims(), "aud", json!(["oaiapp_1", 7])),
            IdTokenError::Audience,
        ),
        (
            header(),
            with(claims(), "aud", json!([])),
            IdTokenError::Audience,
        ),
        (
            header(),
            with(claims(), "at_hash", json!(42)),
            IdTokenError::AccessTokenHash,
        ),
        (
            header(),
            with(claims(), "exp", json!(NOW - 61)),
            IdTokenError::Expired,
        ),
        (
            header(),
            with(claims(), "nonce", json!("n-2")),
            IdTokenError::Nonce,
        ),
        (
            header(),
            with(claims(), "at_hash", json!(at_hash("other"))),
            IdTokenError::AccessTokenHash,
        ),
        (
            header(),
            with(claims(), "sub", json!("")),
            IdTokenError::Subject,
        ),
    ];
    for (header, claims, error) in cases {
        assert_eq!(
            check(&header, &claims, &expected()),
            Err(error.clone()),
            "{error}"
        );
    }
}

#[test]
fn expiry_allows_sixty_seconds_of_skew() {
    let claims = with(claims(), "exp", json!(NOW - 59));
    assert!(check(&header(), &claims, &expected()).is_ok());
}

#[test]
fn a_tampered_payload_fails_the_signature() {
    let token = sign(&header(), &claims());
    let mut parts: Vec<&str> = token.split('.').collect();
    let forged = URL_SAFE_NO_PAD.encode(with(claims(), "sub", json!("user-2")).to_string());
    parts[1] = &forged;
    let jwks = Jwks::from_document(&jwks_document("k1"));
    assert_eq!(
        verify(&parts.join("."), &jwks, &expected()),
        Err(IdTokenError::Signature)
    );
}

#[test]
fn malformed_tokens_are_refused() {
    let jwks = Jwks::from_document(&jwks_document("k1"));
    for token in ["", "a.b", "a.b.c.d", "!!.!!.!!"] {
        assert_eq!(
            verify(token, &jwks, &expected()),
            Err(IdTokenError::Malformed),
            "{token:?}"
        );
    }
}

#[test]
fn jwks_skips_keys_whose_use_or_alg_is_not_rs256_signing() {
    let token = sign(&header(), &claims());
    let rsa_key = jwks_document("k1")["keys"][1].clone();

    for rejected in [
        with(rsa_key.clone(), "use", json!("enc")),
        with(rsa_key.clone(), "use", json!(7)),
        with(rsa_key.clone(), "alg", json!("PS256")),
        with(rsa_key.clone(), "alg", json!("RS384")),
        with(rsa_key.clone(), "alg", Value::Null),
    ] {
        let jwks = Jwks::from_document(&json!({ "keys": [rejected] }));
        assert_eq!(
            verify(&token, &jwks, &expected()),
            Err(IdTokenError::UnknownKey)
        );
    }

    // RFC 7517 makes `use` and `alg` optional on a JWK; omitting them still verifies.
    let mut bare_key = rsa_key.clone();
    let object = bare_key.as_object_mut().expect("jwk object");
    object.remove("use");
    object.remove("alg");
    let bare_jwks = Jwks::from_document(&json!({ "keys": [bare_key] }));
    assert!(verify(&token, &bare_jwks, &expected()).is_ok());

    // A non-signing key listed ahead of the signing key never shadows it.
    let mut enc_shadow = with(rsa_key.clone(), "use", json!("enc"));
    enc_shadow["n"] = json!("AA");
    let mixed_jwks = Jwks::from_document(&json!({ "keys": [enc_shadow, rsa_key] }));
    assert!(verify(&token, &mixed_jwks, &expected()).is_ok());
}
