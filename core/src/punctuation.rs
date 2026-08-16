use crate::config::PunctuationConfig;
use regex::Regex;
use once_cell::sync::Lazy;

// URL character class is restricted to ASCII URL-safe characters so the match
// naturally stops at the first CJK character even when there's no whitespace
// separating a URL from the following prose (real scripts do this).
/// Byte-indexed mask of which positions in `input` sit inside a URL, so other passes
/// (marker stripping in `clean`) can leave URLs alone the same way this module does.
pub(crate) fn url_mask(input: &str) -> Vec<bool> {
    let mut mask = vec![false; input.len()];
    for m in URL_RE.find_iter(input) {
        for i in m.start()..m.end() {
            mask[i] = true;
        }
    }
    mask
}

static URL_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"(?i)(?:https?|ftp)[:：]//[A-Za-z0-9\-._~:/?#\[\]@!$&'()*+,;=%]+|www\.[A-Za-z0-9\-._~:/?#\[\]@!$&'()*+,;=%]+",
    )
    .unwrap()
});

#[derive(Debug, Clone, Default, PartialEq)]
pub struct PunctuationResult {
    pub text: String,
    pub warnings: Vec<String>,
}

/// Normalize punctuation per spec §4: paired straight quotes -> corner brackets,
/// `.` -> `、` unless flanked by digits on both sides, and a handful of halfwidth
/// symbols -> fullwidth. URLs are protected from every transform, and whitespace
/// (other than leading/trailing trim) is preserved.
pub fn normalize(input: &str, cfg: &PunctuationConfig) -> PunctuationResult {
    let input = input.trim();
    let mut is_url = vec![false; input.len()];
    if cfg.protect_urls {
        for m in URL_RE.find_iter(input) {
            for i in m.start()..m.end() {
                is_url[i] = true;
            }
        }
    }

    let mut warnings = Vec::new();
    let chars: Vec<(usize, char)> = input.char_indices().collect();
    let mut out = String::with_capacity(input.len());
    let mut quote_open = false;
    let mut quote_count = 0usize;

    for (idx, (byte_i, c)) in chars.iter().enumerate() {
        if is_url[*byte_i] {
            out.push(*c);
            continue;
        }

        if cfg.quotes_to_corner && *c == '"' {
            quote_count += 1;
            out.push(if quote_open { '」' } else { '「' });
            quote_open = !quote_open;
            continue;
        }

        if cfg.dot_to_enumeration && *c == '.' {
            let prev_digit = idx > 0 && {
                let (pb, pc) = chars[idx - 1];
                !is_url[pb] && pc.is_ascii_digit()
            };
            let next_digit = idx + 1 < chars.len() && {
                let (nb, nc) = chars[idx + 1];
                !is_url[nb] && nc.is_ascii_digit()
            };
            if prev_digit && next_digit {
                out.push('.');
            } else {
                out.push('、');
            }
            continue;
        }

        let mapped = cfg.map.get(&c.to_string());
        if let Some(m) = mapped {
            out.push_str(m);
            continue;
        }

        out.push(*c);
    }

    if quote_count % 2 != 0 {
        warnings.push(format!("引號數量為單數（{} 個），請確認「」配對是否正確", quote_count));
    }

    PunctuationResult { text: out, warnings }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::PunctuationConfig;

    fn cfg() -> PunctuationConfig {
        PunctuationConfig::default()
    }

    #[test]
    fn quotes_pair_alternating() {
        let r = normalize(r#"車主控新車被瞞"烤漆.換把手""#, &cfg());
        assert_eq!(r.text, "車主控新車被瞞「烤漆、換把手」");
        assert!(r.warnings.is_empty());
    }

    #[test]
    fn odd_quote_count_warns() {
        let r = normalize(r#"開頭"沒收尾"#, &cfg());
        assert_eq!(r.text, "開頭「沒收尾");
        assert_eq!(r.warnings.len(), 1);
    }

    #[test]
    fn dot_between_words_becomes_enumeration_mark() {
        let r = normalize("SUP.海釣.烤肉", &cfg());
        assert_eq!(r.text, "SUP、海釣、烤肉");
    }

    #[test]
    fn dot_after_number_before_word_becomes_enumeration_mark() {
        let r = normalize("至少111死.逾千棟房受損", &cfg());
        assert_eq!(r.text, "至少111死、逾千棟房受損");
    }

    #[test]
    fn dot_between_digits_is_preserved() {
        let r = normalize("哥倫比亞7.4強震", &cfg());
        assert_eq!(r.text, "哥倫比亞7.4強震");
    }

    #[test]
    fn halfwidth_symbols_become_fullwidth_but_letters_and_digits_stay_halfwidth() {
        let r = normalize("記者A:B(2026)!", &cfg());
        assert_eq!(r.text, "記者A：B（2026）！");
    }

    #[test]
    fn url_is_fully_protected_from_all_transforms() {
        let r = normalize(
            "詳見 https://www.youtube.com/watch?v=PU7gRlGA7wE 謝謝收看",
            &cfg(),
        );
        assert_eq!(
            r.text,
            "詳見 https://www.youtube.com/watch?v=PU7gRlGA7wE 謝謝收看"
        );
    }

    #[test]
    fn halfwidth_space_and_other_symbols_are_untouched() {
        let r = normalize("A B 100%-200 …完", &cfg());
        assert_eq!(r.text, "A B 100%-200 …完");
    }
}
