use super::*;

// ---------- connections ----------

pub(super) fn handle_conn(s: Shared, conn: UnixStream) -> std::io::Result<()> {
    let mut rd = BufReader::new(conn.try_clone()?);
    let mut line = String::new();
    // 1 行 (1 リクエスト) は 4MB まで。終わらない入力でメモリを使い切らない
    (&mut rd).take(4 << 20).read_line(&mut line)?;
    let mut w = conn;
    let req: Value = match serde_json::from_str(&line) {
        Ok(v) => v,
        Err(e) => return writeln!(w, "{}", json!({"ok": false, "error": format!("invalid json: {e}")})),
    };
    if req["cmd"] == "attach" {
        return attach(s, rd, w, &req);
    }
    if req["cmd"] == "subscribe" {
        let (tx, rx) = channel();
        lock(&s).subs.push(tx);
        for l in rx {
            if writeln!(w, "{l}").is_err() {
                break;
            }
        }
        return Ok(());
    }
    let resp = api(&s, &req).unwrap_or_else(|e| err_json(&e));
    writeln!(w, "{resp}")
}

pub(super) fn tab_id(v: &Value) -> Result<u32, String> {
    v["tab"].as_u64().map(|n| n as u32).ok_or_else(|| "tab (数値) が必要".into())
}

pub(super) fn validate_name<'a>(name: &str, mut taken: impl Iterator<Item = &'a str>) -> Result<(), String> {
    if name.is_empty() || name.chars().count() > 32 || name.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err("名前は空白なしの 1〜32 文字".into());
    }
    if name.chars().all(|c| c.is_ascii_digit()) {
        return Err("数字だけの名前はタブ番号と紛れるので使えない".into());
    }
    if taken.any(|t| t == name) {
        return Err(format!("名前が重複している: {name}"));
    }
    Ok(())
}

pub(super) fn timeout(req: &Value, default_ms: u64) -> Duration {
    Duration::from_millis(req["timeout_ms"].as_u64().unwrap_or(default_ms).min(1_800_000))
}

/// API の入口。呼び出しと結果を events.log に残す
pub(super) fn api(s: &Shared, req: &Value) -> Result<Value, String> {
    let sock = lock(s).sock.clone();
    log_event(&sock, "api", req);
    let r = api_inner(s, req);
    match &r {
        Ok(v) => log_event(&sock, "api_ok", &json!({"cmd": req["cmd"], "ok": v["ok"], "id": v["id"], "state": v["state"], "error": v["error"]})),
        Err(e) => log_event(&sock, "api_error", &json!({"cmd": req["cmd"], "error": e})),
    }
    r
}

/// エラーの印。API のエラーは「印: 説明」の文字列で作り、返すときに {code, message} にする
pub(super) const ERROR_CODES: [&str; 7] = ["invalid", "no_such_tab", "busy", "not_ready", "not_found", "failed", "unknown_cmd"];

/// 「印: 説明」を {ok: false, error: {code, message}} にする。印の無いものは failed
pub(super) fn err_json(e: &str) -> Value {
    let (code, message) = match e.split_once(": ") {
        Some((c, m)) if ERROR_CODES.contains(&c) => (c, m),
        _ => ("failed", e),
    };
    json!({"ok": false, "error": {"code": code, "message": message}})
}

/// 画面に出すときは印を外す
pub(super) fn err_message(e: &str) -> String {
    err_json(e)["error"]["message"].as_str().unwrap_or(e).to_string()
}

pub(super) fn invalid(e: impl std::fmt::Display) -> String {
    format!("invalid: {e}")
}

/// API で受ける owner。空文字は無し
pub(super) fn owner_arg(req: &Value) -> Result<Option<String>, String> {
    match req["owner"].as_str().filter(|o| !o.is_empty()) {
        Some(o) => validate_name(o, std::iter::empty()).map(|_| Some(o.to_string())).map_err(invalid),
        None => Ok(None),
    }
}

