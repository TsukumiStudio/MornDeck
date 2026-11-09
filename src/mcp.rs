//! stdio の MCP サーバー。ツール呼び出しをそのままソケット API へ渡す。stdout は JSON-RPC 専用。

use std::io::{BufRead, Write};

use serde_json::{json, Value};

const TAB: &str = "タブの番号 (数値) か名前 (文字列)";

fn tools() -> Value {
    let tab = json!({"type": ["string", "integer"], "description": TAB});
    let timeout = json!({"type": "integer", "description": "待つ上限 (ms)。既定 120000。時間切れは ok:true / state:working / timeout:true で返るので wait を呼び直す"});
    json!([
        {"name": "list_tabs", "description": "タブ一覧 (id, name, agent, state)",
         "inputSchema": {"type": "object", "properties": {}}},
        {"name": "new_tab", "description": "名前付きのタブを作り、claude / codex を起動して準備完了まで待つ。表示中のタブは切り替えない。確認画面で止まると error=blocked と options を返す",
         "inputSchema": {"type": "object", "required": ["name"], "properties": {
            "name": {"type": "string", "description": "重複不可。空白なし 1〜32 文字。Bot ごとに固定する"},
            "agent": {"type": "string", "enum": ["claude", "codex"]},
            "args": {"type": "string", "description": "エージェントに付ける引数"},
            "cwd": {"type": "string"},
            "timeout_ms": timeout}}},
        {"name": "send", "description": "エージェントに指示を送る。wait=true (既定) なら done / waiting まで待ち、answer か options を返す。wait=false なら expect_seq (この依頼の回答に付く seq) を返す。作業中のタブには busy で断る",
         "inputSchema": {"type": "object", "required": ["tab", "text"], "properties": {
            "tab": tab, "text": {"type": "string"}, "wait": {"type": "boolean"},
            "force": {"type": "boolean", "description": "作業中でも送る"}, "timeout_ms": timeout}}},
        {"name": "select_choice", "description": "提示中の選択肢を番号で選び、次に止まるまで待つ",
         "inputSchema": {"type": "object", "required": ["tab", "n"], "properties": {
            "tab": tab, "n": {"type": "integer"}, "timeout_ms": timeout}}},
        {"name": "wait", "description": "作業が終わるまで待ち、answer か options を返す",
         "inputSchema": {"type": "object", "required": ["tab"], "properties": {"tab": tab, "timeout_ms": timeout}}},
        {"name": "answer", "description": "最新の回答。作業中は text が null。seq は回答ごとに増える",
         "inputSchema": {"type": "object", "required": ["tab"], "properties": {"tab": tab}}},
        {"name": "choices", "description": "提示中の選択肢",
         "inputSchema": {"type": "object", "required": ["tab"], "properties": {"tab": tab}}},
        {"name": "read", "description": "表示中の画面テキスト",
         "inputSchema": {"type": "object", "required": ["tab"], "properties": {"tab": tab}}},
        {"name": "webhooks", "description": "done と選択肢の提示を POST する先を丸ごと置き換える。set を省くと現在の一覧 (ヘッダーは名前だけ)。空配列で解除",
         "inputSchema": {"type": "object", "properties": {"set": {"type": "array", "items": {"type": "object", "required": ["url"], "properties": {
            "url": {"type": "string"}, "headers": {"type": "object", "additionalProperties": {"type": "string"}}}}}}}},
        {"name": "close_tab", "description": "タブを閉じる",
         "inputSchema": {"type": "object", "required": ["tab"], "properties": {"tab": tab}}}
    ])
}

fn to_request(name: &str, mut args: Value) -> Option<Value> {
    let cmd = match name {
        "list_tabs" => "list",
        "new_tab" => {
            if args["agent"].is_null() {
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
            args["text"] = json!(args["n"].as_u64()?.to_string());
            args["raw"] = json!(true);
            args["wait"] = json!(true);
            "send"
        }
        "close_tab" => "close",
        "webhooks" => "webhooks",
        "wait" | "answer" | "choices" | "read" => name,
        _ => return None,
    };
    if !args.is_object() {
        args = json!({});
    }
    // Bot 側の接続が先に切れないよう、MCP からの待ちは短めにして呼び直してもらう
    if matches!(cmd, "send" | "wait" | "new") && args["timeout_ms"].is_null() {
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

pub fn run(sock: &str) -> std::io::Result<()> {
    let mut out = std::io::stdout();
    for line in std::io::stdin().lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let resp = match serde_json::from_str::<Value>(&line) {
            Ok(msg) => handle(sock, &msg),
            Err(e) => Some(json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32700, "message": e.to_string()}})),
        };
        if let Some(r) = resp {
            // ホストが stdout を閉じたら静かに終わる
            if writeln!(out, "{r}").and_then(|_| out.flush()).is_err() {
                return Ok(());
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    #[test]
    fn select_choice_sends_number_as_raw_key_and_waits() {
        let r = super::to_request("select_choice", json!({"tab": "bot", "n": 2})).unwrap();
        assert_eq!(r, json!({"tab": "bot", "n": 2, "text": "2", "raw": true, "wait": true, "cmd": "send", "timeout_ms": 120000}));
        assert!(super::to_request("nope", json!({})).is_none());
        assert_eq!(super::to_request("send", json!({"tab": 1, "text": "x"})).unwrap()["wait"], true);
    }
}
