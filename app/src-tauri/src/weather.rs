//! Import the nationwide outlook paragraph used to fill the 08:22 weather rundown.
//!
//! The visible CWA page builds `#f-1` from W01_Content in this public data script;
//! fetching the page's initial HTML does not contain the article.

use chrono::{FixedOffset, Utc};
use news_script_core::model::{NewsEntry, Outcome};
use regex::Regex;
use std::time::Duration;

const CWA_W01: &str = "https://www.cwa.gov.tw/Data/js/fcst/W01_Data.js";

/// Only the first two agreed name rules. A lone presenter name is not enough.
fn weather_rank(slug: &str) -> Option<u8> {
    let compact: String = slug.chars().filter(|c| !c.is_whitespace()).collect();
    if compact.contains("淑麗") && compact.contains("氣象") && compact.contains("0822") {
        Some(1)
    } else if compact.contains("氣象") && compact.contains("0822") {
        Some(2)
    } else {
        None
    }
}

/// Extract string elements from a JavaScript array (or `new Array(...)`) without
/// executing the remote script. W01 is made of strings; unknown syntax fails closed.
fn js_strings(source: &str, name: &str) -> Result<Vec<String>, String> {
    let assignment = Regex::new(&format!(
        r"\b{}\s*=\s*(?:new\s+Array\s*)?",
        regex::escape(name)
    ))
    .map_err(|e| e.to_string())?;
    let found = assignment
        .find(source)
        .ok_or_else(|| format!("找不到 {name}"))?;
    let mut chars = source[found.end()..].chars().peekable();
    let opening = chars.next().ok_or_else(|| format!("{name} 沒有內容"))?;
    let closing = match opening {
        '[' => ']',
        '(' => ')',
        _ => return Err(format!("{name} 不是可讀的陣列")),
    };
    let mut result = Vec::new();
    loop {
        while matches!(chars.peek(), Some(c) if c.is_whitespace() || *c == ',') {
            chars.next();
        }
        let Some(next) = chars.next() else {
            return Err(format!("{name} 未完整結束"));
        };
        if next == closing {
            break;
        }
        if next != '\'' && next != '"' {
            return Err(format!("{name} 含非文字資料"));
        }
        let quote = next;
        let mut value = String::new();
        loop {
            let c = chars.next().ok_or_else(|| format!("{name} 引號未結束"))?;
            if c == quote {
                break;
            }
            if c == '\\' {
                let escaped = chars
                    .next()
                    .ok_or_else(|| format!("{name} 跳脫字元不完整"))?;
                match escaped {
                    'n' => value.push('\n'),
                    'r' => value.push('\r'),
                    't' => value.push('\t'),
                    'u' => {
                        let hex: String = (0..4)
                            .map(|_| chars.next())
                            .collect::<Option<String>>()
                            .ok_or_else(|| format!("{name} Unicode 跳脫不完整"))?;
                        let code = u32::from_str_radix(&hex, 16)
                            .map_err(|_| format!("{name} Unicode 跳脫錯誤"))?;
                        value.push(
                            char::from_u32(code)
                                .ok_or_else(|| format!("{name} Unicode 字元錯誤"))?,
                        );
                    }
                    other => value.push(other),
                }
            } else {
                value.push(c);
            }
        }
        result.push(value);
    }
    Ok(result)
}

fn plain_text(html: &str) -> String {
    let tags = Regex::new(r"<[^>]*>").unwrap();
    let no_tags = tags.replace_all(html, "");
    no_tags
        .replace("&nbsp;", " ")
        .replace("&#160;", " ")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .trim()
        .to_string()
}

#[derive(Debug, PartialEq)]
struct Outlook {
    date: String, // YYYY-MM-DD, checked against the iNews 修改時間
    body: String,
}

fn parse_outlook(script: &str) -> Result<Outlook, String> {
    let time = js_strings(script, "W01_TIME")?.join(" ");
    let date_re = Regex::new(r"(\d{2,3})年\s*(\d{1,2})月\s*(\d{1,2})日").unwrap();
    let time_text = plain_text(&time);
    let cap = date_re
        .captures(&time_text)
        .ok_or("氣象署資料缺少發布日期")?;
    let year = cap[1].parse::<u32>().map_err(|_| "發布年份錯誤")? + 1911;
    let month = cap[2].parse::<u32>().map_err(|_| "發布月份錯誤")?;
    let day = cap[3].parse::<u32>().map_err(|_| "發布日期錯誤")?;
    let date = format!("{year:04}-{month:02}-{day:02}");
    let contents = js_strings(script, "W01_Content")?;
    let candidates: Vec<String> = contents
        .into_iter()
        .map(|s| plain_text(&s))
        .filter(|s| s.starts_with("今、明") || s.starts_with("今，明") || s.starts_with("今,明"))
        .collect();
    if candidates.len() != 1 {
        return Err(format!(
            "氣象署今、明段落找到 {} 則，無法安全選取",
            candidates.len()
        ));
    }
    Ok(Outlook {
        date,
        body: candidates.into_iter().next().unwrap(),
    })
}

