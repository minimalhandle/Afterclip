use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

const MIN_BUFFER_SECONDS: i64 = 5;
const MAX_BUFFER_SECONDS: i64 = 1800;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ClipPreset {
    /// Seconds to keep. 0 means the whole buffer.
    pub seconds: i64,
    pub hotkey: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioSource {
    /// "output" records a playback device, "input" a microphone, and "app" one program such as Discord.
    pub kind: String,
    /// A device id, or for an app the window match string the engine lists.
    pub device: String,
    pub label: String,
    /// Heard in track 1, the main mix that ordinary players use.
    pub mix: bool,
    /// Also written to a track of its own, for editing later.
    pub own_track: bool,
    /// Whether the mic mute key silences this source. Unset uses the default below.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mute_with_key: Option<bool>,
    /// 1.0 is unchanged. Up to 2.0 boosts.
    #[serde(default = "full_volume")]
    pub volume: f64,
}

fn full_volume() -> f64 {
    1.0
}

/// A Discord channel that Send to Discord can post to. The address is a secret, so it stays out of logs and messages.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Webhook {
    pub name: String,
    pub url: String,
}

/// A hotkey that marks a moment with a name, such as "clutch". During a recording it adds a chapter with
/// that name. Otherwise it saves a clip tagged with it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MarkerKey {
    pub label: String,
    pub hotkey: String,
}

const MAX_MARKER_KEYS: usize = 8;
/// What libobs supports, including the main mix.
const MAX_AUDIO_TRACKS: usize = 6;
const MAX_MARKER_LABEL_CHARS: usize = 30;
pub const AUTO_CLIP_SOURCES: [&str; 2] = ["mic", "game"];

const MAX_WEBHOOKS: usize = 10;
const MAX_WEBHOOK_NAME_CHARS: usize = 40;

impl AudioSource {
    /// By default the key mutes inputs, but not virtual cables, which usually carry apps like Discord.
    pub fn muted_by_key(&self) -> bool {
        self.mute_with_key.unwrap_or(self.kind == "input" && !self.label.to_lowercase().contains("cable"))
    }
}

