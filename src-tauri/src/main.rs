#![windows_subsystem = "windows"]

mod api;
mod capture;
mod controller;
mod data_move;
mod discord;
mod elevated_task;
mod engine;
mod library;
mod settings;
mod updater;
mod win32;

use api::{start_api, ApiHandlers};
use base64::Engine as _;
use capture::CaptureManager;
use engine::{ClipResult, Engine, EngineEvent};
use library::{clean_file_stem, older_than, over_limit, LibraryData, StoredClip};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use settings::{estimate_buffer_mb, Settings};
use std::fs;
use std::io::Write;
use std::os::windows::ffi::OsStrExt;
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};
use tauri::image::Image;
use tauri::menu::MenuBuilder;
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::utils::config::Color;
use webview2_com::Microsoft::Web::WebView2::Win32::{COREWEBVIEW2_PROCESS_FAILED_KIND, COREWEBVIEW2_PROCESS_FAILED_KIND_BROWSER_PROCESS_EXITED, COREWEBVIEW2_PROCESS_FAILED_KIND_RENDER_PROCESS_EXITED};
use webview2_com::ProcessFailedEventHandler;
use tauri::{AppHandle, Emitter, Manager, PhysicalPosition, RunEvent, WebviewUrl, WebviewWindow, WebviewWindowBuilder, WindowEvent};
use tauri_plugin_dialog::DialogExt;
use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};
use win32::play_tones;
use windows::core::PCWSTR;
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

const PANEL: &str = "panel";
const EDITOR: &str = "editor";
const BACKGROUND: Color = Color(14, 12, 31, 255);
// The panel keeps its window this long after hiding, so reopening is instant, and then frees the memory.
const PANEL_KEEP_ALIVE: Duration = Duration::from_secs(20);
// A window that hasn't said it rendered by now gets reloaded, then shown regardless.
const READY_RELOAD_AFTER: Duration = Duration::from_secs(4);
const READY_SHOW_AFTER: Duration = Duration::from_secs(8);

struct Core {
    app: AppHandle,
    engine: Arc<Engine>,
    capture: OnceLock<Arc<CaptureManager>>,
    settings: Arc<Mutex<Settings>>,
    settings_file: PathBuf,
    token_file: PathBuf,
    ui_log: PathBuf,
    api: Mutex<Option<Arc<tiny_http::Server>>>,
    hotkey_failures: Mutex<Vec<String>>,
    thumbnail_lock: Mutex<()>,
    restart_lock: Mutex<()>,
    quitting: AtomicBool,
    /// Set while a dialog is open, because the panel losing focus to it must not close it.
    keep_panel_open: AtomicBool,
    panel_hidden_at: Mutex<Option<Instant>>,
    panel_recovered_at: Mutex<Option<Instant>>,
    panel_hide_count: AtomicU64,
    tray_anchor: Mutex<Option<(i32, i32)>>,
    pending_editor_files: Mutex<Vec<String>>,
    ready_windows: Mutex<Vec<String>>,
    library: Mutex<LibraryData>,
    library_file: PathBuf,
    /// The game whose profile capture is using right now.
    active_profile: Mutex<Option<String>>,
    test_game: Mutex<Option<String>>,
    controller: OnceLock<Arc<controller::Controller>>,
    update: Mutex<updater::UpdateState>,
    update_ready: Mutex<Option<(tauri_plugin_updater::Update, Vec<u8>)>>,
    update_busy: AtomicBool,
    storage_lock: Mutex<()>,
}

type Shared = Arc<Core>;

/// Updates hold off during these, so they never interrupt anything.
pub(crate) fn on_game_or_recording(core: &Shared) -> bool {
    core.engine.status().recording || running_game(core).is_some()
}

fn running_game(core: &Core) -> Option<String> {
    core.test_game.lock().unwrap().clone().or_else(|| core.capture.get().and_then(|c| c.state().exe))
}

impl Core {
    /// The base settings with the running game's profile on top, which is what capture actually uses.
    fn effective(&self, base: &Settings) -> Settings {
        let active = self.active_profile.lock().unwrap().clone();
        match active.and_then(|exe| base.game_profiles.iter().find(|p| p.exe == exe).cloned()) {
            Some(profile) => base.with_profile(&profile, win32::primary_display_mode()),
            None => base.clone(),
        }
    }

    fn log_ui(&self, message: &str) {
        if let Ok(mut file) = fs::OpenOptions::new().create(true).append(true).open(&self.ui_log) {
            let _ = writeln!(file, "{} {message}", chrono::Local::now().format("%Y-%m-%d %H:%M:%S"));
        }
    }

    fn tone(&self, frequencies: &[f64]) {
        if self.settings.lock().unwrap().beep_enabled {
            play_tones(frequencies);
        }
    }

    fn clip(&self, seconds: i64) -> Result<ClipResult, String> {
        match self.engine.clip(seconds) {
            Ok(result) => {
                self.tone(&[880.0, 1320.0]);
                if self.settings.lock().unwrap().toast_enabled {
                    let total = result.actual_seconds.round() as i64;
                    win32::show_toast(&format!("Clip saved · {}:{:02}", total / 60, total % 60));
                }
                let short = seconds > 0 && result.actual_seconds < seconds as f64 - 1.0;
                let _ = self.app.emit("feedback", json!({
                    "kind": "clip", "ok": true, "short": short, "file": result.file,
                    "requestedSeconds": result.requested_seconds, "actualSeconds": result.actual_seconds,
                }));
                if let Some(core) = self.app.try_state::<Shared>() {
                    let core: Shared = Arc::clone(&core);
                    thread::spawn(move || enforce_storage_rules(&core));
                }
                Ok(result)
            }
            Err(message) => {
                self.tone(&[220.0, 220.0]);
                let _ = self.app.emit("feedback", json!({ "kind": "clip", "ok": false, "message": message }));
                Err(message)
            }
        }
    }

    fn toggle_record(&self) -> Result<bool, String> {
        match self.engine.toggle_record() {
            Ok(recording) => {
                self.tone(if recording { &[660.0, 880.0] } else { &[880.0, 660.0] });
                let _ = self.app.emit("feedback", json!({ "kind": "record", "ok": true, "recording": recording }));
                Ok(recording)
            }
            Err(message) => {
                self.tone(&[220.0, 220.0]);
                let _ = self.app.emit("feedback", json!({ "kind": "record", "ok": false, "message": message }));
                Err(message)
            }
        }
    }

    fn toggle_mic(&self) -> Result<bool, String> {
        let muted = !self.settings.lock().unwrap().mic_muted;
        match self.engine.set_mic_muted(muted) {
            Ok(()) => {
                let saved = {
                    let mut settings = self.settings.lock().unwrap();
                    settings.mic_muted = muted;
                    settings.clone()
                };
                let _ = saved.save(&self.settings_file);
                self.engine.use_settings(self.effective(&saved));
                // A falling pair for muted and a rising pair for live, like recording.
                self.tone(if muted { &[660.0, 440.0] } else { &[440.0, 660.0] });
                let _ = self.app.emit("feedback", json!({ "kind": "mic", "ok": true, "micMuted": muted }));
                Ok(muted)
            }
            Err(message) => {
                self.tone(&[220.0, 220.0]);
                let _ = self.app.emit("feedback", json!({ "kind": "mic", "ok": false, "message": message }));
                Err(message)
            }
        }
    }

    fn toggle_clean_mic(&self) -> Result<bool, String> {
        let on = !self.settings.lock().unwrap().clean_mic;
        match self.engine.set_clean_mic(on) {
            Ok(()) => {
                let saved = {
                    let mut settings = self.settings.lock().unwrap();
                    settings.clean_mic = on;
                    settings.clone()
                };
                let _ = saved.save(&self.settings_file);
                self.engine.use_settings(self.effective(&saved));
                self.tone(if on { &[440.0, 660.0] } else { &[660.0, 440.0] });
                let _ = self.app.emit("feedback", json!({ "kind": "clean-mic", "ok": true, "cleanMic": on }));
                Ok(on)
            }
            Err(message) => {
                self.tone(&[220.0, 220.0]);
                let _ = self.app.emit("feedback", json!({ "kind": "clean-mic", "ok": false, "message": message }));
                Err(message)
            }
        }
    }

