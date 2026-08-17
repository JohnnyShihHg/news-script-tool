#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use news_script_core::config::Config;
use news_script_core::gemini::{self, GeminiConfig};
use news_script_core::model::{NewsEntry, Outcome};
use news_script_core::{import_files, parse};
use serde::{Deserialize, Serialize};
use std::sync::Mutex;
use tauri::{Emitter, Manager};
use tauri_plugin_dialog::DialogExt;

const KEYRING_SERVICE: &str = "news-script-tool";
const KEYRING_USER: &str = "gemini_api_key";
const COLLAB_WINDOW_LABEL: &str = "collab";
const COLLAB_URL: &str = "https://small-helper-4-20260724.ebclan1.chatgpt.site/helper4";

struct AppState {
    config: Mutex<Config>,
    /// Holds the reply channel for an in-flight scrape while we wait for the
    /// injected script running in the collab webview to call `receive_scraped_text`.
    scrape_reply: Mutex<Option<tokio::sync::oneshot::Sender<Result<String, String>>>>,
}

fn config_path() -> Option<std::path::PathBuf> {
    directories::ProjectDirs::from("tw", "ebc", "news-script-tool")
        .map(|d| d.config_dir().join("config.toml"))
}

fn load_config_from_disk_or_default() -> Config {
    let mut cfg = match config_path() {
        Some(path) if path.exists() => match news_script_core::config::load_from_path(&path) {
            Ok(cfg) => cfg,
            Err(_) => Config::default(),
        },
        _ => Config::default(),
    };

    // A saved list shadows its default entirely, so without this any newly shipped
    // default (a new style, a new refresh keyword) would never reach a machine that
    // already has a config file -- which is every machine that has opened settings.
    // Persist immediately so the merge happens once rather than on every launch.
    if news_script_core::config::migrate(&mut cfg) {
        if let Some(path) = config_path() {
            let _ = news_script_core::config::save_to_path(&cfg, &path);
        }
    }
    cfg
}

#[derive(Serialize, Clone)]
#[serde(tag = "kind")]
enum EntryDto {
    Passed(EntryFields),
    UnknownStyle(EntryFields),
    NeedsManualContent(EntryFields),
    FilteredByStyle(EntryFields),
    ParseFailed { file_name: String, reason: String },
}

#[derive(Serialize, Clone)]
struct EntryFields {
    file_name: String,
    slug: String,
    style: String,
    time: String,
    group: String,
    title: String,
    /// Label for the slug line; composed at output time so `slug` itself stays
    /// exactly as iNews wrote it and keeps matching against the shared doc.
    slug_marker: String,
    body: String,
    raw_title: String,
    raw_body: String,
    warnings: Vec<String>,
}

impl From<&NewsEntry> for EntryFields {
    fn from(e: &NewsEntry) -> Self {
        EntryFields {
            file_name: e.file_name.clone(),
            slug: e.slug.clone(),
            style: e.style.clone(),
            time: e.time.clone(),
            group: e.group.clone(),
            title: e.title.clone(),
            slug_marker: e.slug_marker.clone(),
            body: e.body.clone(),
            raw_title: e.raw_title.clone(),
            raw_body: e.raw_body.clone(),
            warnings: e.warnings.clone(),
        }
    }
}

#[derive(Serialize)]
struct ImportSummaryDto {
    total_files: usize,
    loaded: usize,
    passed: usize,
    filtered: usize,
    unknown: usize,
    needs_manual: usize,
    failed: usize,
    entries: Vec<EntryDto>,
}

/// Tauri v2 does not ACL-gate app-defined commands (only plugin commands go through
/// the capabilities system) -- and `withGlobalTauri` exposes `window.__TAURI__` to
/// every webview the app creates, including the `collab` window that loads a
/// third-party remote origin we don't control. Without this guard, that remote
/// page's own script could call any of these commands directly (read/write files,
/// read/change the Gemini API key, etc.), not just the scraper snippet we `eval`
/// into it. Every command below except `receive_scraped_text` must reject callers
/// that aren't the app's own main window.
fn require_main_window(window: &tauri::Window) -> Result<(), String> {
    if window.label() == "main" {
        Ok(())
    } else {
        Err("此操作僅限主視窗呼叫".to_string())
    }
}