/// Settings for one game, applied while it's being captured. Anything left out uses the normal setting.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GameProfile {
    /// The game's program name, in lower case, such as cod.exe.
    pub exe: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quality: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame_rate: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub buffer_seconds: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub game_audio: Option<bool>,
    /// Captures this game in 9:16, or wide, whatever the normal setting says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vertical: Option<bool>,
    /// Starts a recording when the game opens, and stops it when the game closes.
    #[serde(default)]
    pub auto_record: bool,
    #[serde(default)]
    pub capture_privacy: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    pub buffer_seconds: i64,
    pub presets: Vec<ClipPreset>,
    pub record_hotkey: String,
    pub output_dir: String,
    pub encoder: String,
    /// low, medium, high, full or custom. The UI fills in the fields below from the first four.
    pub quality: String,
    /// native, a height such as 1080, or custom to keep width and height as they are.
    pub resolution: String,
    /// native, 60, 120, or custom to keep fps as it is.
    pub frame_rate: String,
    /// h264, hevc or av1.
    pub codec: String,
    /// p1 (fastest) to p7 (best quality).
    pub encoder_preset: String,
    pub multipass: bool,
    pub toast_enabled: bool,
    pub beep_enabled: bool,
    pub launch_on_startup: bool,
    pub start_minimized: bool,
    pub close_to_tray: bool,
    pub fps: i64,
    pub width: i64,
    pub height: i64,
    pub bitrate_kbps: i64,
    /// Only read when upgrading from a version before audio_sources existed.
    pub capture_mic: bool,
    pub audio_sources: Vec<AudioSource>,
    /// Records the detected game's own audio, wherever it's routed.
    pub game_audio: bool,
    pub game_audio_own_track: bool,
    /// Silences the marked sources in recordings, without affecting the mic anywhere else.
    pub mic_muted: bool,
    pub mic_hotkey: String,
    /// Noise suppression and a noise gate on the mic sources.
    pub clean_mic: bool,
    pub game_audio_volume: f64,
    /// Starts a new recording file after this long. 0 keeps one file.
    pub record_split_seconds: i64,
    pub marker_hotkey: String,
    pub screenshot_hotkey: String,
    /// The one webhook versions before 1.4.0 had. Only read, to move it into discord_webhooks.
    #[serde(default, skip_serializing)]
    pub discord_webhook: String,
    /// Where Send to Discord can post. The first is used by the Stream Deck key and the API.
    pub discord_webhooks: Vec<Webhook>,
    #[serde(default)]
    pub share_recipes: Vec<crate::advanced::Recipe>,
    #[serde(default)]
    pub vertical_layouts: std::collections::BTreeMap<String, crate::advanced::Layout>,
    /// Compress, Send to Discord and vertical copies even out the volume, so quiet clips come out as loud as others.
    pub even_audio: bool,
    /// Saves a clip of star_clip_seconds and stars it, so clean-up never removes it.
    pub star_clip_hotkey: String,
    /// 0 means the whole buffer.
    pub star_clip_seconds: i64,
    /// Relaunches through UAC at every start, which some games need for game capture.
    pub run_as_admin: bool,
    /// Moves the oldest clips and recordings to the Recycle Bin above this size. 0 keeps everything.
    pub storage_limit_gb: f64,
    /// Moves clips, recordings and screenshots to the Recycle Bin after this many days. 0 keeps them.
    pub delete_after_days: i64,
    /// The storage limit and age cleanup also leave alone clips you tagged (the automatic "auto" tag doesn't count).
    pub keep_tagged: bool,
    pub game_profiles: Vec<GameProfile>,
    /// Checks for updates and installs them when it's safe to.
    pub auto_update: bool,
    /// The version someone chose to skip, which isn't offered again.
    pub skipped_update: String,
    /// The last version whose patch notes were shown, so they show once after each update.
    pub seen_version: String,
    /// The bundled Stream Deck plugin version whose install offer was answered, so it isn't offered again
    /// until a newer plugin ships.
    pub stream_deck_offer_seen: String,
    /// Lets a controller run the actions below, each with View, Create or Minus held.
    pub controller_enabled: bool,
    /// The second button for each action, by position, as controller::button reads it. Empty is off.
    pub controller_clip: String,
    pub controller_record: String,
    pub controller_mic: String,
    pub controller_marker: String,
    pub controller_screenshot: String,
    /// How much a controller clip keeps. 0 means the whole buffer.
    pub controller_clip_seconds: i64,
    /// Keeps capturing this long after a clip is asked for, so the moment isn't cut off. 0 saves straight away.
    pub clip_after_seconds: i64,
    /// Stops the buffer while no game is running, and starts it again when one opens.
    pub buffer_only_in_games: bool,
    /// The mute-key sources are only recorded while this key is held.
    pub push_to_talk: bool,
    /// A key or mouse button, as win32::virtual_key reads it.
    pub push_to_talk_key: String,
    /// Puts a webcam in a corner of every clip and recording.
    pub camera_enabled: bool,
    /// The webcam's id, from the engine's list.
    pub camera_device: String,
    /// top-left, top-right, bottom-left or bottom-right.
    pub camera_corner: String,
    /// The webcam's width, as a percentage of the picture's width.
    pub camera_size: i64,
    /// Vertical copies of a clip with a webcam put the webcam above the game.
    pub vertical_face_cam: bool,
    /// Saves into a folder for each game, inside Clips, Recordings and Screenshots.
    pub game_folders: bool,
    /// Warns when the save location has less than this free. 0 never warns.
    pub low_space_warn_gb: f64,
    /// Captures in 9:16 for phones: the middle of the game, with the webcam across the top. The resolution
    /// setting still decides the height, and is kept for when this is off.
    pub vertical_mode: bool,
    /// Lets a Stream Deck on another PC on the local network control Afterclip, with the key.
    pub remote_stream_deck: bool,
    /// The port a Stream Deck on another PC connects to.
    pub remote_port: i64,
    /// Replaces screen and audio capture with a colour card. Used by the tests.
    pub test_pattern: bool,
    /// Which corner notes such as Clip saved appear in, on the monitor you're using.
    pub toast_corner: String,
    /// Hotkeys that mark a moment with a name.
    pub marker_keys: Vec<MarkerKey>,
    /// How much a named marker key saves when no recording is running. 0 means the whole buffer.
    pub marker_clip_seconds: i64,
    /// Saves a clip by itself when your mic or the game gets loud, during a game.
    pub auto_clip: bool,
    /// What it listens to: mic or game.
    pub auto_clip_source: String,
    /// How loud, in dB below full scale, counts as a moment. Closer to 0 needs more.
    pub auto_clip_level_db: f64,
    /// How much an automatic clip keeps, ending a few seconds after the moment.
    pub auto_clip_seconds: i64,
    /// Keeps the buffer in files beside the clips instead of in RAM, apart from its last few seconds.
    pub disk_buffer: bool,
    /// When memory runs low during a game, moves the buffer to disk without losing what's in it.
    pub free_memory_in_games: bool,
    /// Captures in HDR, as 10-bit HEVC or AV1, while Windows HDR is on for the main monitor.
    pub hdr_capture: bool,
    /// Records a capture card, such as an Elgato 4K X, instead of this PC's screen and games.
    pub capture_card: bool,
    /// The card's video device id, from the engine's list.
    pub capture_card_device: String,
    /// The card's audio device id, from the engine's list. Empty records no sound from the card.
    pub capture_card_audio: String,
    /// The card's own video settings, used instead of the PC's while it's recorded, and never limited by this
    /// PC's monitor: a picture height (2160, 1440, 1080 or 720), frames per second, and bitrate.
    pub card_resolution: String,
    pub card_fps: i64,
    pub card_bitrate_kbps: i64,
    /// The card's sound: how loud, and whether it also gets an audio track of its own.
    pub card_audio_volume: f64,
    pub card_audio_own_track: bool,
    /// Whether the console sends HDR. It's the card's picture, apart from hdr_capture, which is the recording's:
    /// HDR read as SDR looks grey and washed out, and SDR read as HDR too bright.
    pub card_hdr: bool,
    /// Pauses the buffer while the card shows only a still, plain picture, such as its own NO SIGNAL screen,
    /// so nothing is encoded while the console is off. It starts again as soon as a picture arrives.
    pub card_pause_without_signal: bool,
    /// Switches between this PC and the capture card.
    pub card_hotkey: String,
    /// Once a week, joins the week's favourites, or clips with weekly_reel_tag, into one video.
    pub weekly_reel: bool,
    /// The tag a clip needs to go in the weekly reel. Empty means favourites.
    pub weekly_reel_tag: String,
    /// Anonymous usage count: None until the user is asked, then their answer (usage.rs).
    #[serde(default)]
    pub usage_ping: Option<bool>,
}

