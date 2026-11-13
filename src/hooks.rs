use std::io::{BufRead, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Value, json};

/// フックの呼び出し内容を変えたら上げる。古い版を見つけるとサイドバーが更新を促す。
/// codex は v3 (タブの番号を環境変数に頼らず、サーバーが会話と cwd から決める)。
pub fn version(agent: &str) -> u32 {
    if agent == "codex" { 3 } else { 2 }
}

const EVENTS: [&str; 7] = [
    "SessionStart",
    "UserPromptSubmit",
    "PreToolUse",
    "PostToolUse",
    "PermissionRequest",
    "Stop",
    "SessionEnd",
];
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
    let (_, var, dir, _) = TARGETS
        .into_iter()
        .find(|t| t.0 == agent)
        .unwrap_or(("", "", "", ""));
    env(var)
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(dir))
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
    /// claude: settings.json に入っているが、組織の管理設定で実行されない。plugin_helps ならプラグインで入れれば動く
    Blocked {
        plugin_helps: bool,
    },
    /// claude: プラグインとして入っている (settings.json の代わり)
    Plugin,
}

impl Status {
    pub fn label(self, agent: &str) -> String {
        let latest = version(agent);
        match self {
            Status::NoAgent => "agent not found".into(),
            Status::Missing => "hooks not installed".into(),
            Status::Outdated(v) if v >= latest => format!("hooks v{v} (partly installed)"),
            Status::Outdated(v) => format!("hooks v{v} → v{latest}"),
            Status::Current if agent == "claude" => {
                format!("installed in settings.json (hooks v{latest})")
            }
            Status::Current => format!("installed (hooks v{latest})"),
            Status::Blocked { plugin_helps: true } => {
                "blocked by org policy, install as plugin (p)".into()
            }
            Status::Blocked {
                plugin_helps: false,
            } => "blocked by org policy".into(),
            Status::Plugin => format!("installed as plugin (hooks v{latest})"),
        }
    }
}

