//! stdio の MCP サーバー。ツール呼び出しをそのままソケット API へ渡す。stdout は JSON-RPC 専用。

use std::io::{BufRead, Write};

use serde_json::{json, Value};

const TAB: &str = "タブの番号 (数値) か名前";

/// ツール名はソケット API のコマンド名と同じ。引数も同じ (cmd 以外)
fn tools() -> Value {
    let tab = json!({"type": "string", "description": TAB});
    let owner = json!({"type": "string", "description": "Bot の名前 (owner)。この名前の通知先 (webhook) に、この名前のタブの完了と選択肢が届く"});
    let wait_ms = json!({"type": "integer", "description": "待つ上限 (ms)。既定 120000。時間切れでも ok:true で timed_out:true と今の state を返すので、wait を呼び直す"});
    let turn = "戻り値: {ok, tab, state, seq, answer, timed_out, question?, options?}。state が done なら answer が回答、waiting なら question と options (choose で答える)";
    json!([
        {"name": "list_tabs", "description": "タブの一覧。各タブは {id, name, title, owner, cwd, agent, state, active, last_used, auto_yes, yolo, unread}",
         "inputSchema": {"type": "object", "properties": {}}},
        {"name": "open_tab", "description": format!("cwd でタブを開き、エージェントを起動して準備できるまで待つ (表示中のタブは切り替えない)。agent も run も無ければ claude。シェルだけのタブは run に \"\" か実行するコマンド。起動時の確認画面 (フォルダの信頼など) で止まると state: blocked と options を返すので choose で答える。時間切れでもタブは作られている (作り直さず wait か choose を続ける)。{turn}"),
         "inputSchema": {"type": "object", "required": ["cwd"], "properties": {
            "cwd": {"type": "string", "description": "起動するフォルダの絶対パス"},
            "name": {"type": "string", "description": "タブ名。重複不可、空白なし 1〜32 文字、数字だけは不可"},
            "owner": owner.clone(),
            "agent": {"type": "string", "enum": ["claude", "codex", "copilot"], "description": "起動するエージェント (run と同時には渡さない)"},
            "args": {"type": "string", "description": "エージェントに付ける引数 (シェルへそのまま渡る)"},
            "run": {"type": "string", "description": "agent の代わりにシェルで実行するコマンド。\"\" ならシェルのまま"},
            "yolo": {"type": "boolean", "description": "確認もサンドボックスも無しで起動する (claude --dangerously-skip-permissions / codex --dangerously-bypass-approvals-and-sandbox / copilot --yolo)。再起動後の再開でも付いたまま。既定 false"},
            "auto_yes": {"type": "boolean", "description": "実行の確認 (Yes / No のメニュー) に自動で最初の Yes を選ぶ。削除や push の確認にも答える。再起動後も続く。既定 false"},
            "wait": {"type": "boolean", "description": "準備できるまで待つ (既定 true)"},
            "timeout_ms": {"type": "integer", "description": "待つ上限 (ms)。既定 60000"}}}},
        {"name": "close_tab", "description": "タブを閉じる。中のエージェントも終わる",
         "inputSchema": {"type": "object", "required": ["tab"], "properties": {"tab": tab.clone()}}},
        {"name": "close_tabs", "description": "owner のタブをまとめて閉じる (Bot が落ちて置き去りにしたタブの片付け)",
         "inputSchema": {"type": "object", "required": ["owner"], "properties": {"owner": owner.clone()}}},
        {"name": "update_tab", "description": "タブの名前・owner・auto_yes を変える。渡したものだけ変え、name と owner は \"\" で外す",
         "inputSchema": {"type": "object", "required": ["tab"], "properties": {
            "tab": tab.clone(), "name": {"type": "string"}, "owner": owner.clone(), "auto_yes": {"type": "boolean"}}}},
        {"name": "show_tab", "description": "人が見ている画面に出すタブを切り替える",
         "inputSchema": {"type": "object", "required": ["tab"], "properties": {"tab": tab.clone()}}},
        {"name": "send", "description": format!("エージェントに指示を送る (貼り付けて Enter)。既定は回答か選択肢が出るまで待つ。作業中のタブは busy で断る。wait: false なら expect_seq (この依頼の回答の seq) を返す。{turn}"),
         "inputSchema": {"type": "object", "required": ["tab", "text"], "properties": {
            "tab": tab.clone(), "text": {"type": "string"},
            "wait": {"type": "boolean", "description": "回答か選択肢が出るまで待つ (既定 true)"},
            "request_id": {"type": "string", "description": "依頼ごとの ID。同じ ID の送り直しは打ち込まず、最初の受付結果を返す"},
            "force": {"type": "boolean", "description": "作業中でも送る"}, "timeout_ms": wait_ms.clone()}}},
        {"name": "press_keys", "description": "キーをそのまま送る (例: \"\\u001b\" で Esc、\"\\r\" で Enter)。作業中かどうかの扱いは変えない",
         "inputSchema": {"type": "object", "required": ["tab", "keys"], "properties": {"tab": tab.clone(), "keys": {"type": "string"}}}},
        {"name": "choose", "description": format!("提示中の選択肢 (waiting / blocked の options) を番号で選び、既定は次に止まるまで待つ。{turn}"),
         "inputSchema": {"type": "object", "required": ["tab", "n"], "properties": {
            "tab": tab.clone(), "n": {"type": "integer"}, "wait": {"type": "boolean", "description": "既定 true"}, "timeout_ms": wait_ms.clone()}}},
        {"name": "wait", "description": format!("作業が終わる (done / waiting) まで待つ。{turn}"),
         "inputSchema": {"type": "object", "required": ["tab"], "properties": {"tab": tab.clone(), "timeout_ms": wait_ms}}},
        {"name": "get_answer", "description": format!("待たずに今の回答を返す (作業中は answer が null)。{turn}"),
         "inputSchema": {"type": "object", "required": ["tab"], "properties": {"tab": tab.clone()}}},
        {"name": "get_choices", "description": "提示中の選択肢 {n, label, selected, numbered} と、確認を出したツール (question: {tool, input})",
         "inputSchema": {"type": "object", "required": ["tab"], "properties": {"tab": tab.clone()}}},
        {"name": "get_screen", "description": "表示中の画面の各行 (lines)",
         "inputSchema": {"type": "object", "required": ["tab"], "properties": {"tab": tab}}},
        {"name": "list_webhooks", "description": "通知先の一覧 (ヘッダーは名前だけ)。owner を渡すとその Bot の一覧、省略すると全タブの通知が届く全体の一覧",
         "inputSchema": {"type": "object", "properties": {"owner": owner.clone()}}},
        {"name": "add_webhook", "description": "通知先を足す。完了 (state: done)・選択肢 (choices)・自動 Yes (auto_yes) を POST する。同じ URL はヘッダーごと置き換える。URL は Bot ごとに固定する (依頼ごとに変えると溜まる)。失敗しても再送しない",
         "inputSchema": {"type": "object", "required": ["url"], "properties": {
            "owner": owner.clone(), "url": {"type": "string"},
            "headers": {"type": "object", "additionalProperties": {"type": "string"}, "description": "例: {\"Authorization\": \"Bearer …\"}"}}}},
        {"name": "remove_webhook", "description": "通知先を消す",
         "inputSchema": {"type": "object", "required": ["url"], "properties": {"owner": owner.clone(), "url": {"type": "string"}}}},
        {"name": "test_webhooks", "description": "通知先にテストの通知 ({event: \"test\", owner, message}) を今すぐ POST し、1 件ずつ {url (末尾は伏せる), http, error} を返す。届かないものがあれば ok:false",
         "inputSchema": {"type": "object", "properties": {"owner": owner}}}
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
    if !tools().as_array()?.iter().any(|t| t["name"] == name) {
        return None;
    }
    // Bot 側の接続が先に切れないよう、MCP からの待ちは短めにして呼び直してもらう
    if matches!(name, "open_tab" | "send" | "choose" | "wait") && args["timeout_ms"].is_null() {
        args["timeout_ms"] = json!(if name == "open_tab" { 60_000 } else { 120_000 });
    }
    args["cmd"] = json!(name);
    Some(args)
}

fn handle(sock: &str, msg: &Value) -> Option<Value> {
    let id = msg.get("id")?.clone(); // id の無いものは通知なので返さない
    let result = match msg["method"].as_str().unwrap_or("") {
        "initialize" => json!({
            "protocolVersion": msg["params"]["protocolVersion"].as_str().unwrap_or("2025-06-18"),
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "mdeck", "version": env!("CARGO_PKG_VERSION")},
            "instructions": "MornDeck のタブで claude / codex / copilot を動かす。open_tab で開き、send で指示を送る。戻り値の state が done なら answer が回答、waiting なら options を choose で選ぶ、working のまま timed_out なら wait を呼び直す。完了の通知は add_webhook で受け取る (owner を付けたタブだけ届く)。"
        }),
        "ping" => json!({}),
        "tools/list" => json!({"tools": tools()}),
        "tools/call" => {
            let name = msg["params"]["name"].as_str().unwrap_or("");
            let resp = match to_request(name, msg["params"]["arguments"].clone()) {
                Some(req) => crate::request(sock, &req).unwrap_or_else(|e| json!({"ok": false, "error": {"code": "failed", "message": e.to_string()}})),
                None => json!({"ok": false, "error": {"code": "unknown_cmd", "message": format!("知らないツール: {name}")}}),
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
    crate::server::append_log(&std::path::Path::new(sock).with_file_name("mcp.log"), &format!("pid={} {msg}", std::process::id()));
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
    fn tool_names_are_api_commands_and_mcp_shortens_waits() {
        let r = super::to_request("choose", json!({"tab": "bot", "n": 2})).unwrap();
        assert_eq!(r, json!({"tab": "bot", "n": 2, "cmd": "choose", "timeout_ms": 120000}));
        // n が無くてもサーバーまで渡す (サーバーが「n が必要」と返す)
        assert!(super::to_request("choose", json!({"tab": 1})).is_some());
        assert!(super::to_request("nope", json!({})).is_none());
        let r = super::to_request("open_tab", json!("{\"name\":\"bot\",\"cwd\":\"/tmp\"}")).unwrap();
        assert_eq!((r["cmd"].as_str(), r["timeout_ms"].as_u64()), (Some("open_tab"), Some(60000)));
        assert!(super::to_request("get_screen", json!({"tab": 1})).unwrap()["timeout_ms"].is_null());
        let names: Vec<String> = super::tools().as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap().to_string()).collect();
        assert_eq!(names.len(), 17);
        for n in &names {
            assert_eq!(super::to_request(n, json!({})).unwrap()["cmd"], n.as_str());
        }
    }
}
