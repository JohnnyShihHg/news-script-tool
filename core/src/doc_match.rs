/// Status of one iNews slug against the work-doc text, per spec §6.
#[derive(Debug, Clone, PartialEq)]
pub enum MatchStatus {
    /// Not found in the doc at all — today's news to cut.
    ToCut,
    /// Found with nothing but whitespace before it (a line this tool wrote, or one a
    /// person added by hand), or a `(抓新)` line for the same 累積時間 — already in the
    /// doc, never written again.
    ///
    /// Used to be folded into ToCut with `matched_line` set, which left every caller
    /// one forgotten check away from writing a hand-added story a second time.
    InDoc,
    /// Found with a prefix containing a refresh keyword (e.g. "抓新") — already
    /// uploaded once but flagged to re-paste.
    KeepRefresh,
    /// Found with some other non-empty prefix before it — already handled, drop it.
    Removed,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MatchResult {
    pub status: MatchStatus,
    /// The doc line that matched, if any (for the user to double-check the call).
    pub matched_line: Option<String>,
    /// The match came from the previous day's section (same slug and same title): a
    /// re-run of yesterday's story, not a new one.
    pub cross_day: bool,
}

impl MatchResult {
    fn miss() -> Self {
        MatchResult { status: MatchStatus::ToCut, matched_line: None, cross_day: false }
    }
}

/// Fold fullwidth ASCII (｜FF01–FF5E) and the fullwidth space (U+3000) down to their
/// halfwidth equivalents, and collapse/trim whitespace, so slug text copied from
/// iNews compares equal to the same text as typed/pasted in the work doc.
fn normalize_for_match(s: &str) -> String {
    let folded: String = s
        .chars()
        .map(|c| {
            let code = c as u32;
            if (0xFF01..=0xFF5E).contains(&code) {
                char::from_u32(code - 0xFEE0).unwrap_or(c)
            } else if code == 0x3000 {
                ' '
            } else {
                c
            }
        })
        .collect();
    folded.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// `HH:MM:SS` zero-padded to 8 chars, or None if `s` is not a time.
fn padded_time(s: &str) -> Option<String> {
    static RE: once_cell::sync::Lazy<regex::Regex> =
        once_cell::sync::Lazy::new(|| regex::Regex::new(r"^\d{1,2}:\d{2}:\d{2}$").unwrap());
    let s = s.trim();
    RE.is_match(s).then(|| format!("{s:0>8}"))
}

/// The first `HH:MM:SS` on a doc line after the slug -- the 累積時間 this tool writes
/// into its header line, which producers copy along when they tag it `(抓新)`.
fn time_after(hay: &str) -> Option<String> {
    static RE: once_cell::sync::Lazy<regex::Regex> =
        once_cell::sync::Lazy::new(|| regex::Regex::new(r"\d{1,2}:\d{2}:\d{2}").unwrap());
    RE.find(hay).and_then(|m| padded_time(m.as_str()))
}

/// Classify one slug against the work-doc text, ignoring time. See `match_entry`.
pub fn match_slug(slug: &str, doc_text: &str, refresh_keywords: &[String]) -> MatchResult {
    match_entry(slug, "", doc_text, refresh_keywords)
}

/// Classify one entry (slug + 累積時間) against the work-doc text (one entry per line,
/// as pasted or scraped verbatim). See spec §6 for the "prefix before the slug" rule.
///
/// Takes the **last** matching line, not the first. The same slug legitimately
/// appears more than once across a shift: a producer's original `(抓新) slug` request
/// line, followed later by the plain `slug` line this tool itself writes back. Taking
/// the first line would keep reading the original `(抓新)` request forever and write
/// the same story back on every re-run, even after it was already handled. The last
/// line is whatever is currently true about the story -- if someone adds a fresh
/// `(抓新) slug` line below the plain one later, that also correctly asks for a
/// re-paste again.
///
/// A `(抓新)` line carrying the same time as the entry is the version already in
/// the doc, not a new one: it reads as already there. Without this, a stale scrape
/// (the written-back line not in it yet) kept asking for the same version again --
/// a re-grab only makes sense once iNews has a different 累積時間. Lines or entries
/// without a time fall back to the slug-only rule.
pub fn match_entry(slug: &str, time: &str, doc_text: &str, refresh_keywords: &[String]) -> MatchResult {
    let needle = normalize_for_match(slug);
    if needle.is_empty() {
        return MatchResult::miss();
    }
    let entry_time = padded_time(time);
    doc_text
        .lines()
        .rev()
        .find_map(|line| line_status(line, &needle, entry_time.as_deref(), refresh_keywords))
        .unwrap_or_else(MatchResult::miss)
}

/// Whether the text before a slug marks it for a re-grab: one of the refresh
/// keywords (抓新), or a bracket holding a 1-2 digit number -- `(19)slug1800` reads
/// 「抓 19」, the version after the 19:00 show, and counts the same as (抓新); so do
/// `(25)` and `(X/19)` (v0.3.7, user's call). A 4-digit `(2100)` does not, and bare
/// codes such as `00` / `03` outside brackets still mean "handled".
fn is_refresh_prefix(prefix: &str, refresh_keywords: &[String]) -> bool {
    static BRACKET: once_cell::sync::Lazy<regex::Regex> =
        once_cell::sync::Lazy::new(|| regex::Regex::new(r"\(([^()]*)\)").unwrap());
    static SHORT_NUMBER: once_cell::sync::Lazy<regex::Regex> =
        once_cell::sync::Lazy::new(|| regex::Regex::new(r"(^|\D)\d{1,2}(\D|$)").unwrap());
    refresh_keywords.iter().any(|k| prefix.contains(normalize_for_match(k).as_str()))
        || BRACKET.captures_iter(prefix).any(|c| SHORT_NUMBER.is_match(&c[1]))
}

/// How one doc line reads for the slug `needle` (already normalized), or None when
/// the slug is not on it.
fn line_status(line: &str, needle: &str, entry_time: Option<&str>, refresh_keywords: &[String]) -> Option<MatchResult> {
    let hay = normalize_for_match(line);
    let idx = hay.find(needle)?;
    let prefix = hay[..idx].trim();
    let status = if prefix.is_empty() {
        MatchStatus::InDoc
    } else if is_refresh_prefix(prefix, refresh_keywords) {
        match (entry_time, time_after(&hay[idx + needle.len()..])) {
            (Some(e), Some(l)) if e == l => MatchStatus::InDoc,
            _ => MatchStatus::KeepRefresh,
        }
    } else {
        MatchStatus::Removed
    };
    Some(MatchResult { status, matched_line: Some(line.trim().to_string()), cross_day: false })
}

/// Classify every (slug, 累積時間) entry against the work-doc text in one pass.
pub fn match_all<'a>(
    entries: impl IntoIterator<Item = (&'a str, &'a str)>,
    doc_text: &str,
    refresh_keywords: &[String],
) -> Vec<(&'a str, MatchResult)> {
    entries
        .into_iter()
        .map(|(slug, time)| (slug, match_entry(slug, time, doc_text, refresh_keywords)))
        .collect()
}

// --- 只比對今天的段落（v0.3.2） ---------------------------------------------------
//
// 協作文件通常同時留著今天和前一天的內容，slug 又會跨天重複使用（每天都有
// `季瑩氣象2100`），掃整份會把今天的新稿當成「已上傳」。所以今天的段落照原本的規則
// 比對；前一天的段落只有 slug 和標題都相同才算重複（真的重播）。

/// 協作文件依日期分隔線切出來的兩段。
#[derive(Debug, Clone, PartialEq)]
pub struct DaySections {
    /// 今天的段落；文件裡還沒有今天的分隔線是 None。
    pub today: Option<String>,
    /// 前一天的段落。
    pub previous: Option<String>,
    /// 文件裡至少有一條日期分隔線。沒有的話只能退回掃整份。
    pub has_dividers: bool,
    /// 照文件裡最後一條日期分隔線的格式，換成今天日期的分隔線（今天的段落不存在時
    /// 寫入前補上）。
    pub today_header: Option<String>,
}

pub fn day_sections(doc_text: &str, today: &str, previous: &str) -> DaySections {
    let last_divider = doc_text.lines().rev().find(|l| date_divider(l).is_some());
    let today_header = last_divider.map(|line| {
        let line = line.trim();
        let mmdd = date_divider(line).unwrap_or_default();
        // The digits may have been typed fullwidth; replace whichever form is there.
        match line.rfind(&mmdd) {
            Some(i) => format!("{}{}{}", &line[..i], today, &line[i + mmdd.len()..]),
            None => {
                let wide: String = mmdd.chars().map(|c| char::from_u32(c as u32 + 0xFEE0).unwrap_or(c)).collect();
                line.replacen(&wide, today, 1)
            }
        }
    });
    DaySections {
        today: date_section(doc_text, today),
        previous: date_section(doc_text, previous),
        has_dividers: last_divider.is_some(),
        today_header,
    }
}

/// 標題比對前的正規化：全形轉半形，去掉工具自己加的前綴（`最新》`、`獨家》`）和標記
/// （`(勿上網)` 等），再去掉所有空白和標點符號。只留文字和數字，完全相同才算同一則。
pub fn normalize_title(title: &str, strip: &[String]) -> String {
    let mut s = normalize_for_match(title);
    for tag in strip {
        let tag = normalize_for_match(tag);
        if !tag.is_empty() {
            s = s.replace(&tag, "");
        }
    }
    s.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect()
}

/// 比對一則稿件需要的東西。
#[derive(Debug, Clone, Copy)]
pub struct EntryQuery<'a> {
    pub slug: &'a str,
    pub time: &'a str,
    pub title: &'a str,
}

