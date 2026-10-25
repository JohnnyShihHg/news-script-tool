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

/// 依開始時間排好的節次（2400 排最前面）。只看資料夾名稱；被跨節合併或空節次
/// 跳過的判斷要讀 rundown，在 `plan_slots` 裡做。
pub fn sorted_shows(names: &[String]) -> Vec<(String, u32)> {
    let mut shows: Vec<(String, u32)> =
        names.iter().filter_map(|n| show_start(n).map(|s| (n.clone(), s))).collect();
    shows.sort_by_key(|(_, s)| *s);
    shows.dedup_by_key(|(_, s)| *s);
    shows
}

/// 距離 `target` 還有幾秒（循環，0..86400）。
pub fn seconds_until(target: u32, now: u32) -> u32 {
    (target as i64 - now as i64).rem_euclid(DAY) as u32
}

/// 從 `from` 往後到 `to` 的整點距離（循環，1..=86400：同一個時間算一整天）。
fn forward_gap(from: u32, to: u32) -> i64 {
    match seconds_until(to, from) {
        0 => DAY,
        g => g as i64,
    }
}

/// 節次的第一個硬時間最早可以比整點早多少、最晚可以晚多少，超出就不可信（殘留的
/// 舊稿，例：1400 的 rundown 第一個硬時間是 06:41），改用 `lead_minutes`。
pub const START_EARLY_LIMIT: i64 = 30 * 60;
pub const START_LATE_LIMIT: i64 = 15 * 60;
/// 累積時間比節次開始時間早這麼多以上，是 rundown 裡殘留的舊稿，不處理。
pub const RESIDUAL_SECONDS: i64 = 10 * 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartSource {
    /// 該節 rundown 的第一個硬時間。
    HardTime,
    /// 沒有硬時間，或硬時間不在整點 −30 分 ～ +15 分之間：整點 − lead_minutes。
    Fallback,
}

impl StartSource {
    pub fn label(self) -> &'static str {
        match self {
            StartSource::HardTime => "硬時間",
            StartSource::Fallback => "fallback",
        }
    }
}

/// 節次的實際開始時間（拉到整點附近，可能是負的或超過 86400）與來源。
pub fn slot_start(nominal: u32, first_hard: Option<u32>, lead_minutes: u32) -> (i64, StartSource) {
    let nominal = nominal as i64;
    if let Some(hard) = first_hard {
        let h = unwrap_near(hard as i64, nominal);
        if h >= nominal - START_EARLY_LIMIT && h <= nominal + START_LATE_LIMIT {
            return (h, StartSource::HardTime);
        }
    }
    (nominal - lead_minutes as i64 * 60, StartSource::Fallback)
}

/// 選節次時需要知道的 rundown 資訊。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ShowProbe {
    pub first_hard: Option<u32>,
    /// 有沒有「帶樣式的非 float 稿」。沒有就是空節次，自動跳過。
    pub has_stories: bool,
}

impl ShowProbe {
    pub fn of(rows: &[RundownRow]) -> Self {
        ShowProbe {
            first_hard: rows.iter().find_map(|r| r.hard),
            has_stories: rows.iter().any(|r| !r.floated && r.styled),
        }
    }
}

/// 一個有效節次：名稱、整點、實際開始時間。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Slot {
    pub show: String,
    pub nominal: u32,
    /// 拉到 `nominal` 附近的實際開始時間。
    pub start: i64,
    pub source: StartSource,
}

impl Slot {
    /// `other` 的開始時間，換算到這一節的時間軸上（一定在這一節之後）。
    pub fn later_start(&self, other: &Slot) -> i64 {
        self.nominal as i64 + forward_gap(self.nominal, other.nominal) + (other.start - other.nominal as i64)
    }

    /// 把一個當天秒數（或累積時間）拉到這一節的時間軸上。
    pub fn axis(&self, t: i64) -> i64 {
        unwrap_near(t, self.nominal as i64)
    }
}

