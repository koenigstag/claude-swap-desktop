//! Guided re-login for one claude-swap account.
//!
//! 1. prepare  – read the account and who the default login currently is
//! 2. backup   – `claude-swap add` the current default login (only if its live
//!               token is healthy), so its newest token is not lost
//! 3. login    – `claude auth login --email <account>` in a new terminal; wait
//! 4. verify   – `claude auth status --json` must show the expected email/org,
//!               otherwise stop before anything is saved
//! 5. save     – `claude-swap add`
//! 6. restore  – switch the default login back to the previous account, so the
//!               re-logged account is not both the default login and a session
//!               profile (two holders of one single-use refresh token)
//! 7. confirm  – `claude-swap list --token-status` must show the account fresh
//!               with a refresh token
//!
//! Every step reports progress on the `relogin-progress` event.

use crate::{cli, tokens};
use serde::Serialize;
use serde_json::Value;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter};

pub const EVENT: &str = "relogin-progress";
const LOGIN_TIMEOUT: Duration = Duration::from_secs(15 * 60);

#[derive(Serialize, Clone)]
struct Progress {
    step: &'static str,
    status: &'static str,
    detail: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Outcome {
    pub ok: bool,
    pub message: String,
    pub token_lines: Vec<tokens::TokenLine>,
}

struct Reporter<'a> {
    app: &'a AppHandle,
}

impl Reporter<'_> {
    fn send(&self, step: &'static str, status: &'static str, detail: impl Into<String>) {
        let _ = self.app.emit(
            EVENT,
            Progress {
                step,
                status,
                detail: detail.into(),
            },
        );
    }

    /// Report a failed step and hand back the message for the `Err` path.
    fn fail(&self, step: &'static str, msg: impl Into<String>) -> String {
        let msg = msg.into();
        self.send(step, "failed", msg.clone());
        msg
    }
}

pub fn run(app: &AppHandle, number: u32, restore_previous: bool, cancel: &AtomicBool) -> Outcome {
    flow(&Reporter { app }, number, restore_previous, cancel).unwrap_or_else(|message| Outcome {
        ok: false,
        message,
        token_lines: Vec::new(),
    })
}

fn accounts(list: &Value) -> &[Value] {
    list["accounts"].as_array().map(Vec::as_slice).unwrap_or_default()
}

fn str_of<'v>(v: &'v Value, key: &str) -> &'v str {
    v[key].as_str().unwrap_or("")
}

/// Emails come from claude-swap's own data, but they end up as a process
/// argument, so refuse anything that doesn't look like a plain address.
fn plausible_email(e: &str) -> bool {
    let ok_char = |c: char| c.is_ascii_alphanumeric() || "@.-_+".contains(c);
    e.len() <= 254 && e.matches('@').count() == 1 && e.chars().all(ok_char)
}

