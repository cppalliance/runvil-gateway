//! `POST /admin/chatgpt/models/enable` and `POST /admin/chatgpt/models/forget`:
//! owner-gated mutations of `gateway.toml` and live routing for ChatGPT plan
//! endpoints and models.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use gateway_chatgpt_plan::plan::{ChatGptPlan, SessionFence};
use gateway_config::{Config, ProfileName, shadow_path, write_atomic};
#[cfg(feature = "web-search")]
use gateway_web_search::WebSearchState;
use serde::Serialize;

use crate::AppState;
use crate::auth::OwnerCaller;
use crate::chatgpt::PlanModelEntry;
use crate::config_shadow::{PendingEnv, rollback_path};
use crate::error::{GatewayError, blocking, config_write_error};
use crate::routing::Routing;

#[path = "models-toml.rs"]
mod toml_ops;

/// `POST /admin/chatgpt/models/enable` reply.
#[derive(Debug, Serialize)]
pub(super) struct EnableReply {
    endpoint: String,
    added: Vec<String>,
    renamed: Vec<RenamedModel>,
    skipped: Vec<SkippedModel>,
}

/// One plan model whose upstream slug collided with a non-plan model name.
#[derive(Debug, Serialize)]
pub(super) struct RenamedModel {
    from: String,
    to: String,
}

/// One plan model omitted from `gateway.toml`, with a stable reason code.
#[derive(Debug, Serialize)]
pub(super) struct SkippedModel {
    name: String,
    reason: &'static str,
}

/// `POST /admin/chatgpt/models/forget` reply.
#[derive(Debug, Serialize)]
pub(super) struct ForgetReply {
    removed_endpoints: Vec<String>,
    removed: Vec<String>,
    kept: Vec<String>,
}

/// Isolated naming rule when a plan model's `slug` is already taken by a
/// non-plan model in `gateway.toml`.
fn plan_collision_name(slug: &str) -> String {
    format!("{slug}-chatgpt")
}

/// `POST /admin/chatgpt/models/enable`: fetches the account's visible plan
/// models before taking the apply lock, then commits them to `gateway.toml`
/// and swaps live routing atomically under the session fence.
pub(super) async fn enable_models(
    State(state): State<AppState>,
    _owner: OwnerCaller,
) -> Result<Json<EnableReply>, GatewayError> {
    let plan = state.chatgpt.broker()?.clone();
    let config_path = crate::admin::config_path(&state)?.to_path_buf();
    let _ = plan.current_fence()?;
    if has_pending_drafts(&config_path) {
        return Err(GatewayError::ConfigHasPendingChanges);
    }

    let (fence, entries) = state.chatgpt.fetch_plan_models().await?;

    let _apply_guard = state.apply.lock().await;
    if !plan.verify_fence(&fence) {
        return Err(GatewayError::ChatGptSignInChanged);
    }
    if has_pending_drafts(&config_path) {
        return Err(GatewayError::ConfigHasPendingChanges);
    }

    let (reply, prepared) = {
        let path = config_path.clone();
        blocking(move || prepare_enable(&path, entries)).await??
    };
    commit_prepared(&state, config_path, prepared, Some((&plan, &fence))).await?;
    Ok(Json(reply))
}

/// `POST /admin/chatgpt/models/forget`: removes `auth = "chatgpt-plan"`
/// endpoints and plan-only models from `gateway.toml` and live routing.
pub(super) async fn forget_models(
    State(state): State<AppState>,
    _owner: OwnerCaller,
) -> Result<Json<ForgetReply>, GatewayError> {
    let plan = state.chatgpt.broker()?.clone();
    let config_path = crate::admin::config_path(&state)?.to_path_buf();

    let _apply_guard = state.apply.lock().await;
    if has_pending_drafts(&config_path) {
        return Err(GatewayError::ConfigHasPendingChanges);
    }
    plan.supersede_model_setup();

    let (reply, prepared) = {
        let path = config_path.clone();
        blocking(move || prepare_forget(&path)).await??
    };
    commit_prepared(&state, config_path, prepared, None).await?;
    Ok(Json(reply))
}

