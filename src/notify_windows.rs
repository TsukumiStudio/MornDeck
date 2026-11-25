//! Windows のトースト。「MornDeck」の名前 (AUMID) は本人のレジストリに登録する (管理者の権限は要らない)

const AUMID: &str = "TsukumiStudio.MornDeck";

pub fn post(subtitle: &str, body: &str) -> Result<(), String> {
    let key = format!("HKCU\\Software\\Classes\\AppUserModelId\\{AUMID}");
    let _ = std::process::Command::new("reg")
        .args([
            "add",
            &key,
            "/v",
            "DisplayName",
            "/t",
            "REG_SZ",
            "/d",
            "MornDeck",
            "/f",
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
    tauri_winrt_notification::Toast::new(AUMID)
        .title(subtitle)
        .text1(body)
        .show()
        .map_err(|e| format!("toast: {e}"))
}

/// mdeck notify。ここでは post と同じ
pub fn post_here(subtitle: &str, body: &str) -> Result<(), String> {
    post(subtitle, body)
}
