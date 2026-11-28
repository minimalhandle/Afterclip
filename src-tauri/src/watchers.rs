//! Background loops that keep an eye on things while Afterclip runs: games opening and closing, free space and
//! memory, push-to-talk, loud moments, and the engine's health.

use super::*;

// A game's profile stays on this long after it closes, so its last moments are still in the buffer to clip.
pub(crate) fn profile_hold() -> Duration {
    if api::is_test_instance() { Duration::from_secs(3) } else { Duration::from_secs(10 * 60) }
}
pub(crate) const GAME_WATCH_EVERY: Duration = Duration::from_secs(2);
// Polls in a row with no game before an automatic recording stops, so a brief alt-tab or restart doesn't end it.
pub(crate) const GAME_GONE_POLLS: u32 = 3;
pub(crate) const STORAGE_CHECK_POLLS: u32 = 30;

/// Whether a game is in front right now, for when capture is stopped and can't say.
pub(crate) fn game_in_front(core: &Core, settings: &Settings) -> bool {
    let added: Vec<String> = settings.game_profiles.iter().map(|p| p.exe.clone()).collect();
    core.test_game.lock().unwrap().is_some() || win32::foreground_game_app(&added).is_some()
}

/// Stops a recording before the drive fills, and warns once when space gets low.
pub(crate) fn check_free_space(core: &Shared, settings: &Settings, recording: bool, warned: &mut bool) {
    let folder = PathBuf::from(&settings.output_dir);
    let Some(existing) = folder.ancestors().find(|p| p.exists()) else { return };
    let tested = *core.test_free_bytes.lock().unwrap();
    let Some(free) = tested.or_else(|| win32::free_bytes(existing)) else { return };
    let gb = |bytes: u64| (bytes as f64 / 1e9 * 10.0).floor() / 10.0;
    if recording && free < STOP_RECORDING_BELOW && core.toggle_record().is_ok() {
        let message = format!("Recording stopped and saved, because your drive has only {} GB free.", gb(free));
        if settings.toast_enabled {
            win32::show_toast(&message);
        }
        let _ = core.app.emit("feedback", json!({ "kind": "low-space", "ok": false, "message": message }));
    }
    let warn_below = (settings.low_space_warn_gb * 1e9) as u64;
    if warn_below > 0 && free < warn_below {
        if !*warned {
            *warned = true;
            let message = format!("Your drive has only {} GB free. Clips and recordings may fail when it fills up.", gb(free));
            if settings.toast_enabled {
                win32::show_toast(&format!("Low disk space: {} GB left", gb(free)));
            }
            let _ = core.app.emit("feedback", json!({ "kind": "low-space", "ok": false, "message": message }));
        }
    } else if free > warn_below + warn_below / 10 {
        // A little above the line, so hovering around it doesn't warn again and again.
        *warned = false;
    }
}

/// Lets the mic through only while the push-to-talk key is held. It reads the key's state, which never stops
/// the game from getting the press.
pub(crate) fn watch_push_to_talk(core: Shared) {
    let mut held = false;
    loop {
        let (on, key) = {
            let s = core.settings.lock().unwrap();
            (s.push_to_talk, s.push_to_talk_key.clone())
        };
        let code = if on { win32::virtual_key(&key) } else { None };
        let tested = *core.test_talk.lock().unwrap();
        // The Stream Deck's Hold to Talk key works alongside the keyboard or mouse one.
        let deck = on && core.deck_talking.load(Ordering::SeqCst);
        let now = deck || code.is_some_and(|code| tested.unwrap_or_else(|| win32::key_held(code)));
        if now != held {
            held = now;
            core.talking.store(now, Ordering::SeqCst);
            if core.engine.status().buffering {
                let settings = core.settings.lock().unwrap().clone();
                let _ = core.engine.set_mic_muted(core.mic_silenced(&settings));
            }
        }
        thread::sleep(if code.is_some() { PUSH_TO_TALK_POLL } else { PUSH_TO_TALK_IDLE });
    }
}

