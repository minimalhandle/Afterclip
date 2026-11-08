use super::*;
use std::os::windows::process::CommandExt;
use windows::core::PCSTR;
use windows::Win32::Foundation::FreeLibrary;
use windows::Win32::System::LibraryLoader::{
    GetProcAddress, LoadLibraryExW, LOAD_LIBRARY_SEARCH_DEFAULT_DIRS,
    LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR,
};

fn voicemeeter_dll() -> Option<PathBuf> {
    let reg = PathBuf::from(std::env::var_os("SystemRoot")?).join("System32/reg.exe");
    let key = r"HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\VB:Voicemeeter {17359A74-1236-5467}";
    for view in ["/reg:64", "/reg:32"] {
        let result = Command::new(&reg)
            .args(["query", key, "/v", "UninstallString", view])
            .creation_flags(0x0800_0000)
            .output()
            .ok()?;
        if !result.status.success() {
            continue;
        }
        let text = String::from_utf8_lossy(&result.stdout);
        let exe = text.lines().find_map(|line| {
            line.split_once("REG_SZ")
                .map(|(_, value)| value.trim().trim_matches('"'))
        })?;
        return Path::new(exe)
            .parent()
            .map(|folder| folder.join("VoicemeeterRemote64.dll"));
    }
    None
}

fn mixer_labels() -> Result<Value, String> {
    let path = voicemeeter_dll()
        .filter(|path| path.is_file())
        .ok_or("Voicemeeter was not found on this PC.")?;
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    unsafe {
        let library = LoadLibraryExW(
            PCWSTR(wide.as_ptr()),
            None,
            LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_DEFAULT_DIRS,
        )
        .map_err(|e| e.to_string())?;
        let result = (|| {
            let symbol = |name: &'static [u8]| {
                GetProcAddress(library, PCSTR(name.as_ptr()))
                    .ok_or("Voicemeeter Remote API is unavailable.")
            };
            let login: unsafe extern "system" fn() -> i32 =
                std::mem::transmute(symbol(b"VBVMR_Login\0")?);
            let logout: unsafe extern "system" fn() -> i32 =
                std::mem::transmute(symbol(b"VBVMR_Logout\0")?);
            let edition: unsafe extern "system" fn(*mut i32) -> i32 =
                std::mem::transmute(symbol(b"VBVMR_GetVoicemeeterType\0")?);
            let get: unsafe extern "system" fn(*const u8, *mut u16) -> i32 =
                std::mem::transmute(symbol(b"VBVMR_GetParameterStringW\0")?);
            let connected = login();
            if connected < 0 {
                return Err("Couldn't connect to Voicemeeter.".into());
            }
            let mut kind = 0;
            let status = edition(&mut kind);
            let labels = if connected != 0 || status != 0 {
                Err("Start Voicemeeter to read the channel names.".into())
            } else {
                let (inputs, hardware, virtual_buses) = match kind {
                    3 => (8, 5, 3),
                    2 => (5, 3, 2),
                    _ => (3, 1, 1),
                };
                let label = |parameter: String| {
                    let mut buf = [0u16; 512];
                    if get(parameter.as_ptr(), buf.as_mut_ptr()) != 0 {
                        return String::new();
                    }
                    let end = buf.iter().position(|&unit| unit == 0).unwrap_or(buf.len());
                    String::from_utf16_lossy(&buf[..end])
                };
                let strips: Vec<_> = (0..inputs).map(|i| json!({ "channel": format!("Input {}", i + 1), "name": label(format!("Strip[{i}].Label\0")) })).collect();
                let buses: Vec<_> = (0..hardware + virtual_buses)
                    .map(|i| {
                        let channel = if i < hardware {
                            format!("A{}", i + 1)
                        } else {
                            format!("B{}", i - hardware + 1)
                        };
                        json!({ "channel": channel, "name": label(format!("Bus[{i}].Label\0")) })
                    })
                    .collect();
                Ok(json!({ "inputs": strips, "outputs": buses }))
            };
            logout();
            labels
        })();
        let _ = FreeLibrary(library);
        result
    }
}

#[tauri::command(async)]
pub fn audio_setup(core: tauri::State<Shared>) -> Value {
    let settings = core.settings.lock().unwrap().clone();
    let mixer = mixer_labels();
    let sources: Vec<_> = settings.audio_sources.iter().enumerate()
        .map(|(index, source)| json!({ "index": index, "label": source.label, "kind": source.kind, "ownTrack": source.own_track })).collect();
    json!({ "sources": sources, "gameTrack": settings.game_audio_own_track,
        "mixer": mixer.as_ref().ok(), "mixerError": mixer.err() })
}