    fn add_marker(&self) -> Result<u64, String> {
        match self.engine.add_marker() {
            Ok(number) => {
                self.tone(&[990.0]);
                if self.settings.lock().unwrap().toast_enabled {
                    win32::show_toast(&format!("Marker {number} added"));
                }
                let _ = self.app.emit("feedback", json!({ "kind": "marker", "ok": true, "number": number }));
                Ok(number)
            }
            Err(message) => {
                self.tone(&[220.0, 220.0]);
                let _ = self.app.emit("feedback", json!({ "kind": "marker", "ok": false, "message": message }));
                Err(message)
            }
        }
    }

    fn screenshot(&self) -> Result<PathBuf, String> {
        match self.engine.screenshot() {
            Ok(file) => {
                self.tone(&[1320.0]);
                if self.settings.lock().unwrap().toast_enabled {
                    win32::show_toast("Screenshot saved");
                }
                let _ = self.app.emit("feedback", json!({ "kind": "screenshot", "ok": true, "file": file }));
                if let Some(core) = self.app.try_state::<Shared>() {
                    let core: Shared = Arc::clone(&core);
                    thread::spawn(move || enforce_storage_rules(&core));
                }
                Ok(file)
            }
            Err(message) => {
                self.tone(&[220.0, 220.0]);
                let _ = self.app.emit("feedback", json!({ "kind": "screenshot", "ok": false, "message": message }));
                Err(message)
            }
        }
    }

    fn capture_state(&self) -> Value {
        self.capture.get().map(|c| json!(c.state())).unwrap_or(json!({ "mode": "desktop" }))
    }

    /// Lets the editor window load videos from the output folders, and nothing else.
    fn allow_media_folders(&self) {
        let scope = self.app.asset_protocol_scope();
        let _ = scope.allow_directory(self.engine.clips_dir(), false);
        let _ = scope.allow_directory(self.engine.recordings_dir(), false);
        let _ = scope.allow_directory(self.engine.screenshots_dir(), false);
    }
}

struct Api(Shared);

impl ApiHandlers for Api {
    fn clip(&self, seconds: i64) -> Result<Value, String> {
        self.0.clip(seconds).map(|r| json!(r))
    }
    fn toggle_record(&self) -> Result<Value, String> {
        self.0.toggle_record().map(|recording| json!({ "recording": recording }))
    }
    fn toggle_mic(&self) -> Result<Value, String> {
        self.0.toggle_mic().map(|muted| json!({ "micMuted": muted }))
    }
    fn toggle_clean_mic(&self) -> Result<Value, String> {
        self.0.toggle_clean_mic().map(|on| json!({ "cleanMic": on }))
    }
    fn add_marker(&self) -> Result<Value, String> {
        self.0.add_marker().map(|number| json!({ "marker": number }))
    }
    fn screenshot(&self) -> Result<Value, String> {
        self.0.screenshot().map(|file| json!({ "file": file }))
    }
    fn open_panel(&self) -> Result<Value, String> {
        show_panel(&self.0);
        Ok(json!({}))
    }
    fn open_clips_folder(&self) -> Result<Value, String> {
        let folder = self.0.engine.clips_dir();
        fs::create_dir_all(&folder).map_err(|e| e.to_string())?;
        // The tests check the address without leaving Explorer windows on someone's desktop.
        if !api::is_test_instance() && !shell_execute("open", folder.as_os_str(), None, None) {
            return Err("Could not open the clips folder".into());
        }
        Ok(json!({ "folder": folder }))
    }
    fn edit_last_clip(&self) -> Result<Value, String> {
        let clip = last_clip(&self.0)?;
        show_editor(&self.0, vec![clip.clone()]);
        Ok(json!({ "file": clip }))
    }
    fn copy_last_clip(&self) -> Result<Value, String> {
        let clip = last_clip(&self.0)?;
        win32::copy_file(Path::new(&clip))?;
        Ok(json!({ "file": clip }))
    }
    fn test_open(&self, what: &str) {
        match what {
            "panel" => show_panel(&self.0),
            "editor" => show_editor(&self.0, Vec::new()),
            _ => {}
        }
    }
    /// Lets the UI tests run while someone uses the PC, because any click elsewhere closes the panel.
    fn test_game(&self, exe: &str) {
        *self.0.test_game.lock().unwrap() = Some(exe.to_string()).filter(|e| !e.is_empty());
    }
    /// Sends a controller report through the real path. It's written as hex, with zeros to pad it to size.
    fn test_controller(&self, kind: &str, report: &str) {
        let kind = if kind == "switch" { controller::Kind::SwitchPro } else { controller::Kind::DualSense };
        let bytes: Vec<u8> = report.as_bytes().chunks(2).filter_map(|pair| u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok()).collect();
        if let Some(listener) = self.0.controller.get() {
            listener.test_report(kind, &bytes);
        }
    }
    fn test_hold_panel(&self, hold: bool) {
        self.0.keep_panel_open.store(hold, Ordering::SeqCst);
        // Focus left during the hold, so no blur is coming. Do what that blur would have done.
        let focused = self.0.app.get_webview_window(PANEL).is_some_and(|w| w.is_focused().unwrap_or(false));
        if !hold && !focused {
            on_panel_blur(&self.0);
        }
    }
    fn status(&self) -> Value {
        let mut status = json!(self.0.engine.status());
        status["capture"] = self.0.capture_state();
        status["hotkeyFailures"] = json!(*self.0.hotkey_failures.lock().unwrap());
        let settings = self.0.settings.lock().unwrap();
        status["micMuted"] = json!(settings.mic_muted);
        status["cleanMic"] = json!(settings.clean_mic);
        drop(settings);
        status
    }
}

fn register_hotkeys(core: &Shared) {
    let shortcuts = core.app.global_shortcut();
    let _ = shortcuts.unregister_all();
    let mut failures = Vec::new();
    let settings = core.settings.lock().unwrap().clone();
    let mut register = |accelerator: &str, action: Box<dyn Fn(&Shared) + Send + Sync>| {
        if accelerator.is_empty() {
            return;
        }
        let owner = Arc::clone(core);
        let registered = shortcuts.on_shortcut(accelerator, move |_, _, event| {
            if event.state() == ShortcutState::Pressed {
                action(&owner);
            }
        });
        if registered.is_err() {
            failures.push(accelerator.to_string());
        }
    };
    for preset in &settings.presets {
        let seconds = preset.seconds;
        register(&preset.hotkey, Box::new(move |core| {
            let core = Arc::clone(core);
            thread::spawn(move || { let _ = core.clip(seconds); });
        }));
    }
    register(&settings.record_hotkey, Box::new(|core| {
        let core = Arc::clone(core);
        thread::spawn(move || { let _ = core.toggle_record(); });
    }));
    register(&settings.mic_hotkey, Box::new(|core| {
        let core = Arc::clone(core);
        thread::spawn(move || { let _ = core.toggle_mic(); });
    }));
    register(&settings.marker_hotkey, Box::new(|core| {
        let core = Arc::clone(core);
        thread::spawn(move || { let _ = core.add_marker(); });
    }));
    register(&settings.screenshot_hotkey, Box::new(|core| {
        let core = Arc::clone(core);
        thread::spawn(move || { let _ = core.screenshot(); });
    }));
    *core.hotkey_failures.lock().unwrap() = failures;
}

#[derive(Serialize)]
struct Media {
    file: String,
    name: String,
    size: u64,
    mtime: f64,
    kind: &'static str,
    favourite: bool,
    /// Only set for a renamed clip, whose file name no longer says.
    #[serde(skip_serializing_if = "Option::is_none")]
    game: Option<String>,
}

// More than this is too much to send to the panel in one go.
const MAX_LISTED_MEDIA: usize = 2000;

fn modified_ms(meta: &fs::Metadata) -> f64 {
    meta.modified().ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as f64)
        .unwrap_or(0.0)
}

