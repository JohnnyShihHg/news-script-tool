//! iNEWS FTP 用戶端與自動匯入。
//!
//! 只用 std 的 `TcpStream`（不加 FTP 套件），所有連線都是阻塞的，由呼叫端放進
//! `spawn_blocking`。判斷邏輯（LIST 解析、節次、截止點、累積時間）全在
//! `news_script_core::inews`，這裡只負責連線、快取與把結果串起來。
//!
//! 伺服器行為（台內實測，banner `220 NEWS Rxnet (ftp server) 3.5.0.121`）：
//! - USER/PASS → `TYPE A` → `SITE FORMAT=3NSML`（回 200）。`SITE LONGNAMES` 會回 501，不送。
//! - 指令 UTF-8、**絕對不能帶 BOM**（帶 BOM 第一個指令會被回 `500 not recognized`）。
//! - 路徑是點號串起來的完整路徑，一次 CWD 到底；`CWD /` 只會回到個人資料夾。
//! - 資料連線用 PASV。回應可能是多行（`123-` 開頭）。空 queue 可能回 550。

use news_script_core::config::InewsConfig;
use news_script_core::doc_match::TimeRow;
use news_script_core::guard::{Guard, GuardConfig, Hold, RowObs};
use news_script_core::inews::{self, ListEntry, ListKind, NsmlStory, RundownRow};
use news_script_core::parse;
use news_script_core::rundown::{self, RundownDay, RundownStory};
use news_script_core::search::{SearchDay, SearchEntry};
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream, ToSocketAddrs};
use std::time::{Duration, Instant};

const IO_TIMEOUT: Duration = Duration::from_secs(15);
/// 節次清單不常變，十分鐘重抓一次就好，不必每一輪都 LIST 根目錄。
const SHOW_LIST_TTL: Duration = Duration::from_secs(600);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reply {
    pub code: u16,
    pub text: String,
}

impl Reply {
    fn is(&self, class: u16) -> bool {
        self.code / 100 == class
    }
}

#[derive(Debug)]
pub enum FtpError {
    /// 連線層的錯誤：可以重連再試。
    Io(std::io::Error),
    /// 伺服器回了不預期的代碼。
    Reply { command: String, reply: Reply },
    Protocol(String),
}

impl std::fmt::Display for FtpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FtpError::Io(e) => write!(f, "連線錯誤：{e}"),
            FtpError::Reply { command, reply } => write!(f, "{command} 失敗：{} {}", reply.code, reply.text.trim()),
            FtpError::Protocol(s) => write!(f, "{s}"),
        }
    }
}

impl From<std::io::Error> for FtpError {
    fn from(e: std::io::Error) -> Self {
        FtpError::Io(e)
    }
}

type FtpResult<T> = Result<T, FtpError>;

/// `227 Entering Passive Mode (10,3,125,1,195,80)` → 10.3.125.1:50000。
pub fn parse_pasv(text: &str) -> Option<SocketAddr> {
    let start = text.find('(')?;
    let end = text[start..].find(')')? + start;
    let nums: Vec<u8> = text[start + 1..end].split(',').map(|n| n.trim().parse().ok()).collect::<Option<_>>()?;
    if nums.len() != 6 {
        return None;
    }
    let ip = Ipv4Addr::new(nums[0], nums[1], nums[2], nums[3]);
    Some(SocketAddr::new(IpAddr::V4(ip), u16::from(nums[4]) * 256 + u16::from(nums[5])))
}

/// 一個控制連線。
pub struct FtpSession {
    reader: BufReader<TcpStream>,
    writer: TcpStream,
    peer: IpAddr,
    pub banner: String,
}

impl FtpSession {
    pub fn connect(host: &str, port: u16) -> FtpResult<Self> {
        let addr = (host, port)
            .to_socket_addrs()?
            .next()
            .ok_or_else(|| FtpError::Protocol(format!("找不到主機 {host}")))?;
        let stream = TcpStream::connect_timeout(&addr, IO_TIMEOUT)?;
        stream.set_read_timeout(Some(IO_TIMEOUT))?;
        stream.set_write_timeout(Some(IO_TIMEOUT))?;
        let writer = stream.try_clone()?;
        let mut session = FtpSession { reader: BufReader::new(stream), writer, peer: addr.ip(), banner: String::new() };
        let banner = session.read_reply()?;
        if banner.code != 220 {
            return Err(FtpError::Reply { command: "連線".into(), reply: banner });
        }
        session.banner = banner.text.trim().to_string();
        Ok(session)
    }

    /// 讀一個（可能多行的）回應。多行是 `123-…` 開頭，到 `123 …` 結束。
    pub fn read_reply(&mut self) -> FtpResult<Reply> {
        let first = self.read_line()?;
        let code = parse_code(&first).ok_or_else(|| FtpError::Protocol(format!("看不懂的回應：{first}")))?;
        let mut text = first.get(4..).unwrap_or("").to_string();
        if first.as_bytes().get(3) == Some(&b'-') {
            let end = format!("{code} ");
            loop {
                let line = self.read_line()?;
                text.push('\n');
                if let Some(rest) = line.strip_prefix(&end) {
                    text.push_str(rest);
                    break;
                }
                text.push_str(&line);
            }
        }
        Ok(Reply { code, text })
    }

    fn read_line(&mut self) -> FtpResult<String> {
        let mut buf = Vec::new();
        let n = self.reader.read_until(b'\n', &mut buf)?;
        if n == 0 {
            return Err(FtpError::Io(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "伺服器關閉了連線")));
        }
        Ok(String::from_utf8_lossy(&buf).trim_end_matches(['\r', '\n']).to_string())
    }

    /// 送一個指令。純 UTF-8、CRLF 結尾、不帶 BOM。
    pub fn command(&mut self, line: &str) -> FtpResult<Reply> {
        self.writer.write_all(format!("{line}\r\n").as_bytes())?;
        self.writer.flush()?;
        self.read_reply()
    }

    fn expect(&mut self, line: &str, class: u16) -> FtpResult<Reply> {
        let reply = self.command(line)?;
        if reply.is(class) {
            Ok(reply)
        } else {
            // 錯誤訊息不能帶出密碼。
            let command = if line.starts_with("PASS ") { "PASS".to_string() } else { line.to_string() };
            Err(FtpError::Reply { command, reply })
        }
    }

    pub fn login(&mut self, user: &str, pass: &str) -> FtpResult<()> {
        let reply = self.command(&format!("USER {user}"))?;
        match reply.code {
            230 => {}
            331 | 332 => {
                self.expect(&format!("PASS {pass}"), 2)?;
            }
            _ => return Err(FtpError::Reply { command: "USER".into(), reply }),
        }
        self.expect("TYPE A", 2)?;
        self.expect("SITE FORMAT=3NSML", 2)?;
        Ok(())
    }

    pub fn cwd(&mut self, path: &str) -> FtpResult<()> {
        self.expect(&format!("CWD {path}"), 2).map(|_| ())
    }

    fn open_data(&mut self) -> FtpResult<TcpStream> {
        let reply = self.expect("PASV", 2)?;
        let mut addr =
            parse_pasv(&reply.text).ok_or_else(|| FtpError::Protocol(format!("看不懂的 PASV 回應：{}", reply.text)))?;
        // 有些伺服器在 NAT 後面會回 0.0.0.0，改連控制連線的那台。
        if addr.ip().is_unspecified() {
            addr.set_ip(self.peer);
        }
        let data = TcpStream::connect_timeout(&addr, IO_TIMEOUT)?;
        data.set_read_timeout(Some(IO_TIMEOUT))?;
        Ok(data)
    }

    /// PASV + 指令 + 讀完資料連線 + 等 226。`550`（空 queue）回 None。
    fn transfer(&mut self, line: &str) -> FtpResult<Option<Vec<u8>>> {
        let mut data = self.open_data()?;
        let reply = self.command(line)?;
        if reply.code == 550 {
            return Ok(None);
        }
        if !reply.is(1) {
            return Err(FtpError::Reply { command: line.to_string(), reply });
        }
        let mut buf = Vec::new();
        data.read_to_end(&mut buf)?;
        drop(data);
        let done = self.read_reply()?;
        if !done.is(2) {
            return Err(FtpError::Reply { command: line.to_string(), reply: done });
        }
        Ok(Some(buf))
    }

    /// 目前資料夾的 LIST，順序就是 rundown 順序。空 queue（550）是空清單。
    pub fn list(&mut self) -> FtpResult<Vec<ListEntry>> {
        let bytes = self.transfer("LIST")?.unwrap_or_default();
        Ok(inews::parse_list(&parse::decode_and_normalize(&bytes)))
    }

    pub fn retr(&mut self, name: &str) -> FtpResult<String> {
        match self.transfer(&format!("RETR {name}"))? {
            Some(bytes) => Ok(parse::decode_and_normalize(&bytes)),
            None => Err(FtpError::Protocol(format!("讀不到稿件 {name}（550）"))),
        }
    }

    pub fn quit(mut self) {
        let _ = self.command("QUIT");
    }
}

fn parse_code(line: &str) -> Option<u16> {
    let code = line.get(..3)?;
    if code.chars().all(|c| c.is_ascii_digit()) {
        code.parse().ok()
    } else {
        None
    }
}

#[derive(Clone)]
pub struct Credentials {
    pub user: String,
    pub pass: String,
}

/// 測試連線：登入、進根目錄、數節次。用全新的連線，不碰自動匯入的那條。
pub fn test_connection(cfg: &InewsConfig, creds: &Credentials) -> Result<String, String> {
    let mut s = FtpSession::connect(&cfg.server, cfg.port).map_err(|e| format!("連不上 {}：{e}", cfg.server))?;
    let banner = s.banner.clone();
    s.login(&creds.user, &creds.pass).map_err(|e| format!("登入失敗：{e}"))?;
    s.cwd(&cfg.root).map_err(|e| format!("登入成功，但進不了 {}：{e}", cfg.root))?;
    let shows = s.list().map_err(|e| format!("登入成功，但列不出 {}：{e}", cfg.root))?;
    let n = shows.iter().filter(|e| e.kind == ListKind::Folder && inews::show_start(&e.name).is_some()).count();
    s.quit();
    Ok(format!("登入成功（{banner}）。{} 底下有 {n} 個節次。", cfg.root))
}

/// 預列清單一列一列看的時候怎麼處理（v0.3.26）：還沒拿到任何一則時空白列跳過，
/// 拿到之後遇到空白列就停。
#[derive(Debug, PartialEq)]
enum TopBlockStep {
    Skip,
    Stop,
    Take,
}

fn top_block_step(collected: usize, slug: &str) -> TopBlockStep {
    match (slug.trim().is_empty(), collected) {
        (true, 0) => TopBlockStep::Skip,
        (true, _) => TopBlockStep::Stop,
        (false, _) => TopBlockStep::Take,
    }
}

#[cfg(test)]
mod top_block_tests {
    use super::*;

    /// 11/14 APPR 的樣子：第一列留空，接著五則早班稿，再來空白列和別的稿。
    #[test]
    fn leading_blank_rows_are_skipped_and_the_next_blank_ends_the_block() {
        let rows = ["", "閃狗撞噴泉0600", "怪女鬧超商0630", "掃黑查酒妹0700", "毒駕阻扣車0730", "網紅狐狸毛0800", "", "", "XX九合一快報"];
        let mut taken = Vec::new();
        for slug in rows {
            match top_block_step(taken.len(), slug) {
                TopBlockStep::Skip => continue,
                TopBlockStep::Stop => break,
                TopBlockStep::Take => taken.push(slug),
            }
        }
        assert_eq!(taken, ["閃狗撞噴泉0600", "怪女鬧超商0630", "掃黑查酒妹0700", "毒駕阻扣車0730", "網紅狐狸毛0800"]);
    }
}

struct CachedStory {
    story: NsmlStory,
    /// RETR 回來的原文（v0.3.15）：存進搜尋用的本地紀錄，之後載入成卡片不用再連 iNEWS。
    raw: String,
}

/// 一個 queue 讀回來的樣子：rundown 順序的 (LIST 項目, 稿件)。
type Rundown = Vec<(ListEntry, NsmlStory)>;

/// 自動匯入在兩輪之間保留的東西：控制連線、節次清單、已經 RETR 過的稿件，以及
/// 換節／補掃要記得的上一輪狀態。
#[derive(Default)]
pub struct InewsState {
    session: Option<FtpSession>,
    session_key: String,
    shows: Option<(Instant, Vec<String>)>,
    /// key = LIST 上的完整名稱（含 ID，ID 第三段是修改時間戳）：稿件一改就換 key，
    /// 自然會重新 RETR。
    cache: HashMap<String, CachedStory>,
    /// 上一輪自動同步選中的節次：換了就先對它做最後一次同步。
    last_show: Option<String>,
    /// 上一次延遲補掃時的「目前節次」（見 `inews::sweep_target`）。
    swept_for: Option<String>,
    /// story key → 上一次看到的版本，決策紀錄算「新增／變動」用。
    versions: HashMap<String, String>,
    /// 上次存檔的節次狀態：剛打開自動刷新那一輪拿來接續（見 `SavedSession`）。
    resume: Option<SavedSession>,
    /// rundown 異常保護（v0.3.11）：寫入確認、異常冷卻、可疑稿。
    guard: Guard,
    /// 上面這些狀態屬於哪一個播出日。
    guard_day: String,
    /// 這個播出日程式跟過（當過目前節次）的節次：定時複查只看這些。
    followed: HashSet<String>,
    /// 上一輪等確認的稿（story key → slug）：這一輪不再是已播出就記一筆 `[確認略過]`。
    confirm_prev: HashMap<String, String>,
    /// `confirm_prev` 屬於哪一節。
    confirm_show: String,
    /// 目前節次上一輪的樣子，異常時存檔用。
    last_obs: Vec<RowObs>,
    recheck: RecheckState,
}

/// 定時複查（v0.3.11）：每次讀兩輪（第一輪記下補掃後才出現的稿，第二輪還是已播出才寫），
/// 時間校正也因此有連續兩輪可以確認。
#[derive(Default)]
struct RecheckState {
    day: String,
    /// 上一次複查完成（或開始跟這個播出日）的時間（當天第幾秒）。
    last: Option<u32>,
    /// 0 = 等時間到；1 = 第一輪做完，下一輪做第二輪。
    pass: u8,
    shows: Vec<String>,
    candidates: HashSet<String>,
}

/// 複查第二輪的結果，前端拿來找「寫過但已經不是已播出」的稿（疑似誤寫）。
#[derive(Serialize, Clone, Debug, Default)]
pub struct RecheckMeta {
    pub shows: Vec<String>,
    /// 複查的節次與目前節次裡所有已播出的 story key。
    pub aired_keys: Vec<String>,
}

/// 存在檔案裡的自動刷新狀態。程式重開或關掉自動刷新時，記憶體裡的 `last_show` /
/// `swept_for` 會不見；換節瞬間程式沒在跑的話，上一節就沒有最後同步和補掃（v0.3.5）。
/// 只接續同一個播出日、而且是「上次在跟的那一節」或它的下一節：程式完全沒跟過的
/// 節次不回頭補。
#[derive(Serialize, serde::Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct SavedSession {
    /// 播出日 MMDD（`broadcast_days` 的 today）。
    pub day: String,
    pub last_show: Option<String>,
    pub swept_for: Option<String>,
}

