//! Parser for `claude-swap list --token-status`, which has no JSON form.
//!
//! The lines we care about look like:
//!
//! ```text
//!   1: someone@example.com [Org] (active)
//!      no credentials
//!      • session profile: fresh, refresh token yes, expires 22:30 in 7h 18m
//!      • stored backup: expired, refresh token yes, expires Sep 28 23:59 in 0m
//! ```

use serde::Serialize;

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct TokenLine {
    /// "active profile", "session profile" or "stored backup".
    pub source: String,
    /// "fresh", "expired", ...
    pub state: String,
    pub refresh: bool,
    pub expires: String,
}

impl TokenLine {
    pub fn healthy(&self) -> bool {
        self.state == "fresh" && self.refresh
    }
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct AccountTokens {
    pub number: u32,
    pub email: String,
    pub active: bool,
    pub no_credentials: bool,
    pub lines: Vec<TokenLine>,
}

impl AccountTokens {
    /// The credential copy claude-swap relies on for this account: the live
    /// default login when it is active, its stored backup otherwise.
    pub fn primary(&self) -> Option<&TokenLine> {
        let want = if self.active { "active profile" } else { "stored backup" };
        self.lines.iter().find(|l| l.source == want)
    }

    pub fn healthy(&self) -> bool {
        self.primary().is_some_and(TokenLine::healthy)
    }
}

pub fn parse(text: &str) -> Vec<AccountTokens> {
    let mut out = Vec::new();
    let mut cur: Option<AccountTokens> = None;
    for raw in text.lines() {
        let line = raw.trim();
        if line.starts_with("Running instances") {
            break;
        }
        if let Some(h) = parse_header(line) {
            out.extend(cur.replace(h));
            continue;
        }
        let Some(c) = cur.as_mut() else { continue };
        if line == "no credentials" {
            c.no_credentials = true;
        } else if let Some(rest) = line.strip_prefix('•') {
            c.lines.extend(parse_token_line(rest.trim()));
        }
    }
    out.extend(cur);
    out
}

fn parse_header(line: &str) -> Option<AccountTokens> {
    let (num, rest) = line.split_once(": ")?;
    let number = num.parse().ok()?;
    let email = rest.split_whitespace().next()?;
    if !email.contains('@') {
        return None;
    }
    Some(AccountTokens {
        number,
        email: email.to_string(),
        active: rest.trim_end().ends_with("(active)"),
        no_credentials: false,
        lines: Vec::new(),
    })
}

fn parse_token_line(s: &str) -> Option<TokenLine> {
    let (source, rest) = s.split_once(": ")?;
    let mut parts = rest.splitn(3, ", ");
    let state = parts.next()?.trim().to_string();
    let refresh = parts.next()?.trim().strip_prefix("refresh token ")?.trim() == "yes";
    let expires = parts
        .next()
        .map(|e| e.trim().trim_start_matches("expires ").to_string())
        .unwrap_or_default();
    Some(TokenLine {
        source: source.trim().to_string(),
        state,
        refresh,
        expires,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "Accounts:
  1: a@example.com [Org A] (active)
     no credentials
     └ last seen 25% used · 43m ago
     • active profile: expired, refresh token no, expires Jan 1 02:00 in 0m

  2: b@example.com [b@example.com's Organization]
     ├ 5h:     10%   resets 19:10         in 4h 0m
     • session profile: fresh, refresh token yes, expires 22:30 in 7h 18m
     • stored backup: fresh, refresh token yes, expires 22:02 in 6h 52m

Running instances:
  ● Desktop   ~\\somewhere  (1 session)
";

    #[test]
    fn parses_accounts_and_lines() {
        let a = parse(SAMPLE);
        assert_eq!(a.len(), 2);

        assert_eq!(a[0].number, 1);
        assert_eq!(a[0].email, "a@example.com");
        assert!(a[0].active && a[0].no_credentials);
        assert!(!a[0].healthy());

        assert!(!a[1].active);
        assert_eq!(a[1].lines.len(), 2);
        assert_eq!(a[1].primary().unwrap().source, "stored backup");
        assert_eq!(a[1].primary().unwrap().expires, "22:02 in 6h 52m");
        assert!(a[1].healthy());
    }

    /// Parses this machine's real output: `cargo test -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn parses_live_output() {
        let text = crate::cli::cswap_token_status().expect("claude-swap list --token-status");
        let a = parse(&text);
        assert!(!a.is_empty(), "no accounts parsed from:\n{text}");
        for acct in &a {
            println!("{acct:?} healthy={}", acct.healthy());
            assert!(!acct.lines.is_empty(), "no token lines for account {}", acct.number);
        }
    }
}
