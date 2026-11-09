//! rundown 異常保護（v0.3.11）。
//!
//! 自動刷新每分鐘讀一次 rundown。製作人搬稿、還沒重設硬時間的那一分鐘，下面的列
//! 累積時間會整段往前縮，還沒播的稿被算成「已播出」（2026/11/9 12:45：5 則在
//! 12:15～12:24 的位置被寫進文件，下一輪就恢復了）。這裡一輪一輪比較同一節的
//! 已播出列，決定哪些稿這一輪不能寫：
//!
//! - **寫入確認**：連續 `stable_rounds` 輪都是已播出、累積時間沒有大跳才寫。
//! - **異常 → 冷卻**：同一節內「最後納入」倒退、新播出的稿排在已播出範圍之前、或 3 則
//!   以上已播出的稿同時大幅移動，就暫停寫入與校正；連續 `quiet_rounds` 輪穩定才恢復，
//!   最多 `max_cooldown` 秒。
//! - **可疑稿**：異常前還沒播、之後卻排在異常前的「最後納入」之前的稿。冷卻解除或逾時
//!   都不寫，要等它的時間真的推進到那條邊界之後（製作人重設硬時間、主播播到它）。
//!   一直沒推進就一直標在 App 裡讓人判斷。
//!
//! 時間都是拉到節次時間軸上的秒數（跨午夜已經處理過）。

use crate::inews::format_clock;
use serde::Serialize;
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GuardConfig {
    /// 新稿連續幾輪已播出才寫（1 = 不等）。
    pub stable_rounds: u32,
    /// 異常門檻（秒）。
    pub threshold: i64,
    /// 異常後連續幾輪穩定才恢復。
    pub quiet_rounds: u32,
    /// 冷卻上限（秒）。
    pub max_cooldown: i64,
}

impl GuardConfig {
    pub fn from_inews(c: &crate::config::InewsConfig) -> Self {
        GuardConfig {
            stable_rounds: c.write_stable_rounds.max(1),
            threshold: c.anomaly_threshold_seconds.max(1) as i64,
            quiet_rounds: c.anomaly_quiet_rounds.max(1),
            max_cooldown: c.anomaly_max_cooldown_minutes.max(1) as i64 * 60,
        }
    }
}

/// 一列在這一輪的樣子（算不出累積時間的列不放進來）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RowObs {
    pub key: String,
    pub slug: String,
    pub cume: i64,
    pub aired: bool,
}

/// 這一輪不能寫的原因。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Hold {
    /// 第一次判定已播出，等下一輪確認。
    Confirm,
    /// rundown 時間異常，冷卻中。
    Cooldown,
    /// 異常時跑到已播出範圍之前的稿，時間還沒推進。
    Suspect,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Suspect {
    pub show: String,
    pub slug: String,
    /// 目前的累積時間。
    pub cume: i64,
    /// 異常前最後一輪正常時的「最後納入」。
    pub frontier: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Cooldown {
    since: i64,
    quiet: u32,
    /// 進入冷卻前一輪的「最後納入」。
    frontier: Option<i64>,
}

/// 一輪的判斷結果。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Step {
    /// story key → 這一輪不能寫的原因（只列已播出的列）。
    pub holds: HashMap<String, Hold>,
    /// 這一輪判定異常的原因（決策紀錄用）。
    pub anomaly: Option<String>,
    /// 冷卻在這一輪結束（穩定或逾時）的說明。
    pub released: Option<String>,
    /// 冷卻是因為逾時才結束的。
    pub expired: bool,
    /// 這一輪暫停寫入與校正。
    pub cooling: bool,
    /// 這一輪新標成可疑的 story key。
    pub new_suspects: Vec<String>,
}

