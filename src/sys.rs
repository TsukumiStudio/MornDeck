//! OS ごとに違うところ (ソケット・権限・プロセス)。Windows ではネイティブで動く

use std::path::Path;
use std::process::Command;

#[cfg(unix)]
pub use std::os::unix::net::{UnixListener, UnixStream};
// Windows 10 1803 以降の AF_UNIX。std には無い
#[cfg(windows)]
pub use uds_windows::{UnixListener, UnixStream};

/// Windows 10 1803 以降は System32 に curl.exe がある
#[cfg(unix)]
pub const CURL: &str = "/usr/bin/curl";
#[cfg(windows)]
pub const CURL: &str = "curl.exe";

#[cfg(unix)]
pub const DEV_NULL: &str = "/dev/null";
#[cfg(windows)]
pub const DEV_NULL: &str = "NUL";

#[cfg(unix)]
pub type Pid = libc::pid_t;
#[cfg(windows)]
pub type Pid = i32;

/// 本人だけが入れるフォルダにする。Windows は利用者のフォルダの権限に任せる
pub fn private_dir(dir: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
    }
    #[cfg(windows)]
    let _ = dir;
}

/// 本人だけが読める形で作る
pub fn private(o: &mut std::fs::OpenOptions) -> &mut std::fs::OpenOptions {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600)
    }
    #[cfg(windows)]
    o
}

pub fn set_private(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
    }
    #[cfg(windows)]
    {
        let _ = path;
        Ok(())
    }
}

/// 元のファイルの権限 (無ければ 0644)
pub fn perms_like(path: &Path) -> Option<std::fs::Permissions> {
    let p = std::fs::metadata(path).map(|m| m.permissions());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        Some(p.unwrap_or_else(|_| std::fs::Permissions::from_mode(0o644)))
    }
    #[cfg(windows)]
    p.ok()
}

pub fn is_socket(path: &str) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileTypeExt;
        std::fs::metadata(path).is_ok_and(|m| m.file_type().is_socket())
    }
    // Windows の AF_UNIX のソケットは再解析ポイントのファイルで、metadata が読めないことがある
    #[cfg(windows)]
    std::fs::symlink_metadata(path).is_ok()
}

pub fn is_executable(path: &Path) -> bool {
    let m = std::fs::metadata(path);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        m.is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    }
    #[cfg(windows)]
    m.is_ok_and(|m| m.is_file())
}

/// 起動した端末や ssh セッション (Windows は窓) が閉じても巻き添えで終了しないよう切り離す
pub fn detach(cmd: &mut Command) {
    #[cfg(unix)]
    unsafe {
        use std::os::unix::process::CommandExt;
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    // 見えないコンソールを持たせる (DETACHED_PROCESS だとコンソールが無く、
    // そこから起動するものがそのたびに窓を出しうる)。画面の窓を閉じても別のコンソールなので残る
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        use windows_sys::Win32::System::Threading::{CREATE_NEW_PROCESS_GROUP, CREATE_NO_WINDOW};
        cmd.creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW);
        no_inherit_std_handles();
    }
}

/// 呼んだ側の入出力 (mdeck tabs | … のパイプなど) を子に引き継がせない。引き継ぐとパイプが閉じず、呼んだ側が待ち続ける
#[cfg(windows)]
fn no_inherit_std_handles() {
    use windows_sys::Win32::Foundation::{
        HANDLE_FLAG_INHERIT, INVALID_HANDLE_VALUE, SetHandleInformation,
    };
    use windows_sys::Win32::System::Console::{
        GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
    };
    for which in [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
        // SAFETY: 引数は決まった値。無効なハンドルには触らない
        unsafe {
            let h = GetStdHandle(which);
            if !h.is_null() && h != INVALID_HANDLE_VALUE {
                SetHandleInformation(h, HANDLE_FLAG_INHERIT, 0);
            }
        }
    }
}

/// Windows のプロセス一覧 (pid, 親の pid, 実行ファイル名)
#[cfg(windows)]
pub fn processes() -> Vec<(u32, u32, String)> {
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
        TH32CS_SNAPPROCESS,
    };
    let mut out = Vec::new();
    // SAFETY: スナップショットは自分で閉じる。構造体は dwSize を入れてから渡す
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snap == INVALID_HANDLE_VALUE {
            return out;
        }
        let mut e: PROCESSENTRY32W = std::mem::zeroed();
        e.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        let mut ok = Process32FirstW(snap, &mut e) != 0;
        while ok {
            let len = e
                .szExeFile
                .iter()
                .position(|c| *c == 0)
                .unwrap_or(e.szExeFile.len());
            out.push((
                e.th32ProcessID,
                e.th32ParentProcessID,
                String::from_utf16_lossy(&e.szExeFile[..len]),
            ));
            ok = Process32NextW(snap, &mut e) != 0;
        }
        CloseHandle(snap);
    }
    out
}

/// Windows のコンソールの入力で、矢印やマウスを端末と同じ文字の並び (VT) で受け取る
#[cfg(windows)]
pub fn vt_input() {
    use windows_sys::Win32::System::Console::{
        ENABLE_VIRTUAL_TERMINAL_INPUT, ENABLE_VIRTUAL_TERMINAL_PROCESSING, GetConsoleMode,
        GetStdHandle, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, SetConsoleMode,
    };
    // SAFETY: 自分のコンソールのハンドルのモードを足すだけ
    unsafe {
        for (which, add) in [
            (STD_INPUT_HANDLE, ENABLE_VIRTUAL_TERMINAL_INPUT),
            (STD_OUTPUT_HANDLE, ENABLE_VIRTUAL_TERMINAL_PROCESSING),
        ] {
            let h = GetStdHandle(which);
            let mut mode = 0;
            if GetConsoleMode(h, &mut mode) != 0 {
                SetConsoleMode(h, mode | add);
            }
        }
    }
}

/// 資格情報マネージャーの汎用の資格情報で、名前が filter (末尾に * で前方一致) に合う最初のものの中身 (UTF-8)。
// ponytail: copilot-cli (keytar) の名前は「サービス/アカウント」と推測して前方一致で拾う。実機で名前を確かめたら絞る
#[cfg(windows)]
pub fn credential(filter: &str) -> Option<String> {
    use windows_sys::Win32::Security::Credentials::{
        CRED_TYPE_GENERIC, CREDENTIALW, CredEnumerateW, CredFree,
    };
    let filter: Vec<u16> = filter.encode_utf16().chain([0]).collect();
    let (mut n, mut list) = (0u32, std::ptr::null_mut::<*mut CREDENTIALW>());
    if unsafe { CredEnumerateW(filter.as_ptr(), 0, &mut n, &mut list) } == 0 {
        return None;
    }
    let found = (0..n as usize).find_map(|i| {
        let c = unsafe { &**list.add(i) };
        (c.Type == CRED_TYPE_GENERIC && c.CredentialBlobSize > 0).then(|| {
            let blob = unsafe {
                std::slice::from_raw_parts(c.CredentialBlob, c.CredentialBlobSize as usize)
            };
            String::from_utf8_lossy(blob).trim().to_string()
        })
    });
    unsafe { CredFree(list as *const _) };
    found.filter(|k| !k.is_empty())
}
