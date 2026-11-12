//! claude / codex の週次と copilot の月次の使用量。サイドバー下部のゲージに出す。

use std::io::{Read, Seek, SeekFrom, Write};
use std::process::{Command, Stdio};

use serde_json::Value;

pub const WEEK: u64 = 7 * 24 * 3600;

/// 枠の使用量 (0〜100 %)、枠がリセットされる時刻 (unix 秒)、枠の長さ (秒。claude / codex は週、copilot は月)
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Week {
    pub used: f64,
    pub resets_at: u64,
    pub window: u64,
}

/// Claude Code のトークン (キーチェーン) で使用量 API を読む。トークンは引数に出さず curl の標準入力で渡す
pub fn claude() -> Result<Week, String> {
    let out = Command::new("/usr/bin/security").args(["find-generic-password", "-s", "Claude Code-credentials", "-w"]).output().map_err(|e| format!("security: {e}"))?;
    if !out.status.success() {
        // 36 はキーチェーンを使えないセッション (ログインした画面の外から起動したサーバーなど)
        return Err(format!("キーチェーンを読めない (security の終了コード {}): {}", out.status.code().unwrap_or(-1), String::from_utf8_lossy(&out.stderr).trim()));
    }
    let cred: Value = serde_json::from_slice(&out.stdout).map_err(|_| "キーチェーンの中身が読めない".to_string())?;
    let token = cred["claudeAiOauth"]["accessToken"].as_str().ok_or("キーチェーンにトークンが無い")?;
    let mut c = Command::new("/usr/bin/curl")
        .args(["-q", "-sS", "-m", "10", "-K", "-", "-w", "\n%{http_code}", "-H", "anthropic-beta: oauth-2025-04-20", "https://api.anthropic.com/api/oauth/usage"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("curl: {e}"))?;
    c.stdin.take().ok_or("curl")?.write_all(format!("header = \"Authorization: Bearer {token}\"\n").as_bytes()).map_err(|e| e.to_string())?;
    let out = String::from_utf8_lossy(&c.wait_with_output().map_err(|e| e.to_string())?.stdout).into_owned();
    let (body, http) = out.rsplit_once('\n').unwrap_or((&out, ""));
    let body: Value = serde_json::from_str(body).map_err(|_| format!("使用量 API が読めない応答 (HTTP {http})"))?;
    let w = &body["seven_day"];
    let week = || Some(Week { used: w["utilization"].as_f64()?, resets_at: parse_iso(w["resets_at"].as_str()?)?, window: WEEK });
    week().ok_or_else(|| format!("使用量 API に週の枠が無い (HTTP {http}): {}", body["error"]["message"].as_str().unwrap_or("")))
}

/// codex は各セッションの記録 (~/.codex/sessions/年/月/日/*.jsonl) に rate_limits を残す
pub fn codex(home: &std::path::Path) -> Option<Week> {
    // 何もしていないセッションの記録には rate_limits が無いので、新しい記録から順に、見つかるまでさかのぼる
    // ponytail: 直近 3 日のフォルダの新しい 30 件まで。それより前しか使っていなければ出ない
    let mut days: Vec<std::path::PathBuf> = Vec::new();
    for year in dirs(&home.join(".codex/sessions")).into_iter().rev() {
        for month in dirs(&year).into_iter().rev() {
            days.extend(dirs(&month).into_iter().rev());
        }
        if days.len() >= 3 {
            break;
        }
    }
    let mut files: Vec<std::path::PathBuf> = days
        .iter()
        .take(3)
        .flat_map(|d| std::fs::read_dir(d).into_iter().flatten().filter_map(|e| e.ok().map(|e| e.path())))
        .filter(|p| p.extension().is_some_and(|e| e == "jsonl"))
        .collect();
    files.sort_by_key(|p| std::cmp::Reverse(p.metadata().and_then(|m| m.modified()).ok()));
    files.iter().take(30).find_map(|f| weekly_in(f))
}

/// 名前順 (年・月・日のフォルダは日付順) に並べたサブフォルダ
fn dirs(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut v: Vec<std::path::PathBuf> = std::fs::read_dir(dir).into_iter().flatten().filter_map(|e| e.ok().map(|e| e.path())).filter(|p| p.is_dir()).collect();
    v.sort();
    v
}

/// 記録の末尾から、週の枠の rate_limits を読む
fn weekly_in(file: &std::path::Path) -> Option<Week> {
    let mut f = std::fs::File::open(file).ok()?;
    // ponytail: 末尾 512KB だけ読む。その中に rate_limits が無いほど長い出力が続いたら読めない
    let len = f.metadata().ok()?.len();
    f.seek(SeekFrom::Start(len.saturating_sub(512 * 1024))).ok()?;
    // 切り出しの先頭が文字の途中でも読めるようにする
    let mut tail = Vec::new();
    f.read_to_end(&mut tail).ok()?;
    let tail = String::from_utf8_lossy(&tail);
    tail.lines().rev().filter(|l| l.contains("\"rate_limits\"")).find_map(|l| {
        let v: Value = serde_json::from_str(l).ok()?;
        let rl = &v["payload"]["rate_limits"];
        ["primary", "secondary"].iter().map(|k| &rl[k]).find(|w| w["window_minutes"] == 10080).and_then(|w| {
            Some(Week { used: w["used_percent"].as_f64()?, resets_at: w["resets_at"].as_u64()?, window: WEEK })
        })
    })
}

/// copilot CLI のログイン (キーチェーンの copilot-cli か ~/.copilot/config.json) で月のプレミアムリクエスト枠を読む。
/// MornAIMeter と同じ GitHub の内部 API。無制限のプランは None
pub fn copilot(home: &std::path::Path) -> Option<Week> {
    // キーチェーンは macOS の許可ダイアログを出すことがあるので、トークンはサーバーの起動中 1 回だけ読む
    // ponytail: 後から copilot login してもサーバーを起動し直すまで読まない
    static TOKEN: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    let token = TOKEN.get_or_init(|| copilot_token(home)).as_deref()?;
    let mut c = Command::new("/usr/bin/curl")
        .args(["-q", "-sS", "-m", "10", "-K", "-", "-H", "User-Agent: MornDeck", "https://api.github.com/copilot_internal/user"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    c.stdin.take()?.write_all(format!("header = \"Authorization: Bearer {token}\"\n").as_bytes()).ok()?;
    copilot_week(&serde_json::from_slice(&c.wait_with_output().ok()?.stdout).ok()?)
}

fn copilot_token(home: &std::path::Path) -> Option<String> {
    let env = ["COPILOT_GITHUB_TOKEN", "GH_TOKEN", "GITHUB_TOKEN"].iter().find_map(|k| std::env::var(k).ok().filter(|v| !v.is_empty()));
    if env.is_some() {
        return env;
    }
    let out = Command::new("/usr/bin/security").args(["find-generic-password", "-s", "copilot-cli", "-w"]).output().ok()?;
    let key = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if !key.is_empty() {
        return Some(key);
    }
    // config.json は // のコメント行を含むことがある
    let raw = std::fs::read_to_string(home.join(".copilot/config.json")).ok()?;
    let raw: String = raw.lines().filter(|l| !l.trim_start().starts_with("//")).collect::<Vec<_>>().join("\n");
    let v: Value = serde_json::from_str(&raw).ok()?;
    let u = &v["lastLoggedInUser"];
    (u["host"] == "https://github.com").then_some(())?;
    v["copilotTokens"][format!("{}:{}", u["host"].as_str()?, u["login"].as_str()?)].as_str().filter(|t| !t.is_empty()).map(str::to_string)
}

fn copilot_week(v: &Value) -> Option<Week> {
    let q = &v["quota_snapshots"]["premium_interactions"];
    if q["unlimited"] == true || q["entitlement"].as_f64().is_some_and(|e| e <= 0.0) {
        return None;
    }
    let remaining = q["percent_remaining"].as_f64().or_else(|| Some(q["remaining"].as_f64()? / q["entitlement"].as_f64()? * 100.0))?;
    let reset = v["quota_reset_date_utc"].as_str().and_then(parse_iso).or_else(|| parse_iso(&format!("{}T00:00:00Z", v["quota_reset_date"].as_str()?)))?;
    Some(Week { used: 100.0 - remaining, resets_at: reset, window: reset - month_before(reset) })
}

/// 同じ日時の 1 か月前 (unix 秒)。月末は前の月の末日に丸める
fn month_before(t: u64) -> u64 {
    let (days, secs) = ((t / 86400) as i64, t % 86400);
    let (y, m, d) = civil(days);
    let (y, m) = if m == 1 { (y - 1, 12) } else { (y, m - 1) };
    let last = civil(days_from_civil(if m == 12 { y + 1 } else { y }, if m == 12 { 1 } else { m + 1 }, 1) - 1).2;
    (days_from_civil(y, m, d.min(last)) * 86400) as u64 + secs
}

/// 1970-01-01 からの日数 → 年月日 (Howard Hinnant の civil_from_days)
fn civil(z: i64) -> (i64, i64, i64) {
    let z = z + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

/// 年月日 → 1970-01-01 からの日数 (Howard Hinnant の days_from_civil)
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + d - 1;
    era * 146097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719468
}

/// リセット時刻を過ぎた記録は、新しい週の 0% とみなす
// ponytail: codex は次に使うまで記録が更新されないので推定。実際の値は次の記録で直る
pub fn fresh(mut w: Week, now: u64) -> Week {
    while w.resets_at <= now && w.window > 0 {
        w = Week { used: 0.0, resets_at: w.resets_at + w.window, window: w.window };
    }
    w
}

/// 週の何割が過ぎたかに対して何割使ったかのずれ (秒)。使いすぎが +
pub fn ahead(w: Week, now: u64) -> i64 {
    ((w.used / 100.0 - elapsed(w, now)) * w.window as f64) as i64
}

/// 枠の何割が過ぎたか
fn elapsed(w: Week, now: u64) -> f64 {
    w.window.saturating_sub(w.resets_at.saturating_sub(now)).min(w.window) as f64 / w.window.max(1) as f64
}

/// +2d12h / -1h / ±0h
pub fn fmt_ahead(secs: i64) -> String {
    let h = secs.unsigned_abs() / 3600;
    if h == 0 {
        return "±0h".into();
    }
    let sign = if secs > 0 { '+' } else { '-' };
    match (h / 24, h % 24) {
        (0, h) => format!("{sign}{h}h"),
        (d, 0) => format!("{sign}{d}d"),
        (d, h) => format!("{sign}{d}d{h}h"),
    }
}

/// 週の経過位置 (0..width) と使用量のセル数。
/// 塗りが今の位置の線を越えるのは使いすぎ (ずれが +) のときだけにする (セルへの丸めで、-1h なのに線を越えて見えていた)
pub fn gauge(w: Week, now: u64, width: usize) -> (usize, usize) {
    let at = ((elapsed(w, now) * width as f64) as usize).min(width.saturating_sub(1));
    let used = ((w.used / 100.0).clamp(0.0, 1.0) * width as f64).round() as usize;
    (at, if ahead(w, now) > 0 { used.max(at + 1).min(width) } else { used.min(at) })
}

/// 2026-11-16T18:59:59.711994+00:00 → unix 秒
fn parse_iso(s: &str) -> Option<u64> {
    let n = |r: std::ops::Range<usize>| s.get(r)?.parse::<i64>().ok();
    let (y, mo, d, h, mi, se) = (n(0..4)?, n(5..7)?, n(8..10)?, n(11..13)?, n(14..16)?, n(17..19)?);
    // 末尾の +09:00 / -05:00 / Z
    let off = match s.rfind(['+', '-']).filter(|i| *i > 18) {
        Some(i) => {
            let sign = if &s[i..i + 1] == "-" { -1 } else { 1 };
            sign * (s.get(i + 1..i + 3)?.parse::<i64>().ok()? * 3600 + s.get(i + 4..i + 6)?.parse::<i64>().ok()? * 60)
        }
        None => 0,
    };
    u64::try_from(days_from_civil(y, mo, d) * 86400 + h * 3600 + mi * 60 + se - off).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso_pace_and_format() {
        assert_eq!(parse_iso("1970-01-02T00:00:00Z"), Some(86400));
        assert_eq!(parse_iso("2026-11-16T18:59:59.711994+00:00"), Some(1794855599));
        assert_eq!(parse_iso("2026-11-17T03:59:59+09:00"), Some(1794855599));
        // 週の半分が過ぎて 50% → ずれ無し。75% なら 1.75 日分の使いすぎ
        let now = 1_000_000_000;
        let half = |used| Week { used, resets_at: now + WEEK / 2, window: WEEK };
        assert_eq!(fmt_ahead(ahead(half(50.0), now)), "±0h");
        assert_eq!(fmt_ahead(ahead(half(75.0), now)), "+1d18h");
        assert_eq!(fmt_ahead(ahead(half(25.0), now)), "-1d18h");
        assert_eq!(fmt_ahead(-3600), "-1h");
        assert_eq!(fmt_ahead(2 * 86400), "+2d");
        assert_eq!(gauge(half(25.0), now, 20), (10, 5));
        // 経過 53.5% (10.7 セル)。53% は丸めると 11 セルだが、遅れている (-0h) ので線を越えない。54% は越える
        let w = |used| Week { used, resets_at: now + WEEK * 465 / 1000, window: WEEK };
        assert_eq!((gauge(w(53.0), now, 20), ahead(w(53.0), now) < 0), ((10, 10), true));
        assert_eq!(gauge(w(54.0), now, 20), (10, 11));
        // 丸めると線に届かないが使いすぎ (+) なら線を越える
        assert_eq!(gauge(half(50.5), now, 20), (10, 11));
        assert_eq!(fresh(Week { used: 80.0, resets_at: now - 10, window: WEEK }, now), Week { used: 0.0, resets_at: now - 10 + WEEK, window: WEEK });
        // copilot は月の枠 (MornAIMeter と同じ応答の形)。12/1 リセットなら 11/1 からの 30 日
        let reset = parse_iso("2026-12-01T00:00:00Z").unwrap();
        let v = serde_json::json!({"quota_snapshots": {"premium_interactions": {"entitlement": 300, "remaining": 225, "percent_remaining": 75.0, "unlimited": false}}, "quota_reset_date_utc": "2026-12-01T00:00:00Z"});
        assert_eq!(copilot_week(&v), Some(Week { used: 25.0, resets_at: reset, window: 30 * 86400 }));
        let v = serde_json::json!({"quota_snapshots": {"premium_interactions": {"entitlement": 300, "remaining": 150}}, "quota_reset_date": "2026-03-31"});
        assert_eq!(copilot_week(&v).map(|w| (w.used, w.window)), Some((50.0, 31 * 86400)), "3/31 の 1 か月前は 2/28");
        let v = serde_json::json!({"quota_snapshots": {"premium_interactions": {"unlimited": true}}});
        assert_eq!(copilot_week(&v), None);
    }

    #[test]
    fn codex_reads_weekly_window_from_newest_session() {
        let home = std::env::temp_dir().join(format!("mdeck-usage-{}", std::process::id()));
        let day = home.join(".codex/sessions/2026/11/10");
        std::fs::create_dir_all(&day).unwrap();
        std::fs::create_dir_all(home.join(".codex/sessions/2026/10/01")).unwrap();
        let line = |p: f64| format!(r#"{{"type":"event_msg","payload":{{"type":"token_count","rate_limits":{{"primary":{{"used_percent":{p},"window_minutes":10080,"resets_at":1794385033}},"secondary":null}}}}}}"#);
        std::fs::write(day.join("a.jsonl"), format!("{}\n{}\n{{\"type\":\"x\"}}\n", line(10.0), line(31.0))).unwrap();
        assert_eq!(codex(&home), Some(Week { used: 31.0, resets_at: 1794385033, window: WEEK }));
        // 後から始めた何もしていないセッションの記録 (rate_limits 無し) があっても、前の記録から読む
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(day.join("z.jsonl"), "{\"type\":\"session_meta\"}\n").unwrap();
        assert_eq!(codex(&home), Some(Week { used: 31.0, resets_at: 1794385033, window: WEEK }), "新しい空の記録で読めなくなった");
        // 末尾 512KB の切れ目が全角文字の途中でも読める
        let mut big = format!("{}\n{}\n", "あ".repeat(200_000), line(42.0));
        if big.is_char_boundary(big.len() - 512 * 1024) {
            big.push('\n');
        }
        assert!(!big.is_char_boundary(big.len() - 512 * 1024));
        std::fs::write(day.join("zz.jsonl"), big).unwrap();
        assert_eq!(codex(&home).map(|w| w.used), Some(42.0));
        std::fs::remove_dir_all(home).unwrap();
    }
}
