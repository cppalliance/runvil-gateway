//! Routes the document leaves out, each with the reason the coverage test checks.
//!
//! A mounted route that is neither here nor in the document fails the test,
//! and so does an entry here that nothing mounts.

use axum::http::Method;

/// Why a mounted route is absent from the document.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Reason {
    /// Open admin tier, deferred to a follow-up.
    OpenAdminDeferred,
    /// Walled tier: loopback-only, and not a public API.
    WalledTier,
    /// A config UI asset, which is a page rather than an API.
    ConfigUiAsset,
}

/// A route the document deliberately leaves out, with the reason.
pub(super) struct Exclusion {
    /// The registry path.
    pub path: &'static str,
    /// The methods the route answers.
    pub methods: &'static [Method],
    /// The class of exclusion the coverage test checks.
    pub kind: Reason,
    /// The sentence naming why this route is absent.
    pub reason: &'static str,
    /// The feature that mounts it, when it is not in every build.
    pub feature: Option<&'static str>,
}

/// The routes the document does not describe. The coverage test fails when
/// a mounted route is missing from both this list and the document, and when
/// an entry here is not mounted.
const EXCLUSIONS: &[Exclusion] = &[
    Exclusion {
        path: "/admin/profiles",
        methods: &[Method::GET],
        kind: Reason::OpenAdminDeferred,
        reason: "open admin tier, deferred",
        feature: None,
    },
    Exclusion {
        path: "/admin/switch-profile",
        methods: &[Method::POST],
        kind: Reason::OpenAdminDeferred,
        reason: "open admin tier, deferred",
        feature: None,
    },
    Exclusion {
        path: "/admin/status",
        methods: &[Method::GET],
        kind: Reason::OpenAdminDeferred,
        reason: "open admin tier, deferred",
        feature: None,
    },
    Exclusion {
        path: "/admin/progress",
        methods: &[Method::GET],
        kind: Reason::OpenAdminDeferred,
        reason: "open admin tier, deferred",
        feature: None,
    },
    Exclusion {
        path: "/admin/queue/cancel",
        methods: &[Method::POST],
        kind: Reason::OpenAdminDeferred,
        reason: "open admin tier, deferred",
        feature: None,
    },
    Exclusion {
        path: "/admin/queue/cancel-pending",
        methods: &[Method::POST],
        kind: Reason::OpenAdminDeferred,
        reason: "open admin tier, deferred",
        feature: None,
    },
    Exclusion {
        path: "/shutdown",
        methods: &[Method::POST],
        kind: Reason::WalledTier,
        reason: "walled tier: reads nothing a public client needs and stops the process",
        feature: None,
    },
    Exclusion {
        path: "/admin/restart",
        methods: &[Method::POST],
        kind: Reason::WalledTier,
        reason: "walled tier",
        feature: None,
    },
    Exclusion {
        path: "/admin/system",
        methods: &[Method::GET],
        kind: Reason::WalledTier,
        reason: "walled tier",
        feature: None,
    },
    Exclusion {
        path: "/admin/config",
        methods: &[Method::GET, Method::PUT],
        kind: Reason::WalledTier,
        reason: "walled tier: the body is the rendered configuration, not a typed schema",
        feature: None,
    },
    Exclusion {
        path: "/admin/config-pending",
        methods: &[Method::GET],
        kind: Reason::WalledTier,
        reason: "walled tier",
        feature: None,
    },
    Exclusion {
        path: "/admin/config-dirty",
        methods: &[Method::GET],
        kind: Reason::WalledTier,
        reason: "walled tier",
        feature: None,
    },
    Exclusion {
        path: "/admin/config-apply",
        methods: &[Method::POST],
        kind: Reason::WalledTier,
        reason: "walled tier",
        feature: None,
    },
    Exclusion {
        path: "/admin/config-revert",
        methods: &[Method::POST],
        kind: Reason::WalledTier,
        reason: "walled tier",
        feature: None,
    },
    Exclusion {
        path: "/admin/env",
        methods: &[Method::GET, Method::PUT],
        kind: Reason::WalledTier,
        reason: "walled tier",
        feature: None,
    },
    Exclusion {
        path: "/admin/cloud-models",
        methods: &[Method::GET],
        kind: Reason::WalledTier,
        reason: "walled tier",
        feature: None,
    },
    Exclusion {
        path: "/admin/cloud-models/refresh",
        methods: &[Method::POST],
        kind: Reason::WalledTier,
        reason: "walled tier",
        feature: None,
    },
    Exclusion {
        path: "/admin/reveal",
        methods: &[Method::POST],
        kind: Reason::WalledTier,
        reason: "walled tier",
        feature: None,
    },
    Exclusion {
        path: "/admin/hf/search",
        methods: &[Method::GET],
        kind: Reason::WalledTier,
        reason: "walled tier",
        feature: None,
    },
    Exclusion {
        path: "/admin/hf/model/{owner}/{name}",
        methods: &[Method::GET],
        kind: Reason::WalledTier,
        reason: "walled tier",
        feature: None,
    },
    Exclusion {
        path: "/admin/hf/model/{owner}/{name}/readme",
        methods: &[Method::GET],
        kind: Reason::WalledTier,
        reason: "walled tier",
        feature: None,
    },
    Exclusion {
        path: "/admin/chat-templates",
        methods: &[Method::GET],
        kind: Reason::WalledTier,
        reason: "walled tier",
        feature: Some("local"),
    },
    Exclusion {
        path: "/admin/orphans",
        methods: &[Method::GET],
        kind: Reason::WalledTier,
        reason: "walled tier",
        feature: Some("local"),
    },
    Exclusion {
        path: "/admin/model-info",
        methods: &[Method::GET],
        kind: Reason::WalledTier,
        reason: "walled tier",
        feature: Some("local"),
    },
    Exclusion {
        path: "/config",
        methods: &[Method::GET],
        kind: Reason::WalledTier,
        reason: "walled tier: redirects to the settings page",
        feature: Some("config-ui"),
    },
    Exclusion {
        path: "/auth",
        methods: &[Method::GET],
        kind: Reason::WalledTier,
        reason: "walled tier: mints the browser session cookie",
        feature: Some("config-ui"),
    },
    Exclusion {
        path: "/config/",
        methods: &[Method::GET],
        kind: Reason::ConfigUiAsset,
        reason: "config UI asset, not an API",
        feature: Some("config-ui"),
    },
    Exclusion {
        path: "/config/app.js",
        methods: &[Method::GET],
        kind: Reason::ConfigUiAsset,
        reason: "config UI asset, not an API",
        feature: Some("config-ui"),
    },
    Exclusion {
        path: "/config/app.css",
        methods: &[Method::GET],
        kind: Reason::ConfigUiAsset,
        reason: "config UI asset, not an API",
        feature: Some("config-ui"),
    },
    Exclusion {
        path: "/config/icons/runvil-icon.png",
        methods: &[Method::GET],
        kind: Reason::ConfigUiAsset,
        reason: "config UI asset, not an API",
        feature: Some("config-ui"),
    },
    Exclusion {
        path: "/config/icons/runvil-icon@2x.png",
        methods: &[Method::GET],
        kind: Reason::ConfigUiAsset,
        reason: "config UI asset, not an API",
        feature: Some("config-ui"),
    },
];

/// Exclusions whose feature this build compiled in.
pub(super) fn active_exclusions() -> Vec<&'static Exclusion> {
    EXCLUSIONS
        .iter()
        .filter(|exclusion| feature_enabled(exclusion.feature))
        .collect()
}

fn feature_enabled(feature: Option<&str>) -> bool {
    match feature {
        None => true,
        Some("local") => cfg!(feature = "local"),
        Some("web-search") => cfg!(feature = "web-search"),
        Some("config-ui") => cfg!(feature = "config-ui"),
        Some("stt") => cfg!(feature = "stt"),
        Some(other) => panic!("unknown exclusion feature {other}"),
    }
}

/// Whether this build has every route-bearing feature. Blessing a partial
/// build would publish a document that drops routes.
pub(super) fn full_feature_build() -> bool {
    cfg!(all(
        feature = "local",
        feature = "web-search",
        feature = "config-ui",
        feature = "stt"
    ))
}
