use crate::auth::{
    authenticate_google_user, extract_token, get_user_by_id, login_email_user, max_users,
    register_email_user, session_user, AuthUser,
};
use crate::db::now_unix;
use crate::local_judge::{judge_local, TestCase};
use crate::oj_sync::{fetch_oj_problem_details, parse_oj_problem};
use crate::scoreboard::{
    compute_standings, ContestMeta, ScoreboardProblem, SubmissionForScore,
};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, patch, post};
use axum::{Json, Router};
use rusqlite::{params, Connection};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::{Arc, Mutex};
use tokio::sync::broadcast;
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::StreamExt;
use uuid::Uuid;

#[derive(Clone)]
pub struct AppState {
    pub db: Arc<Mutex<Connection>>,
    pub http: reqwest::Client,
    pub cf_api_base: String,
    pub atcoder_api_base: String,
    pub sse_channels: Arc<Mutex<HashMap<i64, broadcast::Sender<String>>>>,
}

impl AppState {
    pub fn broadcast_contest(&self, contest_id: i64, msg: &str) {
        let sender = {
            let map = self.sse_channels.lock().unwrap();
            map.get(&contest_id).cloned()
        };
        if let Some(tx) = sender {
            let _ = tx.send(msg.to_string());
        }
    }

    pub fn get_or_create_channel(&self, contest_id: i64) -> broadcast::Sender<String> {
        let mut map = self.sse_channels.lock().unwrap();
        map.entry(contest_id)
            .or_insert_with(|| broadcast::channel(64).0)
            .clone()
    }
}

fn err_resp(status: u16, msg: impl Into<String>) -> Response {
    let code = StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    (code, Json(json!({ "error": msg.into() }))).into_response()
}

fn session_cookie_header(token: &str) -> HeaderMap {
    let mut headers = HeaderMap::new();
    let val = format!("vj_session={token}; Path=/; HttpOnly; SameSite=Lax; Max-Age=604800");
    if let Ok(hv) = HeaderValue::from_str(&val) {
        headers.insert("set-cookie", hv);
    }
    headers
}

fn require_user(state: &AppState, headers: &HeaderMap) -> Result<AuthUser, Response> {
    let conn = state.db.lock().unwrap();
    session_user(&conn, headers).ok_or_else(|| err_resp(401, "Login required."))
}

fn is_coordinator(user: &AuthUser) -> bool {
    user.role == "coordinator" || user.role == "admin"
}

pub fn api_router(state: AppState) -> Router {
    Router::new()
        .route("/api/health", get(health))
        .route("/api/auth/config", get(auth_config))
        .route("/api/auth/register", post(register))
        .route("/api/auth/login", post(login))
        .route("/api/auth/google", post(google_auth))
        .route("/api/auth/supabase", post(supabase_auth))
        .route("/api/auth/logout", post(logout))
        .route("/api/auth/me", get(me))
        .route("/api/oj/link", post(link_oj))
        .route("/api/oj/unlink", post(unlink_oj))
        .route("/api/problems/fetch", post(fetch_problem_endpoint))
        .route("/api/problems", get(list_problems).post(create_problem))
        .route("/api/contests", get(list_contests).post(create_contest))
        .route("/api/contests/join", post(join_contest_by_code))
        .route("/api/contests/:id", get(get_contest))
        .route("/api/contests/:id/prepare-external-submit", post(prepare_external_submit))
        .route("/api/contests/:id/sync", post(sync_contest))
        .route("/api/contests/:id/sync-all", post(sync_all_contest))
        .route("/api/contests/:id/submit", post(submit_local))
        .route("/api/contests/:id/submissions", get(list_submissions))
        .route("/api/contests/:id/standings", get(get_standings))
        .route("/api/contests/:id/clarifications", post(post_clarification))
        .route("/api/contests/:id/clarifications/:clar_id", patch(answer_clarification))
        .route("/api/contests/:id/events", get(contest_events))
        .with_state(state)
}

async fn auth_config() -> Response {
    let supabase_url = std::env::var("SUPABASE_URL").unwrap_or_default();
    let supabase_anon_key = std::env::var("SUPABASE_ANON_KEY").unwrap_or_default();
    let enabled = !supabase_url.trim().is_empty() && !supabase_anon_key.trim().is_empty();
    Json(json!({
        "supabaseEnabled": enabled,
        "supabaseUrl": if enabled { Some(supabase_url) } else { None },
        "supabaseAnonKey": if enabled { Some(supabase_anon_key) } else { None }
    }))
    .into_response()
}

#[derive(Deserialize)]
struct SupabaseAuthReq {
    #[serde(rename = "accessToken")]
    access_token: String,
}

async fn supabase_auth(State(state): State<AppState>, Json(body): Json<SupabaseAuthReq>) -> Response {
    let supabase_url = std::env::var("SUPABASE_URL").unwrap_or_default();
    let supabase_anon_key = std::env::var("SUPABASE_ANON_KEY").unwrap_or_default();
    if supabase_url.trim().is_empty() || supabase_anon_key.trim().is_empty() {
        return err_resp(400, "SUPABASE_URL and SUPABASE_ANON_KEY are not configured on the server.");
    }

    let url = format!("{}/auth/v1/user", supabase_url.trim_end_matches('/'));
    let resp = match state
        .http
        .get(&url)
        .header("apikey", supabase_anon_key.trim())
        .bearer_auth(body.access_token.trim())
        .send()
        .await
    {
        Ok(r) if r.status().is_success() => match r.json::<Value>().await {
            Ok(v) => v,
            Err(_) => return err_resp(502, "Invalid response from Supabase Auth."),
        },
        Ok(r) => return err_resp(401, format!("Supabase rejected token (HTTP {}).", r.status())),
        Err(e) => return err_resp(502, format!("Could not reach Supabase Auth: {e}")),
    };

    let email = resp["email"].as_str().unwrap_or("").trim().to_lowercase();
    if !email.contains('@') {
        return err_resp(400, "Supabase user has no verified email.");
    }
    let name = resp["user_metadata"]["full_name"]
        .as_str()
        .or_else(|| resp["user_metadata"]["name"].as_str())
        .unwrap_or_else(|| email.split('@').next().unwrap_or("user"));

    let conn = state.db.lock().unwrap();
    match futures_free_google_upsert(&conn, &email, name, Some("supabase-verified")) {
        Ok((user, token)) => (
            StatusCode::OK,
            session_cookie_header(&token),
            Json(json!({ "user": user, "token": token })),
        )
            .into_response(),
        Err((s, m)) => err_resp(s, m),
    }
}

async fn health(State(state): State<AppState>) -> Response {
    let conn = state.db.lock().unwrap();
    let user_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM users", [], |r| r.get(0))
        .unwrap_or(0);
    let contest_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM contests", [], |r| r.get(0))
        .unwrap_or(0);
    Json(json!({
        "ok": true,
        "engine": "rust-axum",
        "userCount": user_count,
        "maxUsers": max_users(),
        "contestCount": contest_count
    }))
    .into_response()
}

#[derive(Deserialize)]
struct RegisterReq {
    email: String,
    #[serde(rename = "displayName")]
    display_name: String,
    password: String,
}

