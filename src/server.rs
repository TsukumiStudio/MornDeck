use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::Duration;

use alacritty_terminal::event::{Event, EventListener, WindowSize};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::test::TermSize;
use alacritty_terminal::term::{Config, TermMode};
use alacritty_terminal::vte::ansi::{Color as AColor, CursorShape, NamedColor, Processor, Rgb};
use alacritty_terminal::Term;
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use ratatui::backend::CrosstermBackend;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::{Frame, Terminal, TerminalOptions, Viewport};
use serde_json::{json, Value};

use crate::hooks;

const SIDEBAR_MIN: u16 = 12;
// F12: control+] は Warp などが横取りし、ssh 越しに届かないことがある
const PREFIX: &[u8] = b"\x1b[24~";

pub const IN_INPUT: u8 = 0;
pub const IN_RESIZE: u8 = 1;

#[derive(Clone)]
struct Listener(Sender<Event>);
impl EventListener for Listener {
    fn send_event(&self, e: Event) {
        let _ = self.0.send(e);
    }
}

struct Tab {
    id: u32,
    term: Term<Listener>,
    parser: Processor,
    events: Receiver<Event>,
    writer: Box<dyn Write + Send>,
    master: Box<dyn MasterPty + Send>,
    child: Box<dyn Child + Send + Sync>,
    title: String,
    name: Option<String>,
    agent: Option<String>,
    state: Option<&'static str>,
    answer: Option<String>,
    answer_seq: u64,
    question: Value,
    /// 復元時にエージェントの会話を再開するための情報
    session: Option<String>,
    args: Option<String>,
    /// API や復元でエージェントを起動し、まだ SessionStart が届いていない
    starting: bool,
    /// タブを開いた Bot。まとめて閉じるときに使う
    owner: Option<String>,
    last_used: u64,
    /// 送り直しを無視するための依頼 ID と、そのとき返した expect_seq
    requests: Vec<(String, u64)>,
}

fn now_secs() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

#[derive(Default)]
struct State {
    tabs: Vec<Tab>,
    active: usize,
    cols: u16,
    rows: u16,
    next_id: u32,
    next_client: u64,
    clients: Vec<(u64, Sender<Vec<u8>>)>,
    subs: Vec<Sender<String>>,
    dirty: bool,
    full_redraw: bool,
    sock: String,
    hooks: Vec<(&'static str, hooks::Status)>,
    menu: Option<Menu>,
    prefix: bool,
    sidebar: u16,
    webhooks: Vec<Value>,
    rename: Option<Rename>,
    keys: Keys,
}

struct Rename {
    tab: u32,
    buf: String,
    error: Option<String>,
}

struct Menu {
    sel: usize,
    notice: Option<String>,
    /// アップデート確認で見つけた、今より新しい版
    update: Option<String>,
    /// キー設定の画面。Some の間はこちらが入力を受ける
    keys: Option<KeyEdit>,
}

struct KeyEdit {
    /// ACTIONS の番号。ACTIONS.len() は「既定に戻す」
    sel: usize,
    /// 次に押したキーを割り当てる
    capture: bool,
}

/// F12 の後に押すキー (名前, 表示名, 既定)。メニューで変え、sock の隣の keys.json に保存する。
const ACTIONS: [(&str, &str, u8); 7] = [
    ("new", "新規タブ", b'c'),
    ("rename", "名前", b'r'),
    ("close", "タブを閉じる", b'x'),
    ("next", "次", b'n'),
    ("prev", "前", b'p'),
    ("menu", "メニュー", b'm'),
    ("detach", "離脱", b'd'),
];
type Keys = [u8; ACTIONS.len()];

fn keys_path(sock: &str) -> std::path::PathBuf {
    std::path::Path::new(sock).with_file_name("keys.json")
}

/// 壊れた値や重複は既定に戻す。
fn parse_keys(v: &Value) -> Keys {
    let mut k = ACTIONS.map(|a| a.2);
    for (i, (name, _, _)) in ACTIONS.iter().enumerate() {
        if let Some(&[c]) = v[name].as_str().map(str::as_bytes)
            && c.is_ascii_graphic()
        {
            k[i] = c;
        }
    }
    if (0..k.len()).any(|i| k[i + 1..].contains(&k[i])) {
        return ACTIONS.map(|a| a.2);
    }
    k
}

fn load_keys(sock: &str) -> Keys {
    let v = std::fs::read_to_string(keys_path(sock)).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
    parse_keys(&v)
}

fn save_keys(sock: &str, k: &Keys) -> std::io::Result<()> {
    let v: serde_json::Map<String, Value> =
        ACTIONS.iter().zip(k).map(|((name, _, _), c)| (name.to_string(), json!((*c as char).to_string()))).collect();
    std::fs::write(keys_path(sock), Value::Object(v).to_string())
}

const MENU_LEN: usize = 6;
// メニュー項目の並び。0 と 1 は hooks::status() の順 (claude, codex)
const M_UPDATE: usize = 2;
const M_SIDEBAR: usize = 3;
const M_KEYS: usize = 4;
const RELEASES_URL: &str = "https://api.github.com/repos/TsukumiStudio/MornDeck/releases/latest";
const UPGRADE_CMD: &str = "HOMEBREW_NO_AUTOREMOVE=1 brew upgrade --cask morndeck && echo '更新した。mdeck サーバーの再起動で反映 (動いている claude / codex は終了する)'";

fn hook_item(hooks: &[(&str, hooks::Status)], i: usize) -> String {
    let Some((a, s)) = hooks.get(i) else { return String::new() };
    let verb = match s {
        hooks::Status::NoAgent => "",
        hooks::Status::Missing => "  Enter で導入",
        hooks::Status::Outdated(_) => "  Enter で更新",
        hooks::Status::Current => "  Enter で入れ直す",
    };
    format!("{a} のフック: {}{verb}", s.label())
}

fn menu_items(m: &Menu, sidebar: u16, hooks: &[(&str, hooks::Status)]) -> [String; MENU_LEN] {
    [
        hook_item(hooks, 0),
        hook_item(hooks, 1),
        match &m.update {
            Some(v) => format!("v{v} に更新する (brew upgrade)"),
            None => "アップデートを確認".into(),
        },
        format!("サイドバーの幅  ← {sidebar} →"),
        "キー設定".into(),
        "閉じる".into(),
    ]
}

fn version_parts(v: &str) -> Vec<u32> {
    v.trim_start_matches('v').split('.').map(|p| p.parse().unwrap_or(0)).collect()
}

/// GitHub の最新リリースの版。新しいものがあれば Some。
fn check_update() -> Result<Option<String>, String> {
    let out = std::process::Command::new("curl")
        .args(["-fsSL", "-m", "10", "-H", "Accept: application/vnd.github+json", RELEASES_URL])
        .output()
        .map_err(|e| format!("curl を起動できない: {e}"))?;
    if !out.status.success() {
        return Err(format!("取得できない: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    let v: Value = serde_json::from_slice(&out.stdout).map_err(|e| e.to_string())?;
    let tag = v["tag_name"].as_str().ok_or("tag_name が無い")?;
    let latest = tag.trim_start_matches('v').to_string();
    Ok((version_parts(&latest) > version_parts(env!("CARGO_PKG_VERSION"))).then_some(latest))
}

type Shared = Arc<(Mutex<State>, Condvar)>;

fn lock(s: &Shared) -> MutexGuard<'_, State> {
    s.0.lock().unwrap_or_else(|e| e.into_inner())
}

impl State {
    fn inner_size(&self) -> (u16, u16) {
        (self.cols.saturating_sub(self.sidebar).max(2), self.rows.max(1))
    }
    fn tab(&mut self, id: u32) -> Option<&mut Tab> {
        self.tabs.iter_mut().find(|t| t.id == id)
    }
    fn emit(&mut self, v: Value) {
        let line = v.to_string();
        self.subs.retain(|s| s.send(line.clone()).is_ok());
    }
    fn tab_json(&self, i: usize) -> Value {
        let t = &self.tabs[i];
        json!({
            "id": t.id, "name": t.name, "title": t.title, "agent": t.agent, "state": status(t), "active": i == self.active,
            "owner": t.owner, "cwd": t.child.process_id().and_then(proc_cwd), "last_used": t.last_used,
        })
    }
    /// API の `tab` は番号 (id) でも名前でもよい。
    fn find(&self, v: &Value) -> Result<usize, String> {
        let i = match v {
            Value::Number(n) => self.tabs.iter().position(|t| Some(t.id as u64) == n.as_u64()),
            Value::String(name) => self.tabs.iter().position(|t| t.name.as_deref() == Some(name.as_str())),
            _ => return Err("tab (番号か名前) が必要".into()),
        };
        i.ok_or_else(|| format!("no such tab: {v}"))
    }
    fn check_name(&self, name: &str, except: Option<u32>) -> Result<(), String> {
        validate_name(name, self.tabs.iter().filter(|t| Some(t.id) != except).filter_map(|t| t.name.as_deref()))
    }
    /// subscribe に流し、done / choices は webhook にも送る。
    fn notify(&mut self, v: Value) {
        if v["event"] == "choices" || v["state"] == "done" {
            post_webhooks(&self.webhooks, &v);
        }
        self.emit(v);
    }
}

fn mark(s: &Shared, st: &mut State) {
    st.dirty = true;
    s.1.notify_one();
}

/// 生きているサーバーのソケットは奪わない。応答の無い残骸だけ消して bind する。
fn bind_sock(sock: &str) -> std::io::Result<UnixListener> {
    if UnixStream::connect(sock).is_ok() {
        return Err(std::io::Error::new(std::io::ErrorKind::AddrInUse, format!("server already running on {sock}")));
    }
    let _ = std::fs::remove_file(sock);
    UnixListener::bind(sock)
}

pub fn run(sock: &str) -> std::io::Result<()> {
    let listener = bind_sock(sock)?;
    let shared: Shared = Arc::new((
        Mutex::new(State {
            cols: 80,
            rows: 24,
            next_id: 1,
            sock: sock.into(),
            hooks: hooks::status(),
            sidebar: 24,
            webhooks: load_webhooks(sock),
            keys: load_keys(sock),
            ..Default::default()
        }),
        Condvar::new(),
    ));
    restore(&shared);
    if lock(&shared).tabs.is_empty() {
        new_tab(&shared, None, None, None, true).map_err(std::io::Error::other)?;
    }
    let r = shared.clone();
    std::thread::spawn(move || render_loop(r));
    let r = shared.clone();
    std::thread::spawn(move || save_loop(r));

    for conn in listener.incoming().flatten() {
        let s = shared.clone();
        std::thread::spawn(move || {
            let _ = handle_conn(s, conn);
        });
    }
    Ok(())
}

fn new_tab(s: &Shared, cwd: Option<&str>, run: Option<&str>, name: Option<String>, focus: bool) -> Result<u32, String> {
    let mut st = lock(s);
    if let Some(n) = &name {
        st.check_name(n, None)?;
    }
    let (cols, rows) = st.inner_size();
    let id = st.next_id;
    st.next_id += 1;
    let pair = native_pty_system()
        .openpty(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })
        .map_err(|e| e.to_string())?;
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into());
    let mut cmd = CommandBuilder::new(&shell);
    cmd.arg("-l");
    let home = std::env::var("HOME").unwrap_or_else(|_| "/".into());
    cmd.cwd(cwd.unwrap_or(&home));
    cmd.env("TERM", "xterm-256color");
    cmd.env("COLORTERM", "truecolor");
    cmd.env("MDECK_TAB", id.to_string());
    cmd.env("MDECK_SOCK", &st.sock);
    // 親 (herdr / Claude Code 内から起動した場合) の識別子を子へ漏らすと、そちらのフックが反応する
    for (k, _) in std::env::vars() {
        if k.starts_with("HERDR_") || k.starts_with("CLAUDE_CODE_") || k == "CLAUDECODE" || k == "TMUX" {
            cmd.env_remove(k);
        }
    }
    let child = pair.slave.spawn_command(cmd).map_err(|e| e.to_string())?;
    drop(pair.slave);
    let mut reader = pair.master.try_clone_reader().map_err(|e| e.to_string())?;
    let mut writer = pair.master.take_writer().map_err(|e| e.to_string())?;
    if let Some(r) = run {
        let _ = writer.write_all(format!("{r}\r").as_bytes());
    }
    let (tx, rx) = channel();
    let term = Term::new(
        Config { scrolling_history: 10000, ..Default::default() },
        &TermSize::new(cols as usize, rows as usize),
        Listener(tx),
    );
    st.tabs.push(Tab {
        id,
        term,
        parser: Processor::new(),
        events: rx,
        writer,
        master: pair.master,
        child,
        title: name.clone().unwrap_or_else(|| format!("tab {id}")),
        name,
        agent: None,
        state: None,
        answer: None,
        answer_seq: 0,
        question: Value::Null,
        session: None,
        args: None,
        starting: false,
        owner: None,
        last_used: now_secs(),
        requests: Vec::new(),
    });
    if focus || st.tabs.len() == 1 {
        st.active = st.tabs.len() - 1;
    }
    let j = st.tab_json(st.tabs.len() - 1);
    st.emit(json!({"event": "tab_new", "tab": j}));
    mark(s, &mut st);
    drop(st);

    let s2 = s.clone();
    std::thread::spawn(move || {
        let mut buf = vec![0u8; 65536];
        loop {
            let n = match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => n,
            };
            let mut st = lock(&s2);
            let (cols, rows) = st.inner_size();
            let Some(t) = st.tab(id) else { break };
            t.parser.advance(&mut t.term, &buf[..n]);
            while let Ok(e) = t.events.try_recv() {
                handle_term_event(t, e, cols, rows);
            }
            mark(&s2, &mut st);
        }
        let mut st = lock(&s2);
        if let Some(i) = st.tabs.iter().position(|t| t.id == id) {
            let mut t = st.tabs.remove(i);
            let _ = t.child.try_wait();
            if st.active > i || st.active >= st.tabs.len() {
                st.active = st.active.saturating_sub(1);
            }
            st.emit(json!({"event": "tab_closed", "id": id}));
            mark(&s2, &mut st);
        }
    });
    Ok(id)
}

