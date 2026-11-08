use super::*;
use std::collections::{HashMap, VecDeque};
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom};
use std::os::windows::fs::OpenOptionsExt;
use std::sync::atomic::AtomicBool;
use tiny_http::{Header, Method, Response, Server, StatusCode};

const STUDIO: &str = "studio";
const TRANSFER_PORT: u16 = 0;

#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Preferences {
    recovery_seconds: u64,
    recovery_mb: u64,
    watch_folder: Option<PathBuf>,
    minecraft_log: Option<PathBuf>,
    minecraft_player: String,
    cs2_token: Option<String>,
}

#[derive(Clone, Deserialize, Serialize)]
pub struct Bookmark {
    pub time: f64,
    pub title: String,
}

#[derive(Clone, Serialize)]
struct Job {
    id: u64,
    file: String,
    recipe: String,
    status: &'static str,
    detail: String,
}

pub struct Studio {
    prefs: Mutex<Preferences>,
    jobs: Mutex<VecDeque<Job>>,
    batch_selection: Mutex<Vec<String>>,
    next_job: AtomicU64,
    paused: AtomicBool,
    pause_in_game: AtomicBool,
    transfer: Mutex<Option<Arc<AtomicBool>>>,
    checkpoint_lock: Mutex<()>,
}

fn dir() -> PathBuf {
    home_dir().join("studio")
}

impl Studio {
    pub fn load() -> Self {
        let prefs = fs::read(dir().join("settings.json"))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        preserve_previous();
        Self {
            prefs: Mutex::new(prefs),
            jobs: Mutex::new(VecDeque::new()),
            batch_selection: Mutex::new(Vec::new()),
            next_job: AtomicU64::new(1),
            paused: AtomicBool::new(false),
            pause_in_game: AtomicBool::new(true),
            transfer: Mutex::new(None),
            checkpoint_lock: Mutex::new(()),
        }
    }