async fn register(State(state): State<AppState>, Json(body): Json<RegisterReq>) -> Response {
    let conn = state.db.lock().unwrap();
    match register_email_user(&conn, &body.email, &body.display_name, &body.password, max_users()) {
        Ok((user, token)) => (
            StatusCode::CREATED,
            session_cookie_header(&token),
            Json(json!({ "user": user, "token": token })),
        )
            .into_response(),
        Err((s, m)) => err_resp(s, m),
    }
}

#[derive(Deserialize)]
struct LoginReq {
    email: String,
    password: String,
}

async fn login(State(state): State<AppState>, Json(body): Json<LoginReq>) -> Response {
    let conn = state.db.lock().unwrap();
    match login_email_user(&conn, &body.email, &body.password) {
        Ok((user, token)) => (
            StatusCode::OK,
            session_cookie_header(&token),
            Json(json!({ "user": user, "token": token })),
        )
            .into_response(),
        Err((s, m)) => err_resp(s, m),
    }
}

#[derive(Deserialize)]
struct GoogleAuthReq {
    #[serde(rename = "idToken")]
    id_token: Option<String>,
    email: Option<String>,
    #[serde(rename = "displayName")]
    display_name: Option<String>,
}

async fn google_auth(State(state): State<AppState>, Json(body): Json<GoogleAuthReq>) -> Response {
    // If idToken is provided, verify via Google before acquiring DB lock
    let verified_from_google = if let Some(tok) = body.id_token.as_deref().filter(|s| !s.trim().is_empty()) {
        let url = format!("https://oauth2.googleapis.com/tokeninfo?id_token={tok}");
        match state.http.get(&url).send().await {
            Ok(r) if r.status().is_success() => match r.json::<Value>().await {
                Ok(info) => Some((
                    info["email"].as_str().unwrap_or("").to_lowercase(),
                    info["sub"].as_str().unwrap_or("").to_string(),
                    info["name"].as_str().unwrap_or("").to_string(),
                )),
                Err(_) => return err_resp(401, "Invalid Google response."),
            },
            _ => return err_resp(401, "Invalid Google ID token."),
        }
    } else {
        None
    };

    let conn = state.db.lock().unwrap();
    let result = if let Some((email, _sub, name)) = verified_from_google {
        // Use verified email & name
        futures_free_google_upsert(&conn, &email, &name, body.id_token.as_deref())
    } else {
        // Synchronous demo/local Google flow (no await needed)
        let _ = authenticate_google_user; // kept for library callers
        let email = body
            .email
            .as_deref()
            .map(|s| s.trim().to_lowercase())
            .filter(|s| s.contains('@'))
            .ok_or((400u16, "Provide a valid Google email.".to_string()));
        match email {
            Ok(em) => {
                let nm = body
                    .display_name
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| em.split('@').next().unwrap_or("user"));
                futures_free_google_upsert(&conn, &em, nm, None)
            }
            Err(e) => Err(e),
        }
    };

    match result {
        Ok((user, token)) => (
            StatusCode::OK,
            session_cookie_header(&token),
            Json(json!({ "user": user, "token": token })),
        )
            .into_response(),
        Err((s, m)) => err_resp(s, m),
    }
}

fn futures_free_google_upsert(
    conn: &Connection,
    email: &str,
    display_name: &str,
    id_token: Option<&str>,
) -> Result<(AuthUser, String), (u16, String)> {
    let sub = if id_token.is_some() {
        format!("google:{email}")
    } else {
        format!("local-google:{email}")
    };

    let existing_id: Option<i64> = conn
        .query_row(
            "SELECT id FROM users WHERE google_sub = ? OR lower(email) = lower(?)",
            params![sub, email],
            |r| r.get(0),
        )
        .ok();

    let user_id = if let Some(uid) = existing_id {
        uid
    } else {
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM users", [], |r| r.get(0))
            .map_err(|e| (500, e.to_string()))?;
        if count >= max_users() {
            return Err((403, format!("Platform user limit reached ({} max).", max_users())));
        }
        let now = now_unix();
        let allowlist = std::env::var("COORDINATOR_EMAILS").unwrap_or_default();
        let is_coord = allowlist
            .split(',')
            .any(|e| e.trim().eq_ignore_ascii_case(email));
        let role = if is_coord { "coordinator" } else { "participant" };
        conn.execute(
            "INSERT INTO users (email, google_sub, display_name, password_hash, role, created_at) VALUES (?, ?, ?, '', ?, ?)",
            params![email, sub, display_name, role, now],
        )
        .map_err(|e| (500, e.to_string()))?;
        conn.last_insert_rowid()
    };

    let token = crate::auth::create_session(conn, user_id).map_err(|e| (500, e.to_string()))?;
    let user = get_user_by_id(conn, user_id).ok_or((500, "Failed to load user".to_string()))?;
    Ok((user, token))
}

async fn logout(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Some(tok) = extract_token(&headers) {
        let conn = state.db.lock().unwrap();
        if let Some(u) = session_user(&conn, &headers) {
            let _ = conn.execute(
                "INSERT OR REPLACE INTO sessions (token, user_id, expires_at) VALUES (?, ?, 0)",
                params![tok, u.id],
            );
        } else {
            let _ = conn.execute("UPDATE sessions SET expires_at = 0 WHERE token = ?", params![tok]);
        }
    }
    let mut h = HeaderMap::new();
    h.insert(
        "set-cookie",
        HeaderValue::from_static("vj_session=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0"),
    );
    (StatusCode::OK, h, Json(json!({ "ok": true }))).into_response()
}

async fn me(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let conn = state.db.lock().unwrap();
    let user = session_user(&conn, &headers);
    let user_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM users", [], |r| r.get(0))
        .unwrap_or(0);
    Json(json!({
        "user": user,
        "userCount": user_count,
        "maxUsers": max_users()
    }))
    .into_response()
}

#[derive(Deserialize)]
struct LinkOjReq {
    platform: String,
    handle: String,
}

async fn link_oj(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<LinkOjReq>,
) -> Response {
    let user = match require_user(&state, &headers) {
        Ok(u) => u,
        Err(r) => return r,
    };
    let platform = body.platform.trim().to_lowercase();
    if !matches!(platform.as_str(), "codeforces" | "atcoder" | "cses" | "codechef") {
        return err_resp(400, "Unsupported platform. Use codeforces, atcoder, cses, or codechef.");
    }
    let handle = body.handle.trim();
    if handle.len() < 2 || handle.len() > 32 {
        return err_resp(400, "Handle must be 2-32 characters.");
    }

    let conn = state.db.lock().unwrap();
    let _ = conn.execute(
        "INSERT INTO oj_handles (user_id, platform, handle, verified, last_sync_at)
         VALUES (?, ?, ?, 1, 0)
         ON CONFLICT(user_id, platform) DO UPDATE SET handle = excluded.handle, verified = 1",
        params![user.id, platform, handle],
    );
    let updated = get_user_by_id(&conn, user.id);
    Json(json!({ "ok": true, "user": updated })).into_response()
}

#[derive(Deserialize)]
struct UnlinkOjReq {
    platform: String,
}

