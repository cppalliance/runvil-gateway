//! The Gateway's OpenAPI 3.1 description, assembled by utoipa from the
//! `#[utoipa::path]` attributes on the handlers and the `ToSchema` derives
//! on the wire types, and pinned as `openapi.json` beside this crate's
//! manifest.
//!
//! utoipa writes the document. [`markers`] then adds the `x-runvil-*`
//! markers its attributes cannot carry, each at a fixed JSON pointer, and
//! fails when a pointer no longer resolves.

use utoipa::openapi::security::{ApiKey, ApiKeyValue, HttpAuthScheme, HttpBuilder, SecurityScheme};
use utoipa::{Modify, OpenApi};

mod markers;

#[path = "openapi-tests.rs"]
mod tests;

#[path = "openapi-validity-tests.rs"]
mod validity_tests;

/// The cargo features that compile documented elements in, and whether
/// this build has each. `local` is on in every test build, because the
/// crate's self dev-dependency enables `test-fixtures`.
const ROUTE_FEATURES: [(&str, bool); 4] = [
    ("local", cfg!(feature = "local")),
    ("web-search", cfg!(feature = "web-search")),
    ("stt", cfg!(feature = "stt")),
    ("config-ui", cfg!(feature = "config-ui")),
];

/// The routes every build mounts.
#[derive(OpenApi)]
#[openapi(
    info(
        title = "Runvil Gateway",
        description = "The Gateway's HTTP API: OpenAI-shaped routes for chat, embeddings, rerank, speech, transcription, and the model list, plus the Gateway's own extensions. Every route follows the standard it is modeled on where one exists, and every addition is named as an extension (`x-runvil-extension`) and is opt-in where the protocol offers a way to ask. Every route except `/health` needs a credential, except that while `[server] trust_loopback` is on, the default, a loopback peer that sends none is admitted; the operations that admit it carry `x-runvil-loopback-trust`.",
        license(name = "BSL-1.0"),
    ),
    paths(
        crate::relay::chat_completions,
        crate::relay::embeddings,
        crate::relay::rerank,
        crate::speech::audio_speech,
        crate::speech::audio_voices,
        crate::models::list_models,
        crate::health::health,
    ),
    components(schemas(crate::wire::ChatChunk)),
    modifiers(&Conventions),
    security(("bearer" = []), ("sessionCookie" = [])),
    tags(
        (name = "relay", description = "The OpenAI passthrough surface: chat completions, embeddings, and rerank."),
        (name = "speech", description = "Speech synthesis and the voice catalog."),
        (name = "models", description = "The model catalog."),
        (name = "health", description = "The liveness probe."),
        (name = "web_search", description = "The Brave-backed web search tool."),
        (name = "cache", description = "The local blob cache, which downloads and keeps files by SHA-256."),
        (name = "stt", description = "Batch and realtime transcription."),
    ),
)]
struct GatewayApi;

/// The web search route, mounted in `web-search` builds.
#[cfg(feature = "web-search")]
#[derive(OpenApi)]
#[openapi(paths(crate::web_search::web_search))]
struct WebSearchApi;

/// The blob cache routes, mounted in `local` builds.
#[cfg(feature = "local")]
#[derive(OpenApi)]
#[openapi(paths(
    crate::cache::list_cache,
    crate::cache::post_cache,
    crate::cache::delete_cache
))]
struct CacheApi;

/// The document-level facts the derive attributes have no place for: the
/// two security schemes and the licence's SPDX identifier.
struct Conventions;

impl Modify for Conventions {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        if let Some(license) = openapi.info.license.as_mut() {
            license.identifier = Some("BSL-1.0".to_owned());
        }
        let components = openapi.components.get_or_insert_with(Default::default);
        components.add_security_scheme(
            "bearer",
            SecurityScheme::Http(
                HttpBuilder::new()
                    .scheme(HttpAuthScheme::Bearer)
                    .description(Some(
                        "An `Authorization: Bearer` token equal to the gateway's key. While `[server] trust_loopback` is on, the default, a loopback peer that sends no `Authorization` header and no `Sec-Fetch-Site` other than `same-origin` or `none` is admitted without one.",
                    ))
                    .build(),
            ),
        );
        components.add_security_scheme(
            "sessionCookie",
            SecurityScheme::ApiKey(ApiKey::Cookie(ApiKeyValue::with_description(
                "runvil-gateway-session",
                "The session cookie the `/auth` browser handoff sets, accepted only on a request whose `Sec-Fetch-Site` is `same-origin` or `none`.",
            ))),
        );
    }
}

/// Whether this build compiles in the named feature.
fn enabled(feature: &str) -> bool {
    ROUTE_FEATURES
        .iter()
        .any(|(name, on)| *name == feature && *on)
}

/// The features this build leaves out.
fn missing_features() -> Vec<&'static str> {
    ROUTE_FEATURES
        .iter()
        .filter(|(_, on)| !on)
        .map(|(name, _)| *name)
        .collect()
}

/// The description of this build: utoipa's document, with the feature
/// routes merged in and the markers added.
///
/// # Errors
/// Returns every marker pointer that did not resolve, or the serializer's
/// failure.
fn document() -> Result<serde_json::Value, Vec<String>> {
    let mut api = GatewayApi::openapi();
    #[cfg(feature = "web-search")]
    api.merge(WebSearchApi::openapi());
    #[cfg(feature = "local")]
    api.merge(CacheApi::openapi());
    #[cfg(feature = "stt")]
    api.merge(gateway_stt::SpeechApi::openapi());
    let mut document = serde_json::to_value(&api).map_err(|error| vec![error.to_string()])?;
    markers::apply(&mut document)?;
    Ok(document)
}

/// The committed rendering: sorted keys (the workspace bans serde_json's
/// `preserve_order`, so every map is sorted), two-space indent, and a
/// trailing newline.
///
/// # Errors
/// Returns the serializer's failure.
fn render(document: &serde_json::Value) -> Result<String, serde_json::Error> {
    serde_json::to_string_pretty(document).map(|text| text + "\n")
}
