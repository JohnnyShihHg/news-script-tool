# Windows 啟動閃退調查（待獨立檢驗）

日期：2026-09-18  
調查基準：`v0.1.9` / commit `c661e59`

## 問題現象與已知條件

- Windows 正式版啟動後游標短暫轉動，接著整個程式無提示關閉。
- 將設定中的文稿來源資料夾清空後，程式可正常啟動。
- 因此問題範圍高度集中於啟動時的自動匯入流程：
  `initFromConfig` → `import_folder` → `read_txt_files` → `import_files` → `process_text`。
- 公司 Windows 的事件紀錄目前無法取得，因此以下結論以原始碼審查及本機最小重現為依據。

## 結論摘要

目前找到兩個能由特定文稿內容觸發的 UTF-8 非法切片。第一項已用最小程式實際重現 panic；第二項依相同 Rust UTF-8 邊界規則可直接成立。正式 Windows 版使用 `windows_subsystem = "windows"`，panic 不會顯示主控台，因此使用者看見的外觀會是無訊息閃退。

此外，另找到一個 `v0.1.9` 前端資料映射遺漏：它不會造成閃退，但會使本版新增的「勿上網不進 Gemini 批次」及卡片 badge 失效。

## 1. 已重現：全形冒號會造成標頭解析 panic

位置：`core/src/parse.rs` 的 `parse_header`（目前約第 30–32 行）

```rust
if let Some(idx) = line.find([':', '：']) {
    let key = line[..idx].trim().to_string();
    let value = line[idx + 1..].trim().to_string();
}
```

### 原因

`str::find` 回傳 byte index。半形 `:` 是一個 byte，但全形 `：` 在 UTF-8 是三個 bytes。找到全形冒號後使用 `idx + 1`，會把字串切在 `：` 的多位元組字元內部；Rust 字串切片要求起訖點必須位於 UTF-8 字元邊界，因此直接 panic。

### 可觸發範例

```text
編輯備註：勿上網
新聞名稱(標題)：合成測試稿1000
樣式：SL
```

### 實際最小重現結果

以這段程式執行：

```rust
let header = "編輯備註：勿上網";
let idx = header.find([':', '：']).unwrap();
println!("{}", &header[idx + 1..]);
```

得到：

```text
thread 'main' panicked:
start byte index 13 is not a char boundary;
it is inside '：'
```

### 建議修法

不要把兩種分隔符混在同一次 `find` 後固定加一。可用 `split_once` 分別處理，或取得實際命中的字元長度，例如：

```rust
let split = line
    .split_once(':')
    .or_else(|| line.split_once('：'));

if let Some((key, value)) = split {
    let key = key.trim().to_string();
    let value = value.trim().to_string();
    // ...
}
```

### 必要回歸測試

- ASCII 冒號：`編輯備註: 勿上網`
- 全形冒號：`編輯備註：勿上網`
- 同一份 header 混用半形及全形冒號
- 冒號後空白及無空白兩種格式

## 2. 高度確定：標題卡後的中文雜訊行會造成 panic

位置：`core/src/parse.rs` 的 `scan_window_for_t2`（目前約第 98–99 行）

```rust
if t.len() >= 2 && t[..2].eq_ignore_ascii_case("t2") && !t[2..].trim().is_empty() {
    return Some(t[2..].trim().to_string());
}
```

### 原因

`t.len()` 是 byte 數，不是字元數。若 `t` 以中文字開頭，長度通常至少三個 bytes，因此 `t.len() >= 2` 成立；接著 `t[..2]` 會在第一個中文字的 UTF-8 bytes 中間切開並 panic。

函式註解表示它應能略過來源 credit 或其他雜訊，但目前只有以 ASCII 開頭的雜訊能安全略過。

### 可觸發形狀

```text
[BAR_某某大]
來源 中央社
T2這才是真正標題
```

或標題卡後只有中文、沒有 T2：

```text
[BAR]
標題待補
[下一張卡]
```

### 建議修法

使用不會破壞 UTF-8 邊界的前綴方法：

```rust
if let Some(rest) = t
    .strip_prefix("T2")
    .or_else(|| t.strip_prefix("t2"))
{
    let title = rest.trim();
    if !title.is_empty() {
        return Some(title.to_string());
    }
}
```

若要完整支援 ASCII case-insensitive，可先安全取得前兩個 `char`，或寫一個只檢查 ASCII `T/t` 和 `2` 的 helper；不要以 byte index 切任意輸入字串。

### 必要回歸測試