/// v1 のフックから呼ばれる。エージェントを止めないよう、何があっても無言で終わる。
pub fn run(agent: &str) {
    let mut input = String::new();
    let _ = std::io::stdin().read_to_string(&mut input);
    let (Ok(tab), Ok(sock)) = (std::env::var("MDECK_TAB"), std::env::var("MDECK_SOCK")) else {
        return;
    };
    let Ok(tab) = tab.parse::<u32>() else { return };
    let v: Value = serde_json::from_str(&input).unwrap_or(Value::Null);
    let Some(event) = v["hook_event_name"].as_str() else {
        return;
    };
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
    log_failure(
        &sock,
        &format!(
            "tab={tab} agent={agent} event={event} session={} {err}",
            v["session_id"]
        ),
    );
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
    let event = event
        .map(|e| format!(r#""event":"{e}","#))
        .unwrap_or_default();
    let version = version(agent);
    // codex は常駐のサーバーがフックを動かすので、タブの環境変数が届かない (別のタブのものが入っていることもある)。
    // 番号は手がかりとして送るだけにし、どのタブかはサーバーが会話と cwd から決める。MornDeck の外の codex からも届く
    if agent == "codex" {
        return format!(
            r#"S="$MDECK_SOCK"; [ -S "$S" ] || S="$HOME/.mdeck/mdeck.sock"; [ -S "$S" ] || exit 0; {{ printf '{{"cmd":"hook","agent":"codex","tab":"%s","payload":' "$MDECK_TAB"; tr -d '\n'; echo '}}'; }} | /usr/bin/nc -U -w 2 "$S" >/dev/null 2>&1; exit 0 # mdeck hook codex v{version}"#
        );
    }
    format!(
        r#"[ -n "$MDECK_TAB" ] && [ -S "$MDECK_SOCK" ] || exit 0; {{ printf '{{"cmd":"hook","agent":"{agent}",{event}"tab":%s,"payload":' "$MDECK_TAB"; tr -d '\n'; echo '}}'; }} | /usr/bin/nc -U -w 2 "$MDECK_SOCK" >/dev/null 2>&1 || echo "$(date +%s) tab=$MDECK_TAB agent={agent} nc failed" >> "${{MDECK_SOCK%/*}}/hook.log"; exit 0 # mdeck hook {agent} v{version}"#
    )
}

fn cmd_version(cmd: &str, agent: &str) -> Option<u32> {
    let marker = format!(" hook {agent}");
    let rest = &cmd[cmd.find(&marker)? + marker.len()..];
    cmd.contains("mdeck").then(|| {
        rest.trim()
            .strip_prefix('v')
            .and_then(|v| v.parse().ok())
            .unwrap_or(0)
    })
}

fn our_version(group: &Value, agent: &str) -> Option<u32> {
    group["hooks"]
        .as_array()?
        .iter()
        .find_map(|h| cmd_version(h["command"].as_str()?, agent))
}

fn status_of(root: &Value, agent: &str) -> Status {
    let versions: Vec<Option<u32>> = if agent == "copilot" {
        // copilot は 1 イベントに {type, bash} を並べる形
        COPILOT_EVENTS
            .iter()
            .map(|(ev, _)| {
                root["hooks"][ev].as_array().and_then(|a| {
                    a.iter()
                        .find_map(|h| cmd_version(h["bash"].as_str()?, agent))
                })
            })
            .collect()
    } else {
        EVENTS
            .iter()
            .map(|ev| {
                root["hooks"][ev]
                    .as_array()
                    .and_then(|a| a.iter().find_map(|g| our_version(g, agent)))
            })
            .collect()
    };
    if versions.iter().all(|v| *v == Some(version(agent))) {
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
    TARGETS
        .into_iter()
        .map(move |t| (t.0, dir_in(home, env, t.0), t.3))
        .filter(|(_, dir, _)| dir.is_dir())
        .map(|(a, dir, file)| (a, dir.join(file)))
}

fn read(path: &Path) -> Result<Value, String> {
    match std::fs::read_to_string(path) {
        // 設定はオブジェクトのはず。配列などは壊さないよう触らない
        Ok(s) => serde_json::from_str(&s)
            .map_err(|e| e.to_string())
            .and_then(|v: Value| {
                if v.is_object() {
                    Ok(v)
                } else {
                    Err("not a JSON object".into())
                }
            })
            .map_err(|e| format!("{}: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(json!({})),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

/// 各エージェントのフックのファイル (設定フォルダが無くても返す)。
pub fn files() -> Vec<(&'static str, PathBuf)> {
    TARGETS
        .into_iter()
        .map(|t| (t.0, dir_in(&home(), &real_env, t.0).join(t.3)))
        .collect()
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
            Some((a, p)) => {
                let s = read(p).map(|v| status_of(&v, a)).unwrap_or(Status::Missing);
                if *a != "claude" {
                    return (*a, s);
                }
                let dir = dir_in(h, env, "claude");
                let policy = policy_in(&dir, env);
                let s = match (s, policy) {
                    // プラグインでも動かない設定は、プラグインが入っていても止められていると出す
                    (_, Some(p)) if !p.plugin_helps => Status::Blocked {
                        plugin_helps: false,
                    },
                    _ if plugin_installed_in(&dir) => Status::Plugin,
                    (Status::Current | Status::Outdated(_), Some(p)) => Status::Blocked {
                        plugin_helps: p.plugin_helps,
                    },
                    (s, _) => s,
                };
                (*a, s)
            }
            None => (t.0, Status::NoAgent),
        })
        .collect()
}

// ---------- claude: 組織の管理設定 ----------

/// claude のフックを止めている設定。what はキーとファイル、plugin_helps はプラグインで入れれば動くか
#[derive(Clone, Debug, PartialEq)]
pub struct Policy {
    pub what: String,
    pub plugin_helps: bool,
}

#[cfg(target_os = "macos")]
const MANAGED_DIR: &str = "/Library/Application Support/ClaudeCode";
#[cfg(not(target_os = "macos"))]
const MANAGED_DIR: &str = "/etc/claude-code";

/// 組織の設定 (サーバーから配られたもの、managed-settings.json) と、ユーザーの設定にある、フックを止めるキーを探す
pub fn claude_policy() -> Option<Policy> {
    policy_in(&agent_dir("claude"), &real_env)
}

fn policy_in(claude_dir: &Path, env: Env) -> Option<Policy> {
    let mut managed: Vec<PathBuf> = vec![
        env("CLAUDE_CODE_REMOTE_SETTINGS_PATH")
            .map(PathBuf::from)
            .unwrap_or_else(|| claude_dir.join("remote-settings.json")),
    ];
    for dir in [
        Some(PathBuf::from(MANAGED_DIR)),
        env("CLAUDE_CODE_MANAGED_SETTINGS_PATH").map(PathBuf::from),
    ]
    .into_iter()
    .flatten()
    {
        managed.push(dir.join("managed-settings.json"));
        let mut more: Vec<PathBuf> = std::fs::read_dir(dir.join("managed-settings.d"))
            .into_iter()
            .flatten()
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| policy_file(p))
            .collect();
        more.sort();
        managed.extend(more);
    }
    let user = [claude_dir.join("settings.json")];
    let docs = |files: &[PathBuf]| -> Vec<(PathBuf, Value)> {
        files
            .iter()
            .filter_map(|f| {
                Some((
                    f.clone(),
                    serde_json::from_str(&std::fs::read_to_string(f).ok()?).ok()?,
                ))
            })
            .collect()
    };
    let (managed, user) = (docs(&managed), docs(&user));
    let at = |key: &str, f: &Path| Policy {
        what: format!("{key} in {}", f.display()),
        plugin_helps: false,
    };
    // プラグインでも動かないものを先に見る
    for (f, v) in &managed {
        if find_key(v, "allowManagedHooksOnly").is_some_and(|x| *x == true) {
            return Some(at("allowManagedHooksOnly", f));
        }
    }
    for (f, v) in managed.iter().chain(&user) {
        if find_key(v, "disableAllHooks").is_some_and(|x| *x == true) {
            return Some(at("disableAllHooks", f));
        }
    }
    for (f, v) in &managed {
        let locks = find_key(v, "strictPluginOnlyCustomization").is_some_and(|x| {
            *x == true || x.as_array().is_some_and(|a| a.iter().any(|s| s == "hooks"))
        });
        if locks {
            return Some(Policy {
                plugin_helps: true,
                ..at("strictPluginOnlyCustomization", f)
            });
        }
    }
    None
}

/// managed-settings.d の中で Claude Code が読むファイル (隠しファイルと .json 以外は読まない)
fn policy_file(p: &Path) -> bool {
    p.extension().is_some_and(|e| e == "json")
        && !p
            .file_name()
            .is_some_and(|n| n.to_string_lossy().starts_with('.'))
}

/// 入れ子のどこかにあるキーの値 (配られた設定は包まれていることがある)
fn find_key<'a>(v: &'a Value, key: &str) -> Option<&'a Value> {
    match v {
        Value::Object(o) => o
            .get(key)
            .or_else(|| o.values().find_map(|x| find_key(x, key))),
        Value::Array(a) => a.iter().find_map(|x| find_key(x, key)),
        _ => None,
    }
}

// ---------- claude: プラグインとして入れる ----------
// 組織の strictPluginOnlyCustomization は settings.json のフックを止めるが、プラグインのフックは止めない。
// ~/.mdeck/claude-plugin をローカルのマーケットプレイスとして claude に登録し、そこからプラグインを入れる

const PLUGIN_ID: &str = "mdeck@mdeck";

fn plugin_dir() -> PathBuf {
    home().join(".mdeck/claude-plugin")
}

/// settings.json と同じコマンドに、届いた経路の印 via を足す (両方から届いても、サーバーが二重に数えない)
fn plugin_command() -> String {
    command("claude", None).replacen(
        r#""agent":"claude","tab""#,
        r#""agent":"claude","via":"plugin","tab""#,
        1,
    )
}

/// マーケットプレイスとプラグインのファイルを dir に書く
fn write_plugin(dir: &Path) -> Result<(), String> {
    let plugin = dir.join("plugins/mdeck");
    let hooks: serde_json::Map<String, Value> = EVENTS.iter().map(|ev| (ev.to_string(), json!([{"hooks": [{"type": "command", "command": plugin_command(), "timeout": 5}]}]))).collect();
    let files = [
        (
            dir.join(".claude-plugin/marketplace.json"),
            json!({"name": "mdeck", "owner": {"name": "MornDeck"}, "plugins": [{"name": "mdeck", "source": "./plugins/mdeck", "description": "Reports Claude Code state to MornDeck"}]}),
        ),
        (
            plugin.join(".claude-plugin/plugin.json"),
            json!({"name": "mdeck", "version": format!("{}.0.0", version("claude")), "description": "Reports Claude Code state to MornDeck"}),
        ),
        (plugin.join("hooks/hooks.json"), json!({"hooks": hooks})),
    ];
    for (path, body) in files {
        std::fs::create_dir_all(path.parent().unwrap_or(dir)).map_err(|e| e.to_string())?;
        std::fs::write(
            &path,
            serde_json::to_string_pretty(&body).map_err(|e| e.to_string())? + "\n",
        )
        .map_err(|e| format!("{}: {e}", path.display()))?;
    }
    Ok(())
}

/// claude のコマンド。画面側で起動したサーバーの PATH には ~/.local/bin などが無い
fn claude_bin() -> Option<PathBuf> {
    let h = home();
    std::env::var_os("PATH")
        .into_iter()
        .flat_map(|p| std::env::split_paths(&p).collect::<Vec<_>>())
        .chain([
            h.join(".local/bin"),
            h.join(".claude/local"),
            "/opt/homebrew/bin".into(),
            "/usr/local/bin".into(),
        ])
        .map(|d| d.join("claude"))
        .find(|f| f.is_file())
}

/// `claude plugin ...` を実行する。失敗したら出力の最初の行を返す
/// deadline を過ぎたら止める (一連の操作全体の期限)
fn claude_plugin(args: &[&str], deadline: std::time::Instant) -> Result<(), String> {
    // 見つからなければ実行しない (名前だけで実行すると、作業フォルダの claude を拾いうる)
    let bin = claude_bin().ok_or("claude CLI not found")?;
    // npm で入れた claude は node を PATH から探す。画面側で起動したサーバーの PATH には足りないので、claude のフォルダを先頭に足す
    let path = std::env::join_paths(
        bin.parent()
            .map(Path::to_path_buf)
            .into_iter()
            .chain(
                std::env::var_os("PATH")
                    .map(|p| std::env::split_paths(&p).collect::<Vec<_>>())
                    .unwrap_or_default(),
            )
            .chain(["/opt/homebrew/bin".into(), "/usr/local/bin".into()]),
    )
    .unwrap_or_default();
    let mut child = std::process::Command::new(&bin)
        .env("PATH", path)
        .arg("plugin")
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| {
            format!(
                "claude CLI not found or not runnable ({}): {e}",
                bin.display()
            )
        })?;
    // 固まった CLI でサーバーの起動や設定の画面を止めない
    while child.try_wait().map_err(|e| e.to_string())?.is_none() {
        if std::time::Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!(
                "claude plugin {} did not finish in time",
                args.join(" ")
            ));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    if out.status.success() {
        return Ok(());
    }
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout)
    );
    Err(text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("failed")
        .to_string())
}

