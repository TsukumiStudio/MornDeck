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
    /// session を受け入れたときの前面のプロセス群。別の会話のフックが、起動し直した会話か中で動く別の会話かを見分ける
    session_pgrp: Option<libc::pid_t>,
    args: Option<String>,
    /// API や復元でエージェントを起動し、まだ SessionStart が届いていない
    starting: bool,
    /// タブの持ち主 (owner)。まとめて閉じるときに使う
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
    /// 答えなかったメニューの画面 (auto_yes.log に同じ画面を何度も書かない)
    auto_skipped: Option<Vec<String>>,
    /// 前面で動いているエージェント (フックの無いときのアイコン) と、調べた時刻。描くたびに調べると重いので 1 秒は使い回す
    fg: std::cell::Cell<(Option<&'static str>, Option<std::time::Instant>)>,
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
    /// タブの状態か画面が変わるたびに進む。待つ側 (wait / auto_yes_loop) が見逃しを見分ける
    changes: u64,
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
    /// owner ごとの webhook。その owner のタブの通知だけを送る
    owner_webhooks: serde_json::Map<String, Value>,
    /// 完了したときの音と通知 (Config で切り替え、config.json に残す)
    alerts: Alerts,
    /// 最後に描いた画面の、マウスで押せる場所
    hits: std::cell::RefCell<Vec<(Rect, Hit)>>,
    /// マウスが乗っているサイドバーの行 (CUR_CONFIG / CUR_EXIT / CUR_TABS.. / 「+」)
    hover: Option<usize>,
    /// claude / codex の週次と copilot の月次の使用量 (サイドバー下部のゲージ)
    usage: [Option<crate::usage::Week>; 3],
}

#[derive(Clone, Debug, PartialEq)]
struct Alerts {
    /// 完了の通知音 (/System/Library/Sounds の名前)。None は鳴らさない
    sound: Option<String>,
    notify: bool,
    /// webhook の届くタブでは鳴らさない・出さない
    quiet_hooked: bool,
    /// 使用量のゲージ。オフならトークンも記録も読まず、使用量の API も呼ばない
    usage: bool,
}

impl Default for Alerts {
    fn default() -> Self {
        Alerts { sound: None, notify: false, quiet_hooked: true, usage: true }
    }
}

const SOUNDS_DIR: &str = "/System/Library/Sounds";

/// 選べる通知音 (mac のシステムの音の名前)
fn sounds() -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(SOUNDS_DIR)
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok()?.path().file_stem()?.to_str().map(str::to_string))
        .collect();
    v.sort();
    v
}

fn play_sound(name: &str) {
    let path = format!("{SOUNDS_DIR}/{name}.aiff");
    let _ = std::process::Command::new("/usr/bin/afplay").arg(path).spawn().map(|mut c| std::thread::spawn(move || c.wait()));
}

fn config_path(sock: &str) -> std::path::PathBuf {
    std::path::Path::new(sock).with_file_name("config.json")
}

fn load_alerts(sock: &str) -> Alerts {
    let v: Value = std::fs::read_to_string(config_path(sock)).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
    let d = Alerts::default();
    let b = |k: &str, d: bool| v[k].as_bool().unwrap_or(d);
    // 前の版はオン/オフ (true は Glass)
    let sound = v["sound"].as_str().map(str::to_string).or_else(|| (v["sound"] == true).then(|| "Glass".into()));
    Alerts { sound, notify: b("notify", d.notify), quiet_hooked: b("quiet_hooked", d.quiet_hooked), usage: b("usage", d.usage) }
}

fn save_alerts(sock: &str, a: &Alerts) {
    let v = json!({"sound": a.sound, "notify": a.notify, "quiet_hooked": a.quiet_hooked, "usage": a.usage});
    log_event(sock, "config", &v);
    let _ = write_atomic(&config_path(sock), &v.to_string());
}

/// 完了したタブを見ていないときに、通知音と macOS の通知を出す
fn alert(st: &State, i: usize) {
    let a = &st.alerts;
    let t = &st.tabs[i];
    let hooked = !st.webhooks.is_empty() || t.owner.as_ref().and_then(|o| st.owner_webhooks.get(o)).and_then(Value::as_array).is_some_and(|l| !l.is_empty());
    if a.quiet_hooked && hooked {
        return;
    }
    if let Some(name) = &a.sound {
        play_sound(name);
    }
    if a.notify {
        let name = t.name.clone().unwrap_or_else(|| t.title.clone());
        let text: String = t.answer.as_deref().unwrap_or("done").chars().take(120).collect();
        // 文字列は引数で渡す (スクリプトに埋めると引用符で壊れる)
        let _ = std::process::Command::new("/usr/bin/osascript")
            .args(["-e", "on run argv", "-e", "display notification (item 1 of argv) with title \"MornDeck\" subtitle (item 2 of argv)", "-e", "end run", &text, &format!("{name} finished")])
            .spawn()
            .map(|mut c| std::thread::spawn(move || c.wait()));
    }
}