#[derive(Debug, Default)]
pub struct Guard {
    show: Option<String>,
    /// story key → (累積時間, 是否已播出)，上一輪。
    prev: HashMap<String, (i64, bool)>,
    prev_frontier: Option<i64>,
    /// story key → (上一輪的累積時間, 連續已播出輪數)。
    streak: HashMap<String, (i64, u32)>,
    cooldown: Option<Cooldown>,
    suspects: HashMap<String, Suspect>,
    /// 這一節在可信的輪次（沒有異常）播出過、又不是可疑的稿。搬走再搬回原位的稿本來就
    /// 播過，不能因為它「這一輪才又變成已播出」就標成可疑。
    known: HashSet<String>,
    /// story key → slug，上一輪（認出「換稿」用）。
    prev_slug: HashMap<String, String>,
}

impl Guard {
    /// 換播出日：全部重來。
    pub fn new_day(&mut self) {
        *self = Guard::default();
    }

    pub fn is_suspect(&self, key: &str) -> bool {
        self.suspects.contains_key(key)
    }

    /// 目前所有可疑稿，依節次、時間排序（App 顯示用）。
    pub fn suspects(&self) -> Vec<&Suspect> {
        let mut v: Vec<&Suspect> = self.suspects.values().collect();
        v.sort_by(|a, b| (&a.show, a.cume, &a.slug).cmp(&(&b.show, b.cume, &b.slug)));
        v
    }

