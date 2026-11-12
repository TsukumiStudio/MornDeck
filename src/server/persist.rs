use super::*;

// ---------- 落ちても戻れるようにタブの情報を保存する ----------

pub(super) fn tabs_path(sock: &str) -> std::path::PathBuf {
    std::path::Path::new(sock).with_file_name("tabs.json")
}

pub(super) fn proc_cwd(pid: u32) -> Option<String> {
    let mut info: libc::proc_vnodepathinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of_val(&info) as i32;
    let n = unsafe { libc::proc_pidinfo(pid as i32, libc::PROC_PIDVNODEPATHINFO, 0, (&raw mut info).cast(), size) };
    if n != size {
        return None;
    }
    let raw: Vec<u8> = info.pvi_cdir.vip_path.iter().flatten().map(|c| *c as u8).take_while(|c| *c != 0).collect();
    String::from_utf8(raw).ok().filter(|p| !p.is_empty())
}

pub(super) fn snapshot(st: &State) -> Value {
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
pub(super) fn usage_loop(s: Shared) {
    let home = std::path::PathBuf::from(std::env::var("HOME").unwrap_or_default());
    let (mut claude, mut copilot) = (None, None);
    let mut i = 0u64;
    loop {
        // オフの間は何も読まない。オンに戻したら次の回ですぐ読む
        if !lock(&s).alerts.usage {
            (claude, copilot, i) = (None, None, 0);
            std::thread::sleep(Duration::from_secs(5));
            continue;
        }
        if i.is_multiple_of(5) {
            // 取れないときは前の値を出したままにし、理由を events.log に残す
            // 入れていないエージェントのものは読まない (キーチェーンの許可ダイアログを出さない)
            if crate::hooks::agent_dir("claude").is_dir() {
                match crate::usage::claude() {
                    Ok(w) => claude = Some(w),
                    Err(e) => log_event(&lock(&s).sock.clone(), "usage", &json!({"agent": "claude", "error": e})),
                }
            }
            if home.join(".copilot").is_dir() {
                copilot = crate::usage::copilot(&home).or(copilot);
            }
        }
        let codex_dir = crate::hooks::agent_dir("codex");
        let codex = crate::usage::codex(&codex_dir);
        if codex.is_none() && i.is_multiple_of(5) && codex_dir.is_dir() {
            log_event(&lock(&s).sock.clone(), "usage", &json!({"agent": "codex", "error": "no usage (rate_limits) in recent session logs"}));
        }
        {
            let mut st = lock(&s);
            if st.alerts.usage {
                st.usage = [claude, codex, copilot];
            }
            mark(&s, &mut st);
        }
        i += 1;
        // 途中でオフにされたらすぐ気づけるよう、1 分を小刻みに待つ
        for _ in 0..12 {
            if lock(&s).alerts.usage {
                std::thread::sleep(Duration::from_secs(5));
            }
        }
    }
}

pub(super) fn save_loop(s: Shared) {
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

pub(super) fn write_atomic(path: &std::path::Path, body: &str) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, body).and_then(|_| std::fs::rename(&tmp, path))
}

/// 新しい版のサーバーに入れ替える。タブを保存して閉じ、同じソケットで起動し直す。
/// claude / codex は保存した会話 ID で再開し、画面のクライアントは RESTART_MARK を見てつなぎ直す。
/// 待ち受けソケットの番号。起動し直すときに新しいプロセスへ渡す
pub(super) static LISTEN_FD: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(-1);

pub(super) fn restart(s: &Shared, except: Option<u64>, exe: Option<std::path::PathBuf>) {
    let mut st = lock(s); // 保存し終わるまで save_loop に上書きさせない
    log_event(&st.sock, "restart", &json!({"except": except, "exe": exe}));
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
    let mut cmd = std::process::Command::new(exe.unwrap_or_else(server_exe));
    cmd.arg("server");
    // 待ち受けソケットを開いたまま新しいプロセスへ渡す (exec で閉じないようにする)
    let fd = LISTEN_FD.load(std::sync::atomic::Ordering::Relaxed);
    if fd >= 0 && unsafe { libc::fcntl(fd, libc::F_SETFD, 0) } == 0 {
        cmd.env(LISTEN_FD_ENV, fd.to_string());
    }
    let e = cmd.exec();
    // exec できなければ終わる。次に画面をつないだときに mdeck が起動し直す
    eprintln!("restart: exec: {e}");
    std::process::exit(1);
}

/// upgrade で起動した版の実体は消えるので、起動に使ったパス (brew のリンク) か PATH 上の mdeck を使う。
pub(super) fn server_exe() -> std::path::PathBuf {
    std::env::args()
        .next()
        .map(std::path::PathBuf::from)
        .filter(|p| p.is_absolute() && p.is_file())
        .or_else(|| std::env::var_os("PATH").and_then(|p| std::env::split_paths(&p).map(|d| d.join("mdeck")).find(|f| f.is_file())))
        // 画面側で起動したサーバーの PATH には brew のフォルダが無い
        .or_else(|| ["/opt/homebrew/bin/mdeck", "/usr/local/bin/mdeck"].into_iter().map(std::path::PathBuf::from).find(|f| f.is_file()))
        .unwrap_or_else(|| "mdeck".into())
}

/// 保存された会話 ID で再開するコマンド。ID はシェルに打ち込むので英数字と - だけを通す。
pub(super) fn resume_command(agent: Option<&str>, session: Option<&str>, args: Option<&str>) -> Option<String> {
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

pub(super) fn restore(s: &Shared) {
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
        let created = new_tab(s, cwd, run.as_deref(), name, false, None, false).or_else(|_| new_tab(s, cwd, run.as_deref(), None, false, None, false));
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
