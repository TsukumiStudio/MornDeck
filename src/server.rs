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
use ratatui::buffer::{Buffer, CellDiffOption};
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
    /// 実行の確認 (Yes / No だけの選択肢) に自動で Yes と答える
    auto_yes: bool,
    /// 完了してから、画面でも API (answer / wait / read など) でもまだ見ていない
    unread: bool,
    /// 自動 Yes に回した確認 (PermissionRequest) を受けた時刻。確認が描かれないまま時間が過ぎたら waiting にする
    auto_wait: Option<std::time::Instant>,
    /// 最後に自動で Yes を押したときの画面。画面が変わるまでは押さない (同じ確認を二度押さない)
    auto_pressed: Option<Vec<String>>,
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
    /// 完了したときの音と通知 (Config で切り替え、config.json に残す)
    alerts: Alerts,
    /// claude / codex の週次と copilot の月次の使用量 (サイドバー下部のゲージ)
    usage: [Option<crate::usage::Week>; 3],
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Alerts {
    sound: bool,
    notify: bool,
    /// webhook の届くタブでは鳴らさない・出さない
    quiet_hooked: bool,
}

impl Default for Alerts {
    fn default() -> Self {
        Alerts { sound: false, notify: false, quiet_hooked: true }
    }
}

fn config_path(sock: &str) -> std::path::PathBuf {
    std::path::Path::new(sock).with_file_name("config.json")
}

fn load_alerts(sock: &str) -> Alerts {
    let v: Value = std::fs::read_to_string(config_path(sock)).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
    let d = Alerts::default();
    let b = |k: &str, d: bool| v[k].as_bool().unwrap_or(d);
    Alerts { sound: b("sound", d.sound), notify: b("notify", d.notify), quiet_hooked: b("quiet_hooked", d.quiet_hooked) }
}

fn save_alerts(sock: &str, a: Alerts) {
    let _ = write_atomic(&config_path(sock), &json!({"sound": a.sound, "notify": a.notify, "quiet_hooked": a.quiet_hooked}).to_string());
}

/// 完了したタブを見ていないときに、音 (システムの Glass) と macOS の通知を出す
fn alert(st: &State, i: usize) {
    let a = st.alerts;
    let t = &st.tabs[i];
    let hooked = !st.webhooks.is_empty() || t.owner.as_ref().and_then(|o| st.owner_webhooks.get(o)).and_then(Value::as_array).is_some_and(|l| !l.is_empty());
    if a.quiet_hooked && hooked {
        return;
    }
    if a.sound {
        let _ = std::process::Command::new("afplay").arg("/System/Library/Sounds/Glass.aiff").spawn().map(|mut c| std::thread::spawn(move || c.wait()));
    }
    if a.notify {
        let name = t.name.clone().unwrap_or_else(|| t.title.clone());
        let text: String = t.answer.as_deref().unwrap_or("完了").chars().take(120).collect();
        // 文字列は引数で渡す (スクリプトに埋めると引用符で壊れる)
        let _ = std::process::Command::new("osascript")
            .args(["-e", "on run argv", "-e", "display notification (item 1 of argv) with title \"MornDeck\" subtitle (item 2 of argv)", "-e", "end run", &text, &format!("{name} が完了")])
            .spawn()
            .map(|mut c| std::thread::spawn(move || c.wait()));
    }
}

/// サイドバーでタブを選んで → で開く、そのタブのメニュー。tab が None なら Config の通知先の一覧
struct SessionPop {
    tab: Option<u32>,
    sel: usize,
    /// 入力中の欄と内容
    edit: Option<(Field, String)>,
    notice: Option<String>,
    /// 「タブを削除する」を選んだ。「削除しない」/「削除する」を選ばせる (エージェントの会話ごと終わるので)
    confirm_close: bool,
    /// 通知先の行で ←/→ で選ぶ操作。false は編集、true は削除
    hook_del: bool,
    /// 削除を確かめている通知先 (名前, URL)
    confirm_hook: Option<(String, String)>,
    /// 編集中の通知先の元の (名前, URL)。None なら追加
    edit_hook: Option<(String, String)>,
    /// 追加中の通知先の名前 (名前を決めて URL を入力中)
    new_dest: Option<String>,
    /// 入力位置。末尾から何文字手前か (0 で末尾。新しく入力を始めると末尾)
    edit_back: usize,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Field {
    Name,
    DestName,
    Webhook,
}

#[derive(Clone, PartialEq, Debug)]
enum SessionItem {
    Name,
    Dest,
    Hook(String, String),
    AddHook,
    AutoYes,
    Close,
    KeepOpen,
    ConfirmClose,
    KeepHook,
    ConfirmHook,
}

/// サイドバー上部の行 (Config, Exit, タブの一覧の先頭)。見出し「MornDeck vX」は入れば 1 行、狭ければ 2 行。「+」はタブの最後尾
fn rows(sidebar: u16) -> (u16, u16, u16) {
    let head = if sidebar as usize >= head_title().chars().count() + 2 { 1 } else { 2 };
    (head, head + 1, head + 3)
}

fn head_title() -> String {
    format!("MornDeck v{}", env!("CARGO_PKG_VERSION"))
}
/// サイドバー下部でタブを描かない行数 (使用量 6 行・フックの警告 2 行・F12 の案内)
const FOOTER: u16 = 9;
/// 項目の文字の列。タブは 1 列目にエージェント、2 列目に状態を出す
const LABEL_X: u16 = 4;

/// 画面に並べられるタブの数と「+」の行。入りきらないときも「+」の行は残す
fn tab_rows(tabs: usize, height: u16, sidebar: u16) -> (usize, u16) {
    let top = rows(sidebar).2;
    let fit = height.saturating_sub(FOOTER + top + 1) as usize;
    let shown = tabs.min(fit);
    (shown, top + shown as u16)
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

/// new_tab の yolo で付ける、確認もサンドボックスも無しで動かすフラグ
const YOLO: [(&str, &str); 3] =
    [("claude", "--dangerously-skip-permissions"), ("codex", "--dangerously-bypass-approvals-and-sandbox"), ("copilot", "--yolo")];
/// タブで起動できるエージェント
const AGENTS: [&str; 3] = ["claude", "codex", "copilot"];

const MENU_LEN: usize = 10;
// コンフィグ項目の並び。0〜2 は hooks::status() の順 (claude, codex, copilot)
/// 「更新  確認する」。新しい版が見つかると「反映する」に変わる
const M_UPDATE: usize = 3;
const M_SIDEBAR: usize = 4;
const M_SOUND: usize = 5;
const M_NOTIFY: usize = 6;
const M_QUIET: usize = 7;
const M_DEST: usize = 8;
// API (api.github.com) は未認証だと IP ごとに 1 時間 60 回で尽きるので、リダイレクト先 (.../releases/tag/vX) から読む
const RELEASES_URL: &str = "https://github.com/TsukumiStudio/MornDeck/releases/latest";
// 新しい版を一度起動して初回の検査をここで済ませ、フックや MCP の起動で待たせない。
// 最後にサーバーを新しい版で起動し直す (この更新用のタブは戻さない)。
// brew upgrade の自動 update は間隔 (既定 24 時間) 内だと飛ばされ、tap が古いまま「更新なし」で終わるので先に update する
const UPGRADE_CMD: &str =
    r#"brew update --quiet; HOMEBREW_NO_AUTO_UPDATE=1 HOMEBREW_NO_AUTOREMOVE=1 HOMEBREW_NO_ASK=1 brew upgrade --cask morndeck && mdeck --version && mdeck api "{\"cmd\":\"restart\",\"except\":$MDECK_TAB}""#;

/// フックの行。「フック」の見出しの下に 1 段下げて並べる
fn hook_item(hooks: &[(&str, hooks::Status)], i: usize) -> (String, String) {
    let Some((a, s)) = hooks.get(i) else { return Default::default() };
    let name = match *a {
        "claude" => "Claude Code",
        "codex" => "Codex",
        "copilot" => "GitHub Copilot",
        other => other,
    };
    (format!("  {name}"), s.label())
}

/// フックの項目を選んでいるときに Enter で何をするか
fn hook_verb(s: hooks::Status) -> &'static str {
    match s {
        hooks::Status::NoAgent => "↑↓/j/k 選択  ← 戻る  F12 閉じる",
        hooks::Status::Missing => "Enter で導入  ↑↓/j/k 選択  ← 戻る",
        hooks::Status::Outdated(_) => "Enter で更新  ↑↓/j/k 選択  ← 戻る",
        hooks::Status::Current => "Enter で入れ直す  ↑↓/j/k 選択  ← 戻る",
    }
}

/// 項目名と設定値の 2 列。設定値の始まりを揃える
fn menu_items(m: &Menu, st: &State) -> [String; MENU_LEN] {
    let (sidebar, hooks, a) = (st.sidebar, &st.hooks, st.alerts);
    let on = |b: bool| if b { "オン".to_string() } else { "オフ".to_string() };
    let items: [(String, String); MENU_LEN] = [
        hook_item(hooks, 0),
        hook_item(hooks, 1),
        hook_item(hooks, 2),
        match &m.update {
            Some(v) => ("更新".into(), format!("反映する (v{} → v{v})", env!("CARGO_PKG_VERSION"))),
            None => ("更新".into(), "確認する".into()),
        },
        ("サイドバーの幅".into(), format!("← {sidebar} →")),
        ("完了したら音を鳴らす".into(), on(a.sound)),
        ("完了したら通知を出す".into(), on(a.notify)),
        ("通知先のあるタブは音・通知なし".into(), on(a.quiet_hooked)),
        ("通知先 (webhook)".into(), format!("{} 件  Enter で一覧", st.owner_webhooks.len())),
        ("閉じる".into(), String::new()),
    ];
    let w = |s: &str| ratatui::text::Line::from(s).width();
    let col = items.iter().map(|(l, _)| w(l)).max().unwrap_or(0) + 2;
    items.map(|(l, v)| if v.is_empty() { l } else { format!("{l}{}{v}", " ".repeat(col - w(&l))) })
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
            "auto_yes": t.auto_yes, "unread": t.unread, "yolo": t.args.as_deref().is_some_and(|a| YOLO.iter().any(|(_, f)| a.contains(f))),
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
            post_webhooks(&self.sock, &self.webhooks, &v);
            if let Some(Value::Array(list)) = owner.and_then(|o| self.owner_webhooks.get(&o)) {
                post_webhooks(&self.sock, list, &v);
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
            alerts: load_alerts(sock),
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
    let r = shared.clone();
    std::thread::spawn(move || usage_loop(r));
    let r = shared.clone();
    std::thread::spawn(move || auto_yes_loop(r));

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
        auto_yes: false,
        unread: false,
        auto_wait: None,
        auto_pressed: None,
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
        // 画面に出したタブは既読
        let active = st.active;
        if !st.clients.is_empty() && let Some(t) = st.tabs.get_mut(active) {
            t.unread = false;
        }
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
            // マウスは端末の中の文字の選択とホイールにだけ使う。どの版の画面がつないでもここで有効にする
            out.extend_from_slice(b"\x1b[?1002h\x1b[?1006h\x1b[0m\x1b[H\x1b[2J");
            term = Some(t);
            modes = None;
        }
        let t = term.as_mut().unwrap();
        // 描画で panic しても描画スレッドを止めない (止まると画面だけ固まる)。次は全面を描き直す
        let ok = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = t.draw(|f| draw(f, &st));
        }))
        .is_ok();
        if !ok {
            term = None;
            st.dirty = true;
            continue;
        }
        let t = term.as_mut().unwrap();
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
    let cursor = |row: usize, base: Style| if st.prefix && st.cursor == row { base.patch(cursor_style()) } else { base };
    let (row_config, row_exit, tabs_top) = rows(side.width);
    let ver = format!("v{}", env!("CARGO_PKG_VERSION"));
    let (vx, vy) = if row_config == 1 { (10, 0) } else { (1, 1) };
    buf.set_stringn(1, 0, "MornDeck", side.width.saturating_sub(1) as usize, bg.add_modifier(Modifier::BOLD));
    buf.set_stringn(vx, vy, ver, side.width.saturating_sub(vx + 1) as usize, bg.fg(Color::Indexed(245)));
    let h = side.height;
    let plus = CUR_TABS + st.tabs.len();
    let (shown, plus_y) = tab_rows(st.tabs.len(), h, side.width);
    for (row, y, icon, label) in [(CUR_CONFIG, row_config, "⚙", "Config"), (CUR_EXIT, row_exit, "✕", "Exit"), (plus, plus_y, "+", "")] {
        let style = cursor(row, bg);
        buf.set_style(Rect::new(0, y, side.width, 1), style);
        buf.set_stringn(1, y, icon, 2, style);
        buf.set_stringn(LABEL_X, y, label, side.width.saturating_sub(LABEL_X + 1) as usize, style);
    }
    buf.set_stringn(0, tabs_top - 1, "─".repeat(side.width as usize), side.width as usize, bg.fg(Color::Indexed(240)));
    for (i, t) in st.tabs.iter().enumerate().take(shown) {
        let y = tabs_top + i as u16;
        let (g, gc, label) = state_glyph(t.state);
        let base = if i == st.active { bg.bg(Color::Indexed(24)).add_modifier(Modifier::BOLD) } else { bg };
        let base = cursor(CUR_TABS + i, base);
        buf.set_style(Rect::new(0, y, side.width, 1), base);
        let (icon, ic) = match t.agent.as_deref().or_else(|| fg_agent(t.master.process_group_leader())) {
            Some("claude") => ("✻", Color::Rgb(217, 119, 87)),
            Some("codex") => ("❖", Color::Rgb(16, 163, 127)),
            Some("copilot") => ("◈", Color::Rgb(137, 87, 229)),
            _ => (" ", Color::Reset),
        };
        buf.set_stringn(1, y, icon, 1, base.fg(ic));
        buf.set_stringn(2, y, g, 1, base.fg(gc));
        // 完了してまだ誰も見ていないタブは「未読」と ● で目立たせる
        let unread = t.unread && t.state == Some("done");
        let (label, base) = if unread { ("未読", base.fg(Color::LightGreen).add_modifier(Modifier::BOLD)) } else { (label, base) };
        if unread {
            buf.set_stringn(3, y, "●", 1, base);
        }
        let title = t.name.as_ref().unwrap_or(&t.title);
        // ラベルは全角最大 4 文字 (入力待ち)。幅 8 セルに詰めてタイトルの開始位置を揃える
        let pad = " ".repeat(2 * 4usize.saturating_sub(label.chars().count()));
        let name = if label.is_empty() { title.clone() } else { format!("{label}{pad} {title}") };
        buf.set_stringn(LABEL_X, y, name, side.width.saturating_sub(LABEL_X + 1) as usize, base);
        // F12 で選んでいるタブは右端に → (→ でメニュー)
        if st.prefix && st.cursor == CUR_TABS + i && side.width > LABEL_X + 4 {
            buf.set_string(side.width - 3, y, " →", base);
        }
    }
    draw_usage(buf, side, st, now_secs());
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
        buf.set_stringn(0, h.saturating_sub(2), "  F12 → Config", side.width as usize, warn);
    }

    // Config / Exit / + を選んでいるときと Config を開いているときは、右側を端末の代わりにその説明かメニューにする
    let panel = st.menu.is_some() || st.session.as_ref().is_some_and(|p| p.tab.is_none()) || (st.prefix && (st.cursor < CUR_TABS || st.cursor == plus));
    match st.tabs.get(st.active) {
        _ if panel => f.render_widget(ratatui::widgets::Clear, main),
        Some(t) => {
            if let Some(cur) = draw_term(buf, main, &t.term) {
                f.set_cursor_position(cur);
            }
        }
        None => {
            buf.set_string(main.x + 1, 0, "タブがない。サイドバーの + で新規", Style::default());
        }
    }
    // F12 操作中と分かるよう画面全体を黄色い枠で囲む。端末の大きさは変えず端の 1 セルに重ねる (下端はフッター)
    let f12 = st.prefix || st.menu.is_some() || st.session.is_some();
    if f12 {
        let yellow = Style::default().fg(Color::Yellow);
        let title = ratatui::text::Span::styled(" F12 操作中 ", Style::default().bg(Color::Yellow).fg(Color::Black).add_modifier(Modifier::BOLD));
        f.render_widget(ratatui::widgets::Block::bordered().border_style(yellow).title(title), area);
    }
    // サイドバーやメニューの操作中は、日本語入力の変換候補がコンソールに出ないよう、カーソルを選んでいる項目へ置く
    if let Some(m) = &st.menu {
        let pos = draw_menu(f, main, m, st);
        f.set_cursor_position(pos);
    } else if panel && st.prefix {
        use ratatui::text::Line;
        match st.cursor {
            CUR_CONFIG => {
                draw_menu(f, main, &Menu { sel: MENU_LEN, notice: Some("Enter / → で開く".into()), update: None }, st);
            }
            CUR_EXIT => {
                draw_box(f, main, " Exit ", vec![
                Line::from("MornDeck の画面を閉じる。"),
                Line::from("タブとサーバーは動いたまま残り、mdeck でまた開ける。"),
                Line::from(""),
                Line::styled("Enter で閉じる", Style::default().fg(Color::LightGreen)),
                ]);
            }
            _ => {
                draw_box(f, main, " 新しいタブ ", vec![
                Line::from("シェルのタブをホームで開く。"),
                Line::from("claude / codex / copilot はその中で起動する。"),
                Line::from(""),
                Line::styled("Enter で開く", Style::default().fg(Color::LightGreen)),
                ]);
            }
        }
    }
    if let Some(p) = &st.session {
        let pos = draw_session(f, main, p, st);
        f.set_cursor_position(pos);
    }
    if st.prefix {
        let y = match st.cursor {
            CUR_CONFIG => row_config,
            CUR_EXIT => row_exit,
            c => tabs_top + (c - CUR_TABS).min(shown) as u16,
        };
        f.set_cursor_position((1, y));
    }
    if f12 {
        let text = if st.prefix { " ↑↓/j/k 選択  Enter 決定  → 詳細  F12 閉じる" } else { " ↑↓/j/k 選択  Enter 決定  ← 戻る  F12 閉じる" };
        let bar = Rect::new(0, area.height.saturating_sub(1), area.width, 1);
        // 下の端末の文字が残らないよう、行を消してから書く
        f.render_widget(ratatui::widgets::Clear, bar);
        let on = Style::default().bg(Color::Yellow).fg(Color::Black);
        f.buffer_mut().set_style(bar, on);
        f.buffer_mut().set_stringn(0, bar.y, text, bar.width as usize, on);
    }
}

