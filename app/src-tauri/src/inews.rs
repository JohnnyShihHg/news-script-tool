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

/// 自動匯入在兩輪之間保留的東西：控制連線、節次清單、已經 RETR 過的稿件。
#[derive(Default)]
pub struct InewsState {
    session: Option<FtpSession>,
    session_key: String,
    shows: Option<(Instant, Vec<String>)>,
    /// key = LIST 上的完整名稱（含 ID，ID 第三段是修改時間戳）：稿件一改就換 key，
    /// 自然會重新 RETR。
    cache: HashMap<String, CachedStory>,
}

/// 一則被選中的稿件，給前端做單則變動偵測。
#[derive(Serialize, Clone, Debug)]
pub struct StoryMeta {
    pub file_name: String,
    pub key: String,
    pub version: String,
    pub cume: String,
    pub floated: bool,
}

#[derive(Serialize, Clone, Debug)]
pub struct InewsMeta {
    pub show: String,
    pub show_label: String,
    pub queue_path: String,
    pub now: String,
    pub switched_early: bool,
    /// "after_preview" | "last_prefixed" | "none"
    pub cutoff_rule: String,
    pub cutoff_title: Option<String>,
    pub list_count: usize,
    pub retrieved: usize,
    pub pending: usize,
    pub stories: Vec<StoryMeta>,
    pub warnings: Vec<String>,
}

pub struct FetchResult {
    pub files: Vec<(String, String)>,
    pub meta: InewsMeta,
}