/// 以日期段落為範圍比對一則稿件。
///
/// - 文件沒有任何日期分隔線：退回掃整份（呼叫端要提醒使用者）。
/// - 今天的段落有這個 slug：照 `match_entry` 的規則（同一天重播 = 已上傳）。
/// - 今天沒有、前一天有：`title_strip` 是 Some（有開跨天標題比對）而且兩邊都抓得到
///   標題、正規化後相同，而且前一天那一行 slug 前面有處理標記，才算已上傳的重播
///   （`Removed` + `cross_day`）；前一天沒標記或是 (抓新) 都還沒上，跟新稿一樣要寫。
///   標題取那一行下面第一個非空白行 —— 這個工具寫進文件的第二行就是標題。
pub fn match_in_days(
    q: EntryQuery,
    days: &DaySections,
    whole_doc: &str,
    refresh_keywords: &[String],
    title_strip: Option<&[String]>,
) -> MatchResult {
    if !days.has_dividers {
        return match_entry(q.slug, q.time, whole_doc, refresh_keywords);
    }
    let today = match_entry(q.slug, q.time, days.today.as_deref().unwrap_or(""), refresh_keywords);
    if today.matched_line.is_some() {
        return today;
    }
    let (Some(strip), Some(previous)) = (title_strip, days.previous.as_deref()) else {
        return today;
    };
    let wanted = normalize_title(q.title, strip);
    let needle = normalize_for_match(q.slug);
    if wanted.is_empty() || needle.is_empty() {
        return today;
    }
    let lines: Vec<&str> = previous.lines().collect();
    let entry_time = padded_time(q.time);
    (0..lines.len())
        .rev()
        .find_map(|i| {
            let r = line_status(lines[i], &needle, entry_time.as_deref(), refresh_keywords)?;
            let doc_title = lines[i + 1..].iter().find(|l| !l.trim().is_empty())?;
            (normalize_title(doc_title, strip) == wanted).then_some(r)
        })
        .map(|r| match r.status {
            // 前一天有處理標記（00、UP、(X/…)）＝已上傳過的重播，不寫。
            MatchStatus::Removed => MatchResult { cross_day: true, ..r },
            // 前一天 slug 前面沒標記＝還沒處理；(抓新) 的隔天沒上也要重抓。都當新稿寫（v0.3.5）。
            _ => today.clone(),
        })
        .unwrap_or(today)
}

/// 決策紀錄用：前一天段落裡出現這個 slug 的每一行，和它下面那一行標題（正規化後）。
pub fn previous_day_lines(slug: &str, days: &DaySections, strip: &[String]) -> Vec<(String, String)> {
    let needle = normalize_for_match(slug);
    let Some(previous) = days.previous.as_deref() else { return Vec::new() };
    if needle.is_empty() {
        return Vec::new();
    }
    let lines: Vec<&str> = previous.lines().collect();
    (0..lines.len())
        .filter(|&i| normalize_for_match(lines[i]).contains(&needle))
        .map(|i| {
            let title = lines[i + 1..].iter().find(|l| !l.trim().is_empty()).map(|t| normalize_title(t, strip));
            (lines[i].trim().to_string(), title.unwrap_or_default())
        })
        .collect()
}

// --- 晚報 (evening bulletin) ---------------------------------------------------
//
// The evening list asks the opposite question from the everyday compare. There, a
// slug *missing* from the doc is news to cut; here, only a story that is in the doc
// under the chosen day *and* went online counts, so "not found" is a miss.
//
// A prefix alone does not keep a story off. The 1800 bulletin re-runs (possibly
// revised) 1200 stories whose doc lines already carry an "uploaded" prefix such as
// YH / 00 / UP, and those must stay selectable. Only a bracketed tag in front of the
// slug -- (YT), (勿上網), (x/threads)... the tags that mean it never went online,
// exactly as in the everyday compare -- or one of the configured markers (抓新)
// keeps it off.

/// Whether a line is a date divider (`========1017========`), returning its MMDD.
///
/// Date dividers carry `=` on both sides, but so do some hand-typed time-slot
/// dividers (`====0900====`), so the `=` alone cannot tell them apart. What does:
/// slots only ever mark the hour (`HH00`: 0900, 1800, 2400, 0000), while a date is
/// a real MMDD (1028, 1030) -- month 01-12, day 01-31, so never `..00` (v0.3.6).
/// The real doc mixes spaces, tabs and fullwidth `＝`, so the line is normalized first.
fn date_divider(line: &str) -> Option<String> {
    static RE: once_cell::sync::Lazy<regex::Regex> =
        once_cell::sync::Lazy::new(|| regex::Regex::new(r"^=+[=\s]*(\d{4})\s*=[=\s]*$").unwrap());
    let norm = normalize_for_match(line);
    let mmdd = RE.captures(&norm)?[1].to_string();
    // 節次分隔線只標整點（0900、1800、2400），日期是 MMDD（1028、1030）。
    let (mm, dd): (u32, u32) = (mmdd[..2].parse().ok()?, mmdd[2..].parse().ok()?);
    ((1..=12).contains(&mm) && (1..=31).contains(&dd)).then_some(mmdd)
}

/// The lines of the doc that sit under the `mmdd` date divider, up to the next date
/// divider. If the same day appears more than once (someone re-pasted a divider),
/// every block for it is included. `None` when the doc has no such divider at all,
/// so the caller can tell "wrong date" apart from "nothing matched".
pub fn date_section(doc_text: &str, mmdd: &str) -> Option<String> {
    let mut found = false;
    let mut inside = false;
    let mut out = Vec::new();
    for line in doc_text.lines() {
        if let Some(d) = date_divider(line) {
            inside = d == mmdd;
            found |= inside;
            continue;
        }
        if inside {
            out.push(line);
        }
    }
    found.then(|| out.join("\n"))
}

