//! Test support: an issuer that signs real RS256 JWTs, so verification runs
//! the same code path it runs against OpenAI.

use std::sync::LazyLock;

use aws_lc_rs::rand::SystemRandom;
use aws_lc_rs::rsa::{KeyPair, KeySize};
use aws_lc_rs::signature::{KeyPair as _, RSA_PKCS1_SHA256, RsaPublicKeyComponents};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};

/// The issuer the tests pin.
#[cfg(test)]
pub(crate) const ISSUER: &str = "https://auth.example.test";

/// One generated signing key, shared by every test in the process.
static KEY: LazyLock<KeyPair> =
    LazyLock::new(|| KeyPair::generate(KeySize::Rsa2048).unwrap_or_else(|_| unreachable!()));

/// The test issuer's JWKS document, with the key under `kid`.
pub(crate) fn jwks_document(kid: &str) -> Value {
    let public = RsaPublicKeyComponents::<Vec<u8>>::from(KEY.public_key());
    json!({ "keys": [
        { "kty": "EC", "kid": "ignored", "crv": "P-256", "x": "AA", "y": "AA" },
        { "kty": "RSA", "kid": kid, "alg": "RS256", "use": "sig",
          "n": URL_SAFE_NO_PAD.encode(&public.n),
          "e": URL_SAFE_NO_PAD.encode(&public.e) }
    ]})
}

/// A JWT with `header` and `claims`, signed by the test key.
pub(crate) fn sign(header: &Value, claims: &Value) -> String {
    let signed = format!(
        "{}.{}",
        URL_SAFE_NO_PAD.encode(header.to_string()),
        URL_SAFE_NO_PAD.encode(claims.to_string())
    );
    let mut signature = vec![0_u8; KEY.public_modulus_len()];
    KEY.sign(
        &RSA_PKCS1_SHA256,
        &SystemRandom::new(),
        signed.as_bytes(),
        &mut signature,
    )
    .unwrap_or_else(|_| unreachable!());
    format!("{signed}.{}", URL_SAFE_NO_PAD.encode(signature))
}

/// The `at_hash` of `access_token`.
pub(crate) fn at_hash(access_token: &str) -> String {
    let digest = Sha256::digest(access_token.as_bytes());
    URL_SAFE_NO_PAD.encode(&digest[..digest.len() / 2])
}
