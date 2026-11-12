//! stdio の MCP サーバー。ツール呼び出しをそのままソケット API へ渡す。stdout は JSON-RPC 専用。

use std::io::{BufRead, Write};

use serde_json::{Value, json};

const TAB: &str = "Tab id or name, as a string (\"3\" or \"work\")";

/// ツール名はソケット API のコマンド名と同じ。引数も同じ (cmd 以外)。説明は承認画面にも出るので英語
fn tools() -> Value {
    let tab = json!({"type": "string", "description": TAB});
    let owner = json!({"type": "string", "description": "Owner name of the tab. Webhooks registered under this owner receive the events of tabs with the same owner"});
    let wait_ms = json!({"type": "integer", "description": "Maximum wait in ms (default 90000). On timeout the call still returns ok:true with timed_out:true and the current state; call get_answer with timeout_ms to keep waiting"});
    let turn = "Returns {ok, tab, state, seq, answer, timed_out, question?, options?}. state is one of: starting (the agent is not up yet), blocked (a startup prompt needs choose), idle (ready, nothing asked yet), working, waiting (the agent asks something: read question and options, answer with choose), done (answer holds the reply), null (no agent in the tab). answer is null while the state is starting, blocked, working or waiting. seq counts the agent's replies. options[] items are {n, label, selected (the cursor is on it), numbered}";
    json!([
        {"name": "list_tabs", "description": "List tabs. Each tab is {id, name, title, owner, cwd, agent, state, active, last_used (unix seconds), auto_yes, yolo, unread (done but not read yet), self (true for the tab you are running in; never send to, choose in or close that tab)}",
         "inputSchema": {"type": "object", "properties": {}}},
        {"name": "open_tab", "description": format!("Open a tab in cwd, start an agent and wait until it is ready (the visible tab does not change). Without agent or run, starts claude. For a plain shell pass run as \"\" or a command. If a startup prompt (folder trust etc.) blocks the agent, returns state: blocked with options; answer with choose. On timeout the tab still exists (do not open another; keep waiting or choose). {turn}"),
         "inputSchema": {"type": "object", "required": ["cwd"], "properties": {
            "cwd": {"type": "string", "description": "Absolute path of the folder to start in"},
            "name": {"type": "string", "description": "Tab name. Unique, 1-32 characters, no whitespace, not digits only"},
            "owner": owner.clone(),
            "agent": {"type": "string", "enum": ["claude", "codex", "copilot"], "description": "Agent to start (do not pass together with run)"},
            "args": {"type": "string", "description": "Arguments for the agent (passed to the shell as is)"},
            "run": {"type": "string", "description": "Shell command to run instead of an agent. \"\" leaves a plain shell"},
            "yolo": {"type": "boolean", "description": "DANGEROUS: start with no permission prompts and no sandbox (claude --dangerously-skip-permissions / codex --dangerously-bypass-approvals-and-sandbox / copilot --yolo). Stays on when the tab is resumed after a restart. Default false"},
            "auto_yes": {"type": "boolean", "description": "Automatically pick the first Yes on Yes/No permission menus, including confirmations for deleting files or pushing. Persists across restarts. Default false"},
            "wait": {"type": "boolean", "description": "Wait until ready (default true)"},
            "timeout_ms": {"type": "integer", "description": "Maximum wait in ms. Default 50000"}}}},
        {"name": "close_tab", "description": "Close a tab; the agent inside ends too. Pass owner instead of tab to close every tab of that owner (the tab you are running in is skipped). Closing your own tab is refused unless force: true",
         "inputSchema": {"type": "object", "properties": {"tab": tab.clone(), "owner": owner.clone(), "force": {"type": "boolean", "description": "Also close the tab you are running in"}}}},
        {"name": "update_tab", "description": "Change a tab's name, owner or auto_yes. Only the given fields change; \"\" clears name or owner",
         "inputSchema": {"type": "object", "required": ["tab"], "properties": {
            "tab": tab.clone(), "name": {"type": "string"}, "owner": owner.clone(), "auto_yes": {"type": "boolean"}}}},
        {"name": "send", "description": format!("Send an instruction to the agent (paste, then Enter). By default waits for the reply or a choice. A working tab is refused with busy, and a tab whose agent is asking (state waiting) with tab_waiting plus the question and options: answer those with choose. The result has expect_seq: the seq the reply to this request will have (compare it with seq when you wait later with get_answer). Sending to the tab you are running in is refused. {turn}"),
         "inputSchema": {"type": "object", "required": ["tab", "text"], "properties": {
            "tab": tab.clone(), "text": {"type": "string"},
            "wait": {"type": "boolean", "description": "Wait for the reply or a choice (default true)"},
            "request_id": {"type": "string", "description": "Id of this request. Resending the same id does not type again; the first result is returned"},
            "force": {"type": "boolean", "description": "Send even while the agent is working, while it is asking (the text is typed into the prompt), or to the tab you are running in"}, "timeout_ms": wait_ms.clone()}}},
        {"name": "press_keys", "description": "Send raw keys (e.g. \"\\u001b\" for Esc, \"\\r\" for Enter). Use it to interrupt, or to answer a prompt with free text (type the text, then send \"\\r\" in a second call). Does not change the working state",
         "inputSchema": {"type": "object", "required": ["tab", "keys"], "properties": {"tab": tab.clone(), "keys": {"type": "string"}}}},
        {"name": "choose", "description": format!("Pick one of the options on screen (options of a waiting / blocked tab); by default waits until the agent stops again. Options that come from the agent UI rather than the question (such as \"Type something\") can be picked the same way; for free text use press_keys. {turn}"),
         "inputSchema": {"type": "object", "required": ["tab", "n"], "properties": {
            "tab": tab.clone(), "n": {"type": "integer", "description": "The n of the option to pick (1-based, as in options[].n)"}, "wait": {"type": "boolean", "description": "Default true"}, "timeout_ms": wait_ms.clone()}}},
        {"name": "get_answer", "description": format!("Return the current reply (answer is null while working). With timeout_ms, waits until the agent stops (done / waiting); keep it at 90000 or less and call again. {turn}"),
         "inputSchema": {"type": "object", "required": ["tab"], "properties": {"tab": tab.clone(), "timeout_ms": {"type": "integer", "description": "If given, wait up to this many ms. If omitted, return at once"}}}},
        {"name": "get_screen", "description": "The lines currently on the tab's screen (lines)",
         "inputSchema": {"type": "object", "required": ["tab"], "properties": {"tab": tab}}},
        {"name": "list_webhooks", "description": "List webhooks (header names only). With owner, that owner's list; without, the global list that receives every tab's events",
         "inputSchema": {"type": "object", "properties": {"owner": owner.clone()}}},
        {"name": "add_webhook", "description": "Add a webhook. Replies (state: done), choices and auto-yes answers are POSTed to it, including the reply text. The same URL is replaced together with its headers. Keep one URL per owner (a new URL per request piles up). Failed deliveries are not retried",
         "inputSchema": {"type": "object", "required": ["url"], "properties": {
            "owner": owner.clone(), "url": {"type": "string"},
            "headers": {"type": "object", "additionalProperties": {"type": "string"}, "description": "e.g. {\"Authorization\": \"Bearer ...\"}"}}}},
        {"name": "remove_webhook", "description": "Remove a webhook",
         "inputSchema": {"type": "object", "required": ["url"], "properties": {"owner": owner.clone(), "url": {"type": "string"}}}},
        {"name": "test_webhooks", "description": "POST a test event ({event: \"test\", owner, message}) to the webhooks now and return {url (tail masked), http, error} for each. ok:false if any failed",
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
    // 呼ぶ側の接続が先に切れないよう、MCP からの待ちは短めにして呼び直してもらう
    if matches!(name, "open_tab" | "send" | "choose") && args["timeout_ms"].is_null() {
        args["timeout_ms"] = json!(if name == "open_tab" { 50_000 } else { 90_000 });
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
            "instructions": "Runs claude / codex / copilot in MornDeck tabs. Typical flow: open_tab (answer a blocked startup prompt with choose), send an instruction, and read the result: state done means answer is the reply; waiting means pick from options with choose; timed_out with state working means call get_answer with timeout_ms to keep waiting. Close the tabs you opened with close_tab. list_tabs marks the tab you run in with self: true; never send to, choose in or close it. Give your tabs an owner so you can find and close them. Webhooks (add_webhook) are only needed when nothing can wait on get_answer."
        }),
        "ping" => json!({}),
        "tools/list" => json!({"tools": tools()}),
        "tools/call" => {
            let name = msg["params"]["name"].as_str().unwrap_or("");
            let resp = match to_request(name, msg["params"]["arguments"].clone()) {
                Some(req) => crate::request(sock, &crate::with_caller(req)).unwrap_or_else(
                    |e| json!({"ok": false, "error": {"code": "failed", "message": e.to_string()}}),
                ),
                None => {
                    json!({"ok": false, "error": {"code": "unknown_cmd", "message": format!("unknown tool: {name}")}})
                }
            };
            let is_error = resp["ok"] != true;
            json!({"content": [{"type": "text", "text": resp.to_string()}], "isError": is_error})
        }
        m => {
            return Some(
                json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": format!("method not found: {m}")}}),
            );
        }
    };
    Some(json!({"jsonrpc": "2.0", "id": id, "result": result}))
}

/// stdout は JSON-RPC 専用で stderr も ssh 越しでは見えないので、経路の調査用にファイルへ残す。
fn log(sock: &str, msg: &str) {
    crate::server::append_log(
        &std::path::Path::new(sock).with_file_name("mcp.log"),
        &format!("pid={} {msg}", std::process::id()),
    );
}

pub fn run(sock: &str) -> std::io::Result<()> {
    let s = sock.to_string();
    std::panic::set_hook(Box::new(move |p| log(&s, &format!("panic: {p}"))));
    log(
        sock,
        &format!(
            "start ssh={}",
            std::env::var("SSH_CONNECTION").unwrap_or_default()
        ),
    );
    let mut out = std::io::stdout();
    let end = (|| -> String {
        for line in std::io::stdin().lock().lines() {
            let Ok(line) = line else {
                return "stdin error".into();
            };
            if line.trim().is_empty() {
                continue;
            }
            let start = std::time::Instant::now();
            let (resp, what) = match serde_json::from_str::<Value>(&line) {
                Ok(msg) => {
                    let what = format!(
                        "{} {}",
                        msg["method"].as_str().unwrap_or("?"),
                        msg["params"]["name"].as_str().unwrap_or("")
                    );
                    (handle(sock, &msg), what)
                }
                Err(e) => (
                    Some(
                        json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32700, "message": e.to_string()}}),
                    ),
                    "parse error".into(),
                ),
            };
            let ok = resp
                .as_ref()
                .map(|r| r["result"]["isError"] != true && r["error"].is_null());
            log(
                sock,
                &format!("{what} ok={ok:?} {}ms", start.elapsed().as_millis()),
            );
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
        let r = super::to_request("choose", json!({"tab": "work", "n": 2})).unwrap();
        assert_eq!(
            r,
            json!({"tab": "work", "n": 2, "cmd": "choose", "timeout_ms": 90000})
        );
        // n が無くてもサーバーまで渡す (サーバーが「n が必要」と返す)
        assert!(super::to_request("choose", json!({"tab": 1})).is_some());
        assert!(super::to_request("nope", json!({})).is_none());
        let r =
            super::to_request("open_tab", json!("{\"name\":\"work\",\"cwd\":\"/tmp\"}")).unwrap();
        assert_eq!(
            (r["cmd"].as_str(), r["timeout_ms"].as_u64()),
            (Some("open_tab"), Some(50000))
        );
        assert!(
            super::to_request("get_screen", json!({"tab": 1})).unwrap()["timeout_ms"].is_null()
        );
        let names: Vec<String> = super::tools()
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(names.len(), 13);
        for n in &names {
            assert_eq!(super::to_request(n, json!({})).unwrap()["cmd"], n.as_str());
        }
    }
}