async fn unlink_oj(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<UnlinkOjReq>,
) -> Response {
    let user = match require_user(&state, &headers) {
        Ok(u) => u,
        Err(r) => return r,
    };
    let platform = body.platform.trim().to_lowercase();
    let conn = state.db.lock().unwrap();
    let _ = conn.execute(
        "DELETE FROM oj_handles WHERE user_id = ? AND platform = ?",
        params![user.id, platform],
    );
    let updated = get_user_by_id(&conn, user.id);
    Json(json!({ "ok": true, "user": updated })).into_response()
}

async fn list_problems(State(state): State<AppState>) -> Response {
    let conn = state.db.lock().unwrap();
    let mut stmt = conn
        .prepare(
            "SELECT id, source_type, platform, external_id, title, time_limit_ms, memory_limit_mb, created_at
             FROM problems ORDER BY id DESC",
        )
        .unwrap();
    let probs: Vec<Value> = stmt
        .query_map([], |r| {
            Ok(json!({
                "id": r.get::<_, i64>(0)?,
                "source_type": r.get::<_, String>(1)?,
                "platform": r.get::<_, Option<String>>(2)?,
                "external_id": r.get::<_, Option<String>>(3)?,
                "title": r.get::<_, String>(4)?,
                "time_limit_ms": r.get::<_, i64>(5)?,
                "memory_limit_mb": r.get::<_, i64>(6)?,
                "created_at": r.get::<_, i64>(7)?
            }))
        })
        .unwrap()
        .filter_map(Result::ok)
        .collect();
    Json(json!({ "problems": probs })).into_response()
}

#[derive(Deserialize)]
struct FetchProblemReq {
    platform: String,
    #[serde(rename = "externalId")]
    external_id: String,
}

async fn fetch_problem_endpoint(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<FetchProblemReq>,
) -> Response {
    if let Err(r) = require_user(&state, &headers) {
        return r;
    }
    match fetch_oj_problem_details(&state.http, &body.platform, &body.external_id, &state.cf_api_base).await {
        Ok(details) => Json(json!({ "problem": details })).into_response(),
        Err((s, m)) => err_resp(s, m),
    }
}

#[derive(Deserialize)]
struct CreateProblemReq {
    #[serde(rename = "sourceType")]
    source_type: String,
    platform: Option<String>,
    #[serde(rename = "externalId")]
    external_id: Option<String>,
    title: Option<String>,
    statement: Option<String>,
    #[serde(rename = "sampleTests")]
    sample_tests: Option<Vec<TestCase>>,
    #[serde(rename = "hiddenTests")]
    hidden_tests: Option<Vec<TestCase>>,
    #[serde(rename = "timeLimitMs")]
    time_limit_ms: Option<i64>,
    #[serde(rename = "memoryLimitMb")]
    memory_limit_mb: Option<i64>,
}

async fn create_problem(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<CreateProblemReq>,
) -> Response {
    let user = match require_user(&state, &headers) {
        Ok(u) => u,
        Err(r) => return r,
    };

    let now = now_unix();
    let source_type = if body.source_type == "local" { "local" } else { "external" };
    let mut samples_vec = body.sample_tests.unwrap_or_default();
    let hidden = serde_json::to_string(&body.hidden_tests.unwrap_or_default()).unwrap_or_else(|_| "[]".into());
    let mut tl = body.time_limit_ms.unwrap_or(2000);
    let mut ml = body.memory_limit_mb.unwrap_or(256);
    let mut final_title = body.title.unwrap_or_default().trim().to_string();

    let (platform, ext_id, stmt_text) = if source_type == "external" {
        let plat = body.platform.as_deref().unwrap_or("codeforces");
        let raw = body.external_id.as_deref().unwrap_or("");
        let Some(parsed) = parse_oj_problem(plat, raw) else {
            return err_resp(400, "Invalid OJ problem ID or URL.");
        };

        let mut stmt_val = body.statement.unwrap_or_default().trim().to_string();
        if final_title.is_empty() || stmt_val.is_empty() {
            if let Ok(fetched) = fetch_oj_problem_details(&state.http, &parsed.platform, raw, &state.cf_api_base).await {
                if final_title.is_empty() {
                    final_title = fetched.title;
                }
                if stmt_val.is_empty() {
                    stmt_val = fetched.statement;
                }
                if samples_vec.is_empty() && !fetched.sample_tests.is_empty() {
                    samples_vec = fetched
                        .sample_tests
                        .into_iter()
                        .map(|s| TestCase { input: s.input, output: s.output })
                        .collect();
                }
                tl = fetched.time_limit_ms;
                ml = fetched.memory_limit_mb;
            }
        }
        if final_title.is_empty() {
            final_title = format!("{} {}", parsed.platform.to_uppercase(), parsed.external_id);
        }
        if stmt_val.is_empty() {
            stmt_val = format!("Solve {} problem {} ({}).", parsed.platform, parsed.external_id, parsed.problem_url);
        }
        (Some(parsed.platform), Some(parsed.external_id), stmt_val)
    } else {
        let s = body.statement.unwrap_or_default();
        if final_title.is_empty() || s.trim().is_empty() {
            return err_resp(400, "Problem title and statement are required for local problems.");
        }
        (Some("local".to_string()), None, s)
    };

    let samples = serde_json::to_string(&samples_vec).unwrap_or_else(|_| "[]".into());
    let conn = state.db.lock().unwrap();
    conn.execute(
        "INSERT INTO problems (source_type, platform, external_id, title, statement, sample_tests_json, hidden_tests_json, time_limit_ms, memory_limit_mb, created_by, created_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        params![
            source_type,
            platform,
            ext_id,
            final_title,
            stmt_text,
            samples,
            hidden,
            tl,
            ml,
            user.id,
            now
        ],
    )
    .unwrap();
    let id = conn.last_insert_rowid();
    (StatusCode::CREATED, Json(json!({ "id": id }))).into_response()
}

#[derive(Deserialize)]
struct ContestsQuery {
    limit: Option<i64>,
}

async fn list_contests(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<ContestsQuery>,
) -> Response {
    let user = match require_user(&state, &headers) {
        Ok(u) => u,
        Err(r) => return r,
    };
    let conn = state.db.lock().unwrap();
    let limit = q.limit.unwrap_or(10).clamp(1, 50);
    let now = now_unix();

    let mut stmt = conn
        .prepare(
            "SELECT c.id, c.title, c.description, c.access_code, c.start_time, c.end_time,
                    c.penalty_minutes, c.freeze_minutes, c.created_by,
                    COALESCE(u.display_name, 'Coordinator') AS creator_name,
                    (SELECT COUNT(*) FROM contest_problems cp WHERE cp.contest_id = c.id) AS prob_count,
                    (SELECT COUNT(*) FROM contest_participants p WHERE p.contest_id = c.id) AS part_count,
                    (SELECT COUNT(*) FROM submissions s WHERE s.contest_id = c.id) AS sub_count
             FROM contests c
             LEFT JOIN users u ON u.id = c.created_by
             ORDER BY CASE WHEN ? <= c.end_time THEN 0 ELSE 1 END, c.id DESC
             LIMIT ?",
        )
        .unwrap();

    let rows: Vec<Value> = stmt
        .query_map(params![now, limit], |r| {
            let cid: i64 = r.get(0)?;
            let start: i64 = r.get(4)?;
            let end: i64 = r.get(5)?;
            let access_code: String = r.get(3)?;
            let created_by: Option<i64> = r.get(8)?;
            let status = if now < start {
                "upcoming"
            } else if now <= end {
                "running"
            } else {
                "ended"
            };
            let is_owner = created_by.is_some_and(|owner| owner == user.id);
            let joined = is_owner
                || conn
                    .query_row(
                        "SELECT COUNT(*) FROM contest_participants WHERE contest_id = ? AND user_id = ?",
                        params![cid, user.id],
                        |x| x.get::<_, i64>(0),
                    )
                    .unwrap_or(0)
                    > 0;

            Ok(json!({
                "id": cid,
                "title": r.get::<_, String>(1)?,
                "description": r.get::<_, String>(2)?,
                "access_code": if is_owner { Some(access_code) } else { None },
                "start_time": start,
                "end_time": end,
                "penalty_minutes": r.get::<_, i64>(6)?,
                "freeze_minutes": r.get::<_, i64>(7)?,
                "creator_name": r.get::<_, String>(9)?,
                "problem_count": r.get::<_, i64>(10)?,
                "participant_count": r.get::<_, i64>(11)?,
                "submission_count": r.get::<_, i64>(12)?,
                "status": status,
                "is_creator": is_owner,
                "joined": joined
            }))
        })
        .unwrap()
        .filter_map(Result::ok)
        .collect();

    Json(json!({ "contests": rows })).into_response()
}