    /// 目前節次的一輪。`now` 是節次時間軸上的現在時間。
    pub fn step(&mut self, show: &str, rows: &[RowObs], now: i64, cfg: &GuardConfig) -> Step {
        if self.show.as_deref() != Some(show) {
            // 換節：時間軸不同，不能跟上一節比。可疑稿留著，補掃、複查還要用。
            self.show = Some(show.to_string());
            self.prev.clear();
            self.prev_slug.clear();
            self.prev_frontier = None;
            self.streak.clear();
            self.known.clear();
            self.cooldown = None;
        }
        let th = cfg.threshold;
        let aired: Vec<&RowObs> = rows.iter().filter(|r| r.aired).collect();
        let frontier = aired.iter().map(|r| r.cume).max();
        // 不算「往過去插入」的新列（v0.3.13，11/9 15:23 `里長介社區1430` 誤判）：
        // - 換稿：上一輪已播出、這一輪不見了的列裡有同一個 slug，時間差在門檻內。製作人把
        //   那一列換成新存的稿（同 slug、同位置，iNEWS ID 變了）。
        // - 長度 0：下一列的時間跟它一樣。插進去不會讓任何一列移動，不可能是把還沒播的稿
        //   往前搬（真的搬稿一定會把後面已播出的列往後推）。
        let present: HashSet<&str> = aired.iter().map(|r| r.key.as_str()).collect();
        let vanished: Vec<(&str, i64)> = self
            .prev
            .iter()
            .filter(|(k, (_, a))| *a && !present.contains(k.as_str()))
            .filter_map(|(k, (c, _))| self.prev_slug.get(k).filter(|s| !s.is_empty()).map(|s| (s.as_str(), *c)))
            .collect();
        let benign: HashSet<&str> = rows
            .iter()
            .enumerate()
            .filter(|(_, r)| r.aired && !matches!(self.prev.get(&r.key), Some((_, true))))
            .filter(|(i, r)| {
                let zero_len = rows.get(i + 1).is_some_and(|next| next.cume == r.cume);
                let replaced = !r.slug.is_empty() && vanished.iter().any(|(s, c)| *s == r.slug && (r.cume - c).abs() <= th);
                zero_len || replaced
            })
            .map(|(_, r)| r.key.as_str())
            .collect();
        let was_aired = |key: &str| {
            matches!(self.prev.get(key), Some((_, true))) || self.known.contains(key) || benign.contains(key)
        };

        // 1. 異常偵測（要有上一輪才比）。
        let mut reasons = Vec::new();
        if let Some(pf) = self.prev_frontier {
            match frontier {
                Some(f) if f < pf - th => {
                    reasons.push(format!("最後納入 {} → {}（倒退 {} 秒）", format_clock(pf), format_clock(f), pf - f))
                }
                None => reasons.push(format!("最後納入 {} → 沒有已播出的稿", format_clock(pf))),
                _ => {}
            }
            let inserted = aired.iter().filter(|r| !was_aired(&r.key) && r.cume < pf - th).count();
            if inserted > 0 {
                reasons.push(format!("{inserted} 則新播出的稿排在已播出範圍（{}）之前", format_clock(pf)));
            }
            let moved = aired
                .iter()
                .filter(|r| matches!(self.prev.get(&r.key), Some((c, true)) if (r.cume - c).abs() > th))
                .count();
            if moved >= 3 {
                reasons.push(format!("{moved} 則已播出的稿時間同時移動超過 {th} 秒"));
            }
        }
        let anomaly = (!reasons.is_empty()).then(|| reasons.join("；"));

        // 2. 可疑稿：這一輪才變成已播出、卻排在（異常前的）最後納入之前。
        let reference = self.cooldown.as_ref().and_then(|c| c.frontier).or(self.prev_frontier);
        let mut new_suspects = Vec::new();
        if let Some(rf) = reference {
            for r in aired.iter().filter(|r| !was_aired(&r.key) && r.cume < rf - th) {
                if !self.suspects.contains_key(&r.key) {
                    new_suspects.push(r.key.clone());
                }
                self.suspects.insert(
                    r.key.clone(),
                    Suspect { show: show.to_string(), slug: r.slug.clone(), cume: r.cume, frontier: rf },
                );
            }
        }
        // 時間推進到邊界之後、或不再是已播出（製作人改回去了）：不再可疑。
        let by_key: HashMap<&str, &RowObs> = rows.iter().map(|r| (r.key.as_str(), r)).collect();
        self.suspects.retain(|key, s| {
            if s.show != show {
                return true;
            }
            match by_key.get(key.as_str()) {
                Some(r) if r.aired && r.cume < s.frontier - th => {
                    s.cume = r.cume;
                    true
                }
                _ => false,
            }
        });

        // 3. 冷卻。
        let mut released = None;
        let mut expired = false;
        if anomaly.is_some() {
            match &mut self.cooldown {
                Some(c) => c.quiet = 0,
                None => self.cooldown = Some(Cooldown { since: now, quiet: 0, frontier: self.prev_frontier }),
            }
        } else if let Some(c) = &mut self.cooldown {
            c.quiet += 1;
            if c.quiet >= cfg.quiet_rounds {
                released = Some(format!("連續 {} 輪穩定，冷卻 {} 分鐘", c.quiet, (now - c.since).max(0) / 60));
                self.cooldown = None;
            }
        }
        if let Some(c) = &self.cooldown {
            if now - c.since >= cfg.max_cooldown {
                expired = true;
                released = Some(format!(
                    "冷卻超過 {} 分鐘仍不穩定，恢復一般判定（可疑稿照樣不寫）",
                    cfg.max_cooldown / 60
                ));
                self.cooldown = None;
            }
        }
        let cooling = self.cooldown.is_some();

        // 4. 寫入確認。
        let mut streak = HashMap::new();
        for r in &aired {
            let n = match self.streak.get(&r.key) {
                Some((c, n)) if (r.cume - c).abs() <= th => n + 1,
                _ => 1,
            };
            streak.insert(r.key.clone(), (r.cume, n));
        }
        let mut holds = HashMap::new();
        for r in &aired {
            let hold = if self.suspects.contains_key(&r.key) {
                Some(Hold::Suspect)
            } else if cooling {
                Some(Hold::Cooldown)
            } else if streak[&r.key].1 < cfg.stable_rounds {
                Some(Hold::Confirm)
            } else {
                None
            };
            if let Some(h) = hold {
                holds.insert(r.key.clone(), h);
            }
        }

        self.streak = streak;
        self.prev = rows.iter().map(|r| (r.key.clone(), (r.cume, r.aired))).collect();
        self.prev_slug = rows.iter().map(|r| (r.key.clone(), r.slug.clone())).collect();
        self.prev_frontier = frontier;
        if anomaly.is_none() {
            for r in &aired {
                if !self.suspects.contains_key(&r.key) {
                    self.known.insert(r.key.clone());
                }
            }
        }
        Step { holds, anomaly, released, expired, cooling, new_suspects }
    }