fn list_media(core: &Core) -> Vec<Media> {
    let active = core.engine.active_recording();
    let library = core.library.lock().unwrap();
    let mut media = Vec::new();
    const VIDEOS: &[&str] = &["mp4", "mkv", "mov"];
    for (dir, kind, extensions) in [(core.engine.clips_dir(), "clip", VIDEOS), (core.engine.recordings_dir(), "recording", VIDEOS), (core.engine.screenshots_dir(), "screenshot", &["png"])] {
        let Ok(entries) = fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            let wanted = path.extension().and_then(|e| e.to_str())
                .is_some_and(|e| extensions.contains(&e.to_lowercase().as_str()));
            if !wanted || active.as_deref() == Some(path.as_path()) {
                continue;
            }
            let Ok(meta) = entry.metadata() else { continue };
            let name = entry.file_name().to_string_lossy().into_owned();
            media.push(Media {
                file: path.to_string_lossy().into_owned(),
                favourite: library.favourites.contains(&name),
                game: library.games.get(&name).cloned(),
                name,
                size: meta.len(),
                mtime: modified_ms(&meta),
                kind,
            });
        }
    }
    media.sort_by(|a, b| b.mtime.total_cmp(&a.mtime));
    media
}

/// The newest saved clip. Recordings aren't clips, so they don't count.
fn last_clip(core: &Shared) -> Result<String, String> {
    list_media(core).into_iter().find(|m| m.kind == "clip").map(|m| m.file).ok_or_else(|| "No clips yet".to_string())
}

/// Applies the chosen age and size limits. Runs after saves, and regularly for recordings and for clips getting older.
fn enforce_storage_rules(core: &Shared) {
    let (limit_gb, days) = {
        let settings = core.settings.lock().unwrap();
        (settings.storage_limit_gb, settings.delete_after_days)
    };
    if limit_gb <= 0.0 && days <= 0 {
        return;
    }
    let _one_at_a_time = core.storage_lock.lock().unwrap();
    let clips: Vec<StoredClip> = list_media(core).into_iter().map(|m| StoredClip {
        path: PathBuf::from(m.file),
        size: m.size,
        modified: m.mtime,
        favourite: m.favourite,
    }).collect();
    let now_ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0.0, |d| d.as_millis() as f64);
    let mut chosen = if days > 0 { older_than(&clips, days, now_ms) } else { Vec::new() };
    if limit_gb > 0.0 {
        let left = clips.into_iter().filter(|c| !chosen.contains(&c.path)).collect();
        chosen.extend(over_limit(left, (limit_gb * 1e9) as u64));
    }
    let mut removed = 0;
    for path in chosen {
        match remove_media(core, &path) {
            Ok(()) => removed += 1,
            Err(e) => core.log_ui(&format!("storage rules could not remove {}: {e}", path.display())),
        }
    }
    if removed > 0 {
        let _ = core.app.emit("feedback", json!({ "kind": "storage", "ok": true, "removed": removed }));
    }
}

/// Sends a clip to the Recycle Bin, and forgets what was known about it.
fn remove_media(core: &Core, path: &Path) -> Result<(), String> {
    trash::delete(path).map_err(|e| e.to_string())?;
    let name = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
    forget_thumbnails(core, &name);
    let mut library = core.library.lock().unwrap();
    library.forget(&name);
    library.save(&core.library_file)
}

fn forget_thumbnails(core: &Core, name: &str) {
    let prefix = format!("{name}-");
    if let Ok(entries) = fs::read_dir(core.engine.thumbs_dir()) {
        for entry in entries.flatten() {
            if entry.file_name().to_string_lossy().starts_with(&prefix) {
                let _ = fs::remove_file(entry.path());
            }
        }
    }
}

const PANEL_RECOVER_GAP: Duration = Duration::from_secs(30);

// A game's profile stays on this long after it closes, so its last moments are still in the buffer to clip.
fn profile_hold() -> Duration {
    if api::is_test_instance() { Duration::from_secs(3) } else { Duration::from_secs(10 * 60) }
}
const GAME_WATCH_EVERY: Duration = Duration::from_secs(2);
// Polls in a row with no game before an automatic recording stops, so a brief alt-tab or restart doesn't end it.
const GAME_GONE_POLLS: u32 = 3;
const STORAGE_CHECK_POLLS: u32 = 30;

/// Applies per-game profiles and automatic recording as games come and go.
fn watch_games(core: Shared) {
    let mut last_seen = Instant::now();
    let mut handled: Option<String> = None;
    let mut started_recording = false;
    let mut gone_polls = 0;
    let mut storage_polls = 0;
    loop {
        thread::sleep(GAME_WATCH_EVERY);
        storage_polls += 1;
        if storage_polls >= STORAGE_CHECK_POLLS {
            storage_polls = 0;
            enforce_storage_rules(&core);
        }
        let base = core.settings.lock().unwrap().clone();
        let game = running_game(&core);
        if game.is_some() {
            last_seen = Instant::now();
        }
        let recording = core.engine.status().recording;

        let active = core.active_profile.lock().unwrap().clone();
        let wanted = match &game {
            Some(exe) => Some(exe.clone()).filter(|e| base.game_profiles.iter().any(|p| &p.exe == e)),
            None if last_seen.elapsed() < profile_hold() => active.clone(),
            None => None,
        };
        // A restart would end a recording, so a change waits until it stops.
        if wanted != active && !recording && core.engine.status().state == "ready" {
            *core.active_profile.lock().unwrap() = wanted.clone();
            let effective = core.effective(&base);
            if core.engine.current_settings().needs_engine_restart(&effective) {
                let _one_at_a_time = core.restart_lock.lock().unwrap();
                core.engine.stop();
                let _ = core.engine.start(Some(effective.clone()));
                core.allow_media_folders();
            } else {
                core.engine.use_settings(effective.clone());
            }
            let _ = core.app.emit("profile", json!({ "game": wanted, "bufferSeconds": effective.buffer_seconds }));
        }

        match &game {
            Some(exe) => {
                gone_polls = 0;
                let wants_recording = base.game_profiles.iter().any(|p| &p.exe == exe && p.auto_record);
                let settled = wanted == *core.active_profile.lock().unwrap() && core.engine.status().buffering;
                if handled.as_ref() != Some(exe) && settled {
                    handled = Some(exe.clone());
                    if wants_recording && !recording && core.toggle_record().is_ok() {
                        started_recording = true;
                    }
                }
            }
            None => {
                gone_polls += 1;
                if gone_polls >= GAME_GONE_POLLS {
                    handled = None;
                    if started_recording {
                        started_recording = false;
                        if recording {
                            let _ = core.toggle_record();
                        }
                    }
                }
            }
        }
    }
}

/// The UI only gets to act on files inside our own output folders.
fn own_media(core: &Core, file: &str) -> Result<PathBuf, String> {
    let path = PathBuf::from(file);
    let traversal = path.components().any(|c| matches!(c, Component::ParentDir));
    let parent = path.parent().map(Path::to_path_buf);
    let own_folder = [core.engine.clips_dir(), core.engine.recordings_dir(), core.engine.screenshots_dir()].into_iter().any(|dir| parent.as_ref() == Some(&dir));
    if traversal || !own_folder {
        return Err("Not a Afterclip file".into());
    }
    Ok(path)
}

fn wide(text: &std::ffi::OsStr) -> Vec<u16> {
    text.encode_wide().chain(std::iter::once(0)).collect()
}