/// サイドバー下部に claude / codex の週次と copilot の月次の使用量。名前と枠の経過に対するずれ (使いすぎが +)、下の行にゲージ。
/// ゲージは塗りが使用量、│ が枠の中の今の位置
fn draw_usage(buf: &mut ratatui::buffer::Buffer, side: Rect, st: &State, now: u64) {
    use crate::usage::{ahead, fmt_ahead, fresh, gauge};
    let bg = Style::default().bg(Color::Indexed(235));
    let width = side.width.saturating_sub(2) as usize;
    for (i, (name, week)) in ["Claude", "Codex", "Copilot"].iter().zip(st.usage).enumerate() {
        let y = side.height.saturating_sub(FOOTER) + 2 * i as u16;
        let Some(w) = week.map(|w| fresh(w, now)) else {
            buf.set_stringn(1, y, format!("{name} —"), width, bg.fg(Color::Indexed(245)));
            continue;
        };
        let a = ahead(w, now);
        let color = match a {
            ..=0 => Color::LightGreen,
            1..86400 => Color::Yellow,
            _ => Color::LightRed,
        };
        buf.set_stringn(1, y, format!("{name} {}", fmt_ahead(a)), width, bg.fg(color));
        let (at, used) = gauge(w, now, width);
        for x in 0..width {
            let cell = if x < used { bg.bg(color) } else { bg.bg(Color::Indexed(238)) };
            let (sym, style) = if x == at { ("│", cell.fg(Color::White).add_modifier(Modifier::BOLD)) } else { (" ", cell) };
            buf.set_string(1 + x as u16, y + 1, sym, style);
        }
    }
}

/// 枠付きのポップアップを area の中央に出す (枠 + 左右 2 / 上下 1 の余白)。
/// 幅は一番長い行に合わせ (最小 60、最大は画面幅)。画面に入らない行は呼ぶ側が fit で詰めておく
fn draw_box(f: &mut Frame, area: Rect, title: &str, lines: Vec<ratatui::text::Line>) -> Rect {
    use ratatui::widgets::{Block, Clear, Padding, Paragraph};
    let need = lines.iter().map(|l| l.width() as u16 + BOX_PAD).max().unwrap_or(0);
    // 周りに黒い余白 (左右 2・上下 1) を残す
    let w = need.max(90).min(area.width.saturating_sub(4));
    let h = (lines.len() as u16 + 2 + 2 * PAD_Y).min(area.height.saturating_sub(2));
    let r = Rect::new(area.x + (area.width - w) / 2, area.y + (area.height - h) / 2, w, h);
    let margin = Rect::new(r.x.saturating_sub(2).max(area.x), r.y.saturating_sub(1).max(area.y), w + 4, h + 2).intersection(area);
    f.render_widget(Clear, margin);
    f.render_widget(Paragraph::new(lines).block(Block::bordered().title(title).padding(Padding::new(PAD_X, PAD_X, PAD_Y, PAD_Y))), r);
    r
}

/// 選んでいる項目の色 (サイドバーとポップアップで同じ)
fn cursor_style() -> Style {
    Style::default().bg(Color::Rgb(255, 150, 40)).fg(Color::Black)
}

/// ポップアップの中の余白 (左右・上下)
const PAD_X: u16 = 4;
const PAD_Y: u16 = 2;
/// 枠 2 + 左右の余白
const BOX_PAD: u16 = 2 + 2 * PAD_X;

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

