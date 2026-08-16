use serde::Deserialize;
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct GeminiConfig {
    pub api_key: String,
    pub model: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum GeminiError {
    /// Network-level failure (timeout, DNS, connection refused, ...).
    Transport(String),
    /// The API responded with a non-2xx status after retries were exhausted.
    Api { status: u16, body: String },
    /// The response didn't parse as the expected JSON keyword array.
    Parse(String),
}

impl GeminiError {
    /// True when the request failed because the per-minute quota was exhausted, so
    /// callers can tell "wait and retry" apart from errors that retrying won't fix.
    pub fn is_rate_limited(&self) -> bool {
        matches!(self, GeminiError::Api { status: 429, .. })
    }
}

impl std::fmt::Display for GeminiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GeminiError::Transport(e) => write!(f, "連線失敗：{e}"),
            // 429 is the one users hit routinely (the free tier caps requests per
            // minute), so it gets an actionable message instead of a raw status dump.
            GeminiError::Api { status: 429, .. } => write!(
                f,
                "已達 Gemini 每分鐘請求上限（429）。請等約一分鐘後再產生關鍵字。"
            ),
            GeminiError::Api { status, body } => write!(f, "Gemini API 錯誤（{status}）：{body}"),
            GeminiError::Parse(e) => write!(f, "回應格式錯誤：{e}"),
        }
    }
}

#[derive(Deserialize)]
struct GeminiResponse {
    candidates: Option<Vec<Candidate>>,
}
#[derive(Deserialize)]
struct Candidate {
    content: Option<Content>,
}
#[derive(Deserialize)]
struct Content {
    parts: Option<Vec<Part>>,
}
#[derive(Deserialize)]
struct Part {
    text: Option<String>,
}

impl GeminiResponse {
    fn first_text(&self) -> Option<String> {
        self.candidates.as_ref()?.first()?.content.as_ref()?.parts.as_ref()?.first()?.text.clone()
    }
}

fn keyword_prompt(title: &str, body: &str, count: usize) -> String {
    format!(
        "你是新聞編輯，請根據以下新聞標題與內文，產生正好 {count} 個繁體中文關鍵字，\n\
         用途是給讀者搜尋、歸類這則新聞用的標籤。\n\n\
         規則：\n\
         1. 只能使用稿件中明確提到的人事時地物，不可杜撰、不可超譯、不可加入稿件未提及的資訊。\n\
         2. 優先選：人名、地名（縣市/行政區/路名）、機構或店家名稱、案件或事件類型（例如竊盜、鬥毆、詐騙）。\n\
         3. 避免選：金額、數量、時間點、門號、案發時刻這類純數字資訊 —— 這些幾乎每則社會新聞都有，\n\
            當關鍵字沒有辨識度，除非那個數字本身就是新聞的重點（例如「reward」懸賞金額創新高的新聞）。\n\
         4. 可包含英文或數字（不含前述第 3 點排除的情況）；每個關鍵字精簡（通常 2-6 字）。\n\
         5. 直接輸出 JSON 字串陣列，不要加任何說明文字或 markdown 標記。\n\n\
         範例（不是這則稿件的內容，只示範選擇標準）：\n\
         好：[\"台中西區\", \"明禮街\", \"竊盜\", \"監視器\"]\n\
         差：[\"1萬1千元\", \"4分鐘\", \"230支\", \"18:04\"] （純數字資訊，沒有辨識度）\n\n\
         標題：{title}\n內文：{body}"
    )
}

fn keyword_response_schema(count: usize) -> serde_json::Value {
    serde_json::json!({
        "type": "ARRAY",
        "items": { "type": "STRING" },
        "minItems": count,
        "maxItems": count
    })
}

fn normalize_keyword(raw: &str) -> String {
    let trimmed = raw.trim().trim_start_matches('#').trim();
    format!("#{trimmed}")
}

/// Parse the model's JSON-array response text into `#`-prefixed keywords.
/// Pulled out as a pure function so it's testable without a live API call.
pub fn parse_keywords_json(text: &str) -> Result<Vec<String>, GeminiError> {
    // Models sometimes wrap JSON in a ```json fence despite instructions not to;
    // strip that defensively even though responseMimeType=application/json should prevent it.
    let cleaned = text
        .trim()
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();

    let words: Vec<String> =
        serde_json::from_str(cleaned).map_err(|e| GeminiError::Parse(format!("{e}：{cleaned}")))?;
    if words.is_empty() {
        return Err(GeminiError::Parse("關鍵字陣列為空".to_string()));
    }
    Ok(words.iter().map(|w| normalize_keyword(w)).collect())
}