/// 一節的有效範圍：開始 ～ 下一個有效節次的開始（不含）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Window {
    pub slot: Slot,
    /// 下一個有效節次；None = 只有這一節，範圍到 24 小時後。
    pub next: Option<Slot>,
}

impl Window {
    pub fn start(&self) -> i64 {
        self.slot.start
    }

    pub fn end(&self) -> i64 {
        match &self.next {
            Some(n) => self.slot.later_start(n),
            None => self.slot.nominal as i64 + DAY,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkipReason {
    /// 被某節的跨節設定蓋過（1200 → 1400 時的 1300）。
    Spanned(String),
    /// 沒有帶樣式的非 float 稿。
    Empty,
}

/// 這一輪選中的節次。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    pub current: Window,
    /// 上一個有效節次（補掃、換節最後同步用）。
    pub previous: Option<Window>,
    /// 已過整點的最近一節還沒結束，但下一節已經開始，所以提前換過去了。
    pub switched_early: bool,
    /// 判斷過程中跳過的節次（只列有讀到的）。
    pub skipped: Vec<(String, SkipReason)>,
}

/// `plan_slots` 的工作狀態：排好的節次、讀過的 rundown 摘要、跨節設定、跳過紀錄。
struct Planner<'a> {
    shows: Vec<(String, u32)>,
    /// (起點 index, 起點到終點的距離)。
    spans: Vec<(usize, i64)>,
    lead_minutes: u32,
    probes: HashMap<String, ShowProbe>,
    probe: &'a mut dyn FnMut(&str) -> ShowProbe,
    skipped: Vec<(String, SkipReason)>,
}

impl Planner<'_> {
    fn len(&self) -> usize {
        self.shows.len()
    }

    fn probe(&mut self, i: usize) -> ShowProbe {
        let name = self.shows[i].0.clone();
        if let Some(p) = self.probes.get(&name) {
            return *p;
        }
        let p = (self.probe)(&name);
        self.probes.insert(name, p);
        p
    }

    fn skip(&mut self, i: usize, reason: SkipReason) {
        let name = &self.shows[i].0;
        if !self.skipped.iter().any(|(s, _)| s == name) {
            self.skipped.push((name.clone(), reason));
        }
    }

    /// 沒被跨節設定蓋過、而且有帶樣式的非 float 稿。跨節的起點那節本身是空的，就不
    /// 蓋過後面的節次。
    fn valid(&mut self, i: usize) -> bool {
        for k in 0..self.spans.len() {
            let (from, len) = self.spans[k];
            if from == i {
                continue;
            }
            if forward_gap(self.shows[from].1, self.shows[i].1) < len && self.probe(from).has_stories {
                let by = self.shows[from].0.clone();
                self.skip(i, SkipReason::Spanned(by));
                return false;
            }
        }
        if !self.probe(i).has_stories {
            self.skip(i, SkipReason::Empty);
            return false;
        }
        true
    }

    fn slot(&mut self, i: usize) -> Slot {
        let (name, nominal) = self.shows[i].clone();
        let (start, source) = slot_start(nominal, self.probe(i).first_hard, self.lead_minutes);
        Slot { show: name, nominal, start, source }
    }

    /// 下一個有效節次：有跨節設定就從終點那節（或終點之後的第一節）開始找。
    fn next_valid(&mut self, i: usize) -> Option<usize> {
        let n = self.len();
        let span = self.spans.iter().find(|(from, _)| *from == i).map(|&(_, len)| len);
        let from_k = match span {
            Some(len) => {
                let k = (1..n).find(|&k| forward_gap(self.shows[i].1, self.shows[(i + k) % n].1) >= len)?;
                // 跳過的中間節次也記下來，決策紀錄才看得到「1300 被 1200 合併」。
                for m in 1..k {
                    let by = self.shows[i].0.clone();
                    self.skip((i + m) % n, SkipReason::Spanned(by));
                }
                k
            }
            None => 1,
        };
        (from_k..n).map(|k| (i + k) % n).find(|&j| self.valid(j))
    }