    fn edit(&self, change: impl FnOnce(&mut Preferences)) -> Result<(), String> {
        let mut prefs = self.prefs.lock().unwrap();
        let mut updated = prefs.clone();
        change(&mut updated);
        fs::create_dir_all(dir()).map_err(|e| e.to_string())?;
        fs::write(
            dir().join("settings.json"),
            serde_json::to_vec_pretty(&updated).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        *prefs = updated;
        Ok(())
    }
}

fn name(path: &Path) -> String {
    path.file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned()
}

fn previous_replay() -> PathBuf {
    dir().join("replay-previous.mp4")
}

fn live_recovery_files() -> Vec<PathBuf> {
    let mut files: Vec<_> = ["replay-a.mp4", "replay-b.mp4"]
        .into_iter()
        .map(|file| dir().join(file))
        .filter_map(|file| {
            fs::metadata(&file)
                .ok()?
                .modified()
                .ok()
                .map(|modified| (modified, file))
        })
        .collect();
    files.sort_by_key(|(modified, _)| std::cmp::Reverse(*modified));
    files.into_iter().map(|(_, path)| path).collect()
}

fn preserve_previous() {
    let previous = previous_replay();
    if previous.exists() {
        return;
    }
    if let Some(latest) = live_recovery_files().into_iter().next() {
        let _ = fs::rename(latest, previous);
    }
}

fn restorable_replay() -> Option<PathBuf> {
    let previous = previous_replay();
    if previous.is_file() {
        Some(previous)
    } else {
        live_recovery_files().into_iter().next()
    }
}

fn checkpoint(core: &Shared, prefs: &Preferences) -> Result<(), String> {
    let _lock = core
        .studio
        .checkpoint_lock
        .try_lock()
        .map_err(|_| "A replay checkpoint is already running.")?;
    if prefs.recovery_seconds == 0 || !core.engine.status().buffering {
        return Err("Start the replay buffer first.".into());
    }
    let cap = prefs.recovery_mb.clamp(100, 5000) * 1_000_000;
    fs::create_dir_all(dir()).map_err(|e| e.to_string())?;
    if win32::free_bytes(&dir()).is_some_and(|bytes| bytes < cap + STOP_RECORDING_BELOW) {
        return Err("Not enough disk space for a replay checkpoint.".into());
    }
    let newest = live_recovery_files().into_iter().next();
    let slot = if newest.as_ref().is_some_and(|p| name(p) == "replay-a.mp4") {
        "replay-b.mp4"
    } else {
        "replay-a.mp4"
    };
    let destination = dir().join(slot);
    let partial = dir().join("replay.part.mp4");
    let rate = core.engine.current_settings().bitrate_kbps.max(1) as u64 * 125;
    let seconds = prefs.recovery_seconds.min(300).min((cap / 2 / rate).max(1));
    let result = core
        .engine
        .checkpoint_replay(&partial, seconds)
        .and_then(|()| {
            if fs::metadata(&partial).map_err(|e| e.to_string())?.len() > cap / 2 {
                return Err("Replay recovery checkpoint exceeds the storage limit.".into());
            }
            if destination.exists() {
                fs::remove_file(&destination).map_err(|e| e.to_string())?;
            }
            fs::rename(&partial, &destination).map_err(|e| e.to_string())?;
            if previous_replay().is_file() {
                for older in live_recovery_files()
                    .into_iter()
                    .filter(|path| path != &destination)
                {
                    let _ = fs::remove_file(older);
                }
            }
            Ok(())
        });
    let _ = fs::remove_file(partial);
    result
}

#[tauri::command(async)]
pub fn configure_recovery(core: tauri::State<Shared>, payload: (u64, u64)) -> Result<(), String> {
    let (seconds, mb) = payload;
    core.studio.edit(|prefs| {
        prefs.recovery_seconds = seconds.min(300);
        prefs.recovery_mb = mb.clamp(100, 5000);
    })
}

#[tauri::command(async)]
pub fn checkpoint_now(core: tauri::State<Shared>) -> Result<(), String> {
    let prefs = core.studio.prefs.lock().unwrap().clone();
    if prefs.recovery_seconds == 0 {
        return Err("Turn on replay recovery first.".into());
    }
    checkpoint(&core, &prefs)
}

#[tauri::command(async)]
pub fn restore_replay(core: tauri::State<Shared>) -> Result<String, String> {
    let _lock = core.studio.checkpoint_lock.lock().unwrap();
    let path = restorable_replay().ok_or("A recovery checkpoint hasn't been saved yet.")?;
    let clips = core.engine.clips_dir();
    fs::create_dir_all(&clips).map_err(|e| e.to_string())?;
    let file = import_file(&path, &clips, Some("Recovered replay"))?;
    if path == previous_replay() {
        let _ = fs::remove_file(path);
    }
    let _ = core.app.emit("studio-import", 1);
    Ok(file)
}

fn video_file(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| matches!(ext.to_ascii_lowercase().as_str(), "mp4" | "mkv" | "mov"))
}

fn import_file(source: &Path, folder: &Path, label: Option<&str>) -> Result<String, String> {
    if !video_file(source)
        || fs::symlink_metadata(source)
            .map_err(|e| e.to_string())?
            .file_type()
            .is_symlink()
    {
        return Err("Choose an MP4, MKV, or MOV file.".into());
    }
    // A recorder still writing the source cannot share this handle for reading.
    let mut input = OpenOptions::new()
        .read(true)
        .share_mode(1)
        .open(source)
        .map_err(|e| e.to_string())?;
    let stem = label.map(str::to_owned).unwrap_or_else(|| {
        source
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned()
    });
    let stem = clean_file_stem(&stem).unwrap_or_else(|_| "Imported clip".into());
    let ext = source.extension().unwrap_or_default().to_string_lossy();
    fs::create_dir_all(folder).map_err(|e| e.to_string())?;
    let scratch = folder.join(format!(".import-{:016x}.part", nonce()?));
    let mut output = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&scratch)
        .map_err(|e| e.to_string())?;
    let copied = std::io::copy(&mut input, &mut output);
    drop(output);
    let result = copied.map_err(|e| e.to_string()).and_then(|_| {
        for index in 1..=1000 {
            let file = if index == 1 {
                format!("{stem}.{ext}")
            } else {
                format!("{stem} ({index}).{ext}")
            };
            let file = folder.join(file);
            if file.exists() {
                continue;
            }
            fs::rename(&scratch, &file).map_err(|e| e.to_string())?;
            return Ok(file.to_string_lossy().into_owned());
        }
        Err("Too many imports with this filename.".into())
    });
    if result.is_err() {
        let _ = fs::remove_file(scratch);
    }
    result
}

