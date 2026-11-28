//! The clips, recordings and screenshots on disk: listing them, the storage rules, deleting and restoring, and
//! the previews made from them (thumbnails, scrub strips, loudness) and overlapping clips.

use super::*;

#[derive(Serialize)]
pub(crate) struct Media {
    pub(crate) file: String,
    pub(crate) name: String,
    pub(crate) size: u64,
    pub(crate) mtime: f64,
    pub(crate) kind: &'static str,
    pub(crate) favourite: bool,
    /// Only set for a renamed clip, whose file name no longer says.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) game: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) tags: Vec<String>,
}

// More than this is too much to send to the panel in one go.
pub(crate) const MAX_LISTED_MEDIA: usize = 2000;

pub(crate) fn modified_ms(meta: &fs::Metadata) -> f64 {
    meta.modified().ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as f64)
        .unwrap_or(0.0)
}

/// The files in a folder, and in the game folders inside it. Hidden folders such as .staging are left out.
pub(crate) fn files_in(dir: &Path) -> Vec<fs::DirEntry> {
    let mut found = Vec::new();
    let Ok(entries) = fs::read_dir(dir) else { return found };
    for entry in entries.flatten() {
        let hidden = entry.file_name().to_string_lossy().starts_with('.');
        match entry.file_type() {
            Ok(kind) if kind.is_dir() && !hidden => {
                let inside = fs::read_dir(entry.path()).into_iter().flatten().flatten();
                found.extend(inside.filter(|e| e.file_type().is_ok_and(|t| t.is_file())));
            }
            Ok(kind) if kind.is_file() => found.push(entry),
            _ => {}
        }
    }
    found
}

