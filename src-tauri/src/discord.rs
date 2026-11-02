//! Posts a clip to a Discord channel through a webhook the user pasted in. No account or hosting is involved.
use reqwest::blocking::{multipart, Client};
use reqwest::{redirect, StatusCode, Url};
use std::path::Path;
use std::time::Duration;

/// What Discord takes on a server with no boosts. A bigger clip is sent as a smaller copy.
pub const MAX_UPLOAD_BYTES: u64 = 10 * 1024 * 1024;
const UPLOAD_TIMEOUT: Duration = Duration::from_secs(300);
const HOSTS: [&str; 4] = ["discord.com", "ptb.discord.com", "canary.discord.com", "discordapp.com"];

/// Only Discord's own webhook address gets clips, so a wrong or tampered setting can't send them anywhere else.
pub fn is_webhook(address: &str, allow_local: bool) -> bool {
    let Ok(url) = Url::parse(address) else { return false };
    if !url.username().is_empty() || url.password().is_some() {
        return false;
    }
    let host = url.host_str().unwrap_or("");
    let discord = url.scheme() == "https" && HOSTS.contains(&host) && url.path().starts_with("/api/webhooks/");
    discord || (allow_local && url.scheme() == "http" && host == "127.0.0.1")
}

fn content_type(file: &Path) -> &'static str {
    match file.extension().and_then(|e| e.to_str()).map(str::to_lowercase).as_deref() {
        Some("png") => "image/png",
        Some("mov") => "video/quicktime",
        Some("mkv") => "video/x-matroska",
        _ => "video/mp4",
    }
}

fn refusal(status: StatusCode) -> String {
    match status {
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN | StatusCode::NOT_FOUND => "Discord doesn't recognise this webhook. Paste a new one in Settings, under Clips and sharing.".into(),
        StatusCode::PAYLOAD_TOO_LARGE => "This is too big for Discord. Trim it first.".into(),
        StatusCode::TOO_MANY_REQUESTS => "Discord is limiting uploads right now. Try again in a minute.".into(),
        other => format!("Discord didn't accept this (code {}).", other.as_u16()),
    }
}

/// The webhook is a secret, and network errors quote the address they failed on, so they're never passed on.
pub fn send(webhook: &str, file: &Path) -> Result<(), String> {
    let unreachable = |_| "Couldn't reach Discord. Check your connection and try again.".to_string();
    let part = multipart::Part::file(file).map_err(|e| format!("Couldn't read this file: {e}"))?.mime_str(content_type(file)).map_err(unreachable)?;
    let client = Client::builder().timeout(UPLOAD_TIMEOUT).redirect(redirect::Policy::none()).build().map_err(unreachable)?;
    let response = client.post(webhook).multipart(multipart::Form::new().part("files[0]", part)).send().map_err(unreachable)?;
    if response.status().is_success() { Ok(()) } else { Err(refusal(response.status())) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_only_discord_webhooks() {
        assert!(is_webhook("https://discord.com/api/webhooks/123/abc-DEF_1", false));
        assert!(is_webhook("https://canary.discord.com/api/webhooks/123/abc", false));
        assert!(is_webhook("https://discordapp.com/api/webhooks/123/abc", false));
        for bad in [
            "",
            "not a url",
            "http://discord.com/api/webhooks/123/abc",
            "https://discord.com/channels/123",
            "https://discord.com.evil.example/api/webhooks/123/abc",
            "https://evil.example/api/webhooks/123/abc",
            "https://discord.com@evil.example/api/webhooks/123/abc",
            "https://user@discord.com/api/webhooks/123/abc",
            "http://127.0.0.1:9000/hook",
        ] {
            assert!(!is_webhook(bad, false), "{bad}");
        }
    }

    #[test]
    fn a_local_address_is_only_for_tests() {
        assert!(is_webhook("http://127.0.0.1:9000/hook", true));
        assert!(!is_webhook("http://192.168.1.2:9000/hook", true));
    }

    #[test]
    fn an_unreachable_webhook_fails_quickly_without_quoting_it() {
        let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let file = std::env::temp_dir().join(format!("afterclip-discord-test-{closed}.mp4"));
        std::fs::write(&file, vec![0u8; MAX_UPLOAD_BYTES as usize - 1024]).unwrap();
        let started = std::time::Instant::now();
        let sent = send(&format!("http://127.0.0.1:{closed}/api/webhooks/1/secret-token"), &file);
        let _ = std::fs::remove_file(&file);
        assert!(started.elapsed() < Duration::from_secs(20), "took {:?}", started.elapsed());
        assert!(!sent.unwrap_err().contains("secret-token"));
    }

    #[test]
    fn refusals_never_quote_the_webhook() {
        for status in [StatusCode::NOT_FOUND, StatusCode::PAYLOAD_TOO_LARGE, StatusCode::TOO_MANY_REQUESTS, StatusCode::BAD_GATEWAY] {
            assert!(!refusal(status).contains("webhooks/"));
        }
    }
}