#[derive(Deserialize)]
struct CreateContestReq {
    title: String,
    description: Option<String>,
    #[serde(rename = "accessCode")]
    access_code: Option<String>,
    #[serde(rename = "startTime")]
    start_time: i64,
    #[serde(rename = "endTime")]
    end_time: i64,
    #[serde(rename = "penaltyMinutes")]
    penalty_minutes: Option<i64>,
    #[serde(rename = "freezeMinutes")]
    freeze_minutes: Option<i64>,
    #[serde(rename = "problemIds")]
    problem_ids: Vec<i64>,
}

async fn create_contest(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<CreateContestReq>,
) -> Response {
    let user = match require_user(&state, &headers) {
        Ok(u) => u,
        Err(r) => return r,
    };
    if body.title.trim().is_empty() || body.end_time <= body.start_time || body.problem_ids.is_empty() {
        return err_resp(400, "Provide title, valid time range, and at least one problem.");
    }

    let code = body
        .access_code
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| format!("CLUB-{}", &Uuid::new_v4().simple().to_string()[..6].to_uppercase()));
    let now = now_unix();

    let conn = state.db.lock().unwrap();
    if let Err(e) = conn.execute(
        "INSERT INTO contests (title, description, access_code, start_time, end_time, penalty_minutes, freeze_minutes, created_by, created_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        params![
            body.title.trim(),
            body.description.unwrap_or_default(),
            code.trim().to_uppercase(),
            body.start_time,
            body.end_time,
            body.penalty_minutes.unwrap_or(20),
            body.freeze_minutes.unwrap_or(30),
            user.id,
            now
        ],
    ) {
        return err_resp(409, format!("Could not create contest (duplicate access code?): {e}"));
    }

    let cid = conn.last_insert_rowid();
    for (idx, pid) in body.problem_ids.iter().enumerate() {
        let alias = ((b'A' + idx as u8) as char).to_string();
        let _ = conn.execute(
            "INSERT INTO contest_problems (contest_id, problem_id, alias) VALUES (?, ?, ?)",
            params![cid, pid, alias],
        );
    }
    let _ = conn.execute(
        "INSERT OR IGNORE INTO contest_participants (contest_id, user_id, joined_at) VALUES (?, ?, ?)",
        params![cid, user.id, now],
    );

    (StatusCode::CREATED, Json(json!({ "id": cid, "access_code": code.trim().to_uppercase() }))).into_response()
}

#[derive(Deserialize)]
struct JoinContestReq {
    #[serde(rename = "accessCode")]
    access_code: String,
    #[serde(rename = "contestId")]
    contest_id: Option<i64>,
}

async fn join_contest_by_code(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<JoinContestReq>,
) -> Response {
    let user = match require_user(&state, &headers) {
        Ok(u) => u,
        Err(r) => return r,
    };
    let clean_code = body.access_code.trim().to_uppercase();
    if clean_code.is_empty() {
        return err_resp(400, "Enter a contest access code.");
    }
    let conn = state.db.lock().unwrap();
    let row: Option<(i64, String)> = if let Some(target_cid) = body.contest_id {
        conn.query_row(
            "SELECT id, title FROM contests WHERE id = ? AND upper(access_code) = ?",
            params![target_cid, clean_code],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .ok()
    } else {
        conn.query_row(
            "SELECT id, title FROM contests WHERE upper(access_code) = ?",
            params![clean_code],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .ok()
    };

    let Some((contest_id, title)) = row else {
        let msg = if body.contest_id.is_some() {
            "Invalid access code for this contest."
        } else {
            "Invalid contest access code."
        };
        return err_resp(403, msg);
    };

    let _ = conn.execute(
        "INSERT OR IGNORE INTO contest_participants (contest_id, user_id, joined_at) VALUES (?, ?, ?)",
        params![contest_id, user.id, now_unix()],
    );

    Json(json!({ "ok": true, "contestId": contest_id, "title": title })).into_response()
}

async fn get_contest(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(contest_id): Path<i64>,
) -> Response {
    let user = match require_user(&state, &headers) {
        Ok(u) => u,
        Err(r) => return r,
    };
    let conn = state.db.lock().unwrap();
    let now = now_unix();

    let contest = conn
        .query_row(
            "SELECT id, title, description, access_code, start_time, end_time, penalty_minutes, freeze_minutes, created_by
             FROM contests WHERE id = ?",
            params![contest_id],
            |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, i64>(4)?,
                    r.get::<_, i64>(5)?,
                    r.get::<_, i64>(6)?,
                    r.get::<_, i64>(7)?,
                    r.get::<_, Option<i64>>(8)?,
                ))
            },
        )
        .ok();

    let Some((id, title, desc, code, start, end, pen, freeze, created_by)) = contest else {
        return err_resp(404, "Contest not found.");
    };

    let is_owner = created_by.is_some_and(|owner| owner == user.id);
    let is_ended = now > end;
    let joined = is_ended
        || is_owner
        || conn
            .query_row(
                "SELECT COUNT(*) FROM contest_participants WHERE contest_id = ? AND user_id = ?",
                params![id, user.id],
                |r| r.get::<_, i64>(0),
            )
            .unwrap_or(0)
            > 0;

    if !joined {
        return err_resp(403, "Enter the contest access code to view this contest.");
    }

    let hide_problems = now < start && !is_owner;
    let problems: Vec<Value> = if hide_problems {
        Vec::new()
    } else {
        let mut prob_stmt = conn
            .prepare(
                "SELECT cp.alias, p.id, p.source_type, p.platform, p.external_id, p.title, p.statement,
                        p.input_spec, p.output_spec, p.sample_tests_json, p.hidden_tests_json, p.time_limit_ms, p.memory_limit_mb
                 FROM contest_problems cp
                 JOIN problems p ON p.id = cp.problem_id
                 WHERE cp.contest_id = ?
                 ORDER BY cp.alias ASC",
            )
            .unwrap();

        prob_stmt
            .query_map(params![id], |r| {
                let source_type: String = r.get(2)?;
                let platform: String = r.get::<_, Option<String>>(3)?.unwrap_or_else(|| "local".into());
                let ext_id: Option<String> = r.get(4)?;
                let samples_str: String = r.get(9)?;
                let hidden_str: String = r.get(10)?;
                let samples: Vec<TestCase> = serde_json::from_str(&samples_str).unwrap_or_default();
                let hidden: Vec<TestCase> = serde_json::from_str(&hidden_str).unwrap_or_default();

                let parsed = ext_id
                    .as_deref()
                    .and_then(|eid| parse_oj_problem(&platform, eid));

                Ok(json!({
                    "alias": r.get::<_, String>(0)?,
                    "id": r.get::<_, i64>(1)?,
                    "source_type": source_type,
                    "platform": platform,
                    "external_id": ext_id,
                    "title": r.get::<_, String>(5)?,
                    "statement": r.get::<_, String>(6)?,
                    "input_spec": r.get::<_, String>(7)?,
                    "output_spec": r.get::<_, String>(8)?,
                    "sampleTests": samples,
                    "hasLocalTests": !samples.is_empty() || !hidden.is_empty(),
                    "submitUrl": parsed.as_ref().map(|p| &p.submit_url),
                    "problemUrl": parsed.as_ref().map(|p| &p.problem_url),
                    "time_limit_ms": r.get::<_, i64>(11)?,
                    "memory_limit_mb": r.get::<_, i64>(12)?
                }))
            })
            .unwrap()
            .filter_map(Result::ok)
            .collect()
    };

    let mut clar_stmt = conn
        .prepare(
            "SELECT c.id, c.problem_alias, c.question, c.answer, c.is_announcement, c.created_at, u.display_name
             FROM clarifications c
             JOIN users u ON u.id = c.user_id
             WHERE c.contest_id = ?
             ORDER BY c.is_announcement DESC, c.id DESC",
        )
        .unwrap();
    let clarifications: Vec<Value> = clar_stmt
        .query_map(params![id], |r| {
            Ok(json!({
                "id": r.get::<_, i64>(0)?,
                "problem_alias": r.get::<_, String>(1)?,
                "question": r.get::<_, String>(2)?,
                "answer": r.get::<_, String>(3)?,
                "is_announcement": r.get::<_, i64>(4)? != 0,
                "created_at": r.get::<_, i64>(5)?,
                "username": r.get::<_, String>(6)?
            }))
        })
        .unwrap()
        .filter_map(Result::ok)
        .collect();

    let show_code = is_owner;
    let status = if now < start { "upcoming" } else if now <= end { "running" } else { "ended" };

    Json(json!({
        "contest": {
            "id": id,
            "title": title,
            "description": desc,
            "access_code": if show_code { Some(code) } else { None },
            "start_time": start,
            "end_time": end,
            "penalty_minutes": pen,
            "freeze_minutes": freeze,
            "status": status,
            "is_creator": is_owner
        },
        "problems": problems,
        "clarifications": clarifications
    }))
    .into_response()
}

