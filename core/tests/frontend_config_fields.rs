//! 前端用到的設定欄位，後端的預設設定都要有（AUDIT_REPORT #8）。
//!
//! app.js 以前在 19 處寫 `?? 10`、`?? 120` 這種預設值，跟 `config.rs` 各寫一份；現在前端直接用
//! `get_config` 回傳的值（`Config` 整份序列化，每個欄位都有 serde 預設）。這個測試把 app.js 裡
//! 讀設定的地方全部找出來，確認每個欄位在 `Config::default()` 序列化後都存在、不是 null ——
//! 新增設定欄位卻忘了在 Rust 加預設值，或前端打錯欄位名稱，這裡就會失敗。
//!
//! 找的寫法：`config.<區塊>.<欄位>`、`inewsCfg.<欄位>`（= config.inews）、以及設定頁的
//! `"<區塊>.<欄位>"` 路徑字串（`num(…, "inews.sweep_delay_minutes", …)`、`data-cfg="…"`）。

use news_script_core::config::Config;
use regex::Regex;
use std::collections::BTreeSet;

fn app_js() -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../app/src/app.js");
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("讀不到 {}：{e}", path.display()))
}

fn used_fields(js: &str, sections: &BTreeSet<String>) -> BTreeSet<(String, String)> {
    let mut out = BTreeSet::new();
    let dotted = Regex::new(r"\bconfig\??\.([a-z_]+)\??\.([a-z_]+)").unwrap();
    for c in dotted.captures_iter(js) {
        out.insert((c[1].to_string(), c[2].to_string()));
    }
    let inews = Regex::new(r"\binewsCfg\.([a-z_]+)").unwrap();
    for c in inews.captures_iter(js) {
        out.insert(("inews".to_string(), c[1].to_string()));
    }
    let quoted = Regex::new(r#"["'`]([a-z_]+)\.([a-z_]+)["'`]"#).unwrap();
    for c in quoted.captures_iter(js) {
        if sections.contains(&c[1]) {
            out.insert((c[1].to_string(), c[2].to_string()));
        }
    }
    out
}

#[test]
fn every_setting_the_frontend_reads_has_a_backend_default() {
    let defaults = serde_json::to_value(Config::default()).unwrap();
    let sections: BTreeSet<String> = defaults.as_object().unwrap().keys().cloned().collect();
    let used = used_fields(&app_js(), &sections);
    assert!(used.len() > 40, "app.js 裡找到的設定欄位太少（{}），比對的寫法可能變了", used.len());
    // 一定要涵蓋以前寫死預設值的那幾個（數字、字串、清單、勾選）。
    for f in [
        "inews.sweep_delay_minutes",
        "inews.anomaly_threshold_seconds",
        "inews.write_stable_rounds",
        "inews.unknown_review_volume",
        "inews.broadcast_day_start",
        "inews.write_start",
        "inews.prelist_queue",
        "inews.program_divider_format",
        "inews.programs",
        "inews.unknown_review_sound_kind",
        "inews.off_air_on_empty",
        "inews.correct_time",
        "import.default_folder",
        "filter.force_pass_slug_prefixes",
        "filter.no_body_pass_terms",
        "filter.blank_style_timecode_pass",
        "evening.window_start",
        "evening.blocked_styles",
        "evening.anchors",
        "annotations.exclusive_prefix",
        "output.keyword_csv_line",
    ] {
        let (s, k) = f.split_once('.').unwrap();
        assert!(used.contains(&(s.to_string(), k.to_string())), "沒有找到 {f}，比對的寫法可能變了");
    }
    let missing: Vec<String> = used
        .iter()
        .filter(|(s, f)| defaults.get(s).and_then(|v| v.get(f)).is_none_or(|v| v.is_null()))
        .map(|(s, f)| format!("{s}.{f}"))
        .collect();
    assert!(
        missing.is_empty(),
        "app.js 用到、但 Config::default() 沒有（或是 null）的設定：{missing:?}\n\
         請在 core/src/config.rs 加 serde 預設值，不要在前端寫 `?? 數字`。"
    );
}
