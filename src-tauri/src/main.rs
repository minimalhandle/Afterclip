#![windows_subsystem = "windows"]

mod api;
mod capture;
mod elevated_task;
mod engine;
mod settings;
mod win32;

use api::{start_api, ApiHandlers};
use base64::Engine as _;
use capture::CaptureManager;
use engine::{ClipResult, Engine, EngineEvent};
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
    panel_hide_count: AtomicU64,
    tray_anchor: Mutex<Option<(i32, i32)>>,
    pending_editor_files: Mutex<Vec<String>>,
    ready_windows: Mutex<Vec<String>>,
}

type Shared = Arc<Core>;

impl Core {
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
                self.engine.use_settings(saved);
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
                self.engine.use_settings(saved);
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

    fn capture_state(&self) -> Value {
        self.capture.get().map(|c| json!(c.state())).unwrap_or(json!({ "mode": "desktop" }))
    }

    /// Lets the editor window load videos from the output folders, and nothing else.
    fn allow_media_folders(&self) {
        let scope = self.app.asset_protocol_scope();
        let _ = scope.allow_directory(self.engine.clips_dir(), false);
        let _ = scope.allow_directory(self.engine.recordings_dir(), false);
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
    fn test_open(&self, what: &str) {
        match what {
            "panel" => show_panel(&self.0),
            "editor" => show_editor(&self.0, Vec::new()),
            _ => {}
        }
    }
    /// Lets the UI tests run while someone uses the PC, because any click elsewhere closes the panel.
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
    *core.hotkey_failures.lock().unwrap() = failures;
}

#[derive(Serialize)]
struct Media {
    file: String,
    name: String,
    size: u64,
    mtime: f64,
    kind: &'static str,
}

fn modified_ms(meta: &fs::Metadata) -> f64 {
    meta.modified().ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as f64)
        .unwrap_or(0.0)
}

fn list_media(core: &Core) -> Vec<Media> {
    let active = core.engine.active_recording();
    let mut media = Vec::new();
    for (dir, kind) in [(core.engine.clips_dir(), "clip"), (core.engine.recordings_dir(), "recording")] {
        let Ok(entries) = fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            let is_video = path.extension().and_then(|e| e.to_str())
                .is_some_and(|e| ["mp4", "mkv", "mov"].contains(&e.to_lowercase().as_str()));
            if !is_video || active.as_deref() == Some(path.as_path()) {
                continue;
            }
            let Ok(meta) = entry.metadata() else { continue };
            media.push(Media {
                file: path.to_string_lossy().into_owned(),
                name: entry.file_name().to_string_lossy().into_owned(),
                size: meta.len(),
                mtime: modified_ms(&meta),
                kind,
            });
        }
    }
    media.sort_by(|a, b| b.mtime.total_cmp(&a.mtime));
    media.truncate(200);
    media
}

/// The UI only gets to act on files inside our own output folders.
fn own_media(core: &Core, file: &str) -> Result<PathBuf, String> {
    let path = PathBuf::from(file);
    let traversal = path.components().any(|c| matches!(c, Component::ParentDir));
    let parent = path.parent().map(Path::to_path_buf);
    if traversal || (parent != Some(core.engine.clips_dir()) && parent != Some(core.engine.recordings_dir())) {
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

#[tauri::command(async)]
fn state(core: tauri::State<Shared>) -> Value {
    let settings = core.settings.lock().unwrap().clone();
    json!({
        "estimateMb": estimate_buffer_mb(settings.buffer_seconds, settings.bitrate_kbps),
        "encoderInUse": core.engine.encoder_in_use(),
        "settings": settings,
        "status": core.engine.status(),
        "capture": core.capture_state(),
        "hotkeyFailures": *core.hotkey_failures.lock().unwrap(),
        "media": list_media(&core),
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

#[tauri::command(async)]
fn trash(core: tauri::State<Shared>, payload: String) -> Result<(), String> {
    let path = own_media(&core, &payload)?;
    trash::delete(&path).map_err(|e| e.to_string())?;
    let prefix = format!("{}-", path.file_name().unwrap_or_default().to_string_lossy());
    if let Ok(entries) = fs::read_dir(core.engine.thumbs_dir()) {
        for entry in entries.flatten() {
            if entry.file_name().to_string_lossy().starts_with(&prefix) {
                let _ = fs::remove_file(entry.path());
            }
        }
    }
    Ok(())
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
    let restart = current.needs_engine_restart(&clean);
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
            let newest = again.settings.lock().unwrap().clone();
            again.engine.stop();
            let _ = again.engine.start(Some(newest));
            again.allow_media_folders();
        });
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
fn export_edit(core: tauri::State<Shared>, payload: Vec<ExportSegment>) -> Result<String, String> {
    if payload.is_empty() {
        return Err("Add a clip first".into());
    }
    let mut segments = Vec::with_capacity(payload.len());
    for segment in &payload {
        segments.push((own_media(&core, &segment.file)?, segment.start.max(0.0), segment.end));
    }
    let file = core.engine.export_segments(&segments)?;
    let _ = core.app.emit("feedback", json!({ "kind": "exported", "ok": true, "file": file }));
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

fn build_window<'a>(app: &'a AppHandle, label: &str, page: &str) -> WebviewWindowBuilder<'a, tauri::Wry, AppHandle> {
    let mut builder = WebviewWindowBuilder::new(app, label, WebviewUrl::App(page.into()))
        .title("Afterclip")
        // Hidden until the page reports it has rendered, on a dark background rather than white.
        .visible(false)
        .background_color(BACKGROUND);
    if api::is_test_instance() {
        if let Some(home) = std::env::var_os("AFTERCLIP_HOME") {
            builder = builder.data_directory(PathBuf::from(home).join("webview"));
        }
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
        .inner_size(420.0, 660.0)
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
    if let Err(e) = built {
        core.log_ui(&format!("panel could not open: {e}"));
        return;
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
    if let Err(e) = built {
        core.log_ui(&format!("editor could not open: {e}"));
        return;
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
                if args.iter().any(|a| a == "--editor") {
                    show_editor(&core, Vec::new());
                } else {
                    show_panel(&core);
                }
            }
        }));
    }
    builder
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![
            state, estimate, clip, toggle_record, toggle_mic, toggle_clean_mic, add_marker, set_volume, set_meters, levels, stats, close_panel, open, reveal, trash, thumbnail, pick_folder, audio_choices, display_info,
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
            let test_home = std::env::var_os("AFTERCLIP_HOME").filter(|_| api::is_test_instance()).map(PathBuf::from);
            let local = std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
            let roaming = std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
            let home = test_home.clone().unwrap_or_else(|| local.join("Afterclip"));
            // The Stream Deck plugin reads its token from here too.
            let user_data = test_home.map(|h| h.join("userData")).unwrap_or_else(|| roaming.join("Afterclip"));
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
                panel_hide_count: AtomicU64::new(0),
                tray_anchor: Mutex::new(None),
                pending_editor_files: Mutex::new(Vec::new()),
                ready_windows: Mutex::new(Vec::new()),
            });
            let capture_events = handle.clone();
            let detection_settings = Arc::clone(&shared_settings);
            let manager = CaptureManager::start(
                Arc::clone(&engine),
                move || !detection_settings.lock().unwrap().test_pattern,
                move |state| { let _ = capture_events.emit("capture", state); },
            );
            let _ = core.capture.set(manager);
            app.manage(Arc::clone(&core));

            create_tray(&handle, Arc::clone(&core))?;
            register_hotkeys(&core);
            start_services(&core);
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