/// claude にプラグインが入っていて有効か
fn plugin_installed_in(claude_dir: &Path) -> bool {
    let read_json = |f: &str| {
        std::fs::read_to_string(claude_dir.join(f))
            .ok()
            .and_then(|s| serde_json::from_str::<Value>(&s).ok())
            .unwrap_or(Value::Null)
    };
    let installed = read_json("plugins/installed_plugins.json")["plugins"][PLUGIN_ID]
        .as_array()
        .is_some_and(|a| !a.is_empty());
    installed && read_json("settings.json")["enabledPlugins"][PLUGIN_ID] == true
}

/// プラグインとして入れる (入っていれば新しい中身にする)。settings.json の claude のフックは外す (両方から届かないように)
pub fn install_plugin() -> Result<(), String> {
    let r = install_plugin_now();
    // 入れられなかった理由を残す (起動のたびにやり直さないため、と doctor で見せるため)。入れられたら消す
    let _ = match &r {
        Ok(()) => std::fs::remove_file(plugin_failed()),
        Err(e) => std::fs::write(
            plugin_failed(),
            format!("{}\n{e}\n", env!("CARGO_PKG_VERSION")),
        ),
    };
    r
}

/// プラグインを入れられなかった理由 (1 行目は試した mdeck の版)
fn plugin_failed() -> PathBuf {
    home().join(".mdeck/claude-plugin-failed")
}