/// 一則被選中的稿件，給前端做單則變動偵測。
#[derive(Serialize, Clone, Debug)]
pub struct StoryMeta {
    pub file_name: String,
    pub key: String,
    pub version: String,
    pub cume: String,
    pub floated: bool,
    /// 這則屬於哪一節（換節最後同步、補掃時會跟目前節次不同）。
    pub show: String,
    /// 這一輪不能自動寫入的原因（v0.3.11）：等確認、冷卻中、可疑。
    pub hold: Option<Hold>,
    /// 定時複查找到的（補掃後才出現的已播出稿）：寫在文件最下面，不寫分隔線。
    pub recheck: bool,
}

#[derive(Serialize, Clone, Debug)]
pub struct InewsMeta {
    pub show: String,
    pub show_label: String,
    pub queue_path: String,
    pub now: String,
    pub switched_early: bool,
    /// 目前節次的有效範圍與來源（`硬時間` / `fallback`）。
    pub range_start: String,
    pub range_end: String,
    pub start_source: String,
    pub end_show: Option<String>,
    pub end_source: Option<String>,
    /// 這一輪對剛換掉的舊節次做了最後一次同步。
    pub switched_from: Option<String>,
    /// 這一輪完成了這一節的延遲補掃：它的分隔線現在可以寫了。
    pub closed_show: Option<String>,
    /// 目前節次是停播節次（關鍵時刻、收播），不匯入。
    pub off_air: bool,
    /// 停播節次對到的節目名稱（slug 開頭對 `programs`）。
    pub program: Option<String>,
    /// 跟 `closed_show` 一起來：那一節的分隔線之後接著寫這條節目分隔線。
    pub program_divider: Option<String>,
    /// 標記式截止（預設關閉）："after_preview" | "last_prefixed" | "none"
    pub cutoff_rule: String,
    pub cutoff_title: Option<String>,
    pub list_count: usize,
    pub retrieved: usize,
    pub pending: usize,
    pub stories: Vec<StoryMeta>,
    /// 換節、補掃、跳過的節次，跟決策紀錄寫的是同一份。
    pub events: Vec<String>,
    pub warnings: Vec<String>,
    /// 剛打開自動刷新時接續了上次存檔的節次：前端拿來當「上次寫到哪一節」。
    pub resumed_show: Option<String>,
    /// 時間校正用（v0.3.9）：目前節次和還沒補掃完的上一節裡，每一則已播出的列。
    /// 冷卻中不放目前節次的列，可疑稿也不放（v0.3.11）。
    pub time_rows: Vec<TimeRow>,
    /// rundown 時間異常，這一輪暫停寫入與時間校正（v0.3.11）。
    pub cooling: bool,
    /// 定時複查第二輪才有。
    pub recheck: Option<RecheckMeta>,
}

/// 這一輪讀到的一整節 rundown（剪輯工具的 `rundown\YYYYMMDD.json` 用）：不管有沒有
/// 播出、有沒有被樣式濾掉，只排除廣告列和分隔列。
pub struct ShowSnapshot {
    pub show: String,
    pub nominal: u32,
    pub stories: Vec<RundownStory>,
}

/// 搜尋用的本地紀錄裡的一節（v0.3.15）。
pub struct SearchShow {
    pub show: String,
    pub nominal: u32,
    pub stories: Vec<SearchRow>,
}

pub struct SearchRow {
    pub entry: SearchEntry,
    /// 稿件原文；這一輪快取裡沒有就是 None（不寫原文檔）。
    pub raw: Option<String>,
}

pub struct FetchResult {
    pub files: Vec<(String, String)>,
    pub meta: InewsMeta,
    pub rundown: Vec<ShowSnapshot>,
    /// 搜尋用的本地紀錄（跟剪輯工具的 `rundown` 分開存）。
    pub search: Vec<SearchShow>,
    /// 決策紀錄的這一行（不含時間戳）。
    pub log_line: String,
    /// 接在後面的決策紀錄（`[異常]`、`[可疑]`、`[複查]`…，v0.3.11）。
    pub extra_logs: Vec<String>,
    /// 判定異常那一輪的前後兩輪 rundown（JSON），存到紀錄資料夾。
    pub anomaly_dump: Option<String>,
}

/// 手動按「從 iNEWS 匯入」只讀目前節次；自動刷新才做換節最後同步與延遲補掃。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FetchMode {
    Manual,
    /// `session_start` = 剛打開自動刷新：記憶體裡的狀態作廢，改從存檔接續（`SavedSession`）；
    /// 程式沒跟過的節次不回頭補掃。
    Auto { session_start: bool },
}

/// 現在是台北時間的當天第幾秒。跟電腦的時區設定無關：rundown 的時間是台北時間。
pub fn taipei_seconds_of_day() -> u32 {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    ((now + 8 * 3600) % 86_400) as u32
}

/// 一則要輸出的稿：哪一節、rundown 裡的哪一列、算出來的累積時間。
struct Pick {
    show: String,
    index: usize,
    cume: Option<i64>,
    hold: Option<Hold>,
    recheck: bool,
}

impl Pick {
    fn new(show: &str, index: usize, cume: Option<i64>) -> Self {
        Pick { show: show.to_string(), index, cume, hold: None, recheck: false }
    }
}

fn clock(t: i64) -> String {
    inews::format_clock(t)
}

/// `07:55:00（硬時間）～ 08:55:00（0900 硬時間）`
fn describe_window(w: &inews::Window) -> String {
    let end = match &w.next {
        Some(n) => format!("{}（{} {}）", clock(w.end()), n.show, n.source.label()),
        None => format!("{}（沒有下一節）", clock(w.end())),
    };
    format!("{}（{}）～ {end}", clock(w.start()), w.slot.source.label())
}

/// 決策紀錄裡的一則：`09:01:50 合成第二則0900`。
/// 看現在時間的判斷：`select_rows` 加上直進暫緩（長度 0:00 的直進／LIVE 播出後，
/// 後面的稿等新的硬時間）。暫緩寫進 events，逾時恢復寫進 warnings。
fn select_now(
    cfg: &InewsConfig,
    rows: &[RundownRow],
    now: u32,
    w: &inews::Window,
    marker: inews::MarkerCutoff,
    (events, warnings): &mut (&mut Vec<String>, &mut Vec<String>),
) -> inews::Selection {
    let mut s = inews::select_rows(rows, Some(now), w, cfg.include_floated, marker);
    if cfg.hold_after_open_live {
        for h in inews::hold_after_open_live(rows, &mut s, now, w, cfg.open_live_max_minutes) {
            let live = format!("{} {}", clock(h.live_cume), rows[h.live].title);
            if h.expired {
                warnings.push(format!(
                    "{} 直進暫緩逾 {} 分鐘仍沒有新的硬時間，已恢復一般判定：{live}",
                    w.slot.show, cfg.open_live_max_minutes
                ));
            } else {
                events.push(format!("直進暫緩：{live} 起，後續 {} 則等待新硬時間", h.held.len()));
            }
        }
    }
    s
}

/// 停播節次的節目名稱：第一個 slug 開頭對得到 `programs` 的列。
fn program_of(cfg: &InewsConfig, rundown: &Rundown) -> Option<String> {
    rundown.iter().find_map(|(entry, story)| {
        let slugs = [story.title().trim(), entry.slug()];
        cfg.programs.iter().find(|(prefix, _)| {
            let prefix = prefix.trim();
            !prefix.is_empty() && slugs.iter().any(|s| s.starts_with(prefix))
        })
        .map(|(_, name)| name.clone())
    })
}

/// 判定異常那一輪的存檔（`logs\anomaly-*.json`）：前後兩輪每一列的 slug、累積時間、
/// 是否已播出，之後才查得到是搬稿還是別的原因。
fn anomaly_json(show: &str, now: u32, reason: &str, previous: &[RowObs], current: &[RowObs]) -> String {
    let rows = |v: &[RowObs]| -> Vec<serde_json::Value> {
        v.iter()
            .map(|o| serde_json::json!({ "key": o.key, "slug": o.slug, "cume": clock(o.cume), "aired": o.aired }))
            .collect()
    };
    serde_json::json!({
        "show": show,
        "now": clock(now as i64),
        "reason": reason,
        "previous": rows(previous),
        "current": rows(current),
    })
    .to_string()
}

fn describe_row(rows: &[RundownRow], sel: &inews::Selection, i: Option<usize>) -> String {
    match i {
        Some(i) => format!("{} {}", sel.cume[i].map(clock).unwrap_or_else(|| "--:--:--".into()), rows[i].title),
        None => "（無）".into(),
    }
}

impl InewsState {
    /// 拿到可用的連線；沒有就連線並登入。帳密或伺服器換了也重連。
    fn session(&mut self, cfg: &InewsConfig, creds: &Credentials) -> FtpResult<&mut FtpSession> {
        let key = format!("{}:{}|{}", cfg.server, cfg.port, creds.user);
        if self.session_key != key {
            self.drop_session();
        }
        if self.session.is_none() {
            let mut s = FtpSession::connect(&cfg.server, cfg.port)?;
            s.login(&creds.user, &creds.pass)?;
            self.session = Some(s);
            self.session_key = key;
        }
        Ok(self.session.as_mut().unwrap())
    }

    fn drop_session(&mut self) {
        if let Some(s) = self.session.take() {
            s.quit();
        }
        self.session_key.clear();
    }

    fn list_dir(&mut self, cfg: &InewsConfig, creds: &Credentials, path: &str) -> FtpResult<Vec<ListEntry>> {
        let s = self.session(cfg, creds)?;
        match s.cwd(path) {
            Ok(()) => s.list(),
            // 空的 queue 有時候連 CWD 都回 550。
            Err(FtpError::Reply { reply, .. }) if reply.code == 550 => Ok(Vec::new()),
            Err(e) => Err(e),
        }
    }

    fn show_names(&mut self, cfg: &InewsConfig, creds: &Credentials) -> FtpResult<Vec<String>> {
        if let Some((at, names)) = &self.shows {
            if at.elapsed() < SHOW_LIST_TTL && !names.is_empty() {
                return Ok(names.clone());
            }
        }
        let root = cfg.root.clone();
        let names: Vec<String> = self
            .list_dir(cfg, creds, &root)?
            .into_iter()
            .filter(|e| e.kind == ListKind::Folder && inews::show_start(&e.name).is_some())
            .map(|e| e.name)
            .collect();
        self.shows = Some((Instant::now(), names.clone()));
        Ok(names)
    }

    /// 讀一個 queue：一次 LIST，只 RETR 快取裡沒有的（新的或改過的）。
    fn read_queue(&mut self, cfg: &InewsConfig, creds: &Credentials, path: &str, retrieved: &mut usize) -> FtpResult<Rundown> {
        let entries: Vec<ListEntry> =
            self.list_dir(cfg, creds, path)?.into_iter().filter(|e| e.kind == ListKind::Story).collect();
        let mut out = Vec::with_capacity(entries.len());
        for entry in entries {
            let story = match self.cache.get(&entry.name) {
                Some(c) => c.story.clone(),
                None => {
                    let text = self.session(cfg, creds)?.retr(&entry.name)?;
                    *retrieved += 1;
                    let story = inews::parse_nsml(&text);
                    self.cache.insert(entry.name.clone(), CachedStory { story: story.clone(), raw: text });
                    story
                }
            };
            out.push((entry, story));
        }
        Ok(out)
    }

    /// 搜尋結果「載入成卡片」用（v0.3.15）：讀一節的 LIST，找到同一則稿（先比 story key，
    /// 沒有 key 再比 slug），只 RETR 這一則。找不到（稿子被抽掉了）回 None。連線斷了重連
    /// 再試一次，跟自動刷新一樣。
    pub fn fetch_story(
        &mut self,
        cfg: &InewsConfig,
        creds: &Credentials,
        show: &str,
        key: &str,
        slug: &str,
    ) -> Result<Option<(ListEntry, NsmlStory)>, String> {
        match self.fetch_story_once(cfg, creds, show, key, slug) {
            Ok(r) => Ok(r),
            // 跟 `fetch` 一樣：閒置被踢（421）、登入狀態失效（530）、連線層錯誤都重新登入再試。
            Err(FtpError::Io(_)) | Err(FtpError::Protocol(_)) if self.session.is_some() => {
                self.drop_session();
                self.fetch_story_once(cfg, creds, show, key, slug).map_err(|e| e.to_string())
            }
            Err(FtpError::Reply { ref reply, .. }) if self.session.is_some() && matches!(reply.code, 421 | 530) => {
                self.drop_session();
                self.fetch_story_once(cfg, creds, show, key, slug).map_err(|e| e.to_string())
            }
            Err(e) => Err(e.to_string()),
        }
    }

    /// 預列清單（v0.3.23）：讀一個 queue 最上面那一段，回傳 (slug, 樣式) 和一行讀取過程
    /// （寫進決策紀錄）。最多看 30 列。連線斷了重連再試一次，跟 `fetch_story` 一樣。
    ///
    /// 「那一段」＝最上面的空白列先跳過，從第一則有 slug 的稿開始，到下一個空白列為止
    /// （v0.3.26）：APPR 第一列固定留空，11/14 照「遇到空白列就停」一開始就停下，讀到 0 則。
    pub fn read_top_block(&mut self, cfg: &InewsConfig, creds: &Credentials, path: &str) -> Result<(Vec<(String, String)>, String), String> {
        match self.read_top_block_once(cfg, creds, path) {
            Ok(r) => Ok(r),
            Err(FtpError::Io(_)) | Err(FtpError::Protocol(_)) if self.session.is_some() => {
                self.drop_session();
                self.read_top_block_once(cfg, creds, path).map_err(|e| e.to_string())
            }
            Err(FtpError::Reply { ref reply, .. }) if self.session.is_some() && matches!(reply.code, 421 | 530) => {
                self.drop_session();
                self.read_top_block_once(cfg, creds, path).map_err(|e| e.to_string())
            }
            Err(e) => Err(e.to_string()),
        }
    }

    fn read_top_block_once(&mut self, cfg: &InewsConfig, creds: &Credentials, path: &str) -> FtpResult<(Vec<(String, String)>, String)> {
        let entries: Vec<ListEntry> =
            self.list_dir(cfg, creds, path)?.into_iter().filter(|e| e.kind == ListKind::Story).take(30).collect();
        let mut out = Vec::new();
        let mut trace = Vec::new();
        for (n, entry) in entries.into_iter().enumerate() {
            let story = match self.cache.get(&entry.name) {
                Some(c) => c.story.clone(),
                None => {
                    let text = self.session(cfg, creds)?.retr(&entry.name)?;
                    let story = inews::parse_nsml(&text);
                    self.cache.insert(entry.name.clone(), CachedStory { story: story.clone(), raw: text });
                    story
                }
            };
            let slug = match story.title().trim() {
                "" => entry.slug().trim().to_string(),
                t => t.to_string(),
            };
            match top_block_step(out.len(), &slug) {
                TopBlockStep::Skip => trace.push(format!("第{}列空白（跳過）", n + 1)),
                TopBlockStep::Stop => {
                    trace.push(format!("第{}列空白（停）", n + 1));
                    break;
                }
                TopBlockStep::Take => {
                    let style = story.raw("v-style").trim().to_string();
                    trace.push(format!("第{}列 {slug} {style}", n + 1));
                    out.push((slug, style));
                }
            }
        }
        Ok((out, trace.join("、")))
    }