/// Where the webcam sits on the picture, in pixels: x, y, width, height.
pub type CameraBox = (i64, i64, i64, i64);

const CAMERA_SIZES: (i64, i64) = (10, 50);

impl Settings {
    pub fn defaults(videos_dir: &Path, width: i64, height: i64) -> Self {
        let preset = |seconds, hotkey: &str| ClipPreset { seconds, hotkey: hotkey.into() };
        Settings {
            buffer_seconds: 120,
            // Alt+Shift avoids common overlay defaults such as NVIDIA's Alt+F9/F10.
            presets: vec![
                preset(15, "Alt+Shift+1"),
                preset(30, "Alt+Shift+2"),
                preset(60, "Alt+Shift+3"),
                preset(0, "Alt+Shift+4"),
            ],
            record_hotkey: "Alt+Shift+R".into(),
            output_dir: videos_dir.join("Afterclip").to_string_lossy().into_owned(),
            encoder: "auto".into(),
            // Medium: 1080p at 20 Mbps. Light on the GPU and half the buffer's memory of High.
            quality: "medium".into(),
            resolution: "1080".into(),
            frame_rate: "60".into(),
            codec: "h264".into(),
            encoder_preset: "p3".into(),
            multipass: false,
            toast_enabled: true,
            beep_enabled: true,
            launch_on_startup: false,
            start_minimized: true,
            close_to_tray: true,
            fps: 60,
            width,
            height,
            bitrate_kbps: 20000,
            capture_mic: true,
            audio_sources: vec![
                AudioSource { kind: "output".into(), device: "default".into(), label: "Desktop audio".into(), mix: true, own_track: false, mute_with_key: None, volume: 1.0 },
                AudioSource { kind: "input".into(), device: "default".into(), label: "Microphone".into(), mix: true, own_track: false, mute_with_key: None, volume: 1.0 },
            ],
            game_audio: true,
            game_audio_own_track: false,
            mic_muted: false,
            mic_hotkey: "Alt+Shift+M".into(),
            clean_mic: false,
            game_audio_volume: 1.0,
            record_split_seconds: 0,
            marker_hotkey: "Alt+Shift+K".into(),
            screenshot_hotkey: "Alt+Shift+S".into(),
            discord_webhook: String::new(),
            discord_webhooks: Vec::new(),
            share_recipes: Vec::new(),
            vertical_layouts: Default::default(),
            even_audio: true,
            star_clip_hotkey: "Alt+Shift+F".into(),
            star_clip_seconds: 30,
            run_as_admin: false,
            storage_limit_gb: 0.0,
            delete_after_days: 0,
            keep_tagged: false,
            game_profiles: Vec::new(),
            auto_update: true,
            skipped_update: String::new(),
            seen_version: String::new(),
            stream_deck_offer_seen: String::new(),
            controller_enabled: false,
            controller_clip: "r3".into(),
            controller_record: String::new(),
            controller_mic: String::new(),
            controller_marker: String::new(),
            controller_screenshot: String::new(),
            controller_clip_seconds: 30,
            clip_after_seconds: 0,
            // No capture on the desktop: the GPU, encoder and memory are only used while a game is open.
            buffer_only_in_games: true,
            push_to_talk: false,
            push_to_talk_key: "Mouse4".into(),
            camera_enabled: false,
            camera_device: String::new(),
            camera_corner: "bottom-right".into(),
            camera_size: 22,
            vertical_face_cam: true,
            game_folders: false,
            low_space_warn_gb: 10.0,
            vertical_mode: false,
            remote_stream_deck: false,
            remote_port: crate::api::REMOTE_PORT as i64,
            test_pattern: false,
            toast_corner: "top-right".into(),
            marker_keys: Vec::new(),
            marker_clip_seconds: 30,
            auto_clip: false,
            auto_clip_source: "mic".into(),
            auto_clip_level_db: -6.0,
            auto_clip_seconds: 30,
            disk_buffer: false,
            free_memory_in_games: true,
            hdr_capture: false,
            capture_card: false,
            capture_card_device: String::new(),
            capture_card_audio: String::new(),
            card_resolution: "2160".into(),
            card_fps: 60,
            card_bitrate_kbps: 60000,
            card_audio_volume: 1.0,
            card_audio_own_track: false,
            card_hdr: false,
            card_pause_without_signal: true,
            card_hotkey: String::new(),
            weekly_reel: false,
            weekly_reel_tag: String::new(),
            usage_ping: None,
        }
    }

    /// Points each output and input source whose device is gone at a working device with the same name, such as
    /// after Voicemeeter or VB-Cable is reinstalled and Windows gives its devices new ids. Each list is the working
    /// devices' ids and names, or None if Windows couldn't be asked, which changes nothing. Returns whether any
    /// source changed, and the names of the ones still missing.
    pub fn relink_audio(&mut self, outputs: Option<&[(String, String)]>, inputs: Option<&[(String, String)]>) -> (bool, Vec<String>) {
        let (mut changed, mut missing) = (false, Vec::new());
        for source in &mut self.audio_sources {
            let devices = match source.kind.as_str() {
                "output" => outputs,
                "input" => inputs,
                _ => continue,
            };
            let Some(devices) = devices else { continue };
            if source.device.is_empty() || source.device == "default" || devices.iter().any(|(id, _)| *id == source.device) {
                continue;
            }
            match devices.iter().find(|(_, name)| *name == source.label) {
                Some((id, _)) => {
                    source.device = id.clone();
                    changed = true;
                }
                None => missing.push(source.label.clone()),
            }
        }
        (changed, missing)
    }