/// 前に入れられなかった理由。無ければ None
pub fn plugin_failure() -> Option<String> {
    let s = std::fs::read_to_string(plugin_failed()).ok()?;
    s.lines().nth(1).map(str::to_string)
}

fn install_plugin_now() -> Result<(), String> {
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    let dir = plugin_dir();
    write_plugin(&dir)?;
    let d = dir.to_string_lossy();
    // 既に登録してあれば add は失敗するので update する。update も失敗したら、add の理由 (組織の制限など) を返す
    if let Err(add) = claude_plugin(&["marketplace", "add", &d], deadline) {
        if add.contains("did not finish") || add.contains("not found") {
            return Err(add);
        }
        claude_plugin(&["marketplace", "update", "mdeck"], deadline).map_err(|_| add)?;
    }
    if plugin_installed_in(&agent_dir("claude")) {
        claude_plugin(&["update", PLUGIN_ID, "--scope", "user"], deadline)?;
    } else {
        claude_plugin(&["install", PLUGIN_ID, "--scope", "user"], deadline)?;
        // 無効にしてあったときのため
        if !plugin_installed_in(&agent_dir("claude")) {
            let _ = claude_plugin(&["enable", PLUGIN_ID, "--scope", "user"], deadline);
        }
    }
    // プラグインが有効になったのを確かめてから settings.json のフックを外す (どちらも無い状態にしない)
    if !plugin_installed_in(&agent_dir("claude")) {
        return Err(
            "the plugin is installed but not enabled; the hooks in settings.json are kept".into(),
        );
    }
    if let Some((_, path)) = targets(&home(), &real_env).find(|(a, _)| *a == "claude") {
        remove(&path, "claude")?;
    }
    Ok(())
}

