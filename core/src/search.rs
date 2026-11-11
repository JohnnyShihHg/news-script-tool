//! 搜尋用的本地紀錄（v0.3.15）。
//!
//! 剪輯工具讀的 `rundown\YYYYMMDD.json` 格式不動：播出時間會跟著編輯調整排播一直變，
//! 剪輯工具要自己判斷，不能拿這裡的時間。所以搜尋另外存一份，放在
//! `search\YYYYMMDD\`：
//!
//! - `index.json`：節次 → 稿件（slug、iNEWS 稿件 ID、版本、樣式、播出時間、修改時間），
//!   每一輪自動刷新整份重寫，很小。
//! - `stories\<ID>_<版本>.nsml`：每個版本的稿件原文，第一次看到這個版本才寫一次。
//!   點搜尋結果載入成卡片時直接讀這裡，不用再連 iNEWS。

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// 索引裡的一則稿。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SearchEntry {
    pub slug: String,
    /// iNEWS 稿件 ID 跨修改不變的前兩段（`040CE84D:0044D4CD`）。
    pub id: String,
    /// iNEWS 稿件 ID 的第三段（修改時間戳）：稿件一改就變。原文檔名用它。
    pub version: String,
    pub style: String,
    /// 播出時間：照 rundown 算出來的累積時間 `HH:MM:SS`；算不出來是空字串。
    pub air_time: String,
    /// iNEWS 的最後修改時間 `yyyy-mm-dd HH:MM:SS`。
    pub updated: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SearchDay {
    pub date: String,
    /// 節次資料夾名稱 → 稿件，第一次看到的順序。
    pub shows: BTreeMap<String, Vec<SearchEntry>>,
}

impl SearchDay {
    /// 把這一輪讀到的一節併進去。同一則稿（先認 ID，沒有 ID 認 slug）用這一輪的內容
    /// 覆蓋；這一輪算不出播出時間就沿用上次的。這一輪沒看到的舊稿留著（播完被抽掉的
    /// 也還搜得到）。
    pub fn merge(&mut self, show: &str, incoming: Vec<SearchEntry>) {
        let stories = self.shows.entry(show.to_string()).or_default();
        for mut s in incoming {
            let same = |o: &SearchEntry| if s.id.is_empty() { o.slug == s.slug } else { o.id == s.id };
            match stories.iter_mut().find(|o| same(o)) {
                Some(old) => {
                    if s.air_time.is_empty() {
                        s.air_time = old.air_time.clone();
                    }
                    *old = s;
                }
                None => stories.push(s),
            }
        }
    }

