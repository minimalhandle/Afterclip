//! Keeps Afterclip up to date from GitHub releases. Every update is checked against a signature made with a
//! private key, so a tampered download is refused. It never interrupts a game or a recording: it waits, gives
//! a countdown that can be skipped, and only installs by itself when that can't raise a UAC prompt.
use crate::{api, on_game_or_recording, win32, Shared};
use serde::Serialize;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tauri::Emitter;
use tauri_plugin_updater::{Update, UpdaterExt};

const FIRST_CHECK_AFTER: Duration = Duration::from_secs(30);
const CHECK_EVERY: Duration = Duration::from_secs(6 * 60 * 60);
const RETRY_AFTER: Duration = Duration::from_secs(5 * 60);
const TICK: Duration = Duration::from_secs(15);
/// How long someone has to skip an update before it installs by itself.
const INSTALL_COUNTDOWN: Duration = Duration::from_secs(5 * 60);
const CHANGELOG: &str = include_str!("../../CHANGELOG.md");

/// One version's patch notes from CHANGELOG.md, without its heading.
pub fn patch_notes(version: &str) -> Option<String> {
    let heading = format!("## {version}");
    let start = CHANGELOG.lines().position(|line| line.trim() == heading)?;
    let body: Vec<&str> = CHANGELOG.lines().skip(start + 1).take_while(|line| !line.starts_with("## ")).collect();
    let notes = body.join("\n").trim().to_string();
    (!notes.is_empty()).then_some(notes)
}

#[derive(Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateState {
    /// idle, checking, current, downloading, ready, installing or failed.
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// When it installs by itself, in milliseconds since 1970. None means it waits for a click.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub installs_at: Option<u128>,
    /// The patch notes that came with the update.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

fn publish(core: &Shared, change: impl FnOnce(&mut UpdateState)) {
    let mut state = core.update.lock().unwrap();
    change(&mut state);
    let _ = core.app.emit("update", state.clone());
}

fn set_status(core: &Shared, status: &str, version: Option<String>, message: Option<String>) {
    publish(core, |s| {
        s.status = status.into();
        s.version = version;
        s.message = message;
        s.installs_at = None;
        s.notes = None;
    });
}

async fn find_update(core: &Shared) -> Result<Option<Update>, String> {
    let mut builder = core.app.updater_builder();
    if api::is_test_instance() {
        if let Ok(url) = std::env::var("AFTERCLIP_UPDATE_URL") {
            let url = url.parse().map_err(|_| "Bad test update address".to_string())?;
            builder = builder.endpoints(vec![url]).map_err(|e| e.to_string())?;
        }
    }
    let engine = Arc::clone(&core.engine);
    let builder = builder.on_before_exit(move || engine.stop());
    builder.build().map_err(|e| e.to_string())?.check().await.map_err(|e| e.to_string())
}

/// Looks for an update and downloads it. `asked` means someone pressed the button, so it runs even during a
/// game, and offers a version they skipped earlier. Returns false if it should try again soon.
pub fn check(core: &Shared, asked: bool) -> bool {
    if core.update_busy.swap(true, Ordering::SeqCst) {
        return true;
    }
    let result = tauri::async_runtime::block_on(check_inner(core, asked));
    core.update_busy.store(false, Ordering::SeqCst);
    result
}

