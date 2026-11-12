use std::io::{BufRead, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{json, Value};

/// フックの呼び出し内容を変えたら上げる。古い版を見つけるとサイドバーが更新を促す。
pub const VERSION: u32 = 2;

const EVENTS: [&str; 7] =
    ["SessionStart", "UserPromptSubmit", "PreToolUse", "PostToolUse", "PermissionRequest", "Stop", "SessionEnd"];
/// エージェント、設定フォルダの場所を変える環境変数、既定の設定フォルダ (ホームから)、その中のフックのファイル
const TARGETS: [(&str, &str, &str, &str); 3] = [
    ("claude", "CLAUDE_CONFIG_DIR", ".claude", "settings.json"),
    ("codex", "CODEX_HOME", ".codex", "hooks.json"),
    ("copilot", "", ".copilot", "hooks/mdeck.json"),
];

/// 環境変数の読み方。テストは本物の環境を見ない
type Env<'a> = &'a dyn Fn(&str) -> Option<String>;

fn real_env(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.is_empty())
}

/// エージェントの設定フォルダ (~/.claude など。CLAUDE_CONFIG_DIR / CODEX_HOME があればそちら)
pub fn agent_dir(agent: &str) -> PathBuf {
    dir_in(&home(), &real_env, agent)
}

fn dir_in(home: &Path, env: Env, agent: &str) -> PathBuf {
    let (_, var, dir, _) = TARGETS.into_iter().find(|t| t.0 == agent).unwrap_or(("", "", "", ""));
    env(var).map(PathBuf::from).unwrap_or_else(|| home.join(dir))
}
/// copilot のイベント名と、サーバーが受ける名前 (claude と同じ)。copilot の payload にはイベント名が無いので、コマンドに書いて渡す。
/// permissionRequest は確認を出すか決める前に呼ばれるので使わない (入力待ちを誤判定する)
const COPILOT_EVENTS: [(&str, &str); 6] = [
    ("sessionStart", "SessionStart"),
    ("userPromptSubmitted", "UserPromptSubmit"),
    ("preToolUse", "PreToolUse"),
    ("postToolUse", "PostToolUse"),
    ("agentStop", "Stop"),
    ("sessionEnd", "SessionEnd"),
];

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Status {
    /// エージェントの設定フォルダが無い (未インストール)
    NoAgent,
    Missing,
    /// 入っている中で最も古い版 (版番号の無い初期版は 0)。全イベントに揃っていないものもここ
    Outdated(u32),
    Current,
}

impl Status {
    pub fn label(self) -> String {
        match self {
            Status::NoAgent => "agent not found".into(),
            Status::Missing => "not installed".into(),
            Status::Outdated(v) if v >= VERSION => format!("hooks v{v} (partly installed)"),
            Status::Outdated(v) => format!("hooks v{v} → v{VERSION}"),
            Status::Current => format!("installed (hooks v{VERSION})"),
        }
    }
}

/// v1 のフックから呼ばれる。エージェントを止めないよう、何があっても無言で終わる。
pub fn run(agent: &str) {
    let mut input = String::new();
    let _ = std::io::stdin().read_to_string(&mut input);
    let (Ok(tab), Ok(sock)) = (std::env::var("MDECK_TAB"), std::env::var("MDECK_SOCK")) else { return };
    let Ok(tab) = tab.parse::<u32>() else { return };
    let v: Value = serde_json::from_str(&input).unwrap_or(Value::Null);
    let Some(event) = v["hook_event_name"].as_str() else { return };
    let req = json!({
        "cmd": "hook", "tab": tab, "agent": agent, "event": event,
        "message": v["last_assistant_message"], "tool": v["tool_name"], "input": v["tool_input"],
        "session": v["session_id"],
    });
    let mut err = String::new();
    for i in 0..3 {
        if i > 0 {
            std::thread::sleep(Duration::from_millis(200));
        }
        match send(&sock, &req) {
            Ok(()) => return,
            Err(e) => err = e,
        }
    }
    log_failure(&sock, &format!("tab={tab} agent={agent} event={event} session={} {err}", v["session_id"]));
}

fn send(sock: &str, req: &Value) -> Result<(), String> {
    let mut c = UnixStream::connect(sock).map_err(|e| format!("connect {sock}: {e}"))?;
    let _ = c.set_read_timeout(Some(Duration::from_millis(500)));
    writeln!(c, "{req}").map_err(|e| format!("write: {e}"))?;
    let mut line = String::new();
    let _ = std::io::BufReader::new(c).read_line(&mut line);
    match serde_json::from_str::<Value>(&line) {
        Ok(r) if r["ok"] == false => Err(format!("rejected: {}", r["error"])),
        _ => Ok(()),
    }
}