    /// The capture card's video and audio devices while it's what's recorded, or None for the screen and games.
    pub fn card(&self) -> Option<(&str, &str)> {
        (self.capture_card && !self.capture_card_device.is_empty() && !self.test_pattern)
            .then_some((self.capture_card_device.as_str(), self.capture_card_audio.as_str()))
    }

    /// These settings with the card's own resolution, frame rate and quality, while a card is recorded.
    /// The picture is 16:9, the shape consoles send, whatever this PC's monitor is.
    pub fn with_card(mut self) -> Settings {
        if self.card().is_some() {
            self.height = self.card_resolution.parse::<i64>().unwrap_or(2160);
            self.width = (self.height * 16 / 9 + 1) / 2 * 2;
            self.fps = self.card_fps;
            self.bitrate_kbps = self.card_bitrate_kbps;
            // Game audio follows a game on this PC, and the card's games aren't on it.
            self.game_audio = false;
        }
        self
    }

    /// Whether the card's sound is recorded on a track of its own.
    pub fn card_track(&self) -> bool {
        self.card().is_some_and(|(_, audio)| !audio.is_empty()) && self.card_audio_own_track
    }

    /// Whether the buffer stops while no game is open. A capture card's games run on another device, so it never does.
    pub fn pauses_without_game(&self) -> bool {
        self.buffer_only_in_games && self.card().is_none()
    }

    /// The webcam's place on the picture, in a 16:9 box with a small margin, or None with no webcam chosen.
    /// It's there even while the webcam is switched off, so switching it back on needs no restart.
    pub fn camera_box(&self) -> Option<CameraBox> {
        if self.camera_device.is_empty() {
            return None;
        }
        let (canvas_width, canvas_height) = self.canvas();
        // In vertical mode it goes across the top, the whole width, as a face cam does.
        if self.vertical_mode {
            return Some((0, 0, canvas_width, (canvas_width * 9 / 16) / 2 * 2));
        }
        let width = (canvas_width * self.camera_size / 100) / 2 * 2;
        let height = (width * 9 / 16) / 2 * 2;
        let margin = (canvas_height / 40) / 2 * 2;
        let x = if self.camera_corner.ends_with("left") { margin } else { canvas_width - width - margin };
        let y = if self.camera_corner.starts_with("top") { margin } else { canvas_height - height - margin };
        Some((x, y, width, height))
    }

    /// What each audio track is called, in the order the engine fills them: the main mix, then each source with
    /// a track of its own, then game audio. Keep in step with createAudioSources in afterclip-engine.cpp.
    pub fn track_names(&self) -> Vec<String> {
        let clean = |name: &str| {
            let name = name.replace('|', "/").chars().filter(|c| !c.is_control()).collect::<String>();
            let name = name.trim();
            // Windows adds the driver in brackets, such as "CABLE-D Output (VB-Audio Cable D)", which editors cut off.
            let short = name.strip_suffix(')').and_then(|rest| rest.rfind(" (").map(|at| rest[..at].trim())).unwrap_or(name);
            if short.is_empty() { name.to_string() } else { short.to_string() }
        };
        let mut names = vec!["Main mix".to_string()];
        for (i, source) in self.audio_sources.iter().enumerate().filter(|(_, s)| s.own_track) {
            let label = clean(&source.label);
            names.push(if label.is_empty() { format!("Source {}", i + 1) } else { label });
        }
        if self.game_audio && self.game_audio_own_track {
            names.push("Game".into());
        }
        if self.card_track() {
            names.push("Capture card".into());
        }
        names.truncate(MAX_AUDIO_TRACKS);
        names
    }

    /// The size of what's captured. Vertical mode keeps the height and narrows the picture to 9:16, so
    /// it's the game's own pixels from the middle, not scaled up.
    pub fn canvas(&self) -> (i64, i64) {
        if self.vertical_mode {
            ((self.height * 9 / 16) / 2 * 2, self.height)
        } else {
            (self.width, self.height)
        }
    }

    /// Works out the real width, height and frame rate from the monitor. Heights never exceed the
    /// monitor, because scaling up adds size without adding detail.
    pub fn resolved_for(mut self, (width, height, refresh): (u32, u32, u32)) -> Self {
        let (width, height) = (width as i64, height as i64);
        match self.resolution.as_str() {
            "custom" => {}
            "native" => (self.width, self.height) = (width, height),
            chosen => {
                let target = chosen.parse::<i64>().unwrap_or(height).min(height);
                self.height = target;
                self.width = (width * target / height.max(1) + 1) / 2 * 2;
            }
        }
        match self.frame_rate.as_str() {
            "custom" => {}
            "native" => self.fps = (refresh as i64).max(30),
            chosen => self.fps = chosen.parse().unwrap_or(60),
        }
        self
    }

