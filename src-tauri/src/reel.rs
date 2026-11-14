//! The weekly reel: once a week, the week's favourites, or clips with a chosen tag, joined into one video.
//! Joining copies the clips without re-encoding, so it takes seconds, and it only runs while no game is open.
use super::*;
use std::collections::BTreeMap;

const CHECK_EVERY: Duration = Duration::from_secs(10 * 60);
const FIRST_CHECK_AFTER: Duration = Duration::from_secs(2 * 60);
const WEEK_MS: f64 = 7.0 * 24.0 * 60.0 * 60.0 * 1000.0;
/// Each clip's moment is near its end, because clips save what just happened, so the reel keeps each one's end.
const SECONDS_PER_CLIP: f64 = 30.0;
/// The newest clips are kept when a week has more than this.
const MOST_SECONDS: f64 = 10.0 * 60.0;
pub const REEL_TAG: &str = "reel";

fn stamp_file() -> PathBuf {
    home_dir().join("weekly-reel.txt")
}

/// When the last reel was made, in milliseconds since 1970.
fn last_made() -> f64 {
    fs::read_to_string(stamp_file()).ok().and_then(|text| text.trim().parse().ok()).unwrap_or(0.0)
}

fn remember_made() {
    let _ = fs::create_dir_all(home_dir());
    let _ = fs::write(stamp_file(), format!("{}", now_ms()));
}

/// What a weekly reel was made from.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Reel {
    file: String,
    clips: usize,
    seconds: f64,
}

/// The clips a reel would use: this week's, oldest first, that are favourites or carry the tag.
fn wanted_clips(core: &Core, tag: &str) -> Vec<Media> {
    let since = now_ms() - WEEK_MS;
    let mut clips: Vec<Media> = list_media(core)
        .into_iter()
        .filter(|m| m.kind == "clip" && m.mtime >= since)
        .filter(|m| !m.tags.iter().any(|t| t.eq_ignore_ascii_case(REEL_TAG)))
        .filter(|m| if tag.is_empty() { m.favourite } else { m.tags.iter().any(|t| t.eq_ignore_ascii_case(tag)) })
        .collect();
    clips.sort_by(|a, b| a.mtime.total_cmp(&b.mtime));
    clips
}

/// Makes the reel, or returns None when the week has fewer than two clips that can go together.
pub fn make(core: &Shared) -> Result<Option<Reel>, String> {
    let tag = core.settings.lock().unwrap().weekly_reel_tag.clone();
    // Lossless joining needs matching video settings, so the reel uses the largest group of clips that match.
    let mut groups: BTreeMap<String, Vec<(PathBuf, f64)>> = BTreeMap::new();
    for clip in wanted_clips(core, &tag) {
        let path = PathBuf::from(&clip.file);
        if let Ok((signature, seconds)) = core.engine.probe(&path) {
            if seconds > 0.5 {
                groups.entry(signature).or_default().push((path, seconds));
            }
        }
    }
    let Some(group) = groups.into_values().max_by_key(|g| g.len()).filter(|g| g.len() >= 2) else {
        return Ok(None);
    };
    // The newest clips, back to the time limit, then put in order.
    let mut segments = Vec::new();
    let mut total = 0.0;
    for (path, seconds) in group.into_iter().rev() {
        let start = (seconds - SECONDS_PER_CLIP).max(0.0);
        if total + (seconds - start) > MOST_SECONDS && !segments.is_empty() {
            break;
        }
        total += seconds - start;
        segments.push((path, start, seconds));
    }
    segments.reverse();
    let clips_dir = core.engine.clips_dir();
    fs::create_dir_all(&clips_dir).map_err(|e| format!("Could not create {}: {e}", clips_dir.display()))?;
    let file = engine::unique_path(&clips_dir.join(format!("Weekly reel {}.mp4", chrono::Local::now().format("%Y-%m-%d"))));
    core.engine.join_in_background(&file, &segments)?;
    {
        let name = file.file_name().unwrap_or_default().to_string_lossy().into_owned();
        let mut library = core.library.lock().unwrap();
        library.set_tags(&name, &[REEL_TAG.to_string()]);
        let _ = library.save(&core.library_file);
    }
    Ok(Some(Reel { file: file.to_string_lossy().into_owned(), clips: segments.len(), seconds: total }))
}

fn announce(core: &Shared, reel: &Reel) {
    let total = reel.seconds.round() as i64;
    if core.settings.lock().unwrap().toast_enabled {
        win32::show_toast(&format!("Your week: {} clips, {}:{:02}. The reel is in your clips.", reel.clips, total / 60, total % 60));
    }
    let _ = core.app.emit("feedback", json!({ "kind": "reel", "ok": true, "file": reel.file, "clips": reel.clips, "seconds": reel.seconds }));
}

/// Makes a reel once a week, at the first moment no game is open or recording.
pub fn watch(core: Shared) {
    thread::sleep(FIRST_CHECK_AFTER);
    while !core.quitting.load(Ordering::SeqCst) {
        let wanted = core.settings.lock().unwrap().weekly_reel;
        if wanted && !on_game_or_recording(&core) && now_ms() - last_made() >= WEEK_MS {
            // Remembered first, so a reel that fails isn't tried again every few minutes.
            remember_made();
            match make(&core) {
                Ok(Some(reel)) => {
                    core.log_ui(&format!("made the weekly reel from {} clips", reel.clips));
                    announce(&core, &reel);
                }
                Ok(None) => core.log_ui("not enough clips this week for a weekly reel"),
                Err(e) => core.log_ui(&format!("the weekly reel could not be made: {e}")),
            }
        }
        thread::sleep(CHECK_EVERY);
    }
}

/// Make one now, from Settings.
#[tauri::command(async)]
pub fn make_weekly_reel(core: tauri::State<Shared>) -> Result<Option<Reel>, String> {
    if on_game_or_recording(&core) {
        return Err("Close your game, or stop recording, first.".into());
    }
    let reel = make(&core)?;
    if let Some(reel) = &reel {
        remember_made();
        announce(&core, reel);
    }
    Ok(reel)
}
