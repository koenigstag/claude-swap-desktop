# Claude Swap Desktop

A small Windows tray app for [claude-swap](https://github.com/realiti4/claude-swap). It shows each
account's usage and token health, switches the default login, and runs a guided **re-login**.

Built with Tauri 2 (Rust + a plain HTML/JS popover, no frontend framework). The release exe is about 6 MB.
The UI and flow were inspired by [Claude-Swap-Desktop](https://github.com/adithyanraj03/Claude-Swap-Desktop)
(MIT). This is a separate implementation.

## What it does

- **Accounts:** 5h / 7d / per-model usage from `claude-swap list --json`, plus token health per credential copy
  (active profile, session profile, stored backup) parsed from `claude-swap list --token-status`.
- **Warnings:**
  - the default login (`~/.claude`) is signed out
  - it's signed in as a different account than claude-swap thinks
  - the default-login account is also mapped to folders. Sessions there run from a session profile that shares
    one single-use refresh token with the default login, so one copy can invalidate the other.
- **Make default:** `claude-swap switch <n>`, behind a lock (🔒) and a confirmation.
- **Mappings tab:** every `claude-swap map` entry with its on-disk path casing and account token health.
  - **Add / Change** (`claude-swap map <n> <folder>`) with a folder picker. It warns when the account is the default
    login, when the folder is already mapped, and when it overrides a parent folder's mapping.
  - **Remove** (`claude-swap unmap <folder>`) says what the folder falls back to: the closest mapped parent,
    otherwise the default login.
  - The result is checked against `mappings.json` afterwards, because `unmap` succeeds silently even when nothing was
    mapped.
- **Re-login** (per account):
  1. save the current default login (`claude-swap add`), but only if its live token is healthy
  2. open a terminal with `claude auth login --email <account>` and wait for it to close (cancellable, 15 min limit)
  3. check `claude auth status --json` shows the expected email and organization. Otherwise stop before saving.
  4. `claude-swap add`
  5. switch the default login back to the previous account (on by default)
  6. confirm the account shows `fresh` with a refresh token in `claude-swap list --token-status`

Every external call runs the program directly (no shell), with no console window and a timeout.
`CLAUDE_CONFIG_DIR` and `CSWAP_ACCOUNT` are removed from their environment, so `~/.claude` is always the
login being addressed. The app only reads `~/.claude-swap-backup/mappings.json` and `~/.claude/sessions/*.json`.
It never touches credential files itself.

## Requirements

Windows 10/11 with WebView2, `claude-swap` and `claude` (`claude.exe`) on PATH. `~/.local/bin` and
`C:\tools\direnv\bin` are also checked.

## Develop

```bash
npm install
```

```bash
npm run dev
```

Unit tests (the live test runs the real `claude-swap`):

```bash
cargo test --manifest-path src-tauri/Cargo.toml -- --include-ignored
```

UI preview in a browser with mocked data: serve the repo root (e.g. `python -m http.server 5178`) and open
`/dev/preview.html`, or `/dev/preview.html?scenario=broken`.

## Build

Standalone exe (`src-tauri/target/release/claude-swap-desktop.exe`):

```bash
npm run build:exe
```

Installer (NSIS):

```bash
npm run build
```

## Install and start with Windows

Run `src-tauri\target\release\bundle\nsis\Claude Swap Desktop_<version>_x64-setup.exe`. It installs per-user (no admin)
to `%LOCALAPPDATA%\Claude Swap Desktop\`, with a Start menu shortcut and an uninstall entry. Add `/S` for a silent
install. To update, run a newer installer over it, after quitting the app from the tray.

- **Start with Windows** is turned on automatically the first time the *installed* app runs: a per-user
  `HKCU\Software\Microsoft\Windows\CurrentVersion\Run` entry named "Claude Swap Desktop". Turn it off with the
  checkbox in the popover footer, or in Task Manager → Startup apps. Once you've turned it off, it stays off.
- A build run from `src-tauri\target\…` never registers itself, because that path changes on every rebuild. The
  checkbox is disabled there.
- **Single instance:** starting it again (Start menu or autostart) opens the running app's popover instead of adding
  a second tray icon.