/// 待つ系 (open_tab / send / choose / wait / get_answer) の返し方。確認画面で止まっても時間切れでも ok:true で、state と timed_out で表す
pub(super) fn turn_json(st: &State, i: usize, timed_out: bool) -> Value {
    let t = &st.tabs[i];
    let state = status(t);
    // 作業中に前回の回答を返すと取り違えるので本文は出さない
    let answer = if matches!(state, Some("working" | "starting")) { None } else { t.answer.clone() };
    let mut v = json!({"ok": true, "tab": st.tab_json(i), "state": state, "seq": t.answer_seq, "answer": answer, "timed_out": timed_out});
    // 止まっているときと、フックでは分からないがカーソル付きのメニューが出ているとき (copilot など) に選択肢を返す。
    // 回答の文中の番号付きの行にはカーソルが付かないので、選択肢と取り違えない
    let options = screen_options(&t.term);
    if matches!(state, Some("waiting" | "blocked")) || options.iter().any(|o| o["selected"] == true) {
        v["question"] = t.question.clone();
        v["options"] = json!(options);
    }
    v
}

/// 通知先の一覧を書き換えて保存する。owner が無ければ全体の一覧
pub(super) fn store_webhooks(st: &mut State, owner: Option<&str>, hooks: Vec<Value>) -> Result<(), String> {
    let mut all = st.owner_webhooks.clone();
    match owner {
        Some(o) if hooks.is_empty() => drop(all.remove(o)),
        Some(o) => drop(all.insert(o.to_string(), json!(hooks))),
        None => {}
    }
    let (path, body) = match owner {
        Some(_) => (owner_webhooks_path(&st.sock), Value::Object(all.clone())),
        None => (webhooks_path(&st.sock), json!(hooks)),
    };
    write_atomic(&path, &body.to_string()).map_err(|e| format!("failed: 保存できない: {e}"))?;
    // ヘッダーに送信キーを含むので本人だけが読めるようにする
    let _ = std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o600));
    match owner {
        Some(_) => st.owner_webhooks = all,
        None => st.webhooks = hooks,
    }
    Ok(())
}

pub(super) fn webhook_list(st: &State, owner: Option<&str>) -> Vec<Value> {
    match owner {
        Some(o) => st.owner_webhooks.get(o).and_then(Value::as_array).cloned().unwrap_or_default(),
        None => st.webhooks.clone(),
    }
}

/// 一覧の返し方。ヘッダーは名前だけ (値は返さない)
pub(super) fn webhooks_json(st: &State, owner: Option<&str>) -> Value {
    let shown: Vec<Value> = webhook_list(st, owner)
        .iter()
        .map(|w| json!({"url": w["url"], "headers": w["headers"].as_object().map(|h| h.keys().cloned().collect::<Vec<_>>()).unwrap_or_default()}))
        .collect();
    json!({"ok": true, "owner": owner, "webhooks": shown})
}

