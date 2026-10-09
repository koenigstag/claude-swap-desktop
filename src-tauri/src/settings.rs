//! App settings persisted as JSON in the app's config folder. Read by the
//! background checker and the window handling, so they live on the Rust side
//! (not in the webview).

use crate::alerts::Kind;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::Path;

/// Choices offered for "Refresh every" (minutes).
pub const REFRESH_CHOICES: [u32; 6] = [1, 2, 5, 10, 15, 30];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Settings {
    /// Master switch for Windows notifications.
    pub notifications: bool,
    pub notify_relogin: bool,
    pub notify_limits: bool,
    pub notify_credits: bool,
    pub notify_login: bool,

    /// Keep the popover above other windows.
    pub always_on_top: bool,
    /// Hide the popover when it loses focus (the pin button overrides this for a while).
    pub hide_on_blur: bool,
    /// Disable "Make default" until unlocked (the header's lock button).
    pub lock_switching: bool,
    /// Ask before "Make default" switches the default login.
    pub confirm_switch: bool,
    /// Mention running Claude sessions before a switch or re-login.
    pub warn_running: bool,
    /// Background check interval, one of `REFRESH_CHOICES`.
    pub refresh_minutes: u32,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            notifications: true,
            notify_relogin: true,
            notify_limits: true,
            notify_credits: true,
            notify_login: true,
            always_on_top: true,
            hide_on_blur: true,
            lock_switching: true,
            confirm_switch: true,
            warn_running: true,
            refresh_minutes: 5,
        }
    }
}

impl Settings {
    pub fn allows(&self, kind: Kind) -> bool {
        self.notifications
            && match kind {
                Kind::Relogin => self.notify_relogin,
                Kind::Limit => self.notify_limits,
                Kind::Credits => self.notify_credits,
                Kind::Login => self.notify_login,
            }
    }

    /// Apply the fields of `patch` that this struct knows, when the value has
    /// the same JSON type as the current one; ignore the rest. Out-of-range
    /// values fall back to their defaults.
    pub fn merged(&self, patch: &Value) -> Settings {
        let mut current = serde_json::to_value(self).unwrap_or_default();
        if let (Some(target), Some(updates)) = (current.as_object_mut(), patch.as_object()) {
            for (k, v) in updates {
                let same_type = target.get(k).is_some_and(|old| {
                    (old.is_boolean() && v.is_boolean()) || (old.is_u64() && v.is_u64())
                });
                if same_type {
                    target.insert(k.clone(), v.clone());
                }
            }
        }
        serde_json::from_value::<Settings>(current)
            .map(Settings::sanitized)
            .unwrap_or_else(|_| self.clone())
    }

    fn sanitized(mut self) -> Settings {
        if !REFRESH_CHOICES.contains(&self.refresh_minutes) {
            self.refresh_minutes = Settings::default().refresh_minutes;
        }
        self
    }
}

pub fn load(path: &Path) -> Settings {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str::<Settings>(&s).ok())
        .map(Settings::sanitized)
        .unwrap_or_default()
}

pub fn save(path: &Path, settings: &Settings) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let tmp = path.with_extension("json.tmp");
    let text = serde_json::to_string_pretty(settings).map_err(|e| e.to_string())?;
    std::fs::write(&tmp, text).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, path).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn merge_only_known_fields_of_the_same_type() {
        let s = Settings::default().merged(&json!({ "notifyCredits": false, "bogus": false, "notifications": "no" }));
        assert!(!s.notify_credits);
        assert!(s.notifications);
        assert!(!s.allows(Kind::Credits));
        assert!(s.allows(Kind::Relogin));
        let off = s.merged(&json!({ "notifications": false }));
        assert!(!off.allows(Kind::Relogin));
        // A number where a boolean belongs, and vice versa, is ignored.
        let t = Settings::default().merged(&json!({ "alwaysOnTop": 0, "refreshMinutes": true }));
        assert!(t.always_on_top);
        assert_eq!(t.refresh_minutes, 5);
    }

    #[test]
    fn refresh_interval_is_limited_to_the_offered_choices() {
        assert_eq!(Settings::default().merged(&json!({ "refreshMinutes": 15 })).refresh_minutes, 15);
        assert_eq!(Settings::default().merged(&json!({ "refreshMinutes": 7 })).refresh_minutes, 5);
        assert_eq!(Settings::default().merged(&json!({ "refreshMinutes": 0 })).refresh_minutes, 5);
    }

    #[test]
    fn older_settings_files_get_defaults_for_new_fields() {
        let old: Settings = serde_json::from_str(r#"{ "notifications": false }"#).unwrap();
        assert!(!old.notifications);
        assert!(old.hide_on_blur && old.lock_switching && old.confirm_switch);
        assert_eq!(old.refresh_minutes, 5);
    }
}
