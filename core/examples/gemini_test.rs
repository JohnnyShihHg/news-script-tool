#[tokio::main]
async fn main() {
    match news_script_core::gemini::api_key_from_env() {
        Some(_) => println!("GEMINI_API_KEY is set, testing a real call..."),
        None => { println!("GEMINI_API_KEY not set in this process's environment."); return; }
    }
    let cfg = news_script_core::gemini::GeminiConfig {
        api_key: news_script_core::gemini::api_key_from_env().unwrap(),
        model: "gemini-3.5-flash-lite".to_string(),
    };
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .unwrap();
    let result = news_script_core::gemini::generate_keywords(
        &client,
        &cfg,
        "看環景錄影才知！ 車主控新車被瞞「烤漆、換把手」",
        "有車主新車買完快一個月後，意外透過環景錄影存檔發現，新車左後車門，有被重新烤過漆，就連把手也被換過，整個過程還是在交車前全被錄下，質疑車商業務。",
        4,
    ).await;
    match result {
        Ok(words) => println!("OK: {:?}", words),
        Err(e) => println!("ERR: {}", e),
    }
}