#[tauri::command(async)]
pub fn apply_audio_roles(
    core: tauri::State<Shared>,
    payload: [Option<i32>; 3],
) -> Result<(), String> {
    let mut settings = core.settings.lock().unwrap().clone();
    let mut seen = std::collections::HashSet::new();
    let added = payload
        .iter()
        .flatten()
        .filter(|&&index| index >= 0)
        .filter(|&&index| {
            settings
                .audio_sources
                .get(index as usize)
                .is_some_and(|source| !source.own_track)
        })
        .count();
    let game_track = usize::from(settings.game_audio_own_track || payload[0] == Some(-1));
    if settings
        .audio_sources
        .iter()
        .filter(|source| source.own_track)
        .count()
        + added
        + game_track
        > 5
    {
        return Err(
            "Afterclip can capture up to five separate tracks. Disable an unused track first."
                .into(),
        );
    }
    for (role, index) in ["Game", "Mic", "Chat"].into_iter().zip(payload) {
        let Some(index) = index else {
            continue;
        };
        if !seen.insert(index) {
            return Err("Choose a different source for each role.".into());
        }
        if index == -1 && role == "Game" {
            settings.game_audio = true;
            settings.game_audio_own_track = true;
            continue;
        }
        let source = settings
            .audio_sources
            .get_mut(index as usize)
            .ok_or("That audio source is unavailable.")?;
        source.label = format!(
            "{role} · {}",
            source
                .label
                .split_once(" · ")
                .map_or(source.label.as_str(), |(_, name)| name)
        );
        source.own_track = true;
    }
    apply_settings(&core, settings).map(|_| ())
}

fn samples(path: &Path) -> Result<Vec<f64>, String> {
    let bytes = fs::read(path).map_err(|e| e.to_string())?;
    let pcm = bytes.get(44..).ok_or("Couldn't read the test audio.")?;
    Ok(pcm
        .chunks_exact(2)
        .map(|x| f64::from(i16::from_le_bytes([x[0], x[1]])) / 32768.0)
        .collect())
}

fn similar(a: &[f64], b: &[f64]) -> bool {
    let count = a.len().min(b.len());
    if count < 8000 {
        return false;
    }
    let mut aa = 0.0;
    let mut bb = 0.0;
    let mut ab = 0.0;
    for (&x, &y) in a.iter().zip(b).take(count) {
        aa += x * x;
        bb += y * y;
        ab += x * y;
    }
    aa > 0.01 && bb > 0.01 && ab / (aa * bb).sqrt() > 0.96
}

#[tauri::command(async)]
pub fn test_audio_tracks(core: tauri::State<Shared>) -> Result<Value, String> {
    let _job = core
        .analysis_lock
        .try_lock()
        .map_err(|_| "Another audio analysis is running.")?;
    // Allow the user to speak or play a sound after pressing Test.
    thread::sleep(Duration::from_secs(4));
    let sample = core.engine.scratch_file("audio-check.mp4")?;
    let result = (|| {
        core.engine.checkpoint_replay(&sample, 4)?;
        let count = core.engine.track_count(&sample)?.clamp(0, 6) as usize;
        let mut tracks: Vec<(usize, Vec<f64>)> = Vec::new();
        let mut result = Vec::new();
        for index in 0..count {
            let wav = core.engine.scratch_file("audio-check.wav")?;
            let audio = core
                .engine
                .wave(&sample, &wav, &[index])
                .and_then(|()| samples(&wav));
            let _ = fs::remove_file(wav);
            match audio {
                Ok(audio) => {
                    let peak = audio.iter().map(|x| x.abs()).fold(0.0, f64::max);
                    let db = 20.0 * peak.max(0.00001).log10();
                    let duplicate = (index > 0)
                        .then(|| {
                            tracks
                                .iter()
                                .find(|(track, other)| *track > 0 && similar(&audio, other))
                                .map(|(track, _)| track + 1)
                        })
                        .flatten();
                    result.push(json!({ "track": index + 1, "db": db.round(), "silent": db < -50.0, "duplicate": duplicate }));
                    tracks.push((index, audio));
                }
                Err(error) => result.push(json!({ "track": index + 1, "error": error })),
            }
        }
        Ok(json!(result))
    })();
    let _ = fs::remove_file(sample);
    result
}