#[derive(Debug, Clone, PartialEq)]
pub enum EveningStatus {
    /// In the day's section and online: nothing before the slug, or only an
    /// "uploaded" code such as YH / 00 / UP.
    Hit,
    /// In the day's section, but a bracketed tag or a configured marker before it
    /// says it never went online.
    NoOnline,
    /// Not in the day's section at all.
    NotFound,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EveningMatch {
    pub status: EveningStatus,
    pub matched_line: Option<String>,
}

/// Whether an (already normalized) prefix holds a bracketed tag like `(YT)` or
/// `【勿上網】`. Fullwidth `（）` has been folded to `()` by `normalize_for_match`.
fn has_bracket_tag(prefix: &str) -> bool {
    static RE: once_cell::sync::Lazy<regex::Regex> =
        once_cell::sync::Lazy::new(|| regex::Regex::new(r"\([^()]*\)|【[^【】]*】").unwrap());
    RE.is_match(prefix)
}

/// Classify one evening-rundown slug against one day's section of the doc.
///
/// Last matching line wins, for the same reason as `match_slug`: a later line is
/// whatever is currently true about the story.
pub fn match_evening(slug: &str, section_text: &str, no_online_markers: &[String]) -> EveningMatch {
    let needle = normalize_for_match(slug);
    let miss = EveningMatch { status: EveningStatus::NotFound, matched_line: None };
    if needle.is_empty() {
        return miss;
    }
    let markers: Vec<String> = no_online_markers
        .iter()
        .map(|m| normalize_for_match(m))
        .filter(|m| !m.is_empty())
        .collect();

    let mut result = None;
    for line in section_text.lines() {
        let hay = normalize_for_match(line);
        if let Some(idx) = hay.find(&needle) {
            let prefix = &hay[..idx];
            let status = if has_bracket_tag(prefix) || markers.iter().any(|m| prefix.contains(m.as_str())) {
                EveningStatus::NoOnline
            } else {
                EveningStatus::Hit
            };
            result = Some(EveningMatch { status, matched_line: Some(line.trim().to_string()) });
        }
    }
    result.unwrap_or(miss)
}

#[cfg(test)]
mod evening_tests {
    use super::*;

    fn markers() -> Vec<String> {
        crate::config::EveningConfig::default().no_online_markers
    }

    // Synthetic doc: two days, slot dividers, a messy fullwidth/tab divider, and the
    // same slug on both days so the day boundary is what decides.
    const DOC: &str = "\
========1016========
合成昨日新聞1200 SOT 12:00:00
合成跨日新聞1800 SOT 18:00:00
＝＝＝＝ 1017\t＝＝＝＝
(X)合成頂端清單0600\t短SOT
======1200
合成排隊新聞1200 SOT 12:06:50 生活
(勿上網)合成保固新聞1200 SOT 12:10:00
YH 合成已上傳新聞1200 SOT 12:20:00
（國際）合成外電新聞1200 SOT 12:30:00
(抓新) 合成重貼新聞1200 SOT 12:40:00
======1800
合成跨日新聞1800 SOT 18:00:00
========1018========
合成明日新聞0600 SOT 06:00:00
";

    fn section() -> String {
        date_section(DOC, "1017").expect("1017 section")
    }

    #[test]
    fn section_stops_at_the_next_day_and_ignores_slot_dividers() {
        let s = section();
        assert!(s.contains("合成排隊新聞1200"));
        assert!(s.contains("======1800"), "slot dividers stay inside the day");
        assert!(!s.contains("合成昨日新聞1200"));
        assert!(!s.contains("合成明日新聞0600"));
    }

    #[test]
    fn a_missing_day_is_none_not_an_empty_section() {
        assert_eq!(date_section(DOC, "1231"), None);
    }

    #[test]
    fn a_time_slot_divider_is_not_a_date_divider() {
        assert_eq!(date_divider("======1200"), None);
        assert_eq!(date_divider("========0811========="), Some("0811".to_string()));
        // 有人把節次分隔線兩邊都打 =：只標整點的就是節次（v0.3.6）。
        for slot in ["====0900====", "====1500====", "========0000", "====2400====", "===1300==="] {
            assert_eq!(date_divider(slot), None, "{slot}");
        }
        assert_eq!(date_divider("＝＝＝＝＝＝＝＝＝＝＝＝1028＝＝＝＝＝＝＝＝＝＝＝＝＝"), Some("1028".to_string()));
        assert_eq!(date_divider("=====1030====="), Some("1030".to_string()));
        assert_eq!(date_divider("=====1332====="), None);
    }

    #[test]
    fn plain_line_in_the_day_is_a_hit() {
        let r = match_evening("合成排隊新聞1200", &section(), &markers());
        assert_eq!(r.status, EveningStatus::Hit);
    }

    #[test]
    fn uploaded_codes_stay_selectable_for_a_rerun() {
        // A 1200 story re-run at 1800 already carries its uploaded prefix.
        assert_eq!(match_evening("合成已上傳新聞1200", &section(), &markers()).status, EveningStatus::Hit);
        for line in [
            "00 合成重播新聞1200 SOT",
            "UP 合成重播新聞1200 SOT",
            "03 合成重播新聞1200 SOT",
            "**合成重播新聞1200 SOT",
            "** 合成重播新聞1200 SOT",
            "＊＊合成重播新聞1200 SOT",
        ] {
            assert_eq!(match_evening("合成重播新聞1200", line, &markers()).status, EveningStatus::Hit, "{line}");
        }
    }

    #[test]
    fn any_bracketed_tag_means_it_never_went_online() {
        for line in [
            "(YT)合成平台新聞1800 SOT",
            "（ＹＴ）合成平台新聞1800 SOT",
            "(x/threads) 合成平台新聞1800 SOT",
            "【勿上網】合成平台新聞1800 SOT",
        ] {
            assert_eq!(match_evening("合成平台新聞1800", line, &markers()).status, EveningStatus::NoOnline, "{line}");
        }
    }

    #[test]
    fn a_grab_new_line_is_skipped_even_though_it_is_online() {
        assert_eq!(match_evening("合成重貼新聞1200", &section(), &markers()).status, EveningStatus::NoOnline);
        let fullwidth = "（抓新）合成重貼新聞1200 SOT";
        assert_eq!(match_evening("合成重貼新聞1200", fullwidth, &markers()).status, EveningStatus::NoOnline);
    }

    #[test]
    fn do_not_publish_markers_are_not_online_including_fullwidth_brackets() {
        for slug in ["合成保固新聞1200", "合成外電新聞1200", "合成頂端清單0600"] {
            assert_eq!(match_evening(slug, &section(), &markers()).status, EveningStatus::NoOnline, "{slug}");
        }
    }

    #[test]
    fn a_story_only_on_another_day_is_not_found() {
        let r = match_evening("合成昨日新聞1200", &section(), &markers());
        assert_eq!(r.status, EveningStatus::NotFound);
        assert_eq!(r.matched_line, None);
    }