fn handle_term_event(t: &mut Tab, e: Event, cols: u16, rows: u16) {
    match e {
        Event::PtyWrite(s) => {
            let _ = t.writer.write_all(s.as_bytes());
        }
        Event::Title(s) => t.title = s,
        Event::ResetTitle => t.title = t.name.clone().unwrap_or_else(|| format!("tab {}", t.id)),
        Event::TextAreaSizeRequest(f) => {
            let ws = WindowSize { num_lines: rows, num_cols: cols, cell_width: 8, cell_height: 16 };
            let _ = t.writer.write_all(f(ws).as_bytes());
        }
        // ponytail: 実際のクライアント配色は分からないので暗背景を固定で答える
        Event::ColorRequest(i, f) => {
            let c = if i == 257 { Rgb { r: 0, g: 0, b: 0 } } else { Rgb { r: 0xd0, g: 0xd0, b: 0xd0 } };
            let _ = t.writer.write_all(f(c).as_bytes());
        }
        _ => {}
    }
}

fn resize(s: &Shared, cols: u16, rows: u16) {
    let mut st = lock(s);
    if (st.cols, st.rows) == (cols, rows) {
        return;
    }
    st.cols = cols;
    st.rows = rows;
    relayout(s, &mut st);
}

fn set_sidebar(s: &Shared, st: &mut State, w: i32) {
    let w = w.clamp(SIDEBAR_MIN as i32, (st.cols as i32 - 20).max(SIDEBAR_MIN as i32)) as u16;
    if w != st.sidebar {
        st.sidebar = w;
        relayout(s, st);
    }
}

fn relayout(s: &Shared, st: &mut State) {
    let (c, r) = st.inner_size();
    for t in &mut st.tabs {
        t.term.resize(TermSize::new(c as usize, r as usize));
        let _ = t.master.resize(PtySize { rows: r, cols: c, pixel_width: 0, pixel_height: 0 });
    }
    st.full_redraw = true;
    mark(s, st);
}

// ---------- rendering ----------

fn render_loop(s: Shared) {
    let mut term: Option<Terminal<CrosstermBackend<Vec<u8>>>> = None;
    let mut size = (0, 0);
    let mut modes = None;
    loop {
        {
            let mut st = lock(&s);
            while !st.dirty {
                st = s.1.wait(st).unwrap_or_else(|e| e.into_inner());
            }
        }
        std::thread::sleep(Duration::from_millis(4)); // 連続出力をまとめて 1 フレームにする
        let mut st = lock(&s);
        st.dirty = false;
        let full = std::mem::take(&mut st.full_redraw) || size != (st.cols, st.rows) || term.is_none();
        let mut out = b"\x1b[?2026h".to_vec();
        if full {
            size = (st.cols, st.rows);
            let t = Terminal::with_options(
                CrosstermBackend::new(Vec::new()),
                TerminalOptions { viewport: Viewport::Fixed(Rect::new(0, 0, size.0, size.1)) },
            )
            .expect("vec backend");
            // Terminal::clear は実端末へカーソル位置を問い合わせるため tty の無いサーバでは失敗する。自前で消す
            out.extend_from_slice(b"\x1b[0m\x1b[H\x1b[2J");
            term = Some(t);
            modes = None;
        }
        let t = term.as_mut().unwrap();
        let _ = t.draw(|f| draw(f, &st));
        out.append(t.backend_mut().writer_mut());
        // 外側の端末が送るキー列 (矢印・ペースト) を中のアプリのモードに合わせる
        let m = st.tabs.get(st.active).map(|t| *t.term.mode()).unwrap_or(TermMode::empty());
        let want = (m.contains(TermMode::APP_CURSOR), m.contains(TermMode::BRACKETED_PASTE));
        if modes != Some(want) {
            out.extend_from_slice(if want.0 { b"\x1b[?1h" } else { b"\x1b[?1l" });
            out.extend_from_slice(if want.1 { b"\x1b[?2004h" } else { b"\x1b[?2004l" });
            modes = Some(want);
        }
        out.extend_from_slice(b"\x1b[?2026l");
        st.clients.retain(|(_, c)| c.send(out.clone()).is_ok());
    }
}

fn state_glyph(state: Option<&str>) -> (&'static str, Color, &'static str) {
    match state {
        Some("working") => ("●", Color::Yellow, "考え中"),
        Some("waiting") => ("◆", Color::LightRed, "入力待ち"),
        Some("done") => ("✓", Color::LightGreen, "完了"),
        Some(_) => ("○", Color::DarkGray, "待機"),
        None => (" ", Color::Reset, ""),
    }
}