#[derive(Deserialize)]
struct PrepExtReq {
    #[serde(rename = "problemId")]
    problem_id: i64,
    language: Option<String>,
    code: Option<String>,
}

async fn prepare_external_submit(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(contest_id): Path<i64>,
    Json(body): Json<PrepExtReq>,
) -> Response {
    let user = match require_user(&state, &headers) {
        Ok(u) => u,
        Err(r) => return r,
    };

    let conn = state.db.lock().unwrap();
    let row: Option<(String, String)> = conn
        .query_row(
            "SELECT p.platform, p.external_id
             FROM contest_problems cp
             JOIN problems p ON p.id = cp.problem_id
             WHERE cp.contest_id = ? AND cp.problem_id = ? AND p.source_type = 'external'",
            params![contest_id, body.problem_id],
            |r| Ok((r.get::<_, Option<String>>(0)?.unwrap_or_default(), r.get::<_, Option<String>>(1)?.unwrap_or_default())),
        )
        .ok();

    let Some((platform, ext_id)) = row else {
        return err_resp(400, "Problem is not an external OJ problem.");
    };
    let Some(parsed) = parse_oj_problem(&platform, &ext_id) else {
        return err_resp(400, "Could not parse external OJ problem URL.");
    };

    if let Some(code) = body.code.filter(|c| !c.trim().is_empty()) {
        let _ = conn.execute(
            "INSERT INTO external_submit_drafts (contest_id, problem_id, user_id, language, code, used, created_at)
             VALUES (?, ?, ?, ?, ?, 0, ?)",
            params![
                contest_id,
                body.problem_id,
                user.id,
                body.language.unwrap_or_else(|| "cpp".into()),
                code,
                now_unix()
            ],
        );
    }

    Json(json!({
        "ok": true,
        "platform": parsed.platform,
        "externalId": parsed.external_id,
        "submitUrl": parsed.submit_url,
        "problemUrl": parsed.problem_url
    }))
    .into_response()
}

async fn sync_contest(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(contest_id): Path<i64>,
) -> Response {
    let user = match require_user(&state, &headers) {
        Ok(u) => u,
        Err(r) => return r,
    };

    // Clone handles + contest info without holding Mutex across .await
    let (start_time, end_time, handles, contest_probs) = {
        let conn = state.db.lock().unwrap();
        let times: Option<(i64, i64)> = conn
            .query_row(
                "SELECT start_time, end_time FROM contests WHERE id = ?",
                params![contest_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .ok();
        let Some((st, en)) = times else {
            return err_resp(404, "Contest not found.");
        };

        let mut h_stmt = conn
            .prepare("SELECT platform, handle FROM oj_handles WHERE user_id = ?")
            .unwrap();
        let h_list: Vec<(String, String)> = h_stmt
            .query_map(params![user.id], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .filter_map(Result::ok)
            .collect();

        let mut p_stmt = conn
            .prepare(
                "SELECT p.id, p.platform, p.external_id
                 FROM contest_problems cp
                 JOIN problems p ON p.id = cp.problem_id
                 WHERE cp.contest_id = ? AND p.source_type = 'external'",
            )
            .unwrap();
        let p_list: Vec<(i64, String, String)> = p_stmt
            .query_map(params![contest_id], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, Option<String>>(1)?.unwrap_or_default(),
                    r.get::<_, Option<String>>(2)?.unwrap_or_default(),
                ))
            })
            .unwrap()
            .filter_map(Result::ok)
            .collect();

        (st, en, h_list, p_list)
    };

    if handles.is_empty() {
        return err_resp(400, "Link an OJ handle (Codeforces / AtCoder) first.");
    }

    // Fetch external APIs asynchronously without holding DB lock
    let _ = (start_time, end_time, contest_probs); // used inside sync_user_contest_ojs helper below
    // Note: use a dedicated blocking-free fetch + short lock pattern via reqwest
    let mut fetched_cf = Vec::new();
    let mut fetched_ac = Vec::new();
    let mut notes = Vec::new();

    for (plat, handle) in &handles {
        if plat == "codeforces" {
            let url = format!("{}/user.status?handle={}&from=1&count=30", state.cf_api_base, handle);
            match state.http.get(&url).send().await {
                Ok(r) if r.status().is_success() => {
                    if let Ok(json) = r.json::<Value>().await {
                        fetched_cf.push(json);
                    }
                }
                Ok(r) => notes.push(format!("Codeforces returned HTTP {}", r.status())),
                Err(e) => notes.push(format!("Codeforces unreachable: {e}")),
            }
        } else if plat == "atcoder" {
            let url = format!(
                "{}/atcoder-api/v3/user/submissions?user={}&from_second={}",
                state.atcoder_api_base, handle, start_time
            );
            if let Ok(r) = state.http.get(&url).send().await {
                if r.status().is_success() {
                    if let Ok(json) = r.json::<Value>().await {
                        fetched_ac.push(json);
                    }
                }
            }
        }
    }

    let (synced, updated) = {
        let conn = state.db.lock().unwrap();
        apply_fetched_oj_results(&conn, contest_id, user.id, &fetched_cf, &fetched_ac)
    };

    if synced > 0 || updated > 0 {
        state.broadcast_contest(contest_id, "sync");
    }

    Json(json!({
        "ok": true,
        "syncedCount": synced,
        "updatedCount": updated,
        "notes": notes
    }))
    .into_response()
}

