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

/// Classify one slug against the work-doc text (one entry per line, as pasted or
/// scraped verbatim). See spec §6 for the "prefix before the slug" rule.
///
/// Takes the **last** matching line, not the first. The same slug legitimately
/// appears more than once across a shift: a producer's original `(抓新) slug` request
/// line, followed later by the plain `slug` line this tool itself writes back. Taking
/// the first line would keep reading the original `(抓新)` request forever and write
/// the same story back on every re-run, even after it was already handled. The last
/// line is whatever is currently true about the story -- if someone adds a fresh
/// `(抓新) slug` line below the plain one later, that also correctly asks for a
/// re-paste again.
pub fn match_slug(slug: &str, doc_text: &str, refresh_keywords: &[String]) -> MatchResult {
    let needle = normalize_for_match(slug);
    if needle.is_empty() {
        return MatchResult { status: MatchStatus::ToCut, matched_line: None };
    }

    let mut result = None;
    for line in doc_text.lines() {
        let hay = normalize_for_match(line);
        if let Some(idx) = hay.find(&needle) {
            let prefix = hay[..idx].trim();
            let status = if prefix.is_empty() {
                MatchStatus::ToCut
            } else if refresh_keywords.iter().any(|k| prefix.contains(normalize_for_match(k).as_str())) {
                MatchStatus::KeepRefresh
            } else {
                MatchStatus::Removed
            };
            result = Some(MatchResult { status, matched_line: Some(line.trim().to_string()) });
        }
    }

    result.unwrap_or(MatchResult { status: MatchStatus::ToCut, matched_line: None })
}

/// Classify every entry's slug against the work-doc text in one pass.
pub fn match_all<'a>(
    slugs: impl IntoIterator<Item = &'a str>,
    doc_text: &str,
    refresh_keywords: &[String],
) -> Vec<(&'a str, MatchResult)> {
    slugs.into_iter().map(|slug| (slug, match_slug(slug, doc_text, refresh_keywords))).collect()
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
    fn date_and_time_divider_lines_never_falsely_match_a_real_slug() {
        let r = match_slug("0807", DOC_TEXT, &refresh());
        // "0807" is a substring of the date divider "========0807========";
        // the divider itself has no real slug semantics but the substring search
        // will still find it -- prefix is "========" (non-empty, no refresh
        // keyword), so it resolves to Removed rather than crashing or panicking.
        assert_eq!(r.status, MatchStatus::Removed);
    }
}