/// `mdeck hooks settings` を選んだ印。無ければ claude のフックはプラグインで入れる
fn settings_choice() -> PathBuf {
    home().join(".mdeck/claude-hooks-settings")
}

/// claude のフックを settings.json で入れることを選んでいるか (既定はプラグイン)
pub fn prefers_settings() -> bool {
    settings_choice().exists()
}

/// claude のフックを既定の方法で入れる。既定はプラグイン。claude の CLI が無いなど、プラグインで入れられなければ settings.json に入れ、理由を返す
pub fn install_claude() -> Result<String, String> {
    let dir = agent_dir("claude");
    if !dir.is_dir() {
        return Err(format!("claude is not installed (no {})", dir.display()));
    }
    if prefers_settings() {
        return install("claude").map(|_| {
            "installed in settings.json (chosen with mdeck hooks settings). Restart claude".into()
        });
    }
    match install_plugin() {
        Ok(()) => Ok("installed as plugin. Running claude tabs report again after a restart (or /reload-plugins)".into()),
        // プラグインが入っているなら、settings.json に重ねて入れない
        Err(e) if plugin_installed() => Err(e),
        Err(e) => install("claude").map(|_| format!("could not install the plugin ({e}); installed in settings.json instead")),
    }
}

/// 起動時: settings.json に入っている claude のフックをプラグインへ移す (settings.json を選んでいなければ)。
/// プラグインが有効になったのを確かめてから settings.json の分を外す。移せなければ settings.json のまま、理由を返す
pub fn migrate_claude() -> Option<Result<(), String>> {
    if prefers_settings() {
        return None;
    }
    let dir = agent_dir("claude");
    let settings = dir.join("settings.json");
    let in_settings = read(&settings)
        .map(|v| status_of(&v, "claude"))
        .is_ok_and(|s| matches!(s, Status::Current | Status::Outdated(_)));
    if plugin_installed_in(&dir) {
        // プラグインが消えていたら書き直す (~/.mdeck/claude-plugin から、その場で読まれる)。settings.json に残っていれば外すだけ
        let _ = write_plugin_if_changed(&plugin_dir());
        return in_settings.then(|| remove(&settings, "claude").map(|_| ()));
    }
    // この版で入れられなかったなら、起動のたびにやり直さない (Enter か mdeck hooks install でやり直せる)
    let failed_here = std::fs::read_to_string(plugin_failed())
        .is_ok_and(|s| s.lines().next() == Some(env!("CARGO_PKG_VERSION")));
    (in_settings && !failed_here).then(install_plugin)
}

/// プラグインのファイルが今の中身と違うときだけ書く
fn write_plugin_if_changed(dir: &Path) -> Result<(), String> {
    let tmp = std::env::temp_dir().join(format!("mdeck-plugin-{}", std::process::id()));
    write_plugin(&tmp)?;
    let same = [
        "plugins/mdeck/hooks/hooks.json",
        "plugins/mdeck/.claude-plugin/plugin.json",
        ".claude-plugin/marketplace.json",
    ]
    .iter()
    .all(|f| std::fs::read(tmp.join(f)).ok() == std::fs::read(dir.join(f)).ok());
    let _ = std::fs::remove_dir_all(&tmp);
    if same { Ok(()) } else { write_plugin(dir) }
}

/// claude のフックを settings.json で入れることを選ぶ (false ならプラグインに戻す)
pub fn choose_settings(yes: bool) -> Result<(), String> {
    let f = settings_choice();
    if yes {
        std::fs::create_dir_all(f.parent().unwrap_or(&f))
            .and_then(|_| std::fs::write(&f, "settings\n"))
            .map_err(|e| e.to_string())
    } else {
        match std::fs::remove_file(&f) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.to_string()),
            _ => Ok(()),
        }
    }
}

/// プラグインが入っていて有効か
pub fn plugin_installed() -> bool {
    plugin_installed_in(&agent_dir("claude"))
}