    fn fetch_story_once(
        &mut self,
        cfg: &InewsConfig,
        creds: &Credentials,
        show: &str,
        key: &str,
        slug: &str,
    ) -> FtpResult<Option<(ListEntry, NsmlStory)>> {
        let path = format!("{}.{}.{}", cfg.root, show, cfg.queue);
        let entries: Vec<ListEntry> =
            self.list_dir(cfg, creds, &path)?.into_iter().filter(|e| e.kind == ListKind::Story).collect();
        let found = entries
            .iter()
            .find(|e| !key.is_empty() && e.story_key() == key)
            .or_else(|| entries.iter().find(|e| e.slug().trim() == slug.trim()));
        let Some(entry) = found.cloned() else { return Ok(None) };
        let story = match self.cache.get(&entry.name) {
            Some(c) => c.story.clone(),
            None => {
                let text = self.session(cfg, creds)?.retr(&entry.name)?;
                let story = inews::parse_nsml(&text);
                self.cache.insert(entry.name.clone(), CachedStory { story: story.clone(), raw: text });
                story
            }
        };
        Ok(Some((entry, story)))
    }

    fn fetch_once(&mut self, cfg: &InewsConfig, creds: &Credentials, now: u32, mode: FetchMode) -> FtpResult<FetchResult> {
        let names = self.show_names(cfg, creds)?;
        let queue_of = |show: &str| format!("{}.{}.{}", cfg.root, show, cfg.queue);
        let mut retrieved = 0;

        // 選節次要看好幾節的 rundown（有沒有稿、第一個硬時間），讀過的這一輪都留著用。
        let mut rundowns: HashMap<String, Rundown> = HashMap::new();
        let mut probe_err: Option<FtpError> = None;
        let plan = {
            let mut probe = |show: &str| -> inews::ShowProbe {
                if probe_err.is_some() {
                    return inews::ShowProbe::default();
                }
                if !rundowns.contains_key(show) {
                    match self.read_queue(cfg, creds, &queue_of(show), &mut retrieved) {
                        Ok(r) => {
                            rundowns.insert(show.to_string(), r);
                        }
                        Err(e) => {
                            probe_err = Some(e);
                            return inews::ShowProbe::default();
                        }
                    }
                }
                let rows: Vec<RundownRow> = rundowns[show].iter().map(|(e, s)| RundownRow::from_story(s, e.floated)).collect();
                inews::ShowProbe::of(&rows)
            };
            inews::plan_slots(&names, now, &cfg.slot_spans, cfg.lead_minutes, cfg.off_air_on_empty, &mut probe)
        };
        if let Some(e) = probe_err {
            return Err(e);
        }
        let plan = plan.ok_or_else(|| FtpError::Protocol(format!("{} 底下找不到 4 位數的節次資料夾", cfg.root)))?;

        let rundown_of = |state: &mut Self, rundowns: &mut HashMap<String, Rundown>, show: &str, retrieved: &mut usize| -> FtpResult<()> {
            if !rundowns.contains_key(show) {
                let r = state.read_queue(cfg, creds, &queue_of(show), retrieved)?;
                rundowns.insert(show.to_string(), r);
            }
            Ok(())
        };
        let cur = plan.current.clone();
        rundown_of(self, &mut rundowns, &cur.slot.show, &mut retrieved)?;
        let marker = cfg.cutoff_by_marker.then_some((cfg.cutoff_preview_keyword.as_str(), cfg.cutoff_page_prefix.as_str()));
        let rows_of = |rundowns: &HashMap<String, Rundown>, show: &str| -> Vec<RundownRow> {
            rundowns[show].iter().map(|(e, s)| RundownRow::from_story(s, e.floated)).collect()
        };

        let mut events: Vec<String> = Vec::new();
        let mut warnings = Vec::new();
        // 停播節次（關鍵時刻、收播）不匯入：裡面只有沒樣式的 slug 列。
        let program = cur.slot.off_air.then(|| program_of(cfg, &rundowns[&cur.slot.show])).flatten();
        let cur_rows = if cur.slot.off_air { Vec::new() } else { rows_of(&rundowns, &cur.slot.show) };
        let sel = select_now(cfg, &cur_rows, now, &cur, marker, &mut (&mut events, &mut warnings));
        if cur.slot.off_air {
            events.push(format!(
                "{} 停播節次{}，不匯入",
                cur.slot.show,
                program.as_deref().map(|p| format!("（{p}）")).unwrap_or_default()
            ));
        }

        for (show, reason) in &plan.skipped {
            events.push(match reason {
                inews::SkipReason::Spanned(by) => format!("跳過 {show}（併入 {by}）"),
                inews::SkipReason::Empty => format!("跳過 {show}（沒有帶樣式的稿）"),
                inews::SkipReason::OffAir(by) => format!("跳過 {show}（併入停播的 {by}）"),
            });
        }

        // 舊節次（換節最後同步或延遲補掃）要輸出的稿排在前面，跟寫進文件的順序一致。
        let mut extra: Vec<Pick> = Vec::new();
        let mut switched_from = None;
        let mut closed_show = None;
        let mut program_divider = None;
        let mut resumed_show = None;
        let today = broadcast_days(taipei_now(), &cfg.broadcast_day_start).0;
        if let FetchMode::Auto { session_start } = mode {
            if self.guard_day != today {
                self.guard.new_day();
                self.followed.clear();
                self.confirm_prev.clear();
                self.last_obs.clear();
                self.guard_day = today.clone();
            }
            if session_start || self.recheck.day != today {
                // 剛開始跟（或換了播出日）：複查從現在起算。
                self.recheck = RecheckState { day: today.clone(), last: Some(now), ..Default::default() };
            }
            if !cur.slot.off_air {
                self.followed.insert(cur.slot.show.clone());
            }
            if session_start {
                self.last_show = None;
                self.swept_for = None;
                if let Some(saved) = self.resume.clone() {
                    let prev = plan.previous.as_ref().map(|p| p.slot.show.as_str());
                    match saved.last_show.as_deref() {
                        Some(last) if last == cur.slot.show => {
                            events.push(format!("接續上次的節次 {last}"));
                            self.last_show = saved.last_show.clone();
                            self.swept_for = saved.swept_for.clone();
                            resumed_show = saved.last_show.clone();
                        }
                        Some(last) if Some(last) == prev => {
                            events.push(format!("接續上次：程式停下時在跟 {last}，補做 {last} 的收尾"));
                            self.last_show = saved.last_show.clone();
                            // 補掃要看到 swept_for 不是目前節次才會做。
                            self.swept_for = saved.swept_for.clone().or_else(|| saved.last_show.clone());
                            resumed_show = saved.last_show.clone();
                        }
                        Some(last) => events.push(format!(
                            "上次停在 {last}，不是 {} 或它的上一個有效節次，中間沒跟到的節次不補（要補請手動匯入）",
                            cur.slot.show
                        )),
                        None => {}
                    }
                }
            }
            // 剛開始自動刷新：不回頭補掃開啟前就結束的節次。
            if self.swept_for.is_none() {
                self.swept_for = Some(cur.slot.show.clone());
            }
            let sweep = inews::sweep_target(&plan, now, cfg.sweep_delay_minutes, self.swept_for.as_deref()).cloned();
            if let Some(last) = self.last_show.clone().filter(|l| *l != cur.slot.show) {
                events.push(format!("換節 {last} → {}", cur.slot.show));
                match plan.previous.as_ref().filter(|p| p.slot.show == last) {
                    // 停播節次沒有要同步的稿。
                    Some(prev) if prev.slot.off_air => {}
                    // 補掃同一節的話，補掃已經涵蓋最後同步，不必做兩次。
                    Some(prev) if sweep.as_ref().map(|w| &w.slot.show) != Some(&last) => {
                        rundown_of(self, &mut rundowns, &last, &mut retrieved)?;
                        let rows = rows_of(&rundowns, &last);
                        let s = select_now(cfg, &rows, now, prev, marker, &mut (&mut events, &mut warnings));
                        events.push(format!("{last} 最後同步：納入 {} 則（累積時間 < {}）", s.rows.len(), clock(prev.end())));
                        extra.extend(s.rows.iter().map(|&i| Pick::new(&last, i, s.cume[i])));
                        switched_from = Some(last);
                    }
                    Some(_) => {}
                    None => events.push(format!("{last} 不是 {} 的上一個有效節次，不做最後同步", cur.slot.show)),
                }
            }
            if let Some(w) = sweep {
                rundown_of(self, &mut rundowns, &w.slot.show, &mut retrieved)?;
                let rows = rows_of(&rundowns, &w.slot.show);
                let s = inews::select_rows(&rows, None, &w, cfg.include_floated, marker);
                events.push(format!(
                    "補掃 {}：範圍 {}，納入 {} 則，最後一則 {}",
                    w.slot.show,
                    describe_window(&w),
                    s.rows.len(),
                    describe_row(&rows, &s, s.last_included())
                ));
                extra.extend(s.rows.iter().map(|&i| Pick::new(&w.slot.show, i, s.cume[i])));
                closed_show = Some(w.slot.show.clone());
                if let Some(name) = &program {
                    program_divider = Some(
                        cfg.program_divider_format
                            .replace("{hh}", &inews::show_hour_label(&cur.slot.show))
                            .replace("{show}", &cur.slot.show)
                            .replace("{name}", name),
                    );
                }
                self.swept_for = Some(cur.slot.show.clone());
            }
            self.last_show = Some(cur.slot.show.clone());
        }

        // 定時複查（v0.3.11）：這個播出日程式跟過、已經補掃完的節次。
        let mut recheck_picks: Vec<Pick> = Vec::new();
        let mut recheck_sels: Vec<(inews::Window, inews::Selection)> = Vec::new();
        let mut recheck_next: Option<RecheckState> = None;
        let mut recheck_done = false;
        let mut extra_logs: Vec<String> = Vec::new();
        if let FetchMode::Auto { session_start: false } = mode {
            let interval = i64::from(cfg.recheck_interval_minutes) * 60;
            let due = cfg.recheck_interval_minutes > 0
                && (self.recheck.pass == 1
                    || self.recheck.last.is_some_and(|t| (now as i64 - t as i64).rem_euclid(86_400) >= interval));
            if due {
                let windows = self.recheck_windows(cfg, creds, &names, now, &mut rundowns, &mut retrieved, &plan, closed_show.as_deref())?;
                let second = self.recheck.pass == 1;
                let mut candidates = HashSet::new();
                let mut shows = Vec::new();
                for w in windows {
                    if second && !self.recheck.shows.contains(&w.slot.show) {
                        continue;
                    }
                    let rows = rows_of(&rundowns, &w.slot.show);
                    let s = inews::select_rows(&rows, None, &w, cfg.include_floated, marker);
                    for &i in &s.rows {
                        let key = rundowns[&w.slot.show][i].0.story_key();
                        let take = if second { self.recheck.candidates.contains(&key) } else { !self.versions.contains_key(&key) };
                        if take {
                            candidates.insert(key);
                            let mut p = Pick::new(&w.slot.show, i, s.cume[i]);
                            p.recheck = true;
                            p.hold = (!second).then_some(Hold::Confirm);
                            recheck_picks.push(p);
                        }
                    }
                    shows.push(w.slot.show.clone());
                    recheck_sels.push((w, s));
                }
                if second {
                    let slugs: Vec<String> =
                        recheck_picks.iter().map(|p| rundowns[&p.show][p.index].1.title().trim().to_string()).collect();
                    extra_logs.push(format!(
                        "[複查] {}：補掃後才出現的已播出稿 {} 則{}",
                        if shows.is_empty() { "（沒有要複查的節次）".to_string() } else { shows.join("、") },
                        slugs.len(),
                        if slugs.is_empty() { String::new() } else { format!("（{}），寫在文件最下面", slugs.join("、")) }
                    ));
                    recheck_done = true;
                    recheck_next = Some(RecheckState { day: today.clone(), last: Some(now), ..Default::default() });
                } else {
                    recheck_next = Some(RecheckState { day: today.clone(), last: self.recheck.last, pass: 1, shows, candidates });
                }
            }
        }

        // 這一輪要讀的 rundown 到這裡都要讀完：下面開始改異常保護的狀態，讀到一半斷線
        // 重試的話，同一分鐘會被算成兩輪（等確認的稿就直接放行了）。上一節通常已經在
        // `plan_slots` 讀過，這裡只是確保。
        if let (FetchMode::Auto { .. }, Some(prev)) = (mode, plan.previous.as_ref()) {
            if !prev.slot.off_air {
                rundown_of(self, &mut rundowns, &prev.slot.show, &mut retrieved)?;
            }
        }

        // rundown 異常保護（v0.3.11）：目前節次每一輪跟上一輪比。
        let gcfg = GuardConfig::from_inews(cfg);
        let mut holds: HashMap<String, Hold> = HashMap::new();
        let mut cooling = false;
        let mut anomaly_dump = None;
        if let (FetchMode::Auto { .. }, false) = (mode, cur.slot.off_air) {
            let aired: HashSet<usize> = sel.rows.iter().copied().collect();
            let obs: Vec<RowObs> = (0..cur_rows.len())
                .filter(|&i| sel.fates[i].is_some())
                .filter_map(|i| {
                    let c = sel.cume[i]?;
                    Some(RowObs {
                        key: rundowns[&cur.slot.show][i].0.story_key(),
                        slug: cur_rows[i].title.trim().to_string(),
                        cume: cur.slot.axis(c),
                        aired: aired.contains(&i),
                    })
                })
                .collect();
            let step = self.guard.step(&cur.slot.show, &obs, cur.slot.axis(now as i64), &gcfg);
            let show = &cur.slot.show;
            if let Some(why) = &step.anomaly {
                extra_logs.push(format!("[異常] {show} {why}，暫停寫入與時間校正"));
                anomaly_dump = Some(anomaly_json(show, now, why, &self.last_obs, &obs));
            }
            let obs_of: HashMap<&str, &RowObs> = obs.iter().map(|o| (o.key.as_str(), o)).collect();
            for k in &step.new_suspects {
                if let Some(o) = obs_of.get(k.as_str()) {
                    extra_logs.push(format!(
                        "[可疑] {show} {} {}：rundown 時間異常時排到已播出範圍之前，不寫入，等時間推進",
                        o.slug,
                        clock(o.cume)
                    ));
                }
            }
            if let Some(r) = &step.released {
                extra_logs.push(format!("[異常解除] {show} {r}"));
                if step.expired {
                    warnings.push(format!("{show} {r}"));
                }
            }
            // 換節那一輪不比：上一輪等確認的是上一節的稿，換節最後同步會處理。
            let same_show = self.confirm_show == *show;
            for (k, slug) in self.confirm_prev.iter().filter(|_| same_show) {
                if !obs_of.get(k.as_str()).is_some_and(|o| o.aired) {
                    extra_logs.push(format!("[確認略過] {slug}：只有一輪判定已播出，下一輪就不是了，沒有寫入"));
                }
            }
            self.confirm_show = show.clone();
            self.confirm_prev = step
                .holds
                .iter()
                .filter(|(_, h)| **h == Hold::Confirm)
                .filter_map(|(k, _)| obs_of.get(k.as_str()).map(|o| (k.clone(), o.slug.clone())))
                .collect();
            if step.cooling {
                warnings.push(format!(
                    "{show} rundown 時間異常，暫停寫入與時間校正，等連續 {} 輪穩定（最多 {} 分鐘）。",
                    cfg.anomaly_quiet_rounds.max(1),
                    cfg.anomaly_max_cooldown_minutes.max(1)
                ));
            }
            cooling = step.cooling;
            holds = step.holds;
            self.last_obs = obs;
        }
        // 換節最後同步、補掃、複查的稿：異常時被標成可疑的照樣擋。
        for p in extra.iter_mut().chain(recheck_picks.iter_mut()) {
            let key = rundowns[&p.show][p.index].0.story_key();
            let nominal = inews::show_start(&p.show).unwrap_or(0) as i64;
            if let Some(c) = p.cume {
                if self.guard.hold_suspect(&p.show, &key, inews::unwrap_near(c, nominal), gcfg.threshold) {
                    p.hold = Some(Hold::Suspect);
                }
            }
        }
        let suspects = self.guard.suspects();
        if !suspects.is_empty() {
            warnings.push(format!(
                "可疑稿 {} 則不寫入（rundown 時間異常時跑到已播出範圍之前，時間推進後才會寫；請人工確認）：{}",
                suspects.len(),
                suspects.iter().map(|s| format!("{}（{} {}）", s.slug, s.show, clock(s.cume))).collect::<Vec<_>>().join("、")
            ));
        }
        if let Some(r) = recheck_next {
            self.recheck = r;
        }

        // 時間校正的對象：目前節次已播出的列，加上還沒補掃完（或這一輪才補掃）的上一節。
        let time_row = |rundowns: &HashMap<String, Rundown>, w: &inews::Window, i: usize, cume: Option<i64>| {
            cume.map(|c| TimeRow {
                show: w.slot.show.clone(),
                slug: rundowns[&w.slot.show][i].1.title().trim().to_string(),
                time: clock(c),
                nominal: w.slot.nominal,
                start: w.start(),
                end: w.end(),
            })
        };
        let mut time_rows: Vec<TimeRow> = Vec::new();
        if let (FetchMode::Auto { .. }, Some(prev)) = (mode, plan.previous.as_ref()) {
            let open = self.swept_for.as_deref() != Some(cur.slot.show.as_str()) || closed_show.as_deref() == Some(prev.slot.show.as_str());
            if open && !prev.slot.off_air {
                rundown_of(self, &mut rundowns, &prev.slot.show, &mut retrieved)?;
                let rows = rows_of(&rundowns, &prev.slot.show);
                let s = inews::select_rows(&rows, None, prev, cfg.include_floated, marker);
                time_rows.extend(
                    s.rows
                        .iter()
                        .filter(|&&i| !self.guard.is_suspect(&rundowns[&prev.slot.show][i].0.story_key()))
                        .filter_map(|&i| time_row(&rundowns, prev, i, s.cume[i])),
                );
            }
        }
        if !cur.slot.off_air && !cooling {
            time_rows.extend(
                sel.rows
                    .iter()
                    .filter(|&&i| holds.get(&rundowns[&cur.slot.show][i].0.story_key()) != Some(&Hold::Suspect))
                    .filter_map(|&i| time_row(&rundowns, &cur, i, sel.cume[i])),
            );
        }
        for (w, s) in &recheck_sels {
            time_rows.extend(
                s.rows
                    .iter()
                    .filter(|&&i| !self.guard.is_suspect(&rundowns[&w.slot.show][i].0.story_key()))
                    .filter_map(|&i| time_row(&rundowns, w, i, s.cume[i])),
            );
        }
        let recheck_meta = recheck_done.then(|| {
            let mut aired_keys: Vec<String> = recheck_sels
                .iter()
                .flat_map(|(w, s)| s.rows.iter().map(|&i| rundowns[&w.slot.show][i].0.story_key()).collect::<Vec<_>>())
                .collect();
            let mut shows: Vec<String> = recheck_sels.iter().map(|(w, _)| w.slot.show.clone()).collect();
            if !cur.slot.off_air {
                aired_keys.extend(sel.rows.iter().map(|&i| rundowns[&cur.slot.show][i].0.story_key()));
                shows.push(cur.slot.show.clone());
            }
            RecheckMeta { shows, aired_keys }
        });

        // 目前節次與上一個有效節次整份存下來，停播節次不算。`plan_slots` 為了判斷有效節次
        // 每一輪都會讀這兩節（包括剛打開自動刷新那一輪），所以不用另外讀、也不看補掃狀態。
        let snapshot_of = |w: &inews::Window| -> Option<ShowSnapshot> {
            let r = rundowns.get(&w.slot.show).filter(|_| !w.slot.off_air)?;
            let stories = r.iter().filter_map(|(e, s)| rundown::story_record(e, s)).collect();
            Some(ShowSnapshot { show: w.slot.show.clone(), nominal: w.slot.nominal, stories })
        };
        // 搜尋用的本地紀錄（v0.3.15）：同樣的節次，另外記播出時間和稿件原文。跟剪輯工具的
        // 檔案分開：播出時間會跟著編輯調整一直變，剪輯工具要自己判斷，不能拿這裡的。
        let search_of = |w: &inews::Window| -> Option<SearchShow> {
            let r = rundowns.get(&w.slot.show).filter(|_| !w.slot.off_air)?;
            let rows: Vec<RundownRow> = r.iter().map(|(e, s)| RundownRow::from_story(s, e.floated)).collect();
            let cume = inews::compute_cume(&rows);
            let stories = r
                .iter()
                .zip(cume)
                .filter_map(|((e, s), c)| {
                    let rec = rundown::story_record(e, s)?;
                    Some(SearchRow {
                        entry: SearchEntry {
                            slug: rec.slug,
                            id: e.story_key(),
                            version: e.version().to_string(),
                            style: rec.style,
                            air_time: c.map(clock).unwrap_or_default(),
                            updated: rec.updated,
                        },
                        raw: self.cache.get(&e.name).map(|c| c.raw.clone()),
                    })
                })
                .collect();
            Some(SearchShow { show: w.slot.show.clone(), nominal: w.slot.nominal, stories })
        };
        // 上一個有效節次會繞回前一天（一天的第一節的上一節是昨晚的最後一節，裡面是昨天的
        // 殘留稿），所以只收同一個播出日、排在目前節次前面的。
        let day_start = cfg.broadcast_day_start.as_str();
        let same_day = |w: &&inews::Window| day_order(day_start, w.slot.nominal) < day_order(day_start, cur.slot.nominal);
        let snapshots: Vec<ShowSnapshot> =
            plan.previous.iter().filter(same_day).chain(std::iter::once(&cur)).filter_map(snapshot_of).collect();
        let search: Vec<SearchShow> =
            plan.previous.iter().filter(same_day).chain(std::iter::once(&cur)).filter_map(search_of).collect();

        // 快取只留這一輪還看得到的稿件，換節後舊的就放掉。
        let kept: std::collections::HashSet<&String> = rundowns.values().flatten().map(|(e, _)| &e.name).collect();
        self.cache.retain(|name, _| kept.contains(name));

        if cfg.cutoff_by_marker && sel.rule == inews::CutoffRule::None {
            warnings.push(format!(
                "已開啟標記式截止，但找不到截止點（標題含「{}」之後序號以「{}」開頭的列），只用時間判斷。",
                cfg.cutoff_preview_keyword, cfg.cutoff_page_prefix
            ));
        }
        if !cur_rows.is_empty() && cur_rows.iter().all(|r| r.hard.is_none()) {
            warnings.push("這一節的 rundown 沒有任何硬時間（＠），算不出累積時間，暫時不匯入任何稿件。".into());
        }

        let picks: Vec<Pick> = extra
            .into_iter()
            .chain(recheck_picks)
            .chain(sel.rows.iter().map(|&i| {
                let mut p = Pick::new(&cur.slot.show, i, sel.cume[i]);
                p.hold = holds.get(&rundowns[&cur.slot.show][i].0.story_key()).copied();
                p
            }))
            .collect();
        let titles: Vec<String> = picks.iter().map(|p| rundowns[&p.show][p.index].1.title().to_string()).collect();
        let file_names = inews::unique_file_names(&titles);
        let mut files = Vec::with_capacity(picks.len());
        let mut metas = Vec::with_capacity(picks.len());
        let (mut added, mut changed) = (0, 0);
        for (pick, file_name) in picks.iter().zip(file_names) {
            let (entry, story) = &rundowns[&pick.show][pick.index];
            let cume = pick.cume.map(inews::format_clock).unwrap_or_default();
            let fill = (cfg.fill_computed_time && !cume.is_empty()).then_some(cume.as_str());
            files.push((file_name.clone(), inews::story_to_text(story, fill)));
            let key = entry.story_key();
            match self.versions.insert(key.clone(), entry.version().to_string()) {
                None => added += 1,
                Some(v) if v != entry.version() => changed += 1,
                _ => {}
            }
            metas.push(StoryMeta {
                file_name,
                key,
                version: entry.version().to_string(),
                cume,
                floated: entry.floated,
                show: pick.show.clone(),
                hold: pick.hold,
                recheck: pick.recheck,
            });
        }

        let log_line = format!(
            "節次 {}{}｜範圍 {}｜新增 {added}・變動 {changed}・納入 {}・排除 {}｜最後納入 {}｜第一則排除 {}{}",
            cur.slot.show,
            if plan.switched_early { "（提前換節）" } else { "" },
            describe_window(&cur),
            picks.len(),
            sel.excluded,
            describe_row(&cur_rows, &sel, sel.last_included()),
            describe_row(&cur_rows, &sel, sel.first_excluded()),
            if events.is_empty() { String::new() } else { format!("｜{}", events.join("；")) },
        );

        Ok(FetchResult {
            files,
            log_line,
            rundown: snapshots,
            search,
            meta: InewsMeta {
                show_label: inews::show_hour_label(&cur.slot.show),
                show: cur.slot.show.clone(),
                queue_path: queue_of(&cur.slot.show),
                now: inews::format_clock(now as i64),
                switched_early: plan.switched_early,
                range_start: clock(cur.start()),
                range_end: clock(cur.end()),
                start_source: cur.slot.source.label().into(),
                end_show: cur.next.as_ref().map(|n| n.show.clone()),
                end_source: cur.next.as_ref().map(|n| n.source.label().to_string()),
                switched_from,
                closed_show,
                off_air: cur.slot.off_air,
                program,
                program_divider,
                cutoff_rule: match sel.rule {
                    inews::CutoffRule::AfterPreview => "after_preview",
                    inews::CutoffRule::LastPrefixed => "last_prefixed",
                    inews::CutoffRule::None => "none",
                }
                .into(),
                cutoff_title: sel.cutoff.map(|i| cur_rows[i].title.clone()),
                list_count: cur_rows.len(),
                retrieved,
                pending: sel.pending,
                stories: metas,
                events,
                warnings,
                resumed_show,
                time_rows,
                cooling,
                recheck: recheck_meta,
            },
            extra_logs,
            anomaly_dump,
        })
    }