pub(crate) const MEMORY_CHECK_EVERY: Duration = Duration::from_secs(5);
// Two low readings in a row, so a moment's spike while a game loads doesn't count.
pub(crate) const LOW_MEMORY_READINGS: u32 = 2;
// Free memory below the larger of these is low. Windows starts paging the game out not far below it.
pub(crate) const LOW_MEMORY_BYTES: u64 = 2_500_000_000;
pub(crate) const LOW_MEMORY_SHARE: f64 = 0.12;
// A buffer smaller than this frees too little to be worth moving.
pub(crate) const MOVE_BUFFER_FROM_MB: i64 = 500;
pub(crate) const MOVE_RETRY_AFTER: Duration = Duration::from_secs(60);

/// During a game, when memory runs low, moves the buffer to disk so the game keeps its memory. Nothing in the
/// buffer is lost, and it goes back into RAM the next time capture restarts, such as when the game closes.
pub(crate) fn watch_memory(core: Shared) {
    let mut low_readings = 0;
    while !core.quitting.load(Ordering::SeqCst) {
        thread::sleep(MEMORY_CHECK_EVERY);
        // Files on disk are also checked here, so none outlives the buffer by more than a few seconds.
        if core.engine.buffer_on_disk() {
            core.engine.prune_disk();
        }
        let status = core.engine.status();
        let capture = core.engine.current_settings();
        let worth_it = core.settings.lock().unwrap().free_memory_in_games
            && status.buffering
            && !status.buffer_on_disk
            && running_game(&core).is_some()
            && estimate_buffer_mb(capture.buffer_seconds, capture.bitrate_kbps) >= MOVE_BUFFER_FROM_MB;
        let low = worth_it && win32::memory_status().is_some_and(|(free, total)| free < LOW_MEMORY_BYTES.max((total as f64 * LOW_MEMORY_SHARE) as u64));
        low_readings = if low { low_readings + 1 } else { 0 };
        if low_readings < LOW_MEMORY_READINGS {
            continue;
        }
        low_readings = 0;
        match core.engine.move_buffer_to_disk() {
            Ok(()) => {
                let freed = estimate_buffer_mb(capture.buffer_seconds, capture.bitrate_kbps);
                core.log_ui(&format!("memory was low during a game, so the buffer moved to disk, freeing about {freed} MB"));
                let message = "Memory was low, so the buffer moved to disk. Nothing in it was lost.";
                if core.settings.lock().unwrap().toast_enabled {
                    win32::show_toast(message);
                }
                let _ = core.app.emit("feedback", json!({ "kind": "buffer-on-disk", "ok": true, "message": message }));
            }
            Err(e) => {
                core.log_ui(&format!("memory was low, but the buffer couldn't move to disk: {e}"));
                thread::sleep(MOVE_RETRY_AFTER);
            }
        }
    }
}

// Readings while listening, and checks for whether to listen at all, which cost more.
pub(crate) const LOUD_READ_EVERY: Duration = Duration::from_millis(100);
pub(crate) const LOUD_CHECK_EVERY: Duration = Duration::from_secs(1);
// Meters are found again this often, so a game that starts playing through another device is still heard.
pub(crate) const LOUD_REOPEN_EVERY: Duration = Duration::from_secs(5);
// An automatic clip ends this long after the moment, so the reaction is in it too.
pub(crate) const LOUD_AFTER_SECONDS: i64 = 3;
pub(crate) const AUTO_TAG: &str = "auto";

/// What Clip loud moments listens to right now, or None if it shouldn't listen: it's off, no game is
/// running, the buffer is stopped, or the mic it would listen to is muted in clips.
pub(crate) fn loud_source(core: &Core) -> Option<loud::Source> {
    let settings = core.settings.lock().unwrap().clone();
    if !settings.auto_clip || core.paused.load(Ordering::SeqCst) || !core.engine.status().buffering {
        return None;
    }
    let game = running_game(core)?;
    loud_source_for(core, &settings, Some(&game))
}

