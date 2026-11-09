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

/// 再起動の直前に画面へ流す印。受け取ったクライアントは切れた後につなぎ直す
pub const RESTART_MARK: &[u8] = b"\x1b]7777;mdeck-restart\x07";

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
    /// F12 でサイドバーを操作中。cursor は CUR_CONFIG / CUR_EXIT / CUR_TABS.. (タブ) / その次が「+」
    prefix: bool,
    cursor: usize,
    session: Option<SessionPop>,
    sidebar: u16,
    webhooks: Vec<Value>,
    /// owner (Bot 名) ごとの webhook。その owner のタブの通知だけを送る
    owner_webhooks: serde_json::Map<String, Value>,
    /// 最後に描いたポップアップの枠。幅は中身で変わるので、クリックの当たり判定はこれを見る
    popup: std::cell::Cell<Rect>,
}

/// サイドバーでセッションを選んで Enter で開く、そのタブの設定
struct SessionPop {
    tab: u32,
    sel: usize,
    /// 入力中の欄と内容
    edit: Option<(Field, String)>,
    notice: Option<String>,
    /// 「タブを削除する」を選んだ。「削除しない」/「削除する」を選ばせる (エージェントの会話ごと終わるので)
    confirm_close: bool,
    /// webhook の行で ←/→ で選ぶ操作。false は編集、true は削除
    hook_del: bool,
    /// 削除を確かめている webhook の URL
    confirm_hook: Option<String>,
    /// 編集中の webhook の元の URL (None なら追加)
    edit_hook: Option<String>,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Field {
    Name,
    Owner,
    Webhook,
}

#[derive(Clone, PartialEq, Debug)]
enum SessionItem {
    Name,
    Owner,
    Hook(String),
    AddHook,
    Close,
    KeepOpen,
    ConfirmClose,
    KeepHook,
    ConfirmHook,
}

/// サイドバー上部の行。タブの一覧は TABS_TOP から、「+」はその最後尾
const ROW_CONFIG: u16 = 2;
const ROW_EXIT: u16 = 3;
const TABS_TOP: u16 = 5;
/// サイドバー下部でタブを描かない行数 (フックの警告 2 行・F12 の案内)
const FOOTER: u16 = 3;
/// 項目の文字の列。タブは 1 列目にエージェント、2 列目に状態を出す
const LABEL_X: u16 = 4;

/// 画面に並べられるタブの数と「+」の行。入りきらないときも「+」の行は残す
fn tab_rows(tabs: usize, height: u16) -> (usize, u16) {
    let fit = height.saturating_sub(FOOTER + TABS_TOP + 1) as usize;
    let shown = tabs.min(fit);
    (shown, TABS_TOP + shown as u16)
}
const CUR_CONFIG: usize = 0;
const CUR_EXIT: usize = 1;
const CUR_TABS: usize = 2;

struct Menu {
    sel: usize,
    notice: Option<String>,
    /// アップデート確認で見つけた、今より新しい版
    update: Option<String>,
}

const MENU_LEN: usize = 6;
// コンフィグ項目の並び。0 と 1 は hooks::status() の順 (claude, codex)
const M_CHECK: usize = 2;
const M_APPLY: usize = 3;
const M_SIDEBAR: usize = 4;
// API (api.github.com) は未認証だと IP ごとに 1 時間 60 回で尽きるので、リダイレクト先 (.../releases/tag/vX) から読む
const RELEASES_URL: &str = "https://github.com/TsukumiStudio/MornDeck/releases/latest";
// 新しい版を一度起動して初回の検査をここで済ませ、フックや MCP の起動で待たせない。
// 最後にサーバーを新しい版で起動し直す (この更新用のタブは戻さない)。
// brew upgrade の自動 update は間隔 (既定 24 時間) 内だと飛ばされ、tap が古いまま「更新なし」で終わるので先に update する
const UPGRADE_CMD: &str =
    r#"brew update --quiet; HOMEBREW_NO_AUTO_UPDATE=1 HOMEBREW_NO_AUTOREMOVE=1 brew upgrade --cask morndeck && mdeck --version && mdeck api "{\"cmd\":\"restart\",\"except\":$MDECK_TAB}""#;

fn hook_item(hooks: &[(&str, hooks::Status)], i: usize) -> String {
    let Some((a, s)) = hooks.get(i) else { return String::new() };
    // claude / codex の名前を同じ幅に揃え、版の列を縦に並べる
    format!("{a:<6} のフック  {}", s.label())
}

/// フックの項目を選んでいるときに Enter で何をするか
fn hook_verb(s: hooks::Status) -> &'static str {
    match s {
        hooks::Status::NoAgent => "j/k 選択  Esc 戻る  F12 閉じる",
        hooks::Status::Missing => "Enter で導入  j/k 選択  Esc 戻る",
        hooks::Status::Outdated(_) => "Enter で更新  j/k 選択  Esc 戻る",
        hooks::Status::Current => "Enter で入れ直す  j/k 選択  Esc 戻る",
    }
}

fn menu_items(m: &Menu, sidebar: u16, hooks: &[(&str, hooks::Status)]) -> [String; MENU_LEN] {
    [
        hook_item(hooks, 0),
        hook_item(hooks, 1),
        "更新を確認".into(),
        match &m.update {
            Some(v) => format!("更新を反映 (v{} → v{v})", env!("CARGO_PKG_VERSION")),
            None => "更新を反映".into(),
        },
        format!("サイドバーの幅  ← {sidebar} →"),
        "閉じる".into(),
    ]
}

fn version_parts(v: &str) -> Vec<u32> {
    v.trim_start_matches('v').split('.').map(|p| p.parse().unwrap_or(0)).collect()
}