    #[test]
    fn a_slug_on_two_days_only_reads_the_chosen_one() {
        assert_eq!(match_evening("合成跨日新聞1800", &section(), &markers()).status, EveningStatus::Hit);
        let other = date_section(DOC, "1018").unwrap();
        assert_eq!(match_evening("合成跨日新聞1800", &other, &markers()).status, EveningStatus::NotFound);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn refresh() -> Vec<String> {
        vec!["抓新".to_string()]
    }

    // Synthetic rundown lines. Their *shapes* mirror what the live collaboration tool
    // produces -- date/section dividers, bracketed and numeric prefixes, a refresh tag
    // sitting directly before a slug and another buried further back, and fullwidth
    // punctuation -- because those shapes are what the matching rules turn on. The
    // wording is invented: real rundowns carry unpublished copy and are marked 勿上網.
    const DOC_TEXT: &str = "\
========0807========
(勿上網)合成保固報導0600 短sot
(勿上網)合成警務報導0630 短sot
======0600
03 合成開幕報導0600 BS 06：08：32
YH 合成氣象 06：24：02
======1800
03 合成飲料報導1200 SOT 12:06:50 中部中心 06/17
(抓新) 合成物價報導1800 SOT 18:17:54 生活
抓新 20後(合成事務所YT) 合成司法報導1800 SOT 19:02:27 中部中心 獨
合成颱風報導1530 直進 15:30:37
";

    #[test]
    fn slug_with_no_prefix_is_already_in_the_doc() {
        let r = match_slug("合成颱風報導1530", DOC_TEXT, &refresh());
        assert_eq!(r.status, MatchStatus::InDoc);
        assert!(r.matched_line.is_some());
    }

    #[test]
    fn slug_missing_from_doc_entirely_is_to_cut() {
        let r = match_slug("完全沒出現的新聞0000", DOC_TEXT, &refresh());
        assert_eq!(r.status, MatchStatus::ToCut);
        assert_eq!(r.matched_line, None);
    }

    #[test]
    fn slug_with_non_refresh_prefix_is_removed() {
        let r = match_slug("合成保固報導0600", DOC_TEXT, &refresh());
        assert_eq!(r.status, MatchStatus::Removed);
    }

    #[test]
    fn slug_with_numeric_prefix_is_removed() {
        let r = match_slug("合成飲料報導1200", DOC_TEXT, &refresh());
        assert_eq!(r.status, MatchStatus::Removed);
    }

    #[test]
    fn slug_with_refresh_keyword_directly_before_is_kept() {
        let r = match_slug("合成物價報導1800", DOC_TEXT, &refresh());
        assert_eq!(r.status, MatchStatus::KeepRefresh);
    }

    #[test]
    fn an_hour_in_brackets_before_the_slug_counts_as_a_regrab() {
        // (19)slug1800 = 抓 19 那節播出後的版本，跟 (抓新) 一樣（v0.3.7）。
        for line in [
            "(19)合成抓節次1800 SOT 18:20:00",
            "（19）合成抓節次1800",
            "00 ( 9 ) 合成抓節次1800",
            "(0)合成抓節次1800",
            "(25)合成抓節次1800",
            "(X/19)合成抓節次1800",
        ] {
            let r = match_slug("合成抓節次1800", line, &refresh());
            assert_eq!(r.status, MatchStatus::KeepRefresh, "{line}");
        }
        // 時間跟文件那行相同＝已經是抓回來的版本。
        let r = match_entry("合成抓節次1800", "18:20:00", "(19)合成抓節次1800 SOT 18:20:00", &refresh());
        assert_eq!(r.status, MatchStatus::InDoc);
        // 沒括號的 00、括號裡 3 位數以上、沒有數字的括號標記都不是抓新。
        for line in ["00 合成抓節次1800", "(2100)合成抓節次1800", "(X/threads)合成抓節次1800", "(YT)合成抓節次1800"] {
            let r = match_slug("合成抓節次1800", line, &refresh());
            assert_eq!(r.status, MatchStatus::Removed, "{line}");
        }
    }

    #[test]
    fn slug_with_refresh_keyword_buried_earlier_in_prefix_is_kept() {
        // spec: any non-empty prefix containing the refresh keyword counts, not just
        // an exact "(抓新)" tag -- "抓新 20後(合成事務所YT) " should still match.
        let r = match_slug("合成司法報導1800", DOC_TEXT, &refresh());
        assert_eq!(r.status, MatchStatus::KeepRefresh);
    }

    #[test]
    fn fullwidth_and_halfwidth_slugs_compare_equal() {
        let r = match_slug("合成保固報導０６００", DOC_TEXT, &refresh());
        assert_eq!(r.status, MatchStatus::Removed);
    }

    #[test]
    fn empty_slug_is_to_cut_not_a_false_match() {
        let r = match_slug("", DOC_TEXT, &refresh());
        assert_eq!(r.status, MatchStatus::ToCut);
        assert_eq!(r.matched_line, None);
    }

    // --- Repeat writes: the last occurrence of a slug is what currently matters ---

    const REPEAT_DOC: &str = "\
(抓新)為鸚鵡砸親1100 slive 11:02:06
普渡丟鈔票1100 TEL 11:07:43 中
為鸚鵡砸親1100 slive 11:02:06
";

    #[test]
    fn once_this_tool_has_written_the_plain_line_the_slug_reads_as_already_in_the_doc() {
        // Screenshot scenario: the original (抓新) request line is still first, but a
        // second, unprefixed occurrence was appended by this tool's own write-back.
        // The first-line-wins rule used to keep reporting KeepRefresh forever and
        // re-append the story on every re-run; the last line is what is actually true.
        let r = match_slug("為鸚鵡砸親1100", REPEAT_DOC, &refresh());
        assert_eq!(r.status, MatchStatus::InDoc);
        assert_eq!(r.matched_line.as_deref(), Some("為鸚鵡砸親1100 slive 11:02:06"));
    }

    #[test]
    fn a_fresh_refresh_tag_appended_after_a_plain_line_asks_for_a_re_paste_again() {
        // The reverse direction: a story already written plain, then someone adds a
        // new (抓新) request below it. The last line must win here too.
        let doc = "為鸚鵡砸親1100 slive 11:02:06\n(抓新)為鸚鵡砸親1100 slive 11:02:06\n";
        let r = match_slug("為鸚鵡砸親1100", doc, &refresh());
        assert_eq!(r.status, MatchStatus::KeepRefresh);
    }

    #[test]
    fn three_occurrences_still_resolve_on_the_last_one_not_a_count() {
        let doc = "\
(抓新)重播新聞0900 SOT 09:00:00
重播新聞0900 SOT 09:00:00
03 重播新聞0900 SOT 09:00:00
";
        let r = match_slug("重播新聞0900", doc, &refresh());
        assert_eq!(r.status, MatchStatus::Removed);
        assert_eq!(r.matched_line.as_deref(), Some("03 重播新聞0900 SOT 09:00:00"));
    }

    #[test]
    fn a_grab_new_line_for_the_same_time_reads_as_already_in_the_doc() {
        // Stale scrape: only the producer's request is there, not our written-back
        // line. The entry still at 12:40:00 is the version that line is about.
        let doc = "(抓新) 合成重貼新聞1200 SOT 12:40:00 生活\n";
        let r = match_entry("合成重貼新聞1200", "12:40:00", doc, &refresh());
        assert_eq!(r.status, MatchStatus::InDoc);
        assert!(r.matched_line.is_some(), "must read as already in the doc, not as new");
    }

    #[test]
    fn a_grab_new_line_for_a_different_time_still_asks_for_a_re_paste() {
        let doc = "(抓新) 合成重貼新聞1200 SOT 12:40:00 生活\n";
        let r = match_entry("合成重貼新聞1200", "13:05:10", doc, &refresh());
        assert_eq!(r.status, MatchStatus::KeepRefresh);
        // Single-digit hours and fullwidth colons still compare.
        let doc = "(抓新) 合成重貼新聞0900 SOT 9：05：00\n";
        assert_eq!(match_entry("合成重貼新聞0900", "09:05:00", doc, &refresh()).status, MatchStatus::InDoc);
    }

    #[test]
    fn a_grab_new_line_without_a_time_keeps_the_slug_only_rule() {
        let doc = "(抓新) 合成重貼新聞1200\n";
        let r = match_entry("合成重貼新聞1200", "12:40:00", doc, &refresh());
        assert_eq!(r.status, MatchStatus::KeepRefresh);
    }

    #[test]
    fn date_and_time_divider_lines_never_falsely_match_a_real_slug() {
        let r = match_slug("0807", DOC_TEXT, &refresh());
        // "0807" is a substring of the date divider "========0807========";
        // the divider itself has no real slug semantics but the substring search
        // will still find it -- prefix is "========" (non-empty, no refresh
        // keyword), so it resolves to Removed rather than crashing or panicking.
        assert_eq!(r.status, MatchStatus::Removed);
    }
}

#[cfg(test)]
mod day_tests {
    use super::*;

    fn refresh() -> Vec<String> {
        vec!["抓新".to_string()]
    }

    fn strip() -> Vec<String> {
        ["最新》", "獨家》", "(勿上網)", "(版權問題)", "(可上網)"].iter().map(|s| s.to_string()).collect()
    }

