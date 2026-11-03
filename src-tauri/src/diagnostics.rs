//! One zip of what's needed to look into a problem, with nothing private in it. Nothing is sent anywhere.
use serde_json::Value;
use std::fs;
use std::io::Write;
use std::path::Path;
use zip::write::SimpleFileOptions;

/// Writes the Windows user folder as %USERPROFILE%, which also keeps the username out. Paths in logs differ
/// in case and in how they're escaped, so each spelling is matched without regard to case.
fn scrub(text: &str, profile: &str) -> String {
    if profile.is_empty() {
        return text.to_string();
    }
    let mut out = text.to_string();
    for spelling in [profile.to_string(), profile.replace('\\', "\\\\"), profile.replace('\\', "/")] {
        let needle = spelling.to_ascii_lowercase();
        let mut result = String::with_capacity(out.len());
        let lower = out.to_ascii_lowercase();
        let mut from = 0;
        while let Some(at) = lower[from..].find(&needle) {
            result.push_str(&out[from..from + at]);
            result.push_str("%USERPROFILE%");
            from += at + needle.len();
        }
        result.push_str(&out[from..]);
        out = result;
    }
    out
}

/// The settings, without the Discord webhook, which lets anyone post to that channel.
fn settings_without_secrets(file: &Path) -> Option<String> {
    let mut settings: Value = serde_json::from_str(&fs::read_to_string(file).ok()?).ok()?;
    if let Some(map) = settings.as_object_mut() {
        map.remove("discordWebhook");
    }
    serde_json::to_string_pretty(&settings).ok()
}

/// `files` are (name in the zip, file on disk), and missing ones are left out.
pub fn write(out: &Path, files: &[(&str, &Path)], settings_file: &Path, about: &str, profile: &str) -> Result<(), String> {
    let fail = |e: &dyn std::fmt::Display| format!("Couldn't save the diagnostics: {e}");
    let mut zip = zip::ZipWriter::new(fs::File::create(out).map_err(|e| fail(&e))?);
    let options = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    let mut add = |name: &str, text: &str| -> Result<(), String> {
        zip.start_file(name, options).map_err(|e| fail(&e))?;
        zip.write_all(scrub(text, profile).as_bytes()).map_err(|e| fail(&e))
    };
    add("about.txt", about)?;
    if let Some(settings) = settings_without_secrets(settings_file) {
        add("settings.json", &settings)?;
    }
    for (name, file) in files {
        // A log can hold text that isn't valid UTF-8, which is still worth reading.
        if let Ok(bytes) = fs::read(file) {
            add(name, &String::from_utf8_lossy(&bytes))?;
        }
    }
    zip.finish().map_err(|e| fail(&e))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_user_folder_is_hidden_however_it_is_written() {
        let text = r#"C:\Users\Sam\Videos and c:\users\sam\AppData and "C:\\Users\\Sam\\out" and C:/Users/Sam/x but C:\Users\Samantha"#;
        let scrubbed = scrub(text, r"C:\Users\Sam");
        assert!(!scrubbed.to_lowercase().contains(r"users\sam\") && !scrubbed.to_lowercase().contains(r"users\\sam") && !scrubbed.contains("Users/Sam"), "{scrubbed}");
        assert_eq!(scrubbed.matches("%USERPROFILE%").count(), 5, "{scrubbed}");
    }

    #[test]
    fn the_webhook_is_left_out() {
        let file = std::env::temp_dir().join(format!("afterclip-diagnostics-{}.json", std::process::id()));
        fs::write(&file, r#"{"discordWebhook":"https://discord.com/api/webhooks/1/secret","fps":60}"#).unwrap();
        let settings = settings_without_secrets(&file).unwrap();
        let _ = fs::remove_file(&file);
        assert!(!settings.contains("secret") && settings.contains("\"fps\": 60"), "{settings}");
    }
}