fn has_pending_drafts(config_path: &Path) -> bool {
    shadow_path(config_path).is_file() || shadow_path(&config_path.with_extension("env")).is_file()
}

fn read_toml_table(
    config_path: &Path,
) -> Result<(String, toml::map::Map<String, toml::Value>), GatewayError> {
    let raw = std::fs::read_to_string(config_path)
        .map_err(|source| GatewayError::ConfigWriteIo(Box::new(source)))?;
    let value: toml::Value = toml::from_str(&raw)
        .map_err(|error| GatewayError::ConfigWriteRejected(error.to_string()))?;
    let toml::Value::Table(table) = value else {
        return Err(GatewayError::ConfigWriteRejected(
            "gateway.toml root must be a TOML table".to_owned(),
        ));
    };
    Ok((raw, table))
}

struct PreparedMutation {
    disk_write: Option<(String, String)>,
    validated: Config,
}

fn prepare_enable(
    config_path: &Path,
    entries: Vec<PlanModelEntry>,
) -> Result<(EnableReply, PreparedMutation), GatewayError> {
    let (raw_toml, mut root) = read_toml_table(config_path)?;
    let mut mutated = false;

    let (endpoint_id, plan_ids, created_endpoint) = toml_ops::ensure_plan_endpoint(&mut root)?;
    if created_endpoint {
        mutated = true;
    }

    let (mut taken_names, mut bound_upstreams) = toml_ops::collect_catalog_names(&root, &plan_ids);
    let mut added = Vec::new();
    let mut renamed = Vec::new();
    let mut skipped = Vec::new();

    for entry in entries {
        let Some(context) = entry.context_window.filter(|&ctx| ctx > 0) else {
            skipped.push(SkippedModel {
                name: entry.slug,
                reason: "missing_context_window",
            });
            continue;
        };
        if bound_upstreams.contains(&entry.slug) {
            continue;
        }
        let chosen_name = if taken_names.contains(&entry.slug) {
            let candidate = plan_collision_name(&entry.slug);
            if taken_names.contains(&candidate) {
                skipped.push(SkippedModel {
                    name: entry.slug,
                    reason: "name_taken",
                });
                continue;
            }
            renamed.push(RenamedModel {
                from: entry.slug.clone(),
                to: candidate.clone(),
            });
            candidate
        } else {
            entry.slug.clone()
        };

        let description = entry.display_name.unwrap_or_else(|| entry.slug.clone());
        toml_ops::push_plan_model(
            &mut root,
            &chosen_name,
            &description,
            context,
            &entry.slug,
            &endpoint_id,
        )?;
        taken_names.insert(chosen_name.clone());
        bound_upstreams.insert(entry.slug);
        added.push(chosen_name);
        mutated = true;
    }

    let reply = EnableReply {
        endpoint: endpoint_id,
        added,
        renamed,
        skipped,
    };
    let prepared = prepare_toml(config_path, raw_toml, mutated.then_some(root))?;
    Ok((reply, prepared))
}

fn prepare_forget(config_path: &Path) -> Result<(ForgetReply, PreparedMutation), GatewayError> {
    let (raw_toml, mut root) = read_toml_table(config_path)?;
    let removed_endpoints = toml_ops::strip_plan_endpoints(&mut root);
    if removed_endpoints.is_empty() {
        let prepared = prepare_toml(config_path, raw_toml, None)?;
        return Ok((
            ForgetReply {
                removed_endpoints,
                removed: Vec::new(),
                kept: Vec::new(),
            },
            prepared,
        ));
    }

    let removed_set: HashSet<&str> = removed_endpoints.iter().map(String::as_str).collect();
    let (removed, kept) = toml_ops::strip_plan_models(&mut root, &removed_set);
    let prepared = prepare_toml(config_path, raw_toml, Some(root))?;
    Ok((
        ForgetReply {
            removed_endpoints,
            removed,
            kept,
        },
        prepared,
    ))
}