pub(crate) fn loud_source_for(core: &Core, settings: &Settings, game: Option<&str>) -> Option<loud::Source> {
    if settings.auto_clip_source == "game" {
        let added: Vec<String> = settings.game_profiles.iter().map(|p| p.exe.clone()).collect();
        let front = win32::foreground_game_app(&added)?;
        // Only the game being captured counts, not whatever else is in front.
        if game.is_some_and(|g| !front.exe.eq_ignore_ascii_case(g)) {
            return None;
        }
        return Some(loud::Source::App(front.pid));
    }
    if core.mic_silenced(settings) {
        return None;
    }
    let mic = settings.audio_sources.iter().find(|s| s.kind == "input").map(|s| s.device.clone())?;
    Some(loud::Source::Mic(mic))
}

/// Clip loud moments: saves a clip, tagged #auto, when the mic or the game stays loud for a moment.
pub(crate) fn watch_loud_moments(core: Shared) {
    win32::init_com_for_thread();
    let mut listener: Option<loud::Listener> = None;
    // What was last tried, so a source with no meters yet, such as a game that hasn't played a sound, is only
    // looked for again every few seconds rather than at every reading.
    let mut tried: Option<loud::Source> = None;
    let mut opened_at = Instant::now();
    let mut checked_at = Instant::now() - LOUD_CHECK_EVERY;
    let mut wanted: Option<loud::Source> = None;
    let mut detector = loud::Detector::new();
    let mut next_allowed = Instant::now();
    let mut level_db = -6.0;
    loop {
        if checked_at.elapsed() >= LOUD_CHECK_EVERY {
            checked_at = Instant::now();
            wanted = loud_source(&core);
            level_db = core.settings.lock().unwrap().auto_clip_level_db;
            if wanted.is_none() {
                listener = None;
                detector.reset();
            }
        }
        let Some(source) = wanted.clone() else {
            thread::sleep(LOUD_CHECK_EVERY);
            continue;
        };
        if tried.as_ref() != Some(&source) || opened_at.elapsed() >= LOUD_REOPEN_EVERY {
            listener = loud::Listener::open(source.clone());
            tried = Some(source);
            opened_at = Instant::now();
        }
        if let Some(heard) = listener.as_ref() {
            if detector.hear(heard.peak_db(), level_db) && Instant::now() >= next_allowed {
                let (seconds, after) = {
                    let s = core.settings.lock().unwrap();
                    (s.auto_clip_seconds, s.clip_after_seconds)
                };
                // The next moment can't start before this clip ends, so automatic clips never overlap.
                next_allowed = Instant::now() + Duration::from_secs((seconds + LOUD_AFTER_SECONDS).max(0) as u64);
                let saver = Arc::clone(&core);
                thread::spawn(move || {
                    // Keep recording after you save already waits, so this only makes up the difference.
                    let wait = (LOUD_AFTER_SECONDS - after).max(0);
                    thread::sleep(Duration::from_secs(wait as u64));
                    saver.log_ui("a loud moment was heard, so an automatic clip is being saved");
                    let _ = saver.save_clip_as(seconds + LOUD_AFTER_SECONDS.max(after), false, Some(AUTO_TAG), "Auto clip saved");
                });
            }
        }
        thread::sleep(LOUD_READ_EVERY);
    }
}

// Waits before each try at starting capture again after it stops by itself. A graphics driver update takes a
// minute or two, during which capture can't start, so the tries spread over about seven minutes.
pub(crate) const RECOVERY_DELAYS: [u64; 10] = [5, 10, 20, 30, 60, 60, 60, 60, 60, 60];
// Running this long without a problem starts the count of tries again.
pub(crate) const HEALTHY_AFTER: Duration = Duration::from_secs(10 * 60);
pub(crate) const HEALTH_CHECK_EVERY: Duration = Duration::from_secs(1);
// After falling back to the CPU, how long to wait between tries of the graphics card's encoder again.
pub(crate) const ENCODER_RETRY_EVERY: Duration = Duration::from_secs(60);
pub(crate) const ENCODER_RETRIES: u32 = 10;

