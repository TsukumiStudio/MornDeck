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
    agent: Option<String>,
    state: Option<&'static str>,
    answer: Option<String>,
    question: Value,
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
}

struct Menu {
    sel: usize,
    notice: Option<String>,
}

const MENU_ITEMS: [&str; 2] = ["claude / codex のフックを導入・更新", "閉じる"];

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
        json!({"id": t.id, "title": t.title, "agent": t.agent, "state": t.state, "active": i == self.active})
    }
}

fn mark(s: &Shared, st: &mut State) {
    st.dirty = true;
    s.1.notify_one();
}

pub fn run(sock: &str) -> std::io::Result<()> {
    let _ = std::fs::remove_file(sock);
    let listener = UnixListener::bind(sock)?;
    let shared: Shared = Arc::new((
        Mutex::new(State {
            cols: 80,
            rows: 24,
            next_id: 1,
            sock: sock.into(),
            hooks: hooks::status(),
            sidebar: 24,
            ..Default::default()
        }),
        Condvar::new(),
    ));
    new_tab(&shared, None, None)?;
    let r = shared.clone();
    std::thread::spawn(move || render_loop(r));
    for conn in listener.incoming().flatten() {
        let s = shared.clone();
        std::thread::spawn(move || {
            let _ = handle_conn(s, conn);
        });
    }
    Ok(())
}

fn new_tab(s: &Shared, cwd: Option<&str>, run: Option<&str>) -> std::io::Result<u32> {
    let mut st = lock(s);
    let (cols, rows) = st.inner_size();
    let id = st.next_id;
    st.next_id += 1;
    let pair = native_pty_system()
        .openpty(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })
        .map_err(std::io::Error::other)?;
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
    let child = pair.slave.spawn_command(cmd).map_err(std::io::Error::other)?;
    drop(pair.slave);
    let mut reader = pair.master.try_clone_reader().map_err(std::io::Error::other)?;
    let mut writer = pair.master.take_writer().map_err(std::io::Error::other)?;
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
        title: format!("tab {id}"),
        agent: None,
        state: None,
        answer: None,
        question: Value::Null,
    });
    st.active = st.tabs.len() - 1;
    let j = st.tab_json(st.active);
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
        Event::ResetTitle => t.title = format!("tab {}", t.id),
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
            let mut t = Terminal::with_options(
                CrosstermBackend::new(Vec::new()),
                TerminalOptions { viewport: Viewport::Fixed(Rect::new(0, 0, size.0, size.1)) },
            )
            .expect("vec backend");
            let _ = t.clear();
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
        buf.set_stringn(0, y, format!(" {} ", i + 1), 3, base);
        let (icon, ic) = match t.agent.as_deref().or_else(|| fg_agent(t.master.process_group_leader())) {
            Some("claude") => ("✻", Color::Rgb(217, 119, 87)),
            Some("codex") => ("❖", Color::Rgb(16, 163, 127)),
            _ => (" ", Color::Reset),
        };
        buf.set_stringn(3, y, icon, 1, base.fg(ic));
        buf.set_stringn(4, y, g, 1, base.fg(gc));
        let name = if label.is_empty() { t.title.clone() } else { format!("{label} {}", t.title) };
        buf.set_stringn(6, y, name, side.width.saturating_sub(7) as usize, base);
    }
    let h = side.height;
    buf.set_stringn(0, h.saturating_sub(1), "F12 で操作一覧", side.width as usize, bg.fg(Color::Indexed(250)));
    let warn = if st.hooks.iter().any(|(_, s)| *s == hooks::Status::Outdated) {
        Some("! フックが古い")
    } else if st.hooks.iter().any(|(_, s)| *s == hooks::Status::Missing) {
        Some("! フック未導入")
    } else {
        None
    };
    if let Some(w) = warn {
        let warn = bg.fg(Color::Yellow);
        buf.set_stringn(0, h.saturating_sub(3), w, side.width as usize, warn);
        buf.set_stringn(0, h.saturating_sub(2), "  F12 → m で開く", side.width as usize, warn);
    }

    match st.tabs.get(st.active) {
        Some(t) => {
            if let Some(cur) = draw_term(buf, main, &t.term) {
                f.set_cursor_position(cur);
            }
        }
        None => {
            buf.set_string(main.x + 1, 0, "タブがない。F12 → c で新規", Style::default());
        }
    }
    if let Some(m) = &st.menu {
        draw_menu(f, main, m, &st.hooks);
    }
    if st.prefix {
        let bar = Rect::new(0, area.height.saturating_sub(1), area.width, 1);
        let on = Style::default().bg(Color::Yellow).fg(Color::Black);
        let keys = " F12 › c)新規タブ  x)タブを閉じる  n)次  p)前  1-9)番号へ  m)メニュー  d)離脱  F12)F12を送る";
        f.buffer_mut().set_style(bar, on);
        f.buffer_mut().set_stringn(0, bar.y, keys, area.width as usize, on);
    }
}

