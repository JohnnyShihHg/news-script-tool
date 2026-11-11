#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod inews;
mod weather;

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
const KEYRING_INEWS_USER: &str = "inews_username";
const KEYRING_INEWS_PASS: &str = "inews_password";
const COLLAB_WINDOW_LABEL: &str = "collab";

/// 每個協作操作的流水號，用來確認回覆屬於哪一次請求。
static NEXT_COLLAB_REQUEST_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
const COLLAB_URL: &str = "https://small-helper-4-20260724.ebclan1.chatgpt.site/helper4";

/// 拿鎖，而且不怕「毒化」。
///
/// Rust 的 Mutex 會在「有人持鎖時 panic」之後把鎖標記成毒化，之後每次 `lock()` 都回
/// 傳 Err。原本每一處都寫 `.unwrap()`，等於毒化後每一個功能都會再 panic 一次 ——
/// 一次意外變成整個 App 永久故障，而且 `windows_subsystem = "windows"` 下沒有任何
/// 訊息，就是無聲當掉（v0.1.9 那次閃退的同一種死法）。
///
/// 這裡存的是設定、回覆通道、iNEWS 連線狀態這類「整個換掉」的值，不是改到一半就會
/// 壞掉的結構，所以毒化之後把裡面的值直接拿出來用是安全的。
pub fn lock_or_recover<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

struct AppState {
    config: Mutex<Config>,
    /// Holds the reply channel for an in-flight scrape while we wait for the
    /// injected script running in the collab webview to call `receive_scraped_text`,
    /// together with the id of the request that is waiting for it.
    ///
    /// 編號是為了擋「回覆送錯對象」：兩個協作操作重疊時，第二個等待者會收到第一個
    /// 腳本的回覆（例如比對收到「已寫入 12 行」當成文件內容，於是整份文件在比對眼中
    /// 只有一行，每一則都被判成不在文件裡 —— 然後整批重複寫進協作文件）。
    scrape_reply: Mutex<Option<(u64, tokio::sync::oneshot::Sender<Result<String, String>>)>>,
    /// 協作操作一次只跑一個。讀文件、寫入、校正時間都走同一條 eval→回覆的通道，
    /// 沒有這把鎖就會互搶。
    collab_turn: tokio::sync::Mutex<()>,
    /// iNEWS 的控制連線與稿件快取，跨輪保留。Arc 是因為 FTP 是阻塞的，要帶進
    /// `spawn_blocking`；Mutex 同時保證兩輪不會同時跑。
    inews: std::sync::Arc<Mutex<inews::InewsState>>,
    /// 比對寫進決策紀錄的上一份內容：一樣就不重複寫（自動刷新每分鐘都會比對）。
    last_compare_log: Mutex<String>,
    /// 時間校正跨輪要記得的東西（連續幾輪同一個新時間、已經記過的略過原因）。
    time_fix: Mutex<TimeFixState>,
}

#[derive(Default)]
struct TimeFixState {
    seen: news_script_core::doc_match::TimeStreaks,
    /// 今天已經寫進決策紀錄的 `[校正略過]`，同一個原因只記一次。
    logged: std::collections::HashSet<String>,
    day: String,
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
    /// Carries the full entry like every other bucket (reason first in `warnings`),
    /// so it can be ticked for output.
    ParseFailed(EntryFields),
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
    /// Only set by `import_inews`: which show was read and which story each entry is.
    #[serde(skip_serializing_if = "Option::is_none")]
    inews: Option<inews::InewsMeta>,
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
        // Case-insensitive: an export saved as `.TXT` used to be skipped without a word.
        if path.is_file() && path.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("txt")) {
            let name = path.file_name().unwrap().to_string_lossy().to_string();
            let bytes = std::fs::read(&path)?;
            let text = parse::decode_and_normalize(&bytes);
            files.push((name, text));
        }
    }
    files.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(files)
}

/// The shared half of every import: a list of (file name, txt text) goes through
/// `import_files` exactly as a folder of manual exports would, so iNEWS and folder
/// imports can never drift apart in how they judge a script.
async fn import_texts(files: &[(String, String)], cfg: &Config) -> ImportSummaryDto {
    let total_files = files.len();
    let mut summary = import_files(files, cfg);
    weather::fill_manual_weather(&mut summary.needs_manual).await;
    // 氣象稿沒內文也寫入（v0.3.11）：補完 0822 的內文之後，還是空的才移到通過。
    news_script_core::pass_weather_without_body(&mut summary);

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
        if let Outcome::ParseFailed { entry, .. } = o {
            entries.push(EntryDto::ParseFailed(entry.as_ref().into()));
        }
    }

    ImportSummaryDto {
        total_files,
        loaded: summary.loaded,
        passed: summary.passed.len(),
        filtered: summary.filtered.len(),
        unknown: summary.unknown.len(),
        needs_manual: summary.needs_manual.len(),
        failed: summary.failed.len(),
        entries,
        inews: None,
    }
}

#[tauri::command]
async fn import_folder(window: tauri::Window, state: tauri::State<'_, AppState>, folder: String) -> Result<ImportSummaryDto, String> {
    require_main_window(&window)?;
    let files = read_txt_files(&folder).map_err(|e| e.to_string())?;
    let cfg = lock_or_recover(&state.config).clone();
    Ok(import_texts(&files, &cfg).await)
}

// --- iNEWS FTP ---

fn inews_credentials() -> Result<inews::Credentials, String> {
    let get = |user: &str| {
        keyring::Entry::new(KEYRING_SERVICE, user)
            .ok()
            .and_then(|e| e.get_password().ok())
            .filter(|v| !v.is_empty())
    };
    match (get(KEYRING_INEWS_USER), get(KEYRING_INEWS_PASS)) {
        (Some(user), Some(pass)) => Ok(inews::Credentials { user, pass }),
        _ => Err("尚未設定 iNEWS 帳號密碼，請到設定頁輸入。".to_string()),
    }
}

/// 自動刷新的節次狀態存檔（重開 App 後接續換節收尾）：`%APPDATA%\\ebc\\news-script-tool\\state\\inews-session.json`。
fn inews_state_file() -> Option<std::path::PathBuf> {
    directories::ProjectDirs::from("tw", "ebc", "news-script-tool")
        .and_then(|d| d.config_dir().parent().map(|p| p.join("state").join("inews-session.json")))
}

/// 決策紀錄資料夾：`%APPDATA%\\ebc\\news-script-tool\\logs`（設定檔在同一層的 `config`）。
fn inews_log_dir() -> Option<std::path::PathBuf> {
    directories::ProjectDirs::from("tw", "ebc", "news-script-tool")
        .and_then(|d| d.config_dir().parent().map(|p| p.join("logs")))
}

