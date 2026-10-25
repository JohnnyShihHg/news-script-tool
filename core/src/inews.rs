//! iNEWS FTP 匯入的純函式部分：NSML → 「匯出延伸文稿」txt、LIST 解析、節次選擇、
//! 截止點、累積時間與已播出判斷。
//!
//! 連線本身在 app 端（`app/src-tauri/src/inews.rs`）；這裡全部是不碰網路的純函式，
//! 才能在 `cargo test -p news-script-core` 裡完整測到 —— CI 的 test job 只跑 core。
//!
//! 轉換規則是使用者用真實稿件在台內驗證過的：輸出要跟 iNEWS 客戶端手動
//! 「匯出延伸文稿」**位元組完全相同**，現有的 `import_files` 才能原封不動地使用。

use once_cell::sync::Lazy;
use regex::Regex;
use std::collections::HashMap;

/// 表頭 25 行：(標籤, NSML 欄位 id)，順序就是匯出檔的順序。
pub const HEADER_FIELDS: [(&str, &str); 25] = [
    ("序號", "page-number"),
    ("編輯備註", "v-editnote"),
    ("新聞名稱(標題)", "title"),
    ("樣式", "v-style"),
    ("稿頭", "audio-time"),
    ("帶長", "tape-time"),
    ("總長度", "total-time"),
    ("累積時間", "cume-time"),
    ("組", "v-group1"),
    ("文字攝影", "v-reporter"),
    ("註記", "v-affair"),
    ("影像編號", "video-id"),
    ("影像狀態", "event-status"),
    ("文稿狀態", "ready"),
    ("內容說明", "v-caution"),
    ("併稿-組.記者-1", "v-comx1"),
    ("併稿-組.記者-2", "v-comx2"),
    ("併稿-組.記者-3", "v-comx3"),
    ("併稿-組.記者-4", "v-comx4"),
    ("導播", "sng-directed"),
    ("撤收時間", "v-comx5"),
    ("撰稿人", "create-by"),
    ("撰稿時間", "create-date"),
    ("修改人", "modify-by"),
    ("修改時間", "modify-date"),
];

const DIVIDER_LEN: usize = 63;
const CRLF: &str = "\r\n";
const DAY: i64 = 86_400;

/// NSML 欄位的元素型別，決定值要怎麼格式化。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldKind {
    String,
    Duration,
    Date,
    Boolean,
}

/// 一則 NSML 稿件解析後的樣子。欄位值已經 HTML decode 並 trim，但還沒格式化。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct NsmlStory {
    pub fields: HashMap<String, (FieldKind, String)>,
    /// 匯出檔表頭分隔線以下的每一行（anchor 已展開），還沒用 CRLF 接起來。
    pub body_lines: Vec<String>,
}

impl NsmlStory {
    pub fn raw(&self, id: &str) -> &str {
        self.fields.get(id).map(|(_, v)| v.as_str()).unwrap_or("")
    }

    pub fn page_number(&self) -> &str {
        self.raw("page-number")
    }

    pub fn title(&self) -> &str {
        self.raw("title")
    }

    /// total-time 的秒數，空的或不是數字就當 0（不佔時間）。
    pub fn total_seconds(&self) -> u32 {
        self.raw("total-time").trim().parse().unwrap_or(0)
    }

    /// 手動設定的硬時間（cume-time 的 `＠秒數`），當天第幾秒。
    pub fn hard_time(&self) -> Option<u32> {
        parse_hard_time(self.raw("cume-time"))
    }
}

/// `＠26932` → 26932。全形／半形 @ 都接受；沒有數字就是 None。
pub fn parse_hard_time(value: &str) -> Option<u32> {
    let digits: String = value.chars().filter(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        None
    } else {
        digits.parse().ok()
    }
}

static TAG_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"<[^>]*>").unwrap());
static ENTITY_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"&(#[0-9]+|#[xX][0-9a-fA-F]+|[a-zA-Z]+);").unwrap());
static ID_ATTR_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r#"\bid\s*=\s*"([^"]*)""#).unwrap());
static BODY_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?s)<body\b[^>]*>(.*?)</body>").unwrap());
static AESET_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?s)<aeset\b[^>]*>(.*?)</aeset>").unwrap());
static AE_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r#"(?s)<ae\b([^>]*)>(.*?)</ae>"#).unwrap());
// `<p\b` 不會吃到 `<pi>`：p 跟 i 之間沒有字界。
static P_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?s)<p\b[^>]*?(?:/>|>(.*?)</p>)").unwrap());
static AP_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?s)<ap\b[^>]*?(?:/>|>(.*?)</ap>)").unwrap());
static ANCHOR_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r#"<a\b[^>]*\bidref\s*=\s*"([^"]*)"[^>]*?(?:/>|>\s*</a>)"#).unwrap());