fn flow(r: &Reporter, number: u32, restore_previous: bool, cancel: &AtomicBool) -> Result<Outcome, String> {
    // 1. prepare
    r.send("prepare", "running", "Reading claude-swap accounts");
    let list = cli::cswap_list().map_err(|e| r.fail("prepare", e))?;
    let target = accounts(&list)
        .iter()
        .find(|a| a["number"].as_u64() == Some(u64::from(number)))
        .ok_or_else(|| r.fail("prepare", format!("claude-swap has no account {number}")))?;
    let email = str_of(target, "email").to_string();
    if !plausible_email(&email) {
        return Err(r.fail("prepare", format!("Account {number} has an unexpected email: {email:?}")));
    }
    let org_uuid = str_of(target, "organizationUuid").to_string();
    let org_name = str_of(target, "organizationName").to_string();
    let previous = list["activeAccountNumber"]
        .as_u64()
        .and_then(|n| u32::try_from(n).ok());
    r.send(
        "prepare",
        "done",
        match previous {
            Some(p) => format!("Account {number} ({email}); default login is account {p}"),
            None => format!("Account {number} ({email}); no default login is recorded"),
        },
    );

    // 2. backup the current default login
    r.send("backup", "running", "Checking the current default login");
    backup_current(r, &list, &email)?;

    // 3. login in a terminal
    if cancel.load(Ordering::SeqCst) {
        return Err(r.fail("login", "Cancelled. Nothing was changed."));
    }
    r.send(
        "login",
        "running",
        format!("Sign in as {email} in the terminal window, then close it if it stays open"),
    );
    let claude = cli::claude_exe().map_err(|e| r.fail("login", e))?;
    let mut child = cli::spawn_console(&claude, &["auth", "login", "--email", &email])
        .map_err(|e| r.fail("login", e))?;
    let started = Instant::now();
    let status = loop {
        if cancel.load(Ordering::SeqCst) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(r.fail(
                "login",
                "Cancelled. If you already finished signing in, the default login may have changed — check it before switching.",
            ));
        }
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) => {}
            Err(e) => return Err(r.fail("login", format!("Lost track of the login window: {e}"))),
        }
        if started.elapsed() > LOGIN_TIMEOUT {
            let _ = child.kill();
            let _ = child.wait();
            return Err(r.fail("login", "Login took longer than 15 minutes and was stopped."));
        }
        std::thread::sleep(Duration::from_millis(250));
    };
    if status.success() {
        r.send("login", "done", "Login window closed");
    } else {
        r.send(
            "login",
            "warn",
            format!("claude auth login exited with code {:?}; checking the result anyway", status.code()),
        );
    }

    // 4. verify the right account signed in
    r.send("verify", "running", "Checking who is signed in");
    let auth = cli::auth_status().map_err(|e| r.fail("verify", e))?;
    if auth["loggedIn"] != Value::Bool(true) {
        return Err(r.fail("verify", "The default login is not signed in. Nothing was saved."));
    }
    let got_email = str_of(&auth, "email");
    let got_org = str_of(&auth, "orgId");
    let got_org_name = str_of(&auth, "orgName");
    if !got_email.eq_ignore_ascii_case(&email) {
        return Err(r.fail(
            "verify",
            format!(
                "Signed in as {got_email}, expected {email}. Nothing was saved. \
                 The default login is now {got_email} — switch back before using Claude."
            ),
        ));
    }
    if !org_uuid.is_empty() && !got_org.is_empty() && !got_org.eq_ignore_ascii_case(&org_uuid) {
        return Err(r.fail(
            "verify",
            format!(
                "Signed in to organization \"{got_org_name}\", expected \"{org_name}\". Nothing was saved."
            ),
        ));
    }
    r.send("verify", "done", format!("Signed in as {got_email} ({got_org_name})"));

    // 5. save
    r.send("save", "running", "Running claude-swap add");
    let out = cli::cswap_add().map_err(|e| r.fail("save", e))?;
    if !out.ok() {
        return Err(r.fail("save", format!("claude-swap add failed:\n{}", out.text())));
    }
    let text = out.text();
    if text.to_lowercase().contains("could not verify") {
        r.send("save", "warn", text);
    } else {
        r.send("save", "done", text);
    }

    // 6. restore the previous default login
    match previous {
        Some(p) if restore_previous && p != number => {
            r.send("restore", "running", format!("Switching the default login back to account {p}"));
            match cli::cswap_switch(p) {
                Ok(_) => r.send("restore", "done", format!("Default login is account {p} again")),
                Err(e) => r.send("restore", "warn", format!("Couldn't switch back to account {p}: {e}")),
            }
        }
        _ => r.send(
            "restore",
            "skipped",
            if restore_previous {
                "This account was already the default login"
            } else {
                "Switching back is turned off"
            },
        ),
    }

    // 7. confirm
    r.send("confirm", "running", "Checking token status");
    let text = cli::cswap_token_status().map_err(|e| r.fail("confirm", e))?;
    let parsed = tokens::parse(&text);
    let acct = parsed
        .iter()
        .find(|a| a.number == number)
        .ok_or_else(|| r.fail("confirm", format!("Account {number} is missing from the token status")))?;
    match acct.primary() {
        Some(p) if p.healthy() => {
            let msg = format!(
                "Account {number} is signed in again: {} is {}, refresh token yes, expires {}",
                p.source, p.state, p.expires
            );
            r.send("confirm", "done", msg.clone());
            Ok(Outcome {
                ok: true,
                message: msg,
                token_lines: acct.lines.clone(),
            })
        }
        Some(p) => Err(r.fail(
            "confirm",
            format!(
                "Account {number}'s {} is {} (refresh token {}). The re-login didn't stick.",
                p.source,
                p.state,
                if p.refresh { "yes" } else { "no" }
            ),
        )),
        None => Err(r.fail("confirm", format!("No credential status reported for account {number}"))),
    }
}

/// Save the current default login into its own claude-swap slot before the
/// login replaces it — but only when that live token is healthy, so a wiped or
/// expired credential never overwrites a good backup.
fn backup_current(r: &Reporter, list: &Value, target_email: &str) -> Result<(), String> {
    let auth = match cli::auth_status() {
        Ok(a) => a,
        Err(e) => {
            r.send("backup", "warn", format!("Couldn't read the default login ({e}); continuing"));
            return Ok(());
        }
    };
    if auth["loggedIn"] != Value::Bool(true) {
        r.send("backup", "skipped", "The default login is signed out; nothing to save");
        return Ok(());
    }
    let live = str_of(&auth, "email");
    if live.eq_ignore_ascii_case(target_email) {
        r.send("backup", "skipped", "The default login already is this account");
        return Ok(());
    }
    let Some(managed) = accounts(list)
        .iter()
        .find(|a| str_of(a, "email").eq_ignore_ascii_case(live))
    else {
        r.send(
            "backup",
            "warn",
            format!("The default login {live} isn't managed by claude-swap and will be replaced"),
        );
        return Ok(());
    };
    let n = managed["number"].as_u64().unwrap_or(0);

    let healthy = cli::cswap_token_status()
        .map(|t| {
            tokens::parse(&t)
                .iter()
                .any(|a| u64::from(a.number) == n && a.active && a.healthy())
        })
        .unwrap_or(false);
    if !healthy {
        r.send(
            "backup",
            "warn",
            format!("Account {n}'s live token isn't healthy, so its stored backup was left as it is"),
        );
        return Ok(());
    }

    let out = cli::cswap_add().map_err(|e| r.fail("backup", e))?;
    if !out.ok() {
        return Err(r.fail("backup", format!("claude-swap add failed:\n{}", out.text())));
    }
    r.send("backup", "done", format!("Saved account {n} ({live})"));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::plausible_email;

    #[test]
    fn email_guard() {
        assert!(plausible_email("someone.name+tag@example-co.com"));
        assert!(!plausible_email("a@b\" & calc"));
        assert!(!plausible_email("no-at-sign"));
        assert!(!plausible_email("two@@example.com"));
    }
}
