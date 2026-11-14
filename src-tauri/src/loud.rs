//! Hears loud moments, such as you shouting into your mic or the game going off, to save a clip by itself.
//! It reads the peak meters Windows already keeps for every device and app, so it adds no audio processing
//! and nothing to the capture engine.
use windows::core::{Interface, HSTRING};
use windows::Win32::Media::Audio::Endpoints::IAudioMeterInformation;
use windows::Win32::Media::Audio::{eCapture, eConsole, eRender, IAudioSessionControl2, IAudioSessionManager2, IMMDeviceEnumerator, MMDeviceEnumerator, DEVICE_STATE_ACTIVE};
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_ALL};

/// What to listen to: a microphone by its device id ("default" for Windows' default), or an app's sound.
#[derive(Clone, Debug, PartialEq)]
pub enum Source {
    Mic(String),
    App(u32),
}

/// The meters for one source, kept between readings so each one is a single cheap call.
pub struct Listener {
    meters: Vec<IAudioMeterInformation>,
}

impl Listener {
    /// Needs COM on the calling thread, as win32::init_com_for_thread sets up.
    pub fn open(source: Source) -> Option<Listener> {
        let meters = unsafe {
            match &source {
                Source::Mic(device) => mic_meter(device).ok().into_iter().collect(),
                Source::App(pid) => app_meters(*pid).unwrap_or_default(),
            }
        };
        (!meters.is_empty()).then_some(Listener { meters })
    }

    /// The loudest peak right now, in dB below full scale. Silence reads as -100.
    pub fn peak_db(&self) -> f64 {
        let peak = self.meters.iter().filter_map(|m| unsafe { m.GetPeakValue().ok() }).fold(0.0f32, f32::max);
        to_db(peak)
    }
}

pub fn to_db(peak: f32) -> f64 {
    if peak <= 0.000_01 { -100.0 } else { 20.0 * (peak as f64).log10() }
}

unsafe fn mic_meter(device: &str) -> windows::core::Result<IAudioMeterInformation> {
    let devices: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
    let chosen = if device.is_empty() || device == "default" {
        devices.GetDefaultAudioEndpoint(eCapture, eConsole)?
    } else {
        // A device that's gone, such as an unplugged headset, falls back to the default.
        match devices.GetDevice(&HSTRING::from(device)) {
            Ok(found) => found,
            Err(_) => devices.GetDefaultAudioEndpoint(eCapture, eConsole)?,
        }
    };
    chosen.Activate(CLSCTX_ALL, None)
}

/// One meter for each place the app is playing sound, since a game can play through more than one device.
unsafe fn app_meters(pid: u32) -> windows::core::Result<Vec<IAudioMeterInformation>> {
    let devices: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
    let all = devices.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE)?;
    let mut meters = Vec::new();
    for i in 0..all.GetCount()? {
        let Ok(device) = all.Item(i) else { continue };
        let Ok(manager) = device.Activate::<IAudioSessionManager2>(CLSCTX_ALL, None) else { continue };
        let Ok(sessions) = manager.GetSessionEnumerator() else { continue };
        for k in 0..sessions.GetCount().unwrap_or(0) {
            let Ok(session) = sessions.GetSession(k) else { continue };
            let Ok(control) = session.cast::<IAudioSessionControl2>() else { continue };
            if control.GetProcessId().ok() == Some(pid) {
                if let Ok(meter) = session.cast::<IAudioMeterInformation>() {
                    meters.push(meter);
                }
            }
        }
    }
    Ok(meters)
}

/// Decides when a run of readings is a moment: loud for most of a short window, so one click or bump
/// doesn't count, then quiet for a while before the next one can.
pub struct Detector {
    recent: u32,
    readings: u32,
}

/// Readings in the window, at the listener's rate of ten a second, and how many must be loud.
pub const WINDOW: u32 = 5;
pub const NEEDED: u32 = 3;

impl Detector {
    pub fn new() -> Self {
        Detector { recent: 0, readings: 0 }
    }

    /// Takes one reading and says whether it completes a moment.
    pub fn hear(&mut self, db: f64, level_db: f64) -> bool {
        self.recent = (self.recent << 1 | (db >= level_db) as u32) & ((1 << WINDOW) - 1);
        self.readings = (self.readings + 1).min(WINDOW);
        if self.readings == WINDOW && self.recent.count_ones() >= NEEDED {
            self.recent = 0;
            self.readings = 0;
            return true;
        }
        false
    }

    pub fn reset(&mut self) {
        self.recent = 0;
        self.readings = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_single_bump_is_not_a_moment_but_a_shout_is() {
        let mut d = Detector::new();
        let heard: Vec<bool> = [-3.0, -40.0, -40.0, -40.0, -40.0, -40.0].iter().map(|&db| d.hear(db, -6.0)).collect();
        assert!(!heard.contains(&true), "one loud reading is a click, not a moment");
        let mut d = Detector::new();
        let heard: Vec<bool> = [-3.0, -2.0, -40.0, -4.0, -40.0].iter().map(|&db| d.hear(db, -6.0)).collect();
        assert_eq!(heard, [false, false, false, false, true]);
        assert!(!d.hear(-3.0, -6.0), "it starts counting again after a moment");
    }

    #[test]
    fn silence_reads_very_quiet() {
        assert_eq!(to_db(0.0), -100.0);
        assert!((to_db(1.0)).abs() < 1e-9);
        assert!((to_db(0.5) + 6.02).abs() < 0.01);
    }
}
