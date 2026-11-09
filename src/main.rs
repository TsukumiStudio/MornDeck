mod hooks;
mod server;

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
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
        Some("hook") => {
            hooks::run(args.get(1).map(String::as_str).unwrap_or("agent"));
            Ok(())
        }
        Some(_) => {
            eprintln!("usage: mdeck [attach|server|api '<json>']");
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

static WINCH: AtomicBool = AtomicBool::new(false);
extern "C" fn on_winch(_: libc::c_int) {
    WINCH.store(true, Ordering::Relaxed);
}

fn attach(sock: &str) -> std::io::Result<()> {
    let mut conn = connect(sock)?;
    let (cols, rows) = terminal::size()?;
    writeln!(conn, "{}", json!({"cmd": "attach", "cols": cols, "rows": rows}))?;
    terminal::enable_raw_mode()?;
    let mut out = std::io::stdout();
    out.write_all(b"\x1b[?1049h\x1b[?1002h\x1b[?1006h")?;
    out.flush()?;

    unsafe { libc::signal(libc::SIGWINCH, on_winch as *const () as libc::sighandler_t) };
    let mut wc = conn.try_clone()?;
    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_millis(50));
        if WINCH.swap(false, Ordering::Relaxed)
            && let Ok((c, r)) = terminal::size() {
                let mut m = vec![server::IN_RESIZE];
                m.extend(c.to_be_bytes());
                m.extend(r.to_be_bytes());
                if wc.write_all(&m).is_err() {
                    break;
                }
            }
    });
    let mut wi = conn.try_clone()?;
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
            if wi.write_all(&m).is_err() {
                break;
            }
        }
    });

    let mut buf = vec![0u8; 65536];
    while let Ok(n) = conn.read(&mut buf) {
        if n == 0 || out.write_all(&buf[..n]).and_then(|_| out.flush()).is_err() {
            break;
        }
    }
    let _ = out.write_all(b"\x1b[?1002l\x1b[?1006l\x1b[?2004l\x1b[?1l\x1b[?25h\x1b[?1049l");
    let _ = out.flush();
    let _ = terminal::disable_raw_mode();
    std::process::exit(0);
}

fn api(sock: &str, req: &str) -> std::io::Result<()> {
    let v: Value = serde_json::from_str(req).map_err(std::io::Error::other)?;
    let mut c = connect(sock)?;
    writeln!(c, "{v}")?;
    for l in BufReader::new(c).lines() {
        println!("{}", l?);
    }
    Ok(())
}