    // Two days in the doc, the same slugs reused across them, in the tool's own
    // four-line write format (header, title, body, keywords).
    const DOC: &str = "\
==========1025==========
季瑩氣象2100 SOT 21:05:00 生活
明天北部轉涼 早晚溫差大
合成昨日氣象內文。
氣象 轉涼

UP 北投強風垮1800 SOT 18:10:00 社會
最新》北投強風！路樹倒塌「壓車」
合成昨日強風內文。
強風 路樹

(抓新) 合成重貼新聞1800 SOT 18:30:00
合成重貼標題
合成內文。

北投路樹1800 SOT 18:40:00
==========1026==========
淑麗氣象0900 SOT 09:05:00 生活
今天各地回溫
合成今日內文。

蔣萬安直進 直進 16:00:00
";

    fn days() -> DaySections {
        day_sections(DOC, "1026", "1025")
    }

    fn q<'a>(slug: &'a str, title: &'a str) -> EntryQuery<'a> {
        EntryQuery { slug, time: "", title }
    }

    fn check(slug: &str, title: &str) -> MatchResult {
        match_in_days(q(slug, title), &days(), DOC, &refresh(), Some(&strip()))
    }

    #[test]
    fn yesterdays_slug_with_a_different_title_is_new() {
        let r = check("季瑩氣象2100", "今晚雨勢趨緩 明起回暖");
        assert_eq!(r.status, MatchStatus::ToCut);
        assert_eq!(r.matched_line, None);
    }

    #[test]
    fn yesterdays_story_with_the_same_title_is_a_rerun_already_uploaded() {
        // Only punctuation and the 最新》 prefix differ.
        let r = check("北投強風垮1800", "北投強風 路樹倒塌壓車");
        assert_eq!(r.status, MatchStatus::Removed);
        assert!(r.cross_day);
        assert_eq!(r.matched_line.as_deref(), Some("UP 北投強風垮1800 SOT 18:10:00 社會"));
    }

    #[test]
    fn a_story_flagged_for_regrab_yesterday_is_written_again_today() {
        // (抓新) 前一天沒上，隔天自然要重抓：跟新稿一樣（v0.3.5）。
        let r = check("合成重貼新聞1800", "合成重貼標題");
        assert_eq!(r.status, MatchStatus::ToCut);
        assert_eq!(r.matched_line, None);
        assert!(!r.cross_day);
    }

    #[test]
    fn yesterdays_unmarked_line_with_the_same_title_was_never_handled_so_it_is_written() {
        // 前一天 slug 前面沒標記＝還沒處理（v0.3.5）。
        let r = check("季瑩氣象2100", "明天北部轉涼，早晚溫差大");
        assert_eq!(r.status, MatchStatus::ToCut);
        assert_eq!(r.matched_line, None);
        assert!(!r.cross_day);
    }

    #[test]
    fn yesterdays_hour_regrab_is_written_again_today() {
        let doc = "=====1028=====\n(19)合成抓節次1800 SOT 18:20:00\n抓節次標題\n=====1029=====\n";
        let d = day_sections(doc, "1029", "1028");
        let r = match_in_days(q("合成抓節次1800", "抓節次標題"), &d, doc, &refresh(), Some(&strip()));
        assert_eq!(r.status, MatchStatus::ToCut);
        assert!(!r.cross_day);
    }

    #[test]
    fn slot_dividers_typed_with_equals_on_both_sides_do_not_cut_the_day() {
        // 2026/10/29 實測：1028 段落在 ====0900==== 被切斷，只讀到 132 行，跨天 0 則。
        let doc = "\
＝＝＝＝＝＝＝＝＝＝＝＝1028＝＝＝＝＝＝＝＝＝＝＝＝＝
======0700
早上稿0700 SOT 07:05:00
早上標題

====0900====
九點稿0900 SOT 09:05:00
九點標題

====1300====
========22 2300
00 蘇替英圓夢2100 SOT 21:12:56 政治
蔡英文喊幫自己圓新北市長夢想 蘇：一定完成

＝＝＝＝＝＝＝＝＝＝＝＝1029＝＝＝＝＝＝＝＝＝＝＝＝＝
====0900====
今天稿0900 SOT 09:01:00
";
        let d = day_sections(doc, "1029", "1028");
        let previous = d.previous.as_deref().unwrap();
        assert!(previous.contains("====0900===="));
        assert!(previous.contains("00 蘇替英圓夢2100"), "{previous}");
        assert!(!previous.contains("今天稿0900"));
        assert!(d.today.as_deref().unwrap().contains("今天稿0900"));
        assert_eq!(d.today_header.as_deref(), Some("＝＝＝＝＝＝＝＝＝＝＝＝1029＝＝＝＝＝＝＝＝＝＝＝＝＝"));
        let r = match_in_days(q("蘇替英圓夢2100", "蔡英文喊幫自己圓新北市長夢想 蘇：一定完成"), &d, doc, &refresh(), Some(&strip()));
        assert_eq!(r.status, MatchStatus::Removed);
        assert!(r.cross_day);
    }

    #[test]
    fn the_1028_reruns_reported_on_1029() {
        // 2026/10/29 實測：1028 晚上的重播在 1029 早上被當新稿寫進去。
        let doc = "\
＝＝＝＝＝＝＝＝＝＝＝＝1028＝＝＝＝＝＝＝＝＝＝＝＝＝
========1800
球棒砸美容1800 SOT 18:20:00 社會
球棒砸美容院 店員嚇壞

========22 2300
00 蘇替英圓夢2100 SOT 21:12:56 政治
蔡英文喊幫自己圓新北市長夢想 蘇：一定完成
合成內文。

========0000
＝＝＝＝＝＝＝＝＝＝＝＝1029＝＝＝＝＝＝＝＝＝＝＝＝＝
";
        let d = day_sections(doc, "1029", "1028");
        assert!(d.previous.is_some());
        let rerun = match_in_days(q("蘇替英圓夢2100", "蔡英文喊幫自己圓新北市長夢想 蘇：一定完成"), &d, doc, &refresh(), Some(&strip()));
        assert_eq!(rerun.status, MatchStatus::Removed);
        assert!(rerun.cross_day);
        let unhandled = match_in_days(q("球棒砸美容1800", "球棒砸美容院 店員嚇壞"), &d, doc, &refresh(), Some(&strip()));
        assert_eq!(unhandled.status, MatchStatus::ToCut);
        assert_eq!(
            previous_day_lines("蘇替英圓夢2100", &d, &strip()),
            [("00 蘇替英圓夢2100 SOT 21:12:56 政治".to_string(), normalize_title("蔡英文喊幫自己圓新北市長夢想蘇一定完成", &strip()))]
        );
    }

    #[test]
    fn without_a_title_on_either_side_only_today_counts() {
        assert_eq!(check("北投強風垮1800", "").status, MatchStatus::ToCut);
        // Yesterday's line is the last one of its day: no title under it.
        assert_eq!(check("北投路樹1800", "北投路樹").status, MatchStatus::ToCut);
    }

    #[test]
    fn todays_section_decides_when_the_slug_is_there() {
        let r = check("淑麗氣象0900", "完全不同的標題");
        assert_eq!(r.status, MatchStatus::InDoc);
        assert!(!r.cross_day);
    }

    #[test]
    fn a_hand_added_line_without_a_prefix_reads_as_already_in_the_doc() {
        // The user typed it in by hand after it never got written automatically.
        let r = check("蔣萬安直進", "最新》蔣萬安直進標題");
        assert_eq!(r.status, MatchStatus::InDoc);
        assert_eq!(r.matched_line.as_deref(), Some("蔣萬安直進 直進 16:00:00"));
    }

    #[test]
    fn turning_cross_day_matching_off_ignores_yesterday() {
        let r = match_in_days(q("北投強風垮1800", "北投強風路樹倒塌壓車"), &days(), DOC, &refresh(), None);
        assert_eq!(r.status, MatchStatus::ToCut);
    }