    fn prev_valid(&mut self, i: usize) -> Option<usize> {
        let n = self.len();
        (1..n).map(|k| (i + n - k) % n).find(|&j| self.valid(j))
    }

    fn window(&mut self, i: usize) -> Window {
        let next = self.next_valid(i).map(|j| self.slot(j));
        Window { slot: self.slot(i), next }
    }
}

/// 節次選擇。`probe` 讀一節的 rundown（呼叫端快取），只會被呼叫在需要判斷的節次上。
///
/// - 有效節次 = 沒被跨節設定蓋過、而且有帶樣式的非 float 稿。
/// - 下一個有效節次：有跨節設定（1200 → 1400）就是終點那節，沒有就是下一節（都跳過無效的）。
/// - 目前節次 = 已過整點的最近一個有效節次；下一個有效節次的開始時間 ≤ 現在就提前換過去。
pub fn plan_slots(
    names: &[String],
    now: u32,
    spans: &std::collections::BTreeMap<String, String>,
    lead_minutes: u32,
    probe: &mut dyn FnMut(&str) -> ShowProbe,
) -> Option<Plan> {
    let shows = sorted_shows(names);
    if shows.is_empty() {
        return None;
    }
    let span_list = spans
        .iter()
        .filter_map(|(from, to)| {
            let i = shows.iter().position(|(name, _)| name == from.trim())?;
            let to = show_start(to.trim())?;
            Some((i, forward_gap(shows[i].1, to)))
        })
        .collect();
    let mut p = Planner { shows, spans: span_list, lead_minutes, probes: HashMap::new(), probe, skipped: Vec::new() };
    let n = p.len();

    let base = p.shows.iter().rposition(|(_, s)| *s <= now).unwrap_or(n - 1);
    let mut cur = (0..n).map(|k| (base + n - k) % n).find(|&j| p.valid(j)).unwrap_or(base);

    let mut switched_early = false;
    for _ in 0..n {
        let Some(nx) = p.next_valid(cur) else { break };
        let (cur_slot, next_slot) = (p.slot(cur), p.slot(nx));
        if cur_slot.axis(now as i64) >= cur_slot.later_start(&next_slot) {
            cur = nx;
            switched_early = true;
        } else {
            break;
        }
    }

    let current = p.window(cur);
    let previous = p.prev_valid(cur).map(|j| p.window(j));
    // 「提前」= 現在還沒到這一節的整點（08:56 換到 0900）。
    let switched_early = switched_early && current.slot.axis(now as i64) < current.slot.nominal as i64;
    Some(Plan { current, previous, switched_early, skipped: p.skipped })
}

/// 延遲補掃：目前節次的整點過了 `delay_minutes` 分鐘、而且這一節還沒補掃過，就補掃
/// 上一個有效節次，回傳它的範圍。
///
/// `swept_for` 是上一次補掃時的「目前節次」。延續中的跨節節次（13:10 時的 1200）在
/// 12:10 就補掃過 1100 了，所以 13:10 不會再觸發；14:10 目前節次變成 1400，才補掃 1200。
pub fn sweep_target<'a>(plan: &'a Plan, now: u32, delay_minutes: u32, swept_for: Option<&str>) -> Option<&'a Window> {
    let cur = &plan.current.slot;
    if swept_for == Some(cur.show.as_str()) {
        return None;
    }
    if cur.axis(now as i64) < cur.nominal as i64 + delay_minutes as i64 * 60 {
        return None;
    }
    plan.previous.as_ref()
}

// --- Rundown -------------------------------------------------------------------

/// Rundown 裡判斷截止點和累積時間需要的欄位。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RundownRow {
    pub floated: bool,
    pub page: String,
    pub title: String,
    pub total: u32,
    pub hard: Option<u32>,
    /// v-style 有填：判斷空節次用。
    pub styled: bool,
}

