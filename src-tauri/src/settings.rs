use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

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
    /// Relaunches through UAC at every start, which some games need for game capture.
    pub run_as_admin: bool,
    /// Moves the oldest clips and recordings to the Recycle Bin above this size. 0 keeps everything.
    pub storage_limit_gb: f64,
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
    /// How much a controller clip keeps. 0 means the whole buffer.
    pub controller_clip_seconds: i64,
    /// Replaces screen and audio capture with a colour card. Used by the tests.
    pub test_pattern: bool,
}

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
            run_as_admin: false,
            storage_limit_gb: 0.0,
            game_profiles: Vec::new(),
            auto_update: true,
            skipped_update: String::new(),
            controller_enabled: false,
            controller_clip: "r3".into(),
            controller_record: String::new(),
            controller_mic: String::new(),
            controller_marker: String::new(),
            controller_clip_seconds: 30,
            test_pattern: false,
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
        s.resolved_for(display).sanitized()
    }

    pub fn sanitized(mut self) -> Self {
        self.storage_limit_gb = self.storage_limit_gb.max(0.0);
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
            || self.bitrate_kbps != other.bitrate_kbps
            // Volume changes live, so it alone never needs a restart.
            || without_volume(&self.audio_sources) != without_volume(&other.audio_sources)
            || self.game_audio != other.game_audio
            || self.game_audio_own_track != other.game_audio_own_track
            || self.test_pattern != other.test_pattern
    }
}

fn without_volume(sources: &[AudioSource]) -> Vec<AudioSource> {
    sources.iter().cloned().map(|mut s| { s.volume = 1.0; s }).collect()
}

/// Rough RAM the buffer will hold, which is the main cost of a long buffer.
pub fn estimate_buffer_mb(buffer_seconds: i64, bitrate_kbps: i64) -> i64 {
    (((bitrate_kbps + 320) * buffer_seconds) as f64 / 8.0 / 1024.0).round() as i64
}
