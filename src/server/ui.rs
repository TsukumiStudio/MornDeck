use super::*;

// ---------- rendering ----------

pub(super) fn render_loop(s: Shared) {
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
        // 画面がつながっていなければ描かない (つないだときに全面を描く)
        if st.clients.is_empty() {
            st.full_redraw = true;
            continue;
        }
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
            // マウスはボタンを押していない移動 (ホバー) まで受け取る (1003)。どの版の画面がつないでもここで有効にする
            out.extend_from_slice(b"\x1b[?1003h\x1b[?1006h\x1b[0m\x1b[H\x1b[2J");
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
        if st.prefix || st.menu.as_ref().is_some_and(|m| m.edit.is_none()) || st.session.as_ref().is_some_and(|p| p.name.is_none()) {
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

pub(super) fn state_glyph(state: Option<&str>) -> (&'static str, Color, &'static str) {
    match state {
        Some("working") => ("●", Color::Yellow, "考え中"),
        Some("waiting") => ("◆", Color::LightRed, "入力待ち"),
        Some("done") => ("✓", Color::LightGreen, "完了"),
        Some(_) => ("○", Color::DarkGray, "待機"),
        None => (" ", Color::Reset, ""),
    }
}

pub(super) fn draw(f: &mut Frame, st: &State) {
    let area = f.area();
    st.hits.borrow_mut().clear();
    let side = Rect::new(0, 0, st.sidebar.min(area.width), area.height);
    let main = Rect::new(side.width, 0, area.width - side.width, area.height);
    let buf = f.buffer_mut();
    let bg = Style::default().bg(Color::Indexed(235));
    buf.set_style(side, bg);
    let cursor = |row: usize, base: Style| {
        if st.prefix && st.cursor == row {
            base.patch(cursor_style())
        } else if st.hover == Some(row) {
            base.bg(Color::Indexed(239))
        } else {
            base
        }
    };
    let side_hit = |y: u16, row: usize| st.hits.borrow_mut().push((Rect::new(0, y, side.width.saturating_sub(1), 1), Hit::Side(row)));
    st.hits.borrow_mut().push((Rect::new(side.width.saturating_sub(1), 0, 1, side.height), Hit::Border));
    let (row_config, row_exit, tabs_top) = rows(side.width);
    let ver = format!("v{}", env!("CARGO_PKG_VERSION"));
    let (vx, vy) = if row_config == 1 { (10, 0) } else { (1, 1) };
    buf.set_stringn(1, 0, "MornDeck", side.width.saturating_sub(1) as usize, bg.add_modifier(Modifier::BOLD));
    buf.set_stringn(vx, vy, ver, side.width.saturating_sub(vx + 1) as usize, bg.fg(Color::Indexed(245)));
    let h = side.height;
    let plus = CUR_TABS + st.tabs.len();
    let (shown, plus_y) = tab_rows(st.tabs.len(), h, side.width);
    for (row, y, icon, label) in [(CUR_CONFIG, row_config, "⚙", "Config (c)"), (CUR_EXIT, row_exit, "✕", "Exit (e)"), (plus, plus_y, "+", "")] {
        let style = cursor(row, bg);
        side_hit(y, row);
        buf.set_style(Rect::new(0, y, side.width, 1), style);
        buf.set_stringn(1, y, icon, 2, style);
        buf.set_stringn(LABEL_X, y, label, side.width.saturating_sub(LABEL_X + 1) as usize, style);
    }
    buf.set_stringn(0, tabs_top - 1, "─".repeat(side.width as usize), side.width as usize, bg.fg(Color::Indexed(240)));
    for (i, t) in st.tabs.iter().enumerate().take(shown) {
        let y = tabs_top + i as u16;
        side_hit(y, CUR_TABS + i);
        let (g, gc, label) = state_glyph(t.state);
        let base = if i == st.active { bg.bg(Color::Indexed(24)).add_modifier(Modifier::BOLD) } else { bg };
        let base = cursor(CUR_TABS + i, base);
        buf.set_style(Rect::new(0, y, side.width, 1), base);
        let (icon, ic) = match t.agent.as_deref().or_else(|| cached_fg_agent(t)) {
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
    let panel = st.menu.is_some() || (st.prefix && (st.cursor < CUR_TABS || st.cursor == plus));
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
                draw_menu(f, main, &Menu { notice: Some("Enter / → / c で開く".into()), ..Menu::new(usize::MAX) }, st);
            }
            CUR_EXIT => {
                draw_box(f, main, " Exit ", vec![
                Line::from("MornDeck の画面を閉じます。"),
                Line::from("タブとサーバーは動いたまま残るため、mdeck で再開できます。"),
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
        let text = if st.prefix { " ↑↓/jk 選択  Enter 決定  → 詳細  c Config  e Exit  F12 閉じる" } else if st.menu.is_some() { " ↑↓/jk 選択  Enter 決定  F12 閉じる" } else { " ↑↓/jk 選択  Enter 決定  ← 戻る  F12 閉じる" };
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
pub(super) fn draw_usage(buf: &mut ratatui::buffer::Buffer, side: Rect, st: &State, now: u64) {
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
pub(super) fn draw_box(f: &mut Frame, area: Rect, title: &str, lines: Vec<ratatui::text::Line>) -> Rect {
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
pub(super) fn cursor_style() -> Style {
    Style::default().bg(Color::Rgb(255, 150, 40)).fg(Color::Black)
}

/// ポップアップの中の余白 (左右・上下)
pub(super) const PAD_X: u16 = 4;
pub(super) const PAD_Y: u16 = 2;
/// 枠 2 + 左右の余白
pub(super) const BOX_PAD: u16 = 2 + 2 * PAD_X;

/// ポップアップの 1 行に入る幅
pub(super) fn box_inner(area: Rect) -> usize {
    area.width.saturating_sub(BOX_PAD) as usize
}

/// 表示幅 max に収める。はみ出すなら真ん中を … にする (URL の頭と末尾が見えるように)。
/// tail なら頭を … にして末尾を見せる (入力中の行)
pub(super) fn fit(text: &str, max: usize, tail: bool) -> String {
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

pub(super) fn session_items(st: &State, p: &SessionPop) -> Vec<(SessionItem, String)> {
    if p.confirm_close {
        return vec![(SessionItem::KeepOpen, "削除しない".into()), (SessionItem::ConfirmClose, "削除する".into())];
    }
    let Some(t) = st.tabs.iter().find(|t| t.id == p.tab) else { return Vec::new() };
    let name = match &p.name {
        Some(e) => e.shown(),
        None => t.name.clone().unwrap_or_else(|| "(なし)".into()),
    };
    vec![
        (SessionItem::Name, kv("タブ名", &name)),
        (SessionItem::Dest, kv("通知先", &format!("← {} →", t.owner.as_deref().unwrap_or("なし")))),
        (SessionItem::AutoYes, kv("AutoYes", if t.auto_yes { "← オン →" } else { "← オフ →" })),
        (SessionItem::Close, "タブを削除".into()),
        (SessionItem::Done, "閉じる (F12)".into()),
    ]
}

/// タブのメニューの「項目名  値」。値の始まりを揃える (項目名は最長の AutoYes に合わせる)
pub(super) fn kv(label: &str, value: &str) -> String {
    let w = ratatui::text::Line::from(label).width();
    format!("{label}{}{value}", " ".repeat(9 - w.min(7)))
}

/// タブの通知先の候補。なし・登録済みの通知先・今の通知先 (登録が消えていても選べるように)
pub(super) fn dest_choices(st: &State, current: Option<&str>) -> Vec<Option<String>> {
    let mut v: Vec<Option<String>> = std::iter::once(None).chain(st.owner_webhooks.keys().cloned().map(Some)).collect();
    if let Some(c) = current.filter(|c| !st.owner_webhooks.contains_key(*c)) {
        v.push(Some(c.to_string()));
    }
    v
}

/// 戻り値は選んでいる項目の位置。入力中なら入力位置。日本語入力の変換候補はカーソルの位置に出るので、そこへ置く
pub(super) fn draw_session(f: &mut Frame, area: Rect, p: &SessionPop, st: &State) -> (u16, u16) {
    use ratatui::text::Line;
    let items = session_items(st, p);
    let title = st.tabs.iter().find(|t| t.id == p.tab).map(|t| t.name.clone().unwrap_or_else(|| t.title.clone())).unwrap_or_default();
    let red = Style::default().fg(Color::LightRed).add_modifier(Modifier::BOLD);
    let max = box_inner(area);
    // 見出しは削除の確認だけ
    let mut lines = if p.confirm_close {
        vec![Line::styled(fit(&format!("{title} を本当に削除しますか？"), max, false), red), Line::from("")]
    } else {
        Vec::new()
    };
    let (mut cursor_x, mut cursor_y) = (1, 0);
    let mut hit_rows = Vec::new();
    for (i, (item, label)) in items.iter().enumerate() {
        // 「タブを削除」と「閉じる」の前は 1 行空ける
        if matches!(item, SessionItem::Close | SessionItem::Done) {
            lines.push(Line::from(""));
        }
        let selected = i == p.sel;
        let editing = selected && p.name.is_some();
        let label = fit(label, max.saturating_sub(2), editing);
        if selected {
            cursor_y = lines.len() as u16;
            if editing {
                cursor_x = Line::from(format!(" {}", label.split('▏').next().unwrap_or(""))).width() as u16;
            }
        }
        hit_rows.push((lines.len() as u16, i));
        let style = match (item, selected) {
            (SessionItem::ConfirmClose, true) => Style::default().bg(Color::LightRed).fg(Color::Black),
            (SessionItem::ConfirmClose, false) => Style::default().fg(Color::LightRed),
            (_, true) => cursor_style(),
            _ => Style::default(),
        };
        lines.push(Line::styled(format!(" {label} "), style));
    }
    let hint = match (&p.name, items.get(p.sel).map(|x| &x.0)) {
        (Some(_), _) => "Enter 決定  F12 やめる  空で外す",
        (None, Some(SessionItem::Dest)) => "←/→ 通知先を選ぶ  ↑↓/jk 選択",
        (None, Some(SessionItem::AutoYes)) => "←/→ オン・オフ  ↑↓/jk 選択",
        (None, Some(SessionItem::Name)) => "Enter 入力  ↑↓/jk 選択",
        (None, _) if p.confirm_close => "「削除する」を選んで Enter",
        _ => "↑↓/jk 選択  Enter 決定",
    };
    lines.push(Line::from(""));
    lines.push(Line::styled(fit(p.notice.as_deref().unwrap_or(hint), max, false), Style::default().fg(Color::LightGreen)));
    let r = draw_box(f, area, " タブ ", lines);
    let mut hits = st.hits.borrow_mut();
    for (line, i) in hit_rows {
        hits.push((Rect::new(r.x + 1, r.y + 1 + PAD_Y + line, r.width.saturating_sub(2), 1), Hit::TabRow(i)));
    }
    (r.x + 1 + PAD_X + cursor_x, r.y + 1 + PAD_Y + cursor_y)
}

pub(super) fn open_session(st: &mut State, idx: usize) {
    let Some(t) = st.tabs.get(idx) else { return };
    st.session = Some(SessionPop { tab: t.id, sel: 0, name: None, notice: None, confirm_close: false });
    st.prefix = false;
}

pub(super) fn open_menu(st: &mut State) {
    st.hooks = hooks::status();
    st.menu = Some(Menu::new(0));
    st.prefix = false;
}

pub(super) fn session_input(s: &Shared, rest: &[u8]) -> usize {
    let starts = |a: &[u8], b: &[u8]| rest.starts_with(a) || rest.starts_with(b);
    let (up, down) = (starts(b"\x1b[A", b"\x1bOA"), starts(b"\x1b[B", b"\x1bOB"));
    let (left, right) = (starts(b"\x1b[D", b"\x1bOD"), starts(b"\x1b[C", b"\x1bOC"));
    let mut st = lock(s);
    let Some(mut p) = st.session.take() else { return 0 };
    let Some(idx) = st.tabs.iter().position(|t| t.id == p.tab) else {
        mark(s, &mut st);
        return 0; // タブが消えたら閉じて、入力はそのまま流す
    };
    // タブ名の入力中
    if let Some(mut e) = p.name.take() {
        let (used, enter) = e.key(rest);
        if enter {
            let v = e.buf.trim().to_string();
            let r = if v.is_empty() { Ok(()) } else { st.check_name(&v, Some(p.tab)) };
            match r {
                Ok(()) => {
                    st.tabs[idx].name = (!v.is_empty()).then_some(v);
                    log_event(&st.sock, "ui", &json!({"tab": p.tab, "name": st.tabs[idx].name}));
                    p.notice = Some("変更した".into());
                }
                Err(err) => {
                    p.notice = Some(err);
                    p.name = Some(e);
                }
            }
        } else {
            p.name = Some(e);
        }
        st.session = Some(p);
        mark(s, &mut st);
        return used;
    }
    p.notice = None;
    let items = session_items(&st, &p);
    let used = if rest[0] == 0x1b { esc_len(rest) } else { 1 };
    let last = items.len().saturating_sub(1);
    let mut open = true;
    match (rest[0], items.get(p.sel).map(|x| x.0.clone())) {
        _ if up => p.sel = p.sel.saturating_sub(1),
        _ if down => p.sel = (p.sel + 1).min(last),
        (b'k', _) => p.sel = p.sel.saturating_sub(1),
        (b'j', _) => p.sel = (p.sel + 1).min(last),
        // 値は ←/→ で変える。Enter は入力 (タブ名) と操作 (削除・閉じる) だけ
        (_, Some(SessionItem::Dest)) if left || right => {
            let cur = st.tabs[idx].owner.clone();
            let choices = dest_choices(&st, cur.as_deref());
            let at = choices.iter().position(|c| *c == cur).unwrap_or(0) as i64;
            let next = (at + if left { -1 } else { 1 }).rem_euclid(choices.len() as i64) as usize;
            st.tabs[idx].owner = choices[next].clone();
            log_event(&st.sock, "ui", &json!({"tab": p.tab, "owner": st.tabs[idx].owner}));
        }
        (b'\r', Some(SessionItem::Name)) => p.name = Some(LineEdit::new(st.tabs[idx].name.clone().unwrap_or_default())),
        (_, Some(SessionItem::AutoYes)) if left || right => {
            st.tabs[idx].auto_yes = !st.tabs[idx].auto_yes;
            log_event(&st.sock, "ui", &json!({"tab": p.tab, "auto_yes": st.tabs[idx].auto_yes}));
        }
        // 既定は「削除しない」。Enter を続けて押しても消さない
        (b'\r', Some(SessionItem::Close)) => {
            p.confirm_close = true;
            p.sel = 0;
        }
        (b'\r', Some(SessionItem::KeepOpen)) => {
            p.confirm_close = false;
            p.sel = session_items(&st, &p).iter().position(|x| x.0 == SessionItem::Close).unwrap_or(0);
        }
        (b'\r', Some(SessionItem::ConfirmClose)) => {
            log_event(&st.sock, "ui", &json!({"tab": p.tab, "close": true}));
            let _ = st.tabs[idx].child.kill();
            open = false;
        }
        (b'\r', Some(SessionItem::Done)) => open = false,
        _ => {}
    }
    let n = session_items(&st, &p).len();
    p.sel = p.sel.min(n.saturating_sub(1));
    if open {
        st.session = Some(p);
    }
    mark(s, &mut st);
    used
}

/// 矢印などのエスケープ列は丸ごと 1 つとして数える。
pub(super) fn esc_len(rest: &[u8]) -> usize {
    if rest.get(1) == Some(&b'[') || rest.get(1) == Some(&b'O') {
        rest.iter().skip(2).position(|c| c.is_ascii_alphabetic() || *c == b'~').map_or(rest.len(), |p| p + 3)
    } else {
        1
    }
}

pub(super) fn draw_menu(f: &mut Frame, area: Rect, m: &Menu, st: &State) -> (u16, u16) {
    use ratatui::text::{Line, Span};
    let mut lines = Vec::new();
    let (mut k, mut cursor_x, mut cursor_y) = (0, 1, 0);
    let mut selected = None;
    // マウスで押せる行: (行の位置, 選べる行の位置, 通知先の行ならボタンの始まりの列)
    let mut hit_rows: Vec<(u16, usize, Option<u16>)> = Vec::new();
    let max = box_inner(area).saturating_sub(2);
    // 通知先の行の幅。箱は周りの余白の分だけ狭くなることがあるので控えめにとる
    let dest_w = max.saturating_sub(4);
    // 後ろのボタンの幅 (「 削除しない 」「 削除する 」が一番長い)
    const BUTTONS: usize = 24;
    for (row, text) in menu_lines(m, st) {
        if let Row::Head(_) = row {
            // 見出しの前は 1 行空ける
            lines.push(Line::from(""));
            lines.push(Line::styled(format!(" {text} "), Style::default().add_modifier(Modifier::BOLD)));
            continue;
        }
        if row == Row::Close {
            lines.push(Line::from(""));
        }
        let is_sel = k == m.sel;
        let editing = is_sel && m.edit.is_some();
        // 通知先の行は後ろに「編集」「削除」(確認中は「削除しない」「削除する」) の幅を取り、選んでも箱の幅を変えない
        let dest = matches!(row, Row::Dest(..)) && !editing;
        let dest_edit = matches!(row, Row::Dest(..)) && editing;
        let text = fit(&text, if dest { dest_w.saturating_sub(BUTTONS + 1) } else if dest_edit { dest_w.saturating_sub(1) } else { max }, editing);
        let color = match &row {
            Row::Hook(i) => match st.hooks.get(*i).map(|h| h.1) {
                Some(hooks::Status::Current) => Color::LightGreen,
                Some(hooks::Status::NoAgent) => Color::DarkGray,
                _ => Color::Yellow,
            },
            _ => Color::Reset,
        };
        let style = if is_sel { cursor_style() } else { Style::default().fg(color) };
        if is_sel {
            cursor_y = lines.len() as u16;
            selected = Some(row.clone());
            if editing {
                cursor_x = Line::from(format!(" {}", text.split('▏').next().unwrap_or(""))).width() as u16;
            }
        }
        if dest {
            let w = Line::from(format!(" {text}")).width();
            let pad = " ".repeat(dest_w.saturating_sub(BUTTONS).saturating_sub(w) + 1);
            hit_rows.push((lines.len() as u16, k, Some((w + pad.len()) as u16)));
            let (a, b) = if m.confirm { (" 削除しない ", " 削除する ") } else { (" 編集 ", " 削除 ") };
            let on = |del: bool| match (is_sel, m.del == del, del) {
                (true, true, true) => Style::default().bg(Color::LightRed).fg(Color::Black),
                (true, true, false) => cursor_style(),
                _ => Style::default(),
            };
            let label_style = if is_sel { Style::default().add_modifier(Modifier::BOLD) } else { Style::default() };
            let mut spans = vec![Span::styled(format!(" {text}"), label_style), Span::raw(pad)];
            // ボタンは選んだ行にだけ出すが、どの行もボタンの幅を取っておき、選んでも確認に変わっても箱の幅を変えない
            if is_sel {
                let rest = (BUTTONS - 1).saturating_sub(Line::from(format!("{a} {b}")).width());
                spans.extend([Span::styled(a, on(false)), Span::raw(" "), Span::styled(b, on(true)), Span::raw(" ".repeat(rest))]);
            } else {
                spans.push(Span::raw(" ".repeat(BUTTONS - 1)));
            }
            lines.push(Line::from(spans));
        } else if dest_edit {
            hit_rows.push((lines.len() as u16, k, None));
            // 編集に入っても、選んだ通知先の行 (「編集」「削除」付き) と同じ幅にする
            let w = Line::from(format!(" {text}")).width();
            lines.push(Line::styled(format!(" {text}{}", " ".repeat(dest_w.saturating_sub(w))), style));
        } else {
            hit_rows.push((lines.len() as u16, k, None));
            lines.push(Line::styled(format!(" {text} "), style));
        }
        k += 1;
    }
    let hint = match (&selected, &m.edit) {
        (_, Some(DestEdit { target: None, name: None, .. })) => "Enter 次へ (URL)  F12 やめる",
        (_, Some(_)) => "Enter 決定  F12 やめる",
        (Some(Row::Hook(i)), _) => st.hooks.get(*i).map_or("↑↓/jk 選択", |h| hook_verb(h.1)),
        (Some(Row::Sidebar), _) => "←/→ 幅  ↑↓/jk 選択  F12 閉じる",
        (Some(Row::Sound), _) => "←/→ 通知音を選ぶ (鳴らして確かめる)  ↑↓/jk 選択  F12 閉じる",
        (Some(Row::Popup | Row::Quiet), _) => "←/→ オン・オフ  ↑↓/jk 選択  F12 閉じる",
        (Some(Row::Dest(..)), _) if m.confirm => "←/→ 削除しない・削除する  Enter 決定",
        (Some(Row::Dest(..)), _) => "←/→ 編集・削除  Enter 決定  ↑↓/jk 選択",
        (Some(Row::AddDest), _) => "Enter 追加 (名前 → URL)  ↑↓/jk 選択  F12 閉じる",
        _ => "↑↓/jk 選択  Enter 決定  F12 閉じる",
    };
    lines.push(Line::from(""));
    lines.push(Line::styled(fit(m.notice.as_deref().unwrap_or(hint), box_inner(area), false), Style::default().fg(Color::LightGreen)));
    let r = draw_box(f, area, " Config ", lines);
    let (x0, y0, w) = (r.x + 1, r.y + 1 + PAD_Y, r.width.saturating_sub(2));
    let mut hits = st.hits.borrow_mut();
    for (line, k, buttons) in hit_rows {
        // ボタン (「 編集 」「 削除 」、確認中は「 削除しない 」「 削除する 」。間は 1) は行より先に当てる
        if let Some(bx) = buttons {
            let (aw, bw) = if m.confirm { (12, 10) } else { (6, 6) };
            let bx = x0 + PAD_X + bx;
            hits.push((Rect::new(bx, y0 + line, aw, 1), Hit::MenuButton(k, false)));
            hits.push((Rect::new(bx + aw + 1, y0 + line, bw, 1), Hit::MenuButton(k, true)));
        }
        hits.push((Rect::new(x0, y0 + line, w, 1), Hit::MenuRow(k)));
    }
    (r.x + 1 + PAD_X + cursor_x, r.y + 1 + PAD_Y + cursor_y)
}

pub(super) fn menu_input(s: &Shared, rest: &[u8]) -> usize {
    let starts = |a: &[u8], b: &[u8]| rest.starts_with(a) || rest.starts_with(b);
    let (up, down) = (starts(b"\x1b[A", b"\x1bOA"), starts(b"\x1b[B", b"\x1bOB"));
    let (left, right) = (starts(b"\x1b[D", b"\x1bOD"), starts(b"\x1b[C", b"\x1bOC"));
    let mut st = lock(s);
    let Some(mut m) = st.menu.take() else { return 0 };
    // 通知先の入力中
    if let Some(mut e) = m.edit.take() {
        let (used, enter) = e.line.key(rest);
        if !enter {
            m.edit = Some(e);
        } else {
            let v = e.line.buf.trim().to_string();
            match (&e.target, &e.name) {
                // 名前を決めたら続けて URL を入力する
                (None, None) => match validate_name(&v, std::iter::empty()) {
                    Ok(()) => {
                        e.name = Some(v);
                        e.line = LineEdit::new("https://");
                        m.notice = None;
                        m.edit = Some(e);
                    }
                    Err(err) => {
                        m.notice = Some(err);
                        m.edit = Some(e);
                    }
                },
                // 編集は同じ位置で URL だけ差し替え、ヘッダーは残す
                (target, name) => {
                    let r = match (target, name) {
                        (Some((_, old)), _) if *old == v => Ok(()),
                        (Some((n, old)), _) => match parse_webhook(&json!(v)) {
                            Ok(_) => {
                                let mut list = webhook_list(&st, Some(n));
                                list.iter_mut().filter(|h| h["url"] == old.as_str()).for_each(|h| h["url"] = json!(v));
                                store_webhooks(&mut st, Some(n), list)
                            }
                            Err(err) => Err(err),
                        },
                        (None, Some(n)) => {
                            drop(st);
                            let r = api(s, &json!({"cmd": "add_webhook", "owner": n, "url": v})).map(|_| ());
                            st = lock(s);
                            r
                        }
                        (None, None) => Ok(()),
                    };
                    match r {
                        Ok(()) => m.notice = Some("変更した".into()),
                        Err(err) => {
                            m.notice = Some(err_message(&err));
                            m.edit = Some(e);
                        }
                    }
                }
            }
        }
        st.menu = Some(m);
        mark(s, &mut st);
        return used;
    }
    let used = if rest[0] == 0x1b { esc_len(rest) } else { 1 };
    let mut open = true;
    if up || down || matches!(rest[0], b'j' | b'k') {
        m.del = false;
        m.confirm = false;
    }
    m.notice = None;
    let choices = menu_choices(&st);
    let last = choices.len().saturating_sub(1);
    m.sel = m.sel.min(last);
    let row = choices[m.sel].clone();
    let (lr, dir) = (left || right || matches!(rest[0], b'h' | b'l'), if left || rest[0] == b'h' { -1 } else { 1 });
    match (rest[0], &row) {
        _ if up => m.sel = m.sel.saturating_sub(1),
        _ if down => m.sel = (m.sel + 1).min(last),
        (b'k', _) => m.sel = m.sel.saturating_sub(1),
        (b'j', _) => m.sel = (m.sel + 1).min(last),
        (_, Row::Sidebar) if lr => {
            let w = st.sidebar as i32 + 2 * dir;
            set_sidebar(s, &mut st, w);
        }
        // 値は ←/→ で変える (Enter は入力と操作だけ)。通知音は なし → mac の音 … と選び、選んだ音を鳴らす
        (_, Row::Sound) if lr => {
            let list: Vec<Option<String>> = std::iter::once(None).chain(sounds().into_iter().map(Some)).collect();
            let at = list.iter().position(|x| *x == st.alerts.sound).unwrap_or(0) as i64;
            let next = list[(at + dir as i64).rem_euclid(list.len() as i64) as usize].clone();
            if let Some(n) = &next {
                play_sound(n);
            }
            st.alerts.sound = next;
            let sock = st.sock.clone();
            save_alerts(&sock, &st.alerts);
        }
        (_, Row::Popup | Row::Quiet) if lr => {
            let a = &mut st.alerts;
            let flag = if row == Row::Popup { &mut a.notify } else { &mut a.quiet_hooked };
            *flag = !*flag;
            let sock = st.sock.clone();
            save_alerts(&sock, &st.alerts);
        }
        (b'\r', Row::Hook(i)) => {
            if let Some(&(agent, status)) = st.hooks.get(*i) {
                log_event(&st.sock, "ui", &json!({"hook_install": agent}));
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
        (b'\r', Row::Update) if m.update.is_some() => {
            log_event(&st.sock, "ui", &json!({"update": m.update}));
            // 進み具合と失敗が見えるよう、更新は新しいタブで走らせる
            drop(st);
            let _ = new_tab(s, None, Some(UPGRADE_CMD), None, true, None, false);
            return 1;
        }
        (b'\r', Row::Update) => {
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
        // 通知先の行は ←/→ で編集か削除を選ぶ (確認中は「削除しない」か「削除する」)
        (_, Row::Dest(..)) if left || right => m.del = right,
        (b'\r', Row::Dest(name, url)) => match (m.confirm, m.del) {
            (true, del) => {
                if del {
                    drop(st);
                    let r = api(s, &json!({"cmd": "remove_webhook", "owner": name, "url": url}));
                    st = lock(s);
                    m.notice = Some(match r {
                        Ok(_) => "削除した".into(),
                        Err(e) => err_message(&e),
                    });
                }
                m.confirm = false;
                m.del = false;
            }
            // 削除はいきなりせず確認を挟む (既定は「削除しない」)
            (false, true) => {
                m.confirm = true;
                m.del = false;
            }
            (false, false) => m.edit = Some(DestEdit { target: Some((name.clone(), url.clone())), name: Some(name.clone()), line: LineEdit::new(url.clone()) }),
        },
        (b'\r', Row::AddDest) => m.edit = Some(DestEdit { target: None, name: None, line: LineEdit::new("") }),
        (b'\r', Row::Close) => open = false,
        _ => {}
    }
    if open {
        st.menu = Some(m);
    }
    mark(s, &mut st);
    used
}

/// フックが未導入でもアイコンを出せるよう、前面プロセス群の実行ファイル名から判定する。
pub(super) fn cached_fg_agent(t: &Tab) -> Option<&'static str> {
    let (agent, at) = t.fg.get();
    if at.is_some_and(|at| at.elapsed() < Duration::from_secs(1)) {
        return agent;
    }
    let agent = fg_agent(t.master.process_group_leader());
    t.fg.set((agent, Some(std::time::Instant::now())));
    agent
}

pub(super) fn fg_agent(pgrp: Option<libc::pid_t>) -> Option<&'static str> {
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

pub(super) fn draw_term(buf: &mut Buffer, area: Rect, term: &Term<Listener>) -> Option<(u16, u16)> {
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

pub(super) fn map_color(c: AColor) -> Color {
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

pub(super) fn cell_style(fg: AColor, bg: AColor, fl: Flags) -> Style {
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