fn external_source(core: &Core, path: &Path) -> bool {
    let Ok(path) = path.canonicalize() else {
        return false;
    };
    let output = PathBuf::from(&core.settings.lock().unwrap().output_dir);
    !output
        .canonicalize()
        .is_ok_and(|root| path.starts_with(&root) || root.starts_with(&path))
}

#[tauri::command(async)]
pub fn import_clips(
    core: tauri::State<Shared>,
    payload: Option<Vec<PathBuf>>,
) -> Result<Value, String> {
    let paths = if let Some(paths) = payload.filter(|_| api::is_test_instance()) {
        paths
    } else {
        let Some(paths) = core
            .app
            .dialog()
            .file()
            .add_filter("Video", &["mp4", "mkv", "mov"])
            .blocking_pick_files()
        else {
            return Ok(json!({ "count": 0, "errors": [] }));
        };
        paths
            .into_iter()
            .filter_map(|p| p.into_path().ok())
            .collect()
    };
    let mut errors = Vec::new();
    let mut count = 0;
    for path in paths {
        if !external_source(&core, &path) {
            errors.push(format!(
                "{} is already inside the capture folder.",
                name(&path)
            ));
            continue;
        }
        match import_file(&path, &core.engine.clips_dir(), None) {
            Ok(_) => count += 1,
            Err(error) => errors.push(format!("{}: {error}", name(&path))),
        }
    }
    if count > 0 {
        let _ = core.app.emit("studio-import", count);
    }
    Ok(json!({ "count": count, "errors": errors }))
}

#[tauri::command(async)]
pub fn set_watch_folder(
    core: tauri::State<Shared>,
    payload: (bool, Option<PathBuf>),
) -> Result<(), String> {
    let folder = if payload.0 {
        let chosen = payload.1.filter(|_| api::is_test_instance()).or_else(|| {
            core.app
                .dialog()
                .file()
                .blocking_pick_folder()
                .and_then(|p| p.into_path().ok())
        });
        let Some(path) = chosen else {
            return Ok(());
        };
        if !external_source(&core, &path) {
            return Err("Choose a folder separate from Afterclip's capture folder.".into());
        }
        Some(path)
    } else {
        None
    };
    core.studio.edit(|prefs| prefs.watch_folder = folder)
}

type Fingerprint = (u64, std::time::SystemTime);
#[derive(Default)]
struct ImportWatch {
    folder: Option<PathBuf>,
    imported: HashMap<PathBuf, Fingerprint>,
    pending: HashMap<PathBuf, (Fingerprint, Instant)>,
}

impl ImportWatch {
    fn scan(&mut self, core: &Shared, folder: Option<PathBuf>) {
        let changed = self.folder != folder;
        if changed {
            self.imported.clear();
            self.pending.clear();
            self.folder = folder.clone();
        }
        let Some(folder) = folder.filter(|path| external_source(core, path)) else {
            return;
        };
        let Ok(entries) = fs::read_dir(folder) else {
            return;
        };
        let mut count = 0;
        let files: HashMap<_, _> = entries
            .flatten()
            .filter_map(|entry| {
                let path = entry.path();
                if !entry.file_type().ok()?.is_file() || !video_file(&path) {
                    return None;
                }
                let meta = entry.metadata().ok()?;
                Some((path, (meta.len(), meta.modified().ok()?)))
            })
            .collect();
        self.imported.retain(|path, _| files.contains_key(path));
        self.pending.retain(|path, _| files.contains_key(path));
        for (path, fingerprint) in files {
            if changed {
                self.imported.insert(path, fingerprint);
                continue;
            }
            if self.imported.get(&path) == Some(&fingerprint) {
                continue;
            }
            let pending = self
                .pending
                .entry(path.clone())
                .or_insert((fingerprint, Instant::now()));
            if pending.0 != fingerprint {
                *pending = (fingerprint, Instant::now());
                continue;
            }
            if pending.1.elapsed() >= Duration::from_secs(6)
                && import_file(&path, &core.engine.clips_dir(), None).is_ok()
            {
                self.imported.insert(path.clone(), fingerprint);
                self.pending.remove(&path);
                count += 1;
            }
        }
        if count > 0 {
            let _ = core.app.emit("studio-import", count);
        }
    }
}