    #[test]
    fn a_missing_today_section_means_nothing_from_today_matches() {
        let d = day_sections(DOC, "1027", "1026");
        assert_eq!(d.today, None);
        assert!(d.has_dividers);
        assert_eq!(d.today_header.as_deref(), Some("==========1027=========="));
        // 淑麗氣象0900 is in 1026 (now the previous day) with this title, unmarked:
        // never handled, so it is written (v0.3.5).
        let r = match_in_days(q("淑麗氣象0900", "今天各地回溫"), &d, DOC, &refresh(), Some(&strip()));
        assert!(!r.cross_day);
        assert_eq!(r.status, MatchStatus::ToCut);
        let r = match_in_days(q("北投強風垮1800", "北投強風路樹倒塌壓車"), &day_sections(DOC, "1026", "1025"), DOC, &refresh(), Some(&strip()));
        assert!(r.cross_day);
    }

    #[test]
    fn the_header_copies_the_last_dividers_shape() {
        let doc = "＝＝＝＝ 1025 ＝＝＝＝\n內容\n=====1026=====\n";
        assert_eq!(day_sections(doc, "1027", "1026").today_header.as_deref(), Some("=====1027====="));
        let wide = "＝＝＝＝ １０２６ ＝＝＝＝\n";
        assert_eq!(day_sections(wide, "1027", "1026").today_header.as_deref(), Some("＝＝＝＝ 1027 ＝＝＝＝"));
    }

    #[test]
    fn a_doc_without_any_date_divider_falls_back_to_the_whole_doc() {
        let doc = "UP 北投強風垮1800 SOT\n";
        let d = day_sections(doc, "1026", "1025");
        assert!(!d.has_dividers);
        assert_eq!(d.today_header, None);
        let r = match_in_days(q("北投強風垮1800", ""), &d, doc, &refresh(), Some(&strip()));
        assert_eq!(r.status, MatchStatus::Removed);
    }

    #[test]
    fn title_normalization_drops_prefixes_markers_spaces_and_punctuation() {
        assert_eq!(
            normalize_title("最新》 北投強風！路樹倒塌「壓車」(勿上網)", &strip()),
            normalize_title("北投強風 路樹倒塌壓車", &strip())
        );
        assert_eq!(normalize_title("ＡＢＣ．測試，", &strip()), "abc測試");
    }
}

// --- 時間校正（v0.3.9） -------------------------------------------------------------
//
// 文稿編輯會校正 rundown 的時間，大家又以文稿時間為準，所以協作文件裡已經寫進去的
// 稿要跟著 rundown 改時間。只改今天段落裡、時間落在那一節範圍內的行：範圍外的同
// slug 行是別節的播出紀錄（17:04 的 1700 重播），不是這一節的。

/// rundown 裡一列已播出的稿，和它所屬節次的範圍（`inews::Window`，秒，節次時間軸）。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TimeRow {
    pub show: String,
    pub slug: String,
    /// rundown 的時間 `HH:MM:SS`（工具寫進文件的那個累積時間）。
    pub time: String,
    /// 節次整點（當天第幾秒），把時間拉到同一條時間軸用。
    pub nominal: u32,
    pub start: i64,
    pub end: i64,
}

/// 要改的一行。`line` 是整份文件的第幾行（0 起算），`offset` 是時間在那一行裡的
/// UTF-16 位置（Quill 用的單位）。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TimeCorrection {
    pub show: String,
    pub slug: String,
    pub line: usize,
    pub line_text: String,
    pub offset: usize,
    pub old_time: String,
    pub new_time: String,
    /// 這一行是同一節同一個 slug 的第幾個（依時間先後），連續幾輪是否一樣用。
    pub ordinal: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TimePlan {
    pub corrections: Vec<TimeCorrection>,
    /// `[校正略過]` 的內容（`slug：原因`）。
    pub skips: Vec<String>,
}

/// 一行裡時間字串的位置（字元 index）和秒數。
struct TimeSpot {
    start: usize,
    len: usize,
    secs: i64,
}

/// 全形英數符號轉半形（一個字元對一個字元，位置不變）。
fn fold_char(c: char) -> char {
    let code = c as u32;
    if (0xFF01..=0xFF5E).contains(&code) {
        char::from_u32(code - 0xFEE0).unwrap_or(c)
    } else if code == 0x3000 {
        ' '
    } else {
        c
    }
}

/// `from` 之後第一個時間：`18:05:52`、`18：05：52`（折成半形後一樣）、`180552`。
/// 前後不能緊接數字（`1800` 的 slug 尾巴、`2026110` 這種不算）。
fn find_time(chars: &[char], from: usize) -> Option<TimeSpot> {
    let digit = |i: usize| chars.get(i).is_some_and(|c| c.is_ascii_digit());
    let num = |a: usize, b: usize| -> i64 {
        chars[a..b].iter().fold(0, |n, c| n * 10 + c.to_digit(10).unwrap_or(0) as i64)
    };
    let valid = |h: i64, m: i64, s: i64| h < 30 && m < 60 && s < 60;
    let mut i = from;
    while i < chars.len() {
        if !digit(i) || (i > 0 && digit(i - 1)) {
            i += 1;
            continue;
        }
        let mut run = i;
        while digit(run) {
            run += 1;
        }
        let n = run - i;
        // HH:MM:SS（時可以是 1 位數）
        if (n == 1 || n == 2)
            && chars.get(run) == Some(&':')
            && digit(run + 1) && digit(run + 2) && !digit(run + 3)
            && chars.get(run + 3) == Some(&':')
            && digit(run + 4) && digit(run + 5) && !digit(run + 6)
        {
            let (h, m, s) = (num(i, run), num(run + 1, run + 3), num(run + 4, run + 6));
            if valid(h, m, s) {
                return Some(TimeSpot { start: i, len: run + 6 - i, secs: h * 3600 + m * 60 + s });
            }
        }
        if n == 6 {
            let (h, m, s) = (num(i, i + 2), num(i + 2, i + 4), num(i + 4, i + 6));
            if valid(h, m, s) {
                return Some(TimeSpot { start: i, len: 6, secs: h * 3600 + m * 60 + s });
            }
        }
        i = run;
    }
    None
}

/// slug 在這一行（已折半形）的結束位置。slug 後面緊接英數字或 `-` 就是另一個 slug
/// （`甲1800-1`），不算。
fn slug_end(folded: &[char], needle: &[char]) -> Option<usize> {
    if needle.is_empty() || folded.len() < needle.len() {
        return None;
    }
    (0..=folded.len() - needle.len()).find_map(|i| {
        let end = i + needle.len();
        let boundary = folded.get(end).map_or(true, |c| !(c.is_alphanumeric() || *c == '-'));
        (folded[i..end] == *needle && boundary).then_some(end)
    })
}

/// `HH:MM:SS` → 秒。
fn clock_secs(s: &str) -> Option<i64> {
    let p: Vec<i64> = s.trim().split(':').map(|x| x.parse().ok()).collect::<Option<_>>()?;
    match p[..] {
        [h, m, s] if m < 60 && s < 60 => Some(h * 3600 + m * 60 + s),
        _ => None,
    }
}

/// 照舊時間的寫法寫新時間：分隔符號、全形半形、時是不是 1 位數都照舊。
fn restyle_time(old: &[char], new_secs: i64) -> String {
    let s = new_secs.rem_euclid(86_400);
    let digits = format!("{:02}{:02}{:02}", s / 3600, (s / 60) % 60, s % 60);
    let old_digits = old.iter().filter(|c| fold_char(**c).is_ascii_digit()).count();
    let mut new: Vec<char> = digits.chars().collect();
    if old_digits == 5 && new[0] == '0' {
        new.remove(0);
    }
    let wide = old.iter().any(|c| ('０'..='９').contains(c));
    let put = |c: char| if wide { char::from_u32(c as u32 + 0xFEE0).unwrap_or(c) } else { c };
    let mut out = String::new();
    let mut it = new.into_iter();
    if old_digits == 5 && digits.len() - old_digits == 1 && !digits.starts_with('0') {
        // 9:05:00 → 10:01:00：多出來的那位數放最前面。
        out.push(put(it.next().unwrap_or('0')));
    }
    for c in old {
        if fold_char(*c).is_ascii_digit() {
            out.push(put(it.next().unwrap_or('0')));
        } else {
            out.push(*c);
        }
    }
    out
}

/// 一行在今天段落裡、含某個 slug 的候選。
struct Candidate<'a> {
    line: usize,
    text: &'a str,
    spot: TimeSpot,
}

