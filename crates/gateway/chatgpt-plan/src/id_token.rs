//! ID-token verification: the checks a token response must pass before any
//! of its tokens is kept.
//!
//! The order is fixed. The header's `alg` must be `RS256` before anything is
//! verified. The signature is checked against the issuer's JWKS key named by
//! `kid`; an unknown `kid` is its own error, so the caller can refetch the
//! JWKS once. Only then are the claims read: `iss`, `aud` (the issued client
//! id), `exp` with 60 seconds of skew, the sign-in `nonce` when one is
//! expected, `at_hash` when the token carries one, and a non-empty `sub`.
//! `aud` must name this client and no other audience.
//! Errors name the failed check only; no token content reaches a message.
//!
//! Reference: OpenAI's "Registration and sign-in", section "4. Validate the
//! ID token and ChatGPT plan permission",
//! <https://developers.openai.com/siwc/token-sharing-open-source/sign-in>.

use aws_lc_rs::signature::{RSA_PKCS1_2048_8192_SHA256, RsaPublicKeyComponents};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde_json::Value;
use sha2::{Digest as _, Sha256};

/// The clock skew allowed on `exp`.
const CLOCK_SKEW_SECS: u64 = 60;

/// The issuer's signing keys, as its JWKS document lists them. Keys that
/// are not RSA `RS256` signing keys are ignored.
#[derive(Debug, Clone, Default)]
pub struct Jwks {
    keys: Vec<(String, Vec<u8>, Vec<u8>)>,
}

impl Jwks {
    /// Reads the RSA signing keys from a JWKS document. A key that is not RSA,
    /// whose `use` is present and not `sig`, whose `alg` is present and not
    /// `RS256`, missing its `kid`, `n` or `e`, or with undecodable components,
    /// is skipped.
    #[must_use]
    pub fn from_document(document: &Value) -> Jwks {
        let keys = document
            .get("keys")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter(|key| key.get("kty").and_then(Value::as_str) == Some("RSA"))
            .filter(|key| key.get("use").is_none_or(|u| u.as_str() == Some("sig")))
            .filter(|key| key.get("alg").is_none_or(|a| a.as_str() == Some("RS256")))
            .filter_map(|key| {
                let kid = key.get("kid")?.as_str()?.to_owned();
                let n = URL_SAFE_NO_PAD.decode(key.get("n")?.as_str()?).ok()?;
                let e = URL_SAFE_NO_PAD.decode(key.get("e")?.as_str()?).ok()?;
                Some((kid, n, e))
            })
            .collect();
        Jwks { keys }
    }

    fn key(&self, kid: &str) -> Option<(&[u8], &[u8])> {
        self.keys
            .iter()
            .find(|(id, _, _)| id == kid)
            .map(|(_, n, e)| (n.as_slice(), e.as_slice()))
    }
}

/// What the caller expects of an ID token.
#[derive(Debug, Clone, Copy)]
pub struct Expected<'a> {
    /// The pinned issuer, compared exactly with `iss`.
    pub issuer: &'a str,
    /// The issued client id, which `aud` must name.
    pub client_id: &'a str,
    /// The sign-in attempt's nonce. `None` on a refresh, whose ID token
    /// carries no fresh nonce.
    pub nonce: Option<&'a str>,
    /// The access token issued with the ID token. Every token response
    /// carries one, so the `at_hash` check always has its subject.
    pub access_token: &'a str,
    /// The current time, in Unix seconds.
    pub now: u64,
}

/// The claims kept from a verified ID token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdClaims {
    /// The account's stable subject, `sub`.
    pub subject: String,
    /// The account's email, when the token names one.
    pub email: Option<String>,
}

