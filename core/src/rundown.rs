//! 給剪輯工具用的每節 rundown 資料（`rundown\YYYYMMDD.json`）：每則稿的 slug、樣式、
//! 帶長，以及從製作區字卡讀出來的標題字卡、記者署名、雙框、地點。
//!
//! 這裡全部是不碰網路、不碰檔案的純函式，才能在 `cargo test -p news-script-core` 裡
//! 完整測到。讀寫檔案在 app 端（`app/src-tauri/src/inews.rs`）。

use crate::inews::{civil_from_days, ListEntry, NsmlStory};
use once_cell::sync::Lazy;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// `[BAR]`、`[BAR_大]`、`[BAR_最新大]`、`[bar_最新大]`……
static BAR_CUE: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?i)^\[bar(?:_[^\]]*)?\]").unwrap());
/// `[SUPER右]`、`[SUPER大]`（後面可以再接 `_xxx`）。
static SUPER_CUE: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?i)^\[super[右大][^\]]*\]").unwrap());

/// 署名裡要有這些詞才算記者署名（`推播記者 傅詩涵`、`推播 繆孟潔`、`記者 王小明`）。
const CREDIT_WORDS: [&str; 2] = ["記者", "推播"];

/// 從稿頭內文／製作區讀出來的東西。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScriptInfo {
    /// 每一張標題字卡的 `T2`，照出現順序，重複的只留第一個。
    pub bars: Vec<String>,
    /// `[SUPER右]`／`[SUPER大]` 後面含「記者」或「推播」的 `T1`；沒有就是空字串。
    pub credit: String,
    /// 有 `[雙框` 字卡。
    pub dual: bool,
    /// `[地點` 字卡後面的 `T1`。
    pub location: String,
}

/// `T1黃文華` → `黃文華`。不分大小寫（`t2`），標籤後面的空白不算。
fn tag_text<'a>(line: &'a str, tag: &str) -> Option<&'a str> {
    let head = line.get(..tag.len())?;
    head.eq_ignore_ascii_case(tag).then(|| line[tag.len()..].trim())
}

/// 讀稿的每一行（匯出檔分隔線以下，字卡已展開成 `[< ... >]`）。
///
/// 字卡的範圍是 cue 行（`[BAR_大]`）之後，到空行、下一個 cue 行或 `>]` 為止。
pub fn parse_script<S: AsRef<str>>(lines: &[S]) -> ScriptInfo {
    let lines: Vec<&str> = lines.iter().map(|l| l.as_ref().trim()).collect();
    let mut info = ScriptInfo::default();
    info.dual = lines.iter().any(|l| l.contains("[雙框"));
    for (i, cue) in lines.iter().enumerate() {
        let is_bar = BAR_CUE.is_match(cue);
        let is_super = SUPER_CUE.is_match(cue);
        let is_location = cue.starts_with("[地點");
        if !(is_bar || is_super || is_location) {
            continue;
        }
        let block = lines[i + 1..].iter().take_while(|l| !l.is_empty() && !l.starts_with('[') && **l != ">]");
        if is_bar {
            for t2 in block.clone().filter_map(|l| tag_text(l, "T2")).filter(|t| !t.is_empty()) {
                if !info.bars.iter().any(|b| b == t2) {
                    info.bars.push(t2.to_string());
                }
            }
        }
        if is_super && info.credit.is_empty() {
            if let Some(t1) = block.clone().filter_map(|l| tag_text(l, "T1")).find(|t| CREDIT_WORDS.iter().any(|w| t.contains(w))) {
                info.credit = t1.to_string();
            }
        }
        if is_location && info.location.is_empty() {
            if let Some(t1) = block.filter_map(|l| tag_text(l, "T1")).find(|t| !t.is_empty()) {
                info.location = t1.to_string();
            }
        }
    }
    info
}

/// 檔案裡的一則稿。欄位順序就是 JSON 的順序。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RundownStory {
    pub slug: String,
    /// iNEWS 稿件 ID 跨修改不變的前兩段（`040CE84D:0044D4CD`）；LIST 讀不到就是空字串。
    pub id: String,
    pub style: String,
    /// 帶長（`tape-time`）的秒數，空的當 0。
    pub tape_seconds: u32,
    pub bars: Vec<String>,
    pub credit: String,
    pub dual: bool,
    pub location: String,
    /// 工具看到這則稿的第幾個版本：第一次看到是 1，之後內容每變一次加 1。iNEWS 本身
    /// 沒有版本號，不是 iNEWS 的修改次數。
    pub version: u32,
    /// `modify-date`（iNEWS 的最後修改時間），台北時間 `yyyy-mm-dd HH:MM:SS`。
    pub updated: String,
}

