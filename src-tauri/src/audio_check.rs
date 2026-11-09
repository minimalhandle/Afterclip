use super::*;
use std::os::windows::process::CommandExt;
use windows::core::PCSTR;
use windows::Win32::Foundation::FreeLibrary;
use windows::Win32::System::LibraryLoader::{
    GetProcAddress, LoadLibraryExW, LOAD_LIBRARY_SEARCH_DEFAULT_DIRS,
    LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR,
};

fn sleekmeeter_installed() -> bool {
    use winreg::enums::{
        HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ, KEY_WOW64_32KEY, KEY_WOW64_64KEY,
    };
    use winreg::RegKey;

    const UNINSTALL: &str = r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall";
    [HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE]
        .into_iter()
        .any(|root| {
            let root = RegKey::predef(root);
            [KEY_WOW64_64KEY, KEY_WOW64_32KEY].into_iter().any(|view| {
                let Ok(uninstall) = root.open_subkey_with_flags(UNINSTALL, KEY_READ | view) else {
                    return false;
                };
                uninstall.enum_keys().flatten().any(|key| {
                    uninstall
                        .open_subkey(key)
                        .ok()
                        .and_then(|entry| entry.get_value::<String, _>("DisplayName").ok())
                        .is_some_and(|name| name.trim().eq_ignore_ascii_case("Sleekmeeter"))
                })
            })
        })
}

fn device_name(name: &str) -> String {
    let name = name.trim().to_ascii_lowercase();
    let name = name
        .strip_prefix("wdm: ")
        .or_else(|| name.strip_prefix("mme: "))
        .or_else(|| name.strip_prefix("ks: "))
        .unwrap_or(&name);
    name.trim().to_owned()
}

fn endpoint_label(name: &str, capture: bool, mixer: &Value) -> Option<String> {
    let name = device_name(name);
    let (base, adapter) = name.split_once(" (").unwrap_or((&name, ""));
    let mut labels = Vec::new();
    for (index, strip) in mixer["inputs"].as_array().into_iter().flatten().enumerate() {
        let device = device_name(strip["device"].as_str().unwrap_or_default());
        let (device_base, device_adapter) = device.split_once(" (").unwrap_or((&device, ""));
        let same_adapter =
            adapter.is_empty() || device_adapter.is_empty() || adapter == device_adapter;
        let vm_input = strip["endpoint"].as_str();
        let matched = if capture {
            !device.is_empty() && same_adapter && device_base == base
        } else if let Some(endpoint) = vm_input {
            base == endpoint
        } else {
            base == format!("voicemeeter in {}", index + 1)
                || (device_base.starts_with("cable")
                    && same_adapter
                    && device_base
                        .strip_suffix(" output")
                        .zip(base.strip_suffix(" input"))
                        .is_some_and(|(heard, played)| heard == played))
        };
        if matched {
            labels.push(strip["name"].as_str().unwrap_or_default().trim());
        }
    }
    for bus in mixer["outputs"].as_array().into_iter().flatten() {
        let channel = bus["channel"]
            .as_str()
            .unwrap_or_default()
            .to_ascii_lowercase();
        let device = device_name(bus["device"].as_str().unwrap_or_default());
        let matched = if capture {
            base == format!("voicemeeter out {channel}")
                || matches!(
                    (channel.as_str(), base),
                    ("b1", "voicemeeter output")
                        | ("b2", "voicemeeter aux output")
                        | ("b3", "voicemeeter vaio3 output")
                        | ("b3", "vaio3 output")
                )
        } else {
            !device.is_empty() && device == name
        };
        if matched {
            labels.push(bus["name"].as_str().unwrap_or_default().trim());
        }
    }
    // A device used by multiple differently named channels has no unambiguous alias.
    let first = *labels.first()?;
    (!first.is_empty() && labels.iter().all(|label| *label == first)).then(|| first.to_owned())
}

