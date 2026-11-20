# nst-dev 專案檢證報告（第 1～4 階段）

> 依 `project-audit-playbook.md` 執行，日期 2026-11-20，對象 commit `009f4aa`（版本 0.3.40）。
> 本次只讀：除了這份報告沒有改任何檔案、沒有裝套件、沒有 commit／push／tag。
> 沒有執行任何連 iNEWS、寫協作文件、呼叫 Gemini、抓氣象署的程式。

---

## 4.1 摘要

現有測試全部通過（Rust 348 個、UI 邏輯 105 個、collab-marker 63 個），純邏輯的部分保護得不錯；問題集中在**測試碰不到的那一層**。最常出事的地方（app.js 的自動刷新流程、main.rs 注入協作視窗的寫入／校正腳本）幾乎沒有自動測試，而且這幾週的修正有一大半都在這裡，交接清單上 0.3.31～0.3.40 有 8 個版本標著「真環境還沒驗證」。

最值得處理的三件事：

1. **寫入／時間校正腳本（`build_append_script`、`build_correct_script`）沒有任何測試**，而它是唯一會改動同事共用文件的地方。可以用假的 Quill 頁面＋固定的文件樣本做 golden test，不碰真文件（#1）。
2. **沒有每次 push 都跑的檢查**，而且發版時的 CI 只跑 core 的測試，app crate 的 35 個測試和 collab-marker 的 63 個測試從來不在 CI 裡跑；CI 的註解還寫「app crate 有 0 個測試」，已經跟事實不符（#2）。
3. **「為什麼沒寫」的判斷有兩份、而且已經不一致**（`heldReason` 的 `why` vs `stuckReasons`），加上重開就失憶的狀態分散在兩個存檔、以及 story key 當身分，這三個是過去幾週反覆出錯的根源（#3、#4、#5）。把這些判斷收進 `logic.js` 一個地方並補回歸測試，是成本最低、效果最大的一步。

---

## 1. 盤點

### 1.1 用途與技術

- **一句話**：從公司 iNEWS（FTP）讀 rundown 和稿件，整理成固定格式、比對同事共用的線上協作文件後自動寫入，並用 Gemini 產生關鍵字的 Windows 桌面工具。
- **語言／框架**：Rust（edition 2021）＋ Tauri 2；前端是純 HTML/JS，無建置步驟、無框架。
- **主要相依**：`tauri` 2、`tauri-plugin-updater`／`dialog`／`process`、`reqwest`（rustls）、`tokio`、`keyring` 3（windows-native）、`regex`、`chrono`、`encoding_rs`、`serde`／`toml`。前端沒有任何 npm 相依（`app/package.json` 只有 test 指令）。

### 1.2 功能清單（之後報告、測試、AGENTS.md 都用這組名稱）

| 名稱 | 一句話 |
|---|---|
| **匯入整理** | 讀資料夾裡的 iNEWS txt，解析表頭、依樣式收錄／濾除、正規化標點，輸出四行格式 |
| **iNEWS 匯入** | 透過 FTP 讀當節 ONAIR rundown，算累積時間判斷已播出，轉成同樣的稿件 |
| **自動刷新** | 定時重讀 rundown，串起比對 → 關鍵字 → 寫入的整條流程（含換節、延遲補掃、暫停、冷卻） |
| **狀態比對** | 對照協作文件今天的日期段落，標出待處理／要重貼／已剔除／已在文件 |
| **關鍵字** | 呼叫 Gemini 產生關鍵字（TEL、勿上網、無內文不送） |
| **寫入** | 注入腳本到協作視窗，在文件最下方附加稿件、節次分隔線、日期分隔線；寫入前重新比對 |
| **時間校正** | 已寫入的行，rundown 時間變了就只改時間字串 |
| **定時複查** | 每 30 分鐘重讀已補掃完的節次，補寫漏掉的、列出疑似誤寫 |
| **異常保護** | rundown 時間大跳時冷卻、標可疑稿、寫入前要連續兩輪確認 |
| **未知樣式審查** | 新出現的未知樣式跳出擋畫面的審查視窗 |
| **rundown 搜尋** | 搜本地存的 rundown 紀錄，載入成手動卡片 |
| **晚報** | 從本地紀錄抓 18:30～19:30 的稿另外比對 |
| **氣象補內文** | 0722／0822 氣象稿從氣象署抓天氣概況補進內文 |
| **決策紀錄** | 每個判斷寫一行到 `%APPDATA%\ebc\news-script-tool\logs\inews-YYYYMMDD.log` |
| **守門員** | 注入協作視窗的 `collab-guard.user.js`：卡住重整、補讀、防捲動跳動 |
| **上網標記查詢** | `tools/collab-marker` 書籤小工具，查協作文件裡的上網標記 |
| **自動更新** | tag 觸發 GitHub Actions 建置簽章，App 內一鍵更新 |

