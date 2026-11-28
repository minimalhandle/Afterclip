//! Uploads a clip to a server you choose, such as a self-hosted file host, and returns the link it answers with.
//! The address and token are secrets, so they never appear in messages, logs or diagnostics.

use reqwest::blocking::{multipart, Client};
use reqwest::{redirect, StatusCode, Url};
use serde_json::Value;
use std::path::Path;
use std::time::Duration;

const UPLOAD_TIMEOUT: Duration = Duration::from_secs(600);
/// The form field the file goes in when none is set. Most self-hosted uploaders take this.
pub const DEFAULT_FIELD: &str = "file";
const MAX_FIELD_CHARS: usize = 40;
const UNREACHABLE: &str = "Couldn't reach your upload server. Check your connection and the address in Settings, under Clips and sharing.";

/// Only HTTPS, so the clip and token never cross the network in the clear. A test copy may use its own PC.
pub fn is_address(address: &str, allow_local: bool) -> bool {
    let Ok(url) = Url::parse(address.trim()) else { return false };
    if !url.username().is_empty() || url.password().is_some() || url.host_str().is_none_or(str::is_empty) {
        return false;
    }
    url.scheme() == "https" || (allow_local && url.scheme() == "http" && url.host_str() == Some("127.0.0.1"))
}

/// A form field name a server can read: letters, digits, `_`, `-`, `[` and `]`, or the default.
pub fn clean_field(typed: &str) -> String {
    let field: String = typed.trim().chars()
        .filter(|c| c.is_ascii_alphanumeric() || "_-[]".contains(*c))
        .take(MAX_FIELD_CHARS)
        .collect();
    if field.is_empty() { DEFAULT_FIELD.into() } else { field }
}

fn is_link(text: &str) -> bool {
    Url::parse(text).is_ok_and(|u| matches!(u.scheme(), "http" | "https") && u.host_str().is_some())
}

/// The link in a server's answer: plain text, or JSON with it under a key such as url, link or files. Keys that
/// usually hold the link are looked at first, at each level, so a thumbnail or deletion link isn't taken instead.
pub fn link_in(body: &str) -> Option<String> {
    fn find(value: &Value) -> Option<String> {
        match value {
            Value::String(text) => is_link(text.trim()).then(|| text.trim().to_string()),
            Value::Array(items) => items.iter().find_map(find),
            Value::Object(map) => {
                const LIKELY: [&str; 7] = ["url", "link", "href", "files", "file", "data", "result"];
                const UNLIKELY: [&str; 4] = ["delete", "deletion", "thumb", "thumbnail"];
                let likely = LIKELY.iter().filter_map(|key| map.iter().find(|(k, _)| k.eq_ignore_ascii_case(key))).map(|(_, v)| v);
                let rest = map.iter()
                    .filter(|(k, _)| !LIKELY.iter().any(|l| k.eq_ignore_ascii_case(l)))
                    .filter(|(k, _)| !UNLIKELY.iter().any(|u| k.to_lowercase().contains(u)))
                    .map(|(_, v)| v);
                likely.chain(rest).find_map(find)
            }
            _ => None,
        }
    }
    let body = body.trim();
    match serde_json::from_str::<Value>(body) {
        Ok(value) => find(&value),
        Err(_) => body.split_whitespace().find(|word| is_link(word)).map(str::to_string),
    }
}

fn refusal(status: StatusCode) -> String {
    match status {
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => "Your upload server refused the token. Check it in Settings, under Clips and sharing.".into(),
        StatusCode::NOT_FOUND | StatusCode::METHOD_NOT_ALLOWED => "Your upload server has nothing at that address. Check it in Settings, under Clips and sharing.".into(),
        StatusCode::PAYLOAD_TOO_LARGE => "This is too big for your upload server. Trim or compress it first.".into(),
        StatusCode::TOO_MANY_REQUESTS => "Your upload server is limiting uploads right now. Try again in a minute.".into(),
        other => format!("Your upload server didn't accept this (code {}).", other.as_u16()),
    }
}

fn content_type(file: &Path) -> &'static str {
    match file.extension().and_then(|e| e.to_str()).map(str::to_lowercase).as_deref() {
        Some("png") => "image/png",
        Some("mov") => "video/quicktime",
        Some("mkv") => "video/x-matroska",
        _ => "video/mp4",
    }
}

/// Posts the file and returns the link the server answers with. It runs on a thread of its own, because
/// reqwest's blocking client panics on the async runtime's threads, where Tauri runs async commands.
pub fn send(address: &str, token: &str, field: &str, file: &Path) -> Result<String, String> {
    let (address, token, field, file) = (address.trim().to_string(), token.trim().to_string(), clean_field(field), file.to_path_buf());
    std::thread::spawn(move || send_here(&address, &token, &field, &file))
        .join()
        .unwrap_or_else(|_| Err(UNREACHABLE.into()))
}