fn shell_execute(verb: &str, file: &std::ffi::OsStr, args: Option<&str>, dir: Option<&Path>) -> bool {
    let verb = wide(verb.as_ref());
    let file = wide(file);
    let args = args.map(|a| wide(a.as_ref()));
    let dir = dir.map(|d| wide(d.as_os_str()));
    let args_ptr = args.as_ref().map_or(PCWSTR::null(), |a| PCWSTR(a.as_ptr()));
    let dir_ptr = dir.as_ref().map_or(PCWSTR::null(), |d| PCWSTR(d.as_ptr()));
    let result = unsafe { ShellExecuteW(None, PCWSTR(verb.as_ptr()), PCWSTR(file.as_ptr()), args_ptr, dir_ptr, SW_SHOWNORMAL) };
    // ShellExecute reports success as a value above 32.
    result.0 as isize > 32
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct EstimateArgs {
    buffer_seconds: i64,
    bitrate_kbps: i64,
}

#[derive(Deserialize)]
struct ExportSegment {
    file: String,
    start: f64,
    end: f64,
}

#[derive(Deserialize)]
struct ExportArgs {
    segments: Vec<ExportSegment>,
    /// Each audio track's volume, where 1 leaves it untouched and 0 removes it.
    #[serde(default)]
    gains: Vec<f64>,
}

#[derive(Deserialize)]
struct FavouriteArgs {
    file: String,
    on: bool,
}

#[derive(Deserialize)]
struct RenameArgs {
    file: String,
    name: String,
    game: Option<String>,
}

#[tauri::command(async)]
fn state(core: tauri::State<Shared>) -> Value {
    let settings = core.settings.lock().unwrap().clone();
    let mut media = list_media(&core);
    let storage_used: u64 = media.iter().map(|m| m.size).sum();
    media.truncate(MAX_LISTED_MEDIA);
    json!({
        "estimateMb": estimate_buffer_mb(settings.buffer_seconds, settings.bitrate_kbps),
        "encoderInUse": core.engine.encoder_in_use(),
        "settings": settings,
        "status": core.engine.status(),
        "capture": core.capture_state(),
        "hotkeyFailures": *core.hotkey_failures.lock().unwrap(),
        "controllers": core.controller.get().map(|c| c.seen()).unwrap_or_default(),
        "media": media,
        "storageUsedBytes": storage_used,
        "activeProfile": *core.active_profile.lock().unwrap(),
        "activeBufferSeconds": core.engine.current_settings().buffer_seconds,
        "update": core.update.lock().unwrap().clone(),
        "version": core.app.package_info().version.to_string(),
    })
}

#[tauri::command(async)]
fn estimate(payload: EstimateArgs) -> i64 {
    estimate_buffer_mb(payload.buffer_seconds, payload.bitrate_kbps)
}

#[tauri::command(async)]
fn clip(core: tauri::State<Shared>, payload: i64) -> Result<ClipResult, String> {
    core.clip(payload)
}

#[tauri::command(async)]
fn toggle_record(core: tauri::State<Shared>) -> Result<Value, String> {
    core.toggle_record().map(|recording| json!({ "recording": recording }))
}

#[tauri::command(async)]
fn toggle_mic(core: tauri::State<Shared>) -> Result<bool, String> {
    core.toggle_mic()
}

#[tauri::command(async)]
fn toggle_clean_mic(core: tauri::State<Shared>) -> Result<bool, String> {
    core.toggle_clean_mic()
}

#[tauri::command(async)]
fn add_marker(core: tauri::State<Shared>) -> Result<u64, String> {
    core.add_marker()
}

#[tauri::command(async)]
fn screenshot(core: tauri::State<Shared>) -> Result<String, String> {
    core.screenshot().map(|file| file.to_string_lossy().into_owned())
}

#[derive(Deserialize)]
struct VolumeArgs {
    index: i64,
    volume: f64,
}

#[tauri::command(async)]
fn set_volume(core: tauri::State<Shared>, payload: VolumeArgs) -> Result<(), String> {
    core.engine.set_volume(payload.index, payload.volume.clamp(0.0, 2.0))
}

#[tauri::command(async)]
fn set_meters(core: tauri::State<Shared>, payload: bool) -> Result<(), String> {
    core.engine.set_meters(payload)
}

#[tauri::command(async)]
fn levels(core: tauri::State<Shared>) -> Result<Value, String> {
    core.engine.levels()
}

#[tauri::command(async)]
fn stats(core: tauri::State<Shared>) -> Result<Value, String> {
    core.engine.stats()
}

#[tauri::command(async)]
fn open(core: tauri::State<Shared>, payload: String) -> Result<(), String> {
    let path = own_media(&core, &payload)?;
    if shell_execute("open", path.as_os_str(), None, None) { Ok(()) } else { Err("Could not open the file".into()) }
}

#[tauri::command(async)]
fn reveal(core: tauri::State<Shared>, payload: String) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    let path = own_media(&core, &payload)?;
    // Explorer needs /select, joined to the path, with quoting Rust's default escaping would break.
    Command::new("explorer").raw_arg(format!("/select,\"{}\"", path.display())).spawn().map_err(|e| e.to_string())?;
    Ok(())
}

/// Hands a clip to Windows to drag into another app. The panel stays open during the drag, because moving
/// to another app takes its focus, then closes if the drop left that app focused.
#[tauri::command(async)]
fn start_drag(core: tauri::State<Shared>, payload: String) -> Result<(), String> {
    let core: Shared = Arc::clone(&core);
    let path = own_media(&core, &payload)?;
    let was_held = core.keep_panel_open.swap(true, Ordering::SeqCst);
    let (done, finished) = std::sync::mpsc::channel();
    let result = core
        .app
        .run_on_main_thread(move || { let _ = done.send(win32::drag_file(&path)); })
        .map_err(|e| e.to_string())
        .and_then(|_| finished.recv().map_err(|e| e.to_string())?);
    core.keep_panel_open.store(was_held, Ordering::SeqCst);
    let focused = core.app.get_webview_window(PANEL).is_some_and(|w| w.is_focused().unwrap_or(false));
    if !was_held && !focused {
        on_panel_blur(&core);
    }
    result
}

#[tauri::command(async)]
fn copy_file(core: tauri::State<Shared>, payload: String) -> Result<(), String> {
    win32::copy_file(&own_media(&core, &payload)?)
}

#[tauri::command(async)]
fn trash(core: tauri::State<Shared>, payload: String) -> Result<(), String> {
    let path = own_media(&core, &payload)?;
    remove_media(&core, &path)
}

#[tauri::command(async)]
fn set_favourite(core: tauri::State<Shared>, payload: FavouriteArgs) -> Result<(), String> {
    let path = own_media(&core, &payload.file)?;
    let name = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
    let mut library = core.library.lock().unwrap();
    if payload.on {
        library.favourites.insert(name);
    } else {
        library.favourites.remove(&name);
    }
    library.save(&core.library_file)
}

#[tauri::command(async)]
fn rename_clip(core: tauri::State<Shared>, payload: RenameArgs) -> Result<String, String> {
    let path = own_media(&core, &payload.file)?;
    let stem = clean_file_stem(&payload.name)?;
    let extension = path.extension().unwrap_or_default().to_string_lossy().into_owned();
    let target = path.with_file_name(format!("{stem}.{extension}"));
    let old_name = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
    let new_name = target.file_name().unwrap_or_default().to_string_lossy().into_owned();
    if old_name == new_name {
        return Ok(payload.file);
    }
    // Windows names ignore case, so only a real different file counts as taken.
    if target.exists() && !old_name.eq_ignore_ascii_case(&new_name) {
        return Err(format!("You already have a clip called {stem}"));
    }
    fs::rename(&path, &target).map_err(|e| format!("Could not rename this clip: {e}"))?;
    forget_thumbnails(&core, &old_name);
    let mut library = core.library.lock().unwrap();
    library.rename(&old_name, &new_name, payload.game);
    library.save(&core.library_file)?;
    Ok(target.to_string_lossy().into_owned())
}

/// Makes a smaller copy to share. The original is never changed.
#[tauri::command(async)]
fn compress_clip(core: tauri::State<Shared>, payload: String) -> Result<String, String> {
    let core: Shared = Arc::clone(&core);
    let path = own_media(&core, &payload)?;
    // Reported as an event, because the panel may have closed by the time a long clip finishes.
    let toast = core.settings.lock().unwrap().toast_enabled;
    match core.engine.compress(&path) {
        Ok(output) => {
            let bytes = fs::metadata(&output).map(|m| m.len()).unwrap_or(0);
            let megabytes = (bytes as f64 / 1_000_000.0 * 10.0).ceil() / 10.0;
            if toast {
                win32::show_toast(&format!("{megabytes} MB copy ready"));
            }
            let _ = core.app.emit("feedback", json!({ "kind": "compressed", "ok": true, "file": output, "megabytes": megabytes, "overTarget": bytes > engine::COMPRESS_TARGET_BYTES }));
            enforce_storage_rules(&core);
            Ok(output.to_string_lossy().into_owned())
        }
        Err(message) => {
            let _ = core.app.emit("feedback", json!({ "kind": "compressed", "ok": false, "message": message }));
            Err(message)
        }
    }
}

#[tauri::command(async)]
fn clip_tracks(core: tauri::State<Shared>, payload: String) -> Result<i64, String> {
    core.engine.track_count(&own_media(&core, &payload)?)
}

