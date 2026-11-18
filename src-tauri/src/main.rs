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

fn main() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![
            save_oj_credentials,
            get_oj_credentials,
            show_oj_window,
            oj_submit_finished,
            submit_via_webview
        ])
        .run(tauri::generate_context!())
        .expect("error while running VJ-Lite Desktop");
}