#[tauri::command(async)]
pub fn configure_bookmarks(
    core: tauri::State<Shared>,
    payload: (bool, String, Option<PathBuf>),
) -> Result<(), String> {
    let (enabled, player, test_log) = payload;
    let log = if enabled {
        let player = player.trim();
        if player.is_empty()
            || player.len() > 32
            || !player
                .bytes()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == b'_')
        {
            return Err("Enter the Minecraft Java player name first.".into());
        }
        let chosen = test_log.filter(|_| api::is_test_instance()).or_else(|| {
            core.app
                .dialog()
                .file()
                .add_filter("Minecraft log", &["log"])
                .blocking_pick_file()
                .and_then(|p| p.into_path().ok())
        });
        let Some(log) = chosen else {
            return Ok(());
        };
        Some(log)
    } else {
        None
    };
    core.studio.edit(|prefs| {
        prefs.minecraft_log = log;
        prefs.minecraft_player = player.trim().into();
    })
}

pub fn bookmark_file(recording: &Path) -> PathBuf {
    recording.with_extension("afterclip-bookmarks.json")
}

#[derive(Default)]
struct GameLog {
    path: Option<PathBuf>,
    offset: u64,
    partial: String,
}

impl GameLog {
    fn read(&mut self, path: Option<PathBuf>, player: &str) -> Vec<String> {
        let changed = path != self.path;
        self.path = path;
        let Some(path) = &self.path else {
            return vec![];
        };
        let Ok(mut log) = File::open(path) else {
            return vec![];
        };
        let length = log.metadata().map(|meta| meta.len()).unwrap_or(0);
        if changed || length < self.offset {
            self.offset = if changed { length } else { 0 };
            self.partial.clear();
        }
        self.offset = self.offset.max(length.saturating_sub(256 * 1024));
        if log.seek(SeekFrom::Start(self.offset)).is_err() {
            return vec![];
        }
        let mut bytes = Vec::new();
        if log.take(256 * 1024).read_to_end(&mut bytes).is_err() {
            return vec![];
        }
        self.offset += bytes.len() as u64;
        self.partial.push_str(&String::from_utf8_lossy(&bytes));
        let Some(end) = self.partial.rfind('\n') else {
            return vec![];
        };
        let rest = self.partial.split_off(end + 1);
        let lines = std::mem::replace(&mut self.partial, rest);
        lines
            .lines()
            .filter_map(|line| {
                let (_, message) = line.split_once("[CHAT] ")?;
                let event = message.strip_prefix(player)?.strip_prefix(' ')?;
                let useful = [
                    "has made the advancement",
                    "has reached the goal",
                    "has completed the challenge",
                    "was slain by",
                    "was shot by",
                    "fell from",
                    "blew up",
                    "drowned",
                ]
                .iter()
                .any(|prefix| event.starts_with(prefix));
                useful.then(|| message.chars().take(180).collect())
            })
            .collect()
    }
}

fn store_bookmarks(path: &Path, titles: Vec<String>) {
    if titles.is_empty() {
        return;
    }
    static BOOKMARK_WRITE: Mutex<()> = Mutex::new(());
    let _write = BOOKMARK_WRITE.lock().unwrap();
    let time = fs::metadata(path)
        .ok()
        .and_then(|meta| meta.created().ok())
        .and_then(|start| start.elapsed().ok())
        .map_or(0.0, |elapsed| elapsed.as_secs_f64());
    let file = bookmark_file(path);
    let mut marks = read_bookmarks(path);
    marks.extend(titles.into_iter().map(|title| Bookmark { time, title }));
    if let Ok(bytes) = serde_json::to_vec(&marks) {
        let _ = fs::write(file, bytes);
    }
}

const CS2_PORT: u16 = 4578;

