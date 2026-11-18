pub mod auth;
pub mod db;
pub mod local_judge;
pub mod oj_sync;
pub mod routes;
pub mod scoreboard;

use axum::Router;
use routes::{api_router, AppState};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tower_http::cors::CorsLayer;
use tower_http::services::{ServeDir, ServeFile};

pub fn build_app(
    db_path: &str,
    ui_dir: PathBuf,
    cf_api_base: Option<String>,
    atcoder_api_base: Option<String>,
) -> Result<(Router, AppState), rusqlite::Error> {
    let conn = db::open_database(db_path)?;
    let state = AppState {
        db: Arc::new(Mutex::new(conn)),
        http: reqwest::Client::new(),
        cf_api_base: cf_api_base.unwrap_or_else(|| "https://codeforces.com/api".into()),
        atcoder_api_base: atcoder_api_base.unwrap_or_else(|| "https://kenkoooo.com/atcoder".into()),
        sse_channels: Arc::new(Mutex::new(HashMap::new())),
    };

    let index_file = ui_dir.join("index.html");
    let serve_ui = ServeDir::new(ui_dir).not_found_service(ServeFile::new(index_file));

    let router = api_router(state.clone())
        .fallback_service(serve_ui)
        .layer(CorsLayer::permissive());

    Ok((router, state))
}