/// Makes a 9:16 copy for phones. The original is never changed.
#[tauri::command(async)]
fn vertical_clip(core: tauri::State<Shared>, payload: String) -> Result<String, String> {
    let core: Shared = Arc::clone(&core);
    let path = own_media(&core, &payload)?;
    let toast = core.settings.lock().unwrap().toast_enabled;
    match core.engine.make_vertical(&path) {
        Ok(output) => {
            if toast {
                win32::show_toast("Vertical copy ready");
            }
            let _ = core.app.emit("feedback", json!({ "kind": "vertical", "ok": true, "file": output }));
            enforce_storage_rules(&core);
            Ok(output.to_string_lossy().into_owned())
        }
        Err(message) => {
            let _ = core.app.emit("feedback", json!({ "kind": "vertical", "ok": false, "message": message }));
            Err(message)
        }
    }
}

/// Sends a file as it is if Discord will take it. Otherwise it sends a smaller copy, which isn't kept.
fn upload_to_discord(core: &Core, webhook: &str, path: &Path) -> Result<(), String> {
    if !discord::is_webhook(webhook, api::is_test_instance()) {
        return Err("Add your Discord webhook in Settings, under Clips and sharing.".into());
    }
    let size = fs::metadata(path).map_err(|e| format!("Couldn't read this file: {e}"))?.len();
    if size <= discord::MAX_UPLOAD_BYTES {
        return discord::send(webhook, path);
    }
    if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("png")) {
        return Err("This screenshot is too big for Discord.".into());
    }
    let copy = core.engine.shrink(path, discord::MAX_UPLOAD_BYTES / 1024 / 1024).map_err(|_| "This is too long to fit Discord's 10 MB limit. Trim it first.".to_string())?;
    let sent = discord::send(webhook, &copy);
    let _ = fs::remove_file(&copy);
    sent
}

/// Reported as an event, because the panel may have closed by the time a long clip finishes.
#[tauri::command(async)]
fn send_to_discord(core: tauri::State<Shared>, payload: String) -> Result<(), String> {
    let path = own_media(&core, &payload)?;
    let (webhook, toast) = {
        let settings = core.settings.lock().unwrap();
        (settings.discord_webhook.clone(), settings.toast_enabled)
    };
    let sent = upload_to_discord(&core, &webhook, &path);
    match &sent {
        Ok(()) => {
            if toast {
                win32::show_toast("Sent to Discord");
            }
            let _ = core.app.emit("feedback", json!({ "kind": "discord", "ok": true }));
        }
        Err(message) => { let _ = core.app.emit("feedback", json!({ "kind": "discord", "ok": false, "message": message })); }
    }
    sent
}

#[tauri::command(async)]
fn check_update(core: tauri::State<Shared>) {
    let core: Shared = Arc::clone(&core);
    thread::spawn(move || { updater::check(&core, true); });
}

#[tauri::command(async)]
fn install_update(core: tauri::State<Shared>) -> Result<(), String> {
    updater::install(&core)
}

#[tauri::command(async)]
fn skip_update(core: tauri::State<Shared>) -> Result<(), String> {
    updater::skip(&core)
}

/// Returns a cached thumbnail and duration. They're generated one at a time, only for cards on screen.
#[tauri::command(async)]
fn thumbnail(core: tauri::State<Shared>, payload: String) -> Option<Value> {
    let path = own_media(&core, &payload).ok()?;
    let _one_at_a_time = core.thumbnail_lock.lock().unwrap();
    let mtime = modified_ms(&fs::metadata(&path).ok()?) as u64;
    let name = path.file_name()?.to_string_lossy().into_owned();
    let image = core.engine.thumbs_dir().join(format!("{name}-{mtime}.jpg"));
    let length = core.engine.thumbs_dir().join(format!("{name}-{mtime}.txt"));
    if !image.exists() {
        fs::create_dir_all(core.engine.thumbs_dir()).ok()?;
        core.engine.make_thumbnail(&path, &image).ok()?;
    }
    let duration = match fs::read_to_string(&length).ok().and_then(|t| t.trim().parse::<f64>().ok()) {
        Some(seconds) => seconds,
        // A picture has no length to show.
        None if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("png")) => 0.0,
        None => {
            let seconds = core.engine.duration(&path).ok()?;
            let _ = fs::write(&length, seconds.to_string());
            seconds
        }
    };
    let bytes = fs::read(image).ok()?;
    Some(json!({
        "url": format!("data:image/jpeg;base64,{}", base64::engine::general_purpose::STANDARD.encode(bytes)),
        "duration": duration,
    }))
}

#[tauri::command(async)]
fn pick_folder(core: tauri::State<Shared>) -> Option<String> {
    let start = core.settings.lock().unwrap().output_dir.clone();
    core.keep_panel_open.store(true, Ordering::SeqCst);
    let picked = core.app.dialog().file().set_directory(start).blocking_pick_folder().map(|p| p.to_string());
    core.keep_panel_open.store(false, Ordering::SeqCst);
    picked
}

#[tauri::command(async)]
fn running_apps() -> Vec<win32::RunningApp> {
    win32::running_apps()
}

/// For a game that isn't open right now. Only the program's name is kept, which is all a profile looks for.
#[tauri::command(async)]
fn pick_game(core: tauri::State<Shared>) -> Option<String> {
    core.keep_panel_open.store(true, Ordering::SeqCst);
    let picked = core.app.dialog().file().add_filter("Games", &["exe"]).blocking_pick_file();
    core.keep_panel_open.store(false, Ordering::SeqCst);
    let path = picked?.into_path().ok()?;
    path.file_name().map(|n| n.to_string_lossy().into_owned())
}

/// The primary monitor's real mode, which the quality presets are worked out from.
#[tauri::command(async)]
fn display_info() -> Value {
    let (width, height, refresh) = win32::primary_display_mode();
    json!({ "width": width, "height": height, "refresh": refresh })
}

#[tauri::command(async)]
fn audio_choices(core: tauri::State<Shared>) -> Result<Value, String> {
    core.engine.audio_choices()
}

#[tauri::command(async)]
fn save_settings(core: tauri::State<Shared>, payload: Settings) -> Result<Value, String> {
    let core: Shared = Arc::clone(&core);
    let mut clean = payload.resolved_for(win32::primary_display_mode()).sanitized();
    let current = core.settings.lock().unwrap().clone();
    // The form can hold an older value, because the mic key changes this at any time.
    clean.mic_muted = current.mic_muted;
    clean.clean_mic = current.clean_mic;
    let effective = core.effective(&clean);
    let restart = core.engine.current_settings().needs_engine_restart(&effective);
    if restart && core.engine.status().recording {
        return Err("Stop recording before changing these settings".into());
    }
    // Tests must not touch the real sign-in list.
    if clean.launch_on_startup != current.launch_on_startup && !api::is_test_instance() {
        win32::set_launch_on_startup(clean.launch_on_startup)?;
    }
    // Unelevated, it can wait: an off setting never uses the task, and the next elevated start tidies it.
    if clean.run_as_admin != current.run_as_admin && win32::is_self_elevated() {
        sync_elevated_task(&core.ui_log, clean.run_as_admin);
    }
    clean.save(&core.settings_file).map_err(|e| format!("Could not save settings: {e}"))?;
    let clean_size = (clean.width, clean.height, clean.fps);
    *core.settings.lock().unwrap() = clean;
    register_hotkeys(&core);
    if restart {
        let again = Arc::clone(&core);
        thread::spawn(move || {
            // Settings now save as they change, so restarts can queue up. Each one uses the newest settings.
            let _one_at_a_time = again.restart_lock.lock().unwrap();
            let newest = again.effective(&again.settings.lock().unwrap().clone());
            again.engine.stop();
            let _ = again.engine.start(Some(newest));
            again.allow_media_folders();
        });
    } else {
        // Settings that apply without a restart, such as how long recordings split.
        core.engine.use_settings(effective);
    }
    {
        let again = Arc::clone(&core);
        thread::spawn(move || enforce_storage_rules(&again));
    }
    let failures = core.hotkey_failures.lock().unwrap().clone();
    Ok(json!({ "restarted": restart, "hotkeyFailures": failures, "width": clean_size.0, "height": clean_size.1, "fps": clean_size.2 }))
}