/// 届かなかったフックを sock と同じフォルダの hook.log に残す。
fn log_failure(sock: &str, msg: &str) {
    crate::server::append_log(&Path::new(sock).with_file_name("hook.log"), msg);
}

fn home() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/".into()))
}

/// フックの JSON を改行を除いて nc でソケットへ流す。mdeck 本体を起動すると、upgrade 中や
/// 更新直後の初回起動の検査 (数分かかることがある) の間に 5 秒で打ち切られて状態を取りこぼす。
/// 末尾のコメントは導入済みの版を見分ける印。
fn command(agent: &str, event: Option<&str>) -> String {
    let event = event.map(|e| format!(r#""event":"{e}","#)).unwrap_or_default();
    format!(
        r#"[ -n "$MDECK_TAB" ] && [ -S "$MDECK_SOCK" ] || exit 0; {{ printf '{{"cmd":"hook","agent":"{agent}",{event}"tab":%s,"payload":' "$MDECK_TAB"; tr -d '\n'; echo '}}'; }} | /usr/bin/nc -U -w 2 "$MDECK_SOCK" >/dev/null 2>&1 || echo "$(date +%s) tab=$MDECK_TAB agent={agent} nc failed" >> "${{MDECK_SOCK%/*}}/hook.log"; exit 0 # mdeck hook {agent} v{VERSION}"#
    )
}

fn cmd_version(cmd: &str, agent: &str) -> Option<u32> {
    let marker = format!(" hook {agent}");
    let rest = &cmd[cmd.find(&marker)? + marker.len()..];
    cmd.contains("mdeck").then(|| rest.trim().strip_prefix('v').and_then(|v| v.parse().ok()).unwrap_or(0))
}

fn our_version(group: &Value, agent: &str) -> Option<u32> {
    group["hooks"].as_array()?.iter().find_map(|h| cmd_version(h["command"].as_str()?, agent))
}

fn status_of(root: &Value, agent: &str) -> Status {
    let versions: Vec<Option<u32>> = if agent == "copilot" {
        // copilot は 1 イベントに {type, bash} を並べる形
        COPILOT_EVENTS
            .iter()
            .map(|(ev, _)| root["hooks"][ev].as_array().and_then(|a| a.iter().find_map(|h| cmd_version(h["bash"].as_str()?, agent))))
            .collect()
    } else {
        EVENTS.iter().map(|ev| root["hooks"][ev].as_array().and_then(|a| a.iter().find_map(|g| our_version(g, agent)))).collect()
    };
    if versions.iter().all(|v| *v == Some(VERSION)) {
        Status::Current
    } else if let Some(oldest) = versions.iter().flatten().min() {
        Status::Outdated(*oldest)
    } else {
        Status::Missing
    }
}

fn apply(root: &mut Value, agent: &str) {
    // copilot は MornDeck 専用のファイルなので丸ごと書く
    if agent == "copilot" {
        let hooks: serde_json::Map<String, Value> = COPILOT_EVENTS
            .iter()
            .map(|(ev, name)| (ev.to_string(), json!([{"type": "command", "bash": command(agent, Some(name)), "timeoutSec": 5}])))
            .collect();
        *root = json!({"version": 1, "hooks": hooks});
        return;
    }
    let command = command(agent, None);
    let command = command.as_str();
    if !root.is_object() {
        *root = json!({});
    }
    if !root["hooks"].is_object() {
        root["hooks"] = json!({});
    }
    for ev in EVENTS {
        let list = &mut root["hooks"][ev];
        if !list.is_array() {
            *list = json!([]);
        }
        let arr = list.as_array_mut().unwrap();
        arr.retain(|g| our_version(g, agent).is_none());
        arr.push(json!({"hooks": [{"type": "command", "command": command, "timeout": 5}]}));
    }
}

/// (エージェント, フックのファイル)。設定フォルダの無いエージェント (未インストール) は対象外。
fn targets<'a>(home: &'a Path, env: Env<'a>) -> impl Iterator<Item = (&'static str, PathBuf)> + 'a {
    TARGETS.into_iter().map(move |t| (t.0, dir_in(home, env, t.0), t.3)).filter(|(_, dir, _)| dir.is_dir()).map(|(a, dir, file)| (a, dir.join(file)))
}