async fn check_inner(core: &Shared, asked: bool) -> bool {
    if core.update.lock().unwrap().status == "ready" {
        return true;
    }
    if asked {
        set_status(core, "checking", None, None);
    }
    let found = match find_update(core).await {
        Ok(found) => found,
        Err(message) => {
            core.log_ui(&format!("update check failed: {message}"));
            // A failed check nobody asked for isn't worth interrupting anyone over.
            if asked {
                set_status(core, "failed", None, Some(format!("Couldn't check for updates: {message}")));
            }
            return false;
        }
    };
    let Some(update) = found else {
        if asked {
            set_status(core, "current", None, None);
        }
        return true;
    };
    let version = update.version.clone();
    if !asked && core.settings.lock().unwrap().skipped_update == version {
        return true;
    }
    // A download can hurt a game's connection, so it waits unless someone asked for it now.
    if !asked && on_game_or_recording(core) {
        return false;
    }
    set_status(core, "downloading", Some(version.clone()), None);
    match update.download(|_, _| {}, || {}).await {
        Ok(bytes) => {
            let notes = update.body.clone().filter(|n| !n.trim().is_empty());
            *core.update_ready.lock().unwrap() = Some((update, bytes));
            set_status(core, "ready", Some(version.clone()), None);
            publish(core, |s| s.notes = notes);
            if core.settings.lock().unwrap().toast_enabled {
                win32::show_toast(&format!("Update {version} is ready"));
            }
            true
        }
        Err(message) => {
            core.log_ui(&format!("update download failed: {message}"));
            set_status(core, "failed", Some(version), Some(format!("The update couldn't be downloaded: {message}")));
            false
        }
    }
}

pub fn install(core: &Shared) -> Result<(), String> {
    if core.engine.status().recording {
        return Err("Stop recording before updating".into());
    }
    let Some((update, bytes)) = core.update_ready.lock().unwrap().take() else {
        return Err("There's no update ready to install".into());
    };
    let version = update.version.clone();
    set_status(core, "installing", Some(version.clone()), None);
    // The tests stop here, so they can never replace the app that's running them.
    if api::is_test_instance() {
        return Ok(());
    }
    core.quitting.store(true, Ordering::SeqCst);
    // On success this runs the installer and exits.
    let result = update.install(bytes).map_err(|e| e.to_string());
    if let Err(message) = &result {
        core.quitting.store(false, Ordering::SeqCst);
        core.log_ui(&format!("update install failed: {message}"));
        set_status(core, "failed", Some(version), Some(format!("The update couldn't be installed: {message}")));
        let engine = Arc::clone(&core.engine);
        thread::spawn(move || { let _ = engine.start(None); });
    }
    result
}

/// Skipping forgets the download and keeps this version from being offered again. A newer one still will be.
pub fn skip(core: &Shared) -> Result<(), String> {
    let version = core.update.lock().unwrap().version.clone().ok_or("There's no update to skip")?;
    let saved = {
        let mut settings = core.settings.lock().unwrap();
        settings.skipped_update = version;
        settings.clone()
    };
    saved.save(&core.settings_file).map_err(|e| e.to_string())?;
    *core.update_ready.lock().unwrap() = None;
    set_status(core, "idle", None, None);
    Ok(())
}

fn now_ms() -> u128 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis())
}

/// Starts or holds the countdown. Without admin rights the installer would raise a UAC prompt out of nowhere,
/// so those installs wait for a click.
fn tick_countdown(core: &Shared) {
    let (status, installs_at) = {
        let state = core.update.lock().unwrap();
        (state.status.clone(), state.installs_at)
    };
    if status != "ready" {
        return;
    }
    let allowed = core.settings.lock().unwrap().auto_update && win32::is_self_elevated() && !api::is_test_instance();
    if !allowed || on_game_or_recording(core) {
        if installs_at.is_some() {
            publish(core, |s| s.installs_at = None);
        }
        return;
    }
    match installs_at {
        None => publish(core, |s| s.installs_at = Some(now_ms() + INSTALL_COUNTDOWN.as_millis())),
        Some(at) if now_ms() >= at => { let _ = install(core); }
        Some(_) => {}
    }
}

pub fn start(core: Shared) {
    if api::is_test_instance() {
        return;
    }
    thread::spawn(move || {
        let mut next_check = Instant::now() + FIRST_CHECK_AFTER;
        loop {
            thread::sleep(TICK);
            if Instant::now() >= next_check && core.settings.lock().unwrap().auto_update {
                next_check = Instant::now() + if check(&core, false) { CHECK_EVERY } else { RETRY_AFTER };
            }
            tick_countdown(&core);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patch_notes_cover_this_version() {
        let notes = patch_notes(env!("CARGO_PKG_VERSION")).expect("CHANGELOG.md needs a section for this version");
        assert!(notes.lines().all(|line| !line.starts_with("## ")));
        assert_eq!(patch_notes("0.0.0"), None);
    }
}