fn session_items(st: &State, p: &SessionPop) -> Vec<(SessionItem, String)> {
    if p.confirm_close {
        return vec![(SessionItem::KeepOpen, "削除しない".into()), (SessionItem::ConfirmClose, "削除する".into())];
    }
    if p.confirm_hook.is_some() {
        return vec![(SessionItem::KeepHook, "削除しない".into()), (SessionItem::ConfirmHook, "削除する".into())];
    }
    let Some(tab) = p.tab else {
        // Config の通知先: 名前と URL の行 (1 つの名前に URL が複数あれば行も複数) と追加
        let mut v = Vec::new();
        for (name, list) in &st.owner_webhooks {
            for h in list.as_array().into_iter().flatten() {
                let url = h["url"].as_str().unwrap_or("").to_string();
                v.push((SessionItem::Hook(name.clone(), url.clone()), format!("{name}  {url}")));
            }
        }
        v.push((SessionItem::AddHook, "+ 通知先を追加".into()));
        return v;
    };
    let Some(t) = st.tabs.iter().find(|t| t.id == tab) else { return Vec::new() };
    vec![
        (SessionItem::Name, kv("タブ名", t.name.as_deref().unwrap_or("(なし)"))),
        (SessionItem::Dest, kv("通知先", &format!("← {} →", t.owner.as_deref().unwrap_or("なし")))),
        (SessionItem::AutoYes, kv("AutoYes", if t.auto_yes { "オン" } else { "オフ" })),
        (SessionItem::Close, "タブを削除".into()),
    ]
}

/// タブのメニューの「項目名  値」。値の始まりを揃える (項目名は最長の AutoYes に合わせる)
fn kv(label: &str, value: &str) -> String {
    let w = ratatui::text::Line::from(label).width();
    format!("{label}{}{value}", " ".repeat(9 - w.min(7)))
}

/// タブの通知先の候補。なし・登録済みの通知先・今の通知先 (登録が消えていても選べるように)
fn dest_choices(st: &State, current: Option<&str>) -> Vec<Option<String>> {
    let mut v: Vec<Option<String>> = std::iter::once(None).chain(st.owner_webhooks.keys().cloned().map(Some)).collect();
    if let Some(c) = current.filter(|c| !st.owner_webhooks.contains_key(*c)) {
        v.push(Some(c.to_string()));
    }
    v
}

/// 戻り値は選んでいる項目の位置。入力中なら入力の末尾。日本語入力の変換候補はカーソルの位置に出るので、そこへ置く
fn draw_session(f: &mut Frame, area: Rect, p: &SessionPop, st: &State) -> (u16, u16) {
    use ratatui::text::Line;
    let items = session_items(st, p);
    let tab = p.tab.and_then(|id| st.tabs.iter().find(|t| t.id == id));
    let title = tab.map(|t| t.name.clone().unwrap_or_else(|| t.title.clone())).unwrap_or_default();
    // 見出しは確認と通知先の一覧だけ 2 行。タブのメニューは項目だけ
    let red = Style::default().fg(Color::LightRed).add_modifier(Modifier::BOLD);
    let max = box_inner(area);
    let mut lines = if p.confirm_close {
        vec![Line::styled(fit(&format!("{title} を削除する？"), max, false), red), Line::from("中で動いている claude / codex / copilot も終了する")]
    } else if let Some((name, url)) = &p.confirm_hook {
        vec![Line::styled(fit(&format!("通知先 {name} のこの URL を削除する？"), max, false), red), Line::from(fit(url, max, false))]
    } else if p.tab.is_some() {
        Vec::new()
    } else {
        vec![Line::from("完了と選択肢を POST する先。タブのメニューで選ぶ"), Line::from("")]
    };
    let mut cursor_x = 0;
    let head = lines.len() as u16;
    // 「タブを削除」の前は 1 行空ける (選んでいる行の位置もその分ずらす)
    let mut gap = 0;
    for (i, (item, label)) in items.iter().enumerate() {
        if *item == SessionItem::Close {
            lines.push(Line::from(""));
            if p.sel >= i {
                gap = 1;
            }
        }
        let style = if i == p.sel { cursor_style() } else { Style::default() };
        // 編集中の webhook の行も「編集」「削除」の分の幅を取り、箱の幅を変えない
        let hook_w = matches!(item, SessionItem::Hook(..)).then(|| Line::from(format!(" {}", fit(label, max.saturating_sub(16), false))).width() + 15);
        let caret = |b: &str| {
            let at = b.chars().count().saturating_sub(p.edit_back);
            format!("{}▏{}", b.chars().take(at).collect::<String>(), b.chars().skip(at).collect::<String>())
        };
        let label = match (&p.edit, item) {
            (Some((Field::Name, b)), SessionItem::Name) => kv("タブ名", &caret(b)),
            (Some((Field::DestName, b)), SessionItem::AddHook) => format!("名前: {}", caret(b)),
            (Some((Field::Webhook, b)), SessionItem::AddHook) if p.edit_hook.is_none() => {
                format!("{}  URL: {}", p.new_dest.as_deref().unwrap_or(""), caret(b))
            }
            (Some((Field::Webhook, b)), SessionItem::Hook(n, u)) if p.edit_hook.as_ref() == Some(&(n.clone(), u.clone())) => {
                format!("{n}  URL: {}", caret(b))
            }
            _ => label.clone(),
        };
        // 前後の空白 2 つ分。選んでいる webhook の行は後ろの「編集」「削除」の分も空ける。入力中は末尾 (入力位置) を見せる
        let editing = p.edit.is_some() && i == p.sel;
        // webhook の行は選ばなくても後ろの「編集」「削除」の分の幅を取り、選んだときに箱の幅が変わらないようにする
        let wide = matches!(item, SessionItem::Hook(..)) && !editing;
        let hook_row = wide && i == p.sel && p.edit.is_none();
        let label = fit(&label, max.saturating_sub(if wide { 16 } else { 2 }), editing);
        if i == p.sel {
            cursor_x = if p.edit.is_some() { Line::from(format!(" {}", label.split('▏').next().unwrap_or(""))).width() as u16 } else { 1 };
        }
        // 選んでいる webhook の行は、後ろの「編集」「削除」のどちらかを反転する
        if hook_row {
            use ratatui::text::Span;
            let (e, d) = if p.hook_del { (Style::default(), Style::default().bg(Color::LightRed).fg(Color::Black)) } else { (cursor_style(), Style::default()) };
            lines.push(Line::from(vec![Span::raw(format!(" {label}  ")), Span::styled(" 編集 ", e), Span::raw(" "), Span::styled(" 削除 ", d)]));
            continue;
        }
        let style = match (matches!(item, SessionItem::ConfirmClose | SessionItem::ConfirmHook), i == p.sel) {
            (true, true) => Style::default().bg(Color::LightRed).fg(Color::Black),
            (true, false) => style.fg(Color::LightRed),
            _ => style,
        };
        let w = Line::from(format!(" {label}")).width();
        let pad = " ".repeat(if wide { 15 } else { hook_w.map_or(1, |h| h.saturating_sub(w).max(1)) });
        lines.push(Line::styled(format!(" {label}{pad}"), style));
    }
    let hint = match (&p.edit, items.get(p.sel).map(|x| &x.0)) {
        (Some((Field::Webhook, _)), _) => "Enter 決定  F12 やめる",
        (Some((Field::DestName, _)), _) => "Enter 次へ (URL)  F12 やめる",
        (Some(_), _) => "Enter 決定  F12 やめる  空で外す",
        (None, Some(SessionItem::Hook(..))) => "←/→ 編集・削除  Enter 実行  ↑↓/j/k 選択",
        (None, Some(SessionItem::Dest)) => "←/→ 通知先を選ぶ  ↑↓/j/k 選択",
        (None, _) if p.confirm_close || p.confirm_hook.is_some() => "「削除する」を選んで Enter  ← やめる",
        _ => "↑↓/j/k 選択  Enter 実行  ← 戻る",
    };
    lines.push(Line::from(""));
    lines.push(Line::styled(fit(p.notice.as_deref().unwrap_or(hint), max, false), Style::default().fg(Color::LightGreen)));
    let r = draw_box(f, area, if p.tab.is_some() { " タブ " } else { " 通知先 " }, lines);
    // 枠 + 余白 + 見出し 2 行の下が項目
    (r.x + 1 + PAD_X + cursor_x, r.y + 1 + PAD_Y + head + gap + p.sel as u16)
}

impl SessionPop {
    fn new(tab: Option<u32>) -> Self {
        SessionPop { tab, sel: 0, edit: None, notice: None, confirm_close: false, hook_del: false, confirm_hook: None, edit_hook: None, new_dest: None, edit_back: 0 }
    }
}