/// サイドバーでタブを選んで → で開く、そのタブのメニュー
struct SessionPop {
    tab: u32,
    sel: usize,
    /// タブ名の入力中
    name: Option<LineEdit>,
    notice: Option<String>,
    /// 「タブを削除」を選んだ。「削除しない」/「削除する」を選ばせる (エージェントの会話ごと終わるので)
    confirm_close: bool,
}

#[derive(Clone, PartialEq, Debug)]
enum SessionItem {
    Name,
    Dest,
    AutoYes,
    Close,
    /// 閉じる (F12)
    Done,
    KeepOpen,
    ConfirmClose,
}

/// マウスで押せる場所
#[derive(Clone, Copy, PartialEq, Debug)]
enum Hit {
    /// サイドバーの行 (CUR_CONFIG / CUR_EXIT / CUR_TABS.. / 「+」)
    Side(usize),
    /// サイドバーの境界 (ドラッグで幅を変える)
    Border,
    /// Config の項目 (選べる行の位置)
    MenuRow(usize),
    /// Config の通知先の行の「編集」(false) / 「削除」(true)
    MenuButton(usize, bool),
    /// タブのメニューの項目
    TabRow(usize),
}

/// 1 行の入力欄。back は入力位置 (末尾から何文字手前か。0 で末尾)
#[derive(Clone, Default, Debug)]
struct LineEdit {
    buf: String,
    back: usize,
}

impl LineEdit {
    fn new(s: impl Into<String>) -> Self {
        LineEdit { buf: s.into(), back: 0 }
    }

    /// キー 1 つ分を処理し、使ったバイト数と Enter だったかを返す。←/→ で入力位置を動かし、BS で消す。
    /// Esc や他のエスケープ列は何もしない (やめるのは F12)
    fn key(&mut self, rest: &[u8]) -> (usize, bool) {
        let starts = |a: &[u8], b: &[u8]| rest.starts_with(a) || rest.starts_with(b);
        let used = if rest[0] == 0x1b { esc_len(rest) } else { 1 };
        let len = self.buf.chars().count();
        self.back = self.back.min(len);
        let at = self.buf.char_indices().nth(len - self.back).map_or(self.buf.len(), |(i, _)| i);
        match rest[0] {
            _ if starts(b"\x1b[D", b"\x1bOD") => self.back = (self.back + 1).min(len),
            _ if starts(b"\x1b[C", b"\x1bOC") => self.back = self.back.saturating_sub(1),
            b'\r' => return (1, true),
            0x7f | 0x08 => {
                if let Some((i, _)) = self.buf[..at].char_indices().next_back() {
                    self.buf.remove(i);
                }
            }
            c if c < 0x20 || c == 0x1b => {}
            _ => {
                let n = rest.iter().position(|c| *c < 0x20 || *c == 0x7f).unwrap_or(rest.len());
                self.buf.insert_str(at, &String::from_utf8_lossy(&rest[..n]));
                return (n, false);
            }
        }
        (used, false)
    }

    /// 入力位置に ▏ を入れた表示
    fn shown(&self) -> String {
        let at = self.buf.chars().count().saturating_sub(self.back);
        format!("{}▏{}", self.buf.chars().take(at).collect::<String>(), self.buf.chars().skip(at).collect::<String>())
    }
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
    /// 通知先の入力中
    edit: Option<DestEdit>,
    /// 通知先の行で ←/→ で選ぶ操作。true は削除 (確認中は「削除する」)
    del: bool,
    /// 通知先の削除を確かめている
    confirm: bool,
}

impl Menu {
    fn new(sel: usize) -> Self {
        Menu { sel, notice: None, update: None, edit: None, del: false, confirm: false }
    }
}

/// 通知先の入力。追加は名前 → URL の順、編集は URL だけ
struct DestEdit {
    /// 編集する通知先の元の (名前, URL)。None は追加
    target: Option<(String, String)>,
    /// 決めた名前。None なら名前を入力中
    name: Option<String>,
    line: LineEdit,
}