/// 四種欄位元素各一個 regex：regex crate 沒有 backreference，不能用 `</\1>`。
static FIELD_RES: Lazy<Vec<(FieldKind, Regex)>> = Lazy::new(|| {
    [
        (FieldKind::String, "string"),
        (FieldKind::Duration, "duration"),
        (FieldKind::Date, "date"),
        (FieldKind::Boolean, "boolean"),
    ]
    .into_iter()
    .map(|(kind, tag)| {
        let re = Regex::new(&format!(r"(?s)<{tag}\b([^>]*?)(?:/>|>(.*?)</{tag}>)")).unwrap();
        (kind, re)
    })
    .collect()
});

/// HTML entity decode：具名的常見五個加 `&nbsp;`，以及十進位／十六進位字元參照。
/// 認不得的 entity 原樣保留，不要吞掉字。
pub fn html_decode(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    ENTITY_RE
        .replace_all(s, |caps: &regex::Captures| {
            let name = &caps[1];
            let decoded = if let Some(hex) = name.strip_prefix("#x").or_else(|| name.strip_prefix("#X")) {
                u32::from_str_radix(hex, 16).ok().and_then(char::from_u32).map(String::from)
            } else if let Some(dec) = name.strip_prefix('#') {
                dec.parse::<u32>().ok().and_then(char::from_u32).map(String::from)
            } else {
                match name {
                    "amp" => Some("&".into()),
                    "lt" => Some("<".into()),
                    "gt" => Some(">".into()),
                    "quot" => Some("\"".into()),
                    "apos" => Some("'".into()),
                    "nbsp" => Some("\u{a0}".into()),
                    _ => None,
                }
            };
            decoded.unwrap_or_else(|| caps[0].to_string())
        })
        .into_owned()
}

/// 去標籤再 decode（順序不能反：decode 出來的 `&lt;` 是字，不是標籤）。
fn text_of(fragment: &str) -> String {
    html_decode(&TAG_RE.replace_all(fragment, ""))
}

fn strip_newlines(s: &str) -> String {
    s.replace(['\r', '\n'], "")
}

/// 秒數 → `m:ss`（116 → `1:56`、0 → `0:00`）。不是數字就原樣回傳。
pub fn format_duration(value: &str) -> String {
    let v = value.trim();
    if v.is_empty() {
        return String::new();
    }
    match v.parse::<i64>() {
        Ok(secs) if secs >= 0 => format!("{}:{:02}", secs / 60, secs % 60),
        _ => v.to_string(),
    }
}

/// 當天第幾秒 → `HH:mm:ss`。超過一天的部分繞回（午夜那節的時鐘會跑過 86400）。
pub fn format_clock(secs: i64) -> String {
    let s = secs.rem_euclid(DAY);
    format!("{:02}:{:02}:{:02}", s / 3600, (s / 60) % 60, s % 60)
}

/// cume-time：`＠26932` → `07:28:52`；空的留空。
pub fn format_cume(value: &str) -> String {
    match parse_hard_time(value) {
        Some(secs) => format_clock(secs as i64),
        None => String::new(),
    }
}

/// 1970-01-01 起算的天數 → (年, 月, 日)。Howard Hinnant 的 civil_from_days。
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Unix 秒 → 台北時間（UTC+8）`yyyy/M/d HH:mm:ss`。1775436877 → `2026/4/6 08:54:37`。
pub fn format_date(value: &str) -> String {
    let v = value.trim();
    if v.is_empty() {
        return String::new();
    }
    let Ok(unix) = v.parse::<i64>() else {
        return v.to_string();
    };
    let local = unix + 8 * 3600;
    let (y, m, d) = civil_from_days(local.div_euclid(DAY));
    format!("{y}/{m}/{d} {}", format_clock(local))
}

/// 一個欄位在匯出檔裡的樣子。
fn format_field(id: &str, kind: FieldKind, value: &str) -> String {
    if id == "cume-time" {
        return format_cume(value);
    }
    match kind {
        FieldKind::Duration => format_duration(value),
        FieldKind::Date => format_date(value),
        FieldKind::String | FieldKind::Boolean => value.to_string(),
    }
}

/// 解析 aeset：`<ae id="N">` → 每個 `<ap>` 一行。
fn parse_aeset(nsml: &str) -> HashMap<String, Vec<String>> {
    let mut map = HashMap::new();
    let Some(aeset) = AESET_RE.captures(nsml) else {
        return map;
    };
    for ae in AE_RE.captures_iter(&aeset[1]) {
        let Some(id) = ID_ATTR_RE.captures(&ae[1]).map(|c| c[1].to_string()) else {
            continue;
        };
        let lines = AP_RE
            .captures_iter(&ae[2])
            .map(|ap| text_of(&strip_newlines(ap.get(1).map_or("", |m| m.as_str()))))
            .collect();
        map.insert(id, lines);
    }
    map
}

