//! Conditions worth a Windows notification, derived from the state the app
//! already collects (`collect_state`). Pure functions, so they're unit-tested;
//! main.rs decides which ones are new and shows them.

use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// An account's credentials are broken and need a fresh sign-in.
    Relogin,
    /// A plan limit (5h, 7d, per-model) is used up.
    Limit,
    /// Extra-usage credits passed 80% or ran out.
    Credits,
    /// The default login (~/.claude) changed, signed out, or shares its token.
    Login,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Alert {
    /// Stable identity of the condition: notified once while it lasts.
    pub key: String,
    pub kind: Kind,
    pub title: String,
    pub body: String,
}

pub fn money(amount: f64, currency: &str) -> String {
    match currency {
        "USD" | "" => format!("${amount:.2}"),
        "EUR" => format!("€{amount:.2}"),
        "GBP" => format!("£{amount:.2}"),
        other => format!("{amount:.2} {other}"),
    }
}

/// Current usage, or the last known one when the fetch failed.
pub fn usage_of(account: &Value) -> &Value {
    if account["usage"].is_object() {
        &account["usage"]
    } else {
        &account["lastGoodUsage"]
    }
}

/// Plan windows at or over 100%: (display name, reset clock).
pub fn spent_windows(usage: &Value) -> Vec<(String, Option<String>)> {
    let full = |w: &Value| w["pct"].as_f64().is_some_and(|p| p >= 100.0);
    let clock = |w: &Value| w["clock"].as_str().map(str::to_string);
    let mut out = Vec::new();
    for (key, name) in [("fiveHour", "5h"), ("sevenDay", "7d")] {
        if full(&usage[key]) {
            out.push((name.to_string(), clock(&usage[key])));
        }
    }
    for w in usage["scoped"].as_array().into_iter().flatten() {
        if full(w) {
            out.push((w["name"].as_str().unwrap_or("model").to_string(), clock(w)));
        }
    }
    out
}

pub fn limit_spent(usage: &Value) -> bool {
    !spent_windows(usage).is_empty()
}

/// "$19.85 / $50.00", or "$19.85 spent" without a limit.
pub fn credits_amount(spend: &Value) -> Option<String> {
    let used = spend["used"].as_f64()?;
    let cur = spend["currency"].as_str().unwrap_or("USD");
    Some(match spend["limit"].as_f64().filter(|l| *l > 0.0) {
        Some(limit) => format!("{} / {}", money(used, cur), money(limit, cur)),
        None => format!("{} spent", money(used, cur)),
    })
}

fn credits_pct(spend: &Value) -> Option<f64> {
    spend["pct"].as_f64().or_else(|| {
        let limit = spend["limit"].as_f64().filter(|l| *l > 0.0)?;
        Some(spend["used"].as_f64()? / limit * 100.0)
    })
}

/// Broken credentials: any copy (live login, session profile or stored backup)
/// has lost its refresh token — Claude Code wipes a copy only after a refresh
/// was rejected — or the copy claude-swap relies on (live login when active,
/// stored backup otherwise) is missing. An expired access token that still has
/// a refresh token renews on next use and is not an alert.
pub fn needs_relogin(tokens_entry: &Value) -> Option<String> {
    let lines = tokens_entry["lines"].as_array().map(Vec::as_slice).unwrap_or_default();
    if let Some(l) = lines.iter().find(|l| l["refresh"] != Value::Bool(true)) {
        return Some(format!(
            "{} is {}, refresh token no",
            l["source"].as_str().unwrap_or("credential"),
            l["state"].as_str().unwrap_or("unknown")
        ));
    }
    let active = tokens_entry["active"] == Value::Bool(true);
    let want = if active { "active profile" } else { "stored backup" };
    let has_primary = lines.iter().any(|l| l["source"] == want);
    (!has_primary && (active || tokens_entry["noCredentials"] == Value::Bool(true)))
        .then(|| "no usable credentials".to_string())
}

