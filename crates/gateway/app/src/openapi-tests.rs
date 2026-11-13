//! The document against the router, the committed artifact, and the source.

use std::collections::BTreeSet;

use axum::http::Method;

use super::document;
use super::exclusions::{Reason, active_exclusions, full_feature_build};
use super::fields::labeled;
use super::operations::operations;
use super::render::{self, diff};
use super::source;
use crate::registry::{self, Tier};

const ARTIFACT: &str = "openapi.json";
const REGENERATE: &str = "BLESS_OPENAPI=1 cargo test -p gateway --lib openapi";

fn artifact_path() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(ARTIFACT)
}

pub(super) fn generated() -> serde_json::Value {
    match document() {
        Ok(document) => document,
        Err(errors) => panic!("{}", errors.join("\n")),
    }
}

#[test]
fn every_mounted_route_is_documented_or_excluded() {
    let document = generated();
    let mut documented = BTreeSet::new();
    let paths = document["paths"].as_object().expect("paths");
    for (path, item) in paths {
        let item = item.as_object().expect("a path item");
        for method in item.keys() {
            documented.insert((path.clone(), method.to_ascii_uppercase()));
        }
    }
    let excluded = active_exclusions();
    for route in registry::all() {
        for method in route.methods {
            let pair = (route.path.to_owned(), method.as_str().to_owned());
            let in_document = documented.contains(&pair);
            let in_exclusion = excluded.iter().any(|exclusion| {
                exclusion.path == route.path && exclusion.methods.contains(method)
            });
            assert!(
                in_document ^ in_exclusion,
                "{method} {} must be exactly one of documented ({in_document}) or excluded ({in_exclusion}, tier {:?})",
                route.path,
                route.tier
            );
        }
    }
    for (path, method) in &documented {
        assert!(
            !path.contains("{*"),
            "{path} uses an axum wildcard template, which OpenAPI cannot express"
        );
        let mounted = registry::all().into_iter().any(|route| {
            route.path == path && route.methods.iter().any(|item| item.as_str() == method)
        });
        assert!(
            mounted,
            "{method} {path} is documented but nothing mounts it"
        );
    }
    for exclusion in &excluded {
        for method in exclusion.methods {
            let mounted = registry::all()
                .into_iter()
                .any(|route| route.path == exclusion.path && route.methods.contains(method));
            assert!(
                mounted,
                "{method} {} is excluded ({}) but nothing mounts it",
                exclusion.path, exclusion.reason
            );
        }
    }
    for exclusion in &excluded {
        let route = registry::all()
            .into_iter()
            .find(|route| route.path == exclusion.path)
            .expect("the exclusion is mounted");
        match exclusion.kind {
            Reason::OpenAdminDeferred => assert!(
                route.tier == Tier::Open && exclusion.path.starts_with("/admin"),
                "{} is deferred open admin ({})",
                exclusion.path,
                exclusion.reason
            ),
            Reason::WalledTier => assert_eq!(
                route.tier,
                Tier::Walled,
                "{} is walled ({})",
                exclusion.path,
                exclusion.reason
            ),
            Reason::ConfigUiAsset => assert!(
                exclusion.path.starts_with("/config/") && route.tier == Tier::Walled,
                "{} is a config UI asset ({})",
                exclusion.path,
                exclusion.reason
            ),
        }
    }
}

#[test]
fn every_operation_is_bound_to_the_handler_it_names() {
    for operation in operations().unwrap_or_else(|error| panic!("{error}")) {
        let mounts = source::mounts(operation.file).unwrap_or_else(|error| panic!("{error}"));
        let method = operation.method.as_str().to_ascii_lowercase();
        let bound = mounts.iter().any(|mount| {
            mount.path == operation.path
                && mount.method == method
                && mount.handler == operation.handler
        });
        assert!(
            bound,
            "{} {} should bind {} in {}, mounts: {mounts:?}",
            operation.method, operation.path, operation.handler, operation.file
        );
    }
}

/// The status an operation declares for success is in the document, including
/// a WebSocket upgrade's `101`, which has no body to describe.
#[test]
fn every_operation_documents_its_success_status() {
    let document = generated();
    for operation in operations().unwrap_or_else(|error| panic!("{error}")) {
        let method = operation.method.as_str().to_ascii_lowercase();
        let status = operation.success_status.to_string();
        assert!(
            document["paths"][operation.path][method.as_str()]["responses"]
                .get(&status)
                .is_some(),
            "{} {} documents no {status} response",
            operation.method,
            operation.path
        );
    }
}

