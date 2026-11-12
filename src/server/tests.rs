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
    assert!(
        w(&m) <= 20 && m.starts_with("https://") && m.ends_with("cccc") && m.contains('…'),
        "{m}"
    );
    let t = fit(url, 20, true);
    assert!(
        w(&t) <= 20 && t.starts_with('…') && t.ends_with("cccc"),
        "{t}"
    );
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
    assert_eq!(
        state(&s),
        (true, super::CUR_TABS, false),
        "ショートカットは無い"
    );
    inp.feed(&s, b"kk\r");
    assert_eq!(state(&s), (false, super::CUR_CONFIG, true));
    inp.feed(&s, f12);
    assert_eq!(
        (state(&s).0, state(&s).2),
        (false, false),
        "メニューを開いた状態の F12 は閉じる"
    );
    inp.feed(&s, f12);
    assert!(inp.feed(&s, b"k\r"), "Exit で離脱");
    inp.feed(&s, f12);
    assert!(inp.feed(&s, b"e"), "F12 e で離脱しない");
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
    super::api(
        &s,
        &json!({"cmd": "open_tab", "cwd": dir.to_str().unwrap(), "run": run}),
    )
    .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(500));
    super::Input::default().feed(&s, b"a\x1b[<0;88;23M\x1b[<0;88;23mb\x1b[M !!c");
    let got = (0..30).find_map(|_| {
        std::thread::sleep(std::time::Duration::from_millis(100));
        std::fs::read(&out).ok().filter(|b| b.ends_with(b"c"))
    });
    assert_eq!(got.as_deref(), Some(&b"abc"[..]));
    for t in super::lock(&s).tabs.iter() {
        let _ = std::process::Command::new("kill")
            .arg(t.child.process_id().unwrap().to_string())
            .status();
    }
    let _ = std::fs::remove_dir_all(dir);
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
        std::thread::spawn(move || {
            super::attach(
                s,
                std::io::BufReader::new(server),
                w,
                &json!({"cols": cols, "rows": rows}),
            )
        });
        client
    };
    let size = |s: &super::Shared| {
        let st = super::lock(s);
        (st.cols, st.rows)
    };
    let wait = |want: (u16, u16)| {
        (0..50).any(|_| {
            std::thread::sleep(std::time::Duration::from_millis(20));
            size(&s) == want
        })
    };
    let mut half = connect(50, 20);
    assert!(wait((50, 20)));
    let _full = connect(120, 40);
    assert!(wait((120, 40)));
    // 半画面の画面で入力すると、その大きさに戻る
    let mut msg = vec![super::IN_INPUT];
    msg.extend_from_slice(&1u32.to_be_bytes());
    msg.push(b'x');
    half.write_all(&msg).unwrap();
    assert!(
        wait((50, 20)),
        "最後に操作した画面の大きさにならない: {:?}",
        size(&s)
    );
}

#[test]
fn wheel_goes_to_apps_that_ask_for_mouse_reports() {
    use super::TermMode as M;
    assert_eq!(super::mouse_report(M::empty(), 64, 3, 4), None);
    assert_eq!(
        super::mouse_report(M::MOUSE_REPORT_CLICK | M::SGR_MOUSE, 65, 3, 4).unwrap(),
        b"\x1b[<65;3;4M"
    );
    assert_eq!(
        super::mouse_report(M::MOUSE_DRAG, 64, 1, 300).unwrap(),
        [0x1b, b'[', b'M', 96, 33, 255]
    );
}