fn send_here(address: &str, token: &str, field: &str, file: &Path) -> Result<String, String> {
    // Network errors quote the address, which is a secret, so they're never passed on.
    let unreachable = |_| UNREACHABLE.to_string();
    let name = file.file_name().unwrap_or_default().to_string_lossy().into_owned();
    let part = multipart::Part::file(file).map_err(|e| format!("Couldn't read this file: {e}"))?
        .file_name(name)
        .mime_str(content_type(file)).map_err(unreachable)?;
    let client = Client::builder().timeout(UPLOAD_TIMEOUT).redirect(redirect::Policy::none()).build().map_err(unreachable)?;
    let mut request = client.post(address).multipart(multipart::Form::new().part(field.to_string(), part));
    if !token.is_empty() {
        request = request.bearer_auth(token);
    }
    let response = request.send().map_err(unreachable)?;
    if !response.status().is_success() {
        return Err(refusal(response.status()));
    }
    let body = response.text().map_err(unreachable)?;
    link_in(&body).ok_or_else(|| "Your upload server took the clip, but didn't answer with a link to it.".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    #[test]
    fn only_https_addresses_get_clips() {
        assert!(is_address("https://clips.example.com/api/upload", false));
        assert!(is_address(" https://example.com ", false));
        for bad in ["", "not a url", "http://clips.example.com/upload", "ftp://example.com", "https://user:pass@example.com/upload", "http://127.0.0.1:9000/up"] {
            assert!(!is_address(bad, false), "{bad}");
        }
        assert!(is_address("http://127.0.0.1:9000/up", true));
        assert!(!is_address("http://192.168.1.2:9000/up", true));
    }

    #[test]
    fn field_names_are_tidied() {
        assert_eq!(clean_field(""), "file");
        assert_eq!(clean_field(" upload "), "upload");
        assert_eq!(clean_field("files[]"), "files[]");
        assert_eq!(clean_field("a\"; evil"), "aevil");
    }

    #[test]
    fn links_are_found_in_common_answers() {
        assert_eq!(link_in("https://x.example/a.mp4\n").as_deref(), Some("https://x.example/a.mp4"));
        assert_eq!(link_in(r#"{"url":"https://x.example/a"}"#).as_deref(), Some("https://x.example/a"));
        assert_eq!(link_in(r#"{"files":["https://x.example/b"]}"#).as_deref(), Some("https://x.example/b"));
        assert_eq!(link_in(r#"{"data":{"link":"https://x.example/c"}}"#).as_deref(), Some("https://x.example/c"));
        assert_eq!(link_in(r#"{"deletion_url":"https://x.example/del","thumb":"https://x.example/t","link":"https://x.example/d"}"#).as_deref(), Some("https://x.example/d"));
        assert_eq!(link_in(r#"{"delete":"https://x.example/del","page":"https://x.example/e"}"#).as_deref(), Some("https://x.example/e"));
        assert_eq!(link_in(r#"{"ok":true}"#), None);
        assert_eq!(link_in("Uploaded!"), None);
    }

    /// A one-request server that checks what it's sent and answers with `reply`.
    fn serve_once(reply: &'static str) -> (String, std::thread::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = format!("http://127.0.0.1:{}/upload", listener.local_addr().unwrap().port());
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            let mut seen = Vec::new();
            let mut buf = [0u8; 8192];
            // Reads until the multipart body's closing boundary.
            while let Ok(n) = stream.read(&mut buf) {
                if n == 0 { break; }
                seen.extend_from_slice(&buf[..n]);
                if seen.windows(4).any(|w| w == b"--\r\n") && String::from_utf8_lossy(&seen).contains("hello clip") { break; }
            }
            stream.write_all(reply.as_bytes()).unwrap();
            String::from_utf8_lossy(&seen).into_owned()
        });
        (address, server)
    }

    #[test]
    fn uploads_with_the_token_and_field_and_returns_the_link() {
        let (address, server) = serve_once("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 34\r\nConnection: close\r\n\r\n{\"url\":\"https://x.example/clip1\"}\n");
        let file = std::env::temp_dir().join(format!("afterclip-upload-test-{}.mp4", std::process::id()));
        std::fs::write(&file, b"hello clip").unwrap();
        let link = send(&address, "secret-token", "upload", &file);
        let seen = server.join().unwrap();
        let _ = std::fs::remove_file(&file);
        assert_eq!(link.as_deref(), Ok("https://x.example/clip1"));
        assert!(seen.contains("authorization: Bearer secret-token") || seen.contains("Authorization: Bearer secret-token"), "{seen}");
        assert!(seen.contains("name=\"upload\""), "{seen}");
        assert!(seen.contains("video/mp4"), "{seen}");
    }

    #[test]
    fn a_refusal_never_quotes_the_address() {
        let (address, server) = serve_once("HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        let file = std::env::temp_dir().join(format!("afterclip-upload-refused-{}.mp4", std::process::id()));
        std::fs::write(&file, b"hello clip").unwrap();
        let refused = send(&address, "", "", &file).unwrap_err();
        let _ = server.join();
        let _ = std::fs::remove_file(&file);
        assert!(refused.contains("refused the token") && !refused.contains("127.0.0.1"), "{refused}");
    }
}