fn draw_menu(f: &mut Frame, area: Rect, m: &Menu, hooks: &[(&str, hooks::Status)]) {
    use ratatui::text::Line;
    use ratatui::widgets::{Block, Clear, Paragraph};
    let status: Vec<String> = hooks.iter().map(|(a, s)| format!("{a}: {}", s.label())).collect();
    let mut lines = vec![Line::from(format!("フック v{}  {}", hooks::VERSION, status.join("  "))), Line::from("")];
    for (i, item) in MENU_ITEMS.iter().enumerate() {
        let style = if i == m.sel { Style::default().add_modifier(Modifier::REVERSED) } else { Style::default() };
        lines.push(Line::styled(format!(" {item} "), style));
    }
    lines.push(Line::from(""));
    lines.push(Line::styled(
        m.notice.clone().unwrap_or_else(|| "j/k 選択  Enter 実行  Esc 閉じる".into()),
        Style::default().fg(Color::DarkGray),
    ));
    let (w, h) = (48.min(area.width), (lines.len() as u16 + 2).min(area.height));
    let r = Rect::new(area.x + (area.width - w) / 2, area.y + (area.height - h) / 2, w, h);
    f.render_widget(Clear, r);
    f.render_widget(Paragraph::new(lines).block(Block::bordered().title(" メニュー ")), r);
}