fn draw(f: &mut Frame, st: &State) {
    let area = f.area();
    let side = Rect::new(0, 0, st.sidebar.min(area.width), area.height);
    let main = Rect::new(side.width, 0, area.width - side.width, area.height);
    let buf = f.buffer_mut();
    let bg = Style::default().bg(Color::Indexed(235));
    buf.set_style(side, bg);
    let footer = 3;
    for (i, t) in st.tabs.iter().enumerate() {
        let y = i as u16;
        if y >= side.height.saturating_sub(footer) {
            break;
        }
        let (g, gc, label) = state_glyph(t.state);
        let base = if i == st.active { bg.bg(Color::Indexed(24)).add_modifier(Modifier::BOLD) } else { bg };
        buf.set_style(Rect::new(0, y, side.width, 1), base);
        let (icon, ic) = match t.agent.as_deref().or_else(|| fg_agent(t.master.process_group_leader())) {
            Some("claude") => ("✻", Color::Rgb(217, 119, 87)),
            Some("codex") => ("❖", Color::Rgb(16, 163, 127)),
            _ => (" ", Color::Reset),
        };
        buf.set_stringn(1, y, icon, 1, base.fg(ic));
        buf.set_stringn(2, y, g, 1, base.fg(gc));
        let title = t.name.as_ref().unwrap_or(&t.title);
        let name = if label.is_empty() { title.clone() } else { format!("{label} {title}") };
        buf.set_stringn(4, y, name, side.width.saturating_sub(5) as usize, base);
    }
    let h = side.height;
    buf.set_stringn(0, h.saturating_sub(1), "F12 で操作一覧", side.width as usize, bg.fg(Color::Indexed(250)));
    let warn = if st.hooks.iter().any(|(_, s)| matches!(s, hooks::Status::Outdated(_))) {
        Some("! フックが古い")
    } else if st.hooks.iter().any(|(_, s)| *s == hooks::Status::Missing) {
        Some("! フック未導入")
    } else {
        None
    };
    if let Some(w) = warn {
        let warn = bg.fg(Color::Yellow);
        buf.set_stringn(0, h.saturating_sub(3), w, side.width as usize, warn);
        buf.set_stringn(0, h.saturating_sub(2), format!("  F12 → {} で開く", key(st, "menu")), side.width as usize, warn);
    }

    match st.tabs.get(st.active) {
        Some(t) => {
            if let Some(cur) = draw_term(buf, main, &t.term) {
                f.set_cursor_position(cur);
            }
        }
        None => {
            buf.set_string(main.x + 1, 0, format!("タブがない。F12 → {} で新規", key(st, "new")), Style::default());
        }
    }
    if let Some(m) = &st.menu {
        draw_menu(f, main, m, st);
    }
    if st.prefix {
        let bar = Rect::new(0, area.height.saturating_sub(1), area.width, 1);
        let on = Style::default().bg(Color::Yellow).fg(Color::Black);
        let list: Vec<String> = ACTIONS.iter().zip(st.keys).map(|((_, label, _), k)| format!("{}){label}", k as char)).collect();
        let keys = format!(" F12 › {}  F12)閉じる", list.join("  "));
        f.buffer_mut().set_style(bar, on);
        f.buffer_mut().set_stringn(0, bar.y, keys, area.width as usize, on);
    }
    if let Some(r) = &st.rename {
        let bar = Rect::new(0, area.height.saturating_sub(1), area.width, 1);
        let on = Style::default().bg(Color::Yellow).fg(Color::Black);
        let hint = r.error.as_deref().unwrap_or("Enter 決定  Esc 取消  空で名前を外す");
        let line = format!(" タブ名: {}▏  {hint}", r.buf);
        f.buffer_mut().set_style(bar, on);
        f.buffer_mut().set_stringn(0, bar.y, line, area.width as usize, on);
    }
}

fn key(st: &State, action: &str) -> char {
    ACTIONS.iter().position(|a| a.0 == action).map_or('?', |i| st.keys[i] as char)
}

/// 矢印などのエスケープ列は丸ごと 1 つとして数える。
fn esc_len(rest: &[u8]) -> usize {
    if rest.get(1) == Some(&b'[') || rest.get(1) == Some(&b'O') {
        rest.iter().skip(2).position(|c| c.is_ascii_alphabetic() || *c == b'~').map_or(rest.len(), |p| p + 3)
    } else {
        1
    }
}

fn rename_input(s: &Shared, rest: &[u8]) -> usize {
    let mut st = lock(s);
    let Some(r) = st.rename.as_mut() else { return 0 };
    let used = match rest[0] {
        b'\r' => {
            let (tab, name) = (r.tab, r.buf.clone());
            let res = if name.is_empty() { Ok(()) } else { st.check_name(&name, Some(tab)) };
            match res {
                Ok(()) => {
                    if let Some(t) = st.tab(tab) {
                        t.name = (!name.is_empty()).then_some(name);
                    }
                    st.rename = None;
                }
                Err(e) => st.rename.as_mut().unwrap().error = Some(e),
            }
            1
        }
        0x1b => {
            st.rename = None;
            esc_len(rest)
        }
        0x7f | 0x08 => {
            r.buf.pop();
            1
        }
        c if c < 0x20 => 1,
        _ => {
            let n = rest.iter().position(|c| *c < 0x20 || *c == 0x7f).unwrap_or(rest.len());
            r.buf.push_str(&String::from_utf8_lossy(&rest[..n]));
            r.error = None;
            n
        }
    };
    mark(s, &mut st);
    used
}

fn draw_menu(f: &mut Frame, area: Rect, m: &Menu, st: &State) {
    use ratatui::text::Line;
    use ratatui::widgets::{Block, Clear, Padding, Paragraph};
    let pick = |i: usize, sel: usize| if i == sel { Style::default().add_modifier(Modifier::REVERSED) } else { Style::default() };
    let mut lines = Vec::new();
    let hint;
    if let Some(k) = &m.keys {
        lines.push(Line::from("F12 の後に押すキー"));
        lines.push(Line::from(""));
        for (i, ((_, label, _), c)) in ACTIONS.iter().zip(st.keys).enumerate() {
            lines.push(Line::styled(format!(" {}  {label} ", c as char), pick(i, k.sel)));
        }
        lines.push(Line::styled(" 既定に戻す ".to_string(), pick(ACTIONS.len(), k.sel)));
        hint = if k.capture { "割り当てるキーを押す (F12 は不要)  Esc 取消" } else { "j/k 選択  Enter 変更  Esc 戻る" };
    } else {
        lines.push(Line::from(format!("MornDeck v{}", env!("CARGO_PKG_VERSION"))));
        lines.push(Line::from(""));
        for (i, item) in menu_items(m, st.sidebar, &st.hooks).iter().enumerate() {
            let color = match st.hooks.get(i).map(|h| h.1) {
                Some(hooks::Status::Current) => Color::LightGreen,
                Some(hooks::Status::NoAgent) => Color::DarkGray,
                Some(_) => Color::Yellow,
                None => Color::Reset,
            };
            lines.push(Line::styled(format!(" {item} "), pick(i, m.sel).fg(color)));
        }
        hint = "j/k 選択  Enter 実行  ←/→ 幅  Esc/F12 閉じる";
    }
    lines.push(Line::from(""));
    lines.push(Line::styled(m.notice.clone().unwrap_or_else(|| hint.into()), Style::default().fg(Color::DarkGray)));
    // 枠 + 左右 2 / 上下 1 の余白
    let (w, h) = (60.min(area.width), (lines.len() as u16 + 4).min(area.height));
    let r = Rect::new(area.x + (area.width - w) / 2, area.y + (area.height - h) / 2, w, h);
    f.render_widget(Clear, r);
    f.render_widget(Paragraph::new(lines).block(Block::bordered().title(" メニュー ").padding(Padding::new(2, 2, 1, 1))), r);
}

fn menu_input(s: &Shared, rest: &[u8]) -> usize {
    let starts = |a: &[u8], b: &[u8]| rest.starts_with(a) || rest.starts_with(b);
    let (up, down) = (starts(b"\x1b[A", b"\x1bOA"), starts(b"\x1b[B", b"\x1bOB"));
    let (left, right) = (starts(b"\x1b[D", b"\x1bOD"), starts(b"\x1b[C", b"\x1bOC"));
    let mut st = lock(s);
    let Some(mut m) = st.menu.take() else { return 0 };
    let used = if rest[0] == 0x1b { esc_len(rest) } else { 1 };
    let mut open = true;
    if let Some(k) = m.keys.as_mut() {
        if k.capture {
            k.capture = false;
            let c = rest[0];
            let taken = st.keys.iter().position(|x| *x == c).filter(|j| *j != k.sel);
            m.notice = Some(if c == 0x1b {
                "取り消した".into()
            } else if !c.is_ascii_graphic() {
                "英数字か記号 1 文字にする".into()
            } else if let Some(j) = taken {
                format!("{} は「{}」で使っている", c as char, ACTIONS[j].1)
            } else {
                st.keys[k.sel] = c;
                match save_keys(&st.sock, &st.keys) {
                    Ok(()) => "保存した".into(),
                    Err(e) => format!("保存できない: {e}"),
                }
            });
        } else {
            m.notice = None;
            match rest[0] {
                _ if up => k.sel = k.sel.saturating_sub(1),
                _ if down => k.sel = (k.sel + 1).min(ACTIONS.len()),
                b'k' => k.sel = k.sel.saturating_sub(1),
                b'j' => k.sel = (k.sel + 1).min(ACTIONS.len()),
                b'\r' if k.sel == ACTIONS.len() => {
                    st.keys = ACTIONS.map(|a| a.2);
                    let _ = std::fs::remove_file(keys_path(&st.sock));
                    m.notice = Some("既定に戻した".into());
                }
                b'\r' => k.capture = true,
                b'q' | 0x1b => m.keys = None,
                _ => {}
            }
        }
        st.menu = Some(m);
        mark(s, &mut st);
        return used;
    }
    let sel = m.sel;
    match rest[0] {
        _ if up => m.sel = m.sel.saturating_sub(1),
        _ if down => m.sel = (m.sel + 1).min(MENU_LEN - 1),
        _ if left || right => {
            if sel == M_SIDEBAR {
                let w = st.sidebar as i32 + if right { 2 } else { -2 };
                set_sidebar(s, &mut st, w);
            }
        }
        b'k' => m.sel = m.sel.saturating_sub(1),
        b'j' => m.sel = (m.sel + 1).min(MENU_LEN - 1),
        b'h' | b'l' if sel == M_SIDEBAR => {
            let w = st.sidebar as i32 + if rest[0] == b'l' { 2 } else { -2 };
            set_sidebar(s, &mut st, w);
        }
        b'\r' if sel < M_UPDATE => {
            if let Some(&(agent, status)) = st.hooks.get(sel) {
                m.notice = Some(match status {
                    hooks::Status::NoAgent => format!("{agent} が入っていない (~/.{agent} が無い)"),
                    _ => match hooks::install(agent) {
                        Ok(()) => format!("v{} にした。起動中の {agent} は再起動で反映", hooks::VERSION),
                        Err(e) => format!("失敗: {e}"),
                    },
                });
                st.hooks = hooks::status();
            }
        }
        b'\r' if sel == M_UPDATE && m.update.is_some() => {
            // 進み具合と失敗が見えるよう、更新は新しいタブで走らせる
            drop(st);
            let _ = new_tab(s, None, Some(UPGRADE_CMD), None, true);
            return 1;
        }
        b'\r' if sel == M_UPDATE => {
            m.notice = Some("確認中…".into());
            let s2 = s.clone();
            std::thread::spawn(move || {
                let r = check_update();
                let mut st = lock(&s2);
                if let Some(m) = st.menu.as_mut() {
                    m.notice = Some(match &r {
                        Ok(Some(v)) => format!("v{v} がある。Enter で更新"),
                        Ok(None) => format!("最新 (v{})", env!("CARGO_PKG_VERSION")),
                        Err(e) => format!("確認失敗: {e}"),
                    });
                    m.update = r.ok().flatten();
                    mark(&s2, &mut st);
                }
            });
        }
        b'\r' if sel == M_SIDEBAR => {}
        b'\r' if sel == M_KEYS => {
            m.notice = None;
            m.keys = Some(KeyEdit { sel: 0, capture: false });
        }
        b'\r' | b'q' | 0x1b => open = false,
        _ => {}
    }
    if open {
        st.menu = Some(m);
    }
    mark(s, &mut st);
    used
}