- 標題卡後直接 `T2標題`
- 小寫 `t2標題`
- 裸 `T2`（應視為未填標題，不 panic）
- T2 前有一行中文來源資訊（應略過並找到後續 T2）
- T2 前有中文雜訊且後方沒有 T2（應回傳 `None`，不 panic）
- emoji、全形英數或其他多位元組字元開頭的雜訊行

## 3. 建議的防線：單檔解析失敗不應拖垮整批或整個程式

位置：

- `core/src/lib.rs` 的 `import_files`
- `app/src-tauri/src/main.rs` 的 `import_folder`

目前 `import_files` 直接對每份檔案呼叫 `process_text`：

```rust
for (name, text) in files {
    let outcome = process_text(name, text, cfg);
    // ...
}
```

即使修掉目前已知的兩個切片，未來遇到其他非預期輸入仍可能 panic。文稿來源是外部系統產出的資料，不應假設永遠符合格式。

建議至少在每一份檔案的解析邊界使用 `std::panic::catch_unwind`，把該檔案轉成 `Outcome::ParseFailed`，錯誤訊息包含檔名；其餘檔案仍正常匯入。更理想的長期方向是移除 parser 中所有可能因輸入內容 panic 的 `unwrap`／直接索引，讓解析函式全面回傳 `Result`。

驗收條件：一個資料夾同時放入 15 份正常稿和 1 份刻意製造的異常稿時，程式不退出，正常稿仍顯示，異常稿列在「解析失敗」。

## 4. 次要風險：整批讀取與隱藏 DOM 可能造成記憶體暴增

這是次要可能，尚未證明是本次閃退主因。

目前流程會：

1. `read_txt_files` 一次將資料夾內全部 `.txt` 解碼並保留在 `Vec<(String, String)>`。
2. parser 同時保留 `raw_body`、清理後 `body`、header、標題等副本。
3. Tauri IPC 再將整批序列化成 JSON 傳給 WebView。
4. 前端 `render()` 對全部稿件建立完整 HTML；即使卡片是收合狀態，textarea、原文／轉換後 diff 仍已存在 DOM。

如果資料夾長期累積大量稿件，或混入異常大的 `.txt`，WebView2 的記憶體占用可能快速增加。建議：

- 對單檔大小設合理上限並顯示「檔案過大」。
- 視需要限制／警告一次匯入的檔案總數或總 bytes。
- 收合卡片只渲染摘要，展開時才建立完整 body 與 diff DOM。
- 後端可逐檔處理，不必先把全部原文保留在 `files` vector。

## 5. 與閃退無關但需修正：`v0.1.9` 遺漏 `slug_marker` 映射

位置：`app/src/app.js` 的 `loadSummary`（目前約第 229–256 行）

建立前端 item 時只保存：

```javascript
return {
  dto,
  kind,
  bucket,
  // ...
  title: fields.title ?? "",
  body: fields.body ?? "",
  // 沒有 slug_marker
};
```

但 `v0.1.9` 新增的判斷與 badge 讀取的是：

```javascript
const marker = (item.slug_marker ?? "").trim();
const noUpload = isNoUpload(item, noUploadLabel);
```

因此 `item.slug_marker` 永遠是 `undefined`，造成：

- 卡片上的 `(勿上網)` badge 不會出現。
- 批次 `selectKeywordTargets(items, noUploadLabel)` 無法排除勿上網稿件。

輸出路徑在 `app/src/app.js` 後段重新從 `fields.slug_marker` 建立輸出 item，因此輸出標記可能仍正常；不能用輸出正常來推論 Gemini 排除也正常。

### 建議修法

在 `loadSummary` 建立 item 時加入：

```javascript
slug_marker: fields.slug_marker ?? "",
```

並新增一個跨層測試：輸入後端 DTO 形狀，經 `loadSummary` 的資料映射後，再驗證 `selectKeywordTargets`。目前單元測試直接手工建立含 `slug_marker` 的 item，因此沒有覆蓋到這個映射缺口。

## 建議處理優先序

1. 修正 `parse_header` 的全形冒號 UTF-8 切片。
2. 修正 `scan_window_for_t2` 的 `t[..2]` UTF-8 切片。
3. 在單檔解析邊界增加防 panic 隔離，避免再次整批閃退。
4. 修正前端 `slug_marker` 映射並補跨層測試。
5. 加入啟動／panic log，讓正式 Windows 版下次能留下原因。
6. 視資料量再處理批次記憶體及延遲渲染。

## 如何用原始問題資料驗證

若仍保有造成閃退的那批文稿，可用二分法找出單一檔案：每次只放回一半，直到找出一放入就會閃退的檔案。找到後請先檢查：

