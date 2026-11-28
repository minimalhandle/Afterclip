#![windows_subsystem = "windows"]

mod api;
mod advanced;
mod audio_check;
mod capture;
mod controller;
mod data_move;
mod deck;
mod diagnostics;
mod double_press;
mod discord;
mod elevated_task;
mod engine;
mod library;
mod loud;
mod overlaps;
mod previews;
mod reel;
mod render;
mod settings;
mod studio;
mod updater;
mod upload;
mod usage;
mod win32;

// Parts of the app itself, split out of this file. Their items are used here as if they were still in it.
mod app_windows;
mod media;
mod watchers;
pub(crate) use app_windows::*;
pub(crate) use media::*;
pub(crate) use watchers::*;

use api::{start_api, ApiHandlers};
use capture::{CaptureManager, Watch};
use engine::{ClipResult, Engine, EngineEvent, ShareAudio};
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
const SHARE: &str = "share";
const BACKGROUND: Color = Color(14, 12, 31, 255);
// The panel keeps its window this long after hiding, so reopening is instant, and then frees the memory.
const PANEL_KEEP_ALIVE: Duration = Duration::from_secs(20);
// A window that hasn't said it rendered by now gets reloaded, then shown regardless.
const READY_RELOAD_AFTER: Duration = Duration::from_secs(4);
const READY_SHOW_AFTER: Duration = Duration::from_secs(8);
// A recording stops below this much free space, while the file can still be finished properly.
const STOP_RECORDING_BELOW: u64 = 2_000_000_000;
const PUSH_TO_TALK_POLL: Duration = Duration::from_millis(30);
const PUSH_TO_TALK_IDLE: Duration = Duration::from_millis(500);
const WAITING_FOR_GAME: &str = "Afterclip only buffers while a game is open. Start a game, or turn this off in Settings, under Video.";

struct Core {
    app: AppHandle,
    engine: Arc<Engine>,
    capture: OnceLock<Arc<CaptureManager>>,
    settings: Arc<Mutex<Settings>>,
    settings_file: PathBuf,
    token_file: PathBuf,
    ui_log: PathBuf,
    api: Mutex<Option<Arc<tiny_http::Server>>>,
    /// The API for a Stream Deck on another PC, while that's on.
    remote_api: Mutex<Option<Arc<tiny_http::Server>>>,
    /// The Stream Deck key, which every request must carry.
    api_key: Arc<std::sync::RwLock<String>>,
    /// Why the API couldn't start, such as another app holding its port.
    api_error: Mutex<Option<String>>,
    hotkey_failures: Mutex<Vec<String>>,
    /// Thumbnails being made, a few at a time.
    thumbnails: previews::Gate,
    restart_lock: Mutex<()>,
    quitting: AtomicBool,
    /// Set while a dialog is open, because the panel losing focus to it must not close it.
    keep_panel_open: AtomicBool,
    panel_hidden_at: Mutex<Option<Instant>>,
    /// When the panel was rebuilt after its browser crashed, lately.
    panel_recoveries: Mutex<Vec<Instant>>,
    panel_hide_count: AtomicU64,
    tray_anchor: Mutex<Option<(i32, i32)>>,
    pending_editor_files: Mutex<Vec<String>>,
    share_file: Mutex<Option<ShareSource>>,
    angle_file: Mutex<Option<PathBuf>>,
    analysis_lock: Mutex<()>,
    studio: studio::Studio,
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
    /// Scrub strips and loudness being made, a few at a time.
    strips: previews::Gate,
    /// The engine is stopped on purpose until a game opens.
    paused: AtomicBool,
    /// The push-to-talk key is held.
    talking: AtomicBool,
    test_talk: Mutex<Option<bool>>,
    test_free_bytes: Mutex<Option<u64>>,
    /// The Stream Deck's Hold to Talk key is down.
    deck_talking: AtomicBool,
    /// Files lately moved to the Recycle Bin, with what the library knew about them, so Undo can put both back.
    deleted: Mutex<Vec<DeletedMedia>>,
    /// The last game session that saved clips, until it's dismissed.
    last_session: Mutex<Option<GameSession>>,
    /// The last press of a clip key, which a quick second press extends.
    last_clip_press: Mutex<Option<Arc<double_press::Press>>>,
}

/// A file moved to the Recycle Bin, and its library details.
struct DeletedMedia {
    path: PathBuf,
    favourite: bool,
    game: Option<String>,
    tags: Option<Vec<String>>,
    camera: Option<[f64; 4]>,
}

// Undo is only offered for a moment, so only the latest few deletions need remembering.
const MAX_REMEMBERED_DELETES: usize = 200;

/// A game that was played, from when it was first seen to when it closed, and how many clips were saved meanwhile.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct GameSession {
    start_ms: f64,
    end_ms: f64,
    clips: usize,
}