/// フックが未導入でもアイコンを出せるよう、前面プロセス群の実行ファイル名から判定する。
fn fg_agent(pgrp: Option<libc::pid_t>) -> Option<&'static str> {
    let pgrp = pgrp?;
    let mut pids = [0 as libc::pid_t; 64];
    let n = unsafe { libc::proc_listpgrppids(pgrp, pids.as_mut_ptr().cast(), std::mem::size_of_val(&pids) as i32) };
    pids[..(n.max(0) as usize).min(64)].iter().find_map(|&pid| {
        let mut path = [0u8; 4096];
        let len = unsafe { libc::proc_pidpath(pid, path.as_mut_ptr().cast(), path.len() as u32) };
        let path = std::str::from_utf8(&path[..len.max(0) as usize]).ok()?;
        ["claude", "codex"].into_iter().find(|a| path.contains(a))
    })
}

fn draw_term(buf: &mut Buffer, area: Rect, term: &Term<Listener>) -> Option<(u16, u16)> {
    let c = term.renderable_content();
    let off = c.display_offset as i32;
    let sel = c.selection;
    for ind in c.display_iter {
        let row = ind.point.line.0 + off;
        let col = ind.point.column.0 as u16;
        if row < 0 || row as u16 >= area.height || col >= area.width {
            continue;
        }
        let cell = &ind.cell;
        let out = &mut buf[(area.x + col, area.y + row as u16)];
        if cell.flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
            out.set_symbol(" ");
            continue;
        }
        let mut sym = String::from(if cell.c == '\0' || cell.c == '\t' { ' ' } else { cell.c });
        if let Some(z) = cell.zerowidth() {
            sym.extend(z);
        }
        out.set_symbol(&sym);
        let mut style = cell_style(cell.fg, cell.bg, cell.flags);
        if sel.is_some_and(|r| r.contains(ind.point)) {
            style = style.add_modifier(Modifier::REVERSED);
        }
        out.set_style(style);
    }
    if off == 0 && c.mode.contains(TermMode::SHOW_CURSOR) && c.cursor.shape != CursorShape::Hidden {
        let (x, y) = (c.cursor.point.column.0 as u16, c.cursor.point.line.0);
        if y >= 0 && (y as u16) < area.height && x < area.width {
            return Some((area.x + x, area.y + y as u16));
        }
    }
    None
}

fn map_color(c: AColor) -> Color {
    match c {
        AColor::Spec(Rgb { r, g, b }) => Color::Rgb(r, g, b),
        AColor::Indexed(i) => Color::Indexed(i),
        AColor::Named(n) => {
            let i = n as usize;
            if i < 16 {
                Color::Indexed(i as u8)
            } else if (NamedColor::DimBlack as usize..=NamedColor::DimWhite as usize).contains(&i) {
                Color::Indexed((i - NamedColor::DimBlack as usize) as u8)
            } else {
                Color::Reset
            }
        }
    }
}

fn cell_style(fg: AColor, bg: AColor, fl: Flags) -> Style {
    let mut m = Modifier::empty();
    for (f, md) in [
        (Flags::BOLD, Modifier::BOLD),
        (Flags::ITALIC, Modifier::ITALIC),
        (Flags::ALL_UNDERLINES, Modifier::UNDERLINED),
        (Flags::DIM, Modifier::DIM),
        (Flags::INVERSE, Modifier::REVERSED),
        (Flags::HIDDEN, Modifier::HIDDEN),
        (Flags::STRIKEOUT, Modifier::CROSSED_OUT),
    ] {
        if fl.intersects(f) {
            m |= md;
        }
    }
    Style::default().fg(map_color(fg)).bg(map_color(bg)).add_modifier(m)
}

// ---------- connections ----------

fn handle_conn(s: Shared, conn: UnixStream) -> std::io::Result<()> {
    let mut rd = BufReader::new(conn.try_clone()?);
    let mut line = String::new();
    rd.read_line(&mut line)?;
    let mut w = conn;
    let req: Value = match serde_json::from_str(&line) {
        Ok(v) => v,
        Err(e) => return writeln!(w, "{}", json!({"ok": false, "error": format!("invalid json: {e}")})),
    };
    if req["cmd"] == "attach" {
        return attach(s, rd, w, &req);
    }
    if req["cmd"] == "subscribe" {
        let (tx, rx) = channel();
        lock(&s).subs.push(tx);
        for l in rx {
            if writeln!(w, "{l}").is_err() {
                break;
            }
        }
        return Ok(());
    }
    let resp = api(&s, &req).unwrap_or_else(|e| json!({"ok": false, "error": e}));
    writeln!(w, "{resp}")
}

fn tab_id(v: &Value) -> Result<u32, String> {
    v["tab"].as_u64().map(|n| n as u32).ok_or_else(|| "tab (数値) が必要".into())
}

fn validate_name<'a>(name: &str, mut taken: impl Iterator<Item = &'a str>) -> Result<(), String> {
    if name.is_empty() || name.chars().count() > 32 || name.chars().any(char::is_whitespace) {
        return Err("名前は空白なしの 1〜32 文字".into());
    }
    if name.chars().all(|c| c.is_ascii_digit()) {
        return Err("数字だけの名前はタブ番号と紛れるので使えない".into());
    }
    if taken.any(|t| t == name) {
        return Err(format!("名前が重複している: {name}"));
    }
    Ok(())
}

fn timeout(req: &Value, default_ms: u64) -> Duration {
    Duration::from_millis(req["timeout_ms"].as_u64().unwrap_or(default_ms).min(1_800_000))
}