fn open_session(st: &mut State, idx: usize) {
    let Some(t) = st.tabs.get(idx) else { return };
    st.session = Some(SessionPop::new(Some(t.id)));
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
    // タブのメニューなら、そのタブの位置。通知先の一覧なら None
    let idx = match p.tab {
        Some(id) => match st.tabs.iter().position(|t| t.id == id) {
            Some(i) => Some(i),
            None => {
                mark(s, &mut st);
                return 0; // タブが消えたら閉じて、入力はそのまま流す
            }
        },
        None => None,
    };
    let items = session_items(&st, &p);
    let mut used = if rest[0] == 0x1b { esc_len(rest) } else { 1 };
    let mut open = true;
    let (left, right) = (starts(b"\x1b[D", b"\x1bOD"), starts(b"\x1b[C", b"\x1bOC"));
    if let Some((field, buf)) = p.edit.as_mut() {
        let len = buf.chars().count();
        p.edit_back = p.edit_back.min(len);
        // 入力位置 (バイト)
        let at = buf.char_indices().nth(len - p.edit_back).map_or(buf.len(), |(i, _)| i);
        match rest[0] {
            _ if left => p.edit_back = (p.edit_back + 1).min(len),
            _ if right => p.edit_back = p.edit_back.saturating_sub(1),
            b'\r' => {
                let (field, value) = (*field, buf.trim().to_string());
                let res = match (field, idx) {
                    (Field::Name, Some(idx)) => {
                        let r = if value.is_empty() { Ok(()) } else { st.check_name(&value, p.tab) };
                        r.map(|_| st.tabs[idx].name = (!value.is_empty()).then_some(value))
                    }
                    (Field::Name, None) => Ok(()),
                    // 名前を決めたら続けて URL を入力する
                    (Field::DestName, _) => match validate_name(&value, std::iter::empty()) {
                        Ok(()) => {
                            p.new_dest = Some(value);
                            p.edit = Some((Field::Webhook, "https://".into()));
                            p.edit_back = 0;
                            p.notice = None;
                            st.session = Some(p);
                            mark(s, &mut st);
                            return used;
                        }
                        Err(e) => Err(e),
                    },
                    // 編集は同じ位置で URL だけ差し替え、ヘッダーは残す
                    (Field::Webhook, _) => {
                        let req = match (&p.edit_hook, &p.new_dest) {
                            (Some((_, old)), _) if *old == value => json!(null),
                            (Some((name, old)), _) => {
                                let mut list = st.owner_webhooks.get(name).and_then(Value::as_array).cloned().unwrap_or_default();
                                list.iter_mut().filter(|h| h["url"] == old.as_str()).for_each(|h| h["url"] = json!(value));
                                json!({"cmd": "webhooks", "owner": name, "set": list})
                            }
                            (None, Some(name)) => json!({"cmd": "webhooks", "owner": name, "add": value}),
                            (None, None) => json!(null),
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
                        p.new_dest = None;
                        p.edit_back = 0;
                        p.notice = Some("変更した".into());
                    }
                    Err(e) => p.notice = Some(e),
                }
            }
            // Esc や他のエスケープ列 (↑↓ など) は何もしない。やめるのは F12
            0x1b => {}
            0x7f | 0x08 => {
                if let Some((i, _)) = buf[..at].char_indices().next_back() {
                    buf.remove(i);
                }
            }
            c if c < 0x20 => {}
            _ => {
                let n = rest.iter().position(|c| *c < 0x20 || *c == 0x7f).unwrap_or(rest.len());
                buf.insert_str(at, &String::from_utf8_lossy(&rest[..n]));
                p.notice = None;
                used = n;
            }
        }
    } else {
        p.notice = None;
        let last = items.len().saturating_sub(1);
        let item = items.get(p.sel).map(|x| x.0.clone());
        let on_hook = matches!(item, Some(SessionItem::Hook(..)));
        let back = left || rest[0] == b'q';
        if up || down || matches!(rest[0], b'j' | b'k') {
            p.hook_del = false;
        }
        match rest[0] {
            _ if on_hook && (left || right) => p.hook_del = right,
            // 通知先は ←/→ (と Enter) で候補を順に選ぶ
            _ if item == Some(SessionItem::Dest) && (left || right || rest[0] == b'\r') => {
                if let Some(idx) = idx {
                    let cur = st.tabs[idx].owner.clone();
                    let choices = dest_choices(&st, cur.as_deref());
                    let at = choices.iter().position(|c| *c == cur).unwrap_or(0) as i64;
                    let next = (at + if left { -1 } else { 1 }).rem_euclid(choices.len() as i64) as usize;
                    st.tabs[idx].owner = choices[next].clone();
                }
            }
            _ if up => p.sel = p.sel.saturating_sub(1),
            _ if down => p.sel = (p.sel + 1).min(last),
            b'k' => p.sel = p.sel.saturating_sub(1),
            b'j' => p.sel = (p.sel + 1).min(last),
            b'\r' => match (item, idx) {
                (Some(SessionItem::Name), Some(idx)) => p.edit = Some((Field::Name, st.tabs[idx].name.clone().unwrap_or_default())),
                (Some(SessionItem::AddHook), _) => p.edit = Some((Field::DestName, String::new())),
                // 削除はいきなりせず確認を挟む (既定は「削除しない」)
                (Some(SessionItem::Hook(name, url)), _) if p.hook_del => {
                    p.confirm_hook = Some((name, url));
                    p.sel = 0;
                }
                (Some(SessionItem::Hook(name, url)), _) => {
                    p.edit = Some((Field::Webhook, url.clone()));
                    p.edit_hook = Some((name, url));
                }
                (Some(SessionItem::KeepHook), _) => keep_hook(&st, &mut p),
                (Some(SessionItem::ConfirmHook), _) => {
                    let (name, url) = p.confirm_hook.take().unwrap_or_default();
                    drop(st);
                    let r = api(s, &json!({"cmd": "webhooks", "owner": name, "remove": url}));
                    st = lock(s);
                    p.hook_del = false;
                    p.sel = 0;
                    p.notice = Some(match r {
                        Ok(_) => "削除した".into(),
                        Err(e) => e,
                    });
                }
                (Some(SessionItem::AutoYes), Some(idx)) => {
                    st.tabs[idx].auto_yes = !st.tabs[idx].auto_yes;
                }
                (Some(SessionItem::ConfirmClose), Some(idx)) => {
                    let _ = st.tabs[idx].child.kill();
                    open = false;
                }
                // 既定は「閉じない」。Enter を続けて押しても閉じない
                (Some(SessionItem::Close), _) => {
                    p.confirm_close = true;
                    p.sel = 0;
                }
                (Some(SessionItem::KeepOpen), _) => {
                    p.confirm_close = false;
                    p.sel = session_items(&st, &p).iter().position(|x| x.0 == SessionItem::Close).unwrap_or(0);
                }
                _ => {}
            },
            // → は通知先の行でだけ使う
            _ if right => {}
            // 確認中の ← は閉じるのをやめるだけ
            _ if back && p.confirm_close => {
                p.confirm_close = false;
                p.sel = session_items(&st, &p).iter().position(|x| x.0 == SessionItem::Close).unwrap_or(0);
            }
            _ if back && p.confirm_hook.is_some() => keep_hook(&st, &mut p),
            // ← はタブのメニューならサイドバーへ、通知先の一覧なら Config へ戻る (Esc は何もしない)
            _ if back => {
                open = false;
                match idx {
                    Some(idx) => {
                        st.prefix = true;
                        st.cursor = CUR_TABS + idx;
                    }
                    None => st.menu = Some(Menu { sel: M_DEST, notice: None, update: None }),
                }
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
    let hook = p.confirm_hook.take().map(|(n, u)| SessionItem::Hook(n, u));
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
    // フックの行の上に見出し (選べない行)
    let mut lines = vec![Line::from(" フック")];
    for (i, item) in menu_items(m, st).iter().enumerate() {
        let color = match st.hooks.get(i).map(|h| h.1) {
            Some(hooks::Status::Current) => Color::LightGreen,
            Some(hooks::Status::NoAgent) => Color::DarkGray,
            Some(_) => Color::Yellow,
            None => Color::Reset,
        };
        let style = if i == m.sel { cursor_style() } else { Style::default().fg(color) };
        lines.push(Line::styled(format!(" {} ", fit(item, box_inner(area).saturating_sub(2), false)), style));
    }
    let hint = match st.hooks.get(m.sel) {
        Some(h) => hook_verb(h.1),
        None if m.sel == M_SIDEBAR => "←/→ 幅  ↑↓/j/k 選択  F12 閉じる",
        None => "↑↓/j/k 選択  Enter 実行  ← 戻る  F12 閉じる",
    };
    lines.push(Line::from(""));
    lines.push(Line::styled(fit(m.notice.as_deref().unwrap_or(hint), box_inner(area), false), Style::default().fg(Color::LightGreen)));
    let r = draw_box(f, area, " Config ", lines);
    (r.x + 1 + PAD_X + 1, r.y + 1 + PAD_Y + 1 + m.sel as u16)
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
        // ← は幅の項目以外ではサイドバーへ戻る
        _ if left && sel != M_SIDEBAR => {
            open = false;
            st.prefix = true;
            st.cursor = CUR_CONFIG;
        }
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
                        Ok(Some(v)) => format!("v{v} がある。もう一度 Enter で入れ替える"),
                        Ok(None) => format!("最新 (v{})", env!("CARGO_PKG_VERSION")),
                        Err(e) => format!("確認失敗: {e}"),
                    });
                    m.update = r.ok().flatten();
                    mark(&s2, &mut st);
                }
            });
        }
        b'\r' if sel == M_SIDEBAR => {}
        b'\r' if sel == M_DEST => {
            open = false;
            st.session = Some(SessionPop::new(None));
        }
        b'\r' if (M_SOUND..=M_QUIET).contains(&sel) => {
            let a = &mut st.alerts;
            let flag = match sel {
                M_SOUND => &mut a.sound,
                M_NOTIFY => &mut a.notify,
                _ => &mut a.quiet_hooked,
            };
            *flag = !*flag;
            let (sock, a) = (st.sock.clone(), st.alerts);
            save_alerts(&sock, a);
        }
        b'\r' => open = false,
        b'q' => {
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
        AGENTS.into_iter().find(|a| path.contains(a))
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
        // 列幅は alacritty の決めたものを使わせる。✔ + VS16 などを ratatui は 2 列と数え、ずれると差分の計算が画面の外を読んで落ちる
        let width = if cell.flags.contains(Flags::WIDE_CHAR) { 2 } else { 1 };
        out.set_diff_option(CellDiffOption::ForcedWidth(std::num::NonZeroU16::new(width).unwrap()));
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
            // rename 以外は回答や画面を読んだので既読
            if cmd != "rename" && std::mem::take(&mut st.tabs[i].unread) {
                mark(s, &mut st);
            }
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
            let args = agent_args(agent, req["yolo"] == true, req["args"].as_str());
            let run = match agent {
                Some(a) if AGENTS.contains(&a) => Some(format!("{a} {}", args.as_deref().unwrap_or("")).trim_end().to_string()),
                Some(a) => return Err(format!("agent は {}: {a}", AGENTS.join(" / "))),
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
                t.auto_yes = req["auto_yes"] == true;
                if agent.is_some() {
                    t.args = args;
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
        // 後から owner を付け替える。空文字か省略で外す。webhook を渡すと新しい owner の一覧にも入れる
        "owner" => {
            let owner = req["owner"].as_str().filter(|o| !o.is_empty()).map(str::to_string);
            if let Some(o) = &owner {
                validate_name(o, std::iter::empty())?;
            }
            let id = {
                let st = lock(s);
                st.tabs[st.find(&req["tab"])?].id
            };
            if !req["webhook"].is_null() {
                ensure_owner_webhook(s, owner.as_deref(), &req["webhook"])?;
            }
            let mut st = lock(s);
            let i = st.find(&json!(id))?;
            st.tabs[i].owner = owner;
            mark(s, &mut st);
            Ok(json!({"ok": true, "tab": st.tab_json(i)}))
        }
        // 登録した webhook に今すぐテストの通知を送り、届いたか (HTTP ステータス) を返す
        "webhook_test" => {
            let owner = req["owner"].as_str();
            let (sock, list) = {
                let st = lock(s);
                let list = match owner {
                    Some(o) => st.owner_webhooks.get(o).and_then(Value::as_array).cloned().unwrap_or_default(),
                    None => st.webhooks.clone(),
                };
                (st.sock.clone(), list)
            };
            if list.is_empty() {
                return Err(format!("webhook が登録されていない (owner: {})", owner.unwrap_or("なし = 全体")));
            }
            let body = json!({"event": "test", "owner": owner, "message": "MornDeck からのテスト通知"});
            let results: Vec<Value> = std::thread::scope(|sc| {
                let hs: Vec<_> = list.iter().map(|h| sc.spawn(|| post_webhook(&sock, h, &body))).collect();
                hs.into_iter().map(|h| h.join().unwrap_or(Value::Null)).collect()
            });
            let ok = results.iter().all(|r| (200..300).contains(&r["http"].as_u64().unwrap_or(0)));
            Ok(json!({"ok": ok, "owner": owner, "results": results}))
        }
        "auto_yes" => {
            let mut st = lock(s);
            let i = st.find(&req["tab"])?;
            st.tabs[i].auto_yes = req["on"] == true;
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
                press_choice(t, &options, target)?;
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
        // exe を渡すとその実行ファイルで起動し直す (手元のビルドで確かめるとき。戻すときは brew の mdeck を渡す)
        "restart" => {
            let exe = req["exe"].as_str().map(std::path::PathBuf::from);
            if let Some(e) = exe.as_ref().filter(|e| !e.is_file()) {
                return Err(format!("exe が無い: {}", e.display()));
            }
            let (s, except) = (s.clone(), req["except"].as_u64());
            // 先に呼んだ側へ返事を返す
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(200));
                restart(&s, except, exe);
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
                // copilot は camelCase でイベント名が無いので、フックのコマンドが event を付けて送る
                let or = |a: &str, b: &str| if p[a].is_null() { p[b].clone() } else { p[a].clone() };
                let event = if p["hook_event_name"].is_null() { req["event"].clone() } else { p["hook_event_name"].clone() };
                json!({"tab": req["tab"], "agent": req["agent"], "event": event, "message": p["last_assistant_message"],
                       "tool": or("tool_name", "toolName"), "input": or("tool_input", "toolArgs"), "session": or("session_id", "sessionId")})
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
            // 自動 Yes のタブは waiting にせず、確認が描かれたら auto_yes_loop が答える
            if state == Some("waiting") && wants_yes(t) {
                t.auto_wait = Some(std::time::Instant::now());
                set_state(s, &mut st, id, Some("working"));
                return Ok(json!({"ok": true}));
            }
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
    // done は回答ごとに seq が進むので、done が続いても (UserPromptSubmit の無いターンなど) 毎回送る
    if !changed && state != Some("done") {
        return;
    }
    st.notify(ev);
    if state == Some("done") && let Some(i) = st.tabs.iter().position(|t| t.id == id) {
        // 画面で表示中のタブは見たことにする
        let seen = i == st.active && !st.clients.is_empty();
        st.tabs[i].unread = !seen;
        if !seen {
            alert(st, i);
        }
    }
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

/// 自由な権限のフラグは args に入れ、復元 (resume) でも付くようにする
fn agent_args(agent: Option<&str>, yolo: bool, args: Option<&str>) -> Option<String> {
    match YOLO.iter().find(|(a, _)| Some(*a) == agent).filter(|_| yolo) {
        Some((_, flag)) => Some(format!("{flag} {}", args.unwrap_or("")).trim_end().to_string()),
        None => args.map(str::to_string),
    }
}

/// 選択肢 target を選ぶキーを送る。番号の無い選択肢は矢印で動かして Enter
fn press_choice(t: &mut Tab, options: &[Value], target: usize) -> Result<(), String> {
    let keys = if options[target]["numbered"] == true {
        options[target]["n"].to_string()
    } else {
        let cur = options.iter().position(|o| o["selected"] == true).unwrap_or(0) as i64;
        let app = t.term.mode().contains(TermMode::APP_CURSOR);
        let (up, down) = if app { ("\x1bOA", "\x1bOB") } else { ("\x1b[A", "\x1b[B") };
        let d = target as i64 - cur;
        format!("{}\r", if d < 0 { up } else { down }.repeat(d.unsigned_abs() as usize))
    };
    t.writer.write_all(keys.as_bytes()).map_err(|e| e.to_string())
}

/// 全部が Yes / No で始まる選択肢 (実行の確認) なら、最初の Yes (今回だけ許可) の位置。質問や他の選択肢は None
fn yes_choice(options: &[Value]) -> Option<usize> {
    let label = |o: &Value| o["label"].as_str().unwrap_or("").trim().to_lowercase();
    let yes = |o: &Value| label(o).starts_with("yes");
    let no = |o: &Value| label(o).starts_with("no");
    if !options.iter().any(no) || !options.iter().all(|o| yes(o) || no(o)) {
        return None;
    }
    options.iter().position(yes)
}

/// 自動 Yes の対象か。質問 (AskUserQuestion) と計画の承認は自動で答えない
fn wants_yes(t: &Tab) -> bool {
    t.auto_yes && !matches!(t.question["tool"].as_str(), Some("AskUserQuestion" | "ExitPlanMode"))
}

/// 自動 Yes のタブを見張り、実行の確認 (選択肢が全部 Yes / No で始まり、カーソルの付いたメニュー) が描かれたら最初の Yes を選ぶ。
/// フックの直後は確認がまだ描かれていないことがあり、フックの来ない確認もあるので、画面を見て答える。
/// 完了や待機中のタブも見る (メインの会話が終わった後に、背景のサブエージェントが確認を出すことがある)。
/// 回答の文中の番号付きの行にはカーソル (❯) が付かないので、カーソル付きのメニューだけに答える
fn auto_yes_loop(s: Shared) {
    loop {
        std::thread::sleep(Duration::from_millis(300));
        let mut st = lock(&s);
        let now = std::time::Instant::now();
        let mut give_up = Vec::new();
        for i in 0..st.tabs.len() {
            let t = &mut st.tabs[i];
            if !wants_yes(t) || t.agent.is_none() {
                continue;
            }
            let lines = screen_lines(&t.term);
            if t.auto_pressed.as_ref() == Some(&lines) {
                continue;
            }
            let options = parse_options(&lines);
            let menu = options.iter().any(|o| o["selected"] == true);
            match yes_choice(&options).filter(|_| menu) {
                Some(k) if press_choice(t, &options, k).is_ok() => {
                    t.auto_pressed = Some(lines);
                    t.auto_wait = None;
                    let ev = json!({"event": "auto_yes", "tab": t.id, "label": options[k]["label"]});
                    st.emit(ev);
                }
                // 確認を受けたのに Yes / No の確認が出てこない (他の選択肢など) なら、ふつうに入力待ちにする
                _ if t.auto_wait.is_some_and(|w| now - w > Duration::from_secs(3)) => {
                    t.auto_wait = None;
                    give_up.push(t.id);
                }
                _ => {}
            }
        }
        for id in give_up {
            set_state(&s, &mut st, id, Some("waiting"));
        }
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
                "timeout: 起動を確認できない。フックが未導入か古い (F12 → Config)"
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
                   "owner": t.owner, "last_used": t.last_used, "auto_yes": t.auto_yes, "unread": t.unread})
        })
        .collect();
    json!({"active": st.active, "sidebar": st.sidebar, "tabs": tabs})
}

/// 週次の使用量を読み直す。codex は手元の記録なので毎分、claude は API なので 5 分おき
fn usage_loop(s: Shared) {
    let home = std::path::PathBuf::from(std::env::var("HOME").unwrap_or_default());
    let (mut claude, mut copilot) = (None, None);
    for i in 0u64.. {
        if i % 5 == 0 {
            claude = crate::usage::claude().or(claude);
            // copilot を入れていなければ読まない (キーチェーンの許可ダイアログを出さない)
            if home.join(".copilot").is_dir() {
                copilot = crate::usage::copilot(&home).or(copilot);
            }
        }
        let codex = crate::usage::codex(&home);
        {
            let mut st = lock(&s);
            st.usage = [claude, codex, copilot];
            mark(&s, &mut st);
        }
        std::thread::sleep(Duration::from_secs(60));
    }
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
fn restart(s: &Shared, except: Option<u64>, exe: Option<std::path::PathBuf>) {
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
    let e = std::process::Command::new(exe.unwrap_or_else(server_exe)).arg("server").exec();
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
        "copilot" => format!("copilot --resume {sid}"),
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
                tab.auto_yes = t["auto_yes"].as_bool().unwrap_or(false);
                tab.unread = t["unread"].as_bool().unwrap_or(false);
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

/// 届いたかを後から追えるよう webhook.log に結果を残す。URL の末尾 (Bot ごとの ID) とヘッダーは残さない
fn post_webhooks(sock: &str, hooks: &[Value], body: &Value) {
    for hook in hooks {
        let (sock, hook, body) = (sock.to_string(), hook.clone(), body.clone());
        std::thread::spawn(move || post_webhook(&sock, &hook, &body));
    }
}

/// 1 件 POST して終わるまで待つ。結果 {url (末尾は伏せる), http, error} を返し、webhook.log にも残す
fn post_webhook(sock: &str, hook: &Value, body: &Value) -> Value {
    let url = hook["url"].as_str().unwrap_or("");
    let shown = url.rsplit_once('/').map_or(url.to_string(), |(base, id)| format!("{base}/{}…", id.chars().take(8).collect::<String>()));
    let mut args: Vec<String> = ["-sS", "-m", "10", "-o", "/dev/null", "-w", "%{http_code}", "-X", "POST", "-H", "Content-Type: application/json"]
        .map(String::from).to_vec();
    for (k, v) in hook["headers"].as_object().into_iter().flatten() {
        args.extend(["-H".into(), format!("{k}: {}", v.as_str().unwrap_or(""))]);
    }
    args.extend(["--data-binary".into(), "@-".into(), url.into()]);
    // ponytail: ヘッダー値が数秒 ps に見える。共用マシンで使うなら curl の --config を一時ファイルで渡す
    let out = std::process::Command::new("curl")
        .args(&args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut c| {
            if let Some(mut i) = c.stdin.take() {
                let _ = i.write_all(body.to_string().as_bytes());
            }
            c.wait_with_output()
        });
    let (http, error) = match out {
        Ok(o) => {
            let err = String::from_utf8_lossy(&o.stderr).trim().to_string();
            (String::from_utf8_lossy(&o.stdout).parse::<u16>().unwrap_or(0), (!o.status.success()).then(|| format!("curl {}: {err}", o.status.code().unwrap_or(-1))))
        }
        Err(e) => (0, Some(format!("curl: {e}"))),
    };
    append_log(&std::path::Path::new(sock).with_file_name("webhook.log"), &format!(
        "tab={} owner={} event={} state={} seq={} url={shown} http={http} err={}",
        body["tab"], body["owner"], body["event"], body["state"], body["seq"], error.as_deref().unwrap_or("")));
    json!({"url": shown, "http": http, "error": error})
}

fn append_log(path: &std::path::Path, line: &str) {
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(f, "{} {line}", now_secs());
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
    /// 右下に絵文字 + VS16 (alacritty は 1 列、ratatui は 2 列と数える) を背景色付きで出し、次のフレームで普通の文字に変えても
    /// 描画が落ちない。ratatui 0.30.2 は前フレームの幅で後ろのセルを読み、画面の外を読んで panic していた
    #[test]
    fn emoji_vs16_at_bottom_right_does_not_panic() {
        use alacritty_terminal::vte::ansi::Processor;
        let (cols, rows) = (10u16, 2u16);
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut term = alacritty_terminal::Term::new(
            alacritty_terminal::term::Config::default(),
            &alacritty_terminal::term::test::TermSize::new(cols as usize, rows as usize),
            super::Listener(tx),
        );
        let mut parser: Processor = Processor::new();
        let mut t = ratatui::Terminal::new(ratatui::backend::TestBackend::new(cols, rows)).unwrap();
        let area = ratatui::layout::Rect::new(0, 0, cols, rows);
        parser.advance(&mut term, "\x1b[2;10H\x1b[44m\u{2714}\u{fe0f}".as_bytes());
        t.draw(|f| {
            super::draw_term(f.buffer_mut(), area, &term);
        })
        .unwrap();
        parser.advance(&mut term, "\x1b[2;10Ha".as_bytes());
        t.draw(|f| {
            super::draw_term(f.buffer_mut(), area, &term);
        })
        .unwrap();
    }

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
    fn mouse_reports_are_dropped_not_typed_into_the_tab() {
        let _fork = crate::FORK_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let s: super::Shared = Default::default();
        let dir = std::env::temp_dir().join(format!("mdeck-mouse-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let out = dir.join("typed.txt");
        {
            let mut st = super::lock(&s);
            st.sock = dir.join("t.sock").to_string_lossy().into_owned();
            st.cols = 100;
            st.rows = 30;
            st.sidebar = 24;
        }
        // 端末の加工 (エコー・行単位) を切って、届いたバイトをそのまま書く
        let run = format!("stty raw -echo; cat > '{}'", out.display());
        super::api(&s, &json!({"cmd": "new", "cwd": dir.to_str().unwrap(), "run": run})).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(500));
        super::Input::default().feed(&s, b"a\x1b[<0;88;23M\x1b[<0;88;23mb\x1b[M !!c");
        let got = (0..30).find_map(|_| {
            std::thread::sleep(std::time::Duration::from_millis(100));
            std::fs::read(&out).ok().filter(|b| b.ends_with(b"c"))
        });
        assert_eq!(got.as_deref(), Some(&b"abc"[..]));
        for t in super::lock(&s).tabs.iter() {
            let _ = std::process::Command::new("kill").arg(t.child.process_id().unwrap().to_string()).status();
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn size_follows_the_screen_used_last() {
        use std::io::Write;
        use std::os::unix::net::UnixStream;
        let s: super::Shared = Default::default();
        let connect = |cols: u16, rows: u16| {
            let (client, server) = UnixStream::pair().unwrap();
            let s = s.clone();
            let w = server.try_clone().unwrap();
            std::thread::spawn(move || super::attach(s, std::io::BufReader::new(server), w, &json!({"cols": cols, "rows": rows})));
            client
        };
        let size = |s: &super::Shared| {
            let st = super::lock(s);
            (st.cols, st.rows)
        };
        let wait = |want: (u16, u16)| (0..50).any(|_| {
            std::thread::sleep(std::time::Duration::from_millis(20));
            size(&s) == want
        });
        let mut half = connect(50, 20);
        assert!(wait((50, 20)));
        let _full = connect(120, 40);
        assert!(wait((120, 40)));
        // 半画面の画面で入力すると、その大きさに戻る
        let mut msg = vec![super::IN_INPUT];
        msg.extend_from_slice(&1u32.to_be_bytes());
        msg.push(b'x');
        half.write_all(&msg).unwrap();
        assert!(wait((50, 20)), "最後に操作した画面の大きさにならない: {:?}", size(&s));
    }

    #[test]
    fn wheel_goes_to_apps_that_ask_for_mouse_reports() {
        use super::TermMode as M;
        assert_eq!(super::mouse_report(M::empty(), 64, 3, 4), None);
        assert_eq!(super::mouse_report(M::MOUSE_REPORT_CLICK | M::SGR_MOUSE, 65, 3, 4).unwrap(), b"\x1b[<65;3;4M");
        assert_eq!(super::mouse_report(M::MOUSE_DRAG, 64, 1, 300).unwrap(), [0x1b, b'[', b'M', 96, 33, 255]);
    }

    #[test]
    fn base64_matches_rfc4648() {
        for (i, o) in [("", ""), ("f", "Zg=="), ("fo", "Zm8="), ("foo", "Zm9v"), ("foob", "Zm9vYg==")] {
            assert_eq!(super::base64(i.as_bytes()), o);
        }
    }

    #[test]
    fn drag_selects_only_inside_the_terminal() {
        let _fork = crate::FORK_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let s: super::Shared = Default::default();
        let dir = std::env::temp_dir().join(format!("mdeck-select-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        {
            let mut st = super::lock(&s);
            st.sock = dir.join("t.sock").to_string_lossy().into_owned();
            st.cols = 100;
            st.rows = 30;
            st.sidebar = 24;
        }
        super::api(&s, &json!({"cmd": "new", "cwd": dir.to_str().unwrap(), "run": "clear; printf 'hello world'; sleep 5"})).unwrap();
        // hello world が出た行と列 (1 始まりの画面座標にする)
        let (row, col) = (0..50)
            .find_map(|_| {
                std::thread::sleep(std::time::Duration::from_millis(100));
                let lines = super::screen_lines(&super::lock(&s).tabs[0].term);
                lines.iter().enumerate().find_map(|(y, l)| l.find("hello world").map(|x| (y + 1, l[..x].chars().count() + 25)))
            })
            .unwrap();
        let at = |btn: u8, x: usize, up: bool| format!("\x1b[<{btn};{x};{row}{}", if up { 'm' } else { 'M' }).into_bytes();
        let selected = |s: &super::Shared| super::lock(s).tabs[0].term.selection_to_string();
        let mut inp = super::Input::default();
        // サイドバーで押しても選ばない
        inp.feed(&s, &at(0, 3, false));
        inp.feed(&s, &at(32, col + 4, false));
        assert_eq!(selected(&s), None, "サイドバーから選び始めた");
        inp.feed(&s, &at(0, 3, true));
        // 端末の中を押してドラッグすると、端末の文字だけが選ばれる (離すとクリップボードへ入るので離さない)
        inp.feed(&s, &at(0, col, false));
        inp.feed(&s, &at(32, col + 4, false));
        assert_eq!(selected(&s).as_deref(), Some("hello"));
        // サイドバーへはみ出しても端末の左端で止まる
        inp.feed(&s, &at(32, 2, false));
        // 端末の左端から押した位置までで、サイドバーの文字は入らない
        assert!(selected(&s).is_some_and(|t| t.chars().count() <= col - 24), "{:?}", selected(&s));
        for t in super::lock(&s).tabs.iter() {
            let _ = std::process::Command::new("kill").arg(t.child.process_id().unwrap().to_string()).status();
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn auto_yes_loop_answers_three_way_proceed_but_not_finished_answers() {
        let _fork = crate::FORK_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let s: super::Shared = Default::default();
        let dir = std::env::temp_dir().join(format!("mdeck-autoyes-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        {
            let mut st = super::lock(&s);
            st.sock = dir.join("t.sock").to_string_lossy().into_owned();
            st.cols = 100;
            st.rows = 30;
            st.sidebar = 24;
        }
        // claude の「Do you want to proceed?」と同じ 3 択を出し、押されたキーをファイルに書く
        let menu = |name: &str| {
            let out = dir.join(format!("{name}.txt"));
            // done のタブは回答の文中の番号付きの行 (カーソル無し) を出す
            let cursor = if name == "done" { " " } else { "❯" };
            let run = format!(
                "stty raw -echo; printf ' Do you want to proceed?\\r\\n {cursor} 1. Yes\\r\\n   2. Yes, and switch to auto mode · auto mode handles these prompts for you\\r\\n   3. No\\r\\n'; cat > '{}'",
                out.display()
            );
            let id = super::api(&s, &json!({"cmd": "new", "cwd": dir.to_str().unwrap(), "run": run, "name": name})).unwrap()["id"].as_u64().unwrap() as u32;
            (id, out)
        };
        let (_, busy_out) = menu("busy");
        let (_, done_out) = menu("done");
        std::thread::sleep(std::time::Duration::from_millis(500));
        {
            let mut st = super::lock(&s);
            for t in st.tabs.iter_mut() {
                t.auto_yes = true;
                t.agent = Some("claude".into());
                // メインの会話が完了した後 (背景のサブエージェントの確認) でも答える。カーソルの無い「1. Yes」には答えない
                t.state = Some("done");
            }
        }
        let s2 = s.clone();
        std::thread::spawn(move || super::auto_yes_loop(s2));
        let typed = (0..30).find_map(|_| {
            std::thread::sleep(std::time::Duration::from_millis(100));
            std::fs::read(&busy_out).ok().filter(|b| !b.is_empty())
        });
        assert_eq!(typed.as_deref(), Some(&b"1"[..]), "1. Yes を選ばない");
        std::thread::sleep(std::time::Duration::from_millis(2000));
        assert_eq!(std::fs::read(&busy_out).unwrap(), b"1", "同じ確認を二度押した");
        assert!(std::fs::read(&done_out).unwrap_or_default().is_empty(), "カーソルの無い番号付きの行で押した");
        for t in super::lock(&s).tabs.iter() {
            let _ = std::process::Command::new("kill").arg(t.child.process_id().unwrap().to_string()).status();
        }
        std::fs::remove_dir_all(dir).unwrap();
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
        // F12 でカーソルは表示中のタブ。上で Config (Exit の上)、Enter で開く
        inp.feed(&s, f12);
        assert_eq!(ui(&s), (true, tab0, false, None));
        // 選んでいるタブの右端に → (メニューの印)
        {
            let st = super::lock(&s);
            let mut t = ratatui::Terminal::new(ratatui::backend::TestBackend::new(st.cols, st.rows)).unwrap();
            t.draw(|f| super::draw(f, &st)).unwrap();
            let y = super::rows(st.sidebar).2 + (tab0 - super::CUR_TABS) as u16;
            assert_eq!(t.backend().buffer()[(st.sidebar - 2, y)].symbol(), "→");
        }
        inp.feed(&s, b"kk\r");
        assert_eq!(ui(&s), (false, super::CUR_CONFIG, true, None));
        inp.feed(&s, b"\x1b");
        assert_eq!(ui(&s), (false, super::CUR_CONFIG, true, None), "Config の Esc で閉じた");
        inp.feed(&s, b"\x1b[D");
        assert_eq!(ui(&s), (true, super::CUR_CONFIG, false, None), "Config の ← はサイドバーへ戻る");
        inp.feed(&s, b"\x1b");
        assert_eq!(ui(&s), (true, super::CUR_CONFIG, false, None), "サイドバーの Esc で閉じた");
        inp.feed(&s, b"\x1b[C");
        assert_eq!(ui(&s), (false, super::CUR_CONFIG, true, None), "→ でもコンフィグを開く");
        inp.feed(&s, f12);

        // 下でタブを選ぶとそのタブを表示し、Enter でそのタブのメニュー
        inp.feed(&s, f12);
        inp.feed(&s, b"j");
        assert_eq!(super::lock(&s).active, 1);
        inp.feed(&s, b"\x1b[C");
        assert_eq!(ui(&s), (false, tab0 + 1, false, Some((Some(b), 0))), "→ でタブのメニューを開かない");
        inp.feed(&s, b"\x1b[C");
        assert_eq!(ui(&s), (false, tab0 + 1, false, Some((Some(b), 0))), "タブのメニューの → で閉じた");
        inp.feed(&s, b"\x1b[D");
        assert_eq!(ui(&s), (true, tab0 + 1, false, None), "← でサイドバーへ戻らない");
        inp.feed(&s, b"\r");
        assert_eq!(ui(&s), (false, tab0 + 1, false, None), "タブの Enter で作業に戻らない");
        inp.feed(&s, f12);
        inp.feed(&s, b"\x1b[C");

        // 名前を変える
        inp.feed(&s, b"\r");
        inp.feed(&s, b"alpha\r");
        assert_eq!(super::lock(&s).tabs.iter().find(|t| t.id == b).unwrap().name.as_deref(), Some("alpha"));
        // Config の通知先で botX を足す (名前 → URL)
        let open_dest = |s: &super::Shared| {
            let mut inp = super::Input::default();
            inp.feed(s, f12);
            inp.feed(s, f12);
            let c = super::lock(s).cursor;
            inp.feed(s, &b"k".repeat(c));
            inp.feed(s, b"\r");
            inp.feed(s, &b"j".repeat(super::M_DEST));
            inp.feed(s, b"\r");
            assert!(super::lock(s).session.as_ref().is_some_and(|p| p.tab.is_none()), "通知先の一覧が開かない");
        };
        open_dest(&s);
        inp.feed(&s, b"\rbotX\rx.example/h\r");
        assert_eq!(super::lock(&s).owner_webhooks["botX"][0]["url"], "https://x.example/h");
        assert!(super::lock(&s).webhooks.is_empty(), "全体の一覧に入った");
        // 追加した通知先は先頭の行。Enter で URL を編集
        assert_eq!(ui(&s).3, Some((None, 0)));
        let url = |s: &super::Shared| super::lock(s).owner_webhooks["botX"][0]["url"].clone();
        // 入力中の ←/→ はカーソルを動かすだけで閉じない
        inp.feed(&s, b"\r");
        inp.feed(&s, b"\x1b[D\x1b[D\x1b[C");
        inp.feed(&s, b"zz");
        inp.feed(&s, b"\x7f");
        let buf = super::lock(&s).session.as_ref().and_then(|p| p.edit.clone()).map(|e| e.1);
        assert_eq!(buf.as_deref(), Some("https://x.example/zh"), "← で閉じたか、カーソルの位置に入らない");
        // Esc は何もしない。編集を続けて 2 を足す
        inp.feed(&s, b"\x1b\x7f");
        assert!(super::lock(&s).session.as_ref().unwrap().edit.is_some(), "Esc で入力をやめた");
        assert_eq!(url(&s), "https://x.example/h");
        inp.feed(&s, b"\x1b[C2\r");
        assert_eq!(url(&s), "https://x.example/h2", "Enter で保存しない");
        // タブのメニューでは通知先を ←/→ で選ぶ (なし ↔ botX)。通知先の行の ← は「編集」なので、追加の行で ← を押して戻る
        inp.feed(&s, b"j\x1b[D");
        assert_eq!(ui(&s), (false, super::CUR_CONFIG, true, None), "通知先の一覧の ← で Config に戻らない");
        inp.feed(&s, b"\x1b[D");
        let c = super::lock(&s).cursor;
        inp.feed(&s, &b"j".repeat(tab0 + 1 - c));
        inp.feed(&s, b"\x1b[Cj\x1b[C");
        let dest = |s: &super::Shared| super::lock(s).tabs.iter().find(|t| t.id == b).unwrap().owner.clone();
        assert_eq!(dest(&s).as_deref(), Some("botX"));
        inp.feed(&s, b"\x1b[C");
        assert_eq!(dest(&s), None);
        inp.feed(&s, b"\x1b[D");
        assert_eq!(dest(&s).as_deref(), Some("botX"));
        // → で「削除」を選ぶと確認になり、既定の「削除しない」では消さない
        open_dest(&s);
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
        assert!(super::lock(&s).session.is_some(), "Esc で閉じた");
        inp.feed(&s, b"j\x1b[D\x1b[D");
        assert_eq!(ui(&s), (true, super::CUR_CONFIG, false, None), "← で Config からサイドバーへ戻らない");
        inp.feed(&s, b"jj\x1b[C\r");
        inp.feed(&s, b"alpha\r");
        assert!(super::lock(&s).tabs.iter().find(|t| t.id == a).unwrap().name.is_none());
        inp.feed(&s, f12);
        assert_eq!(ui(&s).3, None);

        // 自動 Yes は Enter で切り替わる
        inp.feed(&s, f12);
        inp.feed(&s, b"\x1b[Cjj\r");
        let auto = |s: &super::Shared| super::lock(s).tabs.iter().find(|t| t.id == a).unwrap().auto_yes;
        assert!(auto(&s));
        inp.feed(&s, b"\r");
        assert!(!auto(&s));
        // 「タブを削除する」は確認になり、既定の「削除しない」で Enter を重ねても閉じない
        inp.feed(&s, b"j\r");
        assert!(super::lock(&s).session.as_ref().unwrap().confirm_close);
        inp.feed(&s, b"\r");
        assert!(!super::lock(&s).session.as_ref().unwrap().confirm_close, "「閉じない」で確認が終わらない");
        assert_eq!(super::lock(&s).tabs.len(), 2);
        inp.feed(&s, b"\r\x1b[D");
        assert!(!super::lock(&s).session.as_ref().unwrap().confirm_close, "確認中の ← でやめない");
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

        // F12 操作中は画面全体に枠
        inp.feed(&s, f12);
        {
            let st = super::lock(&s);
            let mut t = ratatui::Terminal::new(ratatui::backend::TestBackend::new(st.cols, st.rows)).unwrap();
            t.draw(|f| super::draw(f, &st)).unwrap();
            let b = t.backend().buffer();
            let row = |y: u16| (0..b.area.width).map(|x| b[(x, y)].symbol().to_string()).collect::<String>();
            assert!(row(0).starts_with('┌') && row(0).replace(" ", "").contains("F12操作中"), "{}", row(0));
            assert_eq!((b[(0, 10)].symbol(), b[(b.area.width - 1, 10)].symbol()), ("│", "│"));
            if std::env::var("MDECK_DUMP").is_ok() {
                (0..b.area.height).for_each(|y| eprintln!("{}", row(y)));
            }
        }
        inp.feed(&s, f12);

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
    fn set_owner_routes_done_to_new_owner_webhook() {
        use std::io::Read;
        let _fork = crate::FORK_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let s: super::Shared = Default::default();
        let dir = std::env::temp_dir().join(format!("mdeck-setowner-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        {
            let mut st = super::lock(&s);
            st.sock = dir.join("t.sock").to_string_lossy().into_owned();
            st.cols = 100;
            st.rows = 30;
            st.sidebar = 24;
        }
        let api = |req: serde_json::Value| super::api(&s, &req);
        let id = api(json!({"cmd": "new", "cwd": dir.to_str().unwrap(), "run": ""})).unwrap()["id"].as_u64().unwrap();
        let srv = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/h", srv.local_addr().unwrap());
        let r = api(json!({"cmd": "owner", "tab": id, "owner": "botY", "webhook": url})).unwrap();
        assert_eq!(r["tab"]["owner"], "botY");
        let listed = api(json!({"cmd": "list"})).unwrap();
        assert_eq!(listed["tabs"][0]["owner"], "botY");
        // done が続いても 2 回目を送る
        {
            let mut st = super::lock(&s);
            super::set_state(&s, &mut st, id as u32, Some("done"));
            super::set_state(&s, &mut st, id as u32, Some("done"));
        }
        let (mut c, _) = srv.accept().unwrap();
        c.set_read_timeout(Some(std::time::Duration::from_secs(5))).unwrap();
        let mut got = String::new();
        let mut buf = [0u8; 4096];
        while !got.contains("\"botY\"") {
            let n = c.read(&mut buf).unwrap();
            assert!(n > 0, "{got}");
            got.push_str(&String::from_utf8_lossy(&buf[..n]));
        }
        assert!(got.contains("\"done\""), "{got}");
        drop(c);
        // 送信結果は webhook.log に残り、URL の ID は全部は出さない
        let log = (0..50).find_map(|_| {
            std::thread::sleep(std::time::Duration::from_millis(100));
            std::fs::read_to_string(dir.join("webhook.log")).ok().filter(|l| l.contains("http="))
        }).expect("webhook.log");
        assert!(log.contains("owner=\"botY\"") && log.contains("state=\"done\"") && !log.contains("/h\n"), "{log}");
        srv.set_nonblocking(true).unwrap();
        let second = (0..50).any(|_| {
            std::thread::sleep(std::time::Duration::from_millis(100));
            srv.accept().is_ok()
        });
        assert!(second, "2 回目の done を送らない");
        // 長い webhook を編集に入っても箱の幅が変わらない
        let long = format!("https://example.com/{}", "a".repeat(60));
        api(json!({"cmd": "webhooks", "owner": "botY", "add": long})).unwrap();
        let width = |s: &super::Shared| {
            let st = super::lock(s);
            let mut t = ratatui::Terminal::new(ratatui::backend::TestBackend::new(st.cols, st.rows)).unwrap();
            t.draw(|f| super::draw(f, &st)).unwrap();
            // ポップアップの枠 (画面の枠より内側の ┌ から ┐ まで)
            let b = t.backend().buffer();
            (0..b.area.height).find_map(|y| {
                let l = (1..b.area.width).find(|&x| b[(x, y)].symbol() == "┌")?;
                (l..b.area.width).find(|&x| b[(x, y)].symbol() == "┐").map(|r| r - l + 1)
            }).unwrap()
        };
        {
            let mut st = super::lock(&s);
            // 画面幅で頭打ちにならないよう広くする
            st.cols = 200;
            let t = st.tabs.iter().find(|t| t.id as u64 == id).unwrap().id;
            let _ = t;
            st.session = Some(super::SessionPop::new(None));
            let p = st.session.as_ref().unwrap();
            let sel = super::session_items(&st, p).iter().position(|x| x.0 == super::SessionItem::Hook("botY".into(), long.clone())).unwrap();
            st.session.as_mut().unwrap().sel = sel;
        }
        let before = width(&s);
        super::session_input(&s, b"\r");
        assert!(super::lock(&s).session.as_ref().unwrap().edit.is_some());
        assert_eq!(width(&s), before, "編集に入ると箱の幅が変わった");
        super::lock(&s).session = None;
        // 外すと tab の owner は null、不正な名前は断る
        assert!(api(json!({"cmd": "owner", "tab": id, "owner": ""})).unwrap()["tab"]["owner"].is_null());
        assert!(api(json!({"cmd": "owner", "tab": id, "owner": "a b"})).is_err());
        assert!(api(json!({"cmd": "owner", "tab": id, "webhook": "https://g/x"})).unwrap_err().contains("owner"));
        for t in super::lock(&s).tabs.iter() {
            let _ = std::process::Command::new("kill").arg(t.child.process_id().unwrap().to_string()).status();
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn sidebar_shows_weekly_usage_and_one_line_title() {
        use crate::usage::{Week, WEEK};
        let mut st = super::State { sidebar: 24, ..Default::default() };
        let now = super::now_secs();
        // 週の半分で 25% (1d18h の余裕)、codex は記録なし、copilot は月 (30 日) の半分で 10% (12 日の余裕)
        let month = 30 * 86400;
        st.usage = [Some(Week { used: 25.0, resets_at: now + WEEK / 2, window: WEEK }), None, Some(Week { used: 10.0, resets_at: now + month / 2, window: month })];
        let mut t = ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 30)).unwrap();
        t.draw(|f| super::draw(f, &st)).unwrap();
        let b = t.backend().buffer();
        let row = |y: u16| (0..24).map(|x| b[(x, y)].symbol().to_string()).collect::<String>();
        if std::env::var("MDECK_DUMP").is_ok() {
            (0..30).for_each(|y| eprintln!("{}", row(y)));
        }
        assert!(row(0).starts_with(&format!(" {}", super::head_title())), "{}", row(0));
        assert!(row(1).contains("Config"), "{}", row(1));
        assert!(row(21).contains("Claude -1d18h"), "{}", row(21));
        assert_eq!(b[(1 + 11, 22)].symbol(), "│", "今の位置");
        assert_eq!(b[(1, 22)].bg, ratatui::style::Color::LightGreen, "使用量の塗り");
        assert!(row(23).contains("Codex —"), "{}", row(23));
        assert!(row(25).contains("Copilot -12d"), "{}", row(25));
        // 狭いと見出しは 2 行
        assert_eq!(super::rows(14).0, 2);
        // F12 で Exit / + / Config を選ぶと、右側は端末の代わりに説明かメニュー
        let all = |st: &super::State| {
            let mut t = ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 30)).unwrap();
            t.draw(|f| super::draw(f, st)).unwrap();
            let b = t.backend().buffer().clone();
            (0..30).map(|y| (0..80).map(|x| b[(x, y)].symbol().to_string()).collect::<String>().replace(' ', "")).collect::<Vec<_>>().join("\n")
        };
        assert!(all(&st).contains("タブがない"));
        st.prefix = true;
        for (cursor, want) in [(super::CUR_EXIT, "MornDeckの画面を閉じる"), (super::CUR_TABS, "シェルのタブをホームで開く"), (super::CUR_CONFIG, "Enter/→で開く")] {
            st.cursor = cursor;
            let text = all(&st);
            assert!(text.contains(want) && !text.contains("タブがない"), "{cursor}: {text}");
        }
        // Config の設定値は始まりの列が揃う
        let items = super::menu_items(&super::Menu { sel: 0, notice: None, update: None }, &st);
        let col = |s: &str| {
            let (l, _) = s.split_once("  ").unwrap();
            let rest = &s[l.len()..];
            ratatui::text::Line::from(l).width() + rest.len() - rest.trim_start().len()
        };
        assert_eq!(col(&items[super::M_SIDEBAR]), col(&items[super::M_SOUND]), "{items:?}");
        assert_eq!(col(&items[super::M_SOUND]), col(&items[super::M_QUIET]), "{items:?}");
    }

    #[test]
    fn done_is_unread_until_read_and_alert_settings_persist() {
        let _fork = crate::FORK_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let s: super::Shared = Default::default();
        let dir = std::env::temp_dir().join(format!("mdeck-unread-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let sock = dir.join("t.sock").to_string_lossy().into_owned();
        super::lock(&s).sock = sock.clone();
        let api = |req: serde_json::Value| super::api(&s, &req);
        let id = api(json!({"cmd": "new", "cwd": dir.to_str().unwrap(), "run": ""})).unwrap()["id"].as_u64().unwrap() as u32;
        let unread = |s: &super::Shared| super::lock(s).tabs[0].unread;
        {
            let mut st = super::lock(&s);
            super::set_state(&s, &mut st, id, Some("done"));
        }
        assert!(unread(&s), "画面も API も見ていない完了が未読にならない");
        api(json!({"cmd": "list"})).unwrap();
        assert!(unread(&s), "一覧で既読になった");
        api(json!({"cmd": "answer", "tab": id})).unwrap();
        assert!(!unread(&s), "answer で既読にならない");
        // Config の切り替えは config.json に残る (既定は音・通知オフ、webhook のあるタブは鳴らさない)
        assert_eq!(super::load_alerts(&sock), super::Alerts { sound: false, notify: false, quiet_hooked: true });
        for sel in [super::M_NOTIFY, super::M_QUIET] {
            super::lock(&s).menu = Some(super::Menu { sel, notice: None, update: None });
            super::menu_input(&s, b"\r");
        }
        assert_eq!(super::load_alerts(&sock), super::Alerts { sound: false, notify: true, quiet_hooked: false });
        for t in super::lock(&s).tabs.iter() {
            let _ = std::process::Command::new("kill").arg(t.child.process_id().unwrap().to_string()).status();
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn copilot_hook_payload_is_camel_case_with_event_from_command() {
        let _fork = crate::FORK_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let s: super::Shared = Default::default();
        let dir = std::env::temp_dir().join(format!("mdeck-cophook-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        super::lock(&s).sock = dir.join("t.sock").to_string_lossy().into_owned();
        let api = |req: serde_json::Value| super::api(&s, &req);
        let id = api(json!({"cmd": "new", "cwd": dir.to_str().unwrap(), "run": ""})).unwrap()["id"].as_u64().unwrap();
        let hook = |event: &str, payload: serde_json::Value| api(json!({"cmd": "hook", "agent": "copilot", "event": event, "tab": id, "payload": payload})).unwrap();
        hook("UserPromptSubmit", json!({"sessionId": "abc-1", "prompt": "x", "cwd": "/"}));
        {
            let st = super::lock(&s);
            assert_eq!((st.tabs[0].state, st.tabs[0].agent.as_deref(), st.tabs[0].session.as_deref()), (Some("working"), Some("copilot"), Some("abc-1")));
        }
        hook("Stop", json!({"sessionId": "abc-1", "stopReason": "end_turn", "transcriptPath": "/x"}));
        let (state, seq) = {
            let st = super::lock(&s);
            (st.tabs[0].state, st.tabs[0].answer_seq)
        };
        assert_eq!((state, seq), (Some("done"), 1));
        // 再開は copilot --resume
        assert_eq!(super::resume_command(Some("copilot"), Some("abc-1"), Some("--yolo")).as_deref(), Some("copilot --resume abc-1 --yolo"));
        for t in super::lock(&s).tabs.iter() {
            let _ = std::process::Command::new("kill").arg(t.child.process_id().unwrap().to_string()).status();
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn webhook_test_posts_now_and_returns_status() {
        use std::io::{Read, Write};
        let s: super::Shared = Default::default();
        let dir = std::env::temp_dir().join(format!("mdeck-whtest-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        super::lock(&s).sock = dir.join("t.sock").to_string_lossy().into_owned();
        let api = |req: serde_json::Value| super::api(&s, &req);
        assert!(api(json!({"cmd": "webhook_test", "owner": "botT"})).unwrap_err().contains("登録されていない"));
        let srv = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/secret-id", srv.local_addr().unwrap());
        api(json!({"cmd": "webhooks", "owner": "botT", "add": url})).unwrap();
        let reply = |code: &'static str| {
            let srv = srv.try_clone().unwrap();
            std::thread::spawn(move || {
                let (mut c, _) = srv.accept().unwrap();
                let mut got = Vec::new();
                let mut buf = [0u8; 4096];
                while !String::from_utf8_lossy(&got).contains("テスト通知") {
                    let n = c.read(&mut buf).unwrap();
                    got.extend_from_slice(&buf[..n]);
                }
                let _ = c.write_all(format!("HTTP/1.1 {code}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").as_bytes());
            })
        };
        let t = reply("204 No Content");
        let r = api(json!({"cmd": "webhook_test", "owner": "botT"})).unwrap();
        t.join().unwrap();
        assert_eq!((r["ok"].as_bool(), r["results"][0]["http"].as_u64()), (Some(true), Some(204)), "{r}");
        assert!(!r.to_string().contains("secret-id"), "URL の末尾を伏せていない: {r}");
        let t = reply("401 Unauthorized");
        let r = api(json!({"cmd": "webhook_test", "owner": "botT"})).unwrap();
        t.join().unwrap();
        assert_eq!((r["ok"].as_bool(), r["results"][0]["http"].as_u64()), (Some(false), Some(401)), "{r}");
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
        let a = super::agent_args;
        assert_eq!(a(Some("claude"), true, None).as_deref(), Some("--dangerously-skip-permissions"));
        assert_eq!(a(Some("codex"), true, Some("-m x")).as_deref(), Some("--dangerously-bypass-approvals-and-sandbox -m x"));
        assert_eq!(a(Some("claude"), false, Some("-c")).as_deref(), Some("-c"));
        assert_eq!(a(None, true, None), None);
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
        assert_eq!(super::yes_choice(&o), Some(0));
        // 質問など Yes / No 以外が混じる選択肢は選ばない
        let q: Vec<String> = [" ❯ 1. 赤", "   2. No"].iter().map(|s| s.to_string()).collect();
        assert_eq!(super::yes_choice(&super::parse_options(&q)), None);
        let codex: Vec<String> = [" › 1. Yes, proceed (y)", "   2. No, and tell Codex what to do differently (esc)"].iter().map(|s| s.to_string()).collect();
        assert_eq!(super::yes_choice(&super::parse_options(&codex)), Some(0));
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
    // 画面が複数つながっているときは、最後に操作した画面の大きさに合わせる
    let mut size = (req["cols"].as_u64().unwrap_or(80) as u16, req["rows"].as_u64().unwrap_or(24) as u16);
    resize(&s, size.0, size.1);
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
                    size = (u16::from_be_bytes([b[0], b[1]]), u16::from_be_bytes([b[2], b[3]]));
                    resize(&s, size.0, size.1);
                }
                _ => {
                    let mut l = [0u8; 4];
                    rd.read_exact(&mut l)?;
                    let mut data = vec![0u8; u32::from_be_bytes(l) as usize];
                    rd.read_exact(&mut data)?;
                    resize(&s, size.0, size.1);
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
        _ => {}
    }
    if (CUR_TABS..plus).contains(&st.cursor) {
        st.active = st.cursor - CUR_TABS;
    }
    mark(s, &mut st);
    (used, false)
}

/// サイドバーの項目を開く (Enter / →)。戻り値は離脱するか。
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
    /// 端末の中をドラッグして文字を選んでいる
    selecting: bool,
}

impl Input {
    /// returns true when the client asked to detach.
    fn feed(&mut self, s: &Shared, data: &[u8]) -> bool {
        let mut fwd = Vec::new();
        let mut i = 0;
        while i < data.len() {
            // マウスの報告 (SGR) は選択とホイールに使い、中へは流さない。旧形式 (ESC [ M + 3 バイト) は捨てる
            if data[i..].starts_with(b"\x1b[<")
                && let Some(end) = data[i..].iter().position(|c| *c == b'M' || *c == b'm')
            {
                flush(s, &mut fwd);
                self.mouse(s, &data[i + 3..i + end], data[i + end] == b'M');
                i += end + 1;
                continue;
            }
            if data[i..].starts_with(b"\x1b[M") && data.len() >= i + 6 {
                i += 6;
                continue;
            }
            if data[i..].starts_with(PREFIX) {
                // F12 は中に送らない。開いているもの (サイドバー操作・Config・タブのメニュー) を閉じ、何も無ければサイドバー操作に入る
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

    /// 端末の中の左ドラッグで文字を選び、離したらクリップボードへ。ホイールはスクロール。クリックでの操作は無い
    fn mouse(&mut self, s: &Shared, seq: &[u8], press: bool) {
        let p: Vec<i32> = std::str::from_utf8(seq).unwrap_or("").split(';').filter_map(|x| x.parse().ok()).collect();
        let [btn, x, y] = p[..] else { return };
        let (x, y) = (x - 1, y - 1);
        let mut st = lock(s);
        if st.menu.is_some() || st.session.is_some() {
            return;
        }
        let side = st.sidebar as i32;
        let a = st.active;
        let Some(t) = st.tabs.get_mut(a) else { return };
        if btn & 64 != 0 {
            let up = btn & 1 == 0;
            let mode = *t.term.mode();
            if let Some(report) = mouse_report(mode, btn, x - side + 1, y + 1) {
                let _ = t.writer.write_all(&report);
            } else if mode.contains(TermMode::ALT_SCREEN | TermMode::ALTERNATE_SCROLL) {
                let key: &[u8] = match (up, mode.contains(TermMode::APP_CURSOR)) {
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
        // 端末の外 (サイドバー) へはみ出しても、端末の端の列で選ぶ
        let g = t.term.grid();
        let col = (x - side).clamp(0, g.columns() as i32 - 1) as usize;
        let pt = Point::new(Line(y.clamp(0, g.screen_lines() as i32 - 1) - g.display_offset() as i32), Column(col));
        match (self.selecting, btn & 32 != 0, press) {
            (false, false, true) if x >= side => {
                t.term.selection = Some(Selection::new(SelectionType::Simple, pt, Side::Left));
                self.selecting = true;
            }
            (true, true, _) => {
                if let Some(sel) = t.term.selection.as_mut() {
                    sel.update(pt, Side::Right);
                }
            }
            (true, false, false) => {
                self.selecting = false;
                let text = t.term.selection_to_string().filter(|s| !s.trim().is_empty());
                match text {
                    Some(text) => copy(&mut st, &text),
                    None => t.term.selection = None,
                }
            }
            _ => {}
        }
        mark(s, &mut st);
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