pub fn annotate_audio_choices(choices: &mut Value) {
    if !sleekmeeter_installed() {
        return;
    }
    let Ok(mixer) = mixer_labels() else {
        return;
    };
    for (key, capture) in [("inputs", true), ("outputs", false)] {
        if let Some(devices) = choices[key].as_array_mut() {
            for device in devices {
                if let Some(label) = device["name"]
                    .as_str()
                    .and_then(|name| endpoint_label(name, capture, &mixer))
                {
                    device["sleekmeeterName"] = json!(label);
                }
            }
        }
    }
}

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
    static REMOTE: Mutex<()> = Mutex::new(());
    let _session = REMOTE.lock().unwrap();
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
            let refresh: unsafe extern "system" fn() -> i32 =
                std::mem::transmute(symbol(b"VBVMR_IsParametersDirty\0")?);
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
                refresh();
                let (hardware_inputs, virtual_inputs, hardware, virtual_buses): (
                    usize,
                    usize,
                    usize,
                    usize,
                ) = match kind {
                    3 => (5, 3, 5, 3),
                    2 => (3, 2, 3, 2),
                    _ => (2, 1, 1, 1),
                };
                let label = |parameter: String| {
                    let mut buf = [0u16; 512];
                    if get(parameter.as_ptr(), buf.as_mut_ptr()) != 0 {
                        return String::new();
                    }
                    let end = buf.iter().position(|&unit| unit == 0).unwrap_or(buf.len());
                    String::from_utf16_lossy(&buf[..end])
                };
                let virtual_devices = [
                    "voicemeeter input",
                    "voicemeeter aux input",
                    "voicemeeter vaio3 input",
                ];
                let strips: Vec<_> = (0..hardware_inputs + virtual_inputs).map(|i| {
                    let endpoint = i.checked_sub(hardware_inputs).map(|index| virtual_devices[index]);
                    json!({ "channel": format!("Input {}", i + 1), "name": label(format!("Strip[{i}].Label\0")),
                        "device": if endpoint.is_none() { label(format!("Strip[{i}].device.name\0")) } else { String::new() },
                        "endpoint": endpoint })
                }).collect();
                let buses: Vec<_> = (0..hardware + virtual_buses)
                    .map(|i| {
                        let channel = if i < hardware {
                            format!("A{}", i + 1)
                        } else {
                            format!("B{}", i - hardware + 1)
                        };
                        json!({ "channel": channel, "name": label(format!("Bus[{i}].Label\0")),
                            "device": if i < hardware { label(format!("Bus[{i}].device.name\0")) } else { String::new() } })
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

#[cfg(test)]
mod endpoint_tests {
    use super::*;

    #[test]
    fn uses_the_channel_on_each_side_of_a_virtual_cable() {
        let mixer = json!({
            "inputs": [
                { "device": "WDM: CABLE-A Output (VB-Audio Cable A)", "name": "Discord" },
                { "device": "CABLE-B Output (VB-Audio Cable B)", "name": "Music" },
                { "endpoint": "voicemeeter input", "name": "Desktop" }
            ],
            "outputs": [{ "channel": "B2", "name": "Stream mix" }]
        });
        assert_eq!(
            endpoint_label("Cable-A Output", true, &mixer).as_deref(),
            Some("Discord")
        );
        assert_eq!(
            endpoint_label("CABLE-A Input (VB-Audio Cable A)", false, &mixer).as_deref(),
            Some("Discord")
        );
        assert_eq!(
            endpoint_label("CABLE-B Input (VB-Audio Cable B)", false, &mixer).as_deref(),
            Some("Music")
        );
        assert_eq!(
            endpoint_label(
                "Voicemeeter Input (VB-Audio Voicemeeter VAIO)",
                false,
                &mixer
            )
            .as_deref(),
            Some("Desktop")
        );
        assert_eq!(
            endpoint_label(
                "Voicemeeter Out B2 (VB-Audio Voicemeeter AUX VAIO)",
                true,
                &mixer
            )
            .as_deref(),
            Some("Stream mix")
        );
        assert_eq!(
            endpoint_label("Microphone (USB Audio Device)", true, &mixer),
            None
        );
        assert_eq!(
            endpoint_label("CABLE-A Input (Other Vendor)", false, &mixer),
            None
        );
    }

    #[test]
    fn leaves_shared_devices_with_conflicting_labels_unchanged() {
        let mixer = json!({ "inputs": [
            { "device": "CABLE-A Output", "name": "Discord" },
            { "device": "CABLE-A Output", "name": "Music" }
        ] });
        assert_eq!(endpoint_label("CABLE-A Output", true, &mixer), None);
    }
}