/// Unix 秒 → 台北時間 `yyyy-mm-dd HH:MM:SS`。空的或不是數字回空字串。
pub fn format_stamp(value: &str) -> String {
    let Ok(unix) = value.trim().parse::<i64>() else {
        return String::new();
    };
    let local = unix + 8 * 3600;
    let (y, m, d) = civil_from_days(local.div_euclid(86_400));
    let s = local.rem_euclid(86_400);
    format!("{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02}", s / 3600, (s / 60) % 60, s % 60)
}

/// 一列 rundown → 檔案裡的一則稿。節目結構列（序號以 `=` 開頭的廣告列、分隔列）和
/// 沒有 slug 的列回 None。版本號由 `RundownDay::merge` 填。
pub fn story_record(entry: &ListEntry, story: &NsmlStory) -> Option<RundownStory> {
    let slug = story.title().trim();
    if slug.is_empty() || story.page_number().trim_start().starts_with('=') {
        return None;
    }
    let script = parse_script(&story.body_lines);
    Some(RundownStory {
        slug: slug.to_string(),
        id: entry.story_key(),
        style: story.raw("v-style").trim().to_string(),
        tape_seconds: story.raw("tape-time").trim().parse().unwrap_or(0),
        bars: script.bars,
        credit: script.credit,
        dual: script.dual,
        location: script.location,
        version: 0,
        updated: format_stamp(story.raw("modify-date")),
    })
}

/// 一個播出日的檔案。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RundownDay {
    /// `2026-11-01`
    pub date: String,
    /// 節次資料夾名稱（`1000`）→ 稿件，第一次看到的順序。
    pub shows: BTreeMap<String, Vec<RundownStory>>,
}

impl RundownDay {
    pub fn new(date: &str) -> Self {
        RundownDay { date: date.to_string(), shows: BTreeMap::new() }
    }

    /// 把這一輪讀到的一節併進去。同一節同 slug 只留最新的一份（原位置覆蓋）；這一輪
    /// 沒看到的舊稿留著（播完被抽掉的稿剪輯工具還要用）。內容有變版本加 1，沒變不動，
    /// 所以每一輪重複寫同樣的內容不會讓版本一直增加。
    pub fn merge(&mut self, show: &str, incoming: Vec<RundownStory>) {
        // 同一輪裡 slug 重複的，後面的蓋掉前面的。
        let mut latest: Vec<RundownStory> = Vec::new();
        for s in incoming {
            match latest.iter_mut().find(|o| o.slug == s.slug) {
                Some(o) => *o = s,
                None => latest.push(s),
            }
        }
        let stories = self.shows.entry(show.to_string()).or_default();
        for mut s in latest {
            match stories.iter_mut().find(|o| o.slug == s.slug) {
                Some(old) => {
                    let same = RundownStory { version: old.version, ..s.clone() } == *old;
                    s.version = old.version.max(1) + u32::from(!same);
                    *old = s;
                }
                None => {
                    s.version = 1;
                    stories.push(s);
                }
            }
        }
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_default()
    }

    pub fn from_json(text: &str) -> Result<Self, String> {
        serde_json::from_str(text).map_err(|e| e.to_string())
    }
}

// ---- 當天 rundown 的 slug 搜尋（v0.3.15）----

/// 一筆搜尋結果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SlugHit {
    pub show: String,
    pub slug: String,
    pub style: String,
    /// iNEWS 的最後修改時間（`yyyy-mm-dd HH:MM:SS`）。
    pub updated: String,
}