#[cfg(feature = "stt")]
#[test]
fn the_multipart_fields_match_parse_form() {
    let fields = source::multipart_fields("crates/gateway/stt/api/src/batch.rs")
        .unwrap_or_else(|error| panic!("{error}"));
    let document = generated();
    let properties = &document["paths"]["/v1/audio/transcriptions"]["post"]["requestBody"]["content"]
        ["multipart/form-data"]["schema"]["properties"];
    let mut documented: Vec<&str> = properties
        .as_object()
        .expect("multipart properties")
        .keys()
        .map(String::as_str)
        .collect();
    documented.sort_unstable();
    let mut parsed = fields;
    parsed.sort();
    assert_eq!(documented, parsed, "a multipart field was added or dropped");
}

#[test]
fn a_labeled_paragraph_is_found_only_at_the_start_of_a_paragraph() {
    let doc = "Intro.\n\n`file`: The audio,\nover two lines.\n\nSee `model`: here.";
    assert_eq!(labeled(doc, "file"), Some("The audio,\nover two lines."));
    assert_eq!(labeled(doc, "model"), None, "a label inside a paragraph");
    assert_eq!(labeled(doc, "prompt"), None, "an absent label");
}

/// A multipart field, a path capture, a batch error code, and a realtime
/// refusal are each described by their labeled paragraph, never a fallback.
/// A paragraph the handler's own doc lends to a part is not repeated in the
/// operation's description or its success description.
#[cfg(any(feature = "local", feature = "stt"))]
#[test]
fn parts_without_their_own_doc_are_described_by_labeled_paragraphs() {
    use super::fields::labeled_paragraph;
    let document = generated();
    #[cfg(feature = "local")]
    {
        let cache = "crates/gateway/app/src/cache.rs";
        let delete = &document["paths"]["/v1/cache/{sha256}"]["delete"];
        let parameter = &delete["parameters"][0];
        assert_eq!(parameter["name"], "sha256");
        let paragraph = labeled_paragraph(cache, "delete_cache", "sha256")
            .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(parameter["description"].as_str(), Some(paragraph.as_str()));
        for text in [
            &delete["description"],
            &delete["responses"]["200"]["description"],
        ] {
            let text = text.as_str().expect("text");
            assert!(!text.contains(&paragraph), "`sha256` repeated: {text}");
        }
    }
    #[cfg(feature = "stt")]
    {
        let batch = "crates/gateway/stt/api/src/batch.rs";
        let post = &document["paths"]["/v1/audio/transcriptions"]["post"];
        let properties =
            post["requestBody"]["content"]["multipart/form-data"]["schema"]["properties"]
                .as_object()
                .expect("multipart properties");
        for (field, schema) in properties {
            let expected = labeled_paragraph(batch, "parse_form", field);
            assert_eq!(
                schema["description"].as_str(),
                expected.ok().as_deref(),
                "{field}"
            );
        }
        for status in ["400", "404", "413", "500"] {
            let response = &post["responses"][status];
            let code = response["x-runvil-error-codes"][0]
                .as_str()
                .expect("a code");
            let expected = labeled_paragraph(batch, "error_response", code);
            assert_eq!(
                response["description"].as_str(),
                expected.ok().as_deref(),
                "{status}"
            );
        }
        let realtime = "crates/gateway/stt/api/src/realtime/route.rs";
        let get = &document["paths"]["/v1/realtime"]["get"];
        let operation = get["description"].as_str().expect("text");
        for status in ["400", "429", "503"] {
            let paragraph = labeled_paragraph(realtime, "upgrade", status)
                .unwrap_or_else(|error| panic!("{error}"));
            let description = get["responses"][status]["description"]
                .as_str()
                .expect("text");
            assert!(description.ends_with(&paragraph), "{status}: {description}");
            assert!(
                !operation.contains(&paragraph),
                "`{status}` repeated: {operation}"
            );
        }
    }
}

#[test]
fn the_committed_document_matches_the_generated_one() {
    let document = generated();
    let text = render::render(&document);
    let path = artifact_path();
    let bless = std::env::var_os("BLESS_OPENAPI").is_some();
    assert!(
        !bless || full_feature_build(),
        "BLESS_OPENAPI needs local, web-search, config-ui, and stt; this build is missing at least one"
    );
    // The committed file describes the default build. A reduced build is
    // checked by `stripping_feature_markers_matches_a_reduced_build`.
    if !full_feature_build() {
        return;
    }
    let committed = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            assert!(
                bless,
                "{} is missing; write it with: {REGENERATE}",
                path.display()
            );
            std::fs::write(&path, &text)
                .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
            return;
        }
        Err(error) => panic!("{}: {error}", path.display()),
    };
    if committed == text {
        return;
    }
    if bless {
        std::fs::write(&path, &text).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
        return;
    }
    let committed_value: serde_json::Value = serde_json::from_str(&committed)
        .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    let changes = diff(&committed_value, &document, 50);
    panic!(
        "{} is stale:\n{}\nRewrite it with: {REGENERATE}",
        path.display(),
        changes.join("\n")
    );
}