/// The panel's X button: back to the tray, or a full exit if that setting is off.
#[tauri::command]
fn close_panel(core: tauri::State<Shared>) {
    if core.settings.lock().unwrap().close_to_tray {
        if let Some(window) = core.app.get_webview_window(PANEL) {
            let _ = window.hide();
        }
    } else {
        core.app.exit(0);
    }
}

/// A failure only means the next start asks through UAC again, so it's logged rather than shown.
fn sync_elevated_task(ui_log: &Path, wanted: bool) {
    if let Err(e) = elevated_task::sync(wanted) {
        if let Ok(mut file) = fs::OpenOptions::new().create(true).append(true).open(ui_log) {
            let _ = writeln!(file, "{} could not update the administrator task: {e}", chrono::Local::now().format("%Y-%m-%d %H:%M:%S"));
        }
    }
}

/// Relaunches elevated, through the task or UAC. The buffer can't survive this, so it only ever runs from the user's click.
/// The engine and API let go first, and come back if UAC is declined.
#[tauri::command(async)]
fn restart_admin(core: tauri::State<Shared>) -> Result<(), String> {
    let core: Shared = Arc::clone(&core);
    if core.engine.status().recording {
        return Err("Stop recording before restarting as administrator".into());
    }
    core.engine.stop();
    if let Some(server) = core.api.lock().unwrap().take() {
        server.unblock();
    }
    // Without this the elevated copy would see this one and quit as a second instance.
    tauri_plugin_single_instance::destroy(&core.app);

    core.keep_panel_open.store(true, Ordering::SeqCst);
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let use_task = core.settings.lock().unwrap().run_as_admin;
    let accepted = (use_task && elevated_task::start()) || shell_execute("runas", exe.as_os_str(), None, std::env::current_dir().ok().as_deref());
    core.keep_panel_open.store(false, Ordering::SeqCst);
    if accepted {
        core.quitting.store(true, Ordering::SeqCst);
        core.app.exit(0);
        return Ok(());
    }
    start_services(&core);
    let engine = Arc::clone(&core.engine);
    thread::spawn(move || { let _ = engine.start(None); });
    Err("Administrator permission was declined".into())
}

/// A window calls this once it has rendered. Windows stay hidden until then, so nobody sees a blank page.
#[tauri::command]
fn window_ready(window: WebviewWindow, core: tauri::State<Shared>) {
    core.ready_windows.lock().unwrap().push(window.label().to_string());
    if window.label() == PANEL {
        position_panel(&core, &window);
    }
    let _ = window.show();
    let _ = window.set_focus();
}

// Must stay async. Creating a window inside a synchronous command deadlocks WebView2 on Windows, freezing the app.
#[tauri::command(async)]
fn open_editor(core: tauri::State<Shared>, payload: Vec<String>) {
    show_editor(&core, payload);
}

/// The clips the editor was opened with. Each batch is only handed over once.
#[tauri::command]
fn editor_files(core: tauri::State<Shared>) -> Vec<String> {
    std::mem::take(&mut *core.pending_editor_files.lock().unwrap())
}

#[tauri::command(async)]
fn export_edit(core: tauri::State<Shared>, payload: ExportArgs) -> Result<String, String> {
    let core: Shared = Arc::clone(&core);
    if payload.segments.is_empty() {
        return Err("Add a clip first".into());
    }
    let mut segments = Vec::with_capacity(payload.segments.len());
    for segment in &payload.segments {
        segments.push((own_media(&core, &segment.file)?, segment.start.max(0.0), segment.end));
    }
    let file = core.engine.export_segments(&segments, &payload.gains)?;
    let _ = core.app.emit("feedback", json!({ "kind": "exported", "ok": true, "file": file }));
    enforce_storage_rules(&core);
    Ok(file.to_string_lossy().into_owned())
}

#[tauri::command]
fn quit_app(app: AppHandle) {
    app.exit(0);
}

fn start_services(core: &Shared) {
    let handlers: Arc<dyn ApiHandlers> = Arc::new(Api(Arc::clone(core)));
    match start_api(load_token(&core.token_file), handlers) {
        Ok(server) => *core.api.lock().unwrap() = Some(server),
        Err(message) => core.log_ui(&format!("Stream Deck API could not start: {message}")),
    }
}

fn load_token(file: &Path) -> String {
    if let Ok(token) = fs::read_to_string(file) {
        return token.trim().to_string();
    }
    let mut bytes = [0u8; 24];
    getrandom::fill(&mut bytes).expect("the system random source failed");
    let token: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    if let Some(dir) = file.parent() {
        let _ = fs::create_dir_all(dir);
    }
    let _ = fs::write(file, &token);
    token
}

/// Reloads a window that never reported rendering, then shows it regardless, and logs both for diagnosis.
fn watch_startup(core: &Shared, label: &'static str) {
    core.ready_windows.lock().unwrap().retain(|l| l != label);
    let core = Arc::clone(core);
    thread::spawn(move || {
        let is_ready = |core: &Core| core.ready_windows.lock().unwrap().iter().any(|l| l == label);
        thread::sleep(READY_RELOAD_AFTER);
        if is_ready(&core) {
            return;
        }
        core.log_ui(&format!("{label} had not rendered after {READY_RELOAD_AFTER:?}, reloading"));
        if let Some(window) = core.app.get_webview_window(label) {
            let _ = window.eval("window.location.reload()");
        }
        thread::sleep(READY_SHOW_AFTER - READY_RELOAD_AFTER);
        if is_ready(&core) {
            core.log_ui(&format!("{label} rendered after reloading"));
            return;
        }
        core.log_ui(&format!("{label} still had not rendered after {READY_SHOW_AFTER:?}, showing it anyway"));
        if let Some(window) = core.app.get_webview_window(label) {
            let _ = window.show();
        }
    });
}

fn local_app_data() -> PathBuf {
    std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir)
}

/// Where everything except clips and recordings lives.
fn home_dir() -> PathBuf {
    std::env::var_os("AFTERCLIP_HOME").filter(|_| api::is_test_instance()).map(PathBuf::from).unwrap_or_else(|| local_app_data().join("Afterclip"))
}

/// Earlier versions kept settings in Roaming and the windows' data in a folder of its own.
fn move_old_data(home: &Path, identifier: &str) {
    if let Some(roaming) = std::env::var_os("APPDATA").map(PathBuf::from) {
        data_move::move_files(&roaming.join("Afterclip"), home, &["settings.json", "library.json", "api-token.txt"]);
    }
    let old_windows = local_app_data().join(identifier);
    thread::spawn(move || {
        let _ = fs::remove_dir_all(old_windows.join("EBWebView"));
        let _ = fs::remove_dir(old_windows);
    });
}

fn build_window<'a>(app: &'a AppHandle, label: &str, page: &str) -> WebviewWindowBuilder<'a, tauri::Wry, AppHandle> {
    let mut builder = WebviewWindowBuilder::new(app, label, WebviewUrl::App(page.into()))
        .title("Afterclip")
        // Hidden until the page reports it has rendered, on a dark background rather than white.
        .visible(false)
        .background_color(BACKGROUND)
        .data_directory(home_dir().join("webview"));
    if api::is_test_instance() {
        if let Ok(port) = std::env::var("AFTERCLIP_DEBUG_PORT") {
            builder = builder.additional_browser_args(&format!("--remote-debugging-port={port}"));
        }
    }
    builder
}

/// Docks the panel flush into the corner of the usable area beside the tray, touching the taskbar.
fn position_panel(core: &Core, window: &WebviewWindow) {
    let anchor = core.tray_anchor.lock().unwrap().or_else(|| {
        let tray = core.app.tray_by_id("afterclip")?;
        let rect = tray.rect().ok()??;
        let position = rect.position.to_physical::<f64>(1.0);
        let size = rect.size.to_physical::<f64>(1.0);
        Some(((position.x + size.width / 2.0) as i32, (position.y + size.height / 2.0) as i32))
    });
    let (ax, ay) = anchor.unwrap_or_else(|| {
        let (w, h) = win32::primary_monitor_size();
        (w as i32, h as i32)
    });
    let (left, top, right, bottom) = win32::work_area_at(ax, ay);
    let Ok(size) = window.outer_size() else { return };
    let (w, h) = (size.width as i32, size.height as i32);
    // Works for a taskbar on any edge: the corner nearest the tray icon.
    let x = if ax > (left + right) / 2 { right - w } else { left };
    let y = if ay > (top + bottom) / 2 { bottom - h } else { top };
    let _ = window.set_position(PhysicalPosition::new(x, y));
}