fn prepare_toml(
    config_path: &Path,
    raw_before: String,
    mutated_root: Option<toml::map::Map<String, toml::Value>>,
) -> Result<PreparedMutation, GatewayError> {
    let pending_env = PendingEnv::new(config_path)?;
    if let Some(root) = mutated_root {
        let rendered = toml::to_string_pretty(&toml::Value::Table(root))
            .map_err(|error| GatewayError::ConfigWriteRejected(error.to_string()))?;
        let validated =
            Config::from_toml_with_lookup(&rendered, Some(config_path), &|name: &str| {
                pending_env.resolve_var(name)
            })
            .map_err(config_write_error)?;
        Ok(PreparedMutation {
            disk_write: Some((raw_before, rendered)),
            validated,
        })
    } else {
        let validated =
            Config::from_toml_with_lookup(&raw_before, Some(config_path), &|name: &str| {
                pending_env.resolve_var(name)
            })
            .map_err(config_write_error)?;
        Ok(PreparedMutation {
            disk_write: None,
            validated,
        })
    }
}

fn write_config_files(
    config_path: &Path,
    raw_before: &str,
    rendered: &str,
) -> Result<(), GatewayError> {
    let prev = rollback_path(config_path);
    if !prev.exists() {
        write_atomic(&prev, raw_before).map_err(config_write_error)?;
    }
    write_atomic(config_path, rendered).map_err(config_write_error)?;
    Ok(())
}

async fn commit_prepared(
    state: &AppState,
    config_path: PathBuf,
    prepared: PreparedMutation,
    fence_check: Option<(&ChatGptPlan, &SessionFence)>,
) -> Result<(), GatewayError> {
    let mut live = state.live.write().await;
    let active_name = live
        .config
        .active_profile()
        .and_then(|profile| ProfileName::parse(profile.name()).ok());
    let selected = match active_name.as_ref() {
        Some(name) => prepared
            .validated
            .select_profile(Some(name))
            .or_else(|_| prepared.validated.select_profile(None)),
        None => prepared.validated.select_profile(None),
    }
    .map_err(config_write_error)?;

    let remote = Routing::from_config(&selected)
        .map_err(|error| GatewayError::switch_failed("build-routing", error))?;
    #[cfg(feature = "local")]
    let routing = remote
        .merge(live.local.models().iter().cloned())
        .map_err(|error| GatewayError::switch_failed("merge-routing", error))?;
    #[cfg(not(feature = "local"))]
    let routing = remote;
    #[cfg(feature = "web-search")]
    let web_search = selected
        .web_search_config()
        .map(WebSearchState::new)
        .map(Arc::new);

    #[cfg(test)]
    state.park_at(crate::park::Phase::ChatGptEnableCommit).await;
    match (fence_check, prepared.disk_write) {
        (Some((plan, fence)), Some((raw_before, rendered))) => {
            let plan = plan.clone();
            let fence = fence.clone();
            blocking(move || {
                plan.commit_fenced(
                    &fence,
                    || GatewayError::ChatGptSignInChanged,
                    || write_config_files(&config_path, &raw_before, &rendered),
                )
            })
            .await??;
        }
        (Some((plan, fence)), None) => {
            plan.commit_fenced(fence, || GatewayError::ChatGptSignInChanged, || Ok(()))?;
        }
        (None, Some((raw_before, rendered))) => {
            blocking(move || write_config_files(&config_path, &raw_before, &rendered)).await??;
        }
        (None, None) => {}
    }

    live.routing = Arc::new(routing);
    live.config = Arc::new(selected);
    #[cfg(feature = "web-search")]
    {
        live.web_search = web_search;
    }
    Ok(())
}
