/// Status of one iNews slug against the work-doc text, per spec §6.
#[derive(Debug, Clone, PartialEq)]
pub enum MatchStatus {
    /// Not found in the doc at all, or found with nothing but whitespace before it
    /// on that line — today's news to cut.
    ToCut,
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
        return MatchResult { status: MatchStatus::ToCut, matched_line: None };
    }
    let entry_time = padded_time(time);

    let mut result = None;
    for line in doc_text.lines() {
        let hay = normalize_for_match(line);
        if let Some(idx) = hay.find(&needle) {
            let prefix = hay[..idx].trim();
            let status = if prefix.is_empty() {
                MatchStatus::ToCut
            } else if refresh_keywords.iter().any(|k| prefix.contains(normalize_for_match(k).as_str())) {
                let line_time = time_after(&hay[idx + needle.len()..]);
                match (&entry_time, line_time) {
                    (Some(e), Some(l)) if *e == l => MatchStatus::ToCut,
                    _ => MatchStatus::KeepRefresh,
                }
            } else {
                MatchStatus::Removed
            };
            result = Some(MatchResult { status, matched_line: Some(line.trim().to_string()) });
        }
    }

    result.unwrap_or(MatchResult { status: MatchStatus::ToCut, matched_line: None })
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
/// Date dividers carry `=` on both sides; time-slot dividers (`======0700`) only on
/// the left, which is what keeps a slot from being mistaken for a new day. The real
/// doc mixes spaces, tabs and fullwidth `＝`, so the line is normalized first.
fn date_divider(line: &str) -> Option<String> {
    static RE: once_cell::sync::Lazy<regex::Regex> =
        once_cell::sync::Lazy::new(|| regex::Regex::new(r"^=+[=\s]*(\d{4})\s*=[=\s]*$").unwrap());
    let norm = normalize_for_match(line);
    RE.captures(&norm).map(|c| c[1].to_string())
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
    fn slug_with_no_prefix_is_to_cut() {
        let r = match_slug("合成颱風報導1530", DOC_TEXT, &refresh());
        assert_eq!(r.status, MatchStatus::ToCut);
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
        assert_eq!(r.status, MatchStatus::ToCut);
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
        assert_eq!(r.status, MatchStatus::ToCut);
        assert!(r.matched_line.is_some(), "must read as already in the doc, not as new");
    }

    #[test]
    fn a_grab_new_line_for_a_different_time_still_asks_for_a_re_paste() {
        let doc = "(抓新) 合成重貼新聞1200 SOT 12:40:00 生活\n";
        let r = match_entry("合成重貼新聞1200", "13:05:10", doc, &refresh());
        assert_eq!(r.status, MatchStatus::KeepRefresh);
        // Single-digit hours and fullwidth colons still compare.
        let doc = "(抓新) 合成重貼新聞0900 SOT 9：05：00\n";
        assert_eq!(match_entry("合成重貼新聞0900", "09:05:00", doc, &refresh()).status, MatchStatus::ToCut);
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
