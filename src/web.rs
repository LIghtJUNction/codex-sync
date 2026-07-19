use crate::{config::Config, discovery, sync};
use anyhow::Result;
use axum::{
    Router,
    body::Body,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{Html, IntoResponse},
    routing::{get, post},
};
use std::{net::SocketAddr, sync::Arc};

type AppState = Arc<Config>;

pub async fn serve(cfg: Config) -> Result<()> {
    let addr: SocketAddr = cfg.listen.parse()?;
    let discovery_cfg = cfg.clone();
    tokio::spawn(async move {
        if let Err(e) = discovery::responder(discovery_cfg).await {
            eprintln!("发现服务停止：{e}");
        }
    });
    let state = Arc::new(cfg);
    let app = Router::new()
        .route("/", get(index))
        .route("/api/status", get(status))
        .route("/api/peers", get(peers))
        .route("/api/manifest", get(manifest))
        .route("/api/file/{*path}", get(file))
        .route("/api/pull", post(pull))
        .with_state(state);
    println!("Codex Sync 已启动：http://{addr}");
    axum::serve(tokio::net::TcpListener::bind(addr).await?, app).await?;
    Ok(())
}

fn authorized(headers: &HeaderMap, cfg: &Config) -> bool {
    headers.get("authorization").and_then(|h| h.to_str().ok())
        == Some(&format!("Bearer {}", cfg.shared_key))
}

async fn index() -> Html<&'static str> {
    Html(include_str!("web.html"))
}

async fn status(State(cfg): State<AppState>) -> impl IntoResponse {
    match sync::manifest(&cfg) {
        Ok(m) => axum::Json(m).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

async fn peers(State(cfg): State<AppState>) -> impl IntoResponse {
    match discovery::discover(&cfg, 2).await {
        Ok(p) => axum::Json(p).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

async fn manifest(State(cfg): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    if !authorized(&headers, &cfg) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    status(State(cfg)).await.into_response()
}

async fn file(
    State(cfg): State<AppState>,
    headers: HeaderMap,
    Path(path): Path<String>,
) -> impl IntoResponse {
    if !authorized(&headers, &cfg) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    match sync::safe_path(&cfg.sync_dir, &path).and_then(|p| std::fs::read(p).map_err(Into::into)) {
        Ok(bytes) => Body::from(bytes).into_response(),
        Err(_) => StatusCode::NOT_FOUND.into_response(),
    }
}

#[derive(serde::Deserialize)]
struct PullRequest {
    peer: String,
    overwrite: Option<bool>,
}
async fn pull(
    State(cfg): State<AppState>,
    axum::Json(req): axum::Json<PullRequest>,
) -> impl IntoResponse {
    match sync::pull(&cfg, &req.peer, req.overwrite.unwrap_or(false)).await {
        Ok(r) => axum::Json(r).into_response(),
        Err(e) => (StatusCode::BAD_GATEWAY, e.to_string()).into_response(),
    }
}
