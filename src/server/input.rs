use super::*;

pub(super) fn attach(s: Shared, mut rd: BufReader<UnixStream>, w: UnixStream, req: &Value) -> std::io::Result<()> {
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
pub(super) fn sidebar_input(s: &Shared, rest: &[u8], fwd: &mut Vec<u8>) -> (usize, bool) {
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
        // c で Config、e で Exit
        b'c' => return (used, activate(s, st, CUR_CONFIG, fwd)),
        b'e' => return (used, activate(s, st, CUR_EXIT, fwd)),
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
pub(super) fn activate(s: &Shared, mut st: MutexGuard<'_, State>, row: usize, fwd: &mut Vec<u8>) -> bool {
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
            let _ = new_tab(s, None, None, None, true, None, false);
            return false;
        }
    }
    mark(s, &mut st);
    false
}

#[derive(Default)]
pub(super) struct Input {
    /// 端末の中をドラッグして文字を選んでいる
    selecting: bool,
    /// サイドバーの境界をドラッグしている
    resizing: bool,
}

impl Input {
    /// returns true when the client asked to detach.
    pub(super) fn feed(&mut self, s: &Shared, data: &[u8]) -> bool {
        let mut fwd = Vec::new();
        let mut i = 0;
        while i < data.len() {
            // マウスの報告 (SGR) は選択とホイールに使い、中へは流さない。旧形式 (ESC [ M + 3 バイト) は捨てる
            if data[i..].starts_with(b"\x1b[<")
                && let Some(end) = data[i..].iter().position(|c| *c == b'M' || *c == b'm')
            {
                flush(s, &mut fwd);
                if self.mouse(s, &data[i + 3..i + end], data[i + end] == b'M') {
                    return true; // Exit を押した
                }
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

    /// マウス。サイドバーとポップアップは乗ると反応し、左クリックで選ぶ・実行、右クリックでタブのメニュー (値の行は 1 つ戻す)。
    /// 端末の中は左ドラッグで文字を選んで離すとクリップボードへ、ホイールでスクロール。戻り値は Exit を押したか
    fn mouse(&mut self, s: &Shared, seq: &[u8], press: bool) -> bool {
        let p: Vec<i32> = std::str::from_utf8(seq).unwrap_or("").split(';').filter_map(|x| x.parse().ok()).collect();
        let [btn, x, y] = p[..] else { return false };
        let (x, y) = (x - 1, y - 1);
        let (motion, button, wheel) = (btn & 32 != 0, btn & 3, btn & 64 != 0);
        let mut st = lock(s);
        let at = ratatui::layout::Position::new(x.max(0) as u16, y.max(0) as u16);
        let hit = st.hits.borrow().iter().find(|(r, _)| r.contains(at)).map(|(_, h)| *h);
        let popup = st.menu.is_some() || st.session.is_some();
        // サイドバーの境界のドラッグ
        if self.resizing {
            if motion {
                set_sidebar(s, &mut st, x + 1);
            } else if !press {
                self.resizing = false;
            }
            return false;
        }
        // ボタンを押していない移動 (ホバー)。サイドバーの行を明るくし、ポップアップの項目は選ぶ
        if motion && button == 3 {
            let hover = match hit {
                Some(Hit::Side(r)) => Some(r),
                _ => None,
            };
            let mut changed = std::mem::replace(&mut st.hover, hover) != hover;
            match hit {
                Some(Hit::MenuRow(k) | Hit::MenuButton(k, _)) => {
                    let del = matches!(hit, Some(Hit::MenuButton(_, true)));
                    if let Some(m) = st.menu.as_mut().filter(|m| m.edit.is_none()) {
                        if m.sel != k {
                            (m.sel, m.del, m.confirm) = (k, false, false);
                            changed = true;
                        }
                        if matches!(hit, Some(Hit::MenuButton(..))) && m.del != del {
                            m.del = del;
                            changed = true;
                        }
                    }
                }
                Some(Hit::TabRow(i)) => {
                    if let Some(p) = st.session.as_mut().filter(|p| p.name.is_none() && p.sel != i) {
                        p.sel = i;
                        changed = true;
                    }
                }
                _ => {}
            }
            if changed {
                mark(s, &mut st);
            }
            return false;
        }
        // ポップアップの上のホイールは項目を上下に動かす
        if wheel && popup {
            drop(st);
            let key: &[u8] = if btn & 1 == 0 { b"k" } else { b"j" };
            if session_input(s, key) == 0 {
                menu_input(s, key);
            }
            return false;
        }
        if press && !motion && !wheel {
            // 値を選ぶ行 (←/→) は、左クリックで次、右クリックで前の値。それ以外は左クリックで Enter
            let key = |value: bool| -> Option<&'static [u8]> {
                match (value, button) {
                    (true, 0) => Some(b"\x1b[C"),
                    (true, 2) => Some(b"\x1b[D"),
                    (false, 0) => Some(b"\r"),
                    _ => None,
                }
            };
            match (button, hit) {
                (0, Some(Hit::Side(row))) => {
                    st.menu = None;
                    st.session = None;
                    let plus = CUR_TABS + st.tabs.len();
                    // タブは選んで作業に戻る
                    if (CUR_TABS..plus).contains(&row) {
                        st.active = row - CUR_TABS;
                        st.prefix = false;
                        mark(s, &mut st);
                        return false;
                    }
                    return activate(s, st, row, &mut Vec::new());
                }
                // タブの右クリックはそのタブのメニュー
                (2, Some(Hit::Side(row))) if (CUR_TABS..CUR_TABS + st.tabs.len()).contains(&row) => {
                    st.menu = None;
                    st.active = row - CUR_TABS;
                    open_session(&mut st, row - CUR_TABS);
                    mark(s, &mut st);
                    return false;
                }
                (0, Some(Hit::Border)) => {
                    self.resizing = true;
                    return false;
                }
                (_, Some(Hit::MenuRow(k))) => {
                    let Some(m) = st.menu.as_mut().filter(|m| m.edit.is_none()) else { return false };
                    (m.sel, m.del, m.confirm) = (k, false, false);
                    let value = matches!(menu_choices(&st).get(k), Some(Row::Sidebar | Row::Usage | Row::Sound | Row::Popup | Row::Quiet));
                    drop(st);
                    if let Some(k) = key(value) {
                        menu_input(s, k);
                    }
                    return false;
                }
                (0, Some(Hit::MenuButton(k, del))) => {
                    let Some(m) = st.menu.as_mut().filter(|m| m.edit.is_none()) else { return false };
                    // 確認中でなければ、削除のボタンは確認を出す (確認中の「削除する」で消す)
                    if m.sel != k {
                        m.confirm = false;
                    }
                    (m.sel, m.del) = (k, del);
                    drop(st);
                    menu_input(s, b"\r");
                    return false;
                }
                (_, Some(Hit::TabRow(i))) => {
                    let Some(p) = st.session.as_mut().filter(|p| p.name.is_none()) else { return false };
                    p.sel = i;
                    let value = matches!(session_items(&st, st.session.as_ref().unwrap()).get(i).map(|x| &x.0), Some(SessionItem::Dest | SessionItem::AutoYes));
                    drop(st);
                    if let Some(k) = key(value) {
                        session_input(s, k);
                    }
                    return false;
                }
                // ポップアップの外を押すと閉じる
                (0, None) if popup => {
                    st.menu = None;
                    st.session = None;
                    mark(s, &mut st);
                    return false;
                }
                _ => {}
            }
        }
        if popup {
            return false;
        }
        let side = st.sidebar as i32;
        let a = st.active;
        let Some(t) = st.tabs.get_mut(a) else { return false };
        if wheel {
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
            return false;
        }
        if button != 0 {
            return false;
        }
        // 端末の外 (サイドバー) へはみ出しても、端末の端の列で選ぶ
        let g = t.term.grid();
        let col = (x - side).clamp(0, g.columns() as i32 - 1) as usize;
        let pt = Point::new(Line(y.clamp(0, g.screen_lines() as i32 - 1) - g.display_offset() as i32), Column(col));
        match (self.selecting, motion, press) {
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
        false
    }
}

/// 中のアプリがマウス報告を求めていれば、その形式で送るバイト列 (col, row は 1 始まり)。
/// claude などは自分でスクロールするので、矢印に変えると入力履歴が動いてしまう。
pub(super) fn mouse_report(mode: TermMode, btn: i32, col: i32, row: i32) -> Option<Vec<u8>> {
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
pub(super) fn copy(st: &mut State, text: &str) {
    if let Ok(mut c) = std::process::Command::new("pbcopy").stdin(std::process::Stdio::piped()).spawn() {
        if let Some(mut i) = c.stdin.take() {
            let _ = i.write_all(text.as_bytes());
        }
        std::thread::spawn(move || c.wait());
    }
    let osc = format!("\x1b]52;c;{}\x07", base64(text.as_bytes())).into_bytes();
    st.clients.retain(|(_, c)| c.send(osc.clone()).is_ok());
}

pub(super) fn base64(data: &[u8]) -> String {
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

pub(super) fn flush(s: &Shared, fwd: &mut Vec<u8>) {
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