/// The encoder setting an engine encoder id belongs to: "nvenc", "amd", "qsv" or "x264".
pub(crate) fn encoder_vendor(id: &str) -> &'static str {
    if id.contains("nvenc") {
        "nvenc"
    } else if id.contains("amf") {
        "amd"
    } else if id.contains("qsv") {
        "qsv"
    } else {
        "x264"
    }
}

/// How far down the engine's order an encoder is: NVIDIA, AMD, Intel, then the CPU.
pub(crate) fn encoder_rank(id: &str) -> usize {
    ["nvenc", "amd", "qsv", "x264"].iter().position(|v| *v == encoder_vendor(id)).unwrap_or(3)
}

/// Whether capture fell back to a worse encoder than one that worked earlier, which is what happens if
/// capture restarts while a graphics driver is being installed: to the CPU, or to another GPU such as the
/// processor's own graphics. An encoder the settings chose by name is never a fallback.
pub(crate) fn fell_back(chosen: &str, in_use: Option<&str>, best_seen: Option<&str>) -> bool {
    let (Some(in_use), Some(best_seen)) = (in_use, best_seen) else { return false };
    chosen != "x264" && encoder_vendor(in_use) != chosen && encoder_rank(in_use) > encoder_rank(best_seen)
}

impl Core {
    /// Stops and starts capture with the current settings. The buffer starts again from empty.
    fn restart_capture(&self) -> Result<(), String> {
        let _one_at_a_time = self.restart_lock.lock().unwrap();
        let settings = self.effective(&self.settings.lock().unwrap().clone());
        self.engine.stop();
        let started = self.engine.start(Some(settings));
        self.allow_media_folders();
        started
    }
}

/// Starts capture again by itself when it stops without being asked, such as after an encoder error, an
/// engine crash or a graphics driver update. It gives up after a while, so a broken setup can't loop forever.
pub(crate) fn watch_engine(core: Shared) {
    let mut tries = 0usize;
    let mut failed_at: Option<Instant> = None;
    let mut ready_since: Option<Instant> = None;
    let mut hardware_seen: Option<String> = None;
    let mut encoder_retries = 0u32;
    let mut encoder_tried_at = Instant::now();
    loop {
        thread::sleep(HEALTH_CHECK_EVERY);
        if core.quitting.load(Ordering::SeqCst) || core.paused.load(Ordering::SeqCst) {
            failed_at = None;
            continue;
        }
        let status = core.engine.status();
        if status.state == "ready" {
            failed_at = None;
            let since = *ready_since.get_or_insert_with(Instant::now);
            if since.elapsed() >= HEALTHY_AFTER {
                tries = 0;
            }
            let in_use = core.engine.encoder_in_use();
            let software_gpu = core.engine.on_software_gpu();
            let chosen = core.settings.lock().unwrap().encoder.clone();
            // Only a proper graphics driver counts, and only the best encoder seen, so a later fallback shows.
            if let Some(e) = in_use.as_deref().filter(|e| *e != "obs_x264" && !software_gpu) {
                if hardware_seen.as_deref().map_or(true, |best| encoder_rank(e) <= encoder_rank(best)) {
                    hardware_seen = Some(e.to_string());
                }
            }
            let fell = fell_back(&chosen, in_use.as_deref(), hardware_seen.as_deref());
            if !software_gpu && !fell {
                encoder_retries = 0;
            }
            let retry_due = encoder_tried_at.elapsed() >= ENCODER_RETRY_EVERY && since.elapsed() >= ENCODER_RETRY_EVERY;
            if (software_gpu || fell) && retry_due && !status.recording {
                if encoder_retries < ENCODER_RETRIES {
                    encoder_retries += 1;
                    encoder_tried_at = Instant::now();
                    let why = if software_gpu { "on Windows' software renderer, with no graphics driver ready" } else { "on a worse encoder after a problem" };
                    core.log_ui(&format!("capture is {why}, so trying the graphics card again ({encoder_retries} of {ENCODER_RETRIES})"));
                    ready_since = None;
                    let _ = core.restart_capture();
                } else if software_gpu && status.message.is_none() {
                    core.engine.set_message("Capture is running on the CPU because no graphics driver was ready. Restart Afterclip, or update your graphics driver.".into());
                }
            }
            continue;
        }
        ready_since = None;
        if status.state != "error" {
            failed_at = None;
            continue;
        }
        let problem = status.message.clone().unwrap_or_default();
        let Some(&delay) = RECOVERY_DELAYS.get(tries) else {
            if !problem.contains("Restart Afterclip") {
                core.log_ui(&format!("capture could not start again after {tries} tries, so it stays stopped: {problem}"));
                core.engine.set_message(format!("{problem}. Afterclip tried {tries} times to start it again. Restart Afterclip, or update your graphics driver."));
            }
            continue;
        };
        let since = match failed_at {
            Some(at) => at,
            None => {
                core.log_ui(&format!("capture stopped by itself ({problem}), so it starts again in {delay}s"));
                core.engine.set_message(format!("{problem}. Starting again in {delay} seconds."));
                let _ = core.app.emit("feedback", json!({ "kind": "recovering", "ok": true, "seconds": delay }));
                *failed_at.insert(Instant::now())
            }
        };
        if since.elapsed() < Duration::from_secs(delay) {
            continue;
        }
        tries += 1;
        failed_at = None;
        let started = core.restart_capture();
        match &started {
            Ok(()) => core.log_ui(&format!("capture started again (try {tries})")),
            Err(e) => core.log_ui(&format!("capture could not start again (try {tries}): {e}")),
        }
        if started.is_ok() {
            let _ = core.app.emit("feedback", json!({ "kind": "recovered", "ok": true }));
        }
    }
}