#[test]
fn head_follows_get_except_on_the_realtime_upgrade() {
    use axum::body::Body;
    use axum::extract::ConnectInfo;
    use axum::http::header::AUTHORIZATION;
    use axum::http::{Request, StatusCode};
    use std::net::SocketAddr;
    use tower::ServiceExt as _;

    // Built before the probe runtime: error sampling starts its own runtime.
    let document = generated();
    let documented = document["paths"].as_object().expect("paths").clone();
    let (_temp, state) = crate::test_support::walled_fixture();
    let peer: SocketAddr = "127.0.0.1:50000".parse().expect("a socket address");
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("a runtime");
    for route in registry::all() {
        // Excluded routes include streams that never finish (`/admin/progress`).
        // Implicit HEAD is a claim about the routes the document describes.
        if !route.methods.contains(&Method::GET) || !documented.contains_key(route.path) {
            continue;
        }
        let path = route
            .path
            .replace("{owner}", "owner")
            .replace("{name}", "na%20me")
            .replace("{sha256}", "not-a-digest");
        let once = |method: Method| {
            let path = path.clone();
            let state = state.clone();
            async move {
                let mut request = Request::builder()
                    .method(method)
                    .uri(path)
                    .header(AUTHORIZATION, "Bearer test-token")
                    .body(Body::empty())
                    .expect("request builds");
                request.extensions_mut().insert(ConnectInfo(peer));
                let response = crate::build_router(state, None)
                    .oneshot(request)
                    .await
                    .expect("the router is infallible");
                let status = response.status();
                let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
                    .await
                    .expect("the body reads");
                (status, bytes)
            }
        };
        let (get_status, _) = runtime.block_on(async {
            tokio::time::timeout(std::time::Duration::from_secs(5), once(Method::GET))
                .await
                .unwrap_or_else(|_| panic!("GET {path} did not answer within 5s"))
        });
        let (head_status, head_body) = runtime.block_on(async {
            tokio::time::timeout(std::time::Duration::from_secs(5), once(Method::HEAD))
                .await
                .unwrap_or_else(|_| panic!("HEAD {path} did not answer within 5s"))
        });
        if route.path == "/v1/realtime" {
            assert_eq!(
                get_status,
                StatusCode::BAD_REQUEST,
                "GET /v1/realtime without upgrade headers"
            );
            assert_eq!(
                head_status,
                StatusCode::METHOD_NOT_ALLOWED,
                "HEAD /v1/realtime"
            );
            continue;
        }
        assert_eq!(head_status, get_status, "HEAD {path} status");
        assert!(
            head_body.is_empty(),
            "HEAD {path} body is empty, was {head_body:?}"
        );
    }
}

fn committed() -> serde_json::Value {
    let path = artifact_path();
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

#[test]
fn stripping_feature_markers_matches_a_reduced_build() {
    let committed = committed();
    let stripped = super::strip::strip_reduced(&committed);
    if full_feature_build() {
        assert_ne!(
            stripped, committed,
            "the default document should carry web-search or stt elements to strip"
        );
        return;
    }
    let generated = generated();
    if stripped != generated {
        let changes = diff(&stripped, &generated, 50);
        panic!(
            "the reduced document is not the committed one with web-search, stt, and config-ui removed:\n{}",
            changes.join("\n")
        );
    }
}

#[test]
fn cache_routes_are_gated_on_local_and_marked() {
    let registry = source::root().join("crates/gateway/app/src/registry.rs");
    let text = std::fs::read_to_string(&registry)
        .unwrap_or_else(|error| panic!("{}: {error}", registry.display()));
    let needle = "routes.extend_from_slice(cache::ROUTES)";
    let position = text
        .find(needle)
        .unwrap_or_else(|| panic!("{needle} is not in registry.rs"));
    let start = text[..position]
        .rfind("#[cfg")
        .unwrap_or_else(|| panic!("cache routes have no cfg attribute"));
    let attribute = text[start..].lines().next().unwrap_or("");
    assert!(
        attribute.contains("feature = \"local\""),
        "cache routes are compiled in by: {attribute}"
    );
    let document = generated();
    for (path, method) in [
        ("/v1/cache", "get"),
        ("/v1/cache", "post"),
        ("/v1/cache/{sha256}", "delete"),
    ] {
        assert_eq!(
            document["paths"][path][method]["x-runvil-feature"], "local",
            "{method} {path} must carry x-runvil-feature local"
        );
    }
}
