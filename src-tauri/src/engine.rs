use crate::settings::{estimate_buffer_mb, Settings};
use crate::win32::{ActiveEndpoint, FullscreenApp};
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const BELOW_NORMAL_PRIORITY_CLASS: u32 = 0x0000_4000;
const RPC_TIMEOUT: Duration = Duration::from_secs(60);
const SAVE_TIMEOUT: Duration = Duration::from_secs(60);
const START_TIMEOUT: Duration = Duration::from_secs(30);
const STOP_TIMEOUT: Duration = Duration::from_secs(12);

#[derive(Clone, Debug, Serialize)]
pub struct EngineStatus {
    pub state: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    pub buffering: bool,
    /// When the buffer started filling, in milliseconds since 1970, so the UI can show how full it is.
    #[serde(rename = "bufferingSince", skip_serializing_if = "Option::is_none")]
    pub buffering_since: Option<u128>,
    pub recording: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClipResult {
    pub file: String,
    pub requested_seconds: i64,
    pub actual_seconds: f64,
}

pub enum EngineEvent {
    Status(EngineStatus),
    RecordingSaved(String),
    Warning(String),
}

struct Running {
    child: Child,
    stdin: ChildStdin,
    generation: u64,
}

/// Runs afterclip-engine.exe, a windowless libobs host bundled with the app, and talks to it over private
/// stdin/stdout pipes. It opens no listening socket and needs no OBS install. It exits when the pipes close.
pub struct Engine {
    home: PathBuf,
    root: PathBuf,
    settings: Mutex<Settings>,
    status: Mutex<EngineStatus>,
    running: Mutex<Option<Running>>,
    pending: Mutex<HashMap<u64, Sender<Result<Value, String>>>>,
    saved: Mutex<Option<Sender<String>>>,
    clip_lock: Mutex<()>,
    recording_file: Mutex<Option<PathBuf>>,
    game: Mutex<Option<String>>,
    encoder_in_use: Mutex<Option<String>>,
    marker_count: AtomicU64,
    next_id: AtomicU64,
    generation: AtomicU64,
    stopping: AtomicBool,
    on_event: Box<dyn Fn(EngineEvent) + Send + Sync>,
}

impl Engine {
    pub fn new(home: PathBuf, root: PathBuf, settings: Settings, on_event: Box<dyn Fn(EngineEvent) + Send + Sync>) -> Arc<Self> {
        Arc::new(Engine {
            home,
            root,
            settings: Mutex::new(settings),
            status: Mutex::new(EngineStatus { state: "stopped", message: None, buffering: false, buffering_since: None, recording: false }),
            running: Mutex::new(None),
            pending: Mutex::new(HashMap::new()),
            saved: Mutex::new(None),
            clip_lock: Mutex::new(()),
            recording_file: Mutex::new(None),
            game: Mutex::new(None),
            encoder_in_use: Mutex::new(None),
            marker_count: AtomicU64::new(0),
            next_id: AtomicU64::new(1),
            generation: AtomicU64::new(0),
            stopping: AtomicBool::new(false),
            on_event,
        })
    }

    fn output_dir(&self) -> PathBuf {
        PathBuf::from(&self.settings.lock().unwrap().output_dir)
    }
    fn staging_dir(&self) -> PathBuf {
        self.output_dir().join(".staging")
    }
    pub fn clips_dir(&self) -> PathBuf {
        self.output_dir().join("Clips")
    }
    pub fn recordings_dir(&self) -> PathBuf {
        self.output_dir().join("Recordings")
    }
    pub fn thumbs_dir(&self) -> PathBuf {
        self.output_dir().join(".thumbs")
    }
    fn bin_dir(&self) -> PathBuf {
        self.root.join("bin").join("64bit")
    }

    /// The file being recorded right now, which isn't playable until it stops.
    pub fn active_recording(&self) -> Option<PathBuf> {
        if self.status().recording {
            self.recording_file.lock().unwrap().clone()
        } else {
            None
        }
    }