/// 現在是台北時間的當天第幾秒。跟電腦的時區設定無關：rundown 的時間是台北時間。
pub fn taipei_seconds_of_day() -> u32 {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    ((now + 8 * 3600) % 86_400) as u32
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
    fn read_queue(
        &mut self,
        cfg: &InewsConfig,
        creds: &Credentials,
        path: &str,
        stop_at_first_hard: bool,
        retrieved: &mut usize,
    ) -> FtpResult<Vec<(ListEntry, NsmlStory)>> {
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
            let hard = story.hard_time().is_some();
            out.push((entry, story));
            if stop_at_first_hard && hard {
                break;
            }
        }
        Ok(out)
    }

    fn fetch_once(&mut self, cfg: &InewsConfig, creds: &Credentials, now: u32) -> FtpResult<FetchResult> {
        let names = self.show_names(cfg, creds)?;
        let ((mut show, mut start), next) = inews::current_and_next(&names, now)
            .ok_or_else(|| FtpError::Protocol(format!("{} 底下找不到 4 位數的節次資料夾", cfg.root)))?;
        let queue_of = |show: &str| format!("{}.{}.{}", cfg.root, show, cfg.queue);
        let mut retrieved = 0;
        let mut switched_early = false;
        let mut kept = Vec::new();

        if let Some((next_show, next_start)) = next {
            if inews::seconds_until(next_start, now) <= inews::NEXT_SHOW_PROBE_SECONDS {
                let probe = self.read_queue(cfg, creds, &queue_of(&next_show), true, &mut retrieved)?;
                let first_hard = probe.iter().find_map(|(_, s)| s.hard_time());
                kept.extend(probe.into_iter().map(|(e, _)| e.name));
                if inews::should_switch_early(next_start, now, first_hard, cfg.lead_minutes) {
                    show = next_show;
                    start = next_start;
                    switched_early = true;
                }
            }
        }

        let queue_path = queue_of(&show);
        let stories = self.read_queue(cfg, creds, &queue_path, false, &mut retrieved)?;
        kept.extend(stories.iter().map(|(e, _)| e.name.clone()));
        // 快取只留這一輪還看得到的稿件，換節後舊的就放掉。
        self.cache.retain(|name, _| kept.contains(name));

        let rows: Vec<RundownRow> = stories.iter().map(|(e, s)| RundownRow::from_story(s, e.floated)).collect();
        let sel = inews::select_rows(
            &rows,
            now,
            start,
            &cfg.cutoff_preview_keyword,
            &cfg.cutoff_page_prefix,
            cfg.include_floated,
        );

        let mut warnings = Vec::new();
        if sel.rule == inews::CutoffRule::None {
            warnings.push(format!(
                "找不到截止點（標題含「{}」之後序號以「{}」開頭的列），整節都會處理，預備稿可能也會被匯入。",
                cfg.cutoff_preview_keyword, cfg.cutoff_page_prefix
            ));
        }
        if !rows.is_empty() && rows.iter().all(|r| r.hard.is_none()) {
            warnings.push("這一節的 rundown 沒有任何硬時間（＠），算不出累積時間，暫時不匯入任何稿件。".into());
        }

        let titles: Vec<String> = sel.rows.iter().map(|&i| stories[i].1.title().to_string()).collect();
        let file_names = inews::unique_file_names(&titles);
        let mut files = Vec::with_capacity(sel.rows.len());
        let mut metas = Vec::with_capacity(sel.rows.len());
        for (&i, file_name) in sel.rows.iter().zip(file_names) {
            let (entry, story) = &stories[i];
            let cume = sel.cume[i].map(inews::format_clock).unwrap_or_default();
            let fill = (cfg.fill_computed_time && !cume.is_empty()).then_some(cume.as_str());
            files.push((file_name.clone(), inews::story_to_text(story, fill)));
            metas.push(StoryMeta {
                file_name,
                key: entry.story_key(),
                version: entry.version().to_string(),
                cume,
                floated: entry.floated,
            });
        }

        Ok(FetchResult {
            files,
            meta: InewsMeta {
                show_label: inews::show_hour_label(&show),
                show,
                queue_path,
                now: inews::format_clock(now as i64),
                switched_early,
                cutoff_rule: match sel.rule {
                    inews::CutoffRule::AfterPreview => "after_preview",
                    inews::CutoffRule::LastPrefixed => "last_prefixed",
                    inews::CutoffRule::None => "none",
                }
                .into(),
                cutoff_title: sel.cutoff.map(|i| rows[i].title.clone()),
                list_count: stories.len(),
                retrieved,
                pending: sel.pending,
                stories: metas,
                warnings,
            },
        })
    }

    /// 一輪自動匯入。連線斷了（伺服器閒置踢人、網路閃斷）就重連再試一次。
    pub fn fetch(&mut self, cfg: &InewsConfig, creds: &Credentials, now: u32) -> Result<FetchResult, String> {
        match self.fetch_once(cfg, creds, now) {
            Ok(r) => Ok(r),
            // 421 = 伺服器閒置踢人；530 = 舊連線的登入狀態失效。都重新登入再試。
            Err(FtpError::Io(_)) | Err(FtpError::Protocol(_)) if self.session.is_some() => {
                self.drop_session();
                self.fetch_once(cfg, creds, now).map_err(|e| e.to_string())
            }
            Err(FtpError::Reply { ref reply, .. }) if self.session.is_some() && matches!(reply.code, 421 | 530) => {
                self.drop_session();
                self.fetch_once(cfg, creds, now).map_err(|e| e.to_string())
            }
            Err(e) => {
                // 登入失敗之類的：下一輪重新連線，不要卡在壞掉的連線上。
                self.drop_session();
                Err(e.to_string())
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

    #[test]
    fn a_round_imports_only_aired_rows_above_the_cutoff() {
        let server = two_shows(None);
        let mut state = InewsState::default();
        // 08:20：大片頭 07:55、早安稿 07:56、第二則 07:58、預告 07:59:30 都播過了。
        let r = state.fetch(&cfg(server.port), &creds("secret"), 8 * 3600 + 20 * 60).unwrap();
        assert_eq!(r.meta.show, "0800");
        assert!(!r.meta.switched_early);
        assert_eq!(r.meta.cutoff_rule, "after_preview");
        assert_eq!(r.meta.cutoff_title.as_deref(), Some("=CM 3=="));
        let names: Vec<&str> = r.files.iter().map(|(n, _)| n.as_str()).collect();
        // float 的框不輸出；截止點（=CM 3==）以下的預備稿不處理。
        assert_eq!(names, ["大片頭.txt", "合成早安稿0800.txt", "合成第二則0800.txt", "下節預告三.txt"]);
        let early = &r.files[1].1;
        assert!(early.contains("\r\n累積時間: 07:56:00\r\n"), "computed cume filled in");
        assert_eq!(r.meta.stories[1].key, "0000000A:00000002");
        assert_eq!(r.meta.stories[1].version, "00000A02");
        assert_eq!(*server.retrs.lock().unwrap(), 7);

        // 第二輪：LIST 一次，沒有任何變動就不 RETR。
        let r = state.fetch(&cfg(server.port), &creds("secret"), 8 * 3600 + 21 * 60).unwrap();
        assert_eq!(r.meta.retrieved, 0);
        assert_eq!(*server.retrs.lock().unwrap(), 7);
        let lists = server.commands().iter().filter(|c| *c == "LIST").count();
        assert_eq!(lists, 3, "root once (cached), the queue once per round");
    }

    #[test]
    fn the_next_show_takes_over_once_its_first_hard_time_passes() {
        let server = two_shows(None);
        let mut state = InewsState::default();
        let r = state.fetch(&cfg(server.port), &creds("secret"), 8 * 3600 + 54 * 60).unwrap();
        assert_eq!(r.meta.show, "0800");
        // 08:55:30：0900 的大片頭（08:55:00）已經開始，九點稿（08:56:00）還沒。
        let r = state.fetch(&cfg(server.port), &creds("secret"), 8 * 3600 + 55 * 60 + 30).unwrap();
        assert_eq!(r.meta.show, "0900");
        assert!(r.meta.switched_early);
        assert_eq!(r.meta.show_label, "09");
        assert_eq!(r.files.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>(), ["大片頭.txt"]);
    }

    #[test]
    fn a_dropped_connection_is_reconnected_transparently() {
        // 第 8 個指令（進 queue 的 CWD）時伺服器直接斷線，像閒置被踢掉一樣。
        let server = two_shows(Some(8));
        let mut state = InewsState::default();
        let r = state.fetch(&cfg(server.port), &creds("secret"), 8 * 3600 + 20 * 60).unwrap();
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
        let r = InewsState::default().fetch(&cfg(server.port), &creds("secret"), 8 * 3600).unwrap();
        assert!(r.files.is_empty());
        assert_eq!(r.meta.list_count, 0);
    }
}
