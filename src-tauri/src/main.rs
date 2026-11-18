use keyring::Entry;
use serde::Serialize;
use tauri::webview::PageLoadEvent;
use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindowBuilder};

const STANDARD_USER_AGENT: &str =
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.5 Safari/605.1.15";

#[derive(Serialize)]
struct LocalCreds {
    handle: String,
    has_password: bool,
}

#[derive(Clone, Serialize)]
struct OjSessionSavedPayload {
    platform: String,
    handle: Option<String>,
    method: String,
}

fn load_saved_keychain_creds(platform: &str) -> Option<(String, String)> {
    let service = format!("vj-lite-{}", platform.to_lowercase());
    let active_entry = Entry::new(&service, "__active_handle__").ok()?;
    let handle = active_entry.get_password().ok()?;
    if handle.trim().is_empty() {
        return None;
    }
    let pw_entry = Entry::new(&service, &handle).ok()?;
    let password = pw_entry.get_password().ok()?;
    Some((handle, password))
}

#[tauri::command]
fn save_oj_credentials(platform: String, handle: String, password: String) -> Result<(), String> {
    let service = format!("vj-lite-{}", platform.to_lowercase());
    if let Ok(active) = Entry::new(&service, "__active_handle__") {
        let _ = active.set_password(&handle);
    }
    let entry = Entry::new(&service, &handle).map_err(|e| e.to_string())?;
    entry.set_password(&password).map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
fn get_oj_credentials(platform: String, handle: String) -> Result<LocalCreds, String> {
    let service = format!("vj-lite-{}", platform.to_lowercase());
    let entry = Entry::new(&service, &handle).map_err(|e| e.to_string())?;
    let has_password = entry.get_password().is_ok();
    Ok(LocalCreds {
        handle,
        has_password,
    })
}

#[tauri::command]
fn show_oj_window(app: AppHandle) {
    if let Some(win) = app.get_webview_window("oj-submit") {
        let _ = win.show();
        let _ = win.set_focus();
    }
}

#[tauri::command]
fn oj_submit_finished(app: AppHandle, platform: String) {
    if let Some(win) = app.get_webview_window("oj-submit") {
        let _ = win.close();
    }
    let _ = app.emit("oj-submit-done", platform);
}

fn oj_login_url(platform: &str) -> Result<&'static str, String> {
    match platform.to_lowercase().as_str() {
        "codeforces" => Ok("https://codeforces.com/enter"),
        "atcoder" => Ok("https://atcoder.jp/login"),
        "cses" => Ok("https://cses.fi/login"),
        "codechef" => Ok("https://www.codechef.com/login"),
        _ => Err(format!("Unsupported platform: {platform}")),
    }
}

fn build_oj_login_init_script(handle_opt: Option<&str>, password_opt: Option<&str>) -> String {
    let h_json = serde_json::to_string(&handle_opt.unwrap_or("")).unwrap_or_else(|_| "\"\"".into());
    let p_json = serde_json::to_string(&password_opt.unwrap_or("")).unwrap_or_else(|_| "\"\"".into());

    format!(
        r#"
        window.addEventListener('DOMContentLoaded', () => {{
          // Keep Google / OAuth popups inside this persistent WebView session
          window.open = function(url) {{
            if (url) window.location.href = url;
            return window;
          }};
          document.querySelectorAll('a[target="_blank"]').forEach(a => a.removeAttribute('target'));

          const host = window.location.hostname.toLowerCase();
          const path = window.location.pathname.toLowerCase();
          const isAuthPage = path.includes('enter') || path.includes('login') || host.includes('google.com');

          if (!isAuthPage) {{
            const b = document.createElement('div');
            b.style.cssText = 'position:fixed;top:0;left:0;right:0;z-index:999999;background:#059669;color:#fff;padding:10px 16px;font:600 14px sans-serif;text-align:center;box-shadow:0 2px 10px rgba(0,0,0,0.3)';
            b.textContent = '✓ OJ Session Active & Saved in VJ-Lite! You can close this window.';
            document.body?.prepend(b);
            return;
          }}

          const h = {h_json};
          const p = {p_json};
          if (h && p && !sessionStorage.getItem('__vj_autofilled')) {{
            const userInp = document.querySelector('#handleOrEmail, #username, input[name="handleOrEmail"], input[name="username"], input[name="nick"], #edit-name');
            const passInp = document.querySelector('#password, input[name="password"], input[name="pass"], #edit-pass, input[type="password"]');
            if (userInp && passInp) {{
              sessionStorage.setItem('__vj_autofilled', '1');
              userInp.value = h;
              passInp.value = p;
              userInp.dispatchEvent(new Event('input', {{ bubbles: true }}));
              passInp.dispatchEvent(new Event('input', {{ bubbles: true }}));
              const remember = document.querySelector('#remember, input[name="remember"]');
              if (remember && !remember.checked) remember.checked = true;
              const form = passInp.closest('form');
              if (form) setTimeout(() => form.submit(), 250);
            }}
          }}
        }});
        "#
    )
}