/// 全形英數轉半形、全形空白轉半形、英文轉小寫，空白合併。
fn fold_for_search(s: &str) -> String {
    let folded: String = s
        .chars()
        .map(|c| match c as u32 {
            code @ 0xFF01..=0xFF5E => char::from_u32(code - 0xFEE0).unwrap_or(c),
            0x3000 => ' ',
            _ => c,
        })
        .flat_map(char::to_lowercase)
        .collect();
    folded.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// 在當天的 rundown 資料裡找 slug：全形半形、大小寫不分，空白分開的每個關鍵字都要出現
/// （AND）。`show_order` 決定節次的排列（播出日 02:00 分界時，0100 排在 2300 後面）；
/// 同一節裡照 rundown 第一次看到的順序。空的查詢回空的結果。
pub fn search_slugs(day: &RundownDay, query: &str, show_order: impl Fn(&str) -> i64) -> Vec<SlugHit> {
    let terms: Vec<String> = fold_for_search(query).split(' ').filter(|t| !t.is_empty()).map(str::to_string).collect();
    if terms.is_empty() {
        return Vec::new();
    }
    let mut shows: Vec<(&String, &Vec<RundownStory>)> = day.shows.iter().collect();
    shows.sort_by_key(|(name, _)| (show_order(name), (*name).clone()));
    let terms = &terms;
    shows
        .into_iter()
        .flat_map(|(show, stories)| {
            stories.iter().filter_map(move |s| {
                let hay = fold_for_search(&s.slug);
                terms.iter().all(|t| hay.contains(t.as_str())).then(|| SlugHit {
                    show: show.clone(),
                    slug: s.slug.clone(),
                    style: s.style.clone(),
                    updated: s.updated.clone(),
                })
            })
        })
        .collect()
}

#[cfg(test)]
mod search_tests {
    use super::*;

    fn story(slug: &str, style: &str) -> RundownStory {
        RundownStory { slug: slug.into(), style: style.into(), updated: "2026-11-11 09:00:00".into(), ..Default::default() }
    }

    fn day() -> RundownDay {
        let mut d = RundownDay::new("2026-11-11");
        d.merge("0100", vec![story("深夜合成稿0100", "SOT")]);
        d.merge("0900", vec![story("合成氣象0900", ""), story("ＳＯＴ全形合成0900", "SOT"), story("(寰)合成國際0900", "SOT")]);
        d.merge("1200", vec![story("合成氣象1200", "LIVE")]);
        d
    }

    /// 02:00 分界：0100 是播出日的最後。
    fn order(show: &str) -> i64 {
        let n: i64 = show.parse().unwrap_or(0);
        (n - 200).rem_euclid(2400)
    }

    #[test]
    fn finds_by_substring_in_broadcast_day_order() {
        let hits = search_slugs(&day(), "氣象", order);
        let got: Vec<(&str, &str)> = hits.iter().map(|h| (h.show.as_str(), h.slug.as_str())).collect();
        assert_eq!(got, [("0900", "合成氣象0900"), ("1200", "合成氣象1200")]);
        assert_eq!(hits[1].style, "LIVE");
    }

    #[test]
    fn all_terms_must_match_and_width_and_case_do_not_matter() {
        assert_eq!(search_slugs(&day(), "氣象 1200", order).len(), 1);
        assert_eq!(search_slugs(&day(), "sot 全形", order)[0].slug, "ＳＯＴ全形合成0900");
        assert_eq!(search_slugs(&day(), "（寰）", order)[0].slug, "(寰)合成國際0900");
    }

    #[test]
    fn the_late_night_show_comes_last() {
        let hits = search_slugs(&day(), "合成", order);
        assert_eq!(hits.last().unwrap().show, "0100");
    }

    #[test]
    fn an_empty_query_finds_nothing() {
        assert!(search_slugs(&day(), "   ", order).is_empty());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inews::{parse_list_line, parse_nsml};

    /// 使用者給的真實稿件字卡。
    const SAMPLE: &str = "[雙框_記者右]\nT1黃文華\nT2繆孟潔\n\n[SUPER右]\nT1推播 繆孟潔\n\n[BAR_最新大]\nT2眼科醫轉戰奇美\"月薪被開5萬\" 拒去轟:污辱!\n\n[BAR_最新]\nT2奇美眼科醫\"滿一年變0底薪.抽成制\" 引熱議";

    fn lines(text: &str) -> Vec<String> {
        text.lines().map(String::from).collect()
    }

    #[test]
    fn the_sample_cards_give_bars_credit_and_dual() {
        let info = parse_script(&lines(SAMPLE));
        assert_eq!(
            info.bars,
            ["眼科醫轉戰奇美\"月薪被開5萬\" 拒去轟:污辱!", "奇美眼科醫\"滿一年變0底薪.抽成制\" 引熱議"]
        );
        // 推播跟記者一樣算署名。
        assert_eq!(info.credit, "推播 繆孟潔");
        assert!(info.dual);
        assert_eq!(info.location, "");
    }

    #[test]
    fn cues_and_tags_ignore_case_and_only_bar_cards_give_bars() {
        let text = "[bar_最新大]\nt2小寫標題\n\n[BAR]\nT2小寫標題\n\n[BAR_獨大]\nT2 獨家標題 \n\n[其他]\nT2不是標題\n\n[SUPER右]\nT1推播記者 傅詩涵";
        let info = parse_script(&lines(text));
        assert_eq!(info.bars, ["小寫標題", "獨家標題"], "deduped, in order, T2 under other cues ignored");
        assert_eq!(info.credit, "推播記者 傅詩涵");
        assert!(!info.dual);
    }

    #[test]
    fn a_super_card_without_a_reporter_is_not_a_credit() {
        let text = "[SUPER右]\nT1現場畫面\n\n[SUPER大]\nT1記者 王小明\n\n[SUPER左]\nT1推播 不是右大";
        assert_eq!(parse_script(&lines(text)).credit, "記者 王小明");
        assert_eq!(parse_script(&lines("[SUPER右]\nT1現場畫面")).credit, "");
        assert_eq!(parse_script(&lines("[SUPER左]\nT1記者 王小明")).credit, "");
    }

    #[test]
    fn the_location_is_the_trimmed_t1_after_the_cue() {
        let info = parse_script(&lines("[地點]\nT1  台北市信義區 \nT2不要\n\n[BAR]\nT2標題"));
        assert_eq!(info.location, "台北市信義區");
        assert_eq!(info.bars, ["標題"]);
        assert_eq!(parse_script(&lines("[地點_大]\nT1宜蘭")).location, "宜蘭");
    }

    #[test]
    fn a_card_ends_at_the_blank_line_the_next_cue_or_the_block_close() {
        // [BAR] 後面沒有 T2、直接接下一張：不能把下一張的 T2 算進來。
        let info = parse_script(&lines("[<\n[BAR]\n[SUPER右]\nT2不是標題\n>]\nT2區塊外"));
        assert!(info.bars.is_empty(), "{:?}", info.bars);
    }

    #[test]
    fn a_script_without_cards_is_empty() {
        assert_eq!(parse_script::<String>(&[]), ScriptInfo::default());
        assert_eq!(parse_script(&lines("今天天氣很好。")), ScriptInfo::default());
    }

    #[test]
    fn timestamps_are_taipei_local_time() {
        assert_eq!(format_stamp("1775436877"), "2026-04-06 08:54:37");
        assert_eq!(format_stamp("1767196800"), "2026-01-01 00:00:00");
        assert_eq!(format_stamp(""), "");
        assert_eq!(format_stamp("abc"), "");
    }

    fn nsml(page: &str, title: &str, style: &str, tape: &str, modified: &str, script: &str) -> NsmlStory {
        let aps: String = script.lines().map(|l| format!("<ap>{l}</ap>")).collect();
        parse_nsml(&format!(
            "<nsml><fields><string id=\"page-number\">{page}</string><string id=\"title\">{title}</string>\
             <string id=\"v-style\">{style}</string><duration id=\"tape-time\">{tape}</duration>\
             <date id=\"modify-date\">{modified}</date></fields>\
             <body><p><a idref=\"0\"/></p><p>內文。</p></body><aeset><ae id=\"0\">{aps}</ae></aeset></nsml>"
        ))
    }

    fn entry() -> ListEntry {
        parse_list_line("-f-------- 1 1     0 Oct 25 06:19  040CE84D:0044D4CD:6ADD2EE7 訊息框").unwrap()
    }

    #[test]
    fn a_rundown_row_becomes_a_story_record() {
        let story = nsml("A5", "推播繆孟潔眼科", "LIVE", "45", "1793499135", SAMPLE);
        let r = story_record(&entry(), &story).unwrap();
        assert_eq!((r.slug.as_str(), r.id.as_str(), r.style.as_str()), ("推播繆孟潔眼科", "040CE84D:0044D4CD", "LIVE"));
        assert_eq!(r.tape_seconds, 45);
        assert_eq!(r.bars.len(), 2);
        assert_eq!(r.credit, "推播 繆孟潔");
        assert!(r.dual);
        assert_eq!(r.updated, "2026-11-01 10:12:15");
        // 帶長空白或不是數字：0。
        assert_eq!(story_record(&entry(), &nsml("A5", "x", "", "", "", "")).unwrap().tape_seconds, 0);
        assert_eq!(story_record(&entry(), &nsml("A5", "x", "", "1:05", "", "")).unwrap().tape_seconds, 0);
    }

    #[test]
    fn commercials_dividers_and_rows_without_a_slug_are_not_stories() {
        assert!(story_record(&entry(), &nsml("=========", "=CM 3==", "", "", "", "")).is_none());
        assert!(story_record(&entry(), &nsml(" =CM", "廣告", "", "", "", "")).is_none());
        assert!(story_record(&entry(), &nsml("A1", "  ", "SOT", "", "", "")).is_none());
        // 沒有序號、沒有樣式的列（大片頭）照留。
        assert!(story_record(&entry(), &nsml("", "大片頭", "", "", "", "")).is_some());
    }

    fn rec(slug: &str, updated: &str) -> RundownStory {
        RundownStory { slug: slug.into(), updated: updated.into(), ..Default::default() }
    }

    #[test]
    fn merging_overwrites_by_slug_keeps_unseen_stories_and_counts_changes() {
        let mut day = RundownDay::new("2026-11-01");
        day.merge("1000", vec![rec("甲", "a"), rec("乙", "a"), rec("甲", "b")]);
        let versions = |d: &RundownDay| d.shows["1000"].iter().map(|s| (s.slug.clone(), s.version, s.updated.clone())).collect::<Vec<_>>();
        // 同一輪重複的 slug 只留後面那份，也不算「變過」。
        assert_eq!(versions(&day), [("甲".into(), 1, "b".into()), ("乙".into(), 1, "a".into())]);
        // 一模一樣的下一輪：版本不動。
        day.merge("1000", vec![rec("甲", "b"), rec("乙", "a")]);
        assert_eq!(versions(&day)[0].1, 1);
        // 乙改了、甲這輪被抽掉、丙是新的。
        day.merge("1000", vec![rec("乙", "c"), rec("丙", "c")]);
        assert_eq!(
            versions(&day),
            [("甲".into(), 1, "b".into()), ("乙".into(), 2, "c".into()), ("丙".into(), 1, "c".into())]
        );
        // 別的節次同 slug 是另一則。
        day.merge("1100", vec![rec("甲", "z")]);
        assert_eq!(day.shows["1100"][0].version, 1);
    }

    #[test]
    fn the_json_has_the_agreed_shape_and_round_trips() {
        let mut day = RundownDay::new("2026-11-01");
        let story = nsml("A5", "推播繆孟潔眼科", "LIVE", "0", "1793499135", SAMPLE);
        day.merge("1000", vec![story_record(&entry(), &story).unwrap()]);
        let json = day.to_json();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["date"], "2026-11-01");
        let s = &v["shows"]["1000"][0];
        assert_eq!(s["slug"], "推播繆孟潔眼科");
        assert_eq!(s["id"], "040CE84D:0044D4CD");
        assert_eq!(s["style"], "LIVE");
        assert_eq!(s["tape_seconds"], 0);
        assert_eq!(s["bars"][1], "奇美眼科醫\"滿一年變0底薪.抽成制\" 引熱議");
        assert_eq!(s["credit"], "推播 繆孟潔");
        assert_eq!(s["dual"], true);
        assert_eq!(s["location"], "");
        assert_eq!(s["version"], 1);
        assert_eq!(s["updated"], "2026-11-01 10:12:15");
        // 欄位順序照約定。
        let keys: Vec<&str> = s.as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(RundownDay::from_json(&json).unwrap(), day);
        assert!(json.find("\"slug\"").unwrap() < json.find("\"id\"").unwrap());
        assert!(json.find("\"location\"").unwrap() < json.find("\"version\"").unwrap());
        assert_eq!(keys.len(), 10);
    }
}