fn push_anchor(lines: &mut Vec<String>, aeset: &HashMap<String, Vec<String>>, idref: &str) {
    lines.push("[<".into());
    if let Some(aps) = aeset.get(idref) {
        lines.extend(aps.iter().cloned());
    }
    lines.push(">]".into());
    lines.push(String::new());
}

/// `<body>` → 匯出檔分隔線以下的行。每個 `<p>` 一行；`<a idref>` 展開成製作區。
fn parse_body_lines(nsml: &str, aeset: &HashMap<String, Vec<String>>) -> Vec<String> {
    let Some(body) = BODY_RE.captures(nsml) else {
        return Vec::new();
    };
    let body = strip_newlines(&body[1]);
    let mut lines = Vec::new();
    for p in P_RE.captures_iter(&body) {
        let inner = p.get(1).map_or("", |m| m.as_str());
        if !ANCHOR_RE.is_match(inner) {
            lines.push(text_of(inner));
            continue;
        }
        // 同一個 <p> 裡有 anchor 也有字的情形沒在真實稿件看過：依出現順序輸出，
        // 字的部分只在不是空白時自成一行，免得多出一行空行。
        let mut at = 0;
        for a in ANCHOR_RE.captures_iter(inner) {
            let m = a.get(0).unwrap();
            let before = text_of(&inner[at..m.start()]);
            if !before.trim().is_empty() {
                lines.push(before);
            }
            push_anchor(&mut lines, aeset, &a[1]);
            at = m.end();
        }
        let after = text_of(&inner[at..]);
        if !after.trim().is_empty() {
            lines.push(after);
        }
    }
    lines
}

pub fn parse_nsml(nsml: &str) -> NsmlStory {
    let mut fields = HashMap::new();
    // 只看 <fields> 區塊，避免 body/aeset 裡碰巧出現同名元素。找不到就看整份。
    let scope = match (nsml.find("<fields"), nsml.find("</fields>")) {
        (Some(a), Some(b)) if a < b => &nsml[a..b],
        _ => nsml,
    };
    for (kind, re) in FIELD_RES.iter() {
        for c in re.captures_iter(scope) {
            let Some(id) = ID_ATTR_RE.captures(&c[1]).map(|m| m[1].to_string()) else {
                continue;
            };
            let value = text_of(c.get(2).map_or("", |m| m.as_str())).trim().to_string();
            fields.entry(id).or_insert((*kind, value));
        }
    }
    let aeset = parse_aeset(nsml);
    NsmlStory { fields, body_lines: parse_body_lines(nsml, &aeset) }
}

/// 稿件 → 匯出檔文字。`cume_override` 是自己算出來的累積時間（`HH:mm:ss`），只在
/// NSML 本身的 cume-time 是空的時候才填進去；給 None 就跟手動匯出位元組完全相同。
pub fn story_to_text(story: &NsmlStory, cume_override: Option<&str>) -> String {
    let mut lines: Vec<String> = HEADER_FIELDS
        .iter()
        .map(|(label, id)| {
            let mut value = story
                .fields
                .get(*id)
                .map(|(kind, v)| format_field(id, *kind, v))
                .unwrap_or_default();
            if *id == "cume-time" && value.is_empty() {
                if let Some(c) = cume_override {
                    value = c.to_string();
                }
            }
            // 值是空的也保留冒號後面那個半形空格。
            format!("{label}: {value}")
        })
        .collect();
    lines.push("_".repeat(DIVIDER_LEN));
    lines.push(String::new());
    lines.extend(story.body_lines.iter().cloned());
    lines.join(CRLF)
}

/// NSML → 跟 iNEWS 「匯出延伸文稿」位元組完全相同的 txt（UTF-8 無 BOM、CRLF、結尾不補換行）。
pub fn nsml_to_text(nsml: &str) -> String {
    story_to_text(&parse_nsml(nsml), None)
}

// --- FTP LIST ------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ListKind {
    Folder,
    Story,
}

/// LIST 的一行。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListEntry {
    pub kind: ListKind,
    /// 第二個字元是 `f`：iNEWS 的 float 列。
    pub floated: bool,
    /// 日期後面的完整名稱。稿件用它 RETR（含 ID）；資料夾已去掉開頭的點。
    pub name: String,
}

impl ListEntry {
    /// 稿件名稱的第一段（`040CE84D:0044D4CD:6ADD2EE7`）。
    pub fn story_id(&self) -> &str {
        self.name.split_whitespace().next().unwrap_or("")
    }