/// 依今天段落和 rundown 已播出的列，算出要改時間的行。
///
/// - 只看 `today` 日期分隔線底下的行；文件沒有今天的分隔線就什麼都不改。
/// - 同一節同一個 slug：範圍內的行（不含抓新行）和 rundown 的列各自依時間排序後一對一
///   配對，數量不同就不改。時間一樣的不列出來。
/// - 範圍外的行是別節的播出紀錄，不改也不參與配對。
pub fn plan_time_corrections(doc_text: &str, today: &str, rows: &[TimeRow], refresh_keywords: &[String]) -> TimePlan {
    let mut plan = TimePlan::default();
    let mut inside = false;
    let mut found = false;
    let mut lines: Vec<(usize, &str)> = Vec::new();
    for (n, line) in doc_text.lines().enumerate() {
        if let Some(d) = date_divider(line) {
            inside = d == today;
            found |= inside;
            continue;
        }
        if inside {
            lines.push((n, line));
        }
    }
    if !found {
        return plan;
    }

    // (節次, slug) 依 rundown 順序分組。
    let mut groups: Vec<(&str, &str, Vec<&TimeRow>)> = Vec::new();
    for r in rows {
        match groups.iter_mut().find(|(show, slug, _)| *show == r.show && *slug == r.slug) {
            Some(g) => g.2.push(r),
            None => groups.push((&r.show, &r.slug, vec![r])),
        }
    }

    for (show, slug, group) in groups {
        let needle: Vec<char> = normalize_for_match(slug).chars().collect();
        if needle.is_empty() {
            continue;
        }
        let first = group[0];
        let axis = |t: i64| crate::inews::unwrap_near(t, first.nominal as i64);
        let in_range = |t: i64| (first.start..first.end).contains(&axis(t));

        let mut candidates = Vec::new();
        let mut refresh_lines = 0;
        let mut out_of_range = Vec::new();
        let mut untimed = false;
        for &(n, text) in &lines {
            let folded: Vec<char> = text.chars().map(fold_char).collect();
            let Some(end) = slug_end(&folded, &needle) else { continue };
            let start = end - needle.len();
            let prefix: String = folded[..start].iter().collect();
            if is_refresh_prefix(&normalize_for_match(&prefix), refresh_keywords) {
                refresh_lines += 1;
                continue;
            }
            match find_time(&folded, end) {
                Some(spot) if in_range(spot.secs) => candidates.push(Candidate { line: n, text, spot }),
                Some(spot) => out_of_range.push(crate::inews::format_clock(spot.secs)),
                None => untimed = true,
            }
        }
        if refresh_lines > 0 && candidates.is_empty() {
            plan.skips.push(format!("{slug}：{show} 只有抓新行，不改（抓新要用它的時間判斷）"));
        }
        if !out_of_range.is_empty() && candidates.is_empty() {
            plan.skips.push(format!("{slug}：文件的 {} 在 {show} 節次範圍外，視為其他節次的播出紀錄", out_of_range.join("、")));
        }
        if untimed {
            plan.skips.push(format!("{slug}：今天段落有一行找不到時間，這一輪不改"));
            continue;
        }
        if candidates.is_empty() {
            continue;
        }
        if candidates.len() != group.len() {
            plan.skips.push(format!(
                "{slug}：{show} rundown 有 {} 列、文件範圍內有 {} 行，無法配對",
                group.len(),
                candidates.len()
            ));
            continue;
        }
        let mut times = Vec::with_capacity(group.len());
        for r in &group {
            match clock_secs(&r.time) {
                Some(t) => times.push(axis(t)),
                None => {
                    plan.skips.push(format!("{slug}：rundown 時間「{}」無法解析", r.time));
                    times.clear();
                    break;
                }
            }
        }
        if times.len() != group.len() {
            continue;
        }
        times.sort();
        candidates.sort_by_key(|c| axis(c.spot.secs));

        for (ordinal, (c, new_secs)) in candidates.iter().zip(times).enumerate() {
            if axis(c.spot.secs) == new_secs {
                continue;
            }
            let chars: Vec<char> = c.text.chars().collect();
            let old: &[char] = &chars[c.spot.start..c.spot.start + c.spot.len];
            plan.corrections.push(TimeCorrection {
                show: show.to_string(),
                slug: slug.to_string(),
                line: c.line,
                line_text: c.text.to_string(),
                offset: chars[..c.spot.start].iter().map(|ch| ch.len_utf16()).sum(),
                old_time: old.iter().collect(),
                new_time: restyle_time(old, new_secs),
                ordinal,
            });
        }
    }
    plan
}

/// 連續幾輪的紀錄：`節次|slug|第幾個|舊時間` → (上一輪提出的新時間, 連續幾輪)。
pub type TimeStreaks = std::collections::HashMap<String, (String, u32)>;

pub fn streak_key(c: &TimeCorrection) -> String {
    format!("{}|{}|{}|{}", c.show, c.slug, c.ordinal, c.old_time)
}

/// 同一行、同一個新時間連續 `need` 輪都提出才改（排播調整到一半不要來回改）。回傳
/// (這一輪可以改的, 還在等的數量)。這一輪沒提出的行從紀錄裡拿掉，下次重新算。
pub fn stable_corrections(streaks: &mut TimeStreaks, corrections: &[TimeCorrection], need: u32) -> (Vec<TimeCorrection>, usize) {
    let mut next = TimeStreaks::new();
    let mut ready = Vec::new();
    let mut waiting = 0;
    for c in corrections {
        let key = streak_key(c);
        let rounds = match streaks.get(&key) {
            Some((t, n)) if *t == c.new_time => n + 1,
            _ => 1,
        };
        if rounds >= need.max(1) {
            ready.push(c.clone());
        } else {
            waiting += 1;
        }
        next.insert(key, (c.new_time.clone(), rounds));
    }
    *streaks = next;
    (ready, waiting)
}

#[cfg(test)]
mod time_tests {
    use super::*;

    fn refresh() -> Vec<String> {
        crate::config::MarkersConfig::default().refresh_keywords
    }

    /// 1800 節次：範圍 17:43:30 ～ 19:55:00。
    fn row(slug: &str, time: &str) -> TimeRow {
        TimeRow {
            show: "1800".into(),
            slug: slug.into(),
            time: time.into(),
            nominal: 18 * 3600,
            start: 17 * 3600 + 43 * 60 + 30,
            end: 19 * 3600 + 55 * 60,
        }
    }

    fn doc(body: &str) -> String {
        format!("========1031========\n合成昨天1800 SOT 18:05:52\n========1101========\n{body}")
    }

    fn fix(body: &str, rows: &[TimeRow]) -> TimePlan {
        plan_time_corrections(&doc(body), "1101", rows, &refresh())
    }

    fn changes(p: &TimePlan) -> Vec<(String, String)> {
        p.corrections.iter().map(|c| (c.old_time.clone(), c.new_time.clone())).collect()
    }

    #[test]
    fn a_line_in_the_same_show_takes_the_rundown_time() {
        let p = fix("合成掃街1800 SOT 18:05:52 社會\n標題", &[row("合成掃街1800", "18:01:51")]);
        assert_eq!(changes(&p), vec![("18:05:52".into(), "18:01:51".into())]);
        let c = &p.corrections[0];
        assert_eq!(c.line, 3);
        assert_eq!(c.line_text, "合成掃街1800 SOT 18:05:52 社會");
        assert_eq!(c.offset, "合成掃街1800 SOT ".encode_utf16().count());
    }

