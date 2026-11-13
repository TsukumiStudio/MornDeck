//! macOS 以外 (WSL の中の Linux) の通知。Windows 側の mdeck.exe があれば、それに Windows のトーストを出させる
//! (WSL からは Windows の exe を実行できる)。無ければ notify-send

/// 通知を出す。Windows 側の mdeck.exe の場所は、mdeck.exe がサーバーを起動するときに MDECK_WIN_EXE で渡す
pub fn post(subtitle: &str, body: &str) -> Result<(), String> {
    if let Some(exe) = std::env::var_os("MDECK_WIN_EXE").filter(|v| !v.is_empty()) {
        let out = std::process::Command::new(&exe)
            .args(["notify", subtitle, body])
            .output()
            .map_err(|e| format!("{}: {e}", std::path::Path::new(&exe).display()))?;
        let said = String::from_utf8_lossy(&out.stdout);
        if said.lines().any(|l| l.trim() == "ok") {
            return Ok(());
        }
        return Err(said
            .lines()
            .map(str::trim)
            .rfind(|l| !l.is_empty())
            .unwrap_or("no answer from mdeck.exe")
            .to_string());
    }
    let st = std::process::Command::new("notify-send")
        .args(["--", "MornDeck", &format!("{subtitle}\n{body}")])
        .status()
        .map_err(|e| format!("notify-send: {e}"))?;
    if st.success() {
        Ok(())
    } else {
        Err(format!("notify-send exit code {}", st.code().unwrap_or(-1)))
    }
}

/// mdeck notify (macOS では MornDeck.app として呼ばれる)。ここでは post と同じ
pub fn post_here(subtitle: &str, body: &str) -> Result<(), String> {
    post(subtitle, body)
}