fn apply_fetched_oj_results(
    conn: &Connection,
    contest_id: i64,
    user_id: i64,
    cf_payloads: &[Value],
    ac_payloads: &[Value],
) -> (usize, usize) {
    let (start_time, end_time): (i64, i64) = conn
        .query_row(
            "SELECT start_time, end_time FROM contests WHERE id = ?",
            params![contest_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap_or((0, 0));

    let mut prob_stmt = conn
        .prepare(
            "SELECT p.id, p.platform, p.external_id
             FROM contest_problems cp
             JOIN problems p ON p.id = cp.problem_id
             WHERE cp.contest_id = ? AND p.source_type = 'external'",
        )
        .unwrap();
    let mut prob_map: HashMap<(String, String), i64> = HashMap::new();
    for (pid, plat, ext) in prob_stmt
        .query_map(params![contest_id], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, Option<String>>(1)?.unwrap_or_default().to_lowercase(),
                r.get::<_, Option<String>>(2)?.unwrap_or_default().to_uppercase(),
            ))
        })
        .unwrap()
        .filter_map(Result::ok)
    {
        prob_map.insert((plat, ext), pid);
    }

    let mut synced = 0usize;
    let mut updated = 0usize;

    let is_ended = now_unix() > end_time;
    for json in cf_payloads {
        if let Some(items) = json["result"].as_array() {
            for item in items {
                let created = item["creationTimeSeconds"].as_i64().unwrap_or(0);
                if created + 60 < start_time || (!is_ended && created > end_time) {
                    continue;
                }
                let cid = item["problem"]["contestId"].as_i64().unwrap_or(0);
                let idx = item["problem"]["index"].as_str().unwrap_or("").to_uppercase();
                let Some(&pid) = prob_map.get(&("codeforces".into(), format!("{cid}{idx}"))) else {
                    continue;
                };
                let sid = item["id"].as_i64().unwrap_or(0);
                if sid == 0 {
                    continue;
                }
                let ext_sub_id = format!("cf:{sid}");
                let verdict = crate::oj_sync::map_cf_verdict(item["verdict"].as_str());
                let time_ms = item["timeConsumedMillis"].as_i64().unwrap_or(0);
                let mem_kb = item["memoryConsumedBytes"].as_i64().unwrap_or(0) / 1024;
                let passed = item["passedTestCount"].as_i64().unwrap_or(0);
                let lang = item["programmingLanguage"].as_str().unwrap_or("Codeforces");

                record_synced_sub(
                    conn, contest_id, pid, user_id, "codeforces", &ext_sub_id, lang, verdict,
                    time_ms, mem_kb, passed, created, &mut synced, &mut updated,
                );
            }
        }
    }

    for json in ac_payloads {
        if let Some(items) = json.as_array() {
            for item in items {
                let created = item["epoch_second"].as_i64().unwrap_or(0);
                if created + 60 < start_time || (!is_ended && created > end_time) {
                    continue;
                }
                let pcode = item["problem_id"].as_str().unwrap_or("").to_uppercase();
                let Some(&pid) = prob_map.get(&("atcoder".into(), pcode)) else {
                    continue;
                };
                let sid = item["id"].as_i64().unwrap_or(0);
                if sid == 0 {
                    continue;
                }
                let ext_sub_id = format!("ac:{sid}");
                let verdict = crate::oj_sync::map_atcoder_verdict(item["result"].as_str().unwrap_or("WJ"));
                let time_ms = item["execution_time"].as_i64().unwrap_or(0);
                let lang = item["language"].as_str().unwrap_or("AtCoder");

                record_synced_sub(
                    conn, contest_id, pid, user_id, "atcoder", &ext_sub_id, lang, verdict,
                    time_ms, 0, 0, created, &mut synced, &mut updated,
                );
            }
        }
    }

    (synced, updated)
}