### 1.3 目錄結構與職責

| 路徑 | 行數 | 職責 |
|---|---|---|
| `core/src/` | 約 8,800 | 純函式核心：`parse`、`clean`、`punctuation`、`format`、`doc_match`（比對、寫入前過濾、時間校正計畫）、`inews`（rundown 時間計算）、`config`、`gemini`、`guard`、`search`、`rundown` |
| `core/tests/` | 931 | 整合測試＋合成 fixture |
| `app/src-tauri/src/main.rs` | 2,189 | Tauri 指令、協作視窗 eval、注入腳本（寫入／校正／讀取）、keyring、設定存取 |
| `app/src-tauri/src/inews.rs` | 2,511 | FTP 連線、自動刷新狀態機（`fetch_once`）、決策紀錄、rundown／搜尋紀錄存檔 |
| `app/src-tauri/src/weather.rs` | 447 | 氣象署天氣概況 |
| `app/src/app.js` | 3,109 | 前端接線、渲染、自動刷新流程、寫入流程、設定頁 |
| `app/src/logic.js` | 1,081 | 可測試的純決策邏輯（README 說 app.js「只負責接線與渲染」，實際上不是，見 #3） |
| `app/tests/` | 約 1,450 | `logic.test.js`（node:test）、瀏覽器 harness |
| `tools/collab-guard/` | 約 1,500 | 守門員使用者腳本＋手動測試頁 |
| `tools/collab-marker/` | 約 1,000 | 上網標記書籤工具＋63 個測試 |

### 1.4 進入點

- 開發執行：`cargo run -p news-script-tool`
- 建置／發版：改 `tauri.conf.json` 版本 → 推 `v*` tag → `.github/workflows/release.yml`
- 測試：`cargo test --workspace`；`cd app && node --test tests/logic.test.js`（無 Node 時用 `tests/browser-harness.html`）；`node --test tools/collab-marker/tests/lookup.test.js`
- 這台電腦：預設 gnu 工具鏈編不過，要用 `+stable-x86_64-pc-windows-msvc`；沒有 Node。

### 1.5 現有的檢查工具

| 項目 | 狀態 |
|---|---|
| formatter | 無設定檔；`rustfmt` 在 msvc 工具鏈**沒有安裝** |
| linter | 無設定檔；`clippy` 在 msvc 工具鏈**沒有安裝**；JS 無 ESLint |
| 型別檢查 | Rust 編譯器；JS 無 |
| 測試 | Rust 單元＋整合、`logic.test.js`、`lookup.test.js`；守門員只有手動測試頁 |
| CI | 只有 `release.yml`（tag／手動觸發），**沒有 push／PR 觸發的檢查** |

### 1.6 AI 規則檔

專案裡**沒有** `AGENTS.md`、`CLAUDE.md`、`.cursor/rules`。實際上扮演規則檔角色的是 repo 外的 `inews/工作紀錄_交接清單.md`（112 行，含版本表和待辦）和使用者的 Claude 記憶。

### 1.7 設定與機密

- 設定：`%APPDATA%\ebc\news-script-tool\config.toml`（`core/src/config.rs`，`CURRENT_CONFIG_VERSION = 17`，有 migrate）。
- 機密：Gemini API key、iNEWS 帳密存 Windows 認證管理員（keyring），**沒有在程式裡找到寫死的金鑰或密碼**。`.gitignore` 排除簽章私鑰與真實稿件。
- 狀態檔：`state\inews-session.json`（後端，`main.rs:355`）、`state\write-session.json`（前端，v0.3.38）、搜尋紀錄、rundown JSON、決策紀錄、異常存檔。

---

## 2. 健檢