/// 剪輯工具用的每節資料夾：`%APPDATA%\ebc\news-script-tool\rundown`，跟 `logs` 同一層。
/// 搜尋用的本地紀錄：`%APPDATA%\ebc\news-script-tool\search`，跟 `rundown` 同一層（v0.3.15）。
fn inews_search_dir() -> Option<std::path::PathBuf> {
    directories::ProjectDirs::from("tw", "ebc", "news-script-tool")
        .and_then(|d| d.config_dir().parent().map(|p| p.join("search")))
}

fn inews_rundown_dir() -> Option<std::path::PathBuf> {
    directories::ProjectDirs::from("tw", "ebc", "news-script-tool")
        .and_then(|d| d.config_dir().parent().map(|p| p.join("rundown")))
}

/// 從 iNEWS FTP 讀當節次的 rundown，把已播出、有效範圍內的稿件轉成跟手動「匯出
/// 延伸文稿」相同的 txt，再走跟 `import_folder` 完全相同的流程。`auto` = 自動刷新
/// 的一輪（會做換節最後同步與延遲補掃）；`session_start` = 剛打開自動刷新。
/// 每一輪（含失敗）都附加一行到決策紀錄。
#[tauri::command]
async fn import_inews(
    window: tauri::Window,
    state: tauri::State<'_, AppState>,
    auto: Option<bool>,
    session_start: Option<bool>,
) -> Result<ImportSummaryDto, String> {
    require_main_window(&window)?;
    let cfg = lock_or_recover(&state.config).clone();
    let creds = inews_credentials()?;
    let shared = state.inews.clone();
    let inews_cfg = cfg.inews.clone();
    let mode = if auto.unwrap_or(false) {
        inews::FetchMode::Auto { session_start: session_start.unwrap_or(false) }
    } else {
        inews::FetchMode::Manual
    };
    let log_dir = inews_log_dir();
    let state_file = inews_state_file();
    let rundown_dir = inews_rundown_dir();
    let search_dir = inews_search_dir();
    let (day, _) = inews::broadcast_days(inews::taipei_now(), &inews_cfg.broadcast_day_start);
    let fetched = tauri::async_runtime::spawn_blocking(move || {
        let mut st = shared.lock().unwrap_or_else(|p| p.into_inner());
        if let inews::FetchMode::Auto { session_start } = mode {
            // 只接續同一個播出日的狀態：隔天的同名節次是另一節。
            let saved = state_file
                .as_ref()
                .filter(|_| session_start)
                .and_then(|f| std::fs::read_to_string(f).ok())
                .and_then(|t| serde_json::from_str::<inews::SavedSession>(&t).ok())
                .filter(|s| s.day == day);
            st.set_resume(saved);
        }
        let mut r = st.fetch(&inews_cfg, &creds, inews::taipei_seconds_of_day(), mode);
        if let (Ok(_), inews::FetchMode::Auto { .. }, Some(f)) = (&r, mode, &state_file) {
            if let Ok(json) = serde_json::to_string(&st.saved(&day)) {
                let _ = f.parent().map(std::fs::create_dir_all);
                let _ = std::fs::write(f, json);
            }
        }
        if let Some(dir) = &log_dir {
            let tag = if mode == inews::FetchMode::Manual { "[手動]" } else { "[自動]" };
            match &r {
                Ok(f) => {
                    inews::append_log(dir, &format!("{tag} {}", f.log_line));
                    for line in &f.extra_logs {
                        inews::append_log(dir, line);
                    }
                    // 異常那一輪的前後兩輪 rundown（v0.3.11）：寫不進去只少一份診斷資料。
                    if let Some(json) = &f.anomaly_dump {
                        let stamp = inews::taipei_now().format("%Y%m%d-%H%M%S");
                        let _ = std::fs::write(dir.join(format!("anomaly-{stamp}.json")), json);
                    }
                }
                Err(e) => inews::append_log(dir, &format!("{tag} 錯誤：{e}")),
            }
        }
        // 剪輯工具用的 rundown JSON：寫不進去只留警告，不影響匯入。
        if let Ok(f) = &mut r {
            let snapshots = std::mem::take(&mut f.rundown);
            let written = rundown_dir.as_ref().map(|dir| {
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    inews::write_rundown(dir, inews::taipei_now(), &inews_cfg.broadcast_day_start, snapshots)
                }))
                .unwrap_or_else(|_| Err("寫入時發生未預期錯誤".to_string()))
            });
            if let (Some(Err(e)), Some(dir)) = (written, &log_dir) {
                inews::append_log(dir, &format!("[rundown] 警告：寫入 rundown JSON 失敗：{e}"));
            }
            // 搜尋用的本地紀錄（v0.3.15）：同樣寫不進去只留警告。
            let search = std::mem::take(&mut f.search);
            let written = search_dir.as_ref().map(|dir| {
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    inews::write_search(dir, inews::taipei_now(), &inews_cfg.broadcast_day_start, search)
                }))
                .unwrap_or_else(|_| Err("寫入時發生未預期錯誤".to_string()))
            });
            if let (Some(Err(e)), Some(dir)) = (written, &log_dir) {
                inews::append_log(dir, &format!("[搜尋紀錄] 警告：寫入失敗：{e}"));
            }
        }
        r
    })
    .await
    .map_err(|e| e.to_string())??;
    let mut dto = import_texts(&fetched.files, &cfg).await;
    dto.inews = Some(fetched.meta);
    Ok(dto)
}

#[derive(Serialize)]
struct SlugSearchDto {
    /// 播出日 `2026-11-11`。
    date: String,
    /// 當天的檔案裡有幾節、幾則稿（讓使用者知道資料涵蓋多少）。
    shows: usize,
    stories: usize,
    hits: Vec<news_script_core::search::SlugHit>,
}

/// 當天 rundown 的 slug 搜尋（v0.3.15）：讀 App 每一輪存的搜尋紀錄 `search\YYYYMMDD\index.json`
/// （跟剪輯工具的 rundown 檔分開，帶播出時間），只讀不寫，不打 iNEWS。只有 App 開著時讀過的
/// 節次會在裡面。
#[tauri::command]
fn search_rundown_slugs(
    window: tauri::Window,
    state: tauri::State<'_, AppState>,
    query: String,
) -> Result<SlugSearchDto, String> {
    require_main_window(&window)?;
    let day_start = lock_or_recover(&state.config).inews.broadcast_day_start.clone();
    let dir = inews_search_dir().ok_or_else(|| "找不到搜尋紀錄資料夾路徑".to_string())?;
    let date = inews::broadcast_date(inews::taipei_now(), &day_start);
    let path = dir.join(date.format("%Y%m%d").to_string()).join("index.json");
    let mut day = match std::fs::read_to_string(&path) {
        Ok(text) => news_script_core::search::SearchDay::from_json(&text).map_err(|e| format!("讀不懂 {}：{e}", path.display()))?,
        Err(_) => news_script_core::search::SearchDay::default(),
    };
    // 同一天更早的節次只在剪輯工具的 rundown 檔（搜尋紀錄是 0.3.16 之後才開始存的）：
    // 補進來一起搜，今天全部都找得到（v0.3.16）。只讀，讀不到就算了。
    if let Some(rundown) = inews_rundown_dir()
        .and_then(|dir| std::fs::read_to_string(dir.join(format!("{}.json", date.format("%Y%m%d")))).ok())
        .and_then(|text| news_script_core::rundown::RundownDay::from_json(&text).ok())
    {
        day.fill_from_rundown(&rundown);
    }
    let hits = news_script_core::search::search_slugs(&day, &query, |show| inews::show_day_order(&day_start, show));
    Ok(SlugSearchDto {
        date: date.format("%Y-%m-%d").to_string(),
        shows: day.shows.len(),
        stories: day.shows.values().map(Vec::len).sum(),
        hits,
    })
}