/// Overlays like RivaTuner can crash the browser behind a window. The window stays, showing nothing, and
/// would never paint again. Removing it means the next open builds a new one. A visible panel comes straight
/// back, but not repeatedly, so something that crashes every browser can't start a loop.
fn recover_from_browser_crash(core: &Shared, window: &WebviewWindow) {
    let label = window.label().to_string();
    let core = Arc::clone(core);
    let _ = window.with_webview(move |webview| unsafe {
        let Ok(browser) = webview.controller().CoreWebView2() else { return };
        let handler = ProcessFailedEventHandler::create(Box::new(move |_, args| {
            let mut kind = COREWEBVIEW2_PROCESS_FAILED_KIND::default();
            if let Some(args) = args {
                args.ProcessFailedKind(&mut kind)?;
            }
            if [COREWEBVIEW2_PROCESS_FAILED_KIND_BROWSER_PROCESS_EXITED, COREWEBVIEW2_PROCESS_FAILED_KIND_RENDER_PROCESS_EXITED].contains(&kind) {
                let core = Arc::clone(&core);
                let label = label.clone();
                // Never from inside the browser's own event.
                thread::spawn(move || {
                    let Some(window) = core.app.get_webview_window(&label) else { return };
                    core.log_ui(&format!("{label}'s browser stopped, so its window will be rebuilt"));
                    let reopen = label == PANEL && window.is_visible().unwrap_or(false);
                    let _ = window.destroy();
                    let mut last = core.panel_recovered_at.lock().unwrap();
                    if reopen && last.is_none_or(|at| at.elapsed() > PANEL_RECOVER_GAP) {
                        *last = Some(Instant::now());
                        drop(last);
                        show_panel(&core);
                    }
                });
            }
            Ok(())
        }));
        let mut token = Default::default();
        let _ = browser.add_ProcessFailed(&handler, &mut token);
    });
}

/// The panel is the app's only front door. It's opened from the tray and closes when it loses focus.
fn show_panel(core: &Shared) {
    if let Some(window) = core.app.get_webview_window(PANEL) {
        position_panel(core, &window);
        let _ = window.show();
        let _ = window.set_focus();
        let _ = window.emit("panel-shown", ());
        return;
    }
    let built = build_window(&core.app, PANEL, "index.html")
        .inner_size(420.0, 700.0)
        .resizable(false)
        .decorations(false)
        .skip_taskbar(true)
        .always_on_top(true)
        // A shadow adds an invisible frame around the window, which would leave a gap from the taskbar.
        .shadow(false)
        // See-through outside the page's rounded corners.
        .transparent(true)
        .background_color(Color(0, 0, 0, 0))
        .build();
    match built {
        Ok(window) => recover_from_browser_crash(core, &window),
        Err(e) => {
            core.log_ui(&format!("panel could not open: {e}"));
            return;
        }
    }
    watch_startup(core, PANEL);
}

fn toggle_panel(core: &Shared) {
    let visible = core.app.get_webview_window(PANEL).is_some_and(|w| w.is_visible().unwrap_or(false));
    if visible {
        if let Some(window) = core.app.get_webview_window(PANEL) {
            let _ = window.hide();
        }
        return;
    }
    // Clicking the tray takes focus from the panel first, which already hid it. This click means close.
    let just_hidden = core.panel_hidden_at.lock().unwrap().is_some_and(|at| at.elapsed() < Duration::from_millis(300));
    if !just_hidden {
        show_panel(core);
    }
}

fn show_editor(core: &Shared, files: Vec<String>) {
    core.pending_editor_files.lock().unwrap().extend(files);
    if let Some(window) = core.app.get_webview_window(EDITOR) {
        let _ = window.emit("editor-files", ());
        let _ = window.show();
        let _ = window.set_focus();
        return;
    }
    let built = build_window(&core.app, EDITOR, "editor.html")
        .title("Afterclip Editor")
        .inner_size(1120.0, 740.0)
        .min_inner_size(860.0, 600.0)
        .build();
    match built {
        Ok(window) => recover_from_browser_crash(core, &window),
        Err(e) => {
            core.log_ui(&format!("editor could not open: {e}"));
            return;
        }
    }
    watch_startup(core, EDITOR);
}

fn on_panel_blur(core: &Shared) {
    if core.keep_panel_open.load(Ordering::SeqCst) {
        return;
    }
    let Some(window) = core.app.get_webview_window(PANEL) else { return };
    let _ = window.hide();
    *core.panel_hidden_at.lock().unwrap() = Some(Instant::now());
    let hide = core.panel_hide_count.fetch_add(1, Ordering::SeqCst) + 1;
    let core = Arc::clone(core);
    thread::spawn(move || {
        thread::sleep(PANEL_KEEP_ALIVE);
        let still_hidden = core.panel_hide_count.load(Ordering::SeqCst) == hide;
        if let Some(window) = core.app.get_webview_window(PANEL) {
            if still_hidden && !window.is_visible().unwrap_or(true) {
                let _ = window.destroy();
            }
        }
    });
}