pub(super) fn api_inner(s: &Shared, req: &Value) -> Result<Value, String> {
    let cmd = req["cmd"].as_str().unwrap_or("");
    // タブの回答や画面を読む・送るものは、そのタブを使ったことにして既読にする
    if matches!(cmd, "send" | "press_keys" | "choose" | "get_answer" | "get_screen") {
        let mut st = lock(s);
        if let Ok(i) = st.find(&req["tab"]) {
            st.tabs[i].last_used = now_secs();
            if std::mem::take(&mut st.tabs[i].unread) {
                mark(s, &mut st);
            }
        }
    }
    match cmd {
        "list_tabs" => {
            let st = lock(s);
            let tabs: Vec<Value> = (0..st.tabs.len()).map(|i| st.tab_json(i)).collect();
            Ok(json!({"ok": true, "tabs": tabs}))
        }
        // エージェントを起動するタブを開く。agent も run も無ければ claude。シェルだけは run: ""
        "open_tab" => {
            let cwd = req["cwd"].as_str().ok_or_else(|| invalid("cwd (起動するフォルダの絶対パス) が必要"))?;
            if !std::path::Path::new(cwd).is_dir() {
                return Err(invalid(format!("フォルダが無い: {cwd}")));
            }
            let run_arg = req["run"].as_str();
            let agent = match req["agent"].as_str() {
                Some(a) if AGENTS.contains(&a) => Some(a),
                Some(a) => return Err(invalid(format!("agent は {}: {a}", AGENTS.join(" / ")))),
                None if run_arg.is_none() => Some("claude"),
                None => None,
            };
            if agent.is_some() && run_arg.is_some() {
                return Err(invalid("agent と run は片方だけ渡す"));
            }
            let args = agent_args(agent, req["yolo"] == true, req["args"].as_str());
            let run = match agent {
                Some(a) => Some(format!("{a} {}", args.as_deref().unwrap_or("")).trim_end().to_string()),
                None => run_arg.filter(|r| !r.is_empty()).map(str::to_string),
            };
            let name = req["name"].as_str().filter(|n| !n.is_empty()).map(str::to_string);
            if let Some(n) = &name {
                lock(s).check_name(n, None).map_err(invalid)?;
            }
            let owner = owner_arg(req)?;
            let id = new_tab(s, Some(cwd), run.as_deref(), name, false, owner, req["auto_yes"] == true)?;
            if agent.is_some()
                && let Some(t) = lock(s).tab(id)
            {
                t.args = args;
                t.starting = true;
            }
            if agent.is_none() || !req["wait"].as_bool().unwrap_or(true) {
                let st = lock(s);
                let i = st.find(&json!(id))?;
                return Ok(json!({"ok": true, "tab": st.tab_json(i)}));
            }
            wait_ready(s, id, timeout(req, 60_000))
        }
        "close_tab" => {
            let mut st = lock(s);
            // tab の代わりに owner を渡すと、その owner のタブをまとめて閉じる (置き去りになったタブの片付け)
            if req["tab"].is_null() {
                let owner = owner_arg(req)?.ok_or_else(|| invalid("tab か owner が必要"))?;
                let mut closed = Vec::new();
                for t in st.tabs.iter_mut().filter(|t| t.owner.as_deref() == Some(owner.as_str())) {
                    let _ = t.child.kill();
                    closed.push(t.id);
                }
                return Ok(json!({"ok": true, "closed": closed}));
            }
            let i = st.find(&req["tab"])?;
            let id = st.tabs[i].id;
            st.tabs[i].child.kill().map_err(|e| format!("failed: {e}"))?;
            Ok(json!({"ok": true, "closed": [id]}))
        }
        "update_tab" => {
            let mut st = lock(s);
            let i = st.find(&req["tab"])?;
            let id = st.tabs[i].id;
            if let Some(n) = req["name"].as_str() {
                if !n.is_empty() {
                    st.check_name(n, Some(id)).map_err(invalid)?;
                }
                st.tabs[i].name = (!n.is_empty()).then(|| n.to_string());
            }
            if req["owner"].is_string() {
                st.tabs[i].owner = owner_arg(req)?;
            }
            match &req["auto_yes"] {
                Value::Bool(b) => st.tabs[i].auto_yes = *b,
                Value::Null => {}
                _ => return Err(invalid("auto_yes は true か false")),
            }
            mark(s, &mut st);
            Ok(json!({"ok": true, "tab": st.tab_json(i)}))
        }
        // 指示を送る (貼り付けて Enter)。既定は回答か選択肢が出るまで待つ
        "send" => {
            let text = req["text"].as_str().filter(|t| !t.is_empty()).ok_or_else(|| invalid("text が必要"))?;
            let wait = req["wait"].as_bool().unwrap_or(true);
            let request_id = req["request_id"].as_str().map(str::to_string);
            let (id, expect_seq) = {
                let mut st = lock(s);
                let i = st.find(&req["tab"])?;
                let t = &mut st.tabs[i];
                // 接続が切れて送り直された依頼は打ち込まず、最初の受付結果を返す
                if let Some(rid) = &request_id
                    && let Some((_, seq)) = t.requests.iter().find(|(r, _)| r == rid)
                {
                    let (tid, seq) = (t.id, *seq);
                    if !wait {
                        let i = st.find(&json!(tid))?;
                        return Ok(json!({"ok": true, "tab": st.tab_json(i), "expect_seq": seq, "duplicate": true}));
                    }
                    drop(st);
                    let mut v = wait_turn(s, tid, timeout(req, 600_000))?;
                    v["expect_seq"] = json!(seq);
                    v["duplicate"] = json!(true);
                    return Ok(v);
                }
                if t.state == Some("working") && req["force"] != true {
                    return Err("busy: エージェントが作業中。get_answer (timeout_ms 付き) で終わるのを待つか force: true".into());
                }
                // 確認画面に指示を打ち込まないよう、起動待ちのタブには送らない (選択は choose)
                if t.starting {
                    return Err(format!("not_ready: エージェントが起動していない (state: {})。get_answer の options を見て choose で答える", status(t).unwrap_or("")));
                }
                let data = if t.term.mode().contains(TermMode::BRACKETED_PASTE) { format!("\x1b[200~{text}\x1b[201~") } else { text.to_string() };
                t.writer.write_all(data.as_bytes()).map_err(|e| format!("failed: {e}"))?;
                if let Some(rid) = request_id {
                    t.requests.push((rid, t.answer_seq + 1));
                    let extra = t.requests.len().saturating_sub(100);
                    t.requests.drain(..extra);
                }
                (t.id, t.answer_seq + 1)
            };
            // 本文と同じ書き込みで送ると TUI がペーストの一部として扱い送信しない
            std::thread::sleep(Duration::from_millis(150));
            {
                let mut st = lock(s);
                let t = st.tab(id).ok_or("failed: タブが閉じた")?;
                t.writer.write_all(b"\r").map_err(|e| format!("failed: {e}"))?;
                if t.agent.is_some() {
                    set_state(s, &mut st, id, Some("working"));
                }
            }
            if !wait {
                let st = lock(s);
                let i = st.find(&json!(id))?;
                return Ok(json!({"ok": true, "tab": st.tab_json(i), "expect_seq": expect_seq}));
            }
            let mut v = wait_turn(s, id, timeout(req, 600_000))?;
            v["expect_seq"] = json!(expect_seq);
            Ok(v)
        }
        // キーをそのまま送る (Esc で中断など)。作業中の扱いは変えない
        "press_keys" => {
            let keys = req["keys"].as_str().filter(|k| !k.is_empty()).ok_or_else(|| invalid("keys が必要"))?;
            let mut st = lock(s);
            let i = st.find(&req["tab"])?;
            st.tabs[i].writer.write_all(keys.as_bytes()).map_err(|e| format!("failed: {e}"))?;
            Ok(json!({"ok": true, "tab": st.tab_json(i)}))
        }
        // 提示中の選択肢を番号で選ぶ。既定は次に止まるまで待つ
        "choose" => {
            let n = req["n"].as_u64().ok_or_else(|| invalid("n (選択肢の番号) が必要"))?;
            let id = {
                let mut st = lock(s);
                let i = st.find(&req["tab"])?;
                let t = &mut st.tabs[i];
                let options = screen_options(&t.term);
                let target = options.iter().position(|o| o["n"] == n).ok_or_else(|| format!("not_found: 選択肢 {n} が無い"))?;
                press_choice(t, &options, target).map_err(|e| format!("failed: {e}"))?;
                t.id
            };
            if !req["wait"].as_bool().unwrap_or(true) {
                let st = lock(s);
                let i = st.find(&json!(id))?;
                return Ok(json!({"ok": true, "tab": st.tab_json(i)}));
            }
            {
                let mut st = lock(s);
                if st.tab(id).is_some_and(|t| t.agent.is_some() && !t.starting) {
                    set_state(s, &mut st, id, Some("working"));
                }
            }
            std::thread::sleep(Duration::from_millis(300));
            let starting = lock(s).tab(id).is_some_and(|t| t.starting);
            if starting {
                return wait_ready(s, id, timeout(req, 60_000));
            }
            wait_turn(s, id, timeout(req, 600_000))
        }
        // 今の回答 (作業中なら answer は null)。timeout_ms を渡すと、回答か選択肢が出るまで待つ
        "get_answer" => {
            let st = lock(s);
            let i = st.find(&req["tab"])?;
            if req["timeout_ms"].is_null() {
                return Ok(turn_json(&st, i, false));
            }
            let id = st.tabs[i].id;
            drop(st);
            wait_turn(s, id, timeout(req, 0))
        }
        "get_screen" => {
            let st = lock(s);
            let i = st.find(&req["tab"])?;
            Ok(json!({"ok": true, "tab": st.tab_json(i), "lines": screen_lines(&st.tabs[i].term)}))
        }
        "list_webhooks" => {
            let owner = owner_arg(req)?;
            Ok(webhooks_json(&lock(s), owner.as_deref()))
        }
        // 通知先を足す。同じ URL は置き換える (ヘッダーの更新)
        "add_webhook" => {
            let owner = owner_arg(req)?;
            let hook = parse_webhook(&json!({"url": req["url"], "headers": if req["headers"].is_null() { json!({}) } else { req["headers"].clone() }})).map_err(invalid)?;
            let plain = plain_http(hook["url"].as_str().unwrap_or(""));
            let mut st = lock(s);
            let mut list = webhook_list(&st, owner.as_deref());
            if !list.contains(&hook) {
                list.retain(|w| w["url"] != hook["url"]);
                list.push(hook);
                store_webhooks(&mut st, owner.as_deref(), list)?;
            }
            let mut v = webhooks_json(&st, owner.as_deref());
            if plain {
                v["warning"] = json!("http:// の通知先には、回答の本文とヘッダーが暗号化されずに送られる。https:// を使う");
            }
            Ok(v)
        }
        "remove_webhook" => {
            let owner = owner_arg(req)?;
            let url = req["url"].as_str().ok_or_else(|| invalid("url が必要"))?;
            let mut st = lock(s);
            let mut list = webhook_list(&st, owner.as_deref());
            let before = list.len();
            list.retain(|w| w["url"] != url);
            if list.len() == before {
                return Err(format!("not_found: 登録されていない URL: {url}"));
            }
            store_webhooks(&mut st, owner.as_deref(), list)?;
            Ok(webhooks_json(&st, owner.as_deref()))
        }
        // 登録した通知先に今すぐテストの通知を送り、届いたか (HTTP ステータス) を返す
        "test_webhooks" => {
            let owner = owner_arg(req)?;
            let (sock, list) = {
                let st = lock(s);
                (st.sock.clone(), webhook_list(&st, owner.as_deref()))
            };
            if list.is_empty() {
                return Err(format!("not_found: 通知先が登録されていない (owner: {})", owner.as_deref().unwrap_or("なし = 全体")));
            }
            let body = json!({"event": "test", "owner": owner, "message": "MornDeck からのテスト通知"});
            let results: Vec<Value> = std::thread::scope(|sc| {
                let hs: Vec<_> = list.iter().map(|h| sc.spawn(|| post_webhook(&sock, h, &body))).collect();
                hs.into_iter().map(|h| h.join().unwrap_or(Value::Null)).collect()
            });
            let failed = results.iter().filter(|r| !(200..300).contains(&r["http"].as_u64().unwrap_or(0))).count();
            if failed > 0 {
                let mut v = err_json(&format!("failed: {failed} 件届かなかった"));
                v["results"] = json!(results);
                return Ok(v);
            }
            Ok(json!({"ok": true, "owner": owner, "results": results}))
        }
        // 内部用: サーバーを起動し直す。exe を渡すとその実行ファイルで起動する (手元のビルドで確かめるとき)
        "restart" => {
            let exe = req["exe"].as_str().map(std::path::PathBuf::from);
            if let Some(e) = &exe {
                use std::os::unix::fs::PermissionsExt;
                let runnable = std::fs::metadata(e).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0);
                if !runnable {
                    return Err(invalid(format!("実行できるファイルではない: {}", e.display())));
                }
            }
            let (s, except) = (s.clone(), req["except"].as_u64());
            // 先に呼んだ側へ返事を返す
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(200));
                restart(&s, except, exe);
            });
            Ok(json!({"ok": true}))
        }
        // 内部用: claude / codex / copilot のフックから届く
        "hook" => {
            // v2 のフックはエージェントの JSON を payload のまま送ってくる
            let p = &req["payload"];
            let req = &if p.is_object() {
                // copilot は camelCase でイベント名が無いので、フックのコマンドが event を付けて送る
                let or = |a: &str, b: &str| if p[a].is_null() { p[b].clone() } else { p[a].clone() };
                let event = if p["hook_event_name"].is_null() { req["event"].clone() } else { p["hook_event_name"].clone() };
                json!({"tab": req["tab"], "agent": req["agent"], "event": event, "message": p["last_assistant_message"],
                       "tool": or("tool_name", "toolName"), "input": or("tool_input", "toolArgs"), "session": or("session_id", "sessionId"), "reason": p["reason"]})
            } else {
                req.clone()
            };
            let id = tab_id(req)?;
            let agent = req["agent"].as_str().unwrap_or("agent").to_string();
            let ask = req["tool"] == "AskUserQuestion";
            let event = req["event"].as_str().unwrap_or("");
            let state = match event {
                "SessionStart" => Some("idle"),
                "PreToolUse" if ask => Some("waiting"),
                "UserPromptSubmit" | "PreToolUse" | "PostToolUse" => Some("working"),
                "PermissionRequest" => Some("waiting"),
                "Stop" => Some("done"),
                "SessionEnd" => None,
                _ => return Ok(json!({"ok": true})),
            };
            let mut st = lock(s);
            let t = st.tab(id).ok_or("no_such_tab: タブが無い")?;
            let sid = req["session"].as_str();
            let pgrp = t.master.process_group_leader();
            // タブの会話とは別の会話 (エージェントが中で起動した claude など) のフックは、タブの状態にも回答にも混ぜない。
            // 前面のプロセスが入れ替わった後の SessionStart だけは、起動し直した会話として受け入れる (SessionEnd 無しで落ちた後など)
            if let (Some(cur), Some(x)) = (t.session.as_deref(), sid)
                && cur != x
                && !(event == "SessionStart" && pgrp != t.session_pgrp)
            {
                return Ok(json!({"ok": true, "ignored": "別の会話"}));
            }
            // /clear は同じエージェントがすぐ次の会話を始めるので、終了扱いにせず次の SessionStart を待つ
            if event == "SessionEnd" && req["reason"] == "clear" {
                t.session = None;
                return Ok(json!({"ok": true}));
            }
            if state == Some("done") {
                // 本文の無い完了で前の回答を返さない
                t.answer = req["message"].as_str().map(str::to_string);
                t.answer_seq += 1;
            }
            t.question = if state == Some("waiting") {
                json!({"tool": req["tool"], "input": req["input"]})
            } else {
                Value::Null
            };
            if state.is_none() {
                t.session = None;
            } else if let Some(sid) = sid.filter(|x| t.session.as_deref() != Some(x)) {
                t.session = Some(sid.to_string());
                t.session_pgrp = pgrp;
            }
            t.agent = state.map(|_| agent);
            t.starting = false;
            // 自動 Yes のタブは waiting にせず、確認が描かれたら auto_yes_loop が答える
            if state == Some("waiting") && wants_yes(t) {
                t.auto_wait = Some(std::time::Instant::now());
                set_state(s, &mut st, id, Some("working"));
                return Ok(json!({"ok": true}));
            }
            set_state(s, &mut st, id, state);
            Ok(json!({"ok": true}))
        }
        c => Err(format!("unknown_cmd: 知らないコマンド: {c}")),
    }
}


