# 新聞文稿整理工具

把 iNews 匯出的 txt 稿件整理成可直接使用的文稿格式，比對協作文件避免重複，並用 Gemini 產生關鍵字。Tauri 桌面應用（Rust core + 原生 HTML/JS 前端）。

## 功能

- **匯入整理** — 讀取資料夾內的 iNews txt，解析表頭、依樣式過濾、正規化標點，輸出「表頭／標題／內文／關鍵字」四行格式
- **狀態比對** — 對照協作文件，標示每則是待處理、要重貼、已剔除或已在文件中，避免重複處理
- **關鍵字產生** — 呼叫 Gemini 產生關鍵字，已在文件中的稿件不會送出（不浪費額度）
- **寫回協作工具** — 將整理結果附加到協作文件最下方並套用標題格式，不覆蓋既有內容
- **自動更新** — 有新版時在 App 內一鍵完成

## 安裝

到 [Releases](../../releases) 下載最新的 `新聞文稿整理工具_x.y.z_x64-setup.exe` 執行。

首次啟動可能出現「Windows 已保護您的電腦」，點「其他資訊」→「仍要執行」即可。這是因為本程式未購買程式碼簽章憑證，屬預期行為。

安裝後每台電腦需各自設定一次：

1. **⚙ 設定 → Gemini API Key** — 存進 Windows 認證管理員，不會寫入明碼設定檔
2. **⚙ 設定 → 預設匯入資料夾** — 設定後每次開啟會自動匯入

## 開發

```bash
cargo test --workspace          # Rust：解析、清洗、標點、比對、Gemini 回應處理
cd app && node --test tests/logic.test.js   # UI 決策邏輯（不需 npm install）

cargo run -p news-script-tool   # 開發模式執行
```

### 專案結構

| 路徑 | 內容 |
|---|---|
| `core/` | 純函式核心：解析、清洗、標點正規化、輸出格式、slug 比對、Gemini 客戶端 |
| `app/src-tauri/` | Tauri 外殼：IPC 指令、設定、keyring、協作視窗 |
| `app/src/` | 前端。`logic.js` 是可測試的純決策邏輯，`app.js` 只負責接線與渲染 |
| `news-script-tool-spec.md` | 專案規格 |

### 測試資料

`core/tests/fixtures/` 是**合成檔案** —— 欄位結構與正式匯出相同，內容全部虛構。真實新聞稿含未播出內容、同事姓名與內部編號，部分標記「勿上網」，不進入這個公開 repo。新增 fixture 時請一併保持合成。

## 發布新版

1. 更新 `app/src-tauri/tauri.conf.json` 的 `version`
2. `git tag v0.2.0 && git push origin v0.2.0`
3. GitHub Actions 會跑測試、建置、簽章並發布 Release
4. 已安裝的使用者下次開啟就會收到更新提示

需要在 repo 設定兩個 secret：

- `TAURI_SIGNING_PRIVATE_KEY` — `.tauri/updater.key` 的內容
- `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` — 金鑰密碼（本專案為空字串）

> **私鑰遺失就無法再發布更新**，已安裝的版本會停在原地。請另外備份到密碼管理器。
