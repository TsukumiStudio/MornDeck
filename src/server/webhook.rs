use super::*;

pub(super) fn webhooks_path(sock: &str) -> std::path::PathBuf {
    std::path::Path::new(sock).with_file_name("webhooks.json")
}

/// 全体の webhooks.json は配列のまま残し、owner ごとの分は別ファイルに置く (既存の登録と旧版を壊さない)
pub(super) fn owner_webhooks_path(sock: &str) -> std::path::PathBuf {
    std::path::Path::new(sock).with_file_name("owner_webhooks.json")
}

pub(super) fn load_owner_webhooks(sock: &str) -> serde_json::Map<String, Value> {
    let v: Value = std::fs::read_to_string(owner_webhooks_path(sock)).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
    v.as_object()
        .into_iter()
        .flatten()
        .filter_map(|(o, list)| {
            let hooks: Vec<Value> = list.as_array()?.iter().filter_map(|h| parse_webhook(h).ok()).collect();
            (!hooks.is_empty()).then(|| (o.clone(), json!(hooks)))
        })
        .collect()
}

/// `"https://..."` か `{"url": "https://...", "headers": {"X-Key": "..."}}`。
pub(super) fn parse_webhook(v: &Value) -> Result<Value, String> {
    let (url, headers) = match v {
        Value::String(u) => (u.as_str(), json!({})),
        Value::Object(o) => (o.get("url").and_then(Value::as_str).unwrap_or(""), o.get("headers").cloned().unwrap_or(json!({}))),
        _ => return Err(format!("webhook は URL か {{url, headers}}: {v}")),
    };
    if !url.starts_with("http://") && !url.starts_with("https://") {
        return Err(format!("http(s) の URL ではない: {url}"));
    }
    // 画面に出すので、空白と制御文字 (エスケープ列) は入れさせない
    if url.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err("URL に空白や制御文字は使えない".into());
    }
    let Some(h) = headers.as_object() else { return Err("headers はオブジェクト".into()) };
    for (k, val) in h {
        let ok_name = !k.is_empty() && k.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
        let ok_val = val.as_str().is_some_and(|v| !v.contains(['\r', '\n']));
        if !ok_name || !ok_val {
            return Err(format!("使えないヘッダー: {k}"));
        }
    }
    Ok(json!({"url": url, "headers": headers}))
}

pub(super) fn load_webhooks(sock: &str) -> Vec<Value> {
    let list: Vec<Value> =
        std::fs::read_to_string(webhooks_path(sock)).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
    list.iter().filter_map(|v| parse_webhook(v).ok()).collect()
}

/// 届いたかを後から追えるよう webhook.log に結果を残す。URL の末尾 (通知先ごとの ID) とヘッダーは残さない
pub(super) fn post_webhooks(sock: &str, hooks: &[Value], body: &Value) {
    for hook in hooks {
        let (sock, hook, body) = (sock.to_string(), hook.clone(), body.clone());
        std::thread::spawn(move || post_webhook(&sock, &hook, &body));
    }
}