/// 起動待ちのタブは、確認画面が出ていれば blocked、出ていなければ starting と見せる。
pub(super) fn status(t: &Tab) -> Option<&'static str> {
    if !t.starting {
        return t.state;
    }
    Some(if screen_options(&t.term).is_empty() { "starting" } else { "blocked" })
}

pub(super) fn set_state(s: &Shared, st: &mut State, id: u32, state: Option<&'static str>) {
    changed(s, st);
    let Some(i) = st.tabs.iter().position(|t| t.id == id) else { return };
    let changed = st.tabs[i].state != state;
    st.tabs[i].state = state;
    let mut ev = json!({"event": "state", "tab": st.tab_json(i), "state": state});
    if state == Some("done") {
        ev["answer"] = json!(st.tabs[i].answer);
        ev["seq"] = json!(st.tabs[i].answer_seq);
    }
    // done は回答ごとに seq が進むので、done が続いても (UserPromptSubmit の無いターンなど) 毎回送る
    if !changed && state != Some("done") {
        return;
    }
    st.notify(ev);
    if state == Some("done") && let Some(i) = st.tabs.iter().position(|t| t.id == id) {
        // 画面で表示中のタブは見たことにする
        let seen = i == st.active && !st.clients.is_empty();
        st.tabs[i].unread = !seen;
        if !seen {
            alert(st, i);
        }
    }
    mark(s, st);
    if state == Some("waiting") {
        let s = s.clone();
        std::thread::spawn(move || {
            // フックは選択肢の描画より先に届くので、画面が出揃うのを待ってから読む
            std::thread::sleep(Duration::from_millis(500));
            let mut st = lock(&s);
            let Some(i) = st.tabs.iter().position(|t| t.id == id && t.state == Some("waiting")) else { return };
            let t = &st.tabs[i];
            let ev = json!({"event": "choices", "tab": st.tab_json(i), "question": t.question, "options": screen_options(&t.term)});
            st.notify(ev);
        });
    }
}