    /// 同一則稿件跨修改不變的部分：ID 的前兩段。
    pub fn story_key(&self) -> String {
        let id = self.story_id();
        let parts: Vec<&str> = id.split(':').collect();
        if parts.len() >= 3 {
            format!("{}:{}", parts[0], parts[1])
        } else {
            id.to_string()
        }
    }

    /// ID 的第三段是修改時間戳，稿件一修改就變，拿來做單則變動偵測。
    pub fn version(&self) -> &str {
        self.story_id().rsplit(':').next().unwrap_or("")
    }

    /// ID 後面的 slug（畫面上的標題），只拿來顯示。
    pub fn slug(&self) -> &str {
        let id = self.story_id();
        self.name[self.name.find(id).map_or(0, |i| i + id.len())..].trim()
    }
}

/// 解析一行 LIST。格式（已在台內伺服器實測）：
/// `d--------- 1 d    27 Apr 30  2025 .SHOWS`
/// `-f-------- 1 1     0 Oct 25 06:19  040CE84D:0044D4CD:6ADD2EE7 訊息框`
/// 名稱是第 7 個欄位（日期）之後的全部內容。
pub fn parse_list_line(line: &str) -> Option<ListEntry> {
    let line = line.trim_end_matches(['\r', '\n']);
    let mut rest = line;
    let mut perms = "";
    for i in 0..7 {
        let trimmed = rest.trim_start();
        let end = trimmed.find(char::is_whitespace)?;
        if i == 0 {
            perms = &trimmed[..end];
        }
        rest = &trimmed[end..];
    }
    let name = rest.trim_start();
    if name.is_empty() || perms.len() < 2 {
        return None;
    }
    let mut chars = perms.chars();
    let kind = match chars.next()? {
        'd' => ListKind::Folder,
        '-' => ListKind::Story,
        _ => return None,
    };
    let floated = chars.next() == Some('f');
    let name = match kind {
        ListKind::Folder => name.strip_prefix('.').unwrap_or(name).to_string(),
        ListKind::Story => name.to_string(),
    };
    Some(ListEntry { kind, floated, name })
}

/// 整份 LIST。認不得的行（`total 12` 之類）略過；順序就是 rundown 順序。
pub fn parse_list(text: &str) -> Vec<ListEntry> {
    text.lines().filter_map(parse_list_line).collect()
}

// --- 節次 ----------------------------------------------------------------------

/// `0900` → 32400。只認 4 位數 HHMM；`2400` 是午夜那節，排序時當 `0000`（0 秒）。
pub fn show_start(name: &str) -> Option<u32> {
    if name.len() != 4 || !name.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let h: u32 = name[..2].parse().ok()?;
    let m: u32 = name[2..].parse().ok()?;
    if h > 24 || m >= 60 || (h == 24 && m != 0) {
        return None;
    }
    Some((h % 24) * 3600 + m * 60)
}

/// 節次資料夾名稱的小時（`0800` → `08`），分隔線用。2400 回 `24`。
pub fn show_hour_label(name: &str) -> String {
    name.get(..2).unwrap_or(name).to_string()
}

/// 依開始時間排好的節次（2400 排最前面）。
pub fn sorted_shows(names: &[String]) -> Vec<(String, u32)> {
    let mut shows: Vec<(String, u32)> =
        names.iter().filter_map(|n| show_start(n).map(|s| (n.clone(), s))).collect();
    shows.sort_by_key(|(_, s)| *s);
    shows.dedup_by_key(|(_, s)| *s);
    shows
}

/// 「已過整點的最近一節」與它的下一節（循環，2300 的下一節是 2400）。
/// 現在時間比當天第一節還早，就是前一天的最後一節。
pub fn current_and_next(names: &[String], now: u32) -> Option<((String, u32), Option<(String, u32)>)> {
    let shows = sorted_shows(names);
    if shows.is_empty() {
        return None;
    }
    let idx = shows.iter().rposition(|(_, s)| *s <= now).unwrap_or(shows.len() - 1);
    let next = (shows.len() > 1).then(|| shows[(idx + 1) % shows.len()].clone());
    Some((shows[idx].clone(), next))
}

/// 距離 `target` 還有幾秒（循環，0..86400）。
pub fn seconds_until(target: u32, now: u32) -> u32 {
    (target as i64 - now as i64).rem_euclid(DAY) as u32
}

/// 第一個硬時間離整點超過這麼遠就不可信（多半是別天的值），改用 `lead_minutes`。
const MAX_EARLY_START: i64 = 2 * 3600;