fn api(s: &Shared, req: &Value) -> Result<Value, String> {
    let cmd = req["cmd"].as_str().unwrap_or("");
    if matches!(cmd, "send" | "choose" | "wait" | "read" | "answer" | "choices" | "rename") {
        let mut st = lock(s);
        if let Ok(i) = st.find(&req["tab"]) {
            st.tabs[i].last_used = now_secs();
        }
    }
    match cmd {
        "list" => {
            let st = lock(s);
            let tabs: Vec<Value> = (0..st.tabs.len()).map(|i| st.tab_json(i)).collect();
            Ok(json!({"ok": true, "tabs": tabs}))
        }
        "new" => {
            let agent = req["agent"].as_str();
            let run = match agent {
                Some(a @ ("claude" | "codex")) => Some(format!("{a} {}", req["args"].as_str().unwrap_or("")).trim_end().to_string()),
                Some(a) => return Err(format!("agent は claude か codex: {a}")),
                None => req["run"].as_str().map(str::to_string),
            };
            let name = req["name"].as_str().map(str::to_string);
            // 既定のホームは claude の信頼確認で止まりやすいので、起動先は呼ぶ側に決めさせる
            let cwd = req["cwd"].as_str().ok_or("cwd (起動するフォルダ) が必要")?;
            if !std::path::Path::new(cwd).is_dir() {
                return Err(format!("フォルダが無い: {cwd}"));
            }
            let owner = req["owner"].as_str().map(str::to_string);
            if let Some(o) = &owner {
                validate_name(o, std::iter::empty())?;
            }
            let id = new_tab(s, Some(cwd), run.as_deref(), name, false)?;
            if let Some(t) = lock(s).tab(id) {
                t.owner = owner;
                if agent.is_some() {
                    t.args = req["args"].as_str().map(str::to_string);
                    t.starting = true;
                }
            }
            if agent.is_some() && req["wait"].as_bool().unwrap_or(true)
                && let Err(e) = wait_ready(s, id, timeout(req, 60_000)) {
                    return Ok(e);
                }
            let st = lock(s);
            let i = st.find(&json!(id))?;
            Ok(json!({"ok": true, "id": id, "tab": st.tab_json(i)}))
        }
        "rename" => {
            let mut st = lock(s);
            let i = st.find(&req["tab"])?;
            let id = st.tabs[i].id;
            let name = req["name"].as_str().filter(|n| !n.is_empty());
            if let Some(n) = name {
                st.check_name(n, Some(id))?;
            }
            st.tabs[i].name = name.map(str::to_string);
            mark(s, &mut st);
            Ok(json!({"ok": true, "tab": st.tab_json(i)}))
        }
        "send" => {
            let text = req["text"].as_str().unwrap_or("");
            let raw = req["raw"].as_bool().unwrap_or(false);
            let wait = req["wait"].as_bool().unwrap_or(false);
            let enter = req["enter"].as_bool().unwrap_or(!raw);
            let request_id = req["request_id"].as_str().map(str::to_string);
            let id = {
                let mut st = lock(s);
                let i = st.find(&req["tab"])?;
                let t = &mut st.tabs[i];
                // 接続が切れて送り直された依頼は打ち込まず、最初の受付結果を返す
                if let Some(rid) = &request_id
                    && let Some((_, seq)) = t.requests.iter().find(|(r, _)| r == rid)
                {
                    let (tid, seq) = (t.id, *seq);
                    drop(st);
                    if wait {
                        let mut v = wait_turn(s, tid, timeout(req, 600_000))?;
                        v["duplicate"] = json!(true);
                        return Ok(v);
                    }
                    return Ok(json!({"ok": true, "id": tid, "expect_seq": seq, "duplicate": true}));
                }
                let force = req["force"].as_bool().unwrap_or(false);
                if t.state == Some("working") && !force {
                    return Err("busy: エージェントが作業中。wait で終わるのを待つか force: true".into());
                }
                // 確認画面に指示を打ち込まないよう、起動待ちのタブには選択 (choose / raw) しか通さない
                if t.starting && !raw && !force {
                    return Err(format!("not_ready: エージェントが起動していない (state: {})。choices を見て select_choice で答える", status(t).unwrap_or("")));
                }
                let data = if !raw && t.term.mode().contains(TermMode::BRACKETED_PASTE) {
                    format!("\x1b[200~{text}\x1b[201~")
                } else {
                    text.to_string()
                };
                t.writer.write_all(data.as_bytes()).map_err(|e| e.to_string())?;
                if let Some(rid) = request_id {
                    t.requests.push((rid, t.answer_seq + 1));
                    let extra = t.requests.len().saturating_sub(100);
                    t.requests.drain(..extra);
                }
                (t.id, t.answer_seq + 1)
            };
            let (id, expect_seq) = id;
            if enter {
                // 本文と同じ書き込みで送ると TUI がペーストの一部として扱い送信しない
                std::thread::sleep(Duration::from_millis(150));
                let mut st = lock(s);
                let t = st.tab(id).ok_or("tab closed")?;
                t.writer.write_all(b"\r").map_err(|e| e.to_string())?;
            }
            {
                let mut st = lock(s);
                let has_agent = st.tab(id).is_some_and(|t| t.agent.is_some());
                if has_agent && (enter || wait) {
                    set_state(s, &mut st, id, Some("working"));
                }
            }
            if wait {
                return wait_turn(s, id, timeout(req, 600_000));
            }
            Ok(json!({"ok": true, "id": id, "expect_seq": expect_seq}))
        }
        "choose" => {
            let n = req["n"].as_u64().ok_or("n (選択肢の番号) が必要")?;
            let id = {
                let mut st = lock(s);
                let i = st.find(&req["tab"])?;
                let t = &mut st.tabs[i];
                let options = screen_options(&t.term);
                let target = options.iter().position(|o| o["n"] == n).ok_or_else(|| format!("選択肢 {n} が無い"))?;
                let keys = if options[target]["numbered"] == true {
                    n.to_string()
                } else {
                    // 番号の無い選択肢は矢印で動かして Enter
                    let cur = options.iter().position(|o| o["selected"] == true).unwrap_or(0) as i64;
                    let app = t.term.mode().contains(TermMode::APP_CURSOR);
                    let (up, down) = if app { ("\x1bOA", "\x1bOB") } else { ("\x1b[A", "\x1b[B") };
                    let d = target as i64 - cur;
                    format!("{}\r", if d < 0 { up } else { down }.repeat(d.unsigned_abs() as usize))
                };
                t.writer.write_all(keys.as_bytes()).map_err(|e| e.to_string())?;
                t.id
            };
            if !req["wait"].as_bool().unwrap_or(false) {
                return Ok(json!({"ok": true, "id": id}));
            }
            {
                let mut st = lock(s);
                if st.tab(id).is_some_and(|t| t.agent.is_some() && !t.starting) {
                    set_state(s, &mut st, id, Some("working"));
                }
            }
            std::thread::sleep(Duration::from_millis(300));
            wait_turn(s, id, timeout(req, 600_000))
        }
        "wait" => {
            let id = {
                let st = lock(s);
                st.tabs[st.find(&req["tab"])?].id
            };
            wait_turn(s, id, timeout(req, 600_000))
        }
        "read" => {
            let st = lock(s);
            let i = st.find(&req["tab"])?;
            Ok(json!({"ok": true, "lines": screen_lines(&st.tabs[i].term)}))
        }
        "answer" => {
            let st = lock(s);
            let t = &st.tabs[st.find(&req["tab"])?];
            Ok(answer_json(t))
        }
        "choices" => {
            let st = lock(s);
            let t = &st.tabs[st.find(&req["tab"])?];
            Ok(json!({"ok": true, "state": status(t), "question": t.question, "options": screen_options(&t.term)}))
        }
        "select" => {
            let mut st = lock(s);
            st.active = st.find(&req["tab"])?;
            mark(s, &mut st);
            Ok(json!({"ok": true}))
        }
        "close" => {
            let mut st = lock(s);
            // Bot が落ちて置き去りにしたタブをまとめて片付けられるよう、owner でも閉じられる
            if let Some(owner) = req["owner"].as_str() {
                let mut closed = Vec::new();
                for t in st.tabs.iter_mut().filter(|t| t.owner.as_deref() == Some(owner)) {
                    let _ = t.child.kill();
                    closed.push(t.id);
                }
                return Ok(json!({"ok": true, "closed": closed}));
            }
            let i = st.find(&req["tab"])?;
            st.tabs[i].child.kill().map_err(|e| e.to_string())?;
            Ok(json!({"ok": true, "closed": [st.tabs[i].id]}))
        }
        "webhooks" => {
            let mut st = lock(s);
            let mut next = None;
            if let Some(list) = req["set"].as_array() {
                next = Some(list.iter().map(parse_webhook).collect::<Result<Vec<_>, _>>()?);
            }
            // 複数の Bot が互いの登録を消さないよう、1 件ずつの追加と削除を用意する
            if !req["add"].is_null() {
                let hook = parse_webhook(&req["add"])?;
                let mut list = st.webhooks.clone();
                list.retain(|w| w["url"] != hook["url"]);
                list.push(hook);
                next = Some(list);
            }
            if let Some(url) = req["remove"].as_str() {
                let mut list = st.webhooks.clone();
                list.retain(|w| w["url"] != url);
                next = Some(list);
            }
            if let Some(hooks) = next {
                let path = webhooks_path(&st.sock);
                std::fs::write(&path, serde_json::to_string(&hooks).unwrap()).map_err(|e| e.to_string())?;
                // ヘッダーに送信キーを含むので本人だけが読めるようにする
                let _ = std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o600));
                st.webhooks = hooks;
            }
            let shown: Vec<Value> = st.webhooks.iter().map(|w| {
                let names: Vec<&String> = w["headers"].as_object().map(|h| h.keys().collect()).unwrap_or_default();
                json!({"url": w["url"], "headers": names})
            }).collect();
            Ok(json!({"ok": true, "webhooks": shown}))
        }
        "hook" => {
            let id = tab_id(req)?;
            let agent = req["agent"].as_str().unwrap_or("agent").to_string();
            let ask = req["tool"] == "AskUserQuestion";
            let state = match req["event"].as_str().unwrap_or("") {
                "SessionStart" => Some("idle"),
                "PreToolUse" if ask => Some("waiting"),
                "UserPromptSubmit" | "PreToolUse" | "PostToolUse" => Some("working"),
                "PermissionRequest" => Some("waiting"),
                "Stop" => Some("done"),
                "SessionEnd" => None,
                _ => return Ok(json!({"ok": true})),
            };
            let mut st = lock(s);
            let t = st.tab(id).ok_or("no such tab")?;
            if state == Some("done") {
                t.answer = req["message"].as_str().map(str::to_string).or(t.answer.take());
                t.answer_seq += 1;
            }
            t.question = if state == Some("waiting") {
                json!({"tool": req["tool"], "input": req["input"]})
            } else {
                Value::Null
            };
            if state.is_none() {
                t.session = None;
            } else if let Some(sid) = req["session"].as_str() {
                t.session = Some(sid.to_string());
            }
            t.agent = state.map(|_| agent);
            t.starting = false;
            set_state(s, &mut st, id, state);
            Ok(json!({"ok": true}))
        }
        c => Err(format!("unknown cmd: {c}")),
    }
}

fn answer_json(t: &Tab) -> Value {
    // 作業中に前回の回答を返すと取り違えるので本文は出さない
    let text = if t.state == Some("working") { None } else { t.answer.clone() };
    json!({"ok": true, "id": t.id, "name": t.name, "agent": t.agent, "state": status(t), "seq": t.answer_seq, "text": text})
}

/// 起動待ちのタブは、確認画面が出ていれば blocked、出ていなければ starting と見せる。
fn status(t: &Tab) -> Option<&'static str> {
    if !t.starting {
        return t.state;
    }
    Some(if screen_options(&t.term).is_empty() { "starting" } else { "blocked" })
}

fn set_state(s: &Shared, st: &mut State, id: u32, state: Option<&'static str>) {
    let Some(t) = st.tab(id) else { return };
    let changed = t.state != state;
    t.state = state;
    let mut ev = json!({"event": "state", "tab": id, "name": t.name, "agent": t.agent, "state": t.state});
    if state == Some("done") {
        ev["answer"] = json!(t.answer);
        ev["seq"] = json!(t.answer_seq);
    }
    if !changed {
        return;
    }
    st.notify(ev);
    mark(s, st);
    if state == Some("waiting") {
        let s = s.clone();
        std::thread::spawn(move || {
            // フックは選択肢の描画より先に届くので、画面が出揃うのを待ってから読む
            std::thread::sleep(Duration::from_millis(500));
            let mut st = lock(&s);
            let Some(t) = st.tab(id).filter(|t| t.state == Some("waiting")) else { return };
            let ev = json!({"event": "choices", "tab": id, "name": t.name, "question": t.question, "options": screen_options(&t.term)});
            st.notify(ev);
        });
    }
}