/// 自由な権限のフラグは args に入れ、復元 (resume) でも付くようにする
pub(super) fn agent_args(agent: Option<&str>, yolo: bool, args: Option<&str>) -> Option<String> {
    match YOLO.iter().find(|(a, _)| Some(*a) == agent).filter(|_| yolo) {
        Some((_, flag)) => Some(format!("{flag} {}", args.unwrap_or("")).trim_end().to_string()),
        None => args.map(str::to_string),
    }
}

/// 選択肢 target を選ぶキーを送る。番号の無い選択肢は矢印で動かして Enter
pub(super) fn press_choice(t: &mut Tab, options: &[Value], target: usize) -> Result<(), String> {
    let keys = if options[target]["numbered"] == true {
        options[target]["n"].to_string()
    } else {
        let cur = options.iter().position(|o| o["selected"] == true).unwrap_or(0) as i64;
        let app = t.term.mode().contains(TermMode::APP_CURSOR);
        let (up, down) = if app { ("\x1bOA", "\x1bOB") } else { ("\x1b[A", "\x1b[B") };
        let d = target as i64 - cur;
        format!("{}\r", if d < 0 { up } else { down }.repeat(d.unsigned_abs() as usize))
    };
    t.writer.write_all(keys.as_bytes()).map_err(|e| e.to_string())
}

