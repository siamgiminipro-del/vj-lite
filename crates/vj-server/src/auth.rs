use crate::db::{hash_password, now_unix, verify_password};
use axum::http::HeaderMap;
use rusqlite::{params, Connection};
use serde::Serialize;
use uuid::Uuid;

const SESSION_TTL_SECS: i64 = 7 * 24 * 3600;

#[derive(Debug, Clone, Serialize)]
pub struct OjHandleInfo {
    pub platform: String,
    pub handle: String,
    pub verified: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct AuthUser {
    pub id: i64,
    pub email: String,
    pub display_name: String,
    pub role: String,
    pub oj_handles: Vec<OjHandleInfo>,
}

pub fn max_users() -> i64 {
    std::env::var("MAX_USERS")
        .ok()
        .and_then(|v| v.parse::<i64>().ok())
        .filter(|&n| n > 0)
        .unwrap_or(50)
}

pub fn create_session(conn: &Connection, user_id: i64) -> Result<String, rusqlite::Error> {
    let token = Uuid::new_v4().simple().to_string();
    let expires_at = now_unix() + SESSION_TTL_SECS;
    conn.execute(
        "INSERT INTO sessions (token, user_id, expires_at) VALUES (?, ?, ?)",
        params![token, user_id, expires_at],
    )?;
    Ok(token)
}

pub fn get_user_by_id(conn: &Connection, user_id: i64) -> Option<AuthUser> {
    let row = conn
        .query_row(
            "SELECT id, email, display_name, role FROM users WHERE id = ?",
            params![user_id],
            |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                ))
            },
        )
        .ok()?;

    let mut stmt = conn
        .prepare("SELECT platform, handle, verified FROM oj_handles WHERE user_id = ? ORDER BY platform ASC")
        .ok()?;
    let handles = stmt
        .query_map(params![user_id], |r| {
            Ok(OjHandleInfo {
                platform: r.get(0)?,
                handle: r.get(1)?,
                verified: r.get::<_, i64>(2)? != 0,
            })
        })
        .ok()?
        .filter_map(Result::ok)
        .collect();

    Some(AuthUser {
        id: row.0,
        email: row.1,
        display_name: row.2,
        role: row.3,
        oj_handles: handles,
    })
}

pub fn extract_token(headers: &HeaderMap) -> Option<String> {
    if let Some(auth) = headers.get("authorization").and_then(|v| v.to_str().ok()) {
        if let Some(tok) = auth.strip_prefix("Bearer ") {
            return Some(tok.trim().to_string());
        }
    }
    if let Some(cookie) = headers.get("cookie").and_then(|v| v.to_str().ok()) {
        for part in cookie.split(';') {
            if let Some((k, v)) = part.trim().split_once('=') {
                if k == "vj_session" && !v.is_empty() {
                    return Some(v.to_string());
                }
            }
        }
    }
    None
}

pub fn session_user(conn: &Connection, headers: &HeaderMap) -> Option<AuthUser> {
    let token = extract_token(headers)?;
    let now = now_unix();
    let row: (i64, i64) = conn
        .query_row(
            "SELECT user_id, expires_at FROM sessions WHERE token = ?",
            params![token],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .ok()?;

    if row.1 < now {
        let _ = conn.execute("DELETE FROM sessions WHERE token = ?", params![token]);
        return None;
    }
    get_user_by_id(conn, row.0)
}

pub fn register_email_user(
    conn: &Connection,
    email: &str,
    display_name: &str,
    password: &str,
    max_cap: i64,
) -> Result<(AuthUser, String), (u16, String)> {
    let clean_email = email.trim().to_lowercase();
    let clean_name = display_name.trim();
    if !clean_email.contains('@') || clean_name.len() < 2 || password.len() < 4 {
        return Err((400, "Provide a valid email, display name (2+ chars), and password (4+ chars).".into()));
    }

    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM users", [], |r| r.get(0))
        .map_err(|e| (500, e.to_string()))?;
    if count >= max_cap {
        return Err((
            403,
            format!("Platform user limit reached ({max_cap} max). Contact the coordinator."),
        ));
    }

    let exists: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM users WHERE lower(email) = lower(?)",
            params![clean_email],
            |r| r.get(0),
        )
        .map_err(|e| (500, e.to_string()))?;
    if exists > 0 {
        return Err((409, "An account with this email already exists.".into()));
    }

    let now = now_unix();
    let role = if count == 0 { "coordinator" } else { "participant" };
    conn.execute(
        "INSERT INTO users (email, display_name, password_hash, role, created_at) VALUES (?, ?, ?, ?, ?)",
        params![clean_email, clean_name, hash_password(password), role, now],
    )
    .map_err(|e| (500, e.to_string()))?;
    let user_id = conn.last_insert_rowid();
    let token = create_session(conn, user_id).map_err(|e| (500, e.to_string()))?;
    let user = get_user_by_id(conn, user_id).ok_or((500, "Failed to load created user".into()))?;
    Ok((user, token))
}

