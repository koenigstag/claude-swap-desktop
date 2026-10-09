//! App settings persisted as JSON in the app's config folder. Read by the
//! background notifier, so they live on the Rust side (not in the webview).

use crate::alerts::Kind;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Settings {
    /// Master switch for Windows notifications.
    pub notifications: bool,
    pub notify_relogin: bool,
    pub notify_limits: bool,
    pub notify_credits: bool,
    pub notify_login: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            notifications: true,
            notify_relogin: true,
            notify_limits: true,
            notify_credits: true,
            notify_login: true,
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

    /// Apply the boolean fields of `patch` that this struct knows; ignore the rest.
    pub fn merged(&self, patch: &Value) -> Settings {
        let mut current = serde_json::to_value(self).unwrap_or_default();
        if let (Some(target), Some(updates)) = (current.as_object_mut(), patch.as_object()) {
            for (k, v) in updates {
                if v.is_boolean() && target.contains_key(k) {
                    target.insert(k.clone(), v.clone());
                }
            }
        }
        serde_json::from_value(current).unwrap_or_else(|_| self.clone())
    }
}

pub fn load(path: &Path) -> Settings {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
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
    fn merge_only_known_booleans() {
        let s = Settings::default().merged(&json!({ "notifyCredits": false, "bogus": false, "notifications": "no" }));
        assert!(!s.notify_credits);
        assert!(s.notifications);
        assert!(!s.allows(Kind::Credits));
        assert!(s.allows(Kind::Relogin));
        let off = s.merged(&json!({ "notifications": false }));
        assert!(!off.allows(Kind::Relogin));
    }
}