    pub fn status(&self) -> EngineStatus {
        self.status.lock().unwrap().clone()
    }

    /// Changes on every start, which is how capture detection knows to start over.
    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::SeqCst)
    }

    fn update(&self, change: impl FnOnce(&mut EngineStatus)) {
        let snapshot = {
            let mut status = self.status.lock().unwrap();
            change(&mut status);
            status.clone()
        };
        (self.on_event)(EngineEvent::Status(snapshot));
    }

    fn rpc(&self, cmd: &str, args: Value) -> Result<Value, String> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = channel();
        let mut message = args;
        message["id"] = json!(id);
        message["cmd"] = json!(cmd);
        {
            let mut running = self.running.lock().unwrap();
            let running = running.as_mut().ok_or("Capture engine is not running")?;
            self.pending.lock().unwrap().insert(id, tx);
            let line = format!("{message}\n");
            if running.stdin.write_all(line.as_bytes()).and_then(|_| running.stdin.flush()).is_err() {
                self.pending.lock().unwrap().remove(&id);
                return Err("Capture engine is not running".into());
            }
        }
        let reply = rx.recv_timeout(RPC_TIMEOUT);
        self.pending.lock().unwrap().remove(&id);
        reply.map_err(|_| format!("Capture engine did not answer {cmd}"))?
    }

    fn on_message(&self, line: &str) {
        let Ok(m) = serde_json::from_str::<Value>(line) else { return };
        let active = m["active"].as_bool().unwrap_or(false);
        let code = m["code"].as_i64().unwrap_or(0);
        match m["event"].as_str() {
            Some("replay") => {
                let unexpected = !active && !self.stopping.load(Ordering::SeqCst) && self.status().state == "ready";
                self.update(|s| {
                    s.buffering = active;
                    s.buffering_since = active.then(|| {
                        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis())
                    });
                    if unexpected {
                        s.state = "error";
                        s.message = Some(format!("Replay buffer stopped unexpectedly (code {code})"));
                    }
                });
            }
            Some("record") => {
                self.update(|s| s.recording = active);
                if !active {
                    if let Some(file) = self.recording_file.lock().unwrap().take() {
                        if file.exists() {
                            (self.on_event)(EngineEvent::RecordingSaved(file.to_string_lossy().into_owned()));
                        }
                        if code != 0 {
                            (self.on_event)(EngineEvent::Warning(format!("Recording stopped with an error (code {code})")));
                        }
                    }
                }
            }
            // A long recording moved on to its next file, so the last one is finished.
            Some("recordFile") => {
                if let Some(next) = m["path"].as_str() {
                    let finished = self.recording_file.lock().unwrap().replace(PathBuf::from(next.replace('/', "\\")));
                    if let Some(file) = finished {
                        (self.on_event)(EngineEvent::RecordingSaved(file.to_string_lossy().into_owned()));
                    }
                }
            }
            Some("saved") => {
                if let (Some(path), Some(tx)) = (m["path"].as_str(), self.saved.lock().unwrap().as_ref()) {
                    let _ = tx.send(path.to_string());
                }
            }
            _ => {
                if let Some(id) = m["id"].as_u64() {
                    if let Some(tx) = self.pending.lock().unwrap().remove(&id) {
                        let reply = if m["ok"].as_bool() == Some(true) {
                            Ok(m)
                        } else {
                            Err(m["error"].as_str().unwrap_or("Engine command failed").to_string())
                        };
                        let _ = tx.send(reply);
                    }
                }
            }
        }
    }

    fn on_exit(&self, generation: u64) {
        {
            let mut running = self.running.lock().unwrap();
            if running.as_ref().map(|r| r.generation) != Some(generation) {
                return;
            }
            *running = None;
        }
        self.pending.lock().unwrap().clear();
        if !self.stopping.load(Ordering::SeqCst) {
            self.update(|s| {
                s.state = "error";
                s.message = Some("Capture engine exited unexpectedly".into());
                s.buffering = false;
                s.recording = false;
            });
        }
    }

    pub fn start(self: &Arc<Self>, settings: Option<Settings>) -> Result<(), String> {
        if let Some(settings) = settings {
            *self.settings.lock().unwrap() = settings;
        }
        self.stopping.store(false, Ordering::SeqCst);
        let result = self.try_start();
        if let Err(e) = &result {
            self.stop();
            self.update(|s| {
                s.state = "error";
                s.message = Some(e.clone());
            });
        }
        result
    }

    fn try_start(self: &Arc<Self>) -> Result<(), String> {
        self.update(|s| {
            s.state = "starting";
            s.message = Some("Starting capture".into());
            s.buffering = false;
            s.recording = false;
        });
        let config = self.home.join("engine");
        for dir in [self.clips_dir(), self.recordings_dir(), self.staging_dir(), config.clone()] {
            fs::create_dir_all(&dir).map_err(|e| format!("Could not create {}: {e}", dir.display()))?;
        }
        // Earlier versions unpacked a full OBS here. Nothing uses it any more.
        let (old_obs, old_password) = (self.home.join("obs"), self.home.join("engine-password.txt"));
        thread::spawn(move || {
            let _ = fs::remove_dir_all(old_obs);
            let _ = fs::remove_file(old_password);
        });

        let bin = self.bin_dir();
        let exe = bin.join("afterclip-engine.exe");
        if !exe.exists() {
            return Err(format!("Afterclip's capture engine is missing from {}", bin.display()));
        }
        let mut child = Command::new(&exe)
            .arg(&self.root)
            .arg(&config)
            .current_dir(&bin)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .map_err(|e| format!("Could not start the capture engine: {e}"))?;
        let stdin = child.stdin.take().ok_or("No engine stdin")?;
        let stdout = child.stdout.take().ok_or("No engine stdout")?;
        let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        *self.running.lock().unwrap() = Some(Running { child, stdin, generation });

        let me = Arc::clone(self);
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                me.on_message(&line);
            }
            me.on_exit(generation);
        });

        let s = self.settings.lock().unwrap().clone();
        let started = self.rpc("start", json!({
            "root": self.root,
            "width": s.width,
            "height": s.height,
            "fps": s.fps,
            "bitrateKbps": s.bitrate_kbps,
            "bufferSeconds": s.buffer_seconds,
            // The size cap must not cut the buffer short, so leave headroom over the estimate.
            "bufferMaxMb": estimate_buffer_mb(s.buffer_seconds, s.bitrate_kbps) * 3 / 2 + 64,
            "stagingDir": self.staging_dir(),
            "encoder": s.encoder,
            "codec": s.codec,
            "preset": s.encoder_preset,
            "multipass": s.multipass,
            "audio": s.audio_sources.iter().map(|a| {
                let mut source = json!(a);
                source["muteKey"] = json!(a.muted_by_key());
                source
            }).collect::<Vec<_>>(),
            "micMuted": s.mic_muted,
            "gameAudio": s.game_audio,
            "gameAudioOwnTrack": s.game_audio_own_track,
            "gameAudioVolume": s.game_audio_volume,
            "cleanMic": s.clean_mic,
            "testPattern": s.test_pattern,
        }))?;
        *self.encoder_in_use.lock().unwrap() = started["encoder"].as_str().map(str::to_string);

        let deadline = Instant::now() + START_TIMEOUT;
        while !self.status().buffering {
            if Instant::now() > deadline {
                return Err("The replay buffer did not start".into());
            }
            thread::sleep(Duration::from_millis(100));
        }
        self.update(|s| {
            s.state = "ready";
            s.message = None;
        });
        Ok(())
    }

    pub fn stop(&self) {
        self.stopping.store(true, Ordering::SeqCst);
        let running = self.running.lock().unwrap().take();
        if let Some(Running { mut child, stdin, .. }) = running {
            // Closing stdin is the shutdown signal. The engine finalizes any recording first.
            drop(stdin);
            let deadline = Instant::now() + STOP_TIMEOUT;
            while Instant::now() < deadline {
                if matches!(child.try_wait(), Ok(Some(_))) {
                    break;
                }
                thread::sleep(Duration::from_millis(50));
            }
            let _ = child.kill();
            let _ = child.wait();
        }
        self.pending.lock().unwrap().clear();
        self.update(|s| {
            s.state = "stopped";
            s.message = None;
            s.buffering = false;
            s.recording = false;
        });
    }

    /// Saves a clip of the last `seconds`. 0 keeps the whole buffer. Saves run one at a time.
    pub fn clip(&self, seconds: i64) -> Result<ClipResult, String> {
        let _one_at_a_time = self.clip_lock.lock().unwrap();
        if !self.status().buffering {
            return Err("Replay buffer is not running".into());
        }
        let replay = PathBuf::from(self.save_replay()?);
        let label = if seconds > 0 { format!("{seconds}s") } else { "full".to_string() };
        let file = unique_path(&self.clips_dir().join(format!("{}_{}_{label}.mp4", self.name_prefix("Clip"), timestamp())));

        let available = self.duration(&replay)?;
        if seconds > 0 && (seconds as f64) < available {
            self.media(&["trim".as_ref(), replay.as_os_str(), file.as_os_str(), seconds.to_string().as_ref()], false)?;
            let _ = fs::remove_file(&replay);
        } else {
            move_file(&replay, &file)?;
        }
        Ok(ClipResult {
            actual_seconds: self.duration(&file)?,
            file: file.to_string_lossy().into_owned(),
            requested_seconds: seconds,
        })
    }

    /// Mutes or unmutes the marked sources straight away, without restarting capture.
    pub fn set_mic_muted(&self, muted: bool) -> Result<(), String> {
        let source_count = self.rpc("micMute", json!({ "muted": muted }))?["sources"].as_i64().unwrap_or(0);
        if source_count == 0 {
            return Err("No source is set to mute. Tick Mute key on your mic in Settings.".into());
        }
        Ok(())
    }

    /// Takes the settings the UI just saved. A restart reads them.
    pub fn use_settings(&self, settings: Settings) {
        *self.settings.lock().unwrap() = settings;
    }

    /// The encoder that actually started, which can differ from the choice if that one isn't available.
    pub fn encoder_in_use(&self) -> Option<String> {
        self.encoder_in_use.lock().unwrap().clone()
    }

    pub fn game_audio_enabled(&self) -> bool {
        self.settings.lock().unwrap().game_audio
    }

    /// Points game audio at an app, or turns it off for None. `mix` adds it to the main mix.
    pub fn set_game_audio(&self, app: Option<&FullscreenApp>, mix: bool) -> Result<(), String> {
        let window = app.map(window_match).unwrap_or_default();
        self.rpc("gameAudio", json!({ "window": window, "mix": mix })).map(|_| ())
    }

    /// Whether the main mix already records this output device, in which case adding the game would double it.
    pub fn mix_records(&self, endpoint: &ActiveEndpoint) -> bool {
        let device_base = |name: &str| {
            let end = [" Input", " Output"].iter().filter_map(|w| name.find(w)).min().unwrap_or(name.len());
            name[..end].to_string()
        };
        self.settings.lock().unwrap().audio_sources.iter().filter(|s| s.mix).any(|s| match s.kind.as_str() {
            "output" => s.device == endpoint.id || (s.device == "default" && endpoint.is_default),
            // A virtual cable: what plays into "CABLE-C Input" comes out of "CABLE-C Output".
            "input" => device_base(&s.label) == device_base(&endpoint.name) && s.label != endpoint.name,
            _ => false,
        })
    }

    /// Files are named after the game being captured, which is what the clip list shows.
    pub fn set_game(&self, game: Option<String>) {
        *self.game.lock().unwrap() = game;
    }

    fn name_prefix(&self, fallback: &str) -> String {
        let game = self.game.lock().unwrap();
        let safe: String = game.as_deref().unwrap_or("").chars()
            // % too, because split recordings name their files through a date pattern.
            .filter(|c| !r#"<>:"/\|?*%"#.contains(*c) && !c.is_control())
            .collect();
        let safe = safe.trim().trim_end_matches('.').to_string();
        if safe.is_empty() { fallback.to_string() } else { safe }
    }

    /// The audio devices and running apps that can be recorded right now.
    pub fn audio_choices(&self) -> Result<Value, String> {
        self.rpc("audioChoices", json!({}))
    }

    /// Joins ranges of clips into a new file in Clips, losslessly. Originals are never changed.
    pub fn export_segments(&self, segments: &[(PathBuf, f64, f64)]) -> Result<PathBuf, String> {
        let kind = if segments.len() > 1 { "Merged" } else { "Edit" };
        let file = unique_path(&self.clips_dir().join(format!("{kind}_{}.mp4", timestamp())));
        let mut args: Vec<std::ffi::OsString> = vec!["join".into(), file.clone().into()];
        for (path, start, end) in segments {
            args.push(path.into());
            args.push(start.to_string().into());
            args.push(end.to_string().into());
        }
        let refs: Vec<&std::ffi::OsStr> = args.iter().map(|a| a.as_os_str()).collect();
        if let Err(e) = self.media(&refs, false) {
            let _ = fs::remove_file(&file);
            return Err(e);
        }
        Ok(file)
    }

    fn save_replay(&self) -> Result<String, String> {
        let (tx, rx) = channel();
        *self.saved.lock().unwrap() = Some(tx);
        let result = self.rpc("save", json!({})).and_then(|_| {
            rx.recv_timeout(SAVE_TIMEOUT).map_err(|_| "The replay did not save in time".to_string())
        });
        *self.saved.lock().unwrap() = None;
        result
    }

    pub fn duration(&self, file: &Path) -> Result<f64, String> {
        let out = self.media(&["duration".as_ref(), file.as_os_str()], false)?;
        out.trim().parse().map_err(|_| format!("Could not read duration of {}", file.display()))
    }

    /// Runs one afterclip-media.exe job. Background work drops in priority, so it can't compete with a game.
    fn media(&self, args: &[&std::ffi::OsStr], background: bool) -> Result<String, String> {
        let flags = CREATE_NO_WINDOW | if background { BELOW_NORMAL_PRIORITY_CLASS } else { 0 };
        let out = Command::new(self.bin_dir().join("afterclip-media.exe"))
            .args(args)
            .current_dir(self.bin_dir())
            .stdin(Stdio::null())
            .creation_flags(flags)
            .output()
            .map_err(|e| format!("Could not run the media tool: {e}"))?;
        if out.status.success() {
            Ok(String::from_utf8_lossy(&out.stdout).into_owned())
        } else {
            Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
        }
    }

    pub fn make_thumbnail(&self, input: &Path, output: &Path) -> Result<(), String> {
        self.media(&["thumb".as_ref(), input.as_os_str(), output.as_os_str()], true).map(|_| ())
    }

    pub fn toggle_record(&self) -> Result<bool, String> {
        if self.status().recording {
            self.rpc("recordStop", json!({}))?;
            return Ok(false);
        }
        let prefix = self.name_prefix("Recording");
        let file = unique_path(&self.recordings_dir().join(format!("{prefix}_{}.mp4", timestamp())));
        *self.recording_file.lock().unwrap() = Some(file.clone());
        self.marker_count.store(0, Ordering::SeqCst);
        let split = self.settings.lock().unwrap().record_split_seconds;
        let started = self.rpc("recordStart", json!({
            "path": file,
            "splitSeconds": split,
            "directory": self.recordings_dir(),
            "format": format!("{prefix}_%CCYY-%MM-%DD_%hh-%mm-%ss"),
        }));
        if let Err(e) = started {
            *self.recording_file.lock().unwrap() = None;
            return Err(e);
        }
        Ok(true)
    }

    /// Drops a chapter into the running recording. Returns its number.
    pub fn add_marker(&self) -> Result<u64, String> {
        let number = self.marker_count.load(Ordering::SeqCst) + 1;
        self.rpc("marker", json!({ "name": format!("Marker {number}") }))?;
        self.marker_count.store(number, Ordering::SeqCst);
        Ok(number)
    }

    pub fn set_clean_mic(&self, on: bool) -> Result<(), String> {
        if self.rpc("cleanMic", json!({ "on": on }))?["filters"].as_i64().unwrap_or(0) == 0 {
            return Err("No mic is set. Tick Mute key on your mic in Settings.".into());
        }
        Ok(())
    }

    /// Changes one source's volume straight away. Index -1 is game audio.
    pub fn set_volume(&self, index: i64, volume: f64) -> Result<(), String> {
        self.rpc("volume", json!({ "index": index, "volume": volume })).map(|_| ())
    }

    pub fn set_meters(&self, on: bool) -> Result<(), String> {
        self.rpc("meters", json!({ "on": on })).map(|_| ())
    }

    pub fn levels(&self) -> Result<Value, String> {
        self.rpc("levels", json!({})).map(|m| m["levels"].clone())
    }

    pub fn stats(&self) -> Result<Value, String> {
        self.rpc("stats", json!({}))
    }

    /// Records the primary monitor, with game capture off.
    pub fn capture_desktop(&self) -> Result<(), String> {
        self.rpc("gameOff", json!({}))?;
        self.rpc("display", json!({ "hasOrigin": false }))?;
        self.set_display_visible(true)
    }

    /// Tries to hook a fullscreen app, recording its monitor underneath until the hook lands or if it never does.
    pub fn capture_app(&self, app: &FullscreenApp) -> Result<(), String> {
        self.rpc("display", json!({ "hasOrigin": true, "x": app.monitor_left, "y": app.monitor_top }))?;
        self.set_display_visible(true)?;
        self.rpc("game", json!({ "window": window_match(app) })).map(|_| ())
    }

    /// Display capture is the costliest source, so it stays off while a hooked game covers it.
    pub fn set_display_visible(&self, visible: bool) -> Result<(), String> {
        self.rpc("displayVisible", json!({ "visible": visible })).map(|_| ())
    }

    pub fn is_app_hooked(&self) -> Result<bool, String> {
        Ok(self.rpc("hooked", json!({}))?["hooked"].as_bool().unwrap_or(false))
    }
}