    /// 剪輯工具的 rundown 檔裡有、搜尋紀錄沒有的稿補進來（v0.3.16）。搜尋紀錄是更新到
    /// 0.3.16 之後才開始存的，同一天更早的節次只在 rundown 檔裡；這樣「今天全部」都搜得到。
    /// 補進來的沒有播出時間，也沒有版本（本地沒有原文，載入時會改連 iNEWS）。只在記憶體裡
    /// 合併，兩個檔案都不改。
    pub fn fill_from_rundown(&mut self, rundown: &crate::rundown::RundownDay) {
        for (show, stories) in &rundown.shows {
            let list = self.shows.entry(show.clone()).or_default();
            for s in stories {
                let known = list.iter().any(|o| if s.id.is_empty() { o.slug == s.slug } else { o.id == s.id });
                if !known {
                    list.push(SearchEntry {
                        slug: s.slug.clone(),
                        id: s.id.clone(),
                        version: String::new(),
                        style: s.style.clone(),
                        air_time: String::new(),
                        updated: s.updated.clone(),
                    });
                }
            }
        }
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    pub fn from_json(text: &str) -> Result<Self, String> {
        serde_json::from_str(text).map_err(|e| e.to_string())
    }
}

/// 稿件原文的檔名：`<ID>_<版本>.nsml`，ID 裡的 `:` 換成 `_`。ID 或版本是空的回 None
/// （認不出是哪個版本，就不存，載入時改連 iNEWS）。只留英數字和 `_`，避免怪字元。
pub fn story_file_name(id: &str, version: &str) -> Option<String> {
    let clean = |s: &str| -> String { s.chars().filter(|c| c.is_ascii_alphanumeric()).collect() };
    let parts: Vec<String> = id.split(':').map(clean).filter(|p| !p.is_empty()).collect();
    let version = clean(version);
    (!parts.is_empty() && !version.is_empty()).then(|| format!("{}_{version}.nsml", parts.join("_")))
}

/// 一筆搜尋結果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SlugHit {
    pub show: String,
    pub slug: String,
    pub id: String,
    pub version: String,
    pub style: String,
    pub air_time: String,
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

/// 在當天的索引裡找 slug：全形半形、大小寫不分，空白分開的每個關鍵字都要出現（AND）。
/// `show_order` 決定節次的排列（播出日 02:00 分界時，0100 排在 2300 後面）；同一節裡照
/// 第一次看到的順序。空的查詢回空的結果。
pub fn search_slugs(day: &SearchDay, query: &str, show_order: impl Fn(&str) -> i64) -> Vec<SlugHit> {
    let terms: Vec<String> = fold_for_search(query).split(' ').filter(|t| !t.is_empty()).map(str::to_string).collect();
    if terms.is_empty() {
        return Vec::new();
    }
    let mut shows: Vec<(&String, &Vec<SearchEntry>)> = day.shows.iter().collect();
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
                    id: s.id.clone(),
                    version: s.version.clone(),
                    style: s.style.clone(),
                    air_time: s.air_time.clone(),
                    updated: s.updated.clone(),
                })
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(slug: &str, id: &str, air: &str) -> SearchEntry {
        SearchEntry {
            slug: slug.into(),
            id: id.into(),
            version: "6AEF0001".into(),
            style: "SOT".into(),
            air_time: air.into(),
            updated: "2026-11-11 09:00:00".into(),
        }
    }

    fn day() -> SearchDay {
        let mut d = SearchDay { date: "2026-11-11".into(), ..Default::default() };
        d.merge("0100", vec![entry("深夜合成稿0100", "A:1", "01:05:00")]);
        d.merge(
            "0900",
            vec![entry("合成氣象0900", "B:1", "09:12:58"), entry("ＳＯＴ全形合成0900", "B:2", ""), entry("(寰)合成國際0900", "B:3", "")],
        );
        d.merge("1200", vec![SearchEntry { style: "LIVE".into(), ..entry("合成氣象1200", "C:1", "12:30:00") }]);
        d
    }

    /// 02:00 分界：0100 是播出日的最後。
    fn order(show: &str) -> i64 {
        let n: i64 = show.parse().unwrap_or(0);
        (n - 200).rem_euclid(2400)
    }

    #[test]
    fn finds_by_substring_in_broadcast_day_order_with_air_time() {
        let hits = search_slugs(&day(), "氣象", order);
        let got: Vec<(&str, &str, &str)> = hits.iter().map(|h| (h.show.as_str(), h.slug.as_str(), h.air_time.as_str())).collect();
        assert_eq!(got, [("0900", "合成氣象0900", "09:12:58"), ("1200", "合成氣象1200", "12:30:00")]);
        assert_eq!(hits[1].style, "LIVE");
        assert_eq!(hits[0].id, "B:1");
    }

    #[test]
    fn all_terms_must_match_and_width_and_case_do_not_matter() {
        assert_eq!(search_slugs(&day(), "氣象 1200", order).len(), 1);
        assert_eq!(search_slugs(&day(), "sot 全形", order)[0].slug, "ＳＯＴ全形合成0900");
        assert_eq!(search_slugs(&day(), "（寰）", order)[0].slug, "(寰)合成國際0900");
    }

    #[test]
    fn the_late_night_show_comes_last_and_an_empty_query_finds_nothing() {
        assert_eq!(search_slugs(&day(), "合成", order).last().unwrap().show, "0100");
        assert!(search_slugs(&day(), "   ", order).is_empty());
    }

    #[test]
    fn a_story_is_updated_in_place_and_keeps_its_air_time_when_this_round_has_none() {
        let mut d = day();
        d.merge("0900", vec![SearchEntry { version: "6AEF0002".into(), ..entry("合成氣象0900", "B:1", "") }]);
        let s = &d.shows["0900"][0];
        assert_eq!((s.version.as_str(), s.air_time.as_str()), ("6AEF0002", "09:12:58"));
        assert_eq!(d.shows["0900"].len(), 3, "同一則稿不重複");
        // slug 改了但 ID 一樣：還是同一則。
        d.merge("0900", vec![entry("合成氣象改名0900", "B:1", "09:13:30")]);
        assert_eq!((d.shows["0900"][0].slug.as_str(), d.shows["0900"].len()), ("合成氣象改名0900", 3));
        let back = SearchDay::from_json(&d.to_json()).unwrap();
        assert_eq!(back, d);
    }

    #[test]
    fn todays_earlier_shows_come_from_the_rundown_file() {
        use crate::rundown::{RundownDay, RundownStory};
        let mut rundown = RundownDay::new("2026-11-11");
        let rec = |slug: &str, id: &str| RundownStory { slug: slug.into(), id: id.into(), style: "SOT".into(), ..Default::default() };
        // 0600 只在 rundown 檔；0900 兩邊都有同一則（ID 一樣）。
        rundown.merge("0600", vec![rec("早安合成0600", "Z:1")]);
        rundown.merge("0900", vec![rec("合成氣象0900", "B:1"), rec("只在舊檔0900", "B:9")]);
        let mut d = day();
        d.fill_from_rundown(&rundown);
        let hit = &search_slugs(&d, "早安", order)[0];
        assert_eq!((hit.show.as_str(), hit.air_time.as_str(), hit.version.as_str()), ("0600", "", ""));
        // 兩邊都有的不重複，而且留著搜尋紀錄那份（有播出時間）。
        let weather: Vec<_> = search_slugs(&d, "合成氣象0900", order);
        assert_eq!(weather.len(), 1);
        assert_eq!(weather[0].air_time, "09:12:58");
        assert_eq!(search_slugs(&d, "只在舊檔", order).len(), 1);
    }

    #[test]
    fn story_file_names_are_safe() {
        assert_eq!(story_file_name("040CE84D:0044D4CD", "6ADD2EE7").as_deref(), Some("040CE84D_0044D4CD_6ADD2EE7.nsml"));
        assert_eq!(story_file_name("../x:y", "1/2").as_deref(), Some("x_y_12.nsml"));
        assert_eq!(story_file_name("", "1"), None);
        assert_eq!(story_file_name("A:B", ""), None);
    }
}
