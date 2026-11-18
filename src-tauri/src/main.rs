use keyring::Entry;
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindowBuilder};

#[derive(Serialize)]
struct LocalCreds {
    handle: String,
    has_password: bool,
}

#[tauri::command]
fn save_oj_credentials(platform: String, handle: String, password: String) -> Result<(), String> {
    let service = format!("vj-lite-{platform}");
    let entry = Entry::new(&service, &handle).map_err(|e| e.to_string())?;
    entry.set_password(&password).map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
fn get_oj_credentials(platform: String, handle: String) -> Result<LocalCreds, String> {
    let service = format!("vj-lite-{platform}");
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
    let plat_json = serde_json::to_string(&platform).map_err(|e| e.to_string())?;

    let init_js = format!(
        r#"
        window.addEventListener('DOMContentLoaded', () => {{
          const path = window.location.pathname;
          if (!path.includes('submit')) {{
            window.__TAURI__?.core?.invoke('oj_submit_finished', {{ platform: {plat_json} }});
            return;
          }}
          const form = document.querySelector('form.submit-form, form[action*="submit"]');
          if (!form) {{
            window.__TAURI__?.core?.invoke('show_oj_window');
            return;
          }}
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
    WebviewWindowBuilder::new(&app, "oj-submit", WebviewUrl::External(parsed_url))
        .title("OJ Background Submit")
        .visible(false)
        .initialization_script(&init_js)
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
            submit_via_webview,
            start_supabase_google_oauth
        ])
        .run(tauri::generate_context!())
        .expect("error while running VJ-Lite Desktop");
}
