//! Pure TOML table transforms for adding and removing `auth = "chatgpt-plan"`
//! endpoints and model bindings in `gateway.toml`.

use std::collections::HashSet;

use crate::error::GatewayError;

const DEFAULT_PLAN_ENDPOINT_ID: &str = "chatgpt";
const FALLBACK_PLAN_ENDPOINT_ID: &str = "chatgpt-plan";

pub(super) fn ensure_plan_endpoint(
    root: &mut toml::map::Map<String, toml::Value>,
) -> Result<(String, HashSet<String>, bool), GatewayError> {
    let mut all_ids = HashSet::new();
    let mut plan_ids = Vec::new();
    if let Some(endpoints) = root.get("endpoint").and_then(toml::Value::as_array) {
        for ep in endpoints {
            let Some(id) = ep.get("id").and_then(toml::Value::as_str) else {
                continue;
            };
            all_ids.insert(id.to_owned());
            if ep.get("auth").and_then(toml::Value::as_str) == Some("chatgpt-plan") {
                plan_ids.push(id.to_owned());
            }
        }
    }
    if let Some(first) = plan_ids.first().cloned() {
        return Ok((first, plan_ids.into_iter().collect(), false));
    }

    let chosen = pick_endpoint_id(&all_ids);
    let entry = root
        .entry("endpoint")
        .or_insert_with(|| toml::Value::Array(Vec::new()));
    let toml::Value::Array(endpoints) = entry else {
        return Err(GatewayError::ConfigWriteRejected(
            "`endpoint` must be an array of tables".to_owned(),
        ));
    };
    let mut table = toml::map::Map::new();
    table.insert("id".to_owned(), toml::Value::String(chosen.clone()));
    table.insert(
        "protocol".to_owned(),
        toml::Value::String("openai-responses".to_owned()),
    );
    table.insert(
        "auth".to_owned(),
        toml::Value::String("chatgpt-plan".to_owned()),
    );
    endpoints.push(toml::Value::Table(table));

    let mut set = HashSet::new();
    set.insert(chosen.clone());
    Ok((chosen, set, true))
}

fn pick_endpoint_id(existing: &HashSet<String>) -> String {
    if !existing.contains(DEFAULT_PLAN_ENDPOINT_ID) {
        return DEFAULT_PLAN_ENDPOINT_ID.to_owned();
    }
    if !existing.contains(FALLBACK_PLAN_ENDPOINT_ID) {
        return FALLBACK_PLAN_ENDPOINT_ID.to_owned();
    }
    let mut idx = 2_u32;
    loop {
        let candidate = format!("{FALLBACK_PLAN_ENDPOINT_ID}-{idx}");
        if !existing.contains(&candidate) {
            return candidate;
        }
        idx += 1;
    }
}

pub(super) fn collect_catalog_names(
    root: &toml::map::Map<String, toml::Value>,
    plan_ids: &HashSet<String>,
) -> (HashSet<String>, HashSet<String>) {
    let mut names = HashSet::new();
    let mut bound_upstreams = HashSet::new();

    for section in ["model", "local_model", "stt_model"] {
        let Some(items) = root.get(section).and_then(toml::Value::as_array) else {
            continue;
        };
        for item in items {
            if let Some(name) = item.get("name").and_then(toml::Value::as_str) {
                names.insert(name.to_owned());
            }
            if section == "model" {
                // `Routing::from_config` binds only `endpoints[0]`, so an
                // API-first mixed row (`["paid", "chatgpt"]`) is not an active
                // plan binding.
                let uses_plan = item
                    .get("endpoints")
                    .and_then(toml::Value::as_array)
                    .and_then(|eps| eps.first())
                    .and_then(toml::Value::as_str)
                    .is_some_and(|ep| plan_ids.contains(ep));
                if uses_plan
                    && let Some(up) = item
                        .get("upstream")
                        .or_else(|| item.get("name"))
                        .and_then(toml::Value::as_str)
                {
                    bound_upstreams.insert(up.to_owned());
                }
            }
        }
    }
    (names, bound_upstreams)
}

pub(super) fn push_plan_model(
    root: &mut toml::map::Map<String, toml::Value>,
    name: &str,
    description: &str,
    context: u32,
    upstream: &str,
    endpoint_id: &str,
) -> Result<(), GatewayError> {
    let entry = root
        .entry("model")
        .or_insert_with(|| toml::Value::Array(Vec::new()));
    let toml::Value::Array(models) = entry else {
        return Err(GatewayError::ConfigWriteRejected(
            "`model` must be an array of tables".to_owned(),
        ));
    };
    let mut table = toml::map::Map::new();
    table.insert("name".to_owned(), toml::Value::String(name.to_owned()));
    table.insert(
        "description".to_owned(),
        toml::Value::String(description.to_owned()),
    );
    table.insert(
        "context".to_owned(),
        toml::Value::Integer(i64::from(context)),
    );
    table.insert(
        "upstream".to_owned(),
        toml::Value::String(upstream.to_owned()),
    );
    table.insert(
        "endpoints".to_owned(),
        toml::Value::Array(vec![toml::Value::String(endpoint_id.to_owned())]),
    );
    models.push(toml::Value::Table(table));
    Ok(())
}

pub(super) fn strip_plan_endpoints(root: &mut toml::map::Map<String, toml::Value>) -> Vec<String> {
    let Some(toml::Value::Array(endpoints)) = root.get_mut("endpoint") else {
        return Vec::new();
    };
    let mut removed = Vec::new();
    endpoints.retain(|ep| {
        let is_plan = ep.get("auth").and_then(toml::Value::as_str) == Some("chatgpt-plan");
        if is_plan && let Some(id) = ep.get("id").and_then(toml::Value::as_str) {
            removed.push(id.to_owned());
        }
        !is_plan
    });
    if endpoints.is_empty() {
        root.remove("endpoint");
    }
    removed
}

pub(super) fn strip_plan_models(
    root: &mut toml::map::Map<String, toml::Value>,
    removed_endpoints: &HashSet<&str>,
) -> (Vec<String>, Vec<String>) {
    let Some(toml::Value::Array(models)) = root.get_mut("model") else {
        return (Vec::new(), Vec::new());
    };
    let mut removed = Vec::new();
    let mut kept = Vec::new();
    models.retain_mut(|item| {
        let Some(table) = item.as_table_mut() else {
            return true;
        };
        let name = table
            .get("name")
            .and_then(toml::Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let Some(toml::Value::Array(eps)) = table.get_mut("endpoints") else {
            return true;
        };
        let before = eps.len();
        eps.retain(|ep| ep.as_str().is_none_or(|id| !removed_endpoints.contains(id)));
        if eps.len() == before {
            return true;
        }
        if eps.is_empty() {
            removed.push(name);
            false
        } else {
            kept.push(name);
            true
        }
    });
    if models.is_empty() {
        root.remove("model");
    }
    (removed, kept)
}