#[tauri::command(async)]
pub fn configure_cs2(core: tauri::State<Shared>, payload: bool) -> Result<(), String> {
    if !payload {
        return core.studio.edit(|prefs| prefs.cs2_token = None);
    }
    let Some(target) = ask_save_path(
        &core,
        "gamestate_integration_afterclip.cfg".into(),
        "Counter-Strike 2 config",
        "cfg",
    ) else {
        return Ok(());
    };
    let previous = core.studio.prefs.lock().unwrap().cs2_token.clone();
    let token = match previous {
        Some(token) => token,
        None => format!("{:016x}{:016x}", nonce()?, nonce()?),
    };
    let cfg = format!(
        r#""Afterclip"
{{
    "uri" "http://127.0.0.1:{CS2_PORT}/cs2"
    "timeout" "2.0"
    "buffer" "0.1"
    "throttle" "0.3"
    "heartbeat" "15.0"
    "auth" {{ "token" "{token}" }}
    "data"
    {{
        "provider" "1"
        "map" "1"
        "round" "1"
        "player_id" "1"
        "player_match_stats" "1"
    }}
}}
"#
    );
    fs::write(target, cfg).map_err(|e| e.to_string())?;
    core.studio.edit(|prefs| prefs.cs2_token = Some(token))
}

#[derive(Default)]
struct Cs2Match {
    map: String,
    kills: Option<u64>,
    won_round: Option<u64>,
}

impl Cs2Match {
    fn events(&mut self, state: &Value) -> Vec<String> {
        let Some(steam) = state["provider"]["steamid"].as_str() else {
            return vec![];
        };
        if state["player"]["steamid"].as_str() != Some(steam) {
            return vec![];
        }
        let Some(map) = state["map"]["name"].as_str() else {
            return vec![];
        };
        let kills = state["player"]["match_stats"]["kills"].as_u64();
        let round = state["map"]["round"].as_u64();
        if self.map != map || self.kills.zip(kills).is_some_and(|(old, new)| new < old) {
            self.map = map.into();
            self.kills = kills;
            self.won_round = None;
            return vec![];
        }
        let mut events = Vec::new();
        if let Some(count) = self
            .kills
            .zip(kills)
            .and_then(|(old, new)| new.checked_sub(old))
        {
            if count == 1 {
                events.push("CS2 · Kill".into());
            }
            if count > 1 {
                events.push(format!("CS2 · {count} kills"));
            }
        }
        self.kills = kills.or(self.kills);
        if let Some(round) = round {
            let winner = state["round"]["win_team"].as_str();
            if self.won_round != Some(round)
                && state["round"]["phase"] == "over"
                && winner.is_some()
                && winner == state["player"]["team"].as_str()
            {
                self.won_round = Some(round);
                events.push(format!("CS2 · Won round {}", round.saturating_add(1)));
            }
        }
        events
    }
}

pub fn watch_cs2(core: Shared) {
    while !core.quitting.load(Ordering::Relaxed) {
        let enabled = core.studio.prefs.lock().unwrap().cs2_token.clone();
        let Some(token) = enabled else {
            thread::sleep(Duration::from_secs(1));
            continue;
        };
        let Ok(server) = Server::http((std::net::Ipv4Addr::LOCALHOST, CS2_PORT)) else {
            thread::sleep(Duration::from_secs(5));
            continue;
        };
        let mut game = Cs2Match::default();
        while !core.quitting.load(Ordering::Relaxed)
            && core.studio.prefs.lock().unwrap().cs2_token.as_deref() == Some(&token)
        {
            let Ok(Some(mut request)) = server.recv_timeout(Duration::from_millis(500)) else {
                continue;
            };
            if request.method() != &Method::Post || request.url() != "/cs2" {
                let _ = request.respond(Response::empty(404));
                continue;
            }
            let mut bytes = Vec::new();
            let parsed: Option<Value> = request
                .as_reader()
                .take(64 * 1024)
                .read_to_end(&mut bytes)
                .ok()
                .and_then(|_| serde_json::from_slice(&bytes).ok());
            let Some(state) =
                parsed.filter(|state| state["auth"]["token"].as_str() == Some(&token))
            else {
                let _ = request.respond(Response::empty(403));
                continue;
            };
            let _ = request.respond(Response::empty(204));
            let events = game.events(&state);
            if let Some(recording) = core.engine.active_recording() {
                store_bookmarks(&recording, events);
            }
        }
    }
}

