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
        None | Some("attach") => attach(&sock),
        Some("server") => server::run(&sock),
        Some("api") => api(&sock, args.get(1).map(String::as_str).unwrap_or("")),
        Some("hook") => {
            hook(args.get(1).map(String::as_str).unwrap_or("agent"));
            Ok(())
        }
        Some("install-hooks") => install_hooks(),
        Some(_) => {
            eprintln!("usage: mdeck [attach|server|api '<json>'|hook <claude|codex>|install-hooks]");
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
    out.write_all(b"\x1b[?1049h\x1b[?1000h\x1b[?1006h")?;
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
    let _ = out.write_all(b"\x1b[?1000l\x1b[?1006l\x1b[?2004l\x1b[?1l\x1b[?25h\x1b[?1049l");
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

/// claude / codex のフックから呼ばれる。エージェントを止めないよう、何があっても無言で終わる。
fn hook(agent: &str) {
    let mut input = String::new();
    let _ = std::io::stdin().read_to_string(&mut input);
    let (Ok(tab), Ok(sock)) = (std::env::var("MDECK_TAB"), std::env::var("MDECK_SOCK")) else { return };
    let Ok(tab) = tab.parse::<u32>() else { return };
    let v: Value = serde_json::from_str(&input).unwrap_or(Value::Null);
    let Some(event) = v["hook_event_name"].as_str() else { return };
    let Ok(mut c) = UnixStream::connect(sock) else { return };
    let _ = c.set_read_timeout(Some(Duration::from_millis(500)));
    let _ = writeln!(c, "{}", json!({"cmd": "hook", "tab": tab, "agent": agent, "event": event}));
    let _ = c.read(&mut [0u8; 256]);
}

const HOOK_EVENTS: [&str; 7] =
    ["SessionStart", "UserPromptSubmit", "PreToolUse", "PostToolUse", "PermissionRequest", "Stop", "SessionEnd"];

fn install_hooks() -> std::io::Result<()> {
    let exe = std::env::current_exe()?.to_string_lossy().to_string();
    for (agent, path) in [("claude", ".claude/settings.json"), ("codex", ".codex/hooks.json")] {
        let path = home().join(path);
        let mut root: Value = match std::fs::read_to_string(&path) {
            Ok(s) => serde_json::from_str(&s).map_err(std::io::Error::other)?,
            Err(_) => json!({}),
        };
        add_hooks(&mut root, &format!("'{exe}' hook {agent}"));
        std::fs::write(&path, serde_json::to_string_pretty(&root)? + "\n")?;
        println!("{}", path.display());
    }
    Ok(())
}

fn add_hooks(root: &mut Value, command: &str) {
    if !root["hooks"].is_object() {
        root["hooks"] = json!({});
    }
    for ev in HOOK_EVENTS {
        let list = &mut root["hooks"][ev];
        if !list.is_array() {
            *list = json!([]);
        }
        let arr = list.as_array_mut().unwrap();
        if arr.iter().any(|g| g.to_string().contains(" hook ") && g.to_string().contains("mdeck")) {
            continue;
        }
        arr.push(json!({"hooks": [{"type": "command", "command": command, "timeout": 5}]}));
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn add_hooks_keeps_existing_and_is_idempotent() {
        let mut v = serde_json::json!({"model": "x", "hooks": {"Stop": [{"hooks": [{"command": "other"}]}]}});
        super::add_hooks(&mut v, "'/bin/mdeck' hook claude");
        super::add_hooks(&mut v, "'/bin/mdeck' hook claude");
        assert_eq!(v["model"], "x");
        assert_eq!(v["hooks"]["Stop"].as_array().unwrap().len(), 2);
        assert_eq!(v["hooks"]["SessionStart"].as_array().unwrap().len(), 1);
    }
}