fn read_txt_files(folder: &str) -> std::io::Result<Vec<(String, String)>> {
    let mut files = Vec::new();
    for entry in std::fs::read_dir(folder)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_file() && path.extension().and_then(|e| e.to_str()) == Some("txt") {
            let name = path.file_name().unwrap().to_string_lossy().to_string();
            let bytes = std::fs::read(&path)?;
            let text = parse::decode_and_normalize(&bytes);
            files.push((name, text));
        }
    }
    files.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(files)
}

#[tauri::command]
fn import_folder(window: tauri::Window, state: tauri::State<AppState>, folder: String) -> Result<ImportSummaryDto, String> {
    require_main_window(&window)?;
    let files = read_txt_files(&folder).map_err(|e| e.to_string())?;
    let total_files = files.len();
    let cfg = state.config.lock().unwrap().clone();
    let summary = import_files(&files, &cfg);

    let mut entries = Vec::new();
    for e in &summary.passed {
        entries.push(EntryDto::Passed(e.into()));
    }
    for o in &summary.unknown {
        if let Outcome::UnknownStyle(e) = o {
            entries.push(EntryDto::UnknownStyle(e.into()));
        }
    }
    for o in &summary.needs_manual {
        if let Outcome::NeedsManualContent(e) = o {
            entries.push(EntryDto::NeedsManualContent(e.into()));
        }
    }
    for o in &summary.filtered {
        if let Outcome::FilteredByStyle(e) = o {
            entries.push(EntryDto::FilteredByStyle(e.into()));
        }
    }
    for o in &summary.failed {
        if let Outcome::ParseFailed { file_name, reason } = o {
            entries.push(EntryDto::ParseFailed {
                file_name: file_name.clone(),
                reason: reason.clone(),
            });
        }
    }

    Ok(ImportSummaryDto {
        total_files,
        loaded: summary.loaded,
        passed: summary.passed.len(),
        filtered: summary.filtered.len(),
        unknown: summary.unknown.len(),
        needs_manual: summary.needs_manual.len(),
        failed: summary.failed.len(),
        entries,
    })
}

#[tauri::command]
async fn pick_folder(window: tauri::Window, app: tauri::AppHandle, start_dir: Option<String>) -> Result<Option<String>, String> {
    require_main_window(&window)?;
    let (tx, rx) = std::sync::mpsc::channel();
    let mut dialog = app.dialog().file();
    if let Some(dir) = start_dir.filter(|d| !d.trim().is_empty()) {
        dialog = dialog.set_directory(dir);
    }
    dialog.pick_folder(move |path| {
        let _ = tx.send(path);
    });
    Ok(rx.recv().ok().flatten().map(|p| p.to_string()))
}

/// Permanently deletes every top-level `.txt` file in `folder` (not subfolders, and
/// not non-txt files) so re-running import on the same folder can't waste Gemini
/// tokens re-processing already-exported entries. Irreversible by design — the
/// frontend is expected to confirm with the user before calling this.
#[tauri::command]
fn clear_folder(window: tauri::Window, folder: String) -> Result<usize, String> {
    require_main_window(&window)?;
    let path = std::path::Path::new(&folder);
    if !path.is_dir() {
        return Err("找不到這個資料夾".to_string());
    }
    let mut deleted = 0usize;
    for entry in std::fs::read_dir(path).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let p = entry.path();
        if p.is_file() && p.extension().and_then(|e| e.to_str()) == Some("txt") {
            std::fs::remove_file(&p).map_err(|e| e.to_string())?;
            deleted += 1;
        }
    }
    Ok(deleted)
}

#[tauri::command]
async fn save_text_file(
    window: tauri::Window,
    app: tauri::AppHandle,
    default_name: String,
    content: String,
) -> Result<bool, String> {
    require_main_window(&window)?;
    let (tx, rx) = std::sync::mpsc::channel();
    app.dialog()
        .file()
        .set_file_name(&default_name)
        .add_filter("Text", &["txt"])
        .save_file(move |path| {
            let _ = tx.send(path);
        });
    let path = rx.recv().map_err(|e| e.to_string())?;
    match path {
        Some(p) => {
            let path_buf = p.into_path().map_err(|e| e.to_string())?;
            std::fs::write(path_buf, content).map_err(|e| e.to_string())?;
            Ok(true)
        }
        None => Ok(false),
    }
}