/// Applies per-game profiles and automatic recording as games come and go.
pub(crate) fn watch_games(core: Shared) {
    let mut last_seen = Instant::now();
    let mut handled: Option<String> = None;
    let mut started_recording = false;
    let mut gone_polls = 0;
    let mut session_start: Option<f64> = None;
    let mut storage_polls = 0;
    let mut warned_space = false;
    loop {
        thread::sleep(GAME_WATCH_EVERY);
        storage_polls += 1;
        if storage_polls >= STORAGE_CHECK_POLLS {
            storage_polls = 0;
            enforce_storage_rules(&core);
        }
        let base = core.settings.lock().unwrap().clone();
        let game = running_game(&core);
        let recording = core.engine.status().recording;
        // A recording keeps the buffer going too, so the time without a game counts from when it ends.
        if game.is_some() || recording {
            last_seen = Instant::now();
        }
        check_free_space(&core, &base, recording, &mut warned_space);

        // The buffer stops a while after the game closes, so its last moments can still be clipped.
        if core.paused.load(Ordering::SeqCst) {
            if !base.pauses_without_game() || game_in_front(&core, &base) {
                if let Err(e) = core.wake_engine() {
                    core.log_ui(&format!("capture could not start again: {e}"));
                }
                last_seen = Instant::now();
            }
            continue;
        }
        if base.pauses_without_game() && game.is_none() && !recording && last_seen.elapsed() >= profile_hold() && core.engine.status().state == "ready" {
            core.pause_engine();
            continue;
        }

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
                session_start.get_or_insert_with(now_ms);
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
                    if let Some(start) = session_start.take() {
                        end_session(&core, start);
                    }
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

/// When a game closes having saved clips, says how many, so they can be looked over together.
pub(crate) fn end_session(core: &Core, start_ms: f64) {
    let end_ms = now_ms();
    let clips = list_media(core).iter().filter(|m| m.kind == "clip" && m.mtime >= start_ms && m.mtime <= end_ms).count();
    if clips == 0 {
        return;
    }
    let session = GameSession { start_ms, end_ms, clips };
    *core.last_session.lock().unwrap() = Some(session.clone());
    if core.settings.lock().unwrap().toast_enabled {
        win32::show_toast(&format!("{clips} {} saved this session", if clips == 1 { "clip" } else { "clips" }));
    }
    let _ = core.app.emit("session", &session);
}