fn modified_date(entry: &NewsEntry) -> Option<String> {
    let text = entry.header.get("修改時間")?;
    let cap = Regex::new(r"^(\d{4})/(\d{1,2})/(\d{1,2})\b")
        .unwrap()
        .captures(text)?;
    let year = cap[1].parse::<u32>().ok()?;
    let month = cap[2].parse::<u32>().ok()?;
    let day = cap[3].parse::<u32>().ok()?;
    Some(format!("{year:04}-{month:02}-{day:02}"))
}

fn apply_outlook(entry: &mut NewsEntry, response: &Result<Outlook, String>, today: &str) {
    match response {
        Ok(outlook) if outlook.date != today => entry.warnings.push(format!(
            "未補入氣象署內文：網站發布日期 {} 不是台灣今天 {today}",
            outlook.date
        )),
        Ok(outlook) => match modified_date(entry) {
            Some(date) if date == outlook.date => {
                entry.body = outlook.body.clone();
                entry
                    .warnings
                    .retain(|w| w != "找到標題但無稿頭內文，需人工補稿");
                entry.warnings.push(format!(
                    "已用中央氣象署 {date} 的「今、明」天氣概況補入內文，請人工核對"
                ));
            }
            Some(date) => entry.warnings.push(format!(
                "未補入氣象署內文：稿件修改日期 {date} 與網站發布日期 {} 不同",
                outlook.date
            )),
            None => entry
                .warnings
                .push("未補入氣象署內文：稿件沒有可確認的修改日期".to_string()),
        },
        Err(reason) => entry.warnings.push(format!("未補入氣象署內文：{reason}")),
    }
}

fn select_target(manual: &[Outcome]) -> Result<Option<usize>, Vec<usize>> {
    let ranked: Vec<(usize, u8)> = manual
        .iter()
        .enumerate()
        .filter_map(|(i, o)| {
            if let Outcome::NeedsManualContent(e) = o {
                weather_rank(&e.slug).map(|rank| (i, rank))
            } else {
                None
            }
        })
        .collect();
    let Some(best_rank) = ranked.iter().map(|(_, rank)| *rank).min() else {
        return Ok(None);
    };
    let winners: Vec<usize> = ranked
        .into_iter()
        .filter(|(_, rank)| *rank == best_rank)
        .map(|(i, _)| i)
        .collect();
    if winners.len() == 1 {
        Ok(Some(winners[0]))
    } else {
        Err(winners)
    }
}