pub fn current(state: &Value) -> Vec<Alert> {
    let mut out = Vec::new();
    let Some(accounts) = state["list"]["accounts"].as_array() else {
        return out;
    };
    let num = |v: &Value| v["number"].as_u64().unwrap_or(0);
    let email = |v: &Value| v["email"].as_str().unwrap_or("").to_string();

    for t in state["tokens"].as_array().into_iter().flatten() {
        if let Some(detail) = needs_relogin(t) {
            let n = num(t);
            out.push(Alert {
                key: format!("relogin:{n}"),
                kind: Kind::Relogin,
                title: format!("Account {n} needs re-login"),
                body: format!("{}: {detail}. Use Re-login in Claude Swap.", email(t)),
            });
        }
    }

    for a in accounts {
        let n = num(a);
        let usage = usage_of(a);
        let spend = &usage["spend"];
        let spent = spent_windows(usage);
        if !spent.is_empty() {
            let names = spent.iter().map(|(name, _)| name.as_str()).collect::<Vec<_>>().join(", ");
            let clock = spent.iter().find_map(|(_, clock)| clock.as_deref());
            // Short, most important first: the collapsed toast shows one body line.
            let window = match clock {
                Some(c) => format!("{names} resets {c}"),
                None => format!("{names} limit reached"),
            };
            let (title, body) = match (credits_amount(spend), credits_pct(spend)) {
                (Some(amount), Some(pct)) if pct >= 100.0 => (
                    format!("Account {n}: limit reached, credits used up"),
                    format!("{amount} · {window}"),
                ),
                (Some(amount), Some(pct)) => (
                    format!("Account {n} is on credits ({pct:.0}%)"),
                    format!("{amount} · {window}"),
                ),
                (Some(amount), None) => (format!("Account {n} is on credits"), format!("{amount} · {window}")),
                (None, _) => (
                    format!("Account {n} hit its {names} limit{}", if spent.len() > 1 { "s" } else { "" }),
                    match clock {
                        Some(c) => format!("Resets {c}. No extra usage credits are set up."),
                        None => "No extra usage credits are set up.".to_string(),
                    },
                ),
            };
            out.push(Alert { key: format!("limit:{n}"), kind: Kind::Limit, title, body });
        }
        if let (Some(amount), Some(pct)) = (credits_amount(spend), credits_pct(spend)) {
            if pct >= 100.0 {
                out.push(Alert {
                    key: format!("credits100:{n}"),
                    kind: Kind::Credits,
                    title: format!("Account {n}: credits used up"),
                    body: format!("{amount}. Paid requests stop until the period resets."),
                });
            } else if pct >= 80.0 {
                out.push(Alert {
                    key: format!("credits80:{n}"),
                    kind: Kind::Credits,
                    title: format!("Account {n}: credits at {pct:.0}%"),
                    body: format!("{amount} used."),
                });
            }
        }
    }

    let active = accounts.iter().find(|a| a["active"] == Value::Bool(true));
    let login = &state["defaultLogin"];
    if login.is_object() {
        if login["loggedIn"] == Value::Bool(false) {
            out.push(Alert {
                key: "signedout".into(),
                kind: Kind::Login,
                title: "Default login is signed out".into(),
                body: "~/.claude has no working login. Re-login the default account in Claude Swap.".into(),
            });
        } else if let (Some(a), Some(live)) = (active, login["email"].as_str()) {
            if !live.eq_ignore_ascii_case(&email(a)) {
                out.push(Alert {
                    key: format!("mismatch:{}", live.to_lowercase()),
                    kind: Kind::Login,
                    title: "Default login changed".into(),
                    body: format!(
                        "~/.claude is signed in as {live}, but claude-swap expects account {} ({}).",
                        num(a),
                        email(a)
                    ),
                });
            }
        }
    }

    if let Some(a) = active {
        let mapped: Vec<&str> = state["mappings"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|m| m["email"].as_str().is_some_and(|e| e.eq_ignore_ascii_case(&email(a))))
            .filter_map(|m| m["displayPath"].as_str().or_else(|| m["path"].as_str()))
            .collect();
        if !mapped.is_empty() {
            out.push(Alert {
                key: format!("shared:{}", num(a)),
                kind: Kind::Login,
                title: "Shared refresh token risk".into(),
                body: format!(
                    "Account {} is the default login and also mapped to {}. One copy can invalidate the other.",
                    num(a),
                    mapped.join(", ")
                ),
            });
        }
    }

    out
}