    /// What capture should use while this game runs. Keep the quality levels in step with QUALITY in app.js.
    pub fn with_profile(&self, profile: &GameProfile, display: (u32, u32, u32)) -> Settings {
        let mut s = self.clone();
        if let Some(quality) = &profile.quality {
            let level = match quality.as_str() {
                "low" => Some(("720", "60", 8000, "p1", false)),
                "medium" => Some(("1080", "60", 20000, "p3", false)),
                "high" => Some(("native", "60", 40000, "p3", false)),
                "full" => Some(("native", "native", 150000, "p5", true)),
                _ => None,
            };
            if let Some((resolution, frame_rate, bitrate, preset, multipass)) = level {
                s.quality = quality.clone();
                s.resolution = resolution.into();
                s.frame_rate = frame_rate.into();
                s.bitrate_kbps = bitrate;
                s.encoder_preset = preset.into();
                s.multipass = multipass;
            }
        }
        if let Some(frame_rate) = &profile.frame_rate {
            s.frame_rate = frame_rate.clone();
        }
        if let Some(seconds) = profile.buffer_seconds {
            s.buffer_seconds = seconds;
        }
        if let Some(on) = profile.game_audio {
            s.game_audio = on;
        }
        if let Some(on) = profile.vertical {
            s.vertical_mode = on;
        }
        s.resolved_for(display).sanitized()
    }

    pub fn sanitized(mut self) -> Self {
        self.storage_limit_gb = self.storage_limit_gb.max(0.0);
        self.delete_after_days = self.delete_after_days.max(0);
        // Moves a webhook from before 1.4.0 into the list.
        let legacy = std::mem::take(&mut self.discord_webhook);
        if !legacy.trim().is_empty() && !self.discord_webhooks.iter().any(|w| w.url.trim() == legacy.trim()) {
            self.discord_webhooks.insert(0, Webhook { name: "Discord".into(), url: legacy });
        }
        self.discord_webhooks.truncate(MAX_WEBHOOKS);
        for (i, hook) in self.discord_webhooks.iter_mut().enumerate() {
            hook.url = hook.url.trim().to_string();
            hook.name = hook.name.chars().filter(|c| !c.is_control()).take(MAX_WEBHOOK_NAME_CHARS).collect::<String>().trim().to_string();
            if hook.name.is_empty() {
                hook.name = format!("Channel {}", i + 1);
            }
        }
        self.star_clip_seconds = self.star_clip_seconds.clamp(0, MAX_BUFFER_SECONDS);
        self.clip_after_seconds = self.clip_after_seconds.clamp(0, 30);
        self.camera_size = self.camera_size.clamp(CAMERA_SIZES.0, CAMERA_SIZES.1);
        if !["2160", "1440", "1080", "720"].contains(&self.card_resolution.as_str()) {
            self.card_resolution = "2160".into();
        }
        if ![30, 60, 120].contains(&self.card_fps) {
            self.card_fps = 60;
        }
        self.card_bitrate_kbps = self.card_bitrate_kbps.clamp(2000, 150000);
        self.card_audio_volume = self.card_audio_volume.clamp(0.0, 2.0);
        self.low_space_warn_gb = self.low_space_warn_gb.max(0.0);
        if !["top-right", "top-left", "bottom-right", "bottom-left"].contains(&self.toast_corner.as_str()) {
            self.toast_corner = "top-right".into();
        }
        for key in &mut self.marker_keys {
            key.label = key.label.trim().trim_start_matches('#').chars().filter(|c| !c.is_control()).take(MAX_MARKER_LABEL_CHARS).collect::<String>().trim().to_string();
        }
        self.marker_keys.truncate(MAX_MARKER_KEYS);
        self.marker_clip_seconds = self.marker_clip_seconds.clamp(0, MAX_BUFFER_SECONDS);
        if !AUTO_CLIP_SOURCES.contains(&self.auto_clip_source.as_str()) {
            self.auto_clip_source = "mic".into();
        }
        self.auto_clip_level_db = if self.auto_clip_level_db.is_finite() { self.auto_clip_level_db.clamp(-40.0, -1.0) } else { -6.0 };
        self.auto_clip_seconds = self.auto_clip_seconds.clamp(10, 120);
        self.weekly_reel_tag = crate::library::clean_tags(&[self.weekly_reel_tag.clone()]).into_iter().next().unwrap_or_default();
        // Below 1024 needs administrator rights on some systems, and this PC's own port is taken.
        if !(1024..=65535).contains(&self.remote_port) || self.remote_port == crate::api::LOCAL_PORT as i64 {
            self.remote_port = crate::api::REMOTE_PORT as i64;
        }
        self.buffer_seconds = self.buffer_seconds.clamp(MIN_BUFFER_SECONDS, MAX_BUFFER_SECONDS);
        self.fps = self.fps.clamp(10, 240);
        self.bitrate_kbps = self.bitrate_kbps.clamp(1000, 200_000);
        self.width = self.width.max(320);
        self.height = self.height.max(240);
        for p in &mut self.presets {
            p.seconds = p.seconds.max(0);
        }
        self
    }