fn now_ms() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0.0, |d| d.as_millis() as f64)
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
        let mut effective = match active.and_then(|exe| base.game_profiles.iter().find(|p| p.exe == exe).cloned()) {
            Some(profile) => base.with_profile(&profile, win32::primary_display_mode()),
            None => base.clone(),
        };
        effective.mic_muted = self.mic_silenced(base);
        effective.with_card()
    }

    /// The mute key wins. With push-to-talk on, the mic is only heard while its key is held.
    fn mic_silenced(&self, settings: &Settings) -> bool {
        settings.mic_muted || (settings.push_to_talk && !self.talking.load(Ordering::SeqCst))
    }

    /// Stops capture until a game opens, because the buffer only runs during games.
    fn pause_engine(&self) {
        let _one_at_a_time = self.restart_lock.lock().unwrap();
        if self.paused.swap(true, Ordering::SeqCst) {
            return;
        }
        self.engine.stop();
        self.engine.set_waiting_for_game(true);
        self.log_ui("no game is open, so the buffer stopped until one does");
    }

    /// Starts capture again after a pause. Does nothing if it wasn't paused.
    fn wake_engine(&self) -> Result<(), String> {
        let _one_at_a_time = self.restart_lock.lock().unwrap();
        if !self.paused.swap(false, Ordering::SeqCst) {
            return Ok(());
        }
        let settings = self.effective(&self.settings.lock().unwrap().clone());
        let started = self.engine.start(Some(settings));
        self.allow_media_folders();
        started
    }

    /// Where the webcam sits in what's being captured, as fractions, or None without one.
    fn camera_place(&self) -> Option<[f64; 4]> {
        let s = self.engine.current_settings();
        if !s.camera_enabled {
            return None;
        }
        let (x, y, width, height) = s.camera_box()?;
        let (width_px, height_px) = s.canvas();
        let (w, h) = (width_px.max(1) as f64, height_px.max(1) as f64);
        Some([x as f64 / w, y as f64 / h, width as f64 / w, height as f64 / h])
    }

    /// Remembers where a file's webcam is, so a vertical copy can find it.
    fn remember_camera(&self, file: &Path, place: Option<[f64; 4]>) {
        let Some(place) = place else { return };
        let name = file.file_name().unwrap_or_default().to_string_lossy().into_owned();
        let mut library = self.library.lock().unwrap();
        library.cameras.insert(name, place);
        let _ = library.save(&self.library_file);
    }

    /// Plays the error tone and tells the panel, then hands the message back.
    fn fail(&self, kind: &str, message: String) -> String {
        self.tone(&[220.0, 220.0]);
        let _ = self.app.emit("feedback", json!({ "kind": kind, "ok": false, "message": message }));
        message
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

    /// Saves a clip of the last `seconds`. A second press soon after a first saves one longer clip in place of
    /// both, when that's on.
    fn clip(&self, seconds: i64) -> Result<ClipResult, String> {
        if !self.settings.lock().unwrap().extend_on_double_press {
            return self.save_clip(seconds, false);
        }
        let now = Instant::now();
        let (earlier, press) = {
            let mut last = self.last_clip_press.lock().unwrap();
            let earlier = last.take().and_then(|e| double_press::extended_seconds(&e, now, seconds).map(|s| (e, s)));
            let press = Arc::new(double_press::Press::new(now, earlier.as_ref().map_or(seconds, |(_, s)| *s)));
            *last = Some(Arc::clone(&press));
            (earlier.map(|(e, _)| e), press)
        };
        let result = self.save_clip(press.seconds, false);
        press.finish(result.as_ref().ok().map(|r| PathBuf::from(&r.file)));
        let result = result?;
        if let Some(earlier) = earlier {
            self.replace_earlier_clip(&earlier, &result);
        }
        Ok(result)
    }

    /// Moves the clip an earlier press saved to the Recycle Bin, now a longer one covers it, giving the longer
    /// one its star and tags.
    fn replace_earlier_clip(&self, earlier: &double_press::Press, longer: &ClipResult) {
        let Some(old) = earlier.wait(double_press::WAIT_FOR_EARLIER) else { return };
        let new = PathBuf::from(&longer.file);
        if same_path(&old, &new) {
            return;
        }
        let name = |p: &Path| p.file_name().unwrap_or_default().to_string_lossy().into_owned();
        {
            let mut library = self.library.lock().unwrap();
            library.carry_over(&name(&old), &name(&new));
            let _ = library.save(&self.library_file);
        }
        match remove_media(self, &old) {
            Ok(()) => {
                self.log_ui(&format!("a quick second press extended {} into {}", name(&old), name(&new)));
                let _ = self.app.emit("feedback", json!({ "kind": "clip-extended", "ok": true, "file": longer.file, "replaced": old }));
            }
            Err(e) => self.log_ui(&format!("a quick second press couldn't remove the shorter clip {}: {e}", old.display())),
        }
    }

    fn save_clip(&self, seconds: i64, star: bool) -> Result<ClipResult, String> {
        self.save_clip_as(seconds, star, None, "Clip saved")
    }

    /// Saves a clip, optionally tagged, and confirms it with `note`.
    fn save_clip_as(&self, seconds: i64, star: bool, tag: Option<&str>, note: &str) -> Result<ClipResult, String> {
        if self.paused.load(Ordering::SeqCst) {
            return Err(self.fail("clip", WAITING_FOR_GAME.into()));
        }
        // Capture carries on for a moment first, so what happens just after the press is in the clip too.
        let after = self.settings.lock().unwrap().clip_after_seconds;
        let seconds = if after > 0 && self.engine.status().buffering {
            self.tone(&[660.0]);
            let _ = self.app.emit("feedback", json!({ "kind": "clip-waiting", "ok": true, "seconds": after }));
            thread::sleep(Duration::from_secs(after as u64));
            if seconds > 0 { seconds + after } else { 0 }
        } else {
            seconds
        };
        let camera = self.camera_place();
        // A concurrent clean-up must see the star before it can consider the new file.
        let storage_guard = star.then(|| self.storage_lock.lock().unwrap());
        match self.engine.clip(seconds) {
            Ok(result) => {
                self.remember_camera(Path::new(&result.file), camera);
                if star || tag.is_some() {
                    let name = Path::new(&result.file).file_name().unwrap_or_default().to_string_lossy().into_owned();
                    let mut library = self.library.lock().unwrap();
                    if star {
                        library.favourites.insert(name.clone());
                    }
                    if let Some(tag) = tag {
                        let mut tags = library.tags.get(&name).cloned().unwrap_or_default();
                        tags.push(tag.to_string());
                        library.set_tags(&name, &tags);
                    }
                    library.save(&self.library_file).map_err(|message| self.fail("favourite", message))?;
                }
                drop(storage_guard);
                self.tone(&[880.0, 1320.0]);
                if self.settings.lock().unwrap().toast_enabled {
                    let total = result.actual_seconds.round() as i64;
                    win32::show_toast(&format!("{note} · {}:{:02}", total / 60, total % 60));
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

    /// Saves a clip and stars it at once, so clean-up never removes it.
    fn clip_and_star(&self, seconds: i64) -> Result<ClipResult, String> {
        let result = self.save_clip(seconds, true)?;
        let _ = self.app.emit("feedback", json!({ "kind": "starred-clip", "ok": true, "file": result.file }));
        Ok(result)
    }

    fn toggle_record(&self) -> Result<bool, String> {
        // Recording works without a game, so it wakes a paused buffer.
        if let Err(message) = self.wake_engine() {
            return Err(self.fail("record", message));
        }
        match self.engine.toggle_record() {
            Ok(recording) => {
                if let Some(file) = self.engine.recording_file().filter(|_| recording) {
                    self.remember_camera(&file, self.camera_place());
                }
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
        let push_to_talk = self.settings.lock().unwrap().push_to_talk;
        match self.engine.set_mic_muted(muted || (push_to_talk && !self.talking.load(Ordering::SeqCst))) {
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
        self.add_named_marker(None)
    }

    /// A named marker key: a chapter with that name during a recording, or else a clip tagged with it, so the
    /// moment is easy to find either way.
    fn mark_moment(&self, label: &str) -> Result<(), String> {
        if self.engine.status().recording {
            return self.add_named_marker(Some(label)).map(|_| ());
        }
        let seconds = self.settings.lock().unwrap().marker_clip_seconds;
        self.save_clip_as(seconds, false, Some(label), &format!("#{label} saved")).map(|_| ())
    }

    fn add_named_marker(&self, label: Option<&str>) -> Result<u64, String> {
        match self.engine.add_marker(label) {
            Ok(number) => {
                self.tone(&[990.0]);
                if self.settings.lock().unwrap().toast_enabled {
                    win32::show_toast(&match label {
                        Some(label) => format!("Marker added: {label}"),
                        None => format!("Marker {number} added"),
                    });
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
        if self.paused.load(Ordering::SeqCst) {
            return Err(self.fail("screenshot", WAITING_FOR_GAME.into()));
        }
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

    /// The audio for a copy to share: these tracks, evened out if that's on.
    fn share_audio(&self, tracks: &[usize]) -> ShareAudio {
        let mut tracks = tracks.to_vec();
        tracks.sort_unstable();
        tracks.dedup();
        // Only the main mix is what a copy has anyway.
        if tracks == [0] {
            tracks.clear();
        }
        ShareAudio { tracks, even: self.settings.lock().unwrap().even_audio }
    }

    /// Where the webcam sat in a clip, if it had one.
    fn camera_of(&self, path: &Path) -> Option<[f64; 4]> {
        let name = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
        self.library.lock().unwrap().cameras.get(&name).copied()
    }

    /// Makes a smaller copy to share. The original is never changed. It's reported as an event too, because
    /// the panel may have closed by the time a long clip finishes. `named` is the clip the copy is named after,
    /// which is the original when `path` is a trimmed part of it.
    fn compress(&self, path: &Path, named: &Path, audio: &ShareAudio) -> Result<PathBuf, String> {
        let toast = self.settings.lock().unwrap().toast_enabled;
        match self.engine.compress_as(path, named, audio) {
            Ok(output) => {
                self.remember_camera(&output, self.camera_of(named));
                let bytes = fs::metadata(&output).map(|m| m.len()).unwrap_or(0);
                let megabytes = (bytes as f64 / 1_000_000.0 * 10.0).ceil() / 10.0;
                if toast {
                    win32::show_toast(&format!("{megabytes} MB copy ready"));
                }
                let _ = self.app.emit("feedback", json!({ "kind": "compressed", "ok": true, "file": output, "megabytes": megabytes, "overTarget": bytes > engine::COMPRESS_TARGET_BYTES }));
                if let Some(core) = self.app.try_state::<Shared>() {
                    enforce_storage_rules(&core);
                }
                Ok(output)
            }
            Err(message) => {
                let _ = self.app.emit("feedback", json!({ "kind": "compressed", "ok": false, "message": message }));
                Err(message)
            }
        }
    }

    /// The first webhook with an address, which the Stream Deck key and the plain Send to Discord use.
    fn first_webhook(&self) -> usize {
        self.settings.lock().unwrap().discord_webhooks.iter().position(|h| !h.url.is_empty()).unwrap_or(0)
    }

    /// Posts to one of the webhooks, by its place in the list.
    fn send_to_discord(&self, path: &Path, webhook: usize, audio: &ShareAudio) -> Result<(), String> {
        let (hook, toast) = {
            let settings = self.settings.lock().unwrap();
            (settings.discord_webhooks.get(webhook).cloned(), settings.toast_enabled)
        };
        let sent = match &hook {
            Some(hook) => upload_to_discord(self, &hook.url, path, audio),
            None => Err(NO_WEBHOOK.into()),
        };
        match &sent {
            Ok(()) => {
                let name = hook.map(|h| h.name).unwrap_or_default();
                if toast {
                    win32::show_toast(&format!("Sent to {name}"));
                }
                let _ = self.app.emit("feedback", json!({ "kind": "discord", "ok": true, "name": name }));
            }
            Err(message) => { let _ = self.app.emit("feedback", json!({ "kind": "discord", "ok": false, "message": message })); }
        }
        sent
    }

    /// Uploads a file to the server set in Settings, and returns the link it answers with.
    fn upload(&self, path: &Path) -> Result<String, String> {
        let (address, token, field, toast) = {
            let s = self.settings.lock().unwrap();
            (s.upload_url.clone(), s.upload_token.clone(), s.upload_field.clone(), s.toast_enabled)
        };
        let sent = if upload::is_address(&address, api::is_test_instance()) {
            upload::send(&address, &token, &field, path)
        } else {
            Err(NO_UPLOAD_SERVER.into())
        };
        match &sent {
            Ok(link) => {
                if toast {
                    win32::show_toast("Uploaded");
                }
                let _ = self.app.emit("feedback", json!({ "kind": "uploaded", "ok": true, "link": link }));
            }
            Err(message) => { let _ = self.app.emit("feedback", json!({ "kind": "uploaded", "ok": false, "message": message })); }
        }
        sent
    }

    /// Switches the webcam on or off, live, and remembers it. Returns whether it's now on.
    fn toggle_camera(&self) -> Result<bool, String> {
        let current = self.settings.lock().unwrap().clone();
        if current.camera_device.is_empty() {
            return Err(self.fail("camera", "Choose a webcam in Settings, under Video.".into()));
        }
        let on = !current.camera_enabled;
        // With capture stopped, waiting for a game, it starts with the new choice.
        if self.engine.status().buffering {
            self.engine.set_camera(on).map_err(|message| self.fail("camera", message))?;
        }
        let saved = {
            let mut settings = self.settings.lock().unwrap();
            settings.camera_enabled = on;
            settings.clone()
        };
        let _ = saved.save(&self.settings_file);
        self.engine.use_settings(self.effective(&saved));
        self.tone(if on { &[440.0, 660.0] } else { &[660.0, 440.0] });
        let _ = self.app.emit("feedback", json!({ "kind": "camera", "ok": true, "cameraOn": on }));
        Ok(on)
    }

    /// Stars or unstars a clip. Returns whether it's now a favourite.
    fn toggle_favourite(&self, path: &Path) -> Result<bool, String> {
        let name = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
        let mut library = self.library.lock().unwrap();
        let on = !library.favourites.contains(&name);
        if on {
            library.favourites.insert(name);
        } else {
            library.favourites.remove(&name);
        }
        library.save(&self.library_file)?;
        drop(library);
        let _ = self.app.emit("feedback", json!({ "kind": "favourite", "ok": true, "favourite": on }));
        Ok(on)
    }

    /// Makes a 9:16 copy for phones. The original is never changed. A clip with a webcam can have it above the game.
    /// `named` is the clip the copy is named after, as for compress.
    fn make_vertical(&self, path: &Path, named: &Path, audio: &ShareAudio) -> Result<PathBuf, String> {
        let (toast, face_cam) = {
            let settings = self.settings.lock().unwrap();
            (settings.toast_enabled, settings.vertical_face_cam)
        };
        let camera = self.camera_of(named).filter(|_| face_cam);
        match self.engine.make_vertical(path, named, camera, audio) {
            Ok(output) => {
                if toast {
                    win32::show_toast("Vertical copy ready");
                }
                let _ = self.app.emit("feedback", json!({ "kind": "vertical", "ok": true, "file": output }));
                if let Some(core) = self.app.try_state::<Shared>() {
                    enforce_storage_rules(&core);
                }
                Ok(output)
            }
            Err(message) => {
                let _ = self.app.emit("feedback", json!({ "kind": "vertical", "ok": false, "message": message }));
                Err(message)
            }
        }
    }

    /// Turns every mute-key source up or down. Returns the first one's new volume, in percent.
    fn change_mic_volume(&self, percent: i64) -> Result<i64, String> {
        let (saved, changed) = {
            let mut settings = self.settings.lock().unwrap();
            let mut changed = Vec::new();
            for (i, source) in settings.audio_sources.iter_mut().enumerate().filter(|(_, s)| s.muted_by_key()) {
                source.volume = ((source.volume * 100.0).round() as i64 + percent).clamp(0, 200) as f64 / 100.0;
                changed.push((i as i64, source.volume));
            }
            (settings.clone(), changed)
        };
        let Some(&(_, first)) = changed.first() else {
            return Err("No source is set to mute. Tick Mute key on your mic in Settings.".into());
        };
        let _ = saved.save(&self.settings_file);
        self.engine.use_settings(self.effective(&saved));
        for (index, volume) in changed {
            let _ = self.engine.set_volume(index, volume);
        }
        let volume = (first * 100.0).round() as i64;
        let _ = self.app.emit("feedback", json!({ "kind": "mic-volume", "ok": true, "volume": volume }));
        Ok(volume)
    }

    fn capture_state(&self) -> Value {
        self.capture.get().map(|c| json!(c.state())).unwrap_or(json!({ "mode": "desktop" }))
    }

    /// Lets the editor window load videos from the output folders, and nothing else.
    fn allow_media_folders(&self) {
        let scope = self.app.asset_protocol_scope();
        // With game folders, files can be one folder down.
        let _ = scope.allow_directory(self.engine.clips_dir(), true);
        let _ = scope.allow_directory(self.engine.recordings_dir(), true);
        let _ = scope.allow_directory(self.engine.screenshots_dir(), true);
    }
}

struct Api(Shared);

impl ApiHandlers for Api {
    fn recipe_last_clip(&self, name: &str) -> Result<Value, String> {
        let payload = ShareArgs {
            file: last_clip(&self.0)?, action: "recipe".into(), start: None, end: None,
            tracks: Vec::new(), webhook: 0, even: None, max_mb: None, layout: None, recipe: Some(name.into()),
        };
        run_share(&self.0, payload).map(|output| json!({ "file": output }))
    }
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
    fn compress_last_clip(&self) -> Result<Value, String> {
        let clip = PathBuf::from(last_clip(&self.0)?);
        self.0.compress(&clip, &clip, &self.0.share_audio(&[])).map(|file| json!({ "file": file }))
    }
    fn send_last_clip(&self) -> Result<Value, String> {
        let clip = last_clip(&self.0)?;
        self.0.send_to_discord(Path::new(&clip), self.0.first_webhook(), &self.0.share_audio(&[])).map(|_| json!({ "file": clip }))
    }
    fn change_mic_volume(&self, percent: i64) -> Result<Value, String> {
        self.0.change_mic_volume(percent).map(|volume| json!({ "micVolume": volume }))
    }
    fn test_free_space(&self, bytes: Option<u64>) {
        *self.0.test_free_bytes.lock().unwrap() = bytes;
    }
    fn vertical_last_clip(&self) -> Result<Value, String> {
        let clip = PathBuf::from(last_clip(&self.0)?);
        self.0.make_vertical(&clip, &clip, &self.0.share_audio(&[])).map(|file| json!({ "file": file }))
    }
    fn clip_and_star(&self, seconds: i64) -> Result<Value, String> {
        self.0.clip_and_star(seconds).map(|r| json!(r))
    }
    fn star_last_clip(&self) -> Result<Value, String> {
        let clip = last_clip(&self.0)?;
        self.0.toggle_favourite(Path::new(&clip)).map(|on| json!({ "file": clip, "favourite": on }))
    }
    fn toggle_camera(&self) -> Result<Value, String> {
        self.0.toggle_camera().map(|on| json!({ "cameraOn": on }))
    }
    fn toggle_card(&self) -> Result<Value, String> {
        toggle_card(&self.0).map(|on| json!({ "cardOn": on }))
    }
    fn deck_talk(&self, held: bool) -> Result<Value, String> {
        if !self.0.settings.lock().unwrap().push_to_talk {
            return Err("Turn on Push to talk in Settings, under Audio.".into());
        }
        self.0.deck_talking.store(held, Ordering::SeqCst);
        Ok(json!({ "talking": held }))
    }
    fn test_engine(&self, what: &str) {
        match what {
            "crash" => self.0.engine.test_crash(),
            "stop-replay" => { let _ = self.0.engine.test_stop_replay(); }
            "buffer-to-disk" => { let _ = self.0.engine.move_buffer_to_disk(); }
            _ => {}
        }
    }
    fn test_talk(&self, held: bool) {
        *self.0.test_talk.lock().unwrap() = Some(held);
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
        let exe = Some(exe.to_string()).filter(|e| !e.is_empty());
        // Files are named after it too, as they would be for a real game.
        self.0.engine.set_game(exe.as_deref().map(|e| e.trim_end_matches(".exe").to_string()));
        *self.0.test_game.lock().unwrap() = exe;
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
        status["pushToTalk"] = json!(settings.push_to_talk);
        status["talking"] = json!(self.0.talking.load(Ordering::SeqCst));
        status["micVolume"] = json!(settings.audio_sources.iter().find(|s| s.muted_by_key()).map(|s| (s.volume * 100.0).round() as i64));
        drop(settings);
        status["bufferSeconds"] = json!(self.0.engine.current_settings().buffer_seconds);
        let settings = self.0.settings.lock().unwrap();
        status["cameraOn"] = json!(settings.camera_enabled);
        status["cameraChosen"] = json!(!settings.camera_device.is_empty());
        status["cardOn"] = json!(settings.card().is_some());
        status["cardChosen"] = json!(!settings.capture_card_device.is_empty());
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
    let star_seconds = settings.star_clip_seconds;
    register(&settings.star_clip_hotkey, Box::new(move |core| {
        let core = Arc::clone(core);
        thread::spawn(move || { let _ = core.clip_and_star(star_seconds); });
    }));
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
    register(&settings.card_hotkey, Box::new(|core| {
        let core = Arc::clone(core);
        thread::spawn(move || { let _ = toggle_card(&core); });
    }));
    for key in settings.marker_keys.iter().filter(|k| !k.label.is_empty()) {
        let label = key.label.clone();
        register(&key.hotkey, Box::new(move |core| {
            let (core, label) = (Arc::clone(core), label.clone());
            thread::spawn(move || { let _ = core.mark_moment(&label); });
        }));
    }
    *core.hotkey_failures.lock().unwrap() = failures;
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
struct TagArgs {
    file: String,
    tags: Vec<String>,
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
        "patchNotes": updater::patch_notes(&core.app.package_info().version.to_string()),
        "streamDeck": stream_deck_json(&core, &settings),
        "remote": remote_json(&core),
        "lastSession": *core.last_session.lock().unwrap(),
    })
}

#[tauri::command(async)]
fn restore(core: tauri::State<Shared>, payload: Vec<String>) -> Result<usize, String> {
    // A fresh thread, for the same reason as recycle.
    let core: Shared = Arc::clone(&core);
    thread::spawn(move || restore_media(&core, &payload))
        .join()
        .unwrap_or_else(|_| Err("The Recycle Bin couldn't be reached.".into()))
}

#[tauri::command]
fn dismiss_session(core: tauri::State<Shared>) {
    *core.last_session.lock().unwrap() = None;
}

#[tauri::command(async)]
fn estimate(payload: EstimateArgs) -> i64 {
    estimate_buffer_mb(payload.buffer_seconds, payload.bitrate_kbps)
}

/// This PC's installed memory in MB, so Settings can warn about a buffer that takes too much of it.
#[tauri::command(async)]
fn installed_memory_mb() -> Option<u64> {
    win32::memory_status().map(|(_, total)| total / 1024 / 1024)
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

/// How loud Clip loud moments hears its source right now, so the level can be set by ear. Works outside a
/// game too, for setting it up.
#[tauri::command(async)]
fn loud_level(core: tauri::State<Shared>, payload: String) -> Option<f64> {
    let core: Shared = Arc::clone(&core);
    // On a thread of its own: this one is shared with every other command, and setting it up for COM here would
    // stop anything that needs COM set up differently, such as moving files to the Recycle Bin.
    thread::spawn(move || {
        win32::init_com_for_thread();
        let mut settings = core.settings.lock().unwrap().clone();
        settings.auto_clip_source = payload;
        let game = running_game(&core);
        let source = loud_source_for(&core, &settings, game.as_deref())?;
        let listener = loud::Listener::open(source)?;
        // A meter just opened can read zero, so it takes the loudest of a few quick readings.
        let mut loudest = listener.peak_db();
        for _ in 0..3 {
            thread::sleep(Duration::from_millis(30));
            loudest = loudest.max(listener.peak_db());
        }
        Some((loudest * 10.0).round() / 10.0)
    })
    .join()
    .ok()
    .flatten()
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
fn set_tags(core: tauri::State<Shared>, payload: TagArgs) -> Result<Vec<String>, String> {
    let path = own_media(&core, &payload.file)?;
    let name = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
    let mut library = core.library.lock().unwrap();
    library.set_tags(&name, &payload.tags);
    library.save(&core.library_file)?;
    Ok(library.tags.get(&name).cloned().unwrap_or_default())
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
    let _ = fs::rename(studio::bookmark_file(&path), studio::bookmark_file(&target));
    forget_thumbnails(&core, &old_name);
    let mut library = core.library.lock().unwrap();
    library.rename(&old_name, &new_name, payload.game);
    library.save(&core.library_file)?;
    Ok(target.to_string_lossy().into_owned())
}

#[tauri::command(async)]
fn clip_tracks(core: tauri::State<Shared>, payload: String) -> Result<i64, String> {
    core.engine.track_count(&own_media(&core, &payload)?)
}

const NO_UPLOAD_SERVER: &str = "Add your upload server's HTTPS address in Settings, under Clips and sharing.";
const NO_WEBHOOK: &str ="Add your Discord webhook in Settings, under Clips and sharing.";

/// Sends a file as it is if Discord will take it. Otherwise it sends a smaller copy, which isn't kept. Choosing
/// tracks other than the main mix always makes a copy, because Discord only plays a video's first track.
fn upload_to_discord(core: &Core, webhook: &str, path: &Path, audio: &ShareAudio) -> Result<(), String> {
    if !discord::is_webhook(webhook, api::is_test_instance()) {
        return Err(NO_WEBHOOK.into());
    }
    let size = fs::metadata(path).map_err(|e| format!("Couldn't read this file: {e}"))?.len();
    let picture = path.extension().is_some_and(|e| e.eq_ignore_ascii_case("png"));
    if size <= discord::MAX_UPLOAD_BYTES && (picture || (!audio.even && audio.tracks.is_empty())) {
        return discord::send(webhook, path);
    }
    if picture {
        return Err("This screenshot is too big for Discord.".into());
    }
    let copy = core.engine.shrink(path, discord::MAX_UPLOAD_BYTES / 1024 / 1024, audio).map_err(|_| "This is too long to fit Discord's 10 MB limit. Trim it first.".to_string())?;
    let sent = discord::send(webhook, &copy);
    let _ = fs::remove_file(&copy);
    sent
}

/// Reported as an event, because the panel may have closed by the time a long clip finishes. Posts to the first webhook.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ShareArgs {
    file: String,
    /// trim, compress, discord or vertical.
    action: String,
    /// The part to keep, in seconds. Both left out keeps the whole clip.
    #[serde(default)]
    start: Option<f64>,
    #[serde(default)]
    end: Option<f64>,
    /// Audio tracks for the copy, counted from 0, which is the main mix. Empty means the main mix.
    #[serde(default)]
    tracks: Vec<usize>,
    /// Which webhook, by its place in the list, for discord.
    #[serde(default)]
    webhook: usize,
    #[serde(default)]
    even: Option<bool>,
    #[serde(default)]
    max_mb: Option<u64>,
    #[serde(default)]
    layout: Option<advanced::Layout>,
    #[serde(default)]
    recipe: Option<String>,
}

/// Trims a clip, or makes a copy to share from part of it, from Edit and share. The original is never changed.
/// Results are reported as events too, like Compress, because a long job can outlast the panel.
#[tauri::command(async)]
fn share_clip(core: tauri::State<Shared>, payload: ShareArgs) -> Result<Option<String>, String> {
    run_share(&core, payload)
}

fn run_share(core: &Shared, mut payload: ShareArgs) -> Result<Option<String>, String> {
    let path = own_media(&core, &payload.file)?;
    if let Some(name) = &payload.recipe {
        let settings = core.settings.lock().unwrap();
        let recipe = settings.share_recipes.iter().find(|r| r.name == *name && r.valid()).ok_or("That share recipe is unavailable.")?;
        payload.action = recipe.action.clone();
        payload.tracks = recipe.tracks.clone();
        payload.even = Some(recipe.even);
        payload.max_mb = recipe.max_mb;
        payload.layout = recipe.layout.clone();
        if payload.action.ends_with("discord") {
            let mut matches = settings.discord_webhooks.iter().enumerate()
                .filter(|(_, h)| Some(&h.name) == recipe.channel.as_ref() && !h.url.is_empty());
            payload.webhook = matches.next().map(|(index, _)| index).ok_or("The recipe's Discord channel was removed or renamed.")?;
            if matches.next().is_some() {
                return Err("Give each Discord channel a unique name before using this recipe.".into());
            }
        }
    }
    if payload.layout.as_ref().is_some_and(|layout| !layout.valid()) || payload.max_mb.is_some_and(|mb| !(1..=250).contains(&mb)) {
        return Err("Invalid crop or file size target.".into());
    }
    if payload.action != "trim" && !payload.tracks.is_empty() {
        let count = core.engine.track_count(&path)? as usize;
        if payload.tracks.iter().any(|track| *track >= count) {
            return Err("A selected audio track is missing from this clip. Choose tracks that are available.".into());
        }
    }
    let mut audio = core.share_audio(&payload.tracks);
    if let Some(even) = payload.even { audio.even = even; }
    let range = match (payload.start, payload.end) {
        (None, None) => None,
        (start, end) => {
            let (start, end) = (start.unwrap_or(0.0).max(0.0), end.unwrap_or(-1.0));
            if end >= 0.0 && end - start < 0.5 {
                return Err("Keep at least half a second.".into());
            }
            Some((start, end))
        }
    };
    if payload.action == "trim" {
        let Some((start, end)) = range else { return Err("Choose the part to keep first.".into()) };
        let made = core.engine.trim_copy(&path, start, end, None).map_err(|message| core.fail("trimmed", message))?;
        core.remember_camera(&made, core.camera_of(&path));
        let _ = core.app.emit("feedback", json!({ "kind": "trimmed", "ok": true, "file": made }));
        enforce_storage_rules(&core);
        return Ok(Some(made.to_string_lossy().into_owned()));
    }
    // A copy of part of a clip is made from a lossless trim, which is thrown away afterwards.
    let part = match range {
        Some((start, end)) => {
            let scratch = core.engine.scratch_file("part.mp4").map_err(|message| core.fail(&payload.action, message))?;
            Some(core.engine.trim_copy(&path, start, end, Some(scratch)).map_err(|message| core.fail(&payload.action, message))?)
        }
        None => None,
    };
    let source = part.as_deref().unwrap_or(&path);
    let result = match payload.action.as_str() {
        "compress" if payload.max_mb.is_some() => core.engine.shrink_as(source, &path, payload.max_mb.unwrap(), &audio).map(|p| Some(p.to_string_lossy().into_owned())),
        "compress" => core.compress(source, &path, &audio).map(|p| Some(p.to_string_lossy().into_owned())),
        "vertical" if payload.layout.is_some() || payload.max_mb.is_some() => (|| {
            let vertical = core.engine.make_vertical_layout(source, &path, None, &audio, payload.layout.as_ref())?;
            let Some(mb) = payload.max_mb else { return Ok(Some(vertical.to_string_lossy().into_owned())) };
            let output = fit_vertical(core, &vertical, mb);
            output.map(|p| Some(p.to_string_lossy().into_owned()))
        })(),
        "vertical" => core.make_vertical(source, &path, &audio).map(|p| Some(p.to_string_lossy().into_owned())),
        "discord" if payload.max_mb.is_some() => send_sized(core, source, payload.webhook, payload.max_mb.unwrap(), &audio).map(|_| None),
        "discord" => core.send_to_discord(source, payload.webhook, &audio).map(|_| None),
        "vertical-discord" => (|| {
            let scratch = core.engine.scratch_file("share.mp4")?;
            let face_cam = core.settings.lock().unwrap().vertical_face_cam;
            let camera = core.camera_of(&path).filter(|_| face_cam);
            let vertical = match core.engine.make_vertical_layout(source, &scratch, camera, &audio, payload.layout.as_ref()) {
                Ok(file) => file,
                Err(error) if error == "This clip is already vertical." => {
                    return send_sized(core, source, payload.webhook, payload.max_mb.unwrap_or(10), &audio).map(|_| None);
                }
                Err(error) => return Err(error),
            };
            // The vertical pass has already applied the selected tracks and volume.
            let upload = send_sized(core, &vertical, payload.webhook, payload.max_mb.unwrap_or(10), &ShareAudio { tracks: vec![], even: false });
            let _ = fs::remove_file(vertical);
            upload.map(|_| None)
        })(),
        // Answers with the link rather than a file.
        "upload" => core.upload(source).map(Some),
        other => Err(format!("Unknown action {other}")),
    };
    if let Some(part) = part {
        let _ = fs::remove_file(part);
    }
    if matches!(&result, Ok(Some(_))) && (payload.layout.is_some() || payload.max_mb.is_some()) {
        enforce_storage_rules(core);
    }
    result
}

fn fit_vertical(core: &Core, vertical: &Path, mb: u64) -> Result<PathBuf, String> {
    if fs::metadata(vertical).is_ok_and(|meta| meta.len() <= mb * 1_000_000) {
        return Ok(vertical.to_path_buf());
    }
    let sized = core.engine.shrink(vertical, mb, &ShareAudio { tracks: vec![], even: false });
    let _ = fs::remove_file(vertical);
    sized
}

fn send_sized(core: &Core, source: &Path, webhook: usize, mb: u64, audio: &ShareAudio) -> Result<(), String> {
    if mb >= 10 {
        return core.send_to_discord(source, webhook, audio);
    }
    let named = core.engine.scratch_file("discord.mp4")?;
    let copy = core.engine.shrink_as(source, &named, mb, audio)?;
    let result = core.send_to_discord(&copy, webhook, &ShareAudio { tracks: vec![], even: false });
    let _ = fs::remove_file(copy);
    result
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

/// The Stream Deck plugin offer: "install" or "update" while it's unanswered, and what Settings can always do.
fn stream_deck_json(core: &Core, settings: &Settings) -> Value {
    // Tests run on PCs that may have a Stream Deck, and mustn't depend on it, so they say what to offer.
    let available = if api::is_test_instance() {
        match std::env::var("AFTERCLIP_TEST_STREAM_DECK").as_deref() {
            Ok("install") => Some("install"),
            Ok("update") => Some("update"),
            _ => None,
        }
    } else {
        deck::offer(&core.app)
    };
    let unanswered = available.filter(|_| settings.stream_deck_offer_seen != deck::bundled_version());
    json!({ "offer": unanswered, "available": available })
}

/// Hands the bundled plugin to the Stream Deck app, which shows its own install prompt.
#[tauri::command(async)]
fn install_stream_deck_plugin(core: tauri::State<Shared>) -> Result<(), String> {
    // A test copy never hands anything to a real Stream Deck app.
    if api::is_test_instance() {
        return remember_stream_deck_offer(&core);
    }
    let file = deck::install(&core.app)?;
    if !shell_execute("open", file.as_os_str(), None, None) {
        return Err("The Stream Deck app couldn't open the plugin. Open the Stream Deck app, then try again.".into());
    }
    remember_stream_deck_offer(&core)
}

/// Stops offering the plugin until a newer one ships. It can still be installed from Settings.
#[tauri::command(async)]
fn dismiss_stream_deck_plugin(core: tauri::State<Shared>) -> Result<(), String> {
    remember_stream_deck_offer(&core)
}

fn remember_stream_deck_offer(core: &Core) -> Result<(), String> {
    let saved = {
        let mut settings = core.settings.lock().unwrap();
        settings.stream_deck_offer_seen = deck::bundled_version();
        settings.clone()
    };
    saved.save(&core.settings_file).map_err(|e| e.to_string())
}

/// Hides this version's patch notes until the next update.
#[tauri::command(async)]
fn dismiss_patch_notes(core: tauri::State<Shared>) -> Result<(), String> {
    let saved = {
        let mut settings = core.settings.lock().unwrap();
        settings.seen_version = release_version(&core.app.package_info().version.to_string());
        settings.clone()
    };
    saved.save(&core.settings_file).map_err(|e| e.to_string())
}

/// Asks where to save a file, keeping the panel open behind the dialog. None if cancelled.
fn ask_save_path(core: &Core, name: String, kind: &str, extension: &str) -> Option<PathBuf> {
    core.keep_panel_open.store(true, Ordering::SeqCst);
    let picked = core.app.dialog().file().set_file_name(name).add_filter(kind, &[extension]).blocking_save_file();
    core.keep_panel_open.store(false, Ordering::SeqCst);
    picked.and_then(|p| p.into_path().ok())
}

/// Saves the settings to a file, to set up another PC or a reinstall. A test copy names the file itself.
#[tauri::command(async)]
fn export_settings(core: tauri::State<Shared>, payload: Option<String>) -> Result<Option<String>, String> {
    let target = match payload.filter(|_| api::is_test_instance()).map(PathBuf::from) {
        Some(path) => path,
        None => match ask_save_path(&core, "Afterclip settings.json".into(), "Settings", "json") {
            Some(path) => path,
            None => return Ok(None),
        },
    };
    let text = serde_json::to_string_pretty(&core.settings.lock().unwrap().exported()).map_err(|e| e.to_string())?;
    fs::write(&target, text).map_err(|e| format!("Couldn't save the settings: {e}"))?;
    Ok(Some(target.to_string_lossy().into_owned()))
}

/// Loads settings saved by Export settings. Returns false if cancelled.
#[tauri::command(async)]
fn import_settings(core: tauri::State<Shared>, payload: Option<String>) -> Result<bool, String> {
    let core: Shared = Arc::clone(&core);
    let file = match payload.filter(|_| api::is_test_instance()).map(PathBuf::from) {
        Some(path) => path,
        None => {
            core.keep_panel_open.store(true, Ordering::SeqCst);
            let picked = core.app.dialog().file().add_filter("Settings", &["json"]).blocking_pick_file();
            core.keep_panel_open.store(false, Ordering::SeqCst);
            match picked.and_then(|p| p.into_path().ok()) {
                Some(path) => path,
                None => return Ok(false),
            }
        }
    };
    let current = core.settings.lock().unwrap().clone();
    apply_settings(&core, current.imported(&file)?)?;
    Ok(true)
}

#[tauri::command(async)]
fn camera_choices(core: tauri::State<Shared>) -> Result<Value, String> {
    core.engine.camera_choices()
}

/// A small live copy of the capture card's picture for the panel, as RGBA pixels in base64. Empty until the
/// first copy is made, and while no card is being recorded.
#[tauri::command(async)]
fn card_preview(core: tauri::State<Shared>) -> Value {
    core.engine.card_preview().unwrap_or_else(|_| json!({}))
}

/// The sizes and highest frame rates a capture card sends, for greying out the rest in Settings. Seconds the
/// first time for each card.
#[tauri::command(async)]
fn card_modes(core: tauri::State<Shared>, payload: Option<String>) -> Result<Value, String> {
    core.engine.card_modes(&payload.unwrap_or_default())
}

#[tauri::command(async)]
fn card_choices(core: tauri::State<Shared>) -> Result<Value, String> {
    let mut choices = core.engine.card_choices()?;
    let settings = core.settings.lock().unwrap().clone();
    choices["doubled"] = json!(doubled_card_audio(&settings));
    Ok(choices)
}

#[tauri::command(async)]
fn pick_folder(core: tauri::State<Shared>) -> Option<String> {
    let start = core.settings.lock().unwrap().output_dir.clone();
    core.keep_panel_open.store(true, Ordering::SeqCst);
    let picked = core.app.dialog().file().set_directory(start).blocking_pick_folder().map(|p| p.to_string());
    core.keep_panel_open.store(false, Ordering::SeqCst);
    picked
}

/// Saves the logs and settings in one zip, to send with a problem report. A test copy names the file itself.
#[tauri::command(async)]
fn save_diagnostics(core: tauri::State<Shared>, payload: Option<String>) -> Result<Option<String>, String> {
    use std::os::windows::process::CommandExt;
    let chosen = payload.filter(|_| api::is_test_instance()).map(PathBuf::from);
    let target = match chosen.clone() {
        Some(path) => path,
        None => {
            let name = format!("Afterclip diagnostics {}.zip", chrono::Local::now().format("%Y-%m-%d %H-%M"));
            match ask_save_path(&core, name, "Zip file", "zip") {
                Some(path) => path,
                None => return Ok(None),
            }
        }
    };
    let home = core.ui_log.parent().map(Path::to_path_buf).unwrap_or_default();
    let (engine_log, previous_log) = (home.join("engine").join("engine.log"), home.join("engine").join("engine-previous.log"));
    let about = format!(
        "Afterclip {}\nSaved {}\nRunning as administrator: {}\n",
        core.app.package_info().version,
        chrono::Local::now().format("%Y-%m-%d %H:%M:%S"),
        if win32::is_self_elevated() { "yes" } else { "no" },
    );
    let profile = std::env::var("USERPROFILE").unwrap_or_default();
    let files = [("ui.log", core.ui_log.as_path()), ("engine.log", engine_log.as_path()), ("engine-previous.log", previous_log.as_path())];
    diagnostics::write(&target, &files, &core.settings_file, &about, &profile)?;
    if chosen.is_none() {
        let _ = Command::new("explorer").raw_arg(format!("/select,\"{}\"", target.display())).spawn();
    }
    Ok(Some(target.to_string_lossy().into_owned()))
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
    let mut choices = core.engine.audio_choices()?;
    audio_check::annotate_audio_choices(&mut choices);
    Ok(choices)
}

#[tauri::command(async)]
fn save_settings(core: tauri::State<Shared>, payload: Settings) -> Result<Value, String> {
    apply_settings(&core, payload)
}

/// Switches between recording this PC and the capture card, and remembers it. Capture restarts, which
/// empties the buffer. Returns whether the card is now recorded.
fn toggle_card(core: &Shared) -> Result<bool, String> {
    let current = core.settings.lock().unwrap().clone();
    if current.capture_card_device.is_empty() {
        return Err(core.fail("card", "Choose a capture card in Settings, under Video.".into()));
    }
    if core.engine.status().recording {
        return Err(core.fail("card", "Stop recording before switching to or from the capture card.".into()));
    }
    let on = !current.capture_card;
    let toast = current.toast_enabled;
    apply_settings(core, Settings { capture_card: on, ..current }).map_err(|message| core.fail("card", message))?;
    refresh_tray(core);
    core.tone(if on { &[440.0, 660.0] } else { &[660.0, 440.0] });
    if toast {
        win32::show_toast(if on { "Recording the capture card" } else { "Recording this PC" });
    }
    let _ = core.app.emit("feedback", json!({ "kind": "card", "ok": true, "cardOn": on }));
    Ok(on)
}

/// The name of a capture card device from its id, which is its name, a colon, then its path.
fn device_name(id: &str) -> &str {
    id.split(':').next().unwrap_or(id).trim()
}

/// A mic source that is really the capture card's audio, which would record the card's sound twice. A source
/// on Default counts when Windows' default recording device is the card.
fn doubled_card_audio(settings: &Settings) -> Option<String> {
    let (video, audio) = settings.card()?;
    if audio.is_empty() {
        return None;
    }
    let squash = |name: &str| name.to_lowercase().chars().filter(char::is_ascii_alphanumeric).collect::<String>();
    let (card, card_audio) = (squash(device_name(video)), squash(device_name(audio)));
    settings.audio_sources.iter().filter(|s| s.kind == "input").find_map(|source| {
        let name = if source.device.is_empty() || source.device == "default" { win32::default_input_name()? } else { source.label.clone() };
        let heard = squash(&name);
        (heard == card_audio || (!card.is_empty() && heard.contains(&card))).then(|| source.label.clone())
    })
}

/// Saves new settings and applies them, restarting capture only if something needs it.
fn apply_settings(core: &Shared, payload: Settings) -> Result<Value, String> {
    let core: Shared = Arc::clone(core);
    let mut clean = payload.resolved_for(win32::primary_display_mode()).sanitized();
    let current = core.settings.lock().unwrap().clone();
    clean.share_recipes = current.share_recipes.clone();
    clean.vertical_layouts = current.vertical_layouts.clone();
    // The form can hold an older value, because the mic key changes this at any time.
    clean.mic_muted = current.mic_muted;
    clean.clean_mic = current.clean_mic;
    clean.seen_version = current.seen_version.clone();
    clean.stream_deck_offer_seen = current.stream_deck_offer_seen.clone();
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
    let remote_changed = clean.remote_port() != current.remote_port();
    let usage_changed = clean.usage_ping != current.usage_ping;
    if clean.run_as_admin != current.run_as_admin && win32::is_self_elevated() {
        sync_elevated_task(&core.ui_log, clean.run_as_admin);
    }
    clean.save(&core.settings_file).map_err(|e| format!("Could not save settings: {e}"))?;
    win32::set_toast_corner(&clean.toast_corner);
    let clean_size = (clean.width, clean.height, clean.fps);
    let (clean_camera, clean_push_to_talk) = (clean.camera_enabled, clean.push_to_talk);
    *core.settings.lock().unwrap() = clean;
    if remote_changed {
        sync_remote_api(&core);
    }
    if usage_changed {
        usage::nudge();
    }
    register_hotkeys(&core);
    if restart {
        let again = Arc::clone(&core);
        thread::spawn(move || {
            // Settings now save as they change, so restarts can queue up. Each one uses the newest settings.
            let _one_at_a_time = again.restart_lock.lock().unwrap();
            let newest = again.effective(&again.settings.lock().unwrap().clone());
            // Waiting for a game, the new settings are used when it opens.
            if again.paused.load(Ordering::SeqCst) {
                again.engine.use_settings(newest);
                return;
            }
            again.engine.stop();
            let _ = again.engine.start(Some(newest));
            again.allow_media_folders();
        });
    } else {
        // Settings that apply without a restart, such as how long recordings split.
        let silenced = effective.mic_muted;
        core.engine.use_settings(effective);
        // Push-to-talk, or the webcam, may have just been turned on or off.
        if core.engine.status().buffering {
            let _ = core.engine.set_mic_muted(silenced);
            if clean_camera != current.camera_enabled {
                let _ = core.engine.set_camera(clean_camera);
            }
        }
        if !clean_push_to_talk {
            core.deck_talking.store(false, Ordering::SeqCst);
        }
    }
    {
        let again = Arc::clone(&core);
        thread::spawn(move || enforce_storage_rules(&again));
    }
    let failures = core.hotkey_failures.lock().unwrap().clone();
    Ok(json!({ "restarted": restart, "hotkeyFailures": failures, "width": clean_size.0, "height": clean_size.1, "fps": clean_size.2 }))
}

/// The release a version belongs to: a local test build such as 1.6.0-dev belongs to 1.6.0, so its patch
/// notes, once read there, aren't shown again when the release installs over it.
fn release_version(version: &str) -> String {
    version.trim_end_matches("-dev").to_string()
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
    stop_api(&core);
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
    if !core.paused.load(Ordering::SeqCst) {
        let engine = Arc::clone(&core.engine);
        thread::spawn(move || { let _ = engine.start(None); });
    }
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

#[derive(Clone, Deserialize)]
struct ShareSource {
    file: String,
    #[serde(default)]
    game: String,
    #[serde(default)]
    at: f64,
}

#[tauri::command(async)]
fn open_share(core: tauri::State<Shared>, payload: ShareSource) -> Result<(), String> {
    own_media(&core, &payload.file)?;
    *core.share_file.lock().unwrap() = Some(payload);
    if let Some(window) = core.app.get_webview_window(SHARE) {
        let _ = window.emit("share-source", ());
        let _ = window.show();
        let _ = window.set_focus();
        return Ok(());
    }
    let window = build_window(&core.app, SHARE, "share.html")
        .title("Afterclip · Edit and share")
        .inner_size(1120.0, 740.0)
        .min_inner_size(760.0, 540.0)
        .build()
        .map_err(|error| format!("Couldn't open Edit and share: {error}"))?;
    recover_from_browser_crash(&core, &window);
    watch_startup(&core, SHARE);
    Ok(())
}

#[tauri::command(async)]
fn share_source(core: tauri::State<Shared>) -> Result<Value, String> {
    let source = core.share_file.lock().unwrap().clone().ok_or("Open a clip first.")?;
    let path = Path::new(&source.file);
    let settings = core.settings.lock().unwrap();
    let channels: Vec<_> = settings.discord_webhooks.iter().enumerate()
        .filter(|(_, channel)| !channel.url.is_empty())
        .map(|(index, channel)| json!({ "index": index, "name": channel.name }))
        .collect();
    Ok(json!({
        "file": path, "name": path.file_name().unwrap_or_default().to_string_lossy(), "channels": channels,
        "canUpload": !settings.upload_url.is_empty(),
        "game": source.game, "at": source.at, "even": settings.even_audio,
        "recipes": settings.share_recipes, "layout": settings.vertical_layouts.get(&source.game),
        "hasBookmarks": studio::bookmark_file(path).is_file()
    }))
}

#[tauri::command(async)]
fn share_duration(core: tauri::State<Shared>, payload: String) -> Result<f64, String> {
    core.engine.duration(&own_media(&core, &payload)?)
}

#[tauri::command(async)]
fn save_share_recipe(core: tauri::State<Shared>, payload: advanced::Recipe) -> Result<(), String> {
    if !payload.valid() { return Err("Check the recipe name, tracks and size target.".into()); }
    let mut settings = core.settings.lock().unwrap();
    let mut updated = settings.clone();
    updated.share_recipes.retain(|recipe| recipe.name != payload.name);
    if updated.share_recipes.len() >= 30 { return Err("Remove a share recipe before adding another.".into()); }
    updated.share_recipes.push(payload);
    updated.save(&core.settings_file).map_err(|e| e.to_string())?;
    *settings = updated;
    Ok(())
}

#[tauri::command(async)]
fn delete_share_recipe(core: tauri::State<Shared>, payload: String) -> Result<(), String> {
    let mut settings = core.settings.lock().unwrap();
    let mut updated = settings.clone();
    updated.share_recipes.retain(|recipe| recipe.name != payload);
    updated.save(&core.settings_file).map_err(|e| e.to_string())?;
    *settings = updated;
    Ok(())
}

#[derive(Deserialize)]
struct GameLayout { game: String, layout: advanced::Layout }

#[tauri::command(async)]
fn save_game_layout(core: tauri::State<Shared>, payload: GameLayout) -> Result<(), String> {
    if !payload.layout.valid() || payload.game.trim().is_empty() || payload.game.chars().count() > 128 {
        return Err("Choose a game and a valid crop.".into());
    }
    let mut settings = core.settings.lock().unwrap();
    let mut updated = settings.clone();
    updated.vertical_layouts.insert(payload.game, payload.layout);
    updated.save(&core.settings_file).map_err(|e| e.to_string())?;
    *settings = updated;
    Ok(())
}

#[tauri::command(async)]
fn pick_angle(window: WebviewWindow, core: tauri::State<Shared>, payload: Option<String>) -> Result<Option<String>, String> {
    if window.label() != SHARE { return Err("Open Edit and share first.".into()); }
    let path = match payload.filter(|_| api::is_test_instance()) {
        Some(file) => own_media(&core, &file)?,
        None => {
            let Some(path) = core.app.dialog().file().add_filter("Video", &["mp4", "mkv", "mov"]).blocking_pick_file()
                .and_then(|p| p.into_path().ok()) else { return Ok(None) };
            path
        }
    };
    let path = strip_verbatim(path);
    core.app.asset_protocol_scope().allow_file(&path).map_err(|e| e.to_string())?;
    *core.angle_file.lock().unwrap() = Some(path.clone());
    Ok(Some(path.to_string_lossy().into_owned()))
}

#[tauri::command(async)]
fn sync_angle(core: tauri::State<Shared>, payload: String) -> Result<Value, String> {
    let _job = core.analysis_lock.try_lock().map_err(|_| "Another audio analysis is running.")?;
    let source = own_media(&core, &payload)?;
    let angle = core.angle_file.lock().unwrap().clone().ok_or("Choose a second clip first.")?;
    let a = core.engine.scratch_file("angle-a.wav")?;
    let b = core.engine.scratch_file("angle-b.wav")?;
    let result = core.engine.wave(&source, &a, &[]).and_then(|()| core.engine.wave(&angle, &b, &[]))
        .and_then(|()| advanced::sync_audio(&a, &b));
    let _ = fs::remove_file(a);
    let _ = fs::remove_file(b);
    result.map(|(offset, confidence)| json!({ "offset": offset, "confidence": confidence }))
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

/// The API for a Stream Deck on this PC, which runs the whole time, and the one for another PC if it's on.
fn start_services(core: &Shared) {
    let handlers: Arc<dyn ApiHandlers> = Arc::new(Api(Arc::clone(core)));
    match start_api(&format!("127.0.0.1:{}", api::local_port()), Arc::clone(&core.api_key), handlers) {
        Ok(server) => *core.api.lock().unwrap() = Some(server),
        Err(message) => core.log_ui(&format!("Stream Deck API could not start: {message}")),
    }
    sync_remote_api(core);
}

fn stop_api(core: &Core) {
    for server in [core.api.lock().unwrap().take(), core.remote_api.lock().unwrap().take()].into_iter().flatten() {
        api::stop_api(server);
    }
}

/// Opens or closes the API to other PCs on the network, to match the settings. This PC's own never stops.
fn sync_remote_api(core: &Shared) {
    let mut remote = core.remote_api.lock().unwrap();
    if let Some(server) = remote.take() {
        api::stop_api(server);
    }
    *core.api_error.lock().unwrap() = None;
    let Some(port) = core.settings.lock().unwrap().remote_port() else { return };
    let handlers: Arc<dyn ApiHandlers> = Arc::new(Api(Arc::clone(core)));
    match start_api(&format!("0.0.0.0:{port}"), Arc::clone(&core.api_key), handlers) {
        Ok(server) => *remote = Some(server),
        Err(message) => {
            core.log_ui(&format!("Stream Deck API for other PCs could not start: {message}"));
            *core.api_error.lock().unwrap() = Some(format!("{message}. Try another port."));
        }
    }
}

/// What Settings shows for a Stream Deck on another PC: the address to paste into it, with the key in it.
fn remote_json(core: &Core) -> Value {
    let settings = core.settings.lock().unwrap();
    let key = core.api_key.read().unwrap().clone();
    let address = api::lan_address().map(|ip| format!("http://{ip}:{}/?k={key}", settings.remote_port));
    json!({
        "on": settings.remote_stream_deck,
        "port": settings.remote_port,
        "address": address.filter(|_| settings.remote_stream_deck),
        "error": *core.api_error.lock().unwrap(),
    })
}

fn new_token() -> String {
    let mut bytes = [0u8; 24];
    getrandom::fill(&mut bytes).expect("the system random source failed");
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn save_token(file: &Path, token: &str) {
    if let Some(dir) = file.parent() {
        let _ = fs::create_dir_all(dir);
    }
    let _ = fs::write(file, token);
}

fn load_token(file: &Path) -> String {
    if let Ok(token) = fs::read_to_string(file) {
        return token.trim().to_string();
    }
    let token = new_token();
    save_token(file, &token);
    token
}

/// Makes a new Stream Deck key, so an address someone else has seen stops working. A Stream Deck on this PC
/// picks the new key up by itself, and one on another PC needs the new address.
#[tauri::command(async)]
fn new_api_key(core: tauri::State<Shared>) -> Value {
    let core: Shared = Arc::clone(&core);
    let key = new_token();
    save_token(&core.token_file, &key);
    *core.api_key.write().unwrap() = key;
    remote_json(&core)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_mic_that_is_really_the_capture_card_is_found() {
        let source = |label: &str| settings::AudioSource { kind: "input".into(), device: format!("{{{label}}}"), label: label.into(), mix: true, own_track: false, mute_with_key: None, volume: 1.0 };
        let mut s = Settings::defaults(Path::new(r"C:\Videos"), 1920, 1080);
        s.capture_card = true;
        s.capture_card_device = r"Elgato 4K X:\\?\usb#22vid_0fd9".into();
        s.capture_card_audio = "Elgato 4K X (Elgato 4K X):".into();
        s.audio_sources = vec![source("Microphone (Yeti)")];
        assert_eq!(doubled_card_audio(&s), None);
        s.audio_sources.push(source("Elgato 4K X (Elgato 4K X)"));
        assert_eq!(doubled_card_audio(&s).as_deref(), Some("Elgato 4K X (Elgato 4K X)"));
        s.capture_card_audio.clear();
        assert_eq!(doubled_card_audio(&s), None, "no sound from the card, so nothing is doubled");
    }

    /// Read-only: lists where a running process plays sound. Run with AFTERCLIP_TEST_PID set and --ignored --nocapture.
    #[test]
    fn storage_rules_keep_favourites_and_tagged_clips_when_asked() {
        let media = |favourite: bool, tags: &[&str]| Media {
            file: "a.mp4".into(), name: "a.mp4".into(), size: 1, mtime: 0.0, kind: "clip", favourite, game: None,
            tags: tags.iter().map(|t| t.to_string()).collect(),
        };
        assert!(kept_by_rules(&media(true, &[]), false));
        assert!(!kept_by_rules(&media(false, &["clutch"]), false));
        assert!(kept_by_rules(&media(false, &["clutch"]), true));
        assert!(!kept_by_rules(&media(false, &[AUTO_TAG]), true));
        assert!(kept_by_rules(&media(false, &[AUTO_TAG, "clutch"]), true));
    }

    #[test]
    fn loudness_goes_from_silent_to_full() {
        let mut samples = vec![0i16; 1600];
        samples.extend(std::iter::repeat_n(i16::MAX, 1600));
        let levels = loudness_levels(&samples, 2);
        assert_eq!(levels, vec![0.0, 1.0]);
        assert!(loudness_levels(&[], 10).is_empty());
        // About -20 dB is 60% of the way up from the -50 dB floor.
        let quiet = loudness_levels(&vec![3277i16; 100], 1)[0];
        assert!((quiet - 0.6).abs() < 0.01, "{quiet}");
    }

    #[test]
    fn going_back_to_the_graphics_card_only_after_a_fallback() {
        assert!(fell_back("auto", Some("obs_x264"), Some("obs_nvenc_h264_tex")), "fell back during a driver update");
        assert!(!fell_back("x264", Some("obs_x264"), Some("obs_nvenc_h264_tex")), "the CPU was chosen");
        assert!(!fell_back("auto", Some("obs_x264"), None), "no graphics card encoder has ever worked here");
        assert!(!fell_back("auto", Some("obs_nvenc_h264_tex"), Some("obs_nvenc_h264_tex")), "already on the graphics card");
        assert!(fell_back("auto", Some("h265_texture_amf"), Some("obs_nvenc_hevc_tex")), "fell back to the processor's graphics");
        assert!(fell_back("nvenc", Some("h264_texture_amf"), Some("obs_nvenc_h264_tex")), "NVIDIA was chosen but AMD is in use");
        assert!(!fell_back("amd", Some("h264_texture_amf"), Some("obs_nvenc_h264_tex")), "AMD was chosen after NVIDIA worked");
        assert!(!fell_back("auto", Some("obs_nvenc_h264_tex"), Some("h264_texture_amf")), "a better encoder is never a fallback");
    }

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
            state, estimate, installed_memory_mb, clip, share_clip, toggle_record, toggle_mic, toggle_clean_mic, add_marker, screenshot, set_volume, set_meters, levels, loud_level, stats, close_panel, open, reveal, start_drag, trash, restore, dismiss_session, set_favourite, set_tags, rename_clip, clip_tracks, check_update, install_update, skip_update, dismiss_patch_notes, install_stream_deck_plugin, dismiss_stream_deck_plugin, media::thumbnail, media::preview_strip, media::find_overlaps, media::keep_overlap, media::clip_loudness, pick_folder, save_diagnostics, new_api_key, export_settings,
 import_settings, running_apps, pick_game, audio_choices, camera_choices, card_choices, card_preview, card_modes, display_info,
            save_settings, restart_admin, window_ready, open_editor, editor_files, export_edit,
            open_share, share_source, share_duration, save_share_recipe, delete_share_recipe,
            save_game_layout, pick_angle, sync_angle,
            studio::open_studio, studio::studio_state, studio::configure_recovery, studio::checkpoint_now, studio::restore_replay,
            studio::import_clips, studio::set_watch_folder, studio::clip_bookmarks,
            studio::phone_share, studio::stop_phone_share, studio::enqueue_batch, studio::control_batch,
            reel::make_weekly_reel,
            audio_check::audio_setup, audio_check::apply_audio_roles, audio_check::test_audio_tracks,
            render::render_clip, render::export_angles, quit_app
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
            let first_start = !settings_file.exists();
            let mut settings = Settings::load(&settings_file, Settings::defaults(&videos, width, height))
                .resolved_for(win32::primary_display_mode());
            // Patch notes are for people who updated, not for a fresh install.
            if first_start {
                settings.seen_version = release_version(&app.package_info().version.to_string());
            }
            let open_at_start = !settings.start_minimized;
            win32::set_toast_corner(&settings.toast_corner);
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
                remote_api: Mutex::new(None),
                api_key: Arc::new(std::sync::RwLock::new(load_token(&user_data.join("api-token.txt")))),

                api_error: Mutex::new(None),
                hotkey_failures: Mutex::new(Vec::new()),
                thumbnails: previews::Gate::default(),
                restart_lock: Mutex::new(()),
                quitting: AtomicBool::new(false),
                keep_panel_open: AtomicBool::new(false),
                panel_hidden_at: Mutex::new(None),
                panel_recoveries: Mutex::new(Vec::new()),
                panel_hide_count: AtomicU64::new(0),
                tray_anchor: Mutex::new(None),
                pending_editor_files: Mutex::new(Vec::new()),
                share_file: Mutex::new(None),
                angle_file: Mutex::new(None),
                analysis_lock: Mutex::new(()),
                studio: studio::Studio::load(),
                ready_windows: Mutex::new(Vec::new()),
                library: Mutex::new(LibraryData::load(&user_data.join("library.json"))),
                library_file: user_data.join("library.json"),
                active_profile: Mutex::new(None),
                test_game: Mutex::new(None),
                deleted: Mutex::new(Vec::new()),
                last_session: Mutex::new(None),
                last_clip_press: Mutex::new(None),
                controller: OnceLock::new(),
                update: Mutex::new(updater::UpdateState { status: "idle".into(), ..Default::default() }),
                update_ready: Mutex::new(None),
                update_busy: AtomicBool::new(false),
                storage_lock: Mutex::new(()),
                strips: previews::Gate::default(),
                paused: AtomicBool::new(false),
                talking: AtomicBool::new(false),
                test_talk: Mutex::new(None),
                test_free_bytes: Mutex::new(None),
                deck_talking: AtomicBool::new(false),

            });
            let capture_events = handle.clone();
            let detection_settings = Arc::clone(&shared_settings);
            let profile_settings = Arc::clone(&shared_settings);
            let toast_settings = Arc::clone(&shared_settings);
            let warned_no_picture = Arc::new(AtomicBool::new(false));
            let suggested_card_hdr = AtomicBool::new(false);
            let manager = CaptureManager::start(
                Arc::clone(&engine),
                move || {
                    let s = detection_settings.lock().unwrap();
                    if s.test_pattern {
                        Watch::Off
                    } else if s.card().is_some() {
                        Watch::Card { pause_without_signal: s.card_pause_without_signal, card_hdr: s.card_hdr }
                    } else {
                        Watch::Games
                    }
                },
                move || profile_settings.lock().unwrap().game_profiles.iter().map(|p| p.exe.clone()).collect(),
                move |state| {
                    let _ = capture_events.emit("capture", state);
                    // Said once each time it happens, since the panel is usually closed while playing.
                    if state.no_picture && !warned_no_picture.swap(true, Ordering::SeqCst) {
                        if toast_settings.lock().unwrap().toast_enabled {
                            win32::show_toast("No picture from your capture card. Close it in other apps such as OBS, and check the console is on with HDCP off. Afterclip keeps trying.");
                        }
                    } else if !state.no_picture {
                        warned_no_picture.store(false, Ordering::SeqCst);
                    }
                    // Once a session: the panel offers to turn it on.
                    if state.suggest_card_hdr && !suggested_card_hdr.swap(true, Ordering::SeqCst) && toast_settings.lock().unwrap().toast_enabled {
                        win32::show_toast("Your capture card's picture looks grey and washed out, as if your console sends HDR. Open Afterclip to turn on Console sends HDR.");
                    }
                },
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
            usage::start(Arc::clone(&core));
            let watcher = Arc::clone(&core);
            thread::spawn(move || watch_games(watcher));
            let tools = Arc::clone(&core);
            thread::spawn(move || studio::watch_tools(tools));
            let queue = Arc::clone(&core);
            thread::spawn(move || studio::watch_queue(queue));
            let healer = Arc::clone(&core);
            thread::spawn(move || watch_engine(healer));
            let talker = Arc::clone(&core);
            thread::spawn(move || watch_push_to_talk(talker));
            let listener = Arc::clone(&core);
            thread::spawn(move || watch_loud_moments(listener));
            let retagger = Arc::clone(&core);
            thread::spawn(move || retag_old_clips(&retagger));
            let reeler = Arc::clone(&core);
            thread::spawn(move || reel::watch(reeler));
            let saver = Arc::clone(&core);
            thread::spawn(move || watch_memory(saver));
            let sweeper = Arc::clone(&core);
            thread::spawn(move || loop {
                // A while after starting, then daily, so it never slows startup.
                thread::sleep(SWEEP_THUMBNAILS_AFTER);
                sweep_thumbnails(&sweeper);
                thread::sleep(SWEEP_THUMBNAILS_EVERY.saturating_sub(SWEEP_THUMBNAILS_AFTER));
            });
            core.allow_media_folders();
            // Audio devices that got new ids, such as after a driver reinstall, are saved under them, so the
            // settings show what's really recorded. The engine finds them again by name on every start too.
            {
                let (outputs, inputs) = (win32::audio_endpoints(false), win32::audio_endpoints(true));
                let mut settings = core.settings.lock().unwrap();
                let (changed, missing) = settings.relink_audio(outputs.as_deref(), inputs.as_deref());
                if changed {
                    let _ = settings.save(&core.settings_file);
                    core.log_ui("audio sources found again under new device ids");
                }
                if !missing.is_empty() {
                    core.log_ui(&format!("audio sources not connected: {}", missing.join(", ")));
                }
            }
            // With the buffer only running during games, it waits for one.
            let first = core.settings.lock().unwrap().clone();
            if first.pauses_without_game() && !game_in_front(&core, &first) {
                core.paused.store(true, Ordering::SeqCst);
                engine.set_waiting_for_game(true);
            } else {
                // A failure reaches the UI through status. Normally no window opens: the app lives in the tray.
                let starting = Arc::clone(&core);
                thread::spawn(move || { let _ = engine.start(Some(starting.effective(&first))); });
            }

            // A fresh install on a PC with a Stream Deck opens the panel once, where it offers the plugin.
            let offer_deck = first_start && !api::is_test_instance() && deck::offer(&handle) == Some("install");
            if open_at_start || offer_deck {
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