| 指令 | 結果 | 數字 |
|---|---|---|
| `cargo +stable-x86_64-pc-windows-msvc test --workspace` | ✅ 通過 | core 單元 254、`tests/inews.rs` 6、`tests/integration.rs` 53、app crate 35，共 **348 通過、0 失敗**；編譯警告 0 |
| `cargo +stable-x86_64-pc-windows-msvc clippy --workspace` | ⛔ 跑不起來 | `'cargo-clippy.exe' is not installed for the toolchain 'stable-x86_64-pc-windows-msvc'`，依原則不自行安裝 |
| `cargo +stable-x86_64-pc-windows-msvc fmt --check` | ⛔ 跑不起來 | `'cargo-fmt.exe' is not installed …`，同上 |
| `logic-tests` → `tests/browser-harness.html` | ✅ 通過 | **105 passed, 0 failed (of 105)**，載入錯誤 0 |
| `app.js`、`logic.js` 語法（頁面 fetch 後 `new Function`） | ✅ | 兩個都 ok |
| `guard-tests` → `fake-stale.html?mode=stale` | ✅（人工判讀） | 補讀快照 1 次、沒有重整（`loads=1`），畫面有更新 |
| `fake-stale.html?mode=coldhang` | ✅（人工判讀） | 第一次 2 秒逾時、重試成功，`coldAttempts=2`、`coldBackfillDone=true`，遲到的回應不算 |
| `lookup.test.js`（collab-marker） | ✅（用臨時 shim） | **63/63**。沒有官方的瀏覽器 harness，我在頁面裡臨時模擬 `node:test`／`node:assert` 跑的，不是正式管道 |

補充：

- 工作目錄在檢查**之前**就有 35 個檔案顯示已修改：`gen/schemas/*.json` 4 個（實際內容 +386 行），`permissions/autogenerated/*.toml` 31 個（只有行尾 LF/CRLF 差異）。schema 檔最後修改時間是昨天 23:58，不是這次檢查造成的。
- 守門員測試頁的「通過」是人看畫面判斷，頁面本身沒有斷言（見 #12）。

---

## 3. 找問題

### 3.1 不一致

**A. 「為什麼還沒寫」有兩份判斷，而且已經分歧**

- `app/src/app.js:1345-1366` `heldReason` 裡的 `why`（不在 logic.js、沒有測試）
- `app/src/logic.js:619-634` `stuckReasons`（有測試）

同一張卡片在兩邊會得到不同的原因：

| 卡片狀態 | `heldReason` | `stuckReasons` |
|---|---|---|
| `hold === "cooldown"` | 等確認 | rundown 時間異常，暫停 |
| `keywordStatus === "loading"` | 等關鍵字 | 關鍵字產生中 |
| 沒有 API key | 等關鍵字 | 沒有 Gemini API key |
| 額度冷卻中 | 等關鍵字 | 等關鍵字（額度冷卻中） |
| 關鍵字錯誤訊息 | 截 60 字 | 截 80 字 |
| 都不是 | 「等寫入」 | （不會出現） |

決策紀錄是使用者事後查原因的唯一依據，兩份字不一樣會誤導判讀。建議以 `stuckReasons` 為準，`heldReason` 改成呼叫它（推測，交給使用者決定）。

**B. 設定預設值寫了兩份**

`core/src/config.rs` 是正式預設值（例：`:564` `sweep_delay_minutes: 10`），但 `app.js` 另外有 19 處 `?? 數字` 寫死同樣的預設值，例如 `app.js:1326` `inewsCfg.sweep_delay_minutes ?? 10`、`:2175` 設定頁的 `s.sweep_delay_minutes ?? 10`，以及 `anomaly_threshold_seconds ?? 120`、`write_stable_rounds ?? 2`、`recheck_interval_minutes ?? 30` 等 13 個欄位。以後改 Rust 預設值，畫面會顯示舊的數字。建議以 Rust 為準，前端一律讀後端給的值。

**C. 狀態存檔分成兩份、兩種寫法**

- 後端 `state\inews-session.json`：Rust 決定內容，自動刷新成功才寫（`main.rs:446-451`），寫失敗 `let _ =` 忽略。
- 前端 `state\write-session.json`：JS 組 JSON、後端只負責存（`app.js:960-966`），寫失敗只 `console.warn`。
- `writtenKeys`、`writtenStories`、`vanishStreaks`（`app.js:968-970`、`:783`）完全只放在記憶體，重開就沒了。

「重開就失憶」在歷史上出過 3 次以上（見 3.3），每次都是補一個欄位進某一份存檔。建議統一成一份、由同一層負責（推測，交給使用者決定是哪一層）。

**D. 寫紀錄的兩條路**

- 後端直接呼叫 `inews::append_log`（`main.rs` 約 20 處，例 `:457`、`:1728`）。
- 前端 `logDecision` → `append_decision_log` 指令 → `append_log`（`app.js:1483-1485`、`main.rs:768-778`）。