fn menu_input(s: &Shared, rest: &[u8]) -> usize {
    let starts = |a: &[u8], b: &[u8]| rest.starts_with(a) || rest.starts_with(b);
    let (up, down) = (starts(b"\x1b[A", b"\x1bOA"), starts(b"\x1b[B", b"\x1bOB"));
    let mut st = lock(s);
    let Some(m) = st.menu.as_mut() else { return 0 };
    match rest[0] {
        _ if up => m.sel = m.sel.saturating_sub(1),
        _ if down => m.sel = (m.sel + 1).min(MENU_ITEMS.len() - 1),
        b'k' => m.sel = m.sel.saturating_sub(1),
        b'j' => m.sel = (m.sel + 1).min(MENU_ITEMS.len() - 1),
        b'\r' if m.sel == 0 => {
            m.notice = Some(match hooks::install() {
                Ok(()) => "導入した。起動中の claude / codex は再起動で反映".into(),
                Err(e) => format!("失敗: {e}"),
            });
            st.hooks = hooks::status();
        }
        b'\r' | b'q' | 0x1b => st.menu = None,
        _ => {}
    }
    mark(s, &mut st);
    if up || down { 3 } else { 1 }
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
    let req: Value = serde_json::from_str(&line).unwrap_or(Value::Null);
    let mut w = conn;
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

fn api(s: &Shared, req: &Value) -> Result<Value, String> {
    match req["cmd"].as_str().unwrap_or("") {
        "list" => {
            let st = lock(s);
            let tabs: Vec<Value> = (0..st.tabs.len()).map(|i| st.tab_json(i)).collect();
            Ok(json!({"ok": true, "tabs": tabs}))
        }
        "new" => {
            let id = new_tab(s, req["cwd"].as_str(), req["run"].as_str()).map_err(|e| e.to_string())?;
            Ok(json!({"ok": true, "id": id}))
        }
        "send" => {
            let id = tab_id(req)?;
            let text = req["text"].as_str().unwrap_or("");
            let raw = req["raw"].as_bool().unwrap_or(false);
            {
                let mut st = lock(s);
                let t = st.tab(id).ok_or("no such tab")?;
                let data = if !raw && t.term.mode().contains(TermMode::BRACKETED_PASTE) {
                    format!("\x1b[200~{text}\x1b[201~")
                } else {
                    text.to_string()
                };
                t.writer.write_all(data.as_bytes()).map_err(|e| e.to_string())?;
            }
            if req["enter"].as_bool().unwrap_or(!raw) {
                // 本文と同じ書き込みで送ると TUI がペーストの一部として扱い送信しない
                std::thread::sleep(Duration::from_millis(150));
                let mut st = lock(s);
                let t = st.tab(id).ok_or("no such tab")?;
                t.writer.write_all(b"\r").map_err(|e| e.to_string())?;
            }
            Ok(json!({"ok": true}))
        }
        "read" => {
            let id = tab_id(req)?;
            let mut st = lock(s);
            let t = st.tab(id).ok_or("no such tab")?;
            Ok(json!({"ok": true, "lines": screen_lines(&t.term)}))
        }
        "answer" => {
            let id = tab_id(req)?;
            let mut st = lock(s);
            let t = st.tab(id).ok_or("no such tab")?;
            Ok(json!({"ok": true, "agent": t.agent, "state": t.state, "text": t.answer}))
        }
        "choices" => {
            let id = tab_id(req)?;
            let mut st = lock(s);
            let t = st.tab(id).ok_or("no such tab")?;
            Ok(json!({"ok": true, "state": t.state, "question": t.question, "options": screen_options(&t.term)}))
        }
        "select" => {
            let id = tab_id(req)?;
            let mut st = lock(s);
            st.active = st.tabs.iter().position(|t| t.id == id).ok_or("no such tab")?;
            mark(s, &mut st);
            Ok(json!({"ok": true}))
        }
        "close" => {
            let id = tab_id(req)?;
            let mut st = lock(s);
            st.tab(id).ok_or("no such tab")?.child.kill().map_err(|e| e.to_string())?;
            Ok(json!({"ok": true}))
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
            if let Some(m) = req["message"].as_str() {
                t.answer = Some(m.to_string());
            }
            t.question = if state == Some("waiting") {
                json!({"tool": req["tool"], "input": req["input"]})
            } else {
                Value::Null
            };
            if t.state == state && t.agent.as_deref() == Some(&agent) {
                return Ok(json!({"ok": true}));
            }
            t.state = state;
            t.agent = state.map(|_| agent);
            let mut ev = json!({"event": "state", "tab": id, "agent": t.agent, "state": t.state});
            if state == Some("done") {
                ev["answer"] = json!(t.answer);
            }
            st.emit(ev);
            mark(s, &mut st);
            if state == Some("waiting") {
                let s = s.clone();
                std::thread::spawn(move || {
                    // フックは選択肢の描画より先に届くので、画面が出揃うのを待ってから読む
                    std::thread::sleep(Duration::from_millis(500));
                    let mut st = lock(&s);
                    let Some(t) = st.tab(id).filter(|t| t.state == Some("waiting")) else { return };
                    let ev = json!({"event": "choices", "tab": id, "question": t.question, "options": screen_options(&t.term)});
                    st.emit(ev);
                });
            }
            Ok(json!({"ok": true}))
        }
        c => Err(format!("unknown cmd: {c}")),
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
        let selected = t.starts_with(['❯', '›', '>', '▶']);
        let t = t.trim_start_matches(['❯', '›', '>', '▶']).trim_start();
        let digits: String = t.chars().take_while(char::is_ascii_digit).collect();
        let Some(label) = t[digits.len()..].strip_prefix(". ") else { continue };
        let Ok(n) = digits.parse::<u32>() else { continue };
        if n == 1 {
            out.clear();
        }
        let label = label.trim_end_matches([' ', '│', '┃', '|']).trim();
        out.push(json!({"n": n, "label": label, "selected": selected}));
    }
    out
}

#[cfg(test)]
mod tests {
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
            if !self.prefix {
                let used = menu_input(s, &data[i..]);
                if used > 0 {
                    i += used;
                    continue;
                }
            }
            if self.prefix && data[i..].starts_with(PREFIX) {
                self.set_prefix(s, false);
                fwd.extend_from_slice(PREFIX);
                i += PREFIX.len();
                continue;
            }
            if self.prefix {
                self.set_prefix(s, false);
                i += 1;
                match b {
                    b'c' => {
                        flush(s, &mut fwd);
                        let _ = new_tab(s, None, None);
                    }
                    b'n' | b'p' => {
                        let mut st = lock(s);
                        let n = st.tabs.len().max(1);
                        st.active = if b == b'n' { (st.active + 1) % n } else { (st.active + n - 1) % n };
                        mark(s, &mut st);
                    }
                    b'1'..=b'9' => {
                        let mut st = lock(s);
                        let k = (b - b'1') as usize;
                        if k < st.tabs.len() {
                            st.active = k;
                            mark(s, &mut st);
                        }
                    }
                    b'x' => {
                        let mut st = lock(s);
                        let a = st.active;
                        if let Some(t) = st.tabs.get_mut(a) {
                            let _ = t.child.kill();
                        }
                    }
                    b'd' => {
                        flush(s, &mut fwd);
                        return true;
                    }
                    b',' | b'.' => {
                        let mut st = lock(s);
                        let w = st.sidebar as i32 + if b == b'.' { 4 } else { -4 };
                        set_sidebar(s, &mut st, w);
                    }
                    b'm' => {
                        let mut st = lock(s);
                        st.hooks = hooks::status();
                        st.menu = Some(Menu { sel: 0, notice: None });
                        mark(s, &mut st);
                    }
                    _ => {}
                }
                continue;
            }
            if data[i..].starts_with(PREFIX) {
                self.set_prefix(s, true);
                i += PREFIX.len();
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
        let changed = t.term.selection.take().is_some() || t.term.grid().display_offset() != 0;
        t.term.scroll_display(Scroll::Bottom);
        if changed {
            mark(s, &mut st);
        }
    }
    fwd.clear();
}
