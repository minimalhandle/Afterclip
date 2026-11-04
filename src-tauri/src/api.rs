use serde_json::{json, Value};
use std::sync::Arc;
use std::thread;
use tiny_http::{Header, Method, Response, Server};

const DEFAULT_PORT: &str = "4467";

/// True for a copy started by the tests, which must not touch a real running copy.
/// Never while elevated: these variables redirect files and open a debug port, and anything can set them.
pub fn is_test_instance() -> bool {
    std::env::var_os("AFTERCLIP_HOME").is_some() && !crate::win32::is_self_elevated()
}

pub trait ApiHandlers: Send + Sync + 'static {
    /// Opens a window on request. Only reachable in a test instance.
    fn test_open(&self, what: &str);
    /// Stops the panel closing on focus loss. Only reachable in a test instance.
    fn test_hold_panel(&self, hold: bool);
    /// Pretends this program is the running game. Empty means none. Only reachable in a test instance.
    fn test_game(&self, exe: &str);
    fn test_controller(&self, kind: &str, report: &str);
    /// Pretends the drive has this much free. Only reachable in a test instance.
    fn test_free_space(&self, bytes: Option<u64>);
    /// Crashes the engine, or stops its buffer as an encoder error would. Only reachable in a test instance.
    fn test_engine(&self, what: &str);
    /// Pretends the push-to-talk key is held or let go. Only reachable in a test instance.
    fn test_talk(&self, held: bool);
    fn clip(&self, seconds: i64) -> Result<Value, String>;
    fn toggle_record(&self) -> Result<Value, String>;
    fn toggle_mic(&self) -> Result<Value, String>;
    fn toggle_clean_mic(&self) -> Result<Value, String>;
    fn add_marker(&self) -> Result<Value, String>;
    fn screenshot(&self) -> Result<Value, String>;
    fn open_panel(&self) -> Result<Value, String>;
    fn open_clips_folder(&self) -> Result<Value, String>;
    fn edit_last_clip(&self) -> Result<Value, String>;
    fn copy_last_clip(&self) -> Result<Value, String>;
    fn compress_last_clip(&self) -> Result<Value, String>;
    fn send_last_clip(&self) -> Result<Value, String>;
    fn vertical_last_clip(&self) -> Result<Value, String>;
    fn star_last_clip(&self) -> Result<Value, String>;
    fn toggle_camera(&self) -> Result<Value, String>;
    /// The Stream Deck's Hold to Talk key going down or up.
    fn deck_talk(&self, held: bool) -> Result<Value, String>;
    /// Turns every mute-key source up or down by this many percent.
    fn change_mic_volume(&self, percent: i64) -> Result<Value, String>;
    fn status(&self) -> Value;
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |diff, (x, y)| diff | (x ^ y)) == 0
}

/// Local control API for the Stream Deck plugin. It listens on loopback only and needs a
/// bearer token, and it sends no CORS headers, so a web page can't trigger a clip.
pub fn start_api(token: String, handlers: Arc<dyn ApiHandlers>) -> Result<Arc<Server>, String> {
    let port = std::env::var("AFTERCLIP_API_PORT").ok().filter(|_| is_test_instance()).unwrap_or_else(|| DEFAULT_PORT.into());
    let server = Arc::new(Server::http(format!("127.0.0.1:{port}")).map_err(|e| e.to_string())?);
    let expected = format!("Bearer {token}");
    let listener = Arc::clone(&server);
    thread::spawn(move || {
        // incoming_requests ends when the server is told to unblock, which is how the API stops.
        for request in listener.incoming_requests() {
            let authorized = request
                .headers()
                .iter()
                .find(|h| h.field.equiv("Authorization"))
                .is_some_and(|h| constant_time_eq(h.value.as_str().as_bytes(), expected.as_bytes()));
            let handlers = Arc::clone(&handlers);
            // Clips take a moment, and must not hold up other requests.
            thread::spawn(move || {
                let (code, body) = if authorized {
                    route(request.method(), request.url(), handlers.as_ref())
                } else {
                    (401, json!({ "error": "unauthorized" }))
                };
                let header = Header::from_bytes("Content-Type", "application/json").expect("valid header");
                let response = Response::from_string(body.to_string()).with_status_code(code).with_header(header);
                let _ = request.respond(response);
            });
        }
    });
    Ok(server)
}