/// エージェントが作業を終える (done / waiting / 終了) まで待つ。
// ponytail: 200ms ごとに状態を見る。待つ API が増えて重くなったら Condvar で起こす
fn wait_turn(s: &Shared, id: u32, limit: Duration) -> Result<Value, String> {
    let start = std::time::Instant::now();
    loop {
        {
            let st = lock(s);
            let Some(t) = st.tabs.iter().find(|t| t.id == id) else { return Err("tab closed".into()) };
            let st_now = status(t);
            if st_now == Some("blocked") {
                return Ok(json!({"ok": true, "id": id, "name": t.name, "state": "blocked", "options": screen_options(&t.term)}));
            }
            if st_now != Some("working") && st_now != Some("starting") {
                let waiting = t.state == Some("waiting");
                drop(st);
                if waiting {
                    std::thread::sleep(Duration::from_millis(500));
                }
                let st = lock(s);
                let t = st.tabs.iter().find(|t| t.id == id).ok_or("tab closed")?;
                let mut v = answer_json(t);
                if waiting {
                    v["question"] = t.question.clone();
                    v["options"] = json!(screen_options(&t.term));
                }
                return Ok(v);
            }
        }
        if start.elapsed() > limit {
            let st = lock(s);
            let t = st.tabs.iter().find(|t| t.id == id).ok_or("tab closed")?;
            let mut v = answer_json(t);
            v["timeout"] = json!(true);
            return Ok(v);
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// フックの SessionStart が届くまで待つ。確認画面で止まっていればその選択肢を返す。
fn wait_ready(s: &Shared, id: u32, limit: Duration) -> Result<(), Value> {
    let start = std::time::Instant::now();
    loop {
        std::thread::sleep(Duration::from_millis(300));
        let st = lock(s);
        let Some(t) = st.tabs.iter().find(|t| t.id == id) else {
            return Err(json!({"ok": false, "error": "起動に失敗してタブが閉じた", "id": id}));
        };
        if t.agent.is_some() {
            return Ok(());
        }
        let options = screen_options(&t.term);
        let blocked = start.elapsed() > Duration::from_secs(2) && !options.is_empty();
        if blocked || start.elapsed() > limit {
            let lines: Vec<String> = screen_lines(&t.term).into_iter().filter(|l| !l.trim().is_empty()).collect();
            let tail = lines[lines.len().saturating_sub(20)..].to_vec();
            let error = if blocked {
                "blocked: 確認画面で止まっている。options の番号を choose (MCP は select_choice) で選ぶ"
            } else if st.hooks.iter().any(|(_, h)| matches!(h, hooks::Status::Missing | hooks::Status::Outdated(_))) {
                "timeout: 起動を確認できない。フックが未導入か古い (F12 → m)"
            } else {
                "timeout: 起動を確認できない"
            };
            let state = if blocked { "blocked" } else { "starting" };
            return Err(json!({"ok": false, "error": error, "id": id, "state": state, "options": options, "screen": tail}));
        }
    }
}

// ---------- 落ちても戻れるようにタブの情報を保存する ----------

fn tabs_path(sock: &str) -> std::path::PathBuf {
    std::path::Path::new(sock).with_file_name("tabs.json")
}

fn proc_cwd(pid: u32) -> Option<String> {
    let mut info: libc::proc_vnodepathinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of_val(&info) as i32;
    let n = unsafe { libc::proc_pidinfo(pid as i32, libc::PROC_PIDVNODEPATHINFO, 0, (&raw mut info).cast(), size) };
    if n != size {
        return None;
    }
    let raw: Vec<u8> = info.pvi_cdir.vip_path.iter().flatten().map(|c| *c as u8).take_while(|c| *c != 0).collect();
    String::from_utf8(raw).ok().filter(|p| !p.is_empty())
}

fn snapshot(st: &State) -> Value {
    let tabs: Vec<Value> = st
        .tabs
        .iter()
        .map(|t| {
            let cwd = t.child.process_id().and_then(proc_cwd);
            json!({"id": t.id, "name": t.name, "cwd": cwd, "agent": t.agent, "session": t.session, "args": t.args, "seq": t.answer_seq,
                   "owner": t.owner, "last_used": t.last_used})
        })
        .collect();
    json!({"active": st.active, "sidebar": st.sidebar, "tabs": tabs})
}

fn save_loop(s: Shared) {
    let mut last = String::new();
    loop {
        std::thread::sleep(Duration::from_secs(2));
        let (body, path) = {
            let st = lock(&s);
            (snapshot(&st).to_string(), tabs_path(&st.sock))
        };
        if body != last {
            let tmp = path.with_extension("tmp");
            if std::fs::write(&tmp, &body).and_then(|_| std::fs::rename(&tmp, &path)).is_ok() {
                last = body;
            }
        }
    }
}

/// 保存された会話 ID で再開するコマンド。ID はシェルに打ち込むので英数字と - だけを通す。
fn resume_command(agent: Option<&str>, session: Option<&str>, args: Option<&str>) -> Option<String> {
    let sid = session.filter(|s| !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'))?;
    let base = match agent? {
        "claude" => format!("claude --resume {sid}"),
        "codex" => format!("codex resume {sid}"),
        _ => return None,
    };
    Some(match args.filter(|a| !a.trim().is_empty()) {
        Some(a) => format!("{base} {a}"),
        None => base,
    })
}

fn restore(s: &Shared) {
    let sock = lock(s).sock.clone();
    let Some(saved) = std::fs::read_to_string(tabs_path(&sock)).ok().and_then(|b| serde_json::from_str::<Value>(&b).ok()) else {
        return;
    };
    for t in saved["tabs"].as_array().into_iter().flatten() {
        let Some(id) = t["id"].as_u64() else { continue };
        let cwd = t["cwd"].as_str().filter(|d| std::path::Path::new(d).is_dir());
        let run = resume_command(t["agent"].as_str(), t["session"].as_str(), t["args"].as_str());
        lock(s).next_id = id as u32;
        let name = t["name"].as_str().map(str::to_string);
        // 名前が壊れていても他のタブは戻す
        let created = new_tab(s, cwd, run.as_deref(), name, false).or_else(|_| new_tab(s, cwd, run.as_deref(), None, false));
        if let Ok(id) = created
            && let Some(tab) = lock(s).tab(id) {
                tab.args = t["args"].as_str().map(str::to_string);
                tab.answer_seq = t["seq"].as_u64().unwrap_or(0);
                tab.starting = run.is_some();
                tab.owner = t["owner"].as_str().map(str::to_string);
                tab.last_used = t["last_used"].as_u64().unwrap_or_else(now_secs);
            }
    }
    let mut st = lock(s);
    st.next_id = st.tabs.iter().map(|t| t.id + 1).max().unwrap_or(1);
    st.active = (saved["active"].as_u64().unwrap_or(0) as usize).min(st.tabs.len().saturating_sub(1));
    if let Some(w) = saved["sidebar"].as_u64() {
        st.sidebar = (w as u16).max(SIDEBAR_MIN);
    }
}

fn webhooks_path(sock: &str) -> std::path::PathBuf {
    std::path::Path::new(sock).with_file_name("webhooks.json")
}

/// `"https://..."` か `{"url": "https://...", "headers": {"X-Key": "..."}}`。
fn parse_webhook(v: &Value) -> Result<Value, String> {
    let (url, headers) = match v {
        Value::String(u) => (u.as_str(), json!({})),
        Value::Object(o) => (o.get("url").and_then(Value::as_str).unwrap_or(""), o.get("headers").cloned().unwrap_or(json!({}))),
        _ => return Err(format!("webhook は URL か {{url, headers}}: {v}")),
    };
    if !url.starts_with("http://") && !url.starts_with("https://") {
        return Err(format!("http(s) の URL ではない: {url}"));
    }
    let Some(h) = headers.as_object() else { return Err("headers はオブジェクト".into()) };
    for (k, val) in h {
        let ok_name = !k.is_empty() && k.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
        let ok_val = val.as_str().is_some_and(|v| !v.contains(['\r', '\n']));
        if !ok_name || !ok_val {
            return Err(format!("使えないヘッダー: {k}"));
        }
    }
    Ok(json!({"url": url, "headers": headers}))
}

fn load_webhooks(sock: &str) -> Vec<Value> {
    let list: Vec<Value> =
        std::fs::read_to_string(webhooks_path(sock)).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
    list.iter().filter_map(|v| parse_webhook(v).ok()).collect()
}

fn post_webhooks(hooks: &[Value], body: &Value) {
    for hook in hooks {
        let mut args: Vec<String> = ["-sS", "-m", "10", "-X", "POST", "-H", "Content-Type: application/json"].map(String::from).to_vec();
        for (k, v) in hook["headers"].as_object().into_iter().flatten() {
            args.extend(["-H".into(), format!("{k}: {}", v.as_str().unwrap_or(""))]);
        }
        args.extend(["--data-binary".into(), "@-".into(), hook["url"].as_str().unwrap_or("").into()]);
        // ponytail: ヘッダー値が数秒 ps に見える。共用マシンで使うなら curl の --config を一時ファイルで渡す
        let spawned = std::process::Command::new("curl")
            .args(&args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
        if let Ok(mut c) = spawned {
            if let Some(mut i) = c.stdin.take() {
                let _ = i.write_all(body.to_string().as_bytes());
            }
            std::thread::spawn(move || c.wait());
        }
    }
}

fn screen_lines(term: &Term<Listener>) -> Vec<String> {
    use alacritty_terminal::index::{Column, Line};
    let g = term.grid();
    (0..g.screen_lines() as i32)
        .map(|l| {
            let row = &g[Line(l)];
            (0..g.columns())
                .map(|c| &row[Column(c)])
                .filter(|c| !c.flags.contains(Flags::WIDE_CHAR_SPACER))
                .map(|c| c.c)
                .collect::<String>()
                .trim_end()
                .into()
        })
        .collect()
}

/// 画面下側にある「1. ラベル」形式の行の並びを選択肢として返す。番号が 1 に戻ったら新しい並びとみなす。
fn screen_options(term: &Term<Listener>) -> Vec<Value> {
    parse_options(&screen_lines(term))
}

fn parse_options(lines: &[String]) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::new();
    for l in lines {
        let t = l.trim_start_matches([' ', '│', '┃', '|']);
        let selected = t.starts_with(MARKERS);
        let t = t.trim_start_matches(MARKERS).trim_start();
        let digits: String = t.chars().take_while(char::is_ascii_digit).collect();
        let Some(label) = t[digits.len()..].strip_prefix(". ") else { continue };
        let Ok(n) = digits.parse::<u32>() else { continue };
        if n == 1 {
            out.clear();
        }
        let label = label.trim_end_matches([' ', '│', '┃', '|']).trim();
        out.push(json!({"n": n, "label": label, "selected": selected, "numbered": true}));
    }
    if out.is_empty() {
        out = unnumbered_options(lines);
    }
    out
}

const MARKERS: [char; 4] = ['❯', '›', '>', '▶'];

/// 「❯ 選択中」の行と、その文字位置にそろえて上下に並ぶ行を選択肢とみなす (claude の信頼確認など)。
fn unnumbered_options(lines: &[String]) -> Vec<Value> {
    let marker_text_col = |l: &str| -> Option<usize> {
        let lead = l.chars().take_while(|c| *c == ' ').count();
        let rest: String = l.chars().skip(lead).collect();
        let after = rest.strip_prefix(['❯', '›', '▶'])?;
        let gap = after.chars().take_while(|c| *c == ' ').count();
        (gap > 0 && !after.trim().is_empty()).then_some(lead + 1 + gap)
    };
    let sibling = |l: &str, col: usize| {
        let lead = l.chars().take_while(|c| *c == ' ').count();
        let first = l.chars().nth(lead);
        lead == col && first.is_some_and(|c| !('\u{2500}'..='\u{257F}').contains(&c))
    };
    let Some((idx, col)) = lines.iter().enumerate().rev().find_map(|(i, l)| marker_text_col(l).map(|c| (i, c))) else {
        return Vec::new();
    };
    let mut lo = idx;
    while lo > 0 && sibling(&lines[lo - 1], col) {
        lo -= 1;
    }
    let mut hi = idx;
    while hi + 1 < lines.len() && sibling(&lines[hi + 1], col) {
        hi += 1;
    }
    if hi == lo {
        return Vec::new();
    }
    (lo..=hi)
        .map(|i| {
            let label: String = lines[i].chars().skip(col).collect();
            json!({"n": i - lo + 1, "label": label.trim(), "selected": i == idx, "numbered": false})
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    #[test]
    fn parse_keys_takes_single_chars_and_rejects_duplicates() {
        let def = super::ACTIONS.map(|a| a.2);
        assert_eq!(super::parse_keys(&json!(null)), def);
        assert_eq!(super::parse_keys(&json!({"new": "z", "menu": "ab", "next": " "}))[0], b'z');
        assert_eq!(super::parse_keys(&json!({"menu": "ab", "next": " "})), def);
        assert_eq!(super::parse_keys(&json!({"new": "x"})), def, "close と重複");
    }

    #[test]
    fn f12_is_never_forwarded_and_closes_what_is_open() {
        let s: super::Shared = Default::default();
        let dir = std::env::temp_dir().join(format!("mdeck-keys-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        {
            let mut st = super::lock(&s);
            st.keys = super::ACTIONS.map(|a| a.2);
            st.sock = dir.join("t.sock").to_string_lossy().into_owned();
        }
        let mut inp = super::Input::default();
        let f12 = super::PREFIX;
        let state = |s: &super::Shared| {
            let st = super::lock(s);
            (st.prefix, st.menu.is_some())
        };
        inp.feed(&s, f12);
        assert_eq!(state(&s), (true, false));
        inp.feed(&s, f12);
        assert_eq!(state(&s), (false, false), "F12 F12 は操作一覧を閉じるだけ");
        inp.feed(&s, b"\x1b[24~m");
        assert_eq!(state(&s), (false, true));
        inp.feed(&s, f12);
        assert_eq!(state(&s), (false, false), "メニューを開いた状態の F12 は閉じる");

        // メニュー → キー設定 → 「新規タブ」に z を割り当てる
        inp.feed(&s, b"\x1b[24~m");
        inp.feed(&s, b"jjjj\r");
        assert!(super::lock(&s).menu.as_ref().unwrap().keys.is_some());
        inp.feed(&s, b"\rx");
        assert_eq!(super::lock(&s).keys[0], b'c', "使用中のキーは割り当てない");
        // 割り当て待ちの F12 は閉じずに読み捨て、続くキーを割り当てる
        inp.feed(&s, b"\r");
        inp.feed(&s, f12);
        assert_eq!(state(&s), (false, true), "割り当て待ちの F12 でメニューが閉じた");
        inp.feed(&s, b"z");
        assert_eq!(super::lock(&s).keys[0], b'z');
        assert_eq!(super::load_keys(&super::lock(&s).sock)[0], b'z');
        inp.feed(&s, f12);
        assert_eq!(state(&s), (false, false));

        // menu を q に移すと m では開かない
        super::lock(&s).keys[5] = b'q';
        inp.feed(&s, b"\x1b[24~m");
        assert_eq!(state(&s), (false, false));
        inp.feed(&s, b"\x1b[24~q");
        assert_eq!(state(&s), (false, true));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn bind_sock_keeps_live_server_and_replaces_stale_socket() {
        let dir = std::env::temp_dir().join(format!("mdeck-bind-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let sock = dir.join("t.sock").to_string_lossy().into_owned();
        let live = super::bind_sock(&sock).unwrap();
        assert_eq!(super::bind_sock(&sock).unwrap_err().kind(), std::io::ErrorKind::AddrInUse);
        assert!(std::os::unix::net::UnixStream::connect(&sock).is_ok(), "生きているソケットが消された");
        drop(live); // ファイルだけ残った死んだソケット
        assert!(super::bind_sock(&sock).is_ok());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn version_parts_compares_numerically() {
        use super::version_parts as v;
        assert!(v("v0.1.10") > v("0.1.9"));
        assert!(v("0.2.0") > v("0.1.15"));
        assert!(v("0.1.15") <= v("0.1.15"));
    }

    #[test]
    fn validate_name_rejects_duplicates_digits_and_spaces() {
        let taken = ["grok", "codex-bot"];
        assert!(super::validate_name("grok2", taken.into_iter()).is_ok());
        assert!(super::validate_name("ぐろっく", taken.into_iter()).is_ok());
        assert!(super::validate_name("grok", taken.into_iter()).is_err());
        assert!(super::validate_name("12", taken.into_iter()).is_err());
        assert!(super::validate_name("a b", taken.into_iter()).is_err());
        assert!(super::validate_name("", taken.into_iter()).is_err());
    }

    #[test]
    fn parse_webhook_accepts_url_or_object_and_rejects_injection() {
        use serde_json::json;
        assert_eq!(super::parse_webhook(&json!("https://a/x")).unwrap(), json!({"url": "https://a/x", "headers": {}}));
        assert!(super::parse_webhook(&json!({"url": "https://a", "headers": {"X-Key": "s"}})).is_ok());
        assert!(super::parse_webhook(&json!("ftp://a")).is_err());
        assert!(super::parse_webhook(&json!({"url": "https://a", "headers": {"X-Key": "a\r\nEvil: 1"}})).is_err());
        assert!(super::parse_webhook(&json!({"url": "https://a", "headers": {"Bad Name": "v"}})).is_err());
    }

    #[test]
    fn resume_command_uses_session_and_rejects_shell_metachars() {
        use super::resume_command as r;
        assert_eq!(r(Some("claude"), Some("ab-12"), None).as_deref(), Some("claude --resume ab-12"));
        assert_eq!(r(Some("codex"), Some("ab-12"), Some("--yolo")).as_deref(), Some("codex resume ab-12 --yolo"));
        assert_eq!(r(Some("claude"), Some("x; rm -rf ~"), None), None);
        assert_eq!(r(Some("claude"), None, None), None);
        assert_eq!(r(None, Some("ab"), None), None);
    }

    #[test]
    fn unnumbered_menu_is_read_but_idle_prompt_is_not() {
        let v = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let trust = v(&[" Security guide", " ❯ No, exit", "   Yes, I trust this folder", " Enter to confirm · Esc to cancel"]);
        let o = super::parse_options(&trust);
        assert_eq!(o.len(), 2);
        assert_eq!(o[1]["label"], "Yes, I trust this folder");
        assert_eq!(o[0]["selected"], true);
        assert_eq!(o[0]["numbered"], false);
        let idle = v(&["────────", "❯ 途中まで打った文", "────────", "  ? for shortcuts"]);
        assert!(super::parse_options(&idle).is_empty());
    }

    #[test]
    fn base64_matches_rfc4648() {
        for (i, o) in [("", ""), ("f", "Zg=="), ("fo", "Zm8="), ("foo", "Zm9v"), ("foob", "Zm9vYg==")] {
            assert_eq!(super::base64(i.as_bytes()), o);
        }
    }

    #[test]
    fn parse_options_takes_last_numbered_block() {
        let lines: Vec<String> = [
            "1. 前回の回答中の箇条書き",
            "2. これも無視される",
            " Do you want to proceed?",
            " ❯ 1. Yes",
            "   2. Yes, and don't ask again",
            "   3. No, and tell Claude what to do differently (esc)",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let o = super::parse_options(&lines);
        assert_eq!(o.len(), 3);
        assert_eq!(o[0]["label"], "Yes");
        assert_eq!(o[0]["selected"], true);
        assert_eq!(o[2]["n"], 3);
        assert_eq!(o[1]["selected"], false);
    }
}

fn attach(s: Shared, mut rd: BufReader<UnixStream>, w: UnixStream, req: &Value) -> std::io::Result<()> {
    let (tx, rx) = channel::<Vec<u8>>();
    let cid = {
        let mut st = lock(&s);
        st.next_client += 1;
        let cid = st.next_client;
        st.clients.push((cid, tx));
        st.full_redraw = true;
        cid
    };
    resize(&s, req["cols"].as_u64().unwrap_or(80) as u16, req["rows"].as_u64().unwrap_or(24) as u16);
    mark(&s, &mut lock(&s));
    let mut ww = w.try_clone()?;
    std::thread::spawn(move || {
        for b in rx {
            if ww.write_all(&b).is_err() {
                break;
            }
        }
        let _ = ww.shutdown(std::net::Shutdown::Both);
    });
    let mut input = Input::default();
    let res = (|| -> std::io::Result<()> {
        loop {
            let mut kind = [0u8; 1];
            rd.read_exact(&mut kind)?;
            match kind[0] {
                IN_RESIZE => {
                    let mut b = [0u8; 4];
                    rd.read_exact(&mut b)?;
                    resize(&s, u16::from_be_bytes([b[0], b[1]]), u16::from_be_bytes([b[2], b[3]]));
                }
                _ => {
                    let mut l = [0u8; 4];
                    rd.read_exact(&mut l)?;
                    let mut data = vec![0u8; u32::from_be_bytes(l) as usize];
                    rd.read_exact(&mut data)?;
                    if input.feed(&s, &data) {
                        return Ok(()); // detach
                    }
                }
            }
        }
    })();
    lock(&s).clients.retain(|(c, _)| *c != cid);
    let _ = w.shutdown(std::net::Shutdown::Both);
    res
}

#[derive(Default)]
struct Input {
    prefix: bool,
    drag: Drag,
}

#[derive(Default, PartialEq)]
enum Drag {
    #[default]
    None,
    Border,
    Select,
}

impl Input {
    fn set_prefix(&mut self, s: &Shared, on: bool) {
        self.prefix = on;
        let mut st = lock(s);
        st.prefix = on;
        mark(s, &mut st);
    }

    /// returns true when the client asked to detach.
    // ponytail: read の境界で分断されたマウス列は中のアプリへそのまま流れる。問題になったら分断分を次回へ持ち越す
    fn feed(&mut self, s: &Shared, data: &[u8]) -> bool {
        let mut fwd = Vec::new();
        let mut i = 0;
        while i < data.len() {
            let b = data[i];
            if data[i..].starts_with(b"\x1b[<")
                && let Some(end) = data[i..].iter().position(|c| *c == b'M' || *c == b'm')
            {
                let seq = &data[i + 3..i + end];
                let press = data[i + end] == b'M';
                flush(s, &mut fwd);
                if lock(s).menu.is_none() {
                    self.mouse(s, seq, press);
                }
                i += end + 1;
                continue;
            }
            if data[i..].starts_with(PREFIX) {
                // F12 は中に送らない。開いているメニュー・名前入力・操作一覧を閉じ、何も無ければ操作一覧を開く
                let mut st = lock(s);
                // キー設定で割り当てを待っている間は F12 → キーと押されがち。閉じると次のキーがタブに流れるので読み捨てる
                if st.menu.as_ref().and_then(|m| m.keys.as_ref()).is_some_and(|k| k.capture) {
                    i += PREFIX.len();
                    continue;
                }
                let closed = st.menu.take().is_some() | st.rename.take().is_some();
                mark(s, &mut st);
                drop(st);
                let on = !closed && !self.prefix;
                self.set_prefix(s, on);
                i += PREFIX.len();
                continue;
            }
            if !self.prefix {
                let used = rename_input(s, &data[i..]);
                if used > 0 {
                    i += used;
                    continue;
                }
                let used = menu_input(s, &data[i..]);
                if used > 0 {
                    i += used;
                    continue;
                }
            }
            if self.prefix {
                self.set_prefix(s, false);
                i += 1;
                let action = lock(s).keys.iter().position(|k| *k == b).map(|n| ACTIONS[n].0);
                match action.unwrap_or("") {
                    "new" => {
                        flush(s, &mut fwd);
                        let _ = new_tab(s, None, None, None, true);
                    }
                    a @ ("next" | "prev") => {
                        let mut st = lock(s);
                        let n = st.tabs.len().max(1);
                        st.active = if a == "next" { (st.active + 1) % n } else { (st.active + n - 1) % n };
                        mark(s, &mut st);
                    }
                    "close" => {
                        let mut st = lock(s);
                        let a = st.active;
                        if let Some(t) = st.tabs.get_mut(a) {
                            let _ = t.child.kill();
                        }
                    }
                    "detach" => {
                        flush(s, &mut fwd);
                        return true;
                    }
                    "rename" => {
                        let mut st = lock(s);
                        let a = st.active;
                        if let Some(t) = st.tabs.get(a) {
                            let (tab, buf) = (t.id, t.name.clone().unwrap_or_default());
                            st.rename = Some(Rename { tab, buf, error: None });
                            mark(s, &mut st);
                        }
                    }
                    "menu" => {
                        let mut st = lock(s);
                        st.hooks = hooks::status();
                        st.menu = Some(Menu { sel: 0, notice: None, update: None, keys: None });
                        mark(s, &mut st);
                    }
                    _ => {}
                }
                continue;
            }
            fwd.push(b);
            i += 1;
        }
        flush(s, &mut fwd);
        false
    }

    fn mouse(&mut self, s: &Shared, seq: &[u8], press: bool) {
        let p: Vec<i32> = std::str::from_utf8(seq).unwrap_or("").split(';').filter_map(|x| x.parse().ok()).collect();
        let [btn, x, y] = p[..] else { return };
        let (x, y) = (x - 1, y - 1);
        let mut st = lock(s);
        let side = st.sidebar as i32;
        if btn & 64 != 0 {
            let a = st.active;
            let Some(t) = st.tabs.get_mut(a) else { return };
            let up = btn & 1 == 0;
            if t.term.mode().contains(TermMode::ALT_SCREEN) {
                let app = t.term.mode().contains(TermMode::APP_CURSOR);
                let key: &[u8] = match (up, app) {
                    (true, true) => b"\x1bOA",
                    (true, false) => b"\x1b[A",
                    (false, true) => b"\x1bOB",
                    (false, false) => b"\x1b[B",
                };
                let _ = t.writer.write_all(&key.repeat(3));
            } else {
                t.term.scroll_display(Scroll::Delta(if up { 3 } else { -3 }));
            }
            mark(s, &mut st);
            return;
        }
        if btn & 3 != 0 {
            return;
        }
        let a = st.active;
        let point = st.tabs.get(a).map(|t| {
            let g = t.term.grid();
            let col = (x - side).clamp(0, g.columns() as i32 - 1) as usize;
            let line = y.clamp(0, g.screen_lines() as i32 - 1) - g.display_offset() as i32;
            Point::new(Line(line), Column(col))
        });
        let motion = btn & 32 != 0;
        match (&self.drag, motion, press) {
            (Drag::None, false, true) if x == side - 1 => self.drag = Drag::Border,
            (Drag::None, false, true) if x < side => {
                if (y as usize) < st.tabs.len() {
                    st.active = y as usize;
                }
            }
            (Drag::None, false, true) => {
                if let (Some(t), Some(pt)) = (st.tabs.get_mut(a), point) {
                    t.term.selection = Some(Selection::new(SelectionType::Simple, pt, Side::Left));
                    self.drag = Drag::Select;
                }
            }
            (Drag::Border, true, _) => set_sidebar(s, &mut st, x + 1),
            (Drag::Select, true, _) => {
                if let (Some(t), Some(pt)) = (st.tabs.get_mut(a), point)
                    && let Some(sel) = t.term.selection.as_mut() {
                        sel.update(pt, Side::Right);
                    }
            }
            (Drag::Select, false, false) => {
                self.drag = Drag::None;
                let text = st.tabs.get_mut(a).and_then(|t| {
                    let text = t.term.selection_to_string().filter(|s| !s.trim().is_empty());
                    if text.is_none() {
                        t.term.selection = None;
                    }
                    text
                });
                if let Some(text) = text {
                    copy(&mut st, &text);
                }
            }
            (_, false, false) => self.drag = Drag::None,
            _ => {}
        }
        mark(s, &mut st);
    }
}

/// サーバーと同じ Mac の画面なら pbcopy、ssh 越しの端末には OSC 52 で渡す。
fn copy(st: &mut State, text: &str) {
    if let Ok(mut c) = std::process::Command::new("pbcopy").stdin(std::process::Stdio::piped()).spawn() {
        if let Some(mut i) = c.stdin.take() {
            let _ = i.write_all(text.as_bytes());
        }
        std::thread::spawn(move || c.wait());
    }
    let osc = format!("\x1b]52;c;{}\x07", base64(text.as_bytes())).into_bytes();
    st.clients.retain(|(_, c)| c.send(osc.clone()).is_ok());
}

fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for c in data.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        for k in 0..4 {
            out.push(if k <= c.len() { T[(n >> (18 - 6 * k) & 63) as usize] as char } else { '=' });
        }
    }
    out
}

fn flush(s: &Shared, fwd: &mut Vec<u8>) {
    if fwd.is_empty() {
        return;
    }
    let mut st = lock(s);
    let a = st.active;
    if let Some(t) = st.tabs.get_mut(a) {
        let _ = t.writer.write_all(fwd);
        t.last_used = now_secs();
        let changed = t.term.selection.take().is_some() || t.term.grid().display_offset() != 0;
        t.term.scroll_display(Scroll::Bottom);
        if changed {
            mark(s, &mut st);
        }
    }
    fwd.clear();
}