fn read(path: &Path) -> Result<Value, String> {
    match std::fs::read_to_string(path) {
        // 設定はオブジェクトのはず。配列などは壊さないよう触らない
        Ok(s) => serde_json::from_str(&s).map_err(|e| e.to_string()).and_then(|v: Value| if v.is_object() { Ok(v) } else { Err("JSON のオブジェクトではない".into()) }).map_err(|e| format!("{}: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(json!({})),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

/// claude / codex / copilot を別々に返す。インストールされていないものは NoAgent。
pub fn status() -> Vec<(&'static str, Status)> {
    status_in(&home(), &real_env)
}

fn status_in(h: &Path, env: Env) -> Vec<(&'static str, Status)> {
    let found: Vec<_> = targets(h, env).collect();
    TARGETS
        .into_iter()
        .map(|t| match found.iter().find(|(a, _)| *a == t.0) {
            Some((a, p)) => (*a, read(p).map(|v| status_of(&v, a)).unwrap_or(Status::Missing)),
            None => (t.0, Status::NoAgent),
        })
        .collect()
}

pub fn install(agent: &str) -> Result<(), String> {
    install_in(&home(), &real_env, agent)
}

fn install_in(home: &Path, env: Env, only: &str) -> Result<(), String> {
    for (agent, path) in targets(home, env).filter(|(a, _)| *a == only) {
        let mut root = read(&path)?;
        apply(&mut root, agent);
        write(&path, &root, agent != "copilot")?;
    }
    Ok(())
}

/// 設定ファイルを置き換える。シンボリックリンクはリンク先の実体を書き換え、権限は元のまま。
/// backup なら、最初の 1 回だけ元のファイルを <名前>.mdeck-bak に残す (copilot のファイルは MornDeck 専用なので残さない)
fn write(path: &Path, root: &Value, backup: bool) -> Result<(), String> {
    let path = &path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let bak = path.with_extension("json.mdeck-bak");
    if backup && path.is_file() && !bak.exists() {
        std::fs::copy(path, &bak).map_err(|e| format!("{}: {e}", bak.display()))?;
    }
    let tmp = path.with_extension("mdeck-tmp");
    let body = serde_json::to_string_pretty(root).map_err(|e| e.to_string())? + "\n";
    // 設定には鍵が入っていることがあるので、本人だけが読める形で作ってから元の権限に合わせる
    let put = || -> std::io::Result<()> {
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&tmp)?;
        f.write_all(body.as_bytes())?;
        f.set_permissions(std::fs::metadata(path).map(|m| m.permissions()).unwrap_or_else(|_| std::os::unix::fs::PermissionsExt::from_mode(0o644)))?;
        f.sync_all()?;
        std::fs::rename(&tmp, path)
    };
    put().map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("{}: {e}", path.display())
    })
}

/// 全エージェントからフックを外す。エージェントごとに、外したか (無ければ false) か失敗の理由を返す。
pub fn uninstall() -> Vec<(&'static str, Result<bool, String>)> {
    uninstall_in(&home(), &real_env)
}

fn uninstall_in(home: &Path, env: Env) -> Vec<(&'static str, Result<bool, String>)> {
    targets(home, env).map(|(agent, path)| (agent, remove(&path, agent))).collect()
}

/// 1 つのエージェントからフックを外す。
pub fn uninstall_agent(agent: &str) -> Result<(), String> {
    for (a, path) in targets(&home(), &real_env).filter(|(a, _)| *a == agent) {
        remove(&path, a)?;
    }
    Ok(())
}

/// 外したものがあれば true。他のフックと設定は残す
fn remove(path: &Path, agent: &str) -> Result<bool, String> {
    if !path.exists() {
        return Ok(false);
    }
    if agent == "copilot" {
        return std::fs::remove_file(path).map(|_| true).map_err(|e| e.to_string());
    }
    let mut root = read(path)?;
    let before = root.clone();
    // root["hooks"] と書くと、無いときに "hooks": null が足されてしまう
    if let Some(hooks) = root.get_mut("hooks").and_then(Value::as_object_mut) {
        // MornDeck のフックを外して空になったイベントだけ消す (元から空のものは残す)
        hooks.retain(|_, list| {
            let Some(arr) = list.as_array_mut() else { return true };
            let before = arr.len();
            arr.retain(|g| our_version(g, agent).is_none());
            arr.len() == before || !arr.is_empty()
        });
    }
    if root == before {
        return Ok(false);
    }
    write(path, &root, false).map(|_| true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_env(_: &str) -> Option<String> {
        None
    }

    #[test]
    fn install_upgrades_old_entries_and_keeps_others() {
        let home = std::env::temp_dir().join(format!("mdeck-test-{}", std::process::id()));
        std::fs::create_dir_all(home.join(".claude")).unwrap();
        let settings = home.join(".claude/settings.json");
        let old = json!({"model": "x", "hooks": {"Stop": [
            {"hooks": [{"command": "other"}]},
            {"hooks": [{"command": "'/old/mdeck' hook claude"}]}
        ]}});
        std::fs::write(&settings, old.to_string()).unwrap();
        assert_eq!(status_of(&old, "claude"), Status::Outdated(0));
        assert_eq!(Status::Outdated(0).label(), format!("フック v0 → v{VERSION}"));
        assert_eq!(Status::Current.label(), format!("導入済み (フック v{VERSION})"));

        install_in(&home, &no_env, "codex").unwrap();
        assert_eq!(read(&settings).unwrap(), old, "codex の導入で claude を触った");
        install_in(&home, &no_env, "claude").unwrap();
        install_in(&home, &no_env, "claude").unwrap();
        let v = read(&settings).unwrap();
        assert_eq!(v["model"], "x");
        assert_eq!(v["hooks"]["Stop"].as_array().unwrap().len(), 2);
        assert_eq!(status_of(&v, "claude"), Status::Current);
        assert_eq!(status_of(&v, "codex"), Status::Missing);
        assert!(!home.join(".codex").exists());
        assert_eq!(status_in(&home, &no_env), vec![("claude", Status::Current), ("codex", Status::NoAgent), ("copilot", Status::NoAgent)]);
        // copilot は専用のファイルにイベントごとの bash を書き、イベント名をコマンドで渡す
        std::fs::create_dir_all(home.join(".copilot")).unwrap();
        assert_eq!(status_in(&home, &no_env)[2], ("copilot", Status::Missing));
        install_in(&home, &no_env, "copilot").unwrap();
        let c = read(&home.join(".copilot/hooks/mdeck.json")).unwrap();
        assert_eq!(c["version"], 1);
        assert!(c["hooks"]["agentStop"][0]["bash"].as_str().unwrap().contains(r#""event":"Stop","#));
        assert_eq!(status_in(&home, &no_env)[2], ("copilot", Status::Current));
        // 外すと他のフックと設定だけが残る
        assert_eq!(uninstall_in(&home, &no_env), vec![("claude", Ok(true)), ("copilot", Ok(true))]);
        assert_eq!(read(&settings).unwrap(), json!({"model": "x", "hooks": {"Stop": [{"hooks": [{"command": "other"}]}]}}));
        assert!(!home.join(".copilot/hooks/mdeck.json").exists());
        assert_eq!(uninstall_in(&home, &no_env), vec![("claude", Ok(false)), ("copilot", Ok(false))]);
        // フックの無い設定には触らない ("hooks": null を足さない)。オブジェクトでない設定は壊さず失敗にする
        for (body, ok) in [(r#"{"model":"x"}"#, true), (r#"{"hooks":{"Stop":[]}}"#, true), ("[]", false)] {
            std::fs::write(&settings, body).unwrap();
            assert_eq!(uninstall_in(&home, &no_env)[0].1.is_ok(), ok, "{body}");
            assert_eq!(install_in(&home, &no_env, "codex"), Ok(()));
            assert_eq!(std::fs::read_to_string(&settings).unwrap(), body);
        }
        assert!(install_in(&home, &no_env, "claude").is_err(), "配列の設定を書き換えた");
        // CLAUDE_CONFIG_DIR / CODEX_HOME があれば、そちらの設定フォルダを使う
        let moved = home.join("elsewhere");
        std::fs::create_dir_all(&moved).unwrap();
        let env = |k: &str| (k == "CODEX_HOME").then(|| moved.to_string_lossy().into_owned());
        assert_eq!(status_in(&home, &env)[1], ("codex", Status::Missing));
        install_in(&home, &env, "codex").unwrap();
        assert_eq!(status_of(&read(&moved.join("hooks.json")).unwrap(), "codex"), Status::Current);
        assert!(!home.join(".codex").exists());
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn install_keeps_symlink_and_mode_and_backs_up_once() {
        use std::os::unix::fs::PermissionsExt;
        let home = std::env::temp_dir().join(format!("mdeck-link-{}", std::process::id()));
        std::fs::create_dir_all(home.join(".claude")).unwrap();
        std::fs::create_dir_all(home.join("dotfiles")).unwrap();
        let real = home.join("dotfiles/settings.json");
        std::fs::write(&real, r#"{"env": {"KEY": "x"}}"#).unwrap();
        std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o600)).unwrap();
        let link = home.join(".claude/settings.json");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        install_in(&home, &no_env, "claude").unwrap();
        install_in(&home, &no_env, "claude").unwrap();
        assert!(link.symlink_metadata().unwrap().file_type().is_symlink(), "リンクを実ファイルに置き換えた");
        assert_eq!(real.metadata().unwrap().permissions().mode() & 0o777, 0o600, "権限が広がった");
        assert_eq!(status_of(&read(&real).unwrap(), "claude"), Status::Current);
        assert_eq!(std::fs::read_to_string(home.join("dotfiles/settings.json.mdeck-bak")).unwrap(), r#"{"env": {"KEY": "x"}}"#, "最初の中身を残していない");
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn hook_command_streams_payload_without_running_mdeck() {
        let _fork = crate::FORK_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("mdeck-nc-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let sock = dir.join("t.sock");
        let listener = std::os::unix::net::UnixListener::bind(&sock).unwrap();
        let server = std::thread::spawn(move || {
            let (c, _) = listener.accept().unwrap();
            let mut line = String::new();
            std::io::BufReader::new(c.try_clone().unwrap()).read_line(&mut line).unwrap();
            // nc が先に切っていても構わない
            let _ = writeln!(&c, "{{\"ok\":true}}");
            line
        });
        let run = |env: &[(&str, &str)]| {
            let mut child = std::process::Command::new("/bin/sh")
                .args(["-c", &command("claude", None)])
                .env_remove("MDECK_TAB")
                .envs(env.iter().copied())
                .stdin(std::process::Stdio::piped())
                .spawn()
                .unwrap();
            // フックは整形された複数行の JSON を渡すこともある
            child.stdin.take().unwrap().write_all(b"{\n  \"hook_event_name\": \"Stop\",\n  \"session_id\": \"s-1\"\n}\n").unwrap();
            child.wait().unwrap()
        };
        assert!(run(&[]).success(), "MornDeck の外では何もせず成功する");
        assert!(run(&[("MDECK_TAB", "7"), ("MDECK_SOCK", sock.to_str().unwrap())]).success());
        let got: Value = serde_json::from_str(&server.join().unwrap()).unwrap();
        assert_eq!((got["cmd"].as_str(), got["tab"].as_u64(), got["agent"].as_str()), (Some("hook"), Some(7), Some("claude")));
        assert_eq!(got["payload"]["hook_event_name"], "Stop");
        assert_eq!(status_of(&json!({"hooks": {"Stop": [{"hooks": [{"command": command("claude", None)}]}]}}), "claude"), Status::Outdated(VERSION));
        // copilot のコマンドはイベント名を付けて送る
        let sock2 = dir.join("c.sock");
        let listener = std::os::unix::net::UnixListener::bind(&sock2).unwrap();
        let server = std::thread::spawn(move || {
            let (c, _) = listener.accept().unwrap();
            let mut line = String::new();
            std::io::BufReader::new(c.try_clone().unwrap()).read_line(&mut line).unwrap();
            let _ = writeln!(&c, "{{\"ok\":true}}");
            line
        });
        let mut child = std::process::Command::new("/bin/sh")
            .args(["-c", &command("copilot", Some("Stop"))])
            .envs([("MDECK_TAB", "3"), ("MDECK_SOCK", sock2.to_str().unwrap())])
            .stdin(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(b"{\"sessionId\": \"s-2\", \"stopReason\": \"end_turn\"}\n").unwrap();
        assert!(child.wait().unwrap().success());
        let got: Value = serde_json::from_str(&server.join().unwrap()).unwrap();
        assert_eq!((got["agent"].as_str(), got["event"].as_str(), got["tab"].as_u64()), (Some("copilot"), Some("Stop"), Some(3)));
        assert_eq!(got["payload"]["sessionId"], "s-2");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn unreachable_server_is_logged() {
        let dir = std::env::temp_dir().join(format!("mdeck-hooklog-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let sock = dir.join("t.sock").to_string_lossy().into_owned();
        let err = send(&sock, &json!({"cmd": "hook"})).unwrap_err();
        log_failure(&sock, &err);
        let log = std::fs::read_to_string(dir.join("hook.log")).unwrap();
        assert!(log.contains("connect") && log.contains("t.sock"), "{log}");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