    /// Loads a file over the defaults, so settings added in later versions get a value.
    pub fn load(file: &Path, defaults: Settings) -> Settings {
        let merged = fs::read_to_string(file).ok().and_then(|text| {
            let saved: serde_json::Value = serde_json::from_str(&text).ok()?;
            let mut base = serde_json::to_value(&defaults).ok()?;
            base.as_object_mut()?.extend(saved.as_object()?.clone());
            let mut settings = serde_json::from_value::<Settings>(base).ok()?;
            // Someone upgrading with the microphone turned off must not have it switched on.
            if saved.get("audioSources").is_none() && !settings.capture_mic {
                settings.audio_sources.retain(|s| s.kind != "input");
            }
            // Someone upgrading already chose their own values, so they're shown as custom, not changed.
            if saved.get("quality").is_none() {
                settings.quality = "custom".into();
            }
            // Settings from before these existed keep producing exactly the same video.
            if saved.get("resolution").is_none() {
                let native = settings.width == defaults.width && settings.height == defaults.height;
                settings.resolution = if native { "native" } else { "custom" }.into();
            }
            if saved.get("frameRate").is_none() {
                settings.frame_rate = match settings.fps { 60 => "60", 120 => "120", _ => "custom" }.into();
            }
            // The card's HDR used to follow the HDR switch.
            if saved.get("cardHdr").is_none() {
                settings.card_hdr = settings.hdr_capture;
            }
            if saved.get("encoderPreset").is_none() && settings.quality == "full" {
                settings.encoder_preset = "p5".into();
                settings.multipass = true;
            }
            Some(settings)
        });
        merged.unwrap_or(defaults).sanitized()
    }

    pub fn save(&self, file: &Path) -> std::io::Result<()> {
        if let Some(dir) = file.parent() {
            fs::create_dir_all(dir)?;
        }
        fs::write(file, serde_json::to_string_pretty(self)?)
    }

    /// Settings the engine only reads at start, so changing one restarts it and empties the buffer.
    pub fn needs_engine_restart(&self, other: &Settings) -> bool {
        self.buffer_seconds != other.buffer_seconds
            || self.output_dir != other.output_dir
            || self.encoder != other.encoder
            || self.codec != other.codec
            || self.encoder_preset != other.encoder_preset
            || self.multipass != other.multipass
            || self.fps != other.fps
            || self.width != other.width
            || self.height != other.height
            || self.vertical_mode != other.vertical_mode
            || self.disk_buffer != other.disk_buffer
            || self.hdr_capture != other.hdr_capture
            || self.bitrate_kbps != other.bitrate_kbps
            // Volume changes live, so it alone never needs a restart.
            || without_volume(&self.audio_sources) != without_volume(&other.audio_sources)
            || self.game_audio != other.game_audio
            || self.game_audio_own_track != other.game_audio_own_track
            || self.test_pattern != other.test_pattern
            || self.game_profiles.iter().any(|profile| profile.capture_privacy) != other.game_profiles.iter().any(|profile| profile.capture_privacy)
            || self.card() != other.card()
            || self.card_track() != other.card_track()
            || (self.card().is_some() && self.card_hdr != other.card_hdr)
            || self.camera_box() != other.camera_box()
            || (self.camera_box().is_some() && self.camera_device != other.camera_device)
    }

    /// The port the API listens on for other PCs, or None if only this PC may use it.
    pub fn remote_port(&self) -> Option<u16> {
        self.remote_stream_deck.then_some(self.remote_port as u16)
    }

    /// A settings file someone saved, taken over these settings. What belongs to this PC, or is secret, stays.

    pub fn imported(&self, file: &Path) -> Result<Settings, String> {
        let text = fs::read_to_string(file).map_err(|e| format!("Couldn't read that file: {e}"))?;
        let saved: serde_json::Value = serde_json::from_str(&text).map_err(|_| "That isn't an Afterclip settings file.".to_string())?;
        if !saved.get("presets").is_some_and(|p| p.is_array()) || saved.get("bufferSeconds").is_none() {
            return Err("That isn't an Afterclip settings file.".into());
        }
        let mut settings = Settings::load(file, self.clone());
        settings.discord_webhooks = self.discord_webhooks.clone();
        settings.skipped_update = self.skipped_update.clone();
        settings.seen_version = self.seen_version.clone();
        settings.stream_deck_offer_seen = self.stream_deck_offer_seen.clone();
        settings.test_pattern = self.test_pattern;
        // Counting this PC is its own choice, never something a file from another PC decides.
        settings.usage_ping = self.usage_ping;
        // Opening this PC to the network is its own choice, never something a file from another PC decides.
        settings.remote_stream_deck = self.remote_stream_deck;
        settings.remote_port = self.remote_port;
        // Another PC's drive may not exist here.
        let drive = Path::new(&settings.output_dir).components().next().map(|c| PathBuf::from(c.as_os_str()).join("\\"));
        if !drive.is_some_and(|d| d.exists()) {
            settings.output_dir = self.output_dir.clone();
        }
        Ok(settings)
    }