最後都寫到同一個檔案，這點沒問題；不一致的是**失敗處理**：前端那條 `.catch(() => {})` 完全吞掉，後端那條 `append_log` 自己 `let _ = writeln!` 吞掉（`inews.rs:1446`）。目前沒有任何方式知道紀錄寫失敗。另外前端會截 500 字（`main.rs:771`），後端不截。

**E. 寫入協作文件的指令有兩個**

`append_to_collab_doc`（`main.rs:1712`，不重新比對）和 `append_to_collab_doc_checked`（`main.rs:1649`，重新比對）。前端已經完全沒有呼叫前者（`app/src`、`tools` 都找不到），但它仍在 `build.rs:30` 註冊、指令仍可被呼叫。AI 改程式時有機會抄到沒有保護的那一個。

**F. 協作視窗腳本的樣板重複**

`build_append_script`（`main.rs:1559`）、`build_correct_script`（`main.rs:1738`）和 `SCRAPE_SCRIPT`（`main.rs:1324`）各自寫一份 `send()`、找 `Quill`、找 `#editor`、`Quill.find` 的檢查（每份約 20 行幾乎一樣）。改一份忘了另一份的風險高。

### 3.2 風險點

**被吞掉的錯誤**

| 位置 | 吞掉什麼 | 後果 |
|---|---|---|
| `app.js:1484` `.catch(() => {})` | 決策紀錄寫失敗 | 紀錄缺行，事後查不到原因（而「沒寫入卻沒紀錄」正是 0.3.35～0.3.40 一直在補的洞） |
| `app.js:965` 只 `console.warn` | 寫入進度存檔失敗 | 重開後接不回去，沒人知道 |
| `main.rs:449-450` `let _ =` | `inews-session.json` 寫失敗 | 同上 |
| `main.rs:183` `let _ =` | 設定 migrate 後存檔失敗 | 每次啟動重做 migrate，影響小 |
| `inews.rs:1446` `let _ = writeln!` | 決策紀錄寫失敗 | 同第一列 |
| `main.rs:1620`、`:1784` 注入腳本 `catch (nudgeErr) {}` | pagehide 補送失敗 | 寫進去的內容可能沒推上伺服器（註解自己說「這不是根治」） |

Rust 其餘 `let _ =` 多半是刪舊檔、`window.emit`、FTP `QUIT`，合理。

**寫死的門檻值**（未進設定）

| 值 | 位置 | 說明 |
|---|---|---|
| 消失 3 輪才重寫 | `app.js:784` `VANISH_ROUNDS = 3` | 0.3.32 新增 |
| 操作中暫停：閒置 30 秒、最長 3 分鐘 | `app.js:1136-1137` | README 有寫 |
| 關鍵字冷卻 60 秒 | `app.js:647` | |
| 審查提示音每 30 秒 | `app.js:1578` | |
| 協作操作排隊上限 30 秒、單次 eval 逾時 10 秒 | `main.rs:1504`、`:1533` | 見下方「寫入逾時」 |
| FTP 逾時 15 秒、節次清單快取 600 秒 | `inews.rs:26`、`:28` | |
| 紀錄保留 14 天、rundown 30 天 | `inews.rs:1430`、`:1474` | |
| 節次開始 −30／+15 分、殘留稿 10 分 | `core/src/inews.rs:473-476` | |
| 重播樣式清單 `["重播"]` | `logic.js:952` | 交接清單第 13 項已註明「寫死在程式裡」 |

補掃 10 分鐘（`sweep_delay_minutes`）其實**已經是設定**，問題是前端又寫了一份預設值（見 3.1 B）。

**過大的檔案（前 5 名，不含 Cargo.lock）**

| 檔案 | 行數 |
|---|---|
| `app/src/app.js` | 3,109 |
| `app/src-tauri/src/inews.rs` | 2,511（其中測試約 900 行） |
| `app/src-tauri/src/main.rs` | 2,189 |
| `core/src/doc_match.rs` | 2,132 |
| `core/src/inews.rs` | 1,766 |

**過長的函式（前 5 名，粗估）**

| 函式 | 位置 | 行數 |
|---|---|---|
| `fetch_once`（自動刷新一輪的全部判斷） | `app/src-tauri/src/inews.rs:764-1318` | 約 555 |
| `settingsModalHtml` | `app/src/app.js:2109` | 約 285 |
| `openSettings` | `app/src/app.js:2467` | 約 173 |
| `compare_with_collab_doc` | `app/src-tauri/src/main.rs:1896` | 約 142 |
| `plan_time_corrections` | `core/src/doc_match.rs:1391` | 約 134 |