#[derive(Serialize)]
struct LoadedStoryDto {
    summary: ImportSummaryDto,
    /// 卡片用的 iNEWS 識別：story key、版本、所屬節次。
    key: String,
    version: String,
    show: String,
    /// 從哪裡讀到的：「本地紀錄」或「iNEWS」。
    source: String,
}

/// 搜尋結果「載入成卡片」（v0.3.15）：先讀搜尋紀錄裡存的原文（這一輪 App 讀過的同一個
/// 版本），沒有才到 iNEWS 那一節只讀這一則。照一般匯入的規則分類，回傳一張卡片的資料。
/// 累積時間用搜尋清單上的播出時間補（稿件本身的 cume-time 常是空的）。
#[tauri::command]
async fn load_rundown_story(
    window: tauri::Window,
    state: tauri::State<'_, AppState>,
    show: String,
    slug: String,
    id: String,
    version: String,
    air_time: String,
) -> Result<LoadedStoryDto, String> {
    require_main_window(&window)?;
    let cfg = lock_or_recover(&state.config).clone();
    let date = inews::broadcast_date(inews::taipei_now(), &cfg.inews.broadcast_day_start);
    let local = inews_search_dir().and_then(|dir| inews::read_search_story(&dir, date, &id, &version));
    let (story, key, story_version, source) = match local {
        Some(raw) => (news_script_core::inews::parse_nsml(&raw), id.clone(), version.clone(), "本地紀錄"),
        None => {
            let creds = inews_credentials()?;
            let shared = state.inews.clone();
            let inews_cfg = cfg.inews.clone();
            let (show2, id2, slug2) = (show.clone(), id.clone(), slug.clone());
            let found = tauri::async_runtime::spawn_blocking(move || {
                let mut st = shared.lock().unwrap_or_else(|p| p.into_inner());
                st.fetch_story(&inews_cfg, &creds, &show2, &id2, &slug2)
            })
            .await
            .map_err(|e| e.to_string())??;
            let Some((entry, story)) = found else {
                return Err(format!("本地沒有存到「{slug}」的原文，{show} 的 rundown 裡也已經找不到（可能被抽掉或搬到別節）。"));
            };
            (story, entry.story_key(), entry.version().to_string(), "iNEWS")
        }
    };
    let fill = (cfg.inews.fill_computed_time && !air_time.trim().is_empty()).then_some(air_time.trim());
    let text = news_script_core::inews::story_to_text(&story, fill);
    let file_name = news_script_core::inews::unique_file_names(&[story.title().to_string()]).remove(0);
    let summary = import_texts(&[(file_name, text)], &cfg).await;
    if let Some(dir) = inews_log_dir() {
        inews::append_log(&dir, &format!("[手動載入] {slug}（{show} {air_time}，{source}）"));
    }
    Ok(LoadedStoryDto { summary, key, version: story_version, show, source: source.to_string() })
}

/// 前端的決策寫進決策紀錄（v0.3.15：未知樣式審查）。只收一行，太長就截斷。
#[tauri::command]
fn append_decision_log(window: tauri::Window, line: String) -> Result<(), String> {
    require_main_window(&window)?;
    let line: String = line.replace(['\r', '\n'], " ").trim().chars().take(500).collect();
    if line.is_empty() {
        return Ok(());
    }
    let dir = inews_log_dir().ok_or_else(|| "找不到紀錄資料夾路徑".to_string())?;
    inews::append_log(&dir, &line);
    Ok(())
}

/// 「開啟紀錄資料夾」：在檔案總管打開決策紀錄所在的資料夾（沒有就先建立）。
#[tauri::command]
fn open_inews_log_folder(window: tauri::Window) -> Result<String, String> {
    require_main_window(&window)?;
    let dir = inews_log_dir().ok_or_else(|| "找不到紀錄資料夾路徑".to_string())?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("建立紀錄資料夾失敗：{e}"))?;
    let opener = if cfg!(target_os = "windows") {
        "explorer"
    } else if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    std::process::Command::new(opener).arg(&dir).spawn().map_err(|e| format!("開啟資料夾失敗：{e}"))?;
    Ok(dir.display().to_string())
}

#[tauri::command]
async fn test_inews_connection(window: tauri::Window, state: tauri::State<'_, AppState>) -> Result<String, String> {
    require_main_window(&window)?;
    let cfg = lock_or_recover(&state.config).inews.clone();
    let creds = inews_credentials()?;
    tauri::async_runtime::spawn_blocking(move || inews::test_connection(&cfg, &creds))
        .await
        .map_err(|e| e.to_string())?
}

#[derive(Serialize)]
struct InewsCredentialStatus {
    has_credentials: bool,
    username: String,
}

#[tauri::command]
fn get_inews_credential_status(window: tauri::Window) -> Result<InewsCredentialStatus, String> {
    require_main_window(&window)?;
    Ok(match inews_credentials() {
        Ok(c) => InewsCredentialStatus { has_credentials: true, username: c.user },
        Err(_) => InewsCredentialStatus { has_credentials: false, username: String::new() },
    })
}

#[tauri::command]
fn set_inews_credentials(window: tauri::Window, username: String, password: String) -> Result<(), String> {
    require_main_window(&window)?;
    if username.trim().is_empty() || password.is_empty() {
        return Err("帳號和密碼都不能是空白".to_string());
    }
    let set = |user: &str, value: &str| {
        keyring::Entry::new(KEYRING_SERVICE, user).and_then(|e| e.set_password(value)).map_err(|e| e.to_string())
    };
    set(KEYRING_INEWS_USER, username.trim())?;
    set(KEYRING_INEWS_PASS, &password)
}

#[tauri::command]
fn clear_inews_credentials(window: tauri::Window, state: tauri::State<AppState>) -> Result<(), String> {
    require_main_window(&window)?;
    for user in [KEYRING_INEWS_USER, KEYRING_INEWS_PASS] {
        let entry = keyring::Entry::new(KEYRING_SERVICE, user).map_err(|e| e.to_string())?;
        match entry.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => {}
            Err(e) => return Err(e.to_string()),
        }
    }
    // 下一輪要用新的帳密重新登入，不能沿用舊的連線。
    *state.inews.lock().unwrap_or_else(|p| p.into_inner()) = inews::InewsState::default();
    Ok(())
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
    Ok(lock_or_recover(&state.config).clone())
}