pub fn login_email_user(
    conn: &Connection,
    email: &str,
    password: &str,
) -> Result<(AuthUser, String), (u16, String)> {
    let clean_email = email.trim().to_lowercase();
    let row = conn
        .query_row(
            "SELECT id, password_hash FROM users WHERE lower(email) = lower(?)",
            params![clean_email],
            |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)),
        )
        .map_err(|_| (401, "Invalid email or password.".to_string()))?;

    if !verify_password(password, &row.1) {
        return Err((401, "Invalid email or password.".into()));
    }

    let token = create_session(conn, row.0).map_err(|e| (500, e.to_string()))?;
    let user = get_user_by_id(conn, row.0).ok_or((500, "Failed to load user".into()))?;
    Ok((user, token))
}

pub async fn authenticate_google_user(
    conn: &Connection,
    http: &reqwest::Client,
    id_token: Option<&str>,
    fallback_email: Option<&str>,
    fallback_name: Option<&str>,
    max_cap: i64,
) -> Result<(AuthUser, String), (u16, String)> {
    let (email, google_sub, display_name) = if let Some(tok) = id_token.filter(|s| !s.trim().is_empty()) {
        // Verify Google ID token via Google's tokeninfo endpoint
        let url = format!("https://oauth2.googleapis.com/tokeninfo?id_token={tok}");
        let res = http
            .get(&url)
            .send()
            .await
            .map_err(|e| (502, format!("Google verification failed: {e}")))?;
        if !res.status().is_success() {
            return Err((401, "Invalid Google ID token.".into()));
        }
        let info: serde_json::Value = res
            .json()
            .await
            .map_err(|e| (502, format!("Invalid Google response: {e}")))?;
        let email = info["email"]
            .as_str()
            .ok_or((400, "Google token missing email".to_string()))?
            .to_lowercase();
        let sub = info["sub"]
            .as_str()
            .ok_or((400, "Google token missing sub".to_string()))?
            .to_string();
        let name = info["name"]
            .as_str()
            .unwrap_or_else(|| email.split('@').next().unwrap_or("user"))
            .to_string();
        (email, sub, name)
    } else {
        // Local / Demo Google Sign-In when GOOGLE_CLIENT_ID is not yet configured
        let email = fallback_email
            .map(|s| s.trim().to_lowercase())
            .filter(|s| s.contains('@'))
            .ok_or((400, "Provide a valid Google email or ID token.".to_string()))?;
        let name = fallback_name
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| email.split('@').next().unwrap_or("user").to_string());
        let sub = format!("local-google:{email}");
        (email, sub, name)
    };

    let existing_id: Option<i64> = conn
        .query_row(
            "SELECT id FROM users WHERE google_sub = ? OR lower(email) = lower(?)",
            params![google_sub, email],
            |r| r.get(0),
        )
        .ok();

    let user_id = if let Some(uid) = existing_id {
        let _ = conn.execute(
            "UPDATE users SET google_sub = COALESCE(google_sub, ?) WHERE id = ?",
            params![google_sub, uid],
        );
        uid
    } else {
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM users", [], |r| r.get(0))
            .map_err(|e| (500, e.to_string()))?;
        if count >= max_cap {
            return Err((
                403,
                format!("Platform user limit reached ({max_cap} max). Contact the coordinator."),
            ));
        }
        let now = now_unix();
        let coordinator_allowlist = std::env::var("COORDINATOR_EMAILS").unwrap_or_default();
        let is_coord = coordinator_allowlist
            .split(',')
            .any(|e| e.trim().eq_ignore_ascii_case(&email));
        let role = if is_coord { "coordinator" } else { "participant" };

        conn.execute(
            "INSERT INTO users (email, google_sub, display_name, password_hash, role, created_at) VALUES (?, ?, ?, '', ?, ?)",
            params![email, google_sub, display_name, role, now],
        )
        .map_err(|e| (500, e.to_string()))?;
        conn.last_insert_rowid()
    };

    let token = create_session(conn, user_id).map_err(|e| (500, e.to_string()))?;
    let user = get_user_by_id(conn, user_id).ok_or((500, "Failed to load user".into()))?;
    Ok((user, token))
}