pub(crate) fn list_media(core: &Core) -> Vec<Media> {
    let active = core.engine.active_recording();
    let library = core.library.lock().unwrap();
    let mut media = Vec::new();
    const VIDEOS: &[&str] = &["mp4", "mkv", "mov"];
    for (dir, kind, extensions) in [(core.engine.clips_dir(), "clip", VIDEOS), (core.engine.recordings_dir(), "recording", VIDEOS), (core.engine.screenshots_dir(), "screenshot", &["png"])] {
        for entry in files_in(&dir) {
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
                tags: library.tags.get(&name).cloned().unwrap_or_default(),
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
pub(crate) fn last_clip(core: &Shared) -> Result<String, String> {
    list_media(core).into_iter().find(|m| m.kind == "clip").map(|m| m.file).ok_or_else(|| "No clips yet".to_string())
}

/// Applies the chosen age and size limits. Runs after saves, and regularly for recordings and for clips getting older.
/// Whether storage rules must leave this file alone: favourites always, and clips tagged by you when that's on.
pub(crate) fn kept_by_rules(media: &Media, keep_tagged: bool) -> bool {
    media.favourite || (keep_tagged && media.tags.iter().any(|t| t != AUTO_TAG))
}

pub(crate) fn enforce_storage_rules(core: &Shared) {
    let (limit_gb, days, keep_tagged) = {
        let settings = core.settings.lock().unwrap();
        (settings.storage_limit_gb, settings.delete_after_days, settings.keep_tagged)
    };
    if limit_gb <= 0.0 && days <= 0 {
        return;
    }
    let _one_at_a_time = core.storage_lock.lock().unwrap();
    let clips: Vec<StoredClip> = list_media(core).into_iter().map(|m| StoredClip {
        path: PathBuf::from(&m.file),
        size: m.size,
        modified: m.mtime,
        favourite: kept_by_rules(&m, keep_tagged),
    }).collect();
    let mut chosen = if days > 0 { older_than(&clips, days, now_ms()) } else { Vec::new() };
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

// Long enough after start that capture is up and the panel isn't waiting on the media tool.
pub(crate) const RETAG_DELAY: Duration = Duration::from_secs(60);
pub(crate) const RETAG_GAME_POLL: Duration = Duration::from_secs(30);
// Room left on the drive beyond the copy, so this can never be what fills it.
pub(crate) const RETAG_SPARE_BYTES: u64 = 5_000_000_000;

/// HEVC clips saved before 1.3.1 were tagged hev1, which iPhones won't play. This fixes them once, in the background
/// and never while a game is in front. Anything it couldn't do, such as a clip open in a player, is tried at the next start.
pub(crate) fn retag_old_clips(core: &Shared) {
    let done = core.settings_file.with_file_name("hvc1-retagged");
    if done.exists() {
        return;
    }
    thread::sleep(RETAG_DELAY);
    let gaming = || {
        let settings = core.settings.lock().unwrap().clone();
        game_in_front(core, &settings)
    };
    let mut finished = true;
    let (mut fixed, mut checked) = (0, 0);
    for media in list_media(core) {
        let path = PathBuf::from(&media.file);
        // FFmpeg writes this copy as MP4, so only MP4 and MOV files are touched.
        let mp4 = path.extension().and_then(|e| e.to_str()).is_some_and(|e| ["mp4", "mov"].contains(&e.to_lowercase().as_str()));
        if media.kind == "screenshot" || !mp4 {
            continue;
        }
        while gaming() && !core.quitting.load(Ordering::SeqCst) {
            thread::sleep(RETAG_GAME_POLL);
        }
        if core.quitting.load(Ordering::SeqCst) {
            return;
        }
        let Ok(before) = fs::metadata(&path) else { continue };
        if path.parent().and_then(win32::free_bytes).is_some_and(|free| free < before.len() + RETAG_SPARE_BYTES) {
            core.log_ui(&format!("retag skipped {}: not enough free space", path.display()));
            finished = false;
            continue;
        }
        checked += 1;
        match core.engine.retag(&path).and_then(|copy| copy.map_or(Ok(false), |copy| replace_keeping_dates(core, &path, &before, &copy).map(|_| true))) {
            Ok(true) => fixed += 1,
            Ok(false) => {}
            Err(e) => {
                core.log_ui(&format!("retag could not fix {}: {e}", path.display()));
                finished = false;
            }
        }
    }
    core.log_ui(&format!("retag checked {checked} videos and fixed {fixed}"));
    if finished {
        let _ = fs::write(&done, "");
    }
}

/// Puts a fixed copy in place of the original, with the original's dates, so the library order and
/// cached thumbnails stay as they were. If the original changed or went meanwhile, the copy goes instead.
pub(crate) fn replace_keeping_dates(core: &Core, original: &Path, before: &fs::Metadata, copy: &Path) -> Result<(), String> {
    use std::os::windows::fs::FileTimesExt;
    let replaced = (|| {
        let mut times = fs::FileTimes::new().set_modified(before.modified().map_err(|e| e.to_string())?);
        if let Ok(created) = before.created() {
            times = times.set_created(created);
        }
        fs::OpenOptions::new().write(true).open(copy).and_then(|f| f.set_times(times)).map_err(|e| e.to_string())?;
        // The same locks as deleting, in the same order, so a clip can't be deleted or read halfway through this.
        let _no_thumbnail = core.thumbnails.exclusive();
        let _no_preview = core.strips.exclusive();
        let now =fs::metadata(original).map_err(|_| "it was deleted or renamed".to_string())?;
        if now.len() != before.len() || now.modified().ok() != before.modified().ok() {
            return Err("it changed while being fixed".to_string());
        }
        fs::rename(copy, original).map_err(|e| format!("it may be open in another app ({e})"))
    })();
    if replaced.is_err() {
        let _ = fs::remove_file(copy);
    }
    replaced
}

/// Sends a clip to the Recycle Bin, and forgets what was known about it.
/// Moves a file to the Recycle Bin. It runs on a fresh thread, because the Recycle Bin needs COM set up one way,
/// panics if a thread already has it set up another, and a shared thread may have been set up for anything.
pub(crate) fn recycle(path: &Path) -> Result<(), String> {
    let path = path.to_path_buf();
    thread::spawn(move || trash::delete(path).map_err(|e| e.to_string()))
        .join()
        .unwrap_or_else(|_| Err("the Recycle Bin couldn't be reached".into()))
}

pub(crate) fn remove_media(core: &Core, path: &Path) -> Result<(), String> {
    // A thumbnail or preview being made has the file open, which would stop it moving.
    let deleted = {
        let _no_thumbnail = core.thumbnails.exclusive();
        let _no_preview = core.strips.exclusive();
        recycle(path)
    };
    // Something else, such as Explorer's preview, may let go a moment later.
    if deleted.is_err() {
        thread::sleep(Duration::from_millis(500));
        recycle(path).map_err(|e| format!("Couldn't delete this file. It may be open in another app. ({e})"))?;
    }
    let bookmarks = studio::bookmark_file(path);
    if bookmarks.is_file() { let _ = recycle(&bookmarks); }
    // A game folder goes once it's empty. Removing a folder with anything in it fails, which is what's wanted.
    if let Some(folder) = path.parent().filter(|f| in_media_folders(core, f.parent())) {
        let _ = fs::remove_dir(folder);
    }
    let name = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
    forget_thumbnails(core, &name);
    let mut library = core.library.lock().unwrap();
    let remembered = DeletedMedia {
        path: path.to_path_buf(),
        favourite: library.favourites.contains(&name),
        game: library.games.get(&name).cloned(),
        tags: library.tags.get(&name).cloned(),
        camera: library.cameras.get(&name).copied(),
    };
    {
        let mut deleted = core.deleted.lock().unwrap();
        deleted.retain(|d| d.path != path);
        deleted.push(remembered);
        let extra = deleted.len().saturating_sub(MAX_REMEMBERED_DELETES);
        deleted.drain(..extra);
    }
    library.forget(&name);
    library.save(&core.library_file)
}

pub(crate) fn same_path(a: &Path, b: &Path) -> bool {
    a.to_string_lossy().to_lowercase() == b.to_string_lossy().to_lowercase()
}

/// Puts files deleted from Afterclip back where they were, from the Recycle Bin, with their favourite, game and tags.
pub(crate) fn restore_media(core: &Core, files: &[String]) -> Result<usize, String> {
    let wanted: Vec<DeletedMedia> = {
        let mut deleted = core.deleted.lock().unwrap();
        let (found, rest): (Vec<_>, Vec<_>) = deleted.drain(..).partition(|d| files.iter().any(|f| same_path(&d.path, Path::new(f))));
        *deleted = rest;
        found
    };
    if wanted.is_empty() {
        return Err("That can't be undone anymore. It's still in the Recycle Bin.".into());
    }
    let binned = trash::os_limited::list().map_err(|e| format!("Couldn't read the Recycle Bin. ({e})"))?;
    let mut restored = 0;
    let mut problems = Vec::new();
    for item in &wanted {
        let bookmarks = studio::bookmark_file(&item.path);
        // The newest copy, in case an older file of the same name was deleted before.
        let newest = |path: &Path| binned.iter().filter(|b| same_path(&b.original_path(), path)).max_by_key(|b| b.time_deleted).cloned();
        let Some(file) = newest(&item.path) else {
            problems.push(format!("{} is no longer in the Recycle Bin", item.path.display()));
            continue;
        };
        if item.path.exists() {
            problems.push(format!("{} already exists", item.path.display()));
            continue;
        }
        if let Some(folder) = item.path.parent() {
            let _ = fs::create_dir_all(folder);
        }
        if let Err(e) = trash::os_limited::restore_all([file]) {
            problems.push(format!("{} ({e})", item.path.display()));
            continue;
        }
        if let Some(marks) = newest(&bookmarks).filter(|_| !bookmarks.exists()) {
            let _ = trash::os_limited::restore_all([marks]);
        }
        let name = item.path.file_name().unwrap_or_default().to_string_lossy().into_owned();
        let mut library = core.library.lock().unwrap();
        if item.favourite {
            library.favourites.insert(name.clone());
        }
        if let Some(game) = &item.game {
            library.games.insert(name.clone(), game.clone());
        }
        if let Some(tags) = &item.tags {
            library.tags.insert(name.clone(), tags.clone());
        }
        if let Some(camera) = item.camera {
            library.cameras.insert(name, camera);
        }
        restored += 1;
    }
    core.library.lock().unwrap().save(&core.library_file)?;
    if problems.is_empty() {
        Ok(restored)
    } else {
        Err(format!("Couldn't put back {}: {}", problems.len(), problems.join("; ")))
    }
}

pub(crate) fn forget_thumbnails(core: &Core, name: &str) {
    let prefix = format!("{name}-");
    if let Ok(entries) = fs::read_dir(core.engine.thumbs_dir()) {
        for entry in entries.flatten() {
            if entry.file_name().to_string_lossy().starts_with(&prefix) {
                let _ = fs::remove_file(entry.path());
            }
        }
    }
}

/// Whether this is Clips, Recordings or Screenshots itself.
pub(crate) fn in_media_folders(core: &Core, folder: Option<&Path>) -> bool {
    [core.engine.clips_dir(), core.engine.recordings_dir(), core.engine.screenshots_dir()].iter().any(|dir| folder == Some(dir.as_path()))
}

/// The UI only gets to act on files inside our own output folders, or a game folder inside one.
pub(crate) fn own_media(core: &Core, file: &str) -> Result<PathBuf, String> {
    let path = PathBuf::from(file);
    let traversal = path.components().any(|c| matches!(c, Component::ParentDir));
    let parent = path.parent();
    let game_folder = parent.filter(|p| !p.file_name().unwrap_or_default().to_string_lossy().starts_with('.'));
    let own_folder = in_media_folders(core, parent) || in_media_folders(core, game_folder.and_then(Path::parent));
    if traversal || !own_folder {
        return Err("Not a Afterclip file".into());
    }
    Ok(path)
}

pub(crate) const SWEEP_THUMBNAILS_AFTER: Duration = Duration::from_secs(120);
pub(crate) const SWEEP_THUMBNAILS_EVERY: Duration = Duration::from_secs(24 * 60 * 60);

/// How many previews of one kind are made at once: a few on the desktop, one during a game.
pub(crate) fn previews_at_once(core: &Core) -> usize {
    if running_game(core).is_some() { 1 } else { previews::AT_ONCE }
}

/// Deletes cached previews of clips that have gone or changed, such as ones deleted in File Explorer.
pub(crate) fn sweep_thumbnails(core: &Core) {
    let current: std::collections::HashSet<String> = list_media(core).iter()
        .map(|m| format!("{}-{}", m.name, m.mtime as u64))
        .collect();
    let _no_thumbnail = core.thumbnails.exclusive();
    let _no_preview = core.strips.exclusive();
    let removed = previews::sweep(&core.engine.thumbs_dir(), &current);
    if removed > 0 {
        core.log_ui(&format!("removed {removed} cached previews of clips that are gone"));
    }
}

/// A cached image, inside the answer itself. Loading it by file link from the asset protocol instead made
/// WebView2's browser crash after rebuilding windows from an earlier crash, so pictures stay inline.
fn jpeg_url(image: &Path) -> Option<String> {
    use base64::Engine as _;
    let bytes = fs::read(image).ok()?;
    Some(format!("data:image/jpeg;base64,{}", base64::engine::general_purpose::STANDARD.encode(bytes)))
}

/// Returns a cached thumbnail and duration. They're made a few at a time, only for cards on screen, and ones
/// already made are returned straight away rather than waiting.
#[tauri::command(async)]
pub fn thumbnail(core: tauri::State<Shared>, payload: String) -> Option<Value> {
    let path = own_media(&core, &payload).ok()?;
    let mtime = modified_ms(&fs::metadata(&path).ok()?) as u64;
    let name = path.file_name()?.to_string_lossy().into_owned();
    let image = core.engine.thumbs_dir().join(format!("{name}-{mtime}.jpg"));
    let length = core.engine.thumbs_dir().join(format!("{name}-{mtime}.txt"));
    // A picture has no length to show.
    let picture = path.extension().is_some_and(|e| e.eq_ignore_ascii_case("png"));
    let cached_duration = || {
        fs::read_to_string(&length).ok().and_then(|t| t.trim().parse::<f64>().ok())
            .or(picture.then_some(0.0))
    };
    // The media tool writes thumbnails under a temporary name and renames them, so one that exists is whole.
    let ready = |duration: f64| jpeg_url(&image).map(|url| json!({ "url": url, "duration": duration }));
    if image.exists() {
        if let Some(duration) = cached_duration() {
            return ready(duration);
        }
    }
    let _few_at_a_time = core.thumbnails.enter(&image, previews_at_once(&core));
    if !image.exists() {
        fs::create_dir_all(core.engine.thumbs_dir()).ok()?;
        core.engine.make_thumbnail(&path, &image).ok()?;
    }
    let duration = match cached_duration() {
        Some(seconds) => seconds,
        None => {
            let seconds = core.engine.duration(&path).ok()?;
            let _ = fs::write(&length, seconds.to_string());
            seconds
        }
    };
    ready(duration)
}

/// A video's length, from the thumbnail cache when it's there, or read once and kept there.
pub(crate) fn clip_seconds(core: &Core, path: &Path) -> Option<f64> {
    let mtime = modified_ms(&fs::metadata(path).ok()?) as u64;
    let name = path.file_name()?.to_string_lossy().into_owned();
    let length = core.engine.thumbs_dir().join(format!("{name}-{mtime}.txt"));
    if let Some(seconds) = fs::read_to_string(&length).ok().and_then(|t| t.trim().parse::<f64>().ok()) {
        return Some(seconds);
    }
    let seconds = core.engine.duration(path).ok()?;
    let _ = fs::create_dir_all(core.engine.thumbs_dir());
    let _ = fs::write(&length, seconds.to_string());
    Some(seconds)
}

// Levels drawn across the editor's trim bar.
pub(crate) const WAVE_POINTS: usize = 400;
// The media tool's audio extract is 16 kHz mono, and stops after 10 minutes.
pub(crate) const WAVE_RATE: usize = 16000;
pub(crate) const WAVE_FLOOR_DB: f64 = -50.0;

/// How loud each part of 16-bit mono audio is, from 0 (silent or below the floor) to 1 (full scale).
pub(crate) fn loudness_levels(samples: &[i16], points: usize) -> Vec<f32> {
    if samples.is_empty() || points == 0 {
        return Vec::new();
    }
    let per = samples.len().div_ceil(points).max(1);
    samples.chunks(per).map(|chunk| {
        let mean = chunk.iter().map(|&s| (s as f64 / 32768.0).powi(2)).sum::<f64>() / chunk.len() as f64;
        let db = if mean > 0.0 { 10.0 * mean.log10() } else { WAVE_FLOOR_DB };
        (((db - WAVE_FLOOR_DB) / -WAVE_FLOOR_DB).clamp(0.0, 1.0) * 1000.0).round() as f32 / 1000.0
    }).collect()
}

/// The main mix's loudness across a video, for the editor to draw under the trim handles, and how many
/// seconds it covers. Made once in the background, then kept with the thumbnails.
#[tauri::command(async)]
pub fn clip_loudness(core: tauri::State<Shared>, payload: String) -> Result<Value, String> {
    let path = own_media(&core, &payload)?;
    let mtime = modified_ms(&fs::metadata(&path).map_err(|e| e.to_string())?) as u64;
    let name = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
    let cached = core.engine.thumbs_dir().join(format!("{name}-{mtime}-wave.json"));
    if let Some(found) = fs::read_to_string(&cached).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()) {
        return Ok(found);
    }
    // Shares the scrub strips' gate: both are previews made in the background.
    let _few_at_a_time = core.strips.enter(&cached, previews_at_once(&core));
    if let Some(found) = fs::read_to_string(&cached).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()) {
        return Ok(found);
    }
    fs::create_dir_all(core.engine.thumbs_dir()).map_err(|e| e.to_string())?;
    let wav = core.engine.thumbs_dir().join(format!("{name}-{mtime}-wave.wav"));
    let made = core.engine.wave(&path, &wav, &[0]).and_then(|()| fs::read(&wav).map_err(|e| e.to_string()));
    let _ = fs::remove_file(&wav);
    let bytes = made?;
    // A plain 44-byte header, as the media tool writes it, then the samples.
    let samples: Vec<i16> = bytes.get(44..).unwrap_or_default().chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]])).collect();
    let result = json!({
        "levels": loudness_levels(&samples, WAVE_POINTS),
        "seconds": samples.len() as f64 / WAVE_RATE as f64,
    });
    let _ = fs::write(&cached, result.to_string());
    Ok(result)
}