fn save_config_inner(state: &tauri::State<AppState>, config: Config) -> Result<(), String> {
    // Fail loudly on a broken regex rather than silently accepting a config that
    // would make every future import's title extraction fail.
    regex::Regex::new(&config.filter.title_tag_pattern).map_err(|e| format!("標題標記樣式（regex）錯誤：{e}"))?;
    regex::Regex::new(&config.filter.title_tag_fallback_pattern)
        .map_err(|e| format!("標題標記備援樣式（regex）錯誤：{e}"))?;
    if config.inews.server.trim().is_empty() {
        return Err("iNEWS 伺服器不能是空白".to_string());
    }
    if config.inews.poll_seconds < 3 {
        return Err("iNEWS 自動刷新間隔至少 3 秒".to_string());
    }
    if chrono::NaiveTime::parse_from_str(config.inews.broadcast_day_start.trim(), "%H:%M").is_err() {
        return Err(format!("播出日分界「{}」格式錯誤，要寫成 HH:MM，例：02:00", config.inews.broadcast_day_start));
    }
    for (from, to) in &config.inews.slot_spans {
        if news_script_core::inews::show_start(from).is_none() || news_script_core::inews::show_start(to).is_none() {
            return Err(format!("跨節合併「{from}={to}」格式錯誤，要寫成 4 位數節次，例：1200=1400"));
        }
    }

    let path = config_path().ok_or_else(|| "找不到設定檔存放路徑".to_string())?;
    news_script_core::config::save_to_path(&config, &path).map_err(|e| e.to_string())?;
    *lock_or_recover(&state.config) = config;
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
fn normalize_text(
    window: tauri::Window,
    state: tauri::State<AppState>,
    text: String,
    is_title: Option<bool>,
) -> Result<NormalizeResultDto, String> {
    require_main_window(&window)?;
    let cfg = lock_or_recover(&state.config);
    let result = if is_title.unwrap_or(false) {
        news_script_core::punctuation::normalize_title(&text, &cfg.punctuation)
    } else {
        news_script_core::punctuation::normalize(&text, &cfg.punctuation)
    };
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
    let model = lock_or_recover(&state.config).gemini.model.clone();
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

#[derive(Deserialize)]
struct GroupKeywordRequest {
    id: String,
    header: String,
    titles: Vec<String>,
}

#[derive(Serialize)]
struct GroupKeywordResult {
    id: String,
    keywords: Option<Vec<String>>,
    error: Option<String>,
}

/// 晚報: one request per group, each returning `count` keywords for the whole group
/// (its hashtag line) -- 9 stories in 2 groups cost 2 requests. Sequential for
/// the same rate-limit reason as `generate_keywords_batch`; one group failing does
/// not stop the others.
#[tauri::command]
async fn generate_group_keywords(
    window: tauri::Window,
    state: tauri::State<'_, AppState>,
    groups: Vec<GroupKeywordRequest>,
    count: usize,
) -> Result<Vec<GroupKeywordResult>, String> {
    require_main_window(&window)?;
    let gemini_cfg = resolve_gemini_config(&state)?;
    let client = build_gemini_client();
    let total = groups.len();
    let mut results = Vec::with_capacity(total);
    for (idx, group) in groups.into_iter().enumerate() {
        let _ = window.emit(
            "keyword-progress",
            KeywordProgress { done: idx, total, current: format!("第 {} 組", idx + 1) },
        );
        let outcome =
            gemini::generate_group_keywords(&client, &gemini_cfg, &group.header, &group.titles, count).await;
        results.push(match outcome {
            Ok(words) => GroupKeywordResult { id: group.id, keywords: Some(words), error: None },
            Err(e) => GroupKeywordResult { id: group.id, keywords: None, error: Some(e.to_string()) },
        });
    }
    let _ = window.emit("keyword-progress", KeywordProgress { done: total, total, current: String::new() });
    Ok(results)
}

// --- Phase 5: collaboration-tool webview + slug matching ---

#[derive(Serialize)]
struct MatchResultDto {
    slug: String,
    status: String, // "to_cut" | "in_doc" | "keep_refresh" | "removed"
    matched_line: Option<String>,
    /// 前一天段落裡 slug 和標題都相同：前一天已上傳的重播。
    cross_day: bool,
}

/// 比對時用的日期段落。
#[derive(Serialize)]
struct CompareDayDto {
    /// 今天（播出日）的 MMDD。
    today: String,
    /// 文件裡已經有今天的日期分隔線。
    today_found: bool,
    /// 今天的分隔線不存在、而且開了自動補上時，寫入前要先寫的那一行。
    header_to_add: Option<String>,
    /// 文件裡找不到任何日期分隔線（退回掃整份）等情況的警告。
    warning: Option<String>,
    /// 有開跨天比對、卻找不到前一天的日期段落：前一天的重播會被當新稿，
    /// 這一輪不自動寫入，改成等確認（v0.3.5）。
    hold_auto_write: bool,
}

#[derive(Serialize)]
struct CompareDto {
    results: Vec<MatchResultDto>,
    day: CompareDayDto,
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
        // 守門員（tools/collab-guard）每次載入都注入：App 的協作視窗是獨立的 WebView，
        // 沒有竄改猴（v0.3.15）。只內嵌同一份檔案，不另外維護一份。
        .initialization_script(COLLAB_GUARD_SCRIPT)
        .initialization_script(COLLAB_ACTIVITY_SCRIPT)
        .build()
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Called by the script we `eval` into the collab webview once it has collected
/// the outline text; delivers it to whichever `compare_with_collab_doc` call is
/// currently waiting.
#[tauri::command]
fn receive_scraped_text(
    window: tauri::Window,
    state: tauri::State<AppState>,
    text: Result<String, String>,
    id: Option<u64>,
) {
    if window.label() != COLLAB_WINDOW_LABEL {
        return;
    }
    // The connectivity probe pings this command to test the IPC path; that ping must
    // never be handed to a real scrape that happens to be in flight.
    if matches!(&text, Ok(t) if t == "__PROBE_PING__") {
        return;
    }
    let mut slot = lock_or_recover(&state.scrape_reply);
    // 編號對不上就丟掉：遲到的回覆（上一個操作逾時後才送達）絕不能交給正在等的那個
    // 操作，不然比對會拿到寫入的結果、寫入會拿到比對的結果。
    match slot.as_ref() {
        Some((waiting, _)) if Some(*waiting) == id => {}
        _ => return,
    }
    if let Some((_, tx)) = slot.take() {
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
/// 協作工具守門員（竄改猴腳本），原封不動注入 App 的協作視窗。開頭的 `==UserScript==`
/// 表頭只是註解；腳本自己會看頁面上有沒有編輯器才啟動。
const COLLAB_GUARD_SCRIPT: &str = include_str!("../../../tools/collab-guard/collab-guard.user.js");

/// 協作視窗裡記下使用者最後一次操作的時間（v0.3.15，操作中暫停用）。
const COLLAB_ACTIVITY_SCRIPT: &str = r#"
(function () {
  var mark = function () { window.__nstLastInputAt = Date.now(); };
  ['keydown', 'input', 'mousedown', 'wheel', 'paste', 'touchstart'].forEach(function (t) {
    window.addEventListener(t, mark, true);
  });
})();
"#;

/// 回報協作視窗多久沒有操作（秒）；開了之後都沒操作過回一個很大的數。
const COLLAB_IDLE_SCRIPT: &str = r#"
(function () {
  var t = window.__nstLastInputAt || 0;
  var secs = t ? Math.floor((Date.now() - t) / 1000) : 999999;
  if (window.__TAURI__ && window.__TAURI__.core && window.__TAURI__.core.invoke) {
    window.__TAURI__.core.invoke('receive_scraped_text', { text: { Ok: String(secs) }, id: window.__nstReqId })
      .catch(function () {});
  }
})();
"#;

/// 操作中暫停（v0.3.15）：協作視窗多久沒有操作（秒）。沒開協作視窗、別的協作操作正在
/// 進行、或 3 秒內問不到，都回 None（當作沒在操作），不排隊等，免得拖慢每一輪。
#[tauri::command]
async fn collab_idle_seconds(
    window: tauri::Window,
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
) -> Result<Option<u64>, String> {
    require_main_window(&window)?;
    if app.get_webview_window(COLLAB_WINDOW_LABEL).is_none() {
        return Ok(None);
    }
    let Ok(_turn) = state.collab_turn.try_lock() else { return Ok(None) };
    match tokio::time::timeout(std::time::Duration::from_secs(3), eval_in_collab_locked(&app, &state, COLLAB_IDLE_SCRIPT)).await {
        Ok(Ok(text)) => Ok(text.trim().parse::<u64>().ok()),
        _ => Ok(None),
    }
}

const SCRAPE_SCRIPT: &str = r#"
(function() {
  function send(payload) {
    if (!(window.__TAURI__ && window.__TAURI__.core && window.__TAURI__.core.invoke)) {
      console.error('[news-script-tool] window.__TAURI__.core.invoke unavailable in collab webview.', payload);
      return;
    }
    window.__TAURI__.core.invoke('receive_scraped_text', { text: payload, id: window.__nstReqId })
      .catch((e) => console.error('[news-script-tool] receive_scraped_text invoke rejected', e));
  }
  try {
    // 文件還沒讀完（新開或剛自動重整，停在「正在載入共同文章」）或沒連上時，讀到的是
    // 不完整的文件：拿去比對會把已經寫過的稿當成沒寫，重複寫入。回 [未就緒]，前端
    // 這一輪先跳過，下一輪再試，不停掉自動刷新（v0.3.15）。狀態文字在編輯器外面，
    // 文件內容裡剛好有這幾個字不算。
    var notReady = ['正在載入共同文章', '連線中', '離線'];
    var nodes = document.querySelectorAll('span, div, small, p');
    for (var i = 0; i < nodes.length; i++) {
      var n = nodes[i];
      if (n.children.length || (n.closest && n.closest('.ql-editor'))) continue;
      var t = (n.textContent || '').trim();
      for (var j = 0; j < notReady.length; j++) {
        if (t.indexOf(notReady[j]) === 0) {
          send({ Err: '[未就緒] 協作工具顯示「' + t + '」，文件還沒讀完或沒連上，這一輪先不比對、不寫入。' });
          return;
        }
      }
    }
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
    let _turn = collab_turn(state).await?;
    eval_in_collab_locked(app, state, script).await
}

/// 取得「協作操作」的排隊權。
///
/// 一次只跑一個協作操作：讀文件／寫入／校正共用同一條回覆通道，不排隊就會互搶。
/// 等不到就回報錯誤，不要無限等 —— tokio 的 Mutex 不可重入，萬一哪天有人把兩個協作
/// 操作寫成巢狀就會自己鎖死自己，那會是沒有任何訊息的永久卡住。
async fn collab_turn<'a>(
    state: &'a tauri::State<'_, AppState>,
) -> Result<tokio::sync::MutexGuard<'a, ()>, String> {
    match tokio::time::timeout(std::time::Duration::from_secs(30), state.collab_turn.lock()).await {
        Ok(guard) => Ok(guard),
        Err(_) => Err("另一個協作操作還在進行中（等了 30 秒）。請等它跑完再試；一直出現請回報。".to_string()),
    }
}

/// 真正做事的那一半，**呼叫前必須已經持有 `collab_turn`**。
///
/// 需要「讀文件 → 判斷 → 寫入」不被別人插隊的流程（例如寫入前的最後確認）就連續呼叫
/// 這個，整段只佔一次排隊權。直接呼叫 `eval_in_collab_and_wait` 會重新排隊，中間就有
/// 別的操作插進來的空隙。
async fn eval_in_collab_locked(
    app: &tauri::AppHandle,
    state: &tauri::State<'_, AppState>,
    script: &str,
) -> Result<String, String> {
    let collab = app
        .get_webview_window(COLLAB_WINDOW_LABEL)
        .ok_or_else(|| "還沒開啟協作工具視窗，請先點「開啟協作工具」。".to_string())?;

    let id = NEXT_COLLAB_REQUEST_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let (tx, rx) = tokio::sync::oneshot::channel();
    *lock_or_recover(&state.scrape_reply) = Some((id, tx));

    // 注入的腳本用 `window.__nstReqId` 把編號原封帶回來。
    collab
        .eval(&format!("window.__nstReqId = {id};\n{script}"))
        .map_err(|e| e.to_string())?;

    match tokio::time::timeout(std::time::Duration::from_secs(10), rx).await {
        Ok(Ok(Ok(text))) => Ok(text),
        Ok(Ok(Err(e))) => Err(e),
        Ok(Err(_)) => Err("與協作工具溝通時發生內部錯誤（channel closed）。".to_string()),
        Err(_) => {
            *lock_or_recover(&state.scrape_reply) = None;
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
        window.__TAURI__.core.invoke('receive_scraped_text', {{ text: payload, id: window.__nstReqId }})
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

        // 協作工具的同步有一道閘門：它只在 `bootstrapFinalized` 亮起時才排程上傳，
        // 沒亮就把改動堆在記憶體裡、而且不會再重試 —— 那一頁變成只收不發，外觀完全
        // 正常。它另外有一條不看那道閘門的「離開前補送」，掛在 window 的 pagehide 上。
        //
        // 所以每次寫完就補送一個 pagehide 事件，把剛寫的內容立刻推上伺服器，不要等
        // 使用者哪天想到去按 F5。那個監聽器不檢查事件是不是真的來自瀏覽器。
        //
        // 它的補送有 48,000 字元上限、超過就靜默放棄，所以這不是根治 —— 但每寫完就推
        // 一次，就不容易累積到那個上限。根治要請工具作者把上傳解開那道閘門。
        try {{ window.dispatchEvent(new Event('pagehide')); }} catch (nudgeErr) {{}}
        var lineCount = importedText.split('\n').length;
        send({{ Ok: '已寫入 ' + lineCount + ' 行到文件最下方（其中 ' + headingCount + ' 行設為標題）。' }});
      }} catch (e) {{
        send({{ Err: String(e) }});
      }}
    }})();
    "#
    )
}

/// 寫入前的最後確認結果，回給畫面與紀錄。
#[derive(Serialize)]
struct CheckedAppendDto {
    /// 協作工具回報的訊息；整批都已經在文件裡時是 None（沒有寫入動作）。
    message: Option<String>,
    /// 這次被濾掉的 slug 或分隔線 —— 多半是另一台電腦剛寫進去的。
    dropped: Vec<String>,
}

/// 「重新讀文件 → 濾掉已經在裡面的 → 寫入」，整段佔住同一次協作排隊權。
///
/// 解決的是兩台電腦同時開自動寫入的重複寫入：比對跟寫入之間隔著產生關鍵字的那幾十
/// 秒，對方在這段期間寫進去的東西這邊看不到。把確認挪到動手前一刻，危險窗就從幾十秒
/// 縮到「重新讀完到寫下去」之間。
///
/// 這是降低機率不是保證：Yjs 同步還有零點幾秒的延遲，兩台在那個瞬間交錯仍會重複。
/// 真正的保證只有「同一時間只開一台自動寫入」。
#[tauri::command]
async fn append_to_collab_doc_checked(
    window: tauri::Window,
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    parts: Vec<news_script_core::doc_match::WritePart>,
    header: Option<String>,
    log_note: Option<String>,
) -> Result<CheckedAppendDto, String> {
    require_main_window(&window)?;
    if parts.iter().all(|p| p.text.trim().is_empty()) {
        return Err("沒有可寫入的內容。".to_string());
    }
    let cfg = lock_or_recover(&state.config).clone();
    let (today, previous) = inews::broadcast_days(inews::taipei_now(), &cfg.inews.broadcast_day_start);

    let _turn = collab_turn(&state).await?;
    let doc_text = eval_in_collab_locked(&app, &state, SCRAPE_SCRIPT).await?;
    let filtered = news_script_core::doc_match::filter_write_parts(
        &parts,
        header.as_deref(),
        &doc_text,
        &today,
        &previous,
        &cfg.markers.refresh_keywords,
    );

    let log = |line: String| {
        if let Some(dir) = inews_log_dir() {
            inews::append_log(&dir, &line);
        }
    };
    if !filtered.dropped.is_empty() {
        log(format!("[寫入] 寫入前重新比對，濾掉已在文件裡的：{}", filtered.dropped.join("、")));
    }
    if filtered.text.trim().is_empty() {
        if let Some(note) = &log_note {
            log(format!("[寫入] 整批都已經在文件裡，沒有寫入：{note}"));
        }
        return Ok(CheckedAppendDto { message: None, dropped: filtered.dropped });
    }

    let r = eval_in_collab_locked(&app, &state, &build_append_script(&filtered.text)).await;
    if let Some(note) = &log_note {
        match &r {
            Ok(_) => log(format!("[寫入] 成功：{note}")),
            Err(e) => log(format!("[寫入] 失敗：{note}｜{e}")),
        }
    }
    Ok(CheckedAppendDto { message: Some(r?), dropped: filtered.dropped })
}

#[tauri::command]
async fn append_to_collab_doc(
    window: tauri::Window,
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    text: String,
    log_note: Option<String>,
) -> Result<String, String> {
    require_main_window(&window)?;
    if text.trim().is_empty() {
        return Err("沒有可寫入的內容。".to_string());
    }
    let script = build_append_script(&text);
    let r = eval_in_collab_and_wait(&app, &state, &script).await;
    // 自動流程的寫入結果寫進決策紀錄：「沒寫入」時才查得到原因。
    if let (Some(note), Some(dir)) = (&log_note, inews_log_dir()) {
        match &r {
            Ok(_) => inews::append_log(&dir, &format!("[寫入] 成功：{note}")),
            Err(e) => inews::append_log(&dir, &format!("[寫入] 失敗：{note}｜{e}")),
        }
    }
    r
}

/// 把協作文件裡幾行的時間換掉（v0.3.9）。每一行先重新定位：第 `line` 行的內容必須
/// 跟讀文件時完全相同、舊時間還在原位，才只把那幾個字換掉（保留行內格式和該行的
/// 標題格式），source 用 `"user"` 讓 Yjs 同步給其他人。對不上就放棄那一行。
fn build_correct_script(fixes: &[news_script_core::doc_match::TimeCorrection]) -> String {
    let payload = serde_json::to_string(fixes).unwrap_or_else(|_| "[]".into());
    format!(
        r#"
    (function() {{
      function send(payload) {{
        if (!(window.__TAURI__ && window.__TAURI__.core && window.__TAURI__.core.invoke)) return;
        window.__TAURI__.core.invoke('receive_scraped_text', {{ text: payload, id: window.__nstReqId }})
          .catch(function(e) {{ console.error('[news-script-tool] reply failed', e); }});
      }}
      try {{
        if (typeof Quill === 'undefined' || !Quill.find) {{
          send({{ Err: '協作工具頁面上找不到 Quill，無法校正時間。' }});
          return;
        }}
        var container = document.querySelector('#editor');
        if (!container) {{ send({{ Err: '找不到編輯器容器 #editor。' }}); return; }}
        var q = Quill.find(container);
        if (!q || typeof q.updateContents !== 'function') {{
          send({{ Err: '取不到 Quill 編輯器實例，無法校正時間。' }});
          return;
        }}
        var Delta = Quill.import('delta');
        var fixes = {payload};
        var results = fixes.map(function (f) {{
          try {{
            var line = q.getLines()[f.line];
            if (!line) return {{ ok: false, reason: '文件沒有第 ' + (f.line + 1) + ' 行了' }};
            var start = q.getIndex(line);
            var text = q.getText(start, Math.max(0, line.length() - 1));
            if (text !== f.line_text) return {{ ok: false, reason: '該行內容已變動：' + text }};
            if (text.substr(f.offset, f.old_time.length) !== f.old_time) return {{ ok: false, reason: '舊時間不在原位' }};
            var at = start + f.offset;
            var ops = q.getContents(at, f.old_time.length).ops;
            var attrs = (ops[0] && ops[0].attributes) || {{}};
            q.updateContents(new Delta().retain(at).delete(f.old_time.length).insert(f.new_time, attrs), 'user');
            var expected = text.slice(0, f.offset) + f.new_time + text.slice(f.offset + f.old_time.length);
            if (q.getText(start, expected.length) !== expected) return {{ ok: false, reason: '改完後內容跟預期不同' }};
            return {{ ok: true }};
          }} catch (e) {{
            return {{ ok: false, reason: String(e) }};
          }}
        }});
        // 跟寫入同一個理由：補送一個 pagehide，讓校正好的時間立刻推上伺服器。
        // （為什麼需要這招，見 build_append_script 裡的說明。）
        try {{ window.dispatchEvent(new Event('pagehide')); }} catch (nudgeErr) {{}}
        send({{ Ok: JSON.stringify(results) }});
      }} catch (e) {{
        send({{ Err: String(e) }});
      }}
    }})();
    "#
    )
}

#[derive(Deserialize)]
struct CorrectReply {
    ok: bool,
    #[serde(default)]
    reason: Option<String>,
}

#[derive(Serialize, Default)]
struct CorrectDto {
    /// `slug 舊 → 新`，這一輪改好的。
    corrected: Vec<String>,
    /// 要改但改的時候內容不符，下一輪重判。
    failed: Vec<String>,
    /// 新時間還沒連續出現夠多輪，先等。
    waiting: usize,
}

/// 時間校正（v0.3.9）：rundown 已播出的列（`InewsMeta::time_rows`）對今天段落，時間
/// 不同、而且同一個新時間連續 `correct_time_stable_rounds` 輪都一樣的行就改掉。
#[tauri::command]
async fn correct_collab_times(
    window: tauri::Window,
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    rows: Vec<news_script_core::doc_match::TimeRow>,
) -> Result<CorrectDto, String> {
    use news_script_core::doc_match::{plan_time_corrections, stable_corrections, streak_key};
    require_main_window(&window)?;
    let cfg = lock_or_recover(&state.config).clone();
    if !cfg.inews.correct_time {
        return Ok(CorrectDto::default());
    }
    let (today, _) = inews::broadcast_days(inews::taipei_now(), &cfg.inews.broadcast_day_start);
    let plan = if rows.is_empty() {
        Default::default()
    } else {
        plan_time_corrections(&scrape_collab_text(&app, &state).await?, &today, &rows, &cfg.markers.refresh_keywords, cfg.inews.correct_time_max_delta_minutes)
    };
    let log_dir = inews_log_dir();
    let log = |line: String| {
        if let Some(dir) = &log_dir {
            inews::append_log(dir, &line);
        }
    };

    let (ready, waiting) = {
        let mut fix = lock_or_recover(&state.time_fix);
        if fix.day != today {
            fix.day = today.clone();
            fix.logged.clear();
        }
        let r = stable_corrections(&mut fix.seen, &plan.corrections, cfg.inews.correct_time_stable_rounds);
        for s in &plan.skips {
            if fix.logged.insert(s.clone()) {
                log(format!("[校正略過] {s}"));
            }
        }
        r
    };

    let mut dto = CorrectDto { waiting, ..Default::default() };
    if ready.is_empty() {
        return Ok(dto);
    }
    let reply = eval_in_collab_and_wait(&app, &state, &build_correct_script(&ready)).await?;
    let results: Vec<CorrectReply> = serde_json::from_str(&reply).map_err(|e| format!("校正結果無法解讀：{e}"))?;
    let mut fix = lock_or_recover(&state.time_fix);
    for (c, r) in ready.iter().zip(results) {
        let what = format!("{} {} → {}", c.slug, c.old_time, c.new_time);
        if r.ok {
            log(format!("[校正] {what}"));
            fix.seen.remove(&streak_key(c));
            dto.corrected.push(what);
        } else {
            let reason = r.reason.unwrap_or_default();
            log(format!("[校正略過] {}：內容不符，下一輪重新判斷（{what}；{reason}）", c.slug));
            dto.failed.push(format!("{what}（{reason}）"));
        }
    }
    Ok(dto)
}

#[derive(Deserialize)]
struct CompareEntry {
    slug: String,
    #[serde(default)]
    time: String,
    /// 跨天比對用：前一天段落裡同一個 slug，標題也相同才算重播。
    #[serde(default)]
    title: String,
    /// 這張卡片算在哪一節（`1800`）：沒有累積時間時判斷 (抓18) 用（v0.3.20）。
    #[serde(default)]
    show: String,
}

#[tauri::command]
async fn compare_with_collab_doc(
    window: tauri::Window,
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    entries: Vec<CompareEntry>,
) -> Result<CompareDto, String> {
    use news_script_core::doc_match::{day_sections, match_in_days, normalize_title, previous_day_lines, EntryQuery, MatchStatus};
    require_main_window(&window)?;
    let doc_text = scrape_collab_text(&app, &state).await?;

    let cfg = lock_or_recover(&state.config).clone();
    let (today, previous) = inews::broadcast_days(inews::taipei_now(), &cfg.inews.broadcast_day_start);
    let days = day_sections(&doc_text, &today, &previous);
    // 工具自己加在標題前面的前綴和 slug 標記，跨天比對標題時不算。
    let a = &cfg.annotations;
    let strip: Vec<String> = [&a.latest_prefix, &a.exclusive_prefix, &a.no_upload_label, &a.copyright_label, &a.allowed_upload_label]
        .into_iter()
        .cloned()
        .collect();
    let title_strip = cfg.inews.cross_day_title_match.then_some(strip.as_slice());

    let hold_auto_write = days.has_dividers && title_strip.is_some() && days.previous.is_none();
    let warning = if !days.has_dividers {
        Some("協作文件裡找不到任何日期分隔線（==========MMDD==========），改成比對整份文件：跨天重複使用的 slug 可能被誤判為已上傳。".to_string())
    } else if hold_auto_write {
        Some(format!("協作文件裡找不到前一天 {previous} 的日期分隔線：前一天已上傳的重播會被當成新稿，這一輪不自動寫入，請確認後手動寫入。"))
    } else {
        None
    };
    let header_to_add = match (&days.today, cfg.inews.auto_date_header) {
        (None, true) => days.today_header.clone(),
        _ => None,
    };

    // Results come back in the order sent: the UI pairs them by position, since two
    // versions of one slug (different 累積時間) can get different answers.
    let mut counts = [0usize; 5]; // 待處理、已在文件、要重貼、已剔除、其中跨天
    let mut suspects = Vec::new();
    let results: Vec<MatchResultDto> = entries
        .iter()
        .map(|e| {
            let q = EntryQuery { slug: &e.slug, time: &e.time, title: &e.title, show: &e.show };
            let r = match_in_days(q, &days, &doc_text, &cfg.markers.refresh_keywords, title_strip);
            counts[match r.status {
                MatchStatus::ToCut => 0,
                MatchStatus::InDoc => 1,
                MatchStatus::KeepRefresh => 2,
                MatchStatus::Removed => 3,
            }] += 1;
            counts[4] += r.cross_day as usize;
            // 前一天有同 slug 卻當新稿：把兩邊的標題寫出來，查跨天比對為什麼沒對上。
            if r.status == MatchStatus::ToCut {
                let prev = previous_day_lines(&e.slug, &days, &strip);
                if !prev.is_empty() {
                    let theirs: Vec<String> = prev.iter().map(|(line, t)| format!("「{line}」→{t}")).collect();
                    suspects.push(format!(
                        "{}｜這則標題「{}」→{}｜前一天 {}",
                        e.slug,
                        e.title,
                        normalize_title(&e.title, &strip),
                        theirs.join("、")
                    ));
                }
            }
            MatchResultDto {
                slug: e.slug.clone(),
                status: match r.status {
                    MatchStatus::ToCut => "to_cut",
                    MatchStatus::InDoc => "in_doc",
                    MatchStatus::KeepRefresh => "keep_refresh",
                    MatchStatus::Removed => "removed",
                }
                .to_string(),
                matched_line: r.matched_line,
                cross_day: r.cross_day,
            }
        })
        .collect();

    let found = |s: &Option<String>| s.as_ref().map_or("找不到".to_string(), |t| format!("{} 行", t.lines().count()));
    let mut log = vec![format!(
        "[比對] 播出日 {today}：{}・前一天 {previous}：{}・跨天標題比對{}・文件 {} 行｜{} 則：待處理 {}、已在文件 {}、要重貼 {}、已剔除 {}（其中跨天 {}）",
        found(&days.today),
        found(&days.previous),
        if title_strip.is_some() { "開" } else { "關" },
        doc_text.lines().count(),
        entries.len(),
        counts[0],
        counts[1],
        counts[2],
        counts[3],
        counts[4],
    )];
    if let Some(w) = &warning {
        log.push(format!("[比對] 警告：{w}"));
    }
    log.extend(suspects.iter().map(|s| format!("[比對] 前一天有同 slug，當新稿寫：{s}")));
    let log = log.join("\n");
    let mut last = lock_or_recover(&state.last_compare_log);
    if *last != log {
        if let Some(dir) = inews_log_dir() {
            for line in log.lines() {
                inews::append_log(&dir, line);
            }
        }
        *last = log;
    }
    drop(last);

    Ok(CompareDto {
        results,
        day: CompareDayDto { today, today_found: days.today.is_some(), header_to_add, warning, hold_auto_write },
    })
}

// --- 晚報 (evening bulletin) list ---

#[derive(Serialize)]
struct EveningMatchDto {
    slug: String,
    status: String, // "hit" | "no_online" | "not_found"
    matched_line: Option<String>,
}

/// Matches evening-rundown slugs against one day's block of the shared doc. Read-only:
/// the evening list is never written anywhere, it is only copied or saved as txt.
#[tauri::command]
async fn compare_evening(
    window: tauri::Window,
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    slugs: Vec<String>,
    mmdd: String,
) -> Result<Vec<EveningMatchDto>, String> {
    use news_script_core::doc_match::{date_section, match_evening, EveningStatus};
    require_main_window(&window)?;
    let doc_text = scrape_collab_text(&app, &state).await?;
    let section = date_section(&doc_text, &mmdd)
        .ok_or_else(|| format!("協作文件裡找不到 {mmdd} 的日期分隔線（========{mmdd}========），請確認日期。"))?;
    let markers = lock_or_recover(&state.config).evening.no_online_markers.clone();
    Ok(slugs
        .into_iter()
        .map(|slug| {
            let r = match_evening(&slug, &section, &markers);
            EveningMatchDto {
                slug,
                status: match r.status {
                    EveningStatus::Hit => "hit",
                    EveningStatus::NoOnline => "no_online",
                    EveningStatus::NotFound => "not_found",
                }
                .to_string(),
                matched_line: r.matched_line,
            }
        })
        .collect())
}

/// The evening cards and edits, kept beside config.toml so they survive a restart.
/// Stored as the front end's own JSON: only the front end reads it, and keeping it
/// opaque here means a new card field never needs a matching Rust change.
fn evening_state_path() -> Option<std::path::PathBuf> {
    config_path().map(|p| p.with_file_name("evening.json"))
}

#[tauri::command]
fn load_evening_state(window: tauri::Window) -> Result<Option<String>, String> {
    require_main_window(&window)?;
    match evening_state_path() {
        Some(p) if p.exists() => std::fs::read_to_string(p).map(Some).map_err(|e| e.to_string()),
        _ => Ok(None),
    }
}

#[tauri::command]
fn save_evening_state(window: tauri::Window, json: String) -> Result<(), String> {
    require_main_window(&window)?;
    let path = evening_state_path().ok_or_else(|| "找不到設定檔存放路徑".to_string())?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    // Write-then-rename so a crash mid-write can't leave a truncated file behind.
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, json).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, &path).map_err(|e| e.to_string())
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .manage(AppState {
            config: Mutex::new(load_config_from_disk_or_default()),
            scrape_reply: Mutex::new(None),
            collab_turn: tokio::sync::Mutex::new(()),
            inews: Default::default(),
            last_compare_log: Mutex::new(String::new()),
            time_fix: Default::default(),
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
            append_to_collab_doc,
            append_to_collab_doc_checked,
            compare_evening,
            generate_group_keywords,
            load_evening_state,
            save_evening_state,
            import_inews,
            test_inews_connection,
            get_inews_credential_status,
            set_inews_credentials,
            clear_inews_credentials,
            open_inews_log_folder,
            correct_collab_times,
            append_decision_log,
            collab_idle_seconds,
            search_rundown_slugs,
            load_rundown_story
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[cfg(test)]
mod lock_tests {
    use super::lock_or_recover;
    use std::sync::{Arc, Mutex};

    /// 回歸：毒化之後還是要能拿到鎖。
    ///
    /// 舊寫法 `lock().unwrap()` 在這裡會 panic，而且是每一次都 panic —— 設定、匯入、
    /// 產生關鍵字全部跟著死，App 等於變磚只能重開。
    #[test]
    fn a_poisoned_lock_still_hands_back_its_value() {
        let m = Arc::new(Mutex::new(String::from("設定還在")));

        // 製造毒化：另一條執行緒持鎖時 panic。
        let m2 = Arc::clone(&m);
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let _ = std::thread::spawn(move || {
            let _guard = m2.lock().unwrap();
            panic!("模擬持鎖時出事");
        })
        .join();
        std::panic::set_hook(previous);

        assert!(m.lock().is_err(), "這把鎖現在應該是毒化狀態");
        assert_eq!(*lock_or_recover(&m), "設定還在");
        // 毒化不會自己消失，所以第二次也要能拿到。
        assert_eq!(*lock_or_recover(&m), "設定還在");
    }
}
