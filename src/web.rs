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
use subtle::ConstantTimeEq;

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
    let expected = format!("Bearer {}", cfg.shared_key);
    headers
        .get("authorization")
        .map(|actual| actual.as_bytes().ct_eq(expected.as_bytes()).into())
        .unwrap_or(false)
}

async fn index() -> Html<&'static str> {
    Html(include_str!("web.html"))
}

async fn status(State(cfg): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    if !authorized(&headers, &cfg) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    status_response(&cfg)
}

fn status_response(cfg: &Config) -> axum::response::Response {
    match sync::core::manifest(cfg) {
        Ok(m) => axum::Json(m).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

async fn peers(State(cfg): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    if !authorized(&headers, &cfg) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    match discovery::discover(&cfg, 2).await {
        Ok(p) => axum::Json(p).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

async fn manifest(State(cfg): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    if !authorized(&headers, &cfg) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    status_response(&cfg)
}

async fn file(
    State(cfg): State<AppState>,
    headers: HeaderMap,
    Path(path): Path<String>,
) -> impl IntoResponse {
    if !authorized(&headers, &cfg) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    match read_served_file(&cfg, &path) {
        Ok(bytes) => Body::from(bytes).into_response(),
        Err(_) => StatusCode::NOT_FOUND.into_response(),
    }
}

fn read_served_file(cfg: &Config, path: &str) -> Result<Vec<u8>> {
    sync::core::read_local_file(cfg, path)
}

#[derive(serde::Deserialize)]
struct PullRequest {
    peer: String,
    overwrite: Option<bool>,
}
async fn pull(
    State(cfg): State<AppState>,
    headers: HeaderMap,
    axum::Json(req): axum::Json<PullRequest>,
) -> impl IntoResponse {
    if !authorized(&headers, &cfg) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    match sync::http::pull(&cfg, &req.peer, req.overwrite.unwrap_or(false)).await {
        Ok(r) => axum::Json(r).into_response(),
        Err(e) => (StatusCode::BAD_GATEWAY, e.to_string()).into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::{HeaderValue, header::AUTHORIZATION};

    #[test]
    fn every_request_needs_the_exact_shared_key() {
        let cfg = Config {
            shared_key: "a very long test key that exceeds thirty two chars".into(),
            ..Config::default()
        };
        let mut valid = HeaderMap::new();
        valid.insert(
            AUTHORIZATION,
            HeaderValue::from_static("Bearer a very long test key that exceeds thirty two chars"),
        );
        let mut invalid = HeaderMap::new();
        invalid.insert(AUTHORIZATION, HeaderValue::from_static("Bearer wrong"));
        assert!(authorized(&valid, &cfg));
        assert!(!authorized(&invalid, &cfg));
        assert!(!authorized(&HeaderMap::new(), &cfg));
    }
}