/// プラグインを外す (無効にしてあるものも)。入っていなければ false
pub fn uninstall_plugin() -> Result<bool, String> {
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    let dir = agent_dir("claude");
    let recorded = std::fs::read_to_string(dir.join("plugins/installed_plugins.json"))
        .is_ok_and(|s| s.contains(PLUGIN_ID));
    let listed = std::fs::read_to_string(dir.join("plugins/known_marketplaces.json"))
        .is_ok_and(|s| s.contains(".mdeck/claude-plugin"));
    if recorded {
        claude_plugin(&["uninstall", PLUGIN_ID, "--scope", "user"], deadline)?;
    }
    if recorded || listed {
        let _ = claude_plugin(&["marketplace", "remove", "mdeck"], deadline);
    }
    Ok(recorded)
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
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)?;
        f.write_all(body.as_bytes())?;
        f.set_permissions(
            std::fs::metadata(path)
                .map(|m| m.permissions())
                .unwrap_or_else(|_| std::os::unix::fs::PermissionsExt::from_mode(0o644)),
        )?;
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
    targets(home, env)
        .map(|(agent, path)| (agent, remove(&path, agent)))
        .collect()
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
        return std::fs::remove_file(path)
            .map(|_| true)
            .map_err(|e| e.to_string());
    }
    let mut root = read(path)?;
    let before = root.clone();
    // root["hooks"] と書くと、無いときに "hooks": null が足されてしまう
    if let Some(hooks) = root.get_mut("hooks").and_then(Value::as_object_mut) {
        // MornDeck のフックを外して空になったイベントだけ消す (元から空のものは残す)
        hooks.retain(|_, list| {
            let Some(arr) = list.as_array_mut() else {
                return true;
            };
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
        assert_eq!(Status::Outdated(0).label("claude"), "hooks v0 → v2");
        assert_eq!(
            Status::Current.label("claude"),
            "installed in settings.json (hooks v2)"
        );
        assert_eq!(Status::Current.label("codex"), "installed (hooks v3)");

        install_in(&home, &no_env, "codex").unwrap();
        assert_eq!(
            read(&settings).unwrap(),
            old,
            "codex の導入で claude を触った"
        );
        install_in(&home, &no_env, "claude").unwrap();
        install_in(&home, &no_env, "claude").unwrap();
        let v = read(&settings).unwrap();
        assert_eq!(v["model"], "x");
        assert_eq!(v["hooks"]["Stop"].as_array().unwrap().len(), 2);
        assert_eq!(status_of(&v, "claude"), Status::Current);
        assert_eq!(status_of(&v, "codex"), Status::Missing);
        assert!(!home.join(".codex").exists());
        assert_eq!(
            status_in(&home, &no_env),
            vec![
                ("claude", Status::Current),
                ("codex", Status::NoAgent),
                ("copilot", Status::NoAgent)
            ]
        );
        // copilot は専用のファイルにイベントごとの bash を書き、イベント名をコマンドで渡す
        std::fs::create_dir_all(home.join(".copilot")).unwrap();
        assert_eq!(status_in(&home, &no_env)[2], ("copilot", Status::Missing));
        install_in(&home, &no_env, "copilot").unwrap();
        let c = read(&home.join(".copilot/hooks/mdeck.json")).unwrap();
        assert_eq!(c["version"], 1);
        assert!(
            c["hooks"]["agentStop"][0]["bash"]
                .as_str()
                .unwrap()
                .contains(r#""event":"Stop","#)
        );
        assert_eq!(status_in(&home, &no_env)[2], ("copilot", Status::Current));
        // 外すと他のフックと設定だけが残る
        assert_eq!(
            uninstall_in(&home, &no_env),
            vec![("claude", Ok(true)), ("copilot", Ok(true))]
        );
        assert_eq!(
            read(&settings).unwrap(),
            json!({"model": "x", "hooks": {"Stop": [{"hooks": [{"command": "other"}]}]}})
        );
        assert!(!home.join(".copilot/hooks/mdeck.json").exists());
        assert_eq!(
            uninstall_in(&home, &no_env),
            vec![("claude", Ok(false)), ("copilot", Ok(false))]
        );
        // フックの無い設定には触らない ("hooks": null を足さない)。オブジェクトでない設定は壊さず失敗にする
        for (body, ok) in [
            (r#"{"model":"x"}"#, true),
            (r#"{"hooks":{"Stop":[]}}"#, true),
            ("[]", false),
        ] {
            std::fs::write(&settings, body).unwrap();
            assert_eq!(uninstall_in(&home, &no_env)[0].1.is_ok(), ok, "{body}");
            assert_eq!(install_in(&home, &no_env, "codex"), Ok(()));
            assert_eq!(std::fs::read_to_string(&settings).unwrap(), body);
        }
        assert!(
            install_in(&home, &no_env, "claude").is_err(),
            "配列の設定を書き換えた"
        );
        // CLAUDE_CONFIG_DIR / CODEX_HOME があれば、そちらの設定フォルダを使う
        let moved = home.join("elsewhere");
        std::fs::create_dir_all(&moved).unwrap();
        let env = |k: &str| (k == "CODEX_HOME").then(|| moved.to_string_lossy().into_owned());
        assert_eq!(status_in(&home, &env)[1], ("codex", Status::Missing));
        install_in(&home, &env, "codex").unwrap();
        assert_eq!(
            status_of(&read(&moved.join("hooks.json")).unwrap(), "codex"),
            Status::Current
        );
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
        assert!(
            link.symlink_metadata().unwrap().file_type().is_symlink(),
            "リンクを実ファイルに置き換えた"
        );
        assert_eq!(
            real.metadata().unwrap().permissions().mode() & 0o777,
            0o600,
            "権限が広がった"
        );
        assert_eq!(status_of(&read(&real).unwrap(), "claude"), Status::Current);
        assert_eq!(
            std::fs::read_to_string(home.join("dotfiles/settings.json.mdeck-bak")).unwrap(),
            r#"{"env": {"KEY": "x"}}"#,
            "最初の中身を残していない"
        );
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
            std::io::BufReader::new(c.try_clone().unwrap())
                .read_line(&mut line)
                .unwrap();
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
            child
                .stdin
                .take()
                .unwrap()
                .write_all(b"{\n  \"hook_event_name\": \"Stop\",\n  \"session_id\": \"s-1\"\n}\n")
                .unwrap();
            child.wait().unwrap()
        };
        assert!(run(&[]).success(), "MornDeck の外では何もせず成功する");
        assert!(run(&[("MDECK_TAB", "7"), ("MDECK_SOCK", sock.to_str().unwrap())]).success());
        let got: Value = serde_json::from_str(&server.join().unwrap()).unwrap();
        assert_eq!(
            (
                got["cmd"].as_str(),
                got["tab"].as_u64(),
                got["agent"].as_str()
            ),
            (Some("hook"), Some(7), Some("claude"))
        );
        assert_eq!(got["payload"]["hook_event_name"], "Stop");
        assert_eq!(
            status_of(
                &json!({"hooks": {"Stop": [{"hooks": [{"command": command("claude", None)}]}]}}),
                "claude"
            ),
            Status::Outdated(2)
        );
        // copilot のコマンドはイベント名を付けて送る
        let sock2 = dir.join("c.sock");
        let listener = std::os::unix::net::UnixListener::bind(&sock2).unwrap();
        let server = std::thread::spawn(move || {
            let (c, _) = listener.accept().unwrap();
            let mut line = String::new();
            std::io::BufReader::new(c.try_clone().unwrap())
                .read_line(&mut line)
                .unwrap();
            let _ = writeln!(&c, "{{\"ok\":true}}");
            line
        });
        let mut child = std::process::Command::new("/bin/sh")
            .args(["-c", &command("copilot", Some("Stop"))])
            .envs([("MDECK_TAB", "3"), ("MDECK_SOCK", sock2.to_str().unwrap())])
            .stdin(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(b"{\"sessionId\": \"s-2\", \"stopReason\": \"end_turn\"}\n")
            .unwrap();
        assert!(child.wait().unwrap().success());
        let got: Value = serde_json::from_str(&server.join().unwrap()).unwrap();
        assert_eq!(
            (
                got["agent"].as_str(),
                got["event"].as_str(),
                got["tab"].as_u64()
            ),
            (Some("copilot"), Some("Stop"), Some(3))
        );
        assert_eq!(got["payload"]["sessionId"], "s-2");
        // codex のコマンドは、タブの番号が無くても (常駐のサーバーの環境で動いても) 送る。番号は文字列の手がかり
        let sock3 = dir.join("x.sock");
        let listener = std::os::unix::net::UnixListener::bind(&sock3).unwrap();
        let server = std::thread::spawn(move || {
            let (c, _) = listener.accept().unwrap();
            let mut line = String::new();
            std::io::BufReader::new(c.try_clone().unwrap())
                .read_line(&mut line)
                .unwrap();
            let _ = writeln!(&c, "{{\"ok\":true}}");
            line
        });
        let mut child = std::process::Command::new("/bin/sh")
            .args(["-c", &command("codex", None)])
            .env_remove("MDECK_TAB")
            .env("MDECK_SOCK", sock3.to_str().unwrap())
            .stdin(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(
                b"{\"hook_event_name\": \"Stop\", \"session_id\": \"s-3\", \"cwd\": \"/x\"}\n",
            )
            .unwrap();
        assert!(child.wait().unwrap().success());
        let got: Value = serde_json::from_str(&server.join().unwrap()).unwrap();
        assert_eq!(
            (
                got["agent"].as_str(),
                got["tab"].as_str(),
                got["payload"]["session_id"].as_str()
            ),
            (Some("codex"), Some(""), Some("s-3"))
        );
        assert_eq!(cmd_version(&command("codex", None), "codex"), Some(3));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn org_policy_is_found_and_plugin_files_mark_their_path() {
        let dir = std::env::temp_dir().join(format!("mdeck-policy-{}", std::process::id()));
        let claude = dir.join(".claude");
        std::fs::create_dir_all(&claude).unwrap();
        let none = |_: &str| None;
        assert_eq!(policy_in(&claude, &none), None);
        // 配られた設定 (包まれていても見つける)
        std::fs::write(
            claude.join("remote-settings.json"),
            r#"{"settings": {"strictPluginOnlyCustomization": ["hooks"]}}"#,
        )
        .unwrap();
        assert_eq!(
            policy_in(&claude, &none).map(|p| p.plugin_helps),
            Some(true)
        );
        std::fs::write(
            claude.join("remote-settings.json"),
            r#"{"strictPluginOnlyCustomization": ["skills"]}"#,
        )
        .unwrap();
        assert_eq!(
            policy_in(&claude, &none),
            None,
            "hooks を含まない指定で止めた"
        );
        std::fs::write(
            claude.join("remote-settings.json"),
            r#"{"strictPluginOnlyCustomization": true, "allowManagedHooksOnly": true}"#,
        )
        .unwrap();
        assert_eq!(
            policy_in(&claude, &none).map(|p| p.plugin_helps),
            Some(false),
            "プラグインでも動かない設定を先に見ていない"
        );
        std::fs::remove_file(claude.join("remote-settings.json")).unwrap();
        std::fs::write(claude.join("settings.json"), r#"{"disableAllHooks": true}"#).unwrap();
        assert!(
            policy_in(&claude, &none)
                .is_some_and(|p| !p.plugin_helps && p.what.starts_with("disableAllHooks"))
        );
        // settings.json に入っていても、止められていれば Blocked と出す
        std::fs::write(claude.join("settings.json"), "{}").unwrap();
        install_in(&dir, &none, "claude").unwrap();
        std::fs::write(
            claude.join("remote-settings.json"),
            r#"{"strictPluginOnlyCustomization": ["hooks"]}"#,
        )
        .unwrap();
        assert_eq!(
            status_in(&dir, &none)[0],
            ("claude", Status::Blocked { plugin_helps: true })
        );
        // プラグインが入っていればそちら
        std::fs::create_dir_all(claude.join("plugins")).unwrap();
        std::fs::write(
            claude.join("plugins/installed_plugins.json"),
            r#"{"version": 2, "plugins": {"mdeck@mdeck": [{"scope": "user"}]}}"#,
        )
        .unwrap();
        // 記録があっても、settings.json で有効になっていなければ入っていない扱い
        assert_ne!(status_in(&dir, &none)[0].1, Status::Plugin);
        std::fs::write(
            claude.join("settings.json"),
            r#"{"enabledPlugins": {"mdeck@mdeck": true}}"#,
        )
        .unwrap();
        assert_eq!(status_in(&dir, &none)[0], ("claude", Status::Plugin));
        // プラグインでも動かない設定なら、プラグインが入っていても止められていると出す
        std::fs::write(
            claude.join("remote-settings.json"),
            r#"{"allowManagedHooksOnly": true}"#,
        )
        .unwrap();
        assert_eq!(
            status_in(&dir, &none)[0],
            (
                "claude",
                Status::Blocked {
                    plugin_helps: false
                }
            )
        );
        assert!(
            policy_file(Path::new("/x/a.json"))
                && !policy_file(Path::new("/x/a.json.bak"))
                && !policy_file(Path::new("/x/.a.json"))
        );
        // プラグインのフックは settings.json と同じコマンドに via: plugin を付けたもの
        let p = dir.join("plugin");
        write_plugin(&p).unwrap();
        let h = read(&p.join("plugins/mdeck/hooks/hooks.json")).unwrap();
        for ev in EVENTS {
            let c = h["hooks"][ev][0]["hooks"][0]["command"].as_str().unwrap();
            assert!(
                c.contains(r#""agent":"claude","via":"plugin","tab":%s"#),
                "{ev}: {c}"
            );
        }
        assert_eq!(
            read(&p.join(".claude-plugin/marketplace.json")).unwrap()["plugins"][0]["source"],
            "./plugins/mdeck"
        );
        assert_eq!(
            read(&p.join("plugins/mdeck/.claude-plugin/plugin.json")).unwrap()["name"],
            "mdeck"
        );
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