#[tauri::command]
async fn login_oj_with_password(
    app: AppHandle,
    platform: String,
    handle: String,
    password: String,
) -> Result<(), String> {
    save_oj_credentials(platform.clone(), handle.clone(), password.clone())?;
    let login_url = oj_login_url(&platform)?;
    if let Some(existing) = app.get_webview_window("oj-login") {
        let _ = existing.close();
    }
    let parsed_url: tauri::Url = login_url.parse().map_err(|e| format!("Invalid URL: {e}"))?;
    let init_js = build_oj_login_init_script(Some(&handle), Some(&password));
    let label_title = format!(
        "{} — Signing in as @{} (Session Persisted)",
        platform.to_uppercase(),
        handle
    );
    let app_handle = app.clone();
    let plat_clone = platform.to_lowercase();
    let handle_clone = handle.clone();

    WebviewWindowBuilder::new(&app, "oj-login", WebviewUrl::External(parsed_url))
        .title(&label_title)
        .user_agent(STANDARD_USER_AGENT)
        .inner_size(980.0, 720.0)
        .visible(true)
        .initialization_script(&init_js)
        .on_page_load(move |_win, payload| {
            if matches!(payload.event(), PageLoadEvent::Finished) {
                let u = payload.url();
                let host = u.host_str().unwrap_or("").to_lowercase();
                let path = u.path().to_lowercase();
                let on_oj = host.contains("codeforces.")
                    || host.contains("atcoder.")
                    || host.contains("cses.")
                    || host.contains("codechef.");
                if on_oj && !path.contains("enter") && !path.contains("login") {
                    let _ = app_handle.emit(
                        "oj-session-saved",
                        OjSessionSavedPayload {
                            platform: plat_clone.clone(),
                            handle: Some(handle_clone.clone()),
                            method: "password".into(),
                        },
                    );
                }
            }
        })
        .build()
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
async fn open_oj_login_webview(
    app: AppHandle,
    platform: String,
    handle: Option<String>,
) -> Result<(), String> {
    let login_url = oj_login_url(&platform)?;
    if let Some(existing) = app.get_webview_window("oj-login") {
        let _ = existing.close();
    }
    let parsed_url: tauri::Url = login_url.parse().map_err(|e| format!("Invalid URL: {e}"))?;
    let init_js = build_oj_login_init_script(None, None);
    let label_title = format!(
        "Sign in to {} with Google / Browser (Session Persisted)",
        platform.to_uppercase()
    );
    let app_handle = app.clone();
    let plat_clone = platform.to_lowercase();
    let handle_clone = handle.filter(|s| !s.trim().is_empty());

    WebviewWindowBuilder::new(&app, "oj-login", WebviewUrl::External(parsed_url))
        .title(&label_title)
        .user_agent(STANDARD_USER_AGENT)
        .inner_size(980.0, 720.0)
        .visible(true)
        .initialization_script(&init_js)
        .on_page_load(move |_win, payload| {
            if matches!(payload.event(), PageLoadEvent::Finished) {
                let u = payload.url();
                let host = u.host_str().unwrap_or("").to_lowercase();
                let path = u.path().to_lowercase();
                let on_oj = host.contains("codeforces.")
                    || host.contains("atcoder.")
                    || host.contains("cses.")
                    || host.contains("codechef.");
                if on_oj && !path.contains("enter") && !path.contains("login") {
                    let _ = app_handle.emit(
                        "oj-session-saved",
                        OjSessionSavedPayload {
                            platform: plat_clone.clone(),
                            handle: handle_clone.clone(),
                            method: "google_sso".into(),
                        },
                    );
                }
            }
        })
        .build()
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
async fn submit_via_webview(
    app: AppHandle,
    platform: String,
    submit_url: String,
    language: String,
    code: String,
) -> Result<(), String> {
    let code_json = serde_json::to_string(&code).map_err(|e| e.to_string())?;
    let lang_json = serde_json::to_string(&language).map_err(|e| e.to_string())?;
    let saved_creds = load_saved_keychain_creds(&platform);
    let saved_h = serde_json::to_string(
        &saved_creds.as_ref().map(|c| c.0.as_str()).unwrap_or(""),
    )
    .unwrap_or_else(|_| "\"\"".into());
    let saved_p = serde_json::to_string(
        &saved_creds.as_ref().map(|c| c.1.as_str()).unwrap_or(""),
    )
    .unwrap_or_else(|_| "\"\"".into());

    let init_js = format!(
        r#"
        window.addEventListener('DOMContentLoaded', () => {{
          const path = window.location.pathname.toLowerCase();
          if (path.includes('enter') || path.includes('login')) {{
            const h = {saved_h};
            const p = {saved_p};
            if (h && p && !sessionStorage.getItem('__vj_submit_login')) {{
              const userInp = document.querySelector('#handleOrEmail, #username, input[name="handleOrEmail"], input[name="username"], input[name="nick"], #edit-name');
              const passInp = document.querySelector('#password, input[name="password"], input[name="pass"], #edit-pass, input[type="password"]');
              if (userInp && passInp) {{
                sessionStorage.setItem('__vj_submit_login', '1');
                userInp.value = h;
                passInp.value = p;
                const form = passInp.closest('form');
                if (form) {{ form.submit(); return; }}
              }}
            }}
            return;
          }}
          if (!path.includes('submit')) return;
          const form = document.querySelector('form.submit-form, form[action*="submit"]');
          if (!form) return;
          const lang = {lang_json};
          const code = {code_json};
          const cfSelect = form.querySelector('select[name="programTypeId"]');
          if (cfSelect) {{
            const map = {{ cpp: ['89','73','54'], python3: ['31','70'], javascript: ['55'] }};
            for (const val of (map[lang] || [])) {{
              if ([...cfSelect.options].some(o => o.value === val)) {{ cfSelect.value = val; break; }}
            }}
          }}
          const fileInput = form.querySelector('input[type="file"][name="sourceFile"], input[type="file"]');
          const textarea = form.querySelector('#sourceCodeTextarea, textarea[name="source"], textarea[name="sourceCode"]');
          if (fileInput && typeof DataTransfer !== 'undefined') {{
            const dt = new DataTransfer();
            dt.items.add(new File([code], 'solution.txt', {{ type: 'text/plain' }}));
            fileInput.files = dt.files;
          }} else if (textarea) {{
            textarea.value = code;
          }}
          form.submit();
        }});
        "#
    );

    if let Some(existing) = app.get_webview_window("oj-submit") {
        let _ = existing.close();
    }

    let parsed_url: tauri::Url = submit_url.parse().map_err(|e| format!("Invalid URL: {e}"))?;
    let app_handle = app.clone();
    let plat_clone = platform.clone();
    WebviewWindowBuilder::new(&app, "oj-submit", WebviewUrl::External(parsed_url))
        .title("OJ Submit Window")
        .user_agent(STANDARD_USER_AGENT)
        .inner_size(960.0, 720.0)
        .visible(false)
        .initialization_script(&init_js)
        .on_page_load(move |win, payload| {
            if matches!(payload.event(), PageLoadEvent::Finished) {
                let path = payload.url().path().to_lowercase();
                if path.contains("enter") || path.contains("login") {
                    let _ = win.show();
                    let _ = win.set_focus();
                } else if !path.contains("submit") {
                    let _ = win.close();
                    let _ = app_handle.emit("oj-submit-done", plat_clone.clone());
                } else {
                    // If auto-submit didn't redirect within 2.5s (e.g. human verification), show window to user
                    let w = win.clone();
                    tauri::async_runtime::spawn(async move {
                        tokio::time::sleep(std::time::Duration::from_millis(2500)).await;
                        if let Ok(u) = w.url() {
                            let p = u.path().to_lowercase();
                            if p.contains("submit") || p.contains("enter") || p.contains("login") {
                                let _ = w.show();
                                let _ = w.set_focus();
                            }
                        }
                    });
                }
            }
        })
        .build()
        .map_err(|e| e.to_string())?;

    Ok(())
}

#[tauri::command]
fn ui_log(msg: String) {
    eprintln!("[vj-ui] {msg}");
}

fn open_system_browser(url: &str) -> Result<(), String> {
    eprintln!("[vj-desktop] open_system_browser: {url}");
    #[cfg(target_os = "macos")]
    let status = std::process::Command::new("open").arg(url).status();
    #[cfg(target_os = "windows")]
    let status = std::process::Command::new("cmd")
        .args(["/C", "start", "", url])
        .status();
    #[cfg(all(unix, not(target_os = "macos")))]
    let status = std::process::Command::new("xdg-open").arg(url).status();

    status
        .map_err(|e| format!("Failed to open system browser: {e}"))
        .and_then(|s| {
            if s.success() {
                Ok(())
            } else {
                Err("Browser failed to launch".into())
            }
        })
}

#[tauri::command]
async fn start_supabase_google_oauth(app: AppHandle, supabase_url: String) -> Result<(), String> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = match tokio::net::TcpListener::bind("127.0.0.1:43821").await {
        Ok(l) => l,
        Err(_) => tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .map_err(|e| e.to_string())?,
    };
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    let redirect_uri = format!("http://127.0.0.1:{port}/callback");
    let auth_url = format!(
        "{}/auth/v1/authorize?provider=google&redirect_to={}",
        supabase_url.trim_end_matches('/'),
        redirect_uri
    );
    eprintln!("[vj-desktop] Starting Google OAuth in system browser: {auth_url}");

    tokio::spawn(async move {
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(180);
        while tokio::time::Instant::now() < deadline {
            let accept_res = tokio::time::timeout(
                std::time::Duration::from_secs(180),
                listener.accept(),
            )
            .await;
            let Ok(Ok((mut stream, _))) = accept_res else {
                break;
            };

            let mut buf = [0u8; 8192];
            let Ok(n) = stream.read(&mut buf).await else {
                continue;
            };
            let req = String::from_utf8_lossy(&buf[..n]);

            if req.starts_with("GET /callback") {
                let html = r#"<!doctype html><html><body style="font-family:sans-serif;background:#0b0d12;color:#f8fafc;display:grid;place-items:center;height:100vh;margin:0"><div style="text-align:center"><h2>Completing Google Sign-In...</h2><p id="msg">Returning to VJ-Lite Desktop App.</p></div><script>
const p = new URLSearchParams(location.hash.slice(1));
const tok = p.get('access_token');
if (tok) {
  fetch('/token', { method: 'POST', body: tok }).then(() => {
    document.getElementById('msg').textContent = 'Signed in! You can close this tab and return to VJ-Lite.';
  });
} else {
  document.getElementById('msg').textContent = 'No access token found. Please try again.';
}
</script></body></html>"#;
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    html.len(),
                    html
                );
                let _ = stream.write_all(resp.as_bytes()).await;
            } else if req.starts_with("POST /token") {
                if let Some((_, body)) = req.split_once("\r\n\r\n") {
                    let token = body.trim().to_string();
                    if !token.is_empty() {
                        let _ = app.emit("supabase-oauth-token", token);
                        if let Some(win) = app.get_webview_window("main") {
                            let _ = win.set_focus();
                        }
                    }
                }
                let resp = "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok";
                let _ = stream.write_all(resp.as_bytes()).await;
                break;
            } else {
                let _ = stream
                    .write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                    .await;
            }
        }
    });

    open_system_browser(&auth_url)?;
    Ok(())
}

fn main() {
    tauri::Builder::default()
        .setup(|app| {
            eprintln!(
                "[vj-desktop] setup complete, windows={:?}",
                app.webview_windows().keys().collect::<Vec<_>>()
            );
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            ui_log,
            save_oj_credentials,
            get_oj_credentials,
            show_oj_window,
            oj_submit_finished,
            login_oj_with_password,
            open_oj_login_webview,
            submit_via_webview,
            start_supabase_google_oauth
        ])
        .run(tauri::generate_context!())
        .expect("error while running VJ-Lite Desktop");
}
