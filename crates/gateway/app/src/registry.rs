//! The route registry: every mounted route as data, with the tier that
//! mounts it.
//!
//! Each area module declares one [`RouteInfo`] constant per route and
//! binds that constant's path in its `routes()`, never a separate literal,
//! so the registry and the router cannot name different paths. The speech
//! crate's two routes and the config UI's asset routes are mounted by
//! those crates; [`all`] names them from the path constants those crates
//! export, under the same feature gates `build_router` merges them under.
//! The registry is the crate's one enumerable route list: the tests sweep
//! it to prove every walled route refuses a LAN peer and every open route
//! does not, and a route declared in the wrong tier fails that sweep
//! rather than a reader's memory.

use axum::http::Method;

use crate::admin;
#[cfg(feature = "local")]
use crate::cache;
#[cfg(feature = "web-search")]
use crate::web_search;
use crate::{health, models, relay, speech};

/// Which tier mounts a route.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Tier {
    /// Bearer-authed, reachable from any peer the listener admits.
    Open,
    /// Behind the shared loopback wall: a non-loopback peer receives 403
    /// before auth runs.
    Walled,
}

/// One mounted route: its axum path template, the methods it answers,
/// and the tier that mounts it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RouteInfo {
    /// The path template as `Router::route` receives it, captures in
    /// braces.
    pub(crate) path: &'static str,
    /// The methods mounted on the path.
    pub(crate) methods: &'static [Method],
    /// The tier that mounts the route.
    pub(crate) tier: Tier,
}

impl RouteInfo {
    /// A route in the open tier.
    pub(crate) const fn open(path: &'static str, methods: &'static [Method]) -> RouteInfo {
        RouteInfo {
            path,
            methods,
            tier: Tier::Open,
        }
    }

    /// A route in the walled tier.
    pub(crate) const fn walled(path: &'static str, methods: &'static [Method]) -> RouteInfo {
        RouteInfo {
            path,
            methods,
            tier: Tier::Walled,
        }
    }
}

/// The config UI assets, mounted at `/config` behind that crate's own
/// loopback wall. Each path is that mount joined to one of
/// `gateway_config_ui::ASSET_PATHS`, and `registry-tests` fails when the
/// two disagree.
#[cfg(feature = "config-ui")]
const CONFIG_UI_ROUTES: &[RouteInfo] = &[
    RouteInfo::walled("/config/", &[Method::GET]),
    RouteInfo::walled("/config/app.js", &[Method::GET]),
    RouteInfo::walled("/config/app.css", &[Method::GET]),
    RouteInfo::walled("/config/icons/runvil-icon.png", &[Method::GET]),
    RouteInfo::walled("/config/icons/runvil-icon@2x.png", &[Method::GET]),
];

/// The speech crate's routes. `build_router` merges them after
/// `with_state`, because they carry that crate's state rather than
/// `AppState`. The registry names the same path constants the crate binds.
#[cfg(feature = "stt")]
const TRANSCRIPTIONS: RouteInfo =
    RouteInfo::open(gateway_stt::TRANSCRIPTIONS_PATH, &[Method::POST]);
#[cfg(feature = "stt")]
const REALTIME: RouteInfo = RouteInfo::open(gateway_stt::REALTIME_PATH, &[Method::GET]);

/// Every route the assembled router mounts in this build, in mount order.
pub(crate) fn all() -> Vec<RouteInfo> {
    let mut routes = Vec::new();
    routes.extend_from_slice(relay::ROUTES);
    routes.extend_from_slice(speech::ROUTES);
    routes.extend_from_slice(models::ROUTES);
    routes.extend_from_slice(health::ROUTES);
    routes.extend(admin::open::registry());
    #[cfg(feature = "web-search")]
    routes.extend_from_slice(web_search::ROUTES);
    #[cfg(feature = "local")]
    routes.extend_from_slice(cache::ROUTES);
    routes.extend(admin::walled::registry());
    // The config UI nest and the speech merge follow the walled tier in
    // `build_router`, and the registry keeps that order.
    #[cfg(feature = "config-ui")]
    routes.extend_from_slice(CONFIG_UI_ROUTES);
    #[cfg(feature = "stt")]
    routes.extend_from_slice(&[TRANSCRIPTIONS, REALTIME]);
    routes
}

#[cfg(test)]
#[path = "registry-tests.rs"]
mod tests;
