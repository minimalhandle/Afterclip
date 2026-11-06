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
    /// Where Send to Discord posts. It's a secret, so it stays out of logs and messages.
    pub discord_webhook: String,
    /// Relaunches through UAC at every start, which some games need for game capture.
    pub run_as_admin: bool,
    /// Moves the oldest clips and recordings to the Recycle Bin above this size. 0 keeps everything.
    pub storage_limit_gb: f64,
    /// Moves clips, recordings and screenshots to the Recycle Bin after this many days. 0 keeps them.
    pub delete_after_days: i64,
    pub game_profiles: Vec<GameProfile>,
    /// Checks for updates and installs them when it's safe to.
    pub auto_update: bool,
    /// The version someone chose to skip, which isn't offered again.
    pub skipped_update: String,
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
            quality: "high".into(),
            resolution: "native".into(),
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
            bitrate_kbps: 40000,
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
            run_as_admin: false,
            storage_limit_gb: 0.0,
            delete_after_days: 0,
            game_profiles: Vec::new(),
            auto_update: true,
            skipped_update: String::new(),
            controller_enabled: false,
            controller_clip: "r3".into(),
            controller_record: String::new(),
            controller_mic: String::new(),
            controller_marker: String::new(),
            controller_screenshot: String::new(),
            controller_clip_seconds: 30,
            clip_after_seconds: 0,
            buffer_only_in_games: false,
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
        }
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
        self.discord_webhook = self.discord_webhook.trim().to_string();
        self.clip_after_seconds = self.clip_after_seconds.clamp(0, 30);
        self.camera_size = self.camera_size.clamp(CAMERA_SIZES.0, CAMERA_SIZES.1);
        self.low_space_warn_gb = self.low_space_warn_gb.max(0.0);
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
            || self.bitrate_kbps != other.bitrate_kbps
            // Volume changes live, so it alone never needs a restart.
            || without_volume(&self.audio_sources) != without_volume(&other.audio_sources)
            || self.game_audio != other.game_audio
            || self.game_audio_own_track != other.game_audio_own_track
            || self.test_pattern != other.test_pattern
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
        settings.discord_webhook = self.discord_webhook.clone();
        settings.skipped_update = self.skipped_update.clone();
        settings.test_pattern = self.test_pattern;
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

    /// What Export settings writes. The webhook is a secret, so it's left out.
    pub fn exported(&self) -> serde_json::Value {
        let mut value = serde_json::to_value(self).unwrap_or_default();
        if let Some(map) = value.as_object_mut() {
            for key in ["discordWebhook", "skippedUpdate", "testPattern", "micMuted"] {
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
    fn vertical_mode_narrows_the_picture_and_keeps_the_resolution() {
        let mut s = sample();
        s.vertical_mode = true;
        assert_eq!(s.canvas(), (606, 1080));
        assert_eq!((s.width, s.height), (1920, 1080), "the saved resolution is kept for when it's off");
        s.camera_device = "cam".into();
        assert_eq!(s.camera_box(), Some((0, 0, 606, 340)), "the webcam goes across the top");
        assert!(sample().needs_engine_restart(&s));
        let profile = GameProfile { exe: "game.exe".into(), name: "Game".into(), quality: None, frame_rate: None, buffer_seconds: None, game_audio: None, vertical: Some(true), auto_record: false };
        assert_eq!(sample().with_profile(&profile, (1920, 1080, 60)).canvas(), (606, 1080));
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
        current.discord_webhook = "https://discord.com/api/webhooks/1/secret".into();
        let imported = current.imported(&file).unwrap();
        assert_eq!(imported.buffer_seconds, 300);
        assert_eq!(imported.discord_webhook, current.discord_webhook);
        assert_eq!(imported.output_dir, current.output_dir);
        assert!(!other.exported().to_string().contains("discordWebhook"));
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