/// 下一節要提前多少秒換過去。節目會在整點前就開始（0900 那節第一個硬時間是
/// 08:55:00），所以用下一節 rundown 的第一個硬時間；沒有硬時間就退回 `lead_minutes`。
/// 硬時間在整點之後代表那節準時或晚開，回傳負值（不提前）。
pub fn early_start_lead(next_start: u32, next_first_hard: Option<u32>, lead_minutes: u32) -> i64 {
    let fallback = lead_minutes as i64 * 60;
    match next_first_hard {
        Some(hard) => {
            let mut diff = (next_start as i64 - hard as i64).rem_euclid(DAY);
            if diff > DAY / 2 {
                diff -= DAY;
            }
            if diff.abs() <= MAX_EARLY_START {
                diff
            } else {
                fallback
            }
        }
        None => fallback,
    }
}

/// 現在是否該提前換到下一節。
pub fn should_switch_early(next_start: u32, now: u32, next_first_hard: Option<u32>, lead_minutes: u32) -> bool {
    let remaining = seconds_until(next_start, now) as i64;
    remaining > 0 && remaining <= early_start_lead(next_start, next_first_hard, lead_minutes)
}

/// 現在離下一節整點在這個範圍內，才去讀下一節的 rundown 找硬時間 —— 不然每一輪都要
/// 多 LIST 一個 queue。節目通常只提前 5 分鐘左右開始，半小時綽綽有餘。
pub const NEXT_SHOW_PROBE_SECONDS: u32 = 30 * 60;

// --- Rundown -------------------------------------------------------------------

/// Rundown 裡判斷截止點和累積時間需要的欄位。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RundownRow {
    pub floated: bool,
    pub page: String,
    pub title: String,
    pub total: u32,
    pub hard: Option<u32>,
}

impl RundownRow {
    pub fn from_story(story: &NsmlStory, floated: bool) -> Self {
        RundownRow {
            floated,
            page: story.page_number().to_string(),
            title: story.title().to_string(),
            total: story.total_seconds(),
            hard: story.hard_time(),
        }
    }
}

