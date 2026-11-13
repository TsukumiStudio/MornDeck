mod hooks;
mod mcp;
#[cfg(target_os = "macos")]
mod notify;
#[cfg(not(target_os = "macos"))]
#[path = "notify_linux.rs"]
mod notify;
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
use serde_json::{Value, json};

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
    let cmd = args.first().map(String::as_str);
    // 表示だけのものは ~/.mdeck を作らない
    let sock = if matches!(
        cmd,
        Some("--version" | "-V" | "--help" | "-h" | "--licenses")
    ) {
        String::new()
    } else {
        sock_path()
    };
    let r = match cmd {
        // 通知を押すと macOS が MornDeck.app (引数無しの mdeck) を起動する。端末が無ければ何もせずに終わる
        None if unsafe { libc::isatty(0) } == 0 => {
            eprintln!("mdeck: no terminal on stdin (run mdeck in a terminal; from scripts use mdeck tabs / mdeck api)");
            Ok(())
        }
        None => attach(&sock),
        Some("server") => server::run(&sock),
        Some("--version" | "-V") => {
            println!("mdeck {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        Some("--help" | "-h") => {
            print!("{HELP}");
            Ok(())
        }
        Some("--licenses") => {
            // less などが先に閉じても panic しない
            let _ = std::io::stdout().write_all(include_str!("../THIRD_PARTY_LICENSES").as_bytes());
            Ok(())
        }
        Some("api") => api(&sock, args.get(1).map(String::as_str).unwrap_or("")),
        Some("mcp") => match args.get(1).map(String::as_str) {
            Some("install") => mcp_clients(true),
            Some("uninstall") => mcp_clients(false),
            _ => mcp::run(&sock),
        },
        Some("hooks") => match args.get(1).map(String::as_str) {
            // 既定 (プラグイン。だめなら settings.json に入れて理由を出す)
            Some("install") => hooks::install_claude().map(|m| println!("{m}")).map_err(std::io::Error::other),
            Some("plugin") => hooks::choose_settings(false)
                .and_then(|_| hooks::install_plugin())
                .map(|_| println!("installed the claude hooks as a plugin. Running claude sessions report again after a restart (mdeck restart restarts every tab) or /reload-plugins"))
                .map_err(std::io::Error::other),
            // settings.json に入れてからプラグインを外す (どちらも無い状態にしない。重なる間は二重に数えない)
            Some("settings") if !hooks::agent_dir("claude").is_dir() => Err(std::io::Error::other(format!("claude is not installed (no {})", hooks::agent_dir("claude").display()))),
            Some("settings") => hooks::choose_settings(true)
                .and_then(|_| hooks::install("claude"))
                .and_then(|_| hooks::uninstall_plugin())
                .map(|_| println!("installed the claude hooks in settings.json and removed the plugin. `mdeck hooks plugin` switches back"))
                .map_err(std::io::Error::other),
            _ => usage(),
        },
        // サーバーが通知を出すときに、open で起動し直した MornDeck.app として呼ばれる (利用者は使わない)
        Some("notify") => {
            match notify::post_here(args.get(1).map(String::as_str).unwrap_or(""), args.get(2).map(String::as_str).unwrap_or("")) {
                Ok(()) => println!("ok"),
                Err(e) => println!("{e}"),
            }
            Ok(())
        }
        Some("guide") => {
            let _ = std::io::stdout().write_all(include_str!("../docs/agent-guide.md").as_bytes());
            Ok(())
        }
        Some("tabs") => tabs(&sock),
        Some("send") => {
            // --force は、作業中・質問で止まっているタブ・自分のタブにも送る
            let force = args.get(1).is_some_and(|a| a == "--force");
            let rest = &args[if force { 2 } else { 1 }..];
            match rest {
                [tab, text @ ..] if !text.is_empty() => api_value(
                    &sock,
                    json!({"cmd": "send", "tab": tab, "text": text.join(" "), "force": force, "timeout_ms": 1_800_000}),
                ),
                _ => usage(),
            }
        }
        Some("wait") => match args.get(1) {
            Some(tab) => {
                let secs = args
                    .get(2)
                    .and_then(|s| s.parse::<u64>().ok())
                    .unwrap_or(1800);
                api_value(
                    &sock,
                    json!({"cmd": "get_answer", "tab": tab, "timeout_ms": secs.saturating_mul(1000)}),
                )
            }
            None => usage(),
        },
        Some("kill") => kill(&sock),
        Some("restart") => restart(&sock),
        Some("doctor") => {
            doctor(&sock);
            Ok(())
        }
        Some("uninstall") => {
            let mut failed = false;
            for (agent, r) in hooks::uninstall() {
                match r {
                    Ok(true) => println!("removed hooks: {agent}"),
                    Ok(false) => {}
                    Err(e) => {
                        eprintln!("mdeck: cannot remove the {agent} hooks: {e}");
                        failed = true;
                    }
                }
            }
            let r = kill(&sock);
            if let Err(e) = hooks::uninstall_plugin() {
                eprintln!("mdeck: cannot remove the claude plugin: {e}");
                failed = true;
            }
            // 外の CLI を呼ぶので最後に (固まっても、フックの解除とサーバーの停止は済んでいる)
            let _ = mcp_clients(false);
            if failed {
                std::process::exit(1);
            }
            r
        }
        Some("hook") => {
            hooks::run(args.get(1).map(String::as_str).unwrap_or("agent"));
            Ok(())
        }
        Some(_) => usage(),
    };
    if let Err(e) = r {
        eprintln!("mdeck: {e}");
        std::process::exit(1);
    }
}

