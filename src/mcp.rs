//! stdio の MCP サーバー。ツール呼び出しをそのままソケット API へ渡す。stdout は JSON-RPC 専用。

use std::io::{BufRead, Write};

use serde_json::{json, Value};

const TAB: &str = "タブの番号 (数値) か名前 (文字列)";

fn tools() -> Value {
    let tab = json!({"type": ["string", "integer"], "description": TAB});
    let hook = json!({"type": "object", "required": ["url"], "properties": {
        "url": {"type": "string"}, "headers": {"type": "object", "additionalProperties": {"type": "string"}}}});
    let timeout = json!({"type": "integer", "description": "待つ上限 (ms)。既定 120000。時間切れは ok:true / state:working / timeout:true で返るので wait を呼び直す"});
    json!([
        {"name": "list_tabs", "description": "タブ一覧 (id, name, title, owner, cwd, agent, state, active, last_used (unix 秒))",
         "inputSchema": {"type": "object", "properties": {}}},
        {"name": "new_tab", "description": "タブを cwd で作り、agent を起動して準備完了まで待つ。表示中のタブは切り替えない。確認画面で止まると state=blocked と options を返すので select_choice で答える。agent も run も渡さなければ claude を起動する。シェルだけのタブは run に \"\" (空文字) か実行するコマンドを渡す",
         "inputSchema": {"type": "object", "required": ["cwd"], "properties": {
            "name": {"type": "string", "description": "重複不可。空白なし 1〜32 文字、数字だけは不可。Bot ごとに固定する"},
            "agent": {"type": ["string", "null"], "enum": ["claude", "codex", null], "description": "起動するエージェント。null ならエージェント無し (run を使う)"},
            "args": {"type": "string", "description": "エージェントに付ける引数"},
            "run": {"type": "string", "description": "agent を渡さないとき、シェルで実行するコマンド。空文字ならシェルのまま"},
            "wait": {"type": "boolean", "description": "agent の準備完了まで待つ (既定 true)"},
            "cwd": {"type": "string", "description": "起動するフォルダ (必須、絶対パス)"},
            "owner": {"type": "string", "description": "開いた Bot の名前。close_tab の owner でまとめて閉じられる"},
            "timeout_ms": timeout}}},
        {"name": "send", "description": "エージェントに指示を送る。wait=true (既定) なら done / waiting まで待ち、answer か options を返す。wait=false なら expect_seq (この依頼の回答に付く seq) を返す。作業中のタブには busy で断る",
         "inputSchema": {"type": "object", "required": ["tab", "text"], "properties": {
            "tab": tab, "text": {"type": "string"}, "wait": {"type": "boolean", "description": "既定 true"},
            "enter": {"type": "boolean", "description": "最後に Enter を送る (既定は raw でなければ true)"},
            "raw": {"type": "boolean", "description": "貼り付け扱いせずキー列として送る (例: \"\\u001b\" で Esc)"},
            "request_id": {"type": "string", "description": "依頼ごとの ID。同じ ID の送り直しは打ち込まずに最初の受付結果を返す"},
            "force": {"type": "boolean", "description": "作業中でも送る"}, "timeout_ms": timeout}}},
        {"name": "select_choice", "description": "提示中の選択肢 (choices / blocked の options) を番号で選び、次に止まるまで待つ。番号の無い選択肢は矢印キーで選ぶ",
         "inputSchema": {"type": "object", "required": ["tab", "n"], "properties": {
            "tab": tab, "n": {"type": "integer"}, "wait": {"type": "boolean", "description": "次に止まるまで待つ (既定 true)"}, "timeout_ms": timeout}}},
        {"name": "wait", "description": "作業が終わるまで待ち、answer か options を返す",
         "inputSchema": {"type": "object", "required": ["tab"], "properties": {"tab": tab, "timeout_ms": timeout}}},
        {"name": "answer", "description": "最新の回答。作業中は text が null。seq は回答ごとに増える",
         "inputSchema": {"type": "object", "required": ["tab"], "properties": {"tab": tab}}},
        {"name": "choices", "description": "提示中の選択肢 (options: n, label, selected) と、フックが渡した質問内容 (question)",
         "inputSchema": {"type": "object", "required": ["tab"], "properties": {"tab": tab}}},
        {"name": "read", "description": "表示中の画面テキスト",
         "inputSchema": {"type": "object", "required": ["tab"], "properties": {"tab": tab}}},
        {"name": "webhooks", "description": "done と選択肢の提示を POST する先。owner を渡すとその Bot のタブの通知だけが届く専用の一覧を扱い、全体や他の Bot の登録には触れない (Bot は自分の owner を渡す)。owner 無しは全タブの通知が届く全体の一覧。add で 1 件追加 (同じ url は置き換え)、remove で url を 1 件削除、set で丸ごと置き換え。何も渡さなければ一覧 (ヘッダーは名前だけ)",
         "inputSchema": {"type": "object", "properties": {
            "owner": {"type": "string", "description": "new_tab に渡した owner。省略すると全体の一覧"},
            "add": hook.clone(),
            "remove": {"type": "string", "description": "削除する url"},
            "set": {"type": "array", "items": hook}}}},
        {"name": "rename_tab", "description": "タブに名前を付ける。空文字か省略で外す",
         "inputSchema": {"type": "object", "required": ["tab"], "properties": {"tab": tab, "name": {"type": "string"}}}},
        {"name": "select_tab", "description": "画面に表示するタブを切り替える",
         "inputSchema": {"type": "object", "required": ["tab"], "properties": {"tab": tab}}},
        {"name": "close_tab", "description": "タブを閉じる。tab の代わりに owner を渡すとその Bot のタブをまとめて閉じる",
         "inputSchema": {"type": "object", "properties": {"tab": tab, "owner": {"type": "string"}}}}
    ])
}

