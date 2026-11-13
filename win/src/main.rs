//! Windows 側の mdeck.exe。MornDeck の本体 (Linux 版の mdeck) は WSL の中で動く。
//! この exe は、WSL の中に同じ版の本体を入れ、サーバーを起動して、画面をつなぐ。
//! WSL の中のサーバーからは、完了の通知 (mdeck.exe notify) のために呼ばれる。

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};

const VERSION: &str = env!("CARGO_PKG_VERSION");
/// WSL2 が使える最初の版 (Windows 10 2004)。arm64 と wsl --install もここから
const MIN_BUILD: u32 = 19041;
/// WSL の中の本体の置き場所 (ホームから)
const LINUX_BIN: &str = ".local/bin/mdeck";
/// トーストの送り主の名前 (AppUserModelID)。初めて通知を出すときに本人のレジストリに登録する
#[cfg(windows)]
const AUMID: &str = "TsukumiStudio.MornDeck";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("--version" | "-V") => {
            println!("mdeck {VERSION} (Windows)");
            ExitCode::SUCCESS
        }
        // WSL の中のサーバーが呼ぶ。結果は 1 行 (ok かエラー)。macOS 版の mdeck notify と同じ約束
        Some("notify") => {
            let get = |i: usize| args.get(i).map(String::as_str).unwrap_or("");
            match notify(get(1), get(2)) {
                Ok(()) => println!("ok"),
                Err(e) => println!("{e}"),
            }
            ExitCode::SUCCESS
        }
        _ => match run(&args) {
            Ok(code) => code,
            Err(e) => {
                eprintln!("mdeck: {e}");
                ExitCode::FAILURE
            }
        },
    }
}

/// WSL の中の本体を必要なら入れ替えて、引数をそのまま渡して動かす
fn run(args: &[String]) -> Result<ExitCode, String> {
    let wsl = Wsl::find()?;
    wsl.check()?;
    let installed = wsl.installed_version();
    if installed.as_deref() != Some(VERSION) {
        wsl.install()?;
        // 古い版のサーバーが動いていれば、新しい版で起動し直す (タブと会話は保たれる)
        if installed.is_some() && wsl.server_running() {
            let _ = wsl.mdeck(&["restart".into()]).status();
        }
    }
    // 画面をつなぐときは、先にサーバーを Windows 側から起動しておく。
    // この wsl.exe が動いている間は WSL が止まらない (画面を閉じてもタブが動き続ける)
    if args.is_empty() && !wsl.server_running() {
        wsl.start_server()?;
    }
    let status = wsl
        .mdeck(args)
        .status()
        .map_err(|e| format!("wsl.exe: {e}"))?;
    Ok(ExitCode::from(
        status.code().unwrap_or(1).clamp(0, 255) as u8
    ))
}

struct Wsl {
    exe: PathBuf,
    /// MDECK_WSL_DISTRO があれば、そのディストリを使う (無ければ既定のもの)
    distro: Option<String>,
}

impl Wsl {
    fn find() -> Result<Self, String> {
        let root = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
        let exe = Path::new(&root).join("System32").join("wsl.exe");
        if !exe.is_file() {
            return Err(no_wsl());
        }
        Ok(Self {
            exe,
            distro: std::env::var("MDECK_WSL_DISTRO")
                .ok()
                .filter(|d| !d.is_empty()),
        })
    }

