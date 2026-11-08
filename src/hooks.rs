use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{json, Value};

/// フックの呼び出し内容を変えたら上げる。古い版を見つけるとサイドバーが更新を促す。
pub const VERSION: u32 = 1;

const EVENTS: [&str; 7] =
    ["SessionStart", "UserPromptSubmit", "PreToolUse", "PostToolUse", "PermissionRequest", "Stop", "SessionEnd"];
const TARGETS: [(&str, &str); 2] = [("claude", ".claude/settings.json"), ("codex", ".codex/hooks.json")];

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Status {
    Missing,
    Outdated,
    Current,
}

impl Status {
    pub fn label(self) -> &'static str {
        match self {
            Status::Missing => "未導入",
            Status::Outdated => "旧版",
            Status::Current => "最新",
        }
    }
}

/// claude / codex のフックから呼ばれる。エージェントを止めないよう、何があっても無言で終わる。
pub fn run(agent: &str) {
    let mut input = String::new();
    let _ = std::io::stdin().read_to_string(&mut input);
    let (Ok(tab), Ok(sock)) = (std::env::var("MDECK_TAB"), std::env::var("MDECK_SOCK")) else { return };
    let Ok(tab) = tab.parse::<u32>() else { return };
    let v: Value = serde_json::from_str(&input).unwrap_or(Value::Null);
    let Some(event) = v["hook_event_name"].as_str() else { return };
    let Ok(mut c) = UnixStream::connect(sock) else { return };
    let _ = c.set_read_timeout(Some(Duration::from_millis(500)));
    let req = json!({
        "cmd": "hook", "tab": tab, "agent": agent, "event": event,
        "message": v["last_assistant_message"], "tool": v["tool_name"], "input": v["tool_input"],
    });
    let _ = writeln!(c, "{req}");
    let _ = c.read(&mut [0u8; 256]);
}

fn home() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/".into()))
}

/// brew の Cellar 実体を書くと upgrade で消えるので、PATH 上のリンクを優先する。
fn exe() -> String {
    std::env::var_os("PATH")
        .and_then(|p| std::env::split_paths(&p).map(|d| d.join("mdeck")).find(|f| f.is_file()))
        .or_else(|| std::env::current_exe().ok())
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| "mdeck".into())
}

fn our_version(group: &Value, agent: &str) -> Option<u32> {
    let marker = format!(" hook {agent}");
    group["hooks"].as_array()?.iter().find_map(|h| {
        let cmd = h["command"].as_str()?;
        let rest = &cmd[cmd.find(&marker)? + marker.len()..];
        cmd.contains("mdeck").then(|| rest.trim().strip_prefix('v').and_then(|v| v.parse().ok()).unwrap_or(0))
    })
}

fn status_of(root: &Value, agent: &str) -> Status {
    let versions: Vec<Option<u32>> = EVENTS
        .iter()
        .map(|ev| root["hooks"][ev].as_array().and_then(|a| a.iter().find_map(|g| our_version(g, agent))))
        .collect();
    if versions.iter().all(|v| *v == Some(VERSION)) {
        Status::Current
    } else if versions.iter().any(Option::is_some) {
        Status::Outdated
    } else {
        Status::Missing
    }
}

fn apply(root: &mut Value, agent: &str, command: &str) {
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

/// エージェント本体が無い (設定ディレクトリが無い) ものは対象外。
fn targets(home: &Path) -> impl Iterator<Item = (&'static str, PathBuf)> + '_ {
    TARGETS.into_iter().map(|(a, p)| (a, home.join(p))).filter(|(_, p)| p.parent().is_some_and(Path::is_dir))
}

fn read(path: &Path) -> Result<Value, String> {
    match std::fs::read_to_string(path) {
        Ok(s) => serde_json::from_str(&s).map_err(|e| format!("{}: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(json!({})),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

pub fn status() -> Vec<(&'static str, Status)> {
    let h = home();
    targets(&h).map(|(a, p)| (a, read(&p).map(|v| status_of(&v, a)).unwrap_or(Status::Missing))).collect()
}

pub fn install() -> Result<(), String> {
    install_in(&home(), &exe())
}

fn install_in(home: &Path, exe: &str) -> Result<(), String> {
    for (agent, path) in targets(home) {
        let mut root = read(&path)?;
        apply(&mut root, agent, &format!("'{exe}' hook {agent} v{VERSION}"));
        let tmp = path.with_extension("mdeck-tmp");
        let body = serde_json::to_string_pretty(&root).map_err(|e| e.to_string())? + "\n";
        std::fs::write(&tmp, body).and_then(|_| std::fs::rename(&tmp, &path)).map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(status_of(&old, "claude"), Status::Outdated);

        install_in(&home, "/bin/mdeck").unwrap();
        install_in(&home, "/bin/mdeck").unwrap();
        let v = read(&settings).unwrap();
        assert_eq!(v["model"], "x");
        assert_eq!(v["hooks"]["Stop"].as_array().unwrap().len(), 2);
        assert_eq!(status_of(&v, "claude"), Status::Current);
        assert_eq!(status_of(&v, "codex"), Status::Missing);
        assert!(!home.join(".codex").exists());
        std::fs::remove_dir_all(home).unwrap();
    }
}