fn to_request(name: &str, mut args: Value) -> Option<Value> {
    // arguments を JSON 文字列で送ってくるクライアントがある
    if let Some(s) = args.as_str() {
        args = serde_json::from_str(s).unwrap_or(Value::Null);
    }
    if !args.is_object() {
        args = json!({});
    }
    let cmd = match name {
        "list_tabs" => "list",
        "new_tab" => {
            // 既存の Bot は agent 省略で claude を開く。agent も run も無いときだけ補う
            if args.get("agent").is_none() && args.get("run").is_none() {
                args["agent"] = json!("claude");
            }
            "new"
        }
        "send" => {
            if args["wait"].is_null() {
                args["wait"] = json!(true);
            }
            "send"
        }
        "select_choice" => {
            args["n"].as_u64()?;
            if args["wait"].is_null() {
                args["wait"] = json!(true);
            }
            "choose"
        }
        "close_tab" => "close",
        "rename_tab" => "rename",
        "select_tab" => "select",
        "webhooks" => "webhooks",
        "wait" | "answer" | "choices" | "read" => name,
        _ => return None,
    };
    // Bot 側の接続が先に切れないよう、MCP からの待ちは短めにして呼び直してもらう
    if matches!(cmd, "send" | "wait" | "new" | "choose") && args["timeout_ms"].is_null() {
        args["timeout_ms"] = json!(if cmd == "new" { 60_000 } else { 120_000 });
    }
    args["cmd"] = json!(cmd);
    Some(args)
}

fn handle(sock: &str, msg: &Value) -> Option<Value> {
    let id = msg.get("id")?.clone(); // id の無いものは通知なので返さない
    let result = match msg["method"].as_str().unwrap_or("") {
        "initialize" => json!({
            "protocolVersion": msg["params"]["protocolVersion"].as_str().unwrap_or("2025-06-18"),
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "mdeck", "version": env!("CARGO_PKG_VERSION")}
        }),
        "ping" => json!({}),
        "tools/list" => json!({"tools": tools()}),
        "tools/call" => {
            let name = msg["params"]["name"].as_str().unwrap_or("");
            let resp = match to_request(name, msg["params"]["arguments"].clone()) {
                Some(req) => crate::request(sock, &req).unwrap_or_else(|e| json!({"ok": false, "error": e.to_string()})),
                None => json!({"ok": false, "error": format!("unknown tool: {name}")}),
            };
            let is_error = resp["ok"] != true;
            json!({"content": [{"type": "text", "text": resp.to_string()}], "isError": is_error})
        }
        m => return Some(json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": format!("method not found: {m}")}})),
    };
    Some(json!({"jsonrpc": "2.0", "id": id, "result": result}))
}