impl RundownRow {
    pub fn from_story(story: &NsmlStory, floated: bool) -> Self {
        RundownRow {
            floated,
            page: story.page_number().to_string(),
            title: story.title().to_string(),
            total: story.total_seconds(),
            hard: story.hard_time(),
            styled: !story.raw("v-style").trim().is_empty(),
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
    /// 兩個都找不到（或沒開標記式截止）。
    None,
}

/// 標記式截止點（v0.3.0 的規則，v0.3.1 起預設關閉）：從回傳的 index 開始（含它本身）
/// 以下不處理。1200 播到 1400 這種跨節 rundown 中間有好幾個 CM，用標記會切在一半，
/// 所以預設改用時間判斷（`select_rows`）。
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

/// 一列在這一節的時間判斷下的結果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowFate {
    /// 範圍內、已播出：處理。
    Aired,
    /// 範圍內、還沒播到。
    Pending,
    /// 累積時間 ≥ 下一個有效節次的開始時間：預備稿或下一節的事。
    AfterEnd,
    /// 比節次開始時間早 10 分鐘以上：殘留的舊稿。
    Residual,
    /// 算不出累積時間（整份沒有硬時間）。
    NoTime,
}

/// 新規則的「已播出」：累積時間 ≤ 現在、< 下一個有效節次的開始時間、而且不是殘留稿。
/// `now` 是 None = 延遲補掃，不看現在時間。
pub fn is_aired(cume: Option<i64>, now: Option<u32>, window: &Window) -> RowFate {
    let Some(c) = cume else { return RowFate::NoTime };
    let c = window.slot.axis(c);
    if c < window.start() - RESIDUAL_SECONDS {
        RowFate::Residual
    } else if c >= window.end() {
        RowFate::AfterEnd
    } else if now.is_some_and(|n| c > window.slot.axis(n as i64)) {
        RowFate::Pending
    } else {
        RowFate::Aired
    }
}

/// 一節 rundown 要處理哪些列。
#[derive(Debug, Clone, PartialEq)]
pub struct Selection {
    /// 要處理的列的 index（rundown 順序）。
    pub rows: Vec<usize>,
    pub cume: Vec<Option<i64>>,
    /// 每一列的判斷結果（float 且不收 float 的是 None）。
    pub fates: Vec<Option<RowFate>>,
    /// 標記式截止點（只有開了才有）。
    pub cutoff: Option<usize>,
    pub rule: CutoffRule,
    /// 範圍內但還沒播到的列數。
    pub pending: usize,
    /// 沒處理的非 float 列數（還沒播、預備稿、殘留、截止點以下）。
    pub excluded: usize,
}

impl Selection {
    /// 納入的最後一則。
    pub fn last_included(&self) -> Option<usize> {
        self.rows.last().copied()
    }