/// 全部が Yes / No で始まる選択肢 (実行の確認) なら、最初の Yes (今回だけ許可) の位置。質問や他の選択肢は None
pub(super) fn yes_choice(options: &[Value]) -> Option<usize> {
    let label = |o: &Value| o["label"].as_str().unwrap_or("").trim().to_lowercase();
    let yes = |o: &Value| label(o).starts_with("yes");
    let no = |o: &Value| label(o).starts_with("no");
    if !options.iter().any(no) || !options.iter().all(|o| yes(o) || no(o)) {
        return None;
    }
    options.iter().position(yes)
}

/// 自動 Yes の対象か。質問 (AskUserQuestion) と計画の承認は自動で答えない
pub(super) fn wants_yes(t: &Tab) -> bool {
    t.auto_yes && !matches!(t.question["tool"].as_str(), Some("AskUserQuestion" | "ExitPlanMode"))
}

/// 自動 Yes のタブを見張り、実行の確認 (選択肢が全部 Yes / No で始まり、カーソルの付いたメニュー) が描かれたら最初の Yes を選ぶ。
/// フックの直後は確認がまだ描かれていないことがあり、フックの来ない確認もあるので、画面を見て答える。
/// 完了や待機中のタブも見る (メインの会話が終わった後に、背景のサブエージェントが確認を出すことがある)。
/// 回答の文中の番号付きの行にはカーソル (❯) が付かないので、カーソル付きのメニューだけに答える
pub(super) fn auto_yes_loop(s: Shared) {
    let mut seen = 0;
    loop {
        // 出力が続いても 0.3 秒に 1 回だけ見る。何も変わっていなければ、変わるまで眠る
        std::thread::sleep(Duration::from_millis(300));
        let mut st = lock(&s);
        // 時間で見切るもの (auto_wait) があるときと、画面が動かないままオンにされたときのために、長くても 2 秒で見直す
        if st.changes == seen && !st.tabs.iter().any(|t| t.auto_wait.is_some()) {
            st = wait_change(&s, st, Duration::from_secs(2));
        }
        seen = st.changes;
        let now = std::time::Instant::now();
        let mut give_up = Vec::new();
        // 押したときと、カーソル付きのメニューがあるのに答えなかったときを残す (止まったときに理由を追えるように)
        let sock = st.sock.clone();
        for i in 0..st.tabs.len() {
            let t = &mut st.tabs[i];
            // フックが届かずエージェントと分からないタブでも、オンなら答える
            if !wants_yes(t) {
                continue;
            }
            let lines = screen_lines(&t.term);
            if t.auto_pressed.as_ref() == Some(&lines) {
                continue;
            }
            let options = parse_options(&lines);
            let menu = options.iter().any(|o| o["selected"] == true);
            let labels = || options.iter().map(|o| o["label"].as_str().unwrap_or("")).collect::<Vec<_>>().join(" / ");
            match yes_choice(&options).filter(|_| menu) {
                Some(k) if press_choice(t, &options, k).is_ok() => {
                    log_event(&sock, "auto_yes", &json!({"tab": t.id, "name": t.name, "pressed": options[k]["label"], "options": labels()}));
                    t.auto_pressed = Some(lines);
                    t.auto_wait = None;
                    // 何に Yes と答えたかを webhook でも知らせる
                    let ev = json!({"event": "auto_yes", "tab": st.tab_json(i), "label": options[k]["label"], "options": options});
                    st.notify(ev);
                }
                // カーソル付きのメニューだが Yes / No の確認ではない。同じ画面では 1 回だけ残す
                None if menu && t.auto_skipped.as_ref() != Some(&lines) => {
                    log_event(&sock, "auto_yes", &json!({"tab": t.id, "name": t.name, "skipped": "Yes / No の確認ではない", "options": labels()}));
                    t.auto_skipped = Some(lines);
                }
                // 確認を受けたのに Yes / No の確認が出てこない (他の選択肢など) なら、ふつうに入力待ちにする
                _ if t.auto_wait.is_some_and(|w| now - w > Duration::from_secs(3)) => {
                    t.auto_wait = None;
                    give_up.push(t.id);
                }
                _ => {}
            }
        }
        for id in give_up {
            set_state(&s, &mut st, id, Some("waiting"));
        }
    }
}

