use news_script_core::config::Config;
use news_script_core::gemini::{self, GeminiConfig};
use news_script_core::model::{NewsEntry, Outcome};
use news_script_core::{format, import_files, parse};

#[tokio::main]
async fn main() {
    let source = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "G:\\EBC-Document-Conversion\\EXflie".to_string());
    let out_dir = std::env::args()
        .nth(2)
        .unwrap_or_else(|| "G:\\EBC-Document-Conversion\\output".to_string());
    std::fs::create_dir_all(&out_dir).unwrap();

    let mut files = Vec::new();
    for entry in std::fs::read_dir(&source).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("txt") {
            let name = path.file_name().unwrap().to_string_lossy().to_string();
            let bytes = std::fs::read(&path).unwrap();
            let text = parse::decode_and_normalize(&bytes);
            files.push((name, text));
        }
    }
    files.sort_by(|a, b| a.0.cmp(&b.0));

    let cfg = Config::default();
    let summary = import_files(&files, &cfg);

    // Passed + TEL (needs-manual) entries, in the spec's four-line format -- this is
    // what "複製全部 / 存成 txt" in the Tauri app would produce by default.
    let mut output_entries: Vec<NewsEntry> = summary.passed.clone();
    for o in &summary.needs_manual {
        if let Outcome::NeedsManualContent(e) = o {
            output_entries.push(e.clone());
        }
    }

    let mut keyword_failures = Vec::new();
    if let Some(api_key) = gemini::api_key_from_env() {
        let gemini_cfg = GeminiConfig { api_key, model: cfg.gemini.model.clone() };
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .unwrap();

        for entry in output_entries.iter_mut() {
            if entry.body.trim().is_empty() {
                continue; // TEL entries with no content yet -- nothing to base keywords on
            }
            match gemini::generate_keywords(
                &client,
                &gemini_cfg,
                &entry.title,
                &entry.body,
                cfg.output.keyword_count,
            )
            .await
            {
                Ok(words) => entry.keywords = words,
                Err(e) => keyword_failures.push(format!("{}　-- {}", entry.slug, e)),
            }
        }
    } else {
        println!("⚠ 未偵測到 GEMINI_API_KEY，關鍵字欄位留空。");
    }

    let output_text = format::format_batch(&output_entries, &cfg.output);
    let output_path = format!("{}/轉換結果.txt", out_dir);
    std::fs::write(&output_path, &output_text).unwrap();

    // A review report so nothing is silently lost: what was filtered, flagged as
    // unknown style, or failed to parse, and why.
    let mut report = String::new();
    report.push_str(&format!(
        "來源資料夾：{}\n總檔案數：{}　載入：{}　通過：{}　未知樣式：{}　待補稿(TEL)：{}　已濾除：{}　解析失敗：{}\n\n",
        source,
        files.len(),
        summary.loaded,
        summary.passed.len(),
        summary.unknown.len(),
        summary.needs_manual.len(),
        summary.filtered.len(),
        summary.failed.len(),
    ));

    report.push_str("=== 未知樣式（需人工判斷是否保留）===\n");
    for o in &summary.unknown {
        if let Outcome::UnknownStyle(e) = o {
            report.push_str(&format!("  {}　[{}]　{}\n", e.slug, e.style, e.file_name));
        }
    }

    report.push_str("\n=== 待補稿 TEL（保留但無內容）===\n");
    for o in &summary.needs_manual {
        if let Outcome::NeedsManualContent(e) = o {
            report.push_str(&format!("  {}　[{}]　{}\n", e.slug, e.style, e.file_name));
        }
    }

    report.push_str("\n=== 已濾除（樣式在黑名單）===\n");
    for o in &summary.filtered {
        if let Outcome::FilteredByStyle { slug, style, file_name } = o {
            report.push_str(&format!("  {}　[{}]　{}\n", slug, style, file_name));
        }
    }

    report.push_str("\n=== 解析失敗（需回頭檢查原始檔）===\n");
    for o in &summary.failed {
        if let Outcome::ParseFailed { file_name, reason } = o {
            report.push_str(&format!("  {}　-- {}\n", file_name, reason));
        }
    }

    if !keyword_failures.is_empty() {
        report.push_str("\n=== 關鍵字產生失敗（已輸出但該則第 4 行留空，需手動補）===\n");
        for f in &keyword_failures {
            report.push_str(&format!("  {}\n", f));
        }
    }

    let report_path = format!("{}/檢查報告.txt", out_dir);
    std::fs::write(&report_path, &report).unwrap();

    println!("已輸出：{}", output_path);
    println!("已輸出：{}", report_path);
    println!("{}", report);
}