    /// 納入的最後一則之後、第一則沒納入的非 float 列（範圍的邊界在哪裡）。
    pub fn first_excluded(&self) -> Option<usize> {
        let from = self.last_included().map_or(0, |i| i + 1);
        (from..self.fates.len()).find(|&i| matches!(self.fates[i], Some(f) if f != RowFate::Aired))
            .or_else(|| (0..self.fates.len()).find(|&i| matches!(self.fates[i], Some(f) if f != RowFate::Aired)))
    }
}

/// 標記式截止（預設關閉）的設定：(預告關鍵字, 廣告列序號開頭)。
pub type MarkerCutoff<'a> = Option<(&'a str, &'a str)>;

pub fn select_rows(
    rows: &[RundownRow],
    now: Option<u32>,
    window: &Window,
    include_floated: bool,
    marker: MarkerCutoff,
) -> Selection {
    let (cutoff, rule) = match marker {
        Some((keyword, prefix)) => find_cutoff(rows, keyword, prefix),
        None => (None, CutoffRule::None),
    };
    let cume = compute_cume(rows);
    let end = cutoff.unwrap_or(rows.len());
    let mut picked = Vec::new();
    let mut fates = vec![None; rows.len()];
    let mut pending = 0;
    let mut excluded = 0;
    for i in 0..rows.len() {
        if rows[i].floated && !include_floated {
            continue;
        }
        let fate = if i >= end { RowFate::AfterEnd } else { is_aired(cume[i], now, window) };
        fates[i] = Some(fate);
        match fate {
            RowFate::Aired => picked.push(i),
            RowFate::Pending => {
                pending += 1;
                excluded += 1;
            }
            _ => excluded += 1,
        }
    }
    Selection { rows: picked, cume, fates, cutoff, rule, pending, excluded }
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
        RundownRow { floated, page: page.into(), title: title.into(), total, hard, styled: false }
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

    fn clock(h: u32, m: u32, sec: u32) -> u32 {
        h * 3600 + m * 60 + sec
    }

    fn styled(page: &str, title: &str, total: u32, hard: Option<u32>) -> RundownRow {
        RundownRow { floated: false, page: page.into(), title: title.into(), total, hard, styled: true }
    }

    /// 合成的一天，重現使用者用真實 rundown 量到的數據：
    /// 0900 開始 08:55:00、結束 09:55:05（預備稿 颱風陽台掉1800 算出 09:55:05）；
    /// 1100 結束 11:40:00（1200 第一個硬時間）；1200 播到 1400，中間有 CM 和報時列；
    /// 1300 有列但沒有帶樣式的稿；1400 殘留舊稿，第一個硬時間 06:41（無效 → 13:55）。
    fn day() -> HashMap<String, Vec<RundownRow>> {
        let mut d = HashMap::new();
        d.insert("0800".to_string(), vec![
            row("", "大片頭", 60, Some(clock(7, 55, 0)), false),
            styled("A1", "合成早安稿0800", 120, None),
            styled("=========", "=CM 3==", 300, Some(clock(8, 50, 0))),
            styled("C1", "合成預備稿0800", 60, None),
        ]);
        d.insert("0900".to_string(), vec![
            row("", "大片頭", 60, Some(clock(8, 55, 0)), false),
            styled("A1", "合成九點稿0900", 120, None),
            row("", "*CP1 下節預告一", 30, None, false),
            row("=====", "=COMMERCIAL 1==", 180, None, false),
            styled("B1", "合成第二則0900", 90, None),
            row("", "下節預告三 2則", 30, None, false),
            row("=========", "=CM 3==", 305, Some(clock(9, 50, 0)), false),
            row("", "版型框", 999, None, true),
            styled("A1", "颱風陽台掉1800", 127, None),
            styled("A2", "屋飛砸誰賠1800", 60, None),
        ]);
        d.insert("1000".to_string(), vec![
            row("", "! 09:55:05午安片頭", 10, Some(clock(9, 55, 5)), false),
            styled("A1", "合成十點稿1000", 120, None),
        ]);
        d.insert("1100".to_string(), vec![
            row("", "片頭", 60, Some(clock(10, 55, 0)), false),
            styled("A1", "合成十一點稿1100", 120, None),
            // 結尾是交接列，沒有 CM。
            row("====", "不雙框交接12", 30, Some(clock(11, 40, 0)), false),
            styled("C1", "合成預備稿1100", 60, None),
        ]);
        d.insert("1200".to_string(), vec![
            row("", "片頭", 60, Some(clock(11, 40, 0)), false),
            styled("A1", "合成午間稿1200", 600, None),
            row("=========", "=CM 1==", 180, None, false),
            row("", "====13:00====", 0, Some(clock(13, 0, 0)), false),
            styled("B1", "合成一點稿1200", 600, None),
            row("=========", "=CM 2==", 180, None, false),
            styled("C1", "合成一點半稿1200", 300, Some(clock(13, 30, 0))),
            styled("D1", "合成預備稿1200", 60, Some(clock(13, 56, 0))),
        ]);
        let mut thirteen: Vec<RundownRow> = (0..45).map(|i| row("", &format!("1300空列{i}"), 10, None, false)).collect();
        thirteen[0].hard = Some(clock(12, 55, 0));
        d.insert("1300".to_string(), thirteen);
        d.insert("1400".to_string(), vec![
            styled("Z9", "殘留舊稿", 60, Some(clock(6, 41, 0))),
            row("", "片頭", 60, Some(clock(13, 55, 0)), false),
            styled("A1", "合成兩點稿1400", 120, None),
        ]);
        d
    }

    fn spans() -> std::collections::BTreeMap<String, String> {
        crate::config::InewsConfig::default().slot_spans
    }

    fn plan_at(d: &HashMap<String, Vec<RundownRow>>, now: u32, spans: &std::collections::BTreeMap<String, String>) -> Plan {
        let names: Vec<String> = d.keys().cloned().collect();
        let mut probe = |name: &str| ShowProbe::of(d.get(name).map_or(&[][..], |r| r.as_slice()));
        plan_slots(&names, now, spans, 5, &mut probe).unwrap()
    }

    fn titles(rows: &[RundownRow], sel: &Selection) -> Vec<String> {
        sel.rows.iter().map(|&i| rows[i].title.clone()).collect()
    }

    #[test]
    fn the_0900_slot_runs_from_its_first_hard_time_to_the_next_slot() {
        let d = day();
        // 08:56 → 0900（0900 的第一個硬時間 08:55:00 已經到了）。
        let p = plan_at(&d, clock(8, 56, 0), &spans());
        assert_eq!(p.current.slot.show, "0900");
        assert!(p.switched_early);
        assert_eq!(p.current.start(), clock(8, 55, 0) as i64);
        assert_eq!(p.current.slot.source, StartSource::HardTime);
        assert_eq!(p.current.end(), clock(9, 55, 5) as i64);
        assert_eq!(p.previous.as_ref().unwrap().slot.show, "0800");
        // 08:54 還在 0800。
        let p = plan_at(&d, clock(8, 54, 0), &spans());
        assert_eq!(p.current.slot.show, "0800");
        assert!(!p.switched_early);
    }

    #[test]
    fn the_time_cutoff_leaves_out_standby_stories_without_any_marker() {
        let d = day();
        let rows = &d["0900"];
        let p = plan_at(&d, clock(9, 30, 0), &spans());
        // 補掃（不看現在時間）：颱風陽台掉1800 算出 09:55:05 = 下一節開始，不納入。
        let sel = select_rows(rows, None, &p.current, false, None);
        assert_eq!(
            titles(rows, &sel),
            ["大片頭", "合成九點稿0900", "*CP1 下節預告一", "=COMMERCIAL 1==", "合成第二則0900", "下節預告三 2則", "=CM 3=="]
        );
        assert_eq!(sel.fates[8], Some(RowFate::AfterEnd));
        assert_eq!(rows[sel.first_excluded().unwrap()].title, "颱風陽台掉1800");
        assert_eq!(sel.rule, CutoffRule::None, "marker cutoff is off by default");
        // 09:00：第二則（09:01:50）還沒播。
        let sel = select_rows(rows, Some(clock(9, 0, 0)), &p.current, false, None);
        assert_eq!(titles(rows, &sel), ["大片頭", "合成九點稿0900", "*CP1 下節預告一", "=COMMERCIAL 1=="]);
        assert_eq!(sel.pending, 3);
    }

    #[test]
    fn a_handoff_row_ends_the_1100_slot_without_a_commercial() {
        let d = day();
        let p = plan_at(&d, clock(11, 20, 0), &spans());
        assert_eq!(p.current.slot.show, "1100");
        assert_eq!(p.current.end(), clock(11, 40, 0) as i64);
        let rows = &d["1100"];
        let sel = select_rows(rows, None, &p.current, false, None);
        assert_eq!(titles(rows, &sel), ["片頭", "合成十一點稿1100"]);
    }

    #[test]
    fn the_1200_rundown_runs_until_1400_starts_and_1300_is_skipped() {
        let d = day();
        for now in [clock(12, 0, 0), clock(13, 10, 0), clock(13, 54, 59)] {
            let p = plan_at(&d, now, &spans());
            assert_eq!(p.current.slot.show, "1200", "at {}", format_clock(now as i64));
            let next = p.current.next.as_ref().unwrap();
            assert_eq!(next.show, "1400");
            // 1400 的第一個硬時間 06:41 不在 13:30～14:15 之間 → 14:00 − 5 分鐘。
            assert_eq!(next.source, StartSource::Fallback);
            assert_eq!(p.current.end(), clock(13, 55, 0) as i64);
            assert!(p.skipped.contains(&("1300".to_string(), SkipReason::Spanned("1200".into()))));
        }
        // 13:40：中間的 CM 和 ====13:00==== 都不會切斷，13:56 的預備稿不納入。
        let p = plan_at(&d, clock(13, 40, 0), &spans());
        let rows = &d["1200"];
        let sel = select_rows(rows, Some(clock(13, 40, 0)), &p.current, false, None);
        assert!(titles(rows, &sel).contains(&"合成一點半稿1200".to_string()));
        assert!(!titles(rows, &sel).contains(&"合成預備稿1200".to_string()));
        // 標記式截止打開的話會切在第一個 CM（舊行為，預設關閉）。
        let sel = select_rows(rows, Some(clock(13, 40, 0)), &p.current, false, Some(("預告", "=")));
        assert_eq!(sel.rule, CutoffRule::LastPrefixed);
        // 13:55 一到就換 1400。
        let p = plan_at(&d, clock(13, 55, 0), &spans());
        assert_eq!(p.current.slot.show, "1400");
        assert_eq!(p.previous.as_ref().unwrap().slot.show, "1200");
    }

    #[test]
    fn a_show_with_no_styled_stories_is_skipped_even_without_a_span() {
        let d = day();
        let p = plan_at(&d, clock(13, 10, 0), &Default::default());
        assert_eq!(p.current.slot.show, "1200");
        assert!(p.skipped.contains(&("1300".to_string(), SkipReason::Empty)));
        assert_eq!(p.current.next.as_ref().unwrap().show, "1400");
    }

    #[test]
    fn a_first_hard_time_far_from_the_hour_falls_back_to_the_lead() {
        let two = clock(14, 0, 0);
        assert_eq!(slot_start(two, Some(clock(6, 41, 0)), 5), (clock(13, 55, 0) as i64, StartSource::Fallback));
        assert_eq!(slot_start(two, Some(clock(13, 30, 0)), 5), (clock(13, 30, 0) as i64, StartSource::HardTime));
        assert_eq!(slot_start(two, Some(clock(13, 29, 59)), 5).1, StartSource::Fallback);
        assert_eq!(slot_start(two, Some(clock(14, 15, 0)), 5).1, StartSource::HardTime);
        assert_eq!(slot_start(two, Some(clock(14, 15, 1)), 5).1, StartSource::Fallback);
        assert_eq!(slot_start(two, None, 5), (clock(13, 55, 0) as i64, StartSource::Fallback));
        // 午夜那節：23:55 = 整點前 5 分鐘。
        assert_eq!(slot_start(0, Some(clock(23, 55, 0)), 5), (-300, StartSource::HardTime));
    }

    #[test]
    fn leftover_stories_far_before_the_slot_start_are_residual() {
        let d = day();
        let p = plan_at(&d, clock(14, 20, 0), &spans());
        assert_eq!(p.current.slot.show, "1400");
        let rows = &d["1400"];
        let sel = select_rows(rows, Some(clock(14, 20, 0)), &p.current, false, None);
        assert_eq!(sel.fates[0], Some(RowFate::Residual));
        assert_eq!(titles(rows, &sel), ["片頭", "合成兩點稿1400"]);
    }

    #[test]
    fn the_midnight_show_takes_over_across_zero_hour() {
        let mut d = HashMap::new();
        d.insert("2300".to_string(), vec![styled("", "片頭", 600, Some(clock(22, 55, 0))), styled("A1", "十一點稿", 60, None)]);
        d.insert("2400".to_string(), vec![styled("", "片頭", 600, Some(clock(23, 55, 0))), styled("A1", "午夜稿", 60, None)]);
        assert_eq!(plan_at(&d, clock(0, 30, 0), &Default::default()).current.slot.show, "2400");
        assert_eq!(plan_at(&d, clock(23, 50, 0), &Default::default()).current.slot.show, "2300");
        let p = plan_at(&d, clock(23, 56, 0), &Default::default());
        assert_eq!(p.current.slot.show, "2400");
        assert!(p.switched_early);
        // 00:06：片頭（23:55）和午夜稿（00:05:00）都播過了。
        let p = plan_at(&d, clock(0, 6, 0), &Default::default());
        let sel = select_rows(&d["2400"], Some(clock(0, 6, 0)), &p.current, false, None);
        assert_eq!(sel.rows, vec![0, 1]);
    }

    #[test]
    fn floats_are_left_out_unless_asked_for() {
        let d = day();
        let p = plan_at(&d, clock(9, 30, 0), &spans());
        let mut rows = d["0900"].clone();
        rows[1].floated = true;
        let sel = select_rows(&rows, None, &p.current, false, None);
        assert!(!sel.rows.contains(&1));
        assert_eq!(sel.fates[1], None);
        let sel = select_rows(&rows, None, &p.current, true, None);
        assert!(sel.rows.contains(&1));
    }

    #[test]
    fn no_hard_time_means_nothing_is_aired() {
        let d = day();
        let p = plan_at(&d, clock(9, 30, 0), &spans());
        let rows = vec![styled("1", "a", 60, None), styled("2", "b", 60, None)];
        assert_eq!(compute_cume(&rows), vec![None, None]);
        let sel = select_rows(&rows, Some(clock(12, 0, 0)), &p.current, false, None);
        assert!(sel.rows.is_empty());
        assert_eq!(sel.fates[0], Some(RowFate::NoTime));
    }

    #[test]
    fn the_delayed_sweep_waits_for_ten_past_and_skips_a_continuing_span() {
        let d = day();
        let at = |h, m, swept: Option<&str>| {
            let p = plan_at(&d, clock(h, m, 0), &spans());
            sweep_target(&p, clock(h, m, 0), 10, swept).map(|w| (w.slot.show.clone(), w.end()))
        };
        // 09:05 還沒到；09:10 補掃 0800（結束在 0900 開始的 08:55:00）。
        assert_eq!(at(9, 5, Some("0800")), None);
        assert_eq!(at(9, 10, Some("0800")), Some(("0800".into(), clock(8, 55, 0) as i64)));
        // 同一節補掃過就不再補。
        assert_eq!(at(9, 30, Some("0900")), None);
        // 12:10 補掃 1100；13:10 目前還是 1200（12:10 已補掃過），不補。
        assert_eq!(at(12, 10, Some("1100")), Some(("1100".into(), clock(11, 40, 0) as i64)));
        assert_eq!(at(13, 10, Some("1200")), None);
        // 14:10 才補掃 1200，範圍到 1400 開始的 13:55。
        assert_eq!(at(14, 10, Some("1200")), Some(("1200".into(), clock(13, 55, 0) as i64)));
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
    fn the_cutoff_markers_still_work_when_turned_on() {
        assert_eq!(find_cutoff(&day()["0900"], "預告", "="), (Some(6), CutoffRule::AfterPreview));
        assert_eq!(find_cutoff(&day()["0900"], "預告", "#"), (None, CutoffRule::None));
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