    /// 定時複查要看的節次：目前節次之前、同一個播出日、程式跟過、已經補掃完的有效節次
    /// （還沒補掃的上一節本來每一輪都在對）。停播節次不算。
    #[allow(clippy::too_many_arguments)]
    fn recheck_windows(
        &mut self,
        cfg: &InewsConfig,
        creds: &Credentials,
        names: &[String],
        now: u32,
        rundowns: &mut HashMap<String, Rundown>,
        retrieved: &mut usize,
        plan: &inews::Plan,
        closed_this_round: Option<&str>,
    ) -> FtpResult<Vec<inews::Window>> {
        let queue_of = |show: &str| format!("{}.{}.{}", cfg.root, show, cfg.queue);
        let mut err: Option<FtpError> = None;
        let windows = {
            let mut probe = |show: &str| -> inews::ShowProbe {
                if err.is_some() {
                    return inews::ShowProbe::default();
                }
                if !rundowns.contains_key(show) {
                    match self.read_queue(cfg, creds, &queue_of(show), retrieved) {
                        Ok(r) => {
                            rundowns.insert(show.to_string(), r);
                        }
                        Err(e) => {
                            err = Some(e);
                            return inews::ShowProbe::default();
                        }
                    }
                }
                let rows: Vec<RundownRow> = rundowns[show].iter().map(|(e, s)| RundownRow::from_story(s, e.floated)).collect();
                inews::ShowProbe::of(&rows)
            };
            inews::earlier_windows(names, now, &cfg.slot_spans, cfg.lead_minutes, cfg.off_air_on_empty, &mut probe, 24)
        };
        if let Some(e) = err {
            return Err(e);
        }
        let cur = &plan.current.slot;
        let day_start = cfg.broadcast_day_start.as_str();
        // 上一節還沒補掃完（這一輪也沒補掃）就不在複查範圍。
        let prev_open = plan.previous.as_ref().map(|p| p.slot.show.as_str()).filter(|p| {
            self.swept_for.as_deref() != Some(cur.show.as_str()) && closed_this_round != Some(*p)
        });
        let picked: Vec<inews::Window> = windows
            .into_iter()
            .filter(|w| !w.slot.off_air)
            .filter(|w| day_order(day_start, w.slot.nominal) < day_order(day_start, cur.nominal))
            .filter(|w| self.followed.contains(&w.slot.show))
            .filter(|w| Some(w.slot.show.as_str()) != prev_open)
            .collect();
        for w in &picked {
            if !rundowns.contains_key(&w.slot.show) {
                let r = self.read_queue(cfg, creds, &queue_of(&w.slot.show), retrieved)?;
                rundowns.insert(w.slot.show.clone(), r);
            }
        }
        Ok(picked)
    }

    /// 剛打開自動刷新那一輪要接續的存檔狀態（同一個播出日才給）。
    pub fn set_resume(&mut self, saved: Option<SavedSession>) {
        self.resume = saved;
    }

    /// 要存檔的狀態。
    pub fn saved(&self, day: &str) -> SavedSession {
        SavedSession { day: day.to_string(), last_show: self.last_show.clone(), swept_for: self.swept_for.clone() }
    }

    /// 一輪匯入。連線斷了（伺服器閒置踢人、網路閃斷）就重連再試一次。
    pub fn fetch(&mut self, cfg: &InewsConfig, creds: &Credentials, now: u32, mode: FetchMode) -> Result<FetchResult, String> {
        match self.fetch_once(cfg, creds, now, mode) {
            Ok(r) => Ok(r),
            // 421 = 伺服器閒置踢人；530 = 舊連線的登入狀態失效。都重新登入再試。
            Err(FtpError::Io(_)) | Err(FtpError::Protocol(_)) if self.session.is_some() => {
                self.drop_session();
                self.fetch_once(cfg, creds, now, mode).map_err(|e| e.to_string())
            }
            Err(FtpError::Reply { ref reply, .. }) if self.session.is_some() && matches!(reply.code, 421 | 530) => {
                self.drop_session();
                self.fetch_once(cfg, creds, now, mode).map_err(|e| e.to_string())
            }
            Err(e) => {
                // 登入失敗之類的：下一輪重新連線，不要卡在壞掉的連線上。
                self.drop_session();
                Err(e.to_string())
            }
        }
    }
}

// --- 播出日 -----------------------------------------------------------------------

