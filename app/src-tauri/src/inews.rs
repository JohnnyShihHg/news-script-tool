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
use news_script_core::inews::{self, ListEntry, ListKind, NsmlStory, RundownRow};
use news_script_core::parse;
use serde::Serialize;
use std::collections::HashMap;
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

struct CachedStory {
    story: NsmlStory,
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
}

pub struct FetchResult {
    pub files: Vec<(String, String)>,
    pub meta: InewsMeta,
    /// 決策紀錄的這一行（不含時間戳）。
    pub log_line: String,
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
                    self.cache.insert(entry.name.clone(), CachedStory { story: story.clone() });
                    story
                }
            };
            out.push((entry, story));
        }
        Ok(out)
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
        if let FetchMode::Auto { session_start } = mode {
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
                        extra.extend(s.rows.iter().map(|&i| Pick { show: last.clone(), index: i, cume: s.cume[i] }));
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
                extra.extend(s.rows.iter().map(|&i| Pick { show: w.slot.show.clone(), index: i, cume: s.cume[i] }));
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
            .chain(sel.rows.iter().map(|&i| Pick { show: cur.slot.show.clone(), index: i, cume: sel.cume[i] }))
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
            },
        })
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
        let Some(date) = name.strip_prefix("inews-").and_then(|n| n.strip_suffix(".log")) else { continue };
        if let Ok(d) = chrono::NaiveDate::parse_from_str(date, "%Y%m%d") {
            if (today - d).num_days() >= LOG_KEEP_DAYS {
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
            std::thread::spawn(move || {
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
                        log2.lock().unwrap().push(raw.clone());
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
                            "CWD" if queues.contains_key(arg) => {
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
                                    queues.get(&cwd).cloned().filter(|b| !b.is_empty())
                                } else {
                                    *retrs2.lock().unwrap() += 1;
                                    stories.get(arg).cloned()
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
            FakeServer { port, log, retrs }
        }

        fn commands(&self) -> Vec<String> {
            self.log.lock().unwrap().iter().map(|l| String::from_utf8_lossy(l).trim_end().to_string()).collect()
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
        assert!(server.log.lock().unwrap().iter().all(|l| !l.starts_with(&[0xEF, 0xBB, 0xBF]) && l.ends_with(b"\r\n")));
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
        // 0800 全部 7 則 + 為了算 0800 的結束時間讀的 0900 的 2 則。
        assert_eq!(*server.retrs.lock().unwrap(), 9);

        // 第二輪：每個 queue LIST 一次，沒有任何變動就不 RETR。
        let r = state.fetch(&cfg(server.port), &creds("secret"), 8 * 3600 + 21 * 60, FetchMode::Manual).unwrap();
        assert_eq!(r.meta.retrieved, 0);
        assert_eq!(*server.retrs.lock().unwrap(), 9);
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
        // 09:05 補掃 0800：不看現在時間，累積時間 < 08:55:00 的全部納入；分隔線可以寫了。
        let r = state.fetch(&c, &creds("secret"), 9 * 3600 + 5 * 60, auto(false)).unwrap();
        assert_eq!(r.meta.closed_show.as_deref(), Some("0800"));
        let swept: Vec<&str> = r.meta.stories.iter().filter(|s| s.show == "0800").map(|s| s.file_name.as_str()).collect();
        assert_eq!(swept.len(), 5);
        assert!(!r.files.iter().any(|(_, t)| t.contains("合成預備稿0800")));
        assert!(r.log_line.contains("補掃 0800"), "{}", r.log_line);
        // 只補掃一次。
        let r = state.fetch(&c, &creds("secret"), 9 * 3600 + 11 * 60, auto(false)).unwrap();
        assert_eq!(r.meta.closed_show, None);
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

    #[test]
    fn old_logs_are_pruned_after_fourteen_days() {
        let dir = std::env::temp_dir().join(format!("nst-log-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for d in ["20261001", "20261011", "20261012", "20261025"] {
            std::fs::write(dir.join(format!("inews-{d}.log")), "x").unwrap();
        }
        std::fs::write(dir.join("other.txt"), "x").unwrap();
        prune_logs(&dir, "20261025");
        let mut left: Vec<String> = std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().to_string()).collect();
        left.sort();
        assert_eq!(left, ["inews-20261012.log", "inews-20261025.log", "other.txt"]);
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

}