/// エージェントが作業を終える (done / waiting / 終了) まで待つ。
pub(super) fn wait_turn(s: &Shared, id: u32, limit: Duration) -> Result<Value, String> {
    let start = std::time::Instant::now();
    let mut st = lock(s);
    loop {
        {
            let i = st.tabs.iter().position(|t| t.id == id).ok_or("failed: タブが閉じた")?;
            let now = status(&st.tabs[i]);
            if now == Some("blocked") {
                return Ok(turn_json(&st, i, false));
            }
            if now != Some("working") && now != Some("starting") {
                let waiting = now == Some("waiting");
                drop(st);
                // フックは選択肢の描画より先に届くので、画面が出揃うのを待ってから読む
                if waiting {
                    std::thread::sleep(Duration::from_millis(500));
                }
                let st = lock(s);
                let i = st.tabs.iter().position(|t| t.id == id).ok_or("failed: タブが閉じた")?;
                return Ok(turn_json(&st, i, false));
            }
            if start.elapsed() > limit {
                return Ok(turn_json(&st, i, true));
            }
        }
        st = wait_change(s, st, Duration::from_secs(1).min(limit.saturating_sub(start.elapsed())));
    }
}

/// エージェントの起動 (フックの SessionStart) を待つ。確認画面で止まれば state: blocked と options を返す
pub(super) fn wait_ready(s: &Shared, id: u32, limit: Duration) -> Result<Value, String> {
    let start = std::time::Instant::now();
    let mut st = lock(s);
    loop {
        let i = st.tabs.iter().position(|t| t.id == id).ok_or("failed: 起動に失敗してタブが閉じた")?;
        let t = &st.tabs[i];
        if t.agent.is_some() {
            return Ok(turn_json(&st, i, false));
        }
        let blocked = start.elapsed() > Duration::from_secs(2) && !screen_options(&t.term).is_empty();
        if blocked {
            return Ok(turn_json(&st, i, false));
        }
        if start.elapsed() > limit {
            let mut v = turn_json(&st, i, true);
            if st.hooks.iter().any(|(_, h)| matches!(h, hooks::Status::Missing | hooks::Status::Outdated(_))) {
                v["hint"] = json!("起動を確認できない。フックが未導入か古い (F12 → Config → フック)");
            }
            return Ok(v);
        }
        // 確認画面は「2 秒たっても選択肢が出ている」で見分けるので、変化が無くても 0.3 秒ごとに見直す
        st = wait_change(s, st, Duration::from_millis(300));
    }
}
