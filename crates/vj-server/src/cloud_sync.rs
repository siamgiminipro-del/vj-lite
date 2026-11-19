use rusqlite::Connection;
use std::path::Path;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

#[derive(Clone)]
pub struct CloudSyncConfig {
    pub supabase_url: String,
    pub service_key: String,
    pub bucket: String,
    pub object_name: String,
}

impl CloudSyncConfig {
    pub fn from_env() -> Option<Self> {
        let supabase_url = std::env::var("SUPABASE_URL").ok()?.trim().trim_end_matches('/').to_string();
        let service_key = std::env::var("SUPABASE_SERVICE_ROLE_KEY")
            .or_else(|_| std::env::var("SUPABASE_SECRET_KEY"))
            .ok()?
            .trim()
            .to_string();
        if supabase_url.is_empty() || service_key.is_empty() {
            return None;
        }
        let bucket = std::env::var("SUPABASE_DB_BUCKET")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| "vj-lite-db".to_string());
        Some(Self {
            supabase_url,
            service_key,
            bucket,
            object_name: "vj.db".to_string(),
        })
    }

    fn object_url(&self) -> String {
        format!(
            "{}/storage/v1/object/{}/{}",
            self.supabase_url, self.bucket, self.object_name
        )
    }

    fn create_bucket_url(&self) -> String {
        format!("{}/storage/v1/bucket", self.supabase_url)
    }
}

async fn ensure_bucket_exists(http: &reqwest::Client, cfg: &CloudSyncConfig) {
    let body = serde_json::json!({
        "id": cfg.bucket,
        "name": cfg.bucket,
        "public": false
    });
    match http
        .post(cfg.create_bucket_url())
        .header("apikey", &cfg.service_key)
        .bearer_auth(&cfg.service_key)
        .json(&body)
        .send()
        .await
    {
        Ok(r) if r.status().is_success() => {
            println!("[cloud-sync] Created private Supabase Storage bucket '{}'", cfg.bucket);
        }
        Ok(r) if r.status().as_u16() == 409 => {}
        Ok(r) => {
            let txt = r.text().await.unwrap_or_default();
            if !txt.to_lowercase().contains("already exists") {
                eprintln!("[cloud-sync] Bucket check note: {txt}");
            }
        }
        Err(e) => eprintln!("[cloud-sync] Bucket check failed: {e}"),
    }
}

/// Downloads the persisted SQLite DB from Supabase Storage on startup (before opening SQLite).
/// Returns true if an existing database snapshot was restored.
pub async fn restore_db_from_cloud(db_path: &str) -> bool {
    if db_path == ":memory:" {
        return false;
    }
    let Some(cfg) = CloudSyncConfig::from_env() else {
        return false;
    };

    let http = reqwest::Client::new();
    match http
        .get(cfg.object_url())
        .header("apikey", &cfg.service_key)
        .bearer_auth(&cfg.service_key)
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => {
            if let Ok(bytes) = resp.bytes().await {
                if bytes.len() >= 16 && &bytes[..16] == b"SQLite format 3\0" {
                    if let Some(parent) = Path::new(db_path).parent() {
                        let _ = std::fs::create_dir_all(parent);
                    }
                    let _ = std::fs::remove_file(format!("{db_path}-wal"));
                    let _ = std::fs::remove_file(format!("{db_path}-shm"));
                    if let Err(e) = std::fs::write(db_path, &bytes) {
                        eprintln!("[cloud-sync] Failed to write restored DB to {db_path}: {e}");
                        return false;
                    }
                    println!(
                        "[cloud-sync] Restored SQLite database from Supabase Storage ({} bytes)",
                        bytes.len()
                    );
                    return true;
                }
            }
            false
        }
        Ok(resp) => {
            println!(
                "[cloud-sync] No existing cloud DB snapshot (HTTP {}). Ensuring bucket '{}'...",
                resp.status(),
                cfg.bucket
            );
            ensure_bucket_exists(&http, &cfg).await;
            false
        }
        Err(e) => {
            eprintln!("[cloud-sync] Failed to reach Supabase Storage on startup: {e}");
            false
        }
    }
}

async fn upload_snapshot(
    http: &reqwest::Client,
    cfg: &CloudSyncConfig,
    db: &Arc<Mutex<Connection>>,
) {
    let tmp_path = std::env::temp_dir().join(format!("vj_snapshot_{}.db", uuid::Uuid::new_v4().simple()));
    let tmp_str = tmp_path.to_string_lossy().replace('\'', "''");

    let vacuum_res = {
        let conn = db.lock().unwrap();
        conn.execute_batch(&format!("VACUUM INTO '{tmp_str}';"))
    };

    if let Err(e) = vacuum_res {
        eprintln!("[cloud-sync] SQLite snapshot VACUUM INTO failed: {e}");
        let _ = std::fs::remove_file(&tmp_path);
        return;
    }

    let bytes = match std::fs::read(&tmp_path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("[cloud-sync] Failed to read snapshot file: {e}");
            let _ = std::fs::remove_file(&tmp_path);
            return;
        }
    };
    let _ = std::fs::remove_file(&tmp_path);

    let res = http
        .post(cfg.object_url())
        .header("apikey", &cfg.service_key)
        .bearer_auth(&cfg.service_key)
        .header("x-upsert", "true")
        .header("Content-Type", "application/octet-stream")
        .body(bytes.clone())
        .send()
        .await;

    match res {
        Ok(r) if r.status().is_success() => {
            println!("[cloud-sync] Synced SQLite snapshot to Supabase Storage ({} bytes)", bytes.len());
        }
        Ok(r) => {
            let status = r.status();
            let body = r.text().await.unwrap_or_default();
            if body.to_lowercase().contains("bucket not found") || status.as_u16() == 404 {
                ensure_bucket_exists(http, cfg).await;
                let _ = http
                    .post(cfg.object_url())
                    .header("apikey", &cfg.service_key)
                    .bearer_auth(&cfg.service_key)
                    .header("x-upsert", "true")
                    .header("Content-Type", "application/octet-stream")
                    .body(bytes)
                    .send()
                    .await;
            } else {
                eprintln!("[cloud-sync] Snapshot upload failed (HTTP {status}): {body}");
            }
        }
        Err(e) => eprintln!("[cloud-sync] Snapshot upload error: {e}"),
    }
}

/// Spawns a debounced background worker that syncs SQLite snapshots to Supabase Storage on DB writes.
pub fn spawn_db_backup_worker(
    db: Arc<Mutex<Connection>>,
    db_path: &str,
) -> Option<mpsc::UnboundedSender<()>> {
    if db_path == ":memory:" {
        return None;
    }
    let cfg = CloudSyncConfig::from_env()?;
    let (tx, mut rx) = mpsc::unbounded_channel::<()>();
    let http = reqwest::Client::new();

    tokio::spawn(async move {
        while rx.recv().await.is_some() {
            // Debounce rapid consecutive writes
            tokio::time::sleep(std::time::Duration::from_millis(600)).await;
            while rx.try_recv().is_ok() {}
            upload_snapshot(&http, &cfg, &db).await;
        }
    });

    Some(tx)
}
