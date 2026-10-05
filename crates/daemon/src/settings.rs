//! Owner-only workspace metadata and durable naming after setup.
use axum::{
    Extension, Json, Router,
    extract::State,
    routing::{get, put},
};
use pitcrew_hub_work::{WorkError, WorkService};
use pitcrew_protocol::{api::Caller, model::Workspace};
use serde::Deserialize;
use std::{path::PathBuf, sync::Arc};

#[derive(Clone)]
struct Settings {
    work: Arc<WorkService>,
    root: PathBuf,
}

pub fn routes(work: Arc<WorkService>, root: &std::path::Path) -> Router {
    let settings = Settings {
        work,
        root: root.to_path_buf(),
    };
    // Persistence uses startup configuration, never a path extracted from a request.
    let rename_settings = settings.clone();
    Router::new()
        .route("/v1/settings", get(info))
        .route(
            "/v1/settings/workspace",
            put(move |caller, body| rename(rename_settings.clone(), caller, body)),
        )
        .with_state(settings)
}

async fn info(
    State(settings): State<Settings>,
    Extension(caller): Extension<Caller>,
) -> Result<Json<serde_json::Value>, WorkError> {
    tokio::task::spawn_blocking(move || {
        settings.work.require_owner(&caller)?;
        Ok(Json(serde_json::json!({ "owner": caller.member, "data_folder": settings.root.to_string_lossy(), "logs": "Daemon logs go to stderr; the launcher captures them.", "daemon_version": env!("CARGO_PKG_VERSION"), "protocol_version": pitcrew_protocol::PROTOCOL_VERSION })))
    }).await.map_err(|_| WorkError::unavailable("Settings metadata is unavailable."))?
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Rename {
    name: String,
}

async fn rename(
    settings: Settings,
    Extension(caller): Extension<Caller>,
    body: Result<Json<Rename>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<Workspace>, WorkError> {
    let work = Arc::clone(&settings.work);
    tokio::task::spawn_blocking(move || work.require_owner(&caller))
        .await
        .map_err(|_| WorkError::unavailable("Workspace settings are unavailable."))??;
    let Json(body) = body.map_err(|_| WorkError::invalid("Supply a workspace name only."))?;
    tokio::task::spawn_blocking(move || {
        settings
            .work
            .rename_workspace(&caller, &body.name, |workspace| {
                crate::state::write_workspace(&settings.root.join("workspace.json"), workspace)
                    .map_err(|_| {
                        WorkError::unavailable("Could not save the workspace name. Retry.")
                    })
            })
            .map(Json)
    })
    .await
    .map_err(|_| WorkError::unavailable("Workspace settings are unavailable."))?
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::{Body, to_bytes},
        http::Request,
    };
    use pitcrew_protocol::api::TokenScope;
    use tower::ServiceExt as _;

    #[tokio::test]
    async fn owner_metadata_and_workspace_rename_persist_without_restart() {
        let temp = tempfile::tempdir().unwrap();
        let demo = pitcrew_fixtures::demo_workspace().unwrap();
        let store = Arc::new(
            pitcrew_store::Store::open_with(
                temp.path().join("hub.db"),
                Default::default(),
                pitcrew_hub_work::projections(),
            )
            .unwrap(),
        );
        let work = Arc::new(WorkService::new(store, demo.workspace.clone()));
        work.seed(&demo).unwrap();
        let caller = Caller {
            member: demo
                .members
                .iter()
                .find(|m| m.kind == pitcrew_protocol::model::MemberKind::Human)
                .unwrap()
                .id,
            scope: TokenScope::Device,
            on_behalf_of: None,
        };
        let app = routes(Arc::clone(&work), temp.path()).layer(Extension(caller));
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/v1/settings")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        let info: serde_json::Value =
            serde_json::from_slice(&to_bytes(response.into_body(), 1024 * 1024).await.unwrap())
                .unwrap();
        assert_eq!(info["owner"], serde_json::json!(caller.member));
        let response = app
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/v1/settings/workspace")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"name":"../Updated workspace"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        assert_eq!(
            work.workspace_at().unwrap().workspace.name,
            "../Updated workspace"
        );
        assert_eq!(
            crate::state::read_workspace(&temp.path().join("workspace.json"))
                .unwrap()
                .unwrap()
                .name,
            "../Updated workspace"
        );
        let agent = Caller {
            scope: TokenScope::Agent,
            ..caller
        };
        let response = routes(work, temp.path())
            .layer(Extension(agent))
            .oneshot(
                Request::builder()
                    .uri("/v1/settings")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), 403);
    }
}