/// Of the alerts about to be shown together, drop an account's credits alert
/// when the same account's limit alert is among them: the limit alert already
/// carries the credits level ("on credits (91%)", "credits used up") and amount,
/// so one notification says it all. Both still count as announced.
pub fn merge_simultaneous(fresh: Vec<Alert>) -> Vec<Alert> {
    let account = |a: &Alert| a.key.split_once(':').map(|(_, n)| n.to_string());
    let limited: std::collections::HashSet<String> =
        fresh.iter().filter(|a| a.kind == Kind::Limit).filter_map(account).collect();
    fresh
        .into_iter()
        .filter(|a| a.kind != Kind::Credits || !account(a).is_some_and(|n| limited.contains(&n)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn keys(state: &Value) -> Vec<String> {
        current(state).into_iter().map(|a| a.key).collect()
    }

    fn base() -> Value {
        json!({
            "list": { "accounts": [
                { "number": 1, "email": "w@x.com", "active": false,
                  "usage": { "fiveHour": { "pct": 5.0 }, "sevenDay": { "pct": 27.0 },
                             "spend": { "used": 19.85, "limit": 50.0, "pct": 39.7, "currency": "USD" } } },
                { "number": 2, "email": "p@x.com", "active": true,
                  "usage": { "fiveHour": { "pct": 12.0 }, "sevenDay": { "pct": 14.0 } } }
            ]},
            "tokens": [
                { "number": 1, "email": "w@x.com", "active": false, "noCredentials": false,
                  "lines": [{ "source": "stored backup", "state": "fresh", "refresh": true, "expires": "" }] },
                { "number": 2, "email": "p@x.com", "active": true, "noCredentials": false,
                  "lines": [{ "source": "active profile", "state": "fresh", "refresh": true, "expires": "" }] }
            ],
            "defaultLogin": { "loggedIn": true, "email": "p@x.com" },
            "mappings": [{ "path": "d:\\work", "displayPath": "D:\\Work", "email": "w@x.com" }]
        })
    }

    #[test]
    fn healthy_state_has_no_alerts() {
        assert!(keys(&base()).is_empty());
    }

    #[test]
    fn expired_but_renewable_is_not_relogin() {
        let mut s = base();
        s["tokens"][1]["lines"][0]["state"] = json!("expired");
        assert!(keys(&s).is_empty());
        s["tokens"][1]["lines"][0]["refresh"] = json!(false);
        assert_eq!(keys(&s), vec!["relogin:2"]);
    }

    #[test]
    fn wiped_session_copy_is_relogin_even_with_a_renewable_backup() {
        // The 2026-09-30 incident: backup looked renewable, session copy was wiped.
        let mut s = base();
        s["tokens"][0]["lines"] = json!([
            { "source": "session profile", "state": "expired", "refresh": false, "expires": "" },
            { "source": "stored backup", "state": "expired", "refresh": true, "expires": "" }
        ]);
        let alerts = current(&s);
        assert_eq!(alerts.len(), 1);
        assert_eq!(alerts[0].key, "relogin:1");
        assert_eq!(alerts[0].body, "w@x.com: session profile is expired, refresh token no. Use Re-login in Claude Swap.");
    }

    #[test]
    fn limit_with_and_without_credits() {
        let mut s = base();
        s["list"]["accounts"][0]["usage"]["fiveHour"] = json!({ "pct": 100.0, "clock": "19:10" });
        s["list"]["accounts"][1]["usage"]["sevenDay"] = json!({ "pct": 100.0 });
        let alerts = current(&s);
        assert_eq!(alerts[0].title, "Account 1 is on credits (40%)");
        assert_eq!(alerts[0].body, "$19.85 / $50.00 · 5h resets 19:10");
        assert_eq!(alerts[1].title, "Account 2 hit its 7d limit");
        assert_eq!(alerts[1].body, "No extra usage credits are set up.");
    }

    #[test]
    fn limit_and_credits_at_once_make_one_notification() {
        // The reported case: 7d used up while credits were already at 91%.
        let mut s = base();
        s["list"]["accounts"][0]["usage"]["sevenDay"] = json!({ "pct": 100.0, "clock": "Oct 11 16:00" });
        s["list"]["accounts"][0]["usage"]["spend"] = json!({ "used": 45.43, "limit": 50.0, "pct": 90.86, "currency": "USD" });
        let all = current(&s);
        assert_eq!(all.iter().map(|a| a.key.as_str()).collect::<Vec<_>>(), vec!["limit:1", "credits80:1"]);
        let shown = merge_simultaneous(all);
        assert_eq!(shown.len(), 1);
        assert_eq!(shown[0].title, "Account 1 is on credits (91%)");
        assert_eq!(shown[0].body, "$45.43 / $50.00 · 7d resets Oct 11 16:00");

        // Credits used up together with the limit: still one, saying both.
        s["list"]["accounts"][0]["usage"]["spend"]["pct"] = json!(100.0);
        let shown = merge_simultaneous(current(&s));
        assert_eq!(shown.len(), 1);
        assert_eq!(shown[0].title, "Account 1: limit reached, credits used up");

        // A credits alert for another account, or on its own, is kept.
        let lone = Alert { key: "credits80:2".into(), kind: Kind::Credits, title: String::new(), body: String::new() };
        assert_eq!(merge_simultaneous(vec![lone.clone()]), vec![lone]);
    }

    #[test]
    fn credits_thresholds() {
        let mut s = base();
        s["list"]["accounts"][0]["usage"]["spend"]["pct"] = json!(85.0);
        assert_eq!(keys(&s), vec!["credits80:1"]);
        s["list"]["accounts"][0]["usage"]["spend"]["pct"] = json!(100.0);
        assert_eq!(keys(&s), vec!["credits100:1"]);
    }

    #[test]
    fn login_changes_and_shared_token() {
        let mut s = base();
        s["defaultLogin"] = json!({ "loggedIn": true, "email": "w@x.com" });
        assert_eq!(keys(&s), vec!["mismatch:w@x.com"]);
        s["defaultLogin"] = json!({ "loggedIn": false });
        assert_eq!(keys(&s), vec!["signedout"]);
        let mut s = base();
        s["mappings"][0]["email"] = json!("p@x.com");
        assert_eq!(keys(&s), vec!["shared:2"]);
    }
}