/// 照 iNEWS 客戶端的方式算每一列的累積時間（秒，可能超過 86400）。
///
/// 時鐘從第一個硬時間開始；依序遇到 `＠N` 就把時鐘設成 N，這一列的累積時間 = 目前
/// 時鐘，非 float 的列再把時鐘加上自己的 total-time；float 的列不佔時間。第一個硬
/// 時間之前的列用第一個硬時間。整份都沒有硬時間就全部 None。
pub fn compute_cume(rows: &[RundownRow]) -> Vec<Option<i64>> {
    let Some(first) = rows.iter().find_map(|r| r.hard) else {
        return vec![None; rows.len()];
    };
    let mut clock = first as i64;
    let mut seen_hard = false;
    rows.iter()
        .map(|r| {
            if let Some(h) = r.hard {
                clock = h as i64;
                seen_hard = true;
            }
            let cume = clock;
            if seen_hard && !r.floated {
                clock += r.total as i64;
            }
            Some(cume)
        })
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CutoffRule {
    /// 最後一則標題含預告關鍵字的稿之後，第一個序號以前綴開頭的列。
    AfterPreview,
    /// 找不到上面那個，退回最後一個序號以前綴開頭的列。
    LastPrefixed,
    /// 兩個都找不到：整節都處理，UI 要顯示警告。
    None,
}

/// 截止點：從回傳的 index 開始（含它本身）以下全部是預備稿，不處理。
///
/// iNEWS 畫面上最下面那塊灰色**不是** float（廣告列的 float 旗標是關的），所以用的是
/// rundown 結構：`下節預告三` → `=CM 3==`（序號 `=========`）→ 預備稿。
pub fn find_cutoff(rows: &[RundownRow], preview_keyword: &str, page_prefix: &str) -> (Option<usize>, CutoffRule) {
    let prefix = page_prefix.trim();
    let keyword = preview_keyword.trim();
    if prefix.is_empty() {
        return (None, CutoffRule::None);
    }
    let prefixed = |r: &RundownRow| r.page.trim_start().starts_with(prefix);
    if !keyword.is_empty() {
        if let Some(last_preview) = rows.iter().rposition(|r| r.title.contains(keyword)) {
            if let Some(off) = rows[last_preview + 1..].iter().position(prefixed) {
                return (Some(last_preview + 1 + off), CutoffRule::AfterPreview);
            }
        }
    }
    match rows.iter().rposition(prefixed) {
        Some(i) => (Some(i), CutoffRule::LastPrefixed),
        None => (None, CutoffRule::None),
    }
}

/// `t` 加減整天，取最接近 `anchor` 的那個值。午夜那節的時間會橫跨 0 點，
/// 要先拉到同一天才能比大小。
pub fn unwrap_near(t: i64, anchor: i64) -> i64 {
    let mut d = (t - anchor).rem_euclid(DAY);
    if d > DAY / 2 {
        d -= DAY;
    }
    anchor + d
}

/// 累積時間 ≤ 現在時間就算已播出。兩邊都先拉到節次開始時間附近再比。
pub fn is_aired(cume: Option<i64>, now: u32, show_start: u32) -> bool {
    match cume {
        Some(c) => unwrap_near(c, show_start as i64) <= unwrap_near(now as i64, show_start as i64),
        None => false,
    }
}

/// 一節 rundown 要處理哪些列。
#[derive(Debug, Clone, PartialEq)]
pub struct Selection {
    /// 截止點以上、已播出、（預設）非 float 的列的 index。
    pub rows: Vec<usize>,
    pub cume: Vec<Option<i64>>,
    pub cutoff: Option<usize>,
    pub rule: CutoffRule,
    /// 截止點以上但還沒播到的列數（之後一定還會變動，不自動處理）。
    pub pending: usize,
}

pub fn select_rows(
    rows: &[RundownRow],
    now: u32,
    show_start: u32,
    preview_keyword: &str,
    page_prefix: &str,
    include_floated: bool,
) -> Selection {
    let (cutoff, rule) = find_cutoff(rows, preview_keyword, page_prefix);
    let cume = compute_cume(rows);
    let end = cutoff.unwrap_or(rows.len());
    let mut picked = Vec::new();
    let mut pending = 0;
    for i in 0..end {
        if rows[i].floated && !include_floated {
            continue;
        }
        if is_aired(cume[i], now, show_start) {
            picked.push(i);
        } else {
            pending += 1;
        }
    }
    Selection { rows: picked, cume, cutoff, rule, pending }
}

/// 匯入時用的檔名：標題加 `.txt`，同名的加 `_2`、`_3`。檔名不能有的字元換成 `_`。
pub fn unique_file_names(titles: &[String]) -> Vec<String> {
    let mut seen: HashMap<String, usize> = HashMap::new();
    titles
        .iter()
        .map(|t| {
            let clean: String = t
                .trim()
                .chars()
                .map(|c| if matches!(c, '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|') { '_' } else { c })
                .collect();
            let base = if clean.is_empty() { "未命名".to_string() } else { clean };
            let n = seen.entry(base.clone()).or_insert(0);
            *n += 1;
            if *n == 1 {
                format!("{base}.txt")
            } else {
                format!("{base}_{n}.txt")
            }
        })
        .collect()
}

/// 分隔線：`{hh}` 換成節次小時（`08`），`{show}` 換成資料夾名稱（`0800`）。
pub fn format_divider(format: &str, show: &str) -> String {
    format.replace("{hh}", &show_hour_label(show)).replace("{show}", show)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(page: &str, title: &str, total: u32, hard: Option<u32>, floated: bool) -> RundownRow {
        RundownRow { floated, page: page.into(), title: title.into(), total, hard }
    }

    #[test]
    fn durations_are_minutes_and_zero_padded_seconds() {
        assert_eq!(format_duration("116"), "1:56");
        assert_eq!(format_duration("0"), "0:00");
        assert_eq!(format_duration(""), "");
        assert_eq!(format_duration("3605"), "60:05");
    }

    #[test]
    fn cume_time_is_seconds_of_day() {
        assert_eq!(format_cume("＠26932"), "07:28:52");
        assert_eq!(format_cume("@32100"), "08:55:00");
        assert_eq!(format_cume(""), "");
        assert_eq!(parse_hard_time("＠35400"), Some(35400));
    }

    #[test]
    fn dates_are_taipei_local_time_without_month_or_day_padding() {
        assert_eq!(format_date("1775436877"), "2026/4/6 08:54:37");
        assert_eq!(format_date("0"), "1970/1/1 08:00:00");
        // 跨年：UTC 12/31 16:00 已經是台北的 1/1。
        assert_eq!(format_date("1767196800"), "2026/1/1 00:00:00");
        assert_eq!(format_date(""), "");
    }

    #[test]
    fn html_entities_are_decoded_and_unknown_ones_kept() {
        assert_eq!(html_decode("A&amp;B &lt;x&gt; &quot;q&quot; &#20013;&#x6587;"), "A&B <x> \"q\" 中文");
        assert_eq!(html_decode("&bogus; &"), "&bogus; &");
    }

    #[test]
    fn list_lines_parse_folders_and_stories() {
        let folder = parse_list_line("d--------- 1 d    27 Apr 30  2025 .SHOWS").unwrap();
        assert_eq!(folder.kind, ListKind::Folder);
        assert_eq!(folder.name, "SHOWS");
        assert!(!folder.floated);

        let story = parse_list_line("-f-------- 1 1     0 Oct 25 06:19  040CE84D:0044D4CD:6ADD2EE7 訊息框\r").unwrap();
        assert_eq!(story.kind, ListKind::Story);
        assert!(story.floated);
        assert_eq!(story.name, "040CE84D:0044D4CD:6ADD2EE7 訊息框");
        assert_eq!(story.story_key(), "040CE84D:0044D4CD");
        assert_eq!(story.version(), "6ADD2EE7");
        assert_eq!(story.slug(), "訊息框");

        let plain = parse_list_line("---------- 1 1     0 Oct 25 06:19  040CE84D:0044D4CE:6ADD2EE8 颱風 陽台掉1800").unwrap();
        assert!(!plain.floated);
        assert_eq!(plain.slug(), "颱風 陽台掉1800");
    }

    #[test]
    fn a_list_keeps_rundown_order_and_skips_noise() {
        let text = "total 3\r\n\
            ---------- 1 1 0 Oct 25 06:19  0000000A:00000001:00000001 第一則\r\n\
            -f-------- 1 1 0 Oct 25 06:19  0000000A:00000002:00000001 框\r\n\
            ---------- 1 1 0 Oct 25 06:19  0000000A:00000003:00000001 第三則\r\n";
        let list = parse_list(text);
        assert_eq!(list.iter().map(|e| e.slug()).collect::<Vec<_>>(), ["第一則", "框", "第三則"]);
    }

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn the_current_show_is_the_latest_one_past_the_hour() {
        let shows = names(&["0800", "0900", "1000", "2400", "0100", "README"]);
        let ((cur, _), next) = current_and_next(&shows, 8 * 3600 + 56 * 60).unwrap();
        assert_eq!(cur, "0800");
        assert_eq!(next.unwrap().0, "0900");

        // 00:30 → 2400（午夜那節排序時當 0000）。
        let ((cur, start), _) = current_and_next(&shows, 30 * 60).unwrap();
        assert_eq!((cur.as_str(), start), ("2400", 0));

        // 23:57 在 1000 之後；下一節循環到 2400。
        let shows = names(&["2300", "2400", "1000"]);
        let ((cur, _), next) = current_and_next(&shows, 23 * 3600 + 57 * 60).unwrap();
        assert_eq!(cur, "2300");
        assert_eq!(next.unwrap().0, "2400");
    }

    #[test]
    fn the_next_show_takes_over_once_its_first_hard_time_passes() {
        let nine = 9 * 3600;
        // 0900 的第一個硬時間是 大片頭 ＠32100 = 08:55:00。
        assert!(should_switch_early(nine, 8 * 3600 + 56 * 60, Some(32100), 5));
        assert!(should_switch_early(nine, 32100, Some(32100), 5));
        assert!(!should_switch_early(nine, 32099, Some(32100), 5));
        // 沒有硬時間就退回整點前 N 分鐘。
        assert!(should_switch_early(nine, nine - 5 * 60, None, 5));
        assert!(!should_switch_early(nine, nine - 6 * 60, None, 5));
        // 硬時間在整點之後：不提前。
        assert!(!should_switch_early(nine, nine - 60, Some(nine + 10), 5));
        // 午夜那節 2400（start 0），23:55 開始。
        assert!(should_switch_early(0, 23 * 3600 + 56 * 60, Some(86100), 5));
        assert!(!should_switch_early(0, 23 * 3600 + 50 * 60, Some(86100), 5));
    }

    #[test]
    fn cume_follows_hard_times_and_skips_floats() {
        // 對照真實 0900 rundown：=CM 3==（＠35400 = 09:50:00，總長 5:05）之後的兩則。
        let rows = vec![
            row("", "前面的稿", 100, None, false),
            row("=========", "=CM 3==", 305, Some(35400), false),
            row("", "版型框", 999, None, true),
            row("A1", "颱風陽台掉1800", 127, None, false),
            row("A2", "屋飛砸誰賠1800", 60, None, false),
        ];
        let cume = compute_cume(&rows);
        let clock: Vec<String> = cume.iter().map(|c| format_clock(c.unwrap())).collect();
        assert_eq!(clock, ["09:50:00", "09:50:00", "09:55:05", "09:55:05", "09:57:12"]);
    }

    #[test]
    fn no_hard_time_means_no_cume_and_nothing_is_aired() {
        let rows = vec![row("1", "a", 60, None, false), row("2", "b", 60, None, false)];
        assert_eq!(compute_cume(&rows), vec![None, None]);
        let sel = select_rows(&rows, 12 * 3600, 9 * 3600, "預告", "=", false);
        assert!(sel.rows.is_empty());
        assert_eq!(sel.pending, 2);
    }

    fn rundown() -> Vec<RundownRow> {
        vec![
            row("", "大片頭", 60, Some(32100), false),
            row("A1", "第一則", 120, None, false),
            row("", "*CP1 下節預告一", 30, None, false),
            row("", "預告內容", 20, None, false),
            row("=====", "=COMMERCIAL 1==", 180, None, false),
            row("B1", "第二則", 90, None, false),
            row("", "下節預告三 2則", 30, None, false),
            row("=========", "=CM 3==", 305, Some(35400), false),
            row("", "! 09:55:05午安片頭", 10, None, false),
            row("C1", "預備稿", 60, None, false),
        ]
    }

    #[test]
    fn the_cutoff_is_the_first_commercial_after_the_last_preview() {
        assert_eq!(find_cutoff(&rundown(), "預告", "="), (Some(7), CutoffRule::AfterPreview));
    }

    #[test]
    fn the_cutoff_falls_back_to_the_last_commercial_then_to_nothing() {
        let mut rows = rundown();
        rows[6].title = "下節三".into(); // 最後一則預告變成 *CP1 下節預告一，之後有 =COMMERCIAL 1==
        assert_eq!(find_cutoff(&rows, "預告", "="), (Some(4), CutoffRule::AfterPreview));
        assert_eq!(find_cutoff(&rows, "沒有這個詞", "="), (Some(7), CutoffRule::LastPrefixed));
        assert_eq!(find_cutoff(&rows, "預告", "#"), (None, CutoffRule::None));
    }

    #[test]
    fn only_aired_rows_above_the_cutoff_are_selected() {
        // 09:00:00：大片頭 08:55、第一則 08:56、預告 08:58... 第二則 09:01:50 還沒播。
        let sel = select_rows(&rundown(), 9 * 3600, 9 * 3600, "預告", "=", false);
        assert_eq!(sel.rows, vec![0, 1, 2, 3, 4]);
        assert_eq!(sel.pending, 2);
        assert_eq!(sel.cutoff, Some(7));
        // 很晚了：截止點以下的預備稿仍然不處理。
        let sel = select_rows(&rundown(), 12 * 3600, 9 * 3600, "預告", "=", false);
        assert_eq!(sel.rows, vec![0, 1, 2, 3, 4, 5, 6]);
    }

    #[test]
    fn floats_are_left_out_unless_asked_for() {
        let mut rows = rundown();
        rows[1].floated = true;
        let sel = select_rows(&rows, 12 * 3600, 9 * 3600, "預告", "=", false);
        assert!(!sel.rows.contains(&1));
        let sel = select_rows(&rows, 12 * 3600, 9 * 3600, "預告", "=", true);
        assert!(sel.rows.contains(&1));
    }

    #[test]
    fn the_midnight_show_compares_across_zero_hour() {
        let rows = vec![row("", "片頭", 600, Some(86100), false), row("A1", "午夜稿", 60, None, false)];
        // 23:55 開始，00:01 時兩則都播過了（第二則 00:05:00 還沒）。
        let sel = select_rows(&rows, 60, 0, "預告", "=", false);
        assert_eq!(sel.rows, vec![0]);
        let sel = select_rows(&rows, 5 * 60, 0, "預告", "=", false);
        assert_eq!(sel.rows, vec![0, 1]);
        // 23:58：片頭播了，午夜稿還沒。
        let sel = select_rows(&rows, 23 * 3600 + 58 * 60, 0, "預告", "=", false);
        assert_eq!(sel.rows, vec![0]);
    }

    #[test]
    fn file_names_are_unique_and_safe() {
        let t = names(&["颱風/陽台", "颱風/陽台", "", "a:b"]);
        assert_eq!(unique_file_names(&t), ["颱風_陽台.txt", "颱風_陽台_2.txt", "未命名.txt", "a_b.txt"]);
    }

    #[test]
    fn dividers_use_the_show_hour() {
        assert_eq!(format_divider("==== {hh} ====", "0800"), "==== 08 ====");
        assert_eq!(format_divider("====== {show}", "2400"), "====== 2400");
    }

    #[test]
    fn an_anchor_without_an_ae_still_expands_to_an_empty_block() {
        let nsml = "<nsml><fields></fields><body><p><a idref=\"9\"/></p><p>內文</p></body></nsml>";
        assert_eq!(parse_nsml(nsml).body_lines, ["[<", ">]", "", "內文"]);
    }

    #[test]
    fn pi_and_other_p_prefixed_tags_are_not_paragraphs() {
        let nsml = "<body><p>前<pi>（主播）</pi>後</p><p></p></body>";
        assert_eq!(parse_nsml(nsml).body_lines, ["前（主播）後", ""]);
    }
}