// The longest a clip can be is the longest buffer, so clips saved further apart than this can't overlap.
pub(crate) const OVERLAP_REACH_SECONDS: f64 = 1800.0 + 60.0;

/// Clips saved so close together that they show the same moment, with the shorter one suggested to go.
#[tauri::command(async)]
pub fn find_overlaps(core: tauri::State<Shared>) -> Vec<Value> {
    let (kept, favourites) = {
        let library = core.library.lock().unwrap();
        (library.kept_overlaps.clone(), library.favourites.clone())
    };
    let mut timed: Vec<(Media, f64)> = list_media(&core).into_iter()
        .filter(|m| m.kind == "clip")
        .filter_map(|m| overlaps::saved_at(&m.name).map(|at| (m, at)))
        .collect();
    timed.sort_by(|a, b| a.1.total_cmp(&b.1));
    // Only clips with a neighbour close enough get their length read, so a tidy library costs nothing.
    let near = |i: usize| {
        let at = timed[i].1;
        (i > 0 && at - timed[i - 1].1 <= OVERLAP_REACH_SECONDS) || timed.get(i + 1).is_some_and(|next| next.1 - at <= OVERLAP_REACH_SECONDS)
    };
    let files: std::collections::HashMap<String, String> = timed.iter().map(|(m, _)| (m.name.clone(), m.file.clone())).collect();
    let clips: Vec<overlaps::SavedClip> = (0..timed.len()).filter(|&i| near(i)).filter_map(|i| {
        let (m, at) = &timed[i];
        Some(overlaps::SavedClip { name: m.name.clone(), ends: *at, seconds: clip_seconds(&core, Path::new(&m.file))?, favourite: favourites.contains(&m.name) })
    }).collect();
    overlaps::find(clips).into_iter()
        .filter(|o| !kept.contains(&overlaps::pair_key(&o.keep, &o.covered)))
        .map(|o| json!({
            "keep": { "name": o.keep, "file": files.get(&o.keep) },
            "covered": { "name": o.covered, "file": files.get(&o.covered) },
            "sharedSeconds": o.shared_seconds.round(),
        }))
        .collect()
}