/// 播出日的 (今天 MMDD, 前一天 MMDD)。`day_start`（`02:00`）之前還算前一天：公司
/// 02:00 回收 rundown，2400、0100 那節的稿屬於前一天的段落。格式錯就當 00:00。
pub fn broadcast_days(now: chrono::NaiveDateTime, day_start: &str) -> (String, String) {
    let offset = chrono::NaiveTime::parse_from_str(day_start.trim(), "%H:%M")
        .map(|t| t.signed_duration_since(chrono::NaiveTime::MIN))
        .unwrap_or_default();
    let today = (now - offset).date();
    let previous = today.pred_opt().unwrap_or(today);
    (today.format("%m%d").to_string(), previous.format("%m%d").to_string())
}

pub fn taipei_now() -> chrono::NaiveDateTime {
    chrono::Utc::now().with_timezone(&chrono::FixedOffset::east_opt(8 * 3600).unwrap()).naive_local()
}

// --- 決策紀錄 ---------------------------------------------------------------------

/// 紀錄保留天數。
pub const LOG_KEEP_DAYS: i64 = 14;

/// 台北時間的 (yyyymmdd, `yyyy-mm-dd HH:MM:SS`)。
fn taipei_stamp() -> (String, String) {
    let now = taipei_now();
    (now.format("%Y%m%d").to_string(), now.format("%Y-%m-%d %H:%M:%S").to_string())
}

/// 附加一行到 `dir/inews-YYYYMMDD.log`，順便刪掉超過 14 天的紀錄。寫不進去不影響匯入。
pub fn append_log(dir: &std::path::Path, line: &str) {
    let (day, stamp) = taipei_stamp();
    if std::fs::create_dir_all(dir).is_err() {
        return;
    }
    let path = dir.join(format!("inews-{day}.log"));
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(f, "{stamp} {line}");
    }
    prune_logs(dir, &day);
}

/// 刪掉檔名日期比今天早 14 天以上的 `inews-YYYYMMDD.log`。
fn prune_logs(dir: &std::path::Path, today: &str) {
    let Ok(today) = chrono::NaiveDate::parse_from_str(today, "%Y%m%d") else { return };
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        // 決策紀錄 `inews-YYYYMMDD.log`，以及異常存檔 `anomaly-YYYYMMDD-HHMMSS.json`（v0.3.11）。
        let date = name
            .strip_prefix("inews-")
            .and_then(|n| n.strip_suffix(".log"))
            .or_else(|| name.strip_prefix("anomaly-").and_then(|n| n.strip_suffix(".json")).and_then(|n| n.get(..8)));
        let Some(date) = date else { continue };
        if let Ok(d) = chrono::NaiveDate::parse_from_str(date, "%Y%m%d") {
            if (today - d).num_days() >= LOG_KEEP_DAYS {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
}

// --- 剪輯工具用的 rundown 資料 -----------------------------------------------------------

/// `rundown\YYYYMMDD.json` 保留天數。
pub const RUNDOWN_KEEP_DAYS: i64 = 30;

/// 播出日分界（`02:00`）；格式錯就當 00:00，跟 `broadcast_days` 一樣。
fn day_offset(day_start: &str) -> chrono::Duration {
    chrono::NaiveTime::parse_from_str(day_start.trim(), "%H:%M")
        .map(|t| t.signed_duration_since(chrono::NaiveTime::MIN))
        .unwrap_or_default()
}

/// 現在屬於哪一個播出日（02:00 分界：01:30 還算前一天）。
pub fn broadcast_date(now: chrono::NaiveDateTime, day_start: &str) -> chrono::NaiveDate {
    (now - day_offset(day_start)).date()
}

/// 節次資料夾名稱（`0900`）在播出日裡的先後；認不得的排最後。
pub fn show_day_order(day_start: &str, show: &str) -> i64 {
    inews::show_start(show).map_or(i64::MAX, |n| day_order(day_start, n))
}

/// 節次在播出日裡的先後（秒）：02:00 分界下 0100 排在 2300 後面。
fn day_order(day_start: &str, nominal: u32) -> i64 {
    (i64::from(nominal) - day_offset(day_start).num_seconds()).rem_euclid(86_400)
}

/// 一節屬於哪個播出日。節次的開始時間取離 `now` 最近的那一次（08:55 看 2400 是明天
/// 00:00，02:05 看 0100 是今天 01:00），再扣掉播出日分界：0100 的稿算前一天。
pub fn show_broadcast_date(now: chrono::NaiveDateTime, day_start: &str, nominal: u32) -> chrono::NaiveDate {
    let offset = day_offset(day_start);
    let start = chrono::Duration::seconds(i64::from(nominal));
    let air = (-1..=1)
        .map(|d| now.date().and_time(chrono::NaiveTime::MIN) + chrono::Duration::days(d) + start)
        .min_by_key(|t| (*t - now).num_seconds().abs())
        .unwrap_or(now);
    (air - offset).date()
}

/// 把這一輪的每一節併進各自播出日的 `dir/YYYYMMDD.json`（暫存檔寫完再改名），順便刪掉
/// 超過 30 天的。呼叫端只把錯誤寫成警告，不影響匯入。
pub fn write_rundown(
    dir: &std::path::Path,
    now: chrono::NaiveDateTime,
    day_start: &str,
    snapshots: Vec<ShowSnapshot>,
) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("建立資料夾失敗：{e}"))?;
    let mut by_day: std::collections::BTreeMap<chrono::NaiveDate, Vec<ShowSnapshot>> = Default::default();
    for s in snapshots {
        by_day.entry(show_broadcast_date(now, day_start, s.nominal)).or_default().push(s);
    }
    for (date, shows) in by_day {
        let path = dir.join(format!("{}.json", date.format("%Y%m%d")));
        let mut day = match std::fs::read_to_string(&path) {
            Ok(text) => RundownDay::from_json(&text).unwrap_or_else(|_| {
                // 壞掉的檔案留成 .bad 再重寫，不要連同新資料一起蓋掉。
                let _ = std::fs::rename(&path, path.with_extension("json.bad"));
                RundownDay::default()
            }),
            Err(_) => RundownDay::default(),
        };
        day.date = date.format("%Y-%m-%d").to_string();
        for s in shows {
            day.merge(&s.show, s.stories);
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, day.to_json()).map_err(|e| format!("寫入 {} 失敗：{e}", tmp.display()))?;
        std::fs::rename(&tmp, &path).map_err(|e| format!("改名 {} 失敗：{e}", path.display()))?;
    }
    prune_rundown(dir, now.date());
    Ok(())
}

/// 刪掉檔名日期比今天早 30 天以上的 `YYYYMMDD.json`。
/// 搜尋用的本地紀錄（v0.3.15）：`dir\YYYYMMDD\index.json` 整份重寫（先寫暫存檔再改名），
/// 稿件原文 `dir\YYYYMMDD\stories\<ID>_<版本>.nsml` 每個版本只寫一次。
pub fn write_search(dir: &std::path::Path, now: chrono::NaiveDateTime, day_start: &str, shows: Vec<SearchShow>) -> Result<(), String> {
    let mut by_day: std::collections::BTreeMap<chrono::NaiveDate, Vec<SearchShow>> = Default::default();
    for s in shows {
        by_day.entry(show_broadcast_date(now, day_start, s.nominal)).or_default().push(s);
    }
    for (date, shows) in by_day {
        let day_dir = dir.join(date.format("%Y%m%d").to_string());
        let stories_dir = day_dir.join("stories");
        std::fs::create_dir_all(&stories_dir).map_err(|e| format!("建立 {} 失敗：{e}", stories_dir.display()))?;
        let index = day_dir.join("index.json");
        let mut day = std::fs::read_to_string(&index)
            .ok()
            .and_then(|t| SearchDay::from_json(&t).ok())
            .unwrap_or_default();
        day.date = date.format("%Y-%m-%d").to_string();
        for s in shows {
            let mut entries = Vec::with_capacity(s.stories.len());
            for row in s.stories {
                if let (Some(raw), Some(name)) = (&row.raw, news_script_core::search::story_file_name(&row.entry.id, &row.entry.version)) {
                    let path = stories_dir.join(name);
                    if !path.exists() {
                        std::fs::write(&path, raw).map_err(|e| format!("寫入 {} 失敗：{e}", path.display()))?;
                    }
                }
                entries.push(row.entry);
            }
            day.merge(&s.show, entries);
        }
        let tmp = day_dir.join("index.json.tmp");
        std::fs::write(&tmp, day.to_json()).map_err(|e| format!("寫入 {} 失敗：{e}", tmp.display()))?;
        std::fs::rename(&tmp, &index).map_err(|e| format!("改名 {} 失敗：{e}", index.display()))?;
    }
    prune_search(dir, now.date());
    Ok(())
}

/// 搜尋紀錄保留 14 天（含稿件原文，比決策紀錄短沒有意義，比剪輯工具的 30 天短：原文佔空間）。
fn prune_search(dir: &std::path::Path, today: chrono::NaiveDate) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if let Ok(d) = chrono::NaiveDate::parse_from_str(&name, "%Y%m%d") {
            if (today - d).num_days() >= LOG_KEEP_DAYS {
                let _ = std::fs::remove_dir_all(entry.path());
            }
        }
    }
}

/// 讀搜尋紀錄裡的一個稿件原文；沒有回 None。
pub fn read_search_story(dir: &std::path::Path, date: chrono::NaiveDate, id: &str, version: &str) -> Option<String> {
    let name = news_script_core::search::story_file_name(id, version)?;
    std::fs::read_to_string(dir.join(date.format("%Y%m%d").to_string()).join("stories").join(name)).ok()
}