const HELP: &str = "\
usage: mdeck [command]

  (none)        open the screen (starts the server if needed)
  api '<json>'  call the API, e.g. mdeck api '{\"cmd\":\"list_tabs\"}'
  tabs          list the tabs
  send [--force] <tab> <text>   send text to the agent in a tab and wait for its reply (prints JSON)
  wait <tab> [secs]   wait until the agent in a tab replies or asks (default 1800 s)
  mcp           run as a stdio MCP server
  hooks install install the claude hooks: as a Claude Code plugin, or in settings.json if the plugin cannot be installed
  hooks plugin  install the claude hooks as a Claude Code plugin (the default)
  hooks settings      use settings.json for the claude hooks instead of the plugin
  mcp install   register the MCP server with claude, codex and copilot (mcp uninstall removes it)
  guide         print the guide for AI agents that control mdeck
  kill          stop the server and every tab
  restart       restart the server on the installed version (tabs resume their agents)
  doctor        show versions, hook status and whether hooks reach the server
  uninstall     remove the hooks from claude / codex / copilot and stop the server
  --licenses    print third-party licenses
  --version, -V
  --help, -h

https://github.com/TsukumiStudio/MornDeck
";

/// 動いているサーバーを止める (起動はしない)。タブは保存され、次に mdeck を開くと戻る
fn kill(sock: &str) -> std::io::Result<()> {
    let Ok(mut c) = UnixStream::connect(sock) else {
        println!("the server is not running");
        return Ok(());
    };
    writeln!(c, "{}", json!({"cmd": "shutdown"}))?;
    let mut line = String::new();
    BufReader::new(c).read_line(&mut line)?;
    // 0.3.3 までのサーバーは shutdown を知らない
    if serde_json::from_str::<Value>(&line).is_ok_and(|r| r["ok"] != true) {
        return Err(std::io::Error::other(
            "the running server is too old to stop; run `mdeck restart` first",
        ));
    }
    // タブを全部止めてからソケットを消すので、消えるまで待つ
    for _ in 0..600 {
        if !std::path::Path::new(sock).exists() {
            println!("stopped the server");
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Err(std::io::Error::other("the server did not stop"))
}

fn usage() -> std::io::Result<()> {
    eprint!("{HELP}");
    std::process::exit(2);
}

/// 呼んだ側のタブを付ける。サーバーは、自分のタブへの send や close_tab を force 無しでは断る
pub fn with_caller(mut req: Value) -> Value {
    if req.is_object()
        && req["caller"].is_null()
        && let Some(tab) = std::env::var("MDECK_TAB")
            .ok()
            .and_then(|t| t.parse::<u32>().ok())
    {
        req["caller"] = json!(tab);
    }
    req
}

fn tabs(sock: &str) -> std::io::Result<()> {
    let r = request(sock, &with_caller(json!({"cmd": "list_tabs"})))?;
    println!(
        "{:<4} {:<20} {:<8} {:<9} {:<14} cwd",
        "id", "name", "agent", "state", "owner"
    );
    for t in r["tabs"].as_array().into_iter().flatten() {
        let s = |k: &str| t[k].as_str().unwrap_or("-").to_string();
        let me = if t["self"] == true {
            "  <- this tab"
        } else {
            ""
        };
        println!(
            "{:<4} {:<20} {:<8} {:<9} {:<14} {}{me}",
            t["id"].to_string(),
            s("name"),
            s("agent"),
            s("state"),
            s("owner"),
            s("cwd")
        );
    }
    Ok(())
}

/// 入っているエージェントの CLI に、MCP サーバーとして mdeck を登録する (外す)。各 CLI の `mcp add` / `mcp remove` を呼ぶ
fn mcp_clients(install: bool) -> std::io::Result<()> {
    // brew のリンクを登録する (Caskroom の実体のパスは更新で消える)
    let exe = ["/opt/homebrew/bin/mdeck", "/usr/local/bin/mdeck"]
        .into_iter()
        .map(PathBuf::from)
        .find(|p| p.is_file())
        .or_else(|| std::env::current_exe().ok())
        .unwrap_or_else(|| "mdeck".into());
    let exe = exe.to_string_lossy();
    for agent in ["claude", "codex", "copilot"] {
        let mut args: Vec<&str> = vec!["mcp", if install { "add" } else { "remove" }];
        if agent == "claude" {
            args.extend(["-s", "user"]);
        }
        args.push("mdeck");
        if install {
            args.extend(["--", &exe, "mcp"]);
        }
        match Command::new(agent)
            .args(&args)
            .stdin(Stdio::null())
            .output()
        {
            Err(_) => println!("{agent}: not found, skipped"),
            Ok(o) if o.status.success() => println!(
                "{agent}: {}",
                if install { "registered" } else { "removed" }
            ),
            Ok(o) => {
                let out = format!(
                    "{}{}",
                    String::from_utf8_lossy(&o.stderr),
                    String::from_utf8_lossy(&o.stdout)
                );
                println!(
                    "{agent}: {}",
                    out.lines()
                        .find(|l| !l.trim().is_empty())
                        .unwrap_or("failed")
                        .trim()
                );
            }
        }
    }
    Ok(())
}

/// 状態が出ないときに見るもの: 版、サーバー、フックの導入状況、フックが届いているか。サーバーは起動しない
fn doctor(sock: &str) {
    let me = env!("CARGO_PKG_VERSION");
    println!(
        "mdeck      {me} ({})",
        std::env::current_exe()
            .map(|p| p.display().to_string())
            .unwrap_or_default()
    );
    println!("socket     {sock}");
    for key in ["CLAUDE_CONFIG_DIR", "CODEX_HOME", "MDECK_TAB"] {
        if let Ok(v) = std::env::var(key) {
            println!("env        {key}={v}");
        }
    }
    let server = UnixStream::connect(sock)
        .ok()
        .and_then(|_| request(sock, &json!({"cmd": "version"})).ok());
    let hooks = |list: Vec<(String, String, String)>| {
        for (agent, status, file) in list {
            println!("hooks      {agent:<8}{status}  ({file})");
        }
    };
    let mine = || {
        hooks::status()
            .into_iter()
            .zip(hooks::files())
            .map(|((a, s), (_, f))| (a.to_string(), s.label(a), f.display().to_string()))
            .collect()
    };
    match server {
        None => {
            println!("server     not running (run `mdeck` to start it)");
            hooks(mine());
        }
        // 0.4.1 までのサーバーは version を知らない
        Some(v) if v["ok"] != true => {
            println!(
                "server     running, older than 0.4.2. Run `mdeck restart` to switch it to {me}"
            );
            hooks(mine());
        }
        Some(v) => {
            let ver = v["version"].as_str().unwrap_or("?");
            let stale = if ver == me {
                String::new()
            } else {
                format!("  <- differs from mdeck {me}: run `mdeck restart`")
            };
            println!(
                "server     {ver}, pid {}, {} tabs{stale}",
                v["pid"], v["tabs"]
            );
            // フックの場所はサーバーの環境変数で決まるので、サーバーの見ているものを出す
            hooks(
                v["hooks"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|h| {
                        (
                            h["agent"].as_str().unwrap_or("").to_string(),
                            h["status"].as_str().unwrap_or("").to_string(),
                            h["file"].as_str().unwrap_or("").to_string(),
                        )
                    })
                    .collect(),
            );
            let (n, at) = (
                v["hooks_received"].as_u64().unwrap_or(0),
                v["last_hook_at"].as_u64().unwrap_or(0),
            );
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            match n {
                0 => println!("received   {}", no_hook_advice()),
                _ => println!(
                    "received   {n} hooks, the last one {}s ago",
                    now.saturating_sub(at)
                ),
            }
            // フックが一度も届いていないのに、エージェントが動いているタブ
            for t in v["silent_tabs"].as_array().into_iter().flatten() {
                println!(
                    "no hooks   tab {} {} ({}): the agent runs but no hook has arrived from this tab",
                    t["id"],
                    t["name"].as_str().unwrap_or("-"),
                    t["agent"].as_str().unwrap_or("?")
                );
            }
            // エージェントごとの内訳 (0.4.2 以降のサーバー)。ignored は、どのタブのものでもなかった codex のフック
            for (agent, r) in v["received"].as_object().into_iter().flatten() {
                println!(
                    "received   {agent:<8}{} matched to a tab, {} ignored",
                    r["n"], r["ignored"]
                );
            }
        }
    }
    // プラグインを入れられなかった理由 (起動時の移し替えや Enter で失敗したもの)
    if let Some(e) =
        hooks::plugin_failure().filter(|_| !hooks::plugin_installed() && !hooks::prefers_settings())
    {
        println!(
            "plugin     claude hooks stay in settings.json: {e}. Retry: mdeck hooks install. Keep settings.json: mdeck hooks settings"
        );
    }
    // 組織の管理設定で claude のフックが止められていないか (サーバーを通さずに、このシェルから見る)
    if let Some(p) = hooks::claude_policy() {
        let fix = match (p.plugin_helps, hooks::plugin_installed()) {
            (true, true) => "settings.json hooks are skipped; the hooks are installed as a plugin",
            (true, false) => {
                "settings.json hooks are skipped; run `mdeck hooks plugin`, then restart claude"
            }
            _ => "Claude Code runs no hooks; MornDeck cannot show claude state",
        };
        println!("policy     {}: {fix}", p.what);
    }
}

/// フックが 1 件も届いていないときの案内。フックの入り方と組織の設定に合わせる
fn no_hook_advice() -> String {
    let claude = hooks::status()
        .into_iter()
        .find(|(a, _)| *a == "claude")
        .map(|(_, s)| s);
    match claude {
        Some(hooks::Status::Blocked { plugin_helps: true }) => "no hook has arrived: an organization policy makes Claude Code skip hooks in settings.json. Run `mdeck hooks plugin`, then restart claude".into(),
        Some(hooks::Status::Blocked { plugin_helps: false }) => "no hook has arrived: an organization policy stops all Claude Code hooks, so mdeck cannot show claude state".into(),
        Some(hooks::Status::Missing | hooks::Status::Outdated(_)) => "no hook has arrived: install the hooks (`mdeck hooks install`, or F12, c, Hooks, Enter), then restart claude".into(),
        _ => "no hook has arrived yet. Hooks load when an agent starts: start or restart claude in a tab (`mdeck restart` restarts every tab)".into(),
    }
}

/// サーバーを今入っている版で起動し直す (brew upgrade の後など)。タブは保存され、エージェントの会話は再開される。
/// 0.3.x のサーバーにも効く。古いサーバーが新しい版を見つけられずに終わったときは、ここで起動する
fn restart(sock: &str) -> std::io::Result<()> {
    if UnixStream::connect(sock).is_err() {
        println!("the server is not running");
        return Ok(());
    }
    let r = request(sock, &json!({"cmd": "restart"}))?;
    if r["ok"] != true {
        return Err(std::io::Error::other(r["error"].to_string()));
    }
    for _ in 0..120 {
        std::thread::sleep(Duration::from_millis(500));
        // 入れ替わる前のサーバーにつながると、返事の前に切れる
        if let Ok(r) = request(sock, &json!({"cmd": "list_tabs"}))
            && let Some(tabs) = r["tabs"].as_array()
        {
            println!("restarted the server ({} tabs)", tabs.len());
            return Ok(());
        }
    }
    Err(std::io::Error::other("the server did not come back"))
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
    cmd.arg("server")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(log);
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
#[cfg(not(target_os = "macos"))]
fn start_in_gui_session(_: &str) -> bool {
    false
}

#[cfg(target_os = "macos")]
fn start_in_gui_session(sock: &str) -> bool {
    if std::env::var_os("SSH_CONNECTION").is_none() {
        return false;
    }
    let Ok(exe) = std::env::current_exe().and_then(|e| e.canonicalize()) else {
        return false;
    };
    // .../MornDeck.app/Contents/MacOS/mdeck
    let Some(app) = exe
        .ancestors()
        .nth(3)
        .filter(|a| a.extension().is_some_and(|e| e == "app"))
    else {
        return false;
    };
    let mut open = Command::new("/usr/bin/open");
    open.args(["-n", "-g", "-a"])
        .arg(app)
        .args(["--env", &format!("MDECK_SOCK={sock}")]);
    // open は環境を引き継がないので、設定フォルダの場所はここで渡す
    for key in ["CLAUDE_CONFIG_DIR", "CODEX_HOME"] {
        if let Some(v) = std::env::var(key).ok().filter(|v| !v.is_empty()) {
            open.args(["--env", &format!("{key}={v}")]);
        }
    }
    open.args(["--args", "server"])
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
    std::thread::spawn(move || {
        loop {
            std::thread::sleep(Duration::from_millis(50));
            if WINCH.swap(false, Ordering::Relaxed)
                && let Ok((c, r)) = terminal::size()
            {
                let mut m = vec![server::IN_RESIZE];
                m.extend(c.to_be_bytes());
                m.extend(r.to_be_bytes());
                let _ = lock(&wc).write_all(&m);
            }
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
        let Some(mut c) = restarting.then(|| reconnect(sock)).flatten() else {
            break;
        };
        if send_attach(&mut c).is_err() {
            break;
        }
        *lock(&writer) = c.try_clone()?;
        conn = c;
    }
    let _ =
        out.write_all(b"\x1b[?1003l\x1b[?1002l\x1b[?1006l\x1b[?2004l\x1b[?1l\x1b[?25h\x1b[?1049l");
    let _ = out.flush();
    let _ = terminal::disable_raw_mode();
    std::process::exit(0);
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn send_attach(conn: &mut UnixStream) -> std::io::Result<()> {
    let (cols, rows) = terminal::size()?;
    writeln!(
        conn,
        "{}",
        json!({"cmd": "attach", "cols": cols, "rows": rows})
    )
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

fn api_value(sock: &str, req: Value) -> std::io::Result<()> {
    api(sock, &req.to_string())
}

fn api(sock: &str, req: &str) -> std::io::Result<()> {
    let v: Value = match serde_json::from_str(req) {
        Ok(v) => v,
        Err(e) => {
            println!(
                "{}",
                json!({"ok": false, "error": {"code": "invalid", "message": format!("not valid JSON: {e}")}})
            );
            std::process::exit(1);
        }
    };
    let v = with_caller(v);
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
