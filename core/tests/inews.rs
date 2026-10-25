//! NSML → 「匯出延伸文稿」txt 的端對端測試。
//!
//! `tests/fixtures/inews/` 裡的 NSML 和預期 txt 都是**合成的**：欄位結構照真實的
//! iNEWS FTP 輸出，內容全部虛構（真實稿件不進 repo，見 README）。預期 txt 是照
//! 匯出規則手寫的，不是用轉換器產生的，所以這裡比對的是規則本身。

use news_script_core::config::Config;
use news_script_core::inews::{nsml_to_text, parse_nsml, story_to_text};
use news_script_core::model::Outcome;
use news_script_core::{import_files, process_text};

fn fixture(name: &str) -> Vec<u8> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/inews").join(name);
    std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn nsml(name: &str) -> String {
    String::from_utf8(fixture(&format!("{name}.nsml"))).unwrap()
}

#[test]
fn a_full_story_converts_byte_for_byte() {
    let text = nsml_to_text(&nsml("合成颱風報導0900"));
    let expected = fixture("合成颱風報導0900.expected.txt");
    assert_eq!(text, String::from_utf8(expected.clone()).unwrap());
    assert_eq!(text.as_bytes(), expected.as_slice());
}

#[test]
fn a_story_with_empty_fields_keeps_every_label_and_its_space() {
    let text = nsml_to_text(&nsml("合成空欄位框"));
    assert_eq!(text.as_bytes(), fixture("合成空欄位框.expected.txt").as_slice());
}

#[test]
fn the_output_is_crlf_utf8_without_bom_and_no_extra_trailing_newline() {
    let text = nsml_to_text(&nsml("合成颱風報導0900"));
    assert!(!text.starts_with('\u{feff}'));
    assert!(!text.replace("\r\n", "").contains('\n'), "bare LF in output");
    assert!(!text.replace("\r\n", "").contains('\r'), "bare CR in output");
    // 最後一個 <p></p> 自然產生結尾的一個 CRLF，不再多補。
    assert!(text.ends_with("增強。\r\n第二段內文，接在同一段。\r\n"));
    assert!(!text.ends_with("\r\n\r\n"));
}

#[test]
fn the_converted_story_goes_through_the_existing_pipeline() {
    let text = nsml_to_text(&nsml("合成颱風報導0900"));
    let cfg = Config::default();
    let entry = match process_text("合成颱風報導0900.txt", &text, &cfg) {
        Outcome::Passed(e) => e,
        other => panic!("expected Passed, got {other:?}"),
    };
    assert_eq!(entry.slug, "合成颱風報導0900");
    assert_eq!(entry.style, "SOT");
    assert_eq!(entry.time, "07:28:52");
    assert_eq!(entry.group, "生");
    assert_eq!(entry.slug_marker, "(勿上網)");
    assert_eq!(entry.raw_title, "合成颱風標題 風雨增強");
    assert!(entry.body.starts_with("這是合成測試用的颱風稿內文第一段"), "body was {:?}", entry.body);
    assert!(entry.body.contains("第二段內文"), "body was {:?}", entry.body);
    assert_eq!(entry.header.get("撰稿時間"), Some("2026/4/6 08:54:37"));
    assert_eq!(entry.header.get("總長度"), Some("2:21"));
}

#[test]
fn import_files_takes_converted_stories_like_exported_ones() {
    let files = vec![
        ("合成颱風報導0900.txt".to_string(), nsml_to_text(&nsml("合成颱風報導0900"))),
        ("合成空欄位框.txt".to_string(), nsml_to_text(&nsml("合成空欄位框"))),
    ];
    let summary = import_files(&files, &Config::default());
    assert_eq!(summary.loaded, 2);
    assert_eq!(summary.passed.len(), 1);
    // 空欄位框沒有樣式也沒有製作區：照現有規則進解析失敗，不會消失。
    assert_eq!(summary.failed.len(), 1);
}

#[test]
fn a_computed_cume_only_fills_an_empty_cume_time() {
    let story = parse_nsml(&nsml("合成空欄位框"));
    let filled = story_to_text(&story, Some("09:55:05"));
    assert!(filled.contains("\r\n累積時間: 09:55:05\r\n"));

    // NSML 自己有硬時間就用它，不被覆蓋。
    let story = parse_nsml(&nsml("合成颱風報導0900"));
    assert!(story_to_text(&story, Some("09:55:05")).contains("\r\n累積時間: 07:28:52\r\n"));
    assert_eq!(story.hard_time(), Some(26932));
    assert_eq!(story.total_seconds(), 141);
    assert_eq!(story.page_number(), "A3");
}