fn prune_rundown(dir: &std::path::Path, today: chrono::NaiveDate) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        let Some(date) = name.strip_suffix(".json") else { continue };
        if let Ok(d) = chrono::NaiveDate::parse_from_str(date, "%Y%m%d") {
            if (today - d).num_days() >= RUNDOWN_KEEP_DAYS {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};

    #[test]
    fn pasv_replies_parse_to_an_address() {
        let addr = parse_pasv("Entering Passive Mode (10,3,125,1,195,80).").unwrap();
        assert_eq!(addr, "10.3.125.1:50000".parse().unwrap());
        assert!(parse_pasv("Entering Passive Mode").is_none());
        assert!(parse_pasv("(1,2,3)").is_none());
    }

    /// 一台照台內 iNEWS 行為寫的假 FTP 伺服器。`queues` 是 CWD 路徑 → LIST 內容；
    /// `stories` 是 RETR 名稱 → NSML。收到的每一行指令都記在 `log`。
    struct FakeServer {
        port: u16,
        log: Arc<Mutex<Vec<Vec<u8>>>>,
        retrs: Arc<Mutex<usize>>,
        queues: Arc<Mutex<HashMap<String, String>>>,
        stories: Arc<Mutex<HashMap<String, String>>>,
    }

    fn reply(w: &mut TcpStream, s: &str) {
        w.write_all(s.as_bytes()).unwrap();
    }

    impl FakeServer {
        /// `drop_after` = 第幾個指令之後直接斷線（模擬閒置被踢），只發生一次。
        fn start(
            queues: HashMap<String, String>,
            stories: HashMap<String, String>,
            drop_after: Option<usize>,
        ) -> FakeServer {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            let log = Arc::new(Mutex::new(Vec::new()));
            let retrs = Arc::new(Mutex::new(0));
            let (log2, retrs2) = (log.clone(), retrs.clone());
            let queues = Arc::new(Mutex::new(queues));
            let stories = Arc::new(Mutex::new(stories));
            let (queues2, stories2) = (queues.clone(), stories.clone());
            std::thread::spawn(move || {
                let (queues, stories) = (queues2, stories2);
                let mut dropped = false;
                for conn in listener.incoming() {
                    let Ok(mut ctrl) = conn else { return };
                    let mut reader = BufReader::new(ctrl.try_clone().unwrap());
                    reply(&mut ctrl, "220 NEWS Rxnet (ftp server) 3.5.0.121\r\n");
                    let mut cwd = String::new();
                    let mut data_listener: Option<TcpListener> = None;
                    let mut count = 0;
                    loop {
                        let mut raw = Vec::new();
                        if reader.read_until(b'\n', &mut raw).unwrap_or(0) == 0 {
                            break;
                        }
                        crate::lock_or_recover(&log2).push(raw.clone());
                        count += 1;
                        if !dropped && drop_after == Some(count) {
                            dropped = true;
                            break;
                        }
                        let line = String::from_utf8_lossy(&raw).trim_end().to_string();
                        if raw.starts_with(&[0xEF, 0xBB, 0xBF]) {
                            reply(&mut ctrl, "500 not recognized\r\n");
                            continue;
                        }
                        let (cmd, arg) = line.split_once(' ').unwrap_or((line.as_str(), ""));
                        match cmd {
                            "USER" => reply(&mut ctrl, "331 password required\r\n"),
                            "PASS" if arg == "secret" => {
                                reply(&mut ctrl, "230-Welcome\r\n230-to iNEWS\r\n230 logged in\r\n")
                            }
                            "PASS" => reply(&mut ctrl, "530 login incorrect\r\n"),
                            "TYPE" => reply(&mut ctrl, "200 type set\r\n"),
                            "SITE" if arg == "FORMAT=3NSML" => reply(&mut ctrl, "200 format set\r\n"),
                            "SITE" => reply(&mut ctrl, "501 not supported\r\n"),
                            "CWD" if queues.lock().unwrap().contains_key(arg) => {
                                cwd = arg.to_string();
                                reply(&mut ctrl, "250 ok\r\n")
                            }
                            "CWD" => reply(&mut ctrl, "550 no such directory\r\n"),
                            "PASV" => {
                                let l = TcpListener::bind("127.0.0.1:0").unwrap();
                                let p = l.local_addr().unwrap().port();
                                data_listener = Some(l);
                                reply(&mut ctrl, &format!("227 Entering Passive Mode (127,0,0,1,{},{})\r\n", p / 256, p % 256));
                            }
                            "LIST" | "RETR" => {
                                let body = if cmd == "LIST" {
                                    queues.lock().unwrap().get(&cwd).cloned().filter(|b| !b.is_empty())
                                } else {
                                    *crate::lock_or_recover(&retrs2) += 1;
                                    crate::lock_or_recover(&stories).get(arg).cloned()
                                };
                                let l = data_listener.take().unwrap();
                                match body {
                                    None => reply(&mut ctrl, "550 empty\r\n"),
                                    Some(b) => {
                                        reply(&mut ctrl, "150 opening data connection\r\n");
                                        let (mut d, _) = l.accept().unwrap();
                                        d.write_all(b.as_bytes()).unwrap();
                                        drop(d);
                                        reply(&mut ctrl, "226 transfer complete\r\n");
                                    }
                                }
                            }
                            "QUIT" => {
                                reply(&mut ctrl, "221 bye\r\n");
                                break;
                            }
                            _ => reply(&mut ctrl, "500 not recognized\r\n"),
                        }
                    }
                }
            });
            FakeServer { port, log, retrs, queues, stories }
        }

        /// 換掉一個 queue 的內容（模擬製作人改 rundown）。`rows` = (ID, slug, NSML)。
        fn set_queue(&self, path: &str, rows: &[(String, String, String)]) {
            let mut list = String::new();
            let mut stories = self.stories.lock().unwrap();
            for (id, slug, nsml) in rows {
                list.push_str(&list_line(false, id, slug));
                stories.insert(format!("{id} {slug}"), nsml.clone());
            }
            self.queues.lock().unwrap().insert(path.to_string(), list);
        }

        fn commands(&self) -> Vec<String> {
            crate::lock_or_recover(&self.log).iter().map(|l| String::from_utf8_lossy(l).trim_end().to_string()).collect()
        }
    }

    fn cfg(port: u16) -> InewsConfig {
        InewsConfig { server: "127.0.0.1".into(), port, ..InewsConfig::default() }
    }

    fn creds(pass: &str) -> Credentials {
        Credentials { user: "tester".into(), pass: pass.into() }
    }

    fn list_line(floated: bool, id: &str, slug: &str) -> String {
        format!("-{}-------- 1 1     0 Oct 25 06:19  {id} {slug}\r\n", if floated { "f" } else { "-" })
    }

    fn story(page: &str, title: &str, total: u32, cume: &str, style: &str) -> String {
        format!(
            "<nsml><fields><string id=\"page-number\">{page}</string><string id=\"title\">{title}</string>\
             <string id=\"v-style\">{style}</string><duration id=\"total-time\">{total}</duration>\
             <string id=\"cume-time\">{cume}</string></fields><body><p><a idref=\"0\"/></p><p>{title}的合成內文。</p><p></p></body>\
             <aeset><ae id=\"0\"><ap>[BAR_合成大]</ap><ap>T2{title}標題</ap></ae></aeset></nsml>"
        )
    }

    /// 0800 與 0900 兩節的假伺服器。0900 的第一個硬時間是 08:55:00。
    fn two_shows(drop_after: Option<usize>) -> FakeServer {
        let mut queues = HashMap::new();
        let mut stories = HashMap::new();
        queues.insert(
            "51新聞台.SHOWS".to_string(),
            "d--------- 1 d    27 Apr 30  2025 .0800\r\nd--------- 1 d    27 Apr 30  2025 .0900\r\nd--------- 1 d    27 Apr 30  2025 .README\r\n"
                .to_string(),
        );
        let mut add = |queue: &str, rows: &[(bool, &str, String)]| {
            let mut list = String::new();
            for (floated, id, nsml) in rows {
                let slug = format!("稿{}", &id[id.len() - 2..]);
                list.push_str(&list_line(*floated, id, &slug));
                stories.insert(format!("{id} {slug}"), nsml.clone());
            }
            queues.insert(queue.to_string(), list);
        };
        add(
            "51新聞台.SHOWS.0800.ONAIR",
            &[
                (false, "0000000A:00000001:00000A01", story("", "大片頭", 60, "＠28500", "")),
                (false, "0000000A:00000002:00000A02", story("A1", "合成早安稿0800", 120, "", "SOT")),
                (true, "0000000A:00000003:00000A03", story("", "合成框", 30, "", "")),
                (false, "0000000A:00000004:00000A04", story("A2", "合成第二則0800", 90, "", "SOT")),
                (false, "0000000A:00000005:00000A05", story("", "下節預告三", 30, "", "")),
                (false, "0000000A:00000006:00000A06", story("=========", "=CM 3==", 300, "＠31800", "")),
                (false, "0000000A:00000007:00000A07", story("C1", "合成預備稿0800", 60, "", "SOT")),
            ],
        );
        add(
            "51新聞台.SHOWS.0900.ONAIR",
            &[
                (false, "0000000B:00000001:00000B01", story("", "大片頭", 60, "＠32100", "")),
                (false, "0000000B:00000002:00000B02", story("A1", "合成九點稿0900", 120, "", "SOT")),
            ],
        );
        FakeServer::start(queues, stories, drop_after)
    }

    /// `two_shows` 再加一個停播的 1000：只有「關鍵KS」開頭、沒有樣式的列。
    fn with_program() -> FakeServer {
        let mut queues = HashMap::new();
        let mut stories = HashMap::new();
        queues.insert(
            "51新聞台.SHOWS".to_string(),
            "d--------- 1 d    27 Apr 30  2025 .0800\r\nd--------- 1 d    27 Apr 30  2025 .0900\r\nd--------- 1 d    27 Apr 30  2025 .1000\r\n"
                .to_string(),
        );
        let mut add = |queue: &str, rows: &[(&str, String)]| {
            let mut list = String::new();
            for (id, nsml) in rows {
                let slug = format!("稿{}", &id[id.len() - 2..]);
                list.push_str(&list_line(false, id, &slug));
                stories.insert(format!("{id} {slug}"), nsml.clone());
            }
            queues.insert(queue.to_string(), list);
        };
        add("51新聞台.SHOWS.0800.ONAIR", &[
            ("0000000A:00000001:00000A01", story("", "大片頭", 60, "＠28500", "")),
            ("0000000A:00000002:00000A02", story("A1", "合成早安稿0800", 120, "", "SOT")),
        ]);
        add("51新聞台.SHOWS.0900.ONAIR", &[
            ("0000000B:00000001:00000B01", story("", "大片頭", 60, "＠32100", "")),
            ("0000000B:00000002:00000B02", story("A1", "合成九點稿0900", 120, "", "SOT")),
            ("0000000B:00000003:00000B03", story("B1", "合成九點半稿0900", 120, "＠35400", "SOT")),
        ]);
        add("51新聞台.SHOWS.1000.ONAIR", &[
            ("0000000C:00000001:00000C01", story("", "關鍵KS美中AI技術戰", 600, "", "")),
            ("0000000C:00000002:00000C02", story("", "關鍵KS台海局勢", 600, "", "")),
        ]);
        FakeServer::start(queues, stories, None)
    }

    #[test]
    fn an_off_air_program_ends_the_show_on_the_hour_and_adds_its_divider_after_the_sweep() {
        let server = with_program();
        let mut state = InewsState::default();
        let c = cfg(server.port);
        let auto = |start| FetchMode::Auto { session_start: start };
        let r = state.fetch(&c, &creds("secret"), 9 * 3600 + 30 * 60, auto(true)).unwrap();
        assert_eq!(r.meta.show, "0900");
        assert_eq!(r.meta.range_end, "10:00:00");
        assert!(!r.meta.off_air);
        // 10:00:30 換到停播的 1000：0900 最後同步，1000 的關鍵KS 列不匯入。
        let r = state.fetch(&c, &creds("secret"), 10 * 3600 + 30, auto(false)).unwrap();
        assert_eq!(r.meta.show, "1000");
        assert!(r.meta.off_air);
        assert_eq!(r.meta.program.as_deref(), Some("關鍵時刻"));
        assert!(r.meta.stories.iter().all(|s| s.show == "0900"), "{:?}", r.meta.stories);
        assert_eq!(r.meta.switched_from.as_deref(), Some("0900"));
        assert_eq!(r.meta.program_divider, None);
        // 10:10 補掃 0900，帶出節目分隔線。
        let r = state.fetch(&c, &creds("secret"), 10 * 3600 + 10 * 60, auto(false)).unwrap();
        assert_eq!(r.meta.closed_show.as_deref(), Some("0900"));
        assert_eq!(r.meta.program_divider.as_deref(), Some("==== 10 關鍵時刻 ===="));
        assert!(r.meta.stories.iter().all(|s| s.show == "0900"));
        assert!(r.log_line.contains("1000 停播節次（關鍵時刻），不匯入"), "{}", r.log_line);
        // 只補掃一次。
        let r = state.fetch(&c, &creds("secret"), 10 * 3600 + 11 * 60, auto(false)).unwrap();
        assert_eq!(r.meta.closed_show, None);
        assert!(r.files.is_empty());
    }

    /// 0800 有一則長度 0:00 的直進（07:58:00 開始）；0900 跟 `two_shows` 一樣。
    fn live_show() -> FakeServer {
        let mut queues = HashMap::new();
        let mut stories = HashMap::new();
        queues.insert(
            "51新聞台.SHOWS".to_string(),
            "d--------- 1 d    27 Apr 30  2025 .0800\r\nd--------- 1 d    27 Apr 30  2025 .0900\r\n".to_string(),
        );
        let mut add = |queue: &str, rows: &[(&str, String)]| {
            let mut list = String::new();
            for (id, nsml) in rows {
                let slug = format!("稿{}", &id[id.len() - 2..]);
                list.push_str(&list_line(false, id, &slug));
                stories.insert(format!("{id} {slug}"), nsml.clone());
            }
            queues.insert(queue.to_string(), list);
        };
        add(
            "51新聞台.SHOWS.0800.ONAIR",
            &[
                ("0000000A:00000001:00000A01", story("", "大片頭", 60, "＠28500", "")),
                ("0000000A:00000002:00000A02", story("A1", "合成早安稿0800", 120, "", "SOT")),
                ("0000000A:00000003:00000A03", story("A2", "合成直進", 0, "", "直進")),
                ("0000000A:00000004:00000A04", story("A3", "合成直進後一0800", 60, "", "SOT")),
                ("0000000A:00000005:00000A05", story("A4", "合成直進後二0800", 60, "", "SOT")),
            ],
        );
        add(
            "51新聞台.SHOWS.0900.ONAIR",
            &[
                ("0000000B:00000001:00000B01", story("", "大片頭", 60, "＠32100", "")),
                ("0000000B:00000002:00000B02", story("A1", "合成九點稿0900", 120, "", "SOT")),
            ],
        );
        FakeServer::start(queues, stories, None)
    }

    #[test]
    fn stories_after_an_open_live_wait_and_the_hold_is_logged() {
        let server = live_show();
        let mut state = InewsState::default();
        let r = state.fetch(&cfg(server.port), &creds("secret"), 8 * 3600 + 5 * 60, FetchMode::Manual).unwrap();
        assert_eq!(names_of(&r), ["大片頭.txt", "合成早安稿0800.txt", "合成直進.txt"]);
        assert!(r.log_line.contains("直進暫緩：07:58:00 合成直進 起，後續 2 則等待新硬時間"), "{}", r.log_line);

        // 30 分鐘後仍沒有新的硬時間：恢復一般判定，並留下警告。
        let r = state.fetch(&cfg(server.port), &creds("secret"), 8 * 3600 + 28 * 60, FetchMode::Manual).unwrap();
        assert_eq!(r.files.len(), 5);
        assert!(r.meta.warnings.iter().any(|w| w.contains("直進暫緩逾 30 分鐘")), "{:?}", r.meta.warnings);

        // 關掉就是 v0.3.1 的行為。
        let mut off = cfg(server.port);
        off.hold_after_open_live = false;
        let r = state.fetch(&off, &creds("secret"), 8 * 3600 + 5 * 60, FetchMode::Manual).unwrap();
        assert_eq!(r.files.len(), 5);
    }

    #[test]
    fn login_sends_the_verified_sequence_without_bom_or_longnames() {
        let server = two_shows(None);
        let msg = test_connection(&cfg(server.port), &creds("secret")).unwrap();
        assert!(msg.contains("登入成功"), "{msg}");
        assert!(msg.contains("2 個節次"), "{msg}");
        let cmds = server.commands();
        assert_eq!(&cmds[..4], ["USER tester", "PASS secret", "TYPE A", "SITE FORMAT=3NSML"]);
        assert!(!cmds.iter().any(|c| c.contains("LONGNAMES")));
        assert!(crate::lock_or_recover(&server.log).iter().all(|l| !l.starts_with(&[0xEF, 0xBB, 0xBF]) && l.ends_with(b"\r\n")));
        assert!(cmds.contains(&"CWD 51新聞台.SHOWS".to_string()));
    }

    #[test]
    fn a_wrong_password_reports_a_login_failure_without_the_password() {
        let server = two_shows(None);
        let err = test_connection(&cfg(server.port), &creds("wrong")).unwrap_err();
        assert!(err.contains("登入失敗"), "{err}");
        assert!(err.contains("530"), "{err}");
        assert!(!err.contains("wrong"), "password leaked into {err}");
    }

    fn names_of(r: &FetchResult) -> Vec<&str> {
        r.files.iter().map(|(n, _)| n.as_str()).collect()
    }

    #[test]
    fn a_round_imports_aired_rows_inside_the_time_window() {
        let server = two_shows(None);
        let mut state = InewsState::default();
        // 08:20：大片頭 07:55、早安稿 07:56、第二則 07:58、預告 07:59:30 都播過了。
        let r = state.fetch(&cfg(server.port), &creds("secret"), 8 * 3600 + 20 * 60, FetchMode::Manual).unwrap();
        assert_eq!(r.meta.show, "0800");
        assert!(!r.meta.switched_early);
        assert_eq!((r.meta.range_start.as_str(), r.meta.range_end.as_str()), ("07:55:00", "08:55:00"));
        assert_eq!(r.meta.start_source, "硬時間");
        assert_eq!(r.meta.end_show.as_deref(), Some("0900"));
        assert_eq!(r.meta.cutoff_rule, "none", "marker cutoff is off by default");
        // float 的框不輸出；=CM 3==（08:50）還沒播到。
        assert_eq!(names_of(&r), ["大片頭.txt", "合成早安稿0800.txt", "合成第二則0800.txt", "下節預告三.txt"]);
        assert_eq!(r.meta.pending, 1);
        let early = &r.files[1].1;
        assert!(early.contains("\r\n累積時間: 07:56:00\r\n"), "computed cume filled in");
        assert_eq!(r.meta.stories[1].key, "0000000A:00000002");
        assert_eq!(r.meta.stories[1].version, "00000A02");
        assert_eq!(r.meta.stories[1].show, "0800");
        assert!(r.log_line.contains("範圍 07:55:00（硬時間）～ 08:55:00（0900 硬時間）"), "{}", r.log_line);
        assert!(r.log_line.contains("最後納入 07:59:30 下節預告三"), "{}", r.log_line);
        assert!(r.log_line.contains("第一則排除 08:50:00 =CM 3=="), "{}", r.log_line);
        // 時間校正只拿已播出的列：還沒播到的 =CM 3== 不在裡面（v0.3.9）。
        let rows: Vec<(&str, &str)> = r.meta.time_rows.iter().map(|t| (t.slug.as_str(), t.time.as_str())).collect();
        assert_eq!(rows, [("大片頭", "07:55:00"), ("合成早安稿0800", "07:56:00"), ("合成第二則0800", "07:58:00"), ("下節預告三", "07:59:30")]);
        assert_eq!((r.meta.time_rows[0].start, r.meta.time_rows[0].end), (28500, 32100), "07:55:00 ～ 08:55:00");
        // 0800 全部 7 則 + 為了算 0800 的結束時間讀的 0900 的 2 則。
        assert_eq!(*crate::lock_or_recover(&server.retrs), 9);

        // 第二輪：每個 queue LIST 一次，沒有任何變動就不 RETR。
        let r = state.fetch(&cfg(server.port), &creds("secret"), 8 * 3600 + 21 * 60, FetchMode::Manual).unwrap();
        assert_eq!(r.meta.retrieved, 0);
        assert_eq!(*crate::lock_or_recover(&server.retrs), 9);
        let lists = server.commands().iter().filter(|c| *c == "LIST").count();
        assert_eq!(lists, 5, "root once (cached), both queues once per round");
    }

    #[test]
    fn the_next_show_takes_over_once_its_first_hard_time_passes() {
        let server = two_shows(None);
        let mut state = InewsState::default();
        let r = state.fetch(&cfg(server.port), &creds("secret"), 8 * 3600 + 54 * 60, FetchMode::Manual).unwrap();
        assert_eq!(r.meta.show, "0800");
        // 08:55:30：0900 的大片頭（08:55:00）已經開始，九點稿（08:56:00）還沒。
        let r = state.fetch(&cfg(server.port), &creds("secret"), 8 * 3600 + 55 * 60 + 30, FetchMode::Manual).unwrap();
        assert_eq!(r.meta.show, "0900");
        assert!(r.meta.switched_early);
        assert_eq!(r.meta.show_label, "09");
        assert_eq!(names_of(&r), ["大片頭.txt"]);
        assert_eq!(r.meta.switched_from, None, "manual imports never do the final sync");
    }

    #[test]
    fn switching_shows_does_a_final_sync_of_the_old_one_then_sweeps_it_ten_minutes_later() {
        let server = two_shows(None);
        let mut state = InewsState::default();
        let c = cfg(server.port);
        let auto = |start| FetchMode::Auto { session_start: start };
        // 08:51 開始自動刷新：0800，=CM 3==（08:50）剛播。
        let r = state.fetch(&c, &creds("secret"), 8 * 3600 + 51 * 60, auto(true)).unwrap();
        assert_eq!(r.meta.show, "0800");
        assert_eq!(r.meta.closed_show, None, "never sweeps a show that ended before auto-refresh started");
        // 08:55:30 換到 0900：先對 0800 做最後一次同步（累積時間 < 08:55:00），預備稿不納入。
        let r = state.fetch(&c, &creds("secret"), 8 * 3600 + 55 * 60 + 30, auto(false)).unwrap();
        assert_eq!(r.meta.show, "0900");
        assert_eq!(r.meta.switched_from.as_deref(), Some("0800"));
        let shows: Vec<(&str, &str)> = r.meta.stories.iter().map(|s| (s.show.as_str(), s.file_name.as_str())).collect();
        assert_eq!(
            shows,
            [
                ("0800", "大片頭.txt"),
                ("0800", "合成早安稿0800.txt"),
                ("0800", "合成第二則0800.txt"),
                ("0800", "下節預告三.txt"),
                ("0800", "=CM 3==.txt"),
                ("0900", "大片頭_2.txt"),
            ]
        );
        assert!(r.log_line.contains("換節 0800 → 0900"), "{}", r.log_line);
        // 09:04 還沒到補掃時間（0900 在 08:55 換節，換節後 10 分鐘 = 09:05）。
        let r = state.fetch(&c, &creds("secret"), 9 * 3600 + 4 * 60, auto(false)).unwrap();
        assert_eq!(r.meta.closed_show, None);
        assert!(r.meta.stories.iter().all(|s| s.show == "0900"));
        // 0800 還沒補掃完：它已播出的列照樣校正時間。
        assert!(r.meta.time_rows.iter().any(|t| t.show == "0800" && t.slug == "合成早安稿0800"));
        assert!(r.meta.time_rows.iter().any(|t| t.show == "0900"));
        // 09:05 補掃 0800：不看現在時間，累積時間 < 08:55:00 的全部納入；分隔線可以寫了。
        let r = state.fetch(&c, &creds("secret"), 9 * 3600 + 5 * 60, auto(false)).unwrap();
        assert_eq!(r.meta.closed_show.as_deref(), Some("0800"));
        let swept: Vec<&str> = r.meta.stories.iter().filter(|s| s.show == "0800").map(|s| s.file_name.as_str()).collect();
        assert_eq!(swept.len(), 5);
        assert!(!r.files.iter().any(|(_, t)| t.contains("合成預備稿0800")));
        assert!(r.log_line.contains("補掃 0800"), "{}", r.log_line);
        assert!(r.meta.time_rows.iter().any(|t| t.show == "0800"), "the sweep round still corrects 0800");
        // 只補掃一次。補掃完之後 0800 就不再校正。
        let r = state.fetch(&c, &creds("secret"), 9 * 3600 + 11 * 60, auto(false)).unwrap();
        assert_eq!(r.meta.closed_show, None);
        assert!(r.meta.time_rows.iter().all(|t| t.show == "0900"));
    }

    #[test]
    fn a_restart_across_a_show_switch_resumes_the_old_shows_final_sync_and_sweep() {
        // 08:51 在跟 0800，存檔後程式停了；08:56 重開時已經是 0900（v0.3.5）。
        let server = two_shows(None);
        let c = cfg(server.port);
        let auto = |start| FetchMode::Auto { session_start: start };
        let mut state = InewsState::default();
        state.fetch(&c, &creds("secret"), 8 * 3600 + 51 * 60, auto(true)).unwrap();
        let saved = state.saved("1029");
        assert_eq!(saved.last_show.as_deref(), Some("0800"));

        // 重開＝session_start：記憶體裡的狀態作廢，只剩存檔。
        state.set_resume(Some(saved));
        let r = state.fetch(&c, &creds("secret"), 8 * 3600 + 56 * 60, auto(true)).unwrap();
        assert_eq!(r.meta.show, "0900");
        assert_eq!(r.meta.resumed_show.as_deref(), Some("0800"));
        assert_eq!(r.meta.switched_from.as_deref(), Some("0800"));
        assert!(r.log_line.contains("補做 0800 的收尾"), "{}", r.log_line);
        let r = state.fetch(&c, &creds("secret"), 9 * 3600 + 5 * 60, auto(false)).unwrap();
        assert_eq!(r.meta.closed_show.as_deref(), Some("0800"));
    }

    #[test]
    fn a_restart_without_a_saved_show_still_never_sweeps_backwards() {
        let server = two_shows(None);
        let c = cfg(server.port);
        let auto = |start| FetchMode::Auto { session_start: start };
        let mut state = InewsState::default();
        let r = state.fetch(&c, &creds("secret"), 8 * 3600 + 56 * 60, auto(true)).unwrap();
        assert_eq!(r.meta.resumed_show, None);
        assert_eq!(r.meta.switched_from, None);
        let r = state.fetch(&c, &creds("secret"), 9 * 3600 + 5 * 60, auto(false)).unwrap();
        assert_eq!(r.meta.closed_show, None);
        // 存檔的節次離現在太遠（中間有節次沒跟到）：不補，只記下來。
        state.set_resume(Some(SavedSession { day: "1029".into(), last_show: Some("0700".into()), swept_for: None }));
        let r = state.fetch(&c, &creds("secret"), 8 * 3600 + 56 * 60, auto(true)).unwrap();
        assert_eq!(r.meta.switched_from, None);
        assert!(r.log_line.contains("上次停在 0700"), "{}", r.log_line);
    }

    fn slugs(s: &ShowSnapshot) -> Vec<&str> {
        s.stories.iter().map(|r| r.slug.as_str()).collect()
    }

    #[test]
    fn the_rundown_snapshot_keeps_every_story_of_the_show_except_commercials() {
        let server = two_shows(None);
        let mut state = InewsState::default();
        // 08:20：預備稿（累積時間 ≥ 下一節開始）和 float 的框沒被匯入，但 JSON 要有；=CM 3== 不要。
        let r = state.fetch(&cfg(server.port), &creds("secret"), 8 * 3600 + 20 * 60, FetchMode::Manual).unwrap();
        assert_eq!(r.rundown.len(), 1, "0800's wrap-around previous show (0900, yesterday's) is not today's data");
        let s = &r.rundown[0];
        assert_eq!((s.show.as_str(), s.nominal), ("0800", 28800));
        assert_eq!(slugs(s), ["大片頭", "合成早安稿0800", "合成框", "合成第二則0800", "下節預告三", "合成預備稿0800"]);
        let early = &s.stories[1];
        assert_eq!((early.style.as_str(), early.id.as_str()), ("SOT", "0000000A:00000002"));
        assert_eq!(early.bars, ["合成早安稿0800標題"]);
        assert_eq!(r.files.len(), 4, "the collab doc input is unchanged");
    }

    #[test]
    fn the_rundown_snapshot_has_the_old_show_at_a_switch_and_after_a_restart() {
        let server = two_shows(None);
        let c = cfg(server.port);
        let auto = |start| FetchMode::Auto { session_start: start };
        let mut state = InewsState::default();
        state.fetch(&c, &creds("secret"), 8 * 3600 + 51 * 60, auto(true)).unwrap();
        // 換節那一輪：新舊兩節都在。
        let r = state.fetch(&c, &creds("secret"), 8 * 3600 + 55 * 60 + 30, auto(false)).unwrap();
        let shows: Vec<&str> = r.rundown.iter().map(|s| s.show.as_str()).collect();
        assert_eq!(shows, ["0800", "0900"]);
        assert!(slugs(&r.rundown[0]).contains(&"合成預備稿0800"));

        // 重開時沒有存檔：不會對 0800 做最後同步或補掃，但 JSON 還是帶著它。假伺服器一次只
        // 服務一條連線，舊的狀態要先放掉。
        state.drop_session();
        let mut fresh = InewsState::default();
        let r = fresh.fetch(&c, &creds("secret"), 8 * 3600 + 56 * 60, auto(true)).unwrap();
        assert_eq!(r.meta.switched_from, None);
        assert_eq!(r.meta.closed_show, None);
        assert!(r.files.iter().all(|(_, t)| !t.contains("合成預備稿0800")));
        let shows: Vec<&str> = r.rundown.iter().map(|s| s.show.as_str()).collect();
        assert_eq!(shows, ["0800", "0900"]);
        assert!(slugs(&r.rundown[0]).contains(&"合成預備稿0800"));
    }

    #[test]
    fn a_show_belongs_to_the_broadcast_day_it_airs_in() {
        let d = |m, day| chrono::NaiveDate::from_ymd_opt(2026, m, day).unwrap();
        // 08:55 看 2400（00:00）：明天凌晨，但還在今天的播出日裡。
        assert_eq!(show_broadcast_date(at(27, 23, 55), "02:00", 0), d(10, 27));
        // 00:30 的 2400 屬於前一天；02:05 看 0100 也是前一天，看 0200 以後的節次才是當天。
        assert_eq!(show_broadcast_date(at(27, 0, 30), "02:00", 0), d(10, 26));
        assert_eq!(show_broadcast_date(at(27, 2, 5), "02:00", 3600), d(10, 26));
        assert_eq!(show_broadcast_date(at(27, 2, 5), "02:00", 6 * 3600), d(10, 27));
        // 00:05 還在補掃 2300：23:00 是前一個日曆日。
        assert_eq!(show_broadcast_date(at(27, 0, 5), "02:00", 23 * 3600), d(10, 26));
        assert_eq!(show_broadcast_date(at(27, 10, 0), "02:00", 9 * 3600), d(10, 27));
        // 設定壞掉就當 00:00。
        assert_eq!(show_broadcast_date(at(27, 1, 30), "兩點", 3600), d(10, 27));
    }

    fn snapshot(show: &str, nominal: u32, rows: &[(&str, &str)]) -> ShowSnapshot {
        let stories = rows.iter().map(|(slug, updated)| RundownStory { slug: slug.to_string(), updated: updated.to_string(), ..Default::default() });
        ShowSnapshot { show: show.into(), nominal, stories: stories.collect() }
    }

    #[test]
    fn rundown_files_merge_each_round_and_old_ones_are_pruned_after_thirty_days() {
        let dir = std::env::temp_dir().join(format!("nst-rundown-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for d in ["20260926", "20260927", "20261001"] {
            std::fs::write(dir.join(format!("{d}.json")), "{}").unwrap();
        }
        std::fs::write(dir.join("notes.json"), "x").unwrap();

        let now = at(27, 10, 5);
        write_rundown(&dir, now, "02:00", vec![snapshot("1000", 36000, &[("甲", "a"), ("乙", "a")])]).unwrap();
        write_rundown(&dir, now, "02:00", vec![snapshot("0900", 32400, &[("丙", "a")]), snapshot("1000", 36000, &[("乙", "b")])]).unwrap();

        let day = RundownDay::from_json(&std::fs::read_to_string(dir.join("20261027.json")).unwrap()).unwrap();
        assert_eq!(day.date, "2026-10-27");
        let one = |show: &str| day.shows[show].iter().map(|s| (s.slug.as_str(), s.version)).collect::<Vec<_>>();
        assert_eq!(one("1000"), [("甲", 1), ("乙", 2)], "unseen stories stay, a changed one counts a version");
        assert_eq!(one("0900"), [("丙", 1)]);
        let mut left: Vec<String> = std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().to_string()).collect();
        left.sort();
        assert_eq!(left, ["20261001.json", "20261027.json", "notes.json"], "no temp file left, 30+ day old files pruned");

        // 壞掉的檔案不會連同新資料一起被蓋掉。
        std::fs::write(dir.join("20261027.json"), "{ broken").unwrap();
        write_rundown(&dir, now, "02:00", vec![snapshot("1000", 36000, &[("丁", "a")])]).unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("20261027.json.bad")).unwrap(), "{ broken");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_search_store_keeps_air_time_and_story_text_apart_from_the_clip_file() {
        let dir = std::env::temp_dir().join(format!("nst-search-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let now = chrono::NaiveDate::from_ymd_opt(2026, 11, 11).unwrap().and_hms_opt(9, 30, 0).unwrap();
        let row = |air: &str, raw: Option<&str>| SearchRow {
            entry: SearchEntry {
                slug: "合成稿0900".into(),
                id: "040CE84D:0044D4CD".into(),
                version: "6ADD2EE7".into(),
                style: "SOT".into(),
                air_time: air.into(),
                updated: "2026-11-11 09:00:00".into(),
            },
            raw: raw.map(str::to_string),
        };
        let show = |r| vec![SearchShow { show: "0900".into(), nominal: 9 * 3600, stories: vec![r] }];
        write_search(&dir, now, "02:00", show(row("09:12:58", Some("<nsml>原文</nsml>")))).unwrap();
        // 下一輪：時間挪了、這一輪快取裡沒有原文。原文檔不會被刪、時間更新。
        write_search(&dir, now, "02:00", show(row("09:13:30", None))).unwrap();
        let day = SearchDay::from_json(&std::fs::read_to_string(dir.join("20261111").join("index.json")).unwrap()).unwrap();
        assert_eq!(day.shows["0900"].len(), 1);
        assert_eq!(day.shows["0900"][0].air_time, "09:13:30");
        let date = chrono::NaiveDate::from_ymd_opt(2026, 11, 11).unwrap();
        assert_eq!(read_search_story(&dir, date, "040CE84D:0044D4CD", "6ADD2EE7").as_deref(), Some("<nsml>原文</nsml>"));
        assert_eq!(read_search_story(&dir, date, "040CE84D:0044D4CD", "OTHER"), None, "別的版本沒存就是沒有");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn old_logs_are_pruned_after_fourteen_days() {
        let dir = std::env::temp_dir().join(format!("nst-log-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for d in ["20261001", "20261011", "20261012", "20261025"] {
            std::fs::write(dir.join(format!("inews-{d}.log")), "x").unwrap();
        }
        std::fs::write(dir.join("anomaly-20261010-124520.json"), "x").unwrap();
        std::fs::write(dir.join("anomaly-20261024-124520.json"), "x").unwrap();
        std::fs::write(dir.join("other.txt"), "x").unwrap();
        prune_logs(&dir, "20261025");
        let mut left: Vec<String> = std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().to_string()).collect();
        left.sort();
        assert_eq!(left, ["anomaly-20261024-124520.json", "inews-20261012.log", "inews-20261025.log", "other.txt"]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_dropped_connection_is_reconnected_transparently() {
        // 第 8 個指令（進 queue 的 CWD）時伺服器直接斷線，像閒置被踢掉一樣。
        let server = two_shows(Some(8));
        let mut state = InewsState::default();
        let r = state.fetch(&cfg(server.port), &creds("secret"), 8 * 3600 + 20 * 60, FetchMode::Manual).unwrap();
        assert_eq!(r.meta.show, "0800");
        let logins = server.commands().iter().filter(|c| c.starts_with("USER")).count();
        assert!(logins >= 2, "reconnected and logged in again");
    }

    #[test]
    fn an_empty_queue_answering_550_is_an_empty_rundown() {
        let mut queues = HashMap::new();
        queues.insert("51新聞台.SHOWS".to_string(), "d--------- 1 d 27 Apr 30  2025 .0800\r\n".to_string());
        queues.insert("51新聞台.SHOWS.0800.ONAIR".to_string(), String::new());
        let server = FakeServer::start(queues, HashMap::new(), None);
        let r = InewsState::default().fetch(&cfg(server.port), &creds("secret"), 8 * 3600, FetchMode::Manual).unwrap();
        assert!(r.files.is_empty());
        assert_eq!(r.meta.list_count, 0);
    }

    fn at(d: u32, h: u32, m: u32) -> chrono::NaiveDateTime {
        chrono::NaiveDate::from_ymd_opt(2026, 10, d).unwrap().and_hms_opt(h, m, 0).unwrap()
    }

    #[test]
    fn stories_aired_before_two_belong_to_the_previous_day() {
        assert_eq!(broadcast_days(at(27, 1, 30), "02:00"), ("1026".into(), "1025".into()));
        assert_eq!(broadcast_days(at(27, 0, 5), "02:00"), ("1026".into(), "1025".into()));
        assert_eq!(broadcast_days(at(27, 2, 10), "02:00"), ("1027".into(), "1026".into()));
        assert_eq!(broadcast_days(at(26, 23, 59), "02:00"), ("1026".into(), "1025".into()));
        // Month and year boundaries.
        let jan1 = chrono::NaiveDate::from_ymd_opt(2027, 1, 1).unwrap().and_hms_opt(1, 0, 0).unwrap();
        assert_eq!(broadcast_days(jan1, "02:00"), ("1231".into(), "1230".into()));
        // A broken setting reads as midnight rather than failing the compare.
        assert_eq!(broadcast_days(at(27, 1, 30), "兩點"), ("1027".into(), "1026".into()));
    }


    // --- rundown 異常保護的模擬（v0.3.11）--------------------------------------------

    const SIM_KEYS: [&str; 9] = ["A", "B", "C", "D", "E", "M1", "M2", "M3", "F"];

    /// 一節的模擬 rundown：片頭（硬時間 `hard`）後面照 `order` 排。每則稿的 ID 固定，
    /// 搬動不會變。M 開頭的長 2 分鐘，其他 5 分鐘。
    fn sim_rows(show: &str, hard: u32, order: &[&str]) -> Vec<(String, String, String)> {
        let mut v = vec![(format!("{show}0000:00000000:00000001"), "片頭".to_string(), story("", "片頭", 60, &format!("＠{hard}"), ""))];
        for k in order {
            let n = SIM_KEYS.iter().position(|x| x == k).map_or(0x50 + k.len(), |i| i + 1);
            let len = if k.starts_with('M') { 120 } else { 300 };
            let title = format!("{k}模擬稿{show}");
            v.push((format!("{show}0000:{n:08X}:00000001"), format!("稿{k}"), story("A1", &title, len, "", "SOT")));
        }
        v
    }

    fn sim_server(shows: &[&str]) -> FakeServer {
        let mut queues = HashMap::new();
        let root: String = shows.iter().map(|s| format!("d--------- 1 d    27 Apr 30  2025 .{s}\r\n")).collect();
        queues.insert("51新聞台.SHOWS".to_string(), root);
        FakeServer::start(queues, HashMap::new(), None)
    }

    fn hold_of(r: &FetchResult, k: &str) -> Option<Option<Hold>> {
        r.meta.stories.iter().find(|s| s.file_name.starts_with(&format!("{k}模擬稿"))).map(|s| s.hold)
    }

    fn hm(h: u32, m: u32) -> u32 {
        h * 3600 + m * 60
    }

    const NORMAL: [&str; 9] = ["A", "B", "C", "D", "E", "M1", "M2", "M3", "F"];
    /// 製作人把播過的 A～D 拖到最後面，還沒重設硬時間：E、M1～M3、F 往前縮到 11:41～11:52。
    const MOVED: [&str; 9] = ["E", "M1", "M2", "M3", "F", "A", "B", "C", "D"];
    const Q1200: &str = "51新聞台.SHOWS.1200.ONAIR";
    const Q1400: &str = "51新聞台.SHOWS.1400.ONAIR";

    #[test]
    fn sim_a_mass_move_is_held_until_the_rundown_settles_then_written_normally() {
        let server = sim_server(&["1200", "1400"]);
        server.set_queue(Q1200, &sim_rows("1200", 42000, &NORMAL));
        server.set_queue(Q1400, &sim_rows("1400", 50100, &["A"]));
        let c = cfg(server.port);
        let mut st = InewsState::default();
        let auto = |start| FetchMode::Auto { session_start: start };
        let pw = creds("secret");

        // 12:04 剛打開：A～E 已播出，第一輪都等確認；12:05 確認，可以寫。
        let r = st.fetch(&c, &pw, hm(12, 4), auto(true)).unwrap();
        assert_eq!(hold_of(&r, "E"), Some(Some(Hold::Confirm)));
        let r = st.fetch(&c, &pw, hm(12, 5), auto(false)).unwrap();
        assert_eq!(hold_of(&r, "E"), Some(None));
        assert!(!r.meta.cooling && r.extra_logs.is_empty(), "{:?}", r.extra_logs);

        // 12:06 大量搬移：還沒播的 M1～M3、F 被算成 11:46～11:52 已播出。
        server.set_queue(Q1200, &sim_rows("1200", 42000, &MOVED));
        let r = st.fetch(&c, &pw, hm(12, 6), auto(false)).unwrap();
        assert!(r.meta.cooling);
        for k in ["M1", "M2", "M3", "F"] {
            assert_eq!(hold_of(&r, k), Some(Some(Hold::Suspect)), "{k}");
        }
        assert_eq!(hold_of(&r, "A"), Some(Some(Hold::Cooldown)));
        assert!(r.meta.time_rows.iter().all(|t| t.show != "1200"), "no time correction while cooling");
        assert!(r.extra_logs.iter().any(|l| l.starts_with("[異常] 1200") && l.contains("4 則新播出的稿")), "{:?}", r.extra_logs);
        assert_eq!(r.extra_logs.iter().filter(|l| l.starts_with("[可疑] 1200")).count(), 4);
        let dump = r.anomaly_dump.as_deref().expect("anomaly dump");
        assert!(dump.contains("M1模擬稿1200") && dump.contains("\"previous\""), "{dump}");
        assert!(r.meta.warnings.iter().any(|w| w.contains("可疑稿 4 則不寫入")), "{:?}", r.meta.warnings);

        // 12:07 還在搬（同一個錯的樣子）：冷卻中，可疑稿照樣擋。
        let r = st.fetch(&c, &pw, hm(12, 7), auto(false)).unwrap();
        assert!(r.meta.cooling);
        assert_eq!(hold_of(&r, "M1"), Some(Some(Hold::Suspect)));

        // 12:08 製作人搬回去：時間整段跳回來，這一輪也算異常；M1、M2 真的播到了，不再可疑。
        server.set_queue(Q1200, &sim_rows("1200", 42000, &NORMAL));
        let r = st.fetch(&c, &pw, hm(12, 8), auto(false)).unwrap();
        assert!(r.meta.cooling);
        assert_eq!(hold_of(&r, "M1"), Some(Some(Hold::Cooldown)));
        assert_eq!(hold_of(&r, "M2"), Some(Some(Hold::Cooldown)));
        assert_eq!(hold_of(&r, "M3"), None, "M3 (12:10) has not aired yet");
        assert!(!r.meta.warnings.iter().any(|w| w.contains("可疑稿")), "{:?}", r.meta.warnings);

        for m in [9, 10] {
            assert!(st.fetch(&c, &pw, hm(12, m), auto(false)).unwrap().meta.cooling, "12:{m}");
        }
        // 12:11 連續 3 輪穩定：恢復，M1～M3 可以寫（M3 12:10 播出，已經確認過一輪）。
        let r = st.fetch(&c, &pw, hm(12, 11), auto(false)).unwrap();
        assert!(!r.meta.cooling);
        assert!(r.extra_logs.iter().any(|l| l.contains("[異常解除] 1200 連續 3 輪穩定")), "{:?}", r.extra_logs);
        for k in ["A", "E", "M1", "M2", "M3"] {
            assert_eq!(hold_of(&r, k), Some(None), "{k}");
        }
        assert!(r.meta.time_rows.iter().any(|t| t.show == "1200"), "time correction is back");
        // F 12:12 播出：一般流程，等一輪確認。
        let r = st.fetch(&c, &pw, hm(12, 12), auto(false)).unwrap();
        assert_eq!(hold_of(&r, "F"), Some(Some(Hold::Confirm)));
        let r = st.fetch(&c, &pw, hm(12, 13), auto(false)).unwrap();
        assert_eq!(hold_of(&r, "F"), Some(None));
    }

    #[test]
    fn sim_a_move_that_is_never_fixed_keeps_the_stories_out_through_the_sweep() {
        let server = sim_server(&["1200", "1400"]);
        server.set_queue(Q1200, &sim_rows("1200", 42000, &NORMAL));
        server.set_queue(Q1400, &sim_rows("1400", 50100, &["A"]));
        let c = cfg(server.port);
        let mut st = InewsState::default();
        let auto = |start| FetchMode::Auto { session_start: start };
        let pw = creds("secret");
        st.fetch(&c, &pw, hm(12, 4), auto(true)).unwrap();
        st.fetch(&c, &pw, hm(12, 5), auto(false)).unwrap();
        server.set_queue(Q1200, &sim_rows("1200", 42000, &MOVED));
        st.fetch(&c, &pw, hm(12, 6), auto(false)).unwrap();
        st.fetch(&c, &pw, hm(12, 7), auto(false)).unwrap();
        st.fetch(&c, &pw, hm(12, 8), auto(false)).unwrap();
        // 錯的時間一直留著：3 輪穩定後冷卻結束，但可疑稿不放行。
        let r = st.fetch(&c, &pw, hm(12, 9), auto(false)).unwrap();
        assert!(!r.meta.cooling);
        assert_eq!(hold_of(&r, "A"), Some(None));
        for k in ["M1", "M2", "M3", "F"] {
            assert_eq!(hold_of(&r, k), Some(Some(Hold::Suspect)), "{k}");
        }
        // 可疑稿也不拿去校正已經寫進文件的行。
        assert!(!r.meta.time_rows.iter().any(|t| t.slug.starts_with("M1")));

        // 13:56 換到 1400：1200 最後同步；14:06 補掃 1200。可疑稿兩次都擋。
        let r = st.fetch(&c, &pw, hm(13, 56), auto(false)).unwrap();
        assert_eq!(r.meta.switched_from.as_deref(), Some("1200"));
        let m1 = r.meta.stories.iter().find(|s| s.file_name.starts_with("M1模擬稿1200")).unwrap();
        assert_eq!((m1.show.as_str(), m1.hold), ("1200", Some(Hold::Suspect)));
        let r = st.fetch(&c, &pw, hm(14, 6), auto(false)).unwrap();
        assert_eq!(r.meta.closed_show.as_deref(), Some("1200"));
        let swept: Vec<_> = r.meta.stories.iter().filter(|s| s.show == "1200").collect();
        assert!(swept
            .iter()
            .filter(|s| s.file_name.starts_with('M') || s.file_name.starts_with('F'))
            .all(|s| s.hold == Some(Hold::Suspect)));
        assert!(swept.iter().filter(|s| s.file_name.starts_with('E')).all(|s| s.hold.is_none()));
        assert!(r.meta.warnings.iter().any(|w| w.contains("可疑稿 4 則不寫入")), "{:?}", r.meta.warnings);
    }

    #[test]
    fn sim_a_story_that_shows_up_after_the_sweep_is_written_by_the_recheck() {
        let server = sim_server(&["1100", "1200"]);
        let q1100 = "51新聞台.SHOWS.1100.ONAIR";
        server.set_queue(q1100, &sim_rows("1100", 39300, &["A", "B", "C"]));
        server.set_queue(Q1200, &sim_rows("1200", 42900, &["A", "B"]));
        let mut c = cfg(server.port);
        c.slot_spans.clear();
        let mut st = InewsState::default();
        let auto = |start| FetchMode::Auto { session_start: start };
        let pw = creds("secret");

        st.fetch(&c, &pw, hm(11, 0), auto(true)).unwrap();
        st.fetch(&c, &pw, hm(11, 1), auto(false)).unwrap();
        // 11:56 換節（上一節還沒補掃，不在複查範圍），11:57 第二輪：沒有要複查的。
        st.fetch(&c, &pw, hm(11, 56), auto(false)).unwrap();
        let r = st.fetch(&c, &pw, hm(11, 57), auto(false)).unwrap();
        assert!(r.extra_logs.iter().any(|l| l.contains("[複查] （沒有要複查的節次）")), "{:?}", r.extra_logs);
        let r = st.fetch(&c, &pw, hm(12, 6), auto(false)).unwrap();
        assert_eq!(r.meta.closed_show.as_deref(), Some("1100"));

        // 補掃之後，1100 才多出一則 X（11:11 播出）。
        server.set_queue(q1100, &sim_rows("1100", 39300, &["A", "B", "C", "X"]));
        let r = st.fetch(&c, &pw, hm(12, 26), auto(false)).unwrap();
        assert!(hold_of(&r, "X").is_none(), "not due yet");
        let r = st.fetch(&c, &pw, hm(12, 27), auto(false)).unwrap();
        let x = r.meta.stories.iter().find(|s| s.file_name.starts_with("X模擬稿1100")).expect("X picked");
        assert_eq!((x.recheck, x.hold, x.show.as_str()), (true, Some(Hold::Confirm), "1100"));
        assert!(!r.meta.stories.iter().any(|s| s.recheck && !s.file_name.starts_with('X')), "already handled stories are not offered again");
        assert!(r.meta.time_rows.iter().any(|t| t.show == "1100"), "the recheck also corrects times");
        let r = st.fetch(&c, &pw, hm(12, 28), auto(false)).unwrap();
        let x = r.meta.stories.iter().find(|s| s.file_name.starts_with("X模擬稿1100")).expect("X picked");
        assert_eq!((x.recheck, x.hold), (true, None));
        assert!(
            r.extra_logs.iter().any(|l| l == "[複查] 1100：補掃後才出現的已播出稿 1 則（X模擬稿1100），寫在文件最下面"),
            "{:?}",
            r.extra_logs
        );
        let meta = r.meta.recheck.as_ref().expect("recheck meta");
        assert_eq!(meta.shows, ["1100", "1200"]);
        assert!(meta.aired_keys.contains(&x.key));
        let r = st.fetch(&c, &pw, hm(12, 29), auto(false)).unwrap();
        assert!(hold_of(&r, "X").is_none() && r.meta.recheck.is_none());
    }
}