- header 是否使用全形 `：`；
- `[BAR]`／`[BAR_...大]` 後、`T2` 前是否出現中文或其他非 ASCII 行；
- 檔案大小是否明顯異常。

不要將真實新聞內容提交到公開 repo。若要建立回歸 fixture，請只保留觸發問題所需的結構，並將內容改成完全虛構的合成資料。

---

## 【Claude 獨立檢驗結果 2026-09-18】

已在 macOS 本機對 `c661e59` 的實際原始碼跑過測試，逐項結論如下。

| 項目 | 結論 |
|---|---|
| 1. `parse_header` 全形冒號 panic | **確認成立**（實測重現）；但 GPT 提供的修法有 regression，見下方修正 |
| 2. `scan_window_for_t2` 的 `t[..2]` panic | **確認成立**（實測重現）；修法正確 |
| 3. 單檔解析隔離 | 同意，`import_files` 確實無防護 |
| 4. 記憶體 | 同意是次要項，暫不處理 |
| 5. `slug_marker` 映射遺漏 | **確認成立**，`loadSummary` 未帶 `slug_marker`，而 `app.js:336`、`logic.js:78` 都在讀它 |

實測輸出（把兩個案例寫成 `#[cfg(test)]` 丟進 `core/src/parse.rs` 跑 `cargo test`，測完已還原）：

```text
---- parse::gpt_repro::fullwidth_colon_header ----
panicked at core/src/parse.rs:32:29:
start byte index 13 is not a char boundary; it is inside '：' (bytes 12..15 of string)

---- parse::gpt_repro::chinese_noise_before_t2 ----
panicked at core/src/parse.rs:98:29:
end byte index 2 is not a char boundary; it is inside '來' (bytes 0..3 of string)
```

`app/src-tauri/src/main.rs:1` 確為 `#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]`，所以 release 版 panic 無主控台、外觀就是無訊息閃退 —— 這點 GPT 判斷正確。

### ⚠️ 修正 GPT：第 1 項的建議修法是錯的，不要照抄

GPT 建議：

```rust
let split = line.split_once(':').or_else(|| line.split_once('：'));
```

這會改變語意。原本 `find([':', '：'])` 取的是**兩種冒號中最早出現的那一個**；改成 `split_once(':')` 優先，等於只要整行任何位置有半形冒號就拿它來切，即使全形冒號在更前面。

本專案的表頭正好會踩到。`news-script-tool-spec.md:48` 的欄位：

```text
累積時間: 07:49:58
```

若這份稿改用全形冒號 `累積時間：07:49:58`，GPT 的寫法實測得到：

```text
Some(("累積時間：07", "49:58"))
```

key 變成 `累積時間：07`，`core/src/lib.rs:54` 的 `header_value(&header, "累積時間")` 就抓不到時間，連帶影響排序與輸出的 `======HHMM`。等於把閃退換成一個更難發現的靜默錯誤。

**正確修法**：保留 `find`，只是不要寫死 `+1`，改用命中字元的實際 UTF-8 長度：

```rust
if let Some(idx) = line.find([':', '：']) {
    let sep_len = line[idx..].chars().next().unwrap().len_utf8();
    let key = line[..idx].trim().to_string();
    let value = line[idx + sep_len..].trim().to_string();
    if !key.is_empty() {
        fields.push((key, value));
    }
}
```

（`chars().next()` 在 `find` 有命中時必定是 `Some`，`idx` 保證落在字元邊界。）

回歸測試除了 GPT 列的四項，請務必再加一項：**全形冒號的 key，value 內含半形冒號**，例如 `累積時間：07:49:58` 必須解析成 key `累積時間` / value `07:49:58`。

### 補充：第 2 項的修法可用，但請一併保留原本語意

GPT 的 `strip_prefix("T2").or_else(|| t.strip_prefix("t2"))` 涵蓋了原 `eq_ignore_ascii_case` 的全部兩種大小寫（`2` 無大小寫之分），行為等價且不會破邊界，可直接採用。注意 `rest.trim()` 為空時要**繼續往下一行找**、而不是 return，這點與原碼一致，不要改成提早 return `None`。

### 優先序（維持 GPT 排序，僅補註）

第 1、2 項是同一個根因（用 byte index 切使用者輸入的中文字串）。我已把專案內所有 byte-index 字串切片掃過一遍，除了這兩處沒有第三個同類問題（`parse.rs:140` 的 `block_lines[1..]` 切的是 `Vec<&str>`，且 `block_lines[0]` 必為 `[<` 標記行，安全）。所以修完 1、2 就可以直接進 3。