#[test]
fn base64_matches_rfc4648() {
    for (i, o) in [
        ("", ""),
        ("f", "Zg=="),
        ("fo", "Zm8="),
        ("foo", "Zm9v"),
        ("foob", "Zm9vYg=="),
    ] {
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
    super::api(&s, &json!({"cmd": "open_tab", "cwd": dir.to_str().unwrap(), "run": "clear; printf 'hello world'; sleep 5"})).unwrap();
    // hello world が出た行と列 (1 始まりの画面座標にする)
    let (row, col) = (0..50)
        .find_map(|_| {
            std::thread::sleep(std::time::Duration::from_millis(100));
            let lines = super::screen_lines(&super::lock(&s).tabs[0].term);
            lines.iter().enumerate().find_map(|(y, l)| {
                l.find("hello world")
                    .map(|x| (y + 1, l[..x].chars().count() + 25))
            })
        })
        .unwrap();
    let at = |btn: u8, x: usize, up: bool| {
        format!("\x1b[<{btn};{x};{row}{}", if up { 'm' } else { 'M' }).into_bytes()
    };
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
    assert!(
        selected(&s).is_some_and(|t| t.chars().count() <= col - 24),
        "{:?}",
        selected(&s)
    );
    for t in super::lock(&s).tabs.iter() {
        let _ = std::process::Command::new("kill")
            .arg(t.child.process_id().unwrap().to_string())
            .status();
    }
    let _ = std::fs::remove_dir_all(dir);
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
        let id = super::api(
            &s,
            &json!({"cmd": "open_tab", "cwd": dir.to_str().unwrap(), "run": run, "name": name}),
        )
        .unwrap()["tab"]["id"]
            .as_u64()
            .unwrap() as u32;
        (id, out)
    };
    let (_, busy_out) = menu("busy");
    let (_, done_out) = menu("done");
    std::thread::sleep(std::time::Duration::from_millis(500));
    // get_answer は、止まっていなくてもカーソル付きのメニューなら選択肢を返し、回答の文中の番号付きの行は返さない
    let answer = |tab: &str| super::api(&s, &json!({"cmd": "get_answer", "tab": tab})).unwrap();
    let drawn = |tab: &str| {
        super::api(&s, &json!({"cmd": "get_screen", "tab": tab})).unwrap()["lines"]
            .as_array()
            .unwrap()
            .iter()
            .any(|l| l.as_str().unwrap().trim() == "3. No")
    };
    for _ in 0..50 {
        if drawn("busy") && drawn("done") {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    assert_eq!(answer("busy")["options"][0]["label"], "Yes");
    assert!(
        answer("done")["options"].is_null(),
        "カーソルの無い行を選択肢として返した"
    );
    {
        let mut st = super::lock(&s);
        for t in st.tabs.iter_mut() {
            t.auto_yes = true;
            // フックが届かずエージェントと分からないタブ (agent が無い) でも答える
            t.agent = None;
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
    let log = std::fs::read_to_string(dir.join("events.log")).unwrap_or_default();
    assert!(
        log.contains("auto_yes") && log.contains("\"pressed\":\"Yes\""),
        "events.log に残らない: {log}"
    );
    std::thread::sleep(std::time::Duration::from_millis(2000));
    assert_eq!(
        std::fs::read(&busy_out).unwrap(),
        b"1",
        "同じ確認を二度押した"
    );
    assert!(
        std::fs::read(&done_out).unwrap_or_default().is_empty(),
        "カーソルの無い番号付きの行で押した"
    );
    for t in super::lock(&s).tabs.iter() {
        let _ = std::process::Command::new("kill")
            .arg(t.child.process_id().unwrap().to_string())
            .status();
    }
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn background_tab_output_does_not_redraw_but_title_does() {
    let _fork = crate::FORK_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let s: super::Shared = Default::default();
    let dir = std::env::temp_dir().join(format!("mdeck-bg-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    {
        let mut st = super::lock(&s);
        st.sock = dir.join("t.sock").to_string_lossy().into_owned();
        st.cols = 100;
        st.rows = 30;
        st.sidebar = 24;
    }
    let cwd = dir.to_str().unwrap();
    super::api(
        &s,
        &json!({"cmd": "open_tab", "cwd": cwd, "run": "sleep 5"}),
    )
    .unwrap();
    // 裏のタブ: 出力を続けたあとタイトルを変える (タイトルの制御文字は落とす)
    super::api(&s, &json!({"cmd": "open_tab", "cwd": cwd, "run": "sleep 3; for i in 1 2 3 4 5; do printf x; sleep 0.1; done; sleep 1; printf '\\033]0;bgtitle\\007'; sleep 5"})).unwrap();
    // シェルがコマンドを受け取って出力が落ち着いてから見る
    super::lock(&s).active = 0;
    let screen = |s: &super::Shared| super::screen_lines(&super::lock(s).tabs[1].term).join("\n");
    let until = |f: &dyn Fn() -> bool| {
        (0..100).any(|_| {
            std::thread::sleep(std::time::Duration::from_millis(50));
            f()
        })
    };
    // シェルがコマンドを受け取ったら (プロンプトの形はシェルで違うので文字で見る)、sleep 3 の間に落ち着くのを待つ
    assert!(
        until(&|| screen(&s).contains("sleep 3; for")),
        "{}",
        screen(&s)
    );
    std::thread::sleep(std::time::Duration::from_millis(1000));
    super::lock(&s).dirty = false;
    assert!(until(&|| screen(&s).contains("xxxxx")), "{}", screen(&s));
    assert!(!super::lock(&s).dirty, "裏のタブの出力で描き直した");
    assert!(
        until(&|| super::lock(&s).tabs[1].title == "bgtitle"),
        "{:?}",
        super::lock(&s).tabs[1].title
    );
    assert_eq!(
        super::clean_title("✳ a\x1b[31mb\x07"),
        "a[31mb",
        "タイトルの制御文字を落としていない"
    );
    assert!(
        super::lock(&s).dirty,
        "裏のタブのタイトルが変わっても描き直さない"
    );
    for t in super::lock(&s).tabs.iter() {
        let _ = std::process::Command::new("kill")
            .arg(t.child.process_id().unwrap().to_string())
            .status();
    }
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn mouse_hover_highlights_and_clicks_open_and_change_things() {
    let _fork = crate::FORK_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let s: super::Shared = Default::default();
    let dir = std::env::temp_dir().join(format!("mdeck-click-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    {
        let mut st = super::lock(&s);
        st.sock = dir.join("t.sock").to_string_lossy().into_owned();
        st.cols = 120;
        st.rows = 40;
        st.sidebar = 24;
    }
    let cwd = dir.to_str().unwrap();
    for _ in 0..2 {
        super::api(
            &s,
            &json!({"cmd": "open_tab", "cwd": cwd, "run": "sleep 5"}),
        )
        .unwrap();
    }
    // 描いたときに押せる場所が決まる
    let draw = |s: &super::Shared| {
        let st = super::lock(s);
        let mut t =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(st.cols, st.rows)).unwrap();
        t.draw(|f| super::draw(f, &st)).unwrap();
    };
    let mut inp = super::Input::default();
    let ev = |btn: u8, x: u16, y: u16, up: bool| {
        format!(
            "\x1b[<{btn};{};{}{}",
            x + 1,
            y + 1,
            if up { 'm' } else { 'M' }
        )
        .into_bytes()
    };
    let (row_config, _, tabs_top) = super::rows(24);
    draw(&s);
    // サイドバーの行に乗ると明るくなる
    inp.feed(&s, &ev(35, 5, tabs_top + 1, false));
    assert_eq!(super::lock(&s).hover, Some(super::CUR_TABS + 1));
    // 左クリックでタブを選ぶ、右クリックでタブのメニュー
    inp.feed(&s, &ev(0, 5, tabs_top + 1, false));
    assert_eq!(super::lock(&s).active, 1);
    inp.feed(&s, &ev(2, 5, tabs_top, false));
    {
        let st = super::lock(&s);
        assert_eq!(
            st.session.as_ref().map(|p| p.tab),
            Some(st.tabs[0].id),
            "右クリックでタブのメニューが開かない"
        );
    }
    draw(&s);
    // ポップアップの外を押すと閉じる
    inp.feed(&s, &ev(0, 119, 39, false));
    assert!(super::lock(&s).session.is_none(), "外を押しても閉じない");
    // Config を開き、ポップアップの項目に乗ると選ばれ、値の行はクリックで変わる
    draw(&s);
    inp.feed(&s, &ev(0, 5, row_config, false));
    assert!(
        super::lock(&s).menu.is_some(),
        "Config をクリックしても開かない"
    );
    draw(&s);
    let popup = super::menu_sel(&super::lock(&s), |r| *r == super::Row::Popup);
    let (rect, _) = *super::lock(&s)
        .hits
        .borrow()
        .iter()
        .find(|(_, h)| *h == super::Hit::MenuRow(popup))
        .unwrap();
    inp.feed(&s, &ev(35, rect.x + 5, rect.y, false));
    assert_eq!(
        super::lock(&s).menu.as_ref().unwrap().sel,
        popup,
        "乗っても選ばれない"
    );
    assert!(!super::lock(&s).alerts.notify);
    inp.feed(&s, &ev(0, rect.x + 5, rect.y, false));
    assert!(super::lock(&s).alerts.notify, "左クリックで値が変わらない");
    inp.feed(&s, &ev(2, rect.x + 5, rect.y, false));
    assert!(!super::lock(&s).alerts.notify, "右クリックで値が戻らない");
    // Exit のクリックで離脱
    inp.feed(&s, &ev(0, 119, 39, false));
    draw(&s);
    assert!(
        inp.feed(&s, &ev(0, 5, row_config + 1, false)),
        "Exit のクリックで離脱しない"
    );
    for t in super::lock(&s).tabs.iter() {
        let _ = std::process::Command::new("kill")
            .arg(t.child.process_id().unwrap().to_string())
            .status();
    }
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn bind_sock_keeps_live_server_and_replaces_stale_socket() {
    let _fork = crate::FORK_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = std::env::temp_dir().join(format!("mdeck-bind-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let sock = dir.join("t.sock").to_string_lossy().into_owned();
    let live = super::bind_sock(&sock).unwrap();
    assert_eq!(
        super::bind_sock(&sock).unwrap_err().kind(),
        std::io::ErrorKind::AddrInUse
    );
    assert!(
        std::os::unix::net::UnixStream::connect(&sock).is_ok(),
        "生きているソケットが消された"
    );
    drop(live); // ファイルだけ残った死んだソケット
    assert!(super::bind_sock(&sock).is_ok());
    let _ = std::fs::remove_dir_all(dir);
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
    assert!(
        super::validate_name("a\x1b[2Jb", taken.into_iter()).is_err(),
        "制御文字の入った名前を通した"
    );
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
    let (a, b) = (
        super::new_tab(&s, Some(cwd), None, None, false, None, false).unwrap(),
        super::new_tab(&s, Some(cwd), None, None, false, None, false).unwrap(),
    );
    let mut inp = super::Input::default();
    let f12 = super::PREFIX;
    let ui = |s: &super::Shared| {
        let st = super::lock(s);
        (
            st.prefix,
            st.cursor,
            st.menu.is_some(),
            st.session.as_ref().map(|p| (p.tab, p.sel)),
        )
    };
    let tab0 = super::CUR_TABS;
    // F12 でカーソルは表示中のタブ。上で Config (Exit の上)、Enter で開く
    inp.feed(&s, f12);
    assert_eq!(ui(&s), (true, tab0, false, None));
    // 選んでいるタブの右端に → (メニューの印)
    {
        let st = super::lock(&s);
        let mut t =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(st.cols, st.rows)).unwrap();
        t.draw(|f| super::draw(f, &st)).unwrap();
        let y = super::rows(st.sidebar).2 + (tab0 - super::CUR_TABS) as u16;
        assert_eq!(t.backend().buffer()[(st.sidebar - 2, y)].symbol(), "→");
    }
    inp.feed(&s, b"kk\r");
    assert_eq!(ui(&s), (false, super::CUR_CONFIG, true, None));
    inp.feed(&s, b"\x1b");
    assert_eq!(
        ui(&s),
        (false, super::CUR_CONFIG, true, None),
        "Config の Esc で閉じた"
    );
    // Config は ← で閉じない (閉じるは「閉じる (F12)」の行か F12)
    inp.feed(&s, b"\x1b[D");
    assert_eq!(
        ui(&s),
        (false, super::CUR_CONFIG, true, None),
        "Config の ← で閉じた"
    );
    inp.feed(&s, f12);
    inp.feed(&s, f12);
    inp.feed(&s, b"kk");
    assert_eq!(ui(&s), (true, super::CUR_CONFIG, false, None));
    inp.feed(&s, b"\x1b");
    assert_eq!(
        ui(&s),
        (true, super::CUR_CONFIG, false, None),
        "サイドバーの Esc で閉じた"
    );
    inp.feed(&s, b"\x1b[C");
    assert_eq!(
        ui(&s),
        (false, super::CUR_CONFIG, true, None),
        "→ でもコンフィグを開く"
    );
    inp.feed(&s, f12);
    // F12 c で Config
    inp.feed(&s, f12);
    inp.feed(&s, b"c");
    assert!(ui(&s).2, "F12 c で Config が開かない");
    inp.feed(&s, f12);

    // 下でタブを選ぶとそのタブを表示し、→ でそのタブのメニュー
    inp.feed(&s, f12);
    inp.feed(&s, b"j");
    assert_eq!(super::lock(&s).active, 1);
    inp.feed(&s, b"\x1b[C");
    assert_eq!(
        ui(&s),
        (false, tab0 + 1, false, Some((b, 0))),
        "→ でタブのメニューを開かない"
    );
    inp.feed(&s, b"\x1b[C\x1b[D");
    assert_eq!(
        ui(&s),
        (false, tab0 + 1, false, Some((b, 0))),
        "タブのメニューの ←/→ で閉じた"
    );
    // 一番下の「閉じる (F12)」で閉じる
    inp.feed(&s, b"jjjj\r");
    assert_eq!(ui(&s).3, None, "「閉じる」で閉じない");
    inp.feed(&s, f12);
    inp.feed(&s, b"\r");
    assert_eq!(
        ui(&s),
        (false, tab0 + 1, false, None),
        "タブの Enter で作業に戻らない"
    );
    inp.feed(&s, f12);
    inp.feed(&s, b"\x1b[C");

    // 名前を変える
    inp.feed(&s, b"\r");
    inp.feed(&s, b"alpha\r");
    assert_eq!(
        super::lock(&s)
            .tabs
            .iter()
            .find(|t| t.id == b)
            .unwrap()
            .name
            .as_deref(),
        Some("alpha")
    );
    inp.feed(&s, f12);
    // Config の通知先で botX を足す (名前 → URL)。入力は Config の中で済ませる
    let open_dest = |s: &super::Shared| {
        let mut inp = super::Input::default();
        inp.feed(s, f12);
        inp.feed(s, b"c");
        let sel = super::menu_sel(&super::lock(s), |r| {
            matches!(r, super::Row::Dest(..) | super::Row::AddDest)
        });
        inp.feed(s, &b"j".repeat(sel));
    };
    open_dest(&s);
    inp.feed(&s, b"\rbotX\rx.example/h\r");
    assert_eq!(
        super::lock(&s).owner_webhooks["botX"][0]["url"],
        "https://x.example/h"
    );
    assert!(super::lock(&s).webhooks.is_empty(), "全体の一覧に入った");
    assert!(
        super::lock(&s)
            .menu
            .as_ref()
            .is_some_and(|m| m.edit.is_none()),
        "Config が閉じたか入力が終わらない"
    );
    // 追加した通知先の行で Enter を押すと URL の編集
    let url = |s: &super::Shared| super::lock(s).owner_webhooks["botX"][0]["url"].clone();
    let editing = |s: &super::Shared| {
        super::lock(s)
            .menu
            .as_ref()
            .and_then(|m| m.edit.as_ref().map(|e| e.line.buf.clone()))
    };
    let row = super::menu_sel(&super::lock(&s), |r| matches!(r, super::Row::Dest(..)));
    super::lock(&s).menu.as_mut().unwrap().sel = row;
    // 入力中の ←/→ はカーソルを動かすだけで閉じない
    inp.feed(&s, b"\r");
    inp.feed(&s, b"\x1b[D\x1b[D\x1b[C");
    inp.feed(&s, b"zz");
    inp.feed(&s, b"\x7f");
    assert_eq!(
        editing(&s).as_deref(),
        Some("https://x.example/zh"),
        "← で閉じたか、カーソルの位置に入らない"
    );
    // Esc は何もしない。編集を続けて 2 を足す
    inp.feed(&s, b"\x1b\x7f");
    assert!(editing(&s).is_some(), "Esc で入力をやめた");
    assert_eq!(url(&s), "https://x.example/h");
    inp.feed(&s, b"\x1b[C2\r");
    assert_eq!(url(&s), "https://x.example/h2", "Enter で保存しない");
    inp.feed(&s, f12);
    // タブのメニューでは通知先を ←/→ で選ぶ (なし ↔ botX)
    inp.feed(&s, f12);
    let c = super::lock(&s).cursor;
    inp.feed(
        &s,
        &if c > tab0 + 1 {
            b"k".repeat(c - tab0 - 1)
        } else {
            b"j".repeat(tab0 + 1 - c)
        },
    );
    inp.feed(&s, b"\x1b[Cj\x1b[C");
    let dest = |s: &super::Shared| {
        super::lock(s)
            .tabs
            .iter()
            .find(|t| t.id == b)
            .unwrap()
            .owner
            .clone()
    };
    assert_eq!(dest(&s).as_deref(), Some("botX"));
    inp.feed(&s, b"\x1b[C");
    assert_eq!(dest(&s), None);
    inp.feed(&s, b"\x1b[D");
    assert_eq!(dest(&s).as_deref(), Some("botX"));
    inp.feed(&s, f12);
    // → で「削除」を選ぶと確認になり、既定の「削除しない」では消さない
    open_dest(&s);
    inp.feed(&s, b"\x1b[C\r");
    assert!(super::lock(&s).menu.as_ref().unwrap().confirm);
    inp.feed(&s, b"\r");
    assert_eq!(url(&s), "https://x.example/h2", "「削除しない」で消した");
    inp.feed(&s, b"\x1b[C\r\x1b[C\r");
    assert!(!super::lock(&s).owner_webhooks.contains_key("botX"));
    inp.feed(&s, f12);
    // 名前の重複は断る
    inp.feed(&s, f12);
    let c = super::lock(&s).cursor;
    inp.feed(&s, &b"k".repeat(c - tab0));
    inp.feed(&s, b"\x1b[C\r");
    inp.feed(&s, b"alpha\r");
    assert!(
        super::lock(&s)
            .tabs
            .iter()
            .find(|t| t.id == a)
            .unwrap()
            .name
            .is_none()
    );
    inp.feed(&s, f12);
    assert_eq!(ui(&s).3, None);

    // 自動 Yes は Enter で切り替わる
    inp.feed(&s, f12);
    inp.feed(&s, b"\x1b[Cjj\r");
    let auto = |s: &super::Shared| {
        super::lock(s)
            .tabs
            .iter()
            .find(|t| t.id == a)
            .unwrap()
            .auto_yes
    };
    assert!(!auto(&s), "Enter で切り替わった");
    inp.feed(&s, b"\x1b[C");
    assert!(auto(&s));
    inp.feed(&s, b"\x1b[D");
    assert!(!auto(&s));
    // 「タブを削除」は確認になり、既定の「削除しない」で Enter を重ねても閉じない
    inp.feed(&s, b"j\r");
    assert!(super::lock(&s).session.as_ref().unwrap().confirm_close);
    inp.feed(&s, b"\r");
    assert!(
        !super::lock(&s).session.as_ref().unwrap().confirm_close,
        "「削除しない」で確認が終わらない"
    );
    assert_eq!(super::lock(&s).tabs.len(), 2);
    inp.feed(&s, b"\r\r");
    assert_eq!(super::lock(&s).tabs.len(), 2);
    inp.feed(&s, b"\rj\r");
    assert!(super::lock(&s).session.is_none(), "「削除する」で閉じない");
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
        let mut t =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(st.cols, st.rows)).unwrap();
        t.draw(|f| super::draw(f, &st)).unwrap();
        let b = t.backend().buffer();
        let row = |y: u16| {
            (0..b.area.width)
                .map(|x| b[(x, y)].symbol().to_string())
                .collect::<String>()
        };
        assert!(
            row(0).starts_with('┌') && row(0).replace(" ", "").contains("F12sidebar"),
            "{}",
            row(0)
        );
        assert_eq!(
            (b[(0, 10)].symbol(), b[(b.area.width - 1, 10)].symbol()),
            ("│", "│")
        );
        if std::env::var("MDECK_DUMP").is_ok() {
            (0..b.area.height).for_each(|y| eprintln!("{}", row(y)));
        }
    }
    inp.feed(&s, f12);

    let st = super::lock(&s);
    for t in st.tabs.iter() {
        let _ = std::process::Command::new("kill")
            .arg(t.child.process_id().unwrap().to_string())
            .status();
    }
    drop(st);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn api_errors_are_coded_and_arguments_are_checked() {
    let _fork = crate::FORK_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let s: super::Shared = Default::default();
    let dir = std::env::temp_dir().join(format!("mdeck-apierr-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    {
        let mut st = super::lock(&s);
        st.sock = dir.join("t.sock").to_string_lossy().into_owned();
        st.cols = 100;
        st.rows = 30;
        st.sidebar = 24;
    }
    let cwd = dir.to_str().unwrap();
    let api = |req: serde_json::Value| super::api(&s, &req);
    let code = |r: Result<serde_json::Value, String>| {
        super::err_json(&r.unwrap_err())["error"]["code"]
            .as_str()
            .unwrap()
            .to_string()
    };
    assert_eq!(code(api(json!({"cmd": "nope"}))), "unknown_cmd");
    assert_eq!(code(api(json!({"cmd": "open_tab"}))), "invalid");
    assert_eq!(
        code(api(
            json!({"cmd": "open_tab", "cwd": cwd, "agent": "claude", "run": "ls"})
        )),
        "invalid",
        "agent と run を両方通した"
    );
    assert_eq!(
        code(api(json!({"cmd": "open_tab", "cwd": cwd, "agent": "vim"}))),
        "invalid"
    );
    assert!(super::lock(&s).tabs.is_empty(), "断ったのにタブを作った");
    let id = api(json!({"cmd": "open_tab", "cwd": cwd, "run": "", "owner": "botZ", "name": "zz"}))
        .unwrap()["tab"]["id"]
        .as_u64()
        .unwrap();
    // tab は番号・数字の文字列・名前のどれでもよい
    for tab in [json!(id), json!(id.to_string()), json!("zz")] {
        assert_eq!(
            api(json!({"cmd": "get_screen", "tab": tab})).unwrap()["tab"]["id"],
            id
        );
    }
    assert_eq!(
        code(api(json!({"cmd": "get_screen", "tab": "nobody"}))),
        "no_such_tab"
    );
    assert_eq!(
        code(api(json!({"cmd": "get_screen", "tab": ""}))),
        "invalid",
        "空の名前が名前の無いタブに当たった"
    );
    assert_eq!(
        code(api(json!({"cmd": "send", "tab": id}))),
        "invalid",
        "text 抜けを通した"
    );
    assert_eq!(code(api(json!({"cmd": "choose", "tab": id}))), "invalid");
    assert_eq!(
        code(api(
            json!({"cmd": "update_tab", "tab": id, "auto_yes": "yes"})
        )),
        "invalid"
    );
    assert_eq!(
        code(api(json!({"cmd": "close_tab"}))),
        "invalid",
        "tab も owner も無しで閉じた"
    );
    assert_eq!(
        code(api(
            json!({"cmd": "remove_webhook", "url": "https://x/none"})
        )),
        "not_found"
    );
    // 通知先は同じ URL・同じヘッダーなら書き直さず、ヘッダーだけ違えば置き換える
    let hook = |k: &str| json!({"cmd": "add_webhook", "owner": "botZ", "url": "https://g/z", "headers": {"Authorization": k}});
    api(hook("a")).unwrap();
    let mtime = || {
        std::fs::metadata(dir.join("owner_webhooks.json"))
            .unwrap()
            .modified()
            .unwrap()
    };
    let before = mtime();
    std::thread::sleep(std::time::Duration::from_millis(20));
    api(hook("a")).unwrap();
    assert_eq!(mtime(), before, "同じ登録で書き直した");
    let r = api(hook("b")).unwrap();
    assert_eq!(
        r["webhooks"],
        json!([{"url": "https://g/z", "headers": ["Authorization"]}]),
        "ヘッダーの値を返した"
    );
    assert_eq!(
        super::lock(&s).owner_webhooks["botZ"][0]["headers"]["Authorization"],
        "b"
    );
    // owner でまとめて閉じる
    assert_eq!(
        api(json!({"cmd": "close_tab", "owner": "botZ"})).unwrap()["closed"],
        json!([id])
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn update_tab_owner_routes_done_to_that_webhook() {
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
    let id = api(json!({"cmd": "open_tab", "cwd": dir.to_str().unwrap(), "run": ""})).unwrap()["tab"]["id"].as_u64().unwrap();
    let srv = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/h", srv.local_addr().unwrap());
    api(json!({"cmd": "add_webhook", "owner": "botY", "url": url})).unwrap();
    let r = api(json!({"cmd": "update_tab", "tab": id, "owner": "botY"})).unwrap();
    assert_eq!(r["tab"]["owner"], "botY");
    let listed = api(json!({"cmd": "list_tabs"})).unwrap();
    assert_eq!(listed["tabs"][0]["owner"], "botY");
    // done が続いても 2 回目を送る
    {
        let mut st = super::lock(&s);
        super::set_state(&s, &mut st, id as u32, Some("done"));
        super::set_state(&s, &mut st, id as u32, Some("done"));
    }
    let (mut c, _) = srv.accept().unwrap();
    c.set_read_timeout(Some(std::time::Duration::from_secs(5)))
        .unwrap();
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
    let log = (0..50)
        .find_map(|_| {
            std::thread::sleep(std::time::Duration::from_millis(100));
            std::fs::read_to_string(dir.join("webhook.log"))
                .ok()
                .filter(|l| l.contains("http="))
        })
        .expect("webhook.log");
    assert!(
        log.contains("owner=\"botY\"") && log.contains("state=\"done\"") && !log.contains("/h\n"),
        "{log}"
    );
    srv.set_nonblocking(true).unwrap();
    let second = (0..50).any(|_| {
        std::thread::sleep(std::time::Duration::from_millis(100));
        srv.accept().is_ok()
    });
    assert!(second, "2 回目の done を送らない");
    // 長い webhook を編集に入っても箱の幅が変わらない
    let long = format!("https://example.com/{}", "a".repeat(60));
    api(json!({"cmd": "add_webhook", "owner": "botY", "url": long})).unwrap();
    let width = |s: &super::Shared| {
        let st = super::lock(s);
        let mut t =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(st.cols, st.rows)).unwrap();
        t.draw(|f| super::draw(f, &st)).unwrap();
        // ポップアップの枠 (画面の枠より内側の ┌ から ┐ まで)
        let b = t.backend().buffer();
        (0..b.area.height)
            .find_map(|y| {
                let l = (1..b.area.width).find(|&x| b[(x, y)].symbol() == "┌")?;
                (l..b.area.width)
                    .find(|&x| b[(x, y)].symbol() == "┐")
                    .map(|r| r - l + 1)
            })
            .unwrap()
    };
    {
        let mut st = super::lock(&s);
        // 画面幅で頭打ちにならないよう広くする
        st.cols = 200;
        let sel = super::menu_sel(&st, |r| *r == super::Row::Dest("botY".into(), long.clone()));
        st.menu = Some(super::Menu::new(sel));
    }
    // Config の箱の幅は、通知先の行を選んでも (ボタンが出ても) 選ばなくても同じ
    let sel = super::lock(&s).menu.as_ref().unwrap().sel;
    super::lock(&s).menu.as_mut().unwrap().sel = 0;
    let unselected = width(&s);
    super::lock(&s).menu.as_mut().unwrap().sel = sel;
    assert_eq!(width(&s), unselected, "通知先の行を選ぶと箱の幅が変わった");
    // 削除の確認に変わっても、編集に入っても箱の幅が変わらない
    super::lock(&s).menu.as_mut().unwrap().confirm = true;
    assert_eq!(width(&s), unselected, "削除の確認で箱の幅が変わった");
    super::lock(&s).menu.as_mut().unwrap().confirm = false;
    let before = width(&s);
    super::menu_input(&s, b"\r");
    assert!(super::lock(&s).menu.as_ref().unwrap().edit.is_some());
    assert_eq!(width(&s), before, "編集に入ると箱の幅が変わった");
    super::lock(&s).menu = None;
    // 外すと tab の owner は null、不正な名前は断る
    assert!(
        api(json!({"cmd": "update_tab", "tab": id, "owner": ""})).unwrap()["tab"]["owner"]
            .is_null()
    );
    assert!(api(json!({"cmd": "update_tab", "tab": id, "owner": "a b"})).is_err());
    for t in super::lock(&s).tabs.iter() {
        let _ = std::process::Command::new("kill")
            .arg(t.child.process_id().unwrap().to_string())
            .status();
    }
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn sidebar_shows_weekly_usage_and_one_line_title() {
    use crate::usage::{WEEK, Week};
    let mut st = super::State {
        sidebar: 24,
        ..Default::default()
    };
    let now = super::now_secs();
    // 週の半分で 25% (1d18h の余裕)、codex は記録なし、copilot は月 (30 日) の半分で 10% (12 日の余裕)
    let month = 30 * 86400;
    st.usage = [
        Some(Week {
            used: 25.0,
            resets_at: now + WEEK / 2,
            window: WEEK,
        }),
        None,
        Some(Week {
            used: 10.0,
            resets_at: now + month / 2,
            window: month,
        }),
    ];
    let mut t = ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 30)).unwrap();
    t.draw(|f| super::draw(f, &st)).unwrap();
    let b = t.backend().buffer();
    let row = |y: u16| {
        (0..24)
            .map(|x| b[(x, y)].symbol().to_string())
            .collect::<String>()
    };
    if std::env::var("MDECK_DUMP").is_ok() {
        (0..30).for_each(|y| eprintln!("{}", row(y)));
    }
    assert!(
        row(0).starts_with(&format!(" {}", super::head_title())),
        "{}",
        row(0)
    );
    assert!(row(1).contains("Config"), "{}", row(1));
    assert!(row(21).contains("Claude -1d18h"), "{}", row(21));
    assert_eq!(b[(1 + 11, 22)].symbol(), "│", "今の位置");
    assert_eq!(
        b[(1, 22)].bg,
        ratatui::style::Color::LightGreen,
        "使用量の塗り"
    );
    assert!(row(23).contains("Codex —"), "{}", row(23));
    assert!(row(25).contains("Copilot -12d"), "{}", row(25));
    // 狭いと見出しは 2 行
    assert_eq!(super::rows(14).0, 2);
    // F12 で Exit / + / Config を選ぶと、右側は端末の代わりに説明かメニュー
    let all = |st: &super::State| {
        let mut t = ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 30)).unwrap();
        t.draw(|f| super::draw(f, st)).unwrap();
        let b = t.backend().buffer().clone();
        (0..30)
            .map(|y| {
                (0..80)
                    .map(|x| b[(x, y)].symbol().to_string())
                    .collect::<String>()
                    .replace(' ', "")
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    assert!(all(&st).contains("Notabs"));
    st.prefix = true;
    for (cursor, want) in [
        (super::CUR_EXIT, "ClosetheMornDeckscreen"),
        (super::CUR_TABS, "Openashelltabinthehomefolder"),
        (super::CUR_CONFIG, "Enter/→/ctoopen"),
    ] {
        st.cursor = cursor;
        let text = all(&st);
        assert!(
            text.contains(want) && !text.contains("Notabs"),
            "{cursor}: {text}"
        );
    }
    // Config は見出しの下に 1 段下げて並び、設定値の始まりの列が揃う
    let lines = super::menu_lines(&super::Menu::new(0), &st);
    let order: Vec<String> = lines
        .iter()
        .filter(|(r, _)| matches!(r, super::Row::Head(_)))
        .map(|(_, t)| t.clone())
        .collect();
    assert_eq!(order, ["Hooks", "Appearance", "Notifications", "Webhooks"]);
    let text = |want: super::Row| lines.iter().find(|(r, _)| *r == want).unwrap().1.clone();
    let col = |s: &str| {
        let w = |x: &str| ratatui::text::Line::from(x).width();
        let v = s.find("←").unwrap();
        w(&s[..v])
    };
    assert!(text(super::Row::Sidebar).starts_with("  Sidebar width"));
    assert_eq!(
        col(&text(super::Row::Sidebar)),
        col(&text(super::Row::Sound))
    );
    assert_eq!(col(&text(super::Row::Sound)), col(&text(super::Row::Quiet)));
    assert_eq!(text(super::Row::Close), "Close (F12)");
}

/// 待つ API は、状態が変わったらすぐ返る (決まった間隔を待たない)
#[test]
fn wait_returns_as_soon_as_the_state_changes() {
    let _fork = crate::FORK_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let s: super::Shared = Default::default();
    let dir = std::env::temp_dir().join(format!("mdeck-wake-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    super::lock(&s).sock = dir.join("t.sock").to_string_lossy().into_owned();
    let id = super::api(
        &s,
        &json!({"cmd": "open_tab", "cwd": dir.to_str().unwrap(), "run": ""}),
    )
    .unwrap()["tab"]["id"]
        .as_u64()
        .unwrap() as u32;
    {
        let mut st = super::lock(&s);
        super::set_state(&s, &mut st, id, Some("working"));
    }
    let s2 = s.clone();
    let waiter = std::thread::spawn(move || {
        let r = super::api(
            &s2,
            &json!({"cmd": "get_answer", "tab": id, "timeout_ms": 10000}),
        )
        .unwrap();
        (r, std::time::Instant::now())
    });
    // timeout_ms が無ければ待たずに返す
    assert_eq!(
        super::api(&s, &json!({"cmd": "get_answer", "tab": id})).unwrap()["state"],
        "working"
    );
    std::thread::sleep(std::time::Duration::from_millis(300));
    let at = std::time::Instant::now();
    {
        let mut st = super::lock(&s);
        super::set_state(&s, &mut st, id, Some("done"));
    }
    let (r, back) = waiter.join().unwrap();
    assert_eq!(r["state"], "done");
    assert!(
        back - at < std::time::Duration::from_millis(100),
        "変化から返るまで {:?} かかった",
        back - at
    );
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
    let id = api(json!({"cmd": "open_tab", "cwd": dir.to_str().unwrap(), "run": ""})).unwrap()["tab"]["id"].as_u64().unwrap() as u32;
    let unread = |s: &super::Shared| super::lock(s).tabs[0].unread;
    {
        let mut st = super::lock(&s);
        super::set_state(&s, &mut st, id, Some("done"));
    }
    assert!(unread(&s), "画面も API も見ていない完了が未読にならない");
    api(json!({"cmd": "list_tabs"})).unwrap();
    assert!(unread(&s), "一覧で既読になった");
    api(json!({"cmd": "get_answer", "tab": id})).unwrap();
    assert!(!unread(&s), "answer で既読にならない");
    // Config の切り替えは config.json に残る (既定は音・通知オフ、webhook のあるタブは鳴らさない)
    assert_eq!(
        super::load_alerts(&sock),
        super::Alerts {
            sound: None,
            notify: false,
            quiet_hooked: true,
            usage: true
        }
    );
    for row in [super::Row::Popup, super::Row::Quiet] {
        let sel = super::menu_sel(&super::lock(&s), |r| *r == row);
        super::lock(&s).menu = Some(super::Menu::new(sel));
        super::menu_input(&s, b"\r");
        super::menu_input(&s, b"\x1b[C");
    }
    assert_eq!(
        super::load_alerts(&sock),
        super::Alerts {
            sound: None,
            notify: true,
            quiet_hooked: false,
            usage: true
        }
    );
    // 前の版の「音: オン」は Glass として読む
    std::fs::write(
        dir.join("config.json"),
        r#"{"sound": true, "notify": false, "quiet_hooked": true}"#,
    )
    .unwrap();
    assert_eq!(super::load_alerts(&sock).sound.as_deref(), Some("Glass"));
    for t in super::lock(&s).tabs.iter() {
        let _ = std::process::Command::new("kill")
            .arg(t.child.process_id().unwrap().to_string())
            .status();
    }
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn copilot_hook_payload_is_camel_case_with_event_from_command() {
    let _fork = crate::FORK_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let s: super::Shared = Default::default();
    let dir = std::env::temp_dir().join(format!("mdeck-cophook-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    super::lock(&s).sock = dir.join("t.sock").to_string_lossy().into_owned();
    let api = |req: serde_json::Value| super::api(&s, &req);
    let id = api(json!({"cmd": "open_tab", "cwd": dir.to_str().unwrap(), "run": ""})).unwrap()["tab"]["id"].as_u64().unwrap();
    let hook = |event: &str, payload: serde_json::Value| {
        api(json!({"cmd": "hook", "agent": "copilot", "event": event, "tab": id, "payload": payload})).unwrap()
    };
    hook(
        "UserPromptSubmit",
        json!({"sessionId": "abc-1", "prompt": "x", "cwd": "/"}),
    );
    {
        let st = super::lock(&s);
        assert_eq!(
            (
                st.tabs[0].state,
                st.tabs[0].agent.as_deref(),
                st.tabs[0].session.as_deref()
            ),
            (Some("working"), Some("copilot"), Some("abc-1"))
        );
    }
    hook(
        "Stop",
        json!({"sessionId": "abc-1", "stopReason": "end_turn", "transcriptPath": "/x"}),
    );
    let (state, seq) = {
        let st = super::lock(&s);
        (st.tabs[0].state, st.tabs[0].answer_seq)
    };
    assert_eq!((state, seq), (Some("done"), 1));
    // 再開は copilot --resume
    assert_eq!(
        super::resume_command(Some("copilot"), Some("abc-1"), Some("--yolo")).as_deref(),
        Some("copilot --resume abc-1 --yolo")
    );
    for t in super::lock(&s).tabs.iter() {
        let _ = std::process::Command::new("kill")
            .arg(t.child.process_id().unwrap().to_string())
            .status();
    }
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn test_webhooks_posts_now_and_returns_status() {
    use std::io::{Read, Write};
    let s: super::Shared = Default::default();
    let dir = std::env::temp_dir().join(format!("mdeck-whtest-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    super::lock(&s).sock = dir.join("t.sock").to_string_lossy().into_owned();
    let api = |req: serde_json::Value| super::api(&s, &req);
    assert!(
        api(json!({"cmd": "test_webhooks", "owner": "botT"}))
            .unwrap_err()
            .contains("no webhook registered")
    );
    let srv = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/secret-id", srv.local_addr().unwrap());
    api(json!({"cmd": "add_webhook", "owner": "botT", "url": url, "headers": {"Authorization": "Bearer s3cret"}})).unwrap();
    let reply = |code: &'static str| {
        let srv = srv.try_clone().unwrap();
        std::thread::spawn(move || {
            let (mut c, _) = srv.accept().unwrap();
            let mut got = Vec::new();
            let mut buf = [0u8; 4096];
            while !String::from_utf8_lossy(&got).contains("Test notification") {
                let n = c.read(&mut buf).unwrap();
                got.extend_from_slice(&buf[..n]);
            }
            let _ = c.write_all(
                format!("HTTP/1.1 {code}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                    .as_bytes(),
            );
            String::from_utf8_lossy(&got).into_owned()
        })
    };
    let t = reply("204 No Content");
    let r = api(json!({"cmd": "test_webhooks", "owner": "botT"})).unwrap();
    // ヘッダーはファイル経由で届き、ファイルは残らない
    assert!(
        t.join().unwrap().contains("Authorization: Bearer s3cret"),
        "ヘッダーが届かない"
    );
    let left: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().starts_with("webhook-curl"))
        .collect();
    assert!(left.is_empty(), "ヘッダーのファイルが残った");
    assert_eq!(
        (r["ok"].as_bool(), r["results"][0]["http"].as_u64()),
        (Some(true), Some(204)),
        "{r}"
    );
    assert!(
        !r.to_string().contains("secret-id"),
        "URL の末尾を伏せていない: {r}"
    );
    let t = reply("401 Unauthorized");
    let r = api(json!({"cmd": "test_webhooks", "owner": "botT"})).unwrap();
    t.join().unwrap();
    assert_eq!(
        (r["ok"].as_bool(), r["results"][0]["http"].as_u64()),
        (Some(false), Some(401)),
        "{r}"
    );
    let _ = std::fs::remove_dir_all(dir);
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
    api(json!({"cmd": "add_webhook", "owner": "botA", "url": "https://a/1"}));
    api(
        json!({"cmd": "add_webhook", "owner": "botB", "url": "https://b/1", "headers": {"X-Key": "k"}}),
    );
    // botA の一覧に無い URL は消せず、botB と全体は残る
    assert!(
        super::api(
            &s,
            &json!({"cmd": "remove_webhook", "owner": "botA", "url": "https://b/1"})
        )
        .is_err()
    );
    assert!(
        super::api(
            &s,
            &json!({"cmd": "remove_webhook", "owner": "botA", "url": "https://g/old"})
        )
        .is_err()
    );
    let urls = |r: serde_json::Value| {
        r["webhooks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|w| w["url"].as_str().unwrap().to_string())
            .collect::<Vec<_>>()
    };
    assert_eq!(
        urls(api(json!({"cmd": "list_webhooks"}))),
        ["https://g/old"],
        "全体の登録が変わった"
    );
    assert_eq!(
        urls(api(json!({"cmd": "list_webhooks", "owner": "botA"}))),
        ["https://a/1"]
    );
    assert_eq!(
        urls(api(json!({"cmd": "list_webhooks", "owner": "botB"}))),
        ["https://b/1"]
    );
    // 全体の webhooks.json は配列のまま、owner 分は別ファイルに残り、再起動で読み戻せる
    let global: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join("webhooks.json")).unwrap()).unwrap();
    assert!(global.is_array());
    let owners = super::load_owner_webhooks(&sock);
    assert_eq!(owners["botB"][0]["headers"]["X-Key"], "k");
    api(json!({"cmd": "remove_webhook", "owner": "botA", "url": "https://a/1"}));
    assert!(
        !super::load_owner_webhooks(&sock).contains_key("botA"),
        "空になった owner が残っている"
    );
    assert!(super::api(&s, &json!({"cmd": "list_webhooks", "owner": "a b"})).is_err());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn parse_webhook_accepts_url_or_object_and_rejects_injection() {
    use serde_json::json;
    assert_eq!(
        super::parse_webhook(&json!("https://a/x")).unwrap(),
        json!({"url": "https://a/x", "headers": {}})
    );
    assert!(super::parse_webhook(&json!({"url": "https://a", "headers": {"X-Key": "s"}})).is_ok());
    assert!(super::parse_webhook(&json!("ftp://a")).is_err());
    assert!(
        super::parse_webhook(&json!("https://a/\u{1b}[2J")).is_err(),
        "URL の制御文字を通した"
    );
    assert!(
        super::parse_webhook(&json!("https://a/ b")).is_err(),
        "URL の空白を通した"
    );
    // ログではヘッダーの値を伏せ、長い文字列を切る
    let b = super::brief(
        &json!({"add": {"url": "https://a", "headers": {"Authorization": "Bearer s3cret"}}, "text": "x".repeat(1000)}),
    );
    assert_eq!(b["add"]["headers"]["Authorization"], "<hidden>");
    assert!(!b.to_string().contains("s3cret") && b["text"].as_str().unwrap().chars().count() < 400);
    assert!(
        super::parse_webhook(&json!({"url": "https://a", "headers": {"X-Key": "a\r\nEvil: 1"}}))
            .is_err()
    );
    assert!(
        super::parse_webhook(&json!({"url": "https://a", "headers": {"Bad Name": "v"}})).is_err()
    );
}

#[test]
fn resume_command_uses_session_and_rejects_shell_metachars() {
    use super::resume_command as r;
    assert_eq!(
        r(Some("claude"), Some("ab-12"), None).as_deref(),
        Some("claude --resume ab-12")
    );
    assert_eq!(
        r(Some("codex"), Some("ab-12"), Some("--yolo")).as_deref(),
        Some("codex resume ab-12 --yolo")
    );
    assert_eq!(r(Some("claude"), Some("x; rm -rf ~"), None), None);
    assert_eq!(r(Some("claude"), None, None), None);
    assert_eq!(r(None, Some("ab"), None), None);
    let a = super::agent_args;
    assert_eq!(
        a(Some("claude"), true, None).as_deref(),
        Some("--dangerously-skip-permissions")
    );
    assert_eq!(
        a(Some("codex"), true, Some("-m x")).as_deref(),
        Some("--dangerously-bypass-approvals-and-sandbox -m x")
    );
    assert_eq!(a(Some("claude"), false, Some("-c")).as_deref(), Some("-c"));
    assert_eq!(a(None, true, None), None);
}

#[test]
fn unnumbered_menu_is_read_but_idle_prompt_is_not() {
    let v = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let trust = v(&[
        " Security guide",
        " ❯ No, exit",
        "   Yes, I trust this folder",
        " Enter to confirm · Esc to cancel",
    ]);
    let o = super::parse_options(&trust);
    assert_eq!(o.len(), 2);
    assert_eq!(o[1]["label"], "Yes, I trust this folder");
    assert_eq!(o[0]["selected"], true);
    assert_eq!(o[0]["numbered"], false);
    let idle = v(&[
        "────────",
        "❯ 途中まで打った文",
        "────────",
        "  ? for shortcuts",
    ]);
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
    let q: Vec<String> = [" ❯ 1. 赤", "   2. No"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    assert_eq!(super::yes_choice(&super::parse_options(&q)), None);
    let codex: Vec<String> = [
        " › 1. Yes, proceed (y)",
        "   2. No, and tell Codex what to do differently (esc)",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    assert_eq!(super::yes_choice(&super::parse_options(&codex)), Some(0));
}

/// エージェントが中で起動した別の会話 (claude -p など) のフックは、タブの状態にも回答にも混ぜない。
/// 混ざると、作業中なのに get_answer が待たずに返り、別の会話の回答が完了として通知される
#[test]
fn hooks_from_another_session_in_the_tab_are_ignored() {
    let _fork = crate::FORK_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let s: super::Shared = Default::default();
    let dir = std::env::temp_dir().join(format!("mdeck-nested-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    super::lock(&s).sock = dir.join("t.sock").to_string_lossy().into_owned();
    let api = |req: serde_json::Value| super::api(&s, &req);
    let id = api(json!({"cmd": "open_tab", "cwd": dir.to_str().unwrap(), "run": ""})).unwrap()["tab"]["id"].as_u64().unwrap();
    let hook = |event: &str, session: &str, extra: serde_json::Value| {
        let mut p = json!({"hook_event_name": event, "session_id": session});
        p.as_object_mut()
            .unwrap()
            .extend(extra.as_object().cloned().unwrap_or_default());
        api(json!({"cmd": "hook", "agent": "claude", "tab": id, "payload": p})).unwrap()
    };
    let now = || {
        let st = super::lock(&s);
        let t = &st.tabs[0];
        (
            t.state,
            t.agent.clone(),
            t.answer_seq,
            t.answer.clone(),
            t.session.clone(),
        )
    };
    let main = |state, seq, answer: Option<&str>| {
        (
            state,
            Some("claude".to_string()),
            seq,
            answer.map(str::to_string),
            Some("main".to_string()),
        )
    };
    hook("SessionStart", "main", json!({}));
    hook("UserPromptSubmit", "main", json!({}));
    // 中で起動した会話が始まって答えて終わっても、タブは作業中のまま
    hook("SessionEnd", "other", json!({"reason": "other"}));
    assert_eq!(now(), main(Some("working"), 0, None));
    hook("SessionStart", "nested", json!({}));
    hook("UserPromptSubmit", "nested", json!({}));
    hook(
        "Stop",
        "nested",
        json!({"last_assistant_message": "中の回答"}),
    );
    hook("SessionEnd", "nested", json!({"reason": "other"}));
    assert_eq!(now(), main(Some("working"), 0, None));
    // 待っている get_answer も返らない (時間切れまで待つ)
    let r = api(json!({"cmd": "get_answer", "tab": id, "timeout_ms": 50})).unwrap();
    assert_eq!(
        (r["state"].as_str(), r["timed_out"].as_bool()),
        (Some("working"), Some(true))
    );
    hook(
        "Stop",
        "main",
        json!({"last_assistant_message": "本来の回答"}),
    );
    assert_eq!(now(), main(Some("done"), 1, Some("本来の回答")));
    // /clear は終了扱いにせず、次の会話の SessionStart で idle になる
    hook("SessionEnd", "main", json!({"reason": "clear"}));
    assert_eq!(
        (now().0, now().1.as_deref(), now().4),
        (Some("done"), Some("claude"), None)
    );
    hook("SessionStart", "cleared", json!({"source": "clear"}));
    assert_eq!(
        (now().0, now().4.as_deref()),
        (Some("idle"), Some("cleared"))
    );
    // SessionEnd 無しで落ちた後に起動し直した会話 (前面のプロセスが入れ替わっている) は受け入れる
    super::lock(&s).tabs[0].session_pgrp = Some(-1);
    hook("UserPromptSubmit", "restarted", json!({}));
    assert_eq!(
        now().4.as_deref(),
        Some("cleared"),
        "SessionStart 以外は受け入れない"
    );
    hook("SessionStart", "restarted", json!({}));
    assert_eq!(now().4.as_deref(), Some("restarted"));
    for t in super::lock(&s).tabs.iter() {
        let _ = std::process::Command::new("kill")
            .arg(t.child.process_id().unwrap().to_string())
            .status();
    }
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn logs_mask_webhook_urls_and_plain_http_is_flagged() {
    let v = super::brief(
        &json!({"cmd": "add_webhook", "url": "https://example.com/hooks/secret-id-123", "headers": {"Authorization": "Bearer x"}}),
    );
    assert_eq!(
        v,
        json!({"cmd": "add_webhook", "url": "https://example.com/…", "headers": {"Authorization": "<hidden>"}})
    );
    assert_eq!(
        super::mask_url("https://user:pass@api.example.com/bot123:TOKEN/send?key=1"),
        "https://api.example.com/…"
    );
    assert!(super::plain_http("http://example.com/hook"));
    assert!(super::plain_http("http://localhost.example.com/hook"));
    for local in [
        "https://example.com/hook",
        "http://localhost:8080/hook",
        "http://127.0.0.1/hook",
        "http://[::1]:9/x",
        "http://localhost",
    ] {
        assert!(!super::plain_http(local), "{local}");
    }
}

#[test]
fn tiny_screens_draw_nothing_instead_of_panicking() {
    let mut st = super::State {
        sidebar: 24,
        prefix: true,
        ..Default::default()
    };
    st.usage = [Some(crate::usage::Week {
        used: 50.0,
        resets_at: super::now_secs() + 1000,
        window: crate::usage::WEEK,
    }); 3];
    for (cols, rows) in [
        (80, 0),
        (80, 1),
        (80, 3),
        (80, 5),
        (80, 6),
        (80, 9),
        (2, 30),
        (12, 30),
    ] {
        let mut t = ratatui::Terminal::new(ratatui::backend::TestBackend::new(cols, rows)).unwrap();
        t.draw(|f| super::draw(f, &st)).unwrap();
    }
}

/// 起動し直しで止めた前のエージェントの SessionEnd が、再開した会話の SessionStart の前後どちらに届いても、会話 ID を忘れない
#[test]
fn stale_session_end_after_restart_keeps_the_resumed_session() {
    let _fork = crate::FORK_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let s: super::Shared = Default::default();
    let dir = std::env::temp_dir().join(format!("mdeck-stale-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    super::lock(&s).sock = dir.join("t.sock").to_string_lossy().into_owned();
    let api = |req: serde_json::Value| super::api(&s, &req);
    let id = api(json!({"cmd": "open_tab", "cwd": dir.to_str().unwrap(), "run": ""})).unwrap()["tab"]["id"].as_u64().unwrap();
    let hook = |event: &str, reason: &str| {
        api(json!({"cmd": "hook", "agent": "claude", "tab": id, "payload": {"hook_event_name": event, "session_id": "main", "reason": reason}})).unwrap()
    };
    let now = || {
        let st = super::lock(&s);
        (
            st.tabs[0].session.clone(),
            st.tabs[0].agent.clone(),
            st.tabs[0].starting,
            st.tabs[0].state,
        )
    };
    let kept = |starting, state| {
        (
            Some("main".to_string()),
            Some("claude".to_string()),
            starting,
            state,
        )
    };
    // restore が再開したタブ
    {
        let mut st = super::lock(&s);
        let t = &mut st.tabs[0];
        (t.session, t.agent, t.starting, t.restored) = (
            Some("main".into()),
            Some("claude".into()),
            true,
            Some(std::time::Instant::now()),
        );
    }
    hook("SessionEnd", "other");
    assert_eq!(
        now(),
        kept(true, None),
        "再開した会話が始まる前の SessionEnd で忘れた"
    );
    hook("SessionStart", "");
    hook("SessionEnd", "other");
    assert_eq!(
        now(),
        kept(false, Some("idle")),
        "再開した直後に届いた前のエージェントの SessionEnd で忘れた"
    );
    // しばらく経った後の終了と、利用者が終わらせたものは終了として扱う
    super::lock(&s).tabs[0].restored = std::time::Instant::now()
        .checked_sub(super::RESTORE_GRACE + std::time::Duration::from_secs(1));
    assert!(super::lock(&s).tabs[0].restored.is_some());
    hook("SessionEnd", "other");
    assert_eq!(now(), (None, None, false, None));
    {
        let mut st = super::lock(&s);
        let t = &mut st.tabs[0];
        (t.session, t.agent, t.restored) = (
            Some("main".into()),
            Some("claude".into()),
            Some(std::time::Instant::now()),
        );
    }
    hook("SessionEnd", "prompt_input_exit");
    assert_eq!(now(), (None, None, false, None));
    let _ = api(json!({"cmd": "close_tab", "tab": id}));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn test_buttons_report_each_webhook_and_read_the_popup_permission() {
    use std::io::{Read, Write};
    let s: super::Shared = Default::default();
    let dir = std::env::temp_dir().join(format!("mdeck-testbtn-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    super::lock(&s).sock = dir.join("t.sock").to_string_lossy().into_owned();
    let srv = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    // 何も待ち受けていないポート
    let dead = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap();
    let api = |req: serde_json::Value| super::api(&s, &req).unwrap();
    let a_addr = srv.local_addr().unwrap();
    api(json!({"cmd": "add_webhook", "owner": "botA", "url": format!("http://{a_addr}/id-a")}));
    api(json!({"cmd": "add_webhook", "owner": "botB", "url": format!("http://{dead}/id-b")}));
    let t = std::thread::spawn(move || {
        let (mut c, _) = srv.accept().unwrap();
        let mut got = Vec::new();
        let mut buf = [0u8; 4096];
        while !String::from_utf8_lossy(&got).contains("Test notification") {
            let n = c.read(&mut buf).unwrap();
            got.extend_from_slice(&buf[..n]);
        }
        let _ = c.write_all(
            b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        );
        String::from_utf8_lossy(&got).into_owned()
    });
    // 選んだ通知先 (botA) だけに送る。botB には送らない
    let lines = super::test_one_webhook(&s, "botA", &format!("http://{a_addr}/id-a"));
    assert!(
        t.join().unwrap().contains(r#""event":"test""#),
        "event: test で送っていない"
    );
    assert_eq!(lines[0], "Test sent to botA (event: test)");
    assert!(
        lines[1].starts_with("ok  HTTP 204  http://127.0.0.1:"),
        "{lines:?}"
    );
    assert!(!lines.join(" ").contains("id-a"), "URL を伏せていない");
    let lines = super::test_one_webhook(&s, "botB", &format!("http://{dead}/id-b"));
    assert!(
        lines[1].starts_with("failed  ") && lines[1].contains("curl"),
        "{lines:?}"
    );
    // 登録していない URL は送らずに断る。url を渡さない今までの呼び方はそのまま
    let r = super::api(
        &s,
        &json!({"cmd": "test_webhooks", "owner": "botA", "url": "http://127.0.0.1:9/other"}),
    );
    assert!(r.unwrap_err().contains("not_found"));
    // 設定の並び: 通知の下と通知先の下にテストの行がある
    let st = super::lock(&s);
    let rows = super::menu_choices(&st);
    let at = |r: super::Row| rows.iter().position(|x| *x == r).unwrap();
    assert_eq!(at(super::Row::TestAlert), at(super::Row::Quiet) + 1);
    drop(st);
    let prefs = |flags: u64| {
        format!(
            "<dict><key>bundle-id</key><string>studio.tsukumi.MornDeck</string><key>flags</key><integer>{flags}</integer></dict><dict><key>bundle-id</key><string>other</string><key>flags</key><integer>0</integer></dict>"
        )
    };
    assert_eq!(
        super::ncprefs_allows(&prefs(1 << 25 | 14), super::POPUP_APP),
        Some(true)
    );
    assert_eq!(
        super::ncprefs_allows(&prefs(14), super::POPUP_APP),
        Some(false)
    );
    assert_eq!(
        super::ncprefs_allows("<dict></dict>", super::POPUP_APP),
        None
    );
    let _ = std::fs::remove_dir_all(dir);
}

/// codex のフックは環境変数のタブ番号を当てにせず、会話 ID と cwd からタブを決める
#[test]
fn codex_hooks_find_their_tab_by_session_and_cwd() {
    let _fork = crate::FORK_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let s: super::Shared = Default::default();
    let dir = std::env::temp_dir().join(format!("mdeck-codex-{}", std::process::id()));
    let (da, db) = (dir.join("a"), dir.join("b"));
    std::fs::create_dir_all(&da).unwrap();
    std::fs::create_dir_all(&db).unwrap();
    super::lock(&s).sock = dir.join("t.sock").to_string_lossy().into_owned();
    let api = |req: serde_json::Value| super::api(&s, &req).unwrap();
    let open = |d: &std::path::Path| {
        api(json!({"cmd": "open_tab", "cwd": d.to_str().unwrap(), "run": ""}))["tab"]["id"]
            .as_u64()
            .unwrap() as u32
    };
    let (a, b, shell) = (open(&da), open(&db), open(&da));
    // a と b の前面で codex が動いている (shell は同じ cwd だが codex ではない)
    let codex_runs = |ids: &[u32]| {
        let st = super::lock(&s);
        for t in st.tabs.iter().filter(|t| ids.contains(&t.id)) {
            t.fg.set((Some("codex"), Some(std::time::Instant::now())));
        }
        for t in st.tabs.iter().filter(|t| !ids.contains(&t.id)) {
            t.fg.set((None, Some(std::time::Instant::now())));
        }
    };
    let hook = |tab: &str, event: &str, session: &str, cwd: &std::path::Path| {
        codex_runs(&[a, b]);
        api(
            json!({"cmd": "hook", "agent": "codex", "tab": tab, "payload": {"hook_event_name": event, "session_id": session, "cwd": cwd.to_str().unwrap(), "last_assistant_message": "PONG"}}),
        )
    };
    let now = |id: u32| {
        let mut st = super::lock(&s);
        let t = st.tab(id).unwrap();
        (
            t.agent.clone(),
            t.state,
            t.session.clone(),
            t.answer.clone(),
        )
    };
    // 環境変数は別のタブ (a) を指しているが、cwd が b のものなので b に入る
    hook(&a.to_string(), "SessionStart", "s-b", &db);
    assert_eq!(
        now(b),
        (Some("codex".into()), Some("idle"), Some("s-b".into()), None)
    );
    assert_eq!(now(a).0, None, "環境変数のタブ番号を信じた");
    // 最初の指示を送った後に届く SessionStart で、作業中を idle に戻さない
    super::lock(&s).tab(b).unwrap().session = None;
    super::set_state(&s, &mut super::lock(&s), b, Some("working"));
    hook("", "SessionStart", "s-b", &db);
    assert_eq!((now(b).1, now(b).2), (Some("working"), Some("s-b".into())));
    // 番号が空でも、会話 ID で同じタブに入る
    hook("", "Stop", "s-b", &db);
    assert_eq!((now(b).1, now(b).3), (Some("done"), Some("PONG".into())));
    hook("", "SessionStart", "s-a", &da);
    assert_eq!(
        (now(a).2, now(shell).0),
        (Some("s-a".into()), None),
        "codex の動いていないタブに入れた"
    );
    // どのタブにも当たらない会話 (MornDeck の外の codex) は無視する
    let r = hook("", "Stop", "outside", &dir);
    assert!(r["ignored"].is_string(), "{r}");
    // しばらく打ち込んでいないタブには、同じ cwd で始まった外の会話 (デスクトップアプリなど) を結び付けない
    let old = std::time::Instant::now()
        .checked_sub(super::CODEX_BIND + std::time::Duration::from_secs(1));
    assert!(old.is_some());
    super::lock(&s).tab(b).unwrap().typed = old;
    let r = hook("", "SessionStart", "outside-b", &db);
    assert!(
        r["ignored"].is_string() && now(b).2 == Some("s-b".into()),
        "打ち込んでいないタブの会話を外の会話に付け替えた: {r}"
    );
    super::lock(&s).tab(b).unwrap().typed = Some(std::time::Instant::now());
    // 同じタブで始めた新しい会話は SessionStart で入れ替わる
    hook("", "SessionStart", "s-b2", &db);
    assert_eq!(now(b).2, Some("s-b2".into()));
    // 会話の決まっていないタブに、会話の始まり以外 (Stop など) は結び付けない
    let c = open(&db);
    codex_runs(&[a, b, c]);
    let r = api(
        json!({"cmd": "hook", "agent": "codex", "tab": "", "payload": {"hook_event_name": "Stop", "session_id": "outside-c", "cwd": db.to_str().unwrap(), "last_assistant_message": "x"}}),
    );
    assert!(r["ignored"].is_string() && now(c).0.is_none(), "{r}");
    // 同じ cwd の、会話の決まっていない 2 つのタブでは、環境変数の番号 (c) ではなく最後に打ち込んだタブ (d) に入る
    let d = open(&db);
    super::lock(&s).tab(c).unwrap().typed =
        std::time::Instant::now().checked_sub(std::time::Duration::from_secs(5));
    codex_runs(&[a, b, c, d]);
    api(
        json!({"cmd": "hook", "agent": "codex", "tab": c.to_string(), "payload": {"hook_event_name": "SessionStart", "session_id": "s-d", "cwd": db.to_str().unwrap()}}),
    );
    assert_eq!((now(d).2, now(c).2), (Some("s-d".into()), None));
    let v = api(json!({"cmd": "version"}));
    assert!(
        v["received"]["codex"]["n"].as_u64() > Some(0)
            && v["received"]["codex"]["ignored"].as_u64() > Some(0),
        "{v}"
    );
    for id in [a, b, shell, c, d] {
        let _ = api(json!({"cmd": "close_tab", "tab": id}));
    }
    let _ = std::fs::remove_dir_all(dir);
}

/// 送った指示の表示 (❯ で始まり、折り返しが同じ列に並ぶ) を選択肢として読まない。待ちの間は前の回答を返さない
#[test]
fn prompt_echo_is_not_a_menu_and_waiting_hides_the_old_answer() {
    let lines = |s: &str| s.lines().map(str::to_string).collect::<Vec<_>>();
    let echo = lines(
        "❯ Use the AskUserQuestion tool to ask me to pick\n  between the options Apple and Banana, then reply with\n  only the option I picked.\n\n⏺ Banana\n\n────────────\n❯ \n────────────\n  ? for shortcuts",
    );
    assert!(
        super::parse_options(&echo).is_empty(),
        "{:?}",
        super::parse_options(&echo)
    );
    let menu = lines(
        " Quick safety check\n\n ❯ No, exit\n   Yes, I trust this folder\n\n Enter to confirm · Esc to cancel",
    );
    assert_eq!(super::parse_options(&menu).len(), 2);

    let _fork = crate::FORK_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let s: super::Shared = Default::default();
    let dir = std::env::temp_dir().join(format!("mdeck-shape-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    super::lock(&s).sock = dir.join("t.sock").to_string_lossy().into_owned();
    let api = |req: serde_json::Value| super::api(&s, &req).unwrap();
    let id = api(json!({"cmd": "open_tab", "cwd": dir.to_str().unwrap(), "run": ""}))["tab"]["id"]
        .as_u64()
        .unwrap();
    let hook = |event: &str| {
        api(
            json!({"cmd": "hook", "agent": "claude", "tab": id, "payload": {"hook_event_name": event, "session_id": "m", "last_assistant_message": "PONG"}}),
        )
    };
    hook("SessionStart");
    hook("Stop");
    assert_eq!(
        api(json!({"cmd": "get_answer", "tab": id}))["answer"],
        "PONG"
    );
    hook("PermissionRequest");
    let r = api(json!({"cmd": "get_answer", "tab": id}));
    assert_eq!(
        (
            r["state"].as_str(),
            r["answer"].is_null(),
            r["seq"].as_u64()
        ),
        (Some("waiting"), true, Some(1)),
        "{r}"
    );
    // 質問で止まっているタブには force 無しで打ち込まず、今の質問と選択肢を付けて断る
    let r = api(json!({"cmd": "send", "tab": id, "text": "hello", "wait": false}));
    assert_eq!(
        (
            r["ok"].as_bool(),
            r["error"]["code"].as_str(),
            r["state"].as_str()
        ),
        (Some(false), Some("tab_waiting"), Some("waiting")),
        "{r}"
    );
    assert!(r["options"].is_array() && r["question"].is_object(), "{r}");
    assert!(
        !super::screen_lines(&super::lock(&s).tabs[0].term)
            .join(" ")
            .contains("hello"),
        "断ったのに打ち込んだ"
    );
    let r = api(json!({"cmd": "send", "tab": id, "text": "hello", "wait": false, "force": true}));
    assert_eq!(r["ok"], true, "{r}");
    let _ = api(json!({"cmd": "close_tab", "tab": id}));
    let _ = std::fs::remove_dir_all(dir);
}

/// 呼んだ側のタブ (caller) には、force 無しで打ち込まない・閉じない。一覧では self が付く
#[test]
fn caller_tab_is_protected_from_itself() {
    let _fork = crate::FORK_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let s: super::Shared = Default::default();
    let dir = std::env::temp_dir().join(format!("mdeck-self-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    super::lock(&s).sock = dir.join("t.sock").to_string_lossy().into_owned();
    let api = |req: serde_json::Value| super::api(&s, &req);
    let open = || {
        api(json!({"cmd": "open_tab", "cwd": dir.to_str().unwrap(), "run": "", "owner": "bot"}))
            .unwrap()["tab"]["id"]
            .as_u64()
            .unwrap()
    };
    let (me, other) = (open(), open());
    let code = |r: Result<serde_json::Value, String>| {
        super::err_json(&r.unwrap_err())["error"]["code"]
            .as_str()
            .unwrap()
            .to_string()
    };
    let tabs = api(json!({"cmd": "list_tabs", "caller": me})).unwrap();
    assert_eq!(
        (
            tabs["tabs"][0]["self"].as_bool(),
            tabs["tabs"][1]["self"].as_bool()
        ),
        (Some(true), None)
    );
    for cmd in [
        json!({"cmd": "send", "text": "x"}),
        json!({"cmd": "press_keys", "keys": "x"}),
        json!({"cmd": "choose", "n": 1}),
        json!({"cmd": "close_tab"}),
    ] {
        let mut req = cmd.clone();
        req["tab"] = json!(me);
        req["caller"] = json!(me);
        assert_eq!(code(api(req)), "self_tab", "{cmd}");
    }
    // caller の無い呼び出し (古いクライアントや人の操作) と、ほかのタブへは今までどおり
    assert!(api(json!({"cmd": "press_keys", "tab": me, "keys": " "})).is_ok());
    assert!(api(json!({"cmd": "press_keys", "tab": other, "keys": " ", "caller": me})).is_ok());
    assert!(
        api(json!({"cmd": "press_keys", "tab": me, "keys": " ", "caller": me, "force": true}))
            .is_ok()
    );
    // owner でまとめて閉じても、自分のタブは残る
    let r = api(json!({"cmd": "close_tab", "owner": "bot", "caller": me})).unwrap();
    assert_eq!(
        (r["closed"].clone(), r["skipped_self"].clone()),
        (json!([other]), json!([me]))
    );
    let _ = api(json!({"cmd": "close_tab", "tab": me}));
    let _ = std::fs::remove_dir_all(dir);
}

/// settings.json とプラグインの両方から同じフックが届いても、状態・回答・seq・通知は 1 回。同じ経路から続けて届いたものは別のイベント
#[test]
fn hooks_from_settings_and_plugin_count_once() {
    let _fork = crate::FORK_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let s: super::Shared = Default::default();
    let dir = std::env::temp_dir().join(format!("mdeck-dup-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    super::lock(&s).sock = dir.join("t.sock").to_string_lossy().into_owned();
    let api = |req: serde_json::Value| super::api(&s, &req).unwrap();
    let id = api(json!({"cmd": "open_tab", "cwd": dir.to_str().unwrap(), "run": ""}))["tab"]["id"]
        .as_u64()
        .unwrap();
    // 流れたイベント (subscribe と webhook に出るもの) を数える
    let (tx, rx) = std::sync::mpsc::channel();
    super::lock(&s).subs.push(tx);
    let hook = |via: Option<&str>, event: &str, msg: &str| {
        let mut req = json!({"cmd": "hook", "agent": "claude", "tab": id, "payload": {"hook_event_name": event, "session_id": "m", "last_assistant_message": msg}});
        if let Some(v) = via {
            req["via"] = json!(v);
        }
        api(req)
    };
    let seq = || super::lock(&s).tabs[0].answer_seq;
    for via in [None, Some("plugin")] {
        hook(via, "SessionStart", "");
    }
    for via in [None, Some("plugin")] {
        hook(via, "UserPromptSubmit", "");
    }
    // プラグインのほうが先に届いても同じ
    let r = hook(Some("plugin"), "Stop", "PONG");
    assert!(r["ignored"].is_null(), "{r}");
    let r = hook(None, "Stop", "PONG");
    assert_eq!(r["ignored"], "duplicate from another hook source");
    assert_eq!(seq(), 1, "1 回の完了を 2 回数えた");
    let done: Vec<serde_json::Value> = rx
        .try_iter()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(&l).ok())
        .filter(|e| e["event"] == "state" && e["state"] == "done")
        .collect();
    assert_eq!(done.len(), 1, "done の通知が二重: {done:?}");
    // 同じ経路から同じ中身が続けて届いたら、別の完了として数える
    hook(None, "UserPromptSubmit", "");
    hook(None, "Stop", "PONG");
    hook(None, "UserPromptSubmit", "");
    hook(None, "Stop", "PONG");
    assert_eq!(seq(), 3);
    // フックが届いたタブは「no hooks」の対象にならない
    {
        let mut st = super::lock(&s);
        let t = &mut st.tabs[0];
        t.fg_since
            .set(std::time::Instant::now().checked_sub(std::time::Duration::from_secs(60)));
        t.fg.set((Some("claude"), Some(std::time::Instant::now())));
        assert_eq!(super::silent(t), None);
        t.hooked = false;
        assert_eq!(super::silent(t), Some("claude"));
        // codex は最初の指示までフックを送らないので「no hooks」にしない
        t.fg.set((Some("codex"), Some(std::time::Instant::now())));
        assert_eq!(super::silent(t), None);
        t.fg.set((Some("claude"), Some(std::time::Instant::now())));
    }
    let v = api(json!({"cmd": "version"}));
    assert_eq!(v["silent_tabs"][0]["agent"], "claude", "{v}");
    let _ = api(json!({"cmd": "close_tab", "tab": id}));
    let _ = std::fs::remove_dir_all(dir);
}
