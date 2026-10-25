use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FilterConfig {
    pub allowed_styles: Vec<String>,
    pub blocked_styles: Vec<String>,
    pub excluded_slug_suffixes: Vec<String>,
    pub flag_styles: Vec<String>,
    pub title_tag_pattern: String,
    /// Second-pass title-tag pattern, used only when `title_tag_pattern` matches
    /// nothing anywhere in the production block.
    ///
    /// Some scripts carry only plain `[BAR]` cards (weather rundowns) or only
    /// `[BAR_最新]` cards (some live stories), never a `[BAR_..大]`. Without a fallback
    /// those files parse to no title at all. The first such card's T2 is taken.
    /// Deliberately still BAR-family only, not "any T2 in the block" — `[雙框_記者右]`
    /// blocks carry a `T2主播姓名` line that must never be mistaken for a headline.
    #[serde(default = "default_title_tag_fallback_pattern")]
    pub title_tag_fallback_pattern: String,
    /// Terms that identify a style from the slug when 樣式 itself is blank.
    ///
    /// Some rundown rows carry no 樣式 at all and write the format into the slug
    /// instead. Without this the row looks like rundown structure and gets skipped,
    /// so a 推播 silently never reaches the output.
    ///
    /// Matched as a plain substring, anywhere in the slug — the term shows up in
    /// every position in practice (`XX14推播`, `欣怡推播14稿標`, `XX推播預錄`), so
    /// there is deliberately no position or shape rule to go stale.
    ///
    /// Whole terms only, never a single character: `推` alone would also match
    /// unrelated rows such as `鬼月撿便宜14推`.
    #[serde(default = "default_slug_style_terms")]
    pub slug_style_terms: Vec<String>,
    /// Substrings in 編輯備註 that send a story straight to 已濾除 whatever its style
    /// (`預告` marks a promo, not a story to publish). Still a default, not a verdict:
    /// the entry is fully parsed and can be ticked back on.
    #[serde(default = "default_blocked_note_terms")]
    pub blocked_note_terms: Vec<String>,
    /// Terms in the slug that keep a blank-樣式 row out of 解析失敗 without setting a
    /// style. `專` names a 專題, which can run in any format, so it is not a style --
    /// it only means "this is a story", leaving 未知樣式 for a human to call.
    #[serde(default = "default_slug_keep_terms")]
    pub slug_keep_terms: Vec<String>,
    /// `尾碼=樣式` pairs read off the last `-` segment of 影像編號, used only when 樣式
    /// is blank and the slug names no style. `X5` is the camera code for an SL fake
    /// live hit (`20261023-585-X5`), which producers often leave 樣式 blank on.
    #[serde(default = "default_video_id_styles")]
    pub video_id_styles: Vec<String>,
}

fn default_video_id_styles() -> Vec<String> {
    vec!["X5=SL".into()]
}

fn default_slug_keep_terms() -> Vec<String> {
    vec!["專".into()]
}

fn default_blocked_note_terms() -> Vec<String> {
    vec!["預告".into()]
}

fn default_slug_style_terms() -> Vec<String> {
    // SLIVE before SL: the first match wins, and every SLIVE slug also contains SL.
    vec!["推播".into(), "SLIVE".into(), "SL".into(), "連線".into()]
}

/// The fallback shipped before v0.2.4, which only knew plain `[BAR]`.
const LEGACY_TITLE_TAG_FALLBACK_PATTERN: &str = r"^\[BAR\]$";

fn default_title_tag_fallback_pattern() -> String {
    r"^\[BAR(_[^\]]*)?\]$".into()
}

