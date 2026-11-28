//! Anonymous usage count, only when the user says yes. Once a day Afterclip sends a random install ID and its
//! version to afterclip.app, so we can count how many copies are in use. Nothing else: no names, games, clips,
//! settings or hardware.
//!
//! The ID lives in usage.json next to the settings (not in settings.json, which Export settings copies to
//! other PCs) and is deleted when the user says no, so saying yes again starts a new one.
use crate::{api, Shared};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::mpsc::{channel, Sender};
use std::sync::{Mutex, OnceLock};
use std::thread;
use std::time::Duration;

const URL: &str = "https://afterclip.app/api/ping";
/// Wait this long after starting, so the ping never slows startup.
const FIRST: Duration = Duration::from_secs(60);
/// Look again this often (it sends at most once a day).
const EVERY: Duration = Duration::from_secs(600);

/// Wakes the thread when the answer changes, so a yes is counted (and a no forgets the ID) at once.
static WAKE: OnceLock<Mutex<Sender<()>>> = OnceLock::new();

/// Call when the user changes their answer.
pub fn nudge() {
    if let Some(tx) = WAKE.get() {
        let _ = tx.lock().unwrap().send(());
    }
}

fn file(core: &Shared) -> PathBuf {
    core.settings_file.with_file_name("usage.json")
}

/// A new random ID: 32 hex characters, from the operating system's random source.
fn new_id() -> String {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).expect("the system random source failed");
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn valid_id(id: &str) -> bool {
    id.len() == 32 && id.chars().all(|c| c.is_ascii_hexdigit())
}

/// Today in UTC as YYYY-MM-DD, the day the server counts in.
fn today() -> String {
    chrono::Utc::now().format("%Y-%m-%d").to_string()
}

/// What gets sent. Kept here in one place so it's easy to check.
fn payload(id: &str) -> Value {
    json!({ "id": id, "version": env!("CARGO_PKG_VERSION") })
}

fn send(body: &Value) -> Result<(), String> {
    reqwest::blocking::Client::builder()
        .user_agent(concat!("Afterclip/", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(8))
        .build()
        .map_err(|e| e.to_string())?
        .post(URL)
        .header("Content-Type", "application/json")
        .body(body.to_string())
        .send()
        .and_then(|r| r.error_for_status())
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Sends today's ping if the user agreed and it hasn't gone yet; forgets the ID if they didn't. False only if
/// sending failed.
fn tick(core: &Shared) -> bool {
    let on = core.settings.lock().unwrap().usage_ping == Some(true);
    let path = file(core);
    if !on {
        let _ = std::fs::remove_file(&path);
        return true;
    }
    let saved: Value = std::fs::read_to_string(&path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_else(|| json!({}));
    let id = match saved["id"].as_str().filter(|id| valid_id(id)) {
        Some(id) => id.to_string(),
        None => new_id(),
    };
    let day = today();
    if saved["id"] == json!(id) && saved["sent"] == json!(day) {
        return true;
    }
    match send(&payload(&id)) {
        Ok(()) => {
            let _ = std::fs::write(&path, json!({ "id": id, "sent": day }).to_string());
            true
        }
        // Offline or the site is down: try again later, quietly.
        Err(e) => {
            core.log_ui(&format!("Usage count not sent this time: {e}"));
            false
        }
    }
}

pub fn start(core: Shared) {
    // Development builds and test runs aren't real users.
    if cfg!(debug_assertions) || api::is_test_instance() {
        return;
    }
    let (tx, rx) = channel();
    let _ = WAKE.set(Mutex::new(tx));
    thread::spawn(move || {
        // Wait for startup to finish, or for an answer.
        let _ = rx.recv_timeout(FIRST);
        // While offline, each try waits longer than the last, up to a day, instead of every 10 minutes.
        let mut failures = 0;
        loop {
            failures = if tick(&core) { 0 } else { failures + 1 };
            let wait = if failures == 0 { EVERY } else { crate::updater::backoff(failures, EVERY, Duration::from_secs(24 * 60 * 60)) };
            let _ = rx.recv_timeout(wait);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_random_hex() {
        let (a, b) = (new_id(), new_id());
        assert!(valid_id(&a) && valid_id(&b));
        assert_ne!(a, b);
        assert!(!valid_id("not-an-id"));
    }

    #[test]
    fn days_are_utc_calendar_days() {
        let day = today();
        assert_eq!(day.len(), 10);
        assert_eq!(&day[4..5], "-");
    }

    #[test]
    fn sends_only_the_id_and_version() {
        let p = payload("0123456789abcdef0123456789abcdef");
        let keys: Vec<&String> = p.as_object().unwrap().keys().collect();
        assert_eq!(keys, ["id", "version"]);
        assert_eq!(p["version"], env!("CARGO_PKG_VERSION"));
    }
}
