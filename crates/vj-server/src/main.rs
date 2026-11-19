use std::path::PathBuf;
use vj_server::build_app;

#[tokio::main]
async fn main() {
    let port = std::env::var("PORT").unwrap_or_else(|_| "3000".into());
    let host = std::env::var("HOST").unwrap_or_else(|_| "0.0.0.0".into());
    let db_path = std::env::var("DB_PATH").unwrap_or_else(|_| "data/vj.db".into());
    let ui_dir = std::env::var("UI_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("ui"));

    let restored = vj_server::cloud_sync::restore_db_from_cloud(&db_path).await;
    let (app, state) = build_app(&db_path, ui_dir, None, None).expect("Failed to initialize database");
    if !restored {
        state.schedule_cloud_backup();
    }
    let addr = format!("{host}:{port}");
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .expect("Failed to bind port");

    println!("VJ-Lite Rust Server running at http://localhost:{port}");
    println!("Demo Coordinator: admin@vjclub.local / admin123");
    println!("Demo Participants: alice@gmail.com / alice123, bob@gmail.com / bob123");
    println!("Seeded Contest Access Code: CLUB-2026-W1");

    axum::serve(listener, app).await.expect("Server error");
}
