//! Finds the Stream Deck app on this PC, and whether Afterclip's plugin is in it, so the panel can offer to
//! install or update the plugin that ships with Afterclip.
use std::path::{Path, PathBuf};
use tauri::{AppHandle, Manager};
use winreg::enums::HKEY_CURRENT_USER;
use winreg::RegKey;

const PLUGIN_UUID: &str = "com.afterclip.clips";
const PLUGIN_FILE: &str = "com.afterclip.clips.streamDeckPlugin";
/// The plugin this build carries. The packed file is made from this same folder.
const BUNDLED_MANIFEST: &str = include_str!("../../streamdeck/com.afterclip.clips.sdPlugin/manifest.json");

/// What the panel offers: "install" when the Stream Deck app has no Afterclip plugin, "update" when its
/// plugin is older than this build's. None when there's no Stream Deck app, or nothing to do.
pub fn offer(app: &AppHandle) -> Option<&'static str> {
    if !app_installed() || bundled_file(app).is_none() {
        return None;
    }
    match installed_version() {
        None => Some("install"),
        Some(installed) if is_older(&installed, &bundled_version()) => Some("update"),
        Some(_) => None,
    }
}

/// The version of the plugin this build carries, which a declined offer is remembered against.
pub fn bundled_version() -> String {
    version_of(BUNDLED_MANIFEST).unwrap_or_default()
}

/// Hands the packed plugin to the Stream Deck app, which asks to install it.
pub fn install(app: &AppHandle) -> Result<PathBuf, String> {
    if !app_installed() {
        return Err("Install the Stream Deck app from Elgato first, then try again.".into());
    }
    bundled_file(app).ok_or_else(|| "This copy of Afterclip doesn't include the Stream Deck plugin.".to_string())
}

/// The Stream Deck app is installed if its program, its data folder or its settings are there.
fn app_installed() -> bool {
    let programs = ["ProgramW6432", "ProgramFiles"]
        .iter()
        .filter_map(|v| std::env::var_os(v))
        .map(|p| PathBuf::from(p).join("Elgato").join("StreamDeck").join("StreamDeck.exe"));
    programs.into_iter().any(|p| p.exists())
        || plugins_dir().is_some_and(|p| p.parent().is_some_and(Path::exists))
        || RegKey::predef(HKEY_CURRENT_USER).open_subkey(r"Software\Elgato Systems GmbH\StreamDeck").is_ok()
}

fn plugins_dir() -> Option<PathBuf> {
    std::env::var_os("APPDATA").map(|a| PathBuf::from(a).join("Elgato").join("StreamDeck").join("Plugins"))
}

/// The installed plugin's version, if the Stream Deck app has it.
fn installed_version() -> Option<String> {
    let manifest = plugins_dir()?.join(format!("{PLUGIN_UUID}.sdPlugin")).join("manifest.json");
    let text = std::fs::read_to_string(manifest).ok()?;
    // A plugin folder with an unreadable manifest is still there, so it counts as the oldest version.
    Some(version_of(&text).unwrap_or_else(|| "0".into()))
}

/// Installed builds carry the packed plugin in resources. Running from source uses the one npm run plugin makes.
fn bundled_file(app: &AppHandle) -> Option<PathBuf> {
    let bundled = app.path().resource_dir().ok().map(|r| crate::strip_verbatim(r).join("streamdeck").join(PLUGIN_FILE));
    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join("streamdeck").join("dist").join(PLUGIN_FILE);
    bundled.into_iter().chain([source]).find(|p| p.is_file())
}

fn version_of(manifest: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(manifest.trim_start_matches('\u{feff}')).ok()?;
    value.get("Version")?.as_str().map(str::to_string)
}

/// Compares dotted versions number by number, so 0.13 is newer than 0.9.
fn is_older(installed: &str, bundled: &str) -> bool {
    let parts = |v: &str| v.split('.').map(|p| p.trim().parse::<u64>().unwrap_or(0)).collect::<Vec<_>>();
    let (mut a, mut b) = (parts(installed), parts(bundled));
    let len = a.len().max(b.len());
    a.resize(len, 0);
    b.resize(len, 0);
    a < b
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_by_number() {
        assert!(is_older("0.9.0.0", "0.13.0.0"));
        assert!(is_older("0.13", "0.13.0.1"));
        assert!(!is_older("0.13.0.0", "0.13"));
        assert!(!is_older("1.0.0.0", "0.13.0.0"));
    }

    #[test]
    fn bundled_manifest_has_a_version() {
        assert!(!bundled_version().is_empty());
    }
}