/// Ask Gemini for `count` keywords for one entry. Retries 429/5xx up to 3 times
/// with exponential backoff; other errors return immediately so a single failed
/// entry doesn't stall a batch (the caller decides how to continue the batch).
pub async fn generate_keywords(
    client: &reqwest::Client,
    cfg: &GeminiConfig,
    title: &str,
    body: &str,
    count: usize,
) -> Result<Vec<String>, GeminiError> {
    let url = format!(
        "https://generativelanguage.googleapis.com/v1beta/models/{}:generateContent?key={}",
        cfg.model, cfg.api_key
    );
    let payload = serde_json::json!({
        "contents": [{ "parts": [{ "text": keyword_prompt(title, body, count) }] }],
        "generationConfig": {
            "responseMimeType": "application/json",
            "responseSchema": keyword_response_schema(count),
        }
    });

    let max_attempts = 3;
    let mut last_err = None;
    for attempt in 0..max_attempts {
        if attempt > 0 {
            let backoff_ms = 500u64 * 2u64.pow(attempt as u32 - 1);
            tokio::time::sleep(Duration::from_millis(backoff_ms)).await;
        }

        let resp = match client.post(&url).json(&payload).send().await {
            Ok(r) => r,
            Err(e) => {
                last_err = Some(GeminiError::Transport(e.to_string()));
                continue;
            }
        };

        let status = resp.status();
        if status.is_success() {
            let parsed: GeminiResponse = match resp.json().await {
                Ok(p) => p,
                Err(e) => return Err(GeminiError::Parse(e.to_string())),
            };
            let text = parsed
                .first_text()
                .ok_or_else(|| GeminiError::Parse("回應中沒有內容".to_string()))?;
            return parse_keywords_json(&text);
        }

        let retryable = status.as_u16() == 429 || status.is_server_error();
        let body_text = resp.text().await.unwrap_or_default();
        last_err = Some(GeminiError::Api { status: status.as_u16(), body: body_text });
        if !retryable {
            break;
        }
    }

    Err(last_err.unwrap_or_else(|| GeminiError::Transport("未知錯誤".to_string())))
}

/// Read the API key from `GEMINI_API_KEY`. OS-keyring storage is a Tauri-layer
/// concern (needs the `keyring` crate + a settings UI); this pure-core fallback
/// keeps the core crate testable and lets the key be supplied without a UI yet.
pub fn api_key_from_env() -> Option<String> {
    std::env::var("GEMINI_API_KEY").ok().filter(|k| !k.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keyword_prompt_tells_the_model_to_avoid_bare_numbers() {
        // Regression guard for real output like #1萬1千元 / #4分鐘 / #230支 -- the
        // model was technically obeying "do not fabricate" by picking any literal
        // figure from the body, which produces hashtags with no search value. The
        // prompt needs an explicit steer toward named entities/event type and away
        // from amounts/counts/timestamps, not just a wider "do not fabricate" rule.
        let prompt = keyword_prompt("標題", "內文", 4);
        assert!(prompt.contains("避免選"), "prompt lost its guidance against low-signal numeric keywords");
        assert!(prompt.contains("金額") && prompt.contains("時間點"), "prompt should name the exact failure mode seen in production");
        assert!(prompt.contains("人名") && prompt.contains("地名"), "prompt should steer toward named entities instead");
    }

    #[test]
    fn rate_limit_error_tells_the_user_to_wait_rather_than_dumping_the_status() {
        let err = GeminiError::Api { status: 429, body: "{\"error\":\"RESOURCE_EXHAUSTED\"}".into() };
        assert!(err.is_rate_limited());
        let msg = err.to_string();
        assert!(msg.contains("一分鐘"), "expected actionable wait hint, got: {msg}");
        assert!(!msg.contains("RESOURCE_EXHAUSTED"), "raw body should not leak: {msg}");
    }

    #[test]
    fn non_429_api_errors_are_not_treated_as_rate_limiting() {
        let err = GeminiError::Api { status: 404, body: "model not found".into() };
        assert!(!err.is_rate_limited());
        assert!(err.to_string().contains("404"));
    }

    #[test]
    fn parses_plain_json_array() {
        let words = parse_keywords_json(r#"["合成甲", "合成乙", "合成丙", "合成丁"]"#).unwrap();
        assert_eq!(words, vec!["#合成甲", "#合成乙", "#合成丙", "#合成丁"]);
    }

    #[test]
    fn strips_hash_prefix_if_model_already_added_one() {
        let words = parse_keywords_json(r##"["#AI", "2026"]"##).unwrap();
        assert_eq!(words, vec!["#AI", "#2026"]);
    }

    #[test]
    fn strips_markdown_fence_if_model_ignores_the_instruction() {
        let words = parse_keywords_json("```json\n[\"甲\", \"乙\"]\n```").unwrap();
        assert_eq!(words, vec!["#甲", "#乙"]);
    }

    #[test]
    fn empty_array_is_an_error_not_a_silent_empty_result() {
        assert!(parse_keywords_json("[]").is_err());
    }

    #[test]
    fn malformed_json_is_a_parse_error() {
        assert!(parse_keywords_json("not json").is_err());
    }
}