/// new_tab の yolo で付ける、確認もサンドボックスも無しで動かすフラグ
const YOLO: [(&str, &str); 3] =
    [("claude", "--dangerously-skip-permissions"), ("codex", "--dangerously-bypass-approvals-and-sandbox"), ("copilot", "--yolo")];
/// タブで起動できるエージェント
const AGENTS: [&str; 3] = ["claude", "codex", "copilot"];

/// Config の行。Head は見出し (選べない)。並びは menu_rows
#[derive(Clone, PartialEq, Debug)]
enum Row {
    Head(&'static str),
    /// 「更新を確認」。新しい版が見つかると「更新を反映」に変わる
    Update,
    /// hooks::status() の順 (claude, codex, copilot)
    Hook(usize),
    Sidebar,
    /// 使用量のゲージを出すか
    Usage,
    Sound,
    Popup,
    Quiet,
    /// 通知先の (名前, URL)。Enter で通知先の一覧を開いて編集する
    Dest(String, String),
    AddDest,
    Close,
}

fn menu_rows(st: &State) -> Vec<Row> {
    let mut v = vec![Row::Update, Row::Head("Hooks")];
    v.extend((0..st.hooks.len()).map(Row::Hook));
    v.extend([Row::Head("Appearance"), Row::Sidebar, Row::Usage, Row::Head("Notifications"), Row::Sound, Row::Popup, Row::Quiet, Row::Head("Webhooks")]);
    for (name, list) in &st.owner_webhooks {
        for h in list.as_array().into_iter().flatten() {
            v.push(Row::Dest(name.clone(), h["url"].as_str().unwrap_or("").to_string()));
        }
    }
    v.extend([Row::AddDest, Row::Close]);
    v
}

/// 選べる行だけの並び (Menu::sel はこの位置)
fn menu_choices(st: &State) -> Vec<Row> {
    menu_rows(st).into_iter().filter(|r| !matches!(r, Row::Head(_))).collect()
}

/// その行の選択位置
#[cfg(test)]
fn menu_sel(st: &State, want: impl Fn(&Row) -> bool) -> usize {
    menu_choices(st).iter().position(want).unwrap_or(0)
}

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
        hooks::Status::NoAgent => "↑↓/jk select  F12 close",
        hooks::Status::Missing => "↑↓/jk select  Enter install  F12 close",
        hooks::Status::Outdated(_) => "↑↓/jk select  Enter update  F12 close",
        hooks::Status::Current => "↑↓/jk select  Enter remove  F12 close",
    }
}

/// 行の項目名と設定値。見出しの下の行は 1 段下げる
fn row_text(row: &Row, m: &Menu, st: &State) -> (String, String) {
    let on = |b: bool| if b { "on".to_string() } else { "off".to_string() };
    let sub = |l: &str| format!("  {l}");
    match row {
        Row::Head(h) => (h.to_string(), String::new()),
        Row::Update => match &m.update {
            Some(v) => ("Install update".into(), format!("v{} → v{v}", env!("CARGO_PKG_VERSION"))),
            None => ("Check for updates".into(), String::new()),
        },
        Row::Hook(i) => hook_item(&st.hooks, *i),
        Row::Sidebar => (sub("Sidebar width"), format!("← {} →", st.sidebar)),
        Row::Usage => (sub("Usage gauge"), format!("← {} →", on(st.alerts.usage))),
        Row::Sound => (sub("Sound when done"), format!("← {} →", st.alerts.sound.as_deref().unwrap_or("none"))),
        Row::Popup => (sub("Popup when done"), format!("← {} →", on(st.alerts.notify))),
        Row::Quiet => (sub("Mute if webhook"), format!("← {} →", on(st.alerts.quiet_hooked))),
        Row::Dest(name, url) => match &m.edit {
            Some(e) if e.target.as_ref() == Some(&(name.clone(), url.clone())) => (sub(name), format!("URL: {}", e.line.shown())),
            _ => (sub(name), url.clone()),
        },
        Row::AddDest => match &m.edit {
            Some(DestEdit { target: None, name: None, line }) => (sub("Name"), line.shown()),
            Some(DestEdit { target: None, name: Some(n), line }) => (sub(n), format!("URL: {}", line.shown())),
            _ => (sub("+ Add webhook"), String::new()),
        },
        Row::Close => ("Close (F12)".into(), String::new()),
    }
}

