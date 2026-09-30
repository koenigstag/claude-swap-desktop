//! Live Claude Code sessions on the default login.
//!
//! Claude Code writes `~/.claude/sessions/<pid>.json` per running session;
//! a file whose process is gone is stale and ignored.

use serde::Serialize;
use serde_json::Value;

#[derive(Serialize, Clone, Debug)]
pub struct Session {
    pub pid: u32,
    pub cwd: String,
    pub entrypoint: String,
}

pub fn on_default_login() -> Vec<Session> {
    let dir = crate::cli::home().join(".claude").join("sessions");
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .filter_map(|e| std::fs::read_to_string(e.path()).ok())
        .filter_map(|s| serde_json::from_str::<Value>(&s).ok())
        .filter_map(|v| {
            let pid = u32::try_from(v.get("pid")?.as_u64()?).ok()?;
            let str_of = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or("").to_string();
            Some(Session {
                pid,
                cwd: str_of("cwd"),
                entrypoint: str_of("entrypoint"),
            })
        })
        .filter(|s| is_alive(s.pid))
        .collect()
}

#[cfg(windows)]
fn is_alive(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, STILL_ACTIVE};
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    if pid <= 4 {
        return false;
    }
    // SAFETY: plain Win32 calls on a handle we open and close here.
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if h.is_null() {
            return false;
        }
        let mut code = 0u32;
        let ok = GetExitCodeProcess(h, &mut code) != 0;
        CloseHandle(h);
        ok && code == STILL_ACTIVE as u32
    }
}

#[cfg(not(windows))]
fn is_alive(_pid: u32) -> bool {
    false
}