fn read_bookmarks(path: &Path) -> Vec<Bookmark> {
    fs::read(bookmark_file(path))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

#[tauri::command(async)]
pub fn clip_bookmarks(
    core: tauri::State<Shared>,
    payload: String,
) -> Result<Vec<Bookmark>, String> {
    Ok(read_bookmarks(&own_media(&core, &payload)?))
}

fn nonce() -> Result<u64, String> {
    let mut bytes = [0; 8];
    getrandom::fill(&mut bytes).map_err(|e| e.to_string())?;
    Ok(u64::from_le_bytes(bytes))
}

fn header(key: &str, value: impl AsRef<str>) -> Header {
    Header::from_bytes(key, value.as_ref()).expect("valid HTTP header")
}

fn phone_response(request: tiny_http::Request, base: &str, clip: &File, ext: &str) {
    let url = request.url();
    if request.method() != &Method::Get {
        let _ = request.respond(Response::empty(405));
        return;
    }
    if url == base {
        let html = format!("<!doctype html><meta name=viewport content='width=device-width, initial-scale=1'><title>Afterclip</title><style>body{{background:#101019;color:#fff;font:1.1rem system-ui;margin:1rem}}video{{width:100%;max-height:75vh}}a{{color:#b9aaff}}</style><h1>Afterclip</h1><video controls playsinline preload=metadata src='{base}/video'></video><p><a href='{base}/download' download>Download clip</a></p>");
        let _ = request.respond(
            Response::from_string(html)
                .with_header(header("Content-Type", "text/html; charset=utf-8"))
                .with_header(header("Cache-Control", "no-store")),
        );
        return;
    }
    let download = url == format!("{base}/download");
    if !download && url != format!("{base}/video") {
        let _ = request.respond(Response::empty(404));
        return;
    }
    let Ok(mut file) = clip.try_clone() else {
        let _ = request.respond(Response::empty(404));
        return;
    };
    let length = file.metadata().map(|meta| meta.len()).unwrap_or(0);
    let range = request
        .headers()
        .iter()
        .find(|h| h.field.equiv("Range"))
        .map(|h| h.value.as_str());
    let parsed = range.and_then(|value| {
        let (start, end) = value.strip_prefix("bytes=")?.split_once('-')?;
        if length == 0 {
            return None;
        }
        let (start, end) = if start.is_empty() {
            (
                length.saturating_sub(end.parse::<u64>().ok()?.max(1)),
                length - 1,
            )
        } else {
            (
                start.parse::<u64>().ok()?,
                end.parse::<u64>().unwrap_or(length - 1).min(length - 1),
            )
        };
        (start <= end && end < length).then_some((start, end))
    });
    if range.is_some() && parsed.is_none() {
        let _ = request.respond(
            Response::empty(416).with_header(header("Content-Range", format!("bytes */{length}"))),
        );
        return;
    }
    let (start, count) = parsed.map_or((0, length), |(start, end)| (start, end - start + 1));
    if file.seek(SeekFrom::Start(start)).is_err() {
        let _ = request.respond(Response::empty(500));
        return;
    }
    let mime = match ext {
        "mkv" => "video/x-matroska",
        "mov" => "video/quicktime",
        _ => "video/mp4",
    };
    let mut headers = vec![
        header("Content-Type", mime),
        header("Accept-Ranges", "bytes"),
        header("Cache-Control", "no-store"),
    ];
    if let Some((start, end)) = parsed {
        headers.push(header(
            "Content-Range",
            format!("bytes {start}-{end}/{length}"),
        ));
    }
    if download {
        headers.push(header(
            "Content-Disposition",
            format!("attachment; filename=\"Afterclip.{ext}\""),
        ));
    }
    let response = Response::new(
        StatusCode(if parsed.is_some() { 206 } else { 200 }),
        headers,
        file.take(count),
        Some(count as usize),
        None,
    );
    let _ = request.respond(response);
}

#[tauri::command(async)]
pub fn phone_share(core: tauri::State<Shared>, payload: String) -> Result<String, String> {
    let path = own_media(&core, &payload)?;
    if !video_file(&path) || core.engine.active_recording().as_ref() == Some(&path) {
        return Err("Choose a finished video.".into());
    }
    let resolved = path.canonicalize().map_err(|e| e.to_string())?;
    let owned = [core.engine.clips_dir(), core.engine.recordings_dir()]
        .into_iter()
        .any(|dir| {
            dir.canonicalize()
                .is_ok_and(|root| resolved.starts_with(root))
        });
    if !owned {
        return Err("Choose a video inside the Afterclip library.".into());
    }
    let file = File::open(&resolved).map_err(|e| e.to_string())?;
    let extension = path
        .extension()
        .unwrap_or_default()
        .to_string_lossy()
        .to_ascii_lowercase();
    let ip = api::is_test_instance()
        .then_some(std::net::Ipv4Addr::LOCALHOST.into())
        .or_else(api::lan_address)
        .filter(|addr| addr.is_ipv4())
        .ok_or("Connect this PC and your phone to the same network first.")?;
    let server = Server::http((ip, TRANSFER_PORT)).map_err(|e| e.to_string())?;
    let address = server
        .server_addr()
        .to_ip()
        .ok_or("Couldn't start phone sharing.")?;
    let key = format!("/{:016x}{:016x}", nonce()?, nonce()?);
    let stop = Arc::new(AtomicBool::new(false));
    if let Some(old) = core
        .studio
        .transfer
        .lock()
        .unwrap()
        .replace(Arc::clone(&stop))
    {
        old.store(true, Ordering::Relaxed);
    }
    let url = format!("http://{address}{key}");
    thread::spawn(move || {
        let expires = Instant::now() + Duration::from_secs(600);
        while !stop.load(Ordering::Relaxed) && Instant::now() < expires {
            match server.recv_timeout(Duration::from_millis(250)) {
                Ok(Some(request)) => phone_response(request, &key, &file, &extension),
                Ok(None) => (),
                Err(_) => break,
            }
        }
    });
    Ok(url)
}

#[tauri::command]
pub fn stop_phone_share(core: tauri::State<Shared>) {
    if let Some(stop) = core.studio.transfer.lock().unwrap().take() {
        stop.store(true, Ordering::Relaxed);
    }
}

#[derive(Deserialize)]
pub struct Batch {
    files: Vec<String>,
    recipe: String,
}

#[tauri::command(async)]
pub fn enqueue_batch(core: tauri::State<Shared>, payload: Batch) -> Result<(), String> {
    if payload.files.is_empty() || payload.files.len() > 100 {
        return Err("Choose up to 100 clips.".into());
    }
    if !core
        .settings
        .lock()
        .unwrap()
        .share_recipes
        .iter()
        .any(|r| r.name == payload.recipe)
    {
        return Err("Choose a saved recipe.".into());
    }
    for file in &payload.files {
        if !video_file(&own_media(&core, file)?) {
            return Err("Select video clips only.".into());
        }
    }
    let mut jobs = core.studio.jobs.lock().unwrap();
    while jobs.len() + payload.files.len() > 200 {
        let Some(index) = jobs
            .iter()
            .position(|job| matches!(job.status, "done" | "failed"))
        else {
            return Err("The queue is full.".into());
        };
        jobs.remove(index);
    }
    for file in payload.files {
        let id = core.studio.next_job.fetch_add(1, Ordering::Relaxed);
        jobs.push_back(Job {
            id,
            file,
            recipe: payload.recipe.clone(),
            status: "waiting",
            detail: String::new(),
        });
    }
    Ok(())
}

#[tauri::command]
pub fn control_batch(core: tauri::State<Shared>, payload: (bool, bool, Option<u64>)) {
    core.studio.paused.store(payload.0, Ordering::Relaxed);
    core.studio
        .pause_in_game
        .store(payload.1, Ordering::Relaxed);
    if let Some(id) = payload.2 {
        if let Some(job) = core
            .studio
            .jobs
            .lock()
            .unwrap()
            .iter_mut()
            .find(|j| j.id == id && j.status == "failed")
        {
            job.status = "waiting";
            job.detail.clear();
        }
    }
}

pub fn watch_queue(core: Shared) {
    while !core.quitting.load(Ordering::Relaxed) {
        thread::sleep(Duration::from_millis(500));
        if core.studio.paused.load(Ordering::Relaxed)
            || (core.studio.pause_in_game.load(Ordering::Relaxed) && on_game_or_recording(&core))
        {
            continue;
        }
        let job = {
            let mut queue = core.studio.jobs.lock().unwrap();
            queue.iter_mut().find(|j| j.status == "waiting").map(|job| {
                job.status = "running";
                job.clone()
            })
        };
        let Some(job) = job else {
            continue;
        };
        let result = run_share(
            &core,
            ShareArgs {
                file: job.file,
                action: "recipe".into(),
                start: None,
                end: None,
                tracks: vec![],
                webhook: 0,
                even: None,
                max_mb: None,
                layout: None,
                recipe: Some(job.recipe),
            },
        );
        if let Some(entry) = core
            .studio
            .jobs
            .lock()
            .unwrap()
            .iter_mut()
            .find(|j| j.id == job.id)
        {
            match result {
                Ok(output) => {
                    entry.status = "done";
                    entry.detail = output.unwrap_or_else(|| "Sent to Discord".into());
                }
                Err(error) => {
                    entry.status = "failed";
                    entry.detail = error;
                }
            }
        }
    }
}

pub fn watch_tools(core: Shared) {
    let mut imports = ImportWatch::default();
    let mut log = GameLog::default();
    let mut count = 0;
    while !core.quitting.load(Ordering::Relaxed) {
        let prefs = core.studio.prefs.lock().unwrap().clone();
        imports.scan(&core, prefs.watch_folder.clone());
        let titles = log.read(prefs.minecraft_log.clone(), &prefs.minecraft_player);
        if let Some(recording) = core.engine.active_recording() {
            store_bookmarks(&recording, titles);
        }
        if prefs.recovery_seconds > 0 && core.engine.status().state == "error" {
            if let Ok(_lock) = core.studio.checkpoint_lock.try_lock() {
                preserve_previous();
            }
        }
        if prefs.recovery_seconds > 0 && count % 15 == 0 {
            let state = Arc::clone(&core);
            thread::spawn(move || {
                let _ = checkpoint(&state, &prefs);
            });
        }
        count += 1;
        thread::sleep(Duration::from_secs(2));
    }
}

#[tauri::command]
pub fn studio_state(core: tauri::State<Shared>) -> Value {
    let state = &core.studio;
    let recovery = restorable_replay()
        .and_then(|file| fs::metadata(file).ok())
        .map(|meta| modified_ms(&meta));
    let recipes: Vec<_> = core
        .settings
        .lock()
        .unwrap()
        .share_recipes
        .iter()
        .map(|r| r.name.clone())
        .collect();
    let prefs = state.prefs.lock().unwrap();
    let public = json!({ "recoverySeconds": prefs.recovery_seconds, "recoveryMb": prefs.recovery_mb,
        "watchFolder": prefs.watch_folder, "minecraftLog": prefs.minecraft_log, "minecraftPlayer": prefs.minecraft_player,
        "cs2Enabled": prefs.cs2_token.is_some() });
    json!({
        "prefs": public, "recovery": recovery, "recipes": recipes,
        "previousReplay": previous_replay().is_file(),
        "selection": *state.batch_selection.lock().unwrap(),
        "jobs": *state.jobs.lock().unwrap(), "paused": state.paused.load(Ordering::Relaxed),
        "pauseInGame": state.pause_in_game.load(Ordering::Relaxed),
    })
}

#[tauri::command(async)]
pub fn open_studio(core: tauri::State<Shared>, payload: Option<Vec<String>>) -> Result<(), String> {
    if let Some(files) = payload {
        *core.studio.batch_selection.lock().unwrap() = files.into_iter().take(100).collect();
    }
    if let Some(window) = core.app.get_webview_window(STUDIO) {
        let _ = window.show();
        let _ = window.set_focus();
        return Ok(());
    }
    let window = build_window(&core.app, STUDIO, "studio.html")
        .title("Afterclip · Tools")
        .inner_size(780.0, 650.0)
        .min_inner_size(560.0, 440.0)
        .build()
        .map_err(|e| e.to_string())?;
    recover_from_browser_crash(&core, &window);
    watch_startup(&core, STUDIO);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cs2_marks_local_kills_and_wins_once() {
        let mut game = Cs2Match::default();
        let packet = |player: &str, kills, phase: &str| {
            json!({
                "provider": { "steamid": "local" },
                "player": { "steamid": player, "team": "CT", "match_stats": { "kills": kills } },
                "map": { "name": "de_mirage", "round": 4 },
                "round": { "phase": phase, "win_team": "CT" },
            })
        };
        assert!(game.events(&packet("local", 4, "live")).is_empty());
        assert!(game.events(&packet("someone else", 7, "over")).is_empty());
        assert_eq!(
            game.events(&packet("local", 5, "over")),
            ["CS2 · Kill", "CS2 · Won round 5"]
        );
        assert!(game.events(&packet("local", 5, "over")).is_empty());
    }
}