/// Preserve the original outcome and body if data is unavailable or dated differently.
pub async fn fill_manual_weather(manual: &mut [Outcome]) {
    // A 1st-ranked item wins over a substitute's 2nd-ranked item if both happen
    // to be in one export. An equally ranked tie is ambiguous: never fill both.
    let target_index = match select_target(manual) {
        Ok(Some(i)) => i,
        Ok(None) => return,
        Err(ties) => {
            for i in ties {
                if let Outcome::NeedsManualContent(entry) = &mut manual[i] {
                    entry
                        .warnings
                        .push("未補入氣象署內文：同順位氣象稿超過一則，請人工選擇".to_string());
                }
            }
            return;
        }
    };

    let response = async {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(8))
            .build()
            .map_err(|e| e.to_string())?;
        let fresh_url = format!("{CWA_W01}?t={}", Utc::now().timestamp());
        let script = client
            .get(fresh_url)
            .header(
                reqwest::header::CACHE_CONTROL,
                "no-cache, no-store, max-age=0",
            )
            .send()
            .await
            .map_err(|e| e.to_string())?
            .error_for_status()
            .map_err(|e| e.to_string())?
            .text()
            .await
            .map_err(|e| e.to_string())?;
        parse_outlook(&script)
    }
    .await;

    if let Outcome::NeedsManualContent(entry) = &mut manual[target_index] {
        let taipei = FixedOffset::east_opt(8 * 3600).unwrap();
        let today = Utc::now()
            .with_timezone(&taipei)
            .format("%Y-%m-%d")
            .to_string();
        apply_outlook(entry, &response, &today);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_two_agreed_name_rules_match() {
        assert_eq!(weather_rank("淑麗報氣象0822"), Some(1));
        assert_eq!(weather_rank("代班報氣象0822"), Some(2));
        assert_eq!(weather_rank("淑麗報其他0822"), None);
        assert_eq!(weather_rank("淑麗報氣象1000"), None);
        assert_eq!(weather_rank("氣象署講雨1100"), None);
    }

    #[test]
    fn extracts_the_one_outlook_from_cwa_javascript() {
        let js = r#"var W01_TIME = ["中央氣象署氣象報告", "115年9月21日5時發布"];
var W01_Content = ["颱風資訊", "今、明(21日、22日)兩天臺灣各地多雲到晴，<br>午後有雨。", "海上強風特報"];
"#;
        assert_eq!(
            parse_outlook(js),
            Ok(Outlook {
                date: "2026-09-21".into(),
                body: "今、明(21日、22日)兩天臺灣各地多雲到晴，午後有雨。".into()
            })
        );
    }

    #[test]
    fn missing_or_ambiguous_outlook_fails_closed() {
        let js = "var W01_TIME=['115年9月21日5時發布']; var W01_Content=['今、明甲','今、明乙'];";
        assert!(parse_outlook(js).unwrap_err().contains("2 則"));
    }

    #[test]
    fn actual_rundown_shape_is_manual_and_gets_the_outlook_body() {
        let text = "新聞名稱(標題): 淑麗報氣象0822\n樣式: LIVE\n修改時間: 2026/9/21 08:10:31\n\
            _______________________________________________________________\n\
            [<\n[主播_淑麗]\n[BAR]\nT2吹東北風! 秋高氣爽水氣少\n>]\n";
        let mut outcome = news_script_core::process_text("weather.txt", text, &Default::default());
        let Outcome::NeedsManualContent(ref mut entry) = outcome else {
            panic!("{outcome:?}")
        };
        let fetched = Ok(Outlook {
            date: "2026-09-21".into(),
            body: "今、明兩天多雲到晴。".into(),
        });
        apply_outlook(entry, &fetched, "2026-09-21");
        assert_eq!(entry.body, "今、明兩天多雲到晴。");
        assert!(entry.warnings.iter().any(|w| w.contains("請人工核對")));
        assert!(!entry.warnings.iter().any(|w| w.contains("無稿頭內文")));
    }

    #[test]
    fn old_rundown_does_not_receive_todays_weather() {
        let text = "新聞名稱(標題): 代班報氣象0822\n樣式: LIVE\n修改時間: 2026/9/20 08:10:31\n\
            _______________________________________________________________\n\
            [<\n[BAR]\nT2合成標題\n>]\n";
        let mut outcome = news_script_core::process_text("weather.txt", text, &Default::default());
        let Outcome::NeedsManualContent(ref mut entry) = outcome else {
            panic!("{outcome:?}")
        };
        apply_outlook(
            entry,
            &Ok(Outlook {
                date: "2026-09-21".into(),
                body: "今、明兩天多雲到晴。".into(),
            }),
            "2026-09-21",
        );
        assert!(entry.body.is_empty());
        assert!(entry
            .warnings
            .iter()
            .any(|w| w.contains("日期") && w.contains("不同")));
    }

    #[test]
    fn yesterday_site_content_is_not_used_even_if_the_file_is_yesterday_too() {
        let text = "新聞名稱(標題): 淑麗報氣象0822\n樣式: LIVE\n修改時間: 2026/9/20 08:10:31\n\
            _______________________________________________________________\n\
            [<\n[BAR]\nT2合成標題\n>]\n";
        let mut outcome = news_script_core::process_text("weather.txt", text, &Default::default());
        let Outcome::NeedsManualContent(ref mut entry) = outcome else {
            panic!("{outcome:?}")
        };
        apply_outlook(
            entry,
            &Ok(Outlook {
                date: "2026-09-20".into(),
                body: "今、明兩天多雲到晴。".into(),
            }),
            "2026-09-21",
        );
        assert!(entry.body.is_empty());
        assert!(entry.warnings.iter().any(|w| w.contains("不是台灣今天")));
    }

    #[test]
    fn first_rank_wins_and_equal_rank_is_ambiguous() {
        let make = |slug: &str| {
            let text = format!(
                "新聞名稱(標題): {slug}\n樣式: LIVE\n\
                _______________________________________________________________\n\
                [<\n[BAR]\nT2合成標題\n>]\n"
            );
            news_script_core::process_text("weather.txt", &text, &Default::default())
        };
        let entries = vec![make("代班報氣象0822"), make("淑麗報氣象0822")];
        assert_eq!(select_target(&entries), Ok(Some(1)));
        assert_eq!(select_target(&[make("代班報氣象0822")]), Ok(Some(0)));
        assert_eq!(
            select_target(&[make("淑麗報氣象0822"), make("淑麗報氣象0822")]),
            Err(vec![0, 1])
        );
    }
}