impl Default for FilterConfig {
    fn default() -> Self {
        Self {
            allowed_styles: vec![
                "SOT".into(), "短sot".into(), "LIVE".into(), "海神".into(),
                "閃電".into(), "旋風".into(), "4G".into(), "TEL".into(), "電連".into(),
                "SLIVE".into(), "SL".into(), "SNG".into(), "推播".into(), "連線".into(),
            ],
            blocked_styles: vec!["BS".into(), "SO".into()],
            excluded_slug_suffixes: vec!["SOU".into()],
            flag_styles: vec!["TEL".into(), "電連".into()],
            title_tag_pattern: r"^\[BAR_.*大\]$".into(),
            title_tag_fallback_pattern: default_title_tag_fallback_pattern(),
            slug_style_terms: default_slug_style_terms(),
            blocked_note_terms: default_blocked_note_terms(),
            slug_keep_terms: default_slug_keep_terms(),
            video_id_styles: default_video_id_styles(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CleanConfig {
    /// Strip iNews control runs at the start of a line (e.g. `.^_^`), `#` marks, and
    /// runs of two or more ASCII dots left behind by hand-typed notes.
    pub strip_marker_symbols: bool,
    /// Drop whole lines that carry no Chinese characters at all — standalone `##`,
    /// `..`, `OK`, `TEST` notes producers leave in the body.
    ///
    /// This is the most aggressive of the three, and the one that could plausibly
    /// eat real content (a line holding only a URL or only a date), so it is exposed
    /// as a switch rather than hardcoded.
    pub drop_non_cjk_lines: bool,
    /// Strip trailing ASCII symbols from the very end of the body (`##`, `..`),
    /// keeping CJK punctuation such as `。` intact.
    pub strip_trailing_symbols: bool,
}

impl Default for CleanConfig {
    fn default() -> Self {
        Self { strip_marker_symbols: true, drop_non_cjk_lines: true, strip_trailing_symbols: true }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarkersConfig {
    pub refresh_keywords: Vec<String>,
}

impl Default for MarkersConfig {
    fn default() -> Self {
        Self { refresh_keywords: vec!["抓新".into(), "抓".into()] }
    }
}

/// Producer notes in the 編輯備註 header field, normalised into a single marker that
/// gets prefixed onto the slug line at output time.
///
/// Matching is against these exact terms, never a single character: a real script
/// carried the note 「切勿黑畫面」, which a loose 「勿」 test would wrongly flag as
/// do-not-publish. 註記 is deliberately not consulted — in practice it holds a
/// camera operator's name.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnnotationsConfig {
    pub no_upload_terms: Vec<String>,
    pub no_upload_label: String,
    pub copyright_terms: Vec<String>,
    pub copyright_label: String,
    pub allowed_upload_terms: Vec<String>,
    pub allowed_upload_label: String,

    /// Substring in 編輯備註 that marks an exclusive (real notes look like `獨`,
    /// `C獨主`, `獨家 修標7千人`).
    pub exclusive_terms: Vec<String>,
    pub exclusive_prefix: String,
    /// Styles that make a story "breaking"; kept separate from `allowed_styles`
    /// because passing the filter and being time-critical are different questions.
    pub latest_styles: Vec<String>,
    pub latest_prefix: String,
}

impl Default for AnnotationsConfig {
    fn default() -> Self {
        Self {
            no_upload_terms: [
                "勿上網", "勿上YT", "勿YT", "不上YT", "不上網", "不po網", "版權勿上",
                "不要上網", "勿網", "網勿",
            ]
            .iter()
            .map(|s| s.to_string())
            .collect(),
            no_upload_label: "(勿上網)".into(),
            copyright_terms: ["未授權", "不授權", "版權問題"].iter().map(|s| s.to_string()).collect(),
            copyright_label: "(版權問題)".into(),
            allowed_upload_terms: ["已授權", "授權可上", "可上YT", "可上網"]
                .iter()
                .map(|s| s.to_string())
                .collect(),
            allowed_upload_label: "(可上網)".into(),

            exclusive_terms: vec!["獨".into()],
            exclusive_prefix: "獨家》".into(),
            latest_styles: [
                "live", "slive", "sl", "4G", "SNG", "推播", "連線", "旋風", "閃電", "海神",
            ]
            .iter()
            .map(|s| s.to_string())
            .collect(),
            latest_prefix: "最新》".into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PunctuationConfig {
    pub quotes_to_corner: bool,
    pub dot_to_enumeration: bool,
    pub protect_urls: bool,
    pub preserve_halfwidth_space: bool,
    pub map: BTreeMap<String, String>,
}

impl Default for PunctuationConfig {
    fn default() -> Self {
        let mut map = BTreeMap::new();
        map.insert(",".into(), "，".into());
        map.insert(":".into(), "：".into());
        map.insert(";".into(), "；".into());
        map.insert("?".into(), "？".into());
        map.insert("!".into(), "！".into());
        map.insert("(".into(), "（".into());
        map.insert(")".into(), "）".into());
        Self {
            quotes_to_corner: true,
            dot_to_enumeration: true,
            protect_urls: true,
            preserve_halfwidth_space: true,
            map,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutputConfig {
    pub keyword_count: usize,
    pub keyword_separator: String,
    pub entry_blank_lines: usize,
}

impl Default for OutputConfig {
    fn default() -> Self {
        Self { keyword_count: 4, keyword_separator: " ".into(), entry_blank_lines: 1 }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeminiSettingsConfig {
    pub model: String,
    /// How many entries one 產生關鍵字 run may send, so a half-hour batch of dozens of
    /// scripts does not walk straight into the free tier's per-minute request cap and
    /// come back as a screen of failed cards.
    ///
    /// The run stops at this many and reports how many are left; pressing the button
    /// again a minute later picks up exactly the remainder, because entries that
    /// already have keywords are never re-sent.
    #[serde(default = "default_keyword_max_per_run")]
    pub max_per_run: usize,
}

fn default_keyword_max_per_run() -> usize {
    15
}

impl Default for GeminiSettingsConfig {
    fn default() -> Self {
        Self { model: "gemini-3.5-flash-lite".into(), max_per_run: default_keyword_max_per_run() }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ImportConfig {
    /// Empty string means "no default set" — the user must pick a folder each time.
    pub default_folder: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UiConfig {
    /// Colour theme id, matching a `[data-theme]` block in styles.css. Persisted here
    /// (not in browser storage) so a chosen theme survives restarts and stays put
    /// until someone explicitly saves a different one.
    pub theme: String,
}

impl Default for UiConfig {
    fn default() -> Self {
        Self { theme: "light".into() }
    }
}

/// 晚報 (evening-bulletin) list settings.
///
/// Every field carries its own default so a config.toml written before this section
/// existed -- or one where someone deleted a single key -- still loads.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EveningConfig {
    /// Anchors offered in the 主播 picker; the name lands in `【{name}晚報】`.
    #[serde(default = "default_evening_anchors")]
    pub anchors: Vec<String>,
    /// Prefixes in the shared doc that mean "this story did not go online".
    ///
    /// Deliberately *not* the everyday rule ("any prefix = drop"): there, `YH` / `00`
    /// / `UP` mean someone already uploaded it, which for the 晚報 list is exactly a
    /// story that *is* online. Only these markers keep a story off the list.
    #[serde(default = "default_no_online_markers")]
    pub no_online_markers: Vec<String>,
    /// Rundown styles (樣式) that never run in the 晚報 even though they went online.
    /// Evening rundowns are nearly all SOT/短sot, so only SL needs naming.
    #[serde(default = "default_evening_blocked_styles")]
    pub blocked_styles: Vec<String>,
    /// 專題 never runs in the 晚報 even when its style is SOT. It shows up as a slug
    /// starting with 專 (`專ＯＯＯ`) or as 專 in the rundown's 組. Evening only: the
    /// everyday compare is untouched.
    #[serde(default = "default_evening_excluded_terms")]
    pub excluded_terms: Vec<String>,
}

fn default_evening_anchors() -> Vec<String> {
    vec!["吳宇舒".into(), "房業涵".into()]
}

fn default_no_online_markers() -> Vec<String> {
    // 抓新 has no brackets on purpose: any line with it in front of the slug is skipped.
    ["(勿上網)", "(不上網)", "(版權問題)", "(X)", "(國際)", "(偷拍)", "(官方要求下下)", "抓新"]
        .iter()
        .map(|s| s.to_string())
        .collect()
}

fn default_evening_blocked_styles() -> Vec<String> {
    vec!["SL".into()]
}

fn default_evening_excluded_terms() -> Vec<String> {
    vec!["專".into()]
}

impl Default for EveningConfig {
    fn default() -> Self {
        Self {
            anchors: default_evening_anchors(),
            no_online_markers: default_no_online_markers(),
            blocked_styles: default_evening_blocked_styles(),
            excluded_terms: default_evening_excluded_terms(),
        }
    }
}

/// Where 節次分隔線 goes relative to the show it labels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DividerPosition {
    /// 節次結束後：換節時先寫一行標著剛播完那節的分隔線。
    After,
    /// 節次開始前：新的一節第一次寫入前，先寫一行標著這一節的分隔線。
    Before,
}

/// 從 iNEWS FTP 自動匯入（v0.3.0）。帳號密碼不在這裡 —— 跟 Gemini key 一樣存在系統
/// 憑證管理員，絕對不寫進設定檔。
///
/// Container-level `#[serde(default)]`: a config.toml with only some of these keys
/// (or none -- every file written before v0.3.0) still loads, missing keys taking
/// their defaults.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct InewsConfig {
    /// FTP 主機名稱或 IP（台內 `inews` = 10.3.125.1/.2）。
    pub server: String,
    pub port: u16,
    /// 節次資料夾所在的完整路徑，點號串接（`51新聞台.SHOWS`）。
    pub root: String,
    /// 每一節底下要讀的 queue。
    pub queue: String,
    /// 自動刷新間隔（秒）。
    pub poll_seconds: u32,
    /// 下一節 rundown 沒有硬時間時，整點前幾分鐘換節。
    pub lead_minutes: u32,
    /// float 列（版型、框、SOU）也輸出。
    pub include_floated: bool,
    /// 截止點：最後一則標題含這個詞的稿……
    pub cutoff_preview_keyword: String,
    /// ……之後第一個序號以這個開頭的列（廣告列），從它開始以下不處理。
    pub cutoff_page_prefix: String,
    /// 節次分隔線，`{hh}` = 節次小時（08），`{show}` = 資料夾名稱（0800）。
    pub divider_format: String,
    pub divider_position: DividerPosition,
    /// true = 產生關鍵字後直接寫入協作文件；false = 等使用者按確認（預設）。
    pub auto_write: bool,
    /// NSML 的 cume-time 是空的時候，填入照 iNEWS 方式算出來的累積時間。
    /// 關掉就跟手動「匯出延伸文稿」位元組完全相同。
    pub fill_computed_time: bool,
}

impl Default for InewsConfig {
    fn default() -> Self {
        Self {
            server: "inews".into(),
            port: 21,
            root: "51新聞台.SHOWS".into(),
            queue: "ONAIR".into(),
            poll_seconds: 10,
            lead_minutes: 5,
            include_floated: false,
            cutoff_preview_keyword: "預告".into(),
            cutoff_page_prefix: "=".into(),
            divider_format: "==== {hh} ====".into(),
            divider_position: DividerPosition::After,
            auto_write: false,
            fill_computed_time: true,
        }
    }
}

/// Bump whenever new entries are added to a list field that users already have on
/// disk. See `migrate`.
pub const CURRENT_CONFIG_VERSION: u32 = 7;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Config {
    /// 0 means "written before migrations existed".
    #[serde(default)]
    pub config_version: u32,
    #[serde(default)]
    pub filter: FilterConfig,
    #[serde(default)]
    pub clean: CleanConfig,
    #[serde(default)]
    pub markers: MarkersConfig,
    #[serde(default)]
    pub punctuation: PunctuationConfig,
    #[serde(default)]
    pub output: OutputConfig,
    #[serde(default)]
    pub gemini: GeminiSettingsConfig,
    #[serde(default)]
    pub import: ImportConfig,
    #[serde(default)]
    pub ui: UiConfig,
    #[serde(default)]
    pub annotations: AnnotationsConfig,
    #[serde(default)]
    pub evening: EveningConfig,
    #[serde(default)]
    pub inews: InewsConfig,
}

/// Bring a config written by an older version up to date.
///
/// New *sections* need no help — `#[serde(default)]` fills them in. The problem is
/// new entries in an existing list: a saved `allowed_styles` shadows the default
/// completely, so shipping a wider default list would silently never reach anyone
/// who has ever opened the settings page. This unions the defaults back in.
///
/// Union, not replace: entries the user added stay. The trade-off is that an entry
/// they deliberately *deleted* comes back once, which is why this runs only when the
/// stored version is behind rather than on every load.
pub fn migrate(cfg: &mut Config) -> bool {
    if cfg.config_version >= CURRENT_CONFIG_VERSION {
        return false;
    }

    let defaults = Config::default();
    let union = |current: &mut Vec<String>, defaults: &[String]| {
        for d in defaults {
            if !current.iter().any(|c| c.eq_ignore_ascii_case(d)) {
                current.push(d.clone());
            }
        }
    };

    union(&mut cfg.filter.allowed_styles, &defaults.filter.allowed_styles);
    union(&mut cfg.markers.refresh_keywords, &defaults.markers.refresh_keywords);
    union(&mut cfg.filter.slug_style_terms, &defaults.filter.slug_style_terms);
    union(&mut cfg.filter.slug_keep_terms, &defaults.filter.slug_keep_terms);
    union(&mut cfg.filter.video_id_styles, &defaults.filter.video_id_styles);
    union(&mut cfg.evening.no_online_markers, &defaults.evening.no_online_markers);

    // v0.1.9 switched the slug-line markers from 【】 to half-width (). These are
    // scalar fields, so `union` cannot reach them: rewrite only a label still holding
    // the old default, which leaves a label the user typed themselves untouched.
    let retag = |current: &mut String, was: &str, now: &str| {
        if current.trim() == was {
            *current = now.to_string();
        }
    };
    retag(&mut cfg.annotations.no_upload_label, "【勿上網】", &defaults.annotations.no_upload_label);
    retag(&mut cfg.annotations.copyright_label, "【版權問題】", &defaults.annotations.copyright_label);
    retag(
        &mut cfg.annotations.allowed_upload_label,
        "【可上網】",
        &defaults.annotations.allowed_upload_label,
    );
    // v0.2.4 widened the title fallback from `[BAR]` to `[BAR]`/`[BAR_xxx]`.
    retag(
        &mut cfg.filter.title_tag_fallback_pattern,
        LEGACY_TITLE_TAG_FALLBACK_PATTERN,
        &defaults.filter.title_tag_fallback_pattern,
    );

    // v7 (v0.3.0) only added the [inews] section, which `#[serde(default)]` fills in;
    // there is no list to union, so nothing else to do for it here.

    cfg.config_version = CURRENT_CONFIG_VERSION;
    true
}

pub fn load_from_str(s: &str) -> Result<Config, toml::de::Error> {
    toml::from_str(s)
}

pub fn load_from_path(path: &std::path::Path) -> std::io::Result<Config> {
    let s = std::fs::read_to_string(path)?;
    load_from_str(&s).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

pub fn save_to_path(cfg: &Config, path: &std::path::Path) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let s = toml::to_string_pretty(cfg)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    std::fs::write(path, s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_round_trips_through_toml() {
        let cfg = Config::default();
        let s = toml::to_string_pretty(&cfg).unwrap();
        let back: Config = load_from_str(&s).unwrap();
        assert_eq!(back.filter.allowed_styles, cfg.filter.allowed_styles);
        assert_eq!(back.gemini.model, cfg.gemini.model);
        assert_eq!(back.punctuation.map.get(","), Some(&"，".to_string()));
    }
}

#[cfg(test)]
mod migration_tests {
    use super::*;

    /// A config as written by v0.1.2: no version field, and list fields holding the
    /// older, narrower defaults.
    fn legacy_toml() -> &'static str {
        r#"
[filter]
allowed_styles = ["SOT", "短sot", "LIVE", "海神", "閃電", "旋風", "4G", "TEL", "電連"]
blocked_styles = ["BS", "SO"]
excluded_slug_suffixes = ["SOU"]
flag_styles = ["TEL", "電連"]
title_tag_pattern = '^\[BAR_.*大\]$'

[markers]
refresh_keywords = ["抓新"]

[import]
default_folder = 'G:\some\folder'

[ui]
theme = "warm"
"#
    }

    #[test]
    fn new_default_list_entries_reach_a_config_written_before_they_existed() {
        // Without migration a saved list shadows the default completely, so shipping
        // wider defaults would silently never reach anyone who has a config file.
        let mut cfg = load_from_str(legacy_toml()).unwrap();
        assert!(!cfg.filter.allowed_styles.iter().any(|s| s == "SNG"));
        assert!(!cfg.markers.refresh_keywords.iter().any(|s| s == "抓"));

        assert!(migrate(&mut cfg), "a legacy config needs migrating");

        for style in ["SNG", "推播", "連線", "SLIVE", "SL"] {
            assert!(cfg.filter.allowed_styles.iter().any(|s| s == style), "missing {style}");
        }
        assert!(cfg.markers.refresh_keywords.iter().any(|s| s == "抓"));
        assert!(cfg.filter.slug_style_terms.iter().any(|s| s == "推播"));
    }

    #[test]
    fn a_config_file_predating_slug_style_terms_still_loads() {
        // FilterConfig's fields are not individually `#[serde(default)]`, so a new
        // field without its own default would make every existing config.toml fail
        // to parse — the app would come up with factory settings.
        let cfg = load_from_str(legacy_toml()).unwrap();
        assert_eq!(cfg.filter.slug_style_terms, vec!["推播".to_string(), "SLIVE".to_string(), "SL".to_string(), "連線".to_string()]);
        assert_eq!(cfg.filter.blocked_note_terms, vec!["預告".to_string()]);
    }

    #[test]
    fn migration_preserves_the_users_own_settings_and_additions() {
        let mut cfg = load_from_str(legacy_toml()).unwrap();
        cfg.filter.allowed_styles.push("自訂樣式".into());
        migrate(&mut cfg);

        assert_eq!(cfg.ui.theme, "warm", "theme must survive");
        assert_eq!(cfg.import.default_folder, r"G:\some\folder", "folder must survive");
        assert!(cfg.filter.allowed_styles.iter().any(|s| s == "自訂樣式"), "user entry must survive");
        assert!(cfg.filter.allowed_styles.iter().any(|s| s == "SOT"), "original entry must survive");
    }

    #[test]
    fn migration_is_idempotent_and_does_not_run_twice() {
        let mut cfg = load_from_str(legacy_toml()).unwrap();
        assert!(migrate(&mut cfg));
        let after_first = cfg.filter.allowed_styles.clone();

        assert!(!migrate(&mut cfg), "already-current config must not migrate again");
        assert_eq!(cfg.filter.allowed_styles, after_first, "no duplicate entries");
    }

    #[test]
    fn a_saved_full_width_marker_label_is_rewritten_to_half_width() {
        // The labels are scalar strings, so nothing else in `migrate` can reach them:
        // without this, anyone who has opened the settings page keeps 【勿上網】.
        let mut cfg = load_from_str(legacy_toml()).unwrap();
        cfg.annotations.no_upload_label = "【勿上網】".into();
        cfg.annotations.copyright_label = "【版權問題】".into();
        cfg.annotations.allowed_upload_label = "【可上網】".into();
        migrate(&mut cfg);
        assert_eq!(cfg.annotations.no_upload_label, "(勿上網)");
        assert_eq!(cfg.annotations.copyright_label, "(版權問題)");
        assert_eq!(cfg.annotations.allowed_upload_label, "(可上網)");
    }

    #[test]
    fn the_old_bar_only_title_fallback_is_widened_but_a_custom_one_is_kept() {
        let mut cfg = load_from_str(legacy_toml()).unwrap();
        cfg.filter.title_tag_fallback_pattern = LEGACY_TITLE_TAG_FALLBACK_PATTERN.into();
        migrate(&mut cfg);
        assert_eq!(cfg.filter.title_tag_fallback_pattern, default_title_tag_fallback_pattern());

        let mut custom = load_from_str(legacy_toml()).unwrap();
        custom.filter.title_tag_fallback_pattern = r"^\[自訂\]$".into();
        migrate(&mut custom);
        assert_eq!(custom.filter.title_tag_fallback_pattern, r"^\[自訂\]$");
    }

    #[test]
    fn a_hand_typed_marker_label_survives_migration() {
        let mut cfg = load_from_str(legacy_toml()).unwrap();
        cfg.annotations.no_upload_label = "★禁上網★".into();
        migrate(&mut cfg);
        assert_eq!(cfg.annotations.no_upload_label, "★禁上網★");
    }

    #[test]
    fn a_freshly_defaulted_config_is_already_current() {
        let mut cfg = Config::default();
        cfg.config_version = CURRENT_CONFIG_VERSION;
        assert!(!migrate(&mut cfg));
    }

    #[test]
    fn a_config_file_predating_the_evening_section_gets_its_defaults() {
        let cfg = load_from_str(legacy_toml()).unwrap();
        assert_eq!(cfg.evening.anchors, vec!["吳宇舒".to_string(), "房業涵".to_string()]);
        assert!(cfg.evening.no_online_markers.iter().any(|m| m == "(勿上網)"));
    }

    /// A config as written by v0.2.9 (version 6), with some settings the user changed.
    fn v6_toml() -> String {
        let mut cfg = Config::default();
        cfg.config_version = 6;
        cfg.ui.theme = "dark".into();
        cfg.import.default_folder = r"D:\iNews".into();
        cfg.filter.allowed_styles.push("自訂樣式".into());
        cfg.gemini.max_per_run = 7;
        let mut s = toml::to_string_pretty(&cfg).unwrap();
        // v0.2.9 had no [inews] section at all.
        let at = s.find("[inews]").unwrap();
        s.truncate(at);
        s
    }

    #[test]
    fn a_version_6_config_loads_with_inews_defaults_and_keeps_its_settings() {
        let mut cfg = load_from_str(&v6_toml()).unwrap();
        assert_eq!(cfg.inews.server, "inews");
        assert_eq!(cfg.inews.root, "51新聞台.SHOWS");
        assert_eq!(cfg.inews.queue, "ONAIR");
        assert_eq!(cfg.inews.poll_seconds, 10);
        assert!(!cfg.inews.auto_write, "confirm mode is the default");
        assert_eq!(cfg.inews.divider_format, "==== {hh} ====");

        assert!(migrate(&mut cfg));
        assert_eq!(cfg.config_version, CURRENT_CONFIG_VERSION);
        assert_eq!(cfg.ui.theme, "dark");
        assert_eq!(cfg.import.default_folder, r"D:\iNews");
        assert_eq!(cfg.gemini.max_per_run, 7);
        assert!(cfg.filter.allowed_styles.iter().any(|s| s == "自訂樣式"));
        assert_eq!(cfg.filter.allowed_styles.iter().filter(|s| *s == "SOT").count(), 1);
    }

    #[test]
    fn a_partial_inews_section_keeps_what_is_there_and_defaults_the_rest() {
        let s = format!("{}\n[inews]\nserver = \"10.3.125.2\"\ndivider_position = \"before\"\n", v6_toml());
        let cfg = load_from_str(&s).unwrap();
        assert_eq!(cfg.inews.server, "10.3.125.2");
        assert_eq!(cfg.inews.divider_position, DividerPosition::Before);
        assert_eq!(cfg.inews.queue, "ONAIR");
        assert_eq!(cfg.inews.lead_minutes, 5);
    }

    #[test]
    fn missing_sections_fall_back_to_defaults_without_migration() {
        // The legacy file has no [annotations] and no [clean]; serde fills them in.
        let cfg = load_from_str(legacy_toml()).unwrap();
        assert_eq!(cfg.annotations.no_upload_label, "(勿上網)");
        assert!(cfg.clean.strip_marker_symbols);
    }
}