/// The title:class:exe string OBS's capture sources use to find a window.
fn window_match(app: &FullscreenApp) -> String {
    let encode = |s: &str| s.replace('#', "#22").replace(':', "#3A");
    format!("{}:{}:{}", encode(&app.title), encode(&app.window_class), encode(&app.exe))
}

fn timestamp() -> String {
    chrono::Local::now().format("%Y-%m-%d_%H-%M-%S").to_string()
}

fn unique_path(file: &Path) -> PathBuf {
    let dir = file.parent().unwrap_or(Path::new("."));
    let stem = file.file_stem().unwrap_or_default().to_string_lossy().into_owned();
    let ext = file.extension().unwrap_or_default().to_string_lossy().into_owned();
    let mut candidate = file.to_path_buf();
    let mut n = 2;
    while candidate.exists() {
        candidate = dir.join(format!("{stem}_{n}.{ext}"));
        n += 1;
    }
    candidate
}

/// Moves a file, falling back to copy and delete when the rename crosses drives.
fn move_file(from: &Path, to: &Path) -> Result<(), String> {
    if fs::rename(from, to).is_ok() {
        return Ok(());
    }
    fs::copy(from, to).map_err(|e| format!("Could not save clip: {e}"))?;
    let _ = fs::remove_file(from);
    Ok(())
}