`fetch_once` 是換節、補掃、異常、冷卻、可疑、複查全部疊在一起的狀態機，又不在 core 裡，只能靠 app crate 的少數測試。

**已經跟程式不符的註解／文件**

- `.github/workflows/release.yml:23-29`：說 app crate「to run the zero tests it contains」，實際有 35 個測試（`inews.rs` 24、`weather.rs` 9、`main.rs` 2）。AI 讀到這段會以為 app crate 不用測。
- `README.md:56`：`logs\anomaly-…json` 的 `\a` 變成了 BEL 控制字元（`od -c` 看得到 `l o g s \a n o m a l y`），畫面上顯示成 `logsnomaly`。
- `README.md` 專案結構表說 `app.js`「只負責接線與渲染」，但 `heldReason`、`dividerOwed`、`prelistForWrite`、勾選與寫入後的狀態更新（`app.js:1436-1446`）都是決策邏輯。

**寫入逾時的重複寫入風險（推測）**

`eval_in_collab_locked`（`main.rs:1514-1540`）送出腳本後等 10 秒回覆，逾時就回錯誤。如果腳本其實已經 `insertText` 成功、只是回覆沒送回來，前端 `writeInewsItems` 會進 `catch` 停掉自動刷新（`app.js:1459-1466`），卡片仍是待寫入；下次有人按寫入時靠「寫入前重新比對」擋重複。目前沒有測試覆蓋這個情境，風險高低取決於重新比對在那時是否讀得到剛寫入的內容。

**story key 仍被當成身分（推測）**

交接清單和 `inews.rs:313` 都記錄 iNEWS story key「每次存檔、搬稿都會換」，0.3.35 已經把複查、疑似誤寫改成「節次｜slug」。但寫入去重仍用 key：`logic.js:575-577` `writtenKey = key|version`、`app.js:1445` `writtenStories` 以 key 為索引、`logic.js:972` 未知樣式審查也是 `key|version`。目前靠 `writtenSlugs`（slug｜version）和寫入前重新比對兜底，但這是同一類錯誤的剩餘位置。

### 3.3 歷史上出過的錯

`git log` 2026-11-11 起 45 個 commit（0.3.15～0.3.40），只有 12 個有改測試檔。最常改的檔案：`app.js` 17 次、`main.rs` 10 次、`config.rs` 8 次、`logic.js` 8 次、`logic.test.js` 8 次、`doc_match.rs` 6 次、`inews.rs`（app）5 次。

依交接清單版本表與 commit 訊息歸類：

| 根源 | 出現的版本 | 例子 | 有沒有測試 |
|---|---|---|---|
| **重開就失憶**（狀態只在記憶體） | 0.3.24、0.3.28、0.3.37、0.3.38 | 重開後漏寫 `==== 08 ====`、多寫 `==== 17 ====`、漏寫 `==== 22 關鍵時刻 ====` | `restoreWriteSession` 有 7 個測試；`saveWriteSession` 的存檔時機、後端 `inews-session.json` 沒有 |
| **分隔線時機** | 0.3.24、0.3.28、0.3.35、0.3.37 | 分隔線在節次開始前時重啟假設已寫；複查撿回的卡片擋住分隔線 | `composeInewsWrite` 28 處測試，涵蓋好；`dividerOwed`（app.js）沒有 |
| **沒寫入卻沒留紀錄** | 0.3.35、0.3.39、0.3.40 | 11/19 1400 整節卡一小時、11/20 09:06～09:36 0900 整節沒寫，紀錄查不到原因；09 的確切原因至今未證實 | `stuckReasons` 有測試；`heldReason`、`noteOnce`、`logHeld` 在 app.js，沒有 |
| **story key 每次存檔都換** | 0.3.13、0.3.35 | 換稿被當成往過去插入；複查把存檔後的稿當新稿 | core／app crate 有測試（`inews.rs:2475-2507`） |
| **寫過的稿永遠不會重寫** | 0.3.32、0.3.39 | 稿從文件消失後不補；`[重寫]` 放行後卡片仍不勾選 | `reopenForRewrite` 有測試 |
| **協作文件沒讀完就比對** | 0.3.15、0.3.32、0.3.33 | 讀到快取的舊文件，把已有的 24 點稿當新稿寫入 | 守門員只有手動測試頁 |
| **真環境才發現的格式問題** | 0.3.14、0.3.23、0.3.25、0.3.36 | 只寫出內文沒有 slug、日期分隔線沒等號認不得、氣象署「今(dd)日」、晚報空標題 | core 有測試 |