#[derive(Deserialize)]
pub(crate) struct OverlapArgs {
    pub(crate) keep: String,
    pub(crate) covered: String,
}

/// Remembers that someone wants both clips of a pair, so it isn't suggested again.
#[tauri::command(async)]
pub fn keep_overlap(core: tauri::State<Shared>, payload: OverlapArgs) -> Result<(), String> {
    let mut library = core.library.lock().unwrap();
    library.kept_overlaps.insert(overlaps::pair_key(&payload.keep, &payload.covered));
    library.save(&core.library_file)
}

/// A row of frames across a video, to scrub through on hover. Made once, then kept with the thumbnails.
#[tauri::command(async)]
pub fn preview_strip(core: tauri::State<Shared>, payload: String) -> Option<Value> {
    let path = own_media(&core, &payload).ok()?;
    if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("png")) {
        return None;
    }
    let mtime = modified_ms(&fs::metadata(&path).ok()?) as u64;
    let name = path.file_name()?.to_string_lossy().into_owned();
    let image = core.engine.thumbs_dir().join(format!("{name}-{mtime}-strip.jpg"));
    // One already made doesn't wait behind one being made for another clip.
    if !image.exists() {
        let _few_at_a_time = core.strips.enter(&image, previews_at_once(&core));
        if !image.exists() {
            fs::create_dir_all(core.engine.thumbs_dir()).ok()?;
            core.engine.make_preview_strip(&path, &image).ok()?;
        }
    }
    Some(json!({ "url": jpeg_url(&image)?, "frames": engine::PREVIEW_FRAMES }))
}