    /// 補掃、換節最後同步、複查的稿：還是可疑就回 true。時間已經推進到邊界之後的
    /// 不再可疑。`cume` 是那一節時間軸上的累積時間。
    pub fn hold_suspect(&mut self, show: &str, key: &str, cume: i64, threshold: i64) -> bool {
        match self.suspects.get(key) {
            Some(s) if s.show == show && cume < s.frontier - threshold => {
                if let Some(s) = self.suspects.get_mut(key) {
                    s.cume = cume;
                }
                true
            }
            Some(s) if s.show == show => {
                self.suspects.remove(key);
                false
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CFG: GuardConfig = GuardConfig { stable_rounds: 2, threshold: 120, quiet_rounds: 3, max_cooldown: 15 * 60 };

    fn t(s: &str) -> i64 {
        let p: Vec<i64> = s.split(':').map(|x| x.parse().unwrap()).collect();
        p[0] * 3600 + p[1] * 60 + p[2]
    }

    /// (key, 累積時間) 的 rundown，`now` 之前的算已播出。
    fn rows(list: &[(&str, &str)], now: &str) -> Vec<RowObs> {
        list.iter().map(|(k, c)| RowObs { key: k.to_string(), slug: k.to_string(), cume: t(c), aired: t(c) <= t(now) }).collect()
    }

    /// 11/9 1200 的簡化版：A～E 已播出到 12:43，M1～M3 排在後面還沒播。
    fn normal() -> Vec<(&'static str, &'static str)> {
        vec![
            ("A", "12:01:55"),
            ("B", "12:08:49"),
            ("C", "12:19:18"),
            ("D", "12:33:56"),
            ("E", "12:43:05"),
            ("M1", "12:52:00"),
            ("M2", "12:54:13"),
            ("M3", "12:58:33"),
        ]
    }

    /// 搬稿那一分鐘：M1～M3 跑到 12:15～12:24。
    fn moved() -> Vec<(&'static str, &'static str)> {
        vec![
            ("A", "12:01:55"),
            ("B", "12:08:49"),
            ("M1", "12:15:02"),
            ("M2", "12:17:15"),
            ("M3", "12:21:35"),
            ("C", "12:24:23"),
            ("D", "12:30:00"),
            ("E", "12:35:46"),
        ]
    }

    fn warm(g: &mut Guard) {
        g.step("1200", &rows(&normal(), "12:43:20"), t("12:43:20"), &CFG);
        g.step("1200", &rows(&normal(), "12:44:20"), t("12:44:20"), &CFG);
    }

    #[test]
    fn a_new_story_waits_one_round_before_it_is_written() {
        let mut g = Guard::default();
        warm(&mut g);
        let mut list = normal();
        list.insert(5, ("F", "12:44:50"));
        let s = g.step("1200", &rows(&list, "12:45:20"), t("12:45:20"), &CFG);
        assert_eq!(s.holds.get("F"), Some(&Hold::Confirm));
        assert!(s.anomaly.is_none() && !s.cooling);
        assert!(!s.holds.contains_key("E"));
        let s = g.step("1200", &rows(&list, "12:46:20"), t("12:46:20"), &CFG);
        assert!(!s.holds.contains_key("F"));
    }

    #[test]
    fn a_story_aired_for_one_round_only_is_never_released() {
        let mut g = Guard::default();
        let cfg = GuardConfig { threshold: 600, ..CFG }; // 不讓異常偵測搶先擋下
        g.step("1200", &rows(&normal(), "12:43:20"), t("12:43:20"), &cfg);
        g.step("1200", &rows(&normal(), "12:44:20"), t("12:44:20"), &cfg);
        let mut list = normal();
        list[5].1 = "12:44:10";
        let s = g.step("1200", &rows(&list, "12:45:20"), t("12:45:20"), &cfg);
        assert_eq!(s.holds.get("M1"), Some(&Hold::Confirm));
        let s = g.step("1200", &rows(&normal(), "12:46:20"), t("12:46:20"), &cfg);
        assert!(!s.holds.contains_key("M1"), "not aired any more, so not offered at all");
    }

    #[test]
    fn the_1109_1245_round_is_an_anomaly_and_its_stories_are_suspects() {
        let mut g = Guard::default();
        warm(&mut g);
        let s = g.step("1200", &rows(&moved(), "12:45:20"), t("12:45:20"), &CFG);
        let why = s.anomaly.clone().expect("anomaly");
        assert!(why.contains("倒退"), "{why}");
        assert!(why.contains("3 則新播出的稿"), "{why}");
        assert!(s.cooling);
        for k in ["M1", "M2", "M3"] {
            assert_eq!(s.holds.get(k), Some(&Hold::Suspect), "{k}");
        }
        assert_eq!(s.holds.get("A"), Some(&Hold::Cooldown));
        assert_eq!(s.new_suspects.len(), 3);
        let listed: Vec<&str> = g.suspects().iter().map(|x| x.slug.as_str()).collect();
        assert_eq!(listed, ["M1", "M2", "M3"]);

        // 下一輪製作人重設好了：M1～M3 回到後面，不再可疑。時間整段跳回去，這一輪
        // 本身也算異常，之後再穩定 3 輪才恢復。
        let s = g.step("1200", &rows(&normal(), "12:46:20"), t("12:46:20"), &CFG);
        assert!(s.cooling && s.anomaly.is_some());
        assert!(!g.is_suspect("M1"));
        for m in 47..=48 {
            let now = format!("12:{m}:20");
            assert!(g.step("1200", &rows(&normal(), &now), t(&now), &CFG).cooling);
        }
        let s = g.step("1200", &rows(&normal(), "12:49:20"), t("12:49:20"), &CFG);
        assert!(!s.cooling);
        assert!(s.released.unwrap().contains("連續 3 輪穩定"));
        assert!(s.holds.is_empty());
    }

    #[test]
    fn a_long_move_keeps_the_stories_suspect_after_the_cooldown_ends() {
        let mut g = Guard::default();
        warm(&mut g);
        // 製作人一直沒重設：錯的時間穩定地留著。
        let mut s = g.step("1200", &rows(&moved(), "12:45:20"), t("12:45:20"), &CFG);
        for m in 46..=49 {
            let now = format!("12:{m}:20");
            s = g.step("1200", &rows(&moved(), &now), t(&now), &CFG);
        }
        assert!(!s.cooling, "3 quiet rounds end the cooldown");
        assert_eq!(s.holds.get("M2"), Some(&Hold::Suspect), "but never releases the suspects");
        assert!(!s.holds.contains_key("A"));

        // 補掃時也照樣擋。
        assert!(g.hold_suspect("1200", "M2", t("12:17:15"), CFG.threshold));
        // 真的播到它（時間推進到 12:43:05 之後）就放行。
        assert!(!g.hold_suspect("1200", "M2", t("12:50:00"), CFG.threshold));
        assert!(!g.is_suspect("M2"));
    }

    #[test]
    fn a_suspect_is_released_once_it_really_airs() {
        let mut g = Guard::default();
        warm(&mut g);
        g.step("1200", &rows(&moved(), "12:45:20"), t("12:45:20"), &CFG);
        assert!(g.is_suspect("M1"));
        // 重設後 M1 排在 12:52，12:53 真的播出。
        let s = g.step("1200", &rows(&normal(), "12:53:00"), t("12:53:00"), &CFG);
        assert!(!g.is_suspect("M1"));
        assert_eq!(s.holds.get("M1"), Some(&Hold::Cooldown));
    }

    #[test]
    fn the_cooldown_has_a_ceiling() {
        let mut g = Guard::default();
        warm(&mut g);
        let mut s = g.step("1200", &rows(&moved(), "12:45:20"), t("12:45:20"), &CFG);
        // 每一輪都在亂動。
        for m in 0..16 {
            let now = t("12:46:20") + m * 60;
            let list = if m % 2 == 0 { normal() } else { moved() };
            let r: Vec<RowObs> =
                list.iter().map(|(k, c)| RowObs { key: k.to_string(), slug: k.to_string(), cume: t(c), aired: t(c) <= now }).collect();
            s = g.step("1200", &r, now, &CFG);
            if s.expired {
                break;
            }
        }
        assert!(s.expired && !s.cooling);
        assert!(s.released.unwrap().contains("15 分鐘"));
    }

    #[test]
    fn an_aired_story_moved_away_and_back_is_not_a_suspect() {
        let mut g = Guard::default();
        warm(&mut g);
        // D（12:33:56 播過）被拖到最後面，變成還沒播。
        let away = vec![("A", "12:01:55"), ("B", "12:08:49"), ("C", "12:19:18"), ("E", "12:33:56"), ("D", "12:52:00")];
        g.step("1200", &rows(&away, "12:45:20"), t("12:45:20"), &CFG);
        // 又被拖回原位：這一輪「才又變成已播出」，但它本來就播過。
        let s = g.step("1200", &rows(&normal(), "12:46:20"), t("12:46:20"), &CFG);
        assert!(s.new_suspects.is_empty(), "{:?}", s.new_suspects);
        assert_ne!(s.holds.get("D"), Some(&Hold::Suspect));
    }

    /// 11/9 15:23 1500 的形狀（slug 換成合成的）：時間一秒都沒動，只是一列換成新存的稿
    /// （同 slug、新 ID），再多一個長度 0 的空白列。
    fn swap_rounds() -> (Vec<RowObs>, Vec<RowObs>) {
        let row = |k: &str, slug: &str, c: &str, aired: bool| RowObs { key: k.into(), slug: slug.into(), cume: t(c), aired };
        let before = vec![
            row("P", "拍合成1500", "15:00:27", true),
            row("R-old", "里長合成1430", "15:01:16", true),
            row("H", "火車合成1200", "15:02:53", true),
            row("Q", "撤合成1800", "15:09:52", true),
            row("CM3", "=====CM3=====", "15:20:57", true),
            row("N", "番茄合成1420", "15:23:44", false),
        ];
        let after = vec![
            row("P", "拍合成1500", "15:00:27", true),
            row("R-new", "里長合成1430", "15:01:16", true),
            row("H", "火車合成1200", "15:02:53", true),
            row("BLANK", "", "15:09:52", true),
            row("Q", "撤合成1800", "15:09:52", true),
            row("CM3", "=====CM3=====", "15:20:57", true),
            row("N", "番茄合成1420", "15:23:44", false),
        ];
        (before, after)
    }

    #[test]
    fn a_story_swapped_for_a_new_copy_and_a_zero_length_row_are_not_an_anomaly() {
        let (before, after) = swap_rounds();
        let mut g = Guard::default();
        g.step("1500", &before, t("15:21:31"), &CFG);
        g.step("1500", &before, t("15:22:31"), &CFG);
        let s = g.step("1500", &after, t("15:23:32"), &CFG);
        assert!(s.anomaly.is_none(), "{:?}", s.anomaly);
        assert!(!s.cooling && s.new_suspects.is_empty() && g.suspects().is_empty());
        // 新 ID 是新的一則：照一般流程等一輪確認（文件裡已經有了，比對會擋重複寫入）。
        assert_eq!(s.holds.get("R-new"), Some(&Hold::Confirm));
        assert_eq!(s.holds.get("P"), None);
    }

    #[test]
    fn a_swap_to_a_different_slug_is_still_suspect() {
        // 換上去的是別的 slug、而且有長度：跟搬稿分不出來，照樣擋。
        let (before, mut after) = swap_rounds();
        after[1].slug = "別的合成1500".into();
        after.remove(3);
        let mut g = Guard::default();
        g.step("1500", &before, t("15:21:31"), &CFG);
        let s = g.step("1500", &after, t("15:23:32"), &CFG);
        assert!(s.anomaly.is_some());
        assert_eq!(s.holds.get("R-new"), Some(&Hold::Suspect));
    }

    #[test]
    fn normal_progress_with_small_retimes_is_not_an_anomaly() {
        let mut g = Guard::default();
        let list = normal();
        let mut now = t("12:40:00");
        for i in 0..10 {
            // 每一輪已播出的稿時間都動個 ±40 秒（改長度），現在時間往前走。
            let jitter = if i % 2 == 0 { 40 } else { -40 };
            let r: Vec<RowObs> = list
                .iter()
                .map(|(k, c)| RowObs { key: k.to_string(), slug: k.to_string(), cume: t(c) + jitter, aired: t(c) + jitter <= now })
                .collect();
            let s = g.step("1200", &r, now, &CFG);
            assert!(s.anomaly.is_none(), "round {i}: {:?}", s.anomaly);
            now += 60;
        }
    }

    #[test]
    fn a_145_second_retime_of_many_aired_stories_pauses_briefly() {
        // 11/7 1800：改長度讓 9 則同時 −145 秒。門檻 2 分鐘時算異常，暫停幾輪。
        let mut g = Guard::default();
        let base: Vec<(String, i64)> = (0..9).map(|i| (format!("S{i}"), t("18:03:00") + i * 150)).collect();
        let r = |shift: i64, now: i64| -> Vec<RowObs> {
            base.iter().map(|(k, c)| RowObs { key: k.clone(), slug: k.clone(), cume: c + shift, aired: c + shift <= now }).collect()
        };
        g.step("1800", &r(0, t("18:48:00")), t("18:48:00"), &CFG);
        let s = g.step("1800", &r(-145, t("18:49:00")), t("18:49:00"), &CFG);
        assert!(s.anomaly.unwrap().contains("9 則已播出的稿"));
        assert!(s.new_suspects.is_empty(), "already aired stories never become suspects");
        for m in 50..=52 {
            let now = t(&format!("18:{m}:00"));
            let s = g.step("1800", &r(-145, now), now, &CFG);
            assert_eq!(s.cooling, m < 52);
        }
    }

    #[test]
    fn switching_show_starts_over_but_keeps_suspects_for_the_sweep() {
        let mut g = Guard::default();
        warm(&mut g);
        g.step("1200", &rows(&moved(), "12:45:20"), t("12:45:20"), &CFG);
        let s = g.step("1400", &rows(&[("X", "13:56:00")], "13:57:00"), t("13:57:00"), &CFG);
        assert!(s.anomaly.is_none() && !s.cooling);
        assert_eq!(s.holds.get("X"), Some(&Hold::Confirm));
        assert!(g.hold_suspect("1200", "M3", t("12:21:35"), CFG.threshold));
        g.new_day();
        assert!(!g.is_suspect("M3"));
    }

    #[test]
    fn stable_rounds_of_one_writes_at_once() {
        let mut g = Guard::default();
        let cfg = GuardConfig { stable_rounds: 1, ..CFG };
        let s = g.step("1200", &rows(&normal(), "12:44:20"), t("12:44:20"), &cfg);
        assert!(s.holds.is_empty());
    }
}
