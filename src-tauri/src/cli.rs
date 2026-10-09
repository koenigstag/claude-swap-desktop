//! Thin bridge onto the `claude-swap` and `claude` executables.
//!
//! Every call runs the program directly (never through a shell), with no
//! console window, a hard timeout, and `CLAUDE_CONFIG_DIR` / `CSWAP_ACCOUNT`
//! removed so the default login in `~/.claude` is always the one addressed.

use serde_json::Value;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

#[cfg(windows)]
use std::os::windows::process::CommandExt;

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
#[cfg(windows)]
const CREATE_NEW_CONSOLE: u32 = 0x0000_0010;

pub struct Output {
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

impl Output {
    pub fn ok(&self) -> bool {
        self.code == Some(0)
    }

    /// stdout and stderr together, trimmed, for showing to the user.
    pub fn text(&self) -> String {
        let mut s = self.stdout.trim().to_string();
        let err = self.stderr.trim();
        if !err.is_empty() {
            if !s.is_empty() {
                s.push('\n');
            }
            s.push_str(err);
        }
        s
    }
}

pub fn home() -> PathBuf {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
        .unwrap_or_default()
}

fn find_exe(names: &[&str], extra_dirs: &[PathBuf]) -> Option<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    dirs.extend_from_slice(extra_dirs);
    dirs.iter()
        .flat_map(|d| names.iter().map(move |n| d.join(n)))
        .find(|c| c.is_file())
}

pub fn cswap_exe() -> Result<PathBuf, String> {
    let h = home();
    find_exe(
        &["claude-swap.exe", "cswap.exe"],
        &[
            h.join(".local").join("bin"),
            h.join(r"AppData\Roaming\uv\tools\claude-swap\Scripts"),
        ],
    )
    .ok_or_else(|| "claude-swap not found on PATH or in the usual install folders".into())
}

pub fn claude_exe() -> Result<PathBuf, String> {
    find_exe(&["claude.exe"], &[home().join(".local").join("bin")])
        .ok_or_else(|| "claude.exe not found on PATH or in ~/.local/bin".into())
}

fn command(exe: &Path) -> Command {
    let mut c = Command::new(exe);
    c.env_remove("CLAUDE_CONFIG_DIR")
        .env_remove("CSWAP_ACCOUNT")
        .env("NO_COLOR", "1")
        .env("TERM", "dumb")
        .env("PYTHONIOENCODING", "utf-8")
        .env("PYTHONUTF8", "1");
    c
}

/// Run to completion with captured output, killing the process on timeout.
pub fn run(exe: &Path, args: &[&str], timeout: Duration) -> Result<Output, String> {
    let mut cmd = command(exe);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);

    let name = exe.file_name().unwrap_or_default().to_string_lossy().into_owned();
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("could not start {name}: {e}"))?;

    let mut so = child.stdout.take().expect("piped stdout");
    let mut se = child.stderr.take().expect("piped stderr");
    let out_reader = std::thread::spawn(move || {
        let mut b = Vec::new();
        let _ = so.read_to_end(&mut b);
        b
    });
    let err_reader = std::thread::spawn(move || {
        let mut b = Vec::new();
        let _ = se.read_to_end(&mut b);
        b
    });

    let start = Instant::now();
    let status = loop {
        if let Some(s) = child.try_wait().map_err(|e| e.to_string())? {
            break Some(s);
        }
        if start.elapsed() > timeout {
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        std::thread::sleep(Duration::from_millis(50));
    };

    let stdout = String::from_utf8_lossy(&out_reader.join().unwrap_or_default()).into_owned();
    let stderr = String::from_utf8_lossy(&err_reader.join().unwrap_or_default()).into_owned();
    match status {
        Some(s) => Ok(Output { code: s.code(), stdout, stderr }),
        None => Err(format!(
            "{name} {} timed out after {}s",
            args.join(" "),
            timeout.as_secs()
        )),
    }
}

/// Start a program in its own visible console window (Windows Terminal when it
/// is the default terminal). The caller owns the child and waits on it.
pub fn spawn_console(exe: &Path, args: &[&str]) -> Result<Child, String> {
    let mut cmd = command(exe);
    cmd.args(args);
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NEW_CONSOLE);
    cmd.spawn()
        .map_err(|e| format!("could not open a terminal for {}: {e}", exe.display()))
}

/// Parse JSON from CLI output, tolerating a stray banner line around it.
pub fn extract_json(text: &str) -> Option<Value> {
    let t = text.trim();
    if t.is_empty() {
        return None;
    }
    if let Ok(v) = serde_json::from_str(t) {
        return Some(v);
    }
    let (start, end) = (t.find('{')?, t.rfind('}')?);
    if end <= start {
        return None;
    }
    serde_json::from_str(&t[start..=end]).ok()
}

fn json_call(exe: &Path, args: &[&str], timeout: Duration) -> Result<Value, String> {
    let out = run(exe, args, timeout)?;
    let v = extract_json(&out.stdout)
        .or_else(|| extract_json(&out.stderr))
        .ok_or_else(|| {
            let t = out.text();
            if t.is_empty() {
                format!("{} returned no JSON (exit {:?})", args.join(" "), out.code)
            } else {
                t
            }
        })?;
    if let Some(err) = v.get("error") {
        let msg = err
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("claude-swap reported an error");
        return Err(msg.to_string());
    }
    Ok(v)
}

pub fn cswap_list() -> Result<Value, String> {
    json_call(&cswap_exe()?, &["list", "--json"], Duration::from_secs(60))
}

pub fn cswap_token_status() -> Result<String, String> {
    let out = run(&cswap_exe()?, &["list", "--token-status"], Duration::from_secs(60))?;
    if !out.ok() {
        return Err(out.text());
    }
    Ok(out.stdout)
}

pub fn cswap_switch(number: u32) -> Result<Value, String> {
    json_call(
        &cswap_exe()?,
        &["switch", &number.to_string(), "--json"],
        Duration::from_secs(120),
    )
}

/// `claude-swap map <n> <path>` — upserts; never prompts.
pub fn cswap_map(number: u32, path: &str) -> Result<Output, String> {
    run(&cswap_exe()?, &["map", &number.to_string(), path], Duration::from_secs(60))
}

/// `claude-swap unmap <path>` — succeeds silently even when nothing was mapped,
/// so callers must re-read the mappings to confirm.
pub fn cswap_unmap(path: &str) -> Result<Output, String> {
    run(&cswap_exe()?, &["unmap", path], Duration::from_secs(60))
}

pub fn cswap_add() -> Result<Output, String> {
    run(&cswap_exe()?, &["add"], Duration::from_secs(120))
}

/// `claude auth status --json` for the default login (`~/.claude`).
pub fn auth_status() -> Result<Value, String> {
    let out = run(&claude_exe()?, &["auth", "status", "--json"], Duration::from_secs(45))?;
    extract_json(&out.stdout).ok_or_else(|| {
        let t = out.text();
        if t.is_empty() {
            "claude auth status returned nothing".into()
        } else {
            t
        }
    })
}

pub fn open_tui() -> Result<(), String> {
    spawn_console(&cswap_exe()?, &["tui"]).map(|_| ())
}
