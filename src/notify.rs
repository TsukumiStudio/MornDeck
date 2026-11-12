//! macOS の通知を MornDeck.app の名前で出す。osascript の通知は「スクリプトエディタ」の名前で出て、押すとスクリプトエディタが開く。
//! MornDeck.app の名前で出した通知は、押しても画面には何も出ない (起動された mdeck は端末が無いのですぐ終わる)

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::channel;
use std::time::Duration;

use block2::RcBlock;
use objc2::runtime::Bool;
use objc2_foundation::{NSBundle, NSError, NSString};
use objc2_user_notifications::{
    UNAuthorizationOptions, UNMutableNotificationContent, UNNotificationRequest,
    UNUserNotificationCenter,
};

/// MornDeck.app の中の mdeck として動いているか (手元のビルドなど、.app の外では通知を出せない)
pub fn bundled() -> bool {
    NSBundle::mainBundle().bundleIdentifier().is_some()
}

/// 通知の許可。初めてなら macOS が許可を尋ねる。許可されていなければ理由、答えが wait のうちに来なければ None
pub fn authorize(wait: Duration) -> Option<Result<(), String>> {
    if !bundled() {
        return Some(Err("not running from MornDeck.app".into()));
    }
    let (tx, rx) = channel();
    let done = RcBlock::new(move |granted: Bool, err: *mut NSError| {
        // SAFETY: 渡されるときは有効な NSError
        let why = unsafe { err.as_ref() }.map(|e| e.localizedDescription().to_string());
        let _ = tx.send(if granted.as_bool() {
            Ok(())
        } else {
            Err(why.unwrap_or_else(|| "notifications are off for MornDeck in macOS".into()))
        });
    });
    UNUserNotificationCenter::currentNotificationCenter()
        .requestAuthorizationWithOptions_completionHandler(UNAuthorizationOptions::Alert, &done);
    rx.recv_timeout(wait).ok()
}

/// mdeck が入っている MornDeck.app
fn app_bundle() -> Option<std::path::PathBuf> {
    let exe = std::env::current_exe().ok()?.canonicalize().ok()?;
    exe.ancestors()
        .nth(3)
        .filter(|a| a.extension().is_some_and(|e| e == "app"))
        .map(Into::into)
}

/// 通知を出す。macOS は、LaunchServices から起動したアプリにしか通知を許さないので、
/// MornDeck.app を open で起動し直して (mdeck notify) 出させ、結果を受け取る
pub fn post(subtitle: &str, body: &str) -> Result<(), String> {
    let app = app_bundle().ok_or("not running from MornDeck.app")?;
    static N: AtomicU64 = AtomicU64::new(0);
    let out = std::env::temp_dir().join(format!(
        "mdeck-notify-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let status = std::process::Command::new("/usr/bin/open")
        .args(["-n", "-g", "-W", "--stdout"])
        .arg(&out)
        .arg("--stderr")
        .arg(&out)
        .arg("-a")
        .arg(&app)
        .args(["--args", "notify", subtitle, body])
        .status()
        .map_err(|e| format!("open: {e}"))?;
    let said = std::fs::read_to_string(&out).unwrap_or_default();
    let _ = std::fs::remove_file(&out);
    match said.trim() {
        "ok" => Ok(()),
        "" if status.success() => Ok(()),
        "" => Err(format!("open failed ({status})")),
        e => Err(e.lines().last().unwrap_or(e).to_string()),
    }
}

/// この (LaunchServices から起動された) プロセスで通知を出す。許可が無ければ理由を返す
pub fn post_here(subtitle: &str, body: &str) -> Result<(), String> {
    if !bundled() {
        return Err("not running from MornDeck.app".into());
    }
    // 初めてなら、利用者が許可のダイアログに答えるのを待つ
    match authorize(Duration::from_secs(30)) {
        Some(Ok(())) => {}
        Some(Err(e)) => return Err(e),
        None => return Err("waiting for permission: answer the macOS dialog that asks to allow MornDeck notifications".into()),
    }
    let content = UNMutableNotificationContent::new();
    content.setTitle(&NSString::from_str("MornDeck"));
    content.setSubtitle(&NSString::from_str(subtitle));
    content.setBody(&NSString::from_str(body));
    let id = NSString::from_str(&format!("mdeck-{}", std::process::id()));
    let request = UNNotificationRequest::requestWithIdentifier_content_trigger(&id, &content, None);
    let (tx, rx) = channel();
    let done = RcBlock::new(move |err: *mut NSError| {
        // SAFETY: 失敗したときだけ NSError が渡る
        let msg = unsafe { err.as_ref() }.map(|e| e.localizedDescription().to_string());
        let _ = tx.send(msg);
    });
    UNUserNotificationCenter::currentNotificationCenter()
        .addNotificationRequest_withCompletionHandler(&request, Some(&done));
    match rx.recv_timeout(Duration::from_secs(3)) {
        Ok(Some(e)) => Err(e),
        _ => Ok(()),
    }
}