    /// What Export settings writes. Webhooks are secret, so they're left out.
    pub fn exported(&self) -> serde_json::Value {
        let mut value = serde_json::to_value(self).unwrap_or_default();
        if let Some(map) = value.as_object_mut() {
            for key in ["discordWebhook", "discordWebhooks", "skippedUpdate", "seenVersion", "testPattern", "micMuted", "usagePing"] {
                map.remove(key);
            }
        }
        value
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Settings {
        Settings::defaults(Path::new("C:\\Videos"), 1920, 1080)
    }

    #[test]
    fn the_webcam_sits_in_its_corner() {
        let mut s = sample();
        assert_eq!(s.camera_box(), None);
        s.camera_device = "cam".into();
        let off = s.camera_box();
        s.camera_enabled = true;
        assert_eq!(off, s.camera_box(), "switching it on or off doesn't move it, so no restart is needed");
        assert!(!sample().needs_engine_restart(&Settings { camera_enabled: true, ..sample() }));

        s.camera_size = 25;
        // 480 wide, 270 tall, with a 26 pixel margin.
        assert_eq!(s.camera_box(), Some((1920 - 480 - 26, 1080 - 270 - 26, 480, 270)));
        s.camera_corner = "top-left".into();
        assert_eq!(s.camera_box(), Some((26, 26, 480, 270)));
    }

    #[test]
    fn a_capture_card_uses_its_own_video_settings() {
        let pc = Settings { width: 2560, height: 1440, fps: 165, bitrate_kbps: 40000, ..sample() };
        assert_eq!(pc.clone().with_card(), pc, "without a card, the PC's settings are used");
        let card = Settings { capture_card: true, capture_card_device: "Elgato 4K X:path".into(), ..pc.clone() }.with_card();
        assert_eq!((card.width, card.height, card.fps, card.bitrate_kbps), (3840, 2160, 60, 60000), "bigger than the monitor");
        let card = Settings { card_resolution: "1440".into(), card_fps: 120, ..card }.with_card();
        assert_eq!((card.width, card.height, card.fps), (2560, 1440, 120));
        assert!(pc.needs_engine_restart(&card), "switching to the card restarts capture");
        assert!(!card.game_audio, "no game on this PC to follow");
        let tracked = Settings { capture_card_audio: "Elgato 4K X (Elgato 4K X):".into(), card_audio_own_track: true, ..card.clone() };
        assert_eq!(tracked.track_names().last().map(String::as_str), Some("Capture card"));
        assert!(card.needs_engine_restart(&tracked), "a new track needs a restart");
        assert!(!tracked.needs_engine_restart(&Settings { card_audio_volume: 0.5, ..tracked.clone() }), "volume changes live");
    }

    #[test]
    fn audio_devices_with_new_ids_are_found_again_by_name() {
        let source = |kind: &str, device: &str, label: &str| AudioSource { kind: kind.into(), device: device.into(), label: label.into(), mix: true, own_track: false, mute_with_key: None, volume: 1.0 };
        let mut s = sample();
        s.audio_sources = vec![
            source("output", "{old-out}", "Voicemeeter Input (VB-Audio Voicemeeter VAIO)"),
            source("input", "{old-in}", "Voicemeeter Out B1 (VB-Audio Voicemeeter VAIO)"),
            source("input", "{gone}", "Yeti (Blue Yeti)"),
            source("output", "default", "Default"),
            source("app", "Discord:Discord.exe", "Discord"),
            source("input", "{kept}", "Microphone (USB)"),
        ];
        let outputs = vec![("{new-out}".to_string(), "Voicemeeter Input (VB-Audio Voicemeeter VAIO)".to_string())];
        let inputs = vec![
            ("{new-in}".to_string(), "Voicemeeter Out B1 (VB-Audio Voicemeeter VAIO)".to_string()),
            ("{kept}".to_string(), "Microphone (USB)".to_string()),
        ];
        let (changed, missing) = s.relink_audio(Some(&outputs), Some(&inputs));
        assert!(changed);
        assert_eq!(missing, vec!["Yeti (Blue Yeti)".to_string()], "only a device that's really gone is missing");
        let devices: Vec<&str> = s.audio_sources.iter().map(|a| a.device.as_str()).collect();
        assert_eq!(devices, ["{new-out}", "{new-in}", "{gone}", "default", "Discord:Discord.exe", "{kept}"]);
        assert_eq!(s.relink_audio(Some(&outputs), Some(&inputs)).0, false, "nothing changes the second time");
        assert_eq!(s.relink_audio(None, None), (false, vec![]), "Windows not answering isn't every device missing");
    }

    #[test]
    fn the_cards_hdr_is_its_own_and_starts_out_following_the_hdr_switch() {
        let card = Settings { capture_card: true, capture_card_device: "Elgato 4K X:path".into(), ..sample() };
        assert!(card.needs_engine_restart(&Settings { card_hdr: !card.card_hdr, ..card.clone() }), "the card reopens in its new colour space");
        assert!(!sample().needs_engine_restart(&Settings { card_hdr: !sample().card_hdr, ..sample() }), "no card, nothing to reopen");

        let dir = std::env::temp_dir().join(format!("afterclip-card-hdr-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("settings.json");
        for hdr in [false, true] {
            fs::write(&file, format!(r#"{{"hdrCapture": {hdr}, "bufferSeconds": 120}}"#)).unwrap();
            assert_eq!(Settings::load(&file, sample()).card_hdr, hdr, "from before it existed, it follows HDR");
        }
        fs::write(&file, r#"{"hdrCapture": false, "cardHdr": true, "bufferSeconds": 120}"#).unwrap();
        assert!(Settings::load(&file, sample()).card_hdr, "once saved, it's its own");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn vertical_mode_narrows_the_picture_and_keeps_the_resolution() {
        let mut s = sample();
        s.vertical_mode = true;
        assert_eq!(s.canvas(), (606, 1080));
        assert_eq!((s.width, s.height), (1920, 1080), "the saved resolution is kept for when it's off");
        s.camera_device = "cam".into();
        assert_eq!(s.camera_box(), Some((0, 0, 606, 340)), "the webcam goes across the top");
        assert!(sample().needs_engine_restart(&s));
        let profile = GameProfile { exe: "game.exe".into(), name: "Game".into(), quality: None, frame_rate: None, buffer_seconds: None, game_audio: None, vertical: Some(true), auto_record: false, capture_privacy: false };
        assert_eq!(sample().with_profile(&profile, (1920, 1080, 60)).canvas(), (606, 1080));
    }

    #[test]
    fn audio_tracks_are_named_in_the_engines_order() {
        let mut s = sample();
        assert_eq!(s.track_names(), vec!["Main mix"], "sources only in the mix have no track of their own");
        s.audio_sources[1].own_track = true;
        s.audio_sources.push(AudioSource { kind: "app".into(), device: "discord".into(), label: "Disc|ord".into(), mix: false, own_track: true, mute_with_key: None, volume: 1.0 });
        s.game_audio_own_track = true;
        assert_eq!(s.track_names(), vec!["Main mix", "Microphone", "Disc/ord", "Game"], "a bar would split the --names list");
        s.audio_sources[1].label = "Voicemeeter Out B1 (VB-Audio Voicemeeter VAIO)".into();
        s.audio_sources[2].label = "(Unnamed)".into();
        assert_eq!(s.track_names()[1..3], ["Voicemeeter Out B1", "(Unnamed)"], "the driver in brackets goes, unless it's all there is");
        for i in 0..6 {
            s.audio_sources.push(AudioSource { kind: "app".into(), device: format!("app{i}"), label: String::new(), mix: false, own_track: true, mute_with_key: None, volume: 1.0 });
        }
        let names = s.track_names();
        assert_eq!(names.len(), MAX_AUDIO_TRACKS, "libobs has six tracks at most");
        assert_eq!(names[3], "Source 4", "an unlabelled source still gets a name");
    }

    #[test]
    fn disk_buffer_and_hdr_restart_capture_but_the_others_dont() {
        let base = sample();
        assert!(base.needs_engine_restart(&Settings { disk_buffer: true, ..sample() }));
        assert!(base.needs_engine_restart(&Settings { hdr_capture: true, ..sample() }));
        assert!(!base.needs_engine_restart(&Settings { free_memory_in_games: false, weekly_reel: true, weekly_reel_tag: "funny".into(), ..sample() }));
        let tidy = Settings { weekly_reel_tag: "  #Funny ".into(), ..sample() }.sanitized();
        assert_eq!(tidy.weekly_reel_tag, "Funny");
    }

    #[test]
    fn the_webhook_from_before_1_4_moves_into_the_list() {
        let dir = std::env::temp_dir().join(format!("afterclip-webhooks-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("settings.json");
        fs::write(&file, r#"{"discordWebhook": " https://discord.com/api/webhooks/1/old ", "bufferSeconds": 120}"#).unwrap();
        let loaded = Settings::load(&file, sample());
        assert_eq!(loaded.discord_webhooks, vec![Webhook { name: "Discord".into(), url: "https://discord.com/api/webhooks/1/old".into() }]);
        assert!(loaded.discord_webhook.is_empty());
        loaded.save(&file).unwrap();
        let saved = fs::read_to_string(&file).unwrap();
        assert!(!saved.contains("\"discordWebhook\""), "the old field isn't written again");
        assert_eq!(Settings::load(&file, sample()).discord_webhooks.len(), 1, "loading again doesn't add it twice");
        let _ = fs::remove_dir_all(dir);

        let mut tidy = sample();
        tidy.discord_webhooks = vec![Webhook { name: "  ".into(), url: " https://discord.com/api/webhooks/2/x ".into() }];
        let tidy = tidy.sanitized();
        assert_eq!(tidy.discord_webhooks[0].name, "Channel 1");
        assert_eq!(tidy.discord_webhooks[0].url, "https://discord.com/api/webhooks/2/x");
    }

    #[test]
    fn importing_keeps_the_webhook_and_a_save_location_that_exists() {

        let dir = std::env::temp_dir().join(format!("afterclip-import-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("exported.json");
        let mut other = sample();
        other.buffer_seconds = 300;
        other.output_dir = "Q:\\Nowhere\\Afterclip".into();
        fs::write(&file, other.exported().to_string()).unwrap();
        let mut current = sample();
        current.discord_webhooks = vec![Webhook { name: "Clips".into(), url: "https://discord.com/api/webhooks/1/secret".into() }];
        let imported = current.imported(&file).unwrap();
        assert_eq!(imported.buffer_seconds, 300);
        assert_eq!(imported.discord_webhooks, current.discord_webhooks);
        assert_eq!(imported.output_dir, current.output_dir);
        assert!(!other.exported().to_string().contains("discordWebhook"));
        assert!(!current.exported().to_string().contains("secret"));
        fs::write(&file, "{\"hello\": 1}").unwrap();
        assert!(current.imported(&file).is_err());
        let _ = fs::remove_dir_all(dir);
    }
}

fn without_volume(sources: &[AudioSource]) -> Vec<AudioSource> {
    sources.iter().cloned().map(|mut s| { s.volume = 1.0; s }).collect()
}

/// Rough RAM the buffer will hold, which is the main cost of a long buffer.
pub fn estimate_buffer_mb(buffer_seconds: i64, bitrate_kbps: i64) -> i64 {
    (((bitrate_kbps + 320) * buffer_seconds) as f64 / 8.0 / 1024.0).round() as i64
}