/// Why an ID token was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum IdTokenError {
    /// Not three base64url parts holding JSON.
    #[error("the ID token is not a well-formed JWT")]
    Malformed,
    /// The header names an algorithm other than RS256.
    #[error("the ID token is not signed with RS256")]
    Algorithm,
    /// No JWKS key has the header's `kid`. Refetching the JWKS may help.
    #[error("no issuer key matches the ID token")]
    UnknownKey,
    /// The signature does not verify.
    #[error("the ID token's signature is invalid")]
    Signature,
    /// `iss` is not the pinned issuer.
    #[error("the ID token names another issuer")]
    Issuer,
    /// `aud` does not name the issued client.
    #[error("the ID token is for another client")]
    Audience,
    /// `exp` has passed, beyond the allowed skew.
    #[error("the ID token has expired")]
    Expired,
    /// The sign-in attempt's nonce does not match.
    #[error("the ID token does not match this sign-in")]
    Nonce,
    /// `at_hash` does not match the access token issued with it.
    #[error("the ID token does not match its access token")]
    AccessTokenHash,
    /// `sub` is missing or empty.
    #[error("the ID token names no account")]
    Subject,
}

/// Verifies `token` against `jwks` and `expected`, in the order the module
/// documentation gives.
///
/// # Errors
/// Returns the first [`IdTokenError`] that applies.
pub fn verify(token: &str, jwks: &Jwks, expected: &Expected<'_>) -> Result<IdClaims, IdTokenError> {
    let mut parts = token.split('.');
    let (Some(header), Some(payload), Some(signature), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(IdTokenError::Malformed);
    };
    let header_json = decode_json(header)?;
    if header_json.get("alg").and_then(Value::as_str) != Some("RS256") {
        return Err(IdTokenError::Algorithm);
    }
    let kid = header_json
        .get("kid")
        .and_then(Value::as_str)
        .ok_or(IdTokenError::UnknownKey)?;
    let (n, e) = jwks.key(kid).ok_or(IdTokenError::UnknownKey)?;
    let signature = URL_SAFE_NO_PAD
        .decode(signature)
        .map_err(|_| IdTokenError::Malformed)?;
    let signed = &token[..header.len() + 1 + payload.len()];
    RsaPublicKeyComponents { n, e }
        .verify(&RSA_PKCS1_2048_8192_SHA256, signed.as_bytes(), &signature)
        .map_err(|_| IdTokenError::Signature)?;

    let claims = decode_json(payload)?;
    if claims.get("iss").and_then(Value::as_str) != Some(expected.issuer) {
        return Err(IdTokenError::Issuer);
    }
    // The whole claim must be this client: a list with any other audience,
    // or any member that is not a string, is refused.
    let audience_matches = match claims.get("aud") {
        Some(Value::String(aud)) => aud == expected.client_id,
        Some(Value::Array(auds)) => {
            !auds.is_empty()
                && auds
                    .iter()
                    .all(|aud| aud.as_str() == Some(expected.client_id))
        }
        _ => false,
    };
    if !audience_matches {
        return Err(IdTokenError::Audience);
    }
    let exp = claims.get("exp").and_then(Value::as_u64).unwrap_or(0);
    if exp.saturating_add(CLOCK_SKEW_SECS) <= expected.now {
        return Err(IdTokenError::Expired);
    }
    if let Some(nonce) = expected.nonce
        && claims.get("nonce").and_then(Value::as_str) != Some(nonce)
    {
        return Err(IdTokenError::Nonce);
    }
    // `at_hash` is optional in the code flow. When the token carries one it
    // must be a string that matches the access token issued with it.
    if let Some(at_hash) = claims.get("at_hash") {
        let digest = Sha256::digest(expected.access_token.as_bytes());
        let expected_hash = URL_SAFE_NO_PAD.encode(&digest[..digest.len() / 2]);
        if at_hash.as_str() != Some(expected_hash.as_str()) {
            return Err(IdTokenError::AccessTokenHash);
        }
    }
    let subject = claims
        .get("sub")
        .and_then(Value::as_str)
        .filter(|sub| !sub.is_empty())
        .ok_or(IdTokenError::Subject)?
        .to_owned();
    let email = claims
        .get("email")
        .and_then(Value::as_str)
        .map(str::to_owned);
    Ok(IdClaims { subject, email })
}

fn decode_json(part: &str) -> Result<Value, IdTokenError> {
    let bytes = URL_SAFE_NO_PAD
        .decode(part)
        .map_err(|_| IdTokenError::Malformed)?;
    serde_json::from_slice(&bytes).map_err(|_| IdTokenError::Malformed)
}

#[cfg(test)]
#[path = "id_token-tests.rs"]
mod tests;