    /// Windows の版と、WSL のディストリが入っているか
    fn check(&self) -> Result<(), String> {
        if let Some(build) = windows_build()
            && build < MIN_BUILD
        {
            return Err(format!(
                "MornDeck needs Windows 10 version 2004 (build {MIN_BUILD}) or later, or Windows 11. This PC runs build {build}."
            ));
        }
        let ok = self
            .cmd()
            .args(["-e", "true"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
        if ok { Ok(()) } else { Err(no_wsl()) }
    }

    fn cmd(&self) -> Command {
        let mut c = Command::new(&self.exe);
        if let Some(d) = &self.distro {
            c.args(["-d", d]);
        }
        c
    }

    /// WSL の中で sh のスクリプトを動かす。ログインシェルにして ~/.profile の PATH (~/.local/bin など) を通す
    fn sh(&self, script: &str, args: &[String]) -> Command {
        let mut c = self.cmd();
        c.args(["--cd", "~", "-e", "sh", "-lc", script, "sh"])
            .args(args);
        // WSL の中のサーバーが通知に使う。/p で WSL のパス (/mnt/c/...) に直して渡す
        if let Ok(me) = std::env::current_exe() {
            c.env("MDECK_WIN_EXE", me);
            c.env("WSLENV", wslenv(std::env::var("WSLENV").ok().as_deref()));
        }
        c
    }

    /// WSL の中の本体を引数そのままで動かす
    fn mdeck(&self, args: &[String]) -> Command {
        self.sh(&format!("exec \"$HOME/{LINUX_BIN}\" \"$@\""), args)
    }

    fn installed_version(&self) -> Option<String> {
        let out = self
            .mdeck(&["--version".into()])
            .stderr(Stdio::null())
            .output()
            .ok()?;
        parse_version(&String::from_utf8_lossy(&out.stdout))
    }

    fn server_running(&self) -> bool {
        self.mdeck(&["api".into(), r#"{"cmd":"version"}"#.into()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    }

    /// 同梱の Linux 版を WSL の中へ写す。書き終えてから名前を替えるので、動いている本体は壊れない
    fn install(&self) -> Result<(), String> {
        let arch = self.arch()?;
        let src = bundled(&arch)?;
        let data = std::fs::read(&src).map_err(|e| format!("{}: {e}", src.display()))?;
        let script = format!(
            "mkdir -p \"$HOME/.local/bin\" && cat > \"$HOME/{LINUX_BIN}.new\" && chmod 755 \"$HOME/{LINUX_BIN}.new\" && mv -f \"$HOME/{LINUX_BIN}.new\" \"$HOME/{LINUX_BIN}\""
        );
        let mut child = self
            .sh(&script, &[])
            .stdin(Stdio::piped())
            .spawn()
            .map_err(|e| format!("wsl.exe: {e}"))?;
        {
            use std::io::Write;
            let mut stdin = child.stdin.take().ok_or("wsl.exe: no stdin")?;
            stdin
                .write_all(&data)
                .map_err(|e| format!("copy into WSL: {e}"))?;
        }
        let st = child.wait().map_err(|e| format!("wsl.exe: {e}"))?;
        if !st.success() {
            return Err(format!("could not install mdeck into WSL ({st})"));
        }
        eprintln!("mdeck: installed mdeck {VERSION} into WSL (~/{LINUX_BIN})");
        Ok(())
    }

    fn arch(&self) -> Result<String, String> {
        let out = self
            .sh("uname -m", &[])
            .output()
            .map_err(|e| format!("wsl.exe: {e}"))?;
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    }

    /// サーバーを、画面と切り離した wsl.exe で起動する。窓は出さない
    fn start_server(&self) -> Result<(), String> {
        let mut c = self.mdeck(&["server".into()]);
        c.stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const DETACHED_PROCESS: u32 = 0x0000_0008;
            const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            c.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW);
        }
        c.spawn().map_err(|e| format!("start the server: {e}"))?;
        for _ in 0..50 {
            std::thread::sleep(std::time::Duration::from_millis(100));
            if self.server_running() {
                return Ok(());
            }
        }
        // 間に合わなくても、画面側の mdeck がもう一度つなぎにいく
        Ok(())
    }
}

fn no_wsl() -> String {
    "MornDeck runs inside WSL (Windows Subsystem for Linux), which is not set up on this PC. Run `wsl --install` in an administrator PowerShell, restart, finish the Linux setup, then run mdeck again.".into()
}

/// 同梱の Linux 版 (mdeck.exe の隣の mdeck-linux-x86_64 / mdeck-linux-aarch64)
fn bundled(arch: &str) -> Result<PathBuf, String> {
    let name = linux_binary_name(arch)?;
    let dir = std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(Path::to_path_buf))
        .ok_or("cannot find the folder of mdeck.exe")?;
    let p = dir.join(&name);
    if p.is_file() {
        Ok(p)
    } else {
        Err(format!(
            "{name} is missing next to mdeck.exe ({}). Reinstall MornDeck: winget install TsukumiStudio.MornDeck",
            dir.display()
        ))
    }
}

fn linux_binary_name(arch: &str) -> Result<String, String> {
    match arch {
        "x86_64" | "aarch64" => Ok(format!("mdeck-linux-{arch}")),
        "arm64" => Ok("mdeck-linux-aarch64".into()),
        other => Err(format!(
            "WSL runs on {other}, which MornDeck does not support"
        )),
    }
}

/// 「mdeck 0.4.8」から版を取り出す
fn parse_version(out: &str) -> Option<String> {
    let rest = out.lines().next()?.trim().strip_prefix("mdeck ")?;
    Some(rest.split_whitespace().next()?.to_string())
}

/// WSLENV に MDECK_WIN_EXE/p を足す (既にあれば足さない)
fn wslenv(cur: Option<&str>) -> String {
    let cur = cur.unwrap_or("").trim_matches(':');
    if cur
        .split(':')
        .any(|v| v.split('/').next() == Some("MDECK_WIN_EXE"))
    {
        return cur.to_string();
    }
    if cur.is_empty() {
        "MDECK_WIN_EXE/p".into()
    } else {
        format!("{cur}:MDECK_WIN_EXE/p")
    }
}

/// Windows のビルド番号 (ver の「[Version 10.0.19045.1234]」から)
fn windows_build() -> Option<u32> {
    let out = Command::new("cmd").args(["/c", "ver"]).output().ok()?;
    parse_build(&String::from_utf8_lossy(&out.stdout))
}

fn parse_build(ver: &str) -> Option<u32> {
    let v = ver.split("Version ").nth(1)?;
    v.split(['.', ']']).nth(2)?.trim().parse().ok()
}

/// Windows のトーストを「MornDeck」の名前で出す。名前 (AUMID) は本人のレジストリに登録する
#[cfg(windows)]
fn notify(subtitle: &str, body: &str) -> Result<(), String> {
    register_aumid();
    tauri_winrt_notification::Toast::new(AUMID)
        .title(subtitle)
        .text1(body)
        .show()
        .map_err(|e| format!("toast: {e}"))
}

#[cfg(not(windows))]
fn notify(_: &str, _: &str) -> Result<(), String> {
    Err("notifications from mdeck.exe work only on Windows".into())
}

/// HKCU\Software\Classes\AppUserModelId\<AUMID> に表示名を書く (管理者の権限は要らない)
#[cfg(windows)]
fn register_aumid() {
    let key = format!("HKCU\\Software\\Classes\\AppUserModelId\\{AUMID}");
    let _ = Command::new("reg")
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
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_versions_and_builds() {
        assert_eq!(parse_version("mdeck 0.4.8\n").as_deref(), Some("0.4.8"));
        assert_eq!(
            parse_version("mdeck 0.5.0 (Windows)").as_deref(),
            Some("0.5.0")
        );
        assert_eq!(parse_version("sh: mdeck: not found"), None);
        assert_eq!(
            parse_build("\r\nMicrosoft Windows [Version 10.0.19045.5371]\r\n"),
            Some(19045)
        );
        assert_eq!(
            parse_build("Microsoft Windows [Version 10.0.26100]"),
            Some(26100)
        );
        assert_eq!(parse_build("garbage"), None);
    }

    #[test]
    fn picks_the_bundled_linux_binary_and_wslenv() {
        assert_eq!(linux_binary_name("x86_64").unwrap(), "mdeck-linux-x86_64");
        assert_eq!(linux_binary_name("aarch64").unwrap(), "mdeck-linux-aarch64");
        assert!(linux_binary_name("riscv64").is_err());
        assert_eq!(wslenv(None), "MDECK_WIN_EXE/p");
        assert_eq!(wslenv(Some("FOO/u")), "FOO/u:MDECK_WIN_EXE/p");
        assert_eq!(wslenv(Some("MDECK_WIN_EXE/p:FOO")), "MDECK_WIN_EXE/p:FOO");
    }

    /// WSL の中の本体と同じ版でないと、毎回入れ直してしまう
    #[test]
    fn version_matches_the_linux_mdeck() {
        let root = include_str!("../../Cargo.toml");
        let v = root
            .lines()
            .find_map(|l| l.strip_prefix("version = \""))
            .and_then(|l| l.strip_suffix('"'))
            .unwrap();
        assert_eq!(v, VERSION, "win/Cargo.toml の version を本体と同じにする");
    }
}
