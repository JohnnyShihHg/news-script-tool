# 新聞文稿整理工具

把 iNews 匯出的 txt 稿件整理成可直接使用的文稿格式，比對協作文件避免重複，並用 Gemini 產生關鍵字。Tauri 桌面應用（Rust core + 原生 HTML/JS 前端）。

## 功能

- **匯入整理** — 讀取資料夾內的 iNews txt，解析表頭、依樣式過濾、正規化標點，輸出「表頭／標題／內文／關鍵字」四行格式
- **狀態比對** — 對照協作文件，標示每則是待處理、要重貼、已剔除或已在文件中，避免重複處理
- **關鍵字產生** — 呼叫 Gemini 產生關鍵字，已在文件中的稿件不會送出（不浪費額度）
- **寫回協作工具** — 將整理結果附加到協作文件最下方並套用標題格式，不覆蓋既有內容
- **從 iNEWS 自動匯入** — 透過 iNEWS 內建 FTP 讀當節次 ONAIR rundown，已播出的稿件轉成跟「匯出延伸文稿」相同的 txt 後走同一套流程；「自動刷新」會定時重讀，有新稿或內容變動就自動比對、產生關鍵字並寫入（預設等你按確認）
- **自動更新** — 有新版時在 App 內一鍵完成

## 安裝

到 [Releases](../../releases) 下載最新的 `新聞文稿整理工具_x.y.z_x64-setup.exe` 執行。

首次啟動可能出現「Windows 已保護您的電腦」，點「其他資訊」→「仍要執行」即可。這是因為本程式未購買程式碼簽章憑證，屬預期行為。

安裝後每台電腦需各自設定一次：

1. **⚙ 設定 → Gemini API Key** — 存進 Windows 認證管理員，不會寫入明碼設定檔
2. **⚙ 設定 → 預設匯入資料夾** — 設定後每次開啟會自動匯入
3. **⚙ 設定 → iNEWS 自動匯入**（要用 iNEWS 匯入才需要）— 輸入 iNEWS 帳號密碼按「儲存」，再按「測試連線」。帳密同樣存進 Windows 認證管理員，不會寫進設定檔

### 從 iNEWS 匯入的規則

- **節次**：`51新聞台.SHOWS` 底下 4 位數資料夾，選已過整點的最近一節；下一節 rundown 的第一個硬時間（＠）一到就提前換過去（0900 那節 08:55 開始），下一節沒有硬時間就在整點前 `lead_minutes` 分鐘換。`2400` 是午夜那節。
- **截止點**：最後一則標題含「預告」的稿之後，第一個序號以 `=` 開頭的列（例 `=CM 3==`）以下是預備稿，不處理。找不到就用最後一個 `=` 列；再找不到就整節處理並顯示警告。
- **已播出**：照 iNEWS 客戶端的方式算累積時間（從硬時間開始累加總長度，float 不佔時間），累積時間 ≤ 現在才處理。之後的稿一定還會變動，不會自動處理。
- **float 列**（版型、框、SOU）預設不匯入。
- 每輪只 LIST 一次，只重新讀取 ID 時間戳變了的稿件。寫入協作文件前一律先比對；協作工具視窗沒開或沒登入時自動刷新會停下來提示。同一則稿件的同一個版本不會寫入兩次。

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
2. `git tag v0.2.1 && git push origin v0.2.1`
3. GitHub Actions 會跑測試、建置、簽章並發布 Release
4. 已安裝的使用者下次開啟就會收到更新提示

### ⚠ `productName` 必須維持純 ASCII 且不含空格

`tauri.conf.json` 的 `productName` 會直接變成安裝檔檔名。GitHub Release 在儲存 asset 時**會剝除非 ASCII 字元、並把空格轉成點**，檔名一旦被改寫，`tauri-action` 就無法把 `.sig` 對應回它的安裝檔，於是**靜默跳過 `latest.json`**（log 只留一行 `Signature not found for the updater JSON. Skipping upload...`），自動更新整個失效但 CI 仍顯示成功。

v0.1.0 就是這樣壞的。中文名稱改放在視窗標題（`app.windows[].title`），使用者實際看到的仍是「新聞文稿整理工具」。

需要在 repo 設定兩個 secret：

- `TAURI_SIGNING_PRIVATE_KEY` — `.tauri/updater.key` 的內容
- `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` — 金鑰密碼（本專案為空字串）

> **私鑰遺失就無法再發布更新**，已安裝的版本會停在原地。請另外備份到密碼管理器。