// --- Settings: config file (non-secret) ---

#[tauri::command]
fn get_config(window: tauri::Window, state: tauri::State<AppState>) -> Result<Config, String> {
    require_main_window(&window)?;
    Ok(state.config.lock().unwrap().clone())
}

fn save_config_inner(state: &tauri::State<AppState>, config: Config) -> Result<(), String> {
    // Fail loudly on a broken regex rather than silently accepting a config that
    // would make every future import's title extraction fail.
    regex::Regex::new(&config.filter.title_tag_pattern).map_err(|e| format!("標題標記樣式（regex）錯誤：{e}"))?;
    regex::Regex::new(&config.filter.title_tag_fallback_pattern)
        .map_err(|e| format!("標題標記備援樣式（regex）錯誤：{e}"))?;

    let path = config_path().ok_or_else(|| "找不到設定檔存放路徑".to_string())?;
    news_script_core::config::save_to_path(&config, &path).map_err(|e| e.to_string())?;
    *state.config.lock().unwrap() = config;
    Ok(())
}

#[tauri::command]
fn save_config(window: tauri::Window, state: tauri::State<AppState>, config: Config) -> Result<(), String> {
    require_main_window(&window)?;
    save_config_inner(&state, config)
}

#[tauri::command]
fn reset_config(window: tauri::Window, state: tauri::State<AppState>) -> Result<Config, String> {
    require_main_window(&window)?;
    let default = Config::default();
    save_config_inner(&state, default.clone())?;
    Ok(default)
}

#[derive(Serialize)]
struct NormalizeResultDto {
    text: String,
    warnings: Vec<String>,
}

/// Runs the same punctuation pass import already applies, on text the user typed or
/// pasted by hand -- import-time normalization never touches anything edited after
/// the fact, so a manually filled-in body (a needs-manual-content entry, a rescued
/// filtered entry) would otherwise stay unnormalized forever.
#[tauri::command]
fn normalize_text(window: tauri::Window, state: tauri::State<AppState>, text: String) -> Result<NormalizeResultDto, String> {
    require_main_window(&window)?;
    let cfg = state.config.lock().unwrap();
    let result = news_script_core::punctuation::normalize(&text, &cfg.punctuation);
    Ok(NormalizeResultDto { text: result.text, warnings: result.warnings })
}

// --- Settings: Gemini API key (OS keyring, never written to the TOML config) ---

fn keyring_entry() -> Result<keyring::Entry, String> {
    keyring::Entry::new(KEYRING_SERVICE, KEYRING_USER).map_err(|e| e.to_string())
}

fn api_key_from_keyring() -> Option<String> {
    keyring_entry().ok()?.get_password().ok().filter(|k| !k.trim().is_empty())
}

fn resolve_api_key() -> Option<String> {
    api_key_from_keyring().or_else(gemini::api_key_from_env)
}

#[derive(Serialize)]
struct ApiKeyStatus {
    has_key: bool,
    source: String, // "keyring" | "env" | "none"
}

#[tauri::command]
fn get_api_key_status(window: tauri::Window) -> Result<ApiKeyStatus, String> {
    require_main_window(&window)?;
    Ok(if api_key_from_keyring().is_some() {
        ApiKeyStatus { has_key: true, source: "keyring".into() }
    } else if gemini::api_key_from_env().is_some() {
        ApiKeyStatus { has_key: true, source: "env".into() }
    } else {
        ApiKeyStatus { has_key: false, source: "none".into() }
    })
}

#[tauri::command]
fn set_api_key(window: tauri::Window, key: String) -> Result<(), String> {
    require_main_window(&window)?;
    if key.trim().is_empty() {
        return Err("API key 不能是空白".to_string());
    }
    keyring_entry()?.set_password(key.trim()).map_err(|e| e.to_string())
}

#[tauri::command]
fn clear_api_key(window: tauri::Window) -> Result<(), String> {
    require_main_window(&window)?;
    let entry = keyring_entry()?;
    match entry.delete_credential() {
        Ok(()) => Ok(()),
        Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(e.to_string()),
    }
}