另外，交接清單上 0.3.29～0.3.40 有 **8 個版本標「真環境還沒驗證」**。這代表目前「修好了」的證據多半是單元測試，不是實際跑過的紀錄。

### 3.4 沒有保護的關鍵流程

| 流程 | 壞掉的實際損失 | 現有保護 |
|---|---|---|
| **寫入**（`build_append_script` 注入 Quill） | 同事共用文件被寫錯、重複寫入、格式被破壞 | 要寫的文字由 core `filter_write_parts_with_prelist` 決定（有測試）；**腳本本身（插入位置、分隔空行、標題格式、pagehide）0 測試** |
| **時間校正**（`build_correct_script`） | 改錯行的時間，而且不會留下明顯痕跡 | `plan_time_corrections`（core）有測試；**腳本端的「重新定位、內容必須完全相同才改」0 測試** |
| **自動刷新一輪的判斷**（`fetch_once`） | 漏寫、提早寫還沒播的稿 | app crate 24 個測試，但 555 行裡涵蓋多少不清楚 |
| **寫入流程的前端狀態**（`writeInewsItems`，`app.js:1411-1467`） | 寫完沒記、記了沒寫 | 0 測試（在 app.js） |
| **守門員** | 文件沒讀完就寫入、無限重整 | 手動測試頁，沒有斷言 |
| **發版** | 壞掉的版本推到所有同事電腦 | CI 只跑 core 和 logic.test.js；不跑 app crate 測試、不跑 collab-marker、沒有 clippy／fmt |

---

## 4.2 發現清單

