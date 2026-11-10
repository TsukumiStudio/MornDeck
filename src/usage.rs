//! claude / codex の週次の使用量。サイドバー下部のゲージに出す。

use std::io::{Read, Seek, SeekFrom, Write};
use std::process::{Command, Stdio};

use serde_json::Value;

pub const WEEK: u64 = 7 * 24 * 3600;

/// 週の使用量 (0〜100 %) と、週がリセットされる時刻 (unix 秒)
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Week {
    pub used: f64,
    pub resets_at: u64,
}

/// Claude Code のトークン (キーチェーン) で使用量 API を読む。トークンは引数に出さず curl の標準入力で渡す
pub fn claude() -> Option<Week> {
    let out = Command::new("security").args(["find-generic-password", "-s", "Claude Code-credentials", "-w"]).output().ok()?;
    let cred: Value = serde_json::from_slice(&out.stdout).ok()?;
    let token = cred["claudeAiOauth"]["accessToken"].as_str()?;
    let mut c = Command::new("curl")
        .args(["-sS", "-m", "10", "-K", "-", "-H", "anthropic-beta: oauth-2025-04-20", "https://api.anthropic.com/api/oauth/usage"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    c.stdin.take()?.write_all(format!("header = \"Authorization: Bearer {token}\"\n").as_bytes()).ok()?;
    let body: Value = serde_json::from_slice(&c.wait_with_output().ok()?.stdout).ok()?;
    let w = &body["seven_day"];
    Some(Week { used: w["utilization"].as_f64()?, resets_at: parse_iso(w["resets_at"].as_str()?)? })
}

/// codex は各セッションの記録 (~/.codex/sessions/年/月/日/*.jsonl) に rate_limits を残す。一番新しい記録の末尾から読む
pub fn codex(home: &std::path::Path) -> Option<Week> {
    let mut dir = home.join(".codex/sessions");
    // 年・月・日のフォルダは名前順が日付順
    for _ in 0..3 {
        dir = newest(&dir, |p| p.is_dir())?;
    }
    let file = newest(&dir, |p| p.extension().is_some_and(|e| e == "jsonl"))?;
    let mut f = std::fs::File::open(file).ok()?;
    // ponytail: 末尾 512KB だけ読む。その中に rate_limits が無いほど長い出力が続いたら読めない
    let len = f.metadata().ok()?.len();
    f.seek(SeekFrom::Start(len.saturating_sub(512 * 1024))).ok()?;
    let mut tail = String::new();
    f.read_to_string(&mut tail).ok()?;
    tail.lines().rev().filter(|l| l.contains("\"rate_limits\"")).find_map(|l| {
        let v: Value = serde_json::from_str(l).ok()?;
        let rl = &v["payload"]["rate_limits"];
        ["primary", "secondary"].iter().map(|k| &rl[k]).find(|w| w["window_minutes"] == 10080).and_then(|w| {
            Some(Week { used: w["used_percent"].as_f64()?, resets_at: w["resets_at"].as_u64()? })
        })
    })
}

/// リセット時刻を過ぎた記録は、新しい週の 0% とみなす
// ponytail: codex は次に使うまで記録が更新されないので推定。実際の値は次の記録で直る
pub fn fresh(mut w: Week, now: u64) -> Week {
    while w.resets_at <= now {
        w = Week { used: 0.0, resets_at: w.resets_at + WEEK };
    }
    w
}

/// 週の何割が過ぎたかに対して何割使ったかのずれ (秒)。使いすぎが +
pub fn ahead(w: Week, now: u64) -> i64 {
    let elapsed = WEEK.saturating_sub(w.resets_at.saturating_sub(now)).min(WEEK) as f64 / WEEK as f64;
    ((w.used / 100.0 - elapsed) * WEEK as f64) as i64
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

/// 週の経過位置 (0..width) と使用量のセル数
pub fn gauge(w: Week, now: u64, width: usize) -> (usize, usize) {
    let elapsed = WEEK.saturating_sub(w.resets_at.saturating_sub(now)).min(WEEK) as f64 / WEEK as f64;
    let at = ((elapsed * width as f64) as usize).min(width.saturating_sub(1));
    let used = ((w.used / 100.0).clamp(0.0, 1.0) * width as f64).round() as usize;
    (at, used)
}

fn newest(dir: &std::path::Path, keep: impl Fn(&std::path::Path) -> bool) -> Option<std::path::PathBuf> {
    std::fs::read_dir(dir).ok()?.filter_map(|e| e.ok().map(|e| e.path())).filter(|p| keep(p)).max()
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
    // 年月日から 1970-01-01 からの日数 (Howard Hinnant の days_from_civil)
    let y = if mo <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (mo + if mo > 2 { -3 } else { 9 }) + 2) / 5 + d - 1;
    let days = era * 146097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719468;
    u64::try_from(days * 86400 + h * 3600 + mi * 60 + se - off).ok()
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
        let half = |used| Week { used, resets_at: now + WEEK / 2 };
        assert_eq!(fmt_ahead(ahead(half(50.0), now)), "±0h");
        assert_eq!(fmt_ahead(ahead(half(75.0), now)), "+1d18h");
        assert_eq!(fmt_ahead(ahead(half(25.0), now)), "-1d18h");
        assert_eq!(fmt_ahead(-3600), "-1h");
        assert_eq!(fmt_ahead(2 * 86400), "+2d");
        assert_eq!(gauge(half(25.0), now, 20), (10, 5));
        assert_eq!(fresh(Week { used: 80.0, resets_at: now - 10 }, now), Week { used: 0.0, resets_at: now - 10 + WEEK });
    }

    #[test]
    fn codex_reads_weekly_window_from_newest_session() {
        let home = std::env::temp_dir().join(format!("mdeck-usage-{}", std::process::id()));
        let day = home.join(".codex/sessions/2026/11/10");
        std::fs::create_dir_all(&day).unwrap();
        std::fs::create_dir_all(home.join(".codex/sessions/2026/10/01")).unwrap();
        let line = |p: f64| format!(r#"{{"type":"event_msg","payload":{{"type":"token_count","rate_limits":{{"primary":{{"used_percent":{p},"window_minutes":10080,"resets_at":1794385033}},"secondary":null}}}}}}"#);
        std::fs::write(day.join("a.jsonl"), format!("{}\n{}\n{{\"type\":\"x\"}}\n", line(10.0), line(31.0))).unwrap();
        assert_eq!(codex(&home), Some(Week { used: 31.0, resets_at: 1794385033 }));
        std::fs::remove_dir_all(home).unwrap();
    }
}