// --- Gemini keyword generation ---

#[derive(Deserialize)]
struct KeywordRequest {
    id: String,
    title: String,
    body: String,
}

#[derive(Serialize, Clone)]
struct KeywordProgress {
    done: usize,
    total: usize,
    current: String,
}

#[derive(Serialize)]
struct KeywordResult {
    id: String,
    keywords: Option<Vec<String>>,
    error: Option<String>,
}

fn build_gemini_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .expect("failed to build HTTP client")
}

fn resolve_gemini_config(state: &tauri::State<AppState>) -> Result<GeminiConfig, String> {
    let api_key = resolve_api_key()
        .ok_or_else(|| "尚未設定 Gemini API key，請到設定頁輸入。".to_string())?;
    let model = state.config.lock().unwrap().gemini.model.clone();
    Ok(GeminiConfig { api_key, model })
}

#[tauri::command]
async fn generate_keywords(
    window: tauri::Window,
    state: tauri::State<'_, AppState>,
    title: String,
    body: String,
    count: usize,
) -> Result<Vec<String>, String> {
    require_main_window(&window)?;
    let gemini_cfg = resolve_gemini_config(&state)?;
    let client = build_gemini_client();
    gemini::generate_keywords(&client, &gemini_cfg, &title, &body, count)
        .await
        .map_err(|e| e.to_string())
}

/// Runs sequentially (not in parallel) so a burst of requests doesn't trip Gemini's
/// rate limit; a single entry's failure is captured per-item, not raised to abort
/// the whole batch (spec: "單則失敗不中斷整批").
#[tauri::command]
async fn generate_keywords_batch(
    window: tauri::Window,
    state: tauri::State<'_, AppState>,
    items: Vec<KeywordRequest>,
    count: usize,
) -> Result<Vec<KeywordResult>, String> {
    require_main_window(&window)?;
    let gemini_cfg = resolve_gemini_config(&state)?;
    let client = build_gemini_client();
    let total = items.len();
    let mut results = Vec::with_capacity(total);
    // Progress is derived from this loop, not from the API -- emitting it costs no
    // extra requests and therefore no extra tokens.
    for (idx, item) in items.into_iter().enumerate() {
        let _ = window.emit(
            "keyword-progress",
            KeywordProgress { done: idx, total, current: item.title.clone() },
        );
        let outcome = gemini::generate_keywords(&client, &gemini_cfg, &item.title, &item.body, count).await;
        results.push(match outcome {
            Ok(keywords) => KeywordResult { id: item.id, keywords: Some(keywords), error: None },
            Err(e) => KeywordResult { id: item.id, keywords: None, error: Some(e.to_string()) },
        });
    }
    let _ = window.emit(
        "keyword-progress",
        KeywordProgress { done: total, total, current: String::new() },
    );
    Ok(results)
}

// --- Phase 5: collaboration-tool webview + slug matching ---

#[derive(Serialize)]
struct MatchResultDto {
    slug: String,
    status: String, // "to_cut" | "keep_refresh" | "removed"
    matched_line: Option<String>,
}