#[allow(clippy::too_many_arguments)]
fn record_synced_sub(
    conn: &Connection,
    contest_id: i64,
    problem_id: i64,
    user_id: i64,
    platform: &str,
    ext_sub_id: &str,
    lang: &str,
    verdict: &str,
    time_ms: i64,
    memory_kb: i64,
    passed: i64,
    created: i64,
    synced: &mut usize,
    updated: &mut usize,
) {
    let existing: Option<(i64, String)> = conn
        .query_row(
            "SELECT id, verdict FROM submissions WHERE external_sub_id = ?",
            params![ext_sub_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .ok();

    if let Some((row_id, prev)) = existing {
        if prev == "PENDING" && verdict != "PENDING" {
            let _ = conn.execute(
                "UPDATE submissions SET verdict = ?, time_ms = ?, memory_kb = ?, passed_tests = ? WHERE id = ?",
                params![verdict, time_ms, memory_kb, passed, row_id],
            );
            *updated += 1;
        }
    } else {
        let draft: Option<(i64, String)> = conn
            .query_row(
                "SELECT id, code FROM external_submit_drafts WHERE contest_id = ? AND problem_id = ? AND user_id = ? AND used = 0 ORDER BY id DESC LIMIT 1",
                params![contest_id, problem_id, user_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .ok();
        let code_backup = if let Some((did, c)) = draft {
            let _ = conn.execute("UPDATE external_submit_drafts SET used = 1 WHERE id = ?", params![did]);
            c
        } else {
            String::new()
        };

        if conn
            .execute(
                "INSERT INTO submissions (contest_id, problem_id, user_id, source_type, platform, external_sub_id, language, code, verdict, time_ms, memory_kb, passed_tests, total_tests, error_message, submitted_at)
                 VALUES (?, ?, ?, 'external', ?, ?, ?, ?, ?, ?, ?, ?, 0, '', ?)",
                params![contest_id, problem_id, user_id, platform, ext_sub_id, lang, code_backup, verdict, time_ms, memory_kb, passed, created],
            )
            .is_ok()
        {
            *synced += 1;
        }
    }
}

async fn sync_all_contest(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(contest_id): Path<i64>,
) -> Response {
    let user = match require_user(&state, &headers) {
        Ok(u) => u,
        Err(r) => return r,
    };
    if !is_coordinator(&user) {
        return err_resp(403, "Coordinator access required.");
    }

    let (start_time, user_handles): (i64, Vec<(i64, String, String)>) = {
        let conn = state.db.lock().unwrap();
        let st_time: i64 = conn
            .query_row("SELECT start_time FROM contests WHERE id = ?", params![contest_id], |r| r.get(0))
            .unwrap_or(0);
        let mut st = conn.prepare("SELECT user_id, platform, handle FROM oj_handles").unwrap();
        let handles = st
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .filter_map(Result::ok)
            .collect();
        (st_time, handles)
    };

    let mut total_synced = 0usize;
    let mut total_updated = 0usize;

    for (uid, plat, handle) in &user_handles {
        if plat == "codeforces" {
            let url = format!("{}/user.status?handle={}&from=1&count=20", state.cf_api_base, handle);
            if let Ok(r) = state.http.get(&url).send().await {
                if let Ok(json) = r.json::<Value>().await {
                    let conn = state.db.lock().unwrap();
                    let (s, u) = apply_fetched_oj_results(&conn, contest_id, *uid, &[json], &[]);
                    total_synced += s;
                    total_updated += u;
                }
            }
        } else if plat == "atcoder" {
            let url = format!(
                "{}/atcoder-api/v3/user/submissions?user={}&from_second={}",
                state.atcoder_api_base, handle, start_time
            );
            if let Ok(r) = state.http.get(&url).send().await {
                if let Ok(json) = r.json::<Value>().await {
                    let conn = state.db.lock().unwrap();
                    let (s, u) = apply_fetched_oj_results(&conn, contest_id, *uid, &[], &[json]);
                    total_synced += s;
                    total_updated += u;
                }
            }
        }
    }

    if total_synced > 0 || total_updated > 0 {
        state.broadcast_contest(contest_id, "sync-all");
    }

    Json(json!({
        "ok": true,
        "checkedHandles": user_handles.len(),
        "totalSynced": total_synced,
        "totalUpdated": total_updated
    }))
    .into_response()
}

#[derive(Deserialize)]
struct SubmitLocalReq {
    #[serde(rename = "problemId")]
    problem_id: i64,
    language: String,
    code: String,
    #[serde(rename = "useLocalJudge")]
    use_local_judge: Option<bool>,
}

async fn submit_local(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(contest_id): Path<i64>,
    Json(body): Json<SubmitLocalReq>,
) -> Response {
    let user = match require_user(&state, &headers) {
        Ok(u) => u,
        Err(r) => return r,
    };
    if body.code.trim().is_empty() {
        return err_resp(400, "Source code cannot be empty.");
    }

    let now = now_unix();
    let (tests, time_limit_ms, submission_id) = {
        let conn = state.db.lock().unwrap();
        let times: Option<(i64, i64)> = conn
            .query_row(
                "SELECT start_time, end_time FROM contests WHERE id = ?",
                params![contest_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .ok();
        let Some((start, _end)) = times else {
            return err_resp(404, "Contest not found.");
        };
        if !is_coordinator(&user) && now < start {
            return err_resp(400, "Contest has not started yet.");
        }

        let prob: Option<(String, String, String, i64)> = conn
            .query_row(
                "SELECT p.source_type, p.sample_tests_json, p.hidden_tests_json, p.time_limit_ms
                 FROM contest_problems cp
                 JOIN problems p ON p.id = cp.problem_id
                 WHERE cp.contest_id = ? AND cp.problem_id = ?",
                params![contest_id, body.problem_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .ok();
        let Some((src_type, s_json, h_json, tl)) = prob else {
            return err_resp(404, "Problem not in this contest.");
        };
        let mut tests: Vec<TestCase> = serde_json::from_str(&s_json).unwrap_or_default();
        let hidden: Vec<TestCase> = serde_json::from_str(&h_json).unwrap_or_default();
        tests.extend(hidden);

        if src_type != "local" && !(body.use_local_judge.unwrap_or(false) && !tests.is_empty()) {
            return err_resp(400, "Use Background WebView Submit or Official API Sync for external problems.");
        }

        conn.execute(
            "INSERT INTO submissions (contest_id, problem_id, user_id, source_type, platform, language, code, verdict, submitted_at)
             VALUES (?, ?, ?, 'local', 'local', ?, ?, 'PENDING', ?)",
            params![contest_id, body.problem_id, user.id, body.language, body.code, now],
        )
        .unwrap();
        (tests, tl, conn.last_insert_rowid())
    };

    let outcome = judge_local(&body.language, &body.code, &tests, time_limit_ms).await;

    let saved = {
        let conn = state.db.lock().unwrap();
        let _ = conn.execute(
            "UPDATE submissions SET verdict = ?, time_ms = ?, memory_kb = ?, passed_tests = ?, total_tests = ?, error_message = ? WHERE id = ?",
            params![
                outcome.verdict,
                outcome.time_ms,
                outcome.memory_kb,
                outcome.passed_tests,
                outcome.total_tests,
                outcome.error_message,
                submission_id
            ],
        );
        conn.query_row(
            "SELECT s.id, s.verdict, s.time_ms, s.passed_tests, s.total_tests, s.error_message, s.language, s.submitted_at
             FROM submissions s WHERE s.id = ?",
            params![submission_id],
            |r| {
                Ok(json!({
                    "id": r.get::<_, i64>(0)?,
                    "verdict": r.get::<_, String>(1)?,
                    "time_ms": r.get::<_, i64>(2)?,
                    "passed_tests": r.get::<_, i64>(3)?,
                    "total_tests": r.get::<_, i64>(4)?,
                    "error_message": r.get::<_, String>(5)?,
                    "language": r.get::<_, String>(6)?,
                    "submitted_at": r.get::<_, i64>(7)?
                }))
            },
        )
        .unwrap()
    };

    state.broadcast_contest(contest_id, "submission");
    (StatusCode::CREATED, Json(json!({ "submission": saved }))).into_response()
}

#[derive(Deserialize)]
struct SubsQuery {
    mine: Option<String>,
}

async fn list_submissions(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(contest_id): Path<i64>,
    Query(q): Query<SubsQuery>,
) -> Response {
    let conn = state.db.lock().unwrap();
    let user = session_user(&conn, &headers);
    let mine_only = q.mine.as_deref() == Some("1");
    let uid_filter = if mine_only { user.as_ref().map(|u| u.id) } else { None };

    let mut stmt = conn
        .prepare(
            "SELECT s.id, s.user_id, s.problem_id, s.source_type, s.platform, s.language, s.code,
                    s.verdict, s.time_ms, s.memory_kb, s.passed_tests, s.total_tests, s.error_message, s.submitted_at,
                    u.display_name, cp.alias, p.title
             FROM submissions s
             JOIN users u ON u.id = s.user_id
             JOIN contest_problems cp ON cp.contest_id = s.contest_id AND cp.problem_id = s.problem_id
             JOIN problems p ON p.id = s.problem_id
             WHERE s.contest_id = ?
             ORDER BY s.submitted_at DESC, s.id DESC
             LIMIT 200",
        )
        .unwrap();

    let rows: Vec<Value> = stmt
        .query_map(params![contest_id], |r| {
            let sub_uid: i64 = r.get(1)?;
            let code_str: String = r.get(6)?;
            let can_view = user
                .as_ref()
                .is_some_and(|u| is_coordinator(u) || u.id == sub_uid);

            Ok((
                sub_uid,
                json!({
                    "id": r.get::<_, i64>(0)?,
                    "user_id": sub_uid,
                    "problem_id": r.get::<_, i64>(2)?,
                    "source_type": r.get::<_, String>(3)?,
                    "platform": r.get::<_, Option<String>>(4)?,
                    "language": r.get::<_, String>(5)?,
                    "code": if can_view { Some(code_str.clone()) } else { None },
                    "hasCode": !code_str.is_empty(),
                    "verdict": r.get::<_, String>(7)?,
                    "time_ms": r.get::<_, i64>(8)?,
                    "memory_kb": r.get::<_, i64>(9)?,
                    "passed_tests": r.get::<_, i64>(10)?,
                    "total_tests": r.get::<_, i64>(11)?,
                    "error_message": r.get::<_, String>(12)?,
                    "submitted_at": r.get::<_, i64>(13)?,
                    "username": r.get::<_, String>(14)?,
                    "problem_alias": r.get::<_, String>(15)?,
                    "problem_title": r.get::<_, String>(16)?
                }),
            ))
        })
        .unwrap()
        .filter_map(Result::ok)
        .filter(|(suid, _)| uid_filter.is_none_or(|f| f == *suid))
        .map(|(_, v)| v)
        .collect();

    Json(json!({ "submissions": rows })).into_response()
}

async fn get_standings(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(contest_id): Path<i64>,
) -> Response {
    let conn = state.db.lock().unwrap();
    let user = session_user(&conn, &headers);
    let is_admin = user.as_ref().is_some_and(is_coordinator);

    let meta = conn
        .query_row(
            "SELECT id, start_time, end_time, penalty_minutes, freeze_minutes FROM contests WHERE id = ?",
            params![contest_id],
            |r| {
                Ok(ContestMeta {
                    id: r.get(0)?,
                    start_time: r.get(1)?,
                    end_time: r.get(2)?,
                    penalty_minutes: r.get(3)?,
                    freeze_minutes: r.get(4)?,
                })
            },
        )
        .ok();
    let Some(contest) = meta else {
        return err_resp(404, "Contest not found.");
    };

    let mut p_stmt = conn
        .prepare(
            "SELECT cp.problem_id, cp.alias, p.title, COALESCE(p.platform, 'local'), p.external_id
             FROM contest_problems cp
             JOIN problems p ON p.id = cp.problem_id
             WHERE cp.contest_id = ?
             ORDER BY cp.alias ASC",
        )
        .unwrap();
    let probs: Vec<ScoreboardProblem> = p_stmt
        .query_map(params![contest_id], |r| {
            Ok(ScoreboardProblem {
                problem_id: r.get(0)?,
                alias: r.get(1)?,
                title: r.get(2)?,
                platform: r.get(3)?,
                external_id: r.get(4)?,
            })
        })
        .unwrap()
        .filter_map(Result::ok)
        .collect();

    let mut s_stmt = conn
        .prepare(
            "SELECT s.id, s.user_id, u.display_name, u.email, s.problem_id, s.verdict, s.submitted_at
             FROM submissions s
             JOIN users u ON u.id = s.user_id
             WHERE s.contest_id = ?",
        )
        .unwrap();
    let subs: Vec<SubmissionForScore> = s_stmt
        .query_map(params![contest_id], |r| {
            Ok(SubmissionForScore {
                id: r.get(0)?,
                user_id: r.get(1)?,
                display_name: r.get(2)?,
                email: r.get(3)?,
                problem_id: r.get(4)?,
                verdict: r.get(5)?,
                submitted_at: r.get(6)?,
            })
        })
        .unwrap()
        .filter_map(Result::ok)
        .collect();

    let standings = compute_standings(&contest, probs, subs, is_admin, now_unix());
    Json(standings).into_response()
}

#[derive(Deserialize)]
struct ClarReq {
    #[serde(rename = "problemAlias")]
    problem_alias: Option<String>,
    question: String,
    #[serde(rename = "isAnnouncement")]
    is_announcement: Option<bool>,
}

async fn post_clarification(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(contest_id): Path<i64>,
    Json(body): Json<ClarReq>,
) -> Response {
    let user = match require_user(&state, &headers) {
        Ok(u) => u,
        Err(r) => return r,
    };
    if body.question.trim().is_empty() {
        return err_resp(400, "Question cannot be empty.");
    }
    let conn = state.db.lock().unwrap();
    let is_owner = conn
        .query_row(
            "SELECT created_by FROM contests WHERE id = ?",
            params![contest_id],
            |r| r.get::<_, Option<i64>>(0),
        )
        .ok()
        .flatten()
        .is_some_and(|owner| owner == user.id);
    let is_ann = if body.is_announcement.unwrap_or(false) && (is_coordinator(&user) || is_owner) {
        1
    } else {
        0
    };
    conn.execute(
        "INSERT INTO clarifications (contest_id, user_id, problem_alias, question, answer, is_announcement, created_at)
         VALUES (?, ?, ?, ?, '', ?, ?)",
        params![
            contest_id,
            user.id,
            body.problem_alias.unwrap_or_default(),
            body.question.trim(),
            is_ann,
            now_unix()
        ],
    )
    .unwrap();
    let id = conn.last_insert_rowid();
    state.broadcast_contest(contest_id, "clarification");
    (StatusCode::CREATED, Json(json!({ "id": id }))).into_response()
}

#[derive(Deserialize)]
struct AnswerClarReq {
    answer: String,
}

async fn answer_clarification(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((contest_id, clar_id)): Path<(i64, i64)>,
    Json(body): Json<AnswerClarReq>,
) -> Response {
    let user = match require_user(&state, &headers) {
        Ok(u) => u,
        Err(r) => return r,
    };
    let conn = state.db.lock().unwrap();
    let is_owner = conn
        .query_row(
            "SELECT created_by FROM contests WHERE id = ?",
            params![contest_id],
            |r| r.get::<_, Option<i64>>(0),
        )
        .ok()
        .flatten()
        .is_some_and(|owner| owner == user.id);
    if !is_coordinator(&user) && !is_owner {
        return err_resp(403, "Coordinator or contest creator access required.");
    }
    let _ = conn.execute(
        "UPDATE clarifications SET answer = ? WHERE id = ? AND contest_id = ?",
        params![body.answer.trim(), clar_id, contest_id],
    );
    state.broadcast_contest(contest_id, "clarification");
    Json(json!({ "ok": true })).into_response()
}

async fn contest_events(
    State(state): State<AppState>,
    Path(contest_id): Path<i64>,
) -> Sse<impl futures_core::Stream<Item = Result<Event, Infallible>>> {
    let rx = state.get_or_create_channel(contest_id).subscribe();
    let stream = BroadcastStream::new(rx).filter_map(|msg| match msg {
        Ok(payload) => Some(Ok(Event::default().event("update").data(payload))),
        Err(_) => None,
    });
    Sse::new(stream).keep_alive(KeepAlive::default())
}