/// stdout は JSON-RPC 専用で stderr も ssh 越しでは見えないので、経路の調査用にファイルへ残す。
fn log(sock: &str, msg: &str) {
    let path = std::path::Path::new(sock).with_file_name("mcp.log");
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        let _ = writeln!(f, "{now} pid={} {msg}", std::process::id());
    }
}

pub fn run(sock: &str) -> std::io::Result<()> {
    let s = sock.to_string();
    std::panic::set_hook(Box::new(move |p| log(&s, &format!("panic: {p}"))));
    log(sock, &format!("start ssh={}", std::env::var("SSH_CONNECTION").unwrap_or_default()));
    let mut out = std::io::stdout();
    let end = (|| -> String {
        for line in std::io::stdin().lock().lines() {
            let Ok(line) = line else { return "stdin error".into() };
            if line.trim().is_empty() {
                continue;
            }
            let start = std::time::Instant::now();
            let (resp, what) = match serde_json::from_str::<Value>(&line) {
                Ok(msg) => {
                    let what = format!("{} {}", msg["method"].as_str().unwrap_or("?"), msg["params"]["name"].as_str().unwrap_or(""));
                    (handle(sock, &msg), what)
                }
                Err(e) => (Some(json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32700, "message": e.to_string()}})), "parse error".into()),
            };
            let ok = resp.as_ref().map(|r| r["result"]["isError"] != true && r["error"].is_null());
            log(sock, &format!("{what} ok={ok:?} {}ms", start.elapsed().as_millis()));
            if let Some(r) = resp {
                // ホストが stdout を閉じたら静かに終わる
                if writeln!(out, "{r}").and_then(|_| out.flush()).is_err() {
                    return "stdout closed".into();
                }
            }
        }
        "stdin closed".into()
    })();
    log(sock, &format!("exit: {end}"));
    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    #[test]
    fn select_choice_sends_number_as_raw_key_and_waits() {
        let r = super::to_request("select_choice", json!({"tab": "bot", "n": 2})).unwrap();
        assert_eq!(r, json!({"tab": "bot", "n": 2, "wait": true, "cmd": "choose", "timeout_ms": 120000}));
        assert!(super::to_request("nope", json!({})).is_none());
        let r = super::to_request("new_tab", json!("{\"name\":\"bot\",\"cwd\":\"/tmp\"}")).unwrap();
        assert_eq!((r["name"].as_str(), r["agent"].as_str()), (Some("bot"), Some("claude")));
        assert_eq!(super::to_request("send", json!({"tab": 1, "text": "x"})).unwrap()["wait"], true);
    }

    #[test]
    fn new_tab_fills_claude_only_without_agent_and_run() {
        let r = |a| super::to_request("new_tab", a).unwrap();
        assert_eq!(r(json!({"cwd": "/tmp"}))["agent"], "claude");
        let shell = r(json!({"cwd": "/tmp", "run": ""}));
        assert!(shell.get("agent").is_none());
        assert_eq!(shell["run"], "");
        assert!(r(json!({"cwd": "/tmp", "agent": null}))["agent"].is_null());
        assert_eq!(r(json!({"cwd": "/tmp", "agent": "codex"}))["agent"], "codex");
        assert_eq!(super::to_request("select_choice", json!({"tab": 1, "n": 1, "wait": false})).unwrap()["wait"], false);
        assert_eq!(super::to_request("rename_tab", json!({"tab": 1, "name": "a"})).unwrap()["cmd"], "rename");
        assert_eq!(super::to_request("select_tab", json!({"tab": 1})).unwrap()["cmd"], "select");
        let names: Vec<String> = super::tools().as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap().to_string()).collect();
        for n in &names {
            assert!(super::to_request(n, json!({"tab": 1, "n": 1, "cwd": "/tmp"})).is_some(), "{n}");
        }
    }
}