#[tauri::command]
async fn open_collab_window(window: tauri::Window, app: tauri::AppHandle) -> Result<(), String> {
    require_main_window(&window)?;
    if let Some(w) = app.get_webview_window(COLLAB_WINDOW_LABEL) {
        w.show().map_err(|e| e.to_string())?;
        w.set_focus().map_err(|e| e.to_string())?;
        return Ok(());
    }
    let url = tauri::Url::parse(COLLAB_URL).map_err(|e| e.to_string())?;
    tauri::WebviewWindowBuilder::new(&app, COLLAB_WINDOW_LABEL, tauri::WebviewUrl::External(url))
        .title("同事協作工具")
        .inner_size(1100.0, 800.0)
        .build()
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Called by the script we `eval` into the collab webview once it has collected
/// the outline text; delivers it to whichever `compare_with_collab_doc` call is
/// currently waiting.
#[tauri::command]
fn receive_scraped_text(window: tauri::Window, state: tauri::State<AppState>, text: Result<String, String>) {
    if window.label() != COLLAB_WINDOW_LABEL {
        return;
    }
    // The connectivity probe pings this command to test the IPC path; that ping must
    // never be handed to a real scrape that happens to be in flight.
    if matches!(&text, Ok(t) if t == "__PROBE_PING__") {
        return;
    }
    if let Some(tx) = state.scrape_reply.lock().unwrap().take() {
        let _ = tx.send(text);
    }
}

/// Reads the WHOLE document via the Quill instance rather than the sidebar outline.
///
/// The outline is built from `quill.root.querySelectorAll("h1, h2, h3")`, so it only
/// ever lists heading-formatted lines. Anything inserted as plain text -- including
/// everything this app writes -- is invisible there, which made comparison silently
/// miss entries that were demonstrably already in the doc. `getText()` returns every
/// line regardless of formatting, which is also the flat text spec §6 assumes.
const SCRAPE_SCRIPT: &str = r#"
(function() {
  function send(payload) {
    if (!(window.__TAURI__ && window.__TAURI__.core && window.__TAURI__.core.invoke)) {
      console.error('[news-script-tool] window.__TAURI__.core.invoke unavailable in collab webview.', payload);
      return;
    }
    window.__TAURI__.core.invoke('receive_scraped_text', { text: payload })
      .catch((e) => console.error('[news-script-tool] receive_scraped_text invoke rejected', e));
  }
  try {
    if (typeof Quill === 'undefined' || !Quill.find) {
      send({ Err: '協作工具頁面上找不到 Quill，無法讀取文件內容。' });
      return;
    }
    var container = document.querySelector('#editor');
    if (!container) { send({ Err: '找不到編輯器容器 #editor。' }); return; }
    var q = Quill.find(container);
    if (!q || typeof q.getText !== 'function') {
      send({ Err: '取不到 Quill 編輯器實例，無法讀取文件內容。' });
      return;
    }
    var text = q.getText();
    if (!text || !text.trim()) {
      send({ Err: '協作工具文件目前是空的，請確認已經連上房間且文件已同步完成。' });
      return;
    }
    send({ Ok: text });
  } catch (e) {
    send({ Err: String(e) });
  }
})();
"#;

/// Probes the collab webview WITHOUT relying on the IPC bridge: the injected script
/// reports its findings by writing into `document.title`, which Rust can read back
/// directly. This distinguishes the three failure modes a plain timeout can't tell
/// apart -- eval never ran, `window.__TAURI__` isn't exposed on the remote origin,
/// or the DOM selector matched nothing.
#[tauri::command]
async fn diagnose_collab_bridge(window: tauri::Window, app: tauri::AppHandle) -> Result<String, String> {
    require_main_window(&window)?;
    let collab = app
        .get_webview_window(COLLAB_WINDOW_LABEL)
        .ok_or_else(|| "還沒開啟協作工具視窗，請先點「開啟協作工具」。".to_string())?;

    // Readback goes through `location.hash`, which Rust can observe via `url()`.
    // `document.title` is NOT usable here: a Tauri window's native title is set by us
    // at build time and does not track the page's `document.title`, so reading it back
    // would report "no change" even when the script ran fine. Setting a hash triggers
    // no navigation, no network request, and no document mutation, so the colleague's
    // live doc is untouched.
    // Phase 2 of the probe actually attempts the `invoke` that the real scraper relies
    // on, and reports whether the promise resolved or rejected -- that rejection reason
    // is the one thing a plain timeout can never show us.
    let probe = r#"
    (function() {
      function report(s) { location.hash = 'PROBE-' + encodeURIComponent(s); }
      try {
        var hasT = !!(window.__TAURI__);
        var hasCore = !!(window.__TAURI__ && window.__TAURI__.core);
        var hasInvoke = !!(window.__TAURI__ && window.__TAURI__.core && window.__TAURI__.core.invoke);
        var n = document.querySelectorAll('.outline-list .outline-item-label').length;
        var base = 'tauri=' + hasT + ' invoke=' + hasInvoke + ' nodes=' + n;
        if (!hasInvoke) { report(base + ' | invoke 不存在'); return; }
        window.__TAURI__.core.invoke('receive_scraped_text', { text: { Ok: '__PROBE_PING__' } })
          .then(function() { report(base + ' | invoke 成功'); })
          .catch(function(e) { report(base + ' | invoke 被拒: ' + String(e)); });
      } catch (e) {
        report('同步錯誤: ' + String(e));
      }
    })();
    "#;
    collab.eval(probe).map_err(|e| e.to_string())?;

    // The eval is fire-and-forget; give the webview a moment to actually run it.
    let mut found: Option<String> = None;
    for _ in 0..20 {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        if let Ok(u) = collab.url() {
            if let Some(frag) = u.fragment() {
                if frag.starts_with("PROBE-") {
                    found = Some(frag.to_string());
                    break;
                }
            }
        }
    }

    // Clear the probe hash again so the page is left as we found it.
    let _ = collab.eval("history.replaceState(null, '', location.pathname + location.search);");

    match found {
        Some(t) => Ok(percent_decode(t.trim_start_matches("PROBE-"))),
        None => Err("eval 完全沒有生效：注入協作工具視窗的腳本沒有執行（location.hash 沒有被改動）。".to_string()),
    }
}

/// Minimal percent-decoder for the probe's `encodeURIComponent` payload, so the
/// rejection reason comes back readable instead of as `%E5%...` noise.
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(b) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Scrapes the collab doc and hands back the raw text so it can be saved as a TXT
/// for eyeballing, without running any matching. Used to confirm step 1 (can we even
/// read the doc?) independently of the slug-matching logic.
#[tauri::command]
async fn export_collab_text(
    window: tauri::Window,
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
) -> Result<String, String> {
    require_main_window(&window)?;
    scrape_collab_text(&app, &state).await
}

/// Runs `script` inside the collab webview and waits for it to report back through
/// `receive_scraped_text`. Every script passed here must call that command on both
/// its success and failure paths, otherwise this call can only end in a timeout.
async fn eval_in_collab_and_wait(
    app: &tauri::AppHandle,
    state: &tauri::State<'_, AppState>,
    script: &str,
) -> Result<String, String> {
    let collab = app
        .get_webview_window(COLLAB_WINDOW_LABEL)
        .ok_or_else(|| "還沒開啟協作工具視窗，請先點「開啟協作工具」。".to_string())?;

    let (tx, rx) = tokio::sync::oneshot::channel();
    *state.scrape_reply.lock().unwrap() = Some(tx);

    collab.eval(script).map_err(|e| e.to_string())?;

    match tokio::time::timeout(std::time::Duration::from_secs(10), rx).await {
        Ok(Ok(Ok(text))) => Ok(text),
        Ok(Ok(Err(e))) => Err(e),
        Ok(Err(_)) => Err("與協作工具溝通時發生內部錯誤（channel closed）。".to_string()),
        Err(_) => {
            *state.scrape_reply.lock().unwrap() = None;
            Err("逾時（10 秒）。請按「診斷協作連線」查看實際原因。".to_string())
        }
    }
}

async fn scrape_collab_text(
    app: &tauri::AppHandle,
    state: &tauri::State<'_, AppState>,
) -> Result<String, String> {
    eval_in_collab_and_wait(app, state, SCRAPE_SCRIPT).await
}

/// Appends `text` to the bottom of the shared doc, mirroring the collab tool's own
/// `appendNotepadTextToArticle`: insert at `getLength() - 1`, separated by a blank
/// line, with source `"user"` so the QuillBinding propagates it through Yjs to the
/// server and every other connected editor. Nothing existing is replaced or deleted.
///
/// The page keeps its Quill instance in a module-scoped `const`, so it is not
/// reachable via `window`; `Quill.find()` on the editor container is what gets us a
/// handle on it.
fn build_append_script(text: &str) -> String {
    let payload = serde_json::to_string(text).unwrap_or_else(|_| "\"\"".into());
    format!(
        r#"
    (function() {{
      function send(payload) {{
        if (!(window.__TAURI__ && window.__TAURI__.core && window.__TAURI__.core.invoke)) return;
        window.__TAURI__.core.invoke('receive_scraped_text', {{ text: payload }})
          .catch(function(e) {{ console.error('[news-script-tool] reply failed', e); }});
      }}
      try {{
        if (typeof Quill === 'undefined' || !Quill.find) {{
          send({{ Err: '協作工具頁面上找不到 Quill，無法寫入。' }});
          return;
        }}
        var container = document.querySelector('#editor');
        if (!container) {{ send({{ Err: '找不到編輯器容器 #editor。' }}); return; }}
        var q = Quill.find(container);
        if (!q || typeof q.insertText !== 'function') {{
          send({{ Err: '取不到 Quill 編輯器實例，無法寫入。' }});
          return;
        }}
        var importedText = {payload};
        var insertIndex = Math.max(0, q.getLength() - 1);
        var existingText = q.getText(0, insertIndex);
        var separator = existingText.trim() ? '\n\n' : '';
        var textToInsert = separator + importedText;
        q.insertText(insertIndex, textToInsert, 'user');

        // Match the doc's own convention: the collab tool's 「自動標題」 turns any line
        // holding a timecode or a `===` divider into an h1, and its sidebar outline
        // lists *only* h1-h3. Inserting as plain text would leave our entries missing
        // from that outline, so colleagues could not navigate to them. Same pattern
        // and same formatLine call the page itself uses.
        var timecodePattern = /\d{{1,3}}\s*[:：]\s*[0-5]\d\s*[:：]\s*[0-5]\d/;
        var separatorPattern = /(?:[=＝]\s*){{3,}}/;
        var headingCount = 0;
        try {{
          var lines = q.getLines(insertIndex, textToInsert.length);
          lines.forEach(function (line) {{
            var lineStart = q.getIndex(line);
            var lineText = q.getText(lineStart, line.length()).replace(/\n$/, '');
            if (!timecodePattern.test(lineText) && !separatorPattern.test(lineText)) return;
            q.formatLine(lineStart, Math.max(1, lineText.length), 'header', 1, 'user');
            headingCount += 1;
          }});
        }} catch (fmtErr) {{
          console.error('[news-script-tool] heading formatting failed', fmtErr);
        }}

        var lineCount = importedText.split('\n').length;
        send({{ Ok: '已寫入 ' + lineCount + ' 行到文件最下方（其中 ' + headingCount + ' 行設為標題）。' }});
      }} catch (e) {{
        send({{ Err: String(e) }});
      }}
    }})();
    "#
    )
}

#[tauri::command]
async fn append_to_collab_doc(
    window: tauri::Window,
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    text: String,
) -> Result<String, String> {
    require_main_window(&window)?;
    if text.trim().is_empty() {
        return Err("沒有可寫入的內容。".to_string());
    }
    let script = build_append_script(&text);
    eval_in_collab_and_wait(&app, &state, &script).await
}

#[tauri::command]
async fn compare_with_collab_doc(
    window: tauri::Window,
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    slugs: Vec<String>,
) -> Result<Vec<MatchResultDto>, String> {
    require_main_window(&window)?;
    let doc_text = scrape_collab_text(&app, &state).await?;

    let refresh_keywords = state.config.lock().unwrap().markers.refresh_keywords.clone();
    let results = news_script_core::doc_match::match_all(
        slugs.iter().map(|s| s.as_str()),
        &doc_text,
        &refresh_keywords,
    );

    Ok(results
        .into_iter()
        .map(|(slug, r)| MatchResultDto {
            slug: slug.to_string(),
            status: match r.status {
                news_script_core::doc_match::MatchStatus::ToCut => "to_cut",
                news_script_core::doc_match::MatchStatus::KeepRefresh => "keep_refresh",
                news_script_core::doc_match::MatchStatus::Removed => "removed",
            }
            .to_string(),
            matched_line: r.matched_line,
        })
        .collect())
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .manage(AppState {
            config: Mutex::new(load_config_from_disk_or_default()),
            scrape_reply: Mutex::new(None),
        })
        .invoke_handler(tauri::generate_handler![
            import_folder,
            pick_folder,
            clear_folder,
            save_text_file,
            get_config,
            save_config,
            reset_config,
            get_api_key_status,
            set_api_key,
            clear_api_key,
            generate_keywords,
            generate_keywords_batch,
            normalize_text,
            open_collab_window,
            receive_scraped_text,
            compare_with_collab_doc,
            diagnose_collab_bridge,
            export_collab_text,
            append_to_collab_doc
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