/// 1 件 POST して終わるまで待つ。結果 {url (末尾は伏せる), http, error} を返し、webhook.log にも残す
pub(super) fn post_webhook(sock: &str, hook: &Value, body: &Value) -> Value {
    let url = hook["url"].as_str().unwrap_or("");
    let shown = url.rsplit_once('/').map_or(url.to_string(), |(base, id)| format!("{base}/{}…", id.chars().take(8).collect::<String>()));
    let mut args: Vec<String> = ["-sS", "-m", "10", "-o", "/dev/null", "-w", "%{http_code}", "-X", "POST", "-H", "Content-Type: application/json"]
        .map(String::from).to_vec();
    // ヘッダー (Authorization など) は引数に書くと ps で見えるので、本人だけが読めるファイルで渡す (curl の -H @file)
    let headers: String = hook["headers"].as_object().into_iter().flatten().map(|(k, v)| format!("{k}: {}\n", v.as_str().unwrap_or(""))).collect();
    let header_file = (!headers.is_empty()).then(|| {
        static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        std::path::Path::new(sock).with_file_name(format!("webhook-headers-{}-{n}", std::process::id()))
    });
    if let Some(path) = &header_file {
        use std::os::unix::fs::OpenOptionsExt;
        let written = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(path).and_then(|mut f| f.write_all(headers.as_bytes()));
        if written.is_ok() {
            args.extend(["-H".into(), format!("@{}", path.display())]);
        }
    }
    args.extend(["--data-binary".into(), "@-".into(), url.into()]);
    let out = std::process::Command::new("curl")
        .args(&args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut c| {
            if let Some(mut i) = c.stdin.take() {
                let _ = i.write_all(body.to_string().as_bytes());
            }
            c.wait_with_output()
        });
    if let Some(path) = &header_file {
        let _ = std::fs::remove_file(path);
    }
    let (http, error) = match out {
        Ok(o) => {
            let err = String::from_utf8_lossy(&o.stderr).trim().to_string();
            (String::from_utf8_lossy(&o.stdout).parse::<u16>().unwrap_or(0), (!o.status.success()).then(|| format!("curl {}: {err}", o.status.code().unwrap_or(-1))))
        }
        Err(e) => (0, Some(format!("curl: {e}"))),
    };
    append_log(&std::path::Path::new(sock).with_file_name("webhook.log"), &format!(
        "tab={} owner={} event={} state={} seq={} url={shown} http={http} err={}",
        body["tab"]["id"], body["tab"]["owner"], body["event"], body["state"], body["seq"], error.as_deref().unwrap_or("")));
    let r = json!({"url": shown, "http": http, "error": error});
    log_event(sock, "webhook", &json!({"tab": body["tab"]["id"], "owner": body["tab"]["owner"], "event": body["event"], "state": body["state"], "result": r}));
    r
}

/// 何が起きたかを events.log (ソケットと同じフォルダ) に残す。止まったときなどに後から追えるように。
/// API の呼び出しと結果・フック・状態の変化・自動 Yes・webhook・設定の変更・起動し直し。10MB を超えたら events.log.1 へ回す (append_log)
pub(super) fn log_event(sock: &str, what: &str, v: &Value) {
    if sock.is_empty() {
        return;
    }
    append_log(&std::path::Path::new(sock).with_file_name("events.log"), &format!("{what} {}", brief(v)));
}

/// ログ用に縮める。ヘッダー (Authorization など) の値は伏せ、長い文字列は切る
pub(super) fn brief(v: &Value) -> Value {
    match v {
        Value::String(s) if s.chars().count() > 300 => Value::String(format!("{}…({} 文字)", s.chars().take(300).collect::<String>(), s.chars().count())),
        Value::Array(a) => Value::Array(a.iter().map(brief).collect()),
        Value::Object(o) => Value::Object(
            o.iter()
                .map(|(k, x)| {
                    let x = match (k.as_str(), x) {
                        ("headers", Value::Object(h)) => Value::Object(h.keys().map(|k| (k.clone(), json!("<hidden>"))).collect()),
                        _ => brief(x),
                    };
                    (k.clone(), x)
                })
                .collect(),
        ),
        _ => v.clone(),
    }
}

pub(super) fn rotate_log(path: &std::path::Path) {
    if path.metadata().is_ok_and(|m| m.len() > 10 << 20) {
        let _ = std::fs::rename(path, path.with_extension("log.1"));
    }
}

/// 時刻を付けて 1 行足す。10MB を超えたら <名前>.log.1 へ回す
pub(crate) fn append_log(path: &std::path::Path, line: &str) {
    rotate_log(path);
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        // 1 回で書く (分けて書くと、同時に書いた行が混ざる)
        let _ = f.write_all(format!("{} {line}\n", now_secs()).as_bytes());
    }
}