    #[test]
    fn the_same_time_needs_no_correction() {
        let p = fix("合成掃街1800 SOT 18:01:51", &[row("合成掃街1800", "18:01:51")]);
        assert!(p.corrections.is_empty());
        assert!(p.skips.is_empty());
    }

    #[test]
    fn a_prefixed_line_is_corrected_and_keeps_its_prefix() {
        let p = fix("UP 合成撞立院1800 SOT 18:07:58", &[row("合成撞立院1800", "18:03:57")]);
        assert_eq!(changes(&p), vec![("18:07:58".into(), "18:03:57".into())]);
        assert_eq!(p.corrections[0].offset, "UP 合成撞立院1800 SOT ".encode_utf16().count());
        let p = fix("(X)合成撞立院1800 SOT 18:07:58", &[row("合成撞立院1800", "18:03:57")]);
        assert_eq!(p.corrections.len(), 1);
    }

    #[test]
    fn tab_separated_fullwidth_colons_keep_their_style() {
        let p = fix("合成占床1800\tSOT\t18：11：58", &[row("合成占床1800", "18:17:06")]);
        assert_eq!(changes(&p), vec![("18：11：58".into(), "18：17：06".into())]);
        assert_eq!(p.corrections[0].offset, "合成占床1800\tSOT\t".encode_utf16().count());
    }

    #[test]
    fn a_compact_time_stays_compact() {
        let p = fix("合成占床1800 SOT 181158", &[row("合成占床1800", "18:17:06")]);
        assert_eq!(changes(&p), vec![("181158".into(), "181706".into())]);
    }

    #[test]
    fn a_line_outside_the_show_range_is_another_airing() {
        // 1800 範圍從 17:43:30 開始：17:04:21 是 1700 那節的播出紀錄。
        let p = fix("合成砍倒地1800 SOT 17:04:21", &[row("合成砍倒地1800", "18:05:27")]);
        assert!(p.corrections.is_empty());
        assert_eq!(p.skips.len(), 1);
        assert!(p.skips[0].contains("範圍外"), "{:?}", p.skips);
    }

    #[test]
    fn the_range_is_not_limited_to_the_hour() {
        let p = fix("合成晚段1800 SOT 18:50:00", &[row("合成晚段1800", "19:10:00")]);
        assert_eq!(changes(&p), vec![("18:50:00".into(), "19:10:00".into())]);
    }

    #[test]
    fn a_refresh_line_is_never_corrected() {
        for prefix in ["(抓新)", "(19)", "(X/19)"] {
            let p = fix(&format!("{prefix}合成重貼1800 SOT 18:05:52"), &[row("合成重貼1800", "18:01:51")]);
            assert!(p.corrections.is_empty(), "{prefix}");
            assert!(p.skips[0].contains("抓新"), "{:?}", p.skips);
        }
    }

    #[test]
    fn the_previous_day_is_never_corrected() {
        // doc() 前一天段落裡有 合成昨天1800 18:05:52，今天沒有。
        let p = fix("合成別則1800 SOT 18:00:00", &[row("合成昨天1800", "18:01:51")]);
        assert!(p.corrections.is_empty());
        assert!(p.skips.is_empty());
    }

    #[test]
    fn no_today_divider_means_no_corrections() {
        let p = plan_time_corrections("========1031========\n合成掃街1800 SOT 18:05:52", "1101", &[row("合成掃街1800", "18:01:51")], &refresh());
        assert!(p.corrections.is_empty());
    }

    #[test]
    fn two_rows_and_two_lines_pair_in_time_order() {
        let body = "合成雙1800 SOT 18:30:00\n標題\n合成雙1800 SOT 18:10:00\n標題";
        let p = fix(body, &[row("合成雙1800", "18:32:00"), row("合成雙1800", "18:12:00")]);
        let mut got = changes(&p);
        got.sort();
        assert_eq!(got, vec![("18:10:00".into(), "18:12:00".into()), ("18:30:00".into(), "18:32:00".into())]);
        let early = p.corrections.iter().find(|c| c.old_time == "18:10:00").unwrap();
        assert_eq!((early.line, early.ordinal), (5, 0));
    }

    #[test]
    fn two_rows_and_one_line_are_left_alone() {
        let p = fix("合成雙1800 SOT 18:10:00", &[row("合成雙1800", "18:12:00"), row("合成雙1800", "18:32:00")]);
        assert!(p.corrections.is_empty());
        assert!(p.skips[0].contains("無法配對"), "{:?}", p.skips);
    }

    #[test]
    fn a_longer_slug_sharing_the_prefix_is_a_different_story() {
        let p = fix("合成甲1800-1 SOT 18:05:52", &[row("合成甲1800", "18:01:51")]);
        assert!(p.corrections.is_empty());
    }

    #[test]
    fn a_line_without_a_time_stops_that_slug_for_this_round() {
        let p = fix("合成甲1800 SOT\n合成甲1800 SOT 18:05:52", &[row("合成甲1800", "18:01:51")]);
        assert!(p.corrections.is_empty());
        assert!(p.skips[0].contains("找不到時間"), "{:?}", p.skips);
    }

    #[test]
    fn the_midnight_show_compares_across_zero() {
        // 2400：範圍 23:55:00 ～ 隔天 01:00:00，時間軸以 24:00 為準。
        let r = TimeRow {
            show: "2400".into(),
            slug: "合成午夜2400".into(),
            time: "00:03:00".into(),
            nominal: 24 * 3600,
            start: 23 * 3600 + 55 * 60,
            end: 25 * 3600,
        };
        let p = fix("合成午夜2400 SOT 23:58:00", &[r.clone()]);
        assert_eq!(changes(&p), vec![("23:58:00".into(), "00:03:00".into())]);
        let p = fix("合成午夜2400 SOT 22:58:00", &[r]);
        assert!(p.corrections.is_empty(), "22:58 是 2200 的播出紀錄");
    }

    #[test]
    fn a_single_digit_hour_is_kept_as_typed() {
        let mut r = row("合成早1800", "09:01:00");
        (r.nominal, r.start, r.end) = (9 * 3600, 8 * 3600 + 55 * 60, 10 * 3600);
        let p = fix("合成早1800 SOT 9:05:00", &[r.clone()]);
        assert_eq!(changes(&p), vec![("9:05:00".into(), "9:01:00".into())]);
        r.time = "10:01:00".into();
        r.end = 11 * 3600;
        let p = fix("合成早1800 SOT 9:05:00", &[r]);
        assert_eq!(changes(&p), vec![("9:05:00".into(), "10:01:00".into())]);
    }

    #[test]
    fn a_new_time_must_hold_for_two_rounds() {
        let mut streaks = TimeStreaks::new();
        let body = "合成掃街1800 SOT 18:05:52";
        let round = |streaks: &mut TimeStreaks, time: &str| {
            let p = fix(body, &[row("合成掃街1800", time)]);
            stable_corrections(streaks, &p.corrections, 2)
        };
        assert_eq!(round(&mut streaks, "18:01:51").0.len(), 0, "first sighting waits");
        // 下一輪又變回文件的時間：沒有要改的，紀錄清掉。
        assert_eq!(round(&mut streaks, "18:05:52"), (vec![], 0));
        assert_eq!(round(&mut streaks, "18:01:51").1, 1, "starts counting again");
        let (ready, waiting) = round(&mut streaks, "18:01:51");
        assert_eq!((ready.len(), waiting), (1, 0));
        assert_eq!(ready[0].new_time, "18:01:51");
        // 換成另一個新時間就重來。
        assert_eq!(round(&mut streaks, "18:02:00").1, 1);
    }

    #[test]
    fn a_vanished_row_changes_nothing() {
        let p = fix("合成掃街1800 SOT 18:05:52", &[]);
        assert_eq!(p, TimePlan::default());
    }

    #[test]
    fn fullwidth_digits_stay_fullwidth() {
        let p = fix("合成全形1800 SOT １８：０５：５２", &[row("合成全形1800", "18:01:51")]);
        assert_eq!(changes(&p), vec![("１８：０５：５２".into(), "１８：０１：５１".into())]);
    }
}