fn strip_verbatim(path: PathBuf) -> PathBuf {
    match path.to_str().and_then(|p| p.strip_prefix(r"\\?\")) {
        Some(plain) => PathBuf::from(plain),
        None => path,
    }
}

/// Installed builds carry the engine in resources. Running from source uses vendor/engine.
fn engine_root(app: &AppHandle) -> PathBuf {
    if let Ok(resources) = app.path().resource_dir() {
        let bundled = strip_verbatim(resources).join("engine");
        if bundled.join("bin").exists() {
            return bundled;
        }
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join("vendor").join("engine")
}

/// The drawing made for the display's scale, because Windows blurs an icon it has to resize.
fn tray_icon() -> Image<'static> {
    match win32::system_dpi() {
        0..=108 => tauri::include_image!("icons/tray-16.png"),
        109..=132 => tauri::include_image!("icons/tray-20.png"),
        133..=168 => tauri::include_image!("icons/tray-24.png"),
        _ => tauri::include_image!("icons/tray-32.png"),
    }
}

fn create_tray(app: &AppHandle, core: Shared) -> tauri::Result<()> {
    let menu = MenuBuilder::new(app)
        .text("show", "Open Afterclip")
        .text("editor", "Open editor")
        .text("clip", "Clip whole buffer")
        .text("record", "Start / stop recording")
        .separator()
        .text("quit", "Quit")
        .build()?;
    let menu_core = Arc::clone(&core);
    TrayIconBuilder::with_id("afterclip")
        .icon(tray_icon())
        .tooltip("Afterclip")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(move |app, event| match event.id().as_ref() {
            "show" => {
                let core = Arc::clone(&menu_core);
                thread::spawn(move || show_panel(&core));
            }
            "editor" => {
                let core = Arc::clone(&menu_core);
                thread::spawn(move || show_editor(&core, Vec::new()));
            }
            "clip" => {
                let core = Arc::clone(&menu_core);
                thread::spawn(move || { let _ = core.clip(0); });
            }
            "record" => {
                let core = Arc::clone(&menu_core);
                thread::spawn(move || { let _ = core.toggle_record(); });
            }
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(move |_, event| {
            if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, rect, .. } = event {
                let position = rect.position.to_physical::<f64>(1.0);
                let size = rect.size.to_physical::<f64>(1.0);
                *core.tray_anchor.lock().unwrap() = Some(((position.x + size.width / 2.0) as i32, (position.y + size.height / 2.0) as i32));
                toggle_panel(&core);
            }
        })
        .build(app)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Read-only: lists where a running process plays sound. Run with AFTERCLIP_TEST_PID set and --ignored --nocapture.
    #[test]
    #[ignore]
    fn finds_the_device_a_process_plays_through() {
        win32::init_com_for_thread();
        let pid: u32 = std::env::var("AFTERCLIP_TEST_PID").unwrap().parse().unwrap();
        let found = win32::active_endpoints_of(pid);
        println!("ENDPOINTS {found:?}");
        assert!(!found.is_empty());
    }
}

fn main() {
    if win32::is_self_elevated() {
        elevated_task::clear_unsafe_environment();
    }
    let mut builder = tauri::Builder::default();
    // A test copy stays separate, so running the tests can't hand over to someone's real running app.
    if !api::is_test_instance() {
        builder = builder.plugin(tauri_plugin_single_instance::init(|app, args, _| {
            if let Some(core) = app.try_state::<Shared>() {
                let core: Shared = Arc::clone(&core);
                // The second copy is waiting on this message, and windows must open off this thread.
                let editor = args.iter().any(|a| a == "--editor");
                thread::spawn(move || {
                    if editor {
                        show_editor(&core, Vec::new());
                    } else {
                        show_panel(&core);
                    }
                });
            }
        }));
    }
    builder
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![
            state, estimate, clip, toggle_record, toggle_mic, toggle_clean_mic, add_marker, screenshot, set_volume, set_meters, levels, stats, close_panel, open, reveal, start_drag, copy_file, trash, set_favourite, rename_clip, compress_clip, vertical_clip, send_to_discord, clip_tracks, check_update, install_update, skip_update, thumbnail, pick_folder, running_apps, pick_game, audio_choices, display_info,
            save_settings, restart_admin, window_ready, open_editor, editor_files, export_edit, quit_app
        ])
        .on_window_event(|window, event| {
            if window.label() == PANEL {
                if let WindowEvent::Focused(false) = event {
                    if let Some(core) = window.app_handle().try_state::<Shared>() {
                        let core: Shared = Arc::clone(&core);
                        on_panel_blur(&core);
                    }
                }
            }
        })
        .setup(|app| {
            let handle = app.handle().clone();
            let home = home_dir();
            // The Stream Deck plugin reads its token from here too. Test copies start with their settings in a folder of their own.
            let user_data = if api::is_test_instance() { home.join("userData") } else { home.clone() };
            if !api::is_test_instance() {
                move_old_data(&home, &app.config().identifier);
            }
            let settings_file = user_data.join("settings.json");

            let (width, height) = win32::primary_monitor_size();
            let videos = app.path().video_dir().unwrap_or_else(|_| home.clone());
            // Native settings are worked out again at every start, in case the monitor changed.
            let settings = Settings::load(&settings_file, Settings::defaults(&videos, width, height))
                .resolved_for(win32::primary_display_mode());
            let open_at_start = !settings.start_minimized;
            let shared_settings = Arc::new(Mutex::new(settings.clone()));

            // The startup entry follows the setting and this copy's location, because uninstalling an
            // older version removes it, and a move would leave it pointing at nothing.
            if settings.launch_on_startup && !api::is_test_instance() {
                let _ = win32::set_launch_on_startup(true);
            }

            // The task starts the elevated copy with no prompt. Without one it's UAC, and if that's
            // declined, carry on unelevated. Game capture will say if it needs more.
            if settings.run_as_admin && !api::is_test_instance() && !win32::is_self_elevated() {
                // Without this the elevated copy would see this one and quit as a second instance.
                tauri_plugin_single_instance::destroy(&handle);
                let exe = std::env::current_exe()?;
                if elevated_task::start() || shell_execute("runas", exe.as_os_str(), None, std::env::current_dir().ok().as_deref()) {
                    std::process::exit(0);
                }
            }
            if win32::is_self_elevated() {
                win32::accept_second_instance_message(&app.config().identifier);
                let _ = fs::create_dir_all(&home);
                sync_elevated_task(&home.join("ui.log"), settings.run_as_admin);
            }

            let events = handle.clone();
            let engine = Engine::new(home.clone(), engine_root(&handle), settings, Box::new(move |event| {
                match event {
                    EngineEvent::Status(status) => { let _ = events.emit("status", status); }
                    EngineEvent::RecordingSaved(file) => {
                        let _ = events.emit("feedback", json!({ "kind": "recording-saved", "ok": true, "file": file }));
                    }
                    EngineEvent::Warning(message) => {
                        let _ = events.emit("feedback", json!({ "kind": "warning", "ok": false, "message": message }));
                    }
                }
            }));

            let _ = fs::create_dir_all(&home);
            let core: Shared = Arc::new(Core {
                app: handle.clone(),
                engine: Arc::clone(&engine),
                capture: OnceLock::new(),
                settings: Arc::clone(&shared_settings),
                settings_file,
                token_file: user_data.join("api-token.txt"),
                ui_log: home.join("ui.log"),
                api: Mutex::new(None),
                hotkey_failures: Mutex::new(Vec::new()),
                thumbnail_lock: Mutex::new(()),
                restart_lock: Mutex::new(()),
                quitting: AtomicBool::new(false),
                keep_panel_open: AtomicBool::new(false),
                panel_hidden_at: Mutex::new(None),
                panel_recovered_at: Mutex::new(None),
                panel_hide_count: AtomicU64::new(0),
                tray_anchor: Mutex::new(None),
                pending_editor_files: Mutex::new(Vec::new()),
                ready_windows: Mutex::new(Vec::new()),
                library: Mutex::new(LibraryData::load(&user_data.join("library.json"))),
                library_file: user_data.join("library.json"),
                active_profile: Mutex::new(None),
                test_game: Mutex::new(None),
                controller: OnceLock::new(),
                update: Mutex::new(updater::UpdateState { status: "idle".into(), ..Default::default() }),
                update_ready: Mutex::new(None),
                update_busy: AtomicBool::new(false),
                storage_lock: Mutex::new(()),
            });
            let capture_events = handle.clone();
            let detection_settings = Arc::clone(&shared_settings);
            let profile_settings = Arc::clone(&shared_settings);
            let manager = CaptureManager::start(
                Arc::clone(&engine),
                move || !detection_settings.lock().unwrap().test_pattern,
                move || profile_settings.lock().unwrap().game_profiles.iter().map(|p| p.exe.clone()).collect(),
                move |state| { let _ = capture_events.emit("capture", state); },
            );
            let _ = core.capture.set(manager);
            app.manage(Arc::clone(&core));

            create_tray(&handle, Arc::clone(&core))?;
            register_hotkeys(&core);
            let controller_settings = Arc::clone(&shared_settings);
            let controller_core = Arc::clone(&core);
            let controller_events = handle.clone();
            let listener = controller::Controller::start(
                move || {
                    let s = controller_settings.lock().unwrap();
                    s.controller_enabled.then(|| controller::Bindings {
                        clip: controller::button(&s.controller_clip),
                        record: controller::button(&s.controller_record),
                        mic: controller::button(&s.controller_mic),
                        marker: controller::button(&s.controller_marker),
                        screenshot: controller::button(&s.controller_screenshot),
                    })
                },
                move |action| {
                    let core = Arc::clone(&controller_core);
                    thread::spawn(move || match action {
                        controller::Action::Clip => {
                            let seconds = core.settings.lock().unwrap().controller_clip_seconds;
                            let _ = core.clip(seconds);
                        }
                        controller::Action::Record => { let _ = core.toggle_record(); }
                        controller::Action::Mic => { let _ = core.toggle_mic(); }
                        controller::Action::Marker => { let _ = core.add_marker(); }
                        controller::Action::Screenshot => { let _ = core.screenshot(); }
                    });
                },
                move |seen| { let _ = controller_events.emit("controllers", seen); },
            );
            let _ = core.controller.set(listener);
            start_services(&core);
            updater::start(Arc::clone(&core));
            let watcher = Arc::clone(&core);
            thread::spawn(move || watch_games(watcher));
            core.allow_media_folders();
            // A failure reaches the UI through status. Normally no window opens: the app lives in the tray.
            thread::spawn(move || { let _ = engine.start(None); });
            if open_at_start {
                let opening = Arc::clone(&core);
                thread::spawn(move || show_panel(&opening));
            }
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("Afterclip could not start")
        .run(|app, event| match event {
            // With no code, this is the last window closing: keep buffering in the tray.
            RunEvent::ExitRequested { code: None, api, .. } => api.prevent_exit(),
            RunEvent::Exit => {
                if let Some(core) = app.try_state::<Shared>() {
                    if !core.quitting.swap(true, Ordering::SeqCst) {
                        // Waits for the engine, so a running recording is finalized before the process ends.
                        core.engine.stop();
                    }
                }
            }
            _ => {}
        });
}