| # | 問題 | 證據 | 影響 | 建議放在哪一層 | 優先度 |
|---|---|---|---|---|---|
| 1 | 寫入與時間校正的注入腳本沒有任何自動測試 | `main.rs:1559` `build_append_script`、`main.rs:1738` `build_correct_script`；全 repo 找不到對它們的測試 | 唯一會改共用文件的程式碼，壞了直接影響同事 | **測試／驗證**：做一個假的 Quill 頁面（類似 `fake-stale.html`），把 Rust 產生的腳本丟進去跑，比對寫入後的文件文字和標題格式（golden test）；完全不碰真文件 | 高 |
| 2 | 沒有每次 push 跑的檢查；發版 CI 漏跑 app crate（35 個）和 collab-marker（63 個）測試；註解說 app crate 0 個測試 | `.github/workflows/release.yml:6-10`（只有 tag 與手動觸發）、`:23-31` | 壞掉要到發版才發現，或根本不會發現 | **自動檢查**：新增 push 觸發的 workflow，跑 `cargo test --workspace`、兩份 JS 測試；並修正註解 | 高 |
| 3 | 「為什麼沒寫」判斷有兩份且已分歧 | `app.js:1356-1361` vs `logic.js:621-631`（對照表見 3.1 A） | 決策紀錄的原因跟實際不符，誤導事後查錯 | **架構**：`heldReason` 改成呼叫 `stuckReasons`，或兩者合併進 `logic.js` 一個函式＋測試 | 高 |
| 4 | 狀態分散在兩個存檔＋只在記憶體的集合，「重開就失憶」反覆出錯 | `main.rs:355`、`app.js:960-970`、`app.js:783`；版本 0.3.24／0.3.28／0.3.37／0.3.38 | 每次重開都可能漏寫或多寫分隔線、重寫已寫過的稿 | **架構**：列出所有跨輪狀態，決定哪些要存檔、統一由一處存取；再補「重開後」的回歸測試 | 高 |
| 5 | 寫入去重仍用會變的 story key | `logic.js:575-577`、`app.js:1445`、`logic.js:972`；`inews.rs:313` 註解自己承認 key 會換 | 存檔後 key 換掉，去重失效，只剩後端重新比對兜底（推測） | **測試／驗證**：先寫一個「同一則稿寫入後 key 變了」的測試看會不會重寫，再決定要不要改架構 | 中 |
| 6 | 決策紀錄寫失敗被完全吞掉 | `app.js:1484` `.catch(() => {})`、`inews.rs:1446` `let _ = writeln!` | 紀錄缺行時沒人知道，而紀錄是查「為什麼沒寫」的唯一依據 | **自動檢查**：JS 禁止空 catch（現在全專案只有這 1 處，很容易先止血）；失敗時至少在畫面狀態列顯示一次 | 中 |
| 7 | 寫入進度存檔失敗只 console.warn／`let _ =` | `app.js:965`、`main.rs:449-450` | 重開後接不回去，又回到「重開就失憶」 | **規則檔**：存檔失敗要記進決策紀錄（配合 #4 一起處理） | 中 |
| 8 | 前端寫了 19 處設定預設值，和 Rust 重複 | `app.js:1326`、`:2175` 等；`config.rs:564` | 改預設值時畫面與實際行為不一致 | **架構**：後端回傳完整設定（含預設值），前端拿掉 `?? 數字` | 中 |
| 9 | 沒用到但仍註冊的寫入指令 | `main.rs:1712` `append_to_collab_doc`、`build.rs:30`；前端無呼叫 | AI 可能抄到不重新比對的那條；多一個可被呼叫的寫入入口 | **架構**：刪掉這個指令（及對應 permission toml） | 中 |
| 10 | 寫入回覆逾時時，實際可能已寫入（推測） | `main.rs:1533-1539`、`app.js:1459-1466` | 可能重複寫入，或寫了沒記 | **測試／驗證**：在 #1 的假頁面加「寫入成功但回覆延遲」情境 | 中 |
| 11 | `fetch_once` 555 行、`app.js` 3,109 行 | `inews.rs:764-1318`；檔案大小表 | 每次改都有可能動到不相干的判斷；AI 很難完整讀懂 | **不處理**（現在）：先做 #1～#4，拆檔等需要動到時順手做。理由：大拆檔的風險比問題本身大，而且沒有測試保護時拆檔本身就危險 | 低 |
| 12 | 守門員測試頁只能靠人看 | `tools/collab-guard/tests/fake-stale.html`（沒有斷言，結果要讀 `window.__collabGuard`） | 改守門員時容易忘了跑或判讀錯 | **測試／驗證**：每個 mode 加一行自動判定（例：`coldhang` 結束時 `coldAttempts===2 && coldBackfillDone`），印出 PASS/FAIL | 低 |
| 13 | 協作視窗三份腳本樣板重複 | `main.rs:1324`、`:1559`、`:1738` 各自有 `send()`／找 Quill | 改一份忘了另一份 | **架構**：抽出共用的前導段落（Rust 字串常數） | 低 |
| 14 | clippy、rustfmt 沒安裝，無法檢查 | 第 2 節指令輸出 | 沒有格式與 lint 基準 | **自動檢查**：使用者決定要不要裝（見 4.3 Q1） | 低 |
| 15 | 重播樣式清單寫死 | `logic.js:952` `RERUN_STYLES = ["重播"]` | 新增重播類樣式要改程式、發版 | **不處理**：目前只有一種，交接清單已記錄 | 低 |
| 16 | 生成檔被追蹤、工作目錄常態髒 | `git status` 35 個檔案；`gen/schemas` 內容 +386 行，toml 只有行尾差異 | 容易被誤 commit，或掩蓋真正的變更 | **規則檔**：寫明「`gen/schemas`、`permissions/autogenerated` 不手動 commit（新指令的 toml 除外）」；行尾問題可用 `.gitattributes` 處理（見 4.3 Q5） | 低 |
| 17 | README 的路徑有控制字元、結構說明過時 | `README.md:56`（BEL 字元）；專案結構表說 app.js 只負責接線 | 文件誤導 | **不處理**／順手修 | 低 |

---

## 4.3 要使用者決定的問題

