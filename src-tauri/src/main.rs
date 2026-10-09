#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod cli;
mod relogin;
mod sessions;
mod tokens;

use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, State, WebviewWindow, WindowEvent};
use tauri_plugin_positioner::{Position, WindowExt};

const TRAY_ID: &str = "main";

#[derive(Default)]
struct AppState {
    /// A switch or re-login is running; claude-swap must not be driven twice.
    busy: AtomicBool,
    cancel: AtomicBool,
    /// Keep the popover open when it loses focus.
    pinned: AtomicBool,
    /// A native dialog owned by the popover is open; its focus must not hide us.
    dialog_open: AtomicBool,
    /// When blur last hid the popover — a tray click right after must not reopen it.
    hidden_at_ms: AtomicU64,
}

struct Busy<'a>(&'a AtomicBool);

impl<'a> Busy<'a> {
    fn take(flag: &'a AtomicBool) -> Result<Self, String> {
        if flag.swap(true, Ordering::SeqCst) {
            Err("A switch or re-login is already running".into())
        } else {
            Ok(Self(flag))
        }
    }
}

impl Drop for Busy<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or_default()
}

/* ------------------------------- state ---------------------------------- */

/// claude-swap stores mapped folders lowercased (it matches them
/// case-insensitively). Recover the on-disk casing for display; `None` when
/// the folder no longer exists.
fn on_disk_path(path: &str) -> Option<String> {
    let real = std::fs::canonicalize(path).ok()?.to_string_lossy().into_owned();
    Some(real.strip_prefix(r"\\?\").map(str::to_string).unwrap_or(real))
}

fn read_mappings() -> Vec<Value> {
    let file = cli::home().join(".claude-swap-backup").join("mappings.json");
    let Ok(text) = std::fs::read_to_string(file) else {
        return Vec::new();
    };
    let Ok(v) = serde_json::from_str::<Value>(&text) else {
        return Vec::new();
    };
    let mut out: Vec<Value> = v["mappings"]
        .as_object()
        .map(|m| {
            m.iter()
                .map(|(path, entry)| {
                    let real = on_disk_path(path);
                    json!({
                        "path": path,
                        "displayPath": real.clone().unwrap_or_else(|| path.clone()),
                        "exists": real.is_some(),
                        "email": entry["email"],
                        "added": entry["added"],
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    out.sort_by(|a, b| a["path"].as_str().cmp(&b["path"].as_str()));
    out
}

fn split<T: serde::Serialize>(r: Result<T, String>) -> (Value, Value) {
    match r {
        Ok(v) => (json!(v), Value::Null),
        Err(e) => (Value::Null, Value::String(e)),
    }
}

fn collect_state() -> Value {
    let list = std::thread::spawn(cli::cswap_list);
    let tokens = std::thread::spawn(|| cli::cswap_token_status().map(|t| tokens::parse(&t)));
    let auth = std::thread::spawn(cli::auth_status);
    let joined = |e: Box<dyn std::any::Any + Send>| format!("worker panicked: {e:?}");

    let (list, list_error) = split(list.join().map_err(joined).and_then(|r| r));
    let (tokens, tokens_error) = split(tokens.join().map_err(joined).and_then(|r| r));
    let (auth, auth_error) = split(auth.join().map_err(joined).and_then(|r| r));
    json!({
        "list": list,
        "listError": list_error,
        "tokens": tokens,
        "tokensError": tokens_error,
        "defaultLogin": auth,
        "defaultLoginError": auth_error,
        "mappings": read_mappings(),
        "sessions": sessions::on_default_login(),
        "fetchedAt": now_ms(),
    })
}

fn tray_tooltip(state: &Value) -> String {
    let Some(accounts) = state["list"]["accounts"].as_array() else {
        return "Claude Swap — claude-swap unavailable".into();
    };
    let active = accounts.iter().find(|a| a["active"] == Value::Bool(true));
    let mut s = match active {
        Some(a) => {
            let usage = if a["usage"].is_object() { &a["usage"] } else { &a["lastGoodUsage"] };
            let pct = |k: &str| usage[k]["pct"].as_f64().map(|p| format!("{p:.0}%")).unwrap_or("–".into());
            let mut line = format!(
                "Claude Swap — #{} {}\n5h {} · 7d {}",
                a["number"],
                a["email"].as_str().unwrap_or(""),
                pct("fiveHour"),
                pct("sevenDay")
            );
            line.push_str(&credits_line(usage));
            line
        }
        None => "Claude Swap — no active account".into(),
    };
    let unhealthy = state["tokens"]
        .as_array()
        .map(|t| {
            t.iter()
                .filter(|a| {
                    let want = if a["active"] == Value::Bool(true) { "active profile" } else { "stored backup" };
                    !a["lines"].as_array().is_some_and(|ls| {
                        ls.iter().any(|l| {
                            l["source"] == want && l["state"] == "fresh" && l["refresh"] == Value::Bool(true)
                        })
                    })
                })
                .count()
        })
        .unwrap_or(0);
    if unhealthy > 0 {
        s.push_str(&format!("\n⚠ {unhealthy} account(s) need re-login"));
    }
    s
}

/// A plan window (5h, 7d or a per-model one) is used up.
fn limit_spent(usage: &Value) -> bool {
    let full = |w: &Value| w["pct"].as_f64().is_some_and(|p| p >= 100.0);
    full(&usage["fiveHour"])
        || full(&usage["sevenDay"])
        || usage["scoped"].as_array().is_some_and(|s| s.iter().any(full))
}

fn money(amount: f64, currency: &str) -> String {
    match currency {
        "USD" | "" => format!("${amount:.2}"),
        "EUR" => format!("€{amount:.2}"),
        "GBP" => format!("£{amount:.2}"),
        other => format!("{amount:.2} {other}"),
    }
}

/// "\nCredits $19.85 / $50.00" once a limit is used up or credits were spent
/// (claude-swap's `usage.spend`); empty otherwise.
fn credits_line(usage: &Value) -> String {
    let spend = &usage["spend"];
    let Some(used) = spend["used"].as_f64() else {
        return String::new();
    };
    let spent = limit_spent(usage);
    if !spent && used <= 0.0 {
        return String::new();
    }
    let cur = spend["currency"].as_str().unwrap_or("USD");
    let amount = match spend["limit"].as_f64().filter(|l| *l > 0.0) {
        Some(limit) => format!("{} / {}", money(used, cur), money(limit, cur)),
        None => format!("{} spent", money(used, cur)),
    };
    format!("\nCredits {amount}{}", if spent { " (limit reached, on credits)" } else { "" })
}

fn update_tray(app: &AppHandle, state: &Value) {
    if let Some(tray) = app.tray_by_id(TRAY_ID) {
        let _ = tray.set_tooltip(Some(tray_tooltip(state)));
    }
}

/* ------------------------------ popover --------------------------------- */

fn show_popover(app: &AppHandle) {
    let Some(win) = app.get_webview_window("main") else { return };
    // Above the tray icon once the tray has reported where it is; until then
    // (opened from the tray menu first) the plugin errors, so use the corner.
    if win.move_window(Position::TrayCenter).is_err() {
        let _ = win.move_window(Position::BottomRight);
    }
    let _ = win.show();
    let _ = win.set_focus();
    let _ = app.emit("popover-shown", ());
}

fn toggle_popover(app: &AppHandle) {
    let Some(win) = app.get_webview_window("main") else { return };
    if win.is_visible().unwrap_or(false) {
        let _ = win.hide();
        return;
    }
    // Clicking the tray icon blurs the popover first; don't reopen what that just hid.
    let hidden_at = app.state::<AppState>().hidden_at_ms.load(Ordering::SeqCst);
    if now_ms().saturating_sub(hidden_at) < 300 {
        return;
    }
    show_popover(app);
}

fn build_tray(app: &AppHandle) -> tauri::Result<()> {
    let open = MenuItem::with_id(app, "open", "Open", true, None::<&str>)?;
    let tui = MenuItem::with_id(app, "tui", "Open claude-swap dashboard (TUI)", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&open, &tui, &PredefinedMenuItem::separator(app)?, &quit])?;

    let mut builder = TrayIconBuilder::with_id(TRAY_ID)
        .tooltip("Claude Swap")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "open" => show_popover(app),
            "tui" => {
                let _ = cli::open_tui();
            }
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            let app = tray.app_handle();
            tauri_plugin_positioner::on_tray_event(app, &event);
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                toggle_popover(app);
            }
        });
    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }
    builder.build(app)?;
    Ok(())
}

/* ------------------------------ commands -------------------------------- */

#[tauri::command]
async fn get_state(app: AppHandle) -> Result<Value, String> {
    let state = tauri::async_runtime::spawn_blocking(collect_state)
        .await
        .map_err(|e| e.to_string())?;
    update_tray(&app, &state);
    Ok(state)
}

#[tauri::command]
async fn switch_account(state: State<'_, AppState>, number: u32) -> Result<Value, String> {
    let _busy = Busy::take(&state.busy)?;
    tauri::async_runtime::spawn_blocking(move || cli::cswap_switch(number))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn relogin(
    app: AppHandle,
    state: State<'_, AppState>,
    number: u32,
    restore_previous: bool,
) -> Result<relogin::Outcome, String> {
    let _busy = Busy::take(&state.busy)?;
    state.cancel.store(false, Ordering::SeqCst);
    let handle = app.clone();
    let outcome = tauri::async_runtime::spawn_blocking(move || {
        let cancel = &handle.state::<AppState>().cancel;
        relogin::run(&handle, number, restore_previous, cancel)
    })
    .await
    .map_err(|e| e.to_string())?;
    show_popover(&app);
    Ok(outcome)
}

/// Same comparison claude-swap uses: on-disk path, case-folded.
fn same_folder(a: &str, b: &str) -> bool {
    let norm = |p: &str| {
        on_disk_path(p)
            .unwrap_or_else(|| p.to_string())
            .trim_end_matches('\\')
            .to_lowercase()
    };
    norm(a) == norm(b)
}

#[tauri::command]
async fn map_folder(state: State<'_, AppState>, number: u32, path: String) -> Result<Vec<Value>, String> {
    let _busy = Busy::take(&state.busy)?;
    tauri::async_runtime::spawn_blocking(move || {
        let p = std::path::Path::new(&path);
        if !p.is_absolute() || !p.is_dir() {
            return Err(format!("Not an existing folder: {path}"));
        }
        let out = cli::cswap_map(number, &path)?;
        if !out.ok() {
            return Err(format!("claude-swap map failed:\n{}", out.text()));
        }
        let mappings = read_mappings();
        let recorded = mappings
            .iter()
            .any(|m| m["path"].as_str().is_some_and(|k| same_folder(k, &path)));
        if recorded {
            Ok(mappings)
        } else {
            Err(format!("claude-swap didn't record the mapping:\n{}", out.text()))
        }
    })
    .await
    .map_err(|e| e.to_string())?
}

/// `path` is the stored (lowercased) key, which still works for a folder
/// that no longer exists.
#[tauri::command]
async fn unmap_folder(state: State<'_, AppState>, path: String) -> Result<Vec<Value>, String> {
    let _busy = Busy::take(&state.busy)?;
    tauri::async_runtime::spawn_blocking(move || {
        let out = cli::cswap_unmap(&path)?;
        if !out.ok() {
            return Err(format!("claude-swap unmap failed:\n{}", out.text()));
        }
        let mappings = read_mappings();
        if mappings.iter().any(|m| m["path"].as_str() == Some(path.as_str())) {
            Err(format!("The mapping is still there:\n{}", out.text()))
        } else {
            Ok(mappings)
        }
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn pick_folder(app: AppHandle, window: WebviewWindow) -> Result<Option<String>, String> {
    use tauri_plugin_dialog::DialogExt;
    let handle = app.clone();
    handle.state::<AppState>().dialog_open.store(true, Ordering::SeqCst);
    let picked = tauri::async_runtime::spawn_blocking(move || {
        app.dialog()
            .file()
            .set_title("Choose a folder to map")
            .set_parent(&window)
            .blocking_pick_folder()
    })
    .await;
    handle.state::<AppState>().dialog_open.store(false, Ordering::SeqCst);
    let picked = picked.map_err(|e| e.to_string())?;
    Ok(picked
        .and_then(|f| f.into_path().ok())
        .map(|p| p.to_string_lossy().into_owned()))
}

#[tauri::command]
fn cancel_relogin(state: State<'_, AppState>) {
    state.cancel.store(true, Ordering::SeqCst);
}

#[tauri::command]
fn set_pinned(state: State<'_, AppState>, pinned: bool) {
    state.pinned.store(pinned, Ordering::SeqCst);
}

#[tauri::command]
fn open_tui() -> Result<(), String> {
    cli::open_tui()
}

#[tauri::command]
fn hide_window(window: WebviewWindow) {
    let _ = window.hide();
}

#[tauri::command]
fn quit_app(app: AppHandle) {
    app.exit(0);
}

/* ----------------------------- autostart -------------------------------- */

/// Running from an installed location, not a build folder (`…\target\…`).
/// Only then is the exe path stable enough to register for startup.
fn is_installed() -> bool {
    std::env::current_exe()
        .map(|p| !p.components().any(|c| c.as_os_str().eq_ignore_ascii_case("target")))
        .unwrap_or(false)
}

/// Marker that the first-run default (start with Windows: on) was applied, so a
/// later "off" from the user sticks.
fn autostart_marker(app: &AppHandle) -> Option<std::path::PathBuf> {
    app.path().app_config_dir().ok().map(|d| d.join("autostart-initialized"))
}

fn mark_autostart_initialized(app: &AppHandle) {
    if let Some(marker) = autostart_marker(app) {
        if let Some(dir) = marker.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(marker, "");
    }
}

fn enable_autostart_on_first_run(app: &AppHandle) {
    use tauri_plugin_autostart::ManagerExt;
    if !is_installed() || autostart_marker(app).is_some_and(|m| m.exists()) {
        return;
    }
    if app.autolaunch().enable().is_ok() {
        mark_autostart_initialized(app);
    }
}

#[tauri::command]
fn get_autostart(app: AppHandle) -> Value {
    use tauri_plugin_autostart::ManagerExt;
    json!({
        "installed": is_installed(),
        "enabled": app.autolaunch().is_enabled().unwrap_or(false),
    })
}

#[tauri::command]
fn set_autostart(app: AppHandle, enabled: bool) -> Result<bool, String> {
    use tauri_plugin_autostart::ManagerExt;
    if !is_installed() {
        return Err("Install the app first: a build-folder exe can't start with Windows".into());
    }
    let launcher = app.autolaunch();
    if enabled { launcher.enable() } else { launcher.disable() }.map_err(|e| e.to_string())?;
    mark_autostart_initialized(&app);
    launcher.is_enabled().map_err(|e| e.to_string())
}

#[cfg(test)]
mod credit_tests {
    use super::credits_line;
    use serde_json::json;

    #[test]
    fn credits_shown_only_when_relevant() {
        // No spend data (extra usage not set up): nothing.
        assert_eq!(credits_line(&json!({ "fiveHour": { "pct": 100.0 } })), "");
        // Spend set up, nothing spent, limits fine: hidden.
        let idle = json!({ "fiveHour": { "pct": 40.0 }, "spend": { "used": 0.0, "limit": 50.0, "currency": "USD" } });
        assert_eq!(credits_line(&idle), "");
        // Already spent this period: shown.
        let used = json!({ "fiveHour": { "pct": 4.0 }, "spend": { "used": 19.85, "limit": 50.0, "currency": "USD" } });
        assert_eq!(credits_line(&used), "\nCredits $19.85 / $50.00");
        // A per-model limit used up: shown and flagged, even at $0.
        let spent = json!({
            "fiveHour": { "pct": 10.0 },
            "scoped": [{ "name": "Fable", "pct": 100.0 }],
            "spend": { "used": 0.0, "limit": 50.0, "currency": "USD" }
        });
        assert_eq!(credits_line(&spent), "\nCredits $0.00 / $50.00 (limit reached, on credits)");
    }
}

#[cfg(all(test, windows))]
mod tests {
    #[test]
    fn restores_on_disk_casing() {
        let dir = std::env::temp_dir().join("CswapCase_MixedName");
        std::fs::create_dir_all(&dir).unwrap();
        let real = super::on_disk_path(dir.to_str().unwrap()).unwrap();
        let from_lower = super::on_disk_path(&real.to_lowercase());
        let _ = std::fs::remove_dir(&dir);

        assert!(real.ends_with("CswapCase_MixedName"), "{real}");
        assert_eq!(from_lower.as_deref(), Some(real.as_str()));
        assert_eq!(super::on_disk_path(r"z:\definitely\not\here"), None);
    }
}

fn main() {
    tauri::Builder::default()
        // First, so a second launch (Start menu, autostart racing a manual
        // start) just opens the running app's popover and exits.
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| show_popover(app)))
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .plugin(tauri_plugin_positioner::init())
        .plugin(tauri_plugin_dialog::init())
        .manage(AppState::default())
        .invoke_handler(tauri::generate_handler![
            get_state,
            switch_account,
            relogin,
            cancel_relogin,
            map_folder,
            unmap_folder,
            pick_folder,
            set_pinned,
            open_tui,
            hide_window,
            quit_app,
            get_autostart,
            set_autostart
        ])
        .setup(|app| {
            build_tray(app.handle())?;
            enable_autostart_on_first_run(app.handle());
            Ok(())
        })
        .on_window_event(|window, event| match event {
            WindowEvent::Focused(false) => {
                let state = window.state::<AppState>();
                if !state.pinned.load(Ordering::SeqCst) && !state.dialog_open.load(Ordering::SeqCst) {
                    state.hidden_at_ms.store(now_ms(), Ordering::SeqCst);
                    let _ = window.hide();
                }
            }
            WindowEvent::CloseRequested { api, .. } => {
                api.prevent_close();
                let _ = window.hide();
            }
            _ => {}
        })
        .run(tauri::generate_context!())
        .expect("error while running Claude Swap Desktop");
}