/// GitHub の最新リリースの版。新しいものがあれば Some。
fn check_update() -> Result<Option<String>, String> {
    let out = std::process::Command::new("curl")
        .args(["-fsS", "-m", "10", "-o", "/dev/null", "-w", "%{redirect_url}", RELEASES_URL])
        .output()
        .map_err(|e| format!("curl を起動できない: {e}"))?;
    if !out.status.success() {
        return Err(format!("取得できない: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    let url = String::from_utf8_lossy(&out.stdout);
    let tag = url.rsplit_once("/tag/").map(|x| x.1.trim()).ok_or_else(|| format!("版を読めない: {url}"))?;
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
    /// subscribe に流し、done / choices は全体の webhook と、タブの owner の webhook にも送る。
    fn notify(&mut self, mut v: Value) {
        let owner = v["tab"].as_u64().and_then(|id| self.tab(id as u32)).and_then(|t| t.owner.clone());
        v["owner"] = json!(owner);
        if v["event"] == "choices" || v["state"] == "done" {
            post_webhooks(&self.webhooks, &v);
            if let Some(Value::Array(list)) = owner.and_then(|o| self.owner_webhooks.get(&o)) {
                post_webhooks(list, &v);
            }
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
    // 利用者がフックの版を気にしなくて済むよう、入っていて古いものは起動時に入れ替える
    for (agent, status) in hooks::status() {
        if let hooks::Status::Outdated(_) = status
            && let Err(e) = hooks::install(agent) {
                eprintln!("hook update {agent}: {e}");
            }
    }
    let shared: Shared = Arc::new((
        Mutex::new(State {
            cols: 80,
            rows: 24,
            next_id: 1,
            sock: sock.into(),
            hooks: hooks::status(),
            sidebar: 24,
            webhooks: load_webhooks(sock),
            owner_webhooks: load_owner_webhooks(sock),
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

/// Claude Code はタイトル先頭に状態記号 (✳ や点字スピナー) を付ける。状態は別に出しているので落とす
fn strip_glyph(s: &str) -> &str {
    let mut it = s.chars();
    match (it.next(), it.next()) {
        (Some(c), Some(' ')) if !c.is_alphanumeric() => it.as_str(),
        _ => s,
    }
}

fn handle_term_event(t: &mut Tab, e: Event, cols: u16, rows: u16) {
    match e {
        Event::PtyWrite(s) => {
            let _ = t.writer.write_all(s.as_bytes());
        }
        Event::Title(s) => t.title = strip_glyph(&s).to_string(),
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
        // F12 の操作中は日本語入力の変換候補のためにカーソルを項目へ置くが、文字を入力中でなければ見せない
        if st.prefix || st.menu.is_some() || st.session.as_ref().is_some_and(|p| p.edit.is_none()) {
            out.extend_from_slice(b"\x1b[?25l");
        }
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
    let cursor = |row: usize, base: Style| if st.prefix && st.cursor == row { base.bg(Color::Yellow).fg(Color::Black) } else { base };
    buf.set_stringn(1, 0, "MornDeck", side.width.saturating_sub(1) as usize, bg.add_modifier(Modifier::BOLD));
    buf.set_stringn(1, 1, format!("v{}", env!("CARGO_PKG_VERSION")), side.width.saturating_sub(1) as usize, bg.fg(Color::Indexed(245)));
    let h = side.height;
    let plus = CUR_TABS + st.tabs.len();
    let (shown, plus_y) = tab_rows(st.tabs.len(), h);
    for (row, y, icon, label) in [(CUR_CONFIG, ROW_CONFIG, "⚙", "コンフィグ"), (CUR_EXIT, ROW_EXIT, "⏻", "Exit"), (plus, plus_y, "+", "")] {
        let style = cursor(row, bg);
        buf.set_style(Rect::new(0, y, side.width, 1), style);
        buf.set_stringn(1, y, icon, 2, style);
        buf.set_stringn(LABEL_X, y, label, side.width.saturating_sub(LABEL_X + 1) as usize, style);
    }
    buf.set_stringn(0, TABS_TOP - 1, "─".repeat(side.width as usize), side.width as usize, bg.fg(Color::Indexed(240)));
    for (i, t) in st.tabs.iter().enumerate().take(shown) {
        let y = TABS_TOP + i as u16;
        let (g, gc, label) = state_glyph(t.state);
        let base = if i == st.active { bg.bg(Color::Indexed(24)).add_modifier(Modifier::BOLD) } else { bg };
        let base = cursor(CUR_TABS + i, base);
        buf.set_style(Rect::new(0, y, side.width, 1), base);
        let (icon, ic) = match t.agent.as_deref().or_else(|| fg_agent(t.master.process_group_leader())) {
            Some("claude") => ("✻", Color::Rgb(217, 119, 87)),
            Some("codex") => ("❖", Color::Rgb(16, 163, 127)),
            _ => (" ", Color::Reset),
        };
        buf.set_stringn(1, y, icon, 1, base.fg(ic));
        buf.set_stringn(2, y, g, 1, base.fg(gc));
        let title = t.name.as_ref().unwrap_or(&t.title);
        // ラベルは全角最大 4 文字 (入力待ち)。幅 8 セルに詰めてタイトルの開始位置を揃える
        let pad = " ".repeat(2 * 4usize.saturating_sub(label.chars().count()));
        let name = if label.is_empty() { title.clone() } else { format!("{label}{pad} {title}") };
        buf.set_stringn(LABEL_X, y, name, side.width.saturating_sub(LABEL_X + 1) as usize, base);
    }
    buf.set_stringn(0, h.saturating_sub(1), "F12 でサイドバー操作", side.width as usize, bg.fg(Color::Indexed(250)));
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
        buf.set_stringn(0, h.saturating_sub(2), "  F12 → コンフィグ", side.width as usize, warn);
    }

    match st.tabs.get(st.active) {
        Some(t) => {
            if let Some(cur) = draw_term(buf, main, &t.term) {
                f.set_cursor_position(cur);
            }
        }
        None => {
            buf.set_string(main.x + 1, 0, "タブがない。サイドバーの + で新規", Style::default());
        }
    }
    // サイドバーやメニューの操作中は、日本語入力の変換候補がコンソールに出ないよう、カーソルを選んでいる項目へ置く
    if let Some(m) = &st.menu {
        let pos = draw_menu(f, main, m, st);
        f.set_cursor_position(pos);
    }
    if let Some(p) = &st.session {
        let pos = draw_session(f, main, p, st);
        f.set_cursor_position(pos);
    }
    if st.prefix {
        let y = match st.cursor {
            CUR_CONFIG => ROW_CONFIG,
            CUR_EXIT => ROW_EXIT,
            c => TABS_TOP + (c - CUR_TABS).min(shown) as u16,
        };
        f.set_cursor_position((1, y));
    }
    // F12 操作中 (サイドバー操作・コンフィグ・セッション) は常にフッターを出す
    if st.prefix || st.menu.is_some() || st.session.is_some() {
        let text = if st.prefix { " ↑↓/j/k 選択  Enter 開く  → タブのメニュー  Esc/F12 閉じる" } else { " j/k 選択  Enter 実行  Esc 戻る  F12 閉じる" };
        let bar = Rect::new(0, area.height.saturating_sub(1), area.width, 1);
        // 下の端末の文字が残らないよう、行を消してから書く
        f.render_widget(ratatui::widgets::Clear, bar);
        let on = Style::default().bg(Color::Yellow).fg(Color::Black);
        f.buffer_mut().set_style(bar, on);
        f.buffer_mut().set_stringn(0, bar.y, text, bar.width as usize, on);
    }
}

/// 枠付きのポップアップを area の中央に出す (枠 + 左右 2 / 上下 1 の余白)。
/// 幅は一番長い行に合わせ (最小 60、最大は画面幅)。画面に入らない行は呼ぶ側が fit で詰めておく
fn draw_box(f: &mut Frame, area: Rect, title: &str, lines: Vec<ratatui::text::Line>, st: &State) -> Rect {
    use ratatui::widgets::{Block, Clear, Padding, Paragraph};
    let need = lines.iter().map(|l| l.width() as u16 + BOX_PAD).max().unwrap_or(0);
    let w = need.max(60).min(area.width);
    let h = (lines.len() as u16 + 4).min(area.height);
    let r = Rect::new(area.x + (area.width - w) / 2, area.y + (area.height - h) / 2, w, h);
    f.render_widget(Clear, r);
    f.render_widget(Paragraph::new(lines).block(Block::bordered().title(title).padding(Padding::new(2, 2, 1, 1))), r);
    st.popup.set(r);
    r
}

/// 枠 2 + 余白 4
const BOX_PAD: u16 = 6;

/// ポップアップの 1 行に入る幅
fn box_inner(area: Rect) -> usize {
    area.width.saturating_sub(BOX_PAD) as usize
}

/// 表示幅 max に収める。はみ出すなら真ん中を … にする (URL の頭と末尾が見えるように)。
/// tail なら頭を … にして末尾を見せる (入力中の行)
fn fit(text: &str, max: usize, tail: bool) -> String {
    let w = |s: &str| ratatui::text::Line::from(s).width();
    if w(text) <= max {
        return text.to_string();
    }
    let room = max.saturating_sub(1);
    let take = |it: &mut dyn Iterator<Item = char>, n: usize| {
        let mut out = String::new();
        for c in it {
            if w(&out) + w(c.encode_utf8(&mut [0; 4])) > n {
                break;
            }
            out.push(c);
        }
        out
    };
    let back = |n: usize| take(&mut text.chars().rev(), n).chars().rev().collect::<String>();
    if tail {
        return format!("…{}", back(room));
    }
    let head = take(&mut text.chars(), room - room / 2);
    format!("{head}…{}", back(room / 2))
}

/// 本文の行 (枠と上の余白の下が 0)。枠の外なら None。
fn box_line(r: Rect, x: u16, y: u16) -> Option<usize> {
    (x >= r.x && x < r.x + r.width && y >= r.y + 2 && y < r.y + r.height.saturating_sub(2)).then(|| (y - r.y - 2) as usize)
}

fn session_items(st: &State, p: &SessionPop) -> Vec<(SessionItem, String)> {
    let Some(t) = st.tabs.iter().find(|t| t.id == p.tab) else { return Vec::new() };
    if p.confirm_close {
        return vec![(SessionItem::KeepOpen, "削除しない".into()), (SessionItem::ConfirmClose, "削除する".into())];
    }
    if p.confirm_hook.is_some() {
        return vec![(SessionItem::KeepHook, "削除しない".into()), (SessionItem::ConfirmHook, "削除する".into())];
    }
    let mut v = vec![
        (SessionItem::Name, format!("名前: {}", t.name.as_deref().unwrap_or("(なし)"))),
        (SessionItem::Owner, format!("owner: {}", t.owner.as_deref().unwrap_or("(なし)"))),
    ];
    if let Some(o) = &t.owner {
        let hooks = st.owner_webhooks.get(o).and_then(Value::as_array).cloned().unwrap_or_default();
        for h in hooks {
            let url = h["url"].as_str().unwrap_or("").to_string();
            v.push((SessionItem::Hook(url.clone()), format!("  webhook {url}")));
        }
        v.push((SessionItem::AddHook, "  + webhook を追加".into()));
    }
    v.push((SessionItem::Close, "タブを削除する".into()));
    v
}

/// 戻り値は選んでいる項目の位置。入力中なら入力の末尾。日本語入力の変換候補はカーソルの位置に出るので、そこへ置く
fn draw_session(f: &mut Frame, area: Rect, p: &SessionPop, st: &State) -> (u16, u16) {
    use ratatui::text::Line;
    let items = session_items(st, p);
    let title = st.tabs.iter().find(|t| t.id == p.tab).map(|t| t.name.clone().unwrap_or_else(|| t.title.clone())).unwrap_or_default();
    // 見出しは 2 行 (クリックの位置合わせが同じになるよう、確認でも 2 行)
    let red = Style::default().fg(Color::LightRed).add_modifier(Modifier::BOLD);
    let max = box_inner(area);
    let mut lines = if p.confirm_close {
        vec![Line::styled(fit(&format!("タブ {}  {title} を削除する？", p.tab), max, false), red), Line::from("中で動いている claude / codex も終了する")]
    } else if let Some(url) = &p.confirm_hook {
        vec![Line::styled("この webhook を削除する？", red), Line::from(fit(url, max, false))]
    } else {
        vec![Line::from(fit(&format!("タブ {}  {title}", p.tab), max, false)), Line::from("")]
    };
    let mut cursor_x = 0;
    for (i, (item, label)) in items.iter().enumerate() {
        let style = if i == p.sel { Style::default().add_modifier(Modifier::REVERSED) } else { Style::default() };
        let label = match (&p.edit, item) {
            (Some((Field::Name, b)), SessionItem::Name) => format!("名前: {b}▏"),
            (Some((Field::Owner, b)), SessionItem::Owner) => format!("owner: {b}▏"),
            (Some((Field::Webhook, b)), SessionItem::AddHook) if p.edit_hook.is_none() => format!("  URL: {b}▏"),
            (Some((Field::Webhook, b)), SessionItem::Hook(u)) if p.edit_hook.as_ref() == Some(u) => format!("  URL: {b}▏"),
            _ => label.clone(),
        };
        // 前後の空白 2 つ分。選んでいる webhook の行は後ろの「編集」「削除」の分も空ける。入力中は末尾 (入力位置) を見せる
        let editing = p.edit.is_some() && i == p.sel;
        // webhook の行は選ばなくても後ろの「編集」「削除」の分の幅を取り、選んだときに箱の幅が変わらないようにする
        let wide = matches!(item, SessionItem::Hook(_)) && !editing;
        let hook_row = wide && i == p.sel && p.edit.is_none();
        let label = fit(&label, max.saturating_sub(if wide { 16 } else { 2 }), editing);
        if i == p.sel {
            cursor_x = if p.edit.is_some() { Line::from(format!(" {label}")).width() as u16 - 1 } else { 1 };
        }
        // 選んでいる webhook の行は、後ろの「編集」「削除」のどちらかを反転する
        if hook_row {
            use ratatui::text::Span;
            let on = Style::default().add_modifier(Modifier::REVERSED);
            let (e, d) = if p.hook_del { (Style::default(), on.fg(Color::LightRed)) } else { (on, Style::default()) };
            lines.push(Line::from(vec![Span::raw(format!(" {label}  ")), Span::styled(" 編集 ", e), Span::raw(" "), Span::styled(" 削除 ", d)]));
            continue;
        }
        let style = if matches!(item, SessionItem::ConfirmClose | SessionItem::ConfirmHook) { style.fg(Color::LightRed) } else { style };
        let pad = " ".repeat(if wide { 15 } else { 1 });
        lines.push(Line::styled(format!(" {label}{pad}"), style));
    }
    let hint = match (&p.edit, items.get(p.sel).map(|x| &x.0)) {
        (Some((Field::Webhook, _)), _) if p.edit_hook.is_some() => "Enter 保存  Esc/F12 取消",
        (Some((Field::Webhook, _)), _) => "Enter 追加  Esc 取消",
        (Some(_), _) => "Enter 決定  Esc 取消  空で外す",
        (None, Some(SessionItem::Hook(_))) => "←/→ 編集・削除  Enter 実行  Esc 戻る",
        (None, _) if p.confirm_close || p.confirm_hook.is_some() => "「削除する」を選んで Enter  Esc やめる",
        _ => "j/k 選択  Enter 実行  ←/Esc 戻る",
    };
    lines.push(Line::from(""));
    lines.push(Line::styled(fit(p.notice.as_deref().unwrap_or(hint), max, false), Style::default().fg(Color::LightGreen)));
    let r = draw_box(f, area, " セッション ", lines, st);
    (r.x + 3 + cursor_x, r.y + 4 + p.sel as u16)
}

fn open_session(st: &mut State, idx: usize) {
    let Some(t) = st.tabs.get(idx) else { return };
    st.session = Some(SessionPop { tab: t.id, sel: 0, edit: None, notice: None, confirm_close: false, hook_del: false, confirm_hook: None, edit_hook: None });
    st.prefix = false;
}

fn open_menu(st: &mut State) {
    st.hooks = hooks::status();
    st.menu = Some(Menu { sel: 0, notice: None, update: None });
    st.prefix = false;
}

fn session_input(s: &Shared, rest: &[u8]) -> usize {
    let starts = |a: &[u8], b: &[u8]| rest.starts_with(a) || rest.starts_with(b);
    let (up, down) = (starts(b"\x1b[A", b"\x1bOA"), starts(b"\x1b[B", b"\x1bOB"));
    let mut st = lock(s);
    let Some(mut p) = st.session.take() else { return 0 };
    let Some(idx) = st.tabs.iter().position(|t| t.id == p.tab) else {
        mark(s, &mut st);
        return 0; // タブが消えたら閉じて、入力はそのまま流す
    };
    let items = session_items(&st, &p);
    let mut used = if rest[0] == 0x1b { esc_len(rest) } else { 1 };
    let mut open = true;
    if let Some((field, buf)) = p.edit.as_mut() {
        match rest[0] {
            b'\r' => {
                let (field, value) = (*field, buf.trim().to_string());
                let res = match field {
                    Field::Name => {
                        let r = if value.is_empty() { Ok(()) } else { st.check_name(&value, Some(p.tab)) };
                        r.map(|_| st.tabs[idx].name = (!value.is_empty()).then_some(value))
                    }
                    Field::Owner => {
                        let r = if value.is_empty() { Ok(()) } else { validate_name(&value, std::iter::empty()) };
                        r.map(|_| st.tabs[idx].owner = (!value.is_empty()).then_some(value))
                    }
                    // owner が無いと全体の一覧を触ってしまうので、owner のあるタブだけで扱う
                    Field::Webhook => {
                        let Some(owner) = st.tabs[idx].owner.clone() else {
                            st.session = Some(p);
                            mark(s, &mut st);
                            return used;
                        };
                        // 編集は同じ位置で URL だけ差し替え、ヘッダーは残す
                        let req = match &p.edit_hook {
                            Some(old) if *old == value => json!(null),
                            Some(old) => {
                                let mut list = st.owner_webhooks.get(&owner).and_then(Value::as_array).cloned().unwrap_or_default();
                                list.iter_mut().filter(|h| h["url"] == old.as_str()).for_each(|h| h["url"] = json!(value));
                                json!({"cmd": "webhooks", "owner": owner, "set": list})
                            }
                            None => json!({"cmd": "webhooks", "owner": owner, "add": value}),
                        };
                        drop(st);
                        let r = if req.is_null() { Ok(()) } else { api(s, &req).map(|_| ()) };
                        st = lock(s);
                        r
                    }
                };
                match res {
                    Ok(()) => {
                        p.edit = None;
                        p.edit_hook = None;
                        p.notice = Some("変更した".into());
                    }
                    Err(e) => p.notice = Some(e),
                }
            }
            0x1b => {
                p.edit = None;
                p.edit_hook = None;
                p.notice = None;
            }
            0x7f | 0x08 => drop(buf.pop()),
            c if c < 0x20 => {}
            _ => {
                let n = rest.iter().position(|c| *c < 0x20 || *c == 0x7f).unwrap_or(rest.len());
                buf.push_str(&String::from_utf8_lossy(&rest[..n]));
                p.notice = None;
                used = n;
            }
        }
    } else {
        p.notice = None;
        let last = items.len().saturating_sub(1);
        let (left, right) = (starts(b"\x1b[D", b"\x1bOD"), starts(b"\x1b[C", b"\x1bOC"));
        let on_hook = matches!(items.get(p.sel), Some((SessionItem::Hook(_), _)));
        if up || down || matches!(rest[0], b'j' | b'k') {
            p.hook_del = false;
        }
        match rest[0] {
            _ if on_hook && (left || right) => p.hook_del = right,
            _ if up => p.sel = p.sel.saturating_sub(1),
            _ if down => p.sel = (p.sel + 1).min(last),
            b'k' => p.sel = p.sel.saturating_sub(1),
            b'j' => p.sel = (p.sel + 1).min(last),
            b'\r' => match items.get(p.sel).map(|x| x.0.clone()) {
                Some(SessionItem::Name) => p.edit = Some((Field::Name, st.tabs[idx].name.clone().unwrap_or_default())),
                Some(SessionItem::Owner) => p.edit = Some((Field::Owner, st.tabs[idx].owner.clone().unwrap_or_default())),
                Some(SessionItem::AddHook) => p.edit = Some((Field::Webhook, "https://".into())),
                // 削除はいきなりせず確認を挟む (既定は「削除しない」)
                Some(SessionItem::Hook(url)) if p.hook_del => {
                    p.confirm_hook = Some(url);
                    p.sel = 0;
                }
                Some(SessionItem::Hook(url)) => {
                    p.edit = Some((Field::Webhook, url.clone()));
                    p.edit_hook = Some(url);
                }
                Some(SessionItem::KeepHook) => keep_hook(&st, &mut p),
                Some(SessionItem::ConfirmHook) => {
                    let (owner, url) = (st.tabs[idx].owner.clone(), p.confirm_hook.take());
                    drop(st);
                    let r = api(s, &json!({"cmd": "webhooks", "owner": owner, "remove": url}));
                    st = lock(s);
                    p.hook_del = false;
                    p.sel = 0;
                    p.notice = Some(match r {
                        Ok(_) => "削除した".into(),
                        Err(e) => e,
                    });
                }
                Some(SessionItem::ConfirmClose) => {
                    let _ = st.tabs[idx].child.kill();
                    open = false;
                }
                // 既定は「閉じない」。Enter を続けて押しても閉じない
                Some(SessionItem::Close) => {
                    p.confirm_close = true;
                    p.sel = 0;
                }
                Some(SessionItem::KeepOpen) => {
                    p.confirm_close = false;
                    p.sel = session_items(&st, &p).iter().position(|x| x.0 == SessionItem::Close).unwrap_or(0);
                }
                None => {
                    open = false;
                    st.prefix = true;
                    st.cursor = CUR_TABS + idx;
                }
            },
            // → は webhook の行でだけ使う。Esc と同じ 0x1b で始まるので先に拾う
            _ if right => {}
            // 確認中の Esc は閉じるのをやめるだけ
            b'q' | 0x1b if p.confirm_close => {
                p.confirm_close = false;
                p.sel = session_items(&st, &p).iter().position(|x| x.0 == SessionItem::Close).unwrap_or(0);
            }
            b'q' | 0x1b if p.confirm_hook.is_some() => keep_hook(&st, &mut p),
            // Esc と ← はサイドバーへ戻る
            b'q' | 0x1b => {
                open = false;
                st.prefix = true;
                st.cursor = CUR_TABS + idx;
            }
            _ => {}
        }
        let n = session_items(&st, &p).len();
        p.sel = p.sel.min(n.saturating_sub(1));
    }
    if open {
        st.session = Some(p);
    }
    mark(s, &mut st);
    used
}

/// webhook の削除をやめ、その行の「削除」を選んだ状態へ戻す。
fn keep_hook(st: &State, p: &mut SessionPop) {
    let hook = p.confirm_hook.take().map(SessionItem::Hook);
    p.sel = session_items(st, p).iter().position(|x| Some(&x.0) == hook.as_ref()).unwrap_or(0);
    p.hook_del = true;
}

/// 矢印などのエスケープ列は丸ごと 1 つとして数える。
fn esc_len(rest: &[u8]) -> usize {
    if rest.get(1) == Some(&b'[') || rest.get(1) == Some(&b'O') {
        rest.iter().skip(2).position(|c| c.is_ascii_alphabetic() || *c == b'~').map_or(rest.len(), |p| p + 3)
    } else {
        1
    }
}

fn draw_menu(f: &mut Frame, area: Rect, m: &Menu, st: &State) -> (u16, u16) {
    use ratatui::text::Line;
    let pick = |i: usize, sel: usize| if i == sel { Style::default().add_modifier(Modifier::REVERSED) } else { Style::default() };
    let mut lines = Vec::new();
    for (i, item) in menu_items(m, st.sidebar, &st.hooks).iter().enumerate() {
        let color = match st.hooks.get(i).map(|h| h.1) {
            Some(hooks::Status::Current) => Color::LightGreen,
            Some(hooks::Status::NoAgent) => Color::DarkGray,
            Some(_) => Color::Yellow,
            None => Color::Reset,
        };
        lines.push(Line::styled(format!(" {} ", fit(item, box_inner(area).saturating_sub(2), false)), pick(i, m.sel).fg(color)));
    }
    let hint = st.hooks.get(m.sel).map_or("j/k 選択  Enter 実行  ←/→ 幅  Esc 戻る  F12 閉じる", |h| hook_verb(h.1));
    lines.push(Line::from(""));
    lines.push(Line::styled(fit(m.notice.as_deref().unwrap_or(hint), box_inner(area), false), Style::default().fg(Color::LightGreen)));
    let r = draw_box(f, area, " コンフィグ ", lines, st);
    (r.x + 3, r.y + 2 + m.sel as u16)
}

fn menu_input(s: &Shared, rest: &[u8]) -> usize {
    let starts = |a: &[u8], b: &[u8]| rest.starts_with(a) || rest.starts_with(b);
    let (up, down) = (starts(b"\x1b[A", b"\x1bOA"), starts(b"\x1b[B", b"\x1bOB"));
    let (left, right) = (starts(b"\x1b[D", b"\x1bOD"), starts(b"\x1b[C", b"\x1bOC"));
    let mut st = lock(s);
    let Some(mut m) = st.menu.take() else { return 0 };
    let used = if rest[0] == 0x1b { esc_len(rest) } else { 1 };
    let mut open = true;
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
        b'\r' if sel < M_CHECK => {
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
        b'\r' if sel == M_APPLY && m.update.is_some() => {
            // 進み具合と失敗が見えるよう、更新は新しいタブで走らせる
            drop(st);
            let _ = new_tab(s, None, Some(UPGRADE_CMD), None, true);
            return 1;
        }
        b'\r' if sel == M_APPLY => m.notice = Some("新しい版が無い。先に「更新を確認」".into()),
        b'\r' if sel == M_CHECK => {
            m.notice = Some("確認中…".into());
            let s2 = s.clone();
            std::thread::spawn(move || {
                let r = check_update();
                let mut st = lock(&s2);
                if let Some(m) = st.menu.as_mut() {
                    m.notice = Some(match &r {
                        Ok(Some(v)) => format!("v{v} がある。「更新を反映」で入れ替える"),
                        Ok(None) => format!("最新 (v{})", env!("CARGO_PKG_VERSION")),
                        Err(e) => format!("確認失敗: {e}"),
                    });
                    m.update = r.ok().flatten();
                    mark(&s2, &mut st);
                }
            });
        }
        b'\r' if sel == M_SIDEBAR => {}
        b'\r' => open = false,
        // Esc と ← (幅の項目以外) はサイドバーへ戻る
        b'q' | 0x1b => {
            open = false;
            st.prefix = true;
            st.cursor = CUR_CONFIG;
        }
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
            // 起動直後の done から届くよう、タブを作る前に登録する
            if !req["webhook"].is_null() {
                ensure_owner_webhook(s, owner.as_deref(), &req["webhook"])?;
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
            if !req["webhook"].is_null() {
                let owner = {
                    let st = lock(s);
                    let i = st.find(&req["tab"])?;
                    st.tabs[i].owner.clone()
                };
                ensure_owner_webhook(s, owner.as_deref(), &req["webhook"])?;
            }
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
        "restart" => {
            let (s, except) = (s.clone(), req["except"].as_u64());
            // 先に呼んだ側へ返事を返す
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(200));
                restart(&s, except);
            });
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
            // owner を渡すとその Bot 専用の一覧だけを読み書きする (全体と他の Bot の登録には触れない)
            let owner = req["owner"].as_str();
            if let Some(o) = owner {
                validate_name(o, std::iter::empty())?;
            }
            let current: Vec<Value> = match owner {
                Some(o) => st.owner_webhooks.get(o).and_then(Value::as_array).cloned().unwrap_or_default(),
                None => st.webhooks.clone(),
            };
            if let Some(hooks) = edit_webhooks(&current, req)? {
                let mut all = st.owner_webhooks.clone();
                match owner {
                    Some(o) if hooks.is_empty() => drop(all.remove(o)),
                    Some(o) => drop(all.insert(o.to_string(), json!(hooks))),
                    None => {}
                }
                let (path, body) = match owner {
                    Some(_) => (owner_webhooks_path(&st.sock), Value::Object(all.clone())),
                    None => (webhooks_path(&st.sock), json!(hooks)),
                };
                std::fs::write(&path, body.to_string()).map_err(|e| e.to_string())?;
                // ヘッダーに送信キーを含むので本人だけが読めるようにする
                let _ = std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o600));
                match owner {
                    Some(_) => st.owner_webhooks = all,
                    None => st.webhooks = hooks,
                }
            }
            let list = match owner {
                Some(o) => st.owner_webhooks.get(o).and_then(Value::as_array).cloned().unwrap_or_default(),
                None => st.webhooks.clone(),
            };
            let shown: Vec<Value> = list.iter().map(|w| {
                let names: Vec<&String> = w["headers"].as_object().map(|h| h.keys().collect()).unwrap_or_default();
                json!({"url": w["url"], "headers": names})
            }).collect();
            Ok(json!({"ok": true, "owner": owner, "webhooks": shown}))
        }
        "hook" => {
            // v2 のフックはエージェントの JSON を payload のまま送ってくる
            let p = &req["payload"];
            let req = &if p.is_object() {
                json!({"tab": req["tab"], "agent": req["agent"], "event": p["hook_event_name"], "message": p["last_assistant_message"],
                       "tool": p["tool_name"], "input": p["tool_input"], "session": p["session_id"]})
            } else {
                req.clone()
            };
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
                "timeout: 起動を確認できない。フックが未導入か古い (F12 → コンフィグ)"
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
        if body != last && write_atomic(&path, &body).is_ok() {
            last = body;
        }
    }
}

fn write_atomic(path: &std::path::Path, body: &str) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, body).and_then(|_| std::fs::rename(&tmp, path))
}

/// 新しい版のサーバーに入れ替える。タブを保存して閉じ、同じソケットで起動し直す。
/// claude / codex は保存した会話 ID で再開し、画面のクライアントは RESTART_MARK を見てつなぎ直す。
fn restart(s: &Shared, except: Option<u64>) {
    let mut st = lock(s); // 保存し終わるまで save_loop に上書きさせない
    let mut snap = snapshot(&st);
    if let (Some(e), Some(tabs)) = (except, snap["tabs"].as_array_mut()) {
        tabs.retain(|t| t["id"].as_u64() != Some(e));
    }
    if let Err(e) = write_atomic(&tabs_path(&st.sock), &snap.to_string()) {
        eprintln!("restart: save tabs: {e}");
        return;
    }
    st.clients.retain(|(_, c)| c.send(RESTART_MARK.to_vec()).is_ok());
    std::thread::sleep(Duration::from_millis(300)); // クライアントへ印が届くのを待つ
    for t in &mut st.tabs {
        let _ = t.child.kill();
    }
    use std::os::unix::process::CommandExt;
    let e = std::process::Command::new(server_exe()).arg("server").exec();
    // exec できなければ終わる。次に画面をつないだときに mdeck が起動し直す
    eprintln!("restart: exec: {e}");
    std::process::exit(1);
}

/// upgrade で起動した版の実体は消えるので、起動に使ったパス (brew のリンク) か PATH 上の mdeck を使う。
fn server_exe() -> std::path::PathBuf {
    std::env::args()
        .next()
        .map(std::path::PathBuf::from)
        .filter(|p| p.is_absolute() && p.is_file())
        .or_else(|| std::env::var_os("PATH").and_then(|p| std::env::split_paths(&p).map(|d| d.join("mdeck")).find(|f| f.is_file())))
        .unwrap_or_else(|| "mdeck".into())
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

/// 全体の webhooks.json は配列のまま残し、owner ごとの分は別ファイルに置く (既存の登録と旧版を壊さない)
fn owner_webhooks_path(sock: &str) -> std::path::PathBuf {
    std::path::Path::new(sock).with_file_name("owner_webhooks.json")
}

/// new / send の webhook 引数。owner の一覧に入れる。同じ URL・同じヘッダーなら何もせず
/// (Bot が毎回渡しても増えない)、ヘッダーだけ違えば上書きする。
fn ensure_owner_webhook(s: &Shared, owner: Option<&str>, hook: &Value) -> Result<(), String> {
    let owner = owner.ok_or("webhook を付けるには owner が要る (new_tab で owner を付けたタブだけ)")?;
    let hook = parse_webhook(hook)?;
    let same = lock(s).owner_webhooks.get(owner).and_then(Value::as_array).is_some_and(|l| l.contains(&hook));
    if !same {
        api(s, &json!({"cmd": "webhooks", "owner": owner, "add": hook}))?;
    }
    Ok(())
}

/// set / add / remove を current に当てた結果。変更が無ければ None。
fn edit_webhooks(current: &[Value], req: &Value) -> Result<Option<Vec<Value>>, String> {
    let mut next = None;
    if let Some(list) = req["set"].as_array() {
        next = Some(list.iter().map(parse_webhook).collect::<Result<Vec<_>, _>>()?);
    }
    // 複数の Bot が互いの登録を消さないよう、1 件ずつの追加と削除を用意する
    if !req["add"].is_null() {
        let hook = parse_webhook(&req["add"])?;
        let mut list = next.unwrap_or_else(|| current.to_vec());
        list.retain(|w| w["url"] != hook["url"]);
        list.push(hook);
        next = Some(list);
    }
    if let Some(url) = req["remove"].as_str() {
        let mut list = next.unwrap_or_else(|| current.to_vec());
        list.retain(|w| w["url"] != url);
        next = Some(list);
    }
    Ok(next)
}

fn load_owner_webhooks(sock: &str) -> serde_json::Map<String, Value> {
    let v: Value = std::fs::read_to_string(owner_webhooks_path(sock)).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
    v.as_object()
        .into_iter()
        .flatten()
        .filter_map(|(o, list)| {
            let hooks: Vec<Value> = list.as_array()?.iter().filter_map(|h| parse_webhook(h).ok()).collect();
            (!hooks.is_empty()).then(|| (o.clone(), json!(hooks)))
        })
        .collect()
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
    #[test]
    fn fit_keeps_width() {
        use super::fit;
        let w = |s: &str| ratatui::text::Line::from(s).width();
        assert_eq!(fit("short", 10, false), "short");
        let url = "https://example.com/aaaaaaaaaa/bbbbbbbbbb/cccccccccc";
        let m = fit(url, 20, false);
        assert!(w(&m) <= 20 && m.starts_with("https://") && m.ends_with("cccc") && m.contains('…'), "{m}");
        let t = fit(url, 20, true);
        assert!(w(&t) <= 20 && t.starts_with('…') && t.ends_with("cccc"), "{t}");
        let j = fit("あいうえおかきくけこ", 9, false);
        assert!(w(&j) <= 9, "{j}");
    }

    use serde_json::json;

    #[test]
    fn strip_glyph_drops_only_a_leading_symbol() {
        assert_eq!(super::strip_glyph("✳ 作業名"), "作業名");
        assert_eq!(super::strip_glyph("⠐ task"), "task");
        assert_eq!(super::strip_glyph("~/git"), "~/git");
        assert_eq!(super::strip_glyph("a b"), "a b");
    }

    #[test]
    fn f12_is_never_forwarded_and_closes_what_is_open() {
        let s: super::Shared = Default::default();
        let mut inp = super::Input::default();
        let f12 = super::PREFIX;
        let state = |s: &super::Shared| {
            let st = super::lock(s);
            (st.prefix, st.cursor, st.menu.is_some())
        };
        // タブが無いとカーソルは「+」。上に Exit、その上にコンフィグ
        inp.feed(&s, f12);
        assert_eq!(state(&s), (true, super::CUR_TABS, false));
        inp.feed(&s, f12);
        assert!(!state(&s).0, "F12 F12 は閉じるだけ");
        inp.feed(&s, b"\x1b[24~m");
        assert_eq!(state(&s), (true, super::CUR_TABS, false), "ショートカットは無い");
        inp.feed(&s, b"kk\r");
        assert_eq!(state(&s), (false, super::CUR_CONFIG, true));
        inp.feed(&s, f12);
        assert_eq!((state(&s).0, state(&s).2), (false, false), "メニューを開いた状態の F12 は閉じる");
        inp.feed(&s, f12);
        assert!(inp.feed(&s, b"k\r"), "Exit で離脱");
    }

    #[test]
    fn wheel_goes_to_apps_that_ask_for_mouse_reports() {
        use super::TermMode as M;
        assert_eq!(super::mouse_report(M::empty(), 64, 3, 4), None);
        assert_eq!(super::mouse_report(M::MOUSE_REPORT_CLICK | M::SGR_MOUSE, 65, 3, 4).unwrap(), b"\x1b[<65;3;4M");
        assert_eq!(super::mouse_report(M::MOUSE_DRAG, 64, 1, 300).unwrap(), [0x1b, b'[', b'M', 96, 33, 255]);
    }

    #[test]
    fn bind_sock_keeps_live_server_and_replaces_stale_socket() {
        let _fork = crate::FORK_LOCK.lock().unwrap_or_else(|e| e.into_inner());
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
    fn f12_sidebar_opens_config_and_session_settings() {
        let _fork = crate::FORK_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let s: super::Shared = Default::default();
        let dir = std::env::temp_dir().join(format!("mdeck-side-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        {
            let mut st = super::lock(&s);
            st.sock = dir.join("t.sock").to_string_lossy().into_owned();
            st.cols = 100;
            st.rows = 30;
            st.sidebar = 24;
        }
        let cwd = dir.to_str().unwrap();
        let (a, b) = (super::new_tab(&s, Some(cwd), None, None, false).unwrap(), super::new_tab(&s, Some(cwd), None, None, false).unwrap());
        let mut inp = super::Input::default();
        let f12 = super::PREFIX;
        let ui = |s: &super::Shared| {
            let st = super::lock(s);
            (st.prefix, st.cursor, st.menu.is_some(), st.session.as_ref().map(|p| (p.tab, p.sel)))
        };
        let tab0 = super::CUR_TABS;
        // F12 でカーソルは表示中のタブ。上でコンフィグ (Exit の上)、Enter で開く
        inp.feed(&s, f12);
        assert_eq!(ui(&s), (true, tab0, false, None));
        inp.feed(&s, b"kk\r");
        assert_eq!(ui(&s), (false, super::CUR_CONFIG, true, None));
        inp.feed(&s, b"\x1b");
        assert_eq!(ui(&s), (true, super::CUR_CONFIG, false, None), "コンフィグの Esc はサイドバーへ戻る");
        inp.feed(&s, b"\x1b[C");
        assert_eq!(ui(&s), (false, super::CUR_CONFIG, true, None), "→ でもコンフィグを開く");
        inp.feed(&s, b"\x1b");
        inp.feed(&s, f12);

        // 下でタブを選ぶとそのタブを表示し、Enter でそのタブのメニュー
        inp.feed(&s, f12);
        inp.feed(&s, b"j");
        assert_eq!(super::lock(&s).active, 1);
        inp.feed(&s, b"\x1b[C");
        assert_eq!(ui(&s), (false, tab0 + 1, false, Some((b, 0))), "→ でタブのメニューを開かない");
        inp.feed(&s, b"\x1b[C");
        assert_eq!(ui(&s), (false, tab0 + 1, false, Some((b, 0))), "セッションの → で閉じた");
        inp.feed(&s, b"\x1b[D");
        assert_eq!(ui(&s), (true, tab0 + 1, false, None), "← でサイドバーへ戻らない");
        inp.feed(&s, b"\r");
        assert_eq!(ui(&s), (false, tab0 + 1, false, None), "タブの Enter で作業に戻らない");
        inp.feed(&s, f12);
        inp.feed(&s, b"\x1b[C");

        // 名前と owner を変え、owner の webhook を足して消す
        inp.feed(&s, b"\r");
        inp.feed(&s, b"alpha\r");
        inp.feed(&s, b"j\r");
        inp.feed(&s, b"botX\r");
        {
            let st = super::lock(&s);
            let t = st.tabs.iter().find(|t| t.id == b).unwrap();
            assert_eq!((t.name.as_deref(), t.owner.as_deref()), (Some("alpha"), Some("botX")));
        }
        inp.feed(&s, b"j\r");
        inp.feed(&s, b"x.example/h\r");
        assert_eq!(super::lock(&s).owner_webhooks["botX"][0]["url"], "https://x.example/h");
        assert!(super::lock(&s).webhooks.is_empty(), "全体の一覧に入った");
        // 追加後は webhook の行が 2 番目に入る。Enter は編集で、Esc なら元の URL のまま
        assert_eq!(ui(&s).3, Some((b, 2)));
        let url = |s: &super::Shared| super::lock(s).owner_webhooks["botX"][0]["url"].clone();
        inp.feed(&s, b"\r");
        inp.feed(&s, b"zz");
        inp.feed(&s, b"\x1b");
        assert_eq!(url(&s), "https://x.example/h", "Esc で保存した");
        inp.feed(&s, b"\r");
        inp.feed(&s, b"2\r");
        assert_eq!(url(&s), "https://x.example/h2", "Enter で保存しない");
        // → で「削除」を選ぶと確認になり、既定の「削除しない」では消さない
        inp.feed(&s, b"\x1b[C");
        inp.feed(&s, b"\r");
        assert!(super::lock(&s).session.as_ref().unwrap().confirm_hook.is_some());
        inp.feed(&s, b"\r");
        assert_eq!(url(&s), "https://x.example/h2", "「削除しない」で消した");
        inp.feed(&s, b"\r");
        inp.feed(&s, b"j\r");
        assert!(!super::lock(&s).owner_webhooks.contains_key("botX"));
        // 名前の重複は断る
        inp.feed(&s, b"\x1b");
        assert_eq!(ui(&s), (true, tab0 + 1, false, None), "セッションの Esc はサイドバーへ戻る");
        inp.feed(&s, b"k\x1b[C\r");
        inp.feed(&s, b"alpha\r");
        assert!(super::lock(&s).tabs.iter().find(|t| t.id == a).unwrap().name.is_none());
        inp.feed(&s, f12);
        assert_eq!(ui(&s).3, None);

        // 「タブを削除する」は確認になり、既定の「削除しない」で Enter を重ねても閉じない
        inp.feed(&s, f12);
        inp.feed(&s, b"\x1b[Cjj\r");
        assert!(super::lock(&s).session.as_ref().unwrap().confirm_close);
        inp.feed(&s, b"\r");
        assert!(!super::lock(&s).session.as_ref().unwrap().confirm_close, "「閉じない」で確認が終わらない");
        assert_eq!(super::lock(&s).tabs.len(), 2);
        inp.feed(&s, b"\r\x1b");
        assert!(!super::lock(&s).session.as_ref().unwrap().confirm_close, "確認中の Esc でやめない");
        assert_eq!(super::lock(&s).tabs.len(), 2);
        inp.feed(&s, b"\rj\r");
        assert!(super::lock(&s).session.is_none(), "「閉じる」で閉じない");
        // シェルが終わってからタブが消える
        for _ in 0..50 {
            if super::lock(&s).tabs.len() == 1 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        assert_eq!(super::lock(&s).tabs.len(), 1);

        // タブの最後尾の「+」で新規タブ
        inp.feed(&s, f12);
        inp.feed(&s, b"j\r");
        assert_eq!(super::lock(&s).tabs.len(), 2);

        // クリック: コンフィグ → メニューの「閉じる」、タブは選ぶだけで右クリックでメニュー、「+」、Exit
        let click = |x: u16, y: u16| format!("\x1b[<0;{};{}M", x + 1, y + 1).into_bytes();
        inp.feed(&s, &click(2, super::ROW_CONFIG));
        assert!(super::lock(&s).menu.is_some());
        // 箱の位置は描いたときに決まるので、一度描いておく
        {
            let st = super::lock(&s);
            let mut t = ratatui::Terminal::new(ratatui::backend::TestBackend::new(st.cols, st.rows)).unwrap();
            t.draw(|f| super::draw(f, &st)).unwrap();
        }
        // 中央の箱 (幅 60・高さ 項目+2+4) の本文は枠と余白の 2 行下から
        let top = (30 - (super::MENU_LEN as u16 + 6)) / 2 + 2;
        inp.feed(&s, &click(40, top + super::MENU_LEN as u16 - 1));
        assert!(super::lock(&s).menu.is_none(), "「閉じる」のクリックで閉じない");
        let active = super::lock(&s).active as u16;
        inp.feed(&s, &click(5, super::TABS_TOP + active));
        assert!(super::lock(&s).session.is_none(), "タブのクリックでメニューが開いた");
        let other = 1 - active;
        inp.feed(&s, format!("\x1b[<2;6;{}M", super::TABS_TOP + other + 1).as_bytes());
        assert_eq!(super::lock(&s).active as u16, other, "右クリックしたタブを選ばない");
        assert!(super::lock(&s).session.is_some(), "右クリックでメニューが開かない");
        inp.feed(&s, f12);
        inp.feed(&s, &click(2, super::TABS_TOP + 2));
        assert_eq!(super::lock(&s).tabs.len(), 3, "タブの最後尾の「+」で新規タブにならない");
        assert!(inp.feed(&s, &click(2, super::ROW_EXIT)), "Exit のクリックで離脱しない");

        let st = super::lock(&s);
        for t in st.tabs.iter() {
            let _ = std::process::Command::new("kill").arg(t.child.process_id().unwrap().to_string()).status();
        }
        drop(st);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn webhook_arg_registers_once_and_updates_headers() {
        let _fork = crate::FORK_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let s: super::Shared = Default::default();
        let dir = std::env::temp_dir().join(format!("mdeck-wharg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        {
            let mut st = super::lock(&s);
            st.sock = dir.join("t.sock").to_string_lossy().into_owned();
            st.cols = 100;
            st.rows = 30;
            st.sidebar = 24;
        }
        let cwd = dir.to_str().unwrap();
        let hook = |k: &str| json!({"url": "https://g/kime", "headers": {"Authorization": k}});
        let api = |req: serde_json::Value| super::api(&s, &req);
        // owner 無しはタブを作らずに断る
        assert!(api(json!({"cmd": "new", "cwd": cwd, "run": "", "webhook": "https://g/x"})).unwrap_err().contains("owner"));
        assert!(super::lock(&s).tabs.is_empty());
        let id = api(json!({"cmd": "new", "cwd": cwd, "run": "", "owner": "kimekyawa", "webhook": hook("a")})).unwrap()["id"].as_u64().unwrap();
        let mtime = || std::fs::metadata(dir.join("owner_webhooks.json")).unwrap().modified().unwrap();
        let before = mtime();
        std::thread::sleep(std::time::Duration::from_millis(20));
        api(json!({"cmd": "send", "tab": id, "text": "", "enter": false, "webhook": hook("a")})).unwrap();
        assert_eq!(mtime(), before, "同じ登録で書き直した");
        api(json!({"cmd": "send", "tab": id, "text": "", "enter": false, "webhook": hook("b")})).unwrap();
        let list = super::lock(&s).owner_webhooks["kimekyawa"].clone();
        assert_eq!(list, json!([{"url": "https://g/kime", "headers": {"Authorization": "b"}}]));
        // owner 無しのタブへの send は送らずに断る
        let plain = api(json!({"cmd": "new", "cwd": cwd, "run": ""})).unwrap()["id"].as_u64().unwrap();
        assert!(api(json!({"cmd": "send", "tab": plain, "text": "x", "webhook": "https://g/x"})).unwrap_err().contains("owner"));
        assert!(super::lock(&s).webhooks.is_empty());
        for t in super::lock(&s).tabs.iter() {
            let _ = std::process::Command::new("kill").arg(t.child.process_id().unwrap().to_string()).status();
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn owner_webhooks_are_separate_from_global_and_other_owners() {
        let s: super::Shared = Default::default();
        let dir = std::env::temp_dir().join(format!("mdeck-owh-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let sock = dir.join("t.sock").to_string_lossy().into_owned();
        std::fs::write(dir.join("webhooks.json"), r#"["https://g/old"]"#).unwrap();
        {
            let mut st = super::lock(&s);
            st.sock = sock.clone();
            st.webhooks = super::load_webhooks(&sock);
        }
        let api = |req: serde_json::Value| super::api(&s, &req).unwrap();
        api(json!({"cmd": "webhooks", "owner": "botA", "add": "https://a/1"}));
        api(json!({"cmd": "webhooks", "owner": "botB", "add": {"url": "https://b/1", "headers": {"X-Key": "k"}}}));
        // botA が同じ URL を remove しても botB と全体は残る
        api(json!({"cmd": "webhooks", "owner": "botA", "remove": "https://b/1"}));
        api(json!({"cmd": "webhooks", "owner": "botA", "remove": "https://g/old"}));
        let urls = |r: serde_json::Value| r["webhooks"].as_array().unwrap().iter().map(|w| w["url"].as_str().unwrap().to_string()).collect::<Vec<_>>();
        assert_eq!(urls(api(json!({"cmd": "webhooks"}))), ["https://g/old"], "全体の登録が変わった");
        assert_eq!(urls(api(json!({"cmd": "webhooks", "owner": "botA"}))), ["https://a/1"]);
        assert_eq!(urls(api(json!({"cmd": "webhooks", "owner": "botB"}))), ["https://b/1"]);
        // 全体の webhooks.json は配列のまま、owner 分は別ファイルに残り、再起動で読み戻せる
        let global: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(dir.join("webhooks.json")).unwrap()).unwrap();
        assert!(global.is_array());
        let owners = super::load_owner_webhooks(&sock);
        assert_eq!(owners["botB"][0]["headers"]["X-Key"], "k");
        api(json!({"cmd": "webhooks", "owner": "botA", "remove": "https://a/1"}));
        assert!(!super::load_owner_webhooks(&sock).contains_key("botA"), "空になった owner が残っている");
        assert!(super::api(&s, &json!({"cmd": "webhooks", "owner": "a b"})).is_err());
        std::fs::remove_dir_all(dir).unwrap();
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
        let _fork = crate::FORK_LOCK.lock().unwrap_or_else(|e| e.into_inner());
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

/// F12 のサイドバー操作。上下で選び (タブは選ぶと表示する)、Enter か → で開く。タブの行は Enter で作業に戻り、→ でメニュー。戻り値は (使ったバイト数, 離脱するか)。
fn sidebar_input(s: &Shared, rest: &[u8], fwd: &mut Vec<u8>) -> (usize, bool) {
    let starts = |a: &[u8], b: &[u8]| rest.starts_with(a) || rest.starts_with(b);
    let (up, down) = (starts(b"\x1b[A", b"\x1bOA"), starts(b"\x1b[B", b"\x1bOB"));
    let right = starts(b"\x1b[C", b"\x1bOC");
    let mut st = lock(s);
    if !st.prefix {
        return (0, false);
    }
    let used = if rest[0] == 0x1b { esc_len(rest) } else { 1 };
    let plus = CUR_TABS + st.tabs.len();
    st.cursor = st.cursor.min(plus); // 開いている間にタブが減ったとき
    match rest[0] {
        _ if up => st.cursor = st.cursor.saturating_sub(1),
        _ if down => st.cursor = (st.cursor + 1).min(plus),
        b'k' => st.cursor = st.cursor.saturating_sub(1),
        b'j' => st.cursor = (st.cursor + 1).min(plus),
        // タブの行の Enter はそのタブで作業に戻る (メニューは →)
        b'\r' if (CUR_TABS..plus).contains(&st.cursor) => {
            st.active = st.cursor - CUR_TABS;
            st.prefix = false;
        }
        _ if right || rest[0] == b'\r' => {
            let row = st.cursor;
            return (used, activate(s, st, row, fwd));
        }
        0x1b => st.prefix = false,
        _ => {}
    }
    if (CUR_TABS..plus).contains(&st.cursor) {
        st.active = st.cursor - CUR_TABS;
    }
    mark(s, &mut st);
    (used, false)
}

/// サイドバーの項目を開く (Enter とクリック)。戻り値は離脱するか。
fn activate(s: &Shared, mut st: MutexGuard<'_, State>, row: usize, fwd: &mut Vec<u8>) -> bool {
    let plus = CUR_TABS + st.tabs.len();
    match row {
        CUR_CONFIG => open_menu(&mut st),
        CUR_EXIT => {
            st.prefix = false;
            mark(s, &mut st);
            return true;
        }
        r if r < plus => open_session(&mut st, r - CUR_TABS),
        _ => {
            st.prefix = false;
            drop(st);
            flush(s, fwd);
            let _ = new_tab(s, None, None, None, true);
            return false;
        }
    }
    mark(s, &mut st);
    false
}

#[derive(Default)]
struct Input {
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
    /// returns true when the client asked to detach.
    // ponytail: read の境界で分断されたマウス列は中のアプリへそのまま流れる。問題になったら分断分を次回へ持ち越す
    fn feed(&mut self, s: &Shared, data: &[u8]) -> bool {
        let mut fwd = Vec::new();
        let mut i = 0;
        while i < data.len() {
            if data[i..].starts_with(b"\x1b[<")
                && let Some(end) = data[i..].iter().position(|c| *c == b'M' || *c == b'm')
            {
                let seq = &data[i + 3..i + end];
                let press = data[i + end] == b'M';
                flush(s, &mut fwd);
                if self.mouse(s, seq, press, &mut fwd) {
                    return true;
                }
                i += end + 1;
                continue;
            }
            if data[i..].starts_with(PREFIX) {
                // F12 は中に送らない。開いているもの (サイドバー操作・コンフィグ・セッション) を閉じ、何も無ければサイドバー操作に入る
                let mut st = lock(s);
                let closed = st.menu.take().is_some() | st.session.take().is_some() | st.prefix;
                st.prefix = !closed;
                st.cursor = CUR_TABS + st.active;
                mark(s, &mut st);
                i += PREFIX.len();
                continue;
            }
            let used = session_input(s, &data[i..]);
            if used > 0 {
                i += used;
                continue;
            }
            let used = menu_input(s, &data[i..]);
            if used > 0 {
                i += used;
                continue;
            }
            let (used, detach) = sidebar_input(s, &data[i..], &mut fwd);
            if detach {
                flush(s, &mut fwd);
                return true;
            }
            if used > 0 {
                i += used;
                continue;
            }
            fwd.push(data[i]);
            i += 1;
        }
        flush(s, &mut fwd);
        false
    }

    /// returns true when Exit was clicked.
    fn mouse(&mut self, s: &Shared, seq: &[u8], press: bool, fwd: &mut Vec<u8>) -> bool {
        let p: Vec<i32> = std::str::from_utf8(seq).unwrap_or("").split(';').filter_map(|x| x.parse().ok()).collect();
        let [btn, x, y] = p[..] else { return false };
        let (x, y) = (x - 1, y - 1);
        let mut st = lock(s);
        if st.menu.is_some() || st.session.is_some() {
            if btn == 0 && press {
                drop(st);
                popup_click(s, x.max(0) as u16, y.max(0) as u16);
            }
            return false;
        }
        let side = st.sidebar as i32;
        if btn & 64 != 0 {
            let a = st.active;
            let Some(t) = st.tabs.get_mut(a) else { return false };
            let up = btn & 1 == 0;
            let mode = *t.term.mode();
            if let Some(report) = mouse_report(mode, btn, x - side + 1, y + 1) {
                let _ = t.writer.write_all(&report);
            } else if mode.contains(TermMode::ALT_SCREEN | TermMode::ALTERNATE_SCROLL) {
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
            return false;
        }
        // サイドバーのタブの右クリックでそのタブのメニュー
        if btn & 3 == 2 && btn & 32 == 0 && press && x < side - 1 && y >= TABS_TOP as i32 {
            let tab = (y - TABS_TOP as i32) as usize;
            if tab < tab_rows(st.tabs.len(), st.rows).0 {
                st.active = tab;
                return activate(s, st, CUR_TABS + tab, fwd);
            }
        }
        if btn & 3 != 0 {
            return false;
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
                let tab = (y - TABS_TOP as i32) as usize;
                let (shown, plus_y) = tab_rows(st.tabs.len(), st.rows);
                let row = match y {
                    _ if y == ROW_CONFIG as i32 => Some(CUR_CONFIG),
                    _ if y == ROW_EXIT as i32 => Some(CUR_EXIT),
                    _ if y == plus_y as i32 => Some(CUR_TABS + st.tabs.len()),
                    // タブのクリックは選ぶだけ (メニューは右クリック)
                    _ if y >= TABS_TOP as i32 && tab < shown => {
                        st.active = tab;
                        st.prefix = false;
                        None
                    }
                    _ => None,
                };
                if let Some(row) = row {
                    return activate(s, st, row, fwd);
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
        false
    }
}

/// 中のアプリがマウス報告を求めていれば、その形式で送るバイト列 (col, row は 1 始まり)。
/// claude などは自分でスクロールするので、矢印に変えると入力履歴が動いてしまう。
fn mouse_report(mode: TermMode, btn: i32, col: i32, row: i32) -> Option<Vec<u8>> {
    if !mode.intersects(TermMode::MOUSE_MODE) {
        return None;
    }
    let (col, row) = (col.max(1), row.max(1));
    if mode.contains(TermMode::SGR_MOUSE) {
        return Some(format!("\x1b[<{btn};{col};{row}M").into_bytes());
    }
    // 旧形式は 1 バイトで表せる 223 までしか送れない
    let byte = |v: i32| (32 + v.min(223)) as u8;
    Some(vec![0x1b, b'[', b'M', byte(btn), byte(col), byte(row)])
}

/// コンフィグとセッションのポップアップの項目をクリックで選び、Enter と同じに実行する。
fn popup_click(s: &Shared, x: u16, y: u16) {
    let st = lock(s);
    if let Some(p) = &st.session {
        // 見出し 2 行 + 項目 + 空行と案内
        let n = session_items(&st, &p).len();
        let Some(i) = box_line(st.popup.get(), x, y).and_then(|l| l.checked_sub(2)).filter(|i| *i < n) else { return };
        if p.edit.is_some() {
            return;
        }
        drop(st);
        let mut st = lock(s);
        if let Some(p) = st.session.as_mut() {
            p.sel = i;
            p.hook_del = false;
        }
        drop(st);
        session_input(s, b"\r");
    } else if st.menu.is_some() {
        let Some(i) = box_line(st.popup.get(), x, y).filter(|i| *i < MENU_LEN) else { return };
        drop(st);
        let mut st = lock(s);
        if let Some(m) = st.menu.as_mut() {
            m.sel = i;
        }
        drop(st);
        menu_input(s, b"\r");
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