1. **clippy／rustfmt 要不要裝？** 要的話是 `rustup component add --toolchain stable-x86_64-pc-windows-msvc clippy rustfmt`。裝了第一次跑多半會有一批既有警告，會照 playbook「先止血、舊的列待清」處理。不裝的話 #14 改成「不處理」。
2. **#3「為什麼沒寫」要以哪一份為準？** 我建議以 `stuckReasons`（logic.js）為準，因為它有測試、分得比較細。但 `heldReason` 多一個「等寫入」，以及「等上一節收尾」的前綴語意，要不要保留？
3. **#4 狀態存檔要統一在哪一層？** 後端（Rust，和 `inews-session.json` 一起）或前端（JS 組好、後端只存）。哪些集合要在重開後保留（`writtenKeys`、`writtenStories`、`vanishStreaks`、手動勾選記憶）？
4. **寫死的門檻值要不要進設定？** `VANISH_ROUNDS = 3`、操作中暫停 30 秒／3 分鐘、eval 逾時 10 秒，這些是刻意不讓使用者改的，還是只是還沒做？
5. **生成檔的處理**：`gen/schemas` 和 `permissions/autogenerated` 要繼續追蹤（只是不手動 commit），還是加進 `.gitignore`？toml 的 LF/CRLF 差異要不要用 `.gitattributes` 統一？
6. **#9 `append_to_collab_doc` 可以刪嗎？** 有沒有 App 以外的東西（例如其他腳本）會呼叫它？
7. **push 觸發的 CI**：GitHub Actions 的 Windows runner 跑 `cargo test --workspace` 會多約 4 分鐘（release.yml 註解的估計）。可以接受嗎？還是只在 PR 跑？
8. **最近你最常糾正 AI 的錯是什麼？** git log 和交接清單看得出「重開失憶」「分隔線」「沒紀錄」三類，但如果有口頭糾正過、沒寫進紀錄的，這份清單會比 git log 更準。

---

## 4.4 第 5 階段的實作計畫

依建議順序，只做勾選的項目：

| 步驟 | 對應 # | 新增／修改的檔案 | 工作量 |
|---|---|---|---|
| 5.1 一個指令跑完所有檢查 | 2、14 | 新增 `check.ps1`（`cargo test --workspace`＋clippy／fmt（若裝）＋提示用瀏覽器 harness 跑 JS）；新增 `tools/collab-marker/tests/browser-harness.html`（這台沒 Node 也能跑） | 小 |
| 5.1b push 觸發的 CI | 2 | 新增 `.github/workflows/check.yml`；修正 `release.yml:23-31` 註解並改跑 `--workspace`、加 collab-marker 測試 | 小 |
| 5.2 自動檢查規則 | 6 | 沒有 Node 不能跑 ESLint，所以改成在 `check.ps1` 用 grep 擋 `.catch(() => {})` 和空 catch，錯誤訊息寫「改用 logDecision 或在狀態列顯示」。若裝 clippy：`clippy.toml` 只加真的需要的規則 | 小 |
| 5.3a 寫入／校正腳本的 golden test | 1、10、13 | 新增 `app/tests/fake-quill.html`（最小 Quill 替身：`getLength`、`getText`、`insertText`、`getLines`、`formatLine`、`updateContents`）；`main.rs` 加一個測試把 `build_append_script`／`build_correct_script` 的輸出寫到 `target/` 給頁面讀；固定的文件樣本放 `app/tests/fixtures/`（合成內容） | 中 |
| 5.3b「為什麼沒寫」合併＋測試 | 3 | `logic.js`（合併後的函式）、`app.js`（`heldReason` 改呼叫）、`logic.test.js`（cooldown、loading、沒 API key、等上一節收尾各一個） | 小 |
| 5.3c 重開失憶的回歸測試 | 4、5、7 | 先只補測試：`logic.test.js` 加「重開後同播出日」「寫入後 key 變了」兩組；統一存檔是架構改動，等 Q3 決定後另外估（中～大） | 小（測試）／中～大（統一存檔） |
| 5.3d 守門員測試頁自動判定 | 12 | `tools/collab-guard/tests/fake-stale.html` 每個 mode 加判定與 PASS/FAIL 輸出 | 小 |
| 5.3e 刪掉沒用的寫入指令、前端預設值 | 8、9 | `main.rs`、`build.rs`、`capabilities`、對應 toml；`app.js` 19 處 `?? 數字`、後端回傳預設值 | 小（#9）／中（#8） |
| 5.4 `AGENTS.md`＋`CLAUDE.md` | 7、16 等 | 新增兩個檔案：功能清單、`check.ps1`、「msvc 工具鏈」、「不能跑的東西」（iNEWS／協作文件／Gemini／氣象署／推 tag）、生成檔不 commit、修 bug 要附回歸測試、回報附證據；交接清單的長期規則搬進來 | 小 |
| 5.5 驗收 | — | 跑 `check.ps1`；故意寫一個空 catch、改壞一個 golden 預期值，確認會失敗後還原 | 小 |

**刻意不做**：拆 `app.js`／`fetch_once`（#11）、重播樣式進設定（#15）。理由見發現清單。

---

**第 1～4 階段到此結束，等使用者確認要做哪些項目。**
