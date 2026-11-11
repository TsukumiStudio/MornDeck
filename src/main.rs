mod hooks;
mod mcp;
mod server;
mod usage;

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ratatui::crossterm::terminal;
use serde_json::{json, Value};

fn home() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/".into()))
}

fn sock_path() -> String {
    if let Ok(s) = std::env::var("MDECK_SOCK") {
        return s;
    }
    let dir = home().join(".mdeck");
    let _ = std::fs::create_dir_all(&dir);
    let _ = std::fs::set_permissions(&dir, std::os::unix::fs::PermissionsExt::from_mode(0o700));
    dir.join("mdeck.sock").to_string_lossy().into()
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let sock = sock_path();
    let r = match args.first().map(String::as_str) {
        None => attach(&sock),
        Some("server") => server::run(&sock),
        Some("--version") => {
            println!("mdeck {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        Some("api") => api(&sock, args.get(1).map(String::as_str).unwrap_or("")),
        Some("mcp") => mcp::run(&sock),
        Some("hook") => {
            hooks::run(args.get(1).map(String::as_str).unwrap_or("agent"));
            Ok(())
        }
        Some(_) => {
            eprintln!("usage: mdeck [server|api '<json>'|mcp|--version]");
            std::process::exit(2);
        }
    };
    if let Err(e) = r {
        eprintln!("mdeck: {e}");
        std::process::exit(1);
    }
}

fn connect(sock: &str) -> std::io::Result<UnixStream> {
    if let Ok(c) = UnixStream::connect(sock) {
        return Ok(c);
    }
    if start_in_gui_session(sock) {
        // 初回の起動は macOS の検査で時間がかかることがあるので長めに待つ
        for _ in 0..200 {
            std::thread::sleep(Duration::from_millis(50));
            if let Ok(c) = UnixStream::connect(sock) {
                return Ok(c);
            }
        }
    }
    let log = std::fs::File::create(std::path::Path::new(sock).with_extension("log"))?;
    let mut cmd = Command::new(std::env::current_exe()?);
    cmd.arg("server").stdin(Stdio::null()).stdout(Stdio::null()).stderr(log);
    // 起動した端末や ssh セッションが閉じても巻き添えで終了しないよう新しいセッションにする
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    cmd.spawn()?;
    for _ in 0..50 {
        std::thread::sleep(Duration::from_millis(50));
        if let Ok(c) = UnixStream::connect(sock) {
            return Ok(c);
        }
    }
    UnixStream::connect(sock)
}

/// ssh から起動したときは、サーバーを Mac の画面側 (ログイン中のセッション) に立てる。
/// ssh のセッションのプロセスはキーチェーンを読めず、claude / copilot の使用量が取れないため。
/// MornDeck.app を open で起動する (LSUIElement なので窓も Dock のアイコンも出ない)。
/// .app の中で動いていない (手元のビルド) か、画面側にログインしていなくて open に失敗したら false
fn start_in_gui_session(sock: &str) -> bool {
    if std::env::var_os("SSH_CONNECTION").is_none() {
        return false;
    }
    let Ok(exe) = std::env::current_exe().and_then(|e| e.canonicalize()) else { return false };
    // .../MornDeck.app/Contents/MacOS/mdeck
    let Some(app) = exe.ancestors().nth(3).filter(|a| a.extension().is_some_and(|e| e == "app")) else { return false };
    Command::new("/usr/bin/open")
        .args(["-n", "-g", "-a"])
        .arg(app)
        .args(["--env", &format!("MDECK_SOCK={sock}"), "--args", "server"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

static WINCH: AtomicBool = AtomicBool::new(false);
extern "C" fn on_winch(_: libc::c_int) {
    WINCH.store(true, Ordering::Relaxed);
}

fn attach(sock: &str) -> std::io::Result<()> {
    let mut conn = connect(sock)?;
    send_attach(&mut conn)?;
    terminal::enable_raw_mode()?;
    let mut out = std::io::stdout();
    out.write_all(b"\x1b[?1049h\x1b[?1003h\x1b[?1006h")?;
    out.flush()?;

    // 入力と端末サイズは、つなぎ直した後も今の接続へ送る
    let writer = Arc::new(Mutex::new(conn.try_clone()?));
    unsafe { libc::signal(libc::SIGWINCH, on_winch as *const () as libc::sighandler_t) };
    let wc = writer.clone();
    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_millis(50));
        if WINCH.swap(false, Ordering::Relaxed)
            && let Ok((c, r)) = terminal::size() {
                let mut m = vec![server::IN_RESIZE];
                m.extend(c.to_be_bytes());
                m.extend(r.to_be_bytes());
                let _ = lock(&wc).write_all(&m);
            }
    });
    let wi = writer.clone();
    std::thread::spawn(move || {
        let mut stdin = std::io::stdin();
        let mut buf = [0u8; 8192];
        while let Ok(n) = stdin.read(&mut buf) {
            if n == 0 {
                break;
            }
            let mut m = vec![server::IN_INPUT];
            m.extend((n as u32).to_be_bytes());
            m.extend(&buf[..n]);
            let _ = lock(&wi).write_all(&m);
        }
    });

    let mut buf = vec![0u8; 65536];
    loop {
        let mut restarting = false;
        while let Ok(n) = conn.read(&mut buf) {
            if n == 0 || out.write_all(&buf[..n]).and_then(|_| out.flush()).is_err() {
                break;
            }
            restarting |= contains(&buf[..n], server::RESTART_MARK);
        }
        // 離脱やサーバーの終了ではなく、サーバーの入れ替えならつなぎ直す
        let Some(mut c) = restarting.then(|| reconnect(sock)).flatten() else { break };
        if send_attach(&mut c).is_err() {
            break;
        }
        *lock(&writer) = c.try_clone()?;
        conn = c;
    }
    let _ = out.write_all(b"\x1b[?1003l\x1b[?1002l\x1b[?1006l\x1b[?2004l\x1b[?1l\x1b[?25h\x1b[?1049l");
    let _ = out.flush();
    let _ = terminal::disable_raw_mode();
    std::process::exit(0);
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn send_attach(conn: &mut UnixStream) -> std::io::Result<()> {
    let (cols, rows) = terminal::size()?;
    writeln!(conn, "{}", json!({"cmd": "attach", "cols": cols, "rows": rows}))
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

/// 入れ替え中のサーバーが同じソケットで起動するのを待つ。自分で起動すると 2 つが同じソケットを取り合うので、
/// しばらくは待つだけにし、それでも出てこなければ起動する。
fn reconnect(sock: &str) -> Option<UnixStream> {
    for _ in 0..200 {
        std::thread::sleep(Duration::from_millis(100));
        if let Ok(c) = UnixStream::connect(sock) {
            return Some(c);
        }
    }
    connect(sock).ok()
}

fn api(sock: &str, req: &str) -> std::io::Result<()> {
    let v: Value = match serde_json::from_str(req) {
        Ok(v) => v,
        Err(e) => {
            println!("{}", json!({"ok": false, "error": {"code": "invalid", "message": format!("JSON として読めない: {e}")}}));
            std::process::exit(1);
        }
    };
    let mut c = connect(sock)?;
    writeln!(c, "{v}")?;
    // 終了コードで成否が分かるよう、ok が true でなければ 1 で終わる (subscribe は流れ続ける)
    let mut ok = true;
    for l in BufReader::new(c).lines() {
        let l = l?;
        ok &= serde_json::from_str::<Value>(&l).is_ok_and(|r| r["ok"] != false);
        println!("{l}");
    }
    if !ok {
        std::process::exit(1);
    }
    Ok(())
}

/// fork した子は CLOEXEC が付く前のソケットを引き継ぐことがあるので、ソケットを作るテストと子を起動するテストを並べない。
#[cfg(test)]
pub static FORK_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// 1 行送って 1 行受け取る。
pub fn request(sock: &str, req: &Value) -> std::io::Result<Value> {
    let mut c = connect(sock)?;
    writeln!(c, "{req}")?;
    let mut line = String::new();
    BufReader::new(c).read_line(&mut line)?;
    serde_json::from_str(&line).map_err(std::io::Error::other)
}