/// Config の各行の表示。項目名と設定値の 2 列で、設定値の始まりを揃える
fn menu_lines(m: &Menu, st: &State) -> Vec<(Row, String)> {
    let w = |s: &str| ratatui::text::Line::from(s).width();
    let rows = menu_rows(st);
    let texts: Vec<(String, String)> = rows.iter().map(|r| row_text(r, m, st)).collect();
    let col = rows.iter().zip(&texts).filter(|(r, _)| !matches!(r, Row::Head(_) | Row::Dest(..))).map(|(_, (l, _))| w(l)).max().unwrap_or(0) + 2;
    rows.into_iter()
        .zip(texts)
        .map(|(r, (l, v))| {
            let text = if v.is_empty() { l } else { format!("{l}{}{v}", " ".repeat(col.saturating_sub(w(&l)).max(2))) };
            (r, text)
        })
        .collect()
}

fn version_parts(v: &str) -> Vec<u32> {
    v.trim_start_matches('v').split('.').map(|p| p.parse().unwrap_or(0)).collect()
}

/// GitHub の最新リリースの版。新しいものがあれば Some。
fn check_update() -> Result<Option<String>, String> {
    let out = std::process::Command::new("/usr/bin/curl")
        .args(["-q", "-fsS", "-m", "10", "-o", "/dev/null", "-w", "%{redirect_url}", RELEASES_URL])
        .output()
        .map_err(|e| format!("cannot run curl: {e}"))?;
    if !out.status.success() {
        return Err(format!("request failed: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    let url = String::from_utf8_lossy(&out.stdout);
    let tag = url.rsplit_once("/tag/").map(|x| x.1.trim()).ok_or_else(|| format!("cannot read the version: {url}"))?;
    let latest = tag.trim_start_matches('v').to_string();
    Ok((version_parts(&latest) > version_parts(env!("CARGO_PKG_VERSION"))).then_some(latest))
}

/// .1 は描画スレッドを、.2 は状態や画面の変化を待つスレッド (wait / auto_yes_loop) を起こす
type Shared = Arc<(Mutex<State>, Condvar, Condvar)>;

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
        log_event(&self.sock, "event", &v);
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
    /// API の tab は番号でも名前でもよい。数字だけの文字列は番号として探す (数字だけの名前は付けられない)
    fn find(&self, v: &Value) -> Result<usize, String> {
        let id = match v {
            Value::Number(n) => n.as_u64(),
            Value::String(t) if !t.is_empty() && t.chars().all(|c| c.is_ascii_digit()) => t.parse().ok(),
            // 空の名前は、名前の無いタブを指さない
            Value::String(t) if !t.is_empty() => None,
            _ => return Err(invalid("tab (id or name) is required")),
        };
        let i = match id {
            Some(id) => self.tabs.iter().position(|t| t.id as u64 == id),
            None => self.tabs.iter().position(|t| t.name.as_deref() == v.as_str()),
        };
        i.ok_or_else(|| format!("no_such_tab: no such tab: {v}"))
    }
    fn check_name(&self, name: &str, except: Option<u32>) -> Result<(), String> {
        validate_name(name, self.tabs.iter().filter(|t| Some(t.id) != except).filter_map(|t| t.name.as_deref()))
    }
    /// subscribe に流し、完了 (state: done)・選択肢・自動 Yes は全体の webhook と、タブの owner の webhook にも送る
    fn notify(&mut self, v: Value) {
        let owner = v["tab"]["owner"].as_str().map(str::to_string);
        if v["event"] == "choices" || v["event"] == "auto_yes" || v["state"] == "done" {
            post_webhooks(&self.sock, &self.webhooks, &v);
            if let Some(Value::Array(list)) = owner.and_then(|o| self.owner_webhooks.get(&o)) {
                post_webhooks(&self.sock, list, &v);
            }
        }
        self.emit(v);
    }
}

/// タブの状態か画面が変わったことを、待っているスレッドへ知らせる
fn changed(s: &Shared, st: &mut State) {
    st.changes += 1;
    s.2.notify_all();
}

/// 変化があるまで待つ (長くても limit)。知らせを取りこぼしても limit で見直す
fn wait_change<'a>(s: &'a Shared, st: MutexGuard<'a, State>, limit: Duration) -> MutexGuard<'a, State> {
    s.2.wait_timeout(st, limit).unwrap_or_else(|e| e.into_inner()).0
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
    let l = UnixListener::bind(sock)?;
    // 置き場所 (~/.mdeck) は本人だけのフォルダだが、MDECK_SOCK で別の場所にしたときも本人以外がつなげないようにする
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(sock, std::fs::Permissions::from_mode(0o600))?;
    Ok(l)
}

/// 起動し直し (restart) で前のプロセスから引き継いだ待ち受けソケットの番号
const LISTEN_FD_ENV: &str = "MDECK_LISTEN_FD";

/// 引き継いだソケットがあればそれを使い、無ければ作る。
/// 起動し直す間もソケットを閉じないので、その隙に MCP や画面が別のサーバーを起動することが無い
fn listen(sock: &str) -> std::io::Result<UnixListener> {
    use std::os::fd::FromRawFd;
    if let Some(fd) = std::env::var(LISTEN_FD_ENV).ok().and_then(|v| v.parse::<i32>().ok()) {
        unsafe {
            std::env::remove_var(LISTEN_FD_ENV);
            libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC);
            return Ok(UnixListener::from_raw_fd(fd));
        }
    }
    bind_sock(sock)
}

pub fn run(sock: &str) -> std::io::Result<()> {
    let listener = listen(sock)?;
    // 画面側で起動したサーバーは stderr が捨てられるので、panic は events.log に残す
    let s = sock.to_string();
    std::panic::set_hook(Box::new(move |p| log_event(&s, "panic", &json!(p.to_string()))));
    log_event(sock, "server_start", &json!({"version": env!("CARGO_PKG_VERSION"), "exe": std::env::current_exe().ok(), "pid": std::process::id()}));
    // hook.log はフックのシェルも直に足すので、起動時にも大きさを見る
    rotate_log(&std::path::Path::new(sock).with_file_name("hook.log"));
    {
        use std::os::fd::AsRawFd;
        LISTEN_FD.store(listener.as_raw_fd(), std::sync::atomic::Ordering::Relaxed);
    }
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
        Condvar::new(),
    ));
    restore(&shared);
    if lock(&shared).tabs.is_empty() {
        new_tab(&shared, None, None, None, true, None, false).map_err(std::io::Error::other)?;
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

fn new_tab(s: &Shared, cwd: Option<&str>, run: Option<&str>, name: Option<String>, focus: bool, owner: Option<String>, auto_yes: bool) -> Result<u32, String> {
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
        session_pgrp: None,
        args: None,
        starting: false,
        owner,
        last_used: now_secs(),
        requests: Vec::new(),
        auto_yes,
        unread: false,
        auto_wait: None,
        auto_pressed: None,
        auto_skipped: None,
        fg: std::cell::Cell::new((None, None)),
    });
    if focus || st.tabs.len() == 1 {
        st.active = st.tabs.len() - 1;
    }
    let j = st.tab_json(st.tabs.len() - 1);
    st.emit(json!({"event": "tab_opened", "tab": j}));
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
            let shown = st.tabs.get(st.active).is_some_and(|t| t.id == id);
            let Some(t) = st.tab(id) else { break };
            let title = t.title.clone();
            t.parser.advance(&mut t.term, &buf[..n]);
            while let Ok(e) = t.events.try_recv() {
                handle_term_event(t, e, cols, rows);
            }
            let redraw = shown || t.title != title;
            changed(&s2, &mut st);
            // 描き直すのは表示中のタブの出力と、サイドバーに出るタイトルが変わったときだけ (裏のタブのスピナーで描き直さない)
            if redraw {
                mark(&s2, &mut st);
            }
        }
        let mut st = lock(&s2);
        changed(&s2, &mut st);
        if let Some(i) = st.tabs.iter().position(|t| t.id == id) {
            let j = st.tab_json(i);
            let mut t = st.tabs.remove(i);
            let _ = t.child.try_wait();
            if st.active > i || st.active >= st.tabs.len() {
                st.active = st.active.saturating_sub(1);
            }
            st.emit(json!({"event": "tab_closed", "tab": j}));
            mark(&s2, &mut st);
        }
    });
    Ok(id)
}

/// タブのタイトル。状態記号を落とし、制御文字 (エスケープ列) は外側の端末へそのまま出ないよう落とす
fn clean_title(s: &str) -> String {
    strip_glyph(s).chars().filter(|c| !c.is_control()).collect()
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
        Event::Title(s) => t.title = clean_title(&s),
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
        log_event(&st.sock, "config", &json!({"sidebar": w}));
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

mod api;
mod input;
mod persist;
mod screen;
mod ui;
mod webhook;
#[cfg(test)]
mod tests;

use api::*;
use input::*;
use persist::*;
use screen::*;
use ui::*;
use webhook::*;
pub(crate) use webhook::append_log;