fn route(method: &Method, url: &str, handlers: &dyn ApiHandlers) -> (u16, Value) {
    let result = match (method, url) {
        (Method::Get, "/status") => Ok(handlers.status()),
        (Method::Post, "/record/toggle") => handlers.toggle_record(),
        (Method::Post, "/mic/toggle") => handlers.toggle_mic(),
        (Method::Post, "/mic/clean") => handlers.toggle_clean_mic(),
        (Method::Post, "/marker") => handlers.add_marker(),
        (Method::Post, "/screenshot") => handlers.screenshot(),
        (Method::Post, "/open/panel") => handlers.open_panel(),
        (Method::Post, "/open/clips") => handlers.open_clips_folder(),
        (Method::Post, "/last-clip/edit") => handlers.edit_last_clip(),
        (Method::Post, "/last-clip/copy") => handlers.copy_last_clip(),
        (Method::Post, "/last-clip/compress") => handlers.compress_last_clip(),
        (Method::Post, "/last-clip/discord") => handlers.send_last_clip(),
        (Method::Post, "/last-clip/vertical") => handlers.vertical_last_clip(),
        (Method::Post, "/last-clip/star") => handlers.star_last_clip(),
        (Method::Post, "/camera/toggle") => handlers.toggle_camera(),
        (Method::Post, "/talk/on") => handlers.deck_talk(true),
        (Method::Post, "/talk/off") => handlers.deck_talk(false),

        (Method::Post, volume) if volume.starts_with("/mic/volume/") => match volume.trim_start_matches("/mic/volume/").parse::<i64>() {
            Ok(percent) => handlers.change_mic_volume(percent.clamp(-200, 200)),
            Err(_) => return (404, json!({ "error": "not found" })),
        },
        (Method::Post, space) if space.starts_with("/test/free-space/") && is_test_instance() => {
            handlers.test_free_space(space.trim_start_matches("/test/free-space/").parse().ok());
            Ok(json!({}))
        }
        (Method::Post, "/test/engine/crash" | "/test/engine/stop-replay") if is_test_instance() => {
            handlers.test_engine(url.trim_start_matches("/test/engine/"));
            Ok(json!({}))
        }
        (Method::Post, "/test/talk/on" | "/test/talk/off") if is_test_instance() => {

            handlers.test_talk(url.ends_with("/on"));
            Ok(json!({}))
        }

        (Method::Post, "/test/open/panel" | "/test/open/editor") if is_test_instance() => {
            handlers.test_open(url.rsplit('/').next().unwrap_or(""));
            Ok(json!({}))
        }
        (Method::Post, game) if game.starts_with("/test/game/") && is_test_instance() => {
            handlers.test_game(game.trim_start_matches("/test/game/"));
            Ok(json!({}))
        }
        (Method::Post, pad) if pad.starts_with("/test/controller/") && is_test_instance() => {
            let mut parts = pad.trim_start_matches("/test/controller/").splitn(2, '/');
            handlers.test_controller(parts.next().unwrap_or(""), parts.next().unwrap_or(""));
            Ok(json!({}))
        }
        (Method::Post, "/test/panel-hold/on" | "/test/panel-hold/off") if is_test_instance() => {
            handlers.test_hold_panel(url.ends_with("/on"));
            Ok(json!({}))
        }
        (Method::Post, _) => match url.strip_prefix("/clip/").and_then(|s| s.parse::<u32>().ok()) {
            Some(seconds) => handlers.clip(seconds as i64),
            None => return (404, json!({ "error": "not found" })),
        },
        _ => return (404, json!({ "error": "not found" })),
    };
    match result {
        Ok(value) => (200, value),
        Err(error) => (500, json!({ "error": error })),
    }
}
